//! A [`SnapshotBackend`] for tests on any filesystem (design 20 §10.2):
//! snapshots are plain copies at the btrfs layout,
//! `<parent>/.gogo-snapshots/<gogid>/<n>`, so a test's temp dir holds them, and
//! [`Fake::fail_at`] makes one operation fail, part-way where a real one could
//! stop part-way.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::backend::{self, BackendKind, OnDisk, RollbackStep, SnapshotBackend, Space, Support};
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
    /// `rollback` fails before staging anything.
    RollbackStage,
    /// `rollback` stages part of the copy, without the marker, then fails: a
    /// btrfs snapshot is atomic, but a crash can come between it and the
    /// marker.
    RollbackStageMidway,
    /// `rollback` fails after staging, before the swap.
    RollbackExchange,
    /// `rollback` removes part of the old tree after the commit, then fails.
    RollbackDiscardMidway,
}

/// Snapshots as copies. [`Support::Btrfs`] for every dir, so metadata says
/// `btrfs`.
#[derive(Debug, Default)]
pub struct Fake {
    fail: Mutex<Option<FakeStep>>,
    /// [`Fake::subvolumes_only`]: the `(dev, ino)` of every dir
    /// `make_game_dir` made, which stay with the dir through a rename or an
    /// exchange as a subvolume does.
    subvolumes: Option<Mutex<std::collections::HashSet<(u64, u64)>>>,
}

impl Fake {
    pub fn new() -> Self {
        Self::default()
    }

    /// A fake where only the dirs it made are [`SnapshotBackend::ready`], as
    /// on btrfs a plain dir isn't (design 20 §2.1's conversion).
    pub fn subvolumes_only() -> Self {
        Self {
            subvolumes: Some(Mutex::default()),
            ..Self::default()
        }
    }

    fn id(dir: &Path) -> io::Result<(u64, u64)> {
        use std::os::unix::fs::MetadataExt as _;
        let m = std::fs::metadata(dir)?;
        Ok((m.dev(), m.ino()))
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
        let is_dir = std::fs::metadata(game_dir)?.is_dir();
        Ok(match &self.subvolumes {
            Some(made) => {
                is_dir
                    && made
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .contains(&Self::id(game_dir)?)
            }
            None => is_dir,
        })
    }

    fn make_game_dir(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir(dir)?;
        if let Some(made) = &self.subvolumes {
            made.lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(Self::id(dir)?);
        }
        Ok(())
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

    /// The swap is three renames (the game dir aside, the copy in, the old
    /// tree to the staging name): not atomic, as btrfs's `RENAME_EXCHANGE`
    /// is, but tests never stop inside it.
    fn rollback(
        &self,
        game_dir: &Path,
        n: u32,
        step: &mut dyn FnMut(RollbackStep) -> io::Result<()>,
    ) -> io::Result<()> {
        let no_parent = || io::Error::new(io::ErrorKind::InvalidInput, "no parent");
        let src = Self::dir(game_dir, n)?;
        let staged = backend::rollback_staging(game_dir).ok_or_else(no_parent)?;
        if staged.symlink_metadata().is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "{} is there: an earlier rollback isn't settled",
                    staged.display()
                ),
            ));
        }
        if !src.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} isn't there", src.display()),
            ));
        }
        self.take(FakeStep::RollbackStage)?;
        materialize::materialize_tree(&src, &staged, Reflink::Never)?;
        if self.take(FakeStep::RollbackStageMidway).is_err() {
            let all = files(&staged)?;
            for f in all.iter().take(all.len() / 2) {
                std::fs::remove_file(staged.join(f))?;
            }
            return Err(io::Error::other("injected failure at RollbackStageMidway"));
        }
        backend::write_rollback_marker(&staged, n)?;
        step(RollbackStep::Staged)?;
        self.take(FakeStep::RollbackExchange)?;
        let aside = staged.with_extension("swap");
        std::fs::rename(game_dir, &aside)?;
        std::fs::rename(&staged, game_dir)?;
        std::fs::rename(&aside, &staged)?;
        step(RollbackStep::Committed)?;
        if self.take(FakeStep::RollbackDiscardMidway).is_err() {
            let all = files(&staged)?;
            for f in all.iter().take(all.len().div_ceil(2)) {
                std::fs::remove_file(staged.join(f))?;
            }
            return Err(io::Error::other(
                "injected failure at RollbackDiscardMidway",
            ));
        }
        self.discard(&staged)?;
        step(RollbackStep::OldTreeDeleted)
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
