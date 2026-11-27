//! What a set-up would do: the questions first, then the plan card. Nothing is written
//! or stored here.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

use gogo::config::UmuMode;
use gogo::engine::FreeSpace;
use gogo::linuxapp::preview::{Answers, Download, Item, Preview, SetupPreview, Tools};
use gogo::macapp::MacAppError;
use gogo::macapp::prompt::{PromptAnswer as CoreAnswer, PromptKind, PromptSpec as CoreSpec};
use gogo_contract::PlanFacts;
use gogo_gui_model::{ErrorView, Plan, PlanStep, PromptAnswer, PromptSpec, StepKind};

use super::super::errors;
use super::pipeline::Pipelines;
use super::steps::Phase;

/// A set-up the user can agree to.
#[derive(Debug, Clone)]
pub(super) struct Computed {
    pub gogid: String,
    pub title: String,
    pub answers: Answers,
    pub plan: Plan,
    pub inputs: u64,
    pub downloads: Vec<Download>,
    pub tools: Tools,
    pub phases: Vec<Phase>,
}

impl Computed {
    pub(super) fn facts(&self) -> PlanFacts {
        PlanFacts::changes()
    }

    /// Whether this does nothing `agreed` didn't.
    pub(super) fn within(&self, agreed: &Computed) -> bool {
        self.tools.umu == agreed.tools.umu
            && self.tools.runtime == agreed.tools.runtime
            && self.tools.galaxy == agreed.tools.galaxy
            && self.answers == agreed.answers
            && self.downloads.iter().all(|d| agreed.downloads.contains(d))
    }
}

pub(super) enum Planning {
    Ask(PromptSpec),
    Ready(Box<Computed>),
}

/// A question of the core's, as the app asks it.
fn question(spec: &CoreSpec) -> PromptSpec {
    let label = errors::path(&spec.title);
    match spec.kind {
        PromptKind::Confirm { .. } => PromptSpec::Confirm { label },
        _ => PromptSpec::Choice {
            label,
            options: spec
                .items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let default = if i == spec.default { " (default)" } else { "" };
                    errors::path(&format!("{item}{default}"))
                })
                .collect(),
        },
    }
}

fn core_answer(answer: &PromptAnswer) -> Option<CoreAnswer> {
    match answer {
        PromptAnswer::Choice(i) => Some(CoreAnswer::Index(*i)),
        PromptAnswer::Yes => Some(CoreAnswer::Confirmed(true)),
        PromptAnswer::No => Some(CoreAnswer::Confirmed(false)),
        _ => None,
    }
}

/// A failure while planning, in the app's own phrases: the core's text holds paths
/// and parser output, and goes to the log only.
fn refused(error: &MacAppError) -> ErrorView {
    tracing::warn!("planning failed: {}", errors::path(&error.to_string()));
    let (class, message, hint) = match error {
        MacAppError::CatalogUnreachable { .. } | MacAppError::RuntimeDownloadFailed { .. } => (
            "Network",
            "The size of a download could not be read from GitHub.",
            "Check the connection, then try again.",
        ),
        MacAppError::Apps(_) => ("Error", "The game is running.", "Quit it, then try again."),
        MacAppError::Compat { .. } => (
            "Error",
            "A settings file for this game could not be read.",
            "`gogo linux-app doctor` says which.",
        ),
        MacAppError::Library { .. } => (
            "Error",
            "The game's folder or the library can't be used.",
            "Check that its drive is there.",
        ),
        MacAppError::Umu(_) => (
            "Error",
            "umu can't be run as it is set up.",
            "`gogo linux-app doctor` says why.",
        ),
        MacAppError::Proton(_) => (
            "Error",
            "The game's GE-Proton build can't be used.",
            "`gogo linux-app doctor` says why.",
        ),
        MacAppError::Comet(_) => ("Error", "Comet can't be used as it is set up.", ""),
        MacAppError::Io(_) => ("Error", "A file could not be read.", ""),
        _ => (
            "Error",
            "What the run would do could not be worked out.",
            "",
        ),
    };
    ErrorView::new(class, "Set-up can't be planned", message, hint, "7")
}

fn cannot_answer() -> ErrorView {
    ErrorView::failed("Set-up can't be planned", "an answer fits no question")
}

/// Preview until there is no question left, taking `given` in the order asked.
/// `answers` holds the ones already known.
fn answered<T>(
    mut answers: Answers,
    given: &[PromptAnswer],
    preview: impl Fn(&Answers) -> Result<Preview<T>, MacAppError>,
) -> Result<Result<(Answers, T), PromptSpec>, ErrorView> {
    let mut given = given.iter();
    loop {
        match preview(&answers).map_err(|e| refused(&e))? {
            Preview::Ready(ready) => return Ok(Ok((answers, ready))),
            Preview::Ask(q) => {
                let Some(answer) = given.next() else {
                    return Ok(Err(question(&q.spec)));
                };
                let taken = core_answer(answer).is_some_and(|a| answers.answer(&q, &a));
                if !taken {
                    return Err(cannot_answer());
                }
            }
        }
    }
}

fn item_name(item: Item) -> &'static str {
    match item {
        Item::Umu => "umu-launcher",
        Item::Proton => "GE-Proton",
        Item::Comet => "Comet",
    }
}

fn step(kind: StepKind, text: impl Into<String>, bytes: Option<u64>) -> PlanStep {
    PlanStep {
        kind,
        text: text.into(),
        bytes,
    }
}

/// The lines about the tools, shared with a build's card.
fn tool_steps(tools: &Tools, answers: &Answers, steps: &mut Vec<PlanStep>) {
    for d in &tools.downloads {
        let into = d.into.as_ref().map_or_else(
            || "gogo's store".to_string(),
            |p| errors::path(&p.display().to_string()),
        );
        steps.push(step(
            StepKind::Download,
            format!("{} {} into {into}", item_name(d.item), d.version),
            Some(d.bytes),
        ));
    }
    let umu = match tools.umu {
        UmuMode::Zipapp => "gogo's pinned umu-launcher",
        UmuMode::System => "the system's umu-run",
    };
    steps.push(step(
        StepKind::Info,
        format!("Runs with {} and {umu}", errors::path(&tools.runtime)),
        None,
    ));
    if tools.galaxy {
        steps.push(step(StepKind::Info, "Comet runs beside the game", None));
    }
    let mut saved = Vec::new();
    if let Some(mode) = answers.umu {
        saved.push(format!("umu = {}", mode.as_str()));
    }
    if let Some(dir) = answers.proton_dir {
        saved.push(format!("GE-Proton goes to {}", dir.as_str()));
    }
    if let Some(mode) = answers.galaxy {
        saved.push(format!("Comet {} for this game", mode.key()));
    }
    if !saved.is_empty() {
        steps.push(step(
            StepKind::Info,
            format!("Saves your choices: {}", saved.join(", ")),
            None,
        ));
    }
}

fn free_at(space: &dyn FreeSpace, dir: &Path) -> Option<u64> {
    let there = dir.ancestors().find(|p| p.exists())?;
    space.available_bytes(there).ok()
}

fn digest(parts: &impl std::fmt::Debug) -> u64 {
    let mut hasher = DefaultHasher::new();
    format!("{parts:?}").hash(&mut hasher);
    hasher.finish()
}

fn card(preview: &SetupPreview, answers: &Answers, snapshots: bool) -> (Plan, Vec<Phase>) {
    let tools = &preview.tools;
    let mut steps = Vec::new();
    let mut phases = Vec::new();
    for d in &tools.downloads {
        phases.push(match d.item {
            Item::Umu => Phase::Umu,
            Item::Proton => Phase::Proton,
            Item::Comet => continue,
        });
    }
    // The order setup works in.
    phases.sort_by_key(|p| *p != Phase::Proton);
    if let Some((from, to)) = &preview.runtime_change
        && snapshots
    {
        steps.push(step(
            StepKind::Snapshot,
            format!(
                "Snapshot first, where the game's folder allows it: {} to {}",
                errors::path(from),
                errors::path(to)
            ),
            None,
        ));
        phases.push(Phase::Snapshot);
    }
    tool_steps(tools, answers, &mut steps);
    if preview.prefix_exists {
        steps.push(step(
            StepKind::Info,
            "Keeps the game's Wine prefix, with its saves",
            None,
        ));
    } else {
        steps.push(step(StepKind::Info, "Creates the Wine prefix", None));
        steps.push(step(
            StepKind::Info,
            "umu downloads the Steam Linux Runtime on its first run, if it isn't there",
            None,
        ));
        phases.push(Phase::Prefix);
    }
    steps.push(step(
        StepKind::Info,
        "Applies the compatibility fixes, installs missing winetricks verbs and rewrites \
         the launcher and menu entry",
        None,
    ));
    phases.push(Phase::Finish);
    if tools.install_comet {
        phases.push(Phase::Comet);
    }
    let download: u64 = tools.downloads.iter().map(|d| d.bytes).sum();
    let title = errors::path(&preview.title);
    let plan = Plan {
        verb: "Run setup".to_string(),
        title: format!("Run setup for {title}?"),
        subject: title,
        steps,
        download: (download > 0).then_some(download),
        disk_delta: None,
        free: None,
        warnings: vec![
            "Setup can't be stopped once it starts, and the window stays open until it ends."
                .to_string(),
        ],
        reversible: false,
    };
    (plan, phases)
}

/// What setting up `gogid` would do, once `given` answers every question that isn't in
/// `known` already.
pub(super) fn setup(
    pipelines: &dyn Pipelines,
    space: &dyn FreeSpace,
    snapshots: bool,
    gogid: &str,
    known: Answers,
    given: &[PromptAnswer],
) -> Result<Planning, ErrorView> {
    let previewed = answered(known, given, |a| pipelines.preview_setup(gogid, a))?;
    let (answers, preview) = match previewed {
        Ok(ready) => ready,
        Err(question) => return Ok(Planning::Ask(question)),
    };
    let (mut plan, phases) = card(&preview, &answers, snapshots);
    let inputs = digest(&(&plan, &preview.tools, &preview.runtime_change, &answers));
    let root: PathBuf = pipelines.data_root();
    plan.free = free_at(space, &root);
    if let (Some(need), Some(free)) = (plan.download, plan.free)
        && need > free
    {
        return Err(ErrorView::new(
            "Disk",
            "Not enough free space",
            "The downloads are larger than the space left.",
            "Free some space, then try again.",
            "6",
        ));
    }
    Ok(Planning::Ready(Box::new(Computed {
        gogid: gogid.to_string(),
        title: preview.title,
        answers,
        plan,
        inputs,
        downloads: preview.tools.downloads.clone(),
        tools: preview.tools,
        phases,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No path and none of the core's own words.
    fn plain(view: &ErrorView, raw: &[&str]) {
        let shown = format!("{} {} {}", view.title, view.message, view.hint);
        assert!(!shown.contains('/'), "{shown}");
        for raw in raw {
            assert!(!shown.contains(raw), "{shown}");
        }
    }

    #[test]
    fn a_planning_error_is_in_the_apps_own_words() {
        let path = || PathBuf::from("/somewhere/compat/10.toml");
        let said = "expected `=` at line 3";
        for error in [
            MacAppError::Compat {
                path: path(),
                reason: said.into(),
            },
            MacAppError::Library {
                path: path(),
                reason: said.into(),
            },
            MacAppError::Apps(said.into()),
            MacAppError::Umu(said.into()),
            MacAppError::Proton(said.into()),
            MacAppError::Comet(said.into()),
            MacAppError::CatalogUnreachable {
                detail: said.into(),
            },
            MacAppError::Io(std::io::Error::other(said)),
            MacAppError::Recipe(said.into()),
        ] {
            plain(&refused(&error), &["somewhere", said]);
        }
    }
}
