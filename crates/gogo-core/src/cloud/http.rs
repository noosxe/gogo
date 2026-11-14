//! The HTTP client for GOG's cloud storage host (design 22 §2.3, D3).
//!
//! Its own `reqwest::Client`, separate from gogo's (`gog::http`): the storage API
//! checks the User-Agent, so this one sends the Galaxy communication-service UA,
//! and only to the storage host: requests take a path under a fixed base, and
//! redirects aren't followed. It decodes nothing (`Content-Encoding: gzip` objects
//! come back as the stored bytes, whose MD5 is the listed `hash`).
//!
//! Storage URLs hold the user id: [`redact_url`] is the only form they are shown
//! or logged in.

use std::time::Duration;

use reqwest::{Method, RequestBuilder, Response, Url};

use super::auth::{CloudToken, reqwest_kind};
use super::error::CloudError;

/// The storage host; nothing else gets [`USER_AGENT`].
pub const STORAGE_HOST: &str = "cloudstorage.gog.com";

const STORAGE_BASE: &str = "https://cloudstorage.gog.com/v1/";

/// The storage API answers `400 invalid_version` to other UAs on most containers
/// (design 22 §1.3): this is the form Galaxy's communication service sends, which
/// passes only with the `dont_sync_marker/true installation_source/gog` suffix.
/// gogo's own token is appended. Re-verified in the live smoke (D3).
pub const USER_AGENT: &str = concat!(
    "GOGGalaxyCommunicationService/2.0.13.27 (Windows_32bit) ",
    "dont_sync_marker/true installation_source/gog ",
    "gogo/",
    env!("CARGO_PKG_VERSION")
);

/// A client for the storage host.
pub struct StorageClient {
    http: reqwest::Client,
    /// `https://cloudstorage.gog.com/v1/`; a mock server's in tests.
    base: Url,
}

impl StorageClient {
    pub fn new() -> Result<Self, CloudError> {
        Self::at(Url::parse(STORAGE_BASE).expect("constant URL"))
    }

    /// Against a mock server (`<uri>/v1/`), for tests only.
    #[cfg(any(test, feature = "test-util"))]
    pub fn with_base(base: Url) -> Result<Self, CloudError> {
        Self::at(base)
    }

    fn at(base: Url) -> Result<Self, CloudError> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            // Never to another host, and the UA with it.
            .redirect(reqwest::redirect::Policy::none())
            // The stored gzip bytes, as they are (design 22 §1.3's pitfall).
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| CloudError::Network(reqwest_kind(&e).into()))?;
        Ok(Self { http, base })
    }

    /// `<base><userId>/<clientId>[/<object>]`; `object` is `/`-separated
    /// (`saves/slot1/sav.dat`) and each part is percent-encoded on its own.
    pub fn url(&self, token: &CloudToken, client_id: &str, object: Option<&str>) -> Url {
        let mut u = self.base.clone();
        {
            let mut segs = u.path_segments_mut().expect("http base");
            segs.pop_if_empty().push(token.user_id()).push(client_id);
            if let Some(o) = object {
                segs.extend(o.split('/'));
            }
        }
        u
    }

    /// A request with the per-game bearer.
    pub fn request(
        &self,
        method: Method,
        token: &CloudToken,
        client_id: &str,
        object: Option<&str>,
    ) -> RequestBuilder {
        self.http
            .request(method, self.url(token, client_id, object))
            .bearer_auth(token.access_token())
    }

    /// Send `req`; a status that isn't 2xx is [`CloudError::Storage`], with the
    /// body's `error` code when it has one. Errors carry the redacted URL only.
    pub async fn send(&self, req: RequestBuilder) -> Result<Response, CloudError> {
        let resp = req.send().await.map_err(|e| {
            let at = e.url().map(redact_url).unwrap_or_default();
            CloudError::Network(format!("{}: {at}", reqwest_kind(&e)))
        })?;
        let status = resp.status();
        if status.is_success() {
            tracing::trace!(status = status.as_u16(), url = %redact_url(resp.url()), "cloud storage");
            return Ok(resp);
        }
        let url = redact_url(resp.url());
        let code = resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("error")?.as_str().map(short_code));
        Err(CloudError::Storage {
            status: status.as_u16(),
            url,
            code,
        })
    }
}

/// An error code from a body, as shown: a short token, never free text.
fn short_code(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(40)
        .collect()
}

/// `u` as it may be shown or logged (design 07 §4): scheme, host (and port), the
/// first path segment, and how many segments follow; the query is dropped. For
/// the storage host that keeps `/v1` and hides the user id, client id and object.
pub fn redact_url(u: &Url) -> String {
    let mut out = format!("{}://{}", u.scheme(), u.host_str().unwrap_or(""));
    if let Some(p) = u.port() {
        out.push_str(&format!(":{p}"));
    }
    let segs: Vec<&str> = u
        .path_segments()
        .map(|s| s.filter(|x| !x.is_empty()).collect())
        .unwrap_or_default();
    if let Some((first, rest)) = segs.split_first() {
        out.push('/');
        out.push_str(first);
        if !rest.is_empty() {
            out.push_str(&format!("/[redacted {} segments]", rest.len()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const USER: &str = "48151623420000000";
    const CLIENT: &str = "11111111111111111";

    fn token() -> CloudToken {
        CloudToken::new("made-up-access", USER, 3600)
    }

    async fn client(server: &MockServer) -> StorageClient {
        StorageClient::with_base(Url::parse(&format!("{}/v1/", server.uri())).unwrap()).unwrap()
    }

    #[test]
    fn user_agent_is_the_galaxy_form_plus_gogo() {
        assert!(USER_AGENT.starts_with(
            "GOGGalaxyCommunicationService/2.0.13.27 (Windows_32bit) dont_sync_marker/true installation_source/gog gogo/"
        ));
        assert!(USER_AGENT.ends_with(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn urls_are_built_under_the_storage_host() {
        let c = StorageClient::new().unwrap();
        let u = c.url(&token(), CLIENT, Some("saves/Slot 1/sav.dat"));
        assert_eq!(u.host_str(), Some(STORAGE_HOST));
        assert_eq!(
            u.path(),
            format!("/v1/{USER}/{CLIENT}/saves/Slot%201/sav.dat")
        );
        assert_eq!(
            c.url(&token(), CLIENT, None).as_str(),
            format!("https://{STORAGE_HOST}/v1/{USER}/{CLIENT}")
        );
    }

    #[test]
    fn redaction_hides_everything_after_v1() {
        let c = StorageClient::new().unwrap();
        let r = redact_url(&c.url(&token(), CLIENT, Some("saves/a/b.sav")));
        assert_eq!(r, "https://cloudstorage.gog.com/v1/[redacted 5 segments]");
        assert!(!r.contains(USER) && !r.contains(CLIENT));
        let q = Url::parse("https://cloudstorage.gog.com/v1/1/2?token=t").unwrap();
        assert_eq!(
            redact_url(&q),
            "https://cloudstorage.gog.com/v1/[redacted 2 segments]"
        );
        let bare = Url::parse("https://cloudstorage.gog.com/").unwrap();
        assert_eq!(redact_url(&bare), "https://cloudstorage.gog.com");
    }

    #[tokio::test]
    async fn sends_the_ua_and_bearer_and_keeps_gzip_bytes() {
        let server = MockServer::start().await;
        let gz = {
            use std::io::Write;
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
            e.write_all(b"made-up save").unwrap();
            e.finish().unwrap()
        };
        Mock::given(method("GET"))
            .and(path(format!("/v1/{USER}/{CLIENT}/saves/sav.dat")))
            .and(header("user-agent", USER_AGENT))
            .and(header("authorization", "Bearer made-up-access"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-encoding", "gzip")
                    .set_body_bytes(gz.clone()),
            )
            .expect(1)
            .mount(&server)
            .await;
        let c = client(&server).await;
        let req = c.request(Method::GET, &token(), CLIENT, Some("saves/sav.dat"));
        let resp = c.send(req).await.unwrap();
        assert_eq!(resp.bytes().await.unwrap().as_ref(), gz.as_slice());
        let got = &server.received_requests().await.unwrap()[0];
        assert!(
            got.headers.get("accept-encoding").is_none(),
            "asks for no encoding"
        );
    }

    #[tokio::test]
    async fn error_status_carries_code_and_redacted_url() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(400).set_body_string(
                r#"{"error":"invalid_version","error_description":"not supported with this version"}"#,
            ))
            .mount(&server)
            .await;
        let c = client(&server).await;
        let e = c
            .send(c.request(Method::GET, &token(), CLIENT, None))
            .await
            .unwrap_err();
        let CloudError::Storage { status, url, code } = &e else {
            panic!("{e:?}");
        };
        assert_eq!((*status, code.as_deref()), (400, Some("invalid_version")));
        assert!(url.ends_with("/v1/[redacted 2 segments]"));
        let text = format!("{e} {e:?}");
        assert!(!text.contains(USER) && !text.contains(CLIENT), "{text}");
    }

    #[tokio::test]
    async fn redirects_are_not_followed() {
        let elsewhere = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&elsewhere)
            .await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/x", elsewhere.uri())),
            )
            .mount(&server)
            .await;
        let c = client(&server).await;
        let e = c
            .send(c.request(Method::GET, &token(), CLIENT, None))
            .await
            .unwrap_err();
        assert!(matches!(e, CloudError::Storage { status: 302, .. }));
    }

    #[tokio::test]
    async fn network_errors_are_redacted() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let c =
            StorageClient::with_base(Url::parse(&format!("http://127.0.0.1:{port}/v1/")).unwrap())
                .unwrap();
        let e = c
            .send(c.request(Method::GET, &token(), CLIENT, None))
            .await
            .unwrap_err();
        assert!(matches!(e, CloudError::Network(_)));
        assert_eq!(e.exit_code(), 4);
        let text = format!("{e} {e:?}");
        assert!(
            !text.contains(USER) && !text.contains("made-up-access"),
            "{text}"
        );
    }
}
