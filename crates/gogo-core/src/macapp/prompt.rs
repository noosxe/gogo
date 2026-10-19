//! Prompt-spec contract (design 12 §9): core describes **what** to ask, the
//! CLI renders it (dialoguer in `gogo-cli/src/prompt.rs`), tests inject
//! fakes. No UI dependencies live here — dialoguer never leaks into
//! gogo-core.
//!
//! Contract details:
//! - `items` are pre-styled real choices; the `allow_other`/`allow_none`
//!   flags ask the renderer to append its own trailing "other…"/"none"
//!   entries (dialoguer items), reported back as
//!   [`PromptAnswer::Other`]/[`PromptAnswer::NoneSelected`] — never as
//!   indexes into `items`.
//! - Renderers must target **stderr** so NDJSON on stdout stays clean, and
//!   treat Ctrl-C/EOF as a hard cancel (exit 2, never a silent default).

/// Which widget the renderer should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// Type-to-filter list (recipe picker).
    Fuzzy,
    /// Plain arrow-key list (runtime menu).
    Select,
    /// Free-text entry (custom path/slug with re-prompt on invalid input).
    Input,
    /// Yes/no (GPTK EULA consent, final confirmation).
    Confirm { default: bool },
}

/// One question from the core to the renderer.
#[derive(Debug, Clone)]
pub struct PromptSpec {
    pub kind: PromptKind,
    pub title: String,
    /// Pre-styled item lines (no trailing other/none entries).
    pub items: Vec<String>,
    /// Offer an "other…" free-text entry (caller resolves the text).
    pub allow_other: bool,
    /// Offer a "none" entry (skip / opt out).
    pub allow_none: bool,
    /// Preselected entry (rendered as the "easy" default by Select/Fuzzy
    /// renderers). Advisory: Input/Confirm kinds ignore it.
    pub default: usize,
}

/// The renderer's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptAnswer {
    /// An index into `spec.items`.
    Index(usize),
    /// Free text from the "other…" entry.
    Other(String),
    /// The "none" entry was chosen.
    NoneSelected,
    /// A `Confirm` answer.
    Confirmed(bool),
}

/// The injected renderer. Returning `None` cancels the operation.
pub type PromptFn = dyn Fn(&PromptSpec) -> Option<PromptAnswer> + Send + Sync;
