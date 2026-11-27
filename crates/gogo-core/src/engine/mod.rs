//! plan() → execute() orchestration; owns the event channel (`docs/design/01` §1/§4,
//! pipeline in `docs/design/04` §1).

pub mod chunk;
pub mod events;
pub mod job;
#[cfg(test)]
mod job_tests;
pub mod limiter;
pub mod select;
pub mod sidecar;
pub mod task;
#[cfg(test)]
pub(crate) mod tests;
pub mod worker;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::sync::Semaphore;

use crate::app::AppContext;

use crate::engine::events::ProgressEvent;
use crate::engine::job::{JobScope, unless_cancelled};
use crate::engine::select::Filters;
use crate::engine::sidecar::PlanDoc;
use crate::engine::task::{FileTask, Kind, Status};
use crate::engine::worker::{ChunkWork, DownloadError, Job};
use crate::gog::catalog::Product;
use crate::state::records::{InstallRecord, parse_build};

/// Final tally, mirrored by the `Done` event (`docs/design/01` §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Summary {
    pub ok: u32,
    pub failed: u32,
    pub skipped: u32,
    /// Highest failed exit class (C1: outcome severity 3 < 4 < 5 < 6); `None` when
    /// nothing failed.
    pub worst_exit_class: Option<u8>,
}

/// D4: the `available_space` call sits behind a small trait — fs4 in production, a
/// mock in tests.
pub trait FreeSpace: Send + Sync {
    fn available_bytes(&self, path: &Path) -> std::io::Result<u64>;
}

pub struct Fs4FreeSpace;

impl FreeSpace for Fs4FreeSpace {
    fn available_bytes(&self, path: &Path) -> std::io::Result<u64> {
        fs4::available_space(path)
    }
}

enum Outcome {
    Done,
    Skipped(String),
    Failed(DownloadError),
}

/// The `FileSkipped` reason when another run holds the destination: the file
/// was not downloaded, so a frontend can report it as unfinished.
pub const LOCKED_SKIP: &str = "destination locked by another gogo process";

/// Build the work list: filters over catalog → `FileTask`s with plan-time statuses
/// (`docs/design/04` §2). Review B4: O(1) per task — no checksum fetches, no file
/// reads (a `stat` for skip-rule 1 is metadata only). Plan totals may overstate while
/// `pending-adoption` tasks await verification — accepted; execute reports actuals.
pub async fn plan(
    ctx: &Arc<AppContext>,
    products: &[Product],
    filters: &Filters,
    force: bool,
) -> Vec<FileTask> {
    let root = download_root(ctx);
    let mut tasks = Vec::new();
    for product in products {
        tasks.extend(select::for_product(product, filters, &root));
    }
    for task in &mut tasks {
        task.status = classify(task, &ctx.state, &root, force);
    }

    let files = tasks
        .iter()
        .filter(|t| !matches!(t.status, Status::Present))
        .count() as u32;
    let total_bytes = tasks
        .iter()
        .filter(|t| !matches!(t.status, Status::Present))
        .map(|t| t.total_size)
        .sum();
    let skipped_bytes = tasks
        .iter()
        .filter(|t| matches!(t.status, Status::Present))
        .map(|t| t.total_size)
        .sum();
    let _ = ctx.events.send(ProgressEvent::PlanReady {
        files,
        total_bytes,
        skipped_bytes,
    });
    tasks
}

/// Plan-time skip logic (`docs/design/04` §2 rules 1–5). The save name is only known
/// after resolve, so resume/adoption detection is approximate here and authoritative
/// at execute time.
fn classify(
    task: &FileTask,
    state: &Mutex<crate::state::State>,
    root: &Path,
    force: bool,
) -> Status {
    let state = state.lock().expect("state lock");
    let record = state.installs.iter().find(|r| {
        r.product_id == task.product_id && r.dlc_id == task.dlc_id && r.file_id == task.file.id
    });
    if let Some(record) = record {
        if crate::state::records::is_outdated(record, task.entry.version.as_deref(), None, false) {
            return Status::Outdated;
        }
        // `--force` bypasses ONLY the "already present" skip (04 §5): the file is
        // redownloaded fresh. Returning here (not falling through) matters — the
        // adoption rule below would re-adopt the very file being forced.
        if force {
            return Status::New;
        }
        // Rule 1: record + file present + size matches → skip.
        if let Ok(meta) = std::fs::metadata(root.join(&record.path))
            && meta.len() == record.size
        {
            return Status::Present;
        }
        // File gone or changed → fall through (redownload/resume).
    }
    if dir_contains(&task.dir, ".part.json") {
        return Status::Resumable;
    }
    if dir_contains(&task.dir, "") {
        return Status::PendingAdoption;
    }
    Status::New
}

/// C7 strict comparison (06 §4): checksum `timestamp` is primary; build id is
/// compared best-effort when both sides parse (record ts absent → only build).
pub fn strict_outdated_doc(
    record: &InstallRecord,
    doc: &crate::gog::checksum::ChecksumDoc,
) -> bool {
    if record.checksum_ts.is_some() && record.checksum_ts != doc.timestamp {
        return true;
    }
    if let (Some(record_build), Some(doc_build)) = (
        &record.build_id,
        crate::state::records::parse_build(&doc.name),
    ) && record_build != &doc_build
    {
        return true;
    }
    false
}

/// `update --strict` probe: one downlink + checksum resolution per PRESENT task
/// (the sanctioned B4 exception, 06 §4). Resolve failures degrade to "not
/// outdated" — the cheap path already vouched for the version.
pub async fn strict_outdated(ctx: &Arc<AppContext>, bearer: &str, task: &FileTask) -> bool {
    let record = {
        let state = ctx.state.lock().expect("state lock");
        state
            .installs
            .iter()
            .find(|r| {
                r.product_id == task.product_id
                    && r.dlc_id == task.dlc_id
                    && r.file_id == task.file.id
            })
            .cloned()
    };
    let Some(record) = record else {
        return false;
    };
    let dl = match crate::gog::downlink::resolve(&ctx.http, bearer, &task.file.downlink).await {
        Ok(dl) => dl,
        Err(e) => {
            let _ = ctx.events.send(ProgressEvent::Warn {
                message: crate::redact::scrub_text(&format!("strict check skipped (resolve): {e}")),
            });
            return false;
        }
    };
    let raw = match crate::gog::checksum::fetch_raw(&ctx.http, &dl.checksum).await {
        Ok(raw) => raw,
        Err(e) => {
            let _ = ctx.events.send(ProgressEvent::Warn {
                message: crate::redact::scrub_text(&format!(
                    "strict check skipped (checksum): {e}"
                )),
            });
            return false;
        }
    };
    // A5: keep the cache warm — a later `verify` works offline.
    let _ = crate::state::cache::save_checksum(task.product_id, task.dlc_id, &task.file.id, &raw);
    match crate::gog::checksum::parse(&raw) {
        Ok(doc) => strict_outdated_doc(&record, &doc),
        Err(e) => {
            let _ = ctx.events.send(ProgressEvent::Warn {
                message: crate::redact::scrub_text(&format!("strict check skipped (parse): {e}")),
            });
            false
        }
    }
}

/// The bytes of a file an earlier run left on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partial {
    /// The file's name, from the checksum document of that run.
    pub name: String,
    /// Bytes in chunks recorded as done.
    pub done: u64,
    /// The file's size in that checksum document.
    pub total: u64,
}

/// What `task` has on disk from an earlier run: local reads only. The next run
/// still checks the layout against a fresh checksum document (04 §3).
pub fn partial(task: &FileTask) -> Option<Partial> {
    let entries = std::fs::read_dir(&task.dir).ok()?;
    entries.filter_map(Result::ok).find_map(|entry| {
        let name = entry.file_name();
        let dest = task.dir.join(name.to_str()?.strip_suffix(".part.json")?);
        let plan = sidecar::load_plan(&dest)?;
        let same = plan.product_id == task.product_id
            && plan.dlc_id == task.dlc_id
            && plan.file_id == task.file.id
            // GOG keeps file ids across versions.
            && plan.is_for(task.total_size, task.entry.version.as_deref());
        if !same || !sidecar::part_path(&dest).is_file() {
            return None;
        }
        let bitmap = sidecar::load_bitmap(&dest, plan.chunks.len());
        let spans = plan.chunks.iter().map(|c| (c.id, c.from, c.to));
        Some(Partial {
            done: done_bytes(&bitmap, spans),
            name: plan.dest_name,
            total: plan.total_size,
        })
    })
}

/// Sum of the (inclusive) spans whose done-bit is set.
fn done_bytes(bitmap: &[u8], spans: impl Iterator<Item = (u32, u64, u64)>) -> u64 {
    spans
        .filter(|(id, _, _)| sidecar::is_done(bitmap, *id))
        .map(|(_, from, to)| (to + 1).saturating_sub(from))
        .sum()
}

fn dir_contains(dir: &Path, suffix: &str) -> bool {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries.filter_map(|e| e.ok()).any(|e| {
                e.path().extension().is_some() && e.file_name().to_string_lossy().ends_with(suffix)
            })
        })
        .unwrap_or(false)
}

fn download_root(ctx: &Arc<AppContext>) -> PathBuf {
    PathBuf::from(ctx.cfg.download.dir.clone().unwrap_or_default())
}

/// Run the pipeline for the planned tasks: `parallel_files` files at once, N chunk
/// workers each, per-chunk md5, sidecar updates, atomic rename, `InstallRecord` write
/// (`docs/design/04` §4–§7). The app token cancels it and its events go to
/// `ctx.events`; [`execute_job`] runs it as one job among several.
pub async fn execute(ctx: &Arc<AppContext>, tasks: Vec<FileTask>) -> Summary {
    execute_job(ctx, &JobScope::command(ctx), tasks).await
}

/// [`execute`] with `scope`'s own token and events (design 25 §5). A cancel leaves
/// the sidecars for a resume; the files it stopped count as failed (class 130).
/// Stop it by its token: dropping this future leaves the file tasks running.
pub async fn execute_job(ctx: &Arc<AppContext>, scope: &JobScope, tasks: Vec<FileTask>) -> Summary {
    // D4: disk preflight — exit 6, nothing written.
    if ctx.disk_check {
        // What a resumed file already holds is not needed a second time.
        let needed: u64 = tasks
            .iter()
            .filter(|t| !matches!(t.status, Status::Present))
            .map(|t| {
                let done = partial(t).map_or(0, |p| p.done);
                t.total_size.saturating_sub(done)
            })
            .sum();
        if needed > 0 {
            let root = download_root(ctx);
            let _ = std::fs::create_dir_all(&root);
            match ctx.free_space.available_bytes(&root) {
                Ok(have) if needed > have => {
                    let _ = scope.events().send(ProgressEvent::Warn {
                        message: crate::redact::scrub_text(&format!(
                            "insufficient disk space: need {}, have {}",
                            crate::fmt::human_size(needed),
                            crate::fmt::human_size(have)
                        )),
                    });
                    return Summary {
                        worst_exit_class: Some(6),
                        ..Summary::default()
                    };
                }
                Ok(_) => {}
                Err(e) => tracing::debug!("free-space probe failed (skipping): {e}"),
            }
        }
    }

    let sem = Arc::new(Semaphore::new(
        ctx.cfg.download.parallel_files.max(1) as usize
    ));
    let mut summary = Summary::default();
    let mut handles = Vec::with_capacity(tasks.len());
    for task in tasks {
        if matches!(task.status, Status::Present) {
            let _ = scope.events().send(ProgressEvent::FileSkipped {
                id: task.id,
                reason: "already present".into(),
            });
            summary.skipped += 1;
            continue;
        }
        let ctx = Arc::clone(ctx);
        let scope = scope.clone();
        let permit = sem.clone().acquire_owned().await.expect("semaphore");
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            download_one(ctx, scope, task).await
        }));
    }

    for handle in handles {
        let outcome = match handle.await {
            Ok((id, outcome)) => {
                if let Outcome::Skipped(reason) = &outcome {
                    let _ = scope.events().send(ProgressEvent::FileSkipped {
                        id,
                        reason: reason.clone(),
                    });
                }
                outcome
            }
            Err(join) => Outcome::Failed(DownloadError::RetriesExhausted(format!(
                "file task panicked: {join}"
            ))),
        };
        match outcome {
            Outcome::Done => summary.ok += 1,
            Outcome::Skipped(_) => summary.skipped += 1,
            Outcome::Failed(e) => {
                let class = e.exit_class();
                summary.failed += 1;
                summary.worst_exit_class = Some(
                    summary
                        .worst_exit_class
                        .map_or(class, |worst| worst.max(class)),
                );
            }
        }
    }

    let _ = scope.events().send(ProgressEvent::Done {
        ok: summary.ok,
        failed: summary.failed,
        skipped: summary.skipped,
    });
    summary
}

async fn download_one(
    ctx: Arc<AppContext>,
    scope: JobScope,
    mut task: FileTask,
) -> (uuid::Uuid, Outcome) {
    match download_one_inner(&ctx, &scope, &mut task).await {
        Ok(outcome) => (task.id, outcome),
        Err(e) => {
            // Construction-time redaction is the rule; this is the net (07 §4).
            let _ = scope.events().send(ProgressEvent::FileFailed {
                id: task.id,
                error: crate::redact::scrub_text(&e.to_string()),
            });
            (task.id, Outcome::Failed(e))
        }
    }
}

async fn download_one_inner(
    ctx: &Arc<AppContext>,
    scope: &JobScope,
    task: &mut FileTask,
) -> Result<Outcome, DownloadError> {
    // A file the cancel found still waiting leaves nothing behind.
    if scope.cancel().is_cancelled() {
        return Err(DownloadError::Aborted);
    }
    let Some(session) = ctx.session.as_ref() else {
        return Err(DownloadError::Auth("no session — run gogo login".into()));
    };
    // Not raced against the cancel: dropping a token refresh halfway could lose
    // the rotated refresh token.
    let bearer = session
        .bearer()
        .await
        .map_err(|e| DownloadError::Auth(e.to_string()))?;
    let resolve = || async {
        let resolving = crate::gog::downlink::resolve(&ctx.http, &bearer, &task.file.downlink);
        unless_cancelled(scope.cancel(), resolving)
            .await
            .ok_or(DownloadError::Aborted)
    };

    // — resolve (per file, every run; URLs are never persisted) —
    let dl = match resolve().await? {
        Ok(dl) => dl,
        Err(e) if matches!(e, crate::gog::error::ApiError::Forbidden(_)) => {
            return bundled_skip(task, &format!("downlink 403: {e}"));
        }
        Err(e) => {
            return Err(DownloadError::RetriesExhausted(format!("resolve: {e}")));
        }
    };

    let fetching = crate::gog::checksum::fetch_raw(&ctx.http, &dl.checksum);
    let raw = match unless_cancelled(scope.cancel(), fetching)
        .await
        .ok_or(DownloadError::Aborted)?
    {
        Ok(raw) => raw,
        Err(e) if matches!(e, crate::gog::error::ApiError::Forbidden(_)) => {
            return bundled_skip(task, &format!("checksum 403: {e}"));
        }
        Err(e) => {
            return Err(DownloadError::RetriesExhausted(format!("checksum: {e}")));
        }
    };
    // A5: verify-cache keeps the last checksum doc so `gogo verify` works offline.
    let _ = crate::state::cache::save_checksum(task.product_id, task.dlc_id, &task.file.id, &raw);
    let doc = crate::gog::checksum::parse(&raw)
        .map_err(|e| DownloadError::RetriesExhausted(format!("checksum parse: {e}")))?;
    for warning in &doc.warnings {
        let _ = scope.events().send(ProgressEvent::Warn {
            message: crate::redact::scrub_text(&format!("{}: {warning}", task.file.id)),
        });
    }

    // A2: the doc itself says the file is not downloadable.
    if !doc.available {
        let message = doc
            .notavailablemsg
            .clone()
            .unwrap_or_else(|| "not available".into());
        let _ = scope.events().send(ProgressEvent::Warn {
            message: crate::redact::scrub_text(&format!("{}: {message}", task.file.id)),
        });
        return Ok(Outcome::Skipped(format!("unavailable: {message}")));
    }

    // C4 filename chain: checksum `name` → (Content-Disposition: unverified, probe
    // open) → sanitized entry name + file id.
    let save_name = if doc.name.is_empty() {
        let fallback = format!(
            "{}-{}",
            crate::engine::select::sanitize(
                task.entry.name.as_deref().unwrap_or(task.kind.as_str())
            ),
            task.file.id
        );
        let _ = scope.events().send(ProgressEvent::Warn {
            message: crate::redact::scrub_text(&format!(
                "{}: no checksum name — using `{fallback}`",
                task.file.id
            )),
        });
        fallback
    } else {
        doc.name.clone()
    };
    task.save_name = Some(save_name.clone());
    let dest = task.dest();
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // B5: per-destination exclusivity — flock on `{dest}.part` (created if absent).
    // A second gogo process skips this file with a `Warn`.
    let part = sidecar::part_path(&dest);
    let dest_lock = std::fs::OpenOptions::new()
        .create(true)
        // Never truncate: a resumed `.part` holds already-verified chunks (B5).
        .truncate(false)
        .write(true)
        .read(true)
        .open(&part)?;
    {
        if dest_lock.try_lock().is_err() {
            return Ok(Outcome::Skipped(LOCKED_SKIP.into()));
        }
    }

    // B4: adoption at execute time — verify the unrecorded local file, then adopt it.
    if dest.is_file() && record_of(ctx, task).is_none() && verify_against(&dest, &doc) {
        append_record(ctx, task, &doc, &dest);
        return Ok(Outcome::Skipped("adopted existing verified file".into()));
    }
    if dest.is_file() && record_of(ctx, task).is_none() {
        let _ = scope.events().send(ProgressEvent::Warn {
            message: crate::redact::scrub_text(&format!(
                "{save_name}: unrecorded file failed verification — redownloading over it"
            )),
        });
    }

    // Sidecar plan + resume bitmap (layout validation, 04 §3).
    let chunks = chunk_works(&doc);
    match sidecar::load_plan(&dest) {
        Some(plan) if plan.layout_matches(&doc) => {}
        Some(_) => {
            let _ = scope.events().send(ProgressEvent::Warn {
                message: crate::redact::scrub_text(&format!(
                    "{save_name}: remote layout changed — restarting"
                )),
            });
            sidecar::save_plan(
                &dest,
                &PlanDoc::from_checksum(task.product_id, task.dlc_id, &task.file.id, &doc)
                    .listed(task.total_size, task.entry.version.clone()),
            )?;
        }
        None => {
            sidecar::save_plan(
                &dest,
                &PlanDoc::from_checksum(task.product_id, task.dlc_id, &task.file.id, &doc)
                    .listed(task.total_size, task.entry.version.clone()),
            )?;
        }
    }

    let spans = chunks
        .iter()
        .map(|c| (c.spec.index, c.spec.start, c.spec.end));
    let _ = scope.events().send(ProgressEvent::FileStart {
        id: task.id,
        name: save_name.clone(),
        total_bytes: doc.total_size,
        done_bytes: done_bytes(&sidecar::load_bitmap(&dest, chunks.len()), spans),
    });

    // B3/C5: bounded re-resolve loop. Budget = `network.retries`; a 404 link gets
    // exactly one re-resolve before failing.
    let mut budget = ctx.cfg.network.retries;
    let mut gone_tried = false;
    let mut url = dl.downlink;
    loop {
        let job = Job {
            http: ctx.http.clone(),
            url: url.clone(),
            dest: dest.clone(),
            total_size: doc.total_size,
            chunks: chunks.clone(),
            bitmap: sidecar::load_bitmap(&dest, chunks.len()),
            workers: ctx.cfg.download.workers,
            limiter: Arc::clone(&ctx.limiter),
            cancel: scope.cancel().clone(),
            events: scope.events().clone(),
            file_id: task.id,
            display_name: save_name.clone(),
            retries: ctx.cfg.network.retries,
        };
        match worker::download_file(job).await {
            Ok(()) => break,
            Err(DownloadError::UrlExpired) if budget > 0 => {
                budget -= 1;
                match resolve().await? {
                    Ok(fresh) => url = fresh.downlink,
                    Err(e) => {
                        return Err(DownloadError::RetriesExhausted(format!("re-resolve: {e}")));
                    }
                }
            }
            Err(DownloadError::LinkGone) if !gone_tried => {
                gone_tried = true;
                match resolve().await? {
                    Ok(fresh) => url = fresh.downlink,
                    Err(e) => {
                        return Err(DownloadError::RetriesExhausted(format!("re-resolve: {e}")));
                    }
                }
            }
            Err(e) => return Err(e),
        }
    }

    // Finalize (A3): `sync_all` before the final rename — the file with the final
    // name is complete + verified + durable.
    std::fs::File::open(&part)?.sync_all()?;
    std::fs::rename(&part, &dest)?;
    sidecar::remove(&dest)?;
    append_record(ctx, task, &doc, &dest);

    let _ = scope.events().send(ProgressEvent::FileDone {
        id: task.id,
        path: dest,
    });
    Ok(Outcome::Done)
}

fn bundled_skip(task: &FileTask, why: &str) -> Result<Outcome, DownloadError> {
    // 403/404 on resolution for extras → bundled-in-installer (prior-art behavior);
    // for installers it is a file failure (07 §3).
    if task.kind == Kind::BonusContent || task.kind == Kind::LanguagePack {
        Ok(Outcome::Skipped(format!("bundled ({why})")))
    } else {
        Err(DownloadError::RetriesExhausted(why.to_owned()))
    }
}

/// Chunk work list from the authoritative doc. No-chunks case → one whole-file span,
/// md5 from the root element when present, else size-only (04 §3).
fn chunk_works(doc: &crate::gog::checksum::ChecksumDoc) -> Vec<ChunkWork> {
    if doc.chunks.is_empty() {
        if doc.total_size == 0 {
            return Vec::new();
        }
        return vec![ChunkWork {
            spec: crate::engine::chunk::ChunkSpec {
                index: 0,
                start: 0,
                end: doc.total_size - 1,
            },
            md5: doc.md5.clone(),
        }];
    }
    doc.chunks
        .iter()
        .map(|c| ChunkWork {
            spec: crate::engine::chunk::ChunkSpec {
                index: c.id,
                start: c.from,
                end: c.to,
            },
            md5: Some(c.md5.clone()),
        })
        .collect()
}

/// B4 adoption check: per-chunk md5 of the local file against the fresh doc.
/// Verify a local file against a checksum doc (per-chunk md5; size-only when
/// the doc has neither chunks nor a root md5). Public for `gogo verify`.
pub fn verify_against(dest: &Path, doc: &crate::gog::checksum::ChecksumDoc) -> bool {
    verify_against_with(dest, doc, &mut |_| {})
}

/// [`verify_against`], telling `on_bytes` how many bytes it has hashed so far
/// (of `doc.total_size`) as it goes (#420).
pub fn verify_against_with(
    dest: &Path,
    doc: &crate::gog::checksum::ChecksumDoc,
    on_bytes: &mut dyn FnMut(u64),
) -> bool {
    let Ok(file) = std::fs::File::open(dest) else {
        return false;
    };
    let Ok(meta) = file.metadata() else {
        return false;
    };
    if meta.len() != doc.total_size {
        return false;
    }
    let mut buf = vec![0u8; 1024 * 1024];
    let mut hashed = 0u64;
    let mut read = |n: u64| {
        hashed += n;
        on_bytes(hashed);
    };
    if doc.chunks.is_empty() {
        return match &doc.md5 {
            Some(expected) => md5_file(&file, 0, doc.total_size, &mut buf, &mut read)
                .is_some_and(|d| d.eq_ignore_ascii_case(expected)),
            None => true, // size-only
        };
    }
    doc.chunks.iter().all(|chunk| {
        let len = chunk.to + 1 - chunk.from; // inclusive end -> span length
        md5_file(&file, chunk.from, len, &mut buf, &mut read)
            .is_some_and(|d| d.eq_ignore_ascii_case(&chunk.md5))
    })
}

fn md5_file(
    file: &std::fs::File,
    from: u64,
    len: u64,
    buf: &mut [u8],
    on_read: &mut dyn FnMut(u64),
) -> Option<String> {
    use std::os::unix::fs::FileExt;

    use md5::{Digest, Md5};

    let mut md5 = Md5::new();
    let mut pos = from;
    let end = from.checked_add(len)?;
    while pos < end {
        let take = ((end - pos) as usize).min(buf.len());
        file.read_exact_at(&mut buf[..take], pos).ok()?;
        md5.update(&buf[..take]);
        pos += take as u64;
        on_read(take as u64);
    }
    Some(crate::fmt::md5_hex(md5))
}

fn record_of(ctx: &Arc<AppContext>, task: &FileTask) -> Option<InstallRecord> {
    let state = ctx.state.lock().expect("state lock");
    state
        .installs
        .iter()
        .find(|r| {
            r.product_id == task.product_id && r.dlc_id == task.dlc_id && r.file_id == task.file.id
        })
        .cloned()
}

pub fn append_record(
    ctx: &Arc<AppContext>,
    task: &FileTask,
    doc: &crate::gog::checksum::ChecksumDoc,
    dest: &Path,
) {
    let root = download_root(ctx);
    let relative = dest
        .strip_prefix(&root)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| dest.to_path_buf());
    let record = InstallRecord {
        product_id: task.product_id,
        dlc_id: task.dlc_id,
        product_title: task.product_title.clone(),
        file_id: task.file.id.clone(),
        entry_id: task.entry.id.clone(),
        kind: task.kind.as_str().to_owned(),
        version: task.entry.version.clone(),
        build_id: parse_build(&doc.name),
        checksum_ts: doc.timestamp,
        size: doc.total_size,
        path: relative,
        installed_at: crate::time::autosi_offset(jiff::Timestamp::now()),
    };
    {
        let mut state = ctx.state.lock().expect("state lock");
        // Replace-or-append keyed on identity (06 §3).
        let saved = state.update(|state| {
            state.installs.retain(|r| {
                !(r.product_id == record.product_id
                    && r.dlc_id == record.dlc_id
                    && r.file_id == record.file_id)
            });
            state.installs.push(record);
        });
        if let Err(e) = saved {
            tracing::error!("state save failed: {e}");
        }
    }
}

#[cfg(test)]
mod verify_tests {
    use super::verify_against;
    use crate::gog::checksum::parse;
    use md5::Digest;

    #[test]
    fn verify_against_checks_every_chunk_span() {
        // Two spans; the second one catches the end-vs-length regression (chunk 0
        // verified by coincidence under the old bug, chunk 1 never did).
        let left = vec![b'a'; 30];
        let right = vec![b'b'; 20];
        let md5s = [
            crate::fmt::md5_hex(md5::Md5::new_with_prefix(&left)),
            crate::fmt::md5_hex(md5::Md5::new_with_prefix(&right)),
        ];
        let xml = format!(
            r#"<file name="x.bin" total_size="50" chunks="2">
	<chunk id="0" from="0" to="29" method="md5">{}</chunk>
	<chunk id="1" from="30" to="49" method="md5">{}</chunk>
</file>"#,
            md5s[0], md5s[1]
        );
        let doc = parse(&xml).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("x.bin");
        std::fs::write(&dest, [left.as_slice(), right.as_slice()].concat()).unwrap();
        assert!(verify_against(&dest, &doc), "intact file verifies");
        let mut seen = Vec::new();
        assert!(super::verify_against_with(&dest, &doc, &mut |b| seen.push(b)));
        assert_eq!(seen.last(), Some(&doc.total_size), "every byte is counted");
        assert!(seen.windows(2).all(|w| w[0] < w[1]), "{seen:?}");

        let mut corrupted = std::fs::read(&dest).unwrap();
        corrupted[45] = b'c'; // inside chunk 1 only
        std::fs::write(&dest, corrupted).unwrap();
        assert!(
            !verify_against(&dest, &doc),
            "corrupt chunk 1 fails the file"
        );
    }
}
