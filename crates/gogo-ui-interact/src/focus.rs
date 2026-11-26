//! The Tab order.

use crate::frame::{Frame, Target};

/// Every focusable control in Tab order: document order within a layer (S13), layers
/// from the bottom up, and only the layers that take input.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FocusOrder {
    targets: Vec<Target>,
}

impl FocusOrder {
    /// The order of a frame.
    pub fn of(frame: &Frame<'_>) -> FocusOrder {
        let targets = frame
            .interactive()
            .iter()
            .flat_map(|layer| {
                layer
                    .tree
                    .focus_order()
                    .into_iter()
                    .filter_map(move |node| layer.target(node))
            })
            .collect();
        FocusOrder { targets }
    }

    /// The controls in order.
    pub fn targets(&self) -> &[Target] {
        &self.targets
    }

    pub(crate) fn position(&self, target: &Target) -> Option<usize> {
        self.targets.iter().position(|t| t.same(target))
    }

    /// The control after `from` (or before it), wrapping; the first (or last) when
    /// `from` is not in the order.
    pub(crate) fn step(&self, from: Option<&Target>, back: bool) -> Option<&Target> {
        let n = self.targets.len();
        if n == 0 {
            return None;
        }
        let next = match from.and_then(|t| self.position(t)) {
            Some(i) if back => (i + n - 1) % n,
            Some(i) => (i + 1) % n,
            None if back => n - 1,
            None => 0,
        };
        self.targets.get(next)
    }
}
