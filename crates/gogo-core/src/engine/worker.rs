//! Per-chunk downloader: Range GET → write_at → md5 (`docs/design/04` §4).
//!
//! Durability (review A3): preallocation is `set_len` + `sync_all` (durable file size
//! before any chunk is claimed done); each chunk's `sync_data` runs **before** the
//! bitmap rename records it done — a durable bitmap never claims non-durable bytes.
//!
//! Budgets (reviews B3/B6): network retries draw from `network.retries` (the same
//! budget the caller uses for re-resolution); the 30-minute per-file budget counts
//! **backoff idle only** — steady transfer never counts, and any progress resets it.
//! C5: CDN 404 returns [`DownloadError::LinkGone`] (one re-resolve, then fail);
//! 403/416 return [`DownloadError::UrlExpired`] (re-resolve while budget remains).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use md5::{Digest, Md5};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::engine::chunk::ChunkSpec;
use crate::engine::events::ProgressEvent;
use crate::engine::limiter::GlobalLimiter;
use crate::engine::sidecar;

/// One unit of work: span + expected chunk md5 (`None` = size-only verify — the
/// no-checksum-doc whole-file case).
#[derive(Debug, Clone)]
pub struct ChunkWork {
    pub spec: ChunkSpec,
    pub md5: Option<String>,
}

/// Per-file error taxonomy → exit classes (`03` §5, `07` §2; review C1).
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    /// Exit 5 — data integrity.
    #[error("chunk md5 mismatch after retries")]
    ChunkMismatch,
    /// Exit 4 — network-class exhaustion.
    #[error("retries exhausted: {0}")]
    RetriesExhausted(String),
    /// Exit 6 — disk.
    #[error("insufficient disk space: need {need}, have {have}")]
    DiskSpace { need: u64, have: u64 },
    /// Exit 6 for ENOSPC, else 1 (review C1).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Exit 3 — auth (C1: outcome severity 3 < 4 < 5 < 6).
    #[error("auth: {0}")]
    Auth(String),
    /// Exit 130 — cancelled; sidecars flushed at chunk boundaries.
    #[error("download aborted")]
    Aborted,
    /// CDN 403/416 — pre-tokenized URL expired mid-file; caller re-resolves (B3).
    #[error("cdn url expired mid-download")]
    UrlExpired,
    /// CDN 404 — link revoked / file replaced; one re-resolve, then fail (C5).
    #[error("cdn 404: link revoked or file replaced")]
    LinkGone,
}

impl DownloadError {
    /// Exit-class per `03` §5 (outcome severity: auth 3 < network 4 < integrity 5 <
    /// disk 6). Batches exit with the highest failed class (C1).
    pub fn exit_class(&self) -> u8 {
        match self {
            DownloadError::ChunkMismatch => 5,
            DownloadError::RetriesExhausted(_) => 4,
            DownloadError::Auth(_) => 3,
            DownloadError::DiskSpace { .. } => 6,
            DownloadError::Io(e) if e.kind() == std::io::ErrorKind::StorageFull => 6,
            DownloadError::Io(_) => 1,
            DownloadError::Aborted => 130,
            // Internal signals — mapped by the caller before exit accounting.
            DownloadError::UrlExpired | DownloadError::LinkGone => 4,
        }
    }
}

/// B6: 30 minutes of accumulated backoff/no-progress idle per file.
const NO_PROGRESS_BUDGET_MS: u64 = 30 * 60 * 1000;
/// Read granularity for limiter accounting (04 §4: 32–64 KiB reads).
const READ_BUF: u64 = 64 * 1024;

/// Everything one file's download needs. `url` is swapped by the caller's
/// re-resolve loop; `bitmap` reflects previously-completed chunks (resume).
pub struct Job {
    pub http: reqwest::Client,
    pub url: String,
    /// Final destination path (sidecars derive from it).
    pub dest: PathBuf,
    pub total_size: u64,
    pub chunks: Vec<ChunkWork>,
    pub bitmap: Vec<u8>,
    pub workers: u32,
    pub limiter: Arc<GlobalLimiter>,
    /// Per-file child token; the caller cancels it on batch-level Ctrl-C.
    pub cancel: CancellationToken,
    pub events: tokio::sync::broadcast::Sender<ProgressEvent>,
    pub file_id: Uuid,
    pub display_name: String,
    /// Network retry budget (`network.retries`, B3).
    pub retries: u32,
}

struct Shared {
    http: reqwest::Client,
    dest: PathBuf,
    url: String,
    limiter: Arc<GlobalLimiter>,
    cancel: CancellationToken,
    events: tokio::sync::broadcast::Sender<ProgressEvent>,
    file_id: Uuid,
    retries: u32,
    bitmap: Mutex<Vec<u8>>,
    file_bytes: AtomicU64,
    retry_idle_ms: AtomicU64,
}

/// Download all pending chunks of one file. Preallocates `{dest}.part` when absent
/// (A3), spawns `workers` chunk workers, returns when every chunk is durable + marked.
pub async fn download_file(job: Job) -> Result<(), DownloadError> {
    let part = sidecar::part_path(&job.dest);
    if let Some(parent) = part.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Preallocate whenever the SIZE does not match — the B5 destination lock
    // pre-creates `.part` (0 bytes), so a mere existence check would skip A3's
    // set_len + sync_all (found live: .part grew organically, never pre-sized).
    let needs_prealloc = std::fs::metadata(&part)
        .map(|m| m.len() != job.total_size)
        .unwrap_or(true);
    if needs_prealloc {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false) // preallocate-only: set_len below sizes the file
            .read(true)
            .write(true)
            .open(&part)?;
        file.set_len(job.total_size)?;
        // A3: durable file size (metadata) before any chunk is claimed done.
        file.sync_all()?;
    }

    let shared = Arc::new(Shared {
        http: job.http,
        dest: job.dest.clone(),
        url: job.url,
        limiter: job.limiter,
        cancel: job.cancel,
        events: job.events,
        file_id: job.file_id,
        retries: job.retries,
        bitmap: Mutex::new(job.bitmap),
        file_bytes: AtomicU64::new(0),
        retry_idle_ms: AtomicU64::new(0),
    });

    // Pending chunks in a shared deque: each worker pops the next undone span
    // (writes stay disjoint — one span, one worker).
    let queue: Arc<Mutex<VecDeque<ChunkWork>>> = Arc::new(Mutex::new(
        job.chunks
            .iter()
            .filter(|c| !sidecar::is_done(&shared.bitmap.lock().unwrap(), c.spec.index))
            .cloned()
            .collect(),
    ));
    let worker_count = job.workers.max(1);
    let mut handles = Vec::with_capacity(worker_count as usize);
    for _ in 0..worker_count {
        let shared = Arc::clone(&shared);
        let queue = Arc::clone(&queue);
        handles.push(tokio::spawn(async move {
            // Own handle per worker — writes at disjoint offsets, no data-file locks.
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(sidecar::part_path(&shared.dest))?;
            loop {
                let chunk = { queue.lock().unwrap().pop_front() };
                let Some(chunk) = chunk else {
                    return Ok(());
                };
                if let Err(e) = run_chunk(&shared, &file, &chunk).await {
                    shared.cancel.cancel();
                    return Err(e);
                }
            }
        }));
    }

    let mut first_err = None;
    for handle in handles {
        match handle.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                first_err.get_or_insert(e);
            }
            Err(join) => {
                first_err.get_or_insert(DownloadError::RetriesExhausted(format!(
                    "worker panicked: {join}"
                )));
            }
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

async fn run_chunk(
    shared: &Shared,
    file: &std::fs::File,
    chunk: &ChunkWork,
) -> Result<(), DownloadError> {
    let mut md5_attempts: u32 = 0;
    let mut net_attempts: u32 = 0;
    loop {
        if shared.cancel.is_cancelled() {
            return Err(DownloadError::Aborted);
        }
        let resp = shared
            .http
            .get(&shared.url)
            .header(reqwest::header::RANGE, range_header(&chunk.spec))
            .send()
            .await;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                backoff(shared, &mut net_attempts, format!("network: {e}")).await?;
                continue;
            }
        };
        match resp.status().as_u16() {
            206 | 200 => {
                match stream_chunk(shared, file, chunk, resp).await {
                    Ok(ChunkOutcome::Verified { bytes }) => {
                        persist_chunk(shared, file, chunk, bytes)?;
                        return Ok(());
                    }
                    Ok(ChunkOutcome::Corrupt) => {
                        md5_attempts += 1;
                        if md5_attempts >= 3 {
                            return Err(DownloadError::ChunkMismatch);
                        }
                        continue; // fresh GET for the same Range (04 §4)
                    }
                    Err(Retryable(msg)) => {
                        backoff(shared, &mut net_attempts, msg).await?;
                        continue;
                    }
                }
            }
            403 | 416 => return Err(DownloadError::UrlExpired),
            404 => return Err(DownloadError::LinkGone),
            status if status >= 500 || status == 429 => {
                backoff(shared, &mut net_attempts, format!("HTTP {status}")).await?;
                continue;
            }
            status => return Err(DownloadError::RetriesExhausted(format!("HTTP {status}"))),
        }
    }
}

enum ChunkOutcome {
    Verified { bytes: u64 },
    Corrupt,
}
struct Retryable(String);

async fn stream_chunk(
    shared: &Shared,
    file: &std::fs::File,
    chunk: &ChunkWork,
    resp: reqwest::Response,
) -> Result<ChunkOutcome, Retryable> {
    use std::os::unix::fs::FileExt;

    let mut md5 = Md5::new();
    let mut pos = chunk.spec.start;
    let mut bytes: u64 = 0;
    let mut stream = resp.bytes_stream();
    while let Some(item) = stream.next().await {
        let buf = item.map_err(|e| Retryable(format!("stream: {e}")))?;
        for slice in split_reads(buf.len() as u64) {
            shared.limiter.acquire(slice).await;
        }
        if shared.cancel.is_cancelled() {
            // Bitmap reflects only fully-verified chunks — partial writes are simply
            // overwritten by the retry. Nothing to flush here (04 §4: flush at
            // boundaries = per-chunk bitmap saves already done).
            return Err(Retryable("aborted".into()));
        }
        file.write_all_at(&buf, pos)
            .map_err(|e| Retryable(format!("write: {e}")))?;
        md5.update(&buf);
        pos += buf.len() as u64;
        bytes += buf.len() as u64;
        if pos > chunk.spec.end + 1 {
            return Err(Retryable(
                "server sent more than the requested range".into(),
            ));
        }
    }
    if pos != chunk.spec.end + 1 {
        return Err(Retryable("truncated body".into()));
    }
    match &chunk.md5 {
        Some(expected) => {
            let digest = crate::fmt::md5_hex(md5);
            if digest.eq_ignore_ascii_case(expected) {
                Ok(ChunkOutcome::Verified { bytes })
            } else {
                Ok(ChunkOutcome::Corrupt)
            }
        }
        None => Ok(ChunkOutcome::Verified { bytes }), // size-only (no-checksum case)
    }
}

/// A3 ordering: `sync_data` (bytes durable) BEFORE the bitmap rename records the chunk.
fn persist_chunk(
    shared: &Shared,
    file: &std::fs::File,
    chunk: &ChunkWork,
    bytes: u64,
) -> Result<(), DownloadError> {
    file.sync_data()?;
    let bits = {
        let mut bits = shared.bitmap.lock().unwrap();
        sidecar::mark_done(&mut bits, chunk.spec.index);
        sidecar::save_bitmap(&shared.dest, &bits)?;
        bits.clone()
    };
    let _ = bits; // (kept for potential future observability)
    let cumulative = shared.file_bytes.fetch_add(bytes, Ordering::SeqCst) + bytes;
    let _ = shared.events.send(ProgressEvent::ChunkDone {
        id: shared.file_id,
        chunk: chunk.spec.index,
        bytes: u32::try_from(bytes).unwrap_or(u32::MAX),
        file_bytes: cumulative,
        md5_ok: true,
    });
    // B6: progress resets the no-progress budget.
    shared.retry_idle_ms.store(0, Ordering::SeqCst);
    Ok(())
}

/// B6: backoff counts against the budget; steady transfer never does. `delay_k =
/// base · 2^k` capped at 60 s (07 §3; jitter omitted in v1 — no RNG in core deps).
async fn backoff(shared: &Shared, attempts: &mut u32, reason: String) -> Result<(), DownloadError> {
    *attempts += 1;
    if *attempts > shared.retries {
        return Err(DownloadError::RetriesExhausted(reason));
    }
    let delay_ms = (500u64 << (*attempts - 1).min(7)).min(60_000);
    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
    let idle = shared.retry_idle_ms.fetch_add(delay_ms, Ordering::SeqCst) + delay_ms;
    if idle >= NO_PROGRESS_BUDGET_MS {
        return Err(DownloadError::RetriesExhausted(
            "gave up after 30 min of retry idle — rerun to resume".into(),
        ));
    }
    Ok(())
}

fn range_header(spec: &ChunkSpec) -> String {
    format!("bytes={}-{}", spec.start, spec.end)
}

/// The CDN body arrives in reqwest-sized pieces; the limiter sees each piece's length
/// (already ≤ burst for sane limits — the split inside `acquire` covers the rest, A4).
fn split_reads(len: u64) -> Vec<u64> {
    if len == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut remaining = len;
    while remaining > 0 {
        let take = remaining.min(READ_BUF);
        out.push(take);
        remaining -= take;
    }
    out
}
