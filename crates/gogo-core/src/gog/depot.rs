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
use gog_warp::content_system::downloader::Downloader;
use gog_warp::content_system::downloader::progress::DownloadState;
use gog_warp::content_system::types::{Build, Platform};

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
        #[serde(skip_serializing_if = "Vec::is_empty")]
        dlcs: Vec<String>,
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
    },
}

#[derive(thiserror::Error, Debug)]
pub enum DepotError {
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

/// Driver options (design 14 §1); owned by the CLI layer.
#[derive(Clone)]
pub struct DepotOptions {
    pub language: String,
    pub with_dlcs: bool,
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
            with_dlcs: false,
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
    let dlcs = if opts.with_dlcs {
        manifest.dlcs()
    } else {
        Vec::new()
    };
    let (download_size, disk_size) = manifest.install_size(&opts.language, &dlcs, None);
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
        dlcs: dlcs.clone(),
        game_files,
    });
    if opts.dry_run {
        return Ok(install_dir);
    }

    // The preflight (and the downloader) expect the install root to exist;
    // nothing creates it before this point (live finding 2026-10-20: the
    // free-space probe on a missing path is ENOENT, not 0 free).
    std::fs::create_dir_all(&install_root).map_err(DepotError::Io)?;
    let deps_manifest = core
        .get_dependencies_manifest()
        .await
        .map_err(DepotError::library)?;

    let mut builder = Downloader::builder()
        .core(core.clone())
        .manifest(manifest, &build_id)
        .install_root(install_root.clone())
        .support_root(support_root)
        .global_dependencies_root(deps_root)
        .game_dependencies(deps_manifest)
        .verify();
    if generation == 1 {
        builder = builder.old_language(v1_language(&opts.language));
        if opts.with_dlcs {
            builder = builder.old_dlcs(dlcs);
        }
    } else {
        builder = builder.language(opts.language.clone());
        if opts.with_dlcs {
            builder = builder.dlcs(dlcs);
        }
    }
    let mut downloader = builder.build().map_err(DepotError::library)?;

    if let Some(bps) = opts.limit {
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

    if opts.disk_check {
        let required = downloader
            .get_required_space()
            .await
            .map_err(DepotError::library)?
            .max(0) as u64;
        let free = free_space
            .available_bytes(&install_root)
            .map_err(DepotError::Io)?;
        if required > free {
            return Err(DepotError::Library(format!(
                "insufficient space: need {required} bytes, have {free}"
            )));
        }
    }

    downloader.download().await.map_err(DepotError::library)?;
    // The download is done either way; without a record (a full disk, say)
    // the next reuse just checks it again.
    if write_record(&install_root, &build_id).is_err() {
        let _ = std::fs::remove_file(install_root.join(RECORD));
    }
    let _ = events.send(DepotEvent::Finished {
        install_dir: install_dir.clone(),
    });
    Ok(install_dir)
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
    /// Paths relative to the cache root, `/`-separated, sorted.
    files: Vec<String>,
}

/// What a cache root's record says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheCheck {
    /// Everything the download left is still there.
    Complete,
    /// No record: an older gogo downloaded it, or the download never
    /// finished. Whether it's whole is unknown.
    Unrecorded,
    /// These recorded paths are gone (at most [`MISSING_SHOWN`] of them,
    /// plus the count of all).
    Missing { shown: Vec<String>, count: usize },
}

/// How many missing paths [`CacheCheck::Missing`] names.
pub const MISSING_SHOWN: usize = 5;

/// Record what's in `root` after a finished download (overwrites the last).
pub fn write_record(root: &Path, build_id: &str) -> std::io::Result<()> {
    let mut files = Vec::new();
    walk(root, root, &mut files)?;
    files.sort();
    let rec = Record {
        build_id: build_id.to_string(),
        files,
    };
    let text = serde_json::to_string_pretty(&rec).map_err(std::io::Error::other)?;
    let tmp = root.join(format!("{RECORD}.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, root.join(RECORD))
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

/// Check the cache root `root` against its record. An unreadable record
/// counts as none.
pub fn check_cache(root: &Path) -> CacheCheck {
    let Ok(text) = std::fs::read_to_string(root.join(RECORD)) else {
        return CacheCheck::Unrecorded;
    };
    let Ok(rec) = serde_json::from_str::<Record>(&text) else {
        return CacheCheck::Unrecorded;
    };
    let missing: Vec<String> = rec
        .files
        .into_iter()
        .filter(|f| root.join(f).symlink_metadata().is_err())
        .collect();
    if missing.is_empty() {
        CacheCheck::Complete
    } else {
        CacheCheck::Missing {
            count: missing.len(),
            shown: missing.into_iter().take(MISSING_SHOWN).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_round_trip_and_missing_files() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        assert_eq!(check_cache(root), CacheCheck::Unrecorded);
        std::fs::create_dir_all(root.join("Game/Apps")).unwrap();
        std::fs::create_dir_all(root.join("gog-support/1/1/app")).unwrap();
        std::fs::write(root.join("Game/Apps/game.exe"), b"x").unwrap();
        std::fs::write(root.join("gog-support/1/1/app/game.ini"), b"y").unwrap();
        std::fs::write(root.join(WARP_MARKER), b"b1").unwrap();
        std::fs::write(root.join("Game/x.bin.download"), b"").unwrap();
        std::fs::write(root.join("Game/.DS_Store"), b"").unwrap();
        write_record(root, "b1").unwrap();

        let rec: Record =
            serde_json::from_str(&std::fs::read_to_string(root.join(RECORD)).unwrap()).unwrap();
        assert_eq!(rec.build_id, "b1");
        assert_eq!(
            rec.files,
            ["Game/Apps/game.exe", "gog-support/1/1/app/game.ini"]
        );
        assert_eq!(check_cache(root), CacheCheck::Complete);

        std::fs::remove_dir_all(root.join("gog-support")).unwrap();
        assert_eq!(
            check_cache(root),
            CacheCheck::Missing {
                shown: vec!["gog-support/1/1/app/game.ini".into()],
                count: 1
            }
        );
    }

    #[test]
    fn unreadable_record_is_unrecorded() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join(RECORD), "not json").unwrap();
        assert_eq!(check_cache(t.path()), CacheCheck::Unrecorded);
    }

    #[cfg(unix)]
    #[test]
    fn links_are_recorded_not_followed() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::create_dir_all(root.join("Game")).unwrap();
        std::os::unix::fs::symlink("nowhere", root.join("Game/link")).unwrap();
        write_record(root, "b").unwrap();
        assert_eq!(check_cache(root), CacheCheck::Complete);
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
