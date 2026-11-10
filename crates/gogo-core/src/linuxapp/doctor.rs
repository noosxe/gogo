//! `gogo linux-app doctor <gogid>` (design 18 §8, #178): can this game work
//! on this Linux machine? The Mac doctor's findings (ok / warn / fail, one
//! line each with a fix hint) from read-only checks of the game's files,
//! umu, the Proton build, the Steam Linux Runtime, the renderer, the host's
//! Vulkan drivers and the last launch.
//!
//! Read-only: it never launches the game, never runs umu or Proton (umu-run
//! exits 0 on a broken `PROTONPATH`, so running it would prove nothing),
//! and never stops a process. Everything that looks at the machine goes
//! through a [`Host`], so tests run each check against a fake root.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::game::{self, ConfValues, LinuxState};
use super::proton::{self, BuildProblem, Expect, ProtonRef};
use super::{launcher, manage, setup, umu};
use crate::config::{Config, UmuMode};
use crate::macapp::compat::{self, Os, Status};
use crate::macapp::doctor::{Finding, Level, Report, check_log, normalize, walk_links};
use crate::macapp::error::MacAppError;
use crate::macapp::launch::{GameDirs, LaunchToml};
use crate::macapp::pe::{Bitness, PeScan, ScanError};
use crate::macapp::renderer::{self, PROTON_USE_D7VK, Renderer};
use crate::macapp::{catalog, ddraw, store};

/// The Steam Linux Runtime folder umu keeps for each `require_tool_appid`
/// it knows: the variant column of `RUNTIME_VERSIONS` in umu 1.4.4's
/// `umu_runtime.py` (checked against the pinned zipapp, 2026-11-05). The
/// appids are `catalog::umu_runtimes()`; a unit test keeps the two lists
/// together.
pub const CONTAINERS: [(&str, &str); 4] = [
    ("1391110", "steamrt2"),
    ("1628350", "steamrt3"),
    ("4183110", "steamrt4"),
    ("4185400", "steamrt4-arm64"),
];

/// What the first launch downloads for steamrt4 (design 18 §2.3).
const CONTAINER_DOWNLOAD: &str = "175 MB";

/// An environment lookup.
pub type EnvFn = Box<dyn Fn(&str) -> Option<OsString>>;

/// The machine as the checks see it. [`Host::detect`] outside tests.
pub struct Host {
    /// `/` on a real machine: `etc/NIXOS`, `proc/sys/…`, `sys/class/drm`
    /// and the Vulkan ICD dirs are looked up under it.
    pub root: PathBuf,
    /// Environment lookup.
    pub env: EnvFn,
    /// Where the launcher writes run logs: `<logs>/<gogid>/`.
    pub logs: PathBuf,
    /// Run `vulkaninfo --summary` when it's on `PATH` (it only lists the
    /// devices; off in tests).
    pub vulkaninfo: bool,
}

impl Host {
    pub fn detect() -> Host {
        Host {
            root: PathBuf::from("/"),
            env: Box::new(|k| std::env::var_os(k)),
            logs: crate::state::logs_root(),
            vulkaninfo: true,
        }
    }

    pub(crate) fn var(&self, key: &str) -> Option<OsString> {
        (self.env)(key).filter(|v| !v.is_empty())
    }

    /// An absolute host path under [`Host::root`].
    pub(crate) fn at(&self, abs: impl AsRef<Path>) -> PathBuf {
        let p = abs.as_ref();
        self.root.join(p.strip_prefix("/").unwrap_or(p))
    }

    /// Can a launch run `program`: an executable file at that absolute path,
    /// or by that name in one of `PATH`'s folders?
    pub(crate) fn finds_program(&self, program: &str) -> bool {
        use std::os::unix::fs::PermissionsExt as _;
        let runs = |p: PathBuf| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        };
        if program.starts_with('/') {
            return runs(self.at(program));
        }
        self.var("PATH").is_some_and(|path| {
            std::env::split_paths(&path).any(|d| runs(self.at(d.join(program))))
        })
    }

    fn umu(&self) -> umu::Host<'_> {
        umu::Host {
            root: &self.root,
            env: &*self.env,
        }
    }

    fn is_nixos(&self) -> bool {
        self.umu().is_nixos()
    }
}

/// What the checks share: the game's files as they are on disk.
struct Game {
    gogid: String,
    dirs: GameDirs,
    baked: Option<(LaunchToml, Vec<u8>)>,
    conf: Option<ConfValues>,
    state: Option<LinuxState>,
    /// `DATA_ROOT` when that dir exists, else this gogo's data root: what
    /// `launch.sh` resolves relative paths against.
    data_root: PathBuf,
}

impl Game {
    /// A `launch.conf` path, absolute.
    fn path(&self, value: &str) -> PathBuf {
        let p = Path::new(value);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.data_root.join(p)
        }
    }

    fn setup_hint(&self) -> String {
        format!("gogo linux-app setup {}", self.gogid)
    }
}

/// Run every check for `gogid`. Errors only for an id that isn't one;
/// everything else is a finding.
pub fn doctor(gogid: &str, host: &Host) -> Result<Report, MacAppError> {
    compat::validate_gogid(gogid)?;
    let dirs = GameDirs::for_game(gogid)?;
    let mut f = Vec::new();
    f.extend(crate::macapp::doctor::check_location(gogid, &dirs));
    f.extend(crate::macapp::doctor::check_interrupted_move(gogid));

    let baked = game::read_launch_toml(&dirs);
    let conf = ConfValues::read(&dirs.launch_conf).ok();
    let state = match LinuxState::load(&dirs.state_toml) {
        Ok(s) => s,
        Err(e) => {
            f.push(Finding::fail(
                "stamp",
                e.to_string(),
                format!("gogo linux-app setup {gogid}"),
            ));
            None
        }
    };
    let data_root = conf
        .as_ref()
        .map(|c| PathBuf::from(c.get("DATA_ROOT")))
        .filter(|d| d.is_absolute() && d.is_dir())
        .unwrap_or_else(crate::state::data_root);
    let g = Game {
        gogid: gogid.to_string(),
        dirs,
        baked: None,
        conf,
        state,
        data_root,
    };
    let g = Game {
        baked: check_launcher(&g, baked, &mut f),
        ..g
    };
    let installed = manage::InstallScan::default();
    if g.baked.is_some() {
        f.push(check_compat(&g, &installed));
        f.push(check_launch(&g, host, &installed));
        f.push(check_display(&g, host, &installed));
    }
    check_umu(&g, host, &mut f);
    let build = check_runtime(&g, &mut f);
    if let Some((_, appid)) = &build {
        f.push(check_container(&g, host, appid.as_deref()));
    }
    let scan = g.baked.as_ref().map(|(t, _)| installed.get(&g.dirs, t));
    if let (Some(_), Some((dir, _))) = (&g.state, &build) {
        f.push(check_renderer(&g, dir, scan));
    }
    let icds = check_vulkan(host, &mut f);
    f.push(check_vulkan32(
        host,
        &icds,
        scan.map(|(_, s)| s),
        build.as_ref().map(|(d, _)| d.as_path()),
    ));
    if let Some((_, raw)) = &g.baked {
        f.push(check_stamp(&g, raw));
    }
    let proton_dir = build.as_ref().map(|(d, _)| d.clone()).or_else(|| {
        g.conf
            .as_ref()
            .map(|c| c.get("PROTON_DIR"))
            .filter(|d| !d.is_empty())
            .map(|d| g.path(d))
    });
    f.push(check_links(&g, proton_dir.as_deref()));
    f.push(check_processes(&g));
    f.push(check_log(&host.logs.join(gogid)));
    Ok(Report {
        gogid: gogid.to_string(),
        bundle: None,
        findings: f,
    })
}

// — launcher —

/// `gogo-launch.toml`, `launch.conf`, `launch.sh` (this gogo's template)
/// and the `.desktop` file whose `Exec=` runs it. Returns the baked TOML.
fn check_launcher(
    g: &Game,
    baked: Result<(LaunchToml, Vec<u8>), MacAppError>,
    f: &mut Vec<Finding>,
) -> Option<(LaunchToml, Vec<u8>)> {
    let build = format!("build it: gogo linux-app {}", g.gogid);
    let baked = match baked {
        Ok(b) => b,
        Err(_) if !g.dirs.root.is_dir() => {
            f.push(Finding::fail(
                "launcher",
                format!("{} isn't built (no {})", g.gogid, g.dirs.root.display()),
                build,
            ));
            return None;
        }
        Err(e) => {
            f.push(Finding::fail("launcher", e.to_string(), build));
            return None;
        }
    };
    let setup = g.setup_hint();
    let mut fails = Vec::new();
    let mut warns = Vec::new();
    if g.conf.is_none() {
        fails.push(format!("no {}", g.dirs.launch_conf.display()));
    }
    let sh = g.dirs.root.join(game::LAUNCH_SH);
    let mut version = None;
    match std::fs::read_to_string(&sh) {
        Err(_) => fails.push(format!("no {}", sh.display())),
        Ok(script) => {
            use std::os::unix::fs::PermissionsExt as _;
            if std::fs::metadata(&sh).is_ok_and(|m| m.permissions().mode() & 0o111 == 0) {
                fails.push(format!("{} isn't executable", sh.display()));
            }
            version = launcher::version_of(&script).map(str::to_string);
            let ours = launcher::LAUNCHER_VERSION.to_string();
            match version.as_deref() {
                Some(v) if v != ours => warns.push(format!(
                    "launch.sh is launcher {v}, this gogo writes {ours} (the next setup rewrites it)"
                )),
                None => warns.push("launch.sh has no LAUNCHER_VERSION".into()),
                Some(_) if script != launcher::launch_sh() => warns.push(
                    "launch.sh differs from this gogo's launcher (the next setup rewrites it)"
                        .into(),
                ),
                Some(_) => {}
            }
        }
    }
    let desktop = game::desktop_path(&g.gogid);
    let want = launcher::exec_value(&crate::macapp::launch::absolute(&sh));
    match std::fs::read_to_string(&desktop) {
        Err(_) => fails.push(format!("no menu entry {}", desktop.display())),
        Ok(entry) => match entry.lines().find_map(|l| l.strip_prefix("Exec=")) {
            Some(exec) if exec == want => {}
            Some(exec) => fails.push(format!(
                "{}'s Exec={exec} doesn't run {}",
                desktop.display(),
                sh.display()
            )),
            None => fails.push(format!("{} has no Exec= line", desktop.display())),
        },
    }
    f.push(if !fails.is_empty() {
        let mut all = fails;
        all.extend(warns);
        Finding::fail("launcher", all.join("; "), setup)
    } else if !warns.is_empty() {
        Finding::warn("launcher", warns.join("; "), setup)
    } else {
        Finding::ok(
            "launcher",
            format!(
                "{} ({}), launcher {}, menu entry {}",
                g.dirs.root.display(),
                baked.0.title,
                version.unwrap_or_default(),
                desktop.display()
            ),
        )
    });
    Some(baked)
}

// — compat —

/// The Linux status (§5) and the fix summary (§4.3), collisions marked.
fn check_compat(g: &Game, scan: &manage::InstallScan) -> Finding {
    let hint = format!("gogo linux-app compat {}", g.gogid);
    let r = match manage::compat_with(&g.gogid, scan) {
        Ok(r) => r,
        Err(e) => {
            return Finding::fail(
                "compat",
                e.to_string(),
                format!("fix or remove {}", compat::user_path(&g.gogid).display()),
            );
        }
    };
    let mut finding = if r.effective.status_for(None) == Status::Broken {
        Finding::warn(
            "compat",
            format!("{}: it may not run at all", r.status_line),
            hint,
        )
    } else if !r.collisions.is_empty() {
        Finding::warn(
            "compat",
            format!(
                "{}; {} of gogo's values overridden by the protonfix at launch",
                r.status_line,
                r.collisions.len()
            ),
            format!(
                "{hint}; `protonfixes = false` under [platforms.linux] in {} turns the \
                 protonfix off",
                r.user_file.display()
            ),
        )
    } else {
        Finding::ok("compat", r.status_line.clone())
    };
    finding.detail = r
        .warnings
        .iter()
        .map(|w| format!("warning: {w}"))
        .chain(r.fixes.iter().flat_map(|s| s.lines().map(str::to_string)))
        .collect();
    finding
}

// — env and args —

/// What the game gets at launch beyond the exe (#234): the env, each value
/// with its source, the args and the wrapper (#271). Warns when a value of
/// the user's never applies (a bad name, a key the launcher sets, a
/// protonfix that wins, a wrapper setup skips) and when the wrapper's program
/// isn't where the launch would look: this PATH, which is the launch's own
/// when it starts from this shell; a menu's is the session's.
fn check_launch(g: &Game, host: &Host, scan: &manage::InstallScan) -> Finding {
    use super::envargs::{self, EnvSource, EnvState};
    let hint = format!("gogo linux-app env list {}", g.gogid);
    let (env, args) = match (
        envargs::env_report_with(&g.gogid, false, scan),
        envargs::args_report_with(&g.gogid, scan),
    ) {
        (Ok(e), Ok(a)) => (e, a),
        (Err(e), _) | (_, Err(e)) => return Finding::fail("launch", e.to_string(), hint),
    };
    let user = compat::Layer::User;
    let mine: Vec<&envargs::EnvLine> = env.user_lines().collect();
    let mut problems: Vec<String> = env
        .warnings
        .iter()
        .filter(|w| w.starts_with("env ") || w.starts_with("wrapper "))
        .cloned()
        .collect();
    for l in &mine {
        if let EnvState::Overridden { by } = &l.state {
            problems.push(format!("env {}: yours is overridden by {by}", l.key));
        }
    }
    let words = |a: &[String]| {
        if a.is_empty() {
            "(none)".to_string()
        } else {
            envargs::shell_words(a)
        }
    };
    let mut parts = vec![if mine.is_empty() {
        "no env of yours".to_string()
    } else {
        format!(
            "your env: {}",
            mine.iter()
                .map(|l| l.key.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }];
    parts.push(match &args.extra_args {
        Some(x) => format!("extra args {} ({})", words(&x.value), x.layer.describe()),
        None => "no extra args".into(),
    });
    let wrapper = env.wrapper.as_ref();
    let wanted = wrapper.and_then(|v| v.layer.map(|l| (&v.words, l)));
    if let Some((w, layer)) = wanted {
        parts.push(format!("wrapper {} ({})", words(w), layer.describe()));
        if let Some(program) = envargs::wrapper_program(w)
            && !host.finds_program(program)
        {
            problems.push(format!(
                "the wrapper's program `{program}` isn't on PATH: the launch stops and says so \
                 (a menu launch has your session's PATH, which may differ from this shell's)"
            ));
        }
    }
    if mine.iter().any(|l| l.state == EnvState::Pending)
        || args.pending
        || wrapper.is_some_and(|v| v.pending)
    {
        parts.push("some apply at the next launch".into());
    }
    let mut finding = if problems.is_empty() {
        Finding::ok("launch", parts.join("; "))
    } else {
        Finding::warn(
            "launch",
            format!("{}; {}", parts.join("; "), problems.join("; ")),
            hint,
        )
    };
    finding.detail = env
        .lines
        .iter()
        .filter(|l| !matches!(l.state, EnvState::Overridden { .. }))
        .map(|l| {
            let pending = if l.state == EnvState::Pending {
                ", pending"
            } else {
                ""
            };
            let mark = if l.source == (EnvSource::Layer { layer: user }) {
                "* "
            } else {
                ""
            };
            format!("{mark}{}={}  ({}{pending})", l.key, l.value, l.source)
        })
        .chain(std::iter::once(format!(
            "args: {}  (game: {})",
            words(&args.launch_args),
            args.game_args_from
        )))
        .chain(wanted.map(|(w, layer)| {
            format!(
                "wrapper: {}  ({}; outside umu-run, the game's env inside it)",
                words(w),
                layer.describe()
            )
        }))
        .collect();
    finding
}

// — display —

/// The display mode (#292, design 18 §7.9): what's set and from where, what
/// the launcher would pick in this session (the launch's own when it starts
/// from this shell; a menu's is the desktop's), what the last run got, and
/// what undoes it (`hdr` without Wayland mode, gamescope without
/// `--expose-wayland`, the env saying otherwise).
fn check_display(g: &Game, host: &Host, scan: &manage::InstallScan) -> Finding {
    use super::display::{self, DisplayView, Session};
    use compat::DisplayMode;
    let hint = format!("gogo linux-app display {}", g.gogid);
    let r = match manage::compat_with(&g.gogid, scan) {
        Ok(r) => r,
        Err(e) => return Finding::fail("display", e.to_string(), hint),
    };
    let eff = &r.effective;
    let var = |k: &str| host.var(k).map(|v| v.to_string_lossy().into_owned());
    let session = Session::from_env(|k| {
        var(k).map(|v| {
            // The socket is looked up under the host's root, as the rest is.
            if k == "XDG_RUNTIME_DIR" || v.starts_with('/') {
                host.at(&v).to_string_lossy().into_owned()
            } else {
                v
            }
        })
    });
    let view = DisplayView::of(eff, g.conf.as_ref());
    let mut problems = display::conflicts(&g.gogid, eff);
    let mut parts = vec![match &view {
        Some(v) => v.describe(),
        None => "no display mode: the environment decides (X11 through Xwayland unless \
                 PROTON_ENABLE_WAYLAND=1 reaches the game)"
            .to_string(),
    }];
    if let Some(m) = eff.display.as_ref().map(|m| m.value) {
        let picks = session.picks(m);
        parts.push(format!("here: {} ({})", picks.key(), session.describe()));
        if m == DisplayMode::Wayland && picks != DisplayMode::Wayland {
            problems.push(format!(
                "{}: a launch from this shell runs on X11 (a desktop launch has the session's)",
                session.describe()
            ));
        }
    }
    if view.as_ref().is_some_and(|v| v.pending) {
        parts.push("applies at the next launch".into());
    }
    let last = std::fs::read_to_string(host.logs.join(&g.gogid).join("latest.log"))
        .map(|log| display::last_run(&log))
        .unwrap_or_default();
    let mut finding = if problems.is_empty() {
        Finding::ok("display", parts.join("; "))
    } else {
        Finding::warn(
            "display",
            format!("{}; {}", parts.join("; "), problems.join("; ")),
            hint,
        )
    };
    finding.detail = last.into_iter().map(|l| format!("last run: {l}")).collect();
    finding
}

// — umu —

/// Is `python` gogo's FHS env (the flake's `gogo-umu-python`, or what
/// `GOGO_UMU_PYTHON` names)? It isn't run: running it starts bwrap.
fn is_fhs_env(host: &Host, python: &Path) -> bool {
    python.file_name().is_some_and(|n| n == "gogo-umu-python")
        || host
            .var(umu::PYTHON_ENV)
            .is_some_and(|v| Path::new(&v) == python)
}

fn check_umu(g: &Game, host: &Host, f: &mut Vec<Finding>) {
    let setup = g.setup_hint();
    let mode = umu::stored_at(&Config::path());
    let nixos = host.is_nixos();
    let flake = format!(
        "install gogo's Linux game package (nix profile install {}), or nixpkgs' \
         umu-launcher with {} = \"system\"",
        umu::FLAKE_PACKAGE,
        umu::KEY
    );
    let recorded = g.conf.as_ref().map(|c| {
        (
            c.get("UMU_PYTHON").to_string(),
            c.get("UMU_RUN").to_string(),
        )
    });
    match recorded {
        // Not set up yet: what setup would find.
        None => f.push(match mode {
            None => Finding::warn(
                "umu",
                format!("{} is unset: the build asks", umu::KEY),
                format!("gogo linux-app {}", g.gogid),
            ),
            Some(UmuMode::System) => match umu::resolve_system(&host.umu()) {
                Ok(p) => Finding::warn(
                    "umu",
                    format!("system umu-run {} (unpinned)", p.display()),
                    format!("{} = \"zipapp\" for gogo's pinned copy", umu::KEY),
                ),
                Err(e) => Finding::fail("umu", e.to_string(), "install umu-launcher"),
            },
            Some(UmuMode::Zipapp) => match umu::resolve_python(&host.umu()) {
                Ok(p) => Finding::ok("umu", format!("zipapp with {}", p.display())),
                Err(e) if nixos => Finding::fail("umu", e.to_string(), flake.clone()),
                Err(e) => Finding::fail("umu", e.to_string(), "install Python >= 3.10"),
            },
        }),
        Some((python, run)) if python.is_empty() => {
            let run = g.path(&run);
            f.push(if umu::is_executable(&run) {
                Finding::warn(
                    "umu",
                    format!("system umu-run {} (unpinned)", run.display()),
                    format!(
                        "{} = \"zipapp\" for gogo's pinned copy, then {setup}",
                        umu::KEY
                    ),
                )
            } else {
                Finding::fail(
                    "umu",
                    format!("the system umu-run {} is gone", run.display()),
                    format!(
                        "install umu-launcher (nixpkgs: umu-launcher), or {} = \"zipapp\"; \
                         then {setup}",
                        umu::KEY
                    ),
                )
            });
        }
        Some((python, run)) => {
            f.push(check_zipapp(&g.path(&run), &setup));
            let python = PathBuf::from(python);
            let nix_note = if nixos {
                " (NixOS: nix-ld isn't needed, the container carries its libraries)"
            } else {
                ""
            };
            f.push(if !umu::is_executable(&python) {
                let mut hint = format!("{setup} (it records the current one)");
                if nixos && host.var(umu::PYTHON_ENV).is_none() {
                    hint = format!("{flake}; then {setup}");
                }
                Finding::fail(
                    "umu",
                    format!(
                        "umu's Python {} is gone{}",
                        python.display(),
                        if python.starts_with("/nix/store") {
                            " (a Nix garbage collection removes old store paths)"
                        } else {
                            ""
                        }
                    ),
                    hint,
                )
            } else if is_fhs_env(host, &python) {
                Finding::ok(
                    "umu",
                    format!("Python: {} (gogo's FHS env){nix_note}", python.display()),
                )
            } else if nixos {
                Finding::fail(
                    "umu",
                    format!(
                        "on NixOS umu's zipapp needs gogo's FHS env, and {} is a bare Python: \
                         it can't start the Steam Linux Runtime",
                        python.display()
                    ),
                    flake.clone(),
                )
            } else {
                match umu::python_version(&python) {
                    Some(v) if v >= umu::MIN_PYTHON => Finding::ok(
                        "umu",
                        format!("Python {}.{}: {}", v.0, v.1, python.display()),
                    ),
                    v => Finding::fail(
                        "umu",
                        format!(
                            "umu's zipapp needs Python >= {}.{}; {} is {}",
                            umu::MIN_PYTHON.0,
                            umu::MIN_PYTHON.1,
                            python.display(),
                            v.map(|(a, b)| format!("{a}.{b}"))
                                .unwrap_or_else(|| "no Python".into())
                        ),
                        format!(
                            "install Python {}.{} or newer, then {setup}",
                            umu::MIN_PYTHON.0,
                            umu::MIN_PYTHON.1
                        ),
                    ),
                }
            });
        }
    }
    f.push(check_userns(host));
}

/// The store's zipapp at `run`: there, and from the pinned archive.
fn check_zipapp(run: &Path, setup: &str) -> Finding {
    let install = format!("gogo runtimes install umu, then {setup}");
    let Some(entry) = catalog::find(catalog::UMU) else {
        return Finding::fail("umu", "the catalog has no umu entry", install);
    };
    let pin = match &entry.discovery {
        Some(catalog::Discovery::Pinned(p)) => p,
        _ => return Finding::fail("umu", "the catalog's umu isn't pinned", install),
    };
    if !run.is_file() {
        return Finding::fail(
            "umu",
            format!("the zipapp {} is missing", run.display()),
            install,
        );
    }
    let installed =
        store::installed(catalog::UMU, None).filter(|(m, _)| run.starts_with(store::tree_root(m)));
    match installed {
        None => Finding::warn(
            "umu",
            format!(
                "{} isn't a zipapp gogo's store recorded (umu {} is the pin)",
                run.display(),
                pin.version
            ),
            install,
        ),
        Some((m, _)) if m.version == pin.version && !m.sha256.eq_ignore_ascii_case(pin.sha256) => {
            Finding::fail(
                "umu",
                format!(
                    "umu {} in the store has sha256 {}, the pin is {}",
                    m.version, m.sha256, pin.sha256
                ),
                format!("gogo runtimes prune umu, then {install}"),
            )
        }
        Some((m, _)) if m.version != pin.version => Finding::warn(
            "umu",
            format!("umu {} (zipapp); this gogo pins {}", m.version, pin.version),
            setup.to_string(),
        ),
        Some((m, _)) => Finding::ok("umu", format!("umu {} (zipapp, pinned)", m.version)),
    }
}

/// pressure-vessel runs bwrap: unprivileged user namespaces must work.
fn check_userns(host: &Host) -> Finding {
    let read = |p: &str| {
        std::fs::read_to_string(host.at(p))
            .ok()
            .map(|s| s.trim().to_string())
    };
    if read("/proc/sys/kernel/unprivileged_userns_clone").as_deref() == Some("0") {
        return Finding::fail(
            "umu",
            "unprivileged user namespaces are off (kernel.unprivileged_userns_clone = 0): \
             the Steam Linux Runtime's bwrap can't start",
            "sysctl kernel.unprivileged_userns_clone=1 (and keep it in /etc/sysctl.d)",
        );
    }
    let max = read("/proc/sys/user/max_user_namespaces");
    if max.as_deref() == Some("0") {
        return Finding::fail(
            "umu",
            "user namespaces are off (user.max_user_namespaces = 0): the Steam Linux \
             Runtime's bwrap can't start",
            "sysctl user.max_user_namespaces=<a few thousand> (and keep it in /etc/sysctl.d)",
        );
    }
    if read("/proc/sys/kernel/apparmor_restrict_unprivileged_userns").as_deref() == Some("1") {
        return Finding::warn(
            "umu",
            "AppArmor restricts unprivileged user namespaces \
             (kernel.apparmor_restrict_unprivileged_userns = 1): bwrap may be refused",
            "an AppArmor profile allowing userns for bwrap, or \
             sysctl kernel.apparmor_restrict_unprivileged_userns=0",
        );
    }
    Finding::ok(
        "umu",
        format!(
            "unprivileged user namespaces available{}",
            max.map(|m| format!(" (max {m})")).unwrap_or_default()
        ),
    )
}

// — runtime —

/// The Proton build `launch.conf` records (else the one setup would use):
/// there, unchanged since setup, gogo's marker intact and from the pin, and
/// an appid umu knows (R2). Returns its dir and appid.
fn check_runtime(g: &Game, f: &mut Vec<Finding>) -> Option<(PathBuf, Option<String>)> {
    let setup = g.setup_hint();
    let retarget = format!("gogo linux-app --retarget {} --runtime ge-proton", g.gogid);
    let reference = g
        .state
        .as_ref()
        .map(|s| s.runtime.clone())
        .or_else(|| {
            compat::load_user_for(&g.gogid, Os::Linux)
                .ok()?
                .entry
                .runtime
        })
        .or_else(|| g.baked.as_ref().map(|(t, _)| t.runtime.clone()))?;
    let parsed = ProtonRef::parse(&reference).ok();
    let (dir, relative) = match g.conf.as_ref().map(|c| c.get("PROTON_DIR")) {
        Some(d) if !d.is_empty() => (g.path(d), Path::new(d).is_relative()),
        _ => match parsed.as_ref().map(setup::find_proton) {
            Some(Ok(Some(b))) => (b.dir, false),
            Some(Ok(None)) => {
                f.push(Finding::fail(
                    "runtime",
                    format!("{reference} isn't installed"),
                    setup,
                ));
                return None;
            }
            Some(Err(e)) => {
                f.push(Finding::fail("runtime", e.to_string(), retarget));
                return None;
            }
            None => {
                f.push(Finding::fail(
                    "runtime",
                    format!("{reference} isn't a Proton ref"),
                    retarget,
                ));
                return None;
            }
        },
    };
    let managed = proton::is_managed(&dir) || relative;
    let catalog = matches!(parsed, Some(ProtonRef::Catalog { .. }));
    let fix = if catalog { &setup } else { &retarget };

    // The launch check (§2.2), against what setup recorded.
    let mut changed = false;
    if let Some(c) = &g.conf {
        let expect = Expect {
            version: c.get("PROTON_VERSION"),
            proton_sha256: c.get("PROTON_SHA"),
        };
        match proton::check_build(&dir, expect) {
            Ok(()) => {}
            Err(p @ BuildProblem::Missing(_)) => {
                f.push(Finding::fail(
                    "runtime",
                    format!("{reference}: {p}"),
                    fix.clone(),
                ));
                return None;
            }
            Err(p) if managed => {
                changed = true;
                f.push(Finding::fail(
                    "runtime",
                    format!("{reference} changed since setup: {p} (the next launch runs setup)"),
                    setup.clone(),
                ))
            }
            Err(p) => f.push(Finding::warn(
                "runtime",
                format!(
                    "{reference} changed since setup: {p} (unmanaged: the launcher logs it and \
                     launches)"
                ),
                setup.clone(),
            )),
        }
    } else if let Err(p @ BuildProblem::Missing(_)) = proton::check_build(
        &dir,
        Expect {
            version: "",
            proton_sha256: "",
        },
    ) {
        f.push(Finding::fail(
            "runtime",
            format!("{reference}: {p}"),
            fix.clone(),
        ));
        return None;
    }

    // gogo's marker (O1) and the pin it came from; a change the launch
    // check already named isn't named twice.
    if proton::is_managed(&dir) && !changed {
        match proton::check_managed(&dir) {
            Err(p) => f.push(Finding::fail(
                "runtime",
                format!("{reference} no longer matches gogo's marker: {p}"),
                setup.clone(),
            )),
            Ok(m) => {
                if let Some((id, version)) = m.catalog_ref.split_once('/')
                    && let Some(catalog::Discovery::Pinned(pin)) =
                        catalog::find(id).and_then(|e| e.discovery.as_ref())
                    && pin.version == version
                    && !pin.sha256.eq_ignore_ascii_case(&m.archive_sha256)
                {
                    f.push(Finding::fail(
                        "runtime",
                        format!(
                            "{}'s marker records archive sha256 {}, the catalog pins {}",
                            m.catalog_ref, m.archive_sha256, pin.sha256
                        ),
                        setup.clone(),
                    ));
                }
            }
        }
    }

    // R2: a container the pinned umu knows.
    let appid = match proton::require_tool_appid(&dir) {
        Ok(Some(a)) => {
            if let Err(why) = proton::check_appid(&a) {
                f.push(Finding::fail(
                    "runtime",
                    format!("{reference}: {why}: umu can't start it"),
                    if catalog {
                        "bump umu in gogo's catalog with this GE-Proton (one commit)".to_string()
                    } else {
                        retarget.clone()
                    },
                ));
            }
            Some(a)
        }
        Ok(None) => {
            f.push(Finding::warn(
                "runtime",
                format!(
                    "{reference}: {} names no require_tool_appid",
                    proton::TOOLMANIFEST
                ),
                retarget.clone(),
            ));
            None
        }
        Err(e) => {
            f.push(Finding::fail("runtime", e, fix.clone()));
            None
        }
    };

    let version = proton::version_of(&dir).unwrap_or_default();
    let shared = proton::is_managed(&dir) && !dir.starts_with(&g.data_root);
    f.push(if !managed {
        Finding::warn(
            "runtime",
            format!(
                "{reference} ({version}): unmanaged, not pinned or installed by gogo; it may \
                 change or vanish"
            ),
            retarget,
        )
    } else if shared {
        Finding::new(
            "runtime",
            Level::Warn,
            format!(
                "{reference} ({version}) in a shared tools dir, {}: gogo never prunes it there",
                dir.display()
            ),
        )
    } else {
        Finding::ok("runtime", format!("{reference} ({version})"))
    });
    Some((dir, appid))
}

// — container —

/// The Steam Linux Runtime the build needs, in umu's folder (design 18
/// §2.3): umu owns it, so only a warning when it isn't there yet.
fn check_container(g: &Game, host: &Host, appid: Option<&str>) -> Finding {
    let base = match g.conf.as_ref().map(|c| c.get("UMU_FOLDERS")) {
        Some(d) if !d.is_empty() => PathBuf::from(d).join("umu"),
        _ => host
            .var("UMU_FOLDERS_PATH")
            .map(|d| PathBuf::from(d).join("umu"))
            .or_else(|| {
                host.var("XDG_DATA_HOME")
                    .map(|d| PathBuf::from(d).join("umu"))
            })
            .or_else(|| {
                host.var("HOME")
                    .map(|h| PathBuf::from(h).join(".local/share/umu"))
            })
            .unwrap_or_else(|| PathBuf::from("umu")),
    };
    let present: Vec<String> = CONTAINERS
        .iter()
        .map(|(_, v)| *v)
        .filter(|v| base.join(v).join("toolmanifest.vdf").is_file())
        .map(str::to_string)
        .collect();
    let Some(variant) = appid.and_then(|a| CONTAINERS.iter().find(|(id, _)| *id == a)) else {
        return Finding::ok(
            "container",
            format!(
                "the build's runtime is unknown; umu has {} in {}",
                list_or_none(&present),
                base.display()
            ),
        );
    };
    let dir = base.join(variant.1);
    if present.iter().any(|p| p == variant.1) {
        Finding::ok(
            "container",
            format!(
                "{} in {} ({})",
                variant.1,
                dir.display(),
                crate::fmt::human_size(dir_size(&dir))
            ),
        )
    } else {
        Finding::new(
            "container",
            Level::Warn,
            format!(
                "{} (appid {}) isn't in {} yet: the first launch downloads it (about {} for \
                 steamrt4); umu has {}",
                variant.1,
                variant.0,
                base.display(),
                CONTAINER_DOWNLOAD,
                list_or_none(&present)
            ),
        )
    }
}

fn list_or_none(v: &[String]) -> String {
    if v.is_empty() {
        "none".into()
    } else {
        v.join(", ")
    }
}

/// Bytes under `dir`, symlinks not followed.
fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

// — renderer —

/// The renderer setup last applied, against the files it needs (§3): gogo's
/// cnc-ddraw in the dir of the exe that runs, D7VK in the build, never both
/// `ddraw.dll`s, DDrawCompat there unloaded, and any other game `ddraw.dll`
/// there with its `ddraw=n,b`.
fn check_renderer(g: &Game, build: &Path, scan: Option<&manage::InstalledScan>) -> Finding {
    let setup = g.setup_hint();
    let applied = g
        .state
        .as_ref()
        .map(|s| s.renderer.clone())
        .unwrap_or_default();
    let env = g
        .conf
        .as_ref()
        .and_then(|c| c.arrays.get("ENV"))
        .cloned()
        .unwrap_or_default();
    let switches: Vec<&String> = env.iter().filter(|e| e.starts_with("PROTON_")).collect();
    let d7vk_on = env.iter().any(|e| *e == format!("{PROTON_USE_D7VK}=1"));
    let overrides = g
        .conf
        .as_ref()
        .map(|c| c.get("DLL_OVERRIDES").to_string())
        .unwrap_or_default();
    let exe = g.conf.as_ref().map(|c| g.data_root.join(c.get("EXE")));
    let game_dir = scan
        .and_then(|(d, _)| d.as_deref())
        .map(|root| ddraw::dir_for(root, exe.as_deref()));
    let dll_in_game = game_dir.as_deref().is_some_and(ddraw::has_dll);
    let ddrawcompat = game_dir.as_deref().is_some_and(ddraw::is_ddrawcompat);
    let own = !ddrawcompat && game_dir.as_deref().is_some_and(ddraw::is_games);
    let dxguid = scan
        .and_then(|(_, s)| s.as_ref().ok())
        .is_some_and(|s| s.dxguid_block);
    let note = if dxguid {
        format!(
            "; L1's D3D≤7 evidence is only the dxguid GUID block, so the game may be 2D \
             (`gogo linux-app next-renderer {}` tries the next one)",
            g.gogid
        )
    } else {
        String::new()
    };

    if d7vk_on && dll_in_game {
        return Finding::fail(
            "renderer",
            format!(
                "{PROTON_USE_D7VK}=1 with a ddraw.dll in {}: the game dir's wins, D7VK never loads",
                game_dir
                    .as_ref()
                    .map(|d| d.display().to_string())
                    .unwrap_or_default()
            ),
            setup,
        );
    }
    let ddraw_set = overrides
        .split(';')
        .find_map(|o| o.strip_prefix("ddraw="))
        .map(str::to_string);
    if ddrawcompat && ddraw_set.as_deref().is_some_and(|v| v.starts_with('n')) {
        return Finding::fail(
            "renderer",
            format!(
                "launch.conf loads the game's DDrawCompat (ddraw={}), which doesn't run under \
                 Wine: the game exits at once",
                ddraw_set.as_deref().unwrap_or_default()
            ),
            format!(
                "drop ddraw from dll_overrides in {}, then {setup}",
                compat::user_path(&g.gogid).display()
            ),
        );
    }
    if own && ddraw_set.is_none() {
        return Finding::warn(
            "renderer",
            format!(
                "{} ships beside the exe with no ddraw=n,b override, so Wine's builtin ddraw \
                 runs instead",
                game_dir
                    .as_ref()
                    .map(|d| d.join("ddraw.dll").display().to_string())
                    .unwrap_or_default()
            ),
            format!("{setup} (it sets the override)"),
        );
    }
    if let Some(v) = ddraw_set.as_deref().filter(|v| own && !v.starts_with('n')) {
        return Finding::warn(
            "renderer",
            format!("ddraw={v} in launch.conf: the game's own ddraw.dll beside the exe isn't used"),
            format!(
                "drop the override from {} to use it",
                compat::user_path(&g.gogid).display()
            ),
        );
    }
    let r = match renderer::applied_renderer(&applied, Os::Linux) {
        Ok(r) => r,
        Err(e) => return Finding::fail("renderer", format!("{applied}: {e}"), setup),
    };
    match r {
        Renderer::CncDdraw if own => {}
        Renderer::CncDdraw => {
            let placed = dll_in_game && game_dir.as_deref().is_some_and(ddraw::is_gogo);
            if !placed {
                return Finding::fail(
                    "renderer",
                    "cnc-ddraw: gogo's ddraw.dll and ddraw.ini aren't in the game dir",
                    setup,
                );
            }
            if !overrides.split(';').any(|o| o == "ddraw=n,b") {
                return Finding::fail(
                    "renderer",
                    "cnc-ddraw: launch.conf has no ddraw=n,b override, so Wine's ddraw wins",
                    setup,
                );
            }
        }
        Renderer::D7vk if !renderer::has_d7vk(build) => {
            return Finding::fail(
                "renderer",
                format!("d7vk: {} has no {}", build.display(), renderer::D7VK_DLL),
                format!(
                    "gogo linux-app --retarget {} --runtime ge-proton (GE-Proton bundles D7VK), \
                     or --renderer wined3d",
                    g.gogid
                ),
            );
        }
        Renderer::D7vk if !d7vk_on => {
            return Finding::fail(
                "renderer",
                format!("d7vk: launch.conf doesn't set {PROTON_USE_D7VK}=1"),
                setup,
            );
        }
        _ => {}
    }
    let sets = if switches.is_empty() {
        "Proton's defaults".to_string()
    } else {
        switches
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let kept = if ddrawcompat {
        ", the game's DDrawCompat beside the exe left unloaded: it doesn't run under Wine"
    } else if own {
        ", the game's own ddraw.dll beside the exe with ddraw=n,b"
    } else {
        ""
    };
    if applied == renderer::PROTON_DEFAULT && switches.is_empty() {
        let desc = renderer::PROTON_DEFAULT_DESC;
        return Finding::ok("renderer", format!("{applied}: {desc}{kept}{note}"));
    }
    Finding::ok("renderer", format!("{applied} ({sets}{kept}){note}"))
}

// — vulkan, vulkan32 —

/// One Vulkan driver manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icd {
    pub manifest: PathBuf,
    pub library: String,
    /// 32 or 64; `None` when neither the manifest nor the library says.
    pub bits: Option<u8>,
}

/// The ICD manifests the Vulkan loader would read: `VK_DRIVER_FILES` /
/// `VK_ICD_FILENAMES` when set (they replace the search), else the
/// system dirs, NixOS's `/run/opengl-driver{,-32}` and `XDG_DATA_DIRS`.
fn find_icds(host: &Host) -> Vec<Icd> {
    let mut manifests = BTreeSet::new();
    let override_ = host
        .var("VK_DRIVER_FILES")
        .or_else(|| host.var("VK_ICD_FILENAMES"));
    let add_dir = |d: &Path, out: &mut BTreeSet<PathBuf>| {
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "json") && p.is_file() {
                out.insert(p);
            }
        }
    };
    if let Some(list) = override_ {
        for p in std::env::split_paths(&list) {
            let p = host.at(&p);
            if p.is_dir() {
                add_dir(&p, &mut manifests);
            } else if p.is_file() {
                manifests.insert(p);
            }
        }
    } else {
        let mut dirs: Vec<PathBuf> = [
            "/etc/vulkan/icd.d",
            "/usr/local/share/vulkan/icd.d",
            "/usr/share/vulkan/icd.d",
            "/run/opengl-driver/share/vulkan/icd.d",
            "/run/opengl-driver-32/share/vulkan/icd.d",
        ]
        .iter()
        .map(|d| host.at(d))
        .collect();
        if let Some(x) = host.var("XDG_DATA_DIRS") {
            dirs.extend(
                std::env::split_paths(&x)
                    .filter(|d| d.is_absolute())
                    .map(|d| host.at(d.join("vulkan/icd.d"))),
            );
        }
        for d in dirs {
            add_dir(&d, &mut manifests);
        }
    }
    // One per real file (NixOS links the same manifest into several dirs).
    let mut seen = BTreeSet::new();
    manifests
        .into_iter()
        .filter(|m| seen.insert(m.canonicalize().unwrap_or_else(|_| m.clone())))
        .filter_map(|m| read_icd(host, &m))
        .collect()
}

fn read_icd(host: &Host, manifest: &Path) -> Option<Icd> {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(manifest).ok()?).ok()?;
    let icd = v.get("ICD")?;
    let library = icd.get("library_path")?.as_str()?.to_string();
    let named = icd
        .get("library_arch")
        .and_then(|a| a.as_str())
        .and_then(|a| match a {
            "32" => Some(32),
            "64" => Some(64),
            _ => None,
        });
    let lib = Path::new(&library);
    let resolved = if lib.is_absolute() {
        Some(host.at(lib))
    } else if library.contains('/') {
        manifest.parent().map(|d| d.join(lib))
    } else {
        None
    };
    let bits = named
        .or_else(|| resolved.as_deref().and_then(elf_bits))
        .or_else(|| name_bits(manifest));
    Some(Icd {
        manifest: manifest.to_path_buf(),
        library,
        bits,
    })
}

/// An ELF's class: 32 or 64.
fn elf_bits(p: &Path) -> Option<u8> {
    use std::io::Read as _;
    let mut head = [0u8; 5];
    std::fs::File::open(p).ok()?.read_exact(&mut head).ok()?;
    match head {
        [0x7f, b'E', b'L', b'F', 1] => Some(32),
        [0x7f, b'E', b'L', b'F', 2] => Some(64),
        _ => None,
    }
}

/// Mesa names its manifests `<driver>_icd.<arch>.json`; NixOS keeps the
/// 32-bit ones under `/run/opengl-driver-32`.
fn name_bits(manifest: &Path) -> Option<u8> {
    let s = manifest.to_string_lossy();
    if s.contains("opengl-driver-32/") || s.contains(".i686.") || s.contains(".i386.") {
        Some(32)
    } else if s.contains("opengl-driver/") || s.contains(".x86_64.") || s.contains(".aarch64.") {
        Some(64)
    } else {
        None
    }
}

/// A GPU the kernel's DRM layer sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Gpu {
    card: String,
    pub(crate) vendor: String,
}

pub(crate) fn gpus(host: &Host) -> Vec<Gpu> {
    let drm = host.at("/sys/class/drm");
    let mut seen = BTreeSet::new();
    let mut out: Vec<Gpu> = std::fs::read_dir(&drm)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let card = e.file_name().to_str()?.to_string();
            let n = card.strip_prefix("card")?;
            if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let dev = e.path().join("device");
            let vendor = std::fs::read_to_string(dev.join("vendor")).ok()?;
            seen.insert(dev.canonicalize().unwrap_or(dev)).then(|| Gpu {
                card,
                vendor: match vendor.trim() {
                    "0x10de" => "NVIDIA".into(),
                    "0x1002" => "AMD".into(),
                    "0x8086" => "Intel".into(),
                    v => v.to_string(),
                },
            })
        })
        .collect();
    out.sort_by(|a, b| a.card.cmp(&b.card));
    out
}

/// `vulkaninfo --summary`'s device names, when it's on `PATH` and answers
/// within 10 s.
fn vulkaninfo_devices(host: &Host) -> Option<Vec<String>> {
    if !host.vulkaninfo {
        return None;
    }
    let path = host.var("PATH")?;
    let exe = std::env::split_paths(&path)
        .map(|d| d.join("vulkaninfo"))
        .find(|p| umu::is_executable(p))?;
    let mut child = Command::new(exe)
        .arg("--summary")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < Duration::from_secs(10) => {
                std::thread::sleep(Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.trim().strip_prefix("deviceName"))
            .map(|l| l.trim_start_matches([' ', '=']).trim().to_string())
            .collect(),
    )
}

/// A Vulkan driver at all (DXVK, VKD3D-Proton and D7VK need one), and one
/// GPU or a way to pick (R18).
fn check_vulkan(host: &Host, f: &mut Vec<Finding>) -> Vec<Icd> {
    let icds = find_icds(host);
    let nixos = host.is_nixos();
    if icds.is_empty() {
        f.push(Finding::fail(
            "vulkan",
            "no Vulkan driver (ICD manifest) found: DXVK, VKD3D-Proton and D7VK can't run, \
             only wined3d",
            if nixos {
                "hardware.graphics.enable = true (and your GPU's driver) in the NixOS config"
            } else {
                "install your GPU's Vulkan driver (Mesa's vulkan drivers, or NVIDIA's)"
            },
        ));
    } else {
        // One entry per driver (`radeon` from `radeon_icd.x86_64.json`) with
        // the bitnesses seen: NixOS has 14 Mesa drivers twice.
        let mut drivers: std::collections::BTreeMap<String, BTreeSet<String>> =
            std::collections::BTreeMap::new();
        for i in &icds {
            let name = i
                .manifest
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let name = name.split("_icd").next().unwrap_or(&name).to_string();
            let name = name.strip_suffix(".json").unwrap_or(&name).to_string();
            drivers
                .entry(name)
                .or_default()
                .insert(i.bits.map_or("?".to_string(), |b| b.to_string()));
        }
        let names: Vec<String> = drivers
            .iter()
            .map(|(n, bits)| {
                format!(
                    "{n} ({})",
                    bits.iter().cloned().collect::<Vec<_>>().join("/")
                )
            })
            .collect();
        f.push(Finding::ok(
            "vulkan",
            format!(
                "{} driver manifest(s), bits: {}",
                icds.len(),
                names.join(", ")
            ),
        ));
    }
    let gpus = gpus(host);
    let devices = vulkaninfo_devices(host);
    if gpus.len() > 1 {
        let mut w = Finding::warn(
            "vulkan",
            format!(
                "{} GPUs ({}): DXVK and VKD3D-Proton pick one themselves (R18)",
                gpus.len(),
                gpus.iter()
                    .map(|g| format!("{} {}", g.card, g.vendor))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "to choose one, set env = { DXVK_FILTER_DEVICE_NAME = \"<part of its name>\", \
             VKD3D_FILTER_DEVICE_NAME = \"<…>\" } under [platforms.linux] in the game's \
             override file (vulkaninfo --summary lists the names)",
        );
        w.detail = devices.unwrap_or_default();
        f.push(w);
    } else if let Some(d) = devices.filter(|d| !d.is_empty()) {
        f.push(Finding::ok("vulkan", format!("devices: {}", d.join(", "))));
    }
    icds
}

/// A 32-bit game needs a 32-bit Vulkan driver (R19), unless the build runs
/// it through new WoW64 (no `i386-unix` Wine libraries).
fn check_vulkan32(
    host: &Host,
    icds: &[Icd],
    scan: Option<&Result<PeScan, ScanError>>,
    build: Option<&Path>,
) -> Finding {
    let bits = match scan {
        Some(Ok(s)) => s.bitness,
        _ => {
            return Finding::ok("vulkan32", "the game's exe wasn't scanned: not checked");
        }
    };
    if bits == Bitness::X86_64 {
        return Finding::ok("vulkan32", "64-bit game: no 32-bit driver needed");
    }
    if let Some(b) = build
        && !b.join("files/lib/wine/i386-unix").is_dir()
        && b.join("files/lib/wine/x86_64-unix").is_dir()
    {
        return Finding::ok(
            "vulkan32",
            "32-bit game on a WoW64 build (no i386-unix): the 64-bit driver serves it",
        );
    }
    let n32 = icds.iter().filter(|i| i.bits == Some(32)).count();
    let unknown = icds.iter().filter(|i| i.bits.is_none()).count();
    let hint = if host.is_nixos() {
        "hardware.graphics.enable32Bit = true in the NixOS config"
    } else {
        "install your distribution's 32-bit Vulkan driver (lib32-vulkan-* / mesa-vulkan-drivers:i386, or NVIDIA's 32-bit libraries)"
    };
    if n32 > 0 {
        Finding::ok("vulkan32", format!("32-bit game: {n32} 32-bit driver(s)"))
    } else if unknown > 0 {
        Finding::warn(
            "vulkan32",
            format!("32-bit game: no driver manifest says 32-bit ({unknown} don't say which)"),
            hint,
        )
    } else {
        Finding::fail(
            "vulkan32",
            "32-bit game and no 32-bit Vulkan driver: DXVK and D7VK fail, only wined3d works",
            hint,
        )
    }
}

// — stamp, links, processes —

fn check_stamp(g: &Game, toml_raw: &[u8]) -> Finding {
    let setup = g.setup_hint();
    let prefix = &g.dirs.prefix;
    if !prefix.is_dir() {
        return Finding::fail("stamp", format!("no prefix at {}", prefix.display()), setup);
    }
    if !setup::is_prefix(prefix) {
        return Finding::fail(
            "stamp",
            format!(
                "{} isn't a prefix Proton made (no system.reg or drive_c/windows)",
                prefix.display()
            ),
            setup,
        );
    }
    let (Some(state), Some(conf)) = (&g.state, &g.conf) else {
        return Finding::fail(
            "stamp",
            "setup never completed (no state.toml or launch.conf)",
            setup,
        );
    };
    let l4 = std::fs::read(compat::user_path(&g.gogid)).ok();
    let want = crate::macapp::launch::stamp(toml_raw, l4.as_deref());
    if state.stamp == want && conf.get("STAMP") == want {
        Finding::ok(
            "stamp",
            format!(
                "config current (runtime {}, renderer {})",
                state.runtime, state.renderer
            ),
        )
    } else {
        Finding::warn(
            "stamp",
            "config stale: gogo-launch.toml or your override changed since the last setup \
             (the next launch runs it)",
            setup,
        )
    }
}

/// Wine's serial ports: `dosdevices/com1` → `/dev/ttyS0`. Its mountmgr makes
/// them on every start (real smoke, #199), and they reach no files.
fn is_wine_port(prefix: &Path, link: &Path, target: &Path) -> bool {
    let name = link.file_name().and_then(|n| n.to_str()).unwrap_or("");
    link.parent() == Some(prefix.join("dosdevices").as_path())
        && name
            .strip_prefix("com")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        && target.starts_with("/dev")
}

/// Links out of the data root in the prefix, but `dosdevices/z:`, which
/// GE-Proton recreates on every run and setup keeps
/// ([`setup::KEPT_DRIVES`], design 18 §7.2), Wine's serial ports, and links
/// into the game's own Proton build: Proton links its DLLs and fonts into `drive_c/windows`
/// (about 1,300 in a GE-Proton11-12 prefix, real smoke), which are outside
/// the data root for a build in a shared tools dir or a `path:` one.
fn check_links(g: &Game, proton: Option<&Path>) -> Finding {
    let prefix = &g.dirs.prefix;
    if !prefix.is_dir() {
        return Finding::ok("links", "no prefix yet");
    }
    let mut out = Vec::new();
    // `../../../game` stays in the game's dir, wherever that is.
    walk_links(prefix, &[&g.data_root, &g.dirs.root], prefix, &mut out);
    let kept: Vec<PathBuf> = setup::KEPT_DRIVES
        .iter()
        .map(|d| prefix.join("dosdevices").join(d))
        .collect();
    // The build's real path too: Proton links through it when a part of the
    // data root is itself a link.
    let proton_real = proton.and_then(|d| d.canonicalize().ok());
    let proton = proton.map(normalize);
    out.retain(|(p, t)| {
        let resolved = normalize(&p.parent().unwrap_or(prefix).join(t));
        !kept.contains(p)
            && !is_wine_port(prefix, p, t)
            && ![&proton, &proton_real]
                .iter()
                .any(|d| d.as_ref().is_some_and(|d| resolved.starts_with(d)))
    });
    // Links into another Proton build: the one the game ran on before a
    // retarget. Proton re-links the prefix when a build of another version
    // next runs it, so they're a warning, not isolation broken.
    let mut others: BTreeSet<PathBuf> = BTreeSet::new();
    out.retain(|(p, t)| {
        let resolved = normalize(&p.parent().unwrap_or(prefix).join(t));
        match resolved
            .ancestors()
            .find(|a| a.join(proton::TOOLMANIFEST).is_file() && a.join("proton").is_file())
        {
            Some(build) => {
                others.insert(build.to_path_buf());
                false
            }
            None => true,
        }
    });
    if out.is_empty() && others.is_empty() {
        return Finding::ok(
            "links",
            "no links out of the data dir but z:, Wine's serial ports and the Proton build's \
             own files",
        );
    }
    if out.is_empty() {
        let dirs: Vec<String> = others.iter().map(|d| d.display().to_string()).collect();
        return Finding::warn(
            "links",
            format!(
                "the prefix still links into another Proton build, {}: the one it ran on \
                 before (Proton re-links it when a build of another version next runs it)",
                dirs.join(", ")
            ),
            "keep that build until the game has run on the new one",
        );
    }
    let shown: Vec<String> = out
        .iter()
        .take(3)
        .map(|(p, t)| {
            format!(
                "{} -> {}",
                p.strip_prefix(prefix).unwrap_or(p).display(),
                t.display()
            )
        })
        .collect();
    let more = out.len().saturating_sub(shown.len());
    Finding::fail(
        "links",
        format!(
            "{} link(s) out of the data dir: {}{}",
            out.len(),
            shown.join(", "),
            if more > 0 {
                format!(", +{more} more")
            } else {
                String::new()
            }
        ),
        format!(
            "{} (turns home links into dirs, drops host drives but z:)",
            g.setup_hint()
        ),
    )
}

/// Processes left in the prefix with no launcher (the launcher's own
/// markers, [`game::prefix_procs`]). Reported, never stopped.
fn check_processes(g: &Game) -> Finding {
    if !g.dirs.prefix.is_dir() {
        return Finding::ok("processes", "no prefix yet");
    }
    if let Some(pid) = game::live_launcher(&g.dirs.root) {
        return Finding::ok("processes", format!("the game is running (launcher {pid})"));
    }
    let found = game::prefix_procs(&g.dirs.prefix);
    if found.is_empty() {
        return Finding::ok("processes", "no processes left in the prefix");
    }
    let pids: Vec<String> = found.iter().map(|p| p.pid.to_string()).collect();
    Finding::warn(
        "processes",
        format!(
            "{} process(es) still running in the prefix with no launcher: {}",
            found.len(),
            found
                .iter()
                .map(|p| format!("{} {}", p.pid, p.name))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        format!("kill -KILL {}", pids.join(" ")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containers_cover_the_pinned_umus_appids() {
        let ours: Vec<&str> = CONTAINERS.iter().map(|(a, _)| *a).collect();
        assert_eq!(ours, catalog::umu_runtimes().to_vec());
    }

    fn host(root: &Path, env: &[(&'static str, &str)]) -> Host {
        let env: Vec<(&'static str, OsString)> =
            env.iter().map(|(k, v)| (*k, OsString::from(v))).collect();
        Host {
            root: root.to_path_buf(),
            env: Box::new(move |k| env.iter().find(|(e, _)| *e == k).map(|(_, v)| v.clone())),
            logs: root.join("logs"),
            vulkaninfo: false,
        }
    }

    fn write(p: &Path, text: impl AsRef<[u8]>) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn icd_bits_from_the_manifest_the_library_or_the_name() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        let icd = |name: &str, body: &str| {
            write(&r.join("usr/share/vulkan/icd.d").join(name), body);
        };
        write(&r.join("usr/lib32/libv32.so"), [0x7f, b'E', b'L', b'F', 1]);
        icd(
            "a.json",
            r#"{"ICD": {"library_path": "/usr/lib32/libv32.so"}}"#,
        );
        icd(
            "b.json",
            r#"{"ICD": {"library_path": "libGLX_nvidia.so.0", "library_arch": "64"}}"#,
        );
        icd(
            "radeon_icd.i686.json",
            r#"{"ICD": {"library_path": "libvulkan_radeon.so"}}"#,
        );
        icd("plain.json", r#"{"ICD": {"library_path": "libplain.so"}}"#);
        icd("broken.json", "{");
        // The same manifest linked into a second dir counts once.
        write(&r.join("run/opengl-driver-32/share/vulkan/icd.d/.keep"), "");
        std::os::unix::fs::symlink(
            r.join("usr/share/vulkan/icd.d/b.json"),
            r.join("run/opengl-driver-32/share/vulkan/icd.d/b.json"),
        )
        .unwrap();
        let found: Vec<(String, Option<u8>)> = find_icds(&host(r, &[]))
            .into_iter()
            .map(|i| {
                (
                    i.manifest.file_name().unwrap().to_string_lossy().into(),
                    i.bits,
                )
            })
            .collect();
        assert_eq!(
            found,
            [
                ("b.json".into(), Some(64)),
                ("a.json".into(), Some(32)),
                ("plain.json".into(), None),
                ("radeon_icd.i686.json".into(), Some(32)),
            ]
        );
        // VK_DRIVER_FILES replaces the search.
        let only = find_icds(&host(
            r,
            &[("VK_DRIVER_FILES", "/usr/share/vulkan/icd.d/plain.json")],
        ));
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].library, "libplain.so");
    }

    #[test]
    fn user_namespaces_off_fail_and_apparmor_warns() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        assert_eq!(check_userns(&host(r, &[])).level, Level::Ok);
        write(
            &r.join("proc/sys/kernel/apparmor_restrict_unprivileged_userns"),
            "1\n",
        );
        assert_eq!(check_userns(&host(r, &[])).level, Level::Warn);
        write(&r.join("proc/sys/user/max_user_namespaces"), "0\n");
        let f = check_userns(&host(r, &[]));
        assert!(
            f.level == Level::Fail && f.message.contains("max_user_namespaces"),
            "{f:?}"
        );
        write(&r.join("proc/sys/kernel/unprivileged_userns_clone"), "0\n");
        let f = check_userns(&host(r, &[]));
        assert!(f.message.contains("unprivileged_userns_clone"), "{f:?}");
    }

    #[test]
    fn several_gpus_warn_with_the_device_filter() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        write(
            &r.join("usr/share/vulkan/icd.d/x.x86_64.json"),
            r#"{"ICD": {"library_path": "x.so"}}"#,
        );
        for (card, vendor) in [("card0", "0x10de"), ("card1", "0x1002")] {
            write(
                &r.join("sys/class/drm").join(card).join("device/vendor"),
                vendor,
            );
        }
        // A connector of card0 isn't a GPU.
        write(&r.join("sys/class/drm/card0-DP-1/device/vendor"), "0x10de");
        let mut f = Vec::new();
        check_vulkan(&host(r, &[]), &mut f);
        assert_eq!(f[0].level, Level::Ok);
        assert_eq!(f[1].level, Level::Warn);
        assert!(
            f[1].message.starts_with("2 GPUs (card0 NVIDIA, card1 AMD)"),
            "{f:?}"
        );
        assert!(
            f[1].hint
                .as_deref()
                .unwrap()
                .contains("DXVK_FILTER_DEVICE_NAME")
        );
    }

    #[test]
    fn vulkan32_only_for_a_32_bit_game_on_an_old_wow_build() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        let scan = |bitness| -> Result<PeScan, ScanError> {
            let mut s: PeScan = serde_json::from_value(serde_json::json!({
                "root": "/g", "main_exe": "g.exe", "main_exe_source": "heuristic",
                "bitness": "i386", "engine": null, "graphics": "unknown",
                "graphics_source": "none", "graphics_imports": [], "runtime_graphics": [],
                "helpers": [], "runtime_loaded": [], "local_overrides": [],
                "d3d7_confidence": null, "dxguid_block": false, "renderer_dlls": [],
                "files": [],
            }))
            .unwrap();
            s.bitness = bitness;
            Ok(s)
        };
        let h = host(r, &[]);
        let none: Vec<Icd> = vec![];
        let f = check_vulkan32(&h, &none, Some(&scan(Bitness::X86_64)), None);
        assert_eq!(f.level, Level::Ok);
        let f = check_vulkan32(&h, &none, Some(&scan(Bitness::I386)), None);
        assert_eq!(f.level, Level::Fail);
        assert!(
            f.hint.as_deref().unwrap().contains("32-bit Vulkan"),
            "{f:?}"
        );
        write(&r.join("etc/NIXOS"), "");
        let f = check_vulkan32(&h, &none, Some(&scan(Bitness::I386)), None);
        assert_eq!(
            f.hint.as_deref(),
            Some("hardware.graphics.enable32Bit = true in the NixOS config")
        );
        std::fs::create_dir_all(r.join("b/files/lib/wine/x86_64-unix")).unwrap();
        let f = check_vulkan32(&h, &none, Some(&scan(Bitness::I386)), Some(&r.join("b")));
        assert_eq!(f.level, Level::Ok, "{f:?}");
        std::fs::create_dir_all(r.join("b/files/lib/wine/i386-unix")).unwrap();
        let f = check_vulkan32(&h, &none, Some(&scan(Bitness::I386)), Some(&r.join("b")));
        assert_eq!(f.level, Level::Fail);
        let one = [Icd {
            manifest: "x.i686.json".into(),
            library: "x".into(),
            bits: Some(32),
        }];
        let f = check_vulkan32(&h, &one, Some(&scan(Bitness::I386)), Some(&r.join("b")));
        assert_eq!(f.level, Level::Ok);
    }
}
