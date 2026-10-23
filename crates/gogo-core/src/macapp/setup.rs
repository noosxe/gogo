//! `gogo mac-app setup <gogid>` (design 16 §2.2, #67): bring
//! `games/<gogid>/` up to date with the bundle's `gogo-launch.toml` and the
//! L4 override. Builds run it once at the end; the launcher runs it again
//! only when the config stamp is stale or the prefix is missing.
//!
//! The prefix is created by the user's own store runtime under their own
//! account (decision 2), and nothing in it points outside the state dir
//! (decision 4): Windows home folders are real dirs, and `dosdevices/` keeps
//! only `c:` (#55).

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::engine::events::ProgressEvent;

use super::compat::{self, Layer};
use super::error::MacAppError;
use super::installer;
use super::launch::{
    GameDirs, GameState, LaunchToml, SCHEMA, absolute, engine_root, frameworks_of, resolve_ref,
    stamp, state_rel, to_launch_conf, wineserver_of, write_atomic,
};
use super::runtime::{Runtime, wine_cmd};
use super::{catalog, lutris, store, tools, verbs};

/// What [`setup`] did.
#[derive(Debug, Clone)]
pub struct SetupOutcome {
    pub state: GameState,
    pub prefix_created: bool,
    /// Runtime `<id>/<ver>` installed into the store on the way.
    pub runtime_installed: Option<String>,
    pub warnings: Vec<String>,
}

/// Bring `games/<gogid>/` up to date: resolve the runtime (installing it
/// from the catalog when it's missing), create the prefix if it's missing,
/// keep it free of links out of the state dir, apply registry values and
/// winetricks verbs, wait until the prefix is quiet, then write the
/// effective `launch.conf` and `state.toml` with the new stamp. Never
/// touches the bundle; never deletes the prefix or anything in it but links.
///
/// `bundle` is the `.app` path when the caller knows it (the build, or the
/// launcher reporting where it runs from); otherwise `state.toml`'s.
pub fn setup(gogid: &str, bundle: Option<&Path>) -> Result<SetupOutcome, MacAppError> {
    let dirs = GameDirs::for_game(gogid)?;
    let prior = GameState::load(&dirs.state_toml)?;
    let bundle = match (bundle, &prior) {
        (Some(b), _) => absolute(b),
        (None, Some(s)) => s.bundle.clone(),
        (None, None) => {
            return Err(MacAppError::Compat {
                path: dirs.state_toml.clone(),
                reason: format!("no state for {gogid}: build the bundle first, or pass --bundle"),
            });
        }
    };
    let toml_path = bundle.join("Contents/Resources/gogo-launch.toml");
    let raw = std::fs::read(&toml_path).map_err(|e| MacAppError::Compat {
        path: toml_path.clone(),
        reason: e.to_string(),
    })?;
    let baked = LaunchToml::parse(&String::from_utf8_lossy(&raw), &toml_path)?;
    if baked.gogid != gogid {
        return Err(MacAppError::Compat {
            path: toml_path,
            reason: format!("bundle is for {}, not {gogid}", baked.gogid),
        });
    }

    let l4_path = compat::user_path(gogid);
    let l4_raw = match std::fs::read(&l4_path) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let l4 = compat::load_user(gogid)?;
    let mut warnings: Vec<String> = l4
        .warnings
        .iter()
        .map(|w| format!("{}: unknown key {w}", l4_path.display()))
        .collect();
    let mut eff = baked.with_override(&l4.entry);
    let (rt, runtime_installed) = resolve_or_install(&eff.runtime)?;

    let prefix_created = !dirs.prefix.is_dir();
    if prefix_created {
        create_prefix(&rt, &dirs.prefix)?;
    }
    if let Some(name) = eff.tree_name().filter(|_| dirs.game.is_dir())
        && dirs
            .prefix
            .join("drive_c/GOG")
            .join(&name)
            .symlink_metadata()
            .is_err()
    {
        link_game(&dirs, &name)?;
    }

    // Registry values the bundle TOML can't carry: L3, then L4 per field
    // (decision 5). Renderer files and the rest of L3 are #70/#74.
    let db = compat::db_entry(gogid);
    let registry = compat::merge(&[(Layer::Db, &db.entry), (Layer::User, &l4.entry)])
        .registry
        .map(|r| r.value)
        .unwrap_or_default();
    for value in &registry {
        super::interpret::run_in_prefix(
            &rt.wine,
            &dirs.prefix,
            &reg_add_argv(value),
            Duration::from_secs(60),
        )
        .map_err(|e| MacAppError::Recipe(format!("registry {}: {e}", value.key)))?;
    }
    // Verbs: the bundle's L1 list, then L3, then L4 (decision 5). winetricks
    // runs only when the list changed since the last setup or the prefix is
    // new; winetricks would skip installed verbs, but not for free.
    eff.verbs = verbs::effective(&baked.verbs, &db.entry, &l4.entry).value;
    let verbs_applied = !prefix_created
        && prior
            .as_ref()
            .is_some_and(|s| s.verbs == eff.verbs && s.runtime == eff.runtime);
    if !eff.verbs.is_empty() && !verbs_applied {
        run_verbs(&rt, &dirs.prefix, &eff.verbs, &mut warnings)?;
    }
    wait_quiet(&rt, &dirs.prefix);
    // Last: anything above (wineboot, a verb) may have linked out again.
    localize(&dirs.prefix)?;

    let state_root = crate::state::state_dir();
    let stamp = stamp(&raw, l4_raw.as_deref());
    let extra = [
        ("RUNTIME_DIR", state_rel(&state_root, &engine_root(&rt))),
        ("WINE", state_rel(&state_root, &rt.wine)),
        ("WINESERVER", state_rel(&state_root, &wineserver_of(&rt))),
        (
            "FRAMEWORKS",
            frameworks_of(&rt)
                .map(|f| state_rel(&state_root, &f))
                .unwrap_or_default(),
        ),
        ("PREFIX", state_rel(&state_root, &dirs.prefix)),
        ("STAMP", stamp.clone()),
    ];
    write_atomic(&dirs.launch_conf, &to_launch_conf(&eff, &extra))?;
    let state = GameState {
        schema: SCHEMA,
        gogid: gogid.to_string(),
        bundle,
        runtime: eff.runtime.clone(),
        renderer: eff.renderer.clone(),
        verbs: eff.verbs.clone(),
        stamp,
        gogo_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    write_atomic(
        &dirs.state_toml,
        &toml::to_string(&state).expect("GameState always serializes"),
    )?;
    Ok(SetupOutcome {
        state,
        prefix_created,
        runtime_installed,
        warnings,
    })
}

/// Resolve a runtime reference, installing a missing catalog runtime into
/// the store (design 16 §2.2, network allowed under design 12 D8). Anything
/// that can't be installed this way (a `path:` runtime, a consent download)
/// fails with how to install it by hand.
fn resolve_or_install(r: &str) -> Result<(Runtime, Option<String>), MacAppError> {
    let err = match resolve_ref(r) {
        Ok(rt) => return Ok((rt, None)),
        Err(e @ MacAppError::RuntimeNotFound { .. }) if !r.starts_with("path:") => e,
        Err(e) => return Err(e),
    };
    let (id, ver) = r.split_once('/').map_or((r, None), |(i, v)| (i, Some(v)));
    let Some(entry) = catalog::find(id).filter(|e| matches!(e.kind, catalog::Kind::Wine)) else {
        return Err(err);
    };
    let hint = |e: MacAppError| MacAppError::RuntimeNotFound {
        tried: format!(
            "runtime `{r}` is not in the store and could not be installed ({e}); \
             install it with `gogo runtimes install {id}{}`",
            ver.map(|v| format!(" --version {v}")).unwrap_or_default()
        ),
    };
    let http = catalog::http_client().map_err(hint)?;
    let m = lutris::block_on_detached(store::install(
        entry,
        ver,
        &http,
        catalog::GITHUB_API,
        false,
        &mut |_| {},
    ))
    .map_err(hint)?;
    let rt = resolve_ref(&format!("{}/{}", m.id, m.version))?;
    Ok((rt, Some(format!("{}/{}", m.id, m.version))))
}

/// Create a prefix with the runtime (`wineboot -u`), then turn its links into
/// the builder's home into real dirs (decision 4). Drives are left alone:
/// build steps after this still run unix-path installers through `z:`, so
/// [`localize`] drops them once the build is done.
pub fn create_prefix(rt: &Runtime, prefix: &Path) -> Result<(), MacAppError> {
    installer::stage_prefix(rt, prefix)?;
    localize_home(prefix)?;
    Ok(())
}

/// Make the game reachable as `C:\GOG\<name>` without `z:`: a relative
/// symlink `prefix/drive_c/GOG/<name>` → `../../../game`. Replaces whatever
/// was there (a rebuild re-clones `game/`).
pub fn link_game(dirs: &GameDirs, tree_name: &str) -> Result<PathBuf, MacAppError> {
    let gog = dirs.prefix.join("drive_c/GOG");
    std::fs::create_dir_all(&gog)?;
    let link = gog.join(tree_name);
    if let Ok(meta) = link.symlink_metadata() {
        if meta.is_dir() {
            std::fs::remove_dir_all(&link)?;
        } else {
            std::fs::remove_file(&link)?;
        }
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink("../../../game", &link)?;
    Ok(link)
}

/// Decision 4 in full: home folders are real dirs, and `dosdevices/` keeps
/// only `c:`. Idempotent.
pub fn localize(prefix: &Path) -> Result<(), MacAppError> {
    localize_home(prefix)?;
    drop_host_drives(prefix)
}

/// Replace every symlink under `drive_c/users/` whose target is absolute
/// (`Documents -> /Users/<builder>/Documents`, `…/Templates -> ~/Templates`)
/// with an empty real dir. Relative links stay. Returns the replaced paths.
pub fn localize_home(prefix: &Path) -> Result<Vec<PathBuf>, MacAppError> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), MacAppError> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(());
        };
        for e in entries.flatten() {
            let p = e.path();
            let ty = e.file_type()?;
            if ty.is_symlink() {
                if std::fs::read_link(&p)?.is_absolute() {
                    std::fs::remove_file(&p)?;
                    std::fs::create_dir(&p)?;
                    out.push(p);
                }
            } else if ty.is_dir() {
                walk(&p, out)?;
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(&prefix.join("drive_c/users"), &mut out)?;
    Ok(out)
}

/// Remove every `dosdevices/` entry but `c:`: `z:` → `/` and the drive
/// letters Wine maps to whatever volumes the builder had mounted (simulator
/// runtimes, `/Users/…` disk images, their raw `x::` devices). Wine maps the
/// player's own removable volumes again at run time.
pub fn drop_host_drives(prefix: &Path) -> Result<(), MacAppError> {
    let Ok(entries) = std::fs::read_dir(prefix.join("dosdevices")) else {
        return Ok(());
    };
    for e in entries.flatten() {
        if e.file_name() != "c:" && e.file_type()?.is_symlink() {
            std::fs::remove_file(e.path())?;
        }
    }
    Ok(())
}

/// `reg add` argv for one compat registry value.
fn reg_add_argv(v: &compat::RegistryValue) -> Vec<String> {
    let mut argv = vec!["reg".to_string(), "add".to_string(), v.key.clone()];
    match &v.name {
        Some(name) => argv.extend(["/v".to_string(), name.clone()]),
        None => argv.push("/ve".to_string()),
    }
    argv.extend([
        "/t".to_string(),
        v.kind.clone().unwrap_or_else(|| "REG_SZ".to_string()),
        "/d".to_string(),
        v.data.clone(),
        "/f".to_string(),
    ]);
    argv
}

/// winetricks verbs through gogo's extractor shims (design 12 §8).
/// winetricks runs its downloads by unix path, so `z:` is back for the
/// duration and gone again afterwards. It skips verbs already installed.
fn run_verbs(
    rt: &Runtime,
    prefix: &Path,
    verbs: &[String],
    warnings: &mut Vec<String>,
) -> Result<(), MacAppError> {
    let Some(wt) = tools::resolve_or_provision_winetricks(&mut |ev| {
        if let ProgressEvent::Warn { message } = ev {
            warnings.push(message.clone());
        }
    }) else {
        return Err(MacAppError::Recipe(format!(
            "verbs {} need winetricks (auto-provision failed; set WINETRICKS or \
             `gogo runtimes install winetricks`)",
            verbs.join(", ")
        )));
    };
    let z = prefix.join("dosdevices/z:");
    let restored = z.symlink_metadata().is_err();
    if restored {
        std::fs::create_dir_all(prefix.join("dosdevices"))?;
        #[cfg(unix)]
        std::os::unix::fs::symlink("/", &z)?;
    }
    // winetricks is a /bin/sh script and calls wine through /usr/bin/env:
    // SIP strips DYLD_* from both, so wine gets wrappers that set it
    // themselves. WINE64 too: winetricks derives `${WINE}64`, which a wow64
    // engine doesn't have.
    let (wine, server) = tools::wine_wrappers(&rt.wine, &wineserver_of(rt))?;
    let mut cmd = wine_cmd(&wt, &rt.wine);
    cmd.arg("-q")
        .args(verbs)
        .env("WINE", &wine)
        .env("WINE64", &wine)
        .env("WINESERVER", &server)
        .env("WINEPREFIX", prefix)
        .env("PATH", tools::path_with_shims()?);
    let status = installer::run_with_timeout(&mut cmd, Duration::from_secs(1800));
    if restored {
        let _ = std::fs::remove_file(&z);
    }
    match status {
        Some(s) if s.success() => Ok(()),
        Some(s) => Err(MacAppError::Recipe(format!(
            "winetricks {} exited {s}",
            verbs.join(" ")
        ))),
        None => Err(MacAppError::Recipe("winetricks timed out".into())),
    }
}

/// Wait until the prefix's wineserver has exited (`wineserver -w`), so
/// nothing writes into the prefix after setup returns; `-k` after a minute.
fn wait_quiet(rt: &Runtime, prefix: &Path) {
    let server = wineserver_of(rt);
    if !server.is_file() {
        return;
    }
    let mut wait = wine_cmd(&server, &rt.wine);
    wait.arg("-w").env("WINEPREFIX", prefix);
    if installer::run_with_timeout(&mut wait, Duration::from_secs(60)).is_none() {
        let _ = installer::stop_wineserver(rt, prefix);
    }
}

#[cfg(test)]
mod setup_tests {
    use super::*;

    fn link(target: &str, at: &Path) {
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target, at).unwrap();
    }

    /// What a fresh CX24 prefix looks like on the build host (2026-10-23).
    fn builder_prefix(p: &Path) {
        let u = p.join("drive_c/users/crossover");
        std::fs::create_dir_all(u.join("Desktop")).unwrap();
        for (name, target) in [
            ("Documents", "/Users/builder/Documents"),
            ("Downloads", "/Users/builder/Downloads"),
            ("Music", "/Users/builder/Music"),
            ("Pictures", "/Users/builder/Pictures"),
            ("Videos", "/Users/builder/Movies"),
            (
                "AppData/Roaming/Microsoft/Windows/Templates",
                "/Users/builder/Templates",
            ),
        ] {
            link(target, &u.join(name));
        }
        std::fs::create_dir_all(p.join("drive_c/users/Public/Documents")).unwrap();
        link("../../../game", &p.join("drive_c/GOG/Game"));
        link("../drive_c", &p.join("dosdevices/c:"));
        link("/", &p.join("dosdevices/z:"));
        link(
            "/Users/builder/Library/Developer/mounts/x",
            &p.join("dosdevices/e:"),
        );
        link("/dev/rdisk19s1", &p.join("dosdevices/e::"));
    }

    fn links(p: &Path) -> Vec<(String, String)> {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                let ty = e.file_type().unwrap();
                if ty.is_symlink() {
                    out.push((
                        p.strip_prefix(root).unwrap().display().to_string(),
                        std::fs::read_link(&p).unwrap().display().to_string(),
                    ));
                } else if ty.is_dir() {
                    walk(root, &p, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(p, p, &mut out);
        out.sort();
        out
    }

    #[test]
    fn home_links_become_real_dirs_and_only_c_survives() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        builder_prefix(p);
        std::fs::write(p.join("drive_c/users/crossover/Desktop/save.dat"), b"x").unwrap();

        let replaced = localize_home(p).unwrap();
        assert_eq!(replaced.len(), 6, "{replaced:?}");
        // `create_prefix` stops here: build steps still need z:.
        assert!(p.join("dosdevices/z:").symlink_metadata().is_ok());

        localize(p).unwrap();
        assert_eq!(
            links(p),
            [
                ("dosdevices/c:".to_string(), "../drive_c".to_string()),
                ("drive_c/GOG/Game".to_string(), "../../../game".to_string()),
            ]
        );
        let docs = p.join("drive_c/users/crossover/Documents");
        assert!(docs.is_dir() && !docs.symlink_metadata().unwrap().is_symlink());
        assert!(
            p.join("drive_c/users/crossover/AppData/Roaming/Microsoft/Windows/Templates")
                .is_dir()
        );
        // Real dirs and their contents are untouched.
        assert!(p.join("drive_c/users/crossover/Desktop/save.dat").is_file());
        assert!(p.join("drive_c/users/Public/Documents").is_dir());

        // Idempotent.
        assert!(localize_home(p).unwrap().is_empty());
        localize(p).unwrap();
        assert_eq!(links(p).len(), 2);
    }

    #[test]
    fn link_game_is_relative_and_replaces() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = GameDirs::at(tmp.path().join("games/1"));
        std::fs::create_dir_all(&dirs.game).unwrap();
        std::fs::write(dirs.game.join("g.exe"), b"MZ").unwrap();
        std::fs::create_dir_all(dirs.prefix.join("drive_c/GOG/Foo")).unwrap();
        let link = link_game(&dirs, "Foo").unwrap();
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            Path::new("../../../game")
        );
        assert!(link.join("g.exe").is_file());
        link_game(&dirs, "Foo").unwrap();
        std::fs::create_dir_all(dirs.prefix.join("dosdevices")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/", dirs.prefix.join("dosdevices/z:")).unwrap();
        drop_host_drives(&dirs.prefix).unwrap();
        assert!(
            dirs.prefix
                .join("dosdevices/z:")
                .symlink_metadata()
                .is_err()
        );
        drop_host_drives(&dirs.prefix).unwrap();
    }

    #[test]
    fn localize_tolerates_a_missing_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        localize(&tmp.path().join("nope")).unwrap();
    }

    #[test]
    fn reg_add_argv_shapes() {
        let v = compat::RegistryValue {
            key: r"HKCU\Software\Wine\Direct3D".into(),
            name: Some("csmt".into()),
            kind: Some("REG_DWORD".into()),
            data: "0".into(),
        };
        assert_eq!(
            reg_add_argv(&v),
            [
                "reg",
                "add",
                r"HKCU\Software\Wine\Direct3D",
                "/v",
                "csmt",
                "/t",
                "REG_DWORD",
                "/d",
                "0",
                "/f"
            ]
        );
        let d = compat::RegistryValue {
            key: r"HKCU\Software\Game".into(),
            name: None,
            kind: None,
            data: "x".into(),
        };
        assert_eq!(
            reg_add_argv(&d),
            [
                "reg",
                "add",
                r"HKCU\Software\Game",
                "/ve",
                "/t",
                "REG_SZ",
                "/d",
                "x",
                "/f"
            ]
        );
    }
}
