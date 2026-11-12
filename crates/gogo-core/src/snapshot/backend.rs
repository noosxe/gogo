//! The filesystem side of snapshots (design 20 §10.1): one trait, one
//! implementation per filesystem (`btrfs` now, `zfs` later) and a fake for
//! tests. Everything about metadata, numbering, retention and recovery lives
//! in `mod.rs` and is the same for every backend; a backend only makes,
//! lists, measures and removes the snapshots themselves.

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// The dir beside the game dirs that holds their snapshots (design 20 §2.1).
pub const SNAPSHOTS_DIR: &str = ".gogo-snapshots";

/// The marker a rollback writes into its staged tree, holding the target's
/// number (design 20 §5.3.1). In the game dir it means the swap happened and
/// the rollback isn't finished; the launcher refuses while it is there (§5.4).
pub const ROLLBACK_MARKER: &str = ".gogo-rollback";

/// The marker a conversion writes into its copy (design 20 §2.1, #406). In
/// the game dir it means the swap happened and the conversion isn't finished;
/// the launcher refuses while it is there (§5.4).
pub const CONVERTED_MARKER: &str = ".gogo-converted";

/// `<parent>/.gogo-rollback-<gogid>`: where a rollback stages the target's
/// writable copy, and where the old tree is after the commit (design 20
/// §5.3.1). `None` for a path without a parent or a name.
pub fn rollback_staging(game_dir: &Path) -> Option<PathBuf> {
    let parent = game_dir.parent()?;
    let name = game_dir.file_name()?;
    let mut staged = std::ffi::OsString::from(".gogo-rollback-");
    staged.push(name);
    Some(parent.join(staged))
}

/// What the filesystem under a dir offers (design 20 §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Support {
    /// btrfs: game dirs are subvolumes, snapshots read-only subvolumes beside them.
    Btrfs,
    /// A ZFS dataset (not implemented yet, design 20 §2.2): the dataset's name.
    Zfs { dataset: String },
    /// No snapshots here, with the reason in words (`<dir> is on ext4; …`).
    None(String),
}

impl Support {
    /// The backend this support names, for the `backend` field of a snapshot's
    /// metadata; `None` when there is none.
    pub fn kind(&self) -> Option<BackendKind> {
        match self {
            Support::Btrfs => Some(BackendKind::Btrfs),
            Support::Zfs { .. } => Some(BackendKind::Zfs),
            Support::None(_) => None,
        }
    }
}

/// The `backend` a snapshot was taken with (design 20 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
    Btrfs,
    Zfs,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BackendKind::Btrfs => "btrfs",
            BackendKind::Zfs => "zfs",
        }
    }
}

/// A snapshot the filesystem holds, whatever the metadata says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnDisk {
    pub n: u32,
    /// When the filesystem made it (btrfs: the subvolume's birth time), when it
    /// can tell. Shown for a snapshot with no metadata (`?`, design 20 §4).
    pub created: Option<SystemTime>,
}

/// What one snapshot costs: the bytes only it holds (design 20 §7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Space {
    pub n: u32,
    pub exclusive_bytes: u64,
    /// `false` for btrfs's FIEMAP estimate (shown with `~`), `true` for ZFS's
    /// `used`.
    pub exact: bool,
}

/// The points a rollback reports back to its orchestration (design 20
/// §5.3.1), so the journal, the saves, the override and the launcher stay
/// with the caller and are the same for every backend. An error from the
/// callback stops the rollback there, as a crash would: what follows is left
/// for recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollbackStep {
    /// A writable copy of the target is staged beside the game dir
    /// (`.gogo-rollback-<gogid>`), marker written.
    Staged,
    /// The commit: the staged tree and the game dir were swapped.
    Committed,
    /// The old tree (now at the staging name) is gone.
    OldTreeDeleted,
}

/// One filesystem's snapshot operations (design 20 §10.1).
///
/// Every operation names the game dir and a snapshot number; where a backend
/// keeps the snapshot (btrfs: `<parent>/.gogo-snapshots/<gogid>/<n>`) is its
/// own business. None of them takes a lock or checks a refusal (design 20
/// §5.1): the caller holds the locks, and [`super::Store`] makes that explicit
/// with its [`super::Guard`] argument.
pub trait SnapshotBackend: Send + Sync {
    /// What the filesystem under `dir` offers. `dir` need not exist yet (a
    /// game about to be built): its nearest existing ancestor is checked.
    fn probe(&self, dir: &Path) -> Support;

    /// The backend's name in metadata.
    fn kind(&self) -> BackendKind;

    /// Whether `game_dir` can be snapshotted as it is: its own subvolume
    /// (btrfs) or dataset (ZFS). A plain dir on a btrfs filesystem is
    /// [`Support::Btrfs`] but not ready: it needs converting first
    /// (design 20 §2.1). Not part of design 20's sketch of the trait; it keeps
    /// [`Support`] about the filesystem and this about the one dir.
    fn ready(&self, game_dir: &Path) -> io::Result<bool>;

    /// Create `dir` as something that can be snapshotted: a subvolume (btrfs),
    /// or check the prepared dataset (ZFS). `dir` must not exist yet (btrfs).
    fn make_game_dir(&self, dir: &Path) -> io::Result<()>;

    /// Take snapshot `n` of `game_dir`, read-only. Fails if `n` exists.
    fn snapshot(&self, game_dir: &Path, n: u32) -> io::Result<()>;

    /// A readable tree of snapshot `n` (design 20 §6.1's absent-in-snapshot
    /// test), or `None` where the backend can't give one.
    fn snapshot_path(&self, game_dir: &Path, n: u32) -> Option<PathBuf>;

    /// Whether snapshot `n` of `game_dir` is on disk. Not in design 20's
    /// sketch: recovery (§5.2) asks it about one snapshot, where
    /// [`Self::on_disk`] would list them all.
    fn exists(&self, game_dir: &Path, n: u32) -> io::Result<bool>;

    /// Remove snapshot `n`. Removing one that isn't there is `Ok`, so an
    /// interrupted delete can simply run again (design 20 §5.2).
    fn delete(&self, game_dir: &Path, n: u32) -> io::Result<()>;

    /// Roll `game_dir` back to snapshot `n` (design 20 §5.3.1), calling
    /// `step` after each step: a writable copy of `n` at
    /// [`rollback_staging`] with [`ROLLBACK_MARKER`] (holding `n`) in it
    /// ([`RollbackStep::Staged`]); the atomic swap of that copy and the game
    /// dir ([`RollbackStep::Committed`]: the caller finishes the restored game
    /// there and removes the marker); the old tree, now at the staging name,
    /// deleted ([`RollbackStep::OldTreeDeleted`]). Refuses when the staging
    /// name is taken: an earlier rollback's leftover is recovery's to settle.
    fn rollback(
        &self,
        game_dir: &Path,
        n: u32,
        step: &mut dyn FnMut(RollbackStep) -> io::Result<()>,
    ) -> io::Result<()> {
        let _ = (game_dir, n, step);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "rollback is not implemented yet",
        ))
    }

    /// Remove a tree a rollback left at `path` ([`rollback_staging`]): a
    /// staged copy, or the old tree after the commit. A subvolume is deleted
    /// as a snapshot is ([`Self::delete`]); gone already is `Ok`.
    fn discard(&self, path: &Path) -> io::Result<()> {
        match std::fs::symlink_metadata(path) {
            Ok(_) => crate::gog::materialize::remove_tree(path),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Every snapshot of `game_dir` on disk, by number. `ErrorKind::NotFound`
    /// when the game dir's parent isn't there (a drive not mounted): the
    /// snapshots are unreachable, not gone.
    fn on_disk(&self, game_dir: &Path) -> io::Result<Vec<OnDisk>>;

    /// What each of snapshots `ns` holds that neither the game dir nor any
    /// other snapshot in `ns` does (design 20 §7.3), in the order of `ns`.
    fn space(&self, game_dir: &Path, ns: &[u32]) -> io::Result<Vec<Space>>;
}

/// `<parent>/.gogo-snapshots/<gogid>`: where the snapshots of `game_dir`
/// live on btrfs and in the fake (design 20 §2.1). `None` for a path without a
/// parent or a name.
pub fn snapshots_dir(game_dir: &Path) -> Option<PathBuf> {
    let parent = game_dir.parent()?;
    let name = game_dir.file_name()?;
    Some(parent.join(SNAPSHOTS_DIR).join(name))
}

/// `<parent>/.gogo-snapshots/<gogid>/<n>`.
pub fn snapshot_dir(game_dir: &Path, n: u32) -> Option<PathBuf> {
    snapshots_dir(game_dir).map(|d| d.join(n.to_string()))
}

/// A snapshot number from a dir name: only the canonical spelling (`3`, never
/// `03` or `+3`), so one number is one name.
pub(crate) fn parse_n(name: &std::ffi::OsStr) -> Option<u32> {
    let s = name.to_str()?;
    let n: u32 = s.parse().ok()?;
    (n > 0 && n.to_string() == s).then_some(n)
}

/// The snapshots under [`snapshots_dir`], for backends that keep them as dirs
/// there.
#[cfg_attr(
    not(any(target_os = "linux", test, feature = "test-util")),
    allow(dead_code)
)]
pub(crate) fn list_dir(game_dir: &Path) -> io::Result<Vec<OnDisk>> {
    let parent = game_dir
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "game dir has no parent"))?;
    // The parent missing is "unreachable"; the snapshots dir missing is "none".
    std::fs::metadata(parent)?;
    let Some(dir) = snapshots_dir(game_dir) else {
        return Ok(Vec::new());
    };
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in rd {
        let entry = entry?;
        let Some(n) = parse_n(&entry.file_name()) else {
            continue;
        };
        let meta = entry.metadata()?;
        if !meta.is_dir() {
            continue;
        }
        let created = meta.created().or_else(|_| meta.modified()).ok();
        out.push(OnDisk { n, created });
    }
    out.sort_by_key(|o| o.n);
    Ok(out)
}

/// Write [`ROLLBACK_MARKER`] into `dir`, holding `n`, durably.
pub(crate) fn write_rollback_marker(dir: &Path, n: u32) -> io::Result<()> {
    use std::io::Write as _;
    let mut f = std::fs::File::create(dir.join(ROLLBACK_MARKER))?;
    writeln!(f, "{n}")?;
    f.sync_all()?;
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// The snapshot number [`ROLLBACK_MARKER`] in `dir` holds; `None` when there
/// is no marker (or it can't be read).
pub fn read_rollback_marker(dir: &Path) -> Option<u32> {
    std::fs::read_to_string(dir.join(ROLLBACK_MARKER))
        .ok()?
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn the_rollback_staging_dir_sits_beside_the_game_dir() {
        assert_eq!(
            rollback_staging(Path::new("/lib/games/1423049311")).unwrap(),
            Path::new("/lib/games/.gogo-rollback-1423049311")
        );
        assert_eq!(rollback_staging(Path::new("/")), None);
    }

    #[test]
    fn snapshot_paths_sit_beside_the_game_dir() {
        let g = Path::new("/lib/games/1423049311");
        assert_eq!(
            snapshots_dir(g).unwrap(),
            Path::new("/lib/games/.gogo-snapshots/1423049311")
        );
        assert_eq!(
            snapshot_dir(g, 3).unwrap(),
            Path::new("/lib/games/.gogo-snapshots/1423049311/3")
        );
        assert_eq!(snapshots_dir(Path::new("/")), None);
    }

    #[test]
    fn only_canonical_numbers_are_snapshots() {
        assert_eq!(parse_n(OsStr::new("3")), Some(3));
        assert_eq!(parse_n(OsStr::new("120")), Some(120));
        for bad in ["0", "03", "+3", "-1", "x", "3.tmp", ""] {
            assert_eq!(parse_n(OsStr::new(bad)), None, "{bad}");
        }
    }
}
