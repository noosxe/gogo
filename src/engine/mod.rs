//! plan() → execute() orchestration; owns the event channel (`docs/design/01` §1/§4,
//! pipeline in `docs/design/04` §1).

pub mod chunk;
pub mod events;
pub mod limiter;
pub mod select;
pub mod sidecar;
pub mod task;
pub mod worker;

/// Build the work list: filters (os/language/kinds/dlcs) over catalog → `FileTask`s,
/// preflight (disk space via fs4, skip/adopt logic per `docs/design/04` §8).
pub async fn plan(
    _ctx: &std::sync::Arc<crate::app::AppContext>,
) -> anyhow::Result<Vec<task::FileTask>> {
    todo!("docs/design/04 §1 (select → resolve), events: PlanReady")
}

/// Final tally, mirrored by the `Done` event (`docs/design/01` §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub ok: u32,
    pub failed: u32,
    pub skipped: u32,
}

/// Run the pipeline for the planned tasks: sequential files, N chunk workers each,
/// per-chunk md5, sidecar updates, atomic rename, InstallRecord write.
pub async fn execute(
    _ctx: &std::sync::Arc<crate::app::AppContext>,
    _tasks: Vec<task::FileTask>,
) -> anyhow::Result<Summary> {
    todo!("docs/design/04 §4–§7; cancellation → flush sidecars, exit 130 (03 §5)")
}
