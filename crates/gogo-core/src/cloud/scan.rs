//! The local side: the files under one save location (design 22 §3.4, §4.2).
//! Reads metadata only (hashing is the caller's, where the classification
//! needs it) and never follows a link: a link in a save dir is reported and
//! left out, so nothing outside the location is ever read as a save.

use std::path::{Path, PathBuf};

use super::error::CloudError;
use super::locations::Location;

/// How deep a save dir is walked; deeper dirs are reported, not read.
pub const MAX_DEPTH: usize = 32;

/// One regular file below a location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
    /// `/`-separated, below the location, in the case on disk.
    pub rel: String,
    pub path: PathBuf,
    pub size: u64,
    pub mtime: toml::value::Datetime,
}

/// What a location holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    pub files: Vec<LocalFile>,
    /// Left out by design 22 §3.4's excludes.
    pub excluded: usize,
    /// Links, other non-regular files, names that aren't UTF-8 and dirs past
    /// [`MAX_DEPTH`]: never synced (`/`-separated, below the location).
    pub ignored: Vec<String>,
}

fn io_error(path: &Path, e: std::io::Error) -> CloudError {
    CloudError::Io {
        path: path.display().to_string(),
        reason: e.to_string(),
    }
}

/// Walk `loc`. Paths are canonical. A location that doesn't exist yet holds nothing; one whose
/// root is a link isn't walked (the resolver already refused links out).
pub fn scan(loc: &Location) -> Result<Scan, CloudError> {
    let mut out = Scan::default();
    match std::fs::symlink_metadata(&loc.path) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            out.ignored.push(String::new());
            return Ok(out);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(io_error(&loc.path, e)),
    }
    // Canonical, so the paths match [`Location::file`]'s for the same object
    // (the prefix may be reached through a link); nothing below is followed.
    let root = loc.path.canonicalize().map_err(|e| io_error(&loc.path, e))?;
    walk(loc, &root, "", 0, &mut out)?;
    out.files.sort_by(|a, b| a.rel.cmp(&b.rel));
    out.ignored.sort();
    Ok(out)
}

fn walk(
    loc: &Location,
    dir: &Path,
    prefix: &str,
    depth: usize,
    out: &mut Scan,
) -> Result<(), CloudError> {
    let rd = std::fs::read_dir(dir).map_err(|e| io_error(dir, e))?;
    for entry in rd {
        let entry = entry.map_err(|e| io_error(dir, e))?;
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            out.ignored
                .push(format!("{prefix}{}", entry.file_name().to_string_lossy()));
            continue;
        };
        let rel = format!("{prefix}{name}");
        // `file_type` doesn't follow links.
        let ft = entry.file_type().map_err(|e| io_error(&path, e))?;
        if ft.is_dir() {
            if depth + 1 >= MAX_DEPTH {
                out.ignored.push(format!("{rel}/"));
            } else {
                walk(loc, &path, &format!("{rel}/"), depth + 1, out)?;
            }
        } else if ft.is_file() {
            if loc.excluded(&rel) {
                out.excluded += 1;
                continue;
            }
            let meta = entry.metadata().map_err(|e| io_error(&path, e))?;
            let mtime = meta.modified().map_err(|e| io_error(&path, e))?;
            out.files.push(LocalFile {
                rel,
                path,
                size: meta.len(),
                mtime: super::record::datetime(mtime),
            });
        } else {
            out.ignored.push(rel);
        }
    }
    Ok(())
}

/// The plain MD5 of a local file, read without following a link.
pub fn md5_file(path: &Path) -> Result<String, CloudError> {
    use md5::Digest;
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
        .map_err(|e| io_error(path, e))?;
    let mut h = md5::Md5::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).map_err(|e| io_error(path, e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(crate::fmt::md5_hex(h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::locations::{Lane, Layout, resolve_storage};
    use crate::cloud::remote_config::{CloudStorage, RawLocation};
    use std::fs;

    fn location(dir: &Path) -> Location {
        let prefix = dir.join("prefix");
        fs::create_dir_all(prefix.join("drive_c/users/steamuser/Documents")).unwrap();
        fs::create_dir_all(dir.join("game")).unwrap();
        let layout = Layout {
            lane: Lane::Linux,
            prefix,
            game_files: dir.join("game"),
        };
        let s = CloudStorage {
            enabled: true,
            locations: vec![RawLocation {
                name: "saves".into(),
                location: "<?DOCUMENTS?>/G".into(),
            }],
        };
        resolve_storage(Some(&s), "1", &layout).locations.remove(0)
    }

    #[test]
    fn walks_files_skips_excludes_and_never_follows_links() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let loc = location(&dir);
        assert_eq!(scan(&loc).unwrap(), Scan::default(), "not there yet");
        fs::create_dir_all(loc.path.join("Slot 1")).unwrap();
        fs::write(loc.path.join("Slot 1/sav.dat"), b"save").unwrap();
        fs::write(loc.path.join("top.sav"), b"x").unwrap();
        fs::write(loc.path.join("game.log"), b"log").unwrap();
        fs::write(loc.path.join("half.tmp"), b"t").unwrap();
        fs::write(dir.join("secret"), b"outside").unwrap();
        std::os::unix::fs::symlink(dir.join("secret"), loc.path.join("link.sav")).unwrap();
        std::os::unix::fs::symlink(&dir, loc.path.join("linkdir")).unwrap();
        let s = scan(&loc).unwrap();
        let rels: Vec<_> = s.files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, ["Slot 1/sav.dat", "top.sav"]);
        assert_eq!(s.files[0].size, 4);
        assert_eq!(s.excluded, 2);
        assert_eq!(s.ignored, ["link.sav", "linkdir"]);
        assert_eq!(
            md5_file(&s.files[1].path).unwrap(),
            crate::cloud::storage::md5_hex(b"x")
        );
        assert!(md5_file(&loc.path.join("link.sav")).is_err(), "O_NOFOLLOW");
    }
}
