//! Wine runtime resolution + probe (`docs/design/10` §3).
//!
//! gogo never ships or downloads a runtime (docs/13 §3 licensing): it resolves
//! one from the machine — Homebrew wine-crossover, a user-supplied GPTK mount,
//! or an explicit path — and *probes* it by executing `wine --version` before
//! trusting it. The resolved runtime is copied into the bundle by the pipeline.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use super::error::MacAppError;

/// Where the Wine runtime comes from (`--runtime`, config `mac_app.runtime`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeMode {
    #[default]
    Auto,
    Brew,
    Gptk,
    Path,
}

impl RuntimeMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RuntimeMode::Auto => "auto",
            RuntimeMode::Brew => "brew",
            RuntimeMode::Gptk => "gptk",
            RuntimeMode::Path => "path",
        }
    }
}

/// A probed, runnable Wine runtime.
#[derive(Debug, Clone)]
pub struct Runtime {
    /// Which resolution mode found it (recorded into `Resources/runtime.txt`).
    pub mode: RuntimeMode,
    /// The resolved kind label for events/notices ("brew" / "gptk" / "path").
    pub kind: &'static str,
    /// Root directory that gets copied into the bundle (`Resources/wine/`).
    pub root: PathBuf,
    /// Absolute path of the runnable wine binary.
    pub wine: PathBuf,
    /// First line of `wine --version`, e.g. `wine-9.0`.
    pub version: String,
    /// Minimum macOS for Info.plist `LSMinimumSystemVersion` (design 10 §2.6).
    pub min_macos: Option<&'static str>,
}

/// Resolution inputs (`--runtime` + `--runtime-path`).
#[derive(Debug, Clone, Default)]
pub struct RuntimeOptions {
    pub mode: RuntimeMode,
    pub runtime_path: Option<PathBuf>,
}

/// Resolve + probe a runtime. Pure fs/exec — no network, no config access.
pub fn resolve(opts: &RuntimeOptions) -> Result<Runtime, MacAppError> {
    // A supplied --runtime-path implies path mode (live finding 2026-10-19:
    // silently ignoring it in auto mode produced a brew-runtime bundle).
    let opts = if opts.mode == RuntimeMode::Auto && opts.runtime_path.is_some() {
        RuntimeOptions {
            mode: RuntimeMode::Path,
            runtime_path: opts.runtime_path.clone(),
        }
    } else {
        opts.clone()
    };
    let mut tried: Vec<String> = Vec::new();
    match opts.mode {
        RuntimeMode::Brew => resolve_brew(),
        RuntimeMode::Gptk => {
            resolve_explicit(opts.runtime_path.as_deref(), "gptk", RuntimeMode::Gptk)
        }
        RuntimeMode::Path => {
            resolve_explicit(opts.runtime_path.as_deref(), "path", RuntimeMode::Path)
        }
        RuntimeMode::Auto => {
            match resolve_brew() {
                Ok(rt) => return Ok(rt),
                // Surface WHY brew probing failed, not just that it did.
                Err(MacAppError::RuntimeInvalid { reason, .. }) => {
                    tried.push(format!("brew: {reason}"))
                }
                Err(_) => tried.push("brew wine (not installed)".to_string()),
            }
            for candidate in mounted_gptk_candidates() {
                match probe(candidate.clone(), RuntimeMode::Gptk, "gptk") {
                    Ok(rt) => return Ok(rt),
                    Err(reason) => tried.push(format!("{}: {reason}", candidate.display())),
                }
            }
            tried.push("no mounted GPTK volume under /Volumes".to_string());
            Err(MacAppError::RuntimeNotFound {
                tried: tried.join("; "),
            })
        }
    }
}

/// Homebrew wine-crossover: `$(brew --prefix)/opt/wine`.
fn resolve_brew() -> Result<Runtime, MacAppError> {
    let out =
        Command::new("brew")
            .arg("--prefix")
            .output()
            .map_err(|e| MacAppError::RuntimeInvalid {
                path: PathBuf::from("brew"),
                reason: format!("cannot execute brew: {e}"),
            })?;
    if !out.status.success() {
        return Err(MacAppError::RuntimeInvalid {
            path: PathBuf::from("brew"),
            reason: format!("`brew --prefix` exited {}", out.status),
        });
    }
    let prefix = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
    // Two layouts, probed in order (live finding 2026-10-18, design 10 §3):
    // - legacy `opt/wine` (formula-era layout);
    // - cask-linked binaries in `$(brew --prefix)/bin` — e.g. the
    //   `gcenx/wine/game-porting-toolkit` cask (WineHQ casks were disabled
    //   2026-09-01 over Gatekeeper; `wine` cask no longer exists).
    let mut last_reason = String::new();
    for root in [prefix.join("opt/wine"), prefix.clone()] {
        match find_wine_in(&root, RuntimeMode::Brew, "brew") {
            Ok(rt) => return Ok(rt),
            Err(reason) => last_reason = format!("{}: {reason}", root.display()),
        }
    }
    Err(MacAppError::RuntimeInvalid {
        path: prefix,
        reason: last_reason,
    })
}

/// GPTK mount / explicit path: `<dir>/bin/{wine64,wine}` (probed, never assumed).
fn resolve_explicit(
    path: Option<&Path>,
    kind: &'static str,
    mode: RuntimeMode,
) -> Result<Runtime, MacAppError> {
    let root = path.ok_or_else(|| MacAppError::RuntimeNotFound {
        tried: format!(
            "{kind} selected but no --runtime-path given (point it at the mounted GPTK volume or any dir containing bin/wine)"
        ),
    })?;
    find_wine_in(root, mode, kind).map_err(|reason| MacAppError::RuntimeInvalid {
        path: root.to_path_buf(),
        reason,
    })
}

/// Locate + probe a wine binary under `root/bin/`. Prefer `wine` (modern
/// single-binary WoW64 builds), fall back to `wine64`.
fn find_wine_in(root: &Path, mode: RuntimeMode, kind: &'static str) -> Result<Runtime, String> {
    let bin = root.join("bin");
    for name in ["wine", "wine64"] {
        let wine = bin.join(name);
        if wine.is_file() {
            return probe(wine, mode, kind);
        }
    }
    Err(format!(
        "no bin/wine or bin/wine64 under {}",
        root.display()
    ))
}

/// Probe by executing `wine --version` — a runtime we cannot run is a
/// `RuntimeInvalid`, never a silent assumption (design 10 §2.1).
/// Probe an extracted wine tree by executing `<wine> --version` — used by the
/// runtime store after extraction (design 12 §4): a tree we cannot run is a
/// `RuntimeInvalid`, never a silent assumption. No `RuntimeMode`: store trees
/// are not one of the resolution lanes (Phase 3 wires them in).
pub fn probe_tree(wine: &Path) -> Result<String, MacAppError> {
    let out =
        Command::new(wine)
            .arg("--version")
            .output()
            .map_err(|e| MacAppError::RuntimeInvalid {
                path: wine.to_path_buf(),
                reason: format!("cannot execute: {e}"),
            })?;
    if !out.status.success() {
        return Err(MacAppError::RuntimeInvalid {
            path: wine.to_path_buf(),
            reason: format!("`wine --version` exited {}", out.status),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn probe(wine: PathBuf, mode: RuntimeMode, kind: &'static str) -> Result<Runtime, String> {
    // Resolve symlinks first: cask runtimes install `$(brew --prefix)/bin/wine64`
    // as a symlink into the real tree (e.g. Game Porting Toolkit.app). Deriving
    // `root` from the symlink path bundles the wrong directory entirely — live
    // finding 2026-10-19: brew mode copied all of /opt/homebrew into a bundle.
    let wine = wine.canonicalize().unwrap_or_else(|_| wine.clone());
    let out = Command::new(&wine)
        .arg("--version")
        .output()
        .map_err(|e| format!("cannot execute: {e}"))?;
    if !out.status.success() {
        return Err(format!("--version exited {}", out.status));
    }
    let version = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if version.is_empty() {
        return Err("--version printed nothing".to_string());
    }
    // GPTK 2.x baseline (docs/13 §1; Whisky required Sonoma for the same stack).
    let min_macos = matches!(mode, RuntimeMode::Gptk).then_some("14.0");
    Ok(Runtime {
        mode,
        kind,
        root: wine
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("/")),
        wine,
        version,
        min_macos,
    })
}

/// Mounted GPTK volumes: scan `/Volumes/*` (≤2 levels) for a wine binary.
fn mounted_gptk_candidates() -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(volumes) = std::fs::read_dir("/Volumes") else {
        return found;
    };
    for vol in volumes.flatten() {
        let v = vol.path();
        for dir in [v.clone(), v.join("lib/wine")] {
            for name in ["wine64", "wine"] {
                let candidate = dir.join("bin").join(name);
                if candidate.is_file() {
                    found.push(candidate);
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod runtime_tests {
    use super::*;

    fn fake_runtime(dir: &Path, body: &str) -> PathBuf {
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let wine = bin.join("wine");
        std::fs::write(&wine, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&wine, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        wine
    }

    #[test]
    fn probe_runs_wine_version() {
        let tmp = tempfile::tempdir().unwrap();
        fake_runtime(
            tmp.path(),
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo wine-9.0-test; fi\n",
        );
        let rt = probe(tmp.path().join("bin/wine"), RuntimeMode::Path, "path").unwrap();
        assert_eq!(rt.version, "wine-9.0-test");
        assert_eq!(rt.kind, "path");
        // probe canonicalizes the binary, so root is canonical too (/tmp is
        // a symlink to /private/tmp on macOS).
        assert_eq!(rt.root, tmp.path().canonicalize().unwrap());
        assert_eq!(rt.min_macos, None);
    }

    #[test]
    fn probe_follows_symlinked_binary_to_the_real_tree() {
        // Cask layout: $(brew --prefix)/bin/wine64 is a symlink into the real
        // runtime tree; root must be the real tree (2026-10-19: brew mode
        // bundled /opt/homebrew instead of the wine tree).
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("App.app/Contents/Resources/wine");
        fake_runtime(&real, "#!/bin/sh\necho wine-8.0.1-test\n");
        let link_bin = tmp.path().join("link/bin");
        std::fs::create_dir_all(&link_bin).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(real.join("bin/wine"), link_bin.join("wine")).unwrap();
        let rt = probe(link_bin.join("wine"), RuntimeMode::Brew, "brew").unwrap();
        assert_eq!(rt.root, real.canonicalize().unwrap());
        assert!(rt.wine.starts_with(real.canonicalize().unwrap()));
    }

    #[test]
    fn runtime_path_implies_path_mode_in_auto() {
        let tmp = tempfile::tempdir().unwrap();
        fake_runtime(tmp.path(), "#!/bin/sh\necho wine-8.0.1-test\n");
        let rt = resolve(&RuntimeOptions {
            mode: RuntimeMode::Auto,
            runtime_path: Some(tmp.path().to_path_buf()),
        })
        .unwrap();
        assert_eq!(rt.kind, "path");
        assert_eq!(rt.root, tmp.path().canonicalize().unwrap());
    }

    #[test]
    fn probe_fails_when_version_prints_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        fake_runtime(tmp.path(), "#!/bin/sh\n");
        let err = probe(tmp.path().join("bin/wine"), RuntimeMode::Path, "path").unwrap_err();
        assert!(err.contains("--version printed nothing"), "{err}");
    }

    #[test]
    fn gptk_mode_requires_min_macos() {
        let tmp = tempfile::tempdir().unwrap();
        fake_runtime(tmp.path(), "#!/bin/sh\necho wine-10.0\n");
        let rt = probe(tmp.path().join("bin/wine"), RuntimeMode::Gptk, "gptk").unwrap();
        assert_eq!(rt.min_macos, Some("14.0"));
    }

    #[test]
    fn find_wine_prefers_wine_over_wine64() {
        let tmp = tempfile::tempdir().unwrap();
        fake_runtime(tmp.path(), "#!/bin/sh\necho wine-9\n");
        let bin = tmp.path().join("bin");
        std::fs::write(bin.join("wine64"), "#!/bin/sh\necho wine-8\n").unwrap();
        let rt = find_wine_in(tmp.path(), RuntimeMode::Path, "path").unwrap();
        assert!(rt.wine.ends_with("bin/wine"));
    }

    #[test]
    fn explicit_mode_without_path_names_the_remedy() {
        let err = resolve(&RuntimeOptions {
            mode: RuntimeMode::Gptk,
            runtime_path: None,
        })
        .unwrap_err();
        match err {
            MacAppError::RuntimeNotFound { tried } => {
                assert!(tried.contains("--runtime-path"), "{tried}");
            }
            other => panic!("expected RuntimeNotFound, got {other:?}"),
        }
    }
}
