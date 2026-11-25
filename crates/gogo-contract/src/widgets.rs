//! App-owned widgets (design 26 §5): the widgets that carry the safety flow or secrets,
//! so the app draws them. A skin styles them through semantic tokens and, at depth B,
//! through named parts and a fixed set of variants. Unknown part names and variant
//! values are load errors.

/// How much of a widget a skin may change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Depth {
    /// Tokens only: colours, fonts, radius, elevation and motion. Always modal, centred.
    A,
    /// Tokens, per-part styles, the variants and the placement.
    B,
    /// Stock components of the default skin: a skin may copy, change or replace them.
    C,
}

/// A named part of a widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Part {
    pub name: &'static str,
    /// A skin may hide it with `part "<name>" hidden=#true`. Every other part is always
    /// drawn.
    pub optional: bool,
}

/// A variant property and its allowed values; the first is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Variant {
    pub name: &'static str,
    pub values: &'static [&'static str],
}

/// Where a depth-B widget may be placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Placement {
    /// In a slot the skin's tree names.
    Inline,
    /// Over the route, centred.
    Modal,
}

/// An app-owned widget, or a stock component (depth C).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct AppWidget {
    pub name: &'static str,
    pub depth: Depth,
    pub parts: &'static [Part],
    pub variants: &'static [Variant],
    /// The placements a skin may choose. Empty: the app places it.
    pub placements: &'static [Placement],
}

impl AppWidget {
    pub fn part(&self, name: &str) -> Option<&Part> {
        self.parts.iter().find(|p| p.name == name)
    }

    pub fn variant(&self, name: &str) -> Option<&Variant> {
        self.variants.iter().find(|v| v.name == name)
    }

    /// Whether a skin may hide part `name`. False for unknown parts.
    pub fn may_hide(&self, name: &str) -> bool {
        self.part(name).is_some_and(|p| p.optional)
    }
}

const fn req(name: &'static str) -> Part {
    Part {
        name,
        optional: false,
    }
}

const fn opt(name: &'static str) -> Part {
    Part {
        name,
        optional: true,
    }
}

const fn variant(name: &'static str, values: &'static [&'static str]) -> Variant {
    Variant { name, values }
}

const fn widget(
    name: &'static str,
    depth: Depth,
    parts: &'static [Part],
    variants: &'static [Variant],
) -> AppWidget {
    AppWidget {
        name,
        depth,
        parts,
        variants,
        placements: &[],
    }
}

const fn stock(name: &'static str) -> AppWidget {
    widget(name, Depth::C, &[], &[])
}

/// Every app-owned widget and stock component in contract 1.0, in the doc's order.
///
/// The prototype built `plan-card` (inline; the `labels`, `layout` and `markers` variants;
/// all parts but `step.kind-icon`, `totals` and `warning`) and `env-table` (all parts but
/// `source`). The rest is specified but not yet built, and each entry is reconciled when
/// its widget is.
pub const WIDGETS: &[AppWidget] = &[
    AppWidget {
        name: "plan-card",
        depth: Depth::B,
        parts: &[
            req("frame"),
            // `body` wraps header, steps and footnote in the side layout.
            req("body"),
            req("header"),
            req("title"),
            opt("badge"),
            req("steps"),
            req("step"),
            req("step.kind-icon"),
            req("totals"),
            req("warning"),
            // "Nothing changes until you confirm": never hidden (S5).
            req("footnote"),
            req("actions"),
            // Never hidden (S5).
            req("confirm"),
            req("cancel"),
        ],
        variants: &[
            variant("labels", &["verb", "generic"]),
            variant("layout", &["stacked", "side"]),
            variant("markers", &["number", "arrow", "none"]),
            variant("density", &["full", "compact"]),
        ],
        placements: &[Placement::Inline, Placement::Modal],
    },
    widget("confirm-destructive", Depth::A, &[], &[]),
    widget(
        "prompt",
        Depth::B,
        &[
            req("frame"),
            req("label"),
            req("field"),
            req("hint"),
            req("error"),
            req("ok"),
            req("cancel"),
        ],
        &[],
    ),
    widget("secret-input", Depth::A, &[], &[]),
    widget(
        "picker",
        Depth::B,
        &[],
        &[variant("layout", &["list", "grid", "keys"])],
    ),
    widget(
        "saves-conflicts",
        Depth::B,
        &[
            req("row"),
            req("side.local"),
            req("side.cloud"),
            req("choice"),
        ],
        &[],
    ),
    widget(
        "env-table",
        Depth::B,
        &[
            req("frame"),
            req("header"),
            req("title"),
            req("row"),
            req("name"),
            req("value"),
            req("source"),
            // The dots shown for a secret value (S2).
            req("masked"),
            req("reveal"),
            req("edit"),
        ],
        &[variant("row-actions", &["inline", "menu"])],
    ),
    widget(
        "log-view",
        Depth::B,
        &[
            req("frame"),
            req("line"),
            req("line.warn"),
            req("line.error"),
            req("timestamp"),
        ],
        &[],
    ),
    widget(
        "error",
        Depth::B,
        &[
            req("frame"),
            req("title"),
            req("message"),
            req("hint"),
            req("actions"),
        ],
        &[],
    ),
    // The doc gives the toast parts and a corner position, but names neither yet; they
    // are named here when the toast is built.
    widget("toast", Depth::B, &[], &[]),
    widget(
        "palette",
        Depth::B,
        &[
            req("frame"),
            req("field"),
            req("item"),
            req("item.key-hint"),
            req("group"),
        ],
        &[],
    ),
    // A skin may draw its own focus style; if it fails S13 the app's ring is drawn on top.
    widget("focus-ring", Depth::A, &[], &[]),
    stock("step-list"),
    stock("progress"),
    stock("chunk-bar"),
    stock("game-card"),
    stock("tabs"),
    stock("status-badge"),
    stock("meter"),
];

/// The widget called `name`.
pub fn widget_spec(name: &str) -> Option<&'static AppWidget> {
    WIDGETS.iter().find(|w| w.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn plan_card_keeps_its_safety_parts() {
        let card = widget_spec("plan-card").expect("plan-card");
        assert!(card.may_hide("badge"));
        assert!(!card.may_hide("footnote"));
        assert!(!card.may_hide("confirm"));
        assert!(!card.may_hide("no-such-part"));
        assert_eq!(card.variant("markers").map(|v| v.values[0]), Some("number"));
    }

    #[test]
    fn names_are_unique_and_only_depth_b_has_parts() {
        let mut seen = HashSet::new();
        for w in WIDGETS {
            assert!(seen.insert(w.name), "{} twice", w.name);
            if w.depth != Depth::B {
                assert!(
                    w.parts.is_empty() && w.variants.is_empty() && w.placements.is_empty(),
                    "{}",
                    w.name
                );
            }
            let mut parts = HashSet::new();
            for p in w.parts {
                assert!(parts.insert(p.name), "{}: part {} twice", w.name, p.name);
            }
            for v in w.variants {
                assert!(!v.values.is_empty(), "{}: {}", w.name, v.name);
            }
        }
    }
}
