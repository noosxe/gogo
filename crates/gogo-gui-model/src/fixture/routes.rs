//! The sources of the routes beyond the game page: library, queue, updates, installed
//! apps, runtimes, renderers, settings, and the per-game saves and doctor answers they
//! read. Consistent with `library.stats` and `queue.summary`, and as invented as the rest.

use gogo_contract::Value;

use super::{DAY, Fixture, GAME_ID, NOW, check, game_detail, list, map, num, summary, text};

const GB: f64 = 1_000_000_000.0;

/// The library: id, title and status. Twelve games, five of them installed.
const GAMES: &[(&str, &str, &str)] = &[
    (GAME_ID, "Example Quest", "update_available"),
    ("9", "Sample Racer", "downloading"),
    ("11", "Placeholder Tactics", "ready"),
    ("12", "Mock Harbor", "running"),
    ("13", "Stub Valley", "ready"),
    ("14", "Dummy Dungeon", "queued"),
    ("15", "Lorem Skies", "queued"),
    ("16", "Template Tales", "failed"),
    ("17", "Specimen Station", "not_installed"),
    ("18", "Draft Kingdoms", "not_installed"),
    ("19", "Filler Fleet", "not_installed"),
    ("20", "Pattern Peaks", "not_installed"),
];

/// The installed games: id, size, runtime, renderer, DLC count and days since the last
/// launch. Their sizes add up to `library.stats.disk_bytes`.
const APPS: &[App] = &[
    app(GAME_ID, 18.6, ("proton-9", "dxvk"), 1.0, Some(2.0)),
    app("9", 31.4, ("proton-9", "vkd3d"), 0.0, Some(6.0)),
    app("11", 7.0, ("proton-8", "dxvk"), 0.0, Some(30.0)),
    app("12", 26.0, ("proton-9", "vkd3d"), 2.0, Some(0.0)),
    app("13", 12.0, ("proton-8", "wined3d"), 0.0, None),
];

struct App {
    id: &'static str,
    size_gb: f64,
    runtime: &'static str,
    renderer: &'static str,
    dlc: f64,
    launched_days_ago: Option<f64>,
}

const fn app(
    id: &'static str,
    size_gb: f64,
    (runtime, renderer): (&'static str, &'static str),
    dlc: f64,
    launched_days_ago: Option<f64>,
) -> App {
    App {
        id,
        size_gb,
        runtime,
        renderer,
        dlc,
        launched_days_ago,
    }
}

fn game(id: &str) -> Value {
    if id == GAME_ID {
        return game_detail();
    }
    let (_, title, status) =
        GAMES
            .iter()
            .find(|g| g.0 == id)
            .copied()
            .unwrap_or((id, "Unknown", "not_installed"));
    summary(id, title, status)
}

fn with(v: Value, fields: Vec<(&str, Value)>) -> Value {
    let Value::Map(mut m) = v else { return v };
    m.extend(fields.into_iter().map(|(k, v)| (k.to_owned(), v)));
    Value::Map(m)
}

/// A game summary with only the fields of the contract's `GameSummary`.
fn entry(id: &str) -> Value {
    let (_, title, status) =
        GAMES
            .iter()
            .find(|g| g.0 == id)
            .copied()
            .unwrap_or((id, "Unknown", "not_installed"));
    let size = APPS
        .iter()
        .find(|a| a.id == id)
        .map(|a| num(a.size_gb * GB));
    let progress = match id {
        "9" => num(0.62),
        _ => Value::Null,
    };
    let update = if id == GAME_ID {
        update_hint()
    } else {
        Value::Null
    };
    with(
        summary(id, title, status),
        vec![
            ("size_on_disk", size.unwrap_or(Value::Null)),
            ("progress", progress),
            ("update", update),
        ],
    )
}

fn update_hint() -> Value {
    map(vec![
        (
            "game",
            summary(GAME_ID, "Example Quest", "update_available"),
        ),
        ("from", text("1.0.2")),
        ("to", text("1.1.0")),
        ("size", num(1.2 * GB)),
        ("removes_dlc", Value::Bool(false)),
    ])
}

fn step(name: &str, state: &str, took: Option<f64>) -> Value {
    map(vec![
        ("name", text(name)),
        ("state", text(state)),
        ("took", took.map_or(Value::Null, num)),
    ])
}

struct Job {
    id: &'static str,
    kind: &'static str,
    game: &'static str,
    state: &'static str,
    bytes: f64,
    total: f64,
}

fn job(j: Job) -> Value {
    let running = j.state == "running";
    map(vec![
        ("id", text(j.id)),
        ("kind", text(j.kind)),
        ("game", entry(j.game)),
        ("state", text(j.state)),
        ("steps", list(vec![])),
        ("bytes", num(j.bytes)),
        ("total", num(j.total)),
        ("rate", num(if running { 48_200_000.0 } else { 0.0 })),
        ("eta", if running { num(720.0) } else { Value::Null }),
        ("fraction", num(j.bytes / j.total)),
        ("files", list(vec![])),
        ("error", Value::Null),
    ])
}

/// One running job, the two queued ones `queue.summary` counts, and a failed one.
fn queue() -> Value {
    let running = with(
        job(Job {
            id: "j1",
            kind: "update",
            game: "9",
            state: "running",
            bytes: 1.24 * GB,
            total: 2.0 * GB,
        }),
        vec![
            (
                "steps",
                list(vec![
                    step("Check the build", "done", Some(3.0)),
                    step("Download", "now", None),
                    step("Install", "pending", None),
                ]),
            ),
            (
                "files",
                list(vec![map(vec![
                    ("name", text("data/tracks.pak")),
                    ("bytes", num(0.4 * GB)),
                    ("total", num(0.8 * GB)),
                    ("chunks_done", num(40.0)),
                    ("chunks_total", num(80.0)),
                    ("state", text("downloading")),
                ])]),
            ),
        ],
    );
    let queued = |id, game| {
        job(Job {
            id,
            kind: "depot",
            game,
            state: "queued",
            bytes: 0.0,
            total: 4.0 * GB,
        })
    };
    let failed = with(
        job(Job {
            id: "j4",
            kind: "verify",
            game: "16",
            state: "failed",
            bytes: 2.0 * GB,
            total: 6.0 * GB,
        }),
        vec![(
            "error",
            map(vec![
                ("class", text("CORRUPT")),
                ("title", text("Files don't match")),
                ("message", text("2 files differ from the build.")),
                ("hint", text("Repair downloads only those files.")),
                ("exit_class", text("verify")),
            ]),
        )],
    );
    list(vec![
        running,
        queued("j2", "14"),
        queued("j3", "15"),
        failed,
    ])
}

fn catalog(id: &str, latest: &str, cached: &[&str], used_by: &[&str]) -> Value {
    map(vec![
        ("id", text(id)),
        ("latest", text(latest)),
        ("cached", list(cached.iter().map(|v| text(v)).collect())),
        (
            "used_by",
            list(used_by.iter().map(|id| entry(id)).collect()),
        ),
        ("os", text("linux")),
    ])
}

fn setting(
    key: &str,
    kind: &str,
    value: Value,
    default: Value,
    source: &str,
    about: &str,
) -> Value {
    map(vec![
        ("key", text(key)),
        ("type", text(kind)),
        ("value", value),
        ("default", default),
        ("source", text(source)),
        ("description", text(about)),
        ("choices", Value::Null),
    ])
}

fn settings() -> Value {
    let off = || Value::Bool(false);
    list(vec![
        with(
            setting(
                "gui.skin",
                "text",
                text("default"),
                text("default"),
                "default",
                "The skin the app draws with",
            ),
            vec![("choices", list(vec![text("default")]))],
        ),
        with(
            setting(
                "gui.mode",
                "text",
                text("dark"),
                text("dark"),
                "default",
                "Dark or light",
            ),
            vec![("choices", list(vec![text("dark"), text("light")]))],
        ),
        setting(
            "gui.reduced_motion",
            "bool",
            off(),
            off(),
            "default",
            "Reduce motion",
        ),
        setting(
            "gui.big_screen",
            "bool",
            off(),
            off(),
            "default",
            "Larger layout for a distant screen",
        ),
        setting(
            "download.limit",
            "bytes",
            num(0.0),
            num(0.0),
            "default",
            "Download rate limit; 0 is unlimited",
        ),
        setting(
            "download.workers",
            "number",
            num(3.0),
            num(3.0),
            "default",
            "Parallel chunk connections per file",
        ),
        setting(
            "snapshots.keep_auto",
            "number",
            num(8.0),
            num(5.0),
            "file",
            "Automatic snapshots kept per game",
        ),
    ])
}

fn saves(state: &str, pull: f64, push: f64, conflict: f64, files: Vec<Value>) -> Value {
    map(vec![
        ("state", text(state)),
        ("last_sync", num(NOW - 3.0 * DAY)),
        (
            "counts",
            map(vec![
                ("pull", num(pull)),
                ("push", num(push)),
                ("delete", num(0.0)),
                ("conflict", num(conflict)),
            ]),
        ),
        ("files", list(files)),
        ("quota", Value::Null),
        ("checked_at", num(NOW - 300.0)),
    ])
}

fn save_file(path: &str, action: &str, conflict: bool) -> Value {
    map(vec![
        ("display_path", text(path)),
        ("action", text(action)),
        ("conflict", Value::Bool(conflict)),
        ("local_time", num(NOW - 2.0 * DAY)),
        ("cloud_time", num(NOW - 3.0 * DAY)),
    ])
}

fn doctor(ok: f64, warn: f64, fail: f64, checks: Vec<Value>) -> Value {
    map(vec![
        ("checked_at", num(NOW - 120.0)),
        (
            "summary",
            map(vec![
                ("ok", num(ok)),
                ("warn", num(warn)),
                ("fail", num(fail)),
            ]),
        ),
        ("checks", list(checks)),
    ])
}

fn app_entry(a: &App) -> Value {
    let launched = a.launched_days_ago.map(|d| num(NOW - d * DAY));
    map(vec![
        ("game", game(a.id)),
        ("entry_kind", text("menu")),
        ("runtime", text(a.runtime)),
        ("renderer", text(a.renderer)),
        ("dlc", num(a.dlc)),
        ("size", num(a.size_gb * GB)),
        ("last_launch", launched.unwrap_or(Value::Null)),
    ])
}

/// Add every source of this module to `f`.
pub(super) fn install(f: &mut Fixture) {
    f.set_var("library", list(GAMES.iter().map(|g| entry(g.0)).collect()));
    f.set_var("queue", queue());
    f.set_var("updates", list(vec![update_hint()]));
    f.set_var("apps", list(APPS.iter().map(app_entry).collect()));
    f.set_var(
        "runtimes",
        list(vec![
            catalog(
                "proton-9",
                "9.0-4",
                &["9.0-4", "9.0-3"],
                &[GAME_ID, "9", "12"],
            ),
            catalog("proton-8", "8.0-5", &["8.0-5"], &["11", "13"]),
        ]),
    );
    f.set_var(
        "renderers",
        list(vec![
            catalog("dxvk", "2.5", &["2.5"], &[GAME_ID, "11"]),
            catalog("vkd3d", "2.14", &["2.13"], &["9", "12"]),
            catalog("d7vk", "1.0", &[], &[]),
        ]),
    );
    f.set_var("settings", settings());

    // `game.saves` of [`GAME_ID`] is the game page's.
    f.set_call(
        "game.saves",
        "9",
        saves(
            "to_push",
            0.0,
            2.0,
            0.0,
            vec![save_file("Saves/career.sav", "push", false)],
        ),
    );
    f.set_call(
        "game.saves",
        "11",
        saves(
            "conflicts",
            0.0,
            0.0,
            1.0,
            vec![save_file("Saves/campaign.sav", "conflict", true)],
        ),
    );
    f.set_call("game.saves", "12", saves("disabled", 0.0, 0.0, 0.0, vec![]));
    f.set_call(
        "game.saves",
        "13",
        saves("no_remote_config", 0.0, 0.0, 0.0, vec![]),
    );
    f.set_call(
        "game.doctor",
        "9",
        doctor(
            4.0,
            0.0,
            0.0,
            vec![check("disk", "ok", "Enough free space")],
        ),
    );
}
