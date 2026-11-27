//! The command palette over the fake engine, driven as the shell drives the app.

use gogo_contract::routes::ROUTES;
use gogo_gui_model::ErrorView;
use gogo_ui_interact::{ImeInput, Key, Modifiers};
use gogo_ui_shell::accesskit::{Action, Role, TreeId};
use gogo_ui_shell::{ShellApp as _, Zeroizing};

use super::tests::{app, at, bounds, click_at, frame, key};
use super::*;

/// Ctrl K (Cmd K), then the frames that show what it did.
fn ctrl_k(app: &mut App, ms: u64) {
    let mods = match PRIMARY {
        Primary::Meta => Modifiers {
            meta: true,
            ..Modifiers::default()
        },
        _ => Modifiers {
            ctrl: true,
            ..Modifiers::default()
        },
    };
    let key = Key::Char('k');
    app.input(
        Input::KeyDown {
            key,
            mods,
            repeat: false,
        },
        at(ms),
    );
    app.input(Input::KeyUp { key }, at(ms + 1));
    frame(app);
    // Focus reaches the field once the palette is on the display.
    frame(app);
}

fn text(app: &mut App, text: &str, ms: u64) {
    app.input(Input::Text(text.to_owned()), at(ms));
    frame(app);
}

fn is_open(app: &App) -> bool {
    app.shown.popup.is_some()
}

/// The rows a screen reader finds, and which is highlighted.
fn rows(app: &mut App) -> (Vec<String>, Option<String>) {
    let tree = app.accessibility_tree();
    let rows = tree
        .nodes
        .iter()
        .filter(|(_, n)| n.role() == Role::ListBoxOption);
    let names = rows
        .clone()
        .filter_map(|(_, n)| n.label().map(str::to_owned))
        .collect();
    let highlighted = rows
        .filter(|(_, n)| n.is_selected() == Some(true))
        .filter_map(|(_, n)| n.label().map(str::to_owned))
        .next();
    (names, highlighted)
}

fn ran(app: &App) -> Vec<&'static str> {
    app.data
        .backend()
        .runs
        .iter()
        .map(|r| r.call.name())
        .collect()
}

#[test]
fn ctrl_k_opens_it_from_every_route_and_closes_it_again() {
    let mut app = app();
    let game = app
        .data
        .location()
        .params
        .get("id")
        .cloned()
        .expect("on a game");
    let mut ms = 10;
    for route in ROUTES {
        if route.name == "shell" {
            continue;
        }
        let mut go = Invocation::new(actions::action("nav.go").expect("nav.go"))
            .arg("route", Value::str(route.name));
        if route.name == "game" {
            go = go.route_arg("id", game.clone());
        }
        // A press that lands on nothing, for the model to count.
        click_at(&mut app, (-10.0, -10.0), ms);
        let event = act::number(app.interaction.latest());
        app.invoke(&go, event);
        frame(&mut app);
        assert_eq!(app.data.location().route, route.name);
        assert!(app.shown.panel.is_none(), "{}", route.name);
        ctrl_k(&mut app, ms + 10);
        assert!(is_open(&app), "{}", route.name);
        assert_eq!(
            app.interaction.focus().map(|t| t.key.as_str()),
            Some(palette::FIELD),
            "{}",
            route.name
        );
        ctrl_k(&mut app, ms + 20);
        assert!(!is_open(&app), "{}", route.name);
        ms += 100;
    }
}

#[test]
fn it_is_a_modal_dialog_and_esc_closes_it_before_going_back() {
    let mut app = app();
    ctrl_k(&mut app, 10);
    let tree = app.accessibility_tree();
    let dialog = tree
        .nodes
        .iter()
        .find(|(_, n)| n.role() == Role::Dialog)
        .expect("a dialog");
    assert_eq!(dialog.1.label(), Some("Command palette"));
    assert!(dialog.1.is_modal());
    let field = tree.nodes.iter().find(|(id, _)| *id == tree.focus);
    assert!(field.expect("focused").1.active_descendant().is_some());
    assert!(
        !tree
            .nodes
            .iter()
            .any(|(_, n)| n.label() == Some("Verify files"))
    );
    key(&mut app, Key::Escape, 20);
    assert!(!is_open(&app));
    assert_eq!(app.data.location().route, "game");
    key(&mut app, Key::Escape, 30);
    assert_eq!(app.data.location().route, "library");
}

#[test]
fn the_search_button_of_the_skin_opens_it() {
    let mut app = app();
    super::tests::click(&mut app, "search", 10);
    frame(&mut app);
    assert!(is_open(&app));
}

#[test]
fn typing_filters_and_up_down_and_enter_choose() {
    let mut app = app();
    ctrl_k(&mut app, 10);
    text(&mut app, "go to", 20);
    let (names, highlighted) = rows(&mut app);
    assert!(names.iter().all(|n| n.starts_with("Go to")), "{names:?}");
    assert_eq!(highlighted.as_ref(), names.first());
    key(&mut app, Key::ArrowDown, 30);
    assert_eq!(rows(&mut app).1.as_ref(), names.get(1));
    key(&mut app, Key::ArrowUp, 40);
    key(&mut app, Key::ArrowUp, 41);
    assert_eq!(rows(&mut app).1.as_ref(), names.first());
    key(&mut app, Key::Backspace, 50);
    text(&mut app, "o queue", 60);
    key(&mut app, Key::Enter, 70);
    assert!(!is_open(&app));
    assert_eq!(app.data.location().route, "queue");
    // It opens afresh.
    ctrl_k(&mut app, 80);
    text(&mut app, "no such command", 90);
    assert_eq!(rows(&mut app).0, Vec::<String>::new());
    key(&mut app, Key::Enter, 100);
    assert!(is_open(&app));
    assert_eq!(app.data.location().route, "queue");
}

#[test]
fn enter_runs_only_what_was_on_the_display() {
    let mut app = app();
    ctrl_k(&mut app, 10);
    text(&mut app, "refresh", 20);
    // Typed and Enter pressed before the new list was drawn.
    app.input(Input::Text("zzz".to_owned()), at(30));
    let enter = Input::KeyDown {
        key: Key::Enter,
        mods: Modifiers::default(),
        repeat: false,
    };
    app.input(enter, at(31));
    assert!(ran(&app).is_empty());
    assert!(!app.popups.is_empty());
}

#[test]
fn an_entry_that_changes_something_shows_the_plan_first() {
    let mut app = app();
    ctrl_k(&mut app, 10);
    text(&mut app, "download files", 20);
    key(&mut app, Key::Enter, 30);
    frame(&mut app);
    assert!(app.shown.dialog.is_some(), "the plan card");
    assert!(!is_open(&app));
    assert!(ran(&app).is_empty(), "nothing ran before the confirm");
    // Ctrl K under a safety screen does nothing.
    ctrl_k(&mut app, 40);
    assert!(app.popups.is_empty() && app.shown.dialog.is_some());
}

#[test]
fn a_safety_screen_closes_it_and_a_screen_reader_can_use_it() {
    let mut app = app();
    ctrl_k(&mut app, 10);
    let tree = app.accessibility_tree();
    let (field, _) = tree
        .nodes
        .iter()
        .find(|(_, n)| n.role() == Role::TextInput)
        .expect("the field");
    let set = ActionRequest {
        action: Action::SetValue,
        target_tree: TreeId::ROOT,
        target_node: *field,
        data: Some(gogo_ui_shell::accesskit::ActionData::Value(
            "refresh".into(),
        )),
    };
    app.accessibility_action(set, at(20));
    frame(&mut app);
    assert_eq!(rows(&mut app).0, ["Refresh library"]);
    let row = bounds(&mut app, Role::ListBoxOption, "Refresh library");
    app.data
        .failures()
        .report(ErrorView::failed("x", "it failed".to_owned()));
    app.data.pump();
    click_at(&mut app, row, 30);
    assert!(ran(&app).is_empty());
    frame(&mut app);
    frame(&mut app);
    assert!(app.shown.dialog.is_some() && app.popups.is_empty() && !is_open(&app));
}

#[test]
fn a_skin_cannot_change_what_it_lists() {
    let mut plain = app();
    ctrl_k(&mut plain, 10);
    let listed = rows(&mut plain).0;
    let mut files = gogo_skins::default().to_files();
    for (path, data) in &mut files {
        if path == "routes/shell.kdl" {
            let text = String::from_utf8(std::mem::take(data)).expect("utf-8");
            *data = text
                .replacen(
                    "button \"Search…\"",
                    "button \"Delete everything\" action=\"game.remove\" args=\"1 false\"\n            button \"Search…\"",
                    1,
                )
                .into_bytes();
        }
    }
    let mut skinned = app();
    skinned.page.replace_built_in(files);
    frame(&mut skinned);
    ctrl_k(&mut skinned, 10);
    assert_eq!(rows(&mut skinned).0, listed);
}

/// What the field holds, as assistive technology reads it.
fn query(app: &mut App) -> String {
    app.accessibility_tree()
        .nodes
        .iter()
        .find(|(_, n)| n.role() == Role::TextInput)
        .and_then(|(_, n)| n.value().map(str::to_owned))
        .expect("the field")
}

fn chord(app: &mut App, key: Key, mods: Modifiers, ms: u64) {
    app.input(
        Input::KeyDown {
            key,
            mods,
            repeat: false,
        },
        at(ms),
    );
    app.input(Input::KeyUp { key }, at(ms + 1));
    frame(app);
}

fn primary() -> Modifiers {
    match PRIMARY {
        Primary::Meta => Modifiers {
            meta: true,
            ..Modifiers::default()
        },
        _ => Modifiers {
            ctrl: true,
            ..Modifiers::default()
        },
    }
}

#[test]
fn an_input_method_composes_and_commits_into_the_search() {
    let mut app = app();
    assert!(app.ime_area().is_none(), "no field on the page");
    ctrl_k(&mut app, 10);
    let area = app.ime_area().expect("the field takes text");
    let all = rows(&mut app).0;
    app.input(
        Input::Ime(ImeInput::Preedit {
            text: "refr".to_owned(),
            cursor: Some((4, 4)),
        }),
        at(20),
    );
    frame(&mut app);
    // A composition is not searched for, and assistive technology doesn't read it yet.
    assert_eq!(rows(&mut app).0, all);
    assert_eq!(query(&mut app), "");
    // Enter belongs to the input method meanwhile.
    key(&mut app, Key::Enter, 30);
    assert!(is_open(&app) && ran(&app).is_empty());
    app.input(Input::Ime(ImeInput::Commit("refresh".to_owned())), at(40));
    frame(&mut app);
    assert_eq!(query(&mut app), "refresh");
    assert_eq!(rows(&mut app).0, ["Refresh library"]);
    // The candidates follow the caret.
    assert!(app.ime_area().expect("still").x > area.x);
    key(&mut app, Key::Enter, 50);
    assert!(!is_open(&app));
    assert_eq!(ran(&app), ["library.refresh"]);
    assert!(app.ime_area().is_none());
}

#[test]
fn assistive_technology_sets_the_search() {
    let mut app = app();
    ctrl_k(&mut app, 10);
    let tree = app.accessibility_tree();
    let (field, _) = tree
        .nodes
        .iter()
        .find(|(_, n)| n.role() == Role::TextInput)
        .expect("the field");
    let set = ActionRequest {
        action: Action::SetValue,
        target_tree: TreeId::ROOT,
        target_node: *field,
        data: Some(gogo_ui_shell::accesskit::ActionData::Value("queue".into())),
    };
    assert!(app.accessibility_action(set, at(20)));
    // Set, and Enter pressed before the new list was drawn: nothing yet.
    let enter = Input::KeyDown {
        key: Key::Enter,
        mods: Modifiers::default(),
        repeat: false,
    };
    app.input(enter, at(21));
    app.input(Input::KeyUp { key: Key::Enter }, at(22));
    assert_eq!(app.data.location().route, "game");
    frame(&mut app);
    assert_eq!(query(&mut app), "queue");
    assert_eq!(rows(&mut app).0, ["Go to Queue"]);
}

#[test]
fn the_search_is_edited_like_any_field() {
    let mut app = app();
    ctrl_k(&mut app, 10);
    text(&mut app, "go to queue", 20);
    // A word at a time, then everything, then back.
    chord(&mut app, Key::Backspace, primary(), 30);
    assert_eq!(query(&mut app), "go to ");
    chord(&mut app, Key::Char('a'), primary(), 40);
    chord(&mut app, Key::Char('c'), primary(), 50);
    assert_eq!(app.copied().as_deref().map(String::as_str), Some("go to "));
    chord(&mut app, Key::Char('x'), primary(), 60);
    assert_eq!(query(&mut app), "");
    chord(&mut app, Key::Char('z'), primary(), 70);
    assert_eq!(query(&mut app), "go to ");
    key(&mut app, Key::Home, 80);
    key(&mut app, Key::Delete, 90);
    assert_eq!(query(&mut app), "o to ");
    chord(&mut app, Key::Char('v'), primary(), 100);
    assert!(app.wants_paste());
    app.pasted(Some(Zeroizing::new("G".to_owned())));
    frame(&mut app);
    assert_eq!(query(&mut app), "Go to ");
}

#[test]
fn up_and_down_move_the_highlight_and_leave_the_page_where_it_was() {
    let mut app = app();
    click_at(&mut app, (-10.0, -10.0), 5);
    let go = Invocation::new(actions::action("nav.go").expect("nav.go"))
        .arg("route", Value::str("library"));
    let event = act::number(app.interaction.latest());
    app.invoke(&go, event);
    frame(&mut app);
    let page = app.shown.screen.as_ref().expect("a screen").layer().id;
    let offsets = |app: &App| -> Vec<(String, f32)> {
        app.interaction
            .offsets(page)
            .map(|(key, y)| (key.to_owned(), y))
            .collect()
    };
    let before = offsets(&app);
    ctrl_k(&mut app, 10);
    let first = rows(&mut app).1;
    key(&mut app, Key::ArrowDown, 20);
    key(&mut app, Key::ArrowDown, 30);
    assert_ne!(rows(&mut app).1, first);
    key(&mut app, Key::PageDown, 40);
    app.input(
        Input::Wheel {
            x: 700.0,
            y: 700.0,
            dy: 200.0,
        },
        at(50),
    );
    frame(&mut app);
    key(&mut app, Key::ArrowUp, 60);
    key(&mut app, Key::ArrowUp, 70);
    assert_eq!(rows(&mut app).1, first);
    assert_eq!(offsets(&app), before);
}

#[test]
fn an_open_palette_asks_for_each_source_once() {
    let mut app = app();
    ctrl_k(&mut app, 10);
    let subscribed = app.data.backend().subscriptions.len();
    for i in 0..5 {
        frame(&mut app);
        text(&mut app, "g", 20 + i);
    }
    assert_eq!(app.data.backend().subscriptions.len(), subscribed);
}
