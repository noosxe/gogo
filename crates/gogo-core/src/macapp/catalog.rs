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

/// Entry kind: a Wine runtime or a tool provisioned alongside one.
pub enum Kind {
    Wine,
    /// A tool payload; `payload` names the file that proves the extract
    /// (design 13 D2: `winetricks` script vs `cnc-ddraw`'s `ddraw.dll`).
    Tool {
        payload: &'static str,
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
}

static ENTRIES: &[CatalogEntry] = &[
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
        note: "recommended default — gaming patches; gecko+mono bundled. Live                2026-10-19: 32-bit GOG installers (InnoSetup PE32) crash under                Rosetta new-WOW64 (virtual_setup_exception stack overflow); use                the GPTK lane for those",
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
    },
    CatalogEntry {
        id: "cnc-ddraw",
        kind: Kind::Tool {
            payload: "ddraw.dll",
        },
        status: Status::Tool,
        discovery: Some(Discovery::GitHubReleases {
            repo: "FunkyFr3sh/cnc-ddraw",
            asset_prefix: "cnc-ddraw",
            asset_suffix: ".zip",
        }),
        license: "MIT",
        redistributable: true,
        min_macos: None,
        arch: "any",
        note: "DirectDraw wrapper (design 13): game-dir drop-in cures black-screen \
               classics; MIT; applied after the recipe phase, never over a recipe's \
               own ddraw.dll",
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
        "wine-staging",
        Some("recipe targets a Linux wine build; using the closest macOS equivalent"),
    ),
    // Crossover line: upstream is gone (docs/15 §1 tombstone).
    (
        &["wine-crossover", "cx"],
        "wine-staging",
        Some(
            "recipe targets wine-crossover, which is gone upstream since 2026-10-19 \
             (docs/15 §1); using wine-staging",
        ),
    ),
];

/// Map a recipe's `wine.version` pin to a catalog id (design 12 §7, D6).
/// Absent / `system` → staging, no notice; exact mac-servable ids → that id;
/// Linux runner and crossover pins → staging + notice; anything else →
/// staging + notice. `gptk` is deliberately excluded: consent downloads are
/// opt-in (D9), never a recipe side effect.
pub fn recommend_for_wine(v: Option<&str>) -> WineRecommendation {
    let linux_fallback = WineRecommendation {
        id: "wine-staging",
        notice: Some(
            "recipe targets a Linux runner; using wine-staging (closest macOS equivalent)",
        ),
    };
    let Some(v) = v.map(str::trim).filter(|v| !v.is_empty()) else {
        return WineRecommendation {
            id: "wine-staging",
            notice: None,
        };
    };
    if v.eq_ignore_ascii_case("system") {
        return WineRecommendation {
            id: "wine-staging",
            notice: None,
        };
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
    let Discovery::GitHubReleases {
        repo,
        asset_prefix,
        asset_suffix,
    } = disc;
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

    #[test]
    fn entries_and_statuses() {
        assert!(find("wine-staging").is_some());
        assert!(find("nope").is_none());
        assert_eq!(entries().len(), 7);
        assert!(matches!(
            find("cnc-ddraw").unwrap().kind,
            Kind::Tool {
                payload: "ddraw.dll"
            }
        ));
        assert!(matches!(
            find("wine-crossover").unwrap().status,
            Status::Gone { .. }
        ));
        assert!(!find("gptk").unwrap().redistributable);
        assert!(find("wine-staging").unwrap().redistributable);
    }

    #[test]
    fn wine_recommendation_table() {
        // Design 12 §7 — every row of the mapping, as data.
        let staging_plain = WineRecommendation {
            id: "wine-staging",
            notice: None,
        };
        assert_eq!(recommend_for_wine(None), staging_plain);
        assert_eq!(recommend_for_wine(Some("")), staging_plain);
        assert_eq!(recommend_for_wine(Some("  ")), staging_plain);
        assert_eq!(recommend_for_wine(Some("system")), staging_plain);
        // Exact mac-servable ids pass through untouched.
        assert_eq!(recommend_for_wine(Some("wine-staging")), staging_plain);
        assert_eq!(
            recommend_for_wine(Some("wine-stable")),
            WineRecommendation {
                id: "wine-stable",
                notice: None
            }
        );
        // Linux runner pins → staging + notice.
        for v in ["lutris-7.2-2", "GE-Proton9-20", "ge-8-26"] {
            let r = recommend_for_wine(Some(v));
            assert_eq!(r.id, "wine-staging", "{v}");
            assert!(r.notice.is_some(), "{v}");
        }
        // Crossover pins → staging + tombstone notice.
        let r = recommend_for_wine(Some("wine-crossover-22.1.1"));
        assert_eq!(r.id, "wine-staging");
        assert!(r.notice.is_some_and(|n| n.contains("gone upstream")));
        // gptk is never a recipe side effect (D9 consent lane).
        let r = recommend_for_wine(Some("gptk"));
        assert_eq!(r.id, "wine-staging");
        assert!(r.notice.is_some());
        // Unknown ids → the Linux fallback (staging + notice).
        let r = recommend_for_wine(Some("some-future-runner-1.0"));
        assert_eq!(r.id, "wine-staging");
        assert!(r.notice.is_some());
    }
}
