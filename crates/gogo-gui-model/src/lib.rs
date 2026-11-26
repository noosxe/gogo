//! The app layer (design 25): latest-value sources and the job table. Depends only on
//! `gogo-contract`: no IR, skin, renderer, window or async runtime (design 24 §2.1
//! rule 5).
//!
//! - [`source`]: one latest-value slot per source key and one wake per frame (§3).
//! - [`jobs`]: the job table, with progress and a control handle per job (§5).
//! - [`error`]: [`ErrorView`], the one shape every failure takes (§6).
#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod jobs;
pub mod source;

pub use error::ErrorView;
pub use jobs::{
    FileProgress, JobControl, JobId, JobKind, JobSink, JobState, JobStatus, JobTable, Jobs,
    MAX_ENDED, Step, StepState, Transition,
};
pub use source::{Signal, Slot, SourceKey, SourceStore};
