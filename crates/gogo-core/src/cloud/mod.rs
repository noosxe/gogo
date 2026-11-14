//! GOG cloud saves (`docs/design/22-cloud-saves.md`).

pub mod auth;
pub mod backup;
pub mod build_files;
pub mod classify;
pub mod client_id;
pub mod error;
pub mod http;
pub mod locations;
pub mod pull;
pub mod record;
pub mod remote_config;
pub mod save_roots;
pub mod scan;
pub mod status;
pub mod storage;
#[cfg(any(test, feature = "test-util"))]
pub mod testing;
pub mod write;
