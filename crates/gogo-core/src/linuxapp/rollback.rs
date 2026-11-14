//! `gogo linux-app snapshot <gogid> rollback <n>` on btrfs (design 20 §5.3.1,
//! §6): the plan, the rollback under the locks and refusals of §5.1, and its
//! recovery, which every other change of the game runs first.
//!
//! The steps, each a point a crash can stop after:
//!
//! 1. the journal `rollback.json`;
//! 2. the pre-rollback `auto` snapshot (`before rollback to <n>`, always);
//! 3. the newer saves copied to `<data_root>/saves-aside/<gogid>/<time>/`;
//! 4. the backend stages a writable copy of `<n>` beside the game dir, with
//!    the marker `.gogo-rollback` in it, and swaps it in (the commit);
//! 5. the restored game finished: `launch.conf` rewritten when the game moved
//!    since the snapshot, the L4 override restored (unless kept),
//!    `launcher::write`, runtime leftovers cleared, the marker removed;
//! 6. the old tree deleted, the journal removed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};

use super::game;
use super::snapshots::{self, Backends, ContentView, SnapshotView};
use crate::engine::events::ProgressEvent;
use crate::gog::content;
use crate::macapp::doctor::Finding;
use crate::macapp::error::MacAppError;
use crate::macapp::launch::GameDirs;
use crate::macapp::relocate::Lane as _;
use crate::snapshot::backend::{self, ROLLBACK_MARKER, RollbackStep};
use crate::snapshot::saves::{self, BuildFiles, Expect, Found, GameFiles, Group, OwnFiles};
use crate::snapshot::{
    self as snap, Guard, Kind, Meta, NewSnapshot, State, Store, Support, Trigger,
};

/// The points a rollback can be stopped after, for crash injection
/// ([`RollbackOptions::fail_at`]), in the order they happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RbStep {
    /// `rollback.json` written.
    Journal,
    /// The pre-rollback snapshot taken (and named in the journal).
    PreSnapshot,
    /// The newer saves copied aside (or not, with `--drop-saves`).
    SavesCopied,
    /// The writable copy of the target staged, with its marker.
    Staged,
    /// The swap done (and the journal says so), nothing finished yet.
    Committed,
    /// `launch.conf` rewritten for where the game is now.
    Rewritten,
    /// The L4 override restored (or kept).
    OverrideRestored,
    /// `launch.sh`, the icon and the menu entry written.
    LauncherWritten,
    /// The marker removed from the game dir.
    MarkerRemoved,
    /// The old tree deleted.
    OldTreeDeleted,
}

impl RbStep {
    pub const ALL: [RbStep; 10] = [
        RbStep::Journal,
        RbStep::PreSnapshot,
        RbStep::SavesCopied,
        RbStep::Staged,
        RbStep::Committed,
        RbStep::Rewritten,
        RbStep::OverrideRestored,
        RbStep::LauncherWritten,
        RbStep::MarkerRemoved,
        RbStep::OldTreeDeleted,
    ];
}

/// How a rollback runs.
#[derive(Debug, Clone, Default)]
pub struct RollbackOptions {
    /// `--drop-saves`: no copy aside.
    pub drop_saves: bool,
    /// `--keep-override`: the L4 file stays as it is now.
    pub keep_override: bool,
    /// `[snapshots] keep_auto`, for the retention after the pre-rollback
    /// snapshot (the target and that snapshot are never pruned by it).
    pub keep_auto: usize,
    /// The current build's file list when the caller fetched it (the depot
    /// manifests, when the depot cache doesn't hold the build); else the
    /// cache's or the installer log's, else none.
    pub build_files: Option<BuildFiles>,
    /// Crash injection: stop right after this step, leaving everything as it
    /// is (design 20 §10.2).
    #[doc(hidden)]
    pub fail_at: Option<RbStep>,
}

// — the journal —

/// `rollback.json` (design 20 §5.3.1): `{to, pre, staging}`, plus what the
/// finish needs when recovery does it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Journal {
    pub to: u32,
    #[serde(default)]
    pub pre: Option<u32>,
    pub staging: PathBuf,
    pub game_dir: PathBuf,
    #[serde(default)]
    pub keep_override: bool,
    #[serde(default)]
    pub saves_aside: Option<PathBuf>,
    /// Set right after the swap: with no marker in the game dir, it tells
    /// "the old tree is left" from "the copy was never swapped in".
    #[serde(default)]
    pub committed: bool,
}

fn read_journal(path: &Path) -> Result<Option<Journal>, MacAppError> {
    match std::fs::read_to_string(path) {
        Ok(t) => serde_json::from_str(&t)
            .map(Some)
            .map_err(|e| MacAppError::Apps(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn write_journal(path: &Path, j: &Journal) -> Result<(), MacAppError> {
    let text = serde_json::to_vec_pretty(j).expect("a journal serializes");
    snap::write_atomic(path, &text)?;
    Ok(())
}

fn journal_path(gogid: &str) -> PathBuf {
    crate::state::data_root()
        .join("snapshots")
        .join(gogid)
        .join("rollback.json")
}

// — the override —

/// One key of the L4 override a rollback changes (`key old -> new`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverrideChange {
    pub key: String,
    /// `None`: not set.
    pub old: Option<String>,
    pub new: Option<String>,
}

impl OverrideChange {
    pub fn line(&self) -> String {
        format!(
            "{} {} -> {}",
            self.key,
            self.old.as_deref().unwrap_or("(not set)"),
            self.new.as_deref().unwrap_or("(not set)")
        )
    }
}

fn flatten(prefix: &str, v: &toml::Value, out: &mut BTreeMap<String, String>) {
    match v {
        toml::Value::Table(t) => {
            for (k, v) in t {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(&key, v, out);
            }
        }
        other => {
            out.insert(prefix.to_string(), other.to_string());
        }
    }
}

fn keys(text: Option<&str>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(v) = text.and_then(|t| t.parse::<toml::Table>().ok()) {
        flatten("", &toml::Value::Table(v), &mut out);
    }
    out
}

/// The keys that differ between the L4 file now (`now`) and the one the
/// snapshot holds (`then`), as `key now -> then`. A file that doesn't parse
/// shows as one `(the whole file)` line when its bytes differ.
pub fn override_changes(now: Option<&str>, then: Option<&str>) -> Vec<OverrideChange> {
    let parses = |t: Option<&str>| t.is_none_or(|t| t.parse::<toml::Table>().is_ok());
    if !(parses(now) && parses(then)) {
        return if now == then {
            Vec::new()
        } else {
            vec![OverrideChange {
                key: "(the whole file)".into(),
                old: now.map(|_| "as it is now".into()),
                new: then.map(|_| "the snapshot's".into()),
            }]
        };
    }
    let (a, b) = (keys(now), keys(then));
    let mut all: Vec<&String> = a.keys().chain(b.keys()).collect();
    all.sort();
    all.dedup();
    all.into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .map(|k| OverrideChange {
            key: k.clone(),
            old: a.get(k).cloned(),
            new: b.get(k).cloned(),
        })
        .collect()
}

fn read_opt(path: &Path) -> Result<Option<String>, MacAppError> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

// — the plan —

/// What a rollback will do (design 20 §5.3, §6).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackPlan {
    pub gogid: String,
    pub game_dir: PathBuf,
    /// The target.
    pub to: SnapshotView,
    /// The game now.
    pub now: ContentView,
    /// The number the pre-rollback snapshot will most likely get.
    pub pre: u32,
    pub pre_label: String,
    /// The L4 keys the restore changes (`--keep-override`: the ones it would
    /// have changed).
    pub override_changes: Vec<OverrideChange>,
    pub keep_override: bool,
    pub drop_saves: bool,
    /// The newer saves, the files gogo can't attribute, and how the game's
    /// files were checked.
    pub saves: Found,
    pub groups: Vec<Group>,
    /// Where the copy goes (`<data_root>/saves-aside/<gogid>/<UTC time>/`).
    pub saves_aside: PathBuf,
    /// The unattributed new files are copied (under `unattributed/`): they
    /// are small, or the copy can clone them. Else they are listed only.
    pub copy_unattributed: bool,
    /// The game dir moved since the snapshot: `launch.conf` is rewritten.
    pub rewrite_from: Option<PathBuf>,
}

/// [`plan`]'s answer.
#[derive(Debug, Clone)]
pub enum Planned {
    Plan(Box<RollbackPlan>),
    /// The number isn't a snapshot the game can roll back to (a usage
    /// error, exit 2), and why.
    Unknown(String),
}

fn local(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()
}

fn content_line(c: &ContentView) -> String {
    let build = c
        .version
        .as_deref()
        .or(c.build_id.as_deref())
        .unwrap_or("unknown");
    let mut dlcs: Vec<String> = c.dlcs.iter().map(u64::to_string).collect();
    dlcs.sort();
    format!(
        "build {build}, {} DLC{}, {}",
        c.dlcs.len(),
        if dlcs.is_empty() {
            String::new()
        } else {
            format!(" ({})", dlcs.join(", "))
        },
        c.runtime.as_deref().unwrap_or("no runtime recorded")
    )
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl RollbackPlan {
    /// `3 (before adding Phantom Liberty)`.
    pub fn target_name(&self) -> String {
        format!(
            "{} ({})",
            self.to.id,
            self.to.label.as_deref().unwrap_or("no label")
        )
    }

    /// The question (design 20 §6.2), without the `[y/N]` the prompt adds.
    pub fn question(&self) -> String {
        let n = self.saves.files.len();
        let mut q = format!("roll back to {}?", self.target_name());
        if n > 0 {
            let files = plural(n, "newer save file", "newer save files");
            if self.drop_saves {
                q.push_str(&format!(
                    " {files} will be dropped from the game (kept in snapshot {} only)",
                    self.pre
                ));
            } else {
                q.push_str(&format!(
                    " {files} will be dropped from the game (copied to {})",
                    self.saves_aside.display()
                ));
            }
        }
        let un = self.saves.unattributed.len();
        if un > 0 && !self.drop_saves && !self.copy_unattributed {
            q.push_str(&format!(
                " {} gogo can't attribute ({}) won't be copied: no cheap clone there; they stay in snapshot {} only",
                plural(un, "new game file", "new game files"),
                crate::fmt::human_size(self.saves.unattributed_bytes()),
                self.pre
            ));
        }
        q
    }

    /// The plan as lines; `verbose` lists every file (`-v`).
    pub fn lines(&self, verbose: bool) -> Vec<String> {
        let mut out = vec![
            format!(
                "roll {} ({}) back to snapshot {}",
                self.gogid,
                self.game_dir.display(),
                self.target_name()
            ),
            format!(
                "  then: {}, {}",
                self.to
                    .created
                    .as_deref()
                    .and_then(|c| DateTime::parse_from_rfc3339(c).ok())
                    .map_or_else(|| "-".into(), |t| local(t.with_timezone(&Utc))),
                self.to
                    .content
                    .as_ref()
                    .map_or_else(|| "content unknown".into(), content_line)
            ),
            format!("  now:  {}", content_line(&self.now)),
            format!(
                "  first: snapshot {} ({}) of the game as it is now, so this can be undone",
                self.pre, self.pre_label
            ),
        ];
        if let Some(from) = &self.rewrite_from {
            out.push(format!(
                "  the snapshot was taken at {}: launch.conf is rewritten for where the game is now",
                from.display()
            ));
        }
        if self.override_changes.is_empty() {
            out.push("  override: unchanged".into());
        } else if self.keep_override {
            out.push(
                "  override: kept as it is now (--keep-override); the snapshot's differs in:"
                    .into(),
            );
            for c in &self.override_changes {
                out.push(format!("    {}", c.line()));
            }
            out.push("    the next launch runs setup for it".into());
        } else {
            out.push("  override: restored to the snapshot's copy:".into());
            for c in &self.override_changes {
                out.push(format!("    {}", c.line()));
            }
        }
        match &self.saves.game_check {
            saves::GameCheck::SameContent => out.push(
                "  game files: the same build and DLC set as the snapshot: every changed file counts"
                    .into(),
            ),
            saves::GameCheck::List { source, .. } => {
                out.push(format!(
                    "  game files: checked against the build's file list ({source}; by name and size)"
                ));
                out.push(
                    "    the build or DLC set changed: changed files matching the new build are treated as shipped"
                        .into(),
                );
            }
            saves::GameCheck::NoList => out.push(
                "  game files: the build or DLC set changed and there is no file list: changed files only"
                    .into(),
            ),
            saves::GameCheck::None => {}
        }
        if self.saves.files.is_empty() {
            out.push("  newer saves: none".into());
        } else {
            out.push(format!(
                "  newer saves: {}, {}",
                plural(self.saves.files.len(), "file", "files"),
                crate::fmt::human_size(self.saves.bytes())
            ));
            for g in &self.groups {
                out.push(format!(
                    "    {}: {}, {}, newest {}",
                    g.dir,
                    plural(g.files, "file", "files"),
                    crate::fmt::human_size(g.bytes),
                    local(g.newest_at.into())
                ));
            }
            if verbose {
                for f in &self.saves.files {
                    out.push(format!(
                        "      {} ({})",
                        f.path,
                        crate::fmt::human_size(f.size)
                    ));
                }
            }
            if self.drop_saves {
                out.push(format!(
                    "    not copied (--drop-saves): they stay in snapshot {} only",
                    self.pre
                ));
            } else {
                out.push(format!("    copied to {}", self.saves_aside.display()));
            }
        }
        let un = &self.saves.unattributed;
        if !un.is_empty() {
            let place = un[0].path.split('/').next().unwrap_or("game");
            let what = plural(un.len(), "new file", "new files");
            let size = crate::fmt::human_size(self.saves.unattributed_bytes());
            out.push(if self.drop_saves {
                format!(
                    "  {what} in {place} gogo can't attribute ({size}): not copied (--drop-saves); they stay in snapshot {}",
                    self.pre
                )
            } else if self.copy_unattributed {
                format!(
                    "  {what} in {place} gogo can't attribute ({size}): copied to {}",
                    self.saves_aside.join("unattributed").display()
                )
            } else {
                format!(
                    "  {what} in {place} gogo can't attribute ({size}): over {} and no cheap clone there, so not copied; they stay in snapshot {}",
                    crate::fmt::human_size(UNATTRIBUTED_COPY_MAX),
                    self.pre
                )
            });
            if verbose {
                for f in un {
                    out.push(format!(
                        "      {} ({})",
                        f.path,
                        crate::fmt::human_size(f.size)
                    ));
                }
            }
        }
        if !self.saves.registry.is_empty() {
            let names: Vec<&str> = self
                .saves
                .registry
                .iter()
                .map(|f| f.path.as_str())
                .collect();
            out.push(if self.drop_saves {
                format!(
                    "  registry changed ({}): not copied (--drop-saves)",
                    names.join(", ")
                )
            } else {
                format!(
                    "  registry changed ({}): copied to {}",
                    names.join(", "),
                    self.saves_aside.join("registry").display()
                )
            });
        }
        out.push(format!("  not scanned: {}", saves::NOT_SCANNED));
        if verbose {
            out.push(format!(
                "    excluded everywhere: {}",
                saves::EXCLUDED.join(", ")
            ));
        }
        out
    }
}

/// The game's own files as a save root (design 20 §6.1): where they are now
/// and in the snapshot, their list, gogo's own files there.
fn game_files(
    gogid: &str,
    dirs: &GameDirs,
    snap_root: Option<&Path>,
    target: &Meta,
    list: Option<BuildFiles>,
) -> Option<GameFiles> {
    let tree = game::read_launch_toml(dirs)
        .ok()
        .and_then(|(t, _)| t.tree_name());
    let dir = super::setup::game_files(dirs, tree.as_deref());
    if !dir.is_dir() {
        return None;
    }
    let rel = dir.strip_prefix(&dirs.root).ok()?.to_path_buf();
    let label = match rel.strip_prefix("prefix/drive_c") {
        Ok(r) => r.to_string_lossy().into_owned(),
        Err(_) => rel.to_string_lossy().into_owned(),
    };
    let now = content::load(&dirs.root).ok().flatten();
    let list = list.or_else(|| match now.as_ref().map(|c| c.lane) {
        Some(content::Lane::Installer) => {
            let dlcs: Vec<u64> = now
                .iter()
                .flat_map(|c| c.dlc.iter().map(|d| d.id))
                .collect();
            installer_list(dirs, &label, &dlcs, false)
        }
        _ => now
            .as_ref()
            .and_then(|c| cache_list(gogid, c, tree.as_deref())),
    });
    let compat: Vec<String> = game::LinuxState::load(&dirs.state_toml)
        .ok()
        .flatten()
        .map(|s| {
            s.files
                .iter()
                .filter_map(|l| l.split_once(' ').map(|(_, p)| p.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let mut now_dlcs: Vec<u64> = now
        .as_ref()
        .map(|c| c.dlc.iter().map(|d| d.id).collect())
        .unwrap_or_default();
    now_dlcs.sort_unstable();
    let mut then_dlcs = target.content.dlcs.clone();
    then_dlcs.sort_unstable();
    let same_content = now.as_ref().is_some_and(|c| {
        c.build_id == target.content.build_id
            && Some(c.lane.as_str()) == target.content.lane.as_deref()
    }) && now_dlcs == then_dlcs;
    Some(GameFiles {
        own: OwnFiles::scan(&dir, &compat),
        snap_dir: snap_root.map(|s| s.join(&rel)),
        dir,
        label,
        list,
        same_content,
    })
}

/// The depot lane's list from the depot cache, when it holds the game's
/// build: every file of the game's tree and of the support overlays there,
/// at the size the cache holds it.
pub fn cache_list(gogid: &str, c: &content::Content, tree: Option<&str>) -> Option<BuildFiles> {
    cache_list_in(
        crate::gog::depot::default_cache_dir().as_deref(),
        gogid,
        c,
        tree,
    )
}

/// [`cache_list`] with the user cache dir `cache_base`.
pub fn cache_list_in(
    cache_base: Option<&Path>,
    gogid: &str,
    c: &content::Content,
    tree: Option<&str>,
) -> Option<BuildFiles> {
    use crate::gog::depot;
    let id: u64 = gogid.parse().ok()?;
    let (root, ..) = depot::cache_roots(cache_base, id, None)?;
    let rec = depot::read_record(&root)?;
    if c.build_id.as_deref() != Some(rec.build_id.as_str()) {
        return None;
    }
    let tree = match tree {
        Some(t) => t.to_string(),
        None => {
            let dirs: Vec<String> = std::fs::read_dir(&root)
                .ok()?
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter_map(|e| e.file_name().to_str().map(str::to_string))
                .filter(|n| n != "gog-support")
                .collect();
            match dirs.as_slice() {
                [one] => one.clone(),
                _ => return None,
            }
        }
    };
    let mut list = BuildFiles::new("the depot cache");
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(root.join(&rel)) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(ty) = e.file_type() else { continue };
            let r = rel.join(e.file_name());
            if ty.is_dir() {
                stack.push(r);
            } else if ty.is_file()
                && let Ok(m) = e.metadata()
                && let Some(game_rel) = saves::depot_rel(&r.to_string_lossy(), &tree)
            {
                list.insert(&game_rel, Expect::Size(m.len()));
            }
        }
    }
    (!list.is_empty()).then_some(list)
}

/// The depot lane's list from the build's depot manifests (fetched by the
/// caller, `dlc_filter::fetch_depots`): the depots of `products` in
/// `language`, at their listed sizes.
pub fn depots_list(
    depots: &[crate::gog::dlc_filter::DepotList],
    install_dir: &str,
    language: &str,
    products: &[String],
) -> BuildFiles {
    let mut list = BuildFiles::new("the build's depot manifests");
    for d in depots {
        if !products.contains(&d.product)
            || !crate::gog::dlc_filter::language_matches(&d.languages, language)
        {
            continue;
        }
        for (path, size) in &d.sizes {
            if let Some(rel) = saves::depot_rel(path, install_dir) {
                list.insert(&rel, Expect::Size(*size));
            }
        }
    }
    list
}

/// What the caller fetches when the depot cache doesn't hold the game's
/// build (design 20 §6.1): the build's depot manifests, as
/// `depot::installed_build` and `dlc_filter::fetch_depots` give them, then
/// [`depots_list`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchWant {
    pub product_id: u64,
    pub build_id: String,
    pub language: String,
    /// The base game and its DLC, as product ids.
    pub products: Vec<String>,
}

/// A depot-lane game whose depot cache doesn't hold its build: what to fetch
/// for its file list. `None` for the installer lane, a game whose cache
/// holds the build, or one with no recorded build.
pub fn depot_fetch_wanted(gogid: &str) -> Option<FetchWant> {
    let dirs = GameDirs::for_game(gogid).ok()?;
    let c = content::load(&dirs.root).ok().flatten()?;
    if c.lane != content::Lane::Depot {
        return None;
    }
    let tree = game::read_launch_toml(&dirs)
        .ok()
        .and_then(|(t, _)| t.tree_name());
    if cache_list(gogid, &c, tree.as_deref()).is_some() {
        return None;
    }
    let mut products = vec![gogid.to_string()];
    products.extend(c.dlc.iter().map(|d| d.id.to_string()));
    Some(FetchWant {
        product_id: gogid.parse().ok()?,
        build_id: c.build_id.clone()?,
        language: c.language.clone().unwrap_or_else(|| "en-US".to_string()),
        products,
    })
}

/// The installer lane's list: the files the main installer's Inno log
/// (`install/inno.log`) and the logs of DLC `dlcs` that are there
/// (`install/dlc-<id>-inno.log`) name under the install dir (`label`,
/// relative to `drive_c/`), each expected no newer than its log. `None`
/// without the main log, or when it names no file; with `strict` (the
/// cloud-save filter) also when a DLC's log is missing, can't be read or
/// names no file there, since that DLC's files would then pass for saves.
pub fn installer_list(
    dirs: &GameDirs,
    label: &str,
    dlcs: &[u64],
    strict: bool,
) -> Option<BuildFiles> {
    let logs = dirs.root.join(crate::macapp::INSTALL_LOGS);
    let root_win = format!("C:\\{}", label.replace('/', "\\"));
    let mut list = BuildFiles::new("the installer's log");
    // `None`: the log can't be read or names no file under the install dir.
    let mut read = |log: &Path| -> Option<()> {
        let raw = std::fs::read(log).ok()?;
        let at = std::fs::metadata(log).ok()?.modified().ok()?;
        let files = saves::inno_log_files(&raw, &root_win);
        for rel in &files {
            list.insert(rel, Expect::NotAfter(at));
        }
        (!files.is_empty()).then_some(())
    };
    read(&logs.join(crate::macapp::installer::INNO_LOG))?;
    for id in dlcs {
        let read_it = read(&logs.join(super::dlc_installer::inno_log_name(*id)));
        if strict {
            read_it?;
        }
    }
    (!list.is_empty()).then_some(list)
}

/// The newer saves of the game at `dirs` against snapshot `target`.
fn find_saves(
    gogid: &str,
    dirs: &GameDirs,
    b: &dyn snap::SnapshotBackend,
    target: &Meta,
    list: Option<BuildFiles>,
) -> Found {
    let snap_root = b.snapshot_path(&dirs.root, target.id);
    saves::find(&saves::Scan {
        prefix: dirs.prefix.clone(),
        snap_prefix: snap_root.as_ref().map(|r| r.join("prefix")),
        created: target.created.into(),
        game: game_files(gogid, dirs, snap_root.as_deref(), target, list),
    })
}

/// Unattributed new files are copied up to this total even where the copy
/// can't clone them (design 20 §6.2): above it, only a clone is cheap enough.
pub const UNATTRIBUTED_COPY_MAX: u64 = 1 << 30;

/// Whether the unattributed files go aside into `aside`: none to copy, small
/// enough, or clonable there.
fn copy_unattributed(found: &Found, aside: &Path) -> bool {
    found.unattributed_bytes() <= UNATTRIBUTED_COPY_MAX
        || found
            .unattributed
            .first()
            .is_some_and(|f| saves::can_clone(&f.abs, aside))
}

/// `<data_root>/saves-aside/<gogid>/<UTC time>/`, a name not taken yet.
fn aside_dir(gogid: &str) -> PathBuf {
    let base = crate::state::data_root().join("saves-aside").join(gogid);
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let mut dir = base.join(&stamp);
    let mut i = 2;
    while dir.exists() {
        dir = base.join(format!("{stamp}-{i}"));
        i += 1;
    }
    dir
}

/// A refusal of the backend: a rollback needs btrfs and a subvolume.
fn refuse_backend(
    support: &Support,
    b: &dyn snap::SnapshotBackend,
    root: &Path,
) -> Result<(), MacAppError> {
    match support {
        Support::Btrfs => {}
        Support::Zfs { .. } => {
            return Err(MacAppError::Apps(
                "no snapshots: ZFS isn't supported yet".into(),
            ));
        }
        Support::None(reason) => return Err(MacAppError::Apps(format!("no snapshots: {reason}"))),
    }
    snapshots::refuse_unready(support, b, root)
}

/// The target, if the game can roll back to it: `Err` with the usage error
/// when it can't.
fn target(
    s: &Store,
    b: &dyn snap::SnapshotBackend,
    root: &Path,
    n: u32,
) -> Result<Result<Meta, String>, MacAppError> {
    let meta = s.load(n).map_err(snapshots::err)?;
    let on_disk = b.exists(root, n)?;
    Ok(match meta {
        Some(m)
            if m.state == State::Done
                && on_disk
                && m.override_copy == snap::OverrideCopy::Copied
                && !s.override_copy_path(n).is_file() =>
        {
            Err(format!(
                "snapshot {n}'s copy of the override file ({}) is missing: it can't be rolled back to",
                s.override_copy_path(n).display()
            ))
        }
        Some(m) if m.state == State::Done && on_disk => Ok(m),
        Some(m) if m.state != State::Done => {
            Err(format!("snapshot {n} is being created or deleted"))
        }
        Some(_) => Err(format!(
            "snapshot {n} is in the metadata but not on disk: it can't be rolled back to"
        )),
        None if on_disk => Err(format!(
            "snapshot {n} is on disk without metadata (no override copy, no date): it can't be rolled back to"
        )),
        None => Err(format!("there is no snapshot {n}")),
    })
}

/// The pre-rollback snapshot's likely number: what `create` would take.
fn next_number(s: &Store, b: &dyn snap::SnapshotBackend, root: &Path) -> u32 {
    let settings = s.settings().map(|s| s.next).unwrap_or(1);
    let metas = s.metadata().unwrap_or_default();
    let on_disk = b.on_disk(root).unwrap_or_default();
    [
        settings,
        metas.iter().map(|m| m.id + 1).max().unwrap_or(1),
        on_disk.iter().map(|o| o.n + 1).max().unwrap_or(1),
    ]
    .into_iter()
    .max()
    .unwrap_or(1)
}

/// `rollback <n>`'s plan, with every refusal checked (reads only): exit 7 on
/// a refusal before anything is asked.
pub fn plan(
    gogid: &str,
    backends: &Backends,
    n: u32,
    opts: &RollbackOptions,
) -> Result<Planned, MacAppError> {
    let dirs = snapshots::built(gogid)?;
    snapshots::refuse_for_rollback(gogid, &dirs)?;
    let (support, b) = backends.for_dir(&dirs.root);
    refuse_backend(&support, b, &dirs.root)?;
    let s = snapshots::store(gogid, &dirs.root, b);
    let meta = match target(&s, b, &dirs.root, n)? {
        Ok(m) => m,
        Err(why) => return Ok(Planned::Unknown(why)),
    };
    let l4_now = read_opt(&crate::macapp::compat::user_path(gogid))?;
    let l4_then = match meta.override_copy {
        snap::OverrideCopy::Copied => read_opt(&s.override_copy_path(n))?,
        snap::OverrideCopy::Absent => None,
    };
    let found = find_saves(gogid, &dirs, b, &meta, opts.build_files.clone());
    let saves_aside = aside_dir(gogid);
    Ok(Planned::Plan(Box::new(RollbackPlan {
        copy_unattributed: copy_unattributed(&found, &saves_aside),
        gogid: gogid.to_string(),
        to: snapshots::meta_view(&meta),
        now: ContentView::from(&snapshots::content_of(&dirs)),
        pre: next_number(&s, b, &dirs.root),
        pre_label: snap::labels::rollback(n),
        override_changes: override_changes(l4_now.as_deref(), l4_then.as_deref()),
        keep_override: opts.keep_override,
        drop_saves: opts.drop_saves,
        groups: saves::groups(&found.files),
        saves: found,
        saves_aside,
        rewrite_from: (meta.game_dir != dirs.root).then(|| meta.game_dir.clone()),
        game_dir: dirs.root,
    })))
}

// — the rollback —

/// What a rollback did (`--json`: design 20 §8's shape and more).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RolledBack {
    pub to: u32,
    pub pre: u32,
    pub saves_newer: Vec<saves::SaveFile>,
    pub saves_copied_to: Option<PathBuf>,
    pub override_restored: bool,
    pub override_changes: Vec<OverrideChange>,
    /// Snapshots a ZFS rollback destroyed: none on btrfs.
    pub destroyed: Vec<u32>,
    /// Automatic snapshots pruned after the pre-rollback one.
    pub pruned: Vec<u32>,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recovered: Vec<String>,
}

impl RolledBack {
    /// The last line (design 20 §8).
    pub fn last_line(&self) -> String {
        let saves = match &self.saves_copied_to {
            Some(d) if !self.saves_newer.is_empty() => {
                format!("; newer saves copied to {}", d.display())
            }
            Some(d) => format!(
                "; no newer saves; other changed files copied to {}",
                d.display()
            ),
            None if self.saves_newer.is_empty() => "; no newer saves".to_string(),
            None => format!("; newer saves kept in snapshot {} only", self.pre),
        };
        format!(
            "rolled back to {}; the state before is snapshot {}{saves}",
            self.to, self.pre
        )
    }
}

fn injected(step: RbStep) -> MacAppError {
    MacAppError::Apps(format!("injected failure after {step:?}"))
}

/// A crash-injection check.
fn check(fail_at: Option<RbStep>, step: RbStep) -> Result<(), MacAppError> {
    if fail_at == Some(step) {
        Err(injected(step))
    } else {
        Ok(())
    }
}

/// Step 6 of §5.3.1 on the restored game (the commit done): `launch.conf`
/// rewritten when the game moved since the snapshot, the L4 copy restored
/// (unless kept), `launcher::write`, runtime leftovers cleared, the marker
/// removed. Safe to run again: recovery does, from where a crash stopped it.
/// Returns its warnings.
fn finish(
    gogid: &str,
    dirs: &GameDirs,
    s: &Store,
    j: &Journal,
    target: Option<&Meta>,
    fail_at: Option<RbStep>,
) -> Result<Vec<String>, MacAppError> {
    check(fail_at, RbStep::Committed)?;
    let mut warnings = Vec::new();
    let lane = super::relocate::LinuxLane::default();
    if let Some(m) = target
        && m.game_dir != dirs.root
    {
        let from = GameDirs::at(m.game_dir.clone());
        warnings.extend(lane.rewrite_inside(&from, dirs, &dirs.root)?);
    }
    check(fail_at, RbStep::Rewritten)?;
    if !j.keep_override {
        match target {
            Some(m) => {
                let l4 = crate::macapp::compat::user_path(gogid);
                match m.override_copy {
                    snap::OverrideCopy::Copied => match std::fs::read(s.override_copy_path(m.id)) {
                        Ok(bytes) => snap::write_atomic(&l4, &bytes)?,
                        // Checked before the commit; gone since (a crash and
                        // a hand edit): keep the current file, as when the
                        // metadata is gone.
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            warnings.push(format!(
                                "snapshot {}'s copy of the override file is gone: the override file was left as it is",
                                m.id
                            ))
                        }
                        Err(e) => return Err(e.into()),
                    },
                    snap::OverrideCopy::Absent => snap::remove_file(&l4)?,
                }
            }
            None => warnings.push(format!(
                "snapshot {}'s metadata is gone: the override file was left as it is",
                j.to
            )),
        }
    }
    check(fail_at, RbStep::OverrideRestored)?;
    lane.publish(gogid, dirs)?;
    check(fail_at, RbStep::LauncherWritten)?;
    for d in [".launchers", super::launcher::ONE_OFFS] {
        let p = dirs.root.join(d);
        if p.symlink_metadata().is_ok() {
            crate::gog::materialize::remove_tree(&p)?;
        }
    }
    snap::remove_file(&dirs.root.join(ROLLBACK_MARKER))?;
    check(fail_at, RbStep::MarkerRemoved)?;
    Ok(warnings)
}

/// Append a line to the game's `launch.log`, as the launcher writes them:
/// the rollback is part of its history (design 20 §3).
fn log_line(gogid: &str, line: &str) {
    use std::io::Write as _;
    let dir = crate::state::logs_root().join(gogid);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("launch.log"))
    {
        let _ = writeln!(
            f,
            "{} [{}] {line}",
            Local::now().format("%Y-%m-%d %H:%M:%S"),
            std::process::id()
        );
    }
}

/// `snapshot <gogid> rollback <n>` on btrfs (design 20 §5.3.1), under the
/// locks and refusals of §5.1, after any interrupted create, delete or
/// rollback is settled. `plan` is what the user was shown: its aside dir is
/// used; the saves are found again under the locks.
pub fn rollback(
    gogid: &str,
    backends: &Backends,
    n: u32,
    plan: &RollbackPlan,
    opts: &RollbackOptions,
) -> Result<RolledBack, MacAppError> {
    let dirs = snapshots::built(gogid)?;
    let (mut out, recovered) =
        snapshots::locked(gogid, &dirs, backends, false, |s, g, support, b| {
            snapshots::refuse_for_rollback(gogid, &dirs)?;
            refuse_backend(support, b, &dirs.root)?;
            let meta = match target(s, b, &dirs.root, n)? {
                Ok(m) => m,
                Err(why) => return Err(MacAppError::Apps(why)),
            };
            let found = find_saves(gogid, &dirs, b, &meta, opts.build_files.clone());
            let l4_now = read_opt(&crate::macapp::compat::user_path(gogid))?;
            let l4_then = match meta.override_copy {
                snap::OverrideCopy::Copied => read_opt(&s.override_copy_path(n))?,
                snap::OverrideCopy::Absent => None,
            };
            let changes = override_changes(l4_now.as_deref(), l4_then.as_deref());
            run(gogid, &dirs, s, g, b, &meta, found, changes, plan, opts)
        })?;
    out.recovered = recovered;
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn run(
    gogid: &str,
    dirs: &GameDirs,
    s: &Store,
    g: &Guard,
    b: &dyn snap::SnapshotBackend,
    meta: &Meta,
    found: Found,
    changes: Vec<OverrideChange>,
    plan: &RollbackPlan,
    opts: &RollbackOptions,
) -> Result<RolledBack, MacAppError> {
    let n = meta.id;
    let jpath = s.rollback_journal_path();
    let staging = backend::rollback_staging(&dirs.root)
        .ok_or_else(|| MacAppError::Apps(format!("{} has no parent", dirs.root.display())))?;
    let mut j = Journal {
        to: n,
        pre: None,
        staging: staging.clone(),
        game_dir: dirs.root.clone(),
        keep_override: opts.keep_override,
        saves_aside: None,
        committed: false,
    };
    write_journal(&jpath, &j)?;
    check(opts.fail_at, RbStep::Journal)?;

    // D3: always, autosnapshot on or off. It makes the rollback undoable and
    // holds the newer saves.
    let pre = s
        .create(
            g,
            NewSnapshot {
                kind: Kind::Auto,
                trigger: Some(Trigger::Rollback),
                label: Some(snap::labels::rollback(n)),
                content: snapshots::content_of(dirs),
            },
        )
        .map_err(|e| {
            // Nothing changed yet: drop the journal so no settling is pending.
            let _ = snap::remove_file(&jpath);
            MacAppError::Apps(format!(
                "the snapshot before the rollback couldn't be taken, so nothing was changed: {e}"
            ))
        })?;
    j.pre = Some(pre.id);
    write_journal(&jpath, &j)?;
    check(opts.fail_at, RbStep::PreSnapshot)?;

    let copied_to = if opts.drop_saves
        || (found.files.is_empty() && found.unattributed.is_empty() && found.registry.is_empty())
    {
        None
    } else {
        let dir = if plan.saves_aside.exists() {
            aside_dir(gogid)
        } else {
            plan.saves_aside.clone()
        };
        // The plan's choice, asked about: never copy more than was shown.
        let unattributed = plan.copy_unattributed && copy_unattributed(&found, &dir);
        let mut any = false;
        if !found.files.is_empty() {
            saves::copy_aside(&found.files, &dir)?;
            any = true;
        }
        if unattributed && !found.unattributed.is_empty() {
            saves::copy_aside_under(&found.unattributed, &dir, "unattributed")?;
            any = true;
        }
        if !found.registry.is_empty() {
            saves::copy_aside_under(&found.registry, &dir, "registry")?;
            any = true;
        }
        if any {
            j.saves_aside = Some(dir.clone());
            write_journal(&jpath, &j)?;
            Some(dir)
        } else {
            None
        }
    };
    check(opts.fail_at, RbStep::SavesCopied)?;

    let mut warnings = Vec::new();
    let fail_at = opts.fail_at;
    let mut step = |st: RollbackStep| -> std::io::Result<()> {
        let r = match st {
            // The staged tree's `launch.sh` is the snapshot's, which may not
            // know the marker: until step 6 rewrites it, a launch from the
            // menu right after the swap would run the old script. This
            // gogo's goes in first, so the marker refuses that launch.
            RollbackStep::Staged => super::launcher::write_script(&staging)
                .map_err(MacAppError::from)
                .and_then(|()| check(fail_at, RbStep::Staged)),
            RollbackStep::Committed => {
                j.committed = true;
                write_journal(&jpath, &j)
                    .and_then(|()| finish(gogid, dirs, s, &j, Some(meta), fail_at))
                    .map(|w| warnings.extend(w))
            }
            RollbackStep::OldTreeDeleted => check(fail_at, RbStep::OldTreeDeleted),
        };
        r.map_err(|e| std::io::Error::other(e.to_string()))
    };
    b.rollback(&dirs.root, n, &mut step)?;
    snap::remove_file(&jpath)?;

    // Retention as after any `auto` snapshot (design 20 §7.2), once the
    // rollback is done: the pre-rollback snapshot counts as the new one, the
    // target is never pruned. A failed prune is a warning: the rollback is
    // done.
    let mut retention_warnings = Vec::new();
    let pruned = snapshots::retain(s, g, opts.keep_auto, &[pre.id, n], &mut |ev| {
        if let ProgressEvent::Warn { message } = ev {
            retention_warnings.push(message.clone());
        }
    })
    .into_iter()
    .map(|m| m.id)
    .collect();
    warnings.extend(retention_warnings);
    log_line(
        gogid,
        &format!(
            "rolled back to snapshot {n} ({}) by gogo; the state before is snapshot {}",
            meta.label, pre.id
        ),
    );
    Ok(RolledBack {
        to: n,
        pre: pre.id,
        saves_newer: found.files,
        saves_copied_to: copied_to,
        override_restored: !opts.keep_override,
        override_changes: changes,
        destroyed: Vec::new(),
        pruned,
        warnings,
        recovered: Vec::new(),
    })
}

// — recovery —

/// What settling an interrupted rollback did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// Past the commit: the restored game was finished, the old tree deleted.
    Finished { to: u32, pre: Option<u32> },
    /// Before the commit: the game is as it was; a staged copy was deleted.
    Undone { to: u32, pre: Option<u32> },
}

impl Settled {
    pub fn line(&self) -> String {
        match self {
            Settled::Finished { to, pre } => format!(
                "an interrupted rollback to snapshot {to} was finished{}",
                pre.map(|p| format!(" (the state before is snapshot {p})"))
                    .unwrap_or_default()
            ),
            Settled::Undone { to, pre } => format!(
                "an interrupted rollback to snapshot {to} was undone: the game is as it was{}",
                pre.map(|p| format!(" (snapshot {p} was taken before it)"))
                    .unwrap_or_default()
            ),
        }
    }
}

/// Settle a pending rollback of the game at `dirs` (design 20 §5.3.1's
/// table), under the locks: the game dir holding the marker → finish step 6
/// on, delete the old tree; no marker and the journal past the commit → delete
/// the old tree; else → delete a staged copy, if any. The journal goes last.
pub(crate) fn settle_locked(
    gogid: &str,
    dirs: &GameDirs,
    s: &Store,
    b: &dyn snap::SnapshotBackend,
) -> Result<Option<Settled>, MacAppError> {
    let jpath = s.rollback_journal_path();
    let Some(j) = read_journal(&jpath)? else {
        return Ok(None);
    };
    let staging = backend::rollback_staging(&dirs.root).unwrap_or_else(|| j.staging.clone());
    let settled = if dirs.root.join(ROLLBACK_MARKER).symlink_metadata().is_ok() {
        let target = s.load(j.to).map_err(snapshots::err)?;
        finish(gogid, dirs, s, &j, target.as_ref(), None)?;
        b.discard(&staging)?;
        Settled::Finished {
            to: j.to,
            pre: j.pre,
        }
    } else if j.committed {
        b.discard(&staging)?;
        Settled::Finished {
            to: j.to,
            pre: j.pre,
        }
    } else {
        b.discard(&staging)?;
        Settled::Undone {
            to: j.to,
            pre: j.pre,
        }
    };
    snap::remove_file(&jpath)?;
    log_line(gogid, &format!("gogo: {}", settled.line()));
    Ok(Some(settled))
}

/// Settle a pending rollback of `gogid`, then a pending conversion to a
/// subvolume (§2.1, [`super::convert::settle`]), before another change of the
/// game (`dlc add`, `move`, `apps remove`, a build, any `snapshot` command):
/// nothing when there is none, else under the locks, after the refusals
/// settling takes. Returns what it says.
pub fn settle_pending(gogid: &str, backends: &Backends) -> Result<Vec<String>, MacAppError> {
    if crate::macapp::compat::validate_gogid(gogid).is_err() {
        return Ok(Vec::new());
    }
    let mut out = settle_rollback(gogid, backends)?;
    out.extend(super::convert::settle(gogid)?);
    // Snapshots a move across filesystems couldn't delete (design 20 §9.1):
    // tried again, and a failure leaves them for `doctor` to report.
    out.extend(super::relocate::settle_leftovers(gogid, backends).unwrap_or_default());
    Ok(out)
}

/// [`settle_pending`]'s rollback part.
fn settle_rollback(gogid: &str, backends: &Backends) -> Result<Vec<String>, MacAppError> {
    if !journal_path(gogid).is_file() {
        return Ok(Vec::new());
    }
    let dirs = GameDirs::for_game(gogid)?;
    if !dirs.root.is_dir() {
        return Ok(Vec::new());
    }
    let root_lock = crate::macapp::relocate::lock_moves()?;
    let game_lock = crate::macapp::relocate::lock_game(gogid)?;
    let _guard = Guard::new(&root_lock, &game_lock);
    snapshots::refuse_settle(gogid, &dirs)?;
    let (_, b) = backends.for_dir(&dirs.root);
    let s = snapshots::store(gogid, &dirs.root, b);
    Ok(settle_locked(gogid, &dirs, &s, b)?
        .map(|r| vec![r.line()])
        .unwrap_or_default())
}

/// `doctor`'s findings about a pending rollback (design 20 §8): the journal,
/// the marker in the game dir, a leftover staging dir. Each a `fail` naming
/// the command that settles it.
pub fn doctor_findings(gogid: &str, root: &Path) -> Vec<Finding> {
    const CHECK: &str = "snapshots";
    let hint = format!(
        "`gogo linux-app snapshot {gogid} list` settles it (so does any change of the game)"
    );
    let mut out = Vec::new();
    let journal = read_journal(&journal_path(gogid)).ok().flatten();
    if let Some(j) = &journal {
        out.push(Finding::fail(
            CHECK,
            format!(
                "a rollback to snapshot {} didn't finish (rollback.json)",
                j.to
            ),
            hint.clone(),
        ));
    }
    if let Some(n) = backend::read_rollback_marker(root).or_else(|| {
        root.join(ROLLBACK_MARKER)
            .symlink_metadata()
            .is_ok()
            .then_some(0)
    }) && journal.is_none()
    {
        out.push(Finding::fail(
            CHECK,
            format!(
                "the game dir holds {ROLLBACK_MARKER}{} with no rollback journal: the launcher \
                 won't start it",
                if n > 0 {
                    format!(" (snapshot {n})")
                } else {
                    String::new()
                }
            ),
            format!(
                "if no rollback is running, remove {} after checking the game",
                root.join(ROLLBACK_MARKER).display()
            ),
        ));
    }
    if let Some(st) = backend::rollback_staging(root)
        && st.symlink_metadata().is_ok()
        && journal.is_none()
    {
        out.push(Finding::fail(
            CHECK,
            format!("{} is a rollback's leftover", st.display()),
            format!(
                "if no rollback is running, delete it (a btrfs subvolume: `btrfs subvolume \
                 delete {}`)",
                st.display()
            ),
        ));
    }
    out
}

/// The saves-aside dirs of `gogid` with their sizes, newest first (design 20
/// §6.2).
pub fn saves_aside(gogid: &str) -> Vec<(PathBuf, u64)> {
    let mut dirs: Vec<PathBuf> =
        std::fs::read_dir(crate::state::data_root().join("saves-aside").join(gogid))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
    dirs.sort();
    dirs.reverse();
    dirs.into_iter()
        .map(|d| {
            let bytes = tree_bytes(&d);
            (d, bytes)
        })
        .collect()
}

fn tree_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(e.path()),
                Ok(t) if t.is_file() => total += e.metadata().map(|m| m.len()).unwrap_or(0),
                _ => {}
            }
        }
    }
    total
}

/// Remove `<data_root>/saves-aside/<gogid>/` (after `apps remove` asked,
/// default no: design 20 §9.3). Returns the dir when there was one.
pub fn remove_saves_aside(gogid: &str) -> Result<Option<PathBuf>, MacAppError> {
    crate::macapp::compat::validate_gogid(gogid)?;
    let dir = crate::state::data_root().join("saves-aside").join(gogid);
    if !dir.is_dir() {
        return Ok(None);
    }
    crate::gog::materialize::remove_tree(&dir)?;
    Ok(Some(dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_changes_are_key_old_new() {
        let now = "renderer = \"dxvk\"\n[env]\nFOO = \"new\"\nBAR = \"1\"\n";
        let then = "renderer = \"wined3d\"\n[env]\nFOO = \"old\"\n";
        let lines: Vec<String> = override_changes(Some(now), Some(then))
            .iter()
            .map(OverrideChange::line)
            .collect();
        assert_eq!(
            lines,
            [
                "env.BAR \"1\" -> (not set)",
                "env.FOO \"new\" -> \"old\"",
                "renderer \"dxvk\" -> \"wined3d\""
            ]
        );
        assert!(override_changes(Some(now), Some(now)).is_empty());
        assert_eq!(
            override_changes(None, Some("x = 1\n"))[0].line(),
            "x (not set) -> 1"
        );
        assert_eq!(
            override_changes(Some("not toml ["), None)[0].key,
            "(the whole file)"
        );
    }
    /// The installer lane's list holds the DLC installers' files too, so a
    /// rollback doesn't take them for newer saves (design 20 §6.1).
    #[test]
    fn dlc_installer_logs_keep_dlc_files_out_of_the_newer_saves() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let dirs = GameDirs::at(root.clone());
        let gf = dirs.prefix.join("drive_c/GOG/G");
        std::fs::create_dir_all(gf.join("DLC")).unwrap();
        std::fs::create_dir_all(gf.join("SAVE")).unwrap();
        for f in ["Game.exe", "DLC/extra.pak", "SAVE/slot1.sav"] {
            std::fs::write(gf.join(f), b"x").unwrap();
        }
        let logs = root.join(crate::macapp::INSTALL_LOGS);
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(
            logs.join(crate::macapp::installer::INNO_LOG),
            "Dest filename: C:\\GOG\\G\\Game.exe\r\n",
        )
        .unwrap();
        std::fs::write(
            logs.join(super::super::dlc_installer::inno_log_name(9)),
            "Dest filename: C:\\GOG\\G\\DLC\\extra.pak\r\n",
        )
        .unwrap();
        let find = |list| {
            saves::find(&saves::Scan {
                prefix: dirs.prefix.clone(),
                snap_prefix: None,
                created: std::time::SystemTime::UNIX_EPOCH,
                game: Some(GameFiles {
                    dir: gf.clone(),
                    snap_dir: None,
                    label: "GOG/G".into(),
                    list,
                    own: OwnFiles::default(),
                    same_content: false,
                }),
            })
        };
        let paths =
            |found: &Found| -> Vec<String> { found.files.iter().map(|f| f.path.clone()).collect() };
        let with_dlc = find(installer_list(&dirs, "GOG/G", &[9], false));
        assert_eq!(paths(&with_dlc), ["GOG/G/SAVE/slot1.sav"]);
        let without = find(installer_list(&dirs, "GOG/G", &[], false));
        assert_eq!(
            paths(&without),
            ["GOG/G/DLC/extra.pak", "GOG/G/SAVE/slot1.sav"],
            "without the DLC's log its files pass for saves"
        );
    }
}
