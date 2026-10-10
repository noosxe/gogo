//! `AppContext` — one per command, shared via `Arc` (`docs/design/01` §3).

use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::engine::FreeSpace;
use crate::engine::events::ProgressEvent;
use crate::engine::limiter::GlobalLimiter;

/// Everything a command needs: config, HTTP client, session, state, rate limiter,
/// event channel, cancellation (docs/design/01` §1/§3).
pub struct AppContext {
    pub cfg: Config,
    pub http: reqwest::Client,
    /// `None` for commands that work without a session (--help, config path, ...).
    pub session: Option<Arc<dyn crate::auth::TokenProvider>>,
    /// Shared across parallel file tasks; the state file's own advisory lock (B5)
    /// serializes the on-disk read-modify-write.
    pub state: Mutex<crate::state::State>,
    pub limiter: Arc<GlobalLimiter>,
    pub events: broadcast::Sender<ProgressEvent>,
    /// Fed by tokio::signal::ctrl_c(); workers stop at chunk boundaries (exit 130).
    pub cancel: CancellationToken,
    /// `--no-disk-check` disables the preflight (03 §3).
    pub disk_check: bool,
    /// D4: free-space probe behind a trait (mockable in tests).
    pub free_space: Arc<dyn FreeSpace>,
}

impl AppContext {
    /// Build once per command (docs/design/01` §3); construction order matters:
    /// config → http → session (restore) → state (load) → limiter → events → cancel.
    ///
    /// A6: `with_session` restores tokens from the **configured** backend — no
    /// fallback. Keyring configured but unavailable fails loudly here.
    pub async fn build(cfg: Config, with_session: bool) -> anyhow::Result<Self> {
        let http = crate::gog::http()?;
        let session: Option<Arc<dyn crate::auth::TokenProvider>> = if with_session {
            let store = crate::auth::store::open(cfg.auth.token_store);
            Some(Arc::new(
                crate::auth::Session::restore(store.as_ref()).await?,
            ))
        } else {
            None
        };
        let state = crate::state::State::load()?;
        let limiter = Arc::new(GlobalLimiter::new(cfg.download.limit));
        let (events, _rx) = crate::engine::events::channel();
        Ok(Self {
            cfg,
            http,
            session,
            state: Mutex::new(state),
            limiter,
            events,
            cancel: CancellationToken::new(),
            disk_check: true,
            free_space: Arc::new(crate::engine::Fs4FreeSpace),
        })
    }
}
