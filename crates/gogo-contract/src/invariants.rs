//! The safety invariants S1–S21 (design 26 §7). Numbered so tests can name them; where
//! each is enforced is design 25 §4.

/// A safety invariant, by number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Invariant {
    S1,
    S2,
    S3,
    S4,
    S5,
    S6,
    S7,
    S8,
    S9,
    S10,
    S11,
    S12,
    S13,
    S14,
    S15,
    S16,
    S17,
    S18,
    S19,
    S20,
    S21,
}

impl Invariant {
    pub const ALL: &[Invariant] = &[
        Self::S1,
        Self::S2,
        Self::S3,
        Self::S4,
        Self::S5,
        Self::S6,
        Self::S7,
        Self::S8,
        Self::S9,
        Self::S10,
        Self::S11,
        Self::S12,
        Self::S13,
        Self::S14,
        Self::S15,
        Self::S16,
        Self::S17,
        Self::S18,
        Self::S19,
        Self::S20,
        Self::S21,
    ];

    /// `"S1"` … `"S21"`.
    pub const fn id(self) -> &'static str {
        self.entry().0
    }

    /// The invariant's one-line statement, as the doc heads it.
    pub const fn title(self) -> &'static str {
        self.entry().1
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|i| i.id() == id)
    }

    const fn entry(self) -> (&'static str, &'static str) {
        match self {
            Self::S1 => ("S1", "No credentials in any source"),
            Self::S2 => ("S2", "Secret env values are masked"),
            Self::S3 => ("S3", "No path, URL or command comes from a skin"),
            Self::S4 => ("S4", "Actions fire only from user input"),
            Self::S5 => (
                "S5",
                "Every action whose computed plan is non-empty shows its plan first",
            ),
            Self::S6 => ("S6", "Destructive actions need strong confirmation"),
            Self::S7 => ("S7", "App widgets stay legible"),
            Self::S8 => ("S8", "Cancel is always there"),
            Self::S9 => ("S9", "Errors are uniform"),
            Self::S10 => ("S10", "Skin files stay in the skin folder"),
            Self::S11 => ("S11", "No network for skins"),
            Self::S12 => ("S12", "Bounded loading"),
            Self::S13 => ("S13", "Keyboard reachable"),
            Self::S14 => ("S14", "Named for screen readers"),
            Self::S15 => ("S15", "Reduced motion wins"),
            Self::S16 => ("S16", "The contract is checked before use"),
            Self::S17 => ("S17", "No private shortcuts"),
            Self::S18 => ("S18", "Status never by colour alone"),
            Self::S19 => ("S19", "Global keys are the app's"),
            Self::S20 => ("S20", "Skins don't run code"),
            Self::S21 => ("S21", "Skins can't weaken the safety flow through settings"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_numbered_in_order() {
        for (i, inv) in Invariant::ALL.iter().enumerate() {
            assert_eq!(inv.id(), format!("S{}", i + 1));
            assert_eq!(Invariant::from_id(inv.id()), Some(*inv));
        }
        assert_eq!(Invariant::from_id("S22"), None);
    }
}
