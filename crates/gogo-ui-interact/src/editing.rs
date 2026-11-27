//! A focused text field's keys and pointer gestures. What they do to the text is the
//! field's business: here they only become [`Intent`]s.

use std::time::Duration;

use gogo_ui_ir::{DrawnBy, Gesture, Motion, Node, Role};

use crate::frame::{Frame, Target};
use crate::input::{ImeInput, Key, Modifiers};
use crate::intent::{Intent, Response, TextEdit};
use crate::interaction::{Interaction, Primary, find};

/// The longest wait between the presses of a double or triple click.
const MULTI_CLICK: Duration = Duration::from_millis(500);
/// How far the pointer may move between them, in logical px.
const MULTI_CLICK_REACH: f64 = 4.0;

/// The last press in a text field.
#[derive(Debug, Clone)]
pub(crate) struct Click {
    target: Target,
    at: Duration,
    x: f64,
    y: f64,
    /// 1, 2 or 3.
    count: u8,
}

/// What an editing key means.
enum Command {
    Edit(TextEdit),
    Copy { cut: bool },
}

fn command(primary: Primary, key: Key, mods: Modifiers) -> Option<Command> {
    let mac = primary == Primary::Meta;
    // The platform's shortcut modifier alone, apart from Shift.
    let shortcut = !mods.alt
        && match primary {
            Primary::Ctrl => mods.ctrl && !mods.meta,
            Primary::Meta => mods.meta && !mods.ctrl,
        };
    let bare = !(mods.ctrl || mods.alt || mods.meta);
    // Word at a time: Ctrl, or Option on macOS.
    let word = match primary {
        Primary::Ctrl => mods.ctrl && !(mods.alt || mods.meta),
        Primary::Meta => mods.alt && !(mods.ctrl || mods.meta),
    };
    let extend = mods.shift;
    let to = |motion| Some(Command::Edit(TextEdit::Move { motion, extend }));
    let delete =
        |forward| (bare || word).then_some(Command::Edit(TextEdit::Delete { forward, word }));
    match key {
        Key::ArrowLeft if mac && shortcut => to(Motion::Start),
        Key::ArrowRight if mac && shortcut => to(Motion::End),
        Key::ArrowLeft if word => to(Motion::WordLeft),
        Key::ArrowRight if word => to(Motion::WordRight),
        Key::ArrowLeft if bare => to(Motion::Left),
        Key::ArrowRight if bare => to(Motion::Right),
        Key::Home if !mods.alt => to(Motion::Start),
        Key::End if !mods.alt => to(Motion::End),
        Key::Backspace => delete(false),
        Key::Delete => delete(true),
        Key::Char(c) if shortcut => match (c, extend) {
            ('a', false) => Some(Command::Edit(TextEdit::SelectAll)),
            ('c', false) => Some(Command::Copy { cut: false }),
            ('x', false) => Some(Command::Copy { cut: true }),
            ('z', false) => Some(Command::Edit(TextEdit::Undo)),
            ('z', true) => Some(Command::Edit(TextEdit::Redo)),
            ('y', false) if !mac => Some(Command::Edit(TextEdit::Redo)),
            _ => None,
        },
        _ => None,
    }
}

impl Interaction {
    /// The text field that has focus: where typing goes.
    pub fn text_focus(&self, frame: &Frame<'_>) -> Option<Target> {
        self.field(frame)
    }

    /// What an input method sent. Only a focused text field takes it, and none of it
    /// is a press.
    pub(crate) fn ime(&mut self, ime: ImeInput, frame: &Frame<'_>, out: &mut Response) {
        let edit = match ime {
            ImeInput::Preedit { text, cursor } => {
                self.composing = !text.is_empty() && self.field(frame).is_some();
                TextEdit::Preedit { text, cursor }
            }
            ImeInput::Commit(text) => {
                self.composing = false;
                let text: String = text.chars().filter(|c| !c.is_control()).collect();
                TextEdit::Commit(text)
            }
            ImeInput::Disabled => {
                self.composing = false;
                TextEdit::Preedit {
                    text: String::new(),
                    cursor: None,
                }
            }
        };
        self.edit(edit, frame, out);
    }

    /// Focus is leaving, or the pointer went down: the field drops its composition.
    pub(crate) fn end_composition(&mut self, out: &mut Response) {
        if !std::mem::take(&mut self.composing) {
            return;
        }
        // Focus has not moved yet.
        if let Some(target) = self.focus().cloned() {
            out.intents.push(Intent::Edit {
                target,
                edit: TextEdit::Preedit {
                    text: String::new(),
                    cursor: None,
                },
            });
        }
    }

    /// An edit assistive technology asked of the field `node`.
    pub(crate) fn assistive_edit(
        &mut self,
        node: &Node,
        target: Target,
        edit: TextEdit,
        out: &mut Response,
    ) {
        if node.a11y.role != Role::TextInput || !matches!(node.drawn_by, DrawnBy::App(_)) {
            return;
        }
        out.consumed = true;
        // The edit replaces the composition: the keys are the app's again.
        self.end_composition(out);
        self.focus_by_assistive(target.clone(), out);
        out.intents.push(Intent::Edit { target, edit });
    }

    /// A key down while a text field has focus. Returns whether it was an editing key.
    pub(crate) fn field_key(
        &mut self,
        key: Key,
        mods: Modifiers,
        repeat: bool,
        at: Duration,
        frame: &Frame<'_>,
        out: &mut Response,
    ) -> bool {
        let Some(target) = self.field(frame) else {
            return false;
        };
        match command(self.config.primary, key, mods) {
            Some(Command::Edit(edit)) => self.edit(edit, frame, out),
            // The clipboard is written only by a press, not by a key repeating.
            Some(Command::Copy { cut }) => {
                out.consumed = true;
                if !repeat {
                    self.cancel_presses(out);
                    let provenance = self.provenance(at);
                    out.intents.push(Intent::Copy {
                        target,
                        cut,
                        provenance,
                    });
                }
            }
            None => return false,
        }
        true
    }

    /// The primary button went down on `target`: in a text field it places the caret,
    /// or selects a word or everything, and starts a drag.
    pub(crate) fn field_press(
        &mut self,
        target: &Target,
        (x, y): (f64, f64),
        at: Duration,
        frame: &Frame<'_>,
        out: &mut Response,
    ) {
        let last = self.last_click.take();
        let Some((layer, node)) = find(frame, target.layer, &target.key) else {
            return;
        };
        let is_field = layer.tree.get(node).is_some_and(|n| {
            n.a11y.role == Role::TextInput && matches!(n.drawn_by, DrawnBy::App(_))
        });
        if !is_field {
            return;
        }
        let count = match last {
            Some(c)
                if c.target.same(target)
                    && at.saturating_sub(c.at) <= MULTI_CLICK
                    && (c.x - x).abs() <= MULTI_CLICK_REACH
                    && (c.y - y).abs() <= MULTI_CLICK_REACH =>
            {
                c.count % 3 + 1
            }
            _ => 1,
        };
        self.last_click = Some(Click {
            target: target.clone(),
            at,
            x,
            y,
            count,
        });
        let gesture = match count {
            2 => Gesture::Word,
            3 => Gesture::All,
            _ if self.mods.shift => Gesture::Extend,
            _ => Gesture::Place,
        };
        // Only the first press starts a drag: the others have made their selection.
        self.drag = (count == 1).then(|| target.clone());
        self.point(target, (x, y), gesture, frame, out);
    }

    /// The pointer moved with the button down since a press in a text field.
    pub(crate) fn field_drag(&mut self, x: f64, y: f64, frame: &Frame<'_>, out: &mut Response) {
        if let Some(target) = self.drag.clone() {
            self.point(&target, (x, y), Gesture::Extend, frame, out);
        }
    }

    fn point(
        &mut self,
        target: &Target,
        (x, y): (f64, f64),
        gesture: Gesture,
        frame: &Frame<'_>,
        out: &mut Response,
    ) {
        let Some((layer, node)) = find(frame, target.layer, &target.key) else {
            return;
        };
        let (Some(rect), Some(target)) = (layer.laid.rect(node), layer.target(node)) else {
            return;
        };
        out.consumed = true;
        out.intents.push(Intent::Edit {
            target,
            edit: TextEdit::Point {
                x: x - rect.x0,
                y: y - rect.y0,
                gesture,
            },
        });
    }
}
