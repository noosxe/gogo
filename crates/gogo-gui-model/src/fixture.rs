//! Fixture data: a small, fictional library to draw and test the app without an engine
//! (design 08 §7). The skin tests bind against it, and the app draws it through
//! [`FakeBackend::with_fixture`](crate::fake::FakeBackend::with_fixture), so both see the
//! same values. Behind the `fake` feature.
//!
//! Everything here is invented: the games, the player, the sizes and the dates. Art in the
//! data is a plain string handle (`art:1:cover`); [`art`] makes its pixels.

use std::collections::BTreeMap;

use gogo_contract::Value;

pub mod art;

/// The id of the one fixture game with all its sources filled in.
pub const GAME_ID: &str = "1";

/// A fixed "now", in unix seconds: 2023-11-14 22:13:20 UTC.
pub const NOW: f64 = 1_700_000_000.0;

const DAY: f64 = 86_400.0;

/// What the app would feed a skin, with fixed fictional values.
///
/// Sources without arguments (`account`, `route`, `queue.summary`, …) are variables;
/// sources with arguments (`game($id)`, `game.builds($id)`, …) are calls keyed by name and
/// first argument; route parameters (`$id`) are parameters.
#[derive(Debug, Clone)]
pub struct Fixture {
    vars: BTreeMap<String, Value>,
    params: BTreeMap<String, Value>,
    calls: BTreeMap<(String, String), Value>,
    now: f64,
}

fn text(s: &str) -> Value {
    Value::str(s)
}

fn num(n: f64) -> Value {
    Value::Num(n)
}

fn map(pairs: Vec<(&str, Value)>) -> Value {
    Value::Map(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

fn list(items: Vec<Value>) -> Value {
    Value::List(items)
}

fn art(id: &str) -> Value {
    map(vec![
        ("cover", text(&format!("art:{id}:cover"))),
        ("background", text(&format!("art:{id}:background"))),
        ("logo", text(&format!("art:{id}:logo"))),
        ("colors", list(vec![text("#3a5f8a"), text("#c9a227")])),
    ])
}

fn summary(id: &str, title: &str, status: &str) -> Value {
    map(vec![
        ("id", text(id)),
        ("title", text(title)),
        ("art", art(id)),
        ("status", text(status)),
        ("platforms", list(vec![text("windows"), text("linux")])),
        ("runs_via", Value::Null),
        ("size_on_disk", Value::Null),
        ("playtime", Value::Null),
        ("last_played", Value::Null),
        ("progress", Value::Null),
        ("update", Value::Null),
    ])
}

fn game_detail() -> Value {
    let Value::Map(mut m) = summary(GAME_ID, "Example Quest", "update_available") else {
        return Value::Null;
    };
    let mut set = |k: &str, v: Value| {
        m.insert(k.to_owned(), v);
    };
    set("runs_via", text("Proton"));
    set("size_on_disk", num(18_600_000_000.0));
    set("playtime", num(41.0 * 3600.0));
    set("last_played", num(NOW - 2.0 * DAY));
    let other = summary("9", "Sample Racer", "ready");
    set(
        "update",
        map(vec![
            ("game", other),
            ("from", text("1.0.2")),
            ("to", text("1.1.0")),
            ("size", num(1_200_000_000.0)),
            ("removes_dlc", Value::Bool(false)),
        ]),
    );
    set("slug", text("example-quest"));
    set("genre", text("Role-playing"));
    set("year", num(2018.0));
    set("developer", text("Imaginary Studio"));
    set("publisher", text("Imaginary Publishing"));
    set("description", text("A made-up quest for tests."));
    set("languages", list(vec![text("en"), text("de")]));
    set(
        "installed_build",
        map(vec![("version", text("1.0.2")), ("build_id", text("1001"))]),
    );
    set("lane", text("depot"));
    set("running", Value::Bool(false));
    Value::Map(m)
}

fn build(version: &str, age_days: f64, size: f64, newest: bool, here: bool) -> Value {
    map(vec![
        ("version", text(version)),
        ("build_id", text(&format!("b{}", version.replace('.', "")))),
        ("date", num(NOW - age_days * DAY)),
        ("size", num(size)),
        ("generation", num(2.0)),
        ("legacy", Value::Bool(version.starts_with("0."))),
        ("newest", Value::Bool(newest)),
        ("this_machine", Value::Bool(here)),
    ])
}

fn snapshot(n: f64, age_days: f64, kind: &str, label: &str) -> Value {
    map(vec![
        ("n", num(n)),
        ("date", num(NOW - age_days * DAY)),
        ("kind", text(kind)),
        ("label", text(label)),
        ("build", text("1.0.2")),
        ("dlc_count", num(1.0)),
        ("exclusive", Value::Null),
        ("estimate", Value::Bool(false)),
    ])
}

fn dlc(id: &str, title: &str, installed: bool) -> Value {
    map(vec![
        ("id", text(id)),
        ("title", text(title)),
        ("owned", Value::Bool(true)),
        ("installed", Value::Bool(installed)),
        ("download_size", num(300_000_000.0)),
        ("disk_size", num(450_000_000.0)),
        ("in_this_build", Value::Bool(true)),
    ])
}

fn check(name: &str, result: &str, message: &str) -> Value {
    map(vec![
        ("name", text(name)),
        ("result", text(result)),
        ("message", text(message)),
        ("hint", text("")),
        ("fix", Value::Null),
    ])
}

fn game_calls() -> Vec<(&'static str, Value)> {
    let counts = map(vec![
        ("pull", num(0.0)),
        ("push", num(0.0)),
        ("delete", num(0.0)),
        ("conflict", num(0.0)),
    ]);
    vec![
        ("game", game_detail()),
        (
            "game.runner",
            map(vec![
                ("platform", text("linux")),
                ("renderer", text("dxvk")),
                (
                    "renderer_options",
                    list(vec![text("dxvk"), text("wined3d")]),
                ),
                (
                    "runtime",
                    map(vec![("id", text("proton-9")), ("version", text("9.0"))]),
                ),
                (
                    "runtime_options",
                    list(vec![text("proton-9"), text("proton-8")]),
                ),
                (
                    "display",
                    map(vec![
                        ("mode", text("wayland")),
                        ("hdr", Value::Bool(false)),
                        ("monitor", Value::Null),
                    ]),
                ),
                (
                    "galaxy",
                    map(vec![("setting", text("on")), ("state", text("signed_in"))]),
                ),
                ("wrapper", list(vec![])),
                ("args", list(vec![])),
            ]),
        ),
        (
            "game.snapshots",
            map(vec![
                ("auto", Value::Bool(true)),
                (
                    "items",
                    list(vec![
                        snapshot(1.0, 30.0, "initial", "Initial"),
                        snapshot(2.0, 9.0, "auto", "Before update to 1.0.2"),
                        snapshot(3.0, 3.0, "manual", "Before the boss"),
                    ]),
                ),
            ]),
        ),
        (
            "game.builds",
            list(vec![
                build("1.1.0", 1.0, 18_700_000_000.0, true, false),
                build("1.0.2", 40.0, 18_600_000_000.0, false, true),
                build("1.0.1", 90.0, 18_500_000_000.0, false, false),
                build("1.0.0", 200.0, 18_400_000_000.0, false, false),
                build("0.9.0", 400.0, 17_000_000_000.0, false, false),
            ]),
        ),
        (
            "game.saves",
            map(vec![
                ("state", text("in_sync")),
                ("last_sync", num(NOW - 600.0)),
                ("counts", counts),
                (
                    "files",
                    list(vec![map(vec![
                        ("display_path", text("Saves/slot1.sav")),
                        ("action", text("none")),
                        ("conflict", Value::Bool(false)),
                        ("local_time", num(NOW - 700.0)),
                        ("cloud_time", num(NOW - 700.0)),
                    ])]),
                ),
                ("quota", Value::Null),
                ("checked_at", num(NOW - 60.0)),
            ]),
        ),
        (
            "game.files",
            list(vec![map(vec![
                ("kind", text("installer")),
                ("os", text("linux")),
                ("lang", text("en")),
                ("name", text("example_quest_1.1.0.sh")),
                ("size", num(18_700_000_000.0)),
                ("downloaded", Value::Bool(true)),
                ("verified", Value::Bool(true)),
            ])]),
        ),
        (
            "game.dlc",
            list(vec![
                dlc("2", "Bonus Pack", true),
                dlc("3", "Extra Episode", false),
            ]),
        ),
        (
            "game.doctor",
            map(vec![
                ("checked_at", num(NOW - 120.0)),
                (
                    "summary",
                    map(vec![
                        ("ok", num(3.0)),
                        ("warn", num(1.0)),
                        ("fail", num(0.0)),
                    ]),
                ),
                (
                    "checks",
                    list(vec![
                        check("disk", "ok", "Enough free space"),
                        check("gpu", "warn", "No Vulkan device found"),
                    ]),
                ),
            ]),
        ),
        ("game.env", list(vec![])),
    ]
}

impl Fixture {
    /// The fixture library, on the `game` route for [`GAME_ID`].
    pub fn new() -> Self {
        let mut s = Fixture {
            vars: BTreeMap::new(),
            params: BTreeMap::new(),
            calls: BTreeMap::new(),
            now: NOW,
        };
        s.set_var(
            "account",
            map(vec![
                ("signed_in", Value::Bool(true)),
                ("username", text("player-one")),
                ("avatar", Value::Null),
            ]),
        );
        s.set_var(
            "app",
            map(vec![
                ("version", text("0.0.0-test")),
                ("contract", text("1.0")),
                ("os", text("linux")),
                ("reduced_motion", Value::Bool(false)),
                ("high_contrast", Value::Bool(false)),
                ("big_screen", Value::Bool(false)),
                ("locale", text("en")),
            ]),
        );
        s.set_var(
            "library.stats",
            map(vec![
                ("count", num(12.0)),
                ("installed", num(5.0)),
                ("disk_bytes", num(95_000_000_000.0)),
                ("updates", num(1.0)),
            ]),
        );
        let current = map(vec![
            ("id", text("j1")),
            ("kind", text("update")),
            ("game", summary("9", "Sample Racer", "downloading")),
            ("state", text("running")),
            ("steps", list(vec![])),
            ("bytes", num(1_240_000_000.0)),
            ("total", num(2_000_000_000.0)),
            ("rate", num(48_200_000.0)),
            ("eta", num(720.0)),
            ("fraction", num(0.62)),
            ("files", list(vec![])),
            ("error", Value::Null),
        ]);
        s.set_var(
            "queue.summary",
            map(vec![
                ("active", num(1.0)),
                ("queued", num(2.0)),
                ("rate", num(48_200_000.0)),
                ("rate_limit", Value::Null),
                ("current", current),
            ]),
        );
        s.set_var(
            "doctor.all",
            list(vec![
                map(vec![
                    ("game", summary("9", "Sample Racer", "ready")),
                    (
                        "summary",
                        map(vec![
                            ("ok", num(4.0)),
                            ("warn", num(0.0)),
                            ("fail", num(0.0)),
                        ]),
                    ),
                ]),
                map(vec![
                    (
                        "game",
                        summary(GAME_ID, "Example Quest", "update_available"),
                    ),
                    (
                        "summary",
                        map(vec![
                            ("ok", num(3.0)),
                            ("warn", num(1.0)),
                            ("fail", num(0.0)),
                        ]),
                    ),
                ]),
            ]),
        );
        for (name, v) in game_calls() {
            s.set_call(name, GAME_ID, v);
        }
        s.set_route("game", Some(GAME_ID));
        s
    }

    /// The route on screen, and its `id` parameter when it has one.
    pub fn set_route(&mut self, route: &str, id: Option<&str>) {
        self.set_var("route", map(vec![("current", text(route))]));
        match id {
            Some(id) => self.set_param("id", text(id)),
            None => {
                self.params.remove("id");
            }
        }
    }

    /// Set a source that takes no argument.
    pub fn set_var(&mut self, name: &str, v: Value) {
        self.vars.insert(name.to_owned(), v);
    }

    /// Set a route parameter.
    pub fn set_param(&mut self, name: &str, v: Value) {
        self.params.insert(name.to_owned(), v);
    }

    /// Set the answer of a source call for one game id.
    pub fn set_call(&mut self, name: &str, id: &str, v: Value) {
        self.calls.insert((name.to_owned(), id.to_owned()), v);
    }

    /// Change a field of a source that takes no argument, `queue.summary` and `rate_limit`
    /// for example. Does nothing if the source or the field isn't there.
    pub fn set_field(&mut self, source: &str, field: &str, v: Value) {
        if let Some(Value::Map(m)) = self.vars.get_mut(source) {
            m.insert(field.to_owned(), v);
        }
    }

    /// A source that takes no argument.
    pub fn var(&self, name: &str) -> Option<&Value> {
        self.vars.get(name)
    }

    /// A route parameter.
    pub fn param(&self, name: &str) -> Option<&Value> {
        self.params.get(name)
    }

    /// The answer of a source call for one first argument, a game id.
    pub fn call(&self, name: &str, first_arg: &str) -> Option<&Value> {
        self.calls.get(&(name.to_owned(), first_arg.to_owned()))
    }

    /// Whether source `name` has an answer for any argument.
    pub fn has_call_name(&self, name: &str) -> bool {
        self.calls.keys().any(|(n, _)| n == name)
    }

    /// The fixed "now", [`NOW`].
    pub fn now(&self) -> f64 {
        self.now
    }
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}
