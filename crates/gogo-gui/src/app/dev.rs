//! The skin dev mode's inspector in the app (design 25 §8): gated by `--dev`, beside the
//! page, and inert under anything modal.

use std::fs;
use std::path::PathBuf;

use gogo_ui_interact::{Key, Modifiers, PointerButton};
use gogo_ui_shell::accesskit::Role;
use tempfile::TempDir;

use super::safety::{HOLD_LABEL, hold_screen};
use super::tests::{SIZE, app, at, bounds, click_at, frame, key, middle};
use super::*;

/// The built-in skin written out as a folder.
fn copy_of_default() -> (TempDir, PathBuf) {
    let dir = crate::core::tests::scratch();
    let root = dir.path().join("default");
    for (path, data) in gogo_skins::default().files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().expect("a file in a folder")).expect("folders");
        fs::write(path, data).expect("a write");
    }
    (dir, root)
}

/// An app watching a copy of the built-in skin.
pub(super) fn dev_app() -> (TempDir, PathBuf, App) {
    let (dir, skin) = copy_of_default();
    let data = Data::fixture(|| {}).expect("fixture data");
    let mut app = App::new(Some(&skin), data).expect("the built-in skin");
    app.pending.scale = 1.0;
    frame(&mut app);
    (dir, skin, app)
}

fn edit(skin: &std::path::Path, from: &str, to: &str) {
    let file = skin.join("routes/game.kdl");
    let text = fs::read_to_string(&file).expect("the route");
    assert!(text.contains(from), "{from}");
    fs::write(&file, text.replacen(from, to, 1)).expect("a write");
}

/// How wide the page on screen was laid out.
fn page_width(app: &App) -> f64 {
    let screen = app.shown.screen.as_ref().expect("a screen");
    let rect = screen.laid.rect(screen.tree.root()).expect("laid out");
    rect.x1 - rect.x0
}

/// The text the inspector on screen shows, line by line.
fn lines(app: &App) -> Vec<String> {
    let drawn = app.shown.inspector.as_ref().expect("the inspector");
    let tree = drawn.tree();
    tree.iter()
        .filter_map(|id| match &tree.get(id)?.kind {
            gogo_ui_ir::Kind::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// Everything the inspector has to say, paging to its end.
fn all_lines(app: &mut App) -> String {
    let mut out = String::new();
    for turn in 0..40 {
        out += &lines(app).join("\n");
        out.push('\n');
        if !lines(app).iter().any(|l| l == "Later") {
            break;
        }
        let later = bounds(app, Role::Button, "Later");
        click_at(app, later, 10_000 + turn * 10);
        frame(app);
    }
    out
}

#[test]
fn without_dev_mode_f12_changes_nothing() {
    let mut app = app();
    let before = app.list.clone();
    key(&mut app, Key::F12, 10);
    assert!(app.inspector.is_none() && app.shown.inspector.is_none());
    assert_eq!(app.list, before);
    assert!(
        app.shown
            .screen
            .as_ref()
            .expect("a screen")
            .provenance
            .is_none()
    );
}

#[test]
fn f12_opens_the_inspector_beside_the_page_and_closes_it() {
    let (_dir, _skin, mut app) = dev_app();
    assert!(app.shown.inspector.is_none(), "closed at start");
    assert_eq!(page_width(&app), f64::from(SIZE.0));
    key(&mut app, Key::F12, 10);
    let beside = f64::from(inspector::width(SIZE.0));
    assert_eq!(page_width(&app), f64::from(SIZE.0) - beside);
    assert_eq!(app.list.width, f64::from(SIZE.0));
    assert!(
        lines(&app).iter().any(|l| l == "CHILDREN"),
        "{:?}",
        lines(&app)
    );
    key(&mut app, Key::F12, 20);
    assert!(app.shown.inspector.is_none());
    assert_eq!(page_width(&app), f64::from(SIZE.0));
}

#[test]
fn a_node_is_walked_to_and_follows_the_data_and_the_files() {
    let (_dir, skin, mut app) = dev_app();
    key(&mut app, Key::F12, 10);
    // From the root down to the shell's outlet and the game's title.
    for (step, part) in ["#main", "#title"].iter().enumerate() {
        walk_to(&mut app, part, 100 + step as u64 * 100);
    }
    let said = all_lines(&mut app);
    assert!(said.contains("routes/game.kdl:"), "{said}");
    assert!(said.contains("{g.title}"), "{said}");
    assert!(said.contains("PARENTS"), "{said}");
    assert!(SAID.lock().expect("not poisoned").iter().any(|l| {
        l.starts_with("gui: inspector selected root/") && l.contains(" routes/game.kdl:")
    }));

    // An edit of the file changes what is said of the same node.
    edit(
        &skin,
        "text \"{g.title}\" style=\"h1\"",
        "text \"edited {g.title}\" style=\"h1\"",
    );
    assert!(app.files_changed());
    frame(&mut app);
    let said = all_lines(&mut app);
    assert!(said.contains("edited"), "{said}");

    // A broken one keeps the last good skin, and the inspector takes nothing under the
    // error panel.
    edit(&skin, "text \"edited", "nonsense {{ text \"edited");
    app.files_changed();
    frame(&mut app);
    assert!(app.shown.panel.is_some());
    let selected = app.inspector.as_ref().expect("open").selected().to_owned();
    key(&mut app, Key::F12, 20_000);
    assert!(app.inspector.is_some(), "only Esc gets past the panel");
    let point = (f64::from(SIZE.0) - 100.0, 200.0);
    click_at(&mut app, point, 20_100);
    assert_eq!(app.inspector.as_ref().expect("open").selected(), selected);
}

/// Picks children, from the root of what is selected, until one with `part` is picked:
/// depth first over the tree on screen.
fn walk_to(app: &mut App, part: &str, ms: u64) {
    let screen = app.shown.screen.clone().expect("a screen");
    let name = part.trim_start_matches('#');
    let target = screen.tree.by_name(name).expect(name);
    let mut chain = vec![target];
    while let Some(parent) = screen.tree.parent(chain[chain.len() - 1]) {
        chain.push(parent);
    }
    let here = app.inspector.as_ref().expect("open").selected().to_owned();
    let from = screen.tree.by_key(&here).expect("selected");
    let start = chain.iter().position(|&n| n == from).expect("an ancestor");
    for (i, &node) in chain[..start].iter().rev().enumerate() {
        let key = screen.tree.key(node).expect("a key").to_owned();
        // Page to the button that selects it.
        let mut turns = 0;
        while !drawn_picks(app).contains(&key) {
            let later = bounds(app, Role::Button, "Later");
            click_at(app, later, ms + turns);
            frame(app);
            turns += 1;
            assert!(turns < 40, "{key} not listed");
        }
        let drawn = app.shown.inspector.clone().expect("drawn");
        let button = drawn.button(&key).expect("a button");
        click_at(app, button, ms + 50 + i as u64 * 5);
        frame(app);
        assert_eq!(app.inspector.as_ref().expect("open").selected(), key);
    }
}

fn drawn_picks(app: &App) -> Vec<String> {
    app.shown
        .inspector
        .as_ref()
        .expect("drawn")
        .picks()
        .to_vec()
}

#[test]
fn under_a_safety_screen_only_the_dialog_takes_input() {
    // The app of `hold_screen`, with a skin folder being watched.
    let (_dir, skin) = copy_of_default();
    let mut app = hold_screen();
    app.dev = Some(skin);
    // The dialog is up: F12 does nothing.
    key(&mut app, Key::F12, 5000);
    assert!(app.inspector.is_none());

    // Opened before the dialog came, it stays beside the page and takes nothing.
    app.inspector = Some(Inspector::new());
    app.draw(SIZE);
    app.show(at(6000));
    let inspector = app.shown.inspector.clone().expect("drawn");
    assert!(app.shown.dialog.is_some());
    let child = inspector.picks().first().expect("a child").clone();
    let button = inspector.button(&child).expect("a button");
    let selected = app.inspector.as_ref().expect("open").selected().to_owned();
    click_at(&mut app, button, 7000);
    assert_eq!(app.inspector.as_ref().expect("open").selected(), selected);
    key(&mut app, Key::F12, 7100);
    assert!(app.inspector.is_some());
    // Assistive technology is not told of it, and the dialog still wants its hold.
    let tree = app.accessibility_tree();
    assert!(
        !tree
            .nodes
            .iter()
            .any(|(_, n)| n.label() == Some("Inspector"))
    );
    assert!(
        tree.nodes
            .iter()
            .any(|(_, n)| n.label() == Some(HOLD_LABEL))
    );
    assert!(app.data.backend().runs.is_empty());
}

#[test]
fn a_press_on_the_panel_reaches_nothing_of_the_page() {
    let (_dir, _skin, mut app) = dev_app();
    key(&mut app, Key::F12, 10);
    let (x, _) = middle(&app, "main");
    let said = SAID.lock().expect("not poisoned").len();
    let location = app.data.location().clone();
    let spot = (f64::from(SIZE.0) - 20.0, f64::from(SIZE.1) - 4.0);
    assert!(x < spot.0);
    let button = PointerButton::Primary;
    app.input(
        Input::PointerDown {
            button,
            x: spot.0,
            y: spot.1,
        },
        at(100),
    );
    app.input(
        Input::PointerUp {
            button,
            x: spot.0,
            y: spot.1,
        },
        at(101),
    );
    assert_eq!(app.data.location().route, location.route);
    assert_eq!(SAID.lock().expect("not poisoned").len(), said);
}

fn said() -> Vec<String> {
    SAID.lock().expect("not poisoned").clone()
}

fn move_to(app: &mut App, (x, y): (f64, f64), ms: u64) -> bool {
    app.input(Input::PointerMoved { x, y }, at(ms))
}

/// The inspector open and picking.
fn picking_app() -> (TempDir, App) {
    let (dir, _skin, mut app) = dev_app();
    key(&mut app, Key::F12, 10);
    let button = bounds(&mut app, Role::Button, "Pick a node");
    click_at(&mut app, button, 100);
    frame(&mut app);
    assert!(app.shown.inspector.as_ref().expect("drawn").picking);
    (dir, app)
}

fn selected(app: &App) -> String {
    app.inspector.as_ref().expect("open").selected().to_owned()
}

#[test]
fn picking_a_button_selects_it_and_runs_nothing() {
    let (_dir, mut app) = picking_app();
    let play = middle(&app, "play");
    let before = said().len();
    let route = app.data.location().route.clone();
    assert!(move_to(&mut app, play, 200), "the outline moves: a frame");
    let unmarked = app.list.ops.len();
    frame(&mut app);
    assert!(app.list.ops.len() > unmarked, "an outline is drawn");
    assert!(
        !move_to(&mut app, (play.0 + 1.0, play.1), 210),
        "same node: no frame"
    );
    click_at(&mut app, play, 300);
    frame(&mut app);
    assert!(!app.shown.inspector.as_ref().expect("drawn").picking);
    let screen = app.shown.screen.clone().expect("a screen");
    let node = screen.tree.by_name("play").expect("play");
    assert_eq!(Some(selected(&app).as_str()), screen.tree.key(node));
    assert_eq!(app.data.location().route, route);
    assert!(app.data.backend().runs.is_empty());
    assert!(app.data.modal_ticket().is_none(), "no plan card");
    let after = said().split_off(before);
    assert!(
        after
            .iter()
            .any(|l| l.starts_with("gui: inspector selected root/")),
        "{after:?}"
    );
    // Outside picking, the pointer asks for no frame.
    assert!(!move_to(&mut app, (play.0 + 2.0, play.1), 400));
}

#[test]
fn while_picking_keys_and_screen_readers_reach_nothing_of_the_page() {
    let (_dir, mut app) = picking_app();
    let route = app.data.location().route.clone();
    // The skin's own shortcut for Play, and the palette.
    key(&mut app, Key::Char('p'), 200);
    let ctrl = Modifiers {
        ctrl: PRIMARY == Primary::Ctrl,
        meta: PRIMARY == Primary::Meta,
        ..Modifiers::default()
    };
    app.input(
        Input::KeyDown {
            key: Key::Char('k'),
            mods: ctrl,
            repeat: false,
        },
        at(210),
    );
    frame(&mut app);
    assert!(app.popups.top().is_none());
    for step in 0..30 {
        key(&mut app, Key::Tab, 300 + step * 10);
        if let Some(focus) = app.interaction.focus() {
            assert_eq!(focus.layer, app.shown.inspector.as_ref().expect("drawn").id);
        }
    }
    let tree = app.accessibility_tree();
    assert!(!tree.nodes.iter().any(|(_, n)| n.label() == Some("Play")));
    assert!(app.data.backend().runs.is_empty());
    // Esc stops picking and goes nowhere.
    key(&mut app, Key::Escape, 900);
    assert!(!app.shown.inspector.as_ref().expect("drawn").picking);
    assert_eq!(app.data.location().route, route);
    assert_eq!(selected(&app), "root");
    // The same shortcut does run once picking is over.
    assert!(app.data.backend().runs.is_empty());
    key(&mut app, Key::Char('p'), 1000);
    assert!(!app.data.backend().runs.is_empty() || app.data.modal_ticket().is_some());
}

#[test]
fn the_page_gets_nothing_before_the_picking_frame_is_up() {
    let (_dir, _skin, mut app) = dev_app();
    key(&mut app, Key::F12, 10);
    let button = bounds(&mut app, Role::Button, "Pick a node");
    click_at(&mut app, button, 100);
    // No frame yet: input still lands on the one where the page is live.
    assert!(!app.shown.inspector.as_ref().expect("drawn").picking);
    let play = middle(&app, "play");
    click_at(&mut app, play, 200);
    assert!(app.data.backend().runs.is_empty());
    assert!(app.data.modal_ticket().is_none());
}

#[test]
fn a_safety_screen_ends_picking() {
    let (_dir, skin) = copy_of_default();
    let mut app = hold_screen();
    app.dev = Some(skin);
    app.inspector = Some(Inspector::new());
    app.draw(SIZE);
    app.show(at(6000));
    let drawn = app.shown.inspector.clone().expect("drawn");
    let inspector = app.inspector.as_mut().expect("open");
    // As if the press that starts picking came just before the dialog.
    inspector.press(&drawn, "inspector-picking");
    assert!(inspector.picking());
    app.draw(SIZE);
    app.show(at(6002));
    assert!(!app.shown.inspector.as_ref().expect("drawn").picking);
    assert!(!move_to(&mut app, (100.0, 100.0), 6100));
}

#[test]
fn warnings_and_fallbacks_are_listed() {
    let (_dir, skin, mut app) = dev_app();
    key(&mut app, Key::F12, 10);
    let button = bounds(&mut app, Role::Button, "Problems (0)");
    click_at(&mut app, button, 100);
    frame(&mut app);
    assert!(lines(&app).iter().any(|l| l == "none"), "{:?}", lines(&app));

    // A property set twice is a warning; a route the skin leaves out is the default's.
    edit(&skin, "style=\"crumb\"", "style=\"crumb\" style=\"crumb\"");
    let manifest = skin.join("skin.kdl");
    let text = fs::read_to_string(&manifest).expect("the manifest");
    fs::write(
        &manifest,
        text.replacen("\"shell\" \"game\" ", "\"shell\" ", 1),
    )
    .expect("a write");
    fs::remove_file(skin.join("routes/game.kdl")).expect("removed");
    fs::write(
        skin.join("routes/library.kdl"),
        format!(
            "{}\n",
            fs::read_to_string(skin.join("routes/library.kdl"))
                .expect("the route")
                .replacen(
                    "route \"library\" {",
                    "route \"library\" {\n    text \"a\" w=1 w=2",
                    1
                )
        ),
    )
    .expect("a write");
    app.files_changed();
    frame(&mut app);
    assert!(app.shown.panel.is_none(), "{:?}", app.problems);
    let said = all_lines(&mut app);
    assert!(said.contains("warning gogo_skin::"), "{said}");
    assert!(said.contains("routes/library.kdl:"), "{said}");
    assert!(said.contains("route game"), "{said}");
    assert!(said.contains("doesn't cover it"), "{said}");
}
