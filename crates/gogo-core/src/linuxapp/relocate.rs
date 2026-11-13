//! `gogo linux-app move` (design 18 §6.1): the Linux side of
//! [`crate::macapp::relocate`].
//!
//! What a moved prefix needs, read from real ones (#278). Nothing in it:
//!
//! - Its absolute symlinks (about 1300 in `drive_c/windows` and `Program
//!   Files`) lead to the Proton build, which doesn't move, and `dosdevices/z:`
//!   to `/`. None leads into the prefix.
//! - Its links into itself are relative: `pfx`, `dosdevices/c:`, the
//!   `steamuser` ones, and `drive_c/GOG/<name>` to `../../../game`.
//! - The registry holds `C:` paths, and `Z:` only for what's in Proton's own
//!   build or the container.
//! - `config_info` names the Proton build. Its `version` and `tracked_files`
//!   hold no paths.
//!
//! So the installer lane has always been moving prefixes: it renames one out of
//! `.build-<slug>-<pid>/`. A game that wrote its own path into the registry
//! would be the exception, and [`prefix_mentions`] warns about it rather than
//! rewriting what the game owns.

use std::path::{Path, PathBuf};

use super::game::{self, LAUNCH_TOML};
use super::snapshots::{self, Backends};
use super::{launcher, setup};
use crate::macapp::error::MacAppError;
use crate::macapp::launch::{GameDirs, windows_to_unix, write_atomic};
use crate::macapp::relocate::{
    self, KilledAt, Lane, MoveEvent, MoveOptions, MoveOutcome, MovePlan, PlannedSnapshot,
    SnapshotAction, SnapshotSteps, SnapshotsDone, SnapshotsPlan, TakenSnapshot, Target,
};
use crate::snapshot::{Guard, Kind, NewSnapshot, State, Support, backend, labels};

/// The Linux lane's part of a move. `backends`: which snapshot backend
/// serves a dir (the filesystem's, or a fake in tests).
#[derive(Debug, Clone, Default)]
pub struct LinuxLane {
    pub backends: Backends,
}

impl Lane for LinuxLane {
    fn check_built(&self, gogid: &str, dirs: &GameDirs) -> Result<(), MacAppError> {
        if dirs.root.join(LAUNCH_TOML).is_file() {
            return Ok(());
        }
        Err(MacAppError::Apps(format!(
            "{} has no {LAUNCH_TOML}: {gogid} isn't a `gogo linux-app` game",
            dirs.root.display()
        )))
    }

    fn check_target(&self, root: &Path) -> Result<(), MacAppError> {
        game::refuse_private_tmp("the game's new dir", root)
    }

    fn built_id(&self, root: &Path) -> Option<String> {
        game::read_launch_toml(&GameDirs::at(root.to_path_buf()))
            .ok()
            .map(|(toml, _)| toml.gogid)
    }

    fn check_rewritten(&self, to: &GameDirs) -> Result<(), MacAppError> {
        let Ok(text) = std::fs::read_to_string(&to.launch_conf) else {
            return Ok(());
        };
        let conf = game::ConfValues::parse(&text);
        let data_root = crate::state::data_root();
        for key in ["PREFIX", "GAME_DIR"] {
            let Some(v) = conf.values.get(key) else {
                continue;
            };
            let at = data_root.join(v);
            if !at.starts_with(&to.root) {
                return Err(MacAppError::Library {
                    path: to.root.clone(),
                    reason: format!(
                        "its launch.conf names {} ({key}), another place than this: an earlier \
                         adopt probably stopped halfway. Adopt the dir it names first, or put \
                         this one there",
                        at.display()
                    ),
                });
            }
        }
        Ok(())
    }

    fn refuse_running(&self, gogid: &str, dirs: &GameDirs) -> Result<(), MacAppError> {
        game::refuse_running(gogid, &dirs.root, &dirs.prefix)
    }

    fn rewrite_inside(
        &self,
        from: &GameDirs,
        to: &GameDirs,
        at: &Path,
    ) -> Result<Vec<String>, MacAppError> {
        let mut warnings = Vec::new();
        let conf = at.join("launch.conf");
        match std::fs::read_to_string(&conf) {
            Ok(text) => {
                let data_root = crate::state::data_root();
                let new = game::relocated_conf(&text, &from.root, &to.root, &data_root);
                if new != text {
                    write_atomic(&conf, &new)?;
                }
                let old = from.root.to_string_lossy();
                if new.contains(old.as_ref()) {
                    warnings.push(format!(
                        "launch.conf still names {old} in the game's args or env, which gogo \
                         leaves as you set them: see `gogo linux-app env list {id}` and `args`",
                        id = from.root.file_name().unwrap_or_default().to_string_lossy()
                    ));
                }
            }
            // Built, never set up: the first launch writes it, for the new place.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let prefix = at.join("prefix");
        for file in prefix_mentions(&prefix, &from.root) {
            warnings.push(format!(
                "{file} in the prefix records the game's old path ({}): the game wrote it, so \
                 gogo left it; if the game misbehaves, its own settings may need the new path",
                from.root.display()
            ));
        }
        Ok(warnings)
    }

    /// `launch.sh`, `icon.png` and the menu entry, through setup's writer. No
    /// setup: it could need the network or Wine, and the game has already moved.
    /// Everything setup would write differently is in `launch.conf`, and its
    /// stamp is about the config, not where the game is.
    fn publish(&self, gogid: &str, dirs: &GameDirs) -> Result<(), MacAppError> {
        let (baked, _) = game::read_launch_toml(dirs)?;
        let files = setup::game_files(dirs, baked.tree_name().as_deref());
        let exe = windows_to_unix(&dirs.prefix, &baked.exe);
        launcher::write(dirs, gogid, &baked.title, Some(&files), exe.as_deref())?;
        Ok(())
    }

    fn plan_snapshots(
        &self,
        gogid: &str,
        from: &Path,
        to: &Path,
        rename: bool,
    ) -> Result<Option<SnapshotsPlan>, MacAppError> {
        plan_snapshots(&self.backends, gogid, from, to, rename)
    }

    fn make_staging(&self, staging: &Path, warnings: &mut Vec<String>) -> bool {
        let (_, b) = self.backends.for_dir(staging);
        match b.make_game_dir(staging) {
            Ok(()) => true,
            Err(e) => {
                warnings.push(format!(
                    "couldn't make {} a btrfs subvolume ({e}): the game is a plain dir there, \
                     without snapshots (`gogo linux-app snapshot <gogid> auto on` converts it)",
                    staging.display()
                ));
                false
            }
        }
    }

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
        let mut warnings = Vec::new();
        if steps.drop {
            warnings.extend(drop_snapshots(
                &self.backends,
                gogid,
                from,
                to,
                guard,
                killed,
                done,
            )?);
        }
        if steps.initial {
            killed(KilledAt::BeforeInitial)?;
            match take_initial(&self.backends, gogid, to, guard) {
                Ok(t) => done.initial = t,
                Err(e) => warnings.push(format!(
                    "the game moved, but its first snapshot there couldn't be taken ({e}): \
                     `gogo linux-app snapshot {gogid} create` takes one"
                )),
            }
        }
        Ok(warnings)
    }
}

/// What a move does with the snapshots of `gogid` (design 20 §9.1): carry
/// them on one filesystem, drop them across filesystems; and, for a copy,
/// what the target offers. Reads only (the space of snapshots never measured
/// is measured, which only reads).
fn plan_snapshots(
    backends: &Backends,
    gogid: &str,
    from: &Path,
    to: &Path,
    rename: bool,
) -> Result<Option<SnapshotsPlan>, MacAppError> {
    let (source, b) = backends.for_dir(from);
    let s = snapshots::store(gogid, from, b);
    let listing = s.list().map_err(snapshots::err)?;
    let auto = listing.settings.auto;
    let unmeasured: Vec<u32> = listing
        .entries
        .iter()
        .filter(|e| e.meta.as_ref().and_then(|m| m.space).is_none())
        .filter(|e| e.presence != crate::snapshot::Presence::Missing)
        .map(|e| e.id)
        .collect();
    let measured = if unmeasured.is_empty() || rename {
        Vec::new()
    } else {
        b.space(from, &unmeasured).unwrap_or_default()
    };
    let snapshots: Vec<PlannedSnapshot> = listing
        .entries
        .iter()
        .map(|e| PlannedSnapshot {
            id: e.id,
            label: e.meta.as_ref().map(|m| m.label.clone()),
            created: e
                .meta
                .as_ref()
                .map(|m| m.created)
                .or_else(|| e.fs_created.map(chrono::DateTime::<chrono::Utc>::from))
                .map(snapshots::rfc3339),
            bytes: e
                .meta
                .as_ref()
                .and_then(|m| m.space)
                .map(|sp| sp.exclusive_bytes)
                .or_else(|| {
                    measured
                        .iter()
                        .find(|m| m.n == e.id)
                        .map(|m| m.exclusive_bytes)
                })
                .or_else(|| (e.presence == crate::snapshot::Presence::Missing).then_some(0)),
        })
        .collect();
    let action = match (snapshots.is_empty(), rename) {
        (true, _) => SnapshotAction::None,
        (false, true) => SnapshotAction::Carry,
        (false, false) => SnapshotAction::Drop,
    };
    let (mut subvolume, mut initial, mut unavailable) = (false, false, None);
    if !rename {
        match backends.probe(to) {
            Support::Btrfs => {
                subvolume = auto || b.ready(from).unwrap_or(false);
                initial = subvolume && auto;
            }
            // Not implemented yet (#409): a plain dir, as on ext4.
            Support::Zfs { .. } => {
                unavailable = Some("ZFS: gogo doesn't support it yet".to_string());
            }
            Support::None(reason) => unavailable = Some(reason),
        }
        // A game that never had snapshots, going where there are none, has
        // nothing to say about them.
        if unavailable.is_some() && !auto && snapshots.is_empty() && source != Support::Btrfs {
            unavailable = None;
        }
    }
    // Snapshots by this game's name beside the target already (an earlier
    // `--adopt`, a move by hand, a delete that failed): carrying would clash
    // with them, and a delete or `initial` would mix them up with the game's.
    if (action != SnapshotAction::None || initial)
        && let Some(dir) = backend::snapshots_dir(to)
        && dir.symlink_metadata().is_ok()
    {
        return Err(MacAppError::Library {
            path: dir,
            reason: format!(
                "snapshots of {gogid} are already there (left by an earlier `--adopt`, a move by \
                 hand, or a delete that failed): gogo won't mix them with the game's. Look at \
                 them, then remove or rename that dir, and move again"
            ),
        });
    }
    if action == SnapshotAction::None && !subvolume && unavailable.is_none() {
        return Ok(None);
    }
    Ok(Some(SnapshotsPlan {
        action,
        bytes: snapshots.iter().filter_map(|s| s.bytes).sum(),
        snapshots,
        subvolume,
        initial,
        unavailable,
        auto,
    }))
}

/// After a move across filesystems commits: delete the snapshots beside the
/// old place and their metadata (design 20 §9.1, decision D5). A snapshot that
/// won't go loses its metadata (it can't be rolled back to from the new
/// place) and is recorded as a leftover, which `doctor` reports and the next
/// settle tries again. Returns warnings.
fn drop_snapshots(
    backends: &Backends,
    gogid: &str,
    from: &Path,
    to: &Path,
    guard: &Guard,
    killed: &dyn Fn(KilledAt) -> Result<(), MacAppError>,
    done: &mut SnapshotsDone,
) -> Result<Vec<String>, MacAppError> {
    let mut warnings = Vec::new();
    let (_, b) = backends.for_dir(from);
    let s = snapshots::store(gogid, from, b);
    if let Err(e) = s.recover(guard) {
        warnings.push(format!(
            "an interrupted snapshot change at the old place couldn't be settled ({e})"
        ));
    }
    let entries = match s.list() {
        Ok(l) => l.entries,
        Err(e) => {
            warnings.push(format!(
                "the old place's snapshots couldn't be listed ({e}), so none were deleted: \
                 `gogo linux-app doctor {gogid}` shows what is left"
            ));
            return Ok(warnings);
        }
    };
    let mut left = s.leftovers().unwrap_or_default();
    // The metadata is the game's, wherever it is: a snapshot taken at the new
    // place (the `initial` one, when a recovery runs this again) isn't the
    // old place's to delete. The plan refused a target with snapshots of
    // this game beside it already, so nothing else there shares a number.
    let entries: Vec<_> = entries
        .into_iter()
        .filter(|e| e.meta.as_ref().is_none_or(|m| m.game_dir != to))
        .collect();
    let reachable = from.parent().is_some_and(Path::is_dir);
    for (i, e) in entries.iter().enumerate() {
        killed(KilledAt::Dropping(i))?;
        match s.delete(guard, e.id) {
            Ok(_) => done.deleted.push(e.id),
            Err(err) => {
                // Recorded unless it is known to be gone: a drive that isn't
                // there (or a filesystem gogo can't delete on now) may still
                // hold it.
                let at = backend::snapshot_dir(from, e.id)
                    .filter(|p| p.symlink_metadata().is_ok() || !reachable);
                if let Some(at) = at {
                    warnings.push(if reachable {
                        format!(
                            "snapshot {} couldn't be deleted ({err}): it is left at {}, holding \
                             space on that drive; `gogo linux-app snapshot {gogid} list` tries \
                             again",
                            e.id,
                            at.display()
                        )
                    } else {
                        format!(
                            "snapshot {} couldn't be deleted ({err}): the old place isn't there \
                             (a drive not mounted?), so it may still be at {}; `gogo linux-app \
                             snapshot {gogid} list` tries again once it is",
                            e.id,
                            at.display()
                        )
                    });
                    if !left.contains(&at) {
                        left.push(at.clone());
                    }
                    done.left.push(at);
                }
                if let Err(e2) = s.forget_entry(guard, e.id) {
                    warnings.push(format!(
                        "snapshot {}'s metadata couldn't be removed either ({e2})",
                        e.id
                    ));
                }
            }
        }
    }
    if let Err(e) = s.set_leftovers(guard, &left) {
        warnings.push(format!("the leftover snapshots couldn't be recorded ({e})"));
    }
    if let Some(dir) = backend::snapshots_dir(from) {
        let _ = std::fs::remove_dir(&dir);
        if let Some(parent) = dir.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
    Ok(warnings)
}

/// After a move onto a filesystem with snapshots: the game's first snapshot
/// there, `after moving to <dir>` (design 20 §9.1), when autosnapshot is on and
/// the new dir can be snapshotted. Once per move: a recovery that finds it
/// taken already takes no second one.
fn take_initial(
    backends: &Backends,
    gogid: &str,
    to: &Path,
    guard: &Guard,
) -> Result<Option<TakenSnapshot>, MacAppError> {
    let (support, b) = backends.for_dir(to);
    let s = snapshots::store(gogid, to, b);
    if matches!(support, Support::None(_) | Support::Zfs { .. })
        || !s.settings().map_err(snapshots::err)?.auto
        || !b.ready(to).unwrap_or(false)
    {
        return Ok(None);
    }
    s.recover(guard).map_err(snapshots::err)?;
    let label = labels::moved(to.parent().unwrap_or(to));
    let newest = s.metadata().map_err(snapshots::err)?.into_iter().last();
    if let Some(m) = newest
        && m.kind == Kind::Initial
        && m.state == State::Done
        && m.game_dir == to
        && m.label == label
    {
        return Ok(Some(TakenSnapshot {
            id: m.id,
            label: m.label,
        }));
    }
    let meta = s
        .create(
            guard,
            NewSnapshot {
                kind: Kind::Initial,
                trigger: None,
                label: Some(label),
                content: snapshots::content_of(&GameDirs::at(to.to_path_buf())),
            },
        )
        .map_err(snapshots::err)?;
    Ok(Some(TakenSnapshot {
        id: meta.id,
        label: meta.label,
    }))
}

/// Delete the snapshots a move across filesystems left behind (design 20
/// §9.1), the ones `leftovers.toml` names. Only a path shaped like one of this
/// game's snapshots (`…/.gogo-snapshots/<gogid>/<n>`) is touched; one that is
/// gone is dropped from the list. Lines for what went.
pub(crate) fn settle_leftovers(
    gogid: &str,
    backends: &Backends,
) -> Result<Vec<String>, MacAppError> {
    let dirs = GameDirs::for_game(gogid)?;
    let (_, b) = backends.for_dir(&dirs.root);
    if snapshots::store(gogid, &dirs.root, b)
        .leftovers()
        .map_err(snapshots::err)?
        .is_empty()
    {
        return Ok(Vec::new());
    }
    let root_lock = relocate::lock_moves()?;
    let game_lock = relocate::lock_game(gogid)?;
    let guard = Guard::new(&root_lock, &game_lock);
    let s = snapshots::store(gogid, &dirs.root, b);
    let mut keep = Vec::new();
    let mut out = Vec::new();
    for p in s.leftovers().map_err(snapshots::err)? {
        match leftover(gogid, &p, &dirs.root) {
            Leftover::Forget => continue,
            Leftover::Unreachable => keep.push(p),
            Leftover::There => {
                let (_, at) = backends.for_dir(&p);
                match at.discard(&p) {
                    Ok(()) => {
                        out.push(format!(
                            "deleted {}, a snapshot an earlier move couldn't",
                            p.display()
                        ));
                        if let Some(dir) = p.parent() {
                            let _ = std::fs::remove_dir(dir);
                            if let Some(parent) = dir.parent() {
                                let _ = std::fs::remove_dir(parent);
                            }
                        }
                    }
                    Err(_) => keep.push(p),
                }
            }
        }
    }
    s.set_leftovers(&guard, &keep).map_err(snapshots::err)?;
    Ok(out)
}

/// What a recorded leftover is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leftover {
    /// On disk: to delete.
    There,
    /// Its drive isn't there: kept, and reported.
    Unreachable,
    /// Gone, not shaped like one of the game's snapshots, or where the game's
    /// own snapshots live now (it moved back beside it, and a live snapshot
    /// may have its number): dropped from the list, never deleted.
    Forget,
}

fn leftover(gogid: &str, p: &Path, game_root: &Path) -> Leftover {
    if !is_leftover_shape(gogid, p) || p.parent() == backend::snapshots_dir(game_root).as_deref() {
        return Leftover::Forget;
    }
    if p.symlink_metadata().is_ok() {
        return Leftover::There;
    }
    // `<old parent>/.gogo-snapshots/<gogid>/<n>`: the old parent there means
    // the snapshot is gone; not there, a drive that isn't mounted.
    match p.ancestors().nth(3) {
        Some(parent) if !parent.is_dir() => Leftover::Unreachable,
        _ => Leftover::Forget,
    }
}

/// `…/.gogo-snapshots/<gogid>/<n>`.
fn is_leftover_shape(gogid: &str, p: &Path) -> bool {
    let mut parts = p.components().rev();
    let n = parts.next().map(|c| c.as_os_str().to_owned());
    let id = parts.next().map(|c| c.as_os_str().to_owned());
    let dir = parts.next().map(|c| c.as_os_str().to_owned());
    p.is_absolute()
        && n.is_some_and(|n| backend::parse_n(&n).is_some())
        && id.is_some_and(|i| i == gogid)
        && dir.is_some_and(|d| d == backend::SNAPSHOTS_DIR)
}

/// The leftovers `doctor` reports, with whether each is reachable: on disk,
/// or on a drive that isn't there.
pub(crate) fn leftovers(gogid: &str, root: &Path, backends: &Backends) -> Vec<(PathBuf, bool)> {
    let (_, b) = backends.for_dir(root);
    snapshots::store(gogid, root, b)
        .leftovers()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| match leftover(gogid, &p, root) {
            Leftover::There => Some((p, true)),
            Leftover::Unreachable => Some((p, false)),
            Leftover::Forget => None,
        })
        .collect()
}

/// The registry files in `prefix` that name `root`, as a unix path or through
/// `Z:` (case aside: Wine's drive letters don't keep it).
fn prefix_mentions(prefix: &Path, root: &Path) -> Vec<&'static str> {
    let unix = root.to_string_lossy().to_ascii_lowercase();
    let dos = format!("z:{}", unix.replace('/', "\\\\"));
    ["system.reg", "user.reg"]
        .into_iter()
        .filter(|file| {
            std::fs::read(prefix.join(file)).is_ok_and(|bytes| {
                let text = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
                text.contains(&unix) || text.contains(&dos)
            })
        })
        .collect()
}

/// A DLC add in progress or left over (design 19 §3.2, §3.4) holds files in the game
/// dir a move must not split: its journal, its download record, its staging dir.
fn refuse_dlc_add(gogid: &str) -> Result<(), MacAppError> {
    let root = GameDirs::for_game(gogid)?.root;
    let what = [
        crate::gog::lay::JOURNAL,
        super::dlc::DOWNLOAD,
        crate::gog::lay::STAGING,
    ]
    .into_iter()
    .find(|f| root.join(f).symlink_metadata().is_ok());
    match what {
        None => Ok(()),
        Some(f) => {
            let (what, cmd) = super::dlc::pending_kind(&root);
            Err(MacAppError::Apps(format!(
                "a DLC {what} of {gogid} is in progress or didn't finish ({} is there): run \
                 `gogo linux-app dlc {gogid} {cmd}` to finish it, then move the game",
                root.join(f).display()
            )))
        }
    }
}

/// What `gogo linux-app move` will do.
pub fn plan(gogid: &str, target: &Target, opts: &MoveOptions) -> Result<MovePlan, MacAppError> {
    plan_with(gogid, target, opts, &Backends::real())
}

/// [`plan`] with the snapshot backends given (tests).
pub fn plan_with(
    gogid: &str,
    target: &Target,
    opts: &MoveOptions,
    backends: &Backends,
) -> Result<MovePlan, MacAppError> {
    crate::macapp::compat::validate_gogid(gogid)?;
    refuse_dlc_add(gogid)?;
    relocate::plan(gogid, target, &lane(backends), opts)
}

fn lane(backends: &Backends) -> LinuxLane {
    LinuxLane {
        backends: backends.clone(),
    }
}

/// `gogo linux-app move`.
pub fn move_game(
    gogid: &str,
    target: &Target,
    opts: &MoveOptions,
    emit: &mut dyn FnMut(&MoveEvent),
) -> Result<MoveOutcome, MacAppError> {
    move_game_with(gogid, target, opts, &Backends::real(), emit)
}

/// [`move_game`] with the snapshot backends given (tests).
pub fn move_game_with(
    gogid: &str,
    target: &Target,
    opts: &MoveOptions,
    backends: &Backends,
    emit: &mut dyn FnMut(&MoveEvent),
) -> Result<MoveOutcome, MacAppError> {
    crate::macapp::compat::validate_gogid(gogid)?;
    // A snapshot rollback or conversion that stopped is settled first
    // (design 20 §5.3.1, §2.1), and so are snapshots an earlier move left.
    let settled = super::rollback::settle_pending(gogid, backends)?;
    refuse_dlc_add(gogid)?;
    let mut out = relocate::move_game(gogid, target, &lane(backends), opts, emit)?;
    out.recovered.splice(0..0, settled);
    Ok(out)
}

/// What `gogo linux-app move --adopt` will do.
pub fn adopt_plan(gogid: &str, target: &Target) -> Result<relocate::AdoptPlan, MacAppError> {
    relocate::adopt_plan(gogid, target, &LinuxLane::default())
}

/// `gogo linux-app move --adopt`: the game is at `target` now. Its snapshots
/// stay where they are (design 20 §9.1).
pub fn adopt_game(gogid: &str, target: &Target) -> Result<relocate::AdoptOutcome, MacAppError> {
    adopt_game_with(gogid, target, &Backends::real())
}

/// [`adopt_game`] with the snapshot backends given (tests).
pub fn adopt_game_with(
    gogid: &str,
    target: &Target,
    backends: &Backends,
) -> Result<relocate::AdoptOutcome, MacAppError> {
    crate::macapp::compat::validate_gogid(gogid)?;
    let settled = super::rollback::settle_pending(gogid, backends)?;
    let mut out = relocate::adopt_game(gogid, target, &lane(backends))?;
    out.recovered.splice(0..0, settled);
    Ok(out)
}

/// Finish whatever interrupted moves left, before something else looks at the
/// games.
pub fn recover_all() -> Vec<String> {
    relocate::recover_all(&LinuxLane::default())
}

/// Finish an interrupted move of `gogid` (tests: with the backends given).
pub fn recover_with(gogid: &str, backends: &Backends) -> Result<Vec<String>, MacAppError> {
    relocate::recover(gogid, &lane(backends))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_is_searched_for_the_old_path_in_both_forms() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        let root = Path::new("/home/Me/games/7");
        std::fs::write(p.join("system.reg"), r#""A"="C:\\GOG\\x""#).unwrap();
        std::fs::write(
            p.join("user.reg"),
            r#""A"="Z:\\HOME\\me\\games\\7\\prefix""#,
        )
        .unwrap();
        assert_eq!(prefix_mentions(p, root), ["user.reg"]);
        std::fs::write(p.join("system.reg"), "\"A\"=\"/home/Me/games/7/x\"").unwrap();
        assert_eq!(prefix_mentions(p, root), ["system.reg", "user.reg"]);
        assert!(prefix_mentions(&p.join("none"), root).is_empty());
    }
}
