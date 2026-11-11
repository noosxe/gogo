//! Depot (Model B) downloads via gog-warp — `docs/design/14`.
//!
//! Thin driver over gog-warp's `Downloader`: resolve the latest public Windows
//! build, plan, and download the game tree into the user cache directory
//! (macOS and Linux). Turning that tree into a game's `game/` dir is
//! `macapp::bundle::clone_tree` on macOS and [`super::materialize`] on Linux.
//! The heavy lifting (depots, secure links, chunk reconstruction, verification)
//! is the library's; this module owns selection, layout, events, and limits.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tokio_util::sync::CancellationToken;

use gog_warp::Core;
use gog_warp::content_system::downloader::progress::DownloadState;
use gog_warp::content_system::downloader::{Builder, Downloader};
use gog_warp::content_system::types::{Build, Manifest, Platform};

use super::dlc::{self, DlcSelectError, DlcSet, Lane, Licence};
use super::materialize::MaterializePlan;

/// Progress surface for the depot lane (design 14 D4); NDJSON under
/// `--json-progress`.
#[derive(Serialize, Clone, Debug)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DepotEvent {
    Resolved {
        product_id: u64,
        title: String,
        build_id: String,
        version: String,
        generation: u32,
    },
    Plan {
        install_dir: PathBuf,
        download_size: u64,
        disk_size: u64,
        language: String,
        /// The DLC the run takes (design 19), with their depot bytes.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        dlcs: Vec<PlanDlc>,
        /// What the run does with the cache already there, when it isn't
        /// plain (design 19 §3.3): which DLC it fetches, or why it checks.
        #[serde(skip_serializing_if = "Option::is_none")]
        cache: Option<String>,
        /// Linux only: how a build will turn the cache tree into
        /// `games/<gogid>/game` (reflink or copy) and the extra disk it
        /// costs (design 18 §6). macOS clones through APFS and has none.
        #[serde(skip_serializing_if = "Option::is_none")]
        game_files: Option<MaterializePlan>,
    },
    Preparing,
    Allocating {
        fraction: f32,
    },
    Downloading {
        downloaded: u64,
        written: u64,
        total_download: u64,
        total_size: u64,
    },
    Verifying {
        fraction: f32,
    },
    Finished {
        install_dir: PathBuf,
        /// Files the run wrote (new, or changed since it started) and their
        /// bytes, from the cache root before and after (#337): all of them
        /// for a cold download, what was missing for a repair.
        fetched_files: u64,
        fetched_bytes: u64,
    },
}

/// One DLC in the plan: what `--json-progress` and the plan line show.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlanDlc {
    pub id: String,
    pub title: String,
    pub download_size: u64,
    pub disk_size: u64,
}

#[derive(thiserror::Error, Debug)]
pub enum DepotError {
    /// A DLC selection error (design 19 §2.2): a usage error, exit 2.
    #[error(transparent)]
    Dlc(#[from] DlcSelectError),
    #[error("no public Windows builds for product {0}")]
    NoBuilds(u64),
    #[error("no user cache directory available")]
    NoCacheDir,
    #[error("{0}")]
    Library(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl DepotError {
    /// Downgrade a gog-warp error into our taxonomy. Debug, not Display:
    /// their io variants hide the source chain in Display ("io error").
    pub fn library(e: impl std::fmt::Debug) -> Self {
        DepotError::Library(format!("{e:?}"))
    }
}

/// What [`run`] needs to name and check DLC (design 19 §2): the account's owned
/// ids and the game's catalog DLC (titles).
#[derive(Clone, Default)]
pub struct DlcContext {
    pub owned: Vec<u64>,
    pub catalog_dlcs: Vec<super::catalog::Product>,
}

/// Driver options (design 14 §1); owned by the CLI layer.
#[derive(Clone)]
pub struct DepotOptions {
    pub language: String,
    /// Which DLC to take; `Ids`/`AllOwned` need `dlc_context`.
    pub dlcs: DlcSet,
    pub dlc_context: DlcContext,
    pub password: Option<String>,
    /// Replaces `<cache>/gogo/depot/<id>` as the install root.
    pub dir: Option<PathBuf>,
    /// The library a build of this game would go to, for the plan's cost.
    pub library: Option<crate::macapp::launch::Library>,
    /// Bytes/sec; `None` = unlimited.
    pub limit: Option<u64>,
    pub disk_check: bool,
    pub dry_run: bool,
}

impl Default for DepotOptions {
    fn default() -> Self {
        Self {
            language: "en-US".into(),
            dlcs: DlcSet::None,
            dlc_context: DlcContext::default(),
            password: None,
            dir: None,
            library: None,
            limit: None,
            disk_check: true,
            dry_run: false,
        }
    }
}

/// Pure: pick the newest public build, tiebreaking on `build_id`
/// (design 14 D3). `get_builds` already filters by platform.
pub fn pick_build<'a>(items: impl IntoIterator<Item = &'a Build>) -> Option<&'a Build> {
    items.into_iter().filter(|b| *b.public()).max_by(|a, b| {
        a.date_published()
            .cmp(b.date_published())
            .then_with(|| a.build_id().cmp(b.build_id()))
    })
}

/// How a built game's build stands against the builds GOG lists (design 19 §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildMatch {
    /// The installed build is GOG's newest public one.
    Newest,
    /// Still listed; a newer public build exists.
    Older {
        newest_id: String,
        newest_version: String,
    },
    /// No longer listed: its manifest can't be fetched.
    Gone,
    /// The game's build isn't known.
    Unknown,
}

/// Pure: match the installed build `installed` against GOG's `items` (design 19 §4).
/// The listed build is returned with the answer when there is one.
pub fn match_build<'a>(
    installed: Option<&str>,
    items: &'a [Build],
) -> (BuildMatch, Option<&'a Build>) {
    let Some(b) = installed else {
        return (BuildMatch::Unknown, None);
    };
    let Some(found) = items.iter().find(|x| x.build_id() == b) else {
        return (BuildMatch::Gone, None);
    };
    match pick_build(items) {
        Some(newest) if newest.build_id() != b => (
            BuildMatch::Older {
                newest_id: newest.build_id().to_string(),
                newest_version: newest.version_name().to_string(),
            },
            Some(found),
        ),
        _ => (BuildMatch::Newest, Some(found)),
    }
}

/// The installed build as GOG serves it: its match, and when it is listed, its
/// version name, generation and manifest.
pub struct InstalledBuild {
    pub matched: BuildMatch,
    pub listed: Option<(String, u32, Manifest)>,
    /// The newest public build's id, for sizes when the installed one is gone.
    pub newest: Option<(String, Manifest)>,
}

/// Fetch the build list and, for the installed build (else the newest, so a
/// listing still has sizes), its manifest. No depot manifest is fetched.
pub async fn installed_build(
    core: &Core,
    product_id: u64,
    build_id: Option<&str>,
) -> Result<InstalledBuild, DepotError> {
    let builds = core
        .get_builds(&product_id.to_string(), Platform::Windows, None)
        .await
        .map_err(DepotError::library)?;
    let items: Vec<Build> = builds.items().to_vec();
    let (matched, found) = match_build(build_id, &items);
    let listed = match found {
        Some(b) => Some((
            b.version_name().to_string(),
            *b.generation(),
            core.get_manifest(b).await.map_err(DepotError::library)?,
        )),
        None => None,
    };
    let newest = match (&listed, pick_build(&items)) {
        (None, Some(b)) => Some((
            b.build_id().to_string(),
            core.get_manifest(b).await.map_err(DepotError::library)?,
        )),
        _ => None,
    };
    Ok(InstalledBuild {
        matched,
        listed,
        newest,
    })
}

/// Whether GOG publishes a public Windows build for `product_id`, i.e.
/// whether the depot lane can serve it (`gogo mac-app`'s default, #73).
pub async fn has_public_build(core: &Core, product_id: u64) -> Result<bool, DepotError> {
    let builds = core
        .get_builds(&product_id.to_string(), Platform::Windows, None)
        .await
        .map_err(DepotError::library)?;
    Ok(pick_build(builds.items()).is_some())
}

/// Map an IETF code to the plain name the v1 (legacy XML) content system
/// expects for `old_language`; unknown codes fall back to English.
pub fn v1_language(code: &str) -> String {
    match code.to_ascii_lowercase().as_str() {
        "en-us" | "en" => "English",
        "fr-fr" | "fr" => "French",
        "de-de" | "de" => "German",
        "es-es" | "es" => "Spanish",
        "it-it" | "it" => "Italian",
        "pl-pl" | "pl" => "Polish",
        "ru-ru" | "ru" => "Russian",
        "pt-br" | "pt" => "Portuguese",
        "nl-nl" | "nl" => "Dutch",
        _ => "English",
    }
    .to_string()
}

/// Pure: `DownloadState` → `DepotEvent` (design 14 D4). The library's own
/// `Finished` is skipped — we emit ours (with the install dir) after
/// `download()` resolves.
pub fn state_event(s: DownloadState) -> Option<DepotEvent> {
    match s {
        DownloadState::Preparing => Some(DepotEvent::Preparing),
        DownloadState::Allocating(f) => Some(DepotEvent::Allocating { fraction: f }),
        // gog-warp's units differ: `Allocating` is a 0..=1 fraction but
        // `Verifying` is a 0..=100 percentage. `DepotEvent` fractions are 0..=1.
        DownloadState::Verifying(p) => Some(DepotEvent::Verifying {
            fraction: p / 100.0,
        }),
        DownloadState::Downloading(p) => Some(DepotEvent::Downloading {
            downloaded: p.downloaded,
            written: p.written,
            total_download: p.total_download,
            total_size: p.total_size,
        }),
        DownloadState::Finished => None,
    }
}

/// The user-level cache base (`~/Library/Caches` on macOS; XDG on Linux),
/// or `None` where the platform has none.
pub fn default_cache_dir() -> Option<PathBuf> {
    dirs::cache_dir()
}

/// Cache roots (design 14 D2): `(install_root, support_root, global_deps_root)`.
/// `None` when no user cache directory exists on this platform.
pub fn cache_roots(
    cache_base: Option<&Path>,
    product_id: u64,
    override_dir: Option<&Path>,
) -> Option<(PathBuf, PathBuf, PathBuf)> {
    let base = override_dir.map(|p| p.to_path_buf()).or_else(|| {
        cache_base.map(|c| c.join("gogo").join("depot").join(product_id.to_string()))
    })?;
    let support = base.join("gog-support");
    let deps = cache_base?.join("gogo").join("dependencies");
    Some((base, support, deps))
}

/// The newest public build's id and its manifest's DLC with sizes, for naming DLC
/// before a run (design 19 §1.3). No depot manifest is fetched.
pub async fn build_dlcs(
    core: &Core,
    product_id: u64,
    password: Option<String>,
    language: &str,
) -> Result<(String, Vec<dlc::ManifestDlc>), DepotError> {
    let builds = core
        .get_builds(&product_id.to_string(), Platform::Windows, password)
        .await
        .map_err(DepotError::library)?;
    let build = pick_build(builds.items()).ok_or(DepotError::NoBuilds(product_id))?;
    let manifest = core
        .get_manifest(build)
        .await
        .map_err(DepotError::library)?;
    Ok((
        build.build_id().to_string(),
        dlc::manifest_dlcs(&manifest, language),
    ))
}

/// Full lane (design 14 D5). Emits events into `events`; returns the realized
/// install directory. Stops after `Plan` when `opts.dry_run`.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    core: &Core,
    product_id: u64,
    title: &str,
    opts: &DepotOptions,
    events: &tokio::sync::mpsc::UnboundedSender<DepotEvent>,
    cancel: &CancellationToken,
    free_space: &(dyn crate::engine::FreeSpace + Send + Sync),
) -> Result<PathBuf, DepotError> {
    let cache_base = dirs::cache_dir();
    let (install_root, support_root, deps_root) =
        cache_roots(cache_base.as_deref(), product_id, opts.dir.as_deref())
            .ok_or(DepotError::NoCacheDir)?;

    let builds = core
        .get_builds(
            &product_id.to_string(),
            Platform::Windows,
            opts.password.clone(),
        )
        .await
        .map_err(DepotError::library)?;
    let build = pick_build(builds.items()).ok_or(DepotError::NoBuilds(product_id))?;
    let build_id = build.build_id().to_string();
    let generation = *build.generation();
    let _ = events.send(DepotEvent::Resolved {
        product_id,
        title: title.to_string(),
        version: build.version_name().to_string(),
        build_id: build_id.clone(),
        generation,
    });

    let manifest = core
        .get_manifest(build)
        .await
        .map_err(DepotError::library)?;
    let mdlcs = dlc::manifest_dlcs(&manifest, &opts.language);
    let infos = dlc::available(
        &opts.dlc_context.catalog_dlcs,
        &opts.dlc_context.owned,
        Some(&mdlcs),
    );
    let lane = Lane::Depot {
        build_id: build_id.clone(),
    };
    let chosen = dlc::select(&opts.dlcs, title, &infos, &lane)?;
    let dlcs: Vec<String> = chosen.iter().map(u64::to_string).collect();
    let plan_dlcs: Vec<PlanDlc> = chosen
        .iter()
        .filter_map(|id| infos.iter().find(|d| d.id == *id))
        .map(|d| PlanDlc {
            id: d.id.to_string(),
            title: d.title.clone(),
            download_size: d.download_size.unwrap_or(0),
            disk_size: d.disk_size.unwrap_or(0),
        })
        .collect();
    let (download_size, disk_size) = manifest.install_size(&opts.language, &dlcs, None);
    // What the cache already there holds decides how it's completed (design 19 §3.3).
    let prior = load_record(&install_root);
    let want = CacheWant {
        build_id: Some(&build_id),
        language: Some(&opts.language),
        dlcs: &dlcs,
    };
    let check = prior.as_ref().map_or(CacheCheck::Unrecorded, |r| {
        check_record(&install_root, r, &want)
    });
    let inputs = download_inputs(
        generation,
        &opts.language,
        &dlcs,
        &check,
        prior
            .as_ref()
            .and_then(|r| r.dlcs.as_deref())
            .unwrap_or(&[]),
    );
    let install_dir = install_root.join(manifest.install_directory());
    let game_files = cfg!(target_os = "linux").then(|| {
        let game_dir = crate::macapp::launch::GameDirs::for_build(
            &product_id.to_string(),
            opts.library.as_ref(),
        )
        .map(|d| d.game)
        .unwrap_or_else(|_| {
            crate::macapp::launch::games_dir()
                .join(product_id.to_string())
                .join("game")
        });
        super::materialize::plan_tree(&install_root, &game_dir, disk_size)
    });
    let _ = events.send(DepotEvent::Plan {
        install_dir: install_dir.clone(),
        download_size,
        disk_size,
        language: opts.language.clone(),
        dlcs: plan_dlcs,
        cache: cache_note(&check, &inputs),
        game_files,
    });
    if opts.dry_run {
        return Ok(install_dir);
    }

    // A stale owned set can disagree with the server: GOG refuses a DLC's secure
    // link with 403 `invalid_licence` (design 19 §2.2). Ask before any file is
    // fetched; only that verdict is read, never the response body.
    if let Some(id) = first_unowned(core, &chosen, generation, &manifest).await? {
        let mut stale = infos.clone();
        for d in stale.iter_mut().filter(|d| d.id == id) {
            d.owned = false;
        }
        return Err(dlc::check_id(id, title, &stale, &lane)
            .expect_err("an unowned DLC is refused")
            .into());
    }

    let (fetched_files, fetched_bytes) = fetch_into(
        core,
        &FetchSpec {
            manifest: &manifest,
            build_id: &build_id,
            inputs: &inputs,
            install_root: &install_root,
            support_root: &support_root,
            deps_root: &deps_root,
            limit: opts.limit,
            disk_check: opts.disk_check,
        },
        events,
        cancel,
        free_space,
    )
    .await?;
    // The download is done either way; without a record (a full disk, say)
    // the next reuse just checks it again.
    let held = record_dlcs(prior.as_ref(), &build_id, &opts.language, &dlcs);
    if write_record(&install_root, &build_id, &opts.language, &held).is_err() {
        let _ = std::fs::remove_file(install_root.join(RECORD));
    }
    let _ = events.send(DepotEvent::Finished {
        install_dir: install_dir.clone(),
        fetched_files,
        fetched_bytes,
    });
    Ok(install_dir)
}

/// The first of `ids` whose secure link GOG refuses as not owned (design 19 §2.2),
/// asked before any file is fetched. Only the verdict is read, never a body.
pub async fn first_unowned(
    core: &Core,
    ids: &[u64],
    generation: u32,
    manifest: &Manifest,
) -> Result<Option<u64>, DepotError> {
    if ids.is_empty() {
        return Ok(None);
    }
    let token = core
        .obtain_galaxy_token()
        .await
        .map_err(DepotError::library)?;
    for id in ids {
        let verdict = dlc::probe_licence(
            core.reqwest_client(),
            dlc::CONTENT_SYSTEM_BASE,
            token.access_token(),
            *id,
            generation,
            manifest.repository_timestamp(),
        )
        .await;
        if verdict == Licence::NotOwned {
            return Ok(Some(*id));
        }
    }
    Ok(None)
}

/// What [`fetch_into`] downloads, and where.
pub struct FetchSpec<'a> {
    /// The build being downloaded; also the old one for a targeted run.
    pub manifest: &'a Manifest,
    pub build_id: &'a str,
    pub inputs: &'a DownloadInputs,
    pub install_root: &'a Path,
    pub support_root: &'a Path,
    pub deps_root: &'a Path,
    /// Bytes/sec; `None` = unlimited.
    pub limit: Option<u64>,
    pub disk_check: bool,
}

/// Run gog-warp's downloader for `spec` into its install root, forwarding its
/// progress and `cancel`; `(files, bytes)` it wrote (#337). The shared part of
/// [`run`] and `linux-app dlc add` (design 19 §3.2).
pub async fn fetch_into(
    core: &Core,
    spec: &FetchSpec<'_>,
    events: &tokio::sync::mpsc::UnboundedSender<DepotEvent>,
    cancel: &CancellationToken,
    free_space: &(dyn crate::engine::FreeSpace + Send + Sync),
) -> Result<(u64, u64), DepotError> {
    // The preflight (and the downloader) expect the install root to exist;
    // nothing creates it before this point (live finding 2026-10-20: the
    // free-space probe on a missing path is ENOENT, not 0 free).
    std::fs::create_dir_all(spec.install_root).map_err(DepotError::Io)?;
    let deps_manifest = core
        .get_dependencies_manifest()
        .await
        .map_err(DepotError::library)?;

    let builder = Downloader::builder()
        .core(core.clone())
        .manifest(spec.manifest.clone(), spec.build_id)
        .install_root(spec.install_root.to_path_buf())
        .support_root(spec.support_root.to_path_buf())
        .global_dependencies_root(spec.deps_root.to_path_buf())
        .game_dependencies(deps_manifest);
    let builder = spec.inputs.apply(builder, spec.manifest, spec.build_id);
    let mut downloader = builder.build().map_err(DepotError::library)?;

    if let Some(bps) = spec.limit {
        let clamped = bps.min(i32::MAX as u64) as i32;
        downloader.set_max_speed(clamped).await;
    }

    // Forward the downloader's states (take-once receiver) and our ctrl-c.
    if let Some(mut rx) = downloader.take_progress_receiver() {
        let events = events.clone();
        tokio::spawn(async move {
            while let Some(state) = rx.recv().await {
                if let Some(event) = state_event(state) {
                    let _ = events.send(event);
                }
            }
        });
    }
    {
        let dl_cancel = downloader.get_cancellation();
        let ours = cancel.clone();
        tokio::spawn(async move {
            ours.cancelled().await;
            dl_cancel.cancel();
        });
    }

    downloader.prepare().await.map_err(DepotError::library)?;

    if spec.disk_check {
        let required = downloader
            .get_required_space()
            .await
            .map_err(DepotError::library)?
            .max(0) as u64;
        let free = free_space
            .available_bytes(spec.install_root)
            .map_err(DepotError::Io)?;
        if required > free {
            return Err(DepotError::Library(format!(
                "insufficient space: need {required} bytes, have {free}"
            )));
        }
    }

    let before = Snapshot::take(spec.install_root);
    downloader.download().await.map_err(DepotError::library)?;
    Ok(Snapshot::take(spec.install_root).fetched_since(&before))
}

/// The record of a finished download, in the cache root next to the game
/// tree and `gog-support/` (design 14 D2): every file and link the download
/// left there. A reused cache is checked against it, so a tree that lost
/// files since (a part of `gog-support/` deleted, say) isn't taken as whole.
pub const RECORD: &str = ".gogo-depot.json";

/// gog-warp's own marker of a download in progress; gone when one finishes.
const WARP_MARKER: &str = ".gog-warp-build";
/// gog-warp's temp dir for updates, under the cache root.
const WARP_TEMP: &str = "!Temp";

#[derive(Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    build_id: String,
    /// The language the download asked for; absent in records an older gogo wrote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    /// DLC product ids the cache holds, sorted; absent in records an older gogo
    /// wrote, which can't say what the tree holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dlcs: Option<Vec<String>>,
    /// Paths relative to the cache root, `/`-separated, sorted.
    files: Vec<String>,
}

/// What a cache root's record says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheCheck {
    /// Everything the download left is still there, and it holds what's wanted.
    Complete,
    /// No record: an older gogo downloaded it, or the download never
    /// finished. Whether it's whole is unknown.
    Unrecorded,
    /// These recorded paths are gone (at most [`MISSING_SHOWN`] of them,
    /// plus the count of all).
    Missing { shown: Vec<String>, count: usize },
    /// The record predates DLC and language tracking, so what the tree holds
    /// can't be told without hashing it.
    UnknownContent,
    /// These wanted DLC (product ids) aren't in the cache.
    MissingDlc { missing: Vec<String> },
    /// The cache was downloaded in another language.
    OtherLanguage,
    /// The cache holds another build than the one wanted.
    OtherBuild,
}

/// What a caller wants from a cache; a `None` field isn't checked.
#[derive(Debug, Clone, Copy, Default)]
pub struct CacheWant<'a> {
    pub build_id: Option<&'a str>,
    pub language: Option<&'a str>,
    /// DLC product ids.
    pub dlcs: &'a [String],
}

/// How many missing paths [`CacheCheck::Missing`] names.
pub const MISSING_SHOWN: usize = 5;

/// Record what's in `root` after a finished download (overwrites the last).
/// `dlcs` are the product ids the cache holds.
pub fn write_record(
    root: &Path,
    build_id: &str,
    language: &str,
    dlcs: &[String],
) -> std::io::Result<()> {
    let mut files = Vec::new();
    walk(root, root, &mut files)?;
    files.sort();
    let mut dlcs = dlcs.to_vec();
    dlcs.sort();
    dlcs.dedup();
    let rec = Record {
        build_id: build_id.to_string(),
        language: Some(language.to_string()),
        dlcs: Some(dlcs),
        files,
    };
    let text = serde_json::to_string_pretty(&rec).map_err(std::io::Error::other)?;
    let tmp = root.join(format!("{RECORD}.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, root.join(RECORD))
}

/// The DLC a new record lists: those the old record had, when it was for the
/// same build and language (a completion or repair keeps them), plus `wanted`.
fn record_dlcs(
    prior: Option<&Record>,
    build_id: &str,
    language: &str,
    wanted: &[String],
) -> Vec<String> {
    let mut all: Vec<String> = prior
        .filter(|r| r.build_id == build_id && r.language.as_deref() == Some(language))
        .and_then(|r| r.dlcs.clone())
        .unwrap_or_default();
    all.extend(wanted.iter().cloned());
    all.sort();
    all.dedup();
    all
}

/// Files and links under `dir`, relative to `root`; links aren't followed.
fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(root).expect("under root");
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if dir == root
            && (name == RECORD
                || name == WARP_MARKER
                || name == format!("{RECORD}.tmp")
                || name == WARP_TEMP)
        {
            continue;
        }
        // Not the download's: Finder's, and gog-warp's own leftovers.
        if name == ".DS_Store" || name.ends_with(".download") || name.ends_with(".state") {
            continue;
        }
        if entry.file_type()?.is_dir() {
            walk(root, &path, out)?;
        } else {
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            out.push(parts.join("/"));
        }
    }
    Ok(())
}

/// Size and mtime of every file [`walk`] finds under a cache root, to tell
/// what a run wrote (#337). gog-warp writes a file as `<name>.download` and
/// renames it, so a fetched file is new or has a new mtime.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Snapshot(std::collections::HashMap<String, (u64, Option<std::time::SystemTime>)>);

impl Snapshot {
    /// An unreadable or missing root is empty.
    pub fn take(root: &Path) -> Self {
        let mut files = Vec::new();
        let _ = walk(root, root, &mut files);
        Self(
            files
                .into_iter()
                .filter_map(|f| {
                    let meta = root.join(&f).symlink_metadata().ok()?;
                    Some((f, (meta.len(), meta.modified().ok())))
                })
                .collect(),
        )
    }

    /// `(files, bytes)` in `self` that `before` lacks or had otherwise.
    pub fn fetched_since(&self, before: &Snapshot) -> (u64, u64) {
        self.0
            .iter()
            .filter(|(f, now)| before.0.get(*f) != Some(now))
            .fold((0, 0), |(n, b), (_, (len, _))| (n + 1, b + len))
    }

    /// The paths (relative to the root, `/`-separated, sorted) in `self` that
    /// `before` lacks or had otherwise: what a run wrote (design 19 §3.2).
    pub fn changed_since(&self, before: &Snapshot) -> Vec<String> {
        let mut out: Vec<String> = self
            .0
            .iter()
            .filter(|(f, now)| before.0.get(*f) != Some(now))
            .map(|(f, _)| f.clone())
            .collect();
        out.sort();
        out
    }
}

/// What a cache root's record says about its download (design 19 §6.1's fallback).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordSummary {
    pub build_id: String,
    pub language: Option<String>,
    /// DLC product ids; `None` for a record from before DLC tracking.
    pub dlcs: Option<Vec<String>>,
}

/// The record in cache root `root`, if there is a readable one.
pub fn read_record(root: &Path) -> Option<RecordSummary> {
    let rec = load_record(root)?;
    Some(RecordSummary {
        build_id: rec.build_id,
        language: rec.language,
        dlcs: rec.dlcs,
    })
}

fn load_record(root: &Path) -> Option<Record> {
    let text = std::fs::read_to_string(root.join(RECORD)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Check the cache root `root` against its record and `want`. An unreadable
/// record counts as none.
pub fn check_cache(root: &Path, want: &CacheWant<'_>) -> CacheCheck {
    match load_record(root) {
        Some(rec) => check_record(root, &rec, want),
        None => CacheCheck::Unrecorded,
    }
}

fn check_record(root: &Path, rec: &Record, want: &CacheWant<'_>) -> CacheCheck {
    let missing: Vec<String> = rec
        .files
        .iter()
        .filter(|f| root.join(f).symlink_metadata().is_err())
        .cloned()
        .collect();
    if !missing.is_empty() {
        return CacheCheck::Missing {
            count: missing.len(),
            shown: missing.into_iter().take(MISSING_SHOWN).collect(),
        };
    }
    if want.build_id.is_some_and(|b| b != rec.build_id) {
        return CacheCheck::OtherBuild;
    }
    // A record from before DLC tracking only matters to a caller that wants
    // DLC or a language: without them it holds all they ask for.
    if (rec.dlcs.is_none() && !want.dlcs.is_empty())
        || (rec.language.is_none() && want.language.is_some())
    {
        return CacheCheck::UnknownContent;
    }
    if let (Some(have), Some(wanted)) = (&rec.language, want.language)
        && !have.eq_ignore_ascii_case(wanted)
    {
        return CacheCheck::OtherLanguage;
    }
    let held = rec.dlcs.as_deref().unwrap_or(&[]);
    let lacking: Vec<String> = want
        .dlcs
        .iter()
        .filter(|d| !held.contains(d))
        .cloned()
        .collect();
    if lacking.is_empty() {
        CacheCheck::Complete
    } else {
        CacheCheck::MissingDlc { missing: lacking }
    }
}

/// What the downloader is told for a run (design 19 §3.2/§3.3): kept apart
/// from the builder so the choice is testable without a network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadInputs {
    /// Diff against the same build with the recorded DLC (no hashing), not a
    /// full `verify`.
    pub targeted: bool,
    /// `None`: the library's default (generation 1 names it by `old_language`).
    pub language: Option<String>,
    pub old_language: Option<String>,
    pub dlcs: Vec<String>,
    pub old_dlcs: Vec<String>,
}

/// Pure: pick how a run completes the cache. Only a cache of the same build and
/// language that lacks wanted DLC is completed the targeted way, and only on
/// generation 2 (v1's DLC are named by the old-style options and diffed by the
/// library differently); everything else is a full `verify` with the wanted set,
/// which keeps what's valid and fetches the rest.
pub fn download_inputs(
    generation: u32,
    language: &str,
    wanted: &[String],
    check: &CacheCheck,
    recorded: &[String],
) -> DownloadInputs {
    if generation != 1 && matches!(check, CacheCheck::MissingDlc { .. }) {
        let mut all: Vec<String> = recorded.iter().chain(wanted).cloned().collect();
        all.sort();
        all.dedup();
        return DownloadInputs {
            targeted: true,
            language: Some(language.to_string()),
            old_language: Some(language.to_string()),
            dlcs: all,
            old_dlcs: recorded.to_vec(),
        };
    }
    if generation == 1 {
        DownloadInputs {
            targeted: false,
            language: None,
            old_language: Some(v1_language(language)),
            dlcs: Vec::new(),
            old_dlcs: wanted.to_vec(),
        }
    } else {
        DownloadInputs {
            targeted: false,
            language: Some(language.to_string()),
            old_language: None,
            dlcs: wanted.to_vec(),
            old_dlcs: Vec::new(),
        }
    }
}

/// Pure: the inputs of `linux-app dlc add` (design 19 §3.2): the installed build
/// diffed against itself, `old_dlcs` the installed DLC, `dlcs` those plus the
/// wanted ones, the recorded language on both sides, no `verify`.
pub fn add_inputs(language: &str, installed: &[String], wanted: &[String]) -> DownloadInputs {
    download_inputs(
        2,
        language,
        wanted,
        &CacheCheck::MissingDlc {
            missing: wanted.to_vec(),
        },
        installed,
    )
}

impl DownloadInputs {
    /// Set them on the builder: `manifest` is the build being downloaded, also
    /// the old one for a targeted run.
    pub fn apply(&self, mut b: Builder, manifest: &Manifest, build_id: &str) -> Builder {
        if self.targeted {
            b = b.upgrade_from(manifest.clone(), build_id);
        } else {
            b = b.verify();
        }
        if let Some(l) = &self.language {
            b = b.language(l.clone());
        }
        if let Some(l) = &self.old_language {
            b = b.old_language(l.clone());
        }
        if !self.dlcs.is_empty() {
            b = b.dlcs(self.dlcs.clone());
        }
        if !self.old_dlcs.is_empty() {
            b = b.old_dlcs(self.old_dlcs.clone());
        }
        b
    }
}

/// The plan line's words on what happens to the cache (`None`: nothing to say).
fn cache_note(check: &CacheCheck, inputs: &DownloadInputs) -> Option<String> {
    Some(match check {
        CacheCheck::Complete | CacheCheck::Unrecorded => return None,
        CacheCheck::Missing { count, .. } => {
            format!("cache lost {count} file(s): checking it (hashing) and fetching them")
        }
        CacheCheck::UnknownContent => "cache record predates DLC tracking: checking it \
             (hashing) and fetching what's missing"
            .into(),
        CacheCheck::MissingDlc { missing } if inputs.targeted => format!(
            "cache lacks DLC {}: fetching only those (no hashing)",
            missing.join(", ")
        ),
        CacheCheck::MissingDlc { missing } => format!(
            "cache lacks DLC {}: checking it (hashing) and fetching them",
            missing.join(", ")
        ),
        CacheCheck::OtherLanguage => {
            "cache holds another language: checking it (hashing) and fetching the rest".into()
        }
        CacheCheck::OtherBuild => {
            "cache holds another build: checking it (hashing) and fetching the new one".into()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_round_trip_and_missing_files() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        assert_eq!(
            check_cache(root, &CacheWant::default()),
            CacheCheck::Unrecorded
        );
        std::fs::create_dir_all(root.join("Game/Apps")).unwrap();
        std::fs::create_dir_all(root.join("gog-support/1/1/app")).unwrap();
        std::fs::write(root.join("Game/Apps/game.exe"), b"x").unwrap();
        std::fs::write(root.join("gog-support/1/1/app/game.ini"), b"y").unwrap();
        std::fs::write(root.join(WARP_MARKER), b"b1").unwrap();
        std::fs::write(root.join("Game/x.bin.download"), b"").unwrap();
        std::fs::write(root.join("Game/.DS_Store"), b"").unwrap();
        write_record(root, "b1", "en-US", &[]).unwrap();

        let rec: Record =
            serde_json::from_str(&std::fs::read_to_string(root.join(RECORD)).unwrap()).unwrap();
        assert_eq!(rec.build_id, "b1");
        assert_eq!(
            rec.files,
            ["Game/Apps/game.exe", "gog-support/1/1/app/game.ini"]
        );
        assert_eq!(
            check_cache(root, &CacheWant::default()),
            CacheCheck::Complete
        );

        std::fs::remove_dir_all(root.join("gog-support")).unwrap();
        assert_eq!(
            check_cache(root, &CacheWant::default()),
            CacheCheck::Missing {
                shown: vec!["gog-support/1/1/app/game.ini".into()],
                count: 1
            }
        );
    }

    #[test]
    fn snapshot_counts_new_and_rewritten_files() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::create_dir_all(root.join("Game")).unwrap();
        std::fs::write(root.join("Game/kept"), b"kept").unwrap();
        std::fs::write(root.join("Game/rewritten"), b"old").unwrap();
        let before = Snapshot::take(root);
        assert_eq!(Snapshot::take(root).fetched_since(&before), (0, 0));

        std::fs::write(root.join("Game/new"), b"12345").unwrap();
        std::fs::write(root.join("Game/rewritten"), b"newer").unwrap();
        std::fs::write(root.join("Game/x.download"), b"partial").unwrap();
        assert_eq!(Snapshot::take(root).fetched_since(&before), (2, 10));
        assert_eq!(
            Snapshot::take(root).fetched_since(&Snapshot::take(&root.join("nowhere"))),
            (3, 14)
        );
    }

    #[test]
    fn unreadable_record_is_unrecorded() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join(RECORD), "not json").unwrap();
        assert_eq!(
            check_cache(t.path(), &CacheWant::default()),
            CacheCheck::Unrecorded
        );
    }

    #[cfg(unix)]
    #[test]
    fn links_are_recorded_not_followed() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::create_dir_all(root.join("Game")).unwrap();
        std::os::unix::fs::symlink("nowhere", root.join("Game/link")).unwrap();
        write_record(root, "b", "en-US", &[]).unwrap();
        assert_eq!(
            check_cache(root, &CacheWant::default()),
            CacheCheck::Complete
        );
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// A cache root with one game file, recorded as `(build, language, dlcs)`.
    fn recorded(build: &str, language: &str, dlcs: &[&str]) -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("Game")).unwrap();
        std::fs::write(t.path().join("Game/a.bin"), b"x").unwrap();
        write_record(t.path(), build, language, &ids(dlcs)).unwrap();
        t
    }

    #[test]
    fn record_round_trips_dlcs_and_language_and_reads_old_records() {
        let t = recorded("b1", "de-DE", &["9", "3", "9"]);
        let rec = load_record(t.path()).unwrap();
        assert_eq!(rec.language.as_deref(), Some("de-DE"));
        assert_eq!(rec.dlcs, Some(ids(&["3", "9"])));

        // An older gogo's record: no `language`, no `dlcs`.
        std::fs::write(
            t.path().join(RECORD),
            r#"{"buildId":"b1","files":["Game/a.bin"]}"#,
        )
        .unwrap();
        let old = load_record(t.path()).unwrap();
        assert_eq!((old.language, old.dlcs), (None, None));
    }

    #[test]
    fn check_cache_answers() {
        let w = ids(&["5"]);
        let both = ids(&["5", "7"]);
        let want = |build, language, dlcs| CacheWant {
            build_id: build,
            language,
            dlcs,
        };

        // Complete, also with extra DLC in the cache.
        let t = recorded("b1", "en-US", &["5", "6"]);
        assert_eq!(
            check_cache(t.path(), &want(Some("b1"), Some("en-US"), &w)),
            CacheCheck::Complete
        );
        // MissingDlc: only the ones not held.
        assert_eq!(
            check_cache(t.path(), &want(Some("b1"), Some("en-US"), &both)),
            CacheCheck::MissingDlc {
                missing: ids(&["7"])
            }
        );
        assert_eq!(
            check_cache(t.path(), &want(Some("b2"), Some("en-US"), &w)),
            CacheCheck::OtherBuild
        );
        assert_eq!(
            check_cache(t.path(), &want(Some("b1"), Some("de-DE"), &w)),
            CacheCheck::OtherLanguage
        );
        // Missing files win.
        std::fs::remove_file(t.path().join("Game/a.bin")).unwrap();
        assert!(matches!(
            check_cache(t.path(), &want(Some("b1"), Some("en-US"), &w)),
            CacheCheck::Missing { count: 1, .. }
        ));
        assert_eq!(
            check_cache(Path::new("/nonexistent"), &want(None, None, &w)),
            CacheCheck::Unrecorded
        );

        // An older record: unknown to a caller that wants DLC or a language,
        // whole to one that asks for neither.
        let old = recorded("b1", "en-US", &[]);
        std::fs::write(
            old.path().join(RECORD),
            r#"{"buildId":"b1","files":["Game/a.bin"]}"#,
        )
        .unwrap();
        assert_eq!(
            check_cache(old.path(), &want(Some("b1"), Some("en-US"), &w)),
            CacheCheck::UnknownContent
        );
        assert_eq!(
            check_cache(old.path(), &want(Some("b1"), Some("en-US"), &[])),
            CacheCheck::UnknownContent
        );
        assert_eq!(
            check_cache(old.path(), &CacheWant::default()),
            CacheCheck::Complete
        );
    }

    #[test]
    fn targeted_completion_diffs_from_the_recorded_set_without_verify() {
        let check = CacheCheck::MissingDlc {
            missing: ids(&["7"]),
        };
        let got = download_inputs(2, "de-DE", &ids(&["7"]), &check, &ids(&["5"]));
        assert_eq!(
            got,
            DownloadInputs {
                targeted: true,
                language: Some("de-DE".into()),
                old_language: Some("de-DE".into()),
                dlcs: ids(&["5", "7"]),
                old_dlcs: ids(&["5"]),
            }
        );
    }

    #[test]
    fn everything_else_is_a_full_verify_with_the_wanted_set() {
        let w = ids(&["7"]);
        for check in [
            CacheCheck::UnknownContent,
            CacheCheck::Unrecorded,
            CacheCheck::OtherBuild,
            CacheCheck::OtherLanguage,
            CacheCheck::Complete,
            CacheCheck::Missing {
                shown: vec![],
                count: 1,
            },
        ] {
            let got = download_inputs(2, "en-US", &w, &check, &ids(&["5"]));
            assert!(!got.targeted, "{check:?}");
            assert_eq!(got.dlcs, w);
            assert!(got.old_dlcs.is_empty());
            assert_eq!(got.old_language, None);
        }
        // Generation 1 names language and DLC the old way, even when a DLC is missing.
        let miss = CacheCheck::MissingDlc { missing: w.clone() };
        let v1 = download_inputs(1, "de-DE", &w, &miss, &ids(&["5"]));
        assert!(!v1.targeted);
        assert_eq!(v1.old_language.as_deref(), Some("German"));
        assert_eq!((v1.dlcs, v1.old_dlcs), (vec![], w));
    }

    #[test]
    fn new_record_keeps_the_dlcs_of_the_same_build_and_language() {
        let t = recorded("b1", "en-US", &["5"]);
        let prior = load_record(t.path());
        let w = ids(&["7"]);
        assert_eq!(
            record_dlcs(prior.as_ref(), "b1", "en-US", &w),
            ids(&["5", "7"])
        );
        assert_eq!(record_dlcs(prior.as_ref(), "b2", "en-US", &w), w);
        assert_eq!(record_dlcs(prior.as_ref(), "b1", "de-DE", &w), w);
        assert_eq!(record_dlcs(None, "b1", "en-US", &w), w);
    }

    #[test]
    fn plan_says_what_happens_to_the_cache() {
        let miss = CacheCheck::MissingDlc {
            missing: ids(&["7"]),
        };
        let t = download_inputs(2, "en-US", &ids(&["7"]), &miss, &[]);
        assert_eq!(
            cache_note(&miss, &t).as_deref(),
            Some("cache lacks DLC 7: fetching only those (no hashing)")
        );
        let f = download_inputs(2, "en-US", &[], &CacheCheck::OtherBuild, &[]);
        assert!(
            cache_note(&CacheCheck::OtherBuild, &f)
                .unwrap()
                .contains("another build")
        );
        assert_eq!(cache_note(&CacheCheck::Complete, &f), None);
    }

    fn build(id: &str, published: &str, public: bool) -> Build {
        serde_json::from_value(serde_json::json!({
            "build_id": id,
            "product_id": "1207659046",
            "os": "windows",
            "branch": null,
            "version_name": "v1",
            "tags": [],
            "public": public,
            "date_published": published,
            "generation": 2,
            "urls": []
        }))
        .expect("Build from json")
    }

    /// Design 19 §4 as a pure function of the installed build and GOG's list.
    #[test]
    fn the_installed_build_is_matched_against_the_listed_ones() {
        let items = vec![
            build("old", "2024-01-01T00:00:00+0000", true),
            build("new", "2025-01-01T00:00:00+0000", true),
        ];
        let (m, b) = match_build(Some("new"), &items);
        assert_eq!(m, BuildMatch::Newest);
        assert_eq!(b.unwrap().build_id(), "new");
        let (m, b) = match_build(Some("old"), &items);
        assert_eq!(
            m,
            BuildMatch::Older {
                newest_id: "new".into(),
                newest_version: "v1".into()
            }
        );
        assert_eq!(b.unwrap().build_id(), "old");
        let (m, b) = match_build(Some("rotated"), &items);
        assert!(m == BuildMatch::Gone && b.is_none());
        assert_eq!(match_build(None, &items).0, BuildMatch::Unknown);
    }

    /// `dlc add`'s downloader inputs (design 19 §3.2): the installed build against
    /// itself, `old_dlcs` = installed, `dlcs` = installed ∪ wanted, the recorded
    /// language on both sides, no verify.
    #[test]
    fn add_inputs_diff_the_installed_set_against_installed_plus_wanted() {
        let i = add_inputs("de-DE", &ids(&["5"]), &ids(&["9", "7"]));
        assert_eq!(
            i,
            DownloadInputs {
                targeted: true,
                language: Some("de-DE".into()),
                old_language: Some("de-DE".into()),
                dlcs: ids(&["5", "7", "9"]),
                old_dlcs: ids(&["5"]),
            }
        );
        // The first DLC: nothing installed, still the targeted diff.
        let first = add_inputs("en-US", &[], &ids(&["9"]));
        assert!(first.targeted && first.old_dlcs.is_empty());
    }

    #[test]
    fn a_snapshot_names_what_changed_and_round_trips() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("a"), b"1").unwrap();
        let before = Snapshot::take(t.path());
        let back: Snapshot =
            serde_json::from_str(&serde_json::to_string(&before).unwrap()).unwrap();
        assert_eq!(back, before);
        std::fs::create_dir_all(t.path().join("d")).unwrap();
        std::fs::write(t.path().join("d/b"), b"2").unwrap();
        assert_eq!(Snapshot::take(t.path()).changed_since(&back), ["d/b"]);
    }

    #[test]
    fn picks_newest_public_build() {
        let items = vec![
            build("b1", "2024-01-01T00:00:00Z", true),
            build("b2", "2025-06-01T00:00:00Z", true),
            build("b3", "2030-01-01T00:00:00Z", false), // newer but private
        ];
        assert_eq!(pick_build(&items).unwrap().build_id(), "b2");
    }

    #[test]
    fn tiebreaks_on_build_id() {
        let items = vec![
            build("aaa", "2025-01-01T00:00:00Z", true),
            build("zzz", "2025-01-01T00:00:00Z", true),
        ];
        assert_eq!(pick_build(&items).unwrap().build_id(), "zzz");
    }

    #[test]
    fn empty_when_only_private() {
        let items = vec![build("b3", "2030-01-01T00:00:00Z", false)];
        assert!(pick_build(&items).is_none());
    }

    #[test]
    fn v1_language_maps_codes() {
        assert_eq!(v1_language("en-US"), "English");
        assert_eq!(v1_language("fr"), "French");
        assert_eq!(v1_language("xx-XX"), "English");
    }

    #[test]
    fn event_ndjson_shape() {
        let e = DepotEvent::Resolved {
            product_id: 1,
            title: "T".into(),
            build_id: "b".into(),
            version: "v".into(),
            generation: 2,
        };
        let j = serde_json::to_string(&e).unwrap();
        assert_eq!(
            j,
            r#"{"type":"resolved","productId":1,"title":"T","buildId":"b","version":"v","generation":2}"#
        );
    }

    #[test]
    fn plan_ndjson_shape_with_and_without_game_files() {
        // macOS (no game_files): the shape it had before Linux.
        let mut e = DepotEvent::Plan {
            install_dir: "/c/T".into(),
            download_size: 1,
            disk_size: 2,
            language: "en-US".into(),
            dlcs: vec![],
            cache: None,
            game_files: None,
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"type":"plan","installDir":"/c/T","downloadSize":1,"diskSize":2,"language":"en-US"}"#
        );
        if let DepotEvent::Plan { game_files, .. } = &mut e {
            *game_files = Some(MaterializePlan {
                method: super::super::materialize::TreeMethod::Copy,
                game_dir: "/d/games/1/game".into(),
                extra_bytes: 2,
            });
        }
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"type":"plan","installDir":"/c/T","downloadSize":1,"diskSize":2,"language":"en-US","gameFiles":{"method":"copy","gameDir":"/d/games/1/game","extraBytes":2}}"#
        );
    }

    #[test]
    fn roots_layout() {
        let (install, support, deps) = cache_roots(Some(Path::new("/C")), 42, None).unwrap();
        assert_eq!(install, Path::new("/C/gogo/depot/42"));
        assert_eq!(support, Path::new("/C/gogo/depot/42/gog-support"));
        assert_eq!(deps, Path::new("/C/gogo/dependencies"));

        let (install, ..) =
            cache_roots(Some(Path::new("/C")), 42, Some(Path::new("/D/here"))).unwrap();
        assert_eq!(install, Path::new("/D/here"));
        assert!(cache_roots(None, 42, None).is_none());
    }

    #[test]
    fn state_event_normalizes_progress_to_fractions() {
        // Pins gog-warp's units: a bump that changes them must fail here.
        assert!(matches!(
            state_event(DownloadState::Allocating(0.5)),
            Some(DepotEvent::Allocating { fraction }) if fraction == 0.5
        ));
        assert!(matches!(
            state_event(DownloadState::Verifying(50.0)),
            Some(DepotEvent::Verifying { fraction }) if fraction == 0.5
        ));
        assert!(matches!(
            state_event(DownloadState::Verifying(100.0)),
            Some(DepotEvent::Verifying { fraction }) if fraction == 1.0
        ));
        assert!(state_event(DownloadState::Finished).is_none());
    }
}
