//! Moving a built game to another dir (design 18 §6.1, #278), keeping its
//! prefix and saves. Everything here is about where the game's dir is; what a
//! lane has to do about it (what counts as running, which of its files name
//! the old place) is a [`Lane`]. Only Linux has one so far.
//!
//! In order:
//!
//! 1. A journal, `<data_root>/game-moves/<gogid>`, says where from and where
//!    to. Nothing else about a move needs to be believed after a crash.
//! 2. The data. On one filesystem, a rename. Across filesystems, a copy into
//!    `<target>/.moving-<gogid>/`, flushed, compared byte for byte, marked as
//!    ours and renamed into place.
//! 3. The game's own files inside the new dir get the new paths. Nothing can
//!    see that dir as the game yet.
//! 4. The commit: the location record is written, or removed for `games/`.
//!    This is the one step that changes where the game is. Before it, the game
//!    runs from the old place, however far the copy got.
//! 5. What's outside the dir (the menu entry) follows, the source goes, the
//!    journal goes.
//!
//! The next `move` (or `apps remove`) of a game with a journal first works out
//! which side of step 4 the last one died on ([`recover`]): before it, a copy
//! is deleted and the game stays; after it, the rest of step 5 runs. A rename
//! that died before the commit is the one case with nowhere to run from until
//! then, for as long as it takes to write one small file.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::error::MacAppError;
use super::launch::{self, GameDirs, Library};
use super::store;
use crate::gog::materialize::{self, TreeMethod};
use crate::snapshot::Guard;
use crate::snapshot::backend::snapshots_dir;

/// Where a game is going.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `<library>/<gogid>/`.
    Library(Library),
    /// `<data_root>/games/<gogid>/`, with no record.
    Default,
}

impl Target {
    /// The game's dir at this target.
    pub fn root(&self, gogid: &str) -> PathBuf {
        match self {
            Target::Library(l) => l.dir().join(gogid),
            Target::Default => launch::games_dir().join(gogid),
        }
    }
}

/// What a lane knows about its games.
pub trait Lane {
    /// Is this a game this lane can move?
    fn check_built(&self, gogid: &str, dirs: &GameDirs) -> Result<(), MacAppError>;
    /// Could the game run from `root`?
    fn check_target(&self, root: &Path) -> Result<(), MacAppError>;
    /// The game id `root` holds, for an adopt to compare with the one asked for.
    /// `None`: the lane's files there can't be read, and an adopt is refused.
    fn built_id(&self, root: &Path) -> Option<String>;
    /// After `rewrite_inside` for an adopt: do the game's own files name `to`?
    /// They may not when an earlier adopt stopped halfway, since the rewrite
    /// only knows the old place from the record.
    fn check_rewritten(&self, to: &GameDirs) -> Result<(), MacAppError>;
    /// Refuse while anything runs from the game's dir.
    fn refuse_running(&self, gogid: &str, dirs: &GameDirs) -> Result<(), MacAppError>;
    /// Point the game's own files at the new place. `at` is where they are
    /// right now, which isn't the game's dir yet; they may be rewritten twice.
    /// Returns warnings.
    fn rewrite_inside(
        &self,
        from: &GameDirs,
        to: &GameDirs,
        at: &Path,
    ) -> Result<Vec<String>, MacAppError>;
    /// The game now lives at `dirs`: rewrite what's outside its dir.
    fn publish(&self, gogid: &str, dirs: &GameDirs) -> Result<(), MacAppError>;

    /// What a move from `from` to `to` does with the game's snapshots
    /// (design 20 §9.1). `rename`: one filesystem. `None` for a lane without
    /// snapshots.
    fn plan_snapshots(
        &self,
        gogid: &str,
        from: &Path,
        to: &Path,
        rename: bool,
    ) -> Result<Option<SnapshotsPlan>, MacAppError> {
        let _ = (gogid, from, to, rename);
        Ok(None)
    }

    /// Make the copy target `staging` as a btrfs subvolume. `false`: it isn't
    /// there, and the copy makes a plain dir (a failure is a warning).
    fn make_staging(&self, staging: &Path, warnings: &mut Vec<String>) -> bool {
        let _ = (staging, warnings);
        false
    }

    /// After the commit, in step 5: delete the old place's snapshots and
    /// their metadata (`steps.drop`), take the `initial` snapshot at the new
    /// place (`steps.initial`). Runs again after a crash, so each part must
    /// be safe to repeat. Failures are warnings; `killed` is a test's kill.
    fn after_commit(
        &self,
        gogid: &str,
        from: &Path,
        to: &Path,
        steps: SnapshotSteps,
        guard: &Guard,
        killed: &dyn Fn(KilledAt) -> Result<(), MacAppError>,
        done: &mut SnapshotsDone,
    ) -> Result<Vec<String>, MacAppError> {
        let _ = (gogid, from, to, steps, guard, killed, done);
        Ok(Vec::new())
    }
}

/// How the data would move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    Rename,
    Reflink,
    Copy,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Rename => "rename",
            Method::Reflink => "reflink",
            Method::Copy => "copy",
        }
    }
}

/// What a move will do, before it does it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MovePlan {
    pub gogid: String,
    pub from: PathBuf,
    pub to: PathBuf,
    pub method: Method,
    /// The game's size.
    pub bytes: u64,
    /// Disk needed at the target while both copies exist.
    pub extra_bytes: u64,
    /// What happens to the game's snapshots (design 20 §9.1); `None` for a
    /// lane without them, or nothing to say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshots: Option<SnapshotsPlan>,
}

/// What a move does with the game's snapshots (design 20 §9.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotAction {
    /// One filesystem: they are renamed beside the game's new dir.
    Carry,
    /// Across filesystems: they are deleted after the commit, with their
    /// metadata (decision D5).
    Drop,
    /// The game has none.
    None,
}

/// One snapshot as a move's plan lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedSnapshot {
    pub id: u32,
    /// `None`: on disk without metadata.
    pub label: Option<String>,
    /// RFC 3339, UTC.
    pub created: Option<String>,
    /// What only it holds, last measured (or measured for the plan).
    pub bytes: Option<u64>,
}

/// The snapshot part of a [`MovePlan`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotsPlan {
    pub action: SnapshotAction,
    /// Newest first.
    pub snapshots: Vec<PlannedSnapshot>,
    /// The sum of the known sizes.
    pub bytes: u64,
    /// A copy: the new game dir is made a btrfs subvolume.
    pub subvolume: bool,
    /// A copy: an `initial` snapshot is taken after the commit (autosnapshot
    /// is on).
    pub initial: bool,
    /// A copy to a place without snapshots: why. The game's setting stays
    /// as it is and does nothing there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    /// The game's autosnapshot setting.
    pub auto: bool,
}

impl SnapshotsPlan {
    /// Deleting snapshots needs the user's yes (`--yes`, or the question).
    pub fn needs_yes(&self) -> bool {
        self.action == SnapshotAction::Drop && !self.snapshots.is_empty()
    }

    /// What the journal records for recovery.
    pub fn steps(&self) -> SnapshotSteps {
        SnapshotSteps {
            carry: self.action == SnapshotAction::Carry,
            drop: self.action == SnapshotAction::Drop,
            initial: self.initial,
        }
    }

    /// The plan's lines about snapshots; `to` is the game's new dir.
    pub fn lines(&self, to: &Path) -> Vec<String> {
        let n = self.snapshots.len();
        let plural = if n == 1 { "" } else { "s" };
        let mut out = Vec::new();
        match self.action {
            SnapshotAction::Carry => out.push(format!(
                "snapshots: {n} snapshot{plural} move with the game (one filesystem: a rename)"
            )),
            SnapshotAction::Drop => {
                out.push(format!(
                    "snapshots: {n} snapshot{plural} can't cross filesystems: deleted after the \
                     move, with their metadata ({}{} stored)",
                    if self.snapshots.iter().all(|s| s.bytes.is_some()) {
                        "~"
                    } else {
                        "at least ~"
                    },
                    crate::fmt::human_size(self.bytes)
                ));
                for s in &self.snapshots {
                    let day = s
                        .created
                        .as_deref()
                        .map_or("-", |c| c.get(..10).unwrap_or(c));
                    let size = s.bytes.map_or_else(
                        || "size unknown".to_string(),
                        |b| format!("~{}", crate::fmt::human_size(b)),
                    );
                    out.push(format!(
                        "  {}  {day}  {}  {size}",
                        s.id,
                        s.label.as_deref().unwrap_or("(no metadata)")
                    ));
                }
            }
            SnapshotAction::None => {}
        }
        if let Some(reason) = &self.unavailable {
            out.push(format!(
                "snapshots at {}: unavailable ({reason}); autosnapshot stays {} and does \
                 nothing there",
                to.display(),
                if self.auto { "on" } else { "off" }
            ));
        } else if self.subvolume {
            let parent = to.parent().unwrap_or(to);
            out.push(if self.initial {
                format!(
                    "snapshots at {}: the game dir is made a btrfs subvolume, and an `initial` \
                     snapshot (after moving to {}) is taken after the move",
                    to.display(),
                    parent.display()
                )
            } else {
                format!(
                    "snapshots at {}: the game dir is made a btrfs subvolume (autosnapshot \
                     stays off)",
                    to.display()
                )
            });
        }
        out
    }
}

/// What a move still has to do about snapshots, as its journal records it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SnapshotSteps {
    /// Rename `.gogo-snapshots/<gogid>/` beside the new dir, before the
    /// commit.
    pub carry: bool,
    /// Delete the old place's snapshots and their metadata, after the commit.
    pub drop: bool,
    /// Take an `initial` snapshot at the new place, after the commit.
    pub initial: bool,
}

impl SnapshotSteps {
    fn words(self) -> Vec<&'static str> {
        [
            (self.carry, "carry"),
            (self.drop, "drop"),
            (self.initial, "initial"),
        ]
        .into_iter()
        .filter_map(|(on, w)| on.then_some(w))
        .collect()
    }

    fn parse(line: &str) -> Self {
        let mut s = SnapshotSteps::default();
        for w in line.split(',').map(str::trim) {
            match w {
                "carry" => s.carry = true,
                "drop" => s.drop = true,
                "initial" => s.initial = true,
                _ => {}
            }
        }
        s
    }
}

/// A snapshot a move took.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TakenSnapshot {
    pub id: u32,
    pub label: String,
}

/// What a move did with the game's snapshots.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotsDone {
    /// Renamed beside the new dir.
    pub carried: Vec<u32>,
    /// Deleted with their metadata (a move across filesystems).
    pub deleted: Vec<u32>,
    /// Snapshots that couldn't be deleted, left on the old drive.
    pub left: Vec<PathBuf>,
    /// The `initial` snapshot at the new place.
    pub initial: Option<TakenSnapshot>,
}

impl SnapshotsDone {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// One line each for what happened.
    pub fn lines(&self) -> Vec<String> {
        let ids = |v: &[u32]| {
            v.iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut out = Vec::new();
        if !self.carried.is_empty() {
            out.push(format!(
                "snapshots {} moved with the game",
                ids(&self.carried)
            ));
        }
        if !self.deleted.is_empty() {
            out.push(format!(
                "snapshots {} deleted (they can't cross filesystems)",
                ids(&self.deleted)
            ));
        }
        if let Some(t) = &self.initial {
            out.push(format!("snapshot {} taken ({})", t.id, t.label));
        }
        out
    }
}

/// What a move did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveOutcome {
    #[serde(flatten)]
    pub plan: MovePlan,
    /// What the last, interrupted move left and what became of it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recovered: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// What happened to the game's snapshots.
    #[serde(rename = "snapshotsDone", skip_serializing_if = "SnapshotsDone::is_empty")]
    pub snapshots: SnapshotsDone,
}

/// A long move's progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveEvent {
    Copying { done: u64, total: u64 },
    Verifying { done: u64, total: u64 },
}

/// Where a test stops a move, as a kill would.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KilledAt {
    /// After this many files are copied.
    Copying(usize),
    /// Copied and compared, not yet at the target's name.
    BeforeRename,
    /// The data is in place; the record still says the old place.
    BeforeCommit,
    /// The record says the new place; the source is still there.
    BeforeCleanup,
    /// A rename: the data is at the target and this many of the game's
    /// snapshots are beside it; the rest are still beside the source.
    Carrying(usize),
    /// Across filesystems, after the commit: this many of the old place's
    /// snapshots are deleted.
    Dropping(usize),
    /// After the commit and the deletions, before the `initial` snapshot.
    BeforeInitial,
}

/// What tests can't get from a real disk.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Faults {
    /// Act as if `rename` said the target is on another filesystem.
    pub across_filesystems: bool,
    /// The disk fills up after this many files.
    pub disk_full_after: Option<usize>,
    /// The target has this much free space.
    pub free_bytes: Option<u64>,
    /// One byte of the copy goes bad before the comparison.
    pub corrupt_copy: bool,
    pub killed_at: Option<KilledAt>,
}

/// How to run a move.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MoveOptions {
    /// The user's yes to deleting the game's snapshots, which can't cross
    /// filesystems (design 20 §9.1, decision D5). Without it, such a move is
    /// refused.
    pub drop_snapshots: bool,
    #[doc(hidden)]
    pub faults: Faults,
}

const JOURNAL_DIR: &str = "game-moves";
const LOCK: &str = ".lock";
/// In a copy that has been compared: "this dir came from a move". Only a dir
/// with this in it is ever deleted as a leftover.
const MARKER: &str = ".gogo-moved";

fn journal_path(gogid: &str) -> PathBuf {
    crate::state::data_root().join(JOURNAL_DIR).join(gogid)
}

fn staging_of(to: &Path, gogid: &str) -> PathBuf {
    to.with_file_name(format!(".moving-{gogid}"))
}

/// A move's record of itself.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Journal {
    from: PathBuf,
    to: PathBuf,
    /// A third line, `snapshots: carry|drop|initial` (design 20 §9.1). A
    /// journal from before snapshots has none.
    snapshots: SnapshotSteps,
}

/// A journal whose paths aren't ones gogo would write (the same rule as a
/// location record, since a recovery deletes them) isn't believed.
fn read_journal(gogid: &str) -> Result<Option<Journal>, MacAppError> {
    let raw = match std::fs::read_to_string(journal_path(gogid)) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut lines = raw.lines();
    let mut path = || lines.next().and_then(|l| launch::parse_location(l, gogid));
    let Some((from, to)) = path().zip(path()) else {
        return Ok(None);
    };
    let snapshots = lines
        .next()
        .and_then(|l| l.strip_prefix("snapshots:"))
        .map(SnapshotSteps::parse)
        .unwrap_or_default();
    Ok(Some(Journal {
        from,
        to,
        snapshots,
    }))
}

fn write_journal(gogid: &str, j: &Journal) -> Result<(), MacAppError> {
    let mut text = format!("{}\n{}\n", j.from.display(), j.to.display());
    let words = j.snapshots.words();
    if !words.is_empty() {
        text.push_str(&format!("snapshots: {}\n", words.join(",")));
    }
    launch::write_atomic(&journal_path(gogid), &text)
}

fn clear_journal(gogid: &str) -> Result<(), MacAppError> {
    match std::fs::remove_file(journal_path(gogid)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Where an interrupted move of `gogid` was going, for `doctor`. A move
/// running right now looks the same.
pub fn interrupted(gogid: &str) -> Option<PathBuf> {
    if journal_path(gogid).exists() {
        return Some(
            read_journal(gogid)
                .ok()
                .flatten()
                .map_or_else(|| journal_path(gogid), |j| j.to),
        );
    }
    None
}

/// The data root's lock, `game-moves/.lock`: one move (or snapshot change,
/// design 20 §5.1) at a time over a data root, whatever the game. The kernel
/// drops the lock when the process dies, however it dies. Take it before
/// [`lock_game`].
pub fn lock_moves() -> Result<std::fs::File, MacAppError> {
    let dir = crate::state::data_root().join(JOURNAL_DIR);
    std::fs::create_dir_all(&dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(MacAppError::Apps(
            "another `gogo` is moving a game or changing its snapshots: wait for it to finish"
                .into(),
        )),
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// The game's own lock, `game-moves/<gogid>.lock`, held exclusively for the
/// whole of a move or a recovery (design 18 section 6.1). The launcher takes it
/// shared, without waiting, once it has registered itself: a launch that finds
/// it held is refused, and one that got in first is seen by the move's own
/// running-checks, which come after this. It is per game so that playing A never
/// stops a move of B; the kernel drops it when the process dies, so an
/// interrupted move never blocks a launch. Never waits: the launcher holds it
/// only for the instant of its try, so a hold seen here is a launch in that
/// instant or another process, and the move says so and can be run again.
pub fn lock_game(gogid: &str) -> Result<std::fs::File, MacAppError> {
    std::fs::create_dir_all(crate::state::data_root().join(JOURNAL_DIR))?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(
            crate::state::data_root()
                .join(JOURNAL_DIR)
                .join(format!("{gogid}.lock")),
        )?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(MacAppError::Apps(format!(
            "{gogid} is being launched or moved by another process: quit it first, or try again"
        ))),
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// What a move would do, or why it wouldn't. Creates nothing but
/// [`materialize::plan_tree`]'s probe files, removed again.
pub fn plan(
    gogid: &str,
    target: &Target,
    lane: &dyn Lane,
    opts: &MoveOptions,
) -> Result<MovePlan, MacAppError> {
    let from = GameDirs::for_game(gogid)?;
    let recorded = launch::recorded_location(gogid).is_some();
    let Ok(found) = from.root.symlink_metadata() else {
        return Err(if recorded {
            MacAppError::Library {
                path: from.root,
                reason: format!(
                    "{gogid} is recorded there and it isn't there (a drive not mounted?): \
                     mount it and move it then, `gogo linux-app move {gogid} --adopt <dir>` \
                     if it is somewhere else now, or `gogo apps remove {gogid}` to forget it"
                ),
            }
        } else {
            MacAppError::Apps(format!(
                "{gogid} isn't built here: see `gogo apps list`, or build it with --library"
            ))
        });
    };
    if found.file_type().is_symlink() {
        return Err(MacAppError::Library {
            path: from.root,
            reason: "it is a symlink, so the game is already somewhere gogo doesn't know \
                     about: replace the link with the real dir, then move it"
                .into(),
        });
    }
    lane.check_built(gogid, &from)?;

    let to = target.root(gogid);
    let (real_from, real_to) = (launch::real_path(&from.root), launch::real_path(&to));
    if real_from == real_to {
        return Err(MacAppError::Library {
            path: to,
            reason: format!("{gogid} is already there"),
        });
    }
    if real_to.starts_with(&real_from) {
        return Err(MacAppError::Library {
            path: to,
            reason: format!("it is inside {gogid}'s own dir ({})", from.root.display()),
        });
    }
    if to.symlink_metadata().is_ok() {
        return Err(MacAppError::Library {
            path: to,
            reason: "it already exists. If it is an old copy of this game, gogo won't guess \
                     which one is yours: remove or rename it first"
                .into(),
        });
    }
    lane.check_target(&to)?;

    let bytes = store::dir_size(&from.root)?;
    let near = materialize::existing_ancestor(&to).ok_or_else(|| MacAppError::Library {
        path: to.clone(),
        reason: "no part of the path exists".into(),
    })?;
    let (method, extra_bytes) =
        if same_filesystem(&from.root, near)? && !opts.faults.across_filesystems {
            (Method::Rename, 0)
        } else {
            copy_cost(&from.root, &to, bytes)
        };
    let snapshots = lane.plan_snapshots(gogid, &from.root, &to, method == Method::Rename)?;
    let plan = MovePlan {
        gogid: gogid.to_string(),
        from: from.root,
        to,
        method,
        bytes,
        extra_bytes,
        snapshots,
    };
    check_space(&plan, opts)?;
    Ok(plan)
}

/// Whether the game dir `from` can be renamed to a name under `near`: one
/// device, or (btrfs) a subvolume root in the same mount. A btrfs subvolume
/// has its own device number, but its root renames across the subvolumes of
/// one mount; a plain dir doesn't.
fn same_filesystem(from: &Path, near: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt as _;
    if from.metadata()?.dev() == near.metadata()?.dev() {
        return Ok(true);
    }
    #[cfg(target_os = "linux")]
    {
        let mount = |p: &Path| {
            rustix::fs::statx(
                rustix::fs::CWD,
                p,
                rustix::fs::AtFlags::empty(),
                rustix::fs::StatxFlags::MNT_ID,
            )
            .ok()
            .filter(|st| st.stx_mask & rustix::fs::StatxFlags::MNT_ID.bits() != 0)
            .map(|st| st.stx_mnt_id)
        };
        if let (Some(a), Some(b)) = (mount(from), mount(near))
            && a == b
            && crate::snapshot::btrfs::is_subvolume(from).unwrap_or(false)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// How a copy would go, and the disk it costs.
fn copy_cost(from: &Path, to: &Path, bytes: u64) -> (Method, u64) {
    match materialize::plan_tree(from, to, bytes).method {
        TreeMethod::Reflink => (Method::Reflink, 0),
        TreeMethod::Copy => (Method::Copy, bytes),
    }
}

/// A copy that can't fit is refused before it fills a disk.
fn check_space(plan: &MovePlan, opts: &MoveOptions) -> Result<(), MacAppError> {
    if plan.extra_bytes == 0 {
        return Ok(());
    }
    let near = materialize::existing_ancestor(&plan.to).ok_or_else(|| MacAppError::Library {
        path: plan.to.clone(),
        reason: "no part of the path exists".into(),
    })?;
    let free = match opts.faults.free_bytes {
        Some(free) => free,
        None => fs4::available_space(near)?,
    };
    if free < plan.extra_bytes {
        return Err(MacAppError::Library {
            path: plan.to.clone(),
            reason: format!(
                "a copy needs {} there and {} is free",
                crate::fmt::human_size(plan.extra_bytes),
                crate::fmt::human_size(free)
            ),
        });
    }
    Ok(())
}

/// An error after a recovery that did something: say what it did, so those
/// lines aren't lost with the refusal.
fn after_recovery(recovered: &[String], e: MacAppError) -> MacAppError {
    if recovered.is_empty() {
        return e;
    }
    let note = format!("(before that, recovery: {})", recovered.join("; "));
    match e {
        MacAppError::Library { path, reason } => MacAppError::Library {
            path,
            reason: format!("{reason} {note}"),
        },
        MacAppError::Apps(s) => MacAppError::Apps(format!("{s} {note}")),
        other => MacAppError::Apps(format!("{other} {note}")),
    }
}

/// Move `gogid` to `target`: see the module docs. An `Err` leaves the game
/// runnable where it was, unless it says to run the move again.
pub fn move_game(
    gogid: &str,
    target: &Target,
    lane: &dyn Lane,
    opts: &MoveOptions,
    emit: &mut dyn FnMut(&MoveEvent),
) -> Result<MoveOutcome, MacAppError> {
    super::compat::validate_gogid(gogid)?;
    let lock = lock_moves()?;
    let game_lock = lock_game(gogid)?;
    let guard = Guard::new(&lock, &game_lock);
    let recovered = recover_locked(gogid, lane, &guard)?;
    let mut plan = plan(gogid, target, lane, opts).map_err(|e| after_recovery(&recovered, e))?;
    let from = GameDirs::at(plan.from.clone());
    let to = GameDirs::at(plan.to.clone());
    lane.refuse_running(gogid, &from)
        .map_err(|e| after_recovery(&recovered, e))?;
    if let Some(s) = &plan.snapshots {
        refuse_dropping(gogid, s, opts).map_err(|e| after_recovery(&recovered, e))?;
    }

    let mut journal = Journal {
        from: plan.from.clone(),
        to: plan.to.clone(),
        snapshots: plan.snapshots.as_ref().map(|s| s.steps()).unwrap_or_default(),
    };
    write_journal(gogid, &journal)?;
    let mut done = SnapshotsDone::default();
    let moved = move_data(
        &mut plan,
        &mut journal,
        &from,
        &to,
        lane,
        opts,
        emit,
        &mut done,
    );
    let mut warnings = match moved {
        Ok(w) => w,
        Err(Failure::Killed(e)) => return Err(e),
        Err(Failure::Plain(e)) => {
            // Nothing has reached the target's name: the game never left.
            let _ = materialize::remove_tree(&staging_of(&plan.to, gogid));
            clear_journal(gogid)?;
            return Err(after_recovery(&recovered, e));
        }
    };
    killed(opts, KilledAt::BeforeCommit)?;
    commit(gogid, &plan.to).map_err(|e| {
        after_recovery(
            &recovered,
            MacAppError::Apps(format!(
                "the data is at {} but gogo couldn't record that ({e}): run the move again once \
                 gogo's data dir takes writes",
                plan.to.display()
            )),
        )
    })?;
    killed(opts, KilledAt::BeforeCleanup)?;
    let kill = |at| killed(opts, at);
    warnings.extend(
        finish(
            gogid,
            &from,
            &to,
            lane,
            journal.snapshots,
            &guard,
            &kill,
            &mut done,
        )
        .map_err(|e| after_recovery(&recovered, e))?,
    );
    Ok(MoveOutcome {
        plan,
        recovered,
        warnings,
        snapshots: done,
    })
}

/// A move that would delete the game's snapshots needs the user's yes.
fn refuse_dropping(
    gogid: &str,
    s: &SnapshotsPlan,
    opts: &MoveOptions,
) -> Result<(), MacAppError> {
    if !s.needs_yes() || opts.drop_snapshots {
        return Ok(());
    }
    let n = s.snapshots.len();
    Err(MacAppError::Apps(format!(
        "{gogid} has {n} snapshot{}, which can't cross filesystems and would be deleted after          the move: `gogo linux-app move` shows them and asks (--yes agrees)",
        if n == 1 { "" } else { "s" }
    )))
}

/// Rename one snapshot. A read-only btrfs subvolume can't move to another dir
/// (`EROFS`: its root's `..` changes), so the flag is cleared for the rename
/// and set again after it. A snapshot is always read-only, so one a crash
/// left writable is made read-only again when it is carried.
fn rename_snapshot(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use crate::snapshot::btrfs;
        let subvolume = btrfs::is_subvolume(from).unwrap_or(false);
        if subvolume && btrfs::is_readonly(from)? {
            btrfs::set_readonly(from, false)?;
        }
        if let Err(e) = std::fs::rename(from, to) {
            if subvolume {
                let _ = btrfs::set_readonly(from, true);
            }
            return Err(e);
        }
        if subvolume {
            btrfs::set_readonly(to, true)?;
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::fs::rename(from, to)
    }
}

/// Rename the game's snapshots, `.gogo-snapshots/<gogid>/<n>`, from beside
/// `from` to beside `to` (design 20 §9.1), one by one: a btrfs snapshot is a
/// subvolume root, which renames across the subvolumes of one mount where
/// the plain dir holding them wouldn't. Safe to run again: what is already
/// there is skipped. Returns the numbers moved.
fn carry_snapshots(
    from: &Path,
    to: &Path,
    killed_at: Option<KilledAt>,
) -> Result<Vec<u32>, Failure> {
    let (Some(old), Some(new)) = (snapshots_dir(from), snapshots_dir(to)) else {
        return Ok(Vec::new());
    };
    let mut names: Vec<std::ffi::OsString> = match std::fs::read_dir(&old) {
        Ok(rd) => rd
            .map(|e| e.map(|e| e.file_name()))
            .collect::<std::io::Result<_>>()?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    names.sort();
    let mut moved = Vec::new();
    if killed_at == Some(KilledAt::Carrying(0)) {
        return Err(Failure::Killed(MacAppError::Apps(
            "killed: carrying 0".into(),
        )));
    }
    if !names.is_empty() {
        std::fs::create_dir_all(&new)?;
    }
    for name in names {
        let dst = new.join(&name);
        if dst.symlink_metadata().is_ok() {
            return Err(Failure::Plain(MacAppError::Library {
                path: dst,
                reason: "a snapshot by that name is already there: gogo won't guess which one                          is the game's; move it out of the way"
                    .into(),
            }));
        }
        rename_snapshot(&old.join(&name), &dst)?;
        if let Some(n) = crate::snapshot::backend::parse_n(&name) {
            moved.push(n);
        }
        if killed_at == Some(KilledAt::Carrying(moved.len())) {
            return Err(Failure::Killed(MacAppError::Apps(format!(
                "killed: carrying {}",
                moved.len()
            ))));
        }
    }
    if let Some(parent) = old.parent() {
        let _ = std::fs::remove_dir(&old);
        let _ = std::fs::remove_dir(parent);
    }
    if let Some(parent) = new.parent() {
        let _ = std::fs::File::open(parent).and_then(|d| d.sync_all());
    }
    Ok(moved)
}

/// What an adopt will do, before it does it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptPlan {
    pub gogid: String,
    /// Where gogo thought the game was.
    pub from: PathBuf,
    /// Where it is.
    pub to: PathBuf,
}

/// What an adopt did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptOutcome {
    #[serde(flatten)]
    pub plan: AdoptPlan,
    /// What an interrupted move left and what became of it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recovered: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Where an adopt would put the record, or why it can't. Reads only.
fn plan_adopt(gogid: &str, target: &Target, lane: &dyn Lane) -> Result<AdoptPlan, MacAppError> {
    let from = GameDirs::for_game(gogid)?;
    let to = target.root(gogid);
    let refuse = |reason: String| MacAppError::Library {
        path: to.clone(),
        reason,
    };
    let Ok(found) = to.symlink_metadata() else {
        return Err(refuse(format!(
            "there is no dir there: {gogid} has to be at the path you give (a drive not \
             mounted?)"
        )));
    };
    if found.file_type().is_symlink() || !found.is_dir() {
        return Err(refuse(
            "it isn't a real dir (a symlink, or a file): give the game's own dir".into(),
        ));
    }
    if launch::real_path(&from.root) == launch::real_path(&to) {
        return Err(refuse(format!(
            "gogo already has {gogid} there: nothing to adopt (`gogo linux-app setup {gogid}` writes its menu \
             entry again)"
        )));
    }
    lane.check_built(gogid, &GameDirs::at(to.clone()))?;
    match lane.built_id(&to) {
        Some(id) if id == gogid => {}
        Some(id) => return Err(refuse(format!("that is game {id}, not {gogid}"))),
        None => {
            return Err(refuse(
                "its game file can't be read, so gogo can't tell whose game it is".into(),
            ));
        }
    }
    // What an interrupted move left isn't a game to adopt.
    if to.join(MARKER).symlink_metadata().is_ok() {
        return Err(refuse(
            "it is a copy an earlier move left (it holds a `.gogo-moved` marker): finish or \
             clean up that move first"
                .into(),
        ));
    }
    lane.check_target(&to)?;
    // Two copies are never chosen between by gogo: the old one has to be gone.
    // Only "not there" counts as gone: a stale mount's error says nothing.
    let old_there = match from.root.symlink_metadata() {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            return Err(MacAppError::Library {
                path: from.root.clone(),
                reason: format!(
                    "gogo can't tell whether the old place is there ({e}): fix that, then adopt"
                ),
            });
        }
    };
    if old_there {
        return Err(MacAppError::Library {
            path: from.root.clone(),
            reason: format!(
                "gogo has {gogid} at {}, and it is there; {} holds a game by that id too. gogo \
                 won't guess which one is yours: move or remove one, then adopt the other",
                from.root.display(),
                to.display()
            ),
        });
    }
    Ok(AdoptPlan {
        gogid: gogid.to_string(),
        from: from.root,
        to,
    })
}

/// What an adopt of `gogid` at `target` would do, or why it wouldn't.
pub fn adopt_plan(gogid: &str, target: &Target, lane: &dyn Lane) -> Result<AdoptPlan, MacAppError> {
    super::compat::validate_gogid(gogid)?;
    // The real run settles it first, which can change what is recorded.
    if journal_path(gogid).exists() {
        return Err(MacAppError::Apps(format!(
            "an earlier move of {gogid} left a journal ({}): the adopt settles it first, so \
             a dry run can't say what it would do",
            journal_path(gogid).display()
        )));
    }
    plan_adopt(gogid, target, lane)
}

/// Tell gogo that `gogid` is at `target` now, moving no data (design 18 §6.1):
/// for a drive that mounts somewhere else, or a dir moved by hand. Under the
/// same two locks a move takes, an interrupted move of the game is settled
/// first (and one that can't be is a refusal); then the steps of a move without
/// its data: the game's own files, the commit, the menu entry.
pub fn adopt_game(
    gogid: &str,
    target: &Target,
    lane: &dyn Lane,
) -> Result<AdoptOutcome, MacAppError> {
    super::compat::validate_gogid(gogid)?;
    let lock = lock_moves()?;
    let game_lock = lock_game(gogid)?;
    let guard = Guard::new(&lock, &game_lock);
    // A full copy and no source: recovery refuses it, because the source may be
    // on a drive that isn't mounted. Naming a path settles it (below).
    let copy_only = copy_only_journal(gogid)?;
    if let Some(j) = &copy_only
        && launch::real_path(&target.root(gogid)) == launch::real_path(&j.to)
    {
        return adopt_the_copy(gogid, j, lane, &guard);
    }
    let mut recovered = if copy_only.is_some() {
        Vec::new()
    } else {
        recover_locked(gogid, lane, &guard)?
    };
    // A journal that survives a recovery has work waiting on a game that is
    // running or a source that wouldn't go: what's recorded isn't settled.
    if copy_only.is_none() && journal_path(gogid).exists() {
        return Err(after_recovery(
            &recovered,
            MacAppError::Apps(format!(
                "an earlier move of {gogid} isn't finished ({}): run `gogo linux-app move \
                 {gogid} <dir>` or `apps remove` first",
                journal_path(gogid).display()
            )),
        ));
    }
    let plan = plan_adopt(gogid, target, lane).map_err(|e| after_recovery(&recovered, e))?;
    let (from, to) = (
        GameDirs::at(plan.from.clone()),
        GameDirs::at(plan.to.clone()),
    );
    lane.refuse_running(gogid, &to)
        .map_err(|e| after_recovery(&recovered, e))?;
    // The old place is gone, but a game started there may still be running.
    lane.refuse_running(gogid, &from)
        .map_err(|e| after_recovery(&recovered, e))?;
    if let Some(j) = &copy_only {
        // The game is at the path the user named (checked above: a real dir, this
        // game, no marker), so the marked copy is the spare and goes. Nothing is
        // deleted before that check; a kill here leaves the same journal.
        // Renamed to the staging name first (atomic): a kill or an I/O error
        // mid-delete must not leave a half-deleted dir that recovery could
        // record as the game. Recovery removes a staging dir.
        let (spare, named) = (launch::real_path(&j.to), launch::real_path(&plan.to));
        if spare.starts_with(&named) || named.starts_with(&spare) {
            return Err(MacAppError::Library {
                path: plan.to.clone(),
                reason: format!(
                    "it is nested with the unfinished copy at {}: give a path outside it",
                    j.to.display()
                ),
            });
        }
        let staging = staging_of(&j.to, gogid);
        std::fs::rename(&j.to, &staging)?;
        materialize::remove_tree(&staging)?;
        clear_journal(gogid)?;
        recovered.push(format!(
            "removed {}, a full copy an earlier move never recorded: the game is at {}",
            j.to.display(),
            plan.to.display()
        ));
    }

    let mut warnings = lane
        .rewrite_inside(&from, &to, &plan.to)
        .map_err(|e| after_recovery(&recovered, e))?;
    lane.check_rewritten(&to)
        .map_err(|e| after_recovery(&recovered, e))?;
    commit(gogid, &plan.to).map_err(|e| {
        after_recovery(
            &recovered,
            MacAppError::Apps(format!(
                "gogo couldn't record that {gogid} is at {} ({e}): run the adopt again once \
                 gogo's data dir takes writes",
                plan.to.display()
            )),
        )
    })?;
    if let Err(e) = lane.publish(gogid, &to) {
        warnings.push(format!(
            "{gogid} is recorded, but its menu entry didn't follow ({e}): `gogo linux-app setup \
             {gogid}` writes it again"
        ));
    }
    Ok(AdoptOutcome {
        plan,
        recovered,
        warnings,
    })
}

/// The journal of an interrupted move that left a full marked copy at its target
/// and no source dir, the one case recovery won't settle by itself.
fn copy_only_journal(gogid: &str) -> Result<Option<Journal>, MacAppError> {
    let Some(j) = read_journal(gogid)? else {
        return Ok(None);
    };
    let there = |p: &Path| p.symlink_metadata().is_ok();
    let current = GameDirs::for_game(gogid)?.root;
    Ok((current == j.from && !there(&j.from) && there(&j.to.join(MARKER))).then_some(j))
}

/// `--adopt` at the target of a copy-only move: "the copy is mine". The game's
/// files must be this game's. The marker goes (the only change to the copy; no
/// data is deleted), and the recovery that is left is the one a rename needs:
/// rewrite, record, menu entry, journal. A kill after the marker is gone is
/// settled by the next run's recovery the same way.
fn adopt_the_copy(
    gogid: &str,
    j: &Journal,
    lane: &dyn Lane,
    guard: &Guard,
) -> Result<AdoptOutcome, MacAppError> {
    let to = GameDirs::at(j.to.clone());
    let refuse = |reason: String| MacAppError::Library {
        path: j.to.clone(),
        reason,
    };
    lane.check_built(gogid, &to)?;
    match lane.built_id(&j.to) {
        Some(id) if id == gogid => {}
        Some(id) => return Err(refuse(format!("that is game {id}, not {gogid}"))),
        None => {
            return Err(refuse(
                "its game file can't be read, so gogo can't tell whose game it is".into(),
            ));
        }
    }
    lane.check_target(&j.to)?;
    lane.refuse_running(gogid, &to)?;
    lane.refuse_running(gogid, &GameDirs::at(j.from.clone()))?;
    std::fs::remove_file(j.to.join(MARKER))?;
    let mut recovered = recover_locked(gogid, lane, guard)?;
    recovered.insert(
        0,
        format!(
            "adopted the copy at {}, which an earlier move left (its source {} isn't there)",
            j.to.display(),
            j.from.display()
        ),
    );
    Ok(AdoptOutcome {
        plan: AdoptPlan {
            gogid: gogid.to_string(),
            from: j.from.clone(),
            to: j.to.clone(),
        },
        recovered,
        warnings: Vec::new(),
    })
}

enum Failure {
    /// A test's kill: everything stays as it is.
    Killed(MacAppError),
    Plain(MacAppError),
}

impl From<MacAppError> for Failure {
    fn from(e: MacAppError) -> Failure {
        Failure::Plain(e)
    }
}

impl From<std::io::Error> for Failure {
    fn from(e: std::io::Error) -> Failure {
        Failure::Plain(e.into())
    }
}

fn killed(opts: &MoveOptions, at: KilledAt) -> Result<(), MacAppError> {
    if opts.faults.killed_at == Some(at) {
        return Err(MacAppError::Apps(format!("killed: {at:?}")));
    }
    Ok(())
}

/// Steps 2 and 3: the data at the target's name, the game's files rewritten.
/// Returns warnings.
#[allow(clippy::too_many_arguments)]
fn move_data(
    plan: &mut MovePlan,
    journal: &mut Journal,
    from: &GameDirs,
    to: &GameDirs,
    lane: &dyn Lane,
    opts: &MoveOptions,
    emit: &mut dyn FnMut(&MoveEvent),
    done: &mut SnapshotsDone,
) -> Result<Vec<String>, Failure> {
    if let Some(parent) = plan.to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if !opts.faults.across_filesystems {
        match std::fs::rename(&plan.from, &plan.to) {
            Ok(()) => {
                // A rename: the snapshots follow it, whatever the plan
                // guessed.
                let renamed = SnapshotSteps {
                    carry: true,
                    ..SnapshotSteps::default()
                };
                let back = |e: MacAppError| {
                    // Nothing else knows it's there: it goes back.
                    let undone = carry_snapshots(&plan.to, &plan.from, None)
                        .map_err(|f| match f {
                            Failure::Killed(e) | Failure::Plain(e) => e,
                        })
                        .and_then(|_| Ok(std::fs::rename(&plan.to, &plan.from)?));
                    match undone {
                        Ok(()) => Failure::Plain(e),
                        Err(back) => Failure::Killed(MacAppError::Library {
                            path: plan.to.clone(),
                            reason: format!(
                                "the game moved here, its files couldn't follow ({e}) and it \
                                 couldn't go back ({back}): run the move again"
                            ),
                        }),
                    }
                };
                if journal.snapshots != renamed {
                    journal.snapshots = renamed;
                    if let Err(e) = write_journal(&plan.gogid, journal) {
                        return Err(back(e));
                    }
                }
                let warnings = lane.rewrite_inside(from, to, &plan.to).map_err(back)?;
                match carry_snapshots(&plan.from, &plan.to, opts.faults.killed_at) {
                    Ok(moved) => done.carried = moved,
                    Err(Failure::Killed(e)) => return Err(Failure::Killed(e)),
                    Err(Failure::Plain(e)) => return Err(back(e)),
                }
                return Ok(warnings);
            }
            // One device number and still no rename: a bind mount, say.
            Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
                (plan.method, plan.extra_bytes) = copy_cost(&plan.from, &plan.to, plan.bytes);
                // The snapshots can't follow a copy: the plan is a copy's now,
                // and it may need the yes it didn't ask for.
                plan.snapshots = lane.plan_snapshots(&plan.gogid, &plan.from, &plan.to, false)?;
                if let Some(s) = &plan.snapshots {
                    refuse_dropping(&plan.gogid, s, opts)?;
                }
                let steps = plan.snapshots.as_ref().map(|s| s.steps()).unwrap_or_default();
                if journal.snapshots != steps {
                    journal.snapshots = steps;
                    write_journal(&plan.gogid, journal)?;
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    copy_verified(plan, from, to, lane, opts, emit)
}

/// The copy of step 2, then step 3 in it, then its rename into place. The
/// source is only read.
fn copy_verified(
    plan: &MovePlan,
    from: &GameDirs,
    to: &GameDirs,
    lane: &dyn Lane,
    opts: &MoveOptions,
    emit: &mut dyn FnMut(&MoveEvent),
) -> Result<Vec<String>, Failure> {
    check_space(plan, opts)?;
    let staging = staging_of(&plan.to, &plan.gogid);
    // Whatever is here has no journal: a move this one has no record of.
    if staging.symlink_metadata().is_ok() {
        materialize::remove_tree(&staging)?;
    }
    let total = plan.bytes;
    let (mut done, mut files) = (0u64, 0usize);
    let mut was_killed = false;
    let mut warnings = Vec::new();
    // On btrfs, the new game dir is a subvolume from the start (design 20
    // §9.1), so it can have snapshots there.
    let into = plan.snapshots.as_ref().is_some_and(|s| s.subvolume)
        && lane.make_staging(&staging, &mut warnings);
    let mut on_file = |_: &Path, size| {
        done += size;
        files += 1;
        emit(&MoveEvent::Copying { done, total });
        if opts.faults.killed_at == Some(KilledAt::Copying(files)) {
            was_killed = true;
            return Err(std::io::Error::other("killed"));
        }
        if opts.faults.disk_full_after == Some(files) {
            return Err(std::io::ErrorKind::StorageFull.into());
        }
        Ok(())
    };
    let copied = if into {
        materialize::copy_exact_into(
            &plan.from,
            &staging,
            materialize::IntoOptions {
                reflink: materialize::Reflink::Try,
                copy_budget: None,
            },
            &mut on_file,
        )
    } else {
        materialize::copy_exact(&plan.from, &staging, &mut on_file)
    };
    if was_killed {
        return Err(Failure::Killed(MacAppError::Apps("killed: copying".into())));
    }
    let copied = copied.map_err(|e| {
        Failure::Plain(MacAppError::Library {
            path: plan.to.clone(),
            reason: format!(
                "the copy failed ({e}); the game wasn't touched and stays at {}",
                plan.from.display()
            ),
        })
    })?;
    if opts.faults.corrupt_copy {
        corrupt_a_file(&staging)?;
    }
    // Compared after the flush, so a write error the flush reports comes first.
    materialize::sync_filesystem(&staging)?;
    let mut done = 0u64;
    materialize::verify_exact(
        &plan.from,
        &staging,
        copied.xattrs_lost > 0,
        &mut |_, size| {
            done += size;
            emit(&MoveEvent::Verifying { done, total });
            Ok(())
        },
    )
    .map_err(|e| {
        Failure::Plain(MacAppError::Library {
            path: plan.to.clone(),
            reason: format!(
                "the copy doesn't match ({e}), so it was deleted; the game wasn't touched \
                 and stays at {}",
                plan.from.display()
            ),
        })
    })?;
    // It may have been launched during the copy. Saves written after the
    // comparison would be lost with the source.
    lane.refuse_running(&plan.gogid, from)?;
    std::fs::write(staging.join(MARKER), "")?;
    warnings.extend(lane.rewrite_inside(from, to, &staging)?);
    if opts.faults.killed_at == Some(KilledAt::BeforeRename) {
        return Err(Failure::Killed(MacAppError::Apps("killed: copied".into())));
    }
    materialize::sync_filesystem(&staging)?;
    std::fs::rename(&staging, &plan.to)?;
    // Nothing is recorded yet, so a copy that can't be made durable just goes.
    if let Some(parent) = plan.to.parent()
        && let Err(e) = std::fs::File::open(parent).and_then(|d| d.sync_all())
    {
        let _ = materialize::remove_tree(&plan.to);
        return Err(e.into());
    }
    if copied.xattrs_lost > 0 {
        warnings.push(format!(
            "{} extended attribute(s) stayed behind: {} doesn't take them (Wine keeps file \
             attributes there, and a game rarely notices)",
            copied.xattrs_lost,
            plan.to.display()
        ));
    }
    if !copied.skipped.is_empty() {
        warnings.push(format!(
            "{} socket(s) or FIFO(s) weren't copied: {}",
            copied.skipped.len(),
            copied.skipped[0].display()
        ));
    }
    Ok(warnings)
}

/// Flip a byte in a copied file, as bad memory or a bad cable would.
fn corrupt_a_file(dir: &Path) -> std::io::Result<()> {
    let path = dir.join("launch.sh");
    let mut bytes = std::fs::read(&path)?;
    bytes[0] ^= 0xff;
    std::fs::write(path, bytes)
}

/// Step 4: where the game is, from now on.
fn commit(gogid: &str, to: &Path) -> Result<(), MacAppError> {
    // A rename and its rewrites first: a record must never outlive the data it
    // names, or a recovery would trust it.
    materialize::sync_filesystem(to)?;
    if launch::in_library(to) {
        launch::record_location(gogid, to)?;
    } else {
        launch::forget_location(gogid)?;
    }
    // Before anything is deleted: a power cut mustn't undo the record and keep
    // the deletion.
    materialize::sync_filesystem(&crate::state::data_root())?;
    Ok(())
}

/// Step 5, which can run again. A failure here doesn't undo a move: the game is
/// fine where it is, so each one is a warning, and a source that wouldn't go
/// keeps the journal for the next run. The source only goes when the game is at
/// the target and nothing runs from the source: it may be all that's left of the
/// game, if a power cut undid the rest, or have a game writing saves to it.
#[allow(clippy::too_many_arguments)]
fn finish(
    gogid: &str,
    from: &GameDirs,
    to: &GameDirs,
    lane: &dyn Lane,
    steps: SnapshotSteps,
    guard: &Guard,
    killed: &dyn Fn(KilledAt) -> Result<(), MacAppError>,
    done: &mut SnapshotsDone,
) -> Result<Vec<String>, MacAppError> {
    let mut warnings = Vec::new();
    if let Err(e) = lane.publish(gogid, to) {
        warnings.push(format!(
            "the game moved, but its menu entry didn't follow ({e}): `gogo linux-app setup \
             {gogid}` writes it again"
        ));
    }
    if from.root.symlink_metadata().is_ok() {
        if let Err(e) = lane.refuse_running(gogid, from) {
            warnings.push(format!(
                "{} stays for now, because it is in use ({e}): any saves written since the \
                 move are there. Quit the game, then run the move again to finish",
                from.root.display()
            ));
            return Ok(warnings);
        }
        if let Err(e) = materialize::remove_tree(&from.root) {
            warnings.push(format!(
                "the game moved, but {} wouldn't go ({e}): delete it yourself, or run the \
                 move again",
                from.root.display()
            ));
            return Ok(warnings);
        }
    }
    // The snapshots, once the source is gone (so this runs once per move but
    // for a crash): the old place's deleted, the new place's first one.
    warnings.extend(lane.after_commit(gogid, &from.root, &to.root, steps, guard, killed, done)?);
    match std::fs::remove_file(to.root.join(MARKER)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    clear_journal(gogid)?;
    Ok(warnings)
}

/// Finish what an interrupted move of `gogid` left, if it left anything.
/// Returns what happened, one line each.
pub fn recover(gogid: &str, lane: &dyn Lane) -> Result<Vec<String>, MacAppError> {
    if !journal_path(gogid).exists() {
        return Ok(Vec::new());
    }
    let lock = lock_moves()?;
    let game_lock = lock_game(gogid)?;
    recover_locked(gogid, lane, &Guard::new(&lock, &game_lock))
}

/// [`recover`] for every game with a journal. One that can't be settled yet (a
/// drive that isn't mounted) becomes a line: it mustn't stop `apps remove`, which
/// is how to forget such a game, or anything about the others.
pub fn recover_all(lane: &dyn Lane) -> Vec<String> {
    let dir = crate::state::data_root().join(JOURNAL_DIR);
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if let Some(gogid) = e.file_name().to_str()
            && super::compat::validate_gogid(gogid).is_ok()
        {
            match recover(gogid, lane) {
                Ok(lines) => out.extend(lines),
                Err(e) => out.push(format!("{gogid}: {e}")),
            }
        }
    }
    out
}

fn recover_locked(
    gogid: &str,
    lane: &dyn Lane,
    guard: &Guard,
) -> Result<Vec<String>, MacAppError> {
    if !journal_path(gogid).exists() {
        return Ok(Vec::new());
    }
    let Some(j) = read_journal(gogid)? else {
        clear_journal(gogid)?;
        return Ok(vec![format!(
            "{}: not a journal gogo wrote; ignored and removed",
            journal_path(gogid).display()
        )]);
    };
    let (from, to) = (GameDirs::at(j.from.clone()), GameDirs::at(j.to.clone()));
    let staging = staging_of(&j.to, gogid);
    let current = GameDirs::for_game(gogid)?.root;
    let there = |p: &Path| p.symlink_metadata().is_ok();
    let mut out = Vec::new();

    // Past the commit: only cleaning up was left.
    if current == j.to {
        if lane.check_built(gogid, &to).is_err() {
            return Err(MacAppError::Library {
                path: j.to,
                reason: format!(
                    "{gogid} is recorded there and isn't there, and an earlier move left {} \
                     behind, which nothing was deleted from: mount the target if it is on a \
                     drive that isn't mounted, then run the move again",
                    j.from.display()
                ),
            });
        }
        out.push(format!(
            "an earlier move to {} had finished all but its cleanup; done now",
            j.to.display()
        ));
        let mut done = SnapshotsDone::default();
        out.extend(finish(
            gogid,
            &from,
            &to,
            lane,
            j.snapshots,
            guard,
            &|_| Ok(()),
            &mut done,
        )?);
        out.extend(done.lines());
        return Ok(out);
    }
    // A rename leaves no source and no marker: the game is at the target, and
    // only the commit was left.
    if current == j.from
        && !there(&j.from)
        && lane.check_built(gogid, &to).is_ok()
        && !there(&j.to.join(MARKER))
    {
        out.extend(lane.rewrite_inside(&from, &to, &j.to)?);
        // The snapshots follow the rename before the commit, as in the move
        // itself; the ones already there are skipped.
        let mut done = SnapshotsDone::default();
        done.carried = carry_snapshots(&j.from, &j.to, None).map_err(|f| match f {
            Failure::Killed(e) | Failure::Plain(e) => e,
        })?;
        commit(gogid, &j.to)?;
        out.extend(finish(
            gogid,
            &from,
            &to,
            lane,
            j.snapshots,
            guard,
            &|_| Ok(()),
            &mut done,
        )?);
        out.extend(done.lines());
        out.insert(
            0,
            format!(
                "an earlier move to {} stopped just before recording it; recorded now",
                j.to.display()
            ),
        );
        return Ok(out);
    }
    // A copy with no source: the source may be on a drive that isn't mounted
    // and may have newer saves. That isn't for gogo to settle.
    if current == j.from && !there(&j.from) && there(&j.to.join(MARKER)) {
        return Err(MacAppError::Library {
            path: j.from.clone(),
            reason: format!(
                "an earlier move left a full copy at {to}, and the game's own dir {from} isn't \
                 there (a drive not mounted, or mounted somewhere else now?). Nothing was \
                 deleted, and the way out depends on which copy is the game: if the drive can \
                 be mounted at {from} again, mount it and run `gogo linux-app move {gogid} \
                 <dir>` again (that cleans the copy up); if the game is at another path now, \
                 run `gogo linux-app move {gogid} --adopt <that path>` (that drops the copy); \
                 if the copy is the one to keep, run `gogo linux-app move {gogid} --adopt {to}`",
                to = j.to.display(),
                from = j.from.display(),
            ),
        });
    }
    // Before the commit, or the game has gone since: what the move made goes.
    if there(&staging) {
        materialize::remove_tree(&staging)?;
        out.push(format!(
            "removed {}, a copy an earlier move didn't finish",
            staging.display()
        ));
    }
    if there(&j.to.join(MARKER)) {
        materialize::remove_tree(&j.to)?;
        out.push(format!(
            "removed {}, a copy an earlier move never recorded",
            j.to.display()
        ));
    }
    clear_journal(gogid)?;
    Ok(out)
}
