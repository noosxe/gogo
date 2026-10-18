//! `MacAppError` taxonomy (`docs/design/10` §4): every variant maps to exit 7
//! except [`MacAppError::UnsupportedPlatform`], which is a usage error (exit 2,
//! `07` §2 normative table).

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MacAppError {
    #[error(
        "no usable Wine runtime found (tried: {tried}); remedies: `brew install wine`, \
         or mount the GPTK dmg and pass `--runtime gptk --runtime-path <dir>`, \
         or pass `--runtime path --runtime-path <dir>`"
    )]
    RuntimeNotFound { tried: String },

    #[error("runtime at {path} is not runnable: {reason}")]
    RuntimeInvalid { path: PathBuf, reason: String },

    #[error(
        "installer exited {exit} — partial bundle kept at {bundle} for diagnosis \
         (installer stderr was not silenced; re-run with -v for detail)"
    )]
    InstallerFailed { exit: i32, bundle: PathBuf },

    #[error(
        "no launchable entry point under {dir}: no goggame-*.info with a runnable \
         file task (.lnk-only tasks are not supported yet)"
    )]
    EntryPointNotFound { dir: PathBuf },

    #[error("{path} already exists — pass --yes to overwrite it")]
    BundleExists { path: PathBuf },

    #[error("codesign failed: {stderr}")]
    Codesign { stderr: String },

    #[error(
        "`gogo mac-app` is macOS-only — on Linux, run the installer with Wine \
         (e.g. `WINEPREFIX=… wine setup.exe /VERYSILENT …`) or use Lutris"
    )]
    UnsupportedPlatform,

    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
