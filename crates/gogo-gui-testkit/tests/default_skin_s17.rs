//! S17 (design 26): the built-in Default skin loads through the public loader with no
//! findings at all, binds every route it covers against the fixture scope without falling
//! back, and covers the routes it says it covers (design 27 §9, §10).

use std::collections::BTreeSet;

use gogo_contract::routes::ROUTES;
use gogo_gui_testkit::{FixtureScope, GAME_ID};
use gogo_skin::{Binding, DefaultSkin, Mode, SkinPackage, UiState};

fn skin() -> SkinPackage {
    let d = gogo_skins::default();
    match SkinPackage::from_files(d.name, d.to_files()) {
        Ok(p) => p,
        Err(r) => panic!("{r}"),
    }
}

/// The routes of contract 1.0 the Default skin has no route file for yet. It is the
/// fallback for every route (design 26 §8), so this list only shrinks: a route that
/// becomes covered must be removed from it, and every route is either covered or listed.
/// Tracking issue: #576. Until a route is covered, binding it returns a bind error (the
/// skin has no such route), even through `bind_with_fallback`: the Default skin has nothing
/// to fall back to.
const NOT_YET_COVERED: &[&str] = &["saves", "apps", "runtimes", "doctor", "settings", "about"];

fn scope_on(route: &str) -> FixtureScope {
    FixtureScope::new().on(route, (route == "game").then_some(GAME_ID))
}

#[test]
fn the_default_skin_loads_with_no_errors_and_no_warnings() {
    let pkg = skin();
    assert!(pkg.report().findings().is_empty(), "{}", pkg.report());
    assert!(pkg.report().is_clean(), "{}", pkg.report());
    assert_eq!(pkg.manifest.name, "default");
}

#[test]
fn every_route_the_default_skin_covers_binds_without_falling_back() {
    let pkg = skin();
    for route in pkg.routes.keys().filter(|r| *r != "shell") {
        let scope = scope_on(route);
        let ui = UiState::defaults(&pkg.manifest);
        let b = Binding {
            scope: &scope,
            ui: &ui,
            mode: Mode::Dark,
            default: Some(DefaultSkin {
                skin: &pkg,
                ui: &ui,
            }),
        };
        let bound = match pkg.bind_with_fallback(route, &b) {
            Ok(b) => b,
            Err(e) => panic!("{route}: {e}"),
        };
        bound.tree.validate().expect("a valid tree");
        assert!(bound.fallbacks.is_empty(), "{route}: {:?}", bound.fallbacks);
    }
}

#[test]
fn the_shell_binds_with_each_covered_route_in_its_outlet() {
    let pkg = skin();
    assert!(pkg.routes.contains_key("shell"));
    for route in pkg.routes.keys().filter(|r| *r != "shell") {
        let scope = scope_on(route);
        let ui = UiState::defaults(&pkg.manifest);
        let b = Binding {
            scope: &scope,
            ui: &ui,
            mode: Mode::Dark,
            default: Some(DefaultSkin {
                skin: &pkg,
                ui: &ui,
            }),
        };
        let bound = match pkg.bind_with_fallback("shell", &b) {
            Ok(b) => b,
            Err(e) => panic!("shell around {route}: {e}"),
        };
        bound.tree.validate().expect("a valid tree");
        assert!(
            bound.fallbacks.is_empty(),
            "shell around {route}: {:?}",
            bound.fallbacks
        );
    }
}

#[test]
fn every_route_is_covered_or_listed_as_not_yet_covered() {
    let pkg = skin();
    let covered: BTreeSet<&str> = pkg.routes.keys().map(String::as_str).collect();
    let listed: BTreeSet<&str> = NOT_YET_COVERED.iter().copied().collect();
    assert_eq!(
        listed.len(),
        NOT_YET_COVERED.len(),
        "a route is listed twice"
    );
    let contract: BTreeSet<&str> = ROUTES.iter().map(|r| r.name).collect();
    for r in &covered {
        assert!(contract.contains(r), "the skin covers `{r}`, not a route");
    }
    for r in &listed {
        assert!(contract.contains(r), "`{r}` is not a route");
    }
    let now_covered: Vec<_> = listed.intersection(&covered).collect();
    assert!(
        now_covered.is_empty(),
        "covered now, remove from NOT_YET_COVERED: {now_covered:?}"
    );
    let missing: Vec<_> = contract
        .iter()
        .filter(|r| !covered.contains(*r) && !listed.contains(*r))
        .collect();
    assert!(
        missing.is_empty(),
        "neither covered nor listed: {missing:?}"
    );
}

#[test]
fn the_game_route_binds_on_every_tab_without_falling_back() {
    let pkg = skin();
    let tabs = [
        "overview",
        "builds",
        "dlc",
        "saves",
        "snapshots",
        "setup",
        "files",
    ];
    for tab in tabs {
        let scope = scope_on("game");
        let mut ui = UiState::defaults(&pkg.manifest);
        ui.set(
            &pkg.manifest,
            "tab",
            gogo_contract::Value::Str(tab.to_owned()),
        )
        .unwrap_or_else(|e| panic!("{tab}: {e}"));
        let b = Binding {
            scope: &scope,
            ui: &ui,
            mode: Mode::Dark,
            default: Some(DefaultSkin {
                skin: &pkg,
                ui: &ui,
            }),
        };
        let bound = match pkg.bind_with_fallback("game", &b) {
            Ok(b) => b,
            Err(e) => panic!("game tab {tab}: {e}"),
        };
        bound.tree.validate().expect("a valid tree");
        assert!(bound.fallbacks.is_empty(), "{tab}: {:?}", bound.fallbacks);
    }
}

#[test]
fn an_uncovered_route_returns_a_bind_error() {
    let pkg = skin();
    let route = NOT_YET_COVERED[0];
    let scope = scope_on(route);
    let ui = UiState::defaults(&pkg.manifest);
    let b = Binding {
        scope: &scope,
        ui: &ui,
        mode: Mode::Dark,
        default: Some(DefaultSkin {
            skin: &pkg,
            ui: &ui,
        }),
    };
    let Err(e) = pkg.bind_with_fallback(route, &b) else {
        panic!("{route} bound, remove it from NOT_YET_COVERED");
    };
    assert!(
        e.to_string()
            .contains(&format!("the skin has no `{route}` route")),
        "{e}"
    );
}
