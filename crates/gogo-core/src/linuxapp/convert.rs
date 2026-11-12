//! Converting a plain game dir on btrfs into a subvolume (design 20 §2.1), so
//! a game built before snapshots (or with `--no-snapshots`) can have them.
//! Only ever on request: `snapshot <gogid> create` or `auto on`, after a plan
//! and a question (decision D8). Never as a side effect of another command,
//! which only settles a conversion that was interrupted.
//!
//! A file can't be renamed across a subvolume boundary, so the game is copied
//! into a new subvolume beside it with `move`'s exact copy (FICLONE per file,
//! symlinks, modes, mtimes, `user.*` attributes), checked, and swapped in with
//! `renameat2(RENAME_EXCHANGE)`:
//!
//! 1. the journal `<data_root>/snapshots/<gogid>/convert.json`;
//! 2. the subvolume `<parent>/.gogo-convert-<gogid>/`;
//! 3. the copy;
//! 4. the check: the same entries, kinds, sizes, modes, link targets and
//!    attributes, and the bytes of the files that were copied rather than
//!    cloned (a clone shares the source's extents);
//! 5. the marker `.gogo-converted` in the copy;
//! 6. the exchange: the commit;
//! 7. the marker removed, the old plain tree (now at the staging name)
//!    deleted, the journal removed.
//!
//! Recovery doesn't need to know how far it got: before the exchange the
//! staging dir holds the copy, after it the old tree, and either way it goes.
//! The marker in the game dir says the exchange happened.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::gog::materialize::{self, TreeMethod};
use crate::macapp::error::MacAppError;
use crate::macapp::launch::GameDirs;
use crate::macapp::relocate;
use crate::snapshot::Guard;

/// In the game dir from the copy's step 5 to step 7: the exchange happened
/// and the cleanup didn't. The launcher refuses while it is there (§5.4).
pub const MARKER: &str = crate::snapshot::backend::CONVERTED_MARKER;

/// The staging subvolume's name prefix, beside the game dir.
pub const STAGING_PREFIX: &str = ".gogo-convert-";

const JOURNAL: &str = "convert.json";

/// `<parent>/.gogo-convert-<gogid>`: where the copy is made, and where the old
/// tree is after the exchange.
pub fn staging_of(game_dir: &Path, gogid: &str) -> Option<PathBuf> {
    game_dir
        .parent()
        .map(|p| p.join(format!("{STAGING_PREFIX}{gogid}")))
}

fn journal_path(gogid: &str) -> PathBuf {
    crate::state::data_root()
        .join("snapshots")
        .join(gogid)
        .join(JOURNAL)
}

/// `convert.json`: what a recovery needs to find the two dirs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Journal {
    game_dir: PathBuf,
    staging: PathBuf,
    started: String,
}

fn write_journal(gogid: &str, j: &Journal) -> Result<(), MacAppError> {
    let path = journal_path(gogid);
    let dir = path.parent().expect("the journal has a dir");
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{JOURNAL}.tmp"));
    {
        use std::io::Write as _;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(j).map_err(io::Error::other)?)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    sync_dir(dir);
    Ok(())
}

fn read_journal(gogid: &str) -> Option<Journal> {
    let bytes = std::fs::read(journal_path(gogid)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn remove_journal(gogid: &str) -> Result<(), MacAppError> {
    let path = journal_path(gogid);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    if let Some(dir) = path.parent() {
        sync_dir(dir);
    }
    Ok(())
}

fn sync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

// — the plan —

/// What a conversion will do, shown before the question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvertPlan {
    pub game_dir: PathBuf,
    pub staging: PathBuf,
    /// The game's size.
    pub bytes: u64,
    /// `reflink` where FICLONE works between the game dir and its parent
    /// (one btrfs filesystem: always), else `copy`.
    pub method: TreeMethod,
    /// The disk the copy needs: 0 for a reflink copy, the game's size else.
    pub extra_bytes: u64,
    pub free_bytes: Option<u64>,
    /// Sockets and FIFOs: not copied (they mean nothing without the process
    /// that made them), so they go with the old tree.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub not_copied: Vec<PathBuf>,
}

impl ConvertPlan {
    /// The plan's lines (design 20 §2.1, D1).
    pub fn lines(&self) -> Vec<String> {
        let size = crate::fmt::human_size(self.bytes);
        let mut out = vec![
            format!(
                "{} is a plain dir: it becomes a btrfs subvolume first, so it can be \
                 snapshotted",
                self.game_dir.display()
            ),
            format!(
                "  the game ({size}) is copied into a new subvolume, {}, checked against the \
                 original, and swapped in; the old plain dir is deleted",
                self.staging.display()
            ),
        ];
        out.push(match self.method {
            TreeMethod::Reflink => "  a reflink copy: the files share their data with the \
                                    old ones, so it costs almost no space"
                .to_string(),
            TreeMethod::Copy => format!(
                "  reflinks don't work here: a full copy, which needs {size} free{}",
                self.free_bytes.map_or_else(String::new, |f| format!(
                    " ({} is)",
                    crate::fmt::human_size(f)
                ))
            ),
        });
        if let Some(first) = self.not_copied.first() {
            out.push(format!(
                "  {} socket(s) or FIFO(s) aren't copied and go with the old dir: {}{}",
                self.not_copied.len(),
                first.display(),
                if self.not_copied.len() > 1 {
                    ", …"
                } else {
                    ""
                }
            ));
        }
        out.push(
            "  the game dir then drops out of snapshots of the subvolume around it \
             (snapper, Timeshift, btrfs send backups); file-level backups are not affected"
                .to_string(),
        );
        out
    }
}

/// Where a conversion stops, for crash injection ([`ConvertOptions::fail_at`]),
/// in the order of the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertStep {
    Journal,
    Subvolume,
    Copied,
    Verified,
    Marked,
    Exchanged,
    MarkerRemoved,
    OldTreeDeleted,
}

/// Test seams; the default is a real run.
#[derive(Debug, Clone, Default)]
pub struct ConvertOptions {
    /// Stop right after this step, as a crash there would.
    #[doc(hidden)]
    pub fail_at: Option<ConvertStep>,
    /// The free space to assume.
    #[doc(hidden)]
    pub free_bytes: Option<u64>,
    /// Plan as if FICLONE didn't work.
    #[doc(hidden)]
    pub assume_copy: bool,
    /// Change a file's mode in the copy before the check.
    #[doc(hidden)]
    pub corrupt_copy: bool,
    /// Copy as if FICLONE didn't work, whatever the plan said.
    #[doc(hidden)]
    pub no_reflink: bool,
    /// Plan as if FICLONE worked.
    #[doc(hidden)]
    pub assume_reflink: bool,
    /// Take this path in the game tree for one on another device.
    #[doc(hidden)]
    pub other_device_at: Option<PathBuf>,
}

/// What the copy must leave out of the free space it found: a 20th of it, at
/// least 256 MiB, so a conversion never fills the disk.
fn margin(free: u64) -> u64 {
    (free / 20).max(256 << 20)
}

/// What [`scan`] found in a game tree.
#[derive(Debug, Default)]
struct Scan {
    bytes: u64,
    /// An entry on another device than the game dir: a mount point, a bind
    /// mount or a nested subvolume. The copy would walk into it, and deleting
    /// the old tree would empty it.
    other_device: Option<PathBuf>,
    /// What the copy can't keep, one line each: hard links, attributes other
    /// than `user.*` (`security.selinux` aside: the check compares it), inode
    /// flags (`chattr`).
    lossy: Vec<String>,
    not_copied: Vec<PathBuf>,
}

/// The inode flags a copy doesn't carry over and that someone set on purpose.
#[cfg(target_os = "linux")]
fn lossy_flags(path: &Path, dir: bool) -> Option<String> {
    use rustix::fs::{IFlags, Mode, OFlags};
    let mut how = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    if dir {
        how |= OFlags::DIRECTORY;
    }
    let fd = rustix::fs::open(path, how, Mode::empty()).ok()?;
    let flags = rustix::fs::ioctl_getflags(&fd).ok()?;
    let named = [
        (IFlags::APPEND, "a"),
        (IFlags::COMPRESSED, "c"),
        (IFlags::NOCOW, "C"),
        (IFlags::NODUMP, "d"),
        (IFlags::IMMUTABLE, "i"),
        (IFlags::NOATIME, "A"),
        (IFlags::SYNC, "S"),
        (IFlags::DIRSYNC, "D"),
        (IFlags::SECURE_REMOVAL, "s"),
        (IFlags::UNRM, "u"),
        (IFlags::PROJECT_INHERIT, "P"),
    ];
    let set: String = named
        .iter()
        .filter(|(f, _)| flags.contains(*f))
        .map(|(_, c)| *c)
        .collect();
    (!set.is_empty()).then(|| format!("{}: chattr +{set}", path.display()))
}

/// The extended attributes a copy doesn't carry over: everything but
/// `user.*` and the SELinux label.
#[cfg(target_os = "linux")]
fn lossy_xattrs(path: &Path) -> Option<String> {
    let size = rustix::fs::llistxattr_query(path).ok()?;
    let mut list = vec![0u8; size];
    let n = rustix::fs::llistxattr(path, &mut list[..]).ok()?;
    list.truncate(n);
    let names: Vec<String> = list
        .split(|b| *b == 0)
        .filter(|n| !n.is_empty() && !n.starts_with(b"user.") && *n != b"security.selinux")
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .collect();
    (!names.is_empty()).then(|| format!("{}: {}", path.display(), names.join(", ")))
}

#[cfg(not(target_os = "linux"))]
fn lossy_flags(_path: &Path, _dir: bool) -> Option<String> {
    None
}

#[cfg(not(target_os = "linux"))]
fn lossy_xattrs(_path: &Path) -> Option<String> {
    None
}

/// Walk the game tree as the copy will, without following links: its size,
/// what isn't on its device, what the copy would lose, what it skips. Stops at
/// the first entry on another device.
fn scan(root: &Path, other_device_at: Option<&Path>) -> io::Result<Scan> {
    use std::os::unix::fs::MetadataExt as _;
    let dev = root.symlink_metadata()?.dev();
    let mut s = Scan::default();
    s.lossy.extend(lossy_flags(root, true));
    s.lossy.extend(lossy_xattrs(root));
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            let m = path.symlink_metadata()?;
            let kind = m.file_type();
            if kind.is_symlink() {
                s.lossy.extend(lossy_xattrs(&path));
                continue;
            }
            if m.dev() != dev || other_device_at == Some(path.as_path()) {
                s.other_device = Some(path);
                return Ok(s);
            }
            if kind.is_dir() {
                s.lossy.extend(lossy_flags(&path, true));
                stack.push(path.clone());
            } else if kind.is_file() {
                s.bytes += m.len();
                if m.nlink() > 1 {
                    s.lossy.push(format!(
                        "{}: a hard link ({} names)",
                        path.display(),
                        m.nlink()
                    ));
                }
                s.lossy.extend(lossy_flags(&path, false));
            } else {
                s.not_copied.push(path);
                continue;
            }
            s.lossy.extend(lossy_xattrs(&path));
        }
    }
    Ok(s)
}

/// The plan for converting `dirs.root`, with the refusals that are the
/// conversion's own: a symlinked or mounted game dir, another filesystem or
/// subvolume inside it, what the copy can't keep (hard links, attributes
/// other than `user.*`, `chattr` flags), a staging dir in the way, a copy
/// that doesn't fit. Creates nothing but [`materialize::plan_tree`]'s probe
/// files.
pub fn plan(
    gogid: &str,
    dirs: &GameDirs,
    opts: &ConvertOptions,
) -> Result<ConvertPlan, MacAppError> {
    use std::os::unix::fs::MetadataExt as _;
    let root = &dirs.root;
    let refuse = |reason: String| MacAppError::Library {
        path: root.clone(),
        reason,
    };
    let meta = root.symlink_metadata()?;
    if meta.file_type().is_symlink() {
        return Err(refuse(
            "it is a symlink: gogo converts only a real dir; replace the link with the dir \
             it points at first"
                .into(),
        ));
    }
    let (Some(parent), Some(staging)) = (root.parent(), staging_of(root, gogid)) else {
        return Err(refuse("it has no parent dir".into()));
    };
    if parent.metadata()?.dev() != meta.dev() {
        return Err(refuse(
            "it is a mount point or a subvolume of its own already, which gogo won't \
             convert"
                .into(),
        ));
    }
    if staging.symlink_metadata().is_ok() {
        return Err(refuse(format!(
            "{} is in the way: `gogo linux-app snapshot {gogid} list` settles what an \
             interrupted conversion left; if gogo didn't make it, move it away first",
            staging.display()
        )));
    }
    let scan = scan(root, opts.other_device_at.as_deref())?;
    if let Some(other) = &scan.other_device {
        return Err(refuse(format!(
            "{} is on another filesystem or is a subvolume of its own: the copy would walk \
             into it and deleting the old dir would empty it; unmount or move it out first",
            other.display()
        )));
    }
    if !scan.lossy.is_empty() {
        let shown: Vec<&str> = scan.lossy.iter().take(3).map(String::as_str).collect();
        return Err(refuse(format!(
            "{} item(s) carry what the copy can't keep, and the old dir is deleted after it: \
             {}{}; remove those (or make the dir a subvolume by hand) first",
            scan.lossy.len(),
            shown.join("; "),
            if scan.lossy.len() > 3 { "; …" } else { "" }
        )));
    }
    let bytes = scan.bytes;
    let method = if opts.assume_copy {
        TreeMethod::Copy
    } else if opts.assume_reflink {
        TreeMethod::Reflink
    } else {
        materialize::plan_tree(root, &staging, bytes).method
    };
    let extra_bytes = match method {
        TreeMethod::Reflink => 0,
        TreeMethod::Copy => bytes,
    };
    let free_bytes = opts
        .free_bytes
        .or_else(|| fs4::available_space(parent).ok());
    if extra_bytes > 0
        && let Some(free) = free_bytes
        && free < extra_bytes + margin(free)
    {
        return Err(refuse(format!(
            "converting it to a subvolume needs a full copy here (reflinks don't work), \
             {} free, and {} is",
            crate::fmt::human_size(extra_bytes),
            crate::fmt::human_size(free)
        )));
    }
    Ok(ConvertPlan {
        game_dir: root.clone(),
        staging,
        bytes,
        method,
        extra_bytes,
        free_bytes,
        not_copied: scan.not_copied,
    })
}

// — the conversion —

/// What a conversion did (`--json`'s `conversion`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Converted {
    pub game_dir: PathBuf,
    /// The time it took, start to finish.
    pub seconds: f64,
    pub bytes: u64,
    pub reflinked_files: u64,
    pub copied_files: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// A conversion's progress, in bytes of the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertProgress {
    Copying { done: u64, total: u64 },
    Verifying { done: u64, total: u64 },
}

fn stop(opts: &ConvertOptions, at: ConvertStep) -> Result<(), MacAppError> {
    if opts.fail_at == Some(at) {
        return Err(MacAppError::Apps(format!("injected stop after {at:?}")));
    }
    Ok(())
}

/// `renameat2(RENAME_EXCHANGE)`: `a` and `b` swap names, atomically.
#[cfg(target_os = "linux")]
fn exchange(a: &Path, b: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    renameat_with(CWD, a, CWD, b, RenameFlags::EXCHANGE).map_err(io::Error::from)
}

/// Snapshots, and so conversions, are Linux-only (design 20).
#[cfg(not(target_os = "linux"))]
fn exchange(_a: &Path, _b: &Path) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// Convert `gogid`'s game dir into what `make_subvolume` makes (a btrfs
/// subvolume), under the locks the guard stands for. The caller has settled
/// any earlier conversion ([`settle_locked`]) and checked §5.1's refusals.
/// An error before the exchange leaves the game as it was and nothing behind
/// (a stop injected by [`ConvertOptions::fail_at`] leaves everything, as a
/// crash would); after it, the game is converted and the next `snapshot`
/// command finishes the cleanup.
pub(crate) fn convert_locked(
    gogid: &str,
    dirs: &GameDirs,
    make_subvolume: &dyn Fn(&Path) -> io::Result<()>,
    _guard: &Guard,
    opts: &ConvertOptions,
    emit: &mut dyn FnMut(ConvertProgress),
) -> Result<Converted, MacAppError> {
    let started = Instant::now();
    let plan = plan(gogid, dirs, opts)?;
    let root = &plan.game_dir;
    let staging = &plan.staging;
    write_journal(
        gogid,
        &Journal {
            game_dir: root.clone(),
            staging: staging.clone(),
            started: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        },
    )?;
    stop(opts, ConvertStep::Journal)?;
    let copied = match copy_checked(gogid, dirs, &plan, make_subvolume, opts, emit) {
        Ok(c) => c,
        Err(Stopped::Injected(e)) => return Err(e),
        Err(Stopped::NotMade(e)) => {
            // Whatever is at the staging name isn't this run's: left alone.
            remove_journal(gogid)?;
            return Err(MacAppError::Library {
                path: staging.clone(),
                reason: format!("the subvolume for the copy couldn't be made ({e})"),
            });
        }
        Err(Stopped::Failed(e)) => {
            // The game was only read: the copy goes, the game stays as it was.
            let _ = remove_staging(staging);
            remove_journal(gogid)?;
            return Err(MacAppError::Library {
                path: root.clone(),
                reason: format!(
                    "converting it to a subvolume failed ({e}); the game wasn't touched"
                ),
            });
        }
    };
    exchange(staging, root).map_err(|e| {
        let _ = remove_staging(staging);
        let _ = remove_journal(gogid);
        MacAppError::Library {
            path: root.clone(),
            reason: format!(
                "the converted copy couldn't be swapped in ({e}); the game wasn't touched"
            ),
        }
    })?;
    if let Some(parent) = root.parent() {
        sync_dir(parent);
    }
    stop(opts, ConvertStep::Exchanged)?;
    finish(gogid, root, staging, opts).map_err(|e| MacAppError::Library {
        path: root.clone(),
        reason: format!(
            "it is a subvolume now, but the cleanup failed ({e}): `gogo linux-app snapshot \
             {gogid} list` finishes it"
        ),
    })?;
    let mut warnings = Vec::new();
    if copied.xattrs_lost > 0 {
        warnings.push(format!(
            "{} extended attribute(s) weren't taken by the copy",
            copied.xattrs_lost
        ));
    }
    if !copied.skipped.is_empty() {
        warnings.push(format!(
            "{} socket(s) or FIFO(s) weren't copied: {}",
            copied.skipped.len(),
            copied.skipped[0].display()
        ));
    }
    Ok(Converted {
        game_dir: root.clone(),
        seconds: started.elapsed().as_secs_f64(),
        bytes: plan.bytes,
        reflinked_files: copied.report.reflinked_files,
        copied_files: copied.report.copied_files,
        warnings,
    })
}

enum Stopped {
    /// A crash injected: everything stays as it is.
    Injected(MacAppError),
    /// The staging subvolume wasn't made: nothing of this run's to remove.
    NotMade(io::Error),
    Failed(MacAppError),
}

impl From<io::Error> for Stopped {
    fn from(e: io::Error) -> Self {
        Stopped::Failed(e.into())
    }
}

/// Steps 2 to 5: the subvolume, the copy, the check, the marker.
fn copy_checked(
    gogid: &str,
    dirs: &GameDirs,
    plan: &ConvertPlan,
    make_subvolume: &dyn Fn(&Path) -> io::Result<()>,
    opts: &ConvertOptions,
    emit: &mut dyn FnMut(ConvertProgress),
) -> Result<materialize::ExactCopy, Stopped> {
    let (root, staging) = (&plan.game_dir, &plan.staging);
    let injected = |at| stop(opts, at).map_err(Stopped::Injected);
    make_subvolume(staging).map_err(Stopped::NotMade)?;
    injected(ConvertStep::Subvolume)?;
    let total = plan.bytes;
    let mut done = 0u64;
    let how = materialize::IntoOptions {
        reflink: if opts.no_reflink {
            materialize::Reflink::Never
        } else {
            materialize::Reflink::Try
        },
        copy_budget: plan.free_bytes.map(|f| f.saturating_sub(margin(f))),
    };
    let copied = materialize::copy_exact_into(root, staging, how, &mut |_, size| {
        done += size;
        emit(ConvertProgress::Copying { done, total });
        Ok(())
    })?;
    injected(ConvertStep::Copied)?;
    if opts.corrupt_copy {
        corrupt_a_mode(staging)?;
    }
    // Compared after the flush, so a write error the flush reports comes first.
    materialize::sync_filesystem(staging)?;
    let bytes_of: HashSet<PathBuf> = copied.copied.iter().cloned().collect();
    let mut done = 0u64;
    materialize::verify_tree(
        root,
        staging,
        &materialize::Verify {
            xattrs_lost: copied.xattrs_lost > 0,
            modes: true,
            bytes_of: Some(&bytes_of),
            mtimes: true,
            selinux: true,
        },
        &mut |_, size| {
            done += size;
            emit(ConvertProgress::Verifying { done, total });
            Ok(())
        },
    )
    .map_err(|e| Stopped::Failed(MacAppError::Apps(format!("the copy doesn't match: {e}"))))?;
    injected(ConvertStep::Verified)?;
    // It may have been launched during the copy: what it wrote would be lost
    // with the old tree.
    super::snapshots::refuse(gogid, dirs).map_err(Stopped::Failed)?;
    std::fs::write(staging.join(MARKER), "")?;
    materialize::sync_filesystem(staging)?;
    injected(ConvertStep::Marked)?;
    Ok(copied)
}

/// Make the first file found under `dir` world-writable, as a copy that went
/// wrong would differ.
fn corrupt_a_mode(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        if e.file_type()?.is_file() {
            let m = e.metadata()?.permissions().mode();
            return std::fs::set_permissions(e.path(), std::fs::Permissions::from_mode(m ^ 0o002));
        }
    }
    Ok(())
}

/// A copy subvolume (or, after the exchange, the old plain tree): emptied and
/// removed with `rmdir`, which removes an empty subvolume unprivileged (Linux
/// 4.18+).
fn remove_staging(staging: &Path) -> io::Result<()> {
    match staging.symlink_metadata() {
        Ok(_) => materialize::remove_tree(staging),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Step 7.
fn finish(
    gogid: &str,
    root: &Path,
    staging: &Path,
    opts: &ConvertOptions,
) -> Result<(), MacAppError> {
    match std::fs::remove_file(root.join(MARKER)) {
        Ok(()) => sync_dir(root),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    stop(opts, ConvertStep::MarkerRemoved)?;
    remove_staging(staging)?;
    if let Some(parent) = staging.parent() {
        sync_dir(parent);
    }
    stop(opts, ConvertStep::OldTreeDeleted)?;
    remove_journal(gogid)
}

// — recovery and doctor —

/// What an interrupted conversion left: each is a reason to settle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pending {
    pub journal: Option<PathBuf>,
    /// The marker in the game dir: the exchange happened.
    pub marker: Option<PathBuf>,
    pub staging: Option<PathBuf>,
    /// The staging dir is there, and nothing says gogo made it: no journal
    /// names it, it isn't a subvolume root, it holds no marker. Settling
    /// leaves it alone.
    pub foreign: bool,
}

impl Pending {
    pub fn is_empty(&self) -> bool {
        self.journal.is_none() && self.marker.is_none() && self.staging.is_none()
    }

    /// Something settling would do: more than a staging dir gogo can't tell
    /// is its own, which it leaves alone.
    pub fn needs_settling(&self) -> bool {
        !self.is_empty() && !self.only_foreign()
    }

    /// Only a staging dir gogo can't tell is its own: settling won't touch it.
    pub fn only_foreign(&self) -> bool {
        self.foreign && self.journal.is_none() && self.marker.is_none()
    }

    /// What is there, in words (`doctor`).
    pub fn describe(&self) -> String {
        let mut what = Vec::new();
        if let Some(j) = &self.journal {
            what.push(format!("the journal {}", j.display()));
        }
        if let Some(m) = &self.marker {
            what.push(format!("the marker {}", m.display()));
        }
        if let Some(s) = &self.staging {
            what.push(format!("{}", s.display()));
        }
        what.join(", ")
    }
}

/// Whether the dir at the staging name is one gogo made: the journal names
/// it, or it is a btrfs subvolume root (inode 256: the copy before the
/// exchange), or it holds the marker.
fn ours(staging: &Path, journal: Option<&Journal>) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    journal.is_some_and(|j| j.staging == staging)
        || staging
            .symlink_metadata()
            .is_ok_and(|m| m.is_dir() && m.ino() == 256)
        || staging.join(MARKER).symlink_metadata().is_ok()
}

/// What an interrupted conversion of `gogid` (game dir `root`) left. Reads
/// only.
pub fn pending(gogid: &str, root: &Path) -> Pending {
    let journal = read_journal(gogid);
    let root = journal
        .as_ref()
        .map_or_else(|| root.to_path_buf(), |j| j.game_dir.clone());
    let staging = staging_of(&root, gogid).filter(|p| p.symlink_metadata().is_ok());
    Pending {
        foreign: staging
            .as_deref()
            .is_some_and(|s| !ours(s, journal.as_ref())),
        journal: Some(journal_path(gogid)).filter(|p| p.is_file()),
        marker: Some(root.join(MARKER)).filter(|p| p.symlink_metadata().is_ok()),
        staging,
    }
}

/// Settle what an interrupted conversion of `gogid` left, if anything, under
/// the two locks and after the refusals settling takes (an unreachable or
/// running game): every command that changes a game calls this first, through
/// `rollback::settle_pending` (design 20 §2.1). Returns what it did, one line
/// each. Never converts, and never deletes a staging dir gogo can't tell is
/// its own.
pub fn settle(gogid: &str) -> Result<Vec<String>, MacAppError> {
    crate::macapp::compat::validate_gogid(gogid)?;
    let dirs = GameDirs::for_game(gogid)?;
    if !pending(gogid, &dirs.root).needs_settling() {
        return Ok(Vec::new());
    }
    let root_lock = relocate::lock_moves()?;
    let game_lock = relocate::lock_game(gogid)?;
    super::snapshots::refuse_settle(gogid, &dirs)?;
    settle_locked(gogid, &dirs.root, &Guard::new(&root_lock, &game_lock))
}

/// [`settle`] under the locks the guard stands for.
pub(crate) fn settle_locked(
    gogid: &str,
    root: &Path,
    _guard: &Guard,
) -> Result<Vec<String>, MacAppError> {
    let p = pending(gogid, root);
    if !p.needs_settling() {
        return Ok(Vec::new());
    }
    let journal = read_journal(gogid);
    let root = journal
        .as_ref()
        .map_or_else(|| root.to_path_buf(), |j| j.game_dir.clone());
    if !root.parent().is_some_and(Path::is_dir) {
        return Err(MacAppError::Apps(format!(
            "a conversion of {gogid} to a subvolume was interrupted, and {} isn't there (a \
             drive not mounted?): mount it first",
            root.display()
        )));
    }
    let staging = staging_of(&root, gogid).expect("a game dir with a parent");
    let committed = p.marker.is_some();
    let mut out = Vec::new();
    if p.foreign {
        // Only the marker and the journal go.
        match std::fs::remove_file(root.join(MARKER)) {
            Ok(()) => sync_dir(&root),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        remove_journal(gogid)?;
        out.push(format!(
            "{} was left alone: nothing says gogo made it; look at it and remove it by hand",
            staging.display()
        ));
    } else {
        finish(gogid, &root, &staging, &ConvertOptions::default())?;
    }
    out.insert(
        0,
        if committed {
            format!(
                "an interrupted conversion of {} to a subvolume was finished",
                root.display()
            )
        } else {
            format!(
                "an interrupted conversion of {} to a subvolume was undone: the game is as it \
                 was",
                root.display()
            )
        },
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_says_what_a_reflink_copy_costs_and_what_d1_means() {
        let p = ConvertPlan {
            game_dir: "/g/1".into(),
            staging: "/g/.gogo-convert-1".into(),
            bytes: 3 << 30,
            method: TreeMethod::Reflink,
            extra_bytes: 0,
            free_bytes: Some(1 << 30),
            not_copied: Vec::new(),
        };
        let text = p.lines().join("\n");
        assert!(text.contains("costs almost no space"), "{text}");
        assert!(text.contains("3.0 GiB"), "{text}");
        assert!(
            text.contains("drops out of snapshots of the subvolume around it"),
            "{text}"
        );
        let copy = ConvertPlan {
            method: TreeMethod::Copy,
            extra_bytes: 3 << 30,
            ..p
        };
        let text = copy.lines().join("\n");
        assert!(
            text.contains("a full copy, which needs 3.0 GiB free (1.0 GiB is)"),
            "{text}"
        );
    }

    #[test]
    fn the_staging_dir_is_beside_the_game_dir() {
        assert_eq!(
            staging_of(Path::new("/lib/games/42"), "42"),
            Some(PathBuf::from("/lib/games/.gogo-convert-42"))
        );
    }
}
