//! A Linux game's fixes, worked out at app creation (design 18 §4.3,
//! #171): gogo's layers merged under the ownership rule (O3), the
//! protonfix read by [`super::protonfix`], one [`Summary`] with each fix's
//! source and the collisions marked (O2), and the verbs setup pre-installs
//! (R12).
//!
//! - **Ownership (O3, R11).** When the build has a `gamefixes-gog` module
//!   for the game and protonfixes isn't turned off, an L3 file whose
//!   `source` starts with `umu-protonfixes:` loses its top-level fixes
//!   ([`compat::TOP_FIX_KEYS`]), per file; the summary shows them as
//!   covered. Its `[platforms.linux]` table still applies. The Mac lane
//!   never comes here.
//! - **Collisions (O2, R10).** protonfixes sets its env, DLL overrides and
//!   registry values after gogo's, so a gogo value it also sets loses; the
//!   summary says so. `protonfixes = false` (`[platforms.linux]`) puts
//!   `PROTONFIXES_DISABLE=1` into the effective env ([`compat::merge`]) and
//!   the summary lists what the game loses. gogo never sets it on its own
//!   and never writes a protonfixes localfix.
//!
//! The callers (the build plan, `compat`, setup, `doctor`) come with #176
//! and #178.

use std::path::Path;
use std::process::Command;

use super::protonfix::{self, Fix, Module, OtherCode};
use super::umu::UmuRun;
use crate::macapp::compat::{self, CompatEntry, Effective, Layer, Os};

/// An L3 `source` naming a umu-protonfixes file it was translated from.
pub const UMU_SOURCE: &str = "umu-protonfixes:";

/// Where a fix comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixSource {
    /// The game's own module (`umu-910830`).
    Protonfix(String),
    /// `gamefixes-gog/default.py`.
    ProtonfixDefault,
    Layer(Layer),
}

impl std::fmt::Display for FixSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FixSource::Protonfix(id) => write!(f, "protonfix {id}"),
            FixSource::ProtonfixDefault => write!(f, "protonfix default (every GOG game)"),
            FixSource::Layer(l) => f.write_str(layer_label(*l)),
        }
    }
}

/// The summary's names for gogo's layers (design 18 §4.3).
fn layer_label(l: Layer) -> &'static str {
    match l {
        Layer::Default => "L0 default",
        Layer::Auto => "L1 auto",
        Layer::Gog => "L2 GOG-native",
        Layer::Db => "L3 compat DB",
        Layer::User => "L4 override",
        Layer::Flags => "flags",
    }
}

/// The short tag a collision line uses (`L4 "0" is overridden …`).
fn layer_tag(l: Layer) -> &'static str {
    match l {
        Layer::Default => "L0",
        Layer::Auto => "L1",
        Layer::Gog => "L2",
        Layer::Db => "L3",
        Layer::User => "L4",
        Layer::Flags => "flags",
    }
}

/// What [`resolve`] works from.
pub struct Inputs<'a> {
    /// `umu-<id>`, as `GAMEID` takes it (`umu_id::lookup`).
    pub umu_id: &'a str,
    /// The game's module in the chosen build ([`protonfix::read_module`]).
    pub module: Option<Module>,
    /// The build's `default.py` ([`protonfix::read_default`]).
    pub default: Option<Module>,
    /// The L3 file as text, and its label for warnings
    /// ([`compat::db_raw`]): the ownership rule needs its top level apart.
    pub db: Option<(String, String)>,
    /// Every other layer (L1 auto, L4, flags, …), resolved for Linux.
    pub layers: Vec<(Layer, CompatEntry)>,
}

/// A Linux game's fixes: what gogo applies, and what protonfixes will.
#[derive(Debug, Clone)]
pub struct Fixes {
    pub umu_id: String,
    /// gogo's layers merged, with L3 under the ownership rule.
    pub effective: Effective,
    /// L3's top-level fixes, not applied: the protonfix owns the game.
    pub covered: Option<CompatEntry>,
    pub module: Option<Module>,
    pub default: Option<Module>,
    /// L3's parse warnings, each a full message.
    pub warnings: Vec<String>,
}

/// Merge the layers for Linux under the ownership rule (see the module
/// docs). `protonfixes = false` in any layer that wins keeps L3 whole:
/// with protonfixes off it owns nothing.
pub fn resolve(i: Inputs) -> Fixes {
    let mut warnings = Vec::new();
    let mut l3 = CompatEntry::default();
    let mut covered = None;
    if let Some((label, raw)) = &i.db {
        match compat::parse(raw, Os::Linux, Layer::Db) {
            Ok(p) => {
                warnings.extend(p.warnings.iter().map(|w| format!("{label}: {w}")));
                l3 = p.entry;
            }
            Err(e) => warnings.push(format!("{label} unreadable: {e}")),
        }
    }
    let merge = |l3: &CompatEntry| {
        let mut all: Vec<(Layer, &CompatEntry)> = i.layers.iter().map(|(l, e)| (*l, e)).collect();
        all.push((Layer::Db, l3));
        compat::merge(&all)
    };
    let mut effective = merge(&l3);
    let off = effective.protonfixes.as_ref().is_some_and(|p| !p.value);
    let owned = l3
        .source
        .as_deref()
        .is_some_and(|s| s.starts_with(UMU_SOURCE));
    if i.module.is_some()
        && !off
        && owned
        && let Some((_, raw)) = &i.db
        && let Ok((kept, top)) = compat::parse_without_top_fixes(raw, Os::Linux, Layer::Db)
    {
        effective = merge(&kept.entry);
        if top != CompatEntry::default() {
            covered = Some(top);
        }
    }
    Fixes {
        umu_id: i.umu_id.to_string(),
        effective,
        covered,
        module: i.module,
        default: i.default,
        warnings,
    }
}

impl Fixes {
    /// The layer whose `protonfixes = false` turned protonfixes off.
    pub fn disabled_by(&self) -> Option<Layer> {
        self.effective
            .protonfixes
            .as_ref()
            .filter(|p| !p.value)
            .map(|p| p.layer)
    }

    /// `state.toml`'s `protonfix`: the module's sha256, or `none`
    /// ([`protonfix::compare`] tells setup what changed).
    pub fn protonfix_state(&self) -> String {
        protonfix::state_value(self.module.as_ref())
    }

    /// The verbs setup pre-installs (R12): the effective ones (L1, L3, L4,
    /// in order), then the protonfixes' literal `protontricks` verbs
    /// (`default.py`'s, then the game's) unless protonfixes is off. No
    /// duplicates. Verbs the reader couldn't see still install at launch.
    pub fn setup_verbs(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .effective
            .verbs
            .iter()
            .map(|s| s.value.clone())
            .collect();
        if self.disabled_by().is_none() {
            for m in [&self.default, &self.module].into_iter().flatten() {
                for v in m.reading.fixes.iter().filter_map(Fix::verb) {
                    if !out.iter().any(|o| o == v) {
                        out.push(v.to_string());
                    }
                }
            }
        }
        out
    }

    /// The one list the plan, `compat` and `doctor` show.
    pub fn summary(&self) -> Summary {
        let disabled = self.disabled_by();
        let mut items = Vec::new();
        let pf: Vec<(FixSource, &Fix)> = self
            .protonfixes()
            .flat_map(|(src, m)| m.reading.fixes.iter().map(move |f| (src.clone(), f)))
            .collect();
        for g in gogo_items(&self.effective) {
            if disabled.is_some()
                && g.text
                    .starts_with(&format!("env {}", compat::PROTONFIXES_DISABLE))
            {
                continue; // the header says it
            }
            // The protonfix applied last wins: the game's after default.py.
            let by = g.key.as_ref().and_then(|(norm, value)| {
                pf.iter()
                    .rev()
                    .find_map(|(src, f)| {
                        collision_key(f)
                            .filter(|(n, _)| n == norm)
                            .map(|(_, v)| (src, v))
                    })
                    .filter(|(_, v)| v != value)
            });
            let state = match (by, disabled) {
                (Some((src, v)), None) => State::Overridden {
                    by: src.clone(),
                    by_value: v,
                },
                _ => State::Applies,
            };
            items.push(Item {
                fix: g.text,
                key: g.label,
                value: g.key.map(|(_, v)| v),
                source: FixSource::Layer(g.layer),
                state,
            });
        }
        if let Some(c) = &self.covered {
            let eff = compat::merge(&[(Layer::Db, c)]);
            for g in gogo_items(&eff) {
                items.push(Item {
                    fix: g.text,
                    key: g.label,
                    value: g.key.map(|(_, v)| v),
                    source: FixSource::Layer(Layer::Db),
                    state: State::Covered {
                        by: FixSource::Protonfix(self.umu_id.clone()),
                    },
                });
            }
        }
        let mut other = Vec::new();
        for (src, m) in self.protonfixes() {
            for f in &m.reading.fixes {
                items.push(Item {
                    fix: f.to_string(),
                    key: f.key(),
                    value: collision_key(f).map(|(_, v)| v),
                    source: src.clone(),
                    state: if disabled.is_some() {
                        State::Lost
                    } else {
                        State::Applies
                    },
                });
            }
            for o in &m.reading.other {
                other.push((src.clone(), o.clone()));
            }
        }
        Summary {
            umu_id: self.umu_id.clone(),
            module: self.module.as_ref().map(|m| {
                format!(
                    "{} (sha256 {})",
                    m.describe(),
                    &m.sha256[..m.sha256.len().min(12)]
                )
            }),
            items,
            other,
            disabled,
        }
    }

    /// `default.py`, then the game's module, each with its source label.
    fn protonfixes(&self) -> impl Iterator<Item = (FixSource, &Module)> {
        let d = self
            .default
            .as_ref()
            .map(|m| (FixSource::ProtonfixDefault, m));
        let g = self
            .module
            .as_ref()
            .map(|m| (FixSource::Protonfix(self.umu_id.clone()), m));
        d.into_iter().chain(g)
    }
}

/// One fix in the summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The fix and its value: `env DXVK_ASYNC = "1"`, `verb xact`.
    pub fix: String,
    /// What a collision is about (`env DXVK_ASYNC`), for env, DLL overrides
    /// and registry values.
    pub key: Option<String>,
    /// The value a collision compares, raw.
    pub value: Option<String>,
    pub source: FixSource,
    pub state: State,
}

/// Whether an [`Item`] takes effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Applies,
    /// A protonfix sets the same key to another value after gogo (O2).
    Overridden {
        by: FixSource,
        by_value: String,
    },
    /// An L3 fix not applied: the protonfix owns the game (O3).
    Covered {
        by: FixSource,
    },
    /// A protonfix fix that `protonfixes = false` turns off.
    Lost,
}

/// The fix summary (design 18 §4.3): build it with [`Fixes::summary`],
/// show it with `Display`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub umu_id: String,
    /// The module, when the build has one: `gamefixes-gog/umu-40950.py ->
    /// gamefixes-steam/40950.py (sha256 0123456789ab)`.
    pub module: Option<String>,
    /// gogo's fixes, then L3's covered ones, then `default.py`'s, then the
    /// game module's.
    pub items: Vec<Item>,
    /// Statements the reader couldn't read, per module (R29).
    pub other: Vec<(FixSource, OtherCode)>,
    /// The layer of `protonfixes = false`, when it's set.
    pub disabled: Option<Layer>,
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.module {
            Some(m) => writeln!(f, "protonfix {}: {m}", self.umu_id)?,
            None => writeln!(f, "protonfix {}: none in this Proton build", self.umu_id)?,
        }
        let shown: Vec<&Item> = self
            .items
            .iter()
            .filter(|i| i.state != State::Lost)
            .collect();
        if shown.is_empty() && (self.other.is_empty() || self.disabled.is_some()) {
            writeln!(f, "no known fixes")?;
        }
        for i in shown {
            match &i.state {
                State::Applies => writeln!(f, "{}  ({})", i.fix, i.source)?,
                State::Overridden { by, by_value } => {
                    let FixSource::Layer(l) = i.source else {
                        unreachable!("only gogo's layers are overridden")
                    };
                    writeln!(
                        f,
                        "{}: {} {:?} is overridden at launch by {by} ({by_value:?})",
                        i.key.as_deref().unwrap_or(&i.fix),
                        layer_tag(l),
                        i.value.as_deref().unwrap_or_default(),
                    )?;
                }
                State::Covered { by } => {
                    writeln!(f, "{}  ({}: covered by {by}, not applied)", i.fix, i.source)?
                }
                State::Lost => {}
            }
        }
        match self.disabled {
            None => {
                let mut last = None;
                for (src, o) in &self.other {
                    if last != Some(src) {
                        writeln!(f, "other code in {src} (may do more at launch):")?;
                        last = Some(src);
                    }
                    writeln!(f, "  {o}")?;
                }
            }
            Some(l) => {
                let lost: Vec<&Item> = self
                    .items
                    .iter()
                    .filter(|i| i.state == State::Lost)
                    .collect();
                if lost.is_empty() && self.other.is_empty() {
                    writeln!(
                        f,
                        "protonfixes = false ({}): {}=1, nothing lost",
                        layer_label(l),
                        compat::PROTONFIXES_DISABLE
                    )?;
                } else {
                    writeln!(
                        f,
                        "protonfixes = false ({}): {}=1, so the game loses:",
                        layer_label(l),
                        compat::PROTONFIXES_DISABLE
                    )?;
                    for i in lost {
                        writeln!(f, "  {}  ({})", i.fix, i.source)?;
                    }
                    for (src, o) in &self.other {
                        writeln!(f, "  other code in {src}: {o}")?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// A gogo fix from the effective config.
struct GogoItem {
    text: String,
    /// The collision label (`env K`).
    label: Option<String>,
    /// (normalized key, raw value), for env / DLL / registry fixes.
    key: Option<(String, String)>,
    layer: Layer,
}

/// The fixes of an effective config, in schema order. Not `title`,
/// `runtime`, `status` and the other facts: those aren't fixes.
fn gogo_items(e: &Effective) -> Vec<GogoItem> {
    let mut out = Vec::new();
    let mut plain = |text: String, layer: Layer| {
        out.push(GogoItem {
            text,
            label: None,
            key: None,
            layer,
        })
    };
    if let Some(s) = &e.renderer {
        plain(format!("renderer = {:?}", s.value), s.layer);
    }
    if let Some(s) = &e.exe {
        plain(format!("exe = {:?}", s.value), s.layer);
    }
    if let Some(s) = &e.variant {
        plain(format!("variant = {:?}", s.value), s.layer);
    }
    if let Some(s) = &e.args {
        plain(format!("args = {:?}", s.value), s.layer);
    }
    if let Some(s) = &e.replace_command {
        plain(
            format!("replace_command {:?} -> {:?}", s.value.from, s.value.to),
            s.layer,
        );
    }
    for s in &e.verbs {
        plain(format!("verb {}", s.value), s.layer);
    }
    let mut keyed = |label: String, norm: String, raw: String, text: String, layer: Layer| {
        out.push(GogoItem {
            text,
            label: Some(label),
            key: Some((norm, raw)),
            layer,
        })
    };
    for (dll, s) in &e.dll_overrides {
        let label = format!("dll_overrides {dll}");
        keyed(
            label.clone(),
            dll_key(dll),
            s.value.clone(),
            format!("{label} = {:?}", s.value),
            s.layer,
        );
    }
    for (k, s) in &e.env {
        let label = format!("env {k}");
        keyed(
            label.clone(),
            label.clone(),
            s.value.clone(),
            format!("{label} = {:?}", s.value),
            s.layer,
        );
    }
    if let Some(s) = &e.registry {
        for r in &s.value {
            let name = r.name.as_deref().unwrap_or("@");
            let label = format!("registry {} {name}", r.key);
            let kind = r
                .kind
                .as_deref()
                .map(|k| format!("{k} "))
                .unwrap_or_default();
            keyed(
                label.clone(),
                reg_key(&r.key, name),
                r.data.clone(),
                format!("{label} = {kind}{:?}", r.data),
                s.layer,
            );
        }
    }
    out
}

/// A protonfix fix's (normalized key, raw value), for the kinds gogo's
/// layers can also set.
fn collision_key(f: &Fix) -> Option<(String, String)> {
    match f {
        Fix::Env { key, value } => Some((format!("env {key}"), value.clone())),
        Fix::DllOverride { dll, mode } => Some((dll_key(dll), mode.clone())),
        Fix::Registry {
            key,
            value: Some(v),
            ..
        } => Some((reg_key(key, &v.name), v.data.clone())),
        _ => None,
    }
}

/// DLL names compare case-insensitively, without Wine's `*` (any path).
fn dll_key(dll: &str) -> String {
    format!(
        "dll {}",
        dll.trim_start_matches('*')
            .trim_end_matches(".dll")
            .to_lowercase()
    )
}

/// Registry keys compare case-insensitively, hives abbreviated.
fn reg_key(key: &str, name: &str) -> String {
    let k = key.to_lowercase();
    let k = [
        ("hkey_current_user", "hkcu"),
        ("hkey_local_machine", "hklm"),
        ("hkey_classes_root", "hkcr"),
        ("hkey_users", "hku"),
    ]
    .iter()
    .fold(k, |k, (long, short)| match k.strip_prefix(long) {
        Some(rest) => format!("{short}{rest}"),
        None => k,
    });
    format!("registry {k} {}", name.to_lowercase())
}

// — the verb pre-install (R12) —

/// `verbs` minus those `<prefix>/winetricks.log` lists. umu-run's
/// winetricks mode exits 1 without running anything when any verb it's
/// given is already in the log (umu 1.4.4, `is_installed_verb`), so setup
/// passes only the missing ones.
pub fn pending_verbs(prefix: &Path, verbs: &[String]) -> Vec<String> {
    let log = std::fs::read_to_string(prefix.join("winetricks.log")).unwrap_or_default();
    let installed: Vec<&str> = log.lines().map(str::trim).collect();
    verbs
        .iter()
        .filter(|v| !installed.contains(&v.as_str()))
        .cloned()
        .collect()
}

/// `umu-run winetricks <verbs>` against the game's prefix with the chosen
/// build's own winetricks (umu adds `-q`); `None` when there's nothing to
/// install. Setup runs it (#176/#177). `PROTONFIXES_DISABLE=1`: the
/// winetricks run is a `waitforexitandrun` Proton call, which would
/// otherwise run the game's protonfix too; that runs at launch.
pub fn winetricks_command(
    umu: &UmuRun,
    proton: &Path,
    prefix: &Path,
    umu_id: &str,
    verbs: &[String],
) -> Option<Command> {
    if verbs.is_empty() {
        return None;
    }
    let mut c = umu.command();
    c.arg("winetricks")
        .args(verbs)
        .env("WINEPREFIX", prefix)
        .env("PROTONPATH", proton)
        .env("GAMEID", umu_id)
        .env("STORE", "gog")
        .env(compat::PROTONFIXES_DISABLE, "1");
    Some(c)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::linuxapp::protonfix::read_source;

    fn module(name: &str, src: &str) -> Module {
        Module {
            name: name.into(),
            path: PathBuf::from(format!("/b/protonfixes/gamefixes-gog/{name}.py")),
            target: None,
            sha256: "0123456789abcdef".repeat(4),
            reading: read_source(src),
        }
    }

    // Synthesized, in the shape of umu-protonfixes' modules.
    fn default_py() -> Module {
        module(
            "default",
            "from protonfixes import util\ndef main() -> None:\n    util.set_environment('OPENSSL_ia32cap', '~0x20000000')\n",
        )
    }

    fn game_py() -> Module {
        module(
            "umu-910830",
            "from protonfixes import util\n\ndef main():\n    util.protontricks('d3dcompiler_47')\n    \
             util.set_environment('DXVK_ASYNC', '1')\n    util.winedll_override('*D3D8', util.OverrideOrder.NATIVE)\n",
        )
    }

    fn l4(raw: &str) -> CompatEntry {
        compat::parse(raw, Os::Linux, Layer::User).unwrap().entry
    }

    fn inputs(module: Option<Module>, db: Option<&str>, user: &str) -> Inputs<'static> {
        let auto = CompatEntry {
            verbs: Some(vec!["d3dx9".into()]),
            ..Default::default()
        };
        Inputs {
            umu_id: "umu-910830",
            module,
            default: Some(default_py()),
            db: db.map(|r| ("compat DB 1".into(), r.into())),
            layers: vec![(Layer::Auto, auto), (Layer::User, l4(user))],
        }
    }

    #[test]
    fn summary_lists_sources_and_collisions() {
        let user = "[platforms.linux]\nenv = { DXVK_ASYNC = \"0\", OTHER = \"x\" }\n\
                    dll_overrides = { d3d8 = \"b\" }\n";
        let f = resolve(inputs(Some(game_py()), None, user));
        let s = f.summary().to_string();
        assert_eq!(
            s,
            "protonfix umu-910830: /b/protonfixes/gamefixes-gog/umu-910830.py (sha256 0123456789ab)\n\
             verb d3dx9  (L1 auto)\n\
             dll_overrides d3d8: L4 \"b\" is overridden at launch by protonfix umu-910830 (\"n\")\n\
             env DXVK_ASYNC: L4 \"0\" is overridden at launch by protonfix umu-910830 (\"1\")\n\
             env OTHER = \"x\"  (L4 override)\n\
             env OPENSSL_ia32cap = \"~0x20000000\"  (protonfix default (every GOG game))\n\
             verb d3dcompiler_47  (protonfix umu-910830)\n\
             env DXVK_ASYNC = \"1\"  (protonfix umu-910830)\n\
             dll_overrides *D3D8 = \"n\"  (protonfix umu-910830)\n"
        );
        assert_eq!(f.setup_verbs(), ["d3dx9", "d3dcompiler_47"]);
        assert_eq!(f.protonfix_state(), "0123456789abcdef".repeat(4));
        // default.py collides too, and the same value is no collision.
        let user = "[platforms.linux]\nenv = { OPENSSL_ia32cap = \"0\", DXVK_ASYNC = \"1\" }\n";
        let s = resolve(inputs(Some(game_py()), None, user))
            .summary()
            .to_string();
        assert!(s.contains(
            "env OPENSSL_ia32cap: L4 \"0\" is overridden at launch by protonfix default (every GOG game) (\"~0x20000000\")\n"
        ));
        assert!(s.contains("env DXVK_ASYNC = \"1\"  (L4 override)\n"), "{s}");
    }

    #[test]
    fn no_module_no_fixes() {
        let mut i = inputs(None, None, "");
        i.layers.clear();
        i.default = None;
        let f = resolve(i);
        assert_eq!(
            f.summary().to_string(),
            "protonfix umu-910830: none in this Proton build\nno known fixes\n"
        );
        assert_eq!(f.protonfix_state(), "none");
        assert!(f.setup_verbs().is_empty());
    }

    #[test]
    fn other_code_is_listed() {
        let m = module(
            "umu-1",
            "from protonfixes import util\ndef main():\n    if x:\n        util.protontricks('y')\n",
        );
        let mut i = inputs(Some(m), None, "");
        i.umu_id = "umu-1";
        let s = resolve(i).summary().to_string();
        assert!(
            s.ends_with(
                "other code in protonfix umu-1 (may do more at launch):\n  line 3: if x:\n"
            ),
            "{s}"
        );
    }

    #[test]
    fn protonfixes_false_disables_and_lists_losses() {
        let user = "[platforms.linux]\nprotonfixes = false\nenv = { DXVK_ASYNC = \"0\" }\n";
        let f = resolve(inputs(Some(game_py()), None, user));
        assert_eq!(f.disabled_by(), Some(Layer::User));
        let env = &f.effective.env[compat::PROTONFIXES_DISABLE];
        assert_eq!((env.value.as_str(), env.layer), ("1", Layer::User));
        // The protonfix's verbs aren't pre-installed: it doesn't run.
        assert_eq!(f.setup_verbs(), ["d3dx9"]);
        assert_eq!(
            f.summary().to_string(),
            "protonfix umu-910830: /b/protonfixes/gamefixes-gog/umu-910830.py (sha256 0123456789ab)\n\
             verb d3dx9  (L1 auto)\n\
             env DXVK_ASYNC = \"0\"  (L4 override)\n\
             protonfixes = false (L4 override): PROTONFIXES_DISABLE=1, so the game loses:\n  \
             env OPENSSL_ia32cap = \"~0x20000000\"  (protonfix default (every GOG game))\n  \
             verb d3dcompiler_47  (protonfix umu-910830)\n  \
             env DXVK_ASYNC = \"1\"  (protonfix umu-910830)\n  \
             dll_overrides *D3D8 = \"n\"  (protonfix umu-910830)\n"
        );
        // A higher layer's `protonfixes = true` wins over L3's `false`.
        let db = "title = \"T\"\nsource = \"gogo-host\"\n[platforms.linux]\nprotonfixes = false\n";
        let f = resolve(inputs(
            Some(game_py()),
            Some(db),
            "[platforms.linux]\nprotonfixes = true\n",
        ));
        assert_eq!(f.disabled_by(), None);
        assert!(!f.effective.env.contains_key(compat::PROTONFIXES_DISABLE));
    }

    /// Stronghold HD's L3 file, as in `compat/1207658712.toml`.
    const STRONGHOLD: &str = "title  = \"Stronghold HD\"\nverbs  = [\"directplay\"]\n\
        source = \"umu-protonfixes:gamefixes-steam/40950.py@3603ccbbc20de88c2878306acdfdd4e9a07fc8c4\"\n";

    #[test]
    fn l3_from_protonfixes_applies_without_a_gog_module() {
        // Under STORE=gog protonfixes only looks in gamefixes-gog: no
        // module for Stronghold HD, so its L3 verb applies on Linux.
        let mut i = inputs(None, Some(STRONGHOLD), "");
        i.umu_id = "umu-40950";
        let f = resolve(i);
        assert_eq!(f.covered, None);
        assert_eq!(f.setup_verbs(), ["d3dx9", "directplay"]);
        assert!(
            f.summary()
                .to_string()
                .contains("verb directplay  (L3 compat DB)\n")
        );
        // The real file reads the same.
        let (_, raw) = compat::db_raw("1207658712").unwrap();
        let mut i = inputs(None, Some(&raw), "");
        i.umu_id = "umu-40950";
        assert_eq!(resolve(i).setup_verbs(), ["d3dx9", "directplay"]);
    }

    #[test]
    fn l3_from_protonfixes_is_covered_by_a_gog_module() {
        let m = module(
            "umu-40950",
            "from protonfixes import util\ndef main():\n    util.protontricks('directplay')\n",
        );
        let db = format!(
            "{STRONGHOLD}[platforms.linux]\nnotes = \"seen on Linux\"\nverbs = [\"xact\"]\n"
        );
        let mut i = inputs(Some(m), Some(&db), "");
        i.umu_id = "umu-40950";
        let f = resolve(i);
        let covered = f.covered.as_ref().unwrap();
        assert_eq!(covered.verbs, Some(vec!["directplay".into()]));
        // The platform table still applies; the top-level verb doesn't.
        assert_eq!(f.effective.notes.as_ref().unwrap().value, "seen on Linux");
        assert_eq!(f.effective.title.as_ref().unwrap().value, "Stronghold HD");
        let verbs: Vec<_> = f.effective.verbs.iter().map(|v| v.value.as_str()).collect();
        assert_eq!(verbs, ["d3dx9", "xact"]);
        // Installed once, by the protonfix's literal verb.
        assert_eq!(f.setup_verbs(), ["d3dx9", "xact", "directplay"]);
        let s = f.summary().to_string();
        assert!(
            s.contains(
                "verb directplay  (L3 compat DB: covered by protonfix umu-40950, not applied)\n"
            ),
            "{s}"
        );
        assert!(
            s.contains("verb directplay  (protonfix umu-40950)\n"),
            "{s}"
        );

        // Another source keeps its fixes, module or not.
        let other = STRONGHOLD.replace(
            "umu-protonfixes:gamefixes-steam/40950.py@3603ccbbc20de88c2878306acdfdd4e9a07fc8c4",
            "gogo-host",
        );
        let mut i = inputs(Some(module("umu-40950", "")), Some(&other), "");
        i.umu_id = "umu-40950";
        assert!(resolve(i).covered.is_none());

        // With protonfixes off, nothing owns the game but gogo: L3 applies.
        let mut i = inputs(
            Some(module("umu-40950", "")),
            Some(STRONGHOLD),
            "[platforms.linux]\nprotonfixes = false\n",
        );
        i.umu_id = "umu-40950";
        let f = resolve(i);
        assert!(f.covered.is_none());
        assert!(f.setup_verbs().contains(&"directplay".to_string()));
    }

    #[test]
    fn mac_lane_is_unchanged() {
        // The same files resolved for macOS: `protonfixes` is dropped, so no
        // PROTONFIXES_DISABLE, and L3 applies whole (the Mac lane merges
        // through compat::merge directly, never through `resolve`).
        let db = format!("{STRONGHOLD}[platforms.linux]\nprotonfixes = false\n");
        let l3 = compat::parse(&db, Os::Macos, Layer::Db).unwrap().entry;
        let l4 = compat::parse(
            "[platforms.linux]\nprotonfixes = false\n",
            Os::Macos,
            Layer::User,
        )
        .unwrap()
        .entry;
        let eff = compat::merge(&[(Layer::Db, &l3), (Layer::User, &l4)]);
        assert_eq!(eff.protonfixes, None);
        assert!(eff.env.is_empty());
        assert_eq!(eff.verbs[0].value, "directplay");
    }

    #[test]
    fn winetricks_skips_installed_verbs() {
        let t = tempfile::tempdir().unwrap();
        let verbs: Vec<String> = ["d3dx9", "xact", "directplay"].map(String::from).into();
        assert_eq!(pending_verbs(t.path(), &verbs), verbs);
        std::fs::write(t.path().join("winetricks.log"), "d3dx9\nwin10\n xact \n").unwrap();
        let pending = pending_verbs(t.path(), &verbs);
        assert_eq!(pending, ["directplay"]);

        let umu = UmuRun::Zipapp {
            python: "/py/python3".into(),
            zipapp: "/store/umu/umu-run".into(),
        };
        let proton = Path::new("/store/GE-Proton11-12");
        let c = winetricks_command(&umu, proton, t.path(), "umu-40950", &pending).unwrap();
        assert_eq!(c.get_program(), "/py/python3");
        let args: Vec<_> = c.get_args().collect();
        assert_eq!(args, ["/store/umu/umu-run", "winetricks", "directplay"]);
        let env: std::collections::BTreeMap<_, _> = c
            .get_envs()
            .map(|(k, v)| (k.to_owned(), v.unwrap().to_owned()))
            .collect();
        assert_eq!(env[std::ffi::OsStr::new("GAMEID")], "umu-40950");
        assert_eq!(env[std::ffi::OsStr::new("STORE")], "gog");
        assert_eq!(env[std::ffi::OsStr::new("PROTONPATH")], proton.as_os_str());
        assert_eq!(
            env[std::ffi::OsStr::new("WINEPREFIX")],
            t.path().as_os_str()
        );
        assert!(winetricks_command(&umu, proton, t.path(), "umu-1", &[]).is_none());
    }
}
