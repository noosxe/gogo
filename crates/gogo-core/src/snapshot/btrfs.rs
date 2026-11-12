//! btrfs snapshots through the kernel's ioctls (design 20 §2.1, §10.1), on
//! `rustix`, unprivileged. No `btrfs-progs`, no library: five ioctls with
//! fixed structs, written from the kernel's UAPI headers.
//!
//! Sources, both from the kernel tree (`include/uapi/linux/`):
//! - `btrfs.h`: `BTRFS_IOCTL_MAGIC` (0x94), `struct btrfs_ioctl_vol_args`,
//!   `struct btrfs_ioctl_vol_args_v2`, `BTRFS_SUBVOL_RDONLY`, and the
//!   `BTRFS_IOC_SUBVOL_CREATE` (14), `BTRFS_IOC_SNAP_CREATE_V2` (23),
//!   `BTRFS_IOC_SUBVOL_GETFLAGS` (25), `BTRFS_IOC_SUBVOL_SETFLAGS` (26) and
//!   `BTRFS_IOC_SNAP_DESTROY_V2` (63) numbers;
//! - `fiemap.h`: `struct fiemap`, `struct fiemap_extent`, the `FIEMAP_*`
//!   flags, and `FS_IOC_FIEMAP` (`fs.h`: `_IOWR('f', 11, struct fiemap)`);
//! - `magic.h`: `BTRFS_SUPER_MAGIC` and the other `f_type` values named below.
//!
//! Every `unsafe` of the snapshot module is in this file.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;
use rustix::ioctl::{Getter, Opcode, Setter, Updater, ioctl, opcode};

use super::backend::{self, BackendKind, OnDisk, SnapshotBackend, Space, Support};

// — magic numbers (`include/uapi/linux/magic.h`) —

/// `BTRFS_SUPER_MAGIC`.
pub const BTRFS_SUPER_MAGIC: u32 = 0x9123_683E;
const EXT4_SUPER_MAGIC: u32 = 0xEF53;
const XFS_SUPER_MAGIC: u32 = 0x5846_5342;
const TMPFS_MAGIC: u32 = 0x0102_1994;
const NFS_SUPER_MAGIC: u32 = 0x6969;
const FUSE_SUPER_MAGIC: u32 = 0x6573_5546;
const OVERLAYFS_SUPER_MAGIC: u32 = 0x794C_7630;
/// ZFS's `ZFS_SUPER_MAGIC` (OpenZFS's `zfs_vfsops`, not in the kernel tree).
const ZFS_SUPER_MAGIC: u32 = 0x2FC1_2FC1;
const BCACHEFS_SUPER_MAGIC: u32 = 0xCA45_1A4E;

/// A subvolume's root dir always has this inode number
/// (<https://btrfs.readthedocs.io/en/latest/Subvolumes.html>).
pub const SUBVOLUME_ROOT_INO: u64 = 256;

// — `include/uapi/linux/btrfs.h` —

const BTRFS_IOCTL_MAGIC: u8 = 0x94;
/// `BTRFS_PATH_NAME_MAX`.
const PATH_NAME_MAX: usize = 4087;
/// `BTRFS_SUBVOL_NAME_MAX`.
const SUBVOL_NAME_MAX: usize = 4039;
/// `BTRFS_SUBVOL_RDONLY`: in `vol_args_v2.flags` and the subvolume flags.
const SUBVOL_RDONLY: u64 = 1 << 1;

/// `struct btrfs_ioctl_vol_args { __s64 fd; char name[BTRFS_PATH_NAME_MAX + 1]; }`.
#[repr(C)]
struct VolArgs {
    fd: i64,
    name: [u8; PATH_NAME_MAX + 1],
}

/// `struct btrfs_ioctl_vol_args_v2`: `__s64 fd; __u64 transid; __u64 flags;`
/// then a union of `{ __u64 size; struct btrfs_qgroup_inherit *qgroup_inherit; }`
/// and `__u64 unused[4]` (32 bytes, unused here), then a union of
/// `char name[BTRFS_SUBVOL_NAME_MAX + 1]`, `__u64 devid` and `__u64 subvolid`.
#[repr(C)]
struct VolArgsV2 {
    fd: i64,
    transid: u64,
    flags: u64,
    unused: [u64; 4],
    name: [u8; SUBVOL_NAME_MAX + 1],
}

/// `BTRFS_IOC_SUBVOL_CREATE`: `_IOW(BTRFS_IOCTL_MAGIC, 14, struct btrfs_ioctl_vol_args)`.
const IOC_SUBVOL_CREATE: Opcode = opcode::write::<VolArgs>(BTRFS_IOCTL_MAGIC, 14);
/// `BTRFS_IOC_SNAP_CREATE_V2`: `_IOW(BTRFS_IOCTL_MAGIC, 23, struct btrfs_ioctl_vol_args_v2)`.
const IOC_SNAP_CREATE_V2: Opcode = opcode::write::<VolArgsV2>(BTRFS_IOCTL_MAGIC, 23);
/// `BTRFS_IOC_SUBVOL_GETFLAGS`: `_IOR(BTRFS_IOCTL_MAGIC, 25, __u64)`.
const IOC_SUBVOL_GETFLAGS: Opcode = opcode::read::<u64>(BTRFS_IOCTL_MAGIC, 25);
/// `BTRFS_IOC_SUBVOL_SETFLAGS`: `_IOW(BTRFS_IOCTL_MAGIC, 26, __u64)`.
const IOC_SUBVOL_SETFLAGS: Opcode = opcode::write::<u64>(BTRFS_IOCTL_MAGIC, 26);
/// `BTRFS_IOC_SNAP_DESTROY_V2`: `_IOW(BTRFS_IOCTL_MAGIC, 63, struct btrfs_ioctl_vol_args_v2)`.
const IOC_SNAP_DESTROY_V2: Opcode = opcode::write::<VolArgsV2>(BTRFS_IOCTL_MAGIC, 63);

// — `include/uapi/linux/fiemap.h` —

/// `struct fiemap_extent`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FiemapExtent {
    fe_logical: u64,
    fe_physical: u64,
    fe_length: u64,
    fe_reserved64: [u64; 2],
    fe_flags: u32,
    fe_reserved: [u32; 3],
}

/// `struct fiemap`, without its flexible `fm_extents[]` tail.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Fiemap {
    fm_start: u64,
    fm_length: u64,
    fm_flags: u32,
    fm_mapped_extents: u32,
    fm_extent_count: u32,
    fm_reserved: u32,
}

/// Extents asked for per call.
const FIEMAP_BATCH: usize = 64;

/// [`Fiemap`] with room for [`FIEMAP_BATCH`] extents after it, laid out as the
/// kernel expects (`fm_extents` directly follows the 32-byte header).
#[repr(C)]
struct FiemapBuf {
    head: Fiemap,
    extents: [FiemapExtent; FIEMAP_BATCH],
}

/// `FS_IOC_FIEMAP`: `_IOWR('f', 11, struct fiemap)`; the size in the number is
/// the header's, whatever follows it.
const FS_IOC_FIEMAP: Opcode = opcode::read_write::<Fiemap>(b'f', 11);
/// `FIEMAP_MAX_OFFSET`.
const FIEMAP_MAX_OFFSET: u64 = !0;
/// `FIEMAP_FLAG_SYNC`: flush the file first, so fresh writes have extents.
const FIEMAP_FLAG_SYNC: u32 = 0x1;
const FIEMAP_EXTENT_LAST: u32 = 0x1;
const FIEMAP_EXTENT_UNKNOWN: u32 = 0x2;
const FIEMAP_EXTENT_DELALLOC: u32 = 0x4;
/// `FIEMAP_EXTENT_DATA_INLINE`: data in the metadata, no physical address.
const FIEMAP_EXTENT_DATA_INLINE: u32 = 0x200;

// — detection —

/// The `f_type` of the filesystem `path` is on.
pub fn fs_type(path: &Path) -> io::Result<u32> {
    let st = rustix::fs::statfs(path)?;
    // f_type is a signed or unsigned word depending on the arch; the magics
    // are 32-bit, so keep the low 32 bits.
    #[allow(clippy::unnecessary_cast)]
    Ok(st.f_type as u32)
}

/// Whether `path` is on btrfs.
pub fn is_btrfs(path: &Path) -> io::Result<bool> {
    Ok(fs_type(path)? == BTRFS_SUPER_MAGIC)
}

/// Whether `path` is the root of a btrfs subvolume: a directory on btrfs with
/// inode number 256. Never follows a symlink.
pub fn is_subvolume(path: &Path) -> io::Result<bool> {
    let meta = std::fs::symlink_metadata(path)?;
    Ok(meta.is_dir() && meta.ino() == SUBVOLUME_ROOT_INO && is_btrfs(path)?)
}

/// A filesystem's name for the `none` reason, from its `f_type`.
fn fs_name(magic: u32) -> Option<&'static str> {
    Some(match magic {
        EXT4_SUPER_MAGIC => "ext4",
        XFS_SUPER_MAGIC => "XFS",
        TMPFS_MAGIC => "tmpfs",
        NFS_SUPER_MAGIC => "NFS",
        FUSE_SUPER_MAGIC => "a FUSE mount",
        OVERLAYFS_SUPER_MAGIC => "overlayfs",
        ZFS_SUPER_MAGIC => "ZFS",
        BCACHEFS_SUPER_MAGIC => "bcachefs",
        BTRFS_SUPER_MAGIC => "btrfs",
        _ => return None,
    })
}

/// What `dir` (or, when it doesn't exist yet, its nearest existing ancestor)
/// offers (design 20 §2).
pub fn probe(dir: &Path) -> Support {
    let Some(at) = crate::gog::materialize::existing_ancestor(dir) else {
        return Support::None(format!("{} doesn't exist", dir.display()));
    };
    let magic = match fs_type(at) {
        Ok(m) => m,
        Err(e) => return Support::None(format!("can't check {}: {e}", at.display())),
    };
    match magic {
        BTRFS_SUPER_MAGIC => Support::Btrfs,
        ZFS_SUPER_MAGIC => Support::None("ZFS: not supported yet".into()),
        BCACHEFS_SUPER_MAGIC => Support::None("bcachefs: not supported yet".into()),
        other => {
            let fs = fs_name(other)
                .map_or_else(|| format!("a filesystem of type {other:#x}"), String::from);
            Support::None(format!(
                "{} is on {fs}; snapshots need btrfs, or a ZFS dataset per game",
                dir.display()
            ))
        }
    }
}

// — the ioctls —

fn open_dir(path: &Path) -> io::Result<rustix::fd::OwnedFd> {
    Ok(rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}

/// `path`'s parent (opened) and its last component, checked to fit `max`
/// bytes plus the NUL.
fn parent_and_name(path: &Path, max: usize) -> io::Result<(rustix::fd::OwnedFd, &OsStr)> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no name"))?;
    let bytes = name.as_bytes();
    if bytes.len() > max || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{}: not a usable subvolume name", path.display()),
        ));
    }
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    Ok((open_dir(parent)?, name))
}

fn copy_name(dst: &mut [u8], name: &OsStr) {
    let b = name.as_bytes();
    dst[..b.len()].copy_from_slice(b);
}

/// Create `path` as a new, empty subvolume (`BTRFS_IOC_SUBVOL_CREATE` on its
/// parent). Unprivileged; `EEXIST` when `path` exists.
pub fn create_subvolume(path: &Path) -> io::Result<()> {
    let (parent, name) = parent_and_name(path, PATH_NAME_MAX)?;
    let mut args = VolArgs {
        fd: 0,
        name: [0; PATH_NAME_MAX + 1],
    };
    copy_name(&mut args.name, name);
    // SAFETY: IOC_SUBVOL_CREATE is `_IOW(0x94, 14, struct btrfs_ioctl_vol_args)`
    // and `VolArgs` is that struct's exact `#[repr(C)]` layout (checked by the
    // layout tests below); the kernel only reads it. The name is NUL-terminated:
    // it is at most PATH_NAME_MAX bytes in a zeroed array one longer.
    unsafe { ioctl(&parent, Setter::<IOC_SUBVOL_CREATE, VolArgs>::new(args)) }?;
    Ok(())
}

/// Snapshot subvolume `src` as `dst` (`BTRFS_IOC_SNAP_CREATE_V2` on `dst`'s
/// parent), read-only when `readonly`. The caller must own `src`; both must be
/// on one filesystem (`EXDEV` otherwise).
pub fn snapshot(src: &Path, dst: &Path, readonly: bool) -> io::Result<()> {
    let source = open_dir(src)?;
    let (parent, name) = parent_and_name(dst, SUBVOL_NAME_MAX)?;
    let mut args = VolArgsV2 {
        fd: i64::from(rustix::fd::AsRawFd::as_raw_fd(&source)),
        transid: 0,
        flags: if readonly { SUBVOL_RDONLY } else { 0 },
        unused: [0; 4],
        name: [0; SUBVOL_NAME_MAX + 1],
    };
    copy_name(&mut args.name, name);
    // SAFETY: IOC_SNAP_CREATE_V2 is `_IOW(0x94, 23, struct
    // btrfs_ioctl_vol_args_v2)` and `VolArgsV2` is its exact layout (layout
    // tests below); the kernel only reads it. `fd` is `source`, open until the
    // call returns; the qgroup-inherit union is zeroed (no inherit, as the
    // flags don't ask for one); the name is NUL-terminated in a zeroed array.
    unsafe { ioctl(&parent, Setter::<IOC_SNAP_CREATE_V2, VolArgsV2>::new(args)) }?;
    drop(source);
    Ok(())
}

fn get_flags(fd: impl AsFd) -> io::Result<u64> {
    // SAFETY: IOC_SUBVOL_GETFLAGS is `_IOR(0x94, 25, __u64)`: the kernel writes
    // one u64, which is the getter's output type.
    Ok(unsafe { ioctl(fd, Getter::<IOC_SUBVOL_GETFLAGS, u64>::new()) }?)
}

/// Whether subvolume `path` is read-only (`BTRFS_IOC_SUBVOL_GETFLAGS`).
pub fn is_readonly(path: &Path) -> io::Result<bool> {
    Ok(get_flags(open_dir(path)?)? & SUBVOL_RDONLY != 0)
}

/// Make subvolume `path` read-only or writable (`BTRFS_IOC_SUBVOL_SETFLAGS`).
/// Unprivileged for the subvolume's owner; `EPERM` while a `send` uses it.
pub fn set_readonly(path: &Path, readonly: bool) -> io::Result<()> {
    let fd = open_dir(path)?;
    let flags = get_flags(&fd)?;
    let want = if readonly {
        flags | SUBVOL_RDONLY
    } else {
        flags & !SUBVOL_RDONLY
    };
    if want == flags {
        return Ok(());
    }
    // SAFETY: IOC_SUBVOL_SETFLAGS is `_IOW(0x94, 26, __u64)`: the kernel reads
    // one u64, the setter's input type.
    unsafe { ioctl(&fd, Setter::<IOC_SUBVOL_SETFLAGS, u64>::new(want)) }?;
    Ok(())
}

/// How [`destroy`] removed a subvolume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destroyed {
    /// `BTRFS_IOC_SNAP_DESTROY_V2`: the mount has `user_subvol_rm_allowed`, or
    /// the caller has `CAP_SYS_ADMIN`.
    Ioctl,
    /// Read-only cleared, the tree removed file by file, then `rmdir(2)`.
    Rmdir,
}

/// Remove subvolume `path` (design 20 §2.1): `BTRFS_IOC_SNAP_DESTROY_V2`
/// where the mount allows it, else, on `EPERM` (or `ENOTTY` from a kernel
/// before 5.7, which lacks the v2 ioctl), [`destroy_by_rmdir`].
pub fn destroy(path: &Path) -> io::Result<Destroyed> {
    // With `user_subvol_rm_allowed` the kernel checks write access to the
    // subvolume, so a read-only snapshot is refused with `EROFS` (seen on a
    // loop mount in CI): clear the flag first, as the rmdir route does.
    if is_subvolume(path)? {
        set_readonly(path, false)?;
    }
    match destroy_by_ioctl(path) {
        Ok(()) => Ok(Destroyed::Ioctl),
        Err(e) if e == Errno::PERM || e == Errno::NOTTY => {
            destroy_by_rmdir(path)?;
            Ok(Destroyed::Rmdir)
        }
        Err(e) => Err(e.into()),
    }
}

/// `BTRFS_IOC_SNAP_DESTROY_V2` on `path`'s parent, by name.
pub fn destroy_by_ioctl(path: &Path) -> Result<(), Errno> {
    let (parent, name) = parent_and_name(path, SUBVOL_NAME_MAX)
        .map_err(|e| Errno::from_io_error(&e).unwrap_or(Errno::INVAL))?;
    let mut args = VolArgsV2 {
        fd: 0,
        transid: 0,
        flags: 0,
        unused: [0; 4],
        name: [0; SUBVOL_NAME_MAX + 1],
    };
    copy_name(&mut args.name, name);
    // SAFETY: IOC_SNAP_DESTROY_V2 is `_IOW(0x94, 63, struct
    // btrfs_ioctl_vol_args_v2)`, `VolArgsV2` its exact layout (layout tests
    // below), read only by the kernel. With `flags` 0 (no
    // BTRFS_SUBVOL_SPEC_BY_ID) the kernel reads the NUL-terminated `name`.
    unsafe { ioctl(&parent, Setter::<IOC_SNAP_DESTROY_V2, VolArgsV2>::new(args)) }
}

/// The unprivileged delete (Linux 4.18+): clear read-only, remove everything
/// inside, then `rmdir(2)` the empty subvolume. Its walk is the slow part for a
/// prefix with many files; the kernel frees the extents in the background.
pub fn destroy_by_rmdir(path: &Path) -> io::Result<()> {
    if is_subvolume(path)? {
        set_readonly(path, false)?;
    }
    crate::gog::materialize::remove_tree(path)
}

// — FIEMAP —

/// One physical extent of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    pub physical: u64,
    pub length: u64,
    pub flags: u32,
}

/// The extents of `file` that have a physical address (`FS_IOC_FIEMAP`, after
/// a sync). Inline, delayed and unknown extents are left out: they have no
/// address to compare.
pub fn extents(file: &File) -> io::Result<Vec<Extent>> {
    let mut out = Vec::new();
    let mut start = 0u64;
    loop {
        let mut buf = FiemapBuf {
            head: Fiemap {
                fm_start: start,
                fm_length: FIEMAP_MAX_OFFSET - start,
                fm_flags: FIEMAP_FLAG_SYNC,
                fm_extent_count: FIEMAP_BATCH as u32,
                ..Fiemap::default()
            },
            extents: [FiemapExtent::default(); FIEMAP_BATCH],
        };
        // SAFETY: FS_IOC_FIEMAP is `_IOWR('f', 11, struct fiemap)`. The kernel
        // reads the `Fiemap` header (its exact layout, tested below) and writes
        // at most `fm_extent_count` (= FIEMAP_BATCH) `struct fiemap_extent`s
        // right after it, which is where `FiemapBuf::extents` is (`#[repr(C)]`,
        // the header is 32 bytes and 8-aligned, offset tested below), and
        // updates `fm_mapped_extents`. `buf` lives across the call.
        unsafe { ioctl(file, Updater::<FS_IOC_FIEMAP, FiemapBuf>::new(&mut buf)) }?;
        let mapped = (buf.head.fm_mapped_extents as usize).min(FIEMAP_BATCH);
        if mapped == 0 {
            break;
        }
        let mut last = false;
        for e in &buf.extents[..mapped] {
            last |= e.fe_flags & FIEMAP_EXTENT_LAST != 0;
            let skip = FIEMAP_EXTENT_UNKNOWN | FIEMAP_EXTENT_DELALLOC | FIEMAP_EXTENT_DATA_INLINE;
            if e.fe_flags & skip == 0 && e.fe_physical != 0 && e.fe_length != 0 {
                out.push(Extent {
                    physical: e.fe_physical,
                    length: e.fe_length,
                    flags: e.fe_flags,
                });
            }
        }
        let tail = &buf.extents[mapped - 1];
        let next = tail.fe_logical.saturating_add(tail.fe_length);
        if last || next <= start {
            break;
        }
        start = next;
    }
    Ok(out)
}

/// The physical byte ranges the regular files under `root` use, merged.
/// Doesn't follow symlinks or leave `root`'s subvolume (a nested one is an
/// empty stub in a snapshot anyway).
fn tree_ranges(root: &Path) -> io::Result<Vec<(u64, u64)>> {
    let dev = std::fs::symlink_metadata(root)?.dev();
    let mut ranges = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            let path = entry.path();
            if ft.is_dir() {
                if std::fs::symlink_metadata(&path)?.dev() == dev {
                    stack.push(path);
                }
            } else if ft.is_file() {
                let file = File::open(&path)?;
                for e in extents(&file)? {
                    ranges.push((e.physical, e.physical.saturating_add(e.length)));
                }
            }
        }
    }
    Ok(merge(ranges))
}

fn merge(mut ranges: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    ranges.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for (s, e) in ranges {
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// For each tree, the bytes of physical ranges that no other tree uses. Each
/// tree's ranges must be merged (no overlaps within one tree).
pub(crate) fn exclusive_bytes(trees: &[Vec<(u64, u64)>]) -> Vec<u64> {
    // A sweep over every range's start and end: a stretch covered by exactly
    // one tree is that tree's alone.
    let mut events: BTreeMap<u64, Vec<(usize, bool)>> = BTreeMap::new();
    for (i, ranges) in trees.iter().enumerate() {
        for &(s, e) in ranges {
            events.entry(s).or_default().push((i, true));
            events.entry(e).or_default().push((i, false));
        }
    }
    let mut out = vec![0u64; trees.len()];
    let mut active = BTreeSet::new();
    let mut prev: Option<u64> = None;
    for (pos, evs) in events {
        if let (Some(p), Some(&only)) = (prev, active.first())
            && active.len() == 1
        {
            out[only] += pos - p;
        }
        for (i, open) in evs {
            if open {
                active.insert(i);
            } else {
                active.remove(&i);
            }
        }
        prev = Some(pos);
    }
    out
}

/// The FIEMAP estimate of design 20 §7.3: for each of `snapshots`, the bytes
/// of physical extents that neither `game_dir` nor another of `snapshots`
/// uses. An estimate: compressed extents are counted by their logical length,
/// and data shared with something outside this set (a reflink copy elsewhere)
/// counts as exclusive.
pub fn estimate_exclusive(game_dir: &Path, snapshots: &[PathBuf]) -> io::Result<Vec<u64>> {
    let mut trees = Vec::with_capacity(snapshots.len() + 1);
    trees.push(match tree_ranges(game_dir) {
        Ok(r) => r,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    });
    for s in snapshots {
        trees.push(tree_ranges(s)?);
    }
    Ok(exclusive_bytes(&trees).split_off(1))
}

// — the backend —

/// The btrfs backend: game dirs are subvolumes, snapshot `n` of
/// `<parent>/<gogid>` is the read-only subvolume
/// `<parent>/.gogo-snapshots/<gogid>/<n>`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Btrfs;

fn no_parent(game_dir: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{}: no parent dir to keep snapshots in", game_dir.display()),
    )
}

impl SnapshotBackend for Btrfs {
    fn probe(&self, dir: &Path) -> Support {
        probe(dir)
    }

    fn kind(&self) -> BackendKind {
        BackendKind::Btrfs
    }

    fn ready(&self, game_dir: &Path) -> io::Result<bool> {
        is_subvolume(game_dir)
    }

    fn make_game_dir(&self, dir: &Path) -> io::Result<()> {
        create_subvolume(dir)
    }

    fn snapshot(&self, game_dir: &Path, n: u32) -> io::Result<()> {
        let dst = backend::snapshot_dir(game_dir, n).ok_or_else(|| no_parent(game_dir))?;
        if let Some(dir) = dst.parent() {
            std::fs::create_dir_all(dir)?;
        }
        snapshot(game_dir, &dst, true)
    }

    fn snapshot_path(&self, game_dir: &Path, n: u32) -> Option<PathBuf> {
        backend::snapshot_dir(game_dir, n)
    }

    fn exists(&self, game_dir: &Path, n: u32) -> io::Result<bool> {
        let dst = backend::snapshot_dir(game_dir, n).ok_or_else(|| no_parent(game_dir))?;
        match std::fs::symlink_metadata(&dst) {
            Ok(m) => Ok(m.is_dir()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn delete(&self, game_dir: &Path, n: u32) -> io::Result<()> {
        if !self.exists(game_dir, n)? {
            return Ok(());
        }
        let dst = backend::snapshot_dir(game_dir, n).ok_or_else(|| no_parent(game_dir))?;
        destroy(&dst)?;
        // The per-game dir goes with its last snapshot; a race with another
        // snapshot, or anything else in it, just leaves it.
        if let Some(dir) = dst.parent() {
            let _ = std::fs::remove_dir(dir);
        }
        Ok(())
    }

    fn on_disk(&self, game_dir: &Path) -> io::Result<Vec<OnDisk>> {
        backend::list_dir(game_dir)
    }

    fn space(&self, game_dir: &Path, ns: &[u32]) -> io::Result<Vec<Space>> {
        let paths = ns
            .iter()
            .map(|&n| backend::snapshot_dir(game_dir, n).ok_or_else(|| no_parent(game_dir)))
            .collect::<io::Result<Vec<_>>>()?;
        let bytes = estimate_exclusive(game_dir, &paths)?;
        Ok(ns
            .iter()
            .zip(bytes)
            .map(|(&n, exclusive_bytes)| Space {
                n,
                exclusive_bytes,
                exact: false,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn vol_args_match_the_kernel_layout() {
        // btrfs.h: both structs are 4096 bytes.
        assert_eq!(size_of::<VolArgs>(), 4096);
        assert_eq!(offset_of!(VolArgs, fd), 0);
        assert_eq!(offset_of!(VolArgs, name), 8);
        assert_eq!(size_of::<VolArgsV2>(), 4096);
        assert_eq!(offset_of!(VolArgsV2, fd), 0);
        assert_eq!(offset_of!(VolArgsV2, transid), 8);
        assert_eq!(offset_of!(VolArgsV2, flags), 16);
        assert_eq!(offset_of!(VolArgsV2, unused), 24);
        assert_eq!(offset_of!(VolArgsV2, name), 56);
    }

    #[test]
    fn fiemap_matches_the_kernel_layout() {
        assert_eq!(size_of::<Fiemap>(), 32);
        assert_eq!(offset_of!(Fiemap, fm_start), 0);
        assert_eq!(offset_of!(Fiemap, fm_length), 8);
        assert_eq!(offset_of!(Fiemap, fm_flags), 16);
        assert_eq!(offset_of!(Fiemap, fm_mapped_extents), 20);
        assert_eq!(offset_of!(Fiemap, fm_extent_count), 24);
        assert_eq!(offset_of!(Fiemap, fm_reserved), 28);
        assert_eq!(size_of::<FiemapExtent>(), 56);
        assert_eq!(offset_of!(FiemapExtent, fe_logical), 0);
        assert_eq!(offset_of!(FiemapExtent, fe_physical), 8);
        assert_eq!(offset_of!(FiemapExtent, fe_length), 16);
        assert_eq!(offset_of!(FiemapExtent, fe_reserved64), 24);
        assert_eq!(offset_of!(FiemapExtent, fe_flags), 40);
        assert_eq!(offset_of!(FiemapExtent, fe_reserved), 44);
        // fm_extents[] starts right after the header.
        assert_eq!(offset_of!(FiemapBuf, extents), 32);
    }

    /// The numbers as the generic `_IOC` encoding (x86, arm, riscv…) gives
    /// them; other arches encode the direction differently.
    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn ioctl_numbers_match_the_headers() {
        assert_eq!(IOC_SUBVOL_CREATE as u64, 0x5000_940E);
        assert_eq!(IOC_SNAP_CREATE_V2 as u64, 0x5000_9417);
        assert_eq!(IOC_SUBVOL_GETFLAGS as u64, 0x8008_9419);
        assert_eq!(IOC_SUBVOL_SETFLAGS as u64, 0x4008_941A);
        assert_eq!(IOC_SNAP_DESTROY_V2 as u64, 0x5000_943F);
        assert_eq!(FS_IOC_FIEMAP as u64, 0xC020_660B);
    }

    #[test]
    fn exclusive_bytes_counts_what_one_tree_alone_covers() {
        // game: [0,100) and [300,400); snap a: [0,100) and [100,200);
        // snap b: [150,250) and [300,400).
        let game = vec![(0, 100), (300, 400)];
        let a = vec![(0, 200)];
        let b = vec![(150, 250), (300, 400)];
        let ex = exclusive_bytes(&[game, a, b]);
        // a alone: [100,150); b alone: [200,250); game alone: nothing.
        assert_eq!(ex, vec![0, 50, 50]);
        assert_eq!(exclusive_bytes(&[vec![], vec![(10, 20)]]), vec![0, 10]);
    }

    #[test]
    fn merge_joins_overlapping_and_touching_ranges() {
        assert_eq!(
            merge(vec![(5, 10), (0, 3), (3, 4), (8, 12), (20, 21)]),
            vec![(0, 4), (5, 12), (20, 21)]
        );
    }

    #[test]
    fn fs_names_cover_the_common_cases() {
        assert_eq!(fs_name(EXT4_SUPER_MAGIC), Some("ext4"));
        assert_eq!(fs_name(0x1234), None);
    }
}
