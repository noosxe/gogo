//! Actions (design 26 §4): what a skin may ask the app to do, by name, from a bound user
//! input (S4).
//!
//! Each action has typed parameters, a usual **kind** and **tier**, and possibly an
//! [`Escalation`]: a rule that changes the kind or the tier once the plan is known. The
//! app plans first, then [`ActionSpec::decide`] applies the strongest tier the plan calls
//! for. Nothing in a skin can name a kind or a tier.
//!
//! No parameter type is a path, URL, shell command or free environment value (S3):
//! [`ParamType`] has no such variant. Folders and pages are opened by kind.

/// What the app does when the action fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Kind {
    /// Does it now.
    Immediate,
    /// Computes a plan, shows it, runs it only after confirmation.
    Plan,
    /// Collects input in the app's own dialog first.
    Prompt,
    /// Starts a job.
    Job,
    /// A plan, then a job once confirmed.
    PlanThenJob,
    /// One combined plan, then a job per item.
    PlanThenJobs,
    /// A prompt, then a plan.
    PromptThenPlan,
}

impl Kind {
    pub const fn doc(self) -> &'static str {
        match self {
            Self::Immediate => "immediate",
            Self::Plan => "plan",
            Self::Prompt => "prompt",
            Self::Job => "job",
            Self::PlanThenJob => "plan → job",
            Self::PlanThenJobs => "plan → jobs",
            Self::PromptThenPlan => "prompt → plan",
        }
    }

    /// Whether the kind shows a plan before anything changes.
    pub const fn plans(self) -> bool {
        matches!(
            self,
            Self::Plan | Self::PlanThenJob | Self::PlanThenJobs | Self::PromptThenPlan
        )
    }
}

/// How strongly the user must agree. Ordered weakest first; the app applies the strongest
/// one the plan calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Tier {
    /// Reversible or trivially undone.
    Safe,
    /// The plan must be confirmed.
    Confirm,
    /// Confirmed, and also needs a setting to be on (cloud writes).
    Gated,
    /// Strong confirmation on `confirm-destructive` (S6).
    Destructive,
}

impl Tier {
    pub const fn doc(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Confirm => "confirm",
            Self::Gated => "gated + confirm",
            Self::Destructive => "destructive",
        }
    }
}

/// A rule that changes an action's kind or tier once its plan is known (§4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Escalation {
    /// Destructive when the plan removes DLC or downgrades (`game.update`).
    DestructiveIfRemovesDlcOrDowngrades,
    /// Destructive when the plan crosses filesystems: snapshots are deleted (`game.move`).
    DestructiveAcrossFilesystems,
    /// Destructive with `purge_state` (`account.sign_out`).
    DestructiveWithPurgeState,
    /// Plan → job, confirm, when a download is needed (`runner.set_runtime`).
    PlanThenJobIfDownload,
    /// A plan when something must be downloaded first (`runner.set_galaxy`: Comet).
    PlanIfDownload,
    /// Immediate and safe when turning it off (`snapshot.set_auto`).
    ImmediateWhenOff,
}

/// What the computed plan contains, as far as kind and tier depend on it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlanFacts {
    /// The plan would change something. Empty plans (nothing to do) show nothing.
    pub non_empty: bool,
    pub removes_dlc: bool,
    pub downgrades: bool,
    pub needs_download: bool,
    pub crosses_filesystems: bool,
    /// The action's `purge_state` argument is set.
    pub purge_state: bool,
    /// The action turns something off (`on` is false).
    pub turning_off: bool,
}

/// The kind and tier the app applies to one invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub kind: Kind,
    pub tier: Tier,
    /// The plan is shown on the app's `plan-card`, and nothing changes until it is
    /// confirmed there (S5).
    pub shows_plan: bool,
}

/// Who may bind an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Binding {
    /// Any skin.
    Skin,
    /// Only an app-owned widget's own control (`env.reveal` in `env-table`).
    AppOnly,
}

/// The type of an action parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum ParamType {
    GameId,
    GameIds,
    JobId,
    /// A route name (§6.2).
    Route,
    /// The parameters of that route.
    RouteParams,
    /// A palette scope.
    PaletteScope,
    /// A variable the skin declared (§6.4).
    UiVar,
    /// A value for it: enum, bool, number or short literal.
    UiValue,
    /// A bound non-secret field of a source.
    BoundField,
    Bool,
    /// A number the app listed (an exe or a snapshot `n`).
    Index,
    Indices,
    /// DLC ids, or every DLC.
    DlcIdsOrAll,
    DlcIds,
    Bytes,
    /// One of a fixed set of words.
    OneOf(&'static [&'static str]),
    /// A download selection: `{os, lang, kinds, dlc}`.
    Selection,
    /// A runtime or renderer id from the catalogue.
    CatalogId,
    /// `unused`, or a catalogue id.
    UnusedOrCatalogId,
    Version,
    BuildId,
    Lang,
    /// A monitor the app listed.
    Monitor,
    /// The name of an existing `game.env(id)` row.
    EnvName,
    /// A key on the skin-settable list (S21).
    SettingKey,
    /// A value for it, validated like `config set`.
    SettingValue,
}

/// An action parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Param {
    pub name: &'static str,
    pub ty: ParamType,
    pub optional: bool,
    /// The doc writes it by its values alone (`id, on|off|default`).
    pub positional: bool,
}

/// An action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ActionSpec {
    pub name: &'static str,
    pub params: &'static [Param],
    /// The usual kind, before the plan is known.
    pub kind: Kind,
    /// The usual tier, before the plan is known.
    pub tier: Tier,
    pub escalation: Option<Escalation>,
    pub binding: Binding,
}

impl ActionSpec {
    pub fn param(&self, name: &str) -> Option<&Param> {
        self.params.iter().find(|p| p.name == name)
    }

    /// The kind and tier for one invocation, from what its plan contains: the usual ones,
    /// changed by the escalation, then raised so that a non-empty plan is always shown
    /// and confirmed (S5).
    pub fn decide(&self, facts: &PlanFacts) -> Decision {
        let (mut kind, mut tier) = (self.kind, self.tier);
        match self.escalation {
            Some(Escalation::DestructiveIfRemovesDlcOrDowngrades)
                if facts.removes_dlc || facts.downgrades =>
            {
                tier = Tier::Destructive;
            }
            Some(Escalation::DestructiveAcrossFilesystems) if facts.crosses_filesystems => {
                tier = Tier::Destructive;
            }
            Some(Escalation::DestructiveWithPurgeState) if facts.purge_state => {
                tier = Tier::Destructive
            }
            Some(Escalation::PlanThenJobIfDownload) if facts.needs_download => {
                (kind, tier) = (Kind::PlanThenJob, Tier::Confirm);
            }
            Some(Escalation::PlanIfDownload) if facts.needs_download => {
                (kind, tier) = (Kind::Plan, Tier::Confirm)
            }
            Some(Escalation::ImmediateWhenOff) if facts.turning_off => {
                (kind, tier) = (Kind::Immediate, Tier::Safe)
            }
            _ => {}
        }
        let shows_plan = facts.non_empty;
        if shows_plan {
            tier = tier.max(Tier::Confirm);
        }
        Decision {
            kind,
            tier,
            shows_plan,
        }
    }

    /// The doc's Kind / tier cell for this action, as §4's tables write it (without the
    /// `**` emphasis and backticks).
    pub fn doc_kind_tier(&self) -> String {
        if self.binding == Binding::AppOnly {
            return "app-only".to_owned();
        }
        let base = format!("{} / {}", self.kind.doc(), self.tier.doc());
        match self.escalation {
            None => base,
            Some(Escalation::DestructiveIfRemovesDlcOrDowngrades) => {
                format!("{base}; destructive if the plan removes DLC or downgrades")
            }
            Some(Escalation::DestructiveAcrossFilesystems) => {
                format!("{base}; destructive across filesystems (snapshots are deleted)")
            }
            Some(Escalation::DestructiveWithPurgeState) => {
                format!("{base}; destructive with purge_state")
            }
            Some(Escalation::PlanThenJobIfDownload) => {
                format!(
                    "{}, or plan → job if a download is needed / confirm",
                    self.kind.doc()
                )
            }
            Some(Escalation::PlanIfDownload) => {
                format!("{}; plan if Comet must be downloaded", self.kind.doc())
            }
            Some(Escalation::ImmediateWhenOff) => format!("{base} (on), immediate (off)"),
        }
    }
}

const fn p(name: &'static str, ty: ParamType) -> Param {
    Param {
        name,
        ty,
        optional: false,
        positional: false,
    }
}

const fn o(name: &'static str, ty: ParamType) -> Param {
    Param {
        name,
        ty,
        optional: true,
        positional: false,
    }
}

const fn positional(name: &'static str, ty: ParamType) -> Param {
    Param {
        name,
        ty,
        optional: false,
        positional: true,
    }
}

const fn act(name: &'static str, params: &'static [Param], kind: Kind, tier: Tier) -> ActionSpec {
    ActionSpec {
        name,
        params,
        kind,
        tier,
        escalation: None,
        binding: Binding::Skin,
    }
}

const fn esc(
    name: &'static str,
    params: &'static [Param],
    kind: Kind,
    tier: Tier,
    e: Escalation,
) -> ActionSpec {
    ActionSpec {
        name,
        params,
        kind,
        tier,
        escalation: Some(e),
        binding: Binding::Skin,
    }
}

use Kind::{Immediate, Job, Plan, PlanThenJob, PlanThenJobs, Prompt, PromptThenPlan};
use ParamType as T;
use Tier::{Confirm, Destructive, Gated, Safe};

const ID: &[Param] = &[p("id", T::GameId)];
const JOB: &[Param] = &[p("job", T::JobId)];
const PREFER: ParamType = T::OneOf(&["local", "cloud", "newer"]);

/// Every action in contract 1.0, in the doc's order.
pub const ACTIONS: &[ActionSpec] = &[
    // §4.1 Navigation and shell
    act(
        "nav.go",
        &[p("route", T::Route), p("params", T::RouteParams)],
        Immediate,
        Safe,
    ),
    act("nav.back", &[], Immediate, Safe),
    act(
        "palette.open",
        &[o("scope", T::PaletteScope)],
        Immediate,
        Safe,
    ),
    act("account.menu", &[], Immediate, Safe),
    act(
        "ui.set",
        &[p("var", T::UiVar), p("value", T::UiValue)],
        Immediate,
        Safe,
    ),
    act(
        "clipboard.copy",
        &[p("field", T::BoundField)],
        Immediate,
        Safe,
    ),
    // §4.2 Account
    act("account.sign_in", &[], Prompt, Safe),
    esc(
        "account.sign_out",
        &[p("purge_state", T::Bool)],
        Plan,
        Confirm,
        Escalation::DestructiveWithPurgeState,
    ),
    act("library.refresh", &[], Immediate, Safe),
    // §4.3 Play, files, verify
    act(
        "game.play",
        &[p("id", T::GameId), o("exe", T::Index)],
        Immediate,
        Safe,
    ),
    act(
        "game.download",
        &[p("id", T::GameId), p("selection", T::Selection)],
        PlanThenJob,
        Confirm,
    ),
    act("game.verify", ID, Job, Safe),
    act("game.verify_fix", ID, PlanThenJob, Confirm),
    act(
        "game.open_folder",
        &[
            p("id", T::GameId),
            p("which", T::OneOf(&["game", "prefix", "logs", "saves"])),
        ],
        Immediate,
        Safe,
    ),
    // §4.4 Builds and updates
    esc(
        "game.update",
        &[p("id", T::GameId), o("build", T::BuildId)],
        PlanThenJob,
        Confirm,
        Escalation::DestructiveIfRemovesDlcOrDowngrades,
    ),
    act(
        "updates.apply_all",
        &[o("ids", T::GameIds)],
        PlanThenJobs,
        Confirm,
    ),
    act("game.update_finish", ID, Plan, Confirm),
    act("game.update_undo", ID, Plan, Confirm),
    // §4.5 Set up for this machine
    act(
        "game.build",
        &[
            p("id", T::GameId),
            p("platform", T::OneOf(&["linux", "mac"])),
            o("runtime", T::CatalogId),
            o("renderer", T::CatalogId),
            o("dlc", T::DlcIdsOrAll),
            o("lang", T::Lang),
            o("build", T::BuildId),
        ],
        PlanThenJob,
        Confirm,
    ),
    act("game.setup", ID, Job, Safe),
    esc(
        "game.move",
        &[
            p("id", T::GameId),
            p("target", T::OneOf(&["default", "chosen"])),
        ],
        PlanThenJob,
        Confirm,
        Escalation::DestructiveAcrossFilesystems,
    ),
    act(
        "game.remove",
        &[p("id", T::GameId), p("keep_override", T::Bool)],
        Plan,
        Destructive,
    ),
    act(
        "apps.prune_orphaned",
        &[p("keep_override", T::Bool)],
        Plan,
        Destructive,
    ),
    act(
        "runner.set_renderer",
        &[p("id", T::GameId), p("renderer", T::CatalogId)],
        Immediate,
        Safe,
    ),
    act("runner.next_renderer", ID, Immediate, Safe),
    esc(
        "runner.set_runtime",
        &[p("id", T::GameId), p("runtime", T::CatalogId)],
        Immediate,
        Safe,
        Escalation::PlanThenJobIfDownload,
    ),
    act(
        "runner.set_display",
        &[
            p("id", T::GameId),
            o("mode", T::OneOf(&["wayland", "x11"])),
            o("hdr", T::Bool),
            o("monitor", T::Monitor),
        ],
        Immediate,
        Safe,
    ),
    esc(
        "runner.set_galaxy",
        &[
            p("id", T::GameId),
            positional("setting", T::OneOf(&["on", "off", "default"])),
        ],
        Immediate,
        Safe,
        Escalation::PlanIfDownload,
    ),
    act("runner.edit_args", ID, Prompt, Safe),
    act("runner.edit_wrapper", ID, Prompt, Safe),
    act(
        "env.edit",
        &[p("id", T::GameId), o("name", T::EnvName)],
        Prompt,
        Safe,
    ),
    act(
        "env.unset",
        &[p("id", T::GameId), p("name", T::EnvName)],
        Plan,
        Confirm,
    ),
    ActionSpec {
        name: "env.reveal",
        params: &[p("id", T::GameId), p("name", T::EnvName)],
        kind: Immediate,
        tier: Safe,
        escalation: None,
        binding: Binding::AppOnly,
    },
    act(
        "exe.run",
        &[p("id", T::GameId), p("n", T::Index)],
        Immediate,
        Safe,
    ),
    // §4.6 DLC
    act(
        "dlc.add",
        &[p("id", T::GameId), p("dlc", T::DlcIdsOrAll)],
        PlanThenJob,
        Confirm,
    ),
    act(
        "dlc.remove",
        &[p("id", T::GameId), p("dlc", T::DlcIds)],
        PlanThenJob,
        Destructive,
    ),
    act("dlc.abandon", ID, Plan, Confirm),
    // §4.7 Snapshots
    act("snapshot.create", ID, PromptThenPlan, Confirm),
    act(
        "snapshot.delete",
        &[p("id", T::GameId), p("n", T::Indices)],
        Plan,
        Destructive,
    ),
    act(
        "snapshot.rollback",
        &[
            p("id", T::GameId),
            p("n", T::Index),
            p("drop_saves", T::Bool),
        ],
        Plan,
        Destructive,
    ),
    esc(
        "snapshot.set_auto",
        &[p("id", T::GameId), p("on", T::Bool)],
        Plan,
        Confirm,
        Escalation::ImmediateWhenOff,
    ),
    // §4.8 Cloud saves
    act("saves.check", ID, Immediate, Safe),
    act(
        "saves.pull",
        &[p("id", T::GameId), o("prefer", PREFER)],
        PlanThenJob,
        Confirm,
    ),
    act(
        "saves.push",
        &[
            p("id", T::GameId),
            o("prefer", PREFER),
            o("no_delete", T::Bool),
        ],
        PlanThenJob,
        Gated,
    ),
    act(
        "saves.sync",
        &[
            p("id", T::GameId),
            o("prefer", PREFER),
            o("no_delete", T::Bool),
        ],
        PlanThenJob,
        Gated,
    ),
    // §4.9 Queue and machine
    act("job.pause", JOB, Immediate, Safe),
    act("job.resume", JOB, Immediate, Safe),
    act("job.retry", JOB, Immediate, Safe),
    act("job.cancel", JOB, Immediate, Safe),
    act(
        "queue.set_rate_limit",
        &[o("rate", T::Bytes)],
        Immediate,
        Safe,
    ),
    act(
        "runtimes.install",
        &[p("id", T::CatalogId), o("version", T::Version)],
        PlanThenJob,
        Confirm,
    ),
    act(
        "renderers.install",
        &[p("id", T::CatalogId), o("version", T::Version)],
        PlanThenJob,
        Confirm,
    ),
    act(
        "runtimes.prune",
        &[positional("which", T::UnusedOrCatalogId)],
        Plan,
        Confirm,
    ),
    act(
        "renderers.prune",
        &[positional("which", T::UnusedOrCatalogId)],
        Plan,
        Confirm,
    ),
    act("doctor.run", &[o("id", T::GameId)], Immediate, Safe),
    act("doctor.stop_leftovers", ID, Plan, Confirm),
    act(
        "settings.set",
        &[p("key", T::SettingKey), p("value", T::SettingValue)],
        Immediate,
        Safe,
    ),
    act("settings.open_file", &[], Immediate, Safe),
    act(
        "app.open_url",
        &[
            p(
                "kind",
                T::OneOf(&["store_page", "support", "login", "docs"]),
            ),
            o("id", T::GameId),
        ],
        Immediate,
        Safe,
    ),
];

/// The action called `name`, bindable or not.
pub fn action(name: &str) -> Option<&'static ActionSpec> {
    ACTIONS.iter().find(|a| a.name == name)
}

/// The action called `name`, if a skin may bind it.
pub fn skin_action(name: &str) -> Option<&'static ActionSpec> {
    action(name).filter(|a| a.binding == Binding::Skin)
}

/// The input variables bound when a control's action fires: the option a `select` was
/// set to (`value`) and a `toggle`'s state before the input (`on`). Only those two
/// elements may use them in `args`.
pub const INPUT_VARS: &[&str] = &["value", "on"];

/// The elements whose `args` may use [`INPUT_VARS`].
pub const INPUT_ELEMENTS: &[&str] = &["toggle", "select"];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn get(name: &str) -> &'static ActionSpec {
        action(name).unwrap_or_else(|| panic!("no action {name}"))
    }

    #[test]
    fn names_are_unique() {
        let mut seen = HashSet::new();
        for a in ACTIONS {
            assert!(seen.insert(a.name), "{} twice", a.name);
        }
    }

    #[test]
    fn env_reveal_is_not_bindable() {
        assert!(skin_action("env.reveal").is_none());
        assert!(skin_action("env.edit").is_some());
        assert_eq!(
            ACTIONS
                .iter()
                .filter(|a| a.binding == Binding::AppOnly)
                .count(),
            1
        );
    }

    #[test]
    fn update_is_destructive_when_it_removes_dlc_or_downgrades() {
        let a = get("game.update");
        let plain = PlanFacts {
            non_empty: true,
            ..Default::default()
        };
        assert_eq!(a.decide(&plain).tier, Tier::Confirm);
        for facts in [
            PlanFacts {
                removes_dlc: true,
                ..plain
            },
            PlanFacts {
                downgrades: true,
                ..plain
            },
        ] {
            assert_eq!(a.decide(&facts).tier, Tier::Destructive);
        }
    }

    #[test]
    fn move_is_destructive_across_filesystems() {
        let facts = PlanFacts {
            non_empty: true,
            crosses_filesystems: true,
            ..Default::default()
        };
        assert_eq!(get("game.move").decide(&facts).tier, Tier::Destructive);
    }

    #[test]
    fn sign_out_is_destructive_with_purge_state() {
        let a = get("account.sign_out");
        let plain = PlanFacts {
            non_empty: true,
            ..Default::default()
        };
        assert_eq!(a.decide(&plain).tier, Tier::Confirm);
        assert_eq!(
            a.decide(&PlanFacts {
                purge_state: true,
                ..plain
            })
            .tier,
            Tier::Destructive
        );
    }

    #[test]
    fn runtime_and_galaxy_plan_when_a_download_is_needed() {
        let none = PlanFacts::default();
        let dl = PlanFacts {
            non_empty: true,
            needs_download: true,
            ..Default::default()
        };
        let rt = get("runner.set_runtime");
        assert_eq!(
            rt.decide(&none),
            Decision {
                kind: Kind::Immediate,
                tier: Tier::Safe,
                shows_plan: false
            }
        );
        assert_eq!(
            rt.decide(&dl),
            Decision {
                kind: Kind::PlanThenJob,
                tier: Tier::Confirm,
                shows_plan: true
            }
        );
        let gx = get("runner.set_galaxy");
        assert_eq!(
            gx.decide(&dl),
            Decision {
                kind: Kind::Plan,
                tier: Tier::Confirm,
                shows_plan: true
            }
        );
    }

    #[test]
    fn auto_snapshots_off_is_immediate() {
        let a = get("snapshot.set_auto");
        let off = PlanFacts {
            turning_off: true,
            ..Default::default()
        };
        assert_eq!(
            a.decide(&off),
            Decision {
                kind: Kind::Immediate,
                tier: Tier::Safe,
                shows_plan: false
            }
        );
        let on = PlanFacts {
            non_empty: true,
            ..Default::default()
        };
        assert_eq!(a.decide(&on).tier, Tier::Confirm);
    }

    #[test]
    fn a_non_empty_plan_is_always_shown_and_confirmed() {
        // S5: whatever the usual kind and tier.
        let facts = PlanFacts {
            non_empty: true,
            ..Default::default()
        };
        for a in ACTIONS {
            let d = a.decide(&facts);
            assert!(d.shows_plan, "{}", a.name);
            assert!(d.tier >= Tier::Confirm, "{}", a.name);
            assert!(
                d.tier >= a.tier,
                "{}: the plan never weakens the tier",
                a.name
            );
        }
    }

    #[test]
    fn gated_actions_stay_gated() {
        // No escalation may turn a gated action into one that skips its setting.
        for a in ACTIONS.iter().filter(|a| a.tier == Tier::Gated) {
            assert_eq!(a.escalation, None, "{}", a.name);
            let facts = PlanFacts {
                non_empty: true,
                ..Default::default()
            };
            assert_eq!(a.decide(&facts).tier, Tier::Gated, "{}", a.name);
        }
    }

    #[test]
    fn destructive_tier_needs_a_plan() {
        // A destructive action always shows what it deletes first.
        for a in ACTIONS {
            let worst = PlanFacts {
                non_empty: true,
                removes_dlc: true,
                downgrades: true,
                crosses_filesystems: true,
                purge_state: true,
                ..Default::default()
            };
            let d = a.decide(&worst);
            if d.tier == Tier::Destructive {
                assert!(d.kind.plans(), "{}", a.name);
            }
        }
    }

    #[test]
    fn settings_set_takes_only_a_setting_key() {
        assert_eq!(
            get("settings.set").param("key").map(|p| p.ty),
            Some(ParamType::SettingKey)
        );
    }
}
