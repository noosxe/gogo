//! winit's events as `gogo-ui-interact`'s [`Input`] (design 25 §4). Nothing is decided
//! here: what a press means is the interaction layer's business.

use gogo_ui_interact::{Input, Key, Modifiers, PointerButton};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key as WinitKey, KeyCode, ModifiersState, NamedKey, PhysicalKey};

/// What translating needs to remember between events.
#[derive(Debug, Default)]
pub(crate) struct Translator {
    modifiers: ModifiersState,
    /// Keys down that were read by where they are, to be let go of as the same key.
    chords: Vec<(PhysicalKey, Key)>,
    /// The last position of the pointer in the window, in logical px.
    pointer: Option<(f64, f64)>,
}

impl Translator {
    /// The input `event` stands for, if any. `scale` is device px per logical px.
    pub(crate) fn translate(&mut self, event: &WindowEvent, scale: f64) -> Option<Input> {
        match event {
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
                Some(Input::Modifiers(held(self.modifiers)))
            }
            WindowEvent::CursorMoved { position, .. } => Some(self.moved(*position, scale)),
            WindowEvent::CursorLeft { .. } => {
                self.pointer = None;
                Some(Input::PointerLeft)
            }
            WindowEvent::MouseInput { state, button, .. } => self.button(*state, *button),
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => self.key(
                &event.logical_key,
                event.physical_key,
                event.state,
                event.repeat,
                *is_synthetic,
            ),
            WindowEvent::Focused(false) => {
                // What was held is released somewhere else.
                self.modifiers = ModifiersState::empty();
                self.chords.clear();
                Some(Input::FocusLost)
            }
            _ => None,
        }
    }

    /// What a key press in `event` typed, to be sent after its
    /// [`Translator::translate`] input.
    pub(crate) fn text(&self, event: &WindowEvent) -> Option<Input> {
        match event {
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => typed(
                event.text.as_deref(),
                event.state,
                *is_synthetic,
                self.modifiers,
            ),
            _ => None,
        }
    }

    fn moved(&mut self, position: PhysicalPosition<f64>, scale: f64) -> Input {
        let (x, y) = (position.x / scale, position.y / scale);
        self.pointer = Some((x, y));
        Input::PointerMoved { x, y }
    }

    fn key(
        &mut self,
        key: &WinitKey,
        physical: PhysicalKey,
        state: ElementState,
        repeat: bool,
        synthetic: bool,
    ) -> Option<Input> {
        // The platform makes up key events when the window gains or loses focus: they are
        // not presses.
        if synthetic {
            return None;
        }
        let key = self.chord(map_key(key), physical, state);
        Some(key_input(key, state, repeat, self.modifiers))
    }

    /// An editing chord on a layout that types other letters there (Cyrillic, Greek,
    /// Armenian) is read by the key's place: Ctrl and the key left of V still copies.
    fn chord(&mut self, key: Key, physical: PhysicalKey, state: ElementState) -> Key {
        let kept = self.chords.iter().position(|(p, _)| *p == physical);
        if state == ElementState::Released {
            return kept.map_or(key, |i| self.chords.swap_remove(i).1);
        }
        if let Some(i) = kept {
            self.chords.swap_remove(i);
        }
        let chord = self.modifiers.control_key() || self.modifiers.super_key();
        let foreign = matches!(key, Key::Char(c) if !c.is_ascii());
        let letter = match physical {
            PhysicalKey::Code(KeyCode::KeyA) => 'a',
            PhysicalKey::Code(KeyCode::KeyC) => 'c',
            PhysicalKey::Code(KeyCode::KeyV) => 'v',
            PhysicalKey::Code(KeyCode::KeyX) => 'x',
            PhysicalKey::Code(KeyCode::KeyY) => 'y',
            PhysicalKey::Code(KeyCode::KeyZ) => 'z',
            _ => return key,
        };
        if !(chord && foreign) {
            return key;
        }
        self.chords.push((physical, Key::Char(letter)));
        Key::Char(letter)
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
            mods: self::held(held),
            repeat,
        },
        ElementState::Released => Input::KeyUp { key },
    }
}

fn held(held: ModifiersState) -> Modifiers {
    Modifiers {
        ctrl: held.control_key(),
        shift: held.shift_key(),
        alt: held.alt_key(),
        meta: held.super_key(),
    }
}

/// The text of a real press. Ctrl and Super chords type nothing; Alt may (Option on
/// macOS).
fn typed(
    text: Option<&str>,
    state: ElementState,
    synthetic: bool,
    held: ModifiersState,
) -> Option<Input> {
    let chord = held.control_key() || held.super_key();
    let text = text.filter(|t| !t.chars().all(char::is_control))?;
    (state == ElementState::Pressed && !synthetic && !chord).then(|| Input::Text(text.to_owned()))
}

/// A key by meaning. A bare modifier, or anything else without one here, is
/// [`Key::Other`].
fn map_key(key: &WinitKey) -> Key {
    match key {
        WinitKey::Named(named) => match named {
            NamedKey::Enter => Key::Enter,
            NamedKey::Space => Key::Space,
            NamedKey::Escape => Key::Escape,
            NamedKey::Backspace => Key::Backspace,
            NamedKey::Delete => Key::Delete,
            NamedKey::Home => Key::Home,
            NamedKey::End => Key::End,
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
    fn typed_text_keeps_its_case_and_chords_type_nothing() {
        let down = |text, held| typed(Some(text), ElementState::Pressed, false, held);
        let none = ModifiersState::empty();
        assert_eq!(down("É", none), Some(Input::Text("É".into())));
        assert_eq!(down(" ", none), Some(Input::Text(" ".into())));
        assert_eq!(
            down("@", ModifiersState::ALT),
            Some(Input::Text("@".into()))
        );
        assert_eq!(down("v", ModifiersState::CONTROL), None);
        assert_eq!(down("v", ModifiersState::SUPER), None);
        assert_eq!(down("\r", none), None);
        assert_eq!(typed(Some("a"), ElementState::Released, false, none), None);
        assert_eq!(typed(Some("a"), ElementState::Pressed, true, none), None);
        assert_eq!(
            map_key(&WinitKey::Named(NamedKey::Backspace)),
            Key::Backspace
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
    fn a_key_the_platform_made_up_is_dropped() {
        let mut t = Translator::default();
        let enter = WinitKey::Named(NamedKey::Enter);
        let place = PhysicalKey::Code(KeyCode::Enter);
        assert_eq!(
            t.key(&enter, place, ElementState::Pressed, false, true),
            None
        );
        assert_eq!(
            t.key(&enter, place, ElementState::Released, false, true),
            None
        );
        assert!(
            t.key(&enter, place, ElementState::Pressed, false, false)
                .is_some()
        );
    }

    #[test]
    fn losing_focus_lets_go_of_the_modifiers() {
        let mut t = Translator::default();
        t.translate(
            &WindowEvent::ModifiersChanged(ModifiersState::CONTROL.into()),
            1.0,
        );
        assert_eq!(t.modifiers, ModifiersState::CONTROL);
        t.translate(&WindowEvent::Focused(false), 1.0);
        assert_eq!(t.modifiers, ModifiersState::empty());
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

    #[test]
    fn a_chord_on_a_foreign_layout_is_read_by_the_keys_place() {
        let mut t = Translator::default();
        let v = PhysicalKey::Code(KeyCode::KeyV);
        let foreign = Key::Char('м');
        let down = ElementState::Pressed;
        let up = ElementState::Released;
        assert_eq!(t.chord(foreign, v, down), foreign, "typing is left alone");
        assert_eq!(t.chord(foreign, v, up), foreign);
        t.modifiers = ModifiersState::CONTROL;
        assert_eq!(t.chord(foreign, v, down), Key::Char('v'));
        assert_eq!(t.chord(foreign, v, down), Key::Char('v'), "repeating");
        // Ctrl is let go first: the release is still of the same key.
        t.modifiers = ModifiersState::empty();
        assert_eq!(t.chord(foreign, v, up), Key::Char('v'));
        assert_eq!(t.chord(foreign, v, up), foreign);
        // A layout with Latin letters keeps its own.
        t.modifiers = ModifiersState::CONTROL;
        assert_eq!(t.chord(Key::Char('k'), v, down), Key::Char('k'));
        assert_eq!(map_key(&WinitKey::Named(NamedKey::Delete)), Key::Delete);
    }
}
