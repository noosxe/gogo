//! The per-game Galaxy switch (design 23 decision 3, #489): whether a game
//! runs with Comet beside it.
//!
//! The choice lives in the game's L4 override file as `[platforms.linux]
//! galaxy = "comet" | "off"` (`gogo linux-app galaxy <gogid> on|off`), like
//! `display` and `wrapper`; setup renders it into `launch.conf` as
//! `GALAXY=1`, which `launch.sh` acts on. Lower layers only recommend:
//!
//! 1. the game's own L4 value, `off` included;
//! 2. else `[linux] galaxy_default`: `on` turns it on for a game whose files
//!    ship the Galaxy SDK, `ask` asks once on a terminal about a game that
//!    ships it or that a compat layer recommends Comet for, and stores the
//!    answer in L4 (headless: off, with a hint), `off` (the default) leaves
//!    it off;
//! 3. else off.
//!
//! Nothing here downloads Comet: a missing binary is a hint to run
//! `gogo runtimes install comet`.

use std::path::Path;

use serde::Serialize;

use super::comet;
use super::envargs::EditOutcome;
use super::game::{ConfValues, LinuxState};
use super::manage;
use crate::config::{Config, GalaxyDefault};
use crate::macapp::compat::{self, Effective, GalaxyMode, Layer, Os, UserEdit};
use crate::macapp::doctor::{Finding, Level};
use crate::macapp::error::MacAppError;
use crate::macapp::launch::GameDirs;
use crate::macapp::prompt::{PromptAnswer, PromptFn, PromptKind, PromptSpec};

/// The SDK's DLL names, lowercase (32-bit and 64-bit).
const SDK_DLLS: [&str; 2] = ["galaxy.dll", "galaxy64.dll"];

/// The deepest the SDK scan looks, and the most entries it visits: a game
/// tree is never walked without a bound.
const SCAN_DEPTH: usize = 8;
const SCAN_ENTRIES: usize = 100_000;

/// Does the game ship the Galaxy SDK (`Galaxy.dll` / `Galaxy64.dll`, any
/// case, anywhere under `root`)? `Some(true)` found, `Some(false)` the whole
/// tree was looked at and it isn't there, `None` the scan hit its bounds
/// first ([`SCAN_DEPTH`], [`SCAN_ENTRIES`]) or couldn't read a dir, so it
/// can't say. Symlinks are not followed.
pub fn detect_sdk(root: &Path) -> Option<bool> {
    detect_within(root, SCAN_DEPTH, SCAN_ENTRIES)
}

fn detect_within(root: &Path, depth: usize, entries: usize) -> Option<bool> {
    let mut scan = Scan {
        left: entries,
        complete: true,
    };
    if scan.walk(root, depth) {
        return Some(true);
    }
    scan.complete.then_some(false)
}

struct Scan {
    left: usize,
    complete: bool,
}

impl Scan {
    fn walk(&mut self, dir: &Path, depth: usize) -> bool {
        let Ok(rd) = std::fs::read_dir(dir) else {
            self.complete = false;
            return false;
        };
        for e in rd.flatten() {
            if self.left == 0 {
                self.complete = false;
                return false;
            }
            self.left -= 1;
            let Ok(t) = e.file_type() else {
                self.complete = false;
                continue;
            };
            if t.is_dir() {
                if depth == 0 {
                    self.complete = false;
                } else if self.walk(&e.path(), depth - 1) {
                    return true;
                }
            } else if t.is_file()
                && e.file_name()
                    .to_str()
                    .is_some_and(|n| SDK_DLLS.contains(&n.to_ascii_lowercase().as_str()))
            {
                return true;
            }
        }
        false
    }
}

/// What [`decide`] settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    /// Comet runs beside the game.
    pub on: bool,
    /// The game's own L4 value decided.
    pub own: bool,
    /// `galaxy_default = "ask"` applies and nothing answered yet.
    pub ask: bool,
}

/// The merged compat (shipped data, then the game's own L4 entry) every
/// Galaxy decision reads, so setup, its question, `apps show` and `doctor`
/// can't disagree.
pub fn effective(gogid: &str, l4: &compat::CompatEntry) -> Effective {
    let db = compat::db_entry_for(gogid, Os::Linux);
    compat::merge(&[(Layer::Db, &db.entry), (Layer::User, l4)])
}

/// The game's own value and whether a lower layer recommends Comet, from
/// the merged compat.
pub fn split(eff: &Effective) -> (Option<GalaxyMode>, bool) {
    match &eff.galaxy {
        Some(s) if s.layer == Layer::User => (Some(s.value), false),
        Some(s) => (None, s.value == GalaxyMode::Comet),
        None => (None, false),
    }
}

/// The rules in the module docs.
pub fn decide(
    own: Option<GalaxyMode>,
    recommended: bool,
    default: GalaxyDefault,
    sdk: Option<bool>,
) -> Decision {
    if let Some(m) = own {
        return Decision {
            on: m == GalaxyMode::Comet,
            own: true,
            ask: false,
        };
    }
    // `on` is for the games that ship the SDK; a recommendation alone only
    // makes a game worth asking about (and worth a hint).
    Decision {
        on: sdk == Some(true) && default == GalaxyDefault::On,
        own: false,
        ask: (recommended || sdk == Some(true)) && default == GalaxyDefault::Ask,
    }
}

/// `[linux] galaxy_default` in the config at `path`: unset is `off`; a
/// value that isn't one of the three is `off` with the reason.
pub fn default_at(path: &Path) -> (GalaxyDefault, Option<String>) {
    match super::stored_key(path, "galaxy_default").as_deref() {
        None | Some("off") => (GalaxyDefault::Off, None),
        Some("on") => (GalaxyDefault::On, None),
        Some("ask") => (GalaxyDefault::Ask, None),
        Some(other) => (
            GalaxyDefault::Off,
            Some(format!(
                "[linux] galaxy_default = {other:?}: use \"off\", \"on\" or \"ask\"; treated as \"off\""
            )),
        ),
    }
}

/// The hint for a game that would have Comet but can't have it yet.
pub const INSTALL_HINT: &str = "Comet isn't installed: `gogo runtimes install comet` fetches it \
     (the launch skips it until then)";

/// A hint when Comet can't be found without a download.
pub fn comet_hint(host: &super::umu::Host<'_>, config: &Path) -> Option<String> {
    match comet::mode_at(config).and_then(|m| comet::available(host, &m)) {
        Ok(Some(_)) => None,
        Ok(None) => Some(INSTALL_HINT.to_string()),
        Err(e) => Some(format!("Comet: {e}")),
    }
}

/// The question a terminal gets (`galaxy_default = "ask"`): once per game.
pub fn ask_spec(gogid: &str, sdk: bool, recommended: bool) -> PromptSpec {
    let why = match (sdk, recommended) {
        (true, true) => "it ships the Galaxy SDK and gogo's compat data recommends Comet",
        (false, true) => "gogo's compat data recommends Comet for it",
        _ => "it ships the Galaxy SDK",
    };
    PromptSpec {
        kind: PromptKind::Select,
        title: format!(
            "Game {gogid}: run Comet (a GOG Galaxy service stand-in) beside it? {why}. Comet \
             gives the game's Galaxy SDK your signed-in user, achievements and \
             cross-progression saves; it needs your gogo login. Asked once; \
             `gogo linux-app galaxy {gogid} on|off` changes it later."
        ),
        items: vec![
            "on: start Comet when the game starts".into(),
            "off: no Comet".into(),
        ],
        allow_other: false,
        allow_none: false,
        default: 1,
    }
}

/// Ask `prompt` and store the answer in the game's override file. `None`:
/// there was no answer (no terminal, or the prompt gave none).
pub fn ask_and_store(
    gogid: &str,
    prompt: Option<&PromptFn>,
    sdk: bool,
    recommended: bool,
) -> Result<Option<GalaxyMode>, MacAppError> {
    let Some(answer) = prompt.and_then(|p| p(&ask_spec(gogid, sdk, recommended))) else {
        return Ok(None);
    };
    let mode = match answer {
        PromptAnswer::Index(0) => GalaxyMode::Comet,
        PromptAnswer::Index(_) => GalaxyMode::Off,
        _ => return Ok(None),
    };
    compat::edit_user_platform_for(gogid, &[UserEdit::Galaxy(Some(mode))], Os::Linux)?;
    Ok(Some(mode))
}

/// `gogo linux-app galaxy <gogid> on|off|--clear`: write the game's own
/// value. `on` never downloads: a missing Comet is a warning with the
/// command.
pub fn set_galaxy(gogid: &str, mode: Option<GalaxyMode>) -> Result<EditOutcome, MacAppError> {
    compat::validate_gogid(gogid)?;
    let changes = compat::edit_user_platform_for(gogid, &[UserEdit::Galaxy(mode)], Os::Linux)?;
    let mut warnings = Vec::new();
    if mode == Some(GalaxyMode::Comet) {
        warnings.extend(comet_hint(&super::umu::Host::real(), &Config::path()));
    }
    if let Ok(r) = manage::compat(gogid)
        && let Some(g) = r
            .effective
            .galaxy
            .as_ref()
            .filter(|g| g.layer != Layer::User)
    {
        warnings.push(format!(
            "`galaxy` is {:?} from the {} (a recommendation: `galaxy_default` decides)",
            g.value.key(),
            g.layer.describe()
        ));
    }
    Ok(EditOutcome {
        gogid: gogid.to_string(),
        file: compat::user_path(gogid),
        changes,
        warnings,
        built: super::game::launch_toml_path(&GameDirs::for_game(gogid)?).is_file(),
    })
}

/// What `apps show`, `linux-app galaxy` and `doctor` say about a game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GalaxyView {
    /// What `launch.conf` has now (`GALAXY=1`): what a launch does.
    pub on: bool,
    /// What the next setup writes.
    pub wanted: bool,
    /// Where the answer comes from.
    pub source: Source,
    /// A compat layer recommends Comet for the game.
    pub recommended: bool,
    /// The game ships `Galaxy.dll` / `Galaxy64.dll`; `None`: not looked at.
    pub sdk: Option<bool>,
    /// The Comet a launch would run, as `0.4.1`, `system` or `path:`;
    /// `None`: none without a download.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comet: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The game's override file.
    Own,
    /// `[linux] galaxy_default = "on"`.
    Default,
    /// Nothing turned it on.
    Off,
}

impl GalaxyView {
    /// `comet on (0.4.1)`, `comet on (not installed)`, `off`,
    /// `off (compat recommends comet)`, plus a pending change.
    pub fn line(&self) -> String {
        let mut out = if self.on {
            format!(
                "comet on ({})",
                self.comet.as_deref().unwrap_or("not installed")
            )
        } else {
            "off".to_string()
        };
        if !self.on && self.recommended {
            out.push_str(" (compat recommends comet)");
        }
        if self.on != self.wanted {
            out.push_str(&format!(
                "; the next setup makes it {}",
                if self.wanted { "on" } else { "off" }
            ));
        }
        out
    }

    /// `shipped`, `not found` or `unknown`.
    pub fn sdk_line(&self) -> &'static str {
        match self.sdk {
            Some(true) => "shipped",
            Some(false) => "not found",
            None => "unknown",
        }
    }
}

/// The view of a built game from what is on disk. `scan`: look for the SDK
/// when `state.toml` has no record (an old build); never writes it.
pub fn view(gogid: &str, dirs: &GameDirs, tree: Option<&str>, scan: bool) -> Option<GalaxyView> {
    let conf = ConfValues::read(&dirs.launch_conf).ok();
    let state = LinuxState::load(&dirs.state_toml).ok().flatten();
    let l4 = compat::load_user_for(gogid, Os::Linux).ok()?;
    let (own, recommended) = split(&effective(gogid, &l4.entry));
    let sdk = state.as_ref().and_then(|s| s.galaxy_sdk).or_else(|| {
        scan.then(|| detect_sdk(&super::setup::game_files(dirs, tree)))
            .flatten()
    });
    let (default, _) = default_at(&Config::path());
    let d = decide(own, recommended, default, sdk);
    let on = conf.as_ref().is_some_and(|c| c.get("GALAXY") == "1");
    let source = if d.own {
        Source::Own
    } else if d.on {
        Source::Default
    } else {
        Source::Off
    };
    let comet = comet::available(
        &super::umu::Host::real(),
        &comet::mode_at(&Config::path()).unwrap_or(crate::config::CometMode::Fetch),
    )
    .ok()
    .flatten()
    .map(|b| comet::version_label(&b));
    Some(GalaxyView {
        on,
        wanted: d.on,
        source,
        recommended,
        sdk,
        comet,
    })
}

/// What `doctor` read of the machine for one game with Comet on.
pub struct Facts {
    /// The Comet a launch would run, or why there is none.
    pub comet: Result<Option<comet::CometBin>, String>,
    /// `None`: not checked.
    pub signed_in: Option<bool>,
    /// Why `$XDG_RUNTIME_DIR` can't hold Comet's registry.
    pub runtime_problem: Option<String>,
    /// Something listens on Comet's port, and who when it can be said.
    pub port: Option<Option<String>>,
}

/// `doctor`'s Galaxy findings (design 23 §3). Nothing here holds a token,
/// an id or a user name. Every problem is a warning: a launch goes on
/// without Comet.
pub fn findings(gogid: &str, v: &GalaxyView, facts: Option<&Facts>) -> Vec<Finding> {
    let on_hint = format!("gogo linux-app galaxy {gogid} on");
    let off_hint = format!("gogo linux-app galaxy {gogid} off");
    let mut out = Vec::new();
    let active = v.on || v.wanted;
    out.push(match (active, v.sdk, v.recommended) {
        (true, Some(false), _) => Finding::warn(
            "galaxy",
            format!("{}, but the game has no Galaxy SDK (Galaxy.dll/Galaxy64.dll): Comet does nothing for it", v.line()),
            off_hint,
        ),
        (true, _, _) => Finding::ok("galaxy", format!("{}; Galaxy SDK {}", v.line(), v.sdk_line())),
        (false, Some(true), _) => Finding::new(
            "galaxy",
            Level::Info,
            "the game ships the Galaxy SDK and galaxy is off: Comet could give it your signed-in \
             user, achievements and cross-platform saves",
        )
        .hint(on_hint),
        (false, _, true) => Finding::new(
            "galaxy",
            Level::Info,
            "off, and compat data recommends comet for this game",
        )
        .hint(on_hint),
        (false, _, false) => Finding::ok("galaxy", format!("off; Galaxy SDK {}", v.sdk_line())),
    });
    let (true, Some(f)) = (active, facts) else {
        return out;
    };
    out.push(match &f.comet {
        Ok(Some(bin)) => Finding::ok(
            "comet",
            match bin.source {
                comet::Source::Store | comet::Source::Env => format!(
                    "{} ({}): the pinned release",
                    comet::label(bin),
                    bin.path.display()
                ),
                _ => format!(
                    "{} ({}): not gogo's pin, its version isn't checked",
                    comet::label(bin),
                    bin.path.display()
                ),
            },
        ),
        Ok(None) => Finding::warn(
            "comet",
            "Comet isn't installed: the launch goes on without it",
            "gogo runtimes install comet",
        ),
        Err(e) => Finding::warn(
            "comet",
            format!("{e}: the launch goes on without Comet"),
            "gogo runtimes list",
        ),
    });
    match f.signed_in {
        Some(true) => out.push(Finding::ok("galaxy-login", "signed in")),
        Some(false) => out.push(Finding::warn(
            "galaxy-login",
            "not signed in: Comet is skipped at launch",
            "gogo login",
        )),
        None => {}
    }
    out.push(match &f.runtime_problem {
        None => Finding::ok(
            "galaxy-runtime",
            "$XDG_RUNTIME_DIR can hold Comet's registry",
        ),
        Some(why) => Finding::warn(
            "galaxy-runtime",
            format!("{why}: Comet is skipped at launch"),
            "log in through a session that sets XDG_RUNTIME_DIR (systemd-logind does)",
        ),
    });
    out.push(match &f.port {
        None => Finding::ok(
            "galaxy-port",
            format!("port {} is free", super::galaxy::PORT),
        ),
        Some(who) => Finding::new(
            "galaxy-port",
            Level::Info,
            format!(
                "port {} is held{}: the launch reuses it and starts no Comet of its own",
                super::galaxy::PORT,
                who.as_deref()
                    .map(|w| format!(" by {w}"))
                    .unwrap_or_default()
            ),
        ),
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eff(toml_layers: &[(Layer, &str)]) -> Effective {
        let parsed: Vec<_> = toml_layers
            .iter()
            .map(|(l, t)| (*l, compat::parse(t, Os::Linux, *l).unwrap().entry))
            .collect();
        let refs: Vec<_> = parsed.iter().map(|(l, e)| (*l, e)).collect();
        compat::merge(&refs)
    }

    #[test]
    fn detects_the_sdk_by_name_in_any_case_and_depth() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(detect_sdk(t.path()), Some(false));
        let bin = t.path().join("bin/x64");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("game.exe"), "x").unwrap();
        std::fs::write(bin.join("galaxy-helper.dll"), "x").unwrap();
        assert_eq!(detect_sdk(t.path()), Some(false));
        std::fs::write(bin.join("GALAXY64.DLL"), "x").unwrap();
        assert_eq!(detect_sdk(t.path()), Some(true));
        let u = tempfile::tempdir().unwrap();
        std::fs::write(u.path().join("Galaxy.dll"), "x").unwrap();
        assert_eq!(detect_sdk(u.path()), Some(true));
    }

    #[test]
    fn a_symlinked_dir_is_not_followed() {
        let t = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("Galaxy64.dll"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path(), t.path().join("link")).unwrap();
        assert_eq!(detect_sdk(t.path()), Some(false));
    }

    #[test]
    fn the_games_own_value_beats_everything() {
        for d in [GalaxyDefault::Off, GalaxyDefault::On, GalaxyDefault::Ask] {
            let on = decide(Some(GalaxyMode::Comet), false, d, Some(false));
            assert!(on.on && on.own && !on.ask);
            let off = decide(Some(GalaxyMode::Off), true, d, Some(true));
            assert!(!off.on && off.own && !off.ask);
        }
    }

    #[test]
    fn a_default_turns_on_only_games_that_ship_the_sdk() {
        let sdk = Some(true);
        assert!(!decide(None, false, GalaxyDefault::Off, sdk).on);
        assert!(decide(None, false, GalaxyDefault::On, sdk).on);
        // A recommendation without the SDK doesn't turn it on, nor does an
        // unknown scan or no SDK.
        assert!(!decide(None, true, GalaxyDefault::On, Some(false)).on);
        assert!(!decide(None, true, GalaxyDefault::On, None).on);
        assert!(!decide(None, false, GalaxyDefault::On, Some(false)).on);
        // `ask` asks about a game that ships it or is recommended.
        let ask = decide(None, false, GalaxyDefault::Ask, sdk);
        assert!(ask.ask && !ask.on);
        assert!(decide(None, true, GalaxyDefault::Ask, Some(false)).ask);
        assert!(!decide(None, false, GalaxyDefault::Ask, Some(false)).ask);
        assert!(!decide(None, true, GalaxyDefault::Off, sdk).ask);
        assert!(!decide(None, true, GalaxyDefault::Off, None).on);
    }

    #[test]
    fn a_scan_that_hits_its_bounds_says_unknown_not_absent() {
        let t = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(t.path().join(format!("f{i}")), "x").unwrap();
        }
        assert_eq!(detect_within(t.path(), 3, 100), Some(false));
        // Too many entries.
        assert_eq!(detect_within(t.path(), 3, 3), None);
        // Too deep.
        let deep = t.path().join("a/b/c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("Galaxy.dll"), "x").unwrap();
        assert_eq!(detect_within(t.path(), 1, 100), None);
        // Found before the bound is reached is still found.
        assert_eq!(detect_within(t.path(), 3, 100), Some(true));
        // An unreadable root can't say either.
        assert_eq!(detect_within(&t.path().join("missing"), 3, 100), None);
    }

    #[test]
    fn a_compat_layer_recommends_and_the_users_file_decides() {
        let rec = "[platforms.linux]\ngalaxy = \"comet\"\n";
        assert_eq!(split(&eff(&[(Layer::Db, rec)])), (None, true));
        assert_eq!(
            split(&eff(&[
                (Layer::Db, rec),
                (Layer::User, "[platforms.linux]\ngalaxy = \"off\"\n")
            ])),
            (Some(GalaxyMode::Off), false)
        );
        assert_eq!(split(&eff(&[(Layer::Db, "")])), (None, false));
    }

    #[test]
    fn the_config_default_reads_three_words() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("config.toml");
        assert_eq!(default_at(&p), (GalaxyDefault::Off, None));
        for (w, d) in [
            ("on", GalaxyDefault::On),
            ("ask", GalaxyDefault::Ask),
            ("off", GalaxyDefault::Off),
        ] {
            std::fs::write(&p, format!("[linux]\ngalaxy_default = \"{w}\"\n")).unwrap();
            assert_eq!(default_at(&p), (d, None));
        }
        std::fs::write(&p, "[linux]\ngalaxy_default = \"yes\"\n").unwrap();
        let (d, why) = default_at(&p);
        assert_eq!(d, GalaxyDefault::Off);
        assert!(why.unwrap().contains("galaxy_default"));
    }

    #[test]
    fn asking_stores_the_answer_and_no_answer_stores_nothing() {
        let _g = crate::test_util::env_lock();
        let t = tempfile::tempdir().unwrap();
        // SAFETY: env mutation under the shared env mutex.
        unsafe { std::env::set_var("GOGO_HOME", t.path()) };
        let yes: Box<PromptFn> = Box::new(|_| Some(PromptAnswer::Index(0)));
        let no: Box<PromptFn> = Box::new(|_| Some(PromptAnswer::Index(1)));
        assert_eq!(ask_and_store("1", None, true, false).unwrap(), None);
        assert!(!compat::user_path("1").exists());
        assert_eq!(
            ask_and_store("1", Some(&*yes), true, false).unwrap(),
            Some(GalaxyMode::Comet)
        );
        let l4 = compat::load_user_for("1", Os::Linux).unwrap();
        assert_eq!(l4.entry.galaxy, Some(GalaxyMode::Comet));
        assert_eq!(
            ask_and_store("1", Some(&*no), true, false).unwrap(),
            Some(GalaxyMode::Off)
        );
    }

    fn view_of(on: bool, sdk: Option<bool>, recommended: bool) -> GalaxyView {
        GalaxyView {
            on,
            wanted: on,
            source: Source::Own,
            recommended,
            sdk,
            comet: None,
        }
    }

    fn facts(comet: Result<Option<comet::CometBin>, String>) -> Facts {
        Facts {
            comet,
            signed_in: Some(false),
            runtime_problem: Some("no XDG_RUNTIME_DIR".into()),
            port: Some(Some("comet, pid 7".into())),
        }
    }

    #[test]
    fn doctor_hints_at_the_sdk_with_galaxy_off_and_warns_on_the_reverse() {
        let level = |f: Vec<Finding>| f[0].level;
        assert_eq!(
            level(findings("1", &view_of(false, Some(true), false), None)),
            Level::Info
        );
        assert_eq!(
            level(findings("1", &view_of(false, Some(false), true), None)),
            Level::Info
        );
        assert_eq!(
            level(findings("1", &view_of(false, Some(false), false), None)),
            Level::Ok
        );
        assert_eq!(
            level(findings("1", &view_of(false, None, false), None)),
            Level::Ok
        );
        assert_eq!(
            level(findings("1", &view_of(true, Some(false), false), None)),
            Level::Warn
        );
        assert_eq!(
            level(findings("1", &view_of(true, Some(true), false), None)),
            Level::Ok
        );
        // Off: no machine checks at all.
        assert_eq!(
            findings("1", &view_of(false, Some(true), false), None).len(),
            1
        );
    }

    #[test]
    fn doctor_checks_the_machine_only_with_galaxy_on_and_never_prints_a_secret() {
        let v = view_of(true, Some(true), false);
        let f = findings("1", &v, Some(&facts(Ok(None))));
        let by = |c: &str| f.iter().find(|x| x.check == c).unwrap();
        assert_eq!(by("comet").level, Level::Warn);
        assert!(
            by("comet")
                .hint
                .as_deref()
                .unwrap()
                .contains("runtimes install comet")
        );
        assert_eq!(by("galaxy-login").level, Level::Warn);
        assert_eq!(by("galaxy-runtime").level, Level::Warn);
        assert_eq!(by("galaxy-port").level, Level::Info);
        assert!(by("galaxy-port").message.contains("comet, pid 7"));
        let bin = comet::CometBin {
            path: "/store/comet".into(),
            source: comet::Source::Store,
        };
        let ok = Facts {
            comet: Ok(Some(bin)),
            signed_in: Some(true),
            runtime_problem: None,
            port: None,
        };
        let f = findings("1", &v, Some(&ok));
        assert!(f.iter().all(|x| x.level == Level::Ok), "{f:?}");
        let err = findings("1", &v, Some(&facts(Err("refused".into()))));
        assert!(
            err.iter()
                .any(|x| x.check == "comet" && x.message.contains("refused"))
        );
    }

    #[test]
    fn the_view_lines() {
        let mut v = GalaxyView {
            on: true,
            wanted: true,
            source: Source::Own,
            recommended: false,
            sdk: Some(true),
            comet: Some("0.4.1".into()),
        };
        assert_eq!(v.line(), "comet on (0.4.1)");
        assert_eq!(v.sdk_line(), "shipped");
        v.comet = None;
        assert_eq!(v.line(), "comet on (not installed)");
        v.on = false;
        v.wanted = false;
        v.recommended = true;
        assert_eq!(v.line(), "off (compat recommends comet)");
        v.wanted = true;
        assert!(v.line().ends_with("the next setup makes it on"));
        v.sdk = None;
        assert_eq!(v.sdk_line(), "unknown");
    }
}
