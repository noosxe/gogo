//! A fixture data scope: [`gogo_gui_model::fixture`] as the scope the skin tests bind
//! against.

use gogo_contract::{FieldType, shapes, sources::SOURCES};
use gogo_gui_model::fixture::Fixture;
use gogo_skin::Value;
use gogo_skin::expr::{CallError, Scope};

pub use gogo_gui_model::fixture::{GAME_ID, NOW};

/// The game ids the fixture answers per-game calls for.
const CALL_IDS: [&str; 5] = [GAME_ID, "9", "11", "12", "13"];

/// What a skin's expressions read from the app, with the data of [`Fixture`].
///
/// Sources without arguments are variables, sources with arguments are calls keyed by name
/// and first argument, and route parameters are parameters. A call for a game the fixture
/// doesn't have answers `NotFound`, as the app would.
#[derive(Debug, Clone, Default)]
pub struct FixtureScope {
    data: Fixture,
}

impl FixtureScope {
    /// The fixture library, on the `game` route for [`GAME_ID`].
    pub fn new() -> Self {
        FixtureScope::default()
    }

    /// A new install: signed out, every list empty, every optional field null, every
    /// number zero. The game still has its id, title and status; the route and the app's
    /// own facts are the fixture's.
    pub fn emptied() -> Self {
        let mut scope = Self::new().reshaped(&|v, ty| *v = blank(ty));
        for (source, field, value) in [
            ("account", "signed_in", Value::Bool(false)),
            ("app", "version", Value::str("0.0.0-test")),
            ("app", "contract", Value::str("1.0")),
            ("app", "os", Value::str("linux")),
        ] {
            scope.set_field(source, field, value);
        }
        for id in CALL_IDS {
            scope.set_in_call("game", id, "id", Value::str(id));
            scope.set_in_call("game", id, "title", Value::str("Example Quest"));
            scope.set_in_call("game.saves", id, "state", Value::str("disabled"));
            scope.set_in_call("game.saves", id, "checked_at", Value::Null);
            scope.set_in_call("game.doctor", id, "checked_at", Value::Null);
        }
        scope
    }

    /// The fixture with every optional field null.
    pub fn without_optionals() -> Self {
        Self::new().reshaped(&|v, ty| {
            walk(v, ty, &|v, ty| {
                if matches!(ty, FieldType::Option(_)) {
                    *v = Value::Null;
                }
            })
        })
    }

    /// The fixture with every list cut to its first item.
    pub fn single_items() -> Self {
        Self::new().reshaped(&|v, ty| {
            walk(v, ty, &|v, _| {
                if let Value::List(l) = v {
                    l.truncate(1);
                }
            })
        })
    }

    /// Apply `change` to every source and per-game answer, with its contract shape.
    fn reshaped(mut self, change: &dyn Fn(&mut Value, &FieldType)) -> Self {
        for source in SOURCES {
            // `route` says what is on screen, not data of the app.
            let Some(ty) = shapes::source_shape(source.name).filter(|_| source.name != "route")
            else {
                continue;
            };
            if let Some(mut v) = self.data.var(source.name).cloned() {
                change(&mut v, &ty);
                self.data.set_var(source.name, v);
            }
            for id in CALL_IDS {
                if let Some(mut v) = self.data.call(source.name, id).cloned() {
                    change(&mut v, &ty);
                    self.data.set_call(source.name, id, v);
                }
            }
        }
        self
    }

    /// Change a field of a per-game answer. Does nothing if there is no such answer.
    pub fn set_in_call(&mut self, name: &str, id: &str, field: &str, v: Value) {
        if let Some(Value::Map(mut m)) = self.data.call(name, id).cloned() {
            m.insert(field.to_owned(), v);
            self.data.set_call(name, id, Value::Map(m));
        }
    }

    /// The route on screen, and its `id` parameter when it has one.
    pub fn set_route(&mut self, route: &str, id: Option<&str>) {
        self.data.set_route(route, id);
    }

    /// The same data on another route.
    pub fn on(mut self, route: &str, id: Option<&str>) -> Self {
        self.set_route(route, id);
        self
    }

    /// Set a source that takes no argument.
    pub fn set_var(&mut self, name: &str, v: Value) {
        self.data.set_var(name, v);
    }

    /// Set a route parameter.
    pub fn set_param(&mut self, name: &str, v: Value) {
        self.data.set_param(name, v);
    }

    /// Set the answer of a source call for one game id.
    pub fn set_call(&mut self, name: &str, id: &str, v: Value) {
        self.data.set_call(name, id, v);
    }

    /// Change a field of a source that takes no argument, `queue.summary` and `rate_limit`
    /// for example. Does nothing if the source or the field isn't there.
    pub fn set_field(&mut self, source: &str, field: &str, v: Value) {
        self.data.set_field(source, field, v);
    }
}

/// A value of type `ty` that says nothing, as the app gives for a source with no data.
fn blank(ty: &FieldType) -> Value {
    match ty {
        FieldType::Bool => Value::Bool(false),
        FieldType::Number
        | FieldType::Bytes
        | FieldType::Time
        | FieldType::Duration
        | FieldType::Fraction => Value::Num(0.0),
        FieldType::OneOf(words) => Value::str(words.first().copied().unwrap_or_default()),
        FieldType::Status => Value::str("not_installed"),
        FieldType::Os => Value::str("linux"),
        FieldType::Color => Value::str("#000000"),
        FieldType::List(_) => Value::List(Vec::new()),
        FieldType::Record(shape) => Value::Map(
            shape
                .all_fields()
                .into_iter()
                .map(|f| (f.name.to_owned(), blank(&f.ty)))
                .collect(),
        ),
        FieldType::Option(_) | FieldType::ActionRef | FieldType::Opaque(_) | FieldType::Ui => {
            Value::Null
        }
        _ => Value::str(""),
    }
}

/// Run `visit` on `v` and then on everything inside it, each with its type.
fn walk(v: &mut Value, ty: &FieldType, visit: &dyn Fn(&mut Value, &FieldType)) {
    visit(v, ty);
    match (v, ty) {
        (v, FieldType::Option(inner)) if !matches!(v, Value::Null) => walk(v, inner, visit),
        (Value::List(l), FieldType::List(item)) => {
            for v in l {
                walk(v, item, visit);
            }
        }
        (Value::Map(m), FieldType::Record(shape)) => {
            for f in shape.all_fields() {
                if let Some(v) = m.get_mut(f.name) {
                    walk(v, &f.ty, visit);
                }
            }
        }
        _ => {}
    }
}

impl Scope for FixtureScope {
    fn var(&self, name: &str) -> Option<&Value> {
        self.data.var(name)
    }

    fn param(&self, name: &str) -> Option<&Value> {
        self.data.param(name)
    }

    fn token(&self, _name: &str) -> Option<&Value> {
        None
    }

    fn call(&self, name: &str, args: &[&Value]) -> Option<Result<Value, CallError>> {
        if !self.data.has_call_name(name) {
            return None;
        }
        let Some(id) = args.first().map(|v| v.display()) else {
            return Some(Err(CallError::BadArgument {
                index: 0,
                expected: "a game id",
            }));
        };
        Some(
            self.data
                .call(name, &id)
                .cloned()
                .ok_or(CallError::NotFound),
        )
    }

    fn now(&self) -> f64 {
        self.data.now()
    }
}
