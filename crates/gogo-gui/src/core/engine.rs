//! Reading gogo-core and shaping what it says into sources (design 25 §2). Every method
//! here runs on the runtime, never on the UI thread.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use gogo::auth::TokenProvider;
use gogo::config::Config;
use gogo::engine::FreeSpace;
use gogo::engine::select::{self, Filters};
use gogo::engine::task::Kind;
use gogo::gog::catalog::{Os, Product};
use gogo::gog::endpoints::Endpoints;
use gogo::gog::error::{ApiError, AuthError};
use gogo::gog::library::LibraryProduct;
use gogo::gog::overview::Overview;
use gogo_contract::Value;
use gogo_gui_model::{ErrorView, SourceKey};
use tokio::sync::OnceCell;
use zeroize::Zeroizing;

use super::account::Account;
use super::browser::Browser;
use super::errors;
use super::install::Pipelines;
use super::local::{Local, Machine};
use super::text;
use super::values;
use super::values::{art, line, list, map, num, opt, os_word, str};

/// What the engine is made of; tests swap every part that reaches outside.
pub(crate) struct Parts {
    pub account: Arc<dyn Account>,
    pub local: Arc<dyn Local>,
    pub http: reqwest::Client,
    pub endpoints: Endpoints,
    /// The preferred installer language (config `gog.language`).
    pub language: String,
    pub download_dir: PathBuf,
    pub browser: Arc<dyn Browser>,
    /// The configuration downloads run with; `download.dir` is [`Parts::download_dir`].
    pub config: Config,
    pub free_space: Arc<dyn FreeSpace>,
    /// The install pipelines.
    pub pipelines: Arc<dyn Pipelines>,
}

/// The owned games and what is set up of them.
struct Library {
    products: Vec<LibraryProduct>,
    machine: Machine,
}

/// Who is signed in, and their games.
struct Snapshot {
    username: Option<String>,
    signed_in: bool,
    library: Library,
    /// What went wrong while reading, until a source takes it.
    failures: Mutex<Vec<ErrorView>>,
}

impl Snapshot {
    /// The first source to ask reports them: one screen, not one per source.
    fn take_failures(&self) -> Vec<ErrorView> {
        std::mem::take(&mut *lock(&self.failures))
    }
}

type Cell<T> = Arc<OnceCell<Arc<T>>>;

pub(crate) struct Engine {
    parts: Parts,
    /// Replaced by [`Engine::forget`].
    snapshot: Mutex<Cell<Snapshot>>,
    overviews: Mutex<HashMap<u64, Cell<Option<Overview>>>>,
    /// One token refresh at a time: gog-warp has no lock of its own.
    refresh: tokio::sync::Mutex<()>,
    /// What the last read of the account found.
    signed_in: AtomicBool,
    /// Why a sign-in could not be kept.
    cannot_sign_in: Mutex<Option<ErrorView>>,
}

/// A source's value, and what to tell the user if it could not be read.
pub(crate) type Read = (Value, Vec<ErrorView>);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Engine {
    pub(crate) fn new(parts: Parts) -> Self {
        Engine {
            parts,
            snapshot: Mutex::default(),
            overviews: Mutex::default(),
            refresh: tokio::sync::Mutex::new(()),
            signed_in: AtomicBool::new(false),
            cannot_sign_in: Mutex::new(None),
        }
    }

    /// The configuration of the download context.
    pub(super) fn config(&self) -> &Config {
        &self.parts.config
    }

    pub(super) fn free_space(&self) -> Arc<dyn FreeSpace> {
        Arc::clone(&self.parts.free_space)
    }

    pub(super) fn pipelines(&self) -> Arc<dyn Pipelines> {
        Arc::clone(&self.parts.pipelines)
    }

    /// A current access token of whoever is signed in.
    pub(super) async fn access_token(&self) -> Result<String, AuthError> {
        let Some(session) = self.parts.account.session().await? else {
            return Err(AuthError::Unauthorized);
        };
        let _one = self.refresh.lock().await;
        session.bearer().await
    }

    /// The owned product ids; none when signed out.
    pub(super) async fn owned_ids(&self) -> Result<Vec<u64>, ApiError> {
        let Some(session) = self.parts.account.session().await? else {
            return Ok(Vec::new());
        };
        let bearer = self.bearer(session.as_ref()).await?;
        self.parts
            .endpoints
            .owned_ids(&self.parts.http, &bearer)
            .await
    }

    /// The `GameSummary` a job about `id` shows, from what was last read.
    pub(super) fn job_game(&self, id: u64, title: &str) -> Value {
        self.game_as(id, title, "downloading")
    }

    /// [`job_game`](Self::job_game) for a set-up.
    pub(super) fn setup_game(&self, id: u64, title: &str) -> Value {
        self.game_as(id, title, "setting_up")
    }

    fn game_as(&self, id: u64, title: &str, status: &str) -> Value {
        let cell = Arc::clone(&lock(&self.snapshot));
        let none = Machine::default();
        let machine = cell.get().map_or(&none, |s| &s.library.machine);
        let mut fields = summary(id, title, machine);
        for (name, value) in &mut fields {
            if *name == "status" {
                *value = str(status);
            }
        }
        record(fields)
    }

    pub(crate) fn signed_in(&self) -> bool {
        self.signed_in.load(Ordering::Acquire)
    }

    pub(crate) fn cannot_sign_in(&self) -> Option<ErrorView> {
        lock(&self.cannot_sign_in).clone()
    }

    /// Opens the sign-in page. `false` when no browser could be started.
    pub(crate) fn open_sign_in_page(&self) -> bool {
        self.parts
            .browser
            .open(&gogo::auth::browser::authorize_url())
    }

    /// Exchange `code` for a session, and drop what was read without one.
    pub(crate) async fn sign_in(&self, code: Zeroizing<String>) -> Result<(), ErrorView> {
        self.parts
            .account
            .login(code)
            .await
            .map_err(|e| errors::view("Sign-in failed", &e))?;
        self.signed_in.store(true, Ordering::Release);
        self.forget();
        Ok(())
    }

    /// Drop what was read, so that the next read asks again.
    pub(crate) fn forget(&self) {
        *lock(&self.snapshot) = Cell::default();
        lock(&self.overviews).clear();
    }

    pub(crate) async fn persist(&self) {
        if let Err(e) = self.parts.account.persist().await {
            tracing::warn!("{}", errors::view("The sign-in was not kept", &e).message);
        }
    }

    async fn bearer(&self, session: &dyn TokenProvider) -> Result<String, ApiError> {
        let _one = self.refresh.lock().await;
        Ok(session.bearer().await?)
    }

    /// A product's public page, read once. `None` when it could not be read.
    pub(crate) async fn overview(&self, id: u64) -> Arc<Option<Overview>> {
        let cell = Arc::clone(lock(&self.overviews).entry(id).or_default());
        let read = cell.get_or_init(|| async {
            let read = self.parts.endpoints.overview(&self.parts.http, id).await;
            if let Err(e) = &read {
                tracing::warn!(
                    "{}",
                    errors::view("A game's page could not be read", e).message
                );
            }
            Arc::new(read.ok())
        });
        Arc::clone(read.await)
    }

    async fn snapshot(&self) -> Arc<Snapshot> {
        let cell = Arc::clone(&lock(&self.snapshot));
        Arc::clone(
            cell.get_or_init(|| async { Arc::new(self.take_snapshot().await) })
                .await,
        )
    }

    async fn take_snapshot(&self) -> Snapshot {
        let mut failures = Vec::new();
        let mut cannot = None;
        let session = match self.parts.account.session().await {
            Ok(session) => session,
            Err(e) => {
                let view = errors::view("The sign-in could not be read", &e);
                failures.push(view.clone());
                cannot = Some(view);
                None
            }
        };
        self.signed_in.store(session.is_some(), Ordering::Release);
        if session.is_none() && cannot.is_none() {
            let account = Arc::clone(&self.parts.account);
            cannot = tokio::task::spawn_blocking(move || account.probe())
                .await
                .ok()
                .and_then(Result::err)
                .map(|e| errors::view("A sign-in could not be kept here", &e));
        }
        *lock(&self.cannot_sign_in) = cannot;
        let mut username = None;
        let mut products = Vec::new();
        if let Some(session) = &session {
            match self.owned_games(session.as_ref()).await {
                Ok(games) => products = games,
                Err(e) => failures.push(errors::view("The library could not be read", &e)),
            }
            match self.parts.account.username(session.as_ref()).await {
                Ok(name) => username = name,
                // The name is a nicety: no screen for it.
                Err(e) => tracing::warn!(
                    "{}",
                    errors::view("The user name could not be read", &e).message
                ),
            }
        }
        let ids = products.iter().map(|p| p.id).collect();
        let machine = self.machine(ids).await;
        Snapshot {
            username,
            signed_in: session.is_some(),
            library: Library { products, machine },
            failures: Mutex::new(failures),
        }
    }

    async fn owned_games(
        &self,
        session: &dyn TokenProvider,
    ) -> Result<Vec<LibraryProduct>, ApiError> {
        let bearer = self.bearer(session).await?;
        self.parts
            .endpoints
            .list_games(&self.parts.http, &bearer)
            .await
    }

    async fn machine(&self, ids: Vec<u64>) -> Machine {
        let local = Arc::clone(&self.parts.local);
        tokio::task::spawn_blocking(move || local.read(&ids))
            .await
            .unwrap_or_default()
    }

    pub(super) async fn detail(&self, id: u64) -> Result<Option<Product>, ApiError> {
        let Some(session) = self.parts.account.session().await? else {
            return Ok(None);
        };
        let bearer = self.bearer(session.as_ref()).await?;
        let product = self
            .parts
            .endpoints
            .detail(&self.parts.http, &bearer, id)
            .await?;
        Ok(Some(product))
    }

    /// The value of `key`. A source this version reads nothing for, and one that
    /// could not be read, gets [`values::nothing`], so no screen waits for ever.
    pub(crate) async fn read(&self, key: &SourceKey) -> Read {
        let name = key.name();
        // The product a `game…` source is about; anything else is no product.
        let id: Option<u64> = key.args().first().and_then(|a| a.parse().ok());
        match (name, id) {
            ("app", _) => (values::app(), Vec::new()),
            ("account", _) => {
                let s = self.snapshot().await;
                (
                    values::account(s.signed_in, s.username.as_deref()),
                    s.take_failures(),
                )
            }
            ("library", _) => {
                let s = self.snapshot().await;
                (library(&s.library), s.take_failures())
            }
            ("library.stats", _) => {
                let s = self.snapshot().await;
                (stats(&s.library), s.take_failures())
            }
            ("game", Some(id)) => self.game(id).await,
            ("game.builds", Some(id)) => {
                or_nothing(name, "The builds could not be read", self.builds(id).await)
            }
            ("game.dlc", Some(id)) => {
                or_nothing(name, "The DLC could not be read", self.dlc(id).await)
            }
            ("game.files", Some(id)) => {
                or_nothing(name, "The files could not be read", self.files(id).await)
            }
            _ => (values::nothing(name), Vec::new()),
        }
    }

    async fn game(&self, id: u64) -> Read {
        let snapshot = self.snapshot().await;
        let overview = self.overview(id).await;
        let mut machine = snapshot.library.machine.clone();
        let owned = snapshot.library.products.iter().find(|p| p.id == id);
        if owned.is_none() {
            machine = self.machine(vec![id]).await;
        }
        let page = overview.as_ref().as_ref();
        let title = owned
            .map(|p| p.title.clone())
            .or_else(|| page.and_then(|o| o.title.clone()))
            .unwrap_or_else(|| id.to_string());
        (
            game(id, &title, owned, page, &machine),
            snapshot.take_failures(),
        )
    }

    async fn builds(&self, id: u64) -> Result<Value, ErrorView> {
        let title = "The builds could not be read";
        let session = self.parts.account.session().await;
        let Some(session) = session.map_err(|e| errors::view(title, &e))? else {
            return Ok(Value::List(Vec::new()));
        };
        // Refresh under the lock before gog-warp's own calls.
        self.bearer(session.as_ref())
            .await
            .map_err(|e| errors::view(title, &e))?;
        // Windows builds on both systems: they are what a compatibility app runs.
        let builds =
            gogo::gog::builds_report::list(session.as_ref(), id, false, &self.parts.language)
                .await
                .map_err(|e| errors::view(title, &e))?;
        let machine = self.machine(vec![id]).await;
        let here = machine
            .built
            .get(&id)
            .and_then(|b| b.installed_build.as_ref())
            .map(|(_, build_id)| build_id.as_str());
        Ok(list(builds.iter().map(|b| {
            map([
                ("version", line(&b.entry.version)),
                ("build_id", line(&b.entry.build_id)),
                ("date", Value::Num(b.entry.published.as_second() as f64)),
                ("size", num(b.download_size.unwrap_or(0))),
                ("generation", num(u64::from(b.entry.generation))),
                ("legacy", Value::Bool(b.entry.generation == 1)),
                ("newest", Value::Bool(b.newest)),
                (
                    "this_machine",
                    Value::Bool(here == Some(b.entry.build_id.as_str())),
                ),
            ])
        })))
    }

    async fn dlc(&self, id: u64) -> Result<Value, ErrorView> {
        let title = "The DLC could not be read";
        let Some(product) = self.detail(id).await.map_err(|e| errors::view(title, &e))? else {
            return Ok(Value::List(Vec::new()));
        };
        if product.expanded_dlcs.is_empty() {
            return Ok(Value::List(Vec::new()));
        }
        let owned = self.owned_ids().await;
        let owned = owned.map_err(|e| errors::view(title, &e))?;
        let machine = self.machine(vec![id]).await;
        let installed = machine.built.get(&id).map(|b| b.dlcs.clone());
        let infos =
            gogo::gog::dlc::installer_infos(&product.expanded_dlcs, &owned, &self.parts.language);
        Ok(list(infos.iter().map(|d| {
            let here = installed.as_ref().is_some_and(|ids| ids.contains(&d.id));
            map([
                ("id", str(d.id.to_string())),
                ("title", line(&d.title)),
                ("owned", Value::Bool(d.owned)),
                ("installed", Value::Bool(here)),
                ("download_size", num(d.download_size.unwrap_or(0))),
                ("disk_size", num(d.disk_size.unwrap_or(0))),
                ("in_this_build", Value::Bool(d.in_build)),
            ])
        })))
    }

    async fn files(&self, id: u64) -> Result<Value, ErrorView> {
        let title = "The files could not be read";
        let Some(product) = self.detail(id).await.map_err(|e| errors::view(title, &e))? else {
            return Ok(Value::List(Vec::new()));
        };
        let filters = Filters {
            os: None,
            language: None,
            kinds: vec![
                Kind::Installer,
                Kind::Patch,
                Kind::BonusContent,
                Kind::LanguagePack,
            ],
            with_dlcs: false,
            only_dlcs: None,
        };
        let tasks = select::for_product(&product, &filters, &self.parts.download_dir);
        let machine = self.machine(vec![id]).await;
        Ok(list(tasks.iter().map(|t| {
            let kind = match t.kind {
                Kind::Installer => "installer",
                Kind::Patch => "patch",
                Kind::BonusContent => "extra",
                Kind::LanguagePack => "lang_pack",
            };
            let lang = t.entry.language_full.as_deref();
            let downloaded =
                machine
                    .downloaded
                    .contains(&(t.product_id, t.dlc_id, t.file.id.clone()));
            map([
                ("kind", str(kind)),
                ("os", os_word(t.entry.os.unwrap_or(Os::Windows))),
                (
                    "lang",
                    line(lang.or(t.entry.language.as_deref()).unwrap_or_default()),
                ),
                ("name", line(t.entry.name.as_deref().unwrap_or(&t.file.id))),
                ("size", num(t.file.size)),
                ("downloaded", Value::Bool(downloaded)),
                // Nothing records a check: only hashing the file again would tell.
                ("verified", Value::Null),
            ])
        })))
    }
}

fn or_nothing(source: &str, title: &str, read: Result<Value, ErrorView>) -> Read {
    match read {
        Ok(value) => (value, Vec::new()),
        Err(error) => {
            tracing::warn!("{title}: {}", error.message);
            (values::nothing(source), vec![error])
        }
    }
}

/// The `GameSummary` fields of an owned product.
fn summary(id: u64, title: &str, machine: &Machine) -> [(&'static str, Value); 11] {
    let built = machine.built.get(&id);
    let status = match built {
        Some(b) if b.running => "running",
        Some(b) if b.update_available => "update_available",
        Some(_) => "ready",
        None => "not_installed",
    };
    let bytes = built
        .map(|b| b.bytes)
        .or_else(|| machine.downloaded_bytes.get(&id).copied());
    let platforms = machine.platforms.get(&id).map(Vec::as_slice);
    [
        ("id", str(id.to_string())),
        ("title", line(title)),
        ("art", art(id)),
        ("status", str(status)),
        (
            "platforms",
            list(platforms.unwrap_or_default().iter().map(|&o| os_word(o))),
        ),
        (
            "runs_via",
            opt(built.and_then(|b| b.runtime.as_deref()).map(line)),
        ),
        ("size_on_disk", opt(bytes.map(num))),
        ("playtime", Value::Null),
        (
            "last_played",
            opt(built.and_then(|b| b.last_played).map(Value::Num)),
        ),
        ("progress", Value::Null),
        ("update", Value::Null),
    ]
}

fn record<'a>(fields: impl IntoIterator<Item = (&'a str, Value)>) -> Value {
    Value::Map(
        fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    )
}

fn library(library: &Library) -> Value {
    list(
        library
            .products
            .iter()
            .map(|p| record(summary(p.id, &p.title, &library.machine))),
    )
}

fn stats(library: &Library) -> Value {
    let machine = &library.machine;
    let here = |id: &u64| {
        machine.built.contains_key(id)
            || machine.depot_cache.contains(id)
            || machine.downloaded_bytes.contains_key(id)
    };
    let ids = || library.products.iter().map(|p| &p.id);
    let disk: u64 = ids()
        .map(|id| {
            machine.built.get(id).map_or(0, |b| b.bytes)
                + machine.downloaded_bytes.get(id).copied().unwrap_or(0)
        })
        .sum();
    let updates = ids()
        .filter(|id| machine.built.get(id).is_some_and(|b| b.update_available))
        .count();
    map([
        ("count", num(library.products.len() as u64)),
        ("installed", num(ids().filter(|id| here(id)).count() as u64)),
        ("disk_bytes", num(disk)),
        ("updates", num(updates as u64)),
    ])
}

fn game(
    id: u64,
    title: &str,
    owned: Option<&LibraryProduct>,
    page: Option<&Overview>,
    machine: &Machine,
) -> Value {
    let built = machine.built.get(&id);
    let first = |v: &[String]| {
        v.first()
            .map(String::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let year = page
        .and_then(|o| o.release_date.as_deref())
        .and_then(|d| d.get(..4)?.parse::<u64>().ok())
        .unwrap_or(0);
    let lane = match built {
        Some(b) if b.linux => "linux_app",
        Some(_) => "mac_app",
        None if machine.depot_cache.contains(&id) => "depot",
        None if machine.downloaded_bytes.contains_key(&id) => "installer",
        None => "none",
    };
    let installed = built.and_then(|b| b.installed_build.as_ref());
    let slug = owned
        .and_then(|p| p.slug.clone())
        .or_else(|| page.and_then(|o| o.slug.clone()))
        .unwrap_or_default();
    let mut fields: Vec<(&str, Value)> = summary(id, title, machine).into();
    fields.extend([
        ("slug", line(&slug)),
        (
            "genre",
            line(&page.map(|o| first(&o.genres)).unwrap_or_default()),
        ),
        ("year", num(year)),
        (
            "developer",
            line(&page.map(|o| o.developers.join(", ")).unwrap_or_default()),
        ),
        (
            "publisher",
            line(&page.map(|o| o.publishers.join(", ")).unwrap_or_default()),
        ),
        (
            "description",
            str(page
                .and_then(|o| o.description.as_deref())
                .map(text::description)
                .unwrap_or_default()),
        ),
        (
            "languages",
            list(
                page.map(|o| o.languages.as_slice())
                    .unwrap_or_default()
                    .iter()
                    .map(|l| line(l)),
            ),
        ),
        (
            "installed_build",
            opt(installed.map(|(version, build_id)| {
                map([("version", line(version)), ("build_id", line(build_id))])
            })),
        ),
        ("lane", str(lane)),
        ("running", Value::Bool(built.is_some_and(|b| b.running))),
    ]);
    record(fields)
}
