//! `MacAppError` taxonomy (`docs/design/10` §4): every variant maps to exit 7
//! (`07` §2 normative table).

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
        "installer exited {exit} — partial bundle kept at {bundle}; Inno's log \
         ({inno}) and Wine's ({wine}) are in {logs}{last}",
        inno = super::installer::INNO_LOG,
        wine = super::installer::WINE_LOG,
        last = last_lines(.tail)
    )]
    InstallerFailed {
        exit: i32,
        bundle: PathBuf,
        /// `run_silent`'s log dir (#56).
        logs: PathBuf,
        /// The Inno log's last lines ("" without one).
        tail: String,
    },

    #[error(
        "no launchable entry point under {dir}: no goggame-*.info with a runnable \
         file task (.lnk-only tasks are not supported yet)"
    )]
    EntryPointNotFound { dir: PathBuf },

    #[error("{path} already exists — pass --yes to overwrite it")]
    BundleExists { path: PathBuf },

    #[error("codesign failed: {stderr}")]
    Codesign { stderr: String },

    /// Runtime catalog probe/discovery failure (design 12 §3): API unreachable
    /// after retries, unparseable, or entry not downloadable.
    #[error("runtime catalog unreachable: {detail}")]
    CatalogUnreachable { detail: String },

    #[error("download failed: {url}: {reason}")]
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

    /// `gogo apps remove|prune`: a game that can't be removed right now.
    #[error("{0}")]
    Apps(String),

    /// A runtime or renderer of the wrong architecture for this Mac or for
    /// each other (#82).
    #[error("{0}")]
    Arch(String),

    /// umu-launcher can't be provisioned or run as configured (design 18
    /// §2.1): `[linux] umu` unset headless, no usable Python, no `umu-run`.
    #[error("umu: {0}")]
    Umu(String),

    /// A Proton build that can't be placed or used (design 18 §1, §2.2).
    #[error("proton: {0}")]
    Proton(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// The Inno log tail of [`MacAppError::InstallerFailed`], on its own lines.
fn last_lines(tail: &str) -> String {
    if tail.is_empty() {
        String::new()
    } else {
        format!("\nlast lines of the Inno log:\n{tail}")
    }
}
