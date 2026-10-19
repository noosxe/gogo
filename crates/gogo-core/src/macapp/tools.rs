//! winetricks + shim tooling (design 12 §8): the executor preference chain
//! (vendored → `WINETRICKS`/PATH override → skip-loud), on-demand
//! provisioning of the winetricks GitHub release into the store, and the
//! materialized re-exec extractor shims (`cabextract`, `7z`) PATH-scoped to
//! winetricks invocations — users install nothing (docs/15 §3, D5).

use std::path::PathBuf;

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

/// `PATH` value with the shim dir prepended (scoped to child invocations —
/// the user's own environment is never modified).
pub fn path_with_shims() -> Result<String, MacAppError> {
    let dir = materialize_shims()?;
    let orig = std::env::var("PATH").unwrap_or_default();
    Ok(format!("{}:{}", dir.display(), orig))
}

/// The winetricks to use (design 12 D4 chain): `WINETRICKS` env → vendored
/// store copy → host `winetricks` on PATH. `None` = skip-loud.
pub fn resolve_winetricks() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("WINETRICKS") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some((m, root)) = store::installed(WINETRICKS_ID, None)
        && root.join(&m.entry_rel).is_file()
    {
        return Some(root.join(&m.entry_rel));
    }
    which("winetricks")
}

/// Resolve or auto-provision winetricks (network exception, design 12 D8).
/// Provisioning failure degrades to nothing — never fatal to the recipe.
pub fn resolve_or_provision_winetricks(emit: &mut dyn FnMut(&ProgressEvent)) -> Option<PathBuf> {
    if let Some(p) = resolve_winetricks() {
        return Some(p);
    }
    let entry = catalog::find(WINETRICKS_ID)?;
    let Ok(http) = catalog::http_client() else {
        return None;
    };
    let outcome = lutris::block_on_detached(store::install(
        entry,
        None,
        &http,
        catalog::GITHUB_API,
        emit,
    ));
    match outcome {
        Ok(m) => {
            let p = store::tree_root(&m).join(&m.entry_rel);
            p.is_file().then_some(p)
        }
        Err(e) => {
            emit(&ProgressEvent::Warn {
                message: format!("winetricks auto-provision failed ({e}); skipping verbs"),
            });
            None
        }
    }
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
}
