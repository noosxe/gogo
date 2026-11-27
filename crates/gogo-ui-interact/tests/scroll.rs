//! Scrolling (design 25 §4), and design 26 §7 S22: input reaches only what is seen.

mod common;

use common::{Driver, NONE, PAGE, app_button, button, names};
use gogo_ui_interact::{
    Frame, Identity, Input, Intent, Key, Layer, LayerId, LayerKind, Motion, PointerButton, Request,
    RequestKind, SCROLL_LINE, TextEdit,
};
use gogo_ui_ir::{A11y, Dim, Direction, LayoutStyle, Node, Role, ScrollSpec, Tree, VirtualList};
use gogo_ui_layout::{Laid, Scroll, ScrollBox, Viewport, layout};
use gogo_ui_text::FontSet;

/// A header button, then ten 40 px buttons `b0` to `b9` in a container 100 px high,
/// 40 px down the window.
struct Page {
    tree: Tree,
    laid: Laid,
}

fn tree_of(buttons: impl Fn(usize) -> Node) -> Tree {
    let mut style = LayoutStyle::default();
    style.width = Dim::Px(800.0);
    style.height = Dim::Px(600.0);
    style.direction = Direction::Column;
    let mut tree = Tree::new("root", Node::container().with_layout(style));
    let root = tree.root();
    tree.add_child(root, "top", button("top", 20)).expect("add");
    let mut list = Node::container();
    list.layout.width = Dim::Px(200.0);
    list.layout.height = Dim::Px(100.0);
    list.layout.shrink = 0.0;
    list.scroll = Some(ScrollSpec::default());
    let list = tree.add_child(root, "list", list).expect("add");
    for i in 0..10 {
        tree.add_child(list, format!("b{i}"), buttons(i))
            .expect("add");
    }
    tree
}

const IDENTITY: [u64; 21] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
];

fn plain(i: usize) -> Node {
    button(&format!("b{i}"), i as u32)
}

/// A frame as the app makes one: laid out with the offsets the interaction holds.
fn page(d: &Driver, tree: &Tree) -> Page {
    let mut scroll = Scroll::none();
    for (key, y) in d.ix.offsets(PAGE) {
        scroll.set(tree.by_key(key).expect(key), 0.0, y);
    }
    let laid = layout(
        tree,
        &mut FontSet::bundled(),
        Viewport::new(800.0, 600.0),
        &scroll,
    )
    .expect("layout");
    Page {
        tree: tree.clone(),
        laid,
    }
}

impl Page {
    fn layer(&self) -> Layer<'_> {
        Layer::new(PAGE, LayerKind::Page, &self.tree, &self.laid).identity(&IDENTITY)
    }

    fn list(&self) -> ScrollBox {
        let list = self.tree.by_key("list").expect("list");
        *self.laid.scroll_box(list).expect("a box")
    }

    fn offset(&self) -> f64 {
        self.list().offset
    }

    fn seen(&self, key: &str) -> f64 {
        let node = self.tree.by_key(key).expect(key);
        self.laid.get(node).expect("placed").visible.height()
    }
}

/// Runs `f` on the frame of `p`.
fn on<T>(p: &Page, f: impl FnOnce(&Frame<'_>) -> T) -> T {
    let layers = [p.layer()];
    f(&Frame { layers: &layers })
}

const IN_LIST: (f64, f64) = (50.0, 60.0);

fn wheel(d: &mut Driver, p: &Page, at: (f64, f64), dy: f64) -> bool {
    on(p, |f| {
        d.send(
            f,
            Input::Wheel {
                x: at.0,
                y: at.1,
                dy,
            },
        )
        .redraw
    })
}

#[test]
fn the_wheel_scrolls_under_the_pointer_and_stops_at_the_ends() {
    let tree = tree_of(plain);
    let mut d = Driver::new();
    let p = page(&d, &tree);
    assert!(
        wheel(&mut d, &p, IN_LIST, 60.0),
        "a scroll asks for a frame"
    );
    // Wheels before the next frame add up.
    wheel(&mut d, &p, IN_LIST, 10.0);
    assert_eq!(page(&d, &tree).offset(), 70.0);
    wheel(&mut d, &p, IN_LIST, 10_000.0);
    let end = page(&d, &tree);
    assert_eq!(end.offset(), 300.0);
    assert!(!wheel(&mut d, &end, IN_LIST, 10.0));
    wheel(&mut d, &end, IN_LIST, -10_000.0);
    assert_eq!(page(&d, &tree).offset(), 0.0);
    // Outside the container there is nothing to move.
    let p = page(&d, &tree);
    assert!(!wheel(&mut d, &p, (500.0, 500.0), 60.0));
    assert_eq!(d.ix.latest(), Default::default(), "no input numbers");
}

#[test]
fn what_is_scrolled_out_of_sight_takes_no_press() {
    let tree = tree_of(plain);
    let mut d = Driver::new();
    let p = page(&d, &tree);
    wheel(&mut d, &p, IN_LIST, 100.0);
    let p = page(&d, &tree);
    assert_eq!(p.seen("b0"), 0.0);
    // Where b0 was before the header: the header, not what scrolled up past it.
    let clicked = on(&p, |f| d.click(f, (50.0, 20.0)));
    assert_eq!(names(&clicked), ["invoke top"]);
    // Below the container, where later buttons would be.
    assert!(on(&p, |f| d.click(f, (50.0, 160.0))).is_empty());
}

#[test]
fn a_scroll_cancels_the_press_inside_it() {
    let tree = tree_of(plain);
    let mut d = Driver::new();
    let p = page(&d, &tree);
    on(&p, |f| d.press_at(f, IN_LIST));
    // So little that the same button stays under the pointer.
    wheel(&mut d, &p, IN_LIST, 5.0);
    let p = page(&d, &tree);
    assert!(on(&p, |f| d.release_at(f, IN_LIST)).is_empty());
    // A press outside what moved is left alone.
    on(&p, |f| d.press_at(f, (50.0, 20.0)));
    wheel(&mut d, &p, IN_LIST, 5.0);
    let p = page(&d, &tree);
    let released = on(&p, |f| d.release_at(f, (50.0, 20.0)));
    assert_eq!(names(&released), ["invoke top"]);
}

#[test]
fn a_focused_control_scrolled_away_is_brought_back_before_it_acts() {
    let tree = tree_of(plain);
    let mut d = Driver::new();
    let p = page(&d, &tree);
    assert_eq!(on(&p, |f| d.tab(f)), "top");
    assert_eq!(on(&p, |f| d.tab(f)), "b0");
    wheel(&mut d, &p, IN_LIST, 200.0);
    for key in [Key::Enter, Key::Space] {
        let p = page(&d, &tree);
        if p.seen("b0") == 0.0 {
            assert!(on(&p, |f| d.tap(f, key)).is_empty(), "{key:?}");
        }
        let p = page(&d, &tree);
        assert_eq!(p.offset(), 0.0);
        assert_eq!(names(&on(&p, |f| d.tap(f, key))), ["invoke b0"]);
        wheel(&mut d, &p, IN_LIST, 200.0);
    }
}

#[test]
fn moving_focus_brings_the_control_into_view() {
    let tree = tree_of(plain);
    let mut d = Driver::new();
    for i in 0..12 {
        let p = page(&d, &tree);
        let focus = on(&p, |f| d.tab(f));
        let p = page(&d, &tree);
        if i > 0 && focus != "top" {
            assert_eq!(p.seen(&focus), 40.0, "{focus} is wholly in view");
        }
    }
    // Around to the top of the order and in again: the list shows b0.
    let p = page(&d, &tree);
    assert_eq!(on(&p, |f| d.tab(f)), "b0");
    assert_eq!(page(&d, &tree).offset(), 0.0);
    // And backwards, from the first control to the last.
    let p = page(&d, &tree);
    let last = on(&p, |f| {
        d.back_tab(f);
        d.back_tab(f)
    });
    assert_eq!(last, "b9");
    assert_eq!(page(&d, &tree).offset(), 300.0);
}

#[test]
fn a_press_does_not_scroll_a_half_seen_control() {
    let tree = tree_of(plain);
    let mut d = Driver::new();
    let p = page(&d, &tree);
    // b2 shows its upper half, from y 120 to 140.
    assert_eq!(p.seen("b2"), 20.0);
    let clicked = on(&p, |f| d.click(f, (50.0, 130.0)));
    assert_eq!(names(&clicked), ["invoke b2"]);
    assert_eq!(page(&d, &tree).offset(), 0.0);
}

#[test]
fn a_hold_ends_when_its_control_scrolls_away_and_is_confirmed_only_in_sight() {
    let tree = tree_of(plain);
    let mut d = Driver::new();
    let held: &[&str] = &["b0"];
    let hold = |p: &Page, d: &mut Driver, input: Input| {
        let layers = [p.layer().hold(held)];
        names(&d.send(&Frame { layers: &layers }, input).intents)
    };
    let p = page(&d, &tree);
    on(&p, |f| {
        d.tab(f);
        d.tab(f)
    });
    let down = Input::KeyDown {
        key: Key::Space,
        mods: NONE,
        repeat: false,
    };
    assert_eq!(hold(&p, &mut d, down.clone()), ["hold b0"]);
    let scrolled = Input::Wheel {
        x: IN_LIST.0,
        y: IN_LIST.1,
        dy: 200.0,
    };
    assert_eq!(hold(&p, &mut d, scrolled), ["end b0"]);
    hold(&p, &mut d, Input::KeyUp { key: Key::Space });
    // Out of sight, Space brings it back and starts nothing.
    let away = page(&d, &tree);
    assert!(hold(&away, &mut d, down.clone()).is_empty());
    hold(&away, &mut d, Input::KeyUp { key: Key::Space });
    assert_eq!(page(&d, &tree).offset(), 0.0);

    // The same for a screen reader: it does not arm what is out of sight.
    let p = page(&d, &tree);
    wheel(&mut d, &p, IN_LIST, 200.0);
    let away = page(&d, &tree);
    let activate = Input::Accessibility(Request {
        layer: PAGE,
        key: "b0".to_owned(),
        identity: Some(Identity::Known(0)),
        kind: RequestKind::Activate,
    });
    assert!(hold(&away, &mut d, activate.clone()).is_empty());
    let back = page(&d, &tree);
    assert_eq!(back.offset(), 0.0);
    assert_eq!(hold(&back, &mut d, activate), ["armed b0"]);
}

#[test]
fn a_screen_reader_activates_what_it_read_and_it_comes_into_view() {
    let tree = tree_of(plain);
    let mut d = Driver::new();
    let p = page(&d, &tree);
    let acted = on(&p, |f| d.assist(f, PAGE, "b8", RequestKind::Activate));
    assert_eq!(names(&acted), ["invoke b8"]);
    assert_eq!(page(&d, &tree).seen("b8"), 40.0);
}

#[test]
fn a_shortcut_fires_only_while_its_control_is_seen() {
    let tree = tree_of(|i| {
        let mut n = plain(i);
        if i == 0 {
            n.a11y.key = Some("P".to_owned());
        }
        n
    });
    let mut d = Driver::new();
    let p = page(&d, &tree);
    assert_eq!(names(&on(&p, |f| d.tap(f, Key::Char('p')))), ["invoke b0"]);
    wheel(&mut d, &p, IN_LIST, 200.0);
    let p = page(&d, &tree);
    assert!(on(&p, |f| d.tap(f, Key::Char('p'))).is_empty());
}

#[test]
fn an_app_control_must_be_wholly_in_view() {
    let tree = tree_of(|i| app_button(&format!("b{i}"), i as u32));
    let mut d = Driver::new();
    let p = page(&d, &tree);
    on(&p, |f| d.assist(f, PAGE, "b1", RequestKind::Focus));
    // b1 covers 80 to 120; 30 px on, its centre is still seen but not all of it.
    let p = page(&d, &tree);
    wheel(&mut d, &p, IN_LIST, 50.0);
    let p = page(&d, &tree);
    assert_eq!(p.seen("b1"), 30.0);
    assert!(on(&p, |f| d.tap(f, Key::Enter)).is_empty());
    let p = page(&d, &tree);
    assert_eq!(p.seen("b1"), 40.0);
    assert!(matches!(
        on(&p, |f| d.tap(f, Key::Enter)).as_slice(),
        [Intent::App { .. }]
    ));
}

#[test]
fn the_thumb_drags_and_beside_it_turns_a_page() {
    let tree = tree_of(plain);
    let mut d = Driver::new();
    let p = page(&d, &tree);
    let bar = p.list();
    let x = (bar.track.x0 + bar.track.x1) / 2.0;
    let grab = bar.thumb.y0 + 5.0;
    on(&p, |f| d.press_at(f, (x, grab)));
    // Half of the way the thumb can travel.
    let half = (bar.track.height() - bar.thumb.height()) / 2.0;
    on(&p, |f| {
        d.send(
            f,
            Input::PointerMoved {
                x: 0.0,
                y: grab + half,
            },
        )
    });
    let moved = page(&d, &tree);
    assert!((moved.offset() - 150.0).abs() < 0.01, "{}", moved.offset());
    assert!(on(&moved, |f| d.release_at(f, IN_LIST)).is_empty());
    // Released: moving no longer scrolls.
    on(&moved, |f| {
        d.send(f, Input::PointerMoved { x: 0.0, y: 500.0 })
    });
    assert_eq!(page(&d, &tree).offset(), moved.offset());
    // Below the thumb, a page on.
    let below = (x, moved.list().track.y1 - 1.0);
    on(&moved, |f| d.click(f, below));
    let paged = page(&d, &tree).offset();
    assert!((paged - moved.offset() - 90.0).abs() < 0.01, "{paged}");
    assert_eq!(d.ix.latest(), Default::default(), "no input numbers");
    assert!(d.ix.hovered().is_none() && d.ix.pressed().is_none());
}

#[test]
fn the_scroll_keys_move_the_focused_container_else_the_largest() {
    let tree = tree_of(plain);
    let mut d = Driver::new();
    for (key, want) in [
        (Key::PageDown, 90.0),
        (Key::ArrowDown, 90.0 + SCROLL_LINE),
        (Key::ArrowUp, 90.0),
        (Key::End, 300.0),
        (Key::PageUp, 210.0),
        (Key::Home, 0.0),
    ] {
        let p = page(&d, &tree);
        on(&p, |f| d.tap(f, key));
        let got = page(&d, &tree).offset();
        assert!((got - want).abs() < 0.01, "{key:?}: {got}");
    }
}

#[test]
fn home_and_end_edit_a_focused_field_and_leave_the_page_where_it_is() {
    let tree = tree_of(|i| {
        let mut n = plain(i);
        if i == 0 {
            n = app_button("name", 1);
            n.a11y = A11y::control(Role::TextInput, "name");
        }
        n
    });
    let mut d = Driver::new();
    let p = page(&d, &tree);
    on(&p, |f| {
        d.tab(f);
        d.tab(f)
    });
    assert_eq!(d.focused(), "b0");
    // Wheeled on, so there is somewhere to go both ways.
    wheel(&mut d, &p, IN_LIST, 20.0);
    for (key, motion) in [(Key::Home, Motion::Start), (Key::End, Motion::End)] {
        let p = page(&d, &tree);
        let intents = on(&p, |f| d.tap(f, key));
        assert!(
            matches!(
                intents.as_slice(),
                [Intent::Edit {
                    edit: TextEdit::Move { motion: m, extend: false },
                    ..
                }] if *m == motion
            ),
            "{key:?}: {intents:?}"
        );
        assert_eq!(page(&d, &tree).offset(), 20.0, "{key:?}");
    }
    // The field has no use for the page keys, and they leave the page alone too.
    for key in [Key::PageUp, Key::PageDown, Key::ArrowUp, Key::ArrowDown] {
        let p = page(&d, &tree);
        assert!(on(&p, |f| d.tap(f, key)).is_empty(), "{key:?}");
        assert_eq!(page(&d, &tree).offset(), 20.0, "{key:?}");
    }
}

#[test]
fn under_a_dialog_the_page_does_not_scroll() {
    let tree = tree_of(plain);
    let mut d = Driver::new();
    let p = page(&d, &tree);
    let dialog = common::Screen::new(vec![("ok", app_button("OK", 1))]);
    let layers = [p.layer(), dialog.layer(LayerId(9), LayerKind::Dialog)];
    let frame = Frame { layers: &layers };
    let (x, y) = IN_LIST;
    d.send(&frame, Input::Wheel { x, y, dy: 60.0 });
    d.tap(&frame, Key::PageDown);
    d.send(
        &frame,
        Input::PointerDown {
            button: PointerButton::Primary,
            x: 196.0,
            y: 100.0,
        },
    );
    assert_eq!(page(&d, &tree).offset(), 0.0);
}

/// A button, an 80 px container of five 40 px buttons and another button, all inside a
/// container 100 px high: both containers scroll.
fn nested_tree() -> Tree {
    let mut style = LayoutStyle::default();
    style.width = Dim::Px(800.0);
    style.height = Dim::Px(600.0);
    style.direction = Direction::Column;
    let mut tree = Tree::new("root", Node::container().with_layout(style));
    let root = tree.root();
    tree.add_child(root, "top", button("top", 20)).expect("add");
    let container = |height: f32| {
        let mut node = Node::container();
        node.layout.width = Dim::Px(200.0);
        node.layout.height = Dim::Px(height);
        node.layout.shrink = 0.0;
        node.scroll = Some(ScrollSpec::default());
        node
    };
    let outer = tree
        .add_child(root, "outer", container(100.0))
        .expect("add");
    tree.add_child(outer, "above", button("above", 10))
        .expect("add");
    let inner = tree
        .add_child(outer, "inner", container(80.0))
        .expect("add");
    for i in 0..5 {
        tree.add_child(inner, format!("b{i}"), plain(i))
            .expect("add");
    }
    tree.add_child(outer, "below", button("below", 11))
        .expect("add");
    tree
}

fn offsets(p: &Page) -> (f64, f64) {
    let at = |key: &str| {
        let node = p.tree.by_key(key).expect(key);
        p.laid.scroll_box(node).expect("a box").offset
    };
    (at("outer"), at("inner"))
}

#[test]
fn the_wheel_goes_on_to_the_outer_container_at_the_inner_ones_end() {
    let tree = nested_tree();
    let mut d = Driver::new();
    let p = page(&d, &tree);
    // Inside the inner container.
    wheel(&mut d, &p, (50.0, 100.0), 1_000.0);
    let p = page(&d, &tree);
    assert_eq!(offsets(&p), (0.0, 120.0), "the inner one first, alone");
    wheel(&mut d, &p, (50.0, 100.0), 1_000.0);
    let p = page(&d, &tree);
    assert_eq!(offsets(&p), (60.0, 120.0));
    // And back the same way: the inner one, now higher up, then the outer.
    wheel(&mut d, &p, (50.0, 60.0), -1_000.0);
    let p = page(&d, &tree);
    assert_eq!(offsets(&p), (60.0, 0.0));
    wheel(&mut d, &p, (50.0, 60.0), -1_000.0);
    assert_eq!(offsets(&page(&d, &tree)), (0.0, 0.0));
}

#[test]
fn a_focus_move_brings_a_control_into_view_through_both_containers() {
    let tree = nested_tree();
    let mut d = Driver::new();
    let p = page(&d, &tree);
    assert_eq!(p.seen("b4"), 0.0);
    on(&p, |f| d.assist(f, PAGE, "b4", RequestKind::Focus));
    let p = page(&d, &tree);
    let (outer, inner) = offsets(&p);
    assert_eq!(inner, 120.0);
    assert!(outer > 0.0 && outer <= 60.0, "{outer}");
    assert_eq!(p.seen("b4"), 40.0, "wholly in view");
}

/// A virtualised list of 100 items 40 px high, of which `bound` are there, in a
/// container 100 px high between two buttons.
fn virtual_tree(bound: &[u32]) -> Tree {
    let mut style = LayoutStyle::default();
    style.width = Dim::Px(800.0);
    style.height = Dim::Px(600.0);
    style.direction = Direction::Column;
    let mut tree = Tree::new("root", Node::container().with_layout(style));
    let root = tree.root();
    tree.add_child(root, "top", button("top", 20)).expect("add");
    let mut scroller = Node::container();
    scroller.layout.width = Dim::Px(200.0);
    scroller.layout.height = Dim::Px(100.0);
    scroller.layout.shrink = 0.0;
    scroller.scroll = Some(ScrollSpec::default());
    let scroller = tree.add_child(root, "list", scroller).expect("add");
    let mut list = Node::container();
    list.layout.direction = Direction::Column;
    list.layout.shrink = 0.0;
    list.list = Some(VirtualList {
        count: 100,
        columns: 1,
        item_h: 40.0,
        gap: 0.0,
        window: 0..3,
    });
    let list = tree.add_child(scroller, "items", list).expect("add");
    let mut next = 0;
    for &i in bound {
        if i > next {
            let mut gap = Node::container();
            gap.layout.height = Dim::Px((i - next) as f32 * 40.0);
            gap.layout.shrink = 0.0;
            tree.add_child(list, format!("gap{next}"), gap)
                .expect("add");
        }
        next = i + 1;
        let mut item = button(&format!("i{i}"), i % 20);
        item.item = Some(i);
        tree.add_child(list, format!("i{i}"), item).expect("add");
    }
    tree
}

#[test]
fn tab_does_not_pass_over_items_that_are_not_there_yet() {
    let tree = virtual_tree(&[0, 1, 2, 99]);
    let mut d = Driver::new();
    let p = page(&d, &tree);
    for want in ["top", "i0", "i1", "i2"] {
        assert_eq!(on(&p, |f| d.tab(f)), want);
    }
    // Items 3 to 98 come before i99: focus waits, and the list moves on.
    let p = page(&d, &tree);
    assert_eq!(on(&p, |f| d.tab(f)), "i2");
    assert!(page(&d, &tree).offset() > 0.0);
    // With the next ones bound, it goes on.
    let more = virtual_tree(&[0, 1, 2, 3, 4, 99]);
    let p = page(&d, &more);
    assert_eq!(on(&p, |f| d.tab(f)), "i3");
}

#[test]
fn tab_enters_a_list_at_either_end() {
    let tree = virtual_tree(&[0, 50, 51, 99]);
    let mut d = Driver::new();
    let p = page(&d, &tree);
    assert_eq!(on(&p, |f| d.tab(f)), "top");
    assert_eq!(on(&p, |f| d.tab(f)), "i0");
    let mut d = Driver::new();
    let p = page(&d, &tree);
    assert_eq!(on(&p, |f| d.back_tab(f)), "i99");
    // Not from i99 back over what is missing.
    let p = page(&d, &tree);
    assert_eq!(on(&p, |f| d.back_tab(f)), "i99");
}

// ---- under a popup ----

#[test]
fn under_a_popup_neither_the_wheel_nor_the_scroll_keys_move_the_page() {
    const POPUP: LayerId = LayerId(10);
    let tree = tree_of(plain);
    let mut field = app_button("Search", 0);
    field.a11y.role = Role::TextInput;
    let mut row = app_button("One", 1);
    row.a11y.role = Role::ListOption;
    let item = |name: &str, part: u32| {
        let mut node = app_button(name, part);
        node.a11y.role = Role::MenuItem;
        node
    };
    // A palette (a field over a list) and a menu.
    let palette = common::Screen::new(vec![("field", field), ("row", row)]);
    let menu = common::Screen::new(vec![("one", item("One", 1)), ("two", item("Two", 2))]);
    let mut d = Driver::new();
    let p = page(&d, &tree);
    for (popup, start, submit) in [(&palette, "field", Some("row")), (&menu, "one", None)] {
        let mut over = popup.layer(POPUP, LayerKind::Popup).initial_focus(start);
        if let Some(key) = submit {
            over = over.submit(key);
        }
        let layers = [p.layer(), over];
        let frame = Frame { layers: &layers };
        d.ix.sync(&frame);
        // Over the page's list, where the popup has nothing.
        d.send(&frame, Input::PointerMoved { x: 100.0, y: 90.0 });
        d.send(
            &frame,
            Input::Wheel {
                x: 100.0,
                y: 90.0,
                dy: 60.0,
            },
        );
        for key in [
            Key::PageDown,
            Key::End,
            Key::ArrowDown,
            Key::ArrowDown,
            Key::PageUp,
            Key::Home,
        ] {
            let out = d.down(&frame, key, NONE);
            d.up(&frame, key);
            if submit.is_some() && key == Key::ArrowDown {
                assert_eq!(names(&out), ["move field Next"]);
            }
        }
        assert!(d.ix.offsets(PAGE).all(|(_, y)| y == 0.0), "{start}");
        d.ix.sync(&Frame {
            layers: &[p.layer()],
        });
    }
}
