//! The model the shell drives: sources, the safety flow, jobs and the route history,
//! over one [`Backend`].

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use gogo_contract::Value;

use crate::backend::{Backend, Subscription};
use crate::call::{Call, Invocation};
use crate::error::ErrorView;
use crate::failures::Failures;
use crate::jobs::{JobId, JobTable};
use crate::prompt::{PromptAnswer, PromptSpec};
use crate::safety::{
    Effect, Engine, InputEvent, Modal, Outcome, Press, Preview, Route, SafetyFlow, Ticket, route,
};
use crate::source::{Signal, SourceKey, SourceStore};

/// How many places `nav.back` remembers.
const HISTORY: usize = 64;

/// A route and its parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct Location {
    /// The route's name.
    pub route: String,
    /// Its parameters.
    pub params: BTreeMap<&'static str, Value>,
}

/// The app layer. It lives on the main thread.
pub struct Model<B: Backend> {
    backend: B,
    signal: Arc<Signal>,
    sources: SourceStore,
    subscriptions: HashMap<SourceKey, Subscription>,
    jobs: JobTable,
    flow: SafetyFlow,
    failures: Failures,
    history: Vec<Location>,
    /// The sources the model answers itself, kept so they can be lent out.
    own: HashMap<SourceKey, Value>,
}

impl<B: Backend> Model<B> {
    /// `wake` is called from any thread, at most once between two [`pump`](Self::pump)
    /// calls.
    pub fn new(mut backend: B, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let signal = Signal::new(wake);
        let failures = Failures::new(Arc::clone(&signal));
        backend.attach(failures.clone());
        let mut model = Model {
            backend,
            sources: SourceStore::new(Arc::clone(&signal)),
            subscriptions: HashMap::new(),
            jobs: JobTable::new(Arc::clone(&signal)),
            flow: SafetyFlow::new(Arc::clone(&signal)),
            failures,
            history: vec![Location {
                route: "library".to_owned(),
                params: BTreeMap::new(),
            }],
            own: HashMap::new(),
            signal,
        };
        model.refresh_own();
        model
    }

    /// The engine behind the model.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// For wiring and tests. Nothing here can make a [`Run`](crate::Run), so it opens no
    /// way round the safety flow.
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Where to report what fails when no call is waiting for it.
    pub fn failures(&self) -> Failures {
        self.failures.clone()
    }

    /// The job table.
    pub fn jobs(&self) -> &JobTable {
        &self.jobs
    }

    /// The route on screen.
    pub fn location(&self) -> &Location {
        // `history` is never empty: `nav.back` keeps the last one.
        &self.history[self.history.len() - 1]
    }

    // --- sources ---

    /// Make sure `key` is being fed.
    pub fn watch(&mut self, key: &SourceKey) {
        if is_own(key.name()) || self.subscriptions.contains_key(key) {
            return;
        }
        let slot = self.sources.slot(key);
        let subscription = self.backend.subscribe(key, slot);
        self.subscriptions.insert(key.clone(), subscription);
    }

    /// Stop feeding `key` and forget its value.
    pub fn unwatch(&mut self, key: &SourceKey) {
        self.subscriptions.remove(key);
        self.sources.close(key);
    }

    /// A source's value as of the last [`pump`](Self::pump).
    pub fn value(&self, key: &SourceKey) -> Option<&Value> {
        if is_own(key.name()) {
            self.own.get(key)
        } else {
            self.sources.get(key)
        }
    }

    fn refresh_own(&mut self) {
        let mut own = HashMap::new();
        let mut put = |name: &str, args: &[&str], value: Value| {
            if let Some(key) = SourceKey::new(name, args) {
                own.insert(key, value);
            }
        };
        let current = Value::str(&self.location().route);
        put(
            "route",
            &[],
            Value::Map(BTreeMap::from([("current".to_owned(), current)])),
        );
        put("queue", &[], self.jobs.queue_value());
        put(
            "queue.summary",
            &[],
            self.jobs.summary_value(self.backend.rate_limit()),
        );
        for id in self.jobs.ids() {
            if let Some(job) = self.jobs.job_value(id) {
                put("job", &[id.as_str()], job);
            }
        }
        self.own = own;
    }

    /// Take everything that arrived since the last call. Returns the sources whose
    /// values changed.
    pub fn pump(&mut self) -> Vec<SourceKey> {
        self.signal.take();
        let mut changed = self.sources.drain();
        // Questions first: draining forgets unlisted jobs that ended.
        let questions = self.jobs.take_prompts();
        let transitions = self.jobs.drain();
        let failed = self.failures.take();
        let (flow, mut engine) = self.split();
        for error in failed {
            flow.fail(error);
        }
        flow.pump(&mut engine, questions, &transitions);

        let before = std::mem::take(&mut self.own);
        self.refresh_own();
        changed.extend(
            self.own
                .iter()
                .filter(|(k, v)| before.get(*k) != Some(*v))
                .map(|(k, _)| k.clone()),
        );
        changed.extend(before.into_keys().filter(|k| !self.own.contains_key(k)));
        changed.sort();
        changed
    }

    fn split(&mut self) -> (&mut SafetyFlow, Engine<'_, B>) {
        (
            &mut self.flow,
            Engine {
                backend: &mut self.backend,
                jobs: &mut self.jobs,
            },
        )
    }

    // --- what a skin asks for ---

    /// An action a skin's control invoked, from one real input (S4).
    pub fn invoke(&mut self, invocation: &Invocation, event: InputEvent) -> Outcome {
        let (flow, mut engine) = self.split();
        let call = match flow.invoke(&mut engine, invocation, event) {
            Ok(call) => call,
            Err(outcome) => return outcome,
        };
        // Which skin is drawn is the window's business, not the engine's.
        let skin = call.name() == "settings.set" && call.text("key") == Some("gui.skin");
        if skin || route(call.spec()) == Route::Shell {
            self.shell(&call);
            return Outcome::Accepted;
        }
        flow.dispatch(&mut engine, call)
    }

    fn shell(&mut self, call: &Call) {
        let text = |name: &str| call.text(name).map(str::to_owned);
        match call.name() {
            "nav.go" => {
                if let Some(route) = text("route") {
                    self.history.push(Location {
                        route,
                        params: call.route_params().clone(),
                    });
                    if self.history.len() > HISTORY {
                        self.history.remove(0);
                    }
                }
            }
            "nav.back" => {
                if self.history.len() > 1 {
                    self.history.pop();
                }
            }
            "palette.open" => self.flow.push_effect(Effect::OpenPalette {
                scope: text("scope"),
            }),
            "account.menu" => self.flow.push_effect(Effect::AccountMenu),
            // Only `gui.skin` comes here.
            "settings.set" => {
                if let Some(value) = call.arg("value") {
                    self.flow.push_effect(Effect::SetSkin(value.display()));
                }
            }
            "ui.set" => {
                if let (Some(var), Some(value)) = (text("var"), call.arg("value")) {
                    self.flow.push_effect(Effect::SetUi {
                        var,
                        value: value.clone(),
                    });
                }
            }
            "clipboard.copy" => {
                if let Some(value) = call.arg("field") {
                    self.flow.push_effect(Effect::Copy(value.display()));
                }
            }
            _ => {}
        }
        self.signal.raise();
    }

    /// Plan an action to show inline. See [`SafetyFlow`].
    ///
    /// # Errors
    /// The call is refused, has no plan, or its gate is off.
    pub fn preview(&mut self, invocation: &Invocation) -> Result<Ticket, ErrorView> {
        let (flow, mut engine) = self.split();
        flow.preview(&mut engine, invocation)
    }

    /// Where an inline preview is; `None` once it is used or dropped.
    pub fn preview_state(&self, ticket: Ticket) -> Option<Preview<'_>> {
        self.flow.preview_state(ticket)
    }

    /// Forget every preview: the data they were computed from changed.
    pub fn clear_previews(&mut self) {
        self.flow.clear_previews();
    }

    // --- the app's own widgets ---

    /// The screen on top.
    pub fn modal(&self) -> Option<Modal<'_>> {
        self.flow.modal()
    }

    /// What the shell has to do since the last call.
    pub fn take_effects(&mut self) -> Vec<Effect> {
        self.flow.take_effects()
    }

    /// The screen with `ticket` is drawn at `now`; `latest` is the last input event
    /// numbered.
    pub fn presented(&mut self, ticket: Ticket, latest: InputEvent, now: Duration) {
        self.flow.presented(ticket, latest, now);
    }

    /// The plan card's confirm control (S5).
    pub fn confirm(&mut self, ticket: Ticket, press: Press) -> Outcome {
        let (flow, mut engine) = self.split();
        flow.confirm(&mut engine, ticket, press)
    }

    /// Enter. It confirms a plan card and answers nothing else (S19).
    pub fn enter(&mut self, press: Press) -> Outcome {
        match self.flow.modal() {
            Some(Modal::PlanCard { ticket, .. }) => self.confirm(ticket, press),
            _ => Outcome::Ignored,
        }
    }

    /// Submit the typed strong confirmation.
    pub fn type_name(&mut self, ticket: Ticket, typed: &str, press: Press) -> Outcome {
        let (flow, mut engine) = self.split();
        flow.type_name(&mut engine, ticket, typed, press)
    }

    /// The hold button of a strong confirmation went down (S6).
    pub fn hold_start(&mut self, ticket: Ticket, press: Press) -> Outcome {
        self.flow.hold_start(ticket, press)
    }

    /// The hold button came up, or the window lost the input.
    pub fn hold_stop(&mut self, ticket: Ticket) {
        self.flow.hold_stop(ticket);
    }

    /// Call while a hold is down, with the clock of its [`Press::at`].
    pub fn tick(&mut self, now: Duration) {
        let (flow, mut engine) = self.split();
        flow.tick(&mut engine, now);
    }

    /// Answer the prompt on screen.
    pub fn answer(&mut self, ticket: Ticket, answer: PromptAnswer, press: Press) -> Outcome {
        let (flow, mut engine) = self.split();
        flow.answer(&mut engine, ticket, answer, press)
    }

    /// Ask a question of the app's own, such as which file to read. No skin can call
    /// this. Its answer arrives as [`Effect::Answered`] with the ticket given here;
    /// `None` while another screen is up.
    pub fn ask(&mut self, spec: PromptSpec) -> Option<Ticket> {
        self.flow.ask(spec)
    }

    /// Cancel a screen or preview, or dismiss an error (S8).
    pub fn cancel(&mut self, ticket: Ticket) {
        self.flow.cancel(ticket);
    }

    /// Esc (S8, S19).
    pub fn dismiss(&mut self) -> Outcome {
        self.flow.dismiss()
    }

    /// Asked before the window closes. `true`: a job is running something that must
    /// not be stopped, so the window stays and the app says why.
    pub fn refuse_close(&mut self) -> bool {
        if !self.jobs.unstoppable() {
            return false;
        }
        self.flow.fail(ErrorView::new(
            "Usage",
            "The window stays open for now",
            "a job is running something that can't be stopped half way",
            "Close the window again once the job has ended",
            "2",
        ));
        true
    }

    /// Stop a job from the app's own control, whatever is on screen.
    pub fn cancel_job(&mut self, id: &JobId) {
        let (flow, mut engine) = self.split();
        flow.cancel_job(&mut engine, id);
    }
}

/// The sources the model answers from its own state.
fn is_own(name: &str) -> bool {
    matches!(name, "route" | "queue" | "queue.summary" | "job")
}
