//! Installs over gogo-core's blocking pipelines (design 25 §5): one at a time, and
//! never beside a download of the same game. A pipeline can't be stopped once it
//! runs, so its job says so; only a waiting job can be cancelled.

mod pipeline;
mod plan;
mod steps;
#[cfg(test)]
mod tests;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use gogo::linuxapp::preview::Answers;
use gogo::macapp::MacAppError;
use gogo_gui_model::{
    Call, ErrorView, JobControl, JobKind, JobSink, JobState, JobStatus, Jobs, PlanHandle,
    PlanReply, Planned, PromptAnswer, PromptReply, PromptSpec, Run,
};
use tokio::runtime::Handle;
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;

#[cfg(test)]
pub(crate) use self::pipeline::NoPipelines;
pub(crate) use self::pipeline::{Core, Failed, Pipelines};
use self::plan::{Computed, Planning};
use self::steps::Steps;
use super::download::{Downloads, Schedule};
use super::engine::Engine;
use super::{errors, lock};

/// Progress is reported at most this often (design 25 §3).
const PACE: Duration = Duration::from_millis(100);

struct State {
    /// `None` only until the table has given the job one.
    sink: Option<JobSink>,
    status: JobStatus,
    /// The current run's own token: it only ends the wait for a turn.
    cancel: CancellationToken,
    /// What the user agreed to.
    agreed: Computed,
}

impl State {
    fn report(&self) {
        if let Some(sink) = &self.sink {
            sink.report(self.status.clone());
        }
    }
}

struct Job {
    product: u64,
    state: Mutex<State>,
}

impl Job {
    fn update(&self, change: impl FnOnce(&mut JobStatus)) {
        let mut state = lock(&self.state);
        change(&mut state.status);
        state.report();
    }

    /// Stop a job that is waiting. One whose pipeline runs is left alone.
    fn stop(&self, to: JobState) {
        let mut state = lock(&self.state);
        if !state.status.stoppable {
            return;
        }
        state.cancel.cancel();
        state.status.state = to;
        state.report();
    }

    fn fail(&self, error: ErrorView) {
        self.update(|s| {
            s.state = JobState::Failed;
            s.stoppable = true;
            s.error = Some(error);
        });
    }
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

pub(crate) struct Installs {
    engine: Arc<Engine>,
    runtime: Handle,
    downloads: Arc<Downloads>,
    /// One install at a time: they share the runtime store (design 25 §5).
    lane: Schedule,
    jobs: Mutex<Vec<Weak<Job>>>,
    going: watch::Sender<usize>,
    closing: AtomicBool,
    /// Hears that an install of a product has ended.
    ended: Box<dyn Fn(u64) + Send + Sync>,
}

/// The `id` argument as a product and the name core knows it by.
fn product(call: &Call) -> Result<(u64, String), ErrorView> {
    call.text("id")
        .and_then(|id| id.parse::<u64>().ok())
        .map(|id| (id, id.to_string()))
        .ok_or_else(|| ErrorView::usage("Set-up can't be planned", "the game is unknown"))
}

fn already(title: &str) -> ErrorView {
    ErrorView::usage(
        "Already in the queue",
        format!("{title} is being set up; see the queue."),
    )
}

impl Installs {
    pub(super) fn new(
        engine: Arc<Engine>,
        runtime: Handle,
        downloads: Arc<Downloads>,
        ended: impl Fn(u64) + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Installs {
            engine,
            runtime,
            downloads,
            lane: Schedule::with_slots(1),
            jobs: Mutex::default(),
            going: watch::Sender::new(0),
            closing: AtomicBool::new(false),
            ended: Box::new(ended),
        })
    }

    fn jobs(&self) -> Vec<Arc<Job>> {
        let mut jobs = lock(&self.jobs);
        jobs.retain(|j| j.strong_count() > 0);
        jobs.iter().filter_map(Weak::upgrade).collect()
    }

    /// Whether an install of `product` is queued, running or paused.
    fn live(&self, product: u64) -> bool {
        self.jobs()
            .iter()
            .any(|j| j.product == product && !lock(&j.state).status.state.is_terminal())
    }

    /// Preview off the runtime's workers: it reads the disk and asks GitHub.
    async fn preview(
        self: &Arc<Self>,
        gogid: String,
        known: Answers,
        given: Vec<PromptAnswer>,
    ) -> Result<Planning, ErrorView> {
        let engine = Arc::clone(&self.engine);
        tokio::task::spawn_blocking(move || {
            plan::setup(
                engine.pipelines().as_ref(),
                engine.free_space().as_ref(),
                engine.config().snapshots.auto,
                &gogid,
                known,
                &given,
            )
        })
        .await
        .unwrap_or_else(|_| Err(ErrorView::failed("Set-up can't be planned", "it stopped")))
    }

    /// `Backend::plan` for `game.setup`: returns at once.
    pub(crate) fn plan(self: &Arc<Self>, call: &Call, reply: PlanReply) {
        if !self.engine.pipelines().supported() {
            return reply.send(Err(errors::unavailable()));
        }
        let (id, gogid) = match product(call) {
            Ok(p) => p,
            Err(e) => return reply.send(Err(e)),
        };
        if self.live(id) {
            return reply.send(Err(already("This game")));
        }
        let this = Arc::clone(self);
        let given = reply.answers().to_vec();
        self.runtime.spawn(async move {
            let planned = this.preview(gogid, Answers::default(), given).await;
            if reply.is_abandoned() {
                return;
            }
            match planned {
                Ok(Planning::Ask(question)) => reply.ask(question),
                Ok(Planning::Ready(c)) => reply.send(Ok(Planned {
                    plan: c.plan.clone(),
                    facts: c.facts(),
                    inputs: c.inputs,
                    handle: PlanHandle::new(*c),
                })),
                Err(e) => reply.send(Err(e)),
            }
        });
    }

    /// `Backend::run` for `game.setup`: queue what was confirmed.
    pub(crate) fn run(self: &Arc<Self>, run: Run, jobs: &mut Jobs<'_>) -> Result<(), ErrorView> {
        let (id, _) = product(run.call())?;
        let Some(planned) = run.into_plan().and_then(PlanHandle::take::<Computed>) else {
            return Err(ErrorView::failed("Set-up can't start", "it has no plan"));
        };
        if self.live(id) {
            return Err(already(&planned.title));
        }
        let mut status = JobStatus::queued(JobKind::Setup);
        status.game = Some(self.engine.setup_game(id, &planned.title));
        Steps::new(&planned.phases).report(&mut status);
        let cancel = CancellationToken::new();
        let job = Arc::new(Job {
            product: id,
            state: Mutex::new(State {
                sink: None,
                status: status.clone(),
                cancel: cancel.clone(),
                agreed: planned,
            }),
        });
        let sink = jobs.add(
            status,
            Box::new(Control {
                installs: Arc::clone(self),
                job: Arc::clone(&job),
            }),
        );
        lock(&job.state).sink = Some(sink);
        lock(&self.jobs).push(Arc::downgrade(&job));
        self.start(job, cancel, None);
        Ok(())
    }

    /// `again`: the state a resumed or retried job was in, so it plans anew first.
    fn start(self: &Arc<Self>, job: Arc<Job>, cancel: CancellationToken, again: Option<JobState>) {
        if self.closing.load(Ordering::Acquire) {
            return;
        }
        let going = Going::new(&self.going);
        let this = Arc::clone(self);
        self.runtime.spawn(async move {
            let _going = going;
            this.drive(job, cancel, again).await;
        });
    }

    async fn drive(
        self: Arc<Self>,
        job: Arc<Job>,
        cancel: CancellationToken,
        again: Option<JobState>,
    ) {
        if let Some(before) = again
            && !self.again(&job, &cancel, before).await
        {
            return;
        }
        let mut lane = self.lane.join(job.product);
        if cancel.run_until_cancelled(lane.turn()).await.is_none() {
            return;
        }
        // Beside the downloads, not among them: it takes no slot of theirs.
        let mut game = self.downloads.lease(job.product);
        if cancel.run_until_cancelled(game.turn()).await.is_none() {
            return;
        }
        self.execute(&job, &cancel).await;
        drop(game);
        drop(lane);
        (self.ended)(job.product);
    }

    /// Plan a resumed or retried job anew. `false` when it should not run.
    async fn again(
        self: &Arc<Self>,
        job: &Arc<Job>,
        cancel: &CancellationToken,
        before: JobState,
    ) -> bool {
        let agreed = lock(&job.state).agreed.clone();
        let fresh = self
            .preview(agreed.gogid.clone(), agreed.answers.clone(), Vec::new())
            .await;
        if cancel.is_cancelled() {
            return false;
        }
        let fresh = match fresh {
            Ok(Planning::Ready(fresh)) => *fresh,
            // A new question needs the whole flow again.
            Ok(Planning::Ask(_)) => {
                job.fail(ErrorView::failed(
                    "Set-up can't carry on",
                    "something changed that needs a new answer; start the set-up again",
                ));
                return false;
            }
            Err(e) => {
                job.fail(e);
                return false;
            }
        };
        if !fresh.within(&agreed) {
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
                let back = match before {
                    JobState::Paused => JobState::Paused,
                    _ => JobState::Cancelled,
                };
                job.update(|s| s.state = back);
                return false;
            }
        }
        lock(&job.state).agreed = fresh;
        true
    }

    async fn execute(&self, job: &Arc<Job>, cancel: &CancellationToken) {
        let agreed = {
            let mut state = lock(&job.state);
            // The last moment a cancel or a closing app is heeded.
            if cancel.is_cancelled() || self.closing.load(Ordering::Acquire) {
                return;
            }
            state.status.state = JobState::Running;
            state.status.stoppable = false;
            state.report();
            state.agreed.clone()
        };
        let engine = Arc::clone(&self.engine);
        let running = Arc::clone(job);
        let done = tokio::task::spawn_blocking(move || {
            let mut steps = Steps::new(&agreed.phases);
            let mut reported = Instant::now();
            let mut emit = |event: &gogo::engine::events::ProgressEvent| {
                let now = Instant::now();
                let news = steps.apply(event, now);
                if news || now.duration_since(reported) >= PACE {
                    reported = now;
                    running.update(|s| steps.report(s));
                }
            };
            let done = engine
                .pipelines()
                .setup(&agreed.gogid, &agreed.answers, &mut emit);
            if done.is_ok() {
                steps.end(true, Instant::now());
            } else {
                steps.closing_if_idle(Instant::now());
                steps.end(false, Instant::now());
            }
            running.update(|s| steps.report(s));
            done
        })
        .await;
        match done {
            Ok(Ok(())) => job.update(|s| {
                s.state = JobState::Done;
                s.stoppable = true;
                s.fraction = Some(1.0);
            }),
            Ok(Err(failed)) => job.fail(failure(&failed)),
            Err(_) => job.fail(ErrorView::failed(
                "Set-up failed",
                "it stopped unexpectedly",
            )),
        }
    }

    /// Stop what waits, and wait for a running pipeline to end: it can't be cut short
    /// without leaving a game half set up. The app refuses to close while one runs, so
    /// this waits only when the window went some other way.
    pub(crate) async fn shutdown(&self) {
        self.closing.store(true, Ordering::Release);
        for job in self.jobs() {
            let state = lock(&job.state);
            if state.status.stoppable {
                state.cancel.cancel();
            } else {
                tracing::warn!("waiting for a set-up to end");
            }
        }
        let mut going = self.going.subscribe();
        // The sender lives as long as `self`, so this only ends at zero.
        let _ = going.wait_for(|&n| n == 0).await;
    }
}

/// A failed run's error, in the app's own phrases: the core's text holds paths and log
/// lines, and goes to the log only.
fn failure(failed: &Failed) -> ErrorView {
    let (Failed::Pipeline(e) | Failed::Comet(e)) = failed;
    tracing::warn!("set-up failed: {}", errors::path(&e.to_string()));
    let (class, message) = match (failed, e) {
        (Failed::Comet(_), _) => ("Error", "The game is set up, but Comet was not installed."),
        (_, MacAppError::CatalogUnreachable { .. })
        | (_, MacAppError::RuntimeDownloadFailed { .. }) => ("Network", "A download failed."),
        (_, MacAppError::RuntimeChecksumMismatch { .. }) => {
            ("CORRUPT", "A download did not match its checksum.")
        }
        (_, MacAppError::Umu(_)) => ("Error", "umu could not be run."),
        (_, MacAppError::Proton(_)) => ("Error", "The GE-Proton build could not be used."),
        (_, MacAppError::Io(io)) if io.kind() == std::io::ErrorKind::StorageFull => {
            ("Disk", "There is not enough free space.")
        }
        (_, MacAppError::Io(_)) => ("Error", "A file could not be read or written."),
        _ => ("Error", "Setting up the game did not finish."),
    };
    ErrorView::new(
        class,
        "Set-up failed",
        message,
        "Retry. The game's install folder holds the logs.",
        "7",
    )
}

struct Control {
    installs: Arc<Installs>,
    job: Arc<Job>,
}

impl Control {
    fn restart(&mut self, sink: JobSink) {
        let cancel = CancellationToken::new();
        let before = {
            let mut state = lock(&self.job.state);
            let before = state.status.state;
            state.sink = Some(sink);
            state.cancel = cancel.clone();
            state.status.state = JobState::Queued;
            state.status.stoppable = true;
            state.status.error = None;
            Steps::new(&state.agreed.phases.clone()).report(&mut state.status);
            state.report();
            before
        };
        self.installs
            .start(Arc::clone(&self.job), cancel, Some(before));
    }
}

impl JobControl for Control {
    fn cancel(&mut self) {
        self.job.stop(JobState::Cancelled);
    }

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
