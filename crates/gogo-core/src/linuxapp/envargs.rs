//! Per-game env vars, launch args and wrapper on the Linux lane (#234, #271,
//! design 18 §7.6, §7.8): `gogo linux-app env list|set|unset`,
//! `gogo linux-app args` and `gogo linux-app wrapper`.
//!
//! All live in the game's L4 file under `[platforms.linux]`: `env` (a
//! table, the key the schema always had), `extra_args` (appended after
//! the game's own args; `args` replaces those) and `wrapper` (the command
//! the game runs under). Nothing here runs setup: the launch stamp covers
//! the L4 file, so the next launch sees it stale, runs setup, and
//! `launch.conf` gets the new `ENV` / `ARGS` / `WRAPPER`.
//!
//! What wins at launch, lowest first (the launcher's `run_game`):
//! the environment `launch.sh` starts with; gogo's default
//! `PROTON_SET_GAME_DRIVE=0`; `launch.conf`'s `ENV` (the renderer's
//! switches fill gaps only, so L3/L4 win over them); `WINEDLLOVERRIDES`
//! from `DLL_OVERRIDES`; then `WINEPREFIX`, `GAMEID`, `STORE`,
//! `PROTONPATH`, which are the launcher's. Inside umu, protonfixes sets its
//! env last and wins over all of gogo's (design 18 §4.3, O2).
//!
//! Values are the user's and are shown, except for keys that look like
//! secrets ([`is_secret`]): those print as `<hidden>` unless asked for.
//! gogo never logs env values (the run log has no env line).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use super::fixes::{FixSource, State};
use super::game::{self, ConfValues};
use super::manage;
use super::setup;
use crate::macapp::compat::{self, Change, CompatEntry, Effective, Layer, Os, Sourced, UserEdit};
use crate::macapp::error::MacAppError;
use crate::macapp::launch::{GameDirs, sh_quote};
use crate::macapp::renderer;

/// Set by the launcher after `ENV`: an L4 value would never apply.
pub const LAUNCHER_KEYS: &[&str] = &["WINEPREFIX", "GAMEID", "STORE", "PROTONPATH"];

/// Shown in place of a value [`is_secret`] hides.
pub use crate::macapp::compat::HIDDEN;

/// Is `key` a valid env var name for the launcher (`export "K=V"` in
/// bash): ASCII letters, digits and `_`, not starting with a digit.
pub fn check_key(key: &str) -> Result<(), String> {
    let mut bytes = key.bytes();
    match bytes.next() {
        None => Err("an empty name".into()),
        Some(b) if !(b.is_ascii_alphabetic() || b == b'_') => {
            Err("a name starts with a letter or `_`".into())
        }
        _ if !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_') => {
            Err("a name has only letters, digits and `_`".into())
        }
        _ => Ok(()),
    }
}

/// A value or an arg the launcher can carry: no NUL (no process can take
/// one) and no newline (`launch.conf` stays one assignment per line).
pub fn check_value(what: &str, v: &str) -> Result<(), String> {
    if v.contains('\0') {
        Err(format!("{what} has a NUL byte"))
    } else if v.contains('\n') || v.contains('\r') {
        Err(format!("{what} has a line break"))
    } else {
        Ok(())
    }
}

/// The word of a wrapper that is the program to run.
pub fn wrapper_program(words: &[String]) -> Option<&str> {
    words.first().map(String::as_str)
}

/// A word's program name: `/x/gamescope` and `gamescope` are both `gamescope`.
fn base_name(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// Does the wrapper run `name` anywhere in its chain? (`gamemoderun gamescope
/// -f --` runs both; a word after `--` isn't gamescope's own option.)
pub fn wrapper_runs(words: &[String], name: &str) -> bool {
    words.iter().any(|w| base_name(w) == name)
}

/// Can the launcher run `words` as a wrapper (design 18 §7.8)? Argv form
/// only: a program, a bare name for `PATH` or an absolute path, and its
/// words. The warnings are for what's allowed but likely wrong.
pub fn check_wrapper(words: &[String]) -> Result<Vec<String>, String> {
    let Some(program) = wrapper_program(words) else {
        return Err("a wrapper has at least a program (`--clear` drops it)".into());
    };
    for w in words {
        check_value(&format!("wrapper word {w:?}"), w)?;
        if w.contains("%command%") {
            return Err(format!(
                "{w:?} is Steam's: gogo puts the game's command after the wrapper's last word"
            ));
        }
    }
    if program.is_empty() {
        return Err("the wrapper's program is empty".into());
    }
    if program.starts_with('-') {
        return Err(format!(
            "the wrapper's first word is its program, and {program:?} looks like an option"
        ));
    }
    if !program.starts_with('/') {
        if program.contains('/') {
            return Err(format!(
                "the wrapper's program {program:?} is relative: a bare name for PATH or an \
                 absolute path"
            ));
        }
        if program.contains(char::is_whitespace) {
            return Err(format!(
                "{program:?} is a shell string: a wrapper is one word per argument, not run \
                 through a shell"
            ));
        }
        if program.contains('=') {
            return Err(format!(
                "{program:?} is a shell assignment: use `env {program} …`, or `linux-app env \
                 set` for what the game should see"
            ));
        }
    }
    let mut warnings = Vec::new();
    if let Some(at) = words.iter().position(|w| base_name(w) == "gamescope")
        && !words[at..].iter().any(|w| w == "--")
    {
        warnings.push(
            "gamescope's own options end at `--`, and the game's command comes after this \
             wrapper's last word: this usually ends in `--`"
                .to_string(),
        );
    }
    Ok(warnings)
}

/// What gogo does with a key the user sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Policy {
    /// The user's alone.
    Free,
    /// Written; the user's value wins over gogo's, and this says over what.
    Warn(String),
    /// Not written: it would never apply, or has its own setting.
    Refuse(String),
}

/// Which keys gogo owns (#234). [`LAUNCHER_KEYS`] and the two with their
/// own setting are refused; the renderer's switches and gogo's
/// `PROTON_SET_GAME_DRIVE=0` are the user's to override, with a warning.
pub fn policy(key: &str) -> Policy {
    if LAUNCHER_KEYS.contains(&key) {
        return Policy::Refuse(format!(
            "{key} is the launcher's: it sets it after the game's env, so yours would never apply"
        ));
    }
    match key {
        "WINEDLLOVERRIDES" => Policy::Refuse(
            "WINEDLLOVERRIDES is built from the renderer's and the `dll_overrides` table: \
             put `<dll> = \"n,b\"` under [platforms.linux.dll_overrides] in the override file"
                .into(),
        ),
        compat::PROTONFIXES_DISABLE => Policy::Refuse(
            "PROTONFIXES_DISABLE comes from `protonfixes = false` under [platforms.linux] in \
             the override file (then the fix summary knows what the game loses)"
                .into(),
        ),
        _ if key == setup::GAME_DRIVE_OFF.0 => Policy::Warn(format!(
            "yours wins over gogo's default {}={} (host drives like x: -> $HOME come back with 1)",
            setup::GAME_DRIVE_OFF.0,
            setup::GAME_DRIVE_OFF.1
        )),
        renderer::PROTON_USE_D7VK | renderer::PROTON_USE_WINED3D | renderer::PROTON_DXVK_D3D8 => {
            Policy::Warn(format!(
                "{key} is a renderer switch: yours wins over the renderer pick's \
                 (`gogo linux-app --retarget <gogid> --renderer <id>` picks a renderer)"
            ))
        }
        _ => Policy::Free,
    }
}

pub use crate::macapp::compat::is_secret;

/// `value`, or [`HIDDEN`] for a secret-looking `key` unless `reveal`.
pub fn shown(key: &str, value: &str, reveal: bool) -> String {
    if !reveal && is_secret(key) {
        HIDDEN.to_string()
    } else {
        value.to_string()
    }
}

/// The args the exe gets: the game's (`args` as resolved) then
/// `extra_args`. What setup writes as `launch.conf`'s `ARGS`; the
/// launcher adds its own command-line args after these.
pub fn launch_args(game_args: &[String], eff: &Effective) -> Vec<String> {
    let mut out = game_args.to_vec();
    if let Some(x) = &eff.extra_args {
        out.extend(x.value.iter().cloned());
    }
    out
}

/// A command line for humans: each word `sh_quote`d unless it's plain.
pub fn shell_words(words: &[String]) -> String {
    words
        .iter()
        .map(|w| {
            let plain = !w.is_empty()
                && w.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_=./:,+@%".contains(&b));
            if plain { w.clone() } else { sh_quote(w) }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// — the report —

/// Where an env value comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnvSource {
    /// gogo's default (`PROTON_SET_GAME_DRIVE=0`); any layer wins over it.
    GogoDefault,
    /// `gogo-launch.toml`'s `env`, baked at build.
    Baked,
    /// `launch.conf` has it and no layer does: the renderer's switches
    /// (setup's last run).
    Renderer { renderer: String },
    /// As [`EnvSource::Renderer`], but the override changed since that
    /// setup: the value may be one since removed, and the next launch's
    /// setup decides.
    LastSetup,
    /// A compat layer: L3, L4 (or `protonfixes = false` at that layer).
    Layer { layer: Layer },
    /// `WINEDLLOVERRIDES`, from `DLL_OVERRIDES`.
    DllOverrides,
    /// Set by the launcher last ([`LAUNCHER_KEYS`]).
    Launcher,
    /// Set by protonfixes inside umu, after everything gogo sets.
    Protonfix { from: String },
}

impl EnvSource {
    /// Who wins: higher is later at launch.
    fn rank(&self) -> u8 {
        match self {
            EnvSource::GogoDefault => 0,
            EnvSource::Renderer { .. } | EnvSource::LastSetup => 1,
            EnvSource::Baked => 2,
            EnvSource::Layer { layer } => 3 + *layer as u8,
            EnvSource::DllOverrides => 20,
            EnvSource::Launcher => 21,
            EnvSource::Protonfix { .. } => 22,
        }
    }
}

impl std::fmt::Display for EnvSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvSource::GogoDefault => f.write_str("gogo default"),
            EnvSource::Baked => f.write_str("gogo-launch.toml"),
            EnvSource::Renderer { renderer } => write!(f, "renderer {renderer}"),
            EnvSource::LastSetup => f.write_str("last setup; the next one may drop it"),
            EnvSource::Layer { layer } => f.write_str(layer.describe()),
            EnvSource::DllOverrides => f.write_str("gogo: dll_overrides + renderer"),
            EnvSource::Launcher => f.write_str("launcher, always"),
            EnvSource::Protonfix { from } => write!(f, "{from}, at launch"),
        }
    }
}

/// Whether a value takes effect at the next launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EnvState {
    /// It's what the game gets.
    Applies,
    /// It will be: `launch.conf` doesn't have it yet, and the next launch
    /// runs setup first (the override changed).
    Pending,
    /// Another source sets the key later.
    Overridden { by: String },
}

/// One value of one key, from one source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvLine {
    pub key: String,
    /// [`HIDDEN`] for a secret-looking key unless revealed.
    pub value: String,
    pub source: EnvSource,
    #[serde(flatten)]
    pub state: EnvState,
}

/// The command the game runs under, set and as last applied (#271).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WrapperView {
    /// What the next launch's setup writes; empty: none. A hand-written
    /// wrapper [`check_wrapper`] refuses isn't here (setup skips it).
    pub words: Vec<String>,
    /// The layer `words` come from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<Layer>,
    /// `launch.conf`'s `WRAPPER`: what the last setup wrote.
    pub applied: Vec<String>,
    /// `words` isn't `applied` yet: the next launch's setup writes it.
    pub pending: bool,
}

/// [`WrapperView`] and why a hand-written one doesn't count; `None` when no
/// layer sets one and `launch.conf` has none.
fn wrapper_view(eff: &Effective, conf: Option<&ConfValues>) -> (Option<WrapperView>, Vec<String>) {
    let mut warnings = Vec::new();
    let set = eff
        .wrapper
        .as_ref()
        .filter(|w| match check_wrapper(&w.value) {
            Ok(_) => true,
            Err(why) => {
                warnings.push(format!(
                    "wrapper {:?} in the {}: {why}; setup skips it",
                    w.value,
                    w.layer.describe()
                ));
                false
            }
        });
    let applied = conf
        .and_then(|c| c.arrays.get("WRAPPER").cloned())
        .unwrap_or_default();
    if set.is_none() && applied.is_empty() {
        return (None, warnings);
    }
    let words = set.map(|w| w.value.clone()).unwrap_or_default();
    let view = WrapperView {
        pending: conf.is_some() && words != applied,
        layer: set.map(|w| w.layer),
        words,
        applied,
    };
    (Some(view), warnings)
}

/// `gogo linux-app env list <gogid>`.
#[derive(Debug, Clone, Serialize)]
pub struct EnvReport {
    pub gogid: String,
    pub user_file: PathBuf,
    pub built: bool,
    /// By key; for each key the one that applies first, then the ones it
    /// beats.
    pub lines: Vec<EnvLine>,
    /// The command the game runs under, when there is one. Not an env var,
    /// but "what's active" asks about it too.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrapper: Option<WrapperView>,
    pub warnings: Vec<String>,
}

impl EnvReport {
    /// The lines of keys the L4 file sets.
    pub fn user_lines(&self) -> impl Iterator<Item = &EnvLine> {
        self.lines
            .iter()
            .filter(|l| l.source == EnvSource::Layer { layer: Layer::User })
    }
}

/// The game's env at its next launch, and where each value comes from.
/// Reads only. An unbuilt game shows L3 and L4.
pub fn env_report(gogid: &str, reveal: bool) -> Result<EnvReport, MacAppError> {
    let r = manage::compat(gogid)?;
    let dirs = GameDirs::for_game(gogid)?;
    let conf = ConfValues::read(&dirs.launch_conf).ok();
    let baked = game::read_launch_toml(&dirs).ok().map(|(t, _)| t);
    let stale = conf.as_ref().is_some_and(|c| {
        game::current_stamp(gogid)
            .ok()
            .flatten()
            .is_some_and(|s| s != c.get("STAMP"))
    });
    let applied: BTreeMap<String, String> = conf
        .as_ref()
        .and_then(|c| c.arrays.get("ENV"))
        .into_iter()
        .flatten()
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    let mut cands: Vec<(String, String, EnvSource)> = Vec::new();
    let (k, v) = setup::GAME_DRIVE_OFF;
    cands.push((k.into(), v.into(), EnvSource::GogoDefault));
    for (k, s) in &r.effective.env {
        cands.push((
            k.clone(),
            s.value.clone(),
            EnvSource::Layer { layer: s.layer },
        ));
    }
    if let Some(b) = &baked {
        for (k, v) in &b.env {
            cands.push((k.clone(), v.clone(), EnvSource::Baked));
        }
    }
    if let Some(c) = &conf {
        let known = |k: &str| cands.iter().any(|(ck, _, _)| ck == k);
        let from_renderer: Vec<_> = applied
            .iter()
            .filter(|(k, _)| !known(k))
            .map(|(k, v)| {
                let src = if stale {
                    EnvSource::LastSetup
                } else {
                    EnvSource::Renderer {
                        renderer: c.get("RENDERER").to_string(),
                    }
                };
                (k.clone(), v.clone(), src)
            })
            .collect();
        cands.extend(from_renderer);
        if !c.get("DLL_OVERRIDES").is_empty() {
            cands.push((
                "WINEDLLOVERRIDES".into(),
                c.get("DLL_OVERRIDES").into(),
                EnvSource::DllOverrides,
            ));
        }
        for (k, v) in [
            ("WINEPREFIX", c.get("PREFIX")),
            ("GAMEID", c.get("UMU_ID")),
            ("STORE", "gog"),
            ("PROTONPATH", c.get("PROTON_DIR")),
        ] {
            cands.push((k.into(), v.into(), EnvSource::Launcher));
        }
    }
    if let Some(sum) = &r.summary {
        for i in &sum.items {
            let (Some(key), Some(value)) = (&i.key, &i.value) else {
                continue;
            };
            let Some(k) = key.strip_prefix("env ") else {
                continue;
            };
            if i.state != State::Applies || matches!(i.source, FixSource::Layer(_)) {
                continue;
            }
            cands.push((
                k.into(),
                value.clone(),
                EnvSource::Protonfix {
                    from: i.source.to_string(),
                },
            ));
        }
    }

    // Per key: the winner first, then the rest, by rank.
    cands.sort_by(|a, b| a.0.cmp(&b.0).then(b.2.rank().cmp(&a.2.rank())));
    let mut lines: Vec<EnvLine> = Vec::new();
    let mut winner: Option<(String, String)> = None;
    for (k, v, src) in cands {
        let state = match &winner {
            Some((wk, by)) if *wk == k => EnvState::Overridden { by: by.clone() },
            _ => {
                winner = Some((k.clone(), src.to_string()));
                let in_conf = matches!(src, EnvSource::Layer { .. } | EnvSource::Baked)
                    .then(|| applied.get(&k) == Some(&v));
                match (conf.is_some(), in_conf) {
                    (true, Some(false)) => EnvState::Pending,
                    _ => EnvState::Applies,
                }
            }
        };
        lines.push(EnvLine {
            value: shown(&k, &v, reveal),
            key: k,
            source: src,
            state,
        });
    }

    let mut warnings = r.warnings.clone();
    let (wrapper, wrapper_warnings) = wrapper_view(&r.effective, conf.as_ref());
    warnings.extend(wrapper_warnings);
    if conf.is_none() && r.built {
        warnings.push(format!(
            "{gogid} has no launch.conf yet: `gogo linux-app setup {gogid}` writes it"
        ));
    }
    if stale {
        warnings.push(
            "the override changed since the last setup: the next launch runs setup first, so \
             `pending` values apply then and renderer values may change"
                .into(),
        );
    }
    for l in &lines {
        if l.source == (EnvSource::Layer { layer: Layer::User }) {
            match (check_key(&l.key), policy(&l.key)) {
                (Err(why), _) => warnings.push(format!(
                    "env {} in the override file: {why}; setup skips it",
                    l.key
                )),
                (_, Policy::Refuse(why)) => warnings.push(format!("env {}: {why}", l.key)),
                _ => {}
            }
        }
    }
    Ok(EnvReport {
        gogid: gogid.to_string(),
        user_file: r.user_file,
        built: r.built,
        lines,
        wrapper,
        warnings,
    })
}

/// `gogo linux-app wrapper <gogid>`.
#[derive(Debug, Clone, Serialize)]
pub struct WrapperReport {
    pub gogid: String,
    pub user_file: PathBuf,
    pub built: bool,
    /// `None`: no wrapper set or applied.
    pub wrapper: Option<WrapperView>,
    pub warnings: Vec<String>,
}

/// The command the game runs under. Reads only.
pub fn wrapper_report(gogid: &str) -> Result<WrapperReport, MacAppError> {
    let r = manage::compat(gogid)?;
    let dirs = GameDirs::for_game(gogid)?;
    let conf = ConfValues::read(&dirs.launch_conf).ok();
    let (wrapper, mut warnings) = wrapper_view(&r.effective, conf.as_ref());
    warnings.extend(r.warnings);
    Ok(WrapperReport {
        gogid: gogid.to_string(),
        user_file: r.user_file,
        built: r.built,
        wrapper,
        warnings,
    })
}

/// `gogo linux-app args <gogid>`.
#[derive(Debug, Clone, Serialize)]
pub struct ArgsReport {
    pub gogid: String,
    pub user_file: PathBuf,
    pub built: bool,
    /// The entry point, as the launcher runs it (unix, relative to the data
    /// root), when built.
    pub exe: Option<String>,
    /// The game's own args, and where they come from: GOG's
    /// (`gogo-launch.toml`), the variant's, or an `args` layer (which
    /// replaces them).
    pub game_args: Vec<String>,
    pub game_args_from: String,
    /// `extra_args`, appended, and its layer.
    pub extra_args: Option<Sourced<Vec<String>>>,
    /// What the exe gets at the next launch: game args, then extra args.
    pub launch_args: Vec<String>,
    /// `launch.conf`'s `ARGS`: what the last setup wrote.
    pub applied: Option<Vec<String>>,
    /// `launch_args` isn't `applied` yet: the next launch's setup writes it.
    pub pending: bool,
    pub warnings: Vec<String>,
}

/// The game's launch args and where they come from. Reads only.
pub fn args_report(gogid: &str) -> Result<ArgsReport, MacAppError> {
    let r = manage::compat(gogid)?;
    let dirs = GameDirs::for_game(gogid)?;
    let conf = ConfValues::read(&dirs.launch_conf).ok();
    let baked = game::read_launch_toml(&dirs).ok().map(|(t, _)| t);
    let e = &r.effective;
    let (game_args, game_args_from) = match (&e.args, &baked) {
        (Some(a), _) => (
            a.value.clone(),
            format!("`args` in {} (replaces GOG's)", a.layer.describe()),
        ),
        (None, Some(b)) => {
            let over = CompatEntry {
                variant: e.variant.as_ref().map(|s| s.value.clone()),
                ..Default::default()
            };
            match b.effective(&CompatEntry::default(), &over) {
                Ok(t) => (
                    t.args,
                    match &e.variant {
                        Some(v) => format!("variant {:?}", v.value),
                        None => "GOG (gogo-launch.toml)".into(),
                    },
                ),
                Err(_) => (b.args.clone(), "GOG (gogo-launch.toml)".into()),
            }
        }
        (None, None) => (vec![], "not built yet".into()),
    };
    let launch = launch_args(&game_args, e);
    let applied = conf.as_ref().and_then(|c| c.arrays.get("ARGS").cloned());
    let pending = applied.as_ref().is_some_and(|a| *a != launch);
    Ok(ArgsReport {
        gogid: gogid.to_string(),
        user_file: r.user_file,
        built: r.built,
        exe: conf
            .as_ref()
            .map(|c| c.get("EXE").to_string())
            .filter(|e| !e.is_empty()),
        game_args,
        game_args_from,
        extra_args: e.extra_args.clone(),
        launch_args: launch,
        applied,
        pending,
        warnings: r.warnings,
    })
}

// — edits —

/// What an edit did.
#[derive(Debug, Clone, Serialize)]
pub struct EditOutcome {
    pub gogid: String,
    pub file: PathBuf,
    /// Raw values: hide secrets with [`shown`] before printing.
    pub changes: Vec<Change>,
    pub warnings: Vec<String>,
    /// Built: the next launch applies it (setup runs on the changed
    /// override). Not built: the build does.
    pub built: bool,
}

fn refused(gogid: &str, reason: String) -> MacAppError {
    MacAppError::Compat {
        path: compat::user_path(gogid),
        reason,
    }
}

/// Check `KEY=VALUE` pairs for [`set_env`]: the warnings, or the first
/// refusal. `gogo linux-app env set` calls it first for a usage error.
pub fn check_set(pairs: &[(String, String)]) -> Result<Vec<String>, String> {
    let mut warnings = Vec::new();
    for (k, v) in pairs {
        check_key(k).map_err(|why| format!("env {k:?}: {why}"))?;
        check_value(&format!("the value of {k}"), v)?;
        match policy(k) {
            Policy::Refuse(why) => return Err(why),
            Policy::Warn(w) => warnings.push(format!("{k}: {w}")),
            Policy::Free => {}
        }
    }
    Ok(warnings)
}

fn built(gogid: &str) -> Result<bool, MacAppError> {
    Ok(game::launch_toml_path(&GameDirs::for_game(gogid)?).is_file())
}

/// After an env edit: warn where a protonfix still wins at launch.
fn protonfix_wins(gogid: &str, keys: &[&str], warnings: &mut Vec<String>) {
    let Ok(r) = env_report(gogid, false) else {
        return;
    };
    for l in &r.lines {
        if keys.contains(&l.key.as_str())
            && matches!(l.source, EnvSource::Protonfix { .. })
            && l.state == EnvState::Applies
        {
            warnings.push(format!(
                "{}: {} sets it at launch and wins (`protonfixes = false` under \
                 [platforms.linux] in {} turns the protonfix off)",
                l.key,
                l.source,
                r.user_file.display()
            ));
        }
    }
}

/// `gogo linux-app env set <gogid> KEY=VALUE…`: write them into L4's
/// `[platforms.linux.env]`. Refused keys ([`policy`]) write nothing.
pub fn set_env(gogid: &str, pairs: &[(String, String)]) -> Result<EditOutcome, MacAppError> {
    compat::validate_gogid(gogid)?;
    let mut warnings = check_set(pairs).map_err(|r| refused(gogid, r))?;
    let edits: Vec<UserEdit> = pairs
        .iter()
        .map(|(k, v)| UserEdit::SetEnv(k.clone(), v.clone()))
        .collect();
    let changes = compat::edit_user_platform_for(gogid, &edits, Os::Linux)?;
    let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
    protonfix_wins(gogid, &keys, &mut warnings);
    Ok(EditOutcome {
        gogid: gogid.to_string(),
        file: compat::user_path(gogid),
        changes,
        warnings,
        built: built(gogid)?,
    })
}

/// `gogo linux-app env unset <gogid> KEY…`: drop them from L4. A key that
/// another layer sets is named in a warning: unsetting only takes yours.
pub fn unset_env(gogid: &str, keys: &[String]) -> Result<EditOutcome, MacAppError> {
    compat::validate_gogid(gogid)?;
    for k in keys {
        check_key(k).map_err(|why| refused(gogid, format!("env {k:?}: {why}")))?;
    }
    let edits: Vec<UserEdit> = keys.iter().map(|k| UserEdit::UnsetEnv(k.clone())).collect();
    let changes = compat::edit_user_platform_for(gogid, &edits, Os::Linux)?;
    let mut warnings = Vec::new();
    if let Ok(r) = env_report(gogid, false) {
        for k in keys {
            if let Some(l) = r
                .lines
                .iter()
                .find(|l| l.key == *k && !matches!(l.state, EnvState::Overridden { .. }))
            {
                warnings.push(format!(
                    "{k} is still set: by {} (set it yourself to override that)",
                    l.source
                ));
            } else if !changes.iter().any(|c| c.key == format!("env.{k}")) {
                warnings.push(format!("{k} wasn't set"));
            }
        }
    }
    Ok(EditOutcome {
        gogid: gogid.to_string(),
        file: compat::user_path(gogid),
        changes,
        warnings,
        built: built(gogid)?,
    })
}

/// `gogo linux-app args <gogid> -- <args…>` (`Some`) or `--clear`
/// (`None`): L4's `[platforms.linux] extra_args`.
pub fn set_args(gogid: &str, args: Option<Vec<String>>) -> Result<EditOutcome, MacAppError> {
    compat::validate_gogid(gogid)?;
    for a in args.iter().flatten() {
        check_value(&format!("arg {a:?}"), a).map_err(|r| refused(gogid, r))?;
    }
    let changes = compat::edit_user_platform_for(gogid, &[UserEdit::ExtraArgs(args)], Os::Linux)?;
    let mut warnings = Vec::new();
    if let Ok(r) = args_report(gogid)
        && r.game_args_from.starts_with("`args`")
        && r.extra_args.is_some()
    {
        warnings.push(format!(
            "the game's args come from {}; the extra args go after them",
            r.game_args_from
        ));
    }
    Ok(EditOutcome {
        gogid: gogid.to_string(),
        file: compat::user_path(gogid),
        changes,
        warnings,
        built: built(gogid)?,
    })
}

/// `gogo linux-app wrapper <gogid> -- <words…>` (`Some`) or `--clear`
/// (`None`): L4's `[platforms.linux] wrapper`. A refused one writes nothing.
pub fn set_wrapper(gogid: &str, words: Option<Vec<String>>) -> Result<EditOutcome, MacAppError> {
    compat::validate_gogid(gogid)?;
    let mut warnings = match &words {
        Some(w) => check_wrapper(w).map_err(|r| refused(gogid, r))?,
        None => Vec::new(),
    };
    let changes = compat::edit_user_platform_for(gogid, &[UserEdit::Wrapper(words)], Os::Linux)?;
    if let Ok(r) = wrapper_report(gogid)
        && let Some(v) = r.wrapper
        && let (Some(program), Some(layer)) = (wrapper_program(&v.words), v.layer)
    {
        if !super::doctor::Host::detect().finds_program(program) {
            warnings.push(format!(
                "`{program}` isn't on PATH here: a launch that doesn't find it stops and says so"
            ));
        }
        if layer != Layer::User {
            warnings.push(format!(
                "the game still runs under {} (from the {})",
                shell_words(&v.words),
                layer.describe()
            ));
        }
    }
    Ok(EditOutcome {
        gogid: gogid.to_string(),
        file: compat::user_path(gogid),
        changes,
        warnings,
        built: built(gogid)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names() {
        for ok in ["DXVK_HUD", "_X", "a1", "PROTON_ENABLE_NVAPI"] {
            assert_eq!(check_key(ok), Ok(()), "{ok}");
        }
        for bad in ["", "1A", "A-B", "A B", "A=B", "Ä", "A.B"] {
            assert!(check_key(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn values_and_args() {
        assert!(check_value("v", "a b 'c' \"d\" $HOME `x` \\").is_ok());
        assert!(check_value("v", "").is_ok());
        assert!(check_value("v", "a\nb").is_err());
        assert!(check_value("v", "a\0b").is_err());
    }

    #[test]
    fn wrappers_are_argv_form() {
        let w = |words: &[&str]| {
            check_wrapper(&words.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        };
        for ok in [
            &["gamemoderun"][..],
            &["gamescope", "-f", "--"],
            &["gamemoderun", "gamescope", "-w", "1920", "--"],
            &["gamescope", "--", "gamemoderun"],
            &["env", "A=b c", "gamemoderun"],
            &["/opt/my tools/wrap", "a b", "'q'", "$HOME"],
        ] {
            assert_eq!(w(ok), Ok(vec![]), "{ok:?}");
        }
        for bad in [
            &[][..],
            &[""],
            &["-f"],
            &["gamescope -f --"],
            &["./wrap"],
            &["tools/wrap"],
            &["DXVK_HUD=1", "gamemoderun"],
            &["gamescope", "--", "%command%"],
            &["gamescope", "a\nb"],
            &["gamescope", "a\rb"],
            &["gamescope", "a\0b"],
        ] {
            assert!(w(bad).is_err(), "{bad:?}");
        }
        for odd in [&["gamescope", "-f"][..], &["gamemoderun", "/x/gamescope"]] {
            let warned = w(odd).unwrap();
            assert_eq!(warned.len(), 1, "{odd:?}");
            assert!(warned[0].contains("`--`"), "{warned:?}");
        }
    }

    #[test]
    fn wrapper_chains_name_what_they_run() {
        let words: Vec<String> = ["gamemoderun", "/x/gamescope", "-f", "--"]
            .map(String::from)
            .to_vec();
        assert_eq!(wrapper_program(&words), Some("gamemoderun"));
        assert!(wrapper_runs(&words, "gamescope"));
        assert!(wrapper_runs(&words, "gamemoderun"));
        assert!(!wrapper_runs(&words, "mangohud"));
    }

    #[test]
    fn the_view_says_what_the_next_setup_changes() {
        let conf = |text: &str| ConfValues::parse(text);
        let mut eff = Effective::default();
        assert_eq!(wrapper_view(&eff, None), (None, vec![]));
        assert_eq!(wrapper_view(&eff, Some(&conf("ARGS=()\n"))), (None, vec![]));
        eff.wrapper = Some(Sourced {
            value: vec!["gamemoderun".into()],
            layer: Layer::User,
        });
        // Set, not built: nothing pending. Built with another: pending.
        let (v, _) = wrapper_view(&eff, None);
        assert!(!v.unwrap().pending);
        let (v, _) = wrapper_view(&eff, Some(&conf("ARGS=()\n")));
        let v = v.unwrap();
        assert!(v.pending);
        assert_eq!(v.layer, Some(Layer::User));
        let (v, _) = wrapper_view(&eff, Some(&conf("WRAPPER=('gamemoderun')\n")));
        assert!(!v.unwrap().pending);
        // Dropped, launch.conf still has it.
        eff.wrapper = None;
        let (v, _) = wrapper_view(&eff, Some(&conf("WRAPPER=('gamemoderun')\n")));
        let v = v.unwrap();
        assert!(v.pending && v.words.is_empty() && v.layer.is_none());
        // Hand-written and refused: setup skips it, and that's said.
        eff.wrapper = Some(Sourced {
            value: vec!["gamescope -f --".into()],
            layer: Layer::User,
        });
        let (v, w) = wrapper_view(&eff, None);
        assert_eq!(v, None);
        assert!(w[0].contains("setup skips it"), "{w:?}");
    }

    #[test]
    fn ownership() {
        for k in LAUNCHER_KEYS {
            assert!(matches!(policy(k), Policy::Refuse(_)), "{k}");
        }
        assert!(matches!(policy("WINEDLLOVERRIDES"), Policy::Refuse(_)));
        assert!(matches!(policy("PROTONFIXES_DISABLE"), Policy::Refuse(_)));
        assert!(matches!(policy("PROTON_USE_D7VK"), Policy::Warn(_)));
        assert!(matches!(policy("PROTON_SET_GAME_DRIVE"), Policy::Warn(_)));
        assert_eq!(policy("PROTON_ENABLE_NVAPI"), Policy::Free);
        let w = check_set(&[
            ("DXVK_HUD".into(), "fps".into()),
            ("PROTON_USE_WINED3D".into(), "1".into()),
        ])
        .unwrap();
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(check_set(&[("GAMEID".into(), "x".into())]).is_err());
        assert!(check_set(&[("A B".into(), "x".into())]).is_err());
    }

    #[test]
    fn secrets_are_hidden() {
        assert!(is_secret("STEAM_API_KEY"));
        assert!(is_secret("my_token"));
        assert!(!is_secret("DXVK_HUD"));
        assert_eq!(shown("GH_TOKEN", "abc", false), HIDDEN);
        assert_eq!(shown("GH_TOKEN", "abc", true), "abc");
        assert_eq!(shown("DXVK_HUD", "fps", false), "fps");
    }

    #[test]
    fn launch_args_append_extra() {
        let mut eff = Effective::default();
        assert_eq!(launch_args(&["-a".into()], &eff), ["-a"]);
        eff.extra_args = Some(Sourced {
            value: vec!["--launcher-skip".into(), "-skipStartScreen".into()],
            layer: Layer::User,
        });
        assert_eq!(
            launch_args(&["-a".into()], &eff),
            ["-a", "--launcher-skip", "-skipStartScreen"]
        );
    }

    #[test]
    fn shell_words_quote_what_needs_it() {
        let w: Vec<String> = ["-skipStartScreen", "a b", "it's", "$HOME", ""]
            .map(String::from)
            .to_vec();
        assert_eq!(
            shell_words(&w),
            r#"-skipStartScreen 'a b' 'it'\''s' '$HOME' ''"#
        );
    }
}
