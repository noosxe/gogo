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
    let mut walk = Walk {
        mode: reflink,
        exact: false,
        on_file: &mut |_, _| Ok(()),
        report: MaterializeReport::default(),
        skipped: Vec::new(),
        xattrs_lost: 0,
    };
    // The root itself is followed (it may be a symlinked cache dir); below
    // it, nothing is.
    copy_dir(src, dst, &fs::metadata(src)?, &mut walk)?;
    Ok(walk.report)
}

/// What [`copy_exact`] did, beyond the usual report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExactCopy {
    pub report: MaterializeReport,
    /// Sockets and FIFOs: they mean nothing without the process that made
    /// them, and no process runs in a game being moved.
    pub skipped: Vec<PathBuf>,
    /// `user.*` extended attributes the destination wouldn't take.
    pub xattrs_lost: u64,
}

/// [`materialize_tree`] for a tree that is someone's only copy (a game being
/// moved, design 18 §6.1): `user.*` extended attributes come too (Wine keeps
/// security descriptors and DOS attributes there), and a socket or FIFO is
/// skipped rather than fatal. `on_file` hears each finished file and its size,
/// and an error from it stops the copy. A failure leaves the partial tree.
pub fn copy_exact(
    src: &Path,
    dst: &Path,
    on_file: &mut dyn FnMut(&Path, u64) -> io::Result<()>,
) -> io::Result<ExactCopy> {
    if dst.symlink_metadata().is_ok() {
        return Err(io::Error::other(format!(
            "destination {} already exists",
            dst.display()
        )));
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut walk = Walk {
        mode: Reflink::Try,
        exact: true,
        on_file,
        report: MaterializeReport::default(),
        skipped: Vec::new(),
        xattrs_lost: 0,
    };
    copy_dir(src, dst, &fs::metadata(src)?, &mut walk)?;
    Ok(ExactCopy {
        report: walk.report,
        skipped: walk.skipped,
        xattrs_lost: walk.xattrs_lost,
    })
}

/// One tree walk's settings and tallies.
struct Walk<'a> {
    mode: Reflink,
    /// [`copy_exact`] rather than [`materialize_tree`].
    exact: bool,
    on_file: &'a mut dyn FnMut(&Path, u64) -> io::Result<()>,
    report: MaterializeReport,
    skipped: Vec<PathBuf>,
    xattrs_lost: u64,
}

fn copy_dir(src: &Path, dst: &Path, meta: &fs::Metadata, walk: &mut Walk) -> io::Result<()> {
    fs::create_dir(dst)?;
    walk.report.dirs += 1;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let m = fs::symlink_metadata(&from)?;
        let kind = m.file_type();
        if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&from)?, &to)?;
            walk.report.symlinks += 1;
        } else if kind.is_dir() {
            copy_dir(&from, &to, &m, walk)?;
        } else if kind.is_file() {
            copy_file(&from, &to, &m, walk)?;
        } else if walk.exact {
            walk.skipped.push(from);
        } else {
            return Err(io::Error::other(format!(
                "{} is not a file, directory or symlink",
                from.display()
            )));
        }
    }
    if walk.exact {
        walk.xattrs_lost += copy_xattrs(src, dst)?;
    }
    // After the children: a read-only directory must still take them.
    File::open(dst)?.set_modified(meta.modified()?)?;
    fs::set_permissions(dst, meta.permissions())
}

fn copy_file(src: &Path, dst: &Path, meta: &fs::Metadata, walk: &mut Walk) -> io::Result<()> {
    let mut s = File::open(src)?;
    let mut d = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dst)?;
    if walk.mode == Reflink::Try && reflink(&s, &d).is_ok() {
        walk.report.reflinked_files += 1;
        walk.report.reflinked_bytes += meta.len();
    } else {
        // A failed FICLONE leaves `d` untouched (the ioctl is all or
        // nothing). `io::copy` uses copy_file_range, which may still share
        // extents (NFS server-side copy); the report counts it as a copy.
        io::copy(&mut s, &mut d)?;
        walk.report.copied_files += 1;
        walk.report.copied_bytes += meta.len();
    }
    if walk.exact {
        walk.xattrs_lost += copy_xattrs(src, dst)?;
    }
    d.set_modified(meta.modified()?)?;
    d.set_permissions(meta.permissions())?;
    (walk.on_file)(src, meta.len())
}

/// Is `dst` the same tree as `src`? The same names and kinds, the same link
/// targets, the same bytes in every file, and the same `user.*` attributes
/// unless `xattrs_lost` says some didn't fit. Sockets and FIFOs in `src` don't
/// count ([`copy_exact`] skips them). Modes and mtimes aren't compared: every
/// filesystem that can hold a prefix stores bytes alike, but not those. `on_file`
/// hears each compared file; the error names the first difference.
pub fn verify_exact(
    src: &Path,
    dst: &Path,
    xattrs_lost: bool,
    on_file: &mut dyn FnMut(&Path, u64) -> io::Result<()>,
) -> io::Result<()> {
    let differ = |p: &Path, what: &str| {
        io::Error::other(format!("{} differs in the copy: {what}", p.display()))
    };
    if !xattrs_lost && user_xattrs(src)? != user_xattrs(dst)? {
        return Err(differ(src, "extended attributes"));
    }
    let mut names = std::collections::BTreeSet::new();
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let m = fs::symlink_metadata(&from)?;
        let kind = m.file_type();
        if !(kind.is_symlink() || kind.is_dir() || kind.is_file()) {
            continue;
        }
        names.insert(entry.file_name());
        let Ok(d) = fs::symlink_metadata(&to) else {
            return Err(differ(&from, "missing"));
        };
        if d.file_type() != kind {
            return Err(differ(&from, "another kind of file"));
        }
        if kind.is_symlink() {
            if fs::read_link(&from)? != fs::read_link(&to)? {
                return Err(differ(&from, "another link target"));
            }
        } else if kind.is_dir() {
            verify_exact(&from, &to, xattrs_lost, on_file)?;
        } else {
            if m.len() != d.len() {
                return Err(differ(&from, "another size"));
            }
            if !same_bytes(&from, &to)? {
                return Err(differ(&from, "other bytes"));
            }
            if !xattrs_lost && user_xattrs(&from)? != user_xattrs(&to)? {
                return Err(differ(&from, "extended attributes"));
            }
            on_file(&from, m.len())?;
        }
    }
    for entry in fs::read_dir(dst)? {
        let name = entry?.file_name();
        if !names.contains(&name) {
            return Err(differ(&dst.join(name), "only in the copy"));
        }
    }
    Ok(())
}

fn same_bytes(a: &Path, b: &Path) -> io::Result<bool> {
    use std::io::Read as _;
    const CHUNK: usize = 1 << 20;
    let (mut a, mut b) = (File::open(a)?, File::open(b)?);
    // Read the copy from the disk, not from what the kernel remembers of writing
    // it: [`sync_filesystem`] came first, so these pages are clean. Best effort.
    #[cfg(target_os = "linux")]
    let _ = rustix::fs::fadvise(&b, 0, None, rustix::fs::Advice::DontNeed);
    let (mut x, mut y) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
    loop {
        let n = a.read(&mut x)?;
        if n == 0 {
            return Ok(b.read(&mut y)? == 0);
        }
        // `read` may return less than asked: compare what `a` gave.
        if b.read_exact(&mut y[..n]).is_err() || x[..n] != y[..n] {
            return Ok(false);
        }
    }
}

/// [`fs::remove_dir_all`] for a tree that may have read-only dirs in it, as a
/// game's or a copy of one may ([`copy_exact`] keeps modes). Never follows a
/// symlink: one at `path` is unlinked, its target left alone.
pub fn remove_tree(path: &Path) -> io::Result<()> {
    fn writable(dir: &Path) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        // chmod follows links: only ever touch a real dir.
        if !fs::symlink_metadata(dir)?.is_dir() {
            return Ok(());
        }
        let mode = fs::symlink_metadata(dir)?.permissions().mode();
        fs::set_permissions(dir, fs::Permissions::from_mode(mode | 0o700))?;
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                writable(&entry.path())?;
            }
        }
        Ok(())
    }
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return fs::remove_file(path);
    }
    match fs::remove_dir_all(path) {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            writable(path)?;
            fs::remove_dir_all(path)
        }
        other => other,
    }
}

/// Flush the filesystem `path` is on: what was written there survives a power
/// cut from here on.
pub fn sync_filesystem(path: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        rustix::fs::syncfs(File::open(path)?).map_err(io::Error::from)
    }
    #[cfg(not(target_os = "linux"))]
    {
        File::open(path)?.sync_all()
    }
}

/// `path`'s `user.*` extended attributes, sorted. None where the filesystem
/// has none.
#[cfg(target_os = "linux")]
fn user_xattrs(path: &Path) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let unsupported = |e: rustix::io::Errno| e == rustix::io::Errno::OPNOTSUPP;
    let size = match rustix::fs::llistxattr_query(path) {
        Ok(n) => n,
        Err(e) if unsupported(e) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut list = vec![0u8; size];
    let n = rustix::fs::llistxattr(path, &mut list[..])?;
    list.truncate(n);
    let mut out = Vec::new();
    for name in list.split(|b| *b == 0).filter(|n| n.starts_with(b"user.")) {
        let cname = std::ffi::CString::new(name).map_err(io::Error::other)?;
        let size = rustix::fs::lgetxattr_query(path, cname.as_c_str())?;
        let mut value = vec![0u8; size];
        let n = rustix::fs::lgetxattr(path, cname.as_c_str(), &mut value[..])?;
        value.truncate(n);
        out.push((name.to_vec(), value));
    }
    out.sort();
    Ok(out)
}

#[cfg(not(target_os = "linux"))]
fn user_xattrs(_path: &Path) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    Ok(Vec::new())
}

/// Copy `src`'s `user.*` attributes to `dst`; how many it wouldn't take.
#[cfg(target_os = "linux")]
fn copy_xattrs(src: &Path, dst: &Path) -> io::Result<u64> {
    let mut lost = 0;
    for (name, value) in user_xattrs(src)? {
        let cname = std::ffi::CString::new(name).map_err(io::Error::other)?;
        match rustix::fs::lsetxattr(
            dst,
            cname.as_c_str(),
            &value,
            rustix::fs::XattrFlags::empty(),
        ) {
            Ok(()) => {}
            Err(e) if e == rustix::io::Errno::OPNOTSUPP || e == rustix::io::Errno::PERM => {
                lost += 1;
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(lost)
}

#[cfg(not(target_os = "linux"))]
fn copy_xattrs(_src: &Path, _dst: &Path) -> io::Result<u64> {
    Ok(0)
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

pub(crate) fn existing_ancestor(path: &Path) -> Option<&Path> {
    path.ancestors().find(|p| p.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exact_pair(root: &Path) -> (PathBuf, PathBuf) {
        let (src, dst) = (root.join("src"), root.join("dst"));
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("a"), b"hello").unwrap();
        fs::write(src.join("sub/b"), vec![7u8; 3 << 20]).unwrap();
        std::os::unix::fs::symlink("/no/such/place", src.join("abs")).unwrap();
        std::os::unix::fs::symlink("../a", src.join("sub/rel")).unwrap();
        (src, dst)
    }

    fn check(src: &Path, dst: &Path) -> Result<(), String> {
        verify_exact(src, dst, false, &mut |_, _| Ok(())).map_err(|e| e.to_string())
    }

    #[test]
    fn an_exact_copy_verifies_and_every_difference_is_named() {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dst) = exact_pair(tmp.path());
        let mut files = 0;
        let out = copy_exact(&src, &dst, &mut |_, _| {
            files += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!((files, out.report.symlinks), (2, 2));
        check(&src, &dst).unwrap();
        assert_eq!(
            fs::read_link(dst.join("abs")).unwrap(),
            Path::new("/no/such/place")
        );

        // A byte at the very end, past the first chunk.
        let b = dst.join("sub/b");
        let mut bytes = fs::read(&b).unwrap();
        *bytes.last_mut().unwrap() = 8;
        fs::write(&b, &bytes).unwrap();
        assert!(check(&src, &dst).unwrap_err().contains("other bytes"));
        fs::write(&b, vec![7u8; 3 << 20]).unwrap();
        check(&src, &dst).unwrap();

        fs::write(&b, b"short").unwrap();
        assert!(check(&src, &dst).unwrap_err().contains("another size"));
        fs::write(&b, vec![7u8; 3 << 20]).unwrap();

        fs::remove_file(dst.join("sub/rel")).unwrap();
        assert!(check(&src, &dst).unwrap_err().contains("missing"));
        std::os::unix::fs::symlink("../elsewhere", dst.join("sub/rel")).unwrap();
        assert!(check(&src, &dst).unwrap_err().contains("link target"));
        fs::remove_file(dst.join("sub/rel")).unwrap();
        std::os::unix::fs::symlink("../a", dst.join("sub/rel")).unwrap();

        fs::write(dst.join("sub/extra"), "").unwrap();
        assert!(check(&src, &dst).unwrap_err().contains("only in the copy"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_exact_copy_skips_a_fifo_and_stops_when_told() {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dst) = exact_pair(tmp.path());
        let fifo = src.join("pipe");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            0,
        )
        .unwrap();
        let out = copy_exact(&src, &dst, &mut |_, _| Ok(())).unwrap();
        assert_eq!(out.skipped, [fifo]);
        check(&src, &dst).unwrap();

        let stopped = tmp.path().join("stopped");
        let err = copy_exact(&src, &stopped, &mut |_, _| {
            Err(io::ErrorKind::StorageFull.into())
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::StorageFull);
    }

    /// Where the test dir takes `user.*` attributes: Wine's stay on the copy, on
    /// a file and a dir, and a changed one is a difference.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_exact_copy_keeps_user_attributes() {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dst) = exact_pair(tmp.path());
        let name = c"user.wine.sd";
        let set =
            |p: &Path, v: &[u8]| rustix::fs::lsetxattr(p, name, v, rustix::fs::XattrFlags::empty());
        if set(&src.join("a"), b"sd").is_err() {
            eprintln!("SKIP no user attributes here");
            return;
        }
        set(&src.join("sub"), b"dir").unwrap();
        let out = copy_exact(&src, &dst, &mut |_, _| Ok(())).unwrap();
        assert_eq!(out.xattrs_lost, 0);
        check(&src, &dst).unwrap();
        assert_eq!(user_xattrs(&dst.join("sub")).unwrap().len(), 1);
        set(&dst.join("a"), b"other").unwrap();
        assert!(check(&src, &dst).unwrap_err().contains("attributes"));
    }

    #[test]
    fn remove_tree_takes_read_only_dirs_and_leaves_link_targets() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let (keep, tree) = (tmp.path().join("keep"), tmp.path().join("tree"));
        fs::create_dir_all(&keep).unwrap();
        fs::write(keep.join("f"), "").unwrap();
        let ro = tree.join("a/ro");
        fs::create_dir_all(&ro).unwrap();
        fs::write(ro.join("f"), "").unwrap();
        std::os::unix::fs::symlink(&keep, ro.join("out")).unwrap();
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(tree.join("a"), fs::Permissions::from_mode(0o555)).unwrap();
        remove_tree(&tree).unwrap();
        assert!(!tree.exists());
        assert!(keep.join("f").is_file());
    }

    #[test]
    fn remove_tree_on_a_link_leaves_a_read_only_target_alone() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let (target, link) = (tmp.path().join("target"), tmp.path().join("link"));
        fs::create_dir(&target).unwrap();
        fs::write(target.join("f"), "").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o555)).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let res = remove_tree(&link);
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        res.unwrap();
        assert!(link.symlink_metadata().is_err());
        assert_eq!(mode, 0o555);
        assert!(target.join("f").is_file());
    }

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
