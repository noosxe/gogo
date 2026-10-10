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
        state_dir().join("state.json")
    }

    /// Load or default; corrupt state → warn + default (never hard-fail reads).
    /// B5: takes the advisory lock briefly (shared) so a concurrent writer's rename
    /// never interleaves with this read.
    pub fn load() -> anyhow::Result<Self> {
        let path = Self::path();
        if !path.is_file() {
            return Ok(Self::default());
        }
        let _lock = StateLock::shared()?;
        let raw = std::fs::read_to_string(&path)?;
        match serde_json::from_str(&raw) {
            Ok(state) => Ok(state),
            Err(e) => {
                tracing::warn!("state.json corrupt ({e}) — starting from default");
                Ok(Self::default())
            }
        }
    }

    /// Atomic save: temp + fsync + rename, under the advisory state lock (B5: held
    /// only for the short write window).
    pub fn save(&self) -> anyhow::Result<()> {
        use std::io::Write;

        let path = Self::path();
        let _lock = StateLock::exclusive()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, &path)?;
        // Best-effort directory fsync so the rename itself is durable.
        if let Some(parent) = path.parent()
            && let Ok(dir) = std::fs::File::open(parent)
        {
            let _ = dir.sync_all();
        }
        Ok(())
    }
}

/// The B5 state lock: advisory `fs4` lock on `state.lock`, short-held (write window)
/// or briefly shared (reads) — `gogo list` never fails because a download is running.
/// Download exclusivity is separate: per-destination `.part` flocks in the engine
/// (06 §1). A second process skips locked destinations with a `Warn`.
struct StateLock(std::fs::File);

impl StateLock {
    fn lock_file() -> anyhow::Result<std::fs::File> {
        let path = state_dir().join("state.lock");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(std::fs::File::create(path)?)
    }

    fn shared() -> anyhow::Result<Self> {
        let file = Self::lock_file()?;
        file.lock_shared()?;
        Ok(Self(file))
    }

    fn exclusive() -> anyhow::Result<Self> {
        let file = Self::lock_file()?;
        file.lock()?;
        Ok(Self(file))
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_corrupt_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::test_util::env_lock();
        // SAFETY: env mutation under the shared env mutex.
        unsafe { std::env::set_var("GOGO_HOME", dir.path()) };

        assert!(State::load().unwrap().installs.is_empty());

        let mut state = State::default();
        state.installs.push(InstallRecord {
            product_id: 7,
            dlc_id: None,
            product_title: Some("Game".into()),
            file_id: "en1installer0".into(),
            entry_id: "en1installer0".into(),
            kind: "installer".into(),
            version: Some("1".into()),
            build_id: None,
            checksum_ts: Some(9),
            size: 1,
            path: "Game/a.exe".into(),
            installed_at: "2026-10-09T00:00:00Z".into(),
        });
        state.save().unwrap();
        assert_eq!(State::load().unwrap().installs.len(), 1);

        // Corrupt state → warn + default, never a hard failure.
        std::fs::write(State::path(), "{not json").unwrap();
        assert!(State::load().unwrap().installs.is_empty());

        unsafe { std::env::remove_var("GOGO_HOME") };
    }
}
