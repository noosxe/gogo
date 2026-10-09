//! `AppContext` — one per command, shared via `Arc` (`docs/design/01` §3).

use std::sync::Arc;

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::engine::events::ProgressEvent;
use crate::engine::limiter::Limiter;

/// Everything a command needs: config, HTTP client, session, state, rate limiter,
/// event channel, cancellation (docs/design/01 §1/§3).
pub struct AppContext {
    pub cfg: Config,
    pub http: reqwest::Client,
    /// `None` for commands that work without a session (--help, config path, ...).
    pub session: Option<crate::auth::Session>,
    pub state: crate::state::State,
    pub limiter: Arc<Limiter>,
    pub events: broadcast::Sender<ProgressEvent>,
    /// Fed by tokio::signal::ctrl_c(); workers stop at chunk boundaries (exit 130).
    pub cancel: CancellationToken,
}

impl AppContext {
    /// Build once per command (docs/design/01 §3); construction order matters:
    /// config → http → session (restore) → state (lock) → limiter → events → cancel.
    pub async fn build(_cfg: Config) -> anyhow::Result<Self> {
        todo!("docs/design/01 §3 + 06 §2 (state lock)")
    }
}
