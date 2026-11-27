//! Downloads over gogo-core: one shared [`AppContext`], a job per confirmed plan with
//! its own token and events, and the rules of design 25 §5. A job is only ever stopped by
//! its token: dropping its future would leave the file tasks writing.

mod plan;
mod progress;
mod schedule;
#[cfg(test)]
pub(super) mod tests;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use gogo::app::AppContext;
use gogo::auth::TokenProvider;
use gogo::engine::Summary;
use gogo::engine::job::JobScope;
use gogo::engine::task::FileTask;
use gogo::gog::error::AuthError;
use gogo_contract::Value;
use gogo_gui_model::{
    Call, ErrorView, JobControl, JobKind, JobSink, JobState, JobStatus, Jobs, PlanHandle,
    PlanReply, Planned, PromptAnswer, PromptReply, PromptSpec, Run,
};
use tokio::runtime::Handle;
use tokio::sync::{OnceCell, broadcast::error::RecvError, oneshot, watch};
use tokio::time::{MissedTickBehavior, interval_at};
use tokio_util::sync::CancellationToken;

use self::plan::{Computed, Request, Wanted};
use self::progress::Progress;
use self::schedule::Place;
pub(crate) use self::schedule::Schedule;
use super::engine::Engine;
use super::{errors, lock};

/// Progress is reported at most this often (design 25 §3).
const PACE: Duration = Duration::from_millis(100);

/// How long closing the app waits for the downloads to stop.
const STOPPING: Duration = Duration::from_secs(5);

/// The session as the engine asks for it: whoever is signed in now, one token refresh
/// at a time.
struct Tokens(Arc<Engine>);

#[async_trait]
impl TokenProvider for Tokens {
    async fn bearer(&self) -> Result<String, AuthError> {
        self.0.access_token().await
    }

    fn user_id(&self) -> Option<u64> {
        None
    }
}

/// What changes as a job goes.
struct State {
    /// `None` only until the table has given the job one.
    sink: Option<JobSink>,
    status: JobStatus,
    /// The current run's own token.
    cancel: CancellationToken,
    /// The files the user agreed to.
    files: Vec<Wanted>,
}

impl State {
    fn report(&self) {
        if let Some(sink) = &self.sink {
            sink.report(self.status.clone());
        }
    }
}

struct Job {
    request: Request,
    state: Mutex<State>,
}

impl Job {
    /// Change and report the status, unless the run of `cancel` has been stopped: what
    /// the user did since is what the job shows.
    fn update(&self, cancel: &CancellationToken, change: impl FnOnce(&mut JobStatus)) {
        let mut state = lock(&self.state);
        if cancel.is_cancelled() {
            return;
        }
        change(&mut state.status);
        state.report();
    }

    fn fail(&self, cancel: &CancellationToken, error: ErrorView) {
        self.update(cancel, |s| {
            settle(s, JobState::Failed);
            s.error = Some(error);
        });
    }

    /// Stop the current run and show `state`.
    fn stop(&self, to: JobState) {
        let mut state = lock(&self.state);
        state.cancel.cancel();
        settle(&mut state.status, to);
        state.report();
    }
}

fn settle(status: &mut JobStatus, to: JobState) {
    status.state = to;
    status.rate = 0;
    status.eta = None;
}

/// Tells how many runs are still going.
struct Going(watch::Sender<usize>);

impl Going {
    fn new(count: &watch::Sender<usize>) -> Self {
        count.send_modify(|n| *n += 1);
        Going(count.clone())
    }
}

impl Drop for Going {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n -= 1);
    }
}

pub(crate) struct Downloads {
    engine: Arc<Engine>,
    runtime: Handle,
    ctx: OnceCell<Arc<AppContext>>,
    /// Bytes per second; 0 is no limit.
    rate: AtomicU64,
    schedule: Schedule,
    jobs: Mutex<Vec<Weak<Job>>>,
    going: watch::Sender<usize>,
    /// What the progress ticks of every job count from.
    epoch: tokio::time::Instant,
    closing: AtomicBool,
    /// Hears that a download of a product has ended.
    ended: Box<dyn Fn(u64) + Send + Sync>,
}

impl Downloads {
    pub(super) fn new(
        engine: Arc<Engine>,
        runtime: Handle,
        ended: impl Fn(u64) + Send + Sync + 'static,
    ) -> Arc<Self> {
        let rate = engine.config().download.limit;
        Arc::new(Downloads {
            engine,
            runtime,
            ctx: OnceCell::new(),
            rate: AtomicU64::new(rate),
            schedule: Schedule::default(),
            jobs: Mutex::default(),
            going: watch::Sender::new(0),
            epoch: tokio::time::Instant::now(),
            closing: AtomicBool::new(false),
            ended: Box::new(ended),
        })
    }

    /// The one context every download shares, made when first needed. Making it reads
    /// the record of downloads and writes nothing; the downloads run on it later add
    /// their files to that record.
    async fn context(self: &Arc<Self>) -> Result<Arc<AppContext>, ErrorView> {
        let made = self.ctx.get_or_try_init(|| async {
            let engine = Arc::clone(&self.engine);
            let made = tokio::task::spawn_blocking(move || {
                // The core's own client: a total timeout would cut long files off.
                let http = gogo::gog::http().ok()?;
                let tokens = Arc::new(Tokens(Arc::clone(&engine)));
                let mut ctx =
                    AppContext::assemble(engine.config().clone(), http, Some(tokens)).ok()?;
                ctx.free_space = engine.free_space();
                Some(Arc::new(ctx))
            })
            .await;
            made.ok().flatten().ok_or_else(|| {
                ErrorView::failed("Downloads can't start", "the engine could not be set up")
            })
        });
        let ctx = Arc::clone(made.await?);
        ctx.limiter.set_rate(self.rate.load(Ordering::Acquire));
        Ok(ctx)
    }

    /// Hold `product` against downloads of it, without taking a download slot.
    pub(super) fn lease(&self, product: u64) -> Place {
        self.schedule.lease(product)
    }

    pub(crate) fn rate_limit(&self) -> Option<u64> {
        Some(self.rate.load(Ordering::Acquire)).filter(|&r| r > 0)
    }

    /// The limit every download shares, for this session.
    pub(crate) fn set_rate_limit(&self, rate: Option<u64>) {
        let rate = rate.unwrap_or(0);
        self.rate.store(rate, Ordering::Release);
        if let Some(ctx) = self.ctx.get() {
            ctx.limiter.set_rate(rate);
        }
    }

    fn jobs(&self) -> Vec<Arc<Job>> {
        let mut jobs = lock(&self.jobs);
        jobs.retain(|j| j.strong_count() > 0);
        jobs.iter().filter_map(Weak::upgrade).collect()
    }

    /// Whether a download of `product` is queued, running or paused.
    fn live(&self, product: u64) -> bool {
        self.jobs().iter().any(|job| {
            job.request.product == product && !lock(&job.state).status.state.is_terminal()
        })
    }

    /// What the other live jobs still have to write.
    fn queued_bytes(&self, but: Option<&Job>) -> u64 {
        self.jobs()
            .iter()
            .filter(|job| but.is_none_or(|b| !std::ptr::eq(b, job.as_ref())))
            .map(|job| {
                let state = lock(&job.state);
                if state.status.state.is_terminal() {
                    0
                } else {
                    state
                        .status
                        .total
                        .unwrap_or(0)
                        .saturating_sub(state.status.bytes)
                }
            })
            .sum()
    }

    fn already(title: &str) -> ErrorView {
        ErrorView::usage(
            "Already in the queue",
            format!("{title} has a download in the queue; see the queue."),
        )
    }

    /// `Backend::plan` for `game.download`: returns at once.
    pub(crate) fn plan(self: &Arc<Self>, call: &Call, reply: PlanReply) {
        let request = match Request::of(call) {
            Ok(request) => request,
            Err(e) => return reply.send(Err(e)),
        };
        let this = Arc::clone(self);
        self.runtime.spawn(async move {
            let planned = async {
                if this.live(request.product) {
                    return Err(Self::already("This game"));
                }
                let ctx = this.context().await?;
                let queued = this.queued_bytes(None);
                plan::compute(&this.engine, &ctx, request, queued).await
            }
            .await;
            this.engine.persist().await;
            if reply.is_abandoned() {
                return;
            }
            reply.send(planned.map(|c| Planned {
                plan: c.plan.clone(),
                facts: c.facts(),
                inputs: c.inputs,
                handle: PlanHandle::new(c),
            }));
        });
    }

    /// `Backend::run` for `game.download`: queue exactly the confirmed files.
    pub(crate) fn run(self: &Arc<Self>, run: Run, jobs: &mut Jobs<'_>) -> Result<(), ErrorView> {
        let planned = run.into_plan().and_then(PlanHandle::take::<Computed>);
        let (Some(planned), Some(ctx)) = (planned, self.ctx.get().cloned()) else {
            return Err(ErrorView::failed(
                "The download can't start",
                "it has no plan",
            ));
        };
        if self.live(planned.request.product) {
            return Err(Self::already(&planned.title));
        }
        let mut status = JobStatus::queued(JobKind::Download);
        status.game = Some(
            self.engine
                .job_game(planned.request.product, &planned.title),
        );
        status.total = Some(planned.files.iter().map(|f| f.task.total_size).sum());
        status.bytes = planned.files.iter().map(|f| f.done).sum();
        let scope = ctx.job();
        let job = Arc::new(Job {
            request: planned.request,
            state: Mutex::new(State {
                sink: None,
                status: status.clone(),
                cancel: scope.cancel_token(),
                files: planned.files,
            }),
        });
        let sink = jobs.add(
            status,
            Box::new(Control {
                downloads: Arc::clone(self),
                job: Arc::clone(&job),
            }),
        );
        lock(&job.state).sink = Some(sink);
        lock(&self.jobs).push(Arc::downgrade(&job));
        let place = self.schedule.join(job.request.product);
        self.start(ctx, job, scope, Start::Confirmed(place));
        Ok(())
    }

    fn start(self: &Arc<Self>, ctx: Arc<AppContext>, job: Arc<Job>, scope: JobScope, how: Start) {
        if self.closing.load(Ordering::Acquire) {
            return;
        }
        let going = Going::new(&self.going);
        let this = Arc::clone(self);
        self.runtime.spawn(async move {
            let _going = going;
            this.drive(ctx, job, scope, how).await;
        });
    }

    /// One run of a job, from its place in the line to its end.
    async fn drive(
        self: Arc<Self>,
        ctx: Arc<AppContext>,
        job: Arc<Job>,
        scope: JobScope,
        how: Start,
    ) {
        let cancel = scope.cancel_token();
        let mut place = match how {
            Start::Confirmed(place) => place,
            Start::Again(before) => {
                if !self.again(&ctx, &job, &cancel, before).await {
                    return;
                }
                self.schedule.join(job.request.product)
            }
        };
        // An earlier run of this job holds its slot until its files are closed.
        if cancel.run_until_cancelled(place.turn()).await.is_none() {
            return;
        }
        self.execute(&ctx, &job, &scope).await;
        drop(place);
        self.engine.persist().await;
        (self.ended)(job.request.product);
    }

    /// Plan a resumed or retried job anew. `false` when it should not run.
    ///
    /// Planning is never dropped half way, even when the job is stopped meanwhile: it
    /// can hold a token refresh whose rotated token would be lost.
    async fn again(
        &self,
        ctx: &Arc<AppContext>,
        job: &Arc<Job>,
        cancel: &CancellationToken,
        before: JobState,
    ) -> bool {
        let queued = self.queued_bytes(Some(job));
        let fresh = plan::compute(&self.engine, ctx, job.request.clone(), queued).await;
        self.engine.persist().await;
        if cancel.is_cancelled() {
            return false;
        }
        let fresh = match fresh {
            Ok(fresh) => fresh,
            Err(e) => {
                job.fail(cancel, e);
                return false;
            }
        };
        if fresh.files.is_empty() {
            job.update(cancel, |s| {
                settle(s, JobState::Done);
                s.bytes = s.total.unwrap_or(s.bytes);
                s.fraction = Some(1.0);
            });
            return false;
        }
        let changed = !plan::within(&fresh.files, &lock(&job.state).files);
        if changed {
            let (tx, rx) = oneshot::channel();
            let reply = PromptReply::new(move |answer| {
                // The run may have been stopped, and nobody waits.
                let _ = tx.send(answer);
            });
            let Some(sink) = lock(&job.state).sink.clone() else {
                return false;
            };
            sink.ask(PromptSpec::Plan(Box::new(fresh.plan.clone())), reply);
            let Some(answer) = cancel.run_until_cancelled(rx).await else {
                return false;
            };
            if answer.ok().flatten() != Some(PromptAnswer::Yes) {
                // Not agreed: the job goes back to where it was.
                let back = match before {
                    JobState::Paused => JobState::Paused,
                    _ => JobState::Cancelled,
                };
                job.update(cancel, |s| settle(s, back));
                return false;
            }
        }
        let left = fresh.left();
        let total: u64 = fresh.files.iter().map(|f| f.task.total_size).sum();
        lock(&job.state).files = fresh.files;
        job.update(cancel, |s| {
            s.total = Some(total);
            s.bytes = total.saturating_sub(left);
        });
        true
    }

    /// Download the agreed files with the engine, folding its events into the job.
    async fn execute(&self, ctx: &Arc<AppContext>, job: &Arc<Job>, scope: &JobScope) {
        let cancel = scope.cancel_token();
        match gogo::state::State::load() {
            Ok(state) => *ctx.state.lock().expect("state lock") = state,
            Err(_) => tracing::warn!("the record of downloads could not be read again"),
        }
        // What arrived since the plan is not downloaded again. Nothing is added.
        let files: Vec<Wanted> = lock(&job.state)
            .files
            .iter()
            .filter(|f| !arrived(ctx, &f.task))
            .cloned()
            .collect();
        let mut progress = Progress::new(files.iter().map(|f| {
            let done = gogo::engine::partial(&f.task).map_or(0, |p| p.done);
            (&f.task, done)
        }));
        let mut events = scope.subscribe();
        job.update(&cancel, |s| {
            s.state = JobState::Running;
            progress.report(Instant::now(), s);
        });
        let tasks: Vec<FileTask> = files.into_iter().map(|f| f.task).collect();
        let mut engine = {
            let (ctx, scope) = (Arc::clone(ctx), scope.clone());
            tokio::spawn(async move { gogo::engine::execute_job(&ctx, &scope, tasks).await })
        };
        // Every job ticks on the same instants, so their reports share a wake.
        let mut pace = interval_at(self.epoch, PACE);
        pace.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let summary = loop {
            tokio::select! {
                ended = &mut engine => break ended,
                event = events.recv() => match event {
                    Ok(event) => progress.apply(&event),
                    // Each event says where its file is, so a gap heals itself.
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => break (&mut engine).await,
                },
                _ = pace.tick() => job.update(&cancel, |s| progress.report(Instant::now(), s)),
            }
        };
        while let Ok(event) = events.try_recv() {
            progress.apply(&event);
        }
        let outcome = match summary {
            Ok(summary) => ending(&summary, &progress),
            Err(_) => Err(errors::download(1, None)),
        };
        job.update(&cancel, |s| {
            progress.report(Instant::now(), s);
            match outcome {
                Ok(()) => {
                    settle(s, JobState::Done);
                    s.fraction = Some(1.0);
                }
                Err(error) => {
                    settle(s, JobState::Failed);
                    s.error = Some(error);
                }
            }
        });
    }

    /// Stop every download by its token and wait for the files to be closed, so that
    /// what is on disk can be resumed. Called when the app closes.
    pub(crate) async fn shutdown(&self) {
        self.closing.store(true, Ordering::Release);
        for job in self.jobs() {
            lock(&job.state).cancel.cancel();
        }
        let mut going = self.going.subscribe();
        if tokio::time::timeout(STOPPING, going.wait_for(|&n| n == 0))
            .await
            .is_err()
        {
            tracing::warn!("a download did not stop in time");
        }
        self.engine.persist().await;
    }
}

/// How a run that was not stopped ended.
fn ending(summary: &Summary, progress: &Progress) -> Result<(), ErrorView> {
    if summary.failed > 0 || summary.worst_exit_class.is_some() {
        let class = summary.worst_exit_class.unwrap_or(1);
        return Err(errors::download(class, progress.failed.as_deref()));
    }
    if progress.locked {
        return Err(errors::locked());
    }
    Ok(())
}

/// Whether `task`'s file is recorded at this version and on disk.
fn arrived(ctx: &AppContext, task: &FileTask) -> bool {
    let root = std::path::PathBuf::from(ctx.cfg.download.dir.clone().unwrap_or_default());
    let state = ctx.state.lock().expect("state lock");
    state.installs.iter().any(|r| {
        r.product_id == task.product_id
            && r.dlc_id == task.dlc_id
            && r.file_id == task.file.id
            && r.version == task.entry.version
            && std::fs::metadata(root.join(&r.path)).is_ok_and(|m| m.len() == r.size)
    })
}

/// How a run begins.
enum Start {
    /// The user has just confirmed the files.
    Confirmed(Place),
    /// A resume or retry of a job that was in this state: plan again first.
    Again(JobState),
}

struct Control {
    downloads: Arc<Downloads>,
    job: Arc<Job>,
}

impl Control {
    fn restart(&mut self, sink: JobSink) {
        let Some(ctx) = self.downloads.ctx.get().cloned() else {
            return;
        };
        let scope = ctx.job();
        let before = {
            let mut state = lock(&self.job.state);
            let before = state.status.state;
            state.sink = Some(sink);
            state.cancel = scope.cancel_token();
            state.status.error = None;
            settle(&mut state.status, JobState::Queued);
            state.report();
            before
        };
        let job = Arc::clone(&self.job);
        self.downloads.start(ctx, job, scope, Start::Again(before));
    }
}

impl JobControl for Control {
    fn cancel(&mut self) {
        self.job.stop(JobState::Cancelled);
    }

    /// The core has no way to hold a download, so a pause stops it and keeps what is on
    /// disk; resuming asks for the checksums again and loses the chunks in flight.
    fn pause(&mut self) {
        self.job.stop(JobState::Paused);
    }

    fn resume(&mut self, sink: JobSink) {
        self.restart(sink);
    }

    fn retry(&mut self, sink: JobSink) {
        self.restart(sink);
    }
}

/// The `queue.set_rate_limit` argument.
pub(super) fn rate_arg(call: &Call) -> Option<u64> {
    match call.arg("rate") {
        Some(Value::Num(rate)) if *rate >= 1.0 => Some(*rate as u64),
        _ => None,
    }
}
