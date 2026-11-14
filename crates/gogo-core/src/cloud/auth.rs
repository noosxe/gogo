//! Cloud-save auth (design 22 §2): the game's client credentials and the per-game
//! token they buy.
//!
//! The storage API refuses the Galaxy token (403); it wants a token for the game's
//! own client, exchanged with the game's `clientId`/`clientSecret` and the user's
//! Galaxy refresh token. gog-warp does the exchange (`Core::obtain_token`). It runs
//! in a separate, short-lived `Core` holding only the Galaxy token, so the per-game
//! token never reaches the main session's map and so never the token store (D4).
//!
//! [`CloudAuth`] is the entry point. Fetching a manifest and refreshing the Galaxy
//! token can rotate the main session's token; `CloudAuth` persists it after every
//! call that touched the main session, as every gogo command does
//! (`Session::persist_if_changed`), so a caller can't forget it.
//!
//! SECURITY: the client secret and the per-game token live in [`Secret`]s, whose
//! `Debug` prints a placeholder and which have no `Display`. gog-warp's errors wrap
//! a `reqwest::Error` whose text holds the request URL (for the exchange: client
//! secret and refresh token in the query): they are mapped by kind and status only.

use std::collections::HashMap;
use std::path::Path;

use gog_warp::Core;
use gog_warp::auth::types::Token;
use gog_warp::constants::GALAXY_CLIENT_ID;
use gog_warp::content_system::types::{Build, Manifest, Platform};
use gog_warp::errors::ErrorKind;
use zeroize::Zeroizing;

use super::client_id::{self, InfoId};
use super::error::CloudError;
use super::locations::{Game, Layout};
use crate::auth::store::TokenStore;
use crate::auth::{Session, TokenProvider};
use crate::gog::build_manifest;
use crate::gog::error::AuthError;

/// A secret string that is never printed. [`Secret::expose`] is the only way to the
/// value, for the one request that needs it. This copy is zeroed on drop; the
/// copies gog-warp and serde make on the way in are plain `String`s and aren't.
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

/// The `clientId` of the game's `goggame-<gogid>.info` (the one reader,
/// [`super::client_id`]): `Ok(None)` when there is no such file or it has no
/// `clientId`. A file that is there but can't be used is [`CloudError::Info`]:
/// it is the check a fetched manifest is held to, so it is never skipped.
pub fn info_client_id(layout: &Layout, gogid: u64) -> Result<Option<String>, CloudError> {
    match client_id::info_id(layout, gogid) {
        InfoId::Absent => Ok(None),
        InfoId::Id(id) => Ok(Some(id)),
        InfoId::Bad { path, reason } => Err(CloudError::Info {
            path: path.display().to_string(),
            reason,
        }),
    }
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

// ---------------------------------------------------------------- seams

/// Where fetched manifests come from: [`WarpManifests`] live, fakes in tests
/// (gog-warp's content-system URLs are fixed). Windows builds only for now: both
/// lanes run them (design 22 §11).
#[async_trait::async_trait]
pub trait ManifestSource: Send + Sync {
    /// The game's Windows builds.
    async fn builds(&self, gogid: u64) -> Result<Vec<Build>, CloudError>;
    async fn manifest(&self, gogid: u64, build: &Build) -> Result<Manifest, CloudError>;
}

/// The content system through the main session's `Core`. Not
/// `builds::fetch_listing`: nothing of this fetch is kept (§2.2), not even the
/// builds-listing cache.
pub struct WarpManifests<'a>(pub &'a Core);

#[async_trait::async_trait]
impl ManifestSource for WarpManifests<'_> {
    async fn builds(&self, gogid: u64) -> Result<Vec<Build>, CloudError> {
        let r = self
            .0
            .get_builds(&gogid.to_string(), Platform::Windows, None)
            .await
            .map_err(|e| content_error(e, gogid))?;
        Ok(r.items().to_vec())
    }

    async fn manifest(&self, gogid: u64, build: &Build) -> Result<Manifest, CloudError> {
        self.0
            .get_manifest(build)
            .await
            .map_err(|e| content_error(e, gogid))
    }
}

/// The exchange: [`WarpExchange`] live, fakes in tests (gog-warp's token endpoint
/// is a fixed URL). `galaxy` is the main session's freshly refreshed Galaxy token.
#[async_trait::async_trait]
pub trait TokenExchange: Send + Sync {
    async fn exchange(
        &self,
        galaxy: Token,
        gogid: u64,
        creds: &ClientCredentials,
    ) -> Result<CloudToken, CloudError>;
}

/// The live exchange, in a [`SeparateCore`] made for it and dropped after it.
pub struct WarpExchange;

#[async_trait::async_trait]
impl TokenExchange for WarpExchange {
    async fn exchange(
        &self,
        galaxy: Token,
        gogid: u64,
        creds: &ClientCredentials,
    ) -> Result<CloudToken, CloudError> {
        SeparateCore::seeded(galaxy)?.obtain(gogid, creds).await
    }
}

/// A `Core` seeded with the Galaxy token only, so the per-game token (and its own
/// refresh token) exists only here and goes with it.
pub struct SeparateCore {
    core: Core,
}

impl SeparateCore {
    pub fn seeded(galaxy: Token) -> Result<Self, CloudError> {
        let seed_failed = |_| CloudError::Auth("could not seed the separate token map".into());
        let map = HashMap::from([(GALAXY_CLIENT_ID.to_string(), galaxy)]);
        let blob =
            Zeroizing::new(serde_json::to_string(&map).map_err(|e| seed_failed(e.to_string()))?);
        let core = Core::new();
        core.deserialize_tokens(&blob)
            .map_err(|e| seed_failed(e.to_string()))?;
        Ok(Self { core })
    }

    /// The client ids in the separate map (tests: only Galaxy's before an exchange).
    pub fn client_ids(&self) -> Vec<String> {
        let blob = Zeroizing::new(self.core.serialize_tokens().unwrap_or_default());
        serde_json::from_str::<HashMap<String, serde_json::Value>>(&blob)
            .map(|m| m.into_keys().collect())
            .unwrap_or_default()
    }

    pub async fn obtain(
        &self,
        gogid: u64,
        creds: &ClientCredentials,
    ) -> Result<CloudToken, CloudError> {
        tracing::debug!(client_id = %creds.client_id, "cloud saves: exchanging for a per-game token");
        let t = self
            .core
            .obtain_token(&creds.client_id, creds.secret.expose())
            .await
            .map_err(|e| exchange_error(e, gogid))?;
        Ok(CloudToken::new(
            t.access_token().as_str(),
            t.user_id().as_str(),
            *t.expires_in(),
        ))
    }
}

// ---------------------------------------------------------------- entry point

type Persist<'a> = Box<dyn Fn() -> Result<(), AuthError> + Send + Sync + 'a>;

/// Credentials and per-game tokens for one command. Every call that may have
/// refreshed the main session's Galaxy token persists it before returning.
pub struct CloudAuth<'a> {
    session: &'a dyn TokenProvider,
    persist: Persist<'a>,
    manifests: Box<dyn ManifestSource + 'a>,
    exchange: Box<dyn TokenExchange + 'a>,
}

impl<'a> CloudAuth<'a> {
    /// The live one: `session` restored from `store`, which it is persisted back to.
    pub fn new(session: &'a Session, store: &'a dyn TokenStore) -> Self {
        let core = session
            .gog_warp_core()
            .expect("a live session has a gog-warp core");
        Self {
            session,
            persist: Box::new(move || session.persist_if_changed(store)),
            manifests: Box::new(WarpManifests(core)),
            exchange: Box::new(WarpExchange),
        }
    }

    /// With test doubles (here and in other crates' tests).
    #[cfg(any(test, feature = "test-util"))]
    pub fn with_parts(
        session: &'a dyn TokenProvider,
        persist: impl Fn() -> Result<(), AuthError> + Send + Sync + 'a,
        manifests: impl ManifestSource + 'a,
        exchange: impl TokenExchange + 'a,
    ) -> Self {
        Self {
            session,
            persist: Box::new(persist),
            manifests: Box::new(manifests),
            exchange: Box::new(exchange),
        }
    }

    fn persist(&self) -> Result<(), CloudError> {
        (self.persist)().map_err(|e| CloudError::Auth(e.to_string()))
    }

    /// The game's client credentials, in design 22 §2.2's order: the kept
    /// `build-manifest.json` in the game's dir, else the manifest of the
    /// installed build `installed_build` (else the newest, generation 2 only)
    /// fetched for this command and checked against `goggame-<gogid>.info`
    /// (found in the game's files, or under `drive_c` for the installer lane).
    pub async fn credentials(
        &self,
        game: &Game,
        installed_build: Option<&str>,
    ) -> Result<ClientCredentials, CloudError> {
        let gogid = game.id()?;
        if let Some(c) = from_kept_manifest(&game.root) {
            tracing::debug!(client_id = %c.client_id, "cloud saves: credentials from the kept manifest");
            return Ok(c);
        }
        let info_id = info_client_id(&game.layout, gogid)?;
        let fetched = self.fetch(gogid, installed_build, info_id.as_deref()).await;
        // The fetch may have refreshed the Galaxy token, failed or not.
        self.persist()?;
        fetched
    }

    async fn fetch(
        &self,
        gogid: u64,
        installed_build: Option<&str>,
        info_id: Option<&str>,
    ) -> Result<ClientCredentials, CloudError> {
        let builds = self.manifests.builds(gogid).await?;
        let build = pick_credentials_build(&builds, installed_build).ok_or_else(|| {
            CloudError::NoCredentials {
                gogid,
                reason: "GOG lists no generation-2 Windows build to read them from".into(),
            }
        })?;
        let manifest = self.manifests.manifest(gogid, build).await?;
        let c = checked_fetched(gogid, &manifest, build.build_id(), info_id)?;
        tracing::debug!(
            client_id = %c.client_id,
            build = %build.build_id(),
            "cloud saves: credentials from a fetched manifest"
        );
        Ok(c)
    }

    /// The per-game token for `gogid` (D4): the Galaxy token is refreshed in the
    /// main session and persisted (design 22 §2.1 step 4), then copied into a
    /// separate `Core` for the exchange.
    pub async fn token(
        &self,
        gogid: u64,
        creds: &ClientCredentials,
    ) -> Result<CloudToken, CloudError> {
        let refreshed = self.session.bearer().await.map_err(auth_error);
        self.persist()?;
        refreshed?;
        let galaxy = self
            .session
            .gog_warp_core()
            .and_then(|c| c.get_token_raw(GALAXY_CLIENT_ID))
            .ok_or(CloudError::NotLoggedIn)?;
        self.exchange.exchange(galaxy, gogid, creds).await
    }
}

// ---------------------------------------------------------------- error maps

/// The main session's refresh (`bearer()`).
fn auth_error(e: AuthError) -> CloudError {
    match e {
        AuthError::Unauthorized => CloudError::NotLoggedIn,
        // `AuthError::Upstream` holds gog-warp's kind text only, never a URL; its
        // `InvalidSession` text is the one asking for a re-login (pinned by a test).
        AuthError::Upstream(m) if m.contains("re-login") => CloudError::InvalidSession,
        // Its text would hold the request URL.
        AuthError::Http(e) => CloudError::Network(reqwest_kind(&e).into()),
        other => CloudError::Auth(other.to_string()),
    }
}

/// The HTTP status of a gog-warp request error, without touching its text.
fn warp_status(e: &gog_warp::Error) -> Option<Result<u16, &'static str>> {
    let r = e
        .source
        .as_deref()
        .and_then(|s| s.downcast_ref::<reqwest::Error>())?;
    Some(
        r.status()
            .map(|s| s.as_u16())
            .ok_or_else(|| reqwest_kind(r)),
    )
}

/// The per-game exchange's errors. Only here is a 403 `no_license`.
pub fn exchange_error(e: gog_warp::Error, gogid: u64) -> CloudError {
    match e.kind() {
        ErrorKind::NotLoggedIn => CloudError::NotLoggedIn,
        // gog-warp's names for 400 and 401; either side may be at fault.
        ErrorKind::InvalidSession => CloudError::ExchangeRejected { gogid, status: 400 },
        ErrorKind::Unauthorized => CloudError::ExchangeRejected { gogid, status: 401 },
        ErrorKind::MaximumRetries => CloudError::Network("retries exhausted".into()),
        ErrorKind::Request => match warp_status(&e) {
            // The only 403 the exchange gives is `no_license` (design 22 §1.1);
            // gog-warp doesn't read the body to say so.
            Some(Ok(403)) => CloudError::NoLicense { gogid },
            Some(Ok(status)) => CloudError::ExchangeStatus { gogid, status },
            Some(Err(kind)) => CloudError::Network(kind.into()),
            None => CloudError::Exchange {
                gogid,
                kind: e.to_string(),
            },
        },
        _ => CloudError::Exchange {
            gogid,
            kind: e.to_string(),
        },
    }
}

/// The builds listing's and manifest fetch's errors.
pub fn content_error(e: gog_warp::Error, gogid: u64) -> CloudError {
    match e.kind() {
        ErrorKind::NotLoggedIn => CloudError::NotLoggedIn,
        // Here gog-warp is refreshing the main session's Galaxy token.
        ErrorKind::InvalidSession => CloudError::InvalidSession,
        ErrorKind::Unauthorized => CloudError::Manifest {
            gogid,
            status: Some(401),
        },
        ErrorKind::MaximumRetries => CloudError::Network("retries exhausted".into()),
        ErrorKind::Request => match warp_status(&e) {
            Some(Ok(status)) => CloudError::Manifest {
                gogid,
                status: Some(status),
            },
            Some(Err(kind)) => CloudError::Network(kind.into()),
            None => CloudError::Manifest {
                gogid,
                status: None,
            },
        },
        _ => CloudError::Manifest {
            gogid,
            status: None,
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
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SECRET: &str = "made-up-secret-0123456789abcdef";
    const CLIENT: &str = "11111111111111111";

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

    fn creds() -> ClientCredentials {
        ClientCredentials {
            client_id: CLIENT.into(),
            secret: Secret::new(SECRET),
            source: CredentialSource::KeptManifest,
        }
    }

    /// A main session backed by a real `Core` with a fresh made-up Galaxy token:
    /// `bearer()` needs no network.
    struct Main(Core);

    impl Main {
        fn new() -> Self {
            let core = Core::new();
            let map = HashMap::from([(GALAXY_CLIENT_ID.to_string(), galaxy_token())]);
            core.deserialize_tokens(&serde_json::to_string(&map).unwrap())
                .unwrap();
            Self(core)
        }
    }

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

    /// A listing and one manifest per build id; records what was fetched.
    #[derive(Default)]
    struct FakeManifests {
        builds: Vec<Build>,
        manifests: HashMap<String, Manifest>,
        fail_builds: Option<u16>,
        fetched: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl ManifestSource for &FakeManifests {
        async fn builds(&self, gogid: u64) -> Result<Vec<Build>, CloudError> {
            self.fetched.lock().unwrap().push("builds".into());
            match self.fail_builds {
                Some(s) => Err(CloudError::Manifest {
                    gogid,
                    status: Some(s),
                }),
                None => Ok(self.builds.clone()),
            }
        }
        async fn manifest(&self, _: u64, b: &Build) -> Result<Manifest, CloudError> {
            self.fetched.lock().unwrap().push(b.build_id().clone());
            Ok(self.manifests[b.build_id()].clone())
        }
    }

    struct FakeExchange(fn(u64) -> Result<CloudToken, CloudError>);

    #[async_trait::async_trait]
    impl TokenExchange for FakeExchange {
        async fn exchange(
            &self,
            galaxy: Token,
            gogid: u64,
            _: &ClientCredentials,
        ) -> Result<CloudToken, CloudError> {
            assert_eq!(galaxy.refresh_token(), "made-up-refresh");
            (self.0)(gogid)
        }
    }

    fn ok_exchange() -> FakeExchange {
        FakeExchange(|_| Ok(CloudToken::new("a", "7", 3600)))
    }

    #[test]
    fn secrets_never_debug_print() {
        let dbg = format!("{:?}", creds());
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
        let m = v2_manifest(Some((CLIENT, SECRET)));
        let c = from_manifest(&m, CredentialSource::KeptManifest).unwrap();
        assert_eq!(c.client_id, CLIENT);
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

    /// A game whose dir, files and prefix are all `dir` (enough for these tests).
    fn game_at(dir: &Path, gogid: u64) -> Game {
        Game {
            gogid: gogid.to_string(),
            layout: Layout {
                lane: super::super::locations::Lane::Linux,
                prefix: dir.join("prefix"),
                game_files: dir.to_path_buf(),
            },
            root: dir.to_path_buf(),
        }
    }

    #[test]
    fn info_client_id_reads_the_games_own_info() {
        let dir = tempfile::tempdir().unwrap();
        let layout = game_at(dir.path(), 100).layout;
        assert_eq!(info_client_id(&layout, 100).unwrap(), None);
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
            info_client_id(&layout, 100).unwrap().as_deref(),
            Some("22222222222222222")
        );
        // The installer lane: under the prefix's drive_c.
        let other = tempfile::tempdir().unwrap();
        let g = game_at(other.path(), 500);
        let inst = g.layout.prefix.join("drive_c/GOG Games/G");
        std::fs::create_dir_all(&inst).unwrap();
        std::fs::write(inst.join("goggame-500.info"), r#"{"clientId":"4444"}"#).unwrap();
        assert_eq!(
            info_client_id(&g.layout, 500).unwrap().as_deref(),
            Some("4444")
        );
    }

    #[test]
    fn a_bad_info_file_refuses_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let layout = game_at(dir.path(), 300).layout;
        std::fs::write(dir.path().join("goggame-300.info"), "{").unwrap();
        let e = info_client_id(&layout, 300).unwrap_err();
        assert!(matches!(e, CloudError::Info { .. }));
        let text = e.to_string();
        assert!(text.contains("goggame-300.info") && text.contains("isn't a valid info file"));
        // An id that could escape a URL or a path is refused too, not skipped.
        std::fs::write(dir.path().join("goggame-300.info"), r#"{"clientId":"../x"}"#).unwrap();
        assert!(matches!(
            info_client_id(&layout, 300),
            Err(CloudError::Info { .. })
        ));
        // Unreadable: a dir where the file should be.
        std::fs::create_dir(dir.path().join("goggame-400.info")).unwrap();
        let e = info_client_id(&layout, 400).unwrap_err();
        assert!(
            e.to_string().contains("goggame-400.info") && e.to_string().contains("can't be read")
        );
        assert_eq!(e.exit_code(), 1);
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
        let m = v2_manifest(Some((CLIENT, SECRET)));
        assert!(checked_fetched(100, &m, "5", Some(CLIENT)).is_ok());
        assert!(checked_fetched(100, &m, "5", None).is_ok());
        let e = checked_fetched(100, &m, "5", Some("99999999999999999")).unwrap_err();
        assert!(matches!(e, CloudError::ClientIdMismatch { gogid: 100 }));
        assert!(!e.to_string().contains("99999999999999999"));
        assert!(matches!(
            checked_fetched(100, &v2_manifest(None), "5", None),
            Err(CloudError::NoCredentials { .. })
        ));
    }

    fn fetching() -> FakeManifests {
        FakeManifests {
            builds: vec![
                build("3", 2, "2024-03-01T00:00:00Z"),
                build("2", 2, "2024-02-01T00:00:00Z"),
                build("1", 1, "2024-01-01T00:00:00Z"),
            ],
            manifests: HashMap::from([
                ("3".into(), v2_manifest(Some((CLIENT, "newest-secret")))),
                ("2".into(), v2_manifest(Some((CLIENT, "installed-secret")))),
            ]),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn credentials_end_to_end() {
        let main = Main::new();
        let persisted = AtomicUsize::new(0);
        let persist = || {
            persisted.fetch_add(1, Ordering::SeqCst);
            Ok(())
        };

        // The kept manifest first: nothing fetched, nothing to persist.
        let kept = tempfile::tempdir().unwrap();
        build_manifest::write(kept.path(), "2", 2, &v2_manifest(Some((CLIENT, SECRET)))).unwrap();
        let m = fetching();
        let auth = CloudAuth::with_parts(&main, persist, &m, ok_exchange());
        let c = auth.credentials(&game_at(kept.path(), 100), Some("2")).await.unwrap();
        assert_eq!(
            (c.source, c.secret.expose()),
            (CredentialSource::KeptManifest, SECRET)
        );
        assert!(m.fetched.lock().unwrap().is_empty());
        assert_eq!(persisted.load(Ordering::SeqCst), 0);

        // No kept manifest: the installed v2 build's.
        let empty = tempfile::tempdir().unwrap();
        let c = auth
            .credentials(&game_at(empty.path(), 100), Some("2"))
            .await
            .unwrap();
        assert_eq!(c.secret.expose(), "installed-secret");
        assert_eq!(
            c.source,
            CredentialSource::FetchedManifest {
                build_id: "2".into()
            }
        );
        assert_eq!(persisted.load(Ordering::SeqCst), 1);

        // An installed v1 build (or none listed): the newest v2 build's; v1 skipped.
        for installed in [Some("1"), Some("gone"), None] {
            let c = auth
                .credentials(&game_at(empty.path(), 100), installed)
                .await
                .unwrap();
            assert_eq!(c.secret.expose(), "newest-secret");
        }
        assert!(!m.fetched.lock().unwrap().contains(&"1".to_string()));

        // The info file's clientId disagrees: refused.
        std::fs::write(
            empty.path().join("goggame-100.info"),
            r#"{"clientId":"99999999999999999"}"#,
        )
        .unwrap();
        let e = auth
            .credentials(&game_at(empty.path(), 100), Some("2"))
            .await
            .unwrap_err();
        assert!(matches!(e, CloudError::ClientIdMismatch { gogid: 100 }));
        assert_eq!(persisted.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn a_failed_fetch_still_persists() {
        let main = Main::new();
        let persisted = AtomicUsize::new(0);
        let m = FakeManifests {
            fail_builds: Some(503),
            ..Default::default()
        };
        let auth = CloudAuth::with_parts(
            &main,
            || {
                persisted.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            &m,
            ok_exchange(),
        );
        let dir = tempfile::tempdir().unwrap();
        let e = auth.credentials(&game_at(dir.path(), 100), None).await.unwrap_err();
        assert!(matches!(
            e,
            CloudError::Manifest {
                status: Some(503),
                ..
            }
        ));
        assert_eq!(persisted.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn token_persists_the_main_session_and_leaves_its_map_alone() {
        let main = Main::new();
        let before = main.0.serialize_tokens().unwrap();
        let persisted = AtomicUsize::new(0);
        let m = FakeManifests::default();
        let auth = CloudAuth::with_parts(
            &main,
            || {
                persisted.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            &m,
            ok_exchange(),
        );
        let t = auth.token(100, &creds()).await.unwrap();
        assert_eq!((t.access_token(), t.user_id()), ("a", "7"));
        assert_eq!(persisted.load(Ordering::SeqCst), 1);
        assert_eq!(main.0.serialize_tokens().unwrap(), before);

        let refused = CloudAuth::with_parts(
            &main,
            || Ok(()),
            &m,
            FakeExchange(|gogid| Err(CloudError::NoLicense { gogid })),
        );
        assert_eq!(refused.token(9, &creds()).await.unwrap_err().exit_code(), 3);
    }

    #[tokio::test]
    async fn a_failed_persist_is_an_auth_error() {
        let main = Main::new();
        let m = FakeManifests::default();
        let auth = CloudAuth::with_parts(
            &main,
            || Err(AuthError::Store("locked".into())),
            &m,
            ok_exchange(),
        );
        let e = auth.token(100, &creds()).await.unwrap_err();
        assert!(matches!(e, CloudError::Auth(_)));
        assert_eq!(e.exit_code(), 3);
    }

    #[tokio::test]
    async fn no_core_is_not_logged_in() {
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
        let m = FakeManifests::default();
        let auth = CloudAuth::with_parts(&Static, || Ok(()), &m, ok_exchange());
        assert!(matches!(
            auth.token(1, &creds()).await,
            Err(CloudError::NotLoggedIn)
        ));
    }

    #[test]
    fn separate_core_holds_only_the_galaxy_token() {
        let x = SeparateCore::seeded(galaxy_token()).unwrap();
        assert_eq!(x.client_ids(), vec![GALAXY_CLIENT_ID.to_string()]);
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

    fn kind(k: ErrorKind) -> gog_warp::Error {
        gog_warp::Error::new(k, None::<String>)
    }

    #[tokio::test]
    async fn a_403_is_no_license_only_from_the_exchange() {
        let e = exchange_error(request_error(status_error(403).await), 1495134320);
        assert!(matches!(e, CloudError::NoLicense { gogid: 1495134320 }));
        assert_eq!(e.exit_code(), 3);

        let e = content_error(request_error(status_error(403).await), 1495134320);
        assert!(matches!(
            e,
            CloudError::Manifest {
                status: Some(403),
                ..
            }
        ));
        assert!(!e.to_string().contains("license"));
        assert_eq!(e.exit_code(), 1);

        let e = content_error(request_error(status_error(404).await), 1);
        assert!(e.to_string().contains("build manifest") && e.to_string().contains("404"));
        assert!(!e.to_string().contains("exchange"));
    }

    #[tokio::test]
    async fn exchange_errors_map_without_the_url() {
        let e = exchange_error(request_error(status_error(502).await), 1);
        assert!(matches!(e, CloudError::ExchangeStatus { status: 502, .. }));
        assert!(!format!("{e} {e:?}").contains(SECRET));

        // 400 and 401 don't send the user to re-login for certain.
        for (k, status) in [
            (ErrorKind::InvalidSession, 400),
            (ErrorKind::Unauthorized, 401),
        ] {
            let e = exchange_error(kind(k), 1);
            assert!(
                matches!(e, CloudError::ExchangeRejected { status: s, .. } if s == status),
                "{e:?}"
            );
            assert_eq!(e.exit_code(), 3);
            assert!(
                e.to_string()
                    .contains("session or the game's client credentials")
            );
        }
        assert!(matches!(
            exchange_error(kind(ErrorKind::NotLoggedIn), 1),
            CloudError::NotLoggedIn
        ));
        assert!(matches!(
            exchange_error(kind(ErrorKind::MaximumRetries), 1),
            CloudError::Network(_)
        ));
        // The main session's own refresh failing is a re-login.
        assert!(matches!(
            content_error(kind(ErrorKind::InvalidSession), 1),
            CloudError::InvalidSession
        ));
    }

    /// `auth_error` keys on this text; a gog-warp bump that changes it fails here.
    #[test]
    fn invalid_session_text_is_pinned() {
        let text = kind(ErrorKind::InvalidSession).to_string();
        assert!(text.contains("re-login"), "{text}");
        let via_auth = AuthError::from(kind(ErrorKind::InvalidSession));
        assert!(matches!(auth_error(via_auth), CloudError::InvalidSession));
        assert!(matches!(
            auth_error(AuthError::from(kind(ErrorKind::NotLoggedIn))),
            CloudError::NotLoggedIn
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
        let e = exchange_error(request_error(err), 1);
        assert!(matches!(e, CloudError::Network(_)));
        assert_eq!(e.exit_code(), 4);
        assert!(!format!("{e} {e:?}").contains(SECRET));
    }
}
