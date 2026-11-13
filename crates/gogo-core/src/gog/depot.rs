//! Depot (Model B) downloads via gog-warp — `docs/design/14`.
//!
//! Thin driver over gog-warp's `Downloader`: resolve the latest public Windows
//! build, plan, and download the game tree into the user cache directory
//! (macOS and Linux). Turning that tree into a game's `game/` dir is
//! `macapp::bundle::clone_tree` on macOS and [`super::materialize`] on Linux.
//! The heavy lifting (depots, secure links, chunk reconstruction, verification)
//! is the library's; this module owns selection, layout, events, and limits.

use std::collections::BTreeMap;
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
        /// The whole build for the language and DLC wanted: an upper bound when
        /// `estimated_download_size` is present.
        download_size: u64,
        disk_size: u64,
        language: String,
        /// An estimate of what a switch of language fetches, when the cache holds
        /// another language of this build (design 14 §11, #386): the depots the new
        /// language needs that the cached one lacks, by manifest hash. A guess until
        /// the downloader has hashed the cache; the plan line shows it in place of
        /// `download_size`.
        #[serde(skip_serializing_if = "Option::is_none")]
        estimated_download_size: Option<u64>,
        /// The cache already holds this build, language and DLC (#388): nothing is
        /// fetched, `estimated_download_size` is 0 and `download_size` stays the
        /// build's size.
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        cache_complete: bool,
        /// What `--language` said, when `language` is the code it resolved to
        /// (`de` → `de-DE`).
        #[serde(skip_serializing_if = "Option::is_none")]
        requested_language: Option<String>,
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
    /// Something the run says in passing: a switch of builds that fell back to
    /// hashing the cache (design 21 §3.5, #429).
    Note {
        message: String,
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
    /// `--language` names no language of the build (design 14 §11): a usage error.
    #[error(transparent)]
    Language(#[from] super::language::LanguageError),
    /// `--build` names no single listed build, or the game has none (design 21 §3.3,
    /// D9): a usage error, exit 2.
    #[error(transparent)]
    Pick(#[from] super::builds::PickError),
    #[error("no public Windows builds for product {0}")]
    NoBuilds(u64),
    #[error("no user cache directory available")]
    NoCacheDir,
    #[error("{0}")]
    Library(String),
    /// The cache holds DLC the build doesn't take and their files can't be told
    /// apart (design 19 §3.3): the build is refused, nothing changed.
    #[error("{0}")]
    Filter(String),
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
    /// `language` was asked for, not the default: it is checked against the build's
    /// languages and a short form resolved (design 14 §11).
    pub language_explicit: bool,
    /// Which DLC to take; `Ids`/`AllOwned` need `dlc_context`.
    pub dlcs: DlcSet,
    pub dlc_context: DlcContext,
    pub password: Option<String>,
    /// `--build`: an id or version name (design 21 §3.2). `None`: the newest public
    /// build on the default branch.
    pub build: Option<String>,
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
            language_explicit: false,
            dlcs: DlcSet::None,
            dlc_context: DlcContext::default(),
            password: None,
            build: None,
            dir: None,
            library: None,
            limit: None,
            disk_check: true,
            dry_run: false,
        }
    }
}

pub use super::builds::pick_build;

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
    let items = super::builds::fetch_listing(core, product_id, Platform::Windows, None)
        .await
        .map_err(DepotError::library)?
        .builds;
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
    let builds = super::builds::fetch_listing(core, product_id, Platform::Windows, None)
        .await
        .map_err(DepotError::library)?;
    Ok(pick_build(&builds.builds).is_some())
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

/// The newest public build: what `gogo depot` and the listings read before any
/// depot manifest.
pub struct NewestBuild {
    pub build_id: String,
    pub version: String,
    pub generation: u32,
    pub manifest: Manifest,
}

/// The list entry GOG's builds list gives build `build_id` of `product_id` (its
/// version name for `content.toml`, #418, and its branch). `None` when the list
/// can't be fetched or doesn't hold the build.
pub async fn build_info(
    core: &Core,
    product_id: u64,
    password: Option<String>,
    build_id: &str,
) -> Option<super::builds::BuildEntry> {
    let builds = super::builds::fetch_listing(core, product_id, Platform::Windows, password)
        .await
        .ok()?;
    builds
        .builds
        .iter()
        .find(|b| b.build_id() == build_id)
        .map(super::builds::BuildEntry::from_build)
}

/// Build `build_id`'s list entry and, when the list holds it and the manifest
/// can be fetched, `(id, generation, manifest)` for `build-manifest.json`
/// (design 21 §5.2). One listing request; best effort, `None` entry as
/// [`build_info`].
pub async fn build_info_with_manifest(
    core: &Core,
    product_id: u64,
    password: Option<String>,
    build_id: &str,
) -> Option<(super::builds::BuildEntry, Option<(String, u32, Manifest)>)> {
    let builds = super::builds::fetch_listing(core, product_id, Platform::Windows, password)
        .await
        .ok()?;
    let b = builds.builds.iter().find(|b| b.build_id() == build_id)?;
    let kept = core
        .get_manifest(b)
        .await
        .ok()
        .map(|m| (b.build_id().clone(), *b.generation(), m));
    Some((super::builds::BuildEntry::from_build(b), kept))
}

/// The version name GOG's builds list gives build `build_id` of `product_id`
/// (`2.31`), for `content.toml` (#418). `None` when the list can't be fetched or
/// doesn't hold the build: the record then goes without a version.
pub async fn build_version(
    core: &Core,
    product_id: u64,
    password: Option<String>,
    build_id: &str,
) -> Option<String> {
    build_info(core, product_id, password, build_id)
        .await
        .map(|e| e.version)
        .filter(|v| !v.trim().is_empty())
}

/// Fetch the newest public build's list entry and its build manifest.
pub async fn newest_build(
    core: &Core,
    product_id: u64,
    password: Option<String>,
) -> Result<NewestBuild, DepotError> {
    chosen_build(core, product_id, "", password, None).await
}

/// The build a run takes: `want` (`--build`, design 21 §3.2) or the newest public
/// build on the default branch, with its build manifest.
pub async fn chosen_build(
    core: &Core,
    product_id: u64,
    title: &str,
    password: Option<String>,
    want: Option<&str>,
) -> Result<NewestBuild, DepotError> {
    let builds = super::builds::fetch_listing(core, product_id, Platform::Windows, password)
        .await
        .map_err(DepotError::library)?;
    let build = match want {
        Some(v) => super::builds::pick_for_run(
            &builds,
            v,
            &super::builds::ResolveCtx {
                title,
                installed: None,
            },
        )?,
        None => pick_build(&builds.builds).ok_or(DepotError::NoBuilds(product_id))?,
    };
    let manifest = core
        .get_manifest(build)
        .await
        .map_err(DepotError::library)?;
    Ok(NewestBuild {
        build_id: build.build_id().to_string(),
        version: build.version_name().to_string(),
        generation: *build.generation(),
        manifest,
    })
}

/// The language a run takes: `requested` checked against the build's languages when
/// it was asked for (`explicit`), else as it is (design 14 §11).
pub fn pick_language(
    manifest: &Manifest,
    requested: &str,
    explicit: bool,
) -> Result<String, DepotError> {
    if !explicit {
        return Ok(requested.to_string());
    }
    Ok(super::language::of_manifest(manifest).resolve(requested)?)
}

/// The run's build's id and its manifest's DLC with sizes, for naming DLC before a
/// run (design 19 §1.3): the newest, or `want` (`--build`). No depot manifest is
/// fetched. An `explicit` language is checked first, and sizes are for the code it
/// resolves to.
pub async fn build_dlcs(
    core: &Core,
    product_id: u64,
    title: &str,
    password: Option<String>,
    want: Option<&str>,
    language: &str,
    explicit: bool,
) -> Result<(String, Vec<dlc::ManifestDlc>), DepotError> {
    let b = chosen_build(core, product_id, title, password, want).await?;
    let language = pick_language(&b.manifest, language, explicit)?;
    Ok((b.build_id, dlc::manifest_dlcs(&b.manifest, &language)))
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

    let builds =
        super::builds::fetch_listing(core, product_id, Platform::Windows, opts.password.clone())
            .await
            .map_err(DepotError::library)?;
    let build = match opts.build.as_deref() {
        Some(v) => super::builds::pick_for_run(
            &builds,
            v,
            &super::builds::ResolveCtx {
                title,
                installed: None,
            },
        )?,
        None => pick_build(&builds.builds).ok_or(DepotError::NoBuilds(product_id))?,
    };
    let build_id = build.build_id().to_string();
    let generation = *build.generation();
    let version = build.version_name().to_string();
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
    // Before any hashing or download: an unknown language stops the run (design 14 §11).
    let language = pick_language(&manifest, &opts.language, opts.language_explicit)?;
    let requested_language = (language != opts.language).then(|| opts.language.clone());
    let mdlcs = dlc::manifest_dlcs(&manifest, &language);
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
    let (download_size, disk_size) = manifest.install_size(&language, &dlcs, None);
    // What the cache already there holds decides how it's completed (design 19 §3.3).
    let prior = load_record(&install_root);
    let want = CacheWant {
        build_id: Some(&build_id),
        language: Some(&language),
        dlcs: &dlcs,
    };
    let check = prior.as_ref().map_or(CacheCheck::Unrecorded, |r| {
        check_record(&install_root, r, &want)
    });
    let cache_complete = check == CacheCheck::Complete;
    let estimated_download_size = if cache_complete {
        Some(0)
    } else {
        estimate_switch(&check, prior.as_ref(), &manifest, &language, &dlcs)
    };
    // A cache of another build whose manifest is known switches by its diff
    // (design 21 §3.5, #429); without it, it is hashed.
    let cached = prior.as_ref().map(|r| CachedBuild {
        build_id: &r.build_id,
        language: r.language.as_deref(),
        dlcs: r.dlcs.as_deref(),
    });
    let old_manifest = match &cached {
        Some(c) if can_switch(generation, &language, &check, c) => {
            cached_manifest(core, product_id, c.build_id, &builds.builds)
                .await
                .filter(|old| same_install_dir(old, &manifest))
        }
        _ => None,
    };
    let verify_inputs = || {
        download_inputs(
            generation,
            &language,
            &dlcs,
            &check,
            prior
                .as_ref()
                .and_then(|r| r.whole_dlcs(&language))
                .unwrap_or(&[]),
        )
    };
    let inputs = match (&old_manifest, &cached) {
        (Some(_), Some(c)) => switch_inputs(&language, &dlcs, c),
        _ => verify_inputs(),
    };
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
        estimated_download_size,
        cache_complete,
        language: language.clone(),
        requested_language,
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

    // The files of another build are about to be overwritten: until the record of
    // the finished download replaces this one, the tree may mix both builds, so the
    // record says so (design 21 §3.5). A cache nobody can vouch for is refused.
    mark_upgrading(&install_root, &build_id)?;
    // A switch starts from a whole cache of the old build, so a `!Temp` there is an
    // earlier run's leftover: gog-warp would take its files as this run's (#362).
    let warp_temp = match &inputs.switch_from {
        Some(_) => match warp_temp_dir(&install_root, &manifest) {
            Ok(t) => {
                if !cancel.is_cancelled() {
                    let _ = std::fs::remove_dir_all(&t);
                }
                Some(t)
            }
            Err(why) => {
                let _ = events.send(DepotEvent::Note { message: why });
                None
            }
        },
        None => None,
    };
    let spec = FetchSpec {
        manifest: &manifest,
        build_id: &build_id,
        inputs: &inputs,
        old_manifest: old_manifest.as_ref(),
        install_root: &install_root,
        support_root: &support_root,
        deps_root: &deps_root,
        limit: opts.limit,
        disk_check: opts.disk_check,
    };
    let (fetched_files, fetched_bytes) = match &inputs.switch_from {
        Some(from) => {
            let verify = verify_inputs();
            let verify_spec = FetchSpec {
                inputs: &verify,
                old_manifest: None,
                ..spec
            };
            switch_or_verify(
                warp_temp.as_deref(),
                from,
                cancel,
                events,
                fetch_into(core, &spec, events, cancel, free_space),
                || async {
                    if let Some(old) = &old_manifest {
                        prune_dropped(core, &install_root, old, &manifest, &inputs, events).await;
                    }
                    fetch_into(core, &verify_spec, events, cancel, free_space).await
                },
            )
            .await?
        }
        None => fetch_into(core, &spec, events, cancel, free_space).await?,
    };
    // The download is done either way; without a record (a full disk, say)
    // the next reuse just checks it again.
    let held = record_dlcs(prior.as_ref(), &build_id, &language, &dlcs);
    let was_whole = whole_before(prior.as_ref(), &check, &build_id, &language);
    if write_record_build(
        &install_root,
        &BuildStamp {
            build_id: &build_id,
            version: Some(&version),
            generation: Some(generation),
        },
        &language,
        &held,
    )
    .is_err()
    {
        let _ = std::fs::remove_file(install_root.join(RECORD));
    } else {
        // Keep the products' file lists for a build that leaves a DLC or a language
        // out, and settle the other languages (design 19 §3.3, #389). Best effort:
        // they stay not whole, and a build fetches the lists it lacks.
        let rec = load_record(&install_root);
        let languages: Vec<String> = rec
            .as_ref()
            .map(|r| {
                std::iter::once(language.clone())
                    .chain(r.other_languages.keys().cloned())
                    .collect()
            })
            .unwrap_or_default();
        let mut products = vec![manifest.product_id()];
        products.extend(held.iter().cloned());
        products.extend(
            rec.iter()
                .flat_map(|r| r.other_languages.values())
                .flat_map(|h| h.dlcs.iter().cloned()),
        );
        products.sort();
        products.dedup();
        let lacking = rec.as_ref().is_some_and(|r| {
            languages.iter().any(|l| {
                let have = r.products_in(l);
                products
                    .iter()
                    .any(|p| have.is_none_or(|h| !h.contains_key(p)))
            })
        });
        let several = languages.len() > 1;
        let wrote = fetched_files > 0;
        if (several || !held.is_empty()) && (lacking || (wrote && !was_whole.is_empty())) {
            match super::dlc_filter::fetch_depots(
                core.reqwest_client(),
                &manifest,
                Some((&products, &languages)),
            )
            .await
            {
                Ok(depots) => {
                    let _ =
                        settle_languages(&install_root, &build_id, &was_whole, &products, &depots);
                }
                Err(_) if !wrote => {
                    let _ = keep_whole(&install_root, &build_id, &was_whole);
                }
                Err(_) => {}
            }
        } else if !wrote && !was_whole.is_empty() {
            let _ = keep_whole(&install_root, &build_id, &was_whole);
        }
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
    /// The cache's build manifest, for a switch ([`DownloadInputs::switch_from`]).
    pub old_manifest: Option<&'a Manifest>,
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
    let builder = spec
        .inputs
        .apply(builder, spec.manifest, spec.build_id, spec.old_manifest);
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
/// gog-warp's temp dir for diffs from an old manifest: in the game tree's dir
/// (`<root>/<installDirectory>/!Temp`), where its move loop renames from.
const WARP_TEMP: &str = "!Temp";

/// A path a manifest names, to join onto a root: not empty, not absolute, and
/// only plain components (no `..`, no `.`, no empty one). Backslashes count as
/// separators; one trailing separator is allowed.
fn plain_relative(path: &str) -> bool {
    let p = path.replace('\\', "/");
    let p = p.strip_suffix('/').unwrap_or(&p);
    !p.is_empty() && p.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

/// gog-warp's `!Temp` for a download of `manifest` into cache root `root`, or why
/// it isn't removed: an install directory that isn't a plain relative path.
pub fn warp_temp_dir(root: &Path, manifest: &Manifest) -> Result<PathBuf, String> {
    let dir = manifest.install_directory();
    if plain_relative(&dir) {
        Ok(root.join(dir.replace('\\', "/")).join(WARP_TEMP))
    } else {
        Err(format!(
            "not clearing the download's temp dir: the build's install directory {dir:?} \
             isn't a plain relative path"
        ))
    }
}

/// Pure: whether builds `old` and `new` lay their game tree in the same dir of the
/// cache. gog-warp joins the new build's onto the root and diffs paths below it,
/// so a switch across two dirs would leave the unchanged files behind.
pub fn same_install_dir(old: &Manifest, new: &Manifest) -> bool {
    let norm = |m: &Manifest| {
        m.install_directory()
            .replace('\\', "/")
            .trim_matches('/')
            .to_string()
    };
    norm(old).eq_ignore_ascii_case(&norm(new))
}

/// Pure: the paths build `old` lays that build `new` doesn't, compared without
/// case as gog-warp does: what a finished switch deletes (its `report.deleted`).
pub fn dropped_paths(
    old: &[super::dlc_filter::DepotList],
    new: &[super::dlc_filter::DepotList],
) -> Vec<String> {
    let kept: std::collections::HashSet<String> = new
        .iter()
        .flat_map(|d| d.paths.iter().map(|p| p.to_lowercase()))
        .collect();
    let mut out: Vec<String> = old
        .iter()
        .flat_map(|d| d.paths.iter())
        .filter(|p| !kept.contains(&p.to_lowercase()))
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Remove `paths` (relative to cache root `root`, from [`dropped_paths`]): only
/// plain relative ones, and only files and links, never a dir. How many went.
pub fn remove_dropped(root: &Path, paths: &[String]) -> usize {
    paths
        .iter()
        .filter(|p| plain_relative(p))
        .filter(|p| {
            let at = root.join(p);
            at.symlink_metadata().is_ok_and(|m| !m.is_dir()) && std::fs::remove_file(at).is_ok()
        })
        .count()
}

/// What gog-warp would have deleted after a switch that failed in its move loop
/// (#362): the files the cache's build `old` lists and the new one doesn't, from
/// both builds' depot manifests, for the run's language and DLC. `verify` never
/// removes a file, so without this they would stay and the record would list them.
/// A failed listing leaves them, and says so.
async fn prune_dropped(
    core: &Core,
    root: &Path,
    old: &Manifest,
    new: &Manifest,
    inputs: &DownloadInputs,
    events: &tokio::sync::mpsc::UnboundedSender<DepotEvent>,
) {
    let from = inputs.switch_from.as_deref().unwrap_or("?");
    let side = |m: &Manifest, lang: &Option<String>, dlcs: &[String]| {
        let mut products = vec![m.product_id()];
        products.extend(dlcs.iter().cloned());
        (products, lang.iter().cloned().collect::<Vec<String>>())
    };
    let (old_products, old_langs) = side(old, &inputs.old_language, &inputs.old_dlcs);
    let (new_products, new_langs) = side(new, &inputs.language, &inputs.dlcs);
    let http = core.reqwest_client();
    let lists = (
        super::dlc_filter::fetch_depots(http, old, Some((&old_products, &old_langs))).await,
        super::dlc_filter::fetch_depots(http, new, Some((&new_products, &new_langs))).await,
    );
    let message = match lists {
        (Ok(a), Ok(b)) => {
            let gone = remove_dropped(root, &dropped_paths(&a, &b));
            if gone == 0 {
                return;
            }
            format!("removed {gone} file(s) that build {from} had and this build dropped")
        }
        (Err(e), _) | (_, Err(e)) => format!(
            "couldn't list the files build {from} had and this build dropped ({e}): they \
             stay in the cache"
        ),
    };
    let _ = events.send(DepotEvent::Note { message });
}

#[derive(Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    build_id: String,
    /// The build's version name and generation (design 21 §5.3); absent in records
    /// an older gogo wrote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generation: Option<u32>,
    /// Set before a download onto a cache of another build, cleared by the record
    /// the finished download writes (design 21 §3.5). A record that has it never
    /// counts as complete: the next run checks the tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    upgrading: Option<Upgrading>,
    /// The language the last download asked for; absent in records an older gogo
    /// wrote. A path two languages share holds this one's bytes. The cache can hold
    /// other languages too (`other_languages`, #389): a switch deletes nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    /// DLC product ids the cache holds, sorted; absent in records an older gogo
    /// wrote, which can't say what the tree holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dlcs: Option<Vec<String>>,
    /// Each product's files, from the build's depot manifests (design 19 §3.3):
    /// what a build that leaves a cached DLC out uses to tell its files apart. Only
    /// the products some build needed; absent until one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    products: Option<super::dlc_filter::ProductFiles>,
    /// The other languages whose files the cache still holds (#389), by code: the
    /// cache is never pruned, so a build leaves them out and a switch back can cost
    /// no download. Absent in records an older gogo wrote, which are taken to hold
    /// `language` only.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    other_languages: BTreeMap<String, HeldLanguage>,
    /// `language` and `other_languages` name every language whose files are all in
    /// the cache (#391). A gogo from before #389 kept the old language's files on a
    /// switch but recorded only the new one, so a record without this flag may hold
    /// more: [`super::cache_upgrade::scan_languages`] finds those once and sets it.
    /// A new record keeps the last one's flag; the first record of a cache hasn't it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    languages_scanned: bool,
    /// Paths relative to the cache root, `/`-separated, sorted.
    files: Vec<String>,
}

/// A cache switching builds (design 21 §3.5).
#[derive(Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
struct Upgrading {
    from: String,
    to: String,
}

/// Mark cache root `root`'s record as switching to build `to`, durably, before any
/// file is written. A no-op for a record of `to` (or none): a plain repair keeps the
/// files of its own build. Best effort: `Err` means the marker isn't there.
fn mark_upgrading(root: &Path, to: &str) -> std::io::Result<()> {
    let Some(mut rec) = load_record(root) else {
        return Ok(());
    };
    if rec.build_id == to && rec.upgrading.is_none() {
        return Ok(());
    }
    if rec.upgrading.is_none() {
        rec.upgrading = Some(Upgrading {
            from: rec.build_id.clone(),
            to: to.to_string(),
        });
    }
    save_record(root, &rec)
}

/// One of a record's other languages (#389).
#[derive(Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct HeldLanguage {
    /// Its files are there as it has them: no path it shares with a language
    /// fetched since holds that one's bytes (checked against the build's depot
    /// manifests). Only then can a build in it go without a download, and only
    /// then is `dlcs` true. Set back to false by every download that can't check.
    #[serde(default)]
    whole: bool,
    /// The DLC it held whole when it was the record's language.
    #[serde(default)]
    dlcs: Vec<String>,
    /// Its per-product file lists, like the record's `products`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    products: Option<super::dlc_filter::ProductFiles>,
}

impl Record {
    /// Its other language `language` (any case).
    fn other(&self, language: &str) -> Option<(&String, &HeldLanguage)> {
        self.other_languages
            .iter()
            .find(|(l, _)| l.eq_ignore_ascii_case(language))
    }

    fn is_language(&self, language: &str) -> bool {
        self.language
            .as_deref()
            .is_some_and(|l| l.eq_ignore_ascii_case(language))
    }

    /// The DLC it holds whole in `language`: its own for its language, a whole
    /// other language's; `None` for a language it doesn't hold whole.
    fn whole_dlcs(&self, language: &str) -> Option<&[String]> {
        if self.is_language(language) {
            return Some(self.dlcs.as_deref().unwrap_or(&[]));
        }
        self.other(language)
            .filter(|(_, h)| h.whole)
            .map(|(_, h)| h.dlcs.as_slice())
    }

    /// The languages it holds whole, with their DLC: its own and the whole others.
    fn whole_languages(&self) -> Vec<(&str, &[String])> {
        self.language
            .as_deref()
            .map(|l| (l, self.dlcs.as_deref().unwrap_or(&[])))
            .into_iter()
            .chain(
                self.other_languages
                    .iter()
                    .filter(|(_, h)| h.whole)
                    .map(|(l, h)| (l.as_str(), h.dlcs.as_slice())),
            )
            .collect()
    }

    /// The file lists it keeps for `language`.
    fn products_in(&self, language: &str) -> Option<&super::dlc_filter::ProductFiles> {
        if self.is_language(language) {
            self.products.as_ref()
        } else {
            self.other(language).and_then(|(_, h)| h.products.as_ref())
        }
    }

    fn products_in_mut(
        &mut self,
        language: &str,
    ) -> Option<&mut Option<super::dlc_filter::ProductFiles>> {
        if self.is_language(language) {
            return Some(&mut self.products);
        }
        self.other_languages
            .iter_mut()
            .find(|(l, _)| l.eq_ignore_ascii_case(language))
            .map(|(_, h)| &mut h.products)
    }
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
    /// A switch from build `from` to `to` started and never finished (design 21
    /// §3.5): the tree may mix both, whatever the record's files say.
    Interrupted { from: String, to: String },
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

/// Record what's in `root` after a finished download in `language` (overwrites
/// the last). `dlcs` are the product ids the cache holds in `language`. The
/// per-product file lists of the last record stay when it was for the same build.
///
/// The languages the last record held stay held (#389): a download deletes
/// nothing. None of them is whole any more, since the download may have written a
/// path one shares with `language`; [`settle_languages`] checks that afterwards.
/// For another build, they keep no DLC or lists: their files are an older build's.
pub fn write_record(
    root: &Path,
    build_id: &str,
    language: &str,
    dlcs: &[String],
) -> std::io::Result<()> {
    write_record_build(
        root,
        &BuildStamp {
            build_id,
            version: None,
            generation: None,
        },
        language,
        dlcs,
    )
}

/// The build a record is for. `version` and `generation` that aren't given are
/// kept from the last record when it was for the same build.
pub struct BuildStamp<'a> {
    pub build_id: &'a str,
    pub version: Option<&'a str>,
    pub generation: Option<u32>,
}

/// [`write_record`] for a known build (design 21 §5.3).
pub fn write_record_build(
    root: &Path,
    stamp: &BuildStamp<'_>,
    language: &str,
    dlcs: &[String],
) -> std::io::Result<()> {
    let build_id = stamp.build_id;
    let mut files = Vec::new();
    walk(root, root, &mut files)?;
    files.sort();
    let mut dlcs = dlcs.to_vec();
    dlcs.sort();
    dlcs.dedup();
    let prior = load_record(root);
    let same_build = prior.as_ref().filter(|p| p.build_id == build_id);
    let version = stamp
        .version
        .map(str::to_string)
        .or_else(|| same_build.and_then(|p| p.version.clone()))
        .filter(|v| !v.is_empty());
    let generation = stamp
        .generation
        .or_else(|| same_build.and_then(|p| p.generation));
    let languages_scanned = prior.as_ref().is_some_and(|p| p.languages_scanned);
    let (products, other_languages) = match prior {
        Some(p) => carry_languages(p, build_id, language),
        None => (None, BTreeMap::new()),
    };
    let rec = Record {
        build_id: build_id.to_string(),
        version,
        generation,
        upgrading: None,
        language: Some(language.to_string()),
        dlcs: Some(dlcs),
        products,
        other_languages,
        languages_scanned,
        files,
    };
    save_record(root, &rec)
}

/// Pure: what a new record in `language` of `build_id` keeps of the last one
/// `prior`: the lists of `language` (same build), and every other language it held,
/// none of them whole.
fn carry_languages(
    mut prior: Record,
    build_id: &str,
    language: &str,
) -> (
    Option<super::dlc_filter::ProductFiles>,
    BTreeMap<String, HeldLanguage>,
) {
    let same = prior.build_id == build_id;
    let mut held = std::mem::take(&mut prior.other_languages);
    if let Some(l) = prior.language.take() {
        held.insert(
            l,
            HeldLanguage {
                whole: false,
                dlcs: prior.dlcs.take().unwrap_or_default(),
                products: prior.products.take(),
            },
        );
    }
    let mut products = None;
    held.retain(|l, h| {
        if l.eq_ignore_ascii_case(language) {
            products = h.products.take();
            false
        } else {
            true
        }
    });
    for h in held.values_mut() {
        h.whole = false;
        if !same {
            *h = HeldLanguage::default();
        }
    }
    (products.filter(|_| same), held)
}

/// After a download of `build_id` into cache root `root` and its [`write_record`],
/// settle the record's other languages from the build's depots (#389): keep each
/// one's file lists, and mark whole those of `was_whole` (whole before the
/// download) that share no path with the record's language through another depot
/// manifest ([`super::dlc_filter::clash`]). `depots` must hold every depot of
/// `products` in each of the record's languages; every product gets a list in each,
/// empty when it has no depots there. Also keeps the record language's lists. A
/// no-op when the record isn't for `build_id`.
pub fn settle_languages(
    root: &Path,
    build_id: &str,
    was_whole: &[String],
    products: &[String],
    depots: &[super::dlc_filter::DepotList],
) -> std::io::Result<()> {
    let Some(mut rec) = load_record(root).filter(|r| r.build_id == build_id) else {
        return Ok(());
    };
    let Some(language) = rec.language.clone() else {
        return Ok(());
    };
    let lists = super::dlc_filter::product_files(depots, &language, products);
    rec.products.get_or_insert_default().extend(lists);
    for (l, h) in rec.other_languages.iter_mut() {
        let lists = super::dlc_filter::product_files(depots, l, products);
        h.products.get_or_insert_default().extend(lists);
        h.whole = was_whole.iter().any(|w| w.eq_ignore_ascii_case(l))
            && super::dlc_filter::clash(depots, &language, |d| {
                super::dlc_filter::language_matches(&d.languages, l)
            })
            .is_none();
    }
    save_record(root, &rec)
}

/// Mark the record's other languages of `was_whole` whole again, after a download of
/// `build_id` that wrote nothing (no path changed, so none was overwritten).
fn keep_whole(root: &Path, build_id: &str, was_whole: &[String]) -> std::io::Result<()> {
    let Some(mut rec) = load_record(root).filter(|r| r.build_id == build_id) else {
        return Ok(());
    };
    for (l, h) in rec.other_languages.iter_mut() {
        h.whole = was_whole.iter().any(|w| w.eq_ignore_ascii_case(l));
    }
    save_record(root, &rec)
}

/// Durably: a crash leaves the old record or the new one, never a torn one, and
/// the new one only once the files it names are on disk (callers write it last).
fn save_record(root: &Path, rec: &Record) -> std::io::Result<()> {
    super::lay::write_json_durably(&root.join(RECORD), rec)
}

/// Whether cache root `root`'s record is for build `build_id` and predates DLC and
/// language tracking (no `language` or no `dlcs`): one [`upgrade_record`] can bring
/// up to date.
pub fn record_is_old(root: &Path, build_id: &str) -> bool {
    load_record(root)
        .is_some_and(|r| r.build_id == build_id && (r.language.is_none() || r.dlcs.is_none()))
}

/// Bring an older record of build `build_id` up to date (#374): `language`, the DLC
/// it holds and the products' file lists, its `files` as they were (the tree isn't
/// touched). Only while it is still that old record, else `Ok(false)` and nothing
/// changes. The caller has checked every file the new record claims.
pub(crate) fn upgrade_record(
    root: &Path,
    build_id: &str,
    language: &str,
    dlcs: &[String],
    products: &super::dlc_filter::ProductFiles,
) -> std::io::Result<bool> {
    let Some(mut rec) = load_record(root).filter(|r| {
        r.build_id == build_id
            && (r.language.is_none() || r.dlcs.is_none())
            && r.language
                .as_deref()
                .is_none_or(|l| l.eq_ignore_ascii_case(language))
    }) else {
        return Ok(false);
    };
    let mut held = dlcs.to_vec();
    held.sort();
    held.dedup();
    let mut all = rec.products.take().unwrap_or_default();
    all.extend(products.iter().map(|(k, v)| (k.clone(), v.clone())));
    rec.language = Some(language.to_string());
    rec.dlcs = Some(held);
    rec.products = Some(all);
    save_record(root, &rec)?;
    Ok(true)
}

/// Whether cache root `root`'s record is for build `build_id` and hasn't had its
/// languages scanned yet (#391): it may hold files of a language it doesn't name.
/// Every older record (#374) is one.
pub fn languages_unscanned(root: &Path, build_id: &str) -> bool {
    load_record(root).is_some_and(|r| r.build_id == build_id && !r.languages_scanned)
}

/// Record what [`super::cache_upgrade::scan_languages`] found in cache root
/// `root` (#391): each language of `found` the record doesn't name yet goes into
/// `other_languages`, `lists` (the record language's) into `products`, and the
/// record is marked scanned. Only while it is still the unscanned record of
/// `build_id` in `language` it was scanned from, else `Ok(false)` and nothing
/// changes. The record is the only write; `files` and the tree stay as they are.
pub(crate) fn record_scanned_languages(
    root: &Path,
    build_id: &str,
    language: &str,
    found: &[super::cache_upgrade::FoundLanguage],
    lists: &super::dlc_filter::ProductFiles,
) -> std::io::Result<bool> {
    let Some(mut rec) = load_record(root)
        .filter(|r| r.build_id == build_id && !r.languages_scanned && r.is_language(language))
    else {
        return Ok(false);
    };
    for f in found {
        if rec.is_language(&f.language) || rec.other(&f.language).is_some() {
            continue;
        }
        rec.other_languages.insert(
            f.language.clone(),
            HeldLanguage {
                whole: f.whole,
                dlcs: f.dlcs.clone(),
                products: Some(f.products.clone()),
            },
        );
    }
    rec.products
        .get_or_insert_default()
        .extend(lists.iter().map(|(k, v)| (k.clone(), v.clone())));
    rec.languages_scanned = true;
    save_record(root, &rec)?;
    Ok(true)
}

/// The per-product file lists cache root `root`'s record keeps for its language,
/// when it is for build `build_id`.
pub fn record_products(root: &Path, build_id: &str) -> Option<super::dlc_filter::ProductFiles> {
    load_record(root)
        .filter(|r| r.build_id == build_id)
        .and_then(|r| r.products)
}

/// The per-product file lists cache root `root`'s record keeps for `language` (its
/// own or another it holds, #389), when it is for build `build_id`.
pub fn record_products_in(
    root: &Path,
    build_id: &str,
    language: &str,
) -> Option<super::dlc_filter::ProductFiles> {
    load_record(root)
        .filter(|r| r.build_id == build_id)
        .and_then(|r| r.products_in(language).cloned())
}

/// Add `lists` to the record of cache root `root` for its language, when it is for
/// build `build_id` (there's nothing to add them to otherwise).
pub fn store_products(
    root: &Path,
    build_id: &str,
    lists: &super::dlc_filter::ProductFiles,
) -> std::io::Result<()> {
    let Some(language) = load_record(root).and_then(|r| r.language) else {
        return Ok(());
    };
    store_products_in(root, build_id, &language, lists)
}

/// Add `lists` of `language` to the record of cache root `root`, when it is for
/// build `build_id` and holds `language` (#389).
pub fn store_products_in(
    root: &Path,
    build_id: &str,
    language: &str,
    lists: &super::dlc_filter::ProductFiles,
) -> std::io::Result<()> {
    let Some(mut rec) = load_record(root).filter(|r| r.build_id == build_id) else {
        return Ok(());
    };
    let Some(slot) = rec.products_in_mut(language) else {
        return Ok(());
    };
    slot.get_or_insert_default()
        .extend(lists.iter().map(|(k, v)| (k.clone(), v.clone())));
    save_record(root, &rec)
}

/// The file lists of `products` in build `build_id` of `product_id` (design 19
/// §3.3): from cache root `root`'s record when it has them all, else fetched from
/// GOG (the build must still be listed) and added to the record.
pub async fn product_lists(
    core: &Core,
    product_id: u64,
    root: &Path,
    build_id: &str,
    language: &str,
    products: &[String],
) -> Result<super::dlc_filter::ProductFiles, DepotError> {
    let have = record_products_in(root, build_id, language).unwrap_or_default();
    if products.iter().all(|p| have.contains_key(p)) {
        return Ok(have);
    }
    let found = installed_build(core, product_id, Some(build_id)).await?;
    let Some((_, _, manifest)) = found.listed else {
        return Err(DepotError::Library(format!(
            "build {build_id} is no longer served by GOG, so its file lists can't be fetched"
        )));
    };
    let missing: Vec<String> = products
        .iter()
        .filter(|p| !have.contains_key(*p))
        .cloned()
        .collect();
    let fetched =
        super::dlc_filter::fetch(core.reqwest_client(), &manifest, language, &missing).await?;
    // A record that can't take them only costs the next build this fetch.
    let _ = store_products_in(root, build_id, language, &fetched);
    let mut all = have;
    all.extend(fetched);
    Ok(all)
}

/// The DLC a new record lists: those the old record held whole in `language`, when
/// it was for the same build (a completion or repair keeps them), plus `wanted`.
fn record_dlcs(
    prior: Option<&Record>,
    build_id: &str,
    language: &str,
    wanted: &[String],
) -> Vec<String> {
    let mut all: Vec<String> = prior
        .filter(|r| r.build_id == build_id)
        .and_then(|r| r.whole_dlcs(language))
        .map(<[String]>::to_vec)
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
        if dir == root && (name == RECORD || name == WARP_MARKER || name == format!("{RECORD}.tmp"))
        {
            continue;
        }
        // gog-warp's scratch, at the root or in the game tree's dir.
        if name == WARP_TEMP && (dir == root || dir.parent() == Some(root)) {
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

    /// The size it recorded for `path` (relative to the root), if it has it.
    pub fn size_of(&self, path: &str) -> Option<u64> {
        self.0.get(path).map(|(len, _)| *len)
    }

    /// The paths (relative to the root, `/`-separated, sorted) `before` had and
    /// `self` hasn't: what a run deleted (design 21 §11g).
    pub fn gone_since(&self, before: &Snapshot) -> Vec<String> {
        let mut out: Vec<String> = before
            .0
            .keys()
            .filter(|f| !self.0.contains_key(*f))
            .cloned()
            .collect();
        out.sort();
        out
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
    /// The build's version name and generation, when the record has them.
    pub version: Option<String>,
    pub generation: Option<u32>,
    pub language: Option<String>,
    /// DLC product ids; `None` for a record from before DLC tracking.
    pub dlcs: Option<Vec<String>>,
    /// The other languages the cache holds (#389), with the DLC each held.
    pub other_languages: Vec<(String, Vec<String>)>,
}

impl RecordSummary {
    /// Every language the cache holds: the record's, then the others.
    pub fn languages(&self) -> Vec<String> {
        self.language
            .iter()
            .cloned()
            .chain(self.other_languages.iter().map(|(l, _)| l.clone()))
            .collect()
    }

    /// Every DLC the cache holds files of, in any language.
    pub fn all_dlcs(&self) -> Vec<String> {
        let mut all: Vec<String> = self
            .dlcs
            .iter()
            .flatten()
            .chain(self.other_languages.iter().flat_map(|(_, d)| d))
            .cloned()
            .collect();
        all.sort();
        all.dedup();
        all
    }
}

/// The record in cache root `root`, if there is a readable one.
pub fn read_record(root: &Path) -> Option<RecordSummary> {
    let rec = load_record(root)?;
    Some(RecordSummary {
        build_id: rec.build_id,
        version: rec.version,
        generation: rec.generation,
        language: rec.language,
        dlcs: rec.dlcs,
        other_languages: rec
            .other_languages
            .into_iter()
            .map(|(l, h)| (l, h.dlcs))
            .collect(),
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
    if let Some(u) = &rec.upgrading {
        return CacheCheck::Interrupted {
            from: u.from.clone(),
            to: u.to.clone(),
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
    // Another language the cache holds whole counts as held (#389).
    let held = match want.language {
        Some(wanted) if rec.language.is_some() => match rec.whole_dlcs(wanted) {
            Some(d) => d,
            None => return CacheCheck::OtherLanguage,
        },
        _ => rec.dlcs.as_deref().unwrap_or(&[]),
    };
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

/// The other languages that were whole before a download in `language` of
/// `build_id` (#389): the last record's language and its whole others, when the
/// cache had lost nothing and was for the same build.
fn whole_before(
    prior: Option<&Record>,
    check: &CacheCheck,
    build_id: &str,
    language: &str,
) -> Vec<String> {
    let Some(rec) = prior.filter(|r| r.build_id == build_id) else {
        return Vec::new();
    };
    if matches!(
        check,
        CacheCheck::Missing { .. }
            | CacheCheck::Unrecorded
            | CacheCheck::UnknownContent
            | CacheCheck::Interrupted { .. }
    ) {
        return Vec::new();
    }
    rec.whole_languages()
        .into_iter()
        .map(|(l, _)| l.to_string())
        .filter(|l| !l.eq_ignore_ascii_case(language))
        .collect()
}

/// The plan's estimate for a language switch (#386): only when the record names the
/// other language the cache holds; an old record without one has no estimate, and the
/// plan keeps the whole-build figure.
fn estimate_switch(
    check: &CacheCheck,
    prior: Option<&Record>,
    manifest: &Manifest,
    language: &str,
    dlcs: &[String],
) -> Option<u64> {
    let rec = prior?;
    if *check != CacheCheck::OtherLanguage {
        return None;
    }
    rec.language.as_ref()?;
    super::language::switch_download_held(manifest, &rec.whole_languages(), language, dlcs)
}

/// What the downloader is told for a run (design 19 §3.2/§3.3): kept apart
/// from the builder so the choice is testable without a network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadInputs {
    /// Diff against the same build with the recorded DLC (no hashing), not a
    /// full `verify`; against build `switch_from` when that is set.
    pub targeted: bool,
    /// A switch of builds (design 21 §3.5): diff from this build, the cache's,
    /// whose manifest the fetch is given.
    pub switch_from: Option<String>,
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
            switch_from: None,
            language: Some(language.to_string()),
            old_language: Some(language.to_string()),
            dlcs: all,
            old_dlcs: recorded.to_vec(),
        };
    }
    if generation == 1 {
        DownloadInputs {
            targeted: false,
            switch_from: None,
            language: None,
            old_language: Some(v1_language(language)),
            dlcs: Vec::new(),
            old_dlcs: wanted.to_vec(),
        }
    } else {
        DownloadInputs {
            targeted: false,
            switch_from: None,
            language: Some(language.to_string()),
            old_language: None,
            dlcs: wanted.to_vec(),
            old_dlcs: Vec::new(),
        }
    }
}

/// What a cache record says about its build, for a switch (design 21 §3.5).
#[derive(Debug, Clone, Copy)]
pub struct CachedBuild<'a> {
    pub build_id: &'a str,
    pub language: Option<&'a str>,
    /// `None`: a record from before DLC tracking.
    pub dlcs: Option<&'a [String]>,
}

/// Pure: whether a cache of another build can be switched to the wanted one by
/// diffing from its build (`upgrade_from`, design 21 §3.5) rather than hashing it.
/// Only a generation-2 build, from a record that names its language (the wanted
/// one) and DLC; the caller then needs the cached build's manifest.
pub fn can_switch(
    generation: u32,
    language: &str,
    check: &CacheCheck,
    cached: &CachedBuild<'_>,
) -> bool {
    generation == 2
        && *check == CacheCheck::OtherBuild
        && cached
            .language
            .is_some_and(|l| l.eq_ignore_ascii_case(language))
        && cached.dlcs.is_some()
}

/// Pure: the inputs of a switch from `cached` (see [`can_switch`]). The new side is
/// the wanted DLC; the old side the recorded ones still wanted, so a recorded DLC
/// the run doesn't take is left as it is (as `verify` leaves it), not deleted, and
/// the record drops it.
pub fn switch_inputs(
    language: &str,
    wanted: &[String],
    cached: &CachedBuild<'_>,
) -> DownloadInputs {
    let old_dlcs: Vec<String> = cached
        .dlcs
        .unwrap_or(&[])
        .iter()
        .filter(|d| wanted.contains(d))
        .cloned()
        .collect();
    DownloadInputs {
        targeted: true,
        switch_from: Some(cached.build_id.to_string()),
        language: Some(language.to_string()),
        old_language: Some(cached.language.unwrap_or(language).to_string()),
        dlcs: wanted.to_vec(),
        old_dlcs,
    }
}

/// The cached build's manifest from the game dir `game_root`'s
/// `build-manifest.json` (design 21 §5.2), when it is build `build_id`'s and of
/// generation 2.
pub fn kept_manifest(game_root: &Path, build_id: &str) -> Option<Manifest> {
    super::build_manifest::load(game_root)
        .filter(|s| s.build_id == build_id && s.generation == 2)
        .map(|s| s.manifest)
        .filter(|m| matches!(m, Manifest::V2(_)))
}

/// The manifest of the cache's build `from`: the one kept in the game dir, else
/// GOG's listing. `None` when neither has it: the switch verifies.
async fn cached_manifest(
    core: &Core,
    product_id: u64,
    from: &str,
    listed: &[Build],
) -> Option<Manifest> {
    let kept = crate::macapp::launch::GameDirs::for_game(&product_id.to_string())
        .ok()
        .and_then(|d| kept_manifest(&d.root, from));
    if kept.is_some() {
        return kept;
    }
    let b = listed.iter().find(|b| b.build_id() == from)?;
    if *b.generation() != 2 {
        return None;
    }
    core.get_manifest(b)
        .await
        .ok()
        .filter(|m| matches!(m, Manifest::V2(_)))
}

/// Run `switch` (a diff from the cache's build); when it fails, and the run
/// wasn't cancelled, say so and run `verify` instead (design 21 §3.5, #429):
/// gog-warp's move out of `!Temp` can fail on directory entries (#362). Its
/// `!Temp` (`warp_temp`, when [`warp_temp_dir`] vouched for it) is removed first:
/// what the switch left there is redone by the check. The cache record keeps its
/// `upgrading` marker throughout, so a cache that neither run finished is checked
/// next time.
pub async fn switch_or_verify<S, V>(
    warp_temp: Option<&Path>,
    from: &str,
    cancel: &CancellationToken,
    events: &tokio::sync::mpsc::UnboundedSender<DepotEvent>,
    switch: S,
    verify: impl FnOnce() -> V,
) -> Result<(u64, u64), DepotError>
where
    S: std::future::Future<Output = Result<(u64, u64), DepotError>>,
    V: std::future::Future<Output = Result<(u64, u64), DepotError>>,
{
    match switch.await {
        Err(e) if !cancel.is_cancelled() => {
            let _ = events.send(DepotEvent::Note {
                message: format!(
                    "switching the cache from build {from} by its changed files failed \
                     ({e}); checking the cache (hashing) and fetching what's missing instead"
                ),
            });
            if let Some(t) = warp_temp {
                let _ = std::fs::remove_dir_all(t);
            }
            verify().await
        }
        r => r,
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
    /// the old one for a targeted run; `old` is the cache's build for a switch.
    /// A switch without its old manifest verifies.
    pub fn apply(
        &self,
        mut b: Builder,
        manifest: &Manifest,
        build_id: &str,
        old: Option<&Manifest>,
    ) -> Builder {
        b = match (&self.switch_from, old) {
            (Some(from), Some(old)) if self.targeted => b.upgrade_from(old.clone(), from),
            (None, _) if self.targeted => b.upgrade_from(manifest.clone(), build_id),
            _ => b.verify(),
        };
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
        CacheCheck::Unrecorded => return None,
        CacheCheck::Complete => "cache holds this build, language and DLC: nothing to fetch \
             (a run still checks it by hashing)"
            .into(),
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
        CacheCheck::OtherLanguage => "cache holds another language: checking it (hashing) and \
             fetching the rest; the other language's files stay in the cache, and builds \
             leave them out"
            .into(),
        CacheCheck::OtherBuild => match &inputs.switch_from {
            Some(from) if inputs.targeted => format!(
                "cache holds build {from}: fetching only the files this build changed (no \
                 hashing) and deleting the ones it dropped"
            ),
            _ => "cache holds another build: checking it (hashing) and fetching the new one".into(),
        },
        CacheCheck::Interrupted { from, to } => format!(
            "an earlier switch from build {from} to {to} didn't finish: checking the cache \
             (hashing) and fetching what's missing"
        ),
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

    fn depot(
        language: &str,
        manifest: &str,
        paths: &[&str],
    ) -> super::super::dlc_filter::DepotList {
        super::super::dlc_filter::DepotList {
            product: "1".into(),
            languages: vec![language.into()],
            manifest: manifest.into(),
            paths: paths.iter().map(|p| p.to_string()).collect(),
            sizes: Default::default(),
            md5s: Default::default(),
        }
    }

    /// A switch keeps the old language (#389): not whole until settled, then whole
    /// unless a path it shares with the new one comes from another depot manifest.
    #[test]
    fn a_switch_keeps_the_old_language_and_settles_whether_it_is_whole() {
        let want = |language| CacheWant {
            build_id: Some("b1"),
            language: Some(language),
            dlcs: &[],
        };
        let t = recorded("b1", "en-US", &["5"]);
        store_products(
            t.path(),
            "b1",
            &[("1".to_string(), ids(&["Game/a.bin"]))].into(),
        )
        .unwrap();
        write_record(t.path(), "b1", "de-DE", &[]).unwrap();
        let rec = load_record(t.path()).unwrap();
        let (_, en) = rec.other("en-us").unwrap();
        assert!(!en.whole);
        assert_eq!(en.dlcs, ids(&["5"]));
        assert!(en.products.is_some(), "its lists stay");
        assert_eq!(rec.products, None, "de-DE has none yet");
        assert_eq!(
            check_cache(t.path(), &want("en-US")),
            CacheCheck::OtherLanguage
        );

        let neutral = depot("*", "n", &["Game/a.bin"]);
        // No clash: en-US is whole again, with its DLC.
        let fine = [
            neutral.clone(),
            depot("en-US", "e", &["Game/en.pak"]),
            depot("de-DE", "d", &["Game/de.pak"]),
        ];
        settle_languages(t.path(), "b1", &ids(&["en-US"]), &ids(&["1"]), &fine).unwrap();
        assert_eq!(check_cache(t.path(), &want("en-US")), CacheCheck::Complete);
        assert_eq!(
            check_cache(
                t.path(),
                &CacheWant {
                    dlcs: &ids(&["5", "6"]),
                    ..want("en-US")
                }
            ),
            CacheCheck::MissingDlc {
                missing: ids(&["6"])
            },
            "against en-US's own DLC"
        );
        assert_eq!(
            record_products_in(t.path(), "b1", "de-DE").unwrap()["1"],
            ids(&["Game/a.bin", "Game/de.pak"])
        );
        // Settled again, only from what was whole: a language not whole stays so.
        settle_languages(t.path(), "b1", &[], &ids(&["1"]), &fine).unwrap();
        assert_eq!(
            check_cache(t.path(), &want("en-US")),
            CacheCheck::OtherLanguage
        );
        // One path, two depot manifests: the cache holds de-DE's bytes there.
        let clash = [
            neutral,
            depot("en-US", "e", &["Game/voice.pak"]),
            depot("de-DE", "d", &["Game/VOICE.pak"]),
        ];
        settle_languages(t.path(), "b1", &ids(&["en-US"]), &ids(&["1"]), &clash).unwrap();
        assert_eq!(
            check_cache(t.path(), &want("en-US")),
            CacheCheck::OtherLanguage
        );

        // Another build: the old languages stay held, with nothing claimed.
        write_record(t.path(), "b2", "de-DE", &[]).unwrap();
        let rec = load_record(t.path()).unwrap();
        assert_eq!(
            rec.other("en-US").map(|(_, h)| h),
            Some(&HeldLanguage::default())
        );
    }

    /// The switch estimate counts a depot any whole language has as had (#389).
    #[test]
    fn the_switch_estimate_counts_every_whole_language() {
        let m: Manifest = serde_json::from_value(serde_json::json!({
            "baseProductId": "1", "installDirectory": "G", "platform": "windows",
            "products": [], "depots": [
                {"size": 9, "compressedSize": 9, "languages": ["en-US"], "manifest": "e", "productId": "1"},
                {"size": 5, "compressedSize": 5, "languages": ["de-DE", "fr-FR"], "manifest": "d", "productId": "1"},
                {"size": 3, "compressedSize": 3, "languages": ["fr-FR"], "manifest": "f", "productId": "1"}]
        }))
        .unwrap();
        let mut rec = Record {
            build_id: "b".into(),
            version: None,
            generation: None,
            upgrading: None,
            language: Some("en-US".into()),
            dlcs: Some(vec![]),
            products: None,
            other_languages: BTreeMap::new(),
            languages_scanned: false,
            files: vec![],
        };
        let est =
            |r: &Record| estimate_switch(&CacheCheck::OtherLanguage, Some(r), &m, "fr-FR", &[]);
        assert_eq!(est(&rec), Some(8));
        rec.other_languages.insert(
            "de-DE".into(),
            HeldLanguage {
                whole: false,
                ..Default::default()
            },
        );
        assert_eq!(est(&rec), Some(8), "not whole: not counted");
        rec.other_languages.get_mut("de-DE").unwrap().whole = true;
        assert_eq!(est(&rec), Some(3));
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

    /// The plan's note for each answer of [`check_cache`] (#388): only a complete
    /// cache says nothing is fetched, and only an unrecorded one says nothing.
    #[test]
    fn the_plan_notes_each_cache_answer() {
        let note = |check: CacheCheck| {
            let inputs = download_inputs(2, "en-US", &ids(&["5"]), &check, &[]);
            cache_note(&check, &inputs)
        };
        let complete = note(CacheCheck::Complete).unwrap();
        assert!(complete.contains("nothing to fetch"), "{complete}");
        assert_eq!(note(CacheCheck::Unrecorded), None);
        for (check, says) in [
            (
                CacheCheck::Missing {
                    shown: ids(&["Game/a.bin"]),
                    count: 1,
                },
                "cache lost 1 file(s)",
            ),
            (CacheCheck::UnknownContent, "predates DLC tracking"),
            (
                CacheCheck::MissingDlc {
                    missing: ids(&["5"]),
                },
                "fetching only those (no hashing)",
            ),
            (CacheCheck::OtherLanguage, "another language"),
            (CacheCheck::OtherBuild, "another build"),
        ] {
            let n = note(check.clone()).unwrap();
            assert!(n.contains(says), "{check:?}: {n}");
            assert!(!n.contains("nothing to fetch"), "{check:?}: {n}");
        }
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
                switch_from: None,
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
    fn file_lists_stay_for_the_same_build_and_language_only() {
        let t = recorded("b1", "en-US", &["5"]);
        let root = t.path();
        assert!(record_products(root, "b1").is_none());
        let mut lists = super::super::dlc_filter::ProductFiles::new();
        lists.insert("1".into(), vec!["Game/a".into()]);
        store_products(root, "b1", &lists).unwrap();
        // Another build's record takes none.
        store_products(root, "b2", &Default::default()).unwrap();
        let mut more = super::super::dlc_filter::ProductFiles::new();
        more.insert("5".into(), vec!["Game/b".into()]);
        store_products(root, "b1", &more).unwrap();
        let have = record_products(root, "b1").unwrap();
        assert_eq!(have.keys().collect::<Vec<_>>(), ["1", "5"]);
        assert!(record_products(root, "b2").is_none());
        // A rewrite (a completion, a `dlc add`) keeps them; another build drops them.
        write_record(root, "b1", "en-US", &ids(&["5", "7"])).unwrap();
        assert_eq!(record_products(root, "b1"), Some(have));
        write_record(root, "b1", "de-DE", &[]).unwrap();
        assert!(record_products(root, "b1").is_none());
        // Records without lists still read.
        assert_eq!(read_record(root).unwrap().build_id, "b1");
    }

    #[test]
    fn switch_estimate_needs_a_record_naming_another_language() {
        let m: Manifest = serde_json::from_value(serde_json::json!({
            "baseProductId": "1", "installDirectory": "G", "platform": "windows",
            "products": [], "depots": [
                {"size": 9, "compressedSize": 9, "languages": ["en-US"], "manifest": "e", "productId": "1"},
                {"size": 5, "compressedSize": 5, "languages": ["de-DE"], "manifest": "d", "productId": "1"}]
        }))
        .unwrap();
        let rec = |language: Option<&str>| Record {
            build_id: "b".into(),
            version: None,
            generation: None,
            upgrading: None,
            language: language.map(Into::into),
            dlcs: Some(vec![]),
            products: None,
            other_languages: BTreeMap::new(),
            languages_scanned: false,
            files: vec![],
        };
        let other = CacheCheck::OtherLanguage;
        let est = |c: &CacheCheck, r: Option<&Record>, l: &str| estimate_switch(c, r, &m, l, &[]);
        assert_eq!(est(&other, Some(&rec(Some("en-US"))), "de-DE"), Some(5));
        // The same language, an old record, no record: no estimate.
        assert_eq!(
            est(&CacheCheck::Complete, Some(&rec(Some("en-US"))), "en-US"),
            None
        );
        assert_eq!(
            est(&CacheCheck::UnknownContent, Some(&rec(None)), "de-DE"),
            None
        );
        assert_eq!(est(&CacheCheck::Unrecorded, None, "de-DE"), None);
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
        // A complete cache says nothing is fetched (#388).
        assert!(
            cache_note(&CacheCheck::Complete, &f)
                .unwrap()
                .contains("nothing to fetch")
        );
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
                switch_from: None,
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
    fn an_asked_language_is_checked_against_the_build_before_anything_runs() {
        use crate::gog::language::fixtures::v2;
        let m = v2(&[("1", &["*"]), ("1", &["en-US"]), ("1", &["de-DE"])]);
        // Unknown: refused (a usage error), with the valid codes.
        let e = pick_language(&m, "xx-XX", true).unwrap_err();
        assert!(matches!(e, DepotError::Language(_)));
        assert!(e.to_string().contains("de-DE, en-US"), "{e}");
        // A short form resolves; case is ignored.
        assert_eq!(pick_language(&m, "DE", true).unwrap(), "de-DE");
        // The default isn't asked for, so a build without it still builds.
        assert_eq!(pick_language(&m, "fr-FR", false).unwrap(), "fr-FR");
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
            estimated_download_size: None,
            cache_complete: false,
            disk_size: 2,
            language: "en-US".into(),
            requested_language: None,
            dlcs: vec![],
            cache: None,
            game_files: None,
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"type":"plan","installDir":"/c/T","downloadSize":1,"diskSize":2,"language":"en-US"}"#
        );
        if let DepotEvent::Plan {
            estimated_download_size,
            ..
        } = &mut e
        {
            *estimated_download_size = Some(1);
        }
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"type":"plan","installDir":"/c/T","downloadSize":1,"diskSize":2,"language":"en-US","estimatedDownloadSize":1}"#
        );
        // A complete cache (#388): `downloadSize` stays the build's size.
        if let DepotEvent::Plan {
            estimated_download_size,
            cache_complete,
            ..
        } = &mut e
        {
            *estimated_download_size = Some(0);
            *cache_complete = true;
        }
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"type":"plan","installDir":"/c/T","downloadSize":1,"diskSize":2,"language":"en-US","estimatedDownloadSize":0,"cacheComplete":true}"#
        );
        if let DepotEvent::Plan {
            game_files,
            estimated_download_size,
            cache_complete,
            ..
        } = &mut e
        {
            *estimated_download_size = None;
            *cache_complete = false;
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

    // ---- the record's build stamp (design 21 §5.3) ----

    #[test]
    fn a_record_keeps_the_builds_version_and_generation() {
        let t = tempfile::tempdir().unwrap();
        let stamp = BuildStamp {
            build_id: "58989493373906337",
            version: Some("2.31a"),
            generation: Some(2),
        };
        write_record_build(t.path(), &stamp, "en-US", &[]).unwrap();
        let r = read_record(t.path()).unwrap();
        assert_eq!(r.version.as_deref(), Some("2.31a"));
        assert_eq!(r.generation, Some(2));
        // A rewrite for the same build that knows no stamp (a DLC add) keeps it.
        write_record(t.path(), "58989493373906337", "en-US", &["7".into()]).unwrap();
        let r = read_record(t.path()).unwrap();
        assert_eq!(r.version.as_deref(), Some("2.31a"));
        assert_eq!(r.generation, Some(2));
        // Another build never inherits it.
        write_record(t.path(), "60152385347545148", "en-US", &[]).unwrap();
        let r = read_record(t.path()).unwrap();
        assert_eq!((r.version, r.generation), (None, None));
    }

    #[test]
    fn a_record_an_older_gogo_wrote_reads_without_a_stamp_and_omits_it() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(
            t.path().join(RECORD),
            r#"{"buildId":"b1","language":"en-US","dlcs":[],"files":[]}"#,
        )
        .unwrap();
        let r = read_record(t.path()).unwrap();
        assert_eq!((r.version, r.generation), (None, None));
        // An unstamped record doesn't gain empty keys, so an older gogo reads
        // what it always did.
        write_record(t.path(), "b1", "en-US", &[]).unwrap();
        let raw = std::fs::read_to_string(t.path().join(RECORD)).unwrap();
        assert!(
            !raw.contains("version") && !raw.contains("generation"),
            "{raw}"
        );
    }

    // ---- a switch of builds that never finished (design 21 §3.5) ----

    #[test]
    fn a_record_marked_as_switching_is_never_complete() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::create_dir_all(root.join("Game")).unwrap();
        std::fs::write(root.join("Game/a.bin"), b"x").unwrap();
        write_record(root, "b1", "en-US", &[]).unwrap();
        let want = |b| CacheWant {
            build_id: b,
            language: Some("en-US"),
            dlcs: &[],
        };
        assert_eq!(check_cache(root, &want(Some("b1"))), CacheCheck::Complete);

        // A download of b2 onto a cache of b1 marks the record first. If it stops,
        // every later look sees a switch in progress, for either build and for none.
        mark_upgrading(root, "b2").unwrap();
        let stuck = CacheCheck::Interrupted {
            from: "b1".into(),
            to: "b2".into(),
        };
        for b in [Some("b1"), Some("b2"), None] {
            assert_eq!(check_cache(root, &want(b)), stuck, "{b:?}");
        }
        // Marking again (a second interrupted run) keeps the first `from`.
        mark_upgrading(root, "b3").unwrap();
        assert_eq!(check_cache(root, &want(None)), stuck);
        // The next run hashes the tree: no targeted fetch, and the plan says why.
        let inputs = download_inputs(2, "en-US", &[], &stuck, &[]);
        assert!(!inputs.targeted);
        let note = cache_note(&stuck, &inputs).unwrap();
        assert!(
            note.contains("b1") && note.contains("didn't finish"),
            "{note}"
        );

        // The record the finished download writes clears it.
        write_record_build(
            root,
            &BuildStamp {
                build_id: "b2",
                version: Some("2.0"),
                generation: Some(2),
            },
            "en-US",
            &[],
        )
        .unwrap();
        assert_eq!(check_cache(root, &want(Some("b2"))), CacheCheck::Complete);
        let raw = std::fs::read_to_string(root.join(RECORD)).unwrap();
        assert!(!raw.contains("upgrading"), "{raw}");
    }

    #[test]
    fn a_repair_of_the_same_build_or_a_cold_cache_leaves_no_mark() {
        let t = tempfile::tempdir().unwrap();
        mark_upgrading(t.path(), "b1").unwrap();
        assert!(load_record(t.path()).is_none());
        write_record(t.path(), "b1", "en-US", &[]).unwrap();
        mark_upgrading(t.path(), "b1").unwrap();
        assert!(
            !std::fs::read_to_string(t.path().join(RECORD))
                .unwrap()
                .contains("upgrading")
        );
    }

    fn cached<'a>(language: Option<&'a str>, dlcs: Option<&'a [String]>) -> CachedBuild<'a> {
        CachedBuild {
            build_id: "b1",
            language,
            dlcs,
        }
    }

    #[test]
    fn only_another_generation_2_build_in_the_same_language_with_known_dlc_switches() {
        let none: &[String] = &[];
        let ok = cached(Some("en-US"), Some(none));
        assert!(can_switch(2, "en-US", &CacheCheck::OtherBuild, &ok));
        assert!(can_switch(
            2,
            "EN-us",
            &CacheCheck::OtherBuild,
            &cached(Some("en-US"), Some(none))
        ));
        // Generation 1, another language, a record without language or DLC: hashed.
        assert!(!can_switch(1, "en-US", &CacheCheck::OtherBuild, &ok));
        assert!(!can_switch(2, "de-DE", &CacheCheck::OtherBuild, &ok));
        assert!(!can_switch(
            2,
            "en-US",
            &CacheCheck::OtherBuild,
            &cached(None, Some(none))
        ));
        assert!(!can_switch(
            2,
            "en-US",
            &CacheCheck::OtherBuild,
            &cached(Some("en-US"), None)
        ));
        // Anything but a whole cache of another build: an interrupted switch, lost
        // files, the same build lacking DLC, are never diffed from the record.
        for check in [
            CacheCheck::Interrupted {
                from: "b0".into(),
                to: "b1".into(),
            },
            CacheCheck::Missing {
                shown: vec!["a".into()],
                count: 1,
            },
            CacheCheck::Unrecorded,
            CacheCheck::Complete,
            CacheCheck::OtherLanguage,
            CacheCheck::UnknownContent,
            CacheCheck::MissingDlc {
                missing: vec!["7".into()],
            },
        ] {
            assert!(!can_switch(2, "en-US", &check, &ok), "{check:?}");
        }
    }

    #[test]
    fn a_switch_diffs_from_the_cached_build_and_the_dlc_still_wanted() {
        let held = ["7".to_string(), "8".to_string()];
        let c = cached(Some("en-US"), Some(&held));
        let got = switch_inputs("en-US", &["8".into(), "9".into()], &c);
        assert_eq!(
            got,
            DownloadInputs {
                targeted: true,
                switch_from: Some("b1".into()),
                language: Some("en-US".into()),
                old_language: Some("en-US".into()),
                // 9 is new: fetched whole; 7 isn't wanted: left alone, not deleted.
                dlcs: vec!["8".into(), "9".into()],
                old_dlcs: vec!["8".into()],
            }
        );
        let note = cache_note(&CacheCheck::OtherBuild, &got).unwrap();
        assert!(note.contains("b1") && note.contains("no hashing"), "{note}");
        // Without the old manifest it's the hashing path, and the plan says so.
        let v = download_inputs(2, "en-US", &[], &CacheCheck::OtherBuild, &[]);
        assert!(v.switch_from.is_none() && !v.targeted);
        assert!(
            cache_note(&CacheCheck::OtherBuild, &v)
                .unwrap()
                .contains("hashing")
        );
    }

    fn v2_manifest() -> Manifest {
        serde_json::from_value(serde_json::json!({
            "baseProductId": "1", "installDirectory": "G", "platform": "windows",
            "products": [{"name": "G", "productId": "1", "temp_executable": "", "temp_arguments": ""}],
            "depots": [{"size": 9, "compressedSize": 4, "languages": ["en-US"],
                        "manifest": "e", "productId": "1"}]
        }))
        .unwrap()
    }

    #[test]
    fn the_kept_manifest_serves_only_its_own_generation_2_build() {
        let d = tempfile::tempdir().unwrap();
        assert!(kept_manifest(d.path(), "b1").is_none());
        let m = v2_manifest();
        assert!(matches!(m, Manifest::V2(_)));
        super::super::build_manifest::write(d.path(), "b1", 2, &m).unwrap();
        assert!(kept_manifest(d.path(), "b1").is_some());
        assert!(kept_manifest(d.path(), "b2").is_none());
        super::super::build_manifest::write(d.path(), "b1", 1, &m).unwrap();
        assert!(kept_manifest(d.path(), "b1").is_none());
    }

    /// A cache of b1 switching to b2, marked as `run` marks it.
    fn switching_cache() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::create_dir_all(root.join("Game/Data")).unwrap();
        std::fs::write(root.join("Game/Data/a.bin"), b"x").unwrap();
        write_record(root, "b1", "en-US", &[]).unwrap();
        mark_upgrading(root, "b2").unwrap();
        t
    }

    fn stuck() -> CacheCheck {
        CacheCheck::Interrupted {
            from: "b1".into(),
            to: "b2".into(),
        }
    }

    fn notes(rx: &mut tokio::sync::mpsc::UnboundedReceiver<DepotEvent>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(e) = rx.try_recv() {
            if let DepotEvent::Note { message } = e {
                out.push(message);
            }
        }
        out
    }

    #[tokio::test]
    async fn a_failed_switch_says_why_and_verifies_instead() {
        let t = switching_cache();
        let root = t.path();
        // What gog-warp's move loop leaves in the game tree's `!Temp` when a
        // directory rename fails (#362, seen on a real switch).
        let game = root.join("Game");
        std::fs::create_dir_all(game.join("!Temp/Data")).unwrap();
        std::fs::write(game.join("!Temp/Data/b.bin"), b"y").unwrap();
        std::fs::write(game.join("!Temp/.gog-warp-build"), b"b2").unwrap();
        // The record never lists it.
        write_record(root, "b1", "en-US", &[]).unwrap();
        mark_upgrading(root, "b2").unwrap();
        let rec = load_record(root).unwrap();
        assert_eq!(rec.files, ["Game/Data/a.bin"]);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let verified = std::cell::Cell::new(false);
        let got = switch_or_verify(
            Some(&game.join("!Temp")),
            "b1",
            &cancel,
            &tx,
            async {
                Err(DepotError::Library(
                    "Directory not empty (os error 39)".into(),
                ))
            },
            || async {
                verified.set(true);
                // The check runs on a cache without the switch's leftovers.
                assert!(!game.join("!Temp").exists());
                assert!(game.join("Data/a.bin").exists());
                Ok((3, 30))
            },
        )
        .await
        .unwrap();
        assert_eq!(got, (3, 30));
        assert!(verified.get());
        let n = notes(&mut rx);
        assert_eq!(n.len(), 1, "{n:?}");
        assert!(
            n[0].contains("b1") && n[0].contains("os error 39") && n[0].contains("hashing"),
            "{n:?}"
        );
        // Until the record of the finished download replaces it, the cache is
        // still switching: never complete.
        assert_eq!(check_cache(root, &CacheWant::default()), stuck());
    }

    #[tokio::test]
    async fn a_switch_and_its_check_both_failing_leave_the_cache_marked() {
        let t = switching_cache();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let got = switch_or_verify(
            Some(&t.path().join("Game/!Temp")),
            "b1",
            &cancel,
            &tx,
            async { Err(DepotError::Library("rename failed".into())) },
            || async { Err(DepotError::Library("network".into())) },
        )
        .await;
        assert!(matches!(got, Err(DepotError::Library(e)) if e == "network"));
        // The next run sees the switch that didn't finish and hashes the tree.
        let check = check_cache(t.path(), &CacheWant::default());
        assert_eq!(check, stuck());
        let none: &[String] = &[];
        assert!(!can_switch(
            2,
            "en-US",
            &check,
            &cached(Some("en-US"), Some(none))
        ));
        assert!(!download_inputs(2, "en-US", &[], &check, &[]).targeted);
    }

    #[tokio::test]
    async fn a_cancelled_or_finished_switch_runs_no_check() {
        for (cancelled, result) in [
            (true, Err(DepotError::Library("cancelled".into()))),
            (false, Ok((5, 50))),
        ] {
            let t = switching_cache();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let cancel = CancellationToken::new();
            if cancelled {
                cancel.cancel();
            }
            let ok = result.is_ok();
            let got = switch_or_verify(
                Some(&t.path().join("Game/!Temp")),
                "b1",
                &cancel,
                &tx,
                async { result },
                || async { panic!("no check after a cancel or a finished switch") },
            )
            .await;
            assert_eq!(got.is_ok(), ok);
            assert!(notes(&mut rx).is_empty());
            // Ctrl-C mid-switch: the marker stays, and the next run checks.
            if cancelled {
                assert_eq!(check_cache(t.path(), &CacheWant::default()), stuck());
            }
        }
    }

    fn manifest_in(install_dir: &str) -> Manifest {
        serde_json::from_value(serde_json::json!({
            "baseProductId": "1", "installDirectory": install_dir, "platform": "windows",
            "products": [{"name": "G", "productId": "1", "temp_executable": "", "temp_arguments": ""}],
            "depots": []
        }))
        .unwrap()
    }

    #[test]
    fn a_switch_needs_both_builds_in_the_same_install_dir() {
        let a = manifest_in("Chaos Gate");
        assert!(same_install_dir(&a, &manifest_in("Chaos Gate")));
        assert!(same_install_dir(&a, &manifest_in("chaos gate")));
        assert!(same_install_dir(&manifest_in("A\\B"), &manifest_in("a/b/")));
        // Chaos Gate's v5 build lays its tree in `chaosgate`: no diff across.
        assert!(!same_install_dir(&a, &manifest_in("chaosgate")));
    }

    #[test]
    fn the_temp_dir_is_only_cleared_under_a_plain_install_dir() {
        let root = Path::new("/cache/1");
        assert_eq!(
            warp_temp_dir(root, &manifest_in("WH40K Chaos Gate")).unwrap(),
            root.join("WH40K Chaos Gate/!Temp")
        );
        assert_eq!(
            warp_temp_dir(root, &manifest_in("A\\B")).unwrap(),
            root.join("A/B/!Temp")
        );
        for bad in ["", "/etc", "\\etc", "../x", "a/../../x", ".", "a/./b"] {
            let why = warp_temp_dir(root, &manifest_in(bad)).unwrap_err();
            assert!(why.contains("not clearing"), "{bad:?}: {why}");
        }
    }

    fn listed(paths: &[&str]) -> super::super::dlc_filter::DepotList {
        super::super::dlc_filter::DepotList {
            product: "1".into(),
            languages: vec!["*".into()],
            manifest: "m".into(),
            paths: paths.iter().map(|p| p.to_string()).collect(),
            sizes: BTreeMap::new(),
            md5s: Default::default(),
        }
    }

    #[test]
    fn a_failed_switch_removes_only_what_the_old_build_had_and_the_new_dropped() {
        let old = [
            listed(&["G/a.bin", "G/Old.dat", "G/Data/x.bin"]),
            listed(&["gog-support/1/1/s.ini", "G/../escape.txt"]),
        ];
        let new = [listed(&["G/a.bin", "G/data/X.BIN", "G/new.bin"])];
        let dropped = dropped_paths(&old, &new);
        assert_eq!(
            dropped,
            ["G/../escape.txt", "G/Old.dat", "gog-support/1/1/s.ini"]
        );

        let t = tempfile::tempdir().unwrap();
        let cache = t.path().join("cache");
        for f in ["G/a.bin", "G/Old.dat", "G/Data/x.bin", "G/mine.sav"] {
            let p = cache.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"x").unwrap();
        }
        // Outside the cache, reachable only through `..`.
        std::fs::write(t.path().join("escape.txt"), b"keep").unwrap();
        // A dropped path that is a dir on disk is never removed.
        std::fs::create_dir_all(cache.join("gog-support/1/1/s.ini")).unwrap();

        assert_eq!(remove_dropped(&cache, &dropped), 1);
        assert!(!cache.join("G/Old.dat").exists());
        assert!(t.path().join("escape.txt").exists());
        assert!(cache.join("gog-support/1/1/s.ini").is_dir());
        // What both builds list, and what neither lists, stay.
        for f in ["G/a.bin", "G/Data/x.bin", "G/mine.sav"] {
            assert!(cache.join(f).exists(), "{f}");
        }
    }
}
