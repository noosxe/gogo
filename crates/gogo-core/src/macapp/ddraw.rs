//! Native cnc-ddraw application (design 13 D2–D4, docs/16 S1).
//!
//! DirectDraw-era GOG games black-screen or instant-exit under wine's mac
//! display driver; the community cure — the cnc-ddraw wrapper (FunkyFr3sh,
//! MIT) dropped into the game dir — is live-proven (2026-10-19, pharaoh:
//! zero `ChangeDisplaySettings` failures, audio engine live). gogo applies
//! it natively instead of relying on Lutris CD-recipes whose paths target
//! the CD layout, not our `drive_c/GOG/<slug>` installs.

use crate::macapp::error::MacAppError;
use std::path::{Path, PathBuf};

pub const CNC_DDRAW_ID: &str = "cnc-ddraw";

/// The live-proven ini (design 13 D3): fullscreen with maintained aspect
/// ratio inside a window — renders everywhere the mac driver cannot match
/// legacy exclusive modes.
pub const DDRAW_INI: &str = "; gogo: DirectDraw wrapper config (design 13 D3)\n\
     ; renderer=gdi is pinned for determinism (cnc-ddraw's GL path is not\n\
     ; needed for classic GOG titles); flip to `auto` for shader scaling.\n\
     ; generated — edit freely; see https://github.com/FunkyFr3sh/cnc-ddraw\n\
     [ddraw]\n\
     renderer=gdi\n\
     fullscreen=true\n\
     maintas=true\n\
     windowed=true\n";

/// Resolve from the store, provisioning on demand (winetricks-style):
/// cached tree → download+extract via the catalog entry → tree containing
/// `ddraw.dll`.
pub fn resolve_or_provision(
    emit: &mut dyn FnMut(&crate::macapp::ProgressEvent),
) -> Option<PathBuf> {
    // Power-user / test override (WINETRICKS-style): a tree that already
    // contains ddraw.dll skips the store entirely.
    if let Some(dir) = std::env::var_os("GOGO_CNC_DDRAW_DIR") {
        let dir = PathBuf::from(dir);
        if dir.join("ddraw.dll").is_file() {
            return Some(dir);
        }
    }
    let entry = crate::macapp::catalog::find(CNC_DDRAW_ID)?;
    if let Some((m, root)) = crate::macapp::store::installed(CNC_DDRAW_ID, None)
        && root.join(&m.entry_rel).is_file()
    {
        return Some(root);
    }
    let Ok(http) = crate::macapp::catalog::http_client() else {
        return None;
    };
    let outcome = crate::macapp::lutris::block_on_detached(crate::macapp::store::install(
        entry,
        None,
        &http,
        crate::macapp::catalog::GITHUB_API,
        false,
        emit,
    ));
    outcome.ok().and_then(|m| {
        let root = crate::macapp::store::tree_root(&m);
        root.join(&m.entry_rel).is_file().then_some(root)
    })
}

/// Outcome of [`apply`] — the caller reports it (D5 events).
#[derive(Debug)]
pub enum Applied {
    /// `ddraw.dll` (+ `Shaders/`) and the gogo ini now sit in the game dir.
    Applied { shaders: usize },
    /// A `ddraw.dll` was already present (recipe-placed) — never overwritten
    /// (D4).
    AlreadyPresent,
}

/// Copy `ddraw.dll` + `Shaders/` from the provisioned store tree into the
/// game dir and write the gogo-authored `ddraw.ini` (D3). Game-dir placement
/// only: the PE loader finds the local dll before any builtin (live-proven),
/// so no registry dll-overrides are touched.
pub fn apply(store_tree: &Path, game_dir: &Path) -> Result<Applied, MacAppError> {
    if game_dir.join("ddraw.dll").exists() {
        return Ok(Applied::AlreadyPresent);
    }
    let dll = store_tree.join("ddraw.dll");
    if !dll.is_file() {
        return Err(MacAppError::Recipe(format!(
            "cnc-ddraw store tree has no ddraw.dll: {}",
            dll.display()
        )));
    }
    std::fs::copy(&dll, game_dir.join("ddraw.dll"))?;
    std::fs::write(game_dir.join("ddraw.ini"), DDRAW_INI)?;
    let mut shaders = 0;
    let src_shaders = store_tree.join("Shaders");
    if src_shaders.is_dir() {
        copy_dir(&src_shaders, &game_dir.join("Shaders"), &mut shaders)?;
    }
    Ok(Applied::Applied { shaders })
}

/// Is the `ddraw.dll` in `game_dir` the one [`apply`] put there (its
/// `ddraw.ini` is gogo's)?
pub fn is_gogo(game_dir: &Path) -> bool {
    std::fs::read_to_string(game_dir.join("ddraw.ini")).is_ok_and(|s| s.starts_with("; gogo:"))
}

/// Undo [`apply`] when the game moves to another renderer: remove
/// `ddraw.dll` and `ddraw.ini` from `game_dir`, but only when the ini is
/// gogo's (a game's or recipe's own wrapper is never touched). `Shaders/`
/// stays; without the dll nothing loads it. Returns whether anything went.
pub fn remove_gogo(game_dir: &Path) -> bool {
    let ini = game_dir.join("ddraw.ini");
    if !is_gogo(game_dir) {
        return false;
    }
    let _ = std::fs::remove_file(game_dir.join("ddraw.dll"));
    std::fs::remove_file(&ini).is_ok()
}

/// Set the per-prefix DllOverrides entry `ddraw = n,b` (native, builtin)
/// with the bundled wine — the missing half of the wrapper (live finding
/// 2026-10-19: wine's default load order for draw is builtin-first, so the
/// game-dir dll is silently ignored; winetricks' cnc-ddraw verb and Lutris
/// `wine.overrides` both do exactly this).
pub fn register_override(bundled_wine: &Path, prefix: &Path) -> Result<(), MacAppError> {
    let argv = vec![
        "reg".to_string(),
        "add".to_string(),
        r"HKCU\Software\Wine\DllOverrides".to_string(),
        "/v".to_string(),
        "ddraw".to_string(),
        "/t".to_string(),
        "REG_SZ".to_string(),
        "/d".to_string(),
        "n,b".to_string(),
        "/f".to_string(),
    ];
    crate::macapp::interpret::run_in_prefix(
        bundled_wine,
        prefix,
        &argv,
        std::time::Duration::from_secs(60),
    )
    .map_err(|e| MacAppError::Recipe(format!("ddraw override: {e}")))
}

fn copy_dir(src: &Path, dst: &Path, count: &mut usize) -> Result<(), MacAppError> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &to, count)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
            *count += 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(tmp: &Path) -> std::path::PathBuf {
        let t = tmp.join("store");
        std::fs::create_dir_all(t.join("Shaders/xbr")).unwrap();
        std::fs::write(t.join("ddraw.dll"), b"MZ-dll").unwrap();
        std::fs::write(t.join("Shaders/xbr/one.fxc"), b"shader").unwrap();
        std::fs::write(t.join("cnc-ddraw config.exe"), b"cfg").unwrap();
        t
    }

    #[test]
    fn apply_copies_dll_ini_and_shaders() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tree(tmp.path());
        let game = tmp.path().join("game");
        std::fs::create_dir_all(&game).unwrap();
        match apply(&t, &game).unwrap() {
            Applied::Applied { shaders } => assert_eq!(shaders, 1),
            other => panic!("{other:?}"),
        }
        assert_eq!(std::fs::read(game.join("ddraw.dll")).unwrap(), b"MZ-dll");
        let ini = std::fs::read_to_string(game.join("ddraw.ini")).unwrap();
        assert!(
            ini.contains("renderer=gdi") && ini.contains("maintas=true") && ini.contains("[ddraw]")
        );
        assert!(game.join("Shaders/xbr/one.fxc").is_file());
        // Everything else in the store tree stays out of the game dir (D3).
        assert!(!game.join("cnc-ddraw config.exe").exists());
    }

    #[test]
    fn remove_gogo_only_removes_gogos_wrapper() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tree(tmp.path());
        let game = tmp.path().join("game");
        std::fs::create_dir_all(&game).unwrap();
        apply(&t, &game).unwrap();
        assert!(remove_gogo(&game));
        assert!(!game.join("ddraw.dll").exists() && !game.join("ddraw.ini").exists());
        assert!(!remove_gogo(&game));
        // A game's own wrapper (its own ini) stays.
        std::fs::write(game.join("ddraw.dll"), b"own").unwrap();
        std::fs::write(game.join("ddraw.ini"), b"[ddraw]\n").unwrap();
        assert!(!remove_gogo(&game));
        assert!(game.join("ddraw.dll").is_file());
    }

    #[test]
    fn apply_never_overwrites_an_existing_wrapper() {
        // D4: a recipe-placed ddraw.dll wins.
        let tmp = tempfile::tempdir().unwrap();
        let t = tree(tmp.path());
        let game = tmp.path().join("game");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(game.join("ddraw.dll"), b"recipe-dll").unwrap();
        assert!(matches!(apply(&t, &game).unwrap(), Applied::AlreadyPresent));
        assert_eq!(
            std::fs::read(game.join("ddraw.dll")).unwrap(),
            b"recipe-dll"
        );
        assert!(!game.join("ddraw.ini").exists());
    }
}
