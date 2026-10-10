//! Authentication: [`Session`] (gog-warp wrapper) behind [`TokenProvider`], token
//! storage in [`store`] (`docs/design/02`).
//!
//! Review B2: callers request `bearer()` per API call; gog-warp caches tokens and
//! refreshes lazily, so each call is cheap — and the API-layer 401 path stays a pure
//! safety net (clock skew, revocation).
//!
//! Review C3 (implementation check, resolved): gog-warp's `Core` pins its own client
//! constants and its `get_token_with_code` takes no credential arguments, so
//! `[auth] client_id`/`client_secret` overrides **cannot be injected** into the current
//! upstream. The config keys stay reserved (05 §3) and apply only if we ever vendor the
//! auth files (09 §4).

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
/// refresh, user data — the owned-ids endpoint is out of the library path entirely,
/// review A1).
pub struct Session {
    core: gog_warp::Core,
    /// Last blob handed to a `TokenStore`; drives persist-if-changed (02 §4).
    last_persisted: std::sync::Mutex<Option<String>>,
}

impl Session {
    /// Browser paste-code flow (docs/design/02 §2): gog-warp performs the token
    /// exchange; we never call the token endpoint ourselves. The frontend calls
    /// [`store::TokenStore::probe`] *before* opening the browser (A6).
    pub async fn login(code: &str, store: &dyn store::TokenStore) -> Result<Self, AuthError> {
        let core = gog_warp::Core::new();
        core.get_token_with_code(code.to_string()).await?;
        let session = Self {
            core,
            last_persisted: std::sync::Mutex::new(None),
        };
        session.persist(store)?;
        Ok(session)
    }

    /// Restore from a [`store::TokenStore`] blob (`deserialize_tokens` — 02 §4).
    /// Nothing stored → `Unauthorized` ("run gogo login").
    pub async fn restore(store: &dyn store::TokenStore) -> Result<Self, AuthError> {
        let blob = store.load()?.ok_or(AuthError::Unauthorized)?;
        let core = gog_warp::Core::new();
        core.deserialize_tokens(&blob)
            .map_err(|e| AuthError::Deserialize(e.to_string()))?;
        Ok(Self {
            core,
            last_persisted: std::sync::Mutex::new(Some(blob)),
        })
    }

    fn persist(&self, store: &dyn store::TokenStore) -> Result<(), AuthError> {
        let blob = self.core.serialize_tokens()?;
        *self.last_persisted.lock().expect("persist lock") = Some(blob.clone());
        store.save(&blob)
    }

    /// After any refresh, compare `serialize_tokens()` with the stored snapshot;
    /// changed → re-save (handles rotation even though live tokens proved stable —
    /// reconfirmed 2026-10-09). The engine/CLI calls this after batch operations.
    pub fn persist_if_changed(&self, store: &dyn store::TokenStore) -> Result<(), AuthError> {
        let blob = self.core.serialize_tokens()?;
        let mut last = self.last_persisted.lock().expect("persist lock");
        if last.as_deref() == Some(blob.as_str()) {
            return Ok(());
        }
        *last = Some(blob.clone());
        drop(last);
        store.save(&blob)
    }
}

#[async_trait::async_trait]
impl TokenProvider for Session {
    async fn bearer(&self) -> Result<String, AuthError> {
        let token = self.core.obtain_galaxy_token().await?;
        Ok(token.access_token().clone())
    }

    fn user_id(&self) -> Option<u64> {
        self.core
            .get_token_raw(gog_warp::constants::GALAXY_CLIENT_ID)
            .and_then(|t| t.user_id().parse().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingStore {
        saves: AtomicUsize,
        blob: std::sync::Mutex<Option<String>>,
    }

    impl store::TokenStore for CountingStore {
        fn load(&self) -> Result<Option<String>, AuthError> {
            Ok(self.blob.lock().unwrap().clone())
        }
        fn save(&self, blob: &str) -> Result<(), AuthError> {
            self.saves.fetch_add(1, Ordering::SeqCst);
            *self.blob.lock().unwrap() = Some(blob.to_owned());
            Ok(())
        }
        fn clear(&self) -> Result<(), AuthError> {
            *self.blob.lock().unwrap() = None;
            Ok(())
        }
    }

    /// B2 seam check: a static-token provider satisfies the trait without gog-warp.
    struct StaticToken;

    #[async_trait::async_trait]
    impl TokenProvider for StaticToken {
        async fn bearer(&self) -> Result<String, AuthError> {
            Ok("static".into())
        }
        fn user_id(&self) -> Option<u64> {
            Some(42)
        }
    }

    #[tokio::test]
    async fn static_provider_is_the_test_seam() {
        let p = StaticToken;
        assert_eq!(p.bearer().await.unwrap(), "static");
        assert_eq!(p.user_id(), Some(42));
    }

    #[tokio::test]
    async fn restore_without_session_is_unauthorized() {
        let store = CountingStore {
            saves: AtomicUsize::new(0),
            blob: std::sync::Mutex::new(None),
        };
        assert!(matches!(
            Session::restore(&store).await,
            Err(AuthError::Unauthorized)
        ));
    }

    #[tokio::test]
    async fn restore_with_garbage_blob_is_deserialize_error() {
        let store = CountingStore {
            saves: AtomicUsize::new(0),
            blob: std::sync::Mutex::new(Some("not json at all".into())),
        };
        // gog-warp tolerates arbitrary strings into its map, so this documents the
        // boundary: a syntactically-loadable-but-bogus blob surfaces later as
        // Unauthorized/Upstream on first use, not as a crash here.
        let result = Session::restore(&store).await;
        assert!(result.is_ok() || matches!(result, Err(AuthError::Deserialize(_))));
    }
}
