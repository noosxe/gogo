use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use super::*;
use crate::cloud::remote_config::RawLocation;

struct Tree {
    _dir: tempfile::TempDir,
    layout: Layout,
    /// A dir outside the prefix and the game's files.
    outside: PathBuf,
}

fn tree(lane: Lane) -> Tree {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let prefix = base.join("games/1/prefix");
    let game = base.join("games/1/game");
    let outside = base.join("elsewhere");
    for d in [&prefix, &game, &outside] {
        fs::create_dir_all(d).unwrap();
    }
    fs::create_dir_all(prefix.join(format!("drive_c/users/{}", lane.windows_user()))).unwrap();
    Tree {
        layout: Layout {
            lane,
            prefix,
            game_files: game,
        },
        outside,
        _dir: dir,
    }
}

fn user(t: &Tree) -> PathBuf {
    t.layout
        .prefix
        .join("drive_c/users")
        .join(t.layout.lane.windows_user())
}

fn storage(locs: &[(&str, &str)]) -> CloudStorage {
    CloudStorage {
        enabled: true,
        locations: locs
            .iter()
            .map(|(n, l)| RawLocation {
                name: n.to_string(),
                location: l.to_string(),
            })
            .collect(),
    }
}

fn one(t: &Tree, location: &str) -> Result<Location, SkipReason> {
    let mut r = resolve_storage(Some(&storage(&[("saves", location)])), "42", &t.layout);
    match r.locations.pop() {
        Some(l) => Ok(l),
        None => Err(r.skipped.pop().unwrap().reason),
    }
}

// — parsing —

#[test]
fn splits_on_both_separators_and_collapses_runs() {
    let (v, rest) = parse_location(r"<?DOCUMENTS?>\\The Witcher 3").unwrap();
    assert_eq!(
        (v, rest),
        (Variable::Documents, vec!["The Witcher 3".into()])
    );
    let (v, rest) = parse_location("<?SAVED_GAMES?>/CD Projekt Red/Cyberpunk 2077").unwrap();
    assert_eq!(v, Variable::SavedGames);
    assert_eq!(rest, ["CD Projekt Red", "Cyberpunk 2077"]);
    let (_, rest) = parse_location(r"<?APPLICATION_DATA_LOCAL?>/a\b//c\\\d").unwrap();
    assert_eq!(rest, ["a", "b", "c", "d"]);
    let (v, rest) = parse_location("<?INSTALL?>").unwrap();
    assert_eq!((v, rest.len()), (Variable::Install, 0));
}

#[test]
fn every_variable_is_known() {
    for (name, var) in [
        ("INSTALL", Variable::Install),
        ("DOCUMENTS", Variable::Documents),
        ("SAVED_GAMES", Variable::SavedGames),
        ("APPLICATION_DATA_ROAMING", Variable::ApplicationDataRoaming),
        ("APPLICATION_DATA_LOCAL", Variable::ApplicationDataLocal),
        (
            "APPLICATION_DATA_LOCAL_LOW",
            Variable::ApplicationDataLocalLow,
        ),
    ] {
        assert_eq!(parse_location(&format!("<?{name}?>/x")).unwrap().0, var);
        assert_eq!(var.name(), name);
    }
}

#[test]
fn refuses_what_is_not_a_known_windows_variable() {
    assert_eq!(
        parse_location("<?APPLICATION_SUPPORT?>/x"),
        Err(SkipReason::NotForWindowsBuild)
    );
    assert_eq!(
        parse_location("<?NOPE?>/x"),
        Err(SkipReason::UnknownVariable("NOPE".into()))
    );
    for bad in [
        r"C:\Users\x",
        "/etc/passwd",
        r"\\server\share",
        "relative/dir",
        "",
        "<?DOCUMENTS",
        "x<?DOCUMENTS?>",
    ] {
        assert!(
            matches!(parse_location(bad), Err(SkipReason::NotAVariable(_))),
            "{bad:?}"
        );
    }
    // A variable in a later part is no variable.
    assert!(matches!(
        parse_location("<?DOCUMENTS?>/<?INSTALL?>"),
        Err(SkipReason::NotAVariable(_))
    ));
    for bad in [
        "<?DOCUMENTS?>/..",
        r"<?DOCUMENTS?>\..\x",
        "<?DOCUMENTS?>/a/./b",
        "<?DOCUMENTS?>/c:",
        "<?DOCUMENTS?>/a\0b",
    ] {
        assert!(
            matches!(parse_location(bad), Err(SkipReason::BadPath(_))),
            "{bad:?}"
        );
    }
}

// — mapping, both lanes —

#[test]
fn maps_every_variable_in_both_lanes() {
    for lane in [Lane::Linux, Lane::Mac] {
        let t = tree(lane);
        let u = format!(
            "{}/drive_c/users/{}",
            t.layout.prefix.display(),
            lane.windows_user()
        );
        for (loc, want) in [
            ("<?DOCUMENTS?>/G", format!("{u}/Documents/G")),
            ("<?SAVED_GAMES?>/G", format!("{u}/Saved Games/G")),
            (
                "<?APPLICATION_DATA_ROAMING?>/G",
                format!("{u}/AppData/Roaming/G"),
            ),
            (
                "<?APPLICATION_DATA_LOCAL?>/G",
                format!("{u}/AppData/Local/G"),
            ),
            (
                "<?APPLICATION_DATA_LOCAL_LOW?>/G",
                format!("{u}/AppData/LocalLow/G"),
            ),
            (
                "<?INSTALL?>/saves",
                format!("{}/saves", t.layout.game_files.display()),
            ),
        ] {
            let l = one(&t, loc).unwrap();
            assert_eq!(l.path, Path::new(&want), "{lane:?} {loc}");
            assert!(!l.exists && !l.unverified);
        }
    }
    assert_eq!(Lane::Linux.windows_user(), "steamuser");
    assert_eq!(Lane::Mac.windows_user(), "crossover");
}

#[test]
fn the_other_lanes_user_is_not_used() {
    let t = tree(Lane::Linux);
    fs::create_dir_all(t.layout.prefix.join("drive_c/users/crossover/Documents")).unwrap();
    let l = one(&t, "<?DOCUMENTS?>/G").unwrap();
    assert!(l.path.to_string_lossy().contains("/steamuser/"));
}

#[test]
fn installer_lane_install_is_the_game_dir_in_the_prefix() {
    let mut t = tree(Lane::Linux);
    t.layout.game_files = t.layout.prefix.join("drive_c/GOG/Some Game");
    fs::create_dir_all(&t.layout.game_files).unwrap();
    let l = one(&t, "<?INSTALL?>/Saves").unwrap();
    assert_eq!(l.path, t.layout.game_files.join("Saves"));
    assert!(l.is_install());
}

#[test]
fn existing_dirs_set_exists() {
    let t = tree(Lane::Linux);
    fs::create_dir_all(user(&t).join("Documents/My Games/G")).unwrap();
    assert!(one(&t, "<?DOCUMENTS?>/My Games/G").unwrap().exists);
    assert!(!one(&t, "<?DOCUMENTS?>/My Games/H").unwrap().exists);
}

// — case-insensitive lookup —

#[test]
fn looks_parts_up_without_regard_to_case() {
    let t = tree(Lane::Linux);
    fs::create_dir_all(user(&t).join("Documents/my GAMES/xcom")).unwrap();
    let l = one(&t, "<?DOCUMENTS?>/My Games/XCom").unwrap();
    assert_eq!(l.path, user(&t).join("Documents/my GAMES/xcom"));
    assert!(l.exists);
    // The variable's own dir too (a prefix that spells it otherwise).
    fs::rename(user(&t).join("Documents"), user(&t).join("DOCUMENTS")).unwrap();
    let l = one(&t, "<?DOCUMENTS?>/my games/xcom").unwrap();
    assert_eq!(l.path, user(&t).join("DOCUMENTS/my GAMES/xcom"));
}

#[test]
fn a_missing_part_keeps_its_exact_case_and_so_do_those_after_it() {
    let t = tree(Lane::Linux);
    fs::create_dir_all(user(&t).join("Documents")).unwrap();
    let l = one(&t, "<?DOCUMENTS?>/New Dir/Sub/File").unwrap();
    assert_eq!(l.path, user(&t).join("Documents/New Dir/Sub/File"));
    assert!(!l.exists);
}

#[test]
fn exact_case_wins_and_ambiguity_skips_only_that_location() {
    let t = tree(Lane::Linux);
    let d = user(&t).join("Documents");
    fs::create_dir_all(d.join("Game")).unwrap();
    if d.join("game").exists() {
        // A case-insensitive filesystem (macOS) can't hold both dirs.
        return;
    }
    fs::create_dir_all(d.join("game")).unwrap();
    assert_eq!(one(&t, "<?DOCUMENTS?>/Game").unwrap().path, d.join("Game"));
    assert_eq!(one(&t, "<?DOCUMENTS?>/game").unwrap().path, d.join("game"));
    assert!(matches!(
        one(&t, "<?DOCUMENTS?>/GAME"),
        Err(SkipReason::Ambiguous(_))
    ));
    let r = resolve_storage(
        Some(&storage(&[
            ("a", "<?DOCUMENTS?>/GAME"),
            ("b", "<?DOCUMENTS?>/Game"),
        ])),
        "42",
        &t.layout,
    );
    assert_eq!(r.locations.len(), 1);
    assert_eq!(r.locations[0].name, "b");
    assert_eq!(r.skipped.len(), 1);
    assert_eq!(r.skipped[0].name, "a");
}

// — the escape guard —

#[test]
fn a_dotdot_part_skips_the_location() {
    let t = tree(Lane::Linux);
    for loc in [
        "<?DOCUMENTS?>/../../../../../..",
        r"<?DOCUMENTS?>\..\..\x",
        "<?INSTALL?>/../prefix",
    ] {
        assert!(matches!(one(&t, loc), Err(SkipReason::BadPath(_))), "{loc}");
    }
}

#[test]
fn absolute_and_drive_paths_skip_the_location() {
    let t = tree(Lane::Linux);
    for loc in ["/etc", r"C:\Windows", "//host/share", "~/x"] {
        assert!(
            matches!(one(&t, loc), Err(SkipReason::NotAVariable(_))),
            "{loc}"
        );
    }
}

#[test]
fn a_link_out_of_the_prefix_skips_the_location() {
    let t = tree(Lane::Linux);
    // Proton's default: Documents is a link to the host's home.
    symlink(&t.outside, user(&t).join("Documents")).unwrap();
    assert_eq!(one(&t, "<?DOCUMENTS?>/G"), Err(SkipReason::EscapesPrefix));
    assert_eq!(one(&t, "<?DOCUMENTS?>"), Err(SkipReason::EscapesPrefix));
}

#[test]
fn a_link_deeper_in_the_path_is_caught_too() {
    let t = tree(Lane::Mac);
    let docs = user(&t).join("Documents");
    fs::create_dir_all(&docs).unwrap();
    symlink(&t.outside, docs.join("My Games")).unwrap();
    assert_eq!(
        one(&t, "<?DOCUMENTS?>/My Games/G/sub"),
        Err(SkipReason::EscapesPrefix)
    );
    // Through a case-insensitive hit as well.
    assert_eq!(
        one(&t, "<?DOCUMENTS?>/my games/G"),
        Err(SkipReason::EscapesPrefix)
    );
}

#[test]
fn a_dangling_link_counts_as_leaving() {
    let t = tree(Lane::Linux);
    let docs = user(&t).join("Documents");
    fs::create_dir_all(&docs).unwrap();
    symlink(t.outside.join("nope"), docs.join("G")).unwrap();
    assert_eq!(one(&t, "<?DOCUMENTS?>/G"), Err(SkipReason::EscapesPrefix));
}

#[test]
fn a_link_that_stays_inside_is_followed() {
    let mut t = tree(Lane::Linux);
    // The depot lane's drive_c/GOG/<name> -> game link.
    t.layout.game_files = t.layout.prefix.parent().unwrap().join("game");
    fs::create_dir_all(t.layout.prefix.join("drive_c/GOG")).unwrap();
    symlink(
        &t.layout.game_files,
        t.layout.prefix.join("drive_c/GOG/Some Game"),
    )
    .unwrap();
    // A save dir in the prefix that is a link to the game's files.
    let docs = user(&t).join("Documents");
    fs::create_dir_all(&docs).unwrap();
    symlink(&t.layout.game_files, docs.join("Saves")).unwrap();
    let l = one(&t, "<?DOCUMENTS?>/Saves/slot1").unwrap();
    assert_eq!(l.path, docs.join("Saves/slot1"));
    // And a link to another dir of the prefix.
    fs::create_dir_all(user(&t).join("AppData/Local/Real")).unwrap();
    symlink(user(&t).join("AppData/Local/Real"), docs.join("Alias")).unwrap();
    assert!(one(&t, "<?DOCUMENTS?>/Alias").is_ok());
}

#[test]
fn a_prefix_that_is_itself_a_link_works() {
    // umu's `pfx` links to the prefix; the game's prefix dir may be linked.
    let t = tree(Lane::Linux);
    let real = t.layout.prefix.clone();
    let pfx = real.parent().unwrap().join("pfx");
    symlink(&real, &pfx).unwrap();
    let layout = Layout {
        prefix: pfx,
        ..t.layout.clone()
    };
    let r = resolve_storage(Some(&storage(&[("s", "<?DOCUMENTS?>/G")])), "42", &layout);
    assert_eq!(r.locations.len(), 1, "{:?}", r.skipped);
}

#[test]
fn file_refuses_everything_that_leaves_the_location_roots() {
    let t = tree(Lane::Linux);
    let docs = user(&t).join("Documents/G");
    fs::create_dir_all(&docs).unwrap();
    fs::write(t.outside.join("secret"), "x").unwrap();
    let l = one(&t, "<?DOCUMENTS?>/G").unwrap();
    for rel in [
        "../secret",
        r"..\secret",
        "a/../../../../../elsewhere/secret",
        "/etc/passwd",
        r"\etc\passwd",
        "",
        "/",
        "a/b/../..",
        "x:y",
    ] {
        assert!(l.file(rel).is_err(), "{rel:?}");
    }
    // A link inside the location that points out.
    symlink(&t.outside, docs.join("out")).unwrap();
    assert_eq!(l.file("out/secret"), Err(SkipReason::EscapesPrefix));
    assert_eq!(l.file("out"), Err(SkipReason::EscapesPrefix));
    // A file that is a link out.
    symlink(t.outside.join("secret"), docs.join("s.sav")).unwrap();
    assert_eq!(l.file("s.sav"), Err(SkipReason::EscapesPrefix));
    // A dangling one.
    symlink(t.outside.join("gone"), docs.join("d.sav")).unwrap();
    assert_eq!(l.file("d.sav"), Err(SkipReason::EscapesPrefix));
    // A link to a path whose tail doesn't exist yet.
    assert_eq!(l.file("out/new/dir/x"), Err(SkipReason::EscapesPrefix));
}

#[test]
fn file_resolves_inside_with_case_lookup_and_either_separator() {
    let t = tree(Lane::Linux);
    let loc = user(&t).join("Documents/G");
    fs::create_dir_all(loc.join("Slot1")).unwrap();
    fs::write(loc.join("Slot1/Save.dat"), "x").unwrap();
    let l = one(&t, "<?DOCUMENTS?>/G").unwrap();
    assert_eq!(
        l.file("slot1/save.DAT").unwrap(),
        loc.join("Slot1/Save.dat")
    );
    assert_eq!(
        l.file(r"Slot1\Save.dat").unwrap(),
        loc.join("Slot1/Save.dat")
    );
    assert_eq!(l.file("New/f.sav").unwrap(), loc.join("New/f.sav"));
}

#[test]
fn roots_contains_checks_without_creating_anything() {
    let t = tree(Lane::Linux);
    let roots = Roots::of(&t.layout);
    assert!(roots.contains(&t.layout.game_files.join("a/b/c")));
    assert!(roots.contains(&t.layout.prefix));
    assert!(!roots.contains(&t.outside));
    assert!(!roots.contains(&t.layout.prefix.join("../elsewhere")));
    assert!(!roots.contains(Path::new("/")));
    assert!(!roots.contains(Path::new("relative")));
    assert!(!t.layout.game_files.join("a").exists());
}

// — resolve —

#[test]
fn disabled_or_absent_storage_resolves_to_nothing() {
    let t = tree(Lane::Linux);
    let mut s = storage(&[("saves", "<?DOCUMENTS?>/G")]);
    s.enabled = false;
    let r = resolve_storage(Some(&s), "42", &t.layout);
    assert!(!r.enabled && r.locations.is_empty() && r.skipped.is_empty());
    let r = resolve_storage(None, "42", &t.layout);
    assert!(!r.enabled);
    let cfg =
        RemoteConfig::parse(br#"{"content":{"MacOS":{"cloudStorage":{"enabled":true}}}}"#).unwrap();
    assert!(!resolve(&cfg, "42", &t.layout).enabled);
}

#[test]
fn several_locations_resolve_and_bad_ones_are_listed() {
    let t = tree(Lane::Linux);
    let s = storage(&[
        ("saves", "<?DOCUMENTS?>/My Games/X"),
        ("saves_pool", r"<?DOCUMENTS?>\My Games\X\Pool"),
        ("bad var", "<?WAT?>/x"),
        ("mac", "<?APPLICATION_SUPPORT?>/x"),
        ("drive", r"C:\x"),
        ("saves", "<?DOCUMENTS?>/Again"),
        ("a/b", "<?DOCUMENTS?>/x"),
        ("", "<?DOCUMENTS?>/x"),
        ("..", "<?DOCUMENTS?>/x"),
    ]);
    let r = resolve_storage(Some(&s), "42", &t.layout);
    assert!(r.enabled);
    let names: Vec<&str> = r.locations.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["saves", "saves_pool"]);
    let reasons: Vec<&SkipReason> = r.skipped.iter().map(|s| &s.reason).collect();
    assert_eq!(reasons.len(), 7);
    assert!(matches!(reasons[0], SkipReason::UnknownVariable(_)));
    assert!(matches!(reasons[1], SkipReason::NotForWindowsBuild));
    assert!(matches!(reasons[2], SkipReason::NotAVariable(_)));
    assert!(matches!(reasons[3], SkipReason::DuplicateName));
    assert!(
        reasons[4..]
            .iter()
            .all(|r| matches!(r, SkipReason::BadName(_)))
    );
    assert!(r.skipped.iter().all(|s| !s.message.is_empty()));
}

#[test]
fn an_empty_list_falls_back_to_the_unverified_default() {
    for lane in [Lane::Linux, Lane::Mac] {
        let t = tree(lane);
        let r = resolve_storage(Some(&storage(&[])), "98765", &t.layout);
        assert_eq!(r.locations.len(), 1);
        let l = &r.locations[0];
        assert_eq!(l.name, "__default");
        assert!(l.unverified);
        assert_eq!(
            l.path,
            user(&t).join("AppData/Local/GOG.com/Galaxy/Applications/98765/Storage/Shared/Files")
        );
    }
    // A client id that isn't one can't build the path.
    let t = tree(Lane::Linux);
    let r = resolve_storage(Some(&storage(&[])), "../x", &t.layout);
    assert!(r.locations.is_empty() && r.skipped.len() == 1);
    // Declared locations suppress it.
    let r = resolve_storage(Some(&storage(&[("s", "<?DOCUMENTS?>/G")])), "1", &t.layout);
    assert!(r.locations.iter().all(|l| !l.unverified));
}

#[test]
fn resolved_serializes_without_the_roots() {
    let t = tree(Lane::Linux);
    let r = resolve_storage(
        Some(&storage(&[("saves", "<?DOCUMENTS?>/G")])),
        "1",
        &t.layout,
    );
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["locations"][0]["variable"], "documents");
    assert!(v["locations"][0].get("roots").is_none());
}

// — excludes —

#[test]
fn excludes_logs_dumps_caches_and_partial_files() {
    for rel in [
        "a.log",
        "sub/crash.DMP",
        "Thumbs.db",
        "x/thumbs.db",
        "slot.sav.part",
        "slot.sav.TMP",
        "slot.~incomplete",
        r"sub\slot.part",
        "shader.dxvk-cache",
        "CrashDumps/x.bin",
    ] {
        assert!(excluded(rel), "{rel}");
    }
    for rel in [
        "slot1/save.dat",
        "settings.ini",
        "a.cfg",
        "partial.txt",
        "tmp/a.sav",
    ] {
        assert!(!excluded(rel), "{rel}");
    }
}

// — the game and its client id —

fn gogo_home() -> (std::sync::MutexGuard<'static, ()>, tempfile::TempDir) {
    let g = crate::test_util::env_lock();
    let home = tempfile::tempdir().unwrap();
    // SAFETY: serialized by env_lock.
    unsafe { std::env::set_var("GOGO_HOME", home.path()) };
    (g, home)
}

#[test]
fn finds_a_built_depot_game_and_its_client_id_from_the_info() {
    let (_g, home) = gogo_home();
    let game = home.path().join("games/123/game");
    fs::create_dir_all(&game).unwrap();
    fs::create_dir_all(home.path().join("games/123/prefix")).unwrap();
    fs::write(
        game.join("GOGGAME-123.info"),
        "\u{feff}{\"clientId\":\"4242\",\"gameId\":\"123\"}",
    )
    .unwrap();
    let g = Game::find("123", Lane::Linux).unwrap();
    assert_eq!(g.layout.game_files, game);
    assert_eq!(g.layout.prefix, home.path().join("games/123/prefix"));
    assert_eq!(g.client_id().unwrap(), "4242");
    assert!(matches!(
        Game::find("999", Lane::Linux),
        Err(CloudError::NotBuilt(_))
    ));
    assert!(Game::find("12x", Lane::Linux).is_err());
    unsafe { std::env::remove_var("GOGO_HOME") };
}

#[test]
fn the_installer_lane_info_is_found_in_the_prefix() {
    let (_g, home) = gogo_home();
    let root = home.path().join("games/5");
    let inst = root.join("prefix/drive_c/GOG Games/Some Game");
    fs::create_dir_all(&inst).unwrap();
    fs::write(inst.join("goggame-5.info"), r#"{"clientId":"777"}"#).unwrap();
    let g = Game::find("5", Lane::Linux).unwrap();
    assert_eq!(g.client_id().unwrap(), "777");
    unsafe { std::env::remove_var("GOGO_HOME") };
}

#[test]
fn the_client_id_falls_back_to_the_kept_manifest_and_never_reads_the_secret() {
    let (_g, home) = gogo_home();
    let root = home.path().join("games/7");
    fs::create_dir_all(root.join("game")).unwrap();
    fs::create_dir_all(root.join("prefix")).unwrap();
    let g = Game::find("7", Lane::Mac).unwrap();
    assert!(matches!(g.client_id(), Err(CloudError::NoClientId(_))));
    // The wrapped form gog-warp's enum serializes to, and the flat one.
    for body in [
        r#"{"schema":1,"manifest":{"V2":{"clientId":"31337","clientSecret":"made-up"}}}"#,
        r#"{"schema":1,"manifest":{"clientId":"31337","clientSecret":"made-up"}}"#,
    ] {
        fs::write(root.join("build-manifest.json"), body).unwrap();
        assert_eq!(g.client_id().unwrap(), "31337");
    }
    // An info without a usable id doesn't block the fallback.
    fs::write(root.join("game/goggame-7.info"), r#"{"gameId":"7"}"#).unwrap();
    assert_eq!(g.client_id().unwrap(), "31337");
    // A client id that isn't alphanumeric is refused.
    fs::write(
        root.join("build-manifest.json"),
        r#"{"manifest":{"clientId":"../x"}}"#,
    )
    .unwrap();
    fs::remove_file(root.join("game/goggame-7.info")).unwrap();
    assert!(g.client_id().is_err());
    unsafe { std::env::remove_var("GOGO_HOME") };
}
