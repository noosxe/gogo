//! Depot (Model B) downloads via gog-warp — `docs/design/14`.
//!
//! Thin driver over gog-warp's `Downloader`: resolve the latest public Windows
//! build, plan, and materialize the game tree into the user cache directory.
//! The heavy lifting (depots, secure links, chunk reconstruction, verification)
//! is the library's; this module owns selection, layout, events, and limits.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tokio_util::sync::CancellationToken;

use gog_warp::Core;
use gog_warp::content_system::downloader::Downloader;
use gog_warp::content_system::downloader::progress::DownloadState;
use gog_warp::content_system::types::{Build, Platform};

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
    #[error("depot downloads are not implemented on {0} yet")]
    UnsupportedPlatform(&'static str),
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

/// Pure: `DownloadState` → `DepotEvent` (design 14 D4).
/// Pure: `DownloadState` → `DepotEvent` (design 14 D4). The library's own
/// `Finished` is skipped — we emit ours (with the install dir) after
/// `download()` resolves.
pub fn state_event(s: DownloadState) -> Option<DepotEvent> {
    match s {
        DownloadState::Preparing => Some(DepotEvent::Preparing),
        DownloadState::Allocating(f) => Some(DepotEvent::Allocating { fraction: f }),
        DownloadState::Verifying(f) => Some(DepotEvent::Verifying { fraction: f }),
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

    // D7: the materialization lane is macOS-only for now; resolve+plan is
    // allowed everywhere, but no directory may be created elsewhere.
    let materialize = cfg!(target_os = "macos");
    if !materialize && !opts.dry_run {
        return Err(DepotError::UnsupportedPlatform("this platform"));
    }

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
    let _ = events.send(DepotEvent::Plan {
        install_dir: install_dir.clone(),
        download_size,
        disk_size,
        language: opts.language.clone(),
        dlcs: dlcs.clone(),
    });
    if opts.dry_run {
        return Ok(install_dir);
    }
    if !materialize {
        return Err(DepotError::UnsupportedPlatform("this platform"));
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
    let _ = events.send(DepotEvent::Finished {
        install_dir: install_dir.clone(),
    });
    Ok(install_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
