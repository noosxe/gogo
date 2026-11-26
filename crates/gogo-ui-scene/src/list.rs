//! The display list and its ops (design 28 §1).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use gogo_ui_ir::Fit;
use gogo_ui_text::GlyphRun;
use kurbo::{Affine, BezPath, Ellipse, Point, Rect, RoundedRect, RoundedRectRadii, Shape as _};
use peniko::{Color, Gradient};

use crate::{ImageId, ImageKey};

/// Where a fill, clip or backdrop applies, in logical px.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Shape {
    /// A rectangle with square corners.
    Rect(Rect),
    /// A rectangle with rounded corners.
    RoundedRect(RoundedRect),
    /// An ellipse (a circle in a square box).
    Ellipse(Ellipse),
    /// Any path. With `even_odd` the inside is by the even-odd rule, which makes a ring out
    /// of an outer and an inner outline (borders, the focus ring).
    Path {
        /// The outline.
        path: BezPath,
        /// Fill rule.
        even_odd: bool,
    },
}

impl Shape {
    /// The shape as a path and its fill rule, for a renderer without native shapes.
    pub fn to_path(&self) -> (BezPath, bool) {
        match self {
            Shape::Rect(r) => (r.to_path(0.1), false),
            Shape::RoundedRect(r) => (r.to_path(0.1), false),
            Shape::Ellipse(e) => (e.to_path(0.1), false),
            Shape::Path { path, even_odd } => (path.clone(), *even_odd),
        }
    }

    /// The smallest rectangle around the shape.
    pub fn bounds(&self) -> Rect {
        match self {
            Shape::Rect(r) => *r,
            Shape::RoundedRect(r) => r.rect(),
            Shape::Ellipse(e) => e.bounding_box(),
            Shape::Path { path, .. } => path.bounding_box(),
        }
    }
}

/// What a fill paints with.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Brush {
    /// One colour, straight alpha.
    Solid(Color),
    /// A linear or radial gradient in logical px.
    Gradient(Gradient),
    /// An image, placed by `transform` (image px → logical px).
    Image {
        /// The image, an index into [`DisplayList::images`].
        id: ImageId,
        /// Image px → logical px.
        transform: Affine,
        /// Tile the image over the shape; otherwise the edge pixels extend.
        repeat: bool,
        /// Opacity, 0 to 1.
        alpha: f32,
    },
}

/// One drawing instruction. A list is drawn first to last; layers opened by a `Push…` op
/// close at the matching [`Op::Pop`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Op {
    /// Fills a shape.
    Fill {
        /// What to fill.
        shape: Shape,
        /// What to fill it with.
        brush: Brush,
    },
    /// A Gaussian-blurred rounded rectangle: shadows, glow and (with `inset`) inner shadows.
    /// Painted only inside `bounds`.
    BlurRect {
        /// The rectangle that is blurred.
        rect: Rect,
        /// Its corner radii.
        radii: RoundedRectRadii,
        /// The Gaussian's standard deviation in logical px.
        sigma: f64,
        /// The colour at full coverage, straight alpha.
        color: Color,
        /// Paint the inverse (`1 - coverage`): an inner shadow. Always inside a
        /// [`Op::PushClip`] of the box it shades.
        inset: bool,
        /// The area the blur is painted in: `rect` grown by 3 sigma for an outer blur, the
        /// shaded box for an inset one.
        bounds: Rect,
    },
    /// A shaped glyph run.
    Glyphs {
        /// The glyphs, relative to `origin`. Shared: bloom draws a run twice.
        run: Arc<GlyphRun>,
        /// The top-left of the text box in logical px.
        origin: Point,
        /// The colour.
        brush: Brush,
    },
    /// An image from the store.
    Image {
        /// The image, an index into [`DisplayList::images`].
        id: ImageId,
        /// The box the image fills.
        rect: Rect,
        /// How it fills the box. The overflow of `Cover` is cut by the box, so the scene
        /// wraps it in a [`Op::PushClip`].
        fit: Fit,
    },
    /// Clips what follows to a shape, until the matching [`Op::Pop`].
    PushClip {
        /// The clip.
        shape: Shape,
    },
    /// Draws what follows into a layer with this opacity, until the matching [`Op::Pop`].
    PushOpacity {
        /// 0 to 1.
        alpha: f32,
    },
    /// Draws what follows into a layer and blurs it, until the matching [`Op::Pop`]. The
    /// scene wraps exactly one primitive in it.
    PushFilter {
        /// The blur's standard deviation in logical px.
        sigma: f64,
    },
    /// Closes the layer opened last.
    Pop,
    /// Blurs what is already drawn under `shape` (design 28 §3).
    Backdrop {
        /// The area that shows the blurred backdrop.
        shape: Shape,
        /// The blur's standard deviation in logical px.
        sigma: f64,
    },
}

impl Op {
    /// The op's name: its variant, for summaries and tests.
    pub fn kind(&self) -> &'static str {
        match self {
            Op::Fill { .. } => "Fill",
            Op::BlurRect { .. } => "BlurRect",
            Op::Glyphs { .. } => "Glyphs",
            Op::Image { .. } => "Image",
            Op::PushClip { .. } => "PushClip",
            Op::PushOpacity { .. } => "PushOpacity",
            Op::PushFilter { .. } => "PushFilter",
            Op::Pop => "Pop",
            Op::Backdrop { .. } => "Backdrop",
        }
    }
}

/// A frame's drawing instructions, in logical px.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct DisplayList {
    /// Width of the window in logical px.
    pub width: f64,
    /// Height of the window in logical px.
    pub height: f64,
    /// What the frame is cleared to: the root's solid fill, or opaque black.
    pub background: Color,
    /// The ops, first drawn first.
    pub ops: Vec<Op>,
    /// The images the ops use, by [`ImageId`] index.
    pub images: Vec<ImageKey>,
}

impl DisplayList {
    /// The key of an image id of this list.
    pub fn image(&self, id: ImageId) -> Option<&ImageKey> {
        self.images.get(id.index())
    }

    /// How many ops of each kind: a compact structure for snapshot tests.
    pub fn summary(&self) -> OpSummary {
        let mut counts = BTreeMap::new();
        for op in &self.ops {
            *counts.entry(op.kind()).or_insert(0) += 1;
        }
        OpSummary {
            total: self.ops.len(),
            images: self.images.len(),
            counts,
        }
    }
}

/// Op counts by kind, printed one per line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpSummary {
    /// All ops.
    pub total: usize,
    /// Distinct images.
    pub images: usize,
    /// Ops per kind, by name.
    pub counts: BTreeMap<&'static str, usize>,
}

impl fmt::Display for OpSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "total {}", self.total)?;
        writeln!(f, "images {}", self.images)?;
        for (k, n) in &self.counts {
            writeln!(f, "{k} {n}")?;
        }
        Ok(())
    }
}
