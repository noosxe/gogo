//! GOG's remote config for a game's client (design 22 §1.4): which cloud-save
//! locations the game declares. Public, no auth.
//!
//! `GET remote-config.gog.com/components/galaxy_client/clients/<clientId>
//! ?component_version=<COMPONENT_VERSION>`. The answer is cached on disk
//! (`<cache>/remote-config/<clientId>.json`, no secrets in it) so a later,
//! offline command can still resolve the locations.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// The `component_version` gogo asks for (design 22 D13). GOG answers per
/// version, so this is a constant; the ignored live test
/// (`live_component_version_still_answers`) fails when GOG stops answering it
/// the way the fixtures say.
pub const COMPONENT_VERSION: &str = "2.0.45";

/// The service's base URL.
pub const BASE: &str = "https://remote-config.gog.com";

/// How long a cached answer counts as fresh.
pub const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, thiserror::Error)]
pub enum RemoteConfigError {
    #[error("not a GOG client id: {0:?}")]
    BadClientId(String),
    #[error("remote config request failed: {0}")]
    Http(String),
    #[error("remote config for client {client_id} answered {status}")]
    Status { client_id: String, status: u16 },
    #[error("remote config isn't the JSON gogo expects: {0}")]
    Parse(String),
}

/// One declared location: the cloud dir's name and the Windows path with
/// `<?VAR?>` variables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawLocation {
    pub name: String,
    pub location: String,
}

/// `content.<platform>.cloudStorage`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudStorage {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub locations: Vec<RawLocation>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Platform {
    #[serde(rename = "cloudStorage", default)]
    cloud_storage: Option<CloudStorage>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SharedStorage {
    #[serde(default)]
    quota: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Content {
    #[serde(rename = "Windows", default)]
    windows: Option<Platform>,
    #[serde(rename = "MacOS", default)]
    macos: Option<Platform>,
    #[serde(rename = "Linux", default)]
    linux: Option<Platform>,
    #[serde(rename = "cloudStorage", default)]
    cloud_storage: Option<SharedStorage>,
}

/// The parts of a remote config gogo reads. Unknown fields are ignored.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RemoteConfig {
    #[serde(default)]
    content: Content,
}

impl RemoteConfig {
    /// Parse an answer. One without a `content` object (`{}`, an error body,
    /// a captive portal's JSON) isn't a remote config: it would read as "no
    /// cloud saves" and be cached as such.
    pub fn parse(bytes: &[u8]) -> Result<Self, RemoteConfigError> {
        let v: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|e| RemoteConfigError::Parse(e.to_string()))?;
        if !v.get("content").is_some_and(serde_json::Value::is_object) {
            return Err(RemoteConfigError::Parse("no `content` object".into()));
        }
        serde_json::from_value(v).map_err(|e| RemoteConfigError::Parse(e.to_string()))
    }

    /// `content.Windows.cloudStorage`: what both lanes use (they run Windows
    /// builds, design 22 §3.1). `None` when the game has no `Windows` key.
    pub fn windows_storage(&self) -> Option<&CloudStorage> {
        self.content
            .windows
            .as_ref()
            .and_then(|p| p.cloud_storage.as_ref())
    }

    /// `content.MacOS.cloudStorage`, for the native-build work of §11.
    pub fn macos_storage(&self) -> Option<&CloudStorage> {
        self.content
            .macos
            .as_ref()
            .and_then(|p| p.cloud_storage.as_ref())
    }

    /// `content.Linux.cloudStorage`, for the native-build work of §11.
    pub fn linux_storage(&self) -> Option<&CloudStorage> {
        self.content
            .linux
            .as_ref()
            .and_then(|p| p.cloud_storage.as_ref())
    }

    /// `content.cloudStorage.quota`, bytes.
    pub fn quota(&self) -> Option<u64> {
        self.content.cloud_storage.as_ref().and_then(|s| s.quota)
    }
}

/// A client id goes into a URL and a file name: GOG's are digits, but accept
/// any short alphanumeric run and nothing else.
pub fn check_client_id(client_id: &str) -> Result<(), RemoteConfigError> {
    if !client_id.is_empty()
        && client_id.len() <= 64
        && client_id.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        Ok(())
    } else {
        Err(RemoteConfigError::BadClientId(client_id.to_string()))
    }
}

/// The request URL under `base`.
pub fn url(base: &str, client_id: &str) -> Result<String, RemoteConfigError> {
    check_client_id(client_id)?;
    Ok(format!(
        "{}/components/galaxy_client/clients/{client_id}?component_version={COMPONENT_VERSION}",
        base.trim_end_matches('/')
    ))
}

/// Fetch the raw answer (no cache). `base` is [`BASE`] outside tests.
pub async fn fetch_raw(
    http: &reqwest::Client,
    base: &str,
    client_id: &str,
) -> Result<Vec<u8>, RemoteConfigError> {
    let url = url(base, client_id)?;
    let resp = http
        .get(&url)
        .send()
        .await
        .map_err(|e| RemoteConfigError::Http(e.without_url().to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(RemoteConfigError::Status {
            client_id: client_id.to_string(),
            status: status.as_u16(),
        });
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| RemoteConfigError::Http(e.without_url().to_string()))?;
    Ok(bytes.to_vec())
}

/// `<cache>/remote-config/<clientId>.json`.
pub fn cache_path(client_id: &str) -> Result<PathBuf, RemoteConfigError> {
    cache_path_in(&crate::state::cache::cache_dir(), client_id)
}

fn cache_path_in(cache: &Path, client_id: &str) -> Result<PathBuf, RemoteConfigError> {
    check_client_id(client_id)?;
    Ok(cache
        .join("remote-config")
        .join(format!("{client_id}.json")))
}

/// Where an answer came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The cache, within [`CACHE_TTL`].
    Cache,
    /// Fetched just now (and cached).
    Network,
    /// The network failed; an older cached answer stands in.
    StaleCache,
}

fn read_cache(path: &Path) -> Option<(RemoteConfig, Duration)> {
    let meta = std::fs::metadata(path).ok()?;
    let raw = std::fs::read(path).ok()?;
    let cfg = RemoteConfig::parse(&raw).ok()?;
    // A modification time in the future (a clock that was wrong) is no
    // proof of freshness: the entry counts as expired.
    let age = SystemTime::now()
        .duration_since(meta.modified().ok()?)
        .unwrap_or(Duration::MAX);
    Some((cfg, age))
}

/// The cached answer, whatever its age; `None` without one. No network:
/// what a rollback offline resolves the save roots from (design 22 §7.2).
pub fn load_cached(client_id: &str) -> Option<RemoteConfig> {
    read_cache(&cache_path(client_id).ok()?).map(|(c, _)| c)
}

/// Write the answer to the cache atomically (tmp + fsync + rename).
fn store(path: &Path, raw: &[u8]) -> std::io::Result<()> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.{}.{n}.tmp", std::process::id()));
    let written = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(raw)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// The remote config for `client_id`: the cache when fresh, else the network
/// (then cached), else a stale cache when the network fails. A cache that
/// can't be written is not an error.
pub async fn get(
    http: &reqwest::Client,
    base: &str,
    client_id: &str,
) -> Result<(RemoteConfig, Source), RemoteConfigError> {
    get_in(&crate::state::cache::cache_dir(), http, base, client_id).await
}

/// [`get`] with an explicit cache dir.
pub async fn get_in(
    cache: &Path,
    http: &reqwest::Client,
    base: &str,
    client_id: &str,
) -> Result<(RemoteConfig, Source), RemoteConfigError> {
    let path = cache_path_in(cache, client_id)?;
    let cached = read_cache(&path);
    if let Some((cfg, age)) = &cached
        && *age < CACHE_TTL
    {
        return Ok((cfg.clone(), Source::Cache));
    }
    let fetched = fetch_raw(http, base, client_id)
        .await
        .and_then(|raw| RemoteConfig::parse(&raw).map(|cfg| (cfg, raw)));
    match fetched {
        Ok((cfg, raw)) => {
            if let Err(e) = store(&path, &raw) {
                tracing::debug!("remote config cache not written: {e}");
            }
            Ok((cfg, Source::Network))
        }
        Err(e) => match cached {
            Some((cfg, _)) => Ok((cfg, Source::StaleCache)),
            None => Err(e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    pub(crate) const CYBERPUNK_LIKE: &str = r#"{
      "content": {
        "Windows": {"cloudStorage": {"enabled": true, "locations": [
          {"name": "saves", "location": "<?SAVED_GAMES?>/Example Studio/Example Game"}]}},
        "MacOS": {"cloudStorage": {"enabled": false, "locations": []}},
        "cloudStorage": {"quota": 2097152000},
        "somethingElse": {"a": 1}
      }
    }"#;

    #[test]
    fn parses_windows_storage_and_quota() {
        let c = RemoteConfig::parse(CYBERPUNK_LIKE.as_bytes()).unwrap();
        let w = c.windows_storage().unwrap();
        assert!(w.enabled);
        assert_eq!(w.locations.len(), 1);
        assert_eq!(w.locations[0].name, "saves");
        assert_eq!(c.quota(), Some(2_097_152_000));
        assert!(!c.macos_storage().unwrap().enabled);
        assert!(c.linux_storage().is_none());
    }

    #[test]
    fn missing_platform_keys_are_not_errors() {
        let c = RemoteConfig::parse(br#"{"content":{}}"#).unwrap();
        assert!(c.windows_storage().is_none());
        let c = RemoteConfig::parse(br#"{"content":{"Windows":{}}}"#).unwrap();
        assert!(c.windows_storage().is_none());
    }

    #[test]
    fn an_answer_without_a_content_object_is_no_remote_config() {
        for body in [
            "{}",
            r#"{"error":"nope"}"#,
            r#"{"content":null}"#,
            r#"{"content":[]}"#,
            r#"{"content":"x"}"#,
            "[]",
            "null",
            "not json",
            "",
        ] {
            assert!(RemoteConfig::parse(body.as_bytes()).is_err(), "{body:?}");
        }
    }

    #[test]
    fn url_has_the_pinned_component_version() {
        let u = url("https://example.test/", "12345").unwrap();
        assert_eq!(
            u,
            "https://example.test/components/galaxy_client/clients/12345?component_version=2.0.45"
        );
    }

    #[test]
    fn client_ids_that_could_escape_a_url_or_a_path_are_refused() {
        for bad in ["", "../x", "a/b", "a?b", "a b", "a\\b", "é"] {
            assert!(check_client_id(bad).is_err(), "{bad:?}");
            assert!(url(BASE, bad).is_err());
            assert!(cache_path(bad).is_err());
        }
    }

    #[tokio::test]
    async fn fetches_caches_and_falls_back_to_stale() {
        let home = tempfile::tempdir().unwrap();
        let cache = home.path();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/components/galaxy_client/clients/777"))
            .and(query_param("component_version", COMPONENT_VERSION))
            .respond_with(ResponseTemplate::new(200).set_body_string(CYBERPUNK_LIKE))
            .expect(1)
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        let (cfg, src) = get_in(cache, &http, &server.uri(), "777").await.unwrap();
        assert_eq!(src, Source::Network);
        assert!(cfg.windows_storage().unwrap().enabled);
        assert!(cache_path_in(cache, "777").unwrap().exists());
        // Fresh cache: no second request (expect(1)).
        let (_, src) = get_in(cache, &http, &server.uri(), "777").await.unwrap();
        assert_eq!(src, Source::Cache);
        assert!(read_cache(&cache_path_in(cache, "777").unwrap()).is_some());
        assert!(read_cache(&cache_path_in(cache, "888").unwrap()).is_none());

        // Age the cache past the TTL, then fail the network: stale stands in.
        let old = SystemTime::now() - CACHE_TTL - Duration::from_secs(60);
        let f = std::fs::File::options()
            .write(true)
            .open(cache_path_in(cache, "777").unwrap())
            .unwrap();
        f.set_modified(old).unwrap();
        drop(server);
        let (_, src) = get_in(cache, &http, "http://127.0.0.1:1", "777")
            .await
            .unwrap();
        assert_eq!(src, Source::StaleCache);
        // No cache and no network is an error.
        assert!(
            get_in(cache, &http, "http://127.0.0.1:1", "999")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn an_http_error_status_is_an_error() {
        let home = tempfile::tempdir().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let err = get_in(home.path(), &reqwest::Client::new(), &server.uri(), "1")
            .await
            .unwrap_err();
        assert!(matches!(err, RemoteConfigError::Status { status: 404, .. }));
    }

    async fn mock_serving(status: u16, body: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn a_bodyless_or_foreign_200_is_neither_used_nor_cached() {
        let home = tempfile::tempdir().unwrap();
        let cache = home.path();
        let http = reqwest::Client::new();
        for body in ["{}", r#"{"error":"captive"}"#] {
            let server = mock_serving(200, body).await;
            let err = get_in(cache, &http, &server.uri(), "5").await.unwrap_err();
            assert!(matches!(err, RemoteConfigError::Parse(_)), "{body}");
            assert!(!cache_path_in(cache, "5").unwrap().exists());
        }
        // With a good (stale) cache, that answer falls back to it.
        store(
            &cache_path_in(cache, "5").unwrap(),
            CYBERPUNK_LIKE.as_bytes(),
        )
        .unwrap();
        age(cache, "5", CACHE_TTL + Duration::from_secs(60));
        let server = mock_serving(200, "{}").await;
        let (cfg, src) = get_in(cache, &http, &server.uri(), "5").await.unwrap();
        assert_eq!(src, Source::StaleCache);
        assert!(cfg.windows_storage().unwrap().enabled);
    }

    fn age(cache: &Path, id: &str, by: Duration) {
        let f = std::fs::File::options()
            .write(true)
            .open(cache_path_in(cache, id).unwrap())
            .unwrap();
        f.set_modified(SystemTime::now() - by).unwrap();
    }

    #[tokio::test]
    async fn a_cache_stamped_in_the_future_is_expired() {
        let home = tempfile::tempdir().unwrap();
        let cache = home.path();
        let path = cache_path_in(cache, "6").unwrap();
        store(&path, CYBERPUNK_LIKE.as_bytes()).unwrap();
        let f = std::fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(30 * 24 * 3600))
            .unwrap();
        drop(f);
        let (_, age) = read_cache(&path).unwrap();
        assert!(age >= CACHE_TTL);
        // So the network is asked, not the entry trusted.
        let server = mock_serving(200, CYBERPUNK_LIKE).await;
        let (_, src) = get_in(cache, &reqwest::Client::new(), &server.uri(), "6")
            .await
            .unwrap();
        assert_eq!(src, Source::Network);
    }

    #[tokio::test]
    async fn a_corrupt_cache_is_a_miss_with_or_without_the_network() {
        let home = tempfile::tempdir().unwrap();
        let cache = home.path();
        let http = reqwest::Client::new();
        let path = cache_path_in(cache, "7").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for junk in [&b"\x00\xffgarbage"[..], b"{}", b"", b"{\"content\":"] {
            std::fs::write(&path, junk).unwrap();
            // Offline: an error, not a bogus "no cloud saves".
            assert!(
                get_in(cache, &http, "http://127.0.0.1:1", "7")
                    .await
                    .is_err(),
                "{junk:?}"
            );
            // Online: refetched and the cache repaired.
            std::fs::write(&path, junk).unwrap();
            let server = mock_serving(200, CYBERPUNK_LIKE).await;
            let (_, src) = get_in(cache, &http, &server.uri(), "7").await.unwrap();
            assert_eq!(src, Source::Network);
            assert!(read_cache(&path).is_some());
        }
    }

    #[test]
    fn stores_leave_no_tmp_files_and_unique_names() {
        let home = tempfile::tempdir().unwrap();
        let path = cache_path_in(home.path(), "8").unwrap();
        store(&path, b"{\"content\":{}}").unwrap();
        store(&path, b"{\"content\":{}}").unwrap();
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["8.json"]);
        // A failed write removes its tmp file: the target is a dir.
        let dir_target = home.path().join("remote-config/9.json");
        std::fs::create_dir_all(&dir_target).unwrap();
        assert!(store(&dir_target, b"x").is_err());
        assert!(
            std::fs::read_dir(home.path().join("remote-config"))
                .unwrap()
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().ends_with(".tmp"))
        );
    }

    /// Live, read-only check of design 22 D13: GOG still answers
    /// `COMPONENT_VERSION` with the shape the code reads. Opt in with
    /// `GOGO_LIVE_CLIENT_ID=<a game's client id> cargo nextest run
    /// --run-ignored only live_component_version` (from a game's
    /// `goggame-*.info`). Needs no account.
    #[tokio::test]
    #[ignore = "live network"]
    async fn live_component_version_still_answers() {
        let client_id = std::env::var("GOGO_LIVE_CLIENT_ID")
            .expect("set GOGO_LIVE_CLIENT_ID to a game's client id");
        let http = crate::gog::http().unwrap();
        let raw = fetch_raw(&http, BASE, &client_id).await.unwrap();
        let cfg = RemoteConfig::parse(&raw).unwrap();
        let w = cfg
            .windows_storage()
            .expect("Windows cloudStorage is present");
        assert!(
            w.locations.iter().all(|l| l.location.contains("<?")),
            "locations carry <?VAR?> placeholders"
        );
        assert!(cfg.quota().is_some());
    }
}
