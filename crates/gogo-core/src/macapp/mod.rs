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
pub mod runtime;
pub mod store;
pub mod tools;

use std::path::{Path, PathBuf};

use crate::engine::events::ProgressEvent;

pub use error::MacAppError;
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
    pub recipe: RecipeChoice,
}

/// Conversion inputs (CLI flags, design 10 §1).
#[derive(Debug, Clone, Default)]
pub struct MacAppOptions {
    pub installer: PathBuf,
    pub name: Option<String>,
    pub mode: RuntimeMode,
    pub runtime_path: Option<PathBuf>,
    pub out_dir: PathBuf,
    /// Overwrite an existing bundle (CLI `--yes`).
    pub overwrite: bool,
    /// Lutris recipe selection (design 11).
    pub recipe: RecipeChoice,
}

/// Interactive selection callback: one rendered line per candidate, returns
/// the chosen index. Invoked only when >1 candidates (design 11 §1).
pub type SelectFn = dyn Fn(&[String]) -> Option<usize> + Send + Sync;

/// `--recipe auto|none|<installer-slug>|<script-file>` (design 11 §1).
#[derive(Debug, Clone, Default, PartialEq)]
pub enum RecipeChoice {
    /// Live search + rank; the user is asked via `on_select` when ambiguous.
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

/// Resolve everything up-front: names, runtime (probed), installer (validated),
/// sizes. Emits `RuntimeResolved`. No bundle dirs are created yet.
pub fn plan(
    opts: &MacAppOptions,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<MacAppPlan, MacAppError> {
    let display = display_name(&opts.installer, opts.name.as_deref());
    let slug = slugify(&display);

    let runtime = runtime::resolve(&RuntimeOptions {
        mode: opts.mode,
        runtime_path: opts.runtime_path.clone(),
    })?;
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
        recipe: opts.recipe.clone(),
        bundle,
    })
}

/// Run the conversion. `codesign` is [`CodesignPolicy::Adhoc`] on macOS hosts,
/// [`CodesignPolicy::Skip`] elsewhere/tests (design 10 §2.7). Emits the §6
/// event sequence. Returns the final bundle size in bytes.
pub fn execute(
    plan: &MacAppPlan,
    codesign: CodesignPolicy,
    on_select: Option<&SelectFn>,
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
    let recipe = run_recipe_phase(plan, &bundled_wine, &install_dir, on_select, emit)?;
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

/// The recipe phase (design 11 §5): discover + select the script, download
/// its `files:` into a per-run temp working set, plan, and execute the steps.
/// Auto-mode trouble is never fatal: `RecipeMiss` + recipe-less continuation.
fn run_recipe_phase(
    plan: &MacAppPlan,
    bundled_wine: &Path,
    install_dir: &Path,
    on_select: Option<&SelectFn>,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<Option<RecipeOutcome>, MacAppError> {
    use std::time::Duration;

    if matches!(plan.recipe, RecipeChoice::None) {
        return Ok(None);
    }
    let client = lutris::LutrisClient::new(lutris::DEFAULT_BASE)?;

    let inst: lutris::LutrisInstaller = match &plan.recipe {
        RecipeChoice::None => return Ok(None),
        RecipeChoice::Slug(slug) => lutris::block_on_detached(client.installer(slug))?,
        RecipeChoice::File(path) => {
            let v: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(path)
                    .map_err(|e| MacAppError::Recipe(format!("script {}: {e}", path.display())))?,
            )
            .map_err(|e| MacAppError::Recipe(format!("script parse: {e}")))?;
            lutris::LutrisInstaller {
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
            }
        }
        RecipeChoice::Auto => {
            let ids = info::gog_game_ids(install_dir);
            let name = info::game_name(install_dir);
            let discovery = {
                let client = &client;
                let ids = ids.clone();
                async move {
                    let games: Vec<lutris::LutrisGame> = match &name {
                        Some(n) => client.search(n).await.unwrap_or_default(),
                        None => vec![],
                    };
                    let Some(game) = lutris::match_gog_game(&games, &ids).cloned() else {
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
                    emit(&ProgressEvent::RecipeMiss { key: ids.join(",") });
                    return Ok(None);
                }
                Ok(candidates) => {
                    let picked = match (candidates.len(), on_select) {
                        (1, _) => 0,
                        (_, Some(select)) => {
                            let lines: Vec<String> = candidates
                                .iter()
                                .map(|c| format!("{} — {} ({})", c.slug, c.version, c.author))
                                .collect();
                            select(&lines).unwrap_or(0)
                        }
                        _ => 0,
                    };
                    candidates.into_iter().nth(picked)
                }
                Err(e) => {
                    emit(&ProgressEvent::RecipeMiss { key: e.to_string() });
                    return Ok(None);
                }
            }
            .ok_or_else(|| MacAppError::Recipe("selection out of range".into()))?
        }
    };

    emit(&ProgressEvent::RecipeResolved {
        installer_slug: inst.slug.clone(),
        version: inst.version.clone(),
        author: inst.author.clone(),
    });

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
