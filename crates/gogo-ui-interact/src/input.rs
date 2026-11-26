//! What the shell feeds in. Platform-neutral: the shell translates its own events.

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
#[derive(Debug, Clone, PartialEq)]
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
    /// The window stopped receiving input: presses and holds are cancelled.
    FocusLost,
    /// Assistive technology asked for something.
    Accessibility(Request),
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
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RequestKind {
    /// Press it. Counts as real input.
    Activate,
    /// Move focus to it.
    Focus,
    /// Put this text in a text field.
    SetValue(String),
}
