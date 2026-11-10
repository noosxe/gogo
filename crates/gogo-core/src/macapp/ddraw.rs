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
    if has_dll(game_dir) {
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

/// The dir a game's `ddraw.dll` counts in: the entry exe's, since that's
/// where Wine's loader looks (#199; SimCity 3000 keeps its DDrawCompat in
/// `Apps/`, beside `Apps/SC3U.exe`). `root` when the exe is unknown, outside
/// the tree or in a dir that isn't there.
pub fn dir_for(root: &Path, exe: Option<&Path>) -> PathBuf {
    exe.and_then(Path::parent)
        .filter(|d| d.starts_with(root) && d.is_dir())
        .unwrap_or(root)
        .to_path_buf()
}

/// A `ddraw.dll` in `game_dir`, in any case (Wine's lookup ignores it, as
/// does `pe::scan`'s `local_overrides`).
pub fn has_dll(game_dir: &Path) -> bool {
    std::fs::read_dir(game_dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| e.file_name().eq_ignore_ascii_case("ddraw.dll") && e.path().is_file())
    })
}

/// Strings in every DDrawCompat release's own log lines. The file name says
/// nothing (it's always `ddraw.dll`), and the bare word turns up in other
/// wrappers' compatibility notes.
const DDRAWCOMPAT_MARKERS: [&[u8]; 2] =
    [b"DDrawCompat loaded successfully", b"Loading DDrawCompat "];

/// Is the game's `ddraw.dll` in `game_dir` DDrawCompat (`narzoul/DDrawCompat`)?
/// It patches Windows' own ddraw and doesn't run under any Wine: its README
/// rules Wine out, and v0.5.3 reads a null pointer during init under
/// GE-Proton11-12, ending the game with code 9 (#199). So gogo never loads
/// it: no `ddraw=n,b`, and the game runs on Wine's builtin ddraw.
pub fn is_ddrawcompat(game_dir: &Path) -> bool {
    if is_gogo(game_dir) {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(game_dir) else {
        return false;
    };
    entries
        .flatten()
        .filter(|e| e.file_name().eq_ignore_ascii_case("ddraw.dll"))
        .any(|e| is_ddrawcompat_file(&e.path()))
}

/// Is the file at `path` DDrawCompat, by its log strings, whatever its name?
pub fn is_ddrawcompat_file(path: &Path) -> bool {
    // DDrawCompat is a few MB; a big DLL beside the exe isn't it, and isn't read.
    if std::fs::metadata(path).map_or(true, |m| m.len() > 64 << 20) {
        return false;
    }
    std::fs::read(path).is_ok_and(|bytes| {
        DDRAWCOMPAT_MARKERS
            .iter()
            .any(|m| bytes.windows(m.len()).any(|w| w == *m))
    })
}

/// What the game's DDrawCompat is renamed to while gogo's cnc-ddraw stands in
/// under its name ([`apply_as`]): `xdd.dll` → `xdd.dll.ddrawcompat`. Wine never
/// loads it there, and [`remove_gogo`] puts it back.
pub const KEPT_SUFFIX: &str = ".ddrawcompat";

/// The DLL in `game_dir`, other than `ddraw.dll`, that the entry exe imports
/// (`imports`, lowercased) and that is DDrawCompat, or that gogo's cnc-ddraw
/// already stands in for (its original kept as `<name>`[`KEPT_SUFFIX`]). Its
/// file name as it is on disk.
///
/// GOG's Heroes of Might and Magic 3: Complete patches `Heroes3.exe` to import
/// `DirectDrawCreate` from `xdd.dll`, DDrawCompat renamed (#306). DDrawCompat
/// doesn't run under Wine (#199), and a static import can't be left unloaded
/// the way a `ddraw.dll` can: something has to answer under that name.
pub fn ddrawcompat_import(
    game_dir: &Path,
    imports: &std::collections::BTreeSet<String>,
) -> Option<String> {
    let names: Vec<String> = std::fs::read_dir(game_dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    let mut found: Vec<&String> = names
        .iter()
        .filter(|n| {
            let lower = n.to_ascii_lowercase();
            lower.ends_with(".dll") && lower != "ddraw.dll" && imports.contains(&lower)
        })
        .filter(|n| {
            let kept = format!("{n}{KEPT_SUFFIX}");
            names.iter().any(|k| k.eq_ignore_ascii_case(&kept))
                || is_ddrawcompat_file(&game_dir.join(n))
        })
        .collect();
    found.sort();
    found.first().map(|n| n.to_string())
}

/// [`apply`] under another name (#306): the game's DDrawCompat `name` is
/// renamed to `name`[`KEPT_SUFFIX`], gogo's cnc-ddraw is copied in as `name`,
/// and gogo's `ddraw.ini` (cnc-ddraw reads `ddraw.ini` beside itself, whatever
/// its own name) is written. No DLL override: `name` has no builtin. Already
/// done ([`is_gogo`] and the kept original there): [`Applied::AlreadyPresent`].
pub fn apply_as(store_tree: &Path, game_dir: &Path, name: &str) -> Result<Applied, MacAppError> {
    let target = game_dir.join(name);
    let kept = game_dir.join(format!("{name}{KEPT_SUFFIX}"));
    // In place unless GOG's DDrawCompat came back under the name (a repair).
    if kept.is_file() && target.is_file() && is_gogo(game_dir) && !is_ddrawcompat_file(&target) {
        return Ok(Applied::AlreadyPresent);
    }
    // gogo's ini beside a game's own ddraw.dll would make that DLL look like
    // gogo's, and `remove_gogo` would delete it.
    if has_dll(game_dir) {
        return Err(MacAppError::Recipe(format!(
            "{} has a ddraw.dll of its own: not placing cnc-ddraw as {name}",
            game_dir.display()
        )));
    }
    let dll = store_tree.join("ddraw.dll");
    if !dll.is_file() {
        return Err(MacAppError::Recipe(format!(
            "cnc-ddraw store tree has no ddraw.dll: {}",
            dll.display()
        )));
    }
    if !kept.is_file() {
        if !is_ddrawcompat_file(&target) {
            return Err(MacAppError::Recipe(format!(
                "{} isn't DDrawCompat: not replacing it",
                target.display()
            )));
        }
        std::fs::rename(&target, &kept)?;
    }
    std::fs::copy(&dll, &target)?;
    std::fs::write(game_dir.join("ddraw.ini"), DDRAW_INI)?;
    let mut shaders = 0;
    let src_shaders = store_tree.join("Shaders");
    if src_shaders.is_dir() {
        copy_dir(&src_shaders, &game_dir.join("Shaders"), &mut shaders)?;
    }
    Ok(Applied::Applied { shaders })
}

/// A `ddraw.dll` in `game_dir` that isn't the one [`apply`] put there.
pub fn is_games(game_dir: &Path) -> bool {
    has_dll(game_dir) && !is_gogo(game_dir)
}

/// Is the `ddraw.dll` in `game_dir` the one [`apply`] put there (its
/// `ddraw.ini` is gogo's)?
pub fn is_gogo(game_dir: &Path) -> bool {
    std::fs::read_to_string(game_dir.join("ddraw.ini")).is_ok_and(|s| s.starts_with("; gogo:"))
}

/// Undo [`apply`] when the game moves to another renderer: remove
/// `ddraw.dll` and `ddraw.ini` from `game_dir`, but only when the ini is
/// gogo's (a game's or recipe's own wrapper is never touched), and put back
/// a DDrawCompat [`apply_as`] set aside. `Shaders/` stays; without the dll
/// nothing loads it. Returns whether anything went.
pub fn remove_gogo(game_dir: &Path) -> bool {
    let ini = game_dir.join("ddraw.ini");
    if !is_gogo(game_dir) {
        return false;
    }
    let _ = std::fs::remove_file(game_dir.join("ddraw.dll"));
    // A stand-in for the game's DDrawCompat ([`apply_as`]): the original back.
    for e in std::fs::read_dir(game_dir).into_iter().flatten().flatten() {
        let Some(name) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(orig) = name.strip_suffix(KEPT_SUFFIX) else {
            continue;
        };
        if is_ddrawcompat_file(&e.path()) {
            let _ = std::fs::rename(e.path(), game_dir.join(orig));
        }
    }
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

/// #306, on both OSes: Linux setup runs these.
#[cfg(test)]
mod stand_in_tests {
    use super::*;

    fn tree(tmp: &Path) -> std::path::PathBuf {
        let t = tmp.join("store");
        std::fs::create_dir_all(t.join("Shaders/xbr")).unwrap();
        std::fs::write(t.join("ddraw.dll"), b"MZ-dll").unwrap();
        std::fs::write(t.join("Shaders/xbr/one.fxc"), b"shader").unwrap();
        t
    }

    /// A DDrawCompat under another name, imported by the exe (#306).
    fn xdd_game(tmp: &Path) -> std::path::PathBuf {
        let game = tmp.join("game");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(game.join("xdd.dll"), b"MZ..Loading DDrawCompat from..").unwrap();
        std::fs::write(game.join("BINKW32.DLL"), b"MZ-bink").unwrap();
        game
    }

    fn imports(names: &[&str]) -> std::collections::BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn ddrawcompat_import_finds_a_renamed_one_only_when_imported() {
        let tmp = tempfile::tempdir().unwrap();
        let game = xdd_game(tmp.path());
        let both = imports(&["xdd.dll", "binkw32.dll", "kernel32.dll"]);
        assert_eq!(ddrawcompat_import(&game, &both).as_deref(), Some("xdd.dll"));
        assert_eq!(ddrawcompat_import(&game, &imports(&["binkw32.dll"])), None);
        // A DDrawCompat named ddraw.dll is #199's case, not this one.
        std::fs::rename(game.join("xdd.dll"), game.join("ddraw.dll")).unwrap();
        assert_eq!(ddrawcompat_import(&game, &imports(&["ddraw.dll"])), None);
    }

    #[test]
    fn apply_as_stands_in_and_remove_gogo_puts_ddrawcompat_back() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tree(tmp.path());
        let game = xdd_game(tmp.path());
        let orig = std::fs::read(game.join("xdd.dll")).unwrap();
        assert!(matches!(
            apply_as(&t, &game, "xdd.dll").unwrap(),
            Applied::Applied { shaders: 1 }
        ));
        assert_eq!(std::fs::read(game.join("xdd.dll")).unwrap(), b"MZ-dll");
        assert_eq!(
            std::fs::read(game.join("xdd.dll.ddrawcompat")).unwrap(),
            orig
        );
        assert!(is_gogo(&game) && !has_dll(&game) && !is_ddrawcompat(&game));
        // Applied: still found (by the kept original), and a second apply is a no-op.
        assert_eq!(
            ddrawcompat_import(&game, &imports(&["xdd.dll"])).as_deref(),
            Some("xdd.dll")
        );
        assert!(matches!(
            apply_as(&t, &game, "xdd.dll").unwrap(),
            Applied::AlreadyPresent
        ));
        assert!(remove_gogo(&game));
        assert_eq!(std::fs::read(game.join("xdd.dll")).unwrap(), orig);
        assert!(!game.join("xdd.dll.ddrawcompat").exists() && !game.join("ddraw.ini").exists());
    }

    #[test]
    fn apply_as_places_again_after_a_repair_and_never_beside_a_ddraw_dll() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tree(tmp.path());
        let game = xdd_game(tmp.path());
        let orig = std::fs::read(game.join("xdd.dll")).unwrap();
        apply_as(&t, &game, "xdd.dll").unwrap();
        // GOG's repair puts DDrawCompat back under the name.
        std::fs::write(game.join("xdd.dll"), &orig).unwrap();
        assert!(matches!(
            apply_as(&t, &game, "xdd.dll").unwrap(),
            Applied::Applied { .. }
        ));
        assert_eq!(std::fs::read(game.join("xdd.dll")).unwrap(), b"MZ-dll");
        assert_eq!(
            std::fs::read(game.join("xdd.dll.ddrawcompat")).unwrap(),
            orig
        );

        let game = xdd_game(&tmp.path().join("other"));
        std::fs::write(game.join("ddraw.dll"), b"own").unwrap();
        assert!(apply_as(&t, &game, "xdd.dll").is_err());
        assert_eq!(std::fs::read(game.join("xdd.dll")).unwrap(), orig);
        assert!(!game.join("ddraw.ini").exists());
    }

    #[test]
    fn apply_as_never_replaces_what_isnt_ddrawcompat() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tree(tmp.path());
        let game = xdd_game(tmp.path());
        assert!(apply_as(&t, &game, "BINKW32.DLL").is_err());
        assert_eq!(std::fs::read(game.join("BINKW32.DLL")).unwrap(), b"MZ-bink");
        assert!(!game.join("ddraw.ini").exists());
    }
}

#[cfg(all(test, target_os = "macos"))]
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
