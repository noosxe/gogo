//! A [`SnapshotBackend`] for tests on any filesystem (design 20 §10.2):
//! snapshots are plain copies at the btrfs layout,
//! `<parent>/.gogo-snapshots/<gogid>/<n>`, so a test's temp dir holds them, and
//! [`Fake::fail_at`] makes one operation fail, part-way where a real one could
//! stop part-way.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::backend::{self, BackendKind, OnDisk, SnapshotBackend, Space, Support};
use crate::gog::materialize::{self, Reflink};

/// Where [`Fake`] fails, once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeStep {
    /// `snapshot` fails before copying anything (btrfs's snapshot is atomic:
    /// there is no half-made one).
    Snapshot,
    /// `delete` fails before removing anything.
    Delete,
    /// `delete` removes part of the tree, then fails, as btrfs's unprivileged
    /// file-by-file delete can.
    DeleteMidway,
}

/// Snapshots as copies. [`Support::Btrfs`] for every dir, so metadata says
/// `btrfs`.
#[derive(Debug, Default)]
pub struct Fake {
    fail: Mutex<Option<FakeStep>>,
}

impl Fake {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fail the next operation that reaches `step`, once.
    pub fn fail_at(&self, step: FakeStep) {
        *self.fail.lock().unwrap_or_else(|p| p.into_inner()) = Some(step);
    }

    fn take(&self, step: FakeStep) -> io::Result<()> {
        let mut fail = self.fail.lock().unwrap_or_else(|p| p.into_inner());
        if *fail == Some(step) {
            *fail = None;
            return Err(io::Error::other(format!("injected failure at {step:?}")));
        }
        Ok(())
    }

    fn dir(game_dir: &Path, n: u32) -> io::Result<PathBuf> {
        backend::snapshot_dir(game_dir, n)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent"))
    }
}

/// The regular files under `root`, relative, sorted. Symlinks (a prefix's
/// `dosdevices/`) hold no data of their own and aren't followed.
fn files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let ty = entry.file_type()?;
            if ty.is_dir() {
                stack.push(path);
            } else if ty.is_file()
                && let Ok(rel) = path.strip_prefix(root)
            {
                out.push(rel.to_path_buf());
            }
        }
    }
    out.sort();
    Ok(out)
}

impl SnapshotBackend for Fake {
    fn probe(&self, _dir: &Path) -> Support {
        Support::Btrfs
    }

    fn kind(&self) -> BackendKind {
        BackendKind::Btrfs
    }

    fn ready(&self, game_dir: &Path) -> io::Result<bool> {
        Ok(std::fs::metadata(game_dir)?.is_dir())
    }

    fn make_game_dir(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir(dir)
    }

    fn snapshot(&self, game_dir: &Path, n: u32) -> io::Result<()> {
        self.take(FakeStep::Snapshot)?;
        let dst = Self::dir(game_dir, n)?;
        if dst.exists() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        let parent = dst.parent().expect("snapshot dir has a parent");
        std::fs::create_dir_all(parent)?;
        // Copied under a temporary name and renamed: the copy appears whole or
        // not at all, as a btrfs snapshot does.
        let tmp = parent.join(format!(".{n}.copying"));
        if tmp.exists() {
            materialize::remove_tree(&tmp)?;
        }
        materialize::materialize_tree(game_dir, &tmp, Reflink::Never)?;
        std::fs::rename(&tmp, &dst)
    }

    fn snapshot_path(&self, game_dir: &Path, n: u32) -> Option<PathBuf> {
        backend::snapshot_dir(game_dir, n)
    }

    fn exists(&self, game_dir: &Path, n: u32) -> io::Result<bool> {
        Ok(Self::dir(game_dir, n)?.is_dir())
    }

    fn delete(&self, game_dir: &Path, n: u32) -> io::Result<()> {
        self.take(FakeStep::Delete)?;
        let dst = Self::dir(game_dir, n)?;
        if !dst.exists() {
            return Ok(());
        }
        if self.take(FakeStep::DeleteMidway).is_err() {
            let all = files(&dst)?;
            for f in all.iter().take(all.len().div_ceil(2)) {
                std::fs::remove_file(dst.join(f))?;
            }
            return Err(io::Error::other("injected failure at DeleteMidway"));
        }
        materialize::remove_tree(&dst)?;
        if let Some(dir) = dst.parent() {
            let _ = std::fs::remove_dir(dir);
        }
        Ok(())
    }

    fn on_disk(&self, game_dir: &Path) -> io::Result<Vec<OnDisk>> {
        backend::list_dir(game_dir)
    }

    /// The bytes of each snapshot's files that the game dir doesn't hold the
    /// same at the same path. Not btrfs's extent count, but it moves the same
    /// way: an unchanged game costs nothing, a changed or removed file costs
    /// its size.
    fn space(&self, game_dir: &Path, ns: &[u32]) -> io::Result<Vec<Space>> {
        let mut out = Vec::new();
        for &n in ns {
            let snap = Self::dir(game_dir, n)?;
            let mut bytes = 0;
            for rel in files(&snap)? {
                let ours = std::fs::read(snap.join(&rel))?;
                if std::fs::read(game_dir.join(&rel)).ok().as_deref() != Some(&ours[..]) {
                    bytes += ours.len() as u64;
                }
            }
            out.push(Space {
                n,
                exclusive_bytes: bytes,
                exact: false,
            });
        }
        Ok(out)
    }
}
