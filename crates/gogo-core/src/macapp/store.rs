//! Runtime store (design 12 §4): cached runtime trees + manifest under
//! `<state_dir>/runtimes/` — same conventions as `state/cache.rs` (`GOGO_HOME`
//! flattens; reads never hard-fail). Installs are atomic by construction:
//! download → sha256 verify → extract to a temp dir → locate + probe the wine
//! tree → rename into place → manifest update. The archive is deleted after a
//! successful install; the manifest keeps the sha256 for re-download.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::engine::events::ProgressEvent;

use super::catalog::{self, CatalogEntry, Status};
use super::error::MacAppError;

/// Store root: `<state_dir>/runtimes/` (design 12 §4).
pub fn runtimes_dir() -> PathBuf {
    crate::state::state_dir().join("runtimes")
}

fn tmp_dir() -> PathBuf {
    runtimes_dir().join(".tmp")
}

fn manifest_path() -> PathBuf {
    runtimes_dir().join("manifest.json")
}

/// One cached runtime (manifest record).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManifestEntry {
    pub id: String,
    pub version: String,
    pub sha256: String,
    /// RFC 3339.
    pub fetched_at: String,
    pub upstream_url: String,
    /// Wine binary path relative to the tree root (audit §8.1: the archive
    /// root is an app wrapper, the tree is `Contents/Resources/wine`).
    pub wine_rel: String,
    pub bytes: u64,
}

/// Tree root of a cached runtime: `<runtimes>/<id>/<version>/`.
pub fn tree_root(m: &ManifestEntry) -> PathBuf {
    runtimes_dir().join(&m.id).join(&m.version)
}

/// Load the manifest; missing → empty; corrupt → warn + empty (06 §2: reads
/// never hard-fail).
pub fn load_manifest() -> Vec<ManifestEntry> {
    match std::fs::read_to_string(manifest_path()) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            tracing::warn!("runtimes manifest corrupt ({e}) — treating as empty");
            Vec::new()
        }),
        Err(_) => Vec::new(),
    }
}

pub fn save_manifest(entries: &[ManifestEntry]) -> Result<(), MacAppError> {
    let path = manifest_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let raw = serde_json::to_string_pretty(entries)
        .map_err(|e| MacAppError::Io(std::io::Error::other(e.to_string())))?;
    std::fs::write(&tmp, raw)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// The newest cached instance of `id` (optionally version-pinned), verified to
/// still exist on disk.
pub fn installed(id: &str, version: Option<&str>) -> Option<(ManifestEntry, PathBuf)> {
    let mut hits: Vec<ManifestEntry> = load_manifest()
        .into_iter()
        .filter(|m| m.id == id && version.is_none_or(|v| m.version == v))
        .collect();
    hits.sort_by(|a, b| b.fetched_at.cmp(&a.fetched_at));
    hits.into_iter()
        .find(|m| tree_root(m).join(&m.wine_rel).is_file())
        .map(|m| {
            let root = tree_root(&m);
            (m, root)
        })
}

/// Install (download → verify → extract → probe → cache; design 12 §4). An
/// existing cache entry satisfying the request is reused without downloading.
pub async fn install(
    entry: &CatalogEntry,
    version: Option<&str>,
    http: &reqwest::Client,
    api_base: &str,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<ManifestEntry, MacAppError> {
    match &entry.status {
        Status::Available => {}
        Status::Tool => {
            return Err(MacAppError::RuntimeDownloadFailed {
                url: entry.id.into(),
                reason: "tool provisioning lands with design 12 §8 (Phase 2)".into(),
            });
        }
        Status::Gone { since, story } => {
            return Err(MacAppError::RuntimeDownloadFailed {
                url: entry.id.into(),
                reason: format!("upstream gone since {since} — {story}"),
            });
        }
        Status::ConsentRequired => {
            return Err(MacAppError::RuntimeDownloadFailed {
                url: entry.id.into(),
                reason: "consent-download (Apple EULA, design 12 D9) lands with the \
                         interactive selection; meanwhile register a mount via `mac-app \
                         --runtime gptk --runtime-path <dir>`"
                    .into(),
            });
        }
    }

    let rel = catalog::probe(entry, version, http, api_base).await?;
    if let Some((m, _)) = installed(entry.id, Some(&rel.tag)) {
        emit(&ProgressEvent::Warn {
            message: format!("{} {} already cached — reusing", entry.id, rel.tag),
        });
        return Ok(m);
    }

    emit(&ProgressEvent::RuntimeDownloadStarted {
        id: entry.id.into(),
        version: rel.tag.clone(),
        bytes: rel.size,
    });

    std::fs::create_dir_all(tmp_dir())?;
    let part = tmp_dir().join(format!("{}-{}.tar.xz.part", entry.id, rel.tag));
    download_with_retry(http, &rel.asset_url, &part).await?;

    // Verify against the upstream digest when present; always record the hash.
    let actual = sha256_file(&part)?;
    if let Some(expected) = &rel.sha256
        && !expected.eq_ignore_ascii_case(&actual)
    {
        let _ = std::fs::remove_file(&part);
        return Err(MacAppError::RuntimeChecksumMismatch {
            id: entry.id.into(),
            expected: expected.clone(),
            actual,
        });
    }

    // Extract to a temp dir, then locate + probe the wine tree (blocking work).
    let extract_dir = tmp_dir().join(format!("{}-{}", entry.id, rel.tag));
    let _ = std::fs::remove_dir_all(&extract_dir);
    std::fs::create_dir_all(&extract_dir)?;
    let archive = part.clone();
    let extract_dir2 = extract_dir.clone();
    let bytes = tokio::task::spawn_blocking(move || -> Result<u64, MacAppError> {
        let file = std::fs::File::open(&archive)?;
        let mut tar = tar::Archive::new(xz2::read::XzDecoder::new(file));
        tar.set_preserve_permissions(true);
        tar.unpack(&extract_dir2)?;
        dir_size(&extract_dir2)
    })
    .await
    .map_err(|e| MacAppError::RuntimeDownloadFailed {
        url: rel.asset_url.clone(),
        reason: format!("extract task: {e}"),
    })??;

    // Locate the wine tree and prove it runs BEFORE committing anything.
    let (wine, _version) = locate_and_probe(&extract_dir)?;
    let wine_rel = wine
        .strip_prefix(&extract_dir)
        .expect("wine path is inside the extract dir")
        .to_string_lossy()
        .into_owned();

    // Commit: rename into place, drop the archive, update the manifest.
    let dest = runtimes_dir().join(entry.id).join(&rel.tag);
    std::fs::create_dir_all(dest.parent().expect("dest has a parent"))?;
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&extract_dir, &dest)?;
    let _ = std::fs::remove_file(&part);

    let m = ManifestEntry {
        id: entry.id.into(),
        version: rel.tag.clone(),
        sha256: actual,
        fetched_at: Utc::now().to_rfc3339(),
        upstream_url: rel.asset_url.clone(),
        wine_rel,
        bytes,
    };
    let mut manifest: Vec<ManifestEntry> = load_manifest()
        .into_iter()
        .filter(|e| !(e.id == m.id && e.version == m.version))
        .collect();
    manifest.push(m.clone());
    save_manifest(&manifest)?;

    emit(&ProgressEvent::RuntimeDownloadFinished {
        id: entry.id.into(),
        version: rel.tag.clone(),
    });
    Ok(m)
}

/// Remove every cached version of `ids`; returns the removed tree roots.
pub fn prune(ids: &[String]) -> Result<Vec<PathBuf>, MacAppError> {
    let keep: Vec<ManifestEntry> = load_manifest()
        .into_iter()
        .filter(|m| !ids.iter().any(|i| i == &m.id))
        .collect();
    let mut removed = Vec::new();
    for id in ids {
        let dir = runtimes_dir().join(id);
        if dir.is_dir() {
            std::fs::remove_dir_all(&dir)?;
            removed.push(dir);
        }
    }
    save_manifest(&keep)?;
    Ok(removed)
}

/// Find the wine tree inside an extracted archive: bare `<dir>/bin/wine{,64}`
/// or the audited app-wrapper layout `<dir>/*.app/Contents/Resources/wine/bin/
/// wine{,64}` (docs/15 §8.1); then prove it executes.
fn locate_and_probe(dir: &Path) -> Result<(PathBuf, String), MacAppError> {
    if let Some(wine) = wine_in(&dir.join("bin")) {
        let version = super::runtime::probe_tree(&wine)?;
        return Ok((wine, version));
    }
    for child in std::fs::read_dir(dir)? {
        let child = child?.path();
        if child.extension().and_then(|e| e.to_str()) == Some("app") {
            let root = child.join("Contents/Resources/wine");
            if let Some(wine) = wine_in(&root.join("bin")) {
                let version = super::runtime::probe_tree(&wine)?;
                return Ok((wine, version));
            }
        }
    }
    Err(MacAppError::RuntimeInvalid {
        path: dir.to_path_buf(),
        reason: "archive has no bin/wine{,64} (bare or *.app/Contents/Resources/wine)".into(),
    })
}

fn wine_in(bin: &Path) -> Option<PathBuf> {
    ["wine", "wine64"]
        .iter()
        .map(|n| bin.join(n))
        .find(|p| p.is_file())
}

async fn download_with_retry(
    http: &reqwest::Client,
    url: &str,
    dst: &Path,
) -> Result<(), MacAppError> {
    let mut last = String::new();
    for attempt in 0..3u32 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(500 * u64::from(attempt))).await;
        }
        match http.get(url).send().await {
            Ok(resp) if resp.status().is_server_error() => {
                last = format!("{} on {url}", resp.status());
            }
            Ok(resp) if !resp.status().is_success() => {
                return Err(MacAppError::RuntimeDownloadFailed {
                    url: url.into(),
                    reason: format!("HTTP {}", resp.status()),
                });
            }
            Ok(mut resp) => {
                let mut file = tokio::fs::File::create(dst).await.map_err(|e| {
                    MacAppError::RuntimeDownloadFailed {
                        url: url.into(),
                        reason: format!("create {}: {e}", dst.display()),
                    }
                })?;
                use tokio::io::AsyncWriteExt as _;
                while let Some(chunk) =
                    resp.chunk()
                        .await
                        .map_err(|e| MacAppError::RuntimeDownloadFailed {
                            url: url.into(),
                            reason: format!("download: {e}"),
                        })?
                {
                    file.write_all(&chunk).await.map_err(|e| {
                        MacAppError::RuntimeDownloadFailed {
                            url: url.into(),
                            reason: format!("write: {e}"),
                        }
                    })?;
                }
                file.flush()
                    .await
                    .map_err(|e| MacAppError::RuntimeDownloadFailed {
                        url: url.into(),
                        reason: format!("flush: {e}"),
                    })?;
                return Ok(());
            }
            Err(e) => last = format!("{e} on {url}"),
        }
    }
    Err(MacAppError::RuntimeDownloadFailed {
        url: url.into(),
        reason: format!("unreachable after 3 attempts: {last}"),
    })
}

fn sha256_file(path: &Path) -> Result<String, MacAppError> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn dir_size(p: &Path) -> Result<u64, MacAppError> {
    let mut total = 0u64;
    if p.is_file() {
        return Ok(std::fs::metadata(p)?.len());
    }
    for entry in std::fs::read_dir(p)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            total += dir_size(&entry.path())?;
        } else {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

#[cfg(test)]
mod store_tests {
    use super::*;

    /// Point GOGO_HOME at `dir` for the test's lifetime (env mutex held).
    fn env_home(dir: &Path) -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::test_util::env_lock();
        // SAFETY: env mutation under the shared env mutex.
        unsafe { std::env::set_var("GOGO_HOME", dir) };
        guard
    }

    fn sha256_hex(b: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(b);
        h.finalize().iter().map(|x| format!("{x:02x}")).collect()
    }

    /// A tar.xz with the audited wrapper layout: `Wine Staging.app/Contents/
    /// Resources/wine/bin/wine` (executable fake) + a decoy Info.plist.
    fn fake_wrapper_tar() -> Vec<u8> {
        use std::io::Write as _;
        let mut tar_buf = Vec::new();
        {
            let mut t = tar::Builder::new(&mut tar_buf);
            let script = b"#!/bin/sh\necho wine-11.18-fake\n";
            let mut h = tar::Header::new_gnu();
            h.set_size(script.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            t.append_data(
                &mut h,
                "Wine Staging.app/Contents/Resources/wine/bin/wine",
                &script[..],
            )
            .unwrap();
            let mut h2 = tar::Header::new_gnu();
            h2.set_size(5);
            h2.set_mode(0o644);
            h2.set_cksum();
            t.append_data(
                &mut h2,
                "Wine Staging.app/Contents/Info.plist",
                &b"plist"[..],
            )
            .unwrap();
            t.finish().unwrap();
        }
        let mut xz = xz2::write::XzEncoder::new(Vec::new(), 6);
        xz.write_all(&tar_buf).unwrap();
        xz.finish().unwrap()
    }

    /// wiremock standing in for GitHub: `/releases/latest` + the asset URL.
    /// The asset mock expects exactly one hit, so cache-reuse is provable.
    async fn mock_github(body: &[u8], digest: &str) -> wiremock::MockServer {
        let server = wiremock::MockServer::start().await;
        let asset_url = format!("{}/download/wine-staging-11.18-osx64.tar.xz", server.uri());
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/repos/Gcenx/macOS_Wine_builds/releases/latest",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "tag_name": "11.18",
                    "prerelease": false,
                    "published_at": "2026-09-25T15:27:03Z",
                    "assets": [{
                        "name": "wine-staging-11.18-osx64.tar.xz",
                        "size": body.len(),
                        "browser_download_url": asset_url,
                        "digest": digest,
                    }],
                })),
            )
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/download/wine-staging-11.18-osx64.tar.xz",
            ))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
            .expect(1)
            .mount(&server)
            .await;
        server
    }

    // The env guard must span install's awaits: GOGO_HOME is read throughout.
    // Safe: the test runtime is single-threaded and no other task touches env.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn install_round_trip_wrapper_layout() {
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        let body = fake_wrapper_tar();
        let server = mock_github(&body, &format!("sha256:{}", sha256_hex(&body))).await;
        let entry = catalog::find("wine-staging").unwrap();
        let http = catalog::http_client().unwrap();

        let m = install(entry, None, &http, &server.uri(), &mut |_| {})
            .await
            .unwrap();
        assert_eq!(m.version, "11.18");
        assert_eq!(
            m.wine_rel,
            "Wine Staging.app/Contents/Resources/wine/bin/wine"
        );
        let root = tree_root(&m);
        assert!(root.join(&m.wine_rel).is_file());
        assert_eq!(m.sha256, sha256_hex(&body));

        // Reuse: a second install must not hit the download URL again.
        let m2 = install(entry, None, &http, &server.uri(), &mut |_| {})
            .await
            .unwrap();
        assert_eq!(m2.version, "11.18");
        server.verify().await;

        assert!(installed("wine-staging", Some("11.18")).is_some());
        assert!(installed("wine-staging", Some("9.0")).is_none());

        // Prune removes the tree + the manifest entries.
        let removed = prune(&["wine-staging".to_string()]).unwrap();
        assert_eq!(removed, vec![runtimes_dir().join("wine-staging")]);
        assert!(installed("wine-staging", None).is_none());
    }

    // Same env-guard-spanning-await justification as above.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn checksum_mismatch_detected() {
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        let body = fake_wrapper_tar();
        let server = mock_github(&body, "sha256:deadbeef").await;
        let entry = catalog::find("wine-staging").unwrap();
        let http = catalog::http_client().unwrap();
        let err = install(entry, None, &http, &server.uri(), &mut |_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, MacAppError::RuntimeChecksumMismatch { .. }));
        assert!(load_manifest().is_empty());
    }

    #[tokio::test]
    // Same env-guard-spanning-await justification as above.
    #[allow(clippy::await_holding_lock)]
    async fn gptk_and_tools_are_gated() {
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        let server = wiremock::MockServer::start().await;
        let http = catalog::http_client().unwrap();
        let err = install(
            catalog::find("gptk").unwrap(),
            None,
            &http,
            &server.uri(),
            &mut |_| {},
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("consent"));
        let err = install(
            catalog::find("winetricks").unwrap(),
            None,
            &http,
            &server.uri(),
            &mut |_| {},
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("Phase 2"));
        let err = install(
            catalog::find("wine-crossover").unwrap(),
            None,
            &http,
            &server.uri(),
            &mut |_| {},
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("gone since 2026-10-19"));
    }

    #[test]
    fn manifest_round_trip() {
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        assert!(load_manifest().is_empty());
        let m = ManifestEntry {
            id: "wine-staging".into(),
            version: "11.18".into(),
            sha256: "abc".into(),
            fetched_at: "2026-10-19T00:00:00Z".into(),
            upstream_url: "http://x".into(),
            wine_rel: "bin/wine".into(),
            bytes: 1,
        };
        save_manifest(std::slice::from_ref(&m)).unwrap();
        assert_eq!(load_manifest(), vec![m]);
    }

    #[test]
    fn locate_bare_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let wine = bin.join("wine64");
        std::fs::write(&wine, "#!/bin/sh\necho wine-9.0-fake\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&wine, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let (found, version) = locate_and_probe(tmp.path()).unwrap();
        assert_eq!(found, wine);
        assert_eq!(version, "wine-9.0-fake");
    }
}
