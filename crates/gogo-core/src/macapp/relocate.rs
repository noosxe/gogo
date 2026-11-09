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
    /// `None`: the lane's files there don't say.
    fn built_id(&self, root: &Path) -> Option<String>;
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
    Ok(path().zip(path()).map(|(from, to)| Journal { from, to }))
}

fn write_journal(gogid: &str, j: &Journal) -> Result<(), MacAppError> {
    launch::write_atomic(
        &journal_path(gogid),
        &format!("{}\n{}\n", j.from.display(), j.to.display()),
    )
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

/// One move at a time over a data root, whatever the game: the kernel drops the
/// lock when the process dies, however it dies.
fn lock_moves() -> Result<std::fs::File, MacAppError> {
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
            "another `gogo` is moving a game: wait for it to finish".into(),
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
fn lock_game(gogid: &str) -> Result<std::fs::File, MacAppError> {
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
    let same_fs = {
        use std::os::unix::fs::MetadataExt as _;
        from.root.metadata()?.dev() == near.metadata()?.dev()
    };
    let (method, extra_bytes) = if same_fs && !opts.faults.across_filesystems {
        (Method::Rename, 0)
    } else {
        copy_cost(&from.root, &to, bytes)
    };
    let plan = MovePlan {
        gogid: gogid.to_string(),
        from: from.root,
        to,
        method,
        bytes,
        extra_bytes,
    };
    check_space(&plan, opts)?;
    Ok(plan)
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
    let _lock = lock_moves()?;
    let _game_lock = lock_game(gogid)?;
    let recovered = recover_locked(gogid, lane)?;
    let mut plan = plan(gogid, target, lane, opts)?;
    let from = GameDirs::at(plan.from.clone());
    let to = GameDirs::at(plan.to.clone());
    lane.refuse_running(gogid, &from)?;

    write_journal(
        gogid,
        &Journal {
            from: plan.from.clone(),
            to: plan.to.clone(),
        },
    )?;
    let moved = move_data(&mut plan, &from, &to, lane, opts, emit);
    let mut warnings = match moved {
        Ok(w) => w,
        Err(Failure::Killed(e)) => return Err(e),
        Err(Failure::Plain(e)) => {
            // Nothing has reached the target's name: the game never left.
            let _ = materialize::remove_tree(&staging_of(&plan.to, gogid));
            clear_journal(gogid)?;
            return Err(e);
        }
    };
    killed(opts, KilledAt::BeforeCommit)?;
    commit(gogid, &plan.to).map_err(|e| {
        MacAppError::Apps(format!(
            "the data is at {} but gogo couldn't record that ({e}): run the move again once \
             gogo's data dir takes writes",
            plan.to.display()
        ))
    })?;
    killed(opts, KilledAt::BeforeCleanup)?;
    warnings.extend(finish(gogid, &from, &to, lane)?);
    Ok(MoveOutcome {
        plan,
        recovered,
        warnings,
    })
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
            "gogo already has {gogid} there: nothing to adopt (`gogo linux-app setup {gogid}`              writes its menu entry again)"
        )));
    }
    lane.check_built(gogid, &GameDirs::at(to.clone()))?;
    if let Some(id) = lane.built_id(&to)
        && id != gogid
    {
        return Err(refuse(format!("that is game {id}, not {gogid}")));
    }
    lane.check_target(&to)?;
    // Two copies are never chosen between by gogo: the old one has to be gone.
    if from.root.symlink_metadata().is_ok() {
        return Err(MacAppError::Library {
            path: from.root.clone(),
            reason: format!(
                "gogo has {gogid} at {}, and it is there; {} holds a game by that id too. gogo                  won't guess which one is yours: move or remove one, then adopt the other",
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
    let _lock = lock_moves()?;
    let _game_lock = lock_game(gogid)?;
    let recovered = recover_locked(gogid, lane)?;
    // A journal that survives a recovery has work waiting on a game that is
    // running or a source that wouldn't go: what's recorded isn't settled.
    if journal_path(gogid).exists() {
        return Err(MacAppError::Apps(format!(
            "an earlier move of {gogid} isn't finished ({}): run `gogo linux-app move {gogid}              <dir>` or `apps remove` first",
            journal_path(gogid).display()
        )));
    }
    let plan = plan_adopt(gogid, target, lane)?;
    let (from, to) = (
        GameDirs::at(plan.from.clone()),
        GameDirs::at(plan.to.clone()),
    );
    lane.refuse_running(gogid, &to)?;
    // The old place is gone, but a game started there may still be running.
    lane.refuse_running(gogid, &from)?;

    let mut warnings = lane.rewrite_inside(&from, &to, &plan.to)?;
    commit(gogid, &plan.to).map_err(|e| {
        MacAppError::Apps(format!(
            "gogo couldn't record that {gogid} is at {} ({e}): run the adopt again once \
             gogo's data dir takes writes",
            plan.to.display()
        ))
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
fn move_data(
    plan: &mut MovePlan,
    from: &GameDirs,
    to: &GameDirs,
    lane: &dyn Lane,
    opts: &MoveOptions,
    emit: &mut dyn FnMut(&MoveEvent),
) -> Result<Vec<String>, Failure> {
    if let Some(parent) = plan.to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if !opts.faults.across_filesystems {
        match std::fs::rename(&plan.from, &plan.to) {
            Ok(()) => {
                return lane.rewrite_inside(from, to, &plan.to).map_err(|e| {
                    // Nothing else knows it's there: it goes back.
                    match std::fs::rename(&plan.to, &plan.from) {
                        Ok(()) => Failure::Plain(e),
                        Err(back) => Failure::Killed(MacAppError::Library {
                            path: plan.to.clone(),
                            reason: format!(
                                "the game moved here, its files couldn't follow ({e}) and it \
                                 couldn't go back ({back}): run the move again"
                            ),
                        }),
                    }
                });
            }
            // One device number and still no rename: a bind mount, say.
            Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
                (plan.method, plan.extra_bytes) = copy_cost(&plan.from, &plan.to, plan.bytes);
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
    let copied = materialize::copy_exact(&plan.from, &staging, &mut |_, size| {
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
    });
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
    let mut warnings = lane.rewrite_inside(from, to, &staging)?;
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
fn finish(
    gogid: &str,
    from: &GameDirs,
    to: &GameDirs,
    lane: &dyn Lane,
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
    let _lock = lock_moves()?;
    let _game_lock = lock_game(gogid)?;
    recover_locked(gogid, lane)
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

fn recover_locked(gogid: &str, lane: &dyn Lane) -> Result<Vec<String>, MacAppError> {
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
        out.extend(finish(gogid, &from, &to, lane)?);
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
        commit(gogid, &j.to)?;
        out.extend(finish(gogid, &from, &to, lane)?);
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
            path: j.from,
            reason: format!(
                "an earlier move left a full copy at {}, and the game's own dir isn't there \
                 (a drive not mounted?): mount it and run the move again",
                j.to.display()
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
