//! `gogo saves pull` (design 22 §5.1, §5.2, §7): downloads and local
//! deletions, never a write to the cloud.
//!
//! Order of a real run:
//!
//! 1. the game's cloud-save sync lock, then the game's change lock (another
//!    gogo changing the game, or the game running, refuses: exit 7) — the
//!    sync lock always comes before the snapshot lock (design 22 §12d);
//! 2. `status`'s plan, unchanged ([`super::status::plan_detailed`]);
//! 3. conflicts settled (`--prefer cloud`, the prompt) or left (exit 8);
//! 4. one question when anything is overwritten or deleted;
//! 5. the `cloud-pull` snapshot when anything is overwritten or deleted; a
//!    failure stops the pull with nothing changed;
//! 6. every file that will be overwritten, deleted or kept aside copied to
//!    the backup dir, before any of them is touched;
//! 7. D11: a record for another client set aside, just before the first
//!    write of the record;
//! 8. per file: the path checked again, the object fetched and checked,
//!    written atomically ([`super::write`]), the record saved;
//! 9. `last_sync`, `rolled_back` cleared when nothing is left, backups pruned.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::auth::CloudAuth;
use super::backup::{self, Backup};
use super::classify::{Action, ConflictReason};
use super::error::CloudError;
use super::http::StorageClient;
use super::locations::{Game, Location, Resolved};
use super::record::{self, FileRecord, Record, SyncLock};
use super::remote_config::Source;
use super::status::{self, Detail, FilePlan, Plan};
use super::storage::{self, Entry};
use super::write::{self, Expect, WriteError};

/// What a pull does to one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// A new local file.
    Download,
    /// A download over a local file (backed up first).
    Overwrite,
    /// The local file renamed `<name>.local-<UTC>`, then the cloud's
    /// downloaded (a conflict answered `both`).
    KeepBoth,
    /// The cloud deleted it: the local file removed (backed up first).
    DeleteLocal,
    /// The same on both sides: recorded only.
    Record,
    /// Gone on both sides: dropped from the record.
    Forget,
}

impl Op {
    /// Loses a local file's content unless backed up.
    pub fn destructive(self) -> bool {
        matches!(self, Op::Overwrite | Op::DeleteLocal)
    }
    fn transfers(self) -> bool {
        matches!(self, Op::Download | Op::Overwrite | Op::KeepBoth)
    }
    /// The local file is copied to the backup dir first: the destructive
    /// ones, and a `both`, whose kept file is the only copy otherwise.
    fn backed_up(self) -> bool {
        self.destructive() || self == Op::KeepBoth
    }
}

/// An answer to a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Choice {
    /// The cloud's side wins: downloaded, or the local file deleted.
    Cloud,
    /// The local file stays as it is, for a later push; nothing recorded.
    Local,
    /// The local file renamed aside, the cloud's downloaded.
    Both,
    /// Left for later (exit 8).
    Skip,
}

/// Why a file was left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeftReason {
    /// A conflict nobody decided (no TTY, no `--prefer`, or `skip`).
    Undecided,
    /// Two cloud names on one local file: never settled by a pull.
    CaseClash,
    /// The save dir is gone while the record has files in it (a moved
    /// prefix, a drive not mounted?): a pull doesn't make it again.
    LocationMissing,
    /// The game's prefix (its `drive_c`) or files aren't there: gogo makes
    /// no prefix.
    NoPrefix,
    /// The file changed between the plan and the write.
    ChangedSincePlan,
    /// A link, or a file where a dir should be, on the way to the file:
    /// not followed, nothing written there.
    PathRefused,
}

/// Where one file stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "reason")]
pub enum State {
    /// A dry run: what a real run would do.
    Planned,
    Done,
    /// Needs a decision (exit 8).
    Left(LeftReason),
    /// An upload or cloud deletion: `gogo saves push` does it.
    ForPush,
    /// A conflict answered `local`: kept for a later push.
    KeptLocal,
    /// The cloud object failed its check (exit 5).
    Corrupt,
    /// The local file couldn't be read (exit 1).
    Unreadable,
    /// Not reached: the run stopped before it.
    NotDone,
}

/// One file of the plan that isn't `nothing`.
#[derive(Debug, Clone, Serialize)]
pub struct FileResult {
    pub name: String,
    /// What `status` planned.
    #[serde(flatten)]
    pub planned: Action,
    /// What the pull does.
    pub op: Option<Op>,
    pub choice: Option<Choice>,
    #[serde(flatten)]
    pub state: State,
    /// Where the old local file was backed up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<PathBuf>,
    /// A `both` answer: the local file's new name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kept_as: Option<String>,
}

/// What the one question of a run says.
#[derive(Debug, Clone, Serialize)]
pub struct Consent {
    pub overwrites: usize,
    pub deletes: usize,
    /// Where the backups go.
    pub backups: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    Yes,
    No,
    /// No TTY and no `--yes`: the caller exits as other commands do when
    /// consent can't be asked.
    CannotAsk,
}

/// The decisions of a run: the CLI's flags and prompts, or a test's.
pub trait Decide {
    /// A conflict's answer, from `choices` (never empty; the last is `Skip`).
    fn conflict(&mut self, file: &FilePlan, choices: &[Choice]) -> Choice;
    /// The plan's one question (overwrites or deletes).
    fn confirm(&mut self, consent: &Consent) -> Confirm;
}

/// The game-side guards: the change lock and running refusals, and the
/// `cloud-pull` snapshot. The live one is [`LiveHost`].
pub trait Host {
    /// Take the game's change lock (kept for the run) and refuse while it
    /// runs: [`CloudError::Refused`].
    fn lock(&mut self) -> Result<(), CloudError>;
    /// The `cloud-pull` snapshot, under the lock; the line to show when one
    /// was taken. A failure is [`CloudError::Refused`].
    fn snapshot(&mut self) -> Result<Option<String>, CloudError>;
}

pub struct PullOptions {
    pub dry_run: bool,
    /// `cloud_saves.keep_backups`.
    pub keep_backups: usize,
    pub now: chrono::DateTime<chrono::Utc>,
    /// A test seam: the object name whose write stops after the temp file's
    /// fsync, before the rename, as a crash there would.
    #[doc(hidden)]
    pub stop_before_rename: Option<String>,
}

/// Where a pull reads and writes.
pub struct Inputs<'a> {
    pub game: &'a Game,
    pub client_id: &'a str,
    pub resolved: &'a Resolved,
    pub source: Source,
    pub auth: &'a CloudAuth<'a>,
    pub storage: &'a StorageClient,
    /// `<data_root>/cloud-saves`: the record, its lock and the backups.
    pub record_dir: &'a Path,
}

/// What a pull did, or would do.
#[derive(Debug, Serialize)]
pub struct PullReport {
    pub plan: Plan,
    pub dry_run: bool,
    pub files: Vec<FileResult>,
    /// No TTY and no `--yes` for a plan that overwrites or deletes: nothing
    /// was changed.
    pub needs_consent: bool,
    /// The question was answered no: nothing was changed.
    pub declined: bool,
    /// D11: where the other client's record went.
    pub record_set_aside: Option<PathBuf>,
    pub snapshot: Option<String>,
    pub backup_dir: Option<PathBuf>,
    pub pruned: Vec<PathBuf>,
    pub warnings: Vec<String>,
    pub rolled_back_cleared: bool,
    /// What stopped the run part-way (its exit code wins).
    #[serde(skip)]
    pub error: Option<CloudError>,
    pub stopped: Option<String>,
}

impl PullReport {
    fn count(&self, f: impl Fn(&State) -> bool) -> usize {
        self.files.iter().filter(|r| f(&r.state)).count()
    }
    pub fn left(&self) -> usize {
        self.count(|s| matches!(s, State::Left(_)))
    }
    pub fn corrupt(&self) -> usize {
        self.count(|s| *s == State::Corrupt)
    }
    pub fn unreadable(&self) -> usize {
        self.count(|s| *s == State::Unreadable)
    }
}

/// The cloud side's answer to a conflict: what taking the cloud's state
/// does here. `None`: the cloud has nothing to say (absent, never
/// recorded): the local file is only for a push.
fn cloud_op(d: &Detail) -> Option<Op> {
    let live = d.entry.as_ref().is_some_and(|e| !e.is_tombstone());
    match (live, d.local.is_some()) {
        (true, true) => Some(Op::Overwrite),
        (true, false) => Some(Op::Download),
        (false, true) if d.entry.is_some() || d.record.is_some() => Some(Op::DeleteLocal),
        (false, true) => None,
        (false, false) => Some(Op::Forget),
    }
}

/// The deepest dir gogo may make dirs below for `path`: the prefix's
/// `drive_c`, else the game's files; both must exist.
fn base_for(game: &Game, path: &Path) -> Option<PathBuf> {
    [
        game.layout.prefix.join("drive_c"),
        game.layout.game_files.clone(),
    ]
    .into_iter()
    .filter_map(|b| b.canonicalize().ok())
    .find(|b| path.starts_with(b))
}

fn disk(e: WriteError) -> CloudError {
    match e {
        WriteError::Path { path, reason } => CloudError::Io { path, reason },
        WriteError::Changed { path } => CloudError::Io {
            path,
            reason: "changed since the plan was made".into(),
        },
        WriteError::Io { path, reason } => CloudError::Disk { path, reason },
    }
}

/// One planned file while the run works on it.
struct Work<'a> {
    loc: &'a Location,
    detail: &'a Detail,
    /// [`base_for`] the planned path.
    base: Option<PathBuf>,
    result: FileResult,
}

/// Run a pull of `inputs.game`.
pub async fn pull(
    inputs: &Inputs<'_>,
    opts: &PullOptions,
    host: &mut dyn Host,
    decide: &mut dyn Decide,
) -> Result<PullReport, CloudError> {
    let gogid = inputs.game.id()?;
    let lock = if opts.dry_run {
        None
    } else {
        let l = SyncLock::acquire(inputs.record_dir, gogid).map_err(|e| {
            CloudError::Refused(format!(
                "nothing was changed: another gogo is syncing this game's cloud saves or its \
                 state can't be locked ({e})"
            ))
        })?;
        host.lock()?;
        Some(l)
    };

    let detailed = status::plan_detailed(
        inputs.game,
        inputs.client_id,
        inputs.resolved,
        inputs.source,
        inputs.auth,
        inputs.storage,
        inputs.record_dir,
    )
    .await?;
    let status::Detailed {
        plan,
        details,
        token,
        record: loaded,
    } = detailed;

    // 3. What the pull does with each file; conflicts decided.
    let mut work: Vec<Work> = vec![];
    for (i, lp) in plan.locations.iter().enumerate() {
        let loc = &inputs.resolved.locations[i];
        for (j, fp) in lp.files.iter().enumerate() {
            let d = &details[i][j];
            let (op, choice, state) = settle(fp, d, decide);
            let base = d.path.as_deref().and_then(|p| base_for(inputs.game, p));
            let state = match (op, state) {
                // A save dir the record has files in, gone: not made again.
                (Some(o), State::Planned) if lp.missing && o != Op::Forget && o != Op::Record => {
                    State::Left(LeftReason::LocationMissing)
                }
                (Some(o), State::Planned)
                    if o != Op::Forget && o != Op::Record && base.is_none() =>
                {
                    State::Left(LeftReason::NoPrefix)
                }
                (_, s) => s,
            };
            if fp.action == Action::Nothing {
                continue;
            }
            work.push(Work {
                loc,
                detail: d,
                base,
                result: FileResult {
                    name: fp.name.clone(),
                    planned: fp.action,
                    op,
                    choice,
                    state,
                    backup: None,
                    kept_as: None,
                },
            });
        }
    }

    let backup_root = backup::game_dir(inputs.record_dir, gogid);
    let mut report = PullReport {
        plan,
        dry_run: opts.dry_run,
        files: vec![],
        needs_consent: false,
        declined: false,
        record_set_aside: None,
        snapshot: None,
        backup_dir: None,
        pruned: vec![],
        warnings: vec![],
        rolled_back_cleared: false,
        error: None,
        stopped: None,
    };
    let planned = |w: &Work| w.result.state == State::Planned;
    let destructive = |w: &&Work| planned(w) && w.result.op.is_some_and(Op::destructive);
    let overwrites = work
        .iter()
        .filter(destructive)
        .filter(|w| w.result.op == Some(Op::Overwrite))
        .count();
    let deletes = work
        .iter()
        .filter(destructive)
        .filter(|w| w.result.op == Some(Op::DeleteLocal))
        .count();

    let Some(lock) = lock else {
        report.files = work.into_iter().map(|w| w.result).collect();
        return Ok(report);
    };

    // 4. The one question.
    if overwrites + deletes > 0 {
        match decide.confirm(&Consent {
            overwrites,
            deletes,
            backups: backup_root.clone(),
        }) {
            Confirm::Yes => {}
            c @ (Confirm::No | Confirm::CannotAsk) => {
                report.needs_consent = c == Confirm::CannotAsk;
                report.declined = c == Confirm::No;
                for w in &mut work {
                    if planned(w) {
                        w.result.state = State::NotDone;
                    }
                }
                report.files = work.into_iter().map(|w| w.result).collect();
                return Ok(report);
            }
        }
    }

    // 5. The snapshot, when anything is overwritten or deleted.
    if overwrites + deletes > 0 {
        report.snapshot = host.snapshot()?;
    }

    // 6. Backups of every file that will be overwritten, deleted or kept
    // aside, first.
    let mut bk = Backup::new(backup_root.clone(), opts.now);
    let mut stop: Option<CloudError> = None;
    for w in work
        .iter_mut()
        .filter(|w| planned(w) && w.result.op.is_some_and(Op::backed_up))
    {
        match back_up(w, &mut bk) {
            Ok(p) => w.result.backup = Some(p),
            Err(WriteError::Changed { .. }) => {
                w.result.state = State::Left(LeftReason::ChangedSincePlan)
            }
            Err(WriteError::Path { path, reason }) => {
                report.warnings.push(format!("{path}: {reason}"));
                w.result.state = State::Left(LeftReason::PathRefused)
            }
            Err(e) => {
                stop = Some(disk(e));
                break;
            }
        }
    }
    report.backup_dir = bk.dir().map(Path::to_path_buf);
    if let Some(e) = stop {
        return Ok(stopped(report, work, e));
    }

    // 7. D11: the other client's record set aside, the first write of the
    // record (nothing above wrote it).
    let mut rec = match loaded {
        Some(r) if r.client_id == inputs.client_id => r,
        Some(_) => {
            report.record_set_aside = match lock.set_aside(opts.now) {
                Ok(p) => p,
                Err(e) => return Ok(stopped(report, work, e)),
            };
            report.warnings.push(format!(
                "the game's cloud client changed since the last sync: its old sync record is \
                 kept as {}; this was a first sync",
                report
                    .record_set_aside
                    .as_deref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ));
            Record::new(inputs.client_id)
        }
        None => Record::new(inputs.client_id),
    };

    // 8. The files, in plan order; objects fetched a batch at a time (at
    // most four, and at most `FETCH_BUDGET` bytes unless one object alone is
    // larger), so memory stays bounded by about one batch.
    let stop_at = opts.stop_before_rename.clone();
    let mut wanted: std::collections::VecDeque<(usize, Entry)> = work
        .iter()
        .enumerate()
        .filter_map(|(k, w)| {
            let e = w.detail.entry.clone()?;
            (planned(w) && w.result.op.is_some_and(Op::transfers)).then_some((k, e))
        })
        .collect();
    let mut fetched: std::collections::HashMap<usize, Result<storage::Object, CloudError>> =
        std::collections::HashMap::new();
    for (k, w) in work.iter_mut().enumerate() {
        if !planned(w) {
            continue;
        }
        let op = w.result.op.expect("planned files have an op");
        let object = if op.transfers() {
            while !fetched.contains_key(&k) && !wanted.is_empty() {
                let batch = next_batch(&mut wanted);
                let got = futures::future::join_all(batch.into_iter().map(|(n, e)| {
                    let (storage_c, tok, cid) = (inputs.storage, &token, inputs.client_id);
                    async move { (n, storage::fetch_object(storage_c, tok, cid, &e).await) }
                }))
                .await;
                fetched.extend(got);
            }
            fetched.remove(&k)
        } else {
            None
        };
        let object = match object {
            None => None,
            Some(Ok(o)) => {
                if let Some(bad) = &o.bad_local_last_modified {
                    report.warnings.push(format!(
                        "cloud object {:?}: its uploader's mtime {:?} is out of range; the \
                         file gets the time of the download",
                        w.result.name, bad
                    ));
                }
                Some(o)
            }
            Some(Err(CloudError::Corrupt { name, reason })) => {
                report
                    .warnings
                    .push(format!("cloud object {name:?}: {reason}; not downloaded"));
                w.result.state = State::Corrupt;
                continue;
            }
            Some(Err(e)) => {
                stop = Some(e);
                break;
            }
        };
        let stop_here = stop_at.as_deref() == Some(w.result.name.as_str());
        match apply(
            w,
            op,
            object,
            opts.now,
            &mut rec,
            stop_here,
            &mut report.warnings,
        ) {
            Ok(()) => {
                w.result.state = State::Done;
                if let Err(e) = lock.save(&rec) {
                    stop = Some(e);
                    break;
                }
            }
            Err(Applied::Left(r)) => w.result.state = State::Left(r),
            Err(Applied::Corrupt(msg)) => {
                report.warnings.push(msg);
                w.result.state = State::Corrupt;
            }
            Err(Applied::Stop(e)) => {
                stop = Some(e);
                break;
            }
        }
    }
    if let Some(e) = stop {
        return Ok(stopped(report, work, e));
    }

    // 9. Settled: the record's time, the rollback mark, the backups.
    let settled = work.iter().all(|w| w.result.state == State::Done);
    rec.last_sync = Some(record::datetime(opts.now.into()));
    if rec.rolled_back.is_some() && settled {
        rec.rolled_back = None;
        report.rolled_back_cleared = true;
    }
    if let Err(e) = lock.save(&rec) {
        return Ok(stopped(report, work, e));
    }
    if let Some(current) = bk.dir() {
        let (pruned, warnings) = backup::prune(&backup_root, opts.keep_backups, current);
        report.pruned = pruned;
        report.warnings.extend(warnings);
    }
    report.files = work.into_iter().map(|w| w.result).collect();
    Ok(report)
}

/// The bytes of downloads held in memory at once, beyond one object.
pub const FETCH_BUDGET: u64 = 256 * 1024 * 1024;

/// The next fetches: up to [`status::CONCURRENCY`] objects within
/// [`FETCH_BUDGET`] listed bytes, at least one.
fn next_batch(wanted: &mut std::collections::VecDeque<(usize, Entry)>) -> Vec<(usize, Entry)> {
    let mut batch = vec![];
    let mut bytes = 0u64;
    while let Some((_, e)) = wanted.front() {
        let size = e.bytes.min(storage::MAX_OBJECT_GZIP);
        if !batch.is_empty() && (batch.len() >= status::CONCURRENCY || bytes + size > FETCH_BUDGET)
        {
            break;
        }
        bytes += size;
        batch.push(wanted.pop_front().expect("a front"));
    }
    batch
}

/// A run stopped by `e`: the files not reached say so.
fn stopped(mut report: PullReport, work: Vec<Work>, e: CloudError) -> PullReport {
    report.stopped = Some(e.to_string());
    report.error = Some(e);
    report.files = work
        .into_iter()
        .map(|mut w| {
            if w.result.state == State::Planned {
                w.result.state = State::NotDone;
            }
            w.result
        })
        .collect();
    report
}

/// What the plan's action means for a pull, with conflicts decided.
fn settle(
    fp: &FilePlan,
    d: &Detail,
    decide: &mut dyn Decide,
) -> (Option<Op>, Option<Choice>, State) {
    match fp.action {
        Action::Nothing => (None, None, State::Done),
        Action::Download if d.local.is_some() => (Some(Op::Overwrite), None, State::Planned),
        Action::Download => (Some(Op::Download), None, State::Planned),
        Action::DeleteLocal => (Some(Op::DeleteLocal), None, State::Planned),
        Action::Record => (Some(Op::Record), None, State::Planned),
        Action::Forget => (Some(Op::Forget), None, State::Planned),
        Action::Upload | Action::DeleteCloud => (None, None, State::ForPush),
        Action::Corrupt => (None, None, State::Corrupt),
        Action::Unreadable => (None, None, State::Unreadable),
        Action::Conflict(ConflictReason::CaseClash) => {
            (None, None, State::Left(LeftReason::CaseClash))
        }
        Action::Conflict(ConflictReason::LocationMissing) => {
            (None, None, State::Left(LeftReason::LocationMissing))
        }
        Action::Conflict(_) => {
            let Some(op) = cloud_op(d) else {
                // Only here, never in the cloud: nothing for a pull to decide.
                return (None, None, State::ForPush);
            };
            let mut choices = vec![Choice::Cloud, Choice::Local];
            if op == Op::Overwrite {
                choices.push(Choice::Both);
            }
            choices.push(Choice::Skip);
            let c = decide.conflict(fp, &choices);
            let c = if choices.contains(&c) {
                c
            } else {
                Choice::Skip
            };
            match c {
                Choice::Cloud => (Some(op), Some(c), State::Planned),
                Choice::Both => (Some(Op::KeepBoth), Some(c), State::Planned),
                Choice::Local => (None, Some(c), State::KeptLocal),
                Choice::Skip => (None, Some(c), State::Left(LeftReason::Undecided)),
            }
        }
    }
}

/// The dir and name of a planned file, the path checked again: the same
/// canonical path as planned, not one of the build's files, and its dir
/// opened from `/` without following a link.
fn target(w: &Work) -> Result<(write::Dir, std::ffi::OsString), Applied> {
    let changed = || Applied::Left(LeftReason::ChangedSincePlan);
    let name = &w.result.name;
    let rel = name
        .strip_prefix(&format!("{}/", w.loc.name))
        .ok_or_else(changed)?;
    let planned = w.detail.path.as_deref().ok_or_else(changed)?;
    match w.loc.file(rel) {
        Ok(p) if p == planned && !w.loc.build_path(&p) => {}
        _ => return Err(changed()),
    }
    let (dir, file) = write::split(planned).ok_or_else(changed)?;
    let base = w
        .base
        .as_deref()
        .ok_or(Applied::Left(LeftReason::NoPrefix))?;
    let d = write::open_dir(dir, base).map_err(refuse_or_stop)?;
    d.clear_temps();
    Ok((d, file.to_os_string()))
}

fn back_up(w: &Work, bk: &mut Backup) -> Result<PathBuf, WriteError> {
    let local = w.detail.local.as_ref().ok_or_else(|| WriteError::Changed {
        path: w.result.name.clone(),
    })?;
    let (dir, name) = write::split(&local.path).ok_or_else(|| WriteError::Changed {
        path: w.result.name.clone(),
    })?;
    // Read through the same no-link walk as the writes; no dir is made.
    let d = write::open_dir(dir, dir)?;
    let mut f = d.open_read(name, Expect::of(local))?;
    bk.save(&w.result.name, &mut f)
}

enum Applied {
    Left(LeftReason),
    Corrupt(String),
    Stop(CloudError),
}

/// A write's error for one file: changed or a refused path leaves that
/// file (nothing was written for it), anything else stops the run.
fn refuse_or_stop(e: WriteError) -> Applied {
    match e {
        WriteError::Changed { .. } => Applied::Left(LeftReason::ChangedSincePlan),
        WriteError::Path { .. } => Applied::Left(LeftReason::PathRefused),
        e => Applied::Stop(disk(e)),
    }
}

/// Do one planned file, and record it.
fn apply(
    w: &mut Work,
    op: Op,
    object: Option<storage::Object>,
    now: chrono::DateTime<chrono::Utc>,
    rec: &mut Record,
    stop_here: bool,
    warnings: &mut Vec<String>,
) -> Result<(), Applied> {
    let name = w.result.name.clone();
    let entry: Option<&Entry> = w.detail.entry.as_ref();
    match op {
        Op::Record => {
            let (Some(local), Some(md5), Some(e)) = (&w.detail.local, &w.detail.local_md5, entry)
            else {
                return Err(Applied::Left(LeftReason::ChangedSincePlan));
            };
            rec.files.insert(
                name,
                FileRecord {
                    cloud_hash: e.hash.clone(),
                    local_md5: md5.clone(),
                    size: local.size,
                    mtime: local.mtime,
                },
            );
            Ok(())
        }
        Op::Forget => {
            rec.files.remove(&name);
            Ok(())
        }
        Op::DeleteLocal => {
            let local = w
                .detail
                .local
                .as_ref()
                .ok_or(Applied::Left(LeftReason::ChangedSincePlan))?;
            let (d, file) = target(w)?;
            d.remove(&file, Expect::of(local)).map_err(refuse_or_stop)?;
            rec.files.remove(&name);
            Ok(())
        }
        Op::Download | Op::Overwrite | Op::KeepBoth => {
            let (Some(e), Some(object)) = (entry, object) else {
                return Err(Applied::Left(LeftReason::ChangedSincePlan));
            };
            let (d, file) = target(w)?;
            let expect = match (&w.detail.local, op) {
                (Some(l), Op::Overwrite | Op::KeepBoth) => Expect::of(l),
                (None, Op::Download) => Expect::Absent,
                _ => return Err(Applied::Left(LeftReason::ChangedSincePlan)),
            };
            // `both`: the local file is linked aside only once the cloud's is
            // whole beside it, so a failure leaves it where it was.
            let aside = (op == Op::KeepBoth).then(|| {
                let mut a = file.clone();
                a.push(format!(".local-{}", now.format("%Y%m%dT%H%M%SZ")));
                a
            });
            let mut failed: Option<Fill> = None;
            let mut md5 = md5::Md5::default();
            let written = d
                .replace(
                    &file,
                    expect,
                    object.local_last_modified,
                    aside.as_deref(),
                    &mut |out| {
                        gunzip_to(&name, &object.gzip, out, &mut md5).map_err(|f| {
                            let reason = match &f {
                                Fill::Corrupt(r) => r.clone(),
                                Fill::Disk(e) => e.to_string(),
                            };
                            failed = Some(f);
                            WriteError::Io {
                                path: name.clone(),
                                reason,
                            }
                        })
                    },
                    &|| stop_here,
                )
                .map_err(|err| match (&failed, err) {
                    (Some(Fill::Corrupt(r)), _) => {
                        Applied::Corrupt(format!("cloud object {name:?}: {r}; not downloaded"))
                    }
                    (Some(Fill::Disk(io)), _) => Applied::Stop(CloudError::Disk {
                        path: w
                            .detail
                            .path
                            .as_deref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                        reason: io.to_string(),
                    }),
                    (None, err) => refuse_or_stop(err),
                })?;
            if let Some(m) = written.mtime_error {
                warnings.push(m);
            }
            w.result.kept_as = written
                .kept_as
                .as_ref()
                .map(|k| k.to_string_lossy().into_owned());
            let (size, mtime) = (written.size, written.mtime);
            rec.files.insert(
                name,
                FileRecord {
                    cloud_hash: e.hash.clone(),
                    local_md5: crate::fmt::md5_hex(md5),
                    size,
                    mtime: record::datetime(mtime),
                },
            );
            Ok(())
        }
    }
}

/// Why a download's content couldn't be written.
#[derive(Debug)]
enum Fill {
    /// Not a gzip stream, or a bomb: the object's fault (exit 5, the run
    /// goes on).
    Corrupt(String),
    /// The local write failed (a full disk): the run stops (exit 6).
    Disk(std::io::Error),
}

/// Gunzip `gzip` into `out`, hashing what is written. A failure of `out`
/// itself is [`Fill::Disk`], never taken for a corrupt object.
fn gunzip_to(
    name: &str,
    gzip: &[u8],
    out: &mut dyn std::io::Write,
    md5: &mut md5::Md5,
) -> Result<(), Fill> {
    let mut tee = Tee {
        out,
        md5,
        err: None,
    };
    match storage::gunzip_into(name, gzip, &mut tee, storage::MAX_PLAIN) {
        Ok(_) => Ok(()),
        Err(e) => match tee.err.take() {
            Some(io) => Err(Fill::Disk(io)),
            None => Err(Fill::Corrupt(match e {
                CloudError::Corrupt { reason, .. } => reason,
                e => e.to_string(),
            })),
        },
    }
}

/// Writes through to `out` and the hash; remembers `out`'s own error.
struct Tee<'a> {
    out: &'a mut dyn std::io::Write,
    md5: &'a mut md5::Md5,
    err: Option<std::io::Error>,
}

impl std::io::Write for Tee<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self.out.write(buf) {
            Ok(n) => {
                md5::Digest::update(self.md5, &buf[..n]);
                Ok(n)
            }
            Err(e) => {
                let kind = e.kind();
                self.err = Some(e);
                Err(std::io::Error::from(kind))
            }
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush().inspect_err(|e| {
            self.err = Some(std::io::Error::from(e.kind()));
        })
    }
}

/// The live [`Host`]: the game's change lock (`game-moves/<gogid>.lock`,
/// design 20 §5.1), the running checks of the game's lane, and design 20's
/// `auto` snapshot with trigger `cloud-pull`.
pub struct LiveHost<'a> {
    pub game: &'a Game,
    pub auto: crate::linuxapp::snapshots::AutoOptions,
    pub emit: &'a mut dyn FnMut(&crate::engine::events::ProgressEvent),
    lock: Option<std::fs::File>,
}

impl<'a> LiveHost<'a> {
    pub fn new(
        game: &'a Game,
        auto: crate::linuxapp::snapshots::AutoOptions,
        emit: &'a mut dyn FnMut(&crate::engine::events::ProgressEvent),
    ) -> Self {
        Self {
            game,
            auto,
            emit,
            lock: None,
        }
    }

    fn dirs(&self) -> crate::macapp::launch::GameDirs {
        let mut d = crate::macapp::launch::GameDirs::at(self.game.root.clone());
        d.prefix = self.game.layout.prefix.clone();
        d
    }
}

fn refused(e: impl std::fmt::Display) -> CloudError {
    CloudError::Refused(format!("nothing was changed: {e}"))
}

impl Host for LiveHost<'_> {
    fn lock(&mut self) -> Result<(), CloudError> {
        let gogid = &self.game.gogid;
        self.lock = Some(crate::macapp::relocate::lock_game(gogid).map_err(refused)?);
        match self.game.layout.lane {
            super::locations::Lane::Linux => crate::linuxapp::game::refuse_running(
                gogid,
                &self.game.root,
                &self.game.layout.prefix,
            )
            .map_err(refused),
            super::locations::Lane::Mac => {
                if let Some(pid) = crate::macapp::doctor::live_launcher(&self.game.root) {
                    return Err(refused(format!(
                        "{gogid} is running (launcher {pid}): quit it first"
                    )));
                }
                let procs = crate::macapp::doctor::prefix_procs(&self.game.layout.prefix);
                if !procs.is_empty() {
                    return Err(refused(format!(
                        "{} process(es) still running in {gogid}'s bottle: quit the game first",
                        procs.len()
                    )));
                }
                Ok(())
            }
        }
    }

    fn snapshot(&mut self) -> Result<Option<String>, CloudError> {
        let dirs = self.dirs();
        let taken = crate::linuxapp::snapshots::protect(
            &self.game.gogid,
            &dirs,
            self.lock.as_ref(),
            crate::snapshot::Trigger::CloudPull,
            crate::snapshot::labels::cloud_pull(),
            &self.auto,
            self.emit,
        )
        .map_err(|e| CloudError::Refused(e.to_string()))?;
        Ok(taken.map(|t| format!("snapshot {} taken ({})", t.meta.id, t.meta.label)))
    }
}

#[cfg(test)]
mod tests;
