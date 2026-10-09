//! Filters (os/language/kinds/dlcs) over catalog → `FileTask`s (`docs/design/04` §1).

use crate::engine::task::FileTask;

/// Filter set, built from CLI flags + config defaults.
#[derive(Debug, Clone)]
pub struct Filters {
    /// Default: host OS with fallback to windows (docs/design/06 §1; first-class on
    /// Linux per docs/design/README non-goals — Wine/Proton workflow).
    pub os: Option<crate::gog::catalog::Os>,
    pub language: Option<String>,
    pub kinds: Vec<crate::engine::task::Kind>,
    pub with_dlcs: bool,
}

/// Reduce a product's `downloads` to concrete file tasks (skips recorded as `Warn`).
pub fn for_product(_product: &crate::gog::catalog::Product, _filters: &Filters) -> Vec<FileTask> {
    todo!("docs/design/04 §1 — os/language/kinds matching, multi-part expansion")
}
