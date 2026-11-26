//! The paste shortcut: under a dialog only, once per press.

mod common;

use common::*;
use gogo_ui_interact::{Frame, Key, LayerKind};

#[test]
fn the_paste_key_acts_under_a_dialog_only() {
    let page = Screen::new(vec![("play", button("Play", 0))]);
    let dialog = Screen::new(vec![("cancel", app_button("Cancel", 0))]);
    let mut d = Driver::new();

    let layers = [page.layer(PAGE, LayerKind::Page)];
    let frame = Frame { layers: &layers };
    assert!(d.down(&frame, Key::Char('v'), CTRL).is_empty());
    d.up(&frame, Key::Char('v'));

    // An error screen is not asking for anything.
    let layers = [
        page.layer(PAGE, LayerKind::Page),
        dialog.layer(DIALOG, LayerKind::Alert),
    ];
    let frame = Frame { layers: &layers };
    assert!(d.down(&frame, Key::Char('v'), CTRL).is_empty());
    d.up(&frame, Key::Char('v'));

    let layers = [
        page.layer(PAGE, LayerKind::Page),
        dialog.layer(DIALOG, LayerKind::Dialog),
    ];
    let frame = Frame { layers: &layers };
    assert!(d.down(&frame, Key::Char('v'), NONE).is_empty());
    d.up(&frame, Key::Char('v'));
    assert!(d.down(&frame, Key::Char('v'), SHIFT).is_empty());
    d.up(&frame, Key::Char('v'));
    assert_eq!(names(&d.down(&frame, Key::Char('v'), CTRL)), ["paste"]);
    assert!(d.repeat(&frame, Key::Char('v')).is_empty());
}
