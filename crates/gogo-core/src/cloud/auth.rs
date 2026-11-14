//! Cloud-save auth (design 22 §2): the game's client credentials and the per-game
//! token they buy.
//!
//! The storage API refuses the Galaxy token (403); it wants a token for the game's
//! own client, exchanged with the game's `clientId`/`clientSecret` and the user's
//! Galaxy refresh token. gog-warp does the exchange (`Core::obtain_token`). It runs
//! in a separate, short-lived `Core` holding only the Galaxy token, so the per-game
//! token never reaches the main session's map and so never the token store (D4).
//!
//! SECURITY: the client secret and the per-game token live in [`Secret`]s, whose
//! `Debug` prints a placeholder and which have no `Display`. gog-warp's exchange
//! errors wrap a `reqwest::Error` whose text holds the request URL, query and all
//! (client secret, refresh token): they are mapped by kind and status only.

use std::collections::HashMap;
use std::path::Path;

use gog_warp::Core;
use gog_warp::auth::types::Token;
use gog_warp::constants::GALAXY_CLIENT_ID;
use gog_warp::content_system::types::{Build, Manifest, Platform};
use gog_warp::errors::ErrorKind;
use serde::Deserialize;
use zeroize::Zeroizing;

use super::error::CloudError;
use crate::auth::TokenProvider;
use crate::gog::build_manifest;
use crate::gog::error::AuthError;

/// A secret string: zeroed on drop, never printed. [`Secret::expose`] is the only
/// way to the value, for the one request that needs it.
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Where the credentials came from (design 22 §2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSource {
    /// The game dir's `build-manifest.json`.
    KeptManifest,
    /// A manifest fetched for this command, of build `build_id`; not kept.
    FetchedManifest { build_id: String },
}

/// A game's Galaxy client: its id (public) and secret.
#[derive(Debug)]
pub struct ClientCredentials {
    pub client_id: String,
    pub secret: Secret,
    pub source: CredentialSource,
}

/// The per-game token: in memory for one command, never stored (D4).
pub struct CloudToken {
    access: Secret,
    user_id: Secret,
    /// Seconds, as GOG gave them (3600 live).
    pub expires_in: u32,
}

impl CloudToken {
    pub fn new(access: impl Into<String>, user_id: impl Into<String>, expires_in: u32) -> Self {
        Self {
            access: Secret::new(access),
            user_id: Secret::new(user_id),
            expires_in,
        }
    }

    /// The bearer for the storage host.
    pub fn access_token(&self) -> &str {
        self.access.expose()
    }

    /// The storage path's `<userId>` (the token response's `user_id`). Personal:
    /// only for building storage URLs, never for output.
    pub fn user_id(&self) -> &str {
        self.user_id.expose()
    }
}

impl std::fmt::Debug for CloudToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudToken")
            .field("access", &self.access)
            .field("user_id", &self.user_id)
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

// ---------------------------------------------------------------- credentials

/// The credentials of a v2 manifest, when it has both fields. v1 manifests have none.
pub fn from_manifest(manifest: &Manifest, source: CredentialSource) -> Option<ClientCredentials> {
    let Manifest::V2(m) = manifest else {
        return None;
    };
    let id = m.client_id().as_deref().filter(|s| !s.is_empty())?;
    let secret = m.client_secret().as_deref().filter(|s| !s.is_empty())?;
    Some(ClientCredentials {
        client_id: id.to_string(),
        secret: Secret::new(secret),
        source,
    })
}

/// The credentials in game dir `game_dir`'s kept `build-manifest.json` (design 21
/// §5.2), read in place. `None` when there is no file or it lacks them.
pub fn from_kept_manifest(game_dir: &Path) -> Option<ClientCredentials> {
    let stored = build_manifest::load(game_dir)?;
    from_manifest(&stored.manifest, CredentialSource::KeptManifest)
}

/// The `clientId` of `game_dir`'s `goggame-<gogid>.info`: `Ok(None)` when there is
/// no such file or it has no `clientId`.
pub fn info_client_id(game_dir: &Path, gogid: u64) -> Result<Option<String>, CloudError> {
    #[derive(Deserialize)]
    struct Info {
        #[serde(rename = "clientId")]
        client_id: Option<String>,
    }
    let name = format!("goggame-{gogid}.info");
    let raw = match std::fs::read(game_dir.join(&name)) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(CloudError::Io(format!("{name}: {e}"))),
    };
    let info: Info = serde_json::from_slice(&raw)
        .map_err(|e| CloudError::Io(format!("{name} is not readable: {e}")))?;
    Ok(info.client_id.filter(|s| !s.is_empty()))
}

/// Pure: the build whose manifest gives the credentials, among `items` (one
/// platform's listing): the installed build when it is listed and generation 2,
/// else the newest public generation-2 build on the default branch.
pub fn pick_credentials_build<'a>(
    items: &'a [Build],
    installed: Option<&str>,
) -> Option<&'a Build> {
    let v2 = || items.iter().filter(|b| *b.generation() == 2);
    installed
        .and_then(|id| v2().find(|b| b.build_id() == id))
        .or_else(|| crate::gog::builds::pick_build(v2()))
}

/// Pure: a fetched manifest's credentials, checked against the info file's
/// `clientId` when there is one (design 22 §2.2: a mismatch is an error).
pub fn checked_fetched(
    gogid: u64,
    manifest: &Manifest,
    build_id: &str,
    info_id: Option<&str>,
) -> Result<ClientCredentials, CloudError> {
    let creds = from_manifest(
        manifest,
        CredentialSource::FetchedManifest {
            build_id: build_id.to_string(),
        },
    )
    .ok_or_else(|| CloudError::NoCredentials {
        gogid,
        reason: format!("the manifest of build {build_id} has no client id and secret"),
    })?;
    match info_id {
        Some(id) if id != creds.client_id => {
            tracing::debug!(
                manifest = %creds.client_id,
                info = %id,
                "cloud saves: client id mismatch"
            );
            Err(CloudError::ClientIdMismatch { gogid })
        }
        _ => Ok(creds),
    }
}

/// The game's client credentials, in design 22 §2.2's order: the kept
/// `build-manifest.json` in `game_dir`, else the manifest of the installed build
/// `installed_build` (else the newest) fetched for this command and checked
/// against `goggame-<gogid>.info`. `game_dir` is the dir holding the game's files
/// and its info file (the caller resolves it per lane).
pub async fn credentials(
    core: &Core,
    gogid: u64,
    game_dir: &Path,
    installed_build: Option<&str>,
) -> Result<ClientCredentials, CloudError> {
    if let Some(c) = from_kept_manifest(game_dir) {
        tracing::debug!(client_id = %c.client_id, "cloud saves: credentials from the kept manifest");
        return Ok(c);
    }
    let info_id = info_client_id(game_dir, gogid)?;
    // Straight from gog-warp, not `builds::fetch_listing`: nothing of this
    // fetch is kept (§2.2), not even the listing cache.
    let listing = core
        .get_builds(&gogid.to_string(), Platform::Windows, None)
        .await
        .map_err(|e| warp_error(e, gogid))?;
    let build = pick_credentials_build(listing.items(), installed_build).ok_or_else(|| {
        CloudError::NoCredentials {
            gogid,
            reason: "GOG lists no generation-2 Windows build to read them from".into(),
        }
    })?;
    let manifest = core
        .get_manifest(build)
        .await
        .map_err(|e| warp_error(e, gogid))?;
    let c = checked_fetched(gogid, &manifest, build.build_id(), info_id.as_deref())?;
    tracing::debug!(
        client_id = %c.client_id,
        build = %build.build_id(),
        "cloud saves: credentials from a fetched manifest"
    );
    Ok(c)
}

// ---------------------------------------------------------------- the token

/// The exchange seam: the live one is [`CoreExchange`]; tests use fakes, because
/// gog-warp's token endpoint can't be pointed at a mock server.
#[async_trait::async_trait]
pub trait TokenExchange: Send + Sync {
    async fn exchange(
        &self,
        gogid: u64,
        creds: &ClientCredentials,
    ) -> Result<CloudToken, CloudError>;
}

/// The live exchange: a separate `Core` seeded with the Galaxy token only, dropped
/// with this value, so the per-game token (and its refresh token) exists only here.
pub struct CoreExchange {
    core: Core,
}

impl CoreExchange {
    /// A `Core` whose token map holds `galaxy` and nothing else.
    pub fn seeded(galaxy: Token) -> Result<Self, CloudError> {
        let map = HashMap::from([(GALAXY_CLIENT_ID.to_string(), galaxy)]);
        let seed_failed = |_| CloudError::Auth("could not seed the separate token map".into());
        let blob =
            Zeroizing::new(serde_json::to_string(&map).map_err(|e| seed_failed(e.to_string()))?);
        let core = Core::new();
        core.deserialize_tokens(&blob)
            .map_err(|e| seed_failed(e.to_string()))?;
        Ok(Self { core })
    }

    /// From the main session: its Galaxy token is refreshed there first (and
    /// persisted by the caller as usual, design 22 §2.1 step 4), then copied.
    pub async fn from_session(session: &dyn TokenProvider) -> Result<Self, CloudError> {
        session.bearer().await.map_err(auth_error)?;
        let galaxy = session
            .gog_warp_core()
            .and_then(|c| c.get_token_raw(GALAXY_CLIENT_ID))
            .ok_or(CloudError::NotLoggedIn)?;
        Self::seeded(galaxy)
    }

    /// The client ids in the separate map (for tests: only Galaxy's before an
    /// exchange).
    pub fn client_ids(&self) -> Vec<String> {
        let blob = Zeroizing::new(self.core.serialize_tokens().unwrap_or_default());
        serde_json::from_str::<HashMap<String, serde_json::Value>>(&blob)
            .map(|m| m.into_keys().collect())
            .unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl TokenExchange for CoreExchange {
    async fn exchange(
        &self,
        gogid: u64,
        creds: &ClientCredentials,
    ) -> Result<CloudToken, CloudError> {
        tracing::debug!(client_id = %creds.client_id, "cloud saves: exchanging for a per-game token");
        let t = self
            .core
            .obtain_token(&creds.client_id, creds.secret.expose())
            .await
            .map_err(|e| warp_error(e, gogid))?;
        Ok(CloudToken::new(
            t.access_token().as_str(),
            t.user_id().as_str(),
            *t.expires_in(),
        ))
    }
}

/// The per-game token for `gogid`: one exchange in a separate `Core` (D4).
pub async fn per_game_token(
    session: &dyn TokenProvider,
    gogid: u64,
    creds: &ClientCredentials,
) -> Result<CloudToken, CloudError> {
    CoreExchange::from_session(session)
        .await?
        .exchange(gogid, creds)
        .await
}

fn auth_error(e: AuthError) -> CloudError {
    match e {
        AuthError::Unauthorized => CloudError::NotLoggedIn,
        // `AuthError::Upstream` holds gog-warp's kind text only, never a URL;
        // its `InvalidSession` text is the one that asks for a re-login.
        AuthError::Upstream(m) if m.contains("re-login") => CloudError::InvalidSession,
        // Its text would hold the request URL.
        AuthError::Http(e) => CloudError::Network(reqwest_kind(&e).into()),
        other => CloudError::Auth(other.to_string()),
    }
}

/// gog-warp's error, by kind and status only (its source's text holds the URL).
pub fn warp_error(e: gog_warp::Error, gogid: u64) -> CloudError {
    match e.kind() {
        ErrorKind::NotLoggedIn => CloudError::NotLoggedIn,
        ErrorKind::InvalidSession => CloudError::InvalidSession,
        ErrorKind::Unauthorized => CloudError::Unauthorized { gogid },
        ErrorKind::MaximumRetries => CloudError::Network("retries exhausted".into()),
        ErrorKind::Request => {
            let req = e
                .source
                .as_deref()
                .and_then(|s| s.downcast_ref::<reqwest::Error>());
            match req {
                // The only 403 the exchange gives is `no_license` (design 22 §1.1);
                // gog-warp doesn't read the body to say so.
                Some(r) if r.status().map(|s| s.as_u16()) == Some(403) => {
                    CloudError::NoLicense { gogid }
                }
                Some(r) if r.status().is_some() => CloudError::ExchangeStatus {
                    gogid,
                    status: r.status().map(|s| s.as_u16()).unwrap_or_default(),
                },
                Some(r) => CloudError::Network(reqwest_kind(r).into()),
                None => CloudError::Exchange {
                    gogid,
                    kind: e.to_string(),
                },
            }
        }
        _ => CloudError::Exchange {
            gogid,
            kind: e.to_string(),
        },
    }
}

/// What kind of failure a status-less `reqwest::Error` is, without its URL.
pub(crate) fn reqwest_kind(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "timed out"
    } else if e.is_connect() {
        "could not connect"
    } else if e.is_decode() || e.is_body() {
        "the answer could not be read"
    } else {
        "request failed"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "made-up-secret-0123456789abcdef";

    fn v2_manifest(client: Option<(&str, &str)>) -> Manifest {
        let mut m = serde_json::json!({
            "baseProductId": "100",
            "depots": [],
            "installDirectory": "Game",
            "platform": "windows",
            "products": []
        });
        if let Some((id, secret)) = client {
            m["clientId"] = id.into();
            m["clientSecret"] = secret.into();
        }
        serde_json::from_value(m).unwrap()
    }

    fn build(id: &str, generation: u32, date: &str) -> Build {
        serde_json::from_value(serde_json::json!({
            "build_id": id,
            "product_id": "100",
            "os": "windows",
            "branch": null,
            "version_name": "1.0",
            "tags": [],
            "public": true,
            "date_published": date,
            "generation": generation,
            "urls": []
        }))
        .unwrap()
    }

    fn galaxy_token() -> Token {
        serde_json::from_value(serde_json::json!({
            "access_token": "made-up-access",
            "refresh_token": "made-up-refresh",
            "user_id": "1",
            "expires_in": 3600,
            "token_type": "bearer",
            "session_id": "s",
            "login_time": chrono::Utc::now().timestamp()
        }))
        .unwrap()
    }

    #[test]
    fn secrets_never_debug_print() {
        let c = ClientCredentials {
            client_id: "11111111111111111".into(),
            secret: Secret::new(SECRET),
            source: CredentialSource::KeptManifest,
        };
        let dbg = format!("{c:?}");
        assert!(!dbg.contains(SECRET), "{dbg}");
        assert!(dbg.contains("<redacted>"));
        let t = CloudToken::new("made-up-access", "4242", 3600);
        let dbg = format!("{t:?}");
        assert!(
            !dbg.contains("made-up-access") && !dbg.contains("4242"),
            "{dbg}"
        );
    }

    #[test]
    fn v2_manifest_gives_credentials_v1_and_partial_none() {
        let m = v2_manifest(Some(("11111111111111111", SECRET)));
        let c = from_manifest(&m, CredentialSource::KeptManifest).unwrap();
        assert_eq!(c.client_id, "11111111111111111");
        assert_eq!(c.secret.expose(), SECRET);
        assert!(from_manifest(&v2_manifest(None), CredentialSource::KeptManifest).is_none());
        assert!(
            from_manifest(
                &v2_manifest(Some(("", SECRET))),
                CredentialSource::KeptManifest
            )
            .is_none()
        );
    }

    #[test]
    fn kept_manifest_is_read_in_place() {
        let dir = tempfile::tempdir().unwrap();
        assert!(from_kept_manifest(dir.path()).is_none());
        let m = v2_manifest(Some(("11111111111111111", SECRET)));
        build_manifest::write(dir.path(), "5555", 2, &m).unwrap();
        let c = from_kept_manifest(dir.path()).unwrap();
        assert_eq!(c.client_id, "11111111111111111");
        assert_eq!(c.source, CredentialSource::KeptManifest);
    }

    #[test]
    fn info_client_id_reads_the_games_own_info() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(info_client_id(dir.path(), 100).unwrap(), None);
        std::fs::write(
            dir.path().join("goggame-100.info"),
            r#"{"gameId":"100","clientId":"22222222222222222","name":"G"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("goggame-200.info"),
            r#"{"gameId":"200","clientId":"33333333333333333"}"#,
        )
        .unwrap();
        assert_eq!(
            info_client_id(dir.path(), 100).unwrap().as_deref(),
            Some("22222222222222222")
        );
        std::fs::write(dir.path().join("goggame-300.info"), "{").unwrap();
        assert!(matches!(
            info_client_id(dir.path(), 300),
            Err(CloudError::Io(_))
        ));
    }

    #[test]
    fn credentials_build_prefers_the_installed_v2_build() {
        let items = vec![
            build("3", 2, "2024-03-01T00:00:00Z"),
            build("2", 2, "2024-02-01T00:00:00Z"),
            build("1", 1, "2024-01-01T00:00:00Z"),
        ];
        let pick = |i| pick_credentials_build(&items, i).map(|b| b.build_id().clone());
        assert_eq!(pick(Some("2")).as_deref(), Some("2"));
        // A v1 installed build has no credentials: the newest v2 one has them.
        assert_eq!(pick(Some("1")).as_deref(), Some("3"));
        assert_eq!(pick(Some("gone")).as_deref(), Some("3"));
        assert_eq!(pick(None).as_deref(), Some("3"));
        let v1_only = vec![build("1", 1, "2024-01-01T00:00:00Z")];
        assert!(pick_credentials_build(&v1_only, None).is_none());
    }

    #[test]
    fn fetched_manifest_is_checked_against_the_info() {
        let m = v2_manifest(Some(("11111111111111111", SECRET)));
        let ok = checked_fetched(100, &m, "5", Some("11111111111111111")).unwrap();
        assert_eq!(
            ok.source,
            CredentialSource::FetchedManifest {
                build_id: "5".into()
            }
        );
        assert!(checked_fetched(100, &m, "5", None).is_ok());
        let e = checked_fetched(100, &m, "5", Some("99999999999999999")).unwrap_err();
        assert!(matches!(e, CloudError::ClientIdMismatch { gogid: 100 }));
        assert!(!e.to_string().contains("99999999999999999"));
        assert!(matches!(
            checked_fetched(100, &v2_manifest(None), "5", None),
            Err(CloudError::NoCredentials { .. })
        ));
    }

    #[test]
    fn separate_core_holds_only_the_galaxy_token() {
        let x = CoreExchange::seeded(galaxy_token()).unwrap();
        assert_eq!(x.client_ids(), vec![GALAXY_CLIENT_ID.to_string()]);
    }

    /// The main session's map, and so its token store blob, is the same before
    /// and after a separate `Core` is seeded from it.
    #[tokio::test]
    async fn seeding_leaves_the_main_session_alone() {
        struct Main(Core);
        #[async_trait::async_trait]
        impl TokenProvider for Main {
            async fn bearer(&self) -> Result<String, AuthError> {
                Ok(self.0.obtain_galaxy_token().await?.access_token().clone())
            }
            fn user_id(&self) -> Option<u64> {
                None
            }
            fn gog_warp_core(&self) -> Option<&Core> {
                Some(&self.0)
            }
        }
        let main = Core::new();
        let map = HashMap::from([(GALAXY_CLIENT_ID.to_string(), galaxy_token())]);
        main.deserialize_tokens(&serde_json::to_string(&map).unwrap())
            .unwrap();
        let before = main.serialize_tokens().unwrap();
        let main = Main(main);
        // A fresh token: `bearer()` needs no network.
        let x = CoreExchange::from_session(&main).await.unwrap();
        assert_eq!(x.client_ids(), vec![GALAXY_CLIENT_ID.to_string()]);
        assert_eq!(main.0.serialize_tokens().unwrap(), before);
    }

    #[tokio::test]
    async fn no_session_is_not_logged_in() {
        struct Static;
        #[async_trait::async_trait]
        impl TokenProvider for Static {
            async fn bearer(&self) -> Result<String, AuthError> {
                Ok("x".into())
            }
            fn user_id(&self) -> Option<u64> {
                None
            }
        }
        assert!(matches!(
            CoreExchange::from_session(&Static).await,
            Err(CloudError::NotLoggedIn)
        ));
    }

    /// A real `reqwest::Error` for `status`, from a mock server, with a secret in
    /// its URL as gog-warp's exchange has.
    async fn status_error(status: u16) -> reqwest::Error {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(
                ResponseTemplate::new(status).set_body_string(r#"{"error":"no_license"}"#),
            )
            .mount(&server)
            .await;
        reqwest::get(format!("{}/token?client_secret={SECRET}", server.uri()))
            .await
            .unwrap()
            .error_for_status()
            .unwrap_err()
    }

    fn request_error(e: reqwest::Error) -> gog_warp::Error {
        gog_warp::Error::new(ErrorKind::Request, Some(e))
    }

    #[tokio::test]
    async fn exchange_errors_map_without_the_url() {
        let e = warp_error(request_error(status_error(403).await), 1495134320);
        assert!(matches!(e, CloudError::NoLicense { gogid: 1495134320 }));
        assert_eq!(e.exit_code(), 3);
        assert!(e.to_string().contains("1495134320"));

        let e = warp_error(request_error(status_error(502).await), 1);
        assert!(matches!(e, CloudError::ExchangeStatus { status: 502, .. }));
        assert!(!format!("{e} {e:?}").contains(SECRET));

        let kind = |k| warp_error(gog_warp::Error::new(k, None::<String>), 1);
        assert!(matches!(
            kind(ErrorKind::InvalidSession),
            CloudError::InvalidSession
        ));
        assert_eq!(kind(ErrorKind::InvalidSession).exit_code(), 3);
        assert!(matches!(
            kind(ErrorKind::NotLoggedIn),
            CloudError::NotLoggedIn
        ));
        assert!(matches!(
            kind(ErrorKind::Unauthorized),
            CloudError::Unauthorized { gogid: 1 }
        ));
        assert!(matches!(
            kind(ErrorKind::MaximumRetries),
            CloudError::Network(_)
        ));
    }

    #[tokio::test]
    async fn connect_failure_is_network_without_the_url() {
        // A port nothing listens on.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let err = reqwest::get(format!(
            "http://127.0.0.1:{port}/token?client_secret={SECRET}"
        ))
        .await
        .unwrap_err();
        let e = warp_error(request_error(err), 1);
        assert!(matches!(e, CloudError::Network(_)));
        assert_eq!(e.exit_code(), 4);
        assert!(!format!("{e} {e:?}").contains(SECRET));
    }

    struct FakeExchange(fn(u64) -> Result<CloudToken, CloudError>);

    #[async_trait::async_trait]
    impl TokenExchange for FakeExchange {
        async fn exchange(
            &self,
            gogid: u64,
            _: &ClientCredentials,
        ) -> Result<CloudToken, CloudError> {
            (self.0)(gogid)
        }
    }

    #[tokio::test]
    async fn the_seam_takes_fakes() {
        let c = ClientCredentials {
            client_id: "11111111111111111".into(),
            secret: Secret::new(SECRET),
            source: CredentialSource::KeptManifest,
        };
        let ok = FakeExchange(|_| Ok(CloudToken::new("a", "7", 3600)));
        let t = ok.exchange(1, &c).await.unwrap();
        assert_eq!((t.access_token(), t.user_id()), ("a", "7"));
        let refused = FakeExchange(|gogid| Err(CloudError::NoLicense { gogid }));
        assert_eq!(refused.exchange(9, &c).await.unwrap_err().exit_code(), 3);
    }
}
