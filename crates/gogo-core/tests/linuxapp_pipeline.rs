//! Integration: `gogo linux-app` build and setup (design 18 §10, #176)
//! against a **fake `umu-run`** on PATH and a fake Proton build — no real
//! Proton, umu or network. The fake logs every run (argv, the env gogo
//! sets, the working dir) and simulates prefix creation (with the links
//! out a real Proton prefix has), `winetricks` (umu's: nothing installed
//! when any verb already is), `reg` and a GOG offline installer.
//!
//! Everything lives under `CARGO_TARGET_TMPDIR`: gogo refuses `/tmp` paths
//! for umu (its FHS env has a private `/tmp`), and `HOME` points into the
//! scratch dir so no Steam of the user's is ever looked at.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};

use gogo::engine::events::ProgressEvent;
use gogo::linuxapp::build::{self, LinuxAppOptions};
use gogo::linuxapp::game::LinuxState;
use gogo::linuxapp::manage;
use gogo::linuxapp::setup::{self, SetupOptions};
use gogo::macapp::launch::Library;
use gogo::macapp::{Renderer, RendererChoice};

const ID: &str = "1999999901";

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The fake umu-run: see the module docs. `$FAKE_UMU_LOG` gets one `ARGV:`
/// and one `ENV:` line per run.
const FAKE_UMU: &str = r#"#!/bin/sh
log="$FAKE_UMU_LOG"
printf 'ARGV:%s\n' "$*" >> "$log"
printf 'ENV:WINEPREFIX=%s PROTONPATH=%s GAMEID=%s STORE=%s PROTONFIXES_DISABLE=%s GAME_DRIVE=%s CWD=%s\n' \
  "$WINEPREFIX" "$PROTONPATH" "$GAMEID" "$STORE" "$PROTONFIXES_DISABLE" "$PROTON_SET_GAME_DRIVE" "$(pwd)" >> "$log"
[ -n "$PRESSURE_VESSEL_FILESYSTEMS_RW" ] && printf 'SHARED:%s\n' "$PRESSURE_VESSEL_FILESYSTEMS_RW" >> "$log"
[ -n "$WINEPREFIX" ] || { echo "no WINEPREFIX" >&2; exit 3; }
case "$1" in
  "")
    # umu's setup_pfx runs before Proton: it leaves drive_c/users either way.
    mkdir -p "$WINEPREFIX/drive_c/users/steamuser"
    ln -sfn . "$WINEPREFIX/pfx"
    [ -n "$FAKE_UMU_BROKEN" ] && { echo "bwrap: Can't find source path /sys" >&2; exit 1; }
    mkdir -p "$WINEPREFIX/drive_c/users/steamuser/AppData/Roaming" "$WINEPREFIX/dosdevices" \
      "$WINEPREFIX/drive_c/windows/system32"
    echo "WINE REGISTRY Version 2" > "$WINEPREFIX/system.reg"
    ln -sfn ../drive_c "$WINEPREFIX/dosdevices/c:"
    ln -sfn / "$WINEPREFIX/dosdevices/z:"
    ln -sfn "$HOME" "$WINEPREFIX/dosdevices/x:"
    ln -sfn /dev/ttyS0 "$WINEPREFIX/dosdevices/com1"
    ln -sfn "$HOME/Documents" "$WINEPREFIX/drive_c/users/steamuser/Documents"
    exit 0 ;;
  winetricks)
    shift
    for v in "$@"; do
      grep -qx "$v" "$WINEPREFIX/winetricks.log" 2>/dev/null && exit 1
    done
    for v in "$@"; do echo "$v" >> "$WINEPREFIX/winetricks.log"; done
    exit 0 ;;
  reg) exit 0 ;;
  *.exe)
    for a in "$@"; do
      case "$a" in /DIR=*) dir="${a#/DIR=}" ;; esac
    done
    rel=$(printf '%s' "${dir#C:\\}" | tr '\\' '/')
    target="$WINEPREFIX/drive_c/$rel"
    mkdir -p "$target"
    cp "$FAKE_GAME_EXE" "$target/game.exe"
    printf '%s' '{"gameId":"1999999901","rootGameId":"1999999901","name":"Test Game","playTasks":[{"type":"FileTask","category":"game","path":"game.exe","isPrimary":true}]}' \
      > "$target/goggame-1999999901.info"
    exit 0 ;;
esac
exit 0
"#;

/// One scratch world: `GOGO_HOME`, `HOME`, the fake umu-run on PATH, a
/// fake Proton build, a depot-style game tree. Env restored on drop.
struct World {
    root: PathBuf,
    home: PathBuf,
    build: PathBuf,
    tree: PathBuf,
    umu_log: PathBuf,
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

/// Can a `user.*` xattr be set under `base`?
fn xattrs_allowed(base: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(base)?;
    let probe = base.join(format!(".xattr-probe-{}", std::process::id()));
    std::fs::write(&probe, b"")?;
    let r = rustix::fs::lsetxattr(
        &probe,
        "user.gogo.probe",
        b"1",
        rustix::fs::XattrFlags::empty(),
    );
    let _ = std::fs::remove_file(&probe);
    r.map_err(std::io::Error::from)
}

impl World {
    fn new(name: &str, imports: &[&str]) -> World {
        Self::new_in(Path::new(env!("CARGO_TARGET_TMPDIR")), name, imports)
    }

    /// A world on btrfs: under `GOGO_TEST_BTRFS_DIR` when set (the
    /// `snapshots-btrfs` CI job), else `CARGO_TARGET_TMPDIR` when that is on
    /// btrfs. `None`, after a `SKIP …` line, elsewhere; a failure instead with
    /// `GOGO_TEST_BTRFS_REQUIRED` set, as in `snapshot_btrfs.rs`.
    fn on_btrfs(test: &str, imports: &[&str]) -> Option<World> {
        let base = std::env::var_os("GOGO_TEST_BTRFS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")));
        if !gogo::snapshot::btrfs::is_btrfs(&base).unwrap_or(false) {
            assert!(
                std::env::var_os("GOGO_TEST_BTRFS_REQUIRED").is_none(),
                "{test}: {} is not on btrfs and GOGO_TEST_BTRFS_REQUIRED is set",
                base.display()
            );
            println!(
                "SKIP {test}: {} is not on btrfs (set GOGO_TEST_BTRFS_DIR to a btrfs dir)",
                base.display()
            );
            return None;
        }
        // Some sandboxes (a Nix build's) put the scratch on the host's btrfs but
        // refuse `user.*` xattrs, which Wine's prefix needs and the convert
        // test sets: a missing capability, not a failure.
        if let Err(e) = xattrs_allowed(&base) {
            assert!(
                std::env::var_os("GOGO_TEST_BTRFS_REQUIRED").is_none(),
                "{test}: {} refuses user.* xattrs ({e}) and GOGO_TEST_BTRFS_REQUIRED is set",
                base.display()
            );
            println!(
                "SKIP {test}: the environment refuses user.* xattrs on {} ({e}; a Nix build sandbox?)",
                base.display()
            );
            return None;
        }
        Some(Self::new_in(&base, test, imports))
    }

    fn new_in(base: &Path, name: &str, imports: &[&str]) -> World {
        let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let root = base
            .join("linuxapp")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("gogo-home");
        let user_home = root.join("user-home");
        let bin = root.join("bin");
        for d in [&home, &user_home, &bin] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(home.join("config.toml"), "[linux]\numu = \"system\"\n").unwrap();
        let umu = bin.join("umu-run");
        std::fs::write(&umu, FAKE_UMU).unwrap();
        chmod_x(&umu);

        let build = root.join("GE-Proton10-34");
        std::fs::create_dir_all(build.join("protonfixes/gamefixes-gog")).unwrap();
        std::fs::write(build.join("proton"), "#!/bin/sh\n# fake proton\n").unwrap();
        std::fs::write(build.join("version"), "1793687165 GE-Proton10-34\n").unwrap();
        std::fs::write(
            build.join("toolmanifest.vdf"),
            "\"manifest\"\n{\n  \"version\" \"2\"\n  \"commandline\" \"/proton %verb%\"\n  \
             \"require_tool_appid\" \"4183110\"\n  \"use_sessions\" \"1\"\n}\n",
        )
        .unwrap();
        std::fs::write(
            build.join("protonfixes/umu-database.csv"),
            format!(
                "TITLE,STORE,CODENAME,UMU_ID,COMMON ACRONYM (Optional),NOTE (Optional),\
                 EXE_STRINGS (Optional)\nTest Game,gog,{ID},umu-777001,,,\n"
            ),
        )
        .unwrap();

        let tree = root.join("depot/Test Game");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("game.exe"), pe_with_imports(false, imports)).unwrap();
        std::fs::write(
            tree.join(format!("goggame-{ID}.info")),
            format!(
                "{{\"gameId\":\"{ID}\",\"rootGameId\":\"{ID}\",\"name\":\"Test Game\",\
                 \"playTasks\":[{{\"type\":\"FileTask\",\"category\":\"game\",\
                 \"path\":\"game.exe\",\"arguments\":\"-windowed\",\"isPrimary\":true}}]}}"
            ),
        )
        .unwrap();
        let exe = root.join("installer-game.exe");
        std::fs::write(&exe, pe_with_imports(false, imports)).unwrap();

        let umu_log = root.join("umu.log");
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut w = World {
            root,
            home,
            build,
            tree,
            umu_log,
            saved: Vec::new(),
            _guard: guard,
        };
        let vars: Vec<(&'static str, std::ffi::OsString)> = vec![
            ("GOGO_HOME", w.home.clone().into()),
            ("GOGO_CONFIG", w.home.join("config.toml").into()),
            ("HOME", user_home.clone().into()),
            ("XDG_DATA_HOME", user_home.join(".local/share").into()),
            ("XDG_CACHE_HOME", user_home.join(".cache").into()),
            ("PATH", path.into()),
            ("FAKE_UMU_LOG", w.umu_log.clone().into()),
            ("FAKE_GAME_EXE", exe.into()),
        ];
        for (k, v) in vars {
            w.saved.push((k, std::env::var_os(k)));
            // SAFETY: env mutation under ENV_LOCK; tests in this binary hold it.
            unsafe { std::env::set_var(k, v) };
        }
        w
    }

    /// The game's dir, wherever gogo finds it.
    fn game(&self) -> PathBuf {
        let dirs = gogo::macapp::launch::GameDirs::for_game(ID).unwrap();
        dirs.root
    }

    /// A library on "another drive": a space, and a `games` part that must
    /// not be taken for the data root's.
    fn library(&self) -> Library {
        Library::resolve(&self.root.join("big drive/games"), None)
            .unwrap()
            .unwrap()
    }

    fn opts_in_library(&self) -> LinuxAppOptions {
        LinuxAppOptions {
            library: Some(self.library()),
            ..self.opts()
        }
    }

    fn locations(&self) -> PathBuf {
        self.home.join("game-locations")
    }

    fn opts(&self) -> LinuxAppOptions {
        LinuxAppOptions {
            game_tree: Some(self.tree.clone()),
            runtime: Some(format!("path:{}", self.build.display())),
            gogo_path: Some(PathBuf::from("/run/current-system/sw/bin/gogo")),
            ..Default::default()
        }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.umu_log).unwrap_or_default()
    }

    fn conf(&self) -> String {
        std::fs::read_to_string(self.game().join("launch.conf")).unwrap()
    }

    fn state(&self) -> LinuxState {
        LinuxState::load(&self.game().join("state.toml"))
            .unwrap()
            .unwrap()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        for (k, v) in self.saved.drain(..) {
            // SAFETY: as in `new`, under ENV_LOCK (still held: `_guard`
            // drops after this).
            unsafe {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

fn chmod_x(p: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn quiet() -> impl FnMut(&ProgressEvent) {
    |_| {}
}

/// Plan and run a depot build with `opts`.
fn build_with(opts: &LinuxAppOptions) -> build::BuildOutcome {
    let plan = build::plan(opts, None, &mut quiet()).unwrap();
    build::execute(&plan, None, &mut quiet()).unwrap()
}

/// Every umu run had the env gogo owes it, and nothing under `/tmp`.
fn assert_runs_are_clean(log: &str, umu_id: &str) {
    let envs: Vec<&str> = log.lines().filter(|l| l.starts_with("ENV:")).collect();
    assert!(!envs.is_empty(), "no umu runs logged");
    for l in &envs {
        assert!(!l.contains("WINEPREFIX= "), "WINEPREFIX unset: {l}");
        assert!(l.contains("STORE=gog"), "{l}");
        assert!(l.contains("PROTONFIXES_DISABLE=1"), "{l}");
        assert!(l.contains("GAME_DRIVE=0"), "{l}");
        assert!(
            l.contains(&format!("GAMEID={umu_id}")) || l.contains("GAMEID=umu-0"),
            "{l}"
        );
    }
    for l in log.lines() {
        for word in l.split([' ', '=']) {
            assert!(
                !word.starts_with("/tmp/") && !word.starts_with("Z:\\tmp\\"),
                "a /tmp path reached umu: {l}"
            );
        }
    }
}

/// No absolute links under `drive_c/users/`; `dosdevices/` is `c:` and
/// `z:` → `/` only (design 16 decision 4, design 18 §7.2).
fn assert_isolated(prefix: &Path) {
    assert!(prefix.join("drive_c").is_dir());
    let mut drives: Vec<String> = std::fs::read_dir(prefix.join("dosdevices"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    drives.sort();
    assert_eq!(drives, ["c:", "z:"], "in {}", prefix.display());
    assert_eq!(
        std::fs::read_link(prefix.join("dosdevices/z:")).unwrap(),
        Path::new("/")
    );
    fn walk(d: &Path) {
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            let ty = e.file_type().unwrap();
            if ty.is_symlink() {
                assert!(
                    !std::fs::read_link(&p).unwrap().is_absolute(),
                    "absolute link {}",
                    p.display()
                );
            } else if ty.is_dir() {
                walk(&p);
            }
        }
    }
    walk(&prefix.join("drive_c/users"));
}

/// `KEY=…` in launch.conf.
fn conf_value<'a>(conf: &'a str, key: &str) -> &'a str {
    conf.lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("no {key} in\n{conf}"))
}

// — the depot lane —

#[test]
fn depot_lane_builds_and_sets_up() {
    let w = World::new("depot", &["d3d9.dll", "d3dx9_43.dll"]);
    let plan = build::plan(&w.opts(), None, &mut quiet()).unwrap();
    assert_eq!(plan.gogid.as_deref(), Some(ID));
    assert!(!plan.rebuild);
    assert!(
        plan.umu_line.contains("umu-run on PATH"),
        "{}",
        plan.umu_line
    );
    assert!(plan.proton_download.is_none());
    assert!(
        plan.runtime_line.contains("unmanaged"),
        "{}",
        plan.runtime_line
    );
    assert!(
        plan.container_line.contains("steamrt4"),
        "{}",
        plan.container_line
    );
    assert!(plan.game_files.is_some());
    let sel = plan.renderer.as_ref().unwrap();
    assert_eq!(sel.renderer.value, Renderer::Dxvk.id());
    assert_eq!(
        build::switches(plan.renderer_setup.as_ref().unwrap()),
        "nothing set: Proton's defaults"
    );
    assert_eq!(plan.umu_id.as_ref().unwrap().id, "umu-777001");
    assert!(plan.fixes.is_some());

    let out = build::execute(&plan, None, &mut quiet()).unwrap();
    assert_eq!(out.gogid, ID);
    assert!(out.setup.prefix_created || w.game().join("prefix/drive_c").is_dir());
    assert!(out.materialized.is_some());

    // gogo-launch.toml: what the build bakes.
    let toml = std::fs::read_to_string(w.game().join("gogo-launch.toml")).unwrap();
    assert!(
        toml.contains(&format!("runtime = \"path:{}\"", w.build.display())),
        "{toml}"
    );
    assert!(toml.contains("renderer = \"dxvk\""), "{toml}");
    assert!(toml.contains("d3dx9_43"), "{toml}");
    assert!(
        toml.contains(r#"exe = 'C:\GOG\Test Game\game.exe'"#)
            || toml.contains(r#"exe = "C:\\GOG\\Test Game\\game.exe""#),
        "{toml}"
    );

    // launch.conf: §7.1's keys, paths relative to the data root.
    let conf = w.conf();
    assert_eq!(conf_value(&conf, "GOGID"), format!("'{ID}'"));
    assert_eq!(conf_value(&conf, "UMU_ID"), "'umu-777001'");
    assert_eq!(conf_value(&conf, "PREFIX"), format!("'games/{ID}/prefix'"));
    assert_eq!(conf_value(&conf, "GAME_DIR"), format!("'games/{ID}/game'"));
    assert_eq!(
        conf_value(&conf, "EXE"),
        format!("'games/{ID}/prefix/drive_c/GOG/Test Game/game.exe'")
    );
    assert_eq!(conf_value(&conf, "ARGS"), "('-windowed')");
    assert_eq!(conf_value(&conf, "RENDERER"), "'dxvk'");
    assert_eq!(conf_value(&conf, "ENV"), "()");
    assert_eq!(
        conf_value(&conf, "GOGO_PATH"),
        "'/run/current-system/sw/bin/gogo'"
    );
    assert_eq!(
        conf_value(&conf, "DATA_ROOT"),
        format!("'{}'", w.home.display())
    );
    assert_eq!(
        conf_value(&conf, "STAMP"),
        format!("'{}'", out.setup.state.stamp)
    );
    assert_eq!(
        gogo::linuxapp::game::current_stamp(ID).unwrap(),
        Some(out.setup.state.stamp.clone())
    );

    // state.toml: the runtime and renderer GC reads, the protonfix hash.
    let st = w.state();
    assert_eq!(st.runtime, format!("path:{}", w.build.display()));
    assert_eq!(st.renderer, "dxvk");
    assert_eq!(st.umu_id, "umu-777001");
    assert_eq!(st.protonfix, "none");
    assert!(st.verbs.contains(&"d3dx9_43".to_string()), "{:?}", st.verbs);

    let log = w.log();
    assert_runs_are_clean(&log, "umu-777001");
    assert_eq!(log.matches("ARGV:winetricks d3dx9_43").count(), 1, "{log}");
    assert_isolated(&w.game().join("prefix"));
    assert!(
        w.game().join("install/setup.log").is_file(),
        "umu's output goes to install/setup.log"
    );

    // Setup again: nothing pending, so winetricks doesn't run (umu's would
    // exit 1 having installed nothing).
    let mtime = |f: &str| {
        std::fs::metadata(w.game().join(f))
            .unwrap()
            .modified()
            .unwrap()
    };
    let before = (mtime("launch.conf"), mtime("state.toml"));
    std::thread::sleep(std::time::Duration::from_millis(20));
    let again = setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert_eq!(
        (mtime("launch.conf"), mtime("state.toml")),
        before,
        "an unchanged setup rewrote its files"
    );
    assert!(!again.prefix_created);
    assert!(again.verbs_installed.is_empty());
    assert_eq!(w.log().matches("ARGV:winetricks").count(), 1);
    assert_isolated(&w.game().join("prefix"));

    // A rebuild keeps the prefix (saves) and replaces the game files.
    std::fs::write(
        w.game().join("prefix/drive_c/users/steamuser/save.dat"),
        "x",
    )
    .unwrap();
    let plan = build::plan(&w.opts(), None, &mut quiet()).unwrap();
    assert!(plan.rebuild);
    build::execute(&plan, None, &mut quiet()).unwrap();
    assert!(
        w.game()
            .join("prefix/drive_c/users/steamuser/save.dat")
            .is_file()
    );
}

#[test]
fn a_new_protonfix_is_noticed_and_its_verbs_installed_alone() {
    let w = World::new("protonfix", &["d3d9.dll", "d3dx9_43.dll"]);
    build_with(&w.opts());
    assert_eq!(w.state().protonfix, "none");

    // The Proton build gains a module for the game's umu id.
    std::fs::write(
        w.build.join("protonfixes/gamefixes-gog/umu-777001.py"),
        "from protonfixes import util\n\ndef main():\n    util.protontricks('corefonts')\n",
    )
    .unwrap();
    let out = setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert_ne!(out.state.protonfix, "none");
    assert!(
        out.notes.iter().any(|n| n.contains("protonfix")),
        "{:?}",
        out.notes
    );
    // Only the missing verb goes to winetricks.
    let log = w.log();
    assert!(log.contains("ARGV:winetricks corefonts\n"), "{log}");
    assert_eq!(log.matches("d3dx9_43").count(), 1, "{log}");
    assert!(out.state.verbs.contains(&"corefonts".to_string()));

    // Unchanged since: no note.
    let out = setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert!(
        !out.notes.iter().any(|n| n.contains("protonfix")),
        "{:?}",
        out.notes
    );
}

// — renderers (design 18 §3) —

#[test]
fn d7vk_for_d3d7_and_never_both_ddraw_dlls() {
    let w = World::new("d7vk", &["ddraw.dll", "d3dim.dll"]);
    let dll = w.build.join(gogo::macapp::renderer::D7VK_DLL);
    std::fs::create_dir_all(dll.parent().unwrap()).unwrap();
    std::fs::write(&dll, "MZ").unwrap();
    let plan = build::plan(&w.opts(), None, &mut quiet()).unwrap();
    assert_eq!(plan.renderer.as_ref().unwrap().renderer.value, "d7vk");
    assert_eq!(
        build::switches(plan.renderer_setup.as_ref().unwrap()),
        "PROTON_USE_D7VK=1"
    );
    build::execute(&plan, None, &mut quiet()).unwrap();
    let conf = w.conf();
    assert_eq!(conf_value(&conf, "ENV"), "('PROTON_USE_D7VK=1')");
    assert!(!w.game().join("game/ddraw.dll").exists());

    // doctor: D7VK is there and set; then a ddraw.dll beside it (never
    // both), then a build without D7VK.
    use gogo::macapp::doctor::Level;
    let host = doctor_host(&w, &[("radeon.x86_64", 2), ("radeon.i686", 1)]);
    let renderer = |r: &gogo::macapp::doctor::Report| {
        r.findings
            .iter()
            .find(|f| f.check == "renderer")
            .map(|f| (f.level, f.message.clone()))
            .unwrap()
    };
    let r = doctor(&w, &host);
    assert_eq!(renderer(&r), (Level::Ok, "d7vk (PROTON_USE_D7VK=1)".into()));
    let stray = w.game().join("game/ddraw.dll");
    std::fs::write(&stray, "MZ").unwrap();
    let (level, msg) = renderer(&doctor(&w, &host));
    assert!(
        level == Level::Fail && msg.contains("D7VK never loads"),
        "{msg}"
    );
    std::fs::remove_file(&stray).unwrap();
    std::fs::remove_file(&dll).unwrap();
    let (level, msg) = renderer(&doctor(&w, &host));
    assert!(
        level == Level::Fail
            && msg.starts_with("d7vk: ")
            && msg.contains("has no files/lib/wine/d7vk"),
        "{msg}"
    );
    std::fs::write(&dll, "MZ").unwrap();

    // The game ships its own ddraw.dll: it's kept, D7VK isn't turned on.
    std::fs::write(w.tree.join("ddraw.dll"), "MZ").unwrap();
    let plan = build::plan(&w.opts(), None, &mut quiet()).unwrap();
    let p = plan.renderer_setup.as_ref().unwrap();
    assert!(!p.env.contains_key("PROTON_USE_D7VK"), "{p:?}");
    build::execute(&plan, None, &mut quiet()).unwrap();
    let conf = w.conf();
    assert!(
        !conf_value(&conf, "ENV").contains("PROTON_USE_D7VK"),
        "{conf}"
    );
    assert_eq!(conf_value(&conf, "DLL_OVERRIDES"), "'ddraw=n,b'");
}

/// SimCity 3000's layout (#199): the exe, and whatever Wine should load
/// beside it, in `Apps/`.
fn exe_in_apps(w: &World) -> PathBuf {
    let apps = w.tree.join("Apps");
    std::fs::create_dir_all(&apps).unwrap();
    std::fs::rename(w.tree.join("game.exe"), apps.join("game.exe")).unwrap();
    let info = w.tree.join(format!("goggame-{ID}.info"));
    let text = std::fs::read_to_string(&info).unwrap();
    std::fs::write(&info, text.replace("\"game.exe\"", r#""Apps\\game.exe""#)).unwrap();
    apps
}

fn renderer_finding(r: &gogo::macapp::doctor::Report) -> (gogo::macapp::doctor::Level, String) {
    r.findings
        .iter()
        .find(|f| f.check == "renderer")
        .map(|f| (f.level, f.message.clone()))
        .unwrap()
}

#[test]
fn the_games_own_ddraw_beside_an_exe_in_a_subdir_is_loaded() {
    use gogo::macapp::doctor::Level;
    let w = World::new("own-apps", &["ddraw.dll"]);
    let apps = exe_in_apps(&w);
    std::fs::write(apps.join("ddraw.dll"), "game").unwrap();
    let plan = build::plan(&w.opts(), None, &mut quiet()).unwrap();
    let p = plan.renderer_setup.as_ref().unwrap();
    assert_eq!(
        build::switches(p),
        "the game's own ddraw.dll, ddraw=n,b",
        "{p:?}"
    );
    build::execute(&plan, None, &mut quiet()).unwrap();
    assert_eq!(conf_value(&w.conf(), "DLL_OVERRIDES"), "'ddraw=n,b'");
    let dll = w.game().join("game/Apps/ddraw.dll");
    assert_eq!(std::fs::read_to_string(&dll).unwrap(), "game");
    assert!(!w.game().join("game/ddraw.ini").exists());

    let host = doctor_host(&w, &[("radeon.x86_64", 2), ("radeon.i686", 1)]);
    let (level, msg) = renderer_finding(&doctor(&w, &host));
    assert!(level == Level::Ok && msg.contains("own ddraw.dll"), "{msg}");

    // What a build before #199 wrote: no override. Not fatal, the game runs
    // on Wine's builtin, but it isn't what the game shipped.
    let conf_path = w.game().join("launch.conf");
    let conf = w.conf();
    std::fs::write(&conf_path, conf.replace("'ddraw=n,b'", "''")).unwrap();
    let (level, msg) = renderer_finding(&doctor(&w, &host));
    assert!(
        level == Level::Warn && msg.contains("no ddraw=n,b override"),
        "{msg}"
    );
    // The next setup picks the fix up, with no config change.
    setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert_eq!(conf_value(&w.conf(), "DLL_OVERRIDES"), "'ddraw=n,b'");

    // An L4 `ddraw = "b"` says not to use it: heard, and said.
    let l4 = gogo::macapp::compat::user_path(ID);
    std::fs::create_dir_all(l4.parent().unwrap()).unwrap();
    std::fs::write(
        &l4,
        "[platforms.linux]\ndll_overrides = { ddraw = \"b\" }\n",
    )
    .unwrap();
    setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert_eq!(conf_value(&w.conf(), "DLL_OVERRIDES"), "'ddraw=b'");
    let (level, msg) = renderer_finding(&doctor(&w, &host));
    assert!(level == Level::Warn && msg.contains("isn't used"), "{msg}");
    std::fs::remove_file(&l4).unwrap();

    // Another case, as Wine's lookup and the scan see it.
    std::fs::rename(&dll, w.game().join("game/Apps/DDRAW.DLL")).unwrap();
    setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert_eq!(conf_value(&w.conf(), "DLL_OVERRIDES"), "'ddraw=n,b'");
}

/// #199: SimCity 3000 Unlimited's shape. DDrawCompat beside the exe isn't
/// loaded, whatever is asked for, and doctor fails only when it is.
#[test]
fn ddrawcompat_beside_the_exe_stays_unloaded() {
    use gogo::macapp::doctor::Level;
    const DLL: &[u8] = b"MZ\0\0DDrawCompat loaded successfully\0";
    let w = World::new("ddrawcompat", &["ddraw.dll"]);
    let apps = exe_in_apps(&w);
    std::fs::write(apps.join("ddraw.dll"), DLL).unwrap();
    let plan = build::plan(&w.opts(), None, &mut quiet()).unwrap();
    let p = plan.renderer_setup.as_ref().unwrap();
    assert_eq!(
        p.ddraw,
        gogo::macapp::renderer::DdrawStep::LeaveDdrawCompat,
        "{p:?}"
    );
    assert!(build::switches(p).contains("DDrawCompat left unloaded"));
    build::execute(&plan, None, &mut quiet()).unwrap();
    assert_eq!(conf_value(&w.conf(), "DLL_OVERRIDES"), "''");
    let dll = w.game().join("game/Apps/ddraw.dll");
    assert_eq!(std::fs::read(&dll).unwrap(), DLL);

    let host = doctor_host(&w, &[("radeon.x86_64", 2), ("radeon.i686", 1)]);
    let (level, msg) = renderer_finding(&doctor(&w, &host));
    assert!(level == Level::Ok && msg.contains("left unloaded"), "{msg}");

    // An L4 that loads it is heard, and doctor says what it does.
    let l4 = gogo::macapp::compat::user_path(ID);
    std::fs::create_dir_all(l4.parent().unwrap()).unwrap();
    std::fs::write(
        &l4,
        "[platforms.linux]\ndll_overrides = { ddraw = \"n,b\" }\n",
    )
    .unwrap();
    setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert_eq!(conf_value(&w.conf(), "DLL_OVERRIDES"), "'ddraw=n,b'");
    let r = doctor(&w, &host);
    let f = r.findings.iter().find(|f| f.check == "renderer").unwrap();
    assert_eq!(f.level, Level::Fail, "{}", f.message);
    assert!(
        f.message.contains("loads the game's DDrawCompat"),
        "{}",
        f.message
    );
    assert!(
        f.hint
            .as_deref()
            .is_some_and(|x| x.contains("dll_overrides"))
    );

    // Dropping it and running setup is the fix.
    std::fs::remove_file(&l4).unwrap();
    setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert_eq!(conf_value(&w.conf(), "DLL_OVERRIDES"), "''");
    let (level, _) = renderer_finding(&doctor(&w, &host));
    assert_eq!(level, Level::Ok);

    // Forcing a renderer that would load or replace it falls back.
    for forced in ["d7vk", "cnc-ddraw"] {
        std::fs::write(&l4, format!("[platforms.linux]\nrenderer = \"{forced}\"\n")).unwrap();
        setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
        let conf = w.conf();
        assert_eq!(conf_value(&conf, "DLL_OVERRIDES"), "''", "{forced}");
        assert!(!conf.contains("PROTON_USE_D7VK"), "{forced}: {conf}");
        assert_eq!(std::fs::read(&dll).unwrap(), DLL, "{forced}");
        assert!(!w.game().join("game/Apps/ddraw.ini").exists(), "{forced}");
    }
    std::fs::remove_file(&l4).unwrap();
}

/// #306: Heroes 3 Complete's exe imports DDrawCompat as `xdd.dll`. gogo's
/// cnc-ddraw goes in under that name, whatever renderer is asked for, and
/// GOG's file is kept beside it.
#[test]
fn ddrawcompat_imported_under_another_name_gets_cnc_ddraw_in_its_place() {
    use gogo::macapp::doctor::Level;
    const DLL: &[u8] = b"MZ\0\0Loading DDrawCompat \0";
    let w = World::new("xdd", &["xdd.dll"]);
    let store = w.root.join("cnc-store");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("ddraw.dll"), "cnc").unwrap();
    // SAFETY: env mutation under ENV_LOCK, held by `w`.
    unsafe { std::env::set_var("GOGO_CNC_DDRAW_DIR", &store) };
    std::fs::write(w.tree.join("xdd.dll"), DLL).unwrap();
    let plan = build::plan(&w.opts(), None, &mut quiet()).unwrap();
    let sel = plan.renderer.as_ref().unwrap();
    assert_eq!(sel.renderer.value, Renderer::CncDdraw.default_ref());
    assert!(
        sel.l1.reason.contains("DDrawCompat as xdd.dll"),
        "{}",
        sel.l1.reason
    );
    let p = plan.renderer_setup.as_ref().unwrap();
    assert!(build::switches(p).contains("as xdd.dll"), "{p:?}");
    build::execute(&plan, None, &mut quiet()).unwrap();
    let tree = w.game().join("game");
    assert_eq!(
        std::fs::read_to_string(tree.join("xdd.dll")).unwrap(),
        "cnc"
    );
    assert_eq!(
        std::fs::read(tree.join("xdd.dll.ddrawcompat")).unwrap(),
        DLL
    );
    assert!(!tree.join("ddraw.dll").exists());
    assert_eq!(conf_value(&w.conf(), "DLL_OVERRIDES"), "''");
    let host = doctor_host(&w, &[("radeon.x86_64", 2), ("radeon.i686", 1)]);
    let (level, msg) = renderer_finding(&doctor(&w, &host));
    assert!(level == Level::Ok && msg.contains("as xdd.dll"), "{msg}");

    // Another renderer asked for: cnc-ddraw still, with no D7VK switch.
    let l4 = gogo::macapp::compat::user_path(ID);
    std::fs::create_dir_all(l4.parent().unwrap()).unwrap();
    std::fs::write(&l4, "[platforms.linux]\nrenderer = \"d7vk\"\n").unwrap();
    setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert!(!w.conf().contains("PROTON_USE_D7VK"));
    assert_eq!(
        std::fs::read_to_string(tree.join("xdd.dll")).unwrap(),
        "cnc"
    );
    std::fs::remove_file(&l4).unwrap();

    // GOG's DDrawCompat back in its place: doctor fails, setup fixes it.
    std::fs::rename(tree.join("xdd.dll.ddrawcompat"), tree.join("xdd.dll")).unwrap();
    let (level, msg) = renderer_finding(&doctor(&w, &host));
    assert!(
        level == Level::Fail && msg.contains("DDrawCompat as xdd.dll"),
        "{msg}"
    );
    setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert_eq!(
        std::fs::read_to_string(tree.join("xdd.dll")).unwrap(),
        "cnc"
    );
    let (level, msg) = renderer_finding(&doctor(&w, &host));
    assert_eq!(level, Level::Ok, "{msg}");
    unsafe { std::env::remove_var("GOGO_CNC_DDRAW_DIR") };
}

#[test]
fn a_ddraw_dll_away_from_the_exe_is_not_the_games_own() {
    let w = World::new("root-dll", &["d3d9.dll"]);
    let _ = exe_in_apps(&w);
    std::fs::write(w.tree.join("ddraw.dll"), "tool").unwrap();
    let plan = build::plan(&w.opts(), None, &mut quiet()).unwrap();
    assert_eq!(
        plan.renderer_setup.as_ref().unwrap().ddraw,
        gogo::macapp::renderer::DdrawStep::None
    );
    build::execute(&plan, None, &mut quiet()).unwrap();
    assert_eq!(conf_value(&w.conf(), "DLL_OVERRIDES"), "''");
    assert_eq!(
        std::fs::read_to_string(w.game().join("game/ddraw.dll")).unwrap(),
        "tool"
    );
}

#[test]
fn cnc_ddraw_goes_beside_the_exe_and_follows_it() {
    use gogo::macapp::doctor::Level;
    let w = World::new("cnc-apps", &["ddraw.dll"]);
    let store = w.root.join("cnc-store");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("ddraw.dll"), "cnc").unwrap();
    // SAFETY: env mutation under ENV_LOCK, held by `w`.
    unsafe { std::env::set_var("GOGO_CNC_DDRAW_DIR", &store) };
    let _ = exe_in_apps(&w);
    let other = w.tree.join("Other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::copy(w.tree.join("Apps/game.exe"), other.join("tool.exe")).unwrap();
    let plan = build::plan(&w.opts(), None, &mut quiet()).unwrap();
    assert_eq!(
        plan.renderer.as_ref().unwrap().renderer.value,
        gogo::macapp::renderer::Renderer::CncDdraw.default_ref()
    );
    build::execute(&plan, None, &mut quiet()).unwrap();
    let tree = w.game().join("game");
    assert_eq!(
        std::fs::read_to_string(tree.join("Apps/ddraw.dll")).unwrap(),
        "cnc"
    );
    assert!(gogo::macapp::ddraw::is_gogo(&tree.join("Apps")));
    assert!(!tree.join("ddraw.dll").exists());
    assert_eq!(conf_value(&w.conf(), "DLL_OVERRIDES"), "'ddraw=n,b'");
    let host = doctor_host(&w, &[("radeon.x86_64", 2), ("radeon.i686", 1)]);
    let (level, msg) = renderer_finding(&doctor(&w, &host));
    assert_eq!(level, Level::Ok, "{msg}");

    // Where a build before #199 left it: out of the root at the next setup,
    // the DLL the game's exe loads untouched.
    std::fs::write(tree.join("ddraw.dll"), "cnc").unwrap();
    std::fs::copy(tree.join("Apps/ddraw.ini"), tree.join("ddraw.ini")).unwrap();
    setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert!(!tree.join("ddraw.dll").exists() && !tree.join("ddraw.ini").exists());
    assert!(tree.join("Apps/ddraw.dll").is_file());

    // Gone from beside the exe: doctor says so, wherever else it is.
    std::fs::remove_file(tree.join("Apps/ddraw.dll")).unwrap();
    std::fs::remove_file(tree.join("Apps/ddraw.ini")).unwrap();
    std::fs::write(tree.join("ddraw.dll"), "cnc").unwrap();
    std::fs::write(tree.join("ddraw.ini"), "; gogo: x\n").unwrap();
    let (level, msg) = renderer_finding(&doctor(&w, &host));
    assert!(
        level == Level::Fail && msg.starts_with("cnc-ddraw: "),
        "{msg}"
    );
    setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();

    // An L4 `exe` in another dir runs instead: the DLL moves with it.
    let l4 = gogo::macapp::compat::user_path(ID);
    std::fs::create_dir_all(l4.parent().unwrap()).unwrap();
    std::fs::write(&l4, "[platforms.linux]\nexe = 'Other\\tool.exe'\n").unwrap();
    setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert!(tree.join("Other/ddraw.dll").is_file());
    assert!(!tree.join("Apps/ddraw.dll").exists());
    let (level, msg) = renderer_finding(&doctor(&w, &host));
    assert_eq!(level, Level::Ok, "{msg}");
    unsafe { std::env::remove_var("GOGO_CNC_DDRAW_DIR") };
}

#[test]
fn wined3d_chosen_sets_the_switch_and_auto_wined3d_sets_nothing() {
    let w = World::new("wined3d", &["d3d9.dll"]);
    let mut opts = w.opts();
    opts.renderer = RendererChoice::Force(Renderer::Wined3d);
    let plan = build::plan(&opts, None, &mut quiet()).unwrap();
    assert_eq!(
        build::switches(plan.renderer_setup.as_ref().unwrap()),
        "PROTON_USE_WINED3D=1"
    );
    build::execute(&plan, None, &mut quiet()).unwrap();
    assert_eq!(conf_value(&w.conf(), "ENV"), "('PROTON_USE_WINED3D=1')");
    assert_eq!(w.state().renderer, "wined3d");
    drop(w);

    // No graphics imports: L1 itself picks wined3d, which sets nothing.
    let w = World::new("wined3d-auto", &["kernel32.dll"]);
    let plan = build::plan(&w.opts(), None, &mut quiet()).unwrap();
    assert_eq!(plan.renderer.as_ref().unwrap().renderer.value, "wined3d");
    assert_eq!(
        build::switches(plan.renderer_setup.as_ref().unwrap()),
        "nothing set: Proton's defaults"
    );
    build::execute(&plan, None, &mut quiet()).unwrap();
    assert_eq!(conf_value(&w.conf(), "ENV"), "()");
    // ... and it is labelled by what runs, not "wined3d" (#229).
    assert_eq!(conf_value(&w.conf(), "RENDERER"), "'proton-default'");
    assert_eq!(w.state().renderer, "proton-default");
    let r = manage::compat(ID).unwrap().renderer.unwrap();
    assert_eq!(
        (r.baked.as_str(), r.auto.as_str(), r.effective.as_str()),
        ("proton-default", "proton-default", "proton-default")
    );
    assert_eq!(r.applied.as_deref(), Some("proton-default"));
    let json = serde_json::to_string(&r).unwrap();
    assert!(json.contains("\"applied\":\"proton-default\""), "{json}");
    // The "next renderer" walk starts from L1's pick: nothing after it.
    assert_eq!(manage::next_renderer(ID, true).unwrap(), None);
    // Doctor says what Proton's defaults mean.
    let host = doctor_host(&w, &[]);
    let r = doctor(&w, &host);
    let (_, msg) = renderer_finding(&r);
    assert!(
        msg.starts_with("proton-default: none (Proton's defaults: DXVK for d3d9–11)"),
        "{msg}"
    );
    drop(w);

    // The same ref chosen (L4) is the real thing: the switch, `wined3d`.
    let w = World::new("wined3d-chosen-over-auto", &["kernel32.dll"]);
    let plan = build::plan(&w.opts(), None, &mut quiet()).unwrap();
    build::execute(&plan, None, &mut quiet()).unwrap();
    let out = manage::retarget(
        ID,
        &[("renderer", Some("wined3d"))],
        &SetupOptions::default(),
        &mut quiet(),
    )
    .unwrap();
    assert_eq!(out.setup.unwrap().unwrap().state.renderer, "wined3d");
    assert_eq!(conf_value(&w.conf(), "ENV"), "('PROTON_USE_WINED3D=1')");
}

/// #300: `--renderer wined3d` on a game whose L1 pick is already wined3d stays
/// a flag (the real wined3d); a file baked before the field was recorded
/// falls back to inferring the layer, as before.
#[test]
fn renderer_flag_equal_to_l1_pick_keeps_its_layer() {
    use gogo::macapp::compat::Layer;
    let w = World::new("wined3d-flag-on-l1", &["kernel32.dll"]);
    let mut opts = w.opts();
    opts.renderer = RendererChoice::Force(Renderer::Wined3d);
    let plan = build::plan(&opts, None, &mut quiet()).unwrap();
    assert_eq!(plan.renderer.as_ref().unwrap().renderer.layer, Layer::Flags);
    assert_eq!(
        build::switches(plan.renderer_setup.as_ref().unwrap()),
        "PROTON_USE_WINED3D=1"
    );
    build::execute(&plan, None, &mut quiet()).unwrap();
    assert_eq!(conf_value(&w.conf(), "ENV"), "('PROTON_USE_WINED3D=1')");
    assert_eq!(conf_value(&w.conf(), "RENDERER"), "'wined3d'");
    assert_eq!(w.state().renderer, "wined3d");
    let r = manage::compat(ID).unwrap().renderer.unwrap();
    assert_eq!(
        (r.baked.as_str(), r.effective.as_str(), r.applied.as_deref()),
        ("wined3d", "wined3d", Some("wined3d"))
    );
    assert_eq!(
        (r.baked_layer, r.effective_layer),
        (Layer::Flags, Layer::Flags)
    );
    assert_eq!(r.auto, "proton-default");
    let host = doctor_host(&w, &[]);
    let (_, msg) = renderer_finding(&doctor(&w, &host));
    assert!(msg.starts_with("wined3d"), "{msg}");

    // A gogo-launch.toml without the field: L1's own pick, as before.
    let toml = w.game().join("gogo-launch.toml");
    let raw = std::fs::read_to_string(&toml).unwrap();
    assert!(raw.contains("renderer_flag = true"), "{raw}");
    std::fs::write(&toml, raw.replace("renderer_flag = true\n", "")).unwrap();
    setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert_eq!(conf_value(&w.conf(), "ENV"), "()");
    assert_eq!(conf_value(&w.conf(), "RENDERER"), "'proton-default'");
    let r = manage::compat(ID).unwrap().renderer.unwrap();
    assert_eq!(r.baked_layer, Layer::Auto);
}

// — the installer lane —

#[test]
fn installer_lane_runs_the_setup_exe_under_umu() {
    let w = World::new("installer", &["d3d9.dll"]);
    let dl = w.root.join("downloads");
    std::fs::create_dir_all(&dl).unwrap();
    let setup_exe = dl.join("setup_test_game_1.0.exe");
    std::fs::write(&setup_exe, "MZ").unwrap();
    let opts = LinuxAppOptions {
        installer: Some(setup_exe.clone()),
        runtime: Some(format!("path:{}", w.build.display())),
        name: Some("Test Game".into()),
        ..Default::default()
    };
    let plan = build::plan(&opts, None, &mut quiet()).unwrap();
    assert!(plan.gogid.is_none());
    let staging = plan.state.root.clone();
    let out = build::execute(&plan, None, &mut quiet()).unwrap();
    assert_eq!(out.gogid, ID);
    assert!(!staging.exists(), "staging dir left behind");
    // The installer lane records its content too (design 19 §6.1).
    let c = gogo::gog::content::load(&out.dirs.root)
        .unwrap()
        .expect("content.toml");
    assert_eq!(c.lane, gogo::gog::content::Lane::Installer);
    assert!(c.dlc.is_empty());

    let log = w.log();
    let argv = log
        .lines()
        .find(|l| l.contains("setup_test_game_1.0.exe"))
        .unwrap_or_else(|| panic!("installer never ran:\n{log}"));
    for flag in ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-"] {
        assert!(argv.contains(flag), "{argv}");
    }
    assert!(
        argv.contains(&format!("/DIR=C:\\GOG\\{}", plan.slug)),
        "{argv}"
    );
    assert!(argv.contains("/LOG=Z:\\"), "{argv}");
    assert!(
        log.contains("GAMEID=umu-0"),
        "the build runs as umu-0:\n{log}"
    );
    assert_runs_are_clean(&log, "umu-777001");

    let toml = std::fs::read_to_string(w.game().join("gogo-launch.toml")).unwrap();
    assert!(toml.contains("lane = \"installer\""), "{toml}");
    let conf = w.conf();
    assert_eq!(
        conf_value(&conf, "EXE"),
        format!("'games/{ID}/prefix/drive_c/GOG/{}/game.exe'", plan.slug)
    );
    assert_isolated(&w.game().join("prefix"));
}

#[test]
fn installer_lane_titles_the_game_from_the_installed_info() {
    let w = World::new("instname", &["d3d9.dll"]);
    let setup_exe = w.root.join("setup_test_game_1.2.3a_(64bit)_(4242).exe");
    std::fs::write(&setup_exe, "MZ").unwrap();
    let opts = LinuxAppOptions {
        installer: Some(setup_exe),
        runtime: Some(format!("path:{}", w.build.display())),
        ..Default::default()
    };
    let plan = build::plan(&opts, None, &mut quiet()).unwrap();
    // The stem is clean before the install; its case is only a guess.
    assert_eq!(plan.display, "Test game");
    assert_eq!(plan.slug, "test-game");
    build::execute(&plan, None, &mut quiet()).unwrap();
    let toml = std::fs::read_to_string(w.game().join("gogo-launch.toml")).unwrap();
    // The title is the installed game's own name; the install dir keeps the slug.
    assert!(toml.contains("title = \"Test Game\""), "{toml}");
    assert!(toml.contains("test-game"), "{toml}");
}

// — after the build: compat, next-renderer, --retarget (#176) —

#[test]
fn compat_next_renderer_and_retarget_work_on_the_linux_layers() {
    let w = World::new("manage", &["d3d9.dll", "d3dx9_43.dll"]);
    let l4 = gogo::macapp::compat::user_path(ID);

    // Not built: L3 and L4 only.
    let r = manage::compat(ID).unwrap();
    assert!(!r.built && r.renderer.is_none());

    build_with(&w.opts());
    let r = manage::compat(ID).unwrap();
    assert!(r.built);
    let x = r.renderer.as_ref().unwrap();
    assert_eq!(
        (x.effective.as_str(), x.baked_layer),
        ("dxvk", gogo::macapp::compat::Layer::Auto)
    );
    assert_eq!(x.applied.as_deref(), Some("dxvk"));
    assert_eq!(
        r.runtime.as_deref(),
        Some(format!("path:{}", w.build.display()).as_str())
    );
    assert!(
        r.fixes.as_deref().unwrap().contains("umu-777001"),
        "{:?}",
        r.fixes
    );
    assert!(
        r.effective.verbs.iter().any(|v| v.value == "d3dx9_43"),
        "{:?}",
        r.effective.verbs
    );
    assert_eq!(r.variants.len(), 1);
    assert!(r.variants[0].selected && r.variants[0].default);

    // next-renderer: the one after dxvk in a D3D9 game's order, written
    // to [platforms.linux] unless --dry-run.
    let next = manage::next_renderer(ID, true).unwrap().unwrap();
    assert!(!l4.exists(), "--dry-run wrote the override file");
    assert_eq!(
        manage::next_renderer(ID, false).unwrap().as_deref(),
        Some(next.as_str())
    );
    let raw = std::fs::read_to_string(&l4).unwrap();
    assert!(
        raw.contains("[platforms.linux]") && raw.contains(&format!("renderer = \"{next}\"")),
        "{raw}"
    );
    let out = setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert_eq!(out.state.renderer, next);

    assert_eq!(next, "wined3d", "a D3D9 game's order: dxvk, then wined3d");
    // `auto` drops it: back to the baked dxvk.
    let out = manage::retarget(
        ID,
        &[("renderer", None)],
        &SetupOptions::default(),
        &mut quiet(),
    )
    .unwrap();
    assert_eq!(out.setup.unwrap().unwrap().state.renderer, "dxvk");
    assert_eq!(conf_value(&w.conf(), "ENV"), "()");

    // --retarget --renderer wined3d: L4, then setup; wined3d chosen by L4
    // sets the switch.
    let out = manage::retarget(
        ID,
        &[("renderer", Some("wined3d"))],
        &SetupOptions::default(),
        &mut quiet(),
    )
    .unwrap();
    assert_eq!(out.changes.len(), 1);
    let s = out.setup.unwrap().unwrap();
    assert_eq!(s.state.renderer, "wined3d");
    assert_eq!(conf_value(&w.conf(), "ENV"), "('PROTON_USE_WINED3D=1')");
    // Again: nothing to change, no setup.
    let out = manage::retarget(
        ID,
        &[("renderer", Some("wined3d"))],
        &SetupOptions::default(),
        &mut quiet(),
    )
    .unwrap();
    assert!(out.changes.is_empty() && out.setup.is_none());

    // A Mac renderer or a bad runtime ref is refused before L4 is written.
    let before = std::fs::read_to_string(&l4).unwrap();
    assert!(
        manage::retarget(
            ID,
            &[("renderer", Some("dxmt"))],
            &SetupOptions::default(),
            &mut quiet()
        )
        .is_err()
    );
    assert!(
        manage::retarget(
            ID,
            &[("runtime", Some("winecx"))],
            &SetupOptions::default(),
            &mut quiet()
        )
        .is_err()
    );
    assert_eq!(std::fs::read_to_string(&l4).unwrap(), before);
    assert_runs_are_clean(&w.log(), "umu-777001");
}

// — gogo apps on Linux —

#[test]
fn apps_lists_and_removes_linux_games_with_their_menu_entry() {
    use gogo::macapp::apps;
    let w = World::new("apps", &["d3d9.dll"]);
    build_with(&w.opts());
    let logs = w.root.join("logs");
    let desktop = gogo::linuxapp::game::desktop_path(ID);
    assert!(
        desktop.starts_with(&w.home),
        "GOGO_HOME keeps the menu entry in scratch"
    );

    // The build wrote the launcher and its menu entry (#177): present.
    let a = &apps::list(&logs).unwrap()[0];
    assert!(a.linux && a.launcher && a.desktop_present && !a.orphaned());
    assert_eq!(
        a.runtime.as_deref(),
        Some(format!("path:{}", w.build.display()).as_str())
    );
    assert_eq!(a.title.as_deref(), Some("Test Game"));

    // A game built before #177 (no launch.sh, no entry) isn't orphaned; a
    // launcher whose entry was deleted is.
    std::fs::rename(w.game().join("launch.sh"), w.root.join("launch.sh")).unwrap();
    std::fs::remove_file(&desktop).unwrap();
    let a = apps::get(ID, &logs).unwrap().unwrap();
    assert!(!a.launcher && !a.desktop_present && !a.orphaned());
    std::fs::rename(w.root.join("launch.sh"), w.game().join("launch.sh")).unwrap();
    assert!(apps::get(ID, &logs).unwrap().unwrap().orphaned());

    // A process in the prefix (found by its WINEPREFIX) blocks removal.
    std::fs::write(&desktop, "[Desktop Entry]\n").unwrap();
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .env("WINEPREFIX", w.game().join("prefix"))
        .spawn()
        .unwrap();
    let a = apps::get(ID, &logs).unwrap().unwrap();
    let err = apps::remove(&a, false).unwrap_err();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(err.to_string().contains("still running"), "{err}");

    // Removed: the game's dir and its menu entry; nothing else of the user's.
    let out = apps::remove(&a, false).unwrap();
    assert!(
        out.paths.contains(&w.game()) && out.paths.contains(&desktop),
        "{:?}",
        out.paths
    );
    assert!(!w.game().exists() && !desktop.exists());
    assert!(apps::list(&logs).unwrap().is_empty());
}

// — content.toml (design 19 §6) —

fn dlc_info(dir: &Path, id: &str, name: &str) {
    std::fs::write(
        dir.join(format!("goggame-{id}.info")),
        format!(
            "{{\"gameId\":\"{id}\",\"rootGameId\":\"{ID}\",\"name\":\"{name}\",\
             \"buildId\":\"4242\",\"playTasks\":[]}}"
        ),
    )
    .unwrap();
}

#[test]
fn a_build_writes_content_toml_and_apps_and_doctor_show_it() {
    use gogo::gog::content;
    use gogo::macapp::{apps, doctor::Level};
    let w = World::new("content", &["d3d9.dll"]);
    dlc_info(&w.tree, "777001", "Extra Pack");
    let opts = LinuxAppOptions {
        dlc_notes: vec!["1 owned DLC not included (--all-dlcs)".into()],
        ..w.opts()
    };
    let plan = build::plan(&opts, None, &mut quiet()).unwrap();
    assert_eq!(plan.dlcs.len(), 1, "the plan lists the DLC in the tree");
    assert_eq!(plan.dlc_notes, opts.dlc_notes);
    let out = build::execute(&plan, None, &mut quiet()).unwrap();

    // Written, atomically (no temp file), by the depot lane.
    let written = content::load(&w.game()).unwrap().expect("content.toml");
    assert_eq!(written, out.content);
    assert_eq!(written.lane, content::Lane::Depot);
    assert_eq!(written.dlc.len(), 1);
    assert_eq!(written.dlc[0].title, "Extra Pack");
    assert_eq!(written.dlc[0].build_id.as_deref(), Some("4242"));
    assert!(!w.game().join("content.toml.tmp").exists());
    // `gogo-launch.toml` doesn't carry it.
    let launch = std::fs::read_to_string(w.game().join("gogo-launch.toml")).unwrap();
    assert!(!launch.contains("Extra Pack"));

    // apps: `dlcs` rows and the record.
    let logs = w.root.join("logs");
    let a = apps::get(ID, &logs).unwrap().unwrap();
    assert_eq!(a.dlcs.len(), 1);
    assert!(!a.content_derived && a.content_error.is_none());
    let j = serde_json::to_value(&a).unwrap();
    assert_eq!(j["dlcs"][0]["title"], "Extra Pack");
    assert_eq!(j["dlcs"][0]["id"], 777001);
    assert_eq!(j["content"]["lane"], "depot");

    // doctor: ok with the list.
    let host = doctor_host(&w, &[("radeon.x86_64", 2), ("radeon.i686", 1)]);
    let dlc = |r: &gogo::macapp::doctor::Report| {
        r.findings
            .iter()
            .find(|f| f.check == "dlc")
            .map(|f| (f.level, f.message.clone()))
            .unwrap()
    };
    let (level, msg) = dlc(&doctor(&w, &host));
    assert!(level == Level::Ok && msg.contains("Extra Pack"), "{msg}");

    // An info added by hand that the record doesn't list: warn.
    dlc_info(&w.game().join("game"), "777002", "Hand Added");
    let (level, msg) = dlc(&doctor(&w, &host));
    assert!(
        level == Level::Warn && msg.contains("Hand Added") && msg.contains("doesn't list"),
        "{msg}"
    );
    // A listed DLC whose info is gone: warn.
    std::fs::remove_file(w.game().join("game/goggame-777002.info")).unwrap();
    std::fs::remove_file(w.game().join("game/goggame-777001.info")).unwrap();
    let (level, msg) = dlc(&doctor(&w, &host));
    assert!(
        level == Level::Warn && msg.contains("Extra Pack") && msg.contains("missing"),
        "{msg}"
    );
}

// — a cache holding more DLC than the build takes (design 19 §3.3, #376) —

const DLC_A: u64 = 1999999902;
const DLC_B: u64 = 1999999903;

/// A depot cache root (the tree's parent) holding the base and DLC A and B. B's
/// manifest spells its dir `Tools/REDmod`, the disk has `tools/redmod`, and B shares
/// one path with the base. Both DLC have a support overlay with their script.
/// The record lists both and keeps the per-product file lists.
fn dlc_cache(w: &World) -> PathBuf {
    let root = w.tree.parent().unwrap().to_path_buf();
    let t = &w.tree;
    let write = |rel: &str, body: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    write("Test Game/data/base.pak", "base");
    write("Test Game/data/shared.cfg", "base's");
    write("Test Game/dlc/a/a.pak", "a");
    write("Test Game/tools/redmod/r.exe", "b");
    write("Test Game/tools/redmod/sub/r.dat", "b");
    dlc_info(t, &DLC_A.to_string(), "Expansion A");
    dlc_info(t, &DLC_B.to_string(), "Mod Tools B");
    write(
        &format!("gog-support/{ID}/{ID}/app/goggame-{ID}.script"),
        "{}",
    );
    write(
        &format!("gog-support/{ID}/{DLC_A}/app/goggame-{DLC_A}.script"),
        "{}",
    );
    write(
        &format!("gog-support/{ID}/{DLC_B}/app/goggame-{DLC_B}.script"),
        "{}",
    );
    gogo::gog::depot::write_record(
        &root,
        "4242",
        "en-US",
        &[DLC_A.to_string(), DLC_B.to_string()],
    )
    .unwrap();
    let mut lists = gogo::gog::dlc_filter::ProductFiles::new();
    lists.insert(
        ID.into(),
        vec![
            "Test Game/game.exe".into(),
            format!("Test Game/goggame-{ID}.info"),
            "Test Game/data/base.pak".into(),
            "Test Game/data/shared.cfg".into(),
            format!("gog-support/{ID}/{ID}/app/goggame-{ID}.script"),
        ],
    );
    lists.insert(
        DLC_A.to_string(),
        vec![
            "Test Game/DLC/A/a.pak".into(),
            format!("Test Game/goggame-{DLC_A}.info"),
            format!("gog-support/{ID}/{DLC_A}/app/goggame-{DLC_A}.script"),
        ],
    );
    lists.insert(
        DLC_B.to_string(),
        vec![
            "Test Game/Tools/REDmod/r.exe".into(),
            "Test Game/Tools/REDmod/sub/r.dat".into(),
            "Test Game/data/shared.cfg".into(),
            format!("Test Game/goggame-{DLC_B}.info"),
            format!("gog-support/{ID}/{DLC_B}/app/goggame-{DLC_B}.script"),
        ],
    );
    gogo::gog::depot::store_products(&root, "4242", &lists).unwrap();
    root
}

/// What the CLI hands the build for `wanted` (its `take_content`), from the record.
fn content_for(root: &Path, w: &World, wanted: &[u64]) -> build::DepotContent {
    use gogo::gog::dlc_filter::{self, Need};
    let titles = |id: u64| {
        if id == DLC_A {
            "Expansion A"
        } else {
            "Mod Tools B"
        }
    };
    let dlcs = wanted
        .iter()
        .map(|id| gogo::gog::content::DlcRef {
            id: *id,
            title: titles(*id).into(),
        })
        .collect();
    let filter = match dlc_filter::need(root, &w.tree, ID, wanted, "en-US") {
        Need::Whole => None,
        Need::Filter {
            extra,
            products,
            build_id,
            ..
        } => {
            let lists =
                gogo::gog::depot::record_products(root, build_id.as_deref().unwrap()).unwrap();
            let keeping: Vec<String> = products
                .into_iter()
                .filter(|p| !extra.iter().any(|d| d.id.to_string() == *p))
                .collect();
            Some(dlc_filter::filter(&lists, &keeping, &extra).unwrap())
        }
    };
    build::DepotContent {
        dlcs,
        language: None,
        version: None,
        filter,
    }
}

fn ids(c: &gogo::gog::content::Content) -> Vec<u64> {
    c.dlc.iter().map(|d| d.id).collect()
}

#[test]
fn a_build_takes_only_the_wanted_dlc_from_a_cache_holding_more() {
    use gogo::gog::content;
    let w = World::new("dlc-filter", &["d3d9.dll"]);
    let root = dlc_cache(&w);
    let game = |rel: &str| w.game().join("game").join(rel);
    let build = |wanted: &[u64]| {
        let opts = LinuxAppOptions {
            depot_content: Some(content_for(&root, &w, wanted)),
            ..w.opts()
        };
        let plan = build::plan(&opts, None, &mut quiet()).unwrap();
        assert_eq!(
            plan.dlcs.iter().map(|d| d.id).collect::<Vec<_>>(),
            wanted,
            "the plan names the wanted DLC, not what the cache holds"
        );
        let mut warnings = Vec::new();
        let out = build::execute(&plan, None, &mut |e| {
            if let ProgressEvent::Warn { message } = e {
                warnings.push(message.clone());
            }
        })
        .unwrap();
        (out, warnings)
    };

    // --no-dlcs: the base only.
    let (out, warnings) = build(&[]);
    assert!(game("data/base.pak").is_file() && game("game.exe").is_file());
    assert_eq!(
        std::fs::read_to_string(game("data/shared.cfg")).unwrap(),
        "base's",
        "a path the base shares with a left-out DLC stays"
    );
    assert!(!game("dlc").exists(), "DLC A's dir isn't created");
    assert!(
        !game("tools").exists(),
        "B's dirs, cased otherwise on disk, aren't either"
    );
    assert!(!game(&format!("goggame-{DLC_A}.info")).exists());
    assert!(!game(&format!("goggame-{DLC_B}.script")).exists());
    assert!(
        game(&format!("goggame-{ID}.script")).is_file(),
        "the base's overlay merges"
    );
    assert!(ids(&out.content).is_empty());
    assert!(ids(&content::load(&w.game()).unwrap().unwrap()).is_empty());
    let m = out.materialized.as_ref().unwrap();
    assert_eq!(m.left_out_files, 5, "{m:?}");
    assert!(
        warnings
            .iter()
            .any(|m| m.contains("Expansion A") && m.contains("Mod Tools B")),
        "{warnings:?}"
    );

    // --dlc A: the base and A, with A's support files.
    let (out, _) = build(&[DLC_A]);
    assert!(game("dlc/a/a.pak").is_file());
    assert!(game(&format!("goggame-{DLC_A}.info")).is_file());
    assert!(game(&format!("goggame-{DLC_A}.script")).is_file());
    assert!(!game("tools").exists());
    assert!(!game(&format!("goggame-{DLC_B}.info")).exists());
    assert_eq!(ids(&out.content), [DLC_A]);
    assert_eq!(out.content.dlc[0].title, "Expansion A");

    // --all-dlcs: nothing to leave out, the cache is taken whole.
    let opts = content_for(&root, &w, &[DLC_A, DLC_B]);
    assert!(opts.filter.is_none());
    let (out, _) = build(&[DLC_A, DLC_B]);
    assert!(game("tools/redmod/sub/r.dat").is_file());
    assert!(game(&format!("goggame-{DLC_B}.script")).is_file());
    assert_eq!(ids(&out.content), [DLC_A, DLC_B]);

    // A rebuild with the recorded set (A) drops B again.
    let recorded: Vec<u64> = ids(&content::load(&w.game()).unwrap().unwrap());
    assert_eq!(recorded, [DLC_A, DLC_B]);
    let (out, _) = build(&[DLC_A]);
    assert!(!game("tools").exists());
    assert_eq!(ids(&out.content), [DLC_A]);
    assert_eq!(
        content::mismatches(&out.content, &w.game().join("game")),
        (Vec::new(), Vec::new()),
        "what's recorded is what's in game/"
    );
}

#[test]
fn a_cache_without_file_lists_gives_no_filter() {
    use gogo::gog::dlc_filter::{self, Need};
    let w = World::new("dlc-filter-none", &["d3d9.dll"]);
    let root = w.tree.parent().unwrap().to_path_buf();
    dlc_info(&w.tree, &DLC_B.to_string(), "Mod Tools B");
    // An older record: no DLC, no lists. The info says B is there.
    std::fs::write(
        root.join(gogo::gog::depot::RECORD),
        r#"{"buildId":"4242","files":[]}"#,
    )
    .unwrap();
    let Need::Filter {
        extra,
        products,
        build_id,
        ..
    } = dlc_filter::need(&root, &w.tree, ID, &[], "en-US")
    else {
        panic!("B is extra");
    };
    assert_eq!(build_id.as_deref(), Some("4242"));
    assert!(gogo::gog::depot::record_products(&root, "4242").is_none());
    // Without lists the CLI refuses (`take_content`): no filter can be built.
    let lacking = dlc_filter::filter(&Default::default(), &products[..1], &extra).unwrap_err();
    assert_eq!(lacking, [ID.to_string(), DLC_B.to_string()]);
}

/// The build's depots in two languages (#389): `*` files, one depot each for en-US
/// and de-DE, and one depot both list (one manifest), so a path the two share.
fn language_depots() -> Vec<gogo::gog::dlc_filter::DepotList> {
    use gogo::gog::dlc_filter::DepotList;
    let d = |languages: &[&str], manifest: &str, paths: &[&str]| DepotList {
        product: ID.into(),
        languages: languages.iter().map(|l| l.to_string()).collect(),
        manifest: manifest.into(),
        paths: paths.iter().map(|p| format!("Test Game/{p}")).collect(),
        sizes: Default::default(),
    };
    vec![
        d(
            &["*"],
            "neutral",
            &["game.exe", &format!("goggame-{ID}.info"), "data/base.pak"],
        ),
        d(&["en-US"], "en", &["lang/en.pak", "Sound/EN/voice.pak"]),
        d(&["de-DE"], "de", &["lang/de.pak", "Sound/DE/voice.pak"]),
        d(&["en-US", "de-DE"], "both", &["lang/common.pak"]),
    ]
}

/// What `gogo depot` leaves after a download in `language`: its files written, the
/// record, then the record settled from the build's depots.
fn fetch_language(root: &Path, language: &str, was_whole: &[&str]) {
    let write = |rel: &str| {
        let p = root.join("Test Game").join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, rel).unwrap();
    };
    write("data/base.pak");
    write("lang/common.pak");
    if language == "en-US" {
        write("lang/en.pak");
        write("Sound/EN/voice.pak");
    } else {
        write("lang/de.pak");
        write("Sound/DE/voice.pak");
    }
    gogo::gog::depot::write_record(root, "4242", language, &[]).unwrap();
    let was: Vec<String> = was_whole.iter().map(|l| l.to_string()).collect();
    gogo::gog::depot::settle_languages(root, "4242", &was, &[ID.into()], &language_depots())
        .unwrap();
}

/// What the CLI hands a build in `language` (its `take_content`), from the record
/// only: no fetch.
fn language_content(root: &Path, w: &World, language: &str) -> build::DepotContent {
    use gogo::gog::dlc_filter::{self, Need};
    let filter = match dlc_filter::need(root, &w.tree, ID, &[], language) {
        Need::Whole => None,
        Need::Filter {
            products,
            build_id,
            languages,
            extra,
            ..
        } => {
            let b = build_id.unwrap();
            let lists = gogo::gog::depot::record_products_in(root, &b, language).unwrap();
            let others: Vec<_> = languages
                .iter()
                .map(|l| {
                    (
                        l.clone(),
                        gogo::gog::depot::record_products_in(root, &b, l).unwrap(),
                    )
                })
                .collect();
            Some(dlc_filter::filter_languages(&lists, &products, &extra, &others).unwrap())
        }
    };
    build::DepotContent {
        dlcs: Vec::new(),
        language: Some(language.into()),
        version: None,
        filter,
    }
}

#[test]
fn a_build_takes_only_its_language_from_a_cache_holding_two() {
    use gogo::gog::{content, depot};
    let w = World::new("language-filter", &["d3d9.dll"]);
    let root = w.tree.parent().unwrap().to_path_buf();
    let game = |rel: &str| w.game().join("game").join(rel);
    let want = |language| depot::CacheWant {
        build_id: Some("4242"),
        language: Some(language),
        dlcs: &[],
    };
    // Downloaded in en-US, then switched to de-DE: nothing deleted.
    fetch_language(&root, "en-US", &[]);
    fetch_language(&root, "de-DE", &["en-US"]);
    assert!(root.join("Test Game/lang/en.pak").is_file(), "never pruned");
    let rec = depot::read_record(&root).unwrap();
    assert_eq!(rec.language.as_deref(), Some("de-DE"), "the last fetched");
    assert_eq!(rec.languages(), ["de-DE", "en-US"]);
    // Switching back fetches nothing: en-US is held whole.
    assert_eq!(
        depot::check_cache(&root, &want("en-US")),
        depot::CacheCheck::Complete
    );
    assert_eq!(
        depot::check_cache(&root, &want("fr-FR")),
        depot::CacheCheck::OtherLanguage
    );

    let build = |language: &str| {
        let mut dc = language_content(&root, &w, language);
        // The depot build's version name reaches content.toml (#418).
        dc.version = Some((depot::read_record(&root).unwrap().build_id, "2.31".into()));
        let opts = LinuxAppOptions {
            depot_content: Some(dc),
            ..w.opts()
        };
        let plan = build::plan(&opts, None, &mut quiet()).unwrap();
        let mut warnings = Vec::new();
        let out = build::execute(&plan, None, &mut |e| {
            if let ProgressEvent::Warn { message } = e {
                warnings.push(message.clone());
            }
        })
        .unwrap();
        (out, warnings)
    };
    for (language, ours, theirs) in [
        (
            "de-DE",
            ["lang/de.pak", "Sound/DE/voice.pak"],
            "lang/en.pak",
        ),
        (
            "en-US",
            ["lang/en.pak", "Sound/EN/voice.pak"],
            "lang/de.pak",
        ),
    ] {
        let (out, warnings) = build(language);
        for f in ours {
            assert!(game(f).is_file(), "{language}: {f}");
        }
        assert!(!game(theirs).exists(), "{language}: {theirs} left out");
        assert!(
            !game("Sound").read_dir().unwrap().any(|e| {
                let n = e.unwrap().file_name();
                !ours.iter().any(|o| o.contains(&*n.to_string_lossy()))
            }),
            "{language}: only its own voice dir"
        );
        assert!(game("lang/common.pak").is_file(), "a shared path stays");
        assert!(game("data/base.pak").is_file() && game("game.exe").is_file());
        let m = out.materialized.as_ref().unwrap();
        assert_eq!(m.left_out_files, 2, "{language}: {m:?}");
        assert!(
            warnings.iter().any(|m| m.contains("languages")),
            "{warnings:?}"
        );
        // content.toml says the build's language; the record still names the last
        // one fetched and holds both.
        assert_eq!(out.content.language.as_deref(), Some(language));
        let file = content::load(&w.game()).unwrap().unwrap();
        assert_eq!(file.language.as_deref(), Some(language));
        assert_eq!(file.version.as_deref(), Some("2.31"));
        let rec = depot::read_record(&root).unwrap();
        assert_eq!(rec.language.as_deref(), Some("de-DE"));
        assert!(rec.languages().iter().any(|l| l == language));
    }

    // Switched back: de-DE is the other language now, whole, and builds the same.
    fetch_language(&root, "en-US", &["de-DE"]);
    assert_eq!(
        depot::check_cache(&root, &want("de-DE")),
        depot::CacheCheck::Complete
    );
    let (out, _) = build("de-DE");
    assert!(game("lang/de.pak").is_file() && !game("lang/en.pak").exists());
    assert_eq!(out.content.language.as_deref(), Some("de-DE"));
}

/// [`language_depots`] with the sizes [`fetch_language`]'s files have.
fn sized_language_depots() -> Vec<gogo::gog::dlc_filter::DepotList> {
    let mut d = language_depots();
    for l in &mut d {
        l.sizes = l
            .paths
            .iter()
            .map(|p| {
                let rel = p.strip_prefix("Test Game/").unwrap();
                (p.clone(), rel.len() as u64)
            })
            .collect();
    }
    d
}

/// A cache an older gogo switched from en-US to de-DE (#391): both languages' files
/// there, the record naming de-DE only. The scan before a build finds en-US, once,
/// and the build leaves its files out; nothing in the cache is touched.
#[test]
fn a_cache_switched_by_an_older_gogo_gets_its_old_language_recorded() {
    use gogo::gog::{cache_upgrade, depot};
    let w = World::new("language-scan", &["d3d9.dll"]);
    let root = w.tree.parent().unwrap().to_path_buf();
    let game = |rel: &str| w.game().join("game").join(rel);
    fetch_language(&root, "en-US", &[]);
    fetch_language(&root, "de-DE", &[]);
    // What a gogo from before #389 left: de-DE only, no other languages, not scanned.
    let rec_path = root.join(depot::RECORD);
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&rec_path).unwrap()).unwrap();
    v.as_object_mut().unwrap().remove("otherLanguages");
    v.as_object_mut().unwrap().remove("languagesScanned");
    std::fs::write(&rec_path, v.to_string()).unwrap();
    assert_eq!(depot::read_record(&root).unwrap().languages(), ["de-DE"]);
    assert!(depot::languages_unscanned(&root, "4242"));

    let build = |language: &str| {
        let opts = LinuxAppOptions {
            depot_content: Some(language_content(&root, &w, language)),
            ..w.opts()
        };
        let plan = build::plan(&opts, None, &mut quiet()).unwrap();
        build::execute(&plan, None, &mut |_| {}).unwrap()
    };
    // Before the scan, en-US's files go into game/ (the bug).
    build("de-DE");
    assert!(game("lang/en.pak").is_file());

    // A fetch failure: a warning, the record byte for byte as it was, and the build
    // goes on as before.
    let before = std::fs::read(&rec_path).unwrap();
    let lines =
        cache_upgrade::bring_up_to_date(&root, "Test", "de-DE", "4242", Err("offline".into()));
    assert!(
        lines.len() == 1 && lines[0].contains("couldn't be looked for: offline"),
        "{lines:?}"
    );
    assert_eq!(std::fs::read(&rec_path).unwrap(), before);
    build("de-DE");
    assert!(game("lang/en.pak").is_file());

    // Every file of the cache but its record, with its bytes.
    let files = || {
        fn go(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    go(&p, out);
                } else if e.file_name() != depot::RECORD {
                    out.push((p.clone(), std::fs::read(&p).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        go(&root, &mut out);
        out.sort();
        out
    };
    let tree_before = files();
    let fetched = || cache_upgrade::Fetched {
        base: ID.into(),
        tree_name: "Test Game".into(),
        depots: sized_language_depots(),
    };
    let lines = cache_upgrade::bring_up_to_date(&root, "Test", "de-DE", "4242", Ok(fetched()));
    assert!(
        lines.iter().any(|l| l.contains("also holds en-US from")),
        "{lines:?}"
    );
    assert_eq!(files(), tree_before, "only the record is written");
    let rec = depot::read_record(&root).unwrap();
    assert_eq!(rec.languages(), ["de-DE", "en-US"]);
    assert!(!depot::languages_unscanned(&root, "4242"));
    // Whole: the one path the two share comes through one manifest.
    assert_eq!(
        depot::check_cache(
            &root,
            &depot::CacheWant {
                build_id: Some("4242"),
                language: Some("en-US"),
                dlcs: &[],
            }
        ),
        depot::CacheCheck::Complete
    );
    // Scanned once: no fetch is needed again, and a later record keeps the flag.
    assert!(
        cache_upgrade::bring_up_to_date(&root, "Test", "de-DE", "4242", Err("offline".into()))
            .is_empty()
    );
    depot::write_record(&root, "4242", "de-DE", &[]).unwrap();
    assert!(!depot::languages_unscanned(&root, "4242"));

    let out = build("de-DE");
    assert!(game("lang/de.pak").is_file() && game("Sound/DE/voice.pak").is_file());
    assert!(!game("lang/en.pak").exists() && !game("Sound/EN").exists());
    assert!(game("lang/common.pak").is_file(), "a shared path stays");
    assert_eq!(out.materialized.as_ref().unwrap().left_out_files, 2);
}

/// Only some of an unrecorded language's files there (#391): not recorded, so its
/// files go into game/ as before; and a wrong size counts as not there.
#[test]
fn a_partly_held_language_is_not_recorded() {
    use gogo::gog::{cache_upgrade, depot};
    let w = World::new("language-scan-partial", &["d3d9.dll"]);
    let root = w.tree.parent().unwrap().to_path_buf();
    fetch_language(&root, "de-DE", &[]);
    let en = root.join("Test Game/lang/en.pak");
    std::fs::write(&en, "lang/en.pak").unwrap();
    let fetched =
        |root: &Path| cache_upgrade::scan_languages(root, ID, &sized_language_depots()).unwrap();
    let scan = fetched(&root);
    assert!(scan.found.is_empty(), "{scan:?}");
    assert!(
        scan.notes.iter().any(|n| n.contains("en-US: 1 of its 2")),
        "{:?}",
        scan.notes
    );
    // Every file there, one of the wrong size.
    let voice = root.join("Test Game/Sound/EN/voice.pak");
    std::fs::create_dir_all(voice.parent().unwrap()).unwrap();
    std::fs::write(&voice, "not the size").unwrap();
    assert!(fetched(&root).found.is_empty());
    std::fs::write(&voice, "Sound/EN/voice.pak").unwrap();
    assert_eq!(fetched(&root).found[0].language, "en-US");

    // Recorded as scanned with nothing found: the filter leaves the cache whole.
    std::fs::remove_file(&voice).unwrap();
    let lines = cache_upgrade::bring_up_to_date(
        &root,
        "Test",
        "de-DE",
        "4242",
        Ok(cache_upgrade::Fetched {
            base: ID.into(),
            tree_name: "Test Game".into(),
            depots: sized_language_depots(),
        }),
    );
    assert!(lines.iter().all(|l| !l.contains("also holds")), "{lines:?}");
    assert_eq!(depot::read_record(&root).unwrap().languages(), ["de-DE"]);
    assert!(!depot::languages_unscanned(&root, "4242"));
    let opts = LinuxAppOptions {
        depot_content: Some(language_content(&root, &w, "de-DE")),
        ..w.opts()
    };
    let plan = build::plan(&opts, None, &mut quiet()).unwrap();
    build::execute(&plan, None, &mut |_| {}).unwrap();
    assert!(
        w.game().join("game/lang/en.pak").is_file(),
        "kept, as before"
    );
}

#[test]
fn an_older_record_holds_its_one_language() {
    use gogo::gog::{depot, dlc_filter};
    let w = World::new("language-old-record", &["d3d9.dll"]);
    let root = w.tree.parent().unwrap().to_path_buf();
    // A record from before #389: one language, its lists, no other languages.
    std::fs::write(
        root.join(depot::RECORD),
        format!(
            r#"{{"buildId":"4242","language":"en-US","dlcs":[],"products":{{"{ID}":["Test Game/game.exe"]}},"files":["Test Game/game.exe"]}}"#
        ),
    )
    .unwrap();
    let rec = depot::read_record(&root).unwrap();
    assert_eq!(rec.languages(), ["en-US"]);
    assert_eq!(
        dlc_filter::need(&root, &w.tree, ID, &[], "en-US"),
        dlc_filter::Need::Whole
    );
    assert_eq!(
        depot::check_cache(
            &root,
            &depot::CacheWant {
                build_id: Some("4242"),
                language: Some("de-DE"),
                dlcs: &[],
            }
        ),
        depot::CacheCheck::OtherLanguage
    );
    // A switch keeps it as another language, with its lists, not whole until settled.
    depot::write_record(&root, "4242", "de-DE", &[]).unwrap();
    let rec = depot::read_record(&root).unwrap();
    assert_eq!(rec.languages(), ["de-DE", "en-US"]);
    assert!(depot::record_products_in(&root, "4242", "en-US").is_some());
    assert!(depot::record_products_in(&root, "4242", "de-DE").is_none());
    assert_eq!(
        depot::check_cache(
            &root,
            &depot::CacheWant {
                build_id: Some("4242"),
                language: Some("en-US"),
                dlcs: &[],
            }
        ),
        depot::CacheCheck::OtherLanguage,
        "not whole: the switch may have written a path they share"
    );
    // Offline, with de-DE's lists missing, the filter can't be built: refused.
    let Some(en) = depot::record_products_in(&root, "4242", "en-US") else {
        unreachable!()
    };
    let lacking = dlc_filter::filter_languages(
        &Default::default(),
        &[ID.to_string()],
        &[],
        &[("en-US".into(), en)],
    )
    .unwrap_err();
    assert_eq!(lacking, [ID.to_string()]);
}

#[test]
fn a_game_without_content_toml_is_derived_from_its_infos_and_nothing_is_written() {
    use gogo::macapp::{apps, doctor::Level};
    let w = World::new("content-derived", &["d3d9.dll"]);
    build_with(&w.opts());
    let file = w.game().join("content.toml");
    std::fs::remove_file(&file).unwrap();
    dlc_info(&w.game().join("game"), "777001", "Extra Pack");

    let logs = w.root.join("logs");
    let a = apps::get(ID, &logs).unwrap().unwrap();
    assert!(a.content_derived);
    assert_eq!(a.dlcs.len(), 1);
    assert_eq!(a.dlcs[0].title, "Extra Pack");
    assert!(!file.exists(), "reading doesn't write the file");

    let host = doctor_host(&w, &[("radeon.x86_64", 2), ("radeon.i686", 1)]);
    let r = doctor(&w, &host);
    let f = r.findings.iter().find(|f| f.check == "dlc").unwrap();
    assert_eq!(f.level, Level::Ok);
    assert!(f.message.contains("derived"), "{}", f.message);
    assert!(!file.exists());

    // A file this gogo can't read is a warning, not a crash.
    std::fs::write(&file, "schema = [").unwrap();
    let a = apps::get(ID, &logs).unwrap().unwrap();
    assert!(a.content_error.is_some() && a.dlcs.is_empty());
    let r = doctor(&w, &host);
    let f = r.findings.iter().find(|f| f.check == "dlc").unwrap();
    assert_eq!(f.level, Level::Warn);
}

// — refusals —

#[test]
fn a_prefix_proton_never_made_fails_the_build() {
    // What the real smoke hit: the container didn't start, umu's setup_pfx
    // had already made drive_c/users. Not a prefix: the build stops.
    let w = World::new("broken", &["d3d9.dll"]);
    // SAFETY: under ENV_LOCK (`w` holds it).
    unsafe { std::env::set_var("FAKE_UMU_BROKEN", "1") };
    let plan = build::plan(&w.opts(), None, &mut quiet()).unwrap();
    let err = build::execute(&plan, None, &mut quiet()).unwrap_err();
    unsafe { std::env::remove_var("FAKE_UMU_BROKEN") };
    assert!(
        err.to_string().contains("didn't create the prefix"),
        "{err}"
    );
    assert!(!w.game().join("launch.conf").exists());
}

#[test]
fn headless_without_umu_set_fails_naming_the_key() {
    let w = World::new("headless", &["d3d9.dll"]);
    std::fs::write(w.home.join("config.toml"), "").unwrap();
    let err = build::plan(&w.opts(), None, &mut quiet()).unwrap_err();
    assert!(err.to_string().contains("[linux] umu"), "{err}");
    assert!(w.log().is_empty(), "nothing ran");
}

#[test]
fn tmp_paths_never_reach_umu() {
    let w = World::new("tmp", &["d3d9.dll"]);
    // A Proton build under /tmp: refused before umu runs.
    let tmp = tempfile::Builder::new().tempdir_in("/tmp").unwrap();
    let build_dir = tmp.path().join("GE-Proton10-34");
    std::fs::create_dir_all(&build_dir).unwrap();
    for f in ["proton", "version", "toolmanifest.vdf"] {
        std::fs::copy(w.build.join(f), build_dir.join(f)).unwrap();
    }
    let mut opts = w.opts();
    opts.runtime = Some(format!("path:{}", build_dir.display()));
    let plan = build::plan(&opts, None, &mut quiet()).unwrap();
    let err = build::execute(&plan, None, &mut quiet()).unwrap_err();
    assert!(err.to_string().contains("private /tmp"), "{err}");
    assert!(w.log().is_empty(), "umu ran:\n{}", w.log());

    // gogo's data dir under /tmp: refused at plan time.
    // SAFETY: under ENV_LOCK (`w` holds it); `w`'s drop restores GOGO_HOME.
    unsafe { std::env::set_var("GOGO_HOME", tmp.path().join("home")) };
    let err = build::plan(&w.opts(), None, &mut quiet()).unwrap_err();
    assert!(err.to_string().contains("private /tmp"), "{err}");
}

// — L1 renderer selection fixtures —

// — the launcher (#177, design 18 §7) —
//
// The build writes `launch.sh`, then umu-run on PATH is swapped for a fake
// game that logs its argv, the env and its working dir, prints a line on
// each stream and exits as told. A fake gogo answers `setup` the way the
// real one leaves `launch.conf` (restamped) and `next-renderer` as #176's.
// The launcher runs with a cleared environment and a PATH of exactly the
// tools it needs, so no dialog tool, clipboard or gogo of the host's is
// ever reached.

/// The fake game (see above). `FAKE_GAME_HANG`: children that ignore TERM
/// in sessions of their own (as Wine's services and umu's container leave
/// the process group), marked as in the container (`WINEPREFIX=<prefix>`,
/// `<prefix>/pfx/`, or only `STEAM_COMPAT_DATA_PATH`), then hang.
const FAKE_GAME_UMU: &str = r#"#!/bin/sh
{
  printf 'ARGV:%s\n' "$*"
  printf 'ENV:WINEPREFIX=%s GAMEID=%s STORE=%s PROTONPATH=%s WINEDLLOVERRIDES=%s PROTONFIXES_DISABLE=%s FOO=%s GAME_DRIVE=%s CWD=%s\n' \
    "${WINEPREFIX-unset}" "${GAMEID-}" "${STORE-}" "${PROTONPATH-}" "${WINEDLLOVERRIDES-}" \
    "${PROTONFIXES_DISABLE-}" "${FOO-}" "${PROTON_SET_GAME_DRIVE-}" "$(pwd)"
  printf 'GAME_SHARED:%s\n' "${PRESSURE_VESSEL_FILESYSTEMS_RW-}"
} >> "$FAKE_UMU_LOG"
if [ -n "${FAKE_ARGS_LOG:-}" ]; then
  : > "$FAKE_ARGS_LOG"
  for a in "$@"; do printf '%s\n' "$a" >> "$FAKE_ARGS_LOG"; done
fi
if [ -n "${FAKE_ENV_DUMP:-}" ]; then env > "$FAKE_ENV_DUMP"; fi
echo "fake-game stdout line"
echo "err: fake-game stderr line" >&2
if [ -n "${FAKE_GAME_HANG:-}" ]; then
  : > "$FAKE_UMU_LOG.pids"
  setsid sh -c 'trap "" TERM; exec sleep 300' </dev/null >/dev/null 2>&1 &
  echo $! >> "$FAKE_UMU_LOG.pids"
  WINEPREFIX="$WINEPREFIX/pfx/" setsid sh -c 'trap "" TERM; exec sleep 300' </dev/null >/dev/null 2>&1 &
  echo $! >> "$FAKE_UMU_LOG.pids"
  STEAM_COMPAT_DATA_PATH="$WINEPREFIX" setsid env -u WINEPREFIX sh -c 'trap "" TERM; exec sleep 300' </dev/null >/dev/null 2>&1 &
  echo $! >> "$FAKE_UMU_LOG.pids"
  echo $$ >> "$FAKE_UMU_LOG.pids"
  # Wine's server: the process the launcher's close request looks for.
  # It must wait without forking: a child (an inherited WINEPREFIX) born
  # between the launcher's listing and its KILL would outlive the sweep.
  # `read -t` on a held-open FIFO is a builtin, so no child is ever made.
  mkfifo "$FAKE_UMU_LOG.fifo"
  setsid "$FAKE_WS_DIR/wineserver" -c 'trap "" TERM; exec 3<>"$1"; while :; do read -t 300 -u 3; done' wineserver "$FAKE_UMU_LOG.fifo" </dev/null >/dev/null 2>&1 &
  echo $! >> "$FAKE_UMU_LOG.pids"
  exec sleep 300
fi
if [ -n "${FAKE_GAME_TERM_OK:-}" ]; then
  trap 'exit 0' TERM
  for _ in $(seq 100); do sleep 0.1; done
fi
if [ -n "${FAKE_GAME_TRAP_TERM:-}" ]; then
  trap 'echo GOT_TERM >> "$FAKE_UMU_LOG"; exit 143' TERM
  echo TRAPPING >> "$FAKE_UMU_LOG"
  while :; do sleep 0.1; done
fi
sleep "${FAKE_GAME_SLEEP:-0}"
if [ -n "${FAKE_GAME_SIGNAL:-}" ]; then kill -s "$FAKE_GAME_SIGNAL" $$; sleep 5; fi
exit "${FAKE_GAME_EXIT:-0}"
"#;

/// The fake gogo. `setup` restamps `launch.conf` (and clears a vanished
/// `UMU_PYTHON`, and puts `FAKE_VERSION` back into `FAKE_VERSION_FILE`, as
/// a reinstall would); `FAKE_SETUP_FAIL` fails it. `next-renderer` answers
/// wined3d until L4 names it.
const FAKE_GOGO: &str = r#"#!/bin/sh
echo "GOGO_HOME=${GOGO_HOME:-} $*" >> "$FAKE_CALLS"
g="$GOGO_HOME/games/$3"
if [ -f "$GOGO_HOME/game-locations/$3" ]; then g="$(cat "$GOGO_HOME/game-locations/$3")"; fi
l4="$GOGO_HOME/compat/$3.toml"
case "$2" in
  setup)
    [ -n "${FAKE_SETUP_FAIL:-}" ] && { echo "warning: a warning"; echo "gogo: setup broke" >&2; exit 1; }
    if [ -n "${FAKE_VERSION_FILE:-}" ]; then printf '%s\n' "$FAKE_VERSION" > "$FAKE_VERSION_FILE"; fi
    new="$(cat "$g/gogo-launch.toml" "$l4" 2>/dev/null | sha256sum)"
    sed -e "s|^STAMP=.*|STAMP='${new%% *}'|" -e "s|^UMU_PYTHON=.*|UMU_PYTHON=''|" \
      "$g/launch.conf" > "$g/launch.conf.new"
    if grep -q wined3d "$l4" 2>/dev/null; then
      sed -i "s|^RENDERER=.*|RENDERER='wined3d'|" "$g/launch.conf.new"
    fi
    mv "$g/launch.conf.new" "$g/launch.conf";;
  next-renderer)
    if grep -q wined3d "$l4" 2>/dev/null; then echo "gogo: $3: no more renderers" >&2; exit 1; fi
    [ "${4:-}" = --dry-run ] || { mkdir -p "$GOGO_HOME/compat"; printf '[platforms.linux]\nrenderer = "wined3d"\n' > "$l4"; }
    echo wined3d;;
esac
"#;

/// What `launch.sh` and the fakes run, linked from the test's PATH.
const LAUNCHER_TOOLS: &[&str] = &[
    "bash",
    "timeout",
    "sh",
    "cat",
    "date",
    "mkdir",
    "rm",
    "rmdir",
    "ln",
    "mv",
    "cp",
    "sha256sum",
    "grep",
    "sed",
    "awk",
    "tr",
    "cut",
    "tail",
    "head",
    "sleep",
    "tee",
    "mkfifo",
    "basename",
    "dirname",
    "readlink",
    "uname",
    "setsid",
    "printf",
    "env",
    "flock",
];

/// A 16×16 `.ico` (one 32 bpp DIB, opaque red).
fn tiny_ico() -> Vec<u8> {
    let mut dib = Vec::new();
    dib.extend_from_slice(&40u32.to_le_bytes());
    dib.extend_from_slice(&16u32.to_le_bytes());
    dib.extend_from_slice(&32u32.to_le_bytes());
    dib.extend_from_slice(&1u16.to_le_bytes());
    dib.extend_from_slice(&32u16.to_le_bytes());
    dib.extend_from_slice(&[0; 24]);
    for _ in 0..256 {
        dib.extend_from_slice(&[0, 0, 255, 255]);
    }
    dib.extend_from_slice(&[0; 64]);
    let mut ico = vec![0, 0, 1, 0, 1, 0, 16, 16, 0, 0, 1, 0, 32, 0];
    ico.extend_from_slice(&(dib.len() as u32).to_le_bytes());
    ico.extend_from_slice(&22u32.to_le_bytes());
    ico.extend_from_slice(&dib);
    ico
}

impl World {
    /// A built game whose launcher can run: GOG's `.ico` in the tree, the
    /// fake gogo as `GOGO_PATH` (`no_gogo`: a path that isn't there), the
    /// fake game as umu-run, and the tools dir. `None` (skipped, said on
    /// stderr) where the launcher can't run: Nix's build sandbox has no
    /// `/usr/bin/env` for its shebang and no `setsid`.
    fn launcher(name: &str, no_gogo: bool) -> Option<World> {
        World::launcher_with(name, no_gogo, false)
    }

    fn launcher_with(name: &str, no_gogo: bool, library: bool) -> Option<World> {
        World::launcher_prep(name, no_gogo, library, &|_| {})
    }

    /// [`World::launcher_with`], with `prep` changing the game tree before
    /// the build.
    fn launcher_prep(
        name: &str,
        no_gogo: bool,
        library: bool,
        prep: &dyn Fn(&World),
    ) -> Option<World> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let found: Vec<Option<PathBuf>> = LAUNCHER_TOOLS
            .iter()
            .map(|t| {
                std::env::split_paths(&path)
                    .map(|d| d.join(t))
                    .find(|p| p.is_file())
            })
            .collect();
        if !Path::new("/usr/bin/env").is_file() || found.iter().any(Option::is_none) {
            eprintln!("no /usr/bin/env or a launcher tool missing from PATH: skipped");
            return None;
        }
        let w = World::launcher_built_prep(name, no_gogo, library, prep);
        let tools = w.root.join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        for (t, p) in LAUNCHER_TOOLS.iter().zip(found) {
            std::os::unix::fs::symlink(p.unwrap(), tools.join(t)).unwrap();
        }
        Some(w)
    }

    /// [`World::launcher`]'s game without the tools: for what setup writes.
    fn launcher_built(name: &str, no_gogo: bool) -> World {
        World::launcher_built_with(name, no_gogo, false)
    }

    fn launcher_built_with(name: &str, no_gogo: bool, library: bool) -> World {
        World::launcher_built_prep(name, no_gogo, library, &|_| {})
    }

    fn launcher_built_prep(
        name: &str,
        no_gogo: bool,
        library: bool,
        prep: &dyn Fn(&World),
    ) -> World {
        let w = World::new(name, &["d3d9.dll"]);
        prep(&w);
        std::fs::write(w.tree.join(format!("goggame-{ID}.ico")), tiny_ico()).unwrap();
        let gogo = w
            .root
            .join(if no_gogo { "no-such-gogo" } else { "fake-gogo" });
        if !no_gogo {
            std::fs::write(&gogo, FAKE_GOGO).unwrap();
            chmod_x(&gogo);
        }
        let mut opts = if library {
            w.opts_in_library()
        } else {
            w.opts()
        };
        opts.gogo_path = Some(gogo);
        build_with(&opts);
        let umu = w.root.join("bin/umu-run");
        std::fs::write(&umu, FAKE_GAME_UMU).unwrap();
        chmod_x(&umu);
        // The close request's pieces: `nsenter` that just runs the command
        // (no namespaces here), and a "wineserver" with a `wine` beside it.
        let nsenter = w.root.join("bin/nsenter");
        std::fs::write(
            &nsenter,
            "#!/bin/sh\nwhile [ \"$1\" != -- ]; do shift; done\nshift\nexec \"$@\"\n",
        )
        .unwrap();
        chmod_x(&nsenter);
        let ws = w.root.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let bash = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|d| d.join("bash"))
            .find(|p| p.is_file())
            .unwrap();
        std::fs::copy(bash, ws.join("wineserver")).unwrap();
        std::fs::write(
            ws.join("wine"),
            "#!/bin/sh\n\
             printf 'CLOSE:%s\\n' \"$*\" >> \"$FAKE_UMU_LOG\"\n\
             if [ -n \"${FAKE_CLOSE_WORKS:-}\" ]; then kill -KILL $(cat \"$FAKE_UMU_LOG.pids\") 2>/dev/null; fi\n",
        )
        .unwrap();
        chmod_x(&ws.join("wine"));
        w
    }

    /// The fake game's runs (the build's umu runs are in the same log).
    fn game_runs(&self) -> usize {
        self.log()
            .lines()
            .filter(|l| l.starts_with("ARGV:") && l.contains("game.exe"))
            .count()
    }

    fn user_home(&self) -> PathBuf {
        self.root.join("user-home")
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.join("gogo-calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Where the launcher logs: `<GOGO_HOME>/logs/<gogid>` (the data root
    /// isn't the XDG default, so it's gogo's GOGO_HOME, as logs_root()).
    fn logs(&self) -> PathBuf {
        self.home.join("logs").join(ID)
    }

    fn launch_log(&self) -> String {
        std::fs::read_to_string(self.logs().join("launch.log")).unwrap_or_default()
    }

    /// The run logs, oldest first.
    fn runs(&self) -> Vec<String> {
        let mut runs: Vec<PathBuf> = std::fs::read_dir(self.logs())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(|c: char| c.is_ascii_digit())
            })
            .collect();
        runs.sort();
        runs.iter()
            .map(|p| std::fs::read_to_string(p).unwrap())
            .collect()
    }

    /// `launch.sh` with a cleared environment: the scratch `HOME` and XDG
    /// dirs, PATH = the tools + `bin/` (+ `extra_bin`), dialogs off unless
    /// `env` sets `GOGO_LAUNCHER_DIALOG`. No GOGO_HOME: the launcher works
    /// it out from where it is.
    fn launch_cmd(&self, env: &[(&str, &str)], args: &[&str]) -> std::process::Command {
        let mut c = std::process::Command::new(self.game().join("launch.sh"));
        let home = self.user_home();
        c.env_clear()
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &home)
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root.join("tools").display(),
                    self.root.join("bin").display()
                ),
            )
            .env("FAKE_UMU_LOG", &self.umu_log)
            .env("FAKE_WS_DIR", self.root.join("ws"))
            .env("FAKE_CALLS", self.root.join("gogo-calls.log"));
        if !env.iter().any(|(k, _)| *k == "GOGO_LAUNCHER_DIALOG") {
            c.env("GOGO_LAUNCHER_NO_DIALOG", "1");
        }
        for (k, v) in env {
            c.env(k, v);
        }
        c
    }

    fn launch(&self, env: &[(&str, &str)], args: &[&str]) -> std::process::Output {
        let out = self.launch_cmd(env, args).output().unwrap();
        eprintln!("launch.log:\n{}", self.launch_log());
        out
    }

    /// Rewrite `KEY=` lines of `launch.conf`.
    fn edit_conf(&self, sets: &[(&str, &str)]) {
        let conf: String = self
            .conf()
            .lines()
            .map(|l| {
                sets.iter()
                    .find(|(k, _)| l.starts_with(&format!("{k}=")))
                    .map(|(k, v)| format!("{k}={v}\n"))
                    .unwrap_or_else(|| format!("{l}\n"))
            })
            .collect();
        std::fs::write(self.game().join("launch.conf"), conf).unwrap();
    }

    /// A dialog stub answering from `answers` in turn, recording each
    /// dialog's argv (title, message, labels) to `dialogs.log`.
    fn dialog_stub(&self, answers: &[&str]) -> PathBuf {
        let file = self.root.join("answers");
        std::fs::write(
            &file,
            answers.iter().map(|a| format!("{a}\n")).collect::<String>(),
        )
        .unwrap();
        let stub = self.root.join("dialog");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\n{{ printf '%s\\n' \"$@\"; echo ===; }} >> \"{log}\"\n\
                 head -n 1 \"{file}\"\nsed -i 1d \"{file}\"\n",
                log = self.root.join("dialogs.log").display(),
                file = file.display()
            ),
        )
        .unwrap();
        chmod_x(&stub);
        stub
    }

    fn dialogs(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.join("dialogs.log"))
            .unwrap_or_default()
            .split("===\n")
            .filter(|d| !d.is_empty())
            .map(str::to_string)
            .collect()
    }
}

/// Poll `f` for up to `secs`.
fn wait_until(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    f()
}

#[test]
fn build_writes_the_launcher_menu_entry_and_icon_and_setup_keeps_them_current() {
    let w = World::launcher_built("files", false);
    let sh = w.game().join("launch.sh");
    let script = std::fs::read_to_string(&sh).unwrap();
    assert_eq!(script, gogo::linuxapp::launcher::launch_sh());
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(
        std::fs::metadata(&sh).unwrap().permissions().mode() & 0o777,
        0o755
    );

    let desktop = gogo::linuxapp::game::desktop_path(ID);
    assert_eq!(
        desktop,
        w.home.join(format!("applications/gogo-{ID}.desktop"))
    );
    let entry = std::fs::read_to_string(&desktop).unwrap();
    assert_eq!(
        entry,
        format!(
            "[Desktop Entry]\nType=Application\nVersion=1.5\nName=Test Game\n\
             Exec=\"{sh}\"\nIcon={icon}\nTerminal=false\nCategories=Game;\n\
             X-Gogo-Gogid={ID}\n",
            sh = sh.display(),
            icon = w.game().join("icon.png").display()
        )
    );
    // The icon: GOG's .ico, as a PNG.
    let png = std::fs::read(w.game().join("icon.png")).unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(&png[16..24], &[0, 0, 0, 16, 0, 0, 0, 16]);

    // R30: an older launcher (or a damaged menu entry) is rewritten by the
    // next setup, which says so; an unchanged one isn't touched.
    std::fs::write(
        &sh,
        script.replace("LAUNCHER_VERSION=15", "LAUNCHER_VERSION=0"),
    )
    .unwrap();
    std::fs::write(&desktop, "[Desktop Entry]\nExec=/old/root/launch.sh\n").unwrap();
    let out = setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert_eq!(std::fs::read_to_string(&sh).unwrap(), script);
    assert_eq!(std::fs::read_to_string(&desktop).unwrap(), entry);
    assert!(
        out.notes.iter().any(|n| n == "launch.sh: launcher 0 -> 15"),
        "{:?}",
        out.notes
    );
    assert_eq!(out.launcher.desktop, desktop);
    let mtime = std::fs::metadata(&sh).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let out = setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert!(out.launcher.updated_from.is_none());
    assert_eq!(std::fs::metadata(&sh).unwrap().modified().unwrap(), mtime);
}

#[test]
fn the_launcher_runs_umu_with_the_games_environment_and_logs_the_run() {
    let Some(w) = World::launcher("run", false) else {
        return;
    };
    w.edit_conf(&[
        ("ENV", "('FOO=bar baz')"),
        ("DLL_OVERRIDES", "'ddraw=n,b'"),
        ("PROTONFIXES", "'0'"),
    ]);
    let out = w.launch(&[], &["-extra"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    // The streams reach the terminal as well as the run log.
    assert!(String::from_utf8_lossy(&out.stdout).contains("fake-game stdout line"));
    assert!(String::from_utf8_lossy(&out.stderr).contains("fake-game stderr line"));
    // Fresh stamp: no setup.
    assert!(w.calls().is_empty(), "{:?}", w.calls());

    let log = w.log();
    let prefix = w.game().join("prefix");
    let workdir = prefix.join("drive_c/GOG/Test Game");
    assert!(
        log.contains(&format!(
            "ARGV:{} -windowed -extra\n",
            workdir.join("game.exe").display()
        )),
        "{log}"
    );
    let env = log.lines().rfind(|l| l.starts_with("ENV:")).unwrap();
    assert_eq!(
        env,
        format!(
            "ENV:WINEPREFIX={} GAMEID=umu-777001 STORE=gog PROTONPATH={} \
             WINEDLLOVERRIDES=ddraw=n,b PROTONFIXES_DISABLE=1 FOO=bar baz GAME_DRIVE=0 CWD={}",
            prefix.display(),
            w.build.display(),
            workdir.display()
        )
    );

    // One run log: header, the game's output, footer; latest.log names it.
    let runs = w.runs();
    assert_eq!(runs.len(), 1);
    // An L4 env can turn Proton's host drives back on.
    w.edit_conf(&[("ENV", "('PROTON_SET_GAME_DRIVE=1')")]);
    assert_eq!(w.launch(&[], &[]).status.code(), Some(0));
    let log = w.log();
    let env = log.lines().rfind(|l| l.starts_with("ENV:")).unwrap();
    assert!(env.contains(" GAME_DRIVE=1 "), "{env}");
    let runs = w.runs();
    let run = &runs[0];
    for want in [
        format!("# gogo run log: Test Game ({ID})\n"),
        "# launcher: 15\n".into(),
        format!(
            "# runtime: path:{} (1793687165 GE-Proton10-34)\n",
            w.build.display()
        ),
        format!("# runtime_dir: {}\n", w.build.display()),
        format!("# umu: {}\n", w.root.join("bin/umu-run").display()),
        "# umu_id: umu-777001\n".into(),
        "# renderer: dxvk\n".into(),
        "# protonfix: none\n".into(),
        "# protonfixes: off\n".into(),
        "# os: ".into(),
        format!("# exe: games/{ID}/prefix/drive_c/GOG/Test Game/game.exe\n"),
        "# args: -windowed -extra\n".into(),
        format!("# prefix: {}\n", prefix.display()),
        // stdout and stderr reach the log through separate tees: either order.
        "fake-game stdout line\n".into(),
        "# exit: 0\n# duration: ".into(),
    ] {
        assert!(run.contains(&want), "{want:?} in:\n{run}");
    }
    assert!(run.contains("err: fake-game stderr line\n"), "{run}");
    let latest = std::fs::read_link(w.logs().join("latest.log")).unwrap();
    assert!(latest.to_string_lossy().ends_with(".log"));
    // Dialogs off: the early exit is only logged.
    assert!(
        w.launch_log().contains("early exit: code 0"),
        "{}",
        w.launch_log()
    );
    // Nothing of the launcher's is left behind.
    assert!(
        std::fs::read_dir(w.game().join(".launchers"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn a_stale_stamp_runs_setup_once_and_without_gogo_the_last_setup_runs() {
    let Some(w) = World::launcher("stamp", false) else {
        return;
    };
    std::fs::create_dir_all(w.home.join("compat")).unwrap();
    std::fs::write(
        w.home.join(format!("compat/{ID}.toml")),
        "[platforms.linux]\nargs = []\n",
    )
    .unwrap();
    assert!(w.launch(&[], &[]).status.success());
    assert!(w.launch(&[], &[]).status.success());
    // Once, with the data root as GOGO_HOME (it isn't the XDG default).
    assert_eq!(
        w.calls(),
        [format!(
            "GOGO_HOME={} linux-app setup {ID}",
            w.home.display()
        )]
    );
    assert!(
        w.launch_log().contains("config stale ("),
        "{}",
        w.launch_log()
    );
    assert_eq!(w.game_runs(), 2);

    // umu's Python gone (a Nix GC): setup again, which records it anew.
    w.edit_conf(&[("UMU_PYTHON", "'/nix/store/gone-umu-python/bin/python3'")]);
    assert!(w.launch(&[], &[]).status.success());
    assert_eq!(w.calls().len(), 2, "{:?}", w.calls());
    assert!(
        w.launch_log()
            .contains("umu's Python /nix/store/gone-umu-python/bin/python3 is gone")
    );

    // A failing setup: logged, in the run log's header, the last config runs.
    std::fs::write(w.home.join(format!("compat/{ID}.toml")), "").unwrap();
    assert!(w.launch(&[("FAKE_SETUP_FAIL", "1")], &[]).status.success());
    let run = w.runs().pop().unwrap();
    assert!(
        run.contains(
            "# setup: setup failed (exit 1): gogo: setup broke; running the last applied config\n"
        ),
        "{run}"
    );

    // No gogo at all: logged, and the last setup runs.
    w.edit_conf(&[("GOGO_PATH", "'/nowhere/gogo'")]);
    let calls = w.calls().len();
    assert!(w.launch(&[], &[]).status.success());
    assert_eq!(w.calls().len(), calls);
    assert!(
        w.launch_log()
            .contains("and no gogo found (GOGO_PATH=/nowhere/gogo): running the last setup"),
        "{}",
        w.launch_log()
    );
}

#[test]
fn a_changed_build_runs_setup_when_managed_and_launches_when_not() {
    let Some(w) = World::launcher("runtime", false) else {
        return;
    };
    let version = w.build.join("version");
    // Unmanaged (the user's own build): logged, in the header, launched.
    std::fs::write(&version, "1793687166 GE-Proton10-35\n").unwrap();
    assert!(w.launch(&[], &[]).status.success());
    assert!(w.calls().is_empty(), "{:?}", w.calls());
    let run = w.runs().pop().unwrap();
    assert!(
        run.contains(
            "# runtime_note: unmanaged build changed: version is '1793687166 GE-Proton10-35', \
             expected '1793687165 GE-Proton10-34'; launching it as it is\n"
        ),
        "{run}"
    );
    assert!(run.contains("(1793687166 GE-Proton10-35)\n"), "{run}");

    // gogo's own (its marker): setup, which reinstalls it, then the launch.
    std::fs::write(w.build.join("gogo-managed.toml"), "schema = 1\n").unwrap();
    let out = w.launch(
        &[
            ("FAKE_VERSION_FILE", version.to_str().unwrap()),
            ("FAKE_VERSION", "1793687165 GE-Proton10-34"),
        ],
        &[],
    );
    assert!(out.status.success(), "{out:?}");
    assert_eq!(w.calls().len(), 1, "{:?}", w.calls());
    assert!(
        w.launch_log().contains("Proton build"),
        "{}",
        w.launch_log()
    );
    let run = w.runs().pop().unwrap();
    assert!(!run.contains("# runtime_note"), "{run}");

    // The build gone and no gogo to reinstall it: no launch, exit 127, a
    // dialog naming the fix, a run log saying why.
    std::fs::remove_dir_all(&w.build).unwrap();
    w.edit_conf(&[("GOGO_PATH", "'/nowhere/gogo'")]);
    let stub = w.dialog_stub(&["OK"]);
    let launches = w.game_runs();
    let out = w.launch(&[("GOGO_LAUNCHER_DIALOG", stub.to_str().unwrap())], &[]);
    assert_eq!(out.status.code(), Some(127), "{out:?}");
    assert_eq!(w.game_runs(), launches);
    let run = w.runs().pop().unwrap();
    assert!(run.contains("# exit: 127 (runtime missing)\n"), "{run}");
    assert!(run.contains("proton is missing"), "{run}");
    let d = w.dialogs();
    assert_eq!(d.len(), 1);
    assert!(
        d[0].contains("is missing or changed")
            && d[0].contains(&format!("gogo linux-app setup {ID}")),
        "{}",
        d[0]
    );
}

#[test]
fn nothing_runs_without_launch_conf_or_a_prefix() {
    let Some(w) = World::launcher("refuse", true) else {
        return;
    };
    std::fs::remove_dir_all(w.game().join("prefix")).unwrap();
    let out = w.launch(&[], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        w.launch_log().contains("not launching"),
        "{}",
        w.launch_log()
    );
    std::fs::remove_file(w.game().join("launch.conf")).unwrap();
    let stub = w.dialog_stub(&["OK"]);
    let out = w.launch(&[("GOGO_LAUNCHER_DIALOG", stub.to_str().unwrap())], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(w.dialogs()[0].contains("isn't set up"), "{:?}", w.dialogs());
    assert_eq!(w.game_runs(), 0);
}

#[test]
fn run_logs_rotate_to_the_newest_ten() {
    let Some(w) = World::launcher("rotate", true) else {
        return;
    };
    std::fs::create_dir_all(w.logs()).unwrap();
    for i in 0..10 {
        std::fs::write(w.logs().join(format!("2000010{i}-000000-1.log")), "# old\n").unwrap();
    }
    assert!(w.launch(&[], &[]).status.success());
    let names: Vec<String> = {
        let mut n: Vec<String> = std::fs::read_dir(w.logs())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(|c: char| c.is_ascii_digit()))
            .collect();
        n.sort();
        n
    };
    assert_eq!(names.len(), 10, "{names:?}");
    assert_eq!(names[0], "20000101-000000-1.log");
    assert!(!names[9].starts_with("2000"));
}

#[test]
fn the_early_exit_dialog_offers_the_next_renderer_and_its_buttons_work() {
    let Some(w) = World::launcher("dialog", false) else {
        return;
    };
    // Wayland, with wl-copy and xdg-open: the clipboard and the opened log
    // are files.
    let bin = w.root.join("bin");
    let clip = w.root.join("clipboard");
    let opened = w.root.join("opened");
    std::fs::write(
        bin.join("wl-copy"),
        format!("#!/bin/sh\ncat > \"{}\"\n", clip.display()),
    )
    .unwrap();
    std::fs::write(
        bin.join("xdg-open"),
        format!("#!/bin/sh\necho \"$1\" > \"{}\"\n", opened.display()),
    )
    .unwrap();
    chmod_x(&bin.join("wl-copy"));
    chmod_x(&bin.join("xdg-open"));
    let stub = w.dialog_stub(&["Try next renderer", "Copy report"]);
    let env = [
        ("GOGO_LAUNCHER_DIALOG", stub.to_str().unwrap()),
        ("FAKE_GAME_EXIT", "3"),
        ("WAYLAND_DISPLAY", "wayland-0"),
    ];
    let out = w.launch(&env, &[]);
    assert_eq!(out.status.code(), Some(3));

    // next-renderer wrote L4; the relaunch ran setup on the stale stamp.
    assert_eq!(
        w.calls(),
        [
            format!(
                "GOGO_HOME={} linux-app next-renderer {ID} --dry-run",
                w.home.display()
            ),
            format!(
                "GOGO_HOME={} linux-app next-renderer {ID}",
                w.home.display()
            ),
            format!("GOGO_HOME={} linux-app setup {ID}", w.home.display()),
            format!(
                "GOGO_HOME={} linux-app next-renderer {ID} --dry-run",
                w.home.display()
            ),
        ]
    );
    let d = w.dialogs();
    assert_eq!(d.len(), 2, "{d:?}");
    assert!(d[0].starts_with("Test Game\n"), "{}", d[0]);
    assert!(
        d[0].contains("(exit code 3) on the dxvk renderer"),
        "{}",
        d[0]
    );
    assert!(d[0].contains("err: fake-game stderr line"), "{}", d[0]);
    assert!(d[0].contains("switches to wined3d"), "{}", d[0]);
    assert!(
        d[0].ends_with("Copy report\nOpen log\nTry next renderer\n"),
        "{}",
        d[0]
    );
    assert!(d[1].contains("on the wined3d renderer"), "{}", d[1]);
    assert!(d[1].ends_with("Copy report\nOpen log\nClose\n"), "{}", d[1]);
    let runs = w.runs();
    assert_eq!(runs.len(), 2);
    assert!(runs[1].contains("# renderer: wined3d\n"), "{}", runs[1]);

    // Copy report: wl-copy got it; the home dir is ~.
    let report = std::fs::read_to_string(&clip).unwrap();
    for want in [
        format!("gogo launch report: Test Game ({ID})\n"),
        "# renderer: wined3d\n".into(),
        "# exit: 3\n".into(),
        "# os: ".into(),
        "--- last 40 log lines ---\n".into(),
        "fake-game stdout line\n".into(),
    ] {
        assert!(report.contains(&want), "{want:?} in:\n{report}");
    }
    assert!(!report.contains("# prefix:"), "{report}");

    // Open log: xdg-open on the run log.
    let stub = w.dialog_stub(&["Open log"]);
    let env = [
        ("GOGO_LAUNCHER_DIALOG", stub.to_str().unwrap()),
        ("FAKE_GAME_EXIT", "3"),
    ];
    assert_eq!(w.launch(&env, &[]).status.code(), Some(3));
    assert!(wait_until(5, || opened.is_file()));
    let target = std::fs::read_to_string(&opened).unwrap();
    assert!(
        target.trim().starts_with(w.logs().to_str().unwrap()),
        "{target}"
    );

    // X11 (no WAYLAND_DISPLAY): xclip.
    std::fs::write(
        bin.join("xclip"),
        format!(
            "#!/bin/sh\necho \"$*\" > \"{clip}.args\"\ncat > \"{clip}\"\n",
            clip = clip.display()
        ),
    )
    .unwrap();
    chmod_x(&bin.join("xclip"));
    std::fs::remove_file(&clip).unwrap();
    let stub = w.dialog_stub(&["Copy report"]);
    let env = [
        ("GOGO_LAUNCHER_DIALOG", stub.to_str().unwrap()),
        ("FAKE_GAME_EXIT", "3"),
    ];
    assert_eq!(w.launch(&env, &[]).status.code(), Some(3));
    assert_eq!(
        std::fs::read_to_string(w.root.join("clipboard.args")).unwrap(),
        "-selection clipboard\n"
    );
    assert!(
        std::fs::read_to_string(&clip)
            .unwrap()
            .contains("# exit: 3\n")
    );

    // Neither tool: Save report writes report.txt next to the run logs.
    std::fs::remove_file(bin.join("xclip")).unwrap();
    std::fs::remove_file(bin.join("wl-copy")).unwrap();
    let stub = w.dialog_stub(&["Save report"]);
    let env = [
        ("GOGO_LAUNCHER_DIALOG", stub.to_str().unwrap()),
        ("FAKE_GAME_EXIT", "3"),
    ];
    assert_eq!(w.launch(&env, &[]).status.code(), Some(3));
    let last = w.dialogs().pop().unwrap();
    assert!(last.ends_with("Save report\nOpen log\nClose\n"), "{last}");
    let saved = std::fs::read_to_string(w.logs().join("report.txt")).unwrap();
    assert!(saved.contains("# exit: 3\n"), "{saved}");
}

/// The prefix's processes as gogo sees them (`/proc/<pid>/environ`).
fn prefix_procs(w: &World) -> Vec<u32> {
    gogo::linuxapp::game::prefix_procs(&w.game().join("prefix"))
        .into_iter()
        .map(|p| p.pid)
        .collect()
}

/// The hanging fake game is up: its fake wineserver, the last process it
/// starts, exists. Waiting for a process count is not enough: the game and
/// its three TERM-proof sleeps already make four, so a TERM could land
/// before the wineserver the launcher's close request looks for.
fn hang_ready(w: &World) -> bool {
    wait_until(10, || {
        gogo::linuxapp::game::prefix_procs(&w.game().join("prefix"))
            .iter()
            .any(|p| p.name == "wineserver")
    })
}

/// Launch the hanging fake game, TERM the launcher, wait for it to exit
/// (its code, the time it took).
fn term_hanging_launcher(w: &World, env: &[(&str, &str)]) -> (Option<i32>, u64) {
    let mut env = env.to_vec();
    env.push(("FAKE_GAME_HANG", "1"));
    let mut child = w
        .launch_cmd(&env, &[])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // The game and its TERM-proof children of other sessions.
    assert!(hang_ready(w), "{:?}", prefix_procs(w));
    let status = std::process::Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let start = std::time::Instant::now();
    let code = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s.code();
        }
        assert!(start.elapsed().as_secs() < 15, "the launcher didn't exit");
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    (code, start.elapsed().as_secs())
}

/// Like `term_hanging_launcher`, but sends `first`, then (after `gap_ms`)
/// `second`; returns the code and how long the launcher lived after the
/// second signal.
fn two_signals_launcher(
    w: &World,
    env: &[(&str, &str)],
    first: &str,
    second: &str,
    gap_ms: u64,
) -> (Option<i32>, std::time::Duration) {
    let mut env = env.to_vec();
    env.push(("FAKE_GAME_HANG", "1"));
    let mut child = w
        .launch_cmd(&env, &[])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    assert!(hang_ready(w), "{:?}", prefix_procs(w));
    let send = |sig: &str| {
        let st = std::process::Command::new("kill")
            .args([sig, &child.id().to_string()])
            .status()
            .unwrap();
        assert!(st.success());
    };
    send(first);
    std::thread::sleep(std::time::Duration::from_millis(gap_ms));
    send(second);
    let start = std::time::Instant::now();
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return (s.code(), start.elapsed());
        }
        if start.elapsed().as_secs() >= 20 {
            let _ = child.kill();
            panic!("the launcher didn't exit");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn a_second_signal_cuts_the_close_wait_short() {
    for (name, first, second, code) in [
        ("sig2term", "-TERM", "-TERM", 143),
        ("sig2int", "-TERM", "-INT", 143),
        ("sig2intint", "-INT", "-INT", 130),
    ] {
        let Some(w) = World::launcher(name, true) else {
            return;
        };
        let (c, after) =
            two_signals_launcher(&w, &[("GOGO_CLOSE_GRACE_SECS", "8")], first, second, 1000);
        assert_eq!(c, Some(code), "{name}");
        // Torn down at once, not after the 8 s wait.
        assert!(after.as_secs() < 6, "{name}: {after:?}");
        assert!(prefix_procs(&w).is_empty(), "{:?}", prefix_procs(&w));
        let log = w.launch_log();
        assert!(log.contains("asked the game to close"), "{log}");
        assert!(log.contains("not waiting for the game to close"), "{log}");
        assert!(!log.contains("didn't close in"), "{log}");
        assert!(log.contains("reaped (SIGKILL): "), "{log}");
    }
}

#[test]
fn teardown_on_term_kills_every_process_with_the_prefix() {
    let Some(w) = World::launcher("teardown", true) else {
        return;
    };
    // A game that ignores the close request: after the grace, TERM then KILL.
    let (code, _) = term_hanging_launcher(&w, &[("GOGO_CLOSE_GRACE_SECS", "2")]);
    assert_eq!(code, Some(143));
    assert!(prefix_procs(&w).is_empty(), "{:?}", prefix_procs(&w));
    let log = w.launch_log();
    assert!(log.contains("got SIGTERM"), "{log}");
    assert!(log.contains("asked the game to close"), "{log}");
    assert!(log.contains("didn't close in 2s: TERM"), "{log}");
    assert!(log.contains("reaped (SIGKILL): "), "{log}");
    assert!(log.contains("sleep;"), "{log}");
    let run = w.runs().pop().unwrap();
    assert!(run.contains("# exit: 143 (SIGTERM)\n"), "{run}");
    // The request ran Wine's wineboot, in the wineserver's namespaces.
    let umu = std::fs::read_to_string(&w.umu_log).unwrap();
    assert!(umu.contains("CLOSE:wineboot --end-session\n"), "{umu}");
}

#[test]
fn a_game_that_closes_on_the_request_is_not_torn_down() {
    let Some(w) = World::launcher("close", true) else {
        return;
    };
    let (code, _) = term_hanging_launcher(&w, &[("FAKE_CLOSE_WORKS", "1")]);
    assert_eq!(code, Some(143));
    assert!(prefix_procs(&w).is_empty(), "{:?}", prefix_procs(&w));
    let log = w.launch_log();
    assert!(log.contains("asked the game to close"), "{log}");
    assert!(log.contains("closed by itself after "), "{log}");
    assert!(!log.contains("didn't close"), "{log}");
    assert!(!log.contains("reaped (SIGKILL)"), "{log}");
    let run = w.runs().pop().unwrap();
    assert!(run.contains("# exit: 143 (SIGTERM)\n"), "{run}");
}

#[test]
fn the_watchdog_tears_down_after_the_launcher_is_killed() {
    let Some(w) = World::launcher("watchdog", true) else {
        return;
    };
    let mut child = w
        .launch_cmd(&[("FAKE_GAME_HANG", "1")], &[])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    assert!(hang_ready(&w));
    assert!(wait_until(5, || w.launch_log().contains("exec umu-run")));
    let pid = child.id();
    // A kill before the watchdog exists would leave nothing to tear down.
    assert!(wait_until(5, || w.launch_log().contains("guard armed")));
    let record = w.game().join(format!(".launchers/{pid}"));
    assert!(wait_until(5, || record.is_file()), "no record for {pid}");
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        wait_until(15, || prefix_procs(&w).is_empty()
            && w.launch_log().contains("watchdog done")),
        "{:?}\n{}",
        prefix_procs(&w),
        w.launch_log()
    );
    assert!(
        w.launch_log()
            .contains(&format!("watchdog: launcher {pid} is gone"))
    );
    let run = w.runs().pop().unwrap();
    assert!(run.contains("# exit: 137 (launcher killed)\n"), "{run}");
    assert!(!w.game().join(format!(".launchers/{pid}")).exists());
}

fn pe_with_imports(pe64: bool, imports: &[&str]) -> Vec<u8> {
    const RVA: u32 = 0x1000;
    const OFF: usize = 0x200;
    let imp_len = 20 * (imports.len() + 1);
    let thunk = imp_len;
    let names_off = thunk + 8;
    let mut names = Vec::new();
    let mut name_rvas = Vec::new();
    for n in imports {
        name_rvas.push(RVA + (names_off + names.len()) as u32);
        names.extend_from_slice(n.as_bytes());
        names.push(0);
    }
    let mut sec = Vec::new();
    for name in &name_rvas {
        for v in [RVA + thunk as u32, 0, 0, *name, RVA + thunk as u32] {
            sec.extend_from_slice(&v.to_le_bytes());
        }
    }
    sec.extend_from_slice(&[0; 20 + 8]);
    sec.extend_from_slice(&names);
    let raw = sec.len().next_multiple_of(0x200);
    sec.resize(raw, 0);

    let mut w = Vec::new();
    let u16le = |w: &mut Vec<u8>, v: u16| w.extend_from_slice(&v.to_le_bytes());
    let u32le = |w: &mut Vec<u8>, v: u32| w.extend_from_slice(&v.to_le_bytes());
    w.extend_from_slice(b"PE\0\0");
    let (machine, opt, chars) = if pe64 {
        (0x8664u16, 240u16, 0x0022u16)
    } else {
        (0x014C, 224, 0x0102)
    };
    u16le(&mut w, machine);
    u16le(&mut w, 1);
    w.extend_from_slice(&[0; 12]);
    u16le(&mut w, opt);
    u16le(&mut w, chars);
    u16le(&mut w, if pe64 { 0x20B } else { 0x10B });
    w.extend_from_slice(&[0, 0]);
    for v in [0, raw as u32, 0, 0, RVA] {
        u32le(&mut w, v);
    }
    if pe64 {
        w.extend_from_slice(&0x1_4000_0000u64.to_le_bytes());
    } else {
        u32le(&mut w, 0);
        u32le(&mut w, 0x40_0000);
    }
    u32le(&mut w, 0x1000);
    u32le(&mut w, 0x200);
    for v in [6u16, 0, 0, 0, 6, 0] {
        u16le(&mut w, v);
    }
    for v in [
        0,
        RVA + (raw as u32).next_multiple_of(0x1000),
        OFF as u32,
        0,
    ] {
        u32le(&mut w, v);
    }
    u16le(&mut w, 2);
    u16le(&mut w, 0);
    w.extend(std::iter::repeat_n(0, 4 * if pe64 { 8 } else { 4 }));
    u32le(&mut w, 0);
    u32le(&mut w, 16);
    for i in 0..16 {
        let (va, size) = if i == 1 {
            (RVA, imp_len as u32)
        } else {
            (0, 0)
        };
        u32le(&mut w, va);
        u32le(&mut w, size);
    }
    w.extend_from_slice(b".rdata\0\0");
    for v in [
        sec.len() as u32,
        RVA,
        raw as u32,
        OFF as u32,
        0,
        0,
        0,
        0x4000_0040,
    ] {
        u32le(&mut w, v);
    }
    let mut h = vec![0u8; OFF];
    h[0..2].copy_from_slice(b"MZ");
    h[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
    h[0x40..0x40 + w.len()].copy_from_slice(&w);
    h.extend_from_slice(&sec);
    h
}

// — `gogo linux-app doctor` (#178) —

/// A host for doctor: a fake root with one GPU, user namespaces on, and
/// the Vulkan drivers `icds` names (`(manifest name, ELF class)`), and the
/// test's env minus anything Vulkan (the real machine's must not leak in).
fn doctor_host(w: &World, icds: &[(&str, u8)]) -> gogo::linuxapp::doctor::Host {
    let names: Vec<&str> = icds.iter().map(|(n, _)| *n).collect();
    let root = w.root.join(format!("host-{}", names.join("+")));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("proc/sys/user")).unwrap();
    std::fs::write(root.join("proc/sys/user/max_user_namespaces"), "63000\n").unwrap();
    std::fs::create_dir_all(root.join("sys/class/drm/card0/device")).unwrap();
    std::fs::write(root.join("sys/class/drm/card0/device/vendor"), "0x1002\n").unwrap();
    let icd_dir = root.join("usr/share/vulkan/icd.d");
    std::fs::create_dir_all(&icd_dir).unwrap();
    std::fs::create_dir_all(root.join("usr/lib")).unwrap();
    for (name, class) in icds {
        let lib = format!("/usr/lib/libvulkan_{name}.so");
        std::fs::write(
            root.join(lib.trim_start_matches('/')),
            [0x7f, b'E', b'L', b'F', *class, 1, 1, 0],
        )
        .unwrap();
        std::fs::write(
            icd_dir.join(format!("{name}_icd.json")),
            format!(
                "{{\"file_format_version\": \"1.0.0\", \"ICD\": {{\"library_path\": \"{lib}\", \
                 \"api_version\": \"1.3.0\"}}}}"
            ),
        )
        .unwrap();
    }
    gogo::linuxapp::doctor::Host {
        root,
        env: Box::new(|k| {
            (!k.starts_with("VK_") && k != "XDG_DATA_DIRS")
                .then(|| std::env::var_os(k))
                .flatten()
        }),
        logs: w.root.join("logs"),
        vulkaninfo: false,
    }
}

fn doctor(w: &World, host: &gogo::linuxapp::doctor::Host) -> gogo::macapp::doctor::Report {
    let _ = w;
    gogo::linuxapp::doctor::doctor(ID, host).unwrap()
}

/// The findings at `level`, as `check: message` lines.
fn at(r: &gogo::macapp::doctor::Report, level: gogo::macapp::doctor::Level) -> Vec<String> {
    r.findings
        .iter()
        .filter(|f| f.level == level)
        .map(|f| format!("{}: {}", f.check, f.message))
        .collect()
}

/// A fake Python printing `version`.
fn fake_python(w: &World, version: &str) -> PathBuf {
    let p = w.root.join(format!("python-{version}"));
    std::fs::write(&p, format!("#!/bin/sh\necho Python {version}\n")).unwrap();
    chmod_x(&p);
    p
}

#[test]
fn one_doctor_report_scans_the_install_once() {
    let w = World::launcher_built("doctor-scan-once", false);
    let host = doctor_host(&w, &[("radeon.x86_64", 2), ("radeon.i686", 1)]);
    let before = gogo::linuxapp::manage::scans_on_this_thread();
    let r = doctor(&w, &host);
    assert!(r.findings.iter().any(|f| f.check == "compat"));
    assert!(r.findings.iter().any(|f| f.check == "launch"));
    assert!(r.findings.iter().any(|f| f.check == "renderer"));
    assert_eq!(
        gogo::linuxapp::manage::scans_on_this_thread() - before,
        1,
        "compat, launch and renderer must share one scan"
    );
}

#[test]
fn doctor_is_green_on_a_set_up_game_and_each_check_fails_alone() {
    use gogo::macapp::doctor::Level;
    let w = World::launcher_built("doctor", false);
    let host = doctor_host(&w, &[("radeon.x86_64", 2), ("radeon.i686", 1)]);

    // Nothing failed; the warnings are this world's: a system umu and an
    // unmanaged build (both by design), no container yet.
    let r = doctor(&w, &host);
    assert!(r.ok(), "{:#?}", at(&r, Level::Fail));
    let checks: Vec<&str> = r.findings.iter().map(|f| f.check).collect();
    for c in [
        "launcher",
        "compat",
        "umu",
        "runtime",
        "container",
        "renderer",
        "vulkan",
        "vulkan32",
        "stamp",
        "links",
        "processes",
        "log",
    ] {
        assert!(checks.contains(&c), "{c} missing: {checks:?}");
    }
    let warns = at(&r, Level::Warn);
    assert!(
        warns.iter().any(|w| w.starts_with("umu: system umu-run")),
        "{warns:?}"
    );
    assert!(warns.iter().any(|w| w.contains("unmanaged")), "{warns:?}");
    assert!(
        warns.iter().any(|w| w.starts_with("container: steamrt4")),
        "{warns:?}"
    );
    assert_eq!(warns.len(), 3, "{warns:?}");
    let ok = at(&r, Level::Ok);
    assert!(
        ok.iter()
            .any(|o| o.starts_with("vulkan32: 32-bit game: 1 32-bit")),
        "{ok:?}"
    );
    assert!(
        ok.iter().any(|o| o
            == "links: no links out of the data dir but z:, Wine's serial ports and the \
                    Proton build's own files"),
        "{ok:?}"
    );
    assert!(
        ok.iter().any(|o| o.starts_with("stamp: config current")),
        "{ok:?}"
    );

    // Each failure alone.
    let fails = |r: &gogo::macapp::doctor::Report| at(r, Level::Fail);

    // No Vulkan driver at all.
    let r = doctor(&w, &doctor_host(&w, &[]));
    let f = fails(&r);
    assert!(
        f.iter().any(|l| l.starts_with("vulkan: no Vulkan driver")),
        "{f:?}"
    );
    assert!(
        f.iter()
            .any(|l| l.starts_with("vulkan32: 32-bit game and no")),
        "{f:?}"
    );
    assert_eq!(f.len(), 2, "{f:?}");

    // A 32-bit game with only a 64-bit driver.
    let r = doctor(&w, &doctor_host(&w, &[("radeon.x86_64", 2)]));
    assert_eq!(
        fails(&r),
        [
            "vulkan32: 32-bit game and no 32-bit Vulkan driver: DXVK and D7VK fail, only wined3d works"
        ]
    );

    // The zipapp gone (umu in zipapp mode, a good Python).
    let good = fake_python(&w, "3.12.4");
    let conf = w.conf();
    w.edit_conf(&[
        ("UMU_PYTHON", &format!("'{}'", good.display())),
        ("UMU_RUN", "'runtimes/umu/1.4.4/umu/umu-run'"),
    ]);
    let r = doctor(&w, &host);
    let f = fails(&r);
    assert_eq!(f.len(), 1, "{f:?}");
    assert!(
        f[0].starts_with("umu: the zipapp ") && f[0].ends_with(" is missing"),
        "{f:?}"
    );
    assert!(
        at(&r, Level::Ok)
            .iter()
            .any(|o| o.starts_with("umu: Python 3.12"))
    );

    // Python too old.
    let zipapp = w.root.join("umu-run.pyz");
    std::fs::write(&zipapp, "zip").unwrap();
    let old = fake_python(&w, "3.8.10");
    w.edit_conf(&[
        ("UMU_PYTHON", &format!("'{}'", old.display())),
        ("UMU_RUN", &format!("'{}'", zipapp.display())),
    ]);
    let f = fails(&doctor(&w, &host));
    assert_eq!(
        f,
        [format!(
            "umu: umu's zipapp needs Python >= 3.10; {} is 3.8",
            old.display()
        )]
    );

    // umu's Python gone (a Nix GC).
    w.edit_conf(&[(
        "UMU_PYTHON",
        "'/nix/store/gone-umu-python/bin/gogo-umu-python'",
    )]);
    let f = fails(&doctor(&w, &host));
    assert_eq!(f.len(), 1, "{f:?}");
    assert!(f[0].contains("is gone (a Nix garbage collection"), "{f:?}");
    std::fs::write(w.game().join("launch.conf"), &conf).unwrap();

    // A changed `proton` script in a build gogo installed (its marker).
    let marker =
        gogo::linuxapp::proton::marker_for(&w.build, "ge-proton/GE-Proton10-34", "ab").unwrap();
    gogo::linuxapp::proton::write_marker(&w.build, &marker).unwrap();
    assert!(doctor(&w, &host).ok(), "{:?}", fails(&doctor(&w, &host)));
    std::fs::write(w.build.join("proton"), "#!/bin/sh\n# changed\n").unwrap();
    let f = fails(&doctor(&w, &host));
    assert!(
        f.iter().all(|l| l.starts_with("runtime: "))
            && f.iter().any(|l| l.contains("changed since setup")),
        "{f:?}"
    );

    // Unmanaged and changed: the launcher launches it, so a warning.
    std::fs::remove_file(w.build.join("gogo-managed.toml")).unwrap();
    let r = doctor(&w, &host);
    assert!(r.ok(), "{:?}", fails(&r));
    assert!(
        at(&r, Level::Warn)
            .iter()
            .any(|l| l.contains("changed since setup"))
    );
}

#[test]
fn doctor_finds_links_stale_stamps_the_menu_entry_and_leftover_processes() {
    use gogo::macapp::doctor::Level;
    let w = World::launcher_built("doctor-files", false);
    let host = doctor_host(&w, &[("radeon.x86_64", 2), ("radeon.i686", 1)]);
    let prefix = w.game().join("prefix");

    // Links into the game's Proton build (outside the data dir here: a
    // `path:` build) are Proton's own DLLs: fine.
    std::fs::create_dir_all(w.build.join("files/lib/wine/i386-windows")).unwrap();
    std::os::unix::fs::symlink(
        w.build.join("files/lib/wine/i386-windows/d3d9.dll"),
        prefix.join("drive_c/windows/system32/d3d9.dll"),
    )
    .unwrap();
    assert!(doctor(&w, &host).ok());
    assert!(
        at(&doctor(&w, &host), Level::Warn)
            .iter()
            .all(|l| !l.starts_with("links"))
    );

    // Into another Proton build (the one before a retarget): a warning.
    let old = w.root.join("GE-Proton-old");
    std::fs::create_dir_all(old.join("files/lib/wine/i386-windows")).unwrap();
    for f in ["proton", "toolmanifest.vdf"] {
        std::fs::write(old.join(f), "x").unwrap();
    }
    let ddraw = prefix.join("drive_c/windows/system32/ddraw.dll");
    std::os::unix::fs::symlink(old.join("files/lib/wine/i386-windows/ddraw.dll"), &ddraw).unwrap();
    let r = doctor(&w, &host);
    assert!(r.ok());
    assert!(
        at(&r, Level::Warn)
            .iter()
            .any(|l| l.starts_with("links: the prefix still links into another Proton build")),
        "{:?}",
        at(&r, Level::Warn)
    );
    std::fs::remove_file(&ddraw).unwrap();

    // A link out of the data dir (Wine's x:), z: kept.
    std::os::unix::fs::symlink(&w.root, prefix.join("dosdevices/x:")).unwrap();
    assert!(prefix.join("dosdevices/z:").symlink_metadata().is_ok());
    let f = at(&doctor(&w, &host), Level::Fail);
    assert_eq!(f.len(), 1, "{f:?}");
    assert!(
        f[0].starts_with("links: 1 link(s) out of the data dir: dosdevices/x: -> "),
        "{f:?}"
    );
    std::fs::remove_file(prefix.join("dosdevices/x:")).unwrap();

    // Wine's serial ports come back on every start and reach no files; a
    // com link anywhere else isn't one.
    let com1 = prefix.join("dosdevices/com1");
    std::os::unix::fs::symlink("/dev/ttyS0", &com1).unwrap();
    assert!(doctor(&w, &host).ok());
    std::fs::remove_file(&com1).unwrap();
    std::os::unix::fs::symlink(&w.root, &com1).unwrap();
    assert!(!doctor(&w, &host).ok());
    std::fs::remove_file(&com1).unwrap();

    // An L4 edit since setup: stale (a warning; the next launch runs setup).
    let l4 = gogo::macapp::compat::user_path(ID);
    std::fs::create_dir_all(l4.parent().unwrap()).unwrap();
    std::fs::write(&l4, "[platforms.linux]\nnotes = \"x\"\n").unwrap();
    let r = doctor(&w, &host);
    assert!(r.ok());
    assert!(
        at(&r, Level::Warn)
            .iter()
            .any(|l| l.starts_with("stamp: config stale"))
    );
    std::fs::remove_file(&l4).unwrap();

    // The menu entry pointing elsewhere.
    let desktop = gogo::linuxapp::game::desktop_path(ID);
    let entry = std::fs::read_to_string(&desktop).unwrap();
    std::fs::write(&desktop, entry.replace("launch.sh", "other.sh")).unwrap();
    let f = at(&doctor(&w, &host), Level::Fail);
    assert!(
        f.len() == 1 && f[0].starts_with("launcher: ") && f[0].contains("doesn't run"),
        "{f:?}"
    );
    std::fs::write(&desktop, &entry).unwrap();

    // A process left in the prefix (Wine's form of WINEPREFIX), no launcher.
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .env("WINEPREFIX", format!("{}/pfx/", prefix.display()))
        .spawn()
        .unwrap();
    let r = doctor(&w, &host);
    let _ = child.kill();
    let _ = child.wait();
    let warn = at(&r, Level::Warn);
    assert!(
        warn.iter()
            .any(|l| l.starts_with("processes: 1 process(es)")
                && l.contains(&format!("{} sleep", child.id()))),
        "{warn:?}"
    );

    // Not built: one failing launcher line, the host checks still run.
    std::fs::remove_dir_all(w.game()).unwrap();
    let r = doctor(&w, &host);
    let f = at(&r, Level::Fail);
    assert_eq!(f.len(), 1, "{f:?}");
    assert!(
        f[0].starts_with("launcher: 1999999901 isn't built"),
        "{f:?}"
    );
    assert!(r.findings.iter().any(|f| f.check == "vulkan"));
}

#[test]
fn env_and_args_from_the_override_reach_the_game_with_their_quoting() {
    use gogo::linuxapp::envargs::{self, EnvSource, EnvState};
    use gogo::macapp::compat::Layer;
    let Some(w) = World::launcher("envargs", false) else {
        return;
    };
    let odd = "a 'b' \"c\" $HOME `x` \\ ; *";
    let out = envargs::set_env(
        ID,
        &[
            ("DXVK_HUD".into(), "fps,gpuload".into()),
            ("ODD".into(), odd.into()),
            ("GAME_TOKEN".into(), "s3cret".into()),
            ("PROTON_SET_GAME_DRIVE".into(), "1".into()),
        ],
    )
    .unwrap();
    assert_eq!(out.changes.len(), 4, "{:?}", out.changes);
    assert!(out.built);
    // gogo's default beaten: said so.
    assert!(
        out.warnings
            .iter()
            .any(|w| w.starts_with("PROTON_SET_GAME_DRIVE:")),
        "{:?}",
        out.warnings
    );
    // The launcher's own keys are refused and nothing is written.
    let l4 = w.home.join(format!("compat/{ID}.toml"));
    let before = std::fs::read_to_string(&l4).unwrap();
    assert!(envargs::set_env(ID, &[("WINEPREFIX".into(), "/x".into())]).is_err());
    assert!(envargs::set_env(ID, &[("WINEDLLOVERRIDES".into(), "d3d9=n".into())]).is_err());
    assert_eq!(std::fs::read_to_string(&l4).unwrap(), before);
    let extra: Vec<String> = [
        "--launcher-skip",
        "-skipStartScreen",
        "two words",
        "it's",
        "$HOME",
        "\"q\"",
        "",
    ]
    .map(String::from)
    .to_vec();
    envargs::set_args(ID, Some(extra.clone())).unwrap();
    // Setting the same again changes nothing.
    assert!(
        envargs::set_args(ID, Some(extra.clone()))
            .unwrap()
            .changes
            .is_empty()
    );

    // Before setup: pending, and doctor says so.
    let a = envargs::args_report(ID).unwrap();
    assert!(a.pending);
    assert_eq!(a.game_args, ["-windowed"]);
    assert_eq!(a.game_args_from, "GOG (gogo-launch.toml)");
    let mut want = vec!["-windowed".to_string()];
    want.extend(extra.iter().cloned());
    assert_eq!(a.launch_args, want);
    let e = envargs::env_report(ID, false).unwrap();
    let line = |e: &envargs::EnvReport, k: &str| {
        e.lines
            .iter()
            .find(|l| l.key == k && !matches!(l.state, EnvState::Overridden { .. }))
            .cloned()
            .unwrap_or_else(|| panic!("{k} in {:?}", e.lines))
    };
    assert_eq!(line(&e, "DXVK_HUD").state, EnvState::Pending);

    // Setup (what the launcher runs on the stale stamp) writes them.
    setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    let conf = gogo::linuxapp::game::ConfValues::parse(&w.conf());
    assert_eq!(conf.arrays["ARGS"], want);
    assert!(
        conf.arrays["ENV"].contains(&format!("ODD={odd}")),
        "{:?}",
        conf.arrays["ENV"]
    );
    let a = envargs::args_report(ID).unwrap();
    assert!(!a.pending, "{a:?}");

    let e = envargs::env_report(ID, false).unwrap();
    let hud = line(&e, "DXVK_HUD");
    assert_eq!(hud.value, "fps,gpuload");
    assert_eq!(hud.source, EnvSource::Layer { layer: Layer::User });
    assert_eq!(hud.state, EnvState::Applies);
    // Secrets: hidden unless revealed.
    assert_eq!(line(&e, "GAME_TOKEN").value, envargs::HIDDEN);
    let shown = envargs::env_report(ID, true).unwrap();
    assert_eq!(line(&shown, "GAME_TOKEN").value, "s3cret");
    // The user's beats gogo's default, which is listed as beaten.
    let drive: Vec<_> = e
        .lines
        .iter()
        .filter(|l| l.key == "PROTON_SET_GAME_DRIVE")
        .collect();
    assert_eq!(drive.len(), 2, "{drive:?}");
    assert_eq!(drive[0].value, "1");
    assert_eq!(drive[1].source, EnvSource::GogoDefault);
    assert!(matches!(drive[1].state, EnvState::Overridden { .. }));
    assert_eq!(line(&e, "WINEPREFIX").source, EnvSource::Launcher);
    assert_eq!(line(&e, "STORE").value, "gog");

    // The game gets every word as it was given, then launch.sh's own.
    let args_log = w.root.join("args.log");
    let env_dump = w.root.join("env.dump");
    let out = w.launch(
        &[
            ("FAKE_ARGS_LOG", args_log.to_str().unwrap()),
            ("FAKE_ENV_DUMP", env_dump.to_str().unwrap()),
        ],
        &["-cli arg"],
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(w.calls().is_empty(), "setup ran: {:?}", w.calls());
    let got: Vec<String> = std::fs::read_to_string(&args_log)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert!(got[0].ends_with("game.exe"), "{got:?}");
    let mut with_cli = want.clone();
    with_cli.push("-cli arg".into());
    assert_eq!(&got[1..], with_cli.as_slice());
    let dump = std::fs::read_to_string(&env_dump).unwrap();
    for want in [
        "DXVK_HUD=fps,gpuload".to_string(),
        format!("ODD={odd}"),
        "PROTON_SET_GAME_DRIVE=1".into(),
        "GAME_TOKEN=s3cret".into(),
    ] {
        assert!(dump.lines().any(|l| l == want), "{want} in\n{dump}");
    }
    // The run log has the args, never an env value.
    let run = w.runs().pop().unwrap();
    assert!(!run.contains("s3cret") && !run.contains("gpuload"), "{run}");

    // Doctor shows them.
    let r = doctor(&w, &doctor_host(&w, &[("radeon", 64)]));
    let f = r.findings.iter().find(|f| f.check == "launch").unwrap();
    assert_eq!(f.level, gogo::macapp::doctor::Level::Ok, "{f:?}");
    assert!(
        f.message
            .contains("your env: DXVK_HUD, GAME_TOKEN, ODD, PROTON_SET_GAME_DRIVE"),
        "{}",
        f.message
    );
    assert!(
        f.message
            .contains("extra args --launcher-skip -skipStartScreen 'two words'"),
        "{}",
        f.message
    );
    assert!(
        f.detail
            .iter()
            .any(|d| d == "* GAME_TOKEN=<hidden>  (L4 user override)"),
        "{:?}",
        f.detail
    );

    // No secret in any of doctor's lines (the fix summary included), nor in
    // `compat`'s.
    for f in &r.findings {
        assert!(!f.message.contains("s3cret"), "{f:?}");
        assert!(f.detail.iter().all(|d| !d.contains("s3cret")), "{f:?}");
    }
    let c = gogo::linuxapp::manage::compat(ID).unwrap();
    assert!(!c.fixes.unwrap_or_default().contains("s3cret"));
    let lines = c.effective.display_lines();
    assert!(
        lines
            .iter()
            .any(|l| l == "env.GAME_TOKEN = <hidden>  (L4 user override)"),
        "{lines:?}"
    );

    // Unset and clear: pending until the next setup; then gone.
    envargs::unset_env(ID, &["DXVK_HUD".into()]).unwrap();
    let out = envargs::unset_env(ID, &["NOPE".into()]).unwrap();
    assert_eq!(out.warnings, ["NOPE wasn't set"]);
    envargs::set_args(ID, None).unwrap();
    let e = envargs::env_report(ID, false).unwrap();
    assert_eq!(line(&e, "DXVK_HUD").source, EnvSource::LastSetup);
    assert!(envargs::args_report(ID).unwrap().pending);
    setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    let e = envargs::env_report(ID, false).unwrap();
    assert!(e.lines.iter().all(|l| l.key != "DXVK_HUD"), "{:?}", e.lines);
    let a = envargs::args_report(ID).unwrap();
    assert_eq!(a.launch_args, ["-windowed"]);
    assert!(!a.pending);
    let l4 = std::fs::read_to_string(&l4).unwrap();
    assert!(!l4.contains("extra_args"), "{l4}");

    // A bad name hand-written into the file is skipped by setup and named.
    std::fs::write(
        w.home.join(format!("compat/{ID}.toml")),
        "[platforms.linux.env]\n\"BAD-NAME\" = \"1\"\nGAMEID = \"x\"\n",
    )
    .unwrap();
    let out = setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert!(
        out.warnings
            .iter()
            .any(|w| w.starts_with("env BAD-NAME (L4 user override):")),
        "{:?}",
        out.warnings
    );
    assert!(!w.conf().contains("BAD-NAME"));
    let r = doctor(&w, &doctor_host(&w, &[("radeon", 64)]));
    let f = r.findings.iter().find(|f| f.check == "launch").unwrap();
    assert_eq!(f.level, gogo::macapp::doctor::Level::Warn, "{f:?}");
    assert!(
        f.message.contains("env GAMEID: GAMEID is the launcher's"),
        "{}",
        f.message
    );
    assert!(
        f.message.contains("env BAD-NAME in the override file"),
        "{}",
        f.message
    );
}

/// `env list --all` on a built game: names from the build's own files, the
/// renderer and the GPU from this game and this machine, active values next to
/// the switches they set. #233, design 18 §7.7.
#[test]
fn the_catalogue_reads_the_games_build_and_marks_what_applies() {
    use gogo::linuxapp::catalogue::{self, Applies, InBuild};
    use gogo::linuxapp::envargs;
    let w = World::new("catalogue", &["d3d12.dll"]);
    build_with(&w.opts());
    envargs::set_env(ID, &[("PROTON_DLSS_INDICATOR".into(), "1".into())]).unwrap();

    std::fs::write(
        w.build.join("proton"),
        "self.check_environment(\"PROTON_DLSS_INDICATOR\", \"dlsshud\")\n\
         self.check_environment(\"PROTON_BRAND_NEW\", \"brandnew\")\n",
    )
    .unwrap();

    let host = doctor_host(&w, &[]);
    let r = catalogue::report(ID, &host, false).unwrap();
    assert_eq!(r.renderer.as_deref(), Some("vkd3d-proton"));
    assert_eq!(r.gpus, ["AMD"]);
    assert_eq!(r.build.as_ref().unwrap().dir, w.build);

    let dlss = r.find("PROTON_DLSS_INDICATOR").unwrap();
    assert_eq!(dlss.in_build, InBuild::Found);
    assert_eq!(dlss.active.len(), 1, "the user's value is next to it");
    // The fake host has an AMD GPU; the DLSS indicator says so rather than hide.
    assert_eq!(
        dlss.applies,
        Applies::No {
            why: "for NVIDIA; this machine has AMD".into()
        }
    );
    // The build reads it and gogo has no words: shown, marked.
    let new = r.find("PROTON_BRAND_NEW").unwrap();
    assert_eq!(new.about, None);
    assert_eq!(new.in_build, InBuild::Found);
    assert_eq!(r.undescribed(), 1);
    // The build doesn't: said, not dropped.
    assert_eq!(r.find("PROTON_LOG").unwrap().in_build, InBuild::NotFound);

    // The plain list is what it was: active values only.
    let plain = envargs::env_report(ID, false).unwrap();
    assert!(plain.lines.iter().all(|l| l.key != "PROTON_LOG"));
}

// — wrappers (#271, design 18 §7.8) —

/// How wrappers treat the game, as fake commands that exec their args one
/// way or another. Each says its pid and environment first.
const WRAP_PREAMBLE: &str = r#"#!/bin/sh
echo $$ > "$FAKE_WRAP_LOG.pid"
: > "$FAKE_WRAP_LOG.argv"
for a in "$@"; do printf '%s\n' "$a" >> "$FAKE_WRAP_LOG.argv"; done
env > "$FAKE_WRAP_LOG.env"
"#;

/// gamemoderun's shape: sets something for the game, then execs it.
const WRAP_EXEC: &str = "WRAP_SET=from-wrapper FOO=from-wrapper exec \"$@\"\n";
/// gamescope's shape (design 18 §7.8): stays the parent in the same process
/// group, handles TERM, exits when its child does, and always with 0.
const WRAP_PARENT: &str =
    "\"$@\" &\nc=$!\ntrap 'kill -TERM $c 2>/dev/null; wait $c; exit 0' TERM\nwait $c\nexit 0\n";
/// gamescope when its window is closed: TERM to its child and the game under it at once, then exit 0.
const WRAP_TERM_CHILD: &str =
    "\"$@\" &\nc=$!\nsleep 1\nkill -TERM $c $(pgrep -P $c)\nwait $c\nexit 0\n";
/// Stays the parent, with the game in a session of its own: nothing sent to
/// the wrapper's process group reaches the game.
const WRAP_SESSION: &str = "setsid \"$@\" &\nwait $!\nexit $?\n";
/// Starts the game and leaves.
const WRAP_DAEMON: &str = "setsid \"$@\" </dev/null >/dev/null 2>&1 &\nexit 0\n";
/// Outlives the game, and doesn't take TERM.
const WRAP_LINGER: &str = "trap '' TERM\n\"$@\"\nwhile :; do sleep 1; done\n";

impl World {
    /// `launch.sh` will run the game under a fake command `fake-wrap` with
    /// these words after it: set through `envargs`, applied by setup.
    fn wrapped(&self, body: &str, words: &[&str]) {
        let path = self.root.join("bin/fake-wrap");
        std::fs::write(&path, format!("{WRAP_PREAMBLE}{body}")).unwrap();
        chmod_x(&path);
        let mut all = vec!["fake-wrap".to_string()];
        all.extend(words.iter().map(|s| s.to_string()));
        gogo::linuxapp::envargs::set_wrapper(ID, Some(all)).unwrap();
        setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    }

    fn wrap_log(&self) -> PathBuf {
        self.root.join("wrap")
    }

    fn wrap_file(&self, ext: &str) -> String {
        let mut p = self.wrap_log().into_os_string();
        p.push(ext);
        std::fs::read_to_string(p).unwrap_or_default()
    }

    /// The wrapper process is gone (a zombie counts: nobody's left to reap it
    /// in a test run's container).
    fn wrapper_gone(&self) -> bool {
        let pid = self.wrap_file(".pid");
        let pid = pid.trim();
        assert!(!pid.is_empty(), "the wrapper never ran");
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(s) => s.rsplit(") ").next().unwrap().starts_with('Z'),
            Err(_) => true,
        }
    }

    /// No game mark, no FIFOs: the run's scratch in the log dir is gone.
    fn run_scratch(&self) -> Vec<String> {
        std::fs::read_dir(self.logs())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(".run-"))
            .collect()
    }
}

#[test]
fn a_wrapper_goes_outside_umu_and_the_games_env_goes_inside_it() {
    let Some(w) = World::launcher("wrap-env", false) else {
        return;
    };
    let odd = "a 'b' \"c\" $HOME `x` \\ ; *";
    gogo::linuxapp::envargs::set_env(
        ID,
        &[
            ("DXVK_HUD".into(), "fps".into()),
            ("FOO".into(), "from-l4".into()),
        ],
    )
    .unwrap();
    // Its own options end at `--`, as gamescope's do.
    let own_options = format!("while [ \"$1\" != -- ]; do shift; done\nshift\n{WRAP_EXEC}");
    w.wrapped(&own_options, &["--opt", odd, "--"]);
    let conf = gogo::linuxapp::game::ConfValues::parse(&w.conf());
    assert_eq!(conf.arrays["WRAPPER"], ["fake-wrap", "--opt", odd, "--"]);

    let log = w.wrap_log();
    let args_log = w.root.join("args.log");
    let env_dump = w.root.join("game.env");
    let out = w.launch(
        &[
            ("FAKE_WRAP_LOG", log.to_str().unwrap()),
            ("FAKE_ARGS_LOG", args_log.to_str().unwrap()),
            ("FAKE_ENV_DUMP", env_dump.to_str().unwrap()),
        ],
        &["-cli arg"],
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(w.calls().is_empty(), "setup ran: {:?}", w.calls());

    // The wrapper's words as typed; then this launcher again, for the game.
    let launch_sh = w.game().join("launch.sh");
    let argv = w.wrap_file(".argv");
    let argv: Vec<&str> = argv.lines().collect();
    assert_eq!(argv[..3], ["--opt", odd, "--"], "{argv:?}");
    assert_eq!(argv[3], launch_sh.to_str().unwrap(), "{argv:?}");
    assert_eq!(argv[4], gogo::linuxapp::launcher::RUN_GAME_ARG, "{argv:?}");
    assert_eq!(argv[6], "-cli arg", "{argv:?}");

    // The wrapper has the launcher's starting environment, none of the game's.
    let wrap_env = w.wrap_file(".env");
    for k in [
        "WINEPREFIX=",
        "GAMEID=",
        "STORE=",
        "PROTONPATH=",
        "DXVK_HUD=",
        "FOO=",
        "PROTON_SET_GAME_DRIVE=",
    ] {
        assert!(
            !wrap_env.lines().any(|l| l.starts_with(k)),
            "{k} in:\n{wrap_env}"
        );
    }
    assert!(wrap_env.contains("FAKE_WRAP_LOG="), "{wrap_env}");

    // umu-run has both: what the wrapper set, with the game's env over it.
    let game_env = std::fs::read_to_string(&env_dump).unwrap();
    let prefix = w.game().join("prefix");
    for want in [
        format!("WINEPREFIX={}", prefix.display()),
        "DXVK_HUD=fps".to_string(),
        "WRAP_SET=from-wrapper".to_string(),
        "FOO=from-l4".to_string(),
        "PROTON_SET_GAME_DRIVE=0".to_string(),
    ] {
        assert!(
            game_env.lines().any(|l| l == want),
            "{want} in:\n{game_env}"
        );
    }
    let got = std::fs::read_to_string(&args_log).unwrap();
    assert!(got.ends_with("-windowed\n-cli arg\n"), "{got}");
    let umu = std::fs::read_to_string(&w.umu_log).unwrap();
    assert!(
        umu.contains(&format!("CWD={}", w.game().join("prefix").display())),
        "{umu}"
    );

    let run = w.runs().pop().unwrap();
    assert!(
        run.contains(&format!("# wrapper: fake-wrap --opt {odd} --\n")),
        "{run}"
    );
    assert!(run.contains("fake-game stdout line\n"), "{run}");
    assert!(run.contains("# exit: 0\n"), "{run}");
    let launch_log = w.launch_log();
    assert!(
        launch_log.contains("under wrapper: fake-wrap"),
        "{launch_log}"
    );
    assert!(
        launch_log.contains("wrapper fake-wrap exited 0"),
        "{launch_log}"
    );
    assert_eq!(w.run_scratch(), Vec::<String>::new());
}

#[test]
fn a_close_request_reaches_the_game_through_any_wrapper() {
    for (name, body) in [
        ("exec", WRAP_EXEC),
        ("parent", WRAP_PARENT),
        ("session", WRAP_SESSION),
        ("daemon", WRAP_DAEMON),
    ] {
        let Some(w) = World::launcher(&format!("wrap-close-{name}"), false) else {
            return;
        };
        w.wrapped(body, &[]);
        let log = w.wrap_log();
        let (code, _) = term_hanging_launcher(
            &w,
            &[
                ("FAKE_CLOSE_WORKS", "1"),
                ("FAKE_WRAP_LOG", log.to_str().unwrap()),
            ],
        );
        assert_eq!(code, Some(143), "{name}");
        assert!(
            prefix_procs(&w).is_empty(),
            "{name}: {:?}",
            prefix_procs(&w)
        );
        assert!(wait_until(5, || w.wrapper_gone()), "{name}");
        let l = w.launch_log();
        assert!(l.contains("asked the game to close"), "{name}: {l}");
        assert!(l.contains("closed by itself after "), "{name}: {l}");
        assert!(!l.contains("didn't close"), "{name}: {l}");
        assert!(!l.contains("reaped (SIGKILL)"), "{name}: {l}");
        let umu = std::fs::read_to_string(&w.umu_log).unwrap();
        assert!(
            umu.contains("CLOSE:wineboot --end-session\n"),
            "{name}: {umu}"
        );
        assert_eq!(w.run_scratch(), Vec::<String>::new(), "{name}");
    }
}

#[test]
fn term_reaches_the_game_through_any_wrapper() {
    for (name, body) in [
        ("exec", WRAP_EXEC),
        ("parent", WRAP_PARENT),
        ("session", WRAP_SESSION),
        ("daemon", WRAP_DAEMON),
    ] {
        let Some(w) = World::launcher(&format!("wrap-term-{name}"), false) else {
            return;
        };
        w.wrapped(body, &[]);
        let log = w.wrap_log();
        let mut child = w
            .launch_cmd(
                &[
                    ("FAKE_GAME_TRAP_TERM", "1"),
                    ("GOGO_CLOSE_GRACE_SECS", "1"),
                    ("FAKE_WRAP_LOG", log.to_str().unwrap()),
                ],
                &[],
            )
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let umu_log = || std::fs::read_to_string(&w.umu_log).unwrap_or_default();
        assert!(wait_until(10, || umu_log().contains("TRAPPING")), "{name}");
        std::process::Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(143), "{name}");
        assert!(umu_log().contains("GOT_TERM"), "{name}: {}", umu_log());
        assert!(wait_until(5, || prefix_procs(&w).is_empty()), "{name}");
        assert!(wait_until(5, || w.wrapper_gone()), "{name}");
        let l = w.launch_log();
        assert!(l.contains("no wineserver of this prefix"), "{name}: {l}");
        assert!(l.contains("didn't close in 1s: TERM"), "{name}: {l}");
    }
}

/// The game answered and left, the wrapper hasn't: that isn't "closed", and
/// the launcher still ends and takes the wrapper with it.
#[test]
fn a_wrapper_that_outlives_the_game_is_not_a_clean_close() {
    let Some(w) = World::launcher("wrap-linger", false) else {
        return;
    };
    w.wrapped(WRAP_LINGER, &[]);
    let log = w.wrap_log();
    let (code, secs) = term_hanging_launcher(
        &w,
        &[
            ("FAKE_CLOSE_WORKS", "1"),
            ("GOGO_CLOSE_GRACE_SECS", "2"),
            ("FAKE_WRAP_LOG", log.to_str().unwrap()),
        ],
    );
    assert_eq!(code, Some(143));
    assert!(secs < 12, "{secs}");
    assert!(wait_until(5, || w.wrapper_gone()));
    let l = w.launch_log();
    assert!(l.contains("didn't close in 2s: TERM"), "{l}");
    assert!(!l.contains("closed by itself"), "{l}");
}

#[test]
fn a_launcher_killed_under_a_wrapper_still_tears_it_all_down() {
    for (name, body) in [("parent", WRAP_PARENT), ("session", WRAP_SESSION)] {
        let Some(w) = World::launcher(&format!("wrap-kill-{name}"), false) else {
            return;
        };
        w.wrapped(body, &[]);
        let log = w.wrap_log();
        let mut child = w
            .launch_cmd(
                &[
                    ("FAKE_GAME_HANG", "1"),
                    ("FAKE_WRAP_LOG", log.to_str().unwrap()),
                ],
                &[],
            )
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        assert!(hang_ready(&w), "{name}");
        assert!(
            wait_until(5, || w.launch_log().contains("guard armed")),
            "{name}"
        );
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(wait_until(15, || prefix_procs(&w).is_empty()), "{name}");
        assert!(wait_until(5, || w.wrapper_gone()), "{name}");
        assert!(
            wait_until(5, || w.launch_log().contains("watchdog done")),
            "{name}"
        );
        assert_eq!(w.run_scratch(), Vec::<String>::new(), "{name}");
    }
}

/// A wrapper that has left while the game plays on: the launcher keeps
/// waiting, and the footer comes when the game has gone.
#[test]
fn a_wrapper_that_leaves_does_not_end_the_run() {
    let Some(w) = World::launcher("wrap-daemon", false) else {
        return;
    };
    w.wrapped(WRAP_DAEMON, &[]);
    let log = w.wrap_log();
    let t0 = std::time::Instant::now();
    let out = w.launch(
        &[
            ("FAKE_GAME_SLEEP", "4"),
            ("FAKE_WRAP_LOG", log.to_str().unwrap()),
        ],
        &[],
    );
    assert!(t0.elapsed().as_secs() >= 4, "{:?}", t0.elapsed());
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(w.game_runs(), 1);
    assert!(prefix_procs(&w).is_empty());
    let l = w.launch_log();
    assert!(l.contains("wrapper fake-wrap exited 0"), "{l}");
    assert!(l.contains("prefix idle"), "{l}");
    assert!(!l.contains("never ran the game"), "{l}");
}

/// gamescope's shape: it drops its child's exit code (always 0), but the
/// game step records umu-run's, which is the run's (design 18 §7.8), and a
/// short run still explains itself.
#[test]
fn the_runs_exit_code_is_the_games_under_a_wrapper_that_drops_it() {
    for (name, env, want) in [
        ("code", vec![("FAKE_GAME_EXIT", "7")], 7),
        (
            "sleep",
            vec![("FAKE_GAME_EXIT", "9"), ("FAKE_GAME_SLEEP", "2")],
            9,
        ),
        ("signal", vec![("FAKE_GAME_SIGNAL", "SEGV")], 139),
    ] {
        let Some(w) = World::launcher(&format!("wrap-code-{name}"), false) else {
            return;
        };
        w.wrapped(WRAP_PARENT, &[]);
        let log = w.wrap_log();
        let stub = w.dialog_stub(&["Close"]);
        let mut e = vec![
            ("FAKE_WRAP_LOG", log.to_str().unwrap()),
            ("GOGO_LAUNCHER_DIALOG", stub.to_str().unwrap()),
        ];
        e.extend(env);
        let out = w.launch(&e, &[]);
        assert_eq!(out.status.code(), Some(want), "{name}: {out:?}");
        let r = w.runs().pop().unwrap();
        assert!(r.contains(&format!("# exit: {want}\n")), "{name}: {r}");
        let d = w.dialogs().pop().unwrap();
        assert!(d.contains(&format!("exit code {want}")), "{name}: {d}");
        assert!(d.contains(", under fake-wrap."), "{name}: {d}");
        let l = w.launch_log();
        assert!(
            l.contains(&format!(
                "wrapper fake-wrap exited 0; the game exited {want}"
            )),
            "{name}: {l}"
        );
        assert!(prefix_procs(&w).is_empty(), "{name}");
        assert_eq!(w.run_scratch(), Vec::<String>::new(), "{name}");
    }
}

/// A wrapper that TERMs the game step, and a game that quits cleanly on it:
/// the step may see umu-run gone before it looks, and still records 0 (not
/// the 143 of the interrupted wait).
#[test]
fn a_game_that_quits_cleanly_on_the_wrappers_term_is_exit_0() {
    for i in 0..3 {
        let Some(w) = World::launcher(&format!("wrap-term-ok-{i}"), false) else {
            return;
        };
        w.wrapped(WRAP_TERM_CHILD, &[]);
        let log = w.wrap_log();
        let stub = w.dialog_stub(&["Close"]);
        let out = w.launch(
            &[
                ("FAKE_GAME_TERM_OK", "1"),
                ("FAKE_WRAP_LOG", log.to_str().unwrap()),
                ("GOGO_LAUNCHER_DIALOG", stub.to_str().unwrap()),
                ("GOGO_EARLY_EXIT_SECS", "0"),
            ],
            &[],
        );
        assert_eq!(out.status.code(), Some(0), "{i}: {out:?}");
        let r = w.runs().pop().unwrap();
        assert!(r.contains("# exit: 0\n"), "{i}: {r}");
        assert!(w.dialogs().is_empty(), "{i}: {:?}", w.dialogs());
        assert!(prefix_procs(&w).is_empty(), "{i}");
    }
}

#[test]
fn a_wrapper_that_never_runs_the_game_says_so_and_offers_no_renderer() {
    for (name, body, want) in [
        ("fails", "echo 'wrap: no display' >&2\nexit 3\n", 3),
        ("quits", "exit 0\n", 1),
    ] {
        let Some(w) = World::launcher(&format!("wrap-never-{name}"), false) else {
            return;
        };
        w.wrapped(body, &[]);
        let log = w.wrap_log();
        let stub = w.dialog_stub(&["OK"]);
        let out = w.launch(
            &[
                ("FAKE_WRAP_LOG", log.to_str().unwrap()),
                ("GOGO_LAUNCHER_DIALOG", stub.to_str().unwrap()),
            ],
            &[],
        );
        assert_eq!(out.status.code(), Some(want), "{name}: {out:?}");
        assert_eq!(w.game_runs(), 0, "{name}");
        let run = w.runs().pop().unwrap();
        assert!(
            run.contains(&format!("# exit: {want} (wrapper never ran the game)\n")),
            "{name}: {run}"
        );
        let dialogs = w.dialogs();
        assert_eq!(dialogs.len(), 1, "{name}: {dialogs:?}");
        assert!(
            dialogs[0].contains("before it started the game"),
            "{name}: {dialogs:?}"
        );
        assert!(
            dialogs[0].contains(&format!("wrapper {ID} --clear")),
            "{name}: {dialogs:?}"
        );
        assert!(
            !dialogs[0].contains("Try next renderer"),
            "{name}: {dialogs:?}"
        );
        assert_eq!(w.run_scratch(), Vec::<String>::new(), "{name}");
    }
    // A wrapper the launch can't find stops it before anything starts.
    let Some(w) = World::launcher("wrap-missing", false) else {
        return;
    };
    w.wrapped(WRAP_EXEC, &[]);
    std::fs::remove_file(w.root.join("bin/fake-wrap")).unwrap();
    let stub = w.dialog_stub(&["OK"]);
    let out = w.launch(&[("GOGO_LAUNCHER_DIALOG", stub.to_str().unwrap())], &[]);
    assert_eq!(out.status.code(), Some(127), "{out:?}");
    assert_eq!(w.game_runs(), 0);
    let run = w.runs().pop().unwrap();
    assert!(run.contains("# exit: 127 (wrapper missing)\n"), "{run}");
    let d = w.dialogs().pop().unwrap();
    assert!(d.contains(&format!("wrapper {ID} --clear")), "{d}");
}

/// The setting from `envargs` to `doctor` and the catalogue: pending, applied,
/// shown, the program looked for, a hand-written refusal, cleared.
#[test]
fn the_wrapper_shows_up_where_a_player_looks() {
    use gogo::linuxapp::catalogue::{self, Applies};
    use gogo::linuxapp::envargs;
    use gogo::macapp::compat::Layer;
    use gogo::macapp::doctor::Level;
    let Some(w) = World::launcher("wrap-shown", false) else {
        return;
    };
    let launch_finding = |h: &gogo::linuxapp::doctor::Host| {
        let r = gogo::linuxapp::doctor::doctor(ID, h).unwrap();
        r.findings
            .into_iter()
            .find(|f| f.check == "launch")
            .unwrap()
    };
    assert_eq!(envargs::wrapper_report(ID).unwrap().wrapper, None);
    assert!(envargs::env_report(ID, false).unwrap().wrapper.is_none());

    // Refused: nothing written.
    for bad in [
        vec![],
        vec!["gamescope -f --".to_string()],
        vec!["a\nb".to_string()],
    ] {
        assert!(envargs::set_wrapper(ID, Some(bad)).is_err());
    }
    let l4 = w.home.join(format!("compat/{ID}.toml"));
    assert!(!l4.exists());

    let words: Vec<String> = ["gamemoderun", "gamescope", "-f", "--"]
        .map(String::from)
        .to_vec();
    let missing = envargs::set_wrapper(ID, Some(vec!["gogo-no-such-tool".into()])).unwrap();
    assert!(
        missing.warnings.iter().any(|w| w.contains("isn't on PATH")),
        "{:?}",
        missing.warnings
    );
    let out = envargs::set_wrapper(ID, Some(words.clone())).unwrap();
    assert_eq!(out.changes[0].key, "wrapper");
    assert!(out.built);
    assert!(
        std::fs::read_to_string(&l4)
            .unwrap()
            .contains("[platforms.linux]")
    );
    let v = envargs::env_report(ID, false).unwrap().wrapper.unwrap();
    assert!(v.pending);
    assert_eq!(v.words, words);
    assert_eq!(v.layer, Some(Layer::User));

    // Applied by setup; nothing else in launch.conf cares.
    setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert!(
        !envargs::wrapper_report(ID)
            .unwrap()
            .wrapper
            .unwrap()
            .pending
    );
    assert!(
        w.conf()
            .contains("WRAPPER=('gamemoderun' 'gamescope' '-f' '--')\n"),
        "{}",
        w.conf()
    );

    // Doctor looks where the launch will: not on PATH warns, on PATH doesn't.
    let mut host = doctor_host(&w, &[("radeon", 64)]);
    let f = launch_finding(&host);
    assert_eq!(f.level, Level::Warn, "{f:?}");
    assert!(
        f.message.contains("wrapper gamemoderun gamescope -f --"),
        "{}",
        f.message
    );
    assert!(
        f.message.contains("`gamemoderun` isn't on PATH"),
        "{}",
        f.message
    );

    // The catalogue: `no` for the missing tool; `yes` and shown once there.
    let on_host = host.root.join("host-bin");
    std::fs::create_dir_all(&on_host).unwrap();
    for t in ["gamescope", "gamemoderun"] {
        std::fs::write(on_host.join(t), "#!/bin/sh\n").unwrap();
        chmod_x(&on_host.join(t));
    }
    let r = catalogue::report(ID, &host, false).unwrap();
    assert!(
        matches!(&r.find("gamescope").unwrap().applies, Applies::No { why } if why.contains("isn't on PATH")),
        "{:?}",
        r.find("gamescope")
    );
    host.env = Box::new(|k| {
        if k == "PATH" {
            Some("/host-bin".into())
        } else {
            (!k.starts_with("VK_") && k != "XDG_DATA_DIRS")
                .then(|| std::env::var_os(k))
                .flatten()
        }
    });
    let found = host;
    assert_eq!(launch_finding(&found).level, Level::Ok);
    let r = catalogue::report(ID, &found, false).unwrap();
    for name in ["gamescope", "gamemoderun"] {
        let e = r.find(name).unwrap();
        assert_eq!(e.applies, Applies::Yes, "{name}");
        assert_eq!(e.active.len(), 1, "{name}");
        assert_eq!(e.active[0].key, "wrapper");
        assert_eq!(e.active[0].value, "gamemoderun gamescope -f --");
    }

    // A wrapper hand-written as a shell string: setup runs the game without
    // it and says so, as does doctor.
    std::fs::write(&l4, "[platforms.linux]\nwrapper = [\"gamescope -f --\"]\n").unwrap();
    let out = setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert!(
        out.warnings
            .iter()
            .any(|w| w.contains("running the game without it")),
        "{:?}",
        out.warnings
    );
    assert!(!w.conf().contains("WRAPPER="), "{}", w.conf());
    let f = launch_finding(&found);
    assert_eq!(f.level, Level::Warn, "{f:?}");
    assert!(f.message.contains("setup skips it"), "{}", f.message);

    // Cleared: gone from the file; pending until setup drops it.
    envargs::set_wrapper(ID, Some(words)).unwrap();
    setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    envargs::set_wrapper(ID, None).unwrap();
    assert!(!std::fs::read_to_string(&l4).unwrap().contains("wrapper"));
    let v = envargs::wrapper_report(ID).unwrap().wrapper.unwrap();
    assert!(v.pending && v.words.is_empty());
    setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert!(!w.conf().contains("WRAPPER="));
    assert_eq!(envargs::wrapper_report(ID).unwrap().wrapper, None);
}

// — the game library (#276, design 18 §6.1) —

/// A game's menu entry, as the build wrote it.
fn desktop_exec() -> String {
    let entry = std::fs::read_to_string(gogo::linuxapp::game::desktop_path(ID)).unwrap();
    entry
        .lines()
        .find_map(|l| l.strip_prefix("Exec="))
        .unwrap()
        .to_string()
}

#[test]
fn without_a_library_nothing_new_appears_and_games_stay_under_the_data_root() {
    let w = World::new("nolib", &["d3d9.dll"]);
    let out = build_with(&w.opts());
    assert_eq!(out.dirs.root, w.home.join("games").join(ID));
    assert!(!w.locations().exists(), "no record for a game under games/");
    let conf = w.conf();
    assert!(!conf.contains("LIBRARY_GAME"), "{conf}");
    assert_eq!(conf_value(&conf, "PREFIX"), format!("'games/{ID}/prefix'"));
    assert!(!w.log().contains("SHARED:"), "{}", w.log());
    assert_eq!(gogo::macapp::launch::recorded_games(), vec![]);

    // A library that is games/ itself is no library: nothing changes either.
    let same = Library::resolve(&w.home.join("games"), None).unwrap();
    assert_eq!(same, None);
}

#[test]
fn a_game_builds_into_a_library_and_everything_finds_it_there() {
    use gogo::macapp::{apps, launch::GameDirs};
    let w = World::new("library", &["d3d9.dll"]);
    let lib = w.library();
    let root = lib.dir().join(ID);
    let plan = build::plan(&w.opts_in_library(), None, &mut quiet()).unwrap();
    assert_eq!(plan.state.root, root);
    assert!(!root.exists(), "a plan builds nothing");
    assert!(!w.locations().exists(), "a plan records nothing");

    let out = build::execute(&plan, None, &mut quiet()).unwrap();
    // Only games/<gogid> moved: the data root keeps the rest.
    assert_eq!(out.dirs.root, root);
    assert!(root.join("game/game.exe").is_file());
    assert!(root.join("prefix/drive_c/GOG/Test Game/game.exe").is_file());
    assert!(!w.home.join("games").join(ID).exists());
    assert_eq!(
        std::fs::read_to_string(w.locations().join(ID)).unwrap(),
        format!("{}\n", root.display())
    );
    assert_eq!(GameDirs::for_game(ID).unwrap().root, root);

    // launch.conf: absolute game paths, the data root named, marked.
    let conf = w.conf();
    assert_eq!(
        conf_value(&conf, "PREFIX"),
        format!("'{}'", root.join("prefix").display())
    );
    assert_eq!(conf_value(&conf, "LIBRARY_GAME"), "'1'");
    assert_eq!(
        conf_value(&conf, "DATA_ROOT"),
        format!("'{}'", w.home.display())
    );
    // The container gets the game's dir, for every umu run.
    assert!(
        w.log().contains(&format!("SHARED:{}\n", root.display())),
        "{}",
        w.log()
    );

    // The menu entry runs the launcher where it is.
    assert!(
        desktop_exec().contains("big drive/games"),
        "{}",
        desktop_exec()
    );

    // Setup again, from the record alone: the setting is gone.
    setup::setup(
        ID,
        &SetupOptions {
            refresh: false,
            prompt: None,
            runtime_snapshot: None,
        },
        &mut quiet(),
    )
    .unwrap();

    // apps list / show, the store's references, env/args and doctor.
    let logs = w.root.join("logs");
    let listed = apps::list(&logs).unwrap();
    assert_eq!(listed.len(), 1);
    let a = &listed[0];
    assert_eq!(a.gogid, ID);
    assert_eq!(a.root, root);
    assert!(a.linux && a.launcher && a.desktop_present && !a.unreachable);
    assert!(a.bytes > 0);
    assert_eq!(apps::get(ID, &logs).unwrap().unwrap().root, root);
    assert!(
        gogo::macapp::store::references()
            .iter()
            .any(|r| r.file == root.join("state.toml")),
        "the store's GC sees what a library game uses"
    );
    let host = doctor_host(&w, &[]);
    let report = gogo::linuxapp::doctor::doctor(ID, &host).unwrap();
    let find = |check: &str| {
        report
            .findings
            .iter()
            .find(|f| f.check == check)
            .unwrap_or_else(|| panic!("no {check} finding"))
    };
    assert_eq!(find("location").level, gogo::macapp::doctor::Level::Ok);
    assert!(find("location").message.contains("big drive"));
    assert_eq!(
        find("links").level,
        gogo::macapp::doctor::Level::Ok,
        "the prefix's link to game/ stays in the game's dir: {:?}",
        find("links")
    );

    // Removal takes the game's dir, the record and the menu entry.
    let desktop = gogo::linuxapp::game::desktop_path(ID);
    let removed = apps::remove(a, false).unwrap();
    assert!(removed.paths.contains(&root), "{:?}", removed.paths);
    assert_eq!(removed.left_behind, None);
    assert!(!root.exists() && !desktop.exists());
    assert!(!w.locations().join(ID).exists());
    assert!(lib.dir().is_dir(), "the library itself stays");
    assert!(apps::list(&logs).unwrap().is_empty());
}

#[test]
fn changing_the_setting_moves_no_game() {
    use gogo::macapp::launch::GameDirs;
    let w = World::new("setting", &["d3d9.dll"]);
    let default_root = w.home.join("games").join(ID);
    // Built before the setting: a rebuild with a library stays, and says so.
    build_with(&w.opts());
    let mut warned = Vec::new();
    let plan = build::plan(&w.opts_in_library(), None, &mut |ev| {
        if let ProgressEvent::Warn { message } = ev {
            warned.push(message.clone());
        }
    })
    .unwrap();
    assert_eq!(plan.state.root, default_root);
    assert!(plan.rebuild);
    assert!(
        warned.iter().any(|m| m.contains("doesn't move games")),
        "{warned:?}"
    );
    build::execute(&plan, None, &mut quiet()).unwrap();
    assert!(!w.locations().exists());
    assert_eq!(GameDirs::for_game(ID).unwrap().root, default_root);
    gogo::macapp::apps::remove(
        &gogo::macapp::apps::get(ID, &w.root.join("logs"))
            .unwrap()
            .unwrap(),
        false,
    )
    .unwrap();

    // Built in a library: a rebuild without the setting stays there.
    let lib_root = w.library().dir().join(ID);
    build_with(&w.opts_in_library());
    let out = build_with(&w.opts());
    assert_eq!(out.dirs.root, lib_root);
    assert!(!default_root.exists());
    assert_eq!(GameDirs::for_game(ID).unwrap().root, lib_root);
}

#[test]
fn an_installer_build_stages_and_lands_in_the_library() {
    let w = World::new("libinst", &["d3d9.dll"]);
    let setup_exe = w.root.join("setup_test_game_1.0.exe");
    std::fs::write(&setup_exe, "MZ").unwrap();
    let lib = w.library();
    let opts = LinuxAppOptions {
        installer: Some(setup_exe),
        runtime: Some(format!("path:{}", w.build.display())),
        name: Some("Test Game".into()),
        library: Some(lib.clone()),
        ..Default::default()
    };
    let plan = build::plan(&opts, None, &mut quiet()).unwrap();
    assert!(
        plan.state.root.starts_with(lib.dir()),
        "{:?}",
        plan.state.root
    );
    let staging = plan.state.root.clone();
    let out = build::execute(&plan, None, &mut quiet()).unwrap();
    assert_eq!(out.dirs.root, lib.dir().join(ID));
    assert!(!staging.exists());
    assert!(out.dirs.root.join("gogo-launch.toml").is_file());
    assert_eq!(
        gogo::macapp::launch::recorded_location(ID),
        Some(out.dirs.root)
    );
}

#[test]
fn a_build_that_fails_early_leaves_a_game_the_next_build_can_finish() {
    let w = World::new("libfail", &["d3d9.dll"]);
    let mut plan = build::plan(&w.opts_in_library(), None, &mut quiet()).unwrap();
    // The source tree goes away after the plan.
    plan.game_tree = Some(w.root.join("no-such-tree"));
    assert!(build::execute(&plan, None, &mut quiet()).is_err());
    assert!(
        plan.state.root.is_dir(),
        "no record of a dir that isn't there"
    );
    let out = build_with(&w.opts_in_library());
    assert_eq!(out.dirs.root, plan.state.root);
}

#[test]
fn removing_a_symlinked_game_dir_never_follows_it() {
    use gogo::macapp::apps;
    let w = World::new("symlink", &["d3d9.dll"]);
    let logs = w.root.join("logs");
    for in_library in [true, false] {
        let opts = if in_library {
            w.opts_in_library()
        } else {
            w.opts()
        };
        let out = build_with(&opts);
        let root = out.dirs.root;
        // The game's dir is moved away and linked back.
        let elsewhere = w.root.join("elsewhere");
        std::fs::rename(&root, &elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &root).unwrap();

        let a = apps::get(ID, &logs).unwrap().unwrap();
        let removed = apps::remove(&a, false).unwrap();
        assert_eq!(removed.left_behind.as_deref(), Some(elsewhere.as_path()));
        assert_eq!(removed.bytes, 0);
        assert!(root.symlink_metadata().is_err(), "the link goes");
        assert!(
            elsewhere.join("game/game.exe").is_file(),
            "what it points at stays"
        );
        assert!(!w.locations().join(ID).exists());
        std::fs::remove_dir_all(&elsewhere).unwrap();
    }
}

/// `0o555` on `dirs`; `unlock_dirs` restores them so tempdir cleanup works.
fn lock_dirs(dirs: &[&Path]) {
    use std::os::unix::fs::PermissionsExt as _;
    for d in dirs {
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o555)).unwrap();
    }
}

fn unlock_dirs(dirs: &[&Path]) {
    use std::os::unix::fs::PermissionsExt as _;
    for d in dirs.iter().rev() {
        let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o755));
    }
}

#[test]
fn removing_a_game_with_read_only_dirs_in_its_prefix_takes_all_of_it() {
    use gogo::macapp::apps;
    let w = World::new("ro-remove", &["d3d9.dll"]);
    let logs = w.root.join("logs");
    let root = build_with(&w.opts()).dirs.root;
    let ro = root.join("prefix/drive_c/ro");
    std::fs::create_dir_all(ro.join("sub")).unwrap();
    std::fs::write(ro.join("f"), "x").unwrap();
    std::fs::write(ro.join("sub/g"), "x").unwrap();
    let locked = [ro.as_path(), &ro.join("sub")];
    lock_dirs(&[&ro.join("sub"), &ro]);
    let a = apps::get(ID, &logs).unwrap().unwrap();
    let res = apps::remove(&a, false);
    if ro.exists() {
        unlock_dirs(&locked); // let the tempdir cleanup work on a failure
    }
    res.unwrap();
    assert!(!root.exists(), "the whole game is gone");
}

#[test]
fn removing_a_symlinked_game_dir_leaves_a_read_only_target_alone() {
    use gogo::macapp::apps;
    use std::os::unix::fs::PermissionsExt as _;
    let w = World::new("ro-symlink", &["d3d9.dll"]);
    let logs = w.root.join("logs");
    let root = build_with(&w.opts()).dirs.root;
    let elsewhere = w.root.join("elsewhere");
    std::fs::rename(&root, &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &root).unwrap();
    let ro = elsewhere.join("prefix/drive_c/ro");
    std::fs::create_dir_all(&ro).unwrap();
    std::fs::write(ro.join("f"), "x").unwrap();
    lock_dirs(&[&ro, &elsewhere]);
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

    let a = apps::get(ID, &logs).unwrap().unwrap();
    let res = apps::remove(&a, false);
    let modes = (mode(&elsewhere), mode(&ro));
    unlock_dirs(&[&elsewhere, &ro]);
    res.unwrap();
    assert!(root.symlink_metadata().is_err(), "the link goes");
    assert!(ro.join("f").is_file(), "the target stays");
    assert_eq!(modes, (0o555, 0o555), "and keeps its modes");
}

#[test]
fn a_library_game_whose_drive_is_gone_is_listed_kept_from_gc_and_can_be_forgotten() {
    use gogo::macapp::{apps, store};
    let w = World::new("gone", &["d3d9.dll"]);
    let logs = w.root.join("logs");
    let root = build_with(&w.opts_in_library()).dirs.root;
    let away = w.root.join("unmounted");
    std::fs::rename(&root, &away).unwrap();

    let listed = apps::list(&logs).unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].unreachable && !listed[0].orphaned());
    assert_eq!(listed[0].root, root);

    // Its references can't be read, so nothing can be called unused.
    let err = store::prune(&["ge-proton".to_string()]).unwrap_err();
    assert!(err.to_string().contains(ID), "{err}");

    let host = doctor_host(&w, &[]);
    let report = gogo::linuxapp::doctor::doctor(ID, &host).unwrap();
    let f = report
        .findings
        .iter()
        .find(|f| f.check == "location")
        .unwrap();
    assert_eq!(f.level, gogo::macapp::doctor::Level::Fail);

    // A build doesn't fill the mount point of a drive that isn't mounted.
    let err = build::plan(&w.opts(), None, &mut quiet()).unwrap_err();
    assert!(err.to_string().contains("apps remove"), "{err}");
    assert!(!root.exists());

    let desktop = gogo::linuxapp::game::desktop_path(ID);
    assert!(desktop.is_file());
    let removed = apps::remove(&listed[0], false).unwrap();
    assert_eq!(removed.left_behind.as_deref(), Some(root.as_path()));
    assert!(!w.locations().join(ID).exists() && !desktop.exists());
    assert!(away.join("game/game.exe").is_file());
    assert!(apps::list(&logs).unwrap().is_empty());
}

#[test]
fn a_location_record_gogo_did_not_write_is_not_trusted() {
    use gogo::macapp::launch::GameDirs;
    let w = World::new("tamper", &["d3d9.dll"]);
    std::fs::create_dir_all(w.locations()).unwrap();
    let default_root = w.home.join("games").join(ID);
    for bad in [
        "relative/path/1999999901\n".to_string(),
        "/home/someone\n".to_string(),
        format!("/data/../{ID}/x\n"),
        format!("/data/../{ID}\n"),
    ] {
        std::fs::write(w.locations().join(ID), &bad).unwrap();
        assert_eq!(GameDirs::for_game(ID).unwrap().root, default_root, "{bad}");
    }
}

#[test]
fn the_launcher_of_a_library_game_keeps_gogos_data_root() {
    let Some(w) = World::launcher_with("liblaunch", false, true) else {
        return;
    };
    let root = w.library().dir().join(ID);
    assert_eq!(w.game(), root);

    let out = w.launch(&[], &[]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(w.calls().is_empty(), "fresh stamp: {:?}", w.calls());
    // The game ran from the library, sharing its dir with the container.
    let log = w.log();
    let env = log.lines().rfind(|l| l.starts_with("ENV:")).unwrap();
    assert!(
        env.contains(&format!("WINEPREFIX={}", root.join("prefix").display())),
        "{env}"
    );
    assert!(
        log.contains(&format!("GAME_SHARED:{}\n", root.display())),
        "{log}"
    );
    // Logs stay under the data root, not the library's parent.
    assert_eq!(w.runs().len(), 1, "run logs under {:?}", w.logs());
    assert!(w.launch_log().contains("launcher 15"));
    assert!(!w.root.join("big drive/logs").exists());

    // Stale config: setup runs with gogo's data root as GOGO_HOME, and from
    // there finds the game through its record.
    let l4 = w.home.join("compat").join(format!("{ID}.toml"));
    std::fs::create_dir_all(l4.parent().unwrap()).unwrap();
    std::fs::write(&l4, "# edited\n").unwrap();
    assert_eq!(w.launch(&[], &[]).status.code(), Some(0));
    assert_eq!(
        w.calls(),
        vec![format!(
            "GOGO_HOME={} linux-app setup {ID}",
            w.home.display()
        )]
    );
    assert!(w.launch_log().contains("config stale"));
    // The fake's restamp reached the library's launch.conf: no second setup.
    assert_eq!(w.launch(&[], &[]).status.code(), Some(0));
    assert_eq!(w.calls().len(), 1);
}

#[test]
fn a_library_game_outlives_a_data_root_that_moved_back_to_the_default() {
    // launch.conf's DATA_ROOT is gone: the launcher falls back to gogo's
    // default root, not to the library's parent.
    let Some(w) = World::launcher_with("libgone", false, true) else {
        return;
    };
    w.edit_conf(&[("DATA_ROOT", "'/nonexistent/gogo-root'")]);
    let out = w.launch(&[], &[]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(
        w.user_home()
            .join(".local/state/gogo/logs")
            .join(ID)
            .join("launch.log")
            .is_file()
    );
}

// — moving a built game (#278, design 18 §6.1) —

mod moving {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    use gogo::macapp::relocate::{Faults, KilledAt, Method, MoveEvent, MoveOptions, Target};

    use super::*;

    const SAVE: &str = "prefix/drive_c/users/steamuser/Saved Games/slot one.sav";

    fn go(target: &Target, faults: Faults) -> Result<gogo::macapp::relocate::MoveOutcome, String> {
        gogo::linuxapp::relocate::move_game(
            ID,
            target,
            &MoveOptions {
                faults,
                ..MoveOptions::default()
            },
            &mut |_| {},
        )
        .map_err(|e| e.to_string())
    }

    fn go_with(
        _w: &World,
        target: &Target,
        faults: Faults,
        emit: &mut dyn FnMut(&MoveEvent),
    ) -> Result<gogo::macapp::relocate::MoveOutcome, String> {
        gogo::linuxapp::relocate::move_game(
            ID,
            target,
            &MoveOptions {
                faults,
                ..MoveOptions::default()
            },
            emit,
        )
        .map_err(|e| e.to_string())
    }

    fn across() -> Faults {
        Faults {
            across_filesystems: true,
            ..Faults::default()
        }
    }

    /// A built game with an icon, so its menu entry has one to follow.
    fn built(name: &str) -> World {
        let w = World::new(name, &["d3d9.dll"]);
        std::fs::write(w.tree.join(format!("goggame-{ID}.ico")), tiny_ico()).unwrap();
        build_with(&w.opts());
        w
    }

    /// What Proton leaves that a move must keep: a save with an attribute Wine
    /// would write, links out to the build and inside the prefix, a read-only
    /// dir and a stray FIFO.
    fn dress(w: &World) {
        let root = w.game();
        let save = root.join(SAVE);
        std::fs::create_dir_all(save.parent().unwrap()).unwrap();
        std::fs::write(&save, b"level 42").unwrap();
        let prefix = root.join("prefix");
        let notepad = prefix.join("drive_c/windows/notepad.exe");
        std::os::unix::fs::symlink(w.build.join("proton"), &notepad).unwrap();
        let _ = std::os::unix::fs::symlink(".", prefix.join("pfx"));
        let ro = prefix.join("drive_c/read only");
        std::fs::create_dir_all(&ro).unwrap();
        std::fs::write(ro.join("f"), "x").unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
        std::fs::set_permissions(ro.join("f"), std::fs::Permissions::from_mode(0o444)).unwrap();
    }

    /// A test's own cleanup can't delete a read-only dir.
    fn undress(root: &Path) {
        let ro = root.join("prefix/drive_c/read only");
        let _ = std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755));
    }

    /// The moved game is whole, and everything finds it at `root`.
    fn assert_game_at(w: &World, root: &Path) {
        assert_eq!(GameDirs::for_game(ID).unwrap().root, root);
        assert_eq!(std::fs::read(root.join(SAVE)).unwrap(), b"level 42");
        let prefix = root.join("prefix");
        // Links came as links, the relative one to game/ still leading there.
        let gog = prefix.join("drive_c/GOG/Test Game");
        assert!(std::fs::read_link(&gog).unwrap().is_relative());
        assert!(gog.join("game.exe").is_file(), "{}", gog.display());
        assert_eq!(
            std::fs::read_link(prefix.join("drive_c/windows/notepad.exe")).unwrap(),
            w.build.join("proton")
        );
        assert_eq!(
            std::fs::read_link(prefix.join("pfx")).unwrap(),
            Path::new(".")
        );
        let ro = prefix.join("drive_c/read only");
        assert_eq!(ro.metadata().unwrap().mode() & 0o777, 0o555);
        assert_eq!(ro.join("f").metadata().unwrap().mode() & 0o777, 0o444);

        // launch.conf and the menu entry say so.
        let conf = std::fs::read_to_string(root.join("launch.conf")).unwrap();
        let in_lib = gogo::macapp::launch::in_library(root);
        let shown = if in_lib {
            root.display().to_string()
        } else {
            format!("games/{ID}")
        };
        for key in ["PREFIX", "WORKDIR", "EXE", "GAME_DIR"] {
            assert!(
                conf_value(&conf, key).starts_with(&format!("'{shown}/")),
                "{key}: {conf}"
            );
        }
        assert_eq!(conf.contains("LIBRARY_GAME='1'"), in_lib, "{conf}");
        assert!(
            desktop_exec().contains(&root.join("launch.sh").display().to_string()),
            "{}",
            desktop_exec()
        );
        let entry = std::fs::read_to_string(gogo::linuxapp::game::desktop_path(ID)).unwrap();
        assert!(
            entry.contains(&format!("Icon={}", root.join("icon.png").display())),
            "{entry}"
        );

        // `apps` and `doctor` agree.
        let logs = w.root.join("logs");
        let listed = gogo::macapp::apps::list(&logs).unwrap();
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].root, root);
        assert!(!listed[0].unreachable && listed[0].desktop_present);
        let report = gogo::linuxapp::doctor::doctor(ID, &doctor_host(w, &[])).unwrap();
        let find = |check: &str| report.findings.iter().find(|f| f.check == check);
        assert!(find("move").is_none(), "{:?}", report.findings);
        assert_eq!(
            find("links").unwrap().level,
            gogo::macapp::doctor::Level::Ok,
            "{:?}",
            find("links")
        );
        assert_eq!(find("location").is_some(), in_lib);
        assert!(!w.home.join("game-moves").join(ID).exists());
    }

    /// Setup, run again from where the game now is, writes the same
    /// `launch.conf`: the move's rewrite is what setup would have done.
    fn assert_setup_agrees(root: &Path) {
        let before = std::fs::read_to_string(root.join("launch.conf")).unwrap();
        setup::setup(
            ID,
            &SetupOptions {
                refresh: false,
                prompt: None,
                runtime_snapshot: None,
            },
            &mut quiet(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("launch.conf")).unwrap(),
            before
        );
    }

    fn both_ways(w: &World, faults: Faults, method: Method) {
        dress(w);
        let home = w.game();
        let conf_at_home = std::fs::read_to_string(home.join("launch.conf")).unwrap();
        let entry_at_home =
            std::fs::read_to_string(gogo::linuxapp::game::desktop_path(ID)).unwrap();
        let lib = w.library();
        let there = lib.dir().join(ID);

        // A dry run moves nothing.
        let target = Target::Library(lib.clone());
        let plan = gogo::linuxapp::relocate::plan(
            ID,
            &target,
            &MoveOptions {
                faults,
                ..MoveOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            (plan.from.as_path(), plan.to.as_path()),
            (home.as_path(), there.as_path())
        );
        assert!(home.is_dir() && !there.exists() && !w.locations().exists());

        let out = go(&target, faults).unwrap();
        assert!(
            out.plan.method == method
                || (method == Method::Copy && out.plan.method == Method::Reflink),
            "{:?}",
            out.plan
        );
        assert!(out.recovered.is_empty(), "{out:?}");
        assert!(!home.exists(), "the source goes");
        assert_eq!(
            std::fs::read_to_string(w.locations().join(ID)).unwrap(),
            format!("{}\n", there.display())
        );
        assert_game_at(w, &there);
        assert!(
            !there.join(".gogo-moved").exists()
                && !lib.dir().join(format!(".moving-{ID}")).exists()
        );
        assert_setup_agrees(&there);

        // And back: no record, and every file as a build under games/ wrote it.
        let out = go(&Target::Default, faults).unwrap();
        assert!(out.warnings.is_empty(), "{out:?}");
        assert!(!there.exists() && lib.dir().is_dir());
        assert!(!w.locations().join(ID).exists());
        assert_game_at(w, &home);
        assert_eq!(
            std::fs::read_to_string(home.join("launch.conf")).unwrap(),
            conf_at_home
        );
        assert_eq!(
            std::fs::read_to_string(gogo::linuxapp::game::desktop_path(ID)).unwrap(),
            entry_at_home
        );
        undress(&home);
    }

    #[test]
    fn a_rename_on_one_filesystem_keeps_the_game_both_ways() {
        let w = built("move-rename");
        both_ways(&w, Faults::default(), Method::Rename);
    }

    #[test]
    fn a_copy_across_filesystems_keeps_the_game_both_ways() {
        let w = built("move-copy");
        both_ways(&w, across(), Method::Copy);
    }

    /// Two real filesystems, where the machine has them: `/dev/shm` is a tmpfs
    /// on most Linux systems, and the test dir usually isn't.
    #[test]
    fn a_real_move_to_another_filesystem_copies() {
        let shm = Path::new("/dev/shm").join(format!("gogo-move-{}", std::process::id()));
        if std::fs::create_dir_all(&shm).is_err() {
            eprintln!("SKIP no /dev/shm here");
            return;
        }
        let dev = |p: &Path| p.metadata().unwrap().dev();
        let w = World::new("move-real", &["d3d9.dll"]);
        if dev(&shm) == dev(&w.root) {
            eprintln!("SKIP /dev/shm is on the test dir's filesystem");
            let _ = std::fs::remove_dir(&shm);
            return;
        }
        std::fs::write(w.tree.join(format!("goggame-{ID}.ico")), tiny_ico()).unwrap();
        build_with(&w.opts());
        dress(&w);
        let home = w.game();
        let lib = Library::resolve(&shm, None).unwrap().unwrap();
        let there = lib.dir().join(ID);
        let out = go(&Target::Library(lib), Faults::default());
        let moved = (|| {
            let out = out?;
            assert_eq!(out.plan.method, Method::Copy, "{:?}", out.plan);
            assert!(!home.exists());
            assert_game_at(&w, &there);
            go(&Target::Default, Faults::default())
        })();
        let _ = std::fs::remove_dir_all(&shm);
        moved.unwrap();
        assert!(!there.exists());
        assert_game_at(&w, &home);
        undress(&home);
    }

    #[test]
    fn a_moved_game_launches_from_its_new_place_with_logs_at_the_data_root() {
        let Some(w) = World::launcher("move-launch", false) else {
            return;
        };
        let lib = w.library();
        let there = lib.dir().join(ID);
        go(&Target::Library(lib), across()).unwrap();
        assert_eq!(w.game(), there);

        let out = w.launch(&[], &[]);
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        let log = w.log();
        let prefix = there.join("prefix");
        assert!(
            log.contains(&format!("WINEPREFIX={} ", prefix.display())),
            "{log}"
        );
        assert!(
            log.contains(&format!(
                "ARGV:{} ",
                prefix.join("drive_c/GOG/Test Game/game.exe").display()
            )),
            "{log}"
        );
        assert!(w.home.join("logs").join(ID).join("launch.log").is_file());
        assert!(!there.parent().unwrap().join("logs").exists());
        // Fresh stamp, moved or not: no setup was needed.
        assert!(w.calls().is_empty(), "{:?}", w.calls());

        go(&Target::Default, Faults::default()).unwrap();
        assert_eq!(w.launch(&[], &[]).status.code(), Some(0));
        let log = w.log();
        let env = log.lines().rfind(|l| l.starts_with("ENV:")).unwrap();
        assert!(env.contains(&format!("games/{ID}/prefix")), "{env}");
    }

    /// A kill at any point of a copy leaves the game running from where it was
    /// (nothing refers to the copy yet), and the next move starts clean.
    #[test]
    fn a_killed_copy_leaves_the_game_where_it_was_and_the_next_move_cleans_up() {
        let w = built("move-killed");
        dress(&w);
        let home = w.game();
        let lib = w.library();
        let there = lib.dir().join(ID);
        let staging = lib.dir().join(format!(".moving-{ID}"));
        let target = Target::Library(lib);
        let conf = std::fs::read_to_string(home.join("launch.conf")).unwrap();

        for (kill, left) in [
            (KilledAt::Copying(2), &staging),
            (KilledAt::BeforeRename, &staging),
            (KilledAt::BeforeCommit, &there),
        ] {
            let faults = Faults {
                killed_at: Some(kill),
                ..across()
            };
            let err = go(&target, faults).unwrap_err();
            assert!(err.contains("killed"), "{err}");
            assert!(left.exists(), "{kill:?} leaves {}", left.display());

            // Nothing about the game changed: same dir, same config, runnable.
            assert_eq!(GameDirs::for_game(ID).unwrap().root, home);
            assert!(!w.locations().join(ID).exists());
            assert_eq!(
                std::fs::read_to_string(home.join("launch.conf")).unwrap(),
                conf
            );
            assert_eq!(std::fs::read(home.join(SAVE)).unwrap(), b"level 42");
            assert!(desktop_exec().contains(&home.display().to_string()));

            // Doctor names it.
            let report = gogo::linuxapp::doctor::doctor(ID, &doctor_host(&w, &[])).unwrap();
            assert!(
                report
                    .findings
                    .iter()
                    .any(|f| f.check == "move" && f.level == gogo::macapp::doctor::Level::Warn),
                "{:?}",
                report.findings
            );

            // The next move clears it, even one refused for being home already.
            let refused = go(&Target::Default, Faults::default()).unwrap_err();
            assert!(refused.contains("already there"), "{refused}");
            assert!(!left.exists(), "{kill:?}: {} stays", left.display());
            assert!(!staging.exists() && !there.exists());
            assert!(!w.home.join("game-moves").join(ID).exists());
        }

        // Past the commit the new place is the game; only the cleanup was left.
        let faults = Faults {
            killed_at: Some(KilledAt::BeforeCleanup),
            ..across()
        };
        go(&target, faults).unwrap_err();
        assert_eq!(GameDirs::for_game(ID).unwrap().root, there);
        assert!(home.is_dir(), "the source waits for the cleanup");
        let done =
            gogo::macapp::relocate::recover(ID, &gogo::linuxapp::relocate::LinuxLane::default())
                .unwrap();
        assert_eq!(done.len(), 1, "{done:?}");
        assert!(!home.exists());
        assert_game_at(&w, &there);
        undress(&there);
    }

    #[test]
    fn a_killed_rename_finishes_on_the_next_move() {
        let w = built("move-rename-killed");
        dress(&w);
        let home = w.game();
        let lib = w.library();
        let there = lib.dir().join(ID);
        let faults = Faults {
            killed_at: Some(KilledAt::BeforeCommit),
            ..Faults::default()
        };
        go(&Target::Library(lib.clone()), faults).unwrap_err();
        assert!(!home.exists() && there.is_dir());

        // The data can't go back on its own: the move finishes where it went.
        let done = gogo::linuxapp::relocate::recover_all();
        assert_eq!(done.len(), 1, "{done:?}");
        assert!(done[0].contains("recorded now"), "{done:?}");
        assert_game_at(&w, &there);
        undress(&there);
    }

    /// A full copy, and a source that isn't there: gogo can't tell a dead drive
    /// from one that isn't mounted, and that source may have newer saves.
    #[test]
    fn a_copy_whose_source_is_missing_is_never_deleted_or_believed() {
        let w = World::new("move-no-source", &["d3d9.dll"]);
        let lib = w.library();
        let other = Library::resolve(&w.root.join("second drive"), None)
            .unwrap()
            .unwrap();
        build_with(&w.opts_in_library());
        let from = w.game();
        let to = other.dir().join(ID);
        let target = Target::Library(other);
        let faults = Faults {
            killed_at: Some(KilledAt::BeforeCommit),
            ..across()
        };
        go(&target, faults).unwrap_err();

        let away = lib.dir().join("unmounted");
        std::fs::rename(&from, &away).unwrap();
        let err = go(&target, Faults::default()).unwrap_err();
        // Both paths, the other mount, and the commands out.
        assert!(
            err.contains(&from.display().to_string()) && err.contains(&to.display().to_string()),
            "{err}"
        );
        assert!(err.contains("mounted somewhere else"), "{err}");
        assert!(err.contains("--adopt <that path>"), "{err}");
        assert!(err.contains(&format!("--adopt {}", to.display())), "{err}");
        assert!(to.join("launch.sh").is_file());
        assert_eq!(GameDirs::for_game(ID).unwrap().root, from);

        std::fs::rename(&away, &from).unwrap();
        go(&target, Faults::default()).unwrap();
        assert_eq!(GameDirs::for_game(ID).unwrap().root, to);
        assert!(!from.exists());
    }

    /// Past the commit the source is only deleted once the game is proved to be
    /// at the target and nothing runs from the source.
    #[test]
    fn a_source_past_the_commit_waits_for_its_target_and_its_processes() {
        let w = built("move-keep-source");
        let home = w.game();
        let lib = w.library();
        let there = lib.dir().join(ID);
        let faults = Faults {
            killed_at: Some(KilledAt::BeforeCleanup),
            ..across()
        };
        go(&Target::Library(lib), faults).unwrap_err();
        assert_eq!(GameDirs::for_game(ID).unwrap().root, there);

        // The target's drive is gone, or a power cut undid its data.
        let away = w.root.join("not mounted");
        std::fs::rename(&there, &away).unwrap();
        let lines = gogo::linuxapp::relocate::recover_all();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("isn't there"), "{lines:?}");
        assert!(home.join("launch.sh").is_file(), "the source stays");
        assert!(w.home.join("game-moves").join(ID).exists());
        std::fs::rename(&away, &there).unwrap();

        // Something still runs from the old prefix: maybe a game writing saves.
        let mut game = std::process::Command::new("sleep")
            .arg("60")
            .env("WINEPREFIX", home.join("prefix"))
            .spawn()
            .unwrap();
        assert!(wait_until(10, || !gogo::linuxapp::game::prefix_procs(
            &home.join("prefix")
        )
        .is_empty()));
        let lines = gogo::linuxapp::relocate::recover_all();
        game.kill().unwrap();
        game.wait().unwrap();
        assert!(lines.iter().any(|l| l.contains("in use")), "{lines:?}");
        assert!(home.join("launch.sh").is_file());
        assert!(w.home.join("game-moves").join(ID).exists());

        assert_eq!(gogo::linuxapp::relocate::recover_all().len(), 1);
        assert!(!home.exists());
        assert!(!w.home.join("game-moves").join(ID).exists());
    }

    #[test]
    fn a_copy_that_fails_or_differs_goes_and_the_game_stays() {
        let w = World::new("move-fail", &["d3d9.dll"]);
        build_with(&w.opts());
        let home = w.game();
        let lib = w.library();
        let target = Target::Library(lib.clone());
        for (faults, says) in [
            (
                Faults {
                    disk_full_after: Some(1),
                    ..across()
                },
                "the copy failed",
            ),
            (
                Faults {
                    corrupt_copy: true,
                    ..across()
                },
                "doesn't match",
            ),
        ] {
            let err = go(&target, faults).unwrap_err();
            assert!(err.contains(says), "{err}");
            assert!(err.contains(&home.display().to_string()), "{err}");
            assert_eq!(GameDirs::for_game(ID).unwrap().root, home);
            assert!(home.join("launch.sh").is_file());
            assert!(!lib.dir().join(ID).exists());
            assert!(!lib.dir().join(format!(".moving-{ID}")).exists());
            assert!(!w.home.join("game-moves").join(ID).exists());
        }

        // Too big for the target is refused before anything's written.
        // (Not where the two share a filesystem that reflinks: that costs nothing.)
        let faults = Faults {
            free_bytes: Some(1),
            ..across()
        };
        match gogo::linuxapp::relocate::plan(
            ID,
            &target,
            &MoveOptions {
                faults,
                ..MoveOptions::default()
            },
        ) {
            Err(e) => assert!(e.to_string().contains("is free"), "{e}"),
            Ok(plan) => assert_eq!(plan.method, Method::Reflink),
        }
    }

    #[test]
    fn a_game_launched_during_the_copy_stops_the_move() {
        let w = World::new("move-launched", &["d3d9.dll"]);
        build_with(&w.opts());
        let home = w.game();
        let lib = w.library();
        let mut game = None;
        let err = gogo::linuxapp::relocate::move_game(
            ID,
            &Target::Library(lib.clone()),
            &MoveOptions {
                faults: across(),
                ..MoveOptions::default()
            },
            &mut |ev| {
                if game.is_none() && matches!(ev, MoveEvent::Copying { .. }) {
                    game = Some(
                        std::process::Command::new("sleep")
                            .arg("60")
                            .env("WINEPREFIX", home.join("prefix"))
                            .spawn()
                            .unwrap(),
                    );
                    // /proc shows its environ once exec is done.
                    assert!(wait_until(10, || !gogo::linuxapp::game::prefix_procs(
                        &home.join("prefix")
                    )
                    .is_empty()));
                }
            },
        )
        .unwrap_err();
        let mut game = game.unwrap();
        game.kill().unwrap();
        game.wait().unwrap();
        assert!(err.to_string().contains("still running"), "{err}");
        assert_eq!(GameDirs::for_game(ID).unwrap().root, home);
        assert!(home.join("launch.sh").is_file());
        assert!(!lib.dir().join(format!(".moving-{ID}")).exists());
        assert!(!lib.dir().join(ID).exists());
    }

    /// What a launch refused for a move leaves: no game, a 127 run, a dialog,
    /// and no launcher record for `live_launcher` to find.
    fn assert_refused_for_a_move(w: &World, out: &std::process::Output) {
        assert_eq!(out.status.code(), Some(127), "{out:?}");
        assert_eq!(w.game_runs(), 0);
        assert!(
            w.launch_log()
                .contains(&format!("a move of {ID} is in progress")),
            "{}",
            w.launch_log()
        );
        assert!(w.dialogs()[0].contains("being moved"), "{:?}", w.dialogs());
        assert!(
            w.runs()
                .last()
                .unwrap()
                .contains("# exit: 127 (game is being moved)")
        );
        assert_eq!(gogo::linuxapp::game::live_launcher(&w.game()), None);
    }

    fn hold(w: &World, name: &str) -> std::fs::File {
        let dir = w.home.join("game-moves");
        std::fs::create_dir_all(&dir).unwrap();
        let f = std::fs::File::create(dir.join(name)).unwrap();
        f.lock().unwrap();
        f
    }

    /// A launch while a move runs is refused, from the first copied file on;
    /// the same launch goes ahead once the move is over.
    #[test]
    fn a_launch_during_a_move_is_refused_and_one_after_it_runs() {
        let Some(w) = World::launcher("move-refuses-launch", false) else {
            return;
        };
        let stub = w.dialog_stub(&["OK"]);
        let env = [("GOGO_LAUNCHER_DIALOG", stub.to_str().unwrap())];
        let lib = w.library();
        let mut tried = 0;
        go_with(&w, &Target::Library(lib), across(), &mut |ev| {
            if matches!(ev, MoveEvent::Verifying { .. }) && tried == 0 {
                tried += 1;
                let out = w.launch(&env, &[]);
                assert_refused_for_a_move(&w, &out);
            }
        })
        .unwrap();
        assert_eq!(tried, 1);
        // The move is over: the game runs from its new place.
        assert_eq!(w.launch(&[], &[]).status.code(), Some(0));
        assert_eq!(w.game_runs(), 1);
    }

    /// The lock alone decides: held, a launch is refused at any point of the
    /// move; a journal with nothing holding the lock (a dead move) blocks
    /// nothing.
    #[test]
    fn only_a_held_lock_refuses_and_a_dead_moves_journal_does_not() {
        let Some(w) = World::launcher("move-dead", false) else {
            return;
        };
        let stub = w.dialog_stub(&["OK"]);
        let env = [("GOGO_LAUNCHER_DIALOG", stub.to_str().unwrap())];
        {
            let _held = hold(&w, &format!("{ID}.lock"));
            let out = w.launch(&env, &[]);
            assert_refused_for_a_move(&w, &out);
        }
        // A move killed just before its commit: its journal stays, its lock is gone.
        let lib = w.library();
        let err = go(
            &Target::Library(lib),
            Faults {
                killed_at: Some(KilledAt::BeforeCommit),
                ..across()
            },
        )
        .unwrap_err();
        assert!(err.contains("killed"), "{err}");
        assert!(w.home.join("game-moves").join(ID).is_file());
        assert!(
            w.home
                .join("game-moves")
                .join(format!("{ID}.lock"))
                .is_file()
        );
        let out = w.launch(&env, &[]);
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        assert_eq!(w.game_runs(), 1);
    }

    /// Another game's move, or the data root's own lock, doesn't stop this
    /// game; and a running game takes no lock a move needs.
    #[test]
    fn a_launch_is_stopped_by_its_own_games_lock_only() {
        let Some(w) = World::launcher("move-other", true) else {
            return;
        };
        let _other = hold(&w, "999.lock");
        let _root = hold(&w, ".lock");
        let mut child = w
            .launch_cmd(&[("FAKE_GAME_HANG", "1")], &[])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        assert!(wait_until(10, || !prefix_procs(&w).is_empty()));
        // Running: a move of another game gets past the locks (to "not built").
        // A move of a game whose lock something holds refuses at once.
        drop(_root);
        let t0 = std::time::Instant::now();
        let err = gogo::linuxapp::relocate::move_game(
            "999",
            &Target::Default,
            &MoveOptions::default(),
            &mut |_| {},
        )
        .unwrap_err()
        .to_string();
        assert!(t0.elapsed().as_secs() < 5, "the move waited");
        assert!(err.contains("999 is being launched or moved"), "{err}");
        drop(_other);
        let err = gogo::linuxapp::relocate::move_game(
            "999",
            &Target::Default,
            &MoveOptions::default(),
            &mut |_| {},
        )
        .unwrap_err()
        .to_string();
        assert!(!err.contains("another `gogo`"), "{err}");
        // Moving this very game is refused by the running check, not by a lock.
        let err = go(&Target::Library(w.library()), Faults::default()).unwrap_err();
        assert!(err.contains("is running"), "{err}");
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status();
        child.wait().unwrap();
    }

    #[test]
    fn what_a_move_refuses() {
        let w = World::new("move-refuse", &["d3d9.dll"]);
        let lib = w.library();
        let target = Target::Library(lib.clone());
        let there = lib.dir().join(ID);

        // Not built.
        let err = go(&target, Faults::default()).unwrap_err();
        assert!(err.contains("isn't built here"), "{err}");
        assert!(gogo::linuxapp::relocate::plan("nope", &target, &MoveOptions::default()).is_err());

        build_with(&w.opts());
        let home = w.game();

        // Its launcher is running; a process is in its prefix.
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
        let start = stat
            .rsplit_once(") ")
            .unwrap()
            .1
            .split_whitespace()
            .nth(19)
            .unwrap();
        let live = home.join(".launchers");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(live.join(std::process::id().to_string()), start).unwrap();
        let err = go(&target, Faults::default()).unwrap_err();
        assert!(err.contains("is running"), "{err}");
        std::fs::remove_dir_all(&live).unwrap();

        let mut game = std::process::Command::new("sleep")
            .arg("60")
            .env("WINEPREFIX", home.join("prefix"))
            .spawn()
            .unwrap();
        assert!(wait_until(10, || !gogo::linuxapp::game::prefix_procs(
            &home.join("prefix")
        )
        .is_empty()));
        let err = go(&target, Faults::default()).unwrap_err();
        game.kill().unwrap();
        game.wait().unwrap();
        assert!(err.contains("still running"), "{err}");
        assert!(home.is_dir() && !there.exists());
        assert!(!w.home.join("game-moves").join(ID).exists());

        // Where it already is, by any name.
        let err = go(&Target::Default, Faults::default()).unwrap_err();
        assert!(err.contains("already there"), "{err}");

        // A target that holds this game, or anything by its name.
        std::fs::create_dir_all(&there).unwrap();
        std::fs::write(there.join("mine"), "keep").unwrap();
        let err = go(&target, Faults::default()).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(std::fs::read_to_string(there.join("mine")).unwrap(), "keep");
        std::fs::remove_dir_all(&there).unwrap();

        // Under umu's private /tmp.
        let tmp = Library::resolve(Path::new("/tmp/gogo-move-test"), None)
            .unwrap()
            .unwrap();
        let err = go(&Target::Library(tmp), Faults::default()).unwrap_err();
        assert!(err.contains("/tmp"), "{err}");

        // Another move is running.
        let lock = w.home.join("game-moves/.lock");
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        let held = std::fs::File::create(&lock).unwrap();
        held.lock().unwrap();
        let err = go(&target, Faults::default()).unwrap_err();
        assert!(err.contains("another"), "{err}");
        held.unlock().unwrap();

        // A symlinked game dir: the data is somewhere gogo doesn't know about.
        let real = w.root.join("elsewhere");
        std::fs::rename(&home, &real).unwrap();
        std::os::unix::fs::symlink(&real, &home).unwrap();
        let err = go(&target, Faults::default()).unwrap_err();
        assert!(err.contains("symlink"), "{err}");
        std::fs::remove_file(&home).unwrap();
        std::fs::rename(&real, &home).unwrap();

        // Inside its own dir.
        go(&target, Faults::default()).unwrap();
        let inside = Library::resolve(&there.join("sub"), None).unwrap().unwrap();
        let err = go(&Target::Library(inside), Faults::default()).unwrap_err();
        assert!(err.contains("inside"), "{err}");

        // A record whose drive isn't mounted.
        let away = w.root.join("unmounted");
        std::fs::rename(&there, &away).unwrap();
        let err = go(&Target::Default, Faults::default()).unwrap_err();
        assert!(err.contains("a drive not mounted"), "{err}");
        assert_eq!(GameDirs::for_game(ID).unwrap().root, there);
    }

    /// A game's own record of where it was, the one thing a move can't rewrite
    /// for it: the move says so. An args line naming the old dir, likewise.
    #[test]
    fn a_game_that_recorded_its_path_gets_a_warning() {
        let w = built("move-mentions");
        let home = w.game();
        let reg = home.join("prefix/user.reg");
        let dos = home.display().to_string().replace('/', "\\\\");
        let mut text = std::fs::read_to_string(&reg).unwrap_or_default();
        text.push_str(&format!("\"Install\"=\"Z:{dos}\\\\prefix\"\n"));
        std::fs::write(&reg, text).unwrap();
        w.edit_conf(&[("ARGS", &format!("('{}/x')", home.display()))]);

        let out = go(&Target::Library(w.library()), Faults::default()).unwrap();
        assert_eq!(out.warnings.len(), 2, "{:?}", out.warnings);
        assert!(
            out.warnings[0].contains("launch.conf"),
            "{:?}",
            out.warnings
        );
        assert!(out.warnings[1].contains("user.reg"), "{:?}", out.warnings);
    }

    #[test]
    fn apps_remove_after_an_interrupted_move_finds_the_game() {
        let w = World::new("move-remove", &["d3d9.dll"]);
        build_with(&w.opts());
        let lib = w.library();
        let faults = Faults {
            killed_at: Some(KilledAt::BeforeCommit),
            ..across()
        };
        go(&Target::Library(lib.clone()), faults).unwrap_err();
        assert_eq!(gogo::linuxapp::relocate::recover_all().len(), 1);
        assert!(!lib.dir().join(ID).exists());
        assert!(!w.home.join("game-moves").join(ID).exists());
        let logs = w.root.join("logs");
        let a = gogo::macapp::apps::get(ID, &logs).unwrap().unwrap();
        gogo::macapp::apps::remove(&a, false).unwrap();
        assert!(gogo::macapp::apps::list(&logs).unwrap().is_empty());
    }

    // — adopting a game that moved without gogo (#283) —

    fn adopt(target: &Target) -> Result<gogo::macapp::relocate::AdoptOutcome, String> {
        gogo::linuxapp::relocate::adopt_game(ID, target).map_err(|e| e.to_string())
    }

    /// Another library, on "another drive".
    fn other_library(w: &World) -> Library {
        Library::resolve(&w.root.join("other drive/lib"), None)
            .unwrap()
            .unwrap()
    }

    /// The game's dir moved by hand: `from` is gone, `to` holds it.
    fn move_by_hand(from: &Path, to: &Path) {
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::rename(from, to).unwrap();
    }

    #[test]
    fn a_game_moved_by_hand_between_libraries_is_adopted_where_it_is() {
        let w = built("adopt-lib");
        dress(&w);
        let home = w.game();
        let (a, b) = (w.library(), other_library(&w));
        go(&Target::Library(a.clone()), Faults::default()).unwrap();
        let (old, new) = (a.dir().join(ID), b.dir().join(ID));
        move_by_hand(&old, &new);
        // Nothing knows yet: the record is stale.
        assert_eq!(GameDirs::for_game(ID).unwrap().root, old);

        let target = Target::Library(b);
        let plan = gogo::linuxapp::relocate::adopt_plan(ID, &target).unwrap();
        assert_eq!(
            (plan.from.as_path(), plan.to.as_path()),
            (old.as_path(), new.as_path())
        );
        assert_eq!(
            GameDirs::for_game(ID).unwrap().root,
            old,
            "a plan writes nothing"
        );

        let out = adopt(&target).unwrap();
        assert!(
            out.recovered.is_empty() && out.warnings.is_empty(),
            "{out:?}"
        );
        assert_eq!(
            std::fs::read_to_string(w.locations().join(ID)).unwrap(),
            format!("{}\n", new.display())
        );
        assert_game_at(&w, &new);
        assert_setup_agrees(&new);
        assert!(!w.home.join("game-moves").join(ID).exists());
        // The data wasn't touched.
        assert!(!old.exists() && !home.exists());
        undress(&new);
    }

    #[test]
    fn a_game_put_back_in_games_by_hand_is_adopted_and_loses_its_record() {
        let w = built("adopt-default");
        dress(&w);
        let home = w.game();
        let conf_at_home = std::fs::read_to_string(home.join("launch.conf")).unwrap();
        let entry_at_home =
            std::fs::read_to_string(gogo::linuxapp::game::desktop_path(ID)).unwrap();
        let lib = w.library();
        go(&Target::Library(lib.clone()), Faults::default()).unwrap();
        move_by_hand(&lib.dir().join(ID), &home);

        adopt(&Target::Default).unwrap();
        assert!(!w.locations().join(ID).exists());
        assert_game_at(&w, &home);
        assert_eq!(
            std::fs::read_to_string(home.join("launch.conf")).unwrap(),
            conf_at_home
        );
        assert_eq!(
            std::fs::read_to_string(gogo::linuxapp::game::desktop_path(ID)).unwrap(),
            entry_at_home
        );
        undress(&home);
    }

    /// A game moved out of `games/` by hand, with no record yet.
    #[test]
    fn a_game_taken_out_of_games_by_hand_is_adopted_into_a_library() {
        let w = built("adopt-out");
        dress(&w);
        let home = w.game();
        let lib = w.library();
        let new = lib.dir().join(ID);
        move_by_hand(&home, &new);
        adopt(&Target::Library(lib)).unwrap();
        assert_game_at(&w, &new);
        assert_setup_agrees(&new);
        undress(&new);
    }

    #[test]
    fn an_adopted_game_launches_from_where_it_is() {
        let Some(w) = World::launcher("adopt-launch", false) else {
            return;
        };
        let (a, b) = (w.library(), other_library(&w));
        go(&Target::Library(a.clone()), across()).unwrap();
        let new = b.dir().join(ID);
        move_by_hand(&a.dir().join(ID), &new);
        adopt(&Target::Library(b)).unwrap();
        assert!(desktop_exec().contains(&new.join("launch.sh").display().to_string()));

        let out = w.launch(&[], &[]);
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        let log = w.log();
        assert!(
            log.contains(&format!("WINEPREFIX={} ", new.join("prefix").display())),
            "{log}"
        );
        assert!(w.calls().is_empty(), "no setup: {:?}", w.calls());
    }

    /// Every refusal leaves the record, `launch.conf` and the menu entry as
    /// they were; the last adopt, with nothing in the way, works.
    #[test]
    fn what_an_adopt_refuses() {
        let w = built("adopt-refuse");
        let (a, b) = (w.library(), other_library(&w));
        go(&Target::Library(a.clone()), Faults::default()).unwrap();
        let (old, new) = (a.dir().join(ID), b.dir().join(ID));
        move_by_hand(&old, &new);
        let target = Target::Library(b.clone());
        let record = std::fs::read_to_string(w.locations().join(ID)).unwrap();
        let conf = std::fs::read_to_string(new.join("launch.conf")).unwrap();
        let entry = std::fs::read_to_string(gogo::linuxapp::game::desktop_path(ID)).unwrap();
        let unchanged = || {
            assert_eq!(
                std::fs::read_to_string(w.locations().join(ID)).unwrap(),
                record
            );
            assert_eq!(
                std::fs::read_to_string(new.join("launch.conf")).unwrap(),
                conf
            );
            assert_eq!(
                std::fs::read_to_string(gogo::linuxapp::game::desktop_path(ID)).unwrap(),
                entry
            );
        };

        // Nowhere to adopt, or not a dir.
        let nowhere = Target::Library(other_library_at(&w, "nowhere"));
        let err = adopt(&nowhere).unwrap_err();
        assert!(err.contains("no dir there"), "{err}");

        // Not this game: the id inside says another.
        let toml = new.join("gogo-launch.toml");
        let text = std::fs::read_to_string(&toml).unwrap();
        std::fs::write(
            &toml,
            text.replace(&format!("gogid = \"{ID}\""), "gogid = \"999\""),
        )
        .unwrap();
        let err = adopt(&target).unwrap_err();
        assert!(err.contains("game 999, not"), "{err}");
        // A game file that can't be read.
        std::fs::write(&toml, "not toml {{{").unwrap();
        let err = adopt(&target).unwrap_err();
        assert!(err.contains("can't tell whose game"), "{err}");
        // A copy an interrupted move left.
        std::fs::write(&toml, &text).unwrap();
        std::fs::write(new.join(".gogo-moved"), "").unwrap();
        let err = adopt(&target).unwrap_err();
        assert!(err.contains(".gogo-moved"), "{err}");
        std::fs::remove_file(new.join(".gogo-moved")).unwrap();
        // A launch.conf an earlier, half-done adopt left naming a third place.
        let conf_text = std::fs::read_to_string(new.join("launch.conf")).unwrap();
        std::fs::write(
            new.join("launch.conf"),
            conf_text.replace("PREFIX='", "PREFIX='/elsewhere/"),
        )
        .unwrap();
        let err = adopt(&target).unwrap_err();
        assert!(err.contains("stopped halfway"), "{err}");
        assert_eq!(
            std::fs::read_to_string(w.locations().join(ID)).unwrap(),
            record
        );
        std::fs::write(new.join("launch.conf"), conf_text).unwrap();
        // Not a built game at all.
        std::fs::remove_file(&toml).unwrap();
        let err = adopt(&target).unwrap_err();
        assert!(err.contains("gogo-launch.toml"), "{err}");
        std::fs::write(&toml, text).unwrap();
        unchanged();

        // Its launcher is running.
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
        let start = stat
            .rsplit_once(") ")
            .unwrap()
            .1
            .split_whitespace()
            .nth(19)
            .unwrap();
        let live = new.join(".launchers");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(live.join(std::process::id().to_string()), start).unwrap();
        let err = adopt(&target).unwrap_err();
        assert!(err.contains("is running"), "{err}");
        std::fs::remove_dir_all(&live).unwrap();
        unchanged();

        // A lock held: another move, or this game's own.
        for (name, says) in [
            (".lock", "another"),
            (&format!("{ID}.lock"), "being launched or moved"),
        ] {
            let held = hold(&w, name);
            let err = adopt(&target).unwrap_err();
            assert!(err.contains(says), "{err}");
            drop(held);
        }
        unchanged();

        // The old place is still there: two copies, and gogo picks neither.
        std::fs::create_dir_all(&old).unwrap();
        let err = adopt(&target).unwrap_err();
        assert!(
            err.contains(&old.display().to_string()) && err.contains(&new.display().to_string()),
            "{err}"
        );
        assert!(err.contains("won't guess"), "{err}");
        unchanged();
        std::fs::remove_dir(&old).unwrap();

        // The old library holds nothing now.
        let err = adopt(&Target::Library(a)).unwrap_err();
        assert!(err.contains("no dir there"), "{err}");

        adopt(&target).unwrap();
        assert_eq!(GameDirs::for_game(ID).unwrap().root, new);
        let err = adopt(&target).unwrap_err();
        assert!(err.contains("nothing to adopt"), "{err}");
    }

    fn other_library_at(w: &World, name: &str) -> Library {
        Library::resolve(&w.root.join(name), None).unwrap().unwrap()
    }

    /// A move that stopped before its commit is settled first, and the adopt
    /// goes ahead; one that can't be settled refuses it.
    #[test]
    fn an_interrupted_move_is_settled_before_an_adopt_or_refuses_it() {
        let w = built("adopt-journal");
        let (a, b) = (w.library(), other_library(&w));
        go(&Target::Library(a.clone()), Faults::default()).unwrap();
        let (old, new) = (a.dir().join(ID), b.dir().join(ID));
        let staging = b.dir().join(format!(".moving-{ID}"));

        // A copy that never finished, then the dir moved by hand.
        let faults = Faults {
            killed_at: Some(KilledAt::Copying(2)),
            ..across()
        };
        go(&Target::Library(b.clone()), faults).unwrap_err();
        assert!(staging.exists());
        let hand = w.root.join("by hand").join(ID);
        move_by_hand(&old, &hand);
        let by_hand = Target::Library(other_library_at(&w, "by hand"));
        let out = adopt(&by_hand).unwrap();
        assert_eq!(out.recovered.len(), 1, "{out:?}");
        assert!(!staging.exists());
        assert!(!w.home.join("game-moves").join(ID).exists());
        assert_eq!(GameDirs::for_game(ID).unwrap().root, hand);

        // A full copy, a source that is gone: that isn't gogo's to settle.
        move_by_hand(&hand, &old);
        let faults = Faults {
            killed_at: Some(KilledAt::BeforeCommit),
            ..across()
        };
        // The record still names `hand`, so put it back for this move.
        std::fs::write(w.locations().join(ID), format!("{}\n", old.display())).unwrap();
        go(&Target::Library(b.clone()), faults).unwrap_err();
        assert!(new.join(".gogo-moved").exists());
        move_by_hand(&old, &hand);
        // The record still names `old`; the journal blocks a plain move, and
        // adopting the source where it is settles it: the marked copy goes.
        let out = adopt(&by_hand).unwrap();
        assert!(
            out.recovered.iter().any(|l| l.contains("removed")),
            "{out:?}"
        );
        assert!(!new.exists(), "the spare copy is removed");
        assert!(hand.join("launch.sh").is_file(), "the named game stays");
        assert!(!w.home.join("game-moves").join(ID).exists());
        assert_eq!(GameDirs::for_game(ID).unwrap().root, hand);
    }

    /// A full copy, a source nobody names: `--adopt` at the copy says "the copy is
    /// mine"; nothing is deleted, and the journal and marker go.
    #[test]
    fn adopting_the_copy_of_an_unfinished_move_keeps_it() {
        let w = built("adopt-copy");
        let (a, b) = (w.library(), other_library(&w));
        go(&Target::Library(a.clone()), Faults::default()).unwrap();
        let (old, new) = (a.dir().join(ID), b.dir().join(ID));
        let faults = Faults {
            killed_at: Some(KilledAt::BeforeCommit),
            ..across()
        };
        go(&Target::Library(b.clone()), faults).unwrap_err();
        assert!(new.join(".gogo-moved").exists());
        let away = w.root.join("unmounted");
        std::fs::rename(&old, &away).unwrap();

        // A dir that isn't the copy is still refused, and nothing changes.
        let err = adopt(&Target::Library(other_library_at(&w, "nowhere"))).unwrap_err();
        assert!(err.contains("no dir there"), "{err}");
        assert!(new.exists() && away.exists());

        let out = adopt(&Target::Library(b)).unwrap();
        assert!(out.recovered[0].contains("adopted the copy"), "{out:?}");
        assert_eq!((out.plan.from, out.plan.to), (old, new.clone()));
        assert!(!new.join(".gogo-moved").exists());
        assert!(new.join("launch.sh").is_file());
        assert!(away.exists(), "the source was never touched");
        assert!(!w.home.join("game-moves").join(ID).exists());
        assert_eq!(GameDirs::for_game(ID).unwrap().root, new);
    }

    /// Recovery that did something, then a refusal: the lines aren't lost.
    #[test]
    fn a_refusal_after_a_recovery_still_says_what_the_recovery_did() {
        let w = built("recovered-lines");
        let lib = w.library();
        let faults = Faults {
            killed_at: Some(KilledAt::BeforeCommit),
            ..Faults::default()
        };
        go(&Target::Library(lib.clone()), faults).unwrap_err();
        // The adopt's recovery records the rename, then the adopt itself is
        // refused (nothing at the named dir).
        let err = adopt(&Target::Library(other_library_at(&w, "nowhere"))).unwrap_err();
        assert!(err.contains("no dir there"), "{err}");
        assert!(err.contains("recorded now"), "{err}");
    }

    #[test]
    fn a_move_refused_after_a_recovery_still_says_what_the_recovery_did() {
        let w = built("recovered-lines-move");
        let lib = w.library();
        let faults = Faults {
            killed_at: Some(KilledAt::BeforeCommit),
            ..Faults::default()
        };
        go(&Target::Library(lib.clone()), faults).unwrap_err();
        let err = go(&Target::Library(lib), Faults::default()).unwrap_err();
        assert!(err.contains("already there"), "{err}");
        assert!(err.contains("recorded now"), "{err}");
    }

    use gogo::macapp::launch::GameDirs;
}

// — display mode (#292, design 18 §7.9) —

/// A listening `wayland-7` in a dir of its own under `/tmp`: a socket's path
/// has to fit `sun_path` (108 bytes), and the test dirs are longer.
fn wayland_socket() -> (tempfile::TempDir, std::os::unix::net::UnixListener) {
    let dir = tempfile::Builder::new()
        .prefix("gogo-wl")
        .tempdir_in("/tmp")
        .unwrap();
    let l = std::os::unix::net::UnixListener::bind(dir.path().join("wayland-7")).unwrap();
    (dir, l)
}

/// `display` and `hdr` reach `launch.conf`, and the launcher checks for a
/// Wayland socket where the game runs: with one the game gets
/// `PROTON_ENABLE_WAYLAND=1` and the HDR switch, without one it runs on X11
/// and the launcher says why. `x11` strips a session's own switch, the
/// player's L4 env wins over the mode, and a cleared mode leaves the
/// environment alone. Doctor shows it all, the last run included.
#[test]
fn the_display_mode_reaches_the_game_with_a_socket_and_falls_back_without_one() {
    use gogo::linuxapp::display::set_display;
    use gogo::linuxapp::game::ConfValues;
    use gogo::macapp::compat::{DisplayMode, Hdr};
    use gogo::macapp::doctor::Level;
    let Some(w) = World::launcher("display", false) else {
        return;
    };
    let setup_again = || setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    let out = set_display(
        ID,
        Some(Some(DisplayMode::Wayland)),
        Some(Some(Hdr::Force)),
        Some(Some("DP-1".into())),
    )
    .unwrap();
    let keys: Vec<&str> = out.changes.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(keys, ["display", "hdr", "monitor"]);
    // A name that isn't an output's is refused, and nothing is written.
    assert!(set_display(ID, None, None, Some(Some("DP 1".into()))).is_err());
    setup_again();
    let conf = ConfValues::parse(&w.conf());
    assert_eq!(conf.get("DISPLAY_MODE"), "wayland");
    assert_eq!(conf.get("DISPLAY_HDR"), "force");
    assert_eq!(conf.get("DISPLAY_MONITOR"), "DP-1");

    let (run_dir, _socket) = wayland_socket();
    let rd = run_dir.path().to_str().unwrap();
    let dump = w.root.join("game.env");
    let dump_s = dump.to_str().unwrap();
    let game_env = || std::fs::read_to_string(&dump).unwrap();
    let has = |env: &str, kv: &str| env.lines().any(|l| l == kv);
    let sets = |env: &str, k: &str| env.lines().any(|l| l.starts_with(&format!("{k}=")));

    // A socket: Wine's Wayland driver, and the HDR switch. A session's
    // alias set to 0 doesn't undo it (Proton reads the alias last).
    let out = w.launch(
        &[
            ("XDG_RUNTIME_DIR", rd),
            ("WAYLAND_DISPLAY", "wayland-7"),
            ("PROTON_USE_WAYLAND", "0"),
            ("FAKE_ENV_DUMP", dump_s),
        ],
        &[],
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let env = game_env();
    assert!(has(&env, "PROTON_ENABLE_WAYLAND=1"), "{env}");
    assert!(!sets(&env, "PROTON_USE_WAYLAND"), "{env}");
    assert!(has(&env, "PROTON_FORCE_HDR=1"), "{env}");
    assert!(has(&env, "PROTON_WAYLAND_MONITOR=DP-1"), "{env}");
    let run = w.runs().pop().unwrap();
    assert!(
        run.contains(
            "# display: wayland (WAYLAND_DISPLAY=wayland-7)\n# hdr: force\n# monitor: DP-1\n"
        ),
        "{run}"
    );
    assert!(
        w.launch_log()
            .contains("display: wayland (WAYLAND_DISPLAY=wayland-7)\n"),
        "{}",
        w.launch_log()
    );

    // Doctor, in a session with that socket (under its root) and without.
    let mut host = doctor_host(&w, &[("radeon.x86_64", 2)]);
    // Under the host's root, as every path doctor reads.
    std::fs::create_dir_all(host.root.join("run/user")).unwrap();
    std::os::unix::fs::symlink(run_dir.path(), host.root.join("run/user/1000")).unwrap();
    host.logs = w.home.join("logs");
    host.env = Box::new(|k| match k {
        "WAYLAND_DISPLAY" => Some("wayland-7".into()),
        "XDG_RUNTIME_DIR" => Some("/run/user/1000".into()),
        _ => std::env::var_os(k),
    });
    let r = doctor(&w, &host);
    let f = r.findings.iter().find(|f| f.check == "display").unwrap();
    // The fixture is a 32-bit game on dxvk, which works on Wayland (#326): no
    // warning, only the display line.
    assert_eq!(f.level, Level::Ok, "{f:#?}");
    assert_eq!(
        f.message,
        "wayland (L4 user override), hdr force (L4 user override), monitor DP-1 \
         (L4 user override); here: wayland (WAYLAND_DISPLAY=wayland-7, socket found)",
        "{f:#?}"
    );
    assert_eq!(
        f.detail,
        [
            "last run: display wayland (WAYLAND_DISPLAY=wayland-7)",
            "last run: hdr force",
            "last run: monitor DP-1"
        ]
    );
    host.env = Box::new(|k| {
        (k != "WAYLAND_DISPLAY")
            .then(|| std::env::var_os(k))
            .flatten()
    });
    let r = doctor(&w, &host);
    let f = r.findings.iter().find(|f| f.check == "display").unwrap();
    assert_eq!(f.level, Level::Warn, "{f:#?}");
    assert!(
        f.message
            .contains("WAYLAND_DISPLAY is unset: a launch from this shell runs on X11"),
        "{}",
        f.message
    );

    // No WAYLAND_DISPLAY: X11, logged; a session's own switch doesn't
    // reach the game either, and the HDR switch isn't set.
    let out = w.launch(
        &[
            ("XDG_RUNTIME_DIR", rd),
            ("PROTON_ENABLE_WAYLAND", "1"),
            ("FAKE_ENV_DUMP", dump_s),
        ],
        &[],
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let env = game_env();
    assert!(!sets(&env, "PROTON_ENABLE_WAYLAND"), "{env}");
    assert!(!sets(&env, "PROTON_FORCE_HDR"), "{env}");
    assert!(!sets(&env, "PROTON_WAYLAND_MONITOR"), "{env}");
    let run = w.runs().pop().unwrap();
    assert!(
        run.contains(
            "# display: x11 (wayland wanted, but WAYLAND_DISPLAY is unset)\n\
             # hdr: force (not applied: not on Wayland)\n\
             # monitor: DP-1 (not applied: not on Wayland)\n"
        ),
        "{run}"
    );
    assert!(
        w.launch_log().contains(
            "display: wayland wanted, but WAYLAND_DISPLAY is unset: the game runs on X11 (Xwayland)"
        ),
        "{}",
        w.launch_log()
    );
    // A WAYLAND_DISPLAY with no socket behind it: the same.
    w.launch(
        &[
            ("XDG_RUNTIME_DIR", rd),
            ("WAYLAND_DISPLAY", "wayland-9"),
            ("FAKE_ENV_DUMP", dump_s),
        ],
        &[],
    );
    assert!(!sets(&game_env(), "PROTON_ENABLE_WAYLAND"));
    assert!(w.runs().pop().unwrap().contains(
        "# display: x11 (wayland wanted, but no Wayland socket at WAYLAND_DISPLAY=wayland-9)\n"
    ));

    // hdr off: a session's PROTON_FORCE_HDR=1 (checked before hdr) and its
    // alias are taken away, so the game really gets no HDR.
    set_display(ID, None, Some(Some(Hdr::Off)), None).unwrap();
    setup_again();
    w.launch(
        &[
            ("XDG_RUNTIME_DIR", rd),
            ("WAYLAND_DISPLAY", "wayland-7"),
            ("PROTON_FORCE_HDR", "1"),
            ("PROTON_USE_HDR", "1"),
            ("FAKE_ENV_DUMP", dump_s),
        ],
        &[],
    );
    let env = game_env();
    assert!(has(&env, "PROTON_ENABLE_HDR=0"), "{env}");
    assert!(!sets(&env, "PROTON_FORCE_HDR"), "{env}");
    assert!(!sets(&env, "PROTON_USE_HDR"), "{env}");
    assert!(w.runs().pop().unwrap().contains("# hdr: off\n"));
    // hdr on: the same, with the switch on.
    set_display(ID, None, Some(Some(Hdr::On)), None).unwrap();
    setup_again();
    w.launch(
        &[
            ("XDG_RUNTIME_DIR", rd),
            ("WAYLAND_DISPLAY", "wayland-7"),
            ("PROTON_FORCE_HDR", "1"),
            ("FAKE_ENV_DUMP", dump_s),
        ],
        &[],
    );
    let env = game_env();
    assert!(has(&env, "PROTON_ENABLE_HDR=1"), "{env}");
    assert!(!sets(&env, "PROTON_FORCE_HDR"), "{env}");
    // The L4 env still wins over the HDR switch, as over the mode.
    gogo::linuxapp::envargs::set_env(ID, &[("PROTON_FORCE_HDR".into(), "1".into())]).unwrap();
    setup_again();
    w.launch(
        &[
            ("XDG_RUNTIME_DIR", rd),
            ("WAYLAND_DISPLAY", "wayland-7"),
            ("FAKE_ENV_DUMP", dump_s),
        ],
        &[],
    );
    assert!(has(&game_env(), "PROTON_FORCE_HDR=1"), "{}", game_env());
    gogo::linuxapp::envargs::unset_env(ID, &["PROTON_FORCE_HDR".into()]).unwrap();

    // A session's own PROTON_WAYLAND_MONITOR gives way to the game's.
    w.launch(
        &[
            ("XDG_RUNTIME_DIR", rd),
            ("WAYLAND_DISPLAY", "wayland-7"),
            ("PROTON_WAYLAND_MONITOR", "HDMI-A-1"),
            ("FAKE_ENV_DUMP", dump_s),
        ],
        &[],
    );
    let env = game_env();
    assert!(has(&env, "PROTON_WAYLAND_MONITOR=DP-1"), "{env}");
    // The L4 env wins over the monitor too (and doctor says so).
    gogo::linuxapp::envargs::set_env(ID, &[("PROTON_WAYLAND_MONITOR".into(), "HDMI-A-1".into())])
        .unwrap();
    setup_again();
    w.launch(
        &[
            ("XDG_RUNTIME_DIR", rd),
            ("WAYLAND_DISPLAY", "wayland-7"),
            ("FAKE_ENV_DUMP", dump_s),
        ],
        &[],
    );
    let env = game_env();
    assert!(has(&env, "PROTON_WAYLAND_MONITOR=HDMI-A-1"), "{env}");
    gogo::linuxapp::envargs::unset_env(ID, &["PROTON_WAYLAND_MONITOR".into()]).unwrap();
    // Cleared alone: no line, no export, and the rest stays.
    set_display(ID, None, None, Some(None)).unwrap();
    setup_again();
    assert!(!w.conf().contains("DISPLAY_MONITOR="), "{}", w.conf());
    assert_eq!(ConfValues::parse(&w.conf()).get("DISPLAY_MODE"), "wayland");
    w.launch(
        &[
            ("XDG_RUNTIME_DIR", rd),
            ("WAYLAND_DISPLAY", "wayland-7"),
            ("FAKE_ENV_DUMP", dump_s),
        ],
        &[],
    );
    let env = game_env();
    assert!(has(&env, "PROTON_ENABLE_WAYLAND=1"), "{env}");
    assert!(!sets(&env, "PROTON_WAYLAND_MONITOR"), "{env}");
    assert!(!w.runs().pop().unwrap().contains("# monitor:"));
    // Set again, for x11 below.
    set_display(ID, None, None, Some(Some("DP-1".into()))).unwrap();
    setup_again();

    // The player's own value in the L4 env wins over the mode.
    gogo::linuxapp::envargs::set_env(ID, &[("PROTON_ENABLE_WAYLAND".into(), "0".into())]).unwrap();
    setup_again();
    let socket_env = [
        ("XDG_RUNTIME_DIR", rd),
        ("WAYLAND_DISPLAY", "wayland-7"),
        ("PROTON_USE_WAYLAND", "1"),
        ("FAKE_ENV_DUMP", dump_s),
    ];
    w.launch(&socket_env, &[]);
    assert!(
        has(&game_env(), "PROTON_ENABLE_WAYLAND=0"),
        "{}",
        game_env()
    );
    gogo::linuxapp::envargs::unset_env(ID, &["PROTON_ENABLE_WAYLAND".into()]).unwrap();

    // x11: both of the session's switches are gone, with a socket there.
    set_display(ID, Some(Some(DisplayMode::X11)), Some(None), None).unwrap();
    setup_again();
    let conf = ConfValues::parse(&w.conf());
    assert_eq!(conf.get("DISPLAY_MODE"), "x11");
    assert!(!w.conf().contains("DISPLAY_HDR="), "{}", w.conf());
    w.launch(&socket_env, &[]);
    let env = game_env();
    assert!(!sets(&env, "PROTON_ENABLE_WAYLAND"), "{env}");
    assert!(!sets(&env, "PROTON_USE_WAYLAND"), "{env}");
    let run = w.runs().pop().unwrap();
    assert!(run.contains("# display: x11\n"), "{run}");
    assert!(!run.contains("# hdr:"), "{run}");
    // x11 with a monitor kept: not exported, and logged as not applied.
    assert!(!sets(&env, "PROTON_WAYLAND_MONITOR"), "{env}");
    assert!(
        run.contains("# monitor: DP-1 (not applied: not on Wayland)\n"),
        "{run}"
    );

    // Cleared: no lines, and the environment's choice goes through.
    let out = set_display(ID, Some(None), Some(None), Some(None)).unwrap();
    assert_eq!(out.changes.len(), 2, "{:?}", out.changes);
    setup_again();
    assert!(!w.conf().contains("DISPLAY_"), "{}", w.conf());
    w.launch(&socket_env, &[]);
    assert!(has(&game_env(), "PROTON_USE_WAYLAND=1"), "{}", game_env());
    assert!(!w.runs().pop().unwrap().contains("# display:"));
}

/// Under a wrapper the socket check runs again in the wrapper's
/// environment, which is the game's: gamescope without --expose-wayland
/// drops WAYLAND_DISPLAY, so the game gets X11 and the run log says so.
#[test]
fn under_a_wrapper_the_display_check_is_the_wrappers() {
    use gogo::linuxapp::display::set_display;
    use gogo::macapp::compat::DisplayMode;
    let Some(w) = World::launcher("display-wrap", false) else {
        return;
    };
    set_display(ID, Some(Some(DisplayMode::Wayland)), None, None).unwrap();
    w.wrapped(&format!("unset WAYLAND_DISPLAY\n{WRAP_EXEC}"), &[]);
    let (run_dir, _socket) = wayland_socket();
    let dump = w.root.join("game.env");
    let log = w.wrap_log();
    let out = w.launch(
        &[
            ("XDG_RUNTIME_DIR", run_dir.path().to_str().unwrap()),
            ("WAYLAND_DISPLAY", "wayland-7"),
            ("FAKE_ENV_DUMP", dump.to_str().unwrap()),
            ("FAKE_WRAP_LOG", log.to_str().unwrap()),
        ],
        &[],
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let env = std::fs::read_to_string(&dump).unwrap();
    assert!(
        !env.lines().any(|l| l.starts_with("PROTON_ENABLE_WAYLAND=")),
        "{env}"
    );
    let run = w.runs().pop().unwrap();
    assert!(
        run.contains("# display: wayland (WAYLAND_DISPLAY=wayland-7)\n"),
        "{run}"
    );
    assert!(
        run.contains(
            "gogo: display under the wrapper: x11 (wayland wanted, but WAYLAND_DISPLAY is unset)\n"
        ),
        "{run}"
    );
    assert!(
        w.launch_log().contains(
            "display: wayland (WAYLAND_DISPLAY=wayland-7), checked again under the wrapper"
        ),
        "{}",
        w.launch_log()
    );
}

/// SimCity 4's case (design 18 §4.10): a compat `files` entry lands in the
/// game dir at setup, survives a re-run, and an edit of the player's stays.
#[test]
fn compat_files_are_written_into_the_game_dir() {
    let w = World::new("files", &["opengl32.dll"]);
    exe_in_apps(&w);
    let plan = build::plan(&w.opts(), None, &mut quiet()).unwrap();
    build::execute(&plan, None, &mut quiet()).unwrap();
    let ini = w.game().join("game/Apps/SimCity 4.ini");
    assert!(!ini.exists());

    let l4 = gogo::macapp::compat::user_path(ID);
    std::fs::create_dir_all(l4.parent().unwrap()).unwrap();
    std::fs::write(
        &l4,
        "files = [{ path = \"Apps/SimCity 4.ini\", text = \"[Directories]\\nData=..\\\\\\n\" }]\n",
    )
    .unwrap();
    let out = setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert_eq!(
        std::fs::read_to_string(&ini).unwrap(),
        "[Directories]\nData=..\\\n"
    );
    assert!(
        out.notes
            .iter()
            .any(|n| n == "wrote Apps/SimCity 4.ini (compat files)"),
        "{:?}",
        out.notes
    );
    let state = w.state();
    assert_eq!(state.files.len(), 1);
    assert!(state.files[0].ends_with(" Apps/SimCity 4.ini"));

    // Again: nothing to do.
    let out = setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert!(!out.notes.iter().any(|n| n.contains("SimCity 4.ini")));
    assert_eq!(w.state().files, state.files);

    // The player's edit stays, said once per setup.
    std::fs::write(&ini, "[Directories]\nData=D:\\SC4\\\n").unwrap();
    let out = setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert!(std::fs::read_to_string(&ini).unwrap().contains("D:\\SC4"));
    assert!(
        out.warnings
            .iter()
            .any(|w| w.contains("Apps/SimCity 4.ini: there already")),
        "{:?}",
        out.warnings
    );
    assert!(w.state().files.is_empty());
}

// — adding DLC to a built game (design 19 §3.2, §3.4, #350) —

mod dlc_add {
    use super::*;
    use gogo::gog::content;
    use gogo::gog::depot::{self, DepotError, DownloadInputs};
    use gogo::gog::dlc::{DlcInfo, ManifestDlc, available};
    use gogo::gog::lay;
    use gogo::linuxapp::dlc::{self, AddError, AddOptions, AddOutcome, Game, PlanInput, Source};
    use gogo::macapp::doctor::Level;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    const DLC: u64 = 777001;
    const ZERO: u64 = 777002;
    const TREE: &str = "Test Game";
    const REG_KEY: &str = r"Software\TestDlc";

    /// A built game whose `content.toml` says build `B1`, `en-US`, no DLC, with a
    /// `shared.pak` the DLC replaces.
    fn built(name: &str) -> World {
        let w = World::new(name, &["d3d9.dll"]);
        std::fs::write(w.tree.join("shared.pak"), b"base shared").unwrap();
        build_with(&w.opts());
        let mut c = content::load(&w.game()).unwrap().unwrap();
        c.build_id = Some("B1".into());
        c.language = Some("en-US".into());
        content::write(&w.game(), &c).unwrap();
        w
    }

    fn cache_base(w: &World) -> PathBuf {
        w.root.join("user-home/.cache")
    }

    fn cache_root(w: &World) -> PathBuf {
        cache_base(w).join("gogo/depot").join(ID)
    }

    /// A depot cache of build B1 (en-US, no DLC) holding the game's tree.
    fn cache(w: &World) {
        let root = cache_root(w);
        gogo::gog::materialize::materialize_tree(
            &w.tree,
            &root.join(TREE),
            gogo::gog::materialize::Reflink::Never,
        )
        .unwrap();
        depot::write_record(&root, "B1", "en-US", &[]).unwrap();
    }

    fn infos() -> Vec<DlcInfo> {
        let m = [
            ManifestDlc {
                id: DLC,
                download_size: 100,
                disk_size: 200,
            },
            ManifestDlc {
                id: ZERO,
                download_size: 0,
                disk_size: 0,
            },
        ];
        let mut v = available(&[], &[DLC, ZERO], Some(&m));
        v[0].title = "Extra Pack".into();
        v[1].title = "Hot Garbage".into();
        v
    }

    fn game(w: &World) -> Game {
        dlc::load_game(ID, Some(&cache_base(w))).unwrap()
    }

    fn plan_for(g: &Game, wanted: &[u64]) -> dlc::AddPlan {
        let infos = infos();
        dlc::plan_add(&PlanInput {
            game: g,
            title: "Test Game",
            version: Some("1.0".into()),
            newer: None,
            tree_name: TREE.into(),
            infos: &infos,
            wanted,
            cache_upgrade: None,
        })
    }

    fn script(id: u64) -> String {
        format!(
            r#"{{"productId":"{id}","actions":[{{"name":"k","install":{{"action":"setRegistry",
            "arguments":{{"root":"HKEY_LOCAL_MACHINE","subkey":"{}","valueName":"Dlc{id}",
            "valueType":"string","valueData":"on"}}}}}}]}}"#,
            REG_KEY.replace('\\', "\\\\")
        )
    }

    /// What gog-warp's targeted diff leaves in the source root: the DLC's files.
    fn dlc_files(root: &Path) {
        let t = root.join(TREE);
        std::fs::create_dir_all(t.join("dlc/ep1")).unwrap();
        std::fs::write(t.join("dlc/ep1/a.archive"), b"dlc archive").unwrap();
        std::fs::write(t.join("shared.pak"), b"dlc shared").unwrap();
        std::fs::write(
            t.join(format!("goggame-{DLC}.info")),
            format!(
                r#"{{"gameId":"{DLC}","rootGameId":"{ID}","name":"Extra Pack","playTasks":[]}}"#
            ),
        )
        .unwrap();
        std::fs::write(t.join(format!("goggame-{DLC}.script")), script(DLC)).unwrap();
    }

    struct Run {
        result: Result<AddOutcome, AddError>,
        inputs: Vec<DownloadInputs>,
        events: Vec<serde_json::Value>,
    }

    /// One add with a fake downloader: `fail` makes it stop as a Ctrl-C would.
    fn run(g: &mut Game, plan: &dlc::AddPlan, opts: AddOptions, fail: bool) -> Run {
        let seen: Arc<Mutex<Vec<DownloadInputs>>> = Arc::default();
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (s, st) = (seen.clone(), stopped.clone());
        let fetch = async move |inputs: DownloadInputs, root: PathBuf| {
            s.lock().unwrap().push(inputs);
            dlc_files(&root);
            if fail {
                st.store(true, std::sync::atomic::Ordering::SeqCst);
                return Err(DepotError::Library("cancelled".into()));
            }
            Ok::<(), DepotError>(())
        };
        let mut events = Vec::new();
        let stop = || stopped.load(std::sync::atomic::Ordering::SeqCst);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = rt.block_on(dlc::add(
            g,
            plan,
            fetch,
            &opts,
            &mut |e| events.push(serde_json::to_value(e).unwrap()),
            &stop,
        ));
        let inputs = seen.lock().unwrap().clone();
        Run {
            result,
            inputs,
            events,
        }
    }

    fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                if p.is_dir() && !p.is_symlink() {
                    walk(root, &p, out);
                } else if p.is_file() {
                    out.insert(rel, std::fs::read(&p).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(dir, dir, &mut out);
        out
    }

    fn no_leftovers(w: &World) {
        for f in [lay::JOURNAL, lay::BACKUP, lay::STAGING, dlc::DOWNLOAD] {
            assert!(!w.game().join(f).exists(), "{f} left behind");
        }
    }

    fn dlc_finding(w: &World) -> (Level, String) {
        let host = doctor_host(w, &[("radeon.x86_64", 2), ("radeon.i686", 1)]);
        let r = doctor(w, &host);
        let f = r.findings.iter().find(|f| f.check == "dlc").unwrap();
        (f.level, f.message.clone())
    }

    fn tag(e: &serde_json::Value) -> String {
        e.as_object()
            .and_then(|o| o.keys().next().cloned())
            .unwrap_or_else(|| e.as_str().unwrap_or("").to_string())
    }

    /// [`built`], with snapshots on (the fake backend): snapshot 1 is the
    /// `initial` one.
    fn built_with_snapshots(
        name: &str,
    ) -> (
        World,
        Arc<gogo::snapshot::fake::Fake>,
        gogo::linuxapp::snapshots::Backends,
    ) {
        use gogo::linuxapp::snapshots::{Backends, SnapshotOptions};
        let w = World::new(name, &["d3d9.dll"]);
        std::fs::write(w.tree.join("shared.pak"), b"base shared").unwrap();
        let f = Arc::new(gogo::snapshot::fake::Fake::new());
        let b = Backends::fixed(f.clone());
        build_with(&LinuxAppOptions {
            snapshots: SnapshotOptions::on(b.clone()),
            ..w.opts()
        });
        let mut c = content::load(&w.game()).unwrap().unwrap();
        c.build_id = Some("B1".into());
        c.language = Some("en-US".into());
        content::write(&w.game(), &c).unwrap();
        (w, f, b)
    }

    fn with_snapshots(b: &gogo::linuxapp::snapshots::Backends, skip: bool) -> AddOptions {
        AddOptions {
            snapshots: gogo::linuxapp::snapshots::AutoOptions {
                skip,
                backends: b.clone(),
                ..Default::default()
            },
            ..AddOptions::default()
        }
    }

    fn snapshot_ids(b: &gogo::linuxapp::snapshots::Backends) -> Vec<u32> {
        let mut v: Vec<u32> = gogo::linuxapp::snapshots::list(ID, b, 5, false)
            .unwrap()
            .snapshots
            .iter()
            .map(|s| s.id)
            .collect();
        v.sort();
        v
    }

    #[test]
    fn an_add_takes_before_adding_first() {
        let (w, _, b) = built_with_snapshots("dlc-autosnap");
        let mut g = game(&w);
        let p = plan_for(&g, &[DLC, ZERO]);
        let r = run(&mut g, &p, with_snapshots(&b, false), false);
        let out = r.result.unwrap();
        let s = out.snapshot.clone().expect("before adding");
        assert_eq!(
            (s.id, s.label.as_str()),
            (2, "before adding Extra Pack, Hot Garbage")
        );
        assert!(out.pruned.is_empty());
        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(json["snapshot"]["id"], 2);
        assert_eq!(json["pruned"], serde_json::json!([]));
        let created: Vec<_> = r
            .events
            .iter()
            .filter_map(|e| e.get("SnapshotCreated"))
            .collect();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0]["trigger"], "dlc-add");
        // Before the download and the lay: the event comes before the plan's,
        // and the snapshot has the game without the DLC.
        let at = |t: &str| r.events.iter().position(|e| tag(e) == t).unwrap();
        assert!(at("SnapshotCreated") < at("DlcPlan"));
        let two = gogo::snapshot::backend::snapshot_dir(&w.game(), 2).unwrap();
        assert_eq!(
            std::fs::read(two.join("game/shared.pak")).unwrap(),
            b"base shared"
        );
        assert!(!two.join("game/dlc").exists());
        assert!(!two.join(dlc::DOWNLOAD).exists());
        let m = gogo::linuxapp::snapshots::list(ID, &b, 5, false).unwrap();
        let s2 = m.snapshots.iter().find(|s| s.id == 2).unwrap();
        assert_eq!(s2.trigger, Some("dlc-add"));
        assert_eq!(s2.content.as_ref().unwrap().dlcs, Vec::<u64>::new());
    }

    #[test]
    fn a_failed_snapshot_stops_the_add_with_nothing_changed() {
        let (w, f, b) = built_with_snapshots("dlc-autosnap-fail");
        let before = tree(&w.game().join("game"));
        let content_before = std::fs::read(w.game().join("content.toml")).unwrap();
        let mut g = game(&w);
        let p = plan_for(&g, &[DLC]);
        f.fail_at(gogo::snapshot::fake::FakeStep::Snapshot);
        let r = run(&mut g, &p, with_snapshots(&b, false), false);
        let e = r.result.expect_err("the add stops");
        assert!(matches!(e, AddError::Game(_)), "exit 7: {e:?}");
        let msg = e.to_string();
        assert!(
            msg.contains("(before adding Extra Pack) couldn't be taken"),
            "{msg}"
        );
        assert!(
            msg.contains("`--no-snapshots` skips it for this run"),
            "{msg}"
        );
        assert!(r.inputs.is_empty(), "nothing fetched");
        assert_eq!(tree(&w.game().join("game")), before);
        assert_eq!(
            std::fs::read(w.game().join("content.toml")).unwrap(),
            content_before
        );
        no_leftovers(&w);
        assert_eq!(snapshot_ids(&b), [1]);

        // `--no-snapshots`: the add goes ahead, the setting stays on.
        let r = run(&mut g, &p, with_snapshots(&b, true), false);
        let out = r.result.unwrap();
        assert!(out.snapshot.is_none());
        assert_eq!(out.added.len(), 1);
        assert_eq!(snapshot_ids(&b), [1]);
        assert!(
            gogo::linuxapp::snapshots::list(ID, &b, 5, false)
                .unwrap()
                .auto
        );
    }

    #[test]
    fn an_add_fetches_only_the_dlc_lays_it_and_records_it() {
        let w = built("dlc-add");
        let mut g = game(&w);
        let p = plan_for(&g, &[DLC]);
        assert_eq!(
            p.source,
            Source::Staging,
            "no cache: staging in the game dir"
        );
        assert_eq!(p.game_files, "rename");
        let r = run(&mut g, &p, AddOptions::default(), false);
        let out = r.result.unwrap();

        // The downloader got the targeted diff for the installed build (§3.2).
        assert_eq!(
            r.inputs,
            [DownloadInputs {
                targeted: true,
                language: Some("en-US".into()),
                old_language: Some("en-US".into()),
                dlcs: vec![DLC.to_string()],
                old_dlcs: vec![],
            }]
        );
        // Laid: the new files, and the replaced base file.
        let gd = w.game().join("game");
        assert_eq!(
            std::fs::read(gd.join("dlc/ep1/a.archive")).unwrap(),
            b"dlc archive"
        );
        assert_eq!(std::fs::read(gd.join("shared.pak")).unwrap(), b"dlc shared");
        no_leftovers(&w);
        // Recorded: the commit.
        let c = content::load(&w.game()).unwrap().unwrap();
        assert_eq!(c.dlc.len(), 1);
        assert_eq!((c.dlc[0].id, c.dlc[0].title.as_str()), (DLC, "Extra Pack"));
        assert_eq!(c.dlc[0].build_id.as_deref(), Some("B1"));
        assert_eq!(out.added.len(), 1);
        // GOG's install script ran through setup.
        assert!(w.log().contains("TestDlc"), "{}", w.log());
        // Events (§7).
        let tags: Vec<String> = r.events.iter().map(tag).collect();
        for t in ["DlcPlan", "GameFilesStarted", "DlcInstalled"] {
            assert!(tags.contains(&t.to_string()), "{tags:?}");
        }
        let started = r
            .events
            .iter()
            .find(|e| tag(e) == "GameFilesStarted")
            .unwrap();
        assert_eq!(started["GameFilesStarted"]["kind"], "dlc");
        assert_eq!(started["GameFilesStarted"]["files"], 4);
        // The lay's running count (#420): one per file, then its end.
        let counts: Vec<&serde_json::Value> = r
            .events
            .iter()
            .filter(|e| tag(e) == "WorkProgress")
            .map(|e| &e["WorkProgress"])
            .collect();
        assert_eq!(counts.len(), 4, "{tags:?}");
        let last = counts[3];
        assert_eq!(
            (&last["files"], &last["files_total"]),
            (&4.into(), &4.into())
        );
        assert_eq!(last["bytes"], last["bytes_total"]);
        assert_eq!(last["step"], "game files");
        let at = |t: &str| tags.iter().position(|x| x == t).unwrap();
        assert!(at("GameFilesStarted") < at("WorkProgress"));
        assert!(tags.iter().rposition(|x| x == "WorkProgress") < Some(at("WorkDone")));
        let plan = r.events.iter().find(|e| tag(e) == "DlcPlan").unwrap();
        assert_eq!(plan["DlcPlan"]["source"], "staging");
        assert_eq!(plan["DlcPlan"]["buildId"], "B1");
        // doctor: ok, naming it.
        let (level, msg) = dlc_finding(&w);
        assert!(level == Level::Ok && msg.contains("Extra Pack"), "{msg}");

        // Again: installed already, a notice; nothing fetched.
        let mut g = game(&w);
        let p = plan_for(&g, &[DLC]);
        assert_eq!(p.already, [DLC]);
        assert!(p.dlcs.is_empty());
        let r = run(&mut g, &p, AddOptions::default(), false);
        assert!(r.result.is_ok() && r.inputs.is_empty());
    }

    #[test]
    fn from_the_cache_the_cache_keeps_the_files_and_its_record_gains_the_dlc() {
        let w = built("dlc-add-cache");
        cache(&w);
        let mut g = game(&w);
        let p = plan_for(&g, &[DLC]);
        assert_eq!(p.source, Source::Cache);
        assert!(matches!(p.game_files.as_str(), "reflink" | "copy"));
        let r = run(
            &mut g,
            &p,
            AddOptions {
                run_setup: false,
                ..Default::default()
            },
            false,
        );
        r.result.unwrap();
        let root = cache_root(&w);
        assert!(root.join(TREE).join("dlc/ep1/a.archive").exists());
        let rec = depot::read_record(&root).unwrap();
        assert_eq!(rec.dlcs, Some(vec![DLC.to_string()]));
        assert_eq!(
            std::fs::read(w.game().join("game/shared.pak")).unwrap(),
            b"dlc shared"
        );
        no_leftovers(&w);
    }

    // — a DLC the cache already holds (#381) —

    /// The DLC's files as its manifest lists them, relative to the cache root.
    fn dlc_paths() -> Vec<String> {
        [
            "dlc/ep1/a.archive".to_string(),
            "shared.pak".to_string(),
            format!("goggame-{DLC}.info"),
            format!("goggame-{DLC}.script"),
        ]
        .iter()
        .map(|p| format!("{TREE}/{p}"))
        .collect()
    }

    /// A depot cache of build B1 that holds the DLC (a `gogo depot --all-dlcs`,
    /// then a build without it): its files, a record listing it and, when given,
    /// the file lists. `shared.pak` is the base's and the DLC's.
    fn cache_with_dlc(w: &World, dlc_list: Option<Vec<String>>) -> PathBuf {
        cache(w);
        let root = cache_root(w);
        dlc_files(&root);
        depot::write_record(&root, "B1", "en-US", &[DLC.to_string()]).unwrap();
        if let Some(list) = dlc_list {
            let mut lists = BTreeMap::new();
            lists.insert(DLC.to_string(), list);
            lists.insert(
                ID.to_string(),
                vec![format!("{TREE}/d3d9.dll"), format!("{TREE}/shared.pak")],
            );
            depot::store_products(&root, "B1", &lists).unwrap();
        }
        root
    }

    fn no_setup() -> AddOptions {
        AddOptions {
            run_setup: false,
            ..Default::default()
        }
    }

    #[test]
    fn a_dlc_the_cache_holds_is_laid_from_it_with_no_download() {
        let w = built("dlc-add-held");
        let root = cache_with_dlc(&w, Some(dlc_paths()));
        let mut g = game(&w);
        let p = plan_for(&g, &[DLC]);
        assert_eq!(p.source, Source::Cache);
        assert_eq!(p.from_cache, [DLC]);
        assert!(!p.downloads() && p.fetches());
        assert_eq!(p.download_size, 0);
        let lines = dlc::plan_lines(&p);
        assert!(
            lines
                .iter()
                .any(|l| l == "files from: the depot cache (already there, no download)"),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("no download, disk")),
            "{lines:?}"
        );
        assert_eq!(
            serde_json::to_value(&p).unwrap()["fromCache"],
            serde_json::json!([DLC])
        );

        let r = run(&mut g, &p, AddOptions::default(), false);
        let out = r.result.unwrap();
        assert!(r.inputs.is_empty(), "no download: {:?}", r.inputs);
        assert_eq!(out.added.len(), 1);
        let gd = w.game().join("game");
        assert_eq!(
            std::fs::read(gd.join("dlc/ep1/a.archive")).unwrap(),
            b"dlc archive"
        );
        // A path the DLC shares with the base is laid as the cache has it.
        assert_eq!(std::fs::read(gd.join("shared.pak")).unwrap(), b"dlc shared");
        assert!(gd.join(format!("goggame-{DLC}.info")).exists());
        // GOG's install script came along and ran.
        assert!(w.log().contains("TestDlc"), "{}", w.log());
        // The cache keeps its files, and its record its DLC and file lists.
        assert!(root.join(TREE).join("dlc/ep1/a.archive").exists());
        assert_eq!(
            depot::read_record(&root).unwrap().dlcs,
            Some(vec![DLC.to_string()])
        );
        assert!(
            depot::record_products(&root, "B1")
                .unwrap()
                .contains_key(&DLC.to_string())
        );
        no_leftovers(&w);
        let c = content::load(&w.game()).unwrap().unwrap();
        assert_eq!(c.dlc.iter().map(|d| d.id).collect::<Vec<_>>(), [DLC]);
        let plan = r.events.iter().find(|e| tag(e) == "DlcPlan").unwrap();
        assert_eq!(plan["DlcPlan"]["source"], "cache");
        assert_eq!(plan["DlcPlan"]["fromCache"], serde_json::json!([DLC]));
        let (level, _) = dlc_finding(&w);
        assert_eq!(level, Level::Ok);
    }

    #[test]
    fn a_cache_missing_some_of_the_dlcs_files_falls_back_to_the_download() {
        let w = built("dlc-add-held-partial");
        let root = cache_with_dlc(&w, Some(dlc_paths()));
        std::fs::remove_file(root.join(TREE).join("dlc/ep1/a.archive")).unwrap();
        // The record is otherwise sound: it lists what's there and keeps its lists.
        depot::write_record(&root, "B1", "en-US", &[DLC.to_string()]).unwrap();
        let mut g = game(&w);
        let p = plan_for(&g, &[DLC]);
        assert_eq!(p.source, Source::Cache);
        assert!(p.from_cache.is_empty());
        assert!(p.downloads());
        let lines = dlc::plan_lines(&p);
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("files from: the depot cache (/")),
            "{lines:?}"
        );
        let r = run(&mut g, &p, no_setup(), false);
        r.result.unwrap();
        assert_eq!(r.inputs.len(), 1, "the targeted diff completes it");
        assert_eq!(r.inputs[0].old_dlcs, Vec::<String>::new());
        assert_eq!(r.inputs[0].dlcs, [DLC.to_string()]);
        assert_eq!(
            std::fs::read(w.game().join("game/dlc/ep1/a.archive")).unwrap(),
            b"dlc archive"
        );
        no_leftovers(&w);
    }

    #[test]
    fn a_cache_without_file_lists_falls_back_to_the_download() {
        let w = built("dlc-add-held-nolist");
        cache_with_dlc(&w, None);
        let mut g = game(&w);
        let p = plan_for(&g, &[DLC]);
        assert_eq!(p.source, Source::Cache);
        assert!(p.from_cache.is_empty() && p.downloads());
        let r = run(&mut g, &p, no_setup(), false);
        r.result.unwrap();
        assert_eq!(r.inputs.len(), 1);
        assert_eq!(content::load(&w.game()).unwrap().unwrap().dlc.len(), 1);
    }

    /// The list spells a dir otherwise than the cache does, and the game has it in a
    /// third casing: the file is found in the cache and laid into the game's dir.
    #[test]
    fn a_case_mismatched_path_is_found_in_the_cache_and_laid_in_the_games_casing() {
        let w = built("dlc-add-held-case");
        let mut list = dlc_paths();
        list[0] = format!("{TREE}/DLC/EP1/a.archive");
        cache_with_dlc(&w, Some(list));
        let gd = w.game().join("game");
        std::fs::create_dir_all(gd.join("Dlc/Ep1")).unwrap();
        let mut g = game(&w);
        let p = plan_for(&g, &[DLC]);
        assert_eq!(p.from_cache, [DLC]);
        let r = run(&mut g, &p, no_setup(), false);
        r.result.unwrap();
        assert!(r.inputs.is_empty());
        assert_eq!(
            std::fs::read(gd.join("Dlc/Ep1/a.archive")).unwrap(),
            b"dlc archive"
        );
        let names: Vec<String> = std::fs::read_dir(&gd)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.contains(&"Dlc".to_string()) && !names.contains(&"dlc".to_string()),
            "{names:?}"
        );
        no_leftovers(&w);
    }

    /// A stop mid-lay from the cache: the journal and the shared file's backup
    /// stay, nothing is fetched, and the next add rolls forward without moving the
    /// cache's files.
    #[test]
    fn an_interrupted_lay_from_the_cache_recovers() {
        let w = built("dlc-add-held-stop");
        let root = cache_with_dlc(&w, Some(dlc_paths()));
        let mut g = game(&w);
        let p = plan_for(&g, &[DLC]);
        let opts = AddOptions {
            run_setup: false,
            fail_at: Some(lay::Step::Backup),
            ..Default::default()
        };
        let r = run(&mut g, &p, opts, false);
        assert!(
            matches!(r.result, Err(AddError::Interrupted("lay", _))),
            "{:?}",
            r.result.err()
        );
        assert!(r.inputs.is_empty());
        assert!(dlc::journal_exists(&w.game()));
        assert_eq!(
            std::fs::read(w.game().join(lay::BACKUP).join("shared.pak")).unwrap(),
            b"base shared"
        );
        let (level, _) = dlc_finding(&w);
        assert_eq!(level, Level::Fail);
        assert!(content::load(&w.game()).unwrap().unwrap().dlc.is_empty());

        let mut g = game(&w);
        let p = plan_for(&g, &[]);
        let r = run(&mut g, &p, no_setup(), false);
        let out = r.result.unwrap();
        assert_eq!(out.recovered, Some(lay::Direction::Forward));
        assert!(r.inputs.is_empty());
        let gd = w.game().join("game");
        assert_eq!(std::fs::read(gd.join("shared.pak")).unwrap(), b"dlc shared");
        assert_eq!(
            std::fs::read(gd.join("dlc/ep1/a.archive")).unwrap(),
            b"dlc archive"
        );
        assert!(root.join(TREE).join("dlc/ep1/a.archive").exists());
        assert_eq!(content::load(&w.game()).unwrap().unwrap().dlc.len(), 1);
        no_leftovers(&w);
    }

    /// The cache lost a file between the stop and the rerun: rolled back, the game
    /// as it was.
    #[test]
    fn an_interrupted_lay_from_a_cache_that_lost_a_file_rolls_back() {
        let w = built("dlc-add-held-back");
        let before = tree(&w.game().join("game"));
        let root = cache_with_dlc(&w, Some(dlc_paths()));
        let mut g = game(&w);
        let p = plan_for(&g, &[DLC]);
        let opts = AddOptions {
            run_setup: false,
            fail_at: Some(lay::Step::Journal),
            ..Default::default()
        };
        assert!(run(&mut g, &p, opts, false).result.is_err());
        std::fs::remove_file(root.join(TREE).join("dlc/ep1/a.archive")).unwrap();
        let mut g = game(&w);
        let back =
            dlc::recover(&mut g, gogo::gog::materialize::Reflink::Never, &mut |_| {}).unwrap();
        assert_eq!(back, Some(lay::Direction::Back));
        assert_eq!(tree(&w.game().join("game")), before);
        assert!(content::load(&w.game()).unwrap().unwrap().dlc.is_empty());
        no_leftovers(&w);
    }

    /// One DLC held, one not: only the other is downloaded; the diff counts the held
    /// one as there.
    #[test]
    fn a_mixed_add_downloads_only_what_the_cache_lacks() {
        const OTHER: u64 = 777003;
        let w = built("dlc-add-held-mixed");
        cache_with_dlc(&w, Some(dlc_paths()));
        let g = game(&w);
        let m = [
            ManifestDlc {
                id: DLC,
                download_size: 100,
                disk_size: 200,
            },
            ManifestDlc {
                id: OTHER,
                download_size: 50,
                disk_size: 60,
            },
        ];
        let mut infos = available(&[], &[DLC, OTHER], Some(&m));
        infos[0].title = "Extra Pack".into();
        infos[1].title = "Other Pack".into();
        let p = dlc::plan_add(&PlanInput {
            game: &g,
            title: "Test Game",
            version: Some("1.0".into()),
            newer: None,
            tree_name: TREE.into(),
            infos: &infos,
            wanted: &[DLC, OTHER],
            cache_upgrade: None,
        });
        assert_eq!(p.from_cache, [DLC]);
        assert_eq!(p.download_size, 50);
        assert_eq!(p.inputs.old_dlcs, [DLC.to_string()]);
        assert_eq!(p.inputs.dlcs, [DLC.to_string(), OTHER.to_string()]);
        let titles: Vec<&str> = p.downloading().iter().map(|d| d.title.as_str()).collect();
        assert_eq!(titles, ["Other Pack"]);
        let lines = dlc::plan_lines(&p);
        assert!(
            lines
                .iter()
                .any(|l| l.ends_with("; Extra Pack already there, the rest downloaded into it")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_stop_during_the_download_leaves_the_game_untouched_and_a_rerun_resumes() {
        let w = built("dlc-add-dl-stop");
        let before = tree(&w.game().join("game"));
        let mut g = game(&w);
        let p = plan_for(&g, &[DLC]);
        let r = run(&mut g, &p, AddOptions::default(), true);
        assert!(
            matches!(r.result, Err(AddError::Interrupted("download", _))),
            "{:?}",
            r.result.err()
        );
        assert_eq!(tree(&w.game().join("game")), before, "game/ untouched");
        assert!(!w.game().join(lay::JOURNAL).exists());
        assert!(dlc::download_pending(&w.game()));
        let (level, msg) = dlc_finding(&w);
        assert!(level == Level::Warn && msg.contains("download"), "{msg}");
        // `move` refuses while the add's files are in the game dir.
        let err = gogo::linuxapp::relocate::plan(
            ID,
            &gogo::macapp::relocate::Target::Default,
            &Default::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("DLC add"), "{err}");

        // A rerun resumes into the same staging dir and finishes.
        let mut g = game(&w);
        let p = plan_for(&g, &[DLC]);
        assert_eq!(p.source, Source::Staging);
        let r = run(&mut g, &p, AddOptions::default(), false);
        r.result.unwrap();
        assert_eq!(
            std::fs::read(w.game().join("game/dlc/ep1/a.archive")).unwrap(),
            b"dlc archive"
        );
        no_leftovers(&w);
    }

    /// A stop at each lay step: the journal stays, doctor fails, `move` refuses, and
    /// the next add rolls forward to the same game as an uninterrupted add.
    #[test]
    fn a_stop_during_the_lay_keeps_the_journal_and_the_next_add_finishes_it() {
        let w = built("dlc-add-lay-stop");
        let mut reference = None;
        for step in [
            lay::Step::Journal,
            lay::Step::New,
            lay::Step::Backup,
            lay::Step::Rename,
        ] {
            // Back to the base game each round.
            let mut c = content::load(&w.game()).unwrap().unwrap();
            c.dlc.clear();
            content::write(&w.game(), &c).unwrap();
            let game_dir = w.game().join("game");
            let _ = std::fs::remove_dir_all(game_dir.join("dlc"));
            for f in [
                format!("goggame-{DLC}.info"),
                format!("goggame-{DLC}.script"),
            ] {
                let _ = std::fs::remove_file(game_dir.join(f));
            }
            std::fs::write(game_dir.join("shared.pak"), b"base shared").unwrap();

            let mut g = game(&w);
            let p = plan_for(&g, &[DLC]);
            let opts = AddOptions {
                run_setup: false,
                fail_at: Some(step),
                ..Default::default()
            };
            let r = run(&mut g, &p, opts, false);
            assert!(
                matches!(r.result, Err(AddError::Interrupted("lay", _))),
                "{step:?}: {:?}",
                r.result.err()
            );
            assert!(dlc::journal_exists(&w.game()), "{step:?}");
            let (level, msg) = dlc_finding(&w);
            assert!(
                level == Level::Fail && msg.contains(lay::JOURNAL),
                "{step:?}: {msg}"
            );
            assert!(
                gogo::linuxapp::relocate::plan(
                    ID,
                    &gogo::macapp::relocate::Target::Default,
                    &Default::default()
                )
                .is_err()
            );
            assert!(content::load(&w.game()).unwrap().unwrap().dlc.is_empty());

            // A bare rerun: the recovery is the job.
            let mut g = game(&w);
            let p = plan_for(&g, &[]);
            let r = run(
                &mut g,
                &p,
                AddOptions {
                    run_setup: false,
                    ..Default::default()
                },
                false,
            );
            let out = r.result.unwrap();
            assert_eq!(out.recovered, Some(lay::Direction::Forward), "{step:?}");
            assert!(
                r.events.iter().any(|e| tag(e) == "DlcRecovered"),
                "{:?}",
                r.events
            );
            no_leftovers(&w);
            assert_eq!(content::load(&w.game()).unwrap().unwrap().dlc.len(), 1);
            let now = tree(&game_dir);
            match &reference {
                None => reference = Some(now),
                Some(r) => assert_eq!(&now, r, "{step:?}"),
            }
        }
        let r = reference.unwrap();
        assert_eq!(r["shared.pak"], b"dlc shared");
        assert_eq!(r["dlc/ep1/a.archive"], b"dlc archive");
    }

    #[test]
    fn a_lay_whose_source_is_gone_rolls_back_to_the_base_game() {
        let w = built("dlc-add-back");
        let before = tree(&w.game().join("game"));
        let mut g = game(&w);
        let p = plan_for(&g, &[DLC]);
        let opts = AddOptions {
            run_setup: false,
            fail_at: Some(lay::Step::Journal),
            ..Default::default()
        };
        assert!(run(&mut g, &p, opts, false).result.is_err());
        // The staging dir lost a file (deleted by hand, a full disk…).
        std::fs::remove_file(
            w.game()
                .join(lay::STAGING)
                .join(TREE)
                .join("dlc/ep1/a.archive"),
        )
        .unwrap();
        let mut g = game(&w);
        let back =
            dlc::recover(&mut g, gogo::gog::materialize::Reflink::Never, &mut |_| {}).unwrap();
        assert_eq!(back, Some(lay::Direction::Back));
        assert_eq!(tree(&w.game().join("game")), before);
        assert!(content::load(&w.game()).unwrap().unwrap().dlc.is_empty());
        no_leftovers(&w);
    }

    /// Design 19 §3.2: a DLC with no depot bytes runs no downloader, writes no
    /// journal and is recorded; its `goggame` files and script come from the cache's
    /// support overlay when it has them.
    #[test]
    fn a_zero_byte_dlc_is_a_record_only_add() {
        let w = built("dlc-add-zero");
        let mut g = game(&w);
        let p = plan_for(&g, &[ZERO]);
        assert!(!p.fetches());
        let r = run(&mut g, &p, AddOptions::default(), false);
        r.result.unwrap();
        assert!(r.inputs.is_empty(), "no downloader");
        assert!(
            !r.events.iter().any(|e| tag(e) == "GameFilesStarted"),
            "nothing laid: {:?}",
            r.events
        );
        no_leftovers(&w);
        let c = content::load(&w.game()).unwrap().unwrap();
        assert_eq!(c.dlc.iter().map(|d| d.id).collect::<Vec<_>>(), [ZERO]);
    }

    #[test]
    fn a_zero_byte_dlc_takes_its_goggame_files_from_the_cache_overlay() {
        let w = built("dlc-add-zero-cache");
        cache(&w);
        let app = cache_root(&w).join(format!("gog-support/{ID}/{ZERO}/app"));
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(
            app.join(format!("goggame-{ZERO}.info")),
            format!(
                r#"{{"gameId":"{ZERO}","rootGameId":"{ID}","name":"Hot Garbage","playTasks":[]}}"#
            ),
        )
        .unwrap();
        std::fs::write(app.join(format!("goggame-{ZERO}.script")), script(ZERO)).unwrap();
        let mut g = game(&w);
        let p = plan_for(&g, &[ZERO]);
        assert_eq!(p.source, Source::Cache);
        let r = run(&mut g, &p, AddOptions::default(), false);
        r.result.unwrap();
        assert!(r.inputs.is_empty(), "no downloader");
        assert!(w.game().join(format!("game/goggame-{ZERO}.info")).exists());
        assert!(w.log().contains(&format!("Dlc{ZERO}")), "{}", w.log());
        let rec = depot::read_record(&cache_root(&w)).unwrap();
        assert_eq!(rec.dlcs, Some(vec![ZERO.to_string()]));
        no_leftovers(&w);
        let (level, _) = dlc_finding(&w);
        assert_eq!(level, Level::Ok);
    }

    #[test]
    fn apps_remove_takes_a_leftover_staging_dir_with_the_game() {
        use gogo::macapp::apps;
        let w = built("dlc-add-remove");
        std::fs::create_dir_all(w.game().join(lay::STAGING).join(TREE)).unwrap();
        let logs = w.root.join("logs");
        let a = apps::get(ID, &logs).unwrap().unwrap();
        apps::remove(&a, false).unwrap();
        assert!(!w.game().exists());
    }

    #[test]
    fn the_launcher_refuses_while_a_dlc_journal_exists() {
        let Some(w) = World::launcher("dlc-launch", true) else {
            return;
        };
        std::fs::write(w.game().join(lay::JOURNAL), "{}").unwrap();
        let stub = w.dialog_stub(&["OK"]);
        let out = w.launch(&[("GOGO_LAUNCHER_DIALOG", stub.to_str().unwrap())], &[]);
        assert_eq!(out.status.code(), Some(1));
        assert!(
            w.launch_log().contains("didn't finish"),
            "{}",
            w.launch_log()
        );
        assert!(
            w.dialogs()[0].contains(&format!("gogo linux-app dlc {ID} add")),
            "{:?}",
            w.dialogs()
        );
        assert_eq!(w.game_runs(), 0);
        std::fs::remove_file(w.game().join(lay::JOURNAL)).unwrap();
    }

    // — the depot cache keeps what an add fetches (#374) —

    /// The cache's base files as a depot manifest would list them (relative to the
    /// cache root), read from a cache holding only the base.
    fn base_paths(root: &Path) -> Vec<String> {
        tree(&root.join(TREE))
            .into_keys()
            .map(|p| format!("{TREE}/{p}"))
            .collect()
    }

    /// The build's depots: the base's (every language) and the DLC's.
    fn depots(base: Vec<String>) -> Vec<gogo::gog::dlc_filter::DepotList> {
        use gogo::gog::dlc_filter::DepotList;
        vec![
            DepotList {
                product: ID.into(),
                languages: vec!["*".into()],
                manifest: "m-base".into(),
                paths: base,
                sizes: Default::default(),
            },
            DepotList {
                product: DLC.to_string(),
                languages: vec!["*".into()],
                manifest: "m-dlc".into(),
                paths: dlc_paths(),
                sizes: Default::default(),
            },
        ]
    }

    /// Rewrite the cache's record as a gogo from before #347 wrote it: `buildId`
    /// and `files` only.
    fn make_old(root: &Path) {
        let path = root.join(depot::RECORD);
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let o = v.as_object_mut().unwrap();
        for k in ["language", "dlcs", "products"] {
            o.remove(k);
        }
        std::fs::write(&path, v.to_string()).unwrap();
        assert!(depot::record_is_old(root, "B1"));
    }

    fn plan_with(
        g: &Game,
        wanted: &[u64],
        up: &Result<gogo::gog::cache_upgrade::Upgrade, String>,
    ) -> dlc::AddPlan {
        let infos = infos();
        dlc::plan_add(&PlanInput {
            game: g,
            title: "Test Game",
            version: Some("1.0".into()),
            newer: None,
            tree_name: TREE.into(),
            infos: &infos,
            wanted,
            cache_upgrade: Some(up),
        })
    }

    /// Remove the game and build it again from the cache with `wanted`, as the CLI
    /// would: the cache needs no repair (no hashing, no download) and no file list
    /// fetch, since its record says it holds them.
    fn rebuild_from_cache(w: &World, root: &Path, wanted: &[u64]) {
        use gogo::gog::dlc_filter::{self, Need};
        let ids: Vec<String> = wanted.iter().map(u64::to_string).collect();
        assert_eq!(
            depot::check_cache(
                root,
                &depot::CacheWant {
                    build_id: Some("B1"),
                    language: Some("en-US"),
                    dlcs: &ids,
                },
            ),
            depot::CacheCheck::Complete,
            "no repair: nothing hashed, nothing downloaded"
        );
        assert_eq!(
            dlc_filter::need(root, &root.join(TREE), ID, wanted, "en-US"),
            Need::Whole
        );
        std::fs::remove_dir_all(w.game()).unwrap();
        let opts = LinuxAppOptions {
            game_tree: Some(root.join(TREE)),
            depot_content: Some(build::DepotContent {
                dlcs: wanted
                    .iter()
                    .map(|id| content::DlcRef {
                        id: *id,
                        title: "Extra Pack".into(),
                    })
                    .collect(),
                language: None,
                version: None,
                filter: None,
            }),
            ..w.opts()
        };
        let out = build_with(&opts);
        assert_eq!(
            out.content.dlc.iter().map(|d| d.id).collect::<Vec<_>>(),
            wanted
        );
        assert_eq!(
            std::fs::read(w.game().join("game/dlc/ep1/a.archive")).unwrap(),
            b"dlc archive"
        );
    }

    #[test]
    fn an_old_cache_record_is_brought_up_to_date_and_the_add_keeps_the_dlc_in_it() {
        let w = built("dlc-add-old-record");
        cache(&w);
        let root = cache_root(&w);
        make_old(&root);
        let old = std::fs::read(root.join(depot::RECORD)).unwrap();
        let up = gogo::gog::cache_upgrade::decide(
            &root,
            TREE,
            ID,
            "B1",
            &depots(base_paths(&root)),
            "en-US",
        );
        let mut g = game(&w);
        let p = plan_with(&g, &[DLC], &up);
        assert_eq!(p.source, Source::Cache);
        let lines = dlc::plan_lines(&p);
        assert!(
            lines.iter().any(|l| l
                == "depot cache: its record is brought up to date (en-US; DLC: none), and \
                    the DLC is kept in it too"),
            "{lines:?}"
        );
        assert_eq!(
            serde_json::to_value(&p).unwrap()["depotCache"],
            serde_json::json!({"action": "kept", "upgraded": "en-US; DLC: none"})
        );
        // The plan writes nothing.
        assert_eq!(std::fs::read(root.join(depot::RECORD)).unwrap(), old);

        let r = run(&mut g, &p, no_setup(), false);
        r.result.unwrap();
        assert_eq!(r.inputs.len(), 1);
        assert_eq!(r.inputs[0].old_dlcs, Vec::<String>::new());
        assert_eq!(r.inputs[0].dlcs, [DLC.to_string()]);
        // The DLC went into the cache, and the record says so.
        assert!(root.join(TREE).join("dlc/ep1/a.archive").is_file());
        let rec = depot::read_record(&root).unwrap();
        assert_eq!(rec.language.as_deref(), Some("en-US"));
        assert_eq!(rec.dlcs, Some(vec![DLC.to_string()]));
        let lists = depot::record_products(&root, "B1").unwrap();
        assert!(lists.contains_key(ID) && lists.contains_key(&DLC.to_string()));
        no_leftovers(&w);

        rebuild_from_cache(&w, &root, &[DLC]);
    }

    #[test]
    fn a_dlc_an_old_cache_holds_is_laid_from_it_after_the_upgrade() {
        let w = built("dlc-add-old-record-held");
        cache(&w);
        let root = cache_root(&w);
        let base = base_paths(&root);
        dlc_files(&root);
        // An older gogo downloaded it with the DLC: the record names every file.
        depot::write_record(&root, "B1", "en-US", &[]).unwrap();
        make_old(&root);
        let up = gogo::gog::cache_upgrade::decide(&root, TREE, ID, "B1", &depots(base), "en-US");
        assert_eq!(up.as_ref().unwrap().dlcs, [DLC.to_string()]);
        let mut g = game(&w);
        let p = plan_with(&g, &[DLC], &up);
        assert_eq!(p.source, Source::Cache);
        assert_eq!(p.from_cache, [DLC]);
        assert!(!p.downloads());
        let r = run(&mut g, &p, no_setup(), false);
        r.result.unwrap();
        assert!(r.inputs.is_empty(), "no download: {:?}", r.inputs);
        assert_eq!(
            std::fs::read(w.game().join("game/dlc/ep1/a.archive")).unwrap(),
            b"dlc archive"
        );
        assert_eq!(
            depot::read_record(&root).unwrap().dlcs,
            Some(vec![DLC.to_string()])
        );
        rebuild_from_cache(&w, &root, &[DLC]);
    }

    /// A DLC staged in an earlier add (`SECOND`) isn't in the cache: the cache is
    /// still the source of the next one, which it keeps, and its record doesn't
    /// claim the staged one.
    #[test]
    fn a_cache_lacking_a_staged_dlc_is_the_source_and_keeps_the_new_one() {
        const SECOND: u64 = 777003;
        let w = built("dlc-add-staged-before");
        cache(&w);
        let root = cache_root(&w);
        let mut c = content::load(&w.game()).unwrap().unwrap();
        c.dlc.push(content::DlcRow {
            id: SECOND,
            title: "Staged Pack".into(),
            build_id: Some("B1".into()),
            version: None,
            added: None,
        });
        content::write(&w.game(), &c).unwrap();
        let mut g = game(&w);
        let p = plan_for(&g, &[DLC]);
        assert_eq!(p.source, Source::Cache);
        assert_eq!(p.depot_cache, dlc::CacheFate::Kept { upgraded: None });
        assert!(
            dlc::plan_lines(&p)
                .iter()
                .any(|l| l == "depot cache: the DLC is kept in it too")
        );
        // The diff is told what the cache holds, not what the game has.
        assert_eq!(p.inputs.old_dlcs, Vec::<String>::new());
        assert_eq!(p.inputs.dlcs, [DLC.to_string()]);
        let r = run(&mut g, &p, no_setup(), false);
        r.result.unwrap();
        assert!(root.join(TREE).join("dlc/ep1/a.archive").is_file());
        assert_eq!(
            depot::read_record(&root).unwrap().dlcs,
            Some(vec![DLC.to_string()]),
            "the staged DLC isn't claimed"
        );
        let c = content::load(&w.game()).unwrap().unwrap();
        assert_eq!(
            c.dlc.iter().map(|d| d.id).collect::<Vec<_>>(),
            [SECOND, DLC]
        );
        no_leftovers(&w);
        rebuild_from_cache(&w, &root, &[DLC]);
    }

    #[test]
    fn an_old_record_that_cant_be_decided_is_left_alone_and_the_add_stages() {
        let w = built("dlc-add-old-record-undecided");
        cache(&w);
        let root = cache_root(&w);
        make_old(&root);
        let old = std::fs::read(root.join(depot::RECORD)).unwrap();
        // The build has English voice files, and the cache doesn't: it may have been
        // downloaded in another language.
        let mut d = depots(base_paths(&root));
        d.push(gogo::gog::dlc_filter::DepotList {
            product: ID.into(),
            languages: vec!["en-US".into()],
            manifest: "m-en".into(),
            paths: vec![format!("{TREE}/vo/en.pak")],
            sizes: Default::default(),
        });
        let up = gogo::gog::cache_upgrade::decide(&root, TREE, ID, "B1", &d, "en-US");
        assert!(up.is_err(), "{up:?}");
        let mut g = game(&w);
        let p = plan_with(&g, &[DLC], &up);
        assert_eq!(p.source, Source::Staging);
        let lines = dlc::plan_lines(&p);
        assert!(
            lines.iter().any(|l| l.starts_with(
                "depot cache: the depot cache's record predates DLC tracking and can't be \
                 brought up to date: "
            ) && l
                .ends_with("so the DLC isn't kept in it (a rebuild with it downloads it again)")),
            "{lines:?}"
        );
        let r = run(&mut g, &p, no_setup(), false);
        r.result.unwrap();
        assert_eq!(
            std::fs::read(root.join(depot::RECORD)).unwrap(),
            old,
            "the record is untouched"
        );
        assert!(!root.join(TREE).join("dlc/ep1/a.archive").exists());
        assert!(w.game().join("game/dlc/ep1/a.archive").is_file());
        no_leftovers(&w);
    }

    #[test]
    fn with_no_cache_the_plan_says_the_dlc_isnt_kept() {
        let w = built("dlc-add-no-cache");
        let g = game(&w);
        let p = plan_for(&g, &[DLC]);
        assert_eq!(p.source, Source::Staging);
        assert_eq!(p.depot_cache, dlc::CacheFate::None);
        assert!(dlc::plan_lines(&p).iter().any(|l| l
            == "depot cache: none, so the DLC isn't kept in one (a rebuild with it \
                    downloads it again)"));
    }

    /// The cache changed between the plan and the add (a DLC file the upgrade
    /// counted is gone): the record stays as it was and nothing is fetched.
    #[test]
    fn an_upgrade_the_cache_no_longer_bears_out_isnt_written() {
        let w = built("dlc-add-old-record-changed");
        cache(&w);
        let root = cache_root(&w);
        let base = base_paths(&root);
        dlc_files(&root);
        depot::write_record(&root, "B1", "en-US", &[]).unwrap();
        make_old(&root);
        let up = gogo::gog::cache_upgrade::decide(&root, TREE, ID, "B1", &depots(base), "en-US");
        let mut g = game(&w);
        let p = plan_with(&g, &[DLC], &up);
        std::fs::remove_file(root.join(TREE).join("dlc/ep1/a.archive")).unwrap();
        let old = std::fs::read(root.join(depot::RECORD)).unwrap();
        let r = run(&mut g, &p, no_setup(), false);
        assert!(r.result.is_err());
        assert!(r.inputs.is_empty());
        assert_eq!(std::fs::read(root.join(depot::RECORD)).unwrap(), old);
        assert!(content::load(&w.game()).unwrap().unwrap().dlc.is_empty());
    }
}

// — a game's exes and one-off runs (#364, design 18 §7.10) —

mod one_off {
    use super::*;
    use gogo::linuxapp::exes;
    use sha2::Digest as _;

    const DLC: &str = "1999999902";

    /// A tree shaped like Cyberpunk 2077's: a primary vendor launcher and the
    /// game sharing one name, a tool with a working dir and quoted args, a
    /// document, a DLC's tool, and exes no task names.
    fn exes_tree(w: &World) {
        let t = &w.tree;
        let put = |rel: &str, pe64: bool| {
            let p = t.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, pe_with_imports(pe64, &["d3d9.dll"])).unwrap();
        };
        put("REDprelauncher.exe", true);
        put("bin/x64/Game64.exe", true);
        put("tools/Config.exe", false);
        put("tools/redmod/bin/redMod.exe", true);
        put("unins000.exe", false);
        put("dlc/editor.exe", false);
        std::fs::write(
            t.join(format!("goggame-{ID}.info")),
            format!(
                r#"{{"gameId":"{ID}","rootGameId":"{ID}","name":"Test Game","playTasks":[
                {{"type":"FileTask","category":"launcher","path":"REDprelauncher.exe","isPrimary":true,"name":"Test Game"}},
                {{"type":"FileTask","category":"game","path":"bin\\x64\\Game64.exe","arguments":"-windowed","name":"Test Game"}},
                {{"type":"FileTask","category":"document","path":"manual.pdf","name":"Manual"}},
                {{"type":"FileTask","category":"tool","path":"tools\\Config.exe","workingDir":"tools","arguments":"-cfg \"my file.ini\"","name":"Settings"}}
                ]}}"#
            ),
        )
        .unwrap();
        std::fs::write(
            t.join(format!("goggame-{DLC}.info")),
            format!(
                r#"{{"gameId":"{DLC}","rootGameId":"{ID}","name":"Test DLC","playTasks":[
                {{"type":"FileTask","category":"tool","path":"dlc\\editor.exe","name":"DLC Editor"}}]}}"#
            ),
        )
        .unwrap();
    }

    fn gog(w: &World) -> PathBuf {
        w.game().join("prefix/drive_c/GOG/Test Game")
    }

    fn argv(p: &exes::RunPlan) -> Vec<String> {
        p.argv()
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect()
    }

    fn one_off_logs(w: &World) -> Vec<String> {
        let mut logs: Vec<PathBuf> = std::fs::read_dir(w.logs())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(gogo::linuxapp::launcher::ONE_OFF_LOG_PREFIX)
            })
            .collect();
        logs.sort();
        logs.iter()
            .map(|p| std::fs::read_to_string(p).unwrap())
            .collect()
    }

    /// The game's configuration: sha256 and mtime of each file a run must
    /// not write (`None`: not there).
    fn config(w: &World) -> Vec<(PathBuf, Option<(String, std::time::SystemTime)>)> {
        let g = w.game();
        [
            g.join("gogo-launch.toml"),
            g.join("launch.conf"),
            g.join("launch.sh"),
            g.join("state.toml"),
            g.join("content.toml"),
            g.join("icon.png"),
            w.home.join(format!("compat/{ID}.toml")),
            gogo::linuxapp::game::desktop_path(ID),
        ]
        .into_iter()
        .map(|p| {
            let v = std::fs::read(&p).ok().map(|b| {
                (
                    format!("{:x}", sha2::Sha256::digest(&b)),
                    std::fs::metadata(&p).unwrap().modified().unwrap(),
                )
            });
            (p, v)
        })
        .collect()
    }

    /// An env dump as a map, without what differs between any two runs.
    fn env_map(p: &Path) -> std::collections::BTreeMap<String, String> {
        std::fs::read_to_string(p)
            .unwrap()
            .lines()
            .filter_map(|l| l.split_once('='))
            .filter(|(k, _)| {
                !matches!(*k, "PWD" | "OLDPWD" | "_" | "SHLVL") && !k.starts_with("FAKE_")
            })
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn exes_lists_base_and_dlc_play_tasks_and_with_all_every_exe() {
        let w = World::launcher_built_prep("exes-list", false, false, &exes_tree);
        let l = exes::list(ID, false).unwrap();
        type Row<'a> = (
            Option<usize>,
            Option<&'a str>,
            &'a str,
            Option<&'a str>,
            bool,
            bool,
            bool,
            Option<u8>,
        );
        let rows: Vec<Row> = l
            .exes
            .iter()
            .map(|e| {
                (
                    e.index,
                    e.name.as_deref(),
                    e.path.as_str(),
                    e.category.as_deref(),
                    e.primary,
                    e.dlc,
                    e.launcher,
                    e.bits,
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                (
                    Some(1),
                    Some("Test Game"),
                    "REDprelauncher.exe",
                    Some("launcher"),
                    true,
                    false,
                    false,
                    Some(64)
                ),
                (
                    Some(2),
                    Some("Test Game"),
                    "bin/x64/Game64.exe",
                    Some("game"),
                    false,
                    false,
                    true,
                    Some(64)
                ),
                (
                    Some(3),
                    Some("Settings"),
                    "tools/Config.exe",
                    Some("tool"),
                    false,
                    false,
                    false,
                    Some(32)
                ),
                (
                    Some(4),
                    Some("DLC Editor"),
                    "dlc/editor.exe",
                    Some("tool"),
                    false,
                    true,
                    false,
                    Some(32)
                ),
            ]
        );
        assert_eq!(l.exes[1].args, ["-windowed"]);
        assert_eq!(l.exes[2].args, ["-cfg", "my file.ini"]);
        assert_eq!(l.exes[2].working_dir.as_deref(), Some("tools"));
        assert_eq!(l.exes[3].game_id.as_deref(), Some(DLC));
        assert_eq!(l.launcher_exe.as_deref(), Some("bin/x64/Game64.exe"));
        assert_eq!(l.game_dir, w.game().join("game"));

        let text = exes::render(&l);
        for want in [
            format!("Test Game ({ID}): game files in "),
            "play tasks:\n".into(),
            "   1  Test Game: REDprelauncher.exe (64-bit) [launcher, primary]\n".into(),
            "   2  Test Game: bin/x64/Game64.exe (64-bit) [game, gogo's launcher runs this]\n      args: -windowed\n".into(),
            "   3  Settings: tools/Config.exe (32-bit) [tool]\n      args: -cfg my file.ini\n      working dir: tools\n".into(),
            format!("   4  DLC Editor: dlc/editor.exe (32-bit) [tool, DLC {DLC}]\n"),
            format!("run one: gogo linux-app run {ID} <number|name|path>"),
        ] {
            assert!(text.contains(&want), "{want:?} in:\n{text}");
        }
        assert!(!text.contains("Manual"), "{text}");
        assert!(!text.contains("other exes"), "{text}");

        let json = serde_json::to_value(&l).unwrap();
        assert_eq!(json["gogid"], ID);
        assert_eq!(json["launcher_exe"], "bin/x64/Game64.exe");
        assert_eq!(json["exes"][1]["index"], 2);
        assert_eq!(json["exes"][1]["launcher"], true);
        assert_eq!(json["exes"][1]["bits"], 64);
        assert_eq!(json["exes"][1]["args"][0], "-windowed");
        assert_eq!(json["exes"][3]["dlc"], true);
        assert_eq!(json["exes"][3]["game_id"], DLC);
        assert_eq!(json["exes"][2]["working_dir"], "tools");

        // --all: the rest, by path, none of them a task again.
        let l = exes::list(ID, true).unwrap();
        let extra: Vec<(&str, Option<u8>, Option<usize>)> = l.exes[4..]
            .iter()
            .map(|e| (e.path.as_str(), e.bits, e.index))
            .collect();
        assert_eq!(
            extra,
            [
                ("game.exe", Some(32), None),
                ("tools/redmod/bin/redMod.exe", Some(64), None),
                ("unins000.exe", Some(32), None),
            ]
        );
        let text = exes::render(&l);
        assert!(
            text.contains("other exes (not play tasks):\n      game.exe (32-bit)\n      tools/redmod/bin/redMod.exe (64-bit)\n"),
            "{text}"
        );
        assert_eq!(
            serde_json::to_value(&l).unwrap()["exes"][5]["index"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn run_picks_by_number_name_or_path_and_refuses_a_shared_name() {
        let w = World::launcher_built_prep("exes-pick", false, false, &exes_tree);
        let gog = gog(&w);

        let p = exes::plan_run(ID, "2", &[]).unwrap();
        assert_eq!(p.exe, gog.join("bin/x64/Game64.exe"));
        assert_eq!(p.workdir, gog.join("bin/x64"));
        assert_eq!(p.args, ["-windowed"]);
        assert_eq!((p.index, p.label.as_str()), (Some(2), "bin/x64/Game64.exe"));
        assert_eq!(p.launch_sh, w.game().join("launch.sh"));
        assert_eq!(
            argv(&p),
            [
                gogo::linuxapp::launcher::ONE_OFF_ARG.to_string(),
                gog.join("bin/x64/Game64.exe").display().to_string(),
                gog.join("bin/x64").display().to_string(),
                "-windowed".into()
            ]
        );

        // Cyberpunk's two tasks share a name: refused, with how to pick.
        let e = exes::plan_run(ID, "Test Game", &[])
            .unwrap_err()
            .to_string();
        assert!(
            e.contains(
                "2 play tasks are named \"Test Game\": 1 (REDprelauncher.exe), 2 (bin/x64/Game64.exe); \
                 pick one by its number or path"
            ),
            "{e}"
        );

        // A name (any case): the task's working dir and args, then the extra.
        let p = exes::plan_run(ID, "settings", &["--x".into()]).unwrap();
        assert_eq!(p.exe, gog.join("tools/Config.exe"));
        assert_eq!(p.workdir, gog.join("tools"));
        assert_eq!(p.args, ["-cfg", "my file.ini", "--x"]);
        assert_eq!(p.index, Some(3));
        // A DLC's task.
        assert_eq!(
            exes::plan_run(ID, "4", &[]).unwrap().exe,
            gog.join("dlc/editor.exe")
        );

        // A path, Windows-style and in any case: a task's exe is that task...
        let p = exes::plan_run(ID, "REDprelauncher.exe", &[]).unwrap();
        assert_eq!((p.index, p.args.len()), (Some(1), 0));
        assert_eq!(p.workdir, gog);
        let p = exes::plan_run(ID, r"BIN\x64\game64.EXE", &[]).unwrap();
        assert_eq!(
            (p.index, p.args.clone()),
            (Some(2), vec!["-windowed".to_string()])
        );
        // ...and any other exe runs alone, in its dir, with only the extra.
        let p = exes::plan_run(ID, r"tools\redmod\bin\REDMOD.exe", &["-h".into()]).unwrap();
        assert_eq!(p.exe, gog.join("tools/redmod/bin/redMod.exe"));
        assert_eq!(p.workdir, gog.join("tools/redmod/bin"));
        assert_eq!((p.index, p.args.clone()), (None, vec!["-h".to_string()]));
        assert_eq!(p.label, "tools/redmod/bin/redMod.exe");

        for (sel, want) in [
            ("9", "no play task 9"),
            ("Nope", "no play task named \"Nope\""),
            ("nope.exe", "no nope.exe in the game's files"),
            ("../escape.exe", "inside them"),
            ("/bin/true.exe", "inside them"),
        ] {
            let e = exes::plan_run(ID, sel, &[]).unwrap_err().to_string();
            assert!(e.contains(want), "{sel}: {e}");
        }
    }

    #[test]
    fn a_one_off_gets_the_games_environment_and_changes_none_of_its_config() {
        let Some(w) = World::launcher_prep("one-off-run", false, false, &exes_tree) else {
            return;
        };
        // An override with env, so the L4 file is one of the files to keep.
        gogo::linuxapp::envargs::set_env(ID, &[("FOO".into(), "from-l4".into())]).unwrap();
        setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
        let normal_env = w.root.join("normal.env");
        let out = w.launch(&[("FAKE_ENV_DUMP", normal_env.to_str().unwrap())], &[]);
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        let latest = std::fs::read_link(w.logs().join("latest.log")).unwrap();
        let before = config(&w);
        assert!(
            before.iter().filter(|(_, v)| v.is_some()).count() >= 7,
            "{before:?}"
        );
        let calls = w.calls().len();

        let plan = exes::plan_run(ID, "Settings", &["--x".into(), "a b".into()]).unwrap();
        let one_env = w.root.join("one-off.env");
        let args_log = w.root.join("args.log");
        let out = w.launch(
            &[
                ("FAKE_ENV_DUMP", one_env.to_str().unwrap()),
                ("FAKE_ARGS_LOG", args_log.to_str().unwrap()),
            ],
            &argv(&plan).iter().map(String::as_str).collect::<Vec<_>>(),
        );
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        assert!(String::from_utf8_lossy(&out.stdout).contains("fake-game stdout line"));

        // The tool, in its dir, with its args and the extra.
        let gog = gog(&w);
        let log = w.log();
        let last = log.lines().rfind(|l| l.starts_with("ARGV:")).unwrap();
        assert_eq!(
            last,
            format!(
                "ARGV:{} -cfg my file.ini --x a b",
                gog.join("tools/Config.exe").display()
            )
        );
        assert_eq!(
            std::fs::read_to_string(&args_log).unwrap(),
            format!(
                "{}\n-cfg\nmy file.ini\n--x\na b\n",
                gog.join("tools/Config.exe").display()
            )
        );
        let env = log.lines().rfind(|l| l.starts_with("ENV:")).unwrap();
        assert!(
            env.ends_with(&format!(" CWD={}", gog.join("tools").display())),
            "{env}"
        );
        // The game's environment, all of it: gogo's, the L4's, the renderer's.
        let (normal, one) = (env_map(&normal_env), env_map(&one_env));
        assert_eq!(normal, one);
        assert_eq!(one.get("FOO").map(String::as_str), Some("from-l4"));
        assert_eq!(one.get("STORE").map(String::as_str), Some("gog"));

        // Nothing of the game's config written; no setup; the menu entry,
        // latest.log and the game's run logs are the game's.
        assert_eq!(config(&w), before);
        assert_eq!(w.calls().len(), calls, "{:?}", w.calls());
        assert_eq!(
            std::fs::read_link(w.logs().join("latest.log")).unwrap(),
            latest
        );
        assert_eq!(w.runs().len(), 1);
        let ones = one_off_logs(&w);
        assert_eq!(ones.len(), 1);
        for want in [
            format!(
                "# gogo run log: Test Game ({ID})\n# one-off: {}\n",
                gog.join("tools/Config.exe").display()
            ),
            "# args: -cfg my file.ini --x a b\n".into(),
            "fake-game stdout line\n".into(),
            "# exit: 0\n".into(),
        ] {
            assert!(ones[0].contains(&want), "{want:?} in:\n{}", ones[0]);
        }
        // No early-exit dialog or renderer switch for a one-off.
        let ll = w.launch_log();
        assert!(ll.contains("one-off run of "), "{ll}");
        assert!(ll.contains("one-off ended: code 0"), "{ll}");
        assert_eq!(ll.matches("early exit").count(), 1, "{ll}");
        // Nothing left behind.
        assert!(
            std::fs::read_dir(w.game().join(".launchers"))
                .unwrap()
                .next()
                .is_none()
        );
        assert!(!w.game().join(gogo::linuxapp::launcher::ONE_OFFS).exists());

        // The game's own launch still runs the game.
        assert_eq!(w.launch(&[], &[]).status.code(), Some(0));
        let log = w.log();
        let last = log.lines().rfind(|l| l.starts_with("ARGV:")).unwrap();
        assert_eq!(
            last,
            format!(
                "ARGV:{} -windowed",
                gog.join("bin/x64/Game64.exe").display()
            )
        );
    }

    #[test]
    fn a_one_off_is_refused_while_the_game_runs_a_dlc_add_or_a_setup_is_pending() {
        let Some(w) = World::launcher_prep("one-off-refused", false, false, &exes_tree) else {
            return;
        };
        let plan = exes::plan_run(ID, "1", &[]).unwrap();
        let args = argv(&plan);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let runs_of = |w: &World| {
            w.log()
                .lines()
                .filter(|l| l.starts_with("ARGV:") && l.contains("REDprelauncher.exe"))
                .count()
        };

        // The game is running.
        let mut game = w
            .launch_cmd(&[("FAKE_GAME_SLEEP", "8")], &[])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        assert!(wait_until(10, || !prefix_procs(&w).is_empty()));
        let e = exes::plan_run(ID, "1", &[]).unwrap_err().to_string();
        assert!(e.contains(&format!("{ID} is running (launcher")), "{e}");
        // A run that got past gogo's check: the launcher refuses it too.
        let out = w.launch(&[], &args);
        assert_eq!(out.status.code(), Some(127), "{out:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("Test Game is running (launcher "), "{err}");
        assert_eq!(runs_of(&w), 0);
        assert!(game.wait().unwrap().success());
        assert!(one_off_logs(&w)[0].contains("# exit: 127 (game is running)\n"));

        // A DLC add that stopped while laying files.
        let journal = gogo::gog::lay::journal_path(&w.game());
        std::fs::write(&journal, "{}").unwrap();
        let e = exes::plan_run(ID, "1", &[]).unwrap_err().to_string();
        assert!(e.contains("a DLC add of"), "{e}");
        let out = w.launch(&[], &args);
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("A DLC install for Test Game stopped")
        );
        std::fs::remove_file(&journal).unwrap();

        // A changed config that setup can't apply: not run on the old one.
        gogo::linuxapp::envargs::set_env(ID, &[("FOO".into(), "new".into())]).unwrap();
        let conf = w.conf();
        let out = w.launch(&[("FAKE_SETUP_FAIL", "1")], &args);
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("config changed and setup didn't apply it"),
            "{err}"
        );
        assert_eq!(runs_of(&w), 0);
        assert_eq!(w.conf(), conf);
        // Setup that works runs first, as for a launch of the game.
        let out = w.launch(&[], &args);
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        assert!(
            w.calls().iter().any(|c| c.contains("linux-app setup")),
            "{:?}",
            w.calls()
        );
        assert_eq!(runs_of(&w), 1);

        // A game that isn't set up.
        std::fs::rename(w.game().join("launch.conf"), w.root.join("launch.conf")).unwrap();
        let e = exes::plan_run(ID, "1", &[]).unwrap_err().to_string();
        assert!(e.contains("isn't set up"), "{e}");
    }

    #[test]
    fn a_one_off_is_closed_and_torn_down_on_term_and_doctor_names_it_meanwhile() {
        let Some(w) = World::launcher_prep("one-off-term", false, false, &exes_tree) else {
            return;
        };
        let plan = exes::plan_run(ID, "REDprelauncher.exe", &[]).unwrap();
        let args = argv(&plan);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut child = w
            .launch_cmd(&[("FAKE_GAME_HANG", "1")], &args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        assert!(hang_ready(&w), "{:?}", prefix_procs(&w));
        let host = doctor_host(&w, &[("radeon", 2)]);
        let r = doctor(&w, &host);
        let procs = r.findings.iter().find(|f| f.check == "processes").unwrap();
        assert_eq!(
            procs.message,
            format!(
                "a one-off run of {} is running (launcher {})",
                gog(&w).join("REDprelauncher.exe").display(),
                child.id()
            )
        );
        assert!(
            std::process::Command::new("kill")
                .args(["-TERM", &child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        assert!(wait_until(20, || child.try_wait().unwrap().is_some()));
        assert_eq!(child.wait().unwrap().code(), Some(143));
        assert!(prefix_procs(&w).is_empty(), "{:?}", prefix_procs(&w));
        // The close request went to the prefix's wineserver, as for the game.
        assert!(
            w.log().contains("CLOSE:wineboot --end-session"),
            "{}",
            w.log()
        );
        assert!(one_off_logs(&w)[0].contains("# exit: 143 (SIGTERM)\n"));
        assert!(!w.game().join(gogo::linuxapp::launcher::ONE_OFFS).exists());
        let r = doctor(&w, &host);
        let procs = r.findings.iter().find(|f| f.check == "processes").unwrap();
        assert_eq!(procs.message, "no processes left in the prefix");
    }

    #[test]
    fn a_one_off_runs_under_the_games_wrapper() {
        let Some(w) = World::launcher_prep("one-off-wrap", false, false, &exes_tree) else {
            return;
        };
        w.wrapped(WRAP_EXEC, &[]);
        let plan = exes::plan_run(ID, "3", &["--x".into()]).unwrap();
        let args = argv(&plan);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let log = w.wrap_log();
        let out = w.launch(&[("FAKE_WRAP_LOG", log.to_str().unwrap())], &args);
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        let wargv = w.wrap_file(".argv");
        let wargv: Vec<&str> = wargv.lines().collect();
        assert_eq!(
            wargv[1],
            gogo::linuxapp::launcher::RUN_ONE_OFF_ARG,
            "{wargv:?}"
        );
        let gog = gog(&w);
        let last = w.log();
        let last = last
            .lines()
            .rfind(|l| l.starts_with("ARGV:"))
            .unwrap()
            .to_string();
        assert_eq!(
            last,
            format!(
                "ARGV:{} -cfg my file.ini --x",
                gog.join("tools/Config.exe").display()
            )
        );
        let env = w.log();
        let env = env
            .lines()
            .rfind(|l| l.starts_with("ENV:"))
            .unwrap()
            .to_string();
        assert!(env.contains("FOO=from-wrapper"), "{env}");
        assert!(
            env.ends_with(&format!(" CWD={}", gog.join("tools").display())),
            "{env}"
        );
        assert!(one_off_logs(&w)[0].contains("# wrapper: fake-wrap\n"));
    }
}

// — snapshots (design 20 §2.1, §5.1, §7.1, §8, §9.3, §9.6) —

mod snapshots {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use super::{ID, LinuxAppOptions, World, build, build_with, quiet};
    use gogo::linuxapp::convert::{self, ConvertOptions, ConvertStep};
    use gogo::linuxapp::snapshots::{self as snap, Backends, BuildSnapshots, SnapshotOptions};
    use gogo::macapp::apps;
    use gogo::snapshot::backend::{self, SnapshotBackend};
    use gogo::snapshot::fake::Fake;

    fn fake() -> Backends {
        Backends::fixed(Arc::new(Fake::new()))
    }

    fn with(opts: LinuxAppOptions, b: &Backends) -> LinuxAppOptions {
        LinuxAppOptions {
            snapshots: SnapshotOptions::on(b.clone()),
            ..opts
        }
    }

    fn snap_dir(root: &Path, n: u32) -> PathBuf {
        backend::snapshot_dir(root, n).unwrap()
    }

    fn meta_dir(w: &World) -> PathBuf {
        w.home.join("snapshots").join(ID)
    }

    /// This process's start time, as a launcher record holds it.
    fn my_start() -> String {
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
        let rest = stat.rsplit_once(") ").unwrap().1;
        rest.split_whitespace().nth(19).unwrap().to_string()
    }

    #[test]
    fn a_first_build_takes_the_initial_snapshot_after_setup() {
        let w = World::new("snap-first", &["d3d9.dll"]);
        let b = fake();
        let plan = build::plan(&with(w.opts(), &b), None, &mut quiet()).unwrap();
        assert_eq!(plan.snapshots, BuildSnapshots::On);
        assert!(
            plan.snapshots
                .line()
                .contains("the game dir leaves snapshots of the subvolume around it"),
            "{}",
            plan.snapshots.line()
        );
        let out = build::execute(&plan, None, &mut quiet()).unwrap();
        let meta = out.snapshot.expect("the initial snapshot");
        assert_eq!(meta.id, 1);
        assert_eq!(meta.kind, gogo::snapshot::Kind::Initial);
        assert_eq!(meta.label, "after the first build");
        // Taken after setup: the snapshot holds setup's state.toml and the
        // build's content.toml.
        let one = snap_dir(&out.dirs.root, 1);
        for f in [
            "gogo-launch.toml",
            "state.toml",
            "content.toml",
            "launch.conf",
        ] {
            assert!(one.join(f).is_file(), "{f} not in the snapshot");
        }
        assert_eq!(meta.content.runtime, Some(w.state().runtime));
        assert_eq!(meta.content.lane.as_deref(), Some("depot"));
        let settings = std::fs::read_to_string(meta_dir(&w).join("settings.toml")).unwrap();
        assert!(settings.contains("auto = true"), "{settings}");

        // A rebuild keeps the dir and takes no initial snapshot: its own is
        // `before rebuild` (#405).
        let plan = build::plan(&with(w.opts(), &b), None, &mut quiet()).unwrap();
        assert_eq!(plan.snapshots, BuildSnapshots::Kept { on: true });
        let again = build::execute(&plan, None, &mut quiet()).unwrap();
        let m = again.snapshot.expect("before rebuild");
        assert_eq!(
            (m.kind, m.label.as_str()),
            (gogo::snapshot::Kind::Auto, "before rebuild")
        );
        assert_eq!(snap::list(ID, &b, 5, false).unwrap().snapshots.len(), 2);
    }

    #[test]
    fn no_snapshots_builds_a_plain_dir_with_autosnapshot_off() {
        let w = World::new("snap-off", &["d3d9.dll"]);
        let opts = LinuxAppOptions {
            snapshots: SnapshotOptions::off("--no-snapshots"),
            ..w.opts()
        };
        let plan = build::plan(&opts, None, &mut quiet()).unwrap();
        assert_eq!(plan.snapshots.line(), "off (--no-snapshots)");
        let out = build::execute(&plan, None, &mut quiet()).unwrap();
        assert!(out.snapshot.is_none());
        let settings = std::fs::read_to_string(meta_dir(&w).join("settings.toml")).unwrap();
        assert!(settings.contains("auto = false"), "{settings}");
        assert!(!backend::snapshots_dir(&out.dirs.root).unwrap().exists());
        // The fake can snapshot anything; the setting stays what the build said.
        assert!(!snap::list(ID, &fake(), 5, false).unwrap().auto);
    }

    #[test]
    fn an_installer_build_turns_its_staging_dir_into_the_game_dir() {
        let w = World::new("snap-inst", &["d3d9.dll"]);
        let setup_exe = w.root.join("setup_test_game_1.0.exe");
        std::fs::write(&setup_exe, "MZ").unwrap();
        let b = fake();
        let opts = with(
            LinuxAppOptions {
                installer: Some(setup_exe),
                runtime: Some(format!("path:{}", w.build.display())),
                name: Some("Test Game".into()),
                ..Default::default()
            },
            &b,
        );
        let plan = build::plan(&opts, None, &mut quiet()).unwrap();
        assert_eq!(plan.snapshots, BuildSnapshots::On);
        let staging = plan.state.root.clone();
        let out = build::execute(&plan, None, &mut quiet()).unwrap();
        assert!(!staging.exists(), "staging dir left behind");
        assert!(out.dirs.root.join("prefix/drive_c/GOG").is_dir());
        let meta = out.snapshot.expect("the initial snapshot");
        assert_eq!(meta.content.lane.as_deref(), Some("installer"));
        assert!(
            snap_dir(&out.dirs.root, 1)
                .join("gogo-launch.toml")
                .is_file()
        );
    }

    #[test]
    fn list_create_delete_and_auto() {
        let w = World::new("snap-cli", &["d3d9.dll"]);
        let b = fake();
        let root = build::execute(
            &build::plan(&with(w.opts(), &b), None, &mut quiet()).unwrap(),
            None,
            &mut quiet(),
        )
        .unwrap()
        .dirs
        .root;

        let v = snap::list(ID, &b, 5, true).unwrap();
        assert_eq!((v.backend, v.ready, v.auto), (Some("btrfs"), true, true));
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["gogid"], serde_json::json!(ID.parse::<u64>().unwrap()));
        assert_eq!(json["keepAuto"], 5);
        assert_eq!(json["reason"], serde_json::Value::Null);
        let s0 = &json["snapshots"][0];
        assert_eq!(s0["id"], 1);
        assert_eq!(s0["kind"], "initial");
        assert_eq!(s0["state"], "done");
        assert_eq!(s0["reachable"], true);
        assert_eq!(s0["content"]["lane"], "depot");
        assert_eq!(s0["space"]["exact"], false, "measured by `list`: {json}");
        let text = snap::render_list(&v);
        assert!(text.contains("backend:      btrfs subvolume"), "{text}");
        assert!(
            text.contains("autosnapshot: on, keeping 5 automatic"),
            "{text}"
        );
        assert!(text.contains("after the first build"), "{text}");

        // create
        let plan = snap::plan_create(ID, &b, Some("test")).unwrap();
        assert_eq!(plan.label, "test");
        let made = snap::create(ID, &b, Some("test")).unwrap();
        assert_eq!(made.snapshot.id, 2);
        assert_eq!(made.snapshot.kind, Some("manual"));
        assert!(snap_dir(&root, 2).is_dir());
        let json = serde_json::to_value(&made).unwrap();
        assert_eq!(json["snapshot"]["label"], "test");

        // delete: unknown numbers are named, the initial one is called that.
        let plan = snap::plan_delete(ID, &b, &[2, 9]).unwrap();
        assert_eq!(plan.unknown, [9]);
        let plan = snap::plan_delete(ID, &b, &[1]).unwrap();
        assert!(
            plan.question()
                .starts_with("delete snapshot 1 (initial: after the first build)"),
            "{}",
            plan.question()
        );
        let gone = snap::delete(ID, &b, &[2]).unwrap();
        assert_eq!(gone.deleted, [2]);
        assert!(!snap_dir(&root, 2).exists());
        let ids: Vec<u32> = snap::list(ID, &b, 5, false)
            .unwrap()
            .snapshots
            .iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, [1]);

        // auto
        assert!(snap::auto(ID, &b, 5).unwrap().auto);
        let off = snap::set_auto(ID, &b, false, 5).unwrap();
        assert!(!off.auto);
        assert!(!snap::auto(ID, &b, 5).unwrap().auto);
        assert_eq!(
            serde_json::to_value(snap::set_auto(ID, &b, true, 5).unwrap()).unwrap()["auto"],
            true
        );
        // A number is never handed out twice.
        assert_eq!(snap::create(ID, &b, None).unwrap().snapshot.id, 3);
    }

    /// `create` (and `auto on`) refused, with nothing touched.
    fn assert_refused(b: &Backends, root: &Path, why: &str) {
        let before = std::fs::read_dir(backend::snapshots_dir(root).unwrap())
            .unwrap()
            .count();
        for e in [
            snap::plan_create(ID, b, None).map(|_| ()).unwrap_err(),
            snap::create(ID, b, None).map(|_| ()).unwrap_err(),
            snap::delete(ID, b, &[1]).map(|_| ()).unwrap_err(),
            snap::set_auto(ID, b, true, 5).map(|_| ()).unwrap_err(),
        ] {
            assert!(e.to_string().contains(why), "{why}: {e}");
        }
        let after = std::fs::read_dir(backend::snapshots_dir(root).unwrap())
            .unwrap()
            .count();
        assert_eq!(before, after, "{why}: something was touched");
        assert!(snap_dir(root, 1).is_dir(), "{why}: snapshot 1 went");
    }

    #[test]
    fn every_refusal_of_a_snapshot_change() {
        let w = World::new("snap-refuse", &["d3d9.dll"]);
        let b = fake();
        let out = build::execute(
            &build::plan(&with(w.opts_in_library(), &b), None, &mut quiet()).unwrap(),
            None,
            &mut quiet(),
        )
        .unwrap();
        let root = out.dirs.root.clone();

        // The game runs: its launcher, then a one-off of another exe.
        let me = std::process::id().to_string();
        let launchers = root.join(".launchers");
        std::fs::create_dir_all(&launchers).unwrap();
        std::fs::write(launchers.join(&me), format!("{}\n", my_start())).unwrap();
        assert_refused(&b, &root, "is running");
        std::fs::remove_dir_all(&launchers).unwrap();
        let one_offs = root.join(".one-offs");
        std::fs::create_dir_all(&one_offs).unwrap();
        std::fs::write(
            one_offs.join(&me),
            format!("{}\nTools/Config.exe\n", my_start()),
        )
        .unwrap();
        assert_refused(&b, &root, "`gogo linux-app run`");
        std::fs::remove_dir_all(&one_offs).unwrap();

        // A DLC add that isn't finished, each way it shows.
        for (path, dir) in [
            ("dlc-journal.json", false),
            ("dlc-download.json", false),
            (".dlc-staging", true),
        ] {
            let p = root.join(path);
            if dir {
                std::fs::create_dir_all(&p).unwrap();
            } else {
                std::fs::write(&p, "{}").unwrap();
            }
            assert_refused(&b, &root, "DLC add");
            if dir {
                std::fs::remove_dir_all(&p).unwrap();
            } else {
                std::fs::remove_file(&p).unwrap();
            }
        }

        // A move that isn't settled.
        let journal = w.home.join("game-moves").join(ID);
        std::fs::write(&journal, "x\n").unwrap();
        assert_refused(&b, &root, "move");
        std::fs::remove_file(&journal).unwrap();

        // Another gogo holds the game's lock.
        let held = gogo::macapp::relocate::lock_game(ID).unwrap();
        assert!(snap::create(ID, &b, None).is_err());
        drop(held);

        // The game's drive isn't there.
        let lib = root.parent().unwrap().to_path_buf();
        let away = lib.with_file_name("unmounted");
        std::fs::rename(&lib, &away).unwrap();
        let e = snap::create(ID, &b, None).unwrap_err().to_string();
        assert!(e.contains("isn't there"), "{e}");
        let listed = snap::list(ID, &b, 5, true).unwrap();
        assert!(!listed.reachable);
        assert_eq!(listed.snapshots[0].presence, "unreachable");
        std::fs::rename(&away, &lib).unwrap();

        // And with all of that gone, it works.
        assert_eq!(snap::create(ID, &b, None).unwrap().snapshot.id, 2);
    }

    #[test]
    fn a_game_built_without_snapshots_is_refused_and_never_converted() {
        let w = World::new("snap-plain", &["d3d9.dll"]);
        // The default options: a plain dir, as a game built before snapshots.
        let root = build::execute(
            &build::plan(&w.opts(), None, &mut quiet()).unwrap(),
            None,
            &mut quiet(),
        )
        .unwrap()
        .dirs
        .root;
        let real = Backends::real();
        for e in [
            snap::create(ID, &real, None).unwrap_err().to_string(),
            snap::set_auto(ID, &real, true, 5)
                .map(|_| ())
                .unwrap_err()
                .to_string(),
        ] {
            // btrfs: a plain dir that needs converting; anything else: none.
            assert!(
                e.contains("needs converting") || e.starts_with("no snapshots: "),
                "{e}"
            );
        }
        assert!(!backend::snapshots_dir(&root).unwrap().exists());
        // `list` still works, and says why.
        let v = snap::list(ID, &real, 5, true).unwrap();
        assert!(v.snapshots.is_empty() && !v.ready && !v.auto);
        // `auto off` is never refused.
        assert!(!snap::set_auto(ID, &real, false, 5).unwrap().auto);
    }

    #[test]
    fn apps_remove_deletes_the_snapshots_first_and_the_metadata_last() {
        let w = World::new("snap-remove", &["d3d9.dll"]);
        let b = fake();
        let root = build::execute(
            &build::plan(&with(w.opts(), &b), None, &mut quiet()).unwrap(),
            None,
            &mut quiet(),
        )
        .unwrap()
        .dirs
        .root;
        snap::create(ID, &b, Some("second")).unwrap();
        let logs = w.root.join("logs");
        let a = apps::get(ID, &logs).unwrap().unwrap();
        let s = a.snapshots.clone().expect("a snapshots section");
        assert_eq!(s.count, 2);
        assert_eq!(s.newest[0].2, "second");
        assert!(s.remove_note().unwrap().starts_with("2 snapshots"));
        assert!(
            s.lines(ID).iter().any(|l| l.contains("autosnapshot on")),
            "{:?}",
            s.lines(ID)
        );

        let removed = apps::remove_with(&a, false, &b).unwrap();
        assert_eq!(removed.snapshots, 2);
        assert!(!root.exists());
        assert!(!backend::snapshots_dir(&root).unwrap().exists());
        assert!(!meta_dir(&w).exists());
        assert!(removed.paths.contains(&meta_dir(&w)));
    }

    #[test]
    fn the_store_gc_keeps_what_a_snapshot_was_set_up_with() {
        let w = World::new("snap-gc", &["d3d9.dll"]);
        let b = fake();
        build::execute(
            &build::plan(&with(w.opts(), &b), None, &mut quiet()).unwrap(),
            None,
            &mut quiet(),
        )
        .unwrap();
        // The world's runtime is a `path:` one, which the store never holds:
        // point snapshot 1 at a store runtime, as a GE-Proton build's would be.
        let file = meta_dir(&w).join("1.toml");
        let raw = std::fs::read_to_string(&file).unwrap();
        let mut meta: gogo::snapshot::Meta = toml::from_str(&raw).unwrap();
        meta.content.runtime = Some("ge-proton/GE-Proton10-25".into());
        meta.content.renderer = Some("dxvk/v2.7".into());
        std::fs::write(&file, toml::to_string(&meta).unwrap()).unwrap();

        let refs = gogo::macapp::store::references();
        for r in ["ge-proton/GE-Proton10-25", "dxvk/v2.7"] {
            assert!(
                refs.iter()
                    .any(|x| x.gogid == ID && x.reference == r && x.file == file),
                "{r}: {refs:?}"
            );
        }
        assert_eq!(gogo::macapp::store::users_of("ge-proton"), [ID]);
        // `doctor` names a snapshot whose runtime isn't in the store.
        let root = gogo::macapp::launch::GameDirs::for_game(ID).unwrap().root;
        let f = snap::doctor(ID, &root, &b);
        assert!(
            f.iter()
                .any(|f| f.message.contains("needs ge-proton/GE-Proton10-25")),
            "{f:?}"
        );
    }

    #[test]
    fn the_snapshots_dir_is_never_a_game_or_a_library() {
        let w = World::new("snap-skip", &["d3d9.dll"]);
        let b = fake();
        let root = build::execute(
            &build::plan(&with(w.opts(), &b), None, &mut quiet()).unwrap(),
            None,
            &mut quiet(),
        )
        .unwrap()
        .dirs
        .root;
        let snaps = backend::snapshots_dir(&root).unwrap();
        assert!(snaps.is_dir());
        let listed: Vec<String> = apps::list(&w.root.join("logs"))
            .unwrap()
            .into_iter()
            .map(|a| a.gogid)
            .collect();
        assert_eq!(listed, [ID]);
        for dir in [snaps.parent().unwrap().to_path_buf(), snaps.join("x")] {
            let e = gogo::macapp::launch::Library::resolve(&dir, None).unwrap_err();
            assert!(e.to_string().contains(".gogo-snapshots"), "{e}");
        }
        let elsewhere = w.root.join("lib/.gogo-snapshots");
        assert!(gogo::macapp::launch::Library::resolve(&elsewhere, None).is_err());
    }

    #[test]
    fn doctor_reports_the_backend_and_strays() {
        let w = World::new("snap-doctor", &["d3d9.dll"]);
        let b = fake();
        let root = build::execute(
            &build::plan(&with(w.opts(), &b), None, &mut quiet()).unwrap(),
            None,
            &mut quiet(),
        )
        .unwrap()
        .dirs
        .root;
        let f = snap::doctor(ID, &root, &b);
        assert_eq!(f[0].level, gogo::macapp::doctor::Level::Ok, "{f:?}");
        assert!(
            f[0].message.starts_with("btrfs subvolume, autosnapshot on"),
            "{f:?}"
        );
        std::fs::create_dir_all(snap_dir(&root, 7)).unwrap();
        let f = snap::doctor(ID, &root, &b);
        assert!(
            f.iter()
                .any(|f| f.level == gogo::macapp::doctor::Level::Warn
                    && f.message.contains("snapshot 7 is on disk without metadata")),
            "{f:?}"
        );
        // The full doctor runs the check too.
        let report =
            gogo::linuxapp::doctor::doctor(ID, &gogo::linuxapp::doctor::Host::detect()).unwrap();
        assert!(report.findings.iter().any(|f| f.check == "snapshots"));
    }

    /// On a real btrfs ([`World::on_btrfs`], else skipped): the game dir is a
    /// subvolume, the initial snapshot a read-only one beside it, and `apps
    /// remove` takes both. Run as [`super::btrfs_build_makes_a_subvolume_game_dir`]:
    /// a top-level `btrfs_` name, which the `snapshots-btrfs` CI job selects.
    pub(super) fn real_btrfs_build() {
        let Some(w) = World::on_btrfs("btrfs_build_makes_a_subvolume_game_dir", &["d3d9.dll"])
        else {
            return;
        };
        let real = Backends::real();
        let plan = build::plan(&with(w.opts(), &real), None, &mut quiet()).unwrap();
        assert_eq!(plan.snapshots, BuildSnapshots::On);
        let out = build::execute(&plan, None, &mut quiet()).unwrap();
        let root = out.dirs.root.clone();
        let guard = Cleanup(root.clone());
        assert!(gogo::snapshot::btrfs::is_subvolume(&root).unwrap());
        assert_eq!(out.snapshot.as_ref().map(|m| m.id), Some(1));
        let one = snap_dir(&root, 1);
        assert!(gogo::snapshot::btrfs::is_subvolume(&one).unwrap());
        assert!(gogo::snapshot::btrfs::is_readonly(&one).unwrap());
        assert!(one.join("state.toml").is_file());
        assert!(gogo::snapshot::btrfs::Btrfs.ready(&root).unwrap());

        let a = apps::get(ID, &w.root.join("logs")).unwrap().unwrap();
        let removed = apps::remove(&a, false).unwrap();
        assert_eq!(removed.snapshots, 1);
        assert!(!root.exists() && !one.exists());
        drop(guard);
    }

    /// A rebuild with snapshots on, on real btrfs: `before rebuild` is a
    /// read-only snapshot of the game as it was (design 20 §7.1).
    pub(super) fn real_btrfs_rebuild() {
        let Some(w) = World::on_btrfs("btrfs_rebuild_leaves_a_read_only_snapshot", &["d3d9.dll"])
        else {
            return;
        };
        let real = Backends::real();
        let out = build::execute(
            &build::plan(&with(w.opts(), &real), None, &mut quiet()).unwrap(),
            None,
            &mut quiet(),
        )
        .unwrap();
        let root = out.dirs.root.clone();
        let guard = Cleanup(root.clone());
        std::fs::write(root.join("game/old-only.txt"), "old").unwrap();
        let plan = build::plan(&with(w.opts(), &real), None, &mut quiet()).unwrap();
        assert_eq!(plan.snapshots, BuildSnapshots::Kept { on: true });
        let again = build::execute(&plan, None, &mut quiet()).unwrap();
        let m = again.snapshot.expect("before rebuild");
        assert_eq!((m.id, m.label.as_str()), (2, "before rebuild"));
        let two = snap_dir(&root, 2);
        assert!(gogo::snapshot::btrfs::is_subvolume(&two).unwrap());
        assert!(gogo::snapshot::btrfs::is_readonly(&two).unwrap());
        assert!(two.join("game/old-only.txt").is_file());
        assert!(!root.join("game/old-only.txt").exists());
        assert!(gogo::snapshot::btrfs::is_subvolume(&root).unwrap());

        let a = apps::get(ID, &w.root.join("logs")).unwrap().unwrap();
        let removed = apps::remove(&a, false).unwrap();
        assert_eq!(removed.snapshots, 2);
        assert!(!root.exists() && !two.exists());
        drop(guard);
    }

    // — converting a plain dir (design 20 §2.1, #406) —

    /// A fake where only the dirs it made are subvolumes, and the backends
    /// around it.
    fn strict() -> (Arc<Fake>, Backends) {
        let f = Arc::new(Fake::subvolumes_only());
        (f.clone(), Backends::fixed(f))
    }

    fn agreed(opts: ConvertOptions) -> snap::Convert<'static> {
        snap::Convert {
            opts,
            ..Default::default()
        }
    }

    /// What a conversion must keep: a big file with Wine's attribute, odd
    /// modes, a link, a dangling link and a read-only dir.
    pub(super) fn oddities(root: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        let d = root.join("prefix/drive_c/odd");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("big.bin"), vec![7u8; 1 << 20]).unwrap();
        let _ = rustix::fs::lsetxattr(
            d.join("big.bin"),
            "user.wine.sd",
            b"\x01\x00\x04\x80",
            rustix::fs::XattrFlags::empty(),
        );
        std::fs::write(d.join("secret"), "s").unwrap();
        std::fs::set_permissions(d.join("secret"), std::fs::Permissions::from_mode(0o640)).unwrap();
        std::fs::write(d.join("tool.sh"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(d.join("tool.sh"), std::fs::Permissions::from_mode(0o750))
            .unwrap();
        std::os::unix::fs::symlink("big.bin", d.join("link")).unwrap();
        std::os::unix::fs::symlink("/nonexistent/target", d.join("dangling")).unwrap();
        let ro = d.join("ro");
        std::fs::create_dir(&ro).unwrap();
        std::fs::write(ro.join("inside"), "x").unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
    }

    /// Every dir under `dir` writable again, so the world's cleanup can go.
    pub(super) fn unlock(dir: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        let Ok(m) = std::fs::symlink_metadata(dir) else {
            return;
        };
        if !m.is_dir() {
            return;
        }
        let _ = std::fs::set_permissions(
            dir,
            std::fs::Permissions::from_mode(m.permissions().mode() | 0o700),
        );
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            unlock(&e.path());
        }
    }

    /// Every entry below `root` with its kind, mode, size, mtime, link
    /// target and `user.wine.sd`, sorted: what a conversion must keep. The
    /// root itself isn't listed (the marker comes and goes in it).
    pub(super) fn tree_of(root: &Path) -> Vec<String> {
        use std::os::unix::fs::MetadataExt as _;
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap() {
                let p = e.unwrap().path();
                let m = std::fs::symlink_metadata(&p).unwrap();
                let rel = p.strip_prefix(root).unwrap().display().to_string();
                let link = std::fs::read_link(&p)
                    .map(|t| t.display().to_string())
                    .unwrap_or_default();
                let mut sd = [0u8; 64];
                let sd = rustix::fs::lgetxattr(&p, "user.wine.sd", &mut sd[..])
                    .map(|n| sd[..n].to_vec())
                    .unwrap_or_default();
                let kind = if m.is_symlink() {
                    "l"
                } else if m.is_dir() {
                    "d"
                } else {
                    "f"
                };
                let size = if m.is_dir() { 0 } else { m.len() };
                out.push(format!(
                    "{rel} {kind} {:o} {size} {} {link} {sd:?}",
                    m.mode() & 0o7777,
                    if m.is_symlink() { 0 } else { m.mtime() }
                ));
                if m.is_dir() {
                    stack.push(p);
                }
            }
        }
        out.sort();
        out
    }

    fn assert_settled(w: &World, root: &Path) {
        assert!(
            convert::pending(ID, root).is_empty(),
            "{:?}",
            convert::pending(ID, root)
        );
        assert!(!root.join(convert::MARKER).exists());
        assert!(!convert::staging_of(root, ID).unwrap().exists());
        assert!(!meta_dir(w).join("convert.json").exists());
    }

    #[test]
    fn a_plain_dir_is_converted_then_snapshotted_and_still_launches() {
        let Some(w) = World::launcher("snap-convert", false) else {
            return;
        };
        let root = w.game();
        oddities(&root);
        let before = tree_of(&root);
        let (f, b) = strict();

        // Never without the user's yes (D8).
        let e = snap::create(ID, &b, None).unwrap_err().to_string();
        assert!(e.contains("needs converting"), "{e}");
        assert!(snap::set_auto(ID, &b, true, 5).is_err());
        assert!(!f.ready(&root).unwrap());

        let plan = snap::plan_create(ID, &b, Some("first")).unwrap();
        let conv = plan.convert.as_ref().expect("a conversion in the plan");
        assert_eq!(conv.staging, convert::staging_of(&root, ID).unwrap());
        let text = plan.lines().join("\n");
        assert!(text.contains("becomes a btrfs subvolume first"), "{text}");
        assert!(
            text.contains("drops out of snapshots of the subvolume around it"),
            "{text}"
        );
        assert!(
            text.contains("costs almost no space") || text.contains("a full copy"),
            "{text}"
        );
        let json = serde_json::to_value(&plan).unwrap();
        assert!(
            json["convert"]["bytes"].as_u64().unwrap() > 1 << 20,
            "{json}"
        );

        let seen = std::cell::RefCell::new(Vec::new());
        let progress = |p| seen.borrow_mut().push(p);
        let out = snap::create_with(
            ID,
            &b,
            Some("first"),
            Some(&snap::Convert {
                progress: &progress,
                ..Default::default()
            }),
        )
        .unwrap();
        assert!(out.converted);
        assert_eq!(out.snapshot.id, 1);
        assert!(f.ready(&root).unwrap(), "the game dir is the new subvolume");
        assert_settled(&w, &root);
        assert_eq!(tree_of(&root), before);
        assert!(matches!(
            seen.borrow().last(),
            Some(snap::ConvertProgress::Verifying { .. })
        ));
        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(json["converted"], true, "{json}");
        assert!(json["conversion"]["seconds"].as_f64().is_some(), "{json}");
        // The snapshot is of the converted dir.
        assert!(
            snap_dir(&root, 1)
                .join("prefix/drive_c/odd/big.bin")
                .is_file()
        );

        // A second create converts nothing.
        let again = snap::create(ID, &b, None).unwrap();
        assert!(!again.converted && again.conversion.is_none());

        let runs = w.game_runs();
        let launched = w.launch(&[], &[]);
        assert_eq!(launched.status.code(), Some(0), "{launched:?}");
        assert_eq!(w.game_runs(), runs + 1);
        unlock(&w.root);
    }

    #[test]
    fn auto_on_converts_and_takes_no_snapshot() {
        let w = World::new("snap-convert-auto", &["d3d9.dll"]);
        let root = build_with(&w.opts()).dirs.root;
        let (f, b) = strict();
        let plan = snap::plan_auto(ID, &b, true).unwrap();
        assert!(plan.convert.is_some());
        assert!(plan.lines().last().unwrap().contains("autosnapshot on"));
        assert!(snap::plan_auto(ID, &b, false).unwrap().convert.is_none());
        let out =
            snap::set_auto_with(ID, &b, true, 5, Some(&agreed(ConvertOptions::default()))).unwrap();
        assert!(out.converted && out.auto);
        assert!(f.ready(&root).unwrap());
        let settings = std::fs::read_to_string(meta_dir(&w).join("settings.toml")).unwrap();
        assert!(settings.contains("auto = true"), "{settings}");
        assert!(snap::list(ID, &b, 5, false).unwrap().snapshots.is_empty());
        assert_settled(&w, &root);
    }

    #[test]
    fn a_conversion_stopped_at_any_step_leaves_the_game_old_or_new() {
        use ConvertStep::*;
        let Some(w) = World::launcher("snap-convert-crash", false) else {
            return;
        };
        let root = w.game();
        oddities(&root);
        let steps = [
            Journal,
            Subvolume,
            Copied,
            Verified,
            Marked,
            Exchanged,
            MarkerRemoved,
            OldTreeDeleted,
        ];
        for (i, step) in steps.into_iter().enumerate() {
            let before = tree_of(&root);
            let (f, b) = strict();
            let conv = agreed(ConvertOptions {
                fail_at: Some(step),
                ..Default::default()
            });
            let e = snap::create_with(ID, &b, None, Some(&conv)).unwrap_err();
            assert!(e.to_string().contains("injected stop"), "{step:?}: {e}");
            let p = convert::pending(ID, &root);
            assert!(p.journal.is_some(), "{step:?}: {p:?}");
            let committed = matches!(step, Exchanged | MarkerRemoved | OldTreeDeleted);
            assert_eq!(p.marker.is_some(), step == Exchanged, "{step:?}: {p:?}");
            if step == Exchanged {
                // The launcher (v15+) refuses while the conversion's own marker
                // is in the game dir.
                assert_eq!(convert::MARKER, backend::CONVERTED_MARKER);
                let runs = w.game_runs();
                let refused = w.launch(&[], &[]);
                assert_eq!(refused.status.code(), Some(127), "{refused:?}");
                assert_eq!(w.game_runs(), runs);
                assert!(
                    w.launch_log()
                        .contains("a conversion for snapshots isn't finished: not launching"),
                    "{}",
                    w.launch_log()
                );
            }
            // `doctor` fails, naming the command that settles it.
            let found = snap::doctor(ID, &root, &b);
            let fail = found
                .iter()
                .find(|f| f.level == gogo::macapp::doctor::Level::Fail)
                .unwrap_or_else(|| panic!("{step:?}: {found:?}"));
            assert!(
                fail.hint
                    .as_deref()
                    .unwrap()
                    .contains(&format!("snapshot {ID} list")),
                "{fail:?}"
            );
            // Settled by the next `snapshot` command, or directly.
            if i % 2 == 0 {
                let v = snap::list(ID, &b, 5, false).unwrap();
                assert!(
                    v.notes.iter().any(|n| n.contains("interrupted conversion")),
                    "{step:?}: {:?}",
                    v.notes
                );
            } else {
                let lines = convert::settle(ID).unwrap();
                let word = if step == Exchanged { "finished" } else { "" };
                assert!(lines[0].contains(word), "{lines:?}");
            }
            assert_settled(&w, &root);
            assert_eq!(f.ready(&root).unwrap(), committed, "{step:?}");
            assert_eq!(tree_of(&root), before, "{step:?}");
            assert!(snap::list(ID, &b, 5, false).unwrap().snapshots.is_empty());
            let runs = w.game_runs();
            let launched = w.launch(&[], &[]);
            assert_eq!(launched.status.code(), Some(0), "{step:?}: {launched:?}");
            assert_eq!(w.game_runs(), runs + 1, "{step:?}");
        }
        // And a rerun converts.
        let (f, b) = strict();
        let out =
            snap::create_with(ID, &b, None, Some(&agreed(ConvertOptions::default()))).unwrap();
        assert!(out.converted && f.ready(&root).unwrap());
        unlock(&w.root);
    }

    #[test]
    fn a_conversion_is_refused_or_undone_and_the_game_kept() {
        let w = World::new("snap-convert-refuse", &["d3d9.dll"]);
        let root = build_with(&w.opts()).dirs.root;
        oddities(&root);
        let before = tree_of(&root);
        let (f, b) = strict();

        // A copy that wouldn't fit is refused in the plan, as `move`'s is.
        let tight = ConvertOptions {
            assume_copy: true,
            free_bytes: Some(1024),
            ..Default::default()
        };
        let e = snap::plan_create_with(ID, &b, None, &tight).unwrap_err();
        assert!(e.to_string().contains("needs a full copy"), "{e}");
        let e = snap::create_with(ID, &b, None, Some(&agreed(tight))).unwrap_err();
        assert!(e.to_string().contains("needs a full copy"), "{e}");
        let roomy = ConvertOptions {
            assume_copy: true,
            free_bytes: Some(u64::MAX),
            ..Default::default()
        };
        let plan = snap::plan_create_with(ID, &b, None, &roomy).unwrap();
        assert!(plan.lines().join("\n").contains("a full copy, which needs"));

        // A copy that doesn't match is deleted, and the game stays as it was.
        let bad = ConvertOptions {
            corrupt_copy: true,
            ..Default::default()
        };
        let e = snap::create_with(ID, &b, None, Some(&agreed(bad))).unwrap_err();
        assert!(e.to_string().contains("doesn't match"), "{e}");
        assert!(e.to_string().contains("another mode"), "{e}");
        assert_settled(&w, &root);
        assert!(!f.ready(&root).unwrap());
        assert_eq!(tree_of(&root), before);

        // The game running: refused before anything.
        let me = std::process::id().to_string();
        let launchers = root.join(".launchers");
        std::fs::create_dir_all(&launchers).unwrap();
        std::fs::write(launchers.join(&me), format!("{}\n", my_start())).unwrap();
        let e = snap::plan_create(ID, &b, None).unwrap_err();
        assert!(e.to_string().contains("is running"), "{e}");
        let e =
            snap::set_auto_with(ID, &b, true, 5, Some(&agreed(Default::default()))).unwrap_err();
        assert!(e.to_string().contains("is running"), "{e}");
        std::fs::remove_dir_all(&launchers).unwrap();
        assert!(convert::pending(ID, &root).is_empty());

        // A symlinked game dir isn't converted.
        let real = w.root.join("elsewhere");
        std::fs::rename(&root, &real).unwrap();
        std::os::unix::fs::symlink(&real, &root).unwrap();
        let e = snap::plan_create(ID, &b, None).unwrap_err();
        assert!(e.to_string().contains("is a symlink"), "{e}");
        std::fs::remove_file(&root).unwrap();
        std::fs::rename(&real, &root).unwrap();
        unlock(&w.root);
    }

    #[test]
    fn the_plan_refuses_what_the_copy_would_walk_into_or_lose() {
        let w = World::new("snap-convert-scan", &["d3d9.dll"]);
        let root = build_with(&w.opts()).dirs.root;
        let (f, b) = strict();
        let refused = |opts: ConvertOptions, why: &str| {
            let e = snap::plan_create_with(ID, &b, None, &opts)
                .unwrap_err()
                .to_string();
            assert!(e.contains(why), "{why}: {e}");
            assert!(convert::pending(ID, &root).is_empty());
        };
        // Another filesystem or a nested subvolume inside: refused, named.
        let nested = root.join("prefix/drive_c/mnt");
        std::fs::create_dir_all(&nested).unwrap();
        refused(
            ConvertOptions {
                other_device_at: Some(nested.clone()),
                ..Default::default()
            },
            &format!("{} is on another filesystem", nested.display()),
        );
        // ... under the locks too.
        let e = snap::create_with(
            ID,
            &b,
            None,
            Some(&agreed(ConvertOptions {
                other_device_at: Some(nested.clone()),
                ..Default::default()
            })),
        )
        .unwrap_err();
        assert!(e.to_string().contains("on another filesystem"), "{e}");
        assert!(!convert::staging_of(&root, ID).unwrap().exists());
        // A hard link.
        let a = root.join("prefix/drive_c/a.dat");
        std::fs::write(&a, "a").unwrap();
        std::fs::hard_link(&a, root.join("prefix/drive_c/b.dat")).unwrap();
        refused(ConvertOptions::default(), "a hard link (2 names)");
        std::fs::remove_file(root.join("prefix/drive_c/b.dat")).unwrap();
        // An ACL (`system.posix_acl_access`), where the filesystem takes one:
        // user::rw-, user:<me>:r--, group::r--, mask::r--, other::r--.
        let uid = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(&root).unwrap());
        let mut acl = 2u32.to_le_bytes().to_vec();
        for (tag, perm, id) in [
            (1u16, 6u16, u32::MAX),
            (2, 4, uid),
            (4, 4, u32::MAX),
            (0x10, 4, u32::MAX),
            (0x20, 4, u32::MAX),
        ] {
            acl.extend(tag.to_le_bytes());
            acl.extend(perm.to_le_bytes());
            acl.extend(id.to_le_bytes());
        }
        if rustix::fs::lsetxattr(
            &a,
            "system.posix_acl_access",
            &acl,
            rustix::fs::XattrFlags::empty(),
        )
        .is_ok()
        {
            refused(ConvertOptions::default(), "system.posix_acl_access");
        }
        std::fs::remove_file(&a).unwrap();
        // A `chattr` flag (+A, which an owner may set), where it is taken.
        let flagged = root.join("prefix/drive_c/noatime.dat");
        std::fs::write(&flagged, "x").unwrap();
        let fd = std::fs::File::open(&flagged).unwrap();
        if rustix::fs::ioctl_setflags(&fd, rustix::fs::IFlags::NOATIME).is_ok() {
            refused(ConvertOptions::default(), "chattr +A");
        }
        drop(fd);
        std::fs::remove_file(&flagged).unwrap();
        // A FIFO is named in the plan, before anything, and isn't copied.
        let fifo = root.join("prefix/drive_c/pipe");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        let plan = snap::plan_create(ID, &b, None).unwrap();
        let text = plan.lines().join("\n");
        assert!(
            text.contains(&format!(
                "1 socket(s) or FIFO(s) aren't copied and go with the old dir: {}",
                fifo.display()
            )),
            "{text}"
        );
        let out =
            snap::create_with(ID, &b, None, Some(&agreed(ConvertOptions::default()))).unwrap();
        assert!(out.converted && f.ready(&root).unwrap());
        assert!(fifo.symlink_metadata().is_err());
        assert_settled(&w, &root);
    }

    #[test]
    fn byte_copies_stop_before_the_free_space_runs_out() {
        let w = World::new("snap-convert-budget", &["d3d9.dll"]);
        let root = build_with(&w.opts()).dirs.root;
        let before = tree_of(&root);
        let (f, b) = strict();
        // The plan expects reflinks; the copy gets none, and 256 MiB free is
        // all margin.
        let conv = agreed(ConvertOptions {
            assume_reflink: true,
            no_reflink: true,
            free_bytes: Some(256 << 20),
            ..Default::default()
        });
        let e = snap::create_with(ID, &b, None, Some(&conv)).unwrap_err();
        assert!(e.to_string().contains("there is room for"), "{e}");
        assert!(e.to_string().contains("the game wasn't touched"), "{e}");
        assert_settled(&w, &root);
        assert!(!f.ready(&root).unwrap());
        assert_eq!(tree_of(&root), before);
    }

    #[test]
    fn a_staging_dir_gogo_cant_tell_is_its_own_is_left_alone() {
        let w = World::new("snap-convert-foreign", &["d3d9.dll"]);
        let root = build_with(&w.opts()).dirs.root;
        let (_, b) = strict();
        let staging = convert::staging_of(&root, ID).unwrap();
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(staging.join("mine.txt"), "someone's").unwrap();
        let p = convert::pending(ID, &root);
        assert!(p.foreign && p.only_foreign(), "{p:?}");
        // Settling leaves it.
        assert!(convert::settle(ID).unwrap().is_empty());
        snap::list(ID, &b, 5, false).unwrap();
        assert!(staging.join("mine.txt").is_file());
        // `doctor` says so, and a conversion is refused while it is there.
        let found = snap::doctor(ID, &root, &b);
        assert!(
            found
                .iter()
                .any(|f| f.level == gogo::macapp::doctor::Level::Fail
                    && f.message.contains("nothing says gogo made it")),
            "{found:?}"
        );
        let e = snap::plan_create(ID, &b, None).unwrap_err();
        assert!(e.to_string().contains("is in the way"), "{e}");
        // A journal of its own makes it gogo's: then it goes.
        let conv = agreed(ConvertOptions {
            fail_at: Some(ConvertStep::Copied),
            ..Default::default()
        });
        std::fs::remove_dir_all(&staging).unwrap();
        assert!(snap::create_with(ID, &b, None, Some(&conv)).is_err());
        assert!(!convert::pending(ID, &root).foreign);
        convert::settle(ID).unwrap();
        assert_settled(&w, &root);
    }

    #[test]
    fn other_commands_settle_a_conversion_and_never_start_one() {
        let w = World::new("snap-convert-settle", &["d3d9.dll"]);
        let root = build_with(&w.opts()).dirs.root;
        let stopped = |b: &Backends, at| {
            let conv = agreed(ConvertOptions {
                fail_at: Some(at),
                ..Default::default()
            });
            assert!(snap::create_with(ID, b, None, Some(&conv)).is_err());
            assert!(!convert::pending(ID, &root).is_empty());
        };
        // A rebuild settles it, and converts nothing (D8).
        let (f, b) = strict();
        stopped(&b, ConvertStep::Copied);
        build_with(&w.opts());
        assert_settled(&w, &root);
        assert!(!f.ready(&root).unwrap());
        // So does a move, which then moves the converted game.
        let (_, b) = strict();
        stopped(&b, ConvertStep::Exchanged);
        let lib = w.library();
        let out = gogo::linuxapp::relocate::move_game(
            ID,
            &gogo::macapp::relocate::Target::Library(lib),
            &Default::default(),
            &mut |_| {},
        )
        .unwrap();
        assert!(
            out.recovered[0].contains("interrupted conversion"),
            "{:?}",
            out.recovered
        );
        assert!(!convert::staging_of(&root, ID).unwrap().exists());
        let moved = w.game();
        assert!(convert::pending(ID, &moved).is_empty());
        // And `apps remove` takes what one left beside the game.
        let (_, b) = strict();
        let conv = agreed(ConvertOptions {
            fail_at: Some(ConvertStep::Verified),
            ..Default::default()
        });
        assert!(snap::create_with(ID, &b, None, Some(&conv)).is_err());
        let staging = convert::staging_of(&moved, ID).unwrap();
        assert!(staging.is_dir());
        let a = apps::get(ID, &w.root.join("logs")).unwrap().unwrap();
        apps::remove_with(&a, false, &b).unwrap();
        assert!(!staging.exists() && !moved.exists());
    }

    /// On a real btrfs ([`World::on_btrfs`], else skipped): a plain game dir
    /// with links, Wine's attribute, odd modes and a read-only dir becomes a
    /// subvolume (inode 256) with the same tree, and its files share their
    /// extents with what they were copied from. Run as
    /// [`super::btrfs_convert_makes_a_plain_game_a_subvolume`].
    pub(super) fn real_btrfs_convert() {
        let Some(w) = World::on_btrfs(
            "btrfs_convert_makes_a_plain_game_a_subvolume",
            &["d3d9.dll"],
        ) else {
            return;
        };
        let root = build_with(&w.opts()).dirs.root;
        let guard = Cleanup(root.clone());
        assert!(!gogo::snapshot::btrfs::is_subvolume(&root).unwrap());
        oddities(&root);
        let big = root.join("prefix/drive_c/odd/big.bin");
        rustix::fs::lsetxattr(
            &big,
            "user.wine.sd",
            b"\x01\x00\x04\x80",
            rustix::fs::XattrFlags::empty(),
        )
        .expect("btrfs takes user.* attributes");
        // A reflinked reference of the original files, outside the game: the
        // converted game must share its extents.
        let reference = w.root.join("reference");
        gogo::gog::materialize::materialize_tree(
            &root,
            &reference,
            gogo::gog::materialize::Reflink::Try,
        )
        .unwrap();
        let real = Backends::real();
        // A nested subvolume inside is refused, named: the copy would walk
        // into it, and deleting the old dir would empty it.
        let nested = root.join("prefix/drive_c/nested");
        gogo::snapshot::btrfs::create_subvolume(&nested).unwrap();
        std::fs::write(nested.join("keep.txt"), "kept").unwrap();
        let e = snap::plan_create(ID, &real, None).unwrap_err().to_string();
        assert!(
            e.contains(&format!("{} is on another filesystem", nested.display())),
            "{e}"
        );
        let e = snap::create_with(ID, &real, None, Some(&agreed(Default::default())))
            .unwrap_err()
            .to_string();
        assert!(e.contains("is a subvolume of its own"), "{e}");
        assert!(nested.join("keep.txt").is_file());
        assert!(!gogo::snapshot::btrfs::is_subvolume(&root).unwrap());
        std::fs::remove_file(nested.join("keep.txt")).unwrap();
        gogo::snapshot::btrfs::destroy_by_rmdir(&nested).unwrap();

        let before = tree_of(&root);
        let plan = snap::plan_create(ID, &real, Some("first")).unwrap();
        let conv = plan.convert.as_ref().expect("a plain dir needs converting");
        assert_eq!(
            conv.method,
            gogo::gog::materialize::TreeMethod::Reflink,
            "one btrfs filesystem: FICLONE works"
        );
        assert_eq!(conv.extra_bytes, 0);
        let out =
            snap::create_with(ID, &real, Some("first"), Some(&agreed(Default::default()))).unwrap();
        let c = out.conversion.expect("converted");
        assert!(c.reflinked_files > 0 && c.copied_files == 0, "{c:?}");
        assert!(gogo::snapshot::btrfs::is_subvolume(&root).unwrap());
        {
            use std::os::unix::fs::MetadataExt as _;
            assert_eq!(std::fs::metadata(&root).unwrap().ino(), 256);
        }
        assert!(gogo::snapshot::btrfs::Btrfs.ready(&root).unwrap());
        assert_settled(&w, &root);
        assert_eq!(tree_of(&root), before);
        assert!(
            snap_dir(&root, 1)
                .join("prefix/drive_c/odd/ro/inside")
                .is_file()
        );

        // The same physical extents as the reference: nothing was written.
        let ext = |p: &Path| {
            gogo::snapshot::btrfs::extents(&std::fs::File::open(p).unwrap())
                .unwrap()
                .iter()
                .map(|e| (e.physical, e.length))
                .collect::<Vec<_>>()
        };
        let shared = ext(&big);
        assert!(!shared.is_empty());
        assert_eq!(shared, ext(&reference.join("prefix/drive_c/odd/big.bin")));
        let exclusive =
            gogo::snapshot::btrfs::estimate_exclusive(&root, std::slice::from_ref(&reference))
                .unwrap();
        assert!(
            exclusive[0] < 64 * 1024,
            "the reference holds {} bytes the converted game doesn't share",
            exclusive[0]
        );
        println!(
            "converted in {:.3} s: {} files reflinked; reference exclusive {} bytes",
            c.seconds, c.reflinked_files, exclusive[0]
        );

        let a = apps::get(ID, &w.root.join("logs")).unwrap().unwrap();
        apps::remove(&a, false).unwrap();
        assert!(!root.exists());
        unlock(&w.root);
        drop(guard);
    }

    /// Whatever a failed btrfs test left: read-only snapshots aren't removed
    /// by the world's cleanup.
    struct Cleanup(PathBuf);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Some(dir) = backend::snapshots_dir(&self.0)
                && let Ok(rd) = std::fs::read_dir(&dir)
            {
                for e in rd.flatten() {
                    let _ = gogo::snapshot::btrfs::destroy_by_rmdir(&e.path());
                }
            }
        }
    }
}

#[test]
fn btrfs_build_makes_a_subvolume_game_dir() {
    snapshots::real_btrfs_build();
}

#[test]
fn btrfs_rebuild_leaves_a_read_only_before_rebuild() {
    snapshots::real_btrfs_rebuild();
}

/// `snapshot <gogid> rollback <n>` (design 20 §5.3.1, §5.4, §6) with the fake
/// backend, the real `launch.sh` and the fake umu: the rollback, a crash at
/// every step and its recovery, the refusals, the override, `rewrite_inside`
/// and the launcher's refusal on the markers.
mod rollback {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use super::{ID, World, build, quiet};
    use gogo::linuxapp::rollback::{self as rb, Planned, RbStep, RollbackOptions, RollbackPlan};
    use gogo::linuxapp::snapshots::{self as snap, Backends, SnapshotOptions};
    use gogo::snapshot::backend;
    use gogo::snapshot::fake::{Fake, FakeStep};

    const SAVES: &str = "prefix/drive_c/users/steamuser/Saved Games/Studio";

    fn put(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn read(root: &Path, rel: &str) -> Option<String> {
        std::fs::read_to_string(root.join(rel)).ok()
    }

    fn l4(w: &World) -> PathBuf {
        w.home.join("compat").join(format!("{ID}.toml"))
    }

    fn plan(b: &Backends, n: u32, opts: &RollbackOptions) -> RollbackPlan {
        match rb::plan(ID, b, n, opts).unwrap() {
            Planned::Plan(p) => *p,
            Planned::Unknown(why) => panic!("{why}"),
        }
    }

    fn go(b: &Backends, n: u32, opts: &RollbackOptions) -> Result<rb::RolledBack, String> {
        let p = plan(b, n, opts);
        rb::rollback(ID, b, n, &p, opts).map_err(|e| e.to_string())
    }

    /// Nothing of a rollback left: no journal, no marker, no staging dir.
    fn assert_settled(w: &World, root: &Path) {
        assert!(
            !w.home
                .join("snapshots")
                .join(ID)
                .join("rollback.json")
                .exists()
        );
        assert!(!root.join(backend::ROLLBACK_MARKER).exists());
        assert!(!backend::rollback_staging(root).unwrap().exists());
    }

    /// The game launches to its game step and exits 0.
    fn assert_runs(w: &World) {
        let before = w.game_runs();
        let out = w.launch(&[], &[]);
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        assert_eq!(w.game_runs(), before + 1);
    }

    /// The state of the world before a rollback: a save and a setting at
    /// snapshot time, then more saves, a rewritten setting and a new override.
    fn dress(w: &World, b: &Backends) -> (PathBuf, u32) {
        let root = w.game();
        put(&root, &format!("{SAVES}/old.sav"), "old");
        put(&root, "game/SAVE/slot0.sav", "slot0");
        put(&root, "game/era.txt", "then");
        gogo::linuxapp::envargs::set_env(ID, &[("FOO".into(), "then".into())]).unwrap();
        // The launcher's config matches the override: setup ran for it.
        gogo::linuxapp::setup::setup(ID, &Default::default(), &mut quiet()).unwrap();
        let n = snap::create(ID, b, Some("before the change"))
            .unwrap()
            .snapshot
            .id;
        // A second apart, so mtimes are after the snapshot's `created`.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        put(&root, &format!("{SAVES}/new.sav"), "new");
        put(&root, "game/SAVE/slot1.sav", "slot1");
        put(&root, "game/era.txt", "now");
        gogo::linuxapp::envargs::set_env(ID, &[("FOO".into(), "now".into())]).unwrap();
        gogo::linuxapp::setup::setup(ID, &Default::default(), &mut quiet()).unwrap();
        (root, n)
    }

    fn fake() -> (Arc<Fake>, Backends) {
        let f = Arc::new(Fake::new());
        let b = Backends::fixed(f.clone());
        (f, b)
    }

    fn opts() -> RollbackOptions {
        RollbackOptions {
            keep_auto: 5,
            ..RollbackOptions::default()
        }
    }

    #[test]
    fn retention_after_a_rollback_spares_its_target_and_counts_its_pre_snapshot() {
        let w = World::new("rb-retain", &["d3d9.dll"]);
        let (_, b) = fake();
        let o = |keep_auto| super::LinuxAppOptions {
            snapshots: SnapshotOptions {
                keep_auto,
                ..SnapshotOptions::on(b.clone())
            },
            ..w.opts()
        };
        let run = |keep_auto| {
            let plan = build::plan(&o(keep_auto), None, &mut quiet()).unwrap();
            build::execute(&plan, None, &mut quiet()).unwrap()
        };
        run(5); // 1: initial
        run(5); // 2: before rebuild
        run(5); // 3: before rebuild
        snap::create(ID, &b, Some("mine")).unwrap(); // 4: manual
        let out = go(
            &b,
            2,
            &RollbackOptions {
                keep_auto: 1,
                ..opts()
            },
        )
        .unwrap();
        // The pre-rollback snapshot (5) is the one `keep_auto = 1` keeps; the
        // target (2) is spared; 3 goes; initial and manual stay.
        assert_eq!(out.pre, 5);
        assert_eq!(out.pruned, [3]);
        let mut left: Vec<u32> = snap::list(ID, &b, 1, false)
            .unwrap()
            .snapshots
            .iter()
            .map(|s| s.id)
            .collect();
        left.sort();
        assert_eq!(left, [1, 2, 4, 5]);
    }

    #[test]
    fn a_rollback_restores_the_game_its_override_and_sets_the_saves_aside() {
        let Some(w) = World::launcher("rb-full", false) else {
            return;
        };
        let (_, b) = fake();
        let (root, n) = dress(&w, &b);
        let l4_then = std::fs::read_to_string(
            w.home
                .join("snapshots")
                .join(ID)
                .join(format!("{n}.override.toml")),
        )
        .unwrap();

        let p = plan(&b, n, &opts());
        let saves: Vec<&str> = p.saves.files.iter().map(|f| f.path.as_str()).collect();
        assert!(
            saves.contains(&"users/steamuser/Saved Games/Studio/new.sav"),
            "{saves:?}"
        );
        assert!(
            !saves.contains(&"users/steamuser/Saved Games/Studio/old.sav"),
            "{saves:?}"
        );
        // No file list for a game built from a plain tree: changed files only,
        // and the build is the snapshot's, so new files count too.
        assert!(saves.contains(&"game/SAVE/slot1.sav"), "{saves:?}");
        assert!(saves.contains(&"game/era.txt"), "{saves:?}");
        assert_eq!(p.pre, n + 1);
        let lines = p.lines(false).join("\n");
        assert!(
            lines.contains(
                "game files: the same build and DLC set as the snapshot: every changed file counts"
            ),
            "{lines}"
        );
        assert!(
            lines.contains("override: restored to the snapshot's copy:"),
            "{lines}"
        );
        assert!(lines.contains("FOO"), "{lines}");
        assert!(lines.contains("Saved Games/Studio: 1 file"), "{lines}");
        assert!(
            !p.lines(false).iter().any(|l| l.contains("slot1.sav")),
            "files only with -v"
        );
        assert!(
            p.lines(true)
                .iter()
                .any(|l| l.contains("game/SAVE/slot1.sav"))
        );
        // The question, as design 20 §6.2 words it.
        let q = p.question();
        assert!(
            q.starts_with(&format!(
                "roll back to {n} (before the change)? {} newer save files will be dropped \
                 from the game (copied to {}",
                p.saves.files.len(),
                w.home.join("saves-aside").join(ID).display()
            )),
            "{q}"
        );
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(json["to"]["id"], n);
        assert!(json["saves"]["files"][0]["mtime"].is_string(), "{json}");

        let out = rb::rollback(ID, &b, n, &p, &opts()).unwrap();
        assert_eq!((out.to, out.pre), (n, n + 1));
        let aside = out.saves_copied_to.clone().expect("copied aside");
        assert_eq!(aside, p.saves_aside);
        assert_eq!(
            out.last_line(),
            format!(
                "rolled back to {n}; the state before is snapshot {}; newer saves copied to {}",
                n + 1,
                aside.display()
            )
        );
        let json = serde_json::to_value(&out).unwrap();
        for k in [
            "to",
            "pre",
            "savesNewer",
            "savesCopiedTo",
            "overrideRestored",
            "destroyed",
        ] {
            assert!(json.get(k).is_some(), "{k}: {json}");
        }
        assert_eq!(json["savesNewer"][0]["path"], out.saves_newer[0].path);

        // The game is the snapshot's, the newer saves are aside, and the pre
        // snapshot holds them.
        assert_eq!(read(&root, "game/era.txt").as_deref(), Some("then"));
        assert_eq!(
            read(&root, &format!("{SAVES}/old.sav")).as_deref(),
            Some("old")
        );
        assert!(!root.join(format!("{SAVES}/new.sav")).exists());
        assert!(!root.join("game/SAVE/slot1.sav").exists());
        assert_eq!(
            read(&aside, "users/steamuser/Saved Games/Studio/new.sav").as_deref(),
            Some("new")
        );
        assert_eq!(
            read(&aside, "game/SAVE/slot1.sav").as_deref(),
            Some("slot1")
        );
        let pre = backend::snapshot_dir(&root, n + 1).unwrap();
        assert_eq!(read(&pre, "game/era.txt").as_deref(), Some("now"));
        assert_eq!(std::fs::read_to_string(l4(&w)).unwrap(), l4_then);
        assert!(out.override_restored);
        assert!(out.override_changes.iter().any(|c| c.key.contains("FOO")));
        assert_settled(&w, &root);
        assert!(
            w.launch_log()
                .contains(&format!("rolled back to snapshot {n} (before the change)")),
            "{}",
            w.launch_log()
        );
        // The pre snapshot is an `auto` one with its label.
        let listed = snap::list(ID, &b, 5, false).unwrap();
        let pre_row = listed.snapshots.iter().find(|s| s.id == n + 1).unwrap();
        assert_eq!(pre_row.kind, Some("auto"));
        assert_eq!(pre_row.trigger, Some("rollback"));
        assert_eq!(
            pre_row.label.as_deref(),
            Some(format!("before rollback to {n}").as_str())
        );

        // It runs as the snapshot's game, with no setup: its override matches.
        assert_runs(&w);
        assert!(w.calls().is_empty(), "{:?}", w.calls());
        // `apps show` names the aside dir with its size.
        let a = gogo::macapp::apps::get(ID, &w.root.join("logs"))
            .unwrap()
            .unwrap();
        let s = a.snapshots.unwrap();
        assert_eq!(s.saves_aside, std::slice::from_ref(&aside));
        assert!(
            s.lines(ID)
                .iter()
                .any(|l| l.starts_with("saves set aside: ") && l.ends_with(" B)")),
            "{:?}",
            s.lines(ID)
        );

        // And the rollback is undone by rolling back to the snapshot before it.
        let back = go(&b, n + 1, &opts()).unwrap();
        assert_eq!(read(&root, "game/era.txt").as_deref(), Some("now"));
        assert_eq!(
            read(&root, &format!("{SAVES}/new.sav")).as_deref(),
            Some("new")
        );
        assert_eq!(back.pre, n + 2);
        assert_runs(&w);
    }

    #[test]
    fn keep_override_keeps_it_and_the_launcher_runs_setup() {
        let Some(w) = World::launcher("rb-keep", false) else {
            return;
        };
        let (_, b) = fake();
        let (root, n) = dress(&w, &b);
        let now = std::fs::read_to_string(l4(&w)).unwrap();
        let o = RollbackOptions {
            keep_override: true,
            drop_saves: true,
            ..opts()
        };
        let p = plan(&b, n, &o);
        assert!(
            p.question().ends_with(&format!(
                "will be dropped from the game (kept in snapshot {} only)",
                p.pre
            )),
            "{}",
            p.question()
        );
        assert!(
            p.lines(false)
                .iter()
                .any(|l| l.contains("kept as it is now (--keep-override)"))
        );
        let out = rb::rollback(ID, &b, n, &p, &o).unwrap();
        assert!(out.saves_copied_to.is_none());
        assert!(!out.override_restored);
        assert_eq!(std::fs::read_to_string(l4(&w)).unwrap(), now);
        assert!(!w.home.join("saves-aside").exists());
        assert_eq!(
            out.last_line(),
            format!(
                "rolled back to {n}; the state before is snapshot {}; newer saves kept in \
                 snapshot {} only",
                out.pre, out.pre
            )
        );
        assert_settled(&w, &root);
        // The stamp differs now: the launcher runs setup, then the game.
        assert_runs(&w);
        assert!(
            w.calls().iter().any(|c| c.contains("linux-app setup")),
            "{:?}",
            w.calls()
        );
    }

    #[test]
    fn with_no_newer_saves_the_question_is_plain() {
        let w = World::new("rb-plain-q", &["d3d9.dll"]);
        let (_, b) = fake();
        super::build_with(&w.opts());
        let n = snap::create(ID, &b, None).unwrap().snapshot.id;
        let p = plan(&b, n, &opts());
        assert!(p.saves.files.is_empty(), "{:?}", p.saves.files);
        assert_eq!(p.question(), format!("roll back to {n} (manual)?"));
        let out = go(&b, n, &opts()).unwrap();
        assert!(out.saves_copied_to.is_none());
        assert!(
            out.last_line().ends_with("; no newer saves"),
            "{}",
            out.last_line()
        );
    }

    /// A crash after every step of the rollback, then the recovery the next
    /// `snapshot` command runs: the game is either the one before (a crash
    /// before the commit) or the snapshot's (after it), with its override,
    /// and it launches.
    #[test]
    fn a_crash_at_every_step_recovers_to_the_old_or_the_new_game() {
        let Some(w) = World::launcher("rb-crash", false) else {
            return;
        };
        let (f, b) = fake();
        let (root, n) = dress(&w, &b);
        let l4_then = std::fs::read_to_string(
            w.home
                .join("snapshots")
                .join(ID)
                .join(format!("{n}.override.toml")),
        )
        .unwrap();
        enum Crash {
            Step(RbStep),
            Fake(FakeStep),
        }
        let mut crashes: Vec<(Crash, bool)> = RbStep::ALL
            .iter()
            .map(|s| {
                let after_commit = !matches!(
                    s,
                    RbStep::Journal | RbStep::PreSnapshot | RbStep::SavesCopied | RbStep::Staged
                );
                (Crash::Step(*s), after_commit)
            })
            .collect();
        crashes.extend([
            (Crash::Fake(FakeStep::RollbackStage), false),
            (Crash::Fake(FakeStep::RollbackStageMidway), false),
            (Crash::Fake(FakeStep::RollbackExchange), false),
            (Crash::Fake(FakeStep::RollbackDiscardMidway), true),
        ]);
        for (i, (crash, after_commit)) in crashes.into_iter().enumerate() {
            // The game as it is now: another era, another override.
            let era = format!("now-{i}");
            put(&root, "game/era.txt", &era);
            gogo::linuxapp::envargs::set_env(ID, &[("FOO".into(), era.clone())]).unwrap();
            gogo::linuxapp::setup::setup(ID, &Default::default(), &mut quiet()).unwrap();
            let l4_now = std::fs::read_to_string(l4(&w)).unwrap();
            let mut o = opts();
            let what = match crash {
                Crash::Step(s) => {
                    o.fail_at = Some(s);
                    format!("{s:?}")
                }
                Crash::Fake(s) => {
                    f.fail_at(s);
                    format!("{s:?}")
                }
            };
            let e = go(&b, n, &o).expect_err(&what);
            assert!(e.contains("injected"), "{what}: {e}");

            // The next snapshot command settles it.
            let listed = snap::list(ID, &b, 5, false).unwrap();
            let note = listed.notes.join("\n");
            assert_settled(&w, &root);
            if after_commit {
                assert!(note.contains("was finished"), "{what}: {note}");
                assert_eq!(
                    read(&root, "game/era.txt").as_deref(),
                    Some("then"),
                    "{what}"
                );
                assert_eq!(std::fs::read_to_string(l4(&w)).unwrap(), l4_then, "{what}");
                assert!(!root.join(".launchers").exists(), "{what}");
            } else {
                assert!(note.contains("was undone"), "{what}: {note}");
                assert_eq!(
                    read(&root, "game/era.txt").as_deref(),
                    Some(era.as_str()),
                    "{what}"
                );
                assert_eq!(std::fs::read_to_string(l4(&w)).unwrap(), l4_now, "{what}");
            }
            assert_runs(&w);
        }
    }

    #[test]
    fn other_changes_of_the_game_settle_a_pending_rollback_and_doctor_fails_it() {
        let w = World::new("rb-settle", &["d3d9.dll"]);
        let (_, b) = fake();
        let root = super::build_with(&w.opts()).dirs.root;
        put(&root, "game/era.txt", "then");
        let n = snap::create(ID, &b, None).unwrap().snapshot.id;
        put(&root, "game/era.txt", "now");
        let o = RollbackOptions {
            fail_at: Some(RbStep::LauncherWritten),
            ..opts()
        };
        go(&b, n, &o).unwrap_err();
        assert!(root.join(backend::ROLLBACK_MARKER).exists());
        let f = snap::doctor(ID, &root, &b);
        assert!(
            f.iter()
                .any(|f| f.level == gogo::macapp::doctor::Level::Fail
                    && f.message
                        .contains(&format!("a rollback to snapshot {n} didn't finish"))
                    && f.hint
                        .as_deref()
                        .is_some_and(|h| h.contains("snapshot 1999999901 list"))),
            "{f:?}"
        );
        // A rebuild settles it first.
        let plan = build::plan(&w.opts(), None, &mut quiet()).unwrap();
        let mut warns = Vec::new();
        build::execute(&plan, None, &mut |e| {
            if let gogo::engine::events::ProgressEvent::Warn { message } = e {
                warns.push(message.clone());
            }
        })
        .unwrap();
        assert!(
            warns
                .iter()
                .any(|m| m.contains(&format!("rollback to snapshot {n} was finished"))),
            "{warns:?}"
        );
        assert_settled(&w, &root);
        assert!(
            !snap::doctor(ID, &root, &b)
                .iter()
                .any(|f| f.level == gogo::macapp::doctor::Level::Fail)
        );

        // `apps remove` too: a staged copy beside the game goes with it.
        put(&root, "game/era.txt", "again");
        let o = RollbackOptions {
            fail_at: Some(RbStep::Staged),
            ..opts()
        };
        go(&b, n, &o).unwrap_err();
        assert!(backend::rollback_staging(&root).unwrap().exists());
        let a = gogo::macapp::apps::get(ID, &w.root.join("logs"))
            .unwrap()
            .unwrap();
        gogo::macapp::apps::remove_with(&a, false, &b).unwrap();
        assert!(!backend::rollback_staging(&root).unwrap().exists());
        assert!(!root.exists());
    }

    /// Every refusal of §5.1, before anything is touched; and numbers that
    /// can't be rolled back to.
    #[test]
    fn every_refusal_of_a_rollback() {
        let w = World::new("rb-refuse", &["d3d9.dll"]);
        let (_, b) = fake();
        let root = super::build_with(&w.opts_in_library()).dirs.root;
        let n = snap::create(ID, &b, None).unwrap().snapshot.id;
        let meta = w.home.join("snapshots").join(ID);
        let refused = |why: &str| {
            let e = rb::plan(ID, &b, n, &opts())
                .map(|_| ())
                .unwrap_err()
                .to_string();
            assert!(e.contains(why), "{why}: {e}");
            // The plan of a moment ago, then the refusal under the locks.
            assert!(!meta.join("rollback.json").exists(), "{why}");
            assert!(
                !meta.join(format!("{}.toml", n + 1)).exists(),
                "{why}: a snapshot was taken"
            );
        };
        let me = std::process::id().to_string();
        let launchers = root.join(".launchers");
        std::fs::create_dir_all(&launchers).unwrap();
        std::fs::write(launchers.join(&me), format!("{}\n", my_start())).unwrap();
        refused("is running");
        std::fs::remove_dir_all(&launchers).unwrap();
        let one_offs = root.join(".one-offs");
        std::fs::create_dir_all(&one_offs).unwrap();
        std::fs::write(one_offs.join(&me), format!("{}\nx.exe\n", my_start())).unwrap();
        refused("`gogo linux-app run`");
        std::fs::remove_dir_all(&one_offs).unwrap();
        for f in ["dlc-journal.json", "dlc-download.json"] {
            std::fs::write(root.join(f), "{}").unwrap();
            refused("DLC add");
            std::fs::remove_file(root.join(f)).unwrap();
        }
        std::fs::create_dir_all(root.join(".dlc-staging")).unwrap();
        refused("DLC add");
        std::fs::remove_dir(root.join(".dlc-staging")).unwrap();
        let journal = w.home.join("game-moves").join(ID);
        std::fs::write(&journal, "x\n").unwrap();
        refused("move");
        std::fs::remove_file(&journal).unwrap();
        let lib = root.parent().unwrap().to_path_buf();
        let away = lib.with_file_name("unmounted");
        std::fs::rename(&lib, &away).unwrap();
        refused("isn't there");
        std::fs::rename(&away, &lib).unwrap();

        // The game's lock held by another gogo: refused under the locks.
        let p = plan(&b, n, &opts());
        let held = gogo::macapp::relocate::lock_game(ID).unwrap();
        assert!(rb::rollback(ID, &b, n, &p, &opts()).is_err());
        drop(held);
        assert!(!meta.join(format!("{}.toml", n + 1)).exists());

        // Numbers that can't be rolled back to: a usage error.
        let unknown = |k: u32, why: &str| match rb::plan(ID, &b, k, &opts()).unwrap() {
            Planned::Unknown(w) => assert!(w.contains(why), "{k}: {w}"),
            Planned::Plan(_) => panic!("{k} planned"),
        };
        unknown(9, "there is no snapshot 9");
        std::fs::create_dir_all(backend::snapshot_dir(&root, 7).unwrap()).unwrap();
        unknown(7, "without metadata");
        let m = snap::create(ID, &b, None).unwrap().snapshot.id;
        std::fs::remove_dir_all(backend::snapshot_dir(&root, m).unwrap()).unwrap();
        unknown(m, "not on disk");

        // And with all of that settled, it works.
        go(&b, n, &opts()).unwrap();
    }

    /// The game moved since the snapshot (on one filesystem): `launch.conf`
    /// is rewritten for where it is now (design 20 §3).
    #[test]
    fn a_snapshot_taken_elsewhere_gets_its_launch_conf_rewritten() {
        let Some(w) = World::launcher_with("rb-moved", false, true) else {
            return;
        };
        let (_, b) = fake();
        let root = w.game();
        let n = snap::create(ID, &b, None).unwrap().snapshot.id;
        // As if taken at another place: the metadata and the snapshot's
        // launch.conf name it.
        let old = w.root.join("old place").join(ID);
        let file = w.home.join("snapshots").join(ID).join(format!("{n}.toml"));
        let mut meta: gogo::snapshot::Meta =
            toml::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        meta.game_dir = old.clone();
        std::fs::write(&file, toml::to_string(&meta).unwrap()).unwrap();
        let conf = backend::snapshot_dir(&root, n).unwrap().join("launch.conf");
        let text = std::fs::read_to_string(&conf).unwrap();
        let here = root.to_string_lossy().into_owned();
        assert!(text.contains(&here), "{text}");
        std::fs::write(&conf, text.replace(&here, &old.to_string_lossy())).unwrap();

        let p = plan(&b, n, &opts());
        assert_eq!(p.rewrite_from.as_deref(), Some(old.as_path()));
        assert!(
            p.lines(false)
                .iter()
                .any(|l| l.contains("launch.conf is rewritten"))
        );
        rb::rollback(ID, &b, n, &p, &opts()).unwrap();
        let now = w.conf();
        assert!(now.contains(&here), "{now}");
        assert!(!now.contains(&old.to_string_lossy().into_owned()), "{now}");
        assert_runs(&w);
    }

    /// The launcher refuses while a rollback or a conversion is unfinished
    /// (design 20 §5.4): no setup, no game, a run log and a dialog.
    #[test]
    fn the_launcher_refuses_while_a_marker_is_in_the_game_dir() {
        let Some(w) = World::launcher("rb-marker", false) else {
            return;
        };
        let stub = w.dialog_stub(&["", ""]);
        for (marker, why, says) in [
            (
                backend::ROLLBACK_MARKER,
                "a snapshot rollback isn't finished",
                "A rollback of Test Game to an earlier snapshot stopped",
            ),
            (
                backend::CONVERTED_MARKER,
                "a conversion for snapshots isn't finished",
                "for snapshots stopped before it finished",
            ),
        ] {
            std::fs::write(w.game().join(marker), "3\n").unwrap();
            // A stale stamp, which would run setup: it doesn't.
            gogo::linuxapp::envargs::set_env(ID, &[("FOO".into(), marker.into())]).unwrap();
            let out = w.launch(&[("GOGO_LAUNCHER_DIALOG", stub.to_str().unwrap())], &[]);
            assert_eq!(out.status.code(), Some(127), "{marker}: {out:?}");
            assert_eq!(w.game_runs(), 0, "{marker}");
            assert!(w.calls().is_empty(), "{marker}: {:?}", w.calls());
            let last = w.dialogs().last().cloned().unwrap_or_default();
            assert!(last.contains(says), "{marker}: {last}");
            assert!(
                w.runs()
                    .last()
                    .unwrap()
                    .contains(&format!("# exit: 127 ({why})")),
                "{marker}"
            );
            assert!(
                w.launch_log().contains(&format!("{why}: not launching")),
                "{marker}"
            );
            std::fs::remove_file(w.game().join(marker)).unwrap();
        }
        // Without them, it runs (setup first: the override changed).
        assert_runs(&w);
    }

    /// The snapshot's DLC set made another than the game's, as if a DLC was
    /// added since: the game's files are checked without a list then.
    fn other_content_since(w: &World, n: u32) {
        let file = w.home.join("snapshots").join(ID).join(format!("{n}.toml"));
        let mut meta: gogo::snapshot::Meta =
            toml::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        meta.content.dlcs = vec![1_234_567];
        std::fs::write(&file, toml::to_string(&meta).unwrap()).unwrap();
    }

    /// The build or DLC set changed and there is no list: new game files are
    /// unattributed and still copied (small, or cloned) under
    /// `unattributed/`; a changed registry file goes under `registry/`; the
    /// `Public` user dir is a save root.
    #[test]
    fn unattributed_files_the_registry_and_public_go_aside() {
        let Some(w) = World::launcher("rb-unattr", false) else {
            return;
        };
        let (_, b) = fake();
        let (root, n) = dress(&w, &b);
        other_content_since(&w, n);
        put(
            &root,
            "prefix/user.reg",
            "WINE REGISTRY Version 2\n[after]\n",
        );
        put(
            &root,
            "prefix/drive_c/users/Public/Documents/Studio/p.sav",
            "pub",
        );
        let p = plan(&b, n, &opts());
        let un: Vec<&str> = p
            .saves
            .unattributed
            .iter()
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(un, ["game/SAVE/slot1.sav"], "{un:?}");
        assert!(p.copy_unattributed);
        let reg: Vec<&str> = p.saves.registry.iter().map(|f| f.path.as_str()).collect();
        assert!(reg.contains(&"user.reg"), "{reg:?}");
        let saves: Vec<&str> = p.saves.files.iter().map(|f| f.path.as_str()).collect();
        assert!(
            saves.contains(&"users/Public/Documents/Studio/p.sav"),
            "{saves:?}"
        );
        // era.txt was in the snapshot: changed, so it counts.
        assert!(saves.contains(&"game/era.txt"), "{saves:?}");
        let lines = p.lines(false).join("\n");
        assert!(
            lines.contains("the build or DLC set changed and there is no file list"),
            "{lines}"
        );
        assert!(lines.contains("gogo can't attribute"), "{lines}");
        assert!(lines.contains("registry changed (user.reg"), "{lines}");
        assert!(lines.contains("not scanned: "), "{lines}");
        assert!(
            !p.question().contains("won't be copied"),
            "{}",
            p.question()
        );

        let out = rb::rollback(ID, &b, n, &p, &opts()).unwrap();
        let aside = out.saves_copied_to.clone().unwrap();
        assert_eq!(
            read(&aside, "unattributed/game/SAVE/slot1.sav").as_deref(),
            Some("slot1")
        );
        assert!(
            read(&aside, "registry/user.reg")
                .unwrap()
                .contains("[after]")
        );
        assert_eq!(
            read(&aside, "users/Public/Documents/Studio/p.sav").as_deref(),
            Some("pub")
        );
        assert!(!root.join("game/SAVE/slot1.sav").exists());
        assert_settled(&w, &root);
    }

    /// A crash right after the swap, before step 6: the game dir is the
    /// snapshot's, whose own `launch.sh` predates the marker check. The
    /// rollback put this gogo's into the staged tree, so a launch from the
    /// menu is refused until the rollback is settled.
    #[test]
    fn right_after_the_swap_the_staged_launcher_refuses_on_the_marker() {
        let Some(w) = World::launcher("rb-staged-sh", false) else {
            return;
        };
        let stub = w.dialog_stub(&["", ""]);
        let (_, b) = fake();
        let (root, n) = dress(&w, &b);
        // The snapshot's launcher: an old one that would just run the game.
        let snap_sh = backend::snapshot_dir(&root, n).unwrap().join("launch.sh");
        let ran = w.root.join("old-launcher-ran");
        std::fs::write(
            &snap_sh,
            format!(
                "#!/bin/sh\nLAUNCHER_VERSION=1\ntouch '{}'\nexit 0\n",
                ran.display()
            ),
        )
        .unwrap();
        let o = RollbackOptions {
            fail_at: Some(RbStep::Committed),
            ..opts()
        };
        go(&b, n, &o).unwrap_err();
        assert!(root.join(backend::ROLLBACK_MARKER).exists());
        assert_eq!(read(&root, "game/era.txt").as_deref(), Some("then"));
        let out = w.launch(&[("GOGO_LAUNCHER_DIALOG", stub.to_str().unwrap())], &[]);
        assert_eq!(out.status.code(), Some(127), "{out:?}");
        assert!(!ran.exists(), "the snapshot's own launch.sh ran");
        assert_eq!(w.game_runs(), 0);
        assert!(
            w.launch_log()
                .contains("a snapshot rollback isn't finished: not launching")
        );
        // Settled, it runs.
        snap::list(ID, &b, 5, false).unwrap();
        assert_settled(&w, &root);
        assert_runs(&w);
    }

    /// A target whose override copy is gone is refused before anything is
    /// touched; one gone after the commit leaves the current override.
    #[test]
    fn a_missing_override_copy_is_refused_before_the_commit() {
        let Some(w) = World::launcher("rb-no-l4", false) else {
            return;
        };
        let (_, b) = fake();
        let (root, n) = dress(&w, &b);
        let copy = w
            .home
            .join("snapshots")
            .join(ID)
            .join(format!("{n}.override.toml"));
        let kept = std::fs::read(&copy).unwrap();
        std::fs::remove_file(&copy).unwrap();
        match rb::plan(ID, &b, n, &opts()).unwrap() {
            Planned::Unknown(why) => {
                assert!(why.contains("copy of the override file"), "{why}")
            }
            Planned::Plan(_) => panic!("planned without its override copy"),
        }
        assert!(
            !w.home
                .join("snapshots")
                .join(ID)
                .join(format!("{}.toml", n + 1))
                .exists()
        );
        assert_eq!(read(&root, "game/era.txt").as_deref(), Some("now"));

        // Gone after the commit (a crash, then a hand edit): recovery keeps
        // the current override and finishes, every time.
        std::fs::write(&copy, &kept).unwrap();
        let l4_now = std::fs::read_to_string(l4(&w)).unwrap();
        let o = RollbackOptions {
            fail_at: Some(RbStep::Committed),
            ..opts()
        };
        go(&b, n, &o).unwrap_err();
        std::fs::remove_file(&copy).unwrap();
        let listed = snap::list(ID, &b, 5, false).unwrap();
        assert!(
            listed.notes.join("\n").contains("was finished"),
            "{:?}",
            listed.notes
        );
        assert_settled(&w, &root);
        assert_eq!(std::fs::read_to_string(l4(&w)).unwrap(), l4_now);
        assert_eq!(read(&root, "game/era.txt").as_deref(), Some("then"));
    }

    /// Settling takes the refusals of a running game: a pending rollback
    /// waits for it to quit, and `list` still lists.
    #[test]
    fn a_running_game_holds_off_settling() {
        let w = World::new("rb-settle-running", &["d3d9.dll"]);
        let (_, b) = fake();
        let root = super::build_with(&w.opts()).dirs.root;
        put(&root, "game/era.txt", "then");
        let n = snap::create(ID, &b, None).unwrap().snapshot.id;
        put(&root, "game/era.txt", "now");
        let o = RollbackOptions {
            fail_at: Some(RbStep::Staged),
            ..opts()
        };
        go(&b, n, &o).unwrap_err();
        let journal = w.home.join("snapshots").join(ID).join("rollback.json");
        assert!(journal.exists());
        // The game runs (its launcher's pid file).
        let launchers = root.join(".launchers");
        std::fs::create_dir_all(&launchers).unwrap();
        std::fs::write(
            launchers.join(std::process::id().to_string()),
            format!("{}\n", my_start()),
        )
        .unwrap();
        let listed = snap::list(ID, &b, 5, false).unwrap();
        assert!(
            listed
                .notes
                .join("\n")
                .contains("an interrupted rollback isn't settled yet"),
            "{:?}",
            listed.notes
        );
        assert!(journal.exists());
        assert!(backend::rollback_staging(&root).unwrap().exists());
        let e = rb::settle_pending(ID, &b).unwrap_err().to_string();
        assert!(e.contains("is running"), "{e}");
        let e = snap::create(ID, &b, None)
            .map(|_| ())
            .unwrap_err()
            .to_string();
        assert!(e.contains("is running"), "{e}");
        assert!(journal.exists());
        // It quits: the next command settles it.
        std::fs::remove_dir_all(&launchers).unwrap();
        let listed = snap::list(ID, &b, 5, false).unwrap();
        assert!(listed.notes.join("\n").contains("was undone"));
        assert_settled(&w, &root);
    }

    fn my_start() -> String {
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
        let rest = stat.rsplit_once(") ").unwrap().1;
        rest.split_whitespace().nth(19).unwrap().to_string()
    }

    /// On a real btrfs ([`World::on_btrfs`], else skipped): the rollback
    /// stages a writable snapshot, swaps it in with `RENAME_EXCHANGE`, keeps
    /// the game dir a subvolume, deletes the old tree, and can be undone. Run
    /// as [`super::btrfs_rollback_swaps_in_a_writable_snapshot`].
    pub(super) fn real_btrfs_rollback() {
        use gogo::snapshot::btrfs;
        let Some(w) = World::on_btrfs("btrfs_rollback_swaps_in_a_writable_snapshot", &["d3d9.dll"])
        else {
            return;
        };
        let real = Backends::real();
        let mut o = w.opts();
        o.snapshots = SnapshotOptions::on(real.clone());
        let built = super::build_with(&o);
        let root = built.dirs.root.clone();
        let _guard = Cleanup(root.clone());
        assert!(btrfs::is_subvolume(&root).unwrap());
        assert_eq!(built.snapshot.as_ref().map(|m| m.id), Some(1));
        put(&root, &format!("{SAVES}/old.sav"), "old");
        put(&root, "game/era.txt", "then");
        let n = snap::create(ID, &real, Some("then")).unwrap().snapshot.id;
        std::thread::sleep(std::time::Duration::from_millis(1100));
        put(&root, &format!("{SAVES}/new.sav"), "new");
        put(&root, "game/era.txt", "now");

        let out = go(&real, n, &opts()).unwrap();
        assert_eq!(read(&root, "game/era.txt").as_deref(), Some("then"));
        assert!(!root.join(format!("{SAVES}/new.sav")).exists());
        assert!(btrfs::is_subvolume(&root).unwrap(), "still a subvolume");
        assert!(!btrfs::is_readonly(&root).unwrap(), "writable");
        assert_settled(&w, &root);
        let pre = backend::snapshot_dir(&root, out.pre).unwrap();
        assert!(btrfs::is_readonly(&pre).unwrap());
        assert_eq!(read(&pre, "game/era.txt").as_deref(), Some("now"));
        let aside = out.saves_copied_to.clone().unwrap();
        assert_eq!(
            read(&aside, "users/steamuser/Saved Games/Studio/new.sav").as_deref(),
            Some("new")
        );

        // Undone: back to the snapshot taken before it.
        go(&real, out.pre, &opts()).unwrap();
        assert_eq!(read(&root, "game/era.txt").as_deref(), Some("now"));
        assert!(btrfs::is_subvolume(&root).unwrap());

        // A crash after the commit: the next command finishes it, and the
        // old tree (a subvolume) goes.
        let o = RollbackOptions {
            fail_at: Some(RbStep::LauncherWritten),
            ..opts()
        };
        go(&real, n, &o).unwrap_err();
        let staged = backend::rollback_staging(&root).unwrap();
        assert!(btrfs::is_subvolume(&staged).unwrap());
        snap::list(ID, &real, 5, false).unwrap();
        assert_settled(&w, &root);
        assert_eq!(read(&root, "game/era.txt").as_deref(), Some("then"));

        let a = gogo::macapp::apps::get(ID, &w.root.join("logs"))
            .unwrap()
            .unwrap();
        gogo::macapp::apps::remove(&a, false).unwrap();
        assert!(!root.exists());
    }

    /// Whatever a failed btrfs test left: read-only snapshots and a staged
    /// subvolume aren't removed by the world's cleanup.
    struct Cleanup(PathBuf);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Some(dir) = backend::snapshots_dir(&self.0)
                && let Ok(rd) = std::fs::read_dir(&dir)
            {
                for e in rd.flatten() {
                    let _ = gogo::snapshot::btrfs::destroy_by_rmdir(&e.path());
                }
            }
            if let Some(st) = backend::rollback_staging(&self.0) {
                let _ = gogo::snapshot::btrfs::destroy_by_rmdir(&st);
            }
        }
    }
}

#[test]
fn btrfs_rollback_swaps_in_a_writable_snapshot() {
    rollback::real_btrfs_rollback();
}

#[test]
fn btrfs_convert_makes_a_plain_game_a_subvolume() {
    snapshots::real_btrfs_convert();
}

/// Automatic snapshots before a change and their retention (design 20 §7.1,
/// §7.2, #405), with the fake backend: a rebuild, a setup that changes the
/// game's GE-Proton, what skips them and what a failed one stops.
mod autosnapshot {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use super::{ID, LinuxAppOptions, World, build, quiet};
    use gogo::linuxapp::snapshots::{
        self as snap, AutoOptions, Backends, BuildSnapshots, SnapshotOptions,
    };
    use gogo::linuxapp::{envargs, manage, setup};
    use gogo::macapp::error::MacAppError;
    use gogo::snapshot::backend::{self, BackendKind, OnDisk, SnapshotBackend, Space, Support};
    use gogo::snapshot::fake::{Fake, FakeStep};
    use gogo::snapshot::{Kind, Trigger};
    use serde_json::Value;

    fn fake() -> (Arc<Fake>, Backends) {
        let f = Arc::new(Fake::new());
        (f.clone(), Backends::fixed(f))
    }

    fn on(w: &World, b: &Backends, keep_auto: usize) -> LinuxAppOptions {
        LinuxAppOptions {
            snapshots: SnapshotOptions {
                keep_auto,
                ..SnapshotOptions::on(b.clone())
            },
            ..w.opts()
        }
    }

    fn auto(b: &Backends) -> AutoOptions {
        AutoOptions {
            backends: b.clone(),
            ..AutoOptions::default()
        }
    }

    /// Plan and run a build, collecting its events as `--json-progress` has
    /// them.
    fn build_events(o: &LinuxAppOptions) -> (Result<build::BuildOutcome, MacAppError>, Vec<Value>) {
        let plan = build::plan(o, None, &mut quiet()).unwrap();
        let mut events = Vec::new();
        let r = build::execute(&plan, None, &mut |e| {
            events.push(serde_json::to_value(e).unwrap())
        });
        (r, events)
    }

    /// A game built with snapshots on: snapshot 1 is its `initial` one.
    fn built(name: &str, b: &Backends) -> World {
        let w = World::new(name, &["d3d9.dll"]);
        let out = build_events(&on(&w, b, 5)).0.unwrap();
        assert_eq!(
            out.snapshot.map(|m| (m.id, m.kind)),
            Some((1, Kind::Initial))
        );
        w
    }

    /// `(id, kind, label)` of the game's snapshots, oldest first.
    fn snaps(b: &Backends) -> Vec<(u32, String, String)> {
        let mut v: Vec<(u32, String, String)> = snap::list(ID, b, 5, false)
            .unwrap()
            .snapshots
            .into_iter()
            .map(|s| {
                (
                    s.id,
                    s.kind.unwrap_or("?").to_string(),
                    s.label.unwrap_or_default(),
                )
            })
            .collect();
        v.sort();
        v
    }

    fn ids(b: &Backends) -> Vec<u32> {
        snaps(b).into_iter().map(|s| s.0).collect()
    }

    /// The events of one kind.
    fn of<'a>(events: &'a [Value], kind: &str) -> Vec<&'a Value> {
        events.iter().filter_map(|e| e.get(kind)).collect()
    }

    fn auto_setting(w: &World) -> String {
        std::fs::read_to_string(w.home.join("snapshots").join(ID).join("settings.toml"))
            .unwrap_or_default()
    }

    #[test]
    fn a_rebuild_takes_before_rebuild_and_retention_keeps_the_newest() {
        let (_, b) = fake();
        let w = built("auto-rebuild", &b);
        let root = w.game();

        let plan = build::plan(&on(&w, &b, 2), None, &mut quiet()).unwrap();
        assert_eq!(plan.snapshots, BuildSnapshots::Kept { on: true });
        assert!(
            plan.snapshots
                .line()
                .contains("`before rebuild` is taken first")
        );
        // A file only the old game has: the snapshot holds the game as it was.
        std::fs::write(root.join("game/old-only.txt"), "old").unwrap();
        let (out, events) = build_events(&on(&w, &b, 2));
        let out = out.unwrap();
        let m = out.snapshot.expect("before rebuild");
        assert_eq!(
            (m.id, m.kind, m.trigger, m.label.as_str()),
            (2, Kind::Auto, Some(Trigger::Rebuild), "before rebuild")
        );
        assert!(out.pruned.is_empty());
        let created = of(&events, "SnapshotCreated");
        assert_eq!(created.len(), 1, "{events:?}");
        assert_eq!(created[0]["id"], 2);
        assert_eq!(created[0]["label"], "before rebuild");
        assert_eq!(created[0]["trigger"], "rebuild");
        let two = backend::snapshot_dir(&root, 2).unwrap();
        assert!(two.join("game/old-only.txt").is_file());
        assert!(!root.join("game/old-only.txt").exists(), "rebuilt after it");

        // A manual one in between is never pruned, nor is the initial one.
        snap::create(ID, &b, Some("mine")).unwrap();
        build_events(&on(&w, &b, 2)).0.unwrap();
        assert_eq!(ids(&b), [1, 2, 3, 4]);
        let (out, events) = build_events(&on(&w, &b, 2));
        let out = out.unwrap();
        assert_eq!(out.snapshot.map(|m| m.id), Some(5));
        assert_eq!(out.pruned.iter().map(|m| m.id).collect::<Vec<_>>(), [2]);
        let pruned = of(&events, "SnapshotPruned");
        assert_eq!(pruned.len(), 1, "{events:?}");
        assert_eq!(pruned[0]["id"], 2);
        assert_eq!(pruned[0]["label"], "before rebuild");
        let line = snap::pruned_line(2, "before rebuild", pruned[0]["created"].as_str().unwrap());
        assert!(
            line.starts_with("pruned snapshot 2 (before rebuild, 20") && line.ends_with(')'),
            "{line}"
        );
        assert_eq!(
            snaps(&b),
            [
                (1, "initial".into(), "after the first build".into()),
                (3, "manual".into(), "mine".into()),
                (4, "auto".into(), "before rebuild".into()),
                (5, "auto".into(), "before rebuild".into()),
            ]
        );
        assert!(!backend::snapshot_dir(&root, 2).unwrap().exists());
    }

    #[test]
    fn keep_auto_zero_keeps_only_the_newest_until_the_next() {
        let (_, b) = fake();
        let w = built("auto-keep0", &b);
        build_events(&on(&w, &b, 0)).0.unwrap();
        assert_eq!(ids(&b), [1, 2], "the new one protects this change");
        let out = build_events(&on(&w, &b, 0)).0.unwrap();
        assert_eq!(out.pruned.iter().map(|m| m.id).collect::<Vec<_>>(), [2]);
        assert_eq!(ids(&b), [1, 3]);
    }

    #[test]
    fn no_snapshots_and_autosnapshot_off_skip_it() {
        let (_, b) = fake();
        let w = built("auto-skip", &b);

        // `--no-snapshots`: none this run, the setting stays on.
        let o = LinuxAppOptions {
            snapshots: SnapshotOptions {
                backends: b.clone(),
                ..SnapshotOptions::no_snapshots()
            },
            ..w.opts()
        };
        let plan = build::plan(&o, None, &mut quiet()).unwrap();
        assert_eq!(plan.snapshots, BuildSnapshots::Skipped);
        assert_eq!(
            plan.snapshots.line(),
            "off for this run (--no-snapshots; a rebuild: the game dir stays as it is)"
        );
        let (out, events) = build_events(&o);
        assert!(out.unwrap().snapshot.is_none());
        assert!(of(&events, "SnapshotCreated").is_empty());
        assert_eq!(ids(&b), [1]);
        assert!(auto_setting(&w).contains("auto = true"));

        // Autosnapshot off for the game.
        snap::set_auto(ID, &b, false, 5).unwrap();
        assert!(build_events(&on(&w, &b, 5)).0.unwrap().snapshot.is_none());
        assert_eq!(ids(&b), [1]);
    }

    #[test]
    fn a_plain_dir_is_skipped_and_never_converted() {
        let w = World::new("auto-plain", &["d3d9.dll"]);
        // Built without snapshots: a plain dir. A setting turned on by hand
        // doesn't make the change convert it (decision D8).
        build_events(&w.opts()).0.unwrap();
        std::fs::write(
            w.home.join("snapshots").join(ID).join("settings.toml"),
            "auto = true\nnext = 1\n",
        )
        .unwrap();
        let strict = Backends::fixed(Arc::new(Fake::subvolumes_only()));
        let dirs = gogo::macapp::launch::GameDirs::for_game(ID).unwrap();
        assert_eq!(
            snap::auto_skipped(ID, &dirs, &auto(&strict)).unwrap(),
            Some("the game dir isn't a subvolume")
        );
        let out = build_events(&on(&w, &strict, 5)).0.unwrap();
        assert!(out.snapshot.is_none());
        assert!(!backend::snapshots_dir(&w.game()).unwrap().exists());
    }

    /// The fake, on a filesystem without snapshots.
    struct NoSnapshots(Fake);

    impl SnapshotBackend for NoSnapshots {
        fn probe(&self, _dir: &Path) -> Support {
            Support::None("tmpfs has no snapshots".into())
        }
        fn kind(&self) -> BackendKind {
            self.0.kind()
        }
        fn ready(&self, game_dir: &Path) -> std::io::Result<bool> {
            self.0.ready(game_dir)
        }
        fn make_game_dir(&self, dir: &Path) -> std::io::Result<()> {
            self.0.make_game_dir(dir)
        }
        fn snapshot(&self, game_dir: &Path, n: u32) -> std::io::Result<()> {
            self.0.snapshot(game_dir, n)
        }
        fn snapshot_path(&self, game_dir: &Path, n: u32) -> Option<PathBuf> {
            self.0.snapshot_path(game_dir, n)
        }
        fn exists(&self, game_dir: &Path, n: u32) -> std::io::Result<bool> {
            self.0.exists(game_dir, n)
        }
        fn delete(&self, game_dir: &Path, n: u32) -> std::io::Result<()> {
            self.0.delete(game_dir, n)
        }
        fn on_disk(&self, game_dir: &Path) -> std::io::Result<Vec<OnDisk>> {
            self.0.on_disk(game_dir)
        }
        fn space(&self, game_dir: &Path, ns: &[u32]) -> std::io::Result<Vec<Space>> {
            self.0.space(game_dir, ns)
        }
    }

    #[test]
    fn a_none_backend_is_skipped() {
        let (_, b) = fake();
        let w = built("auto-none", &b);
        let none = Backends::fixed(Arc::new(NoSnapshots(Fake::new())));
        let dirs = gogo::macapp::launch::GameDirs::for_game(ID).unwrap();
        assert_eq!(
            snap::auto_skipped(ID, &dirs, &auto(&none)).unwrap(),
            Some("no snapshots on this filesystem")
        );
        let taken = snap::protect(
            ID,
            &dirs,
            None,
            Trigger::Rebuild,
            "before rebuild".into(),
            &auto(&none),
            &mut quiet(),
        )
        .unwrap();
        assert!(taken.is_none());
        assert_eq!(ids(&b), [1]);
        drop(w);
    }

    #[test]
    fn a_failed_snapshot_stops_a_rebuild_before_any_write() {
        let (f, b) = fake();
        let w = built("auto-fail", &b);
        let root = w.game();
        std::fs::write(root.join("game/old-only.txt"), "old").unwrap();
        let toml = std::fs::read(root.join("gogo-launch.toml")).unwrap();
        let state = std::fs::read(root.join("state.toml")).unwrap();
        let umu_runs = w.log().lines().count();

        f.fail_at(FakeStep::Snapshot);
        let (out, events) = build_events(&on(&w, &b, 5));
        let e = out.expect_err("the rebuild stops");
        assert!(matches!(e, MacAppError::Apps(_)), "exit 7: {e:?}");
        let msg = e.to_string();
        assert!(msg.contains("(before rebuild) couldn't be taken"), "{msg}");
        assert!(msg.contains("nothing was changed"), "{msg}");
        assert!(
            msg.contains("`--no-snapshots` skips it for this run"),
            "{msg}"
        );
        assert!(of(&events, "SnapshotCreated").is_empty());
        // Nothing changed: the old files, the launch config, no umu run.
        assert!(root.join("game/old-only.txt").is_file());
        assert_eq!(std::fs::read(root.join("gogo-launch.toml")).unwrap(), toml);
        assert_eq!(std::fs::read(root.join("state.toml")).unwrap(), state);
        assert_eq!(w.log().lines().count(), umu_runs);
        assert_eq!(ids(&b), [1], "no entry left for the failed one");

        // `--no-snapshots` lets it go ahead, unprotected by request.
        let o = LinuxAppOptions {
            snapshots: SnapshotOptions {
                backends: b.clone(),
                ..SnapshotOptions::no_snapshots()
            },
            ..w.opts()
        };
        build_events(&o).0.unwrap();
        assert!(!root.join("game/old-only.txt").exists());
    }

    /// Another GE-Proton build, `path:` to it.
    fn other_build(w: &World, name: &str) -> String {
        let to = w.root.join(name);
        gogo::gog::materialize::materialize_tree(
            &w.build,
            &to,
            gogo::gog::materialize::Reflink::Never,
        )
        .unwrap();
        std::fs::write(to.join("version"), format!("1793687166 {name}\n")).unwrap();
        format!("path:{}", to.display())
    }

    fn retarget(
        b: &Backends,
        updates: &[(&str, Option<&str>)],
        skip: bool,
    ) -> (manage::RetargetOutcome, Vec<Value>) {
        let mut events = Vec::new();
        let out = manage::retarget(
            ID,
            updates,
            &setup::SetupOptions {
                refresh: false,
                prompt: None,
                runtime_snapshot: Some(AutoOptions { skip, ..auto(b) }),
            },
            &mut |e| events.push(serde_json::to_value(e).unwrap()),
        )
        .unwrap();
        (out, events)
    }

    #[test]
    fn a_setup_that_changes_the_runtime_takes_before_old_to_new() {
        let (f, b) = fake();
        let w = built("auto-runtime", &b);
        let old = w.state().runtime;
        let new = other_build(&w, "GE-Proton10-40");

        let (out, events) = retarget(&b, &[("runtime", Some(&new))], false);
        let s = out.setup.unwrap().unwrap();
        let t = s.snapshot.expect("before the runtime change");
        assert_eq!(
            (t.meta.id, t.meta.trigger, t.meta.label.as_str()),
            (
                2,
                Some(Trigger::Runtime),
                "before GE-Proton10-34 → GE-Proton10-40"
            )
        );
        // Taken before setup wrote anything: it holds the old runtime.
        assert_eq!(t.meta.content.runtime.as_deref(), Some(old.as_str()));
        assert_eq!(of(&events, "SnapshotCreated").len(), 1);
        assert_eq!(w.state().runtime, new);

        // Not triggers: a renderer change, an env edit (a stamp-only setup),
        // the same setup again.
        retarget(&b, &[("renderer", Some("wined3d"))], false);
        envargs::set_env(ID, &[("FOO".into(), "bar".into())]).unwrap();
        let again = setup::setup(
            ID,
            &setup::SetupOptions {
                runtime_snapshot: Some(auto(&b)),
                ..Default::default()
            },
            &mut quiet(),
        )
        .unwrap();
        assert!(again.snapshot.is_none());
        assert_eq!(ids(&b), [1, 2]);

        // A failed one stops the setup: the runtime stays the old one.
        let newer = other_build(&w, "GE-Proton10-41");
        f.fail_at(FakeStep::Snapshot);
        let (out, _) = retarget(&b, &[("runtime", Some(&newer))], false);
        let e = out.setup.unwrap().unwrap_err().to_string();
        assert!(e.contains("before GE-Proton10-40 → GE-Proton10-41"), "{e}");
        assert!(e.contains("`--no-snapshots` skips it for this run"), "{e}");
        assert_eq!(w.state().runtime, new);
        assert_eq!(ids(&b), [1, 2]);

        // The override says the new runtime (the next setup tries again);
        // `linux-app setup --no-snapshots`: none, and the change goes ahead.
        let mut events = Vec::new();
        let s = setup::setup(
            ID,
            &setup::SetupOptions {
                runtime_snapshot: Some(AutoOptions {
                    skip: true,
                    ..auto(&b)
                }),
                ..Default::default()
            },
            &mut |e| events.push(serde_json::to_value(e).unwrap()),
        )
        .unwrap();
        assert!(s.snapshot.is_none());
        assert!(of(&events, "SnapshotCreated").is_empty());
        assert_eq!(w.state().runtime, newer);
        assert_eq!(ids(&b), [1, 2]);
    }

    #[test]
    fn a_rebuild_onto_another_runtime_takes_one_snapshot() {
        let (_, b) = fake();
        let w = built("auto-rebuild-rt", &b);
        let new = other_build(&w, "GE-Proton10-40");
        let o = LinuxAppOptions {
            runtime: Some(new.clone()),
            ..on(&w, &b, 5)
        };
        let (out, events) = build_events(&o);
        assert_eq!(
            out.unwrap().snapshot.map(|m| m.label),
            Some("before rebuild".into())
        );
        assert_eq!(of(&events, "SnapshotCreated").len(), 1, "{events:?}");
        assert_eq!(w.state().runtime, new);
    }
}

/// `gogo linux-app move` with snapshots (design 20 §9.1, #407), with the fake
/// backend: a rename carries them, a copy deletes them after a yes and takes
/// an `initial` snapshot where the target has snapshots, a crash at every new
/// step is recovered, and `--adopt` leaves them where they are.
mod snapshot_move {
    use std::sync::Arc;

    use gogo::linuxapp::rollback::{self as rb, Planned, RollbackOptions};
    use gogo::linuxapp::snapshots::{self as snap, Backends, SnapshotOptions};
    use gogo::macapp::doctor::Level;
    use gogo::macapp::launch::GameDirs;
    use gogo::macapp::relocate::{
        Faults, KilledAt, Method, MoveOptions, MoveOutcome, SnapshotAction, Target,
    };
    use gogo::snapshot::backend::{self, BackendKind, OnDisk, SnapshotBackend, Space, Support};
    use gogo::snapshot::fake::{Fake, FakeStep};

    use super::*;

    fn fake() -> (Arc<Fake>, Backends) {
        let f = Arc::new(Fake::new());
        (f.clone(), Backends::fixed(f))
    }

    fn with(o: LinuxAppOptions, b: &Backends) -> LinuxAppOptions {
        LinuxAppOptions {
            snapshots: SnapshotOptions::on(b.clone()),
            ..o
        }
    }

    /// A game built with snapshots on (1: `initial`) and a manual one (2),
    /// taken after `game/era.txt` said `then`; it says `now` since.
    fn built(name: &str, b: &Backends) -> World {
        let w = World::new(name, &["d3d9.dll"]);
        let out = build::execute(
            &build::plan(&with(w.opts(), b), None, &mut quiet()).unwrap(),
            None,
            &mut quiet(),
        )
        .unwrap();
        assert_eq!(out.snapshot.map(|m| m.id), Some(1));
        let root = w.game();
        std::fs::write(root.join("game/era.txt"), "then").unwrap();
        assert_eq!(
            snap::create(ID, b, Some("mine")).unwrap().snapshot.id,
            2,
            "the manual one"
        );
        std::fs::write(root.join("game/era.txt"), "now").unwrap();
        w
    }

    fn opts(faults: Faults, yes: bool) -> MoveOptions {
        MoveOptions {
            drop_snapshots: yes,
            faults,
            ..MoveOptions::default()
        }
    }

    fn across() -> Faults {
        Faults {
            across_filesystems: true,
            ..Faults::default()
        }
    }

    fn killed(at: KilledAt, across_fs: bool) -> Faults {
        Faults {
            killed_at: Some(at),
            across_filesystems: across_fs,
            ..Faults::default()
        }
    }

    fn go(b: &Backends, target: &Target, o: MoveOptions) -> Result<MoveOutcome, String> {
        gogo::linuxapp::relocate::move_game_with(ID, target, &o, b, &mut |_| {})
            .map_err(|e| e.to_string())
    }

    /// The game's snapshots, as `list` has them: `(id, presence)`, oldest
    /// first.
    fn snaps(b: &Backends) -> Vec<(u32, &'static str)> {
        let mut v: Vec<_> = snap::list(ID, b, 5, false)
            .unwrap()
            .snapshots
            .iter()
            .map(|s| (s.id, s.presence))
            .collect();
        v.sort();
        v
    }

    /// The snapshot numbers on disk beside `root`.
    fn on_disk(root: &Path) -> Vec<u32> {
        Fake::new()
            .on_disk(root)
            .unwrap_or_default()
            .iter()
            .map(|o| o.n)
            .collect()
    }

    fn journal(w: &World) -> PathBuf {
        w.home.join("game-moves").join(ID)
    }

    fn meta(w: &World) -> PathBuf {
        w.home.join("snapshots").join(ID)
    }

    // — one filesystem —

    #[test]
    fn a_rename_carries_the_snapshots_both_ways() {
        let (_, b) = fake();
        let w = built("snapmove-carry", &b);
        let home = w.game();
        let lib = w.library();
        let there = lib.dir().join(ID);
        let target = Target::Library(lib.clone());

        let plan =
            gogo::linuxapp::relocate::plan_with(ID, &target, &MoveOptions::default(), &b).unwrap();
        assert_eq!(plan.method, Method::Rename);
        let s = plan.snapshots.as_ref().unwrap();
        assert_eq!(s.action, SnapshotAction::Carry);
        assert_eq!(s.snapshots.iter().map(|s| s.id).collect::<Vec<_>>(), [2, 1]);
        assert!(!s.needs_yes() && !s.subvolume && !s.initial);
        assert!(s.lines(&there)[0].contains("2 snapshots move with the game"));

        // No yes needed: nothing is deleted.
        let out = go(&b, &target, MoveOptions::default()).unwrap();
        assert_eq!(out.snapshots.carried, [1, 2]);
        assert!(out.snapshots.deleted.is_empty() && out.snapshots.initial.is_none());
        assert!(out.warnings.is_empty(), "{out:?}");
        assert_eq!(on_disk(&there), [1, 2]);
        assert!(!backend::snapshots_dir(&home).unwrap().exists());
        assert!(!home.parent().unwrap().join(".gogo-snapshots").exists());
        assert_eq!(snaps(&b), [(1, "present"), (2, "present")]);
        // Where each was taken stays recorded: a rollback rewrites from it.
        let two = std::fs::read_to_string(meta(&w).join("2.toml")).unwrap();
        assert!(
            two.contains(&format!("game_dir = \"{}\"", home.display())),
            "{two}"
        );
        assert!(!journal(&w).exists());

        // And back.
        let out = go(&b, &Target::Default, MoveOptions::default()).unwrap();
        assert_eq!(out.snapshots.carried, [1, 2]);
        assert_eq!(on_disk(&home), [1, 2]);
        assert!(!backend::snapshots_dir(&there).unwrap().exists());
        assert_eq!(snaps(&b), [(1, "present"), (2, "present")]);
    }

    /// A rollback after a move on one filesystem: the snapshot names the old
    /// place, and its `launch.conf` is rewritten for the new one.
    #[test]
    fn a_rollback_after_a_rename_finds_the_game_at_its_new_place() {
        let (_, b) = fake();
        let w = built("snapmove-rollback", &b);
        let home = w.game();
        let lib = w.library();
        let there = lib.dir().join(ID);
        go(&b, &Target::Library(lib), MoveOptions::default()).unwrap();

        let o = RollbackOptions {
            keep_auto: 5,
            ..RollbackOptions::default()
        };
        let Planned::Plan(p) = rb::plan(ID, &b, 2, &o).unwrap() else {
            panic!("no plan");
        };
        let out = rb::rollback(ID, &b, 2, &p, &o).unwrap();
        assert_eq!(out.to, 2);
        assert_eq!(GameDirs::for_game(ID).unwrap().root, there);
        assert_eq!(
            std::fs::read_to_string(there.join("game/era.txt")).unwrap(),
            "then"
        );
        let conf = std::fs::read_to_string(there.join("launch.conf")).unwrap();
        for key in ["PREFIX", "GAME_DIR"] {
            assert!(
                conf_value(&conf, key).starts_with(&format!("'{}/", there.display())),
                "{key}: {conf}"
            );
        }
        assert!(!conf.contains(&home.display().to_string()), "{conf}");
        assert!(
            desktop_exec().contains(&there.join("launch.sh").display().to_string()),
            "{}",
            desktop_exec()
        );
        drop(w);
    }

    /// A rename killed at each new step: before any snapshot followed, after
    /// one, after both, before the commit and after it. Recovery finishes the
    /// move, and the snapshots are with the game.
    #[test]
    fn a_killed_rename_leaves_the_snapshots_with_the_game() {
        let (_, b) = fake();
        for (i, at) in [
            KilledAt::Carrying(0),
            KilledAt::Carrying(1),
            KilledAt::Carrying(2),
            KilledAt::BeforeCommit,
            KilledAt::BeforeCleanup,
        ]
        .into_iter()
        .enumerate()
        {
            let w = built(&format!("snapmove-carry-killed-{i}"), &b);
            let home = w.game();
            let there = w.library().dir().join(ID);
            go(
                &b,
                &Target::Library(w.library()),
                opts(killed(at, false), false),
            )
            .unwrap_err();
            assert!(!home.exists() && there.is_dir(), "{at:?}");
            let j = std::fs::read_to_string(journal(&w)).unwrap();
            assert!(j.ends_with("snapshots: carry\n"), "{at:?}: {j}");

            let lines = gogo::linuxapp::relocate::recover_with(ID, &b).unwrap();
            assert!(!lines.is_empty(), "{at:?}");
            assert_eq!(GameDirs::for_game(ID).unwrap().root, there, "{at:?}");
            assert_eq!(on_disk(&there), [1, 2], "{at:?}");
            assert!(!backend::snapshots_dir(&home).unwrap().exists(), "{at:?}");
            assert_eq!(snaps(&b), [(1, "present"), (2, "present")], "{at:?}");
            assert!(!journal(&w).exists(), "{at:?}");
            drop(w);
        }
    }

    // — across filesystems —

    #[test]
    fn a_copy_needs_a_yes_then_deletes_the_snapshots_and_takes_an_initial_one() {
        let (_, b) = fake();
        let w = built("snapmove-drop", &b);
        let home = w.game();
        let lib = w.library();
        let there = lib.dir().join(ID);
        let target = Target::Library(lib.clone());

        let plan =
            gogo::linuxapp::relocate::plan_with(ID, &target, &opts(across(), false), &b).unwrap();
        let s = plan.snapshots.clone().unwrap();
        assert_eq!(s.action, SnapshotAction::Drop);
        assert!(s.needs_yes() && s.subvolume && s.initial && s.unavailable.is_none());
        let lines = s.lines(&there);
        assert!(
            lines[0].contains("2 snapshots can't cross filesystems"),
            "{lines:?}"
        );
        assert!(
            lines[1].contains("  2  ") && lines[1].contains("mine"),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("`initial` snapshot")),
            "{lines:?}"
        );
        let json = serde_json::to_value(&plan).unwrap();
        assert_eq!(json["snapshots"]["action"], "drop");
        assert_eq!(json["snapshots"]["snapshots"][0]["id"], 2);

        // Without the yes: refused, nothing touched.
        let err = go(&b, &target, opts(across(), false)).unwrap_err();
        assert!(err.contains("--yes"), "{err}");
        assert!(home.is_dir() && !there.exists() && !journal(&w).exists());
        assert_eq!(on_disk(&home), [1, 2]);

        let out = go(&b, &target, opts(across(), true)).unwrap();
        assert!(out.warnings.is_empty(), "{out:?}");
        let mut deleted = out.snapshots.deleted.clone();
        deleted.sort();
        assert_eq!(deleted, [1, 2]);
        let initial = out.snapshots.initial.clone().unwrap();
        assert_eq!(initial.id, 3, "numbers aren't reused");
        assert_eq!(
            initial.label,
            format!("after moving to {}", lib.dir().display())
        );
        assert!(!home.exists());
        assert!(!backend::snapshots_dir(&home).unwrap().exists());
        assert!(!meta(&w).join("1.toml").exists() && !meta(&w).join("2.toml").exists());
        assert_eq!(snaps(&b), [(3, "present")]);
        assert_eq!(on_disk(&there), [3]);
        assert!(!journal(&w).exists());
        let lines = out.snapshots.lines();
        assert!(lines.iter().any(|l| l.contains("deleted")), "{lines:?}");
        assert!(
            lines.iter().any(|l| l.contains("snapshot 3 taken")),
            "{lines:?}"
        );
    }

    /// A copy killed at every step. Before the commit the game stays with its
    /// snapshots; after it, recovery deletes them and takes `initial` once.
    #[test]
    fn a_killed_copy_settles_the_snapshots_either_way() {
        let (_, b) = fake();
        let before_commit = [
            KilledAt::Copying(1),
            KilledAt::BeforeRename,
            KilledAt::BeforeCommit,
        ];
        let after_commit = [
            KilledAt::BeforeCleanup,
            KilledAt::Dropping(0),
            KilledAt::Dropping(1),
            KilledAt::BeforeInitial,
        ];
        for (i, at) in before_commit.into_iter().chain(after_commit).enumerate() {
            let w = built(&format!("snapmove-drop-killed-{i}"), &b);
            let home = w.game();
            let there = w.library().dir().join(ID);
            go(
                &b,
                &Target::Library(w.library()),
                opts(killed(at, true), true),
            )
            .unwrap_err();
            let j = std::fs::read_to_string(journal(&w)).unwrap();
            assert!(j.ends_with("snapshots: drop,initial\n"), "{at:?}: {j}");
            gogo::linuxapp::relocate::recover_with(ID, &b).unwrap();
            assert!(!journal(&w).exists(), "{at:?}");
            if before_commit.contains(&at) {
                assert_eq!(GameDirs::for_game(ID).unwrap().root, home, "{at:?}");
                assert!(!there.exists(), "{at:?}");
                assert_eq!(on_disk(&home), [1, 2], "{at:?}");
                assert_eq!(snaps(&b), [(1, "present"), (2, "present")], "{at:?}");
            } else {
                assert_eq!(GameDirs::for_game(ID).unwrap().root, there, "{at:?}");
                assert!(!home.exists(), "{at:?}");
                assert!(!backend::snapshots_dir(&home).unwrap().exists(), "{at:?}");
                assert_eq!(snaps(&b), [(3, "present")], "{at:?}");
            }
            drop(w);
        }
    }

    /// A crash after the `initial` snapshot and before the journal went: the
    /// next recovery takes no second one.
    #[test]
    fn recovery_after_the_initial_snapshot_takes_no_second_one() {
        let (_, b) = fake();
        let w = built("snapmove-initial-once", &b);
        let home = w.game();
        let there = w.library().dir().join(ID);
        go(&b, &Target::Library(w.library()), opts(across(), true)).unwrap();
        std::fs::write(
            journal(&w),
            format!(
                "{}\n{}\nsnapshots: drop,initial\n",
                home.display(),
                there.display()
            ),
        )
        .unwrap();
        gogo::linuxapp::relocate::recover_with(ID, &b).unwrap();
        assert_eq!(snaps(&b), [(3, "present")]);
        assert!(!journal(&w).exists());
    }

    /// A snapshot that won't delete after the commit: a warning naming it,
    /// `doctor` reports it, and the next settle deletes it.
    #[test]
    fn a_snapshot_that_wont_delete_is_a_warning_and_doctor_reports_it() {
        let (f, b) = fake();
        let w = built("snapmove-leftover", &b);
        let home = w.game();
        let there = w.library().dir().join(ID);
        let stuck = backend::snapshot_dir(&home, 2).unwrap();
        f.fail_at(FakeStep::Delete);
        let out = go(&b, &Target::Library(w.library()), opts(across(), true)).unwrap();
        assert_eq!(out.snapshots.deleted, [1]);
        assert_eq!(out.snapshots.left, std::slice::from_ref(&stuck));
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("couldn't be deleted")
                    && w.contains(&stuck.display().to_string())),
            "{out:?}"
        );
        assert!(stuck.is_dir());
        assert!(meta(&w).join("leftovers.toml").is_file(), "{out:?}");
        // Its metadata went: nothing can be rolled back to it from here.
        assert!(!meta(&w).join("2.toml").exists());
        let doctor = snap::doctor(ID, &there, &b);
        let f2 = doctor
            .iter()
            .find(|f| f.message.contains(&stuck.display().to_string()))
            .unwrap_or_else(|| panic!("{doctor:?}"));
        assert_eq!(f2.level, Level::Warn);
        assert!(f2.hint.as_deref().unwrap().contains("snapshot"), "{f2:?}");

        let lines = rb::settle_pending(ID, &b).unwrap();
        assert!(lines.iter().any(|l| l.contains("deleted")), "{lines:?}");
        assert!(!stuck.exists());
        assert!(!meta(&w).join("leftovers.toml").exists());
        assert_eq!(snaps(&b), [(3, "present")]);
        let doctor = snap::doctor(ID, &there, &b);
        assert!(
            !doctor
                .iter()
                .any(|f| f.message.contains(&stuck.display().to_string())),
            "{doctor:?}"
        );
    }

    /// The fake, except that dirs under `under` have no snapshots (an ext4
    /// library, say).
    struct NoneUnder(Fake, PathBuf);

    impl SnapshotBackend for NoneUnder {
        fn probe(&self, dir: &Path) -> Support {
            if dir.starts_with(&self.1) {
                Support::None(format!("{} is on ext4", dir.display()))
            } else {
                Support::Btrfs
            }
        }
        fn kind(&self) -> BackendKind {
            BackendKind::Btrfs
        }
        fn ready(&self, d: &Path) -> std::io::Result<bool> {
            Ok(!d.starts_with(&self.1) && self.0.ready(d)?)
        }
        fn make_game_dir(&self, d: &Path) -> std::io::Result<()> {
            self.0.make_game_dir(d)
        }
        fn snapshot(&self, d: &Path, n: u32) -> std::io::Result<()> {
            self.0.snapshot(d, n)
        }
        fn snapshot_path(&self, d: &Path, n: u32) -> Option<PathBuf> {
            self.0.snapshot_path(d, n)
        }
        fn exists(&self, d: &Path, n: u32) -> std::io::Result<bool> {
            self.0.exists(d, n)
        }
        fn delete(&self, d: &Path, n: u32) -> std::io::Result<()> {
            self.0.delete(d, n)
        }
        fn on_disk(&self, d: &Path) -> std::io::Result<Vec<OnDisk>> {
            self.0.on_disk(d)
        }
        fn space(&self, d: &Path, ns: &[u32]) -> std::io::Result<Vec<Space>> {
            self.0.space(d, ns)
        }
    }

    /// To a place without snapshots: they are deleted, the setting stays and
    /// does nothing there; back onto btrfs, the dir is a subvolume again and
    /// gets its `initial` snapshot.
    #[test]
    fn a_place_without_snapshots_keeps_the_setting_and_back_takes_an_initial_one() {
        let (_, b) = fake();
        let w = built("snapmove-none", &b);
        let home = w.game();
        let lib = w.library();
        let there = lib.dir().join(ID);
        let none = Backends::fixed(Arc::new(NoneUnder(
            Fake::subvolumes_only(),
            lib.dir().to_path_buf(),
        )));

        let plan = gogo::linuxapp::relocate::plan_with(
            ID,
            &Target::Library(lib.clone()),
            &opts(across(), false),
            &none,
        )
        .unwrap();
        let s = plan.snapshots.clone().unwrap();
        assert_eq!(s.action, SnapshotAction::Drop);
        assert!(!s.subvolume && !s.initial);
        assert!(s.unavailable.as_deref().unwrap().contains("ext4"), "{s:?}");
        let lines = s.lines(&there);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("unavailable") && l.contains("autosnapshot stays on")),
            "{lines:?}"
        );

        let out = go(&none, &Target::Library(lib), opts(across(), true)).unwrap();
        assert_eq!(out.snapshots.deleted.len(), 2);
        assert!(out.snapshots.initial.is_none());
        assert!(
            snap::list(ID, &none, 5, false)
                .unwrap()
                .snapshots
                .is_empty()
        );
        let settings = std::fs::read_to_string(meta(&w).join("settings.toml")).unwrap();
        assert!(settings.contains("auto = true"), "{settings}");

        // Back onto btrfs: a subvolume and an `initial` snapshot.
        let plan = gogo::linuxapp::relocate::plan_with(
            ID,
            &Target::Default,
            &opts(across(), false),
            &none,
        )
        .unwrap();
        let s = plan.snapshots.clone().unwrap();
        assert_eq!(s.action, SnapshotAction::None);
        assert!(s.subvolume && s.initial && !s.needs_yes());
        let out = go(&none, &Target::Default, opts(across(), false)).unwrap();
        assert_eq!(out.snapshots.initial.as_ref().map(|t| t.id), Some(3));
        assert!(none.for_dir(&home).1.ready(&home).unwrap(), "a subvolume");
        assert_eq!(on_disk(&home), [3]);
    }

    /// A btrfs target: the copy target is made a subvolume, so the moved game
    /// is one; with autosnapshot off it is still one, and no snapshot is
    /// taken.
    #[test]
    fn a_btrfs_target_gets_a_subvolume() {
        let f = Arc::new(Fake::subvolumes_only());
        let b = Backends::fixed(f.clone());
        let w = built("snapmove-subvol", &b);
        let there = w.library().dir().join(ID);
        let out = go(&b, &Target::Library(w.library()), opts(across(), true)).unwrap();
        assert!(f.ready(&there).unwrap(), "the moved dir is a subvolume");
        assert_eq!(out.snapshots.initial.as_ref().map(|t| t.id), Some(3));

        snap::set_auto(ID, &b, false, 5).unwrap();
        let plan =
            gogo::linuxapp::relocate::plan_with(ID, &Target::Default, &opts(across(), false), &b)
                .unwrap();
        let s = plan.snapshots.unwrap();
        assert!(s.subvolume && !s.initial, "{s:?}");
        let out = go(&b, &Target::Default, opts(across(), true)).unwrap();
        let home = GameDirs::for_game(ID).unwrap().root;
        assert!(f.ready(&home).unwrap());
        assert!(out.snapshots.initial.is_none());
        assert_eq!(out.snapshots.deleted, [3]);
        assert!(snap::list(ID, &b, 5, false).unwrap().snapshots.is_empty());
        drop(w);
    }

    /// `--adopt` moves no data: the snapshots stay beside the place they were
    /// taken, `doctor` says where, and `snapshot delete` drops their metadata.
    #[test]
    fn adopt_leaves_the_snapshots_and_doctor_says_where() {
        let (_, b) = fake();
        let w = built("snapmove-adopt", &b);
        let home = w.game();
        let lib = w.library();
        let there = lib.dir().join(ID);
        std::fs::create_dir_all(lib.dir()).unwrap();
        std::fs::rename(&home, &there).unwrap();
        let out = gogo::linuxapp::relocate::adopt_game_with(ID, &Target::Library(lib), &b).unwrap();
        assert!(out.warnings.is_empty(), "{out:?}");
        assert_eq!(on_disk(&home), [1, 2], "the snapshots stayed");
        assert_eq!(snaps(&b), [(1, "missing"), (2, "missing")]);

        let doctor = snap::doctor(ID, &there, &b);
        let unreachable: Vec<_> = doctor
            .iter()
            .filter(|f| f.message.contains("unreachable from the game's dir"))
            .collect();
        assert_eq!(unreachable.len(), 2, "{doctor:?}");
        assert!(
            unreachable[0]
                .message
                .contains(&backend::snapshots_dir(&home).unwrap().display().to_string()),
            "{unreachable:?}"
        );
        assert!(
            unreachable[0]
                .hint
                .as_deref()
                .unwrap()
                .contains(&format!("snapshot {ID} delete")),
            "{unreachable:?}"
        );

        snap::delete(ID, &b, &[1, 2]).unwrap();
        assert!(snaps(&b).is_empty());
        assert_eq!(
            on_disk(&home),
            [1, 2],
            "on disk, they are the user's to remove"
        );
        for n in [1, 2] {
            let _ = gogo::gog::materialize::remove_tree(&backend::snapshot_dir(&home, n).unwrap());
        }
    }

    // — review edge cases —

    /// Snapshots by the game's name already beside the target (an earlier
    /// `--adopt` left them): the plan refuses, naming the dir, for a rename
    /// and for a copy, and nothing is touched.
    #[test]
    fn snapshots_already_at_the_target_are_refused() {
        let (_, b) = fake();
        let w = built("snapmove-stale", &b);
        let home = w.game();
        let lib = w.library();
        let stale = backend::snapshots_dir(&lib.dir().join(ID)).unwrap();
        std::fs::create_dir_all(stale.join("3")).unwrap();
        std::fs::write(stale.join("3/theirs"), "x").unwrap();
        for o in [opts(Faults::default(), false), opts(across(), true)] {
            let target = Target::Library(lib.clone());
            let err = gogo::linuxapp::relocate::plan_with(ID, &target, &o, &b).unwrap_err();
            assert!(
                err.to_string().contains(&stale.display().to_string()),
                "{err}"
            );
            let err = go(&b, &target, o.clone()).unwrap_err();
            assert!(err.contains("already there"), "{err}");
            assert_eq!(GameDirs::for_game(ID).unwrap().root, home);
            assert_eq!(on_disk(&home), [1, 2]);
            assert!(stale.join("3/theirs").is_file());
            assert!(!journal(&w).exists());
        }
    }

    /// The yes covers the snapshots that were shown: when the move's own plan
    /// lists others, it is refused and says so.
    #[test]
    fn a_yes_covers_only_the_snapshots_shown() {
        let (_, b) = fake();
        let w = built("snapmove-consent", &b);
        let home = w.game();
        let target = Target::Library(w.library());
        let shown = |ids: Vec<u32>| MoveOptions {
            drop_snapshots: true,
            confirmed: Some(ids),
            faults: across(),
        };
        let err =
            gogo::linuxapp::relocate::move_game_with(ID, &target, &shown(vec![1]), &b, &mut |_| {})
                .unwrap_err();
        assert!(gogo::macapp::relocate::snapshots_changed(&err), "{err}");
        assert!(
            err.to_string().contains("delete 1, 2 where 1 were shown"),
            "{err}"
        );
        assert_eq!(GameDirs::for_game(ID).unwrap().root, home);
        assert_eq!(on_disk(&home), [1, 2]);
        assert!(!journal(&w).exists());

        let out = go(&b, &target, shown(vec![2, 1])).unwrap();
        assert_eq!(out.snapshots.deleted.len(), 2);
    }

    /// The old place gone after the commit (its drive unmounted): the deletes
    /// can't be confirmed, so the paths are kept as leftovers, `doctor` says
    /// their drive isn't there, and they are deleted once it is back.
    #[test]
    fn deletes_on_a_drive_that_went_away_are_kept_as_leftovers() {
        let (_, b) = fake();
        let w = built("snapmove-unmounted", &b);
        let home = w.game();
        let games = home.parent().unwrap().to_path_buf();
        let away = games.with_file_name("games-unmounted");
        let there = w.library().dir().join(ID);
        go(
            &b,
            &Target::Library(w.library()),
            opts(killed(KilledAt::BeforeCleanup, true), true),
        )
        .unwrap_err();
        std::fs::rename(&games, &away).unwrap();

        gogo::linuxapp::relocate::recover_with(ID, &b).unwrap();
        assert!(!journal(&w).exists());
        let left = std::fs::read_to_string(meta(&w).join("leftovers.toml")).unwrap();
        for n in [1, 2] {
            let p = backend::snapshot_dir(&home, n).unwrap();
            assert!(left.contains(&p.display().to_string()), "{left}");
        }
        let doctor = snap::doctor(ID, &there, &b);
        assert_eq!(
            doctor
                .iter()
                .filter(|f| f.message.contains("its drive isn't there"))
                .count(),
            2,
            "{doctor:?}"
        );
        // Still away: kept.
        rb::settle_pending(ID, &b).unwrap();
        assert!(meta(&w).join("leftovers.toml").is_file());

        // Back: deleted.
        std::fs::rename(&away, &games).unwrap();
        let lines = rb::settle_pending(ID, &b).unwrap();
        assert_eq!(
            lines.iter().filter(|l| l.contains("deleted")).count(),
            2,
            "{lines:?}"
        );
        assert!(!backend::snapshots_dir(&home).unwrap().exists());
        assert!(!meta(&w).join("leftovers.toml").exists());
    }

    /// A leftover where the game's own snapshots live now (it moved back
    /// beside it) is never deleted: a live snapshot may have its number.
    #[test]
    fn a_leftover_where_live_snapshots_are_is_never_deleted() {
        let (_, b) = fake();
        let w = built("snapmove-live", &b);
        let home = w.game();
        let live = backend::snapshot_dir(&home, 1).unwrap();
        std::fs::write(
            meta(&w).join("leftovers.toml"),
            format!("paths = [\"{}\"]\n", live.display()),
        )
        .unwrap();
        assert!(
            !snap::doctor(ID, &home, &b)
                .iter()
                .any(|f| f.message.contains(&live.display().to_string())),
        );
        let lines = rb::settle_pending(ID, &b).unwrap();
        assert!(!lines.iter().any(|l| l.contains("deleted")), "{lines:?}");
        assert!(live.is_dir());
        assert_eq!(on_disk(&home), [1, 2]);
        assert!(!meta(&w).join("leftovers.toml").exists());
    }

    // — real btrfs (the `snapshots-btrfs` job) —

    /// A move of a small built game with a snapshot to another dir on the same
    /// btrfs mount: a rename; the snapshots are read-only subvolumes beside
    /// the new place, and the game dir is still a subvolume.
    pub(super) fn real_btrfs_move() {
        let Some(w) = World::on_btrfs(
            "btrfs_move_carries_snapshots_on_one_filesystem",
            &["d3d9.dll"],
        ) else {
            return;
        };
        let real = Backends::real();
        let out = build::execute(
            &build::plan(&with(w.opts(), &real), None, &mut quiet()).unwrap(),
            None,
            &mut quiet(),
        )
        .unwrap();
        let home = out.dirs.root.clone();
        let lib = w.library();
        let there = lib.dir().join(ID);
        let guards = (Cleanup(home.clone()), Cleanup(there.clone()));
        std::fs::write(home.join("game/era.txt"), "then").unwrap();
        assert_eq!(
            snap::create(ID, &real, Some("mine")).unwrap().snapshot.id,
            2
        );
        std::fs::write(home.join("game/era.txt"), "now").unwrap();

        let target = Target::Library(lib);
        let plan = gogo::linuxapp::relocate::plan(ID, &target, &MoveOptions::default()).unwrap();
        assert_eq!(plan.method, Method::Rename, "{plan:?}");
        assert_eq!(
            plan.snapshots.as_ref().map(|s| s.action),
            Some(SnapshotAction::Carry)
        );
        let out =
            gogo::linuxapp::relocate::move_game(ID, &target, &MoveOptions::default(), &mut |_| {})
                .unwrap();
        assert_eq!(out.snapshots.carried, [1, 2], "{out:?}");
        assert!(gogo::snapshot::btrfs::is_subvolume(&there).unwrap());
        for n in [1, 2] {
            let s = backend::snapshot_dir(&there, n).unwrap();
            assert!(gogo::snapshot::btrfs::is_subvolume(&s).unwrap(), "{n}");
            assert!(gogo::snapshot::btrfs::is_readonly(&s).unwrap(), "{n}");
        }
        assert_eq!(
            std::fs::read_to_string(
                backend::snapshot_dir(&there, 2)
                    .unwrap()
                    .join("game/era.txt")
            )
            .unwrap(),
            "then"
        );
        assert!(!backend::snapshots_dir(&home).unwrap().exists());
        let listed = snap::list(ID, &real, 5, false).unwrap();
        assert!(
            listed.snapshots.iter().all(|s| s.presence == "present"),
            "{listed:?}"
        );

        let a = gogo::macapp::apps::get(ID, &w.root.join("logs"))
            .unwrap()
            .unwrap();
        let removed = gogo::macapp::apps::remove(&a, false).unwrap();
        assert_eq!(removed.snapshots, 2);
        assert!(!there.exists());
        drop(guards);
    }

    /// Whatever a failed btrfs test left: read-only snapshots aren't removed
    /// by the world's cleanup.
    struct Cleanup(PathBuf);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Some(dir) = backend::snapshots_dir(&self.0)
                && let Ok(rd) = std::fs::read_dir(&dir)
            {
                for e in rd.flatten() {
                    let _ = gogo::snapshot::btrfs::destroy_by_rmdir(&e.path());
                }
            }
            if gogo::snapshot::btrfs::is_subvolume(&self.0).unwrap_or(false) {
                let _ = gogo::snapshot::btrfs::destroy_by_rmdir(&self.0);
            }
        }
    }
}

#[test]
fn btrfs_move_carries_snapshots_on_one_filesystem() {
    snapshot_move::real_btrfs_move();
}
