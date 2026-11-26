//! winit's events as `gogo-ui-interact`'s [`Input`] (design 25 §4). Nothing is decided
//! here: what a press means is the interaction layer's business.

use gogo_ui_interact::{Input, Key, Modifiers, PointerButton};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};

/// What translating needs to remember between events.
#[derive(Debug, Default)]
pub(crate) struct Translator {
    modifiers: ModifiersState,
    /// The last position of the pointer in the window, in logical px.
    pointer: Option<(f64, f64)>,
}

impl Translator {
    /// The input `event` stands for, if any. `scale` is device px per logical px.
    pub(crate) fn translate(&mut self, event: &WindowEvent, scale: f64) -> Option<Input> {
        match event {
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
                None
            }
            WindowEvent::CursorMoved { position, .. } => Some(self.moved(*position, scale)),
            WindowEvent::CursorLeft { .. } => {
                self.pointer = None;
                Some(Input::PointerLeft)
            }
            WindowEvent::MouseInput { state, button, .. } => self.button(*state, *button),
            // The platform makes up key events when the window gains or loses focus: they
            // are not presses.
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } => Some(key_input(
                map_key(&event.logical_key),
                event.state,
                event.repeat,
                self.modifiers,
            )),
            WindowEvent::Focused(false) => Some(Input::FocusLost),
            _ => None,
        }
    }

    fn moved(&mut self, position: PhysicalPosition<f64>, scale: f64) -> Input {
        let (x, y) = (position.x / scale, position.y / scale);
        self.pointer = Some((x, y));
        Input::PointerMoved { x, y }
    }

    fn button(&self, state: ElementState, button: MouseButton) -> Option<Input> {
        // A button without a position in the window has nothing to land on.
        let (x, y) = self.pointer?;
        let button = match button {
            MouseButton::Left => PointerButton::Primary,
            MouseButton::Right => PointerButton::Secondary,
            _ => PointerButton::Other,
        };
        Some(match state {
            ElementState::Pressed => Input::PointerDown { button, x, y },
            ElementState::Released => Input::PointerUp { button, x, y },
        })
    }
}

fn key_input(key: Key, state: ElementState, repeat: bool, held: ModifiersState) -> Input {
    match state {
        ElementState::Pressed => Input::KeyDown {
            key,
            mods: Modifiers {
                ctrl: held.control_key(),
                shift: held.shift_key(),
                alt: held.alt_key(),
                meta: held.super_key(),
            },
            repeat,
        },
        ElementState::Released => Input::KeyUp { key },
    }
}

/// A key by meaning. A bare modifier, or anything else without one here, is
/// [`Key::Other`].
fn map_key(key: &WinitKey) -> Key {
    match key {
        WinitKey::Named(named) => match named {
            NamedKey::Enter => Key::Enter,
            NamedKey::Space => Key::Space,
            NamedKey::Escape => Key::Escape,
            NamedKey::Tab => Key::Tab,
            NamedKey::ArrowLeft => Key::ArrowLeft,
            NamedKey::ArrowRight => Key::ArrowRight,
            NamedKey::ArrowUp => Key::ArrowUp,
            NamedKey::ArrowDown => Key::ArrowDown,
            _ => Key::Other,
        },
        WinitKey::Character(text) => {
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Key::Char(c),
                _ => Key::Other,
            }
        }
        _ => Key::Other,
    }
}

#[cfg(test)]
mod tests {
    use winit::event::DeviceId;

    use super::*;

    fn moved(t: &mut Translator, x: f64, y: f64, scale: f64) -> Option<Input> {
        t.translate(
            &WindowEvent::CursorMoved {
                device_id: DeviceId::dummy(),
                position: PhysicalPosition::new(x, y),
            },
            scale,
        )
    }

    fn click(t: &mut Translator, state: ElementState, button: MouseButton) -> Option<Input> {
        t.translate(
            &WindowEvent::MouseInput {
                device_id: DeviceId::dummy(),
                state,
                button,
            },
            2.0,
        )
    }

    #[test]
    fn the_pointer_is_in_logical_px() {
        let mut t = Translator::default();
        assert_eq!(
            click(&mut t, ElementState::Pressed, MouseButton::Left),
            None
        );
        assert_eq!(
            moved(&mut t, 200.0, 50.0, 2.0),
            Some(Input::PointerMoved { x: 100.0, y: 25.0 })
        );
        assert_eq!(
            click(&mut t, ElementState::Pressed, MouseButton::Right),
            Some(Input::PointerDown {
                button: PointerButton::Secondary,
                x: 100.0,
                y: 25.0
            })
        );
        let left = t.translate(
            &WindowEvent::CursorLeft {
                device_id: DeviceId::dummy(),
            },
            2.0,
        );
        assert_eq!(left, Some(Input::PointerLeft));
        assert_eq!(
            click(&mut t, ElementState::Released, MouseButton::Left),
            None
        );
    }

    #[test]
    fn keys_by_meaning() {
        assert_eq!(map_key(&WinitKey::Named(NamedKey::Tab)), Key::Tab);
        assert_eq!(map_key(&WinitKey::Named(NamedKey::Shift)), Key::Other);
        assert_eq!(map_key(&WinitKey::Character("K".into())), Key::Char('K'));
        assert_eq!(map_key(&WinitKey::Character("ab".into())), Key::Other);
    }

    #[test]
    fn a_press_carries_the_modifiers_and_a_release_does_not() {
        let held = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert_eq!(
            key_input(Key::Tab, ElementState::Pressed, true, held),
            Input::KeyDown {
                key: Key::Tab,
                mods: Modifiers {
                    ctrl: true,
                    shift: true,
                    alt: false,
                    meta: false
                },
                repeat: true
            }
        );
        assert_eq!(
            key_input(Key::Tab, ElementState::Released, false, held),
            Input::KeyUp { key: Key::Tab }
        );
    }

    #[test]
    fn losing_focus_is_reported() {
        let mut t = Translator::default();
        assert_eq!(
            t.translate(&WindowEvent::Focused(false), 1.0),
            Some(Input::FocusLost)
        );
        assert_eq!(t.translate(&WindowEvent::Focused(true), 1.0), None);
    }
}
