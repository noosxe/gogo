//! Engine integration tests (wiremock CDN + static-token session) — the `08` §3
//! scenarios: happy path, resume, corrupt-chunk retry, persistent corruption,
//! `available="false"` skip, adoption, disk preflight.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::app::AppContext;
use crate::auth::TokenProvider;
use crate::config::Config;
use crate::engine::task::{FileTask, Kind, Status};
use crate::engine::{FreeSpace, execute};
use crate::gog::error::AuthError;

struct StaticToken;
#[async_trait::async_trait]
impl TokenProvider for StaticToken {
    async fn bearer(&self) -> Result<String, AuthError> {
        Ok("test-token".into())
    }
    fn user_id(&self) -> Option<u64> {
        Some(1)
    }
}

struct TinySpace(u64);
impl FreeSpace for TinySpace {
    fn available_bytes(&self, _path: &std::path::Path) -> std::io::Result<u64> {
        Ok(self.0)
    }
}

fn md5_hex(data: &[u8]) -> String {
    use md5::Digest;
    crate::fmt::md5_hex(md5::Md5::new_with_prefix(data))
}

async fn test_ctx(root: &std::path::Path) -> Arc<AppContext> {
    let mut cfg = Config::default();
    cfg.download.dir = Some(root.to_string_lossy().into_owned());
    cfg.download.workers = 2;
    cfg.network.retries = 2;
    Arc::new(AppContext {
        cfg,
        http: crate::gog::http().unwrap(),
        session: Some(Arc::new(StaticToken)),
        state: Mutex::new(crate::state::State::default()),
        limiter: Arc::new(crate::engine::limiter::GlobalLimiter::unlimited()),
        events: crate::engine::events::channel().0,
        cancel: tokio_util::sync::CancellationToken::new(),
        disk_check: false,
        free_space: Arc::new(TinySpace(u64::MAX)),
    })
}

fn task_for(downlink_url: String, size: u64) -> FileTask {
    let entry_json = format!(
        r#"{{"id":"en1installer0","os":"windows","language":"en","name":"N",
            "version":"1.0","total_size":{size},
            "files":[{{"id":"en1installer0","size":{size},"downlink":"{downlink_url}"}}]}}"#
    );
    let entry: crate::gog::catalog::InstallerEntry = serde_json::from_str(&entry_json).unwrap();
    let file = entry.files[0].clone();
    FileTask {
        id: uuid::Uuid::new_v4(),
        product_id: 1,
        dlc_id: None,
        product_title: Some("Game".into()),
        kind: Kind::Installer,
        entry,
        file,
        dir: PathBuf::new(),
        save_name: None,
        status: Status::New,
        total_size: size,
        url: None,
        checksum: None,
    }
}

fn checksum_xml(name: &str, chunks: &[(&str, u64, u64)]) -> String {
    let body = chunks
        .iter()
        .map(|(md5, from, to)| {
            format!(
                r#"<chunk id="{0}" from="{from}" to="{to}" method="md5">{md5}</chunk>"#,
                chunks.iter().position(|c| c.0 == *md5).unwrap()
            )
        })
        .collect::<Vec<_>>()
        .join("\n ");
    let total = chunks.last().map(|c| c.2 + 1).unwrap_or(0);
    format!(
        r#"<file name="{name}" available="1" chunks="{}" total_size="{total}">
	{body}
</file>"#,
        chunks.len()
    )
}

/// Isolates GOGO_HOME per test (state.json, caches) under the shared env mutex —
/// engine tests must never touch the user's real state/cache dirs.
struct TestEnv {
    _guard: std::sync::MutexGuard<'static, ()>,
    _dir: tempfile::TempDir,
}

impl TestEnv {
    fn new() -> Self {
        let guard = crate::test_util::env_lock();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: env mutation under the shared env mutex (see test_util).
        unsafe { std::env::set_var("GOGO_HOME", dir.path()) };
        Self {
            _guard: guard,
            _dir: dir,
        }
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        // SAFETY: same mutex.
        unsafe { std::env::remove_var("GOGO_HOME") };
    }
}

/// Wiremock responder with per-call bodies (corrupt-then-good, always-corrupt).
struct Sequence {
    bodies: Mutex<Vec<Vec<u8>>>,
}
impl wiremock::Respond for Sequence {
    fn respond(&self, _request: &wiremock::Request) -> wiremock::ResponseTemplate {
        let mut bodies = self.bodies.lock().unwrap();
        let body = bodies
            .first()
            .cloned()
            .unwrap_or_else(|| bodies.last().cloned().unwrap_or_default());
        if bodies.len() > 1 {
            bodies.remove(0);
        }
        wiremock::ResponseTemplate::new(206).set_body_bytes(body)
    }
}

use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn mount_downlink_and_checksum(server: &MockServer, cdn_url: &str, xml: &str) {
    Mock::given(method("GET"))
        .and(path("/downlink"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "downlink": cdn_url,
            "checksum": format!("{}/checksum.xml", server.uri()),
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/checksum.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_string(xml))
        .mount(server)
        .await;
}

/// Requests that hit the CDN path, optionally filtered by Range header substring.
async fn cdn_hits(server: &wiremock::MockServer, range: Option<&str>) -> usize {
    server
        .received_requests()
        .await
        .map(|reqs| {
            reqs.iter()
                .filter(|r| {
                    r.url.path() == "/cdn"
                        && range.is_none_or(|want| {
                            r.headers
                                .get("range")
                                .and_then(|v| v.to_str().ok())
                                .is_some_and(|v| v.contains(want))
                        })
                })
                .count()
        })
        .unwrap_or(0)
}

#[tokio::test]
async fn happy_path_downloads_verifies_and_records() {
    let _env = TestEnv::new();
    let root = tempfile::tempdir().unwrap();
    let ctx = test_ctx(root.path()).await;
    let server = MockServer::start().await;

    let part0 = b"hello".to_vec();
    let part1 = b"world".to_vec();
    let xml = checksum_xml(
        "setup_game_(1).exe",
        &[
            (md5_hex(&part0).as_str(), 0, 4),
            (md5_hex(&part1).as_str(), 5, 9),
        ],
    );
    mount_downlink_and_checksum(&server, &format!("{}/cdn", server.uri()), &xml).await;

    Mock::given(method("GET"))
        .and(path("/cdn"))
        .and(header("Range", "bytes=0-4"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(part0.clone()))
        .expect(1..)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/cdn"))
        .and(header("Range", "bytes=5-9"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(part1.clone()))
        .expect(1..)
        .mount(&server)
        .await;

    let mut task = task_for(format!("{}/downlink", server.uri()), 10);
    task.dir = root.path().to_path_buf();
    let summary = execute(&ctx, vec![task.clone()]).await;

    assert_eq!(summary.ok, 1);
    assert_eq!(summary.failed, 0);
    let dest = root.path().join("setup_game_(1).exe");
    assert_eq!(std::fs::read(&dest).unwrap(), b"helloworld");
    assert!(!crate::engine::sidecar::part_path(&dest).exists());
    assert!(!crate::engine::sidecar::plan_path(&dest).exists());
    let state = ctx.state.lock().unwrap();
    assert_eq!(state.installs.len(), 1);
    assert_eq!(state.installs[0].file_id, "en1installer0");
    assert_eq!(state.installs[0].build_id.as_deref(), Some("1"));
}

#[tokio::test]
async fn resume_skips_completed_chunks() {
    let _env = TestEnv::new();
    let root = tempfile::tempdir().unwrap();
    let ctx = test_ctx(root.path()).await;
    let server = MockServer::start().await;

    let part0 = b"hello".to_vec();
    let part1 = b"world".to_vec();
    let xml = checksum_xml(
        "setup_game_(1).exe",
        &[
            (md5_hex(&part0).as_str(), 0, 4),
            (md5_hex(&part1).as_str(), 5, 9),
        ],
    );
    mount_downlink_and_checksum(&server, &format!("{}/cdn", server.uri()), &xml).await;

    // Chunk 0 already on disk (bitmap + bytes) — must never be re-fetched.
    Mock::given(method("GET"))
        .and(path("/cdn"))
        .and(header("Range", "bytes=0-4"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(part0.clone()))
        .expect(0)
        .mount(&server)
        .await;
    let _cdn1 = Mock::given(method("GET"))
        .and(path("/cdn"))
        .and(header("Range", "bytes=5-9"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(part1.clone()))
        .named("chunk1")
        .mount_as_scoped(&server)
        .await;

    // Seed the sidecar state exactly as the worker would have left it.
    let dest = root.path().join("setup_game_(1).exe");
    let doc = crate::gog::checksum::parse(&xml).unwrap();
    let plan = crate::engine::sidecar::PlanDoc::from_checksum(1, None, "en1installer0", &doc);
    crate::engine::sidecar::save_plan(&dest, &plan).unwrap();
    let mut bits = crate::engine::sidecar::load_bitmap(&dest, 2);
    crate::engine::sidecar::mark_done(&mut bits, 0);
    crate::engine::sidecar::save_bitmap(&dest, &bits).unwrap();
    let mut part_file = std::fs::File::create(crate::engine::sidecar::part_path(&dest)).unwrap();
    use std::io::Write;
    part_file.write_all(b"hello").unwrap();
    drop(part_file);

    let mut task = task_for(format!("{}/downlink", server.uri()), 10);
    task.dir = root.path().to_path_buf();
    task.status = Status::Resumable;
    let summary = execute(&ctx, vec![task]).await;

    assert_eq!(summary.ok, 1);
    assert_eq!(std::fs::read(&dest).unwrap(), b"helloworld");
    assert_eq!(
        cdn_hits(&server, Some("0-4")).await,
        0,
        "completed chunk never refetched"
    );
    assert!(cdn_hits(&server, Some("5-9")).await >= 1);
}

#[tokio::test]
async fn corrupt_chunk_retries_then_succeeds() {
    let _env = TestEnv::new();
    let root = tempfile::tempdir().unwrap();
    let ctx = test_ctx(root.path()).await;
    let server = MockServer::start().await;

    let good0 = b"hello".to_vec();
    let xml = checksum_xml("x.bin", &[(md5_hex(&good0).as_str(), 0, 4)]);
    mount_downlink_and_checksum(&server, &format!("{}/cdn", server.uri()), &xml).await;

    // First GET corrupt, second GET good — one retry, fresh Range each time (04 §4).
    Mock::given(method("GET"))
        .and(path("/cdn"))
        .respond_with(Sequence {
            bodies: Mutex::new(vec![b"XXXXX".to_vec(), good0.clone()]),
        })
        .expect(2)
        .mount(&server)
        .await;

    let mut task = task_for(format!("{}/downlink", server.uri()), 5);
    task.dir = root.path().to_path_buf();
    let summary = execute(&ctx, vec![task]).await;
    assert_eq!(summary.ok, 1);
    assert_eq!(std::fs::read(root.path().join("x.bin")).unwrap(), b"hello");
}

#[tokio::test]
async fn persistent_corruption_fails_class_5_and_keeps_sidecars() {
    let _env = TestEnv::new();
    let root = tempfile::tempdir().unwrap();
    let ctx = test_ctx(root.path()).await;
    let server = MockServer::start().await;

    let good0 = b"hello".to_vec();
    let xml = checksum_xml("x.bin", &[(md5_hex(&good0).as_str(), 0, 4)]);
    mount_downlink_and_checksum(&server, &format!("{}/cdn", server.uri()), &xml).await;

    Mock::given(method("GET"))
        .and(path("/cdn"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(b"XXXXX".to_vec()))
        .mount(&server)
        .await;

    let mut task = task_for(format!("{}/downlink", server.uri()), 5);
    task.dir = root.path().to_path_buf();
    let summary = execute(&ctx, vec![task]).await;

    assert_eq!(summary.failed, 1);
    assert_eq!(summary.worst_exit_class, Some(5));
    let dest = root.path().join("x.bin");
    assert!(!dest.exists(), "final name never appears on failure");
    assert!(
        crate::engine::sidecar::part_path(&dest).exists(),
        ".part remains for resume"
    );
    assert!(crate::engine::sidecar::plan_path(&dest).exists());
    // No chunk ever verified → the bitmap was never written; that is correct
    // (done-bits record *verified* chunks only).
}

#[tokio::test]
async fn unavailable_doc_skips_with_reason() {
    let _env = TestEnv::new();
    let root = tempfile::tempdir().unwrap();
    let ctx = test_ctx(root.path()).await;
    let server = MockServer::start().await;

    mount_downlink_and_checksum(
        &server,
        &format!("{}/cdn", server.uri()),
        r#"<file name="x.bin" available="false" notavailablemsg="Region blocked"/>"#,
    )
    .await;
    let _cdn = Mock::given(method("GET"))
        .and(path("/cdn"))
        .respond_with(ResponseTemplate::new(206))
        .expect(0)
        .mount_as_scoped(&server)
        .await;

    let mut task = task_for(format!("{}/downlink", server.uri()), 5);
    task.dir = root.path().to_path_buf();
    let summary = execute(&ctx, vec![task]).await;
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.ok, 0);
    assert_eq!(cdn_hits(&server, None).await, 0, "CDN never touched");
}

#[tokio::test]
async fn unrecorded_matching_file_is_adopted() {
    let _env = TestEnv::new();
    let root = tempfile::tempdir().unwrap();
    let ctx = test_ctx(root.path()).await;
    let server = MockServer::start().await;

    let good0 = b"hello".to_vec();
    let xml = checksum_xml("x.bin", &[(md5_hex(&good0).as_str(), 0, 4)]);
    mount_downlink_and_checksum(&server, &format!("{}/cdn", server.uri()), &xml).await;
    let _cdn = Mock::given(method("GET"))
        .and(path("/cdn"))
        .respond_with(ResponseTemplate::new(206))
        .expect(0)
        .mount_as_scoped(&server)
        .await;

    // A pre-existing, unrecorded, verifiable file (B4 adoption).
    std::fs::write(root.path().join("x.bin"), &good0).unwrap();

    let mut task = task_for(format!("{}/downlink", server.uri()), 5);
    task.dir = root.path().to_path_buf();
    task.status = Status::PendingAdoption;
    let summary = execute(&ctx, vec![task]).await;

    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.ok, 0);
    assert_eq!(
        ctx.state.lock().unwrap().installs.len(),
        1,
        "adoption writes the record"
    );
    assert_eq!(cdn_hits(&server, None).await, 0, "CDN never touched");
}

#[tokio::test]
async fn disk_preflight_fails_class_6_nothing_written() {
    let _env = TestEnv::new();
    let root = tempfile::tempdir().unwrap();
    let mut ctx = AppContext {
        cfg: Config::default(),
        http: crate::gog::http().unwrap(),
        session: Some(Arc::new(StaticToken)),
        state: Mutex::new(crate::state::State::default()),
        limiter: Arc::new(crate::engine::limiter::GlobalLimiter::unlimited()),
        events: crate::engine::events::channel().0,
        cancel: tokio_util::sync::CancellationToken::new(),
        disk_check: true,
        free_space: Arc::new(TinySpace(10)),
    };
    ctx.cfg.download.dir = Some(root.path().to_string_lossy().into_owned());
    let ctx = Arc::new(ctx);

    let task = task_for("http://unused".into(), 1000);
    let summary = execute(&ctx, vec![task]).await;
    assert_eq!(summary.worst_exit_class, Some(6));
    assert_eq!(summary.ok + summary.failed + summary.skipped, 0);
    assert!(root.path().read_dir().unwrap().next().is_none());
}

#[tokio::test]
async fn present_task_counts_as_skipped_without_calls() {
    let _env = TestEnv::new();
    let root = tempfile::tempdir().unwrap();
    let ctx = test_ctx(root.path()).await;
    let mut task = task_for("http://unused".into(), 5);
    task.status = Status::Present;
    let summary = execute(&ctx, vec![task]).await;
    assert_eq!(summary.skipped, 1);
    assert!(root.path().read_dir().unwrap().next().is_none());
}
