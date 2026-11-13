//! `gogo linux-app dlc <gogid> remove` on the depot lane (design 19 §3.5, §3.7).
//!
//! The reverse of an add, worked out from the installed build's depot manifests
//! (gog-warp keeps its own diff report crate-private, so gogo reads the same public
//! manifests itself, [`crate::gog::dlc_filter::fetch_depots`]):
//!
//! - a path only the removed DLC list is **taken out** of `game/`, and only while it
//!   still is the file the DLC laid (its MD5, else its size; [`super::update`]'s
//!   check). A changed one stays, with a warning;
//! - a path the DLC shares with the base game or a kept DLC is **restored** to the
//!   version a build without the DLC gives (the first kept depot that lists it, as
//!   gog-warp writes a shared path once, from the first depot). Nothing happens when
//!   the game's file already is that version. Otherwise it comes from the depot
//!   cache when the cache's copy has the right MD5, else from a targeted download
//!   (the installed build against itself, without the DLC) into
//!   `games/<gogid>/.dlc-staging/`, checked by MD5 before anything is laid.
//!
//! Then the same journaled lay as an add or an update ([`crate::gog::lay`],
//! `kind = "remove"`), the commit (`content.toml` without the DLC), and setup once.
//! The DLC stays in the depot cache, whose record isn't touched: a later `dlc add`
//! lays it from there with no download (#381).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::engine::events::ProgressEvent;
use crate::gog::content::{self, Content, DlcRef};
use crate::gog::depot::{self, DepotError, DownloadInputs};
use crate::gog::dlc_filter::DepotList;
use crate::gog::lay::{self, AddingDlc, Direction, Journal, LayFile, LayReport};
use crate::gog::materialize::{self, TreeMethod};
use crate::macapp::error::MacAppError;

use super::dlc::{self, AddError, AddOptions, Game};
use super::game;

/// A file as the build's depot manifests give it: its path relative to the depot
/// cache root (`<install dir>/…`, or a support file under `gog-support/`), with the
/// size and MD5 the manifest gives, when it does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Expect {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
}

/// What removing DLC changes, from the manifests alone ([`reverse_diff`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReverseDiff {
    /// Paths only the removed DLC list, with what the installed build laid there.
    pub removed: Vec<Expect>,
    /// Paths a removed DLC shares with a kept product, with the version a build
    /// without the removed DLC lays there.
    pub shared: Vec<Expect>,
    /// Every path a kept product lists (as its first depot spells it).
    pub kept: Vec<String>,
}

/// Pure: the reverse diff of removing `removing` from a game of `base` with
/// `installed` DLC, from the build's `depots` in the game's language (manifest
/// order), whose install directory is `tree_name`.
///
/// Every entry is keyed by the path it lands on in `game/` (the tree's paths keep
/// theirs, a support overlay's `app/` goes to the game root; anything else, such as
/// GOG's support installers, never reaches `game/` and is left out), lowercased:
/// gog-warp compares paths without case, and a DLC's overlay file can land on a
/// base tree file's path. On one game path a support overlay's entry wins over a
/// tree's, as at a build and a lay; otherwise the first depot's entry counts, since
/// gog-warp writes a path several depots list once, from the first depot that
/// lists it.
pub fn reverse_diff(
    depots: &[DepotList],
    base: &str,
    tree_name: &str,
    installed: &[u64],
    removing: &[u64],
) -> ReverseDiff {
    let gone: HashSet<String> = removing.iter().map(u64::to_string).collect();
    let all: HashSet<String> = installed
        .iter()
        .map(u64::to_string)
        .chain([base.to_string()])
        .collect();
    let keep: HashSet<String> = all.difference(&gone).cloned().collect();
    let entry = |d: &DepotList, p: &String| Expect {
        path: p.clone(),
        size: d.sizes.get(p).copied(),
        md5: d.md5s.get(p).map(|m| m.to_lowercase()),
    };
    let overlay = |e: &Expect| e.path.starts_with("gog-support/");
    // The entry that lays a game path: an overlay's over a tree's, else the first.
    let put = |map: &mut BTreeMap<String, Expect>, key: String, e: Expect| match map.get(&key) {
        Some(cur) if overlay(cur) || !overlay(&e) => {}
        _ => {
            map.insert(key, e);
        }
    };
    // Lowercased game path → its entry, over every installed product and over the
    // kept ones; and the game paths the removed DLC lay.
    let mut now: BTreeMap<String, Expect> = BTreeMap::new();
    let mut after: BTreeMap<String, Expect> = BTreeMap::new();
    let mut theirs: HashSet<String> = HashSet::new();
    for d in depots {
        if !all.contains(&d.product) {
            continue;
        }
        for p in &d.paths {
            let Some(rel) = game_path(tree_name, p) else {
                continue;
            };
            let key = rel.to_lowercase();
            put(&mut now, key.clone(), entry(d, p));
            if keep.contains(&d.product) {
                put(&mut after, key, entry(d, p));
            } else {
                theirs.insert(key);
            }
        }
    }
    let mut out = ReverseDiff {
        kept: after.values().map(|e| e.path.clone()).collect(),
        ..Default::default()
    };
    let mut theirs: Vec<String> = theirs.into_iter().collect();
    theirs.sort();
    for key in theirs {
        match after.get(&key) {
            Some(e) => out.shared.push(e.clone()),
            None => out.removed.extend(now.get(&key).cloned()),
        }
    }
    out
}

/// The MD5 of the file at `path` (lowercase hex).
fn md5_of(path: &Path) -> std::io::Result<String> {
    use md5::Digest;
    let mut h = md5::Md5::new();
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = std::io::Read::read(&mut f, &mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(crate::fmt::md5_hex(h))
}

/// Is the file at `path` the one `e` describes: its MD5 when the manifest gives one,
/// else its size. With neither, or no file, it isn't.
fn holds(path: &Path, e: &Expect) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    match (&e.md5, e.size) {
        (Some(want), _) => md5_of(path).is_ok_and(|m| m.eq_ignore_ascii_case(want)),
        (None, Some(n)) => meta.len() == n,
        (None, None) => false,
    }
}

/// `rel` under `game` as spelled on disk: each component is the entry of that
/// name, else the first one matching it without case (a DLC depot can spell a dir
/// differently from the base tree). Listing each dir, rather than trying the
/// name, gives the on-disk spelling on a case-insensitive filesystem too.
fn find_ci(game: &Path, rel: &str) -> Option<PathBuf> {
    let mut at = game.to_path_buf();
    for comp in rel.split('/').filter(|c| !c.is_empty()) {
        let lower = comp.to_lowercase();
        let names: Vec<String> = std::fs::read_dir(&at)
            .ok()?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.to_lowercase() == lower)
            .collect();
        let found = names
            .iter()
            .find(|n| n.as_str() == comp)
            .or_else(|| names.iter().min())?;
        at = at.join(found);
    }
    Some(at)
}

/// Where a restored file comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RestoreFrom {
    /// The depot cache, whose copy has the right MD5.
    Cache,
    /// A targeted download into the game's staging dir.
    Download,
}

/// A base (or kept DLC) file that comes back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Restore {
    #[serde(flatten)]
    pub file: Expect,
    pub from: RestoreFrom,
}

/// What a removal will do (`--dry-run --json` prints it).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovePlan {
    pub gogid: String,
    pub title: String,
    pub build_id: String,
    pub version: Option<String>,
    pub language: String,
    /// The DLC the game has before the removal.
    pub installed: Vec<DlcRef>,
    pub removing: Vec<DlcRef>,
    pub keeping: Vec<DlcRef>,
    pub tree_name: String,
    /// Paths (relative to the depot cache root) taken out of `game/`.
    pub removed: Vec<Expect>,
    pub removed_bytes: u64,
    /// Paths put back to the version without the DLC.
    pub restored: Vec<Restore>,
    pub restored_bytes: u64,
    /// The depot cache, when its record is the installed build and language.
    pub cache_root: Option<PathBuf>,
    /// The removed DLC are in the depot cache and stay there.
    pub cache_keeps: Vec<DlcRef>,
    /// How restored files reach `game/`: `reflink`, `copy`, `rename` or `none`.
    pub game_files: String,
    /// An `auto` snapshot will be taken first.
    pub autosnapshot: bool,
    pub game_dir: PathBuf,
    /// The targeted download's inputs, when a restored file needs one.
    #[serde(skip)]
    pub inputs: DownloadInputs,
    /// Every path a kept product lists, mapped into `game/` and lowercased: a dir
    /// on one of them is never removed when it empties.
    #[serde(skip)]
    pub kept_dirs: HashSet<String>,
    /// The cache record's DLC set when planned: a removal that restores from the
    /// cache stops when the record changed since.
    #[serde(skip)]
    pub cache_dlcs: Option<Vec<String>>,
}

impl RemovePlan {
    pub fn downloads(&self) -> bool {
        self.restored
            .iter()
            .any(|r| r.from == RestoreFrom::Download)
    }

    pub fn ids(&self) -> Vec<u64> {
        self.removing.iter().map(|d| d.id).collect()
    }
}

/// What the caller fetched for [`plan_remove`].
pub struct PlanInput<'a> {
    pub game: &'a Game,
    pub title: &'a str,
    pub version: Option<String>,
    /// The installed build's generation: a download for a restored file needs 2.
    pub generation: u32,
    /// The installed build's install directory (the tree's name in the cache).
    pub tree_name: String,
    /// The build's depots of the base and the installed DLC in the game's language.
    pub depots: &'a [DepotList],
    /// Installed DLC to remove.
    pub removing: &'a [u64],
    /// Autosnapshot is on for this run.
    pub autosnapshot: bool,
}

/// The names a removal was given, against the installed DLC.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Named {
    /// Installed DLC they name, in the order named.
    pub ids: Vec<u64>,
    /// Names that match no installed DLC (a notice: a rerun after a removal
    /// finished must succeed).
    pub unmatched: Vec<String>,
    /// Names that are part of several installed titles: (name, those titles).
    pub ambiguous: Vec<(String, Vec<String>)>,
    /// Names that are only part of one installed title, where no question will be
    /// asked to confirm it: (name, that title). A usage error.
    pub inexact: Vec<(String, String)>,
}

/// Pure: resolve `names` (product ids or titles) against the installed DLC. An id
/// or a title without case matches. A name that is part of exactly one installed
/// title matches it only with `partial` (the removal will ask before it runs);
/// otherwise it is `inexact`. Ownership doesn't matter: an installed DLC can
/// always come out.
pub fn resolve_installed(names: &[String], installed: &[DlcRef], partial: bool) -> Named {
    let mut out = Named::default();
    for n in names {
        let lower = n.trim().to_lowercase();
        let by_id = n
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|id| installed.iter().any(|d| d.id == *id));
        let exact = installed.iter().find(|d| d.title.to_lowercase() == lower);
        let parts: Vec<&DlcRef> = installed
            .iter()
            .filter(|d| !lower.is_empty() && d.title.to_lowercase().contains(&lower))
            .collect();
        let id = match (by_id.or(exact.map(|d| d.id)), parts.as_slice()) {
            (Some(id), _) => id,
            (None, [one]) if partial => one.id,
            (None, [one]) => {
                out.inexact.push((n.clone(), one.title.clone()));
                continue;
            }
            (None, []) => {
                out.unmatched.push(n.clone());
                continue;
            }
            (None, many) => {
                out.ambiguous
                    .push((n.clone(), many.iter().map(|d| d.title.clone()).collect()));
                continue;
            }
        };
        if !out.ids.contains(&id) {
            out.ids.push(id);
        }
    }
    out
}

/// The game-relative path of a cache-relative one, if it goes into `game/`.
fn game_path(tree_name: &str, path: &str) -> Option<String> {
    dlc::lay_list(Path::new(""), tree_name, &[path.to_string()])
        .into_iter()
        .next()
        .map(|f| f.to)
}

/// The rebuild that leaves `removing` out of the game: the workaround named when a
/// removal can't go on.
pub fn rebuild_hint(game: &Game, removing: &[u64]) -> String {
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
    format!(
        "gogo linux-app {}{flags} --yes (keeps the prefix and saves)",
        game.gogid
    )
}

/// Plan the removal (design 19 §3.7): the reverse diff, then for each shared path
/// whether the game's file needs restoring and where the right version comes from.
/// Reads `game/` and the cache (hashing the shared paths only); writes nothing.
/// `Err` is a refusal: a restore that needs a download on a generation 1 build.
pub fn plan_remove(input: &PlanInput<'_>) -> Result<RemovePlan, String> {
    let game = input.game;
    let language = super::update::language_of(game);
    let build_id = game.content.build_id.clone().unwrap_or_default();
    let installed = game.installed();
    let refs = game.content.refs();
    let removing: Vec<DlcRef> = refs
        .iter()
        .filter(|d| input.removing.contains(&d.id))
        .cloned()
        .collect();
    let ids: Vec<u64> = removing.iter().map(|d| d.id).collect();
    let keeping: Vec<DlcRef> = refs
        .iter()
        .filter(|d| !ids.contains(&d.id))
        .cloned()
        .collect();
    let diff = reverse_diff(
        input.depots,
        &game.gogid,
        &input.tree_name,
        &installed,
        &ids,
    );
    let cache_root = game.cache_root.clone().filter(|root| {
        depot::read_record(root).is_some_and(|r| {
            r.build_id == build_id && r.language.as_deref() == Some(language.as_str())
        })
    });
    let plan_cache_dlcs = cache_root
        .as_ref()
        .and_then(|r| depot::read_record(r))
        .and_then(|r| r.dlcs);
    let cache_keeps: Vec<DlcRef> = cache_root
        .as_ref()
        .and_then(|r| depot::read_record(r))
        .map(|r| {
            let held = r.all_dlcs();
            removing
                .iter()
                .filter(|d| held.contains(&d.id.to_string()))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let mut restored = Vec::new();
    for e in &diff.shared {
        let Some(rel) = game_path(&input.tree_name, &e.path) else {
            continue;
        };
        // Already the version without the DLC: nothing to do.
        if let Some(at) = find_ci(&game.dirs.game, &rel)
            && holds(&at, e)
        {
            continue;
        }
        let from = match &cache_root {
            Some(root) if e.md5.is_some() && holds(&root.join(&e.path), e) => RestoreFrom::Cache,
            _ => RestoreFrom::Download,
        };
        restored.push(Restore {
            file: e.clone(),
            from,
        });
    }
    let downloads = restored.iter().any(|r| r.from == RestoreFrom::Download);
    if downloads && input.generation == 1 {
        return Err(format!(
            "build {build_id} is a generation 1 build, and {} base file(s) the DLC replaced \
             would have to be downloaded, which gogo does only for generation 2; a rebuild \
             without the DLC: {}",
            restored
                .iter()
                .filter(|r| r.from == RestoreFrom::Download)
                .count(),
            rebuild_hint(game, &ids)
        ));
    }
    let restored_bytes: u64 = restored.iter().filter_map(|r| r.file.size).sum();
    let removed_bytes: u64 = diff.removed.iter().filter_map(|e| e.size).sum();
    let game_files = if restored.is_empty() {
        "none"
    } else if !restored.iter().any(|r| r.from == RestoreFrom::Cache) {
        "rename"
    } else {
        let src = cache_root.clone().unwrap_or_default();
        match materialize::plan_tree(&src, &game.dirs.game, restored_bytes).method {
            TreeMethod::Reflink => "reflink",
            TreeMethod::Copy => "copy",
        }
    };
    let mut kept_dirs = HashSet::new();
    for key in &diff.kept {
        if let Some(rel) = game_path(&input.tree_name, key) {
            let mut parts: Vec<&str> = rel.split('/').collect();
            parts.pop();
            let mut at = String::new();
            for p in parts {
                if !at.is_empty() {
                    at.push('/');
                }
                at.push_str(p);
                kept_dirs.insert(at.to_lowercase());
            }
        }
    }
    let keep_ids: Vec<String> = keeping.iter().map(|d| d.id.to_string()).collect();
    Ok(RemovePlan {
        gogid: game.gogid.clone(),
        title: input.title.to_string(),
        build_id,
        version: input.version.clone(),
        language: language.clone(),
        installed: refs,
        removing,
        keeping,
        tree_name: input.tree_name.clone(),
        removed: diff.removed,
        removed_bytes,
        restored,
        restored_bytes,
        cache_root,
        cache_keeps,
        game_files: game_files.to_string(),
        autosnapshot: input.autosnapshot,
        game_dir: game.dirs.game.clone(),
        // The installed build against itself, without the DLC: gog-warp's diff
        // then fetches exactly the shared files whose version changes.
        inputs: DownloadInputs {
            targeted: true,
            switch_from: None,
            language: Some(language.clone()),
            old_language: Some(language),
            dlcs: keep_ids,
            old_dlcs: installed.iter().map(u64::to_string).collect(),
        },
        kept_dirs,
        cache_dlcs: plan_cache_dlcs,
    })
}

fn titles(dlcs: &[DlcRef]) -> String {
    dlcs.iter()
        .map(|d| d.title.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The warning about saves (design 19 §3.7): the plan's line and the question's.
pub fn saves_warning(p: &RemovePlan) -> String {
    saves_line(&p.gogid, &p.removing, p.autosnapshot)
}

/// [`saves_warning`]'s text, for either lane: `autosnapshot` when the `auto`
/// snapshot will be taken.
pub fn saves_line(gogid: &str, removing: &[DlcRef], autosnapshot: bool) -> String {
    let names = titles(removing);
    let ids: String = removing.iter().map(|d| format!(" {}", d.id)).collect();
    let snapshot = if autosnapshot {
        format!(
            "the snapshot \"{}\" can restore the game, or ",
            crate::snapshot::labels::removing(&names)
        )
    } else {
        String::new()
    };
    format!(
        "saves made with {names} may not load without it; {snapshot}gogo linux-app dlc {gogid} \
         add{ids} brings it back"
    )
}

/// The plan's lines (no `gogo:` prefix).
pub fn plan_lines(p: &RemovePlan) -> Vec<String> {
    let size = crate::fmt::human_size;
    let mut out = vec![
        format!(
            "remove: {} from {} ({} file(s), {})",
            titles(&p.removing),
            p.title,
            p.removed.len(),
            size(p.removed_bytes)
        ),
        if p.keeping.is_empty() {
            "keep: no other DLC".to_string()
        } else {
            format!("keep: {}", titles(&p.keeping))
        },
        format!(
            "build {}{}, the installed one",
            p.build_id,
            p.version
                .as_deref()
                .map(|v| format!(" ({v})"))
                .unwrap_or_default()
        ),
    ];
    if p.restored.is_empty() {
        out.push("base files: none to restore".into());
    } else {
        let cache = p
            .restored
            .iter()
            .filter(|r| r.from == RestoreFrom::Cache)
            .count();
        let from = match (cache, p.restored.len() - cache) {
            (_, 0) => "from the depot cache".to_string(),
            (0, _) => "downloaded".to_string(),
            (c, d) => format!("{c} from the depot cache, {d} downloaded"),
        };
        out.push(format!(
            "base files: {} the DLC replaced, restored ({}), {from}",
            p.restored.len(),
            size(p.restored_bytes)
        ));
        out.push(format!("game files: {}", p.game_files));
    }
    if !p.cache_keeps.is_empty() {
        out.push(format!(
            "depot cache: {} stays in it, so a later dlc add takes it from there (no download)",
            titles(&p.cache_keeps)
        ));
    }
    out.push(format!("saves: {}", saves_warning(p)));
    out
}

/// What a removal did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveOutcome {
    pub gogid: String,
    pub build_id: Option<String>,
    pub language: Option<String>,
    /// Every DLC the game has now.
    pub dlcs: Vec<DlcRef>,
    pub removed: Vec<DlcRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovered: Option<Direction>,
    /// Files taken out of `game/`.
    pub files_removed: u64,
    /// Files put back to the version without the DLC.
    pub files_restored: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lay: Option<LayReport>,
    pub snapshot: Option<super::snapshots::SnapshotRef>,
    pub pruned: Vec<super::snapshots::SnapshotRef>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// A removal's download record, in the add's file ([`dlc::DOWNLOAD`]), so every
/// "a DLC change is pending" refusal (a move, a snapshot) applies to it too.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    build_id: String,
    source: PathBuf,
    removing: Vec<u64>,
}

/// Is the download record in `root` a removal's (one that stopped before laying
/// anything: the game is as it was)?
pub fn download_is_removal(root: &Path) -> bool {
    std::fs::read_to_string(root.join(dlc::DOWNLOAD))
        .ok()
        .and_then(|s| serde_json::from_str::<Record>(&s).ok())
        .is_some()
}

/// `content.toml` without `removing`'s rows.
fn without(base: &Content, removing: &[AddingDlc]) -> Content {
    let mut c = base.clone();
    c.dlc.retain(|d| !removing.iter().any(|r| r.id == d.id));
    c
}

/// Settle a removal's journal after [`lay::recover`] went `dir` (the caller holds
/// the game's lock): forward commits it, back leaves the DLC installed.
pub(crate) fn settle(
    game: &mut Game,
    j: &Journal,
    dir: Direction,
    moved: Option<String>,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<(), AddError> {
    let root = game.dirs.root.clone();
    let names = j
        .removing
        .iter()
        .map(|d| d.title.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let message = match dir {
        Direction::Forward => {
            let c = without(&game.content, &j.removing);
            content::write(&root, &c)?;
            game.content = c;
            game.derived = false;
            lay::finish(&root)?;
            // What a removal that didn't stop does after its commit: the emptied
            // dirs, then setup for the products that stay.
            prune_dirs(&game.dirs.game, &j.prune);
            if let Err(e) = run_setup(&game.gogid, emit) {
                emit(&ProgressEvent::Warn {
                    message: format!(
                        "the removal is finished, but setup (GOG's install scripts) failed: \
                         {e}; run `gogo linux-app setup {}`",
                        game.gogid
                    ),
                });
            }
            format!(
                "an earlier removal of {names} had stopped while changing the game files: finished it"
            )
        }
        Direction::Back => format!(
            "an earlier removal of {names} had stopped and {}: undid it, {names} is \
             installed as it was",
            moved.unwrap_or_else(|| "the files it restores were gone".into())
        ),
    };
    let _ = std::fs::remove_file(root.join(dlc::DOWNLOAD));
    emit(&ProgressEvent::Warn { message });
    Ok(())
}

/// Why a removal's journal can't go forward, or `None` when it can: a file it
/// restores from the depot cache (`cache`) while the cache is on another build or
/// language now, or a source of a restored file (the cache's or the staging dir's)
/// that no longer has the MD5 the journal recorded. Going back is then the safe
/// way: the DLC's files and the replaced ones come back from the backup.
pub(crate) fn cache_moved(j: &Journal, cache: Option<&Path>) -> Option<String> {
    if let Some(cache) = cache
        && j.files.iter().any(|f| f.from.starts_with(cache))
    {
        match depot::read_record(cache) {
            Some(r)
                if r.build_id == j.build_id
                    && r.language.as_deref() == Some(j.language.as_str()) => {}
            r => {
                return Some(format!(
                    "the depot cache is on {} now, not build {} in {}",
                    r.map_or("no recorded build".into(), |r| format!(
                        "build {} in {}",
                        r.build_id,
                        r.language.as_deref().unwrap_or("no recorded language")
                    )),
                    j.build_id,
                    j.language
                ));
            }
        }
    }
    for f in &j.files {
        let Some(md5) = j.md5s.get(&f.to) else {
            continue;
        };
        // A source that's gone was laid already (a staging rename), or makes
        // the recovery go back by itself.
        if f.from.symlink_metadata().is_err() {
            continue;
        }
        let e = Expect {
            path: f.to.clone(),
            size: None,
            md5: Some(md5.clone()),
        };
        if !holds(&f.from, &e) {
            return Some(format!(
                "the source of {} ({}) changed since",
                f.to,
                f.from.display()
            ));
        }
    }
    None
}

/// Take out the dirs in `dirs` (relative to `game`, deepest first) that are empty
/// now. Best effort: the game is whole either way.
fn prune_dirs(game: &Path, dirs: &[String]) {
    for d in dirs {
        if lay::plain(d) && lay::symlinked_parent(game, &format!("{d}/x")).is_none() {
            let _ = std::fs::remove_dir(game.join(d));
        }
    }
}

/// Setup once, for the products that stay (GOG's install scripts).
fn run_setup(gogid: &str, emit: &mut dyn FnMut(&ProgressEvent)) -> Result<(), MacAppError> {
    super::setup::setup(
        gogid,
        &super::setup::SetupOptions {
            refresh: false,
            prompt: None,
            // The removal's own snapshot came first.
            runtime_snapshot: None,
        },
        emit,
    )
    .map(|_| ())
}

/// Drop a removal's leftover download record and staging dir (the game wasn't
/// touched by then).
fn clear_download(root: &Path) -> std::io::Result<()> {
    let staging = root.join(lay::STAGING);
    if staging.symlink_metadata().is_ok() {
        materialize::remove_tree(&staging)?;
    }
    match std::fs::remove_file(root.join(dlc::DOWNLOAD)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn changed(gogid: &str, why: &str) -> AddError {
    AddError::Game(MacAppError::Apps(format!(
        "{gogid} changed since the plan ({why}): run the removal again"
    )))
}

fn interrupted_lay(gogid: &str) -> AddError {
    AddError::Interrupted(
        "lay",
        format!(
            "the game won't start until `gogo linux-app dlc {gogid} remove` (or `add`) \
             finishes or undoes it"
        ),
    )
}

/// The step name of the check of the DLC's files before they come out.
pub const CHECKING: &str = "checking the DLC's files";

/// Remove `plan`'s DLC from `game` (design 19 §3.7). `fetch` downloads with the
/// given inputs into the given root (the depot seam), only when a restored file
/// needs it; `stop` says the user asked to stop (Ctrl-C).
pub async fn remove<F>(
    game: &mut Game,
    plan: &RemovePlan,
    fetch: F,
    opts: &AddOptions,
    emit: &mut dyn FnMut(&ProgressEvent),
    stop: &dyn Fn() -> bool,
) -> Result<RemoveOutcome, AddError>
where
    F: AsyncFnOnce(DownloadInputs, PathBuf) -> Result<(), DepotError>,
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
    // game is read again, and a change since stops the removal.
    *game = game.reload()?;
    let mut warnings = Vec::new();
    let now = game.installed();
    let removing: Vec<DlcRef> = plan
        .removing
        .iter()
        .filter(|d| now.contains(&d.id))
        .cloned()
        .collect();
    let outcome = |game: &Game,
                   removed: Vec<DlcRef>,
                   counts: (u64, u64),
                   lay: Option<LayReport>,
                   taken: Option<super::snapshots::Taken>,
                   warnings: Vec<String>| RemoveOutcome {
        gogid: game.gogid.clone(),
        build_id: game.content.build_id.clone(),
        language: game.content.language.clone(),
        dlcs: game.content.refs(),
        removed,
        recovered,
        files_removed: counts.0,
        files_restored: counts.1,
        lay,
        snapshot: taken.as_ref().map(super::snapshots::Taken::snapshot),
        pruned: taken
            .as_ref()
            .map(super::snapshots::Taken::pruned)
            .unwrap_or_default(),
        warnings,
    };
    // A recovery that finished this very removal: nothing left to do.
    if removing.is_empty() {
        return Ok(outcome(game, Vec::new(), (0, 0), None, None, warnings));
    }
    let mut planned: Vec<u64> = plan.installed.iter().map(|d| d.id).collect();
    let mut have = now.clone();
    planned.sort_unstable();
    have.sort_unstable();
    if planned != have {
        return Err(changed(&gogid, "its DLC are not the ones planned"));
    }
    if game.content.build_id.as_deref() != Some(plan.build_id.as_str()) {
        return Err(changed(&gogid, "its build is another one now"));
    }
    if super::update::language_of(game) != plan.language {
        return Err(changed(&gogid, "its language is another one now"));
    }
    if game.dirs.game != plan.game_dir {
        return Err(changed(
            &gogid,
            &format!("it is at {} now", game.dirs.root.display()),
        ));
    }
    // The depot cache the plan restores from must still be what it was then: the
    // same build, language and DLC set (another command may have moved it).
    if plan.restored.iter().any(|r| r.from == RestoreFrom::Cache) {
        let rec = plan.cache_root.as_deref().and_then(depot::read_record);
        let same = rec.as_ref().is_some_and(|r| {
            r.build_id == plan.build_id
                && r.language.as_deref() == Some(plan.language.as_str())
                && r.dlcs == plan.cache_dlcs
        });
        if !same {
            return Err(changed(&gogid, "its depot cache is another one now"));
        }
    }
    // Every restored path is a plain relative one, in the cache and in `game/`: a
    // manifest path with `..` would reach outside either.
    for r in &plan.restored {
        let rel = game_path(&plan.tree_name, &r.file.path);
        if !lay::plain(&r.file.path) || !rel.as_deref().is_some_and(lay::plain) {
            return Err(AddError::Game(MacAppError::Apps(format!(
                "the build's manifest names {}, which isn't a plain path inside the game, so \
                 nothing was changed; a rebuild without {}: {}",
                r.file.path,
                titles(&removing),
                rebuild_hint(game, &plan.ids())
            ))));
        }
    }
    // A download another command left: an add's is finished by the add, an
    // update's by the update. A removal's own leftover (a crash during its
    // download) is dropped: nothing was laid from it.
    if dlc::download_pending(&root) {
        if download_is_removal(&root) {
            clear_download(&root)?;
        } else {
            return Err(AddError::Game(MacAppError::Apps(format!(
                "a DLC add of {gogid} stopped during its download: finish it first with \
                 `gogo linux-app dlc {gogid} add`"
            ))));
        }
    }
    if super::update::download_pending(&root) {
        return Err(AddError::Game(MacAppError::Apps(format!(
            "an update of {gogid} stopped during its download: finish it first with \
             `gogo linux-app update {gogid}`"
        ))));
    }
    // The `auto` snapshot (design 20 §7.1, §9.5): after the recovery and the
    // refusals, before the first write. A failure stops the removal.
    let names = titles(&removing);
    let taken = super::snapshots::protect(
        &gogid,
        &game.dirs,
        Some(&lock),
        crate::snapshot::Trigger::DlcRemove,
        crate::snapshot::labels::removing(&names),
        &opts.snapshots,
        emit,
    )?;
    let staging = root.join(lay::STAGING);
    let downloads = plan.downloads();
    if downloads {
        if staging.symlink_metadata().is_ok() {
            materialize::remove_tree(&staging)?;
        }
        lay::write_json_durably(
            &root.join(dlc::DOWNLOAD),
            &Record {
                build_id: plan.build_id.clone(),
                source: staging.clone(),
                removing: plan.ids(),
            },
        )?;
        std::fs::create_dir_all(&staging)?;
        let fetched = fetch(plan.inputs.clone(), staging.clone()).await;
        let bad = match &fetched {
            Ok(()) => plan
                .restored
                .iter()
                .filter(|r| r.from == RestoreFrom::Download)
                .find(|r| !holds(&staging.join(&r.file.path), &r.file))
                .map(|r| r.file.path.clone()),
            Err(_) => None,
        };
        if fetched.is_err() || bad.is_some() {
            // Nothing was laid: the game is as it was.
            clear_download(&root)?;
        }
        match fetched {
            Ok(()) => {}
            Err(_) if stop() => {
                return Err(AddError::Interrupted(
                    "download",
                    "the game is as it was; run the removal again".into(),
                ));
            }
            Err(e) => return Err(e.into()),
        }
        if let Some(path) = bad {
            return Err(AddError::Game(MacAppError::Apps(format!(
                "the download didn't give the version of {path} the build has without \
                 {names}, so nothing was changed; a rebuild without it: {}",
                rebuild_hint(game, &plan.ids())
            ))));
        }
    }
    // The journal: restored files in, the DLC's files out.
    let mut files: Vec<LayFile> = Vec::new();
    // Each restored file's source → what it must be.
    let mut by_from: HashMap<PathBuf, Expect> = HashMap::new();
    for r in &plan.restored {
        let src = match (r.from, &plan.cache_root) {
            (RestoreFrom::Cache, Some(cache)) => cache.clone(),
            _ => staging.clone(),
        };
        for f in dlc::lay_list(&src, &plan.tree_name, std::slice::from_ref(&r.file.path)) {
            by_from.insert(f.from.clone(), r.file.clone());
            files.push(f);
        }
    }
    let mut expect: HashMap<String, Expect> = HashMap::new();
    let mut removed: Vec<String> = Vec::new();
    for e in &plan.removed {
        if let Some(rel) = game_path(&plan.tree_name, &e.path) {
            expect.insert(rel.to_lowercase(), e.clone());
            removed.push(rel);
        }
    }
    // The DLC's `goggame-<id>.*` by name: a record-only (zero-byte) DLC's came
    // from the support overlay and no depot lists them.
    let mut by_name: HashSet<String> = HashSet::new();
    if let Ok(rd) = std::fs::read_dir(&game.dirs.game) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let lower = name.to_lowercase();
            if removing
                .iter()
                .any(|d| lower.starts_with(&format!("goggame-{}.", d.id)))
                && !expect.contains_key(&lower)
            {
                by_name.insert(lower);
                removed.push(name);
            }
        }
    }
    let source = if downloads {
        staging.clone()
    } else {
        plan.cache_root.clone().unwrap_or_else(|| staging.clone())
    };
    let mut j = Journal {
        gogid: gogid.clone(),
        lane: "depot".into(),
        build_id: plan.build_id.clone(),
        language: plan.language.clone(),
        adding: Vec::new(),
        source,
        source_is_cache: false,
        moves: downloads && !plan.restored.iter().any(|r| r.from == RestoreFrom::Cache),
        files,
        replaced: Vec::new(),
        dirs: Vec::new(),
        kind: Some(lay::KIND_REMOVE.into()),
        from: None,
        to: None,
        removed,
        removing: removing
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
        .filter_map(|f| Some((f.to.clone(), by_from.get(&f.from)?.md5.clone()?)))
        .collect();
    // A file comes out only while it is the one the DLC laid: one the user or the
    // game changed stays, and so does one behind a symlinked dir.
    let total = j.removed.len() as u64;
    let mut done = 0u64;
    let mut kept = Vec::new();
    j.removed.retain(|r| {
        done += 1;
        emit(&ProgressEvent::WorkProgress {
            step: CHECKING.into(),
            files: Some(done),
            files_total: Some(total),
            bytes: None,
            bytes_total: None,
        });
        let lower = r.to_lowercase();
        let ok = if by_name.contains(&lower) {
            match lay::symlinked_parent(&game.dirs.game, r) {
                Some(link) => Err(format!("a dir on its path ({link}) is a symlink")),
                None => Ok(()),
            }
        } else {
            let e = expect.get(&lower);
            super::update::removable(
                &game.dirs.game,
                r,
                e.and_then(|e| e.size),
                e.and_then(|e| e.md5.as_deref()),
            )
        };
        match ok {
            Ok(()) => true,
            Err(why) => {
                kept.push(format!("kept {r}, which {names} laid: {why}"));
                false
            }
        }
    });
    if total > 0 {
        emit(&ProgressEvent::WorkDone {
            step: CHECKING.into(),
        });
    }
    warnings.extend(kept);
    // A restored file behind a symlinked dir refuses the removal, before any write
    // to `game/`.
    if let Err(e) = j.check_links(&game.dirs.game) {
        if downloads {
            clear_download(&root)?;
        }
        return Err(e.into());
    }
    // Each restored file's source is checked again right before the lay, wherever
    // it comes from: the plan's check of the cache was before the question.
    if let Some(f) = j
        .files
        .iter()
        .find(|f| !by_from.get(&f.from).is_some_and(|e| holds(&f.from, e)))
    {
        if downloads {
            clear_download(&root)?;
        }
        return Err(changed(
            &gogid,
            &format!("the copy of {} it restores from changed", f.to),
        ));
    }
    // Dirs the DLC's files leave empty go after the commit, unless a kept product
    // lists a file under them; deepest first. Kept in the journal, so a recovery
    // does the same.
    let mut dirs: Vec<String> = Vec::new();
    for r in &j.removed {
        let mut parts: Vec<&str> = r.split('/').collect();
        parts.pop();
        while !parts.is_empty() {
            let d = parts.join("/");
            if !dirs.contains(&d) && !plan.kept_dirs.contains(&d.to_lowercase()) {
                dirs.push(d);
            }
            parts.pop();
        }
    }
    dirs.sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));
    j.prune = dirs;
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
            kind: Some("dlc-remove".into()),
            files: Some(j.files.len() as u64),
        });
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
            },
        );
        emit(&ProgressEvent::WorkDone {
            step: crate::engine::events::step::GAME_FILES.into(),
        });
        lay_report = Some(report.map_err(|e| match e {
            lay::LayError::Stopped => interrupted_lay(&gogid),
            e => e.into(),
        })?);
    }
    // The commit.
    let c = without(&game.content, &j.removing);
    content::write(&root, &c)?;
    game.content = c;
    game.derived = false;
    lay::finish(&root)?;
    let _ = std::fs::remove_file(root.join(dlc::DOWNLOAD));
    prune_dirs(&game.dirs.game, &j.prune);
    if opts.run_setup
        && let Err(e) = run_setup(&gogid, emit)
    {
        warnings.push(format!(
            "the DLC is removed, but setup (GOG's install scripts) failed: {e}; run `gogo \
             linux-app setup {gogid}`"
        ));
    }
    let counts = (j.removed.len() as u64, j.files.len() as u64);
    Ok(outcome(game, removing, counts, lay_report, taken, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn depot(product: &str, files: &[(&str, u64, &str)]) -> DepotList {
        DepotList {
            product: product.into(),
            languages: vec!["*".into()],
            manifest: format!("m-{product}"),
            paths: files.iter().map(|(p, _, _)| p.to_string()).collect(),
            sizes: files.iter().map(|(p, s, _)| (p.to_string(), *s)).collect(),
            md5s: files
                .iter()
                .map(|(p, _, m)| (p.to_string(), m.to_string()))
                .collect(),
        }
    }

    #[test]
    fn a_dlc_only_file_is_removed_and_a_replaced_base_file_comes_back() {
        // The DLC's depot comes first, so its `shared.pak` is the one laid.
        let depots = vec![
            depot(
                "77",
                &[
                    ("G/dlc/ep1/a.archive", 11, "aa"),
                    ("G/Shared.pak", 10, "dlc"),
                    ("G/goggame-77.info", 2, "info"),
                ],
            ),
            depot(
                "1",
                &[("G/base.dat", 4, "base"), ("G/shared.pak", 11, "old")],
            ),
        ];
        let d = reverse_diff(&depots, "1", "G", &[77], &[77]);
        let removed: Vec<&str> = d.removed.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(removed, ["G/dlc/ep1/a.archive", "G/goggame-77.info"]);
        assert_eq!(d.removed[0].md5.as_deref(), Some("aa"));
        // The version a build without it lays: the base's.
        assert_eq!(
            d.shared,
            [Expect {
                path: "G/shared.pak".into(),
                size: Some(11),
                md5: Some("old".into()),
            }]
        );
        assert!(d.kept.contains(&"G/base.dat".to_string()));
    }

    #[test]
    fn a_path_a_kept_dlc_also_lists_isnt_removed() {
        let depots = vec![
            depot("1", &[("G/base.dat", 4, "base")]),
            depot(
                "77",
                &[("G/dlc/common.pak", 3, "c77"), ("G/dlc/77.pak", 3, "x")],
            ),
            depot("88", &[("G/dlc/common.pak", 3, "c88")]),
        ];
        let d = reverse_diff(&depots, "1", "G", &[77, 88], &[77]);
        let removed: Vec<&str> = d.removed.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(removed, ["G/dlc/77.pak"]);
        // The kept DLC's version of the shared path.
        assert_eq!(d.shared[0].md5.as_deref(), Some("c88"));
        // Removing both takes it out too.
        let d = reverse_diff(&depots, "1", "G", &[77, 88], &[77, 88]);
        let removed: Vec<&str> = d.removed.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(removed, ["G/dlc/77.pak", "G/dlc/common.pak"]);
        assert!(d.shared.is_empty());
    }

    #[test]
    fn a_dlc_overlay_file_on_a_base_tree_path_is_restored_not_just_deleted() {
        // The DLC's support overlay lays `G/data.ini` at the game root's `data.ini`,
        // over the base tree's own `data.ini`: one game path, two depot paths.
        let depots = vec![
            depot("1", &[("G/data.ini", 4, "base"), ("G/base.dat", 4, "b")]),
            depot(
                "77",
                &[
                    ("gog-support/1/77/app/data.ini", 5, "dlc"),
                    ("gog-support/1/77/app/goggame-77.info", 2, "info"),
                    ("gog-support/1/77/installer.exe", 9, "exe"),
                ],
            ),
        ];
        let d = reverse_diff(&depots, "1", "G", &[77], &[77]);
        let removed: Vec<&str> = d.removed.iter().map(|e| e.path.as_str()).collect();
        // The support installer never reaches `game/`: not listed.
        assert_eq!(removed, ["gog-support/1/77/app/goggame-77.info"]);
        // The base's version comes back.
        assert_eq!(
            d.shared,
            [Expect {
                path: "G/data.ini".into(),
                size: Some(4),
                md5: Some("base".into()),
            }]
        );
        // With the DLC, the overlay's copy is the one laid there (it wins over the
        // tree, as at a build), whatever the depot order.
        let d = reverse_diff(&depots, "1", "G", &[77], &[]);
        assert!(d.shared.is_empty() && d.removed.is_empty());
    }

    #[test]
    fn depots_of_products_not_installed_are_ignored() {
        let depots = vec![
            depot("1", &[("G/base.dat", 4, "base")]),
            depot("99", &[("G/base.dat", 9, "other"), ("G/x", 1, "x")]),
            depot("77", &[("G/y", 1, "y")]),
        ];
        let d = reverse_diff(&depots, "1", "G", &[77], &[77]);
        let removed: Vec<&str> = d.removed.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(removed, ["G/y"]);
        assert!(d.shared.is_empty());
    }

    #[test]
    fn names_resolve_against_the_installed_dlc() {
        let installed = vec![
            DlcRef {
                id: 7,
                title: "Phantom Liberty".into(),
            },
            DlcRef {
                id: 8,
                title: "REDmod".into(),
            },
        ];
        let n = |v: &[&str], partial| {
            resolve_installed(
                &v.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                &installed,
                partial,
            )
        };
        assert_eq!(n(&["8", "phantom liberty"], false).ids, [8, 7]);
        assert_eq!(n(&["liberty", "7"], true).ids, [7], "once");
        // Part of one title: only when the removal asks first.
        let r = n(&["liberty"], false);
        assert!(r.ids.is_empty());
        assert_eq!(r.inexact, [("liberty".into(), "Phantom Liberty".into())]);
        // Part of two titles: ambiguous, not "not installed".
        let r = n(&["9", "Hot Garbage", "e"], true);
        assert!(r.ids.is_empty());
        assert_eq!(r.unmatched, ["9", "Hot Garbage"]);
        assert_eq!(
            r.ambiguous,
            [(
                "e".into(),
                vec!["Phantom Liberty".to_string(), "REDmod".to_string()]
            )]
        );
    }

    #[test]
    fn a_removals_pending_download_names_dlc_remove() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(dlc::pending_kind(tmp.path()), ("add", "add"));
        std::fs::write(
            tmp.path().join(dlc::DOWNLOAD),
            r#"{"buildId":"B1","source":"/x/.dlc-staging","removing":[77]}"#,
        )
        .unwrap();
        assert_eq!(dlc::pending_kind(tmp.path()), ("removal", "remove"));
        // An add's record.
        std::fs::write(
            tmp.path().join(dlc::DOWNLOAD),
            r#"{"buildId":"B1","source":"/x","adding":[77],"before":{},"fetched":false}"#,
        )
        .unwrap();
        assert_eq!(dlc::pending_kind(tmp.path()), ("add", "add"));
    }

    #[test]
    fn holds_checks_the_md5_else_the_size() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("f");
        std::fs::write(&f, b"abc").unwrap();
        let md5 = "900150983cd24fb0d6963f7d28e17f72";
        let e = |size, md5: Option<&str>| Expect {
            path: "f".into(),
            size,
            md5: md5.map(str::to_string),
        };
        assert!(holds(&f, &e(Some(99), Some(md5))), "the MD5 decides");
        assert!(!holds(&f, &e(Some(3), Some("00"))));
        assert!(holds(&f, &e(Some(3), None)));
        assert!(!holds(&f, &e(Some(4), None)));
        assert!(!holds(&f, &e(None, None)));
        assert!(!holds(&tmp.path().join("missing"), &e(Some(3), None)));
    }

    #[test]
    fn find_ci_follows_the_casing_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("Archive/PC")).unwrap();
        std::fs::write(tmp.path().join("Archive/PC/x.pak"), b"").unwrap();
        assert_eq!(
            find_ci(tmp.path(), "archive/pc/X.PAK").unwrap(),
            tmp.path().join("Archive/PC/x.pak")
        );
        assert!(find_ci(tmp.path(), "archive/none").is_none());
    }
}
