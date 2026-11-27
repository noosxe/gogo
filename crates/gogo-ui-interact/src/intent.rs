//! What comes out: intents and the proof that input made them.

use std::time::Duration;

use gogo_ui_ir::{ActionRef, AppPart, Gesture, Motion};

use crate::frame::Target;

/// The number of one real input. Strictly increasing from 1; 0 means none yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct InputEvent(pub(crate) u64);

impl InputEvent {
    /// The number.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Proof that a real press made an intent. Only this crate makes one, and it can't be
/// copied, so one press can't be spent twice.
///
/// ```compile_fail
/// fn twice(p: &gogo_ui_interact::InputProvenance) -> gogo_ui_interact::InputProvenance {
///     p.clone()
/// }
/// ```
#[derive(Debug, PartialEq, Eq)]
pub struct InputProvenance {
    pub(crate) event: InputEvent,
    pub(crate) at: Duration,
}

impl InputProvenance {
    /// The press's number, taken when it went down.
    pub fn event(&self) -> InputEvent {
        self.event
    }

    /// When it went down, on the clock given to [`Interaction::handle`](crate::Interaction::handle).
    pub fn at(&self) -> Duration {
        self.at
    }

    /// Whether it is a key repeating. Never: a repeat makes no intent that carries a
    /// provenance. Here so a translation to the app's press type is explicit.
    pub fn repeat(&self) -> bool {
        false
    }
}

/// What a control knew when it was activated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct ControlInput {
    /// A switch's state before this activation.
    pub on: Option<bool>,
}

/// One change to a text field: its text, caret or selection.
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub enum TextEdit {
    /// Type this where the caret is. Has no control characters.
    Insert(String),
    /// Delete the selection, else a character or a word beside the caret.
    Delete {
        /// After the caret, not before it.
        forward: bool,
        /// A word.
        word: bool,
    },
    /// Move the caret.
    Move {
        /// Where.
        motion: Motion,
        /// The selection reaches there.
        extend: bool,
    },
    /// Select everything.
    SelectAll,
    /// Take the last change back.
    Undo,
    /// Make it again.
    Redo,
    /// An input method's composition: shown, not yet part of the text. Empty ends it.
    Preedit {
        /// The text so far.
        text: String,
        /// The input method's selection in it, in bytes; `None` hides the caret.
        cursor: Option<(usize, usize)>,
    },
    /// An input method's result.
    Commit(String),
    /// Assistive technology selects, in bytes of the field's value.
    Select {
        /// The end that stays.
        anchor: usize,
        /// The caret.
        focus: usize,
    },
    /// Assistive technology puts text in place of the selection.
    Replace(String),
    /// A press or a drag in the field.
    Point {
        /// From the left edge of the field's box.
        x: f64,
        /// From its top.
        y: f64,
        /// What it does.
        gesture: Gesture,
    },
}

/// Never the text that was typed.
impl std::fmt::Debug for TextEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TextEdit::Insert(text) => write!(f, "Insert({} bytes)", text.len()),
            TextEdit::Delete { forward, word } => {
                write!(f, "Delete(forward: {forward}, word: {word})")
            }
            TextEdit::Move { motion, extend } => write!(f, "Move({motion:?}, {extend})"),
            TextEdit::SelectAll => f.write_str("SelectAll"),
            TextEdit::Undo => f.write_str("Undo"),
            TextEdit::Redo => f.write_str("Redo"),
            TextEdit::Preedit { text, .. } => write!(f, "Preedit({} bytes)", text.len()),
            TextEdit::Commit(text) => write!(f, "Commit({} bytes)", text.len()),
            TextEdit::Select { anchor, focus } => write!(f, "Select({anchor}, {focus})"),
            TextEdit::Replace(text) => write!(f, "Replace({} bytes)", text.len()),
            TextEdit::Point { x, y, gesture } => write!(f, "Point({x}, {y}, {gesture:?})"),
        }
    }
}

/// A step along a list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Towards the start.
    Prev,
    /// Towards the end.
    Next,
}

/// Something the user asked for.
#[derive(PartialEq)]
#[non_exhaustive]
pub enum Intent {
    /// A skin control was activated.
    Invoke {
        /// The control.
        target: Target,
        /// The action its binding names, valid for the tree it came from.
        action: ActionRef,
        /// The control's input to the action.
        input: ControlInput,
        /// The press.
        provenance: InputProvenance,
    },
    /// A control the app drew was activated. Which one: its part, or its key.
    App {
        /// The control.
        target: Target,
        /// The widget and part.
        part: AppPart,
        /// The press.
        provenance: InputProvenance,
    },
    /// A skin `menu` button or `select` was activated: the app opens its popup. No
    /// action runs.
    Open {
        /// The control.
        target: Target,
        /// The press.
        provenance: InputProvenance,
    },
    /// Up or Down in the text field of a layer with [`Layer::submit`](crate::Layer::submit):
    /// move the highlight. Not a press: it answers nothing.
    Move {
        /// The field.
        target: Target,
        /// Which way.
        step: Step,
    },
    /// A hold control is being held.
    HoldStart {
        /// The control.
        target: Target,
        /// The widget and part, when the app drew it.
        part: Option<AppPart>,
        /// The press.
        provenance: InputProvenance,
    },
    /// The hold ended: released, or cancelled.
    HoldEnd {
        /// The control.
        target: Target,
    },
    /// A screen reader activated a hold control once; a second activation holds it.
    HoldArmed {
        /// The control.
        target: Target,
    },
    /// Assistive technology set a text field. Not a press: it answers nothing.
    SetValue {
        /// The field.
        target: Target,
        /// The new text.
        value: String,
    },
    /// Typing, caret motion or selection in a text field. Not a press: it answers
    /// nothing.
    Edit {
        /// The field.
        target: Target,
        /// What to do to its text.
        edit: TextEdit,
    },
    /// Esc: close, cancel or go back. Also a press outside a popup.
    Dismiss {
        /// The press.
        provenance: InputProvenance,
    },
    /// Enter under a dialog with no control focused: the plan card's Enter.
    Enter {
        /// The press.
        provenance: InputProvenance,
    },
    /// The paste shortcut under a dialog, or in its text field: the widget may take
    /// the clipboard.
    Paste {
        /// The press.
        provenance: InputProvenance,
    },
    /// Copy or cut was pressed in a focused text field: the field may put its
    /// selection on the clipboard.
    Copy {
        /// The field.
        target: Target,
        /// Take the selection out of the field too.
        cut: bool,
        /// The press.
        provenance: InputProvenance,
    },
    /// Open the command palette.
    Palette {
        /// The press.
        provenance: InputProvenance,
    },
}

/// As derived, but for the text assistive technology sets: only its length.
impl std::fmt::Debug for Intent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Intent::Invoke {
                target,
                action,
                input,
                provenance,
            } => f
                .debug_struct("Invoke")
                .field("target", target)
                .field("action", action)
                .field("input", input)
                .field("provenance", provenance)
                .finish(),
            Intent::App {
                target,
                part,
                provenance,
            } => f
                .debug_struct("App")
                .field("target", target)
                .field("part", part)
                .field("provenance", provenance)
                .finish(),
            Intent::HoldStart {
                target,
                part,
                provenance,
            } => f
                .debug_struct("HoldStart")
                .field("target", target)
                .field("part", part)
                .field("provenance", provenance)
                .finish(),
            Intent::Open { target, provenance } => f
                .debug_struct("Open")
                .field("target", target)
                .field("provenance", provenance)
                .finish(),
            Intent::Move { target, step } => f
                .debug_struct("Move")
                .field("target", target)
                .field("step", step)
                .finish(),
            Intent::HoldEnd { target } => {
                f.debug_struct("HoldEnd").field("target", target).finish()
            }
            Intent::HoldArmed { target } => {
                f.debug_struct("HoldArmed").field("target", target).finish()
            }
            Intent::SetValue { target, value } => f
                .debug_struct("SetValue")
                .field("target", target)
                .field("bytes", &value.len())
                .finish(),
            Intent::Edit { target, edit } => f
                .debug_struct("Edit")
                .field("target", target)
                .field("edit", edit)
                .finish(),
            Intent::Dismiss { provenance } => f
                .debug_struct("Dismiss")
                .field("provenance", provenance)
                .finish(),
            Intent::Enter { provenance } => f
                .debug_struct("Enter")
                .field("provenance", provenance)
                .finish(),
            Intent::Paste { provenance } => f
                .debug_struct("Paste")
                .field("provenance", provenance)
                .finish(),
            Intent::Copy {
                target,
                cut,
                provenance,
            } => f
                .debug_struct("Copy")
                .field("target", target)
                .field("cut", cut)
                .field("provenance", provenance)
                .finish(),
            Intent::Palette { provenance } => f
                .debug_struct("Palette")
                .field("provenance", provenance)
                .finish(),
        }
    }
}

/// What one call did.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Response {
    /// In the order they happened.
    pub intents: Vec<Intent>,
    /// The input was used here.
    pub consumed: bool,
    /// Focus, hover or the pressed control changed.
    pub redraw: bool,
}
