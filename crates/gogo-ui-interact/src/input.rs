//! What the shell feeds in. Platform-neutral: the shell translates its own events.

use std::fmt;

use crate::frame::{Identity, LayerId};

/// A pointer button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PointerButton {
    /// The main button, or a touch.
    Primary,
    /// The context button.
    Secondary,
    /// Any other.
    Other,
}

/// A key, by meaning rather than position. Bare modifier keys are [`Key::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Key {
    /// Return or keypad Enter.
    Enter,
    /// The space bar.
    Space,
    /// Escape.
    Escape,
    /// Tab.
    Tab,
    /// Left arrow.
    ArrowLeft,
    /// Right arrow.
    ArrowRight,
    /// Up arrow.
    ArrowUp,
    /// Down arrow.
    ArrowDown,
    /// Backspace.
    Backspace,
    /// Delete, forward.
    Delete,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Home.
    Home,
    /// End.
    End,
    /// A key that types a character: what it types without Ctrl, Alt or Meta.
    Char(char),
    /// Any other key.
    Other,
}

impl Key {
    /// One spelling per key: a typed space is [`Key::Space`], letters are lower case.
    pub(crate) fn normal(self) -> Key {
        match self {
            Key::Char(' ') => Key::Space,
            Key::Char(c) => Key::Char(c.to_lowercase().next().unwrap_or(c)),
            k => k,
        }
    }
}

/// The modifier keys held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    /// Control.
    pub ctrl: bool,
    /// Shift.
    pub shift: bool,
    /// Alt, or Option.
    pub alt: bool,
    /// Command, or the Windows or Super key.
    pub meta: bool,
}

/// One input event, with coordinates in the logical px of [`Laid`](gogo_ui_layout::Laid).
///
/// The shell does not forward key events the platform synthesizes (winit's
/// `is_synthetic`, sent when the window gains or loses focus).
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub enum Input {
    /// The pointer moved.
    PointerMoved {
        /// From the left edge.
        x: f64,
        /// From the top edge.
        y: f64,
    },
    /// The pointer left the window.
    PointerLeft,
    /// A pointer button went down.
    PointerDown {
        /// Which.
        button: PointerButton,
        /// From the left edge.
        x: f64,
        /// From the top edge.
        y: f64,
    },
    /// A pointer button came up.
    PointerUp {
        /// Which.
        button: PointerButton,
        /// From the left edge.
        x: f64,
        /// From the top edge.
        y: f64,
    },
    /// A key went down, or repeats.
    KeyDown {
        /// Which.
        key: Key,
        /// The modifiers held.
        mods: Modifiers,
        /// The platform says it is a repeat.
        repeat: bool,
    },
    /// A key came up.
    KeyUp {
        /// Which.
        key: Key,
    },
    /// The wheel or a touchpad scrolled at a point.
    Wheel {
        /// From the left edge.
        x: f64,
        /// From the top edge.
        y: f64,
        /// How far, in logical px: positive is toward the end of the content.
        dy: f64,
    },
    /// What a key press typed, with its case: the platform's text for the key,
    /// composed dead keys included. Sent after the press's [`Input::KeyDown`], and
    /// not while Ctrl or Meta is held. Only a focused text field takes it.
    Text(String),
    /// The input method did something. Only a focused text field takes it.
    Ime(ImeInput),
    /// The modifier keys held changed. A pointer press has no other way to tell
    /// whether Shift was down.
    Modifiers(Modifiers),
    /// The window stopped receiving input: presses and holds are cancelled.
    FocusLost,
    /// Assistive technology asked for something.
    Accessibility(Request),
}

/// What an input method sends.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImeInput {
    /// What is being composed; empty when there is no longer a composition.
    Preedit {
        /// The text so far.
        text: String,
        /// The input method's selection in it, in bytes; `None` hides the caret.
        cursor: Option<(usize, usize)>,
    },
    /// Text to put in.
    Commit(String),
    /// The input method is off.
    Disabled,
}

/// Never what was composed.
impl fmt::Debug for ImeInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImeInput::Preedit { text, .. } => write!(f, "Preedit({} bytes)", text.len()),
            ImeInput::Commit(text) => write!(f, "Commit({} bytes)", text.len()),
            ImeInput::Disabled => f.write_str("Disabled"),
        }
    }
}

/// A request from assistive technology (`gogo-ui-a11y` maps AccessKit's to this).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The layer of the control.
    pub layer: LayerId,
    /// Its tree key.
    pub key: String,
    /// What its action did when assistive technology read it
    /// ([`Layer::action_identity`](crate::Layer::action_identity)). A request whose
    /// control does something else by now is dropped.
    pub identity: Option<Identity>,
    /// What to do.
    pub kind: RequestKind,
}

/// What assistive technology asks of a control.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RequestKind {
    /// Press it. Counts as real input.
    Activate,
    /// Move focus to it.
    Focus,
    /// Put this text in a text field.
    SetValue(String),
    /// Select in a text field, in bytes of its value.
    SetSelection {
        /// The end that stays.
        anchor: usize,
        /// The caret.
        focus: usize,
    },
    /// Put this text in place of a text field's selection.
    ReplaceSelected(String),
    /// Scroll it into view.
    Reveal,
    /// Scroll a container.
    ScrollBy {
        /// By pages, else by lines.
        pages: bool,
        /// Toward the start, else toward the end.
        back: bool,
    },
    /// Scroll a container to this offset, in logical px.
    ScrollTo(u32),
}

/// Never what was typed: it may be a password, and ends up in logs and test output.
impl fmt::Debug for Input {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Input::PointerMoved { x, y } => write!(f, "PointerMoved({x}, {y})"),
            Input::PointerLeft => f.write_str("PointerLeft"),
            Input::PointerDown { button, x, y } => {
                write!(f, "PointerDown({button:?}, {x}, {y})")
            }
            Input::PointerUp { button, x, y } => write!(f, "PointerUp({button:?}, {x}, {y})"),
            // A character key is what was typed, too.
            Input::KeyDown { key, mods, repeat } => {
                write!(f, "KeyDown({:?}, {mods:?}, repeat: {repeat})", hidden(*key))
            }
            Input::KeyUp { key } => write!(f, "KeyUp({:?})", hidden(*key)),
            Input::Wheel { x, y, dy } => write!(f, "Wheel({x}, {y}, {dy})"),
            Input::Text(text) => write!(f, "Text({} bytes)", text.len()),
            Input::Ime(ime) => write!(f, "Ime({ime:?})"),
            Input::Modifiers(mods) => write!(f, "Modifiers({mods:?})"),
            Input::FocusLost => f.write_str("FocusLost"),
            Input::Accessibility(request) => write!(f, "Accessibility({request:?})"),
        }
    }
}

fn hidden(key: Key) -> Key {
    match key {
        Key::Char(_) => Key::Char('_'),
        k => k,
    }
}

impl fmt::Debug for RequestKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RequestKind::Activate => f.write_str("Activate"),
            RequestKind::Focus => f.write_str("Focus"),
            RequestKind::SetValue(value) => write!(f, "SetValue({} bytes)", value.len()),
            RequestKind::SetSelection { anchor, focus } => {
                write!(f, "SetSelection({anchor}, {focus})")
            }
            RequestKind::ReplaceSelected(text) => {
                write!(f, "ReplaceSelected({} bytes)", text.len())
            }
            RequestKind::Reveal => f.write_str("Reveal"),
            RequestKind::ScrollBy { pages, back } => {
                write!(f, "ScrollBy(pages: {pages}, back: {back})")
            }
            RequestKind::ScrollTo(y) => write!(f, "ScrollTo({y})"),
        }
    }
}
