//! What a download would do: the catalog's listing and local reads. No link is
//! resolved and nothing is created.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gogo::app::AppContext;
use gogo::config::Platform;
use gogo::engine::select::Filters;
use gogo::engine::task::{FileTask, Kind, Status};
use gogo::gog::catalog::{Os, Product};
use gogo_contract::{PlanFacts, Value};
use gogo_gui_model::{Call, ErrorView, Plan, PlanStep, StepKind};

use super::super::engine::Engine;
use super::super::{errors, text};

const TITLE: &str = "The download could not be planned";

/// What was asked for.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Request {
    pub(super) product: u64,
    os: Option<Os>,
    language: Option<String>,
    kinds: Vec<Kind>,
    dlc: Dlc,
}

#[derive(Debug, Clone, PartialEq)]
enum Dlc {
    None,
    All,
    Only(Vec<u64>),
}

impl Request {
    /// The request of a `game.download` call.
    pub(super) fn of(call: &Call) -> Result<Request, ErrorView> {
        let bad = || ErrorView::usage("This download can't be planned", "the selection is unknown");
        let product = call.text("id").and_then(|id| id.parse().ok());
        let mut request = Request {
            product: product.ok_or_else(bad)?,
            os: None,
            language: None,
            kinds: Vec::new(),
            dlc: Dlc::None,
        };
        let Some(Value::Map(selection)) = call.arg("selection") else {
            return Ok(request);
        };
        for (key, value) in selection {
            match (key.as_str(), value) {
                (_, Value::Null) => {}
                ("os", Value::Str(os)) => {
                    request.os = Some(match os.as_str() {
                        "windows" => Os::Windows,
                        "mac" => Os::Mac,
                        "linux" => Os::Linux,
                        _ => return Err(bad()),
                    });
                }
                ("lang", Value::Str(lang)) => request.language = Some(lang.clone()),
                ("kinds", Value::List(kinds)) => {
                    for kind in kinds {
                        request.kinds.push(match kind.as_str() {
                            Some("installer") => Kind::Installer,
                            Some("patch") => Kind::Patch,
                            Some("extra") => Kind::BonusContent,
                            Some("lang_pack") => Kind::LanguagePack,
                            _ => return Err(bad()),
                        });
                    }
                }
                ("dlc", Value::Str(all)) if all.as_str() == "all" => request.dlc = Dlc::All,
                ("dlc", Value::List(ids)) => {
                    let ids = ids
                        .iter()
                        .map(|id| id.as_str().and_then(|id| id.parse().ok()))
                        .collect::<Option<Vec<u64>>>();
                    request.dlc = Dlc::Only(ids.ok_or_else(bad)?);
                }
                _ => return Err(bad()),
            }
        }
        Ok(request)
    }
}

/// One file of the plan that is not on disk yet.
#[derive(Debug, Clone)]
pub(super) struct Wanted {
    pub(super) task: FileTask,
    /// Bytes an earlier run left.
    pub(super) done: u64,
}

impl Wanted {
    pub(super) fn left(&self) -> u64 {
        self.task.total_size.saturating_sub(self.done)
    }

    /// What makes this the same file as the one the user agreed to.
    fn identity(&self) -> (Option<u64>, String, Option<String>, u64, String, PathBuf) {
        let t = &self.task;
        (
            t.dlc_id,
            t.file.id.clone(),
            t.entry.version.clone(),
            t.total_size,
            t.file.downlink.clone(),
            t.dir.clone(),
        )
    }
}

/// A computed download.
pub(super) struct Computed {
    pub(super) request: Request,
    pub(super) title: String,
    pub(super) files: Vec<Wanted>,
    pub(super) plan: Plan,
    pub(super) inputs: u64,
}

impl Computed {
    pub(super) fn facts(&self) -> PlanFacts {
        if self.files.is_empty() {
            PlanFacts::nothing_to_do()
        } else {
            PlanFacts {
                needs_download: true,
                ..PlanFacts::changes()
            }
        }
    }

    pub(super) fn left(&self) -> u64 {
        self.files.iter().map(Wanted::left).sum()
    }
}

/// Whether `new` holds nothing that `agreed` did not: files that have arrived since are
/// progress, not a change. A file that has come to need a warning on the card is one.
pub(super) fn within(new: &[Wanted], agreed: &[Wanted]) -> bool {
    new.iter().all(|f| {
        let warned = matches!(f.task.status, Status::PendingAdoption | Status::Outdated);
        agreed
            .iter()
            .any(|a| a.identity() == f.identity() && (!warned || a.task.status == f.task.status))
    })
}

fn os_name(os: Os) -> &'static str {
    match os {
        Os::Windows => "Windows",
        Os::Mac => "macOS",
        Os::Linux => "Linux",
    }
}

/// The system installers are picked for when the selection names none; `None` is all.
fn default_os(platform: Platform) -> Option<Os> {
    match platform {
        Platform::Windows => Some(Os::Windows),
        Platform::Mac => Some(Os::Mac),
        Platform::Linux => Some(Os::Linux),
        Platform::All => None,
        Platform::Current if cfg!(target_os = "macos") => Some(Os::Mac),
        Platform::Current => Some(Os::Linux),
    }
}

/// The free space where `dir` will be, without creating it.
fn free_space(ctx: &AppContext, dir: &Path) -> Option<u64> {
    let there = dir.ancestors().find(|d| d.is_dir())?;
    ctx.free_space.available_bytes(there).ok()
}

fn owned_dlcs(product: &Product, owned: &[u64]) -> Vec<u64> {
    product
        .expanded_dlcs
        .iter()
        .filter(|d| owned.contains(&d.id))
        .filter(|d| {
            d.downloads
                .as_ref()
                .is_some_and(|d| !d.installers.is_empty())
        })
        .map(|d| d.id)
        .collect()
}

/// Plan `request`. `queued` is what other downloads still have to write.
pub(super) async fn compute(
    engine: &Engine,
    ctx: &Arc<AppContext>,
    request: Request,
    queued: u64,
) -> Result<Computed, ErrorView> {
    let Some(product) = engine
        .detail(request.product)
        .await
        .map_err(|e| errors::view(TITLE, &e))?
    else {
        return Err(errors::signed_out());
    };
    let owned = if product.expanded_dlcs.is_empty() {
        Vec::new()
    } else {
        engine
            .owned_ids()
            .await
            .map_err(|e| errors::view(TITLE, &e))?
    };
    let owned = owned_dlcs(&product, &owned);
    let only_dlcs = match &request.dlc {
        Dlc::None => Vec::new(),
        Dlc::All => owned.clone(),
        Dlc::Only(ids) => ids.iter().copied().filter(|i| owned.contains(i)).collect(),
    };
    let left_out = owned.len() - only_dlcs.len();

    // Another gogo may have downloaded since this one last looked.
    match gogo::state::State::load() {
        Ok(state) => *ctx.state.lock().expect("state lock") = state,
        Err(_) => tracing::warn!("the record of downloads could not be read again"),
    }
    let mut warnings = Vec::new();
    let mut filters = Filters {
        os: request.os.or(default_os(ctx.cfg.gog.platform)),
        language: Some(
            request
                .language
                .clone()
                .unwrap_or_else(|| ctx.cfg.gog.language.clone()),
        ),
        kinds: request.kinds.clone(),
        with_dlcs: !only_dlcs.is_empty(),
        only_dlcs: Some(only_dlcs),
    };
    let products = std::slice::from_ref(&product);
    let mut tasks = gogo::engine::plan(ctx, products, &filters, false).await;
    if tasks.is_empty()
        && request.os.is_none()
        && let Some(os) = filters.os.filter(|&os| os != Os::Windows)
    {
        filters.os = Some(Os::Windows);
        tasks = gogo::engine::plan(ctx, products, &filters, false).await;
        if !tasks.is_empty() {
            warnings.push(format!(
                "No installer for {}: these are for Windows.",
                os_name(os)
            ));
        }
    }
    if tasks.is_empty() {
        return Err(ErrorView::failed(
            "Nothing to download",
            "GOG lists no file for this selection",
        ));
    }

    let title = text::line(product.title.as_deref().unwrap_or("this game"));
    let here = tasks.iter().filter(|t| t.status == Status::Present).count();
    if tasks.iter().any(|t| t.status == Status::PendingAdoption) {
        warnings.push("Files already in the folder are checked and may be replaced.".into());
    }
    if tasks.iter().any(|t| t.status == Status::Outdated) {
        warnings.push("A newer version than the one you downloaded; old files stay.".into());
    }
    let files: Vec<Wanted> = tasks
        .into_iter()
        .filter(|t| t.status != Status::Present)
        .map(|task| {
            let done = gogo::engine::partial(&task).map_or(0, |p| p.done);
            Wanted { task, done }
        })
        .collect();

    let root = PathBuf::from(ctx.cfg.download.dir.clone().unwrap_or_default());
    let folder = files.first().map_or(&root, |f| &f.task.dir);
    let left: u64 = files.iter().map(Wanted::left).sum();
    let free = free_space(ctx, &root);
    if ctx.disk_check
        && let Some(free) = free
        && left > 0
        && left.saturating_add(queued) > free
    {
        let size = gogo::fmt::human_size;
        let mut message = format!("{} needed, {} free", size(left), size(free));
        if queued > 0 {
            message.push_str(&format!(", {} for other downloads", size(queued)));
        }
        return Err(ErrorView::new(
            "Disk",
            "Not enough free space",
            message,
            "Free some space or change the download folder.",
            "6",
        ));
    }

    let info = |text: String| PlanStep {
        kind: StepKind::Info,
        text,
        bytes: None,
    };
    let mut steps = vec![info(format!(
        "Save to {}",
        errors::path(&folder.display().to_string())
    ))];
    steps.extend(files.iter().map(|f| {
        let t = &f.task;
        let mut line = t.entry.name.clone().unwrap_or_else(|| t.file.id.clone());
        let language = t.entry.language_full.as_ref().or(t.entry.language.as_ref());
        for part in [
            t.entry.os.map(os_name),
            language.map(String::as_str),
            t.entry.version.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            line.push_str(" · ");
            line.push_str(part);
        }
        if t.entry.files.len() > 1
            && let Some(at) = t.entry.files.iter().position(|p| p.id == t.file.id)
        {
            line.push_str(&format!(" · part {} of {}", at + 1, t.entry.files.len()));
        }
        if f.done > 0 {
            line.push_str(&format!(
                " · resumes, {} already here",
                gogo::fmt::human_size(f.done)
            ));
        }
        PlanStep {
            kind: StepKind::Download,
            text: text::line(&line),
            bytes: Some(f.left()),
        }
    }));
    if here > 0 {
        steps.push(info(format!("Files already downloaded: {here}")));
    }
    if left_out > 0 {
        steps.push(info(format!("Owned DLC not included: {left_out}")));
    }

    let plan = Plan {
        verb: "Download".into(),
        title: format!("Download {title}?"),
        subject: title.clone(),
        steps,
        download: Some(left),
        disk_delta: i64::try_from(left).ok(),
        free,
        warnings,
        reversible: true,
    };
    let mut digest = DefaultHasher::new();
    for f in &files {
        f.identity().hash(&mut digest);
        format!("{:?}", f.task.status).hash(&mut digest);
        f.done.hash(&mut digest);
    }
    Ok(Computed {
        request,
        title,
        files,
        plan,
        inputs: digest.finish(),
    })
}
