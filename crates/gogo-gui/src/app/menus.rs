//! Menus and selects over the fake engine, driven as the shell drives the app.

use gogo_gui_model::ErrorView;
use gogo_ui_interact::{Key, PointerButton};
use gogo_ui_shell::accesskit::Role;

use super::tests::{app, at, bounds, click_at, frame, key, press};
use super::*;

/// Opens the popup of the control called `name`.
fn open(app: &mut App, role: Role, name: &str, ms: u64) {
    press(app, role, name, ms);
    // Focus reaches the popup once it is on the display.
    frame(app);
}

fn heard(app: &mut App, role: Role) -> Vec<String> {
    app.accessibility_tree()
        .nodes
        .iter()
        .filter(|(_, n)| n.role() == role)
        .filter_map(|(_, n)| n.label().map(str::to_owned))
        .collect()
}

fn ran(app: &App) -> Vec<&'static str> {
    app.data
        .backend()
        .runs
        .iter()
        .map(|r| r.call.name())
        .collect()
}

fn is_open(app: &App) -> bool {
    app.shown.popup.is_some()
}

fn focused(app: &App) -> String {
    app.interaction
        .focus()
        .map(|t| t.key.clone())
        .unwrap_or_default()
}

#[test]
fn the_menu_opens_alone_and_esc_closes_it_before_going_back() {
    let mut app = app();
    open(&mut app, Role::Button, "More", 10);
    assert!(is_open(&app));
    assert_eq!(heard(&mut app, Role::Menu), ["More"]);
    assert_eq!(heard(&mut app, Role::MenuItem), ["Open game folder"]);
    // Nothing of the page is exposed under it.
    assert!(!heard(&mut app, Role::Button).contains(&"Verify files".to_owned()));
    assert_eq!(focused(&app), "popup/item/0");
    key(&mut app, Key::Escape, 20);
    assert!(!is_open(&app));
    assert_eq!(app.data.location().route, "game");
    // Focus is back on what opened it, and the next Esc goes back as before.
    assert!(focused(&app).contains("menu"), "{}", focused(&app));
    key(&mut app, Key::Escape, 30);
    assert_eq!(app.data.location().route, "library");
}

#[test]
fn choosing_an_item_runs_its_call_once_and_closes() {
    let mut app = app();
    open(&mut app, Role::Button, "More", 10);
    assert!(ran(&app).is_empty(), "opening runs nothing");
    key(&mut app, Key::Enter, 20);
    assert_eq!(ran(&app), ["game.open_folder"]);
    assert!(!is_open(&app));
}

#[test]
fn a_press_outside_closes_it_and_presses_nothing() {
    let mut app = app();
    let library = bounds(&mut app, Role::Button, "Library");
    open(&mut app, Role::Button, "More", 10);
    click_at(&mut app, library, 20);
    frame(&mut app);
    assert!(!is_open(&app));
    assert_eq!(app.data.location().route, "game");
    assert!(ran(&app).is_empty());
}

#[test]
fn a_select_runs_its_call_with_the_chosen_option() {
    let mut app = app();
    open(&mut app, Role::ComboBox, "Renderer", 10);
    assert!(is_open(&app), "the select is on screen");
    let options = heard(&mut app, Role::ListBoxOption);
    assert!(options.len() > 1, "{options:?}");
    // On the current one; move to another and choose it.
    let current = popup::index_of(&focused(&app)).expect("on an option");
    let (step, next) = if current + 1 < options.len() {
        (Key::ArrowDown, current + 1)
    } else {
        (Key::ArrowUp, current - 1)
    };
    key(&mut app, step, 20);
    assert_eq!(popup::index_of(&focused(&app)), Some(next));
    key(&mut app, Key::Enter, 30);
    assert_eq!(ran(&app), ["runner.set_renderer"]);
    assert!(!is_open(&app));
    let args = format!("{:?}", app.data.backend().runs[0]);
    assert!(args.contains(&options[next]), "{args}");
}

#[test]
fn a_safety_screen_closes_the_popup_for_good() {
    let mut app = app();
    open(&mut app, Role::Button, "More", 10);
    let item = bounds(&mut app, Role::MenuItem, "Open game folder");
    app.data
        .failures()
        .report(ErrorView::failed("x", "it failed".to_owned()));
    app.data.pump();
    // The error is not on the display yet: a click on the popup does nothing already.
    click_at(&mut app, item, 20);
    assert!(ran(&app).is_empty());
    frame(&mut app);
    frame(&mut app);
    assert!(app.shown.dialog.is_some());
    assert!(!is_open(&app));
    assert!(app.popups.is_empty());
    let ticket = app.data.modal_ticket().expect("an error");
    app.close(ticket);
    frame(&mut app);
    assert!(app.data.modal_ticket().is_none());
    assert!(!is_open(&app), "it does not come back");
}

#[test]
fn before_a_popup_is_on_the_display_only_esc_acts() {
    let mut app = app();
    let library = bounds(&mut app, Role::Button, "Library");
    let more = bounds(&mut app, Role::Button, "More");
    // Opened, and no frame since.
    click_at(&mut app, more, 10);
    assert!(!app.popups.is_empty() && !is_open(&app));
    click_at(&mut app, library, 20);
    assert_eq!(app.data.location().route, "game");
    let escape = Input::KeyDown {
        key: Key::Escape,
        mods: Default::default(),
        repeat: false,
    };
    app.input(escape, at(30));
    assert!(app.popups.is_empty());
    assert_eq!(app.data.location().route, "game", "Esc did not go back");
}

#[test]
fn a_resize_closes_it() {
    let mut app = app();
    open(&mut app, Role::Button, "More", 10);
    app.draw((1000.0, 700.0));
    app.show(at(20));
    assert!(!is_open(&app));
}

#[test]
fn an_item_that_changes_something_shows_the_plan_first() {
    let mut files = gogo_skins::default().to_files();
    for (path, data) in &mut files {
        if path == "routes/game.kdl" {
            let text = String::from_utf8(std::mem::take(data)).expect("utf-8");
            *data = text
                .replace(
                    "item \"Open game folder\" action=\"game.open_folder\" args=\"{$id} game\"",
                    "item \"Fetch\" action=\"game.download\" args=\"{$id}\"",
                )
                .into_bytes();
        }
    }
    let mut app = app();
    app.page.replace_built_in(files);
    frame(&mut app);
    open(&mut app, Role::Button, "More", 10);
    let (x, y) = bounds(&mut app, Role::MenuItem, "Fetch");
    let button = PointerButton::Primary;
    app.input(Input::PointerDown { button, x, y }, at(20));
    app.input(Input::PointerUp { button, x, y }, at(21));
    frame(&mut app);
    frame(&mut app);
    assert!(app.shown.dialog.is_some(), "a safety screen");
    assert!(!is_open(&app));
    assert!(ran(&app).is_empty(), "nothing ran before the confirm");
}
