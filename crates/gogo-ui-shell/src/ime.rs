//! The input method through winit: its events as input, and telling it whether text is
//! wanted and where.

use gogo_ui_interact::{ImeInput, Input};
use winit::dpi::{LogicalPosition, LogicalSize};
use winit::event::Ime;
use winit::window::Window;

/// Where text is being typed, in logical px: the input method puts its candidates
/// beside it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImeArea {
    /// From the left edge of the window.
    pub x: f64,
    /// From its top.
    pub y: f64,
    /// The width.
    pub width: f64,
    /// The height.
    pub height: f64,
}

pub(crate) fn input(event: &Ime) -> Option<Input> {
    Some(Input::Ime(match event {
        Ime::Enabled => return None,
        Ime::Preedit(text, cursor) => ImeInput::Preedit {
            text: text.clone(),
            cursor: *cursor,
        },
        Ime::Commit(text) => ImeInput::Commit(text.clone()),
        Ime::Disabled => ImeInput::Disabled,
    }))
}

/// What the window was last told.
#[derive(Debug, Default)]
pub(crate) struct Wanted(Option<ImeArea>);

impl Wanted {
    /// What changes when the application now wants `area`: whether text is allowed,
    /// and where it goes.
    fn change(&mut self, area: Option<ImeArea>) -> (Option<bool>, Option<ImeArea>) {
        if area == self.0 {
            return (None, None);
        }
        let allowed = (area.is_some() != self.0.is_some()).then_some(area.is_some());
        self.0 = area;
        (allowed, area)
    }

    /// An input method is off until a field has focus, so nothing else is composed.
    pub(crate) fn tell(&mut self, window: &Window, area: Option<ImeArea>) {
        let (allowed, area) = self.change(area);
        if let Some(allowed) = allowed {
            window.set_ime_allowed(allowed);
        }
        if let Some(a) = area {
            window.set_ime_cursor_area(
                LogicalPosition::new(a.x, a.y),
                LogicalSize::new(a.width, a.height),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_become_input() {
        assert_eq!(input(&Ime::Enabled), None);
        assert_eq!(
            input(&Ime::Preedit("ni".into(), Some((2, 2)))),
            Some(Input::Ime(ImeInput::Preedit {
                text: "ni".into(),
                cursor: Some((2, 2))
            }))
        );
        assert_eq!(
            input(&Ime::Commit("x".into())),
            Some(Input::Ime(ImeInput::Commit("x".into())))
        );
        assert_eq!(input(&Ime::Disabled), Some(Input::Ime(ImeInput::Disabled)));
    }

    #[test]
    fn the_window_is_told_only_what_changed() {
        let at = |x| ImeArea {
            x,
            y: 1.0,
            width: 2.0,
            height: 3.0,
        };
        let mut wanted = Wanted::default();
        assert_eq!(wanted.change(None), (None, None));
        assert_eq!(wanted.change(Some(at(1.0))), (Some(true), Some(at(1.0))));
        assert_eq!(wanted.change(Some(at(1.0))), (None, None));
        assert_eq!(wanted.change(Some(at(5.0))), (None, Some(at(5.0))));
        assert_eq!(wanted.change(None), (Some(false), None));
    }
}
