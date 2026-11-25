//! The font families a skin may name (design 27 §3, design 24 decision 10).
//!
//! The app bundles two families and names them as their font files name themselves:
//! Noto Sans, a variable font with a `wdth` axis, and JetBrains Mono, which is static.
//! A `font-family` must be one of these.
//!
//! Skin-shipped OTF/TTF files are accepted as assets, but reading the family name out of
//! one takes a font parser, which this crate may not depend on (design 24 §2.1). Until a
//! later PR gives the loader the family names of the skin's own fonts, a family that isn't
//! bundled is a load error.
// follow-up: read the family names of skin-shipped fonts and accept them here.

/// A bundled font family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Family {
    pub name: &'static str,
    /// Has a `wdth` axis: only these accept `stretch` (design 27 §5.2).
    pub variable: bool,
}

/// The bundled families.
pub const FAMILIES: &[Family] = &[
    Family {
        name: "Noto Sans",
        variable: true,
    },
    Family {
        name: "JetBrains Mono",
        variable: false,
    },
];

/// The bundled family called `name` (exact, case-sensitive).
pub fn family(name: &str) -> Option<&'static Family> {
    FAMILIES.iter().find(|f| f.name == name)
}

/// The bundled family names, for a help line.
pub(crate) fn names() -> String {
    FAMILIES
        .iter()
        .map(|f| format!("`{}`", f.name))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_families() {
        assert!(family("Noto Sans").is_some_and(|f| f.variable));
        assert!(family("JetBrains Mono").is_some_and(|f| !f.variable));
        assert!(family("JetBrainsMono Nerd Font").is_none());
        assert!(family("noto sans").is_none());
    }
}
