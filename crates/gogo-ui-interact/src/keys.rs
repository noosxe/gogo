//! The skin's shortcut strings (`key="P"`, a tab's digit).

use crate::input::{Key, Modifiers};

/// Whether `key` with `mods` is the shortcut `spec`. Reserved keys never are: Esc,
/// Enter, Space, Tab, and K with Ctrl or Meta (S19).
pub(crate) fn matches(spec: &str, key: Key, mods: Modifiers) -> bool {
    let mut want = Modifiers::default();
    let mut tokens = spec.split([' ', '+']).filter(|t| !t.is_empty()).peekable();
    while let Some(token) = tokens.next() {
        if tokens.peek().is_some() {
            match token.to_ascii_lowercase().as_str() {
                "ctrl" => want.ctrl = true,
                "shift" => want.shift = true,
                "alt" => want.alt = true,
                "cmd" | "meta" => want.meta = true,
                _ => return false,
            }
            continue;
        }
        let mut chars = token.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            // Named keys are all reserved.
            return false;
        };
        let spec_key = Key::Char(c).normal();
        let reserved = spec_key == Key::Char('k') && (want.ctrl || want.meta);
        return !reserved && spec_key == key.normal() && want == mods;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers {
        ctrl: false,
        shift: false,
        alt: false,
        meta: false,
    };
    const CTRL: Modifiers = Modifiers { ctrl: true, ..NONE };

    #[test]
    fn letters_digits_and_written_modifiers() {
        assert!(matches("P", Key::Char('p'), NONE));
        assert!(matches("P", Key::Char('P'), NONE));
        assert!(matches("1", Key::Char('1'), NONE));
        assert!(matches("Ctrl R", Key::Char('r'), CTRL));
        assert!(matches("Ctrl+R", Key::Char('r'), CTRL));
        assert!(!matches("P", Key::Char('p'), CTRL));
        assert!(!matches("Ctrl R", Key::Char('r'), NONE));
        assert!(!matches("", Key::Char('p'), NONE));
        assert!(!matches("Hyper R", Key::Char('r'), NONE));
    }

    #[test]
    fn s19_reserved_keys_never_match() {
        assert!(!matches("Ctrl K", Key::Char('k'), CTRL));
        let meta = Modifiers { meta: true, ..NONE };
        assert!(!matches("Cmd K", Key::Char('k'), meta));
        for (spec, key) in [
            ("Enter", Key::Enter),
            ("Esc", Key::Escape),
            ("Space", Key::Space),
            ("Tab", Key::Tab),
        ] {
            assert!(!matches(spec, key, NONE), "{spec}");
        }
        assert!(!matches(" ", Key::Space, NONE));
    }
}
