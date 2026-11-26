//! The job table (design 25 §5): one entry per running action, each with its own
//! latest-value progress and its own control handle, so cancelling one job leaves the
//! others running.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use gogo_contract::Value;

use crate::error::ErrorView;
use crate::source::{Signal, lock};

/// A job's id. The model assigns it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(String);

impl JobId {
    /// The id a skin passed back to a `job.*` action. It may name no job.
    pub fn new(id: impl Into<String>) -> Self {
        JobId(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

macro_rules! words {
    ($(#[$doc:meta])* $name:ident { $($variant:ident => $word:literal),+ $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const ALL: &[$name] = &[$($name::$variant),+];

            /// The word a skin reads.
            pub const fn word(self) -> &'static str {
                match self {
                    $($name::$variant => $word),+
                }
            }
        }
    };
}

words!(
    /// What a job does.
    JobKind {
        Download => "download",
        Depot => "depot",
        Update => "update",
        Setup => "setup",
        DlcAdd => "dlc_add",
        DlcRemove => "dlc_remove",
        Saves => "saves",
        RuntimeInstall => "runtime_install",
        Move => "move",
        Rollback => "rollback",
        Verify => "verify",
    }
);

words!(
    /// Where a job is.
    JobState {
        Queued => "queued",
        Running => "running",
        Paused => "paused",
        Failed => "failed",
        Done => "done",
        Cancelled => "cancelled",
    }
);

words!(
    /// Where one step of a job is.
    StepState {
        Done => "done",
        Now => "now",
        Pending => "pending",
        Failed => "failed",
        AsksFirst => "asks_first",
    }
);

impl JobState {
    /// Nothing more is reported once a job is here, unless it is resumed or retried.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Failed | Self::Done | Self::Cancelled)
    }
}

/// One step of a job.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub name: String,
    pub state: StepState,
    /// Seconds it took, once done.
    pub took: Option<f64>,
}

/// One file of a job.
#[derive(Debug, Clone, PartialEq)]
pub struct FileProgress {
    pub name: String,
    pub bytes: u64,
    pub total: u64,
    pub chunks_done: u32,
    pub chunks_total: u32,
    pub state: String,
}

/// Everything a job reports. Each report replaces the one before.
#[derive(Debug, Clone, PartialEq)]
pub struct JobStatus {
    pub kind: JobKind,
    /// The game's `GameSummary` record, if the job is about a game.
    pub game: Option<Value>,
    pub state: JobState,
    pub steps: Vec<Step>,
    pub bytes: u64,
    pub total: Option<u64>,
    /// Bytes per second.
    pub rate: u64,
    /// Seconds left.
    pub eta: Option<f64>,
    /// 0.0–1.0.
    pub fraction: Option<f64>,
    pub files: Vec<FileProgress>,
    pub error: Option<ErrorView>,
}

impl JobStatus {
    /// A job that hasn't started.
    pub fn queued(kind: JobKind) -> Self {
        JobStatus {
            kind,
            game: None,
            state: JobState::Queued,
            steps: Vec::new(),
            bytes: 0,
            total: None,
            rate: 0,
            eta: None,
            fraction: None,
            files: Vec::new(),
            error: None,
        }
    }

    /// The record a skin reads (the contract's `Job` shape).
    pub fn to_value(&self, id: &JobId) -> Value {
        let num = |n: u64| Value::Num(n as f64);
        let opt = |v: Option<Value>| v.unwrap_or(Value::Null);
        let steps = self.steps.iter().map(|s| {
            record([
                ("name", Value::str(&s.name)),
                ("state", Value::str(s.state.word())),
                ("took", opt(s.took.map(Value::Num))),
            ])
        });
        let files = self.files.iter().map(|f| {
            record([
                ("name", Value::str(&f.name)),
                ("bytes", num(f.bytes)),
                ("total", num(f.total)),
                ("chunks_done", num(f.chunks_done.into())),
                ("chunks_total", num(f.chunks_total.into())),
                ("state", Value::str(&f.state)),
            ])
        });
        record([
            ("id", Value::str(id.as_str())),
            ("kind", Value::str(self.kind.word())),
            ("game", opt(self.game.clone())),
            ("state", Value::str(self.state.word())),
            ("steps", Value::List(steps.collect())),
            ("bytes", num(self.bytes)),
            ("total", opt(self.total.map(num))),
            ("rate", num(self.rate)),
            ("eta", opt(self.eta.map(Value::Num))),
            ("fraction", opt(self.fraction.map(Value::Num))),
            ("files", Value::List(files.collect())),
            ("error", opt(self.error.as_ref().map(ErrorView::to_value))),
        ])
    }
}

fn record<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::Map(BTreeMap::from(fields.map(|(k, v)| (k.to_owned(), v))))
}

/// What the backend gives the table to steer one job. Every method returns at once; the
/// outcome arrives through the job's [`JobSink`].
pub trait JobControl: Send {
    /// Stop the job, leaving it resumable.
    fn cancel(&mut self);
    fn pause(&mut self);
    /// Carry on a paused or cancelled job, reporting to `sink` from now on.
    fn resume(&mut self, sink: JobSink);
    /// Run a failed job again, reporting to `sink`.
    fn retry(&mut self, sink: JobSink);
}

#[derive(Debug)]
struct Latest {
    /// Which run of the job may write: `resume` and `retry` start a new one.
    run: u64,
    version: u64,
    status: JobStatus,
}

/// The writer's end of one job's progress. Cheap to clone and `Send`.
#[derive(Debug, Clone)]
pub struct JobSink {
    latest: Arc<Mutex<Latest>>,
    signal: Arc<Signal>,
    run: u64,
}

impl JobSink {
    /// Replace the job's status. Returns `false`, changing nothing, once the job has
    /// ended or this sink belongs to an earlier run: a late progress report can't hide a
    /// failure.
    pub fn report(&self, status: JobStatus) -> bool {
        {
            let mut latest = lock(&self.latest);
            if latest.run != self.run || latest.status.state.is_terminal() {
                return false;
            }
            latest.version += 1;
            latest.status = status;
        }
        self.signal.raise();
        true
    }
}

struct Entry {
    id: JobId,
    /// Listed in `queue`. Work that finishes at once is tracked but not listed, so its
    /// failure is still seen.
    listed: bool,
    latest: Arc<Mutex<Latest>>,
    control: Box<dyn JobControl>,
    seen: u64,
    /// As of the last drain.
    status: JobStatus,
}

impl fmt::Debug for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Entry")
            .field("id", &self.id)
            .field("listed", &self.listed)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

/// A job whose state moved between two drains. States passed through in between are not
/// reported; a terminal state always is.
#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    pub id: JobId,
    pub listed: bool,
    pub from: JobState,
    pub to: JobState,
    pub error: Option<ErrorView>,
}

/// Every job, oldest first.
#[derive(Debug)]
pub struct JobTable {
    signal: Arc<Signal>,
    entries: Vec<Entry>,
    next: u64,
}

impl JobTable {
    pub fn new(signal: Arc<Signal>) -> Self {
        JobTable {
            signal,
            entries: Vec::new(),
            next: 0,
        }
    }

    /// The handle a backend adds the jobs of one run through.
    pub fn jobs(&mut self, listed: bool) -> Jobs<'_> {
        Jobs {
            table: self,
            listed,
            added: Vec::new(),
        }
    }

    fn add(&mut self, listed: bool, status: JobStatus, control: Box<dyn JobControl>) -> JobSink {
        self.next += 1;
        let latest = Arc::new(Mutex::new(Latest {
            run: 0,
            version: 0,
            status: status.clone(),
        }));
        self.entries.push(Entry {
            id: JobId(format!("job-{}", self.next)),
            listed,
            latest: Arc::clone(&latest),
            control,
            seen: 0,
            status,
        });
        self.signal.raise();
        JobSink {
            latest,
            signal: Arc::clone(&self.signal),
            run: 0,
        }
    }

    fn find(&mut self, id: &JobId) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.id == *id)
    }

    /// Cancel one job. A job that has already ended is left alone. `false` if there is no
    /// such job.
    pub fn cancel(&mut self, id: &JobId) -> bool {
        let Some(e) = self.find(id) else {
            return false;
        };
        let ended = lock(&e.latest).status.state.is_terminal();
        if !ended {
            e.control.cancel();
        }
        true
    }

    /// Pause a queued or running job. `false` if there is none to pause.
    pub fn pause(&mut self, id: &JobId) -> bool {
        let Some(e) = self.find(id) else {
            return false;
        };
        let state = lock(&e.latest).status.state;
        let ok = matches!(state, JobState::Queued | JobState::Running);
        if ok {
            e.control.pause();
        }
        ok
    }

    /// Carry on a paused or cancelled job. `false` if there is none to resume.
    pub fn resume(&mut self, id: &JobId) -> bool {
        self.restart(id, &[JobState::Paused, JobState::Cancelled], |c, s| {
            c.resume(s)
        })
    }

    /// Run a failed job again. `false` if there is none to retry.
    pub fn retry(&mut self, id: &JobId) -> bool {
        self.restart(id, &[JobState::Failed], |c, s| c.retry(s))
    }

    fn restart(
        &mut self,
        id: &JobId,
        from: &[JobState],
        start: impl FnOnce(&mut dyn JobControl, JobSink),
    ) -> bool {
        let signal = Arc::clone(&self.signal);
        let Some(e) = self.find(id) else {
            return false;
        };
        let run = {
            let mut latest = lock(&e.latest);
            if !from.contains(&latest.status.state) {
                return false;
            }
            latest.run += 1;
            latest.version += 1;
            latest.status.state = JobState::Queued;
            latest.status.error = None;
            latest.run
        };
        let sink = JobSink {
            latest: Arc::clone(&e.latest),
            signal: Arc::clone(&signal),
            run,
        };
        start(e.control.as_mut(), sink);
        signal.raise();
        true
    }

    /// Take the newest status of every job written since the last call. Unlisted jobs
    /// that have ended are dropped after they are reported. The caller takes the
    /// [`Signal`] first.
    pub fn drain(&mut self) -> Vec<Transition> {
        let mut moved = Vec::new();
        for e in &mut self.entries {
            let latest = lock(&e.latest);
            if latest.version == e.seen {
                continue;
            }
            e.seen = latest.version;
            let from = e.status.state;
            e.status = latest.status.clone();
            if from != e.status.state {
                moved.push(Transition {
                    id: e.id.clone(),
                    listed: e.listed,
                    from,
                    to: e.status.state,
                    error: e.status.error.clone(),
                });
            }
        }
        self.entries
            .retain(|e| e.listed || !e.status.state.is_terminal());
        moved
    }

    fn listed(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.listed)
    }

    /// A listed job's status as of the last drain.
    pub fn status(&self, id: &JobId) -> Option<&JobStatus> {
        self.listed().find(|e| e.id == *id).map(|e| &e.status)
    }

    /// The listed jobs, oldest first.
    pub fn ids(&self) -> impl Iterator<Item = &JobId> {
        self.listed().map(|e| &e.id)
    }

    /// The `queue` source: `List<Job>`.
    pub fn queue_value(&self) -> Value {
        Value::List(self.listed().map(|e| e.status.to_value(&e.id)).collect())
    }

    /// The `job(id)` source.
    pub fn job_value(&self, id: &JobId) -> Option<Value> {
        self.status(id).map(|s| s.to_value(id))
    }

    /// The `queue.summary` source. `rate_limit` is the backend's.
    pub fn summary_value(&self, rate_limit: Option<u64>) -> Value {
        let in_state = |s: JobState| self.listed().filter(move |e| e.status.state == s);
        let rate: u64 = in_state(JobState::Running).map(|e| e.status.rate).sum();
        record([
            (
                "active",
                Value::Num(in_state(JobState::Running).count() as f64),
            ),
            (
                "queued",
                Value::Num(in_state(JobState::Queued).count() as f64),
            ),
            ("rate", Value::Num(rate as f64)),
            (
                "rate_limit",
                rate_limit.map_or(Value::Null, |r| Value::Num(r as f64)),
            ),
            (
                "current",
                in_state(JobState::Running)
                    .next()
                    .map_or(Value::Null, |e| e.status.to_value(&e.id)),
            ),
        ])
    }
}

/// Adds the jobs of one run: usually one, or one per item of a batch.
#[derive(Debug)]
pub struct Jobs<'a> {
    table: &'a mut JobTable,
    listed: bool,
    added: Vec<JobId>,
}

impl Jobs<'_> {
    /// Add a job that `control` steers, and get the sink it reports to.
    pub fn add(&mut self, status: JobStatus, control: Box<dyn JobControl>) -> JobSink {
        let sink = self.table.add(self.listed, status, control);
        if let Some(e) = self.table.entries.last() {
            self.added.push(e.id.clone());
        }
        sink
    }

    /// The jobs added so far.
    pub fn added(&self) -> &[JobId] {
        &self.added
    }

    /// Cancel and remove every job added through this handle: the run failed.
    pub fn abandon(self) {
        for id in &self.added {
            self.table.cancel(id);
        }
        self.table.entries.retain(|e| !self.added.contains(&e.id));
        self.table.signal.raise();
    }
}
