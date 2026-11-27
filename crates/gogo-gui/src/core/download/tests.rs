//! Downloads over a local server standing in for GOG and a scratch folder under
//! `target/`. Nothing here talks to GOG or touches this machine's state.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gogo::config::Config;
use gogo::engine::{FreeSpace, sidecar};
use gogo::gog::endpoints::Endpoints;
use gogo_contract::{Value, actions};
use gogo_gui_model::{
    DWELL, InputEvent, Invocation, JobId, JobState, JobStatus, Modal, Model, Outcome, Press,
    SourceKey,
};
use md5::{Digest, Md5};
use wiremock::matchers::{header, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::super::tests::{ACCESS, NoBrowser, SignedIn, scratch};
use super::super::{CoreBackend, Engine, Parts};
use super::Downloads;
use crate::app::App;
use crate::data::Data;
use crate::s1::Driven;

/// Far longer than any test waits: a request that only a cancel ends.
const NEVER: Duration = Duration::from_secs(60);
const SOON: Duration = Duration::from_secs(20);
/// Bytes of one file, in three chunks.
const SIZE: usize = 15;

fn contents(name: &str) -> Vec<u8> {
    name.bytes().cycle().take(SIZE).collect()
}

fn md5_hex(data: &[u8]) -> String {
    Md5::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The listing of product `id`: one Windows installer, `files` naming its parts.
fn listing(server: &MockServer, id: u64, files: &[&str], version: &str) -> ResponseTemplate {
    listing_sized(server, id, files, version, SIZE)
}

fn listing_sized(
    server: &MockServer,
    id: u64,
    files: &[&str],
    version: &str,
    size: usize,
) -> ResponseTemplate {
    let parts: Vec<String> = files
        .iter()
        .map(|name| {
            let link = format!("{}/dl/{name}", server.uri());
            format!(r#"{{"id":"{name}","size":{size},"downlink":"{link}"}}"#)
        })
        .collect();
    let body = format!(
        r#"{{"id":{id},"title":"Game {id}","downloads":{{"installers":[{{
            "id":"en1","name":"Setup","os":"windows","language":"en",
            "language_full":"English","version":"{version}",
            "total_size":{},"files":[{}]}}]}}}}"#,
        size * files.len(),
        parts.join(",")
    );
    ResponseTemplate::new(200).set_body_raw(body, "application/json")
}

async fn product(server: &MockServer, id: u64, files: &[&str], version: &str) {
    Mock::given(path(format!("/products/{id}")))
        .and(header("authorization", format!("Bearer {ACCESS}").as_str()))
        .respond_with(listing(server, id, files, version))
        .mount(server)
        .await;
}

/// The link and checksum of file `name`.
async fn links(server: &MockServer, name: &str) {
    let data = contents(name);
    let chunks: String = data
        .chunks(5)
        .enumerate()
        .map(|(i, c)| {
            format!(
                r#"<chunk id="{i}" from="{}" to="{}" method="md5">{}</chunk>"#,
                i * 5,
                i * 5 + 4,
                md5_hex(c)
            )
        })
        .collect();
    let xml = format!(
        r#"<file name="{name}.bin" available="1" chunks="3" total_size="{SIZE}">{chunks}</file>"#
    );
    let link = format!(
        r#"{{"downlink":"{0}/cdn/{name}?token=FAKE-LINK-TOKEN","checksum":"{0}/sum/{name}"}}"#,
        server.uri()
    );
    Mock::given(path(format!("/dl/{name}")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(link, "application/json"))
        .mount(server)
        .await;
    Mock::given(path(format!("/sum/{name}")))
        .respond_with(ResponseTemplate::new(200).set_body_string(xml))
        .mount(server)
        .await;
}

fn range(i: usize) -> String {
    format!("bytes={}-{}", i * 5, i * 5 + 4)
}

fn chunk(name: &str, i: usize) -> ResponseTemplate {
    ResponseTemplate::new(206).set_body_bytes(contents(name)[i * 5..i * 5 + 5].to_vec())
}

/// Chunk `i` of file `name` answers after `delay`.
async fn serve(server: &MockServer, name: &str, delays: [Duration; 3]) {
    links(server, name).await;
    for (i, delay) in delays.into_iter().enumerate() {
        Mock::given(path(format!("/cdn/{name}")))
            .and(header("range", range(i).as_str()))
            .respond_with(chunk(name, i).set_delay(delay))
            .mount(server)
            .await;
    }
}

/// File `name` whose last chunk hangs the first time it is asked for.
async fn serve_stalling_once(server: &MockServer, name: &str) {
    Mock::given(path(format!("/cdn/{name}")))
        .and(header("range", range(2).as_str()))
        .respond_with(chunk(name, 2).set_delay(NEVER))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(server)
        .await;
    serve(server, name, [Duration::ZERO; 3]).await;
}

struct Space(u64);

impl FreeSpace for Space {
    fn available_bytes(&self, _: &std::path::Path) -> std::io::Result<u64> {
        Ok(self.0)
    }
}

struct Scratch {
    root: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Self {
        let root = scratch();
        gogo::state::set_test_home(root.path().join("home"));
        Scratch { root }
    }

    /// Where downloads go.
    fn downloads(&self) -> PathBuf {
        self.root.path().join("downloads")
    }

    fn parts(&self, server: &MockServer, free: u64) -> Parts {
        let mut config = Config::default();
        config.download.dir = Some(self.downloads().display().to_string());
        config.download.workers = 3;
        config.network.retries = 1;
        Parts {
            account: Arc::new(SignedIn(true)),
            local: Arc::new(Nothing),
            http: reqwest::Client::new(),
            endpoints: Endpoints::at(&server.uri()),
            language: "en".to_string(),
            download_dir: self.downloads(),
            browser: Arc::new(NoBrowser),
            config,
            pipelines: Arc::new(super::super::install::NoPipelines),
            free_space: Arc::new(Space(free)),
        }
    }

    /// The folder of product `id`'s files.
    fn game(&self, id: u64) -> PathBuf {
        self.downloads().join(format!("Game {id}"))
    }
}

struct Nothing;

impl super::super::local::Local for Nothing {
    fn read(&self, _: &[u64]) -> super::super::local::Machine {
        super::super::local::Machine::default()
    }
}

struct Rig {
    model: Model<CoreBackend>,
    downloads: Arc<Downloads>,
    scratch: Scratch,
    input: u64,
    now: Duration,
}

impl Rig {
    fn new(server: &MockServer) -> Self {
        Self::with_space(server, u64::MAX)
    }

    fn with_space(server: &MockServer, free: u64) -> Self {
        let scratch = Scratch::new();
        let engine = Arc::new(Engine::new(scratch.parts(server, free)));
        let backend = CoreBackend::new(engine, tokio::runtime::Handle::current(), None);
        let downloads = backend.downloads();
        Rig {
            model: Model::new(backend, || {}),
            downloads,
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

    async fn until(&mut self, what: &str, done: impl Fn(&Model<CoreBackend>) -> bool) {
        let started = Instant::now();
        loop {
            self.model.pump();
            if done(&self.model) {
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

    fn ids(&self) -> Vec<JobId> {
        self.model.jobs().ids().cloned().collect()
    }

    fn statuses(&self) -> Vec<JobStatus> {
        let jobs = self.model.jobs();
        jobs.ids()
            .filter_map(|id| jobs.status(id).cloned())
            .collect()
    }

    fn status(&self, n: usize) -> JobStatus {
        self.statuses()[n].clone()
    }

    async fn job_is(&mut self, n: usize, state: JobState) {
        self.until(&format!("job {n} {state:?}"), |m| {
            let id = m.jobs().ids().nth(n);
            // As of the last drain: what `job.resume` and `job.retry` go by.
            id.and_then(|id| m.jobs().status(id))
                .is_some_and(|s| s.state == state)
        })
        .await;
    }

    /// Wait until job `n` has `bytes` on disk.
    async fn job_has(&mut self, n: usize, bytes: u64) {
        self.until(&format!("job {n} at {bytes}"), |m| {
            let id = m.jobs().ids().nth(n);
            id.and_then(|id| m.jobs().status(id))
                .is_some_and(|s| s.bytes == bytes)
        })
        .await;
    }

    fn job(&mut self, action: &str, n: usize) {
        let id = self.ids()[n].as_str().to_string();
        let spec = actions::action(action).expect("an action");
        let invocation = Invocation::new(spec).arg("job", Value::str(id));
        assert_eq!(self.invoke(&invocation), Outcome::Accepted, "{action}");
    }

    fn ask(&mut self, id: u64) -> Outcome {
        let spec = actions::action("game.download").expect("an action");
        let invocation = Invocation::new(spec).arg("id", Value::str(id.to_string()));
        self.invoke(&invocation)
    }

    /// The text of the plan card, once it is up.
    async fn card(&mut self) -> String {
        self.until("a plan card", |m| {
            matches!(m.modal(), Some(Modal::PlanCard { .. }))
        })
        .await;
        let Some(Modal::PlanCard { plan, .. }) = self.model.modal() else {
            unreachable!()
        };
        format!("{plan:?}")
    }

    /// The card is seen for long enough, and its confirm control pressed.
    fn confirm(&mut self) {
        let ticket = self.model.modal().expect("a screen").ticket();
        self.model
            .presented(ticket, InputEvent(self.input), self.now);
        self.now += DWELL;
        let press = Press {
            event: self.next(),
            at: self.now,
            repeat: false,
        };
        assert_eq!(self.model.confirm(ticket, press), Outcome::Accepted);
        self.model.pump();
    }

    /// Ask for product `id`, agree, and wait for the job.
    async fn start(&mut self, id: u64) {
        let before = self.ids().len();
        assert_eq!(self.ask(id), Outcome::Accepted);
        self.card().await;
        self.confirm();
        self.until("a job", |m| m.jobs().ids().count() > before)
            .await;
    }

    async fn error(&mut self) -> gogo_gui_model::ErrorView {
        self.until("an error", |m| {
            matches!(m.modal(), Some(Modal::Error { .. }))
        })
        .await;
        let Some(Modal::Error { error, .. }) = self.model.modal() else {
            unreachable!()
        };
        error.clone()
    }
}

fn assert_complete(dir: &std::path::Path, name: &str) {
    let dest = dir.join(format!("{name}.bin"));
    assert_eq!(std::fs::read(&dest).expect("the file"), contents(name));
    assert!(!sidecar::part_path(&dest).exists());
}

fn assert_resumable(dir: &std::path::Path, name: &str) {
    let dest = dir.join(format!("{name}.bin"));
    assert!(!dest.exists(), "no final name");
    assert!(sidecar::part_path(&dest).exists());
    assert!(sidecar::plan_path(&dest).exists());
}

/// How often anything under `prefix` was asked for.
async fn asked(server: &MockServer, prefix: &str) -> usize {
    let requests = server.received_requests().await.expect("recording");
    requests
        .iter()
        .filter(|r| r.url.path().starts_with(prefix))
        .count()
}

async fn downloads_asked(server: &MockServer) -> usize {
    asked(server, "/dl/").await + asked(server, "/sum/").await + asked(server, "/cdn/").await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_runs_only_after_confirming_on_the_drawn_plan_card() {
    let server = super::super::tests::gog().await;
    // Over the fixed listing of product 10.
    let scratch = Scratch::new();
    Mock::given(path("/products/10"))
        .respond_with(listing(&server, 10, &["aaa"], "1.0"))
        .with_priority(1)
        .mount(&server)
        .await;
    serve(&server, "aaa", [Duration::ZERO; 3]).await;
    let engine = Arc::new(Engine::new(scratch.parts(&server, u64::MAX)));
    let backend = CoreBackend::new(engine, tokio::runtime::Handle::current(), None);
    let data = Data::core(backend, None, || {});
    let mut driven = Driven {
        app: App::new(None, data).expect("the app"),
        now: Duration::ZERO,
        seen: String::new(),
    };
    driven.until("Made Up").await;
    driven.press("Details of Made Up");
    driven.until("Download files").await;
    driven.press("Files");
    // The files tab has the table of what there is.
    driven.until("Setup").await;
    driven.press("Download");
    driven.until("Nothing changes until you confirm").await;
    assert_eq!(
        downloads_asked(&server).await,
        0,
        "planning asks for no file"
    );
    assert!(!scratch.downloads().exists(), "and creates nothing");
    for _ in 0..20 {
        driven.step();
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        downloads_asked(&server).await,
        0,
        "seeing the card is no answer"
    );
    assert!(!scratch.downloads().exists());

    driven.press("Download");
    let started = Instant::now();
    while !scratch.game(10).join("aaa.bin").is_file() {
        driven.step();
        assert!(started.elapsed() < SOON, "never downloaded");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_complete(&scratch.game(10), "aaa");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_card_says_what_where_and_how_much() {
    let server = MockServer::start().await;
    product(&server, 1, &["aaa", "bbb"], "1.0").await;
    let mut rig = Rig::with_space(&server, 1000);
    assert_eq!(rig.ask(1), Outcome::Accepted);
    let card = rig.card().await;
    for said in [
        "Save to ",
        "Game 1",
        "Setup · Windows · English · 1.0 · part 2 of 2",
        "download: Some(30)",
        "free: Some(1000)",
        "these are for Windows",
    ] {
        assert!(card.contains(said), "{said}: {card}");
    }
    assert!(!card.contains("http"), "{card}");
    assert!(!rig.scratch.downloads().exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn too_little_space_refuses_the_plan() {
    let server = MockServer::start().await;
    product(&server, 1, &["aaa"], "1.0").await;
    let mut rig = Rig::with_space(&server, 10);
    rig.ask(1);
    let error = rig.error().await;
    assert_eq!(
        (error.class.as_str(), error.exit_class.as_str()),
        ("Disk", "6")
    );
    assert!(rig.ids().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_one_of_two_downloads_leaves_the_other() {
    let server = MockServer::start().await;
    product(&server, 1, &["aaa"], "1.0").await;
    product(&server, 2, &["bbb"], "1.0").await;
    serve(&server, "aaa", [Duration::ZERO, Duration::ZERO, NEVER]).await;
    let slow = Duration::from_millis(600);
    serve(&server, "bbb", [Duration::ZERO, Duration::ZERO, slow]).await;
    let mut rig = Rig::new(&server);
    rig.start(1).await;
    rig.start(2).await;
    rig.job_has(0, 10).await;
    rig.job_is(1, JobState::Running).await;
    // What a skin's queue reads comes from these jobs.
    let key = SourceKey::new("queue.summary", &[] as &[&str]).expect("a source");
    let Some(Value::Map(summary)) = rig.model.value(&key) else {
        panic!("no queue summary");
    };
    assert_eq!(summary["active"], Value::Num(2.0));
    let status = rig.status(0);
    assert_eq!((status.total, status.files[0].chunks_total), (Some(15), 3));
    assert_eq!(status.files[0].state, "downloading");
    assert!(status.game.is_some());

    rig.job("job.cancel", 0);
    rig.job_is(0, JobState::Cancelled).await;
    rig.job_is(1, JobState::Done).await;
    assert_eq!(rig.status(0).state, JobState::Cancelled, "it stays so");
    assert_resumable(&rig.scratch.game(1), "aaa");
    assert_complete(&rig.scratch.game(2), "bbb");
    assert!(rig.model.modal().is_none(), "a cancel is no error");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paused_download_resumes_without_a_card_and_verifies() {
    let server = MockServer::start().await;
    product(&server, 1, &["aaa"], "1.0").await;
    serve_stalling_once(&server, "aaa").await;
    let mut rig = Rig::new(&server);
    rig.start(1).await;
    rig.job_has(0, 10).await;

    rig.job("job.pause", 0);
    rig.job_is(0, JobState::Paused).await;
    assert_resumable(&rig.scratch.game(1), "aaa");
    // A paused game takes no second download.
    rig.ask(1);
    assert_eq!(rig.error().await.title, "Already in the queue");
    rig.model.dismiss();

    let first = asked(&server, "/cdn/aaa").await;
    rig.job("job.resume", 0);
    rig.job_is(0, JobState::Done).await;
    assert!(rig.model.modal().is_none(), "the same files: no card");
    assert_eq!(
        asked(&server, "/cdn/aaa").await,
        first + 1,
        "one chunk left"
    );
    assert_complete(&rig.scratch.game(1), "aaa");
    let done = rig.status(0);
    assert_eq!((done.bytes, done.total, done.rate), (15, Some(15), 0));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plan_that_changed_while_paused_is_shown_again() {
    let server = MockServer::start().await;
    product(&server, 1, &["aaa"], "1.0").await;
    serve_stalling_once(&server, "aaa").await;
    let mut rig = Rig::new(&server);
    rig.start(1).await;
    rig.job_has(0, 10).await;
    rig.job("job.pause", 0);
    rig.job_is(0, JobState::Paused).await;

    server.reset().await;
    product(&server, 1, &["aaa"], "2.0").await;
    serve(&server, "aaa", [Duration::ZERO; 3]).await;
    rig.job("job.resume", 0);
    assert!(rig.card().await.contains("2.0"));
    assert_eq!(downloads_asked(&server).await, 0, "not before the answer");

    // Saying no puts the job back.
    rig.model.dismiss();
    rig.job_is(0, JobState::Paused).await;
    assert_eq!(downloads_asked(&server).await, 0);

    rig.job("job.resume", 0);
    rig.card().await;
    rig.confirm();
    rig.job_is(0, JobState::Done).await;
    assert_complete(&rig.scratch.game(1), "aaa");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_link_that_expires_mid_download_is_resolved_again() {
    let server = MockServer::start().await;
    product(&server, 1, &["aaa"], "1.0").await;
    Mock::given(path("/cdn/aaa"))
        .and(header("range", range(1).as_str()))
        .respond_with(ResponseTemplate::new(403))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    serve(&server, "aaa", [Duration::ZERO; 3]).await;
    let mut rig = Rig::new(&server);
    rig.start(1).await;
    rig.job_is(0, JobState::Done).await;
    assert_eq!(asked(&server, "/dl/aaa").await, 2);
    assert_complete(&rig.scratch.game(1), "aaa");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failure_raises_an_alert_without_an_address_and_can_be_retried() {
    let server = MockServer::start().await;
    product(&server, 1, &["aaa"], "1.0").await;
    links(&server, "aaa").await;
    Mock::given(path("/cdn/aaa"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(b"wrong".to_vec()))
        .mount(&server)
        .await;
    let mut rig = Rig::new(&server);
    rig.start(1).await;
    let error = rig.error().await;
    assert_eq!(rig.status(0).state, JobState::Failed);
    assert_eq!(
        (error.class.as_str(), error.exit_class.as_str()),
        ("CORRUPT", "5")
    );
    let said = format!("{error:?} {:?}", rig.status(0));
    assert!(!said.contains("FAKE-") && !said.contains("http"), "{said}");
    rig.model.dismiss();

    server.reset().await;
    product(&server, 1, &["aaa"], "1.0").await;
    serve(&server, "aaa", [Duration::ZERO; 3]).await;
    rig.job("job.retry", 0);
    rig.job_is(0, JobState::Done).await;
    assert_complete(&rig.scratch.game(1), "aaa");
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_of_one_game_run_one_at_a_time() {
    let server = MockServer::start().await;
    product(&server, 1, &["aaa"], "1.0").await;
    serve_stalling_once(&server, "aaa").await;
    let mut rig = Rig::new(&server);
    rig.start(1).await;
    rig.job_has(0, 10).await;
    rig.job("job.cancel", 0);
    rig.job_is(0, JobState::Cancelled).await;

    // The same game again, held at its last chunk; then the first job back.
    server.reset().await;
    product(&server, 1, &["aaa"], "1.0").await;
    let slow = Duration::from_millis(800);
    serve(&server, "aaa", [Duration::ZERO, Duration::ZERO, slow]).await;
    rig.start(1).await;
    rig.job_is(1, JobState::Running).await;
    rig.job("job.resume", 0);
    for _ in 0..20 {
        rig.model.pump();
        assert_eq!(rig.status(0).state, JobState::Queued);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    rig.job_is(1, JobState::Done).await;
    rig.job_is(0, JobState::Done).await;
    // What the second brought is not fetched again by the first.
    assert_eq!(asked(&server, "/cdn/aaa").await, 1);
    assert_complete(&rig.scratch.game(1), "aaa");
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_the_app_stops_downloads_and_keeps_what_they_had() {
    let server = MockServer::start().await;
    product(&server, 1, &["aaa"], "1.0").await;
    serve(&server, "aaa", [Duration::ZERO, Duration::ZERO, NEVER]).await;
    let mut rig = Rig::new(&server);
    rig.start(1).await;
    rig.job_has(0, 10).await;

    let started = Instant::now();
    rig.downloads.shutdown().await;
    assert!(
        started.elapsed() < super::STOPPING,
        "stopped, not timed out"
    );
    assert_eq!(*rig.downloads.going.borrow(), 0);
    assert_resumable(&rig.scratch.game(1), "aaa");
    let task_dir = rig.scratch.game(1);
    let dest = task_dir.join("aaa.bin");
    assert_eq!(sidecar::load_bitmap(&dest, 3), [0b011]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_rate_limit_reaches_the_queue_summary() {
    let server = MockServer::start().await;
    let mut rig = Rig::new(&server);
    let spec = actions::action("queue.set_rate_limit").expect("an action");
    let set = Invocation::new(spec).arg("rate", Value::Num(1_000_000.0));
    assert_eq!(rig.invoke(&set), Outcome::Accepted);
    assert_eq!(rig.downloads.rate_limit(), Some(1_000_000));
    assert_eq!(rig.invoke(&Invocation::new(spec)), Outcome::Accepted);
    assert_eq!(rig.downloads.rate_limit(), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_newer_version_under_the_same_file_id_has_nothing_on_disk() {
    let server = MockServer::start().await;
    product(&server, 1, &["aaa"], "1.0").await;
    serve(&server, "aaa", [Duration::ZERO, Duration::ZERO, NEVER]).await;
    let mut rig = Rig::new(&server);
    rig.start(1).await;
    rig.job_has(0, 10).await;
    rig.job("job.cancel", 0);
    rig.job_is(0, JobState::Cancelled).await;

    server.reset().await;
    product(&server, 1, &["aaa"], "1.0").await;
    rig.ask(1);
    assert!(rig.card().await.contains("resumes"));
    rig.model.dismiss();

    server.reset().await;
    Mock::given(path("/products/1"))
        .respond_with(listing_sized(&server, 1, &["aaa"], "2.0", 20))
        .mount(&server)
        .await;
    rig.ask(1);
    let card = rig.card().await;
    assert!(!card.contains("resumes"), "{card}");
    assert!(card.contains("download: Some(20)"), "{card}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_that_appeared_in_the_folder_shows_the_card_again() {
    let server = MockServer::start().await;
    for (id, name) in [(1, "aaa"), (2, "bbb"), (3, "ccc")] {
        product(&server, id, &[name], "1.0").await;
        serve(&server, name, [NEVER; 3]).await;
    }
    let mut rig = Rig::new(&server);
    // Two others hold both slots, so the third waits and creates nothing.
    rig.start(2).await;
    rig.start(3).await;
    rig.start(1).await;
    rig.job_is(2, JobState::Queued).await;
    rig.job("job.pause", 2);
    rig.job_is(2, JobState::Paused).await;

    let folder = rig.scratch.game(1);
    std::fs::create_dir_all(&folder).expect("the folder");
    std::fs::write(folder.join("aaa.bin"), b"someone else's").expect("a file");
    rig.job("job.resume", 2);
    assert!(rig.card().await.contains("may be replaced"));
    assert_eq!(asked(&server, "/dl/aaa").await, 0, "not before the answer");
    rig.downloads.shutdown().await;
}
