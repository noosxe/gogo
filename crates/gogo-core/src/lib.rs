//! gogo-core — frontend-agnostic core of gogo: GOG authentication, catalog access,
//! the chunked download engine, and local state (`docs/design/01-architecture.md` §1).
//!
//! Frontends (CLI now, TUI in v2) call plan/execute and consume the typed
//! `ProgressEvent` stream — nothing in this crate knows a terminal exists.

pub mod app;
pub mod auth;
pub mod config;
pub mod engine;
pub mod fmt;
pub mod gog;
pub mod macapp;
pub mod search;
pub mod state;

#[cfg(test)]
pub(crate) mod test_util {
    /// Serializes tests that mutate process env (`GOGO_HOME`).
    pub fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }
}
/// Convenience re-exports for consumers of the library.
pub mod prelude {
    pub use crate::app::AppContext;
    pub use crate::config::Config;
    pub use crate::engine::events::ProgressEvent;
}
