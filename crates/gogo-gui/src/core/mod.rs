//! [`CoreBackend`]: the app model over gogo-core (design 25 §2). Only this module and
//! its children speak to the engine.
//!
//! Every [`Backend`] method returns at once: it spawns on the runtime, and what comes
//! back is written to the source's slot, which wakes the UI. Reading only: plans,
//! downloads and the rest come with later PRs.

mod account;
pub(crate) mod art;
mod engine;
mod errors;
mod local;
#[cfg(test)]
mod tests;
mod text;
mod values;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use gogo::config::Config;
use gogo::gog::endpoints::Endpoints;
use gogo_contract::Gates;
use gogo_gui_model::{
    Backend, Call, ErrorView, Failures, Jobs, PlanReply, PromptAnswer, PromptSpec, Run, Slot,
    SourceKey, Subscription,
};
use tokio::runtime::Handle;
use tokio::task::AbortHandle;

pub(crate) use engine::{Engine, Parts};

/// How long an API call may take.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub(crate) enum SetupError {
    #[error("the configuration could not be read: {0}")]
    Config(String),
    #[error("the network client could not be set up")]
    Client,
}

/// The engine as the user has it configured, shared with the CLI.
pub(crate) fn parts() -> Result<Parts, SetupError> {
    // The message can quote a line of the file: name the problem by kind only.
    let loaded = Config::load().map_err(|e| {
        SetupError::Config(match e {
            gogo::config::ConfigError::Io(e) => e.kind().to_string(),
            _ => "`gogo config` shows what is wrong".to_string(),
        })
    })?;
    let cfg = loaded.config;
    let store = gogo::auth::store::open(cfg.auth.token_store);
    let download_dir = PathBuf::from(cfg.download.dir.clone().unwrap_or_default());
    let http = reqwest::Client::builder()
        .user_agent(concat!("gogo/", env!("CARGO_PKG_VERSION")))
        .timeout(TIMEOUT)
        .build()
        .map_err(|_| SetupError::Client)?;
    Ok(Parts {
        account: Arc::new(account::StoredAccount::new(store)),
        local: Arc::new(local::ThisMachine {
            download_dir: download_dir.clone(),
        }),
        http,
        endpoints: Endpoints::default(),
        language: cfg.gog.language,
        download_dir,
    })
}

/// Where decoded covers are kept (design 25 §9).
pub(crate) fn cover_cache() -> Option<PathBuf> {
    match std::env::var_os("GOGO_HOME") {
        Some(home) if !home.is_empty() => Some(PathBuf::from(home).join("gui-cache")),
        _ => Some(gogo::state::cache::cache_dir().join("gui")),
    }
}

/// A source being fed.
struct Fed {
    key: SourceKey,
    slot: Slot,
    /// Gone once the subscription is dropped.
    alive: Weak<Feed>,
}

/// What a subscription holds: dropping it stops the read.
#[derive(Default)]
struct Feed {
    task: Mutex<Option<AbortHandle>>,
}

impl Feed {
    fn replace(&self, task: Option<AbortHandle>) {
        let mut held = self.task.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(old) = std::mem::replace(&mut *held, task) {
            old.abort();
        }
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.replace(None);
    }
}

pub(crate) struct CoreBackend {
    engine: Arc<Engine>,
    runtime: Handle,
    art: Option<art::ArtStore>,
    failures: Option<Failures>,
    fed: Vec<Fed>,
    /// Bumped by a refresh: an older read may no longer write.
    generation: Arc<AtomicU64>,
}

impl CoreBackend {
    pub(crate) fn new(engine: Arc<Engine>, runtime: Handle, art: Option<art::ArtStore>) -> Self {
        CoreBackend {
            engine,
            runtime,
            art,
            failures: None,
            fed: Vec::new(),
            generation: Arc::default(),
        }
    }

    /// Read `key` on the runtime and write it to `slot`.
    fn feed(&self, key: SourceKey, slot: Slot, feed: &Feed) {
        let engine = Arc::clone(&self.engine);
        let failures = self.failures.clone();
        let generation = Arc::clone(&self.generation);
        let mine = generation.load(Ordering::Acquire);
        let task = self.runtime.spawn(async move {
            let (value, errors) = engine.read(&key).await;
            if generation.load(Ordering::Acquire) != mine {
                return;
            }
            slot.set(value);
            if let Some(failures) = failures {
                for error in errors {
                    failures.report(error);
                }
            }
            engine.persist().await;
        });
        feed.replace(Some(task.abort_handle()));
    }

    /// `library.refresh`: read everything again.
    fn refresh(&mut self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.engine.forget();
        if let Some(art) = &self.art {
            art.forget_failures();
        }
        self.fed.retain(|f| f.alive.strong_count() > 0);
        for fed in &self.fed {
            if let Some(feed) = fed.alive.upgrade() {
                self.feed(fed.key.clone(), fed.slot.clone(), &feed);
            }
        }
    }
}

impl Backend for CoreBackend {
    fn attach(&mut self, failures: Failures) {
        self.failures = Some(failures);
    }

    fn subscribe(&mut self, key: &SourceKey, slot: Slot) -> Subscription {
        let feed = Arc::new(Feed::default());
        self.feed(key.clone(), slot.clone(), &feed);
        self.fed.push(Fed {
            key: key.clone(),
            slot,
            alive: Arc::downgrade(&feed),
        });
        Box::new(feed)
    }

    fn gates(&self) -> Gates {
        Gates::default()
    }

    fn rate_limit(&self) -> Option<u64> {
        None
    }

    fn plan(&mut self, _call: &Call, _answer: Option<&PromptAnswer>, reply: PlanReply) {
        reply.send(Err(errors::unavailable()));
    }

    fn prompt_for(&mut self, _call: &Call) -> Result<PromptSpec, ErrorView> {
        Err(errors::unavailable())
    }

    fn run(&mut self, run: Run, _jobs: &mut Jobs<'_>) -> Result<(), ErrorView> {
        match run.call().name() {
            "library.refresh" => {
                self.refresh();
                Ok(())
            }
            _ => Err(errors::unavailable()),
        }
    }
}
