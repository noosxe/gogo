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
printf 'ENV:%s\n' "$WINEPREFIX" >> "{log}"
printf 'CWD:%s\n' "$(pwd)" >> "{log}"
if [ "$1" = "--version" ]; then echo "wine-9.0-fake"; exit 0; fi
if [ "$1" = "wineboot" ]; then
  mkdir -p "$WINEPREFIX/drive_c" "$WINEPREFIX/dosdevices"
  ln -sfn ../drive_c "$WINEPREFIX/dosdevices/c:"
  ln -sfn / "$WINEPREFIX/dosdevices/z:"
  # What real Wine adds (#55): home links into the builder's $HOME and a
  # drive letter per mounted volume.
  mkdir -p "$WINEPREFIX/drive_c/users/${{USER:-u}}/AppData/Roaming"
  ln -sfn "$HOME/Documents" "$WINEPREFIX/drive_c/users/${{USER:-u}}/Documents"
  ln -sfn "$HOME/Templates" "$WINEPREFIX/drive_c/users/${{USER:-u}}/AppData/Roaming/Templates"
  ln -sfn /Volumes/Other "$WINEPREFIX/dosdevices/d:"
  exit 0
fi
case "$1" in
  *fail_support.exe) exit 1;;
  # A v2 launcher run (a Windows path). FAKE_WINE_HANG=<pidfile>: hang, with
  # a TERM-ignoring child in the game's process group (#42 teardown test).
  C:*)
    printf 'DLLOV:%s\n' "${{WINEDLLOVERRIDES:-}}" >> "{log}"
    if [ -n "${{FAKE_WINE_HANG:-}}" ]; then
      (trap '' TERM; exec sleep 300) &
      echo $! > "$FAKE_WINE_HANG"
      sleep 300
    fi
    # FAKE_WINE_EXIT=<n>: chatter on both streams, then exit n (#77 run log).
    if [ -n "${{FAKE_WINE_EXIT:-}}" ]; then
      sleep "${{FAKE_WINE_SLEEP:-0}}"
      echo "fake-wine stdout line"
      echo "fake-wine stderr line" >&2
      exit "$FAKE_WINE_EXIT"
    fi
    exit 0;;
esac
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
    # Only seed the goggame info when the target has none: a depot-lane clone
    # already carries the real one, and a second info would hijack entry
    # discovery nondeterministically (dir-walk order; live CI finding).
    ls "$DIR"/goggame-*.info >/dev/null 2>&1 || printf '%s' '{{"name":"Test Game","playTasks":[{{"type":"FileTask","category":"game","path":"game.exe","isPrimary":true}}]}}' > "$DIR/goggame-1.info"
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
        installer: Some(installer.clone()),
        game_tree: None,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir.clone()),
        out_dir: out.clone(),
        overwrite: true,
        recipe: Default::default(),
        runtime_menu: false,
        runtime_version: None,
        lutris_base: None,
        renderer: macapp::RendererChoice::Auto,
        portable: true,
        gogo_path: None,
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

    let plan = macapp::plan(&opts, None, &mut emit).expect("plan");
    assert_eq!(plan.display, "Test Game");
    assert_eq!(plan.slug, "test-game");
    assert_eq!(plan.runtime.version, "wine-9.0-fake");
    assert_eq!(plan.installer.as_ref().unwrap().bin_parts.len(), 1);

    let bytes = macapp::execute(&plan, CodesignPolicy::Skip, &mut emit).expect("execute");
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
        installer: Some(installer),
        game_tree: None,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir),
        out_dir: tmp.path().to_path_buf(),
        overwrite: true,
        recipe: Default::default(),
        runtime_menu: false,
        runtime_version: None,
        lutris_base: None,
        renderer: macapp::RendererChoice::Auto,
        portable: true,
        gogo_path: None,
    };
    let plan = macapp::plan(&opts, None, &mut |_| {}).expect("plan");
    // Pre-create the bundle root: execute must refuse.
    std::fs::create_dir_all(&plan.bundle.root).unwrap();
    let err = macapp::execute(&plan, CodesignPolicy::Skip, &mut |_| {}).unwrap_err();
    assert!(matches!(
        err,
        gogo::macapp::MacAppError::BundleExists { .. }
    ));

    // plan() without overwrite also refuses on an existing bundle.
    let opts2 = MacAppOptions {
        overwrite: false,
        ..opts
    };
    let err = macapp::plan(&opts2, None, &mut |_| {}).unwrap_err();
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
        installer: Some(installer),
        game_tree: None,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir),
        out_dir: tmp.path().to_path_buf(),
        overwrite: true,
        recipe: gogo::macapp::RecipeChoice::File(recipe),
        runtime_menu: false,
        runtime_version: None,
        lutris_base: None,
        renderer: macapp::RendererChoice::Auto,
        portable: true,
        gogo_path: None,
    };

    // Sandbox the store (shims materialize under state_dir) + override the
    // winetricks resolution with our fake.
    // SAFETY: env mutation under ENV_LOCK; no other test in this binary
    // reads GOGO_HOME/WINETRICKS concurrently.
    unsafe {
        std::env::set_var("GOGO_HOME", tmp.path().join("home"));
        std::env::set_var("WINETRICKS", &wt);
    }
    let plan = macapp::plan(&opts, None, &mut |_| {}).expect("plan");
    macapp::execute(&plan, CodesignPolicy::Skip, &mut |_| {}).expect("execute");
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

#[test]
fn recipe_wine_pin_recommends_cached_catalog_runtime() {
    // Design 12 §6-§7: a recipe's wine pin (D6 advisory) drives the auto
    // runtime ladder to the recommendation; a cached store entry satisfies
    // it without brew or network.
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    // A fake installer + runtime for the pipeline inputs.
    let runtime_dir = tmp.path().join("runtime");
    let log = tmp.path().join("wine.log");
    fake_runtime(&runtime_dir, &log);
    let installer = tmp.path().join("setup_testgame_1.0.exe");
    std::fs::write(&installer, b"MZ").unwrap();

    // A cached winecx store entry (manifest + probeable tree in design 16's
    // `{engine,Frameworks}` layout).
    let home = tmp.path().join("home");
    let tree = home.join("runtimes/winecx/24.0.7_7/engine/bin");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::create_dir_all(home.join("runtimes/winecx/24.0.7_7/Frameworks")).unwrap();
    std::fs::write(
        tree.join("wine"),
        "#!/bin/sh\necho 'wine-9.0 (SikarugirCX 24.0.7)'\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(tree.join("wine"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    std::fs::write(
        home.join("runtimes/manifest.json"),
        r#"[{"id":"winecx","version":"24.0.7_7","sha256":"0","fetched_at":"2026-01-01T00:00:00Z","upstream_url":"x","entry_rel":"engine/bin/wine","bytes":16}]"#,
    )
    .unwrap();

    // The recipe fixture with a Linux wine pin (design 11 §8 shape).
    let recipe = tmp.path().join("recipe.json");
    std::fs::write(
        &recipe,
        r#"{"files": [], "installer": [], "wine": {"version": "lutris-7.2-2"}}"#,
    )
    .unwrap();

    let opts = MacAppOptions {
        installer: Some(installer),
        game_tree: None,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Auto,
        runtime_path: None,
        out_dir: tmp.path().to_path_buf(),
        overwrite: true,
        recipe: gogo::macapp::RecipeChoice::File(recipe),
        runtime_menu: false,
        runtime_version: None,
        lutris_base: None,
        renderer: macapp::RendererChoice::Auto,
        portable: true,
        gogo_path: None,
    };

    let mut warns: Vec<String> = Vec::new();
    // SAFETY: env mutation under ENV_LOCK (established pattern above).
    unsafe {
        std::env::set_var("GOGO_HOME", &home);
    }
    let plan = macapp::plan(&opts, None, &mut |ev| {
        if let ProgressEvent::Warn { message } = ev {
            warns.push(message.clone());
        }
    })
    .expect("plan");
    // SAFETY: restore.
    unsafe {
        std::env::remove_var("GOGO_HOME");
    }
    drop(guard);

    let sel = plan.recipe.as_ref().expect("recipe selected at plan time");
    assert_eq!(sel.wine_version.as_deref(), Some("lutris-7.2-2"));
    assert!(!sel.from_search, "file recipes are pinned, not searched");
    // The ladder: recommendation (the default track, winecx) satisfied from
    // the store cache.
    assert_eq!(plan.runtime.version, "wine-9.0 (SikarugirCX 24.0.7)");
    assert_eq!(plan.runtime.mode, RuntimeMode::Catalog("winecx".into()));
    assert_eq!(
        macapp::launch::frameworks_of(&plan.runtime),
        Some(home.join("runtimes/winecx/24.0.7_7/Frameworks"))
    );
    assert_eq!(plan.runtime_downloaded, None);
    assert!(
        warns
            .iter()
            .any(|w| w.contains("recipe wine pin") && w.contains("Linux")),
        "advisory notice missing: {warns:?}"
    );
}

#[test]
fn auto_recipe_discovery_and_gogid_crosscheck() {
    // Design 12 §6: auto discovery at plan time (search by display name),
    // then the authoritative gogid cross-check post-install — a mismatching
    // pre-selected script downgrades to RecipeMiss + recipe-less continuation.
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let runtime_dir = tmp.path().join("runtime");
    let log = tmp.path().join("wine.log");
    fake_runtime(&runtime_dir, &log);
    let installer = tmp.path().join("setup_testgame_1.0.exe");
    std::fs::write(&installer, b"MZ").unwrap();

    // One runtime keeps the wiremock server alive across plan+execute.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let server = rt.block_on(async {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, ResponseTemplate};
        let server = wiremock::MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/games"))
            .and(query_param("search", "Test Game"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "count": 1,
                "results": [{
                    "id": 1, "slug": "test-game", "name": "Test Game",
                    "provider_games": [{"slug": "1", "service": "gog"}]
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/games/test-game"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<a href="/api/installers/test-game-gog?format=json">Download script</a>"#,
            ))
            .mount(&server)
            .await;
        // gogid 999 mismatches the installed game's goggame-1.info (id 1).
        Mock::given(method("GET"))
            .and(path("/api/installers/test-game-gog"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "count": 1,
                "results": [{
                    "slug": "test-game-gog", "game_slug": "test-game",
                    "name": "Test Game", "version": "1.0", "runner": "wine",
                    "user": "alice", "updated_at": "2026-01-01", "gogid": 999,
                    "script": {
                        "files": [],
                        "installer": [],
                        "game": {"exe": "drive_c/GOG/test-game/other.exe"}
                    }
                }]
            })))
            .mount(&server)
            .await;
        server
    });

    let opts = MacAppOptions {
        installer: Some(installer),
        game_tree: None,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir),
        out_dir: tmp.path().to_path_buf(),
        overwrite: true,
        recipe: gogo::macapp::RecipeChoice::Auto,
        runtime_menu: false,
        runtime_version: None,
        lutris_base: Some(server.uri()),
        renderer: macapp::RendererChoice::Auto,
        portable: true,
        gogo_path: None,
    };

    let mut tags: Vec<&'static str> = Vec::new();
    let mut miss_keys: Vec<String> = Vec::new();
    let mut record = |ev: &ProgressEvent| match ev {
        ProgressEvent::RecipeResolved { .. } => tags.push("RecipeResolved"),
        ProgressEvent::RecipeMiss { key } => miss_keys.push(key.clone()),
        _ => {}
    };
    let plan = macapp::plan(&opts, None, &mut record).expect("plan");
    let sel = plan
        .recipe
        .as_ref()
        .expect("auto discovery selected a script");
    assert!(sel.from_search);
    assert_eq!(sel.installer.slug, "test-game-gog");
    macapp::execute(&plan, CodesignPolicy::Skip, &mut record).expect("execute");
    drop(record);
    assert!(
        tags.contains(&"RecipeResolved"),
        "plan-time resolution: {tags:?}"
    );
    assert!(
        miss_keys
            .iter()
            .any(|k| k.contains("999") && k.contains("1")),
        "gogid mismatch must downgrade: {miss_keys:?}"
    );
    // Recipe-less continuation: the default entry point, not the recipe's exe.
    let conf = std::fs::read_to_string(
        tmp.path()
            .join("Test Game.app/Contents/Resources/launch.conf"),
    )
    .unwrap();
    assert!(conf.contains("game.exe"), "{conf}");
    assert!(!conf.contains("other.exe"), "{conf}");
    drop(guard);
}

#[test]
fn runtime_menu_offers_cached_and_custom_path_lanes() {
    // §5/§9: with nothing auto-resolvable (no cached ladder hit, no brew on
    // the scrubbed PATH), the injected renderer gets the runtime menu; a
    // cached pick resolves from the store, and the custom-path lane
    // re-prompts on invalid input then accepts a valid tree.
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let runtime_dir = tmp.path().join("runtime");
    let log = tmp.path().join("wine.log");
    fake_runtime(&runtime_dir, &log);
    let installer = tmp.path().join("setup_testgame_1.0.exe");
    std::fs::write(&installer, b"MZ").unwrap();

    // A cached wine-devel store entry.
    let home = tmp.path().join("home");
    let tree = home.join("runtimes/wine-devel/1.2/bin");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("wine"), "#!/bin/sh\necho wine-1.2-devel\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(tree.join("wine"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    std::fs::write(
        home.join("runtimes/manifest.json"),
        r#"[{"id":"wine-devel","version":"1.2","sha256":"0","fetched_at":"2026-01-01T00:00:00Z","upstream_url":"x","entry_rel":"bin/wine","bytes":16}]"#,
    )
    .unwrap();

    // Answers: first the menu → custom… path (invalid), then the Input
    // re-prompt → the valid fake runtime dir. Asserts the re-prompt loop.
    let valid = runtime_dir.clone();
    let seen: std::sync::Arc<std::sync::Mutex<Vec<gogo::macapp::PromptSpec>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    let prompter: std::sync::Arc<gogo::macapp::PromptFn> =
        std::sync::Arc::new(move |spec: &gogo::macapp::PromptSpec| {
            seen2.lock().unwrap().push(spec.clone());
            match spec.kind {
                gogo::macapp::PromptKind::Select => Some(gogo::macapp::PromptAnswer::Other(
                    "/nonexistent-runtime".into(),
                )),
                gogo::macapp::PromptKind::Input => Some(gogo::macapp::PromptAnswer::Other(
                    valid.display().to_string(),
                )),
                gogo::macapp::PromptKind::Confirm { .. } => {
                    Some(gogo::macapp::PromptAnswer::Confirmed(false))
                }
                _ => None,
            }
        });

    let opts = MacAppOptions {
        installer: Some(installer),
        game_tree: None,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Auto,
        runtime_path: None,
        out_dir: tmp.path().to_path_buf(),
        overwrite: true,
        recipe: gogo::macapp::RecipeChoice::None,
        runtime_menu: true,
        runtime_version: None,
        lutris_base: None,
        renderer: macapp::RendererChoice::Auto,
        portable: true,
        gogo_path: None,
    };

    let old_path = std::env::var("PATH").unwrap_or_default();
    // SAFETY: env mutation under ENV_LOCK (established pattern above).
    unsafe {
        std::env::set_var("GOGO_HOME", &home);
        std::env::set_var("PATH", "/usr/bin:/bin"); // hide brew → ladder reaches the menu
    }
    let plan = macapp::plan(&opts, Some(prompter.as_ref()), &mut |_| {}).expect("plan");
    // SAFETY: restore.
    unsafe {
        std::env::set_var("PATH", &old_path);
        std::env::remove_var("GOGO_HOME");
    }
    drop(guard);

    // The custom-path lane won after one re-prompt. probe() canonicalizes
    // the binary (/tmp → /private/tmp on macOS), so compare canonical forms.
    assert_eq!(plan.runtime.mode, RuntimeMode::Path);
    let canon = runtime_dir.canonicalize().unwrap();
    assert!(
        plan.runtime.wine.starts_with(&canon),
        "{} !~ {}",
        plan.runtime.wine.display(),
        canon.display()
    );
    let specs = seen.lock().unwrap();
    let menu = specs
        .iter()
        .find(|sp| matches!(sp.kind, gogo::macapp::PromptKind::Select))
        .expect("menu prompted");
    assert!(menu.allow_other, "custom-path entry offered");
    assert!(!menu.allow_none);
    assert!(
        menu.items
            .iter()
            .any(|i| i.contains("wine-devel 1.2 (✓ cached)")),
        "cached flavors lead the menu: {:?}",
        menu.items
    );
    assert!(
        menu.items
            .iter()
            .any(|i| i.starts_with("download wine-staging")),
        "downloadable flavors offered: {:?}",
        menu.items
    );
    assert!(
        menu.items
            .iter()
            .any(|i| i.starts_with("download winecx") && i.contains("recommended")),
        "the default track (winecx) is marked recommended"
    );
    let reprompt = specs
        .iter()
        .find(|sp| matches!(sp.kind, gogo::macapp::PromptKind::Input))
        .expect("re-prompt");
    assert!(
        reprompt.title.contains("invalid runtime path"),
        "{}",
        reprompt.title
    );
}

#[test]
fn recipe_picker_index_and_free_text_lanes() {
    // §9: >1 ranked candidates → the FuzzySelect picker; Index picks the
    // candidate; free text resolves as a local script path (slug-first rule
    // covered by the unit tests of choose_recipe's resolution helper).
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let runtime_dir = tmp.path().join("runtime");
    let log = tmp.path().join("wine.log");
    fake_runtime(&runtime_dir, &log);
    let installer = tmp.path().join("setup_testgame_1.0.exe");
    std::fs::write(&installer, b"MZ").unwrap();

    let recipe_file = tmp.path().join("pinned.json");
    std::fs::write(
        &recipe_file,
        r#"{"files": [], "installer": [], "wine": {"version": "wine-stable"}}"#,
    )
    .unwrap();

    let path_answer = recipe_file.display().to_string();
    let prompter: std::sync::Arc<gogo::macapp::PromptFn> =
        std::sync::Arc::new(move |spec: &gogo::macapp::PromptSpec| match spec.kind {
            gogo::macapp::PromptKind::Fuzzy => {
                assert!(spec.allow_other && spec.allow_none);
                Some(gogo::macapp::PromptAnswer::Other(path_answer.clone()))
            }
            // The ddraw Confirm follows the picker (design 13 D1); decline.
            gogo::macapp::PromptKind::Confirm { .. } => {
                Some(gogo::macapp::PromptAnswer::Confirmed(false))
            }
            _ => None,
        });

    // Wiremock Lutris with two installers (newer first after ranking).
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let server = rt.block_on(async {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};
        let server = wiremock::MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/games"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "count": 1,
                "results": [{
                    "id": 1, "slug": "test-game", "name": "Test Game",
                    "provider_games": [{"slug": "1", "service": "gog"}]
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/games/test-game"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<a href="/api/installers/gog-a?format=json">Download script</a>
                   <a href="/api/installers/gog-b?format=json">Download script</a>"#,
            ))
            .mount(&server)
            .await;
        for (slug, updated) in [("gog-a", "2026-01-02"), ("gog-b", "2026-01-01")] {
            Mock::given(method("GET"))
                .and(path(format!("/api/installers/{slug}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "count": 1,
                    "results": [{
                        "slug": slug, "game_slug": "test-game", "name": "Test Game",
                        "version": "1.0", "runner": "wine", "user": "alice",
                        "updated_at": updated, "gogid": 1,
                        "script": {"files": [], "installer": []}
                    }]
                })))
                .mount(&server)
                .await;
        }
        server
    });

    let opts = MacAppOptions {
        installer: Some(installer),
        game_tree: None,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir),
        out_dir: tmp.path().to_path_buf(),
        overwrite: true,
        recipe: gogo::macapp::RecipeChoice::Auto,
        runtime_menu: false,
        runtime_version: None,
        lutris_base: Some(server.uri()),
        renderer: macapp::RendererChoice::Auto,
        portable: true,
        gogo_path: None,
    };
    let plan = macapp::plan(&opts, Some(prompter.as_ref()), &mut |_| {}).expect("plan");
    drop(guard);

    // Free text resolved as the local script path → pinned file semantics.
    let sel = plan.recipe.as_ref().expect("recipe selected");
    assert_eq!(sel.installer.slug, "pinned");
    assert_eq!(sel.installer.version, "local file");
    assert!(!sel.from_search, "free-text file picks are pinned");
    assert_eq!(sel.wine_version.as_deref(), Some("wine-stable"));
}

#[test]
fn recipes_list_and_show_inspection_path() {
    // Design 11 §1/§3 (design 12 §12 phase 5): recipe_candidates ranks the
    // corpus for a game name; recipe_by_slug fetches one record; the
    // inspection ctx + interpret::plan classify it without touching the
    // filesystem (nominal paths — classification is pure).
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let script = serde_json::json!({
        "wine": {"version": "lutris-ge-proton9-20"},
        "files": [
            {"gog": "N/A:Select the GOG offline installer"},
            {"patch": "https://example.com/patch.zip"}
        ],
        "installer": [
            {"task": {"name": "wineexec", "executable": "gog", "args": "/NOGUI /SUPPRESSMSGBOXES"}},
            {"task": {"name": "winetricks", "app": ["corefonts", "ddraw"]}},
            {"execute": {"file": "patch", "args": "/q"}}
        ]
    });

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let server = rt.block_on(async {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, ResponseTemplate};
        let server = wiremock::MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/games"))
            .and(query_param("search", "Test Game"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "count": 1,
                "results": [{
                    "id": 1, "slug": "test-game", "name": "Test Game",
                    "provider_games": [{"slug": "1", "service": "gog"}]
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/games/test-game"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<a href="/api/installers/gog-a?format=json">Download script</a>"#,
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/installers/gog-a"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "count": 1,
                "results": [{
                    "slug": "gog-a", "game_slug": "test-game", "name": "Test Game",
                    "version": "1.0", "runner": "wine", "user": "alice",
                    "updated_at": "2026-01-02", "gogid": 1,
                    "script": script
                }]
            })))
            .mount(&server)
            .await;
        // A second, stale installer proves ranking (newer first).
        Mock::given(method("GET"))
            .and(path("/games/older-game"))
            .respond_with(ResponseTemplate::new(200).set_body_string(""))
            .mount(&server)
            .await;
        server
    });

    // list: ranked candidates for the game name (recipes list).
    let candidates =
        macapp::recipe_candidates(Some(&server.uri()), "Test Game").expect("candidates");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].slug, "gog-a");
    assert_eq!(candidates[0].runner, "wine");

    // show: fetch by slug, classify with the inspection ctx.
    let inst = macapp::recipe_by_slug(Some(&server.uri()), "gog-a").expect("record");
    assert_eq!(inst.slug, "gog-a");
    let pin = gogo::macapp::lutris::wine_version(&inst.script);
    assert_eq!(pin.as_deref(), Some("lutris-ge-proton9-20"));
    let rec = gogo::macapp::catalog::recommend_for_wine(pin.as_deref());
    assert_eq!(rec.id, "winecx");
    assert!(rec.notice.is_some(), "Linux-only pin carries the notice");

    let ctx = macapp::inspection_ctx(&inst.script);
    let plan = gogo::macapp::interpret::plan(&inst.script, &ctx);
    use gogo::macapp::interpret::Step;
    assert!(plan.steps.iter().any(|(i, s)| {
        matches!(s, Step::HarvestInstallerArgs { args } if args.contains("/NOGUI") && *i == 0)
    }));
    assert!(
        plan.steps.iter().any(
            |(_, s)| matches!(s, Step::Winetricks { verbs } if verbs == &["corefonts", "ddraw"])
        )
    );
    assert!(
        plan.skipped
            .iter()
            .any(|s| s.verb == "execute" && s.reason.to_lowercase().contains("shell"))
    );
    assert!(plan.installer_args.iter().any(|a| a == "/NOGUI"));
    drop(guard);
}

#[test]
fn recipe_discovery_trims_version_and_falls_back_to_shorter_queries() {
    // Live-verified shapes (2026-10-19): "Pharaoh gold 2 1 0 15" must first
    // lose the GOG version tail (display_name), then the strict Lutris search
    // must fall back "Pharaoh gold" → "Pharaoh", and pick_game must land on
    // the token-overlapping GOG-linked game — not the alphabetically-first
    // loose match.
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let display = macapp::display_name(
        std::path::Path::new("/x/setup_pharaoh_gold_2.1.0.15.exe"),
        None,
    );
    assert_eq!(display, "Pharaoh gold");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let server = rt.block_on(async {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, ResponseTemplate};
        let server = wiremock::MockServer::start().await;
        // Strict search: the full name answers nothing…
        Mock::given(method("GET"))
            .and(path("/api/games"))
            .and(query_param("search", "Pharaoh gold"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "count": 0, "results": []
            })))
            .mount(&server)
            .await;
        // …the trimmed name answers the loose 20-match page (subset here).
        Mock::given(method("GET"))
            .and(path("/api/games"))
            .and(query_param("search", "Pharaoh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "count": 20,
                "results": [
                    {"id": 1, "slug": "arizona-rose-and-the-pharaohs-riddles",
                     "name": "Arizona Rose and the Pharaohs' Riddles",
                     "provider_games": []},
                    {"id": 2, "slug": "pharaoh-a-new-era", "name": "Pharaoh: A New Era",
                     "provider_games": [{"slug": "1207659027", "service": "gog"}]},
                    {"id": 3, "slug": "pharaoh", "name": "Pharaoh",
                     "provider_games": [{"slug": "1207659017", "service": "gog"}]}
                ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/games/pharaoh"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<a href="/api/installers/pharaoh-cd-cnc-ddraw?format=json">x</a>"#,
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/installers/pharaoh-cd-cnc-ddraw"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "count": 1,
                "results": [{
                    "slug": "pharaoh-cd-cnc-ddraw", "game_slug": "pharaoh",
                    "name": "Pharaoh", "version": "CD + cnc-ddraw", "runner": "wine",
                    "user": "legluondunet", "updated_at": "2024-06-01",
                    "gogid": 1207659017,
                    "script": {"files": [], "installer": []}
                }]
            })))
            .mount(&server)
            .await;
        server
    });

    let candidates = macapp::recipe_candidates(Some(&server.uri()), &display).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].slug, "pharaoh-cd-cnc-ddraw");
    drop(guard);
}

#[test]
fn search_recipe_failure_degrades_pinned_stays_fatal() {
    // Design 11 §7 (amended): a recipe step escaping both the bundle and the
    // cache fails the phase. Search-discovered → loud warn + recipe-less
    // finalize (the bundle still gets a launcher + Info.plist); pinned → Err.
    // Regression shape of the live 2026-10-19 pharaoh incident, where the
    // same abort left a "damaged or incomplete" half-bundle behind.
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let runtime_dir = tmp.path().join("runtime");
    let log = tmp.path().join("wine.log");
    fake_runtime(&runtime_dir, &log);
    let installer = tmp.path().join("setup_testgame_1.0.exe");
    std::fs::write(&installer, b"MZ").unwrap();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let server =
        rt.block_on(async {
            use wiremock::matchers::{method, path, query_param};
            use wiremock::{Mock, ResponseTemplate};
            let server = wiremock::MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/games"))
                .and(query_param("search", "Test Game"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "count": 1,
                    "results": [{
                        "id": 1, "slug": "test-game", "name": "Test Game",
                        "provider_games": [{"slug": "1", "service": "gog"}]
                    }]
                })))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/games/test-game"))
                .respond_with(ResponseTemplate::new(200).set_body_string(
                    r#"<a href="/api/installers/test-game-gog?format=json">x</a>"#,
                ))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/installers/test-game-gog"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "count": 1,
                    "results": [{
                        "slug": "test-game-gog", "game_slug": "test-game",
                        "name": "Test Game", "version": "1.0", "runner": "wine",
                        "user": "alice", "updated_at": "2026-01-01", "gogid": 1,
                        "script": {
                            "files": [],
                            "installer": [
                                {"write_file": {"file": "$CACHE/../../escaped.txt",
                                                "content": "pwned"}}
                            ]
                        }
                    }]
                })))
                .mount(&server)
                .await;
            server
        });

    // Search-discovered (Auto): degrade to a complete, recipe-less bundle.
    let opts = MacAppOptions {
        installer: Some(installer.clone()),
        game_tree: None,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir.clone()),
        out_dir: tmp.path().join("out-auto"),
        overwrite: true,
        recipe: gogo::macapp::RecipeChoice::Auto,
        runtime_menu: false,
        runtime_version: None,
        lutris_base: Some(server.uri()),
        renderer: macapp::RendererChoice::Auto,
        portable: true,
        gogo_path: None,
    };
    let mut warns = Vec::new();
    let mut record = |ev: &ProgressEvent| {
        if let ProgressEvent::Warn { message } = ev {
            warns.push(message.clone());
        }
    };
    let plan = macapp::plan(&opts, None, &mut record).unwrap();
    macapp::execute(&plan, CodesignPolicy::Skip, &mut record).unwrap();
    let root = plan.bundle_path.clone();
    assert!(
        root.join("Contents/MacOS/Test Game").exists(),
        "no launcher"
    );
    assert!(root.join("Contents/Info.plist").exists(), "no Info.plist");
    assert!(
        warns
            .iter()
            .any(|w| w.contains("recipe test-game-gog failed")),
        "no degrade warn: {warns:?}"
    );
    assert!(
        !tmp.path().join("escaped.txt").exists(),
        "escaping write must not land"
    );

    // Pinned (--recipe <file>): the same failure is fatal (design 11 §7).
    let script = tmp.path().join("escape.yml");
    std::fs::write(
        &script,
        r#"{"files": [], "installer": [
            {"write_file": {"file": "$CACHE/../../escaped.txt", "content": "pwned"}}
        ]}"#,
    )
    .unwrap();
    let opts = MacAppOptions {
        installer: Some(installer),
        game_tree: None,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir),
        out_dir: tmp.path().join("out-pinned"),
        overwrite: true,
        recipe: gogo::macapp::RecipeChoice::File(script.clone()),
        runtime_menu: false,
        runtime_version: None,
        lutris_base: Some(server.uri()),
        renderer: macapp::RendererChoice::Auto,
        portable: true,
        gogo_path: None,
    };
    let mut sink = |_ev: &ProgressEvent| {};
    let plan = macapp::plan(&opts, None, &mut sink).unwrap();
    assert!(macapp::execute(&plan, CodesignPolicy::Skip, &mut sink).is_err());
    drop(guard);
}

#[test]
fn runtime_menu_is_opt_in_with_easy_default() {
    // §5 (amended 2026-10-23, #76): without the menu flag an interactive run
    // takes the ladder hit (cached winecx, the default track; a cached
    // wine-staging is not picked) with no prompt. With it, the menu lists
    // every option, the easy pick is preselected (spec.default + "· easy"),
    // and choosing it resolves from the store.
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let installer = tmp.path().join("setup_testgame_1.0.exe");
    std::fs::write(&installer, b"MZ").unwrap();

    let home = tmp.path().join("home");
    let tree = home.join("runtimes/wine-staging/9.0/bin");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("wine"), "#!/bin/sh\necho wine-9.0-staging\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(tree.join("wine"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    std::fs::write(
        home.join("runtimes/manifest.json"),
        r#"[{"id":"wine-staging","version":"9.0","sha256":"0","fetched_at":"2026-01-01T00:00:00Z","upstream_url":"x","entry_rel":"bin/wine","bytes":16},
            {"id":"winecx","version":"24.0.7_7","sha256":"0","fetched_at":"2026-01-01T00:00:00Z","upstream_url":"x","entry_rel":"engine/bin/wine","bytes":16}]"#,
    )
    .unwrap();
    let cx = home.join("runtimes/winecx/24.0.7_7/engine/bin");
    std::fs::create_dir_all(&cx).unwrap();
    std::fs::write(cx.join("wine"), "#!/bin/sh\necho wine-9.0-cx\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(cx.join("wine"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let seen: std::sync::Arc<std::sync::Mutex<Vec<gogo::macapp::PromptSpec>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    let prompter: std::sync::Arc<gogo::macapp::PromptFn> =
        std::sync::Arc::new(move |spec: &gogo::macapp::PromptSpec| {
            seen2.lock().unwrap().push(spec.clone());
            match spec.kind {
                gogo::macapp::PromptKind::Select => {
                    Some(gogo::macapp::PromptAnswer::Index(spec.default))
                }
                _ => None,
            }
        });

    let mut opts = MacAppOptions {
        installer: Some(installer),
        game_tree: None,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Auto,
        runtime_path: None,
        out_dir: tmp.path().to_path_buf(),
        overwrite: true,
        recipe: gogo::macapp::RecipeChoice::None,
        runtime_menu: false,
        runtime_version: None,
        lutris_base: None,
        renderer: macapp::RendererChoice::Auto,
        portable: true,
        gogo_path: None,
    };

    let old_path = std::env::var("PATH").unwrap_or_default();
    // SAFETY: env mutation under ENV_LOCK (established pattern above).
    unsafe {
        std::env::set_var("GOGO_HOME", &home);
        std::env::set_var("PATH", "/usr/bin:/bin"); // hide brew
    }
    let quiet = macapp::plan(&opts, Some(prompter.as_ref()), &mut |_| {}).expect("plan");
    let no_menu = !seen
        .lock()
        .unwrap()
        .iter()
        .any(|sp| matches!(sp.kind, gogo::macapp::PromptKind::Select));
    seen.lock().unwrap().clear();
    opts.runtime_menu = true;
    let plan = macapp::plan(&opts, Some(prompter.as_ref()), &mut |_| {}).expect("plan");
    unsafe {
        std::env::set_var("PATH", &old_path);
        std::env::remove_var("GOGO_HOME");
    }
    drop(guard);

    assert!(no_menu, "no runtime menu unless asked for");
    let store_root = home.join("runtimes/winecx/24.0.7_7");
    assert!(
        quiet.runtime.wine.starts_with(&store_root),
        "the ladder picks the cached default track: {}",
        quiet.runtime.wine.display()
    );
    let specs = seen.lock().unwrap();
    let menu = specs
        .iter()
        .find(|sp| matches!(sp.kind, gogo::macapp::PromptKind::Select))
        .expect("menu prompted");
    assert!(
        specs.len() <= 2,
        "ddraw Confirm then the menu only: {:?}",
        specs.iter().map(|s| &s.title).collect::<Vec<_>>()
    );
    assert_eq!(menu.default, 0, "easy pick preselected");
    assert!(
        menu.items[0].contains("winecx") && menu.items[0].contains("· easy"),
        "easy marker on the ladder pick: {:?}",
        menu.items[0]
    );
    assert!(
        menu.items
            .iter()
            .any(|i| i.starts_with("wine-staging 9.0 (✓ cached)") && !i.contains("easy")),
        "staging stays selectable: {:?}",
        menu.items
    );
    // from_store keeps the literal GOGO_HOME path (no canonicalization).
    assert!(
        plan.runtime.wine.starts_with(&store_root),
        "default choice resolves to the cached winecx tree: {}",
        plan.runtime.wine.display()
    );
}

#[test]
fn recipe_picker_is_opt_in_and_asks_for_single_candidate() {
    // §1 (amended 2026-10-23, #76): the default recipe is `none`, so an
    // interactive run shows no picker. Asked for (`auto`), even a single
    // candidate is the user's decision — the picker runs with "none"
    // offered; picking it yields a recipe-less plan, picking the candidate
    // selects it.
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let runtime_dir = tmp.path().join("runtime");
    let log = tmp.path().join("wine.log");
    fake_runtime(&runtime_dir, &log);
    let installer = tmp.path().join("setup_testgame_1.0.exe");
    std::fs::write(&installer, b"MZ").unwrap();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let server =
        rt.block_on(async {
            use wiremock::matchers::{method, path, query_param};
            use wiremock::{Mock, ResponseTemplate};
            let server = wiremock::MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/games"))
                .and(query_param("search", "Test Game"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "count": 1,
                    "results": [{
                        "id": 1, "slug": "test-game", "name": "Test Game",
                        "provider_games": [{"slug": "1", "service": "gog"}]
                    }]
                })))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/games/test-game"))
                .respond_with(ResponseTemplate::new(200).set_body_string(
                    r#"<a href="/api/installers/test-game-gog?format=json">x</a>"#,
                ))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/installers/test-game-gog"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "count": 1,
                    "results": [{
                        "slug": "test-game-gog", "game_slug": "test-game",
                        "name": "Test Game", "version": "1.0", "runner": "wine",
                        "user": "alice", "updated_at": "2026-01-01", "gogid": 1,
                        "script": {"files": [], "installer": []}
                    }]
                })))
                .mount(&server)
                .await;
            server
        });

    let mk_opts = |out: &std::path::Path| MacAppOptions {
        installer: Some(installer.clone()),
        game_tree: None,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir.clone()),
        out_dir: out.to_path_buf(),
        overwrite: true,
        recipe: gogo::macapp::RecipeChoice::Auto,
        runtime_menu: false,
        runtime_version: None,
        lutris_base: Some(server.uri()),
        renderer: macapp::RendererChoice::Auto,
        portable: true,
        gogo_path: None,
    };

    // (a) pick the candidate.
    let seen: std::sync::Arc<std::sync::Mutex<Vec<gogo::macapp::PromptSpec>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    let pick: std::sync::Arc<gogo::macapp::PromptFn> = std::sync::Arc::new(move |spec| {
        seen2.lock().unwrap().push(spec.clone());
        match spec.kind {
            gogo::macapp::PromptKind::Fuzzy | gogo::macapp::PromptKind::Select => {
                Some(gogo::macapp::PromptAnswer::Index(0))
            }
            _ => None,
        }
    });
    let opts = mk_opts(&tmp.path().join("out-a"));
    let plan = macapp::plan(&opts, Some(pick.as_ref()), &mut |_| {}).expect("plan");
    assert_eq!(
        plan.recipe.as_ref().unwrap().installer.slug,
        "test-game-gog"
    );

    // (b) opt out via "none".
    let none: std::sync::Arc<gogo::macapp::PromptFn> =
        std::sync::Arc::new(|spec| match spec.kind {
            gogo::macapp::PromptKind::Fuzzy | gogo::macapp::PromptKind::Select => {
                Some(gogo::macapp::PromptAnswer::NoneSelected)
            }
            _ => None,
        });
    let opts = mk_opts(&tmp.path().join("out-b"));
    let plan = macapp::plan(&opts, Some(none.as_ref()), &mut |_| {}).expect("plan");
    assert!(plan.recipe.is_none(), "none must yield a recipe-less plan");

    // (c) the default (`none`): no picker, no recipe.
    seen.lock().unwrap().clear();
    let opts = MacAppOptions {
        recipe: Default::default(),
        ..mk_opts(&tmp.path().join("out-c"))
    };
    let plan = macapp::plan(&opts, Some(pick.as_ref()), &mut |_| {}).expect("plan");
    assert!(plan.recipe.is_none(), "no recipe by default");
    assert!(
        !seen
            .lock()
            .unwrap()
            .iter()
            .any(|sp| matches!(sp.kind, gogo::macapp::PromptKind::Fuzzy)),
        "no recipe picker by default"
    );
    drop(guard);
}

#[test]
fn produced_launcher_execs_wine_with_contents_relative_paths() {
    // Regression (2026-10-19, live "app doesn't run"): launch.conf paths were
    // bundle-root-relative (`Contents/Resources/...`) while the launcher
    // resolves every conf value against its own `..` (= `Contents`), doubling
    // the segment — `cd` failed and `set -e` killed the launch before wine
    // ever ran. Execute the produced stub end-to-end and check the wine
    // invocation's argv, WINEPREFIX, and working directory.
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let runtime_dir = tmp.path().join("runtime");
    let log = tmp.path().join("wine.log");
    fake_runtime(&runtime_dir, &log);
    let installer = tmp.path().join("setup_testgame_1.0.exe");
    std::fs::write(&installer, b"MZ").unwrap();

    let opts = MacAppOptions {
        installer: Some(installer),
        game_tree: None,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir),
        out_dir: tmp.path().to_path_buf(),
        overwrite: true,
        recipe: gogo::macapp::RecipeChoice::None,
        runtime_menu: false,
        runtime_version: None,
        lutris_base: None,
        renderer: macapp::RendererChoice::Auto,
        portable: true,
        gogo_path: None,
    };
    let plan = macapp::plan(&opts, None, &mut |_| {}).unwrap();
    let _bytes = macapp::execute(&plan, CodesignPolicy::Skip, &mut |_| {}).unwrap();

    // The conf itself must be Contents-relative.
    let conf = std::fs::read_to_string(plan.bundle.resources.join("launch.conf")).unwrap();
    assert!(
        conf.contains("WINE='Resources/wine"),
        "conf wine path Contents-relative: {conf}"
    );
    assert!(
        !conf.contains("Contents/Contents"),
        "doubled segment: {conf}"
    );

    // Execute the stub like Finder would.
    let out = std::process::Command::new(plan.bundle.macos.join("Test Game"))
        .env_remove("WINEPREFIX")
        .output()
        .expect("run launcher");
    assert!(out.status.success(), "launcher failed: {out:?}");
    let logged = std::fs::read_to_string(&log).unwrap();
    let env_line = logged
        .lines()
        .filter(|l| l.starts_with("ENV:"))
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .pop()
        .unwrap();
    assert!(
        env_line.contains(&*plan.bundle.prefix.to_string_lossy()),
        "WINEPREFIX wrong: {env_line}"
    );
    let cwd_line = logged
        .lines()
        .filter(|l| l.starts_with("CWD:"))
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .pop()
        .unwrap();
    assert!(
        cwd_line.ends_with("drive_c/GOG/test-game"),
        "wine cwd must be the game dir: {cwd_line}"
    );
    assert!(
        logged.contains("drive_c/GOG/test-game/game.exe"),
        "wine argv must carry the entry exe: {logged}"
    );
    drop(guard);
}

#[test]
fn ddraw_wrapper_application_lanes() {
    // Design 13 (fat bundles): `--renderer cnc-ddraw` applies dll+ini+
    // shaders+MIT notice into the detected game dir via the
    // GOGO_CNC_DDRAW_DIR override (WINETRICKS-style); Auto on a game with no
    // DirectDraw imports (the fake exe isn't a PE) picks wined3d — no
    // wrapper, no notice.
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let runtime_dir = tmp.path().join("runtime");
    let log = tmp.path().join("wine.log");
    fake_runtime(&runtime_dir, &log);
    let installer = tmp.path().join("setup_testgame_1.0.exe");
    std::fs::write(&installer, b"MZ").unwrap();

    let store = tmp.path().join("ddraw-tree");
    std::fs::create_dir_all(store.join("Shaders")).unwrap();
    std::fs::write(store.join("ddraw.dll"), b"MZ-ddraw").unwrap();
    std::fs::write(store.join("Shaders/one.fx"), b"shader").unwrap();

    let mk = |out: &std::path::Path, renderer: macapp::RendererChoice| MacAppOptions {
        installer: Some(installer.clone()),
        game_tree: None,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir.clone()),
        out_dir: out.to_path_buf(),
        overwrite: true,
        recipe: gogo::macapp::RecipeChoice::None,
        runtime_menu: false,
        runtime_version: None,
        lutris_base: None,
        renderer,
        portable: true,
        gogo_path: None,
    };

    // SAFETY: env mutation under ENV_LOCK (established pattern above).
    unsafe {
        std::env::set_var("GOGO_HOME", tmp.path().join("home"));
        std::env::set_var("GOGO_CNC_DDRAW_DIR", &store);
    }
    // (a) On: applied.
    let opts = mk(
        &tmp.path().join("out-on"),
        macapp::RendererChoice::Force(macapp::Renderer::CncDdraw),
    );
    let plan = macapp::plan(&opts, None, &mut |_| {}).unwrap();
    assert!(
        plan.renderer.is_none(),
        "installer lane: decided after the install"
    );
    let mut steps = Vec::new();
    let mut record = |ev: &ProgressEvent| {
        if let ProgressEvent::RecipeStep { verb } = ev {
            steps.push(verb.clone());
        }
    };
    let _ = macapp::execute(&plan, CodesignPolicy::Skip, &mut record).unwrap();
    let game = plan.bundle.prefix.join("drive_c/GOG/test-game");
    assert_eq!(
        std::fs::read(game.join("ddraw.dll")).unwrap(),
        b"MZ-ddraw",
        "wrapper dll in the game dir"
    );
    assert!(
        std::fs::read_to_string(game.join("ddraw.ini"))
            .unwrap()
            .contains("maintas=true")
    );
    assert!(game.join("Shaders/one.fx").is_file());
    assert!(
        plan.bundle
            .resources
            .join("LICENSE-cnc-ddraw.txt")
            .is_file(),
        "MIT notice (design 13 D5)"
    );
    assert!(
        steps.iter().any(|v| v.starts_with("cnc-ddraw")),
        "RecipeStep event: {steps:?}"
    );
    // The load-order half (design 13 D3): the bundled wine must have set the
    // per-prefix DllOverrides entry — the fake runtime logged its argv.
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(
        logged.contains("DllOverrides") && logged.contains("n,b"),
        "override registered via reg add: {logged}"
    );

    // (b) Auto with nothing DirectDraw in the imports: wined3d.
    let opts = mk(&tmp.path().join("out-auto"), macapp::RendererChoice::Auto);
    let plan = macapp::plan(&opts, None, &mut |_| {}).unwrap();
    let _ = macapp::execute(&plan, CodesignPolicy::Skip, &mut |_| {}).unwrap();
    let game = plan.bundle.prefix.join("drive_c/GOG/test-game");
    assert!(!game.join("ddraw.dll").exists());
    assert!(!plan.bundle.resources.join("LICENSE-cnc-ddraw.txt").exists());
    unsafe {
        std::env::remove_var("GOGO_CNC_DDRAW_DIR");
        std::env::remove_var("GOGO_HOME");
    }
    drop(guard);
}

#[test]
fn macapp_from_depot_executes() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime_dir = tmp.path().join("runtime");
    let log = tmp.path().join("wine.log");
    fake_runtime(&runtime_dir, &log);

    // A fake depot tree (design 15 §7): goggame info + exe + nested data to
    // prove a deep clone — exactly what `gogo depot` materializes (docs/18).
    let tree = tmp.path().join("tree").join("Pharaoh Gold");
    std::fs::create_dir_all(tree.join("Data")).unwrap();
    std::fs::write(
        tree.join("goggame-1207659046.info"),
        r#"{"name":"Pharaoh Gold","playTasks":[{"type":"FileTask","category":"game","path":"Pharaoh.exe","isPrimary":true}]}"#,
    )
    .unwrap();
    std::fs::write(tree.join("Pharaoh.exe"), b"MZ").unwrap();
    std::fs::write(tree.join("Data").join("kings.dat"), b"ramses").unwrap();

    // Cache-layout sibling (design 14 D2): GOG's support installer. The fake
    // wine treats any `*.exe` with /DIR= as an installer that drops
    // `goggame-1.info` + `game.exe` into the target — asserting it ran into
    // the clone (D3 amendment).
    let support = tmp
        .path()
        .join("tree")
        .join("gog-support")
        .join("galaxy_pharaoh_2.1.0.15.exe");
    std::fs::create_dir_all(support.parent().unwrap()).unwrap();
    std::fs::write(&support, b"MZ support").unwrap();

    let out = tmp.path().join("out");
    let opts = MacAppOptions {
        installer: None,
        game_tree: Some(tree.clone()),
        name: None,
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir.clone()),
        out_dir: out.clone(),
        overwrite: true,
        recipe: Default::default(),
        runtime_menu: false,
        runtime_version: None,
        lutris_base: None,
        renderer: macapp::RendererChoice::Auto,
        portable: true,
        gogo_path: None,
    };

    let mut emit = |_ev: &ProgressEvent| {};
    let plan = macapp::plan(&opts, None, &mut emit).expect("plan");
    // Display name comes from the goggame info (design 15 D1).
    assert_eq!(plan.display, "Pharaoh Gold");
    assert_eq!(plan.slug, "pharaoh-gold");
    assert!(plan.installer.is_none());
    assert!(plan.game_tree_bytes > 0);

    let bytes = macapp::execute(&plan, CodesignPolicy::Skip, &mut emit).expect("execute");
    assert!(bytes > 0);

    // The tree is cloned into drive_c/GOG/<tree-name> (design 15 D3).
    let game_dir = out.join("Pharaoh Gold.app/Contents/Resources/prefix/drive_c/GOG/Pharaoh Gold");
    assert!(game_dir.join("Pharaoh.exe").is_file());
    assert!(game_dir.join("Data/kings.dat").is_file());
    assert!(game_dir.join("goggame-1207659046.info").is_file());
    // The support installer ran into the clone (fake wine wrote its marker).
    assert!(game_dir.join("game.exe").is_file());

    // Entry discovery ran inside the cloned tree; conf points at it.
    let conf = std::fs::read_to_string(out.join("Pharaoh Gold.app/Contents/Resources/launch.conf"))
        .unwrap();
    assert!(
        conf.contains("drive_c/GOG/Pharaoh Gold/Pharaoh.exe"),
        "conf GAME points into the cloned tree: {conf}"
    );

    // The produced launcher runs end-to-end: fake wine logs its argv.
    std::fs::write(&log, b"").unwrap();
    let status =
        std::process::Command::new(out.join("Pharaoh Gold.app/Contents/MacOS/Pharaoh Gold"))
            .status()
            .unwrap();
    assert!(status.success());
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(
        logged.contains("Pharaoh.exe"),
        "launcher execs the cloned exe: {logged}"
    );
    assert!(
        logged.contains("drive_c/GOG/Pharaoh Gold"),
        "cwd is the game dir: {logged}"
    );
}

#[test]
fn depot_support_decline_degrades_to_warn() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime_dir = tmp.path().join("runtime");
    let log = tmp.path().join("wine.log");
    fake_runtime(&runtime_dir, &log);

    let tree = tmp.path().join("tree").join("Declined Game");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(
        tree.join("goggame-7.info"),
        r#"{"name":"Declined Game","playTasks":[{"type":"FileTask","category":"game","path":"game.exe","isPrimary":true}]}"#,
    )
    .unwrap();
    std::fs::write(tree.join("game.exe"), b"MZ").unwrap();
    // Support installer that declines (fake wine exits 1 for *fail_support.exe).
    let support = tmp
        .path()
        .join("tree")
        .join("gog-support")
        .join("galaxy_fail_support.exe");
    std::fs::create_dir_all(support.parent().unwrap()).unwrap();
    std::fs::write(&support, b"MZ").unwrap();

    let out = tmp.path().join("out");
    let opts = MacAppOptions {
        installer: None,
        game_tree: Some(tree.clone()),
        name: None,
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir),
        out_dir: out.clone(),
        overwrite: true,
        recipe: Default::default(),
        runtime_menu: false,
        runtime_version: None,
        lutris_base: None,
        renderer: macapp::RendererChoice::Auto,
        portable: true,
        gogo_path: None,
    };
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = events.clone();
    let mut emit = move |ev: &ProgressEvent| {
        if let ProgressEvent::Warn { message } = ev {
            sink.lock().unwrap().push(message.clone());
        }
    };
    let plan = macapp::plan(&opts, None, &mut emit).expect("plan");
    macapp::execute(&plan, CodesignPolicy::Skip, &mut emit).expect("execute despite decline");

    // The bundle assembled and the decline was loud + actionable (design 15).
    assert!(
        out.join("Declined Game.app/Contents/Resources/launch.conf")
            .is_file()
    );
    let warns = events.lock().unwrap().join("\n");
    assert!(
        warns.contains("support installer declined"),
        "decline warn: {warns}"
    );
    // No support-written files (the fake wine fails before writing).
    let game = out.join("Declined Game.app/Contents/Resources/prefix/drive_c/GOG/Declined Game");
    assert!(game.join("game.exe").is_file());
}

#[test]
fn depot_support_overlay_merges_into_game_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime_dir = tmp.path().join("runtime");
    let log = tmp.path().join("wine.log");
    fake_runtime(&runtime_dir, &log);

    // Carmageddon's depot shape (docs/19 §1 item 4): the manifest lacks
    // ASSETS/options.txt; GOG ships it in the sibling support overlay.
    let tree = tmp.path().join("tree").join("Carmageddon TDR2K");
    std::fs::create_dir_all(tree.join("ASSETS")).unwrap();
    std::fs::write(
        tree.join("goggame-1146738698.info"),
        r#"{"name":"Carmageddon TDR 2000","playTasks":[{"type":"FileTask","category":"game","path":"TDR2000.exe","isPrimary":true}]}"#,
    )
    .unwrap();
    std::fs::write(tree.join("TDR2000.exe"), b"MZ").unwrap();
    std::fs::write(tree.join("config.ini"), b"depot").unwrap();
    let app = tmp
        .path()
        .join("tree/gog-support/1146738698/1146738698/app");
    std::fs::create_dir_all(app.join("ASSETS")).unwrap();
    std::fs::write(app.join("ASSETS/options.txt"), b"Resolution 640x480").unwrap();
    std::fs::write(app.join("config.ini"), b"overlay").unwrap();

    let out = tmp.path().join("out");
    let opts = MacAppOptions {
        installer: None,
        game_tree: Some(tree.clone()),
        name: None,
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir),
        out_dir: out.clone(),
        overwrite: true,
        recipe: Default::default(),
        runtime_menu: false,
        runtime_version: None,
        lutris_base: None,
        renderer: macapp::RendererChoice::Auto,
        portable: true,
        gogo_path: None,
    };
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = events.clone();
    let mut emit = move |ev: &ProgressEvent| {
        if let ProgressEvent::Warn { message } = ev {
            sink.lock().unwrap().push(message.clone());
        }
    };
    let plan = macapp::plan(&opts, None, &mut emit).expect("plan");
    macapp::execute(&plan, CodesignPolicy::Skip, &mut emit).expect("execute");

    let game = out
        .join("Carmageddon TDR 2000.app/Contents/Resources/prefix/drive_c/GOG/Carmageddon TDR2K");
    assert_eq!(
        std::fs::read(game.join("ASSETS/options.txt")).unwrap(),
        b"Resolution 640x480"
    );
    assert_eq!(
        std::fs::read(game.join("config.ini")).unwrap(),
        b"overlay",
        "overlay files win"
    );
    // The depot cache itself is untouched.
    assert_eq!(std::fs::read(tree.join("config.ini")).unwrap(), b"depot");
    assert!(!tree.join("ASSETS/options.txt").exists());

    let warns = events.lock().unwrap().join("\n");
    assert!(
        warns.contains("merged GOG support overlay into the game dir: 2 file(s)")
            && warns.contains("ASSETS/options.txt"),
        "overlay merge is reported: {warns}"
    );
}

// — v2 launcher bundles (design 16, #66) —

/// A depot tree for game 1207659013 plus a fake runtime; returns the plan
/// options for a v2 (non-portable) build. GOGO_HOME must already point into
/// `tmp`.
fn v2_opts(tmp: &std::path::Path, gogo_path: &std::path::Path) -> MacAppOptions {
    let runtime_dir = tmp.join("runtime");
    fake_runtime(&runtime_dir, &tmp.join("wine.log"));
    let tree = tmp.join("depot").join("Treasure Adventure Game");
    std::fs::create_dir_all(tree.join("data")).unwrap();
    std::fs::write(
        tree.join("goggame-1207659013.info"),
        r#"{"name":"Treasure Adventure Game","rootGameId":"1207659013","playTasks":[{"type":"FileTask","category":"game","path":"Game.exe","isPrimary":true}]}"#,
    )
    .unwrap();
    std::fs::write(tree.join("Game.exe"), b"MZ").unwrap();
    std::fs::write(tree.join("data/level1.dat"), b"x").unwrap();
    MacAppOptions {
        installer: None,
        game_tree: Some(tree),
        name: None,
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir),
        out_dir: tmp.join("out"),
        overwrite: true,
        recipe: Default::default(),
        runtime_menu: false,
        runtime_version: None,
        lutris_base: None,
        renderer: macapp::RendererChoice::Auto,
        portable: false,
        gogo_path: Some(gogo_path.to_path_buf()),
    }
}

/// Every path + file content under `dir` (symlinks by target), sorted: a
/// stand-in for `codesign --verify` on hosts without codesign.
fn tree_digest(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, Vec<u8>)>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap().display().to_string();
            let ty = e.file_type().unwrap();
            if ty.is_symlink() {
                out.push((
                    rel,
                    std::fs::read_link(&p)
                        .unwrap()
                        .display()
                        .to_string()
                        .into_bytes(),
                ));
            } else if ty.is_dir() {
                out.push((rel + "/", Vec::new()));
                walk(root, &p, out);
            } else {
                out.push((rel, std::fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// Run the generated launcher with a clean env (no gogo on PATH).
fn run_launcher(
    app: &std::path::Path,
    home: &std::path::Path,
    extra_env: &[(&str, &std::path::Path)],
) -> std::process::Output {
    let launcher = app.join("Contents/MacOS/Treasure Adventure Game");
    let mut cmd = std::process::Command::new(&launcher);
    cmd.env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("GOGO_LAUNCHER_NO_DIALOG", "1");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

#[test]
fn v2_bundle_is_launcher_only_and_never_written() {
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    // SAFETY: env mutation under ENV_LOCK.
    unsafe { std::env::set_var("GOGO_HOME", &home) };
    let opts = v2_opts(tmp.path(), &tmp.path().join("no-such-gogo"));
    let plan = macapp::plan(&opts, None, &mut |_| {}).expect("plan");
    assert_eq!(plan.gogid.as_deref(), Some("1207659013"));
    macapp::execute(&plan, CodesignPolicy::Skip, &mut |_| {}).expect("execute");

    // The bundle holds the launcher, the TOML + mirror, the plist and the
    // notice: nothing else (design 16 §0).
    let app = tmp.path().join("out/Treasure Adventure Game.app");
    let names: Vec<String> = tree_digest(&app).into_iter().map(|(n, _)| n).collect();
    assert_eq!(
        names,
        [
            "Contents/",
            "Contents/Info.plist",
            "Contents/MacOS/",
            "Contents/MacOS/Treasure Adventure Game",
            "Contents/Resources/",
            "Contents/Resources/LICENSE-wine.txt",
            "Contents/Resources/gogo-launch.toml",
            "Contents/Resources/launch.conf",
        ]
    );
    let toml = std::fs::read_to_string(app.join("Contents/Resources/gogo-launch.toml")).unwrap();
    let t = macapp::launch::LaunchToml::parse(&toml, std::path::Path::new("t")).unwrap();
    assert_eq!(t.gogid, "1207659013");
    assert_eq!(t.exe, r"C:\GOG\Treasure Adventure Game\Game.exe");
    assert_eq!(t.workdir, r"C:\GOG\Treasure Adventure Game");
    assert!(t.runtime.starts_with("path:"), "{}", t.runtime);
    assert_eq!(t.gogo.state, home);

    // State dir: prefix, game clone, state.toml, effective launch.conf.
    let g = home.join("games/1207659013");
    assert!(g.join("game/data/level1.dat").is_file());
    let link = g.join("prefix/drive_c/GOG/Treasure Adventure Game");
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        std::path::Path::new("../../../game")
    );
    assert!(link.join("Game.exe").is_file());
    assert!(g.join("prefix/dosdevices/c:").symlink_metadata().is_ok());
    assert!(
        g.join("prefix/dosdevices/z:").symlink_metadata().is_err(),
        "z: dropped"
    );
    let state = std::fs::read_to_string(g.join("state.toml")).unwrap();
    assert!(state.contains(&*app.to_string_lossy()), "{state}");
    let conf = std::fs::read_to_string(g.join("launch.conf")).unwrap();
    assert!(conf.contains("PREFIX='games/1207659013/prefix'"), "{conf}");
    let stamp = macapp::launch::stamp(toml.as_bytes(), None);
    assert!(conf.contains(&format!("STAMP='{stamp}'")), "{conf}");

    // Run the launcher against the fake wine: Wine gets the Windows path,
    // starts in the game dir, uses the state prefix; the bundle is unchanged.
    let before = tree_digest(&app);
    let wine_log = tmp.path().join("wine.log");
    std::fs::write(&wine_log, "").unwrap();
    let out = run_launcher(&app, &home, &[]);
    assert!(
        out.status.success(),
        "launcher: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let logged = std::fs::read_to_string(&wine_log).unwrap();
    assert!(
        logged.contains(r"C:\GOG\Treasure Adventure Game\Game.exe"),
        "{logged}"
    );
    assert!(
        logged.contains(&format!("ENV:{}", g.join("prefix").display())),
        "{logged}"
    );
    assert!(
        logged.contains("CWD:") && logged.contains("drive_c/GOG/Treasure Adventure Game"),
        "{logged}"
    );
    assert_eq!(
        before,
        tree_digest(&app),
        "nothing under the bundle changed"
    );
    let launch_log =
        std::fs::read_to_string(home.join("Library/Logs/gogo/1207659013/launch.log")).unwrap();
    assert!(
        !launch_log.contains("mac-app setup"),
        "stamp fresh, no setup: {launch_log}"
    );
    unsafe { std::env::remove_var("GOGO_HOME") };
    drop(guard);
}

#[test]
fn v2_launcher_runs_setup_once_after_l4_edit() {
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    unsafe { std::env::set_var("GOGO_HOME", &home) };
    // A fake gogo: logs its argv and does what setup does to the stamp.
    let gogo = tmp.path().join("fake-gogo");
    let calls = tmp.path().join("gogo-calls.log");
    std::fs::write(
        &gogo,
        format!(
            r#"#!/bin/bash
echo "$*" >> "{calls}"
g="{home}/games/$3"
new="$(cat "$5/Contents/Resources/gogo-launch.toml" "{home}/compat/$3.toml" 2>/dev/null | /usr/bin/shasum -a 256)"
grep -v '^STAMP=' "$g/launch.conf" > "$g/launch.conf.new"
echo "STAMP='${{new%% *}}'" >> "$g/launch.conf.new"
mv "$g/launch.conf.new" "$g/launch.conf"
"#,
            calls = calls.display(),
            home = home.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gogo, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let opts = v2_opts(tmp.path(), &gogo);
    let plan = macapp::plan(&opts, None, &mut |_| {}).unwrap();
    macapp::execute(&plan, CodesignPolicy::Skip, &mut |_| {}).unwrap();
    let app = tmp.path().join("out/Treasure Adventure Game.app");

    // Fresh: no setup.
    assert!(run_launcher(&app, &home, &[]).status.success());
    assert!(!calls.exists());
    // Edit L4 (what `--retarget` does): stale, setup runs exactly once.
    macapp::compat::set_user_keys("1207659013", &[("renderer", Some("dxmt"))]).unwrap();
    assert!(run_launcher(&app, &home, &[]).status.success());
    assert!(run_launcher(&app, &home, &[]).status.success());
    let logged = std::fs::read_to_string(&calls).unwrap();
    assert_eq!(logged.lines().count(), 1, "{logged}");
    assert_eq!(
        logged.trim(),
        format!("mac-app setup 1207659013 --bundle {}", app.display())
    );
    unsafe { std::env::remove_var("GOGO_HOME") };
    drop(guard);
}

#[test]
fn v2_real_setup_applies_l4_and_restamps() {
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    unsafe { std::env::set_var("GOGO_HOME", &home) };
    let opts = v2_opts(tmp.path(), &tmp.path().join("no-such-gogo"));
    let plan = macapp::plan(&opts, None, &mut |_| {}).unwrap();
    macapp::execute(&plan, CodesignPolicy::Skip, &mut |_| {}).unwrap();
    let g = home.join("games/1207659013");

    std::fs::create_dir_all(home.join("compat")).unwrap();
    std::fs::write(
        home.join("compat/1207659013.toml"),
        "# mine\ndll_overrides = { d3d9 = \"n,b\" }\nargs = [\"-windowed\"]\n",
    )
    .unwrap();
    // A deleted prefix is recreated (wineboot through the bundle's runtime),
    // re-linked, and z:-free.
    std::fs::remove_dir_all(g.join("prefix")).unwrap();
    let out = macapp::setup::setup("1207659013", None).unwrap();
    assert!(out.prefix_created);
    assert!(
        g.join("prefix/drive_c/GOG/Treasure Adventure Game/Game.exe")
            .is_file()
    );
    assert!(g.join("prefix/dosdevices/z:").symlink_metadata().is_err());
    let conf = std::fs::read_to_string(g.join("launch.conf")).unwrap();
    assert!(conf.contains("ARGS=('-windowed')"), "{conf}");
    assert!(conf.contains("DLL_OVERRIDES='d3d9=n,b'"), "{conf}");

    // The launcher sees a fresh stamp and exports the override.
    let wine_log = tmp.path().join("wine.log");
    std::fs::write(&wine_log, "").unwrap();
    let app = tmp.path().join("out/Treasure Adventure Game.app");
    assert!(run_launcher(&app, &home, &[]).status.success());
    let logged = std::fs::read_to_string(&wine_log).unwrap();
    assert!(logged.contains("DLLOV:d3d9=n,b"), "{logged}");
    assert!(logged.contains("Game.exe -windowed"), "{logged}");
    unsafe { std::env::remove_var("GOGO_HOME") };
    drop(guard);
}

/// #67 / #55: the prefix holds no link out of the state dir after a build
/// or a setup, L4 registry values and verbs are applied (verbs with `z:`
/// back for the run only), and a runtime gogo can't install names the fix.
/// #72: the build bakes the L1 auto verbs into `gogo-launch.toml`; setup
/// runs winetricks for them once, not again while nothing changes, and L4
/// replaces them whole.
#[test]
fn v2_auto_verbs_are_baked_and_run_once() {
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    unsafe { std::env::set_var("GOGO_HOME", &home) };
    let wt_log = tmp.path().join("winetricks.log");
    let wt = tmp.path().join("winetricks");
    std::fs::write(
        &wt,
        format!(
            "#!/bin/sh\nprintf '%s|%s\\n' \"$*\" \"$([ \"$WINE64\" = \"$WINE\" ] && echo wine64ok)\" >> '{}'\n",
            wt_log.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wt, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    unsafe { std::env::set_var("WINETRICKS", &wt) };
    let runs = || {
        std::fs::read_to_string(&wt_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>()
    };

    let opts = v2_opts(tmp.path(), &tmp.path().join("no-such-gogo"));
    let mut plan = macapp::plan(&opts, None, &mut |_| {}).unwrap();
    // The fixture's Game.exe is no PE: no scan result, so set L1 by hand
    // (the scan → verb mapping is unit-tested in macapp::verbs).
    assert_eq!(plan.auto_verbs.as_deref(), Some(&[][..]));
    plan.auto_verbs = Some(vec![
        macapp::verbs::AutoVerb {
            verb: "d3dcompiler_43".into(),
            imports: vec!["d3dcompiler_43.dll".into()],
        },
        macapp::verbs::AutoVerb {
            verb: "d3dx11_43".into(),
            imports: vec!["d3dx11_43.dll".into()],
        },
    ]);
    macapp::execute(&plan, CodesignPolicy::Skip, &mut |_| {}).unwrap();
    let toml =
        std::fs::read_to_string(plan.bundle_path.join("Contents/Resources/gogo-launch.toml"))
            .unwrap();
    assert!(
        toml.contains(r#"verbs = ["d3dcompiler_43", "d3dx11_43"]"#),
        "{toml}"
    );
    assert_eq!(runs(), ["-q d3dcompiler_43 d3dx11_43|wine64ok"]);

    // Nothing changed: no winetricks. An L4 change elsewhere: still none.
    macapp::setup::setup("1207659013", None).unwrap();
    std::fs::create_dir_all(home.join("compat")).unwrap();
    let l4 = home.join("compat/1207659013.toml");
    std::fs::write(&l4, "env = { WINEMSYNC = \"1\" }\n").unwrap();
    macapp::setup::setup("1207659013", None).unwrap();
    assert_eq!(runs().len(), 1, "{:?}", runs());

    // L4 replaces L1 whole: `[]` runs nothing, a new list runs it.
    std::fs::write(&l4, "verbs = []\n").unwrap();
    let out = macapp::setup::setup("1207659013", None).unwrap();
    assert!(out.state.verbs.is_empty());
    std::fs::write(&l4, "verbs = [\"xinput\"]\n").unwrap();
    macapp::setup::setup("1207659013", None).unwrap();
    assert_eq!(runs().last().unwrap(), "-q xinput|wine64ok");
    assert_eq!(runs().len(), 2);

    // A new prefix gets its verbs again.
    std::fs::remove_dir_all(home.join("games/1207659013/prefix")).unwrap();
    macapp::setup::setup("1207659013", None).unwrap();
    assert_eq!(runs().len(), 3);

    unsafe { std::env::remove_var("WINETRICKS") };
    unsafe { std::env::remove_var("GOGO_HOME") };
    drop(guard);
}

#[test]
fn v2_setup_localizes_prefix_and_applies_registry_and_verbs() {
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    unsafe { std::env::set_var("GOGO_HOME", &home) };
    let opts = v2_opts(tmp.path(), &tmp.path().join("no-such-gogo"));
    let plan = macapp::plan(&opts, None, &mut |_| {}).unwrap();
    macapp::execute(&plan, CodesignPolicy::Skip, &mut |_| {}).unwrap();
    let g = home.join("games/1207659013");

    fn absolute_links(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let ty = e.file_type().unwrap();
            if ty.is_symlink() {
                if std::fs::read_link(e.path()).unwrap().is_absolute() {
                    out.push(e.path());
                }
            } else if ty.is_dir() {
                absolute_links(&e.path(), out);
            }
        }
    }
    let check = |when: &str| {
        let mut abs = Vec::new();
        absolute_links(&g.join("prefix"), &mut abs);
        assert!(abs.is_empty(), "{when}: {abs:?}");
        let user = std::env::var("USER").unwrap_or_else(|_| "u".into());
        let docs = g.join(format!("prefix/drive_c/users/{user}/Documents"));
        assert!(docs.is_dir(), "{when}: {}", docs.display());
        let mut drives: Vec<String> = std::fs::read_dir(g.join("prefix/dosdevices"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        drives.sort();
        assert_eq!(drives, ["c:"], "{when}");
    };
    check("after build");

    // L4: registry + a verb, through a fake winetricks.
    let wt_log = tmp.path().join("winetricks.log");
    let wt = tmp.path().join("winetricks");
    std::fs::write(
        &wt,
        format!(
            "#!/bin/sh\nprintf '%s|%s|%s\\n' \"$*\" \"$WINE\" \"$(readlink \"$WINEPREFIX/dosdevices/z:\")\" >> '{}'\n",
            wt_log.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wt, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    unsafe { std::env::set_var("WINETRICKS", &wt) };
    std::fs::create_dir_all(home.join("compat")).unwrap();
    std::fs::write(
        home.join("compat/1207659013.toml"),
        "verbs = [\"d3dx9\"]\n\n[[registry]]\nkey = 'HKCU\\Software\\Wine\\Direct3D'\nname = \"csmt\"\ntype = \"REG_DWORD\"\ndata = \"0\"\n",
    )
    .unwrap();
    let wine_log = tmp.path().join("wine.log");
    std::fs::write(&wine_log, "").unwrap();
    let out = macapp::setup::setup("1207659013", None).unwrap();
    unsafe { std::env::remove_var("WINETRICKS") };
    assert!(!out.prefix_created);
    let logged = std::fs::read_to_string(&wine_log).unwrap();
    assert!(
        logged.contains(r"reg add HKCU\Software\Wine\Direct3D /v csmt /t REG_DWORD /d 0 /f"),
        "{logged}"
    );
    let tricks = std::fs::read_to_string(&wt_log).unwrap();
    assert!(tricks.starts_with("-q d3dx9|"), "{tricks}");
    assert!(
        tricks.contains("runtime/bin/wine|/"),
        "z: during verbs: {tricks}"
    );
    check("after setup with verbs");

    // A deleted prefix comes back localized too.
    std::fs::remove_file(home.join("compat/1207659013.toml")).unwrap();
    std::fs::remove_dir_all(g.join("prefix")).unwrap();
    assert!(
        macapp::setup::setup("1207659013", None)
            .unwrap()
            .prefix_created
    );
    check("after re-creating the prefix");

    // A runtime that isn't in the store and isn't in the catalog: the error
    // says how to install one.
    std::fs::write(
        home.join("compat/1207659013.toml"),
        "runtime = \"no-such-runtime/1.0\"\n",
    )
    .unwrap();
    let err = macapp::setup::setup("1207659013", None).unwrap_err();
    assert!(err.to_string().contains("gogo runtimes install"), "{err}");
    unsafe { std::env::remove_var("GOGO_HOME") };
    drop(guard);
}

#[test]
fn v2_missing_prefix_without_gogo_refuses_to_launch() {
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    unsafe { std::env::set_var("GOGO_HOME", &home) };
    let opts = v2_opts(tmp.path(), &tmp.path().join("no-such-gogo"));
    let plan = macapp::plan(&opts, None, &mut |_| {}).unwrap();
    macapp::execute(&plan, CodesignPolicy::Skip, &mut |_| {}).unwrap();
    std::fs::remove_dir_all(home.join("games/1207659013/prefix")).unwrap();
    let wine_log = tmp.path().join("wine.log");
    std::fs::write(&wine_log, "").unwrap();
    let app = tmp.path().join("out/Treasure Adventure Game.app");
    let out = run_launcher(&app, &home, &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        !std::fs::read_to_string(&wine_log).unwrap().contains("C:"),
        "wine never ran"
    );
    let launch_log =
        std::fs::read_to_string(home.join("Library/Logs/gogo/1207659013/launch.log")).unwrap();
    assert!(launch_log.contains("no gogo found"), "{launch_log}");
    assert!(launch_log.contains("not launching"), "{launch_log}");
    unsafe { std::env::remove_var("GOGO_HOME") };
    drop(guard);
}

/// #42: SIGTERM to the launcher runs `wineserver -k` against this prefix and
/// SIGKILLs what's left of the game's process group; an unrelated process
/// stays alive.
#[test]
fn v2_launcher_teardown_reaps_its_own_prefix_only() {
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    unsafe { std::env::set_var("GOGO_HOME", &home) };
    let opts = v2_opts(tmp.path(), &tmp.path().join("no-such-gogo"));
    let plan = macapp::plan(&opts, None, &mut |_| {}).unwrap();
    macapp::execute(&plan, CodesignPolicy::Skip, &mut |_| {}).unwrap();
    let app = tmp.path().join("out/Treasure Adventure Game.app");
    let wine_log = tmp.path().join("wine.log");
    std::fs::write(&wine_log, "").unwrap();

    let mut unrelated = std::process::Command::new("sleep")
        .arg("300")
        .spawn()
        .unwrap();
    let pidfile = tmp.path().join("daemon.pid");
    let mut launcher =
        std::process::Command::new(app.join("Contents/MacOS/Treasure Adventure Game"))
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("FAKE_WINE_HANG", &pidfile)
            .env("GOGO_LAUNCHER_NO_DIALOG", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !pidfile.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let daemon: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let alive = |pid: i32| {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    };
    assert!(alive(daemon));
    std::process::Command::new("kill")
        .args(["-TERM", &launcher.id().to_string()])
        .status()
        .unwrap();
    let status = launcher.wait().unwrap();
    assert_eq!(status.code(), Some(143));
    // The TERM-ignoring child is gone (zombie at most, reaped by init).
    std::thread::sleep(std::time::Duration::from_millis(200));
    let ps = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &daemon.to_string()])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&ps.stdout);
    assert!(
        stat.trim().is_empty() || stat.contains('Z'),
        "daemon {daemon} still {stat}"
    );
    assert!(
        unrelated.try_wait().unwrap().is_none(),
        "unrelated process untouched"
    );
    unrelated.kill().ok();
    let logged = std::fs::read_to_string(&wine_log).unwrap();
    assert!(logged.contains("wineserver -k"), "{logged}");
    let launch_log =
        std::fs::read_to_string(home.join("Library/Logs/gogo/1207659013/launch.log")).unwrap();
    assert!(launch_log.contains("got SIGTERM"), "{launch_log}");
    assert!(launch_log.contains("reaped (SIGKILL)"), "{launch_log}");
    let run_log =
        std::fs::read_to_string(home.join("Library/Logs/gogo/1207659013/latest.log")).unwrap();
    assert!(run_log.contains("# exit: 143 (SIGTERM)\n"), "{run_log}");
    unsafe { std::env::remove_var("GOGO_HOME") };
    drop(guard);
}

/// #77: each launcher run writes `<ts>-<pid>.log` under the game's log dir
/// (header, Wine's output, exit code and duration), `latest.log` points at
/// it, and only the newest 10 run logs are kept.
#[test]
fn v2_launcher_writes_a_rotated_run_log() {
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    unsafe { std::env::set_var("GOGO_HOME", &home) };
    let opts = v2_opts(tmp.path(), &tmp.path().join("no-such-gogo"));
    let plan = macapp::plan(&opts, None, &mut |_| {}).unwrap();
    macapp::execute(&plan, CodesignPolicy::Skip, &mut |_| {}).unwrap();
    let app = tmp.path().join("out/Treasure Adventure Game.app");
    let logs = home.join("Library/Logs/gogo/1207659013");
    // Twelve older runs: after this one, the oldest three are gone.
    std::fs::create_dir_all(&logs).unwrap();
    for i in 0..12 {
        std::fs::write(logs.join(format!("20000101-0000{i:02}-1.log")), "old").unwrap();
    }

    let out = run_launcher(
        &app,
        &home,
        &[("FAKE_WINE_EXIT", std::path::Path::new("3"))],
    );
    assert_eq!(out.status.code(), Some(3));
    // Teed: the launcher's own streams still carry Wine's output.
    assert!(String::from_utf8_lossy(&out.stdout).contains("fake-wine stdout line"));
    assert!(String::from_utf8_lossy(&out.stderr).contains("fake-wine stderr line"));

    let mut runs: Vec<String> = std::fs::read_dir(&logs)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.as_bytes()[0].is_ascii_digit() && n.ends_with(".log"))
        .collect();
    runs.sort();
    assert_eq!(runs.len(), 10, "{runs:?}");
    assert_eq!(runs[0], "20000101-000003-1.log", "{runs:?}");
    let newest = runs.last().unwrap();
    assert_eq!(
        std::fs::read_link(logs.join("latest.log")).unwrap(),
        std::path::Path::new(newest)
    );
    let log = std::fs::read_to_string(logs.join(newest)).unwrap();
    for want in [
        "# gogo run log: Treasure Adventure Game (1207659013)\n",
        "# runtime: path:",
        "# renderer: ",
        "# verbs: ",
        "# stamp: ",
        "# macos: ",
        "# exe: C:\\GOG\\Treasure Adventure Game\\Game.exe\n",
        "fake-wine stdout line\n",
        "fake-wine stderr line\n",
        "# exit: 3\n",
        "# duration: ",
    ] {
        assert!(log.contains(want), "{want:?} in:\n{log}");
    }
    let stamp = macapp::launch::stamp(
        &std::fs::read(app.join("Contents/Resources/gogo-launch.toml")).unwrap(),
        None,
    );
    assert!(log.contains(&format!("# stamp: {stamp}\n")), "{log}");
    // The footer comes after everything Wine wrote.
    assert!(log.find("# exit: 3").unwrap() > log.find("fake-wine stderr line").unwrap());
    let launch_log = std::fs::read_to_string(logs.join("launch.log")).unwrap();
    assert!(launch_log.contains(newest.as_str()), "{launch_log}");
    unsafe { std::env::remove_var("GOGO_HOME") };
    drop(guard);
}

fn write_exec(path: &std::path::Path, script: &str) {
    std::fs::write(path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// #78: a game that quits after 1 s gets the early-exit dialog; "Try next
/// renderer" writes L4 through gogo and relaunches, the relaunch runs setup
/// on the stale stamp and Wine starts on the new renderer; with nothing left
/// to try, the dialog offers Close, and "Copy report" puts the report on the
/// clipboard.
#[test]
fn v2_early_exit_dialog_tries_the_next_renderer() {
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    unsafe { std::env::set_var("GOGO_HOME", &home) };
    // A fake gogo: `next-renderer` answers wined3d until L4 names it, then
    // runs out; `setup` applies L4's renderer and restamps, like the real one.
    let gogo = tmp.path().join("fake-gogo");
    let calls = tmp.path().join("gogo-calls.log");
    write_exec(
        &gogo,
        &format!(
            r#"#!/bin/bash
echo "$*" >> "{calls}"
l4="{home}/compat/$3.toml"
g="{home}/games/$3"
case "$2" in
  next-renderer)
    if grep -q wined3d "$l4" 2>/dev/null; then echo "gogo: $3: no more renderers" >&2; exit 1; fi
    [ "${{4:-}}" = --dry-run ] || {{ mkdir -p "{home}/compat"; echo 'renderer = "wined3d"' > "$l4"; }}
    echo wined3d;;
  setup)
    new="$(cat "$5/Contents/Resources/gogo-launch.toml" "$l4" 2>/dev/null | /usr/bin/shasum -a 256)"
    grep -v -e '^STAMP=' -e '^RENDERER=' "$g/launch.conf" > "$g/launch.conf.new"
    echo "RENDERER='wined3d'" >> "$g/launch.conf.new"
    echo "STAMP='${{new%% *}}'" >> "$g/launch.conf.new"
    mv "$g/launch.conf.new" "$g/launch.conf";;
esac
"#,
            calls = calls.display(),
            home = home.display()
        ),
    );
    // The dialog stub: records each dialog, answers from a script of choices.
    let dialogs = tmp.path().join("dialogs.log");
    let answers = tmp.path().join("answers");
    std::fs::write(&answers, "Try next renderer\nCopy report\n").unwrap();
    let dialog = tmp.path().join("dialog");
    write_exec(
        &dialog,
        &format!(
            r#"#!/bin/bash
{{ printf '%s\n' "$@"; echo ===; }} >> "{dialogs}"
head -1 "{answers}"
sed -i.bak 1d "{answers}"
"#,
            dialogs = dialogs.display(),
            answers = answers.display()
        ),
    );
    // pbcopy on PATH: the clipboard is a file.
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let clipboard = tmp.path().join("clipboard");
    write_exec(
        &bin.join("pbcopy"),
        &format!("#!/bin/sh\ncat > \"{}\"\n", clipboard.display()),
    );
    let opts = v2_opts(tmp.path(), &gogo);
    let plan = macapp::plan(&opts, None, &mut |_| {}).unwrap();
    macapp::execute(&plan, CodesignPolicy::Skip, &mut |_| {}).unwrap();
    let app = tmp.path().join("out/Treasure Adventure Game.app");
    // As if the build's setup had applied DXMT (no download in a test).
    let g = home.join("games/1207659013");
    let conf = std::fs::read_to_string(g.join("launch.conf")).unwrap();
    let conf: String = conf
        .lines()
        .map(|l| {
            if l.starts_with("RENDERER=") {
                "RENDERER='dxmt/v0.80'"
            } else {
                l
            }
        })
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(g.join("launch.conf"), conf).unwrap();

    let path = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display());
    let out = run_launcher(
        &app,
        &home,
        &[
            ("FAKE_WINE_EXIT", std::path::Path::new("1")),
            ("FAKE_WINE_SLEEP", std::path::Path::new("1")),
            ("GOGO_LAUNCHER_DIALOG", &dialog),
            ("PATH", std::path::Path::new(&path)),
        ],
    );
    assert_eq!(out.status.code(), Some(1));

    // L4 now names the next renderer, written by gogo.
    let l4 = std::fs::read_to_string(home.join("compat/1207659013.toml")).unwrap();
    assert_eq!(l4.trim(), r#"renderer = "wined3d""#);
    let calls = std::fs::read_to_string(&calls).unwrap();
    let calls: Vec<&str> = calls.lines().collect();
    assert_eq!(
        calls,
        [
            "mac-app next-renderer 1207659013 --dry-run",
            "mac-app next-renderer 1207659013",
            &format!("mac-app setup 1207659013 --bundle {}", app.display()),
            "mac-app next-renderer 1207659013 --dry-run",
        ]
    );
    // Two dialogs: the first offers the switch, the second has nothing left.
    let dialogs = std::fs::read_to_string(&dialogs).unwrap();
    let shown: Vec<&str> = dialogs.split("===\n").filter(|d| !d.is_empty()).collect();
    assert_eq!(shown.len(), 2, "{dialogs}");
    assert!(
        shown[0].starts_with("Treasure Adventure Game\n"),
        "{}",
        shown[0]
    );
    assert!(
        shown[0].contains("exit code 1) on the dxmt/v0.80 renderer"),
        "{}",
        shown[0]
    );
    assert!(shown[0].contains("fake-wine stderr line"), "{}", shown[0]);
    assert!(shown[0].contains("switches to wined3d"), "{}", shown[0]);
    assert!(
        shown[0].ends_with("Copy report\nOpen log\nTry next renderer\n"),
        "{}",
        shown[0]
    );
    assert!(shown[1].contains("on the wined3d renderer"), "{}", shown[1]);
    assert!(shown[1].contains("no more renderers"), "{}", shown[1]);
    assert!(
        shown[1].ends_with("Copy report\nOpen log\nClose\n"),
        "{}",
        shown[1]
    );

    // The relaunch ran Wine on the new renderer, in a run log of its own.
    let logs = home.join("Library/Logs/gogo/1207659013");
    let mut runs: Vec<String> = std::fs::read_dir(&logs)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.as_bytes()[0].is_ascii_digit())
        .collect();
    runs.sort();
    assert_eq!(runs.len(), 2, "{runs:?}");
    let first = std::fs::read_to_string(logs.join(&runs[0])).unwrap();
    let second = std::fs::read_to_string(logs.join(&runs[1])).unwrap();
    assert!(first.contains("# renderer: dxmt/v0.80\n"), "{first}");
    assert!(second.contains("# renderer: wined3d\n"), "{second}");
    assert!(second.contains("# exit: 1\n"), "{second}");

    // Copy report: identity, config, macOS and the log tail; no home paths.
    let report = std::fs::read_to_string(&clipboard).unwrap();
    for want in [
        "gogo launch report: Treasure Adventure Game (1207659013)\n",
        "# renderer: wined3d\n",
        "# runtime: path:",
        "# macos: ",
        "# exit: 1\n",
    ] {
        assert!(report.contains(want), "{want:?} in:\n{report}");
    }
    // Wine's stdout and stderr reach the log through separate pipes, so the
    // two lines can land in either order.
    let (_, tail) = report
        .split_once("--- last 40 log lines ---\n")
        .unwrap_or_else(|| panic!("no log tail in:\n{report}"));
    for want in ["fake-wine stdout line\n", "fake-wine stderr line\n"] {
        assert!(tail.contains(want), "{want:?} in:\n{report}");
    }
    assert!(!report.contains("# prefix:"), "{report}");
    unsafe { std::env::remove_var("GOGO_HOME") };
    drop(guard);
}

/// #78, the real `next_renderer`: after what setup last applied, in this
/// game's order, written to L4; then nothing left.
#[test]
fn v2_next_renderer_writes_l4_then_runs_out() {
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    unsafe { std::env::set_var("GOGO_HOME", &home) };
    let opts = v2_opts(tmp.path(), &tmp.path().join("no-such-gogo"));
    let plan = macapp::plan(&opts, None, &mut |_| {}).unwrap();
    macapp::execute(&plan, CodesignPolicy::Skip, &mut |_| {}).unwrap();
    let state_path = home.join("games/1207659013/state.toml");
    let state = std::fs::read_to_string(&state_path).unwrap();
    assert!(state.contains("renderer = \"wined3d\""), "{state}");
    // TAG's stub exe doesn't scan: its order is wined3d alone.
    assert_eq!(
        macapp::setup::next_renderer("1207659013", false).unwrap(),
        None
    );
    let l4 = home.join("compat/1207659013.toml");
    assert!(!l4.exists());

    // As if the last setup had applied DXMT: wined3d is next, and a dry run
    // writes nothing.
    std::fs::write(
        &state_path,
        state.replace("renderer = \"wined3d\"", "renderer = \"dxmt/v0.80\""),
    )
    .unwrap();
    assert_eq!(
        macapp::setup::next_renderer("1207659013", true)
            .unwrap()
            .as_deref(),
        Some("wined3d")
    );
    assert!(!l4.exists());
    assert_eq!(
        macapp::setup::next_renderer("1207659013", false)
            .unwrap()
            .as_deref(),
        Some("wined3d")
    );
    assert_eq!(
        std::fs::read_to_string(&l4).unwrap().trim(),
        r#"renderer = "wined3d""#
    );
    // Setup applies it; the order is used up.
    let out = macapp::setup::setup("1207659013", None).unwrap();
    assert_eq!(out.state.renderer, "wined3d");
    assert_eq!(
        macapp::setup::next_renderer("1207659013", false).unwrap(),
        None
    );
    unsafe { std::env::remove_var("GOGO_HOME") };
    drop(guard);
}

// — L1 renderer selection (#70) —

/// A minimal PE32/PE32+ image importing `imports` (one `.rdata` section with
/// the import descriptors and names): enough for the import scanner.
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

/// A depot tree for `gogid` whose `Game.exe` imports `imports`.
fn depot_game(
    dir: &std::path::Path,
    gogid: &str,
    name: &str,
    pe64: bool,
    imports: &[&str],
) -> std::path::PathBuf {
    let tree = dir.join("depot").join(name);
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(
        tree.join(format!("goggame-{gogid}.info")),
        format!(
            r#"{{"name":"{name}","rootGameId":"{gogid}","playTasks":[{{"type":"FileTask","category":"game","path":"Game.exe","isPrimary":true}}]}}"#
        ),
    )
    .unwrap();
    std::fs::write(tree.join("Game.exe"), pe_with_imports(pe64, imports)).unwrap();
    tree
}

fn renderer_events(
    sink: &std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) -> impl FnMut(&ProgressEvent) + use<> {
    let sink = sink.clone();
    move |ev| {
        if let ProgressEvent::RendererSelected {
            renderer, layer, ..
        } = ev
        {
            sink.lock().unwrap().push(format!("{renderer} {layer}"));
        }
    }
}

#[test]
fn d3d11_game_runs_from_a_derived_dxmt_runtime() {
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    // SAFETY: env mutation under ENV_LOCK.
    unsafe { std::env::set_var("GOGO_HOME", &home) };

    // The default engine in the store (a fake that reports a CX version),
    // with the builtins DXMT replaces …
    let base = home.join("runtimes/winecx/24.0.7_7");
    fake_runtime(&base.join("engine"), &tmp.path().join("wine.log"));
    let wine = base.join("engine/bin/wine");
    let script = std::fs::read_to_string(&wine)
        .unwrap()
        .replace("wine-9.0-fake", "wine-9.0 (SikarugirCX 24.0.7)");
    std::fs::write(&wine, script).unwrap();
    let lib = base.join("engine/lib/wine");
    for arch in ["x86_64-windows", "i386-windows", "x86_64-unix"] {
        std::fs::create_dir_all(lib.join(arch)).unwrap();
    }
    std::fs::write(lib.join("i386-windows/d3d11.dll"), "wined3d").unwrap();
    std::fs::create_dir_all(base.join("Frameworks")).unwrap();
    let m = |id: &str, version: &str, entry_rel: &str| macapp::store::ManifestEntry {
        id: id.into(),
        version: version.into(),
        sha256: "0".into(),
        fetched_at: "2026-10-23T00:00:00Z".into(),
        upstream_url: "http://x".into(),
        entry_rel: entry_rel.into(),
        bytes: 1,
    };
    macapp::store::save_manifest(&[m("winecx", "24.0.7_7", "engine/bin/wine")]).unwrap();
    // … and DXMT in the renderer store, as downloaded.
    let dxmt = home.join("renderers/dxmt/v0.80/x86_64/v0.80");
    for (rel, body) in [
        ("i386-windows/d3d11.dll", "dxmt"),
        ("x86_64-windows/d3d11.dll", "dxmt"),
        ("x86_64-unix/winemetal.so", "metal"),
    ] {
        std::fs::create_dir_all(dxmt.join(rel).parent().unwrap()).unwrap();
        std::fs::write(dxmt.join(rel), body).unwrap();
    }
    macapp::store::save_renderer_manifest(&[m("dxmt", "v0.80", "v0.80/x86_64-unix/winemetal.so")])
        .unwrap();

    // Rebel Galaxy's shape: a 32-bit exe importing d3d11.
    let tree = depot_game(
        tmp.path(),
        "1435582019",
        "Rebel",
        false,
        &["kernel32.dll", "d3d11.dll", "dxgi.dll"],
    );
    let opts = MacAppOptions {
        installer: None,
        game_tree: Some(tree),
        name: None,
        mode: RuntimeMode::Catalog("winecx".into()),
        runtime_path: None,
        out_dir: tmp.path().join("out"),
        overwrite: true,
        recipe: Default::default(),
        runtime_menu: false,
        runtime_version: None,
        lutris_base: None,
        renderer: macapp::RendererChoice::Auto,
        portable: false,
        gogo_path: Some(tmp.path().join("no-such-gogo")),
    };
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let plan = macapp::plan(&opts, None, &mut renderer_events(&events)).expect("plan");
    let sel = plan
        .renderer
        .as_ref()
        .expect("depot lane decides at plan time");
    assert_eq!(sel.renderer.value, "dxmt/v0.80");
    assert_eq!(sel.renderer.layer, macapp::compat::Layer::Auto);
    assert!(
        sel.describe().contains("imports d3d11, dxgi"),
        "{}",
        sel.describe()
    );
    assert_eq!(*events.lock().unwrap(), ["dxmt/v0.80 L1 auto"]);
    macapp::execute(&plan, CodesignPolicy::Skip, &mut |_| {}).expect("execute");

    let app = tmp.path().join("out/Rebel.app");
    let toml = std::fs::read_to_string(app.join("Contents/Resources/gogo-launch.toml")).unwrap();
    let t = macapp::launch::LaunchToml::parse(&toml, std::path::Path::new("t")).unwrap();
    assert_eq!(
        t.runtime, "winecx/24.0.7_7",
        "the bundle names the base runtime"
    );
    assert_eq!(t.renderer, "dxmt/v0.80");
    assert!(t.dll_overrides.is_empty(), "no n,b for an engine overlay");
    let notice = std::fs::read_to_string(app.join("Contents/Resources/LICENSE-dxmt.txt")).unwrap();
    assert!(
        notice.contains("LGPL-2.1") && notice.contains("3Shain/dxmt"),
        "{notice}"
    );

    // Setup ran the game on the derived runtime; the base is unchanged.
    let derived = home.join("runtimes/winecx/24.0.7_7+dxmt-v0.80");
    assert_eq!(
        std::fs::read_to_string(derived.join("engine/lib/wine/i386-windows/d3d11.dll")).unwrap(),
        "dxmt"
    );
    assert!(
        derived
            .join("engine/lib/wine/x86_64-unix/winemetal.so")
            .is_file()
    );
    assert_eq!(
        std::fs::read_to_string(lib.join("i386-windows/d3d11.dll")).unwrap(),
        "wined3d"
    );
    assert!(!lib.join("x86_64-unix/winemetal.so").exists());
    let g = home.join("games/1435582019");
    let conf = std::fs::read_to_string(g.join("launch.conf")).unwrap();
    assert!(
        conf.contains("RUNTIME_DIR='runtimes/winecx/24.0.7_7+dxmt-v0.80/engine'"),
        "{conf}"
    );
    assert!(
        conf.contains("FRAMEWORKS='runtimes/winecx/24.0.7_7+dxmt-v0.80/Frameworks'"),
        "{conf}"
    );
    assert!(conf.contains("DLL_OVERRIDES=''"), "{conf}");
    let state = macapp::launch::GameState::load(&g.join("state.toml"))
        .unwrap()
        .unwrap();
    assert_eq!(state.runtime, "winecx/24.0.7_7+dxmt-v0.80");
    assert_eq!(state.renderer, "dxmt/v0.80");
    // The store's reference list keeps both from `prune`.
    let refs: Vec<String> = macapp::store::references()
        .into_iter()
        .map(|r| r.reference)
        .collect();
    assert!(refs.iter().any(|r| r == "winecx/24.0.7_7+dxmt-v0.80"));
    assert!(refs.iter().any(|r| r == "dxmt/v0.80"));

    // An L4 switch to wined3d moves the game back to the base runtime.
    macapp::compat::set_user_keys("1435582019", &[("renderer", Some("wined3d"))]).unwrap();
    let out = macapp::setup::setup("1435582019", None).unwrap();
    assert_eq!(out.state.runtime, "winecx/24.0.7_7");
    assert_eq!(out.state.renderer, "wined3d");
    unsafe { std::env::remove_var("GOGO_HOME") };
    drop(guard);
}

#[test]
fn ddraw_game_gets_cnc_ddraw_until_l4_says_otherwise() {
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let wrapper = tmp.path().join("cnc-ddraw");
    std::fs::create_dir_all(&wrapper).unwrap();
    std::fs::write(wrapper.join("ddraw.dll"), b"MZ-ddraw").unwrap();
    // SAFETY: env mutation under ENV_LOCK.
    unsafe {
        std::env::set_var("GOGO_HOME", &home);
        std::env::set_var("GOGO_CNC_DDRAW_DIR", &wrapper);
    }
    let runtime_dir = tmp.path().join("runtime");
    fake_runtime(&runtime_dir, &tmp.path().join("wine.log"));
    // Pharaoh's shape: a 32-bit exe importing only ddraw.
    let tree = depot_game(
        tmp.path(),
        "1207659013",
        "Pharaoh",
        false,
        &["kernel32.dll", "ddraw.dll"],
    );
    let opts = MacAppOptions {
        installer: None,
        game_tree: Some(tree),
        name: None,
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir),
        out_dir: tmp.path().join("out"),
        overwrite: true,
        recipe: Default::default(),
        runtime_menu: false,
        runtime_version: None,
        lutris_base: None,
        renderer: macapp::RendererChoice::Auto,
        portable: false,
        gogo_path: Some(tmp.path().join("no-such-gogo")),
    };
    let plan = macapp::plan(&opts, None, &mut |_| {}).expect("plan");
    assert_eq!(
        plan.renderer.as_ref().unwrap().renderer.value,
        "cnc-ddraw/7.1.0.0"
    );
    let mut steps = Vec::new();
    macapp::execute(&plan, CodesignPolicy::Skip, &mut |ev| {
        if let ProgressEvent::RecipeStep { verb } = ev {
            steps.push(verb.clone());
        }
    })
    .expect("execute");
    assert!(
        steps.iter().any(|s| s.starts_with("cnc-ddraw")),
        "{steps:?}"
    );

    let app = tmp.path().join("out/Pharaoh.app");
    assert!(
        app.join("Contents/Resources/LICENSE-cnc-ddraw.txt")
            .is_file()
    );
    let g = home.join("games/1207659013");
    assert_eq!(
        std::fs::read(g.join("game/ddraw.dll")).unwrap(),
        b"MZ-ddraw"
    );
    assert!(g.join("game/ddraw.ini").is_file());
    let conf = std::fs::read_to_string(g.join("launch.conf")).unwrap();
    assert!(conf.contains("DLL_OVERRIDES='ddraw=n,b'"), "{conf}");
    assert!(conf.contains("RENDERER='cnc-ddraw/7.1.0.0'"), "{conf}");

    // `--no-ddraw-wrapper` semantics at the layer level: L4 → wined3d
    // removes gogo's wrapper and its override again.
    macapp::compat::set_user_keys("1207659013", &[("renderer", Some("wined3d"))]).unwrap();
    let out = macapp::setup::setup("1207659013", None).unwrap();
    assert_eq!(out.state.renderer, "wined3d");
    assert!(!g.join("game/ddraw.dll").exists());
    let conf = std::fs::read_to_string(g.join("launch.conf")).unwrap();
    assert!(conf.contains("DLL_OVERRIDES=''"), "{conf}");
    unsafe {
        std::env::remove_var("GOGO_CNC_DDRAW_DIR");
        std::env::remove_var("GOGO_HOME");
    }
    drop(guard);
}

// — compat entries drive the build and setup (#74) —

/// An L3 entry (through `GOGO_COMPAT_DIR`) with exe/renderer/verbs/env/args:
/// the plan shows its status, the bundle TOML bakes its exe, renderer, args,
/// env and DLL overrides over L1 and `goggame-*.info`, and setup runs its
/// verbs through winetricks in place of the L1 ones.
#[test]
fn compat_entry_drives_build_and_setup() {
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let db = tmp.path().join("compat-db");
    std::fs::create_dir_all(&db).unwrap();
    std::fs::write(
        db.join("1435582019.toml"),
        r#"
title    = "Rebel Galaxy"
exe      = 'bin\RealGame.exe'
renderer = "wined3d"
verbs    = ["d3dx9_43", "xact"]
env      = { WINEMSYNC = "1" }
args     = ["-windowed"]
dll_overrides = { xinput1_3 = "n,b" }
status   = "boots"
tested   = [{ runtime = "winecx-24.0.7_7", date = 2026-10-22 }]
"#,
    )
    .unwrap();
    let wt_log = tmp.path().join("winetricks.log");
    let wt = tmp.path().join("winetricks");
    std::fs::write(
        &wt,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n",
            wt_log.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wt, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // SAFETY: env mutation under ENV_LOCK.
    unsafe {
        std::env::set_var("GOGO_HOME", &home);
        std::env::set_var("GOGO_COMPAT_DIR", &db);
        std::env::set_var("WINETRICKS", &wt);
    }
    let runtime_dir = tmp.path().join("runtime");
    fake_runtime(&runtime_dir, &tmp.path().join("wine.log"));
    // A D3D11 launcher as the info's game task (L1: dxmt + d3dx11_43), and
    // the real game exe the compat entry points at.
    let tree = depot_game(
        tmp.path(),
        "1435582019",
        "Rebel Galaxy",
        true,
        &["kernel32.dll", "d3d11.dll", "d3dx11_43.dll"],
    );
    std::fs::create_dir_all(tree.join("bin")).unwrap();
    std::fs::write(tree.join("bin/RealGame.exe"), b"MZ").unwrap();
    let opts = MacAppOptions {
        installer: None,
        game_tree: Some(tree),
        name: None,
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir),
        out_dir: tmp.path().join("out"),
        overwrite: true,
        recipe: Default::default(),
        runtime_menu: false,
        runtime_version: None,
        lutris_base: None,
        renderer: macapp::RendererChoice::Auto,
        portable: false,
        gogo_path: Some(tmp.path().join("no-such-gogo")),
    };
    let plan = macapp::plan(&opts, None, &mut |_| {}).expect("plan");
    assert_eq!(
        plan.compat.as_ref().unwrap().status_line(),
        "known to boot on winecx-24.0.7_7 (2026-10-22)"
    );
    let sel = plan.renderer.as_ref().unwrap();
    assert_eq!(
        (sel.renderer.value.as_str(), sel.renderer.layer),
        ("wined3d", macapp::compat::Layer::Db)
    );
    macapp::execute(&plan, CodesignPolicy::Skip, &mut |_| {}).expect("execute");

    let app = tmp.path().join("out/Rebel Galaxy.app");
    let toml = std::fs::read_to_string(app.join("Contents/Resources/gogo-launch.toml")).unwrap();
    let baked = macapp::launch::LaunchToml::parse(&toml, std::path::Path::new("t")).unwrap();
    assert_eq!(baked.exe, r"C:\GOG\Rebel Galaxy\bin\RealGame.exe", "{toml}");
    assert_eq!(baked.workdir, r"C:\GOG\Rebel Galaxy\bin");
    assert_eq!(baked.renderer, "wined3d");
    assert_eq!(baked.args, ["-windowed"]);
    assert_eq!(baked.env["WINEMSYNC"], "1");
    assert_eq!(baked.dll_overrides["xinput1_3"], "n,b");
    // The TOML keeps L1 verbs (design 16 §2.2); setup applies L3's instead.
    assert_eq!(baked.verbs, ["d3dx11_43"]);
    let runs = std::fs::read_to_string(&wt_log).expect("fake winetricks ran");
    assert_eq!(runs.lines().collect::<Vec<_>>(), ["-q d3dx9_43 xact"]);
    let conf = std::fs::read_to_string(home.join("games/1435582019/launch.conf")).unwrap();
    assert!(
        conf.contains(r"EXE='C:\GOG\Rebel Galaxy\bin\RealGame.exe'"),
        "{conf}"
    );
    assert!(conf.contains("VERBS=('d3dx9_43' 'xact')"), "{conf}");
    assert!(conf.contains("DLL_OVERRIDES='xinput1_3=n,b'"), "{conf}");

    unsafe {
        std::env::remove_var("WINETRICKS");
        std::env::remove_var("GOGO_COMPAT_DIR");
        std::env::remove_var("GOGO_HOME");
    }
    drop(guard);
}

/// An L3 `runtime` pins the auto ladder to that store runtime, even when a
/// newer one of the same lane is cached (#74).
#[test]
fn compat_runtime_pins_the_auto_ladder() {
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    for (ver, label) in [("24.0.7_7", "24.0.7"), ("24.0.6_1", "24.0.6")] {
        let bin = home.join(format!("runtimes/winecx/{ver}/engine/bin"));
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(home.join(format!("runtimes/winecx/{ver}/Frameworks"))).unwrap();
        std::fs::write(
            bin.join("wine"),
            format!("#!/bin/sh\necho 'wine-9.0 (SikarugirCX {label})'\n"),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(bin.join("wine"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(
        home.join("runtimes/manifest.json"),
        r#"[{"id":"winecx","version":"24.0.7_7","sha256":"0","fetched_at":"2026-02-01T00:00:00Z","upstream_url":"x","entry_rel":"engine/bin/wine","bytes":16},
{"id":"winecx","version":"24.0.6_1","sha256":"0","fetched_at":"2026-01-01T00:00:00Z","upstream_url":"x","entry_rel":"engine/bin/wine","bytes":16}]"#,
    )
    .unwrap();
    let db = tmp.path().join("compat-db");
    std::fs::create_dir_all(&db).unwrap();
    std::fs::write(
        db.join("1207659013.toml"),
        "runtime = \"winecx/24.0.6_1\"\n",
    )
    .unwrap();
    // SAFETY: env mutation under ENV_LOCK.
    unsafe {
        std::env::set_var("GOGO_HOME", &home);
        std::env::set_var("GOGO_COMPAT_DIR", &db);
    }
    let tree = depot_game(tmp.path(), "1207659013", "Pinned", false, &["kernel32.dll"]);
    let opts = MacAppOptions {
        installer: None,
        game_tree: Some(tree),
        name: None,
        mode: RuntimeMode::Auto,
        runtime_path: None,
        out_dir: tmp.path().join("out"),
        overwrite: true,
        recipe: Default::default(),
        runtime_menu: false,
        runtime_version: None,
        lutris_base: None,
        renderer: macapp::RendererChoice::Auto,
        portable: false,
        gogo_path: None,
    };
    let pinned = macapp::plan(&opts, None, &mut |_| {}).expect("plan");
    std::fs::remove_file(db.join("1207659013.toml")).unwrap();
    let unpinned = macapp::plan(&opts, None, &mut |_| {}).expect("plan");
    unsafe {
        std::env::remove_var("GOGO_COMPAT_DIR");
        std::env::remove_var("GOGO_HOME");
    }
    drop(guard);
    assert_eq!(pinned.runtime.version, "wine-9.0 (SikarugirCX 24.0.6)");
    assert_eq!(unpinned.runtime.version, "wine-9.0 (SikarugirCX 24.0.7)");
    assert_eq!(pinned.compat.unwrap().status_line(), "untested");
}

/// #69: two store runtimes A and B. Build on A, retarget to B: L4 says B,
/// setup runs `wineboot -u` with B's wine, the launcher execs B's wine, and
/// the signed bundle is byte-for-byte what was built (`codesign --verify`
/// where codesign exists). `prune A` refuses while a game still uses A and
/// succeeds once nothing does. A failed setup and a pruned runtime both end
/// up in the run log.
#[test]
fn retarget_moves_a_game_to_another_runtime_without_touching_the_bundle() {
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    // SAFETY: env mutation under ENV_LOCK.
    unsafe { std::env::set_var("GOGO_HOME", &home) };
    let (log_a, log_b) = (tmp.path().join("wine-a.log"), tmp.path().join("wine-b.log"));
    let m = |id: &str, version: &str| macapp::store::ManifestEntry {
        id: id.into(),
        version: version.into(),
        sha256: "0".into(),
        fetched_at: "2026-10-23T00:00:00Z".into(),
        upstream_url: "http://x".into(),
        entry_rel: "bin/wine".into(),
        bytes: 1,
    };
    fake_runtime(&home.join("runtimes/wine-staging/11.18"), &log_a);
    fake_runtime(&home.join("runtimes/wine-devel/11.19"), &log_b);
    macapp::store::save_manifest(&[m("wine-staging", "11.18"), m("wine-devel", "11.19")]).unwrap();

    let gogid = "1207659013";
    let tree = depot_game(tmp.path(), gogid, "Retarget Game", false, &["kernel32.dll"]);
    // A fake gogo for the launcher's setup: fails the way a runtime that
    // isn't in the store does.
    let gogo = tmp.path().join("fake-gogo");
    write_exec(
        &gogo,
        "#!/bin/sh\necho 'warning: noise'\n\
         echo \"error: runtime \\`wine-staging/11.18\\` is not in the store\" >&2\nexit 1\n",
    );
    let opts = MacAppOptions {
        installer: None,
        game_tree: Some(tree),
        name: None,
        mode: RuntimeMode::Catalog("wine-staging".into()),
        runtime_path: None,
        out_dir: tmp.path().join("out"),
        overwrite: true,
        recipe: Default::default(),
        runtime_menu: false,
        runtime_version: None,
        lutris_base: None,
        renderer: macapp::RendererChoice::Auto,
        portable: false,
        gogo_path: Some(gogo),
    };
    let codesign = std::path::Path::new("/usr/bin/codesign").is_file();
    let policy = if codesign {
        CodesignPolicy::Adhoc
    } else {
        CodesignPolicy::Skip
    };
    let plan = macapp::plan(&opts, None, &mut |_| {}).expect("plan");
    macapp::execute(&plan, policy, &mut |_| {}).expect("execute");
    let app = tmp.path().join("out/Retarget Game.app");
    let toml = std::fs::read_to_string(app.join("Contents/Resources/gogo-launch.toml")).unwrap();
    assert!(toml.contains("runtime = \"wine-staging/11.18\""), "{toml}");
    let built = tree_digest(&app);
    let g = home.join("games").join(gogid);
    let state = || {
        macapp::launch::GameState::load(&g.join("state.toml"))
            .unwrap()
            .unwrap()
    };
    assert_eq!(state().runtime, "wine-staging/11.18");

    // A is in use: prune keeps it.
    let out = macapp::store::prune(&["wine-staging".into()]).unwrap();
    assert!(out.removed.is_empty());
    assert_eq!(out.kept[0].1[0].gogid, gogid);

    // Retarget by bundle path: L4 + setup on B (a new engine: wineboot -u).
    std::fs::write(&log_b, "").unwrap();
    let target = macapp::setup::target_gogid(&app.to_string_lossy()).unwrap();
    assert_eq!(target, gogid);
    let out = macapp::setup::retarget(&target, &[("runtime", Some("wine-devel/11.19"))]).unwrap();
    assert_eq!(out.changes.len(), 1);
    let setup = out.setup.expect("the game has state").expect("setup on B");
    assert!(setup.prefix_updated);
    assert_eq!(setup.state.runtime, "wine-devel/11.19");
    let l4 = std::fs::read_to_string(home.join("compat").join(format!("{gogid}.toml"))).unwrap();
    assert!(l4.contains("runtime = \"wine-devel/11.19\""), "{l4}");
    assert!(
        std::fs::read_to_string(&log_b)
            .unwrap()
            .contains("wineboot -u"),
        "setup updated the prefix with B"
    );
    assert!(g.join("prefix/dosdevices/z:").symlink_metadata().is_err());
    // Retargeting again changes nothing and runs no setup.
    let again = macapp::setup::retarget(gogid, &[("runtime", Some("wine-devel/11.19"))]).unwrap();
    assert!(again.changes.is_empty() && again.setup.is_none());

    // The launcher execs B's wine (fresh stamp: no setup).
    let launch = |extra: &[(&str, &std::path::Path)]| {
        let mut cmd = std::process::Command::new(app.join("Contents/MacOS/Retarget Game"));
        cmd.env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("GOGO_LAUNCHER_NO_DIALOG", "1");
        for (k, v) in extra {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    };
    std::fs::write(&log_a, "").unwrap();
    std::fs::write(&log_b, "").unwrap();
    let run = launch(&[]);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let b = std::fs::read_to_string(&log_b).unwrap();
    assert!(b.contains(r"C:\GOG\Retarget Game\Game.exe"), "{b}");
    assert_eq!(std::fs::read_to_string(&log_a).unwrap(), "", "A never ran");
    let logs = home.join("Library/Logs/gogo").join(gogid);
    let latest = std::fs::read_to_string(logs.join("latest.log")).unwrap();
    assert!(latest.contains("# runtime: wine-devel/11.19\n"), "{latest}");

    // The bundle is what was built, and still verifies.
    assert_eq!(built, tree_digest(&app), "retarget never writes the bundle");
    if codesign {
        let v = std::process::Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(&app)
            .output()
            .unwrap();
        assert!(v.status.success(), "{}", String::from_utf8_lossy(&v.stderr));
    }

    // Nothing references A any more: prune removes it. B stays.
    let out = macapp::store::prune(&["wine-staging".into(), "wine-devel".into()]).unwrap();
    assert_eq!(out.removed, [home.join("runtimes/wine-staging")]);
    assert_eq!(out.kept.len(), 1);
    assert_eq!(out.kept[0].0, "wine-devel");

    // L4 back to A, which is gone: setup fails, the run says so in its
    // header and runs on what was last applied (B).
    macapp::compat::set_user_keys(gogid, &[("runtime", Some("wine-staging/11.18"))]).unwrap();
    std::fs::write(&log_b, "").unwrap();
    let run = launch(&[]);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        std::fs::read_to_string(&log_b)
            .unwrap()
            .contains("Game.exe")
    );
    let latest = std::fs::read_to_string(logs.join("latest.log")).unwrap();
    assert!(
        latest.contains(
            "# setup: setup failed (exit 1): error: runtime `wine-staging/11.18` is not in \
             the store; running the last applied config\n"
        ),
        "{latest}"
    );

    // B pruned behind the game's back: the launcher refuses, and says why
    // in the run log.
    std::fs::remove_dir_all(home.join("runtimes/wine-devel")).unwrap();
    let run = launch(&[]);
    assert_eq!(run.status.code(), Some(1));
    let latest = std::fs::read_to_string(logs.join("latest.log")).unwrap();
    assert!(
        latest.contains("runtime wine-devel/11.19 is not in the runtime store"),
        "{latest}"
    );
    assert!(
        latest.contains("# exit: 127 (runtime missing)\n"),
        "{latest}"
    );

    unsafe { std::env::remove_var("GOGO_HOME") };
    drop(guard);
}

/// #110, design 16 decision 5 (`flags > L4`): a build-time `--renderer` /
/// `--runtime` replaces an existing L4 value for its key (the rest of the
/// file is kept, comments included) and is reported; a key L4 doesn't set,
/// or no L4 file at all, gets nothing written. A `--renderer d3dmetal` the
/// #71 gate sets aside (32-bit, no consent) leaves L4 alone, and a pick in
/// the bare `--runtime` menu (#76) counts as the flag.
#[test]
fn build_flags_replace_existing_l4_values() {
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    // SAFETY: env mutation under ENV_LOCK.
    unsafe { std::env::set_var("GOGO_HOME", &home) };
    let m = |id: &str, version: &str| macapp::store::ManifestEntry {
        id: id.into(),
        version: version.into(),
        sha256: "0".into(),
        fetched_at: "2026-10-23T00:00:00Z".into(),
        upstream_url: "http://x".into(),
        entry_rel: "bin/wine".into(),
        bytes: 1,
    };
    fake_runtime(
        &home.join("runtimes/wine-staging/11.18"),
        &tmp.path().join("a.log"),
    );
    fake_runtime(
        &home.join("runtimes/wine-devel/11.19"),
        &tmp.path().join("b.log"),
    );
    // The default track cached too, so the `auto` ladder never downloads.
    fake_runtime(
        &home.join("runtimes/winecx/24.0.7_7"),
        &tmp.path().join("cx.log"),
    );
    macapp::store::save_manifest(&[
        m("wine-staging", "11.18"),
        m("wine-devel", "11.19"),
        m("winecx", "24.0.7_7"),
    ])
    .unwrap();

    let gogid = "1207659014";
    let tree = depot_game(tmp.path(), gogid, "Flag Game", false, &["kernel32.dll"]);
    let opts = MacAppOptions {
        installer: None,
        game_tree: Some(tree),
        name: None,
        mode: RuntimeMode::Catalog("wine-devel".into()),
        runtime_path: None,
        out_dir: tmp.path().join("out"),
        overwrite: true,
        recipe: Default::default(),
        runtime_menu: false,
        runtime_version: None,
        lutris_base: None,
        renderer: macapp::RendererChoice::Force(macapp::Renderer::Wined3d),
        portable: false,
        gogo_path: Some(tmp.path().join("fake-gogo")),
    };
    let l4_path = home.join("compat").join(format!("{gogid}.toml"));
    let build = |opts: &MacAppOptions, prompt: Option<&gogo::macapp::PromptFn>| {
        let sets = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = sets.clone();
        let mut emit = move |ev: &ProgressEvent| {
            assert!(
                !matches!(ev, ProgressEvent::RuntimeDownloadStarted { .. }),
                "hermetic: {ev:?}"
            );
            if let ProgressEvent::UserOverrideSet { key, old, new, .. } = ev {
                sink.lock()
                    .unwrap()
                    .push((key.clone(), old.clone(), new.clone()));
            }
        };
        let plan = macapp::plan(opts, prompt, &mut emit).expect("plan");
        macapp::execute(&plan, CodesignPolicy::Skip, &mut emit).expect("execute");
        let state =
            macapp::launch::GameState::load(&home.join("games").join(gogid).join("state.toml"))
                .unwrap()
                .unwrap();
        let sets = sets.lock().unwrap().clone();
        (state, sets)
    };
    let s = |v: &str| Some(v.to_string());

    // No L4: the flags are baked, no file appears.
    let (state, sets) = build(&opts, None);
    assert!(sets.is_empty(), "{sets:?}");
    assert!(!l4_path.exists(), "a build never creates an override file");
    assert_eq!(
        (state.runtime.as_str(), state.renderer.as_str()),
        ("wine-devel/11.19", "wined3d")
    );

    // L4 sets both keys (a retarget, "Try next renderer"): the flags replace
    // them, the user's other keys and comments stay, and setup applies them.
    std::fs::create_dir_all(l4_path.parent().unwrap()).unwrap();
    std::fs::write(
        &l4_path,
        "# my notes\nruntime = \"wine-staging/11.18\"\nrenderer = \"dxmt/v0.80\"\n\
         env = { FOO = \"1\" }\n",
    )
    .unwrap();
    let (state, sets) = build(&opts, None);
    assert_eq!(
        sets,
        vec![
            ("renderer".into(), s("dxmt/v0.80"), s("wined3d")),
            (
                "runtime".into(),
                s("wine-staging/11.18"),
                s("wine-devel/11.19")
            ),
        ]
    );
    let l4 = std::fs::read_to_string(&l4_path).unwrap();
    assert!(l4.contains("# my notes"), "{l4}");
    assert!(l4.contains("runtime = \"wine-devel/11.19\""), "{l4}");
    assert!(l4.contains("renderer = \"wined3d\""), "{l4}");
    assert!(l4.contains("env = { FOO = \"1\" }"), "{l4}");
    assert_eq!(
        (state.runtime.as_str(), state.renderer.as_str()),
        ("wine-devel/11.19", "wined3d")
    );

    // A key L4 doesn't set gets nothing, even with its flag given.
    std::fs::write(&l4_path, "runtime = \"wine-staging/11.18\"\n").unwrap();
    let (_, sets) = build(&opts, None);
    assert_eq!(
        sets,
        vec![(
            "runtime".into(),
            s("wine-staging/11.18"),
            s("wine-devel/11.19")
        )]
    );
    assert!(
        !std::fs::read_to_string(&l4_path)
            .unwrap()
            .contains("renderer")
    );

    // `--renderer d3dmetal` on a 32-bit game is set aside (#71): L4 keeps
    // its renderer.
    std::fs::write(&l4_path, "renderer = \"wined3d\"\n").unwrap();
    let d3dmetal = MacAppOptions {
        renderer: macapp::RendererChoice::Force(macapp::Renderer::D3dMetal),
        ..opts.clone()
    };
    let (state, sets) = build(&d3dmetal, None);
    assert!(sets.is_empty(), "{sets:?}");
    assert_eq!(
        std::fs::read_to_string(&l4_path).unwrap(),
        "renderer = \"wined3d\"\n"
    );
    assert_eq!(state.renderer, "wined3d");

    // Bare `--runtime` (#76): the menu pick is the flag.
    std::fs::write(&l4_path, "runtime = \"wine-staging/11.18\"\n").unwrap();
    let pick: std::sync::Arc<gogo::macapp::PromptFn> =
        std::sync::Arc::new(|spec: &gogo::macapp::PromptSpec| match spec.kind {
            gogo::macapp::PromptKind::Select => spec
                .items
                .iter()
                .position(|i| i.starts_with("wine-devel 11.19"))
                .map(gogo::macapp::PromptAnswer::Index),
            _ => None,
        });
    let menu = MacAppOptions {
        mode: RuntimeMode::Auto,
        runtime_menu: true,
        renderer: macapp::RendererChoice::Auto,
        ..opts.clone()
    };
    let (state, sets) = build(&menu, Some(pick.as_ref()));
    assert_eq!(
        sets,
        vec![(
            "runtime".into(),
            s("wine-staging/11.18"),
            s("wine-devel/11.19")
        )]
    );
    assert_eq!(state.runtime, "wine-devel/11.19");
    // Without a flag or a menu, L4 stays and wins at setup.
    let ladder = MacAppOptions {
        runtime_menu: false,
        ..menu
    };
    std::fs::write(&l4_path, "runtime = \"wine-staging/11.18\"\n").unwrap();
    let (state, sets) = build(&ladder, None);
    assert!(sets.is_empty(), "{sets:?}");
    assert_eq!(state.runtime, "wine-staging/11.18");

    unsafe { std::env::remove_var("GOGO_HOME") };
    drop(guard);
}
