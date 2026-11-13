//! `gogo linux-app dlc <gogid> list|add` on the depot lane (design 19 §1.3, §1.4,
//! §3.2, §3.4, §4, §7).
//!
//! The network side (the catalog, the owned set, the build list and manifest, the
//! downloader) is the caller's: [`plan_add`] and [`add`] take what it fetched, and the
//! download is a closure, so everything here runs on fixtures.
//!
//! An add: the game's lock, any interrupted add settled first ([`recover`]); the
//! wanted DLC fetched for the **installed** build into the depot cache (when its
//! record is that build and language, or an older record of that build is brought
//! up to date first, #374), where it stays, or into
//! `games/<gogid>/.dlc-staging/`, except a DLC the cache already holds whole, which
//! isn't fetched again (#381); the files that run wrote, and the held DLC's files
//! from their recorded lists, laid onto `game/` under a
//! journal ([`crate::gog::lay`]); `content.toml` written (the commit); the cache
//! record updated when the cache was the source; staging, backups and the journal
//! deleted; setup run once for the DLC's install scripts.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::engine::events::ProgressEvent;
use crate::gog::cache_upgrade::{self, OnDisk, Upgrade};
use crate::gog::content::{self, Content, DlcRow};
use crate::gog::depot::{self, BuildMatch, DepotError, DownloadInputs, PlanDlc, Snapshot};
use crate::gog::dlc::DlcInfo;
use crate::gog::lay::{self, AddingDlc, Direction, Journal, LayFile, LayReport};
use crate::gog::materialize::{self, Reflink, TreeMethod};
use crate::macapp::error::MacAppError;
use crate::macapp::launch::GameDirs;

use super::game;

/// The download's record in the game dir: the source root, what it fetches and the
/// root's snapshot from before the first try, so a rerun after a stop counts what
/// the earlier try already moved into the cache (design 19 §3.4).
pub const DOWNLOAD: &str = "dlc-download.json";

/// A built game as `dlc` sees it.
#[derive(Debug, Clone)]
pub struct Game {
    pub gogid: String,
    pub dirs: GameDirs,
    pub content: Content,
    /// No `content.toml`: derived from the infos and the cache record.
    pub derived: bool,
    /// The game's depot cache root (`<cache>/gogo/depot/<gogid>`), when there is a
    /// user cache dir.
    pub cache_root: Option<PathBuf>,
}

/// The built game `gogid`, its content and its cache root. Refused when it isn't
/// built, or its recorded dir is unreachable.
pub fn load_game(gogid: &str, cache_base: Option<&Path>) -> Result<Game, MacAppError> {
    let dirs = GameDirs::for_game(gogid)?;
    if !dirs.root.is_dir() {
        return Err(MacAppError::Apps(
            if crate::macapp::launch::recorded_location(gogid).is_some() {
                format!(
                    "{gogid} is recorded at {} and it isn't there (a drive not mounted?)",
                    dirs.root.display()
                )
            } else {
                format!("{gogid} isn't built: no {}", dirs.root.display())
            },
        ));
    }
    if !game::launch_toml_path(&dirs).is_file() {
        return Err(MacAppError::Apps(format!(
            "{} has no {}: {gogid} isn't a `gogo linux-app` game",
            dirs.root.display(),
            game::LAUNCH_TOML
        )));
    }
    let cache_root = depot::cache_roots(cache_base, gogid.parse().unwrap_or(0), None).map(|r| r.0);
    let (lane, install) = if dirs.game.is_dir() {
        (content::Lane::Depot, dirs.game.clone())
    } else {
        (content::Lane::Installer, dirs.prefix.join("drive_c/GOG"))
    };
    let r = content::resolve(&dirs.root, &install, lane, cache_root.as_deref())
        .map_err(|e| MacAppError::Apps(e.to_string()))?;
    Ok(Game {
        gogid: gogid.to_string(),
        dirs,
        content: r.content,
        derived: r.derived,
        cache_root,
    })
}

impl Game {
    /// The game as its dir says now, with the same cache root: what a command
    /// re-reads under the game's lock.
    pub fn reload(&self) -> Result<Game, MacAppError> {
        let dirs = GameDirs::for_game(&self.gogid)?;
        let (lane, install) = if dirs.game.is_dir() {
            (content::Lane::Depot, dirs.game.clone())
        } else {
            (content::Lane::Installer, dirs.prefix.join("drive_c/GOG"))
        };
        let r = content::resolve(&dirs.root, &install, lane, self.cache_root.as_deref())
            .map_err(|e| MacAppError::Apps(e.to_string()))?;
        Ok(Game {
            gogid: self.gogid.clone(),
            dirs,
            content: r.content,
            derived: r.derived,
            cache_root: self.cache_root.clone(),
        })
    }

    pub fn installed(&self) -> Vec<u64> {
        self.content.dlc.iter().map(|d| d.id).collect()
    }

    fn installed_strings(&self) -> Vec<String> {
        self.installed().iter().map(u64::to_string).collect()
    }
}

// — list (§1.3) —

/// One row of `dlc list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListRow {
    pub id: u64,
    pub title: String,
    pub owned: bool,
    pub installed: bool,
    pub installed_build: Option<String>,
    pub installed_version: Option<String>,
    pub in_build: bool,
    pub download_size: Option<u64>,
    pub disk_size: Option<u64>,
}

/// `dlc list`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Listing {
    pub gogid: String,
    pub lane: String,
    pub build_id: Option<String>,
    pub language: Option<String>,
    /// Said under the table (the build is gone, sizes are another build's…).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    pub dlcs: Vec<ListRow>,
}

/// The rows: `infos` (catalog ∪ manifest, design 19 §1.3) with what the game has,
/// then installed DLC neither lists.
pub fn listing(game: &Game, infos: &[DlcInfo], owned: &[u64], notes: Vec<String>) -> Listing {
    let row_of = |id: u64| game.content.dlc.iter().find(|d| d.id == id);
    let mut dlcs: Vec<ListRow> = infos
        .iter()
        .map(|d| {
            let have = row_of(d.id);
            ListRow {
                id: d.id,
                title: have
                    .filter(|_| d.title == d.id.to_string())
                    .map_or(d.title.clone(), |r| r.title.clone()),
                owned: d.owned,
                installed: have.is_some(),
                installed_build: have.and_then(|r| r.build_id.clone()),
                installed_version: have.and_then(|r| r.version.clone()),
                in_build: d.in_build,
                download_size: d.download_size,
                disk_size: d.disk_size,
            }
        })
        .collect();
    for r in &game.content.dlc {
        if !dlcs.iter().any(|d| d.id == r.id) {
            dlcs.push(ListRow {
                id: r.id,
                title: r.title.clone(),
                owned: owned.contains(&r.id),
                installed: true,
                installed_build: r.build_id.clone(),
                installed_version: r.version.clone(),
                in_build: false,
                download_size: None,
                disk_size: None,
            });
        }
    }
    Listing {
        gogid: game.gogid.clone(),
        lane: game.content.lane.as_str().to_string(),
        build_id: game.content.build_id.clone(),
        language: game.content.language.clone(),
        notes,
        dlcs,
    }
}

/// `dlc list`'s table (design 19 §1.3).
pub fn render_list(l: &Listing) -> String {
    let size = |r: &ListRow, v: Option<u64>| -> String {
        if !r.in_build && l.lane == "depot" {
            "-".into()
        } else {
            v.map_or("-".into(), crate::fmt::human_size)
        }
    };
    let rows: Vec<[String; 6]> = l
        .dlcs
        .iter()
        .map(|r| {
            [
                r.id.to_string(),
                r.title.clone(),
                if r.owned { "yes" } else { "no" }.to_string(),
                match (r.installed, &r.installed_version) {
                    (false, _) => "no".to_string(),
                    (true, Some(v)) => format!("yes ({v})"),
                    (true, None) => "yes".to_string(),
                },
                if !r.in_build && l.lane == "depot" {
                    "not in this build".to_string()
                } else {
                    size(r, r.download_size)
                },
                size(r, r.disk_size),
            ]
        })
        .collect();
    let head = ["id", "title", "owned", "installed", "download", "disk"];
    let mut w = head.map(str::len);
    for r in &rows {
        for (i, c) in r.iter().enumerate() {
            w[i] = w[i].max(c.chars().count());
        }
    }
    let line = |cells: [&str; 6]| -> String {
        let mut s = String::new();
        for (i, c) in cells.iter().enumerate() {
            if i == 5 {
                s.push_str(c);
            } else {
                s.push_str(c);
                s.push_str(&" ".repeat(w[i] - c.chars().count() + 2));
            }
        }
        s.trim_end().to_string() + "\n"
    };
    let mut out = format!(
        "{} ({} lane, build {}, {})\n",
        l.gogid,
        l.lane,
        l.build_id.as_deref().unwrap_or("unknown"),
        l.language.as_deref().unwrap_or("language unknown")
    );
    if rows.is_empty() {
        out.push_str("no DLC\n");
    } else {
        out.push_str(&line(head));
        for r in &rows {
            out.push_str(&line([&r[0], &r[1], &r[2], &r[3], &r[4], &r[5]]));
        }
    }
    for n in &l.notes {
        out.push_str(&format!("note: {n}\n"));
    }
    out
}

// — refusals (§4) —

/// The command that updates the game and takes `dlcs` (the installed and the
/// wanted ones: a rebuild's `--dlc` is the whole set, design 19 §1.2).
pub fn update_hint(gogid: &str, dlcs: &[u64]) -> String {
    let flags: String = dlcs.iter().map(|d| format!(" --dlc {d}")).collect();
    format!("gogo linux-app {gogid}{flags} (keeps the prefix, saves and L4)")
}

/// Pure: why an add can't go on for this build match, if it can't (design 19 §4).
pub fn refusal(
    matched: &BuildMatch,
    build_id: Option<&str>,
    gogid: &str,
    dlcs: &[u64],
) -> Option<String> {
    match matched {
        BuildMatch::Newest | BuildMatch::Older { .. } => None,
        BuildMatch::Gone => Some(format!(
            "build {} is no longer served by GOG; update the game first: {}",
            build_id.unwrap_or("?"),
            update_hint(gogid, dlcs)
        )),
        BuildMatch::Unknown => Some(format!(
            "the game's build isn't known (no build in {} or its goggame info), and DLC for a \
             guessed build could mix two versions; update the game first: {}",
            content::FILE,
            update_hint(gogid, dlcs)
        )),
    }
}

// — the plan (§1.4) —

/// Where the DLC's files are fetched to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// The game's depot cache: its record is this build and language. The DLC
    /// stays in it.
    Cache,
    /// `games/<gogid>/.dlc-staging/`.
    Staging,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Cache => "cache",
            Source::Staging => "staging",
        }
    }
}

/// A newer build than the installed one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewerBuild {
    pub build_id: String,
    pub version: String,
}

/// What GOG's install script of one DLC does, when it can be read before the
/// download (it's in the source already).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptPlan {
    pub id: u64,
    /// Registry values it sets; `None`: not known before the download.
    pub registry_values: Option<usize>,
}

/// What `dlc add` will do (design 19 §1.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddPlan {
    pub gogid: String,
    pub title: String,
    pub build_id: String,
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newer: Option<NewerBuild>,
    pub language: String,
    /// No language is recorded: the depot default is assumed.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub language_assumed: bool,
    pub installed: Vec<u64>,
    /// The DLC it adds; a 0-byte one is a record-only add.
    pub dlcs: Vec<PlanDlc>,
    /// Named, and already installed: a notice.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub already: Vec<u64>,
    pub source: Source,
    pub source_root: PathBuf,
    /// What becomes of the depot cache: whether the DLC is kept in it (#374).
    pub depot_cache: CacheFate,
    /// The cache record upgrade [`add`] writes before anything else (#374).
    #[serde(skip)]
    pub upgrade: Option<Upgrade>,
    /// The DLC whose files the depot cache already holds whole (its record lists
    /// them, keeps their file lists, and every listed file is there): laid from the
    /// cache, not downloaded (#381).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub from_cache: Vec<u64>,
    /// Their files: paths relative to the cache root, as on disk.
    #[serde(skip)]
    pub cached_files: Vec<(u64, Vec<String>)>,
    /// The game tree's dir name in the source root (the manifest's install dir).
    pub tree_name: String,
    /// `reflink`, `copy` or `rename` (from staging).
    pub game_files: String,
    pub game_dir: PathBuf,
    pub download_size: u64,
    pub disk_size: u64,
    /// Extra disk the game dir takes: the DLC's size for a copy, else 0 (a staging
    /// root's download is already in the game dir).
    pub extra_bytes: u64,
    pub scripts: Vec<ScriptPlan>,
    #[serde(skip)]
    pub inputs: DownloadInputs,
}

impl AddPlan {
    /// Whether any DLC has files to lay (else a record-only add).
    pub fn fetches(&self) -> bool {
        self.dlcs.iter().any(|d| d.download_size > 0)
    }

    /// The DLC the downloader fetches: those with files the cache doesn't hold.
    pub fn downloading(&self) -> Vec<&PlanDlc> {
        self.dlcs
            .iter()
            .filter(|d| {
                d.download_size > 0
                    && !d
                        .id
                        .parse::<u64>()
                        .is_ok_and(|id| self.from_cache.contains(&id))
            })
            .collect()
    }

    /// Whether anything is downloaded at all.
    pub fn downloads(&self) -> bool {
        !self.downloading().is_empty()
    }

    pub fn ids(&self) -> Vec<u64> {
        self.dlcs.iter().filter_map(|d| d.id.parse().ok()).collect()
    }
}

/// What [`plan_add`] needs from the network side.
pub struct PlanInput<'a> {
    pub game: &'a Game,
    pub title: &'a str,
    /// The installed build's version name.
    pub version: Option<String>,
    pub newer: Option<NewerBuild>,
    /// The manifest's install directory: the tree's name in the source root.
    pub tree_name: String,
    pub infos: &'a [DlcInfo],
    /// Resolved names (owned, in the build); installed ones become `already`.
    pub wanted: &'a [u64],
    /// What an older cache record of the installed build would become
    /// ([`cache_upgrade::check`]); `None` when there's none to bring up to date.
    pub cache_upgrade: Option<&'a Result<Upgrade, String>>,
}

/// The depot default language, for a game with none recorded.
pub const DEFAULT_LANGUAGE: &str = "en-US";

/// The plan for adding `input.wanted` (design 19 §1.4, §3.2).
pub fn plan_add(input: &PlanInput<'_>) -> AddPlan {
    let game = input.game;
    let installed = game.installed();
    let language = game.content.language.clone();
    let language_assumed = language.is_none();
    let language = language.unwrap_or_else(|| DEFAULT_LANGUAGE.to_string());
    let build_id = game.content.build_id.clone().unwrap_or_default();
    let (already, adding): (Vec<u64>, Vec<u64>) =
        input.wanted.iter().partition(|id| installed.contains(id));
    let dlcs: Vec<PlanDlc> = adding
        .iter()
        .map(|id| {
            let d = input.infos.iter().find(|d| d.id == *id);
            PlanDlc {
                id: id.to_string(),
                title: d.map_or(id.to_string(), |d| d.title.clone()),
                download_size: d.and_then(|d| d.download_size).unwrap_or(0),
                disk_size: d.and_then(|d| d.disk_size).unwrap_or(0),
            }
        })
        .collect();
    let picked = pick_source(
        game,
        &build_id,
        &language,
        &input.tree_name,
        input.cache_upgrade,
    );
    let (source, source_root) = (picked.source, picked.root.clone());
    let cached_files: Vec<(u64, Vec<String>)> = if source == Source::Cache {
        dlcs.iter()
            .filter(|d| d.download_size > 0)
            .filter_map(|d| d.id.parse::<u64>().ok())
            .filter_map(|id| {
                Some((
                    id,
                    cached_copy(&source_root, &picked.held, &picked.products, id)?,
                ))
            })
            .collect()
    } else {
        Vec::new()
    };
    let from_cache: Vec<u64> = cached_files.iter().map(|(id, _)| *id).collect();
    let download_size = dlcs
        .iter()
        .filter(|d| !d.id.parse::<u64>().is_ok_and(|id| from_cache.contains(&id)))
        .map(|d| d.download_size)
        .sum();
    let disk_size: u64 = dlcs.iter().map(|d| d.disk_size).sum();
    let method = match source {
        Source::Staging => "rename",
        Source::Cache => {
            match materialize::plan_tree(&source_root, &game.dirs.game, disk_size).method {
                TreeMethod::Reflink => "reflink",
                TreeMethod::Copy => "copy",
            }
        }
    };
    let extra_bytes = if method == "copy" { disk_size } else { 0 };
    let tree = source_root.join(&input.tree_name);
    let ctx = crate::macapp::goggame::Ctx {
        app: &format!(r"C:\GOG\{}", input.tree_name),
        userdocs: r"C:\users\steamuser\Documents",
        group: input.title,
    };
    let scripts = adding
        .iter()
        .map(|id| ScriptPlan {
            id: *id,
            registry_values: script_dirs(&tree)
                .iter()
                .find_map(|d| crate::macapp::goggame::load(d, &id.to_string(), &ctx))
                .map(|p| p.values.len()),
        })
        .collect();
    // The diff is told what the source root holds: for staging, what the game has
    // (only the new DLC's files are fetched); for the cache, the installed DLC it
    // holds (one staged in an earlier add isn't in it, so a file the new DLC shares
    // with that one is fetched into the cache too, #374), plus the DLC it holds
    // that are added now, so a mixed add fetches only the others.
    let mut there: Vec<String> = match source {
        Source::Staging => game.installed_strings(),
        Source::Cache => game
            .installed_strings()
            .into_iter()
            .filter(|d| picked.held.contains(d))
            .collect(),
    };
    there.extend(from_cache.iter().map(u64::to_string));
    let wanted: Vec<String> = adding
        .iter()
        .filter(|id| !from_cache.contains(id))
        .map(u64::to_string)
        .collect();
    AddPlan {
        gogid: game.gogid.clone(),
        title: input.title.to_string(),
        inputs: depot::add_inputs(&language, &there, &wanted),
        from_cache,
        cached_files,
        build_id,
        version: input.version.clone(),
        newer: input.newer.clone(),
        language,
        language_assumed,
        installed,
        dlcs,
        already,
        source,
        source_root,
        depot_cache: picked.fate,
        upgrade: picked.upgrade,
        tree_name: input.tree_name.clone(),
        game_files: method.to_string(),
        game_dir: game.dirs.game.clone(),
        download_size,
        disk_size,
        extra_bytes,
        scripts,
    }
}

fn script_dirs(tree: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![tree.to_path_buf()];
    if let Ok(rd) = std::fs::read_dir(tree) {
        let mut subs: Vec<PathBuf> = rd
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
        subs.sort();
        dirs.extend(subs);
    }
    dirs
}

/// What becomes of the depot cache in an add.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum CacheFate {
    /// The cache is the source: the DLC is fetched into it (or is there already)
    /// and stays. `upgraded`: its record from before DLC tracking is brought up to
    /// date first (#374), with this summary.
    #[serde(rename_all = "camelCase")]
    Kept {
        #[serde(skip_serializing_if = "Option::is_none")]
        upgraded: Option<String>,
    },
    /// There's no depot cache of the game: the DLC is staged and isn't kept in one.
    None,
    /// There's a cache, but it can't take the DLC (why).
    NotKept { reason: String },
}

/// Where an add fetches to, and what the cache holds as the plan sees it.
struct Picked {
    source: Source,
    root: PathBuf,
    fate: CacheFate,
    /// The DLC the cache holds (when it's the source).
    held: Vec<String>,
    /// Its per-product file lists (when it's the source).
    products: crate::gog::dlc_filter::ProductFiles,
    /// The record upgrade [`add`] writes first.
    upgrade: Option<Upgrade>,
}

/// The cache when its record is the installed build and language (or an older
/// record of that build that `upgrade` brings up to date, #374), has lost no file,
/// and its tree is there; else the staging dir. The cache needn't hold every
/// installed DLC: one added while it couldn't be the source is left to the game
/// dir, and the diff is told only what the cache holds (§3.2). A download that
/// stopped keeps its source, so a rerun resumes into it.
fn pick_source(
    game: &Game,
    build_id: &str,
    language: &str,
    tree_name: &str,
    upgrade: Option<&Result<Upgrade, String>>,
) -> Picked {
    let staging = game.dirs.root.join(lay::STAGING);
    let staged = |fate| Picked {
        source: Source::Staging,
        root: staging.clone(),
        fate,
        held: Vec::new(),
        products: Default::default(),
        upgrade: None,
    };
    let current = |cache: &Path| {
        let rec = depot::read_record(cache);
        Picked {
            source: Source::Cache,
            root: cache.to_path_buf(),
            fate: CacheFate::Kept { upgraded: None },
            held: rec.and_then(|r| r.dlcs).unwrap_or_default(),
            products: depot::record_products(cache, build_id).unwrap_or_default(),
            upgrade: None,
        }
    };
    if let Some(rec) = read_download(&game.dirs.root) {
        return if rec.source == staging {
            staged(CacheFate::NotKept {
                reason: "an earlier add that stopped staged it".into(),
            })
        } else {
            current(&rec.source)
        };
    }
    let Some(cache) = game
        .cache_root
        .as_ref()
        .filter(|c| !build_id.is_empty() && c.join(tree_name).is_dir())
    else {
        return staged(CacheFate::None);
    };
    let not_kept = |reason: String| staged(CacheFate::NotKept { reason });
    let Some(rec) = depot::read_record(cache) else {
        return not_kept("the depot cache has no record of a finished download".into());
    };
    if rec.build_id != build_id {
        return not_kept(format!("the depot cache holds build {}", rec.build_id));
    }
    if let depot::CacheCheck::Missing { count, .. } = depot::check_cache(
        cache,
        &depot::CacheWant {
            build_id: Some(build_id),
            ..Default::default()
        },
    ) {
        return not_kept(format!(
            "the depot cache lost {count} file(s) its record names"
        ));
    }
    match (&rec.language, &rec.dlcs) {
        (Some(l), Some(_)) if l.eq_ignore_ascii_case(language) => current(cache),
        (Some(l), Some(_)) => not_kept(format!("the depot cache is in {l}")),
        _ => match upgrade {
            Some(Ok(u)) if u.build_id == build_id && u.language.eq_ignore_ascii_case(language) => {
                Picked {
                    source: Source::Cache,
                    root: cache.clone(),
                    fate: CacheFate::Kept {
                        upgraded: Some(u.summary()),
                    },
                    held: u.dlcs.clone(),
                    products: u.products.clone(),
                    upgrade: Some(u.clone()),
                }
            }
            Some(Err(why)) => not_kept(format!(
                "the depot cache's record predates DLC tracking and can't be brought up \
                 to date: {why}"
            )),
            _ => not_kept("the depot cache's record predates DLC tracking".into()),
        },
    }
}

/// DLC `id`'s files in cache root `root`, as paths relative to it spelled as on
/// disk, when the cache already holds them whole (#381): it lists `id` among the
/// DLC it holds (`held`, from its record or the record's upgrade), keeps `id`'s
/// file list in `products`, and every listed path is there as a file or link.
/// `None` otherwise, and the add downloads it.
///
/// Existence is the check, not a hash and not a size (the lists keep paths only):
/// gog-warp writes a file as `<name>.download` and renames it once it's whole, so a
/// listed path under its own name is a finished file. That's the same trust a build
/// gives the cache tree.
fn cached_copy(
    root: &Path,
    held: &[String],
    products: &crate::gog::dlc_filter::ProductFiles,
    id: u64,
) -> Option<Vec<String>> {
    if !held.contains(&id.to_string()) {
        return None;
    }
    let list = products.get(&id.to_string())?;
    if list.is_empty() {
        return None;
    }
    let mut dirs = OnDisk::default();
    list.iter().map(|p| dirs.find(root, p)).collect()
}

/// The plan's lines (human).
pub fn plan_lines(p: &AddPlan) -> Vec<String> {
    let size = crate::fmt::human_size;
    let mut out = Vec::new();
    let names: Vec<String> = p
        .dlcs
        .iter()
        .map(|d| {
            if d.download_size == 0 {
                format!("{} (0 B: record only)", d.title)
            } else {
                format!(
                    "{} ({}, {} on disk)",
                    d.title,
                    size(d.download_size),
                    size(d.disk_size)
                )
            }
        })
        .collect();
    out.push(format!(
        "add to {} ({}): {}",
        p.title,
        p.gogid,
        names.join(", ")
    ));
    out.push(format!(
        "for build {}{}, the installed one ({}{})",
        p.build_id,
        p.version
            .as_ref()
            .map_or(String::new(), |v| format!(" ({v})")),
        p.language,
        if p.language_assumed {
            ": none recorded, the depot default"
        } else {
            ""
        }
    ));
    if let Some(n) = &p.newer {
        out.push(format!(
            "GOG has a newer build, {} ({}); to update: {}",
            n.build_id,
            n.version,
            update_hint(&p.gogid, &[p.installed.clone(), p.ids()].concat())
        ));
    }
    for id in &p.already {
        out.push(format!("{id} is installed already"));
    }
    if p.fetches() {
        let held: Vec<&str> = p
            .dlcs
            .iter()
            .filter(|d| {
                d.id.parse::<u64>()
                    .is_ok_and(|id| p.from_cache.contains(&id))
            })
            .map(|d| d.title.as_str())
            .collect();
        out.push(format!(
            "files from: {}",
            match p.source {
                Source::Cache if !p.downloads() => {
                    "the depot cache (already there, no download)".to_string()
                }
                Source::Cache if !held.is_empty() => format!(
                    "the depot cache ({}); {} already there, the rest downloaded into it",
                    p.source_root.display(),
                    held.join(", ")
                ),
                Source::Cache => format!("the depot cache ({})", p.source_root.display()),
                Source::Staging => format!("staging in the game dir ({})", p.source_root.display()),
            }
        ));
        out.push(match p.game_files.as_str() {
            "copy" => format!(
                "game files: copy into {} (+{} disk: no reflink from the cache)",
                p.game_dir.display(),
                size(p.extra_bytes)
            ),
            "reflink" => format!(
                "game files: reflink into {} (no extra disk)",
                p.game_dir.display()
            ),
            _ => format!(
                "game files: moved from staging into {}",
                p.game_dir.display()
            ),
        });
        out.push(format!(
            "{}, disk {}{}",
            if p.downloads() {
                format!("download {}", size(p.download_size))
            } else {
                "no download".to_string()
            },
            size(p.disk_size),
            if p.extra_bytes > 0 {
                format!(" (+{} for the copy)", size(p.extra_bytes))
            } else {
                String::new()
            }
        ));
        if let Some(l) = cache_line(p) {
            out.push(l);
        }
    }
    for s in &p.scripts {
        if let Some(n) = s.registry_values {
            out.push(format!(
                "GOG's install script for {}: {n} registry value(s)",
                s.id
            ));
        }
    }
    out
}

/// The plan's word on the depot cache (#374): whether the DLC is kept in it, and a
/// record brought up to date. Nothing to say when nothing is downloaded.
fn cache_line(p: &AddPlan) -> Option<String> {
    let rebuild = "a rebuild with it downloads it again";
    match &p.depot_cache {
        CacheFate::Kept { upgraded: None } if p.downloads() => {
            Some("depot cache: the DLC is kept in it too".into())
        }
        CacheFate::Kept { upgraded: None } => None,
        CacheFate::Kept {
            upgraded: Some(summary),
        } => Some(format!(
            "depot cache: its record is brought up to date ({summary}), and the DLC is \
             kept in it too"
        )),
        CacheFate::None if p.downloads() => Some(format!(
            "depot cache: none, so the DLC isn't kept in one ({rebuild})"
        )),
        CacheFate::NotKept { reason } if p.downloads() => Some(format!(
            "depot cache: {reason}, so the DLC isn't kept in it ({rebuild})"
        )),
        _ => None,
    }
}

// — the add (§3.2, §3.4) —

/// Why an add stopped.
#[derive(Debug, thiserror::Error)]
pub enum AddError {
    #[error(transparent)]
    Depot(#[from] DepotError),
    #[error(transparent)]
    Game(#[from] MacAppError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Ctrl-C or a lost connection. During the download the game is untouched;
    /// during the lay the journal stays and the next `dlc add` settles it.
    #[error("stopped during the {0}; {1}")]
    Interrupted(&'static str, String),
}

impl From<lay::LayError> for AddError {
    fn from(e: lay::LayError) -> Self {
        match e {
            lay::LayError::Stopped => AddError::Interrupted("lay", rerun_note()),
            lay::LayError::Symlink { .. } => AddError::Game(MacAppError::Apps(e.to_string())),
            lay::LayError::Io(e) => AddError::Io(e),
        }
    }
}

fn rerun_note() -> String {
    "the game won't start until `gogo linux-app dlc <gogid> add` finishes or undoes it".into()
}

/// How to run an add.
#[derive(Debug, Clone)]
pub struct AddOptions {
    pub reflink: Reflink,
    /// The `auto` snapshot before the add (design 20 §7.1, §9.4):
    /// `--no-snapshots`, `keep_auto`, the backend.
    pub snapshots: super::snapshots::AutoOptions,
    /// Run setup after the commit (GOG's install scripts). Tests of the lay alone
    /// turn it off.
    pub run_setup: bool,
    /// Crash injection: stop at this lay step (design 19 §10).
    #[doc(hidden)]
    pub fail_at: Option<lay::Step>,
}

impl Default for AddOptions {
    fn default() -> Self {
        Self {
            reflink: Reflink::Try,
            snapshots: super::snapshots::AutoOptions::default(),
            run_setup: true,
            fail_at: None,
        }
    }
}

/// What an add did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddOutcome {
    pub gogid: String,
    pub build_id: Option<String>,
    pub language: Option<String>,
    /// Every DLC the game has now.
    pub dlcs: Vec<content::DlcRef>,
    pub added: Vec<content::DlcRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovered: Option<Direction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lay: Option<LayReport>,
    /// The `auto` snapshot taken before the add (design 20 §7.1), if any.
    pub snapshot: Option<super::snapshots::SnapshotRef>,
    /// Older `auto` snapshots retention deleted after it (§7.2).
    pub pruned: Vec<super::snapshots::SnapshotRef>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DownloadRecord {
    build_id: String,
    source: PathBuf,
    adding: Vec<u64>,
    before: Snapshot,
    /// The downloader finished: a rerun lays without fetching again.
    fetched: bool,
}

fn read_download(root: &Path) -> Option<DownloadRecord> {
    serde_json::from_str(&std::fs::read_to_string(root.join(DOWNLOAD)).ok()?).ok()
}

/// Is a DLC add (or an update, design 21 §11g, or a removal, §3.7) of the game in `root` in the middle
/// of laying files (the launcher refuses, doctor fails)?
pub fn journal_exists(root: &Path) -> bool {
    lay::journal_path(root).exists()
}

/// Which DLC change is pending in the game dir `root` (a journal, a download
/// record or a staging dir): `("removal", "remove")` for a removal's (design 19
/// §3.7), else `("add", "add")`, for the messages that name the command that
/// settles it. An update's journal is settled by either.
pub fn pending_kind(root: &Path) -> (&'static str, &'static str) {
    let removal = lay::read_journal(root)
        .ok()
        .flatten()
        .is_some_and(|j| j.is_removal())
        || super::dlc_remove::download_is_removal(root);
    if removal {
        ("removal", "remove")
    } else {
        ("add", "add")
    }
}

/// Did an add stop during its download (the game is untouched)?
pub fn download_pending(root: &Path) -> bool {
    root.join(DOWNLOAD).exists()
}

/// The files a download wrote into source root `root` (paths relative to it) that
/// go into `game/`: the tree's, and the support overlays' `app/` (which win, as at
/// a build). Anything else (`gog-support`'s installers) stays.
pub fn lay_list(root: &Path, tree_name: &str, changed: &[String]) -> Vec<LayFile> {
    let mut out: Vec<LayFile> = Vec::new();
    let mut overlay: Vec<LayFile> = Vec::new();
    let prefix = format!("{tree_name}/");
    for c in changed {
        if let Some(rel) = c.strip_prefix(&prefix) {
            out.push(LayFile {
                from: root.join(c),
                to: rel.to_string(),
            });
        } else if let Some(rest) = c.strip_prefix("gog-support/") {
            let parts: Vec<&str> = rest.splitn(4, '/').collect();
            if parts.len() == 4 && parts[2] == "app" {
                overlay.push(LayFile {
                    from: root.join(c),
                    to: parts[3].to_string(),
                });
            }
        }
    }
    for o in overlay {
        out.retain(|f| f.to != o.to);
        out.push(o);
    }
    out
}

/// A record-only DLC's `goggame-<id>` files, when the source's support overlay has
/// them (design 19 §3.2).
fn support_files(root: &Path, id: u64) -> Vec<LayFile> {
    let mut out = Vec::new();
    let Ok(outer) = std::fs::read_dir(root.join("gog-support")) else {
        return out;
    };
    for o in outer.flatten() {
        let app = o.path().join(id.to_string()).join("app");
        if let Ok(rd) = std::fs::read_dir(&app) {
            for f in rd.flatten() {
                let name = f.file_name().to_string_lossy().into_owned();
                if name.starts_with(&format!("goggame-{id}")) && f.path().is_file() {
                    out.push(LayFile {
                        from: f.path(),
                        to: name,
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| a.to.cmp(&b.to));
    out
}

/// `content.toml` with `adding` recorded for build `build_id` in `language`.
fn committed(base: &Content, build_id: &str, language: &str, adding: &[AddingDlc]) -> Content {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut c = base.clone();
    if c.build_id.is_none() {
        c.build_id = Some(build_id.to_string());
    }
    if c.language.is_none() {
        c.language = Some(language.to_string());
    }
    for a in adding {
        if !c.dlc.iter().any(|d| d.id == a.id) {
            c.dlc.push(DlcRow {
                id: a.id,
                title: a.title.clone(),
                build_id: Some(build_id.to_string()),
                version: None,
                added: Some(today.clone()),
            });
        }
    }
    c
}

/// The commit of an add: `content.toml`, then the cache record when the cache was
/// the source.
fn commit(game: &Game, j: &Journal) -> Result<Content, AddError> {
    let c = committed(&game.content, &j.build_id, &j.language, &j.adding);
    content::write(&game.dirs.root, &c)?;
    if j.source_is_cache
        && let Some(rec) = depot::read_record(&j.source)
        && rec.build_id == j.build_id
    {
        let mut held = rec.dlcs.unwrap_or_default();
        held.extend(j.adding.iter().map(|a| a.id.to_string()));
        // A record that fails to write only costs the next reuse a check.
        if depot::write_record(&j.source, &j.build_id, &j.language, &held).is_err() {
            let _ = std::fs::remove_file(j.source.join(depot::RECORD));
        }
    }
    Ok(c)
}

/// Settle an interrupted add of `game` (design 19 §3.4): forward (commit) when its
/// files are all there, else back. The caller holds the game's lock. Its content is
/// re-read after a forward recovery.
pub fn recover_locked(
    game: &mut Game,
    reflink: Reflink,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<Option<Direction>, AddError> {
    let root = game.dirs.root.clone();
    // An offline-installer add's journal (design 19 §5) has no files to lay: only
    // running its installers again settles it.
    if let Some(j) = lay::read_journal(&root)?
        && j.lane == super::dlc_installer::LANE
    {
        return Err(AddError::Game(MacAppError::Apps(format!(
            "an earlier DLC add of {} stopped while its installers ran; `gogo linux-app dlc \
             {} add` runs them again",
            game.gogid, game.gogid
        ))));
    }
    // An update's lay (design 21 §11g) shares the journal. Its files come from the
    // depot cache on the target build: a cache that moved since can't finish it,
    // so it goes back.
    // A removal's restored files can come from the cache too (design 19 §3.7).
    let moved = lay::read_journal(&root)?.and_then(|j| {
        if j.is_update() {
            super::update::cache_moved(&j)
        } else if j.is_removal() {
            super::dlc_remove::cache_moved(&j, game.cache_root.as_deref())
        } else {
            None
        }
    });
    let recovered = match moved {
        Some(_) => lay::roll_back(&root, reflink)?,
        None => lay::recover(&root, reflink)?,
    };
    let Some((j, dir)) = recovered else {
        return Ok(None);
    };
    if j.is_update() {
        super::update::settle(game, &j, dir, moved, emit)?;
        return Ok(Some(dir));
    }
    if j.is_removal() {
        super::dlc_remove::settle(game, &j, dir, moved, emit)?;
        return Ok(Some(dir));
    }
    match dir {
        Direction::Forward => {
            game.content = commit(game, &j)?;
            game.derived = false;
            lay::finish(&root)?;
        }
        Direction::Back => {}
    }
    let _ = std::fs::remove_file(root.join(DOWNLOAD));
    emit(&ProgressEvent::DlcRecovered {
        action: dir.as_str().into(),
    });
    Ok(Some(dir))
}

/// [`recover_locked`] under the game's lock, refused while the game runs.
pub fn recover(
    game: &mut Game,
    reflink: Reflink,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<Option<Direction>, AddError> {
    if !journal_exists(&game.dirs.root) {
        return Ok(None);
    }
    let _lock = crate::macapp::relocate::lock_game(&game.gogid)?;
    game::refuse_running(&game.gogid, &game.dirs.root, &game.dirs.prefix)?;
    recover_locked(game, reflink, emit)
}

/// Add `plan`'s DLC to `game` (design 19 §3.2, §3.4). `fetch` downloads with the
/// given inputs into the given source root (the depot seam); `stop` says the user
/// asked to stop (Ctrl-C).
pub async fn add<F>(
    game: &mut Game,
    plan: &AddPlan,
    fetch: F,
    opts: &AddOptions,
    emit: &mut dyn FnMut(&ProgressEvent),
    stop: &dyn Fn() -> bool,
) -> Result<AddOutcome, AddError>
where
    F: AsyncFnOnce(DownloadInputs, PathBuf) -> Result<(), DepotError>,
{
    let root = game.dirs.root.clone();
    // A snapshot rollback that stopped is settled first (design 20 §5.3.1).
    for message in
        super::rollback::settle_pending(&game.gogid, &super::snapshots::Backends::real())?
    {
        emit(&ProgressEvent::Warn { message });
    }
    let lock = crate::macapp::relocate::lock_game(&game.gogid)?;
    game::refuse_running(&game.gogid, &root, &game.dirs.prefix)?;
    let recovered = recover_locked(game, opts.reflink, emit)?;
    let mut warnings = Vec::new();
    // What a recovery already recorded isn't added again.
    let have = game.installed();
    let adding: Vec<AddingDlc> = plan
        .dlcs
        .iter()
        .filter_map(|d| {
            let id = d.id.parse::<u64>().ok()?;
            (!have.contains(&id)).then(|| AddingDlc {
                id,
                title: d.title.clone(),
            })
        })
        .collect();
    // The `auto` snapshot (design 20 §7.1, §9.4): after the recovery and the
    // refusals, before anything is written for the add (the download record,
    // the journal). A resumed download took it on its first try; a failed
    // one stops the add with the game as it was.
    let mut taken = None;
    if !adding.is_empty() && read_download(&root).is_none() {
        let titles: Vec<&str> = adding.iter().map(|a| a.title.as_str()).collect();
        taken = super::snapshots::protect(
            &game.gogid,
            &game.dirs,
            Some(&lock),
            crate::snapshot::Trigger::DlcAdd,
            crate::snapshot::labels::adding(&titles),
            &opts.snapshots,
            emit,
        )?;
    }
    // An older cache record is brought up to date before anything is fetched into
    // the cache (#374): only the record is written, last and durably, and only
    // while every file it will claim is there. A cache that changed since the plan
    // isn't used.
    if let Some(up) = &plan.upgrade {
        let base = game.gogid.clone();
        if !cache_upgrade::apply(&plan.source_root, &base, up)? {
            return Err(AddError::Game(MacAppError::Apps(format!(
                "the depot cache ({}) changed since the plan; run the add again",
                plan.source_root.display()
            ))));
        }
    }
    let mut lay_report = None;
    if !adding.is_empty() {
        emit(&ProgressEvent::DlcPlan {
            dlcs: plan.dlcs.clone(),
            source: plan.source.as_str().into(),
            build_id: plan.build_id.clone(),
            from_cache: plan.from_cache.clone(),
        });
        let staging = plan.source == Source::Staging;
        let src = plan.source_root.clone();
        let mut files: Vec<LayFile> = Vec::new();
        let ids: Vec<u64> = adding.iter().map(|a| a.id).collect();
        // The files of the DLC the cache already holds (#381): laid from it, with
        // no download. Their paths are relative to the cache root, as a download's
        // changed files are.
        let mut paths: Vec<String> = plan
            .cached_files
            .iter()
            .filter(|(id, _)| ids.contains(id))
            .flat_map(|(_, paths)| paths.iter().cloned())
            .collect();
        let fetches = plan
            .downloading()
            .iter()
            .any(|d| ids.contains(&d.id.parse().unwrap_or(0)));
        if fetches {
            // A record for another set of DLC (an add stopped, then other DLC asked for)
            // isn't resumed: its fetched files would be laid unrecorded.
            let mut same = ids.clone();
            same.sort_unstable();
            let prior = read_download(&root).filter(|r| {
                let mut had = r.adding.clone();
                had.sort_unstable();
                r.source == src && r.build_id == plan.build_id && had == same
            });
            let mut rec = match prior {
                Some(r) => r,
                None => {
                    if staging && src.exists() {
                        // A staging dir no (matching) record speaks for is a leftover.
                        materialize::remove_tree(&src)?;
                    }
                    let r = DownloadRecord {
                        build_id: plan.build_id.clone(),
                        source: src.clone(),
                        adding: ids.clone(),
                        before: Snapshot::take(&src),
                        fetched: false,
                    };
                    lay::write_json_durably(&root.join(DOWNLOAD), &r)?;
                    r
                }
            };
            if !rec.fetched {
                std::fs::create_dir_all(&src)?;
                match fetch(plan.inputs.clone(), src.clone()).await {
                    Ok(()) => {}
                    Err(_) if stop() => {
                        return Err(AddError::Interrupted(
                            "download",
                            "the game is as it was; run the add again to resume".into(),
                        ));
                    }
                    Err(e) => return Err(e.into()),
                }
                rec.fetched = true;
                lay::write_json_durably(&root.join(DOWNLOAD), &rec)?;
            }
            paths.extend(Snapshot::take(&src).changed_since(&rec.before));
        }
        if !paths.is_empty() {
            paths.sort();
            paths.dedup();
            files = lay_list(&src, &plan.tree_name, &paths);
        }
        if plan.source == Source::Cache || !fetches {
            let base = if fetches || !plan.from_cache.is_empty() {
                &src
            } else {
                game.cache_root.as_ref().unwrap_or(&src)
            };
            for a in &adding {
                for f in support_files(base, a.id) {
                    if !files.iter().any(|x| x.to == f.to) {
                        files.push(f);
                    }
                }
            }
        }
        let mut j = Journal {
            gogid: game.gogid.clone(),
            lane: "depot".into(),
            build_id: plan.build_id.clone(),
            language: plan.language.clone(),
            adding: adding.clone(),
            source: src.clone(),
            source_is_cache: plan.source == Source::Cache,
            moves: staging && fetches,
            files,
            replaced: Vec::new(),
            dirs: Vec::new(),
            kind: None,
            from: None,
            to: None,
            removed: Vec::new(),
            removing: Vec::new(),
            md5s: Default::default(),
            prune: Vec::new(),
        };
        // A record-only add with nothing to lay writes no journal.
        if !j.files.is_empty() {
            j.survey(&game.dirs.game);
            j.check_links(&game.dirs.game)?;
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
                kind: Some("dlc".into()),
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
            lay_report = Some(report?);
        }
        game.content = commit(game, &j)?;
        game.derived = false;
        lay::finish(&root)?;
        let _ = std::fs::remove_file(root.join(DOWNLOAD));
    }

    if opts.run_setup
        && (!adding.is_empty() || recovered == Some(Direction::Forward))
        && let Err(e) = super::setup::setup(
            &game.gogid,
            &super::setup::SetupOptions {
                refresh: false,
                prompt: None,
                // The add's own snapshot came first.
                runtime_snapshot: None,
            },
            emit,
        )
    {
        warnings.push(format!(
            "the DLC is in place, but setup (GOG's install scripts) failed: {e}; run `gogo \
                 linux-app setup {}`",
            game.gogid
        ));
    }
    for a in &adding {
        emit(&ProgressEvent::DlcInstalled { id: a.id });
    }
    Ok(AddOutcome {
        gogid: game.gogid.clone(),
        build_id: game.content.build_id.clone(),
        language: game.content.language.clone(),
        dlcs: game.content.refs(),
        added: adding
            .iter()
            .map(|a| content::DlcRef {
                id: a.id,
                title: a.title.clone(),
            })
            .collect(),
        recovered,
        lay: lay_report,
        snapshot: taken.as_ref().map(super::snapshots::Taken::snapshot),
        pruned: taken
            .as_ref()
            .map(super::snapshots::Taken::pruned)
            .unwrap_or_default(),
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game_with(dlc: &[(u64, &str)]) -> Game {
        Game {
            gogid: "1423049311".into(),
            dirs: GameDirs::at(PathBuf::from("/nowhere/1423049311")),
            content: Content {
                schema: content::SCHEMA,
                lane: content::Lane::Depot,
                build_id: Some("B25".into()),
                version: None,
                branch: None,
                language: Some("en-US".into()),
                dlc: dlc
                    .iter()
                    .map(|(id, t)| DlcRow {
                        id: *id,
                        title: t.to_string(),
                        build_id: Some("B25".into()),
                        version: Some("2.5".into()),
                        added: None,
                    })
                    .collect(),
            },
            derived: false,
            cache_root: None,
        }
    }

    fn infos() -> Vec<DlcInfo> {
        use crate::gog::dlc::{ManifestDlc, available};
        let catalog: Vec<crate::gog::catalog::Product> = serde_json::from_str(
            r#"[{"id":1256837418,"title":"Phantom Liberty"},{"id":1597316373,"title":"REDmod"},
                {"id":5,"title":"Installer Only","downloads":{"installers":[{"id":"x","files":[]}]}},
                {"id":6,"title":"Hot Garbage"}]"#,
        )
        .unwrap();
        let m = [
            ManifestDlc {
                id: 1256837418,
                download_size: 29 << 30,
                disk_size: 30 << 30,
            },
            ManifestDlc {
                id: 1597316373,
                download_size: 288 << 20,
                disk_size: 605 << 20,
            },
            ManifestDlc {
                id: 6,
                download_size: 0,
                disk_size: 0,
            },
        ];
        available(&catalog, &[1256837418, 1597316373, 5, 6], Some(&m))
    }

    /// Design 19 §1.3: the table and the `--json` shape.
    #[test]
    fn dlc_list_shows_owned_installed_sizes_zero_bytes_and_not_in_build() {
        let g = game_with(&[(1597316373, "REDmod")]);
        let l = listing(&g, &infos(), &[], vec!["a note".into()]);
        let text = render_list(&l);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "1423049311 (depot lane, build B25, en-US)");
        assert!(
            lines[1].starts_with("id") && lines[1].ends_with("disk"),
            "{text}"
        );
        let row = |id: &str| {
            lines
                .iter()
                .find(|l| l.starts_with(id))
                .unwrap()
                .to_string()
        };
        assert!(row("1256837418").contains("no ") && row("1256837418").contains("29.0 GiB"));
        assert!(row("1597316373").contains("yes (2.5)"), "{text}");
        assert!(row("5 ").contains("not in this build"), "{text}");
        assert!(row("6 ").contains("0 B"), "{text}");
        assert_eq!(lines.last().unwrap(), &"note: a note");

        let j = serde_json::to_value(&l).unwrap();
        assert_eq!(j["gogid"], "1423049311");
        assert_eq!(j["lane"], "depot");
        assert_eq!(j["buildId"], "B25");
        assert_eq!(j["language"], "en-US");
        let red = &j["dlcs"][1];
        for k in [
            "id",
            "title",
            "owned",
            "installed",
            "installedBuild",
            "inBuild",
            "downloadSize",
            "diskSize",
        ] {
            assert!(red.get(k).is_some(), "{k} missing: {red}");
        }
        assert_eq!(red["installed"], true);
        assert_eq!(red["installedBuild"], "B25");
        assert_eq!(j["dlcs"][3]["downloadSize"], 0);
        assert_eq!(j["dlcs"][2]["inBuild"], false);
    }

    #[test]
    fn an_installed_dlc_nothing_lists_still_gets_a_row() {
        let g = game_with(&[(42, "Hand Added")]);
        let l = listing(&g, &[], &[42], Vec::new());
        assert_eq!(l.dlcs.len(), 1);
        assert!(l.dlcs[0].installed && l.dlcs[0].owned && !l.dlcs[0].in_build);
    }

    #[test]
    fn plan_splits_installed_names_off_and_marks_record_only_dlc() {
        let g = game_with(&[(1597316373, "REDmod")]);
        let infos = infos();
        let p = plan_add(&PlanInput {
            game: &g,
            title: "Cyberpunk 2077",
            version: Some("2.5".into()),
            newer: None,
            tree_name: "Cyberpunk 2077".into(),
            infos: &infos,
            wanted: &[1597316373, 6],
            cache_upgrade: None,
        });
        assert_eq!(p.already, [1597316373]);
        assert_eq!(p.ids(), [6]);
        assert!(!p.fetches(), "a 0-byte DLC is record only");
        assert_eq!(p.source, Source::Staging);
        assert!(
            plan_lines(&p)
                .iter()
                .any(|l| l.contains("0 B: record only"))
        );
        assert_eq!(p.inputs.old_dlcs, ["1597316373"]);
        assert_eq!(p.inputs.dlcs, ["1597316373", "6"]);
    }

    #[test]
    fn fetched_files_map_to_the_game_and_overlays_win() {
        let root = Path::new("/c");
        let changed: Vec<String> = [
            "Game/archive/pc/ep1/a.archive",
            "Game/goggame-9.info",
            "gog-support/1/9/app/goggame-9.info",
            "gog-support/1/9/setup/installer.exe",
            "elsewhere.txt",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let l = lay_list(root, "Game", &changed);
        let tos: Vec<&str> = l.iter().map(|f| f.to.as_str()).collect();
        assert_eq!(tos, ["archive/pc/ep1/a.archive", "goggame-9.info"]);
        assert_eq!(l[1].from, root.join("gog-support/1/9/app/goggame-9.info"));
    }

    #[test]
    fn a_gone_or_unknown_build_is_refused_naming_the_update() {
        let gone = refusal(&BuildMatch::Gone, Some("B1"), "7", &[1, 2]).unwrap();
        assert!(gone.starts_with("build B1 is no longer served by GOG; update the game first:"));
        assert!(gone.contains("gogo linux-app 7 --dlc 1 --dlc 2 (keeps the prefix, saves and L4)"));
        assert!(
            refusal(&BuildMatch::Unknown, None, "7", &[])
                .unwrap()
                .contains("update the game")
        );
        assert!(refusal(&BuildMatch::Newest, Some("B"), "7", &[]).is_none());
        let older = BuildMatch::Older {
            newest_id: "N".into(),
            newest_version: "2".into(),
        };
        assert!(refusal(&older, Some("B"), "7", &[]).is_none());
    }

    #[test]
    fn committed_records_the_build_language_and_new_rows_once() {
        let base = Content {
            schema: content::SCHEMA,
            lane: content::Lane::Depot,
            build_id: None,
            version: None,
            branch: None,
            language: None,
            dlc: vec![DlcRow {
                id: 1,
                title: "One".into(),
                build_id: None,
                version: None,
                added: None,
            }],
        };
        let add = [
            AddingDlc {
                id: 1,
                title: "One".into(),
            },
            AddingDlc {
                id: 2,
                title: "Two".into(),
            },
        ];
        let c = committed(&base, "B", "de-DE", &add);
        assert_eq!(c.build_id.as_deref(), Some("B"));
        assert_eq!(c.language.as_deref(), Some("de-DE"));
        assert_eq!(c.dlc.len(), 2);
        assert_eq!(c.dlc[1].build_id.as_deref(), Some("B"));
        assert!(c.dlc[1].added.is_some());
    }
}
