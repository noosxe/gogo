//! Runtime catalog (design 12 §3): entries are **data + live probes**, never
//! code paths. Every downloadable entry is re-probed at use time — dead
//! upstreams stay as legible tombstones instead of mystery 404s (docs/15 §1,
//! decision D1). gogo downloads only license-clean entries; GPTK is
//! consent-gated (D9) and never redistributed in gogo's own artifacts.

use serde_json::Value;
use std::time::Duration;

use super::error::MacAppError;

/// GitHub REST root (injectable so tests run against wiremock).
pub const GITHUB_API: &str = "https://api.github.com";

const UA: &str = concat!("gogo/", env!("CARGO_PKG_VERSION"), " (runtime catalog)");

/// Entry kind: a Wine runtime, a tool provisioned alongside one, or a
/// renderer payload.
pub enum Kind {
    Wine,
    /// A tool payload; `payload` names the file that proves the extract
    /// (design 13 D2: the `winetricks` script).
    Tool {
        payload: &'static str,
    },
    /// A renderer (design 16 decisions 3 and 6), stored as downloaded under
    /// `renderers/<id>/<ver>/<arch>/`. `payload` proves the extract like a
    /// tool's. `overlay`: it replaces Wine builtins and is installed as a
    /// derived runtime (DXMT); otherwise it goes into the game dir
    /// (cnc-ddraw).
    Renderer {
        payload: &'static str,
        overlay: bool,
    },
}

/// Static claim about an entry; the live probe refines availability.
pub enum Status {
    /// Downloadable, license-clean.
    Available,
    /// Download gated behind explicit consent (GPTK / Apple EULA — D9).
    ConsentRequired,
    /// Tool provisioning (design 12 §8) — lands with the shims phase.
    Tool,
    /// Upstream gone — tombstone; cached copies remain usable.
    Gone {
        since: &'static str,
        story: &'static str,
    },
}

/// How the artifact is discovered.
pub enum Discovery {
    /// GitHub releases: assets matched by prefix/suffix — tags may carry `_N`
    /// suffixes (`11.0_1`), which the match tolerates (docs/15 §8.2).
    GitHubReleases {
        repo: &'static str,
        asset_prefix: &'static str,
        asset_suffix: &'static str,
    },
    /// One exact asset, pinned by sha256 (docs/19 §6.1). Sikarugir publishes
    /// every engine under a single `v1.0` release, so neither the tag nor
    /// "latest" says anything about the version: the asset name does.
    Pinned(PinnedAsset),
}

/// An exact GitHub release asset and the sha256 gogo expects. GitHub's own
/// asset digest, when present, must agree with it.
pub struct PinnedAsset {
    pub repo: &'static str,
    pub tag: &'static str,
    pub asset: &'static str,
    /// The store version this asset installs as (`runtimes/<id>/<version>`).
    pub version: &'static str,
    pub sha256: &'static str,
}

/// The dylib closure an engine is not standalone without (#62): the
/// top-level `*.dylib` files (and their symlinks) under `dir` inside `asset`,
/// installed as `runtimes/<id>/<ver>/Frameworks/`. Frameworks bundles
/// (`GStreamer.framework`, …) under `dir` are left out.
pub struct FrameworksSet {
    pub asset: PinnedAsset,
    pub dir: &'static str,
    /// `(file-name prefix, component and licence)`, for the store entry's
    /// `NOTICES.txt` and the bundles that use it. gogo downloads these on the
    /// user's machine; it doesn't redistribute them.
    pub notices: &'static [(&'static str, &'static str)],
}

/// One catalog entry (design 12 §3 schema, trimmed to v1 needs).
pub struct CatalogEntry {
    pub id: &'static str,
    pub kind: Kind,
    pub status: Status,
    pub discovery: Option<Discovery>,
    pub license: &'static str,
    /// May gogo redistribute this inside produced artifacts? (GPTK: never.)
    pub redistributable: bool,
    pub min_macos: Option<&'static str>,
    pub arch: &'static str,
    pub note: &'static str,
    /// Engines that need a dylib set beside them (`Frameworks/`).
    pub frameworks: Option<FrameworksSet>,
}

/// The default runtime track (docs/19 §6.1, #62): the CrossOver-source
/// engine. `wine-staging` stays selectable as the alternate.
pub const DEFAULT_RUNTIME: &str = "winecx";

/// Licence notes for the Sikarugir Template `Frameworks/` dylibs (Template
/// 1.0.24 ships none of its own). Matched by file-name prefix, longest first.
const CX_FRAMEWORKS_NOTICES: &[(&str, &str)] = &[
    (
        "libMoltenVK",
        "MoltenVK — Apache-2.0 (KhronosGroup/MoltenVK)",
    ),
    ("libMVK", "MoltenVK — Apache-2.0 (KhronosGroup/MoltenVK)"),
    (
        "libvulkan_kosmickrisp",
        "KosmicKrisp Vulkan driver (Mesa) — MIT",
    ),
    (
        "libvulkan",
        "Vulkan loader — Apache-2.0 (KhronosGroup/Vulkan-Loader)",
    ),
    ("libSDL", "SDL — zlib (libsdl.org)"),
    ("libbrotli", "Brotli — MIT (google/brotli)"),
    ("libbz2", "bzip2 — bzip2 licence (sourceware.org/bzip2)"),
    (
        "libcharset",
        "libcharset (GNU libiconv) — LGPL-2.1-or-later",
    ),
    ("libiconv", "GNU libiconv — LGPL-2.1-or-later"),
    ("libexslt", "libxslt/libexslt — MIT (gnome.org)"),
    ("libxslt", "libxslt — MIT (gnome.org)"),
    ("libxml2", "libxml2 — MIT (gnome.org)"),
    ("libffi", "libffi — MIT"),
    (
        "libfreetype",
        "FreeType — FreeType License (FTL) or GPL-2.0, at the user's choice",
    ),
    ("libgmpxx", "GNU MP — LGPL-3.0-or-later or GPL-2.0-or-later"),
    ("libgmp", "GNU MP — LGPL-3.0-or-later or GPL-2.0-or-later"),
    (
        "libgnutls-openssl",
        "GnuTLS OpenSSL compatibility layer — GPL-3.0-or-later",
    ),
    ("libgnutls", "GnuTLS — LGPL-2.1-or-later"),
    (
        "libhogweed",
        "Nettle (hogweed) — LGPL-3.0-or-later or GPL-2.0-or-later",
    ),
    (
        "libnettle",
        "Nettle — LGPL-3.0-or-later or GPL-2.0-or-later",
    ),
    ("libicu", "ICU — Unicode License v3 (unicode-org/icu)"),
    ("libidn2", "libidn2 — LGPL-3.0-or-later or GPL-2.0-or-later"),
    ("libinotify", "libinotify-kqueue — MIT"),
    ("libintl", "GNU gettext libintl — LGPL-2.1-or-later"),
    ("liblz4", "LZ4 library — BSD-2-Clause"),
    ("liblzma", "XZ Utils liblzma — 0BSD"),
    ("libp11-kit", "p11-kit — BSD-3-Clause"),
    ("p11-kit", "p11-kit — BSD-3-Clause"),
    ("libpcap", "libpcap — BSD-3-Clause (tcpdump.org)"),
    ("libpng", "libpng — libpng License"),
    ("libtasn1", "GNU libtasn1 — LGPL-2.1-or-later"),
    (
        "libunistring",
        "GNU libunistring — LGPL-3.0-or-later or GPL-2.0-or-later",
    ),
    ("libz.", "zlib — zlib License"),
    ("libzstd", "Zstandard — BSD-3-Clause or GPL-2.0"),
];

static ENTRIES: &[CatalogEntry] = &[
    CatalogEntry {
        id: "winecx",
        kind: Kind::Wine,
        status: Status::Available,
        discovery: Some(Discovery::Pinned(PinnedAsset {
            repo: "Sikarugir-App/Engines",
            tag: "v1.0",
            asset: "WS12WineCX24.0.7_7.tar.xz",
            version: "24.0.7_7",
            sha256: "203f9e9fd6c2cc77e6525d798a434ced326145db34a356355e05659d3445fd1c",
        })),
        license: "LGPL-2.1 (Wine; CrossOver 24.0.7 sources by CodeWeavers, built by Gcenx/Sikarugir)",
        redistributable: true,
        min_macos: None,
        arch: "x86_64 (Rosetta 2 on Apple Silicon)",
        note: "default track (docs/19 §6.1): CrossOver-24-source engine (Wine 9.0). Runs \
               32-bit Inno code without #56's overflow, usable OpenGL, FreeType from its \
               own Frameworks/ (#54)",
        frameworks: Some(FrameworksSet {
            asset: PinnedAsset {
                repo: "Sikarugir-App/Template",
                tag: "v1.0",
                asset: "Template-1.0.24.tar.xz",
                version: "1.0.24",
                sha256: "4b5f89eeb9ef139936bdfad33d9b420b05ea4597cb7d63376674f7a8e171321c",
            },
            dir: "Template-1.0.24.app/Contents/Frameworks",
            notices: CX_FRAMEWORKS_NOTICES,
        }),
    },
    CatalogEntry {
        id: "wine-staging",
        kind: Kind::Wine,
        status: Status::Available,
        discovery: Some(Discovery::GitHubReleases {
            repo: "Gcenx/macOS_Wine_builds",
            asset_prefix: "wine-staging-",
            asset_suffix: "-osx64.tar.xz",
        }),
        license: "LGPL-2.1 (Wine)",
        redistributable: true,
        min_macos: None,
        arch: "x86_64 (Rosetta 2 on Apple Silicon)",
        note: "alternate track (the default is winecx) — gaming patches; gecko+mono bundled. Live                2026-10-19: 32-bit GOG installers (InnoSetup PE32) crash under                Rosetta new-WOW64 (virtual_setup_exception stack overflow); use                the GPTK lane for those",
        frameworks: None,
    },
    CatalogEntry {
        id: "wine-devel",
        kind: Kind::Wine,
        status: Status::Available,
        discovery: Some(Discovery::GitHubReleases {
            repo: "Gcenx/macOS_Wine_builds",
            asset_prefix: "wine-devel-",
            asset_suffix: "-osx64.tar.xz",
        }),
        license: "LGPL-2.1 (Wine)",
        redistributable: true,
        min_macos: None,
        arch: "x86_64 (Rosetta 2 on Apple Silicon)",
        note: "WineHQ devel train. Live 2026-10-19: 32-bit GOG installers crash                under Rosetta new-WOW64; use the GPTK lane for those",
        frameworks: None,
    },
    CatalogEntry {
        id: "wine-stable",
        kind: Kind::Wine,
        status: Status::Available,
        discovery: Some(Discovery::GitHubReleases {
            repo: "Gcenx/macOS_Wine_builds",
            asset_prefix: "wine-stable-",
            asset_suffix: "-osx64.tar.xz",
        }),
        license: "LGPL-2.1 (Wine)",
        redistributable: true,
        min_macos: None,
        arch: "x86_64 (Rosetta 2 on Apple Silicon)",
        note: "WineHQ stable train; tags may carry _N suffixes (docs/15 §8.2)",
        frameworks: None,
    },
    CatalogEntry {
        id: "gptk",
        kind: Kind::Wine,
        status: Status::ConsentRequired,
        discovery: Some(Discovery::GitHubReleases {
            repo: "Gcenx/game-porting-toolkit",
            asset_prefix: "game-porting-toolkit-",
            asset_suffix: ".tar.xz",
        }),
        license: "Apple evaluation EULA",
        redistributable: false,
        min_macos: Some("14.0"),
        arch: "aarch64",
        note: "D3DMetal DX11/12→Metal; consent-download (D9); never redistributed",
        frameworks: None,
    },
    CatalogEntry {
        id: "wine-crossover",
        kind: Kind::Wine,
        status: Status::Gone {
            since: "2026-10-19",
            story: "repo + releases 404; WoW64 matured upstream, the cx-llvm toolchain \
                    bit-rotted, and the maintainer pivoted to Gcenx/macOS_Wine_builds \
                    (docs/15 §1)",
        },
        discovery: None,
        license: "LGPL-2.1 (Wine)",
        redistributable: true,
        min_macos: None,
        arch: "x86_64",
        note: "tombstone — cached copies stay usable via `mac-app --runtime path`",
        frameworks: None,
    },
    CatalogEntry {
        id: "winetricks",
        kind: Kind::Tool {
            payload: "winetricks",
        },
        status: Status::Tool,
        discovery: Some(Discovery::GitHubReleases {
            repo: "Winetricks/winetricks",
            asset_prefix: "",
            asset_suffix: ".tar.gz",
        }),
        license: "LGPL-2.1+",
        redistributable: true,
        min_macos: None,
        arch: "any",
        note: "GitHub release tarball (digest-pinned); the script is located post-extract",
        frameworks: None,
    },
    CatalogEntry {
        id: "cnc-ddraw",
        kind: Kind::Renderer {
            payload: "ddraw.dll",
            overlay: false,
        },
        status: Status::Available,
        // The release publishes no asset digest: the pin is the sha256 of
        // the live-proven v7.1.0.0 download (2026-10-19, re-checked 2026-10-23).
        discovery: Some(Discovery::Pinned(PinnedAsset {
            repo: "FunkyFr3sh/cnc-ddraw",
            tag: "v7.1.0.0",
            asset: "cnc-ddraw.zip",
            version: "7.1.0.0",
            sha256: "0b13ab89a64c9918189b1dadd449ef6ed3cb3b7b19cabd96d8adbd95505bb908",
        })),
        license: "MIT (Copyright (c) FunkyFr3sh and the cnc-ddraw contributors)",
        redistributable: true,
        min_macos: None,
        arch: "x86_64",
        note: "renderer: DirectDraw wrapper (design 13) for 2D DirectDraw games, dropped \
               into the game dir with ddraw=n,b; never over a game's or recipe's own \
               ddraw.dll",
        frameworks: None,
    },
    CatalogEntry {
        id: "dxmt",
        kind: Kind::Renderer {
            payload: "winemetal.so",
            overlay: true,
        },
        status: Status::Available,
        // v0.80 ships only the builtin flavour. DXMT accepts no AI-made
        // contributions: a downloaded dependency only (docs/19 §3.2).
        discovery: Some(Discovery::Pinned(PinnedAsset {
            repo: "3Shain/dxmt",
            tag: "v0.80",
            asset: "dxmt-v0.80-builtin.tar.gz",
            version: "v0.80",
            sha256: "8f260e36b5739e68f3bad613381441385c4dc7b85b78ba8de653d5a6a264529d",
        })),
        license: "LGPL-2.1 (DXMT, https://github.com/3Shain/dxmt)",
        redistributable: true,
        min_macos: None,
        arch: "x86_64",
        note: "renderer: D3D10/11 to Metal (docs/19 §6.2). Replaces the engine's \
               d3d11/dxgi/d3d10core/winemetal, so it runs from a derived runtime \
               (design 16 decision 3); needs a CrossOver-source engine (winecx) and a \
               Metal 3 GPU",
        frameworks: None,
    },
];

/// The static catalog (design 12 D1: data, probed at use time).
pub fn entries() -> &'static [CatalogEntry] {
    ENTRIES
}

pub fn find(id: &str) -> Option<&'static CatalogEntry> {
    ENTRIES.iter().find(|e| e.id == id)
}

/// A recipe-driven runtime recommendation (design 12 §7).
#[derive(Debug, Clone, PartialEq)]
pub struct WineRecommendation {
    pub id: &'static str,
    /// Advisory only (D6): rendered as a warning event, never a blocker.
    pub notice: Option<&'static str>,
}

/// The §7 table as data: recipe `wine.version` prefixes → macOS action.
/// Order matters; the first matching prefix wins.
const WINE_PREFIX_MAP: &[(&[&str], &str, Option<&str>)] = &[
    // Linux runner builds — everything Lutris pins on Linux falls here.
    (
        &["lutris-", "ge-proton", "ge-"],
        DEFAULT_RUNTIME,
        Some("recipe targets a Linux wine build; using the default macOS runtime"),
    ),
    // Crossover line: the old wine-crossover repo is gone (docs/15 §1), and
    // the CX-source engine is the default track now (docs/19 §6.1).
    (&["wine-crossover", "cx"], DEFAULT_RUNTIME, None),
];

/// Map a recipe's `wine.version` pin to a catalog id (design 12 §7, D6).
/// Absent / `system` → the default track, no notice; exact mac-servable ids
/// → that id; crossover pins → the default (CX) track, no notice; Linux
/// runner pins and anything else → the default track + notice. `gptk` is deliberately excluded: consent downloads are
/// opt-in (D9), never a recipe side effect.
pub fn recommend_for_wine(v: Option<&str>) -> WineRecommendation {
    let linux_fallback = WineRecommendation {
        id: DEFAULT_RUNTIME,
        notice: Some("recipe targets a Linux runner; using the default macOS runtime"),
    };
    let default = WineRecommendation {
        id: DEFAULT_RUNTIME,
        notice: None,
    };
    let Some(v) = v.map(str::trim).filter(|v| !v.is_empty()) else {
        return default;
    };
    if v.eq_ignore_ascii_case("system") {
        return default;
    }
    // Exact catalog id — only freely-downloadable wine entries.
    if let Some(e) = entries().iter().find(|e| {
        e.id == v && matches!(e.kind, Kind::Wine) && matches!(e.status, Status::Available)
    }) {
        return WineRecommendation {
            id: e.id,
            notice: None,
        };
    }
    let lower = v.to_ascii_lowercase();
    for (prefixes, id, notice) in WINE_PREFIX_MAP {
        if prefixes.iter().any(|p| lower.starts_with(p)) {
            return WineRecommendation {
                id,
                notice: *notice,
            };
        }
    }
    linux_fallback
}

/// A probed, downloadable release artifact.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteRelease {
    pub tag: String,
    pub asset_name: String,
    pub asset_url: String,
    pub size: u64,
    /// GitHub asset digest, hex (no `sha256:` prefix); absent on old releases.
    pub sha256: Option<String>,
}

#[derive(Debug, Clone)]
struct ReleaseDoc {
    tag: String,
    prerelease: bool,
    published_at: String,
    assets: Vec<AssetDoc>,
}

#[derive(Debug, Clone)]
struct AssetDoc {
    name: String,
    size: u64,
    url: String,
    digest: Option<String>,
}

/// Shared HTTP client for catalog probes (GitHub rejects UA-less requests).
pub fn http_client() -> Result<reqwest::Client, MacAppError> {
    reqwest::Client::builder()
        .user_agent(UA)
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| MacAppError::CatalogUnreachable {
            detail: format!("http client: {e}"),
        })
}

/// Probe the live artifact for `entry`. 07 §4 retry semantics: 3 attempts with
/// backoff on 5xx/network errors. `version` pins a release tag; `None` takes
/// the newest release **carrying a matching asset** (non-prerelease
/// preferred) — flavors lag each other (stable last shipped `11.0_1` while
/// latest is `11.18`, docs/15 §8.2), so `/releases/latest` is only a fast
/// path when its assets match; otherwise the release list decides. Asset
/// digests are trusted when present (docs/15 §2).
pub async fn probe(
    entry: &CatalogEntry,
    version: Option<&str>,
    http: &reqwest::Client,
    api_base: &str,
) -> Result<RemoteRelease, MacAppError> {
    let disc = entry
        .discovery
        .as_ref()
        .ok_or_else(|| MacAppError::CatalogUnreachable {
            detail: format!("{} has no downloadable artifact ({})", entry.id, entry.note),
        })?;
    let (repo, asset_prefix, asset_suffix) = match disc {
        Discovery::GitHubReleases {
            repo,
            asset_prefix,
            asset_suffix,
        } => (repo, asset_prefix, asset_suffix),
        Discovery::Pinned(pin) => {
            if let Some(v) = version.filter(|v| *v != pin.version) {
                return Err(MacAppError::RuntimeDownloadFailed {
                    url: format!("{api_base}/repos/{}/releases/tags/{}", pin.repo, pin.tag),
                    reason: format!(
                        "{} is pinned to {} (no other version is catalogued, not '{v}')",
                        entry.id, pin.version
                    ),
                });
            }
            return probe_pinned(pin, http, api_base).await;
        }
    };
    let matches_asset =
        |a: &AssetDoc| a.name.starts_with(asset_prefix) && a.name.ends_with(asset_suffix);

    // Fast path (unpinned only): /releases/latest when it carries the asset.
    if version.is_none() {
        let latest_url = format!("{api_base}/repos/{repo}/releases/latest");
        if let Some(v) = get_json_opt(http, &latest_url).await?
            && let Some(doc) = parse_release(&v)
            && let Some(asset) = doc.assets.iter().find(|a| matches_asset(a))
        {
            return Ok(remote_release(&doc, asset));
        }
    }

    // General: the release list — pinned tag, or newest release with the asset.
    let list_url = format!("{api_base}/repos/{repo}/releases?per_page=30");
    let v =
        get_json_opt(http, &list_url)
            .await?
            .ok_or_else(|| MacAppError::CatalogUnreachable {
                detail: format!("{list_url}: not found"),
            })?;
    let docs: Vec<ReleaseDoc> = v
        .as_array()
        .map(|a| a.iter().filter_map(parse_release).collect())
        .ok_or_else(|| MacAppError::CatalogUnreachable {
            detail: format!("{list_url}: expected an array"),
        })?;
    let chosen = match version {
        Some(v) => docs.into_iter().find(|r| r.tag == v).ok_or_else(|| {
            MacAppError::RuntimeDownloadFailed {
                url: list_url.clone(),
                reason: format!("no release tagged '{v}'"),
            }
        })?,
        None => pick_latest(
            docs.into_iter()
                .filter(|r| r.assets.iter().any(&matches_asset))
                .collect(),
        )
        .ok_or_else(|| MacAppError::CatalogUnreachable {
            detail: format!("{repo}: no release carries {asset_prefix}…{asset_suffix}"),
        })?,
    };
    let asset = chosen
        .assets
        .iter()
        .find(|a| matches_asset(a))
        .ok_or_else(|| MacAppError::RuntimeDownloadFailed {
            url: format!("{api_base}/repos/{repo}/releases/tag/{}", chosen.tag),
            reason: format!("no asset matching {asset_prefix}…{asset_suffix}"),
        })?;
    Ok(remote_release(&chosen, asset))
}

/// Probe one pinned asset: it must exist in its release, and GitHub's digest
/// (when it publishes one) must match the pin. The result carries the pinned
/// version and sha256, which the download is then verified against.
pub async fn probe_pinned(
    pin: &PinnedAsset,
    http: &reqwest::Client,
    api_base: &str,
) -> Result<RemoteRelease, MacAppError> {
    let url = format!("{api_base}/repos/{}/releases/tags/{}", pin.repo, pin.tag);
    let doc = get_json_opt(http, &url)
        .await?
        .as_ref()
        .and_then(parse_release)
        .ok_or_else(|| MacAppError::CatalogUnreachable {
            detail: format!("{url}: not found"),
        })?;
    let asset = doc
        .assets
        .iter()
        .find(|a| a.name == pin.asset)
        .ok_or_else(|| MacAppError::RuntimeDownloadFailed {
            url: url.clone(),
            reason: format!("release {} has no asset {}", pin.tag, pin.asset),
        })?;
    let mut rel = remote_release(&doc, asset);
    if let Some(upstream) = &rel.sha256
        && !upstream.eq_ignore_ascii_case(pin.sha256)
    {
        return Err(MacAppError::CatalogUnreachable {
            detail: format!(
                "{}: upstream digest {upstream} differs from the pinned {} — \
                 the asset was replaced; refusing it",
                pin.asset, pin.sha256
            ),
        });
    }
    rel.tag = pin.version.to_string();
    rel.sha256 = Some(pin.sha256.to_string());
    Ok(rel)
}

fn remote_release(doc: &ReleaseDoc, asset: &AssetDoc) -> RemoteRelease {
    RemoteRelease {
        tag: doc.tag.clone(),
        asset_name: asset.name.clone(),
        asset_url: asset.url.clone(),
        size: asset.size,
        sha256: asset
            .digest
            .as_deref()
            .and_then(|d| d.strip_prefix("sha256:"))
            .map(str::to_string),
    }
}

fn pick_latest(releases: Vec<ReleaseDoc>) -> Option<ReleaseDoc> {
    let newest = |rs: Vec<ReleaseDoc>| {
        rs.into_iter()
            .max_by(|a, b| a.published_at.cmp(&b.published_at))
    };
    let stable: Vec<_> = releases.iter().filter(|r| !r.prerelease).cloned().collect();
    newest(stable).or_else(|| newest(releases))
}

/// GET JSON with 07 §4 retries; 404 maps to `Ok(None)` (definitive, not
/// retryable), other non-success statuses are errors.
async fn get_json_opt(http: &reqwest::Client, url: &str) -> Result<Option<Value>, MacAppError> {
    let mut last = String::new();
    for attempt in 0..3u32 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(250 * u64::from(attempt))).await;
        }
        match http
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
        {
            Ok(resp) if resp.status().as_u16() == 404 => return Ok(None),
            Ok(resp) if resp.status().is_server_error() => {
                last = format!("{} on {url}", resp.status());
            }
            Ok(resp) if resp.status().is_success() => {
                return Some(resp.json::<Value>().await.map_err(|e| {
                    MacAppError::CatalogUnreachable {
                        detail: format!("{url}: {e}"),
                    }
                }))
                .transpose();
            }
            Ok(resp) => {
                return Err(MacAppError::CatalogUnreachable {
                    detail: format!("{} on {url}", resp.status()),
                });
            }
            Err(e) => last = format!("{e} on {url}"),
        }
    }
    Err(MacAppError::CatalogUnreachable {
        detail: format!("unreachable after 3 attempts: {last}"),
    })
}

fn parse_release(v: &Value) -> Option<ReleaseDoc> {
    Some(ReleaseDoc {
        tag: v.get("tag_name")?.as_str()?.to_string(),
        prerelease: v
            .get("prerelease")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        published_at: v
            .get("published_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        assets: v
            .get("assets")?
            .as_array()?
            .iter()
            .filter_map(|a| {
                Some(AssetDoc {
                    name: a.get("name")?.as_str()?.to_string(),
                    size: a.get("size").and_then(Value::as_u64).unwrap_or(0),
                    url: a.get("browser_download_url")?.as_str()?.to_string(),
                    digest: a.get("digest").and_then(Value::as_str).map(str::to_string),
                })
            })
            .collect(),
    })
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    async fn mock_latest(server: &wiremock::MockServer, body: serde_json::Value) {
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/repos/Gcenx/macOS_Wine_builds/releases/latest",
            ))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn latest_release_probes_with_digest() {
        let server = wiremock::MockServer::start().await;
        mock_latest(
            &server,
            serde_json::json!({
                "tag_name": "11.18",
                "prerelease": false,
                "published_at": "2026-09-25T15:27:03Z",
                "assets": [{
                    "name": "wine-devel-11.18-osx64.tar.xz",
                    "size": 10,
                    "browser_download_url": "http://x/d.tar.xz",
                }, {
                    "name": "wine-staging-11.18-osx64.tar.xz",
                    "size": 190974384,
                    "browser_download_url": format!("{}/download/wine-staging-11.18-osx64.tar.xz", server.uri()),
                    "digest": "sha256:aa0ea4c82e636ae7bca2076387cb0a5affa26509ad13f119ecd0d62bd7ba6f82",
                }],
            }),
        )
        .await;
        let entry = find("wine-staging").unwrap();
        let http = http_client().unwrap();
        let rel = probe(entry, None, &http, &server.uri()).await.unwrap();
        assert_eq!(rel.tag, "11.18");
        assert_eq!(rel.asset_name, "wine-staging-11.18-osx64.tar.xz");
        assert_eq!(rel.size, 190974384);
        assert_eq!(
            rel.sha256.as_deref(),
            Some("aa0ea4c82e636ae7bca2076387cb0a5affa26509ad13f119ecd0d62bd7ba6f82")
        );
    }

    #[tokio::test]
    async fn latest_404_falls_back_to_release_list() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/repos/Gcenx/macOS_Wine_builds/releases/latest",
            ))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/repos/Gcenx/macOS_Wine_builds/releases",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!([
                    {
                        "tag_name": "11.19-pre", "prerelease": true,
                        "published_at": "2026-10-01T00:00:00Z",
                        "assets": [{"name": "wine-staging-11.19-pre-osx64.tar.xz", "size": 1,
                                    "browser_download_url": "http://x/1"}]
                    },
                    {
                        "tag_name": "11.17", "prerelease": false,
                        "published_at": "2026-09-01T00:00:00Z",
                        "assets": [{"name": "wine-staging-11.17-osx64.tar.xz", "size": 2,
                                    "browser_download_url": "http://x/2"}]
                    }
                ])),
            )
            .mount(&server)
            .await;
        let entry = find("wine-staging").unwrap();
        let http = http_client().unwrap();
        let rel = probe(entry, None, &http, &server.uri()).await.unwrap();
        // newest non-prerelease wins over the newer prerelease
        assert_eq!(rel.tag, "11.17");
    }

    #[tokio::test]
    async fn pinned_version_finds_tag_with_suffix() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/repos/Gcenx/macOS_Wine_builds/releases/latest",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "tag_name": "11.18", "prerelease": false,
                    "published_at": "2026-09-25T15:27:03Z",
                    "assets": [{"name": "wine-staging-11.18-osx64.tar.xz", "size": 1,
                                "browser_download_url": "http://x/l"}]
                })),
            )
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/repos/Gcenx/macOS_Wine_builds/releases",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!([
                    {
                        "tag_name": "11.0_1", "prerelease": false,
                        "published_at": "2026-04-16T14:05:00Z",
                        "assets": [{"name": "wine-staging-11.0_1-osx64.tar.xz", "size": 3,
                                    "browser_download_url": "http://x/s"}]
                    }
                ])),
            )
            .mount(&server)
            .await;
        let entry = find("wine-staging").unwrap();
        let http = http_client().unwrap();
        let rel = probe(entry, Some("11.0_1"), &http, &server.uri())
            .await
            .unwrap();
        assert_eq!(rel.tag, "11.0_1");
        assert_eq!(rel.asset_name, "wine-staging-11.0_1-osx64.tar.xz");
        let err = probe(entry, Some("9.0"), &http, &server.uri())
            .await
            .unwrap_err();
        assert!(matches!(err, MacAppError::RuntimeDownloadFailed { .. }));
    }

    #[tokio::test]
    async fn unpinned_flavor_lagging_latest_resolves_from_list() {
        // Live finding (2026-10-19): the 11.18 "latest" release carries only
        // staging+devel assets; wine-stable last shipped on 11.0_1 — the
        // probe must fall back to the release list for the flavor.
        let server = wiremock::MockServer::start().await;
        mock_latest(
            &server,
            serde_json::json!({
                "tag_name": "11.18", "prerelease": false,
                "published_at": "2026-09-25T15:27:03Z",
                "assets": [{"name": "wine-staging-11.18-osx64.tar.xz", "size": 1,
                            "browser_download_url": "http://x/st"}]
            }),
        )
        .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/repos/Gcenx/macOS_Wine_builds/releases",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!([
                    {
                        "tag_name": "11.18", "prerelease": false,
                        "published_at": "2026-09-25T15:27:03Z",
                        "assets": [{"name": "wine-staging-11.18-osx64.tar.xz", "size": 1,
                                    "browser_download_url": "http://x/st"}]
                    },
                    {
                        "tag_name": "11.0_1", "prerelease": false,
                        "published_at": "2026-04-16T14:05:00Z",
                        "assets": [{"name": "wine-stable-11.0_1-osx64.tar.xz", "size": 3,
                                    "browser_download_url": "http://x/sb"}]
                    }
                ])),
            )
            .mount(&server)
            .await;
        let entry = find("wine-stable").unwrap();
        let http = http_client().unwrap();
        let rel = probe(entry, None, &http, &server.uri()).await.unwrap();
        assert_eq!(rel.tag, "11.0_1");
        assert_eq!(rel.asset_name, "wine-stable-11.0_1-osx64.tar.xz");
    }

    #[tokio::test]
    async fn no_matching_asset_anywhere_is_named_error() {
        let server = wiremock::MockServer::start().await;
        mock_latest(
            &server,
            serde_json::json!({
                "tag_name": "11.18", "prerelease": false,
                "published_at": "2026-09-25T15:27:03Z",
                "assets": [{"name": "some-unrelated.dmg", "size": 1,
                            "browser_download_url": "http://x/d"}]
            }),
        )
        .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/repos/Gcenx/macOS_Wine_builds/releases",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!([
                    {
                        "tag_name": "11.18", "prerelease": false,
                        "published_at": "2026-09-25T15:27:03Z",
                        "assets": [{"name": "some-unrelated.dmg", "size": 1,
                                    "browser_download_url": "http://x/d"}]
                    }
                ])),
            )
            .mount(&server)
            .await;
        let entry = find("wine-staging").unwrap();
        let http = http_client().unwrap();
        let err = probe(entry, None, &http, &server.uri()).await.unwrap_err();
        assert!(matches!(err, MacAppError::CatalogUnreachable { .. }));
        assert!(err.to_string().contains("no release carries"));
    }

    async fn mock_engines(server: &wiremock::MockServer, digest: Option<&str>) {
        let mut asset = serde_json::json!({
            "name": "WS12WineCX24.0.7_7.tar.xz",
            "size": 172348356,
            "browser_download_url": "http://x/WS12WineCX24.0.7_7.tar.xz",
        });
        if let Some(d) = digest {
            asset["digest"] = d.into();
        }
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/repos/Sikarugir-App/Engines/releases/tags/v1.0",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "tag_name": "v1.0", "prerelease": false,
                    "published_at": "2024-10-03T20:04:52Z",
                    "assets": [
                        {"name": "WS12WineCX24.0.7_6.tar.xz", "size": 1,
                         "browser_download_url": "http://x/6"},
                        asset,
                    ]
                })),
            )
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn pinned_engine_probes_exact_asset_and_version() {
        let server = wiremock::MockServer::start().await;
        let Some(Discovery::Pinned(pin)) = &find("winecx").unwrap().discovery else {
            panic!("winecx is pinned");
        };
        mock_engines(&server, Some(&format!("sha256:{}", pin.sha256))).await;
        let entry = find("winecx").unwrap();
        let http = http_client().unwrap();
        let rel = probe(entry, None, &http, &server.uri()).await.unwrap();
        assert_eq!(rel.tag, "24.0.7_7");
        assert_eq!(rel.asset_name, "WS12WineCX24.0.7_7.tar.xz");
        assert_eq!(rel.sha256.as_deref(), Some(pin.sha256));
        // The pinned version is the only one.
        let rel = probe(entry, Some("24.0.7_7"), &http, &server.uri())
            .await
            .unwrap();
        assert_eq!(rel.tag, "24.0.7_7");
        let err = probe(entry, Some("24.0.7_6"), &http, &server.uri())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("pinned to 24.0.7_7"), "{err}");
    }

    #[tokio::test]
    async fn pinned_engine_without_upstream_digest_still_carries_the_pin() {
        let server = wiremock::MockServer::start().await;
        mock_engines(&server, None).await;
        let http = http_client().unwrap();
        let rel = probe(find("winecx").unwrap(), None, &http, &server.uri())
            .await
            .unwrap();
        assert!(rel.sha256.is_some_and(|d| d.starts_with("203f9e9f")));
    }

    #[tokio::test]
    async fn replaced_upstream_asset_is_refused() {
        let server = wiremock::MockServer::start().await;
        mock_engines(&server, Some("sha256:00ff")).await;
        let http = http_client().unwrap();
        let err = probe(find("winecx").unwrap(), None, &http, &server.uri())
            .await
            .unwrap_err();
        assert!(matches!(err, MacAppError::CatalogUnreachable { .. }));
        assert!(err.to_string().contains("differs from the pinned"), "{err}");
    }

    #[test]
    fn entries_and_statuses() {
        assert!(find("wine-staging").is_some());
        assert!(find("nope").is_none());
        assert_eq!(entries().len(), 9);
        // The default track is the pinned CX engine with its Frameworks set.
        let cx = find(DEFAULT_RUNTIME).unwrap();
        assert_eq!(cx.id, "winecx");
        assert!(matches!(cx.status, Status::Available));
        assert!(matches!(cx.discovery, Some(Discovery::Pinned(_))));
        let fw = cx.frameworks.as_ref().unwrap();
        assert_eq!(fw.dir, "Template-1.0.24.app/Contents/Frameworks");
        for lib in ["libfreetype", "libinotify", "libgnutls", "libMoltenVK"] {
            assert!(fw.notices.iter().any(|(p, _)| *p == lib), "{lib}");
        }
        assert!(
            entries()
                .iter()
                .filter(|e| e.id != "winecx")
                .all(|e| e.frameworks.is_none())
        );
        // Renderers (#70): pinned, game-dir vs engine overlay.
        assert!(matches!(
            find("cnc-ddraw").unwrap().kind,
            Kind::Renderer {
                payload: "ddraw.dll",
                overlay: false
            }
        ));
        let dxmt = find("dxmt").unwrap();
        assert!(matches!(
            dxmt.kind,
            Kind::Renderer {
                payload: "winemetal.so",
                overlay: true
            }
        ));
        assert!(dxmt.license.starts_with("LGPL-2.1"));
        for id in ["cnc-ddraw", "dxmt"] {
            assert!(
                matches!(find(id).unwrap().discovery, Some(Discovery::Pinned(_))),
                "{id}"
            );
        }
        assert!(matches!(
            find("wine-crossover").unwrap().status,
            Status::Gone { .. }
        ));
        assert!(!find("gptk").unwrap().redistributable);
        assert!(find("wine-staging").unwrap().redistributable);
    }

    #[test]
    fn wine_recommendation_table() {
        // Design 12 §7 — every row of the mapping, as data. The default
        // track is winecx (docs/19 §6.1).
        let default_plain = WineRecommendation {
            id: "winecx",
            notice: None,
        };
        assert_eq!(recommend_for_wine(None), default_plain);
        assert_eq!(recommend_for_wine(Some("")), default_plain);
        assert_eq!(recommend_for_wine(Some("  ")), default_plain);
        assert_eq!(recommend_for_wine(Some("system")), default_plain);
        // Exact mac-servable ids pass through untouched.
        assert_eq!(recommend_for_wine(Some("winecx")), default_plain);
        for id in ["wine-staging", "wine-stable"] {
            assert_eq!(
                recommend_for_wine(Some(id)),
                WineRecommendation { id, notice: None }
            );
        }
        // Linux runner pins → the default + notice.
        for v in ["lutris-7.2-2", "GE-Proton9-20", "ge-8-26"] {
            let r = recommend_for_wine(Some(v));
            assert_eq!(r.id, "winecx", "{v}");
            assert!(r.notice.is_some(), "{v}");
        }
        // Crossover pins → the CX track, no notice.
        assert_eq!(
            recommend_for_wine(Some("wine-crossover-22.1.1")),
            default_plain
        );
        // gptk is never a recipe side effect (D9 consent lane).
        let r = recommend_for_wine(Some("gptk"));
        assert_eq!(r.id, "winecx");
        assert!(r.notice.is_some());
        // Unknown ids → the Linux fallback (default + notice).
        let r = recommend_for_wine(Some("some-future-runner-1.0"));
        assert_eq!(r.id, "winecx");
        assert!(r.notice.is_some());
    }
}
