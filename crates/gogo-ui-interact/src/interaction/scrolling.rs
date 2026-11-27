//! What the wheel, the scrollbar, the keys and assistive technology do to the offsets
//! (design 25 §4). None of it is a press: nothing here takes an input number.

use gogo_ui_ir::{NodeId, Role};

use super::Interaction;
use crate::frame::{Frame, Layer, LayerId, Target};
use crate::input::{Key, RequestKind};
use crate::intent::Response;
use crate::scroll::{self, Drag, SCROLL_LINE};

impl Interaction {
    /// The offsets for the containers of `layer`, by tree key: what the app lays the
    /// next frame out with.
    pub fn offsets(&self, layer: LayerId) -> impl Iterator<Item = (&str, f32)> {
        self.scroll.of(layer)
    }

    /// Moves a container. A press, hold or arm on something inside it is cancelled: its
    /// control is not where it was pressed any more.
    fn scroll_to(
        &mut self,
        layer: &Layer<'_>,
        container: NodeId,
        y: f64,
        out: &mut Response,
    ) -> bool {
        if !self.scroll.set(layer, container, y) {
            return false;
        }
        let moved = |t: &Target| {
            t.layer == layer.id
                && layer
                    .tree
                    .by_key(&t.key)
                    .is_some_and(|node| scroll::chain(layer, node).skip(1).any(|n| n == container))
        };
        if self.pointer.as_ref().is_some_and(|p| moved(&p.target)) {
            self.pointer = None;
        }
        if self.space.as_ref().is_some_and(|p| moved(&p.target)) {
            self.space = None;
        }
        if self.armed.as_ref().is_some_and(|a| moved(&a.target)) {
            self.armed = None;
        }
        if self.hold.as_ref().is_some_and(|h| moved(&h.target)) {
            self.end_hold(out);
        }
        true
    }

    fn scroll_by(&mut self, layer: &Layer<'_>, node: NodeId, by: f64, out: &mut Response) {
        if let Some(at) = self.scroll.current(layer, node) {
            self.scroll_to(layer, node, at + by, out);
        }
    }

    /// Scrolls `node` into view.
    pub(super) fn reveal(&mut self, layer: &Layer<'_>, node: NodeId, out: &mut Response) {
        for (container, y) in scroll::into_view(layer, node) {
            self.scroll_to(layer, container, y, out);
        }
    }

    pub(super) fn reveal_target(&mut self, frame: &Frame<'_>, target: &Target, out: &mut Response) {
        if let Some((layer, node)) = super::find(frame, target.layer, &target.key) {
            self.reveal(layer, node, out);
        }
    }

    /// Tab on its way to `next`. Where it would pass over items of a virtualised list
    /// that aren't there yet, focus stays and the list scrolls on; the next press goes
    /// further.
    pub(super) fn tab_to(
        &mut self,
        frame: &Frame<'_>,
        next: Target,
        back: bool,
        out: &mut Response,
    ) {
        let Some((layer, to)) = super::find(frame, next.layer, &next.key) else {
            return;
        };
        let from = self
            .focus
            .as_ref()
            .filter(|f| f.layer == layer.id)
            .and_then(|f| layer.tree.by_key(&f.key));
        self.visible = true;
        match scroll::passed_over(layer, from, to, back) {
            Some(item) => self.reveal(layer, item, out),
            None => {
                self.reveal(layer, to, out);
                self.set_focus(Some(next), out);
            }
        }
    }

    pub(super) fn wheel(&mut self, at: (f64, f64), dy: f64, frame: &Frame<'_>, out: &mut Response) {
        if !dy.is_finite() || dy == 0.0 {
            return;
        }
        if let Some((layer, node)) = scroll::wheel_target(&self.scroll, frame, at, dy) {
            out.consumed = true;
            self.scroll_by(layer, node, dy, out);
        }
    }

    /// A press on a scrollbar: the thumb is dragged, beside it a page is turned.
    pub(super) fn bar_down(
        &mut self,
        x: f64,
        y: f64,
        frame: &Frame<'_>,
        out: &mut Response,
    ) -> bool {
        self.scroll.drag = None;
        let Some((layer, node)) = scroll::bar_at(frame, x, y) else {
            return false;
        };
        let (Some(shown), Some(key)) = (layer.laid.scroll_box(node), layer.tree.key(node)) else {
            return false;
        };
        self.cancel_presses(out);
        // A pointer press, even this one, drops a composition.
        self.end_composition(out);
        self.visible = false;
        self.hovered = None;
        out.consumed = true;
        if shown.thumb.contains(x, y) {
            self.scroll.drag = Some(Drag {
                layer: layer.id,
                key: key.to_owned(),
                grab: y - shown.thumb.y0,
            });
        } else {
            let way = if y < shown.thumb.y0 { -1.0 } else { 1.0 };
            self.scroll_by(layer, node, way * scroll::page(shown), out);
        }
        true
    }

    /// The pointer moved to `y` with a thumb held.
    pub(super) fn bar_drag(&mut self, y: f64, frame: &Frame<'_>, out: &mut Response) -> bool {
        let Some(drag) = self.scroll.drag.clone() else {
            return false;
        };
        let found = super::find(frame, drag.layer, &drag.key)
            .and_then(|(l, n)| Some((l, n, l.laid.scroll_box(n)?)));
        let Some((layer, node, shown)) = found else {
            self.scroll.drag = None;
            return false;
        };
        out.consumed = true;
        self.hovered = None;
        self.scroll_to(layer, node, scroll::dragged(shown, y - drag.grab), out);
        true
    }

    /// PageUp, PageDown, Home, End and the up and down arrows: the focused control's
    /// containers, else the page's largest.
    pub(super) fn scroll_key(&mut self, key: Key, frame: &Frame<'_>, out: &mut Response) {
        let typing = self.focus.as_ref().is_some_and(|f| {
            super::find(frame, f.layer, &f.key)
                .and_then(|(l, n)| l.tree.get(n))
                .is_some_and(|n| n.a11y.role == Role::TextInput)
        });
        if typing {
            return;
        }
        let back = matches!(key, Key::PageUp | Key::Home | Key::ArrowUp);
        let focused = self
            .focus
            .as_ref()
            .and_then(|f| super::find(frame, f.layer, &f.key))
            .filter(|&(l, n)| scroll::containers(l, n).any(|(_, b)| b.max > 0.0));
        let largest = || {
            frame
                .interactive()
                .iter()
                .flat_map(|layer| {
                    layer
                        .tree
                        .iter()
                        .filter_map(move |n| Some((layer, n, layer.laid.scroll_box(n)?)))
                })
                .filter(|(_, _, b)| b.max > 0.0)
                .map(|(l, n, b)| (b.viewport.width() * b.viewport.height(), l, n))
                .max_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, l, n)| (l, n))
        };
        let Some((layer, from)) = focused.or_else(largest) else {
            return;
        };
        let found = scroll::containers(layer, from).find(|&(n, shown)| {
            self.scroll
                .current(layer, n)
                .is_some_and(|at| if back { at > 0.0 } else { at < shown.max })
        });
        let Some((node, shown)) = found else {
            return;
        };
        out.consumed = true;
        let by = |amount: f64| if back { -amount } else { amount };
        match key {
            Key::Home => {
                self.scroll_to(layer, node, 0.0, out);
            }
            Key::End => {
                self.scroll_to(layer, node, shown.max, out);
            }
            Key::PageUp | Key::PageDown => {
                self.scroll_by(layer, node, by(scroll::page(&shown)), out);
            }
            _ => self.scroll_by(layer, node, by(SCROLL_LINE), out),
        }
    }

    /// A screen reader's scroll requests. They press nothing, so they need no identity.
    /// Returns whether `kind` was one.
    pub(super) fn assist_scroll(
        &mut self,
        layer: &Layer<'_>,
        node: NodeId,
        kind: &RequestKind,
        out: &mut Response,
    ) -> bool {
        match *kind {
            RequestKind::Reveal => self.reveal(layer, node, out),
            RequestKind::ScrollBy { pages, back } => {
                if let Some(shown) = layer.laid.scroll_box(node) {
                    let amount = if pages {
                        scroll::page(shown)
                    } else {
                        SCROLL_LINE
                    };
                    self.scroll_by(layer, node, if back { -amount } else { amount }, out);
                }
            }
            RequestKind::ScrollTo(y) => {
                self.scroll_to(layer, node, f64::from(y), out);
            }
            _ => return false,
        }
        out.consumed = true;
        true
    }
}
