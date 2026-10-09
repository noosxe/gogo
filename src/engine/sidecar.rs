//! `.part` + sidecar JSON: load/save, atomic (`docs/design/04` §5).
//!
//! Sidecars carry offsets/md5s only — **never CDN URLs** (re-resolved per run;
//! `docs/design/02` §5). `<dest>.part` + `<dest>.part.json` → atomic rename on
//! completion means "file with final name" ⇒ complete and verified.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sidecar {
    /// Schema version for forward-compatible migration.
    pub version: u32,
    /// Final file name from the authoritative checksum XML `name` attribute.
    pub dest_name: String,
    /// Authoritative XML total_size (can exceed the API's — live-verified).
    pub total_size: u64,
    /// Per-chunk md5 from the checksum XML, aligned with the chunk plan.
    pub chunk_md5s: Vec<String>,
    /// Which chunks are already on disk.
    pub completed: Vec<bool>,
}

/// Load `<dest>.part.json` if it exists and parses; corrupt/unmatching sidecars are
/// discarded (fresh download) — `docs/design/04` §8.
pub fn load(_dest_part: &std::path::Path) -> anyhow::Result<Option<Sidecar>> {
    todo!("docs/design/04 §5")
}

/// Atomic save (temp file + rename + fsync) after every chunk completion.
pub fn save(_dest_part: &std::path::Path, _sidecar: &Sidecar) -> anyhow::Result<()> {
    todo!("docs/design/04 §5")
}

/// Remove sidecar after a successful rename to the final name.
pub fn remove(_dest_part: &std::path::Path) -> anyhow::Result<()> {
    todo!("docs/design/04 §7")
}
