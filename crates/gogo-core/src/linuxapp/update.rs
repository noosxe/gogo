//! `gogo linux-app update <gogid>` on the depot lane (design 21 §7, §11g).
//!
//! The network side (the listing, both build manifests, the depot lists of what the
//! target drops, the download) is the caller's: [`plan_update`] takes what it
//! fetched, and the download is a closure, so everything here runs on fixtures.
//!
//! An update: the game's lock; a pending lay journal settled first; the `before
//! update <old> → <new>` snapshot; the depot cache switched to the target build
//! (the caller's closure, `depot::run`), with a walk of the cache before it kept
//! in `update-download.json`; the files the switch wrote laid onto `game/` and the
//! ones the target dropped taken out of it, under design 19's journal
//! ([`crate::gog::lay`], `kind = "update"`); `build-manifest.json` and
//! `content.toml` written (the commit); setup run once.
//!
//! An installed DLC the target build lacks is taken out in the same run (§11h):
//! the cache switch keeps it on the old side of gog-warp's diff, so the cache ends
//! up as the target without it; [`super::dlc_remove::reverse_diff_to`] across the
//! two builds names its files (taken out of `game/` while they are still the ones
//! it laid) and the paths it shares with a product that stays (laid again from the
//! switched cache); the commit drops its rows from `content.toml`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gog_warp::content_system::types::Manifest;
use serde::{Deserialize, Serialize};

use crate::engine::events::ProgressEvent;
use crate::gog::content::{self, DlcRef};
use crate::gog::depot::{self, CacheCheck, CacheWant, DepotError, Snapshot};
use crate::gog::dlc_filter::DepotList;
use crate::gog::lay::{self, AddingDlc, Direction, Journal, LayReport};
use crate::gog::materialize::{self, Reflink, TreeMethod};
use crate::gog::{build_manifest, content::Lane};
use crate::macapp::error::MacAppError;

use super::dlc::{self, AddError, Game};
use super::dlc_remove::{self, Expect};
use super::game;

/// The update's download record in the game dir (§11g step 4).
pub const DOWNLOAD: &str = "update-download.json";
/// The target build's manifest, until the commit renames it to
/// `build-manifest.json`.
pub const NEXT_MANIFEST: &str = "build-manifest.next.json";

/// A build an update goes from or to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildSide {
    pub build_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// `None` on the default branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u32>,
}

impl BuildSide {
    /// The version name, or the id for a build without one.
    pub fn label(&self) -> String {
        self.version
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or(&self.build_id)
            .to_string()
    }
}

/// A file the target drops, as the installed build's depot manifest gives it: a
/// path relative to the cache root, with the size and MD5 the manifest has for
/// it. The copy in `game/` is only taken out when it still matches (§11g).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DroppedFile {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
}

/// Why an update is refused before anything is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub message: String,
    /// A usage error (exit 2): a DLC or the language the target lacks. Else exit 7.
    pub usage: bool,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}

fn refuse(message: String) -> Refusal {
    Refusal {
        message,
        usage: false,
    }
}

/// The rebuild that updates `gogid` to `build` instead (design 18 §6): it keeps the
/// prefix, saves and L4, and the recorded DLC.
pub fn rebuild_hint(gogid: &str, build: &str) -> String {
    format!("gogo linux-app {gogid} --build {build} --yes (keeps the prefix, saves and L4)")
}

/// An installer-lane game is updated by a re-download and a rebuild (§6, §11 Q5).
pub fn installer_refusal(gogid: &str) -> Refusal {
    refuse(format!(
        "{gogid} was built from GOG's offline installer, which comes in the newest version \
         only and isn't patched in place: update it with a re-download and a rebuild \
         (keeps the prefix, saves and L4): `gogo update {gogid}`, then `gogo linux-app \
         {gogid} --yes`"
    ))
}

/// Both builds' depot lists in the game's language (`dlc_filter::fetch_depots`):
/// what a target that lacks an installed DLC is planned from (§11h).
pub struct DepotLists<'a> {
    /// The installed build's depots of the base and every installed DLC.
    pub from: &'a [DepotList],
    /// The installed build's install directory: its tree's name in those paths.
    pub from_tree: String,
    /// The target's depots of the base and the DLC that stay.
    pub to: &'a [DepotList],
}

/// What [`plan_update`] needs from the network side.
pub struct PlanInput<'a> {
    pub game: &'a Game,
    pub title: &'a str,
    /// The target build.
    pub to: BuildSide,
    /// `--build` chose it.
    pub picked: bool,
    /// The target is older than the installed build.
    pub downgrade: bool,
    /// The installed build's version name and generation, from the listing or
    /// `build-manifest.json`; generation `None`: no manifest of it is known.
    pub from_version: Option<String>,
    pub from_generation: Option<u32>,
    /// The DLC product ids the target build has.
    pub target_dlcs: &'a [u64],
    /// The target's languages when it lacks the game's, for the message.
    pub language_missing: Option<String>,
    /// Both builds lay their tree in the same dir (§11f).
    pub same_install_dir: bool,
    /// The target's install directory: the tree's name in the cache.
    pub tree_name: String,
    /// The files the installed build lays and the target doesn't, from both
    /// builds' depot manifests (empty when they couldn't be listed).
    pub dropped: Vec<DroppedFile>,
    /// Said under the plan (a depot listing that failed…).
    pub notes: Vec<String>,
    /// The target's whole download and disk size for the game's language and DLC.
    pub download_size: u64,
    pub disk_size: u64,
    /// The target's manifest, for `build-manifest.json`: `(id, generation, manifest)`.
    pub manifest: Option<(String, u32, Manifest)>,
    /// Both builds' depot lists, when they could be fetched. An installed DLC the
    /// target lacks needs them: without them the update is refused.
    pub depots: Option<DepotLists<'a>>,
    /// The `auto` snapshot will be taken first (for the saves line).
    pub autosnapshot: bool,
}

/// What `update` will do.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePlan {
    pub gogid: String,
    pub title: String,
    pub from: BuildSide,
    pub to: BuildSide,
    pub picked: bool,
    pub downgrade: bool,
    pub language: String,
    /// The DLC the game has.
    pub dlcs: Vec<DlcRef>,
    /// The DLC of `dlcs` the target lacks: the update takes them out (§11h).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub removing: Vec<DlcRef>,
    /// Their files as the installed build lays them (paths in the installed
    /// build's tree, `from_tree`), each taken out while it still matches.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dlc_files: Vec<Expect>,
    /// Paths they share with a product that stays, whose copy in `game/` isn't
    /// the target's: the target's version (paths in its tree), laid from the
    /// switched cache.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dlc_restored: Vec<Expect>,
    #[serde(skip)]
    pub from_tree: String,
    /// Dirs (in `game/`, lowercased) a product that stays lists a file under: a dir
    /// the removed DLC's files leave empty goes unless it is one of these.
    #[serde(skip)]
    pub kept_dirs: HashSet<String>,
    /// Every path (in `game/`, lowercased) the target lays for the products that
    /// stay: never taken out.
    #[serde(skip)]
    pub target_paths: HashSet<String>,
    #[serde(skip)]
    pub autosnapshot: bool,
    /// An earlier run stopped during its download: this one resumes it.
    pub resume: bool,
    pub cache_root: PathBuf,
    pub tree_name: String,
    /// How many files the target drops, by the depot manifests.
    pub dropped: usize,
    #[serde(skip)]
    pub dropped_paths: Vec<DroppedFile>,
    /// `reflink` or `copy`.
    pub game_files: String,
    pub game_dir: PathBuf,
    /// The whole build's: an upper bound, only changed files are fetched.
    pub download_size: u64,
    pub disk_size: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(skip)]
    pub manifest: Option<(String, u32, Manifest)>,
}

// No derived `Debug`: the manifest carries the game's client secret.
impl std::fmt::Debug for UpdatePlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdatePlan")
            .field("gogid", &self.gogid)
            .field("from", &self.from)
            .field("to", &self.to)
            .field("resume", &self.resume)
            .finish_non_exhaustive()
    }
}

/// [`plan_update`]'s answer.
#[derive(Debug)]
pub enum Planned {
    /// The target is the installed build.
    UpToDate(BuildSide),
    Update(Box<UpdatePlan>),
}

/// The game's language, or the depot default.
pub fn language_of(game: &Game) -> String {
    game.content
        .language
        .clone()
        .unwrap_or_else(|| dlc::DEFAULT_LANGUAGE.to_string())
}

/// The installed build, as `content.toml` says.
fn installed(game: &Game) -> Option<String> {
    game.content
        .build_id
        .clone()
        .filter(|b| !b.trim().is_empty())
}

/// Why the depot cache can't be the update's source (§7.1 step 6, §11 Q6): it must
/// be on the installed build `build_id`, in `language`, hold every installed DLC
/// and have lost nothing. `None` when it can.
pub fn cache_problem(
    game: &Game,
    build_id: &str,
    language: &str,
    tree_name: &str,
) -> Option<String> {
    let Some(cache) = game.cache_root.as_ref() else {
        return Some("there is no user cache dir".into());
    };
    if !cache.join(tree_name).is_dir() {
        return Some(format!(
            "there is no depot cache of the game ({})",
            cache.display()
        ));
    }
    let dlcs: Vec<String> = game.installed().iter().map(u64::to_string).collect();
    let want = CacheWant {
        build_id: Some(build_id),
        language: Some(language),
        dlcs: &dlcs,
    };
    match depot::check_cache(cache, &want) {
        CacheCheck::Complete => None,
        CacheCheck::Unrecorded => {
            Some("the depot cache has no record of a finished download".into())
        }
        CacheCheck::Missing { count, .. } => Some(format!(
            "the depot cache lost {count} file(s) its record names"
        )),
        CacheCheck::UnknownContent => {
            Some("the depot cache's record predates DLC and language tracking".into())
        }
        CacheCheck::MissingDlc { missing } => Some(format!(
            "the depot cache doesn't hold the installed DLC {}",
            missing.join(", ")
        )),
        CacheCheck::OtherLanguage => Some(format!("the depot cache isn't in {language}")),
        CacheCheck::OtherBuild => Some(format!(
            "the depot cache holds build {}, not the installed {build_id}",
            depot::read_record(cache).map_or("?".into(), |r| r.build_id)
        )),
        CacheCheck::Interrupted { from, to } => Some(format!(
            "the depot cache's switch from build {from} to {to} didn't finish"
        )),
    }
}

fn no_cache(gogid: &str, to: &str, why: &str) -> Refusal {
    refuse(format!(
        "no usable depot cache for the update: {why}. gogo doesn't rebuild the cache from \
         the game's files; update with a rebuild instead: {}",
        rebuild_hint(gogid, to)
    ))
}

/// Pure: the plan, or why the update is refused (§11g "Refusals"), or that the
/// game is up to date. Reads the game dir and the cache record, writes nothing.
pub fn plan_update(input: PlanInput<'_>) -> Result<Planned, Refusal> {
    let game = input.game;
    let gogid = &game.gogid;
    if game.content.lane == Lane::Installer {
        return Err(installer_refusal(gogid));
    }
    let Some(from_id) = installed(game) else {
        return Err(refuse(format!(
            "the game's build isn't known (no build in {}), so there is nothing to update \
             from; update with a rebuild instead: {}",
            content::FILE,
            rebuild_hint(gogid, &input.to.build_id)
        )));
    };
    let from = BuildSide {
        build_id: from_id.clone(),
        version: input
            .from_version
            .clone()
            .or_else(|| game.content.version.clone()),
        branch: game.content.branch.clone(),
        generation: input.from_generation,
    };
    if input.to.build_id == from_id {
        return Ok(Planned::UpToDate(from));
    }
    let root = &game.dirs.root;
    if dlc::download_pending(root) || root.join(lay::STAGING).exists() {
        let (what, cmd) = dlc::pending_kind(root);
        return Err(refuse(format!(
            "a DLC {what} of {gogid} stopped during its download: finish it first with `gogo \
             linux-app dlc {gogid} {cmd}`"
        )));
    }
    let mut notes = input.notes;
    // A record from another installed build (a snapshot rollback since) is stale:
    // the update discards it (§11g).
    let pending = match read_download(root) {
        Some(p) if p.from != from_id => {
            notes.push(stale_note(&p, &from_id));
            None
        }
        p => p,
    };
    if let Some(p) = &pending
        && p.to.build_id != input.to.build_id
    {
        return Err(refuse(format!(
            "an update of {gogid} from build {} to {} stopped during its download; finish it \
             first: `gogo linux-app update {gogid} --build {}`",
            p.from, p.to.build_id, p.to.build_id
        )));
    }
    let to = &input.to.build_id;
    let Some(from_generation) = input.from_generation else {
        return Err(refuse(format!(
            "GOG no longer lists the installed build {from_id} and the game has no {}, so \
             there is no manifest to update from; update with a rebuild instead: {}",
            build_manifest::FILE,
            rebuild_hint(gogid, to)
        )));
    };
    let to_generation = input.to.generation.unwrap_or(2);
    if from_generation != 2 || to_generation != 2 {
        return Err(refuse(format!(
            "build {} is a generation 1 build, which gogo updates only with a rebuild: {}",
            if from_generation != 2 { &from_id } else { to },
            rebuild_hint(gogid, to)
        )));
    }
    if !input.same_install_dir {
        return Err(refuse(format!(
            "build {to} installs into another directory than the installed build, so it \
             can't be laid over it; update with a rebuild instead: {}",
            rebuild_hint(gogid, to)
        )));
    }
    let removing: Vec<DlcRef> = game
        .content
        .refs()
        .into_iter()
        .filter(|d| !input.target_dlcs.contains(&d.id))
        .collect();
    let removing_ids: Vec<u64> = removing.iter().map(|d| d.id).collect();
    let language = language_of(game);
    if let Some(langs) = &input.language_missing {
        return Err(Refusal {
            message: format!(
                "build {to} has no {language}, the game's language (it has {langs}); gogo \
                 never switches a game's language on an update"
            ),
            usage: true,
        });
    }
    if !removing.is_empty() && input.depots.is_none() {
        return Err(refuse(format!(
            "build {to} doesn't have the installed DLC {}, and the files to take out with \
             it couldn't be listed (both builds' depot manifests are needed to check them \
             before they go), so nothing was changed; remove it first: gogo linux-app dlc \
             {gogid} remove{}, then the update; or a rebuild without it: {}",
            named(&removing),
            removing_ids
                .iter()
                .map(|d| format!(" {d}"))
                .collect::<String>(),
            rebuild_without(game, to, &removing_ids)
        )));
    }
    let resume = pending.is_some();
    if !resume && let Some(why) = cache_problem(game, &from_id, &language, &input.tree_name) {
        return Err(no_cache(gogid, to, &why));
    }
    let cache_root = game
        .cache_root
        .clone()
        .ok_or_else(|| no_cache(gogid, to, "there is no user cache dir"))?;
    let method = match materialize::plan_tree(&cache_root, &game.dirs.game, input.disk_size).method
    {
        TreeMethod::Reflink => "reflink",
        TreeMethod::Copy => "copy",
    };
    let mut drop = DlcDrop::default();
    if let (false, Some(d)) = (removing.is_empty(), &input.depots) {
        drop = plan_drop(game, d, &input.tree_name, to, &removing_ids)?;
    }
    Ok(Planned::Update(Box::new(UpdatePlan {
        gogid: gogid.clone(),
        title: input.title.to_string(),
        from,
        to: input.to,
        picked: input.picked,
        downgrade: input.downgrade,
        language,
        dlcs: game.content.refs(),
        removing,
        dlc_files: drop.files,
        dlc_restored: drop.restored,
        from_tree: input
            .depots
            .as_ref()
            .map_or_else(|| input.tree_name.clone(), |d| d.from_tree.clone()),
        kept_dirs: drop.kept_dirs,
        target_paths: drop.target_paths,
        autosnapshot: input.autosnapshot,
        resume,
        cache_root,
        tree_name: input.tree_name,
        dropped: input.dropped.len(),
        dropped_paths: input.dropped,
        game_files: method.to_string(),
        game_dir: game.dirs.game.clone(),
        download_size: input.download_size,
        disk_size: input.disk_size,
        notes,
        manifest: input.manifest,
    })))
}

/// `A (id), B (id)`.
fn named(dlcs: &[DlcRef]) -> String {
    dlcs.iter()
        .map(|d| format!("{} ({})", d.title, d.id))
        .collect::<Vec<_>>()
        .join(", ")
}

fn titles(dlcs: &[DlcRef]) -> String {
    dlcs.iter()
        .map(|d| d.title.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The rebuild on build `to` that leaves `removing` out of `game`.
fn rebuild_without(game: &Game, to: &str, removing: &[u64]) -> String {
    let keep: String = game
        .installed()
        .iter()
        .filter(|d| !removing.contains(d))
        .map(|d| format!(" --dlc {d}"))
        .collect();
    let flags = if keep.is_empty() {
        " --no-dlcs".to_string()
    } else {
        keep
    };
    format!("gogo linux-app {} --build {to}{flags} --yes", game.gogid)
}

/// What taking out the DLC the target lacks changes (§11h).
#[derive(Default)]
struct DlcDrop {
    files: Vec<Expect>,
    restored: Vec<Expect>,
    kept_dirs: HashSet<String>,
    target_paths: HashSet<String>,
}

/// The reverse diff of `removing` across the two builds, and the shared paths
/// whose copy in `game/` isn't the target's version yet. Refused when a path the
/// target lays isn't a plain one inside the game. Hashes only the shared paths.
fn plan_drop(
    game: &Game,
    d: &DepotLists<'_>,
    tree_name: &str,
    to: &str,
    removing: &[u64],
) -> Result<DlcDrop, Refusal> {
    let diff = dlc_remove::reverse_diff_to(
        (d.from, &d.from_tree),
        (d.to, tree_name),
        &game.gogid,
        &game.installed(),
        removing,
    );
    let mut restored = Vec::new();
    for e in diff.shared {
        let rel = dlc_remove::game_path(tree_name, &e.path);
        let Some(rel) = rel.filter(|r| lay::plain(&e.path) && lay::plain(r)) else {
            return Err(refuse(format!(
                "build {to}'s manifest names {}, which isn't a plain path inside the game, so \
                 nothing was changed; a rebuild without the DLC: {}",
                e.path,
                rebuild_without(game, to, removing)
            )));
        };
        if dlc_remove::find_ci(&game.dirs.game, &rel).is_some_and(|at| dlc_remove::holds(&at, &e)) {
            continue;
        }
        restored.push(e);
    }
    Ok(DlcDrop {
        files: diff.removed,
        restored,
        kept_dirs: dlc_remove::kept_dirs(tree_name, &diff.kept),
        target_paths: diff
            .kept
            .iter()
            .filter_map(|p| dlc_remove::game_path(tree_name, p))
            .map(|p| p.to_lowercase())
            .collect(),
    })
}

impl UpdatePlan {
    /// The `auto` snapshot's label: `before update A → B`, with `, removing X` when
    /// the update takes DLC out.
    pub fn snapshot_label(&self) -> String {
        let base = crate::snapshot::labels::update(&self.from.label(), &self.to.label());
        if self.removing.is_empty() {
            base
        } else {
            format!("{base}, removing {}", titles(&self.removing))
        }
    }

    /// The DLC that stay.
    pub fn keeping(&self) -> Vec<DlcRef> {
        self.dlcs
            .iter()
            .filter(|d| !self.removing.iter().any(|r| r.id == d.id))
            .cloned()
            .collect()
    }

    /// The warning about saves when the update takes DLC out (design 19 §3.7's,
    /// with the way back an update has: the snapshot, or a rebuild on the
    /// installed build with the DLC). `None` when it takes none out.
    pub fn saves_warning(&self) -> Option<String> {
        if self.removing.is_empty() {
            return None;
        }
        let names = titles(&self.removing);
        let snapshot = if self.autosnapshot {
            format!(
                "the snapshot \"{}\" can restore the game, or ",
                self.snapshot_label()
            )
        } else {
            String::new()
        };
        let dlcs: String = self
            .dlcs
            .iter()
            .map(|d| format!(" --dlc {}", d.id))
            .collect();
        Some(format!(
            "saves made with {names} may not load without it; {snapshot}a rebuild on build \
             {} brings it back: gogo linux-app {} --build {}{dlcs} --yes",
            self.from.label(),
            self.gogid,
            self.from.build_id
        ))
    }
}

/// The plan's lines (human).
pub fn plan_lines(p: &UpdatePlan) -> Vec<String> {
    let size = crate::fmt::human_size;
    let mut out = vec![format!(
        "update {} ({}): {} → {} (build {} → {}){}",
        p.title,
        p.gogid,
        p.from.label(),
        p.to.label(),
        p.from.build_id,
        p.to.build_id,
        if p.picked {
            ", picked with --build"
        } else {
            ""
        }
    )];
    if let Some(b) = &p.to.branch {
        out.push(format!("on branch {b}"));
    }
    if p.downgrade {
        out.push(format!(
            "downgrade from {}: saves made by a newer version may not load",
            p.from.label()
        ));
    }
    if p.resume {
        out.push("resuming: an earlier run stopped during its download".into());
    }
    let keeping = p.keeping();
    out.push(format!(
        "language: {}; DLC kept: {}",
        p.language,
        if keeping.is_empty() {
            "none".to_string()
        } else {
            titles(&keeping)
        }
    ));
    if !p.removing.is_empty() {
        let bytes: u64 = p.dlc_files.iter().filter_map(|e| e.size).sum();
        out.push(format!(
            "DLC removed: {} (build {} doesn't have it): {} file(s) out ({}){}",
            titles(&p.removing),
            p.to.build_id,
            p.dlc_files.len(),
            size(bytes),
            if p.dlc_restored.is_empty() {
                String::new()
            } else {
                format!(
                    ", {} file(s) it replaced laid again in build {}'s version",
                    p.dlc_restored.len(),
                    p.to.build_id
                )
            }
        ));
        out.push(
            "depot cache: switched to the target without it (its files there are deleted)".into(),
        );
    }
    out.push(format!(
        "files from: the depot cache ({}), switched to build {} by its changed files where \
         it can",
        p.cache_root.display(),
        p.to.build_id
    ));
    out.push(format!(
        "game files: changed and new files {} into {}{}; the prefix, saves and settings \
         stay",
        if p.game_files == "copy" {
            "copied (no reflink from the cache)"
        } else {
            "reflinked"
        },
        p.game_dir.display(),
        if p.dropped > 0 {
            format!(", {} file(s) this build dropped removed", p.dropped)
        } else {
            String::new()
        }
    ));
    out.push(format!(
        "download: up to {} (the whole build; only the changed files are fetched), disk {}",
        size(p.download_size),
        size(p.disk_size)
    ));
    for n in &p.notes {
        out.push(format!("note: {n}"));
    }
    if let Some(w) = p.saves_warning() {
        out.push(format!("saves: {w}"));
    }
    out
}

// — the update —

/// How to run an update.
#[derive(Debug, Clone)]
pub struct UpdateOptions {
    pub reflink: Reflink,
    /// The `auto` snapshot before the update (design 20 §7.1, §9.5).
    pub snapshots: super::snapshots::AutoOptions,
    /// Run setup after the commit (GOG's install scripts).
    pub run_setup: bool,
    /// Crash injection: stop at this lay step.
    #[doc(hidden)]
    pub fail_at: Option<lay::Step>,
}

impl Default for UpdateOptions {
    fn default() -> Self {
        Self {
            reflink: Reflink::Try,
            snapshots: super::snapshots::AutoOptions::default(),
            run_setup: true,
            fail_at: None,
        }
    }
}

/// What an update did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateOutcome {
    pub gogid: String,
    pub from: BuildSide,
    pub to: BuildSide,
    pub dlcs: Vec<DlcRef>,
    /// DLC the update took out (the target lacks them).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dlcs_removed: Vec<DlcRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovered: Option<Direction>,
    /// Files laid onto `game/`.
    pub laid: u64,
    /// Files taken out of `game/`.
    pub removed: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lay: Option<LayReport>,
    pub snapshot: Option<super::snapshots::SnapshotRef>,
    pub pruned: Vec<super::snapshots::SnapshotRef>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// `update-download.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadRecord {
    pub from: String,
    pub to: BuildSide,
    pub source: PathBuf,
    pub tree_name: String,
    /// The cache before the first try.
    pub before: Snapshot,
    /// The files the target drops, from the depot manifests.
    pub dropped: Vec<DroppedFile>,
    /// The download finished: a rerun lays without fetching again.
    pub fetched: bool,
    /// The DLC the update takes out (§11h).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removing: Vec<u64>,
}

/// The pending update's download record in game dir `root`.
pub fn read_download(root: &Path) -> Option<DownloadRecord> {
    serde_json::from_str(&std::fs::read_to_string(root.join(DOWNLOAD)).ok()?).ok()
}

/// Did an update stop before its lay (the game is untouched)?
pub fn download_pending(root: &Path) -> bool {
    root.join(DOWNLOAD).exists()
}

fn stale_note(p: &DownloadRecord, installed: &str) -> String {
    format!(
        "an earlier update's download record (build {} to {}) is stale: the game is on \
         build {installed} now, so it is discarded",
        p.from, p.to.build_id
    )
}

/// Remove a download record (and the staged manifest) that another installed build
/// left (a snapshot rollback since), so the update plans afresh. What it said, when
/// there was one.
pub fn discard_stale(root: &Path, installed: &str) -> Option<String> {
    let p = read_download(root).filter(|p| p.from != installed)?;
    let _ = std::fs::remove_file(root.join(DOWNLOAD));
    let _ = std::fs::remove_file(root.join(NEXT_MANIFEST));
    Some(stale_note(&p, installed))
}

/// Why an update's journal can't go forward: its source, the depot cache, isn't
/// on the journal's target build any more. `None` when it is.
pub(crate) fn cache_moved(j: &Journal) -> Option<String> {
    let to = j.to.as_deref().unwrap_or(&j.build_id);
    match depot::read_record(&j.source) {
        // A file laid again in place of a removed DLC's must still be the version
        // the journal recorded.
        Some(r) if r.build_id == to => dlc_remove::sources_changed(j),
        r => Some(format!(
            "the depot cache is on {} now, not build {to}",
            r.map_or("no recorded build".into(), |r| format!(
                "build {}",
                r.build_id
            ))
        )),
    }
}

/// The commit (§11g step 7): the target's manifest into place, then `content.toml`
/// on build `to`, each DLC row with it, without the rows of `removing` (§11h).
fn commit(game: &mut Game, to: &BuildSide, removing: &[AddingDlc]) -> Result<(), AddError> {
    let root = game.dirs.root.clone();
    let next = root.join(NEXT_MANIFEST);
    let kept = build_manifest::path(&root);
    if next.exists() {
        std::fs::rename(&next, &kept)?;
    } else if build_manifest::load(&root).is_some_and(|s| s.build_id != to.build_id) {
        // An older build's manifest describes nothing the game holds now.
        std::fs::remove_file(&kept)?;
    }
    let mut c = game.content.clone();
    c.build_id = Some(to.build_id.clone());
    c.version = to.version.clone().filter(|v| !v.trim().is_empty());
    c.branch = to.branch.clone();
    c.dlc.retain(|d| !removing.iter().any(|r| r.id == d.id));
    for d in &mut c.dlc {
        d.build_id = Some(to.build_id.clone());
        d.version = c.version.clone();
    }
    content::write(&root, &c)?;
    game.content = c;
    game.derived = false;
    Ok(())
}

/// Settle an update's journal after [`lay::recover`] went `dir` (the caller holds
/// the game's lock): forward commits it, back leaves the game on its old build.
/// Either way the download record and the staged manifest go.
pub(crate) fn settle(
    game: &mut Game,
    j: &Journal,
    dir: Direction,
    moved: Option<String>,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<(), AddError> {
    let root = game.dirs.root.clone();
    let to_id = j.to.clone().unwrap_or_else(|| j.build_id.clone());
    let message = match dir {
        Direction::Forward => {
            let to = read_download(&root)
                .map(|r| r.to)
                .filter(|t| t.build_id == to_id)
                .unwrap_or(BuildSide {
                    build_id: to_id.clone(),
                    version: None,
                    branch: None,
                    generation: None,
                });
            commit(game, &to, &j.removing)?;
            lay::finish(&root)?;
            dlc_remove::prune_dirs(&game.dirs.game, &j.prune);
            format!(
                "an earlier update to build {to_id}{} had stopped while laying its files: \
                 finished it",
                removing_note(&j.removing)
            )
        }
        Direction::Back => {
            let _ = std::fs::remove_file(root.join(NEXT_MANIFEST));
            format!(
                "an earlier update to build {to_id}{} had stopped and {}: undid it, the game \
                 is on build {} as it was",
                removing_note(&j.removing),
                moved.unwrap_or_else(|| "the depot cache lost files it needed".into()),
                j.from.as_deref().unwrap_or("?")
            )
        }
    };
    let _ = std::fs::remove_file(root.join(DOWNLOAD));
    emit(&ProgressEvent::Warn { message });
    Ok(())
}

/// `, removing X` for a journal that takes DLC out.
fn removing_note(removing: &[AddingDlc]) -> String {
    if removing.is_empty() {
        String::new()
    } else {
        format!(
            ", removing {}",
            removing
                .iter()
                .map(|d| d.title.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// The game's entry point after the update, when it isn't the exe
/// `gogo-launch.toml` starts: a rebuild rewrites the launch file (§11g step 9).
fn entry_warning(game: &Game) -> Option<String> {
    let (t, _) = game::read_launch_toml(&game.dirs).ok()?;
    let entry = crate::macapp::info::find_entry_point(&game.dirs.game).ok()?;
    let rel = entry.strip_prefix(&game.dirs.game).ok()?;
    let rel: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let rel = rel.join("\\");
    let exe = t.exe.replace('/', "\\").to_lowercase();
    if rel.is_empty() || exe.ends_with(&format!("\\{}", rel.to_lowercase())) {
        return None;
    }
    Some(format!(
        "the new build starts {rel}, but the game's launch file still starts {}; a rebuild \
         rewrites it: gogo linux-app {} --yes",
        t.exe, game.gogid
    ))
}

fn changed(gogid: &str, why: &str) -> AddError {
    AddError::Game(MacAppError::Apps(format!(
        "{gogid} changed since the plan ({why}): run the update again"
    )))
}

/// What differs between the game as read under the lock and the plan: its DLC,
/// language or dirs. `None` when nothing does.
fn drift(game: &Game, plan: &UpdatePlan) -> Option<String> {
    let mut now = game.installed();
    let mut planned: Vec<u64> = plan.dlcs.iter().map(|d| d.id).collect();
    now.sort_unstable();
    planned.sort_unstable();
    if now != planned {
        return Some("its DLC are not the ones planned".into());
    }
    let language = language_of(game);
    if language != plan.language {
        return Some(format!("its language is now {language}"));
    }
    if game.dirs.game != plan.game_dir {
        return Some(format!("it is at {} now", game.dirs.root.display()));
    }
    None
}

/// Whether `game/<rel>` may come out: no dir on its way is a symlink (that would
/// reach outside `game/`), and it still is the installed build's file by the MD5,
/// else the size, that build gives it. `Err` says why it stays.
pub(crate) fn removable(
    game: &Path,
    rel: &str,
    size: Option<u64>,
    md5: Option<&str>,
) -> Result<(), String> {
    if let Some(link) = lay::symlinked_parent(game, rel) {
        return Err(format!("a dir on its path ({link}) is a symlink"));
    }
    let path = game.join(rel);
    let meta = path.symlink_metadata().map_err(|e| e.to_string())?;
    match (md5, size) {
        (Some(want), _) if meta.is_file() => {
            use md5::Digest;
            let mut h = md5::Md5::new();
            let mut f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
            let mut buf = vec![0u8; 1 << 16];
            loop {
                let n = std::io::Read::read(&mut f, &mut buf).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            if crate::fmt::md5_hex(h).eq_ignore_ascii_case(want) {
                Ok(())
            } else {
                Err("changed since install".into())
            }
        }
        (_, Some(n)) if meta.len() == n => Ok(()),
        (_, Some(_)) => Err("changed since install".into()),
        (_, None) => Err("the installed build gives no size or hash to check it by".into()),
    }
}

fn interrupted_lay(gogid: &str) -> AddError {
    AddError::Interrupted(
        "lay",
        format!("the game won't start until `gogo linux-app update {gogid}` finishes or undoes it"),
    )
}

/// Update `game` as `plan` says (§11g). `fetch` switches the depot cache to the
/// target (the caller's `depot::run`); `stop` says the user asked to stop (Ctrl-C).
pub async fn update<F>(
    game: &mut Game,
    plan: &UpdatePlan,
    fetch: F,
    opts: &UpdateOptions,
    emit: &mut dyn FnMut(&ProgressEvent),
    stop: &dyn Fn() -> bool,
) -> Result<UpdateOutcome, AddError>
where
    F: AsyncFnOnce() -> Result<(), DepotError>,
{
    let root = game.dirs.root.clone();
    let gogid = game.gogid.clone();
    // A snapshot rollback that stopped is settled first (design 20 §5.3.1).
    for message in super::rollback::settle_pending(&gogid, &opts.snapshots.backends)? {
        emit(&ProgressEvent::Warn { message });
    }
    let lock = crate::macapp::relocate::lock_game(&gogid)?;
    game::refuse_running(&gogid, &root, &game.dirs.prefix)?;
    let recovered = dlc::recover_locked(game, opts.reflink, emit)?;
    // The plan was made before the lock, across the network and a question: the
    // game is read again, and a change since stops the update (§11g step 2).
    *game = game.reload()?;
    let mut warnings = Vec::new();
    let outcome = |game: &Game,
                   recovered,
                   laid,
                   removed,
                   lay,
                   taken: Option<super::snapshots::Taken>,
                   warnings| UpdateOutcome {
        gogid: game.gogid.clone(),
        from: plan.from.clone(),
        to: plan.to.clone(),
        dlcs: game.content.refs(),
        // Set by the run that takes them out; a recovery that finished it says so
        // itself.
        dlcs_removed: Vec::new(),
        recovered,
        laid,
        removed,
        lay,
        snapshot: taken.as_ref().map(super::snapshots::Taken::snapshot),
        pruned: taken
            .as_ref()
            .map(super::snapshots::Taken::pruned)
            .unwrap_or_default(),
        warnings,
    };
    match installed(game) {
        Some(b) if b == plan.from.build_id => {}
        // A recovery finished this very update.
        Some(b) if b == plan.to.build_id => {
            return Ok(outcome(game, recovered, 0, 0, None, None, warnings));
        }
        _ => {
            return Err(changed(
                &gogid,
                &format!(
                    "its build is now {}",
                    game.content.build_id.as_deref().unwrap_or("unknown")
                ),
            ));
        }
    }
    if let Some(why) = drift(game, plan) {
        return Err(changed(&gogid, &why));
    }
    if let Some(message) = discard_stale(&root, &plan.from.build_id) {
        emit(&ProgressEvent::Warn { message });
    }
    let cache = plan.cache_root.clone();
    let prior = read_download(&root).filter(|r| {
        r.from == plan.from.build_id && r.to.build_id == plan.to.build_id && r.source == cache
    });
    let mut taken = None;
    let mut rec = match prior {
        Some(r) => r,
        None => {
            if dlc::download_pending(&root) {
                let (what, cmd) = dlc::pending_kind(&root);
                return Err(AddError::Game(MacAppError::Apps(format!(
                    "a DLC {what} of {gogid} stopped during its download: finish it first with \
                     `gogo linux-app dlc {gogid} {cmd}`"
                ))));
            }
            if let Some(why) =
                cache_problem(game, &plan.from.build_id, &plan.language, &plan.tree_name)
            {
                return Err(AddError::Game(MacAppError::Apps(
                    no_cache(&gogid, &plan.to.build_id, &why).message,
                )));
            }
            // The `auto` snapshot (design 20 §7.1, §9.5): after the recovery and the
            // refusals, before the first write. A failure stops the update.
            taken = super::snapshots::protect(
                &gogid,
                &game.dirs,
                Some(&lock),
                crate::snapshot::Trigger::Update,
                plan.snapshot_label(),
                &opts.snapshots,
                emit,
            )?;
            match &plan.manifest {
                Some((id, generation, m)) if *id == plan.to.build_id => {
                    build_manifest::write_named(&root, NEXT_MANIFEST, id, *generation, m)?;
                }
                _ => {
                    let _ = std::fs::remove_file(root.join(NEXT_MANIFEST));
                }
            }
            let r = DownloadRecord {
                from: plan.from.build_id.clone(),
                to: plan.to.clone(),
                source: cache.clone(),
                tree_name: plan.tree_name.clone(),
                before: Snapshot::take(&cache),
                dropped: plan.dropped_paths.clone(),
                fetched: false,
                removing: plan.removing.iter().map(|d| d.id).collect(),
            };
            lay::write_json_durably(&root.join(DOWNLOAD), &r)?;
            r
        }
    };
    if !rec.fetched {
        match fetch().await {
            Ok(()) => {}
            Err(_) if stop() => {
                return Err(AddError::Interrupted(
                    "download",
                    format!("the game is as it was; `gogo linux-app update {gogid}` resumes it"),
                ));
            }
            Err(e) => return Err(e.into()),
        }
        let on = depot::read_record(&cache).map(|r| r.build_id);
        if on.as_deref() != Some(plan.to.build_id.as_str()) {
            return Err(AddError::Game(MacAppError::Apps(format!(
                "the depot cache isn't on build {} after the download (its record says {}); \
                 the game is as it was, and `gogo linux-app update {gogid}` tries again",
                plan.to.build_id,
                on.as_deref().unwrap_or("nothing")
            ))));
        }
        rec.fetched = true;
        lay::write_json_durably(&root.join(DOWNLOAD), &rec)?;
    } else {
        // A resumed lay diffs the cache as it is now: another command may have
        // moved it since the download. Then the record can't be trusted, and the
        // update starts over.
        let on = depot::read_record(&cache).map(|r| r.build_id);
        if on.as_deref() != Some(plan.to.build_id.as_str()) {
            let _ = std::fs::remove_file(root.join(DOWNLOAD));
            let _ = std::fs::remove_file(root.join(NEXT_MANIFEST));
            return Err(AddError::Game(MacAppError::Apps(format!(
                "the depot cache moved to {} since this update's download, so its files \
                 can't be laid; the game is on build {} as it was, and `gogo linux-app \
                 update {gogid}` plans it again",
                on.map_or("no recorded build".into(), |b| format!("build {b}")),
                plan.from.build_id
            ))));
        }
    }

    // What the switch wrote goes onto `game/`; what it deleted, and what the
    // target drops by its manifests, comes out of it.
    let now = Snapshot::take(&cache);
    let changed = now.changed_since(&rec.before);
    // What to check each one by: the manifest's size and MD5 for a dropped file,
    // the size the cache had before for one the switch deleted.
    let mut expect: HashMap<String, (Option<u64>, Option<String>)> = Default::default();
    for path in now.gone_since(&rec.before) {
        expect.insert(path.clone(), (rec.before.size_of(&path), None));
    }
    for d in &rec.dropped {
        expect.insert(d.path.clone(), (d.size, d.md5.clone()));
    }
    let mut by_game: HashMap<String, (Option<u64>, Option<String>)> = Default::default();
    for (path, e) in &expect {
        for f in dlc::lay_list(&cache, &rec.tree_name, std::slice::from_ref(path)) {
            by_game.insert(f.to.to_lowercase(), e.clone());
        }
    }
    let mut files = dlc::lay_list(&cache, &rec.tree_name, &changed);
    // A path a removed DLC shared with a product that stays (§11h): the target's
    // version from the switched cache, unless the switch's own files lay it. Each
    // source is checked by its MD5 (else size) before the lay.
    let mut restore: HashMap<PathBuf, Expect> = HashMap::new();
    for e in &plan.dlc_restored {
        for f in dlc::lay_list(&cache, &rec.tree_name, std::slice::from_ref(&e.path)) {
            match files.iter().find(|l| l.to.eq_ignore_ascii_case(&f.to)) {
                Some(l) if l.from != f.from => {}
                Some(_) => {
                    restore.insert(f.from.clone(), e.clone());
                }
                None => {
                    restore.insert(f.from.clone(), e.clone());
                    files.push(f);
                }
            }
        }
    }
    let mut gone: Vec<String> = expect.into_keys().collect();
    gone.sort();
    let mut removed: Vec<String> = dlc::lay_list(&cache, &rec.tree_name, &gone)
        .into_iter()
        .map(|f| f.to)
        .collect();
    // The removed DLC's own files, checked by what the installed build gives them,
    // and their `goggame-<id>.*` by name (a record-only DLC's, no depot lists them).
    let ids: Vec<u64> = plan.removing.iter().map(|d| d.id).collect();
    let mut theirs: HashSet<String> = HashSet::new();
    for e in &plan.dlc_files {
        if let Some(rel) = dlc_remove::game_path(&plan.from_tree, &e.path) {
            let lower = rel.to_lowercase();
            by_game.insert(lower.clone(), (e.size, e.md5.clone()));
            theirs.insert(lower);
            removed.push(rel);
        }
    }
    let mut by_name: HashSet<String> = HashSet::new();
    if !ids.is_empty() {
        for name in dlc_remove::goggame_files(&game.dirs.game, &ids, &theirs) {
            by_name.insert(name.to_lowercase());
            removed.push(name);
        }
    }
    // Never a path the target lays for a product that stays.
    removed.retain(|r| !plan.target_paths.contains(&r.to_lowercase()));
    let mut j = Journal {
        gogid: gogid.clone(),
        lane: "depot".into(),
        build_id: plan.to.build_id.clone(),
        language: plan.language.clone(),
        adding: Vec::new(),
        source: cache.clone(),
        source_is_cache: false,
        moves: false,
        files,
        replaced: Vec::new(),
        dirs: Vec::new(),
        kind: Some(lay::KIND_UPDATE.into()),
        from: Some(plan.from.build_id.clone()),
        to: Some(plan.to.build_id.clone()),
        removed,
        removing: plan
            .removing
            .iter()
            .map(|d| AddingDlc {
                id: d.id,
                title: d.title.clone(),
            })
            .collect(),
        md5s: Default::default(),
        prune: Vec::new(),
    };
    j.survey(&game.dirs.game);
    j.md5s = j
        .files
        .iter()
        .filter_map(|f| Some((f.to.clone(), restore.get(&f.from)?.md5.clone()?)))
        .collect();
    // A dropped file is only taken out while it is the installed build's: one the
    // user or the game changed stays, and so does one behind a symlinked dir.
    let names = titles(&plan.removing);
    let mut kept = Vec::new();
    j.removed.retain(|r| {
        let lower = r.to_lowercase();
        let ok = if by_name.contains(&lower) {
            match lay::symlinked_parent(&game.dirs.game, r) {
                Some(link) => Err(format!("a dir on its path ({link}) is a symlink")),
                None => Ok(()),
            }
        } else {
            let (size, md5) = by_game.get(&lower).cloned().unwrap_or_default();
            removable(&game.dirs.game, r, size, md5.as_deref())
        };
        match ok {
            Ok(()) => true,
            Err(why) if theirs.contains(&lower) || by_name.contains(&lower) => {
                kept.push(format!("kept {r}, which {names} laid: {why}"));
                false
            }
            Err(why) => {
                kept.push(format!(
                    "kept {r}, which build {} dropped: {why}",
                    plan.to.build_id
                ));
                false
            }
        }
    });
    // Reported once, with the outcome's warnings.
    warnings.extend(kept);
    // Dirs the removed DLC's files leave empty go after the commit, unless a
    // product that stays lists a file under them.
    let dlc_removed: Vec<String> = j
        .removed
        .iter()
        .filter(|r| {
            let l = r.to_lowercase();
            theirs.contains(&l) || by_name.contains(&l)
        })
        .cloned()
        .collect();
    j.prune = dlc_remove::prune_list(&dlc_removed, &plan.kept_dirs);
    // A laid file behind a symlinked dir refuses the update, before any write.
    j.check_links(&game.dirs.game)?;
    // A file laid again in place of a removed DLC's must be the target's version
    // in the switched cache; else nothing is laid, and the update stops naming the
    // rebuild (the cache is on the target already, so it can't be retried).
    if let Some(f) = j.files.iter().find(|f| {
        restore
            .get(&f.from)
            .is_some_and(|e| !dlc_remove::holds(&f.from, e))
    }) {
        let _ = std::fs::remove_file(root.join(DOWNLOAD));
        let _ = std::fs::remove_file(root.join(NEXT_MANIFEST));
        return Err(AddError::Game(MacAppError::Apps(format!(
            "the depot cache's copy of {} isn't build {}'s version without {names}, so \
             nothing was laid and the game is on build {} as it was; update with a rebuild \
             instead: {}",
            f.to,
            plan.to.build_id,
            plan.from.build_id,
            rebuild_without(game, &plan.to.build_id, &ids)
        ))));
    }
    let mut lay_report = None;
    if !j.files.is_empty() || !j.removed.is_empty() {
        let bytes: u64 = j
            .files
            .iter()
            .filter_map(|f| f.from.symlink_metadata().ok())
            .map(|m| m.len())
            .sum();
        emit(&ProgressEvent::GameFilesStarted {
            method: plan.game_files.clone(),
            bytes,
            dir: game.dirs.game.clone(),
            kind: Some("update".into()),
            files: Some(j.files.len() as u64),
        });
        let mut tenths = materialize::Tenths::new(bytes);
        let copying = plan.game_files == "copy";
        let files_total = j.files.len() as u64;
        let (mut files_done, mut bytes_done) = (0u64, 0u64);
        let report = lay::lay_files(
            &root,
            &j,
            opts.reflink,
            &mut |s| opts.fail_at == Some(s) || stop(),
            &mut |n| {
                files_done += 1;
                bytes_done += n;
                emit(&ProgressEvent::WorkProgress {
                    step: crate::engine::events::step::GAME_FILES.into(),
                    files: Some(files_done),
                    files_total: Some(files_total),
                    bytes: Some(bytes_done),
                    bytes_total: Some(bytes),
                });
                if copying && let Some(copied) = tenths.add(n) {
                    emit(&ProgressEvent::GameFilesProgress {
                        copied,
                        total: bytes,
                    });
                }
            },
        );
        emit(&ProgressEvent::WorkDone {
            step: crate::engine::events::step::GAME_FILES.into(),
        });
        lay_report = Some(report.map_err(|e| match e {
            lay::LayError::Stopped => interrupted_lay(&gogid),
            e @ lay::LayError::Symlink { .. } => AddError::Game(MacAppError::Apps(e.to_string())),
            lay::LayError::Io(e) => AddError::Io(e),
        })?);
    }
    commit(game, &rec.to, &j.removing)?;
    lay::finish(&root)?;
    dlc_remove::prune_dirs(&game.dirs.game, &j.prune);
    let _ = std::fs::remove_file(root.join(DOWNLOAD));

    if opts.run_setup
        && let Err(e) = super::setup::setup(
            &gogid,
            &super::setup::SetupOptions {
                refresh: false,
                prompt: None,
                // The update's own snapshot came first.
                runtime_snapshot: None,
            },
            emit,
        )
    {
        warnings.push(format!(
            "the update is in place, but setup (GOG's install scripts) failed: {e}; run \
             `gogo linux-app setup {gogid}`"
        ));
    }
    warnings.extend(entry_warning(game));
    let (laid, removed) = (j.files.len() as u64, j.removed.len() as u64);
    let mut out = outcome(game, recovered, laid, removed, lay_report, taken, warnings);
    out.dlcs_removed = plan.removing.clone();
    Ok(out)
}
