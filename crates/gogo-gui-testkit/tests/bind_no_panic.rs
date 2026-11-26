//! Whatever the data, binding ends with a result and never panics (design 27 §7,
//! "Failures"): numbers that are NaN, infinite or huge, very long strings, nulls and masks
//! in the places a skin reads them.

use gogo_gui_testkit::{FixtureScope, GAME_ID};
use gogo_skin::expr::Scope;
use gogo_skin::{Binding, Mode, SkinPackage, UiState, Value};
use proptest::prelude::*;

fn skin() -> SkinPackage {
    let d = gogo_skins::default();
    match SkinPackage::from_files(d.name, d.to_files()) {
        Ok(p) => p,
        Err(r) => panic!("{r}"),
    }
}

/// The sources with an argument the game route reads, answered for the fixture game.
const CALLS: &[&str] = &[
    "game",
    "game.runner",
    "game.snapshots",
    "game.saves",
    "game.dlc",
    "game.doctor",
    "game.builds",
    "game.env",
];

/// The sources without one the shell reads.
const VARS: &[&str] = &["queue.summary", "account", "app", "library.stats"];

const SECTIONS: &[&str] = &[
    "overview",
    "builds",
    "dlc",
    "saves",
    "snapshots",
    "setup",
    "files",
];

fn hostile() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Num(f64::NAN)),
        Just(Value::Num(f64::INFINITY)),
        Just(Value::Num(f64::NEG_INFINITY)),
        Just(Value::Num(1e308)),
        Just(Value::Num(-1e308)),
        Just(Value::Num(f64::MIN_POSITIVE)),
        Just(Value::Num(-0.0)),
        any::<f64>().prop_map(Value::Num),
        "\\PC{0,64}".prop_map(Value::Str),
        (1usize..30_000).prop_map(|n| Value::Str("w\u{e9}".repeat(n))),
        Just(Value::Str(String::new())),
        Just(Value::Null),
        Just(Value::Masked),
        any::<bool>().prop_map(Value::Bool),
        Just(Value::List(Vec::new())),
        Just(Value::Map(Default::default())),
    ]
}

/// How many scalars `v` has.
fn leaves(v: &Value) -> usize {
    match v {
        Value::List(l) => l.iter().map(leaves).sum(),
        Value::Map(m) => m.values().map(leaves).sum(),
        _ => 1,
    }
}

/// Put `with` in place of the `at`-th scalar of `v`.
fn replace(v: &mut Value, at: &mut usize, with: &Value) {
    match v {
        Value::List(l) => l.iter_mut().for_each(|x| replace(x, at, with)),
        Value::Map(m) => m.values_mut().for_each(|x| replace(x, at, with)),
        _ => {
            if *at == 0 {
                *v = with.clone();
            }
            *at = at.wrapping_sub(1);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// A scalar of some source in place of what the fixture has there.
    #[test]
    fn hostile_data_never_panics_the_binder(
        edits in prop::collection::vec((0..CALLS.len() + VARS.len(), any::<usize>(), hostile()), 1..4),
        section in 0..SECTIONS.len(),
        shell in any::<bool>(),
    ) {
        let pkg = skin();
        let mut scope = FixtureScope::new();
        for (which, at, with) in edits {
            if which < CALLS.len() {
                let id = Value::str(GAME_ID);
                let Some(Ok(mut v)) = scope.call(CALLS[which], &[&id]) else { continue };
                let n = leaves(&v).max(1);
                replace(&mut v, &mut (at % n), &with);
                scope.set_call(CALLS[which], GAME_ID, v);
            } else {
                let name = VARS[which - CALLS.len()];
                let Some(mut v) = scope.var(name).cloned() else { continue };
                let n = leaves(&v).max(1);
                replace(&mut v, &mut (at % n), &with);
                scope.set_var(name, v);
            }
        }
        let mut ui = UiState::defaults(&pkg.manifest);
        let _ = ui.set(&pkg.manifest, "tab", Value::str(SECTIONS[section]));
        let b = Binding { scope: &scope, ui: &ui, mode: Mode::Dark, default: None };
        let route = if shell { "shell" } else { "game" };
        if let Ok(bound) = pkg.bind(route, &b) {
            // What binds is a tree the engine can take.
            prop_assert!(bound.tree.validate().is_ok());
        }
    }

    /// A `ui` number is any number, and a state restored from anything is still a state.
    #[test]
    fn a_ui_state_from_anything_binds(
        tab in hostile(),
        view in hostile(),
    ) {
        let pkg = skin();
        let ui = UiState::restore(
            &pkg.manifest,
            [("tab".to_owned(), tab), ("view".to_owned(), view)],
        );
        let scope = FixtureScope::new();
        let b = Binding { scope: &scope, ui: &ui, mode: Mode::Dark, default: None };
        prop_assert!(pkg.bind("game", &b).is_ok());
    }
}
