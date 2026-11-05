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
use gogo::linuxapp::setup::{self, SetupOptions};
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

impl World {
    fn new(name: &str, imports: &[&str]) -> World {
        let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
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

    fn game(&self) -> PathBuf {
        self.home.join("games").join(ID)
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

// — after the build: compat, next-renderer, --retarget (#176) —

#[test]
fn compat_next_renderer_and_retarget_work_on_the_linux_layers() {
    use gogo::linuxapp::manage;
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
} >> "$FAKE_UMU_LOG"
echo "fake-game stdout line"
echo "err: fake-game stderr line" >&2
if [ -n "${FAKE_GAME_HANG:-}" ]; then
  setsid sh -c 'trap "" TERM; exec sleep 300' </dev/null >/dev/null 2>&1 &
  WINEPREFIX="$WINEPREFIX/pfx/" setsid sh -c 'trap "" TERM; exec sleep 300' </dev/null >/dev/null 2>&1 &
  STEAM_COMPAT_DATA_PATH="$WINEPREFIX" setsid env -u WINEPREFIX sh -c 'trap "" TERM; exec sleep 300' </dev/null >/dev/null 2>&1 &
  exec sleep 300
fi
sleep "${FAKE_GAME_SLEEP:-0}"
exit "${FAKE_GAME_EXIT:-0}"
"#;

/// The fake gogo. `setup` restamps `launch.conf` (and clears a vanished
/// `UMU_PYTHON`, and puts `FAKE_VERSION` back into `FAKE_VERSION_FILE`, as
/// a reinstall would); `FAKE_SETUP_FAIL` fails it. `next-renderer` answers
/// wined3d until L4 names it.
const FAKE_GOGO: &str = r#"#!/bin/sh
echo "GOGO_HOME=${GOGO_HOME:-} $*" >> "$FAKE_CALLS"
g="$GOGO_HOME/games/$3"
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
    "sh",
    "cat",
    "date",
    "mkdir",
    "rm",
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
        let w = World::launcher_built(name, no_gogo);
        let tools = w.root.join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        for (t, p) in LAUNCHER_TOOLS.iter().zip(found) {
            std::os::unix::fs::symlink(p.unwrap(), tools.join(t)).unwrap();
        }
        Some(w)
    }

    /// [`World::launcher`]'s game without the tools: for what setup writes.
    fn launcher_built(name: &str, no_gogo: bool) -> World {
        let w = World::new(name, &["d3d9.dll"]);
        std::fs::write(w.tree.join(format!("goggame-{ID}.ico")), tiny_ico()).unwrap();
        let gogo = w
            .root
            .join(if no_gogo { "no-such-gogo" } else { "fake-gogo" });
        if !no_gogo {
            std::fs::write(&gogo, FAKE_GOGO).unwrap();
            chmod_x(&gogo);
        }
        let mut opts = w.opts();
        opts.gogo_path = Some(gogo);
        build_with(&opts);
        let umu = w.root.join("bin/umu-run");
        std::fs::write(&umu, FAKE_GAME_UMU).unwrap();
        chmod_x(&umu);
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
        script.replace("LAUNCHER_VERSION=2", "LAUNCHER_VERSION=0"),
    )
    .unwrap();
    std::fs::write(&desktop, "[Desktop Entry]\nExec=/old/root/launch.sh\n").unwrap();
    let out = setup::setup(ID, &SetupOptions::default(), &mut quiet()).unwrap();
    assert_eq!(std::fs::read_to_string(&sh).unwrap(), script);
    assert_eq!(std::fs::read_to_string(&desktop).unwrap(), entry);
    assert!(
        out.notes.iter().any(|n| n == "launch.sh: launcher 0 -> 2"),
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
        "# launcher: 2\n".into(),
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

#[test]
fn teardown_on_term_kills_every_process_with_the_prefix() {
    let Some(w) = World::launcher("teardown", true) else {
        return;
    };
    let mut child = w
        .launch_cmd(&[("FAKE_GAME_HANG", "1")], &[])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // The game and its TERM-proof children of other sessions.
    assert!(
        wait_until(10, || prefix_procs(&w).len() >= 4),
        "{:?}",
        prefix_procs(&w)
    );
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
    assert_eq!(code, Some(143));
    assert!(prefix_procs(&w).is_empty(), "{:?}", prefix_procs(&w));
    let log = w.launch_log();
    assert!(log.contains("got SIGTERM"), "{log}");
    assert!(log.contains("reaped (SIGKILL): "), "{log}");
    assert!(log.contains("sleep;"), "{log}");
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
    assert!(wait_until(10, || prefix_procs(&w).len() >= 4));
    assert!(wait_until(5, || w.launch_log().contains("exec umu-run")));
    let pid = child.id();
    assert!(w.game().join(format!(".launchers/{pid}")).is_file());
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
