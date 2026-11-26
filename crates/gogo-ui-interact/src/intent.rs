//! What comes out: intents and the proof that input made them.

use std::time::Duration;

use gogo_ui_ir::{ActionRef, AppPart};

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

/// Something the user asked for.
#[derive(Debug, PartialEq)]
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
    /// Esc: close, cancel or go back.
    Dismiss {
        /// The press.
        provenance: InputProvenance,
    },
    /// Enter under a dialog with no control focused: the plan card's Enter.
    Enter {
        /// The press.
        provenance: InputProvenance,
    },
    /// Open the command palette.
    Palette {
        /// The press.
        provenance: InputProvenance,
    },
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
