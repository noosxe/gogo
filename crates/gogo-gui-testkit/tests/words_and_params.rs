//! Enum words that are also source names (design 27 §7, "Names"), and route parameters
//! that are left out.

use gogo_gui_testkit::FixtureScope;
use gogo_skin::expr::{CallError, Scope};
use gogo_skin::ir::{Kind, Tree};
use gogo_skin::{Binding, DefaultSkin, Mode, SkinPackage, UiState, Value};

/// The route names the Default shell compares with that are also sources.
const BOTH: &[&str] = &[
    "library", "queue", "updates", "apps", "runtimes", "settings",
];

/// The fixture, answering each of [`BOTH`] as the app does: with a value that is not the
/// word.
struct Answered {
    fixture: FixtureScope,
    list: Value,
}

impl Scope for Answered {
    fn var(&self, name: &str) -> Option<&Value> {
        if BOTH.contains(&name) {
            return Some(&self.list);
        }
        self.fixture.var(name)
    }
    fn param(&self, name: &str) -> Option<&Value> {
        self.fixture.param(name)
    }
    fn token(&self, name: &str) -> Option<&Value> {
        self.fixture.token(name)
    }
    fn call(&self, name: &str, args: &[&Value]) -> Option<Result<Value, CallError>> {
        self.fixture.call(name, args)
    }
    fn now(&self) -> f64 {
        self.fixture.now()
    }
}

fn load(edit: impl Fn(&str, String) -> String) -> SkinPackage {
    let files: Vec<(String, Vec<u8>)> = gogo_skins::default()
        .to_files()
        .into_iter()
        .map(|(path, data)| {
            let text = String::from_utf8(data).expect("text");
            let data = edit(&path, text).into_bytes();
            (path, data)
        })
        .collect();
    match SkinPackage::from_files("default", files) {
        Ok(p) => p,
        Err(r) => panic!("{r}"),
    }
}

fn bind(pkg: &SkinPackage, route: &str, scope: &dyn Scope) -> Result<Tree, String> {
    let ui = UiState::defaults(&pkg.manifest);
    let b = Binding {
        scope,
        ui: &ui,
        mode: Mode::Dark,
        default: Some(DefaultSkin { skin: pkg, ui: &ui }),
    };
    pkg.bind(route, &b)
        .map(|bound| bound.tree)
        .map_err(|e| e.to_string())
}

/// How the button called `name` is painted.
fn paint(tree: &Tree, name: &str) -> String {
    let node = tree
        .iter()
        .filter_map(|id| tree.get(id))
        .find(|n| n.a11y.name.as_deref().is_some_and(|s| s.starts_with(name)))
        .unwrap_or_else(|| panic!("no node {name}"));
    format!("{:?}", node.paint)
}

#[test]
fn the_nav_marks_the_current_route_when_the_sources_of_those_names_are_answered() {
    // Without the outlet, so only the nav depends on `route.current`.
    let pkg = load(|path, text| match path {
        "routes/shell.kdl" => text.replacen("outlet", "spacer", 1),
        _ => text,
    });
    let navs = [
        ("library", "Library"),
        ("queue", "Queue"),
        ("updates", "Updates"),
        ("apps", "Installed apps"),
        ("runtimes", "Runtimes"),
        ("settings", "Settings"),
    ];
    let scope = |current: &str| Answered {
        fixture: FixtureScope::new().on(current, None),
        list: Value::List(Vec::new()),
    };
    // `about` has no nav button, so every button is off there.
    let off = bind(&pkg, "shell", &scope("about")).expect("binds");
    for (route, label) in navs {
        let on = bind(&pkg, "shell", &scope(route)).expect("binds");
        assert_ne!(paint(&on, label), paint(&off, label), "{route}");
    }
}

#[test]
fn an_absent_optional_route_parameter_is_null() {
    let with = |expr: &'static str| {
        load(move |path, text| match path {
            "routes/game.kdl" => {
                text.replacen("    tabs ", &format!("    text \"{expr}\"\n    tabs "), 1)
            }
            _ => text,
        })
    };
    let scope = FixtureScope::new();
    // `section` is optional on `game`; the fixture gives only `id`.
    let tree = bind(&with("{$section else 'no section'}"), "game", &scope).expect("binds");
    assert!(
        tree.iter()
            .filter_map(|id| tree.get(id))
            .any(|n| format!("{:?}", n.kind).contains("no section")),
    );
}

/// The Default skin with its `game` route's file replaced by `source`.
fn with_game_route(source: &'static str) -> SkinPackage {
    load(move |path, text| match path {
        "routes/game.kdl" => source.to_owned(),
        _ => text,
    })
}

#[test]
fn an_undeclared_or_missing_required_route_parameter_still_fails() {
    // Nothing but the expression reads a parameter, so the error is its own.
    let pkg = with_game_route("route \"game\" {\n    text \"{$nope else 'x'}\"\n}\n");
    let err = bind(&pkg, "game", &FixtureScope::new()).expect_err("unknown");
    assert_eq!(
        err,
        "route `game` (routes/game.kdl:2:10): unknown route parameter `$nope`"
    );
    // `id` is required on `game`.
    let pkg = with_game_route("route \"game\" {\n    text \"{$id}\"\n}\n");
    let scope = FixtureScope::new().on("game", None);
    let err = bind(&pkg, "game", &scope).expect_err("no id");
    assert_eq!(
        err,
        "route `game` (routes/game.kdl:2:10): unknown route parameter `$id`"
    );
}

#[test]
fn a_null_optional_parameter_cannot_reach_a_required_action_argument() {
    let pkg = with_game_route(
        "route \"game\" {\n    button \"Verify\" action=\"game.verify\" args=\"{$section}\"\n}\n",
    );
    let err = bind(&pkg, "game", &FixtureScope::new()).expect_err("null id");
    assert!(
        err.contains("`id` of `game.verify` is null here, and the action needs it"),
        "{err}"
    );
}

#[test]
fn a_bound_variable_beats_the_enum_word_of_the_same_name() {
    // `route.current` is `game`. As words, `library` and `queue` would not equal it; the
    // variables of those names hold `game`.
    let pkg = load(|path, text| match path {
        "routes/game.kdl" => "route \"game\" {\n    \
             column {\n        \
             let library=\"game\"\n        \
             text \"{route.current == library ? 'let wins' : 'word'}\"\n    \
             }\n    \
             use \"probe\" queue=\"game\"\n}\n"
            .to_owned(),
        "components/game.kdl" => format!(
            "{text}\ncomponent \"probe\" params=\"queue\" {{\n    \
             text \"{{route.current == queue ? 'param wins' : 'word'}}\"\n}}\n"
        ),
        _ => text,
    });
    let tree = bind(&pkg, "game", &FixtureScope::new()).expect("binds");
    let texts: Vec<String> = tree
        .iter()
        .filter_map(|id| tree.get(id))
        .filter_map(|n| match &n.kind {
            Kind::Text { text, .. } => Some(text.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["let wins", "param wins"]);
}
