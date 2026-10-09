//! Per-chunk downloader: Range GET → write_at → md5 (`docs/design/04` §6).
//!
//! Workers are `tokio::spawn`ed tasks sharing: an owned `std::fs::File` per worker
//! (writes at offsets), an mpsc job channel of pending chunks, the global rate limiter,
//! and `Arc<FileProgress>` for sidecar updates + indicatif bars + events
//! (`docs/design/01` §3).

use crate::engine::chunk::ChunkSpec;

pub struct WorkerShared {
    /// Pre-tokenized CDN URL — construct-time redaction applies to every log line
    /// that could ever carry it (`docs/design/07` §4).
    pub url: url::Url,
    pub dest_part: std::path::PathBuf,
    pub chunks: Vec<ChunkSpec>,
}

/// Run one worker to completion; returns when the job channel closes or the
/// cancellation token fires (stop at chunk/read boundaries).
pub async fn run(
    _shared: std::sync::Arc<WorkerShared>,
    _jobs: tokio::sync::mpsc::Receiver<ChunkSpec>,
    _limiter: std::sync::Arc<crate::engine::limiter::Limiter>,
    _cancel: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    todo!("Range GET (retry matrix 07 §3) → rate-limited read → write_at → md5 → ChunkDone")
}
