//! A plan as the app shows it on `plan-card` (design 26 §3.6), and how it comes back
//! from the backend.

use std::any::Any;
use std::fmt;
use std::sync::{Arc, Mutex};

use gogo_contract::PlanFacts;

use crate::error::ErrorView;
use crate::prompt::{PromptAnswer, PromptSpec};
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
    /// Free space where the plan writes, in bytes. It moves on its own, so a plan that
    /// differs only here is still the plan the user saw.
    pub free: Option<u64>,
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

/// What the backend came back with.
#[derive(Debug)]
pub(crate) enum Planning {
    Done(Result<Planned, ErrorView>),
    /// It needs an answer before it can plan.
    Ask(PromptSpec),
}

/// Where the backend puts one plan, from any thread. Dropping it unanswered is a failure.
pub struct PlanReply {
    cell: Arc<Mutex<Option<Planning>>>,
    signal: Arc<Signal>,
    answers: Vec<PromptAnswer>,
    sent: bool,
}

impl PlanReply {
    pub(crate) fn channel(
        signal: Arc<Signal>,
        answers: Vec<PromptAnswer>,
    ) -> (PlanReply, PlanWait) {
        let cell: Arc<Mutex<Option<Planning>>> = Arc::default();
        let wait = PlanWait(Arc::clone(&cell));
        let reply = PlanReply {
            cell,
            signal,
            answers,
            sent: false,
        };
        (reply, wait)
    }

    /// The answers to the questions this planning has [`ask`](Self::ask)ed, in the
    /// order asked.
    pub fn answers(&self) -> &[PromptAnswer] {
        &self.answers
    }

    /// Hand the plan, or why there is none, to the model.
    pub fn send(mut self, result: Result<Planned, ErrorView>) {
        self.put(Planning::Done(result));
    }

    /// There is no plan without an answer to `spec`. The app asks it and plans the
    /// same call again, with the answer at the end of [`answers`](Self::answers).
    /// Cancelling it drops the request, so **plan nothing and store nothing** before
    /// every question is answered and the plan is confirmed.
    ///
    /// Only a choice, a yes or no, or a line of text can be asked this way.
    pub fn ask(mut self, spec: PromptSpec) {
        self.put(Planning::Ask(spec));
    }

    /// Nobody waits for this plan any more: the user cancelled.
    pub fn is_abandoned(&self) -> bool {
        Arc::strong_count(&self.cell) == 1
    }

    fn put(&mut self, result: Planning) {
        self.sent = true;
        *lock(&self.cell) = Some(result);
        self.signal.raise();
    }
}

impl Drop for PlanReply {
    fn drop(&mut self) {
        if !self.sent {
            self.put(Planning::Done(Err(ErrorView::failed(
                "Planning failed",
                "no plan came back",
            ))));
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
pub(crate) struct PlanWait(Arc<Mutex<Option<Planning>>>);

impl PlanWait {
    pub(crate) fn take(&self) -> Option<Planning> {
        lock(&self.0).take()
    }
}
