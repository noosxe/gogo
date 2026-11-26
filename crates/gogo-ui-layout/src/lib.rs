//! Layout: IR → taffy → positioned boxes, with hit-testing (design 24 §2). Text leaves are
//! measured through `gogo-ui-text`.
//!
//! [`layout`] turns a [`gogo_ui_ir::Tree`] into a [`Laid`]: for every node an absolute
//! border box and content box in **logical px** (design 28 §1: one layout serves @1× and
//! @2×; the renderer applies the scale), and for every text node the [`TextLayout`]
//! broken at its final width. [`Laid::hit`] answers which node is under a point.
//!
//! # Rules
//!
//! - **Border box.** A node's size includes its border, and the border width is added to
//!   the padding on that side (design 24 §2, the IR's rules): taffy's `border-box`
//!   sizing with the border set from the paint.
//! - **Position is explicit.** Since taffy 0.15 an inset is ignored unless `position` is
//!   set, so every node gets one: in-flow nodes are `Relative` with auto insets (the IR
//!   says an inset on a relative node is ignored), absolute nodes are `Absolute` with
//!   their insets (design 27 §4).
//! - **No detach paths.** The taffy tree is built bottom-up, each parent made with its
//!   children (`new_with_children`), never by `add_child` on a node that already has a
//!   parent (that panics later). Nothing is removed, so the quadratic `remove` is never
//!   hit; a frame builds a fresh tree.
//! - **Whole pixels.** taffy rounds boxes to whole logical px, so an edge never falls
//!   between two device pixels at 1× and 2×.
//! - **Text leaves** are measured by breaking the text at the width taffy offers (min
//!   content: the longest word; max content: no limit), rounded up to whole px, then
//!   broken again at the final content width (design 24 §3 on text). A node whose text
//!   style has `max_lines` ends its last line in an ellipsis.
//! - **Scroll offsets are an input** ([`Scroll`]): a node with an entry is a scroll
//!   container. Its descendants move by minus the offset and are only hit inside the
//!   container's box. The IR has no overflow property yet, so layout does not clamp or
//!   measure scrollable content; the interaction crate owns the scroll state.
//! - **`Paint::offset` is not layout.** A visual offset never moves a box or a hit area.
//! - **Errors, not panics.** A non-finite length, a non-finite viewport or a taffy
//!   failure is a [`LayoutError`]; no input of a valid tree panics.
//!
//! The crate knows nothing about gogo (design 24 §2.1 rule 6).
#![deny(unsafe_code)]
#![warn(missing_docs)]

mod laid;
mod solve;
mod style;

pub use laid::{Laid, Placed, Rect, Scroll};
pub use solve::{LayoutError, Viewport, layout};
