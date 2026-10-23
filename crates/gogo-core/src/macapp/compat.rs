//! Per-game compat data (docs/19 §6.4–§6.5, design 16 decision 5): one
//! declarative entry per GOG product id. The same schema serves the curated
//! compat DB (L3, `compat/` at the repo root, compiled in) and the user's
//! override file (L4, `<state_dir>/compat/<gogid>.toml`). The other layers
//! (L0 default runtime, L1 auto renderer, L2 GOG-native) are computed by the
//! pipeline and fed in as [`CompatEntry`] values of their own.
//!
//! [`merge`] applies `flags > L4 > L3 > L2 > L1 > L0` field by field and
//! keeps the winning layer of every field (of every key or verb for the
//! fields that combine across layers), so `gogo mac-app compat` (and
//! later `doctor`, #79) can show where a value came from. The build and
//! setup apply the result (#74, design 16 §5); this module only parses,
//! merges and writes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::error::MacAppError;

/// One compatibility layer, in ascending precedence (`Ord` = who wins).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Layer {
    /// L0: the default runtime.
    #[serde(rename = "L0")]
    Default,
    /// L1: picked from the game's PE imports (#63): renderer, and the
    /// winetricks verbs for its DirectX helper DLLs (#72).
    #[serde(rename = "L1")]
    Auto,
    /// L2: GOG-native fixes (support overlay, registry, `goggame-*.info`).
    #[serde(rename = "L2")]
    Gog,
    /// L3: gogo's compat DB.
    #[serde(rename = "L3")]
    Db,
    /// L4: the user's override file.
    #[serde(rename = "L4")]
    User,
    /// Flags of the current gogo invocation (not persisted; see [`set_user_keys`]).
    #[serde(rename = "flags")]
    Flags,
}

impl Layer {
    /// Short tag plus what the layer is, for human output.
    pub fn describe(self) -> &'static str {
        match self {
            Layer::Default => "L0 default",
            Layer::Auto => "L1 auto",
            Layer::Gog => "L2 GOG-native",
            Layer::Db => "L3 compat DB",
            Layer::User => "L4 user override",
            Layer::Flags => "flags",
        }
    }
}

/// How well a game runs, as last observed (docs/19 §6.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Unknown,
    Broken,
    Boots,
    Plays,
}

/// How a bundle got its game (#56): from the depot build (design 15, the
/// default) or by running GOG's offline installer. An entry's `lane` says
/// which one its `status` / `notes` / `tested` were seen on; some games
/// differ (Pharaoh asks for its CD from the depot, not once installed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lane {
    Depot,
    Installer,
}

impl Lane {
    /// For human output: "the depot build", "the offline installer".
    pub fn describe(self) -> &'static str {
        match self {
            Lane::Depot => "the depot build",
            Lane::Installer => "the offline installer",
        }
    }
}

/// One registry value to set in the prefix (`reg add` shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryValue {
    /// Full key, e.g. `HKCU\Software\Wine\Direct3D`.
    pub key: String,
    /// Value name; absent = the key's default value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `REG_SZ` (default), `REG_DWORD`, …
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub data: String,
}

/// umu-protonfixes' `replace_command`: substitute part of the launch command
/// (typically a launcher exe for the real game exe).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplaceCommand {
    pub from: String,
    pub to: String,
}

/// Where and when an entry was tested.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tested {
    pub runtime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// A TOML date (`2026-10-22`); kept as its string form.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_datetime"
    )]
    pub date: Option<toml::value::Datetime>,
}

fn ser_datetime<S: serde::Serializer>(
    d: &Option<toml::value::Datetime>,
    s: S,
) -> Result<S::Ok, S::Error> {
    match d {
        Some(d) => s.serialize_str(&d.to_string()),
        None => s.serialize_none(),
    }
}

/// One layer's worth of compat data. Every field is optional: absent means
/// "this layer has no opinion". Maps are merged per key, `verbs` edit the
/// list below them (#128, [`VERBS_CLEAR`]), everything else is replaced whole.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompatEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Entry point, relative to the game dir (or a `C:\` path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
    /// `wined3d`, `dxmt[/<ver>]`, `cnc-ddraw[/<ver>]`, `d3dmetal[/<ver>]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<String>,
    /// Store runtime `<id>[/<ver>]` (design 16 decision 6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// winetricks verbs (applied by setup, never by the launcher). They add
    /// to the lower layers' verbs; `-<verb>` removes one, `-*` all of them
    /// (see [`merge`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbs: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dll_overrides: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<Vec<RegistryValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    /// Which `[[variant]]` of the bundle to run (a `playTasks` name, #85),
    /// instead of the default. Setup applies it over the baked `exe`/`args`;
    /// an L4 `exe`/`args` still win over it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace_command: Option<ReplaceCommand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tested: Option<Vec<Tested>>,
    /// One line on why the game has its `status` (what breaks, what works).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// The build lane `status`, `notes` and `tested` were seen on; absent =
    /// both (#56).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<Lane>,
    /// Provenance: `gogo-vm`, `umu-protonfixes:<file>@<sha>`, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// In a `verbs` list: drop every verb the layers below (and the entries
/// before it) added. `["-*", "d3dx9"]` replaces the list whole.
pub const VERBS_CLEAR: &str = "-*";

/// Top-level keys of the schema (unknown ones warn, like the config loader).
const KNOWN_KEYS: &[&str] = &[
    "title",
    "exe",
    "renderer",
    "runtime",
    "verbs",
    "dll_overrides",
    "env",
    "registry",
    "args",
    "variant",
    "replace_command",
    "status",
    "tested",
    "notes",
    "lane",
    "source",
];

/// Keys gogo itself writes into L4 (`--retarget`, persisted build flags;
/// design 16 decisions 5 and 7). Everything else in the file is the user's.
pub const OWNED_KEYS: &[&str] = &["runtime", "renderer"];

/// A parsed layer plus non-fatal warnings (unknown keys).
#[derive(Debug, Clone, Default)]
pub struct Parsed {
    pub entry: CompatEntry,
    pub warnings: Vec<String>,
}

/// Parse one entry. Unknown keys (top level, and inside `tested` /
/// `registry` items) are reported as warnings, never errors: an older gogo
/// must still read a newer DB.
pub fn parse(raw: &str) -> Result<Parsed, String> {
    let table: toml::Table = toml::from_str(raw).map_err(|e| e.to_string())?;
    let mut warnings = Vec::new();
    for (key, value) in &table {
        if !KNOWN_KEYS.contains(&key.as_str()) {
            warnings.push(key.clone());
            continue;
        }
        let item_keys: &[&str] = match key.as_str() {
            "tested" => &["runtime", "host", "date"],
            "registry" => &["key", "name", "type", "data"],
            _ => continue,
        };
        for (i, item) in value.as_array().into_iter().flatten().enumerate() {
            for k in item.as_table().into_iter().flat_map(|t| t.keys()) {
                if !item_keys.contains(&k.as_str()) {
                    warnings.push(format!("{key}[{i}].{k}"));
                }
            }
        }
    }
    // Straight from the text, not via `toml::Value`: the value
    // deserializer hands dates over as strings.
    let entry: CompatEntry = toml::from_str(raw).map_err(|e| e.to_string())?;
    Ok(Parsed { entry, warnings })
}

/// A GOG product id is all digits; anything else would be a path (`..`).
pub fn validate_gogid(gogid: &str) -> Result<(), MacAppError> {
    if !gogid.is_empty() && gogid.bytes().all(|b| b.is_ascii_digit()) {
        Ok(())
    } else {
        Err(MacAppError::Compat {
            path: PathBuf::from(gogid),
            reason: "a GOG product id is all digits".into(),
        })
    }
}

// — L3: the compiled-in compat DB —

// `(gogid, file contents)` for every `compat/<gogid>.toml` (build.rs).
include!(concat!(env!("OUT_DIR"), "/compat_db.rs"));

/// `GOGO_COMPAT_DIR`: a directory of `<gogid>.toml` files that shadow the
/// compiled-in DB, for trying a new entry before it is contributed (and for
/// tests). Its entries are still L3.
pub const DB_DIR_ENV: &str = "GOGO_COMPAT_DIR";

/// The L3 entry for `gogid`: `$GOGO_COMPAT_DIR/<gogid>.toml` when that
/// exists, else the compiled-in DB's. A DB file that fails to parse reads
/// as "no entry" plus a warning (compiled in, it is a build-time bug).
pub fn db_entry(gogid: &str) -> Parsed {
    if let Some(dir) = std::env::var_os(DB_DIR_ENV).filter(|d| !d.is_empty())
        && validate_gogid(gogid).is_ok()
    {
        let path = Path::new(&dir).join(format!("{gogid}.toml"));
        if let Ok(raw) = std::fs::read_to_string(&path) {
            return parse(&raw).unwrap_or_else(|e| Parsed {
                entry: CompatEntry::default(),
                warnings: vec![format!(
                    "compat DB entry {} unreadable: {e}",
                    path.display()
                )],
            });
        }
    }
    db_lookup(COMPAT_DB, gogid)
}

fn db_lookup(db: &[(&str, &str)], gogid: &str) -> Parsed {
    match db.iter().find(|(id, _)| *id == gogid) {
        Some((_, raw)) => parse(raw).unwrap_or_else(|e| Parsed {
            entry: CompatEntry::default(),
            warnings: vec![format!("compat DB entry {gogid} unreadable: {e}")],
        }),
        None => Parsed::default(),
    }
}

/// Number of entries in the compiled-in DB.
pub fn db_len() -> usize {
    COMPAT_DB.len()
}

// — L4: the user's override file —

/// `<state_dir>/compat/` (design 16 §0; `GOGO_HOME` flattens).
pub fn user_dir() -> PathBuf {
    crate::state::state_dir().join("compat")
}

/// `<state_dir>/compat/<gogid>.toml`.
pub fn user_path(gogid: &str) -> PathBuf {
    user_dir().join(format!("{gogid}.toml"))
}

/// Load the L4 file for `gogid`. A missing file is an empty layer; a file
/// that doesn't parse is an error (it is the user's own edit — say so
/// rather than silently ignoring it).
pub fn load_user(gogid: &str) -> Result<Parsed, MacAppError> {
    validate_gogid(gogid)?;
    load_user_at(&user_path(gogid))
}

fn load_user_at(path: &Path) -> Result<Parsed, MacAppError> {
    match std::fs::read_to_string(path) {
        Ok(raw) => parse(&raw).map_err(|reason| MacAppError::Compat {
            path: path.to_path_buf(),
            reason,
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Parsed::default()),
        Err(e) => Err(e.into()),
    }
}

/// One key gogo changed in an L4 file (reported to the user, decision 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub key: String,
    pub old: Option<String>,
    pub new: Option<String>,
}

/// Set (`Some`) or remove (`None`) gogo-owned keys in the L4 file for
/// `gogid`, keeping everything else (comments included) as the user wrote it.
/// Returns only the keys whose value actually changed; no change, no write.
pub fn set_user_keys(
    gogid: &str,
    updates: &[(&str, Option<&str>)],
) -> Result<Vec<Change>, MacAppError> {
    validate_gogid(gogid)?;
    set_user_keys_at(&user_path(gogid), updates)
}

fn set_user_keys_at(
    path: &Path,
    updates: &[(&str, Option<&str>)],
) -> Result<Vec<Change>, MacAppError> {
    let err = |reason: String| MacAppError::Compat {
        path: path.to_path_buf(),
        reason,
    };
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let mut doc: toml_edit::DocumentMut = raw.parse().map_err(|e| err(format!("{e}")))?;
    let mut changes = Vec::new();
    for (key, new) in updates {
        if !OWNED_KEYS.contains(key) {
            return Err(err(format!("`{key}` is not a key gogo writes")));
        }
        let old = doc.get(key).and_then(|i| i.as_str()).map(str::to_string);
        if old.as_deref() == *new {
            continue;
        }
        match new {
            Some(v) => doc[*key] = toml_edit::value(*v),
            None => {
                doc.remove(key);
            }
        }
        changes.push(Change {
            key: key.to_string(),
            old,
            new: new.map(str::to_string),
        });
    }
    if changes.is_empty() {
        return Ok(changes);
    }
    let out = doc.to_string();
    // Never leave a file behind that the next load would reject.
    parse(&out).map_err(err)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, path)?;
    Ok(changes)
}

// — merge —

/// A value plus the layer it came from.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Sourced<T> {
    pub value: T,
    pub layer: Layer,
}

/// The merged config of every layer, with the winning layer per field (per
/// key for the maps, per verb for `verbs`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Effective {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<Sourced<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exe: Option<Sourced<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renderer: Option<Sourced<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<Sourced<String>>,
    /// In order, each with the layer that added it.
    pub verbs: Vec<Sourced<String>>,
    pub dll_overrides: BTreeMap<String, Sourced<String>>,
    pub env: BTreeMap<String, Sourced<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<Sourced<Vec<RegistryValue>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Sourced<Vec<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<Sourced<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replace_command: Option<Sourced<ReplaceCommand>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Sourced<Status>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tested: Option<Sourced<Vec<Tested>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<Sourced<String>>,
    /// Goes with `status`: a layer that sets one sets both (#56).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane: Option<Sourced<Lane>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Sourced<String>>,
}

/// Merge layers by precedence (design 16 decision 5), field by field. The
/// input order doesn't matter; two entries for the same layer apply in input
/// order (the later wins).
///
/// `verbs` combine instead (#128): each layer's list applies in order to the
/// list the layers below built. `<verb>` adds it unless it's there already
/// (it keeps its place and the layer that added it first), `-<verb>`
/// removes it, [`VERBS_CLEAR`] removes all. winetricks options start with
/// `-` too, so no real verb is lost to the prefix.
pub fn merge(layers: &[(Layer, &CompatEntry)]) -> Effective {
    let mut sorted: Vec<&(Layer, &CompatEntry)> = layers.iter().collect();
    sorted.sort_by_key(|(layer, _)| *layer);
    let mut out = Effective::default();
    fn take<T: Clone>(slot: &mut Option<Sourced<T>>, v: &Option<T>, layer: Layer) {
        if let Some(v) = v {
            *slot = Some(Sourced {
                value: v.clone(),
                layer,
            });
        }
    }
    fn take_map(
        slot: &mut BTreeMap<String, Sourced<String>>,
        m: &BTreeMap<String, String>,
        layer: Layer,
    ) {
        for (k, v) in m {
            slot.insert(
                k.clone(),
                Sourced {
                    value: v.clone(),
                    layer,
                },
            );
        }
    }
    // Ascending precedence: each layer overwrites what the lower ones set.
    for (layer, e) in sorted {
        let layer = *layer;
        take(&mut out.title, &e.title, layer);
        take(&mut out.exe, &e.exe, layer);
        take(&mut out.renderer, &e.renderer, layer);
        take(&mut out.runtime, &e.runtime, layer);
        apply_verbs(
            &mut out.verbs,
            e.verbs.as_deref().unwrap_or_default(),
            layer,
        );
        take_map(&mut out.dll_overrides, &e.dll_overrides, layer);
        take_map(&mut out.env, &e.env, layer);
        take(&mut out.registry, &e.registry, layer);
        take(&mut out.args, &e.args, layer);
        take(&mut out.variant, &e.variant, layer);
        take(&mut out.replace_command, &e.replace_command, layer);
        if e.status.is_some() {
            out.lane = None;
        }
        take(&mut out.status, &e.status, layer);
        take(&mut out.lane, &e.lane, layer);
        take(&mut out.tested, &e.tested, layer);
        take(&mut out.notes, &e.notes, layer);
        take(&mut out.source, &e.source, layer);
    }
    out
}

/// One layer's `verbs` applied to the list below it (see [`merge`]).
fn apply_verbs(list: &mut Vec<Sourced<String>>, edits: &[String], layer: Layer) {
    for edit in edits {
        if edit == VERBS_CLEAR {
            list.clear();
        } else if let Some(verb) = edit.strip_prefix('-') {
            list.retain(|s| s.value != verb);
        } else if !list.iter().any(|s| &s.value == edit) {
            list.push(Sourced {
                value: edit.clone(),
                layer,
            });
        }
    }
}

impl Effective {
    /// `key = value  (layer)` lines for human output, in schema order.
    pub fn display_lines(&self) -> Vec<String> {
        fn line<T: std::fmt::Debug>(out: &mut Vec<String>, key: &str, s: &Option<Sourced<T>>) {
            if let Some(s) = s {
                out.push(format!("{key} = {:?}  ({})", s.value, s.layer.describe()));
            }
        }
        fn map(out: &mut Vec<String>, key: &str, m: &BTreeMap<String, Sourced<String>>) {
            for (k, s) in m {
                out.push(format!(
                    "{key}.{k} = {:?}  ({})",
                    s.value,
                    s.layer.describe()
                ));
            }
        }
        let mut out = Vec::new();
        line(&mut out, "title", &self.title);
        line(&mut out, "exe", &self.exe);
        line(&mut out, "renderer", &self.renderer);
        line(&mut out, "runtime", &self.runtime);
        for s in &self.verbs {
            out.push(format!("verbs += {:?}  ({})", s.value, s.layer.describe()));
        }
        map(&mut out, "dll_overrides", &self.dll_overrides);
        map(&mut out, "env", &self.env);
        line(&mut out, "registry", &self.registry);
        line(&mut out, "args", &self.args);
        line(&mut out, "variant", &self.variant);
        line(&mut out, "replace_command", &self.replace_command);
        line(&mut out, "status", &self.status);
        if let Some(t) = &self.tested {
            out.push(format!(
                "tested = {} run(s)  ({})",
                t.value.len(),
                t.layer.describe()
            ));
        }
        line(&mut out, "notes", &self.notes);
        line(&mut out, "lane", &self.lane);
        line(&mut out, "source", &self.source);
        out
    }

    /// The pre-confirm status line (docs/19 §6.6): `known to boot on
    /// winecx-24.0.7_7 (2026-10-22)`, `untested`, `known broken: <notes>`.
    /// The runtime is the most recent `tested` run (the last one when none
    /// carries a date).
    pub fn status_line(&self) -> String {
        self.status_line_for(None)
    }

    /// The status as it applies to a bundle built from `lane` (#56): one
    /// seen on the other lane doesn't count for it, so it reads "untested"
    /// with the other lane's line in brackets. `None` = any lane.
    pub fn status_for(&self, lane: Option<Lane>) -> Status {
        match (self.lane.as_ref().map(|l| l.value), lane) {
            (Some(seen), Some(built)) if seen != built => Status::Unknown,
            _ => self.status.as_ref().map_or(Status::Unknown, |s| s.value),
        }
    }

    /// [`status_line`](Self::status_line) for a bundle built from `lane`.
    pub fn status_line_for(&self, lane: Option<Lane>) -> String {
        let line = self.lane_free_line();
        let seen = self.lane.as_ref().map(|l| l.value);
        match (seen, lane) {
            _ if self.status_for(None) == Status::Unknown => line,
            (Some(seen), Some(built)) if seen != built => format!(
                "untested from {} ({}: {line})",
                built.describe(),
                seen.describe()
            ),
            (Some(seen), None) => format!("{line} (from {})", seen.describe()),
            _ => line,
        }
    }

    fn lane_free_line(&self) -> String {
        let status = self.status.as_ref().map_or(Status::Unknown, |s| s.value);
        let verb = match status {
            Status::Unknown => return "untested".into(),
            Status::Broken => {
                return match &self.notes {
                    Some(n) => format!("known broken: {}", n.value),
                    None => "known broken".into(),
                };
            }
            Status::Boots => "boot",
            Status::Plays => "play",
        };
        let latest = self.tested.as_ref().and_then(|t| {
            t.value
                .iter()
                .enumerate()
                .max_by_key(|(i, t)| (t.date.map(|d| d.to_string()), *i))
                .map(|(_, t)| t)
        });
        match latest {
            Some(t) => match t.date {
                Some(d) => format!("known to {verb} on {} ({d})", t.runtime),
                None => format!("known to {verb} on {}", t.runtime),
            },
            None => format!("known to {verb}"),
        }
    }
}

/// L3 merged with L4 for `gogid` (what the build reads before confirming),
/// plus both layers' warnings. An unreadable L4 file is a warning here, not
/// an error: setup refuses it later, with the path.
pub fn db_and_user(gogid: &str) -> (Effective, Vec<String>) {
    let db = db_entry(gogid);
    let mut warnings = db.warnings;
    let user = match load_user(gogid) {
        Ok(p) => {
            let path = user_path(gogid);
            warnings.extend(
                p.warnings
                    .iter()
                    .map(|w| format!("{}: unknown key {w}", path.display())),
            );
            p.entry
        }
        Err(e) => {
            warnings.push(e.to_string());
            CompatEntry::default()
        }
    };
    let eff = merge(&[(Layer::Db, &db.entry), (Layer::User, &user)]);
    (eff, warnings)
}

#[cfg(test)]
mod compat_tests {
    use super::*;

    const REBEL_GALAXY: &str = r#"
# compat/1435582019.toml — Rebel Galaxy
title    = "Rebel Galaxy"
exe      = "RebelGalaxyGOG.exe"        # skip GoGLauncher.exe
renderer = "dxmt"
verbs    = ["d3dcompiler_43", "d3dx11_43"]
status   = "boots"
tested   = [{ runtime = "winecx-24.0.7_7", host = "m4pro/macos-27.0.1", date = 2026-10-22 }]
source   = "gogo-vm"
notes    = "boots to the main menu"
env      = { WINEMSYNC = "1" }
dll_overrides = { ddraw = "n,b" }
registry = [{ key = 'HKCU\Software\Wine\Direct3D', name = "renderer", data = "gl" }]
args     = ["-windowed"]
replace_command = { from = "GoGLauncher.exe", to = "RebelGalaxyGOG.exe" }
"#;

    #[test]
    fn parses_the_docs19_example() {
        let p = parse(REBEL_GALAXY).unwrap();
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        let e = p.entry;
        assert_eq!(e.title.as_deref(), Some("Rebel Galaxy"));
        assert_eq!(e.renderer.as_deref(), Some("dxmt"));
        assert_eq!(e.status, Some(Status::Boots));
        assert_eq!(e.verbs.as_ref().unwrap().len(), 2);
        assert_eq!(e.env["WINEMSYNC"], "1");
        assert_eq!(e.dll_overrides["ddraw"], "n,b");
        let reg = &e.registry.as_ref().unwrap()[0];
        assert_eq!(reg.key, r"HKCU\Software\Wine\Direct3D");
        assert_eq!(reg.kind, None);
        let tested = &e.tested.as_ref().unwrap()[0];
        assert_eq!(tested.date.unwrap().to_string(), "2026-10-22");
        assert_eq!(e.replace_command.unwrap().to.as_str(), "RebelGalaxyGOG.exe");
    }

    #[test]
    fn unknown_keys_warn_not_fail() {
        let p = parse(
            "renderer = \"wined3d\"\nfuture = 1\ntested = [{ runtime = \"x\", gpu = \"m4\" }]\n",
        )
        .unwrap();
        assert_eq!(p.entry.renderer.as_deref(), Some("wined3d"));
        assert_eq!(
            p.warnings,
            vec!["future".to_string(), "tested[0].gpu".into()]
        );
    }

    #[test]
    fn variant_is_a_known_key_and_merges_l4_over_l3() {
        let l3 = parse("variant = \"Settings\"\n").unwrap();
        assert!(l3.warnings.is_empty(), "{:?}", l3.warnings);
        let l4 = parse("variant = \"Carmageddon TDR 2000 (safe mode)\"\n").unwrap();
        let eff = merge(&[(Layer::Db, &l3.entry), (Layer::User, &l4.entry)]);
        let v = eff.variant.as_ref().unwrap();
        assert_eq!(v.value, "Carmageddon TDR 2000 (safe mode)");
        assert_eq!(v.layer, Layer::User);
        assert!(
            eff.display_lines().contains(
                &"variant = \"Carmageddon TDR 2000 (safe mode)\"  (L4 user override)".into()
            ),
            "{:?}",
            eff.display_lines()
        );
        assert!(parse("variant = [\"a\"]").is_err());
    }

    #[test]
    fn bad_values_are_errors() {
        assert!(parse("status = \"great\"").is_err());
        assert!(parse("verbs = \"not-a-list\"").is_err());
        assert!(parse("not toml at all =").is_err());
    }

    #[test]
    fn missing_user_file_is_an_empty_layer() {
        let tmp = tempfile::tempdir().unwrap();
        let p = load_user_at(&tmp.path().join("1.toml")).unwrap();
        assert_eq!(p.entry, CompatEntry::default());
        assert!(p.warnings.is_empty());
    }

    #[test]
    fn broken_user_file_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("1.toml");
        std::fs::write(&path, "renderer = ").unwrap();
        assert!(matches!(
            load_user_at(&path),
            Err(MacAppError::Compat { .. })
        ));
    }

    #[test]
    fn gogid_must_be_digits() {
        assert!(validate_gogid("1207659013").is_ok());
        for bad in ["", "../x", "12a", "1/2"] {
            assert!(validate_gogid(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn db_lookup_finds_and_degrades() {
        let db = [("1", "renderer = \"dxmt\""), ("2", "status = \"great\"")];
        assert_eq!(db_lookup(&db, "1").entry.renderer.as_deref(), Some("dxmt"));
        let broken = db_lookup(&db, "2");
        assert_eq!(broken.entry, CompatEntry::default());
        assert_eq!(broken.warnings.len(), 1);
        assert_eq!(db_lookup(&db, "3").entry, CompatEntry::default());
    }

    #[test]
    fn compiled_db_entries_all_parse() {
        for (id, raw) in COMPAT_DB {
            validate_gogid(id).unwrap();
            let p = parse(raw).unwrap_or_else(|e| panic!("compat/{id}.toml: {e}"));
            assert!(p.warnings.is_empty(), "compat/{id}.toml: {:?}", p.warnings);
        }
    }

    /// The repo's `compat/` itself (#75): only `<gogid>.toml` entries plus
    /// docs, every entry says where it came from, and a status other than
    /// `unknown` is backed by at least one `tested` run.
    #[test]
    fn compat_dir_entries_are_sourced_and_tested() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../compat");
        let mut entries = 0;
        for f in std::fs::read_dir(&dir).unwrap() {
            let path = f.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_string();
            if name == "README.md" || name.starts_with("NOTICE-") {
                continue;
            }
            let id = name
                .strip_suffix(".toml")
                .unwrap_or_else(|| panic!("compat/{name}: not a <gogid>.toml"));
            validate_gogid(id).unwrap_or_else(|_| panic!("compat/{name}: not a GOG id"));
            let raw = std::fs::read_to_string(&path).unwrap();
            let p = parse(&raw).unwrap_or_else(|e| panic!("compat/{name}: {e}"));
            assert!(p.warnings.is_empty(), "compat/{name}: {:?}", p.warnings);
            let e = p.entry;
            assert!(
                e.source.as_deref().is_some_and(|s| !s.is_empty()),
                "compat/{name}: no source"
            );
            if e.status.unwrap_or_default() != Status::Unknown {
                assert!(
                    e.tested.as_ref().is_some_and(|t| !t.is_empty()),
                    "compat/{name}: status without a tested run"
                );
            }
            entries += 1;
        }
        assert_eq!(entries, db_len(), "every entry is compiled in");
    }

    const ALL: [Layer; 6] = [
        Layer::Default,
        Layer::Auto,
        Layer::Gog,
        Layer::Db,
        Layer::User,
        Layer::Flags,
    ];

    fn with_renderer(v: &str) -> CompatEntry {
        CompatEntry {
            renderer: Some(v.into()),
            ..Default::default()
        }
    }

    /// For every pair of layers, the higher one wins whatever the input order.
    #[test]
    fn precedence_matrix() {
        for (i, lo) in ALL.iter().enumerate() {
            for hi in &ALL[i + 1..] {
                let a = with_renderer("low");
                let b = with_renderer("high");
                for layers in [[(*lo, &a), (*hi, &b)], [(*hi, &b), (*lo, &a)]] {
                    let eff = merge(&layers);
                    let r = eff.renderer.unwrap();
                    assert_eq!(
                        (r.value.as_str(), r.layer),
                        ("high", *hi),
                        "{lo:?} vs {hi:?}"
                    );
                }
            }
        }
        // All six at once: flags win; drop the top each time and the next wins.
        let entries: Vec<CompatEntry> = ALL
            .iter()
            .map(|l| with_renderer(&format!("{l:?}")))
            .collect();
        for top in (0..ALL.len()).rev() {
            let layers: Vec<(Layer, &CompatEntry)> =
                ALL[..=top].iter().copied().zip(entries.iter()).collect();
            let r = merge(&layers).renderer.unwrap();
            assert_eq!(r.layer, ALL[top]);
            assert_eq!(r.value, format!("{:?}", ALL[top]));
        }
    }

    #[test]
    fn merge_is_field_by_field() {
        let db = parse(REBEL_GALAXY).unwrap().entry;
        let user = parse(
            "renderer = \"wined3d\"\nverbs = [\"-d3dx11_43\", \"xinput\"]\nenv = { DXVK_HUD = \"1\", WINEMSYNC = \"0\" }\n",
        )
        .unwrap()
        .entry;
        let l0 = CompatEntry {
            runtime: Some("wine-staging/11.18".into()),
            ..Default::default()
        };
        let eff = merge(&[
            (Layer::User, &user),
            (Layer::Db, &db),
            (Layer::Default, &l0),
        ]);
        let r = eff.renderer.as_ref().unwrap();
        assert_eq!((r.value.as_str(), r.layer), ("wined3d", Layer::User));
        // Verbs edit the lower layer's list (#128).
        let v: Vec<(&str, Layer)> = eff
            .verbs
            .iter()
            .map(|s| (s.value.as_str(), s.layer))
            .collect();
        assert_eq!(v, [("d3dcompiler_43", Layer::Db), ("xinput", Layer::User)]);
        assert_eq!(eff.exe.as_ref().unwrap().layer, Layer::Db);
        assert_eq!(eff.runtime.as_ref().unwrap().layer, Layer::Default);
        // Maps merge per key.
        assert_eq!(eff.env["WINEMSYNC"].value, "0");
        assert_eq!(eff.env["WINEMSYNC"].layer, Layer::User);
        assert_eq!(eff.env["DXVK_HUD"].layer, Layer::User);
        assert_eq!(eff.dll_overrides["ddraw"].layer, Layer::Db);
        let lines = eff.display_lines();
        assert!(lines.contains(&"renderer = \"wined3d\"  (L4 user override)".to_string()));
        assert!(lines.contains(&"verbs += \"xinput\"  (L4 user override)".to_string()));
        let json = serde_json::to_value(&eff).unwrap();
        assert_eq!(json["renderer"]["layer"], "L4");
        assert_eq!(json["verbs"][1]["value"], "xinput");
        assert_eq!(json["verbs"][1]["layer"], "L4");
        assert_eq!(json["tested"]["value"][0]["date"], "2026-10-22");
    }

    /// Golden status lines (docs/19 §6.6), each from L3 alone or L4 over it.
    #[test]
    fn status_line_goldens() {
        let line = |db: &str, user: &str| {
            let db = parse(db).unwrap().entry;
            let user = parse(user).unwrap().entry;
            merge(&[(Layer::Db, &db), (Layer::User, &user)]).status_line()
        };
        assert_eq!(
            line(REBEL_GALAXY, ""),
            "known to boot on winecx-24.0.7_7 (2026-10-22)"
        );
        assert_eq!(line("", ""), "untested");
        assert_eq!(line("status = \"unknown\"", ""), "untested");
        assert_eq!(line("exe = \"a.exe\"", ""), "untested");
        assert_eq!(
            line(
                "status = \"broken\"\nnotes = \"black screen after the intro video\"",
                ""
            ),
            "known broken: black screen after the intro video"
        );
        assert_eq!(line("status = \"broken\"", ""), "known broken");
        assert_eq!(line("status = \"plays\"", ""), "known to play");
        // The newest dated run wins over list order; L4 overrides L3.
        assert_eq!(
            line(
                REBEL_GALAXY,
                "status = \"plays\"\ntested = [{ runtime = \"winecx-25\", date = 2026-11-01 }, \
                 { runtime = \"winecx-24\", date = 2026-10-01 }]"
            ),
            "known to play on winecx-25 (2026-11-01)"
        );
        assert_eq!(
            line(
                "status = \"boots\"\ntested = [{ runtime = \"a\" }, { runtime = \"b\" }]",
                ""
            ),
            "known to boot on b"
        );
    }

    #[test]
    fn lane_scopes_the_status_to_one_build_lane() {
        let eff = |db: &str, user: &str| {
            let db = parse(db).unwrap().entry;
            let user = parse(user).unwrap().entry;
            merge(&[(Layer::Db, &db), (Layer::User, &user)])
        };
        let pharaoh = "status = \"broken\"\nnotes = \"asks for the CD\"\nlane = \"depot\"";
        let e = eff(pharaoh, "");
        assert_eq!(e.status_for(Some(Lane::Depot)), Status::Broken);
        assert_eq!(e.status_for(Some(Lane::Installer)), Status::Unknown);
        assert_eq!(e.status_for(None), Status::Broken);
        assert_eq!(
            e.status_line_for(Some(Lane::Depot)),
            "known broken: asks for the CD"
        );
        assert_eq!(
            e.status_line_for(Some(Lane::Installer)),
            "untested from the offline installer (the depot build: known broken: asks for the CD)"
        );
        assert_eq!(
            e.status_line(),
            "known broken: asks for the CD (from the depot build)"
        );
        // An L4 status without a lane takes L3's lane with it.
        let e = eff(pharaoh, "status = \"plays\"");
        assert!(e.lane.is_none());
        assert_eq!(e.status_for(Some(Lane::Installer)), Status::Plays);
        // No status: the lane changes nothing.
        assert_eq!(
            eff("lane = \"installer\"", "").status_line_for(Some(Lane::Depot)),
            "untested"
        );
        assert!(parse("lane = \"floppy\"").is_err());
    }

    #[test]
    fn set_user_keys_keeps_comments_and_reports_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("compat/1.toml");
        // Fresh file.
        let c = set_user_keys_at(&path, &[("runtime", Some("winecx/24.0.7_7"))]).unwrap();
        assert_eq!(
            c,
            vec![Change {
                key: "runtime".into(),
                old: None,
                new: Some("winecx/24.0.7_7".into())
            }]
        );
        // The user adds their own keys and a comment.
        let mut raw = std::fs::read_to_string(&path).unwrap();
        raw.push_str("# my note\nenv = { WINEMSYNC = \"1\" }\n");
        std::fs::write(&path, &raw).unwrap();
        // Same value again: no change, no write.
        assert!(
            set_user_keys_at(&path, &[("runtime", Some("winecx/24.0.7_7"))])
                .unwrap()
                .is_empty()
        );
        let c = set_user_keys_at(
            &path,
            &[("renderer", Some("dxmt/v0.80")), ("runtime", None)],
        )
        .unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[1].old.as_deref(), Some("winecx/24.0.7_7"));
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("# my note"), "{after}");
        let p = load_user_at(&path).unwrap().entry;
        assert_eq!(p.renderer.as_deref(), Some("dxmt/v0.80"));
        assert_eq!(p.runtime, None);
        assert_eq!(p.env["WINEMSYNC"], "1");
    }

    #[test]
    fn set_user_keys_refuses_user_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("1.toml");
        assert!(set_user_keys_at(&path, &[("env", Some("x"))]).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn user_path_follows_gogo_home() {
        let _g = crate::test_util::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: env mutation under the shared env mutex.
        unsafe { std::env::set_var("GOGO_HOME", tmp.path()) };
        assert_eq!(user_path("42"), tmp.path().join("compat/42.toml"));
        set_user_keys("42", &[("renderer", Some("wined3d"))]).unwrap();
        let p = load_user("42").unwrap();
        assert_eq!(p.entry.renderer.as_deref(), Some("wined3d"));
        assert!(load_user("../42").is_err());
        unsafe { std::env::remove_var("GOGO_HOME") };
    }
}
