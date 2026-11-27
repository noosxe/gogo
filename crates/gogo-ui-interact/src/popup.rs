//! The app's open popups, and where one goes.

use gogo_ui_layout::Rect;

use crate::frame::LayerId;

/// Popup ids start here, away from the small numbers and hashes an app may use.
const FIRST_ID: u64 = 1 << 63;

/// The popups that are open, bottom first. Each opening gets a new [`LayerId`], so
/// focus and a late press never carry over from one that closed.
#[derive(Debug)]
pub struct PopupStack<T> {
    open: Vec<(LayerId, T)>,
    next: u64,
}

impl<T> Default for PopupStack<T> {
    fn default() -> Self {
        PopupStack {
            open: Vec::new(),
            next: FIRST_ID,
        }
    }
}

impl<T> PopupStack<T> {
    /// Opens a popup over the others, and gives its layer id.
    pub fn open(&mut self, popup: T) -> LayerId {
        let id = LayerId(self.next);
        self.next += 1;
        self.open.push((id, popup));
        id
    }

    /// Closes the top popup: Esc, or a press outside it.
    pub fn dismiss_top(&mut self) -> Option<T> {
        self.open.pop().map(|(_, popup)| popup)
    }

    /// Closes all of them. Whether any was open.
    pub fn clear(&mut self) -> bool {
        let any = !self.open.is_empty();
        self.open.clear();
        any
    }

    /// The top popup.
    pub fn top(&self) -> Option<(LayerId, &T)> {
        self.open.last().map(|(id, popup)| (*id, popup))
    }

    /// The top popup, to change.
    pub fn top_mut(&mut self) -> Option<&mut T> {
        self.open.last_mut().map(|(_, popup)| popup)
    }

    /// Every open popup, bottom first.
    pub fn iter(&self) -> impl Iterator<Item = (LayerId, &T)> {
        self.open.iter().map(|(id, popup)| (*id, popup))
    }

    /// Whether none is open.
    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }
}

/// Where a popup goes, in logical px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// The most it may be tall there.
    pub max_height: f64,
    /// Whether it is above its anchor.
    pub above: bool,
}

/// Places a popup of `size` by the control at `anchor`, in a window of `window` logical px:
/// below it and aligned with its left edge; above when it doesn't fit below and there is
/// more room above; shifted to stay `margin` px inside the window.
pub fn place(
    anchor: Rect,
    (width, height): (f64, f64),
    (window_width, window_height): (f64, f64),
    gap: f64,
    margin: f64,
) -> Placement {
    let below = (window_height - margin - anchor.y1 - gap).max(0.0);
    let over = (anchor.y0 - gap - margin).max(0.0);
    let above = height > below && over > below;
    let max_height = if above { over } else { below };
    let y = if above {
        anchor.y0 - gap - height.min(max_height)
    } else {
        anchor.y1 + gap
    };
    let x = anchor.x0.min(window_width - margin - width).max(margin);
    Placement {
        x,
        y,
        max_height,
        above,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x0: f64, y0: f64) -> Rect {
        Rect {
            x0,
            y0,
            x1: x0 + 100.0,
            y1: y0 + 30.0,
        }
    }

    const WINDOW: (f64, f64) = (800.0, 600.0);

    #[test]
    fn below_the_anchor_when_it_fits() {
        let p = place(at(50.0, 100.0), (200.0, 120.0), WINDOW, 4.0, 8.0);
        assert_eq!((p.x, p.y, p.above), (50.0, 134.0, false));
        assert_eq!(p.max_height, 600.0 - 8.0 - 134.0);
    }

    #[test]
    fn above_when_it_only_fits_there() {
        let p = place(at(50.0, 540.0), (200.0, 120.0), WINDOW, 4.0, 8.0);
        assert!(p.above);
        assert_eq!(p.y, 540.0 - 4.0 - 120.0);
    }

    #[test]
    fn kept_inside_the_window_and_capped_where_it_is_too_tall() {
        let p = place(at(750.0, 200.0), (200.0, 900.0), WINDOW, 4.0, 8.0);
        assert_eq!(p.x, 800.0 - 8.0 - 200.0);
        assert!(!p.above);
        assert_eq!(p.max_height, 600.0 - 8.0 - 234.0);
        let p = place(at(-20.0, 200.0), (200.0, 40.0), WINDOW, 4.0, 8.0);
        assert_eq!(p.x, 8.0);
    }

    #[test]
    fn the_stack_closes_from_the_top_with_a_new_id_each_time() {
        let mut stack = PopupStack::default();
        let a = stack.open("menu");
        let b = stack.open("palette");
        assert_ne!(a, b);
        assert_eq!(stack.top(), Some((b, &"palette")));
        assert_eq!(stack.dismiss_top(), Some("palette"));
        assert_eq!(stack.top(), Some((a, &"menu")));
        assert!(stack.clear());
        assert!(!stack.clear());
        assert!(stack.is_empty());
        assert_ne!(stack.open("menu"), a);
    }
}
