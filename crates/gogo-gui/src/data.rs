//! The app model as a skin reads it (design 25 §3, §5): over [`CoreBackend`], or, in a
//! build with the `fixture` feature, over fictional data.

use std::cell::RefCell;
use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gogo_contract::{Gates, Value};
use gogo_gui_model::{
    Backend, Call, Effect, ErrorView, Failures, InputEvent, Invocation, Jobs, Location, Modal,
    Model, Outcome, PlanReply, Press, PromptAnswer, PromptSpec, Run, Secret, Slot, SourceKey,
    Subscription, Ticket,
};
use gogo_skin::expr::{CallError, Scope};
use gogo_ui_scene::{Image, ImageKey, ImageStore, MemoryStore};

use crate::core::art::ArtStore;
use crate::core::{CoreBackend, SignIn};

#[cfg(feature = "fixture")]
use fictional::FixtureData;

/// Input numbers start above this: the fixture spends it to get to its game, and the
/// model takes nothing numbered at or below an input it has used.
pub(crate) const FIXTURE_EVENT: u64 = 1;

/// The engine the model runs on.
pub(crate) enum Engine {
    #[cfg(feature = "fixture")]
    Fixture(Box<gogo_gui_model::fake::FakeBackend>),
    Core(Box<CoreBackend>),
}

/// Calls the same method on whichever engine there is.
macro_rules! engine {
    ($self:ident, $e:ident => $call:expr) => {
        match $self {
            #[cfg(feature = "fixture")]
            Engine::Fixture($e) => $call,
            Engine::Core($e) => $call,
        }
    };
}

impl Backend for Engine {
    fn attach(&mut self, failures: Failures) {
        engine!(self, e => e.attach(failures))
    }

    fn subscribe(&mut self, key: &SourceKey, slot: Slot) -> Subscription {
        engine!(self, e => e.subscribe(key, slot))
    }

    fn gates(&self) -> Gates {
        engine!(self, e => e.gates())
    }

    fn rate_limit(&self) -> Option<u64> {
        engine!(self, e => e.rate_limit())
    }

    fn plan(&mut self, call: &Call, answer: Option<&PromptAnswer>, reply: PlanReply) {
        engine!(self, e => e.plan(call, answer, reply))
    }

    fn prompt_for(&mut self, call: &Call) -> Result<PromptSpec, ErrorView> {
        engine!(self, e => e.prompt_for(call))
    }

    fn run(&mut self, run: Run, jobs: &mut Jobs<'_>) -> Result<(), ErrorView> {
        engine!(self, e => e.run(run, jobs))
    }
}

/// The model, and what the screen needs beside it.
pub(crate) struct Data {
    model: Model<Engine>,
    /// Over fictional data the queue is the fixture's: the model answers the queue from
    /// its own job table, which is empty without an engine.
    #[cfg(feature = "fixture")]
    fixture: Option<FixtureData>,
    watched: HashSet<SourceKey>,
    art: Option<ArtStore>,
    sign_in: Option<SignIn>,
}

impl Data {
    /// Real data. `wake` is the model's (design 25 §3).
    pub(crate) fn core(
        backend: CoreBackend,
        art: Option<ArtStore>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Data {
        let sign_in = Some(backend.sign_in());
        Data {
            sign_in,
            model: Model::new(Engine::Core(Box::new(backend)), wake),
            #[cfg(feature = "fixture")]
            fixture: None,
            watched: HashSet::new(),
            art,
        }
    }

    /// What one bind reads from.
    pub(crate) fn scope(&self) -> ModelScope<'_> {
        ModelScope {
            data: self,
            asked: RefCell::default(),
        }
    }

    /// Watches the sources of `asked` that are not watched yet and takes what they have.
    /// Returns whether there were any.
    pub(crate) fn watch(&mut self, asked: Vec<SourceKey>) -> bool {
        let mut new = false;
        for key in asked {
            if self.watched.insert(key.clone()) {
                self.model.watch(&key);
                new = true;
            }
        }
        if new {
            self.model.pump();
        }
        new
    }

    /// Takes what arrived. Returns whether anything changed.
    pub(crate) fn pump(&mut self) -> bool {
        let sources = !self.model.pump().is_empty();
        let pictures = self.art.as_ref().is_some_and(ArtStore::take_arrived);
        sources || pictures
    }

    /// Whether a source with no value is still on its way. Over fictional data every
    /// value there will ever be is there at once.
    pub(crate) fn arrives_later(&self) -> bool {
        #[cfg(feature = "fixture")]
        if self.fixture.is_some() {
            return false;
        }
        true
    }

    /// The route on screen.
    pub(crate) fn location(&self) -> &Location {
        self.model.location()
    }

    /// An action a control invoked.
    pub(crate) fn invoke(&mut self, invocation: &Invocation, event: InputEvent) -> Outcome {
        let outcome = self.model.invoke(invocation, event);
        // What the model answers itself (the route on screen, the jobs) is only current
        // after a pump, and the next bind must not wait for the wake.
        self.model.pump();
        outcome
    }

    /// Esc.
    pub(crate) fn dismiss(&mut self) -> Outcome {
        let outcome = self.model.dismiss();
        self.model.pump();
        outcome
    }

    /// The safety screen on top, if it is one the app draws.
    pub(crate) fn screen_to_draw(&self) -> Option<(Ticket, Drawn)> {
        match self.model.modal()? {
            // A job's questions come with the jobs (#536).
            Modal::Prompt {
                ticket,
                spec: PromptSpec::Secret { label },
                job: None,
            } => Some((ticket, Drawn::SignIn(label.clone()))),
            Modal::Error { ticket, error } => Some((ticket, Drawn::Error(error.clone()))),
            _ => None,
        }
    }

    /// Whether the screen on top is one that passes by itself: planning the sign-in
    /// that was just asked for.
    pub(crate) fn passing(&self) -> bool {
        matches!(
            self.model.modal(),
            Some(Modal::Planning {
                action: "account.sign_in",
                ..
            })
        )
    }

    /// The screen with `ticket` is on the display.
    pub(crate) fn presented(&mut self, ticket: Ticket, latest: InputEvent, at: Duration) {
        self.model.presented(ticket, latest, at);
    }

    /// Answer the question on screen with a secret.
    pub(crate) fn answer(&mut self, ticket: Ticket, secret: Secret, press: Press) -> Outcome {
        let outcome = self
            .model
            .answer(ticket, PromptAnswer::Secret(secret), press);
        // The plan of a sign-in is there at once.
        self.model.pump();
        outcome
    }

    pub(crate) fn cancel(&mut self, ticket: Ticket) {
        self.model.cancel(ticket);
        self.model.pump();
    }

    pub(crate) fn signing_in(&self) -> bool {
        self.sign_in.as_ref().is_some_and(SignIn::running)
    }

    /// "Open the page again".
    pub(crate) fn open_sign_in_page(&self) {
        if let Some(sign_in) = &self.sign_in
            && !sign_in.open_page()
        {
            tracing::warn!("no browser could be started");
        }
    }

    /// What the safety flow wants on screen, by kind: never its content, which can hold
    /// the user's data.
    pub(crate) fn modal_kind(&self) -> Option<&'static str> {
        Some(match self.model.modal()? {
            Modal::Prompt { .. } => "a prompt",
            Modal::Planning { .. } => "a planning screen",
            Modal::PlanCard { .. } => "a plan card",
            Modal::Strong { .. } => "a destructive confirmation",
            Modal::Error { .. } => "an error",
        })
    }

    /// The fake, to script it and see what ran.
    #[cfg(test)]
    pub(crate) fn backend(&self) -> &gogo_gui_model::fake::FakeBackend {
        match self.model.backend() {
            Engine::Fixture(fake) => fake,
            Engine::Core(_) => panic!("not on fictional data"),
        }
    }

    /// What the app has to do since the last call.
    pub(crate) fn take_effects(&mut self) -> Vec<Effect> {
        self.model.take_effects()
    }

    /// The pictures for the handles the data names.
    pub(crate) fn images(&self) -> Images {
        match &self.art {
            Some(art) => Images::Art(art.clone()),
            None => Images::Other,
        }
    }

    fn queue(&self, name: &str) -> Option<&Value> {
        #[cfg(feature = "fixture")]
        if let Some(fixture) = &self.fixture {
            return fixture.queue_var(name);
        }
        let _ = name;
        None
    }

    fn is_fixture_queue(&self, name: &str) -> bool {
        #[cfg(feature = "fixture")]
        if self.fixture.is_some() {
            return is_queue(name);
        }
        let _ = name;
        false
    }
}

/// A safety screen the app draws.
pub(crate) enum Drawn {
    /// The sign-in dialog, with what it says.
    SignIn(String),
    Error(ErrorView),
}

/// The sources the model answers from the job table.
fn is_queue(name: &str) -> bool {
    matches!(name, "queue" | "queue.summary" | "job")
}

/// [`Data`] as the skin's expressions see it. It remembers what it was asked for and does
/// not have, so that those sources can be watched.
pub(crate) struct ModelScope<'a> {
    data: &'a Data,
    asked: RefCell<Vec<SourceKey>>,
}

impl ModelScope<'_> {
    pub(crate) fn asked(self) -> Vec<SourceKey> {
        self.asked.into_inner()
    }

    fn ask(&self, key: SourceKey) {
        // The model answers these itself: they are never on the way.
        if !is_queue(key.name()) && key.name() != "route" {
            self.asked.borrow_mut().push(key);
        }
    }
}

impl Scope for ModelScope<'_> {
    fn var(&self, name: &str) -> Option<&Value> {
        if self.data.is_fixture_queue(name) {
            return self.data.queue(name);
        }
        let key = SourceKey::new(name, &[])?;
        let value = self.data.model.value(&key);
        if value.is_none() {
            self.ask(key);
        }
        value
    }

    fn param(&self, name: &str) -> Option<&Value> {
        self.data.model.location().params.get(name)
    }

    fn token(&self, _name: &str) -> Option<&Value> {
        None
    }

    fn call(&self, name: &str, args: &[&Value]) -> Option<Result<Value, CallError>> {
        let args: Vec<String> = args.iter().map(|a| a.display()).collect();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        #[cfg(feature = "fixture")]
        if self.data.is_fixture_queue(name)
            && let Some(fixture) = &self.data.fixture
        {
            return Some(fixture.queue_call(name, args.first()?));
        }
        let key = SourceKey::new(name, &args)?;
        Some(match self.data.model.value(&key) {
            Some(value) => Ok(value.clone()),
            None => {
                self.ask(key);
                Err(CallError::NotFound)
            }
        })
    }

    fn now(&self) -> f64 {
        #[cfg(feature = "fixture")]
        if self.data.fixture.is_some() {
            return gogo_gui_model::fixture::NOW;
        }
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64())
    }
}

/// The pictures of the data: the covers over real data, and procedural art for the
/// fixture's handles. A skin's own asset files are not decoded yet.
pub(crate) enum Images {
    Art(ArtStore),
    Other,
}

impl ImageStore for Images {
    fn get(&self, key: &ImageKey) -> Option<Image> {
        match (self, key) {
            (Images::Art(art), _) => art.get(key),
            #[cfg(feature = "fixture")]
            (Images::Other, ImageKey::Handle(name)) => fictional::picture(name),
            (Images::Other, ImageKey::Noise { .. }) => MemoryStore::new().get(key),
            _ => None,
        }
    }
}

#[cfg(feature = "fixture")]
mod fictional {
    use gogo_contract::actions;
    use gogo_gui_model::fake::FakeBackend;
    use gogo_gui_model::fixture::{self, Fixture, GAME_ID};

    use super::*;

    /// The model could not be put on the fixture's game.
    #[derive(Debug, thiserror::Error)]
    #[error("the fixture data could not be set up: {0}")]
    pub(crate) struct DataError(String);

    pub(crate) struct FixtureData {
        queue: Fixture,
    }

    impl FixtureData {
        pub(super) fn queue_var(&self, name: &str) -> Option<&Value> {
            self.queue.var(name)
        }

        pub(super) fn queue_call(&self, name: &str, arg: &str) -> Result<Value, CallError> {
            let value = self.queue.call(name, arg);
            value.cloned().ok_or(CallError::NotFound)
        }
    }

    impl Data {
        /// Fictional data, showing the fixture's game. `wake` is the model's.
        pub(crate) fn fixture(wake: impl Fn() + Send + Sync + 'static) -> Result<Data, DataError> {
            let fake = Engine::Fixture(Box::new(FakeBackend::with_fixture()));
            let mut model = Model::new(fake, wake);
            let go = actions::action("nav.go")
                .map(|spec| {
                    Invocation::new(spec)
                        .arg("route", Value::str("game"))
                        .route_arg("id", Value::str(GAME_ID))
                })
                .ok_or_else(|| DataError("the contract has no nav.go".to_owned()))?;
            match model.invoke(&go, InputEvent(FIXTURE_EVENT)) {
                Outcome::Accepted => {}
                other => return Err(DataError(format!("nav.go: {other:?}"))),
            }
            Ok(Data {
                model,
                fixture: Some(FixtureData {
                    queue: Fixture::new(),
                }),
                watched: HashSet::new(),
                art: None,
                sign_in: None,
            })
        }
    }

    /// Procedural art for the fixture's handles.
    pub(super) fn picture(name: &str) -> Option<Image> {
        let art = if name == fixture::art::CHECKER {
            fixture::art::checker()
        } else {
            fixture::art::cover(name)
        };
        Image::new(art.width, art.height, art.rgba)
    }
}
