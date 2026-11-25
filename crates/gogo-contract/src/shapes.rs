//! The fields of every source (design 26 §3): what a skin may name after a source.
//!
//! The validator checks every field path in a skin against these shapes at load (S16),
//! and the types where an element needs one (design 27 §7). Shapes may refer to each
//! other in cycles (a [`GAME_SUMMARY`] carries an [`UPDATE_HINT`], which names its game),
//! so a [`Shape`] compares, hashes and prints by name only.

use std::fmt;

use crate::ParamType;

/// The type of a source field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldType {
    Text,
    /// A count or another plain number.
    Number,
    Bool,
    Bytes,
    Time,
    Duration,
    Fraction,
    GameId,
    JobId,
    /// An image handle inside [`ART`]: used by the `art` element only.
    Image,
    /// A `#rrggbb` colour inside [`ART`].
    Color,
    /// A [`crate::Status`].
    Status,
    /// A [`crate::Os`].
    Os,
    RichText,
    ActionRef,
    /// Text, a number or a yes/no: a setting's value and default, typed by the row's
    /// `type` field.
    Scalar,
    /// One of a fixed set of words.
    OneOf(&'static [&'static str]),
    Masked(&'static FieldType),
    List(&'static FieldType),
    Option(&'static FieldType),
    Record(&'static Shape),
    /// The skin's own state (`ui`): its fields are the manifest's `ui { var … }`.
    Ui,
    /// Reaches a skin only through this app-owned widget; it has no fields a skin may
    /// name (`job.log` → `log-view`, `plan.preview` → `plan-card`).
    Opaque(&'static str),
}

impl FieldType {
    /// The type without its `Option`: `?.` and `else` take care of the null.
    pub const fn inner(&self) -> &FieldType {
        match self {
            Self::Option(t) => t.inner(),
            t => t,
        }
    }

    /// The field `name` of a record, looking through `Option`. `None` for types with
    /// no fields.
    pub fn field(&self, name: &str) -> Option<&'static Field> {
        match self.inner() {
            Self::Record(s) => s.field(name),
            _ => None,
        }
    }

    /// The item type of a list, looking through `Option`.
    pub const fn item(&self) -> Option<&'static FieldType> {
        match self.inner() {
            Self::List(t) => Some(t),
            _ => None,
        }
    }

    /// A value a `when` or `on` may test: a yes/no, something that may be null, or a
    /// list (empty or not). A record, a number or text is not.
    pub const fn is_testable(&self) -> bool {
        matches!(
            self,
            Self::Bool | Self::Option(_) | Self::List(_) | Self::Scalar
        )
    }
}

impl ParamType {
    /// The argument must be written out in the skin, not read from a field: a route, a
    /// skin variable or a setting key is checked by name at load (S16, S21).
    pub const fn literal_only(self) -> bool {
        matches!(
            self,
            ParamType::Route | ParamType::UiVar | ParamType::SettingKey | ParamType::PaletteScope
        )
    }

    /// Whether a field of type `field` may be passed for this parameter (design 27 §7).
    ///
    /// This is the load-time check only, and is deliberately lenient: `Option` is looked
    /// through, so a field that may be null passes and the binding layer must still fail
    /// the action when the value is null at run time (design 27 §7, "Failures"). Text and
    /// scalars pass wherever text may, because the app parses the value when it binds.
    /// `RouteParams` and `Selection` take anything, as the action validates them itself.
    /// `Route`, `UiVar`, `SettingKey` and `PaletteScope` are names, written as plain
    /// text in the skin and never taken from a field, so no field type is accepted.
    pub fn accepts(self, field: &FieldType) -> bool {
        use FieldType as F;
        let field = field.inner();
        let text = matches!(field, F::Text | F::Scalar);
        match self {
            ParamType::GameId => *field == F::GameId,
            ParamType::GameIds | ParamType::DlcIds | ParamType::DlcIdsOrAll => {
                matches!(field, F::List(F::GameId))
            }
            ParamType::JobId => *field == F::JobId,
            ParamType::Bool => matches!(field, F::Bool | F::Scalar),
            ParamType::Index => *field == F::Number,
            ParamType::Indices => matches!(field, F::List(F::Number)),
            ParamType::Bytes => matches!(field, F::Bytes | F::Number),
            ParamType::OneOf(words) => {
                text || matches!(field, F::OneOf(w) if w.iter().all(|x| words.contains(x)))
            }
            ParamType::UiValue | ParamType::SettingValue => {
                text || matches!(field, F::Number | F::Bool | F::OneOf(_) | F::Status | F::Os)
            }
            ParamType::BoundField => !matches!(
                field,
                F::Masked(_) | F::Opaque(_) | F::Ui | F::Record(_) | F::List(_)
            ),
            ParamType::CatalogId
            | ParamType::UnusedOrCatalogId
            | ParamType::Version
            | ParamType::BuildId
            | ParamType::Lang
            | ParamType::Monitor
            | ParamType::EnvName => text,
            ParamType::RouteParams | ParamType::Selection => true,
            ParamType::Route
            | ParamType::UiVar
            | ParamType::SettingKey
            | ParamType::PaletteScope => false,
        }
    }
}

/// A named field of a [`Shape`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Field {
    pub name: &'static str,
    pub ty: FieldType,
}

/// The fields of a record. `extends` names a shape whose fields come first (`game(id)`
/// is a [`GAME_SUMMARY`] plus its own).
pub struct Shape {
    /// The doc's name (`GameSummary`), or the source and path for an unnamed record
    /// (`game.saves.counts`). Unique.
    pub name: &'static str,
    pub extends: Option<&'static Shape>,
    pub fields: &'static [Field],
}

impl Shape {
    /// The field `name`, its own or the extended shape's.
    pub fn field(&'static self, name: &str) -> Option<&'static Field> {
        self.fields
            .iter()
            .find(|f| f.name == name)
            .or_else(|| self.extends.and_then(|base| base.field(name)))
    }

    /// Every field, the extended shape's first.
    pub fn all_fields(&'static self) -> Vec<&'static Field> {
        let mut out = match self.extends {
            Some(base) => base.all_fields(),
            None => Vec::new(),
        };
        out.extend(self.fields.iter());
        out
    }
}

impl PartialEq for Shape {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for Shape {}

impl std::hash::Hash for Shape {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

impl fmt::Debug for Shape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name)
    }
}

use FieldType as T;

const fn f(name: &'static str, ty: FieldType) -> Field {
    Field { name, ty }
}

const fn shape(name: &'static str, fields: &'static [Field]) -> Shape {
    Shape {
        name,
        extends: None,
        fields,
    }
}

const OPT_TEXT: T = T::Option(&T::Text);
const OPT_BYTES: T = T::Option(&T::Bytes);
const OPT_TIME: T = T::Option(&T::Time);
const OPT_DURATION: T = T::Option(&T::Duration);
const TEXTS: T = T::List(&T::Text);
const OSES: T = T::List(&T::Os);

static GAME: T = T::Record(&GAME_SUMMARY);
static GAMES: T = T::List(&GAME);
static OPT_GAME: T = T::Option(&GAME);
static ART_T: T = T::Record(&ART);
static OPT_ART: T = T::Option(&ART_T);
static COLORS: T = T::List(&T::Color);

/// `Art` (design 26 §2): image handles and two colours, never a URL.
pub static ART: Shape = shape(
    "Art",
    &[
        f("cover", T::Image),
        f("background", T::Image),
        f("logo", T::Image),
        f("colors", COLORS),
    ],
);

static UPDATE_T: T = T::Record(&UPDATE_HINT);
static OPT_UPDATE: T = T::Option(&UPDATE_T);

pub static GAME_SUMMARY: Shape = shape(
    "GameSummary",
    &[
        f("id", T::GameId),
        f("title", T::Text),
        f("art", ART_T),
        f("status", T::Status),
        f("platforms", OSES),
        f("runs_via", OPT_TEXT),
        f("size_on_disk", OPT_BYTES),
        f("playtime", OPT_DURATION),
        f("last_played", OPT_TIME),
        f("progress", T::Option(&T::Fraction)),
        f("update", OPT_UPDATE),
    ],
);

pub static UPDATE_HINT: Shape = shape(
    "UpdateHint",
    &[
        f("game", GAME),
        f("from", T::Text),
        f("to", T::Text),
        f("size", T::Bytes),
        f("removes_dlc", T::Bool),
    ],
);

pub static ACCOUNT: Shape = shape(
    "account",
    &[
        f("signed_in", T::Bool),
        f("username", OPT_TEXT),
        f("avatar", OPT_ART),
    ],
);

pub static APP: Shape = shape(
    "app",
    &[
        f("version", T::Text),
        f("contract", T::Text),
        f("os", T::Os),
        f("reduced_motion", T::Bool),
        f("high_contrast", T::Bool),
        f("big_screen", T::Bool),
        f("locale", T::Text),
    ],
);

pub static LIBRARY_STATS: Shape = shape(
    "library.stats",
    &[
        f("count", T::Number),
        f("installed", T::Number),
        f("disk_bytes", T::Bytes),
        f("updates", T::Number),
    ],
);

pub static SEARCH_HIT: Shape = shape(
    "SearchHit",
    &[
        f("kind", T::OneOf(&["game", "dlc", "pack", "goodie"])),
        f("id", T::GameId),
        f("title", T::Text),
        f("owned", T::Bool),
        f("os", OSES),
        f("art", ART_T),
    ],
);

pub static BUILD_REF: Shape = shape("BuildRef", &[f("version", T::Text), f("build_id", T::Text)]);
static OPT_BUILD_REF: T = T::Option(&T::Record(&BUILD_REF));

pub static GAME_DETAIL: Shape = Shape {
    name: "game",
    extends: Some(&GAME_SUMMARY),
    fields: &[
        f("slug", T::Text),
        f("genre", T::Text),
        f("year", T::Number),
        f("developer", T::Text),
        f("publisher", T::Text),
        f("description", T::RichText),
        f("languages", TEXTS),
        f("installed_build", OPT_BUILD_REF),
        f(
            "lane",
            T::OneOf(&["installer", "depot", "linux_app", "mac_app", "none"]),
        ),
        f("running", T::Bool),
    ],
};

pub static BUILD: Shape = shape(
    "Build",
    &[
        f("version", T::Text),
        f("build_id", T::Text),
        f("date", T::Time),
        f("size", T::Bytes),
        f("generation", T::Number),
        f("legacy", T::Bool),
        f("newest", T::Bool),
        f("this_machine", T::Bool),
    ],
);

pub static FILE_ENTRY: Shape = shape(
    "FileEntry",
    &[
        f(
            "kind",
            T::OneOf(&["installer", "patch", "extra", "lang_pack"]),
        ),
        f("os", T::Os),
        f("lang", T::Text),
        f("name", T::Text),
        f("size", T::Bytes),
        f("downloaded", T::Bool),
        f("verified", T::Option(&T::Bool)),
    ],
);

pub static DLC: Shape = shape(
    "Dlc",
    &[
        f("id", T::GameId),
        f("title", T::Text),
        f("owned", T::Bool),
        f("installed", T::Bool),
        f("download_size", T::Bytes),
        f("disk_size", T::Bytes),
        f("in_this_build", T::Bool),
    ],
);

pub static SAVE_COUNTS: Shape = shape(
    "game.saves.counts",
    &[
        f("pull", T::Number),
        f("push", T::Number),
        f("delete", T::Number),
        f("conflict", T::Number),
    ],
);

pub static SAVE_FILE: Shape = shape(
    "SaveFile",
    &[
        f("display_path", T::Text),
        f("action", T::Text),
        f("conflict", T::Bool),
        f("local_time", OPT_TIME),
        f("cloud_time", OPT_TIME),
    ],
);

pub static SAVES: Shape = shape(
    "game.saves",
    &[
        f(
            "state",
            T::OneOf(&[
                "in_sync",
                "to_pull",
                "to_push",
                "conflicts",
                "disabled",
                "no_remote_config",
            ]),
        ),
        f("last_sync", OPT_TIME),
        f("counts", T::Record(&SAVE_COUNTS)),
        f("files", T::List(&T::Record(&SAVE_FILE))),
        f("quota", OPT_BYTES),
        f("checked_at", T::Time),
    ],
);

pub static SNAPSHOT: Shape = shape(
    "Snapshot",
    &[
        f("n", T::Number),
        f("date", T::Time),
        f("kind", T::OneOf(&["initial", "manual", "auto"])),
        f("label", T::Text),
        f("build", T::Text),
        f("dlc_count", T::Number),
        f("exclusive", OPT_BYTES),
        f("estimate", T::Bool),
    ],
);

pub static SNAPSHOTS: Shape = shape(
    "game.snapshots",
    &[
        f("auto", T::Bool),
        f("items", T::List(&T::Record(&SNAPSHOT))),
    ],
);

pub static RUNNER_RUNTIME: Shape = shape(
    "game.runner.runtime",
    &[f("id", T::Text), f("version", T::Text)],
);

pub static RUNNER_DISPLAY: Shape = shape(
    "game.runner.display",
    &[
        f("mode", T::OneOf(&["wayland", "x11"])),
        f("hdr", T::Bool),
        f("monitor", OPT_TEXT),
    ],
);

pub static RUNNER_GALAXY: Shape = shape(
    "game.runner.galaxy",
    &[
        f("setting", T::OneOf(&["on", "off", "default"])),
        f(
            "state",
            T::OneOf(&["signed_in", "signed_out", "not_installed"]),
        ),
    ],
);

pub static RUNNER: Shape = shape(
    "game.runner",
    &[
        f("platform", T::OneOf(&["linux", "mac"])),
        f("renderer", T::Text),
        f("renderer_options", TEXTS),
        f("runtime", T::Record(&RUNNER_RUNTIME)),
        f("runtime_options", TEXTS),
        f("display", T::Record(&RUNNER_DISPLAY)),
        f("galaxy", T::Record(&RUNNER_GALAXY)),
        f("wrapper", TEXTS),
        f("args", TEXTS),
    ],
);

/// Where an env value comes from, in the order a later one beats an earlier one.
pub const ENV_SOURCES: &[&str] = &[
    "default",
    "renderer",
    "compat",
    "override",
    "launcher",
    "protonfix",
];

pub static ENV_VAR: Shape = shape(
    "EnvVar",
    &[
        f("name", T::Text),
        f("value", T::Masked(&T::Text)),
        f("source", T::OneOf(ENV_SOURCES)),
        f("beats", T::List(&T::OneOf(ENV_SOURCES))),
        f("locked", T::Bool),
    ],
);

pub static EXE: Shape = shape(
    "Exe",
    &[
        f("n", T::Number),
        f("name", T::Text),
        f("category", T::Text),
        f("primary", T::Bool),
        f("launched", T::Bool),
        f("bits", T::Number),
    ],
);

pub static CHECK_SUMMARY: Shape = shape(
    "game.doctor.summary",
    &[
        f("ok", T::Number),
        f("warn", T::Number),
        f("fail", T::Number),
    ],
);

pub static CHECK: Shape = shape(
    "Check",
    &[
        f("name", T::Text),
        f("result", T::OneOf(&["ok", "warn", "fail"])),
        f("message", T::Text),
        f("hint", T::Text),
        f("fix", T::Option(&T::ActionRef)),
    ],
);

pub static DOCTOR: Shape = shape(
    "game.doctor",
    &[
        f("checked_at", T::Time),
        f("summary", T::Record(&CHECK_SUMMARY)),
        f("checks", T::List(&T::Record(&CHECK))),
    ],
);

pub static COMPAT_VALUE: Shape = shape(
    "CompatValue",
    &[f("key", T::Text), f("value", T::Text), f("source", T::Text)],
);

pub static COMPAT: Shape = shape(
    "game.compat",
    &[
        f("items", T::List(&T::Record(&COMPAT_VALUE))),
        f("protonfix", OPT_TEXT),
        f("checked_at", T::Time),
    ],
);

pub static RECIPE_STEP: Shape = shape(
    "Recipe.steps",
    &[
        f("verb", T::Text),
        f("applied", T::Bool),
        f("reason", T::Text),
    ],
);

pub static RECIPE: Shape = shape(
    "Recipe",
    &[
        f("slug", T::Text),
        f("rank", T::Number),
        f("steps", T::List(&T::Record(&RECIPE_STEP))),
    ],
);

pub static ERROR_VIEW: Shape = shape(
    "ErrorView",
    &[
        f("class", T::Text),
        f("title", T::Text),
        f("message", T::Text),
        f("hint", T::Text),
        f("exit_class", T::Text),
    ],
);

pub static STEP: Shape = shape(
    "Step",
    &[
        f("name", T::Text),
        f(
            "state",
            T::OneOf(&["done", "now", "pending", "failed", "asks_first"]),
        ),
        f("took", OPT_DURATION),
    ],
);

pub static FILE_PROGRESS: Shape = shape(
    "FileProgress",
    &[
        f("name", T::Text),
        f("bytes", T::Bytes),
        f("total", T::Bytes),
        f("chunks_done", T::Number),
        f("chunks_total", T::Number),
        f("state", T::Text),
    ],
);

pub static JOB: Shape = shape(
    "Job",
    &[
        f("id", T::JobId),
        f(
            "kind",
            T::OneOf(&[
                "download",
                "depot",
                "update",
                "setup",
                "dlc_add",
                "dlc_remove",
                "saves",
                "runtime_install",
                "move",
                "rollback",
                "verify",
            ]),
        ),
        f("game", OPT_GAME),
        f(
            "state",
            T::OneOf(&["queued", "running", "paused", "failed", "done", "cancelled"]),
        ),
        f("steps", T::List(&T::Record(&STEP))),
        f("bytes", T::Bytes),
        f("total", OPT_BYTES),
        f("rate", T::Bytes),
        f("eta", OPT_DURATION),
        f("fraction", T::Option(&T::Fraction)),
        f("files", T::List(&T::Record(&FILE_PROGRESS))),
        f("error", T::Option(&T::Record(&ERROR_VIEW))),
    ],
);
static JOB_T: T = T::Record(&JOB);

pub static QUEUE_SUMMARY: Shape = shape(
    "queue.summary",
    &[
        f("active", T::Number),
        f("queued", T::Number),
        f("rate", T::Bytes),
        f("rate_limit", OPT_BYTES),
        f("current", T::Option(&JOB_T)),
    ],
);

pub static APP_ENTRY: Shape = shape(
    "App",
    &[
        f("game", GAME),
        f("entry_kind", T::OneOf(&["menu", "app_bundle"])),
        f("runtime", T::Text),
        f("renderer", T::Text),
        f("dlc", T::Number),
        f("size", T::Bytes),
        f("last_launch", OPT_TIME),
    ],
);

pub static CATALOG_ENTRY: Shape = shape(
    "CatalogEntry",
    &[
        f("id", T::Text),
        f("latest", T::Text),
        f("cached", TEXTS),
        f("used_by", GAMES),
        f("os", T::Os),
    ],
);

pub static DOCTOR_ROW: Shape = shape(
    "doctor.all",
    &[f("game", GAME), f("summary", T::Record(&CHECK_SUMMARY))],
);

pub static SETTING: Shape = shape(
    "Setting",
    &[
        f("key", T::Text),
        f("type", T::Text),
        f("value", T::Scalar),
        f("default", T::Scalar),
        f("source", T::OneOf(&["file", "env", "default"])),
        f("description", T::Text),
        f("choices", T::Option(&TEXTS)),
    ],
);

/// Every shape, so a name finds its table entry. Names are unique.
pub static SHAPES: &[&Shape] = &[
    &ART,
    &GAME_SUMMARY,
    &UPDATE_HINT,
    &ACCOUNT,
    &APP,
    &LIBRARY_STATS,
    &SEARCH_HIT,
    &BUILD_REF,
    &GAME_DETAIL,
    &BUILD,
    &FILE_ENTRY,
    &DLC,
    &SAVE_COUNTS,
    &SAVE_FILE,
    &SAVES,
    &SNAPSHOT,
    &SNAPSHOTS,
    &RUNNER_RUNTIME,
    &RUNNER_DISPLAY,
    &RUNNER_GALAXY,
    &RUNNER,
    &ENV_VAR,
    &EXE,
    &CHECK_SUMMARY,
    &CHECK,
    &DOCTOR,
    &COMPAT_VALUE,
    &COMPAT,
    &RECIPE_STEP,
    &RECIPE,
    &ERROR_VIEW,
    &STEP,
    &FILE_PROGRESS,
    &JOB,
    &QUEUE_SUMMARY,
    &APP_ENTRY,
    &CATALOG_ENTRY,
    &DOCTOR_ROW,
    &SETTING,
];

/// What a source gives a skin, by source name (the names of [`crate::sources::SOURCES`]).
pub fn source_shape(source: &str) -> Option<FieldType> {
    static SEARCH: T = T::Record(&SEARCH_HIT);
    static BUILDS: T = T::Record(&BUILD);
    static FILES: T = T::Record(&FILE_ENTRY);
    static DLCS: T = T::Record(&DLC);
    static ENV: T = T::Record(&ENV_VAR);
    static EXES: T = T::Record(&EXE);
    static RECIPES: T = T::Record(&RECIPE);
    static UPDATES: T = T::Record(&UPDATE_HINT);
    static APPS: T = T::Record(&APP_ENTRY);
    static CATALOG: T = T::Record(&CATALOG_ENTRY);
    static DOCTOR_ROWS: T = T::Record(&DOCTOR_ROW);
    static SETTINGS: T = T::Record(&SETTING);
    static PLAN: T = T::Opaque("plan-card");
    Some(match source {
        "account" => T::Record(&ACCOUNT),
        "app" => T::Record(&APP),
        "ui" => T::Ui,
        "library" => GAMES,
        "library.stats" => T::Record(&LIBRARY_STATS),
        "search" => T::List(&SEARCH),
        "game" => T::Record(&GAME_DETAIL),
        "game.builds" => T::List(&BUILDS),
        "game.files" => T::List(&FILES),
        "game.dlc" => T::List(&DLCS),
        "game.saves" => T::Record(&SAVES),
        "game.snapshots" => T::Record(&SNAPSHOTS),
        "game.runner" => T::Record(&RUNNER),
        "game.env" => T::List(&ENV),
        "game.exes" => T::List(&EXES),
        "game.doctor" => T::Record(&DOCTOR),
        "game.compat" => T::Record(&COMPAT),
        "game.recipes" => T::List(&RECIPES),
        "queue" => T::List(&JOB_T),
        "queue.summary" => T::Record(&QUEUE_SUMMARY),
        "job" => JOB_T,
        "job.log" => T::Opaque("log-view"),
        "updates" => T::List(&UPDATES),
        "apps" => T::List(&APPS),
        "runtimes" | "renderers" => T::List(&CATALOG),
        "doctor.all" => T::List(&DOCTOR_ROWS),
        "settings" => T::List(&SETTINGS),
        "plan.preview" => T::Option(&PLAN),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::sources::SOURCES;

    #[test]
    fn every_source_has_a_shape_and_no_shape_is_spare() {
        for s in SOURCES {
            assert!(source_shape(s.name).is_some(), "{} has no shape", s.name);
        }
        assert!(source_shape("nope").is_none());
    }

    #[test]
    fn shape_names_are_unique_and_fields_too() {
        let mut names = BTreeSet::new();
        for s in SHAPES {
            assert!(names.insert(s.name), "shape {} twice", s.name);
            let mut fields = BTreeSet::new();
            for f in s.all_fields() {
                assert!(fields.insert(f.name), "{}.{} twice", s.name, f.name);
            }
        }
    }

    #[test]
    fn every_record_is_in_the_table() {
        fn walk(t: &FieldType, seen: &mut BTreeSet<&'static str>) {
            match t {
                FieldType::Record(s) => {
                    assert!(
                        SHAPES.iter().any(|x| x.name == s.name),
                        "{} is not in SHAPES",
                        s.name
                    );
                    if seen.insert(s.name) {
                        for f in s.all_fields() {
                            walk(&f.ty, seen);
                        }
                    }
                }
                FieldType::Masked(t) | FieldType::List(t) | FieldType::Option(t) => walk(t, seen),
                _ => {}
            }
        }
        let mut seen = BTreeSet::new();
        for s in SOURCES {
            walk(&source_shape(s.name).expect("checked above"), &mut seen);
        }
        let all: BTreeSet<_> = SHAPES.iter().map(|s| s.name).collect();
        assert_eq!(seen, all, "a shape no source reaches");
    }

    #[test]
    fn lookups_follow_extends_options_and_cycles() {
        let game = source_shape("game").expect("game");
        assert_eq!(game.field("title").map(|f| f.ty), Some(FieldType::Text));
        assert_eq!(game.field("running").map(|f| f.ty), Some(FieldType::Bool));
        let update = game.field("update").expect("update");
        let back = update.ty.field("game").expect("game");
        assert_eq!(
            back.ty.field("status").map(|f| f.ty),
            Some(FieldType::Status)
        );
        assert!(game.field("titel").is_none());
        let builds = source_shape("game.builds").expect("builds");
        assert!(builds.item().and_then(|b| b.field("newest")).is_some());
        assert!(builds.field("newest").is_none(), "a list has no fields");
        assert!(
            source_shape("job.log")
                .expect("log")
                .field("line")
                .is_none()
        );
    }

    #[test]
    fn action_arguments_by_type() {
        use FieldType as F;
        assert!(ParamType::GameId.accepts(&F::GameId));
        assert!(ParamType::GameId.accepts(&F::Option(&F::GameId)));
        assert!(!ParamType::GameId.accepts(&F::Text));
        assert!(!ParamType::GameId.accepts(&F::Record(&GAME_SUMMARY)));
        assert!(ParamType::Bool.accepts(&F::Bool));
        assert!(!ParamType::Bool.accepts(&F::Number));
        assert!(ParamType::Index.accepts(&F::Number));
        assert!(ParamType::OneOf(&["a", "b"]).accepts(&F::OneOf(&["a"])));
        assert!(!ParamType::OneOf(&["a", "b"]).accepts(&F::OneOf(&["a", "c"])));
        assert!(!ParamType::BoundField.accepts(&F::Masked(&F::Text)));
        assert!(ParamType::BoundField.accepts(&F::Text));
        assert!(ParamType::UiValue.accepts(&F::Status));
        assert!(ParamType::SettingValue.accepts(&F::Os));
        for literal in [ParamType::Route, ParamType::UiVar, ParamType::SettingKey] {
            assert!(literal.literal_only());
            assert!(!literal.accepts(&F::Text));
        }
    }

    #[test]
    fn testable_types() {
        assert!(FieldType::Bool.is_testable());
        assert!(FieldType::Option(&FieldType::Text).is_testable());
        assert!(FieldType::List(&FieldType::Text).is_testable());
        assert!(!FieldType::Text.is_testable());
        assert!(!FieldType::Number.is_testable());
        assert!(!FieldType::Record(&ART).is_testable());
    }
}
