//! The data sources (design 26 §3): typed, masked, read-only views of engine state.
//!
//! This table names each source, its parameters, how it updates and the CLI command it
//! stands for. The fields of each shape arrive with the validator that checks them (PR B3,
//! S16).

/// How a source updates (design 26 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Live {
    /// When the engine reports a change: a job event, a setting written, a refresh done.
    Event,
    /// When the skin writes its own state (`ui.set`).
    OnWrite,
    /// Computed when a route asks for it and when its refresh action runs; carries
    /// `checked_at`.
    OnDemand,
    /// On demand, debounced while the input changes.
    OnDemandDebounced,
    /// On demand, and on events as well.
    OnDemandAndEvent,
    /// On demand, and recomputed on events that touch the game.
    OnDemandRecomputed,
    /// A job source: at most 10 updates a second, newest value only (design 25 §3).
    Live,
}

impl Live {
    /// The words the doc's Live column uses.
    pub const fn doc(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::OnWrite => "on write",
            Self::OnDemand => "on demand",
            Self::OnDemandDebounced => "on demand, debounced",
            Self::OnDemandAndEvent => "on demand + event",
            Self::OnDemandRecomputed => "on demand; recomputed on events that touch the game",
            Self::Live => "live",
        }
    }
}

/// A source parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SourceParam {
    pub name: &'static str,
    pub optional: bool,
}

/// A data source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SourceSpec {
    /// `library`, `game.builds`, …
    pub name: &'static str,
    pub params: &'static [SourceParam],
    pub live: Live,
    /// The CLI commands (or engine events) it stands for; empty for app-only state.
    pub from: &'static [&'static str],
    /// Set when the data reaches a skin only through this app-owned widget, never as
    /// bindable data (`job.log` → `log-view`).
    pub widget_only: Option<&'static str>,
}

impl SourceSpec {
    pub fn param(&self, name: &str) -> Option<&SourceParam> {
        self.params.iter().find(|p| p.name == name)
    }
}

const fn req(name: &'static str) -> SourceParam {
    SourceParam {
        name,
        optional: false,
    }
}

const fn opt(name: &'static str) -> SourceParam {
    SourceParam {
        name,
        optional: true,
    }
}

const ID: &[SourceParam] = &[req("id")];
const JOB_EVENTS: &[&str] = &["ProgressEvent", "Work*"];

const fn src(
    name: &'static str,
    params: &'static [SourceParam],
    live: Live,
    from: &'static [&'static str],
) -> SourceSpec {
    SourceSpec {
        name,
        params,
        live,
        from,
        widget_only: None,
    }
}

/// Every source in contract 1.0, in the doc's order.
pub const SOURCES: &[SourceSpec] = &[
    // §3.1 Account and app
    src("account", &[], Live::Event, &["whoami"]),
    src("app", &[], Live::Event, &[]),
    src("ui", &[], Live::OnWrite, &[]),
    // §3.2 Library and search
    src(
        "library",
        &[opt("filter"), opt("sort")],
        Live::Event,
        &["list"],
    ),
    src("library.stats", &[], Live::Event, &["list"]),
    src(
        "search",
        &[req("query"), opt("categories"), opt("os"), opt("owned")],
        Live::OnDemandDebounced,
        &["search"],
    ),
    // §3.3 One game
    src("game", ID, Live::Event, &["info", "apps show"]),
    src(
        "game.builds",
        &[req("id"), opt("os")],
        Live::OnDemand,
        &["builds"],
    ),
    src(
        "game.files",
        ID,
        Live::Event,
        &["download --dry-run", "verify"],
    ),
    src("game.dlc", ID, Live::Event, &["linux-app dlc list"]),
    src(
        "game.saves",
        ID,
        Live::OnDemandAndEvent,
        &["saves status", "saves locations"],
    ),
    src("game.snapshots", ID, Live::Event, &["snapshot list"]),
    src(
        "game.runner",
        ID,
        Live::Event,
        &["compat", "galaxy", "display", "args", "wrapper"],
    ),
    src("game.env", ID, Live::Event, &["env list"]),
    src("game.exes", ID, Live::OnDemand, &["exes"]),
    src(
        "game.doctor",
        ID,
        Live::OnDemand,
        &["linux-app doctor", "mac-app doctor"],
    ),
    src("game.compat", ID, Live::OnDemand, &["compat"]),
    src("game.recipes", ID, Live::OnDemand, &["recipes list/show"]),
    // §3.4 Jobs
    src("queue", &[], Live::Live, JOB_EVENTS),
    src("queue.summary", &[], Live::Live, JOB_EVENTS),
    src("job", ID, Live::Live, JOB_EVENTS),
    SourceSpec {
        name: "job.log",
        params: ID,
        live: Live::Live,
        from: &[],
        widget_only: Some("log-view"),
    },
    // §3.5 Machine and settings
    src(
        "updates",
        &[],
        Live::OnDemandAndEvent,
        &["update", "builds --installed"],
    ),
    src("apps", &[], Live::Event, &["apps list"]),
    src("runtimes", &[], Live::OnDemand, &["runtimes list"]),
    src("renderers", &[], Live::OnDemand, &["renderers list"]),
    src("doctor.all", &[], Live::OnDemand, &["doctor"]),
    src("settings", &[], Live::Event, &["config get"]),
    // §3.6 Plan previews
    src(
        "plan.preview",
        &[req("action"), req("args")],
        Live::OnDemandRecomputed,
        &["--dry-run --json"],
    ),
];

/// The source called `name`.
pub fn source(name: &str) -> Option<&'static SourceSpec> {
    SOURCES.iter().find(|s| s.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn names_are_unique() {
        let mut seen = HashSet::new();
        for s in SOURCES {
            assert!(seen.insert(s.name), "{} twice", s.name);
        }
    }

    #[test]
    fn job_log_reaches_skins_only_through_log_view() {
        assert_eq!(
            source("job.log").and_then(|s| s.widget_only),
            Some("log-view")
        );
        assert!(SOURCES.iter().filter(|s| s.widget_only.is_some()).count() == 1);
    }

    #[test]
    fn lookup() {
        let s = source("game.builds").expect("game.builds");
        assert_eq!(s.param("os").map(|p| p.optional), Some(true));
        assert!(source("game.credentials").is_none());
    }
}
