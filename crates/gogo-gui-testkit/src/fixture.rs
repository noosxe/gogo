//! A fixture data scope: [`gogo_gui_model::fixture`] as the scope the skin tests bind
//! against.

use gogo_gui_model::fixture::Fixture;
use gogo_skin::Value;
use gogo_skin::expr::{CallError, Scope};

pub use gogo_gui_model::fixture::{GAME_ID, NOW};

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
