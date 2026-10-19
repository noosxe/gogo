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
        runtime_version: None,
        lutris_base: None,
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
    assert_eq!(plan.installer.bin_parts.len(), 1);

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
        installer,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir),
        out_dir: tmp.path().to_path_buf(),
        overwrite: true,
        recipe: Default::default(),
        runtime_version: None,
        lutris_base: None,
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
        installer,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir),
        out_dir: tmp.path().to_path_buf(),
        overwrite: true,
        recipe: gogo::macapp::RecipeChoice::File(recipe),
        runtime_version: None,
        lutris_base: None,
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

    // A cached wine-staging store entry (manifest + probeable tree).
    let home = tmp.path().join("home");
    let tree = home.join("runtimes/wine-staging/11.0/bin");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("wine"), "#!/bin/sh\necho wine-11.0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(tree.join("wine"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    std::fs::create_dir_all(home.join("runtimes")).unwrap();
    std::fs::write(
        home.join("runtimes/manifest.json"),
        r#"[{"id":"wine-staging","version":"11.0","sha256":"0","fetched_at":"2026-01-01T00:00:00Z","upstream_url":"x","entry_rel":"bin/wine","bytes":16}]"#,
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
        installer,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Auto,
        runtime_path: None,
        out_dir: tmp.path().to_path_buf(),
        overwrite: true,
        recipe: gogo::macapp::RecipeChoice::File(recipe),
        runtime_version: None,
        lutris_base: None,
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
    // The ladder: recommendation (staging) satisfied from the store cache.
    assert_eq!(plan.runtime.version, "wine-11.0");
    assert_eq!(
        plan.runtime.mode,
        RuntimeMode::Catalog("wine-staging".into())
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
        installer,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir),
        out_dir: tmp.path().to_path_buf(),
        overwrite: true,
        recipe: gogo::macapp::RecipeChoice::Auto,
        runtime_version: None,
        lutris_base: Some(server.uri()),
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
                _ => None,
            }
        });

    let opts = MacAppOptions {
        installer,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Auto,
        runtime_path: None,
        out_dir: tmp.path().to_path_buf(),
        overwrite: true,
        recipe: gogo::macapp::RecipeChoice::None,
        runtime_version: None,
        lutris_base: None,
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
    assert!(
        matches!(specs[0].kind, gogo::macapp::PromptKind::Select),
        "menu first: {:?}",
        specs[0].kind
    );
    assert!(specs[0].allow_other, "custom-path entry offered");
    assert!(!specs[0].allow_none);
    assert!(
        specs[0]
            .items
            .iter()
            .any(|i| i.contains("wine-devel 1.2 (✓ cached)")),
        "cached flavors lead the menu: {:?}",
        specs[0].items
    );
    assert!(
        specs[0]
            .items
            .iter()
            .any(|i| i.starts_with("download wine-staging")),
        "downloadable flavors offered: {:?}",
        specs[0].items
    );
    assert!(
        specs[0]
            .items
            .iter()
            .any(|i| i.starts_with("download wine-staging") && i.contains("recommended")),
        "staging is marked recommended"
    );
    assert!(
        matches!(specs[1].kind, gogo::macapp::PromptKind::Input),
        "re-prompt"
    );
    assert!(
        specs[1].title.contains("invalid runtime path"),
        "{}",
        specs[1].title
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
        std::sync::Arc::new(move |spec: &gogo::macapp::PromptSpec| {
            assert!(matches!(spec.kind, gogo::macapp::PromptKind::Fuzzy));
            assert!(spec.allow_other && spec.allow_none);
            Some(gogo::macapp::PromptAnswer::Other(path_answer.clone()))
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
        installer,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir),
        out_dir: tmp.path().to_path_buf(),
        overwrite: true,
        recipe: gogo::macapp::RecipeChoice::Auto,
        runtime_version: None,
        lutris_base: Some(server.uri()),
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
    assert_eq!(rec.id, "wine-staging");
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
        installer: installer.clone(),
        name: Some("Test Game".into()),
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir.clone()),
        out_dir: tmp.path().join("out-auto"),
        overwrite: true,
        recipe: gogo::macapp::RecipeChoice::Auto,
        runtime_version: None,
        lutris_base: Some(server.uri()),
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
        installer,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir),
        out_dir: tmp.path().join("out-pinned"),
        overwrite: true,
        recipe: gogo::macapp::RecipeChoice::File(script.clone()),
        runtime_version: None,
        lutris_base: Some(server.uri()),
    };
    let mut sink = |_ev: &ProgressEvent| {};
    let plan = macapp::plan(&opts, None, &mut sink).unwrap();
    assert!(macapp::execute(&plan, CodesignPolicy::Skip, &mut sink).is_err());
    drop(guard);
}

#[test]
fn runtime_menu_is_mandatory_with_easy_default() {
    // §5 (amended 2026-10-19): the ladder hit (cached wine-staging) no longer
    // bypasses the menu — interactively the selection is mandatory, the easy
    // pick is preselected (spec.default + "· easy"), and choosing it resolves
    // from the store.
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
        r#"[{"id":"wine-staging","version":"9.0","sha256":"0","fetched_at":"2026-01-01T00:00:00Z","upstream_url":"x","entry_rel":"bin/wine","bytes":16}]"#,
    )
    .unwrap();

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

    let opts = MacAppOptions {
        installer,
        name: Some("Test Game".into()),
        mode: RuntimeMode::Auto,
        runtime_path: None,
        out_dir: tmp.path().to_path_buf(),
        overwrite: true,
        recipe: gogo::macapp::RecipeChoice::None,
        runtime_version: None,
        lutris_base: None,
    };

    let old_path = std::env::var("PATH").unwrap_or_default();
    // SAFETY: env mutation under ENV_LOCK (established pattern above).
    unsafe {
        std::env::set_var("GOGO_HOME", &home);
        std::env::set_var("PATH", "/usr/bin:/bin"); // hide brew
    }
    let plan = macapp::plan(&opts, Some(prompter.as_ref()), &mut |_| {}).expect("plan");
    unsafe {
        std::env::set_var("PATH", &old_path);
        std::env::remove_var("GOGO_HOME");
    }
    drop(guard);

    let specs = seen.lock().unwrap();
    assert!(
        matches!(specs[0].kind, gogo::macapp::PromptKind::Select),
        "menu must be the first prompt even with a ladder hit: {:?}",
        specs[0].kind
    );
    assert_eq!(specs[0].default, 0, "easy pick preselected");
    assert!(
        specs[0].items[0].contains("wine-staging") && specs[0].items[0].contains("· easy"),
        "easy marker on the ladder pick: {:?}",
        specs[0].items[0]
    );
    // from_store keeps the literal GOGO_HOME path (no canonicalization).
    let store_root = home.join("runtimes/wine-staging/9.0");
    assert!(
        plan.runtime.wine.starts_with(&store_root),
        "default choice resolves to the cached staging tree: {}",
        plan.runtime.wine.display()
    );
}

#[test]
fn recipe_picker_is_mandatory_for_single_candidate() {
    // §1 (amended 2026-10-19): even a single candidate is the user's decision
    // interactively — the picker runs with "none" offered; picking it yields
    // a recipe-less plan, picking the candidate selects it.
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
        installer: installer.clone(),
        name: Some("Test Game".into()),
        mode: RuntimeMode::Path,
        runtime_path: Some(runtime_dir.clone()),
        out_dir: out.to_path_buf(),
        overwrite: true,
        recipe: gogo::macapp::RecipeChoice::Auto,
        runtime_version: None,
        lutris_base: Some(server.uri()),
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
    drop(guard);
}
