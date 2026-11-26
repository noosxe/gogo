//! The app layer (design 25): latest-value sources, action dispatch, the safety flow and
//! the job table, behind a [`Backend`] trait. Depends only on `gogo-contract`: no IR,
//! skin, renderer, window or async runtime (design 24 §2.1 rule 5). `gogo-gui` translates
//! between this crate's types and the engine's.
//!
//! - [`model`]: [`Model`], what the shell drives.
//! - [`safety`]: the only path from an action to the backend (§4).
//! - [`call`]: what a skin asks for, checked against the contract.
//! - [`backend`], [`plan`], [`prompt`]: the seam to the engine (§2).
//! - [`source`]: one latest-value slot per source key and one wake per frame (§3).
//! - [`jobs`]: the job table, with progress and a control handle per job (§5).
//! - [`error`]: [`ErrorView`], the one shape every failure takes (§6).
//! - `fake` (feature `fake`): a scripted backend for tests.
#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod backend;
pub mod call;
pub mod error;
#[cfg(feature = "fake")]
pub mod fake;
pub mod jobs;
pub mod model;
pub mod plan;
pub mod prompt;
pub mod safety;
pub mod source;

pub use backend::{Backend, Run, Subscription};
pub use call::{Arg, Call, Invocation};
pub use error::ErrorView;
pub use jobs::{
    FileProgress, JobControl, JobId, JobKind, JobSink, JobState, JobStatus, JobTable, Jobs,
    MAX_ENDED, Step, StepState, Transition,
};
pub use model::{Location, Model};
pub use plan::{Plan, PlanHandle, PlanReply, PlanStep, Planned, StepKind};
pub use prompt::{ConflictFile, PromptAnswer, PromptReply, PromptSpec, Secret, Side};
pub use safety::{
    DWELL, Effect, HOLD, InputEvent, Modal, Outcome, Press, Preview, Route, SafetyFlow, Strength,
    Ticket, Toast,
};
pub use source::{Signal, Slot, SourceKey, SourceStore};
