//! Per-game compat data (docs/19 §6.4–§6.5, design 16 decision 5): one
//! declarative entry per GOG product id. The same schema serves the curated
//! compat DB (L3, `compat/` at the repo root, compiled in) and the user's
//! override file (L4, `<state_dir>/compat/<gogid>.toml`). The other layers
//! (L0 default runtime, L1 auto renderer, L2 GOG-native) are computed by the
//! pipeline and fed in as [`CompatEntry`] values of their own.
//!
//! [`merge`] applies `flags > L4 > L3 > L2 > L1 > L0` field by field and
//! keeps the winning layer of every field, so `gogo mac-app compat` (and
//! later `doctor`, #79) can show where a value came from. Applying the
//! result at build/setup is #74; this module only parses, merges and writes.

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
/// "this layer has no opinion". Maps are merged per key, everything else is
/// replaced whole (so L4 `verbs = []` really clears L3's verbs).
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
    /// winetricks verbs (applied by setup, never by the launcher).
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace_command: Option<ReplaceCommand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tested: Option<Vec<Tested>>,
    /// Provenance: `gogo-vm`, `umu-protonfixes:<file>@<sha>`, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

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
    "replace_command",
    "status",
    "tested",
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

/// The L3 entry for `gogid`, if the compiled-in DB has one. A DB file that
/// fails to parse is a build-time bug; it reads as "no entry" plus a warning.
pub fn db_entry(gogid: &str) -> Parsed {
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
/// key for the maps).
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verbs: Option<Sourced<Vec<String>>>,
    pub dll_overrides: BTreeMap<String, Sourced<String>>,
    pub env: BTreeMap<String, Sourced<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<Sourced<Vec<RegistryValue>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Sourced<Vec<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replace_command: Option<Sourced<ReplaceCommand>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Sourced<Status>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tested: Option<Sourced<Vec<Tested>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Sourced<String>>,
}

/// Merge layers by precedence (design 16 decision 5), field by field. The
/// input order doesn't matter; two entries for the same layer apply in input
/// order (the later wins).
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
        take(&mut out.verbs, &e.verbs, layer);
        take_map(&mut out.dll_overrides, &e.dll_overrides, layer);
        take_map(&mut out.env, &e.env, layer);
        take(&mut out.registry, &e.registry, layer);
        take(&mut out.args, &e.args, layer);
        take(&mut out.replace_command, &e.replace_command, layer);
        take(&mut out.status, &e.status, layer);
        take(&mut out.tested, &e.tested, layer);
        take(&mut out.source, &e.source, layer);
    }
    out
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
        line(&mut out, "verbs", &self.verbs);
        map(&mut out, "dll_overrides", &self.dll_overrides);
        map(&mut out, "env", &self.env);
        line(&mut out, "registry", &self.registry);
        line(&mut out, "args", &self.args);
        line(&mut out, "replace_command", &self.replace_command);
        line(&mut out, "status", &self.status);
        if let Some(t) = &self.tested {
            out.push(format!(
                "tested = {} run(s)  ({})",
                t.value.len(),
                t.layer.describe()
            ));
        }
        line(&mut out, "source", &self.source);
        out
    }
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
            "renderer = \"wined3d\"\nverbs = []\nenv = { DXVK_HUD = \"1\", WINEMSYNC = \"0\" }\n",
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
        // An explicit empty list replaces the lower layer's list.
        let v = eff.verbs.as_ref().unwrap();
        assert_eq!((v.value.len(), v.layer), (0, Layer::User));
        assert_eq!(eff.exe.as_ref().unwrap().layer, Layer::Db);
        assert_eq!(eff.runtime.as_ref().unwrap().layer, Layer::Default);
        // Maps merge per key.
        assert_eq!(eff.env["WINEMSYNC"].value, "0");
        assert_eq!(eff.env["WINEMSYNC"].layer, Layer::User);
        assert_eq!(eff.env["DXVK_HUD"].layer, Layer::User);
        assert_eq!(eff.dll_overrides["ddraw"].layer, Layer::Db);
        let lines = eff.display_lines();
        assert!(lines.contains(&"renderer = \"wined3d\"  (L4 user override)".to_string()));
        let json = serde_json::to_value(&eff).unwrap();
        assert_eq!(json["renderer"]["layer"], "L4");
        assert_eq!(json["tested"]["value"][0]["date"], "2026-10-22");
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
