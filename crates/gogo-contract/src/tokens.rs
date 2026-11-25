//! Semantic tokens (design 26 §6.1). App-owned widgets read only these; a skin may define
//! any number of its own, which app widgets ignore.

/// Colour tokens, status tones included.
pub const COLOURS: &[&str] = &[
    "bg0",
    "bg1",
    "surface",
    "raised",
    "border",
    "outline",
    "text-hi",
    "text-md",
    "text-lo",
    "accent",
    "on-accent",
    "focus",
    "success",
    "warn",
    "error",
    "info",
    "scrim",
    "tone.ready",
    "tone.running",
    "tone.downloading",
    "tone.setting_up",
    "tone.update",
    "tone.failed",
    "tone.not_installed",
    "tone.queued",
    "tone.paused",
];

/// Type roles. Each has a family, a size and a weight ([`FONT_FIELDS`]).
pub const FONT_ROLES: &[&str] = &[
    "font.display",
    "font.title",
    "font.body",
    "font.label",
    "font.mono",
];

/// The fields of a type role: `font.body.size`.
pub const FONT_FIELDS: &[&str] = &["family", "size", "weight"];

/// Shape tokens.
pub const SHAPE: &[&str] = &[
    "radius.sm",
    "radius.md",
    "radius.lg",
    "space.1",
    "space.2",
    "space.3",
    "space.4",
    "space.5",
    "space.6",
    "space.7",
    "space.8",
    "elevation.0",
    "elevation.1",
    "elevation.2",
    "elevation.3",
    "stroke.thin",
    "stroke.thick",
];

/// Motion tokens. Easing curves are open-ended: any `easing.<name>`, one segment
/// ([`EASING_PREFIX`]).
pub const MOTION: &[&str] = &["motion.fast", "motion.normal", "motion.slow"];

/// The prefix of easing tokens (`easing.*`).
pub const EASING_PREFIX: &str = "easing.";

/// Whether `name` is a semantic token: one of the lists above, a type role's field, or
/// an `easing.*` curve.
pub fn is_semantic(name: &str) -> bool {
    if COLOURS.contains(&name)
        || FONT_ROLES.contains(&name)
        || SHAPE.contains(&name)
        || MOTION.contains(&name)
    {
        return true;
    }
    if let Some(curve) = name.strip_prefix(EASING_PREFIX) {
        return !curve.is_empty() && !curve.contains('.');
    }
    FONT_ROLES.iter().any(|role| {
        name.strip_prefix(role)
            .and_then(|rest| rest.strip_prefix('.'))
            .is_some_and(|field| FONT_FIELDS.contains(&field))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_names() {
        for t in [
            "accent",
            "tone.failed",
            "font.body",
            "font.mono.weight",
            "space.8",
            "easing.out",
        ] {
            assert!(is_semantic(t), "{t}");
        }
        for t in [
            "space.9",
            "easing.",
            "font.body.colour",
            "brand-red",
            "tone.installed",
            "easing.a.b",
        ] {
            assert!(!is_semantic(t), "{t}");
        }
    }
}
