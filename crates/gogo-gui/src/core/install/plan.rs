//! What a build or a set-up would do: the questions first, then the plan card. Nothing
//! is written or stored here.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gogo::auth::TokenProvider;
use gogo::config::UmuMode;
use gogo::engine::FreeSpace;
use gogo::gog::depot::DepotError;
use gogo::linuxapp::preview::{
    Answers, BuildPreview, BuildRequest, Download, Item, Preview, SetupPreview, Tools,
};
use gogo::linuxapp::source::{self, Files, Source};
use gogo::macapp::MacAppError;
use gogo::macapp::prompt::{PromptAnswer as CoreAnswer, PromptKind, PromptSpec as CoreSpec};
use gogo_contract::PlanFacts;
use gogo_gui_model::{ErrorView, Plan, PlanStep, PromptAnswer, PromptSpec, StepKind};

use super::super::errors;
use super::pipeline::{Build, Pipelines};
use super::steps::Phase;

/// A build or set-up the user can agree to.
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
    /// A build, with what it was asked for; `None` for a set-up.
    pub build: Option<(Asked, Build)>,
    /// A build replaces a game that is there.
    pub rebuild: bool,
    /// The game's folder.
    pub dir: PathBuf,
}

/// The arguments of a `game.build`.
#[derive(Debug, Clone)]
pub(super) struct Asked {
    pub product: u64,
    pub request: source::Request,
    pub runtime: Option<String>,
    pub renderer: gogo::macapp::RendererChoice,
    /// `library.dir`.
    pub library: Option<PathBuf>,
}

/// Whether `now` builds nothing `agreed` didn't show.
fn same_build(now: &Build, agreed: &Build) -> bool {
    let (a, b) = (&now.source, &agreed.source);
    // Files that arrived since need no download any more.
    let files = a.files == b.files
        || (matches!(a.files, Files::Cached(_)) && matches!(b.files, Files::Fetch { .. }));
    files
        && a.build == b.build
        && a.options.language == b.options.language
        && a.dlcs == b.dlcs
        && now.gogo == agreed.gogo
}

impl Computed {
    pub(super) fn facts(&self) -> PlanFacts {
        PlanFacts::changes()
    }

    /// Whether this does nothing `agreed` didn't.
    pub(super) fn within(&self, agreed: &Computed) -> bool {
        let build = match (&self.build, &agreed.build) {
            (Some((_, now)), Some((_, agreed))) => same_build(now, agreed),
            (None, None) => true,
            _ => false,
        };
        build
            && self.rebuild == agreed.rebuild
            && self.dir == agreed.dir
            && self.tools.umu == agreed.tools.umu
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
fn refused(title: &str, error: &MacAppError) -> ErrorView {
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
    ErrorView::new(class, title, message, hint, "7")
}

/// The titles of plans that can't be made.
const NO_SETUP: &str = "Set-up can't be planned";
const NO_INSTALL: &str = "Install can't be planned";

fn cannot_answer(title: &str) -> ErrorView {
    ErrorView::failed(title, "an answer fits no question")
}

/// Preview until there is no question left, taking `given` in the order asked.
/// `answers` holds the ones already known.
fn answered<T>(
    title: &str,
    mut answers: Answers,
    given: &[PromptAnswer],
    preview: impl Fn(&Answers) -> Result<Preview<T>, MacAppError>,
) -> Result<Result<(Answers, T), PromptSpec>, ErrorView> {
    let mut given = given.iter();
    loop {
        match preview(&answers).map_err(|e| refused(title, &e))? {
            Preview::Ready(ready) => return Ok(Ok((answers, ready))),
            Preview::Ask(q) => {
                let Some(answer) = given.next() else {
                    return Ok(Err(question(&q.spec)));
                };
                let taken = core_answer(answer).is_some_and(|a| answers.answer(&q, &a));
                if !taken {
                    return Err(cannot_answer(title));
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
    steps.extend(saved(answers));
}

/// The line about the choices the run will store.
fn saved(answers: &Answers) -> Option<PlanStep> {
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
    (!saved.is_empty()).then(|| {
        let text = format!("Saves your choices: {}", saved.join(", "));
        step(StepKind::Info, text, None)
    })
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
    let previewed = answered(NO_SETUP, known, given, |a| {
        pipelines.preview_setup(gogid, a)
    })?;
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
        return Err(too_little());
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
        build: None,
        rebuild: false,
        dir: preview.dirs.root,
    })))
}

/// Why there is no source for a build, in the app's own phrases.
fn no_build(error: &DepotError) -> ErrorView {
    tracing::warn!("planning failed: {}", errors::path(&error.to_string()));
    let (class, message) = match error {
        DepotError::NoBuilds(_) => (
            "Error",
            "It has no depot build and no downloaded installer. Download its installer \
             first.",
        ),
        DepotError::Dlc(_) => ("Error", "The DLC asked for don't fit the game's build."),
        DepotError::Language(_) => ("Error", "The build doesn't have that language."),
        DepotError::Pick(_) => ("Error", "No one build matches the one asked for."),
        DepotError::NoCacheDir => ("Error", "There is no cache folder for the game files."),
        DepotError::Library(_) => ("Network", "GOG's builds could not be read."),
        DepotError::Filter(_) => (
            "Error",
            "The downloaded files hold more than the build takes, and can't be told \
             apart.",
        ),
        DepotError::Io(_) => ("Error", "A file could not be read."),
    };
    ErrorView::new(class, NO_INSTALL, message, "", "1")
}

/// The renderer, runtime and DLC lines of a build's card.
fn build_card(asked: &Asked, preview: &BuildPreview, build: &Build) -> (Plan, Vec<Phase>) {
    let tools = &preview.tools;
    let source = &build.source;
    let title = errors::path(&asked.request.title);
    let verb = if preview.rebuild {
        "Rebuild"
    } else {
        "Install"
    };
    let mut steps = Vec::new();
    let mut phases = Vec::new();
    let version = source
        .build
        .as_ref()
        .map(|(id, version)| version.clone().unwrap_or_else(|| id.clone()))
        .unwrap_or_default();
    let language = &source.options.language;
    let fetch = match &source.files {
        Files::Fetch { repair } => {
            let text = if *repair {
                format!(
                    "Checks the downloaded game files and fetches what is missing: build \
                     {version} ({language}), up to its full size"
                )
            } else {
                format!("Game files, build {version} ({language})")
            };
            steps.push(step(StepKind::Download, text, source.sizes.map(|s| s.0)));
            phases.push(Phase::Files);
            source.sizes.map_or(0, |s| s.0)
        }
        Files::Cached(_) => {
            steps.push(step(
                StepKind::Info,
                format!("Uses the downloaded game files: build {version} ({language})"),
                None,
            ));
            0
        }
        Files::Installer(exe) => {
            let name = exe.file_name().map(|n| n.to_string_lossy().into_owned());
            steps.push(step(
                StepKind::Info,
                format!(
                    "Runs the downloaded offline installer {} silently",
                    name.unwrap_or_default()
                ),
                None,
            ));
            0
        }
    };
    for d in &tools.downloads {
        phases.push(match d.item {
            Item::Umu => Phase::Umu,
            Item::Proton => Phase::Proton,
            Item::Comet => continue,
        });
    }
    let named = |dlcs: &[gogo::gog::content::DlcRef]| {
        dlcs.iter()
            .map(|d| errors::path(&d.title))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !source.dlcs.is_empty() {
        let text = format!("With DLC: {}", named(&source.dlcs));
        steps.push(step(StepKind::Info, text, None));
    } else if !source.dlcs_not_taken.is_empty() {
        let text = format!(
            "Without DLC: an installer's DLC can't be added here ({})",
            named(&source.dlcs_not_taken)
        );
        steps.push(step(StepKind::Info, text, None));
    } else {
        steps.push(step(StepKind::Info, "No DLC", None));
    }
    let dir = errors::path(&preview.dirs.root.display().to_string());
    if preview.rebuild {
        steps.push(step(
            StepKind::Replace,
            format!("Replaces the game files in {dir}"),
            None,
        ));
        steps.push(step(
            StepKind::Snapshot,
            "A snapshot first, where the game's folder allows it and snapshots are on",
            None,
        ));
    } else {
        steps.push(step(StepKind::Info, format!("Into {dir}"), None));
    }
    tool_steps(tools, &Answers::default(), &mut steps);
    let renderer = preview
        .renderer
        .clone()
        .unwrap_or_else(|| "picked once the game's files are here".to_string());
    steps.push(step(StepKind::Info, format!("Renderer: {renderer}"), None));
    if !preview.verbs.is_empty() {
        let text = format!("winetricks verbs: {}", preview.verbs.join(", "));
        steps.push(step(StepKind::Info, text, None));
    }
    if preview.prefix_exists {
        steps.push(step(
            StepKind::Info,
            "Keeps the game's Wine prefix, with its saves",
            None,
        ));
    } else {
        steps.push(step(
            StepKind::Info,
            "Creates the Wine prefix; umu downloads the Steam Linux Runtime on its first \
             run, if it isn't there",
            None,
        ));
    }
    steps.push(step(
        StepKind::Info,
        format!(
            "The game's launcher will run {}",
            errors::path(&build.gogo.display().to_string())
        ),
        None,
    ));
    match source.files {
        Files::Installer(_) => phases.extend([Phase::Prefix, Phase::Installer, Phase::Move]),
        _ => phases.extend([Phase::Copy, Phase::Prefix]),
    }
    phases.push(Phase::Finish);
    if tools.install_comet {
        phases.push(Phase::Comet);
    }
    let download: u64 = fetch + tools.downloads.iter().map(|d| d.bytes).sum::<u64>();
    let mut warnings = vec![
        "Once building starts it can't be stopped, and the window stays open until it \
         ends."
            .to_string(),
    ];
    if fetch > 0 {
        warnings.push("The game files download first; that part can be cancelled.".to_string());
    }
    let plan = Plan {
        verb: verb.to_string(),
        title: format!("{verb} {title}?"),
        subject: title,
        steps,
        download: (download > 0).then_some(download),
        disk_delta: source
            .sizes
            .filter(|_| matches!(source.files, Files::Fetch { .. }))
            .and_then(|s| i64::try_from(s.1).ok()),
        free: None,
        warnings,
        reversible: false,
    };
    (plan, phases)
}

fn too_little() -> ErrorView {
    ErrorView::new(
        "Disk",
        "Not enough free space",
        "The downloads are larger than the space left.",
        "Free some space, then try again.",
        "6",
    )
}

/// What building would do, once `given` answers every question that isn't in `known`
/// already.
pub(super) fn build(
    pipelines: &dyn Pipelines,
    space: &dyn FreeSpace,
    session: Arc<dyn TokenProvider>,
    asked: Asked,
    known: Answers,
    given: &[PromptAnswer],
) -> Result<Planning, ErrorView> {
    let Some(gogo) = pipelines.gogo_cli() else {
        return Err(ErrorView::new(
            "Usage",
            "Install can't be planned",
            "A built game's launcher runs the gogo command, and none was found beside \
             this app or on PATH.",
            "Install the gogo command line tool.",
            "2",
        ));
    };
    let source: Source = pipelines
        .source(session, &asked.request)
        .map_err(|e| no_build(&e))?;
    let gogid = asked.product.to_string();
    let tree = match &source.files {
        Files::Cached(tree) => Some(tree.clone()),
        _ => None,
    };
    let request = BuildRequest {
        gogid: &gogid,
        tree: tree.as_deref(),
        runtime: asked.runtime.as_deref(),
        renderer: asked.renderer,
        library: asked.library.as_deref(),
    };
    let previewed = answered(NO_INSTALL, known, given, |a| {
        pipelines.preview_build(&request, a)
    })?;
    let (answers, preview) = match previewed {
        Ok(ready) => ready,
        Err(question) => return Ok(Planning::Ask(question)),
    };
    let build = Build {
        product: asked.product,
        source,
        runtime: asked.runtime.clone(),
        renderer: asked.renderer,
        gogo,
    };
    let (mut plan, phases) = build_card(&asked, &preview, &build);
    // The card leaves the answers to be saved to the tool lines.
    if let Some(saved) = saved(&answers) {
        plan.steps.push(saved);
    }
    let inputs = digest(&(
        &plan,
        &build.source,
        &preview.tools,
        &preview.rebuild,
        &answers,
    ));
    let fetched = match build.source.files {
        Files::Fetch { .. } => build.source.sizes.map_or(0, |s| s.1),
        _ => 0,
    };
    let tools: u64 = preview.tools.downloads.iter().map(|d| d.bytes).sum();
    let cache_free = free_at(space, &build.source.root);
    plan.free = free_at(space, &preview.dirs.root);
    if cache_free.is_some_and(|free| fetched > free) || plan.free.is_some_and(|f| tools > f) {
        return Err(too_little());
    }
    Ok(Planning::Ready(Box::new(Computed {
        gogid,
        title: asked.request.title.clone(),
        answers,
        plan,
        inputs,
        downloads: preview.tools.downloads.clone(),
        tools: preview.tools,
        phases,
        build: Some((asked, build)),
        rebuild: preview.rebuild,
        dir: preview.dirs.root,
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
            plain(&refused(NO_SETUP, &error), &["somewhere", said]);
        }
    }

    #[test]
    fn a_missing_source_is_in_the_apps_own_words() {
        let said = "GET somewhere/builds failed: expected `=`";
        for error in [
            DepotError::NoBuilds(10),
            DepotError::NoCacheDir,
            DepotError::Library(said.into()),
            DepotError::Filter(said.into()),
            DepotError::Io(std::io::Error::other(said)),
        ] {
            plain(&no_build(&error), &["somewhere", said]);
        }
    }
}
