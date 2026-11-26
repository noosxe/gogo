//! What the Default skin shows on each route beyond the game page, state by state: the
//! content is there, not only a tree that binds.

use gogo_gui_testkit::FixtureScope;
use gogo_skin::ir::{Kind, Tree};
use gogo_skin::{Binding, DefaultSkin, Mode, SkinPackage, UiState, Value};

fn skin() -> SkinPackage {
    let d = gogo_skins::default();
    match SkinPackage::from_files(d.name, d.to_files()) {
        Ok(p) => p,
        Err(r) => panic!("{r}"),
    }
}

/// Binds `route` with the `ui` values given, and fails on any fallback.
fn bind(route: &str, scope: &FixtureScope, ui_values: &[(&str, &str)]) -> Tree {
    let pkg = skin();
    let mut ui = UiState::defaults(&pkg.manifest);
    for (name, value) in ui_values {
        ui.set(&pkg.manifest, name, Value::str(*value))
            .unwrap_or_else(|e| panic!("{name}={value}: {e}"));
    }
    let b = Binding {
        scope,
        ui: &ui,
        mode: Mode::Dark,
        default: Some(DefaultSkin {
            skin: &pkg,
            ui: &ui,
        }),
    };
    let bound = pkg
        .bind_with_fallback(route, &b)
        .unwrap_or_else(|e| panic!("{route}: {e}"));
    bound.tree.validate().expect("a valid tree");
    assert!(bound.fallbacks.is_empty(), "{:?}", bound.fallbacks);
    bound.tree
}

fn on(route: &str) -> FixtureScope {
    FixtureScope::new().on(route, None)
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

#[track_caller]
fn shows(words: &[String], expected: &[&str]) {
    for e in expected {
        assert!(
            words.iter().any(|w| w.contains(e)),
            "no `{e}` in {words:#?}"
        );
    }
}

#[track_caller]
fn hides(words: &[String], unexpected: &[&str]) {
    for e in unexpected {
        assert!(!words.iter().any(|w| w.contains(e)), "`{e}` in {words:#?}");
    }
}

fn list(items: Vec<Value>) -> Value {
    Value::List(items)
}

#[test]
fn sign_in_offers_signing_in_or_out() {
    let signed_in = words(&bind("sign-in", &on("sign-in"), &[]));
    shows(
        &signed_in,
        &["Signed in", "player-one", "Sign out and delete local data"],
    );
    hides(&signed_in, &["Sign in to GOG"]);

    let mut scope = on("sign-in");
    scope.set_field("account", "signed_in", Value::Bool(false));
    scope.set_field("account", "username", Value::Null);
    let signed_out = words(&bind("sign-in", &scope, &[]));
    shows(&signed_out, &["Sign in to GOG", "Login page"]);
    hides(&signed_out, &["Sign out"]);
}

#[test]
fn the_library_shows_every_game_in_both_views() {
    for view in ["grid", "list"] {
        let w = words(&bind("library", &on("library"), &[("view", view)]));
        shows(
            &w,
            &[
                "12 games",
                "Example Quest",
                "Pattern Peaks",
                "Play Placeholder Tactics",
                "Update Example Quest",
                "Details of Draft Kingdoms",
            ],
        );
        hides(
            &w,
            &["Play Draft Kingdoms", "Play Mock Harbor", "No games yet"],
        );
    }
}

#[test]
fn the_library_says_when_it_is_empty_or_signed_out() {
    let mut scope = on("library");
    scope.set_var("library", list(Vec::new()));
    scope.set_field("account", "signed_in", Value::Bool(false));
    for view in ["grid", "list"] {
        let w = words(&bind("library", &scope, &[("view", view)]));
        shows(&w, &["No games yet", "You are signed out"]);
    }
}

/// A job of the queue in `state`, from the fixture's first.
fn job_in(state: &str) -> FixtureScope {
    use gogo_skin::expr::Scope;
    let mut scope = on("queue");
    let Some(Value::List(jobs)) = scope.var("queue") else {
        panic!("no queue")
    };
    let Value::Map(mut job) = jobs[0].clone() else {
        panic!("not a job")
    };
    job.insert("state".to_owned(), Value::str(state));
    scope.set_var("queue", list(vec![Value::Map(job)]));
    scope
}

#[test]
fn each_job_state_offers_its_own_controls() {
    const ALL: [&str; 4] = ["Pause ", "Resume ", "Retry ", "Cancel "];
    for (state, offered) in [
        ("queued", &["Pause ", "Cancel "][..]),
        ("running", &["Pause ", "Cancel "]),
        ("paused", &["Resume ", "Cancel "]),
        ("failed", &["Retry "]),
        ("cancelled", &["Resume "]),
        ("done", &[]),
    ] {
        let w = words(&bind("queue", &job_in(state), &[]));
        shows(&w, &["Sample Racer", state]);
        for control in ALL {
            let name = format!("{control}Sample Racer");
            assert_eq!(
                w.contains(&name),
                offered.contains(&control),
                "{state}: {name}"
            );
        }
    }
}

#[test]
fn the_queue_shows_steps_files_errors_and_the_empty_state() {
    let w = words(&bind("queue", &on("queue"), &[]));
    shows(
        &w,
        &[
            "1 active · 2 queued",
            "62%",
            "Check the build",
            "data/tracks.pak",
            "Files don't match",
            "Repair Template Tales",
        ],
    );
    let mut scope = on("queue");
    scope.set_var("queue", list(Vec::new()));
    shows(&words(&bind("queue", &scope, &[])), &["Nothing is queued"]);
}

#[test]
fn updates_lists_the_hints_and_says_when_there_are_none() {
    let w = words(&bind("updates", &on("updates"), &[]));
    shows(&w, &["Update all", "Example Quest", "1.0.2", "1.1.0"]);
    hides(&w, &["Removes DLC", "up to date"]);

    let mut scope = on("updates");
    scope.set_var("updates", list(Vec::new()));
    let w = words(&bind("updates", &scope, &[]));
    shows(&w, &["Everything is up to date"]);
    hides(&w, &["Update all"]);
}

#[test]
fn the_shell_offers_signing_in_only_when_signed_out() {
    let mut scope = on("library");
    hides(&words(&bind("shell", &scope, &[])), &["Sign in"]);
    scope.set_field("account", "signed_in", Value::Bool(false));
    shows(&words(&bind("shell", &scope, &[])), &["Sign in"]);
}
