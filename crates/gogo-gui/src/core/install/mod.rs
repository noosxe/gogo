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

use gogo::gog::depot::DepotError;
use gogo::gog::dlc::Want;
use gogo::linuxapp::preview::Answers;
use gogo::linuxapp::source;
use gogo::macapp::renderer::RendererRef;
use gogo::macapp::{MacAppError, RendererChoice, compat};
use gogo_contract::Value;
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
use self::plan::{Asked, Computed, Planning};
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

    /// Change and report the status, unless the run of `cancel` has been stopped:
    /// what the user did since is what the job shows.
    fn progress(&self, cancel: &CancellationToken, change: impl FnOnce(&mut JobStatus)) {
        let mut state = lock(&self.state);
        if cancel.is_cancelled() {
            return;
        }
        change(&mut state.status);
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

/// What a `game.build` asks for.
struct BuildCall {
    runtime: Option<String>,
    renderer: RendererChoice,
    lang: Option<String>,
    build: Option<String>,
    dlc: Option<Want>,
}

impl BuildCall {
    fn of(call: &Call) -> Result<BuildCall, ErrorView> {
        let bad = |what: &str| ErrorView::usage("This install can't be planned", what);
        if call.text("platform") != Some("linux") {
            return Err(errors::unavailable());
        }
        let renderer = match call.text("renderer") {
            None | Some("auto") => RendererChoice::Auto,
            Some(id) => {
                let r = RendererRef::parse_on(id, compat::Os::Linux)
                    .map_err(|_| bad("the renderer is unknown"))?;
                RendererChoice::Force(r.renderer)
            }
        };
        let dlc = match call.arg("dlc") {
            None | Some(Value::Null) => None,
            Some(Value::Str(all)) if all == "all" => Some(Want::All),
            Some(Value::List(ids)) => {
                let ids: Option<Vec<u64>> = ids
                    .iter()
                    .map(|id| id.as_str().and_then(|s| s.parse().ok()))
                    .collect();
                let ids = ids.ok_or_else(|| bad("a DLC is unknown"))?;
                Some(if ids.is_empty() {
                    Want::NoDlc
                } else {
                    Want::Ids(ids)
                })
            }
            _ => return Err(bad("the DLC selection is unknown")),
        };
        Ok(BuildCall {
            runtime: call.text("runtime").map(str::to_string),
            renderer,
            lang: call.text("lang").map(str::to_string),
            build: call.text("build").map(str::to_string),
            dlc,
        })
    }
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

/// The title of a plan that can't be made.
const CANT_PLAN: &str = "Install can't be planned";

/// What to plan.
enum What {
    Setup,
    Build(Box<Asked>),
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

    /// Preview off the runtime's workers: it reads the disk and asks GOG and GitHub.
    async fn preview(
        self: &Arc<Self>,
        gogid: String,
        what: What,
        known: Answers,
        given: Vec<PromptAnswer>,
    ) -> Result<Planning, ErrorView> {
        let session = match &what {
            What::Setup => None,
            // Refreshes under the engine's lock before core talks to GOG itself.
            What::Build(_) => {
                self.engine
                    .access_token()
                    .await
                    .map_err(|e| errors::view(CANT_PLAN, &e))?;
                let session = self
                    .engine
                    .session()
                    .await
                    .map_err(|e| errors::view(CANT_PLAN, &e))?;
                Some(session.ok_or_else(errors::signed_out)?)
            }
        };
        let engine = Arc::clone(&self.engine);
        tokio::task::spawn_blocking(move || {
            let pipelines = engine.pipelines();
            let space = engine.free_space();
            match (what, session) {
                (What::Build(asked), Some(session)) => plan::build(
                    pipelines.as_ref(),
                    space.as_ref(),
                    session,
                    *asked,
                    known,
                    &given,
                ),
                _ => plan::setup(
                    pipelines.as_ref(),
                    space.as_ref(),
                    engine.config().snapshots.auto,
                    &gogid,
                    known,
                    &given,
                ),
            }
        })
        .await
        .unwrap_or_else(|_| Err(ErrorView::failed(CANT_PLAN, "it stopped")))
    }

    /// Read what a build starts from: the game's page, what is owned and what is
    /// downloaded.
    async fn asked(&self, product: u64, call: BuildCall) -> Result<Asked, ErrorView> {
        let view = |e| errors::view(CANT_PLAN, &e);
        let Some(page) = self.engine.detail(product).await.map_err(view)? else {
            return Err(errors::signed_out());
        };
        let owned = self.engine.owned_ids().await.map_err(view)?;
        let download_dir = self.engine.download_dir().to_path_buf();
        let installer = tokio::task::spawn_blocking(move || {
            let state = gogo::state::State::load().unwrap_or_default();
            gogo::gog::lane::downloaded_installer(&state.installs, product, &download_dir)
        })
        .await
        .ok()
        .flatten();
        let title = page.title.clone().unwrap_or_else(|| product.to_string());
        Ok(Asked {
            product,
            request: source::Request {
                product_id: product,
                title: super::text::line(&title),
                lang: call.lang,
                build: call.build,
                dlc: call.dlc,
                owned,
                catalog_dlcs: page.expanded_dlcs.clone(),
                installer,
            },
            runtime: call.runtime,
            renderer: call.renderer,
            library: self
                .engine
                .config()
                .library
                .dir
                .as_ref()
                .map(|d| gogo::macapp::launch::absolute(std::path::Path::new(d))),
        })
    }

    /// `Backend::plan` for `game.setup` and `game.build`: returns at once.
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
        let build = match call.name() {
            "game.build" => match BuildCall::of(call) {
                Ok(build) => Some(build),
                Err(e) => return reply.send(Err(e)),
            },
            _ => None,
        };
        let this = Arc::clone(self);
        let given = reply.answers().to_vec();
        self.runtime.spawn(async move {
            let what = match build {
                Some(build) => this
                    .asked(id, build)
                    .await
                    .map(|a| What::Build(Box::new(a))),
                None => Ok(What::Setup),
            };
            let planned = match what {
                Ok(what) => this.preview(gogid, what, Answers::default(), given).await,
                Err(e) => Err(e),
            };
            this.engine.persist().await;
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

    /// `Backend::run`: queue what was confirmed.
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
        let what = match &agreed.build {
            Some((asked, _)) => What::Build(Box::new(asked.clone())),
            None => What::Setup,
        };
        let fresh = self
            .preview(
                agreed.gogid.clone(),
                what,
                agreed.answers.clone(),
                Vec::new(),
            )
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

    /// Download the game files a build lacks: the one part that can be cancelled.
    /// `false` when the run is over.
    async fn fetch(
        &self,
        job: &Arc<Job>,
        cancel: &CancellationToken,
        steps: &Arc<Mutex<Steps>>,
    ) -> bool {
        let agreed = lock(&job.state).agreed.clone();
        let Some((_, build)) = agreed.build else {
            return true;
        };
        if !matches!(build.source.files, source::Files::Fetch { .. }) {
            return true;
        }
        if cancel.is_cancelled() || self.closing.load(Ordering::Acquire) {
            return false;
        }
        job.progress(cancel, |s| s.state = JobState::Running);
        let session = match self.session().await {
            Ok(session) => session,
            Err(e) => {
                job.fail(e);
                return false;
            }
        };
        let engine = Arc::clone(&self.engine);
        let limit = self.downloads.rate_limit();
        let (running, token, listed) = (Arc::clone(job), cancel.clone(), Arc::clone(steps));
        // The token stops it; the future is never dropped.
        let done = tokio::task::spawn_blocking(move || {
            let mut reported = Instant::now();
            engine
                .pipelines()
                .fetch(session, &build, limit, &token, &mut |event| {
                    let now = Instant::now();
                    let mut steps = lock(&listed);
                    if steps.download(event, now) || now.duration_since(reported) >= PACE {
                        reported = now;
                        running.progress(&token, |s| steps.report(s));
                    }
                })
        })
        .await;
        self.engine.persist().await;
        if cancel.is_cancelled() {
            return false;
        }
        match done {
            Ok(Ok(())) => true,
            Ok(Err(e)) => {
                lock(steps).end(false, Instant::now());
                job.progress(cancel, |s| lock(steps).report(s));
                job.fail(failure("Install failed", &Failed::Files(e)));
                false
            }
            Err(_) => {
                job.fail(ErrorView::failed(
                    "Install failed",
                    "it stopped unexpectedly",
                ));
                false
            }
        }
    }

    async fn session(&self) -> Result<Arc<dyn gogo::auth::TokenProvider>, ErrorView> {
        let view = |e| errors::view("Install failed", &e);
        self.engine.access_token().await.map_err(view)?;
        let session = self.engine.session().await.map_err(view)?;
        session.ok_or_else(errors::signed_out)
    }

    async fn execute(&self, job: &Arc<Job>, cancel: &CancellationToken) {
        let steps = Arc::new(Mutex::new(Steps::new(&lock(&job.state).agreed.phases)));
        if !self.fetch(job, cancel, &steps).await {
            return;
        }
        let builds = lock(&job.state).agreed.build.is_some();
        let session = if builds {
            match self.session().await {
                Ok(session) => Some(session),
                Err(e) => return job.fail(e),
            }
        } else {
            None
        };
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
        let title = match agreed.build {
            Some(_) => "Install failed",
            None => "Set-up failed",
        };
        let engine = Arc::clone(&self.engine);
        let running = Arc::clone(job);
        let done = tokio::task::spawn_blocking(move || {
            let mut reported = Instant::now();
            let mut emit = |event: &gogo::engine::events::ProgressEvent| {
                let now = Instant::now();
                let mut steps = lock(&steps);
                let news = steps.apply(event, now);
                if news || now.duration_since(reported) >= PACE {
                    reported = now;
                    running.update(|s| steps.report(s));
                }
            };
            let pipelines = engine.pipelines();
            let done = match (&agreed.build, session) {
                (Some((_, build)), Some(session)) => {
                    pipelines.build(session, build, &agreed.answers, &mut emit)
                }
                _ => pipelines.setup(&agreed.gogid, &agreed.answers, &mut emit),
            };
            let mut steps = lock(&steps);
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
        self.engine.persist().await;
        match done {
            Ok(Ok(())) => job.update(|s| {
                s.state = JobState::Done;
                s.stoppable = true;
                s.fraction = Some(1.0);
            }),
            Ok(Err(failed)) => job.fail(failure(title, &failed)),
            Err(_) => job.fail(ErrorView::failed(title, "it stopped unexpectedly")),
        }
    }

    /// [`close`](Self::close), then [`wait`](Self::wait).
    #[cfg(test)]
    pub(crate) async fn shutdown(&self) {
        self.close();
        self.wait().await;
    }

    /// Start nothing more, and stop what can be stopped.
    pub(crate) fn close(&self) {
        self.closing.store(true, Ordering::Release);
        for job in self.jobs() {
            let state = lock(&job.state);
            if state.status.stoppable {
                state.cancel.cancel();
            } else {
                tracing::warn!("waiting for a set-up to end");
            }
        }
    }

    /// Wait for a running pipeline to end: it can't be cut short without leaving a
    /// game half set up. The app refuses to close while one runs, so this waits only
    /// when the window went some other way.
    pub(crate) async fn wait(&self) {
        let mut going = self.going.subscribe();
        // The sender lives as long as `self`, so this only ends at zero.
        let _ = going.wait_for(|&n| n == 0).await;
    }
}

/// A failed run's error, in the app's own phrases: the core's text holds paths and log
/// lines, and goes to the log only.
fn failure(title: &str, failed: &Failed) -> ErrorView {
    let text = match failed {
        Failed::Pipeline(e) | Failed::Comet(e) => e.to_string(),
        Failed::Files(e) => e.to_string(),
    };
    tracing::warn!("{title}: {}", errors::path(&text));
    let full = |io: &std::io::Error| io.kind() == std::io::ErrorKind::StorageFull;
    let disk = ("Disk", "There is not enough free space.");
    let (class, message) = match failed {
        Failed::Comet(_) => ("Error", "The game is set up, but Comet was not installed."),
        Failed::Files(DepotError::Io(io)) if full(io) => disk,
        Failed::Files(DepotError::Filter(_)) => (
            "Error",
            "The downloaded files hold more than the build takes, and can't be told \
             apart.",
        ),
        Failed::Files(_) => ("Network", "The game files could not be downloaded."),
        Failed::Pipeline(e) => match e {
            MacAppError::CatalogUnreachable { .. } | MacAppError::RuntimeDownloadFailed { .. } => {
                ("Network", "A download failed.")
            }
            MacAppError::RuntimeChecksumMismatch { .. } => {
                ("CORRUPT", "A download did not match its checksum.")
            }
            MacAppError::InstallerFailed { .. } => ("Error", "GOG's installer failed."),
            MacAppError::Umu(_) => ("Error", "umu could not be run."),
            MacAppError::Proton(_) => ("Error", "The GE-Proton build could not be used."),
            MacAppError::Io(io) if full(io) => disk,
            MacAppError::Io(_) => ("Error", "A file could not be read or written."),
            _ => ("Error", "Setting up the game did not finish."),
        },
    };
    ErrorView::new(
        class,
        title,
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
