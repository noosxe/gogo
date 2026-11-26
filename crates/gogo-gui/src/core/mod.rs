//! [`CoreBackend`]: the app model over gogo-core (design 25 §2). Only this module and
//! its children speak to the engine.
//!
//! Every [`Backend`] method returns at once: it spawns on the runtime, and what comes
//! back is written to the source's slot, which wakes the UI. Reading only: plans,
//! downloads and the rest come with later PRs.

pub(crate) mod account;
pub(crate) mod art;
pub(crate) mod browser;
mod engine;
mod errors;
mod local;
#[cfg(test)]
pub(crate) mod tests;
mod text;
mod values;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use gogo::config::Config;
use gogo::gog::endpoints::Endpoints;
use gogo_contract::{Gates, PlanFacts};
use gogo_gui_model::{
    Backend, Call, ErrorView, Failures, Jobs, Plan, PlanHandle, PlanReply, Planned, PromptAnswer,
    PromptSpec, Run, Slot, SourceKey, Subscription,
};
use tokio::runtime::Handle;
use tokio::task::AbortHandle;
use zeroize::Zeroizing;

pub(crate) use engine::{Engine, Parts};
#[cfg(test)]
pub(crate) use local::{Local, Machine};

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
        browser: Arc::new(browser::SystemBrowser),
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
        let mut held = lock(&self.task);
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

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the backend shares with the work it spawns.
struct Shared {
    engine: Arc<Engine>,
    runtime: Handle,
    art: Option<art::ArtStore>,
    failures: Mutex<Option<Failures>>,
    fed: Mutex<Vec<Fed>>,
    /// Bumped by a refresh: an older read may no longer write.
    generation: AtomicU64,
    signing_in: Arc<AtomicBool>,
}

impl Shared {
    fn report(&self, error: ErrorView) {
        if let Some(failures) = lock(&self.failures).as_ref() {
            failures.report(error);
        }
    }

    /// Read `key` on the runtime and write it to `slot`.
    fn feed(self: &Arc<Self>, key: SourceKey, slot: Slot, feed: &Feed) {
        let shared = Arc::clone(self);
        let mine = self.generation.load(Ordering::Acquire);
        let task = self.runtime.spawn(async move {
            let (value, errors) = shared.engine.read(&key).await;
            if shared.generation.load(Ordering::Acquire) != mine {
                return;
            }
            slot.set(value);
            for error in errors {
                shared.report(error);
            }
            shared.engine.persist().await;
        });
        feed.replace(Some(task.abort_handle()));
    }

    /// Read everything that is being fed again.
    fn refresh(self: &Arc<Self>) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.engine.forget();
        if let Some(art) = &self.art {
            art.forget_failures();
        }
        let mut fed = lock(&self.fed);
        fed.retain(|f| f.alive.strong_count() > 0);
        for f in fed.iter() {
            if let Some(feed) = f.alive.upgrade() {
                self.feed(f.key.clone(), f.slot.clone(), &feed);
            }
        }
    }
}

pub(crate) struct CoreBackend {
    shared: Arc<Shared>,
}

/// What the window needs of sign-in beside the model.
#[derive(Clone)]
pub(crate) struct SignIn {
    engine: Arc<Engine>,
    running: Arc<AtomicBool>,
}

impl SignIn {
    /// A code is being exchanged.
    pub(crate) fn running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// "Open the page again". `false` when no browser could be started.
    pub(crate) fn open_page(&self) -> bool {
        self.engine.open_sign_in_page()
    }
}

/// What the sign-in dialog says. It holds the page's address only when no browser
/// could be started for it; that address holds no secret.
fn sign_in_label(opened: bool) -> String {
    let copy = "Then copy the whole address of the page you end on, and paste it here.";
    if opened {
        format!("Sign in on the GOG page that opened in your browser. {copy}")
    } else {
        format!(
            "No browser could be started. Open this address in one and sign in: {} {copy}",
            gogo::auth::browser::authorize_url()
        )
    }
}

impl CoreBackend {
    pub(crate) fn new(engine: Arc<Engine>, runtime: Handle, art: Option<art::ArtStore>) -> Self {
        CoreBackend {
            shared: Arc::new(Shared {
                engine,
                runtime,
                art,
                failures: Mutex::default(),
                fed: Mutex::default(),
                generation: AtomicU64::new(0),
                signing_in: Arc::default(),
            }),
        }
    }

    pub(crate) fn sign_in(&self) -> SignIn {
        SignIn {
            engine: Arc::clone(&self.shared.engine),
            running: Arc::clone(&self.shared.signing_in),
        }
    }

    fn refuse_sign_in(&self) -> Option<ErrorView> {
        let busy = |title: &str| Some(ErrorView::usage(title, "There is nothing to do."));
        if self.shared.signing_in.load(Ordering::Acquire) {
            busy("A sign-in is already running")
        } else if self.shared.engine.signed_in() {
            busy("Already signed in")
        } else {
            self.shared.engine.cannot_sign_in()
        }
    }

    /// Exchange the pasted code on the runtime. What fails comes back as a failure.
    fn start_sign_in(&mut self, answer: Option<&PromptAnswer>) -> Result<(), ErrorView> {
        let Some(PromptAnswer::Secret(code)) = answer else {
            return Err(errors::unavailable());
        };
        if let Some(refused) = self.refuse_sign_in() {
            return Err(refused);
        }
        let code = Zeroizing::new(code.expose().to_string());
        let shared = Arc::clone(&self.shared);
        shared.signing_in.store(true, Ordering::Release);
        self.shared.runtime.spawn(async move {
            let done = shared.engine.sign_in(code).await;
            shared.signing_in.store(false, Ordering::Release);
            match done {
                Ok(()) => shared.refresh(),
                Err(error) => {
                    tracing::warn!("{}: {}", error.title, error.message);
                    shared.report(error);
                }
            }
        });
        Ok(())
    }
}

impl Backend for CoreBackend {
    fn attach(&mut self, failures: Failures) {
        *lock(&self.shared.failures) = Some(failures);
    }

    fn subscribe(&mut self, key: &SourceKey, slot: Slot) -> Subscription {
        let feed = Arc::new(Feed::default());
        self.shared.feed(key.clone(), slot.clone(), &feed);
        let mut fed = lock(&self.shared.fed);
        fed.retain(|f| f.alive.strong_count() > 0);
        fed.push(Fed {
            key: key.clone(),
            slot,
            alive: Arc::downgrade(&feed),
        });
        drop(fed);
        Box::new(feed)
    }

    fn gates(&self) -> Gates {
        Gates::default()
    }

    fn rate_limit(&self) -> Option<u64> {
        None
    }

    fn plan(&mut self, call: &Call, _answer: Option<&PromptAnswer>, reply: PlanReply) {
        if call.name() != "account.sign_in" {
            reply.send(Err(errors::unavailable()));
            return;
        }
        // Nothing to show on a card: facts that report a change would make the model
        // ask for a confirmation, and the user has just given the answer.
        reply.send(Ok(Planned {
            plan: Plan {
                verb: "Sign in".to_string(),
                title: "Sign in".to_string(),
                subject: String::new(),
                steps: Vec::new(),
                download: None,
                disk_delta: None,
                warnings: Vec::new(),
                reversible: true,
            },
            facts: PlanFacts::nothing_to_do(),
            inputs: 0,
            handle: PlanHandle::new(()),
        }));
    }

    fn prompt_for(&mut self, call: &Call) -> Result<PromptSpec, ErrorView> {
        if call.name() != "account.sign_in" {
            return Err(errors::unavailable());
        }
        if let Some(refused) = self.refuse_sign_in() {
            return Err(refused);
        }
        let opened = self.shared.engine.open_sign_in_page();
        Ok(PromptSpec::Secret {
            label: sign_in_label(opened),
        })
    }

    fn run(&mut self, run: Run, _jobs: &mut Jobs<'_>) -> Result<(), ErrorView> {
        match run.call().name() {
            "library.refresh" => {
                self.shared.refresh();
                Ok(())
            }
            "account.sign_in" => self.start_sign_in(run.answer()),
            _ => Err(errors::unavailable()),
        }
    }
}
