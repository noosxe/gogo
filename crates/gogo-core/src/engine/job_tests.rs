//! Per-job cancellation (design 25 §5): two jobs on one `AppContext`, one cancelled.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::app::AppContext;
use crate::engine::events::ProgressEvent;
use crate::engine::job::JobScope;
use crate::engine::limiter::GlobalLimiter;
use crate::engine::task::FileTask;
use crate::engine::tests::{TestEnv, checksum_xml, md5_hex, task_for, test_ctx};
use crate::engine::{Summary, execute_job, sidecar};

/// Far longer than any test waits: a request that only a cancel ends.
const NEVER: Duration = Duration::from_secs(60);
const SOON: Duration = Duration::from_secs(10);

/// File `name`: three 5-byte chunks, chunk `i` answered after `delays[i]`.
async fn mount_file(server: &MockServer, name: &str, delays: [Duration; 3]) {
    let chunks: Vec<String> = (0..3).map(|i| format!("{name}-{i:03}")).collect();
    assert!(chunks.iter().all(|c| c.len() == 5), "{chunks:?}");
    let xml = checksum_xml(
        &format!("{name}.bin"),
        &[
            (md5_hex(chunks[0].as_bytes()).as_str(), 0, 4),
            (md5_hex(chunks[1].as_bytes()).as_str(), 5, 9),
            (md5_hex(chunks[2].as_bytes()).as_str(), 10, 14),
        ],
    );
    Mock::given(method("GET"))
        .and(path(format!("/{name}/downlink")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "downlink": format!("{}/{name}/cdn", server.uri()),
            "checksum": format!("{}/{name}/checksum.xml", server.uri()),
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/{name}/checksum.xml")))
        .respond_with(ResponseTemplate::new(200).set_body_string(xml))
        .mount(server)
        .await;
    for (i, (chunk, delay)) in chunks.iter().zip(delays).enumerate() {
        let range = format!("bytes={}-{}", i * 5, i * 5 + 4);
        Mock::given(method("GET"))
            .and(path(format!("/{name}/cdn")))
            .and(header("Range", range.as_str()))
            .respond_with(
                ResponseTemplate::new(206)
                    .set_body_bytes(chunk.clone().into_bytes())
                    .set_delay(delay),
            )
            .mount(server)
            .await;
    }
}

fn contents(name: &str) -> String {
    (0..3).map(|i| format!("{name}-{i:03}")).collect()
}

fn task(server: &MockServer, name: &str, root: &Path) -> FileTask {
    let mut task = task_for(format!("{}/{name}/downlink", server.uri()), 15);
    task.dir = root.to_path_buf();
    task.file.id = name.to_owned();
    task
}

/// How often each chunk of `name` was asked for.
async fn asked(server: &MockServer, name: &str) -> [usize; 3] {
    let mut hits = [0; 3];
    for request in server.received_requests().await.unwrap() {
        if request.url.path() == format!("/{name}/cdn") {
            let range = request.headers.get("range").unwrap().to_str().unwrap();
            let from: usize = range["bytes=".len()..]
                .split('-')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            hits[from / 5] += 1;
        }
    }
    hits
}

fn start(ctx: &Arc<AppContext>, job: &JobScope, task: FileTask) -> JoinHandle<Summary> {
    let (ctx, job) = (Arc::clone(ctx), job.clone());
    tokio::spawn(async move { execute_job(&ctx, &job, vec![task]).await })
}

async fn finished(run: JoinHandle<Summary>) -> Summary {
    tokio::time::timeout(SOON, run)
        .await
        .expect("the job ends")
        .unwrap()
}

/// Wait until `chunks` chunks of the job are verified and on disk.
async fn verified(events: &mut tokio::sync::broadcast::Receiver<ProgressEvent>, chunks: usize) {
    let mut seen = 0;
    while seen < chunks {
        let event = tokio::time::timeout(SOON, events.recv())
            .await
            .expect("a chunk")
            .unwrap();
        seen += usize::from(matches!(event, ProgressEvent::ChunkDone { .. }));
    }
}

fn assert_cancelled(summary: Summary) {
    assert_eq!(
        (summary.ok, summary.failed, summary.worst_exit_class),
        (0, 1, Some(130)),
        "{summary:?}"
    );
}

fn assert_resumable(dest: &Path) {
    assert!(!dest.exists(), "no final name");
    assert!(sidecar::part_path(dest).exists());
    assert!(sidecar::plan_path(dest).exists());
}

async fn one_worker(root: &Path) -> Arc<AppContext> {
    let mut ctx = test_ctx(root).await;
    Arc::get_mut(&mut ctx).unwrap().cfg.download.workers = 1;
    ctx
}

#[tokio::test]
async fn cancelling_one_job_lets_the_other_finish() {
    let _env = TestEnv::new();
    let root = tempfile::tempdir().unwrap();
    let ctx = one_worker(root.path()).await;
    let server = MockServer::start().await;
    let (no, slow) = (Duration::ZERO, Duration::from_millis(300));
    mount_file(&server, "a", [no, NEVER, no]).await;
    mount_file(&server, "b", [slow, slow, slow]).await;

    let mut app_events = ctx.events.subscribe();
    let (a, b) = (ctx.job(), ctx.job());
    let (mut a_events, mut b_events) = (a.subscribe(), b.subscribe());
    let run_a = start(&ctx, &a, task(&server, "a", root.path()));
    let run_b = start(&ctx, &b, task(&server, "b", root.path()));

    verified(&mut a_events, 1).await;
    a.cancel_token().cancel();
    assert_cancelled(finished(run_a).await);
    let summary = finished(run_b).await;
    assert_eq!((summary.ok, summary.failed), (1, 0), "{summary:?}");

    let b_dest = root.path().join("b.bin");
    assert_eq!(std::fs::read_to_string(&b_dest).unwrap(), contents("b"));
    assert!(!sidecar::part_path(&b_dest).exists());
    assert_resumable(&root.path().join("a.bin"));
    assert!(!b.cancel_token().is_cancelled() && !ctx.cancel.is_cancelled());
    // Each job's events are its own.
    assert!(app_events.try_recv().is_err());
    let mut done = Vec::new();
    while let Ok(event) = b_events.try_recv() {
        if let ProgressEvent::Done { ok, failed, .. } = event {
            done.push((ok, failed));
        }
    }
    assert_eq!(done, [(1, 0)]);

    // A new job resumes the cancelled file from its verified chunk.
    server.reset().await;
    mount_file(&server, "a", [no, no, no]).await;
    let summary = finished(start(&ctx, &ctx.job(), task(&server, "a", root.path()))).await;
    assert_eq!(summary.ok, 1, "{summary:?}");
    assert_eq!(
        std::fs::read_to_string(root.path().join("a.bin")).unwrap(),
        contents("a")
    );
    assert_eq!(asked(&server, "a").await, [0, 1, 1]);
}

#[tokio::test]
async fn a_cancel_during_a_resume_keeps_what_was_there() {
    let _env = TestEnv::new();
    let root = tempfile::tempdir().unwrap();
    let ctx = one_worker(root.path()).await;
    let server = MockServer::start().await;
    let no = Duration::ZERO;
    let dest = root.path().join("a.bin");

    // First run: one chunk, then cancelled. Second: one more, then cancelled.
    for (done, delays) in [(1, [no, NEVER, no]), (2, [no, no, NEVER])] {
        server.reset().await;
        mount_file(&server, "a", delays).await;
        let job = ctx.job();
        let mut events = job.subscribe();
        let run = start(&ctx, &job, task(&server, "a", root.path()));
        verified(&mut events, 1).await;
        job.cancel_token().cancel();
        assert_cancelled(finished(run).await);
        assert_resumable(&dest);
        let bits = sidecar::load_bitmap(&dest, 3);
        let kept = (0..3).filter(|&i| sidecar::is_done(&bits, i)).count();
        assert_eq!(kept, done);
    }

    server.reset().await;
    mount_file(&server, "a", [no, no, no]).await;
    let summary = finished(start(&ctx, &ctx.job(), task(&server, "a", root.path()))).await;
    assert_eq!(summary.ok, 1, "{summary:?}");
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), contents("a"));
    assert_eq!(asked(&server, "a").await, [0, 0, 1]);
}

#[tokio::test]
async fn the_app_token_stops_every_job() {
    let _env = TestEnv::new();
    let root = tempfile::tempdir().unwrap();
    let ctx = one_worker(root.path()).await;
    let server = MockServer::start().await;
    mount_file(&server, "a", [Duration::ZERO, NEVER, NEVER]).await;
    mount_file(&server, "b", [Duration::ZERO, NEVER, NEVER]).await;

    let (a, b) = (ctx.job(), ctx.job());
    let (mut a_events, mut b_events) = (a.subscribe(), b.subscribe());
    let run_a = start(&ctx, &a, task(&server, "a", root.path()));
    let run_b = start(&ctx, &b, task(&server, "b", root.path()));
    verified(&mut a_events, 1).await;
    verified(&mut b_events, 1).await;
    ctx.cancel.cancel();
    assert_cancelled(finished(run_a).await);
    assert_cancelled(finished(run_b).await);
    assert_resumable(&root.path().join("a.bin"));
    assert_resumable(&root.path().join("b.bin"));
}

#[tokio::test]
async fn a_job_cancelled_before_a_file_starts_leaves_nothing() {
    let _env = TestEnv::new();
    let root = tempfile::tempdir().unwrap();
    let ctx = test_ctx(root.path()).await;
    let server = MockServer::start().await;
    mount_file(&server, "a", [Duration::ZERO; 3]).await;

    let job = ctx.job();
    job.cancel_token().cancel();
    assert_cancelled(finished(start(&ctx, &job, task(&server, "a", root.path()))).await);
    assert!(server.received_requests().await.unwrap().is_empty());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

/// A job waiting for the shared limiter's budget is cancelled; the limiter then
/// serves the next job.
#[tokio::test]
async fn a_cancel_leaves_the_shared_limiter_usable() {
    let _env = TestEnv::new();
    let root = tempfile::tempdir().unwrap();
    let mut ctx = one_worker(root.path()).await;
    // 20 B/s: the 2000-byte file would wait about 100 s, far past `SOON`.
    Arc::get_mut(&mut ctx).unwrap().limiter = Arc::new(GlobalLimiter::new(20));
    let server = MockServer::start().await;
    let big = vec![b'x'; 2000];
    let xml = checksum_xml("big.bin", &[(md5_hex(&big).as_str(), 0, 1999)]);
    Mock::given(method("GET"))
        .and(path("/big/downlink"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "downlink": format!("{}/big/cdn", server.uri()),
            "checksum": format!("{}/big/checksum.xml", server.uri()),
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/big/checksum.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_string(xml))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/big/cdn"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(big))
        .mount(&server)
        .await;
    mount_file(&server, "b", [Duration::ZERO; 3]).await;

    let a = ctx.job();
    let run_a = start(&ctx, &a, task(&server, "big", root.path()));
    tokio::time::timeout(SOON, async {
        while ctx.limiter.waiting() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the job reaches the limiter");
    a.cancel_token().cancel();
    assert_cancelled(finished(run_a).await);
    assert_eq!(ctx.limiter.waiting(), 0);

    let summary = finished(start(&ctx, &ctx.job(), task(&server, "b", root.path()))).await;
    assert_eq!(summary.ok, 1, "{summary:?}");
}

/// A server that sends half a chunk and then nothing: no timeout would end it.
#[tokio::test]
async fn a_cancel_ends_a_stalled_stream() {
    let _env = TestEnv::new();
    let root = tempfile::tempdir().unwrap();
    let ctx = one_worker(root.path()).await;
    let server = MockServer::start().await;

    let stalled = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let cdn = format!("http://{}/cdn", stalled.local_addr().unwrap());
    let (sent, half_sent) = tokio::sync::oneshot::channel();
    let (_hold, held) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        let (mut socket, _) = stalled.accept().unwrap();
        let _ = socket.read(&mut [0u8; 4096]);
        socket
            .write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 5\r\n\r\nhe")
            .unwrap();
        let _ = sent.send(());
        // Open until the test ends.
        let _ = held.recv();
    });
    let xml = checksum_xml("x.bin", &[(md5_hex(b"hello").as_str(), 0, 4)]);
    Mock::given(method("GET"))
        .and(path("/x/downlink"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "downlink": cdn,
            "checksum": format!("{}/x/checksum.xml", server.uri()),
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/x/checksum.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_string(xml))
        .mount(&server)
        .await;

    let job = ctx.job();
    let run = start(&ctx, &job, task(&server, "x", root.path()));
    tokio::time::timeout(SOON, half_sent)
        .await
        .unwrap()
        .unwrap();
    job.cancel_token().cancel();
    assert_cancelled(finished(run).await);
    assert_resumable(&root.path().join("x.bin"));
}
