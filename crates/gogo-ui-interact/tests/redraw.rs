//! What asks for another frame (design 28 §7): only a change of what a frame draws.

mod common;

use common::{Driver, PAGE, Screen, button};
use gogo_ui_interact::{Frame, Input, LayerKind, PointerButton};

/// Nothing draws the hovered or the pressed control yet. Whoever adds `:hover` or
/// `:pressed` styles puts them back into `Interaction`'s redraw comparison and changes
/// this test.
#[test]
fn hover_and_press_ask_for_no_frame_until_something_draws_them() {
    let screen = Screen::new(vec![("a", button("a", 0))]);
    let layers = [screen.layer(PAGE, LayerKind::Page)];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    // Takes the frame in.
    d.ix.sync(&frame);

    let (x, y) = screen.center("a");
    assert!(!d.send(&frame, Input::PointerMoved { x, y }).redraw);
    assert!(d.ix.hovered().is_some(), "still followed");

    // A press focuses what it is on, and focus is drawn.
    let down = Input::PointerDown {
        button: PointerButton::Primary,
        x,
        y,
    };
    assert!(d.send(&frame, down.clone()).redraw);
    assert!(d.ix.pressed().is_some());
    // The press ending elsewhere, and pressing what has focus already, change nothing
    // that is drawn.
    let off = Input::PointerMoved { x: 700.0, y: 500.0 };
    assert!(!d.send(&frame, off).redraw);
    assert!(d.ix.pressed().is_none());
    let up = Input::PointerUp {
        button: PointerButton::Primary,
        x: 700.0,
        y: 500.0,
    };
    assert!(!d.send(&frame, up).redraw);
    d.send(&frame, Input::PointerMoved { x, y });
    assert!(!d.send(&frame, down).redraw);
}
