//! The display list and the `Renderer` trait: the boundary renderers implement (design 24
//! §2, design 28). Effect lowering lives here.
//!
//! [`build`] turns a [`gogo_ui_ir::Tree`] and its [`gogo_ui_layout::Laid`] into a
//! [`DisplayList`]: a flat list of [`Op`]s in paint order that every renderer draws the same
//! way (design 28 §1). A renderer sees nothing else: not the IR, a skin or the contract
//! (design 24 §2.1 rule 3).
//!
//! # Rules
//!
//! - **Logical px, kurbo and peniko.** The list is built in logical px with `kurbo`
//!   geometry and `peniko` colours and gradients (design 24 decision 6). The renderer
//!   applies the scale, so one list serves @1× and @2×.
//! - **No pixels in the list.** An image is an [`ImageId`], an index into
//!   [`DisplayList::images`], which names the image by an [`ImageKey`]. The app fills an
//!   [`ImageStore`] (design 25 §7) and each renderer turns what it finds into its own form.
//!   The id is only valid for the list that made it; caches key on the [`ImageKey`].
//! - **Paint order per node** is design 28 §1: shadows, glow, backdrop, fill, the image or
//!   icon, noise, scanlines, reflection, inner shadow, border, the node's text, children,
//!   focus ring. A node with `opacity` below 1 wraps all of it, children included, in one
//!   [`Op::PushOpacity`] layer. `Paint::offset` moves a node and its descendants visually;
//!   layout never sees it.
//! - **Effect lowering** (design 28 §2):
//!   - shadow and glow are [`Op::BlurRect`] (a zero-blur shadow is a hard, offset
//!     [`Op::Fill`]); an inner shadow is a [`Op::BlurRect`] with `inset`, inside a
//!     [`Op::PushClip`] of the box;
//!   - noise is a repeating image brush, scanlines are thin fills inside a clip, and a
//!     reflection is a white gradient fill;
//!   - **text bloom** is one [`Op::PushFilter`] layer around *each* glyph run, then the
//!     sharp text. One primitive per filter layer on purpose: multi-primitive filter layers
//!     are not yet proven on both renderers (design 28 §5);
//!   - backdrop blur is [`Op::Backdrop`], drawn by the renderer (design 28 §3);
//!   - no effect uses a mask layer (design 28 §5).
//! - **Scroll containers** (a node whose [`gogo_ui_layout::Placed::clips`] is set) clip
//!   their descendants to their box with a [`Op::PushClip`].
//! - **Icons** are paths this crate owns ([`icons::NAMES`]). An icon node is drawn only
//!   when its paint has a *solid* fill, as a filled glyph in that colour; its box is never
//!   filled. **An unknown icon name, or an icon without a solid fill, draws nothing and is
//!   not an error** (design 27 §5.2 makes unknown names a skin load error; the engine
//!   itself stays quiet).
//! - **Nothing is drawn for nothing.** A node with no area, or whose paint holds a NaN or
//!   infinity (the skin loader keeps paints finite; a hand-built tree might not), draws no
//!   ops of its own, though its children still do. A glow layer, backdrop or text bloom of
//!   radius 0 emits no op.
//! - **Sigma.** Blur values in the list are sigmas (design 28 §2): the IR's CSS radius `r`
//!   becomes `r / 2` for shadows, glow and bloom, and stays `r` for backdrop blur.
//!
//! The crate knows nothing about gogo (design 24 §2.1 rule 6).
#![deny(unsafe_code)]
#![warn(missing_docs)]

mod build;
pub mod icons;
mod image;
mod list;
mod noise;
mod render;

pub use build::{SceneState, build};
pub use image::{Image, ImageId, ImageKey, ImageStore, MemoryStore};
pub use list::{Brush, DisplayList, Op, OpSummary, Shape};
pub use noise::{NOISE_SIZE, noise_tile};
pub use render::{Renderer, Surface};
