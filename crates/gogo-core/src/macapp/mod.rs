//! `gogo mac-app` pipeline (`docs/design/10` §2): resolve runtime → stage
//! prefix → silent install → locate entry point → write launcher + plist →
//! ad-hoc codesign. Synchronous by design — the heavy steps are child
//! processes and fs copies; the CLI runs it on a blocking thread.
//!
//! gogo tracks nothing after conversion (design 10 boundary): no install
//! records, no update detection — the bundle is the artifact.

pub mod bundle;
pub mod error;
pub mod info;
pub mod installer;
pub mod interpret;

use serde_json::Value;
use std::process::Command;
pub mod catalog;
pub mod lutris;
pub mod prompt;
pub mod runtime;
pub mod store;
pub mod tools;

use std::path::{Path, PathBuf};

use crate::engine::events::ProgressEvent;

pub use error::MacAppError;
pub use prompt::{PromptAnswer, PromptFn, PromptKind, PromptSpec};
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
    pub installer: installer::InstallPlan,
    /// Estimated final size: runtime tree + installer + `.bin` parts.
    pub estimated_bytes: u64,
    /// The recipe choice carried to `execute` (resolved post-install).
    /// The recipe selected at plan time (§6 step 2); applied post-install.
    pub recipe: Option<SelectedRecipe>,
    /// `(id, version)` when `plan` downloaded a catalog runtime into the
    /// store (§6 step 3) — shown on the confirmation screen.
    pub runtime_downloaded: Option<(String, String)>,
    /// Lutris API base override (tests; see `MacAppOptions::lutris_base`).
    pub lutris_base: Option<String>,
}

/// Conversion inputs (CLI flags, design 10 §1).
#[derive(Debug, Clone, Default)]
pub struct MacAppOptions {
    pub installer: PathBuf,
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
    let spaced = stem.replace(['_', '-', '.'], " ");
    let spaced = spaced.trim().to_string();
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "Game".to_string(),
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
            let discovery = {
                let client = &client;
                async move {
                    let games: Vec<lutris::LutrisGame> =
                        client.search(display).await.unwrap_or_default();
                    let Some(game) = lutris::pick_game(&games, display) else {
                        return Ok::<Vec<lutris::LutrisInstaller>, MacAppError>(Vec::new());
                    };
                    let mut candidates = client.installers_for(&game.slug).await?;
                    candidates.retain(|i| i.runner == "wine");
                    lutris::rank(&mut candidates);
                    Ok(candidates)
                }
            };
            match lutris::block_on_detached(discovery) {
                Ok(candidates) if candidates.is_empty() => {
                    emit(&ProgressEvent::RecipeMiss {
                        key: display.to_string(),
                    });
                    return Ok(None);
                }
                Ok(candidates) => {
                    // §9: the picker runs only when ambiguous (design 11 §1);
                    // headless runs take the ranked best.
                    if candidates.len() > 1
                        && let Some(p) = prompt
                    {
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
fn parse_script_file(path: &Path) -> Result<lutris::LutrisInstaller, MacAppError> {
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
/// staging default), then brew, then downloads the recommendation into the
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

    // `auto` (§5 ladder): cached(recipe-recommended) → cached(staging) →
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
    if !lanes.contains(&"wine-staging") {
        lanes.push("wine-staging");
    }
    for id in lanes {
        if let Some((m, _)) = store::installed(id, None) {
            match runtime::from_store(&m) {
                Ok(rt) => return Ok((rt, None)),
                Err(e) => emit(&ProgressEvent::Warn {
                    message: format!("cached {id} unusable: {e} — trying next source"),
                }),
            }
        }
    }
    if let Ok(rt) = runtime::resolve(&runtime::RuntimeOptions {
        mode: RuntimeMode::Brew,
        ..Default::default()
    }) {
        return Ok((rt, None));
    }
    // Ladder tail: interactive menu when a renderer is present (§5/§9);
    // headless runs download the recommendation (Phase 3 behavior).
    if let Some(prompt) = prompt {
        return choose_runtime(prompt, emit);
    }
    let id = rec.as_ref().map(|r| r.id).unwrap_or("wine-staging");
    let (m, downloaded) = catalog_lane(id, None, false, emit)?;
    let rt = runtime::from_store(&m)?;
    Ok((rt, downloaded.then(|| (m.id.clone(), m.version.clone()))))
}

/// The §5 runtime menu (interactive ladder tail): cached flavors first
/// (✓ cached), downloadable flavors with arch tags, GPTK behind the D9
/// consent gate (Apple Silicon hosts only — Intel hosts never see it),
/// a custom path lane with re-prompt on invalid input (§9), and cancel.
fn choose_runtime(
    prompt: &prompt::PromptFn,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<(runtime::Runtime, Option<(String, String)>), MacAppError> {
    use prompt::{PromptAnswer, PromptKind, PromptSpec};

    let cancelled = || MacAppError::RuntimeNotFound {
        tried: "runtime selection cancelled".into(),
    };
    let rosetta_missing =
        cfg!(target_arch = "aarch64") && !rosetta_present() && !cfg!(target_os = "linux");

    // Lanes: cached flavors first, then downloadable ones in catalog order.
    let manifest = store::load_manifest();
    let mut cached_lanes: Vec<(&'static catalog::CatalogEntry, Option<store::ManifestEntry>)> =
        Vec::new();
    let mut fresh_lanes: Vec<(&'static catalog::CatalogEntry, Option<store::ManifestEntry>)> =
        Vec::new();
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
        let hit = manifest.iter().find(|m| m.id == entry.id).cloned();
        if hit.is_some() {
            cached_lanes.push((entry, hit));
        } else {
            fresh_lanes.push((entry, None));
        }
    }
    let lanes = [cached_lanes, fresh_lanes].concat();

    let items: Vec<String> = lanes
        .iter()
        .map(|(entry, cached)| match cached {
            Some(m) => format!("{} {} (✓ cached)", entry.id, m.version),
            None => {
                let mut line = format!("download {} — {}", entry.id, entry.arch);
                if entry.id == "wine-staging" {
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
        })
        .collect();

    let spec = PromptSpec {
        kind: PromptKind::Select,
        title: "which wine runtime? (nothing usable cached, no brew wine found)".into(),
        items,
        // "other…" = a custom runtime path (directory containing bin/wine).
        allow_other: true,
        allow_none: false,
    };
    let Some(answer) = prompt(&spec) else {
        return Err(cancelled());
    };
    match answer {
        PromptAnswer::Index(i) => {
            let Some((entry, cached)) = lanes.get(i) else {
                return Err(cancelled());
            };
            if let Some(m) = cached {
                let rt = runtime::from_store(m)?;
                return Ok((rt, None));
            }
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
    let display = display_name(&opts.installer, opts.name.as_deref());
    let slug = slugify(&display);

    // §6 step 2: recipe discovery + selection precede the runtime — a wine
    // pin feeds the recommendation (D2: flag > recipe > default).
    let recipe = select_recipe(opts, &display, prompt, emit)?;

    // §6 step 3: runtime resolution — may download into the store (events).
    let (runtime, runtime_downloaded) = resolve_runtime(opts, recipe.as_ref(), prompt, emit)?;
    let kind = runtime.mode.as_str().to_string();
    let wine = runtime.wine.display().to_string();
    emit(&ProgressEvent::RuntimeResolved { kind, path: wine });

    let install = installer::plan(&opts.installer)?;

    let bundle = bundle::layout(&opts.out_dir, &display);
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

    let estimated_bytes = dir_size(&runtime.root)?
        + file_size(&install.installer)?
        + install
            .bin_parts
            .iter()
            .filter_map(|p| file_size(p).ok())
            .sum::<u64>();

    Ok(MacAppPlan {
        bundle_path: bundle.root.clone(),
        display,
        slug,
        runtime,
        installer: install,
        estimated_bytes,
        recipe,
        runtime_downloaded,
        lutris_base: opts.lutris_base.clone(),
        bundle,
    })
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

    // Runtime copy first: a failing copy must not leave a half-installed
    // prefix behind as the only artifact.
    emit(&ProgressEvent::Warn {
        message: format!(
            "copying runtime {} (~{} MiB) into the bundle",
            plan.runtime.version,
            plan.estimated_bytes / (1024 * 1024)
        ),
    });
    let runtime_bytes = bundle::copy_runtime(&plan.runtime, &plan.bundle.resources.join("wine"))?;
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
    let bundled_wine = plan.bundle.resources.join("wine").join(rel);

    emit(&ProgressEvent::PrefixStaged);
    installer::stage_prefix(&plan.runtime, &plan.bundle.prefix)?;

    let win_target = format!("C:\\GOG\\{}", plan.slug);
    emit(&ProgressEvent::InstallStarted);
    let installer_bytes = file_size(&plan.installer.installer)?
        + plan
            .installer
            .bin_parts
            .iter()
            .filter_map(|p| file_size(p).ok())
            .sum::<u64>();
    installer::run_silent(
        &plan.runtime,
        &plan.installer,
        &plan.bundle.prefix,
        &win_target,
        &plan.bundle_path,
    )?;
    // Quiesce the prefix before sealing: a lingering wineserver keeps writing
    // into it (registry flushes, temp cleanup) — which broke `codesign
    // --verify` mid-seal (live finding 2026-10-18, "sealed resource missing").
    installer::stop_wineserver(&plan.runtime, &plan.bundle.prefix)?;
    emit(&ProgressEvent::InstallFinished {
        bytes: installer_bytes,
    });

    let install_dir = plan.bundle.prefix.join("drive_c/GOG").join(&plan.slug);

    // Recipe phase (design 11 §5): live search + selection + steps.
    let recipe = run_recipe_phase(plan, &bundled_wine, &install_dir, emit)?;
    let entry = match &recipe {
        Some(outcome) if outcome.exe.is_some() => outcome.exe.clone().unwrap(),
        _ => info::find_entry_point(&install_dir)?,
    };
    let (recipe_env, recipe_args) = recipe
        .as_ref()
        .map(|o| (o.env.clone(), o.args.clone()))
        .unwrap_or_default();

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

    let _ = runtime_bytes;
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
            let mut cmd = Command::new(&wt);
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
