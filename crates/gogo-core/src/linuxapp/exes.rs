//! A built game's executables, and one-off runs of them (design 18 §7.10):
//! `gogo linux-app exes <gogid>` lists the play tasks of every installed
//! `goggame-*.info` (base game and DLC), and with `--all` every other `.exe`
//! in the game's files; `gogo linux-app run <gogid> <task|path>` starts one
//! through the game's own `launch.sh` ([`launcher::ONE_OFF_ARG`]), so it gets
//! everything a launch of the game gets, and nothing of the game's config is
//! written.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use super::game::{self, ConfValues, LAUNCH_SH};
use super::{dlc, launcher, setup};
use crate::macapp::error::MacAppError;
use crate::macapp::info::{self, GogTask};
use crate::macapp::launch::GameDirs;
use crate::macapp::pe::{self, Bitness};

/// One executable of the game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Exe {
    /// The play task's number (from 1, base game first, in info order);
    /// `None`: not a play task (`--all`).
    pub index: Option<usize>,
    /// The play task's `name`.
    pub name: Option<String>,
    /// Relative to the game's files, `/`-separated.
    pub path: String,
    /// The play task's `arguments`, as argv.
    pub args: Vec<String>,
    /// The play task's `workingDir`, relative to the game's files.
    pub working_dir: Option<String>,
    /// `game`, `launcher`, `tool`, … (GOG's).
    pub category: Option<String>,
    /// GOG's `isPrimary`.
    pub primary: bool,
    /// The `goggame-<id>.info` the task is in; `None` for an exe that isn't a
    /// play task.
    pub game_id: Option<String>,
    /// The task is a DLC's (its info isn't the base game's).
    pub dlc: bool,
    /// gogo's launcher runs this exe (what the menu entry starts).
    pub launcher: bool,
    /// 32 or 64, from the PE header; `None` when it isn't a PE file.
    pub bits: Option<u8>,
    /// The file is there (a play task can name one that isn't).
    pub exists: bool,
    #[serde(skip)]
    abs: PathBuf,
    #[serde(skip)]
    workdir_abs: PathBuf,
}

/// What `linux-app exes` prints.
#[derive(Debug, Clone, Serialize)]
pub struct Listing {
    pub gogid: String,
    pub title: String,
    /// The game's files: `game/`, or its dir in the prefix (installer lane).
    pub game_dir: PathBuf,
    /// The exe gogo's launcher runs, relative to [`Self::game_dir`] when it
    /// is in it; `None` before the game is set up.
    pub launcher_exe: Option<String>,
    /// Play tasks first, by index; then, with `--all`, the other exes by path.
    pub exes: Vec<Exe>,
}

/// The game, as a run needs it.
struct Built {
    dirs: GameDirs,
    tree: Option<String>,
    game_files: PathBuf,
    title: String,
    conf: Option<ConfValues>,
}

fn load(gogid: &str) -> Result<Built, MacAppError> {
    let dirs = GameDirs::for_game(gogid)?;
    if !dirs.root.is_dir() {
        return Err(MacAppError::Apps(format!(
            "{gogid} isn't built: no {}",
            dirs.root.display()
        )));
    }
    let (baked, _) = game::read_launch_toml(&dirs)?;
    let tree = baked.tree_name();
    let game_files = setup::game_files(&dirs, tree.as_deref());
    let conf = ConfValues::read(&dirs.launch_conf).ok();
    Ok(Built {
        title: baked.title.clone(),
        dirs,
        tree,
        game_files,
        conf,
    })
}

impl Built {
    /// The exe `launch.conf` names, as a path.
    fn launcher_exe(&self) -> Option<PathBuf> {
        let conf = self.conf.as_ref()?;
        let exe = conf.get("EXE");
        if exe.is_empty() {
            return None;
        }
        let exe = Path::new(exe);
        if exe.is_absolute() {
            return Some(exe.to_path_buf());
        }
        // O7: relative to the data root launch.conf names, else gogo's.
        let root = Path::new(conf.get("DATA_ROOT"));
        let root = if root.is_dir() {
            root.to_path_buf()
        } else {
            crate::state::data_root()
        };
        Some(root.join(exe))
    }

    /// `p` relative to the game's files, `/`-separated (as it is, when it
    /// isn't in them).
    fn rel(&self, p: &Path) -> String {
        let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        let rest = p
            .strip_prefix(&self.game_files)
            .ok()
            .map(Path::to_path_buf)
            .or_else(|| {
                canon(p)
                    .strip_prefix(canon(&self.game_files))
                    .ok()
                    .map(Path::to_path_buf)
            });
        match rest {
            Some(r) => r.to_string_lossy().into_owned(),
            None => p.display().to_string(),
        }
    }

    /// `p` in the game's files as Wine sees it: through the prefix's
    /// `drive_c/GOG/<name>` (the link to `game/` on the depot lane), where
    /// the game's own exe runs from.
    fn via_prefix(&self, p: &Path) -> PathBuf {
        if let Some(tree) = &self.tree
            && let Ok(rest) = p.strip_prefix(&self.game_files)
        {
            let dir = self.dirs.prefix.join("drive_c/GOG").join(tree);
            if dir.exists() {
                return dir.join(rest);
            }
        }
        p.to_path_buf()
    }
}

fn bits(p: &Path) -> Option<u8> {
    pe::bitness_of(p).map(|b| match b {
        Bitness::I386 => 32,
        Bitness::X86_64 => 64,
    })
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The play tasks of every `goggame-*.info` in the game's files: the base
/// game's info first, then the DLC's by path.
fn play_tasks(b: &Built) -> Vec<Exe> {
    let mut infos = info::read_info_files(&b.game_files);
    let is_base = |i: &info::GogGameInfo| match (&i.game_id, &i.root_game_id) {
        (Some(id), Some(root)) => id == root,
        _ => i.root_game_id.is_none(),
    };
    infos.sort_by_key(|(p, i)| (!is_base(i), p.clone()));
    let mut out = Vec::new();
    for (path, i) in &infos {
        let base = path.parent().unwrap_or(&b.game_files);
        let dlc = !is_base(i);
        let id = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix("goggame-"))
            .and_then(|n| n.strip_suffix(".info"))
            .map(str::to_string);
        for task in i
            .play_tasks
            .iter()
            .filter(|t| info::is_runnable_file_task(t))
        {
            out.push(task_exe(b, base, task, out.len() + 1, id.clone(), dlc));
        }
    }
    out
}

fn task_exe(
    b: &Built,
    base: &Path,
    task: &GogTask,
    index: usize,
    game_id: Option<String>,
    dlc: bool,
) -> Exe {
    let raw = task.path.as_deref().unwrap_or_default();
    let found = info::resolve_task_path(base, raw);
    let abs = found
        .clone()
        .unwrap_or_else(|| base.join(raw.replace('\\', "/")));
    let wd = task
        .working_dir
        .as_deref()
        .filter(|w| !w.is_empty())
        .map(|w| base.join(w.replace('\\', "/")));
    let workdir_abs = wd
        .clone()
        .unwrap_or_else(|| abs.parent().unwrap_or(base).to_path_buf());
    Exe {
        index: Some(index),
        name: task
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_string),
        path: b.rel(&abs),
        args: info::split_args(task.arguments.as_deref().unwrap_or_default()),
        working_dir: wd.map(|w| b.rel(&w)),
        category: task.category.clone(),
        primary: task.is_primary,
        game_id,
        dlc,
        launcher: false,
        bits: found.as_deref().and_then(bits),
        exists: found.is_some(),
        abs,
        workdir_abs,
    }
}

/// Every `.exe` under `dir` (any case), symlinked dirs not followed.
fn walk_exes(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let Ok(ty) = e.file_type() else { continue };
        let p = e.path();
        if ty.is_dir() {
            walk_exes(&p, out);
        } else if (ty.is_file() || ty.is_symlink())
            && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe"))
            && p.is_file()
        {
            out.push(p);
        }
    }
}

/// `linux-app exes <gogid> [--all]`.
pub fn list(gogid: &str, all: bool) -> Result<Listing, MacAppError> {
    let b = load(gogid)?;
    Ok(listing(&b, gogid, all))
}

fn listing(b: &Built, gogid: &str, all: bool) -> Listing {
    let mut exes = play_tasks(b);
    if all {
        let mut found = Vec::new();
        walk_exes(&b.game_files, &mut found);
        let tasks: Vec<PathBuf> = exes
            .iter()
            .filter_map(|e| e.abs.canonicalize().ok())
            .collect();
        let mut extra: Vec<Exe> = found
            .into_iter()
            .filter(|p| p.canonicalize().map_or(true, |c| !tasks.contains(&c)))
            .map(|p| Exe {
                index: None,
                name: None,
                path: b.rel(&p),
                args: vec![],
                working_dir: None,
                category: None,
                primary: false,
                game_id: None,
                dlc: false,
                launcher: false,
                bits: bits(&p),
                exists: true,
                workdir_abs: p.parent().unwrap_or(&b.game_files).to_path_buf(),
                abs: p,
            })
            .collect();
        extra.sort_by_key(|e| e.path.to_lowercase());
        exes.extend(extra);
    }
    let launcher_exe = b.launcher_exe();
    if let Some(l) = &launcher_exe {
        let conf_args = b
            .conf
            .as_ref()
            .and_then(|c| c.arrays.get("ARGS").cloned())
            .unwrap_or_default();
        let same: Vec<usize> = (0..exes.len())
            .filter(|i| same_file(&exes[*i].abs, l))
            .collect();
        // The task with the launcher's args, else the one whose args the
        // launcher's start with (the override's extra args follow them),
        // else the first with its exe.
        let starts = same
            .iter()
            .filter(|i| conf_args.starts_with(&exes[**i].args))
            .max_by_key(|i| (exes[**i].args.len(), std::cmp::Reverse(**i)));
        if let Some(i) = same
            .iter()
            .find(|i| exes[**i].args == conf_args)
            .or(starts)
            .or(same.first())
        {
            exes[*i].launcher = true;
        }
    }
    Listing {
        gogid: gogid.to_string(),
        title: b.title.clone(),
        game_dir: b.game_files.clone(),
        launcher_exe: launcher_exe.map(|l| b.rel(&l)),
        exes,
    }
}

/// The listing as text.
pub fn render(l: &Listing) -> String {
    let mut out = format!(
        "{} ({}): game files in {}\n",
        l.title,
        l.gogid,
        l.game_dir.display()
    );
    let describe = |e: &Exe| {
        let mut tags: Vec<String> = Vec::new();
        if let Some(c) = &e.category {
            tags.push(c.clone());
        }
        if e.primary {
            tags.push("primary".into());
        }
        if e.dlc {
            tags.push(format!("DLC {}", e.game_id.as_deref().unwrap_or("?")));
        }
        if e.launcher {
            tags.push("gogo's launcher runs this".into());
        }
        let bits = match (e.exists, e.bits) {
            (false, _) => " (missing)".to_string(),
            (true, Some(b)) => format!(" ({b}-bit)"),
            (true, None) => String::new(),
        };
        let tags = if tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", tags.join(", "))
        };
        format!("{}{bits}{tags}", e.path)
    };
    let tasks: Vec<&Exe> = l.exes.iter().filter(|e| e.index.is_some()).collect();
    if tasks.is_empty() {
        out.push_str("no play tasks in the game's goggame-*.info files\n");
    } else {
        out.push_str("play tasks:\n");
    }
    for e in tasks {
        out.push_str(&format!(
            "  {:>2}  {}: {}\n",
            e.index.unwrap_or_default(),
            e.name.as_deref().unwrap_or("(no name)"),
            describe(e)
        ));
        if !e.args.is_empty() {
            out.push_str(&format!("      args: {}\n", e.args.join(" ")));
        }
        if let Some(w) = &e.working_dir {
            out.push_str(&format!("      working dir: {w}\n"));
        }
    }
    let others: Vec<&Exe> = l.exes.iter().filter(|e| e.index.is_none()).collect();
    if !others.is_empty() {
        out.push_str("other exes (not play tasks):\n");
        for e in others {
            out.push_str(&format!("      {}\n", describe(e)));
        }
    }
    match &l.launcher_exe {
        Some(x) if !l.exes.iter().any(|e| e.launcher) => {
            out.push_str(&format!("gogo's launcher runs: {x}\n"));
        }
        None => out.push_str("not set up yet: gogo's launcher runs nothing\n"),
        _ => {}
    }
    out.push_str(&format!(
        "run one: gogo linux-app run {} <number|name|path> [-- <args>…]\n",
        l.gogid
    ));
    out
}

/// A one-off run, ready for `launch.sh`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPlan {
    pub launch_sh: PathBuf,
    /// Through the prefix, as the game's exe is (`drive_c/GOG/<name>/…`).
    pub exe: PathBuf,
    pub workdir: PathBuf,
    /// The play task's args (none for an exe that isn't one), then the extra.
    pub args: Vec<String>,
    /// The exe relative to the game's files, for people.
    pub label: String,
    /// The play task it is, if any.
    pub index: Option<usize>,
}

impl RunPlan {
    /// `launch.sh`'s argv (after its own name).
    pub fn argv(&self) -> Vec<OsString> {
        let mut v: Vec<OsString> = vec![
            launcher::ONE_OFF_ARG.into(),
            self.exe.clone().into(),
            self.workdir.clone().into(),
        ];
        v.extend(self.args.iter().map(OsString::from));
        v
    }
}

/// `linux-app run <gogid> <task|path> [-- extra…]`: which exe, checked
/// against what would make the run unsafe (`launch.sh` checks the running
/// ones again, and setup's state).
pub fn plan_run(gogid: &str, selector: &str, extra: &[String]) -> Result<RunPlan, MacAppError> {
    let b = load(gogid)?;
    let launch_sh = b.dirs.root.join(LAUNCH_SH);
    if !b.dirs.launch_conf.is_file() || !launch_sh.is_file() {
        return Err(MacAppError::Apps(format!(
            "{gogid} isn't set up (no launch.conf or launch.sh): run `gogo linux-app setup {gogid}`"
        )));
    }
    if dlc::journal_exists(&b.dirs.root) {
        let (what, cmd) = dlc::pending_kind(&b.dirs.root);
        return Err(MacAppError::Apps(format!(
            "a DLC {what} of {gogid} stopped while changing its files, so they may mix two \
             versions: run `gogo linux-app dlc {gogid} {cmd}` to finish it (or undo it) first"
        )));
    }
    let l = listing(&b, gogid, false);
    let exe = pick(&b, &l, selector)?;
    if !exe.exists {
        return Err(MacAppError::Apps(format!(
            "play task {} names {}, which isn't in the game's files",
            exe.index.unwrap_or_default(),
            exe.path
        )));
    }
    game::refuse_running(gogid, &b.dirs.root, &b.dirs.prefix)?;
    let mut args = exe.args.clone();
    args.extend(extra.iter().cloned());
    Ok(RunPlan {
        launch_sh,
        exe: b.via_prefix(&exe.abs),
        workdir: b.via_prefix(&exe.workdir_abs),
        args,
        label: exe.path.clone(),
        index: exe.index,
    })
}

/// The exe `selector` names: a play task's number, a play task's name (one
/// task only, any case), or a path relative to the game's files (`/` or `\`,
/// any case). A path that is exactly one play task's exe is that task.
fn pick(b: &Built, l: &Listing, selector: &str) -> Result<Exe, MacAppError> {
    let sel = selector.trim();
    let hint = format!("`gogo linux-app exes {}` lists them", l.gogid);
    if sel.is_empty() {
        return Err(MacAppError::Apps(format!("no exe given; {hint}")));
    }
    if let Ok(n) = sel.parse::<usize>() {
        return l
            .exes
            .iter()
            .find(|e| e.index == Some(n))
            .cloned()
            .ok_or_else(|| {
                MacAppError::Apps(format!(
                    "no play task {n}: {} has {}; {hint}",
                    l.gogid,
                    l.exes.len()
                ))
            });
    }
    let named: Vec<&Exe> = l
        .exes
        .iter()
        .filter(|e| {
            e.name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(sel))
        })
        .collect();
    match named.as_slice() {
        [one] => return Ok((*one).clone()),
        [] => {}
        many => {
            let which: Vec<String> = many
                .iter()
                .map(|e| format!("{} ({})", e.index.unwrap_or_default(), e.path))
                .collect();
            return Err(MacAppError::Apps(format!(
                "{} play tasks are named {sel:?}: {}; pick one by its number or path",
                many.len(),
                which.join(", ")
            )));
        }
    }
    let rel = sel.replace('\\', "/");
    let looks_like_path = rel.contains('/') || rel.to_lowercase().ends_with(".exe");
    if !looks_like_path {
        return Err(MacAppError::Apps(format!(
            "no play task named {sel:?}; {hint}"
        )));
    }
    let rel_path = Path::new(&rel);
    if !rel_path
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(MacAppError::Apps(format!(
            "{sel:?}: give a path relative to the game's files ({}), inside them",
            b.game_files.display()
        )));
    }
    let Some(file) = info::resolve_task_path(&b.game_files, &rel) else {
        return Err(MacAppError::Apps(format!(
            "no {rel} in the game's files ({}); {hint}",
            b.game_files.display()
        )));
    };
    let tasks: Vec<&Exe> = l
        .exes
        .iter()
        .filter(|e| e.exists && same_file(&e.abs, &file))
        .collect();
    if let [one] = tasks.as_slice() {
        return Ok((*one).clone());
    }
    // Not a play task (or several with their own args): the exe alone, in
    // its own dir.
    Ok(Exe {
        index: None,
        name: None,
        path: b.rel(&file),
        args: vec![],
        working_dir: None,
        category: None,
        primary: false,
        game_id: None,
        dlc: false,
        launcher: false,
        bits: bits(&file),
        exists: true,
        workdir_abs: file.parent().unwrap_or(&b.game_files).to_path_buf(),
        abs: file,
    })
}

/// The exe of a one-off that launcher `pid` of the game in `root` runs, from
/// its `.one-offs/<pid>` record (doctor names it).
pub fn running_one_off(root: &Path, pid: u32) -> Option<String> {
    let rec = std::fs::read_to_string(root.join(launcher::ONE_OFFS).join(pid.to_string())).ok()?;
    let live = std::fs::read_to_string(root.join(".launchers").join(pid.to_string())).ok()?;
    let mut lines = rec.lines();
    (lines.next()? == live.trim())
        .then(|| lines.next().map(str::to_string))
        .flatten()
}
