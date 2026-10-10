//! Config schema, defaults < file < env layering merge, validation (`docs/design/05`).
//!
//! Layering is hand-rolled (no figment): three merge steps over one serde struct,
//! fully unit-testable via [`Config::load_from`] (env access injected). CLI flags are
//! applied by the frontend on top (05 §5) — they are not part of this layering.
//!
//! Exit-code contract: any [`ConfigError`] is a usage-class error (exit 2, 03 §5).

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Fully-merged configuration (`docs/design/05` §3 schema).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub auth: AuthConfig,
    pub gog: GogConfig,
    pub network: NetworkConfig,
    pub download: DownloadConfig,
    pub state: StateConfig,
}

/// No silent fallback (review A6): keyring configured but unavailable fails loudly in
/// the auth layer; this config only records the choice.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    pub token_store: TokenStoreKind,
    /// Optional override of the Galaxy public client id — a public value, not a secret
    /// (review C3). Unset = built-in default (02 §1).
    pub client_id: Option<String>,
    /// Optional override of the Galaxy public client secret (review C3).
    pub client_secret: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenStoreKind {
    #[default]
    Keyring,
    File,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GogConfig {
    /// Preferred installer language (fallback chain: 04 §1).
    pub language: String,
    pub platform: Platform,
    /// Products API locale (localized titles).
    pub locale: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    #[default]
    Current,
    Windows,
    Mac,
    Linux,
    All,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    /// Per-request timeout for API JSON calls.
    pub timeout_secs: u64,
    /// No-progress window on CDN streams.
    pub idle_timeout_secs: u64,
    /// Max attempts per retryable request (07 §3; also the re-resolve budget, B3).
    pub retries: u32,
    /// Concurrent catalog/downlink/checksum fetches.
    pub api_parallel: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DownloadConfig {
    /// Download root; `None` = platform default (`<Downloads>/gogo`), resolved at load.
    pub dir: Option<String>,
    /// Parallel chunk connections per file (1..=16).
    pub workers: u32,
    /// Simultaneous files (1..=4).
    pub parallel_files: u32,
    /// Global bytes/sec; 0 = unlimited (05 §3 rate grammar).
    pub limit: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StateConfig {
    /// Catalog cache freshness (06 §3).
    pub catalog_ttl_hours: u32,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            token_store: TokenStoreKind::Keyring,
            client_id: None,
            client_secret: None,
        }
    }
}

impl Default for GogConfig {
    fn default() -> Self {
        Self {
            language: "en".into(),
            platform: Platform::Current,
            locale: "en-US".into(),
        }
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            timeout_secs: 30,
            idle_timeout_secs: 30,
            retries: 5,
            api_parallel: 4,
        }
    }
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            dir: None,
            workers: 3,
            parallel_files: 1,
            limit: 0,
        }
    }
}

impl Default for StateConfig {
    fn default() -> Self {
        Self {
            catalog_ttl_hours: 24,
        }
    }
}

/// Result of [`Config::load`]: merged config + non-fatal warnings + source path.
#[derive(Debug)]
pub struct Loaded {
    pub config: Config,
    /// Unknown TOML keys (ignored, forward compatibility — 05 §1).
    pub warnings: Vec<String>,
    /// File the config was read from (may not exist).
    pub path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file unreadable: {0}")]
    Io(#[from] std::io::Error),
    #[error("config parse error: {0}")]
    Parse(String),
    #[error("invalid value for {key}: {reason}")]
    Invalid { key: String, reason: String },
}

/// Rate grammar: `<number>[.<number>]<unit>[/s]`, unit ∈ B|KiB|MiB|GiB (binary,
/// case-insensitive); bare numbers are bytes/sec; 0 = unlimited. Sub-32 KiB/s nonzero
/// values are rejected with a usage error (05 §3).
pub fn parse_rate(input: &str) -> Result<u64, ConfigError> {
    const FLOOR: u64 = 32 * 1024;
    let key = "download.limit";
    let s = input.trim();
    let s = s
        .strip_suffix("/s")
        .or_else(|| s.strip_suffix("/S"))
        .unwrap_or(s);
    let split = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let unit = unit.to_ascii_lowercase();
    let mult: u64 = match unit.as_str() {
        "b" | "" => 1,
        "kib" => 1024,
        "mib" => 1024 * 1024,
        "gib" => 1024 * 1024 * 1024,
        other => {
            return Err(ConfigError::Invalid {
                key: key.into(),
                reason: format!("`{input}`: unknown unit `{other}` (B, KiB, MiB, GiB)"),
            });
        }
    };
    let value: f64 = num.parse().map_err(|_| ConfigError::Invalid {
        key: key.into(),
        reason: format!("`{input}`: not a number"),
    })?;
    if value < 0.0 {
        return Err(ConfigError::Invalid {
            key: key.into(),
            reason: format!("`{input}`: negative"),
        });
    }
    let bytes = (value * mult as f64).round() as u64;
    if bytes != 0 && bytes < FLOOR {
        return Err(ConfigError::Invalid {
            key: key.into(),
            reason: format!("`{input}`: below the 32 KiB/s floor (pointless, invites stalls)"),
        });
    }
    Ok(bytes)
}

impl Config {
    /// defaults < file < env, from the real environment (05 §1, §4).
    pub fn load() -> Result<Loaded, ConfigError> {
        let path = Self::env_config_path().unwrap_or_else(Self::default_path);
        Self::load_from(&path, |k| std::env::var(k).ok())
    }

    /// Injectable variant (tests): `env` stands in for `std::env::var`.
    pub fn load_from(
        path: &Path,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Loaded, ConfigError> {
        let mut config = Config::default();
        let mut warnings = Vec::new();

        if path.is_file() {
            let raw = std::fs::read_to_string(path)?;
            let table: toml::Table = raw
                .parse()
                .map_err(|e| ConfigError::Parse(format!("{}: {e}", path.display())))?;
            warnings.extend(unknown_keys(&table));
            config = Config::deserialize(table).map_err(|e| ConfigError::Parse(e.to_string()))?;
        }

        apply_env(&mut config, &env)?;
        validate(&mut config)?;
        resolve_dir(&mut config);

        Ok(Loaded {
            config,
            warnings,
            path: path.to_path_buf(),
        })
    }

    /// `GOGO_CONFIG` if set (05 §2).
    fn env_config_path() -> Option<PathBuf> {
        std::env::var_os("GOGO_CONFIG").map(PathBuf::from)
    }

    /// Platform default config path; `GOGO_HOME` collapses dirs (06 §1).
    pub fn default_path() -> PathBuf {
        if let Some(home) = std::env::var_os("GOGO_HOME") {
            return PathBuf::from(home).join("config.toml");
        }
        match dirs::config_dir() {
            Some(dir) => dir.join("gogo").join("config.toml"),
            None => PathBuf::from("gogo").join("config.toml"),
        }
    }
}

/// Flattened `GOGO_<NAME>` overlay (05 §4 normative table; review C2). Bad values are
/// loud errors naming the variable.
fn apply_env(
    config: &mut Config,
    env: &impl Fn(&str) -> Option<String>,
) -> Result<(), ConfigError> {
    if let Some(v) = env("GOGO_DOWNLOAD_DIR") {
        config.download.dir = Some(v);
    }
    if let Some(v) = env("GOGO_WORKERS") {
        config.download.workers = v.parse().map_err(|_| bad_env("GOGO_WORKERS", &v))?;
    }
    if let Some(v) = env("GOGO_PARALLEL_FILES") {
        config.download.parallel_files =
            v.parse().map_err(|_| bad_env("GOGO_PARALLEL_FILES", &v))?;
    }
    if let Some(v) = env("GOGO_LIMIT") {
        config.download.limit = parse_rate(&v).map_err(|_| bad_env("GOGO_LIMIT", &v))?;
    }
    if let Some(v) = env("GOGO_TOKEN_STORE") {
        config.auth.token_store = v.parse().map_err(|_| bad_env("GOGO_TOKEN_STORE", &v))?;
    }
    if let Some(v) = env("GOGO_LANGUAGE") {
        config.gog.language = v;
    }
    if let Some(v) = env("GOGO_PLATFORM") {
        config.gog.platform = v.parse().map_err(|_| bad_env("GOGO_PLATFORM", &v))?;
    }
    if let Some(v) = env("GOGO_LOCALE") {
        config.gog.locale = v;
    }
    if let Some(v) = env("GOGO_RETRIES") {
        config.network.retries = v.parse().map_err(|_| bad_env("GOGO_RETRIES", &v))?;
    }
    Ok(())
}

fn bad_env(name: &str, value: &str) -> ConfigError {
    ConfigError::Invalid {
        key: name.into(),
        reason: format!("`{value}` does not parse"),
    }
}

/// Post-merge range checks (05 §6).
fn validate(config: &mut Config) -> Result<(), ConfigError> {
    let checks = [
        ("download.workers", config.download.workers, 1, 16),
        (
            "download.parallel_files",
            config.download.parallel_files,
            1,
            4,
        ),
        ("network.retries", config.network.retries, 0, 20),
        ("network.api_parallel", config.network.api_parallel, 1, 16),
    ];
    for (key, value, min, max) in checks {
        if !(min..=max).contains(&value) {
            return Err(ConfigError::Invalid {
                key: key.into(),
                reason: format!("{value} not in {min}..={max}"),
            });
        }
    }
    Ok(())
}

/// `~` expansion + absolutization; the engine only ever sees the resolved path (05 §6).
fn resolve_dir(config: &mut Config) {
    let raw = config
        .download
        .dir
        .clone()
        .unwrap_or_else(default_download_dir);
    let resolved = if let Some(rest) = raw.strip_prefix("~/") {
        dirs::home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(raw.clone()))
    } else {
        PathBuf::from(raw)
    };
    config.download.dir = Some(
        std::path::absolute(&resolved)
            .unwrap_or(resolved)
            .to_string_lossy()
            .into_owned(),
    );
}

fn default_download_dir() -> String {
    let base = dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("gogo").to_string_lossy().into_owned()
}

impl std::str::FromStr for TokenStoreKind {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "keyring" => Ok(Self::Keyring),
            "file" => Ok(Self::File),
            _ => Err(()),
        }
    }
}

impl std::str::FromStr for Platform {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "current" => Ok(Self::Current),
            "windows" => Ok(Self::Windows),
            "mac" => Ok(Self::Mac),
            "linux" => Ok(Self::Linux),
            "all" => Ok(Self::All),
            _ => Err(()),
        }
    }
}

/// Unknown-key detection for forward compatibility (05 §1): top-level sections and
/// their known keys. Anything else in the TOML produces a warning, not an error.
fn unknown_keys(table: &toml::Table) -> Vec<String> {
    const KNOWN: &[(&str, &[&str])] = &[
        ("auth", &["token_store", "client_id", "client_secret"]),
        ("gog", &["language", "platform", "locale"]),
        (
            "network",
            &[
                "timeout_secs",
                "idle_timeout_secs",
                "retries",
                "api_parallel",
            ],
        ),
        ("download", &["dir", "workers", "parallel_files", "limit"]),
        ("state", &["catalog_ttl_hours"]),
    ];
    let mut out = Vec::new();
    for (section, value) in table {
        match KNOWN.iter().find(|(name, _)| name == section) {
            Some((_, keys)) => {
                if let Some(sub) = value.as_table() {
                    for (key, _value) in sub {
                        if !keys.contains(&key.as_str()) {
                            out.push(format!("{section}.{key}"));
                        }
                    }
                }
            }
            None => out.push(section.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn envs(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn rate_grammar() {
        assert_eq!(parse_rate("0").unwrap(), 0);
        assert_eq!(parse_rate("10485760").unwrap(), 10_485_760);
        assert_eq!(parse_rate("32768").unwrap(), 32_768);
        assert_eq!(parse_rate("4MiB/s").unwrap(), 4 * 1024 * 1024);
        assert_eq!(parse_rate("500kib").unwrap(), 500 * 1024);
        assert_eq!(parse_rate("0.5MiB").unwrap(), 512 * 1024);
        assert_eq!(parse_rate("1GiB/s").unwrap(), 1024_u64.pow(3));
        assert!(parse_rate("32767").is_err(), "below floor");
        assert!(parse_rate("1KiB/s").is_err(), "below floor with unit");
        assert!(parse_rate("4M").is_err(), "decimal unit rejected");
        assert!(parse_rate("fast").is_err());
        assert!(parse_rate("-1").is_err());
    }

    #[test]
    fn missing_file_yields_defaults() {
        let loaded = Config::load_from(Path::new("/nonexistent/gogo.toml"), no_env).unwrap();
        assert_eq!(loaded.config.download.workers, 3);
        assert_eq!(loaded.config.network.retries, 5);
        assert_eq!(loaded.config.auth.token_store, TokenStoreKind::Keyring);
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn toml_overrides_defaults_per_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[download]\nworkers = 8\n\n[network]\nretries = 9\n").unwrap();
        let loaded = Config::load_from(&path, no_env).unwrap();
        assert_eq!(loaded.config.download.workers, 8);
        assert_eq!(loaded.config.network.retries, 9);
        // untouched keys keep defaults
        assert_eq!(loaded.config.download.parallel_files, 1);
    }

    #[test]
    fn env_beats_file_flattened_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[download]\nworkers = 8\n").unwrap();
        let loaded = Config::load_from(&path, envs(&[("GOGO_WORKERS", "12")])).unwrap();
        assert_eq!(loaded.config.download.workers, 12);
    }

    #[test]
    fn bad_env_is_loud() {
        let err = Config::load_from(
            Path::new("/nonexistent/gogo.toml"),
            envs(&[("GOGO_WORKERS", "fast")]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("GOGO_WORKERS"), "{err}");
        let err = Config::load_from(
            Path::new("/nonexistent/gogo.toml"),
            envs(&[("GOGO_LIMIT", "4M")]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("GOGO_LIMIT"), "{err}");
    }

    #[test]
    fn unknown_keys_warn_not_fail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[download]\nworkers = 4\nfuture_key = true\n\n[future_section]\nx = 1\n",
        )
        .unwrap();
        let loaded = Config::load_from(&path, no_env).unwrap();
        assert_eq!(loaded.config.download.workers, 4);
        assert!(loaded.warnings.contains(&"download.future_key".to_string()));
        assert!(loaded.warnings.contains(&"future_section".to_string()));
    }

    #[test]
    fn range_violations_name_the_key() {
        let err = Config::load_from(
            Path::new("/nonexistent/gogo.toml"),
            envs(&[("GOGO_PARALLEL_FILES", "9")]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("download.parallel_files"), "{err}");
    }

    #[test]
    fn dir_is_resolved_absolute() {
        let home = dirs::home_dir().unwrap();
        let loaded = Config::load_from(
            Path::new("/nonexistent/gogo.toml"),
            envs(&[("GOGO_DOWNLOAD_DIR", "~/games")]),
        )
        .unwrap();
        assert_eq!(
            PathBuf::from(loaded.config.download.dir.clone().unwrap()),
            home.join("games")
        );
    }
}
