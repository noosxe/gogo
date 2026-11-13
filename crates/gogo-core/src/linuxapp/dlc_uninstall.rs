//! DLC removal on the offline-installer lane (design 19 §5.2, #443): each DLC's
//! own Inno uninstaller, run in the game's Wine prefix.
//!
//! - GOG's DLC installers write an uninstaller of their own beside the base
//!   game's (`unins001.exe` and `.dat` next to the base's `unins000`), with a
//!   `unins001.ini` naming its product (`productID=<id>`). That ini, else the
//!   DLC's Inno log (`Creating new uninstall log: …`), says which one is the
//!   DLC's ([`find_uninstaller`]).
//! - Refused, with nothing written: a DLC with no uninstaller of its own (one
//!   that appended to the base's log: running it would uninstall the game), and
//!   a DLC whose install log shows it replaced files the game dir already had
//!   (Inno deletes them and doesn't put the old ones back).
//! - [`remove`] runs them under the game's lock, behind the `auto` snapshot and
//!   `dlc-journal.json` (`lane = "installer"`, `kind = "remove"`). A DLC is
//!   out when its uninstaller exits 0 and both its `goggame-<id>.info` and its
//!   `unins<n>.dat` are gone. A stop leaves the journal: the next `dlc remove`
//!   runs the uninstallers it names again, `dlc remove --abandon` gives it up,
//!   and a rollback to the snapshot taken before it undoes it.
//!
//! No network: the record (`content.toml`), the prefix and the logs are enough.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::engine::events::{ProgressEvent, step, working};
use crate::gog::content::{self, DlcRef};
use crate::gog::lay::{self, AddingDlc, Journal};
use crate::macapp::error::MacAppError;
use crate::macapp::installer;

use super::dlc::{AddError, Game};
use super::dlc_installer::{self as di, LANE};
use super::setup::Umu;

/// One DLC's uninstaller in the base game's dir.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Uninstaller {
    /// `unins<n>`'s number: the newest goes first.
    pub number: u32,
    /// The `.exe` on the host.
    pub exe: PathBuf,
    /// Its uninstall log (`.dat`), which Inno deletes when it is done.
    pub dat: PathBuf,
    /// The `.exe` as Wine runs it (`C:\GOG\<name>\unins001.exe`).
    pub win: String,
}

/// What [`find_uninstaller`] found for a DLC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// An uninstaller of its own.
    Own(Uninstaller),
    /// It appended to another product's uninstall log (the base game's), or the
    /// one it names is the base's slot (`unins000`) or another product's: its
    /// uninstaller would be that product's, `unins<n>.exe` named here.
    Shared(String),
    /// Several `unins<n>.ini` name it: gogo won't pick one.
    Ambiguous(Vec<String>),
    /// Its install log names `unins<n>.exe`, but nothing confirms it is the
    /// DLC's: no ini names the DLC, and none names the base game at another slot.
    Unconfirmed(String),
    /// None at all.
    None,
}

/// The `unins<n>` stem of a file name (`unins001.ini` → 1), any case.
fn unins_number(name: &str, ext: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    let n = lower.strip_prefix("unins")?.strip_suffix(ext)?;
    (n.len() == 3 && n.bytes().all(|b| b.is_ascii_digit()))
        .then(|| n.parse().ok())
        .flatten()
}

/// A file in `dir` named `name` in any case.
fn file_in(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))
        .map(|e| e.path())
        .filter(|p| p.is_file())
}

/// `productID=` of an `unins<n>.ini` (`[InstallSettings]`).
fn ini_product(path: &Path) -> Option<u64> {
    let text = String::from_utf8_lossy(&std::fs::read(path).ok()?).into_owned();
    text.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("productID")
            .then(|| v.trim().parse().ok())
            .flatten()
    })
}

fn uninstaller(install_dir: &Path, win_target: &str, number: u32) -> Option<Uninstaller> {
    let stem = format!("unins{number:03}");
    let exe = file_in(install_dir, &format!("{stem}.exe"))?;
    let dat = file_in(install_dir, &format!("{stem}.dat"))?;
    let exe_name = exe.file_name()?.to_string_lossy().into_owned();
    Some(Uninstaller {
        number,
        win: format!("{win_target}\\{exe_name}"),
        exe,
        dat,
    })
}

/// The uninstall log a DLC's Inno log names: `(appended, file name)`, from its
/// `Creating new uninstall log: <path>` or `Will append to existing uninstall
/// log: <path>` line.
fn log_uninstall(log: &str) -> Option<(bool, String)> {
    for l in log.lines() {
        for (marker, appended) in [
            ("Creating new uninstall log: ", false),
            ("Will append to existing uninstall log: ", true),
        ] {
            if let Some((_, path)) = l.split_once(marker) {
                let name = path.trim().rsplit(['\\', '/']).next()?.to_string();
                return Some((appended, name));
            }
        }
    }
    None
}

/// DLC `id`'s uninstaller in `install_dir` (`win_target` in Windows terms), the
/// base game being `base_id`: the one `unins<n>.ini` naming it, else the uninstall
/// log its Inno log (`inno_log`) names, when that is confirmed. Slot 0 is the base
/// game's and is never a DLC's: running it would uninstall the whole game. One
/// that appended to another product's log is [`Found::Shared`].
pub fn find_uninstaller(
    install_dir: &Path,
    win_target: &str,
    id: u64,
    base_id: u64,
    inno_log: Option<&str>,
) -> Found {
    let mut inis: Vec<(u32, Option<u64>)> = std::fs::read_dir(install_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = unins_number(&e.file_name().to_string_lossy(), ".ini")?;
            Some((n, ini_product(&e.path())))
        })
        .collect();
    inis.sort();
    let naming: Vec<u32> = inis
        .iter()
        .filter(|(_, p)| *p == Some(id))
        .map(|(n, _)| *n)
        .collect();
    let exe = |n: u32| format!("unins{n:03}.exe");
    match naming.as_slice() {
        [] => {}
        [0] => return Found::Shared(exe(0)),
        [n] => {
            if let Some(u) = uninstaller(install_dir, win_target, *n) {
                return Found::Own(u);
            }
        }
        many => return Found::Ambiguous(many.iter().map(|n| exe(*n)).collect()),
    }
    let Some((appended, dat)) = inno_log.and_then(log_uninstall) else {
        return Found::None;
    };
    let Some(n) = unins_number(&dat, ".dat") else {
        return Found::None;
    };
    if appended || n == 0 {
        return Found::Shared(exe(n));
    }
    match inis.iter().find(|(m, _)| *m == n).and_then(|(_, p)| *p) {
        // Named by its ini: the DLC's own (its files gone, else found above).
        Some(p) if p == id => {}
        Some(_) => return Found::Shared(exe(n)),
        // No ini, or one gogo can't read: only when the base game's own ini, at
        // another slot, says the slots are what they seem.
        None => {
            let base_elsewhere = inis.iter().any(|(m, p)| *m != n && *p == Some(base_id));
            if !base_elsewhere {
                return if uninstaller(install_dir, win_target, n).is_some() {
                    Found::Unconfirmed(exe(n))
                } else {
                    Found::None
                };
            }
        }
    }
    match uninstaller(install_dir, win_target, n) {
        Some(u) => Found::Own(u),
        None => Found::None,
    }
}

/// What a DLC's Inno log says it did in the game's dir (`win_target`), by path
/// relative to it: the files it installed, and those of them the dir already
/// had (it replaced them). Its own uninstaller and `goggame-<id>.*` aren't
/// counted as replaced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogFiles {
    pub installed: Vec<String>,
    pub replaced: Vec<String>,
    /// Some file entry is in the game's dir. A DLC's installer always puts its
    /// uninstaller there, so a log with none is about another dir, or written in
    /// a form gogo doesn't read: it says nothing about what was replaced.
    pub matched: bool,
}

/// `path` (a Windows path) relative to `win_target`, compared by component
/// without case (Unicode), in `/` form; `None` outside it.
fn under(path: &str, win_target: &str) -> Option<String> {
    let parts: Vec<&str> = path.split(['\\', '/']).collect();
    let base: Vec<&str> = win_target
        .split(['\\', '/'])
        .filter(|c| !c.is_empty())
        .collect();
    if parts.len() <= base.len()
        || !parts
            .iter()
            .zip(&base)
            .all(|(a, b)| a.to_lowercase() == b.to_lowercase())
    {
        return None;
    }
    let rel: Vec<&str> = parts[base.len()..]
        .iter()
        .copied()
        .filter(|c| !c.is_empty())
        .collect();
    (!rel.is_empty()).then(|| rel.join("/"))
}

/// Read [`LogFiles`] from an Inno log's `-- File entry --` blocks: `Dest
/// filename:`, then `Dest file exists.` when it was there, then `Installing the
/// file.` or a `Skipping` line.
pub fn log_files(log: &str, win_target: &str, id: u64) -> LogFiles {
    let own = format!("goggame-{id}.");
    let mut out = LogFiles::default();
    let mut dest: Option<String> = None;
    let mut existed = false;
    for l in log.lines() {
        if l.contains("-- File entry --") || l.contains("-- Registry entry --") {
            dest = None;
            existed = false;
        } else if let Some((_, d)) = l.split_once("Dest filename: ") {
            dest = under(d.trim(), win_target);
            out.matched |= dest.is_some();
            existed = false;
        } else if l.contains("Dest file exists.") {
            existed = true;
        } else if l.contains("Installing the file.")
            && let Some(rel) = dest.take()
        {
            let lower = rel.to_lowercase();
            let mine = lower.starts_with(&own) || unins_number(&lower, ".exe").is_some();
            if existed && !mine {
                out.replaced.push(rel.clone());
            }
            out.installed.push(rel);
        }
    }
    out
}

/// A DLC's install log (`log`, its contents) is the one of the install recorded
/// now: not older than the DLC's info in the game's dir (`info`), which that
/// install wrote before it closed its log. A log that is older belongs to an
/// earlier install. Two minutes of slack, for a copy that didn't keep times.
fn log_is_current(log: &Path, info: Option<&Path>) -> bool {
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (mtime(log), info.and_then(mtime)) {
        (Some(l), Some(i)) => l + std::time::Duration::from_secs(120) >= i,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// One DLC of a removal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveDlc {
    pub id: u64,
    pub title: String,
    /// Its uninstaller, to run; `None` when its info and uninstaller are both
    /// gone already (only its record goes).
    pub uninstaller: Option<Uninstaller>,
    /// An earlier removal of it stopped: its uninstaller runs again.
    pub resumed: bool,
    /// No install log gogo can trust (none, an older install's, or one about
    /// another dir): gogo can't tell what it replaced.
    pub log_missing: bool,
    /// Game-dir files its Inno log says it installed (paths relative to the
    /// game's dir): the ones still there after the uninstaller are reported.
    #[serde(skip)]
    pub installed: Vec<String>,
}

/// What a removal on this lane does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovePlan {
    pub lane: &'static str,
    pub gogid: String,
    /// The game's dir, `C:\GOG\<name>`.
    pub into: String,
    /// In the order they run: the newest uninstaller first.
    pub removing: Vec<RemoveDlc>,
    /// The `auto` snapshot will be taken (design 20 §7.1).
    pub autosnapshot: bool,
}

impl RemovePlan {
    pub fn refs(&self) -> Vec<DlcRef> {
        self.removing
            .iter()
            .map(|d| DlcRef {
                id: d.id,
                title: d.title.clone(),
            })
            .collect()
    }
}

/// The rebuild that leaves `wanted` out and keeps the prefix and saves.
pub fn rebuild_hint(game: &Game, wanted: &[u64]) -> String {
    let keep: Vec<String> = game
        .installed()
        .into_iter()
        .filter(|i| !wanted.contains(i))
        .map(|i| i.to_string())
        .collect();
    if keep.is_empty() {
        format!("gogo linux-app {} --no-dlcs", game.gogid)
    } else {
        format!(
            "gogo linux-app {} --dlc {}",
            game.gogid,
            keep.join(" --dlc ")
        )
    }
}

/// Work out the removal of `wanted` (installed DLC, or ones a stopped removal
/// names: `resumed`). Refuses a DLC whose uninstaller can't be found while its
/// info is there, one sharing another product's uninstaller, and one that
/// replaced game files.
pub fn plan(
    game: &Game,
    wanted: &[DlcRef],
    resumed: &[u64],
    autosnapshot: bool,
) -> Result<RemovePlan, MacAppError> {
    let (win_target, install_dir) = di::install_dir(game)?;
    let logs = game.dirs.root.join(crate::macapp::INSTALL_LOGS);
    let base_id: u64 = game.gogid.parse().unwrap_or(0);
    let ids: Vec<u64> = wanted.iter().map(|d| d.id).collect();
    let rebuild = || {
        format!(
            "a rebuild without it keeps the prefix and saves: {}",
            rebuild_hint(game, &ids)
        )
    };
    let mut removing = Vec::new();
    for d in wanted {
        let log_path = logs.join(di::inno_log_name(d.id));
        let info_path = di::info_in(&install_dir, d.id);
        let info = info_path.is_some();
        // The install log of the install recorded now, about the game's dir;
        // anything else counts as no log.
        let (log, files) = match std::fs::read(&log_path) {
            Ok(b) if log_is_current(&log_path, info_path.as_deref()) => {
                let text = String::from_utf8_lossy(&b).into_owned();
                let files = log_files(&text, &win_target, d.id);
                if files.matched {
                    (Some(text), files)
                } else {
                    (None, LogFiles::default())
                }
            }
            _ => (None, LogFiles::default()),
        };
        let found = find_uninstaller(&install_dir, &win_target, d.id, base_id, log.as_deref());
        let uninstaller = match found {
            Found::Own(u) => Some(u),
            Found::Shared(exe) => {
                return Err(MacAppError::Apps(format!(
                    "DLC \"{}\" ({}) has no uninstaller of its own: its uninstaller would be \
                     {exe}, which uninstalls the whole game; {}",
                    d.title,
                    d.id,
                    rebuild()
                )));
            }
            Found::Ambiguous(exes) => {
                return Err(MacAppError::Apps(format!(
                    "DLC \"{}\" ({}) is named by several uninstallers ({}), and gogo won't \
                     pick one; {}",
                    d.title,
                    d.id,
                    exes.join(", "),
                    rebuild()
                )));
            }
            Found::Unconfirmed(exe) => {
                return Err(MacAppError::Apps(format!(
                    "DLC \"{}\" ({})'s install log names {exe}, but nothing confirms it is \
                     the DLC's (no unins<n>.ini names the DLC, and none names the base game \
                     at another slot); {}",
                    d.title,
                    d.id,
                    rebuild()
                )));
            }
            Found::None if !info => None,
            Found::None => {
                return Err(MacAppError::Apps(format!(
                    "DLC \"{}\" ({}) has no uninstaller in {} (no unins<n>.ini names it, \
                     and its install log names none); {}",
                    d.title,
                    d.id,
                    install_dir.display(),
                    rebuild()
                )));
            }
        };
        if uninstaller.is_some() && !files.replaced.is_empty() {
            return Err(MacAppError::Apps(format!(
                "DLC \"{}\" ({}) replaced files the game already had ({}), and its \
                 uninstaller would delete them, not put the game's back; {}",
                d.title,
                d.id,
                files.replaced.join(", "),
                rebuild()
            )));
        }
        removing.push(RemoveDlc {
            id: d.id,
            title: d.title.clone(),
            uninstaller,
            resumed: resumed.contains(&d.id),
            log_missing: log.is_none(),
            installed: files.installed,
        });
    }
    // Newest first: a later DLC may sit on an earlier one (GOG's own base
    // uninstaller takes its DLC out bottom to top).
    removing
        .sort_by_key(|d| std::cmp::Reverse(d.uninstaller.as_ref().map_or(u32::MAX, |u| u.number)));
    Ok(RemovePlan {
        lane: LANE,
        gogid: game.gogid.clone(),
        into: win_target,
        removing,
        autosnapshot,
    })
}

/// The plan's lines (no `gogo:` prefix), the saves warning last.
pub fn plan_lines(p: &RemovePlan) -> Vec<String> {
    let mut out = Vec::new();
    for d in &p.removing {
        let again = if d.resumed {
            ", an earlier removal of it stopped: it runs again"
        } else {
            ""
        };
        match &d.uninstaller {
            Some(u) => out.push(format!(
                "dlc: {} ({}): its uninstaller {} runs in the game's prefix{again}",
                d.title, d.id, u.win
            )),
            None => out.push(format!(
                "dlc: {} ({}): its files and uninstaller are gone already; only its record \
                 goes",
                d.title, d.id
            )),
        }
        if d.log_missing && d.uninstaller.is_some() {
            out.push(format!(
                "note: gogo has no install log of {} it can trust (none, one older than the \
                 DLC's install, or one about another dir), so it can't tell whether the DLC \
                 replaced game files (its uninstaller would delete them)",
                d.title
            ));
        }
    }
    out.push(super::dlc_remove::saves_line(
        &p.gogid,
        &p.refs(),
        p.autosnapshot,
    ));
    out
}

/// The ways out of an unfinished removal on this lane, for every message about one.
pub fn ways_out(gogid: &str) -> String {
    format!(
        "`gogo linux-app dlc {gogid} remove` runs the DLC's uninstallers again; `gogo \
         linux-app dlc {gogid} remove --abandon` gives the removal up (a DLC whose files \
         are gone is no longer recorded); or roll back to the snapshot taken before the \
         removal (`gogo linux-app snapshot {gogid} list`, then `… rollback <n>`), which \
         undoes it"
    )
}

/// The DLC an unfinished removal of `root`'s game names: its journal's, when it
/// is this lane's removal.
pub fn pending(root: &Path) -> Vec<AddingDlc> {
    lay::read_journal(root)
        .ok()
        .flatten()
        .filter(|j| j.lane == LANE && j.is_removal())
        .map(|j| j.removing)
        .unwrap_or_default()
}

/// The Inno log of DLC `id`'s uninstaller, beside its install log.
pub fn uninstall_log_name(id: u64) -> String {
    format!("dlc-{id}-uninstall.log")
}

/// The Inno uninstaller's flags: silent, no restart, its log through `z:`.
pub fn uninstall_args(win_exe: &str, log: &Path) -> Vec<String> {
    vec![
        win_exe.to_string(),
        "/VERYSILENT".into(),
        "/SUPPRESSMSGBOXES".into(),
        "/NORESTART".into(),
        format!("/LOG={}", installer::z_path(log)),
    ]
}

/// How to run a removal.
#[derive(Debug, Clone)]
pub struct RemoveOptions {
    pub snapshots: super::snapshots::AutoOptions,
    /// Run setup after the commit. Tests turn it off.
    pub run_setup: bool,
    /// Crash injection: stop after this DLC's uninstaller, before the commit.
    #[doc(hidden)]
    pub stop_after: Option<u64>,
}

impl Default for RemoveOptions {
    fn default() -> Self {
        Self {
            snapshots: super::snapshots::AutoOptions::default(),
            run_setup: true,
            stop_after: None,
        }
    }
}

/// What a removal did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveOutcome {
    pub gogid: String,
    pub lane: &'static str,
    /// Every DLC the game has now.
    pub dlcs: Vec<DlcRef>,
    pub removed: Vec<DlcRef>,
    /// An earlier removal that stopped was finished.
    pub resumed: bool,
    /// Files the DLC's installer put in the game's dir that its uninstaller
    /// left (paths relative to it). gogo doesn't delete them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub left: Vec<String>,
    pub snapshot: Option<super::snapshots::SnapshotRef>,
    pub pruned: Vec<super::snapshots::SnapshotRef>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

fn journal(game: &Game, removing: &[AddingDlc], source: &Path) -> Journal {
    Journal {
        gogid: game.gogid.clone(),
        lane: LANE.into(),
        build_id: game.content.build_id.clone().unwrap_or_default(),
        language: game.content.language.clone().unwrap_or_default(),
        adding: Vec::new(),
        source: source.to_path_buf(),
        source_is_cache: false,
        moves: false,
        files: Vec::new(),
        replaced: Vec::new(),
        dirs: Vec::new(),
        kind: Some(lay::KIND_REMOVE.into()),
        from: None,
        to: None,
        removed: Vec::new(),
        removing: removing.to_vec(),
        md5s: Default::default(),
        prune: Vec::new(),
    }
}

fn interrupted(gogid: &str) -> AddError {
    AddError::Interrupted(
        "DLC uninstallers",
        format!("the game won't start until then: {}", ways_out(gogid)),
    )
}

/// Run one DLC's uninstaller through `umu`, its log in `logs`; check it took
/// the DLC out of `install_dir`.
fn run_uninstaller(
    umu: &Umu,
    d: &RemoveDlc,
    u: &Uninstaller,
    install_dir: &Path,
    logs: &Path,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<(), MacAppError> {
    let log_name = uninstall_log_name(d.id);
    let log = logs.join(&log_name);
    let mut cmd = umu.command(uninstall_args(&u.win, &log));
    // The dir the runtime's container shares for the exe (a `C:` path is no
    // file to umu); it is in the prefix, which it shares anyway.
    cmd.env(di::INSTALL_PATH, install_dir);
    let status = working(emit, step::DLC_UNINSTALLER, || {
        umu.run("a DLC's uninstaller", cmd)
    })?;
    let tail = || {
        let t = installer::log_tail(&log, 8);
        if t.is_empty() {
            String::new()
        } else {
            format!("; the end of its log ({}):\n{t}", log.display())
        }
    };
    if !status.success() {
        return Err(MacAppError::Apps(format!(
            "the uninstaller of DLC \"{}\" ({}) exited {}{}",
            d.title,
            d.id,
            status.code().unwrap_or(-1),
            tail()
        )));
    }
    let info = di::info_in(install_dir, d.id);
    if info.is_some() || u.dat.exists() {
        return Err(MacAppError::Apps(format!(
            "the uninstaller of DLC \"{}\" ({}) exited 0 but left {}{}",
            d.title,
            d.id,
            info.as_deref().unwrap_or(&u.dat).display(),
            tail()
        )));
    }
    Ok(())
}

/// What gives a removal its umu: [`super::setup::game_runner`] in production.
pub type Runner<'a> = di::Runner<'a>;

/// Take `wanted` out of `game` (design 19 §5.2): each DLC's own uninstaller in
/// the game's prefix. `wanted` is the CLI's plan: installed DLC by name, and any
/// a stopped removal names. `stop` says the user asked to stop.
pub fn remove(
    game: &mut Game,
    wanted: &[DlcRef],
    runner: &mut Runner,
    opts: &RemoveOptions,
    emit: &mut dyn FnMut(&ProgressEvent),
    stop: &dyn Fn() -> bool,
) -> Result<RemoveOutcome, AddError> {
    let root = game.dirs.root.clone();
    for message in
        super::rollback::settle_pending(&game.gogid, &super::snapshots::Backends::real())?
    {
        emit(&ProgressEvent::Warn { message });
    }
    let lock = crate::macapp::relocate::lock_game(&game.gogid)?;
    super::game::refuse_running(&game.gogid, &root, &game.dirs.prefix)?;
    *game = game.reload()?;
    let prior = lay::read_journal(&root)?;
    match &prior {
        Some(j) if j.lane != LANE => {
            return Err(AddError::Game(MacAppError::Apps(format!(
                "{} has a depot-lane journal ({}) on an offline-installer game; it can't be \
                 settled here",
                game.gogid,
                lay::journal_path(&root).display()
            ))));
        }
        Some(j) if !j.is_removal() => {
            return Err(AddError::Game(MacAppError::Apps(format!(
                "an earlier DLC add of {} stopped while its installers ran; settle it \
                 before a removal: {}",
                game.gogid,
                di::ways_out(&game.gogid)
            ))));
        }
        _ => {}
    }
    let resumed = prior.is_some();
    let installed = game.installed();
    let mut removing: Vec<AddingDlc> = prior.map(|j| j.removing).unwrap_or_default();
    let resumed_ids: Vec<u64> = removing.iter().map(|a| a.id).collect();
    for d in wanted {
        // One the game no longer has (another removal got there first) is done.
        if installed.contains(&d.id) && !removing.iter().any(|a| a.id == d.id) {
            removing.push(AddingDlc {
                id: d.id,
                title: d.title.clone(),
            });
        }
    }
    let mut warnings = Vec::new();
    if removing.is_empty() {
        // A journal naming nothing (written by hand, or by an older gogo) is a
        // finished removal.
        if resumed {
            lay::finish(&root)?;
        }
        return Ok(outcome(game, &[], resumed, Vec::new(), None, warnings));
    }
    let refs: Vec<DlcRef> = removing
        .iter()
        .map(|a| DlcRef {
            id: a.id,
            title: a.title.clone(),
        })
        .collect();
    // The plan again, under the lock: the prefix as it is now.
    let plan = plan(game, &refs, &resumed_ids, false)?;
    let (_, install_dir) = di::install_dir(game)?;
    let logs = root.join(crate::macapp::INSTALL_LOGS);
    let runs = plan.removing.iter().any(|d| d.uninstaller.is_some());
    // The runtime before the journal: a Proton build or umu that can't be had
    // leaves nothing to settle.
    let r = if runs { Some(runner(emit)?) } else { None };
    let umu = r.as_ref().map(|r| Umu {
        run: &r.umu_run,
        proton: &r.build.dir,
        prefix: &game.dirs.prefix,
        umu_id: &r.umu_id,
        cwd: &root,
        log: logs.join(installer::WINE_LOG),
    });
    if let Some(u) = &umu {
        u.check()?;
    }
    // The `auto` snapshot (design 20 §7.1): after the refusals, before the
    // journal. A rerun after a stop took it on its first try.
    let mut taken = None;
    if !resumed {
        let titles: Vec<&str> = removing.iter().map(|a| a.title.as_str()).collect();
        taken = super::snapshots::protect(
            &game.gogid,
            &game.dirs,
            Some(&lock),
            crate::snapshot::Trigger::DlcRemove,
            crate::snapshot::labels::removing(&titles.join(", ")),
            &opts.snapshots,
            emit,
        )?;
    }
    lay::write_json_durably(
        &lay::journal_path(&root),
        &journal(game, &removing, &install_dir),
    )?;
    if resumed {
        emit(&ProgressEvent::Warn {
            message: format!(
                "an earlier DLC removal stopped while its uninstallers ran: running {} again",
                removing
                    .iter()
                    .map(|a| a.title.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }
    std::fs::create_dir_all(&logs)?;
    for d in &plan.removing {
        if stop() {
            return Err(interrupted(&game.gogid));
        }
        let (Some(u), Some(umu)) = (&d.uninstaller, &umu) else {
            continue;
        };
        match run_uninstaller(umu, d, u, &install_dir, &logs, emit) {
            Ok(()) => {}
            Err(_) if stop() => return Err(interrupted(&game.gogid)),
            Err(e) => {
                return Err(AddError::Game(MacAppError::Apps(format!(
                    "{e}\n{}",
                    ways_out(&game.gogid)
                ))));
            }
        }
        if opts.stop_after == Some(d.id) {
            return Err(interrupted(&game.gogid));
        }
    }
    // What the uninstallers left of what the installers put there.
    let mut left = Vec::new();
    for d in &plan.removing {
        for rel in &d.installed {
            if super::dlc_remove::find_ci(&install_dir, rel).is_some_and(|p| p.exists())
                && !left.contains(rel)
            {
                left.push(rel.clone());
            }
        }
    }

    // The commit: `content.toml` without the DLC, then the journal goes.
    let mut c = game.content.clone();
    c.dlc.retain(|row| !removing.iter().any(|r| r.id == row.id));
    content::write(&root, &c)?;
    game.content = c;
    game.derived = false;
    lay::finish(&root)?;

    if opts.run_setup
        && let Err(e) = super::setup::setup(
            &game.gogid,
            &super::setup::SetupOptions {
                refresh: false,
                prompt: None,
                runtime_snapshot: None,
            },
            emit,
        )
    {
        warnings.push(format!(
            "the DLC is removed, but setup failed: {e}; run `gogo linux-app setup {}`",
            game.gogid
        ));
    }
    Ok(outcome(game, &removing, resumed, left, taken, warnings))
}

fn outcome(
    game: &Game,
    removing: &[AddingDlc],
    resumed: bool,
    left: Vec<String>,
    taken: Option<super::snapshots::Taken>,
    warnings: Vec<String>,
) -> RemoveOutcome {
    RemoveOutcome {
        gogid: game.gogid.clone(),
        lane: LANE,
        dlcs: game.content.refs(),
        removed: removing
            .iter()
            .map(|a| DlcRef {
                id: a.id,
                title: a.title.clone(),
            })
            .collect(),
        resumed,
        left,
        snapshot: taken.as_ref().map(super::snapshots::Taken::snapshot),
        pruned: taken
            .as_ref()
            .map(super::snapshots::Taken::pruned)
            .unwrap_or_default(),
        warnings,
    }
}

/// What [`abandon`] gave up.
#[derive(Debug, Clone, Serialize)]
pub struct Abandoned {
    pub gogid: String,
    /// The DLC whose info is gone: no longer recorded.
    pub dropped: Vec<DlcRef>,
    /// The DLC whose info is still in the game's dir: still recorded.
    pub kept: Vec<DlcRef>,
}

/// `dlc remove --abandon`: drop an unfinished removal on this lane. Under the
/// game's lock, refused while it runs: the journal goes, and the record follows
/// the prefix (a DLC whose `goggame-<id>.info` is gone is no longer recorded;
/// one whose info is there stays). With no such journal there's nothing to do;
/// a depot-lane journal or an add's is refused.
pub fn abandon(game: &mut Game) -> Result<Abandoned, MacAppError> {
    let root = game.dirs.root.clone();
    let _lock = crate::macapp::relocate::lock_game(&game.gogid)?;
    super::game::refuse_running(&game.gogid, &root, &game.dirs.prefix)?;
    *game = game.reload()?;
    let Some(j) = lay::read_journal(&root)? else {
        return Ok(Abandoned {
            gogid: game.gogid.clone(),
            dropped: Vec::new(),
            kept: Vec::new(),
        });
    };
    if j.lane != LANE {
        return Err(MacAppError::Apps(format!(
            "{}'s unfinished DLC change is the depot lane's: `gogo linux-app dlc {} {}` \
             finishes it; --abandon is for offline-installer games",
            game.gogid,
            game.gogid,
            super::dlc::pending_kind(&root).1
        )));
    }
    if !j.is_removal() {
        return Err(MacAppError::Apps(format!(
            "{}'s unfinished DLC change is an add: `gogo linux-app dlc {} add --abandon` \
             gives it up",
            game.gogid, game.gogid
        )));
    }
    let (_, install_dir) = di::install_dir(game)?;
    let (gone, there): (Vec<_>, Vec<_>) = j
        .removing
        .iter()
        .map(|a| DlcRef {
            id: a.id,
            title: a.title.clone(),
        })
        .partition(|d| di::info_in(&install_dir, d.id).is_none());
    let mut c = game.content.clone();
    c.dlc.retain(|row| !gone.iter().any(|g| g.id == row.id));
    content::write(&root, &c)?;
    lay::finish(&root)?;
    *game = game.reload()?;
    Ok(Abandoned {
        gogid: game.gogid.clone(),
        dropped: gone,
        kept: there,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN: &str = "C:\\GOG\\journey";

    fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for (name, body) in files {
            std::fs::write(d.path().join(name), body).unwrap();
        }
        d
    }

    const BASE: u64 = 1519147341;

    #[test]
    fn the_ini_naming_the_dlc_finds_its_uninstaller() {
        let d = dir_with(&[
            ("unins000.exe", "MZ"),
            ("unins000.dat", ""),
            (
                "unins000.ini",
                "[InstallSettings]\r\nproductID=1519147341\r\n",
            ),
            ("UNINS001.exe", "MZ"),
            ("unins001.dat", ""),
            ("unins001.ini", "[InstallSettings]\nproductID=1240193021\n"),
        ]);
        match find_uninstaller(d.path(), WIN, 1240193021, BASE, None) {
            Found::Own(u) => {
                assert_eq!(u.number, 1);
                assert_eq!(u.win, "C:\\GOG\\journey\\UNINS001.exe");
                assert!(u.dat.ends_with("unins001.dat"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(find_uninstaller(d.path(), WIN, 7, BASE, None), Found::None);
    }

    #[test]
    fn slot_0_is_never_a_dlcs() {
        // An ini at slot 0 naming the DLC: still the base's slot.
        let d = dir_with(&[
            ("unins000.exe", "MZ"),
            ("unins000.dat", ""),
            ("unins000.ini", "productID=5\n"),
        ]);
        assert_eq!(
            find_uninstaller(d.path(), WIN, 5, BASE, None),
            Found::Shared("unins000.exe".into())
        );
        // A log naming slot 0, with the base's ini missing or unreadable.
        let log = "t   Creating new uninstall log: C:\\GOG\\journey\\unins000.dat";
        let d = dir_with(&[("unins000.exe", "MZ"), ("unins000.dat", "")]);
        assert_eq!(
            find_uninstaller(d.path(), WIN, 5, BASE, Some(log)),
            Found::Shared("unins000.exe".into())
        );
        let d = dir_with(&[
            ("unins000.exe", "MZ"),
            ("unins000.dat", ""),
            ("unins000.ini", "garbage"),
        ]);
        assert_eq!(
            find_uninstaller(d.path(), WIN, 5, BASE, Some(log)),
            Found::Shared("unins000.exe".into())
        );
    }

    #[test]
    fn two_inis_naming_the_dlc_are_refused() {
        let d = dir_with(&[
            ("unins001.exe", "MZ"),
            ("unins001.dat", ""),
            ("unins001.ini", "productID=5\n"),
            ("unins002.exe", "MZ"),
            ("unins002.dat", ""),
            ("unins002.ini", "productID=5\n"),
        ]);
        assert_eq!(
            find_uninstaller(d.path(), WIN, 5, BASE, None),
            Found::Ambiguous(vec!["unins001.exe".into(), "unins002.exe".into()])
        );
    }

    #[test]
    fn the_inno_log_finds_it_only_when_confirmed() {
        let own = "2026-11-13 18:49:39.133   Creating new uninstall log: \
                   C:\\GOG\\journey\\unins001.dat\n";
        // No ini at slot 1, the base's ini at slot 0: confirmed.
        let d = dir_with(&[
            ("unins000.exe", "MZ"),
            ("unins000.dat", ""),
            ("unins000.ini", "[InstallSettings]\nproductID=1519147341\n"),
            ("unins001.exe", "MZ"),
            ("unins001.dat", ""),
        ]);
        assert!(matches!(
            find_uninstaller(d.path(), WIN, 2, BASE, Some(own)),
            Found::Own(Uninstaller { number: 1, .. })
        ));
        // No ini names the base anywhere else: unconfirmed.
        let d = dir_with(&[
            ("unins000.exe", "MZ"),
            ("unins000.dat", ""),
            ("unins001.exe", "MZ"),
            ("unins001.dat", ""),
            ("unins001.ini", "unreadable"),
        ]);
        assert_eq!(
            find_uninstaller(d.path(), WIN, 2, BASE, Some(own)),
            Found::Unconfirmed("unins001.exe".into())
        );
        // The base's ini at the log's own slot doesn't confirm it either.
        let d = dir_with(&[
            ("unins001.exe", "MZ"),
            ("unins001.dat", ""),
            ("unins001.ini", "productID=1519147341\n"),
        ]);
        assert_eq!(
            find_uninstaller(d.path(), WIN, 2, BASE, Some(own)),
            Found::Shared("unins001.exe".into())
        );
        let appended = "x   Will append to existing uninstall log: C:\\GOG\\journey\\unins003.dat";
        assert_eq!(
            find_uninstaller(d.path(), WIN, 2, BASE, Some(appended)),
            Found::Shared("unins003.exe".into())
        );
    }

    #[test]
    fn an_uninstaller_with_its_dat_gone_isnt_one() {
        let d = dir_with(&[("unins001.exe", "MZ"), ("unins001.ini", "productID=5\n")]);
        assert_eq!(find_uninstaller(d.path(), WIN, 5, BASE, None), Found::None);
    }

    #[test]
    fn a_log_about_another_dir_matches_nothing_and_case_is_unicode() {
        let log =
            "t   -- File entry --\nt   Dest filename: D:\\Other\\x.pak\nt   Installing the file.\n";
        let f = log_files(log, WIN, 9);
        assert!(!f.matched && f.installed.is_empty());
        let log = "t   -- File entry --\nt   Dest filename: C:\\GOG\\ÉPOPÉE\\Data\\x.pak\n\
                   t   Dest file exists.\nt   Installing the file.\n";
        let f = log_files(log, "C:\\GOG\\épopée", 9);
        assert!(f.matched);
        assert_eq!(f.replaced, vec!["Data/x.pak"]);
    }

    #[test]
    fn an_install_log_older_than_the_info_isnt_trusted() {
        let d = dir_with(&[("log", "x"), ("info", "{}")]);
        let (log, info) = (d.path().join("log"), d.path().join("info"));
        let at = |p: &Path, secs: u64| {
            let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
            std::fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(t)
                .unwrap();
        };
        at(&log, 1_000_000);
        at(&info, 1_000_060);
        assert!(log_is_current(&log, Some(&info)), "within the slack");
        at(&info, 1_000_000 + 3600);
        assert!(!log_is_current(&log, Some(&info)), "an earlier install's");
        assert!(
            log_is_current(&log, None),
            "the info is gone (a resumed removal)"
        );
        assert!(!log_is_current(&d.path().join("none"), None));
    }

    #[test]
    fn the_log_tells_installed_from_replaced() {
        let log = "\
t   -- File entry --
t   Dest filename: C:\\GOG\\journey\\unins001.exe
t   Installing the file.
t   -- File entry --
t   Dest filename: C:\\users\\steamuser\\AppData\\Local\\Temp\\is-1.tmp\\2c
t   Installing the file.
t   -- File entry --
t   Dest filename: C:\\GOG\\journey\\goggame-9.ico
t   Dest file exists.
t   Installing the file.
t   -- File entry --
t   Dest filename: C:\\GOG\\journey\\webcache.zip
t   Dest file exists.
t   Skipping due to \"onlyifdoesntexist\" flag.
t   -- File entry --
t   Dest filename: C:\\GOG\\Journey\\Data\\base.pak
t   Dest file exists.
t   Time stamp of existing file: 2019-08-26 17:19:10.000
t   Installing the file.
t   -- File entry --
t   Dest filename: C:\\GOG\\journey\\Data\\dlc.pak
t   Installing the file.
";
        let f = log_files(log, WIN, 9);
        assert!(f.matched);
        assert_eq!(
            f.installed,
            vec![
                "unins001.exe",
                "goggame-9.ico",
                "Data/base.pak",
                "Data/dlc.pak"
            ]
        );
        assert_eq!(f.replaced, vec!["Data/base.pak"]);
    }

    #[test]
    fn uninstall_args_are_silent_with_a_log() {
        let a = uninstall_args("C:\\GOG\\x\\unins001.exe", Path::new("/g/install/u.log"));
        assert_eq!(
            a,
            vec![
                "C:\\GOG\\x\\unins001.exe",
                "/VERYSILENT",
                "/SUPPRESSMSGBOXES",
                "/NORESTART",
                "/LOG=Z:\\g\\install\\u.log",
            ]
        );
    }
}
