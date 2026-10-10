//! `.part` + two sidecars: plan JSON + done-bits bitmap (`docs/design/04` §5, review C6).
//!
//! `{dest}.part` holds the bytes; `{dest}.part.json` is the **plan** (metadata + chunk
//! table, written once at resolve, rewritten only on layout change) and
//! `{dest}.part.bitmap` is the **progress** (done-bits, ~1 bit per chunk, atomic
//! tmp+rename per completion — a 6k-chunk file updates ~750 B, not ~600 KB of JSON).
//! **No URLs in either** (tokens never touch disk; re-resolution is one cheap API call).
//!
//! Durability (review A3): a chunk's `sync_data` happens **before** the bitmap rename
//! records it done — a durable bitmap never claims non-durable bytes. Losing the bitmap
//! itself after a power cut costs one redone chunk, never correctness.

use serde::{Deserialize, Serialize};

use crate::gog::checksum::ChecksumDoc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanDoc {
    /// Schema version for forward-compatible migration.
    pub schema: u32,
    pub product_id: u64,
    #[serde(default)]
    pub dlc_id: Option<u64>,
    pub file_id: String,
    /// Final save name (authoritative checksum XML `name`).
    pub dest_name: String,
    /// Authoritative XML total_size (can exceed the API's — live-verified).
    pub total_size: u64,
    /// Chunk table: `from`/`to` inclusive, aligned with the bitmap bits.
    pub chunks: Vec<ChunkRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkRow {
    pub id: u32,
    pub from: u64,
    pub to: u64,
    pub md5: String,
}

pub fn part_path(dest: &std::path::Path) -> std::path::PathBuf {
    append(dest, "part")
}

pub fn plan_path(dest: &std::path::Path) -> std::path::PathBuf {
    append(dest, "part.json")
}

pub fn bitmap_path(dest: &std::path::Path) -> std::path::PathBuf {
    append(dest, "part.bitmap")
}

fn append(dest: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{suffix}"));
    dest.with_file_name(name)
}

impl PlanDoc {
    pub fn from_checksum(
        product_id: u64,
        dlc_id: Option<u64>,
        file_id: &str,
        doc: &ChecksumDoc,
    ) -> Self {
        Self {
            schema: 1,
            product_id,
            dlc_id,
            file_id: file_id.to_owned(),
            dest_name: doc.name.clone(),
            total_size: doc.total_size,
            chunks: doc
                .chunks
                .iter()
                .map(|c| ChunkRow {
                    id: c.id,
                    from: c.from,
                    to: c.to,
                    md5: c.md5.clone(),
                })
                .collect(),
        }
    }

    /// Resume validation (`docs/design/04` §3): total size, chunk count and spans must
    /// match the fresh `ChecksumDoc`; mismatch means GOG replaced the file.
    pub fn layout_matches(&self, doc: &ChecksumDoc) -> bool {
        if self.total_size != doc.total_size || self.chunks.len() != doc.chunks.len() {
            return false;
        }
        self.chunks
            .iter()
            .zip(&doc.chunks)
            .all(|(row, fresh)| row.id == fresh.id && row.from == fresh.from && row.to == fresh.to)
    }
}

/// Load the plan sidecar if it exists and parses; corrupt → `None` (fresh download,
/// `docs/design/04` §8).
pub fn load_plan(dest: &std::path::Path) -> Option<PlanDoc> {
    let raw = std::fs::read_to_string(plan_path(dest)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Atomic save (tmp + rename) — written once at resolve, or on layout change.
pub fn save_plan(dest: &std::path::Path, plan: &PlanDoc) -> std::io::Result<()> {
    let json = serde_json::to_string(plan).map_err(std::io::Error::other)?;
    atomic_write(&plan_path(dest), json.as_bytes())
}

/// Load the done-bits for `len` chunks; missing/truncated bitmap → all-zero (a lost
/// bitmap only costs redone chunks).
pub fn load_bitmap(dest: &std::path::Path, len: usize) -> Vec<u8> {
    let bytes = len.div_ceil(8);
    let mut bits = vec![0u8; bytes];
    if let Ok(raw) = std::fs::read(bitmap_path(dest)) {
        let n = raw.len().min(bytes);
        bits[..n].copy_from_slice(&raw[..n]);
    }
    bits
}

/// Atomic save (tmp + rename) after every chunk completion; called *after* the chunk's
/// `sync_data` (A3 ordering).
pub fn save_bitmap(dest: &std::path::Path, bits: &[u8]) -> std::io::Result<()> {
    atomic_write(&bitmap_path(dest), bits)
}

pub fn mark_done(bits: &mut [u8], index: u32) {
    let i = index as usize;
    bits[i / 8] |= 1 << (i % 8);
}

pub fn is_done(bits: &[u8], index: u32) -> bool {
    let i = index as usize;
    i / 8 < bits.len() && bits[i / 8] & (1 << (i % 8)) != 0
}

/// Remove both sidecars after the successful rename to the final name (04 §7).
pub fn remove(dest: &std::path::Path) -> std::io::Result<()> {
    let _ = std::fs::remove_file(plan_path(dest));
    let _ = std::fs::remove_file(bitmap_path(dest));
    Ok(())
}

fn atomic_write(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> ChecksumDoc {
        crate::gog::checksum::parse(
            r#"<file name="setup_g_(1).exe" total_size="10485760" chunks="2">
	<chunk id="0" from="0" to="5242879" method="md5">aa</chunk>
	<chunk id="1" from="5242880" to="10485759" method="md5">bb</chunk>
</file>"#,
        )
        .unwrap()
    }

    #[test]
    fn plan_round_trip_and_layout_check() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("setup_g_(1).exe");
        let plan = PlanDoc::from_checksum(7, None, "en1installer0", &doc());
        save_plan(&dest, &plan).unwrap();

        let loaded = load_plan(&dest).unwrap();
        assert_eq!(loaded.dest_name, "setup_g_(1).exe");
        assert_eq!(loaded.chunks.len(), 2);
        assert!(loaded.layout_matches(&doc()));

        let mut replaced = doc();
        replaced.total_size += 1;
        assert!(!plan.layout_matches(&replaced));
    }

    #[test]
    fn corrupt_plan_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("x.exe");
        std::fs::write(plan_path(&dest), "{broken").unwrap();
        assert!(load_plan(&dest).is_none());
    }

    #[test]
    fn bitmap_bits_round_trip_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("x.exe");

        let mut bits = load_bitmap(&dest, 10);
        mark_done(&mut bits, 0);
        mark_done(&mut bits, 9);
        save_bitmap(&dest, &bits).unwrap();

        let reloaded = load_bitmap(&dest, 10);
        assert!(is_done(&reloaded, 0));
        assert!(is_done(&reloaded, 9));
        assert!(!is_done(&reloaded, 5));

        remove(&dest).unwrap();
        let fresh = load_bitmap(&dest, 10);
        assert!(!is_done(&fresh, 0));
    }

    #[test]
    fn sidecar_paths_append_not_replace() {
        let dest = std::path::Path::new("/dl/Game/setup.exe");
        assert_eq!(
            part_path(dest),
            std::path::PathBuf::from("/dl/Game/setup.exe.part")
        );
        assert_eq!(
            plan_path(dest),
            std::path::PathBuf::from("/dl/Game/setup.exe.part.json")
        );
        assert_eq!(
            bitmap_path(dest),
            std::path::PathBuf::from("/dl/Game/setup.exe.part.bitmap")
        );
    }
}
