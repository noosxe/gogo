//! IR and layout → an AccessKit tree; AccessKit requests → input (design 24 §2, design
//! 25 §4).
//!
//! The shell calls [`A11yTree::update`] after every change and gives the result to its
//! platform adapter. A request that comes back goes through [`A11yTree::map_request`] and
//! then [`Interaction::handle`](gogo_ui_interact::Interaction::handle) as
//! `Input::Accessibility`, so a screen reader's
//! activation is numbered and checked like any other press. This crate makes no intent
//! and no provenance.
//!
//! # Rules
//!
//! - **What is exposed.** Every control, every node with a role or a name, and all text.
//!   Boxes with neither are left out and their children take their place, so reading
//!   order is document order. A control or a heading is one node: its name is its
//!   `label`, else its own text, else the text inside it, which is not read again.
//! - **Dialogs.** While a modal layer is up, only the window and the topmost modal are
//!   exposed, not what is below it nor what is drawn over it. Its root
//!   is a modal dialog (an alert for
//!   [`LayerKind::Alert`](gogo_ui_interact::LayerKind)), named by the root's name.
//!   With no control focused, focus is the dialog itself.
//! - **Ids** are stable while a control keeps its layer, key and action
//!   [identity](gogo_ui_interact::Layer::identity). A control that now does something
//!   else gets a new id, and a request carries what its control did, so a late one
//!   reaches nothing even before the next [`A11yTree::update`].
//! - **Hold controls** say how to confirm, and say so again once armed.
//! - **Text fields** with a value and a selection carry their text as runs of at most
//!   255 characters, with the selection in them. A composition is not part of it.
//! - **Requests** honoured: click, focus, and, on a text field, setting its value, its
//!   selection or the selected text.
//! - **Bounds** are layout boxes in logical px under a root transform by the scale.
#![deny(unsafe_code)]
#![warn(missing_docs)]

mod tree;

pub use accesskit;
pub use tree::{A11yTree, HOLD_ARMED_HINT, HOLD_HINT, HOLDING_HINT};
