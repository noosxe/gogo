//! The result of a layout pass: boxes, text and hit-testing.

use std::collections::HashMap;

use gogo_ui_ir::NodeId;
use gogo_ui_text::TextLayout;

/// An axis-aligned rectangle in logical px: `x0, y0` is the top-left corner, `x1, y1` the
/// bottom-right, and `x1 >= x0`, `y1 >= y0`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    /// Left edge.
    pub x0: f64,
    /// Top edge.
    pub y0: f64,
    /// Right edge.
    pub x1: f64,
    /// Bottom edge.
    pub y1: f64,
}

impl Rect {
    /// A rectangle from its corners.
    pub const fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Rect {
        Rect { x0, y0, x1, y1 }
    }

    /// Width.
    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }

    /// Height.
    pub fn height(&self) -> f64 {
        self.y1 - self.y0
    }

    /// Whether the point is inside: the left and top edges count, the right and bottom do
    /// not, so two neighbours never both claim a point.
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    /// The overlap with `other`, or an empty rectangle when they don't meet.
    pub fn intersect(&self, other: &Rect) -> Rect {
        let x0 = self.x0.max(other.x0);
        let y0 = self.y0.max(other.y0);
        Rect {
            x0,
            y0,
            x1: self.x1.min(other.x1).max(x0),
            y1: self.y1.min(other.y1).max(y0),
        }
    }

    /// The rectangle moved by `(dx, dy)`.
    pub fn translate(&self, dx: f64, dy: f64) -> Rect {
        Rect::new(self.x0 + dx, self.y0 + dy, self.x1 + dx, self.y1 + dy)
    }
}

/// Scroll offsets by node: the input that moves a scroll container's content.
///
/// `(x, y)` is how far the content is scrolled, so `(0, 30)` moves the descendants 30 px
/// up. For a node with a [`ScrollSpec`](gogo_ui_ir::ScrollSpec) only `y` counts, and
/// layout clamps it to the content ([`ScrollBox`]); without an entry it is at the top. A
/// node without a spec that has an entry is clipped and moved exactly as given.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scroll {
    offsets: HashMap<NodeId, (f32, f32)>,
}

impl Scroll {
    /// No scroll containers.
    pub fn none() -> Scroll {
        Scroll::default()
    }

    /// Makes `node` a scroll container scrolled by `(x, y)` px. Non-finite values count
    /// as 0.
    pub fn with(mut self, node: NodeId, x: f32, y: f32) -> Scroll {
        self.set(node, x, y);
        self
    }

    /// As [`Scroll::with`], in place.
    pub fn set(&mut self, node: NodeId, x: f32, y: f32) {
        let f = |v: f32| if v.is_finite() { v } else { 0.0 };
        self.offsets.insert(node, (f(x), f(y)));
    }

    /// The offset of a scroll container, `None` for any other node.
    pub fn get(&self, node: NodeId) -> Option<(f32, f32)> {
        self.offsets.get(&node).copied()
    }
}

/// The shortest a scrollbar thumb gets, in logical px, where the track allows it.
pub const MIN_THUMB: f64 = 24.0;

/// A scroll container as placed, in logical px.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct ScrollBox {
    /// How far the content is scrolled, from 0 to [`ScrollBox::max`].
    pub offset: f64,
    /// The largest offset: 0 when the content fits.
    pub max: f64,
    /// The box the content is seen through: inside the border, left of the gutter. Not
    /// cut by the containers above.
    pub viewport: Rect,
    /// The gutter.
    pub track: Rect,
    /// The scrollbar thumb inside [`ScrollBox::track`]; empty when the content fits.
    pub thumb: Rect,
}

impl ScrollBox {
    pub(crate) fn new(offset: f64, max: f64, viewport: Rect, gutter: f64) -> ScrollBox {
        let max = max.max(0.0);
        let offset = offset.clamp(0.0, max);
        let track = Rect::new(viewport.x1, viewport.y0, viewport.x1 + gutter, viewport.y1);
        let height = track.height();
        let thumb = if max > 0.0 && height > 0.0 && gutter > 0.0 {
            let length = (height * viewport.height() / (viewport.height() + max))
                .max(MIN_THUMB)
                .min(height);
            let top = track.y0 + (height - length) * offset / max;
            Rect::new(track.x0, top, track.x1, top + length)
        } else {
            Rect::new(track.x0, track.y0, track.x0, track.y0)
        };
        ScrollBox {
            offset,
            max,
            viewport,
            track,
            thumb,
        }
    }
}

/// One node as placed.
#[derive(Debug)]
#[non_exhaustive]
pub struct Placed {
    /// The border box, absolute, in logical px, after scrolling.
    pub rect: Rect,
    /// The content box: inside the border and the padding. Text is drawn at its corner.
    pub content: Rect,
    /// The part of the border box that can be seen and hit: [`Placed::rect`] cut by every
    /// scroll container above.
    pub visible: Rect,
    /// What the node's descendants are clipped to when [`Placed::clips`], before the
    /// containers above cut it.
    pub clip: Rect,
    /// A node with a [`ScrollSpec`](gogo_ui_ir::ScrollSpec); `None` for any other.
    pub scroll: Option<ScrollBox>,
    /// A text node's shaped text, broken at the content width.
    pub text: Option<TextLayout>,
    /// Whether the node is a scroll container: its descendants are drawn and hit only
    /// inside [`Placed::clip`].
    pub clips: bool,
    /// The parent, `None` for the root.
    pub parent: Option<NodeId>,
}

/// A laid-out tree: a [`Placed`] per node, indexed by [`NodeId`].
#[derive(Debug)]
pub struct Laid {
    pub(crate) placed: Vec<Placed>,
    /// Document (paint) order.
    pub(crate) order: Vec<NodeId>,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

impl Laid {
    /// The viewport size the tree was laid out in, in logical px.
    pub fn size(&self) -> (f32, f32) {
        (self.width, self.height)
    }

    /// The placement of a node; `None` for an id from another tree.
    pub fn get(&self, id: NodeId) -> Option<&Placed> {
        self.placed.get(id.index())
    }

    /// The scroll container's box; `None` for a node that isn't one.
    pub fn scroll_box(&self, id: NodeId) -> Option<&ScrollBox> {
        self.get(id).and_then(|p| p.scroll.as_ref())
    }

    /// The node's border box.
    pub fn rect(&self, id: NodeId) -> Option<Rect> {
        self.get(id).map(|p| p.rect)
    }

    /// The node's content box.
    pub fn content(&self, id: NodeId) -> Option<Rect> {
        self.get(id).map(|p| p.content)
    }

    /// A text node's shaped text.
    pub fn text(&self, id: NodeId) -> Option<&TextLayout> {
        self.get(id).and_then(|p| p.text.as_ref())
    }

    /// Every node in document order, which is also paint order: a parent, then its
    /// children in order.
    pub fn order(&self) -> &[NodeId] {
        &self.order
    }

    /// The topmost node at a point in logical px: the last in paint order whose visible
    /// area holds it. `None` when nothing does. A scroll container hides what its
    /// descendants scroll out of its box, so a point outside the box never hits them.
    ///
    /// This is a linear scan over every node, last to first: call it once per pointer
    /// event, never once per node.
    pub fn hit(&self, x: f64, y: f64) -> Option<NodeId> {
        self.order
            .iter()
            .rev()
            .copied()
            .find(|&id| self.placed[id.index()].visible.contains(x, y))
    }
}
