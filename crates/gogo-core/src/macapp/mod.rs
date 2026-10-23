//! `gogo mac-app` pipeline (`docs/design/10` §2): resolve runtime → stage
//! prefix → silent install → locate entry point → write launcher + plist →
//! ad-hoc codesign. Synchronous by design — the heavy steps are child
//! processes and fs copies; the CLI runs it on a blocking thread.
//!
//! gogo tracks nothing after conversion (design 10 boundary): no install
//! records, no update detection — the bundle is the artifact.

pub mod bundle;
pub mod compat;
pub mod error;
pub mod info;
pub mod installer;
pub mod interpret;
pub mod launch;

use serde_json::Value;
pub mod catalog;
pub mod ddraw;
pub mod lutris;
pub mod pe;
pub mod prompt;
pub mod renderer;
pub mod runtime;
pub mod setup;
pub mod store;
pub mod tools;
pub mod verbs;

use std::path::{Path, PathBuf};

use crate::engine::events::ProgressEvent;

pub use error::MacAppError;
pub use prompt::{PromptAnswer, PromptFn, PromptKind, PromptSpec};
pub use renderer::{Renderer, RendererChoice};
pub use runtime::{RuntimeMode, RuntimeOptions};

use bundle::{Bundle, CodesignPolicy};

/// Everything resolved before any long step runs — also the plan summary the
/// CLI shows for confirmation.
#[derive(Debug, Clone)]
pub struct MacAppPlan {
    pub bundle: Bundle,
    pub bundle_path: PathBuf,
    pub display: String,
    pub slug: String,
    pub runtime: runtime::Runtime,
    /// `None` in the depot lane (design 15 D3: the tree replaces the install).
    pub installer: Option<installer::InstallPlan>,
    /// Depot lane: the source tree and its size on disk.
    pub game_tree: Option<PathBuf>,
    pub game_tree_bytes: u64,
    /// Estimated final size: runtime tree + installer parts (offline lane) or
    /// runtime + tree (depot lane).
    pub estimated_bytes: u64,
    /// The recipe choice carried to `execute` (resolved post-install).
    /// The recipe selected at plan time (§6 step 2); applied post-install.
    pub recipe: Option<SelectedRecipe>,
    /// `(id, version)` when `plan` downloaded a catalog runtime into the
    /// store (§6 step 3) — shown on the confirmation screen.
    pub runtime_downloaded: Option<(String, String)>,
    /// Lutris API base override (tests; see `MacAppOptions::lutris_base`).
    pub lutris_base: Option<String>,
    /// `--renderer` as given; applied at plan time (depot lane) or after
    /// the install (installer lane).
    pub renderer_choice: RendererChoice,
    /// The renderer the bundle will bake (#70), when the game files are
    /// known at plan time (depot lane). The installer lane decides after the
    /// install and reports it as a `RendererSelected` event.
    pub renderer: Option<renderer::Selection>,
    /// v2 bundles (design 16): the per-game state dirs the build writes
    /// into. The depot lane knows its gogid up front; the installer lane
    /// builds in a staging dir and moves it to `games/<gogid>/` once the
    /// install reveals the id. `None` = the fat, self-contained bundle.
    pub state: Option<launch::GameDirs>,
    /// Depot lane: the game's GOG id (installer lane: found post-install).
    pub gogid: Option<String>,
    /// `[gogo] path` for the launcher (decision 2).
    pub gogo_path: PathBuf,
    /// L1 winetricks verbs from the tree's imports (#72). Depot lane only:
    /// the installer lane scans once the install is done.
    pub auto_verbs: Option<Vec<verbs::AutoVerb>>,
}

/// Conversion inputs (CLI flags, design 10 §1).
#[derive(Debug, Clone, Default)]
pub struct MacAppOptions {
    /// Offline installer lane input (design 10). Exactly one of
    /// `installer` / `game_tree` is set (design 15 D2).
    pub installer: Option<PathBuf>,
    /// Depot lane input: an already-materialized game tree (design 15).
    pub game_tree: Option<PathBuf>,
    pub name: Option<String>,
    pub mode: RuntimeMode,
    pub runtime_path: Option<PathBuf>,
    /// `--runtime-version`: pin a catalog release tag (catalog mode only).
    pub runtime_version: Option<String>,
    pub out_dir: PathBuf,
    /// Overwrite an existing bundle (CLI `--yes`).
    pub overwrite: bool,
    /// Lutris recipe selection (design 11).
    pub recipe: RecipeChoice,
    /// Lutris API base override (tests pin a wiremock server; the CLI never
    /// sets it).
    pub lutris_base: Option<String>,
    /// `--renderer` (`--ddraw-wrapper` / `--no-ddraw-wrapper` are aliases).
    pub renderer: RendererChoice,
    /// Build today's fat bundle (runtime, prefix and game inside the `.app`)
    /// instead of a v2 launcher bundle. Kept as the basis for `--portable`
    /// (#84); not exposed on the CLI yet.
    pub portable: bool,
    /// The path gogo was invoked as, recorded in the bundle for the
    /// launcher (design 16 decision 2). `None` = the current executable.
    pub gogo_path: Option<PathBuf>,
}

/// `--recipe auto|none|<installer-slug>|<script-file>` (design 11 §1).
#[derive(Debug, Clone, Default, PartialEq)]
pub enum RecipeChoice {
    /// Live search + rank; the user picks via the injected prompt renderer
    /// when ambiguous (design 11 §1, design 12 §9).
    #[default]
    Auto,
    /// No recipe lookup.
    None,
    /// A pinned Lutris installer slug.
    Slug(String),
    /// A downloaded script JSON (`/api/installers/<slug>?format=json`).
    File(PathBuf),
}

impl RecipeChoice {
    /// Parse the CLI string: keywords, paths, or a bare installer slug.
    pub fn parse(s: &str) -> RecipeChoice {
        match s {
            "auto" => RecipeChoice::Auto,
            "none" => RecipeChoice::None,
            other => {
                let p = Path::new(other);
                if p.exists() || other.ends_with(".json") {
                    RecipeChoice::File(p.to_path_buf())
                } else {
                    RecipeChoice::Slug(other.to_string())
                }
            }
        }
    }
}

/// Bundle title: `--name` verbatim, or derived from the installer stem —
/// `setup_` stripped, separators spaced, first letter capitalized
/// (`setup_heroes3_gold.exe` → `Heroes3 gold`; design 10 §1).
pub fn display_name(installer: &Path, name: Option<&str>) -> String {
    if let Some(n) = name {
        return n.trim().to_string();
    }
    let stem = installer
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "game".to_string());
    let stem = stem.strip_prefix("setup_").unwrap_or(&stem).to_string();
    let stem = strip_version_tail(&stem).to_string();
    let spaced = stem.replace(['_', '-', '.'], " ");
    let spaced = spaced.trim().to_string();
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "Game".to_string(),
    }
}

/// GOG offline installers are `setup_<name>_<major.minor.patch…>.exe`: drop
/// the trailing underscore-delimited **dotted** version so the display name
/// (bundle title + Lutris search query) is the product name —
/// `setup_pharaoh_gold_2.1.0.15.exe` → "Pharaoh gold", not
/// "Pharaoh gold 2 1 0 15". Only multi-part dotted numbers count: titles like
/// "Portal 2" or "Heroes3 Gold" keep their trailing tokens.
fn strip_version_tail(stem: &str) -> &str {
    let Some(pos) = stem.rfind('_') else {
        return stem;
    };
    let tail = &stem[pos + 1..];
    let parts = tail.split('.');
    let all_numeric = parts
        .clone()
        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    let multi_part = parts.count() >= 2;
    if all_numeric && multi_part {
        &stem[..pos]
    } else {
        stem
    }
}

/// Slug: lowercase, non-alphanumerics → `-`, collapsed, trimmed (design 10 §5).
/// Drives the bundle identifier and the Windows install dir — deterministic,
/// no spaces.
pub fn slugify(display: &str) -> String {
    let mut slug = String::new();
    for ch in display.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

/// A recipe selected during planning (design 12 §6 step 2): the fetched
/// installer script plus the context needed to apply it post-install.
#[derive(Debug, Clone)]
pub struct SelectedRecipe {
    pub installer: lutris::LutrisInstaller,
    /// `wine.version` pin (advisory, D6) — feeds the runtime recommendation.
    pub wine_version: Option<String>,
    /// Discovered via live search: the post-install gogid cross-check applies
    /// (design 11 §2). Explicit slugs/files are trusted as pinned.
    pub from_search: bool,
}

/// Ranked recipe candidates for a game name (design 11 §2, shared by
/// `mac-app` auto mode and `gogo recipes list`): search → best game → its
/// `runner: wine` installers, ranked. Nothing is stored.
pub fn recipe_candidates(
    base: Option<&str>,
    display: &str,
) -> Result<Vec<lutris::LutrisInstaller>, MacAppError> {
    let client = lutris::LutrisClient::new(base.unwrap_or(lutris::DEFAULT_BASE))?;
    let discovery = {
        let client = &client;
        async move {
            // Lutris search is strict: "pharaoh gold" answers 0 results
            // while the game is listed as "Pharaoh" (live-verified
            // 2026-10-19). Drop trailing tokens until something answers.
            let mut query = display.to_string();
            let games: Vec<lutris::LutrisGame> = loop {
                let found = client.search(&query).await.unwrap_or_default();
                if !found.is_empty() || !query.contains(' ') {
                    break found;
                }
                query = query
                    .rsplit_once(' ')
                    .map(|(head, _)| head.to_string())
                    .unwrap_or_default();
            };
            let Some(game) = lutris::pick_game(&games, display) else {
                return Ok::<Vec<lutris::LutrisInstaller>, MacAppError>(Vec::new());
            };
            let mut candidates = client.installers_for(&game.slug).await?;
            candidates.retain(|i| i.runner == "wine");
            lutris::rank(&mut candidates);
            Ok(candidates)
        }
    };
    lutris::block_on_detached(discovery)
}

/// Fetch one installer record by slug (live; `gogo recipes show`).
pub fn recipe_by_slug(
    base: Option<&str>,
    slug: &str,
) -> Result<lutris::LutrisInstaller, MacAppError> {
    let client = lutris::LutrisClient::new(base.unwrap_or(lutris::DEFAULT_BASE))?;
    lutris::block_on_detached(client.installer(slug))
}

/// Inspection-only planning context (`gogo recipes show`, design 11 §3):
/// classification is pure — paths are substituted and displayed but never
/// touched — so the nominal root stands in for the not-yet-built bundle.
/// The 1920x1080 stand-in matches `run_recipe_phase`.
pub fn inspection_ctx(script: &serde_json::Value) -> interpret::RecipeCtx {
    let nominal = PathBuf::from("/gogo-recipes-show");
    interpret::RecipeCtx {
        wine: nominal.join("wine"),
        prefix: nominal.join("prefix"),
        install_dir: nominal.join("prefix/drive_c/GOG"),
        bundle: nominal.clone(),
        cache: nominal.join("cache"),
        user: "user".into(),
        width: 1920,
        height: 1080,
        vars: script
            .get("variables")
            .and_then(serde_json::Value::as_object)
            .map(|o| {
                o.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// §6 step 2: resolve the recipe choice to a script, or `None`. Auto-mode
/// trouble is never fatal: `RecipeMiss` + recipe-less continuation. Slug/file
/// errors stay fatal — the user pinned them explicitly.
fn select_recipe(
    opts: &MacAppOptions,
    display: &str,
    prompt: Option<&prompt::PromptFn>,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<Option<SelectedRecipe>, MacAppError> {
    let client =
        lutris::LutrisClient::new(opts.lutris_base.as_deref().unwrap_or(lutris::DEFAULT_BASE))?;
    let (inst, from_search) = match &opts.recipe {
        RecipeChoice::None => return Ok(None),
        RecipeChoice::Slug(slug) => (lutris::block_on_detached(client.installer(slug))?, false),
        RecipeChoice::File(path) => (parse_script_file(path)?, false),
        RecipeChoice::Auto => {
            // Pre-install search by display name (§6): pick the best game,
            // list its installers, rank, select. The authoritative gogid
            // cross-check runs post-install and downgrades wrong picks.
            match recipe_candidates(opts.lutris_base.as_deref(), display) {
                Ok(candidates) if candidates.is_empty() => {
                    emit(&ProgressEvent::RecipeMiss {
                        key: display.to_string(),
                    });
                    return Ok(None);
                }
                Ok(candidates) => {
                    // §1 (amended 2026-10-19): interactively the picker is
                    // mandatory — applying community patches is the user's
                    // decision every time, single candidate included ("none"
                    // is always offered); headless runs take the ranked best.
                    if let Some(p) = prompt {
                        match choose_recipe(&candidates, display, p)? {
                            Some(RecipePick::Installer(inst)) => (inst, true),
                            Some(RecipePick::Choice(RecipeChoice::None)) => return Ok(None),
                            Some(RecipePick::Choice(RecipeChoice::Slug(slug))) => {
                                (lutris::block_on_detached(client.installer(&slug))?, false)
                            }
                            Some(RecipePick::Choice(RecipeChoice::File(path))) => {
                                (parse_script_file(&path)?, false)
                            }
                            Some(RecipePick::Choice(RecipeChoice::Auto)) => {
                                unreachable!("resolved by choose_recipe")
                            }
                            None => return Ok(None),
                        }
                    } else {
                        (
                            candidates
                                .into_iter()
                                .next()
                                .ok_or_else(|| MacAppError::Recipe("no candidates".into()))?,
                            true,
                        )
                    }
                }
                Err(e) => {
                    emit(&ProgressEvent::RecipeMiss { key: e.to_string() });
                    return Ok(None);
                }
            }
        }
    };

    let wine_version = lutris::wine_version(&inst.script);
    emit(&ProgressEvent::RecipeResolved {
        installer_slug: inst.slug.clone(),
        version: inst.version.clone(),
        author: inst.author.clone(),
    });
    Ok(Some(SelectedRecipe {
        installer: inst,
        wine_version,
        from_search,
    }))
}

/// A recipe-picker outcome: a ranked candidate, or a choice derived from
/// free text (§9: slug first, then local script path).
enum RecipePick {
    Installer(lutris::LutrisInstaller),
    Choice(RecipeChoice),
}

/// §9 recipe picker: FuzzySelect over the ranked candidates with trailing
/// custom/none entries. Cancel (renderer `None`) opts out of the recipe.
fn choose_recipe(
    candidates: &[lutris::LutrisInstaller],
    display: &str,
    prompt: &prompt::PromptFn,
) -> Result<Option<RecipePick>, MacAppError> {
    use prompt::{PromptAnswer, PromptKind, PromptSpec};
    let spec = PromptSpec {
        kind: PromptKind::Fuzzy,
        title: format!("apply a Lutris recipe for \u{201c}{display}\u{201d}?"),
        items: candidates
            .iter()
            .map(|c| {
                format!(
                    "{} — {} ({}, updated {})",
                    c.slug, c.version, c.author, c.updated_at
                )
            })
            .collect(),
        allow_other: true,
        allow_none: true,
        default: 0,
    };
    let Some(answer) = prompt(&spec) else {
        return Ok(None);
    };
    let pick = match answer {
        PromptAnswer::Index(i) => candidates.get(i).cloned().map(RecipePick::Installer),
        PromptAnswer::NoneSelected | PromptAnswer::Confirmed(_) => None,
        PromptAnswer::Other(text) => {
            let t = text.trim();
            if t.is_empty() {
                None
            } else {
                // Free text: a Lutris installer slug first, then a local
                // script file path (§9).
                let p = Path::new(t);
                Some(RecipePick::Choice(if p.exists() || t.ends_with(".json") {
                    RecipeChoice::File(p.to_path_buf())
                } else {
                    RecipeChoice::Slug(t.to_string())
                }))
            }
        }
    };
    Ok(pick)
}

/// Parse a downloaded script JSON into a record (the `--recipe <file>` lane).
/// Parse a local script JSON into an installer record (`recipes show <file>`
/// and `--recipe <file>`): slug from the file stem, pinned semantics.
pub fn parse_script_file(path: &Path) -> Result<lutris::LutrisInstaller, MacAppError> {
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(path)
            .map_err(|e| MacAppError::Recipe(format!("script {}: {e}", path.display())))?,
    )
    .map_err(|e| MacAppError::Recipe(format!("script parse: {e}")))?;
    Ok(lutris::LutrisInstaller {
        slug: path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        game_slug: String::new(),
        name: String::new(),
        version: "local file".into(),
        runner: "wine".into(),
        author: "local file".into(),
        updated_at: String::new(),
        gogid: None,
        script: v,
    })
}

/// A catalog lane hit: the manifest entry plus whether it was downloaded
/// just now (false = satisfied from the store cache).
fn catalog_lane(
    id: &str,
    version: Option<&str>,
    consent: bool,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<(store::ManifestEntry, bool), MacAppError> {
    if let Some((m, _)) = store::installed(id, version) {
        return Ok((m, false));
    }
    let entry = catalog::find(id).ok_or_else(|| MacAppError::RuntimeNotFound {
        tried: format!("unknown catalog id '{id}' (see `gogo runtimes list`)"),
    })?;
    let http = catalog::http_client()?;
    let m = lutris::block_on_detached(store::install(
        entry,
        version,
        &http,
        catalog::GITHUB_API,
        consent,
        emit,
    ))?;
    Ok((m, true))
}

/// §6 step 3 + §5: runtime resolution. Explicit flags win (D2 precedence);
/// `auto` prefers cached runtimes (the recipe recommendation first, then the
/// default track, `winecx`), then brew, then downloads the recommendation into the
/// store (network exception D8). Not in runtime.rs: this needs the catalog +
/// store (network), and runtime.rs stays pure fs/exec.
fn resolve_runtime(
    opts: &MacAppOptions,
    recipe: Option<&SelectedRecipe>,
    prompt: Option<&prompt::PromptFn>,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<(runtime::Runtime, Option<(String, String)>), MacAppError> {
    if let RuntimeMode::Catalog(id) = &opts.mode {
        let (m, downloaded) = catalog_lane(id, opts.runtime_version.as_deref(), false, emit)?;
        let rt = runtime::from_store(&m)?;
        return Ok((rt, downloaded.then(|| (m.id.clone(), m.version.clone()))));
    }
    if opts.mode != RuntimeMode::Auto || opts.runtime_path.is_some() {
        // Flag precedence (D2): brew/gptk/path resolve exactly as before.
        let rt = runtime::resolve(&runtime::RuntimeOptions {
            mode: opts.mode.clone(),
            runtime_path: opts.runtime_path.clone(),
            version: opts.runtime_version.clone(),
        })?;
        return Ok((rt, None));
    }

    // `auto` (§5 ladder): cached(recipe-recommended) → cached(default) →
    // brew → download the recommendation.
    let rec = recipe
        .and_then(|r| r.wine_version.as_deref())
        .map(|v| catalog::recommend_for_wine(Some(v)));
    if let Some(notice) = rec.as_ref().and_then(|r| r.notice) {
        emit(&ProgressEvent::Warn {
            message: format!("recipe wine pin: {notice}"),
        });
    }
    let mut lanes: Vec<&str> = rec.as_ref().map(|r| r.id).into_iter().collect();
    if !lanes.contains(&catalog::DEFAULT_RUNTIME) {
        lanes.push(catalog::DEFAULT_RUNTIME);
    }
    // §5 (amended 2026-10-19): the ladder only computes the "easy" default
    // now — interactively the runtime menu is MANDATORY and lists every
    // option (cached, locally installed, downloadable) with the easy pick
    // preselected; headless runs keep the Phase-3 behavior.
    let mut easy: Option<(runtime::Runtime, String)> = None;
    'ladder: for id in lanes {
        if let Some((m, _)) = store::installed(id, None) {
            match runtime::from_store(&m) {
                Ok(rt) => {
                    easy = Some((rt, format!("{}@{}", m.id, m.version)));
                    break 'ladder;
                }
                Err(e) => emit(&ProgressEvent::Warn {
                    message: format!("cached {id} unusable: {e} — trying next source"),
                }),
            }
        }
    }
    if easy.is_none()
        && let Ok(rt) = runtime::resolve(&runtime::RuntimeOptions {
            mode: RuntimeMode::Brew,
            ..Default::default()
        })
    {
        easy = Some((rt, "brew".into()));
    }
    if let Some(prompt) = prompt {
        return choose_runtime(prompt, easy, emit);
    }
    if let Some((rt, _)) = easy {
        return Ok((rt, None));
    }
    let id = rec
        .as_ref()
        .map(|r| r.id)
        .unwrap_or(catalog::DEFAULT_RUNTIME);
    let (m, downloaded) = catalog_lane(id, None, false, emit)?;
    let rt = runtime::from_store(&m)?;
    Ok((rt, downloaded.then(|| (m.id.clone(), m.version.clone()))))
}

/// The §5 runtime menu — MANDATORY in interactive runs (amended
/// 2026-10-19): every option is listed, cached flavors first (✓ cached),
/// locally installed lanes (brew / GPTK) with their probed version,
/// downloadable flavors with arch tags, GPTK downloads behind the D9
/// consent gate (Apple Silicon hosts only), a custom path lane with
/// re-prompt on invalid input (§9), and cancel. The ladder's pick arrives
/// as `easy` and is preselected (spec.default + "· easy" marker).
fn choose_runtime(
    prompt: &prompt::PromptFn,
    easy: Option<(runtime::Runtime, String)>,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<(runtime::Runtime, Option<(String, String)>), MacAppError> {
    use prompt::{PromptAnswer, PromptKind, PromptSpec};

    let cancelled = || MacAppError::RuntimeNotFound {
        tried: "runtime selection cancelled".into(),
    };
    let rosetta_missing =
        cfg!(target_arch = "aarch64") && !rosetta_present() && !cfg!(target_os = "linux");

    // Lanes: cached flavors, then locally installed (brew, GPTK), then
    // downloadable ones in catalog order.
    let manifest = store::load_manifest();
    let mut lanes: Vec<MenuLane> = Vec::new();
    for entry in catalog::entries() {
        if !matches!(entry.kind, catalog::Kind::Wine) {
            continue;
        }
        let selectable = matches!(entry.status, catalog::Status::Available)
            || (matches!(entry.status, catalog::Status::ConsentRequired)
                && cfg!(target_arch = "aarch64"));
        if !selectable {
            continue;
        }
        if let Some(m) = manifest.iter().find(|m| m.id == entry.id).cloned() {
            lanes.push(MenuLane::Cached(entry, m));
        } else {
            lanes.push(MenuLane::Fresh(entry));
        }
    }
    // Locally installed lanes: the ladder may have probed brew already;
    // GPTK gets its own probe. Both are listed only when actually present.
    let mut local = |key: &str, probed: Option<runtime::Runtime>| {
        if let Some(rt) = probed {
            lanes.push(MenuLane::Local {
                label: format!("{key} {} (installed)", rt.version),
                key: key.to_string(),
                rt,
            });
        }
    };
    let brew = match &easy {
        Some((rt, key)) if key == "brew" => Some(rt.clone()),
        _ => runtime::resolve(&runtime::RuntimeOptions {
            mode: RuntimeMode::Brew,
            ..Default::default()
        })
        .ok(),
    };
    local("brew", brew);
    local(
        "gptk",
        runtime::resolve(&runtime::RuntimeOptions {
            mode: RuntimeMode::Gptk,
            ..Default::default()
        })
        .ok(),
    );

    // The easy default: the ladder pick if it maps to a lane, else the
    // default-track recommendation.
    let default_idx = match &easy {
        Some((_, key)) => lanes.iter().position(|l| match (l, key.as_str()) {
            (MenuLane::Cached(_, m), k) => format!("{}@{}", m.id, m.version) == k,
            (MenuLane::Local { key, .. }, k) => key == k,
            _ => false,
        }),
        None => lanes
            .iter()
            .position(|l| matches!(l, MenuLane::Fresh(e) if e.id == catalog::DEFAULT_RUNTIME)),
    }
    .unwrap_or(0);

    let items: Vec<String> = lanes
        .iter()
        .enumerate()
        .map(|(i, lane)| {
            let mut line = match lane {
                MenuLane::Cached(entry, m) => format!("{} {} (✓ cached)", entry.id, m.version),
                MenuLane::Local { label, .. } => label.clone(),
                MenuLane::Fresh(entry) => {
                    let mut line = format!("download {} — {}", entry.id, entry.arch);
                    if entry.id == catalog::DEFAULT_RUNTIME {
                        line.push_str(" · recommended");
                    }
                    if rosetta_missing && entry.arch.starts_with("x86_64") {
                        line.push_str(" — Rosetta missing: softwareupdate --install-rosetta");
                    }
                    if matches!(entry.status, catalog::Status::ConsentRequired) {
                        line.push_str(" (Apple EULA consent)");
                    }
                    line
                }
            };
            if i == default_idx {
                line.push_str(" · easy");
            }
            line
        })
        .collect();

    let spec = PromptSpec {
        kind: PromptKind::Select,
        title: "which wine runtime? (the · easy entry is gogo's default pick)".into(),
        items,
        // "other…" = a custom runtime path (directory containing bin/wine).
        allow_other: true,
        allow_none: false,
        default: default_idx,
    };
    let Some(answer) = prompt(&spec) else {
        return Err(cancelled());
    };
    match answer {
        PromptAnswer::Index(i) => match lanes.get(i) {
            Some(MenuLane::Cached(_, m)) => {
                let rt = runtime::from_store(m)?;
                Ok((rt, None))
            }
            Some(MenuLane::Local { rt, .. }) => Ok((rt.clone(), None)),
            Some(MenuLane::Fresh(entry)) => {
                // D9: GPTK downloads only on explicit consent with the EULA
                // notice in front of the user.
                let consented = if matches!(entry.status, catalog::Status::ConsentRequired) {
                    let consent = prompt(&PromptSpec {
                        kind: PromptKind::Confirm { default: false },
                        title: format!(
                            "{} is covered by Apple's evaluation EULA; gogo downloads it into \
                             your local runtime store and never redistributes it inside bundles. \
                             Download it now?",
                            entry.id
                        ),
                        items: vec![],
                        allow_other: false,
                        allow_none: false,
                        default: 0,
                    });
                    matches!(consent, Some(PromptAnswer::Confirmed(true)))
                } else {
                    true
                };
                if !consented {
                    return Err(cancelled());
                }
                let (m, downloaded) = catalog_lane(entry.id, None, consented, emit)?;
                let rt = runtime::from_store(&m)?;
                Ok((rt, downloaded.then(|| (m.id.clone(), m.version.clone()))))
            }
            None => Err(cancelled()),
        },
        PromptAnswer::Other(text) => {
            // Custom path with re-prompt on invalid input (§9), max 3 tries.
            let mut path = text.trim().to_string();
            for attempt in 0..3 {
                match runtime::resolve(&runtime::RuntimeOptions {
                    mode: RuntimeMode::Path,
                    runtime_path: Some(PathBuf::from(&path)),
                    ..Default::default()
                }) {
                    Ok(rt) => return Ok((rt, None)),
                    Err(e) if attempt < 2 => {
                        let again = prompt(&PromptSpec {
                            kind: PromptKind::Input,
                            title: format!(
                                "invalid runtime path ({e}) — directory containing bin/wine:"
                            ),
                            items: vec![],
                            allow_other: true,
                            allow_none: false,
                            default: 0,
                        });
                        match again {
                            Some(PromptAnswer::Other(t)) if !t.trim().is_empty() => {
                                path = t.trim().to_string();
                            }
                            _ => return Err(cancelled()),
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
            unreachable!("re-prompt loop exits via return")
        }
        PromptAnswer::NoneSelected | PromptAnswer::Confirmed(_) => Err(cancelled()),
    }
}

/// One selectable row of the §5 runtime menu.
enum MenuLane {
    /// A catalog flavor already in the local store.
    Cached(&'static catalog::CatalogEntry, store::ManifestEntry),
    /// A locally installed runtime found by host probe (brew, GPTK).
    Local {
        label: String,
        key: String,
        rt: runtime::Runtime,
    },
    /// A downloadable catalog flavor.
    Fresh(&'static catalog::CatalogEntry),
}

/// Rosetta 2 presence (Apple Silicon hosts): best-effort marker files.
fn rosetta_present() -> bool {
    [
        "/Library/Apple/usr/share/rosetta/rosetta",
        "/usr/libexec/rosetta/oahd",
    ]
    .iter()
    .any(|p| Path::new(p).exists())
}

/// Resolve everything up-front: names, runtime (probed), installer (validated),
/// sizes. Emits `RuntimeResolved`. No bundle dirs are created yet.
pub fn plan(
    opts: &MacAppOptions,
    prompt: Option<&prompt::PromptFn>,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<MacAppPlan, MacAppError> {
    assert!(
        opts.installer.is_some() ^ opts.game_tree.is_some(),
        "exactly one source: installer or game_tree (design 15 D2)"
    );
    // Design 15 §1: depot display name prefers the goggame info's own name.
    let display = match (opts.game_tree.as_ref(), opts.name.as_deref()) {
        (Some(_), Some(n)) => n.to_string(),
        (Some(tree), None) => info::game_name(tree).unwrap_or_else(|| {
            tree.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Game".to_string())
        }),
        (None, _) => display_name(opts.installer.as_ref().unwrap(), opts.name.as_deref()),
    };
    let slug = slugify(&display);

    // §6 step 2: recipe discovery + selection precede the runtime — a wine
    // pin feeds the recommendation (D2: flag > recipe > default). Depot mode
    // skips recipes wholesale (design 15 D6): Lutris scripts are
    // installer-shaped and the tree already is the installed artifact.
    let recipe = if opts.game_tree.is_some() {
        None
    } else {
        select_recipe(opts, &display, prompt, emit)?
    };

    // §6 step 3: runtime resolution — may download into the store (events).
    let (runtime, runtime_downloaded) = resolve_runtime(opts, recipe.as_ref(), prompt, emit)?;
    let kind = runtime.mode.as_str().to_string();
    let wine = runtime.wine.display().to_string();
    emit(&ProgressEvent::RuntimeResolved { kind, path: wine });

    let install = opts
        .installer
        .as_ref()
        .map(|p| installer::plan(p))
        .transpose()?;
    let game_tree_bytes = match &opts.game_tree {
        Some(tree) => dir_size(tree)?,
        None => 0,
    };

    let mut bundle = bundle::layout(&opts.out_dir, &display);
    // Design 16: the prefix and game files live in the state dir, never in
    // the bundle.
    let (state, gogid) = if opts.portable {
        (None, None)
    } else if let Some(tree) = &opts.game_tree {
        let id = info::root_game_id(tree)
            .ok_or_else(|| MacAppError::EntryPointNotFound { dir: tree.clone() })?;
        (Some(launch::GameDirs::for_game(&id)?), Some(id))
    } else {
        let staging = launch::games_dir().join(format!(".build-{slug}-{}", std::process::id()));
        (Some(launch::GameDirs::at(staging)), None)
    };
    if let Some(dirs) = &state {
        bundle.prefix = dirs.prefix.clone();
    }
    if bundle.root.exists() {
        if !opts.overwrite {
            return Err(MacAppError::BundleExists {
                path: bundle.root.clone(),
            });
        }
        // Overwrite confirmed (--yes): clear the stale bundle so execute()
        // starts from a clean root instead of refusing it.
        std::fs::remove_dir_all(&bundle.root)?;
    }

    let runtime_bytes = if opts.portable {
        dir_size(&runtime.root)?
    } else {
        0
    };
    let estimated_bytes = runtime_bytes
        + match (&install, &opts.game_tree) {
            (Some(install), _) => {
                file_size(&install.installer)?
                    + install
                        .bin_parts
                        .iter()
                        .filter_map(|p| file_size(p).ok())
                        .sum::<u64>()
            }
            (None, Some(_)) => game_tree_bytes,
            (None, None) => unreachable!("asserted above"),
        };

    // One import scan (#63) feeds both L1 fields of the depot lane: the
    // renderer (#70) and the winetricks verbs (#72). The tree is the
    // installed game, so both show up in the confirmation summary.
    let scan = opts.game_tree.as_ref().map(|tree| pe::scan(tree));
    let auto_verbs = match (&scan, opts.portable) {
        (Some(scan), false) => Some(auto_verbs(scan, emit)),
        _ => None,
    };
    let renderer = opts
        .game_tree
        .as_ref()
        .zip(scan.as_ref())
        .map(|(tree, scan)| {
            let sel = renderer::select(
                info::root_game_id(tree).as_deref(),
                renderer::pick(scan),
                opts.renderer,
            );
            emit_selection(&sel, emit);
            sel
        });

    Ok(MacAppPlan {
        auto_verbs,
        bundle_path: bundle.root.clone(),
        display,
        slug,
        runtime,
        installer: install,
        game_tree: opts.game_tree.clone(),
        game_tree_bytes,
        estimated_bytes,
        recipe,
        runtime_downloaded,
        lutris_base: opts.lutris_base.clone(),
        renderer_choice: opts.renderer,
        renderer,
        bundle,
        state,
        gogid,
        gogo_path: opts
            .gogo_path
            .as_deref()
            .map(launch::absolute)
            .or_else(|| std::env::current_exe().ok())
            .unwrap_or_else(|| PathBuf::from("gogo")),
    })
}

/// L1 verbs from an import scan; a tree the scan couldn't read gets none
/// (with a warning), since verbs are an improvement, not a requirement.
fn auto_verbs(
    scan: &Result<pe::PeScan, pe::ScanError>,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Vec<verbs::AutoVerb> {
    match scan {
        Ok(scan) => verbs::for_scan(scan),
        Err(e) => {
            emit(&ProgressEvent::Warn {
                message: format!("import scan failed ({e}); no automatic winetricks verbs"),
            });
            Vec::new()
        }
    }
}

/// Report a renderer selection: the event, plus its warnings.
fn emit_selection(sel: &renderer::Selection, emit: &mut dyn FnMut(&ProgressEvent)) {
    emit(&ProgressEvent::RendererSelected {
        renderer: sel.renderer.value.clone(),
        layer: sel.renderer.layer.describe().to_string(),
        reason: sel.reason.clone(),
    });
    for message in &sel.warnings {
        emit(&ProgressEvent::Warn {
            message: message.clone(),
        });
    }
}

/// Write the `HKLM\Software\GOG.com\Games\<id>` key (32-bit view) that GOG
/// installers create and GOG support installers check as a prerequisite
/// (design 15 D3 amendment: fresh depot prefixes lack it, so the support
/// installer "fails to initialize" — live finding 2026-10-20, exit 1 under
/// both GPTK and wine-staging). Values mirror the offline installer's output.
fn write_gog_game_key(
    bundled_wine: &Path,
    prefix: &Path,
    id: &str,
    name: &str,
    win_path: &str,
    exe_file: &str,
) -> Result<(), MacAppError> {
    let key = format!(r"Software\GOG.com\Games\{id}");
    let exe = format!(r"{win_path}\{exe_file}");
    let dll = format!("goggame-{id}.dll");
    let values: [(&str, String); 11] = [
        ("gameID", id.to_string()),
        ("GAMENAME", name.to_string()),
        ("PATH", win_path.to_string()),
        ("EXE", exe.clone()),
        ("EXEFILE", exe_file.to_string()),
        ("GOGGAMEDLL", dll),
        ("LAUNCHCOMMAND", format!("{exe} ")),
        ("LAUNCHPARAM", String::new()),
        ("SAVEGAMEFOLDER", format!(r"{win_path}\")),
        ("DEPENDSON", String::new()),
        ("DLC", String::new()),
    ];
    for (value, data) in values {
        let argv = vec![
            "reg".to_string(),
            "add".to_string(),
            format!(r"HKLM\{key}"),
            "/v".to_string(),
            value.to_string(),
            "/t".to_string(),
            "REG_SZ".to_string(),
            "/d".to_string(),
            data,
            "/f".to_string(),
            "/reg:32".to_string(),
        ];
        interpret::run_in_prefix(
            bundled_wine,
            prefix,
            &argv,
            std::time::Duration::from_secs(60),
        )
        .map_err(|e| MacAppError::Recipe(format!("gog registry: {e}")))?;
    }
    Ok(())
}

/// The depot cache's GOG support installer (`gog-support/**/galaxy_*.exe`,
/// sibling of the install tree): writes the config/registry side artifacts
/// the game exe needs at startup (design 15 D3, amended 2026-10-20 — live
/// finding: `Pharaoh.ini`'s `CDDrive` hint; without it the exe stalls in its
/// CD check: black screen, no audio, no crash).
pub fn locate_support_installer(tree: &Path) -> Option<PathBuf> {
    let support_root = tree.parent()?.join("gog-support");
    fn walk(dir: &Path, out: &mut Option<PathBuf>) {
        if out.is_some() {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                let lower = name.to_ascii_lowercase();
                if lower.starts_with("galaxy") && lower.ends_with(".exe") {
                    *out = Some(path);
                    return;
                }
            }
        }
    }
    let mut found = None;
    walk(&support_root, &mut found);
    found
}

/// The depot cache's GOG support overlays: `gog-support/<id>/<id>/app/`
/// (sibling of the install tree, one per product id that carries one),
/// files Galaxy merges into the install dir (design 15 §3, docs/19 §1 item
/// 4: Carmageddon TDR 2000 aborts at `File.cpp:73` without the overlay's
/// `ASSETS/options.txt`). Sorted for a stable merge order.
pub fn locate_support_overlays(tree: &Path) -> Vec<PathBuf> {
    let Some(support_root) = tree.parent().map(|p| p.join("gog-support")) else {
        return Vec::new();
    };
    let subdirs = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut found: Vec<PathBuf> = subdirs(&support_root)
        .iter()
        .flat_map(|outer| subdirs(outer))
        .map(|inner| inner.join("app"))
        .filter(|app| app.is_dir())
        .collect();
    found.sort();
    found
}

/// Run the conversion. `codesign` is [`CodesignPolicy::Adhoc`] on macOS hosts,
/// [`CodesignPolicy::Skip`] elsewhere/tests (design 10 §2.7). Emits the §6
/// event sequence. Returns the final bundle size in bytes.
pub fn execute(
    plan: &MacAppPlan,
    codesign: CodesignPolicy,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<u64, MacAppError> {
    // A concurrent conversion writing into the same bundle is a clobber race.
    if plan.bundle.root.exists() {
        return Err(MacAppError::BundleExists {
            path: plan.bundle.root.clone(),
        });
    }
    bundle::create_layout(&plan.bundle)?;

    // v2 (design 16): Wine runs from the store and the prefix lives in the
    // state dir. The fat bundle copies the runtime in first: a failing copy
    // must not leave a half-installed prefix behind as the only artifact.
    let bundled_wine = if plan.state.is_some() {
        plan.runtime.wine.clone()
    } else {
        emit(&ProgressEvent::Warn {
            message: format!(
                "copying runtime {} (~{} MiB) into the bundle",
                plan.runtime.version,
                plan.estimated_bytes / (1024 * 1024)
            ),
        });
        bundle::copy_runtime(&plan.runtime, &plan.bundle.resources.join("wine"))?;
        bundle::copy_runtime_dylibs(
            &plan.runtime,
            &plan.bundle.resources.join("wine-frameworks"),
        )?;
        // In-bundle wine path = the runtime binary's own relative position.
        let rel = plan
            .runtime
            .wine
            .strip_prefix(&plan.runtime.root)
            .unwrap_or(Path::new("bin/wine"));
        plan.bundle.resources.join("wine").join(rel)
    };

    emit(&ProgressEvent::PrefixStaged);
    // A rebuild keeps the game's prefix (and the saves in it, decision 6);
    // only `game/` is re-cloned.
    if plan.state.is_some() && plan.bundle.prefix.is_dir() {
        emit(&ProgressEvent::Warn {
            message: format!(
                "reusing the existing prefix {} (saves kept)",
                plan.bundle.prefix.display()
            ),
        });
        // Build steps run unix-path installers through z:, which the last
        // setup dropped; `finish_v2` drops it again.
        let z = plan.bundle.prefix.join("dosdevices/z:");
        if z.symlink_metadata().is_err() {
            #[cfg(unix)]
            std::os::unix::fs::symlink("/", &z)?;
        }
    } else if plan.state.is_some() {
        // Design 16 decision 4 (#67): no links into the builder's home.
        setup::create_prefix(&plan.runtime, &plan.bundle.prefix)?;
    } else {
        installer::stage_prefix(&plan.runtime, &plan.bundle.prefix)?;
    }

    // Design 15 D3: the depot lane swaps exactly this step — a tree clone
    // instead of a silent installer run. Layout is identical afterwards
    // (drive_c/GOG/<name>, docs/18 §5).
    let install_dir = if let Some(install) = &plan.installer {
        let win_target = format!("C:\\GOG\\{}", plan.slug);
        emit(&ProgressEvent::InstallStarted);
        let installer_bytes = file_size(&install.installer)?
            + install
                .bin_parts
                .iter()
                .filter_map(|p| file_size(p).ok())
                .sum::<u64>();
        installer::run_silent(
            &plan.runtime,
            install,
            &plan.bundle.prefix,
            &win_target,
            &plan.bundle_path,
        )?;
        // Quiesce the prefix before sealing: a lingering wineserver keeps
        // writing into it (registry flushes, temp cleanup) — which broke
        // `codesign --verify` mid-seal (live finding 2026-10-18, "sealed
        // resource missing").
        installer::stop_wineserver(&plan.runtime, &plan.bundle.prefix)?;
        emit(&ProgressEvent::InstallFinished {
            bytes: installer_bytes,
        });
        plan.bundle.prefix.join("drive_c/GOG").join(&plan.slug)
    } else {
        let tree = plan.game_tree.as_ref().expect("one source asserted");
        let name = tree
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| plan.slug.clone());
        emit(&ProgressEvent::InstallStarted);
        let dst = match &plan.state {
            // Design 16 decision 4: `game/` is the clone, reached from the
            // prefix as `C:\GOG\<name>` through a relative symlink.
            Some(dirs) => {
                if dirs.game.symlink_metadata().is_ok() {
                    std::fs::remove_dir_all(&dirs.game)?;
                }
                bundle::clone_tree(tree, &dirs.game)?;
                setup::link_game(dirs, &name)?
            }
            None => {
                let dst = plan.bundle.prefix.join("drive_c/GOG").join(&name);
                bundle::clone_tree(tree, &dst)?;
                dst
            }
        };
        // GOG's support overlay (design 15 §3): files Galaxy merges into the
        // install dir, missing from the depot manifest itself.
        for overlay in locate_support_overlays(tree) {
            let merged = bundle::merge_overlay(&overlay, &dst)?;
            if !merged.is_empty() {
                let mut names: Vec<String> = merged
                    .iter()
                    .take(5)
                    .map(|p| p.display().to_string())
                    .collect();
                if merged.len() > names.len() {
                    names.push("…".into());
                }
                emit(&ProgressEvent::Warn {
                    message: format!(
                        "merged GOG support overlay into the game dir: {} file(s) ({})",
                        merged.len(),
                        names.join(", ")
                    ),
                });
            }
        }
        // D3 amendment (2026-10-20): GOG's support installer writes the
        // startup config + registry into the clone. Fatal when present but
        // failing — without it the game stalls at its first media check.
        if let Some(support) = locate_support_installer(tree) {
            // Bootstrap the registry prerequisite the support installer checks
            // (the offline lane's main installer normally writes it).
            if let Some(id) = info::gog_game_ids(tree).first() {
                let entry = info::find_entry_point(tree).ok();
                let exe_file = entry
                    .as_ref()
                    .and_then(|e| e.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .unwrap_or_else(|| "game.exe".to_string());
                let win_path = format!("C:\\GOG\\{name}");
                write_gog_game_key(
                    &bundled_wine,
                    &plan.bundle.prefix,
                    id,
                    &name,
                    &win_path,
                    &exe_file,
                )?;
            }
            emit(&ProgressEvent::Warn {
                message: "running GOG support installer into the clone".into(),
            });
            let support_plan = installer::plan(&support)?;
            let win_target = format!("C:\\GOG\\{name}");
            // Best-effort (docs/18 §5): this Inno build's InitializeSetup
            // declines even against a fully-synthesized GOG registry key and
            // under two independent wines — the gate is internal GOG script
            // logic. Failing loudly-but-softly keeps self-contained games
            // perfect while flagging the install-config class.
            if installer::run_silent(
                &plan.runtime,
                &support_plan,
                &plan.bundle.prefix,
                &win_target,
                &plan.bundle_path,
            )
            .is_err()
            {
                emit(&ProgressEvent::Warn {
                    message: "GOG support installer declined (install-time config \
                              not written) — if the game stalls at launch, re-run \
                              mac-app with the offline installer lane"
                        .into(),
                });
            }
        }
        installer::stop_wineserver(&plan.runtime, &plan.bundle.prefix)?;
        emit(&ProgressEvent::InstallFinished {
            bytes: plan.game_tree_bytes,
        });
        dst
    };

    // Recipe phase (design 11 §5): live search + selection + steps. A
    // search-discovered recipe is an advisory enhancement — a hard failure
    // after the game installed downgrades to a loud warn + recipe-less
    // continuation instead of aborting into a launcher-less half-bundle
    // (live finding 2026-10-19: "damaged or incomplete" .app). Pinned
    // recipes stay fatal (design 11 §7).
    let pinned = plan.recipe.as_ref().is_some_and(|sel| !sel.from_search);
    let recipe = match run_recipe_phase(plan, &bundled_wine, &install_dir, emit) {
        Ok(outcome) => outcome,
        Err(e) if !pinned => {
            let slug = plan
                .recipe
                .as_ref()
                .map(|sel| sel.installer.slug.clone())
                .unwrap_or_default();
            emit(&ProgressEvent::Warn {
                message: format!(
                    "recipe {slug} failed: {e} — continuing without it \
                     (game installed, community patches not applied)"
                ),
            });
            None
        }
        Err(e) => return Err(e),
    };
    let entry = match &recipe {
        Some(outcome) if outcome.exe.is_some() => outcome.exe.clone().unwrap(),
        _ => info::find_entry_point(&install_dir)?,
    };
    let (recipe_env, recipe_args) = recipe
        .as_ref()
        .map(|o| (o.env.clone(), o.args.clone()))
        .unwrap_or_default();

    // The renderer (#70): the depot lane chose it at plan time; the
    // installer lane scans what it just installed, once, for the renderer
    // and the verbs (#72).
    let lane_scan = plan
        .renderer
        .is_none()
        .then(|| pe::scan_exe(&install_dir, &entry));
    let selection = match (&plan.renderer, &lane_scan) {
        (Some(sel), _) => sel.clone(),
        (None, scan) => {
            let scan = scan.as_ref().expect("scanned when not planned");
            let sel = renderer::select(
                info::root_game_id(&install_dir).as_deref(),
                renderer::pick(scan),
                plan.renderer_choice,
            );
            emit_selection(&sel, emit);
            sel
        }
    };

    // Fat bundles apply cnc-ddraw at build (design 13 D3/D4), after the
    // recipe phase so a recipe's own wrapper wins. v2 bundles leave every
    // renderer to setup, which runs at the end of the build and again
    // whenever the config changes.
    let wanted = renderer::RendererRef::parse(&selection.renderer.value)
        .map(|r| r.renderer)
        .unwrap_or(Renderer::Wined3d);
    if plan.state.is_none() && wanted == Renderer::CncDdraw {
        match ddraw::resolve_or_provision(emit) {
            Some(tree) => match ddraw::apply(&tree, &install_dir)? {
                ddraw::Applied::Applied { shaders } => {
                    // The load-order half (live finding 2026-10-19): without
                    // `ddraw=n,b` wine prefers its builtin and the game-dir
                    // dll is silently ignored.
                    ddraw::register_override(&bundled_wine, &plan.bundle.prefix)?;
                    emit(&ProgressEvent::RecipeStep {
                        verb: format!("cnc-ddraw (dll + ini + override, {shaders} shaders)"),
                    });
                    bundle::write_ddraw_notice(&plan.bundle)?;
                }
                ddraw::Applied::AlreadyPresent => emit(&ProgressEvent::Warn {
                    message: "ddraw.dll already present (recipe-placed) — gogo wrapper \
                              skipped"
                        .into(),
                }),
            },
            None => emit(&ProgressEvent::Warn {
                message: "cnc-ddraw unavailable (provisioning failed) — DirectDraw games \
                          may black-screen"
                    .into(),
            }),
        }
    } else if plan.state.is_none() && matches!(wanted, Renderer::Dxmt | Renderer::D3dMetal) {
        emit(&ProgressEvent::Warn {
            message: format!(
                "{} needs a derived store runtime, which a self-contained bundle can't \
                 carry: using wined3d",
                wanted.id()
            ),
        });
    }

    if let Some(dirs) = &plan.state {
        let winetricks_used = recipe.as_ref().is_some_and(|o| o.winetricks_used);
        return finish_v2(
            plan,
            dirs,
            &install_dir,
            &entry,
            (&recipe_env, &recipe_args),
            (&selection, lane_scan.as_ref()),
            winetricks_used,
            codesign,
            emit,
        );
    }

    bundle::write_launch_conf(
        &plan.bundle,
        &bundled_wine,
        &entry,
        &recipe_env,
        &recipe_args,
    )?;
    bundle::write_launcher(&plan.bundle, &plan.display)?;
    bundle::write_info_plist(
        &plan.bundle,
        &plan.display,
        &plan.slug,
        plan.runtime.min_macos,
    )?;
    bundle::write_notices(&plan.bundle, &plan.runtime)?;
    if recipe.as_ref().is_some_and(|o| o.winetricks_used) {
        bundle::write_winetricks_notice(&plan.bundle)?;
    }
    bundle::write_runtime_marker(&plan.bundle, &plan.runtime)?;
    emit(&ProgressEvent::BundleAssembled);

    // Seal-blocking metadata must go before signing (live finding 2026-10-18:
    // GPTK files carry FinderInfo/resource forks from the quarantined app).
    bundle::strip_detritus(&plan.bundle.root, codesign)?;
    bundle::codesign_adhoc(&plan.bundle.root, codesign)?;
    emit(&ProgressEvent::Codesigned);

    dir_size(&plan.bundle.root)
}

/// The v2 tail of [`execute`] (design 16): settle the prefix in
/// `games/<gogid>/`, write the launcher bundle (launcher, TOML + mirror,
/// plist, notices; nothing else), sign it and verify it strictly, then run
/// the same setup the launcher runs when the config is stale.
#[allow(clippy::too_many_arguments)]
fn finish_v2(
    plan: &MacAppPlan,
    dirs: &launch::GameDirs,
    install_dir: &Path,
    entry: &Path,
    (recipe_env, recipe_args): (&str, &str),
    (selection, lane_scan): (
        &renderer::Selection,
        Option<&Result<pe::PeScan, pe::ScanError>>,
    ),
    winetricks_used: bool,
    codesign: CodesignPolicy,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<u64, MacAppError> {
    // Build steps ran unix-path installers through z:; nothing needs it now,
    // nor the builder's other volumes (decision 4).
    installer::stop_wineserver(&plan.runtime, &dirs.prefix)?;
    setup::localize(&dirs.prefix)?;

    let exe = launch::unix_to_windows(&dirs.prefix, entry).ok_or_else(|| {
        MacAppError::EntryPointNotFound {
            dir: install_dir.to_path_buf(),
        }
    })?;
    let gogid = match &plan.gogid {
        Some(id) => id.clone(),
        None => info::root_game_id(install_dir).ok_or_else(|| MacAppError::EntryPointNotFound {
            dir: install_dir.to_path_buf(),
        })?,
    };
    // Installer lane: the id is known only now; move the staging dir into
    // place. An older prefix for the same game is set aside, never deleted.
    if plan.gogid.is_none() {
        let target = launch::GameDirs::for_game(&gogid)?;
        std::fs::create_dir_all(&target.root)?;
        if target.prefix.symlink_metadata().is_ok() {
            let aside = target.root.join(format!(
                "prefix.old-{}",
                chrono::Utc::now().format("%Y%m%dT%H%M%S")
            ));
            std::fs::rename(&target.prefix, &aside)?;
            emit(&ProgressEvent::Warn {
                message: format!("previous prefix moved to {}", aside.display()),
            });
        }
        std::fs::rename(&dirs.prefix, &target.prefix)?;
        std::fs::remove_dir_all(&dirs.root).ok();
    }

    let workdir = exe
        .rsplit_once('\\')
        .map(|(head, _)| head.to_string())
        .unwrap_or_else(|| exe.clone());
    // Renderer overrides (`ddraw=n,b`) are setup's: they follow the
    // effective renderer, so an L4 switch takes them away again.
    let dll_overrides = std::collections::BTreeMap::new();
    let toml = launch::LaunchToml {
        schema: launch::SCHEMA,
        gogid: gogid.clone(),
        title: plan.display.clone(),
        runtime: launch::runtime_ref(&plan.runtime),
        renderer: selection.renderer.value.clone(),
        exe,
        workdir,
        args: recipe_args.split_whitespace().map(str::to_string).collect(),
        env: recipe_env
            .split_whitespace()
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        dll_overrides,
        // L1 only; setup layers L3 and L4 over it (verbs::effective).
        verbs: verbs::names(&plan.auto_verbs.clone().unwrap_or_else(|| match lane_scan {
            Some(scan) => auto_verbs(scan, emit),
            None => auto_verbs(&pe::scan(install_dir), emit),
        })),
        gogo: launch::GogoRef {
            path: plan.gogo_path.clone(),
            state: crate::state::state_dir(),
        },
    };
    launch::write_bundle_config(&plan.bundle.resources, &toml)?;
    launch::write_launcher(&plan.bundle.macos, &plan.display)?;
    bundle::write_info_plist(
        &plan.bundle,
        &plan.display,
        &plan.slug,
        plan.runtime.min_macos,
    )?;
    bundle::write_launcher_notices(&plan.bundle, &plan.runtime)?;
    bundle::write_renderer_notice(&plan.bundle, &selection.renderer.value)?;
    if winetricks_used {
        bundle::write_winetricks_notice(&plan.bundle)?;
    }
    emit(&ProgressEvent::BundleAssembled);

    // Signed last; nothing writes into the bundle after this.
    bundle::strip_detritus(&plan.bundle.root, codesign)?;
    bundle::codesign_and_verify(&plan.bundle.root, codesign, true)?;
    emit(&ProgressEvent::Codesigned);

    let outcome = setup::setup(&gogid, Some(&plan.bundle.root))?;
    for verb in outcome.notes {
        emit(&ProgressEvent::RecipeStep { verb });
    }
    for message in outcome.warnings {
        emit(&ProgressEvent::Warn { message });
    }
    dir_size(&plan.bundle.root)
}

fn file_size(p: &Path) -> Result<u64, MacAppError> {
    Ok(std::fs::metadata(p)?.len())
}

fn dir_size(p: &Path) -> Result<u64, MacAppError> {
    let mut total = 0u64;
    if p.is_file() {
        return Ok(std::fs::metadata(p)?.len());
    }
    for entry in std::fs::read_dir(p)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            total += dir_size(&entry.path())?;
        } else {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

/// Launch-config adjustments a recipe applies (design 11 §3.1 `game:`/`system:`).
pub struct RecipeOutcome {
    /// Entry point override — a Windows path translated into the prefix.
    pub exe: Option<PathBuf>,
    /// Extra launcher env (`K=V` pairs, space-joined; values must not contain
    /// spaces — game env flags like `WINEESYNC=1` are the target case).
    pub env: String,
    /// Extra launcher args (word-split by the stub).
    pub args: String,
    /// Whether any recipe verb ran through the winetricks chain (design 12
    /// §8 — drives the bundle's LGPL notice).
    pub winetricks_used: bool,
}

/// The recipe phase (design 11 §5, restructured by design 12 §6): the script
/// was selected at plan time; this applies it post-install — `files:` into a
/// per-run temp working set, then the planned steps. The authoritative gogid
/// cross-check runs here, against the pre-selected script.
fn run_recipe_phase(
    plan: &MacAppPlan,
    bundled_wine: &Path,
    install_dir: &Path,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<Option<RecipeOutcome>, MacAppError> {
    use std::time::Duration;

    let Some(sel) = &plan.recipe else {
        return Ok(None);
    };
    let inst = &sel.installer;

    // Cross-check (design 11 §2, moved post-install by design 12 §6): the
    // pre-selected script's gogid must agree with the installed game's ids.
    // Search-discovered scripts only — explicit slugs/files are trusted as
    // pinned. Mismatch → RecipeMiss warn + recipe-less continuation.
    if sel.from_search {
        let ids = info::gog_game_ids(install_dir);
        if let Some(gogid) = inst
            .gogid
            .as_deref()
            .filter(|g| !ids.is_empty() && !ids.iter().any(|i| i == g))
        {
            emit(&ProgressEvent::RecipeMiss {
                key: format!("gogid {gogid} ≠ installed {}", ids.join(",")),
            });
            return Ok(None);
        }
    }

    let client =
        lutris::LutrisClient::new(plan.lutris_base.as_deref().unwrap_or(lutris::DEFAULT_BASE))?;

    // Per-run temp working set (design 11 §4): removed at the end, whatever
    // the outcome.
    let cache = std::env::temp_dir().join(format!("gogo-recipe-{}", std::process::id()));
    let result = (|| -> Result<RecipeOutcome, MacAppError> {
        // prefix user dir ($USER): first entry of drive_c/users minus Public
        let users = plan.bundle.prefix.join("drive_c/users");
        let user = std::fs::read_dir(&users)
            .ok()
            .and_then(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .find(|n| n != "Public" && n != "All Users")
            })
            .unwrap_or_else(|| "gogo".into());

        let ctx = interpret::RecipeCtx {
            wine: bundled_wine.to_path_buf(),
            prefix: plan.bundle.prefix.clone(),
            install_dir: install_dir.to_path_buf(),
            bundle: plan.bundle.root.clone(),
            cache: cache.clone(),
            user,
            width: 1920,
            height: 1080,
            vars: inst
                .script
                .get("variables")
                .and_then(Value::as_object)
                .map(|o| {
                    o.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                        .collect()
                })
                .unwrap_or_default(),
        };

        // files: downloads precede steps (design 11 §3.1)
        for (id, url) in interpret::parse_files(&inst.script) {
            if url.starts_with("N/A:") {
                emit(&ProgressEvent::RecipeSkipped {
                    verb: format!("files:{id}"),
                    reason: "user-provided file (interactive prompt) not supported".into(),
                });
                continue;
            }
            let dst = cache.join(&id);
            let bytes = lutris::block_on_detached(client.download(&url, &dst))
                .map_err(|e| MacAppError::Recipe(format!("files:{id}: {e}")))?;
            emit(&ProgressEvent::RecipeStep {
                verb: format!("files:{id} ({bytes} B)"),
            });
        }

        let script_plan = interpret::plan(&inst.script, &ctx);
        for (_, step) in &script_plan.steps {
            let verb = match step {
                interpret::Step::HarvestInstallerArgs { .. } => "harvest-installer-args",
                interpret::Step::Wineexec { .. } => "wineexec",
                interpret::Step::Winetricks { verbs } => {
                    emit(&ProgressEvent::RecipeStep {
                        verb: format!("winetricks:{}", verbs.join(",")),
                    });
                    continue;
                }
                interpret::Step::Download { .. } => continue,
                interpret::Step::Extract { .. } => "extract",
                interpret::Step::Merge { .. } => "merge",
                interpret::Step::Move { .. } => "move",
                interpret::Step::WriteFile { .. } => "write_file",
                interpret::Step::WriteConfig { .. } => "write_config",
                interpret::Step::Chmodx { .. } => "chmodx",
                interpret::Step::Mkdir { .. } => "mkdir",
                interpret::Step::Registry { .. } => "registry",
                interpret::Step::Launch { .. } => "launch-config",
            };
            emit(&ProgressEvent::RecipeStep { verb: verb.into() });
        }
        for skipped in &script_plan.skipped {
            emit(&ProgressEvent::RecipeSkipped {
                verb: skipped.verb.clone(),
                reason: skipped.reason.clone(),
            });
        }

        let mut run_wine = |argv: &[String]| {
            interpret::run_in_prefix(
                bundled_wine,
                &plan.bundle.prefix,
                argv,
                Duration::from_secs(600),
            )
        };

        // Tracks whether any verb ran through the winetricks chain (notice).
        let mut winetricks_used = false;
        let mut run_winetricks = |verbs: &[String]| -> Result<bool, MacAppError> {
            // Preference chain (design 12 §8): vendored/auto-provisioned →
            // WINETRICKS/PATH override → skip-loud. The bundled wine drives
            // the script; extractor shims are PATH-scoped to this child only.
            let Some(wt) = tools::resolve_or_provision_winetricks(&mut |ev: &ProgressEvent| {
                emit(ev);
            }) else {
                emit(&ProgressEvent::RecipeSkipped {
                    verb: "winetricks".into(),
                    reason: format!(
                        "requires winetricks: {} (auto-provision failed; set WINETRICKS or \
                         `brew install winetricks`)",
                        verbs.join(", ")
                    ),
                });
                return Ok(false);
            };
            let shims_path = tools::path_with_shims()?;
            let mut cmd = runtime::wine_cmd(&wt, bundled_wine);
            cmd.arg("-q")
                .args(verbs)
                .env("WINE", bundled_wine)
                .env("WINEPREFIX", &plan.bundle.prefix)
                .env("PATH", shims_path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            winetricks_used = true;
            match installer::run_with_timeout(&mut cmd, Duration::from_secs(1800)) {
                Some(s) if s.success() => Ok(true),
                Some(s) => Err(MacAppError::Recipe(format!("winetricks exited {s}"))),
                None => Err(MacAppError::Recipe("winetricks timed out".into())),
            }
        };

        // Hoisted: the outcome is built AFTER the executor runs, so the
        // winetricks-used flag is final (the closure borrow ends with it).
        let exe = script_plan
            .launch_exe
            .as_ref()
            .map(|w| interpret::windows_to_posix(&plan.bundle.prefix, w));
        let env = script_plan
            .launch_env
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ");
        let args = script_plan.launch_args.join(" ");

        interpret::Executor {
            ctx: &ctx,
            run_wine: &mut run_wine,
            run_winetricks: &mut run_winetricks,
        }
        .run(&script_plan)?;

        let outcome = RecipeOutcome {
            exe,
            env,
            args,
            winetricks_used,
        };

        Ok(outcome)
    })();

    std::fs::remove_dir_all(&cache).ok();
    result.map(Some)
}

#[cfg(test)]
mod pipeline_tests {
    use super::*;

    #[test]
    fn display_name_from_installer_stem() {
        assert_eq!(
            display_name(Path::new("/x/setup_heroes3_gold.exe"), None),
            "Heroes3 gold"
        );
        assert_eq!(display_name(Path::new("/x/game-v2.exe"), None), "Game v2");
        // GOG version tails (dotted, multi-part) are stripped — the display
        // name feeds both the bundle title and the Lutris search query.
        assert_eq!(
            display_name(Path::new("/x/setup_pharaoh_gold_2.1.0.15.exe"), None),
            "Pharaoh gold"
        );
        // Titles with plain trailing numbers keep them (no dotted tail).
        assert_eq!(
            display_name(Path::new("/x/setup_portal_2_2.1.0.4.exe"), None),
            "Portal 2"
        );
        assert_eq!(
            display_name(Path::new("/x/setup_bejeweled_3_1.0.exe"), None),
            "Bejeweled 3"
        );
        assert_eq!(
            display_name(Path::new("/x/setup_alpha.exe"), Some("  My Name ")),
            "My Name"
        );
    }

    #[test]
    fn slug_rules() {
        assert_eq!(slugify("Heroes3 gold"), "heroes3-gold");
        assert_eq!(slugify("A  B!!"), "a-b");
        assert_eq!(slugify("--Weird--name--"), "weird-name");
        assert_eq!(slugify("Ünïcode"), "n-code"); // non-ASCII folds to a separator
    }
}
