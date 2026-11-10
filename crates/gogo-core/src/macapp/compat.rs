//! Per-game compat data (docs/19 §6.4–§6.5, design 16 decision 5): one
//! declarative entry per GOG product id. The same schema serves the curated
//! compat DB (L3, `compat/` at the repo root, compiled in) and the user's
//! override file (L4, `<state_dir>/compat/<gogid>.toml`). The other layers
//! (L0 default runtime, L1 auto renderer, L2 GOG-native) are computed by the
//! pipeline and fed in as [`CompatEntry`] values of their own.
//!
//! A file's top level holds the keys every OS shares; `[platforms.<os>]`
//! the OS-specific ones (design 18 decision 5, #166). [`parse`] resolves a
//! file for one [`Os`] into that layer's [`CompatEntry`].
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
/// `[lanes.<lane>]` gives one lane a status of its own ([`LaneStatus`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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

    /// The schema spelling: `depot`, `installer`.
    pub fn key(self) -> &'static str {
        match self {
            Lane::Depot => "depot",
            Lane::Installer => "installer",
        }
    }
}

/// `[lanes.<lane>]`: the status of a bundle built from that lane, for a game
/// that differs between them. It wins over the top-level `status`, `notes`
/// and `tested` for that lane only.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LaneStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tested: Option<Vec<Tested>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
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

/// One file to put in the game dir (`files`, design 18 §4.10): what GOG's
/// installer would have left there and a depot build lacks (SimCity 4's
/// `Apps/SimCity 4.ini`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameFile {
    /// Relative to the game dir, `/`-separated ([`check_files`]).
    pub path: String,
    /// The file's contents, written as given (UTF-8, line endings and all).
    pub text: String,
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
    /// Files setup writes into the game dir. Replaced whole by a higher
    /// layer, like `registry`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<GameFile>>,
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
    /// `[lanes.<lane>]`: a status of that lane's own, over the top-level one.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lanes: BTreeMap<Lane, LaneStatus>,
    /// `[platforms.linux]` only: `false` turns umu-protonfixes off for the
    /// game. Parsed and checked; applying it (`PROTONFIXES_DISABLE=1`) is
    /// #171 / #177.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protonfixes: Option<bool>,
    /// `[platforms.linux]` only: launch args appended after the game's own
    /// (`args`, the variant's or GOG's), as Heroic's "launcher args" are
    /// (#234). `args` replaces the game's; this keeps them. Replaced whole
    /// by a higher layer; `gogo linux-app args` writes it in L4.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_args: Option<Vec<String>>,
    /// `[platforms.linux]` only: the command the launcher runs the game
    /// under, in argv form (`["gamescope", "-f", "--"]`; #271, design 18
    /// §7.8). It goes outside umu-run. Replaced whole by a higher layer;
    /// `gogo linux-app wrapper` writes it in L4.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapper: Option<Vec<String>>,
    /// `[platforms.linux]` only: which of Wine's display drivers the game
    /// gets (#292, design 18 §7.9): `wayland` is Wine's Wayland driver
    /// (`PROTON_ENABLE_WAYLAND=1`, when the session has a Wayland socket),
    /// `x11` keeps it on X11 (Xwayland on a Wayland desktop) whatever the
    /// environment says. Absent: gogo leaves the choice to the environment.
    /// `gogo linux-app display` writes it in L4.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<DisplayMode>,
    /// `[platforms.linux]` only, with `display = "wayland"`: what gogo asks
    /// of Proton's HDR switches (#292). Absent is [`Hdr::Auto`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdr: Option<Hdr>,
    /// `[platforms.linux]` only, with `display = "wayland"`: the output
    /// (`"DP-1"`) GE-Proton's `PROTON_WAYLAND_MONITOR` names as Wine's
    /// primary monitor (#295). Absent: GE-Proton guesses with `xrandr`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<String>,
    /// Provenance: `gogo-vm`, `umu-protonfixes:<file>@<sha>`, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// `[platforms.linux] display` (#292, design 18 §7.9).
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DisplayMode {
    /// Wine's X11 driver: Xwayland on a Wayland desktop.
    X11,
    /// Wine's Wayland driver, when the session has a Wayland socket.
    Wayland,
}

/// `[platforms.linux] hdr` (#292): mapped onto GE-Proton's own switches,
/// which then set `DXVK_HDR` and the Wayland driver's (design 18 §7.9).
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Hdr {
    /// Nothing set: Proton and DXVK detect HDR from the Wayland output.
    Auto,
    /// `PROTON_ENABLE_HDR=1`: DXVK reports HDR (`DXVK_HDR=1`).
    On,
    /// `PROTON_FORCE_HDR=1`: HDR on outputs the compositor reports as SDR
    /// too (Hyprland's `cm_auto_hdr`, KWin at SDR brightness).
    Force,
    /// `PROTON_ENABLE_HDR=0`: no HDR for the game.
    Off,
}

impl DisplayMode {
    pub const ALL: [DisplayMode; 2] = [DisplayMode::Wayland, DisplayMode::X11];

    /// The schema spelling, also `launch.conf`'s `DISPLAY_MODE`.
    pub fn key(self) -> &'static str {
        match self {
            DisplayMode::X11 => "x11",
            DisplayMode::Wayland => "wayland",
        }
    }

    pub fn from_key(key: &str) -> Option<DisplayMode> {
        Self::ALL.into_iter().find(|m| m.key() == key)
    }
}

impl Hdr {
    pub const ALL: [Hdr; 4] = [Hdr::Auto, Hdr::On, Hdr::Force, Hdr::Off];

    /// The schema spelling, also `launch.conf`'s `DISPLAY_HDR`.
    pub fn key(self) -> &'static str {
        match self {
            Hdr::Auto => "auto",
            Hdr::On => "on",
            Hdr::Force => "force",
            Hdr::Off => "off",
        }
    }

    pub fn from_key(key: &str) -> Option<Hdr> {
        Self::ALL.into_iter().find(|h| h.key() == key)
    }

    /// The Proton switch this sets on Wayland, if any.
    pub fn proton_env(self) -> Option<(&'static str, &'static str)> {
        match self {
            Hdr::Auto => None,
            Hdr::On => Some(("PROTON_ENABLE_HDR", "1")),
            Hdr::Force => Some(("PROTON_FORCE_HDR", "1")),
            Hdr::Off => Some(("PROTON_ENABLE_HDR", "0")),
        }
    }
}

// As the schema spells them (`"wayland"`), in summaries and test failures.
impl std::fmt::Debug for DisplayMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.key())
    }
}

impl std::fmt::Debug for Hdr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.key())
    }
}

/// In a `verbs` list: drop every verb the layers below (and the entries
/// before it) added. `["-*", "d3dx9"]` replaces the list whole.
pub const VERBS_CLEAR: &str = "-*";

/// An OS a `[platforms.<os>]` table is for, named as Rust's `target_os`
/// (design 18 decision 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Macos,
    Linux,
}

impl Os {
    pub const ALL: [Os; 2] = [Os::Macos, Os::Linux];

    /// The OS gogo runs on (macOS for anything that isn't Linux: gogo
    /// targets only those two).
    pub fn current() -> Os {
        if cfg!(target_os = "linux") {
            Os::Linux
        } else {
            Os::Macos
        }
    }

    /// The schema spelling: `macos`, `linux`.
    pub fn key(self) -> &'static str {
        match self {
            Os::Macos => "macos",
            Os::Linux => "linux",
        }
    }

    fn from_key(key: &str) -> Option<Os> {
        Os::ALL.into_iter().find(|os| os.key() == key)
    }
}

/// Keys valid at the top level: the Windows-side facts, the same on every OS.
const TOP_KEYS: &[&str] = &[
    "title",
    "exe",
    "args",
    "variant",
    "replace_command",
    "verbs",
    "dll_overrides",
    "registry",
    "files",
    "source",
];

/// Keys valid only under `[platforms.<os>]` (which takes the top-level
/// ones too, to override or extend them there).
const PLATFORM_KEYS: &[&str] = &[
    "renderer",
    "runtime",
    "env",
    "status",
    "notes",
    "tested",
    "lane",
    "lanes",
    "protonfixes",
    "extra_args",
    "wrapper",
    "display",
    "hdr",
    "monitor",
];

/// [`PLATFORM_KEYS`] only the Linux lane applies: under `[platforms.macos]`
/// they warn and are ignored.
const LINUX_ONLY_KEYS: &[&str] = &[
    "protonfixes",
    "extra_args",
    "wrapper",
    "display",
    "hdr",
    "monitor",
];

/// The OS-specific keys the schema had at the top level before
/// `[platforms.<os>]` (#166). In an L4 file they still read as
/// `[platforms.macos]`: gogo and its users wrote them on a Mac.
const LEGACY_KEYS: &[&str] = &[
    "renderer", "runtime", "env", "status", "notes", "tested", "lane", "lanes",
];

/// Renderers that exist only on macOS: an error under `[platforms.linux]`.
const MAC_ONLY_RENDERERS: &[&str] = &["dxmt", "d3dmetal"];

/// The error for a Mac-only renderer ref (`dxmt[/<ver>]`, `d3dmetal[/<ver>]`)
/// on Linux, `None` for any other ref. The `[platforms.linux]` check and the
/// Linux renderer refs (#172) both use it.
pub fn mac_only_renderer(r: &str) -> Option<String> {
    let id = r.split('/').next().unwrap_or_default().trim();
    MAC_ONLY_RENDERERS
        .contains(&id)
        .then(|| format!("{id} runs on macOS only"))
}

/// Keys gogo itself writes into L4 (`--retarget`, persisted build flags,
/// "Try next renderer"; design 16 decisions 5 and 7), always under
/// `[platforms.<this os>]`. Everything else in the file is the user's.
pub const OWNED_KEYS: &[&str] = &["runtime", "renderer"];

/// A parsed layer, resolved for one OS, plus non-fatal warnings (unknown
/// keys, keys in the wrong place, legacy keys), each a full message.
#[derive(Debug, Clone, Default)]
pub struct Parsed {
    pub entry: CompatEntry,
    pub warnings: Vec<String>,
}

/// Parse one entry and resolve it for `os`: the top level, then
/// `[platforms.<os>]` over it ([`overlay`]). A table for another OS is
/// checked the same way but not applied. `layer` is [`Layer::Db`] or
/// [`Layer::User`]: only L4 reads [`LEGACY_KEYS`] at the top level (as
/// `[platforms.macos]`, so on macOS only); in L3 they warn and are ignored,
/// like `protonfixes` there anywhere and every unknown key. Warnings, never
/// errors: an older gogo must still read a newer DB. Bad values are errors.
pub fn parse(raw: &str, os: Os, layer: Layer) -> Result<Parsed, String> {
    let mut table: toml::Table = toml::from_str(raw).map_err(|e| e.to_string())?;
    let mut warnings = Vec::new();
    let platforms = match table.remove("platforms") {
        None => toml::Table::new(),
        Some(toml::Value::Table(t)) => t,
        Some(_) => return Err("`platforms` must be a table of `[platforms.<os>]`".into()),
    };
    let mut legacy = toml::Table::new();
    for key in table.keys().cloned().collect::<Vec<_>>() {
        if TOP_KEYS.contains(&key.as_str()) {
            continue;
        }
        let value = table.remove(&key).expect("listed above");
        if layer == Layer::User && LEGACY_KEYS.contains(&key.as_str()) {
            legacy.insert(key, value);
        } else if PLATFORM_KEYS.contains(&key.as_str()) {
            warnings.push(format!(
                "`{key}` is only valid under [platforms.<os>]: ignored"
            ));
        } else {
            warnings.push(format!("unknown key {key}"));
        }
    }
    check_items(&table, "", &mut warnings);
    let mut entry = entry_from(table)?;
    check_files(entry.files.as_deref()).map_err(|e| format!("files: {e}"))?;

    let mut own = None;
    if !legacy.is_empty() {
        let keys = legacy.keys().map(|k| format!("`{k}`")).collect::<Vec<_>>();
        let keys = keys.join(", ");
        let mac = platform_entry(legacy, Os::Macos, "", &mut warnings)
            .map_err(|e| format!("legacy top-level keys: {e}"))?;
        match os {
            Os::Macos => {
                warnings.push(format!(
                    "note: top-level {keys} read as [platforms.macos], the shape before \
                     #166 (gogo moves `renderer` / `runtime` there on its next edit; move the \
                     rest to drop this note)"
                ));
                overlay(&mut entry, mac);
            }
            Os::Linux => warnings.push(format!(
                "top-level {keys}: Mac-only values from before [platforms.<os>], ignored on \
                 linux (move them under [platforms.macos])"
            )),
        }
    }
    for (name, value) in platforms {
        let Some(p) = Os::from_key(&name) else {
            warnings.push(format!("unknown key platforms.{name}"));
            continue;
        };
        let toml::Value::Table(t) = value else {
            return Err(format!("`platforms.{name}` must be a table"));
        };
        let prefix = format!("platforms.{name}.");
        let e = platform_entry(t, p, &prefix, &mut warnings)?;
        if p == os {
            own = Some(e);
        }
    }
    if let Some(e) = own {
        overlay(&mut entry, e);
    }
    Ok(Parsed { entry, warnings })
}

/// The top-level keys that are fixes rather than facts about the game or
/// the file (`title`, `variant`, `source`): what design 18 §4.3's ownership
/// rule (O3) skips in an L3 file that protonfixes covers. `env` would be
/// one, but at the top level it is ignored anyway (it is OS-specific).
pub const TOP_FIX_KEYS: &[&str] = &[
    "verbs",
    "dll_overrides",
    "registry",
    "files",
    "args",
    "exe",
    "replace_command",
];

/// [`parse`] without the top level's [`TOP_FIX_KEYS`], plus those fixes on
/// their own (the top level alone, no platform table). The first is what
/// applies when another owner has the file's fixes, the second what the
/// summary shows as covered. `[platforms.<os>]` still applies whole.
pub fn parse_without_top_fixes(
    raw: &str,
    os: Os,
    layer: Layer,
) -> Result<(Parsed, CompatEntry), String> {
    let mut kept: toml::Table = toml::from_str(raw).map_err(|e| e.to_string())?;
    let mut fixes = toml::Table::new();
    for key in TOP_FIX_KEYS {
        if let Some(v) = kept.remove(*key) {
            fixes.insert((*key).to_string(), v);
        }
    }
    let text = |t: &toml::Table| toml::to_string(t).map_err(|e| e.to_string());
    let parsed = parse(&text(&kept)?, os, layer)?;
    let covered = parse(&text(&fixes)?, os, layer)?.entry;
    Ok((parsed, covered))
}

/// One `[platforms.<os>]` table: checked (keys, renderer) and deserialized.
fn platform_entry(
    mut t: toml::Table,
    os: Os,
    prefix: &str,
    warnings: &mut Vec<String>,
) -> Result<CompatEntry, String> {
    for key in t.keys().cloned().collect::<Vec<_>>() {
        let known = TOP_KEYS.contains(&key.as_str()) || PLATFORM_KEYS.contains(&key.as_str());
        if !known {
            warnings.push(format!("unknown key {prefix}{key}"));
            t.remove(&key);
        } else if LINUX_ONLY_KEYS.contains(&key.as_str()) && os != Os::Linux {
            warnings.push(format!("`{prefix}{key}` is Linux only: ignored"));
            t.remove(&key);
        }
    }
    check_items(&t, prefix, warnings);
    let e = entry_from(t).map_err(|e| format!("[platforms.{}]: {e}", os.key()))?;
    if let Some(m) = &e.monitor {
        check_monitor(m).map_err(|e| format!("[platforms.{}]: {e}", os.key()))?;
    }
    check_files(e.files.as_deref()).map_err(|e| format!("[platforms.{}] files: {e}", os.key()))?;
    if os == Os::Linux
        && let Some(r) = &e.renderer
        && let Some(why) = mac_only_renderer(r)
    {
        return Err(format!("renderer `{r}` in [platforms.linux]: {why}"));
    }
    Ok(e)
}

/// Unknown keys inside `tested` / `registry` / `files` items and `[lanes.<lane>]`
/// tables, as warnings (`tested[0].gpu`, `platforms.macos.lanes.depot.foo`).
fn check_items(t: &toml::Table, prefix: &str, warnings: &mut Vec<String>) {
    for (key, value) in t {
        if key == "lanes" {
            for (lane, t) in value.as_table().into_iter().flatten() {
                for k in t.as_table().into_iter().flat_map(|t| t.keys()) {
                    if !["status", "tested", "notes"].contains(&k.as_str()) {
                        warnings.push(format!("unknown key {prefix}lanes.{lane}.{k}"));
                    }
                }
            }
            continue;
        }
        let item_keys: &[&str] = match key.as_str() {
            "tested" => &["runtime", "host", "date"],
            "registry" => &["key", "name", "type", "data"],
            "files" => &["path", "text"],
            _ => continue,
        };
        for (i, item) in value.as_array().into_iter().flatten().enumerate() {
            for k in item.as_table().into_iter().flat_map(|t| t.keys()) {
                if !item_keys.contains(&k.as_str()) {
                    warnings.push(format!("unknown key {prefix}{key}[{i}].{k}"));
                }
            }
        }
    }
}

/// `files` paths: relative to the game dir, `/`-separated, and inside it.
/// No absolute path, drive letter or backslash, no `.`, `..` or empty
/// part, and no path twice.
pub fn check_files(files: Option<&[GameFile]>) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for f in files.unwrap_or_default() {
        let p = f.path.as_str();
        let bad = |why: &str| Err(format!("path {p:?} {why}"));
        if p.is_empty() {
            return bad("is empty");
        }
        if p.contains('\\') {
            return bad("has a backslash: use `/`");
        }
        if p.starts_with('/') || p.as_bytes().get(1) == Some(&b':') {
            return bad("is absolute: give it relative to the game dir");
        }
        if p.contains('\0') {
            return bad("has a NUL");
        }
        if p.split('/').any(|c| c.is_empty() || c == "." || c == "..") {
            return bad("has an empty, `.` or `..` part: it must stay inside the game dir");
        }
        if !seen.insert(p.to_lowercase()) {
            return bad("is given twice");
        }
    }
    Ok(())
}

/// A Wayland output's name as a compositor spells it (`DP-1`, `HDMI-A-1`,
/// `eDP-1`, `Virtual-1`): printable ASCII, no spaces, at most 64 bytes.
pub fn check_monitor(m: &str) -> Result<(), String> {
    if m.is_empty() || m.len() > 64 || !m.chars().all(|c| c.is_ascii_graphic()) {
        return Err(format!(
            "monitor {m:?} is not an output name (`DP-1`, `HDMI-A-1`: printable ASCII, no spaces)"
        ));
    }
    Ok(())
}

/// Deserialize a checked table. Through its text, not the `toml::Value`:
/// the value deserializer hands dates over as strings.
fn entry_from(t: toml::Table) -> Result<CompatEntry, String> {
    let text = toml::to_string(&t).map_err(|e| e.to_string())?;
    toml::from_str(&text).map_err(|e| e.to_string())
}

/// `over` (a `[platforms.<os>]` table) on top of `base` (the top level),
/// within one layer, by the rules [`merge`] uses across layers: maps per
/// key, `verbs` edit the list (`over`'s edits after `base`'s), a `status`
/// takes the lane and lanes below it away, each lane's table and every
/// other field is replaced whole.
fn overlay(base: &mut CompatEntry, over: CompatEntry) {
    fn take<T>(slot: &mut Option<T>, v: Option<T>) {
        if v.is_some() {
            *slot = v;
        }
    }
    let CompatEntry {
        title,
        exe,
        renderer,
        runtime,
        verbs,
        dll_overrides,
        env,
        registry,
        files,
        args,
        variant,
        replace_command,
        status,
        tested,
        notes,
        lane,
        lanes,
        protonfixes,
        extra_args,
        wrapper,
        display,
        hdr,
        monitor,
        source,
    } = over;
    take(&mut base.title, title);
    take(&mut base.exe, exe);
    take(&mut base.renderer, renderer);
    take(&mut base.runtime, runtime);
    if let Some(v) = verbs {
        base.verbs.get_or_insert_default().extend(v);
    }
    base.dll_overrides.extend(dll_overrides);
    base.env.extend(env);
    take(&mut base.registry, registry);
    take(&mut base.files, files);
    take(&mut base.args, args);
    take(&mut base.variant, variant);
    take(&mut base.replace_command, replace_command);
    if status.is_some() {
        base.lane = None;
        base.lanes.clear();
    }
    take(&mut base.status, status);
    take(&mut base.tested, tested);
    take(&mut base.notes, notes);
    take(&mut base.lane, lane);
    base.lanes.extend(lanes);
    take(&mut base.protonfixes, protonfixes);
    take(&mut base.extra_args, extra_args);
    take(&mut base.wrapper, wrapper);
    take(&mut base.display, display);
    take(&mut base.hdr, hdr);
    take(&mut base.monitor, monitor);
    take(&mut base.source, source);
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
    db_entry_for(gogid, Os::current())
}

/// [`db_entry`] resolved for `os` (the Linux renderer walk's tests run on
/// any OS, #172).
pub fn db_entry_for(gogid: &str, os: Os) -> Parsed {
    if let Some(dir) = std::env::var_os(DB_DIR_ENV).filter(|d| !d.is_empty())
        && validate_gogid(gogid).is_ok()
    {
        let path = Path::new(&dir).join(format!("{gogid}.toml"));
        if let Ok(raw) = std::fs::read_to_string(&path) {
            return parse(&raw, os, Layer::Db).unwrap_or_else(|e| Parsed {
                entry: CompatEntry::default(),
                warnings: vec![format!(
                    "compat DB entry {} unreadable: {e}",
                    path.display()
                )],
            });
        }
    }
    db_lookup(COMPAT_DB, gogid, os)
}

fn db_lookup(db: &[(&str, &str)], gogid: &str, os: Os) -> Parsed {
    match db.iter().find(|(id, _)| *id == gogid) {
        Some((_, raw)) => parse(raw, os, Layer::Db).unwrap_or_else(|e| Parsed {
            entry: CompatEntry::default(),
            warnings: vec![format!("compat DB entry {gogid} unreadable: {e}")],
        }),
        None => Parsed::default(),
    }
}

/// The L3 file's text for `gogid`, from where [`db_entry`] reads it, and
/// a label for messages (its path, or `compat DB <gogid>`). For callers
/// that resolve the file their own way (the Linux lane's ownership rule,
/// design 18 §4.3).
pub fn db_raw(gogid: &str) -> Option<(String, String)> {
    if let Some(dir) = std::env::var_os(DB_DIR_ENV).filter(|d| !d.is_empty())
        && validate_gogid(gogid).is_ok()
    {
        let path = Path::new(&dir).join(format!("{gogid}.toml"));
        if let Ok(raw) = std::fs::read_to_string(&path) {
            return Some((path.display().to_string(), raw));
        }
    }
    COMPAT_DB
        .iter()
        .find(|(id, _)| *id == gogid)
        .map(|(_, raw)| (format!("compat DB {gogid}"), raw.to_string()))
}

/// Number of entries in the compiled-in DB.
pub fn db_len() -> usize {
    COMPAT_DB.len()
}

// — L4: the user's override file —

/// `<data_root>/compat/` (design 16 §0; `GOGO_HOME` flattens).
pub fn user_dir() -> PathBuf {
    crate::state::data_root().join("compat")
}

/// `<data_root>/compat/<gogid>.toml`.
pub fn user_path(gogid: &str) -> PathBuf {
    user_dir().join(format!("{gogid}.toml"))
}

/// Load the L4 file for `gogid`, resolved for this OS. A missing file is an
/// empty layer; a file that doesn't parse is an error (it is the user's own
/// edit — say so rather than silently ignoring it).
pub fn load_user(gogid: &str) -> Result<Parsed, MacAppError> {
    load_user_for(gogid, Os::current())
}

/// [`load_user`] resolved for `os`.
pub fn load_user_for(gogid: &str, os: Os) -> Result<Parsed, MacAppError> {
    validate_gogid(gogid)?;
    load_user_at(&user_path(gogid), os)
}

fn load_user_at(path: &Path, os: Os) -> Result<Parsed, MacAppError> {
    match std::fs::read_to_string(path) {
        Ok(raw) => parse(&raw, os, Layer::User).map_err(|reason| MacAppError::Compat {
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
/// `gogid`, under `[platforms.<this os>]`, keeping everything else
/// (comments included) as the user wrote it. Returns only the keys whose
/// value for this OS actually changed; no change, no write.
pub fn set_user_keys(
    gogid: &str,
    updates: &[(&str, Option<&str>)],
) -> Result<Vec<Change>, MacAppError> {
    set_user_keys_for(gogid, updates, Os::current())
}

/// [`set_user_keys`] under `[platforms.<os>]`.
pub fn set_user_keys_for(
    gogid: &str,
    updates: &[(&str, Option<&str>)],
    os: Os,
) -> Result<Vec<Change>, MacAppError> {
    validate_gogid(gogid)?;
    set_user_keys_at(&user_path(gogid), updates, os)
}

/// [`set_user_keys`] for `os`. A write also moves the legacy top-level keys
/// gogo owns (written before #166, on a Mac) under `[platforms.macos]`,
/// unless that table sets them already (then the top-level one never
/// applied and goes). The user's own legacy keys stay where they are.
fn set_user_keys_at(
    path: &Path,
    updates: &[(&str, Option<&str>)],
    os: Os,
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
    let str_at = |doc: &toml_edit::DocumentMut, os: Option<Os>, key: &str| {
        let item = match os {
            None => doc.get(key),
            Some(os) => doc
                .get("platforms")
                .and_then(|p| p.get(os.key()))
                .and_then(|t| t.get(key)),
        };
        item.and_then(|i| i.as_str()).map(str::to_string)
    };
    let mut changes = Vec::new();
    for (key, new) in updates {
        if !OWNED_KEYS.contains(key) {
            return Err(err(format!("`{key}` is not a key gogo writes")));
        }
        // The value this OS reads now: its table's, else (macOS) a legacy one.
        let old = str_at(&doc, Some(os), key)
            .or_else(|| (os == Os::Macos).then(|| str_at(&doc, None, key)).flatten());
        if old.as_deref() == *new {
            continue;
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
    for key in OWNED_KEYS {
        let Some((k, item)) = doc.as_table_mut().remove_entry(key) else {
            continue;
        };
        let mac = platform_table(&mut doc, Os::Macos).map_err(err)?;
        if !mac.contains_key(key) {
            mac.insert(k.get(), item);
            if let Some(mut moved) = mac.key_mut(key) {
                *moved.leaf_decor_mut() = k.leaf_decor().clone();
            }
        }
    }
    for c in &changes {
        let t = platform_table(&mut doc, os).map_err(err)?;
        match &c.new {
            // In place when the key is there: `insert` would replace the
            // key too, and the comment above it lives on the key.
            Some(v) => match t.get_mut(&c.key) {
                Some(item) => {
                    let mut new = toml_edit::value(v.as_str());
                    if let (Some(old), Some(nv)) = (item.as_value(), new.as_value_mut()) {
                        *nv.decor_mut() = old.decor().clone();
                    }
                    *item = new;
                }
                None => {
                    t.insert(&c.key, toml_edit::value(v.as_str()));
                }
            },
            None => {
                t.remove(&c.key);
            }
        }
    }
    prune_platforms(&mut doc);
    let out = doc.to_string();
    // Never leave a file behind that the next load would reject.
    for os in Os::ALL {
        parse(&out, os, Layer::User).map_err(err)?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, path)?;
    Ok(changes)
}

/// One edit of the user's own launch keys in L4's `[platforms.<os>]`
/// (#234, `gogo linux-app env` / `args`). Unlike [`OWNED_KEYS`], these are
/// the user's: gogo writes them only when asked to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserEdit {
    /// `env.<KEY> = "<value>"`.
    SetEnv(String, String),
    /// Drop `env.<KEY>`.
    UnsetEnv(String),
    /// `extra_args = [...]`; `None` drops it.
    ExtraArgs(Option<Vec<String>>),
    /// `wrapper = [...]`; `None` drops it.
    Wrapper(Option<Vec<String>>),
    /// `display = "..."`; `None` drops it (#292).
    Display(Option<DisplayMode>),
    /// `hdr = "..."`; `None` drops it (#292).
    Hdr(Option<Hdr>),
    /// `monitor = "..."`; `None` drops it (#295).
    Monitor(Option<String>),
}

/// Apply `edits` to the L4 file for `gogid` under `[platforms.<os>]`,
/// keeping everything else (comments included). The `env` table is made as
/// `[platforms.<os>.env]` when missing and removed once empty. Returns the
/// values that changed for `os` (`env.<KEY>`; `extra_args` and `wrapper` as a
/// debug-quoted list); no change, no write. Key names aren't checked here: the caller
/// knows which ones it allows.
pub fn edit_user_platform_for(
    gogid: &str,
    edits: &[UserEdit],
    os: Os,
) -> Result<Vec<Change>, MacAppError> {
    validate_gogid(gogid)?;
    edit_user_platform_at(&user_path(gogid), edits, os)
}

/// One string key of [`edit_user_platform_at`]'s table: set or dropped,
/// with its [`Change`]. `false`: it already was.
fn edit_word(
    t: &mut dyn toml_edit::TableLike,
    key: &str,
    old: Option<&str>,
    new: Option<&str>,
    changes: &mut Vec<Change>,
) -> bool {
    if old == new {
        return false;
    }
    match new {
        Some(v) => {
            t.insert(key, toml_edit::value(v));
        }
        None => {
            t.remove(key);
        }
    }
    changes.push(Change {
        key: key.into(),
        old: old.map(str::to_string),
        new: new.map(str::to_string),
    });
    true
}

fn edit_user_platform_at(
    path: &Path,
    edits: &[UserEdit],
    os: Os,
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
    // What this OS reads now: the old values, legacy keys and all.
    let mut now = parse(&raw, os, Layer::User).map_err(err)?.entry;
    let mut doc: toml_edit::DocumentMut = raw.parse().map_err(|e| err(format!("{e}")))?;
    let args_text = |a: &Option<Vec<String>>| a.as_ref().map(|a| format!("{a:?}"));
    let mut changes = Vec::new();
    for edit in edits {
        let t = platform_table(&mut doc, os).map_err(err)?;
        match edit {
            UserEdit::SetEnv(k, _) | UserEdit::UnsetEnv(k) => {
                let new = match edit {
                    UserEdit::SetEnv(_, v) => Some(v.clone()),
                    _ => None,
                };
                let old = now.env.get(k).cloned();
                if old == new {
                    continue;
                }
                if t.get("env").is_none() {
                    t.insert("env", toml_edit::Item::Table(toml_edit::Table::new()));
                }
                let env = t
                    .get_mut("env")
                    .and_then(toml_edit::Item::as_table_like_mut)
                    .ok_or_else(|| err(format!("`platforms.{}.env` must be a table", os.key())))?;
                match &new {
                    Some(v) => match env.get_mut(k) {
                        Some(item) => {
                            let mut nv = toml_edit::value(v.as_str());
                            if let (Some(old), Some(n)) = (item.as_value(), nv.as_value_mut()) {
                                *n.decor_mut() = old.decor().clone();
                            }
                            *item = nv;
                        }
                        None => {
                            env.insert(k, toml_edit::value(v.as_str()));
                        }
                    },
                    None => {
                        env.remove(k);
                    }
                }
                if env.is_empty() {
                    t.remove("env");
                }
                match &new {
                    Some(v) => now.env.insert(k.clone(), v.clone()),
                    None => now.env.remove(k),
                };
                changes.push(Change {
                    key: format!("env.{k}"),
                    old,
                    new,
                });
            }
            UserEdit::ExtraArgs(new) | UserEdit::Wrapper(new) => {
                let (key, held) = match edit {
                    UserEdit::ExtraArgs(_) => ("extra_args", &mut now.extra_args),
                    _ => ("wrapper", &mut now.wrapper),
                };
                if *held == *new {
                    continue;
                }
                match new {
                    Some(a) => {
                        let arr: toml_edit::Array = a.iter().map(String::as_str).collect();
                        t.insert(key, toml_edit::value(arr));
                    }
                    None => {
                        t.remove(key);
                    }
                }
                changes.push(Change {
                    key: key.into(),
                    old: args_text(held),
                    new: args_text(new),
                });
                held.clone_from(new);
            }
            UserEdit::Display(new) => {
                let new = new.map(DisplayMode::key);
                let old = now.display.map(DisplayMode::key);
                if edit_word(t, "display", old, new, &mut changes) {
                    now.display = new.and_then(DisplayMode::from_key);
                }
            }
            UserEdit::Hdr(new) => {
                let new = new.map(Hdr::key);
                let old = now.hdr.map(Hdr::key);
                if edit_word(t, "hdr", old, new, &mut changes) {
                    now.hdr = new.and_then(Hdr::from_key);
                }
            }
            UserEdit::Monitor(new) => {
                if let Some(m) = new {
                    check_monitor(m).map_err(err)?;
                }
                let old = now.monitor.clone();
                if edit_word(t, "monitor", old.as_deref(), new.as_deref(), &mut changes) {
                    now.monitor.clone_from(new);
                }
            }
        }
    }
    if changes.is_empty() {
        return Ok(changes);
    }
    prune_platforms(&mut doc);
    let out = doc.to_string();
    for os in Os::ALL {
        parse(&out, os, Layer::User).map_err(err)?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, path)?;
    Ok(changes)
}

/// `[platforms.<os>]` in an L4 document, made if missing (`[platforms]`
/// itself stays implicit, so no empty header is written).
fn platform_table(
    doc: &mut toml_edit::DocumentMut,
    os: Os,
) -> Result<&mut dyn toml_edit::TableLike, String> {
    let platforms = doc.entry("platforms").or_insert_with(|| {
        let mut t = toml_edit::Table::new();
        t.set_implicit(true);
        toml_edit::Item::Table(t)
    });
    let platforms = platforms
        .as_table_like_mut()
        .ok_or("`platforms` must be a table of `[platforms.<os>]`")?;
    if platforms.get(os.key()).is_none() {
        // Implicit: no `[platforms.<os>]` header while it holds only
        // sub-tables (`[platforms.linux.env]`).
        let mut t = toml_edit::Table::new();
        t.set_implicit(true);
        platforms.insert(os.key(), toml_edit::Item::Table(t));
    }
    platforms
        .get_mut(os.key())
        .and_then(toml_edit::Item::as_table_like_mut)
        .ok_or_else(|| format!("`platforms.{}` must be a table", os.key()))
}

/// Drop `[platforms.<os>]` tables gogo emptied, and `platforms` with them.
fn prune_platforms(doc: &mut toml_edit::DocumentMut) {
    let Some(platforms) = doc.get_mut("platforms").and_then(|p| p.as_table_like_mut()) else {
        return;
    };
    for os in Os::ALL {
        if platforms
            .get(os.key())
            .and_then(|t| t.as_table_like())
            .is_some_and(|t| t.is_empty())
        {
            platforms.remove(os.key());
        }
    }
    if platforms.is_empty() {
        doc.remove("platforms");
    }
}

/// Shown in place of an env value [`is_secret`] hides.
pub const HIDDEN: &str = "<hidden>";

/// An env key whose value may be a credential: `*TOKEN*`, `*SECRET*`,
/// `*PASSWORD*`, `*PASSWD*`, `*API_KEY*`, `*APIKEY*`, `*CREDENTIAL*`,
/// `*AUTH*`, `*PRIVATE*` (any case). Their values are stored as given but
/// shown as [`HIDDEN`] (#234, design 18 §7.6).
pub fn is_secret(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "API_KEY",
        "APIKEY",
        "CREDENTIAL",
        "AUTH",
        "PRIVATE",
    ]
    .iter()
    .any(|s| k.contains(s))
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
    pub files: Option<Sourced<Vec<GameFile>>>,
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
    /// Per lane, whole: a higher layer's `[lanes.<lane>]` replaces a lower
    /// one's, and a layer that sets the top-level `status` drops them all.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub lanes: BTreeMap<Lane, Sourced<LaneStatus>>,
    /// Linux only ([`parse`] drops it for other OSes). `false` puts
    /// `PROTONFIXES_DISABLE = "1"` into `env` ([`merge`], #171); the Linux
    /// launcher exports `env` (#177).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protonfixes: Option<Sourced<bool>>,
    /// Linux only: appended after the effective `args` at launch (#234).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_args: Option<Sourced<Vec<String>>>,
    /// Linux only: what the launcher runs the game under (#271).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrapper: Option<Sourced<Vec<String>>>,
    /// Linux only: Wine's display driver for the game (#292).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<Sourced<DisplayMode>>,
    /// Linux only: the HDR switch, with `display = "wayland"` (#292).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hdr: Option<Sourced<Hdr>>,
    /// Linux only: the primary monitor for the Wayland driver (#295).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor: Option<Sourced<String>>,
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
        take(&mut out.files, &e.files, layer);
        take(&mut out.args, &e.args, layer);
        take(&mut out.variant, &e.variant, layer);
        take(&mut out.replace_command, &e.replace_command, layer);
        if e.status.is_some() {
            out.lane = None;
            out.lanes.clear();
        }
        take(&mut out.status, &e.status, layer);
        take(&mut out.lane, &e.lane, layer);
        for (lane, s) in &e.lanes {
            out.lanes.insert(
                *lane,
                Sourced {
                    value: s.clone(),
                    layer,
                },
            );
        }
        take(&mut out.tested, &e.tested, layer);
        take(&mut out.notes, &e.notes, layer);
        take(&mut out.protonfixes, &e.protonfixes, layer);
        take(&mut out.extra_args, &e.extra_args, layer);
        take(&mut out.wrapper, &e.wrapper, layer);
        take(&mut out.display, &e.display, layer);
        take(&mut out.hdr, &e.hdr, layer);
        take(&mut out.monitor, &e.monitor, layer);
        take(&mut out.source, &e.source, layer);
    }
    // `protonfixes = false` turns umu-protonfixes off for the game (design
    // 18 §4.3, O2), from its layer: a higher layer's own env value wins.
    if let Some(p) = &out.protonfixes
        && !p.value
        && out
            .env
            .get(PROTONFIXES_DISABLE)
            .is_none_or(|e| e.layer <= p.layer)
    {
        out.env.insert(
            PROTONFIXES_DISABLE.into(),
            Sourced {
                value: "1".into(),
                layer: p.layer,
            },
        );
    }
    out
}

/// The env var umu-protonfixes checks to skip every fix (any value but
/// `0`), set by `protonfixes = false`.
pub const PROTONFIXES_DISABLE: &str = "PROTONFIXES_DISABLE";

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
        for (k, s) in &self.env {
            let v = if is_secret(k) {
                HIDDEN.to_string()
            } else {
                format!("{:?}", s.value)
            };
            out.push(format!("env.{k} = {v}  ({})", s.layer.describe()));
        }
        line(&mut out, "registry", &self.registry);
        if let Some(s) = &self.files {
            for f in &s.value {
                out.push(format!(
                    "files {:?} ({} bytes)  ({})",
                    f.path,
                    f.text.len(),
                    s.layer.describe()
                ));
            }
        }
        line(&mut out, "args", &self.args);
        line(&mut out, "extra_args", &self.extra_args);
        line(&mut out, "wrapper", &self.wrapper);
        line(&mut out, "display", &self.display);
        line(&mut out, "hdr", &self.hdr);
        line(&mut out, "monitor", &self.monitor);
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
        for (lane, s) in &self.lanes {
            let at = s.layer.describe();
            let lane = lane.key();
            if let Some(v) = &s.value.status {
                out.push(format!("lanes.{lane}.status = {v:?}  ({at})"));
            }
            if let Some(t) = &s.value.tested {
                out.push(format!("lanes.{lane}.tested = {} run(s)  ({at})", t.len()));
            }
            if let Some(n) = &s.value.notes {
                out.push(format!("lanes.{lane}.notes = {n:?}  ({at})"));
            }
        }
        line(&mut out, "protonfixes", &self.protonfixes);
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

    /// The status as it applies to a bundle built from `lane` (#56): its
    /// `[lanes.<lane>]` status if it has one, else the top-level one unless
    /// that was seen on the other lane ("untested", with what the other
    /// lane saw in brackets). `None` = any lane: the top-level status.
    pub fn status_for(&self, lane: Option<Lane>) -> Status {
        if let Some(s) = lane.and_then(|l| self.lane_status(l)) {
            return s.status.unwrap_or_default();
        }
        match (self.lane.as_ref().map(|l| l.value), lane) {
            (Some(seen), Some(built)) if seen != built => Status::Unknown,
            _ => self.status.as_ref().map_or(Status::Unknown, |s| s.value),
        }
    }

    /// [`status_line`](Self::status_line) for a bundle built from `lane`.
    pub fn status_line_for(&self, lane: Option<Lane>) -> String {
        let seen = self.lane.as_ref().map(|l| l.value);
        let Some(built) = lane else {
            // Every lane: the top-level line, then each lane's own.
            let mut parts = Vec::new();
            if self.status_for(None) != Status::Unknown {
                parts.push(match seen {
                    Some(seen) => format!("{} (from {})", self.top_line(), seen.describe()),
                    None => self.top_line(),
                });
            }
            for (l, s) in self.lanes_with_status() {
                parts.push(format!("{}: {}", l.describe(), lane_line(s)));
            }
            return if parts.is_empty() {
                "untested".into()
            } else {
                parts.join("; ")
            };
        };
        if let Some(s) = self.lane_status(built) {
            return lane_line(s);
        }
        if self.status_for(Some(built)) != Status::Unknown {
            return self.top_line();
        }
        // Nothing seen on this lane: quote what the other lane saw.
        let mut other = Vec::new();
        if let Some(seen) = seen.filter(|s| *s != built)
            && self.status_for(None) != Status::Unknown
        {
            other.push(format!("{}: {}", seen.describe(), self.top_line()));
        }
        for (l, s) in self.lanes_with_status().filter(|(l, _)| *l != built) {
            other.push(format!("{}: {}", l.describe(), lane_line(s)));
        }
        if other.is_empty() {
            "untested".into()
        } else {
            format!("untested from {} ({})", built.describe(), other.join("; "))
        }
    }

    /// `[lanes.<lane>]`, if it sets a status.
    fn lane_status(&self, lane: Lane) -> Option<&LaneStatus> {
        self.lanes
            .get(&lane)
            .map(|s| &s.value)
            .filter(|s| s.status.is_some())
    }

    fn lanes_with_status(&self) -> impl Iterator<Item = (Lane, &LaneStatus)> {
        self.lanes
            .keys()
            .filter_map(|l| self.lane_status(*l).map(|s| (*l, s)))
    }

    fn top_line(&self) -> String {
        status_text(
            self.status.as_ref().map_or(Status::Unknown, |s| s.value),
            self.notes.as_ref().map(|n| n.value.as_str()),
            self.tested.as_ref().map(|t| t.value.as_slice()),
        )
    }
}

fn lane_line(s: &LaneStatus) -> String {
    status_text(
        s.status.unwrap_or_default(),
        s.notes.as_deref(),
        s.tested.as_deref(),
    )
}

/// `known to boot on <runtime> (<date>)`, `known broken: <notes>`,
/// `untested`. The runtime is the most recent `tested` run (the last one
/// when none carries a date).
fn status_text(status: Status, notes: Option<&str>, tested: Option<&[Tested]>) -> String {
    let verb = match status {
        Status::Unknown => return "untested".into(),
        Status::Broken => {
            return match notes {
                Some(n) => format!("known broken: {n}"),
                None => "known broken".into(),
            };
        }
        Status::Boots => "boot",
        Status::Plays => "play",
    };
    let latest = tested.and_then(|t| {
        t.iter()
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

/// L3 merged with L4 for `gogid` (what the build reads before confirming),
/// plus both layers' warnings. An unreadable L4 file is a warning here, not
/// an error: setup refuses it later, with the path.
pub fn db_and_user(gogid: &str) -> (Effective, Vec<String>) {
    let db = db_entry(gogid);
    let mut warnings: Vec<String> = db
        .warnings
        .iter()
        .map(|w| format!("compat DB {gogid}: {w}"))
        .collect();
    let user = match load_user(gogid) {
        Ok(p) => {
            let path = user_path(gogid);
            warnings.extend(
                p.warnings
                    .iter()
                    .map(|w| format!("{}: {w}", path.display())),
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

// OS-independent: every test names the OS it resolves for (#166).
#[cfg(test)]
mod compat_tests {
    use super::*;

    /// An L3 entry resolved for macOS.
    fn db(raw: &str) -> Parsed {
        parse(raw, Os::Macos, Layer::Db).unwrap()
    }

    /// An L4 entry resolved for macOS. Its legacy top-level keys read as
    /// `[platforms.macos]`, so the old shape stays a short way to write a
    /// macOS layer in these tests.
    fn mac(raw: &str) -> CompatEntry {
        parse(raw, Os::Macos, Layer::User).unwrap().entry
    }

    const REBEL_GALAXY: &str = r#"
# compat/1435582019.toml — Rebel Galaxy
title    = "Rebel Galaxy"
exe      = "RebelGalaxyGOG.exe"        # skip GoGLauncher.exe
verbs    = ["d3dcompiler_43", "d3dx11_43"]
source   = "gogo-vm"
dll_overrides = { ddraw = "n,b" }
registry = [{ key = 'HKCU\Software\Wine\Direct3D', name = "renderer", data = "gl" }]
args     = ["-windowed"]
replace_command = { from = "GoGLauncher.exe", to = "RebelGalaxyGOG.exe" }

[platforms.macos]
renderer = "dxmt"
status   = "boots"
tested   = [{ runtime = "winecx-24.0.7_7", host = "m4pro/macos-27.0.1", date = 2026-10-22 }]
notes    = "boots to the main menu"
env      = { WINEMSYNC = "1" }
"#;

    #[test]
    fn parses_the_docs19_example() {
        let p = db(REBEL_GALAXY);
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
        let p = db("future = 1\n[platforms.macos]\nrenderer = \"wined3d\"\n\
             tested = [{ runtime = \"x\", gpu = \"m4\" }]\n[platforms.windows]\nx = 1\n");
        assert_eq!(p.entry.renderer.as_deref(), Some("wined3d"));
        assert_eq!(
            p.warnings,
            [
                "unknown key future",
                "unknown key platforms.macos.tested[0].gpu",
                "unknown key platforms.windows",
            ]
        );
    }

    #[test]
    fn variant_is_a_known_key_and_merges_l4_over_l3() {
        let l3 = db("variant = \"Settings\"\n");
        assert!(l3.warnings.is_empty(), "{:?}", l3.warnings);
        let l4 = mac("variant = \"Carmageddon TDR 2000 (safe mode)\"\n");
        let eff = merge(&[(Layer::Db, &l3.entry), (Layer::User, &l4)]);
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
        assert!(parse("variant = [\"a\"]", Os::Macos, Layer::Db).is_err());
    }

    #[test]
    fn bad_values_are_errors() {
        for os in Os::ALL {
            for layer in [Layer::Db, Layer::User] {
                for bad in [
                    "[platforms.macos]\nstatus = \"great\"",
                    "[platforms.linux]\nstatus = \"great\"",
                    "verbs = \"not-a-list\"",
                    "[platforms.macos]\nverbs = \"not-a-list\"",
                    "platforms = 1",
                    "platforms = { linux = 1 }",
                    "not toml at all =",
                ] {
                    assert!(parse(bad, os, layer).is_err(), "{bad} ({os:?}, {layer:?})");
                }
            }
        }
        // A legacy L4 key is checked too, on every OS.
        assert!(parse("status = \"great\"", Os::Linux, Layer::User).is_err());
    }

    #[test]
    fn missing_user_file_is_an_empty_layer() {
        let tmp = tempfile::tempdir().unwrap();
        let p = load_user_at(&tmp.path().join("1.toml"), Os::Macos).unwrap();
        assert_eq!(p.entry, CompatEntry::default());
        assert!(p.warnings.is_empty());
    }

    #[test]
    fn broken_user_file_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("1.toml");
        std::fs::write(&path, "renderer = ").unwrap();
        assert!(matches!(
            load_user_at(&path, Os::Macos),
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
        let db = [
            ("1", "[platforms.macos]\nrenderer = \"dxmt\""),
            ("2", "[platforms.macos]\nstatus = \"great\""),
        ];
        let found = db_lookup(&db, "1", Os::Macos).entry;
        assert_eq!(found.renderer.as_deref(), Some("dxmt"));
        assert_eq!(db_lookup(&db, "1", Os::Linux).entry.renderer, None);
        let broken = db_lookup(&db, "2", Os::Macos);
        assert_eq!(broken.entry, CompatEntry::default());
        assert_eq!(broken.warnings.len(), 1);
        assert_eq!(db_lookup(&db, "3", Os::Macos).entry, CompatEntry::default());
    }

    #[test]
    fn compiled_db_entries_all_parse() {
        for (id, raw) in COMPAT_DB {
            validate_gogid(id).unwrap();
            for os in Os::ALL {
                let p = parse(raw, os, Layer::Db)
                    .unwrap_or_else(|e| panic!("compat/{id}.toml ({os:?}): {e}"));
                assert!(p.warnings.is_empty(), "compat/{id}.toml: {:?}", p.warnings);
            }
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
            for os in Os::ALL {
                let p = parse(&raw, os, Layer::Db)
                    .unwrap_or_else(|e| panic!("compat/{name} ({os:?}): {e}"));
                assert!(p.warnings.is_empty(), "compat/{name}: {:?}", p.warnings);
                let e = p.entry;
                assert!(
                    e.source.as_deref().is_some_and(|s| !s.is_empty()),
                    "compat/{name}: no source"
                );
                if e.status.unwrap_or_default() != Status::Unknown {
                    assert!(
                        e.tested.as_ref().is_some_and(|t| !t.is_empty()),
                        "compat/{name} ({os:?}): status without a tested run"
                    );
                }
                for (lane, s) in &e.lanes {
                    if s.status.unwrap_or_default() != Status::Unknown {
                        assert!(
                            s.tested.as_ref().is_some_and(|t| !t.is_empty()),
                            "compat/{name} ({os:?}): lanes.{} status without a tested run",
                            lane.key()
                        );
                    }
                }
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
        let db = db(REBEL_GALAXY).entry;
        let user = mac(
            "renderer = \"wined3d\"\nverbs = [\"-d3dx11_43\", \"xinput\"]\nenv = { DXVK_HUD = \"1\", WINEMSYNC = \"0\" }\n",
        );
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
            let (db, user) = (mac(db), mac(user));
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
            let (db, user) = (mac(db), mac(user));
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
        assert!(parse("lane = \"floppy\"", Os::Macos, Layer::User).is_err());
    }

    #[test]
    fn lanes_give_each_build_lane_its_own_status() {
        let eff = |db: &str, user: &str| {
            let (db, user) = (mac(db), mac(user));
            merge(&[(Layer::Db, &db), (Layer::User, &user)])
        };
        let pharaoh = r#"
status = "broken"
notes = "asks for the CD"
lane = "depot"
tested = [{ runtime = "winecx/24.0.7_7", date = 2026-10-23 }]

[lanes.installer]
status = "plays"
tested = [{ runtime = "winecx/24.0.7_7", host = "m4pro/macos-27.0.1", date = 2026-10-23 }]
"#;
        let e = eff(pharaoh, "");
        assert_eq!(e.status_for(Some(Lane::Depot)), Status::Broken);
        assert_eq!(e.status_for(Some(Lane::Installer)), Status::Plays);
        assert_eq!(
            e.status_line_for(Some(Lane::Depot)),
            "known broken: asks for the CD"
        );
        assert_eq!(
            e.status_line_for(Some(Lane::Installer)),
            "known to play on winecx/24.0.7_7 (2026-10-23)"
        );
        assert_eq!(
            e.status_line(),
            "known broken: asks for the CD (from the depot build); \
             the offline installer: known to play on winecx/24.0.7_7 (2026-10-23)"
        );
        assert!(
            e.display_lines()
                .contains(&"lanes.installer.status = Plays  (L3 compat DB)".to_string())
        );

        // Only a lane entry: the other lane is untested, and says so.
        let only = "[lanes.installer]\nstatus = \"boots\"";
        assert_eq!(
            eff(only, "").status_line_for(Some(Lane::Depot)),
            "untested from the depot build (the offline installer: known to boot)"
        );

        // L4 replaces one lane whole and keeps the other.
        let e = eff(pharaoh, "[lanes.installer]\nstatus = \"boots\"");
        assert_eq!(e.status_for(Some(Lane::Installer)), Status::Boots);
        assert_eq!(e.status_for(Some(Lane::Depot)), Status::Broken);
        assert_eq!(e.lanes[&Lane::Installer].layer, Layer::User);

        // An L4 top-level status drops L3's lanes with its lane.
        let e = eff(pharaoh, "status = \"plays\"");
        assert!(e.lanes.is_empty());
        assert_eq!(e.status_for(Some(Lane::Depot)), Status::Plays);

        assert_eq!(
            db("[platforms.macos.lanes.depot]\nstatus = \"plays\"\nfoo = 1").warnings,
            ["unknown key platforms.macos.lanes.depot.foo"]
        );
        assert!(
            parse(
                "[platforms.macos.lanes.floppy]\nstatus = \"plays\"",
                Os::Macos,
                Layer::Db
            )
            .is_err()
        );
    }

    #[test]
    fn set_user_keys_keeps_comments_and_reports_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("compat/1.toml");
        let set = |u: &[(&str, Option<&str>)]| set_user_keys_at(&path, u, Os::Macos).unwrap();
        // Fresh file.
        let c = set(&[("runtime", Some("winecx/24.0.7_7"))]);
        assert_eq!(
            c,
            vec![Change {
                key: "runtime".into(),
                old: None,
                new: Some("winecx/24.0.7_7".into())
            }]
        );
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw, "[platforms.macos]\nruntime = \"winecx/24.0.7_7\"\n");
        // The user adds their own keys and a comment.
        let raw =
            format!("# my note\nverbs = [\"xinput\"]\n\n{raw}env = {{ WINEMSYNC = \"1\" }}\n");
        std::fs::write(&path, &raw).unwrap();
        // Same value again: no change, no write.
        assert!(set(&[("runtime", Some("winecx/24.0.7_7"))]).is_empty());
        let c = set(&[("renderer", Some("dxmt/v0.80")), ("runtime", None)]);
        assert_eq!(c.len(), 2);
        assert_eq!(c[1].old.as_deref(), Some("winecx/24.0.7_7"));
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("# my note"), "{after}");
        // Comments on a key gogo overwrites in its table stay too.
        let raw = after.replace(
            "renderer = \"dxmt/v0.80\"",
            "# why dxmt\nrenderer = \"dxmt/v0.80\" # pinned",
        );
        std::fs::write(&path, &raw).unwrap();
        set(&[("renderer", Some("wined3d"))]);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.contains("# why dxmt\nrenderer = \"wined3d\" # pinned"),
            "{after}"
        );
        set(&[("renderer", Some("dxmt/v0.80"))]);
        let p = load_user_at(&path, Os::Macos).unwrap().entry;
        assert_eq!(p.renderer.as_deref(), Some("dxmt/v0.80"));
        assert_eq!(p.runtime, None);
        assert_eq!(p.env["WINEMSYNC"], "1");
        assert_eq!(p.verbs.as_deref(), Some(&["xinput".to_string()][..]));
        // Nothing gogo wrote applies on Linux.
        assert_eq!(load_user_at(&path, Os::Linux).unwrap().entry.renderer, None);
    }

    #[test]
    fn set_user_keys_writes_this_os_table_only() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("1.toml");
        set_user_keys_at(&path, &[("renderer", Some("dxmt"))], Os::Macos).unwrap();
        set_user_keys_at(&path, &[("renderer", Some("wined3d"))], Os::Linux).unwrap();
        let r = |os| load_user_at(&path, os).unwrap().entry.renderer;
        assert_eq!(r(Os::Macos).as_deref(), Some("dxmt"));
        assert_eq!(r(Os::Linux).as_deref(), Some("wined3d"));
        // Removing the last key gogo wrote there drops the table.
        set_user_keys_at(&path, &[("renderer", None)], Os::Linux).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw, "[platforms.macos]\nrenderer = \"dxmt\"\n");
        // gogo never writes a Mac-only renderer for Linux.
        let err = set_user_keys_at(&path, &[("renderer", Some("dxmt/v0.80"))], Os::Linux);
        assert!(err.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
    }

    /// An L4 file from before #166: gogo's legacy keys move under
    /// `[platforms.macos]` on its next write, the user's stay put.
    #[test]
    fn set_user_keys_moves_legacy_owned_keys() {
        let legacy = "# my notes\nruntime = \"wine-staging/11.18\"  # pinned\n\
                      renderer = \"dxmt/v0.80\"\nenv = { WINEMSYNC = \"1\" }\n";
        for os in Os::ALL {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("1.toml");
            std::fs::write(&path, legacy).unwrap();
            // Same value as the legacy one (on a Mac): nothing changes.
            if os == Os::Macos {
                let c = set_user_keys_at(&path, &[("renderer", Some("dxmt/v0.80"))], os).unwrap();
                assert!(c.is_empty());
                assert_eq!(std::fs::read_to_string(&path).unwrap(), legacy);
            }
            let c = set_user_keys_at(&path, &[("renderer", Some("wined3d"))], os).unwrap();
            let old = (os == Os::Macos).then(|| "dxmt/v0.80".to_string());
            assert_eq!(c[0].old, old, "{os:?}");
            let after = std::fs::read_to_string(&path).unwrap();
            let doc: toml::Table = toml::from_str(&after).unwrap();
            assert!(
                !doc.contains_key("runtime") && !doc.contains_key("renderer"),
                "{after}"
            );
            // The user's own legacy key is theirs: left at the top.
            assert!(doc.contains_key("env"), "{after}");
            assert!(
                after.contains("# my notes") && after.contains("# pinned"),
                "{after}"
            );
            let m = load_user_at(&path, Os::Macos).unwrap();
            assert_eq!(m.entry.runtime.as_deref(), Some("wine-staging/11.18"));
            assert_eq!(m.entry.env["WINEMSYNC"], "1");
            let l = load_user_at(&path, Os::Linux).unwrap();
            assert_eq!(l.entry.runtime, None);
            assert!(l.entry.env.is_empty());
            if os == Os::Macos {
                assert_eq!(m.entry.renderer.as_deref(), Some("wined3d"));
                assert_eq!(l.entry.renderer, None);
            } else {
                // The Mac keeps its value; Linux gets the new one.
                assert_eq!(m.entry.renderer.as_deref(), Some("dxmt/v0.80"));
                assert_eq!(l.entry.renderer.as_deref(), Some("wined3d"));
            }
        }
        // A legacy key [platforms.macos] already overrides never applied: it goes.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("1.toml");
        std::fs::write(
            &path,
            "runtime = \"a/1\"\n[platforms.macos]\nruntime = \"b/2\"\n",
        )
        .unwrap();
        let c = set_user_keys_at(&path, &[("renderer", Some("wined3d"))], Os::Macos).unwrap();
        assert_eq!(c.len(), 1);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(!after.contains("a/1"), "{after}");
        let e = load_user_at(&path, Os::Macos).unwrap().entry;
        assert_eq!(e.runtime.as_deref(), Some("b/2"));
    }

    #[test]
    fn set_user_keys_refuses_user_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("1.toml");
        assert!(set_user_keys_at(&path, &[("env", Some("x"))], Os::Macos).is_err());
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
        let raw = std::fs::read_to_string(user_path("42")).unwrap();
        assert!(
            raw.contains(&format!("[platforms.{}]", Os::current().key())),
            "{raw}"
        );
        let p = load_user("42").unwrap();
        assert_eq!(p.entry.renderer.as_deref(), Some("wined3d"));
        assert!(load_user("../42").is_err());
        unsafe { std::env::remove_var("GOGO_HOME") };
    }

    #[test]
    fn platform_table_merges_over_the_top_level() {
        let raw = r#"
exe   = "Game.exe"
args  = ["-a"]
verbs = ["d3dx9", "xinput", "d3dcompiler_43"]
dll_overrides = { ddraw = "n,b", dinput = "n" }

[platforms.macos]
renderer = "dxmt"
args     = ["-windowed"]
verbs    = ["-xinput", "xact"]
dll_overrides = { ddraw = "b" }
env      = { WINEMSYNC = "1" }

[platforms.linux]
renderer = "wined3d"
verbs    = ["-*", "vcrun2019"]
protonfixes = false
"#;
        let m = db(raw);
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        let m = m.entry;
        assert_eq!(m.exe.as_deref(), Some("Game.exe"));
        assert_eq!(m.renderer.as_deref(), Some("dxmt"));
        assert_eq!(m.args, Some(vec!["-windowed".into()]));
        assert_eq!(m.dll_overrides["ddraw"], "b");
        assert_eq!(m.dll_overrides["dinput"], "n");
        assert_eq!(m.env["WINEMSYNC"], "1");
        assert_eq!(m.protonfixes, None);
        let verbs = |e: &CompatEntry| {
            merge(&[(Layer::Db, e)])
                .verbs
                .into_iter()
                .map(|s| s.value)
                .collect::<Vec<_>>()
        };
        assert_eq!(verbs(&m), ["d3dx9", "d3dcompiler_43", "xact"]);

        let l = parse(raw, Os::Linux, Layer::Db).unwrap().entry;
        assert_eq!(l.renderer.as_deref(), Some("wined3d"));
        assert_eq!(l.args, Some(vec!["-a".into()]));
        assert_eq!(l.dll_overrides["ddraw"], "n,b");
        assert!(l.env.is_empty());
        assert_eq!(l.protonfixes, Some(false));
        assert_eq!(verbs(&l), ["vcrun2019"]);
        // `-*` in a platform table clears the lower layers' verbs too.
        let auto = CompatEntry {
            verbs: Some(vec!["d3dx11_43".into()]),
            ..Default::default()
        };
        let eff = merge(&[(Layer::Auto, &auto), (Layer::Db, &l)]);
        assert_eq!(eff.verbs.len(), 1);
        assert!(!eff.protonfixes.unwrap().value);
    }

    #[test]
    fn a_platform_status_drops_the_lane_below_it() {
        let raw = "[platforms.macos]\nstatus = \"plays\"\n";
        let mut base =
            mac("status = \"broken\"\nlane = \"depot\"\n[lanes.installer]\nstatus = \"boots\"");
        overlay(&mut base, db(raw).entry);
        assert_eq!(base.status, Some(Status::Plays));
        assert_eq!(base.lane, None);
        assert!(base.lanes.is_empty());
    }

    #[test]
    fn os_only_keys_at_the_top_level_warn_in_l3() {
        let p = db(
            "title = \"T\"\nrenderer = \"dxmt\"\nruntime = \"x/1\"\nenv = { A = \"1\" }\n\
                    status = \"plays\"\nprotonfixes = false\n",
        );
        assert_eq!(p.entry.title.as_deref(), Some("T"));
        assert_eq!(p.entry.renderer, None);
        assert_eq!(p.entry.runtime, None);
        assert!(p.entry.env.is_empty());
        assert_eq!(p.entry.status, None);
        assert_eq!(p.entry.protonfixes, None);
        assert_eq!(p.warnings.len(), 5, "{:?}", p.warnings);
        assert!(
            p.warnings
                .contains(&"`renderer` is only valid under [platforms.<os>]: ignored".into()),
            "{:?}",
            p.warnings
        );
        // `protonfixes` was never a top-level key, so in L4 it isn't legacy.
        let p = parse("protonfixes = false", Os::Linux, Layer::User).unwrap();
        assert_eq!(p.entry.protonfixes, None);
        assert_eq!(p.warnings.len(), 1);
        // Linux only.
        let p = db("[platforms.macos]\nprotonfixes = false\n");
        assert_eq!(p.entry.protonfixes, None);
        assert_eq!(
            p.warnings,
            ["`platforms.macos.protonfixes` is Linux only: ignored"]
        );
        assert!(parse("[platforms.linux]\nprotonfixes = 1", Os::Macos, Layer::Db).is_err());
    }

    #[test]
    fn protonfixes_false_sets_protonfixes_disable() {
        let off = "[platforms.linux]\nprotonfixes = false\n";
        let l3 = parse(off, Os::Linux, Layer::Db).unwrap().entry;
        let eff = merge(&[(Layer::Db, &l3)]);
        let env = &eff.env[PROTONFIXES_DISABLE];
        assert_eq!((env.value.as_str(), env.layer), ("1", Layer::Db));
        // A higher layer's own env value wins; a lower one's doesn't.
        let l4 = parse(
            "[platforms.linux]\nenv = { PROTONFIXES_DISABLE = \"0\" }\n",
            Os::Linux,
            Layer::User,
        )
        .unwrap()
        .entry;
        let eff = merge(&[(Layer::Db, &l3), (Layer::User, &l4)]);
        assert_eq!(eff.env[PROTONFIXES_DISABLE].value, "0");
        let eff = merge(&[
            (Layer::Db, &l4),
            (
                Layer::User,
                &parse(off, Os::Linux, Layer::User).unwrap().entry,
            ),
        ]);
        assert_eq!(eff.env[PROTONFIXES_DISABLE].value, "1");
        // `true`, and macOS (where `protonfixes` is dropped): nothing.
        let on = parse(
            "[platforms.linux]\nprotonfixes = true\n",
            Os::Linux,
            Layer::Db,
        )
        .unwrap();
        assert!(merge(&[(Layer::Db, &on.entry)]).env.is_empty());
        let mac = parse(off, Os::Macos, Layer::Db).unwrap();
        assert!(merge(&[(Layer::Db, &mac.entry)]).env.is_empty());
    }

    #[test]
    fn top_fixes_split_off() {
        let raw = "title = \"T\"\nexe = \"G.exe\"\nverbs = [\"a\"]\nargs = [\"-x\"]\n\
                   dll_overrides = { ddraw = \"n\" }\nvariant = \"V\"\n\
                   source = \"umu-protonfixes:gamefixes-steam/1.py@0\"\n\
                   [platforms.linux]\nverbs = [\"b\"]\nnotes = \"n\"\n";
        let (kept, top) = parse_without_top_fixes(raw, Os::Linux, Layer::Db).unwrap();
        assert!(kept.warnings.is_empty(), "{:?}", kept.warnings);
        let k = kept.entry;
        assert_eq!(k.title.as_deref(), Some("T"));
        assert_eq!(k.variant.as_deref(), Some("V"));
        assert_eq!(k.verbs, Some(vec!["b".into()]));
        assert_eq!(k.notes.as_deref(), Some("n"));
        assert!(k.source.unwrap().starts_with("umu-protonfixes:"));
        assert_eq!((k.exe, k.args), (None, None));
        assert!(k.dll_overrides.is_empty());
        assert_eq!(top.verbs, Some(vec!["a".into()]));
        assert_eq!(top.exe.as_deref(), Some("G.exe"));
        assert_eq!(top.dll_overrides["ddraw"], "n");
        assert_eq!((top.title, top.notes), (None, None));
    }

    #[test]
    fn mac_only_renderers_are_an_error_for_linux() {
        for r in ["dxmt", "dxmt/v0.80", "d3dmetal", "d3dmetal/4.0b3"] {
            let raw = format!("[platforms.linux]\nrenderer = \"{r}\"\n");
            for os in Os::ALL {
                let e = parse(&raw, os, Layer::Db).unwrap_err();
                assert!(e.contains("macOS only"), "{e}");
            }
            // Fine for the Mac, and as a legacy (Mac) L4 key on Linux.
            let raw = format!("[platforms.macos]\nrenderer = \"{r}\"\n");
            assert!(parse(&raw, Os::Linux, Layer::Db).is_ok());
            let raw = format!("renderer = \"{r}\"\n");
            assert!(parse(&raw, Os::Linux, Layer::User).is_ok());
        }
        for r in ["wined3d", "cnc-ddraw/v7.1.0.0"] {
            let raw = format!("[platforms.linux]\nrenderer = \"{r}\"\n");
            let e = parse(&raw, Os::Linux, Layer::Db).unwrap().entry;
            assert_eq!(e.renderer.as_deref(), Some(r));
        }
    }

    /// An L4 file from before #166: its top-level OS keys are the Mac's.
    #[test]
    fn legacy_l4_keys_read_as_macos() {
        let raw = "renderer = \"dxmt\"\nruntime = \"winecx/24.0.7_7\"\nexe = \"G.exe\"\n\
                   env = { A = \"1\" }\n";
        let m = parse(raw, Os::Macos, Layer::User).unwrap();
        assert_eq!(m.entry.renderer.as_deref(), Some("dxmt"));
        assert_eq!(m.entry.env["A"], "1");
        assert_eq!(m.warnings.len(), 1, "{:?}", m.warnings);
        assert!(
            m.warnings[0].starts_with(
                "note: top-level `env`, `renderer`, `runtime` read as [platforms.macos]"
            ),
            "{:?}",
            m.warnings
        );
        let l = parse(raw, Os::Linux, Layer::User).unwrap();
        assert_eq!(l.entry.renderer, None);
        assert!(l.entry.env.is_empty());
        assert_eq!(l.entry.exe.as_deref(), Some("G.exe"));
        assert_eq!(l.warnings.len(), 1);
        assert!(
            l.warnings[0].contains("ignored on linux"),
            "{:?}",
            l.warnings
        );
        // [platforms.macos] wins over a legacy key; maps still merge per key.
        let raw = "renderer = \"dxmt\"\nenv = { A = \"1\" }\n\
                   [platforms.macos]\nrenderer = \"wined3d\"\nenv = { B = \"2\" }\n";
        let m = mac(raw);
        assert_eq!(m.renderer.as_deref(), Some("wined3d"));
        assert_eq!((m.env["A"].as_str(), m.env["B"].as_str()), ("1", "2"));
    }

    /// The L3 migration (#166) changed no macOS entry: each pre-#166 file
    /// (kept in `tests/fixtures/compat-pre-166/`, read the way gogo read
    /// them then) merges to the same entry as its migrated `compat/` file.
    /// A file with no `[platforms.linux]` table carries no Linux status.
    #[test]
    fn l3_migration_keeps_every_macos_entry() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let fixtures = root.join("tests/fixtures/compat-pre-166");
        let mut n = 0;
        for f in std::fs::read_dir(&fixtures).unwrap() {
            let path = f.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_string();
            let before: CompatEntry =
                toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let now = std::fs::read_to_string(root.join("../../compat").join(&name)).unwrap();
            let after = db(&now).entry;
            assert_eq!(
                merge(&[(Layer::Db, &before)]),
                merge(&[(Layer::Db, &after)]),
                "compat/{name}"
            );
            if !now.contains("[platforms.linux]") {
                let linux = parse(&now, Os::Linux, Layer::Db).unwrap().entry;
                assert_eq!(linux.status, None, "compat/{name}");
                assert!(
                    linux.lanes.is_empty() && linux.tested.is_none(),
                    "compat/{name}"
                );
            }
            n += 1;
        }
        assert_eq!(n, 11);
    }

    #[test]
    fn user_env_and_extra_args_edits_keep_the_file_and_report_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("compat/1.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "# mine\nverbs = [\"xact\"]\n\n[platforms.macos]\nrenderer = \"dxmt\"\n\n\
             [platforms.linux]\n# the HUD\nenv = { DXVK_HUD = \"fps\" }\n",
        )
        .unwrap();
        let edit = |e: &[UserEdit]| edit_user_platform_at(&path, e, Os::Linux).unwrap();
        let c = edit(&[
            UserEdit::SetEnv("DXVK_HUD".into(), "fps,gpuload".into()),
            UserEdit::SetEnv("ODD".into(), "a 'b' \"c\" $X".into()),
            UserEdit::ExtraArgs(Some(vec!["--launcher-skip".into(), "two words".into()])),
        ]);
        assert_eq!(
            c,
            [
                Change {
                    key: "env.DXVK_HUD".into(),
                    old: Some("fps".into()),
                    new: Some("fps,gpuload".into())
                },
                Change {
                    key: "env.ODD".into(),
                    old: None,
                    new: Some("a 'b' \"c\" $X".into())
                },
                Change {
                    key: "extra_args".into(),
                    old: None,
                    new: Some(r#"["--launcher-skip", "two words"]"#.into())
                },
            ]
        );
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.starts_with("# mine\n"), "{raw}");
        assert!(raw.contains("# the HUD\n"), "{raw}");
        assert!(raw.contains("renderer = \"dxmt\""), "{raw}");
        let linux = parse(&raw, Os::Linux, Layer::User).unwrap();
        assert!(linux.warnings.is_empty(), "{:?}", linux.warnings);
        assert_eq!(linux.entry.env["ODD"], "a 'b' \"c\" $X");
        assert_eq!(
            linux.entry.extra_args.as_deref(),
            Some(["--launcher-skip".to_string(), "two words".to_string()].as_slice())
        );
        // The Mac reads none of it.
        let mac = parse(&raw, Os::Macos, Layer::User).unwrap().entry;
        assert!(mac.env.is_empty() && mac.extra_args.is_none());

        // No change, no write.
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert!(edit(&[UserEdit::SetEnv("ODD".into(), "a 'b' \"c\" $X".into())]).is_empty());
        assert!(edit(&[UserEdit::UnsetEnv("NOPE".into())]).is_empty());
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), mtime);

        // Emptied: the env table and `[platforms.linux]` go, the rest stays.
        let c = edit(&[
            UserEdit::UnsetEnv("DXVK_HUD".into()),
            UserEdit::UnsetEnv("ODD".into()),
            UserEdit::ExtraArgs(None),
        ]);
        assert_eq!(c.len(), 3);
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("linux") && !raw.contains("env"), "{raw}");
        assert!(raw.contains("[platforms.macos]"), "{raw}");

        // A fresh file gets a `[platforms.linux.env]` table.
        let fresh = tmp.path().join("compat/2.toml");
        edit_user_platform_at(
            &fresh,
            &[UserEdit::SetEnv("PROTON_ENABLE_NVAPI".into(), "1".into())],
            Os::Linux,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&fresh).unwrap(),
            "[platforms.linux.env]\nPROTON_ENABLE_NVAPI = \"1\"\n"
        );
    }

    #[test]
    fn wrapper_is_linux_only_replaced_by_the_higher_layer_and_edited_whole() {
        let p = parse(
            "[platforms.macos]\nwrapper = [\"x\"]\n",
            Os::Macos,
            Layer::User,
        )
        .unwrap();
        assert_eq!(p.entry.wrapper, None);
        assert_eq!(
            p.warnings,
            ["`platforms.macos.wrapper` is Linux only: ignored"]
        );
        let at = |raw: &str, layer| parse(raw, Os::Linux, layer).unwrap().entry;
        let eff = merge(&[
            (
                Layer::Db,
                &at(
                    "[platforms.linux]\nwrapper = [\"gamemoderun\"]\n",
                    Layer::Db,
                ),
            ),
            (
                Layer::User,
                &at(
                    "[platforms.linux]\nwrapper = [\"gamescope\", \"--\"]\n",
                    Layer::User,
                ),
            ),
        ]);
        let w = eff.wrapper.unwrap();
        assert_eq!(w.value, ["gamescope", "--"]);
        assert_eq!(w.layer, Layer::User);

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("1.toml");
        std::fs::write(&path, "# mine\n[platforms.linux]\nextra_args = [\"-a\"]\n").unwrap();
        let words = vec!["gamescope".to_string(), "-f".into(), "--".into()];
        let edit = UserEdit::Wrapper(Some(words.clone()));
        let c = edit_user_platform_at(&path, std::slice::from_ref(&edit), Os::Linux).unwrap();
        assert_eq!(c[0].key, "wrapper");
        assert_eq!(c[0].old, None);
        assert_eq!(c[0].new.as_deref(), Some(r#"["gamescope", "-f", "--"]"#));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# mine"), "{text}");
        assert!(text.contains("extra_args"), "{text}");
        let linux = parse(&text, Os::Linux, Layer::User).unwrap().entry;
        assert_eq!(linux.wrapper.as_deref(), Some(words.as_slice()));
        // The same value again: no change, no write.
        assert!(
            edit_user_platform_at(&path, &[edit], Os::Linux)
                .unwrap()
                .is_empty()
        );
        let c = edit_user_platform_at(&path, &[UserEdit::Wrapper(None)], Os::Linux).unwrap();
        assert_eq!(c[0].new, None);
        assert!(!std::fs::read_to_string(&path).unwrap().contains("wrapper"));
    }

    #[test]
    fn extra_args_are_linux_only_and_replaced_by_the_higher_layer() {
        let p = parse(
            "[platforms.macos]\nextra_args = [\"-x\"]\n",
            Os::Macos,
            Layer::User,
        )
        .unwrap();
        assert_eq!(p.entry.extra_args, None);
        assert_eq!(
            p.warnings,
            ["`platforms.macos.extra_args` is Linux only: ignored"]
        );
        let p = parse("extra_args = [\"-x\"]\n", Os::Linux, Layer::Db).unwrap();
        assert_eq!(p.entry.extra_args, None);
        assert_eq!(p.warnings.len(), 1, "{:?}", p.warnings);
        let l3 = parse(
            "[platforms.linux]\nextra_args = [\"-a\"]\n",
            Os::Linux,
            Layer::Db,
        )
        .unwrap()
        .entry;
        let l4 = parse(
            "[platforms.linux]\nextra_args = [\"-b\", \"-c\"]\n",
            Os::Linux,
            Layer::User,
        )
        .unwrap()
        .entry;
        let eff = merge(&[(Layer::User, &l4), (Layer::Db, &l3)]);
        let x = eff.extra_args.unwrap();
        assert_eq!(x.value, ["-b", "-c"]);
        assert_eq!(x.layer, Layer::User);
        // `args` and `extra_args` are separate: one replaces, one appends.
        let l4 = parse("[platforms.linux]\nargs = []\n", Os::Linux, Layer::User)
            .unwrap()
            .entry;
        let eff = merge(&[(Layer::User, &l4), (Layer::Db, &l3)]);
        assert_eq!(eff.args.unwrap().value, Vec::<String>::new());
        assert_eq!(eff.extra_args.unwrap().value, ["-a"]);
    }

    #[test]
    fn files_parse_merge_whole_and_show_their_layer() {
        let raw = "files = [{ path = \"Apps/SimCity 4.ini\", text = \"[Directories]\\nData=..\\\\\\n\" }]\n";
        let l3 = parse(raw, Os::Linux, Layer::Db).unwrap();
        assert!(l3.warnings.is_empty(), "{:?}", l3.warnings);
        let files = l3.entry.files.clone().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "Apps/SimCity 4.ini");
        assert_eq!(files[0].text, "[Directories]\nData=..\\\n");
        // A fix key: an owning protonfix covers it.
        assert!(TOP_FIX_KEYS.contains(&"files"));
        // Valid under a platform table too, where it replaces the top level's.
        let raw2 = "files = [{ path = \"a.ini\", text = \"1\" }]\n\
                    [platforms.linux]\nfiles = [{ path = \"b.ini\", text = \"2\" }]\n";
        let e = parse(raw2, Os::Linux, Layer::Db).unwrap().entry;
        assert_eq!(e.files.unwrap()[0].path, "b.ini");
        // An unknown item key warns.
        let w = parse(
            "files = [{ path = \"a.ini\", text = \"\", mode = \"crlf\" }]\n",
            Os::Linux,
            Layer::Db,
        );
        let w = w.unwrap().warnings;
        assert!(w.iter().any(|w| w.contains("files[0].mode")), "{w:?}");
        // L4 replaces L3's list whole, and the summary names the layer.
        let l4 = parse(
            "files = [{ path = \"other.ini\", text = \"x\" }]\n",
            Os::Linux,
            Layer::User,
        )
        .unwrap()
        .entry;
        let eff = merge(&[(Layer::Db, &l3.entry), (Layer::User, &l4)]);
        let f = eff.files.as_ref().unwrap();
        assert_eq!((f.value.len(), f.layer), (1, Layer::User));
        assert_eq!(f.value[0].path, "other.ini");
        let eff = merge(&[(Layer::Db, &l3.entry)]);
        let lines = eff.display_lines();
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("files \"Apps/SimCity 4.ini\" (23 bytes)")
                    && l.ends_with(&format!("({})", Layer::Db.describe()))),
            "{lines:?}"
        );
    }

    #[test]
    fn files_that_would_leave_the_game_dir_are_refused() {
        for (path, why) in [
            ("", "empty"),
            ("/etc/passwd", "absolute"),
            ("C:/GOG/x.ini", "absolute"),
            ("c:x.ini", "absolute"),
            (r"Apps\x.ini", "backslash"),
            ("../x.ini", "`..`"),
            ("Apps/../../x.ini", "`..`"),
            ("Apps//x.ini", "empty, `.`"),
            ("./x.ini", "empty, `.`"),
            ("Apps/", "empty, `.`"),
        ] {
            let raw = format!("files = [{{ path = {path:?}, text = \"x\" }}]\n");
            for os in Os::ALL {
                let e = parse(&raw, os, Layer::Db).unwrap_err();
                assert!(e.starts_with("files: ") && e.contains(why), "{path:?}: {e}");
            }
            let raw = format!("[platforms.linux]\nfiles = [{{ path = {path:?}, text = \"x\" }}]\n");
            let e = parse(&raw, Os::Linux, Layer::User).unwrap_err();
            assert!(e.starts_with("[platforms.linux] files: "), "{path:?}: {e}");
        }
        // The same file twice, by Windows' case-blind names.
        let raw =
            "files = [{ path = \"a.ini\", text = \"1\" }, { path = \"A.INI\", text = \"2\" }]\n";
        let e = parse(raw, Os::Linux, Layer::Db).unwrap_err();
        assert!(e.contains("twice"), "{e}");
        // Dots inside a name are fine.
        assert!(
            parse(
                "files = [{ path = \"Apps/..x/a..b.ini\", text = \"\" }]\n",
                Os::Linux,
                Layer::Db
            )
            .is_ok()
        );
    }
}
