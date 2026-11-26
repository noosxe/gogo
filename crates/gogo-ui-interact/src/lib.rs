//! Input → intents: hit-testing, focus traversal, key routing, the layer stack and input
//! provenance (S4, S13, S19; design 24 §2, design 25 §4).
//!
//! The shell translates its platform events into [`Input`] and calls
//! [`Interaction::handle`] with the [`Frame`] on screen. What comes back are [`Intent`]s:
//! the only way anything happens. The crate knows nothing about gogo: an action is the
//! IR's opaque [`ActionRef`](gogo_ui_ir::ActionRef), an app control its
//! [`AppPart`](gogo_ui_ir::AppPart) and tree key.
//!
//! # Rules
//!
//! - **Only real input acts** (S4). An intent that carries an [`InputProvenance`] comes
//!   from a pointer press, a key press or an accessibility activation, and one such input
//!   yields at most one of them. Loading, hovering, focusing, [`Interaction::sync`] and
//!   [`Intent::SetValue`] never do. Only this crate can make a provenance.
//! - **Numbered on the way down.** Pointer, key and accessibility input share one
//!   counter. A press gets its number and time when it goes down, even if it acts on
//!   release, so a press begun before a screen appeared can't answer it.
//! - **Key repeat is not a press.** A repeating key gets no number and activates
//!   nothing; it only moves focus (Tab, arrows). A key that is already down counts as
//!   repeating even if the platform did not say so.
//! - **Activation.** A pointer press acts on release over the same control; Enter acts
//!   on the way down, Space on release. A press whose control changed in between (its
//!   layer, key, action [identity](Layer::identity) or on/off state) does nothing.
//! - **Hold controls** ([`Layer::hold`]) give [`Intent::HoldStart`] and
//!   [`Intent::HoldEnd`] and never an activation. A screen reader has only a discrete
//!   activation: the first one arms ([`Intent::HoldArmed`]), a second one at least
//!   [`HOLD_ARM`] and at most [`ARM_EXPIRES`] later starts the hold, which runs until it
//!   is cancelled (Esc, a focus move, any other key or button, the window losing input,
//!   the control or screen changing).
//! - **Global keys are the app's** (S19): Esc is [`Intent::Dismiss`], the platform's
//!   primary modifier with K is [`Intent::Palette`], and Enter under a dialog with
//!   nothing focused is [`Intent::Enter`]. No skin shortcut can take them.
//! - **Focus order is document order** (S13), over the layers that take input. The
//!   topmost dialog traps focus and pointer: nothing below it can be reached.
//! - **Focus-visible.** The focus ring shows after the keyboard or a screen reader moved
//!   focus, not after a click.
//! - **Not here yet:** opening a select or menu, text editing, and scrolling. A select
//!   takes focus but does not activate: the tree does not carry its options.
#![deny(unsafe_code)]
#![warn(missing_docs)]

mod focus;
mod frame;
mod input;
mod intent;
mod interaction;
mod keys;

pub use focus::FocusOrder;
pub use frame::{Frame, Layer, LayerId, LayerKind, Target};
pub use input::{Input, Key, Modifiers, PointerButton, Request, RequestKind};
pub use intent::{ControlInput, InputEvent, InputProvenance, Intent, Response};
pub use interaction::{ARM_EXPIRES, Config, HOLD_ARM, Interaction, Primary};
