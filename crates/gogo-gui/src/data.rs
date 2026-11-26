//! The app model as a skin reads it (design 25 §3, §5), on the fake backend's fictional
//! data until real data is wired in (#535).

use std::cell::RefCell;
use std::collections::HashSet;

use gogo_contract::{Value, actions};
use gogo_gui_model::fake::FakeBackend;
use gogo_gui_model::fixture::{self, Fixture, GAME_ID, NOW};
use gogo_gui_model::{Effect, InputEvent, Invocation, Location, Model, Outcome, SourceKey};
use gogo_skin::expr::{CallError, Scope};
use gogo_ui_scene::{Image, ImageKey, ImageStore, MemoryStore};

/// The number of the input that puts the fixture on its game: the model takes nothing
/// numbered at or below it.
pub(crate) const FIXTURE_EVENT: u64 = 1;

/// The model could not be put on the fixture's game.
#[derive(Debug, thiserror::Error)]
#[error("the fixture data could not be set up: {0}")]
pub(crate) struct DataError(String);

/// The model, and what the screen needs beside it.
pub(crate) struct Data {
    model: Model<FakeBackend>,
    /// The queue the skin shows. The model answers the queue from its own job table, which
    /// is empty without an engine.
    queue: Fixture,
    watched: HashSet<SourceKey>,
}

impl Data {
    /// Fictional data, showing the fixture's game. `wake` is the model's (design 25 §3).
    pub(crate) fn fixture(wake: impl Fn() + Send + Sync + 'static) -> Result<Data, DataError> {
        let mut model = Model::new(FakeBackend::with_fixture(), wake);
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
            queue: Fixture::new(),
            watched: HashSet::new(),
        })
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
        !self.model.pump().is_empty()
    }

    /// The route on screen.
    pub(crate) fn location(&self) -> &Location {
        self.model.location()
    }

    /// An action a control invoked.
    pub(crate) fn invoke(&mut self, invocation: &Invocation, event: InputEvent) -> Outcome {
        self.model.invoke(invocation, event)
    }

    /// Esc.
    pub(crate) fn dismiss(&mut self) -> Outcome {
        self.model.dismiss()
    }

    /// Whether the safety flow has a screen up.
    pub(crate) fn has_modal(&self) -> bool {
        self.model.modal().is_some()
    }

    /// What the app has to do since the last call.
    pub(crate) fn take_effects(&mut self) -> Vec<Effect> {
        self.model.take_effects()
    }

    /// The pictures for the handles the data names.
    pub(crate) fn images(&self) -> Images {
        Images
    }
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
        self.asked.borrow_mut().push(key);
    }
}

impl Scope for ModelScope<'_> {
    fn var(&self, name: &str) -> Option<&Value> {
        if is_queue(name) {
            return self.data.queue.var(name);
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
        if is_queue(name) {
            let value = self.data.queue.call(name, args.first()?);
            return Some(value.cloned().ok_or(CallError::NotFound));
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
        NOW
    }
}

/// Procedural art for the fixture's handles. A skin's own asset files are not decoded yet.
pub(crate) struct Images;

impl ImageStore for Images {
    fn get(&self, key: &ImageKey) -> Option<Image> {
        match key {
            ImageKey::Handle(name) => {
                let art = if &**name == fixture::art::CHECKER {
                    fixture::art::checker()
                } else {
                    fixture::art::cover(name)
                };
                Image::new(art.width, art.height, art.rgba)
            }
            ImageKey::Noise { .. } => MemoryStore::new().get(key),
            _ => None,
        }
    }
}
