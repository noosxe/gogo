//! The only thing the model knows about an engine (design 25 §2). `gogo-gui` implements
//! it over gogo-core; tests use the `fake` feature.

use std::any::Any;

use gogo_contract::Gates;

use crate::call::Call;
use crate::error::ErrorView;
use crate::jobs::Jobs;
use crate::plan::{PlanHandle, PlanReply};
use crate::prompt::{PromptAnswer, PromptSpec};
use crate::source::{Slot, SourceKey};

/// Feeds one source until it is dropped.
pub type Subscription = Box<dyn Any + Send>;

/// Every method returns at once.
pub trait Backend {
    /// Start feeding a source; values arrive through the slot.
    fn subscribe(&mut self, key: &SourceKey, slot: Slot) -> Subscription;

    /// The settings that gate actions, as they are now.
    fn gates(&self) -> Gates;

    /// The download rate limit in bytes per second, for `queue.summary`.
    fn rate_limit(&self) -> Option<u64>;

    /// Compute what `call` would do and **change nothing**. `answer` is the prompt's, for
    /// an action that asks first.
    fn plan(&mut self, call: &Call, answer: Option<&PromptAnswer>, reply: PlanReply);

    /// What an action of kind `prompt` asks.
    ///
    /// # Errors
    /// The action is refused.
    fn prompt_for(&mut self, call: &Call) -> Result<PromptSpec, ErrorView>;

    /// Carry out an immediate action or a confirmed plan. Add a job to `jobs` for
    /// everything that reports back: one for most runs, one per item of a batch.
    ///
    /// A [`Run`] with a plan handle runs **that** plan; it never plans again.
    ///
    /// # Errors
    /// The action is refused. Jobs already added are cancelled.
    fn run(&mut self, run: Run, jobs: &mut Jobs<'_>) -> Result<(), ErrorView>;
}

/// Leave to run something. Only the safety flow makes one, so nothing else can reach
/// [`Backend::run`].
#[derive(Debug)]
pub struct Run {
    call: Call,
    answer: Option<PromptAnswer>,
    plan: Option<PlanHandle>,
}

impl Run {
    pub(crate) fn new(call: Call, answer: Option<PromptAnswer>, plan: Option<PlanHandle>) -> Self {
        Run { call, answer, plan }
    }

    pub fn call(&self) -> &Call {
        &self.call
    }

    pub fn answer(&self) -> Option<&PromptAnswer> {
        self.answer.as_ref()
    }

    /// The handle of the plan to run, if the action was planned.
    pub fn into_plan(self) -> Option<PlanHandle> {
        self.plan
    }
}
