//! Snapshots of a built Linux game (design 20): the game dir as the
//! filesystem's own snapshot, plus the metadata and the L4 override copy gogo
//! keeps with it.
//!
//! - [`backend`]: the [`SnapshotBackend`] trait and its types; [`btrfs`] (Linux)
//!   implements it, `fake` stands in for it in tests.
//! - This file: metadata under `<data_root>/snapshots/<gogid>/` (§4), create
//!   and delete with crash recovery (§5.2), the reconciliation of what is on
//!   disk with the metadata, retention (§7.2) and the stored space (§7.3).
//!
//! Locks and refusals (§5.1) are the caller's: every operation that changes
//! anything takes a [`Guard`], which the caller can only build from the two
//! lock files it holds.

pub mod backend;
#[cfg(target_os = "linux")]
pub mod btrfs;
#[cfg(any(test, feature = "test-util"))]
pub mod fake;
pub mod saves;

use std::fs::File;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, SubsecRound as _, Utc};
use serde::{Deserialize, Serialize};

pub use backend::{BackendKind, OnDisk, RollbackStep, SnapshotBackend, Space, Support};

/// What the filesystem under `dir` offers for snapshots (design 20 §2). `dir`
/// may not exist yet: its nearest existing ancestor is checked.
pub fn probe(dir: &Path) -> Support {
    #[cfg(target_os = "linux")]
    {
        btrfs::probe(dir)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = dir;
        Support::None("snapshots need Linux".into())
    }
}

/// The backend for a [`Support`], `None` where there is none (yet).
pub fn backend_for(support: &Support) -> Option<&'static dyn SnapshotBackend> {
    match support {
        #[cfg(target_os = "linux")]
        Support::Btrfs => Some(&btrfs::Btrfs),
        _ => None,
    }
}

// — metadata types (design 20 §4) —

/// What made a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// After the first build and setup, or after a move to another filesystem.
    Initial,
    /// `snapshot create`.
    Manual,
    /// Before a change gogo makes; its [`Trigger`] says which.
    Auto,
}

/// The change an `auto` snapshot was taken before (design 20 §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Trigger {
    DlcAdd,
    Update,
    Rebuild,
    DlcRemove,
    Runtime,
    Rollback,
    CloudPull,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::DlcAdd => "dlc-add",
            Trigger::Update => "update",
            Trigger::Rebuild => "rebuild",
            Trigger::DlcRemove => "dlc-remove",
            Trigger::Runtime => "runtime",
            Trigger::Rollback => "rollback",
            Trigger::CloudPull => "cloud-pull",
        }
    }
}

/// A snapshot's state: `creating` and `deleting` are only ever seen after a
/// crash, and [`Store::recover`] settles them (design 20 §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Creating,
    Done,
    Deleting,
}

/// Whether the L4 override was copied with the snapshot (`<n>.override.toml`)
/// or didn't exist then.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OverrideCopy {
    Copied,
    Absent,
}

/// The game's build and setup when the snapshot was taken, from its
/// `content.toml` and `state.toml` (the caller reads them).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Content {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default)]
    pub dlcs: Vec<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<String>,
}

/// The last space measurement (design 20 §7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceRecord {
    #[serde(with = "toml_time")]
    pub checked: DateTime<Utc>,
    pub exclusive_bytes: u64,
    pub exact: bool,
}

/// `<n>.toml` (design 20 §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub id: u32,
    #[serde(with = "toml_time")]
    pub created: DateTime<Utc>,
    pub kind: Kind,
    /// `auto` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<Trigger>,
    pub label: String,
    pub state: State,
    pub backend: BackendKind,
    /// Where the game was when it was taken.
    pub game_dir: PathBuf,
    #[serde(rename = "override")]
    pub override_copy: OverrideCopy,
    #[serde(default)]
    pub content: Content,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<SpaceRecord>,
}

/// `settings.toml`: the per-game autosnapshot setting and the next number.
/// A game without the file (built before snapshots) is `auto = false`
/// (design 20 §4, §9.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub auto: bool,
    #[serde(default = "first")]
    pub next: u32,
}

fn first() -> u32 {
    1
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto: false,
            next: 1,
        }
    }
}

/// TOML's own datetime (`created = 2026-11-12T10:41:07Z`, not a string) for a
/// UTC time, to the second.
mod toml_time {
    use chrono::{DateTime, SecondsFormat, Utc};
    use serde::{Deserialize as _, Deserializer, Serialize as _, Serializer, de, ser};

    pub fn serialize<S: Serializer>(t: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
        let dt: toml::value::Datetime = t
            .to_rfc3339_opts(SecondsFormat::Secs, true)
            .parse()
            .map_err(ser::Error::custom)?;
        dt.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error> {
        let dt = toml::value::Datetime::deserialize(d)?;
        DateTime::parse_from_rfc3339(&dt.to_string())
            .map(|t| t.with_timezone(&Utc))
            .map_err(de::Error::custom)
    }
}

// — labels (design 20 §4, §7.1) —

/// gogo's labels for `initial` and `auto` snapshots. A `manual` snapshot's
/// label is the user's `--label`, else `manual`.
pub mod labels {
    pub fn first_build() -> String {
        "after the first build".into()
    }
    pub fn moved(dir: &std::path::Path) -> String {
        format!("after moving to {}", dir.display())
    }
    pub fn adding(titles: &[&str]) -> String {
        format!("before adding {}", titles.join(", "))
    }
    pub fn update(old: &str, new: &str) -> String {
        format!("before update {old} → {new}")
    }
    pub fn rebuild() -> String {
        "before rebuild".into()
    }
    pub fn removing(title: &str) -> String {
        format!("before removing {title}")
    }
    pub fn runtime(old: &str, new: &str) -> String {
        format!("before {old} → {new}")
    }
    pub fn rollback(n: u32) -> String {
        format!("before rollback to {n}")
    }
    pub fn cloud_pull() -> String {
        "before cloud-save download".into()
    }
    pub fn manual() -> String {
        "manual".into()
    }
}

/// The label a snapshot gets when its creator gives none.
fn default_label(kind: Kind, trigger: Option<Trigger>) -> String {
    match (kind, trigger) {
        (Kind::Initial, _) => labels::first_build(),
        (Kind::Manual, _) => labels::manual(),
        (Kind::Auto, Some(Trigger::Rebuild)) => labels::rebuild(),
        (Kind::Auto, Some(Trigger::CloudPull)) => labels::cloud_pull(),
        (Kind::Auto, Some(t)) => format!("before {}", t.as_str()),
        (Kind::Auto, None) => "before a change".into(),
    }
}

// — errors, locks, crash points —

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    /// No snapshots on this filesystem; the reason in words.
    #[error("no snapshots: {0}")]
    Unsupported(String),
    /// The game dir is on a filesystem with snapshots but isn't its own
    /// subvolume (or dataset) yet: it needs converting (design 20 §2.1).
    #[error("{} isn't its own subvolume: it needs converting before it can be snapshotted", .0.display())]
    NotReady(PathBuf),
    /// The game dir's parent isn't there (a drive not mounted?).
    #[error("{} isn't reachable (is its drive mounted?)", .0.display())]
    Unreachable(PathBuf),
    #[error("there is no snapshot {0}")]
    NotFound(u32),
    /// A crashed create or delete is pending: [`Store::recover`] first.
    #[error("snapshot {0} was interrupted while {1}: recover it first")]
    Unsettled(u32, &'static str),
    #[error("{0}")]
    InvalidRequest(String),
    #[error("{}: {source}", path.display())]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("writing snapshot metadata: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Crash injection ([`Store::fail_at`]).
    #[error("injected failure after {0:?}")]
    Injected(Step),
}

pub type Result<T, E = SnapshotError> = std::result::Result<T, E>;

/// Proof that the caller holds the locks every snapshot change runs under
/// (design 20 §5.1): the data root's lock and the game's
/// `game-moves/<gogid>.lock`, both exclusively, for the whole operation, as a
/// `move` takes them. The refusals (a running game, a pending DLC lay or move,
/// an unreachable game) are the caller's too, checked before it calls in.
///
/// Holding the two files is what a [`Guard`] stands for; it can't check that
/// they are locked, so build one only from the files the lock calls returned.
#[derive(Debug, Clone, Copy)]
pub struct Guard<'a> {
    _data_root: &'a File,
    _game: &'a File,
}

impl<'a> Guard<'a> {
    pub fn new(data_root_lock: &'a File, game_lock: &'a File) -> Self {
        Self {
            _data_root: data_root_lock,
            _game: game_lock,
        }
    }
}

/// The points a create or delete can be stopped after, for crash injection
/// ([`Store::fail_at`]), in the order they happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Create: `settings.toml`'s `next` moved past the new number.
    NumberTaken,
    /// Create: `<n>.toml` written with `state = "creating"`.
    Recorded,
    /// Create: the L4 copy written (or nothing, when L4 is absent).
    OverrideCopied,
    /// Create: the filesystem snapshot taken.
    SnapshotTaken,
    /// Delete: `<n>.toml` set to `state = "deleting"`.
    DeleteMarked,
    /// Delete: the filesystem snapshot removed.
    DeleteRemoved,
    /// Delete: the L4 copy removed.
    OverrideDropped,
}

impl Step {
    pub const CREATE: [Step; 4] = [
        Step::NumberTaken,
        Step::Recorded,
        Step::OverrideCopied,
        Step::SnapshotTaken,
    ];
    pub const DELETE: [Step; 3] = [
        Step::DeleteMarked,
        Step::DeleteRemoved,
        Step::OverrideDropped,
    ];
}

// — the store —

/// A snapshot to take.
#[derive(Debug, Clone)]
pub struct NewSnapshot {
    pub kind: Kind,
    /// Required for [`Kind::Auto`], refused for the others.
    pub trigger: Option<Trigger>,
    /// `None` (or blank): the default for the kind and trigger.
    pub label: Option<String>,
    /// The game's build and setup now (from `content.toml` and `state.toml`).
    pub content: Content,
}

/// Where a snapshot is, against its metadata (design 20 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// On disk and in the metadata.
    Present,
    /// In the metadata, not on disk (`missing`; `doctor` warns).
    Missing,
    /// On disk with no metadata (`?`): a data root wiped or restored.
    Stray,
    /// In the metadata, and the game dir's drive isn't there to look.
    Unreachable,
}

/// One row of [`Store::list`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: u32,
    /// `None` for a [`Presence::Stray`] snapshot.
    pub meta: Option<Meta>,
    pub presence: Presence,
    /// The filesystem's date, for a stray one (or any on disk).
    pub fs_created: Option<SystemTime>,
}

/// [`Store::list`]: the setting and every snapshot, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub settings: Settings,
    pub reachable: bool,
    pub entries: Vec<Entry>,
}

/// What [`Store::recover`] did with one interrupted snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovered {
    /// A create whose snapshot was taken: marked done.
    Finished(u32),
    /// A create stopped before its snapshot: the entry dropped.
    Dropped(u32),
    /// A delete carried on to the end.
    Deleted(u32),
}

/// One game's snapshots: the metadata dir `<data_root>/snapshots/<gogid>/` and
/// the backend that holds the snapshots of `game_dir`.
pub struct Store<'b> {
    dir: PathBuf,
    override_path: PathBuf,
    game_dir: PathBuf,
    backend: &'b dyn SnapshotBackend,
    fail_at: Option<Step>,
}

impl<'b> Store<'b> {
    /// The snapshots of game `gogid`, whose dir is `game_dir`, under
    /// `data_root` (`state::data_root()` outside tests).
    pub fn new(
        data_root: &Path,
        gogid: &str,
        game_dir: &Path,
        backend: &'b dyn SnapshotBackend,
    ) -> Self {
        Self {
            dir: data_root.join("snapshots").join(gogid),
            override_path: data_root.join("compat").join(format!("{gogid}.toml")),
            game_dir: game_dir.to_path_buf(),
            backend,
            fail_at: None,
        }
    }

    /// Crash injection: stop right after `step`, as a crash there would,
    /// leaving everything as it is (design 20 §10.2).
    #[doc(hidden)]
    pub fn fail_at(mut self, step: Option<Step>) -> Self {
        self.fail_at = step;
        self
    }

    /// `<data_root>/snapshots/<gogid>/`.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn game_dir(&self) -> &Path {
        &self.game_dir
    }

    fn step(&self, s: Step) -> Result<()> {
        if self.fail_at == Some(s) {
            return Err(SnapshotError::Injected(s));
        }
        Ok(())
    }

    fn settings_path(&self) -> PathBuf {
        self.dir.join("settings.toml")
    }

    fn meta_path(&self, n: u32) -> PathBuf {
        self.dir.join(format!("{n}.toml"))
    }

    /// `rollback.json`: the journal of a rollback in progress (design 20
    /// §5.3.1).
    pub fn rollback_journal_path(&self) -> PathBuf {
        self.dir.join("rollback.json")
    }

    /// `<n>.override.toml`: the L4 copy taken with snapshot `n`.
    pub fn override_copy_path(&self, n: u32) -> PathBuf {
        self.dir.join(format!("{n}.override.toml"))
    }

    fn reachable(&self) -> bool {
        self.game_dir.parent().is_some_and(Path::is_dir)
    }

    fn ensure_reachable(&self) -> Result<()> {
        if self.reachable() {
            Ok(())
        } else {
            Err(SnapshotError::Unreachable(self.game_dir.clone()))
        }
    }

    // — settings —

    /// `settings.toml`, or the default for a game without one (`auto = false`).
    pub fn settings(&self) -> Result<Settings> {
        Ok(read_toml(&self.settings_path())?.unwrap_or_default())
    }

    /// Turn autosnapshot on or off. Taking no snapshot and converting nothing:
    /// `auto on`'s conversion is the caller's (design 20 §8).
    pub fn set_auto(&self, _guard: &Guard, auto: bool) -> Result<()> {
        let mut s = self.settings()?;
        s.auto = auto;
        write_toml(&self.settings_path(), &s)
    }

    // — metadata —

    /// `<n>.toml`, if there is one.
    pub fn load(&self, n: u32) -> Result<Option<Meta>> {
        read_toml(&self.meta_path(n))
    }

    fn save(&self, meta: &Meta) -> Result<()> {
        write_toml(&self.meta_path(meta.id), meta)
    }

    /// Every `<n>.toml`, by number.
    pub fn metadata(&self) -> Result<Vec<Meta>> {
        let mut out = Vec::new();
        for n in self.meta_numbers()? {
            if let Some(m) = self.load(n)? {
                out.push(m);
            }
        }
        Ok(out)
    }

    fn meta_numbers(&self) -> Result<Vec<u32>> {
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut ns = Vec::new();
        for entry in rd {
            let name = entry?.file_name();
            let Some(stem) = name.to_str().and_then(|s| s.strip_suffix(".toml")) else {
                continue;
            };
            if let Some(n) = backend::parse_n(std::ffi::OsStr::new(stem)) {
                ns.push(n);
            }
        }
        ns.sort_unstable();
        Ok(ns)
    }

    // — list and reconcile —

    /// The setting and every snapshot, newest first: the metadata reconciled
    /// with what the backend finds on disk (design 20 §4).
    pub fn list(&self) -> Result<Listing> {
        let settings = self.settings()?;
        let metas = self.metadata()?;
        let on_disk = match self.backend.on_disk(&self.game_dir) {
            Ok(d) => Some(d),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let reachable = on_disk.is_some() && self.reachable();
        let mut entries: Vec<Entry> = Vec::new();
        for meta in metas {
            let found = on_disk
                .as_ref()
                .and_then(|d| d.iter().find(|o| o.n == meta.id));
            let presence = match (&on_disk, found) {
                (None, _) => Presence::Unreachable,
                (Some(_), Some(_)) => Presence::Present,
                (Some(_), None) => Presence::Missing,
            };
            entries.push(Entry {
                id: meta.id,
                fs_created: found.and_then(|o| o.created),
                meta: Some(meta),
                presence,
            });
        }
        for o in on_disk.iter().flatten() {
            if !entries.iter().any(|e| e.id == o.n) {
                entries.push(Entry {
                    id: o.n,
                    meta: None,
                    presence: Presence::Stray,
                    fs_created: o.created,
                });
            }
        }
        entries.sort_by_key(|e| std::cmp::Reverse(e.id));
        Ok(Listing {
            settings,
            reachable,
            entries,
        })
    }

    // — create —

    /// Take a snapshot (design 20 §5.2): `next` moved past its number, `<n>.toml`
    /// as `creating`, the L4 copy, the filesystem snapshot, `done`. Retention
    /// is the caller's next step ([`Self::prune`] after an `auto` one).
    ///
    /// Refuses on a filesystem without snapshots, a game dir that needs
    /// converting, an unreachable game, and while an interrupted create or
    /// delete waits for [`Self::recover`].
    pub fn create(&self, _guard: &Guard, req: NewSnapshot) -> Result<Meta> {
        match (req.kind, req.trigger) {
            (Kind::Auto, None) => {
                return Err(SnapshotError::InvalidRequest(
                    "an automatic snapshot needs its trigger".into(),
                ));
            }
            (Kind::Initial | Kind::Manual, Some(_)) => {
                return Err(SnapshotError::InvalidRequest(
                    "only automatic snapshots have a trigger".into(),
                ));
            }
            _ => {}
        }
        self.ensure_reachable()?;
        if let Support::None(reason) = self.backend.probe(&self.game_dir) {
            return Err(SnapshotError::Unsupported(reason));
        }
        if !self.backend.ready(&self.game_dir)? {
            return Err(SnapshotError::NotReady(self.game_dir.clone()));
        }
        let metas = self.metadata()?;
        self.refuse_unsettled(&metas)?;

        // The number: never one in use, in the metadata or on disk (a `?`
        // snapshot keeps its number), and never one handed out before.
        let mut settings = self.settings()?;
        let on_disk = self.backend.on_disk(&self.game_dir)?;
        let n = [
            settings.next,
            metas.iter().map(|m| m.id + 1).max().unwrap_or(1),
            on_disk.iter().map(|o| o.n + 1).max().unwrap_or(1),
        ]
        .into_iter()
        .max()
        .unwrap_or(1);
        settings.next = n + 1;
        write_toml(&self.settings_path(), &settings)?;
        self.step(Step::NumberTaken)?;

        let l4 = match std::fs::read(&self.override_path) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let label = req
            .label
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map_or_else(|| default_label(req.kind, req.trigger), String::from);
        let mut meta = Meta {
            id: n,
            created: Utc::now().trunc_subsecs(0),
            kind: req.kind,
            trigger: req.trigger,
            label,
            state: State::Creating,
            backend: self.backend.kind(),
            game_dir: self.game_dir.clone(),
            override_copy: if l4.is_some() {
                OverrideCopy::Copied
            } else {
                OverrideCopy::Absent
            },
            content: req.content,
            space: None,
        };
        self.save(&meta)?;
        self.step(Step::Recorded)?;

        match &l4 {
            Some(bytes) => write_atomic(&self.override_copy_path(n), bytes)?,
            None => remove_file(&self.override_copy_path(n))?,
        }
        self.step(Step::OverrideCopied)?;

        if let Err(e) = self.backend.snapshot(&self.game_dir, n) {
            // Nothing on disk: undo the entry now rather than leave it for
            // recovery. Something on disk: recovery decides.
            if !self.backend.exists(&self.game_dir, n).unwrap_or(true) {
                let _ = self.drop_entry(n);
            }
            return Err(e.into());
        }
        self.step(Step::SnapshotTaken)?;

        meta.state = State::Done;
        self.save(&meta)?;
        Ok(meta)
    }

    fn refuse_unsettled(&self, metas: &[Meta]) -> Result<()> {
        for m in metas {
            match m.state {
                State::Creating => return Err(SnapshotError::Unsettled(m.id, "being created")),
                State::Deleting => return Err(SnapshotError::Unsettled(m.id, "being deleted")),
                State::Done => {}
            }
        }
        Ok(())
    }

    /// The L4 copy, then `<n>.toml`: a crash between leaves an entry
    /// recovery still knows what to do with.
    fn drop_entry(&self, n: u32) -> Result<()> {
        remove_file(&self.override_copy_path(n))?;
        self.step(Step::OverrideDropped)?;
        remove_file(&self.meta_path(n))?;
        Ok(())
    }

    // — delete —

    /// Delete snapshot `n` (design 20 §5.2): `deleting`, the filesystem
    /// snapshot, the L4 copy, the entry. A stray one (on disk, no metadata)
    /// is removed from disk; a missing one (metadata only) loses its metadata.
    /// Returns the metadata it had.
    pub fn delete(&self, _guard: &Guard, n: u32) -> Result<Option<Meta>> {
        self.ensure_reachable()?;
        let meta = self.load(n)?;
        match &meta {
            Some(m) if m.state == State::Creating => {
                return Err(SnapshotError::Unsettled(n, "being created"));
            }
            Some(_) => {}
            None if self.backend.exists(&self.game_dir, n)? => {}
            None => return Err(SnapshotError::NotFound(n)),
        }
        self.finish_delete(n, meta.clone())?;
        Ok(meta)
    }

    fn finish_delete(&self, n: u32, meta: Option<Meta>) -> Result<()> {
        if let Some(mut m) = meta
            && m.state != State::Deleting
        {
            m.state = State::Deleting;
            self.save(&m)?;
        }
        self.step(Step::DeleteMarked)?;
        self.backend.delete(&self.game_dir, n)?;
        self.step(Step::DeleteRemoved)?;
        self.drop_entry(n)
    }

    // — recovery —

    /// Settle what a crash left (design 20 §5.2): a `creating` entry whose
    /// snapshot exists is finished, one without is dropped; a `deleting` one
    /// is carried on. Also clears leftover `.tmp` files and L4 copies without
    /// an entry. Refuses for an unreachable game, whose snapshots can't be
    /// looked at.
    pub fn recover(&self, _guard: &Guard) -> Result<Vec<Recovered>> {
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut done = Vec::new();
        let metas = self.metadata()?;
        if metas.iter().any(|m| m.state != State::Done) {
            self.ensure_reachable()?;
        }
        for entry in rd {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.ends_with(".tmp") {
                remove_file(&entry.path())?;
            } else if let Some(stem) = name.strip_suffix(".override.toml")
                && let Some(n) = backend::parse_n(std::ffi::OsStr::new(stem))
                && !metas.iter().any(|m| m.id == n)
            {
                remove_file(&entry.path())?;
            }
        }
        for mut m in metas {
            match m.state {
                State::Done => {}
                State::Creating => {
                    if self.backend.exists(&self.game_dir, m.id)? {
                        m.state = State::Done;
                        self.save(&m)?;
                        done.push(Recovered::Finished(m.id));
                    } else {
                        self.drop_entry(m.id)?;
                        done.push(Recovered::Dropped(m.id));
                    }
                }
                State::Deleting => {
                    let id = m.id;
                    self.finish_delete(id, Some(m))?;
                    done.push(Recovered::Deleted(id));
                }
            }
        }
        Ok(done)
    }

    // — retention (design 20 §7.2) —

    /// Delete the `auto` snapshots beyond the newest `keep_auto`, oldest
    /// first. `initial` and `manual` ones are never touched. Returns what it
    /// deleted, in order.
    pub fn prune(&self, guard: &Guard, keep_auto: usize) -> Result<Vec<Meta>> {
        self.prune_keeping(guard, keep_auto, &[])
    }

    /// [`Self::prune`], never deleting the snapshots in `keep` (and not
    /// counting them): a rollback keeps its target and the snapshot it took
    /// before itself, whatever `keep_auto` says.
    pub fn prune_keeping(
        &self,
        guard: &Guard,
        keep_auto: usize,
        keep: &[u32],
    ) -> Result<Vec<Meta>> {
        let mut autos: Vec<Meta> = self
            .metadata()?
            .into_iter()
            .filter(|m| m.kind == Kind::Auto && m.state == State::Done && !keep.contains(&m.id))
            .collect();
        autos.sort_by_key(|m| std::cmp::Reverse(m.id));
        let mut old: Vec<Meta> = autos.into_iter().skip(keep_auto).collect();
        old.sort_by_key(|m| m.id);
        let mut pruned = Vec::new();
        for m in old {
            self.delete(guard, m.id)?;
            pruned.push(m);
        }
        Ok(pruned)
    }

    // — space (design 20 §7.3) —

    /// Measure what each snapshot on disk holds alone and store it in its
    /// `<n>.toml`'s `[space]`. Stray snapshots are measured (they share data
    /// with the others) but have nowhere to store it.
    pub fn measure_space(&self, _guard: &Guard) -> Result<Vec<Space>> {
        self.ensure_reachable()?;
        let ns: Vec<u32> = self
            .backend
            .on_disk(&self.game_dir)?
            .iter()
            .map(|o| o.n)
            .collect();
        let spaces = self.backend.space(&self.game_dir, &ns)?;
        let checked = Utc::now().trunc_subsecs(0);
        for s in &spaces {
            if let Some(mut m) = self.load(s.n)?
                && m.state == State::Done
            {
                m.space = Some(SpaceRecord {
                    checked,
                    exclusive_bytes: s.exclusive_bytes,
                    exact: s.exact,
                });
                self.save(&m)?;
            }
        }
        Ok(spaces)
    }
}

/// Every snapshot's metadata under `<data_root>/snapshots/`, as `(gogid,
/// <n>.toml, metadata)`, sorted. Reads only; a file that doesn't parse is
/// skipped. For the readers that look across games: the store's GC counts each
/// snapshot's runtime and renderer as in use (design 20 §9.6).
pub fn all_metadata(data_root: &Path) -> Vec<(String, PathBuf, Meta)> {
    let mut out = Vec::new();
    let games = std::fs::read_dir(data_root.join("snapshots"));
    for game in games.into_iter().flatten().flatten() {
        let Some(gogid) = game.file_name().to_str().map(str::to_string) else {
            continue;
        };
        for file in std::fs::read_dir(game.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let path = file.path();
            let numbered = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".toml"))
                .and_then(|n| backend::parse_n(std::ffi::OsStr::new(n)))
                .is_some();
            if numbered && let Ok(Some(meta)) = read_toml::<Meta>(&path) {
                out.push((gogid.clone(), path, meta));
            }
        }
    }
    out.sort_by(|a, b| (&a.0, a.2.id).cmp(&(&b.0, b.2.id)));
    out
}

// — atomic files (design 06 §2) —

fn read_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    toml::from_str(&text)
        .map(Some)
        .map_err(|source| SnapshotError::Parse {
            path: path.to_path_buf(),
            source,
        })
}

fn write_toml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = toml::to_string(value)?;
    Ok(write_atomic(path, text.as_bytes())?)
}

/// Design 06 §2: `<file>.tmp`, fsync, rename over the target, fsync the dir
/// (best effort). A leftover `.tmp` is cleared by [`Store::recover`].
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    sync_dir(dir);
    Ok(())
}

/// Remove `path`; gone already is fine.
pub(crate) fn remove_file(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {
            if let Some(dir) = path.parent() {
                sync_dir(dir);
            }
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

#[cfg(test)]
mod tests;
