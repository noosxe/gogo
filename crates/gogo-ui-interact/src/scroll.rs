//! The offsets of the scroll containers, and what a scrolled frame lets input see
//! (design 25 §4).

use std::collections::HashMap;

use gogo_ui_ir::{DrawnBy, NodeId};
use gogo_ui_layout::ScrollBox;

use crate::frame::{Frame, Layer, LayerId, LayerKind};

/// One wheel line, or one press of an arrow key, in logical px.
pub const SCROLL_LINE: f64 = 48.0;

/// A page key or a click beside the thumb moves this share of what is seen.
const PAGE: f64 = 0.9;

/// The room left around a control that is scrolled into view, for its focus ring.
const MARGIN: f64 = 8.0;

/// How many offsets are remembered before those of other screens are let go.
const REMEMBERED: usize = 256;

/// Whose offset: a route's own, or one every route shares, and the container's key.
type ScrollId = (Option<LayerId>, String);

/// A scrollbar thumb being dragged.
#[derive(Debug, Clone)]
pub(crate) struct Drag {
    pub layer: LayerId,
    pub key: String,
    /// From the top of the thumb to the pointer.
    pub grab: f64,
}

#[derive(Debug, Default)]
pub(crate) struct ScrollState {
    offsets: HashMap<ScrollId, f64>,
    /// Changes with every offset, so a scroll asks for a frame.
    pub revision: u64,
    pub drag: Option<Drag>,
}

fn id(layer: &Layer<'_>, node: NodeId) -> Option<ScrollId> {
    let spec = layer.tree.get(node)?.scroll.as_ref()?;
    let key = layer.tree.key(node)?.to_owned();
    Some((spec.per_route.then_some(layer.id), key))
}

impl ScrollState {
    /// The offsets that apply to the containers of `layer`, by key.
    pub fn of(&self, layer: LayerId) -> impl Iterator<Item = (&str, f32)> {
        self.offsets
            .iter()
            .filter(move |((scope, _), _)| scope.is_none_or(|s| s == layer))
            .map(|((_, key), &y)| (key.as_str(), y as f32))
    }

    /// Where the container is, counting what has not reached the display yet.
    pub fn current(&self, layer: &Layer<'_>, node: NodeId) -> Option<f64> {
        let shown = layer.laid.scroll_box(node)?;
        let stored = id(layer, node).and_then(|id| self.offsets.get(&id).copied());
        Some(stored.unwrap_or(shown.offset).clamp(0.0, shown.max))
    }

    /// Moves the container to `y`, as far as its content goes. Returns whether it moved.
    pub fn set(&mut self, layer: &Layer<'_>, node: NodeId, y: f64) -> bool {
        let (Some(shown), Some(id), Some(old)) = (
            layer.laid.scroll_box(node),
            id(layer, node),
            self.current(layer, node),
        ) else {
            return false;
        };
        let y = y.clamp(0.0, shown.max);
        if !y.is_finite() || y == old {
            return false;
        }
        self.offsets.insert(id, y);
        self.revision += 1;
        true
    }

    /// Keeps the memory bounded.
    pub fn trim(&mut self, frame: &Frame<'_>) {
        if self.offsets.len() > REMEMBERED {
            self.offsets.retain(|(scope, _), _| {
                scope.is_none_or(|s| frame.layers.iter().any(|l| l.id == s))
            });
        }
    }
}

/// `node` and everything above it, from the node up.
pub(crate) fn chain<'a>(layer: &'a Layer<'_>, node: NodeId) -> impl Iterator<Item = NodeId> + 'a {
    std::iter::successors(Some(node), |&n| layer.tree.parent(n))
}

/// The scroll containers at and above `node`, innermost first.
pub(crate) fn containers<'a>(
    layer: &'a Layer<'_>,
    node: NodeId,
) -> impl Iterator<Item = (NodeId, ScrollBox)> + 'a {
    chain(layer, node).filter_map(|n| Some((n, *layer.laid.scroll_box(n)?)))
}

/// Whether the control is in sight enough to be activated without the pointer: its
/// centre inside what its scroll containers show, or for a control the app drew, all of it.
/// A control in no scroll container always is.
pub(crate) fn seen(layer: &Layer<'_>, node: NodeId) -> bool {
    let scrolled = chain(layer, node)
        .skip(1)
        .any(|n| layer.laid.get(n).is_some_and(|p| p.clips));
    if !scrolled {
        return true;
    }
    let (Some(n), Some(p)) = (layer.tree.get(node), layer.laid.get(node)) else {
        return false;
    };
    let (r, v) = (p.rect, p.visible);
    if matches!(n.drawn_by, DrawnBy::App(_)) {
        // Half a pixel of rounding is not a cut.
        v.width() >= r.width() - 0.5 && v.height() >= r.height() - 0.5
    } else {
        v.contains((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0)
    }
}

/// The offsets that bring `node` into view, innermost container first. Reckoned from
/// where the frame shows it.
pub(crate) fn into_view(layer: &Layer<'_>, node: NodeId) -> Vec<(NodeId, f64)> {
    let Some(mut rect) = layer.laid.rect(node) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (container, shown) in containers(layer, node).filter(|(n, _)| *n != node) {
        let above = rect.y0 - MARGIN - shown.viewport.y0;
        let below = rect.y1 + MARGIN - shown.viewport.y1;
        let delta = if above < 0.0 {
            above
        } else if below > 0.0 {
            // Never past the top of something taller than what is seen.
            below.min(above)
        } else {
            0.0
        };
        let to = (shown.offset + delta).clamp(0.0, shown.max);
        if to != shown.offset {
            out.push((container, to));
            rect = rect.translate(0.0, shown.offset - to);
        }
    }
    out
}

/// The topmost node at a point, among the layers that take input.
fn top<'a, 'f>(frame: &Frame<'f>, x: f64, y: f64) -> Option<(&'a Layer<'f>, NodeId)>
where
    'f: 'a,
{
    frame.interactive().iter().rev().find_map(|layer| {
        let node = layer.laid.hit(x, y)?;
        (layer.kind != LayerKind::Overlay || node != layer.tree.root()).then_some((layer, node))
    })
}

/// Whether the point is in the gutter of a scroll container that has something to scroll.
pub(crate) fn in_bar(layer: &Layer<'_>, node: NodeId, x: f64, y: f64) -> bool {
    layer
        .laid
        .scroll_box(node)
        .is_some_and(|b| b.max > 0.0 && b.track.contains(x, y))
}

/// The scrollbar at a point.
pub(crate) fn bar_at<'a, 'f>(frame: &Frame<'f>, x: f64, y: f64) -> Option<(&'a Layer<'f>, NodeId)>
where
    'f: 'a,
{
    top(frame, x, y).filter(|&(layer, node)| in_bar(layer, node, x, y))
}

/// The innermost container under a point that can still move toward `dy`.
pub(crate) fn wheel_target<'a, 'f>(
    state: &ScrollState,
    frame: &Frame<'f>,
    (x, y): (f64, f64),
    dy: f64,
) -> Option<(&'a Layer<'f>, NodeId)>
where
    'f: 'a,
{
    let (layer, node) = top(frame, x, y)?;
    let found = containers(layer, node).find(|&(n, shown)| {
        state
            .current(layer, n)
            .is_some_and(|at| if dy > 0.0 { at < shown.max } else { at > 0.0 })
    });
    found.map(|(n, _)| (layer, n))
}

/// The thumb's offset when its top is `top` px down the window.
pub(crate) fn dragged(shown: &ScrollBox, top: f64) -> f64 {
    let free = shown.track.height() - shown.thumb.height();
    if free <= 0.0 {
        return 0.0;
    }
    (top - shown.track.y0) / free * shown.max
}

/// How far a page moves in this container.
pub(crate) fn page(shown: &ScrollBox) -> f64 {
    shown.viewport.height() * PAGE
}
