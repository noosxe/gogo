//! Engine event channel (`docs/design/01` §4).
//!
//! The engine talks to frontends **only** through typed events — the v2 TUI consumes the
//! same stream the CLI renders. Transport: `tokio::sync::broadcast`; `--json-progress`
//! prints the same events as NDJSON on stdout (docs/design/03` §4).
//!
//! Review D2: `ChunkDone` carries `file_bytes` (cumulative per-file progress) so a
//! consumer that missed earlier events — or hit broadcast `Lagged` — can recompute its
//! bar position from any single event instead of accumulating deltas.

use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize)]
pub enum ProgressEvent {
    PlanReady {
        files: u32,
        total_bytes: u64,
        skipped_bytes: u64,
    },
    FileStart {
        id: Uuid,
        name: String,
        total_bytes: u64,
    },
    ChunkDone {
        id: Uuid,
        chunk: u32,
        bytes: u32,
        /// Cumulative bytes on disk for this file (D2 lag self-heal).
        file_bytes: u64,
        md5_ok: bool,
    },
    FileDone {
        id: Uuid,
        path: std::path::PathBuf,
    },
    FileSkipped {
        id: Uuid,
        reason: String,
    },
    FileFailed {
        id: Uuid,
        error: String,
    },
    /// e.g. "bonus content 403 → skipped (bundled)".
    Warn {
        message: String,
    },
    Done {
        ok: u32,
        failed: u32,
        skipped: u32,
    },
    /// `mac-app`: Wine runtime resolved (design 10 §6).
    RuntimeResolved {
        /// "brew" | "gptk" | "path".
        kind: String,
        path: String,
    },
    /// `mac-app`: WINEPREFIX initialized inside the bundle.
    PrefixStaged,
    /// `mac-app`: silent installer started.
    InstallStarted,
    /// `mac-app`: silent installer finished.
    InstallFinished {
        bytes: u64,
    },
    /// `mac-app`: launcher + plist + notices written.
    BundleAssembled,
    /// `mac-app`: ad-hoc codesign done.
    Codesigned,
    /// `mac-app --recipe`: a Lutris script was resolved (design 11).
    RecipeResolved {
        installer_slug: String,
        version: String,
        author: String,
    },
    /// `mac-app --recipe`: no applicable script (never fatal).
    RecipeMiss {
        key: String,
    },
    /// `mac-app --recipe`: an applied step finished.
    RecipeStep {
        verb: String,
    },
    /// `mac-app --recipe`: a step was skipped, with the reason.
    /// `runtimes install`: archive download started (design 12 §10).
    RuntimeDownloadStarted {
        id: String,
        version: String,
        bytes: u64,
    },
    /// `runtimes install`: archive downloaded, verified, extracted, probed,
    /// and committed to the store.
    RuntimeDownloadFinished {
        id: String,
        version: String,
    },
    /// `runtimes install` of a tool (winetricks): provisioned and ready.
    ToolProvisioned {
        id: String,
        version: String,
    },
    RecipeSkipped {
        verb: String,
        reason: String,
    },
    /// `mac-app`: the renderer the bundle bakes, the layer that chose it and
    /// why (#70, design 16 decision 5).
    RendererSelected {
        renderer: String,
        layer: String,
        reason: String,
    },
    /// `mac-app`: a build flag replaced a key in the game's L4 override file
    /// (`compat/<gogid>.toml`), so the flag wins at launch (#110, design 16
    /// decision 5).
    UserOverrideSet {
        gogid: String,
        key: String,
        old: Option<String>,
        new: Option<String>,
        path: std::path::PathBuf,
    },
    /// `linux-app` from a depot tree: the game files start going into the
    /// game's `game/` dir, by `method` (`reflink` or `copy`, the plan's
    /// probe) (#337, design 18 §6).
    GameFilesStarted {
        method: String,
        bytes: u64,
        dir: std::path::PathBuf,
        /// `"dlc"` for `linux-app dlc add`'s lay (design 19 §7); absent for a build.
        #[serde(skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        /// The files a DLC lay adds.
        #[serde(skip_serializing_if = "Option::is_none")]
        files: Option<u64>,
    },
    /// A copy's progress, at each tenth of `total` (none for a reflink,
    /// which takes no time).
    GameFilesProgress {
        copied: u64,
        total: u64,
    },
    /// `linux-app dlc add` (design 19 §7): the DLC it adds, where their files
    /// come from (`cache` or `staging`) and the installed build.
    DlcPlan {
        dlcs: Vec<crate::gog::depot::PlanDlc>,
        source: String,
        #[serde(rename = "buildId")]
        build_id: String,
        /// The DLC whose files the depot cache already holds: laid from it, not
        /// downloaded (#381).
        #[serde(rename = "fromCache", skip_serializing_if = "Vec::is_empty")]
        from_cache: Vec<u64>,
    },
    /// One DLC is in the game and recorded, after its install script ran.
    DlcInstalled {
        id: u64,
    },
    /// An interrupted add was settled: `forward` (finished) or `back` (undone).
    DlcRecovered {
        action: String,
    },
    /// An `auto` snapshot was taken before a change (design 20 §7.1).
    SnapshotCreated {
        id: u32,
        label: String,
        trigger: String,
    },
    /// Retention deleted an older `auto` snapshot (design 20 §7.2).
    /// `created` is RFC 3339, UTC.
    SnapshotPruned {
        id: u32,
        label: String,
        created: String,
    },
    /// A long step's running count (#420): `files` and `bytes` done so far,
    /// of the totals where known. Sent per file, for a live status line;
    /// `--json-progress` leaves it out (too many lines), as it does
    /// [`ProgressEvent::WorkDone`].
    WorkProgress {
        step: String,
        files: Option<u64>,
        files_total: Option<u64>,
        bytes: Option<u64>,
        bytes_total: Option<u64>,
    },
    /// A long step with no running count starts (#420): a subprocess, an
    /// extraction. A live status line shows a spinner and the time it has
    /// taken; `--json-progress` leaves it out.
    WorkStarted {
        step: String,
    },
    /// The step a [`ProgressEvent::WorkStarted`] or
    /// [`ProgressEvent::WorkProgress`] showed is over.
    WorkDone {
        step: String,
    },
}

/// The `step` names of [`ProgressEvent::WorkProgress`].
pub mod step {
    /// The game files going into a game's `game/` dir (a build, a DLC add).
    pub const GAME_FILES: &str = "game files";
    /// umu's `wineboot` making a new Wine prefix.
    pub const PREFIX: &str = "creating the Wine prefix";
    /// An offline installer running silently.
    pub const INSTALLER: &str = "running the installer";
    /// GOG's support installer and registry key.
    pub const SUPPORT: &str = "running GOG's support installer";
    /// A rebuild clearing the old `game/` dir.
    pub const REMOVING_GAME: &str = "removing the old game files";
    /// A new game's staging dir going into place.
    pub const MOVING_GAME: &str = "moving the game into place";
    /// A rebuild's new prefix going into place.
    pub const MOVING_PREFIX: &str = "moving the Wine prefix into place";
}

impl ProgressEvent {
    /// Only for a live status line (#420): not printed as NDJSON.
    pub fn is_live_only(&self) -> bool {
        matches!(
            self,
            ProgressEvent::WorkProgress { .. }
                | ProgressEvent::WorkStarted { .. }
                | ProgressEvent::WorkDone { .. }
        )
    }
}

/// Run `f` between a [`ProgressEvent::WorkStarted`] and a
/// [`ProgressEvent::WorkDone`] of `step`, whatever it returns.
pub fn working<T>(emit: &mut dyn FnMut(&ProgressEvent), step: &str, f: impl FnOnce() -> T) -> T {
    emit(&ProgressEvent::WorkStarted { step: step.into() });
    let out = f();
    emit(&ProgressEvent::WorkDone { step: step.into() });
    out
}

/// Drive a consumer over the event stream until the engine's terminal `Done`
/// event — or until `done` resolves, for command paths that end without one
/// (e.g. `--dry-run` returns right after planning). A dropped `done` sender
/// also resolves, so the loop always terminates. `Lagged` batches are skipped
/// (progress is lossy by design); `emit` is the frontend (NDJSON, bars).
pub async fn render(
    mut rx: tokio::sync::broadcast::Receiver<ProgressEvent>,
    done: tokio::sync::oneshot::Receiver<()>,
    mut emit: impl FnMut(&ProgressEvent),
) {
    tokio::select! {
        _ = done => {}
        _ = async {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        let terminal = matches!(ev, ProgressEvent::Done { .. });
                        emit(&ev);
                        if terminal {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        } => {}
    }
}

/// Broadcast channel; lagging receivers drop events by design (progress is lossy).
pub fn channel() -> (
    tokio::sync::broadcast::Sender<ProgressEvent>,
    tokio::sync::broadcast::Receiver<ProgressEvent>,
) {
    tokio::sync::broadcast::channel(64)
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use std::time::Duration;

    fn tag(ev: &ProgressEvent) -> &'static str {
        match ev {
            ProgressEvent::PlanReady { .. } => "PlanReady",
            ProgressEvent::FileStart { .. } => "FileStart",
            ProgressEvent::ChunkDone { .. } => "ChunkDone",
            ProgressEvent::FileDone { .. } => "FileDone",
            ProgressEvent::FileSkipped { .. } => "FileSkipped",
            ProgressEvent::FileFailed { .. } => "FileFailed",
            ProgressEvent::Warn { .. } => "Warn",
            ProgressEvent::Done { .. } => "Done",
            ProgressEvent::RuntimeResolved { .. } => "RuntimeResolved",
            ProgressEvent::PrefixStaged => "PrefixStaged",
            ProgressEvent::InstallStarted => "InstallStarted",
            ProgressEvent::InstallFinished { .. } => "InstallFinished",
            ProgressEvent::BundleAssembled => "BundleAssembled",
            ProgressEvent::Codesigned => "Codesigned",
            ProgressEvent::RecipeResolved { .. } => "RecipeResolved",
            ProgressEvent::RecipeMiss { .. } => "RecipeMiss",
            ProgressEvent::RecipeStep { .. } => "RecipeStep",
            ProgressEvent::RecipeSkipped { .. } => "RecipeSkipped",
            ProgressEvent::RuntimeDownloadStarted { .. } => "RuntimeDownloadStarted",
            ProgressEvent::RuntimeDownloadFinished { .. } => "RuntimeDownloadFinished",
            ProgressEvent::ToolProvisioned { .. } => "ToolProvisioned",
            ProgressEvent::RendererSelected { .. } => "RendererSelected",
            ProgressEvent::UserOverrideSet { .. } => "UserOverrideSet",
            ProgressEvent::GameFilesStarted { .. } => "GameFilesStarted",
            ProgressEvent::GameFilesProgress { .. } => "GameFilesProgress",
            ProgressEvent::DlcPlan { .. } => "DlcPlan",
            ProgressEvent::DlcInstalled { .. } => "DlcInstalled",
            ProgressEvent::DlcRecovered { .. } => "DlcRecovered",
            ProgressEvent::SnapshotCreated { .. } => "SnapshotCreated",
            ProgressEvent::SnapshotPruned { .. } => "SnapshotPruned",
            ProgressEvent::WorkProgress { .. } => "WorkProgress",
            ProgressEvent::WorkStarted { .. } => "WorkStarted",
            ProgressEvent::WorkDone { .. } => "WorkDone",
        }
    }

    #[tokio::test]
    async fn render_stops_on_done_event() {
        let (tx, rx) = channel();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let render = render(rx, done_rx, move |ev| sink.lock().unwrap().push(tag(ev)));
        let _ = tx.send(ProgressEvent::PlanReady {
            files: 1,
            total_bytes: 1,
            skipped_bytes: 0,
        });
        let _ = tx.send(ProgressEvent::Done {
            ok: 1,
            failed: 0,
            skipped: 0,
        });
        // After Done the loop is gone: this event must never be delivered.
        let _ = tx.send(ProgressEvent::Warn {
            message: "after".into(),
        });
        tokio::time::timeout(Duration::from_secs(2), render)
            .await
            .expect("render terminates on Done");
        drop(done_tx);
        assert_eq!(*seen.lock().unwrap(), vec!["PlanReady", "Done"]);
    }

    #[tokio::test]
    async fn render_stops_when_command_ends_without_done() {
        // dry-run path: no Done event, sender still alive — `done` must end it.
        let (tx, rx) = channel();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let render = render(rx, done_rx, |_ev| {});
        let _ = tx.send(ProgressEvent::Warn {
            message: "x".into(),
        });
        let _ = done_tx.send(());
        tokio::time::timeout(Duration::from_secs(2), render)
            .await
            .expect("render terminates on done signal");
    }
}
