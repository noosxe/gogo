//! S13: everything with an action is reachable with the keyboard, in document order
//! (design 26 §7).

mod common;

use common::*;
use gogo_ui_interact::{FocusOrder, Frame, Key, LayerKind, RequestKind};
use gogo_ui_ir::{A11y, Node, Role};

fn page() -> Screen {
    Screen::new(vec![
        ("a", button("A", 0)),
        ("label", Node::text("not a control")),
        ("b", button("B", 1)),
        ("c", button("C", 2)),
    ])
}

fn keys(order: &FocusOrder) -> Vec<&str> {
    order.targets().iter().map(|t| t.key.as_str()).collect()
}

#[test]
fn s13_tab_follows_the_tree_and_wraps() {
    let s = page();
    let layers = [s.layer(PAGE, LayerKind::Page)];
    let frame = Frame { layers: &layers };
    assert_eq!(keys(&FocusOrder::of(&frame)), ["a", "b", "c"]);
    let mut d = Driver::new();
    assert_eq!(d.focused(), "", "a route starts on nothing");
    let walked: Vec<String> = (0..4).map(|_| d.tab(&frame)).collect();
    assert_eq!(walked, ["a", "b", "c", "a"]);
    assert_eq!(d.back_tab(&frame), "c");
    let mut d = Driver::new();
    assert_eq!(d.back_tab(&frame), "c");
    // A repeating Tab keeps moving.
    d.down(&frame, Key::Tab, NONE);
    d.repeat(&frame, Key::Tab);
    assert_eq!(d.focused(), "b");
}

#[test]
fn s13_the_ring_shows_for_keys_and_screen_readers_not_clicks() {
    let s = page();
    let layers = [s.layer(PAGE, LayerKind::Page)];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    d.click(&frame, s.center("b"));
    assert_eq!(d.focused(), "b");
    assert!(d.ix.focus_visible().is_none());
    d.tab(&frame);
    assert_eq!(d.ix.focus_visible().map(|t| t.key.as_str()), Some("c"));
    d.click(&frame, s.center("a"));
    assert!(d.ix.focus_visible().is_none());
    d.assist(&frame, PAGE, "b", RequestKind::Focus);
    assert_eq!(d.ix.focus_visible().map(|t| t.key.as_str()), Some("b"));
}

#[test]
fn s13_a_dialog_traps_focus_and_pointer() {
    let s = page();
    let dialog = Screen::new(vec![
        ("pad", Node::text("Remove?")),
        ("ok", app_button("Remove", 0)),
        ("cancel", app_button("Cancel", 1)),
    ]);
    let layers = [
        s.layer(PAGE, LayerKind::Page),
        dialog.layer(DIALOG, LayerKind::Dialog),
    ];
    let frame = Frame { layers: &layers };
    assert_eq!(keys(&FocusOrder::of(&frame)), ["ok", "cancel"]);
    let mut d = Driver::new();
    let walked: Vec<String> = (0..3).map(|_| d.tab(&frame)).collect();
    assert_eq!(walked, ["ok", "cancel", "ok"]);
    // `c` of the page is below any dialog control.
    assert!(d.click(&frame, s.center("c")).is_empty());
    assert!(
        d.assist(&frame, PAGE, "c", RequestKind::Activate)
            .is_empty()
    );
    assert_eq!(d.focused(), "ok");
}

#[test]
fn s13_what_is_drawn_over_a_dialog_takes_no_input() {
    let s = page();
    let dialog = Screen::new(vec![
        ("ok", app_button("Remove", 0)),
        ("cancel", app_button("Cancel", 1)),
    ]);
    // Drawn over the dialog, and over its first control.
    let toast = Screen::new(vec![("undo", app_button("Undo", 2))]);
    let layers = [
        s.layer(PAGE, LayerKind::Page),
        dialog.layer(DIALOG, LayerKind::Dialog),
        toast.layer(TOAST, LayerKind::Overlay),
    ];
    let frame = Frame { layers: &layers };
    assert_eq!(keys(&FocusOrder::of(&frame)), ["ok", "cancel"]);
    let mut d = Driver::new();
    let walked: Vec<String> = (0..3).map(|_| d.tab(&frame)).collect();
    assert_eq!(walked, ["ok", "cancel", "ok"]);
    // Neither the toast's control nor the dialog's one under it.
    assert!(d.click(&frame, toast.center("undo")).is_empty());
    assert!(
        d.assist(&frame, TOAST, "undo", RequestKind::Activate)
            .is_empty()
    );
    assert!(
        d.assist(&frame, TOAST, "undo", RequestKind::Focus)
            .is_empty()
    );
    assert_eq!(d.focused(), "ok");
    // Beside the toast the dialog still works.
    assert_eq!(
        names(&d.click(&frame, dialog.center("cancel"))),
        ["app cancel"]
    );
}

#[test]
fn s13_an_overlay_adds_to_the_order_and_lets_clicks_through() {
    let s = page();
    let toast = {
        let mut tree = gogo_ui_ir::Tree::new("root", s.tree.get(s.tree.root()).unwrap().clone());
        let mut node = app_button("Dismiss", 0);
        node.layout.margin.top = 300.0;
        let root = tree.root();
        tree.add_child(root, "x", node).unwrap();
        Screen::of(tree)
    };
    let layers = [
        s.layer(PAGE, LayerKind::Page),
        toast.layer(TOAST, LayerKind::Overlay),
    ];
    let frame = Frame { layers: &layers };
    assert_eq!(keys(&FocusOrder::of(&frame)), ["a", "b", "c", "x"]);
    let mut d = Driver::new();
    assert_eq!(names(&d.click(&frame, s.center("a"))), ["invoke a"]);
    assert_eq!(names(&d.click(&frame, toast.center("x"))), ["app x"]);
}

#[test]
fn s13_a_dialog_gets_its_initial_focus_and_gives_focus_back() {
    let s = page();
    let dialog = Screen::new(vec![
        ("ok", app_button("Remove", 0)),
        ("cancel", app_button("Cancel", 1)),
    ]);
    let alone = [s.layer(PAGE, LayerKind::Page)];
    let with = [
        s.layer(PAGE, LayerKind::Page),
        dialog
            .layer(DIALOG, LayerKind::Alert)
            .initial_focus("cancel"),
    ];
    let none = [
        s.layer(PAGE, LayerKind::Page),
        dialog.layer(DIALOG, LayerKind::Dialog),
    ];
    let alone = Frame { layers: &alone };
    let with = Frame { layers: &with };
    let mut d = Driver::new();
    d.tab(&alone);
    d.tab(&alone);
    d.ix.sync(&with);
    assert_eq!(d.focused(), "cancel");
    assert!(d.ix.focus_visible().is_some());
    d.ix.sync(&alone);
    assert_eq!(d.focused(), "b");
    d.ix.sync(&Frame { layers: &none });
    assert_eq!(d.focused(), "", "this dialog starts on nothing");
    d.ix.sync(&alone);
    assert_eq!(d.focused(), "b");
}

#[test]
fn s13_focus_survives_a_rebuild_by_key_or_goes_to_the_neighbour() {
    let s = page();
    let layers = [s.layer(PAGE, LayerKind::Page)];
    let mut d = Driver::new();
    d.tab(&Frame { layers: &layers });
    d.tab(&Frame { layers: &layers });
    let moved = Screen::new(vec![
        ("new", button("New", 0)),
        ("a", button("A", 1)),
        ("b", button("B", 2)),
    ]);
    let layers = [moved.layer(PAGE, LayerKind::Page)];
    d.ix.sync(&Frame { layers: &layers });
    assert_eq!(d.focused(), "b");
    let node = d.ix.focus().unwrap().node;
    assert_eq!(moved.tree.key(node), Some("b"), "the id follows the tree");
    let gone = Screen::new(vec![("a", button("A", 0)), ("c", button("C", 1))]);
    let layers = [gone.layer(PAGE, LayerKind::Page)];
    d.ix.sync(&Frame { layers: &layers });
    assert_eq!(d.focused(), "a", "the nearest earlier one");
    // Another route: nothing.
    let layers = [gone.layer(DIALOG, LayerKind::Page)];
    d.ix.sync(&Frame { layers: &layers });
    assert_eq!(d.focused(), "");
}

#[test]
fn s13_arrows_move_among_tabs_without_activating() {
    let mut tree = gogo_ui_ir::Tree::new("root", Node::container());
    let root = tree.root();
    let before = tree.add_child(root, "before", button("Before", 0));
    before.unwrap();
    let mut list = Node::container();
    list.a11y.role = Role::TabList;
    let list = tree.add_child(root, "tabs", list).unwrap();
    for (i, key) in ["one", "two", "three"].into_iter().enumerate() {
        let tab = button(key, i as u32 + 1).with_a11y(A11y::control(Role::Tab, key));
        tree.add_child(list, key, tab).unwrap();
    }
    let s = Screen::of(tree);
    let layers = [s.layer(PAGE, LayerKind::Page)];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    d.tab(&frame);
    assert!(d.tap(&frame, Key::ArrowRight).is_empty());
    assert_eq!(
        d.focused(),
        "before",
        "arrows do nothing outside a tab list"
    );
    assert_eq!(d.tab(&frame), "one", "every tab is a Tab stop");
    let walked: Vec<String> = [
        Key::ArrowRight,
        Key::ArrowRight,
        Key::ArrowRight,
        Key::ArrowLeft,
    ]
    .into_iter()
    .map(|k| {
        assert!(d.tap(&frame, k).is_empty());
        d.focused()
    })
    .collect();
    assert_eq!(walked, ["two", "three", "one", "three"]);
}
