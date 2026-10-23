//! `MacAppError` taxonomy (`docs/design/10` §4): every variant maps to exit 7
//! except [`MacAppError::UnsupportedPlatform`], which is a usage error (exit 2,
//! `07` §2 normative table).

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MacAppError {
    /// Recipe-consumption failure (design 11): discovery/selection issues in
    /// pinned mode, applied-step failures, unfetchable pinned slugs.
    #[error("recipe: {0}")]
    Recipe(String),
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

    /// Runtime catalog probe/discovery failure (design 12 §3): API unreachable
    /// after retries, unparseable, or entry not downloadable.
    #[error("runtime catalog unreachable: {detail}")]
    CatalogUnreachable { detail: String },

    #[error("runtime download failed: {url}: {reason}")]
    RuntimeDownloadFailed { url: String, reason: String },

    #[error(
        "runtime {id} checksum mismatch — expected sha256:{expected}, got sha256:{actual}; \
         the archive was discarded (retry, or pin a version with --runtime-version)"
    )]
    RuntimeChecksumMismatch {
        id: String,
        expected: String,
        actual: String,
    },

    /// A compat DB / user override file (`compat.rs`) that can't be used.
    #[error("compat file {path}: {reason}")]
    Compat { path: PathBuf, reason: String },

    /// `--portable` (#84): what a self-contained bundle can't carry or do.
    #[error("--portable: {0}")]
    Portable(String),

    /// A runtime or renderer of the wrong architecture for this Mac or for
    /// each other (#82).
    #[error("{0}")]
    Arch(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
