//! One job's own cancel token and event channel (design 25 §5), so a frontend that
//! runs several downloads can stop and follow each by itself.

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::app::AppContext;
use crate::engine::events::ProgressEvent;

/// Made by [`AppContext::job`] only, so cancelling the app token stops every job.
#[derive(Debug, Clone)]
pub struct JobScope {
    cancel: CancellationToken,
    events: broadcast::Sender<ProgressEvent>,
}

impl JobScope {
    /// The command's own token and channel: the whole process is the one job.
    pub(crate) fn command(ctx: &AppContext) -> Self {
        Self {
            cancel: ctx.cancel.clone(),
            events: ctx.events.clone(),
        }
    }

    pub(crate) fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }

    pub(crate) fn events(&self) -> &broadcast::Sender<ProgressEvent> {
        &self.events
    }

    /// Cancel it to stop this job and no other. It stays cancelled: resuming takes
    /// a new job. Pass it to the entry points that take a token
    /// ([`crate::gog::depot::run`]), or ask it from a pipeline's `stop` closure.
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// This job's events alone. Subscribe before starting it: earlier events are
    /// not replayed.
    pub fn subscribe(&self) -> broadcast::Receiver<ProgressEvent> {
        self.events.subscribe()
    }
}

impl AppContext {
    /// A new job: a child of [`AppContext::cancel`] and a channel of its own.
    pub fn job(&self) -> JobScope {
        JobScope {
            cancel: self.cancel.child_token(),
            events: crate::engine::events::channel().0,
        }
    }
}

/// `fut`'s output, or `None` once `cancel` is (`fut` is then dropped).
pub(crate) async fn unless_cancelled<T>(
    cancel: &CancellationToken,
    fut: impl Future<Output = T>,
) -> Option<T> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => None,
        out = fut => Some(out),
    }
}
