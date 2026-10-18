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
pub mod runtime;

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
    let entry = info::find_entry_point(&install_dir)?;

    bundle::write_launch_conf(&plan.bundle, &bundled_wine, &entry)?;
    bundle::write_launcher(&plan.bundle, &plan.display)?;
    bundle::write_info_plist(
        &plan.bundle,
        &plan.display,
        &plan.slug,
        plan.runtime.min_macos,
    )?;
    bundle::write_notices(&plan.bundle, &plan.runtime)?;
    bundle::write_runtime_marker(&plan.bundle, &plan.runtime)?;
    emit(&ProgressEvent::BundleAssembled);

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
