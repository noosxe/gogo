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
printf 'ENV:WINEPREFIX=%s PROTONPATH=%s GAMEID=%s STORE=%s PROTONFIXES_DISABLE=%s CWD=%s\n' \
  "$WINEPREFIX" "$PROTONPATH" "$GAMEID" "$STORE" "$PROTONFIXES_DISABLE" "$(pwd)" >> "$log"
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

    // Before #177's launcher: no menu entry yet, and not orphaned.
    let a = &apps::list(&logs).unwrap()[0];
    assert!(a.linux && !a.launcher && !a.desktop_present && !a.orphaned());
    assert_eq!(
        a.runtime.as_deref(),
        Some(format!("path:{}", w.build.display()).as_str())
    );
    assert_eq!(a.title.as_deref(), Some("Test Game"));

    // With a launcher and its menu entry: present; entry deleted: orphaned.
    std::fs::write(w.game().join("launch.sh"), "#!/bin/bash\n").unwrap();
    std::fs::create_dir_all(desktop.parent().unwrap()).unwrap();
    std::fs::write(&desktop, "[Desktop Entry]\n").unwrap();
    assert!(!apps::get(ID, &logs).unwrap().unwrap().orphaned());
    std::fs::remove_file(&desktop).unwrap();
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
