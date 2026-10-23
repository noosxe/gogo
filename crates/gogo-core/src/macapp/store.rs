//! Runtime store (design 12 §4): cached runtime trees + manifest under
//! `<state_dir>/runtimes/` — same conventions as `state/cache.rs` (`GOGO_HOME`
//! flattens; reads never hard-fail). Installs are atomic by construction:
//! download → sha256 verify → extract to a temp dir → locate + probe the wine
//! tree → rename into place → manifest update. The archive is deleted after a
//! successful install; the manifest keeps the sha256 for re-download.
//!
//! Renderers (design 16 decision 6) use the same install path but live under
//! `<state_dir>/renderers/<id>/<ver>/<arch>/` with their own manifest.
//! [`derive`] builds the derived runtimes engine-overlay renderers run from
//! (decision 3).

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

/// Renderer store root: `<state_dir>/renderers/` (design 16 decision 6).
pub fn renderers_dir() -> PathBuf {
    crate::state::state_dir().join("renderers")
}

fn renderer_manifest_path() -> PathBuf {
    renderers_dir().join("manifest.json")
}

/// The `<arch>` level of `renderers/<id>/<ver>/<arch>/`: the runtime arch a
/// payload is for. Every runtime is x86_64 (Rosetta) today; arm64ec joins
/// it with an ARM64 runtime.
pub const RENDERER_ARCH: &str = "x86_64";

/// Is `id` a renderer catalog entry (stored under `renderers/`)?
pub fn is_renderer(id: &str) -> bool {
    catalog::find(id).is_some_and(|e| matches!(e.kind, catalog::Kind::Renderer { .. }))
}

fn manifest_path_for(id: &str) -> PathBuf {
    if is_renderer(id) {
        renderer_manifest_path()
    } else {
        manifest_path()
    }
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
    /// Payload path relative to the tree root: the wine binary (audit §8.1
    /// app-wrapper layout) or the tool script (winetricks).
    pub entry_rel: String,
    pub bytes: u64,
}

/// Tree root of a cached entry: `<runtimes>/<id>/<version>/`, or
/// `<renderers>/<id>/<version>/<arch>/` for a renderer.
pub fn tree_root(m: &ManifestEntry) -> PathBuf {
    if is_renderer(&m.id) {
        renderers_dir()
            .join(&m.id)
            .join(&m.version)
            .join(RENDERER_ARCH)
    } else {
        runtimes_dir().join(&m.id).join(&m.version)
    }
}

/// Is `version` a derived runtime (`<ver>+<renderer>-<rver>`, decision 3)?
pub fn is_derived(version: &str) -> bool {
    version.contains('+')
}

/// Load the runtime manifest; missing → empty; corrupt → warn + empty (06
/// §2: reads never hard-fail).
pub fn load_manifest() -> Vec<ManifestEntry> {
    load_manifest_at(&manifest_path())
}

/// Load the renderer manifest (same rules as [`load_manifest`]).
pub fn load_renderer_manifest() -> Vec<ManifestEntry> {
    load_manifest_at(&renderer_manifest_path())
}

fn load_manifest_at(path: &Path) -> Vec<ManifestEntry> {
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            tracing::warn!("runtimes manifest corrupt ({e}) — treating as empty");
            Vec::new()
        }),
        Err(_) => Vec::new(),
    }
}

pub fn save_manifest(entries: &[ManifestEntry]) -> Result<(), MacAppError> {
    save_manifest_at(&manifest_path(), entries)
}

/// Save the renderer manifest.
pub fn save_renderer_manifest(entries: &[ManifestEntry]) -> Result<(), MacAppError> {
    save_manifest_at(&renderer_manifest_path(), entries)
}

fn save_manifest_at(path: &Path, entries: &[ManifestEntry]) -> Result<(), MacAppError> {
    let path = path.to_path_buf();
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
/// still exist on disk. Unpinned lookups never pick a derived runtime: those
/// are reached only by their full `<ver>+<renderer>-<rver>` version.
pub fn installed(id: &str, version: Option<&str>) -> Option<(ManifestEntry, PathBuf)> {
    let mut hits: Vec<ManifestEntry> = load_manifest_at(&manifest_path_for(id))
        .into_iter()
        .filter(|m| {
            m.id == id
                && match version {
                    Some(v) => m.version == v,
                    None => !is_derived(&m.version),
                }
        })
        .collect();
    hits.sort_by(|a, b| b.fetched_at.cmp(&a.fetched_at));
    hits.into_iter()
        .find(|m| tree_root(m).join(&m.entry_rel).is_file())
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
    consent: bool,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<ManifestEntry, MacAppError> {
    match &entry.status {
        Status::Available | Status::Tool => {}
        Status::Gone { since, story } => {
            return Err(MacAppError::RuntimeDownloadFailed {
                url: entry.id.into(),
                reason: format!("upstream gone since {since} — {story}"),
            });
        }
        // D9: consent downloads only through the interactive EULA prompt
        // (mac-app runtime menu); `consent` records that it happened.
        Status::ConsentRequired if consent => {}
        Status::ConsentRequired if matches!(entry.kind, catalog::Kind::Renderer { .. }) => {
            return Err(MacAppError::RuntimeDownloadFailed {
                url: entry.id.into(),
                reason: format!(
                    "{} needs your consent first ({}): `gogo mac-app` asks on a terminal, \
                     or set {} = \"yes\" in {}",
                    entry.id,
                    entry.license,
                    super::d3dmetal::KEY,
                    super::d3dmetal::config_path().display()
                ),
            });
        }
        Status::ConsentRequired => {
            return Err(MacAppError::RuntimeDownloadFailed {
                url: entry.id.into(),
                reason: "consent-download (Apple EULA, design 12 D9) requires the \
                         interactive runtime selection; meanwhile register a mount via \
                         `mac-app --runtime gptk --runtime-path <dir>`"
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
    let part = tmp_dir().join(format!(
        "{}-{}.{}.part",
        entry.id,
        rel.tag,
        archive_ext(&rel.asset_name)
    ));
    let actual = download_verified(http, &rel, &part, entry.id).await?;
    // The engine's dylib closure (#62), pinned like the engine itself.
    let frameworks_part = match &entry.frameworks {
        Some(fw) => {
            let fw_rel = catalog::probe_pinned(&fw.asset, http, api_base).await?;
            let fw_part = tmp_dir().join(format!(
                "{}-{}-frameworks.{}.part",
                entry.id,
                rel.tag,
                archive_ext(&fw_rel.asset_name)
            ));
            if let Err(e) = download_verified(http, &fw_rel, &fw_part, entry.id).await {
                let _ = std::fs::remove_file(&part);
                return Err(e);
            }
            Some(fw_part)
        }
        None => None,
    };

    // Extract to a temp dir (decoder by archive extension), then locate +
    // probe the payload — blocking work. With a Frameworks set the layout is
    // design 16's `{engine,Frameworks}`: the engine tree is moved to
    // `engine/`, the dylibs to `Frameworks/`.
    let extract_dir = tmp_dir().join(format!("{}-{}", entry.id, rel.tag));
    let _ = std::fs::remove_dir_all(&extract_dir);
    std::fs::create_dir_all(&extract_dir)?;
    let archive = part.clone();
    let asset_name = rel.asset_name.clone();
    let extract_dir2 = extract_dir.clone();
    let fw_job = frameworks_part
        .clone()
        .zip(entry.frameworks.as_ref().map(|f| f.dir));
    let subdir = match entry.kind {
        catalog::Kind::Renderer { subdir, .. } => subdir,
        _ => None,
    };
    let bytes = tokio::task::spawn_blocking(move || -> Result<u64, MacAppError> {
        match fw_job {
            None if subdir.is_some() => {
                let dir = subdir.expect("checked");
                if unpack_subtree(&archive, dir, &extract_dir2)? == 0 {
                    return Err(MacAppError::RuntimeInvalid {
                        path: archive.clone(),
                        reason: format!("nothing under {dir}"),
                    });
                }
            }
            None => unpack(&archive, &asset_name, &extract_dir2)?,
            Some((fw_archive, fw_dir)) => {
                let raw = extract_dir2.join(".raw");
                unpack(&archive, &asset_name, &raw)?;
                let engine = engine_dir_in(&raw).ok_or_else(|| MacAppError::RuntimeInvalid {
                    path: raw.clone(),
                    reason: "engine archive has no <dir>/bin/wine".into(),
                })?;
                std::fs::rename(&engine, extract_dir2.join("engine"))?;
                std::fs::remove_dir_all(&raw)?;
                let n = unpack_dylibs(&fw_archive, fw_dir, &extract_dir2.join("Frameworks"))?;
                if n == 0 {
                    return Err(MacAppError::RuntimeInvalid {
                        path: fw_archive,
                        reason: format!("no *.dylib under {fw_dir}"),
                    });
                }
            }
        }
        dir_size(&extract_dir2)
    })
    .await
    .map_err(|e| MacAppError::RuntimeDownloadFailed {
        url: rel.asset_url.clone(),
        reason: format!("extract task: {e}"),
    })??;
    if let Some(fw) = &entry.frameworks {
        std::fs::write(
            extract_dir.join("NOTICES.txt"),
            notices_text(entry, &rel, fw, &extract_dir.join("Frameworks"))?,
        )?;
    }

    // Locate the payload and prove it BEFORE committing anything: wine trees
    // must run; tool scripts must exist and be made executable.
    let entry_rel = match entry.kind {
        catalog::Kind::Wine => {
            let (wine, _version) = locate_and_probe(&extract_dir)?;
            if entry.frameworks.is_some() {
                // `wine --version` loads none of the closure; wineserver links
                // libinotify, so this proves `Frameworks/` resolves.
                super::runtime::probe_tree(&wine.with_file_name("wineserver"))?;
            }
            wine.strip_prefix(&extract_dir)
                .expect("payload inside the extract dir")
                .to_string_lossy()
                .into_owned()
        }
        catalog::Kind::Tool { payload } | catalog::Kind::Renderer { payload, .. } => {
            let f = locate_named_file(&extract_dir, payload).ok_or_else(|| {
                MacAppError::RuntimeInvalid {
                    path: extract_dir.clone(),
                    reason: format!("archive contains no '{payload}'"),
                }
            })?;
            if payload == "winetricks" {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    let mut perm = std::fs::metadata(&f)?.permissions();
                    perm.set_mode(0o755);
                    std::fs::set_permissions(&f, perm)?;
                }
            }
            f.strip_prefix(&extract_dir)
                .expect("payload inside the extract dir")
                .to_string_lossy()
                .into_owned()
        }
    };

    if let catalog::Kind::Renderer { notice, .. } = entry.kind {
        // The bundles that use it copy this (design 10 §8, as for runtimes).
        std::fs::write(
            extract_dir.join("NOTICES.txt"),
            format!(
                "{} {} — {}\n  {}\n\nDownloaded by gogo on this Mac from the upstream \
                 release; gogo doesn't redistribute it.\n{}",
                entry.id,
                rel.tag,
                entry.license,
                rel.asset_url,
                notice.map(|n| format!("\n{n}")).unwrap_or_default()
            ),
        )?;
    }

    // Commit: rename into place, drop the archive, update the manifest.
    let m = ManifestEntry {
        id: entry.id.into(),
        version: rel.tag.clone(),
        sha256: actual,
        fetched_at: Utc::now().to_rfc3339(),
        upstream_url: rel.asset_url.clone(),
        entry_rel,
        bytes,
    };
    let dest = tree_root(&m);
    std::fs::create_dir_all(dest.parent().expect("dest has a parent"))?;
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&extract_dir, &dest)?;
    let _ = std::fs::remove_file(&part);
    if let Some(p) = &frameworks_part {
        let _ = std::fs::remove_file(p);
    }

    record(&m)?;

    if matches!(
        entry.kind,
        catalog::Kind::Tool { .. } | catalog::Kind::Renderer { .. }
    ) {
        emit(&ProgressEvent::ToolProvisioned {
            id: entry.id.into(),
            version: rel.tag.clone(),
        });
    } else {
        emit(&ProgressEvent::RuntimeDownloadFinished {
            id: entry.id.into(),
            version: rel.tag.clone(),
        });
    }
    Ok(m)
}

/// Add or replace `m` in its manifest.
fn record(m: &ManifestEntry) -> Result<(), MacAppError> {
    let path = manifest_path_for(&m.id);
    let mut manifest: Vec<ManifestEntry> = load_manifest_at(&path)
        .into_iter()
        .filter(|e| !(e.id == m.id && e.version == m.version))
        .collect();
    manifest.push(m.clone());
    save_manifest_at(&path, &manifest)
}

/// `<ver>+<renderer>-<rver>`: the version of the derived runtime for a base
/// runtime version and a renderer (design 16 decision 3).
pub fn derived_version(base_version: &str, renderer: &ManifestEntry) -> String {
    format!("{base_version}+{}-{}", renderer.id, renderer.version)
}

/// The derived runtime for `base` × `renderer` (design 16 decision 3):
/// `runtimes/<id>/<ver>+<renderer>-<rver>/`, built once and shared by every
/// game using that pair. `engine/` is an APFS clone of the base engine with
/// the renderer's `<arch>-windows`/`<arch>-unix` dirs copied over
/// `lib/wine/`; `Frameworks` is a relative symlink to the base's. The base
/// runtime is never written. An existing derived runtime is reused as is
/// (immutable: a bumped input gets a new version, never a patch).
pub fn derive(
    base: &ManifestEntry,
    renderer: &ManifestEntry,
) -> Result<ManifestEntry, MacAppError> {
    let version = derived_version(&base.version, renderer);
    if let Some((m, _)) = installed(&base.id, Some(&version)) {
        return Ok(m);
    }
    let base_root = tree_root(base);
    let invalid = |path: &Path, reason: String| MacAppError::RuntimeInvalid {
        path: path.to_path_buf(),
        reason,
    };
    if !base.entry_rel.starts_with("engine/") {
        return Err(invalid(
            &base_root,
            "a derived runtime needs the {engine,Frameworks} store layout".into(),
        ));
    }
    // The renderer's arch dirs sit next to its payload's dir
    // (`v0.80/x86_64-unix/winemetal.so` → `v0.80/`).
    let payload = tree_root(renderer).join(&renderer.entry_rel);
    let overlay_src = payload
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| invalid(&payload, "renderer payload has no arch dir".into()))?
        .to_path_buf();
    let arch_dirs: Vec<PathBuf> = std::fs::read_dir(&overlay_src)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with("-windows") || n.ends_with("-unix"))
        })
        .collect();
    if arch_dirs.is_empty() {
        return Err(invalid(
            &overlay_src,
            "renderer has no <arch>-windows/<arch>-unix dirs".into(),
        ));
    }

    std::fs::create_dir_all(tmp_dir())?;
    let work = tmp_dir().join(format!("{}-{version}", base.id));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let engine = work.join("engine");
    super::bundle::clone_tree(&base_root.join("engine"), &engine)?;
    let lib_wine = engine.join("lib/wine");
    let mut bytes = 0u64;
    for dir in &arch_dirs {
        let dst = lib_wine.join(dir.file_name().expect("arch dir has a name"));
        bytes += overlay(dir, &dst)?;
    }
    // Host libraries the unix side loads by `@rpath` = `@loader_path`
    // (D3DMetal: `d3d11.so` → `libd3dshared.dylib` →
    // `D3DMetal.framework`) go next to the unix libs, so dyld finds them
    // with no `DYLD_*` help.
    let external = overlay_src.parent().map(|p| p.join("external"));
    if let Some(external) = external.filter(|e| e.is_dir()) {
        for dir in arch_dirs
            .iter()
            .filter(|d| d.to_string_lossy().ends_with("-unix"))
        {
            let dst = lib_wine.join(dir.file_name().expect("arch dir has a name"));
            bytes += overlay(&external, &dst)?;
        }
    }
    if base_root.join("Frameworks").is_dir() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            Path::new("..").join(&base.version).join("Frameworks"),
            work.join("Frameworks"),
        )?;
    }
    let mut notices = std::fs::read_to_string(base_root.join("NOTICES.txt")).unwrap_or_default();
    if let Ok(r) = std::fs::read_to_string(tree_root(renderer).join("NOTICES.txt")) {
        notices.push_str(&format!(
            "\n— engine/lib/wine overlaid with {} {} —\n{r}",
            renderer.id, renderer.version
        ));
    }
    std::fs::write(work.join("NOTICES.txt"), notices)?;

    let dest = runtimes_dir().join(&base.id).join(&version);
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&work, &dest)?;
    // Probed in place: the Frameworks link is relative to the final location.
    if let Err(e) = super::runtime::probe_tree(&dest.join(&base.entry_rel)) {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(e);
    }
    let m = ManifestEntry {
        id: base.id.clone(),
        version,
        sha256: base.sha256.clone(),
        fetched_at: Utc::now().to_rfc3339(),
        upstream_url: format!(
            "derived:{}/{}+{}/{}",
            base.id, base.version, renderer.id, renderer.version
        ),
        entry_rel: base.entry_rel.clone(),
        bytes,
    };
    record(&m)?;
    Ok(m)
}

/// Copy `src`'s files over `dst` (recursively), replacing what is there.
/// A replaced file is unlinked first, so the copy never writes through into
/// an inode it might share with the base. Symlinks are copied as symlinks
/// (a framework's `Versions/Current -> A`). Returns the bytes copied.
fn overlay(src: &Path, dst: &Path) -> Result<u64, MacAppError> {
    std::fs::create_dir_all(dst)?;
    let mut bytes = 0;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        let kind = e.file_type()?;
        if kind.is_dir() {
            bytes += overlay(&e.path(), &to)?;
            continue;
        }
        if to.symlink_metadata().is_ok() {
            std::fs::remove_file(&to)?;
        }
        if kind.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(e.path())?, &to)?;
        } else {
            bytes += std::fs::copy(e.path(), &to)?;
        }
    }
    Ok(bytes)
}

fn archive_ext(name: &str) -> &'static str {
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        "tar.gz"
    } else if name.ends_with(".zip") {
        "zip"
    } else {
        "tar.xz"
    }
}

/// Download `rel` to `part` and check it against `rel.sha256` (the upstream
/// digest, or the catalog pin) and return its sha256. A mismatch deletes
/// the file.
async fn download_verified(
    http: &reqwest::Client,
    rel: &catalog::RemoteRelease,
    part: &Path,
    id: &str,
) -> Result<String, MacAppError> {
    download_with_retry(http, &rel.asset_url, part).await?;
    let actual = sha256_file(part)?;
    if let Some(expected) = &rel.sha256
        && !expected.eq_ignore_ascii_case(&actual)
    {
        let _ = std::fs::remove_file(part);
        return Err(MacAppError::RuntimeChecksumMismatch {
            id: id.into(),
            expected: expected.clone(),
            actual,
        });
    }
    Ok(actual)
}

/// Unpack a tar.xz / tar.gz / zip into `dest` (decoder by asset name).
fn unpack(archive: &Path, asset_name: &str, dest: &Path) -> Result<(), MacAppError> {
    std::fs::create_dir_all(dest)?;
    let file = std::fs::File::open(archive)?;
    match archive_ext(asset_name) {
        "zip" => {
            // Tool zips (design 13 D2: cnc-ddraw) unpack entry-by-entry;
            // `enclosed_name` keeps members inside the extract dir.
            let zip_err = |e: zip::result::ZipError| MacAppError::Recipe(format!("zip: {e}"));
            let mut zip = zip::ZipArchive::new(file).map_err(zip_err)?;
            for i in 0..zip.len() {
                let mut entry = zip.by_index(i).map_err(zip_err)?;
                let Some(rel_path) = entry.enclosed_name() else {
                    continue;
                };
                let out = dest.join(rel_path);
                if entry.is_dir() {
                    std::fs::create_dir_all(&out)?;
                } else {
                    if let Some(parent) = out.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let mut out_file = std::fs::File::create(&out)?;
                    std::io::copy(&mut entry, &mut out_file)?;
                }
            }
        }
        ext => {
            let decoder: Box<dyn std::io::Read> = if ext == "tar.gz" {
                Box::new(flate2::read::GzDecoder::new(file))
            } else {
                Box::new(xz2::read::XzDecoder::new(file))
            };
            let mut tar = tar::Archive::new(decoder);
            tar.set_preserve_permissions(true);
            tar.unpack(dest)?;
        }
    }
    Ok(())
}

/// Unpack the members under `dir` inside a tar.xz into `dest`, `dir`
/// stripped (files, dirs and symlinks as they are); returns how many.
/// Members whose rest of the path isn't plain names are skipped.
fn unpack_subtree(archive: &Path, dir: &str, dest: &Path) -> Result<usize, MacAppError> {
    std::fs::create_dir_all(dest)?;
    let decoder = xz2::read::XzDecoder::new(std::fs::File::open(archive)?);
    let mut tar = tar::Archive::new(decoder);
    tar.set_preserve_permissions(true);
    let dir = Path::new(dir);
    let mut n = 0;
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let Ok(rest) = path.strip_prefix(dir) else {
            continue;
        };
        if rest.as_os_str().is_empty()
            || !rest
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
        {
            continue;
        }
        let out = dest.join(rest);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry.unpack(&out)?;
        n += 1;
    }
    Ok(n)
}

/// Unpack only the top-level `*.dylib` members (files and symlinks) of `dir`
/// inside a tar.xz into `dest`; returns how many. Symlinks stay symlinks
/// (`libfreetype.dylib -> libfreetype.6.dylib`).
fn unpack_dylibs(archive: &Path, dir: &str, dest: &Path) -> Result<usize, MacAppError> {
    std::fs::create_dir_all(dest)?;
    let decoder = xz2::read::XzDecoder::new(std::fs::File::open(archive)?);
    let mut tar = tar::Archive::new(decoder);
    tar.set_preserve_permissions(true);
    let dir = Path::new(dir);
    let mut n = 0;
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let Ok(rest) = path.strip_prefix(dir) else {
            continue;
        };
        let mut parts = rest.components();
        let (Some(std::path::Component::Normal(name)), None) = (parts.next(), parts.next()) else {
            continue;
        };
        let is_dylib = Path::new(name).extension().is_some_and(|e| e == "dylib");
        let kind = entry.header().entry_type();
        if is_dylib && (kind.is_file() || kind.is_symlink()) {
            entry.unpack(dest.join(name))?;
            n += 1;
        }
    }
    Ok(n)
}

/// The engine tree inside an extracted archive: `<dir>` itself or its one
/// child holding `bin/wine{,64}` (`wswine.bundle/` for Sikarugir engines).
fn engine_dir_in(dir: &Path) -> Option<PathBuf> {
    if wine_in(&dir.join("bin")).is_some() {
        return Some(dir.to_path_buf());
    }
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| wine_in(&p.join("bin")).is_some())
}

/// `NOTICES.txt` for a store entry with a Frameworks set: the engine's
/// licence and source, then one line per installed dylib (#62).
fn notices_text(
    entry: &CatalogEntry,
    rel: &catalog::RemoteRelease,
    fw: &catalog::FrameworksSet,
    frameworks: &Path,
) -> Result<String, MacAppError> {
    let mut out = format!(
        "{} {} — {}\n  {}\n\n\
         Frameworks/ — the engine's dylib closure, from {} ({}), installed by\n\
         gogo on this Mac. gogo downloads these; it doesn't redistribute them.\n\n",
        entry.id, rel.tag, entry.license, rel.asset_url, fw.asset.asset, fw.asset.repo,
    );
    let mut names: Vec<String> = std::fs::read_dir(frameworks)?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut prefixes: Vec<&(&str, &str)> = fw.notices.iter().collect();
    prefixes.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
    for name in names {
        let licence = prefixes.iter().find(|(p, _)| name.starts_with(p)).map_or(
            "licence not catalogued — see the upstream project",
            |(_, l)| l,
        );
        out.push_str(&format!("{name}: {licence}\n"));
    }
    Ok(out)
}

/// Remove every cached version of `ids` (runtimes, derived runtimes and
/// renderers alike); returns the removed tree roots. Callers check the
/// game references first (design 16 decision 6).
pub fn prune(ids: &[String]) -> Result<Vec<PathBuf>, MacAppError> {
    let mut removed = Vec::new();
    for (path, root) in [
        (manifest_path(), runtimes_dir()),
        (renderer_manifest_path(), renderers_dir()),
    ] {
        let all = load_manifest_at(&path);
        let keep: Vec<ManifestEntry> = all
            .iter()
            .filter(|m| !ids.iter().any(|i| i == &m.id))
            .cloned()
            .collect();
        for id in ids {
            let dir = root.join(id);
            if dir.is_dir() {
                std::fs::remove_dir_all(&dir)?;
                removed.push(dir);
            }
        }
        if keep.len() != all.len() {
            save_manifest_at(&path, &keep)?;
        }
    }
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
    // Store layout with a Frameworks set: `engine/bin/wine`.
    if let Some(wine) = wine_in(&dir.join("engine/bin")) {
        let version = super::runtime::probe_tree(&wine)?;
        return Ok((wine, version));
    }
    Err(MacAppError::RuntimeInvalid {
        path: dir.to_path_buf(),
        reason: "archive has no bin/wine{,64} (bare, *.app/Contents/Resources/wine or engine/)"
            .into(),
    })
}

/// Find a named file in an extracted tool archive (bounded depth 3, shallowest
/// match wins — release tarballs put `winetricks` at root or one level down).
fn locate_named_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut best: Option<(usize, PathBuf)> = None;
    let mut stack = vec![(0usize, dir.to_path_buf())];
    while let Some((depth, d)) = stack.pop() {
        if depth > 3 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push((depth + 1, p));
            } else if p.file_name().and_then(|n| n.to_str()) == Some(name)
                && best.as_ref().is_none_or(|(bd, _)| depth < *bd)
            {
                best = Some((depth, p));
            }
        }
    }
    best.map(|(_, p)| p)
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

        let m = install(entry, None, &http, &server.uri(), false, &mut |_| {})
            .await
            .unwrap();
        assert_eq!(m.version, "11.18");
        assert_eq!(
            m.entry_rel,
            "Wine Staging.app/Contents/Resources/wine/bin/wine"
        );
        let root = tree_root(&m);
        assert!(root.join(&m.entry_rel).is_file());
        assert_eq!(m.sha256, sha256_hex(&body));

        // Reuse: a second install must not hit the download URL again.
        let m2 = install(entry, None, &http, &server.uri(), false, &mut |_| {})
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
        let err = install(entry, None, &http, &server.uri(), false, &mut |_| {})
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
            false,
            &mut |_| {},
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("consent"));
        // Tools are no longer hard-gated: with an unreachable catalog the
        // install fails as a catalog error instead.
        let err = install(
            catalog::find("winetricks").unwrap(),
            None,
            &http,
            &server.uri(),
            false,
            &mut |_| {},
        )
        .await
        .unwrap_err();
        assert!(matches!(err, MacAppError::CatalogUnreachable { .. }));
        let err = install(
            catalog::find("wine-crossover").unwrap(),
            None,
            &http,
            &server.uri(),
            false,
            &mut |_| {},
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("gone since 2026-10-19"));
    }

    #[tokio::test]
    // Same env-guard-spanning-await justification as above.
    #[allow(clippy::await_holding_lock)]
    async fn gptk_consent_flag_enables_download() {
        // D9: the interactive EULA Confirm records consent; install then
        // proceeds like any Available entry.
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        let body = fake_wrapper_tar();
        let server = mock_github(&body, &format!("sha256:{}", sha256_hex(&body))).await;
        // gptk lives in its own repo: serve that release path too.
        let gptk_url = format!("{}/download/game-porting-toolkit-3.0.tar.xz", server.uri());
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/repos/Gcenx/game-porting-toolkit/releases/latest",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "tag_name": "3.0",
                    "prerelease": false,
                    "published_at": "2026-09-25T15:27:03Z",
                    "assets": [{
                        "name": "game-porting-toolkit-3.0.tar.xz",
                        "size": body.len(),
                        "browser_download_url": gptk_url,
                        "digest": format!("sha256:{}", sha256_hex(&body)),
                    }],
                })),
            )
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/download/game-porting-toolkit-3.0.tar.xz",
            ))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
            .mount(&server)
            .await;
        let entry = catalog::find("gptk").unwrap();
        let http = catalog::http_client().unwrap();
        let m = install(entry, None, &http, &server.uri(), true, &mut |_| {})
            .await
            .unwrap();
        assert_eq!(m.id, "gptk");
        assert!(installed("gptk", None).is_some());
    }

    #[tokio::test]
    // The env guard must span install's awaits: GOGO_HOME is read throughout.
    // Safe: the test runtime is single-threaded and no other task touches env.
    #[allow(clippy::await_holding_lock)]
    async fn winetricks_provisioning_round_trip() {
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        // A release tarball carrying an executable `winetricks` script.
        let mut tar_buf = Vec::new();
        {
            let mut t = tar::Builder::new(&mut tar_buf);
            let script = b"#!/bin/sh\nWINETRICKS_VERSION=20260125-test\n";
            let mut h = tar::Header::new_gnu();
            h.set_size(script.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            t.append_data(&mut h, "winetricks", &script[..]).unwrap();
            t.finish().unwrap();
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        use std::io::Write as _;
        gz.write_all(&tar_buf).unwrap();
        let body = gz.finish().unwrap();
        let digest = format!("sha256:{}", sha256_hex(&body));

        let server = wiremock::MockServer::start().await;
        let asset_url = format!("{}/download/20260125.tar.gz", server.uri());
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/repos/Winetricks/winetricks/releases/latest",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "tag_name": "20260125",
                    "prerelease": false,
                    "published_at": "2026-01-25T00:00:00Z",
                    "assets": [{
                        "name": "20260125.tar.gz",
                        "size": body.len(),
                        "browser_download_url": asset_url,
                        "digest": digest,
                    }],
                })),
            )
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/download/20260125.tar.gz"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(body))
            .expect(1)
            .mount(&server)
            .await;

        let entry = catalog::find("winetricks").unwrap();
        let http = catalog::http_client().unwrap();
        let mut events: Vec<&'static str> = Vec::new();
        let m = install(entry, None, &http, &server.uri(), false, &mut |ev| {
            use crate::engine::events::ProgressEvent as Ev;
            let name = match ev {
                Ev::RuntimeDownloadStarted { .. } => "started",
                Ev::ToolProvisioned { .. } => "provisioned",
                _ => "other",
            };
            events.push(name);
        })
        .await
        .unwrap();
        assert_eq!(m.version, "20260125");
        assert_eq!(m.entry_rel, "winetricks");
        assert_eq!(events, vec!["started", "provisioned"]);
        let wt = tree_root(&m).join(&m.entry_rel);
        assert!(wt.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert!(wt.metadata().unwrap().permissions().mode() & 0o111 != 0);
        }
        // Reuse: a second install must not re-download.
        let m2 = install(entry, None, &http, &server.uri(), false, &mut |_| {})
            .await
            .unwrap();
        assert_eq!(m2.version, "20260125");
        server.verify().await;
    }

    fn tar_xz(build: impl FnOnce(&mut tar::Builder<&mut Vec<u8>>)) -> Vec<u8> {
        use std::io::Write as _;
        let mut tar_buf = Vec::new();
        {
            let mut t = tar::Builder::new(&mut tar_buf);
            build(&mut t);
            t.finish().unwrap();
        }
        let mut xz = xz2::write::XzEncoder::new(Vec::new(), 6);
        xz.write_all(&tar_buf).unwrap();
        xz.finish().unwrap()
    }

    fn add_file(t: &mut tar::Builder<&mut Vec<u8>>, path: &str, body: &[u8], mode: u32) {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(mode);
        h.set_cksum();
        t.append_data(&mut h, path, body).unwrap();
    }

    fn add_symlink(t: &mut tar::Builder<&mut Vec<u8>>, path: &str, target: &str) {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        h.set_mode(0o755);
        t.append_link(&mut h, path, target).unwrap();
    }

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    // The env guard must span install's awaits: GOGO_HOME is read throughout.
    // Safe: the test runtime is single-threaded and no other task touches env.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn engine_with_frameworks_round_trip() {
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        // A Sikarugir-style engine archive (`wswine.bundle/…`) …
        let engine = tar_xz(|t| {
            add_file(
                t,
                "wswine.bundle/bin/wine",
                b"#!/bin/sh\necho 'wine-9.0 (SikarugirCX 24.0.7)'\n",
                0o755,
            );
            add_file(
                t,
                "wswine.bundle/bin/wineserver",
                b"#!/bin/sh\necho 'Wine 9.0'\n",
                0o755,
            );
            add_file(t, "wswine.bundle/version", b"WineCX 24.0.7\n", 0o644);
        });
        // … and a Template whose Frameworks/ holds dylibs, a symlink, and
        // things that must be left out.
        let fw_dir = "Template-1.0.24.app/Contents/Frameworks";
        let template = tar_xz(|t| {
            add_file(t, &format!("{fw_dir}/libinotify.0.dylib"), b"ino", 0o755);
            add_file(t, &format!("{fw_dir}/libfreetype.6.dylib"), b"ft", 0o755);
            add_symlink(
                t,
                &format!("{fw_dir}/libfreetype.dylib"),
                "libfreetype.6.dylib",
            );
            add_file(t, &format!("{fw_dir}/libunknown.1.dylib"), b"?", 0o755);
            add_file(t, &format!("{fw_dir}/README"), b"no", 0o644);
            add_file(
                t,
                &format!("{fw_dir}/GStreamer.framework/Versions/1.0/lib/libgst.dylib"),
                b"no",
                0o755,
            );
            add_file(
                t,
                "Template-1.0.24.app/Contents/MacOS/Sikarugir",
                b"no",
                0o755,
            );
        });

        let server = wiremock::MockServer::start().await;
        for (repo, name, body) in [
            (
                "Sikarugir-App/Engines",
                "WS12WineCX24.0.7_7.tar.xz",
                &engine,
            ),
            (
                "Sikarugir-App/Template",
                "Template-1.0.24.tar.xz",
                &template,
            ),
        ] {
            wiremock::Mock::given(wiremock::matchers::method("GET"))
                .and(wiremock::matchers::path(format!(
                    "/repos/{repo}/releases/tags/v1.0"
                )))
                .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({
                        "tag_name": "v1.0", "prerelease": false,
                        "published_at": "2024-10-03T20:04:52Z",
                        "assets": [{
                            "name": name,
                            "size": body.len(),
                            "browser_download_url": format!("{}/dl/{name}", server.uri()),
                            "digest": format!("sha256:{}", sha256_hex(body)),
                        }],
                    }),
                ))
                .mount(&server)
                .await;
            wiremock::Mock::given(wiremock::matchers::method("GET"))
                .and(wiremock::matchers::path(format!("/dl/{name}")))
                .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(body.clone()))
                .expect(1)
                .mount(&server)
                .await;
        }
        let pin = |repo, asset, version, body: &[u8]| catalog::PinnedAsset {
            repo,
            tag: "v1.0",
            asset,
            version,
            sha256: leak(sha256_hex(body)),
        };
        let real = catalog::find("winecx").unwrap();
        let entry: &'static CatalogEntry = Box::leak(Box::new(CatalogEntry {
            id: "winecx",
            kind: catalog::Kind::Wine,
            status: Status::Available,
            discovery: Some(catalog::Discovery::Pinned(pin(
                "Sikarugir-App/Engines",
                "WS12WineCX24.0.7_7.tar.xz",
                "24.0.7_7",
                &engine,
            ))),
            license: real.license,
            redistributable: true,
            min_macos: None,
            arch: real.arch,
            note: "test",
            frameworks: Some(catalog::FrameworksSet {
                asset: pin(
                    "Sikarugir-App/Template",
                    "Template-1.0.24.tar.xz",
                    "1.0.24",
                    &template,
                ),
                dir: fw_dir,
                notices: real.frameworks.as_ref().unwrap().notices,
            }),
        }));
        let http = catalog::http_client().unwrap();

        let m = install(entry, None, &http, &server.uri(), false, &mut |_| {})
            .await
            .unwrap();
        assert_eq!(m.version, "24.0.7_7");
        assert_eq!(m.entry_rel, "engine/bin/wine");
        assert_eq!(m.sha256, sha256_hex(&engine));
        // Design 16 decision 6: `runtimes/<id>/<ver>/{engine,Frameworks}`.
        let root = tree_root(&m);
        assert_eq!(root, runtimes_dir().join("winecx/24.0.7_7"));
        assert!(root.join("engine/bin/wineserver").is_file());
        assert!(root.join("engine/version").is_file());
        assert!(!root.join("wswine.bundle").exists());
        let mut fw: Vec<String> = std::fs::read_dir(root.join("Frameworks"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        fw.sort();
        assert_eq!(
            fw,
            [
                "libfreetype.6.dylib",
                "libfreetype.dylib",
                "libinotify.0.dylib",
                "libunknown.1.dylib"
            ]
        );
        let link = root.join("Frameworks/libfreetype.dylib");
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            Path::new("libfreetype.6.dylib")
        );
        // The engine finds its closure there.
        assert_eq!(
            super::super::runtime::frameworks_for(&root.join(&m.entry_rel)),
            Some(root.join("Frameworks"))
        );
        // Licence notes per dylib, with a visible gap for unknown ones.
        let notices = std::fs::read_to_string(root.join("NOTICES.txt")).unwrap();
        assert!(notices.contains("LGPL-2.1"), "{notices}");
        assert!(
            notices.contains("libfreetype.6.dylib: FreeType"),
            "{notices}"
        );
        assert!(notices.contains("libinotify.0.dylib: libinotify-kqueue — MIT"));
        assert!(notices.contains("libunknown.1.dylib: licence not catalogued"));
        assert!(!runtimes_dir().join(".tmp/winecx-24.0.7_7").exists());

        // Reuse: a second install downloads nothing (each asset expects 1 hit).
        install(entry, None, &http, &server.uri(), false, &mut |_| {})
            .await
            .unwrap();
        server.verify().await;
    }

    // As above: the env guard spans install's awaits on a single thread.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn d3dmetal_installs_from_the_template_subdir_on_consent_and_derives() {
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        let sub = catalog::D3DMETAL_SUBDIR;
        let template = tar_xz(|t| {
            add_file(
                t,
                "Template-1.0.24.app/Contents/Frameworks/libz.1.dylib",
                b"z",
                0o755,
            );
            add_file(t, &format!("{sub}/License.rtf"), b"EA18380", 0o644);
            add_file(t, &format!("{sub}/Acknowledgements.rtf"), b"ack", 0o644);
            add_file(
                t,
                &format!("{sub}/wine/x86_64-unix/d3d12.so"),
                b"d3d12",
                0o755,
            );
            add_file(
                t,
                &format!("{sub}/wine/x86_64-unix/d3d11.so"),
                b"d3d11",
                0o755,
            );
            add_file(
                t,
                &format!("{sub}/wine/x86_64-windows/d3d11.dll"),
                b"gptk-64",
                0o644,
            );
            add_file(
                t,
                &format!("{sub}/external/libd3dshared.dylib"),
                b"shared",
                0o755,
            );
            let fw = format!("{sub}/external/D3DMetal.framework");
            add_file(t, &format!("{fw}/Versions/A/D3DMetal"), b"metal", 0o755);
            add_symlink(t, &format!("{fw}/Versions/Current"), "A");
            add_symlink(t, &format!("{fw}/D3DMetal"), "Versions/Current/D3DMetal");
            add_file(
                t,
                "Template-1.0.24.app/Contents/SharedSupport/renderer/dxmt/LICENSE",
                b"no",
                0o644,
            );
        });
        let server = wiremock::MockServer::start().await;
        let name = "Template-1.0.24.tar.xz";
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/repos/Sikarugir-App/Template/releases/tags/v1.0",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "tag_name": "v1.0", "prerelease": false,
                    "published_at": "2026-10-17T13:10:02Z",
                    "assets": [{
                        "name": name,
                        "size": template.len(),
                        "browser_download_url": format!("{}/dl/{name}", server.uri()),
                        "digest": format!("sha256:{}", sha256_hex(&template)),
                    }],
                })),
            )
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(format!("/dl/{name}")))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(template.clone()))
            .expect(1)
            .mount(&server)
            .await;
        let real = catalog::find("d3dmetal").unwrap();
        let entry: &'static CatalogEntry = Box::leak(Box::new(CatalogEntry {
            id: "d3dmetal",
            kind: catalog::Kind::Renderer {
                payload: "d3d12.so",
                overlay: true,
                subdir: Some(sub),
                notice: Some(catalog::D3DMETAL_NOTICE),
            },
            status: Status::ConsentRequired,
            discovery: Some(catalog::Discovery::Pinned(catalog::PinnedAsset {
                repo: "Sikarugir-App/Template",
                tag: "v1.0",
                asset: name,
                version: "4.0b3",
                sha256: leak(sha256_hex(&template)),
            })),
            license: real.license,
            redistributable: false,
            min_macos: None,
            arch: real.arch,
            note: "test",
            frameworks: None,
        }));
        let http = catalog::http_client().unwrap();

        // No consent: refused before any download, naming the config key.
        let err = install(entry, None, &http, &server.uri(), false, &mut |_| {})
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("[mac_app] d3dmetal"), "{err}");
        let m = install(entry, None, &http, &server.uri(), true, &mut |_| {})
            .await
            .unwrap();
        server.verify().await;
        assert_eq!(m.version, "4.0b3");
        assert_eq!(m.entry_rel, "wine/x86_64-unix/d3d12.so");
        let r = tree_root(&m);
        assert_eq!(r, renderers_dir().join("d3dmetal/4.0b3/x86_64"));
        // Only the kit: Apple's files kept, nothing else from the Template.
        assert!(r.join("License.rtf").is_file() && r.join("Acknowledgements.rtf").is_file());
        assert!(!r.join("Template-1.0.24.app").exists() && !r.join("libz.1.dylib").exists());
        assert_eq!(
            std::fs::read_link(r.join("external/D3DMetal.framework/Versions/Current")).unwrap(),
            Path::new("A")
        );
        let notices = std::fs::read_to_string(r.join("NOTICES.txt")).unwrap();
        assert!(
            notices.contains("EA18380") && notices.contains("Apple Inc."),
            "{notices}"
        );

        // Derived from a CX-layout base: wine dirs over lib/wine, the host
        // libs next to the unix libs (`@loader_path`), symlinks kept.
        let base = self::entry("winecx", "24.0.7_7", "engine/bin/wine");
        let root = tree_root(&base);
        write_exec(
            &root.join("engine/bin/wine"),
            "#!/bin/sh\necho 'wine-9.0 (SikarugirCX 24.0.7)'\n",
        );
        let lib = root.join("engine/lib/wine");
        std::fs::create_dir_all(lib.join("x86_64-windows")).unwrap();
        std::fs::write(lib.join("x86_64-windows/d3d11.dll"), "wined3d-64").unwrap();
        std::fs::create_dir_all(lib.join("x86_64-unix")).unwrap();
        save_manifest(std::slice::from_ref(&base)).unwrap();
        let d = derive(&base, &m).unwrap();
        assert_eq!(d.version, "24.0.7_7+d3dmetal-4.0b3");
        let dunix = tree_root(&d).join("engine/lib/wine/x86_64-unix");
        let read = |p: PathBuf| std::fs::read_to_string(p).unwrap();
        assert_eq!(read(dunix.join("d3d11.so")), "d3d11");
        assert_eq!(read(dunix.join("libd3dshared.dylib")), "shared");
        assert_eq!(read(dunix.join("D3DMetal.framework/D3DMetal")), "metal");
        assert!(
            dunix
                .join("D3DMetal.framework/Versions/Current")
                .is_symlink()
        );
        assert_eq!(
            read(tree_root(&d).join("engine/lib/wine/x86_64-windows/d3d11.dll")),
            "gptk-64"
        );
        assert!(!lib.join("x86_64-unix/libd3dshared.dylib").exists());
        assert!(read(tree_root(&d).join("NOTICES.txt")).contains("EA18380"));
    }

    fn write_exec(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn entry(id: &str, version: &str, entry_rel: &str) -> ManifestEntry {
        ManifestEntry {
            id: id.into(),
            version: version.into(),
            sha256: "abc".into(),
            fetched_at: "2026-10-23T00:00:00Z".into(),
            upstream_url: "http://x".into(),
            entry_rel: entry_rel.into(),
            bytes: 1,
        }
    }

    #[test]
    fn derive_overlays_a_clone_and_leaves_the_base_alone() {
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        // A base store runtime …
        let base = entry("winecx", "24.0.7_7", "engine/bin/wine");
        let root = tree_root(&base);
        write_exec(
            &root.join("engine/bin/wine"),
            "#!/bin/sh\necho 'wine-9.0 (SikarugirCX 24.0.7)'\n",
        );
        let lib = root.join("engine/lib/wine");
        for (rel, body) in [
            ("x86_64-windows/d3d11.dll", "wined3d-64"),
            ("i386-windows/d3d11.dll", "wined3d-32"),
            ("x86_64-windows/kernel32.dll", "k32"),
            ("x86_64-unix/ntdll.so", "ntdll"),
        ] {
            std::fs::create_dir_all(lib.join(rel).parent().unwrap()).unwrap();
            std::fs::write(lib.join(rel), body).unwrap();
        }
        std::fs::create_dir_all(root.join("Frameworks")).unwrap();
        std::fs::write(root.join("Frameworks/libinotify.0.dylib"), "ino").unwrap();
        std::fs::write(root.join("NOTICES.txt"), "winecx notices\n").unwrap();
        save_manifest(std::slice::from_ref(&base)).unwrap();
        // … and DXMT as downloaded (`v0.80/<arch>-*/`).
        let dxmt = entry("dxmt", "v0.80", "v0.80/x86_64-unix/winemetal.so");
        let r = tree_root(&dxmt);
        assert_eq!(r, renderers_dir().join("dxmt/v0.80/x86_64"));
        for (rel, body) in [
            ("v0.80/x86_64-windows/d3d11.dll", "dxmt-64"),
            ("v0.80/i386-windows/d3d11.dll", "dxmt-32"),
            ("v0.80/x86_64-unix/winemetal.so", "metal"),
        ] {
            std::fs::create_dir_all(r.join(rel).parent().unwrap()).unwrap();
            std::fs::write(r.join(rel), body).unwrap();
        }
        std::fs::write(r.join("NOTICES.txt"), "dxmt LGPL\n").unwrap();
        save_renderer_manifest(std::slice::from_ref(&dxmt)).unwrap();
        assert!(installed("dxmt", Some("v0.80")).is_some());

        let m = derive(&base, &dxmt).unwrap();
        assert_eq!(m.version, "24.0.7_7+dxmt-v0.80");
        let d = tree_root(&m);
        assert_eq!(d, runtimes_dir().join("winecx/24.0.7_7+dxmt-v0.80"));
        let dlib = d.join("engine/lib/wine");
        let read = |p: PathBuf| std::fs::read_to_string(p).unwrap();
        assert_eq!(read(dlib.join("x86_64-windows/d3d11.dll")), "dxmt-64");
        assert_eq!(read(dlib.join("i386-windows/d3d11.dll")), "dxmt-32");
        assert_eq!(read(dlib.join("x86_64-unix/winemetal.so")), "metal");
        assert_eq!(read(dlib.join("x86_64-windows/kernel32.dll")), "k32");
        assert_eq!(read(dlib.join("x86_64-unix/ntdll.so")), "ntdll");
        // The base is untouched.
        assert_eq!(read(lib.join("x86_64-windows/d3d11.dll")), "wined3d-64");
        assert!(!lib.join("x86_64-unix/winemetal.so").exists());
        // Frameworks is shared by a relative link; notices carry both.
        assert_eq!(
            std::fs::read_link(d.join("Frameworks")).unwrap(),
            Path::new("../24.0.7_7/Frameworks")
        );
        assert!(d.join("Frameworks/libinotify.0.dylib").is_file());
        let notices = read(d.join("NOTICES.txt"));
        assert!(notices.contains("winecx notices") && notices.contains("dxmt LGPL"));
        assert_eq!(
            super::super::runtime::frameworks_for(&d.join(&m.entry_rel)),
            Some(d.join("Frameworks"))
        );

        // Reused, never rebuilt; unpinned lookups keep finding the base.
        std::fs::write(dlib.join("marker"), "x").unwrap();
        assert_eq!(derive(&base, &dxmt).unwrap().version, m.version);
        assert!(dlib.join("marker").is_file());
        assert_eq!(installed("winecx", None).unwrap().0.version, "24.0.7_7");
        assert!(installed("winecx", Some("24.0.7_7+dxmt-v0.80")).is_some());

        // Prune reaches both stores.
        let removed = prune(&["winecx".to_string(), "dxmt".to_string()]).unwrap();
        assert_eq!(removed.len(), 2);
        assert!(load_manifest().is_empty() && load_renderer_manifest().is_empty());
    }

    #[test]
    fn derive_needs_the_engine_layout() {
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        let base = entry(
            "wine-staging",
            "11.18",
            "Wine Staging.app/Contents/Resources/wine/bin/wine",
        );
        let dxmt = entry("dxmt", "v0.80", "v0.80/x86_64-unix/winemetal.so");
        let err = derive(&base, &dxmt).unwrap_err();
        assert!(err.to_string().contains("{engine,Frameworks}"), "{err}");
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
            entry_rel: "bin/wine".into(),
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
