//! Text fields the app draws: typing, editing keys, pointer gestures, copy and paste,
//! and what they never do.

mod common;

use std::time::Duration;

use common::*;
use gogo_ui_interact::{
    Config, Frame, Gesture, Input, Intent, Interaction, Key, LayerKind, Modifiers, Motion, Primary,
    TextEdit,
};
use gogo_ui_ir::{A11y, Role};

fn field() -> gogo_ui_ir::Node {
    app_button("", 1).with_a11y(A11y::control(Role::TextInput, "Title"))
}

fn dialog() -> Screen {
    Screen::new(vec![
        ("cancel", app_button("Cancel", 0)),
        ("field", field()),
    ])
}

fn edits(intents: &[Intent]) -> Vec<TextEdit> {
    intents
        .iter()
        .filter_map(|i| match i {
            Intent::Edit { target, edit } => {
                assert_eq!(target.key, "field");
                Some(edit.clone())
            }
            _ => None,
        })
        .collect()
}

fn typed(d: &mut Driver, frame: &Frame<'_>, text: &str) -> Vec<Intent> {
    d.ix.handle(Input::Text(text.to_owned()), Duration::ZERO, frame)
        .intents
}

#[test]
fn only_a_focused_field_takes_text_with_its_case() {
    let screen = dialog();
    let layers = [screen
        .layer(DIALOG, LayerKind::Alert)
        .initial_focus("cancel")];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    d.ix.sync(&frame);
    assert!(typed(&mut d, &frame, "A").is_empty());
    assert!(d.down(&frame, Key::Backspace, NONE).is_empty());
    d.up(&frame, Key::Backspace);

    d.down(&frame, Key::Tab, NONE);
    d.up(&frame, Key::Tab);
    assert_eq!(
        edits(&typed(&mut d, &frame, "Ab É")),
        [TextEdit::Insert("Ab É".into())]
    );
    assert!(typed(&mut d, &frame, "\r\t\u{8}").is_empty());
    assert_eq!(
        edits(&d.down(&frame, Key::Backspace, NONE)),
        [TextEdit::Delete {
            forward: false,
            word: false
        }]
    );
    assert_eq!(
        edits(&d.repeat(&frame, Key::Backspace)),
        [TextEdit::Delete {
            forward: false,
            word: false
        }]
    );
    d.up(&frame, Key::Backspace);
}

#[test]
fn enter_and_space_never_activate_a_field() {
    let screen = dialog();
    let layers = [screen
        .layer(DIALOG, LayerKind::Alert)
        .initial_focus("field")];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    d.ix.sync(&frame);
    for key in [Key::Enter, Key::Space] {
        assert!(d.down(&frame, key, NONE).is_empty(), "{key:?}");
        assert!(d.up(&frame, key).is_empty(), "{key:?}");
    }
    let at = screen.center("field");
    // A click only places the caret.
    let clicked = d.click(&frame, at);
    assert!(matches!(
        edits(&clicked)[..],
        [TextEdit::Point {
            gesture: Gesture::Place,
            ..
        }]
    ));
    assert_eq!(clicked.len(), 1);
}

#[test]
fn paste_acts_in_a_focused_field_of_an_alert() {
    let screen = dialog();
    let layers = [screen
        .layer(DIALOG, LayerKind::Alert)
        .initial_focus("cancel")];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    d.ix.sync(&frame);
    assert!(d.down(&frame, Key::Char('v'), CTRL).is_empty());
    d.up(&frame, Key::Char('v'));
    d.down(&frame, Key::Tab, NONE);
    d.up(&frame, Key::Tab);
    assert_eq!(names(&d.down(&frame, Key::Char('v'), CTRL)), ["paste"]);
}

/// A dialog with its field focused.
fn focused(d: &mut Driver, frame: &Frame<'_>) {
    d.ix.sync(frame);
    d.down(frame, Key::Tab, NONE);
    d.up(frame, Key::Tab);
    assert_eq!(d.focused(), "field");
}

fn press(d: &mut Driver, frame: &Frame<'_>, key: Key, mods: Modifiers) -> Vec<Intent> {
    let out = d.down(frame, key, mods);
    d.up(frame, key);
    out
}

fn mods(spec: &str) -> Modifiers {
    Modifiers {
        ctrl: spec.contains('c'),
        shift: spec.contains('s'),
        alt: spec.contains('a'),
        meta: spec.contains('m'),
    }
}

fn moves(motion: Motion, extend: bool) -> TextEdit {
    TextEdit::Move { motion, extend }
}

fn deletes(forward: bool, word: bool) -> TextEdit {
    TextEdit::Delete { forward, word }
}

#[test]
fn the_editing_keys_of_each_platform() {
    let screen = dialog();
    let layers = [screen
        .layer(DIALOG, LayerKind::Alert)
        .initial_focus("cancel")];
    let frame = Frame { layers: &layers };
    use Motion::{End, Left, Right, Start, WordLeft, WordRight};
    let both = [
        (Key::ArrowLeft, "", moves(Left, false)),
        (Key::ArrowRight, "s", moves(Right, true)),
        (Key::Home, "", moves(Start, false)),
        (Key::End, "s", moves(End, true)),
        (Key::Backspace, "", deletes(false, false)),
        (Key::Delete, "", deletes(true, false)),
    ];
    let linux = [
        (Key::ArrowLeft, "c", moves(WordLeft, false)),
        (Key::ArrowRight, "cs", moves(WordRight, true)),
        (Key::Backspace, "c", deletes(false, true)),
        (Key::Delete, "c", deletes(true, true)),
        (Key::Char('a'), "c", TextEdit::SelectAll),
        (Key::Char('z'), "c", TextEdit::Undo),
        (Key::Char('Z'), "cs", TextEdit::Redo),
        (Key::Char('y'), "c", TextEdit::Redo),
    ];
    let mac = [
        (Key::ArrowLeft, "a", moves(WordLeft, false)),
        (Key::ArrowRight, "as", moves(WordRight, true)),
        (Key::ArrowLeft, "m", moves(Start, false)),
        (Key::ArrowRight, "ms", moves(End, true)),
        (Key::Backspace, "a", deletes(false, true)),
        (Key::Delete, "a", deletes(true, true)),
        (Key::Char('a'), "m", TextEdit::SelectAll),
        (Key::Char('z'), "m", TextEdit::Undo),
        (Key::Char('z'), "ms", TextEdit::Redo),
    ];
    for (primary, own, copy) in [
        (Primary::Ctrl, &linux[..], "c"),
        (Primary::Meta, &mac[..], "m"),
    ] {
        let mut d = Driver::new();
        d.ix = Interaction::new(Config::new(primary));
        focused(&mut d, &frame);
        for (key, held, want) in both.iter().chain(own) {
            let got = press(&mut d, &frame, *key, mods(held));
            assert_eq!(
                edits(&got),
                std::slice::from_ref(want),
                "{primary:?} {key:?} {held}"
            );
        }
        for (c, name) in [('c', "copy"), ('x', "cut")] {
            let got = press(&mut d, &frame, Key::Char(c), mods(copy));
            assert_eq!(names(&got), [name]);
        }
        // Up and Down are left for a list under the field.
        for key in [Key::ArrowUp, Key::ArrowDown] {
            assert!(press(&mut d, &frame, key, NONE).is_empty());
        }
    }
}

#[test]
fn copy_is_a_press_and_only_a_focused_field_gives_it() {
    let screen = dialog();
    let layers = [screen
        .layer(DIALOG, LayerKind::Alert)
        .initial_focus("cancel")];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    d.ix.sync(&frame);
    for key in [Key::Char('c'), Key::Char('x'), Key::Char('a')] {
        assert!(press(&mut d, &frame, key, CTRL).is_empty(), "{key:?}");
    }
    focused(&mut d, &frame);
    let before = d.ix.latest();
    let got = d.down(&frame, Key::Char('c'), CTRL);
    let [Intent::Copy { provenance, .. }] = &got[..] else {
        panic!("{got:?}");
    };
    assert!(provenance.event() > before);
    assert!(d.repeat(&frame, Key::Char('c')).is_empty());
}

#[test]
fn clicks_place_the_caret_select_and_drag() {
    let screen = dialog();
    let layers = [screen
        .layer(DIALOG, LayerKind::Alert)
        .initial_focus("cancel")];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    d.ix.sync(&frame);
    let at = screen.center("field");
    let gestures = |intents: &[Intent]| -> Vec<Gesture> {
        edits(intents)
            .into_iter()
            .filter_map(|e| match e {
                TextEdit::Point { gesture, .. } => Some(gesture),
                _ => None,
            })
            .collect()
    };
    let mut got = Vec::new();
    for _ in 0..4 {
        got.extend(gestures(&d.click(&frame, at)));
    }
    use Gesture::{All, Extend, Place, Word};
    assert_eq!(got, [Place, Word, All, Place]);
    assert_eq!(d.focused(), "field");

    // Far enough away to be a click of its own; then a drag, even out of the field.
    let left = (at.0 - 20.0, at.1);
    assert_eq!(gestures(&d.press_at(&frame, left)), [Place]);
    let away = Input::PointerMoved { x: 0.0, y: 0.0 };
    let dragged = d.ix.handle(away.clone(), Duration::ZERO, &frame).intents;
    assert_eq!(gestures(&dragged), [Extend]);
    d.release_at(&frame, (0.0, 0.0));
    assert!(d.ix.handle(away, Duration::ZERO, &frame).intents.is_empty());

    d.ix.handle(Input::Modifiers(SHIFT), Duration::from_secs(9), &frame);
    assert_eq!(gestures(&d.click(&frame, at)), [Extend]);
    // Coordinates are from the field's box.
    let got = d.press_at(&frame, left);
    let [TextEdit::Point { x, y, .. }] = edits(&got)[..] else {
        panic!("{got:?}");
    };
    assert!(x >= 0.0 && y >= 0.0 && x < at.0, "{x} {y}");
}

#[test]
fn what_is_typed_is_never_printed() {
    let printed = format!(
        "{:?} {:?} {:?} {:?}",
        Input::Text("hush".into()),
        Input::KeyDown {
            key: Key::Char('h'),
            mods: NONE,
            repeat: false
        },
        TextEdit::Insert("hush".into()),
        gogo_ui_interact::RequestKind::SetValue("hush".into()),
    );
    assert!(
        !printed.contains("hush") && !printed.contains("'h'"),
        "{printed}"
    );
}

#[test]
fn a_value_assistive_technology_sets_is_not_printed() {
    let screen = dialog();
    let layers = [screen
        .layer(DIALOG, LayerKind::Alert)
        .initial_focus("cancel")];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    d.ix.sync(&frame);
    let got = d.assist(
        &frame,
        DIALOG,
        "field",
        gogo_ui_interact::RequestKind::SetValue("hush".into()),
    );
    assert!(matches!(got[..], [Intent::SetValue { .. }]));
    let printed = format!("{got:?}");
    assert!(
        printed.contains("bytes: 4") && !printed.contains("hush"),
        "{printed}"
    );
}
