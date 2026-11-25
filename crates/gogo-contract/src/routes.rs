//! Routes and their sections (design 26 §6.2). The set is fixed: v1 skins can't add
//! routes, only merge, split and reorder the sections inside one.

use crate::sources::SourceParam;

/// A route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct RouteSpec {
    pub name: &'static str,
    pub params: &'static [SourceParam],
    /// The sections a skin shows as tabs, one long page, or merged.
    pub sections: &'static [&'static str],
}

const fn route(
    name: &'static str,
    params: &'static [SourceParam],
    sections: &'static [&'static str],
) -> RouteSpec {
    RouteSpec {
        name,
        params,
        sections,
    }
}

const fn opt(name: &'static str) -> SourceParam {
    SourceParam {
        name,
        optional: true,
    }
}

/// Every route in contract 1.0, in the doc's order.
pub const ROUTES: &[RouteSpec] = &[
    route("shell", &[], &[]),
    route("sign-in", &[], &[]),
    route("library", &[opt("filter"), opt("view")], &[]),
    route(
        "game",
        &[
            SourceParam {
                name: "id",
                optional: false,
            },
            opt("section"),
        ],
        &[
            "overview",
            "builds",
            "dlc",
            "saves",
            "snapshots",
            "setup",
            "files",
        ],
    ),
    route("queue", &[], &[]),
    route("updates", &[], &[]),
    route("saves", &[], &[]),
    route("apps", &[], &[]),
    route("runtimes", &[], &[]),
    route("doctor", &[opt("id")], &[]),
    route("settings", &[opt("group")], &[]),
    route("about", &[], &[]),
];

/// The route called `name`.
pub fn route_spec(name: &str) -> Option<&'static RouteSpec> {
    ROUTES.iter().find(|r| r.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_route_has_its_sections() {
        let g = route_spec("game").expect("game");
        assert!(g.sections.contains(&"snapshots"));
        assert!(route_spec("plan").is_none(), "dialogs are not routes");
    }
}
