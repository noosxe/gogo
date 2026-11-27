//! One job's engine events folded into what the job reports: the latest value wins, so
//! nothing queues however fast the events come (design 25 §3).

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gogo::engine::events::ProgressEvent;
use gogo::engine::task::FileTask;
use gogo::engine::{LOCKED_SKIP, sidecar};
use gogo_gui_model::{FileProgress, JobStatus, Step, StepState};

use super::super::text;

/// How far back the rate looks.
const WINDOW: Duration = Duration::from_secs(5);
/// The shortest span a rate is worth saying over.
const LEAST: Duration = Duration::from_secs(1);

/// Bytes per second over the last [`WINDOW`].
#[derive(Default)]
struct Rate {
    samples: VecDeque<(Instant, u64)>,
}

impl Rate {
    fn sample(&mut self, at: Instant, bytes: u64) -> u64 {
        self.samples.push_back((at, bytes));
        while self.samples.len() > 2
            && self
                .samples
                .front()
                .is_some_and(|(t, _)| at.duration_since(*t) > WINDOW)
        {
            self.samples.pop_front();
        }
        let Some(&(from, had)) = self.samples.front() else {
            return 0;
        };
        let span = at.duration_since(from);
        if span < LEAST {
            return 0;
        }
        (bytes.saturating_sub(had) as f64 / span.as_secs_f64()) as u64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Queued,
    Downloading,
    Done,
    Skipped,
    Failed,
}

impl Phase {
    fn word(self) -> &'static str {
        match self {
            Phase::Queued => "queued",
            Phase::Downloading => "downloading",
            Phase::Done => "done",
            Phase::Skipped => "skipped",
            Phase::Failed => "failed",
        }
    }

    fn step(self) -> StepState {
        match self {
            Phase::Queued => StepState::Pending,
            Phase::Downloading => StepState::Now,
            Phase::Done | Phase::Skipped => StepState::Done,
            Phase::Failed => StepState::Failed,
        }
    }
}

struct File {
    name: String,
    dir: PathBuf,
    total: u64,
    /// On disk when this run started.
    before: u64,
    /// What this run has fetched, by the latest count seen.
    fetched: u64,
    /// The size of each chunk, and which are on disk.
    chunks: Vec<u64>,
    done: BTreeSet<u32>,
    phase: Phase,
}

impl File {
    fn bytes(&self) -> u64 {
        match self.phase {
            Phase::Done => self.total,
            _ => {
                // The chunks survive a new link, the count survives a missed event.
                let chunks: u64 = self
                    .done
                    .iter()
                    .filter_map(|&i| self.chunks.get(i as usize))
                    .sum();
                chunks.max(self.before + self.fetched).min(self.total)
            }
        }
    }
}

pub(super) struct Progress {
    files: Vec<File>,
    /// By the task id, as a number.
    by_id: HashMap<u128, usize>,
    rate: Rate,
    /// A file another gogo holds was not downloaded.
    pub(super) locked: bool,
    /// The first file that failed.
    pub(super) failed: Option<String>,
}

impl Progress {
    /// Before anything arrived: `tasks` with what each has on disk.
    pub(super) fn new<'a>(tasks: impl IntoIterator<Item = (&'a FileTask, u64)>) -> Self {
        let mut files = Vec::new();
        let mut by_id = HashMap::new();
        for (task, done) in tasks {
            by_id.insert(task.id.as_u128(), files.len());
            let name = task.entry.name.as_deref().unwrap_or(&task.file.id);
            files.push(File {
                name: text::line(name),
                dir: task.dir.clone(),
                total: task.total_size,
                before: done,
                fetched: 0,
                chunks: Vec::new(),
                done: BTreeSet::new(),
                phase: Phase::Queued,
            });
        }
        Progress {
            files,
            by_id,
            rate: Rate::default(),
            locked: false,
            failed: None,
        }
    }

    fn file(&mut self, id: u128) -> Option<&mut File> {
        let at = *self.by_id.get(&id)?;
        self.files.get_mut(at)
    }

    pub(super) fn apply(&mut self, event: &ProgressEvent) {
        match event {
            ProgressEvent::FileStart {
                id,
                name,
                total_bytes,
                done_bytes,
            } => {
                let Some(file) = self.file(id.as_u128()) else {
                    return;
                };
                let dest = file.dir.join(gogo::engine::select::sanitize(name));
                file.name = text::line(name);
                file.total = *total_bytes;
                file.before = *done_bytes;
                file.fetched = 0;
                file.phase = Phase::Downloading;
                if let Some(plan) = sidecar::load_plan(&dest) {
                    let bits = sidecar::load_bitmap(&dest, plan.chunks.len());
                    file.done = plan
                        .chunks
                        .iter()
                        .filter(|c| sidecar::is_done(&bits, c.id))
                        .map(|c| c.id)
                        .collect();
                    file.chunks = plan
                        .chunks
                        .iter()
                        .map(|c| (c.to + 1).saturating_sub(c.from))
                        .collect();
                }
            }
            ProgressEvent::ChunkDone {
                id,
                chunk,
                file_bytes,
                ..
            } => {
                if let Some(file) = self.file(id.as_u128()) {
                    file.done.insert(*chunk);
                    file.fetched = file.fetched.max(*file_bytes);
                }
            }
            ProgressEvent::FileDone { id, .. } => {
                if let Some(file) = self.file(id.as_u128()) {
                    file.phase = Phase::Done;
                    file.done = (0..file.chunks.len() as u32).collect();
                }
            }
            ProgressEvent::FileSkipped { id, reason } => {
                let locked = reason == LOCKED_SKIP;
                self.locked |= locked;
                // Adopted and already present files are there; the rest are not.
                let there = reason.starts_with("adopted") || reason == "already present";
                if let Some(file) = self.file(id.as_u128()) {
                    file.phase = if there { Phase::Done } else { Phase::Skipped };
                }
            }
            ProgressEvent::FileFailed { id, .. } => {
                if let Some(file) = self.file(id.as_u128()) {
                    file.phase = Phase::Failed;
                    let name = file.name.clone();
                    self.failed.get_or_insert(name);
                }
            }
            _ => {}
        }
    }

    /// Write where the download is, as of `at`, into `status`.
    pub(super) fn report(&mut self, at: Instant, status: &mut JobStatus) {
        // A file that is skipped is not part of what there is to download.
        let counted = || self.files.iter().filter(|f| f.phase != Phase::Skipped);
        let bytes: u64 = counted().map(File::bytes).sum();
        let total: u64 = counted().map(|f| f.total).sum();
        let rate = self.rate.sample(at, bytes);
        status.bytes = bytes;
        status.total = Some(total);
        status.rate = rate;
        status.eta = (rate > 0).then(|| total.saturating_sub(bytes) as f64 / rate as f64);
        status.fraction = (total > 0).then(|| bytes as f64 / total as f64);
        status.steps = self
            .files
            .iter()
            .map(|f| Step {
                name: f.name.clone(),
                state: f.phase.step(),
                took: None,
            })
            .collect();
        status.files = self
            .files
            .iter()
            .map(|f| FileProgress {
                name: f.name.clone(),
                bytes: f.bytes(),
                total: f.total,
                chunks_done: f.done.len() as u32,
                chunks_total: f.chunks.len() as u32,
                state: f.phase.word().to_string(),
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rate_is_over_a_window_and_says_nothing_at_first() {
        let mut rate = Rate::default();
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        assert_eq!(rate.sample(at(0), 0), 0);
        assert_eq!(rate.sample(at(500), 500), 0, "too short to say");
        assert_eq!(rate.sample(at(2000), 2000), 1000);
        // Old samples leave: only the stall is left after a while.
        assert_eq!(rate.sample(at(7000), 7000), 1000);
        assert_eq!(rate.sample(at(12_500), 7000), 0);
    }
}
