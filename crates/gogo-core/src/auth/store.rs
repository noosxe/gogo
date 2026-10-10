//! `TokenStore` trait + `KeyringStore` + `FileStore` (`docs/design/02` §3).
//!
//! The persisted value is gog-warp's `serialize_tokens()` JSON blob, stored verbatim —
//! gog-warp owns the format, we own the at-rest location.
//!
//! Review A6 (owner decision 2026-10-09): the backend is **config-decided with no
//! silent fallback**. Keyring configured but the platform secret service is unavailable
//! → loud `AuthError::Store` (exit 3) carrying the switch hint; `login` calls
//! [`TokenStore::probe`] before the browser ever opens. Switching backends never
//! migrates tokens — re-login. `logout` clears the configured backend.

use std::path::PathBuf;

use crate::gog::error::AuthError;

pub trait TokenStore: Send + Sync {
    /// Cheap availability probe — the early, loud, actionable failure for A6. Called
    /// by the login flow *before* opening the browser. Default: available.
    fn probe(&self) -> Result<(), AuthError> {
        Ok(())
    }

    fn load(&self) -> Result<Option<String>, AuthError>;
    fn save(&self, blob: &str) -> Result<(), AuthError>;
    fn clear(&self) -> Result<(), AuthError>;
}

const SERVICE: &str = "gogo";
const ACCOUNT: &str = "default"; // user id lives inside the blob itself

/// Platform keyring (macOS native / secret-service on Linux via crate features).
pub struct KeyringStore;

impl KeyringStore {
    fn entry() -> Result<keyring::Entry, AuthError> {
        keyring::Entry::new(SERVICE, ACCOUNT).map_err(|e| Self::store_err("open entry", &e))
    }

    /// The A6 error: loud, explained, actionable.
    fn store_err(what: &str, e: &keyring::Error) -> AuthError {
        AuthError::Store(format!(
            "keyring: {what} failed: {e}. If no secret service is available, \
             switch backends: gogo config set auth.token_store file"
        ))
    }
}

impl TokenStore for KeyringStore {
    fn probe(&self) -> Result<(), AuthError> {
        // Store initialization result (one-time, includes connecting to the service):
        // the cheapest complete availability check — and the loud A6 failure.
        if let Err(e) = keyring::Entry::store_status() {
            return Err(Self::store_err("store init", e));
        }
        // Belt and suspenders: a real write+delete on the probe account.
        let entry = Self::entry()?;
        match entry.set_password("gogo keyring probe") {
            Ok(()) => {
                // A failed delete of our own probe value is not fatal.
                let _ = entry.delete_credential();
                Ok(())
            }
            Err(e) => Err(Self::store_err("probe write", &e)),
        }
    }

    fn load(&self) -> Result<Option<String>, AuthError> {
        match Self::entry()?.get_password() {
            Ok(blob) => Ok(Some(blob)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(Self::store_err("read", &e)),
        }
    }

    fn save(&self, blob: &str) -> Result<(), AuthError> {
        Self::entry()?
            .set_password(blob)
            .map_err(|e| Self::store_err("write", &e))
    }

    fn clear(&self) -> Result<(), AuthError> {
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(Self::store_err("clear", &e)),
        }
    }
}

/// `tokens.json` under the state dir (06 §1): machine-managed state, not config.
/// Created `0600`, parents `0700`; wider existing permissions are tightened (with a
/// warning) before writing.
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    /// Default location: `<state_dir>/tokens.json`.
    pub fn new() -> Self {
        Self {
            path: crate::state::state_dir().join("tokens.json"),
        }
    }

    /// Explicit path (tests).
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Default for FileStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenStore for FileStore {
    fn load(&self) -> Result<Option<String>, AuthError> {
        match std::fs::read_to_string(&self.path) {
            Ok(blob) => Ok(Some(blob)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(AuthError::Store(format!("tokens file read: {e}"))),
        }
    }

    fn save(&self, blob: &str) -> Result<(), AuthError> {
        use std::os::unix::fs::PermissionsExt;

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AuthError::Store(format!("tokens dir create: {e}")))?;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }

        // Tighten an existing too-wide file before writing (02 §3).
        if self.path.is_file() {
            let mode = std::fs::metadata(&self.path)
                .map(|m| m.permissions().mode())
                .unwrap_or(0);
            if mode & 0o077 != 0 {
                tracing::warn!("tokens file had group/other bits set — tightening to 0600");
                let _ =
                    std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
            }
        }

        // Atomic: temp + rename, temp created with 0600.
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, blob).map_err(|e| AuthError::Store(format!("tokens write: {e}")))?;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| AuthError::Store(format!("tokens rename: {e}")))?;
        Ok(())
    }

    fn clear(&self) -> Result<(), AuthError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AuthError::Store(format!("tokens remove: {e}"))),
        }
    }
}

/// OS keychain reads can block indefinitely waiting for user authorization —
/// observed live on macOS (2026-10-10): an unattended Security Agent prompt left
/// `find_generic_password` parked forever on a worker thread, hanging the whole
/// command. A batch CLI must fail loudly instead: bound the synchronous read and
/// explain the way out.
pub async fn load_guarded(
    store: std::sync::Arc<dyn TokenStore>,
    timeout: std::time::Duration,
) -> Result<Option<String>, AuthError> {
    match tokio::time::timeout(timeout, tokio::task::spawn_blocking(move || store.load())).await {
        Ok(Ok(result)) => result,
        Ok(Err(join)) => Err(AuthError::Store(format!(
            "token store read panicked: {join}"
        ))),
        Err(_) => Err(AuthError::Store(
            "token store read still pending — the OS credential store is likely waiting \
             for user authorization (unlock the keychain / answer the prompt), or switch \
             backends: gogo config set auth.token_store file"
                .into(),
        )),
    }
}

/// Selected by `auth.token_store` — and ONLY by that (A6: no probing, no fallback).
pub fn open(kind: crate::config::TokenStoreKind) -> std::sync::Arc<dyn TokenStore> {
    match kind {
        crate::config::TokenStoreKind::Keyring => std::sync::Arc::new(KeyringStore),
        crate::config::TokenStoreKind::File => std::sync::Arc::new(FileStore::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_round_trip_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::at(dir.path().join("tokens.json"));

        assert_eq!(store.load().unwrap(), None);
        store.save("{\"access_token\":\"x\"}").unwrap();
        assert_eq!(
            store.load().unwrap().as_deref(),
            Some("{\"access_token\":\"x\"}")
        );

        // 0600, parent 0700.
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("tokens.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let pmode = std::fs::metadata(dir.path()).unwrap().permissions().mode();
        assert_eq!(pmode & 0o777, 0o700);

        store.clear().unwrap();
        assert_eq!(store.load().unwrap(), None);
        store.clear().unwrap(); // clear is idempotent
    }

    #[test]
    fn file_store_tightens_wide_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        FileStore::at(&path).save("blob").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn open_maps_configured_kind_no_fallback() {
        // Structural: both kinds construct; keyring availability is only probed
        // explicitly (A6), never at open() time.
        let _ = open(crate::config::TokenStoreKind::Keyring);
        let _ = open(crate::config::TokenStoreKind::File);
    }
}

#[cfg(test)]
mod guarded_tests {
    use super::*;
    use std::sync::Arc;

    struct HangingStore;
    impl TokenStore for HangingStore {
        fn load(&self) -> Result<Option<String>, AuthError> {
            std::thread::sleep(std::time::Duration::from_millis(500));
            Ok(None)
        }
        fn save(&self, _blob: &str) -> Result<(), AuthError> {
            Ok(())
        }
        fn clear(&self) -> Result<(), AuthError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn guarded_read_fails_loudly_when_store_never_answers() {
        let err = load_guarded(Arc::new(HangingStore), std::time::Duration::from_millis(50))
            .await
            .unwrap_err();
        match err {
            AuthError::Store(msg) => assert!(msg.contains("user authorization"), "{msg}"),
            other => panic!("expected Store error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn guarded_read_passes_through_within_budget() {
        struct OkStore;
        impl TokenStore for OkStore {
            fn load(&self) -> Result<Option<String>, AuthError> {
                Ok(Some("blob".into()))
            }
            fn save(&self, _blob: &str) -> Result<(), AuthError> {
                Ok(())
            }
            fn clear(&self) -> Result<(), AuthError> {
                Ok(())
            }
        }
        assert_eq!(
            load_guarded(Arc::new(OkStore), std::time::Duration::from_secs(5))
                .await
                .unwrap(),
            Some("blob".into())
        );
    }
}
