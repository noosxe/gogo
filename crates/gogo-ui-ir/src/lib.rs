//! The resolved tree (the IR): the boundary between "a skin" and "an engine"
//! (design 24 §2).
//!
//! Everything a skin can say is already applied by the time a [`Tree`] exists:
//! expressions, `each` and `when`, components, styles and states (design 27 §4 to §5.8).
//! What is left is data an engine can lay out, draw and describe to assistive
//! technology without knowing what a skin, GOG or the app is (design 24 §2.1, rule 6):
//!
//! - [`LayoutStyle`]: flex and grid, sizes, gaps, alignment and position, in this crate's
//!   own enums (the layout crate maps them to its solver);
//! - [`Paint`] and [`Effect`]: fills, borders, shadows and the fixed effect list of
//!   design 27 §6, drawn as design 28 §1 and §2 describe;
//! - [`TextStyle`]: one resolved text run;
//! - [`TextMarks`]: an edited run's caret, selection and preedit;
//! - [`Motion`] and [`Gesture`]: how a caret is moved;
//! - [`A11y`] and [`Role`]: what assistive technology is told;
//! - [`ActionRef`]: an opaque id for "activating this does something". The engine hands
//!   it back and never looks inside; the binder maps it to a resolved action;
//! - [`DrawnBy`]: whether the skin drew a node or the app did (an app widget's part,
//!   named by opaque ids).
//!
//! # Rules every consumer follows
//!
//! - **Units** are logical pixels, unless a field says otherwise (a fraction, a
//!   multiple of the font size, em, degrees). A renderer at scale 2 draws every length
//!   twice as large.
//! - **Nothing inherits in the IR.** The skin format inherits text properties and box
//!   effects (design 27 §5.2, §6); the binder has resolved that, so every node carries
//!   its full values.
//! - **Borders take layout space** (CSS `border-box`, design 27 §4): a node's size
//!   includes its border, and the border width is added to the padding on that side
//!   before children are placed.
//! - **Blur follows CSS** (design 27 §6): a shadow, inner shadow, glow or bloom radius
//!   `r` is a Gaussian with sigma `r / 2`; [`Effect::BackdropBlur`] is
//!   `backdrop-filter: blur(r)`, so sigma is `r`.
//! - **Colours** are straight (not premultiplied) sRGB with 8 bits per channel.
//! - **Angles** are degrees, CSS style: 0 points up, 90 right, 180 down.
//! - **Paint order** per node is design 28 §1: shadows, glow, backdrop, fill, noise,
//!   scanlines, reflection, inner shadow, border, the node's text, children, focus ring.
//! - **Document order** of a [`Tree`] (pre-order) is the paint order of siblings. The
//!   keyboard visits the focusable, enabled nodes in the same order
//!   ([`Tree::focus_order`]).
//!
//! The crate depends on no other gogo crate and on no layout, text, render or window
//! crate. Its only dependencies are `thiserror` and, behind the `serde` feature, `serde`.
//!
//! With the `serde` feature every type serializes (the dev inspector); nothing reads
//! a tree back.
#![deny(unsafe_code)]
#![warn(missing_docs)]

mod a11y;
mod layout;
mod paint;
mod text;
mod tree;

#[cfg(test)]
mod tests;

pub use a11y::{A11y, Role};
pub use layout::{Align, Dim, Direction, Display, Justify, LayoutStyle, Position, Sides, Track};
pub use paint::{Color, Effect, Fill, GlowLayer, GradientStop, Paint, Shape, Stroke};
pub use text::{Bloom, Gesture, Motion, TextAlign, TextMarks, TextStyle};
pub use tree::{
    ActionRef, AppPart, DrawnBy, Fit, ImageSource, Kind, Node, NodeId, PartId, SCROLLBAR_WIDTH,
    ScrollSpec, Tree, TreeError, WidgetId,
};
