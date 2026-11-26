//! A plan as the app shows it on `plan-card` (design 26 §3.6), and how it comes back
//! from the backend.

use std::any::Any;
use std::fmt;
use std::sync::{Arc, Mutex};

use gogo_contract::PlanFacts;

use crate::error::ErrorView;
use crate::source::{Signal, lock};

/// What one step of a plan does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    /// Something is downloaded.
    Download,
    /// A file is replaced.
    Replace,
    /// Something is removed.
    Delete,
    /// A snapshot is taken.
    Snapshot,
    /// Something moves.
    Move,
    /// A line that changes nothing.
    Info,
}

/// One line of a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStep {
    /// What the step does.
    pub kind: StepKind,
    /// The line to show.
    pub text: String,
    /// The bytes it is about, if any.
    pub bytes: Option<u64>,
}

/// What the user reads and agrees to. The tier is not here: the model decides it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// "Update".
    pub verb: String,
    /// "Update to 1.5.0?".
    pub title: String,
    /// The game's title, which a strong confirmation asks the user to type. Empty when
    /// the plan is not about one game.
    pub subject: String,
    /// What would happen, in order.
    pub steps: Vec<PlanStep>,
    /// Bytes to download.
    pub download: Option<u64>,
    /// Change in disk use, in bytes.
    pub disk_delta: Option<i64>,
    /// What the user should know first.
    pub warnings: Vec<String>,
    /// Whether it can be undone.
    pub reversible: bool,
}

/// Whatever the backend needs to run exactly the plan it computed. Dropping it discards
/// the plan.
pub struct PlanHandle(Box<dyn Any + Send>);

impl PlanHandle {
    /// Wrap what the backend keeps.
    pub fn new(planned: impl Any + Send) -> Self {
        PlanHandle(Box::new(planned))
    }

    /// The value [`new`](Self::new) was given; `None` if it has another type.
    pub fn take<T: Any>(self) -> Option<T> {
        self.0.downcast().ok().map(|b| *b)
    }
}

impl fmt::Debug for PlanHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PlanHandle")
    }
}

/// A computed plan.
#[derive(Debug)]
pub struct Planned {
    /// What the card shows.
    pub plan: Plan,
    /// What the plan contains. `changes` means "there is something to show on a plan
    /// card": a setting the user just picked is not a change by itself.
    pub facts: PlanFacts,
    /// A digest of everything the plan was computed from. Together with the plan and the
    /// facts it tells whether a plan the user saw is still the plan.
    pub inputs: u64,
    /// What runs the plan.
    pub handle: PlanHandle,
}

type PlanResult = Result<Planned, ErrorView>;

/// Where the backend puts one plan, from any thread. Dropping it unanswered is a failure.
pub struct PlanReply {
    cell: Arc<Mutex<Option<PlanResult>>>,
    signal: Arc<Signal>,
    sent: bool,
}

impl PlanReply {
    pub(crate) fn channel(signal: Arc<Signal>) -> (PlanReply, PlanWait) {
        let cell: Arc<Mutex<Option<PlanResult>>> = Arc::default();
        let wait = PlanWait(Arc::clone(&cell));
        let reply = PlanReply {
            cell,
            signal,
            sent: false,
        };
        (reply, wait)
    }

    /// Hand the plan, or why there is none, to the model.
    pub fn send(mut self, result: Result<Planned, ErrorView>) {
        self.put(result);
    }

    /// Nobody waits for this plan any more: the user cancelled.
    pub fn is_abandoned(&self) -> bool {
        Arc::strong_count(&self.cell) == 1
    }

    fn put(&mut self, result: PlanResult) {
        self.sent = true;
        *lock(&self.cell) = Some(result);
        self.signal.raise();
    }
}

impl Drop for PlanReply {
    fn drop(&mut self) {
        if !self.sent {
            self.put(Err(ErrorView::failed(
                "Planning failed",
                "no plan came back",
            )));
        }
    }
}

impl fmt::Debug for PlanReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PlanReply")
    }
}

/// The model's end. Dropping it abandons the request.
#[derive(Debug)]
pub(crate) struct PlanWait(Arc<Mutex<Option<PlanResult>>>);

impl PlanWait {
    pub(crate) fn take(&self) -> Option<PlanResult> {
        lock(&self.0).take()
    }
}
