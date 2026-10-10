//! state.json load/save — atomic, fsync, fs4 advisory lock (`docs/design/06` §2/§3).

use serde::{Deserialize, Serialize};

use crate::state::records::InstallRecord;

pub mod cache;
pub mod records;

/// Base dir for machine state (`docs/design/06` §1): `$XDG_STATE_HOME/gogo` on Linux,
/// `~/Library/Application Support/gogo` on macOS; `GOGO_HOME` collapses everything.
pub fn state_dir() -> std::path::PathBuf {
    if let Some(home) = std::env::var_os("GOGO_HOME") {
        return std::path::PathBuf::from(home);
    }
    if let Some(dir) = dirs::state_dir() {
        return dir.join("gogo");
    }
    match dirs::data_dir() {
        Some(dir) => dir.join("gogo"),
        None => std::path::PathBuf::from(".gogo"),
    }
}
/// Local install state (`docs/design/06` §3).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    /// One record per downloaded `files[]` entry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub installs: Vec<InstallRecord>,
}

impl State {
    /// Absolute path of state.json (`docs/design/06` §1; GOGO_HOME flattens).
    pub fn path() -> std::path::PathBuf {
        todo!("platform dirs per docs/design/06 §1")
    }

    /// Load or default; corrupt state → warn + default (never hard-fail reads).
    pub fn load() -> anyhow::Result<Self> {
        todo!("docs/design/06 §3")
    }

    /// Atomic save: temp + fsync + rename, under the fs4 advisory lock.
    pub fn save(&self) -> anyhow::Result<()> {
        todo!("docs/design/06 §3")
    }
}
