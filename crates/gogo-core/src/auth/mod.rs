//! Authentication: [`Session`] (gog-warp wrapper) behind [`TokenProvider`], token
//! storage in [`store`] (`docs/design/02`).

pub mod store;

use crate::gog::error::AuthError;

/// The one real seam (docs/design/01 §2): everything below `auth` depends on this
/// interface, not on gog-warp — tests inject a static token.
#[async_trait::async_trait]
pub trait TokenProvider: Send + Sync {
    /// Always-fresh access token.
    async fn bearer(&self) -> Result<String, AuthError>;
    fn user_id(&self) -> Option<u64>;
}

/// Live impl wrapping `gog_warp::Core` (scope per docs/design/01 §2: token exchange,
/// refresh, owned-product ids only). gog-warp refreshes lazily once
/// `login_time + expires_in` passed (live: 3600s exactly).
pub struct Session {
    // core: gog_warp::Core — held here; upstream break is contained to this file + store.rs.
}

impl Session {
    /// Browser paste-code flow (docs/design/02 §2): gog-warp performs the token
    /// exchange; we never call the token endpoint ourselves.
    pub async fn login(_code: &str) -> Result<Self, AuthError> {
        todo!("Core::new() → get_token_with_code — docs/design/02 §2")
    }

    /// Restore from a [`store::TokenStore`] blob (deserialize_tokens →
    /// start_session_from_refresh_token).
    pub async fn restore(_store: &dyn store::TokenStore) -> Result<Self, AuthError> {
        todo!("docs/design/02 §4")
    }

    /// After any refresh, compare `serialize_tokens()` with the stored snapshot;
    /// changed → re-save (handles rotation even though live tokens proved stable).
    pub async fn persist_if_changed(
        &self,
        _store: &dyn store::TokenStore,
    ) -> Result<(), AuthError> {
        todo!("docs/design/02 §4")
    }
}

#[async_trait::async_trait]
impl TokenProvider for Session {
    async fn bearer(&self) -> Result<String, AuthError> {
        todo!("obtain_galaxy_token() — docs/design/01 §2")
    }

    fn user_id(&self) -> Option<u64> {
        todo!("docs/design/02 §2 (from user data)")
    }
}
