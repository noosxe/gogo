//! An idle window does no rendering work: the app driven only through [`ShellApp`], by a
//! stand-in for the shell that draws only when it is asked to, as the shell does.
//!
//! Binding, layout and scene building all happen in [`ShellApp::frame`], so no frame
//! means none of them. As in the shell, only a frame that is shown is followed by
//! [`ShellApp::presented`] and an accessibility update, and all three are counted.

use gogo_contract::Value;
use gogo_gui_model::{DWELL, ErrorView, SourceKey};
use gogo_ui_interact::{Key, Modifiers, PointerButton};
use gogo_ui_shell::accesskit::{Role, TreeUpdate};

use super::safety::{HOLD_LABEL, hold_screen};
use super::tests::{SIZE, app, at, bounds, with_an_unbindable_library};
use super::*;

/// The most frames one thing may take to settle.
const SETTLES: usize = 5;

/// A spot of the window with nothing to press.
const NOTHING: (f64, f64) = (700.0, 780.0);

struct Shell {
    app: App,
    /// A frame is wanted. Requests between two frames are one.
    asked: bool,
    frames: usize,
    presented: usize,
    updates: usize,
    /// The accessibility tree last sent.
    sent: Option<TreeUpdate>,
    ms: u64,
}

impl Shell {
    fn new(app: App) -> Shell {
        let mut shell = Shell {
            app,
            asked: true,
            frames: 0,
            presented: 0,
            updates: 0,
            sent: None,
            ms: 10_000,
        };
        shell.settle();
        shell
    }

    /// Builds one frame, which does not reach the display until it is shown.
    fn build(&mut self) {
        self.asked = false;
        self.ms += 16;
        self.app.frame(SIZE.0, SIZE.1, 1.0);
        self.frames += 1;
    }

    /// Builds one frame and shows it.
    fn draw(&mut self) {
        self.build();
        self.asked |= self.app.presented(at(self.ms), "cpu");
        self.presented += 1;
        self.sent = Some(self.app.accessibility_tree());
        self.updates += 1;
    }

    /// Frames built, frames the app was told are shown, and accessibility updates.
    fn counts(&self) -> [usize; 3] {
        [self.frames, self.presented, self.updates]
    }

    /// Draws until nothing more is asked for, and says how many frames that took.
    fn settle(&mut self) -> usize {
        let before = self.frames;
        while self.asked {
            self.draw();
            assert!(self.frames - before <= SETTLES, "it never settles");
        }
        self.frames - before
    }

    fn input(&mut self, input: Input) {
        self.ms += 1;
        self.asked |= self.app.input(input, at(self.ms));
    }

    fn wake(&mut self) {
        self.asked |= self.app.wake();
    }

    fn pointer(&mut self, (x, y): (f64, f64)) {
        self.input(Input::PointerMoved { x, y });
    }

    fn press(&mut self, (x, y): (f64, f64)) {
        let button = PointerButton::Primary;
        self.pointer((x, y));
        self.input(Input::PointerDown { button, x, y });
    }

    fn release(&mut self, (x, y): (f64, f64)) {
        let button = PointerButton::Primary;
        self.input(Input::PointerUp { button, x, y });
    }

    fn click(&mut self, role: Role, label: &str) {
        let point = bounds(&mut self.app, role, label);
        self.press(point);
        self.release(point);
    }

    fn key(&mut self, key: Key, mods: Modifiers) {
        self.input(Input::KeyDown {
            key,
            mods,
            repeat: false,
        });
        self.input(Input::KeyUp { key });
    }

    /// Everything that happens to a window nobody changes anything in: no frame may
    /// follow.
    fn stays_idle(&mut self, what: &str) {
        let controls: Vec<(f64, f64)> = self
            .app
            .accessibility_tree()
            .nodes
            .iter()
            .filter(|(_, n)| n.role() == Role::Button || n.role() == Role::Tab)
            .filter_map(|(_, n)| n.bounds())
            .map(|r| ((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0))
            .collect();
        assert!(!controls.is_empty(), "{what}: nothing to hover");
        // Wherever the pointer was, it is over nothing from here on.
        self.pointer(NOTHING);
        // The first press on a control moves focus to it, which is drawn.
        self.press(controls[0]);
        self.release(NOTHING);
        self.settle();
        let before = self.counts();
        for _ in 0..3 {
            self.wake();
            for dx in 0..40 {
                self.pointer((NOTHING.0 + f64::from(dx), NOTHING.1));
            }
            // Onto every control and off again.
            for &point in &controls {
                self.pointer(point);
                self.pointer(NOTHING);
            }
            // A press that ends elsewhere does nothing.
            self.press(controls[0]);
            self.release(NOTHING);
            self.key(Key::Char('q'), Modifiers::default());
            self.input(Input::Modifiers(Modifiers::default()));
            self.input(Input::FocusLost);
            assert!(!self.asked, "{what}");
            self.settle();
        }
        assert_eq!(self.counts(), before, "{what}");
    }

    /// Writes a source as its producer does.
    fn write(&mut self, name: &str, value: Value) {
        let key = SourceKey::new(name, &[]).expect(name);
        let backend = self.app.data.backend_mut();
        let (_, slot) = backend
            .subscriptions
            .iter()
            .find(|(k, _)| *k == key)
            .unwrap_or_else(|| panic!("{name} is not watched"));
        slot.set(value);
    }

    /// A source the page on screen watches, and a value it does not have.
    fn changed_value(&mut self) -> (&'static str, Value) {
        let backend = self.app.data.backend_mut();
        let name = backend.subscriptions[0].0.name();
        (name, Value::str("changed"))
    }
}

#[test]
fn an_idle_page_draws_nothing() {
    let mut shell = Shell::new(app());
    shell.stays_idle("the game page");
    shell.click(Role::Button, "Library");
    shell.settle();
    assert_eq!(shell.app.data.location().route, "library");
    shell.stays_idle("the library");
}

#[test]
fn an_idle_palette_draws_nothing() {
    let mut shell = Shell::new(app());
    let ctrl = Modifiers {
        ctrl: PRIMARY == Primary::Ctrl,
        meta: PRIMARY == Primary::Meta,
        ..Modifiers::default()
    };
    shell.key(Key::Char('k'), ctrl);
    shell.settle();
    assert!(shell.app.shown.popup.is_some());
    // A letter would be typed into its search: only what changes nothing.
    let before = shell.counts();
    shell.wake();
    shell.input(Input::Modifiers(Modifiers::default()));
    shell.settle();
    assert_eq!(shell.counts(), before);
}

#[test]
fn an_idle_dialog_draws_nothing_and_the_dwell_needs_no_frame() {
    let mut shell = Shell::new(app());
    let failures = shell.app.data.failures();
    failures.report(ErrorView::failed("Verify failed", "the disk is gone"));
    shell.wake();
    assert!(shell.settle() > 0);
    assert!(shell.app.shown.dialog.is_some());
    shell.stays_idle("an error");

    // Long after, with no frame in between: the press is taken at its own time.
    let before = shell.counts();
    shell.ms += u64::try_from(DWELL.as_millis()).expect("short");
    shell.click(Role::Button, "Close");
    assert_eq!(shell.counts(), before);
    assert!(shell.app.data.modal().is_none(), "the dwell had passed");
    assert!(shell.settle() > 0);
    assert!(shell.app.shown.dialog.is_none());
}

#[test]
fn an_idle_error_panel_draws_nothing() {
    let mut broken = app();
    broken.page.replace_built_in(with_an_unbindable_library());
    let mut shell = Shell::new(broken);
    shell.click(Role::Button, "Library");
    shell.settle();
    assert!(shell.app.shown.panel.is_some());
    // The panel takes the input: there is nothing else to hover.
    let before = shell.frames;
    shell.wake();
    shell.pointer(NOTHING);
    shell.key(Key::Char('q'), Modifiers::default());
    shell.settle();
    assert_eq!(shell.frames, before);
}

#[test]
fn a_value_written_again_draws_nothing() {
    let mut shell = Shell::new(app());
    let (name, value) = shell.changed_value();
    shell.write(name, value.clone());
    shell.wake();
    shell.settle();
    shell.write(name, value);
    shell.wake();
    assert_eq!(shell.settle(), 0);
}

#[test]
fn input_asks_again_for_a_frame_that_never_reached_the_display() {
    let mut shell = Shell::new(app());
    let (name, value) = shell.changed_value();
    shell.write(name, value);
    shell.wake();
    // Built, then skipped or failed.
    shell.build();
    shell.pointer(NOTHING);
    assert!(shell.asked);
    shell.settle();
    shell.pointer((NOTHING.0 + 1.0, NOTHING.1));
    assert!(!shell.asked);
}

#[test]
fn a_changed_value_gets_its_frame() {
    let mut shell = Shell::new(app());
    let (name, value) = shell.changed_value();
    shell.write(name, value);
    shell.wake();
    assert_eq!(shell.settle(), 1);
}

#[test]
fn a_change_taken_by_an_inner_pump_still_gets_its_frame() {
    let mut shell = Shell::new(app());
    let (name, value) = shell.changed_value();
    shell.write(name, value);
    // As a call in the middle of something else does, before the wake arrives.
    shell.app.data.pump();
    shell.wake();
    assert_eq!(shell.settle(), 1);
}

#[test]
fn a_hold_draws_until_it_is_done_and_then_stops() {
    let mut shell = Shell::new(hold_screen());
    let button = bounds(&mut shell.app, Role::Button, HOLD_LABEL);
    shell.press(button);
    let start = shell.ms;
    // The hold asks for one frame after another, at the display's pace.
    while shell.app.data.modal().is_some() {
        assert!(shell.asked, "the hold stopped asking");
        assert!(shell.ms - start < 3_000, "the hold never completes");
        shell.draw();
    }
    shell.release(button);
    shell.settle();
    shell.stays_idle("after a hold");
}

#[test]
fn a_shown_frame_that_changes_the_tree_sends_it_once() {
    let mut shell = Shell::new(app());
    let [frames, presented, updates] = shell.counts();
    let before = shell.sent.clone();
    // Moves focus, which assistive technology is told.
    shell.key(Key::Tab, Modifiers::default());
    assert_eq!(
        shell.counts(),
        [frames, presented, updates],
        "not before it is shown"
    );
    assert_eq!(shell.settle(), 1);
    assert_eq!(shell.counts(), [frames + 1, presented + 1, updates + 1]);
    assert_ne!(shell.sent, before);
}

#[test]
fn a_frame_that_is_not_shown_changes_nothing_assistive_technology_is_told() {
    let mut shell = Shell::new(app());
    let [frames, presented, updates] = shell.counts();
    // Something arrives by itself: no input moves focus.
    let failures = shell.app.data.failures();
    failures.report(ErrorView::failed("Verify failed", "the disk is gone"));
    shell.wake();
    // Built, then skipped or failed.
    shell.build();
    assert_eq!(shell.counts(), [frames + 1, presented, updates]);
    assert_eq!(Some(shell.app.accessibility_tree()), shell.sent);
}
