//! The Linux lane's side of snapshots (design 20): which backend serves a game
//! dir, the build's subvolume and `initial` snapshot, `snapshot <gogid>
//! list|create|delete|auto`, the locks and refusals every change runs under
//! (§5.1), and what `apps`, `doctor` and the store's GC read. The snapshots
//! themselves, their metadata and their crash recovery are
//! [`crate::snapshot`]'s.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Local, SecondsFormat, Utc};
use serde::Serialize;

use super::convert::{self, ConvertOptions, ConvertPlan};
pub use super::convert::{ConvertProgress, Converted};
use super::game::{self, LinuxState};
use crate::engine::events::ProgressEvent;
use crate::macapp::doctor::Finding;
use crate::macapp::error::MacAppError;
use crate::macapp::launch::{self, GameDirs};
use crate::macapp::relocate;
use crate::snapshot::{
    self, BackendKind, Content, Entry, Guard, Kind, Listing, Meta, NewSnapshot, OnDisk, Presence,
    Recovered, SnapshotBackend, SnapshotError, Space, State, Store, Support, Trigger, backend,
};

/// `[snapshots] keep_auto`'s default (design 20 §7.2).
pub const KEEP_AUTO: usize = 5;

// — which backend —

/// Which backend serves a game dir: the filesystem's ([`snapshot::probe`]), or
/// one fixed for every dir (tests: the fake backend).
#[derive(Clone, Default)]
pub struct Backends {
    fixed: Option<Arc<dyn SnapshotBackend>>,
}

impl std::fmt::Debug for Backends {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.fixed.is_some() {
            "Backends(fixed)"
        } else {
            "Backends(real)"
        })
    }
}

impl Backends {
    /// The filesystem's backend for each dir.
    pub fn real() -> Self {
        Self::default()
    }

    /// `backend` for every dir, whatever the filesystem (tests).
    #[doc(hidden)]
    pub fn fixed(backend: Arc<dyn SnapshotBackend>) -> Self {
        Self {
            fixed: Some(backend),
        }
    }

    /// What the filesystem under `dir` offers (`dir` may not exist yet).
    pub fn probe(&self, dir: &Path) -> Support {
        match &self.fixed {
            Some(b) => b.probe(dir),
            None => snapshot::probe(dir),
        }
    }

    /// What the filesystem under `dir` offers, and the backend for it: one
    /// that refuses every change when there is none, so a game on ext4 still
    /// lists (`no snapshots: <reason>`).
    pub fn for_dir(&self, dir: &Path) -> (Support, &dyn SnapshotBackend) {
        let support = self.probe(dir);
        let backend: &dyn SnapshotBackend = match &self.fixed {
            Some(b) => b.as_ref(),
            None => snapshot::backend_for(&support).unwrap_or(&Unavailable),
        };
        (support, backend)
    }
}

/// The backend where the filesystem has none: lists what is there (nothing,
/// normally), takes nothing.
struct Unavailable;

impl SnapshotBackend for Unavailable {
    fn probe(&self, dir: &Path) -> Support {
        match snapshot::probe(dir) {
            Support::None(reason) => Support::None(reason),
            _ => Support::None("gogo has no snapshot backend for this filesystem yet".into()),
        }
    }

    fn kind(&self) -> BackendKind {
        BackendKind::Btrfs
    }

    fn ready(&self, _game_dir: &Path) -> std::io::Result<bool> {
        Ok(false)
    }

    fn make_game_dir(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir(dir)
    }

    fn snapshot(&self, _game_dir: &Path, _n: u32) -> std::io::Result<()> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    fn snapshot_path(&self, _game_dir: &Path, _n: u32) -> Option<PathBuf> {
        None
    }

    fn exists(&self, game_dir: &Path, n: u32) -> std::io::Result<bool> {
        Ok(backend::snapshot_dir(game_dir, n).is_some_and(|d| d.is_dir()))
    }

    fn delete(&self, _game_dir: &Path, _n: u32) -> std::io::Result<()> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    fn on_disk(&self, game_dir: &Path) -> std::io::Result<Vec<OnDisk>> {
        backend::list_dir(game_dir)
    }

    fn space(&self, _game_dir: &Path, _ns: &[u32]) -> std::io::Result<Vec<Space>> {
        Ok(Vec::new())
    }
}

pub(crate) fn err(e: SnapshotError) -> MacAppError {
    MacAppError::Apps(e.to_string())
}

pub(crate) fn store<'b>(gogid: &str, root: &Path, backend: &'b dyn SnapshotBackend) -> Store<'b> {
    Store::new(&crate::state::data_root(), gogid, root, backend)
}

// — the build (design 20 §2.1, §7.1, §8) —

/// What a build does about snapshots: the config and `--no-snapshots`, and
/// the backend.
#[derive(Debug, Clone)]
pub struct SnapshotOptions {
    /// A new game dir becomes a subvolume and gets its `initial` snapshot
    /// (where the filesystem has snapshots). Off: a plain dir, `auto = false`.
    pub on: bool,
    /// Why they're off, for the plan line (`--no-snapshots`,
    /// `[snapshots] auto = false`).
    pub off_because: String,
    /// `--no-snapshots`: on a rebuild, its `before rebuild` snapshot is
    /// skipped for this run and the game's setting stays (design 20 §7.1).
    pub no_snapshots: bool,
    /// `[snapshots] keep_auto`, for the retention after a rebuild's snapshot.
    pub keep_auto: usize,
    pub backends: Backends,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        Self {
            on: false,
            off_because: String::new(),
            no_snapshots: false,
            keep_auto: KEEP_AUTO,
            backends: Backends::real(),
        }
    }
}

impl SnapshotOptions {
    pub fn on(backends: Backends) -> Self {
        Self {
            on: true,
            backends,
            ..Self::default()
        }
    }

    pub fn off(because: impl Into<String>) -> Self {
        Self {
            off_because: because.into(),
            ..Self::default()
        }
    }

    /// `--no-snapshots`: a new game gets none; a rebuild skips its own.
    pub fn no_snapshots() -> Self {
        Self {
            no_snapshots: true,
            ..Self::off("--no-snapshots")
        }
    }

    /// What a rebuild's `auto` snapshot runs with.
    pub fn auto(&self) -> AutoOptions {
        AutoOptions {
            skip: self.no_snapshots,
            keep_auto: self.keep_auto,
            backends: self.backends.clone(),
        }
    }
}

/// The build plan's `snapshots:` line, as data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildSnapshots {
    /// A new game dir made as a subvolume, the `initial` snapshot after setup.
    On,
    /// Turned off, and why.
    Off(String),
    /// The filesystem has no snapshots, and why.
    Unavailable(String),
    /// A rebuild: the game dir stays what it is. `on`: its autosnapshot is on
    /// and the dir can be snapshotted, so `before rebuild` is taken.
    Kept { on: bool },
    /// A rebuild with `--no-snapshots`: no `before rebuild` this run.
    Skipped,
}

impl BuildSnapshots {
    /// What a build of `gogid` into `dir` does (design 20 §8). `rebuild`: the
    /// game is there already.
    pub fn plan(opts: &SnapshotOptions, gogid: Option<&str>, dir: &Path, rebuild: bool) -> Self {
        if rebuild && let Some(id) = gogid {
            if opts.no_snapshots {
                return BuildSnapshots::Skipped;
            }
            let (_, b) = opts.backends.for_dir(dir);
            let auto = store(id, dir, b).settings().is_ok_and(|s| s.auto);
            return BuildSnapshots::Kept {
                on: auto && b.ready(dir).unwrap_or(false),
            };
        }
        if !opts.on {
            let why = if opts.off_because.is_empty() {
                "not asked for".to_string()
            } else {
                opts.off_because.clone()
            };
            return BuildSnapshots::Off(why);
        }
        match opts.backends.probe(dir) {
            Support::None(reason) => BuildSnapshots::Unavailable(reason),
            Support::Btrfs => BuildSnapshots::On,
            Support::Zfs { .. } => {
                BuildSnapshots::Unavailable("ZFS: not supported yet".to_string())
            }
        }
    }

    /// The text after `snapshots:`.
    pub fn line(&self) -> String {
        match self {
            BuildSnapshots::On => "on (btrfs subvolume; first snapshot after setup; the game dir \
                                   leaves snapshots of the subvolume around it)"
                .to_string(),
            BuildSnapshots::Off(why) => format!("off ({why})"),
            BuildSnapshots::Unavailable(reason) => format!("unavailable ({reason})"),
            BuildSnapshots::Kept { on: true } => "on (a rebuild: `before rebuild` is taken first; \
                                                   the game dir stays the subvolume it is)"
                .to_string(),
            BuildSnapshots::Skipped => {
                "off for this run (--no-snapshots; a rebuild: the game dir stays as it is)"
                    .to_string()
            }
            BuildSnapshots::Kept { on: false } => {
                "off for this game (a rebuild: the game dir stays as it is)".to_string()
            }
        }
    }
}

/// Create a new game dir `dir` (its parent too): a subvolume when the plan
/// says snapshots are on, else a plain dir. A dir that is there already is
/// left as it is. A subvolume that can't be made becomes a plain dir and a
/// warning: the build goes on without snapshots.
pub fn make_dir(
    plan: &BuildSnapshots,
    opts: &SnapshotOptions,
    dir: &Path,
    warn: &mut dyn FnMut(String),
) -> Result<(), MacAppError> {
    if dir.symlink_metadata().is_ok() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if *plan == BuildSnapshots::On {
        let (_, b) = opts.backends.for_dir(dir);
        match b.make_game_dir(dir) {
            Ok(()) => return Ok(()),
            Err(e) => warn(format!(
                "couldn't make {} a btrfs subvolume ({e}): it is a plain dir, without snapshots",
                dir.display()
            )),
        }
    }
    std::fs::create_dir(dir)?;
    Ok(())
}

/// A first build's last step (design 20 §4, §7.1), after its setup: write
/// `settings.toml` (`auto = true` where the game dir can be snapshotted and
/// snapshots are on) and take the `initial` snapshot. Errors are the caller's
/// warning: the game is built and plays either way.
pub fn after_first_build(
    gogid: &str,
    dirs: &GameDirs,
    plan: &BuildSnapshots,
    opts: &SnapshotOptions,
) -> Result<Option<Meta>, MacAppError> {
    let root_lock = relocate::lock_moves()?;
    let game_lock = relocate::lock_game(gogid)?;
    let guard = Guard::new(&root_lock, &game_lock);
    let (support, b) = opts.backends.for_dir(&dirs.root);
    let s = store(gogid, &dirs.root, b);
    let ready = *plan == BuildSnapshots::On
        && !matches!(support, Support::None(_))
        && b.ready(&dirs.root).unwrap_or(false);
    s.set_auto(&guard, ready).map_err(err)?;
    if !ready {
        return Ok(None);
    }
    s.create(
        &guard,
        NewSnapshot {
            kind: Kind::Initial,
            trigger: None,
            label: None,
            content: content_of(dirs),
        },
    )
    .map(Some)
    .map_err(err)
}

/// The game's build and setup now, for a snapshot's `[content]`: from
/// `content.toml` and `state.toml` (design 20 §4). `runtime` and `renderer`
/// are `state.toml`'s store references (`ge-proton/<ver>`), which the
/// store's GC matches.
pub fn content_of(dirs: &GameDirs) -> Content {
    let c = crate::gog::content::load(&dirs.root).ok().flatten();
    let s = LinuxState::load(&dirs.state_toml).ok().flatten();
    let set = |v: &str| Some(v.to_string()).filter(|v| !v.is_empty());
    Content {
        lane: c.as_ref().map(|c| c.lane.as_str().to_string()),
        build_id: c.as_ref().and_then(|c| c.build_id.clone()),
        version: c.as_ref().and_then(|c| c.version.clone()),
        language: c.as_ref().and_then(|c| c.language.clone()),
        dlcs: c
            .as_ref()
            .map(|c| c.dlc.iter().map(|d| d.id).collect())
            .unwrap_or_default(),
        runtime: s.as_ref().and_then(|s| set(&s.runtime)),
        renderer: s.as_ref().and_then(|s| set(&s.renderer)),
    }
}

// — locks and refusals (design 20 §5.1) —

/// The game `gogid` as a snapshot command finds it: built by `linux-app`.
pub(crate) fn built(gogid: &str) -> Result<GameDirs, MacAppError> {
    crate::macapp::compat::validate_gogid(gogid)?;
    let dirs = GameDirs::for_game(gogid)?;
    if !dirs.root.is_dir() && launch::recorded_location(gogid).is_some() {
        return Ok(dirs);
    }
    if !game::launch_toml_path(&dirs).is_file() {
        return Err(MacAppError::Apps(format!(
            "{gogid} isn't a game built with `gogo linux-app` here (see `gogo apps list`)"
        )));
    }
    Ok(dirs)
}

fn refuse_unreachable(gogid: &str, dirs: &GameDirs) -> Result<(), MacAppError> {
    if dirs.root.is_dir() {
        return Ok(());
    }
    Err(MacAppError::Apps(format!(
        "{gogid} is recorded at {} and it isn't there (a drive not mounted?): mount it first",
        dirs.root.display()
    )))
}

/// Everything that stops a snapshot change of a game (design 20 §5.1) but the
/// locks: an unreachable game, an unsettled move, the game or a one-off of it
/// running, a DLC add that isn't finished. Reads only.
pub fn refuse(gogid: &str, dirs: &GameDirs) -> Result<(), MacAppError> {
    refuse_unreachable(gogid, dirs)?;
    if let Some(to) = relocate::interrupted(gogid) {
        return Err(MacAppError::Apps(format!(
            "a move of {gogid} to {} is running or was interrupted: if none is running, \
             `gogo linux-app move {gogid} <dir>` settles it",
            to.display()
        )));
    }
    game::refuse_running(gogid, &dirs.root, &dirs.prefix)?;
    if let Some(pid) = game::live_one_off(&dirs.root) {
        return Err(MacAppError::Apps(format!(
            "{gogid} is running another of its programs (`gogo linux-app run`, pid {pid}): \
             quit it first"
        )));
    }
    let root = &dirs.root;
    if super::dlc::journal_exists(root)
        || super::dlc::download_pending(root)
        || root.join(crate::gog::lay::STAGING).exists()
    {
        return Err(MacAppError::Apps(format!(
            "a DLC add of {gogid} isn't finished: finish it first: `gogo linux-app dlc {gogid} \
             add`"
        )));
    }
    Ok(())
}

/// The refusals settling a pending rollback takes (design 20 §5.3.1): the
/// game dir reachable, and neither the game nor another of its programs
/// running in it.
pub(crate) fn refuse_settle(gogid: &str, dirs: &GameDirs) -> Result<(), MacAppError> {
    refuse_unreachable(gogid, dirs)?;
    game::refuse_running(gogid, &dirs.root, &dirs.prefix)?;
    if let Some(pid) = game::live_one_off(&dirs.root) {
        return Err(MacAppError::Apps(format!(
            "{gogid} is running another of its programs (`gogo linux-app run`, pid {pid}): \
             quit it first; an interrupted rollback is settled after that"
        )));
    }
    Ok(())
}

/// Refuse a game dir that can't be snapshotted as it is: no snapshots on its
/// filesystem (§2.4), or a plain dir on btrfs, which only `snapshot create`
/// and `auto on` convert (§2.1).
pub(crate) fn refuse_unready(
    support: &Support,
    b: &dyn SnapshotBackend,
    root: &Path,
) -> Result<(), MacAppError> {
    if needs_converting(support, b, root)? {
        return Err(MacAppError::Apps(format!(
            "{} isn't a btrfs subvolume (the game was built without snapshots, or before \
             them): it needs converting to one first; `gogo linux-app snapshot <gogid> auto \
             on` shows the plan and asks",
            root.display()
        )));
    }
    Ok(())
}

/// Whether a game dir can take a snapshot as it is (`false`), or is a plain
/// dir on btrfs that needs converting first (`true`, §2.1). Refused where
/// there are no snapshots (§2.4).
fn needs_converting(
    support: &Support,
    b: &dyn SnapshotBackend,
    root: &Path,
) -> Result<bool, MacAppError> {
    if let Support::None(reason) = support {
        return Err(MacAppError::Apps(format!("no snapshots: {reason}")));
    }
    if b.ready(root)? {
        return Ok(false);
    }
    if *support == Support::Btrfs {
        return Ok(true);
    }
    Err(MacAppError::Apps(format!(
        "{} can't be snapshotted as it is",
        root.display()
    )))
}

/// A plain dir that needs converting, where the caller didn't agree to it:
/// only `snapshot create` and `auto on` convert, after their plan and
/// question (decision D8).
fn not_agreed(gogid: &str, root: &Path) -> MacAppError {
    MacAppError::Apps(format!(
        "{} isn't a btrfs subvolume (the game was built without snapshots, or before them): \
         it needs converting to one first; `gogo linux-app snapshot {gogid} auto on` shows \
         the plan and asks",
        root.display()
    ))
}

/// Convert the game dir when it needs it and `conv` says the user agreed;
/// refuse when it needs it and they didn't. Under the locks.
fn convert_if_needed(
    gogid: &str,
    dirs: &GameDirs,
    support: &Support,
    b: &dyn SnapshotBackend,
    guard: &Guard,
    conv: Option<&Convert>,
) -> Result<Option<Converted>, MacAppError> {
    if !needs_converting(support, b, &dirs.root)? {
        return Ok(None);
    }
    let Some(conv) = conv else {
        return Err(not_agreed(gogid, &dirs.root));
    };
    let make = |p: &Path| b.make_game_dir(p);
    let mut progress = |p| (conv.progress)(p);
    convert::convert_locked(gogid, dirs, &make, guard, &conv.opts, &mut progress).map(Some)
}

/// The user's yes to converting a plain dir (design 20 §2.1), with its
/// options and where its progress goes.
pub struct Convert<'a> {
    pub opts: ConvertOptions,
    pub progress: &'a dyn Fn(ConvertProgress),
}

impl Default for Convert<'_> {
    fn default() -> Self {
        Self {
            opts: ConvertOptions::default(),
            progress: &|_| {},
        }
    }
}

/// A recovery's lines.
fn recovered_lines(r: &[Recovered]) -> Vec<String> {
    r.iter()
        .map(|r| match r {
            Recovered::Finished(n) => format!("snapshot {n}: an interrupted create was finished"),
            Recovered::Dropped(n) => {
                format!("snapshot {n}: an interrupted create was dropped (nothing was taken)")
            }
            Recovered::Deleted(n) => format!("snapshot {n}: an interrupted delete was finished"),
        })
        .collect()
}

/// Run `f` the way every snapshot change runs (design 20 §5.1): the data
/// root's lock, then the game's, an unreachable game refused, an interrupted
/// create, delete or conversion settled, then the other refusals (`refusals`: off for
/// `auto off`, which changes nothing about the game).
pub(crate) fn locked<T>(
    gogid: &str,
    dirs: &GameDirs,
    backends: &Backends,
    refusals: bool,
    f: impl FnOnce(&Store, &Guard, &Support, &dyn SnapshotBackend) -> Result<T, MacAppError>,
) -> Result<(T, Vec<String>), MacAppError> {
    let root_lock = relocate::lock_moves()?;
    let game_lock = relocate::lock_game(gogid)?;
    let guard = Guard::new(&root_lock, &game_lock);
    refuse_unreachable(gogid, dirs)?;
    let (support, b) = backends.for_dir(&dirs.root);
    let s = store(gogid, &dirs.root, b);
    let mut recovered = recovered_lines(&s.recover(&guard).map_err(err)?);
    // A pending rollback is settled before anything else (design 20 §5.3.1),
    // but never under a running game.
    if s.rollback_journal_path().is_file() {
        refuse_settle(gogid, dirs)?;
    }
    if let Some(r) = super::rollback::settle_locked(gogid, dirs, &s, b)? {
        recovered.push(r.line());
    }
    // So is an interrupted conversion (§2.1), under the same refusals.
    if convert::pending(gogid, &dirs.root).needs_settling() {
        refuse_settle(gogid, dirs)?;
    }
    recovered.extend(convert::settle_locked(gogid, &dirs.root, &guard)?);
    if refusals {
        refuse(gogid, dirs)?;
    }
    Ok((f(&s, &guard, &support, b)?, recovered))
}

// — automatic snapshots before a change (design 20 §7.1, §7.2) —

/// What a changing command does about its `auto` snapshot.
#[derive(Debug, Clone)]
pub struct AutoOptions {
    /// `--no-snapshots`: this run takes none; the game's setting stays.
    pub skip: bool,
    /// `[snapshots] keep_auto`: the `auto` snapshots kept per game, the new
    /// one included (design 20 §7.2).
    pub keep_auto: usize,
    pub backends: Backends,
}

impl Default for AutoOptions {
    fn default() -> Self {
        Self {
            skip: false,
            keep_auto: KEEP_AUTO,
            backends: Backends::real(),
        }
    }
}

impl AutoOptions {
    /// `--no-snapshots`.
    pub fn skipped() -> Self {
        Self {
            skip: true,
            ..Self::default()
        }
    }
}

/// A snapshot as a change's outcome names it (`--json`'s `snapshot` and
/// `pruned`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRef {
    pub id: u32,
    pub label: String,
    /// RFC 3339, UTC.
    pub created: String,
}

impl SnapshotRef {
    pub fn of(m: &Meta) -> Self {
        Self {
            id: m.id,
            label: m.label.clone(),
            created: rfc3339(m.created),
        }
    }

    /// `pruned snapshot 2 (before rebuild, 2026-10-30)` (design 20 §7.2).
    pub fn pruned_line(&self) -> String {
        pruned_line(self.id, &self.label, &self.created)
    }
}

/// `pruned snapshot <n> (<label>, <UTC date>)`; `created` is RFC 3339.
pub fn pruned_line(id: u32, label: &str, created: &str) -> String {
    let day = created.get(..10).unwrap_or(created);
    format!("pruned snapshot {id} ({label}, {day})")
}

/// What [`before_change`] took and pruned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Taken {
    pub meta: Meta,
    pub pruned: Vec<Meta>,
}

impl Taken {
    pub fn snapshot(&self) -> SnapshotRef {
        SnapshotRef::of(&self.meta)
    }

    pub fn pruned(&self) -> Vec<SnapshotRef> {
        self.pruned.iter().map(SnapshotRef::of).collect()
    }
}

/// Why [`before_change`] takes nothing for this game, or `None` when it
/// does: `--no-snapshots`, no snapshots on the filesystem, autosnapshot off
/// in the game's `settings.toml` (a game built before snapshots has none:
/// off), or a game dir that isn't a subvolume (a plain dir is never
/// converted as a side effect, decision D8). Reads only.
pub fn auto_skipped(
    gogid: &str,
    dirs: &GameDirs,
    opts: &AutoOptions,
) -> Result<Option<&'static str>, MacAppError> {
    if opts.skip {
        return Ok(Some("--no-snapshots"));
    }
    let (support, b) = opts.backends.for_dir(&dirs.root);
    if matches!(support, Support::None(_)) {
        return Ok(Some("no snapshots on this filesystem"));
    }
    if !store(gogid, &dirs.root, b).settings().map_err(err)?.auto {
        return Ok(Some("autosnapshot is off for this game"));
    }
    if !b.ready(&dirs.root).unwrap_or(false) {
        return Ok(Some("the game dir isn't a subvolume"));
    }
    Ok(None)
}

/// The `auto` snapshot that protects a change gogo is about to make to a
/// built game (design 20 §7.1), then retention (§7.2).
///
/// Every command that changes a game calls this under the two locks it
/// holds (`guard`: the data root's, then the game's), after its own recovery
/// and refusals and before its first write to the game:
///
/// - `dlc add`: [`Trigger::DlcAdd`], [`snapshot::labels::adding`];
/// - a rebuild of a built game: [`Trigger::Rebuild`],
///   [`snapshot::labels::rebuild`];
/// - a setup that changes the game's GE-Proton (`--retarget`, a new pinned
///   runtime, the launcher's own setup): [`Trigger::Runtime`],
///   [`snapshot::labels::runtime`].
///
/// An update (#360) calls it with [`Trigger::Update`] and
/// [`snapshot::labels::update`], and a DLC removal (#353) with
/// [`Trigger::DlcRemove`] and [`snapshot::labels::removing`], before their
/// journals; [`protect`] does the same for a caller that doesn't hold the
/// locks yet.
///
/// `Ok(None)`: nothing to take ([`auto_skipped`]). `Ok(Some)`: the snapshot,
/// reported as [`ProgressEvent::SnapshotCreated`], and the older `auto` ones
/// retention deleted, each a [`ProgressEvent::SnapshotPruned`]. `initial` and
/// `manual` snapshots are never pruned; a prune that fails is a warning (the
/// change is protected either way). `Err`: the snapshot failed, or a pending
/// rollback or conversion isn't settled; the caller must stop with nothing
/// changed (exit 7). The error says `--no-snapshots` skips it for one run: a
/// protected change never goes ahead silently unprotected.
pub fn before_change(
    gogid: &str,
    dirs: &GameDirs,
    guard: &Guard,
    trigger: Trigger,
    label: String,
    opts: &AutoOptions,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<Option<Taken>, MacAppError> {
    if auto_skipped(gogid, dirs, opts)?.is_some() {
        return Ok(None);
    }
    let (_, b) = opts.backends.for_dir(&dirs.root);
    let s = store(gogid, &dirs.root, b);
    if s.rollback_journal_path().is_file() || convert::pending(gogid, &dirs.root).needs_settling() {
        return Err(MacAppError::Apps(format!(
            "a snapshot rollback or conversion of {gogid} isn't finished, so the snapshot \
             before this change ({label}) can't be taken and nothing was changed: `gogo \
             linux-app snapshot {gogid} list` finishes it"
        )));
    }
    let failed = |e: &dyn std::fmt::Display| {
        MacAppError::Apps(format!(
            "the snapshot before this change ({label}) couldn't be taken, so nothing was \
             changed: {e}; `--no-snapshots` skips it for this run"
        ))
    };
    for message in recovered_lines(&s.recover(guard).map_err(|e| failed(&e))?) {
        emit(&ProgressEvent::Warn { message });
    }
    let meta = s
        .create(
            guard,
            NewSnapshot {
                kind: Kind::Auto,
                trigger: Some(trigger),
                label: Some(label.clone()),
                content: content_of(dirs),
            },
        )
        .map_err(|e| failed(&e))?;
    emit(&ProgressEvent::SnapshotCreated {
        id: meta.id,
        label: meta.label.clone(),
        trigger: trigger.as_str().to_string(),
    });
    let pruned = retain(&s, guard, opts.keep_auto, &[meta.id], emit);
    Ok(Some(Taken { meta, pruned }))
}

/// [`before_change`] for a command that doesn't hold the locks: when there
/// is something to take, the data root's lock and the game's are taken for
/// the snapshot and its retention, then released. `game_lock`: the game's
/// lock, when the caller holds it already (`dlc add`).
#[allow(clippy::too_many_arguments)]
pub fn protect(
    gogid: &str,
    dirs: &GameDirs,
    game_lock: Option<&std::fs::File>,
    trigger: Trigger,
    label: String,
    opts: &AutoOptions,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<Option<Taken>, MacAppError> {
    if auto_skipped(gogid, dirs, opts)?.is_some() {
        return Ok(None);
    }
    let root_lock = relocate::lock_moves()?;
    let own;
    let game_lock = match game_lock {
        Some(l) => l,
        None => {
            own = relocate::lock_game(gogid)?;
            &own
        }
    };
    let guard = Guard::new(&root_lock, game_lock);
    before_change(gogid, dirs, &guard, trigger, label, opts, emit)
}

/// Retention after a new `auto` snapshot (design 20 §7.2): the newest
/// `keep_auto` `auto` snapshots stay, counting the new one, `spare[0]`; the
/// others in `spare` (a rollback's target) stay too and don't count. With
/// `keep_auto = 0` only the new one stays, until the next. `initial` and
/// `manual` ones are never touched. Each pruned one is reported; a failure
/// is a warning.
pub(crate) fn retain(
    s: &Store,
    guard: &Guard,
    keep_auto: usize,
    spare: &[u32],
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Vec<Meta> {
    let keep = keep_auto.saturating_sub(usize::from(!spare.is_empty()));
    match s.prune_keeping(guard, keep, spare) {
        Ok(pruned) => {
            for m in &pruned {
                emit(&ProgressEvent::SnapshotPruned {
                    id: m.id,
                    label: m.label.clone(),
                    created: rfc3339(m.created),
                });
            }
            pruned
        }
        Err(e) => {
            emit(&ProgressEvent::Warn {
                message: format!("older automatic snapshots weren't pruned: {e}"),
            });
            Vec::new()
        }
    }
}

/// A runtime reference as a label shows it: its last part, the build's name
/// (`ge-proton/GE-Proton10-25` and `path:/opt/GE-Proton10-25` are both
/// `GE-Proton10-25`).
pub fn runtime_name(reference: &str) -> &str {
    let r = reference.strip_prefix("path:").unwrap_or(reference);
    r.trim_end_matches('/').rsplit('/').next().unwrap_or(r)
}

// — the views (`--json` and text) —

pub(crate) fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn local(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()
}

fn kind_str(k: Kind) -> &'static str {
    match k {
        Kind::Initial => "initial",
        Kind::Manual => "manual",
        Kind::Auto => "auto",
    }
}

fn state_str(s: State) -> &'static str {
    match s {
        State::Creating => "creating",
        State::Done => "done",
        State::Deleting => "deleting",
    }
}

fn presence_str(p: Presence) -> &'static str {
    match p {
        Presence::Present => "present",
        Presence::Missing => "missing",
        Presence::Stray => "stray",
        Presence::Unreachable => "unreachable",
    }
}

/// A snapshot's `[content]` in `--json` (camelCase, design 20 §8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentView {
    pub lane: Option<String>,
    pub build_id: Option<String>,
    pub version: Option<String>,
    pub language: Option<String>,
    pub dlcs: Vec<u64>,
    pub runtime: Option<String>,
    pub renderer: Option<String>,
}

impl From<&Content> for ContentView {
    fn from(c: &Content) -> Self {
        Self {
            lane: c.lane.clone(),
            build_id: c.build_id.clone(),
            version: c.version.clone(),
            language: c.language.clone(),
            dlcs: c.dlcs.clone(),
            runtime: c.runtime.clone(),
            renderer: c.renderer.clone(),
        }
    }
}

/// A snapshot's stored space in `--json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceView {
    pub exclusive_bytes: u64,
    pub exact: bool,
    pub checked: String,
}

/// One snapshot, as `list` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotView {
    pub id: u32,
    /// UTC, RFC 3339: the metadata's, else the filesystem's (a stray one).
    pub created: Option<String>,
    /// `None` for a snapshot on disk with no metadata (`?`).
    pub kind: Option<&'static str>,
    pub trigger: Option<&'static str>,
    pub label: Option<String>,
    /// `done`, or `creating` / `deleting` after a crash; `None` for a stray one.
    pub state: Option<&'static str>,
    /// `present`, `missing` (metadata only), `stray` (disk only),
    /// `unreachable` (the game's drive isn't there).
    pub presence: &'static str,
    pub reachable: bool,
    pub content: Option<ContentView>,
    pub space: Option<SpaceView>,
    #[serde(skip)]
    created_at: Option<DateTime<Utc>>,
}

impl SnapshotView {
    fn of(e: &Entry) -> Self {
        let created_at = e
            .meta
            .as_ref()
            .map(|m| m.created)
            .or_else(|| e.fs_created.map(DateTime::<Utc>::from));
        let m = e.meta.as_ref();
        SnapshotView {
            id: e.id,
            created: created_at.map(rfc3339),
            kind: m.map(|m| kind_str(m.kind)),
            trigger: m.and_then(|m| m.trigger).map(|t| t.as_str()),
            label: m.map(|m| m.label.clone()),
            state: m.map(|m| state_str(m.state)),
            presence: presence_str(e.presence),
            reachable: e.presence != Presence::Unreachable,
            content: m.map(|m| ContentView::from(&m.content)),
            space: m.and_then(|m| m.space).map(|s| SpaceView {
                exclusive_bytes: s.exclusive_bytes,
                exact: s.exact,
                checked: rfc3339(s.checked),
            }),
            created_at,
        }
    }

    /// `~912 MiB` (an estimate), `912 MiB` (exact), `-` (not measured).
    fn space_text(&self) -> String {
        match &self.space {
            Some(s) if s.exact => crate::fmt::human_size(s.exclusive_bytes),
            Some(s) => format!("~{}", crate::fmt::human_size(s.exclusive_bytes)),
            None => "-".to_string(),
        }
    }

    /// `BUILD`: the version, else the build id's tail.
    fn build_text(&self) -> String {
        let c = self.content.as_ref();
        if let Some(v) = c.and_then(|c| c.version.as_deref()) {
            return v.to_string();
        }
        match c.and_then(|c| c.build_id.as_deref()) {
            Some(b) if b.len() > 8 => format!("…{}", &b[b.len() - 8..]),
            Some(b) => b.to_string(),
            None => "-".to_string(),
        }
    }

    /// The kind column, with what's wrong with it.
    fn kind_text(&self) -> String {
        let kind = self.kind.unwrap_or("?");
        match (self.presence, self.state) {
            ("missing", _) => format!("{kind} (missing)"),
            ("unreachable", _) => format!("{kind} (unreachable)"),
            (_, Some(s @ ("creating" | "deleting"))) => format!("{kind} ({s})"),
            _ => kind.to_string(),
        }
    }

    /// `1 (after the first build)`, `1 (initial: after the first build)`.
    fn named(&self) -> String {
        let label = self.label.as_deref().unwrap_or("no metadata");
        match self.kind {
            Some("initial") => format!("{} (initial: {label})", self.id),
            _ => format!("{} ({label})", self.id),
        }
    }
}

/// `snapshot <gogid> list` (design 20 §8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListView {
    pub gogid: u64,
    /// `btrfs`, or `None` where there are no snapshots.
    pub backend: Option<&'static str>,
    /// Why there are none.
    pub reason: Option<String>,
    /// The game dir can be snapshotted as it is (a subvolume); `false` for a
    /// plain dir on btrfs, which needs converting.
    pub ready: bool,
    pub auto: bool,
    pub keep_auto: usize,
    pub free_bytes: Option<u64>,
    pub game_dir: PathBuf,
    pub reachable: bool,
    pub snapshots: Vec<SnapshotView>,
    /// What a recovery or the space measurement had to say.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl ListView {
    /// The backend line (`btrfs subvolume`, `none: <reason>`).
    pub fn backend_line(&self) -> String {
        match (self.backend, &self.reason) {
            (_, Some(reason)) => format!("none: {reason}"),
            (Some(b), None) if self.ready => format!("{b} subvolume"),
            (Some(b), None) => format!(
                "{b}, but the game dir isn't a subvolume (built without snapshots): \
                 `gogo linux-app snapshot {} auto on` converts it",
                self.gogid
            ),
            (None, None) => "none".to_string(),
        }
    }

    /// `on, keeping 5 automatic` / `off`.
    pub fn auto_line(&self) -> String {
        if self.auto {
            format!("on, keeping {} automatic", self.keep_auto)
        } else {
            "off".to_string()
        }
    }
}

fn view(
    gogid: &str,
    root: &Path,
    support: &Support,
    b: &dyn SnapshotBackend,
    listing: Listing,
    keep_auto: usize,
) -> ListView {
    let reason = match support {
        Support::None(r) => Some(r.clone()),
        _ => None,
    };
    ListView {
        gogid: gogid.parse().unwrap_or(0),
        backend: support.kind().map(BackendKind::as_str),
        ready: reason.is_none() && b.ready(root).unwrap_or(false),
        reason,
        auto: listing.settings.auto,
        keep_auto,
        free_bytes: listing
            .reachable
            .then(|| fs4::available_space(root).ok())
            .flatten(),
        game_dir: root.to_path_buf(),
        reachable: listing.reachable,
        snapshots: listing.entries.iter().map(SnapshotView::of).collect(),
        notes: Vec::new(),
    }
}

/// `snapshot <gogid> list`: the setting and every snapshot, newest first.
/// `measure`: measure the space first (§7.3) and store it, under the locks;
/// skipped with a note when they're held, the game is unreachable or there is
/// nothing to measure.
pub fn list(
    gogid: &str,
    backends: &Backends,
    keep_auto: usize,
    measure: bool,
) -> Result<ListView, MacAppError> {
    let dirs = built(gogid)?;
    // A pending rollback is settled first, as by every snapshot command.
    // A running game holds it off: `list` still lists, and says so.
    let mut notes = match super::rollback::settle_pending(gogid, backends) {
        Ok(n) => n,
        Err(e) => vec![format!("an interrupted rollback isn't settled yet: {e}")],
    };
    let (support, b) = backends.for_dir(&dirs.root);
    let s = store(gogid, &dirs.root, b);
    let has_snapshots = dirs.root.is_dir()
        && !matches!(support, Support::None(_))
        && b.on_disk(&dirs.root).is_ok_and(|d| !d.is_empty());
    if measure && has_snapshots {
        match relocate::lock_moves().and_then(|r| Ok((r, relocate::lock_game(gogid)?))) {
            Ok((r, g)) => {
                if let Err(e) = s.measure_space(&Guard::new(&r, &g)) {
                    notes.push(format!("couldn't measure the space: {e}"));
                }
            }
            Err(e) => notes.push(format!("space not measured now: {e}")),
        }
    }
    let listing = s.list().map_err(err)?;
    let mut v = view(gogid, &dirs.root, &support, b, listing, keep_auto);
    v.notes = notes;
    Ok(v)
}

/// [`ListView`] as text (design 20 §8).
pub fn render_list(v: &ListView) -> String {
    let mut out = format!(
        "{}: snapshots of {}\n  backend:      {}\n  autosnapshot: {}\n",
        v.gogid,
        v.game_dir.display(),
        v.backend_line(),
        v.auto_line()
    );
    if let Some(free) = v.free_bytes {
        out.push_str(&format!(
            "  free:         {}\n",
            crate::fmt::human_size(free)
        ));
    }
    if !v.reachable {
        out.push_str("  the game's dir isn't there (a drive not mounted?): its snapshots can't be looked at\n");
    }
    for n in &v.notes {
        out.push_str(&format!("  note: {n}\n"));
    }
    if v.snapshots.is_empty() {
        match &v.reason {
            Some(r) => out.push_str(&format!("no snapshots: {r}\n")),
            None => out.push_str("no snapshots\n"),
        }
        return out;
    }
    let rows: Vec<[String; 7]> = v
        .snapshots
        .iter()
        .map(|s| {
            [
                s.id.to_string(),
                s.created_at.map_or_else(|| "-".to_string(), local),
                s.kind_text(),
                s.label.clone().unwrap_or_else(|| "-".to_string()),
                s.build_text(),
                s.content
                    .as_ref()
                    .map_or_else(|| "-".to_string(), |c| c.dlcs.len().to_string()),
                s.space_text(),
            ]
        })
        .collect();
    let head = ["N", "DATE", "KIND", "LABEL", "BUILD", "DLC", "SPACE"].map(String::from);
    let mut widths = head.clone().map(|h| h.chars().count());
    for r in &rows {
        for (w, c) in widths.iter_mut().zip(r) {
            *w = (*w).max(c.chars().count());
        }
    }
    for r in std::iter::once(&head).chain(&rows) {
        let cells: Vec<String> = r
            .iter()
            .zip(widths)
            .map(|(c, w)| format!("{c}{}", " ".repeat(w - c.chars().count())))
            .collect();
        out.push_str(cells.join("  ").trim_end());
        out.push('\n');
    }
    out
}

// — create —

/// What `create` will do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePlan {
    pub gogid: String,
    pub game_dir: PathBuf,
    pub label: String,
    pub content: ContentView,
    /// A plain dir on btrfs: the conversion that comes first (design 20 §2.1).
    pub convert: Option<ConvertPlan>,
}

impl CreatePlan {
    pub fn lines(&self) -> Vec<String> {
        let c = &self.content;
        let mut out = self
            .convert
            .as_ref()
            .map(ConvertPlan::lines)
            .unwrap_or_default();
        out.extend([
            format!(
                "snapshot of {} ({}), labelled \"{}\"",
                self.game_dir.display(),
                self.gogid,
                self.label
            ),
            format!(
                "  build {}, {} DLC, {}",
                c.version
                    .as_deref()
                    .or(c.build_id.as_deref())
                    .unwrap_or("unknown"),
                c.dlcs.len(),
                c.runtime.as_deref().unwrap_or("no runtime recorded")
            ),
        ]);
        out
    }
}

/// `create`'s plan, with every refusal checked (reads only): exit 7 on a
/// refusal before anything is asked.
pub fn plan_create(
    gogid: &str,
    backends: &Backends,
    label: Option<&str>,
) -> Result<CreatePlan, MacAppError> {
    plan_create_with(gogid, backends, label, &ConvertOptions::default())
}

/// [`plan_create`] with the conversion's test seams.
pub fn plan_create_with(
    gogid: &str,
    backends: &Backends,
    label: Option<&str>,
    conv: &ConvertOptions,
) -> Result<CreatePlan, MacAppError> {
    let dirs = built(gogid)?;
    refuse(gogid, &dirs)?;
    let convert = plan_convert(gogid, &dirs, backends, conv)?;
    Ok(CreatePlan {
        convert,
        gogid: gogid.to_string(),
        label: label
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map_or_else(snapshot::labels::manual, String::from),
        content: ContentView::from(&content_of(&dirs)),
        game_dir: dirs.root,
    })
}

/// The conversion a plain btrfs dir needs before its first snapshot, or
/// `None` when it can be snapshotted as it is. Reads only; refuses where there
/// are no snapshots.
fn plan_convert(
    gogid: &str,
    dirs: &GameDirs,
    backends: &Backends,
    conv: &ConvertOptions,
) -> Result<Option<ConvertPlan>, MacAppError> {
    let (support, b) = backends.for_dir(&dirs.root);
    if !needs_converting(&support, b, &dirs.root)? {
        return Ok(None);
    }
    convert::plan(gogid, dirs, conv).map(Some)
}

/// What `create` did.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Created {
    pub snapshot: SnapshotView,
    /// The game dir was converted to a subvolume first.
    pub converted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversion: Option<Converted>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recovered: Vec<String>,
}

/// `snapshot <gogid> create`: a `manual` snapshot, under the locks and
/// refusals of §5.1. A plain dir that needs converting is refused.
pub fn create(
    gogid: &str,
    backends: &Backends,
    label: Option<&str>,
) -> Result<Created, MacAppError> {
    create_with(gogid, backends, label, None)
}

/// [`create`]; with `conv` (the user agreed to the plan), a plain dir on
/// btrfs is converted to a subvolume first (design 20 §2.1).
pub fn create_with(
    gogid: &str,
    backends: &Backends,
    label: Option<&str>,
    conv: Option<&Convert>,
) -> Result<Created, MacAppError> {
    let dirs = built(gogid)?;
    let ((meta, conversion), recovered) =
        locked(gogid, &dirs, backends, true, |s, g, support, b| {
            let conversion = convert_if_needed(gogid, &dirs, support, b, g, conv)?;
            s.create(
                g,
                NewSnapshot {
                    kind: Kind::Manual,
                    trigger: None,
                    label: label.map(str::to_string),
                    content: content_of(&dirs),
                },
            )
            .map(|m| (m, conversion))
            .map_err(err)
        })?;
    Ok(Created {
        converted: conversion.is_some(),
        conversion,
        snapshot: SnapshotView::of(&Entry {
            id: meta.id,
            meta: Some(meta),
            presence: Presence::Present,
            fs_created: None,
        }),
        recovered,
    })
}

// — delete —

/// What `delete` will do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletePlan {
    pub gogid: String,
    pub snapshots: Vec<SnapshotView>,
    /// Numbers that aren't snapshots of the game (a usage error).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unknown: Vec<u32>,
}

impl DeletePlan {
    /// The question (design 20 §8): `delete snapshot 1 (initial: after the
    /// first build), ~4.1 GiB?`.
    pub fn question(&self) -> String {
        let space: u64 = self
            .snapshots
            .iter()
            .filter_map(|s| s.space.as_ref())
            .map(|s| s.exclusive_bytes)
            .sum();
        let measured = self.snapshots.iter().any(|s| s.space.is_some());
        let exact = self
            .snapshots
            .iter()
            .all(|s| s.space.as_ref().is_some_and(|s| s.exact));
        let size = match (measured, exact) {
            (false, _) => String::new(),
            (true, true) => format!(", {}", crate::fmt::human_size(space)),
            (true, false) => format!(", ~{}", crate::fmt::human_size(space)),
        };
        match self.snapshots.as_slice() {
            [one] => format!("delete snapshot {}{size}?", one.named()),
            many => {
                let initial = many.iter().filter(|s| s.kind == Some("initial")).count();
                let ids: Vec<String> = many.iter().map(|s| s.id.to_string()).collect();
                let initial = match initial {
                    0 => String::new(),
                    n => format!(", {n} of them initial"),
                };
                format!(
                    "delete {} snapshots ({}{initial}){size}?",
                    many.len(),
                    ids.join(", ")
                )
            }
        }
    }

    pub fn lines(&self) -> Vec<String> {
        self.snapshots
            .iter()
            .map(|s| {
                format!(
                    "snapshot {}: {}, {}, {}",
                    s.named(),
                    s.kind_text(),
                    s.created_at.map_or_else(|| "-".to_string(), local),
                    s.space_text()
                )
            })
            .collect()
    }
}

/// `delete`'s plan, with the refusals checked (reads only).
pub fn plan_delete(
    gogid: &str,
    backends: &Backends,
    ns: &[u32],
) -> Result<DeletePlan, MacAppError> {
    let dirs = built(gogid)?;
    refuse(gogid, &dirs)?;
    let (_, b) = backends.for_dir(&dirs.root);
    let listing = store(gogid, &dirs.root, b).list().map_err(err)?;
    let mut ns = ns.to_vec();
    ns.sort_unstable();
    ns.dedup();
    let mut plan = DeletePlan {
        gogid: gogid.to_string(),
        snapshots: Vec::new(),
        unknown: Vec::new(),
    };
    for n in ns {
        match listing.entries.iter().find(|e| e.id == n) {
            Some(e) => plan.snapshots.push(SnapshotView::of(e)),
            None => plan.unknown.push(n),
        }
    }
    Ok(plan)
}

/// What `delete` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Deleted {
    pub deleted: Vec<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recovered: Vec<String>,
}

/// `snapshot <gogid> delete <n>…`, under the locks and refusals of §5.1.
pub fn delete(gogid: &str, backends: &Backends, ns: &[u32]) -> Result<Deleted, MacAppError> {
    let dirs = built(gogid)?;
    let mut ns = ns.to_vec();
    ns.sort_unstable();
    ns.dedup();
    let (deleted, recovered) = locked(gogid, &dirs, backends, true, |s, g, _, _| {
        let mut done = Vec::new();
        for n in ns {
            s.delete(g, n).map_err(err)?;
            done.push(n);
        }
        Ok(done)
    })?;
    Ok(Deleted { deleted, recovered })
}

// — auto —

/// `auto`'s answer.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoView {
    pub gogid: u64,
    pub auto: bool,
    pub keep_auto: usize,
    /// `auto on` converted the game dir to a subvolume first.
    pub converted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversion: Option<Converted>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recovered: Vec<String>,
}

/// `snapshot <gogid> auto`: the setting. Reads only.
pub fn auto(gogid: &str, backends: &Backends, keep_auto: usize) -> Result<AutoView, MacAppError> {
    let dirs = built(gogid)?;
    let (_, b) = backends.for_dir(&dirs.root);
    Ok(AutoView {
        gogid: gogid.parse().unwrap_or(0),
        auto: store(gogid, &dirs.root, b).settings().map_err(err)?.auto,
        keep_auto,
        converted: false,
        conversion: None,
        recovered: Vec::new(),
    })
}

/// What `auto on|off` will do (`--dry-run`'s JSON).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoPlan {
    pub gogid: String,
    pub game_dir: PathBuf,
    pub auto: bool,
    /// A plain dir on btrfs: the conversion `on` does first.
    pub convert: Option<ConvertPlan>,
}

impl AutoPlan {
    pub fn lines(&self) -> Vec<String> {
        let mut out = self
            .convert
            .as_ref()
            .map(ConvertPlan::lines)
            .unwrap_or_default();
        out.push(format!(
            "autosnapshot {} for {}",
            if self.auto { "on" } else { "off" },
            self.gogid
        ));
        out
    }
}

/// `on`'s refusals, checked before anything (reads only), and the conversion
/// it needs first. `off` has neither.
pub fn plan_auto(gogid: &str, backends: &Backends, on: bool) -> Result<AutoPlan, MacAppError> {
    plan_auto_with(gogid, backends, on, &ConvertOptions::default())
}

/// [`plan_auto`] with the conversion's test seams.
pub fn plan_auto_with(
    gogid: &str,
    backends: &Backends,
    on: bool,
    conv: &ConvertOptions,
) -> Result<AutoPlan, MacAppError> {
    let dirs = built(gogid)?;
    let convert = if on {
        refuse(gogid, &dirs)?;
        plan_convert(gogid, &dirs, backends, conv)?
    } else {
        None
    };
    Ok(AutoPlan {
        gogid: gogid.to_string(),
        game_dir: dirs.root,
        auto: on,
        convert,
    })
}

/// `snapshot <gogid> auto on|off`. `on` takes no snapshot (design 20 §8) and
/// is refused where a snapshot couldn't be taken, or where the game dir needs
/// converting; `off` keeps the snapshots.
pub fn set_auto(
    gogid: &str,
    backends: &Backends,
    on: bool,
    keep_auto: usize,
) -> Result<AutoView, MacAppError> {
    set_auto_with(gogid, backends, on, keep_auto, None)
}

/// [`set_auto`]; with `conv` (the user agreed to the plan), `on` converts a
/// plain dir on btrfs to a subvolume first (design 20 §2.1).
pub fn set_auto_with(
    gogid: &str,
    backends: &Backends,
    on: bool,
    keep_auto: usize,
    conv: Option<&Convert>,
) -> Result<AutoView, MacAppError> {
    let dirs = built(gogid)?;
    let (conversion, recovered) = locked(gogid, &dirs, backends, on, |s, g, support, b| {
        let conversion = if on {
            convert_if_needed(gogid, &dirs, support, b, g, conv)?
        } else {
            None
        };
        s.set_auto(g, on).map_err(err)?;
        Ok(conversion)
    })?;
    Ok(AutoView {
        gogid: gogid.parse().unwrap_or(0),
        auto: on,
        keep_auto,
        converted: conversion.is_some(),
        conversion,
        recovered,
    })
}

// — what `apps` shows and removes (design 20 §8, §9.3) —

/// The `snapshots` section of `apps show` (and the count `apps remove`
/// names).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    /// The backend line, as `list`'s.
    pub backend: String,
    pub auto: bool,
    pub count: usize,
    /// The newest three: `(id, created (UTC, RFC 3339), label)`.
    pub newest: Vec<(u32, Option<String>, String)>,
    /// The stored space of those measured, and when the oldest of those
    /// measurements was taken.
    pub space_bytes: u64,
    pub space_exact: bool,
    pub space_checked: Option<String>,
    /// `<data_root>/saves-aside/<gogid>/<time>/` dirs, newest first.
    pub saves_aside: Vec<PathBuf>,
    /// Their sizes, in the same order.
    pub saves_aside_bytes: Vec<u64>,
}

impl Summary {
    /// `and 4 snapshots, ~6.2 GiB` for `apps remove`'s list, `None` with none.
    pub fn remove_note(&self) -> Option<String> {
        if self.count == 0 {
            return None;
        }
        let size = if self.space_checked.is_some() {
            format!(
                ", {}{}",
                if self.space_exact { "" } else { "~" },
                crate::fmt::human_size(self.space_bytes)
            )
        } else {
            String::new()
        };
        Some(format!(
            "{} snapshot{}{size}",
            self.count,
            if self.count == 1 { "" } else { "s" }
        ))
    }

    /// The section's lines.
    pub fn lines(&self, gogid: &str) -> Vec<String> {
        let mut out = vec![
            format!("backend {}", self.backend),
            format!(
                "autosnapshot {}, {} snapshot{}",
                if self.auto { "on" } else { "off" },
                self.count,
                if self.count == 1 { "" } else { "s" }
            ),
        ];
        for (id, created, label) in &self.newest {
            let date = created
                .as_deref()
                .and_then(|c| DateTime::parse_from_rfc3339(c).ok())
                .map_or_else(|| "-".to_string(), |t| local(t.with_timezone(&Utc)));
            out.push(format!("{id}  {date}  {label}"));
        }
        if let Some(checked) = &self.space_checked {
            out.push(format!(
                "space {}{} (measured {})",
                if self.space_exact { "" } else { "~" },
                crate::fmt::human_size(self.space_bytes),
                DateTime::parse_from_rfc3339(checked)
                    .map_or_else(|_| checked.clone(), |t| local(t.with_timezone(&Utc)))
            ));
        }
        for (i, d) in self.saves_aside.iter().enumerate() {
            match self.saves_aside_bytes.get(i) {
                Some(b) => out.push(format!(
                    "saves set aside: {} ({})",
                    d.display(),
                    crate::fmt::human_size(*b)
                )),
                None => out.push(format!("saves set aside: {}", d.display())),
            }
        }
        if self.count > 0 {
            out.push(format!(
                "`gogo linux-app snapshot {gogid} list` shows them all"
            ));
        }
        out
    }
}

/// What `apps show` says about the snapshots of `gogid` at `root`. Reads
/// only, and the stored space only (no walk).
pub fn summary(gogid: &str, root: &Path, backends: &Backends) -> Summary {
    let (support, b) = backends.for_dir(root);
    let s = store(gogid, root, b);
    let Ok(listing) = s.list() else {
        return Summary {
            backend: "unreadable metadata".to_string(),
            ..Summary::default()
        };
    };
    let v = view(gogid, root, &support, b, listing, KEEP_AUTO);
    let aside = super::rollback::saves_aside(gogid);
    let measured: Vec<&SpaceView> = v
        .snapshots
        .iter()
        .filter_map(|s| s.space.as_ref())
        .collect();
    Summary {
        backend: v.backend_line(),
        auto: v.auto,
        count: v.snapshots.len(),
        newest: v
            .snapshots
            .iter()
            .take(3)
            .map(|s| {
                (
                    s.id,
                    s.created.clone(),
                    s.label.clone().unwrap_or_else(|| "no metadata".to_string()),
                )
            })
            .collect(),
        space_bytes: measured.iter().map(|s| s.exclusive_bytes).sum(),
        space_exact: !measured.is_empty() && measured.iter().all(|s| s.exact),
        space_checked: measured.iter().map(|s| s.checked.clone()).min(),
        saves_aside: aside.iter().map(|(d, _)| d.clone()).collect(),
        saves_aside_bytes: aside.iter().map(|(_, b)| *b).collect(),
    }
}

/// Delete every snapshot of `gogid`, whose dir is `root` (design 20 §9.3),
/// under the locks, before `apps remove` deletes the game dir. Returns the
/// numbers deleted. The game's running check is the caller's.
pub fn delete_all(gogid: &str, root: &Path, backends: &Backends) -> Result<Vec<u32>, MacAppError> {
    let (_, b) = backends.for_dir(root);
    let has_meta = store(gogid, root, b).dir().is_dir();
    let has_disk = b.on_disk(root).is_ok_and(|d| !d.is_empty());
    if !has_meta && !has_disk {
        return Ok(Vec::new());
    }
    let root_lock = relocate::lock_moves()?;
    let game_lock = relocate::lock_game(gogid)?;
    let guard = Guard::new(&root_lock, &game_lock);
    let s = store(gogid, root, b);
    s.recover(&guard).map_err(err)?;
    let mut done = Vec::new();
    for e in s.list().map_err(err)?.entries {
        s.delete(&guard, e.id).map_err(err)?;
        done.push(e.id);
    }
    if let Some(dir) = backend::snapshots_dir(root) {
        let _ = std::fs::remove_dir(&dir);
        if let Some(parent) = dir.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
    Ok(done)
}

/// Drop `<data_root>/snapshots/<gogid>/`, the game's snapshot metadata and
/// setting: `apps remove`'s last step (design 20 §9.3).
pub fn forget(gogid: &str) -> Result<Option<PathBuf>, MacAppError> {
    crate::macapp::compat::validate_gogid(gogid)?;
    let dir = crate::state::data_root().join("snapshots").join(gogid);
    if !dir.is_dir() {
        return Ok(None);
    }
    crate::gog::materialize::remove_tree(&dir)?;
    Ok(Some(dir))
}

// — doctor (design 20 §8) —

/// `doctor`'s `snapshots` check: the backend, the setting against the game
/// dir, the metadata against the disk, the runtimes the snapshots need, and
/// an interrupted create or delete.
pub fn doctor(gogid: &str, root: &Path, backends: &Backends) -> Vec<Finding> {
    const CHECK: &str = "snapshots";
    let (support, b) = backends.for_dir(root);
    let s = store(gogid, root, b);
    let listing = match s.list() {
        Ok(l) => l,
        Err(e) => {
            return vec![Finding::warn(
                CHECK,
                format!("the snapshot metadata can't be read: {e}"),
                format!("look at {}", s.dir().display()),
            )];
        }
    };
    let has_settings = s.dir().join("settings.toml").is_file();
    let v = view(gogid, root, &support, b, listing, KEEP_AUTO);
    let mut out = Vec::new();
    let count = match v.snapshots.len() {
        0 => "no snapshots".to_string(),
        1 => "1 snapshot".to_string(),
        n => format!("{n} snapshots"),
    };
    let space = Summary {
        count: v.snapshots.len(),
        space_bytes: v
            .snapshots
            .iter()
            .filter_map(|s| s.space.as_ref())
            .map(|s| s.exclusive_bytes)
            .sum(),
        space_exact: v
            .snapshots
            .iter()
            .all(|s| s.space.as_ref().is_some_and(|s| s.exact)),
        space_checked: v
            .snapshots
            .iter()
            .filter_map(|s| s.space.as_ref())
            .map(|s| s.checked.clone())
            .min(),
        ..Summary::default()
    }
    .remove_note()
    .unwrap_or(count);
    let pending = convert::pending(gogid, root);
    if pending.only_foreign() {
        out.push(Finding::fail(
            CHECK,
            format!(
                "{} is beside the game dir, and nothing says gogo made it",
                pending.describe()
            ),
            "gogo won't delete it: look at it and remove or rename it by hand; a conversion \
             is refused while it is there",
        ));
    } else if !pending.is_empty() {
        out.push(Finding::fail(
            CHECK,
            format!(
                "a conversion of the game dir to a subvolume was interrupted: {} is still \
                 there",
                pending.describe()
            ),
            format!(
                "`gogo linux-app snapshot {gogid} list` settles it{}",
                if pending.marker.is_some() {
                    " (the game is converted; only the cleanup is left)"
                } else {
                    " (the game is as it was; the copy is deleted)"
                }
            ),
        ));
    }
    for (p, reachable) in super::relocate::leftovers(gogid, root, backends) {
        out.push(if reachable {
            Finding::warn(
                CHECK,
                format!(
                    "{} is a snapshot a move across filesystems couldn't delete: it holds space \
                     on that drive, and nothing can be rolled back to it",
                    p.display()
                ),
                format!("`gogo linux-app snapshot {gogid} list` tries deleting it again"),
            )
        } else {
            Finding::warn(
                CHECK,
                format!(
                    "{} may still hold a snapshot a move across filesystems couldn't delete: \
                     its drive isn't there (not mounted?)",
                    p.display()
                ),
                format!(
                    "mount it, then `gogo linux-app snapshot {gogid} list` tries deleting it \
                     again"
                ),
            )
        });
    }
    if !v.reachable {
        out.push(Finding::ok(
            CHECK,
            format!("{space}; unreachable: the game's dir isn't there"),
        ));
        return out;
    }
    // Where each snapshot was taken: after `move --adopt` (or a move by
    // hand) they stay beside the old place (design 20 §9.1).
    let taken_at: Vec<(u32, PathBuf)> = s
        .metadata()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|m| backend::snapshot_dir(&m.game_dir, m.id).map(|p| (m.id, p)))
        .collect();
    let elsewhere = |id: u32| {
        taken_at
            .iter()
            .find(|(n, _)| *n == id)
            .map(|(_, p)| p.clone())
            .filter(|p| backend::snapshot_dir(root, id).as_ref() != Some(p))
            .filter(|p| p.symlink_metadata().is_ok())
    };
    out.extend(super::rollback::doctor_findings(gogid, root));
    match (&v.reason, v.ready, v.auto, has_settings) {
        (Some(reason), ..) => out.push(Finding::ok(CHECK, format!("none: {reason}; {space}"))),
        (None, false, true, _) => out.push(Finding::warn(
            CHECK,
            format!(
                "autosnapshot is on, but {} is no longer a subvolume (copied by hand?): \
                 snapshots stopped",
                root.display()
            ),
            format!(
                "`gogo linux-app snapshot {gogid} auto on` converts it again; `auto off` \
                 silences this"
            ),
        )),
        (None, false, false, false) => out.push(
            Finding::ok(
                CHECK,
                "built before snapshots: a plain dir, which needs converting to a subvolume \
                 before it can be snapshotted",
            )
            .hint(format!(
                "`gogo linux-app snapshot {gogid} auto on` converts it (a reflink copy, \
                 almost no space) and turns autosnapshot on"
            )),
        ),
        (None, false, false, true) => out.push(
            Finding::ok(CHECK, "off: built without snapshots (a plain dir)").hint(format!(
                "`gogo linux-app snapshot {gogid} auto on` converts it and turns autosnapshot on"
            )),
        ),
        (None, true, auto, _) => {
            let f = Finding::ok(
                CHECK,
                format!(
                    "btrfs subvolume, autosnapshot {}; {space}",
                    if auto { "on" } else { "off" }
                ),
            );
            out.push(if auto {
                f
            } else {
                f.hint(format!(
                    "`gogo linux-app snapshot {gogid} auto on` turns it on"
                ))
            });
        }
    }
    for snap in &v.snapshots {
        match snap.presence {
            "missing" if let Some(at) = elsewhere(snap.id) => out.push(Finding::warn(
                CHECK,
                format!(
                    "snapshot {} is unreachable from the game's dir: it is at {}, beside the \
                     place it was taken (the game was adopted or moved by hand), and can't be \
                     rolled back to from here",
                    snap.named(),
                    at.display()
                ),
                format!(
                    "`gogo linux-app snapshot {gogid} delete {}` removes its metadata (the \
                     snapshot itself stays at that path)",
                    snap.id
                ),
            )),
            "missing" => out.push(Finding::warn(
                CHECK,
                format!(
                    "snapshot {} is in the metadata but not on disk",
                    snap.named()
                ),
                format!(
                    "`gogo linux-app snapshot {gogid} delete {}` drops it",
                    snap.id
                ),
            )),
            "stray" => out.push(Finding::warn(
                CHECK,
                format!(
                    "snapshot {} is on disk without metadata (a data dir wiped or restored?)",
                    snap.id
                ),
                format!(
                    "`gogo linux-app snapshot {gogid} delete {}` removes it",
                    snap.id
                ),
            )),
            _ => {}
        }
        if let Some(st @ ("creating" | "deleting")) = snap.state {
            out.push(Finding::warn(
                CHECK,
                format!("snapshot {} was interrupted while {st}", snap.id),
                format!(
                    "the next snapshot change (`gogo linux-app snapshot {gogid} create` or \
                     `delete`) settles it first"
                ),
            ));
        }
        if let Some(rt) = snap.content.as_ref().and_then(|c| c.runtime.as_deref())
            && !rt.starts_with("path:")
            && !runtime_present(rt)
        {
            out.push(Finding::warn(
                CHECK,
                format!(
                    "snapshot {} needs {rt}, which isn't in the store",
                    snap.named()
                ),
                "a rollback to it fetches it at the next launch's setup".to_string(),
            ));
        }
    }
    out
}

/// Is store reference `r` (`<id>/<ver>`) installed?
fn runtime_present(r: &str) -> bool {
    let (id, ver) = r.split_once('/').map_or((r, None), |(i, v)| (i, Some(v)));
    crate::macapp::store::installed(id, ver).is_some()
}

/// Every snapshot's store references (runtime, renderer) for the store's GC
/// (design 20 §9.6): `(gogid, <n>.toml, reference)`.
pub fn store_references() -> Vec<(String, PathBuf, String)> {
    let mut out = Vec::new();
    for (gogid, file, meta) in snapshot::all_metadata(&crate::state::data_root()) {
        for r in [&meta.content.runtime, &meta.content.renderer]
            .into_iter()
            .flatten()
        {
            out.push((gogid.clone(), file.clone(), r.clone()));
        }
    }
    out
}

/// One snapshot as `list` shows it, from its metadata (a build's `initial`
/// one, for its output).
pub fn meta_view(meta: &Meta) -> SnapshotView {
    SnapshotView::of(&Entry {
        id: meta.id,
        meta: Some(meta.clone()),
        presence: Presence::Present,
        fs_created: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_line_has_the_three_forms_of_design_20() {
        assert_eq!(
            BuildSnapshots::On.line(),
            "on (btrfs subvolume; first snapshot after setup; the game dir leaves snapshots \
             of the subvolume around it)"
        );
        assert_eq!(
            BuildSnapshots::Off("--no-snapshots".into()).line(),
            "off (--no-snapshots)"
        );
        assert_eq!(
            BuildSnapshots::Unavailable(
                "/lib is on ext4; snapshots need btrfs, or a ZFS dataset per game".into()
            )
            .line(),
            "unavailable (/lib is on ext4; snapshots need btrfs, or a ZFS dataset per game)"
        );
        assert_eq!(
            BuildSnapshots::Skipped.line(),
            "off for this run (--no-snapshots; a rebuild: the game dir stays as it is)"
        );
        // `--no-snapshots` on a rebuild skips its snapshot whatever the game says.
        assert_eq!(
            BuildSnapshots::plan(
                &SnapshotOptions::no_snapshots(),
                Some("1"),
                Path::new("/nowhere/1"),
                true
            ),
            BuildSnapshots::Skipped
        );
        assert!(SnapshotOptions::no_snapshots().auto().skip);
        assert_eq!(SnapshotOptions::default().keep_auto, KEEP_AUTO);
        let off = SnapshotOptions::off("[snapshots] auto = false");
        assert_eq!(
            BuildSnapshots::plan(&off, None, Path::new("/nowhere/1"), false),
            BuildSnapshots::Off("[snapshots] auto = false".into())
        );
    }

    #[test]
    fn labels_name_the_runtime_build_and_pruned_lines_the_day() {
        assert_eq!(runtime_name("ge-proton/GE-Proton10-25"), "GE-Proton10-25");
        assert_eq!(runtime_name("path:/opt/GE-Proton10-30/"), "GE-Proton10-30");
        assert_eq!(runtime_name("GE-Proton10-25"), "GE-Proton10-25");
        assert_eq!(
            pruned_line(2, "before rebuild", "2026-10-30T09:12:00Z"),
            "pruned snapshot 2 (before rebuild, 2026-10-30)"
        );
    }
}
