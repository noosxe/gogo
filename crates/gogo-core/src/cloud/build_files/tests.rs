use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::cloud::locations::{Lane, Layout, resolve_storage};
use crate::cloud::remote_config::{CloudStorage, RawLocation};
use crate::gog::depot::RecordSummary;

/// A made-up game dir under `base`: an installer-lane game in
/// `drive_c/GOG/G`, or a depot-lane one in `game/`.
fn game(base: &Path, installer: bool) -> Game {
    let root = base.join("games/5");
    let prefix = root.join("prefix");
    let game_files = if installer {
        prefix.join("drive_c/GOG/G")
    } else {
        root.join("game")
    };
    fs::create_dir_all(prefix.join("drive_c/users/steamuser/Documents")).unwrap();
    fs::create_dir_all(&game_files).unwrap();
    Game {
        gogid: "5".into(),
        layout: Layout {
            lane: Lane::Linux,
            prefix,
            game_files,
        },
        root,
    }
}

fn content(g: &Game, lane: &str, dlcs: &[u64]) {
    let mut s = format!("schema = 1\nlane = \"{lane}\"\nbuild_id = \"b1\"\n");
    for d in dlcs {
        s.push_str(&format!("[[dlc]]\nid = {d}\ntitle = \"D{d}\"\n"));
    }
    fs::write(g.root.join("content.toml"), s).unwrap();
}

fn log(g: &Game, name: &str, files: &[&str]) {
    let dir = g.root.join("install");
    fs::create_dir_all(&dir).unwrap();
    let body: String = files
        .iter()
        .map(|f| format!("2026-11-13 10:00:00.000   Dest filename: C:\\GOG\\G\\{f}\r\n"))
        .collect();
    fs::write(dir.join(name), body).unwrap();
}

fn known(l: Lookup) -> BuildFiles {
    match l {
        Lookup::Known(b) => b,
        other => panic!("expected a list, got {other:?}"),
    }
}

fn unknown(l: Lookup) -> String {
    match l {
        Lookup::Unknown(why) => why,
        other => panic!("expected no list, got {other:?}"),
    }
}

fn base() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let b = tmp.path().canonicalize().unwrap();
    (tmp, b)
}

#[test]
fn the_installer_lane_lists_the_main_and_every_dlc_log() {
    let (_t, b) = base();
    let g = game(&b, true);
    content(&g, "installer", &[9]);
    log(&g, "inno.log", &["Game.exe", "Data\\levels.pak"]);
    assert!(
        unknown(lookup(&g)).contains("no installer log for DLC 9"),
        "a DLC without its log laid files no list names"
    );
    log(&g, "dlc-9-inno.log", &["DLC\\extra.pak"]);
    let list = known(lookup(&g));
    assert_eq!(list.source, "the installer's log");
    assert!(list.lists("game.exe") && list.lists("data/LEVELS.pak"));
    assert!(list.lists("DLC\\extra.pak"), "the DLC's files too");
    assert!(!list.lists("SAVE/slot1.sav"));

    fs::remove_file(g.root.join("install/inno.log")).unwrap();
    assert!(unknown(lookup(&g)).contains("an installer log in install/"));
}

#[test]
fn without_a_content_record_only_an_installer_log_will_do() {
    let (_t, b) = base();
    let g = game(&b, true);
    assert!(unknown(lookup(&g)).contains("no content record"));
    log(&g, "inno.log", &["Game.exe"]);
    assert!(known(lookup(&g)).lists("game.exe"));
    fs::write(g.root.join("content.toml"), "schema = ").unwrap();
    assert!(unknown(lookup(&g)).contains("content record can't be read"));
}

#[test]
fn a_depot_game_without_its_build_in_the_cache_asks_for_a_fetch() {
    let (_t, b) = base();
    let g = game(&b, false);
    content(&g, "depot", &[9, 12]);
    match lookup(&g) {
        Lookup::Fetch(w) => {
            assert_eq!(w.product_id, 5);
            assert_eq!(w.build_id, "b1");
            assert_eq!(w.language, "en-US");
            assert_eq!(w.products, ["5", "9", "12"], "the DLC's depots too");
        }
        other => panic!("expected a fetch, got {other:?}"),
    }
    fs::write(
        g.root.join("content.toml"),
        "schema = 1\nlane = \"depot\"\n",
    )
    .unwrap();
    assert!(unknown(lookup(&g)).contains("names no build"));
}

fn resolved(g: &Game, locs: &[(&str, &str)]) -> Resolved {
    let s = CloudStorage {
        enabled: true,
        locations: locs
            .iter()
            .map(|(n, l)| RawLocation {
                name: n.to_string(),
                location: l.to_string(),
            })
            .collect(),
    };
    resolve_storage(Some(&s), "1", &g.layout)
}

fn listing(files: &[&str]) -> BuildFiles {
    let mut b = BuildFiles::new("the depot cache");
    for f in files {
        b.insert(f, crate::snapshot::saves::Expect::Size(1));
    }
    b
}

#[test]
fn game_file_locations_get_the_list_or_are_skipped() {
    let (_t, b) = base();
    let g = game(&b, false);
    fs::create_dir_all(g.layout.game_files.join("save")).unwrap();
    let locs = [
        ("docs", "<?DOCUMENTS?>/G"),
        ("all", "<?INSTALL?>"),
        ("sub", "<?INSTALL?>/SAVE"),
    ];

    let mut r = resolved(&g, &locs);
    let [docs, all, sub] = &r.locations[..] else {
        panic!("{r:?}")
    };
    assert!(!docs.in_game_files() && all.in_game_files() && sub.in_game_files());
    assert!(!bf(docs, "x.sav"));
    assert!(
        bf(all, "anything.sav") && bf(sub, "slot1.sav"),
        "no list attached: everything there counts as the build's"
    );

    apply(
        &mut r,
        &g,
        Ok(listing(&["Game.exe", "save/default.cfg", "Data/a.pak"])),
    );
    assert!(r.skipped.is_empty());
    let [docs, all, sub] = &r.locations[..] else {
        panic!("{r:?}")
    };
    assert!(docs.build_files.is_none());
    assert_eq!(
        all.build_files,
        Some(BuildNote {
            source: "the depot cache".into(),
            files: Some(3)
        })
    );
    assert!(bf(all, "GAME.EXE") && bf(all, "Data\\a.pak"));
    assert!(!bf(all, "save/slot1.sav"));
    assert!(
        bf(sub, "default.cfg"),
        "matched relative to the game's files, not the location"
    );
    assert!(!bf(sub, "slot1.sav") && !bf(sub, "Game.exe"));

    let mut r = resolved(&g, &locs);
    apply(&mut r, &g, Err("no list here".into()));
    let names: Vec<_> = r.locations.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["docs"], "the other locations still sync");
    let skipped: Vec<_> = r.skipped.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(skipped, ["all", "sub"]);
    assert_eq!(
        r.skipped[0].reason,
        SkipReason::BuildFilesUnknown("no list here".into())
    );
    assert!(
        r.skipped[0]
            .message
            .contains("can't tell the build's own files")
    );
    let json = serde_json::to_value(&r.skipped[0]).unwrap();
    assert_eq!(json["reason"]["kind"], "build_files_unknown");
}

#[test]
fn gogos_own_files_in_the_game_dir_are_left_out_too() {
    let (_t, b) = base();
    let g = game(&b, false);
    let gf = &g.layout.game_files;
    fs::write(gf.join("ddraw.ini"), "; gogo: cnc-ddraw\n").unwrap();
    fs::write(gf.join("ddraw.dll"), "x").unwrap();
    let mut r = resolved(&g, &[("all", "<?INSTALL?>")]);
    apply(&mut r, &g, Ok(listing(&["Game.exe"])));
    let all = &r.locations[0];
    assert!(bf(all, "ddraw.ini") && bf(all, "DDRAW.dll"));
    assert!(!bf(all, "slot.sav"));
}

#[test]
fn apply_offline_skips_a_depot_game_that_needs_a_fetch() {
    let (_t, b) = base();
    let g = game(&b, false);
    content(&g, "depot", &[]);
    let mut r = resolved(&g, &[("all", "<?INSTALL?>"), ("docs", "<?DOCUMENTS?>/G")]);
    apply_offline(&mut r, &g);
    assert_eq!(r.locations.len(), 1);
    assert_eq!(
        r.skipped[0].reason,
        SkipReason::BuildFilesUnknown(NOT_FETCHED.into())
    );
}

#[test]
fn an_older_depot_build_counts_when_its_info_names_the_build() {
    let (_t, b) = base();
    let g = game(&b, false);
    let info = |build: &str| {
        fs::write(
            g.layout.game_files.join("goggame-5.info"),
            format!(r#"{{"gameId":"5","rootGameId":"5","name":"G"{build},"playTasks":[]}}"#),
        )
        .unwrap();
    };
    info("");
    assert!(unknown(lookup(&g)).contains("info file names no build"));
    info(r#","buildId":"b7""#);
    match lookup(&g) {
        Lookup::Fetch(w) => {
            assert_eq!(w.build_id, "b7");
            assert_eq!(w.products, ["5"]);
        }
        other => panic!("expected a fetch, got {other:?}"),
    }
}

/// Whether cloud name `rel` below `l` lands on a build file.
fn bf(l: &crate::cloud::locations::Location, rel: &str) -> bool {
    l.build_path(&l.file(rel).unwrap())
}

/// A depot cache under `<base>/cache` for game 5, build `b1`: the record and
/// `files` (relative to the cache root, tree `G`, overlays included).
fn depot_cache(b: &Path, record: &str, files: &[&str]) -> PathBuf {
    let root = b.join("cache/gogo/depot/5");
    for f in files {
        let p = root.join(f);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"x").unwrap();
    }
    fs::write(root.join(".gogo-depot.json"), record).unwrap();
    b.join("cache")
}

fn content_lang(g: &Game, language: &str, dlcs: &[u64]) {
    content(g, "depot", dlcs);
    let raw = fs::read_to_string(g.root.join("content.toml")).unwrap();
    let raw = raw.replacen(
        "build_id = \"b1\"\n",
        &format!("build_id = \"b1\"\nlanguage = \"{language}\"\n"),
        1,
    );
    fs::write(g.root.join("content.toml"), raw).unwrap();
}

const RECORD: &str = r#"{"buildId":"b1","files":[],"language":"en-US","dlcs":["9"],
    "otherLanguages":{"de-DE":{"whole":true,"dlcs":[]}}}"#;

#[test]
fn a_depot_cache_of_the_installed_build_language_and_dlc_is_the_list() {
    let (_t, b) = base();
    let g = game(&b, false);
    let cache = depot_cache(
        &b,
        RECORD,
        &[
            "G/Game.exe",
            "G/DLC/extra.pak",
            "gog-support/redist/app/support.dll",
        ],
    );
    content_lang(&g, "en-US", &[9]);
    let list = known(lookup_in(&g, Some(&cache)));
    assert_eq!(list.source, "the depot cache");
    assert!(list.lists("Game.exe") && list.lists("dlc/EXTRA.pak"));
    assert!(
        list.lists("support.dll"),
        "a support overlay's files land in the game's files"
    );

    // Through the cloud filter: the overlay's file is the build's.
    let mut r = resolved(&g, &[("all", "<?INSTALL?>")]);
    apply(&mut r, &g, Ok(list));
    assert!(bf(&r.locations[0], "SUPPORT.DLL") && !bf(&r.locations[0], "slot.sav"));

    // Another language the cache still holds, without the DLC: fetch.
    content_lang(&g, "de-DE", &[9]);
    assert!(matches!(lookup_in(&g, Some(&cache)), Lookup::Fetch(_)));
    // A language the cache never held: fetch.
    content_lang(&g, "fr-FR", &[]);
    match lookup_in(&g, Some(&cache)) {
        Lookup::Fetch(w) => assert_eq!(w.language, "fr-FR"),
        other => panic!("expected a fetch, got {other:?}"),
    }
    // German, no DLC: the cache holds it.
    content_lang(&g, "de-DE", &[]);
    assert!(matches!(lookup_in(&g, Some(&cache)), Lookup::Known(_)));
    // A DLC the cache lacks: fetch.
    content_lang(&g, "en-US", &[9, 12]);
    assert!(matches!(lookup_in(&g, Some(&cache)), Lookup::Fetch(_)));
}

#[test]
fn cache_holds_needs_the_language_and_every_dlc() {
    let rec = |language: Option<&str>, dlcs: Option<Vec<&str>>| RecordSummary {
        build_id: "b1".into(),
        version: None,
        generation: None,
        language: language.map(str::to_string),
        dlcs: dlcs.map(|d| d.into_iter().map(str::to_string).collect()),
        other_languages: vec![("pl-PL".into(), vec!["9".into()])],
    };
    let r = rec(Some("en-US"), Some(vec!["9"]));
    assert!(cache_holds(&r, Some("en-US"), &[9]));
    assert!(cache_holds(&r, Some("EN-us"), &[]));
    assert!(cache_holds(&r, None, &[9]), "unknown installed language");
    assert!(!cache_holds(&r, Some("en-US"), &[9, 12]));
    assert!(
        cache_holds(&r, Some("pl-PL"), &[9]),
        "another held language"
    );
    assert!(!cache_holds(&r, Some("fr-FR"), &[]));
    let old = rec(None, None);
    assert!(cache_holds(&old, Some("en-US"), &[]), "an old record");
    assert!(!cache_holds(&old, Some("en-US"), &[9]), "no DLC tracking");
}

#[test]
fn a_dlc_log_that_names_no_file_means_no_list() {
    let (_t, b) = base();
    let g = game(&b, true);
    content(&g, "installer", &[9]);
    log(&g, "inno.log", &["Game.exe"]);
    fs::write(g.root.join("install/dlc-9-inno.log"), "Log opened.\r\n").unwrap();
    assert!(unknown(lookup(&g)).contains("names no file"));
    // The rollback stays lenient.
    let dirs = GameDirs::at(g.root.clone());
    let list = rollback::installer_list(&dirs, "GOG/G", &[9], false).unwrap();
    assert!(list.lists("game.exe"));
    assert!(rollback::installer_list(&dirs, "GOG/G", &[9], true).is_none());
}

#[test]
fn a_location_that_holds_the_game_files_is_filtered_too() {
    let (_t, b) = base();
    let mut g = game(&b, true);
    // The game's files below a user dir, so a variable's location holds
    // them (a `C:\GOG`-like location).
    let gf = g
        .layout
        .prefix
        .join("drive_c/users/steamuser/Documents/GOG/G");
    fs::create_dir_all(&gf).unwrap();
    g.layout.game_files = gf;
    let mut r = resolved(&g, &[("docs", "<?DOCUMENTS?>")]);
    assert!(
        r.locations[0].in_game_files(),
        "an ancestor of the game's files"
    );
    assert!(
        bf(&r.locations[0], "anything.sav"),
        "no list: nothing synced"
    );
    apply(&mut r, &g, Ok(listing(&["Game.exe"])));
    let docs = &r.locations[0];
    assert!(bf(docs, "GOG/G/game.exe") && bf(docs, "gog\\g\\GAME.EXE"));
    assert!(!bf(docs, "Game.exe"), "outside the game's files");
    assert!(!bf(docs, "GOG/G/slot.sav"));

    let mut r = resolved(&g, &[("docs", "<?DOCUMENTS?>")]);
    apply(&mut r, &g, Err("none".into()));
    assert!(r.locations.is_empty() && r.skipped.len() == 1);
}
