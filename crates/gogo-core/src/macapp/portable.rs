//! `gogo mac-app --portable` (#84, docs/19 §6.1, §6.5): a bundle to hand to
//! another Mac. It carries the engine (the derived runtime for DXMT), its
//! `Frameworks/` dylibs, the game tree, and the files the game's winetricks
//! verbs installed; its `gogo-launch.toml` is the effective config as the
//! build resolved it (flags > L4 > L3 > L2 > L1), with a `[portable]` table.
//!
//! The prefix still lives outside the bundle, in the state dir of the Mac it
//! runs on (`~/Library/Application Support/gogo/games/<gogid>/`), and the
//! launcher creates it there on first launch: `wineboot -u` with the bundled
//! Wine, then the verb files and the baked registry values. That Mac may
//! have no gogo, so nothing in it runs winetricks: the build runs the verbs
//! once in a staging prefix next to the bundle and keeps what they added.
//!
//! What a portable bundle gives up: it can't be retargeted in place
//! (`--retarget`, "Try next renderer"), since its runtime and renderer are
//! inside the signed bundle. A rebuild changes them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::engine::events::ProgressEvent;

use super::bundle::{self, CodesignPolicy};
use super::compat::{self, Layer};
use super::error::MacAppError;
use super::launch::{self, GameDirs, GogoRef, LaunchToml, Portable, RegValue};
use super::renderer::{self, Renderer, RendererRef};
use super::runtime::{Runtime, RuntimeMode};
use super::{MacAppPlan, catalog, installer, setup, verbs};

/// Under `Contents/Resources`.
const WINE_DIR: &str = "wine";
const FRAMEWORKS_DIR: &str = "wine-frameworks";
const GAME_DIR: &str = "game";
const WINDOWS_DIR: &str = "verbs";
/// The `drive_c/windows` dirs whose new files a verb run leaves in the
/// bundle: where winetricks puts DLLs and fonts.
const CAPTURED: [&str; 3] = ["system32", "syswow64", "Fonts"];

/// Why `--renderer d3dmetal --portable` is refused (#71, docs/19 §3.2).
pub const D3DMETAL: &str = "D3DMetal can't go into a portable bundle: it is Apple software \
     under the Game Porting Toolkit licence, which gogo downloads only for your own runtime \
     store, after your consent, and never redistributes inside bundles; pick another \
     `--renderer` (dxmt, wined3d)";

/// Can `rt` go inside a bundle someone else gets? Not Apple's GPTK and not
/// a catalog runtime whose download is consent-gated: gogo promised never to
/// redistribute those. #82's vendor-signed runtimes belong here too, once
/// they exist: an entitlement-signed engine can't be copied into another
/// signed bundle either.
pub(super) fn check_runtime(rt: &Runtime) -> Result<(), MacAppError> {
    let gated = match &rt.mode {
        RuntimeMode::Gptk => true,
        _ => {
            let r = launch::runtime_ref(rt);
            r.split_once('/')
                .filter(|_| !r.starts_with("path:"))
                .and_then(|(id, _)| catalog::find(id))
                .is_some_and(|e| matches!(e.status, catalog::Status::ConsentRequired))
        }
    };
    if gated {
        return Err(MacAppError::Portable(format!(
            "runtime {} is consent-gated (Apple's Game Porting Toolkit licence): gogo \
             doesn't redistribute it inside bundles; pick another `--runtime` (winecx)",
            rt.version
        )));
    }
    Ok(())
}

/// `games/<gogid>/portable`, which a portable bundle's launcher writes once
/// it set the prefix up: the stamp it applied and the bundle it ran from.
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub stamp: String,
    pub bundle: PathBuf,
}

/// The marker for `gogid` on this Mac, if a portable bundle set it up.
pub fn marker(gogid: &str) -> Option<Marker> {
    let dirs = GameDirs::for_game(gogid).ok()?;
    let raw = std::fs::read_to_string(dirs.root.join("portable")).ok()?;
    let field = |k: &str| {
        raw.lines()
            .find_map(|l| l.strip_prefix(k)?.strip_prefix('='))
            .map(str::to_string)
    };
    Some(Marker {
        stamp: field("stamp")?,
        bundle: PathBuf::from(field("bundle")?),
    })
}

/// The portable bundle `target` (a `.app` or a gogid) names: a bundle whose
/// TOML has `[portable]`, or a gogid that only a portable bundle set up
/// here (no gogo `state.toml`, a launcher marker).
pub fn portable_target(target: &str) -> Option<PathBuf> {
    if target.bytes().all(|b| b.is_ascii_digit()) && !target.is_empty() {
        let dirs = GameDirs::for_game(target).ok()?;
        if dirs.state_toml.is_file() {
            return None;
        }
        return marker(target).map(|m| m.bundle);
    }
    let toml = Path::new(target).join("Contents/Resources/gogo-launch.toml");
    let raw = std::fs::read_to_string(&toml).ok()?;
    LaunchToml::parse(&raw, &toml)
        .ok()?
        .portable
        .map(|_| launch::absolute(Path::new(target)))
}

/// Refuse `what` (`--retarget`, `next-renderer`, `setup`) on a portable
/// bundle, saying why and what to do instead.
pub fn refuse(target: &str, what: &str) -> Result<(), MacAppError> {
    match portable_target(target) {
        Some(bundle) => Err(MacAppError::Portable(format!(
            "{} is a portable bundle: its runtime and renderer are inside the signed bundle \
             and its launcher sets the prefix up itself, so `{what}` can't change it in \
             place; rebuild it instead (`gogo mac-app <game> --portable --runtime … \
             --renderer …`)",
            bundle.display()
        ))),
        None => Ok(()),
    }
}

/// The portable tail of `execute`: resolve the effective config (L4 and L3
/// included, flags winning), put the engine, the renderer, the verb files
/// and the game into the bundle, write the TOML with `[portable]`, sign and
/// verify. `toml` is what a v2 build would bake; `dirs` is the staging dir
/// the build ran in (removed by the caller).
#[allow(clippy::too_many_arguments)]
pub(super) fn finish(
    plan: &MacAppPlan,
    dirs: &GameDirs,
    install_dir: &Path,
    toml: LaunchToml,
    selection: &renderer::Selection,
    winetricks_used: bool,
    codesign: CodesignPolicy,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<u64, MacAppError> {
    let gogid = toml.gogid.clone();
    let res = &plan.bundle.resources;
    installer::stop_wineserver(&plan.runtime, &dirs.prefix)?;
    if plan.installer.is_some() || plan.recipe.is_some() {
        emit(&ProgressEvent::Warn {
            message: "portable: only the game dir, the verbs' files and the registry values \
                      gogo knows of go into the bundle; what the installer or a recipe wrote \
                      elsewhere in the prefix doesn't (the depot lane fits --portable best)"
                .into(),
        });
    }

    // The effective config, as setup would resolve it, then flags > L4
    // (#110): nothing runs setup later.
    let db = compat::db_entry(&gogid);
    let l4 = match compat::load_user(&gogid) {
        Ok(p) => p,
        Err(e) => {
            emit(&ProgressEvent::Warn {
                message: format!("{e}: ignoring it"),
            });
            compat::Parsed::default()
        }
    };
    let mut eff = toml
        .effective(&db.entry, &l4.entry)
        .map_err(|reason| MacAppError::Compat {
            path: compat::user_path(&gogid),
            reason,
        })?;
    if selection.renderer.layer == Layer::Flags {
        eff.renderer = toml.renderer.clone();
    }
    if plan.runtime_flag {
        eff.runtime = toml.runtime.clone();
    }
    if l4.entry != compat::CompatEntry::default() {
        emit(&ProgressEvent::Warn {
            message: format!(
                "your override file {} is baked into the portable bundle",
                compat::user_path(&gogid).display()
            ),
        });
    }
    let rt = if eff.runtime == toml.runtime {
        plan.runtime.clone()
    } else {
        setup::resolve_or_install(&eff.runtime)?.0
    };
    check_runtime(&rt)?;

    // The renderer: an engine overlay (DXMT) is the derived engine that gets
    // copied below, cnc-ddraw goes into the game dir. Never D3DMetal.
    if RendererRef::parse(&eff.renderer).is_ok_and(|r| r.renderer == Renderer::D3dMetal) {
        let fallback = match selection.l1.renderer {
            Renderer::D3dMetal => Renderer::Wined3d,
            r => r,
        };
        emit(&ProgressEvent::Warn {
            message: format!(
                "D3DMetal never goes into a portable bundle (Apple's licence): using {}",
                fallback.id()
            ),
        });
        eff.renderer = fallback.default_ref();
    }
    let applied = renderer::apply(&eff.renderer, rt, Some(install_dir));
    for message in applied.warnings {
        emit(&ProgressEvent::Warn { message });
    }
    for verb in applied.notes {
        emit(&ProgressEvent::RecipeStep { verb });
    }
    let rt = applied.runtime;
    for (k, v) in applied.dll_overrides {
        eff.dll_overrides.entry(k).or_insert(v);
    }
    eff.renderer = applied.renderer;

    // Verbs: run once here, keep what they added (files, DLL overrides).
    eff.verbs = verbs::effective(&toml.verbs, &db.entry, &l4.entry)
        .into_iter()
        .map(|s| s.value)
        .collect();
    let mut captured = 0;
    if !eff.verbs.is_empty() {
        let windows = dirs.prefix.join("drive_c/windows");
        let before = snapshot(&windows);
        let overrides = dll_overrides(&dirs.prefix);
        let mut warnings = Vec::new();
        let ran = setup::run_verbs(&rt, &dirs.prefix, &eff.verbs, &mut warnings);
        for message in warnings {
            emit(&ProgressEvent::Warn { message });
        }
        ran?;
        setup::wait_quiet(&rt, &dirs.prefix);
        captured = copy_changed(&windows, &before, &res.join(WINDOWS_DIR))?;
        for (k, v) in dll_overrides(&dirs.prefix) {
            if overrides.get(&k) != Some(&v) {
                eff.dll_overrides.entry(k).or_insert(v);
            }
        }
        emit(&ProgressEvent::RecipeStep {
            verb: format!(
                "verbs {}: {captured} file(s) and their DLL overrides go into the bundle \
                 (other registry changes they make are not carried)",
                eff.verbs.join(", ")
            ),
        });
    }

    // Registry values for the new prefix: the GOG.com key GOG's installers
    // write (L2), then L3 and L4's.
    let tree = eff.tree_name().ok_or_else(|| {
        MacAppError::Portable(format!(
            "the entry point {} is not under C:\\GOG\\<game>: nothing to copy",
            eff.exe
        ))
    })?;
    let exe_file = eff.exe.rsplit('\\').next().unwrap_or_default().to_string();
    let mut registry = super::gog_game_key(&gogid, &tree, &eff.game_dir(), &exe_file);
    registry.extend(
        compat::merge(&[(Layer::Db, &db.entry), (Layer::User, &l4.entry)])
            .registry
            .map(|r| r.value)
            .unwrap_or_default()
            .iter()
            .map(RegValue::from),
    );

    // Into the bundle: engine, dylibs, game.
    emit(&ProgressEvent::Warn {
        message: format!(
            "copying runtime {} and the game into the bundle",
            launch::runtime_ref(&rt)
        ),
    });
    let (wine, frameworks) = copy_engine(&rt, res)?;
    let game = format!("{GAME_DIR}/{tree}");
    bundle::clone_tree(&install_dir.canonicalize()?, &res.join(&game))?;

    eff.runtime = match launch::runtime_ref(&rt) {
        // A runtime outside the store: its version, not the builder's path.
        r if r.starts_with("path:") => rt.version.clone(),
        r => r,
    };
    eff.portable = Some(Portable {
        wine,
        frameworks,
        game,
        windows: (captured > 0).then(|| WINDOWS_DIR.to_string()),
        registry,
    });
    // Nothing points back at the builder's Mac.
    eff.gogo = GogoRef {
        path: PathBuf::from("gogo"),
        state: PathBuf::from("~/Library/Application Support/gogo"),
    };
    launch::write_bundle_config(res, &eff)?;
    launch::write_launcher(&plan.bundle.macos, &plan.display)?;
    bundle::write_info_plist(
        &plan.bundle,
        &plan.display,
        &plan.slug,
        plan.runtime.min_macos,
    )?;
    bundle::write_notices(&plan.bundle, &rt)?;
    bundle::write_bundled_renderer_notice(&plan.bundle, &eff.renderer)?;
    if winetricks_used || !eff.verbs.is_empty() {
        bundle::write_winetricks_notice(&plan.bundle)?;
    }
    if captured > 0 {
        bundle::write_verbs_notice(&plan.bundle, &eff.verbs)?;
    }
    emit(&ProgressEvent::BundleAssembled);

    bundle::strip_detritus(&plan.bundle.root, codesign)?;
    bundle::codesign_and_verify(&plan.bundle.root, codesign, true)?;
    emit(&ProgressEvent::Codesigned);
    super::dir_size(&plan.bundle.root)
}

/// The engine into `Resources/wine` (symlinks kept: the CX engine's dylib
/// links, D3DMetal's framework) and the dylibs it loads into
/// `Resources/wine-frameworks`. Returns the TOML's `wine` and `frameworks`.
fn copy_engine(rt: &Runtime, res: &Path) -> Result<(String, Option<String>), MacAppError> {
    let engine = launch::engine_root(rt).canonicalize()?;
    bundle::clone_tree(&engine, &res.join(WINE_DIR))?;
    // A link out of the bundle fails `codesign --strict` and points at
    // nothing on another Mac.
    drop_absolute_links(&res.join(WINE_DIR))?;
    let name = rt
        .wine
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "wine".to_string());
    let wine = format!("{WINE_DIR}/bin/{name}");
    if !res.join(&wine).is_file() {
        return Err(MacAppError::RuntimeInvalid {
            path: res.join(WINE_DIR),
            reason: format!("{wine} missing after the engine copy"),
        });
    }
    let frameworks = match launch::frameworks_of(rt) {
        Some(dir) => {
            copy_dylibs(&dir.canonicalize()?, &res.join(FRAMEWORKS_DIR))?;
            Some(FRAMEWORKS_DIR.to_string())
        }
        None => None,
    };
    Ok((wine, frameworks))
}

/// The top-level `*.dylib`s of `src` (a store `Frameworks/`, or a wrapper's
/// `Contents/Frameworks`, whose `*.framework`s Wine doesn't load), with
/// relative links kept as links.
fn copy_dylibs(src: &Path, dst: &Path) -> Result<(), MacAppError> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)?.flatten() {
        let name = e.file_name();
        if !name.to_string_lossy().contains(".dylib") {
            continue;
        }
        let from = e.path();
        let to = dst.join(&name);
        match std::fs::read_link(&from) {
            #[cfg(unix)]
            Ok(target) if target.is_relative() => std::os::unix::fs::symlink(&target, &to)?,
            _ if from.is_file() => {
                std::fs::copy(&from, &to)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Remove every symlink under `dir` whose target is absolute.
fn drop_absolute_links(dir: &Path) -> Result<(), MacAppError> {
    for e in std::fs::read_dir(dir)?.flatten() {
        let p = e.path();
        let ty = e.file_type()?;
        if ty.is_symlink() {
            if std::fs::read_link(&p)?.is_absolute() {
                std::fs::remove_file(&p)?;
            }
        } else if ty.is_dir() {
            drop_absolute_links(&p)?;
        }
    }
    Ok(())
}

type Snapshot = BTreeMap<PathBuf, (u64, Option<SystemTime>)>;

/// Every regular file under the [`CAPTURED`] dirs of `windows`, by path
/// relative to it, with its size and mtime.
fn snapshot(windows: &Path) -> Snapshot {
    fn walk(root: &Path, dir: &Path, out: &mut Snapshot) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            let Ok(ty) = e.file_type() else { continue };
            if ty.is_dir() {
                walk(root, &p, out);
            } else if ty.is_file()
                && let Ok(meta) = e.metadata()
            {
                let rel = p.strip_prefix(root).unwrap_or(&p).to_path_buf();
                out.insert(rel, (meta.len(), meta.modified().ok()));
            }
        }
    }
    let mut out = Snapshot::new();
    for d in CAPTURED {
        walk(windows, &windows.join(d), &mut out);
    }
    out
}

/// Copy the files of `windows` that are new or changed since `before` to
/// `dst`, same relative layout. Returns how many.
fn copy_changed(windows: &Path, before: &Snapshot, dst: &Path) -> Result<usize, MacAppError> {
    let mut n = 0;
    for (rel, meta) in snapshot(windows) {
        if before.get(&rel) == Some(&meta) {
            continue;
        }
        let to = dst.join(&rel);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(windows.join(&rel), &to)?;
        n += 1;
    }
    Ok(n)
}

/// `HKCU\Software\Wine\DllOverrides` from a prefix's `user.reg` (what
/// winetricks sets for the DLLs it installs), keyed by DLL.
fn dll_overrides(prefix: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(raw) = std::fs::read_to_string(prefix.join("user.reg")) else {
        return out;
    };
    let mut inside = false;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix('[') {
            inside = rest
                .split(']')
                .next()
                .is_some_and(|k| k.eq_ignore_ascii_case(r"Software\\Wine\\DllOverrides"));
            continue;
        }
        if !inside {
            continue;
        }
        if let Some((k, v)) = line.split_once("\"=\"") {
            let k = k.trim_start_matches('"').trim_start_matches('*');
            let v = v.strip_suffix('"').unwrap_or(v);
            if !k.is_empty() {
                out.insert(k.to_ascii_lowercase(), v.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod portable_tests {
    use super::*;

    #[test]
    fn dll_overrides_come_from_the_user_reg_section() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("user.reg"),
            "WINE REGISTRY Version 2\n\n\
             [Software\\\\Wine\\\\DllOverrides] 1729000000\n\
             #time=1db\n\
             \"*d3dx9_43\"=\"native,builtin\"\n\
             \"D3DCompiler_43\"=\"native\"\n\n\
             [Software\\\\Wine\\\\Other] 1729000000\n\
             \"x\"=\"y\"\n",
        )
        .unwrap();
        let ov = dll_overrides(tmp.path());
        assert_eq!(
            ov.into_iter().collect::<Vec<_>>(),
            [
                ("d3dcompiler_43".to_string(), "native".to_string()),
                ("d3dx9_43".to_string(), "native,builtin".to_string()),
            ]
        );
        assert!(dll_overrides(&tmp.path().join("nope")).is_empty());
    }

    #[test]
    fn only_new_or_changed_files_under_the_captured_dirs_are_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let w = tmp.path().join("windows");
        for d in ["system32", "syswow64", "Fonts", "temp"] {
            std::fs::create_dir_all(w.join(d)).unwrap();
        }
        std::fs::write(w.join("system32/kernel32.dll"), b"fake").unwrap();
        std::fs::write(w.join("syswow64/d3d9.dll"), b"old").unwrap();
        let before = snapshot(&w);
        std::fs::write(w.join("syswow64/d3dx9_43.dll"), b"new").unwrap();
        std::fs::write(w.join("syswow64/d3d9.dll"), b"newer").unwrap();
        std::fs::write(w.join("Fonts/arial.ttf"), b"font").unwrap();
        std::fs::write(w.join("temp/junk.log"), b"x").unwrap();
        let dst = tmp.path().join("verbs");
        assert_eq!(copy_changed(&w, &before, &dst).unwrap(), 3);
        assert!(dst.join("syswow64/d3dx9_43.dll").is_file());
        assert!(dst.join("syswow64/d3d9.dll").is_file());
        assert!(dst.join("Fonts/arial.ttf").is_file());
        assert!(!dst.join("system32/kernel32.dll").exists());
        assert!(!dst.join("temp").exists());
    }

    #[test]
    fn engine_copy_keeps_relative_links_and_dylibs_only() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = tmp.path().join("store/winecx/24");
        let bin = entry.join("engine/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(entry.join("engine/lib/wine")).unwrap();
        std::fs::write(bin.join("wine"), b"wine").unwrap();
        std::os::unix::fs::symlink("wine", bin.join("wine64")).unwrap();
        std::os::unix::fs::symlink("/nowhere", entry.join("engine/lib/abs")).unwrap();
        let fw = entry.join("Frameworks");
        std::fs::create_dir_all(fw.join("GStreamer.framework")).unwrap();
        std::fs::write(fw.join("libinotify.0.dylib"), b"dylib").unwrap();
        std::os::unix::fs::symlink("libinotify.0.dylib", fw.join("libinotify.dylib")).unwrap();
        let rt = Runtime {
            mode: RuntimeMode::Path,
            kind: "path",
            root: entry.clone(),
            wine: bin.join("wine"),
            version: "wine-9.0".into(),
            min_macos: None,
        };
        let res = tmp.path().join("Resources");
        let (wine, frameworks) = copy_engine(&rt, &res).unwrap();
        assert_eq!(wine, "wine/bin/wine");
        assert_eq!(frameworks.as_deref(), Some("wine-frameworks"));
        assert_eq!(
            std::fs::read_link(res.join("wine/bin/wine64")).unwrap(),
            Path::new("wine")
        );
        assert!(res.join("wine/lib/wine").is_dir());
        assert!(res.join("wine/lib/abs").symlink_metadata().is_err());
        assert!(res.join("wine-frameworks/libinotify.0.dylib").is_file());
        assert_eq!(
            std::fs::read_link(res.join("wine-frameworks/libinotify.dylib")).unwrap(),
            Path::new("libinotify.0.dylib")
        );
        assert!(!res.join("wine-frameworks/GStreamer.framework").exists());
    }

    #[test]
    fn reg_values_become_reg_add_argv() {
        let v = RegValue {
            key: r"HKLM\Software\GOG.com\Games\1".into(),
            name: Some("PATH".into()),
            kind: "REG_SZ".into(),
            data: r"C:\GOG\Game".into(),
            wow32: true,
        };
        assert_eq!(
            v.argv(),
            [
                "reg",
                "add",
                r"HKLM\Software\GOG.com\Games\1",
                "/v",
                "PATH",
                "/t",
                "REG_SZ",
                "/d",
                r"C:\GOG\Game",
                "/f",
                "/reg:32"
            ]
        );
        let d = RegValue::from(&compat::RegistryValue {
            key: r"HKCU\Software\Game".into(),
            name: None,
            kind: None,
            data: "x".into(),
        });
        assert_eq!(d.argv()[3], "/ve");
        assert_eq!(d.kind, "REG_SZ");
    }
}
