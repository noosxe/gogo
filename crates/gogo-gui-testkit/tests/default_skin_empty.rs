//! The Default skin over data that isn't there: every route, tab and view binds when lists
//! are empty or hold one item and optional fields are null (design 26 §3, design 27 §7).
//! A missing part leaves no `null`, no dangling separator and no half-written line.

use gogo_contract::routes::ROUTES;
use gogo_gui_testkit::{FixtureScope, GAME_ID};
use gogo_skin::ir::{Kind, Tree};
use gogo_skin::{Binding, DefaultSkin, Mode, SkinPackage, UiState, Value};

const TABS: [&str; 7] = [
    "overview",
    "builds",
    "dlc",
    "saves",
    "snapshots",
    "setup",
    "files",
];

fn skin() -> SkinPackage {
    let d = gogo_skins::default();
    match SkinPackage::from_files(d.name, d.to_files()) {
        Ok(p) => p,
        Err(r) => panic!("{r}"),
    }
}

/// Binds `route` of `scope`, and fails on an error or a fallback.
#[track_caller]
fn bind(
    pkg: &SkinPackage,
    route: &str,
    scope: &FixtureScope,
    ui_value: Option<(&str, &str)>,
) -> Tree {
    let mut ui = UiState::defaults(&pkg.manifest);
    if let Some((name, value)) = ui_value {
        ui.set(&pkg.manifest, name, Value::str(value))
            .unwrap_or_else(|e| panic!("{name}={value}: {e}"));
    }
    let b = Binding {
        scope,
        ui: &ui,
        mode: Mode::Dark,
        default: Some(DefaultSkin { skin: pkg, ui: &ui }),
    };
    let bound = pkg
        .bind_with_fallback(route, &b)
        .unwrap_or_else(|e| panic!("{route} {ui_value:?}: {e}"));
    bound.tree.validate().expect("a valid tree");
    assert!(
        bound.fallbacks.is_empty(),
        "{route} {ui_value:?}: {:?}",
        bound.fallbacks
    );
    bound.tree
}

/// Every text drawn and every accessible name.
fn words(tree: &Tree) -> Vec<String> {
    let mut out = Vec::new();
    for node in tree.iter().filter_map(|id| tree.get(id)) {
        if let Kind::Text { text, .. } = &node.kind {
            out.push(text.to_string());
        }
        if let Some(name) = &node.a11y.name {
            out.push(name.to_string());
        }
    }
    out
}

/// No `null`, and nothing left of a line whose value is missing.
#[track_caller]
fn tidy(what: &str, words: &[String]) {
    for w in words {
        let t = w.trim();
        let dangling = t.starts_with('·')
            || t.starts_with("missing")
            || t.contains("· ·")
            // "<title> missing ·" leads into its link.
            || (t.ends_with('·') && !t.ends_with("missing ·"))
            || t == "played"
            || t.ends_with("· played")
            || ["checked", "Checked", "last played", "Last sync", "Last launched"]
                .iter()
                .any(|end| t.ends_with(end));
        assert!(!dangling && !t.contains("null"), "{what}: `{w}`");
    }
}

/// Every route, tab and view of the skin over `scope`, each also inside the shell.
fn every_screen(scope: &FixtureScope) {
    let pkg = skin();
    for spec in ROUTES.iter().filter(|r| r.name != "shell") {
        let route = spec.name;
        let ids: &[Option<&str>] = match route {
            "game" => &[Some(GAME_ID)],
            "doctor" => &[None, Some(GAME_ID)],
            _ => &[None],
        };
        let views: Vec<Option<(&str, &str)>> = match route {
            "game" => TABS.iter().map(|t| Some(("tab", *t))).collect(),
            "library" => vec![Some(("view", "grid")), Some(("view", "list"))],
            _ => vec![None],
        };
        for id in ids {
            let scope = scope.clone().on(route, *id);
            for view in &views {
                let what = format!("{route} {id:?} {view:?}");
                tidy(&what, &words(&bind(&pkg, route, &scope, *view)));
                tidy(&what, &words(&bind(&pkg, "shell", &scope, *view)));
            }
        }
    }
}

/// The words of the shell with the about page in it, checked to be tidy.
#[track_caller]
fn shell(scope: &FixtureScope) -> Vec<String> {
    let scope = scope.clone().on("about", None);
    let w = words(&bind(&skin(), "shell", &scope, None));
    tidy("shell", &w);
    w
}

/// The words of one route over `scope`, checked to be tidy.
#[track_caller]
fn shown(route: &str, id: Option<&str>, scope: &FixtureScope, tab: &str) -> Vec<String> {
    let scope = scope.clone().on(route, id);
    let view = (route == "game").then_some(("tab", tab));
    let w = words(&bind(&skin(), route, &scope, view));
    tidy(route, &w);
    w
}

fn overview(scope: &FixtureScope) -> Vec<String> {
    shown("game", Some(GAME_ID), scope, "overview")
}

#[track_caller]
fn has(words: &[String], expected: &str) {
    assert!(
        words.iter().any(|w| w.contains(expected)),
        "no `{expected}` in {words:#?}"
    );
}

#[track_caller]
fn lacks(words: &[String], unexpected: &str) {
    assert!(
        !words.iter().any(|w| w.contains(unexpected)),
        "`{unexpected}` in {words:#?}"
    );
}

fn map(fields: &[(&str, Value)]) -> Value {
    Value::Map(
        fields
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
    )
}

#[test]
fn every_screen_binds_with_nothing_to_show() {
    every_screen(&FixtureScope::emptied());
}

#[test]
fn every_screen_binds_with_every_optional_field_null() {
    every_screen(&FixtureScope::without_optionals());
}

#[test]
fn every_screen_binds_with_one_item_in_every_list() {
    every_screen(&FixtureScope::single_items());
}

#[test]
fn every_screen_binds_with_every_saves_and_galaxy_state() {
    for saves in [
        "in_sync",
        "to_pull",
        "to_push",
        "conflicts",
        "disabled",
        "no_remote_config",
    ] {
        for galaxy in ["signed_in", "signed_out", "not_installed"] {
            let mut scope = FixtureScope::new();
            for id in [GAME_ID, "9", "11", "12", "13"] {
                scope.set_in_call("game.saves", id, "state", Value::str(saves));
            }
            scope.set_in_call(
                "game.runner",
                GAME_ID,
                "galaxy",
                map(&[
                    ("setting", Value::str("default")),
                    ("state", Value::str(galaxy)),
                ]),
            );
            every_screen(&scope);
        }
    }
}

#[test]
fn the_saves_tile_is_only_there_for_a_game_with_cloud_saves() {
    for state in ["disabled", "no_remote_config"] {
        let mut scope = FixtureScope::new();
        scope.set_in_call("game.saves", GAME_ID, "state", Value::str(state));
        lacks(&overview(&scope), "Saves");
    }
    let mut scope = FixtureScope::new();
    scope.set_in_call("game.saves", GAME_ID, "last_sync", Value::Null);
    let w = overview(&scope);
    has(&w, "Saves in sync");
    assert!(w.iter().any(|w| w == "1 files"), "{w:#?}");
}

#[test]
fn the_dlc_tile_follows_what_there_is_to_add() {
    let dlc = |installed: bool| {
        map(&[
            ("id", Value::str("2")),
            ("title", Value::str("Bonus Pack")),
            ("owned", Value::Bool(true)),
            ("installed", Value::Bool(installed)),
            ("download_size", Value::Num(1.0)),
            ("disk_size", Value::Num(1.0)),
            ("in_this_build", Value::Bool(true)),
        ])
    };
    let with = |items: Vec<Value>| {
        let mut scope = FixtureScope::new();
        scope.set_call("game.dlc", GAME_ID, Value::List(items));
        overview(&scope)
    };

    let none = with(Vec::new());
    lacks(&none, "DLC 0 of 0");
    lacks(&none, "missing");
    lacks(&none, "Add…");

    let all_installed = with(vec![dlc(true)]);
    has(&all_installed, "DLC 1 of 1");
    lacks(&all_installed, "missing");
    lacks(&all_installed, "Add…");

    let one_missing = with(vec![dlc(false)]);
    has(&one_missing, "DLC 0 of 1");
    has(&one_missing, "Bonus Pack missing ·");
    has(&one_missing, "Add…");
}

#[test]
fn a_warning_count_with_no_warning_listed_has_no_second_line() {
    let mut scope = FixtureScope::new();
    scope.set_in_call("game.doctor", GAME_ID, "checks", Value::List(Vec::new()));
    let w = overview(&scope);
    has(&w, "Doctor: 1 warning");
}

#[test]
fn the_meta_line_has_only_the_parts_that_are_known() {
    for known in 0..32u8 {
        let is = |bit: u8| known & (1 << bit) != 0;
        let mut scope = FixtureScope::new();
        let mut parts = Vec::new();
        let mut set =
            |field: &str, there: bool, value: Value, missing: Value, part: &'static str| {
                if there {
                    parts.push(part);
                }
                scope.set_in_call("game", GAME_ID, field, if there { value } else { missing });
            };
        let day = 86_400.0;
        set(
            "genre",
            is(0),
            Value::str("Puzzle"),
            Value::str(""),
            "Puzzle",
        );
        set("year", is(1), Value::Num(2001.0), Value::Num(0.0), "2001");
        set(
            "playtime",
            is(2),
            Value::Num(7200.0),
            Value::Null,
            "2 h played",
        );
        set(
            "last_played",
            is(3),
            Value::Num(gogo_gui_testkit::NOW - 3.0 * day),
            Value::Null,
            "last played 3 days ago",
        );
        set(
            "size_on_disk",
            is(4),
            Value::Num(5e9),
            Value::Null,
            "5.0 GB",
        );
        let line = parts.join(" · ");
        let w = overview(&scope);
        if !line.is_empty() {
            assert!(w.contains(&line), "no `{line}` in {w:#?}");
        }
    }
}

#[test]
fn a_game_the_doctor_list_lacks_is_still_shown() {
    let mut scope = FixtureScope::new();
    scope.set_var("doctor.all", Value::List(Vec::new()));
    let w = shown("doctor", Some(GAME_ID), &scope, "");
    has(&w, "This game");
    has(&w, "Nothing to check yet.");
}

#[test]
fn the_shell_names_a_job_that_has_no_game_by_its_kind() {
    use gogo_skin::expr::Scope;
    let mut scope = FixtureScope::new();
    let Some(Value::Map(summary)) = scope.var("queue.summary") else {
        panic!("no queue summary")
    };
    let Some(Value::Map(mut job)) = summary.get("current").cloned() else {
        panic!("no current job")
    };
    for field in ["game", "total", "eta", "fraction"] {
        job.insert(field.to_owned(), Value::Null);
    }
    job.insert("kind".to_owned(), Value::str("runtime_install"));
    scope.set_field("queue.summary", "current", Value::Map(job));
    let w = shell(&scope);
    has(&w, "runtime_install");
    lacks(&w, "min left");
    lacks(&w, "%");
}

#[test]
fn a_signed_in_account_with_no_name_has_no_empty_line() {
    let mut scope = FixtureScope::new();
    scope.set_field("account", "username", Value::Null);
    let sign_in = shown("sign-in", None, &scope, "");
    assert!(!sign_in.iter().any(String::is_empty), "{sign_in:#?}");
    has(&shell(&scope), "Signed in");
}
