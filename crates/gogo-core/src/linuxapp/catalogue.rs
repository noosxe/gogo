//! Which env vars and switches exist for a game's Proton build (#233, design
//! 18 §7.7): `gogo linux-app env list <gogid> --all` and `env describe`.
//!
//! [`envargs::env_report`] says what's active; this adds what exists. Names
//! come from the build itself where it can say ([`scan`]), words from gogo's
//! table ([`table`]), and each entry says which is which, whether it does
//! anything for this game's renderer and GPU, and what sets it now. Nothing
//! is hidden for not applying, only marked. Reads only; no network.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::Serialize;

pub mod scan;
pub mod table;

use super::doctor::Host;
use super::envargs::{self, EnvLine, EnvReport};
use super::game::{ConfValues, LinuxState};
use super::proton::{self, ProtonRef};
use super::{manage, setup};
use crate::macapp::compat::{self, Os};
use crate::macapp::error::MacAppError;
use crate::macapp::launch::GameDirs;
use crate::macapp::renderer::{Renderer, RendererRef};
use scan::BuildFacts;
use table::{Component, Kind, PLUMBING, REVIEWED_FOR, Spec, TABLE, Topic};

/// Where gogo knows an entry from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    /// The build's `proton` script, with its `STEAM_COMPAT_CONFIG` token.
    ProtonScript { token: Option<String> },
    /// The build's `protonfixes` helpers.
    Protonfixes,
    /// The build's `user_settings.sample.py`.
    SampleSettings,
    /// A string in one of the build's DLLs or modules.
    Dll { file: String },
    /// gogo's table.
    Table,
}

impl Source {
    /// The word `env list --all` prints.
    pub fn tag(&self) -> &'static str {
        match self {
            Source::ProtonScript { .. } => "script",
            Source::Protonfixes => "fixes",
            Source::SampleSettings => "sample",
            Source::Dll { .. } => "dll",
            Source::Table => "table",
        }
    }
}

/// Does the game's build have it?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InBuild {
    Found,
    /// No trace in what was read.
    NotFound,
    /// The files that could say weren't there, or the build wasn't found.
    NotChecked,
    /// A host tool: nothing about the build can say.
    NotOfBuild,
}

/// Does it do anything for this game on this machine?
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Applies {
    Yes,
    No {
        why: String,
    },
    /// gogo can't tell.
    Unknown {
        why: String,
    },
}

/// One switch.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub name: String,
    pub component: Component,
    pub kind: Kind,
    /// `None`: gogo's table doesn't have it.
    pub topic: Option<Topic>,
    /// One line, from gogo's table.
    pub about: Option<String>,
    pub default: Option<String>,
    /// `table` or `proton script`, when there's a default.
    pub default_from: Option<&'static str>,
    pub link: Option<&'static str>,
    pub sources: Vec<Source>,
    pub in_build: InBuild,
    /// The build's script also sets it, at least in some configurations.
    pub proton_sets: bool,
    pub applies: Applies,
    /// What sets it for this game, the winner first. For a flag or a
    /// `dxvk.conf` key: the `VKD3D_CONFIG` / `DXVK_CONFIG` line holding it.
    pub active: Vec<EnvLine>,
}

impl Entry {
    /// The one thing to type to set it, if a user can.
    pub fn set_with(&self, gogid: &str) -> Result<String, String> {
        let set = |pair: String| format!("gogo linux-app env set {gogid} {pair}");
        match self.kind {
            Kind::Vkd3dFlag => Ok(format!(
                "{} (a comma-separated list: keep flags already there)",
                set(format!("VKD3D_CONFIG={}", self.name))
            )),
            Kind::DxvkConf => Ok(format!(
                "{} (`;` between keys: keep keys already there)",
                set(format!("DXVK_CONFIG='{}=<value>'", self.name))
            )),
            Kind::Command => Ok(format!(
                "gogo linux-app wrapper {gogid} -- {}{} (it replaces any wrapper set before)",
                self.name,
                if self.name == "gamescope" {
                    " <options> --"
                } else {
                    ""
                }
            )),
            Kind::Env => match envargs::policy(&self.name) {
                envargs::Policy::Refuse(why) => Err(why),
                _ => Ok(set(format!("{}=<value>", self.name))),
            },
        }
    }
}

/// The build a report describes.
#[derive(Debug, Clone, Serialize)]
pub struct BuildSummary {
    pub dir: PathBuf,
    pub version: Option<String>,
    pub components: BTreeMap<String, String>,
}

/// `gogo linux-app env list <gogid> --all`.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogueReport {
    pub gogid: String,
    pub user_file: PathBuf,
    pub built: bool,
    pub build: Option<BuildSummary>,
    /// The build gogo's table was checked against.
    pub reviewed_for: &'static str,
    /// The renderer this game runs on, once known.
    pub renderer: Option<String>,
    /// GPU vendors the kernel reports.
    pub gpus: Vec<String>,
    /// In the order to show them: component, env vars before flags, topic.
    pub entries: Vec<Entry>,
    pub warnings: Vec<String>,
}

impl CatalogueReport {
    /// An entry by name, ignoring case.
    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name).or_else(|| {
            self.entries
                .iter()
                .find(|e| e.name.eq_ignore_ascii_case(name))
        })
    }

    /// Names holding `text`, ignoring case: what to offer after a miss.
    pub fn similar(&self, text: &str) -> Vec<&str> {
        let t = text.to_ascii_lowercase();
        self.entries
            .iter()
            .filter(|e| e.name.to_ascii_lowercase().contains(&t))
            .map(|e| e.name.as_str())
            .collect()
    }

    /// Entries the build has and the table doesn't.
    pub fn undescribed(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.about.is_none() && e.component != Component::Plumbing)
            .filter(|e| e.in_build == InBuild::Found)
            .count()
    }
}

/// What applicability depends on, apart from the entry.
pub struct Machine<'a> {
    pub renderer: Option<Renderer>,
    /// As [`super::doctor`] labels them: `NVIDIA`, `AMD`, `Intel`, `0x…`.
    pub gpus: Vec<String>,
    pub on_path: &'a dyn Fn(&str) -> bool,
}

fn list(items: impl IntoIterator<Item = String>) -> String {
    items.into_iter().collect::<Vec<_>>().join(", ")
}

/// Whether `spec` does anything here. Order: the build, the host tool, then
/// the renderer and the GPU.
fn applies(spec: &Spec, in_build: InBuild, m: &Machine) -> Applies {
    if in_build == InBuild::NotFound {
        return Applies::No {
            why: "not in this build".into(),
        };
    }
    if let Some(cmd) = spec.needs
        && !(m.on_path)(cmd)
    {
        return Applies::No {
            why: format!("`{cmd}` isn't on PATH"),
        };
    }
    if !spec.renderers.is_empty() {
        let wanted = || list(spec.renderers.iter().map(|r| r.id().to_string()));
        match m.renderer {
            None => {
                return Applies::Unknown {
                    why: format!("for {}; this game has no renderer yet", wanted()),
                };
            }
            Some(r) if !spec.renderers.contains(&r) => {
                return Applies::No {
                    why: format!("for {}; this game's renderer is {}", wanted(), r.id()),
                };
            }
            Some(_) => {}
        }
    }
    if !spec.gpus.is_empty() {
        let wanted = || list(spec.gpus.iter().map(|v| v.label().to_string()));
        if m.gpus.is_empty() {
            return Applies::Unknown {
                why: format!("for {}; gogo sees no GPU", wanted()),
            };
        }
        if !spec
            .gpus
            .iter()
            .any(|v| m.gpus.iter().any(|g| g == v.label()))
        {
            return Applies::No {
                why: format!("for {}; this machine has {}", wanted(), m.gpus.join(", ")),
            };
        }
    }
    Applies::Yes
}

/// The names the table wants looked up in the build's files.
fn wanted_names() -> Vec<&'static str> {
    TABLE
        .iter()
        .filter(|s| !matches!(s.kind, Kind::Vkd3dFlag | Kind::Command))
        .map(|s| s.probe.unwrap_or(s.name))
        .collect()
}

/// The flags to look for: a linker may merge `dxr` into `nodxr`.
fn flag_names() -> Vec<&'static str> {
    TABLE
        .iter()
        .filter(|s| s.kind == Kind::Vkd3dFlag)
        .map(|s| s.name)
        .collect()
}

/// Read a build the way a report does.
pub fn read_build(dir: &std::path::Path) -> BuildFacts {
    BuildFacts::read(dir, &wanted_names(), &flag_names())
}

/// The files that can say anything about `component`.
fn checked(component: Component, facts: &BuildFacts) -> bool {
    let scanned = |c: &str| facts.scanned.contains(c);
    match component {
        Component::Proton | Component::Plumbing | Component::Other => facts.script_read,
        Component::DxvkNvapi => scanned("dxvk-nvapi"),
        Component::Vkd3dProton => scanned("vkd3d-proton"),
        Component::Dxvk => scanned("dxvk"),
        Component::D7vk => scanned("d7vk"),
        Component::Wine => scanned("wine") || facts.script_read,
        Component::Wrappers => false,
    }
}

/// The sources and the state of one name in `facts`.
fn evidence(
    name: &str,
    probe: Option<&str>,
    component: Component,
    facts: Option<&BuildFacts>,
) -> (Vec<Source>, InBuild, bool) {
    if component == Component::Wrappers {
        return (vec![], InBuild::NotOfBuild, false);
    }
    let Some(f) = facts else {
        return (vec![], InBuild::NotChecked, false);
    };
    let mut sources = Vec::new();
    let mut sets = false;
    if let Some(u) = f.script.get(name) {
        if u.reads || u.sets {
            sources.push(Source::ProtonScript {
                token: u.token.clone(),
            });
        }
        if u.fixes_reads {
            sources.push(Source::Protonfixes);
        }
        sets = u.sets;
    }
    if f.sample.contains(name) {
        sources.push(Source::SampleSettings);
    }
    if let Some(files) = f.found.get(probe.unwrap_or(name)) {
        sources.extend(files.iter().map(|file| Source::Dll { file: file.clone() }));
    }
    let state = if !sources.is_empty() {
        InBuild::Found
    } else if checked(component, f) {
        InBuild::NotFound
    } else {
        InBuild::NotChecked
    };
    (sources, state, sets)
}

/// `value` of a `VKD3D_CONFIG` line holds the flag `name`.
fn has_flag(value: &str, name: &str) -> bool {
    value.split(',').any(|w| w.trim() == name)
}

/// `value` of a `DXVK_CONFIG` line sets the key `name`.
fn has_conf_key(value: &str, name: &str) -> bool {
    value
        .split(';')
        .any(|kv| kv.split('=').next().is_some_and(|k| k.trim() == name))
}

/// Which lines of the active env speak for an entry.
fn active_for(name: &str, kind: Kind, env: &EnvReport) -> Vec<EnvLine> {
    let holder = match kind {
        Kind::Env => name,
        Kind::Vkd3dFlag => "VKD3D_CONFIG",
        Kind::DxvkConf => "DXVK_CONFIG",
        Kind::Command => {
            let Some(w) = env
                .wrapper
                .as_ref()
                .filter(|w| envargs::wrapper_runs(&w.words, name))
            else {
                return vec![];
            };
            let Some(layer) = w.layer else {
                return vec![];
            };
            return vec![EnvLine {
                key: "wrapper".into(),
                value: envargs::shell_words(&w.words),
                source: envargs::EnvSource::Layer { layer },
                state: if w.pending {
                    envargs::EnvState::Pending
                } else {
                    envargs::EnvState::Applies
                },
            }];
        }
    };
    env.lines
        .iter()
        .filter(|l| l.key == holder)
        .filter(|l| match kind {
            Kind::Vkd3dFlag => has_flag(&l.value, name),
            Kind::DxvkConf => has_conf_key(&l.value, name),
            _ => true,
        })
        .cloned()
        .collect()
}

/// Not switches: named so nothing is hidden, not described.
fn is_plumbing(name: &str) -> bool {
    PLUMBING.contains(&name) || envargs::LAUNCHER_KEYS.contains(&name) || name == "WINEDLLOVERRIDES"
}

/// A name with no words: read by the build, or set for this game.
fn undescribed(name: &str, env: &EnvReport, facts: Option<&BuildFacts>, proton: bool) -> Entry {
    let component = if is_plumbing(name) {
        Component::Plumbing
    } else if proton {
        Component::guess(name)
    } else {
        Component::Other
    };
    let (sources, in_build, proton_sets) = evidence(name, None, component, facts);
    let default = facts
        .and_then(|f| f.script.get(name))
        .and_then(|u| u.default.clone());
    Entry {
        name: name.to_string(),
        component,
        kind: Kind::Env,
        topic: None,
        about: None,
        default_from: default.is_some().then_some("proton script"),
        default,
        link: component.link(),
        sources,
        in_build,
        proton_sets,
        applies: match component {
            Component::Plumbing => Applies::Unknown {
                why: "not a switch: Steam's, umu's or the build's own".into(),
            },
            Component::Other if !env.lines.iter().any(|l| l.key == name) => Applies::Unknown {
                why: "not in gogo's table".into(),
            },
            Component::Other => Applies::Unknown {
                why: "set for this game; neither the build nor gogo's table knows it".into(),
            },
            _ => Applies::Unknown {
                why: "the build has it; gogo's table doesn't".into(),
            },
        },
        active: active_for(name, Kind::Env, env),
    }
}

/// The catalogue from its parts: every table entry, every name the build
/// has, every key active for the game. Pure, so tests can feed it.
pub fn merge(
    env: EnvReport,
    facts: Option<&BuildFacts>,
    renderer: Option<String>,
    machine: &Machine,
) -> CatalogueReport {
    let mut entries: Vec<Entry> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for s in TABLE {
        let (mut sources, in_build, proton_sets) = evidence(s.name, s.probe, s.component, facts);
        sources.push(Source::Table);
        let script_default = facts
            .and_then(|f| f.script.get(s.name))
            .and_then(|u| u.default.clone());
        let (default, default_from) = if !s.default.is_empty() {
            (Some(s.default.to_string()), Some("table"))
        } else {
            let from = script_default.is_some().then_some("proton script");
            (script_default, from)
        };
        seen.insert(s.name.to_string());
        entries.push(Entry {
            name: s.name.to_string(),
            component: s.component,
            kind: s.kind,
            topic: Some(s.topic),
            about: Some(s.about.to_string()),
            default,
            default_from,
            link: s.component.link(),
            sources,
            in_build,
            proton_sets,
            applies: applies(s, in_build, machine),
            active: active_for(s.name, s.kind, &env),
        });
    }
    for name in facts.map(BuildFacts::switch_names).unwrap_or_default() {
        if seen.insert(name.clone()) {
            entries.push(undescribed(&name, &env, facts, true));
        }
    }
    for l in &env.lines {
        if seen.insert(l.key.clone()) {
            entries.push(undescribed(&l.key, &env, facts, false));
        }
    }
    entries.sort_by(|a, b| {
        let key = |e: &Entry| {
            (
                e.component,
                e.kind != Kind::Env,
                e.topic.is_none(),
                e.topic,
                e.name.clone(),
            )
        };
        key(a).cmp(&key(b))
    });

    let mut warnings = env.warnings.clone();
    match facts.map(|f| f.version.as_deref()) {
        None => warnings.push(
            "this game's Proton build wasn't found, so nothing is checked against it: \
             `gogo linux-app setup <gogid>` installs it"
                .into(),
        ),
        Some(v) if v != Some(REVIEWED_FOR) => warnings.push(format!(
            "gogo's descriptions were reviewed against {REVIEWED_FOR}, this game runs {}: \
             which names exist is this build's, but the words may lag behind it",
            v.unwrap_or("a build with no version file")
        )),
        Some(_) => {}
    }
    if facts.is_some_and(|f| !f.script_read) {
        warnings.push("this build's `proton` script couldn't be read".into());
    }
    CatalogueReport {
        gogid: env.gogid,
        user_file: env.user_file,
        built: env.built,
        build: facts.map(|f| BuildSummary {
            dir: f.dir.clone(),
            version: f.version.clone(),
            components: f.components.clone(),
        }),
        reviewed_for: REVIEWED_FOR,
        renderer,
        gpus: machine.gpus.clone(),
        entries,
        warnings,
    }
}

/// The Proton build the game runs on, as the launcher would find it: what
/// `launch.conf` says, else the reference through gogo's store. Reads only;
/// a build that isn't there is `None`, not an error.
fn locate_build(gogid: &str, dirs: &GameDirs, conf: Option<&ConfValues>) -> Option<PathBuf> {
    if let Some(c) = conf {
        let d = c.get("PROTON_DIR");
        if !d.is_empty() {
            let p = std::path::Path::new(d);
            let abs = if p.is_absolute() {
                p.to_path_buf()
            } else {
                let root = PathBuf::from(c.get("DATA_ROOT"));
                let root = if root.is_absolute() {
                    root
                } else {
                    crate::state::data_root()
                };
                root.join(p)
            };
            if abs.join("proton").is_file() {
                return Some(abs);
            }
        }
    }
    let reference = LinuxState::load(&dirs.state_toml)
        .ok()
        .flatten()
        .map(|s| s.runtime)
        .or_else(|| {
            compat::load_user_for(gogid, Os::Linux)
                .ok()
                .and_then(|l| l.entry.runtime)
        });
    let parsed = match reference {
        Some(r) => ProtonRef::parse(&r).ok(),
        None => {
            let stored = super::stored_key(&crate::config::Config::path(), "default_runtime");
            proton::default_ref(stored.as_deref()).ok()
        }
    }?;
    setup::find_proton(&parsed).ok().flatten().map(|b| b.dir)
}

/// `gogo linux-app env list <gogid> --all`. Reads only.
pub fn report(gogid: &str, host: &Host, reveal: bool) -> Result<CatalogueReport, MacAppError> {
    let env = envargs::env_report(gogid, reveal)?;
    let dirs = GameDirs::for_game(gogid)?;
    let conf = ConfValues::read(&dirs.launch_conf).ok();
    let facts = locate_build(gogid, &dirs, conf.as_ref()).map(|d| read_build(&d));
    let renderer = conf
        .as_ref()
        .map(|c| c.get("RENDERER").to_string())
        .filter(|r| !r.is_empty())
        .or_else(|| {
            manage::compat(gogid)
                .ok()
                .and_then(|c| c.renderer.map(|r| r.effective))
        });
    let parsed = renderer
        .as_deref()
        .and_then(|r| RendererRef::parse_on(r, Os::Linux).ok())
        .map(|r| r.renderer);
    let mut gpus: Vec<String> = super::doctor::gpus(host)
        .into_iter()
        .map(|g| g.vendor)
        .collect();
    gpus.sort();
    gpus.dedup();
    let machine = Machine {
        renderer: parsed,
        gpus,
        on_path: &|c| host.finds_program(c),
    };
    Ok(merge(env, facts.as_ref(), renderer, &machine))
}

#[cfg(test)]
mod tests;
