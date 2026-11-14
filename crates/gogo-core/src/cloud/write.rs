//! Writing local save files (design 22 §5.2 step 6, §7): every write goes
//! through a dir opened one part at a time from `/` without following a
//! link, so a link put in place after [`Location::file`]'s check is refused
//! rather than followed. Files are replaced atomically (a temp file in the
//! same dir, fsync, rename, fsync of the dir); a stop at any point leaves
//! the old file or the new one, never a partial one.
//!
//! [`Location::file`]: super::locations::Location::file

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::SystemTime;

use rustix::fs::{AtFlags, FileType, Mode, OFlags, Stat, Timespec, Timestamps};

/// The prefix of gogo's temp files beside a save; `*.tmp` is excluded from
/// sync (design 22 §3.4), so a leftover is never uploaded.
pub const TEMP_PREFIX: &str = ".gogo-pull-";

/// What is wrong with a target, in words for the output.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WriteError {
    /// A link, a file where a dir should be, or a dir that may not be made:
    /// the path isn't what the plan checked.
    #[error("{path}: {reason}")]
    Path { path: String, reason: String },
    /// The file isn't the one the plan was made on (changed, replaced, or
    /// there when it wasn't).
    #[error("{path}: changed since the plan was made")]
    Changed { path: String },
    /// The filesystem failed (space, permissions, I/O).
    #[error("{path}: {reason}")]
    Io { path: String, reason: String },
}

fn io(path: &Path, e: impl std::fmt::Display) -> WriteError {
    WriteError::Io {
        path: path.display().to_string(),
        reason: e.to_string(),
    }
}

/// A dir opened without following a link anywhere on its path.
pub struct Dir {
    fd: OwnedFd,
    pub path: PathBuf,
}

/// The file a writer expects at a name: the plan's view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// Nothing there.
    Absent,
    /// This regular file, unchanged.
    File {
        dev: u64,
        ino: u64,
        size: u64,
        /// Nanoseconds since the epoch.
        mtime_ns: i128,
    },
}

impl Expect {
    pub fn of(f: &super::scan::LocalFile) -> Expect {
        Expect::File {
            dev: f.dev,
            ino: f.ino,
            size: f.size,
            mtime_ns: datetime_ns(&f.mtime),
        }
    }
}

/// A record time (UTC, to the nanosecond) as nanoseconds since the epoch.
fn datetime_ns(d: &toml::value::Datetime) -> i128 {
    chrono::DateTime::parse_from_rfc3339(&d.to_string())
        .ok()
        .and_then(|t| t.timestamp_nanos_opt())
        .map_or(i128::MIN, i128::from)
}

/// `st` is the regular file `expect` names.
// `st_dev` is an `i32` on macOS, a `u64` on Linux.
#[allow(clippy::unnecessary_cast)]
fn matches(st: &Stat, expect: Expect) -> bool {
    match expect {
        Expect::Absent => false,
        Expect::File {
            dev,
            ino,
            size,
            mtime_ns,
        } => {
            FileType::from_raw_mode(st.st_mode) == FileType::RegularFile
                && st.st_dev as u64 == dev
                && st.st_ino as u64 == ino
                && st.st_size as u64 == size
                && stat_mtime_ns(st) == mtime_ns
        }
    }
}

fn stat_mtime_ns(st: &Stat) -> i128 {
    i128::from(st.st_mtime) * 1_000_000_000 + i128::from(st.st_mtime_nsec)
}

/// Open `dir` (absolute, canonical for the parts that exist) one part at a
/// time from `/`, refusing any link. Missing parts are created only below
/// `base` (the prefix or the game's files, which must exist): gogo never
/// makes a prefix, or a dir above it.
pub fn open_dir(dir: &Path, base: &Path) -> Result<Dir, WriteError> {
    let refuse = |reason: &str| WriteError::Path {
        path: dir.display().to_string(),
        reason: reason.to_string(),
    };
    if !dir.is_absolute() || !dir.starts_with(base) {
        return Err(refuse("not below the game's prefix or files"));
    }
    let flags = OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::RDONLY | OFlags::CLOEXEC;
    let mut fd = rustix::fs::open("/", flags, Mode::empty()).map_err(|e| io(Path::new("/"), e))?;
    let mut at = PathBuf::from("/");
    for c in dir.components() {
        let part = match c {
            Component::RootDir => continue,
            Component::Normal(p) => p,
            _ => return Err(refuse("not a plain path")),
        };
        let next = at.join(part);
        let opened = match rustix::fs::openat(&fd, part, flags, Mode::empty()) {
            Err(rustix::io::Errno::NOENT) if at.starts_with(base) => {
                match rustix::fs::mkdirat(&fd, part, Mode::from_raw_mode(0o777)) {
                    Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                    Err(e) => return Err(io(&next, e)),
                }
                rustix::fs::fsync(&fd).map_err(|e| io(&at, e))?;
                rustix::fs::openat(&fd, part, flags, Mode::empty())
            }
            other => other,
        };
        fd = match opened {
            Ok(f) => f,
            Err(rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) => {
                return Err(WriteError::Path {
                    path: next.display().to_string(),
                    reason: "a link or a file where a dir should be; not followed".into(),
                });
            }
            Err(rustix::io::Errno::NOENT) => {
                return Err(WriteError::Path {
                    path: next.display().to_string(),
                    reason: "not there, and gogo only makes dirs inside the game's prefix or files"
                        .into(),
                });
            }
            Err(e) => return Err(io(&next, e)),
        };
        at = next;
    }
    Ok(Dir {
        fd,
        path: dir.to_path_buf(),
    })
}

impl Dir {
    fn at(&self, name: &OsStr) -> PathBuf {
        self.path.join(name)
    }

    /// What is at `name` now, not following a link.
    fn stat(&self, name: &OsStr) -> Result<Option<Stat>, WriteError> {
        match rustix::fs::statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(s) => Ok(Some(s)),
            Err(rustix::io::Errno::NOENT) => Ok(None),
            Err(e) => Err(io(&self.at(name), e)),
        }
    }

    /// `name` is what `expect` says.
    pub fn check(&self, name: &OsStr, expect: Expect) -> Result<(), WriteError> {
        let changed = || WriteError::Changed {
            path: self.at(name).display().to_string(),
        };
        match self.stat(name)? {
            None if expect == Expect::Absent => Ok(()),
            Some(st) if matches(&st, expect) => Ok(()),
            _ => Err(changed()),
        }
    }

    /// Open `name` for reading: a regular file, no link, no FIFO wait, the
    /// one `expect` names.
    pub fn open_read(&self, name: &OsStr, expect: Expect) -> Result<File, WriteError> {
        let fd = rustix::fs::openat(
            &self.fd,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| match e {
            rustix::io::Errno::LOOP | rustix::io::Errno::NOENT => WriteError::Changed {
                path: self.at(name).display().to_string(),
            },
            e => io(&self.at(name), e),
        })?;
        let st = rustix::fs::fstat(&fd).map_err(|e| io(&self.at(name), e))?;
        let same = matches(&st, expect);
        if !same {
            return Err(WriteError::Changed {
                path: self.at(name).display().to_string(),
            });
        }
        Ok(File::from(fd))
    }

    fn sync(&self) -> Result<(), WriteError> {
        rustix::fs::fsync(&self.fd).map_err(|e| io(&self.path, e))
    }

    /// Remove gogo's own temp files left here by a pull that stopped: read
    /// through the opened dir, removed only when still a regular file.
    pub fn clear_temps(&self) {
        let Ok(rd) = rustix::fs::Dir::read_from(&self.fd) else {
            return;
        };
        let names: Vec<std::ffi::CString> = rd
            .flatten()
            .map(|e| e.file_name().to_owned())
            .filter(|n| {
                let n = n.to_bytes();
                n.starts_with(TEMP_PREFIX.as_bytes()) && n.ends_with(b".tmp")
            })
            .collect();
        for name in names {
            let regular = rustix::fs::statat(&self.fd, name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)
                .is_ok_and(|st| FileType::from_raw_mode(st.st_mode) == FileType::RegularFile);
            if regular {
                let _ = rustix::fs::unlinkat(&self.fd, name.as_c_str(), AtFlags::empty());
            }
        }
    }

    /// Replace (or create) `name` atomically with what `fill` writes: a
    /// temp file here, created exclusively without following a link, filled,
    /// its mtime set to `mtime` (when given), fsynced, `name` checked against
    /// `expect` once more, renamed over it, the dir fsynced. The written
    /// file's size and mtime. `stop` is a test seam: called after the fsync,
    /// before the rename; `true` stops there as a crash would (the temp file
    /// is left).
    pub fn replace(
        &self,
        name: &OsStr,
        expect: Expect,
        mtime: Option<SystemTime>,
        fill: &mut dyn FnMut(&mut dyn Write) -> Result<(), WriteError>,
        stop: &dyn Fn() -> bool,
    ) -> Result<(u64, SystemTime), WriteError> {
        self.check(name, expect)?;
        let mode = match self.stat(name)? {
            Some(st) => Mode::from_raw_mode(st.st_mode & 0o7777),
            None => Mode::from_raw_mode(0o666),
        };
        let tmp = temp_name();
        let fd = rustix::fs::openat(
            &self.fd,
            &tmp,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            mode,
        )
        .map_err(|e| io(&self.at(&tmp), e))?;
        let result = (|| {
            let mut f = File::from(fd);
            {
                let mut w = std::io::BufWriter::new(&mut f);
                fill(&mut w)?;
                w.flush().map_err(|e| io(&self.at(&tmp), e))?;
            }
            if expect != Expect::Absent {
                // An existing file's mode, umask aside.
                rustix::fs::fchmod(f.as_fd(), mode).map_err(|e| io(&self.at(&tmp), e))?;
            }
            if let Some(t) = mtime {
                let ts = timespec(t);
                rustix::fs::futimens(
                    f.as_fd(),
                    &Timestamps {
                        last_access: ts,
                        last_modification: ts,
                    },
                )
                .map_err(|e| io(&self.at(&tmp), e))?;
            }
            f.sync_all().map_err(|e| io(&self.at(&tmp), e))?;
            let st = rustix::fs::fstat(f.as_fd()).map_err(|e| io(&self.at(&tmp), e))?;
            Ok::<_, WriteError>((st.st_size as u64, system_time(&st)))
        })();
        let written = match result {
            Ok(w) => w,
            Err(e) => {
                let _ = rustix::fs::unlinkat(&self.fd, &tmp, AtFlags::empty());
                return Err(e);
            }
        };
        if stop() {
            return Err(WriteError::Io {
                path: self.at(name).display().to_string(),
                reason: "stopped before the rename (test)".into(),
            });
        }
        if let Err(e) = self.check(name, expect) {
            let _ = rustix::fs::unlinkat(&self.fd, &tmp, AtFlags::empty());
            return Err(e);
        }
        if let Err(e) = rustix::fs::renameat(&self.fd, &tmp, &self.fd, name) {
            let _ = rustix::fs::unlinkat(&self.fd, &tmp, AtFlags::empty());
            return Err(io(&self.at(name), e));
        }
        self.sync()?;
        Ok(written)
    }

    /// Remove `name`, which must be what `expect` says; the dir fsynced.
    pub fn remove(&self, name: &OsStr, expect: Expect) -> Result<(), WriteError> {
        self.check(name, expect)?;
        rustix::fs::unlinkat(&self.fd, name, AtFlags::empty())
            .map_err(|e| io(&self.at(name), e))?;
        self.sync()
    }

    /// Give `name` (what `expect` says) the new name `to` here, never over
    /// a file that is there (a hard link, then the old name removed).
    pub fn rename_aside(&self, name: &OsStr, expect: Expect, to: &OsStr) -> Result<(), WriteError> {
        self.check(name, expect)?;
        rustix::fs::linkat(&self.fd, name, &self.fd, to, AtFlags::empty())
            .map_err(|e| io(&self.at(to), e))?;
        if let Err(e) = rustix::fs::unlinkat(&self.fd, name, AtFlags::empty()) {
            let _ = rustix::fs::unlinkat(&self.fd, to, AtFlags::empty());
            return Err(io(&self.at(name), e));
        }
        self.sync()
    }
}

fn temp_name() -> OsString {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    OsString::from(format!(
        "{TEMP_PREFIX}{}-{n}-{nanos:09}.tmp",
        std::process::id()
    ))
}

fn timespec(t: SystemTime) -> Timespec {
    let (sec, nsec) = match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
        Err(e) => {
            let d = e.duration();
            let mut sec = -(d.as_secs() as i64);
            let mut nsec = d.subsec_nanos();
            if nsec > 0 {
                sec -= 1;
                nsec = 1_000_000_000 - nsec;
            }
            (sec, nsec)
        }
    };
    Timespec {
        tv_sec: sec,
        tv_nsec: nsec.into(),
    }
}

fn system_time(st: &Stat) -> SystemTime {
    let ns = stat_mtime_ns(st);
    if ns >= 0 {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(ns as u64)
    } else {
        SystemTime::UNIX_EPOCH - std::time::Duration::from_nanos((-ns) as u64)
    }
}

/// Split an absolute file path into its dir and name.
pub fn split(path: &Path) -> Option<(&Path, &OsStr)> {
    Some((path.parent()?, path.file_name()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let b = t.path().canonicalize().unwrap();
        (t, b)
    }

    fn expect_now(path: &Path) -> Expect {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::symlink_metadata(path).unwrap();
        Expect::File {
            dev: m.dev(),
            ino: m.ino(),
            size: m.len(),
            mtime_ns: i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec()),
        }
    }

    fn fill(body: &'static [u8]) -> impl FnMut(&mut dyn Write) -> Result<(), WriteError> {
        move |w| w.write_all(body).map_err(|e| io(Path::new("t"), e))
    }

    #[test]
    fn creates_dirs_only_below_the_base_and_never_follows_a_link() {
        let (_t, b) = base();
        let prefix = b.join("prefix");
        assert!(matches!(
            open_dir(&prefix.join("a"), &prefix),
            Err(WriteError::Path { .. })
        ));
        std::fs::create_dir(&prefix).unwrap();
        let d = open_dir(&prefix.join("a/b"), &prefix).unwrap();
        assert!(prefix.join("a/b").is_dir());
        drop(d);
        std::fs::create_dir(b.join("outside")).unwrap();
        std::os::unix::fs::symlink(b.join("outside"), prefix.join("a/link")).unwrap();
        let e = open_dir(&prefix.join("a/link/x"), &prefix).err().unwrap();
        assert!(matches!(e, WriteError::Path { .. }), "{e}");
        assert!(!b.join("outside/x").exists());
        assert!(open_dir(&b.join("elsewhere"), &prefix).is_err());
    }

    #[test]
    fn replace_is_atomic_keeps_the_mode_and_sets_the_mtime() {
        use std::os::unix::fs::PermissionsExt;
        let (_t, b) = base();
        let d = open_dir(&b, &b).unwrap();
        let p = b.join("s.sav");
        std::fs::write(&p, b"old").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let (size, mtime) = d
            .replace(
                OsStr::new("s.sav"),
                expect_now(&p),
                Some(t),
                &mut fill(b"new!"),
                &|| false,
            )
            .unwrap();
        assert_eq!((size, mtime), (4, t));
        assert_eq!(std::fs::read(&p).unwrap(), b"new!");
        let m = std::fs::metadata(&p).unwrap();
        assert_eq!(m.permissions().mode() & 0o777, 0o600);
        assert_eq!(m.modified().unwrap(), t);
        assert_eq!(std::fs::read_dir(&b).unwrap().count(), 1, "no temp left");
    }

    #[test]
    fn a_stop_before_the_rename_leaves_the_old_file_and_a_temp_the_next_run_clears() {
        let (_t, b) = base();
        let d = open_dir(&b, &b).unwrap();
        let p = b.join("s.sav");
        std::fs::write(&p, b"old").unwrap();
        let e = d
            .replace(
                OsStr::new("s.sav"),
                expect_now(&p),
                None,
                &mut fill(b"new"),
                &|| true,
            )
            .err()
            .unwrap();
        assert!(matches!(e, WriteError::Io { .. }));
        assert_eq!(std::fs::read(&p).unwrap(), b"old");
        let names: Vec<String> = std::fs::read_dir(&b)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2);
        let temp = names.iter().find(|n| n.starts_with(TEMP_PREFIX)).unwrap();
        assert!(super::super::locations::excluded(temp), "never synced");
        d.clear_temps();
        assert_eq!(std::fs::read_dir(&b).unwrap().count(), 1);
    }

    #[test]
    fn a_failed_fill_leaves_the_old_file_and_no_temp() {
        let (_t, b) = base();
        let d = open_dir(&b, &b).unwrap();
        let p = b.join("s.sav");
        std::fs::write(&p, b"old").unwrap();
        let e = d
            .replace(
                OsStr::new("s.sav"),
                expect_now(&p),
                None,
                &mut |w: &mut dyn Write| {
                    w.write_all(b"half").unwrap();
                    Err(io(Path::new("x"), "corrupt"))
                },
                &|| false,
            )
            .err()
            .unwrap();
        assert!(matches!(e, WriteError::Io { .. }));
        assert_eq!(std::fs::read(&p).unwrap(), b"old");
        assert_eq!(std::fs::read_dir(&b).unwrap().count(), 1);
    }

    #[test]
    fn a_changed_or_swapped_target_is_refused() {
        let (_t, b) = base();
        let d = open_dir(&b, &b).unwrap();
        let p = b.join("s.sav");
        std::fs::write(&p, b"old").unwrap();
        let before = expect_now(&p);
        std::fs::write(&p, b"other").unwrap();
        let e = d
            .replace(OsStr::new("s.sav"), before, None, &mut fill(b"n"), &|| {
                false
            })
            .err()
            .unwrap();
        assert!(matches!(e, WriteError::Changed { .. }));
        // Planned absent, there now.
        let e = d
            .replace(
                OsStr::new("s.sav"),
                Expect::Absent,
                None,
                &mut fill(b"n"),
                &|| false,
            )
            .err()
            .unwrap();
        assert!(matches!(e, WriteError::Changed { .. }));
        // A link where the file was: not followed, not replaced.
        std::fs::write(b.join("target"), b"t").unwrap();
        std::os::unix::fs::symlink(b.join("target"), b.join("l.sav")).unwrap();
        assert!(d.remove(OsStr::new("l.sav"), before).is_err());
        assert!(d.open_read(OsStr::new("l.sav"), before).is_err());
        assert_eq!(std::fs::read(b.join("target")).unwrap(), b"t");
        assert_eq!(std::fs::read(&p).unwrap(), b"other");
    }

    #[test]
    fn rename_aside_never_overwrites() {
        let (_t, b) = base();
        let d = open_dir(&b, &b).unwrap();
        let p = b.join("s.sav");
        std::fs::write(&p, b"mine").unwrap();
        std::fs::write(b.join("taken"), b"x").unwrap();
        assert!(
            d.rename_aside(OsStr::new("s.sav"), expect_now(&p), OsStr::new("taken"))
                .is_err()
        );
        assert_eq!(std::fs::read(b.join("taken")).unwrap(), b"x");
        d.rename_aside(
            OsStr::new("s.sav"),
            expect_now(&p),
            OsStr::new("s.sav.local"),
        )
        .unwrap();
        assert!(!p.exists());
        assert_eq!(std::fs::read(b.join("s.sav.local")).unwrap(), b"mine");
    }
}
