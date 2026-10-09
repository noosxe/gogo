//! `TokenStore` trait + KeyringStore + FileStore (`docs/design/02` §3).
//!
//! The persisted value is gog-warp's `serialize_tokens()` JSON blob, stored verbatim —
//! gog-warp owns the format, we own the at-rest location.

use crate::gog::error::AuthError;

#[async_trait::async_trait]
pub trait TokenStore: Send + Sync {
    fn load(&self) -> Result<Option<String>, AuthError>;
    fn save(&self, blob: &str, user_id: Option<u64>) -> Result<(), AuthError>;
    fn clear(&self) -> Result<(), AuthError>;
}

/// service `"gogo"`, account = user_id or `"default"`. Unavailable platform secret
/// service → warn once, fall back to [`FileStore`] for this run (config stays `keyring`).
pub struct KeyringStore;

#[async_trait::async_trait]
impl TokenStore for KeyringStore {
    fn load(&self) -> Result<Option<String>, AuthError> {
        todo!("keyring::Entry(service=\"gogo\") — docs/design/02 §3")
    }

    fn save(&self, _blob: &str, _user_id: Option<u64>) -> Result<(), AuthError> {
        todo!("docs/design/02 §3")
    }

    fn clear(&self) -> Result<(), AuthError> {
        todo!("docs/design/02 §3")
    }
}

/// `tokens.json` under the state dir (`$XDG_STATE_HOME/gogo`, macOS falls back to
/// Application Support — 06 §1): machine-managed state, not config; created `0600`,
/// parents `0700`; wider existing permissions are tightened (with a warning) before writing.
pub struct FileStore;

#[async_trait::async_trait]
impl TokenStore for FileStore {
    fn load(&self) -> Result<Option<String>, AuthError> {
        todo!("0600 file read — docs/design/02 §3")
    }

    fn save(&self, _blob: &str, _user_id: Option<u64>) -> Result<(), AuthError> {
        todo!("atomic write, mode tighten — docs/design/02 §3")
    }

    fn clear(&self) -> Result<(), AuthError> {
        todo!("docs/design/02 §3")
    }
}

/// Selected by `auth.token_store`; keyring-unavailable fallback happens here.
pub fn open(kind: crate::config::TokenStoreKind) -> std::sync::Arc<dyn TokenStore> {
    match kind {
        crate::config::TokenStoreKind::Keyring => std::sync::Arc::new(KeyringStore),
        crate::config::TokenStoreKind::File => std::sync::Arc::new(FileStore),
    }
}
