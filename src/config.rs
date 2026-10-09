//! Config schema, defaults < file < env layering merge, validation (`docs/design/05`).

use serde::Deserialize;

/// Fully-merged configuration (docs/design/05 §2–§4).
///
/// Precedence: built-in defaults < `gogo.toml` < `GOGO_*` env vars < CLI flags.
/// `GOGO_HOME` flattens config/state/cache into one dir (tests / portable use).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub auth: AuthConfig,
    pub network: NetworkConfig,
    pub download: DownloadConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    /// `keyring` (default) with transparent file fallback, or `file` directly
    /// (docs/design/02 §3).
    pub token_store: TokenStoreKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenStoreKind {
    Keyring,
    File,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkConfig {
    /// Parallel API-call pool, separate from CDN workers (docs/design/01 §3).
    pub api_parallel: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DownloadConfig {
    /// Files run sequentially by default (docs/design/01 §3).
    pub parallel_files: u32,
    /// Chunk workers per file.
    pub workers: u32,
    /// Global bytes/s governor, e.g. "4MiB/s" (docs/design/05 §3 rate grammar).
    pub rate: Option<String>,
    /// Download root; default per platform dirs (docs/design/06 §1).
    pub dir: Option<String>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            token_store: TokenStoreKind::Keyring,
        }
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self { api_parallel: 4 }
    }
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            parallel_files: 1,
            workers: 3,
            rate: None,
            dir: None,
        }
    }
}

impl Config {
    /// defaults < file < env overlay (`docs/design/05` §4); invalid values → exit 6.
    pub fn load() -> Result<Self, crate::gog::error::ApiError> {
        todo!("layering merge + GOGO_* env overlay — docs/design/05 §3–§4")
    }

    /// Absolute path of the config file (creating nothing) — `gogo config path`.
    pub fn path() -> std::path::PathBuf {
        todo!("platform dirs per docs/design/06 §1 (GOGO_HOME flattens)")
    }
}
