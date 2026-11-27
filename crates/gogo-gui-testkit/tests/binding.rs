//! Binding (design 24 §2, design 27 §9 step 4, design 26 §8): the skin is the built-in Default
//! skin (`gogo_skins`) edited in memory, the data is the fixture scope.

use std::collections::BTreeMap;

use gogo_gui_testkit::{FixtureScope, GAME_ID};
use gogo_skin::expr::Scope;
use gogo_skin::ir::{DrawnBy, Fill, Kind, NodeId, Tree};
use gogo_skin::{
    ArgValue, BindErrorKind, Binding, Bound, Code, DefaultSkin, FallbackWhy, Input, Mode,
    PendingError, SkinPackage, UiState, Value,
};

type Files = Vec<(String, Vec<u8>)>;

fn base() -> Files {
    gogo_skins::default().to_files()
}

fn rep(f: &mut Files, path: &str, from: &str, to: &str) {
    let (_, data) = f
        .iter_mut()
        .find(|(p, _)| p == path)
        .unwrap_or_else(|| panic!("no file {path}"));
    let text = String::from_utf8(std::mem::take(data)).unwrap();
    assert!(text.contains(from), "`{from}` is not in {path}");
    *data = text.replacen(from, to, 1).into_bytes();
}

fn append(f: &mut Files, path: &str, extra: &str) {
    let (_, data) = f.iter_mut().find(|(p, _)| p == path).unwrap();
    data.extend(extra.as_bytes());
}

fn rm(f: &mut Files, path: &str) {
    let n = f.len();
    f.retain(|(p, _)| p != path);
    assert!(f.len() < n, "no file {path}");
}

const GAME: &str = "routes/game.kdl";

/// A node added to the game route, ahead of its tabs.
fn game_insert(f: &mut Files, node: &str) {
    rep(f, GAME, "    tabs ", &format!("    {node}\n    tabs "));
}

/// A skin that loads with every route.
fn skin(name: &str, f: Files) -> SkinPackage {
    let pkg = skin_loose(name, f);
    assert!(
        pkg.report().findings().iter().all(|f| !f.is_error()),
        "{}",
        pkg.report()
    );
    pkg
}

/// A skin that may have lost routes at load.
fn skin_loose(name: &str, mut f: Files) -> SkinPackage {
    if name != "default" {
        rep(
            &mut f,
            "skin.kdl",
            "skin \"default\"",
            &format!("skin \"{name}\""),
        );
    }
    match SkinPackage::from_files(name, f) {
        Ok(p) => p,
        Err(r) => panic!("{r}"),
    }
}

fn default_skin() -> SkinPackage {
    skin("default", base())
}

fn bind_in(
    pkg: &SkinPackage,
    route: &str,
    scope: &FixtureScope,
    ui: &UiState,
) -> Result<Bound, gogo_skin::BindError> {
    pkg.bind(
        route,
        &Binding {
            scope,
            ui,
            mode: Mode::Dark,
            default: None,
        },
    )
}

fn bind(pkg: &SkinPackage, route: &str, scope: &FixtureScope) -> Bound {
    let ui = UiState::defaults(&pkg.manifest);
    match bind_in(pkg, route, scope, &ui) {
        Ok(b) => b,
        Err(e) => panic!("{e}"),
    }
}

fn keys(t: &Tree) -> Vec<String> {
    t.iter()
        .map(|id| t.key(id).unwrap_or_default().to_owned())
        .collect()
}

/// The text of every text node, in document order.
fn texts(t: &Tree) -> Vec<String> {
    t.iter()
        .filter_map(|id| match &t.get(id)?.kind {
            Kind::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn call(scope: &FixtureScope, name: &str) -> Value {
    scope.call(name, &[&Value::str(GAME_ID)]).unwrap().unwrap()
}

fn builds(n: usize) -> Value {
    Value::List(
        (0..n)
            .map(|i| {
                let mut m = BTreeMap::new();
                m.insert("version".to_owned(), Value::str(format!("v{i}")));
                m.insert("date".to_owned(), Value::Num(1.0e9));
                m.insert("size".to_owned(), Value::Num(1.0e9));
                m.insert("generation".to_owned(), Value::Num(2.0));
                m.insert("legacy".to_owned(), Value::Bool(false));
                m.insert("newest".to_owned(), Value::Bool(false));
                m.insert("this_machine".to_owned(), Value::Bool(false));
                Value::Map(m)
            })
            .collect(),
    )
}

// ---- the tree ----

#[test]
fn keys_are_unique_and_the_same_for_the_same_skin_and_other_data() {
    let pkg = default_skin();
    let a = bind(&pkg, "shell", &FixtureScope::new());
    let mut scope = FixtureScope::new();
    let Value::Map(mut g) = call(&scope, "game") else {
        panic!("not a record")
    };
    g.insert("title".to_owned(), Value::str("Another Title"));
    scope.set_call("game", GAME_ID, Value::Map(g));
    let b = bind(&pkg, "shell", &scope);
    assert_eq!(keys(&a.tree), keys(&b.tree));
    let mut seen = std::collections::BTreeSet::new();
    assert!(keys(&a.tree).into_iter().all(|k| seen.insert(k)));
    // The data did change what is drawn.
    assert_ne!(texts(&a.tree), texts(&b.tree));
    assert!(texts(&b.tree).contains(&"Another Title".to_owned()));
    // `each` and a component are in the keys.
    assert!(keys(&a.tree).iter().any(|k| k.contains("/each.")));
    assert!(keys(&a.tree).iter().any(|k| k.contains("/use.")));
}

#[test]
fn the_focus_order_is_the_document_order_of_the_actions() {
    // S13: every node with an action is focusable, in document order.
    let pkg = default_skin();
    let b = bind(&pkg, "shell", &FixtureScope::new());
    let actions = b.tree.action_nodes();
    assert!(actions.len() > 10);
    // Every action is in the focus order, in document order; a menu button is focusable
    // without an action of its own.
    let focus: Vec<NodeId> = b
        .tree
        .focus_order()
        .into_iter()
        .filter(|id| b.tree.get(*id).is_some_and(|n| n.action.is_some()))
        .collect();
    assert_eq!(actions, focus);
    for id in actions {
        let n = b.tree.get(id).unwrap();
        assert!(n.a11y.focusable);
        // S14: and each has a name.
        assert!(n.a11y.name.as_deref().is_some_and(|s| !s.trim().is_empty()));
        let r = n.action.unwrap();
        assert!(b.actions.get(r).is_some());
    }
}

#[test]
fn styles_states_and_tokens_reach_the_nodes() {
    let pkg = default_skin();
    let b = bind(&pkg, "game", &FixtureScope::new());
    let t = &b.tree;
    // The page style, then the primary style over the button style, with the token's
    // value in the active mode.
    let page = t.get(t.root()).unwrap();
    assert!(matches!(&page.paint.fill, Some(Fill::Solid(c)) if c.0[3] == 255));
    let play = t.get(t.by_name("play").unwrap()).unwrap();
    assert!(matches!(&play.paint.fill, Some(Fill::Solid(c)) if c.0 == [122, 162, 255, 255]));
    // `:focus` is the focus paint, and only where the style has one.
    assert!(play.focus_paint.is_some());
    // `upper` is applied to the text a card title, a label and a text element show.
    assert!(texts(t).contains(&"BUILDS".to_owned()), "{:?}", texts(t));
    assert!(texts(t).contains(&"RUNS WITH".to_owned()));
    // The tone picks the `chip.warn` style variant, which sets the colour.
    let warn = t
        .iter()
        .find_map(|id| match &t.get(id)?.kind {
            Kind::Text { text, style } if text == "Update 1.1.0 available" => Some(style.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(warn.color.0, [232, 185, 90, 255]);
}

#[test]
fn an_unknown_mode_is_an_error() {
    let pkg = default_skin();
    let scope = FixtureScope::new();
    let ui = UiState::defaults(&pkg.manifest);
    let err = pkg
        .bind(
            "game",
            &Binding {
                scope: &scope,
                ui: &ui,
                mode: Mode::Light,
                default: None,
            },
        )
        .unwrap_err();
    assert!(matches!(err.kind(), BindErrorKind::NoMode(Mode::Light)));
}

// ---- control flow ----

#[test]
fn when_each_and_switch() {
    let mut f = base();
    game_insert(
        &mut f,
        "text \"shown\" when=\"{g.title == 'Example Quest'}\" id=\"w-yes\"\n    text \"hidden\" when=\"{g.runs_via == linux}\" id=\"w-no\"\n    each \"b\" in=\"{game.builds($id) | take 3}\" { text \"{b.version}\" id=\"item\" }\n    switch \"{ui.view}\" { case \"grid\" { text \"grid\" id=\"sw-grid\" }\n    case \"list\" { text \"list\" id=\"sw-list\" }\n    else { text \"else\" id=\"sw-else\" } }",
    );
    let pkg = skin("default", f);
    let scope = FixtureScope::new();
    let mut ui = UiState::defaults(&pkg.manifest);
    let b = bind_in(&pkg, "game", &scope, &ui).unwrap();
    assert!(b.tree.by_name("w-yes").is_some());
    assert!(b.tree.by_name("w-no").is_none());
    let items: Vec<String> = b
        .tree
        .iter()
        .filter(|&id| b.tree.get(id).unwrap().name.as_deref() == Some("item"))
        .map(|id| b.tree.key(id).unwrap().to_owned())
        .collect();
    assert_eq!(items.len(), 3);
    assert!(items[2].contains("#2"), "{items:?}");
    assert!(b.tree.by_name("sw-grid").is_some());
    assert!(b.tree.by_name("sw-list").is_none());

    ui.set(&pkg.manifest, "view", Value::str("list")).unwrap();
    let b = bind_in(&pkg, "game", &scope, &ui).unwrap();
    assert!(b.tree.by_name("sw-grid").is_none());
    assert!(b.tree.by_name("sw-list").is_some());
}

#[test]
fn a_switch_without_a_match_or_an_else_draws_nothing() {
    let mut f = base();
    game_insert(
        &mut f,
        "switch \"{ui.view}\" { case \"nothing\" { text \"x\" id=\"sw-x\" } }",
    );
    let pkg = skin("default", f);
    let b = bind(&pkg, "game", &FixtureScope::new());
    assert!(b.tree.by_name("sw-x").is_none());
}

#[test]
fn a_scroll_element_makes_a_scroll_container() {
    let mut f = base();
    game_insert(
        &mut f,
        "scroll id=\"long\" label=\"Notes\" { text \"one\" }",
    );
    rep(
        &mut f,
        "styles.kdl",
        "style \"scrollbar\" { w 8 }",
        "style \"scrollbar\" { w 12; fill \"#101010\" }",
    );
    rep(
        &mut f,
        "styles.kdl",
        "fill @outline; radius 4 }",
        "fill \"#202020\"; radius 3 }",
    );
    let pkg = skin("default", f);
    let b = bind(&pkg, "game", &FixtureScope::new());
    let node = b
        .tree
        .iter()
        .filter_map(|id| b.tree.get(id))
        .find(|n| n.name.as_deref() == Some("long"))
        .expect("the scroll node");
    let spec = node.scroll.as_ref().expect("a spec");
    assert_eq!((spec.width, spec.radius), (12.0, 3.0));
    assert_eq!(
        spec.track,
        Some(Fill::Solid(gogo_skin::ir::Color([16, 16, 16, 255])))
    );
    assert_eq!(
        spec.thumb,
        Fill::Solid(gogo_skin::ir::Color([32, 32, 32, 255]))
    );
    assert_eq!(node.a11y.name.as_deref(), Some("Notes"));
    // Inside the outlet, or holding it: its own offset per route. This one is bound
    // with no shell around it.
    let containers: Vec<_> = b
        .tree
        .iter()
        .filter_map(|id| b.tree.get(id)?.scroll.as_ref().map(|s| s.per_route))
        .collect();
    assert_eq!(containers, [false]);
    let shell = bind(&pkg, "shell", &FixtureScope::new());
    let containers: Vec<_> = shell
        .tree
        .iter()
        .filter_map(|id| {
            let node = shell.tree.get(id)?;
            Some((node.name.clone()?, node.scroll.as_ref()?.per_route))
        })
        .collect();
    let containers: Vec<_> = containers.iter().map(|(n, p)| (&**n, *p)).collect();
    assert_eq!(containers, [("nav", false), ("main", true), ("long", true)]);
    b.tree.validate().expect("a valid tree");
}

#[test]
fn a_list_is_cut_at_a_thousand_silently() {
    let mut f = base();
    game_insert(
        &mut f,
        "each \"b\" in=\"{game.builds($id)}\" { text \"{b.version}\" id=\"item\" }",
    );
    rep(
        &mut f,
        GAME,
        "rows=\"{game.builds($id) | take 4}\"",
        "rows=\"{game.builds($id)}\"",
    );
    let pkg = skin("default", f);
    let mut scope = FixtureScope::new();
    scope.set_call("game.builds", GAME_ID, builds(1500));
    let b = bind(&pkg, "game", &scope);
    let count = |name: &str| {
        b.tree
            .iter()
            .filter(|&id| b.tree.get(id).unwrap().name.as_deref() == Some(name))
            .count()
    };
    assert_eq!(count("item"), 1000);
    // One row node for each of a thousand rows in the table.
    let rows = keys(&b.tree)
        .iter()
        .filter(|k| k.rsplit('/').next().is_some_and(|l| l.starts_with("row#")))
        .count();
    assert_eq!(rows, 1000);
}

#[test]
fn a_route_that_binds_to_too_many_nodes_fails() {
    // Two repeats of a thousand: data can't make a valid skin fail to load, but it can
    // make a route too big to bind.
    let mut f = base();
    game_insert(
        &mut f,
        "each \"a\" in=\"{game.builds($id)}\" { each \"b\" in=\"{game.builds($id)}\" { text \"{b.version}\" } }",
    );
    let pkg = skin("default", f);
    let mut scope = FixtureScope::new();
    scope.set_call("game.builds", GAME_ID, builds(1500));
    let ui = UiState::defaults(&pkg.manifest);
    let err = bind_in(&pkg, "game", &scope, &ui).unwrap_err();
    assert!(
        matches!(err.kind(), BindErrorKind::TooManyNodes(_)),
        "{err}"
    );
}

#[test]
fn each_over_a_value_that_is_not_a_list_fails_and_null_is_empty() {
    // The loader can't tell what `first` gives; the data decides.
    let mut f = base();
    game_insert(&mut f, "each \"b\" in=\"{$id}\" { text \"x\" id=\"item\" }");
    let pkg = skin("default", f);
    let mut scope = FixtureScope::new();
    let ui = UiState::defaults(&pkg.manifest);
    let err = bind_in(&pkg, "game", &scope, &ui).unwrap_err();
    assert!(
        matches!(err.kind(), BindErrorKind::NotAList { .. }),
        "{err}"
    );

    let mut f = base();
    game_insert(
        &mut f,
        "each \"b\" in=\"{g.runs_via | first}\" { text \"x\" id=\"item\" }",
    );
    let pkg = skin("default", f);
    // Text is not a list, so `first` fails; null passes through, and null is empty.
    assert!(bind_in(&pkg, "game", &scope, &ui).is_err());
    let Value::Map(mut g) = call(&scope, "game") else {
        panic!("not a record")
    };
    g.insert("runs_via".to_owned(), Value::Null);
    scope.set_call("game", GAME_ID, Value::Map(g));
    let b = bind_in(&pkg, "game", &scope, &ui).unwrap();
    assert!(b.tree.by_name("item").is_none());
}

// ---- actions ----

#[test]
fn actions_are_prepared_with_their_arguments() {
    let pkg = default_skin();
    let b = bind(&pkg, "game", &FixtureScope::new());
    let find = |name: &str| {
        b.actions
            .iter()
            .filter(move |(_, c)| c.spec.name == name)
            .map(|(_, c)| c)
            .collect::<Vec<_>>()
    };
    // The stock tile forwards `dlc.add` with `{$id} all`: both terms arrive.
    let add = find("dlc.add");
    assert_eq!(add.len(), 1);
    let vals: Vec<(&str, String)> = add[0]
        .args
        .iter()
        .map(|a| match &a.value {
            ArgValue::Value(v) => (a.param.name, v.display()),
            ArgValue::OnInput(_) => panic!("known now"),
        })
        .collect();
    assert_eq!(vals, [("id", "1".to_owned()), ("dlc", "all".to_owned())]);
    assert!(add[0].location.starts_with("components/") || add[0].location.starts_with("routes/"));

    // `nav.go game {$id} files`: the route, then its parameters by name.
    let nav = find("nav.go");
    let files = nav
        .iter()
        .find(|c| {
            c.args
                .iter()
                .any(|a| a.route_param.is_some_and(|p| p.name == "section"))
        })
        .expect("the files button");
    let shown: Vec<(Option<&str>, String)> = files
        .args
        .iter()
        .map(|a| match &a.value {
            ArgValue::Value(v) => (a.route_param.map(|p| p.name), v.display()),
            ArgValue::OnInput(_) => panic!("known now"),
        })
        .collect();
    assert_eq!(
        shown,
        [
            (None, "game".to_owned()),
            (Some("id"), "1".to_owned()),
            (Some("section"), "files".to_owned())
        ]
    );
}

#[test]
fn a_component_passes_its_args_on_through_two_levels() {
    let mut f = base();
    append(
        &mut f,
        "components/stock.kdl",
        "\ncomponent \"outer\" params=\"action args\" {\n    use \"summary-tile\" mark=\"check\" title=\"T\" link=\"L\" action=\"{action}\" args=\"{args}\"\n}\n",
    );
    game_insert(
        &mut f,
        "use \"outer\" action=\"dlc.add\" args=\"dlc=all id={$id}\"",
    );
    let pkg = skin("default", f);
    let b = bind(&pkg, "game", &FixtureScope::new());
    let adds: Vec<_> = b
        .actions
        .iter()
        .filter(|(_, c)| c.spec.name == "dlc.add")
        .map(|(_, c)| c)
        .collect();
    assert_eq!(adds.len(), 2);
    for c in adds {
        let got: Vec<(&str, String)> = c
            .args
            .iter()
            .map(|a| match &a.value {
                ArgValue::Value(v) => (a.param.name, v.display()),
                ArgValue::OnInput(_) => panic!("known now"),
            })
            .collect();
        assert_eq!(got, [("id", "1".to_owned()), ("dlc", "all".to_owned())]);
    }
}

#[test]
fn an_argument_that_reads_the_input_waits_for_it() {
    let pkg = default_skin();
    let scope = FixtureScope::new();
    let b = bind(&pkg, "game", &scope);
    let call = |name: &str| {
        b.actions
            .iter()
            .find(|(_, c)| c.spec.name == name)
            .map(|(_, c)| c)
            .unwrap_or_else(|| panic!("no {name}"))
    };
    // A toggle: `{not on}` is the state after the input.
    let toggle = call("snapshot.set_auto");
    let ArgValue::OnInput(p) = &toggle.args[1].value else {
        panic!("waits for the input")
    };
    let input = |on| Input {
        on: Some(on),
        value: None,
    };
    assert_eq!(p.eval(&scope, &input(true)).unwrap(), Value::Bool(false));
    assert_eq!(p.eval(&scope, &input(false)).unwrap(), Value::Bool(true));
    assert!(matches!(&toggle.args[0].value, ArgValue::Value(v) if v.display() == "1"));
    // A select: `{value}` is the option chosen.
    let select = call("runner.set_renderer");
    let ArgValue::OnInput(p) = &select.args[1].value else {
        panic!("waits for the input")
    };
    let chosen = Input {
        on: None,
        value: Some(Value::str("wined3d")),
    };
    assert_eq!(p.eval(&scope, &chosen).unwrap(), Value::str("wined3d"));
}

#[test]
fn a_masked_value_never_becomes_an_argument() {
    // S1, S2: the action would get a secret.
    let mut f = base();
    game_insert(
        &mut f,
        "each \"e\" in=\"{game.env($id)}\" { button \"Copy\" action=\"game.open_folder\" args=\"{$id} {true ? e.value : 'game'}\" }",
    );
    let pkg = skin("default", f);
    let mut scope = FixtureScope::new();
    let mut row = BTreeMap::new();
    row.insert("name".to_owned(), Value::str("TOKEN"));
    row.insert("value".to_owned(), Value::Masked);
    scope.set_call("game.env", GAME_ID, Value::List(vec![Value::Map(row)]));
    let ui = UiState::defaults(&pkg.manifest);
    let err = bind_in(&pkg, "game", &scope, &ui).unwrap_err();
    assert!(matches!(err.kind(), BindErrorKind::Masked(_)), "{err}");

    // Shown as text, it reads as the mask and nothing else.
    let mut f = base();
    game_insert(
        &mut f,
        "each \"e\" in=\"{game.env($id)}\" { text \"{e.name}: {e.value}\" id=\"env\" }",
    );
    let pkg = skin("default", f);
    let b = bind_in(&pkg, "game", &scope, &ui).unwrap();
    let id = b.tree.by_name("env").unwrap();
    let Kind::Text { text, .. } = &b.tree.get(id).unwrap().kind else {
        panic!("not text")
    };
    assert_eq!(
        text,
        &format!("TOKEN: {}", Value::Masked.display()),
        "the masked value is its mask"
    );
}

#[test]
fn a_control_whose_name_comes_out_empty_fails_the_route() {
    // S14: the loader accepts a text that is a template; the data decides.
    let mut f = base();
    game_insert(
        &mut f,
        "button \"{g.runs_via}\" action=\"game.verify\" args=\"{$id}\"",
    );
    let pkg = skin("default", f);
    let mut scope = FixtureScope::new();
    let ui = UiState::defaults(&pkg.manifest);
    assert!(bind_in(&pkg, "game", &scope, &ui).is_ok());
    let Value::Map(mut g) = call(&scope, "game") else {
        panic!("not a record")
    };
    g.insert("runs_via".to_owned(), Value::Null);
    scope.set_call("game", GAME_ID, Value::Map(g));
    let err = bind_in(&pkg, "game", &scope, &ui).unwrap_err();
    assert!(matches!(err.kind(), BindErrorKind::NoName(_)), "{err}");
}

#[test]
fn progress_outside_zero_to_one_fails_the_route() {
    let pkg = default_skin();
    let mut scope = FixtureScope::new();
    let ui = UiState::defaults(&pkg.manifest);
    // The shell's transfer strip shows `q.current.fraction`.
    let Value::Map(mut q) = scope.var("queue.summary").cloned().unwrap() else {
        panic!("not a record")
    };
    let Some(Value::Map(mut cur)) = q.remove("current") else {
        panic!("no current job")
    };
    cur.insert("fraction".to_owned(), Value::Num(1.5));
    q.insert("current".to_owned(), Value::Map(cur));
    scope.set_var("queue.summary", Value::Map(q));
    let err = bind_in(&pkg, "shell", &scope, &ui).unwrap_err();
    assert!(matches!(err.kind(), BindErrorKind::Progress(_)), "{err}");
}

// ---- app widgets ----

#[test]
fn an_app_widget_is_a_slot_for_the_app_with_the_skins_choices() {
    let pkg = default_skin();
    let b = bind(&pkg, "game", &FixtureScope::new());
    let plan = b
        .widgets
        .iter()
        .find(|w| w.widget.name == "plan-card")
        .unwrap();
    let node = b.tree.get(plan.node).unwrap();
    assert!(matches!(node.drawn_by, DrawnBy::App(p) if p.part.is_none()));
    assert_eq!(node.name.as_deref(), Some("plan"));
    assert!(
        plan.variants
            .iter()
            .any(|(v, x)| v.name == "layout" && *x == "stacked")
    );
    assert!(
        plan.variants
            .iter()
            .any(|(v, x)| v.name == "density" && *x == "full")
    );
    assert_eq!(format!("{:?}", plan.placement), "Some(Inline)");
    let action = b.actions.get(plan.action.unwrap()).unwrap();
    assert_eq!(action.spec.name, "game.update");
    let parts: Vec<&str> = plan.parts.iter().map(|p| p.part.name).collect();
    assert_eq!(
        parts,
        ["frame", "title", "badge", "step", "confirm", "cancel"]
    );
    let frame = &plan.parts[0];
    assert!(frame.set.contains(&"fill".to_owned()));
    assert!(!frame.hidden);
    let env = b
        .widgets
        .iter()
        .find(|w| w.widget.name == "env-table")
        .unwrap();
    assert!(
        env.props
            .iter()
            .any(|(k, v)| k == "game" && v.display() == "1")
    );
}

#[test]
fn a_part_that_cannot_be_hidden_is_not_drawn_hidden() {
    // S5: the loader refuses the skin; the binder refuses a tree that got past it.
    let mut pkg = default_skin();
    let game = pkg.routes.get_mut("game").unwrap();
    fn walk(nodes: &mut [gogo_skin::Node]) -> bool {
        for n in nodes {
            if n.name == "part" && n.plain_arg(0) == Some("confirm") {
                let mut p = n.props[0].clone();
                p.name = "hidden".to_owned();
                p.arg.val = gogo_skin::Val::Bool(true);
                n.props.push(p);
                return true;
            }
            if walk(&mut n.children) {
                return true;
            }
        }
        false
    }
    assert!(walk(&mut game.children));
    let scope = FixtureScope::new();
    let ui = UiState::defaults(&pkg.manifest);
    let err = bind_in(&pkg, "game", &scope, &ui).unwrap_err();
    assert!(
        matches!(err.kind(), BindErrorKind::HiddenPart { .. }),
        "{err}"
    );
}

// ---- fallback and borrowing ----

fn with_default<'a>(
    other: &'a SkinPackage,
    default: &'a SkinPackage,
    scope: &'a FixtureScope,
    ui: &'a UiState,
    dui: &'a UiState,
) -> (&'a SkinPackage, Binding<'a>) {
    let _ = other;
    (
        other,
        Binding {
            scope,
            ui,
            mode: Mode::Dark,
            default: Some(DefaultSkin {
                skin: default,
                ui: dui,
            }),
        },
    )
}

#[test]
fn a_route_that_failed_to_load_is_the_default_skins_with_the_active_tokens() {
    let default = default_skin();
    let mut f = base();
    rep(&mut f, GAME, "icon=\"play\"", "icon=\"plai\"");
    rep(
        &mut f,
        "tokens.kdl",
        "accent \"#7aa2ff\"",
        "accent \"#ff0000\"",
    );
    let other = skin_loose("other", f);
    assert!(!other.routes.contains_key("game"), "the route did not load");
    let scope = FixtureScope::new();
    let (ui, dui) = (
        UiState::defaults(&other.manifest),
        UiState::defaults(&default.manifest),
    );
    let (other, b) = with_default(&other, &default, &scope, &ui, &dui);

    // Without fallback the route is missing.
    let err = other.bind("shell", &b).unwrap_err();
    assert_eq!(err.route(), "shell");
    assert!(matches!(err.kind(), BindErrorKind::Missing(_)));

    let bound = other.bind_with_fallback("shell", &b).unwrap();
    assert_eq!(bound.fallbacks.len(), 1);
    assert_eq!(bound.fallbacks[0].route, "game");
    assert!(matches!(bound.fallbacks[0].why, FallbackWhy::FailedToLoad));
    // The page is the shell of the active skin around the default skin's game route, and
    // the button is drawn with the active skin's accent.
    let play = bound.tree.get(bound.tree.by_name("play").unwrap()).unwrap();
    assert!(matches!(&play.paint.fill, Some(Fill::Solid(c)) if c.0 == [255, 0, 0, 255]));
}

#[test]
fn a_route_the_skin_does_not_cover_is_the_default_skins() {
    let default = default_skin();
    let mut f = base();
    rm(&mut f, GAME);
    rep(
        &mut f,
        "skin.kdl",
        "routes \"shell\" \"game\"",
        "routes \"shell\"",
    );
    let other = skin("other", f);
    let scope = FixtureScope::new();
    let (ui, dui) = (
        UiState::defaults(&other.manifest),
        UiState::defaults(&default.manifest),
    );
    let (other, b) = with_default(&other, &default, &scope, &ui, &dui);
    let bound = other.bind_with_fallback("shell", &b).unwrap();
    assert!(matches!(bound.fallbacks[0].why, FallbackWhy::NotCovered));
    assert!(bound.tree.by_name("play").is_some());
    // Asked for by name, the route is the default skin's whole.
    let bound = other.bind_with_fallback("game", &b).unwrap();
    assert_eq!(bound.fallbacks.len(), 1);
    assert!(bound.tree.by_name("play").is_some());
}

#[test]
fn a_route_whose_binding_fails_falls_back_and_says_where() {
    let default = default_skin();
    let mut f = base();
    game_insert(&mut f, "text \"to {g.update.to}\" id=\"to\"");
    let other = skin("other", f.clone());
    let mut scope = FixtureScope::new();
    let (ui, dui) = (
        UiState::defaults(&other.manifest),
        UiState::defaults(&default.manifest),
    );
    // With an update the field is there.
    {
        let (other, b) = with_default(&other, &default, &scope, &ui, &dui);
        let bound = other.bind_with_fallback("shell", &b).unwrap();
        assert!(bound.fallbacks.is_empty());
        assert!(bound.tree.by_name("to").is_some());
    }
    // Without, `g.update.to` has no field of a null: the route fails.
    let Value::Map(mut g) = call(&scope, "game") else {
        panic!("not a record")
    };
    g.insert("update".to_owned(), Value::Null);
    scope.set_call("game", GAME_ID, Value::Map(g));
    let (other, b) = with_default(&other, &default, &scope, &ui, &dui);
    let strict = other.bind("shell", &b).unwrap_err();
    assert_eq!(strict.route(), "game");
    assert!(matches!(strict.kind(), BindErrorKind::Eval(_)), "{strict}");
    // The error is reported with file and line.
    let text = String::from_utf8(f.iter().find(|(p, _)| p == GAME).unwrap().1.clone()).unwrap();
    let line = text
        .lines()
        .position(|l| l.contains("{g.update.to}"))
        .unwrap()
        + 1;
    assert!(
        strict.location().starts_with(&format!("{GAME}:{line}:")),
        "{} (line {line})",
        strict.location()
    );

    let bound = other.bind_with_fallback("shell", &b).unwrap();
    assert_eq!(bound.fallbacks.len(), 1);
    assert_eq!(bound.fallbacks[0].route, "game");
    let FallbackWhy::Bind(e) = &bound.fallbacks[0].why else {
        panic!("a binding failure")
    };
    assert_eq!(e.location(), strict.location());
    assert!(bound.tree.by_name("to").is_none());
    assert!(bound.tree.by_name("play").is_some(), "the default's route");
    bound.tree.validate().unwrap();
}

#[test]
fn when_the_default_route_fails_too_the_page_fails() {
    let default = default_skin();
    let other = skin("other", base());
    // The game the route is on has no data: both skins' `game` routes fail.
    let mut scope = FixtureScope::new();
    scope.set_route("game", Some("404"));
    let (ui, dui) = (
        UiState::defaults(&other.manifest),
        UiState::defaults(&default.manifest),
    );
    let (other, b) = with_default(&other, &default, &scope, &ui, &dui);
    let err = other.bind_with_fallback("shell", &b).unwrap_err();
    assert_eq!(err.route(), "game");
    // And with no default skin there is nothing to fall back to.
    let no_default = Binding { default: None, ..b };
    assert!(other.bind_with_fallback("shell", &no_default).is_err());
}

fn borrower_and_default(
    default_extra: &str,
    other_manifest_ui: &str,
) -> (SkinPackage, SkinPackage) {
    let mut df = base();
    rep(
        &mut df,
        "skin.kdl",
        "var \"view\"",
        "var \"mode_x\" type=\"enum(a b)\" default=\"a\"\n        var \"view\"",
    );
    // The Default skin has its own `game.builds`: rename it so the test's fragment (or its
    // absence) is what the borrowing skin gets.
    rep(
        &mut df,
        "components/game.kdl",
        "component \"game.builds\"",
        "component \"game.builds-own\"",
    );
    rep(
        &mut df,
        GAME,
        "use \"game.builds\"",
        "use \"game.builds-own\"",
    );
    append(&mut df, "components/stock.kdl", default_extra);
    let default = skin("default", df);
    let mut of = base();
    rep(
        &mut of,
        GAME,
        "use \"game.builds\"",
        "use default \"game.builds\"",
    );
    if !other_manifest_ui.is_empty() {
        rep(
            &mut of,
            "skin.kdl",
            "var \"view\"",
            &format!("{other_manifest_ui}\n        var \"view\""),
        );
    }
    let other = skin("other", of);
    (default, other)
}

const FRAGMENT: &str = "\ncomponent \"game.builds\" {\n    text \"x is {ui.mode_x == b ? 'b' : 'not b'}\" id=\"frag\"\n}\n";

fn page_with_builds_tab(
    default: &SkinPackage,
    other: &SkinPackage,
    mode_x: Option<&str>,
) -> Result<Bound, gogo_skin::BindError> {
    let scope = FixtureScope::new();
    let mut ui = UiState::defaults(&other.manifest);
    ui.set(&other.manifest, "tab", Value::str("builds"))
        .unwrap();
    if let Some(v) = mode_x {
        ui.set(&other.manifest, "mode_x", Value::str(v)).unwrap();
    }
    // The default skin's own state has `mode_x` at `a`: it must not be what is read.
    let dui = UiState::defaults(&default.manifest);
    other.bind(
        "game",
        &Binding {
            scope: &scope,
            ui: &ui,
            mode: Mode::Dark,
            default: Some(DefaultSkin {
                skin: default,
                ui: &dui,
            }),
        },
    )
}

#[test]
fn a_borrowed_fragment_reads_the_borrowing_skins_ui_state() {
    let (default, other) =
        borrower_and_default(FRAGMENT, "var \"mode_x\" type=\"enum(a b)\" default=\"a\"");
    let b = page_with_builds_tab(&default, &other, Some("b")).unwrap();
    let id = b.tree.by_name("frag").unwrap();
    let Kind::Text { text, .. } = &b.tree.get(id).unwrap().kind else {
        panic!("not text")
    };
    assert_eq!(text, "x is b");
    let b = page_with_builds_tab(&default, &other, None).unwrap();
    assert!(texts(&b.tree).contains(&"x is not b".to_owned()));
}

#[test]
fn a_borrowed_fragments_names_resolve_against_the_borrowers_manifest() {
    // A `ui` variable the borrowing skin doesn't declare: `unknown_field`, as at load.
    let (default, other) = borrower_and_default(FRAGMENT, "");
    let err = page_with_builds_tab(&default, &other, None).unwrap_err();
    let BindErrorKind::Borrow(found) = err.kind() else {
        panic!("{err}")
    };
    assert!(
        found.iter().any(|f| f.code == Code::UnknownField),
        "{found:?}"
    );
    assert_eq!(err.route(), "game");
    assert!(err.location().starts_with(GAME), "{}", err.location());

    // A variable it declares with other words: the word `b` is no word of its enum, so
    // `unknown_name`.
    let (default, other) =
        borrower_and_default(FRAGMENT, "var \"mode_x\" type=\"enum(c d)\" default=\"c\"");
    let err = page_with_builds_tab(&default, &other, None).unwrap_err();
    let BindErrorKind::Borrow(found) = err.kind() else {
        panic!("{err}")
    };
    assert!(
        found.iter().any(|f| f.code == Code::UnknownName),
        "{found:?}"
    );

    // Through the page's fallback, a skin with the failing fragment keeps the default's
    // `game` route.
    let scope = FixtureScope::new();
    let mut ui = UiState::defaults(&other.manifest);
    ui.set(&other.manifest, "tab", Value::str("builds"))
        .unwrap();
    let dui = UiState::defaults(&default.manifest);
    let bound = other
        .bind_with_fallback(
            "shell",
            &Binding {
                scope: &scope,
                ui: &ui,
                mode: Mode::Dark,
                default: Some(DefaultSkin {
                    skin: &default,
                    ui: &dui,
                }),
            },
        )
        .unwrap();
    let FallbackWhy::Bind(e) = &bound.fallbacks[0].why else {
        panic!("a binding failure")
    };
    assert!(matches!(e.kind(), BindErrorKind::Borrow(_)));
}

#[test]
fn a_fragment_the_default_skin_lacks_or_no_default_skin_is_an_error() {
    let (default, other) = borrower_and_default("\ncomponent \"game.other\" { text \"x\" }\n", "");
    let err = page_with_builds_tab(&default, &other, None).unwrap_err();
    assert!(matches!(err.kind(), BindErrorKind::Missing(_)), "{err}");
    let scope = FixtureScope::new();
    let mut ui = UiState::defaults(&other.manifest);
    ui.set(&other.manifest, "tab", Value::str("builds"))
        .unwrap();
    let err = other
        .bind(
            "game",
            &Binding {
                scope: &scope,
                ui: &ui,
                mode: Mode::Dark,
                default: None,
            },
        )
        .unwrap_err();
    assert!(matches!(err.kind(), BindErrorKind::Missing(_)), "{err}");
}

// ---- the skin's state ----

#[test]
fn ui_state_takes_defaults_and_refuses_what_does_not_fit() {
    let pkg = default_skin();
    let m = &pkg.manifest;
    let mut s = UiState::defaults(m);
    assert_eq!(s.get("tab"), Some(&Value::str("overview")));
    assert_eq!(s.get("view"), Some(&Value::str("grid")));
    s.set(m, "view", Value::str("list")).unwrap();
    assert!(s.set(m, "view", Value::str("tiles")).is_err());
    assert!(s.set(m, "tab", Value::str("nothing")).is_err());
    assert!(s.set(m, "nope", Value::Bool(true)).is_err());
    assert_eq!(s.get("view"), Some(&Value::str("list")));
    // Saved state that no longer fits falls back to the declared default, per variable.
    let r = UiState::restore(
        m,
        [
            ("view".to_owned(), Value::str("list")),
            ("tab".to_owned(), Value::Num(3.0)),
            ("gone".to_owned(), Value::Bool(true)),
        ],
    );
    assert_eq!(r.get("view"), Some(&Value::str("list")));
    assert_eq!(r.get("tab"), Some(&Value::str("overview")));
    assert_eq!(r.get("gone"), None);
}

// ---- an argument read from the input gets the checks a known one gets ----

/// The first argument of a call to `action` that waits for the input.
fn pending<'b>(b: &'b Bound, action: &str) -> &'b gogo_skin::PendingArg {
    b.actions
        .iter()
        .filter(|(_, c)| c.spec.name == action)
        .flat_map(|(_, c)| c.args.iter())
        .find_map(|a| match &a.value {
            ArgValue::OnInput(p) => Some(p),
            ArgValue::Value(_) => None,
        })
        .unwrap_or_else(|| panic!("no pending argument of {action}"))
}

fn chosen(v: Value) -> Input {
    Input {
        on: None,
        value: Some(v),
    }
}

#[test]
fn a_name_the_skin_must_write_out_cannot_be_read_from_the_input() {
    // S21, S16: a settings key, a `ui` variable and a route are checked by name at load,
    // so `select … args="key={value}"` can't pick one from data.
    let cases = [
        (
            "settings.set key",
            "field \"F\" { select value=\"{ui.view}\" options=\"a b\" action=\"settings.set\" args=\"key={value} on\" }",
            Code::SettingKey,
        ),
        (
            "settings.set first term",
            "field \"F\" { select value=\"{ui.view}\" options=\"a b\" action=\"settings.set\" args=\"{value} on\" }",
            Code::SettingKey,
        ),
        (
            "ui.set variable",
            "field \"F\" { select value=\"{ui.view}\" options=\"a b\" action=\"ui.set\" args=\"{value} grid\" }",
            Code::TypeMismatch,
        ),
        (
            "nav.go route",
            "field \"F\" { select value=\"{ui.view}\" options=\"a b\" action=\"nav.go\" args=\"{value}\" }",
            Code::TypeMismatch,
        ),
    ];
    for (what, node, code) in cases {
        let mut f = base();
        game_insert(&mut f, node);
        let pkg = skin_loose("default", f);
        assert!(!pkg.routes.contains_key("game"), "{what}: the route loaded");
        assert!(
            pkg.report()
                .findings()
                .iter()
                .any(|f| f.code == code && f.is_error()),
            "{what}: {}",
            pkg.report()
        );
    }
}

#[test]
fn an_argument_read_from_the_input_is_checked_when_the_input_comes() {
    let mut f = base();
    game_insert(
        &mut f,
        "field \"V\" { select value=\"{ui.view}\" options=\"grid list\" action=\"ui.set\" args=\"view {value}\" }",
    );
    game_insert(
        &mut f,
        "field \"S\" { select value=\"{ui.view}\" options=\"a b\" action=\"nav.go\" args=\"game {$id} section={value}\" }",
    );
    let pkg = skin("default", f);
    let scope = FixtureScope::new();
    let b = bind(&pkg, "game", &scope);

    // A word of a fixed set (`runner.set_display`'s `mode`).
    let mode = pending(&b, "runner.set_display");
    assert_eq!(
        mode.eval(&scope, &chosen(Value::str("x11"))).unwrap(),
        Value::str("x11")
    );
    let err = mode
        .eval(&scope, &chosen(Value::str("fullscreen")))
        .unwrap_err();
    assert!(matches!(err, PendingError::Rejected(_)), "{err}");
    assert!(err.to_string().contains("fullscreen"), "{err}");

    // The value of a `ui` variable fits its type (`grid` or `list`).
    let view = pending(&b, "ui.set");
    assert!(view.eval(&scope, &chosen(Value::str("list"))).is_ok());
    for bad in [Value::str("tiles"), Value::Num(3.0), Value::Bool(true)] {
        let err = view.eval(&scope, &chosen(bad)).unwrap_err();
        assert!(matches!(err, PendingError::Rejected(_)), "{err}");
    }

    // A route's section is one of its sections.
    let section = b
        .actions
        .iter()
        .filter(|(_, c)| c.spec.name == "nav.go")
        .flat_map(|(_, c)| c.args.iter())
        .find_map(|a| match (&a.value, a.route_param) {
            (ArgValue::OnInput(p), Some(sp)) if sp.name == "section" => Some(p),
            _ => None,
        })
        .expect("a pending section");
    assert!(section.eval(&scope, &chosen(Value::str("builds"))).is_ok());
    let err = section
        .eval(&scope, &chosen(Value::str("nothing")))
        .unwrap_err();
    assert!(matches!(err, PendingError::Rejected(_)), "{err}");

    // S1, S2: a masked value never gets through, and neither does a null for a required
    // parameter.
    let err = mode.eval(&scope, &chosen(Value::Masked)).unwrap_err();
    assert!(matches!(err, PendingError::Masked(_)), "{err}");
    let err = mode.eval(&scope, &chosen(Value::Null)).unwrap_err();
    assert!(matches!(err, PendingError::Rejected(_)), "{err}");
}

#[test]
fn a_forwarded_action_is_the_one_the_use_named_through_three_levels() {
    let mut f = base();
    append(
        &mut f,
        "components/stock.kdl",
        "\ncomponent \"mid\" params=\"action args\" {\n    use \"summary-tile\" mark=\"check\" title=\"T\" link=\"L\" action=\"{action}\" args=\"{args}\"\n}\ncomponent \"top\" params=\"action args\" {\n    use \"mid\" action=\"{action}\" args=\"{args}\"\n}\n",
    );
    game_insert(&mut f, "use \"top\" action=\"game.verify\" args=\"{$id}\"");
    let pkg = skin("default", f);
    let b = bind(&pkg, "game", &FixtureScope::new());
    // The fixture's own verify button, and the one through three components.
    let verifies = b
        .actions
        .iter()
        .filter(|(_, c)| c.spec.name == "game.verify")
        .count();
    assert_eq!(verifies, 2);
}

// ---- app widgets (design 26 §5) ----

/// The `game` route with `style "plan-card" { <decls> }` added: a style named after the
/// element applies to every plan card, and an app widget takes no style property inline.
fn plan_slot(decls: &str) -> (Bound, usize) {
    let mut f = base();
    append(
        &mut f,
        "styles.kdl",
        &format!("\nstyle \"plan-card\" {{ {decls} }}\n"),
    );
    let pkg = skin("default", f);
    let b = bind(&pkg, "game", &FixtureScope::new());
    let ix = b
        .widgets
        .iter()
        .position(|w| w.widget.name == "plan-card")
        .expect("the plan card");
    (b, ix)
}

#[test]
fn a_skin_cannot_hide_move_or_obscure_an_app_widget() {
    use gogo_skin::ir::{Dim, Paint, Position, Sides};
    // Every one of these is a way to make the widget transparent, move it off screen,
    // shrink it away or draw over it.
    let (b, ix) = plan_slot(
        "opacity 0.05; inset 0 0 0 -5000; offset 9999 9999; w 0; h 0; min-w 0; max-w 0; \
         max-h 0; aspect 3; fill \"#00000000\"; border 4 \"#ffffff\"; radius 40; \
         ring 3 \"#ff0000\"; shape circle; blur-behind 30; scanlines 2 0.9; noise 0.9; \
         shadow 0 0 30 \"#000000\"; inner-shadow 0 0 30 \"#000000\"; glow 20 \"#ff0000\"; \
         reflection 45 0.9; padding 500; margin -900; direction row; justify end; gap 900",
    );
    let node = b.tree.get(b.widgets[ix].node).expect("the node");
    assert_eq!(node.paint, Paint::default(), "no paint reaches the slot");
    let l = &node.layout;
    assert_eq!(l.position, Position::Relative);
    assert_eq!(l.inset, Sides::all(Dim::Auto));
    assert_eq!(l.width, Dim::Auto);
    assert_eq!(l.height, Dim::Auto);
    assert_eq!(l.min_width, Dim::Auto);
    assert_eq!(l.max_width, Dim::Auto);
    assert_eq!(l.max_height, Dim::Auto);
    assert_eq!(l.aspect, None);
    assert_eq!(l.padding, Sides::all(0.0));
    assert_eq!(l.margin, Sides::all(0.0), "a negative margin is zero");
    assert_eq!(l.row_gap, 0.0);
    assert_eq!(l.column_gap, 0.0);
}

#[test]
fn the_parts_of_an_app_widget_are_what_a_skin_styles() {
    use gogo_skin::ir::Paint;
    let (b, ix) = plan_slot("opacity 0.05");
    let frame = b.widgets[ix]
        .parts
        .iter()
        .find(|p| p.part.name == "frame")
        .expect("the frame part");
    assert_ne!(frame.paint, Paint::default());
}

#[test]
fn a_skin_may_place_an_app_widget_in_its_slot() {
    let (b, ix) = plan_slot("grow 2; shrink 0; align-self center; margin 4 5 6 7");
    let node = b.tree.get(b.widgets[ix].node).expect("the node");
    assert_eq!(node.layout.grow, 2.0);
    assert_eq!(node.layout.shrink, 0.0);
    assert_eq!(node.layout.align_self, Some(gogo_skin::ir::Align::Center));
    assert_eq!(node.layout.margin.top, 4.0);
    assert_eq!(node.layout.margin.right, 5.0);
    assert_eq!(node.layout.margin.bottom, 6.0);
    assert_eq!(node.layout.margin.left, 7.0);
}

// ---- borrowed fragments reach into the components they use ----

const NESTED: &str = "\ncomponent \"game.builds\" {\n    use \"frag-inner\"\n}\ncomponent \"frag-inner\" {\n    text \"x is {ui.mode_x == b ? 'b' : 'not b'}\" id=\"frag\"\n}\n";

#[test]
fn a_component_a_borrowed_fragment_uses_is_checked_against_the_borrower() {
    // The `ui` variable is read two components down from the fragment.
    let (default, other) = borrower_and_default(NESTED, "");
    let err = page_with_builds_tab(&default, &other, None).unwrap_err();
    let BindErrorKind::Borrow(found) = err.kind() else {
        panic!("{err}")
    };
    assert!(
        found.iter().any(|f| f.code == Code::UnknownField),
        "{found:?}"
    );
    // The borrower that declares it binds the same fragment, and again after the check
    // was kept.
    let (default, other) =
        borrower_and_default(NESTED, "var \"mode_x\" type=\"enum(a b)\" default=\"a\"");
    for _ in 0..2 {
        let b = page_with_builds_tab(&default, &other, Some("b")).unwrap();
        assert!(texts(&b.tree).contains(&"x is b".to_owned()));
    }
}

// ---- limits at bind (design 27 §8) ----

fn long_list(n: usize) -> Value {
    Value::List((0..n).map(|i| Value::Num(i as f64)).collect())
}

#[test]
fn nested_lists_with_empty_bodies_are_stopped_by_the_step_budget() {
    let mut f = base();
    game_insert(
        &mut f,
        "each \"a\" in=\"{game.env($id)}\" { each \"b\" in=\"{game.env($id)}\" { each \"c\" in=\"{game.env($id)}\" { let x=\"1\" } } }",
    );
    let pkg = skin("default", f);
    let mut scope = FixtureScope::new();
    scope.set_call("game.env", GAME_ID, long_list(1000));
    let ui = UiState::defaults(&pkg.manifest);
    let started = std::time::Instant::now();
    let err = bind_in(&pkg, "game", &scope, &ui).unwrap_err();
    assert!(
        matches!(err.kind(), BindErrorKind::TooManySteps(n) if *n == gogo_skin::bind::MAX_BIND_STEPS),
        "{err}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "it stopped"
    );
    // A list under the budget binds.
    scope.set_call("game.env", GAME_ID, long_list(1000));
    let mut f = base();
    game_insert(&mut f, "each \"a\" in=\"{game.env($id)}\" { let x=\"1\" }");
    let pkg = skin("default", f);
    assert!(bind_in(&pkg, "game", &scope, &ui).is_ok());
}

/// What the shell's transfer strip shows when the queue's current job is at `fraction`.
fn strip_progress(fraction: f64) -> Result<Option<f32>, gogo_skin::BindError> {
    let pkg = default_skin();
    let mut scope = FixtureScope::new();
    let ui = UiState::defaults(&pkg.manifest);
    let Value::Map(mut q) = scope.var("queue.summary").cloned().unwrap() else {
        panic!("not a record")
    };
    let Some(Value::Map(mut cur)) = q.remove("current") else {
        panic!("no current job")
    };
    cur.insert("fraction".to_owned(), Value::Num(fraction));
    q.insert("current".to_owned(), Value::Map(cur));
    scope.set_var("queue.summary", Value::Map(q));
    let b = bind_in(&pkg, "shell", &scope, &ui)?;
    Ok(b.tree.iter().find_map(|id| b.tree.get(id)?.progress))
}

#[test]
fn a_progress_a_hair_over_one_is_one_and_further_is_an_error() {
    assert_eq!(strip_progress(1.0000001).unwrap(), Some(1.0));
    assert_eq!(strip_progress(-0.0000001).unwrap(), Some(0.0));
    assert_eq!(strip_progress(0.5).unwrap(), Some(0.5));
    for bad in [1.01, -0.01, f64::NAN, f64::INFINITY] {
        let err = strip_progress(bad).unwrap_err();
        assert!(
            matches!(err.kind(), BindErrorKind::Progress(_)),
            "{bad}: {err}"
        );
    }
}

#[test]
fn a_state_beats_the_base_of_every_style_on_the_element() {
    // Design 27 §9.1: all the bases left to right, then the states in effect, so the `:on`
    // of the first style wins over the base of the second.
    let mut f = base();
    append(
        &mut f,
        "styles.kdl",
        "\nstyle \"first\" { fill \"#111111\"; :on { fill \"#222222\" } }\nstyle \"second\" { fill \"#333333\" }\n",
    );
    game_insert(
        &mut f,
        "button \"On\" on=#true style=\"first second\" action=\"game.verify\" args=\"{$id}\" id=\"o\"",
    );
    let pkg = skin("default", f);
    let b = bind(&pkg, "game", &FixtureScope::new());
    let node = b.tree.get(b.tree.by_name("o").unwrap()).unwrap();
    assert_eq!(
        node.paint.fill,
        Some(Fill::Solid(gogo_skin::ir::Color::rgba(
            0x22, 0x22, 0x22, 255
        )))
    );
}
