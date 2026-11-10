//! A game's display mode on the Linux lane (#292, design 18 §7.9):
//! `gogo linux-app display <gogid> [wayland|x11] [--hdr …] [--monitor …] [--clear]`.
//!
//! `display`, `hdr` and `monitor` live in the game's L4 file under `[platforms.linux]`,
//! like `wrapper`. Setup writes them into `launch.conf` (`DISPLAY_MODE`,
//! `DISPLAY_HDR`, `DISPLAY_MONITOR`); the launcher checks for a Wayland socket at launch, where
//! the game's environment is, and sets GE-Proton's switches itself:
//!
//! - `wayland` with a socket: `PROTON_ENABLE_WAYLAND=1`, plus `hdr`'s switch.
//!   `monitor` (#295) becomes `PROTON_WAYLAND_MONITOR`, which GE-Proton's
//!   `proton` script copies to `WAYLANDDRV_PRIMARY_MONITOR`, the variable
//!   winewayland reads; unset, the script guesses with `xrandr`, which a
//!   session without an X server fails.
//!   With none (`WAYLAND_DISPLAY` unset, or no socket there) the game runs on
//!   X11 and the launcher logs why. GE-Proton would fall back too, silently.
//! - `x11`: `PROTON_ENABLE_WAYLAND` and `PROTON_USE_WAYLAND` are unset, so a
//!   session-wide `PROTON_ENABLE_WAYLAND=1` doesn't reach this game.
//! - neither: the launcher leaves the environment alone.
//!
//! Both go in before `launch.conf`'s `ENV`, so a player's own value in the L4
//! `env` still wins (as over the renderer's switches, §7.6).

use std::path::PathBuf;

use serde::Serialize;

use super::envargs::{self, EditOutcome};
use super::game::{self, ConfValues};
use super::manage;
use crate::macapp::compat::{self, DisplayMode, Effective, Hdr, Layer, Os, Sourced, UserEdit};
use crate::macapp::error::MacAppError;
use crate::macapp::launch::GameDirs;

/// GE-Proton's Wayland switch, and its alias.
pub const WAYLAND_KEYS: [&str; 2] = ["PROTON_ENABLE_WAYLAND", "PROTON_USE_WAYLAND"];

/// GE-Proton's primary-monitor switch (#295).
pub const MONITOR_KEY: &str = "PROTON_WAYLAND_MONITOR";

/// What the launcher exports for `hdr` and `monitor` when the game gets
/// Wine's Wayland driver. Nothing for `x11` (it unsets [`WAYLAND_KEYS`]).
pub fn wayland_env(hdr: Option<Hdr>, monitor: Option<&str>) -> Vec<(&'static str, String)> {
    let mut out = vec![(WAYLAND_KEYS[0], "1".to_string())];
    out.extend(
        hdr.and_then(Hdr::proton_env)
            .map(|(k, v)| (k, v.to_string())),
    );
    out.extend(monitor.map(|m| (MONITOR_KEY, m.to_string())));
    out
}

/// The mode and HDR switch, as set and as last applied.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DisplayView {
    /// What the next launch's setup writes, and its layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<Sourced<DisplayMode>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hdr: Option<Sourced<Hdr>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor: Option<Sourced<String>>,
    /// `launch.conf`'s `DISPLAY_MODE` / `DISPLAY_HDR` / `DISPLAY_MONITOR`:
    /// what the last setup wrote (`None`: no line).
    pub applied_mode: Option<DisplayMode>,
    pub applied_hdr: Option<Hdr>,
    pub applied_monitor: Option<String>,
    /// The next launch's setup changes them.
    pub pending: bool,
}

impl DisplayView {
    /// `None` when neither is set nor applied.
    pub fn of(eff: &Effective, conf: Option<&ConfValues>) -> Option<DisplayView> {
        let applied_mode = conf.and_then(|c| DisplayMode::from_key(c.get("DISPLAY_MODE")));
        let applied_hdr = conf.and_then(|c| Hdr::from_key(c.get("DISPLAY_HDR")));
        let applied_monitor = conf
            .map(|c| c.get("DISPLAY_MONITOR"))
            .filter(|m| !m.is_empty())
            .map(str::to_string);
        if eff.display.is_none()
            && eff.hdr.is_none()
            && eff.monitor.is_none()
            && applied_mode.is_none()
            && applied_hdr.is_none()
            && applied_monitor.is_none()
        {
            return None;
        }
        let mode = eff.display.clone();
        let hdr = eff.hdr.clone();
        let monitor = eff.monitor.clone();
        // `hdr = "auto"` writes no line, as no `hdr` does.
        let want_hdr = hdr.as_ref().map(|h| h.value).filter(|h| *h != Hdr::Auto);
        let pending = conf.is_some()
            && (mode.as_ref().map(|m| m.value) != applied_mode
                || want_hdr != applied_hdr
                || monitor.as_ref().map(|m| &m.value) != applied_monitor.as_ref());
        Some(DisplayView {
            mode,
            hdr,
            monitor,
            applied_mode,
            applied_hdr,
            applied_monitor,
            pending,
        })
    }

    /// One line for humans: `wayland (L4 user override), hdr force (…)`.
    pub fn describe(&self) -> String {
        let mut out = match &self.mode {
            Some(m) => format!("{} ({})", m.value.key(), m.layer.describe()),
            None => "not set: the environment decides".to_string(),
        };
        if let Some(h) = &self.hdr {
            out.push_str(&format!(", hdr {} ({})", h.value.key(), h.layer.describe()));
        }
        if let Some(m) = &self.monitor {
            out.push_str(&format!(", monitor {} ({})", m.value, m.layer.describe()));
        }
        out
    }
}

/// The session the command runs in, as the launcher would check it: the
/// launch's own when it starts from this shell (a menu's is the desktop's).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Session {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wayland_display: Option<String>,
    /// A socket is where `WAYLAND_DISPLAY` points.
    pub socket: bool,
}

impl Session {
    /// From an env lookup (`WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`), as the
    /// launcher's `wayland_socket` does it.
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Session {
        use std::os::unix::fs::FileTypeExt as _;
        let wayland_display = var("WAYLAND_DISPLAY").filter(|v| !v.is_empty());
        let socket = wayland_display.as_ref().is_some_and(|d| {
            let path = if d.starts_with('/') {
                PathBuf::from(d)
            } else {
                PathBuf::from(var("XDG_RUNTIME_DIR").unwrap_or_default()).join(d)
            };
            std::fs::metadata(path).is_ok_and(|m| m.file_type().is_socket())
        });
        Session {
            wayland_display,
            socket,
        }
    }

    pub fn detect() -> Session {
        Session::from_env(|k| std::env::var(k).ok())
    }

    /// What the launcher would pick for `mode` here.
    pub fn picks(&self, mode: DisplayMode) -> DisplayMode {
        match mode {
            DisplayMode::Wayland if self.socket => DisplayMode::Wayland,
            _ => DisplayMode::X11,
        }
    }

    pub fn describe(&self) -> String {
        match (&self.wayland_display, self.socket) {
            (None, _) => "WAYLAND_DISPLAY is unset".into(),
            (Some(d), true) => format!("WAYLAND_DISPLAY={d}, socket found"),
            (Some(d), false) => format!("no Wayland socket at WAYLAND_DISPLAY={d}"),
        }
    }
}

/// What doesn't add up: `hdr` without Wayland mode, gamescope taking the
/// socket away, the L4 env saying the opposite of the mode.
pub fn conflicts(gogid: &str, eff: &Effective) -> Vec<String> {
    let mut out = Vec::new();
    let mode = eff.display.as_ref().map(|m| m.value);
    if let Some(h) = &eff.hdr
        && h.value != Hdr::Auto
        && mode != Some(DisplayMode::Wayland)
    {
        out.push(format!(
            "hdr {} ({}) applies only in Wayland mode: `gogo linux-app display {gogid} wayland`",
            h.value.key(),
            h.layer.describe()
        ));
    }
    if let Some(m) = &eff.monitor
        && mode != Some(DisplayMode::Wayland)
    {
        out.push(format!(
            "monitor {} ({}) applies only in Wayland mode: `gogo linux-app display {gogid} wayland`",
            m.value,
            m.layer.describe()
        ));
    }
    if let Some(m) = &eff.monitor
        && let Some(e) = eff.env.get(MONITOR_KEY)
        && e.value != m.value
    {
        out.push(format!(
            "env {MONITOR_KEY}={:?} ({}) wins over monitor {}",
            e.value,
            e.layer.describe(),
            m.value
        ));
    }
    if mode == Some(DisplayMode::Wayland)
        && let Some(w) = &eff.wrapper
        && envargs::wrapper_runs(&w.value, "gamescope")
        && !w.value.iter().any(|a| a == "--expose-wayland")
    {
        out.push(
            "the wrapper runs gamescope without --expose-wayland: gamescope unsets \
             WAYLAND_DISPLAY for the game, which then runs on gamescope's Xwayland"
                .into(),
        );
    }
    for k in WAYLAND_KEYS {
        let Some(v) = eff.env.get(k) else { continue };
        let on = !matches!(v.value.trim(), "" | "0");
        let says = match (mode, on) {
            (Some(DisplayMode::Wayland), false) => "turns Wayland mode off",
            (Some(DisplayMode::X11), true) => "turns Wayland on over `display = \"x11\"`",
            (None, _) => {
                "is set by hand: `display` is gogo's switch for it, which checks for a \
                 Wayland socket and shows in doctor"
            }
            _ => continue,
        };
        out.push(format!(
            "env {k}={:?} ({}) {says}",
            v.value,
            v.layer.describe()
        ));
    }
    out
}

/// `gogo linux-app display <gogid>`.
#[derive(Debug, Clone, Serialize)]
pub struct DisplayReport {
    pub gogid: String,
    pub user_file: PathBuf,
    pub built: bool,
    /// `None`: no mode or HDR switch set or applied.
    pub display: Option<DisplayView>,
    /// What the launcher exports on Wayland (with the mode set to wayland).
    pub wayland_env: Vec<(String, String)>,
    pub session: Session,
    /// What the launcher would pick in this session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picks: Option<DisplayMode>,
    pub warnings: Vec<String>,
}

/// The game's display mode. Reads only.
pub fn display_report(gogid: &str) -> Result<DisplayReport, MacAppError> {
    let r = manage::compat(gogid)?;
    let dirs = GameDirs::for_game(gogid)?;
    let conf = ConfValues::read(&dirs.launch_conf).ok();
    let session = Session::detect();
    let view = DisplayView::of(&r.effective, conf.as_ref());
    let mode = r.effective.display.as_ref().map(|m| m.value);
    let hdr = r.effective.hdr.as_ref().map(|h| h.value);
    let monitor = r.effective.monitor.as_ref().map(|m| m.value.as_str());
    let mut warnings = conflicts(gogid, &r.effective);
    warnings.extend(r.warnings);
    Ok(DisplayReport {
        gogid: gogid.to_string(),
        user_file: r.user_file,
        built: r.built,
        display: view,
        wayland_env: match mode {
            Some(DisplayMode::Wayland) => wayland_env(hdr, monitor)
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            _ => vec![],
        },
        picks: mode.map(|m| session.picks(m)),
        session,
        warnings,
    })
}

/// `gogo linux-app display <gogid> …`: `mode`, `hdr` and `monitor` are
/// `None` to leave as they are, `Some(None)` to drop from L4.
pub fn set_display(
    gogid: &str,
    mode: Option<Option<DisplayMode>>,
    hdr: Option<Option<Hdr>>,
    monitor: Option<Option<String>>,
) -> Result<EditOutcome, MacAppError> {
    compat::validate_gogid(gogid)?;
    let mut edits = Vec::new();
    if let Some(m) = mode {
        edits.push(UserEdit::Display(m));
    }
    if let Some(h) = hdr {
        edits.push(UserEdit::Hdr(h));
    }
    if let Some(m) = monitor {
        edits.push(UserEdit::Monitor(m));
    }
    let changes = compat::edit_user_platform_for(gogid, &edits, Os::Linux)?;
    let mut warnings = Vec::new();
    if let Ok(r) = manage::compat(gogid) {
        warnings = conflicts(gogid, &r.effective);
        let eff = &r.effective;
        for (key, layer) in [
            ("display", eff.display.as_ref().map(|m| m.layer)),
            ("hdr", eff.hdr.as_ref().map(|h| h.layer)),
            ("monitor", eff.monitor.as_ref().map(|m| m.layer)),
        ] {
            if let Some(l) = layer.filter(|l| *l != Layer::User) {
                warnings.push(format!("`{key}` still comes from the {}", l.describe()));
            }
        }
        if eff.display.as_ref().map(|m| m.value) == Some(DisplayMode::Wayland) {
            let s = Session::detect();
            if !s.socket {
                warnings.push(format!(
                    "{} here: a launch from this shell would run on X11 (a desktop launch has \
                     the session's)",
                    s.describe()
                ));
            }
        }
    }
    Ok(EditOutcome {
        gogid: gogid.to_string(),
        file: compat::user_path(gogid),
        changes,
        warnings,
        built: game::launch_toml_path(&GameDirs::for_game(gogid)?).is_file(),
    })
}

/// The last run's display, from its log's header (`# display:`, `# hdr:`,
/// `# monitor:`)
/// and the game step's line under a wrapper.
pub fn last_run(log: &str) -> Vec<String> {
    log.lines()
        .take_while(|l| !l.starts_with("# exit:"))
        .filter_map(|l| {
            l.strip_prefix("# display: ")
                .map(|v| format!("display {v}"))
                .or_else(|| l.strip_prefix("# hdr: ").map(|v| format!("hdr {v}")))
                .or_else(|| {
                    l.strip_prefix("# monitor: ")
                        .map(|v| format!("monitor {v}"))
                })
                .or_else(|| {
                    l.strip_prefix("gogo: display under the wrapper: ")
                        .map(|v| format!("under the wrapper: {v}"))
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eff(raw: &str) -> Effective {
        let e = compat::parse(raw, Os::Linux, Layer::User).unwrap().entry;
        compat::merge(&[(Layer::User, &e)])
    }

    #[test]
    fn the_wayland_env_follows_the_hdr_switch() {
        let e = |h, m| {
            wayland_env(h, m)
                .into_iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(e(None, None), ["PROTON_ENABLE_WAYLAND=1"]);
        assert_eq!(e(Some(Hdr::Auto), None), ["PROTON_ENABLE_WAYLAND=1"]);
        assert_eq!(
            e(Some(Hdr::On), None),
            ["PROTON_ENABLE_WAYLAND=1", "PROTON_ENABLE_HDR=1"]
        );
        assert_eq!(
            e(Some(Hdr::Force), None),
            ["PROTON_ENABLE_WAYLAND=1", "PROTON_FORCE_HDR=1"]
        );
        assert_eq!(
            e(Some(Hdr::Off), Some("DP-1")),
            [
                "PROTON_ENABLE_WAYLAND=1",
                "PROTON_ENABLE_HDR=0",
                "PROTON_WAYLAND_MONITOR=DP-1"
            ]
        );
    }

    #[test]
    fn the_view_is_pending_until_launch_conf_has_it() {
        assert_eq!(DisplayView::of(&eff(""), None), None);
        let e = eff("[platforms.linux]\ndisplay = \"wayland\"\nhdr = \"force\"\n");
        let v = DisplayView::of(&e, None).unwrap();
        assert!(!v.pending, "not built: nothing to be pending against");
        let stale = ConfValues::parse("ARGS=()\n");
        assert!(DisplayView::of(&e, Some(&stale)).unwrap().pending);
        let done = ConfValues::parse("DISPLAY_MODE='wayland'\nDISPLAY_HDR='force'\n");
        let v = DisplayView::of(&e, Some(&done)).unwrap();
        assert!(!v.pending);
        assert_eq!(v.applied_mode, Some(DisplayMode::Wayland));
        assert_eq!(
            v.describe(),
            "wayland (L4 user override), hdr force (L4 user override)"
        );
        // Cleared, but launch.conf still has it: shown, and pending.
        let v = DisplayView::of(&eff(""), Some(&done)).unwrap();
        assert!(v.pending);
        assert_eq!(v.mode, None);
        // `auto` is no line.
        let e = eff("[platforms.linux]\ndisplay = \"x11\"\nhdr = \"auto\"\n");
        let conf = ConfValues::parse("DISPLAY_MODE='x11'\n");
        assert!(!DisplayView::of(&e, Some(&conf)).unwrap().pending);
    }

    #[test]
    fn bad_values_are_errors_and_macos_ignores_them() {
        assert!(
            compat::parse(
                "[platforms.linux]\ndisplay = \"wl\"\n",
                Os::Linux,
                Layer::User
            )
            .is_err()
        );
        assert!(compat::parse("[platforms.linux]\nhdr = true\n", Os::Linux, Layer::User).is_err());
        for bad in ["\"\"", "\"DP 1\"", "3", "\"a\\nb\""] {
            let raw = format!("[platforms.linux]\nmonitor = {bad}\n");
            assert!(
                compat::parse(&raw, Os::Linux, Layer::User).is_err(),
                "{bad}"
            );
        }
        let p = compat::parse(
            "[platforms.macos]\ndisplay = \"wayland\"\n",
            Os::Macos,
            Layer::User,
        )
        .unwrap();
        assert_eq!(p.entry.display, None);
        assert_eq!(
            p.warnings,
            ["`platforms.macos.display` is Linux only: ignored"]
        );
    }

    #[test]
    fn conflicts_name_what_undoes_the_mode() {
        assert!(conflicts("1", &eff("[platforms.linux]\ndisplay = \"wayland\"\n")).is_empty());
        let c = conflicts("1", &eff("[platforms.linux]\nhdr = \"on\"\n"));
        assert!(
            c[0].starts_with("hdr on (L4 user override) applies only in Wayland mode"),
            "{c:?}"
        );
        assert!(conflicts("1", &eff("[platforms.linux]\nhdr = \"auto\"\n")).is_empty());
        let c = conflicts("1", &eff("[platforms.linux]\nmonitor = \"DP-1\"\n"));
        assert!(
            c[0].starts_with("monitor DP-1 (L4 user override) applies only in Wayland mode"),
            "{c:?}"
        );
        let wl = "[platforms.linux]\ndisplay = \"wayland\"\nmonitor = \"DP-1\"\n";
        assert!(conflicts("1", &eff(wl)).is_empty());
        let c = conflicts(
            "1",
            &eff(&format!(
                "{wl}[platforms.linux.env]\nPROTON_WAYLAND_MONITOR = \"HDMI-A-1\"\n"
            )),
        );
        assert!(c[0].contains("wins over monitor DP-1"), "{c:?}");
        let c = conflicts(
            "1",
            &eff(
                "[platforms.linux]\ndisplay = \"wayland\"\nwrapper = [\"gamemoderun\", \"gamescope\", \"-f\", \"--\"]\n",
            ),
        );
        assert!(c[0].contains("without --expose-wayland"), "{c:?}");
        assert!(
            conflicts("1", &eff(
                "[platforms.linux]\ndisplay = \"wayland\"\nwrapper = [\"gamescope\", \"--expose-wayland\", \"--\"]\n"
            ))
            .is_empty()
        );
        let c = conflicts(
            "1",
            &eff(
                "[platforms.linux]\ndisplay = \"x11\"\n[platforms.linux.env]\nPROTON_ENABLE_WAYLAND = \"1\"\n",
            ),
        );
        assert!(c[0].contains("turns Wayland on"), "{c:?}");
        let c = conflicts(
            "1",
            &eff(
                "[platforms.linux]\ndisplay = \"wayland\"\n[platforms.linux.env]\nPROTON_USE_WAYLAND = \"0\"\n",
            ),
        );
        assert!(c[0].contains("turns Wayland mode off"), "{c:?}");
        let c = conflicts(
            "1",
            &eff("[platforms.linux.env]\nPROTON_ENABLE_WAYLAND = \"1\"\n"),
        );
        assert!(c[0].contains("is gogo's switch"), "{c:?}");
    }

    #[test]
    fn the_session_check_is_the_launchers() {
        let tmp = tempfile::tempdir().unwrap();
        let _l = std::os::unix::net::UnixListener::bind(tmp.path().join("wayland-1")).unwrap();
        std::fs::write(tmp.path().join("wayland-2"), "").unwrap();
        let dir = tmp.path().to_str().unwrap().to_string();
        let at = |wd: Option<&str>| {
            let dir = dir.clone();
            let wd = wd.map(str::to_string);
            Session::from_env(move |k| match k {
                "WAYLAND_DISPLAY" => wd.clone(),
                "XDG_RUNTIME_DIR" => Some(dir.clone()),
                _ => None,
            })
        };
        let s = at(Some("wayland-1"));
        assert!(s.socket);
        assert_eq!(s.picks(DisplayMode::Wayland), DisplayMode::Wayland);
        assert_eq!(s.picks(DisplayMode::X11), DisplayMode::X11);
        let abs = format!("{dir}/wayland-1");
        assert!(at(Some(&abs)).socket);
        // A file that isn't a socket, a missing one, none at all.
        for s in [
            at(Some("wayland-2")),
            at(Some("wayland-9")),
            at(None),
            at(Some("")),
        ] {
            assert!(!s.socket, "{s:?}");
            assert_eq!(s.picks(DisplayMode::Wayland), DisplayMode::X11);
        }
        assert_eq!(at(None).describe(), "WAYLAND_DISPLAY is unset");
        assert_eq!(
            at(Some("wayland-9")).describe(),
            "no Wayland socket at WAYLAND_DISPLAY=wayland-9"
        );
    }

    #[test]
    fn the_last_run_is_read_from_its_header() {
        let log = "# launcher: 12\n# wrapper: gamescope --\n# display: x11 (wayland wanted, but WAYLAND_DISPLAY is unset)\n\
                   # hdr: force (not applied: not on Wayland)\n# monitor: DP-1 (not applied: not on Wayland)\n# ---\ngogo: display under the wrapper: x11\nout\n# exit: 0\n";
        assert_eq!(
            last_run(log),
            [
                "display x11 (wayland wanted, but WAYLAND_DISPLAY is unset)",
                "hdr force (not applied: not on Wayland)",
                "monitor DP-1 (not applied: not on Wayland)",
                "under the wrapper: x11",
            ]
        );
        assert!(last_run("# launcher: 12\n# ---\n").is_empty());
    }
}
