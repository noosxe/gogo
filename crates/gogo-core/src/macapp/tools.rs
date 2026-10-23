//! winetricks + shim tooling (design 12 §8): the executor preference chain
//! (vendored → `WINETRICKS`/PATH override → skip-loud), on-demand
//! provisioning of the winetricks GitHub release into the store, and the
//! materialized re-exec extractor shims (`cabextract`, `7z`) PATH-scoped to
//! winetricks invocations — users install nothing (docs/15 §3, D5).

use std::path::{Path, PathBuf};

use crate::engine::events::ProgressEvent;

use super::error::MacAppError;
use super::{catalog, lutris, store};

/// Catalog id of the vendored winetricks tool.
pub const WINETRICKS_ID: &str = "winetricks";

/// `<runtimes>/tools/shims/` — materialized extractor shims (design 12 D5).
pub fn shim_dir() -> PathBuf {
    store::runtimes_dir().join("tools").join("shims")
}

/// Write the shim scripts (idempotent): tiny re-exec launchers that invoke
/// this very gogo binary's hidden `__shim` subcommand, so shims stay in
/// lockstep with the running gogo. Re-run before every winetricks
/// invocation — the gogo binary may have moved since the last one.
pub fn materialize_shims() -> Result<PathBuf, MacAppError> {
    let dir = shim_dir();
    std::fs::create_dir_all(&dir)?;
    let exe = std::env::current_exe()?;
    for tool in ["cabextract", "7z"] {
        let path = dir.join(tool);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nexec \"{}\" __shim {} \"$@\"\n",
                exe.display(),
                tool
            ),
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(dir)
}

/// `wine` / `wineserver` for a script to call (winetricks' `WINE`,
/// `WINESERVER`). An engine that needs its Frameworks on the dyld fallback
/// path gets tiny wrappers that export it before `exec`: SIP drops `DYLD_*`
/// whenever a protected binary (`/bin/sh`, `/usr/bin/env`) starts, so the
/// variable can't be passed down through the script. Other engines are
/// returned as they are.
pub fn wine_wrappers(wine: &Path, wineserver: &Path) -> Result<(PathBuf, PathBuf), MacAppError> {
    let Some(fw) = super::runtime::frameworks_for(wine) else {
        return Ok((wine.to_path_buf(), wineserver.to_path_buf()));
    };
    use sha2::{Digest as _, Sha256};
    let key: String = Sha256::digest(wine.as_os_str().as_encoded_bytes())[..6]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let dir = shim_dir().join(format!("wine-{key}"));
    std::fs::create_dir_all(&dir)?;
    let q = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', r"'\''"));
    let mut out = Vec::new();
    for (name, target) in [("wine", wine), ("wineserver", wineserver)] {
        let path = dir.join(name);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nDYLD_FALLBACK_LIBRARY_PATH={}${{DYLD_FALLBACK_LIBRARY_PATH:+:$DYLD_FALLBACK_LIBRARY_PATH}}\n\
                 export DYLD_FALLBACK_LIBRARY_PATH\nexec {} \"$@\"\n",
                q(&fw),
                q(target)
            ),
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        }
        out.push(path);
    }
    let server = out.pop().expect("two wrappers");
    Ok((out.pop().expect("two wrappers"), server))
}

/// `PATH` value with the shim dir prepended (scoped to child invocations —
/// the user's own environment is never modified).
pub fn path_with_shims() -> Result<String, MacAppError> {
    let dir = materialize_shims()?;
    let orig = std::env::var("PATH").unwrap_or_default();
    Ok(format!("{}:{}", dir.display(), orig))
}

/// The winetricks to use (design 12 D4 chain): `WINETRICKS` env → vendored
/// store copy (the catalog's pinned version first, else any cached one) →
/// host `winetricks` on PATH. `None` = skip-loud.
pub fn resolve_winetricks() -> Option<PathBuf> {
    env_winetricks()
        .or_else(|| store_winetricks(pinned_winetricks()))
        .or_else(|| store_winetricks(None))
        .or_else(|| which("winetricks"))
}

/// Resolve or auto-provision winetricks (network exception, design 12 D8).
/// A cached copy older than the catalog's pin is upgraded (#106) and kept
/// as the fallback should that fail. Provisioning failure degrades to
/// nothing — never fatal to the recipe.
pub fn resolve_or_provision_winetricks(emit: &mut dyn FnMut(&ProgressEvent)) -> Option<PathBuf> {
    resolve_or_provision_at(catalog::find(WINETRICKS_ID)?, catalog::GITHUB_API, emit)
}

fn resolve_or_provision_at(
    entry: &catalog::CatalogEntry,
    api_base: &str,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Option<PathBuf> {
    let pinned = entry.discovery.as_ref().and_then(|d| d.pinned_version());
    if let Some(p) = env_winetricks().or_else(|| store_winetricks(pinned)) {
        return Some(p);
    }
    let stale = store::installed(WINETRICKS_ID, None);
    if stale.is_none()
        && let Some(p) = which("winetricks")
    {
        return Some(p);
    }
    let fallback = stale.as_ref().map(|(m, root)| root.join(&m.entry_rel));
    let Ok(http) = catalog::http_client() else {
        return fallback;
    };
    let outcome =
        lutris::block_on_detached(store::install(entry, None, &http, api_base, false, emit));
    match (outcome, stale) {
        (Ok(m), stale) => {
            if let Some((old, _)) = stale {
                emit(&ProgressEvent::Warn {
                    message: format!(
                        "winetricks {} replaced {} for verbs; the old copy can go: \
                         `gogo runtimes prune winetricks/{}`",
                        m.version, old.version, old.version
                    ),
                });
            }
            let p = store::tree_root(&m).join(&m.entry_rel);
            p.is_file().then_some(p)
        }
        (Err(e), Some((old, _))) => {
            emit(&ProgressEvent::Warn {
                message: format!(
                    "winetricks upgrade failed ({e}); using the cached {}",
                    old.version
                ),
            });
            fallback
        }
        (Err(e), None) => {
            emit(&ProgressEvent::Warn {
                message: format!("winetricks auto-provision failed ({e}); skipping verbs"),
            });
            None
        }
    }
}

fn env_winetricks() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("WINETRICKS")?);
    p.is_file().then_some(p)
}

/// The version the catalog pins winetricks to.
fn pinned_winetricks() -> Option<&'static str> {
    catalog::find(WINETRICKS_ID)?
        .discovery
        .as_ref()?
        .pinned_version()
}

/// A cached store copy, of `version` or (with `None`) the newest.
fn store_winetricks(version: Option<&str>) -> Option<PathBuf> {
    let (m, root) = store::installed(WINETRICKS_ID, version)?;
    let p = root.join(&m.entry_rel);
    p.is_file().then_some(p)
}

/// Minimal `which`: first PATH entry holding a file (mirrors mod.rs's).
fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(bin))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tools_tests {
    use super::*;

    #[test]
    fn shims_materialize_and_resolve_chain() {
        let home = tempfile::tempdir().unwrap();
        let guard = crate::test_util::env_lock();
        // SAFETY: env mutation under the shared env mutex.
        unsafe { std::env::set_var("GOGO_HOME", home.path()) };
        // WINETRICKS override wins even with nothing installed.
        let wt = home.path().join("my-winetricks");
        std::fs::write(&wt, "#!/bin/sh\n").unwrap();
        // SAFETY: see above.
        unsafe { std::env::set_var("WINETRICKS", &wt) };
        assert_eq!(resolve_winetricks(), Some(wt));
        // SAFETY: see above.
        unsafe { std::env::remove_var("WINETRICKS") };

        let dir = materialize_shims().unwrap();
        for tool in ["cabextract", "7z"] {
            let p = dir.join(tool);
            let body = std::fs::read_to_string(&p).unwrap();
            assert!(body.contains("__shim"));
            assert!(body.contains(tool));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                assert!(p.metadata().unwrap().permissions().mode() & 0o111 != 0);
            }
        }
        // Idempotent re-run.
        materialize_shims().unwrap();
        // SAFETY: restore for other tests.
        unsafe { std::env::remove_var("GOGO_HOME") };
        drop(guard);
    }

    /// A cached winetricks `version`, fetched at `at` (store layout).
    fn cache_winetricks(version: &str, at: &str) -> PathBuf {
        let m = store::ManifestEntry {
            id: WINETRICKS_ID.into(),
            version: version.into(),
            sha256: "x".into(),
            fetched_at: at.into(),
            upstream_url: "test".into(),
            entry_rel: "winetricks".into(),
            bytes: 1,
        };
        let p = store::tree_root(&m).join(&m.entry_rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, format!("#!/bin/sh\nWINETRICKS_VERSION={version}\n")).unwrap();
        let mut all = store::load_manifest();
        all.push(m);
        store::save_manifest(&all).unwrap();
        p
    }

    /// #106: the catalog's pinned version wins over a stale copy, even one
    /// fetched later.
    #[test]
    fn resolve_prefers_the_pinned_store_copy() {
        let home = tempfile::tempdir().unwrap();
        let guard = crate::test_util::env_lock();
        // SAFETY: env mutation under the shared env mutex.
        unsafe { std::env::set_var("GOGO_HOME", home.path()) };
        let pinned = pinned_winetricks().unwrap();
        assert_eq!(pinned, "20260125");
        let old = cache_winetricks("20210206", "2026-01-01T00:00:00Z");
        assert_eq!(resolve_winetricks(), Some(old));
        let new = cache_winetricks(pinned, "2025-01-01T00:00:00Z");
        cache_winetricks("20220411", "2026-06-01T00:00:00Z");
        assert_eq!(resolve_winetricks(), Some(new));
        // SAFETY: restore for other tests.
        unsafe { std::env::remove_var("GOGO_HOME") };
        drop(guard);
    }

    /// A stale cached winetricks is upgraded to the pin, with a pointer at
    /// pruning it; offline, it stays in use.
    #[tokio::test(flavor = "multi_thread")]
    // The env guard must span the provisioning: GOGO_HOME is read throughout.
    #[allow(clippy::await_holding_lock)]
    async fn stale_winetricks_is_upgraded_or_kept() {
        let home = tempfile::tempdir().unwrap();
        let guard = crate::test_util::env_lock();
        // SAFETY: env mutation under the shared env mutex.
        unsafe { std::env::set_var("GOGO_HOME", home.path()) };
        let old = cache_winetricks("20210206", "2026-01-01T00:00:00Z");
        let body = b"#!/bin/sh\nWINETRICKS_VERSION=20260125\n";
        let mut warns = Vec::new();

        // Offline: the upgrade fails, the stale copy is used.
        let offline = wiremock::MockServer::start().await;
        let (_, entry) = store::store_tests::mock_winetricks(body, "20260125", 0).await;
        let got = resolve_or_provision_at(entry, &offline.uri(), &mut |ev| {
            if let ProgressEvent::Warn { message } = ev {
                warns.push(message.clone());
            }
        });
        assert_eq!(got, Some(old.clone()));
        assert!(warns[0].contains("using the cached 20210206"), "{warns:?}");

        warns.clear();
        let (server, entry) = store::store_tests::mock_winetricks(body, "20260125", 1).await;
        let got = resolve_or_provision_at(entry, &server.uri(), &mut |ev| {
            if let ProgressEvent::Warn { message } = ev {
                warns.push(message.clone());
            }
        })
        .unwrap();
        assert_ne!(got, old);
        assert_eq!(std::fs::read(&got).unwrap(), body);
        assert!(
            warns
                .iter()
                .any(|w| w.contains("20260125 replaced 20210206")
                    && w.contains("gogo runtimes prune winetricks/20210206")),
            "{warns:?}"
        );
        // Now current: resolved from the store, no second download.
        assert_eq!(
            resolve_or_provision_at(entry, &server.uri(), &mut |_| {}),
            Some(got)
        );
        server.verify().await;
        // SAFETY: restore for other tests.
        unsafe { std::env::remove_var("GOGO_HOME") };
        drop(guard);
    }

    /// The wrapper sets the fallback path itself, so it survives being run
    /// from a script under `/usr/bin/env` (SIP drops inherited `DYLD_*`).
    #[test]
    fn wine_wrappers_export_frameworks_through_env() {
        let home = tempfile::tempdir().unwrap();
        let guard = crate::test_util::env_lock();
        // SAFETY: env mutation under the shared env mutex.
        unsafe { std::env::set_var("GOGO_HOME", home.path()) };
        let ver = home.path().join("rt/it's 1.0");
        let bin = ver.join("engine/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(ver.join("Frameworks")).unwrap();
        // The fake wine is a script run by a shell SIP doesn't restrict (the
        // dev shell's; any shell off macOS), as the real wine is a binary
        // SIP doesn't restrict. Under /bin/sh it would lose DYLD_* again.
        let shell = which("bash")
            .filter(|p| !cfg!(target_os = "macos") || !p.starts_with("/bin"))
            .or_else(|| (!cfg!(target_os = "macos")).then(|| PathBuf::from("/bin/sh")));
        let Some(shell) = shell else {
            eprintln!("skipped: no unrestricted shell on PATH");
            return;
        };
        let fake = format!(
            "#!{}\necho \"$DYLD_FALLBACK_LIBRARY_PATH|$*\"\n",
            shell.display()
        );
        for t in ["wine", "wineserver"] {
            std::fs::write(bin.join(t), &fake).unwrap();
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(bin.join(t), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let (wine, server) = wine_wrappers(&bin.join("wine"), &bin.join("wineserver")).unwrap();
        assert_ne!(wine, bin.join("wine"));
        let out = std::process::Command::new("/usr/bin/env")
            .arg(&wine)
            .args(["cmd.exe", "/c"])
            .env_remove("DYLD_FALLBACK_LIBRARY_PATH")
            .output()
            .unwrap();
        let out = String::from_utf8(out.stdout).unwrap();
        assert_eq!(
            out.trim(),
            format!("{}|cmd.exe /c", ver.join("Frameworks").display())
        );
        assert!(server.ends_with("wineserver") && server.parent() == wine.parent());

        // No Frameworks dir: the engine as it is.
        let plain = home.path().join("plain/bin");
        std::fs::create_dir_all(&plain).unwrap();
        let (w, _) = wine_wrappers(&plain.join("wine"), &plain.join("wineserver")).unwrap();
        assert_eq!(w, plain.join("wine"));
        // SAFETY: restore for other tests.
        unsafe { std::env::remove_var("GOGO_HOME") };
        drop(guard);
    }
}
