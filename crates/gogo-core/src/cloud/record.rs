//! The per-game sync record (design 22 §4.1): `<data_root>/cloud-saves/<gogid>.toml`,
//! what the last sync left the same on both sides. Outside the game dir, so a
//! rollback never restores an old one.
//!
//! Written only under the game's sync lock ([`SyncLock`], design 22 §7.4) and
//! atomically. `gogo saves status` only reads it; pull and sync write it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use toml::value::Datetime;

use super::error::CloudError;

pub const SCHEMA: u32 = 1;

/// One file the last sync left the same on both sides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    /// The listing's `hash` after the sync (MD5 of the stored gzip).
    pub cloud_hash: String,
    /// MD5 of the plain file on disk after the sync.
    pub local_md5: String,
    /// Size of the plain file.
    pub size: u64,
    /// The file's mtime, to the nanosecond.
    pub mtime: Datetime,
}

/// The record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub schema: u32,
    /// The game's client id (public): a change means another edition (D11).
    pub client_id: String,
    /// Launch-time sync for this game (§6).
    #[serde(default)]
    pub auto: bool,
    /// Set by a snapshot rollback (§4.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rolled_back: Option<Datetime>,
    /// A post-exit push failed or was skipped offline (§6.3).
    #[serde(default)]
    pub pending_push: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync: Option<Datetime>,
    /// Keyed by object name (`<location>/<path>`).
    #[serde(default)]
    pub files: BTreeMap<String, FileRecord>,
}

impl Record {
    pub fn new(client_id: impl Into<String>) -> Record {
        Record {
            schema: SCHEMA,
            client_id: client_id.into(),
            auto: false,
            rolled_back: None,
            pending_push: false,
            last_sync: None,
            files: BTreeMap::new(),
        }
    }
}

/// `<data_root>/cloud-saves`.
pub fn default_dir() -> PathBuf {
    crate::state::data_root().join("cloud-saves")
}

pub fn path(dir: &Path, gogid: u64) -> PathBuf {
    dir.join(format!("{gogid}.toml"))
}

fn io_error(path: &Path, reason: impl std::fmt::Display) -> CloudError {
    CloudError::Io {
        path: path.display().to_string(),
        reason: reason.to_string(),
    }
}

/// The record of `gogid` in `dir`; `None` when there is none (a first sync).
/// A record that can't be read, or is from a newer gogo, is an error rather
/// than a first sync: it may hold a rollback mark.
pub fn load(dir: &Path, gogid: u64) -> Result<Option<Record>, CloudError> {
    let p = path(dir, gogid);
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error(&p, format!("can't be read ({e})"))),
    };
    let r: Record = toml::from_str(&text)
        .map_err(|e| io_error(&p, format!("isn't a valid sync record ({})", e.message())))?;
    if r.schema != SCHEMA {
        return Err(io_error(
            &p,
            format!(
                "sync record schema {} isn't {SCHEMA} (a newer gogo?)",
                r.schema
            ),
        ));
    }
    Ok(Some(r))
}

/// The game's sync lock, `<dir>/<gogid>.lock` (flock, design 22 §7.4): two
/// syncs of one game never run together, and the record is written only
/// while it is held.
pub struct SyncLock {
    _file: std::fs::File,
    dir: PathBuf,
    gogid: u64,
}

impl SyncLock {
    /// Take it, or fail at once when another gogo holds it.
    pub fn acquire(dir: &Path, gogid: u64) -> Result<SyncLock, CloudError> {
        std::fs::create_dir_all(dir).map_err(|e| io_error(dir, e))?;
        let p = dir.join(format!("{gogid}.lock"));
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&p)
            .map_err(|e| io_error(&p, e))?;
        match f.try_lock() {
            Ok(()) => Ok(SyncLock {
                _file: f,
                dir: dir.to_path_buf(),
                gogid,
            }),
            Err(std::fs::TryLockError::WouldBlock) => Err(io_error(
                &p,
                "another gogo is syncing this game's cloud saves",
            )),
            Err(std::fs::TryLockError::Error(e)) => Err(io_error(&p, e)),
        }
    }

    /// Write `record` atomically (tmp + fsync + rename).
    pub fn save(&self, record: &Record) -> Result<(), CloudError> {
        let p = path(&self.dir, self.gogid);
        let text = toml::to_string(record).map_err(|e| io_error(&p, e))?;
        crate::snapshot::write_atomic(&p, text.as_bytes()).map_err(|e| io_error(&p, e))
    }

    /// A rollback (design 22 §4.3, design 20 §6.3): set `rolled_back` in the
    /// game's record, so the next sync treats every difference as a conflict.
    /// `Ok(false)` when there is no record (the first sync asks anyway). A
    /// mark already set keeps its time: the earliest rollback not yet settled
    /// stands. A record that can't be read is an error: it can't be marked.
    pub fn mark_rolled_back(&self, now: std::time::SystemTime) -> Result<bool, CloudError> {
        let Some(mut record) = load(&self.dir, self.gogid)? else {
            return Ok(false);
        };
        if record.rolled_back.is_none() {
            record.rolled_back = Some(datetime(now));
            self.save(&record)?;
        }
        Ok(true)
    }

    /// D11: the game's client id changed, so the record is set aside as
    /// `<gogid>.client-changed-<UTC timestamp>.toml` and the next sync is a
    /// first sync. The new name; `None` when there was no record.
    pub fn set_aside(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<PathBuf>, CloudError> {
        let from = path(&self.dir, self.gogid);
        if !from.exists() {
            return Ok(None);
        }
        let to = self.dir.join(format!(
            "{}.client-changed-{}.toml",
            self.gogid,
            now.format("%Y%m%dT%H%M%SZ")
        ));
        if to.exists() {
            return Err(io_error(&to, "exists already"));
        }
        std::fs::rename(&from, &to).map_err(|e| io_error(&from, e))?;
        Ok(Some(to))
    }
}

/// A time as the record keeps it: UTC, to the nanosecond.
pub fn datetime(t: std::time::SystemTime) -> Datetime {
    let t: chrono::DateTime<chrono::Utc> = t.into();
    t.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
        .parse()
        .expect("RFC 3339 is a TOML datetime")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Record {
        let mut r = Record::new("11111111111111111");
        r.last_sync = Some("2026-11-13T09:58:00Z".parse().unwrap());
        r.files.insert(
            "saves/ManualSave-12/sav.dat".into(),
            FileRecord {
                cloud_hash: "0123456789abcdef0123456789abcdef".into(),
                local_md5: "fedcba9876543210fedcba9876543210".into(),
                size: 4_718_592,
                mtime: datetime(
                    std::time::UNIX_EPOCH + std::time::Duration::new(1_700_000_000, 123_456_789),
                ),
            },
        );
        r
    }

    #[test]
    fn a_rollback_marks_the_record_and_keeps_the_earliest_mark() {
        let dir = tempfile::tempdir().unwrap();
        let lock = SyncLock::acquire(dir.path(), 5).unwrap();
        // No record: nothing to mark, and none is created.
        assert!(!lock.mark_rolled_back(std::time::UNIX_EPOCH).unwrap());
        assert_eq!(load(dir.path(), 5).unwrap(), None);

        lock.save(&sample()).unwrap();
        let t1 = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        assert!(lock.mark_rolled_back(t1).unwrap());
        let marked = load(dir.path(), 5).unwrap().unwrap();
        assert_eq!(marked.rolled_back, Some(datetime(t1)));
        assert_eq!(marked.files, sample().files, "nothing else changes");

        let t2 = t1 + std::time::Duration::from_secs(60);
        assert!(lock.mark_rolled_back(t2).unwrap());
        assert_eq!(
            load(dir.path(), 5).unwrap().unwrap().rolled_back,
            Some(datetime(t1))
        );
    }

    #[test]
    fn a_record_that_cant_be_read_cant_be_marked() {
        let dir = tempfile::tempdir().unwrap();
        let lock = SyncLock::acquire(dir.path(), 5).unwrap();
        std::fs::write(path(dir.path(), 5), "not toml [").unwrap();
        assert!(lock.mark_rolled_back(std::time::UNIX_EPOCH).is_err());
        assert_eq!(
            std::fs::read_to_string(path(dir.path(), 5)).unwrap(),
            "not toml ["
        );
    }

    #[test]
    fn round_trips_under_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path(), 5).unwrap(), None);
        let lock = SyncLock::acquire(dir.path(), 5).unwrap();
        lock.save(&sample()).unwrap();
        let back = load(dir.path(), 5).unwrap().unwrap();
        assert_eq!(back, sample());
        let text = std::fs::read_to_string(path(dir.path(), 5)).unwrap();
        assert!(
            text.contains("[files.\"saves/ManualSave-12/sav.dat\"]"),
            "{text}"
        );
        assert!(
            text.contains("mtime = 2023-11-14T22:13:20.123456789Z"),
            "{text}"
        );
        assert!(!text.contains("rolled_back"));
        // No tmp file left.
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
    }

    #[test]
    fn the_lock_is_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let held = SyncLock::acquire(dir.path(), 5).unwrap();
        let e = SyncLock::acquire(dir.path(), 5).err().unwrap();
        assert!(e.to_string().contains("another gogo"));
        assert!(SyncLock::acquire(dir.path(), 6).is_ok());
        drop(held);
        assert!(SyncLock::acquire(dir.path(), 5).is_ok());
    }

    #[test]
    fn bad_and_newer_records_are_errors_not_first_syncs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path(dir.path(), 5), "schema = ").unwrap();
        assert!(matches!(load(dir.path(), 5), Err(CloudError::Io { .. })));
        std::fs::write(path(dir.path(), 5), "schema = 2\nclient_id = \"1\"\n").unwrap();
        let e = load(dir.path(), 5).unwrap_err();
        assert!(e.to_string().contains("newer gogo"));
        assert_eq!(e.exit_code(), 1);
    }

    #[test]
    fn a_client_change_sets_the_record_aside() {
        let dir = tempfile::tempdir().unwrap();
        let lock = SyncLock::acquire(dir.path(), 5).unwrap();
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-14T10:00:00Z")
            .unwrap()
            .to_utc();
        assert_eq!(lock.set_aside(now).unwrap(), None);
        lock.save(&sample()).unwrap();
        let to = lock.set_aside(now).unwrap().unwrap();
        assert_eq!(
            to.file_name().unwrap(),
            "5.client-changed-20261114T100000Z.toml"
        );
        assert_eq!(load(dir.path(), 5).unwrap(), None);
        lock.save(&sample()).unwrap();
        assert!(
            lock.set_aside(now).is_err(),
            "never overwrites an older one"
        );
    }
}
