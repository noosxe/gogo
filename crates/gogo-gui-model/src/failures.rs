//! Failures that no call is waiting for (design 25 §6): a source that could not be
//! fetched, or work that failed after [`Backend::run`](crate::Backend::run) had
//! returned. They reach the user as the app's error screen (S9).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::error::ErrorView;
use crate::source::{Signal, lock};

/// How many failures can wait for one pump. Later ones are dropped: the user has
/// enough to read.
const MAX_WAITING: usize = 8;

/// Where a backend reports a failure from any thread.
#[derive(Debug, Clone)]
pub struct Failures {
    waiting: Arc<Mutex<VecDeque<ErrorView>>>,
    signal: Arc<Signal>,
}

impl Failures {
    pub(crate) fn new(signal: Arc<Signal>) -> Self {
        Failures {
            waiting: Arc::default(),
            signal,
        }
    }

    /// Show `error`, which must already be free of credentials (design 07 §4). The
    /// same error twice before a pump counts once.
    pub fn report(&self, error: ErrorView) {
        {
            let mut waiting = lock(&self.waiting);
            if waiting.len() >= MAX_WAITING || waiting.contains(&error) {
                return;
            }
            waiting.push_back(error);
        }
        self.signal.raise();
    }

    pub(crate) fn take(&self) -> VecDeque<ErrorView> {
        std::mem::take(&mut *lock(&self.waiting))
    }
}
