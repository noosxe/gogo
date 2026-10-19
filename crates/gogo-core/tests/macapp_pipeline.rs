//! Integration: full `mac-app` pipeline against a **fake Wine runtime**
//! (docs/design/10 §7) — a shell script named `wine` that logs its argv and
//! simulates `--version` / `wineboot -u` / an Inno Setup silent install. No
//! network, no real Wine; asserts bundle layout, `launch.conf`, plist identity,
//! and installer argv exactness. Runs on every host (codesign skipped).

use gogo::engine::events::ProgressEvent;
use gogo::macapp::bundle::CodesignPolicy;
use gogo::macapp::{self, MacAppOptions, RuntimeMode};

/// Build a fake runtime whose `wine` logs every invocation to `log_path` and
/// simulates: `--version` → `wine-9.0-fake`; `wineboot -u` → ok; a `*.exe`
/// argument → lay down a GOG-style install (`goggame-1.info` + `game.exe`)
/// under the `/DIR=` target derived from the installer argv.
fn fake_runtime(root: &std::path::Path, log_path: &std::path::Path) {
    let script = format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
if [ "$1" = "--version" ]; then echo "wine-9.0-fake"; exit 0; fi
if [ "$1" = "wineboot" ]; then exit 0; fi
case "$1" in
  *.exe)
    TARGET=""
    for a in "$@"; do
      case "$a" in /DIR=*) TARGET="${{a#/DIR=}}";; esac
    done
    # "C:\GOG\test-game" → "$WINEPREFIX/drive_c/GOG/test-game"
    REL=$(printf '%s' "$TARGET" | sed 's|^C:\\||; s|\\|/|g')
    DIR="$WINEPREFIX/drive_c/$REL"
    mkdir -p "$DIR"
    printf '%s' '{{"name":"Test Game","playTasks":[{{"type":"FileTask","category":"game","path":"game.exe","isPrimary":true}}]}}' > "$DIR/goggame-1.info"
    printf 'MZ' > "$DIR/game.exe"
    ;;
esac
exit 0
"#,
        log = log_path.display()
    );
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("wine"), script).unwrap();
    // wineserver: logs, terminates on -k, reports "not running" on -p.
    let ws = format!(
        r#"#!/bin/sh
echo "wineserver $*" >> "{log}"
if [ "$1" = "-k" ]; then exit 0; fi
if [ "$1" = "-p" ]; then exit 3; fi
exit 0
"#,
        log = log_path.display()
    );
    std::fs::write(bin.join("wineserver"), ws).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            bin.join("wineserver"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join("wine"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn full_pipeline_with_fake_runtime() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime_dir = tmp.path().join("runtime");
    let log = tmp.path().join("wine.log");
    fake_runtime(&runtime_dir, &log);

    // A standalone "installer": only used as an argv token by the fake wine.
    let installers = tmp.path().join("installers");
    std::fs::create_dir_all(&installers).unwrap();
    let installer = installers.join("setup_testgame_1.0.exe");
    std::fs::write(&installer, b"MZ fake").unwrap();
    std::fs::write(installers.join("setup_testgame_1.0-1.bin"), b"part").unwrap();

    let out = tmp.path().join("out");
    let opts = MacAppOptions {
        installer: installer.clone(),
        name: Some("Test Game".into()),
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir.clone()),
        out_dir: out.clone(),
        overwrite: true,
        recipe: Default::default(),
    };

    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = events.clone();
    let mut emit = move |ev: &ProgressEvent| {
        sink.lock().unwrap().push(match ev {
            ProgressEvent::RuntimeResolved { .. } => "RuntimeResolved".to_string(),
            ProgressEvent::PrefixStaged => "PrefixStaged".to_string(),
            ProgressEvent::InstallStarted => "InstallStarted".to_string(),
            ProgressEvent::InstallFinished { .. } => "InstallFinished".to_string(),
            ProgressEvent::BundleAssembled => "BundleAssembled".to_string(),
            ProgressEvent::Codesigned => "Codesigned".to_string(),
            _ => "other".to_string(),
        });
    };

    let plan = macapp::plan(&opts, &mut emit).expect("plan");
    assert_eq!(plan.display, "Test Game");
    assert_eq!(plan.slug, "test-game");
    assert_eq!(plan.runtime.version, "wine-9.0-fake");
    assert_eq!(plan.installer.bin_parts.len(), 1);

    let bytes = macapp::execute(&plan, CodesignPolicy::Skip, None, &mut emit).expect("execute");
    assert!(bytes > 0);

    // Bundle layout (design 10 §2.5–2.7).
    let bundle = out.join("Test Game.app");
    assert!(bundle.join("Contents/MacOS/Test Game").is_file());
    assert!(bundle.join("Contents/Info.plist").is_file());
    assert!(bundle.join("Contents/Resources/launch.conf").is_file());
    assert!(bundle.join("Contents/Resources/wine/bin/wine").is_file());
    assert!(bundle.join("Contents/Resources/LICENSE-wine.txt").is_file());
    assert!(bundle.join("Contents/Resources/runtime.txt").is_file());
    assert!(!bundle.join("Contents/Resources/README-GPTK.txt").exists());

    // Plist identity: slug in the identifier, display name in Executable.
    let plist = std::fs::read_to_string(bundle.join("Contents/Info.plist")).unwrap();
    assert!(plist.contains("<string>com.noosxe.gogo.test-game</string>"));
    assert!(plist.contains("<key>CFBundleExecutable</key>"));
    assert!(plist.contains("<string>Test Game</string>"));

    // launch.conf points inside the bundle (wine binary + staged prefix + exe).
    let conf = std::fs::read_to_string(bundle.join("Contents/Resources/launch.conf")).unwrap();
    let wine_line = conf.lines().find(|l| l.starts_with("WINE=")).unwrap();
    assert!(wine_line.contains("Resources/wine/bin/wine"), "{conf}");
    assert!(conf.contains("Resources/prefix"), "{conf}");
    assert!(conf.contains("drive_c/GOG/test-game/game.exe"), "{conf}");

    // Installer argv exactness (design 10 §2.3): wineboot first, then the
    // silent Inno Setup invocation with the deterministic Windows target.
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(logged.contains("wineboot -u"), "{logged}");
    assert!(logged.contains("wineserver -k"), "{logged}");
    assert!(
        logged.contains(&installer.display().to_string()),
        "{logged}"
    );
    assert!(logged.contains("/VERYSILENT"), "{logged}");
    assert!(logged.contains("/SUPPRESSMSGBOXES"), "{logged}");
    assert!(logged.contains("/NORESTART"), "{logged}");
    assert!(logged.contains(r"/DIR=C:\GOG\test-game"), "{logged}");

    // The game landed in the bundled prefix via the fake installer.
    assert!(
        bundle
            .join("Contents/Resources/prefix/drive_c/GOG/test-game/game.exe")
            .is_file()
    );

    // Event sequence (design 10 §6).
    let seen = events.lock().unwrap();
    let expected = [
        "RuntimeResolved",
        "PrefixStaged",
        "InstallStarted",
        "InstallFinished",
        "BundleAssembled",
        "Codesigned",
    ];
    for tag in expected {
        assert!(seen.iter().any(|t| t == tag), "missing {tag} in {seen:?}");
    }
}

#[test]
fn execute_refuses_to_clobber_existing_bundle() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime_dir = tmp.path().join("runtime");
    let log = tmp.path().join("wine.log");
    fake_runtime(&runtime_dir, &log);
    let installer = tmp.path().join("setup_testgame_1.0.exe");
    std::fs::write(&installer, b"MZ").unwrap();

    let opts = MacAppOptions {
        installer,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir),
        out_dir: tmp.path().to_path_buf(),
        overwrite: true,
        recipe: Default::default(),
    };
    let plan = macapp::plan(&opts, &mut |_| {}).expect("plan");
    // Pre-create the bundle root: execute must refuse.
    std::fs::create_dir_all(&plan.bundle.root).unwrap();
    let err = macapp::execute(&plan, CodesignPolicy::Skip, None, &mut |_| {}).unwrap_err();
    assert!(matches!(
        err,
        gogo::macapp::MacAppError::BundleExists { .. }
    ));

    // plan() without overwrite also refuses on an existing bundle.
    let opts2 = MacAppOptions {
        overwrite: false,
        ..opts
    };
    let err = macapp::plan(&opts2, &mut |_| {}).unwrap_err();
    assert!(matches!(
        err,
        gogo::macapp::MacAppError::BundleExists { .. }
    ));
}

/// Env-mutation lock for tests that set GOGO_HOME/WINETRICKS (integration
/// tests cannot reach the core crate's env mutex).
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn winetricks_verbs_run_with_wine_shims_and_notice() {
    // Design 12 §8 composition: a recipe winetricks verb runs through the
    // preference chain with WINE=<bundled wine>, the shim dir prepended to
    // PATH (child-scoped), and produces the bundle's LGPL notice.
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let runtime_dir = tmp.path().join("runtime");
    let log = tmp.path().join("wine.log");
    fake_runtime(&runtime_dir, &log);
    let wt_log = tmp.path().join("winetricks.log");
    let wt = tmp.path().join("fake-winetricks");
    std::fs::write(
        &wt,
        format!(
            "#!/bin/sh\nprintf 'argv: %s\\nWINE=%s\\nPATH=%s\\n' \"$*\" \"$WINE\" \"$PATH\" >> \"{}\"\nexit 0\n",
            wt_log.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&wt, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // A recipe file whose only step is a winetricks verb (design 11 §3.1).
    let recipe = tmp.path().join("recipe.json");
    std::fs::write(
        &recipe,
        r#"{"files": [], "installer": [{"task": {"name": "winetricks", "app": "corefonts"}}]}"#,
    )
    .unwrap();

    let installer = tmp.path().join("setup_testgame_1.0.exe");
    std::fs::write(&installer, b"MZ").unwrap();
    let opts = MacAppOptions {
        installer,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir),
        out_dir: tmp.path().to_path_buf(),
        overwrite: true,
        recipe: gogo::macapp::RecipeChoice::File(recipe),
    };

    // Sandbox the store (shims materialize under state_dir) + override the
    // winetricks resolution with our fake.
    // SAFETY: env mutation under ENV_LOCK; no other test in this binary
    // reads GOGO_HOME/WINETRICKS concurrently.
    unsafe {
        std::env::set_var("GOGO_HOME", tmp.path().join("home"));
        std::env::set_var("WINETRICKS", &wt);
    }
    let plan = macapp::plan(&opts, &mut |_| {}).expect("plan");
    macapp::execute(&plan, CodesignPolicy::Skip, None, &mut |_| {}).expect("execute");
    // SAFETY: restore.
    unsafe {
        std::env::remove_var("WINETRICKS");
        std::env::remove_var("GOGO_HOME");
    }
    drop(guard);

    let logged = std::fs::read_to_string(&wt_log).expect("fake winetricks ran");
    assert!(logged.contains("argv: -q corefonts"), "{logged}");
    assert!(
        logged.contains("Resources/wine/bin/wine"),
        "WINE must be the bundled wine: {logged}"
    );
    assert!(
        logged.contains("tools/shims"),
        "shim dir must be PATH-prepended: {logged}"
    );
    // The LGPL notice lands in the bundle (design 12 §8).
    let bundle = tmp.path().join("Test Game.app");
    assert!(
        bundle
            .join("Contents/Resources/LICENSE-winetricks.txt")
            .is_file()
    );
}
