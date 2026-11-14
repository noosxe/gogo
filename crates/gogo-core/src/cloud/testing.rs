//! Test doubles for the cloud-saves code, here and in the CLI's tests: a main
//! session with a made-up Galaxy token (no network), a fixed token exchange,
//! a manifest source that has nothing, and a kept manifest with made-up
//! client credentials. No real token, secret or id appears here.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use gog_warp::Core;
use gog_warp::auth::types::Token;
use gog_warp::constants::GALAXY_CLIENT_ID;
use gog_warp::content_system::types::{Build, Manifest};

use super::auth::{ClientCredentials, CloudToken, ManifestSource, TokenExchange};
use super::error::CloudError;
use crate::auth::TokenProvider;
use crate::gog::error::AuthError;

/// The made-up user id the fixed exchange answers with: storage paths are
/// `/v1/7/<clientId>`.
pub const USER_ID: &str = "7";

/// A made-up client secret.
pub const SECRET: &str = "made-up-secret-0123456789abcdef";

/// A main session backed by a real `Core` holding a fresh made-up Galaxy
/// token, so `bearer()` needs no network.
pub struct MainSession(Core);

impl MainSession {
    pub fn new() -> Self {
        let token: Token = serde_json::from_value(serde_json::json!({
            "access_token": "made-up-access",
            "refresh_token": "made-up-refresh",
            "user_id": "1",
            "expires_in": 3600,
            "token_type": "bearer",
            "session_id": "s",
            "login_time": chrono::Utc::now().timestamp()
        }))
        .expect("a token");
        let core = Core::new();
        let map = HashMap::from([(GALAXY_CLIENT_ID.to_string(), token)]);
        core.deserialize_tokens(&serde_json::to_string(&map).expect("json"))
            .expect("tokens");
        Self(core)
    }
}

impl Default for MainSession {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl TokenProvider for MainSession {
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

/// An exchange that answers every game with a made-up token for
/// [`USER_ID`], or with `fail`'s error; counts its calls.
#[derive(Default)]
pub struct FixedExchange {
    pub calls: AtomicUsize,
    pub fail: Option<fn(u64) -> CloudError>,
}

#[async_trait::async_trait]
impl TokenExchange for &FixedExchange {
    async fn exchange(
        &self,
        _galaxy: Token,
        gogid: u64,
        _: &ClientCredentials,
    ) -> Result<CloudToken, CloudError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.fail {
            Some(f) => Err(f(gogid)),
            None => Ok(CloudToken::new("made-up-game-token", USER_ID, 3600)),
        }
    }
}

/// A content system with no builds.
pub struct NoManifests;

#[async_trait::async_trait]
impl ManifestSource for NoManifests {
    async fn builds(&self, _: u64) -> Result<Vec<Build>, CloudError> {
        Ok(vec![])
    }
    async fn manifest(&self, gogid: u64, _: &Build) -> Result<Manifest, CloudError> {
        Err(CloudError::Manifest {
            gogid,
            status: None,
        })
    }
}

/// Keep a v2 manifest with `client_id` and [`SECRET`] in game dir `root`.
pub fn keep_manifest(root: &Path, client_id: &str) {
    let m: Manifest = serde_json::from_value(serde_json::json!({
        "baseProductId": "100",
        "depots": [],
        "installDirectory": "Game",
        "platform": "windows",
        "products": [],
        "clientId": client_id,
        "clientSecret": SECRET,
    }))
    .expect("a manifest");
    crate::gog::build_manifest::write(root, "2", 2, &m).expect("manifest written");
}
