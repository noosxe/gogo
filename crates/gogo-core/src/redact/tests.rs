//! Planted secrets must not survive any path into output (design 07 §4, #501).
//!
//! Every value here is made up. The errors are real `reqwest::Error`s from a
//! mock server (or a closed port) whose URLs carry the secrets in the places GOG
//! puts them: the token endpoint's query, and a CDN link's first path segment.

use super::*;
use crate::cloud::auth::{auth_error, content_error, exchange_error};
use crate::gog::depot::DepotError;
use crate::gog::error::{ApiError, AuthError};
use gog_warp::errors::ErrorKind;
use std::time::Duration;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const REFRESH: &str = "FAKE-REFRESH-3d9b51";
const SECRET: &str = "FAKE-SECRET-a0c4e2";
const CODE: &str = "FAKE-CODE-77f0d1";
const CDN: &str = "FAKE-CDN-TOKEN-9e2b";
const USER: &str = "FAKE-USERINFO-41aa";

const FAKES: [&str; 5] = [REFRESH, SECRET, CODE, CDN, USER];

#[track_caller]
fn assert_clean(what: &str, text: &str) {
    for fake in FAKES {
        assert!(!text.contains(fake), "{what}: {fake} leaked in {text:?}");
    }
}

/// Every way an error is turned into text: Display, Debug, anyhow's chain.
#[track_caller]
fn assert_error_clean<E: std::error::Error + Send + Sync + 'static>(what: &str, e: E) {
    assert_clean(what, &e.to_string());
    assert_clean(what, &format!("{e:?}"));
    let any = anyhow::Error::new(e).context("outer");
    assert_clean(what, &format!("{any:#}"));
    assert_clean(what, &format!("{any:?}"));
}

#[derive(Clone, Copy, Debug)]
enum Shape {
    /// The token endpoint answers 500 (`error_for_status`).
    Status,
    /// Nothing listens.
    Connect,
    /// The answer isn't JSON.
    Decode,
    /// The server is too slow.
    Timeout,
}

const SHAPES: [Shape; 4] = [Shape::Status, Shape::Connect, Shape::Decode, Shape::Timeout];

fn token_query() -> String {
    format!(
        "/token?grant_type=refresh_token&client_id=1&client_secret={SECRET}\
         &refresh_token={REFRESH}&code={CODE}"
    )
}

fn cdn_path() -> String {
    format!("/token=nva=1~dirs=/x~hmac={CDN}/secure/offline/setup.exe")
}

/// A real `reqwest::Error` of `shape`, its URL holding every planted secret.
async fn make(shape: Shape) -> reqwest::Error {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_millis(200))
        .build()
        .unwrap();
    let with_userinfo =
        |base: &str, path: &str| base.replacen("://", &format!("://{USER}@"), 1) + path;
    match shape {
        Shape::Connect => {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = l.local_addr().unwrap().port();
            drop(l);
            let url = with_userinfo(&format!("http://127.0.0.1:{port}"), &token_query());
            http.get(url).send().await.unwrap_err()
        }
        Shape::Status | Shape::Decode | Shape::Timeout => {
            let server = MockServer::start().await;
            let resp = match shape {
                Shape::Status => ResponseTemplate::new(500),
                // serde quotes a mistyped value in its message.
                Shape::Decode => {
                    ResponseTemplate::new(200).set_body_string(format!("\"{REFRESH}\""))
                }
                _ => ResponseTemplate::new(200).set_delay(Duration::from_secs(5)),
            };
            Mock::given(method("GET"))
                .respond_with(resp)
                .mount(&server)
                .await;
            let path = match shape {
                Shape::Decode => cdn_path() + &token_query()[6..],
                _ => token_query(),
            };
            let url = with_userinfo(&server.uri(), &path);
            let resp = http.get(url).send().await;
            match shape {
                Shape::Status => resp.unwrap().error_for_status().unwrap_err(),
                Shape::Decode => resp.unwrap().json::<u32>().await.unwrap_err(),
                _ => resp.unwrap_err(),
            }
        }
    }
}

fn warp(e: reqwest::Error) -> gog_warp::Error {
    gog_warp::Error::new(ErrorKind::Request, Some(e))
}

#[tokio::test]
async fn the_planted_errors_do_carry_the_secrets() {
    // Without this the other tests would prove nothing.
    for shape in SHAPES {
        let e = make(shape).await;
        let raw = format!("{e} {e:?}");
        assert!(
            raw.contains(REFRESH) || raw.contains(CDN),
            "{shape:?}: {raw}"
        );
        let w = warp(make(shape).await);
        let dbg = format!("{w:?}");
        assert!(FAKES.iter().any(|f| dbg.contains(f)), "{shape:?}: {dbg}");
    }
}

#[tokio::test]
async fn reqwest_errors_are_clean_on_every_path() {
    for shape in SHAPES {
        let what = format!("{shape:?}");
        assert_clean(&what, &describe_error(&make(shape).await));
        assert_error_clean(&what, NetError::from(make(shape).await));
        assert_error_clean(&what, ApiError::from(make(shape).await));
        assert_error_clean(&what, ApiError::decode(make(shape).await));
        assert_error_clean(&what, AuthError::from(make(shape).await));
        assert_error_clean(&what, auth_error(AuthError::from(make(shape).await)));
    }
}

#[tokio::test]
async fn gog_warp_errors_are_clean_on_every_path() {
    for shape in SHAPES {
        let what = format!("{shape:?}");
        assert_clean(&what, &describe_error(&warp(make(shape).await)));
        assert_error_clean(&what, AuthError::from(warp(make(shape).await)));
        assert_error_clean(
            &what,
            ApiError::from(AuthError::from(warp(make(shape).await))),
        );
        assert_error_clean(&what, DepotError::library(warp(make(shape).await)));
        assert_error_clean(&what, exchange_error(warp(make(shape).await), 1));
        assert_error_clean(&what, content_error(warp(make(shape).await), 1));
        // An io error wrapping the request error (a custom io payload).
        let io = std::io::Error::other(make(shape).await);
        assert_error_clean(
            &what,
            DepotError::library(gog_warp::Error::new(ErrorKind::Io, Some(io))),
        );
    }
}

#[tokio::test]
async fn the_kept_detail_is_still_useful() {
    let e = describe_error(&warp(make(Shape::Status).await));
    assert!(
        e.starts_with("network request error: HTTP 500 (http://127.0.0.1:"),
        "{e}"
    );
    assert!(e.ends_with("/token)"), "{e}");
    let e = describe_error(&make(Shape::Timeout).await);
    assert!(e.starts_with("timed out ("), "{e}");
    let e = describe_error(&make(Shape::Connect).await);
    assert!(e.starts_with("could not connect ("), "{e}");
    let e = describe_error(&make(Shape::Decode).await);
    // A body error carries no URL; serde gives its category and position only.
    assert_eq!(
        e,
        "the answer could not be read: invalid data at line 1 column 21"
    );
    let io = std::io::Error::from_raw_os_error(28);
    let e = describe_error(&gog_warp::Error::new(ErrorKind::Io, Some(io)));
    assert!(e.starts_with("io error: No space left on device"), "{e}");
}

#[test]
fn an_unknown_link_is_hidden() {
    #[derive(Debug)]
    struct Chatty;
    impl fmt::Display for Chatty {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "GET https://x.example/?refresh_token={REFRESH}")
        }
    }
    impl StdError for Chatty {}
    let w = gog_warp::Error::new(ErrorKind::Request, Some(Chatty));
    assert_eq!(describe_error(&w), "network request error: [detail hidden]");
    assert_eq!(describe_error(&Chatty), "[detail hidden]");
}

#[test]
fn invalid_session_is_typed() {
    let e = AuthError::from(gog_warp::Error::new(
        ErrorKind::InvalidSession,
        None::<String>,
    ));
    assert!(matches!(e, AuthError::InvalidSession));
    assert!(matches!(
        auth_error(e),
        crate::cloud::error::CloudError::InvalidSession
    ));
}

#[test]
fn a_corrupt_token_blob_is_not_quoted() {
    // A stored blob whose token sits where a number belongs.
    let blob = format!(r#"{{"x": {{"expires_in": "{REFRESH}"}}}}"#);
    let core = gog_warp::Core::new();
    let e = core.deserialize_tokens(&blob).unwrap_err();
    assert_clean("blob", &describe_error(&e));
    let e = serde_json::from_str::<u32>(&format!("\"{REFRESH}\"")).unwrap_err();
    assert!(e.to_string().contains(REFRESH), "serde quotes the value");
    assert_clean("json", &json_summary(&e));
    assert_error_clean("api json", ApiError::from(e));
}

#[test]
fn debug_of_url_holders_is_redacted() {
    let cdn = format!("https://gog-cdn-fastly.gog.com{}", cdn_path());
    let dl: crate::gog::downlink::Downlink = serde_json::from_value(serde_json::json!({
        "downlink": cdn,
        "checksum": format!("https://x.example/c.xml?token={CDN}"),
    }))
    .unwrap();
    assert_clean("downlink", &format!("{dl:?}"));
    let u = SecretUrl::new(Url::parse(&cdn).unwrap());
    assert_clean("secret url", &format!("{u} {u:?}"));
    assert_eq!(u.expose().as_str(), cdn);
    let cfg = crate::config::AuthConfig {
        client_secret: Some(SECRET.into()),
        ..Default::default()
    };
    assert_clean("auth config", &format!("{cfg:?}"));
}

#[test]
fn redact_url_forms() {
    let r = |s: &str| redact_url(&Url::parse(s).unwrap());
    // CDN: the first segment is the token.
    assert_eq!(
        r(&format!("https://gog-cdn-fastly.gog.com{}", cdn_path())),
        "https://gog-cdn-fastly.gog.com/[redacted 5 segments]"
    );
    // The storage host keeps `/v1` (the cloud form, unchanged).
    assert_eq!(
        r("https://cloudstorage.gog.com/v1/48151623420000000/111/saves/a.sav"),
        "https://cloudstorage.gog.com/v1/[redacted 4 segments]"
    );
    assert_eq!(
        r(&format!(
            "https://auth.gog.com/token{}",
            &token_query()[6..]
        )),
        "https://auth.gog.com/token"
    );
    assert_eq!(
        r(&format!(
            "https://{USER}:{SECRET}@x.example:8443/a/b#{CODE}"
        )),
        "https://x.example:8443/a/[redacted 1 segment]"
    );
    assert_eq!(r("https://x.example/"), "https://x.example");
    // Not a plain word: hidden.
    assert_eq!(
        r("https://x.example/Abc123"),
        "https://x.example/[redacted 1 segment]"
    );
}

#[test]
fn scrub_text_forms() {
    let cases = [
        format!(
            "error sending request for url (https://auth.gog.com{})",
            token_query()
        ),
        format!("GET http://127.0.0.1:9{}: boom", cdn_path()),
        format!("refresh_token={REFRESH}&client_secret={SECRET}"),
        format!(r#"{{"refresh_token":"{REFRESH}","code": "{CODE}"}}"#),
        format!(r#"Token {{ refresh_token: "{REFRESH}" }}"#),
        format!("refresh_token=\"{REFRESH}\" user=x"),
        format!("Authorization: Bearer {SECRET}"),
        format!("HTTPS://{USER}@X.EXAMPLE/a?token={CDN}"),
        format!("/token={CDN}~hmac=x/"),
    ];
    for c in &cases {
        assert_clean(c, &scrub_text(c));
    }
    assert_eq!(
        scrub_text(&cases[0]),
        "error sending request for url (https://auth.gog.com/token)"
    );
    assert_eq!(
        scrub_text("exit code 3, see https://www.gog.com/en/game/x"),
        "exit code 3, see https://www.gog.com/en/[redacted 2 segments]"
    );
    // Words that merely contain a name are left alone.
    assert_eq!(scrub_text("barcode=7 tokens=2"), "barcode=7 tokens=2");
    assert_eq!(scrub_text("plain text"), "plain text");
}

/// A download whose CDN link fails: the progress events (as `--json-progress`
/// writes them) and every captured tracing line stay clean.
#[tokio::test]
async fn a_failing_download_shows_no_cdn_token() {
    use crate::engine::events::ProgressEvent;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf::default();
    let w = buf.clone();
    let sub = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || w.clone())
        .finish();
    let _trace = tracing::subscriber::set_default(sub);

    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let cdn = format!("http://127.0.0.1:{port}{}", cdn_path());
    let events = crate::engine::tests::failing_cdn_download(&cdn).await;

    let failed = events
        .iter()
        .filter(|e| matches!(e, ProgressEvent::FileFailed { .. }))
        .count();
    assert_eq!(failed, 1, "{events:?}");
    for e in &events {
        assert_clean("event json", &serde_json::to_string(e).unwrap());
        assert_clean("event debug", &format!("{e:?}"));
    }
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert_clean("trace log", &log);
}

/// reqwest's verbose connection logging dumps raw request bytes (query and
/// `Authorization` included): no gogo crate may turn it on (design 07 §4).
#[test]
fn no_crate_turns_on_verbose_connection_logging() {
    let needle = concat!("connection_", "verbose");
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut stack = vec![crates];
    let mut seen = 0;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n != "target") {
                    stack.push(p);
                }
            } else if p.extension().is_some_and(|e| e == "rs") {
                seen += 1;
                let text = std::fs::read_to_string(&p).unwrap();
                assert!(!text.contains(needle), "{} sets {needle}", p.display());
            }
        }
    }
    assert!(seen > 50, "walked {seen} files");
}
