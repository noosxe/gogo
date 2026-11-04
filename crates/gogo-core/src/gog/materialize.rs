//! Depot tree materialization on Linux (design 18 §6): the depot cache tree
//! becomes `games/<gogid>/game` by `FICLONE` (reflink) where the filesystem
//! supports it (btrfs, XFS, bcachefs), else by a plain copy.
//!
//! macOS keeps `macapp::bundle::clone_tree` (APFS clonefile through `cp -c`);
//! nothing here runs on the Mac path. The walk is portable unix code, so its
//! copy path is tested on both OSes; only the reflink call is Linux-only.
//!
//! Per file, not per tree: every regular file tries `FICLONE` first and falls
//! back to a copy on its own. The destination is one fresh directory, but the
//! source tree may cross a mount, and a failed ioctl costs microseconds. The
//! plan ([`plan_tree`]) probes once per tree, because before the download
//! there is no tree to walk; the report ([`MaterializeReport`]) says what
//! actually happened.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// How the game files reach `games/<gogid>/game`.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TreeMethod {
    /// Copy-on-write clones: no extra disk until a file diverges.
    Reflink,
    /// Byte copies: the tree's size again on disk.
    Copy,
}

impl TreeMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            TreeMethod::Reflink => "reflink",
            TreeMethod::Copy => "copy",
        }
    }
}

/// What materializing a tree will cost, shown in the plan.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MaterializePlan {
    pub method: TreeMethod,
    /// Where the tree goes (`games/<gogid>/game`).
    pub game_dir: PathBuf,
    /// Disk the materialized tree adds: 0 for a reflink, the tree size for
    /// a copy.
    pub extra_bytes: u64,
}

/// Whether to try `FICLONE` at all; [`Reflink::Never`] forces the copy path
/// (tests, and filesystems known to lie).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reflink {
    Try,
    Never,
}

/// What [`materialize_tree`] did.
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MaterializeReport {
    pub reflinked_files: u64,
    pub reflinked_bytes: u64,
    pub copied_files: u64,
    /// Bytes written as copies: the extra disk the tree costs.
    pub copied_bytes: u64,
    pub symlinks: u64,
    pub dirs: u64,
}

impl MaterializeReport {
    /// `Copy` as soon as one file was copied: the tree then costs disk.
    pub fn method(&self) -> TreeMethod {
        if self.copied_files > 0 {
            TreeMethod::Copy
        } else {
            TreeMethod::Reflink
        }
    }
}

/// Probe whether files under `src` can be reflinked into `dst`: one small
/// file is written in the nearest existing ancestor of each, cloned, and both
/// are removed. Anything that goes wrong reads as "no reflink" (a plan may
/// overstate the cost, never understate it).
pub fn probe_reflink(src: &Path, dst: &Path) -> bool {
    let (Some(src_dir), Some(dst_dir)) = (existing_ancestor(src), existing_ancestor(dst)) else {
        return false;
    };
    let tag = format!(".gogo-reflink-probe-{}", std::process::id());
    let src_file = src_dir.join(format!("{tag}-src"));
    let dst_file = dst_dir.join(format!("{tag}-dst"));
    let ok = (|| -> io::Result<()> {
        // A full block: some filesystems keep tiny files inline, which
        // clones differently from real extents.
        fs::write(&src_file, [0u8; 4096])?;
        let s = File::open(&src_file)?;
        let d = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dst_file)?;
        reflink(&s, &d)
    })()
    .is_ok();
    let _ = fs::remove_file(&src_file);
    let _ = fs::remove_file(&dst_file);
    ok
}

/// The plan for materializing a tree of `tree_bytes` from `src` into
/// `game_dir`: reflink when the probe succeeds, else a copy costing
/// `tree_bytes`.
pub fn plan_tree(src: &Path, game_dir: &Path, tree_bytes: u64) -> MaterializePlan {
    let method = if probe_reflink(src, game_dir) {
        TreeMethod::Reflink
    } else {
        TreeMethod::Copy
    };
    MaterializePlan {
        method,
        game_dir: game_dir.to_path_buf(),
        extra_bytes: match method {
            TreeMethod::Reflink => 0,
            TreeMethod::Copy => tree_bytes,
        },
    }
}

/// Materialize `src` (a directory) as `dst`, which must not exist yet;
/// parents are created. Modes and mtimes are kept and symlinks stay symlinks,
/// as `cp -Rp` does on the Mac path. A failure leaves the partial tree for
/// diagnosis, like `clone_tree`.
pub fn materialize_tree(src: &Path, dst: &Path, reflink: Reflink) -> io::Result<MaterializeReport> {
    if !src.is_dir() {
        return Err(io::Error::other(format!(
            "game tree {} is not a directory",
            src.display()
        )));
    }
    if dst.symlink_metadata().is_ok() {
        return Err(io::Error::other(format!(
            "destination {} already exists",
            dst.display()
        )));
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut report = MaterializeReport::default();
    // The root itself is followed (it may be a symlinked cache dir); below
    // it, nothing is.
    copy_dir(src, dst, &fs::metadata(src)?, reflink, &mut report)?;
    Ok(report)
}

fn copy_dir(
    src: &Path,
    dst: &Path,
    meta: &fs::Metadata,
    mode: Reflink,
    report: &mut MaterializeReport,
) -> io::Result<()> {
    fs::create_dir(dst)?;
    report.dirs += 1;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let m = fs::symlink_metadata(&from)?;
        let kind = m.file_type();
        if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&from)?, &to)?;
            report.symlinks += 1;
        } else if kind.is_dir() {
            copy_dir(&from, &to, &m, mode, report)?;
        } else if kind.is_file() {
            copy_file(&from, &to, &m, mode, report)?;
        } else {
            return Err(io::Error::other(format!(
                "{} is not a file, directory or symlink",
                from.display()
            )));
        }
    }
    // After the children: a read-only directory must still take them.
    File::open(dst)?.set_modified(meta.modified()?)?;
    fs::set_permissions(dst, meta.permissions())
}

fn copy_file(
    src: &Path,
    dst: &Path,
    meta: &fs::Metadata,
    mode: Reflink,
    report: &mut MaterializeReport,
) -> io::Result<()> {
    let mut s = File::open(src)?;
    let mut d = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dst)?;
    if mode == Reflink::Try && reflink(&s, &d).is_ok() {
        report.reflinked_files += 1;
        report.reflinked_bytes += meta.len();
    } else {
        // A failed FICLONE leaves `d` untouched (the ioctl is all or
        // nothing). `io::copy` uses copy_file_range, which may still share
        // extents (NFS server-side copy); the report counts it as a copy.
        io::copy(&mut s, &mut d)?;
        report.copied_files += 1;
        report.copied_bytes += meta.len();
    }
    d.set_modified(meta.modified()?)?;
    d.set_permissions(meta.permissions())
}

/// `FICLONE`: make `dst` share `src`'s extents.
#[cfg(target_os = "linux")]
fn reflink(src: &File, dst: &File) -> io::Result<()> {
    rustix::fs::ioctl_ficlone(dst, src).map_err(io::Error::from)
}

#[cfg(not(target_os = "linux"))]
fn reflink(_src: &File, _dst: &File) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

fn existing_ancestor(path: &Path) -> Option<&Path> {
    path.ancestors().find(|p| p.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_an_existing_destination_and_a_missing_source() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir(&src).unwrap();
        let dst = tmp.path().join("dst");
        fs::create_dir(&dst).unwrap();
        assert!(materialize_tree(&src, &dst, Reflink::Never).is_err());
        assert!(
            materialize_tree(
                &tmp.path().join("nope"),
                &tmp.path().join("x"),
                Reflink::Never
            )
            .is_err()
        );
        // A dangling symlink counts as existing, too.
        std::os::unix::fs::symlink("nowhere", tmp.path().join("dangling")).unwrap();
        assert!(materialize_tree(&src, &tmp.path().join("dangling"), Reflink::Never).is_err());
    }

    #[test]
    fn the_plan_follows_the_probe() {
        let tmp = tempfile::tempdir().unwrap();
        let game = tmp.path().join("games/1/game");
        let plan = plan_tree(tmp.path(), &game, 1234);
        assert_eq!(plan.game_dir, game);
        match plan.method {
            TreeMethod::Reflink => assert_eq!(plan.extra_bytes, 0),
            TreeMethod::Copy => assert_eq!(plan.extra_bytes, 1234),
        }
        // The probe leaves nothing behind and creates no directories.
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[test]
    fn plan_json_shape() {
        let plan = MaterializePlan {
            method: TreeMethod::Copy,
            game_dir: "/g".into(),
            extra_bytes: 5,
        };
        assert_eq!(
            serde_json::to_string(&plan).unwrap(),
            r#"{"method":"copy","gameDir":"/g","extraBytes":5}"#
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn no_reflink_off_linux() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!probe_reflink(tmp.path(), tmp.path()));
    }
}
