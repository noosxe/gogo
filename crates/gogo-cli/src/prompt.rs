//! dialoguer-backed [`PromptFn`] renderer (design 12 §9).
//!
//! - Renders on **stderr** (dialoguer's default terminal) so NDJSON on
//!   stdout stays clean under `--json-progress`.
//! - `allow_other`/`allow_none` append trailing renderer-owned entries;
//!   choosing them is reported as [`PromptAnswer::Other`] /
//!   [`PromptAnswer::NoneSelected`], never as indexes into `spec.items`.
//! - Ctrl-C / EOF: print a one-liner and exit 2 — never a silent default.

use gogo::macapp::prompt::{PromptAnswer, PromptFn, PromptKind, PromptSpec};

const OTHER_ENTRY: &str = "custom…";
const NONE_ENTRY: &str = "none";

/// Install the renderer: an `Arc<PromptFn>` handed to `macapp::plan`.
pub fn renderer() -> std::sync::Arc<PromptFn> {
    std::sync::Arc::new(render)
}

fn render(spec: &PromptSpec) -> Option<PromptAnswer> {
    use dialoguer::{Confirm, FuzzySelect, Input, Select};

    // dialoguer renders the prompt (the spec title) itself on stderr —
    // printing it here too duplicates every question line.
    match spec.kind {
        PromptKind::Fuzzy | PromptKind::Select => {
            let mut items = spec.items.clone();
            if spec.allow_other {
                items.push(OTHER_ENTRY.into());
            }
            if spec.allow_none {
                items.push(NONE_ENTRY.into());
            }
            let real_len = spec.items.len();
            let picked = if spec.kind == PromptKind::Fuzzy {
                FuzzySelect::new()
                    .items(&items)
                    .default(spec.default)
                    .interact_opt()
            } else {
                Select::new()
                    .items(&items)
                    .default(spec.default)
                    .interact_opt()
            };
            match picked.unwrap_or_else(|e| cancel(&e)) {
                None => cancel("cancelled"),
                Some(i) if i < real_len => Some(PromptAnswer::Index(i)),
                Some(i) if items.get(i).is_some_and(|s| s == OTHER_ENTRY) => Some(read_other(spec)),
                Some(_) => Some(PromptAnswer::NoneSelected),
            }
        }
        PromptKind::Input => match Input::<String>::new()
            .with_prompt(&spec.title)
            .interact_text()
        {
            Ok(t) => Some(PromptAnswer::Other(t)),
            Err(e) => cancel(&e),
        },
        PromptKind::Confirm { default } => match Confirm::new()
            .with_prompt(&spec.title)
            .default(default)
            .interact()
        {
            Ok(b) => Some(PromptAnswer::Confirmed(b)),
            Err(e) => cancel(&e),
        },
    }
}

/// The "other…" entry: free text (slug / path).
fn read_other(spec: &PromptSpec) -> PromptAnswer {
    let hint = if spec.title.contains("recipe") {
        "Lutris installer slug or script path"
    } else {
        "path"
    };
    dialoguer::Input::<String>::new()
        .with_prompt(hint)
        .interact_text()
        .map(PromptAnswer::Other)
        .unwrap_or_else(|e| cancel(&e))
}

/// Ctrl-C / EOF / Esc (and render errors): exit 2, never a silent default.
fn cancel(e: impl std::fmt::Display) -> ! {
    eprintln!("gogo: prompt cancelled ({e})");
    std::process::exit(crate::cli::exit::USAGE);
}
