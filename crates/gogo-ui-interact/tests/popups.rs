//! Popups: they trap input while no dialog is up, close from outside, walk their lists and
//! give focus back (design 25 §4).

mod common;

use common::*;
use gogo_ui_interact::{Frame, Input, Key, LayerId, LayerKind, PointerButton, RequestKind};
use gogo_ui_ir::{A11y, Dim, DrawnBy, Node, Role, Tree};

const POPUP: LayerId = LayerId(10);

fn item(name: &str, role: Role, part: u32) -> Node {
    let mut node = app_button(name, part);
    node.a11y.role = role;
    node
}

/// A page with a menu button below the popups' items, so a click meant for it can be told
/// from one meant for them.
fn page() -> Screen {
    let mut spacer = Node::container();
    spacer.layout.height = Dim::Px(400.0);
    let mut more = button("More", 0);
    more.a11y = A11y::control(Role::MenuButton, "More");
    more.action = None;
    Screen::new(vec![
        ("play", button("Play", 0)),
        ("gap", spacer),
        ("more", more),
    ])
}

fn menu() -> Screen {
    Screen::new(vec![
        ("one", item("One", Role::MenuItem, 1)),
        ("two", item("Two", Role::MenuItem, 2)),
        ("three", item("Three", Role::MenuItem, 3)),
    ])
}

/// A palette: a field, then two rows.
fn palette() -> Screen {
    let mut field = app_button("Search", 0);
    field.a11y.role = Role::TextInput;
    Screen::new(vec![
        ("field", field),
        ("one", item("One", Role::ListOption, 1)),
        ("two", item("Two", Role::ListOption, 2)),
    ])
}

#[test]
fn only_the_popup_takes_input_and_a_press_outside_only_closes_it() {
    let (page, menu) = (page(), menu());
    let layers = [
        page.layer(PAGE, LayerKind::Page),
        menu.layer(POPUP, LayerKind::Popup).initial_focus("one"),
    ];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    d.ix.sync(&frame);
    assert_eq!(d.focused(), "one");
    // On the page's button, below the popup's column: it only closes the popup.
    let outside = page.center("more");
    assert_eq!(names(&d.press_at(&frame, outside)), ["dismiss"]);
    assert!(d.release_at(&frame, outside).is_empty());
    // A secondary press closes nothing.
    let (x, y) = outside;
    let button = PointerButton::Secondary;
    assert!(
        d.send(&frame, Input::PointerDown { button, x, y })
            .intents
            .is_empty()
    );
    // Inside, an item acts and a page control can't be reached by a shortcut or Tab.
    assert_eq!(names(&d.click(&frame, menu.center("two"))), ["app two"]);
    for _ in 0..4 {
        assert_ne!(d.tab(&frame), "play");
    }
    assert!(
        d.assist(&frame, PAGE, "play", RequestKind::Activate)
            .is_empty()
    );
    assert_eq!(names(&d.tap(&frame, Key::Escape)), ["dismiss"]);
}

#[test]
fn a_dialog_wins_over_a_popup_wherever_it_is_in_the_stack() {
    let (page, menu) = (page(), menu());
    let dialog = Screen::new(vec![("no", app_button("No", 9))]);
    let popup = menu.layer(POPUP, LayerKind::Popup);
    let ask = dialog.layer(DIALOG, LayerKind::Dialog);
    for layers in [
        [page.layer(PAGE, LayerKind::Page), popup, ask],
        // The app never builds this; the popup still gets nothing.
        [page.layer(PAGE, LayerKind::Page), ask, popup],
    ] {
        let frame = Frame { layers: &layers };
        assert_eq!(frame.interactive().len(), 1);
        assert_eq!(frame.interactive()[0].id, DIALOG);
        let mut d = Driver::new();
        // On a menu item that no dialog control is under: nothing, not even a dismiss.
        assert!(d.click(&frame, menu.center("three")).is_empty());
        assert!(
            d.assist(&frame, POPUP, "one", RequestKind::Activate)
                .is_empty()
        );
        d.tab(&frame);
        assert_ne!(d.focused(), "one");
    }
}

#[test]
fn up_and_down_walk_the_items_and_stop_at_the_ends() {
    let menu = menu();
    let layers = [menu.layer(POPUP, LayerKind::Popup).initial_focus("one")];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    d.ix.sync(&frame);
    d.tap(&frame, Key::ArrowUp);
    assert_eq!(d.focused(), "one");
    d.tap(&frame, Key::ArrowDown);
    d.tap(&frame, Key::ArrowDown);
    d.tap(&frame, Key::ArrowDown);
    assert_eq!(d.focused(), "three");
    // A held arrow moves.
    d.repeat(&frame, Key::ArrowUp);
    assert_eq!(d.focused(), "two");
    assert_eq!(names(&d.tap(&frame, Key::Enter)), ["app two"]);
    // On a page they do nothing.
    let layers = [menu.layer(PAGE, LayerKind::Page)];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    d.tap(&frame, Key::ArrowDown);
    assert_eq!(d.focused(), "");
}

#[test]
fn a_field_over_a_list_moves_the_highlight_and_enter_activates_it() {
    let palette = palette();
    let layers = [palette
        .layer(POPUP, LayerKind::Popup)
        .initial_focus("field")
        .submit("two")];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    d.ix.sync(&frame);
    assert_eq!(names(&d.tap(&frame, Key::ArrowDown)), ["move field Next"]);
    assert_eq!(names(&d.tap(&frame, Key::ArrowUp)), ["move field Prev"]);
    assert_eq!(d.focused(), "field");
    assert_eq!(names(&d.tap(&frame, Key::Enter)), ["app two"]);
    assert!(d.repeat(&frame, Key::Enter).is_empty());
    // Typing still goes to the field.
    let typed = d.send(&frame, Input::Text("a".to_owned())).intents;
    assert_eq!(typed.len(), 1);
}

#[test]
fn closing_a_popup_gives_focus_back_to_what_opened_it() {
    let (page, menu) = (page(), menu());
    let before = [page.layer(PAGE, LayerKind::Page)];
    let before = Frame { layers: &before };
    let mut d = Driver::new();
    d.click(&before, page.center("more"));
    assert_eq!(d.focused(), "more");
    let open = [
        page.layer(PAGE, LayerKind::Page),
        menu.layer(POPUP, LayerKind::Popup).initial_focus("one"),
    ];
    d.ix.sync(&Frame { layers: &open });
    assert_eq!(d.focused(), "one");
    // Opened by a click: no ring until the keyboard moves.
    assert!(d.ix.focus_visible().is_none());
    d.tap(&Frame { layers: &open }, Key::ArrowDown);
    assert!(d.ix.focus_visible().is_some());
    d.ix.sync(&before);
    assert_eq!(d.focused(), "more");
}

#[test]
fn a_press_on_an_item_whose_call_changed_does_nothing() {
    let item = || {
        let mut node = button("Go", 0);
        node.a11y.role = Role::MenuItem;
        node.drawn_by = DrawnBy::App(part_of(1));
        node
    };
    let mut tree = Tree::new("root", Node::container());
    let root = tree.root();
    tree.add_child(root, "go", item()).expect("add");
    let screen = Screen::of(tree);
    let (a, b) = ([1u64], [2u64]);
    let was = [screen.layer(POPUP, LayerKind::Popup).identity(&a)];
    let now = [screen.layer(POPUP, LayerKind::Popup).identity(&b)];
    let at = screen.center("go");
    let mut d = Driver::new();
    d.press_at(&Frame { layers: &was }, at);
    assert!(d.release_at(&Frame { layers: &now }, at).is_empty());
}
