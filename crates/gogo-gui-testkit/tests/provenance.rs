//! The traced bind of the skin dev mode (design 25 §8): the Default skin edited in memory,
//! on the fixture data.

use gogo_gui_testkit::{FixtureScope, GAME_ID};
use gogo_skin::ir::{NodeId, Tree};
use gogo_skin::{
    BindErrorKind, Binding, Bound, DefaultSkin, Lists, MAX_TRACED, Mode, Origin, SkinPackage,
    UiState, Value,
};

/// The Default skin with `nodes` added to the game route, ahead of its tabs, and `files`
/// beside it.
fn skin(nodes: &str, files: &[(&str, &str)]) -> SkinPackage {
    let mut all = gogo_skins::default().to_files();
    for (path, data) in &mut all {
        if path == "routes/game.kdl" {
            let text = String::from_utf8(std::mem::take(data)).unwrap();
            assert!(text.contains("    tabs "));
            *data = text
                .replacen("    tabs ", &format!("    {nodes}\n    tabs "), 1)
                .into_bytes();
        }
    }
    all.extend(
        files
            .iter()
            .map(|(p, text)| ((*p).to_owned(), text.as_bytes().to_vec())),
    );
    let pkg = SkinPackage::from_files("default", all).unwrap_or_else(|r| panic!("{r}"));
    assert!(
        pkg.report().findings().iter().all(|f| !f.is_error()),
        "{}",
        pkg.report()
    );
    pkg
}

fn bind(pkg: &SkinPackage, default: Option<&SkinPackage>, traced: bool) -> Bound {
    bind_on(&game_scope(), pkg, default, traced).unwrap_or_else(|e| panic!("{e}"))
}

fn game_scope() -> FixtureScope {
    FixtureScope::new().on("game", Some(GAME_ID))
}

fn bind_on(
    scope: &FixtureScope,
    pkg: &SkinPackage,
    default: Option<&SkinPackage>,
    traced: bool,
) -> Result<Bound, gogo_skin::BindError> {
    let ui = UiState::defaults(&pkg.manifest);
    let default_ui = default.map(|d| UiState::defaults(&d.manifest));
    let b = Binding {
        scope,
        ui: &ui,
        mode: Mode::Dark,
        default: default
            .zip(default_ui.as_ref())
            .map(|(skin, ui)| DefaultSkin { skin, ui }),
    };
    let lists = Lists {
        height: 600.0,
        ..Lists::default()
    };
    if traced {
        pkg.bind_traced("game", &b, &lists)
    } else {
        pkg.bind_windowed("game", &b, &lists)
    }
}

/// The game scope with one secret environment variable.
fn secret_scope() -> FixtureScope {
    let mut scope = game_scope();
    let row = [
        ("name".to_owned(), Value::str("TOKEN")),
        ("value".to_owned(), Value::Masked),
    ];
    let rows = Value::List(vec![Value::Map(row.into_iter().collect())]);
    scope.set_call("game.env", GAME_ID, rows);
    scope
}

fn named(tree: &Tree, name: &str) -> Vec<NodeId> {
    tree.iter()
        .filter(|&id| {
            tree.get(id)
                .is_some_and(|n| n.name.as_deref() == Some(name))
        })
        .collect()
}

fn origin<'a>(bound: &'a Bound, name: &str) -> &'a Origin {
    let ids = named(&bound.tree, name);
    let id = *ids.first().unwrap_or_else(|| panic!("no node `{name}`"));
    bound
        .provenance
        .as_ref()
        .unwrap()
        .get(id)
        .unwrap_or_else(|| panic!("`{name}` has no origin"))
}

fn binding<'a>(o: &'a Origin, name: &str) -> (&'a str, &'a str) {
    let b = o
        .bindings
        .iter()
        .find(|b| b.name == name)
        .unwrap_or_else(|| panic!("no binding `{name}` in {:?}", o.bindings));
    (&b.source, &b.value)
}

#[test]
fn an_element_says_where_it_is_and_what_its_expressions_gave() {
    let pkg = skin(
        "let who=\"{game($id).title}\"\n    text \"{who}\" id=\"probe\"",
        &[],
    );
    let bound = bind(&pkg, None, true);
    let o = origin(&bound, "probe");
    assert_eq!(o.element, "text");
    assert!(o.location.starts_with("routes/game.kdl:"), "{}", o.location);
    assert!(!o.default_skin);
    let (source, value) = binding(o, "value");
    assert_eq!(source, "\"{who}\"");
    assert!(value.starts_with('"') && value.len() > 2, "{value}");
    let who = o.locals.iter().find(|(n, _)| n == "who").unwrap();
    assert!(who.1.contains(value), "{who:?} {value}");
    // Literals are not bindings.
    assert!(o.bindings.iter().all(|b| b.name != "id"));
}

#[test]
fn repeats_components_and_cases_are_listed_around_a_node() {
    let pkg = skin(
        "each row in=\"{game.builds($id) | take 2}\" { use \"probe-card\" word=\"{row.version}\" }",
        &[(
            "components/probe-card.kdl",
            "component \"probe-card\" params=\"word\" {\n    switch \"{word == '' ? 'none' : 'some'}\" {\n        case \"some\" { text \"{word}\" id=\"some\" }\n        else { text \"-\" }\n    }\n}\n",
        )],
    );
    let bound = bind(&pkg, None, true);
    let o = origin(&bound, "some");
    let within = o.within.join(" | ");
    assert!(within.contains("each row, item 1 of 2"), "{within}");
    assert!(within.contains("use \"probe-card\""), "{within}");
    assert!(within.contains("case \"some\""), "{within}");
    assert!(o.location.starts_with("components/probe-card.kdl:"));
    assert_eq!(
        o.locals.len(),
        1,
        "a component sees its parameters only: {:?}",
        o.locals
    );
    assert_eq!(o.locals[0].0, "word");
}

#[test]
fn what_a_condition_leaves_out_is_listed_on_the_parent() {
    let pkg = skin(
        "column id=\"holder\" {\n        text \"no\" when=\"{1 == 2}\"\n        each x in=\"{game.builds($id) | take 0}\" { text \"{x}\" }\n        switch \"{3}\" { case \"4\" { text \"four\" } }\n    }",
        &[],
    );
    let bound = bind(&pkg, None, true);
    let holder = named(&bound.tree, "holder")[0];
    let left = bound.provenance.as_ref().unwrap().left_out(holder);
    let elements: Vec<&str> = left.iter().map(|l| l.element.as_str()).collect();
    assert_eq!(elements, ["text", "each", "switch"]);
    assert!(left[0].why.ends_with("→ false"), "{}", left[0].why);
    assert!(
        left[1].why.ends_with("→ list of 0 items"),
        "{}",
        left[1].why
    );
    assert!(left[2].why.ends_with("→ 3"), "{}", left[2].why);
}

#[test]
fn an_action_is_listed_with_its_arguments() {
    let pkg = skin(
        "button \"Go\" id=\"go\" action=\"nav.go\" args=\"game {$id}\"",
        &[],
    );
    let bound = bind(&pkg, None, true);
    let action = origin(&bound, "go").action.as_ref().unwrap();
    assert_eq!(action.name, "nav.go");
    assert!(
        action.args.iter().any(|(_, v)| v.contains(GAME_ID)),
        "{:?}",
        action.args
    );
}

#[test]
fn long_text_is_cut_and_lists_and_records_show_no_content() {
    let long = "x".repeat(400);
    let pkg = skin(
        &format!("let all=\"{{game($id)}}\"\n    text \"{{'{long}'}}\" id=\"long\""),
        &[],
    );
    let bound = bind(&pkg, None, true);
    let o = origin(&bound, "long");
    let (source, value) = binding(o, "value");
    for text in [source, value] {
        assert!(text.chars().count() <= gogo_skin::MAX_SHOWN + 3, "{text}");
        assert!(text.contains('…'), "{text}");
    }
    let all = &o.locals.iter().find(|(n, _)| n == "all").unwrap().1;
    assert!(all.contains("record {"), "{all}");
}

#[test]
fn a_fallback_route_is_marked_as_the_default_skins() {
    let default = skin("text \"x\" id=\"d\"", &[]);
    let mut files = gogo_skins::default().to_files();
    files.retain(|(p, _)| p != "routes/game.kdl");
    for (path, data) in &mut files {
        if path == "skin.kdl" {
            let text = String::from_utf8(std::mem::take(data)).unwrap();
            *data = text
                .replacen("skin \"default\"", "skin \"partial\"", 1)
                .replacen("\"shell\" \"game\" ", "\"shell\" ", 1)
                .into_bytes();
        }
    }
    let partial = SkinPackage::from_files("partial", files).unwrap_or_else(|r| panic!("{r}"));
    let bound = bind(&partial, Some(&default), true);
    assert!(origin(&bound, "d").default_skin);
}

#[test]
fn a_traced_bind_draws_the_same_tree() {
    let pkg = skin("", &[]);
    let plain = bind(&pkg, None, false);
    let traced = bind(&pkg, None, true);
    assert!(plain.provenance.is_none());
    assert!(traced.provenance.is_some());
    let nodes = |tree: &Tree| -> Vec<String> {
        tree.iter()
            .map(|id| format!("{:?} {:?}", tree.key(id), tree.get(id)))
            .collect()
    };
    assert_eq!(nodes(&plain.tree), nodes(&traced.tree));
}

#[test]
fn a_tables_rows_and_cells_say_what_they_show() {
    let pkg = skin(
        "data-table id=\"probe\" label=\"Probe\" rows=\"{game.builds($id)}\" highlight=\"{row.this_machine}\" {\n        col \"Build\" \"{row.version}\"\n    }",
        &[],
    );
    let bound = bind(&pkg, None, true);
    let provenance = bound.provenance.as_ref().unwrap();
    let table = named(&bound.tree, "probe")[0];
    // The header row comes first.
    let row = bound.tree.children(table)[1];
    let cell = bound.tree.children(row)[0];
    let row = provenance.get(row).unwrap();
    assert_eq!(row.element, "data-table");
    assert!(binding(row, "highlight").0.contains("this_machine"));
    let cell = provenance.get(cell).unwrap();
    assert_eq!(cell.element, "col");
    assert_eq!(binding(cell, "value").0, "\"{row.version}\"");
    assert!(cell.locals.iter().any(|(n, _)| n == "row"));
}

#[test]
fn a_masked_value_is_recorded_as_its_mask() {
    // S1, S2: in an expression's value and in a name in scope.
    let mask = Value::Masked.display();
    let pkg = skin(
        "each e in=\"{game.env($id)}\" {\n        let secret=\"{e.value}\"\n        text \"{secret}\" id=\"probe\"\n    }",
        &[],
    );
    let bound = bind_on(&secret_scope(), &pkg, None, true).unwrap();
    let o = origin(&bound, "probe");
    assert_eq!(binding(o, "value"), ("\"{secret}\"", mask.as_str()));
    let local = o.locals.iter().find(|(n, _)| n == "secret").unwrap();
    assert_eq!(local.1, mask);

    // As an action argument it fails the bind, traced or not, so no trace can hold one.
    let pkg = skin(
        "each e in=\"{game.env($id)}\" { button \"Copy\" action=\"game.open_folder\" args=\"{$id} {true ? e.value : 'game'}\" }",
        &[],
    );
    let err = bind_on(&secret_scope(), &pkg, None, true).unwrap_err();
    assert!(matches!(err.kind(), BindErrorKind::Masked(_)), "{err}");
}

#[test]
fn names_in_scope_and_left_out_children_are_capped() {
    let lets: String = (0..40).map(|i| format!("let n{i}={i}\n        ")).collect();
    let hidden: String = (0..40)
        .map(|_| "text \"no\" when=\"{1 == 2}\"\n        ")
        .collect();
    let pkg = skin(
        &format!(
            "column id=\"holder\" {{\n        {lets}text \"x\" id=\"probe\"\n        {hidden}}}"
        ),
        &[],
    );
    let bound = bind(&pkg, None, true);
    let locals = &origin(&bound, "probe").locals;
    assert_eq!(locals.len(), 32);
    // The nearest are kept.
    assert_eq!(locals.last().unwrap().0, "n39");
    let holder = named(&bound.tree, "holder")[0];
    let left = bound.provenance.as_ref().unwrap().left_out(holder);
    assert_eq!(left.len(), 32);
}

#[test]
fn only_so_many_nodes_are_recorded() {
    let mut scope = game_scope();
    let row = [
        ("name".to_owned(), Value::str("A")),
        ("value".to_owned(), Value::Masked),
    ];
    let row = Value::Map(row.into_iter().collect());
    scope.set_call("game.env", GAME_ID, Value::List(vec![row; 160]));
    // 160 × 160 texts: more than are recorded.
    let pkg = skin(
        "column id=\"many\" { each a in=\"{game.env($id)}\" { each b in=\"{game.env($id)}\" { text \"x\" } } }",
        &[],
    );
    let bound = bind_on(&scope, &pkg, None, true).unwrap();
    let many = named(&bound.tree, "many")[0];
    assert_eq!(bound.tree.children(many).len(), 160 * 160);
    let provenance = bound.provenance.as_ref().unwrap();
    let traced = bound
        .tree
        .iter()
        .filter(|&id| provenance.get(id).is_some())
        .count();
    assert_eq!(traced, MAX_TRACED);
}
