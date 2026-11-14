//! Backups of local save files before a pull overwrites or deletes them
//! (design 22 §7.1, D7): `<data_root>/cloud-saves/backups/<gogid>/<UTC
//! timestamp>/local/<location>/<path>`, the newest `keep` dirs per game kept.
//! Every file is backed up, empty ones too: a tombstone may stand for
//! someone's real empty save (design 22 §12b).

use std::fs::File;
use std::path::{Path, PathBuf};

use super::write::WriteError;

/// A run's backup dir, made at its first file.
pub struct Backup {
    root: PathBuf,
    stamp: String,
    dir: Option<PathBuf>,
    /// Files backed up so far.
    pub files: usize,
}

fn io(path: &Path, e: impl std::fmt::Display) -> WriteError {
    WriteError::Io {
        path: path.display().to_string(),
        reason: e.to_string(),
    }
}

fn sync_dir(dir: &Path) -> Result<(), WriteError> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| io(dir, e))
}

/// `<record_dir>/backups/<gogid>`.
pub fn game_dir(record_dir: &Path, gogid: u64) -> PathBuf {
    record_dir.join("backups").join(gogid.to_string())
}

/// A backup dir's name: a UTC timestamp, `-<n>` when one is taken already.
fn is_backup_name(n: &str) -> bool {
    let (stamp, suffix) = n.split_once('-').unwrap_or((n, "1"));
    stamp.len() == 16
        && stamp.as_bytes()[8] == b'T'
        && stamp.ends_with('Z')
        && stamp
            .bytes()
            .enumerate()
            .all(|(i, b)| matches!(i, 8 | 15) || b.is_ascii_digit())
        && !suffix.is_empty()
        && suffix.bytes().all(|b| b.is_ascii_digit())
}

/// Sort key: the stamp, then the suffix as a number (`…Z` < `…Z-2` < `…Z-10`).
fn order_key(n: &str) -> (String, u64) {
    match n.split_once('-') {
        Some((s, k)) => (s.to_string(), k.parse().unwrap_or(0)),
        None => (n.to_string(), 1),
    }
}

impl Backup {
    pub fn new(root: PathBuf, now: chrono::DateTime<chrono::Utc>) -> Backup {
        Backup {
            root,
            stamp: now.format("%Y%m%dT%H%M%SZ").to_string(),
            dir: None,
            files: 0,
        }
    }

    /// The dir, once something was backed up.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    fn ensure(&mut self) -> Result<PathBuf, WriteError> {
        if let Some(d) = &self.dir {
            return Ok(d.clone());
        }
        std::fs::create_dir_all(&self.root).map_err(|e| io(&self.root, e))?;
        let mut n = 1;
        loop {
            let name = if n == 1 {
                self.stamp.clone()
            } else {
                format!("{}-{n}", self.stamp)
            };
            let d = self.root.join(name);
            match std::fs::create_dir(&d) {
                Ok(()) => {
                    sync_dir(&self.root)?;
                    self.dir = Some(d.clone());
                    return Ok(d);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(io(&d, e)),
            }
        }
    }

    /// Copy `src` (opened by the caller without following a link) to
    /// `local/<name>` here, fsynced. `name` is the object name,
    /// `<location>/<path>`; `/` and `\` both separate.
    pub fn save(&mut self, name: &str, src: &mut File) -> Result<PathBuf, WriteError> {
        let dir = self.ensure()?;
        let mut dst = dir.join("local");
        for part in name.split(['/', '\\']).filter(|p| !p.is_empty()) {
            if part == "." || part == ".." {
                return Err(WriteError::Path {
                    path: name.to_string(),
                    reason: "not a plain name".into(),
                });
            }
            dst.push(part);
        }
        let parent = dst.parent().expect("below the dir").to_path_buf();
        std::fs::create_dir_all(&parent).map_err(|e| io(&parent, e))?;
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dst)
            .map_err(|e| io(&dst, e))?;
        // `copy_file_range` underneath, which btrfs may answer with a clone.
        std::io::copy(src, &mut out).map_err(|e| io(&dst, e))?;
        out.sync_all().map_err(|e| io(&dst, e))?;
        sync_dir(&parent)?;
        self.files += 1;
        Ok(dst)
    }
}

/// Keep the newest `keep` backup dirs of a game (at least one), oldest
/// removed first; the removed ones. Only dirs with a backup's name are
/// counted or touched. A failure to remove one is a warning in the list.
pub fn prune(root: &Path, keep: usize) -> (Vec<PathBuf>, Vec<String>) {
    let keep = keep.max(1);
    let Ok(rd) = std::fs::read_dir(root) else {
        return (vec![], vec![]);
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| is_backup_name(n))
        .collect();
    names.sort_by_key(|n| order_key(n));
    let drop = names.len().saturating_sub(keep);
    let mut removed = vec![];
    let mut warnings = vec![];
    for n in &names[..drop] {
        let p = root.join(n);
        match std::fs::remove_dir_all(&p) {
            Ok(()) => removed.push(p),
            Err(e) => warnings.push(format!("{}: not pruned ({e})", p.display())),
        }
    }
    (removed, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_order_and_prune_oldest_first() {
        assert!(is_backup_name("20261114T101500Z"));
        assert!(is_backup_name("20261114T101500Z-2"));
        assert!(!is_backup_name("notes"));
        assert!(!is_backup_name("20261114T101500Z-"));
        let t = tempfile::tempdir().unwrap();
        for n in [
            "20261114T101500Z-10",
            "20261114T101500Z",
            "20261114T101500Z-2",
            "20261113T000000Z",
            "keep-me",
        ] {
            std::fs::create_dir(t.path().join(n)).unwrap();
        }
        let (removed, w) = prune(t.path(), 2);
        assert!(w.is_empty());
        let names: Vec<_> = removed
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["20261113T000000Z", "20261114T101500Z"]);
        assert!(t.path().join("keep-me").is_dir());
        assert!(t.path().join("20261114T101500Z-10").is_dir());
        assert_eq!(prune(t.path(), 0).0.len(), 1, "0 keeps one");
    }

    #[test]
    fn a_taken_stamp_gets_a_suffix_and_files_keep_their_path() {
        let t = tempfile::tempdir().unwrap();
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-14T10:15:00Z")
            .unwrap()
            .to_utc();
        std::fs::create_dir_all(t.path().join("20261114T101500Z")).unwrap();
        let src = t.path().join("src");
        std::fs::write(&src, b"").unwrap();
        let mut b = Backup::new(t.path().to_path_buf(), now);
        assert!(b.dir().is_none());
        let p = b
            .save("saves/Slot 1/sav.dat", &mut File::open(&src).unwrap())
            .unwrap();
        assert_eq!(
            p,
            t.path()
                .join("20261114T101500Z-2/local/saves/Slot 1/sav.dat")
        );
        assert_eq!(std::fs::read(&p).unwrap(), b"", "empty files too");
        assert!(
            b.save("saves/../x", &mut File::open(&src).unwrap())
                .is_err()
        );
    }
}
