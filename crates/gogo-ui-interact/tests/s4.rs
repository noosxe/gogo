//! S4: actions fire only from user input, one per input (design 26 §7, design 25 §4).

mod common;

use std::time::Duration;

use common::*;
use gogo_ui_interact::{Frame, Input, Intent, Key, LayerKind, PointerButton, Request, RequestKind};
use gogo_ui_ir::{A11y, Node, Role};

fn page() -> Screen {
    Screen::new(vec![("a", button("A", 0)), ("b", button("B", 1))])
}

fn event(intents: &[Intent]) -> u64 {
    match intents {
        [Intent::Invoke { provenance, .. }] => provenance.event().get(),
        other => panic!("one invoke expected: {other:?}"),
    }
}

#[test]
fn s4_loading_hovering_focusing_and_frames_do_nothing() {
    let s = page();
    let layers = [s.layer(PAGE, LayerKind::Page).initial_focus("a")];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    assert!(d.ix.sync(&frame).intents.is_empty());
    let (x, y) = s.center("a");
    for input in [
        Input::PointerMoved { x, y },
        Input::PointerLeft,
        Input::FocusLost,
    ] {
        assert!(d.send(&frame, input).intents.is_empty());
    }
    assert!(d.assist(&frame, PAGE, "a", RequestKind::Focus).is_empty());
    assert!(d.tap(&frame, Key::Tab).is_empty());
    assert!(d.tap(&frame, Key::ArrowRight).is_empty());
    assert_eq!(d.ix.latest().get(), 0, "none of these is a press");
}

#[test]
fn s4_one_intent_per_press_numbered_in_order() {
    let s = page();
    let layers = [s.layer(PAGE, LayerKind::Page)];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    let first = event(&d.click(&frame, s.center("a")));
    assert_eq!(d.focused(), "a");
    let second = event(&d.tap(&frame, Key::Enter));
    let third = event(&d.tap(&frame, Key::Space));
    let fourth = event(&d.assist(&frame, PAGE, "b", RequestKind::Activate));
    assert_eq!([first, second, third, fourth], [1, 2, 3, 4]);
    assert_eq!(d.ix.latest().get(), 4);
}

#[test]
fn s4_pointer_and_space_act_on_release_enter_on_the_way_down() {
    let s = page();
    let layers = [s.layer(PAGE, LayerKind::Page)];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    assert!(d.press_at(&frame, s.center("a")).is_empty());
    let down_at = d.now;
    d.now += Duration::from_secs(5);
    match &d.release_at(&frame, s.center("a"))[..] {
        [Intent::Invoke { provenance, .. }] => {
            assert_eq!(provenance.at(), down_at, "the time it went down");
            assert!(!provenance.repeat());
        }
        other => panic!("{other:?}"),
    }
    assert!(d.down(&frame, Key::Space, NONE).is_empty());
    assert_eq!(names(&d.up(&frame, Key::Space)), ["invoke a"]);
    assert_eq!(names(&d.down(&frame, Key::Enter, NONE)), ["invoke a"]);
    assert!(d.up(&frame, Key::Enter).is_empty());
}

#[test]
fn s4_a_number_is_taken_on_the_way_down() {
    let s = page();
    let layers = [s.layer(PAGE, LayerKind::Page)];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    d.press_at(&frame, s.center("a"));
    // What an app records when it draws a screen between the down and the up.
    let latest = d.ix.latest().get();
    assert_eq!(event(&d.release_at(&frame, s.center("a"))), latest);
}

#[test]
fn s4_key_repeat_is_never_a_press() {
    let s = page();
    let layers = [s.layer(PAGE, LayerKind::Page)];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    d.tab(&frame);
    assert_eq!(d.down(&frame, Key::Enter, NONE).len(), 1);
    let latest = d.ix.latest();
    assert!(d.repeat(&frame, Key::Enter).is_empty());
    // Not flagged, but the key never came up.
    assert!(d.down(&frame, Key::Enter, NONE).is_empty());
    assert_eq!(d.ix.latest(), latest);
    // Flagged although no down was seen: held when the window got focus.
    assert!(d.repeat(&frame, Key::Space).is_empty());
    assert!(d.up(&frame, Key::Space).is_empty());
}

#[test]
fn s4_a_release_with_no_press_does_nothing() {
    let s = page();
    let layers = [s.layer(PAGE, LayerKind::Page)];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    d.tab(&frame);
    assert!(d.up(&frame, Key::Space).is_empty());
    assert!(d.release_at(&frame, s.center("a")).is_empty());
    d.down(&frame, Key::Space, NONE);
    d.send(&frame, Input::FocusLost);
    assert!(d.up(&frame, Key::Space).is_empty());
}

#[test]
fn s4_dragging_off_or_another_button_does_nothing() {
    let s = page();
    let layers = [s.layer(PAGE, LayerKind::Page)];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    d.press_at(&frame, s.center("a"));
    assert!(d.release_at(&frame, s.center("b")).is_empty());
    d.press_at(&frame, (700.0, 500.0));
    assert!(d.release_at(&frame, s.center("a")).is_empty());
    let (x, y) = s.center("a");
    for button in [PointerButton::Secondary, PointerButton::Other] {
        d.send(&frame, Input::PointerDown { button, x, y });
        let up = d.send(&frame, Input::PointerUp { button, x, y });
        assert!(up.intents.is_empty());
    }
}

/// Presses `a` on the first screen and releases on the second.
fn across(before: &Frame<'_>, after: &Frame<'_>, at: (f64, f64)) -> [Vec<String>; 2] {
    let mut d = Driver::new();
    d.press_at(before, at);
    let pointer = names(&d.release_at(after, at));
    let mut d = Driver::new();
    d.tab(before);
    d.down(before, Key::Space, NONE);
    let key = names(&d.up(after, Key::Space));
    [pointer, key]
}

#[test]
fn s4_a_press_is_dropped_when_its_control_changes() {
    let s = page();
    let at = s.center("a");
    let one = [1, 2];
    let other = [9, 2];
    let before = [s.layer(PAGE, LayerKind::Page).identity(&one)];
    let same = [s.layer(PAGE, LayerKind::Page).identity(&one)];
    let reordered = [s.layer(PAGE, LayerKind::Page).identity(&other)];
    let another_screen = [s.layer(DIALOG, LayerKind::Page).identity(&one)];
    let before = Frame { layers: &before };
    let [p, k] = across(&before, &Frame { layers: &same }, at);
    assert_eq!((p.len(), k.len()), (1, 1));
    for after in [&reordered[..], &another_screen[..], &[]] {
        let [p, k] = across(&before, &Frame { layers: after }, at);
        assert!(p.is_empty() && k.is_empty(), "{p:?} {k:?}");
    }
}

#[test]
fn s4_a_switch_passes_its_state_and_a_flip_drops_the_press() {
    let switch = |on: bool| {
        let mut a11y = A11y::control(Role::Switch, "Galaxy");
        a11y.checked = Some(on);
        Screen::new(vec![("t", button("", 0).with_a11y(a11y))])
    };
    let (off, on) = (switch(false), switch(true));
    let off_layers = [off.layer(PAGE, LayerKind::Page)];
    let on_layers = [on.layer(PAGE, LayerKind::Page)];
    let mut d = Driver::new();
    match &d.click(&Frame { layers: &on_layers }, on.center("t"))[..] {
        [Intent::Invoke { input, .. }] => assert_eq!(input.on, Some(true)),
        other => panic!("{other:?}"),
    }
    let [p, k] = across(
        &Frame {
            layers: &off_layers,
        },
        &Frame { layers: &on_layers },
        off.center("t"),
    );
    assert!(p.is_empty() && k.is_empty());
}

#[test]
fn s4_a_disabled_control_does_not_activate() {
    let mut disabled = button("Off", 0);
    disabled.a11y.disabled = true;
    let s = Screen::new(vec![("off", disabled)]);
    let layers = [s.layer(PAGE, LayerKind::Page)];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    assert!(d.click(&frame, s.center("off")).is_empty());
    assert!(
        d.assist(&frame, PAGE, "off", RequestKind::Activate)
            .is_empty()
    );
}

#[test]
fn s4_a_menu_button_and_a_select_open_and_run_nothing() {
    let mut menu = Node::container();
    menu.a11y = A11y::control(Role::MenuButton, "More");
    menu.layout = button("x", 0).layout;
    let select = button("", 1).with_a11y(A11y::control(Role::ComboBox, "Renderer"));
    let s = Screen::new(vec![("menu", menu), ("select", select)]);
    let layers = [s.layer(PAGE, LayerKind::Page)];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    for key in ["menu", "select"] {
        let opened = [format!("open {key}")];
        assert_eq!(names(&d.click(&frame, s.center(key))), opened);
        assert_eq!(names(&d.tap(&frame, Key::Enter)), opened);
        assert_eq!(names(&d.tap(&frame, Key::Space)), opened);
        assert_eq!(
            names(&d.assist(&frame, PAGE, key, RequestKind::Activate)),
            opened
        );
    }
}

#[test]
fn s4_set_value_carries_no_press() {
    let mut field = app_button("Title", 0);
    field.a11y.role = Role::TextInput;
    let s = Screen::new(vec![("field", field)]);
    let layers = [s.layer(DIALOG, LayerKind::Dialog)];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    let set = d.assist(
        &frame,
        DIALOG,
        "field",
        RequestKind::SetValue("Doom".into()),
    );
    assert_eq!(names(&set), ["set field Doom"]);
    assert_eq!(d.ix.latest().get(), 0);
    assert_eq!(d.focused(), "field");
}

#[test]
fn s4_a_late_request_for_a_changed_control_is_dropped() {
    let s = page();
    let (one, other) = ([1, 2], [9, 2]);
    let before = [s.layer(PAGE, LayerKind::Page).identity(&one)];
    let after = [s.layer(PAGE, LayerKind::Page).identity(&other)];
    let (before, after) = (Frame { layers: &before }, Frame { layers: &after });
    let mut d = Driver::new();
    // Read on the old frame, delivered after the new one.
    let request = Request {
        layer: PAGE,
        key: "a".into(),
        identity: before.layers[0].action_identity(s.tree.by_key("a").unwrap()),
        kind: RequestKind::Activate,
    };
    d.ix.sync(&after);
    let out = d.send(&after, Input::Accessibility(request.clone()));
    assert!(out.intents.is_empty());
    assert_eq!(d.ix.latest().get(), 0);
    let out = d.send(&before, Input::Accessibility(request));
    assert_eq!(names(&out.intents), ["invoke a"]);
}
