//! Set-up over a fake pipeline that asks the core's own questions. Nothing here runs
//! umu or Wine, reaches GitHub or touches this machine's state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use gogo::auth::TokenProvider;
use gogo::config::{Config, ProtonDir};
use gogo::engine::FreeSpace;
use gogo::engine::events::{ProgressEvent, step};
use gogo::gog::content::DlcRef;
use gogo::gog::depot::{DepotError, DepotEvent, DepotOptions};
use gogo::gog::endpoints::Endpoints;
use gogo::linuxapp::preview::{
    Answers, Ask, BuildPreview, BuildRequest, Download, Item, Preview, Question, SetupPreview,
    Tools,
};
use gogo::linuxapp::proton::{self, SteamKind, SteamRoot};
use gogo::linuxapp::source::{self, Files, Source};
use gogo::linuxapp::{comet, galaxy_switch, umu};
use gogo::macapp::MacAppError;
use gogo::macapp::catalog;
use gogo::macapp::launch::GameDirs;
use gogo_contract::{Value, actions};
use gogo_gui_model::{
    DWELL, InputEvent, Invocation, JobId, JobState, JobStatus, Modal, Model, Outcome, Press,
    PromptAnswer, StepState,
};
use tokio_util::sync::CancellationToken;

use super::super::tests::{NoBrowser, SignedIn, scratch};
use super::super::{CoreBackend, Engine, Parts};
use super::pipeline::{Build, find_gogo};
use super::{Failed, Installs, Pipelines};

const SOON: Duration = Duration::from_secs(20);

/// How the next run of the fake pipeline ends.
type End = Result<(), Failed>;

/// A pipeline that asks as core's preview does, writes a marker only when run, and
/// runs until the test lets it end.
struct Fake {
    root: PathBuf,
    /// The runs, with the answers each got.
    runs: Mutex<Vec<(String, Answers)>>,
    ends: Mutex<mpsc::Receiver<End>>,
    /// Bytes of the GE-Proton download.
    bytes: Mutex<u64>,
    /// The `gogo` CLI found.
    cli: Mutex<Option<PathBuf>>,
    /// The products whose files were asked for, and whether the downloads may end.
    fetches: Mutex<Vec<u64>>,
    fetched: AtomicBool,
}

impl Fake {
    fn ran(&self) -> Vec<String> {
        self.runs
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.0.clone())
            .collect()
    }

    fn tools(&self, answers: &Answers) -> Tools {
        Tools {
            umu: answers.umu.expect("answered"),
            runtime: "ge-proton/1".to_string(),
            downloads: vec![Download {
                item: Item::Proton,
                version: "1".to_string(),
                bytes: *self.bytes.lock().unwrap(),
                into: None,
            }],
            galaxy: true,
            install_comet: answers.install_comet == Some(true),
        }
    }

    fn question(&self, answers: &Answers) -> Option<Question> {
        let config = self.root.join("config.toml");
        let entry = |id| catalog::find(id).expect("in the catalog");
        let (ask, spec) = if answers.umu.is_none() {
            let spec = umu::prompt_spec(entry(catalog::UMU), 1_000_000, &config, None, true);
            (Ask::Umu, spec)
        } else if answers.proton_dir.is_none() {
            let roots = [SteamRoot {
                kind: SteamKind::Native,
                path: self.root.join("Steam"),
            }];
            let spec = proton::prompt_spec(
                entry(catalog::GE_PROTON),
                Some(500_000_000),
                &roots,
                &self.root.join("runtimes/ge-proton"),
                &config,
            );
            let dirs = vec![ProtonDir::Store, ProtonDir::Steam];
            (Ask::ProtonDir(dirs), spec)
        } else if answers.galaxy.is_none() {
            (Ask::Galaxy, galaxy_switch::ask_spec("10", true, true))
        } else if answers.install_comet.is_none() {
            let line = comet::plan_line(entry(catalog::COMET), 8_000_000);
            (Ask::InstallComet, galaxy_switch::offer_spec(&line))
        } else {
            return None;
        };
        Some(Question { ask, spec })
    }
}

impl Pipelines for Fake {
    fn supported(&self) -> bool {
        true
    }

    fn preview_setup(
        &self,
        gogid: &str,
        answers: &Answers,
    ) -> Result<Preview<SetupPreview>, MacAppError> {
        if let Some(question) = self.question(answers) {
            return Ok(Preview::Ask(question));
        }
        Ok(Preview::Ready(SetupPreview {
            title: format!("Game {gogid}"),
            dirs: GameDirs::at(self.root.join("games").join(gogid)),
            prefix_exists: false,
            runtime_change: None,
            tools: self.tools(answers),
        }))
    }

    fn setup(
        &self,
        gogid: &str,
        answers: &Answers,
        emit: &mut dyn FnMut(&ProgressEvent),
    ) -> Result<(), Failed> {
        std::fs::write(self.root.join(format!("ran-{gogid}")), b"").expect("a marker");
        emit(&ProgressEvent::WorkStarted {
            step: step::PREFIX.into(),
        });
        self.runs
            .lock()
            .unwrap()
            .push((gogid.to_string(), answers.clone()));
        let end = self.ends.lock().unwrap().recv_timeout(SOON);
        end.expect("the test ends the run")
    }

    fn gogo_cli(&self) -> Option<PathBuf> {
        self.cli.lock().unwrap().clone()
    }

    fn source(
        &self,
        _: Arc<dyn TokenProvider>,
        request: &source::Request,
    ) -> Result<Source, DepotError> {
        let tree = self.root.join(format!("cache/{}/Game", request.product_id));
        let files = if tree.is_dir() {
            Files::Cached(tree)
        } else {
            Files::Fetch { repair: false }
        };
        Ok(Source {
            files,
            root: self.root.join(format!("cache/{}", request.product_id)),
            options: DepotOptions::default(),
            build: Some(("b1".to_string(), Some("1.5".to_string()))),
            sizes: Some((3_000_000_000, 7_000_000_000)),
            dlcs: request
                .owned
                .iter()
                .filter(|&&id| id != request.product_id)
                .map(|&id| DlcRef {
                    id,
                    title: format!("DLC {id}"),
                })
                .collect(),
            dlcs_not_taken: Vec::new(),
        })
    }

    fn preview_build(
        &self,
        request: &BuildRequest<'_>,
        answers: &Answers,
    ) -> Result<Preview<BuildPreview>, MacAppError> {
        if let Some(question) = self.question(answers) {
            return Ok(Preview::Ask(question));
        }
        let dirs = GameDirs::at(self.root.join("games").join(request.gogid));
        Ok(Preview::Ready(BuildPreview {
            rebuild: false,
            prefix_exists: false,
            tree_bytes: None,
            renderer: None,
            verbs: Vec::new(),
            tools: self.tools(answers),
            dirs,
        }))
    }

    fn fetch(
        &self,
        _: Arc<dyn TokenProvider>,
        build: &Build,
        _: Option<u64>,
        cancel: &CancellationToken,
        progress: &mut dyn FnMut(&DepotEvent),
    ) -> Result<(), DepotError> {
        progress(&DepotEvent::Downloading {
            downloaded: 1,
            written: 1,
            total_download: 4,
            total_size: 4,
        });
        self.fetches.lock().unwrap().push(build.product);
        let started = Instant::now();
        while !self.fetched.load(Ordering::Acquire) {
            if cancel.is_cancelled() {
                return Err(DepotError::Library("operation was cancelled".into()));
            }
            assert!(started.elapsed() < SOON, "the test ends the download");
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    fn build(
        &self,
        _: Arc<dyn TokenProvider>,
        build: &Build,
        answers: &Answers,
        emit: &mut dyn FnMut(&ProgressEvent),
    ) -> Result<(), Failed> {
        self.setup(&build.product.to_string(), answers, emit)
    }

    fn data_root(&self) -> PathBuf {
        self.root.clone()
    }
}

struct Space(u64);

impl FreeSpace for Space {
    fn available_bytes(&self, _: &Path) -> std::io::Result<u64> {
        Ok(self.0)
    }
}

struct Nothing;

impl super::super::local::Local for Nothing {
    fn read(&self, _: &[u64]) -> super::super::local::Machine {
        super::super::local::Machine::default()
    }
}

struct Scratch {
    root: tempfile::TempDir,
    fake: Arc<Fake>,
    end: mpsc::Sender<End>,
}

impl Scratch {
    fn new() -> Self {
        let root = scratch();
        gogo::state::set_test_home(root.path().join("home"));
        let (end, ends) = mpsc::channel();
        let fake = Arc::new(Fake {
            root: root.path().to_path_buf(),
            runs: Mutex::default(),
            ends: Mutex::new(ends),
            bytes: Mutex::new(500_000_000),
            cli: Mutex::new(Some(PathBuf::from("/opt/gogo/bin/gogo"))),
            fetches: Mutex::default(),
            fetched: AtomicBool::new(false),
        });
        Scratch { root, fake, end }
    }

    fn backend(&self, endpoints: Endpoints, free: u64) -> CoreBackend {
        let downloads = self.root.path().join("downloads");
        let mut config = Config::default();
        config.download.dir = Some(downloads.display().to_string());
        let engine = Arc::new(Engine::new(Parts {
            account: Arc::new(SignedIn(true)),
            local: Arc::new(Nothing),
            http: reqwest::Client::new(),
            endpoints,
            language: "en".to_string(),
            download_dir: downloads,
            browser: Arc::new(NoBrowser),
            config,
            free_space: Arc::new(Space(free)),
            pipelines: Arc::clone(&self.fake) as Arc<dyn Pipelines>,
        }));
        CoreBackend::new(engine, tokio::runtime::Handle::current(), None)
    }

    /// Everything under the scratch root.
    fn tree(&self) -> BTreeMap<PathBuf, u64> {
        fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, u64>) {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let meta = e.metadata().expect("metadata");
                out.insert(e.path(), meta.len());
                if meta.is_dir() {
                    walk(&e.path(), out);
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(self.root.path(), &mut out);
        out
    }
}

struct Rig {
    model: Model<CoreBackend>,
    installs: Arc<Installs>,
    scratch: Scratch,
    input: u64,
    now: Duration,
}

impl Rig {
    fn new() -> Self {
        Self::with_space(u64::MAX)
    }

    fn with_space(free: u64) -> Self {
        let scratch = Scratch::new();
        let backend = scratch.backend(Endpoints::at("http://127.0.0.1:1"), free);
        let installs = backend.installs();
        Rig {
            model: Model::new(backend, || {}),
            installs,
            scratch,
            input: 0,
            now: Duration::ZERO,
        }
    }

    fn next(&mut self) -> InputEvent {
        self.input += 1;
        InputEvent(self.input)
    }

    fn invoke(&mut self, invocation: &Invocation) -> Outcome {
        let event = self.next();
        let outcome = self.model.invoke(invocation, event);
        self.model.pump();
        outcome
    }

    fn ask(&mut self, id: u64) -> Outcome {
        let spec = actions::action("game.setup").expect("an action");
        let invocation = Invocation::new(spec).arg("id", Value::str(id.to_string()));
        self.invoke(&invocation)
    }

    fn job(&mut self, action: &str, n: usize) -> Outcome {
        let id = self.ids()[n].as_str().to_string();
        let spec = actions::action(action).expect("an action");
        self.invoke(&Invocation::new(spec).arg("job", Value::str(id)))
    }

    fn screen(&self) -> &'static str {
        match self.model.modal() {
            None => "none",
            Some(Modal::Prompt { .. }) => "prompt",
            Some(Modal::Planning { .. }) => "planning",
            Some(Modal::PlanCard { .. }) => "card",
            Some(Modal::Strong { .. }) => "strong",
            Some(Modal::Error { .. }) => "error",
        }
    }

    async fn until(&mut self, what: &str, done: impl Fn(&Rig) -> bool) {
        let started = Instant::now();
        loop {
            self.model.pump();
            if done(self) {
                return;
            }
            assert!(
                started.elapsed() < SOON,
                "never {what}: {:?} {:?}",
                self.model.modal(),
                self.statuses()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    async fn shows(&mut self, screen: &str) {
        let screen = screen.to_string();
        self.until(&format!("shows {screen}"), |r| r.screen() == screen)
            .await;
    }

    /// A new press on the screen on top, seen long enough.
    fn press(&mut self) -> (gogo_gui_model::Ticket, Press) {
        let ticket = self.model.modal().expect("a screen").ticket();
        self.model
            .presented(ticket, InputEvent(self.input), self.now);
        self.now += DWELL;
        let press = Press {
            event: self.next(),
            at: self.now,
            repeat: false,
        };
        (ticket, press)
    }

    /// Answer the questions on the way: the first option, and yes.
    async fn answer_questions(&mut self, how_many: usize) {
        for n in 0..how_many {
            self.shows("prompt").await;
            let (ticket, press) = self.press();
            let answer = if n == 3 {
                PromptAnswer::Yes
            } else {
                PromptAnswer::Choice(0)
            };
            assert_eq!(self.model.answer(ticket, answer, press), Outcome::Accepted);
            self.model.pump();
        }
    }

    fn confirm(&mut self) {
        let (ticket, press) = self.press();
        assert_eq!(self.model.confirm(ticket, press), Outcome::Accepted);
        self.model.pump();
    }

    async fn start(&mut self, id: u64) {
        let before = self.ids().len();
        assert_eq!(self.ask(id), Outcome::Accepted);
        self.answer_questions(4).await;
        self.shows("card").await;
        self.confirm();
        self.until("a job", |r| r.ids().len() > before).await;
    }

    fn ids(&self) -> Vec<JobId> {
        self.model.jobs().ids().cloned().collect()
    }

    fn statuses(&self) -> Vec<JobStatus> {
        let jobs = self.model.jobs();
        jobs.ids()
            .filter_map(|id| jobs.status(id).cloned())
            .collect()
    }

    async fn job_is(&mut self, n: usize, state: JobState) {
        self.until(&format!("job {n} {state:?}"), |r| {
            r.statuses().get(n).is_some_and(|s| s.state == state)
        })
        .await;
    }

    async fn running(&mut self, count: usize) {
        self.until(&format!("{count} runs"), |r| {
            r.scratch.fake.ran().len() == count
        })
        .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_any_question_or_the_card_changes_nothing() {
    let mut rig = Rig::new();
    let before = rig.scratch.tree();
    for questions in 0..=4 {
        for by_esc in [false, true] {
            assert_eq!(rig.ask(10), Outcome::Accepted);
            rig.answer_questions(questions).await;
            rig.shows(if questions < 4 { "prompt" } else { "card" })
                .await;
            if by_esc {
                rig.model.dismiss();
            } else {
                let ticket = rig.model.modal().expect("a screen").ticket();
                rig.model.cancel(ticket);
            }
            rig.model.pump();
            assert_eq!(rig.screen(), "none", "{questions} {by_esc}");
        }
    }
    assert!(rig.scratch.fake.ran().is_empty());
    assert!(rig.ids().is_empty());
    assert_eq!(rig.scratch.tree(), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_the_app_with_a_question_up_changes_nothing() {
    let mut rig = Rig::new();
    let before = rig.scratch.tree();
    rig.ask(10);
    rig.answer_questions(2).await;
    rig.shows("prompt").await;
    assert!(!rig.model.refuse_close());
    let Rig {
        model,
        installs,
        scratch,
        ..
    } = rig;
    drop(model);
    installs.shutdown().await;
    assert!(scratch.fake.ran().is_empty());
    assert_eq!(scratch.tree(), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_set_up_runs_after_the_confirm_and_cannot_be_stopped() {
    let mut rig = Rig::new();
    rig.ask(10);
    rig.answer_questions(4).await;
    rig.shows("card").await;
    let Some(Modal::PlanCard { plan, tier, .. }) = rig.model.modal() else {
        unreachable!()
    };
    assert_eq!(tier, gogo_contract::Tier::Confirm);
    let card = format!("{plan:?}");
    for line in [
        "Run setup for Game 10?",
        "GE-Proton 1 into gogo's store",
        "Creates the Wine prefix",
        "Saves your choices: umu = zipapp, GE-Proton goes to store, Comet comet for this game",
        "can't be stopped",
    ] {
        assert!(card.contains(line), "{line} missing from {card}");
    }
    assert!(rig.scratch.fake.ran().is_empty(), "not before the confirm");
    rig.confirm();
    rig.running(1).await;
    rig.until("running", |r| {
        r.statuses()
            .first()
            .is_some_and(|s| !s.stoppable && s.state == JobState::Running)
    })
    .await;
    let answers = rig.scratch.fake.runs.lock().unwrap()[0].1.clone();
    assert_eq!(answers.proton_dir, Some(ProtonDir::Store));
    assert_eq!(answers.install_comet, Some(true));
    rig.until("the prefix step", |r| {
        r.statuses()[0]
            .steps
            .iter()
            .any(|s| s.name == "Prepare the Wine prefix" && s.state == StepState::Now)
    })
    .await;

    for action in ["job.cancel", "job.pause"] {
        assert!(
            matches!(rig.job(action, 0), Outcome::Refused(_)),
            "{action}"
        );
        let ticket = rig.model.modal().expect("why").ticket();
        rig.model.cancel(ticket);
    }
    assert!(rig.model.refuse_close(), "the window stays");
    let ticket = rig.model.modal().expect("why").ticket();
    rig.model.cancel(ticket);
    assert_eq!(rig.statuses()[0].state, JobState::Running);

    rig.scratch.end.send(Ok(())).unwrap();
    rig.job_is(0, JobState::Done).await;
    let done = &rig.statuses()[0];
    assert!(done.stoppable && done.steps.iter().all(|s| s.state == StepState::Done));
    assert!(!rig.model.refuse_close());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failure_names_no_path_and_a_retry_runs_again() {
    let mut rig = Rig::new();
    rig.start(10).await;
    rig.running(1).await;
    let secret = rig.scratch.root.path().display().to_string();
    let error = MacAppError::Umu(format!("{secret}/umu-run died"));
    rig.scratch.end.send(Err(Failed::Pipeline(error))).unwrap();
    rig.job_is(0, JobState::Failed).await;
    rig.shows("error").await;
    let shown = format!("{:?} {:?}", rig.model.modal(), rig.statuses());
    assert!(shown.contains("umu could not be run"), "{shown}");
    assert!(!shown.contains(&secret), "{shown}");
    assert!(
        rig.statuses()[0]
            .steps
            .iter()
            .any(|s| s.state == StepState::Failed)
    );
    let ticket = rig.model.modal().expect("the error").ticket();
    rig.model.cancel(ticket);

    // Nothing new to agree to: it runs without a card.
    assert_eq!(rig.job("job.retry", 0), Outcome::Accepted);
    rig.running(2).await;
    assert_eq!(rig.screen(), "none");
    rig.scratch.end.send(Ok(())).unwrap();
    rig.job_is(0, JobState::Done).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retry_that_would_do_more_asks_on_the_plan_card() {
    let mut rig = Rig::new();
    rig.start(10).await;
    rig.running(1).await;
    let error = MacAppError::Proton("made up".into());
    rig.scratch.end.send(Err(Failed::Pipeline(error))).unwrap();
    rig.job_is(0, JobState::Failed).await;
    rig.shows("error").await;
    let ticket = rig.model.modal().expect("the error").ticket();
    rig.model.cancel(ticket);

    *rig.scratch.fake.bytes.lock().unwrap() = 900_000_000;
    assert_eq!(rig.job("job.retry", 0), Outcome::Accepted);
    rig.shows("card").await;
    assert_eq!(rig.scratch.fake.ran().len(), 1, "not before the answer");
    let ticket = rig.model.modal().expect("the card").ticket();
    rig.model.cancel(ticket);
    rig.job_is(0, JobState::Cancelled).await;
    assert_eq!(rig.scratch.fake.ran().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn one_set_up_runs_at_a_time_and_a_waiting_one_can_be_cancelled() {
    let mut rig = Rig::new();
    rig.start(10).await;
    rig.running(1).await;
    rig.start(11).await;
    rig.start(12).await;
    assert_eq!(rig.statuses()[1].state, JobState::Queued);
    // The same game again is refused.
    rig.ask(11);
    rig.shows("error").await;
    let ticket = rig.model.modal().expect("the error").ticket();
    rig.model.cancel(ticket);

    assert_eq!(rig.job("job.cancel", 2), Outcome::Accepted);
    rig.job_is(2, JobState::Cancelled).await;
    rig.scratch.end.send(Ok(())).unwrap();
    rig.running(2).await;
    rig.scratch.end.send(Ok(())).unwrap();
    rig.job_is(1, JobState::Done).await;
    assert_eq!(rig.scratch.fake.ran(), ["10", "11"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn shutting_down_drops_what_waits_and_waits_for_what_runs() {
    let mut rig = Rig::new();
    rig.start(10).await;
    rig.running(1).await;
    rig.start(11).await;
    let installs = Arc::clone(&rig.installs);
    let closing = tokio::spawn(async move { installs.shutdown().await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!closing.is_finished(), "the running set-up is waited for");
    rig.scratch.end.send(Ok(())).unwrap();
    tokio::time::timeout(SOON, closing)
        .await
        .expect("closed")
        .expect("no panic");
    assert_eq!(rig.scratch.fake.ran(), ["10"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn too_little_space_is_refused_before_the_card() {
    let mut rig = Rig::with_space(1_000);
    rig.ask(10);
    rig.answer_questions(4).await;
    rig.shows("error").await;
    let Some(Modal::Error { error, .. }) = rig.model.modal() else {
        unreachable!()
    };
    assert_eq!(error.class, "Disk");
    assert!(rig.scratch.fake.ran().is_empty());
}

/// The account the build tests have: product 10 and its DLC 12 are owned.
async fn build_rig() -> (Rig, wiremock::MockServer) {
    let server = super::super::tests::gog().await;
    let scratch = Scratch::new();
    let backend = scratch.backend(Endpoints::at(&server.uri()), u64::MAX);
    let installs = backend.installs();
    let rig = Rig {
        model: Model::new(backend, || {}),
        installs,
        scratch,
        input: 0,
        now: Duration::ZERO,
    };
    (rig, server)
}

impl Rig {
    fn ask_build(&mut self) -> Outcome {
        let spec = actions::action("game.build").expect("an action");
        let invocation = Invocation::new(spec)
            .arg("id", Value::str("10"))
            .arg("platform", Value::str("linux"));
        self.invoke(&invocation)
    }

    async fn card(&mut self) -> String {
        self.answer_questions(4).await;
        self.shows("card").await;
        let Some(Modal::PlanCard { plan, .. }) = self.model.modal() else {
            unreachable!()
        };
        format!("{plan:?}")
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_install_shows_what_it_takes_and_does_nothing_before_the_confirm() {
    let (mut rig, _server) = build_rig().await;
    let before = rig.scratch.tree();
    rig.ask_build();
    let card = rig.card().await;
    for line in [
        "Install Made Up?",
        "Game files, build 1.5 (en-US)",
        "With DLC: DLC 12",
        "The game's launcher will run /opt/gogo/bin/gogo",
        "Renderer: picked once",
        "Saves your choices",
        "that part can be cancelled",
    ] {
        assert!(card.contains(line), "{line} missing from {card}");
    }
    assert!(card.contains("download: Some(3500000000)"), "{card}");
    let ticket = rig.model.modal().expect("the card").ticket();
    rig.model.cancel(ticket);
    rig.model.pump();
    assert!(rig.scratch.fake.fetches.lock().unwrap().is_empty());
    assert!(rig.scratch.fake.ran().is_empty());
    assert_eq!(rig.scratch.tree(), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_download_can_be_cancelled_and_the_build_cannot() {
    let (mut rig, _server) = build_rig().await;
    rig.ask_build();
    rig.card().await;
    rig.confirm();
    rig.until("downloading", |r| {
        r.statuses()
            .first()
            .is_some_and(|s| s.state == JobState::Running && s.bytes == 1 && s.stoppable)
    })
    .await;
    assert!(!rig.model.refuse_close(), "a download may be left");
    assert_eq!(rig.job("job.cancel", 0), Outcome::Accepted);
    rig.job_is(0, JobState::Cancelled).await;
    assert!(rig.scratch.fake.ran().is_empty(), "nothing was built");

    // Resumed with the same files to fetch, it carries on without a card.
    rig.scratch.fake.fetched.store(true, Ordering::Release);
    assert_eq!(rig.job("job.resume", 0), Outcome::Accepted);
    rig.running(1).await;
    rig.until("building", |r| !r.statuses()[0].stoppable).await;
    assert_eq!(rig.screen(), "none");
    assert!(matches!(rig.job("job.cancel", 0), Outcome::Refused(_)));
    let ticket = rig.model.modal().expect("why").ticket();
    rig.model.cancel(ticket);
    assert!(rig.model.refuse_close());
    let ticket = rig.model.modal().expect("why").ticket();
    rig.model.cancel(ticket);
    rig.scratch.end.send(Ok(())).unwrap();
    rig.job_is(0, JobState::Done).await;
    let names: Vec<String> = rig.statuses()[0]
        .steps
        .iter()
        .map(|s| s.name.clone())
        .collect();
    assert_eq!(names[0], "Download the game files", "{names:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_gogo_command_there_is_no_plan() {
    let (mut rig, _server) = build_rig().await;
    *rig.scratch.fake.cli.lock().unwrap() = None;
    rig.ask_build();
    rig.shows("error").await;
    let shown = format!("{:?}", rig.model.modal());
    assert!(shown.contains("gogo command"), "{shown}");
}

#[test]
fn the_gogo_command_is_the_one_beside_the_app_else_on_the_path() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = scratch();
    let exe = |rel: &str| {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    };
    let app = dir.path().join("app/gogo-gui");
    let path = Some(dir.path().join("bin").into_os_string());
    assert_eq!(find_gogo(Some(app.clone()), path.clone()), None);
    let on_path = exe("bin/gogo");
    assert_eq!(find_gogo(Some(app.clone()), path.clone()), Some(on_path));
    let beside = exe("app/gogo");
    assert_eq!(find_gogo(Some(app), path), Some(beside));
}

/// A rig whose server also serves one slow download of product 10.
async fn downloading_rig() -> (Rig, wiremock::MockServer) {
    use super::super::download::tests::{NEVER, product, serve};
    let server = wiremock::MockServer::start().await;
    product(&server, 10, &["aaa"], "1.0").await;
    serve(&server, "aaa", [NEVER; 3]).await;
    let scratch = Scratch::new();
    let backend = scratch.backend(Endpoints::at(&server.uri()), u64::MAX);
    let installs = backend.installs();
    let rig = Rig {
        model: Model::new(backend, || {}),
        installs,
        scratch,
        input: 0,
        now: Duration::ZERO,
    };
    (rig, server)
}

/// How often file bytes were asked for.
async fn file_requests(server: &wiremock::MockServer) -> usize {
    let requests = server.received_requests().await.expect("recording");
    requests
        .iter()
        .filter(|r| r.url.path().starts_with("/cdn/"))
        .count()
}

impl Rig {
    async fn start_download(&mut self, id: u64) {
        let before = self.ids().len();
        let spec = actions::action("game.download").expect("an action");
        let invocation = Invocation::new(spec).arg("id", Value::str(id.to_string()));
        assert_eq!(self.invoke(&invocation), Outcome::Accepted);
        self.shows("card").await;
        self.confirm();
        self.until("a download job", |r| r.ids().len() > before)
            .await;
    }
}

/// What `main` does once the window is gone: the install queued behind a download
/// of its game must not start when that download stops.
#[tokio::test(flavor = "multi_thread")]
async fn closing_never_starts_an_install_that_waited_for_a_download() {
    let (mut rig, _server) = downloading_rig().await;
    rig.start_download(10).await;
    rig.job_is(0, JobState::Running).await;
    rig.start(10).await;
    assert_eq!(rig.statuses()[1].state, JobState::Queued);
    // Were it run, it would end at once and be counted.
    rig.scratch.end.send(Ok(())).unwrap();

    let downloads = rig.model.backend().downloads();
    tokio::time::timeout(SOON, crate::core::shutdown(&downloads, &rig.installs))
        .await
        .expect("closed");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(rig.scratch.fake.ran().is_empty(), "it ran with no window");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_set_up_waits_for_a_download_of_its_game() {
    let (mut rig, _server) = downloading_rig().await;
    rig.start_download(10).await;
    rig.job_is(0, JobState::Running).await;
    rig.start(10).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(rig.statuses()[1].state, JobState::Queued);
    assert!(rig.scratch.fake.ran().is_empty());

    assert_eq!(rig.job("job.cancel", 0), Outcome::Accepted);
    rig.running(1).await;
    rig.scratch.end.send(Ok(())).unwrap();
    rig.job_is(1, JobState::Done).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_waits_for_a_set_up_of_its_game() {
    let (mut rig, server) = downloading_rig().await;
    rig.start(10).await;
    rig.running(1).await;
    rig.start_download(10).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(rig.statuses()[1].state, JobState::Queued);
    assert_eq!(file_requests(&server).await, 0, "not beside the set-up");

    rig.scratch.end.send(Ok(())).unwrap();
    rig.job_is(1, JobState::Running).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_and_a_set_up_of_different_games_run_together() {
    let (mut rig, _server) = downloading_rig().await;
    rig.start_download(10).await;
    rig.job_is(0, JobState::Running).await;
    rig.start(11).await;
    rig.running(1).await;
    assert_eq!(rig.statuses()[0].state, JobState::Running);
    assert_eq!(rig.statuses()[1].state, JobState::Running);
    rig.scratch.end.send(Ok(())).unwrap();
    rig.job_is(1, JobState::Done).await;
}

/// Drawn, through the Default skin, whose install controls show on Linux only.
#[cfg(target_os = "linux")]
mod drawn {
    use super::*;
    use crate::app::App;
    use crate::data::Data;
    use crate::s1::Driven;

    /// The core's own question texts are long: each must be drawn with every option, and a
    /// press on the drawn Cancel or an Esc must change nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn every_question_is_drawn_whole_and_the_run_starts_only_from_the_card() {
        let server = super::super::super::tests::gog().await;
        let scratch = Scratch::new();
        let backend = scratch.backend(Endpoints::at(&server.uri()), u64::MAX);
        let data = Data::core(backend, None, || {});
        let mut driven = Driven {
            app: App::new(None, data).expect("the app"),
            now: Duration::ZERO,
            seen: String::new(),
        };
        driven.until("Made Up").await;
        driven.press("Details of Made Up");
        driven.until("Download files").await;
        driven.press("Linux setup");
        driven.until("Run setup").await;
        let before = scratch.tree();

        let options = [
            "zipapp: download",
            "store: gogo's store",
            "on: start Comet",
            "Yes",
        ];
        // Cancel at each question in turn, then go all the way.
        for stop in 0..options.len() {
            driven.press("Run setup");
            for (n, option) in options.iter().enumerate().take(stop + 1) {
                driven.until(option).await;
                if n < stop {
                    press_starting(&mut driven, option);
                }
            }
            driven.press("Cancel");
            driven.step();
            assert!(scratch.fake.ran().is_empty());
            assert_eq!(scratch.tree(), before, "cancelled at question {stop}");
        }

        driven.press("Run setup");
        for option in options {
            driven.until(option).await;
            press_starting(&mut driven, option);
        }
        driven.until("Nothing changes until you confirm").await;
        for _ in 0..20 {
            driven.step();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            scratch.fake.ran().is_empty(),
            "seeing the card is no answer"
        );
        assert_eq!(scratch.tree(), before);

        scratch.end.send(Ok(())).unwrap();
        driven.press("Run setup");
        let started = Instant::now();
        while scratch.fake.ran().is_empty() {
            driven.step();
            assert!(started.elapsed() < SOON, "never ran");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Press the button whose label starts with `label`: the core's items are long.
    fn press_starting(driven: &mut Driven, label: &str) {
        let tree = driven.step();
        let full = tree
            .nodes
            .iter()
            .filter_map(|(_, n)| n.label())
            .find(|l| l.starts_with(label))
            .unwrap_or_else(|| panic!("no button {label}"))
            .to_string();
        driven.press(&full);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_game_page_installs_only_through_the_drawn_card() {
        let server = super::super::super::tests::gog().await;
        let scratch = Scratch::new();
        scratch.fake.fetched.store(true, Ordering::Release);
        let backend = scratch.backend(Endpoints::at(&server.uri()), u64::MAX);
        let data = Data::core(backend, None, || {});
        let mut driven = Driven {
            app: App::new(None, data).expect("the app"),
            now: Duration::ZERO,
            seen: String::new(),
        };
        driven.until("Made Up").await;
        driven.press("Details of Made Up");
        driven.until("Install").await;
        let before = scratch.tree();
        driven.press("Install");
        for option in [
            "zipapp: download",
            "store: gogo's store",
            "on: start Comet",
            "Yes",
        ] {
            driven.until(option).await;
            press_starting(&mut driven, option);
        }
        driven.until("Nothing changes until you confirm").await;
        assert!(scratch.fake.fetches.lock().unwrap().is_empty());
        assert!(scratch.fake.ran().is_empty());
        assert_eq!(scratch.tree(), before);

        scratch.end.send(Ok(())).unwrap();
        driven.press("Install");
        let started = Instant::now();
        while scratch.fake.ran().is_empty() {
            driven.step();
            assert!(started.elapsed() < SOON, "never built");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(scratch.fake.ran(), ["10"]);
        assert_eq!(*scratch.fake.fetches.lock().unwrap(), [10]);
    }
}
