//! Text fields the app draws: typing, Backspace and paste, and what they never do.

mod common;

use std::time::Duration;

use common::*;
use gogo_ui_interact::{Frame, Input, Intent, Key, LayerKind, TextEdit};
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
        [TextEdit::Backspace]
    );
    assert_eq!(
        edits(&d.repeat(&frame, Key::Backspace)),
        [TextEdit::Backspace]
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
    assert!(d.click(&frame, at).is_empty());
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
