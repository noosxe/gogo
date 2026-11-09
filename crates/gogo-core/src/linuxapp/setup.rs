//! `gogo linux-app setup <gogid>` (design 18 §7, design 16 §2.2 on
//! Linux): bring `games/<gogid>/` up to date with its `gogo-launch.toml` and
//! the L4 override. The build runs it once at the end; the launcher (#177)
//! runs it again when the stamp is stale or the prefix is missing.
//!
//! In order: resolve the Proton build (installing a missing pin), resolve
//! umu (`[linux] umu`, `UMU_PYTHON` re-recorded every time: it's a store
//! path a Nix GC can remove), look up the umu id, read the protonfix, merge
//! the fixes (design 18 §4.3), create the prefix through umu, apply the
//! renderer (§3), the registry values and the missing verbs (R12), keep the
//! prefix free of links out of the data root (design 16 decision 4), then
//! write `launch.conf` and `state.toml`.
//!
//! Every umu run gets `WINEPREFIX` (R7), `PROTONPATH`, `GAMEID`,
//! `STORE=gog` and `PROTONFIXES_DISABLE=1` (the protonfix runs at launch,
//! not during setup), a working dir in the game's dir, and nothing under
//! `/tmp` (#168: umu's FHS env has a private one).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::fixes::{self, Fixes};
use super::game::{self, LaunchConf, LinuxState, STATE_SCHEMA, refuse_private_tmp};
use super::launcher;
use super::proton::{self, ProtonRef};
use super::protonfix;
use super::umu::{self, Host, UmuRun};
use super::umu_id::{self, UmuId};
use crate::engine::events::ProgressEvent;
use crate::macapp::compat::{self, CompatEntry, Layer, Os};
use crate::macapp::error::MacAppError;
use crate::macapp::launch::{GameDirs, LaunchToml, state_rel, windows_to_unix, write_atomic};
use crate::macapp::prompt::PromptFn;
use crate::macapp::renderer::{self, PROTON_USE_WINED3D, Renderer};
use crate::macapp::{catalog, ddraw, lutris, pe, store};

/// `games/<gogid>/install/`: umu's output from the build and setup.
pub const LOG_DIR: &str = "install";
/// Every umu run of setup, appended.
pub const SETUP_LOG: &str = "setup.log";

/// What [`setup`] is told.
#[derive(Default)]
pub struct SetupOptions<'a> {
    /// Refresh the umu-database CSV from GitHub even when the cached copy
    /// is fresh (`--refresh`; it only matters for a build without one).
    pub refresh: bool,
    /// The terminal's prompts, for `[linux] umu` and `[linux] proton_dir`
    /// when they're unset. `None`: headless, as the launcher runs it.
    pub prompt: Option<&'a PromptFn>,
}

/// What [`setup`] did.
#[derive(Debug, Clone)]
pub struct SetupOutcome {
    pub state: LinuxState,
    pub conf: LaunchConf,
    pub prefix_created: bool,
    /// `ge-proton/<ver>` installed on the way.
    pub runtime_installed: Option<String>,
    pub umu_id: UmuId,
    /// The fix summary (design 18 §4.3), as setup applied it.
    pub fixes: fixes::Summary,
    /// Verbs winetricks installed this time.
    pub verbs_installed: Vec<String>,
    pub notes: Vec<String>,
    pub warnings: Vec<String>,
    /// `launch.sh`, the menu entry and the icon.
    pub launcher: launcher::LauncherFiles,
}

/// A Proton build resolved for a game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtonBuild {
    /// What `gogo-launch.toml` and `state.toml` record: `ge-proton/<ver>`
    /// or `path:<dir>`.
    pub reference: String,
    /// `PROTONPATH`: where `proton` and `toolmanifest.vdf` are.
    pub dir: PathBuf,
    /// gogo installed it (the store or a shared tools dir with its marker).
    pub managed: bool,
}

/// The catalog version a version-less `ge-proton` means: the pin.
pub fn pinned_version(id: &str) -> Option<&'static str> {
    catalog::find(id)?.discovery.as_ref()?.pinned_version()
}

/// A Proton ref as it stands now, without installing anything: the build
/// when it's there, `Ok(None)` for a catalog build that isn't installed yet.
pub fn find_proton(r: &ProtonRef) -> Result<Option<ProtonBuild>, MacAppError> {
    match r {
        ProtonRef::Path(dir) => {
            if !dir.join(proton::TOOLMANIFEST).is_file() {
                return Err(MacAppError::Proton(format!(
                    "{} has no {}: umu can't run it (pick a Proton build dir, the one \
                     with `proton` in it)",
                    dir.display(),
                    proton::TOOLMANIFEST
                )));
            }
            Ok(Some(ProtonBuild {
                reference: format!("path:{}", dir.display()),
                dir: dir.clone(),
                managed: proton::is_managed(dir),
            }))
        }
        ProtonRef::Catalog { id, version } => {
            let version = version.as_deref().or(pinned_version(id));
            Ok(store::installed(id, version).map(|(m, _)| ProtonBuild {
                reference: format!("{}/{}", m.id, m.version),
                dir: store::proton_dir(&m),
                managed: true,
            }))
        }
    }
}

/// Why a catalog build gogo installed must be reinstalled (§2.2): it no
/// longer matches its marker (edited, half-overwritten), which the launcher
/// takes as a reason to run setup. `None` for an intact build, one without
/// a marker, or a `path:` build: that one is the user's pick, followed as
/// it is.
pub fn changed_managed(r: &ProtonRef, dir: &Path) -> Option<proton::BuildProblem> {
    match (r, proton::check_managed(dir)) {
        (ProtonRef::Catalog { .. }, Err(p))
            if !matches!(p, proton::BuildProblem::NotManaged(_)) =>
        {
            Some(p)
        }
        _ => None,
    }
}

/// [`find_proton`], installing a catalog build that's missing (design 16
/// §2.2: setup installs the pin). Where it goes is `[linux] proton_dir`,
/// asked once on a terminal ([`proton::placement`]). Returns the build and
/// the `<id>/<ver>` installed, if one was.
pub fn ensure_proton(
    r: &ProtonRef,
    prompt: Option<&PromptFn>,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<(ProtonBuild, Option<String>), MacAppError> {
    if let Some(b) = find_proton(r)? {
        let Some(problem) = changed_managed(r, &b.dir) else {
            return Ok((b, None));
        };
        emit(&ProgressEvent::Warn {
            message: format!(
                "{} ({}): {problem}; reinstalling it",
                b.reference,
                b.dir.display()
            ),
        });
        std::fs::remove_dir_all(&b.dir)?;
    }
    let ProtonRef::Catalog { id, version } = r else {
        unreachable!("find_proton returns path: builds or fails")
    };
    let entry = catalog::find(id)
        .ok_or_else(|| MacAppError::Proton(format!("{id} is not in gogo's catalog")))?;
    let version = version.as_deref().or(pinned_version(id));
    let http = catalog::http_client()?;
    let config = crate::config::Config::path();
    let host = Host::real();
    let asks = prompt.is_some()
        && proton::stored_at(&config).is_none()
        && !proton::steam_roots(&host).is_empty();
    let size = if asks {
        lutris::block_on_detached(catalog::probe(entry, version, &http, catalog::GITHUB_API))
            .ok()
            .map(|r| r.size)
    } else {
        None
    };
    let shared = proton::placement(entry, size, &config, prompt, &host, emit)?;
    let m = lutris::block_on_detached(store::install_to(
        entry,
        version,
        &http,
        catalog::GITHUB_API,
        false,
        shared.as_deref(),
        emit,
    ))?;
    let reference = format!("{}/{}", m.id, m.version);
    Ok((
        ProtonBuild {
            reference: reference.clone(),
            dir: store::proton_dir(&m),
            managed: true,
        },
        Some(reference),
    ))
}

/// How umu runs (`[linux] umu`, downloading the pinned zipapp if needed).
pub fn ensure_umu(
    prompt: Option<&PromptFn>,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<UmuRun, MacAppError> {
    let http = catalog::http_client()?;
    lutris::block_on_detached(umu::provision(
        prompt,
        &Host::real(),
        &http,
        catalog::GITHUB_API,
        emit,
    ))
}

/// One game's umu: every run against its prefix with the same build and id.
pub struct Umu<'a> {
    pub run: &'a UmuRun,
    pub proton: &'a Path,
    pub prefix: &'a Path,
    pub umu_id: &'a str,
    /// The working dir umu starts in (a dir of the game's, never `/tmp`).
    pub cwd: &'a Path,
    /// Where its output is appended.
    pub log: PathBuf,
}

impl Umu<'_> {
    /// Refuse anything umu's container couldn't see (#168).
    pub fn check(&self) -> Result<(), MacAppError> {
        refuse_private_tmp("the Wine prefix", self.prefix)?;
        refuse_private_tmp("the Proton build", self.proton)?;
        refuse_private_tmp("umu's working dir", self.cwd)?;
        refuse_private_tmp("umu-run", self.run.umu_run())
    }

    /// `umu-run <args>` with the game's environment. `PROTONFIXES_DISABLE`:
    /// what setup and the build run isn't the game, and its protonfix
    /// runs at launch.
    pub fn command<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let mut c = self.run.command();
        c.args(args.into_iter().map(Into::into))
            .env("WINEPREFIX", self.prefix)
            .env("PROTONPATH", self.proton)
            .env("GAMEID", self.umu_id)
            .env("STORE", "gog")
            .env(compat::PROTONFIXES_DISABLE, "1")
            .env(GAME_DRIVE_OFF.0, GAME_DRIVE_OFF.1);
        share_library_game(&mut c, self.prefix);
        c
    }

    /// Run `cmd` (one of [`Self::command`]'s, or `fixes::winetricks_command`'s)
    /// from [`Self::cwd`], its output appended to the log under a `## what`
    /// line. The exit status, or why it couldn't start.
    pub fn run(
        &self,
        what: &str,
        mut cmd: Command,
    ) -> Result<std::process::ExitStatus, MacAppError> {
        self.prepare(what, &mut cmd)?;
        cmd.status().map_err(|e| self.cannot_run(what, &e))
    }

    /// [`Self::run`] for a best-effort step that may never end: after `limit`
    /// it kills `cmd` and everything in the prefix, and returns `None`.
    pub fn run_limited(
        &self,
        what: &str,
        mut cmd: Command,
        limit: Duration,
    ) -> Result<Option<std::process::ExitStatus>, MacAppError> {
        self.prepare(what, &mut cmd)?;
        let mut child = cmd.spawn().map_err(|e| self.cannot_run(what, &e))?;
        let deadline = Instant::now() + limit;
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let _ = child.kill();
        let _ = child.wait();
        // umu's container and Wine's services leave the child's process group.
        let pids = game::prefix_procs(self.prefix);
        if !pids.is_empty() {
            let _ = Command::new("kill")
                .arg("-KILL")
                .args(pids.iter().map(|p| p.pid.to_string()))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        Ok(None)
    }

    /// Working dir and log for one run, with its `## what` line.
    fn prepare(&self, what: &str, cmd: &mut Command) -> Result<(), MacAppError> {
        if let Some(dir) = self.log.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)?;
        {
            use std::io::Write as _;
            writeln!(log, "## {what}")?;
        }
        cmd.current_dir(self.cwd)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        Ok(())
    }

    fn cannot_run(&self, what: &str, e: &std::io::Error) -> MacAppError {
        MacAppError::Umu(format!(
            "cannot run {} for {what}: {e}",
            self.run.umu_run().display()
        ))
    }

    /// Create the prefix (`umu-run ""`: Proton sets the prefix up and runs
    /// nothing). Proton exits non-zero for the missing exe on some umu
    /// versions, so success is [`is_prefix`] afterwards.
    pub fn create_prefix(&self) -> Result<(), MacAppError> {
        std::fs::create_dir_all(self.prefix)?;
        let status = self.run("create the prefix", self.command([""]))?;
        if is_prefix(self.prefix) {
            return Ok(());
        }
        Err(MacAppError::Umu(format!(
            "umu-run didn't create the prefix {} (exit {}); its output is in {}",
            self.prefix.display(),
            status.code().unwrap_or(-1),
            self.log.display()
        )))
    }

    /// `reg add …` in the prefix (Wine's `reg.exe`, through Proton).
    pub fn reg_add(&self, v: &compat::RegistryValue) -> Result<(), MacAppError> {
        let argv = crate::macapp::setup::reg_add_argv(v);
        let status = self.run(&format!("registry {}", v.key), self.command(argv))?;
        if status.success() {
            return Ok(());
        }
        Err(MacAppError::Recipe(format!(
            "registry {}: reg add exited {} (output in {})",
            v.key,
            status.code().unwrap_or(-1),
            self.log.display()
        )))
    }
}

/// [`write_atomic`] unless the file already holds `text`: a setup that
/// changed nothing leaves `launch.conf` and `state.toml` as they were.
fn write_if_changed(path: &Path, text: &str) -> Result<(), MacAppError> {
    if std::fs::read(path).is_ok_and(|b| b == text.as_bytes()) {
        return Ok(());
    }
    write_atomic(path, text)
}

/// A Wine prefix Proton made: `system.reg` and `drive_c/windows`. Not just
/// `drive_c`: umu's own `setup_pfx` makes `drive_c/users/<user>` before
/// Proton runs, so a run that failed in the container leaves one behind.
pub fn is_prefix(prefix: &Path) -> bool {
    prefix.join("system.reg").is_file() && prefix.join("drive_c/windows").is_dir()
}

/// The `dosdevices/` entries a Linux prefix keeps (design 18 §7.2): `c:`,
/// and `z:` → `/`, which GE-Proton's `proton` script recreates on every
/// run, so removing it would only hold between runs. Every other drive
/// goes, as on the Mac. [`GAME_DRIVE_OFF`] keeps Proton's host drives from
/// coming back; `com1`… do come back (Wine's mountmgr, every start), so
/// #178's `links` check skips those as well as these.
pub const KEPT_DRIVES: [&str; 2] = ["c:", "z:"];

/// A game in a library may sit outside what the Steam Linux Runtime's
/// container shares by itself (`$HOME`, the prefix, the exe's dir and umu's
/// mount above it), and `prefix/drive_c/GOG/<name>` leads out of the prefix
/// to `game/`: the game's dir is shared by name (design 18 §6.1). The launcher
/// does the same. Nothing changes for a game under the data root.
pub(crate) fn share_library_game(c: &mut Command, prefix: &Path) {
    if let Some(root) = prefix.parent()
        && !prefix.starts_with(crate::state::data_root())
    {
        let mut shared = root.as_os_str().to_os_string();
        if let Some(more) = std::env::var_os(PV_SHARED) {
            shared.push(":");
            shared.push(more);
        }
        c.env(PV_SHARED, shared);
    }
}

/// pressure-vessel's list of extra paths to share, read-write.
const PV_SHARED: &str = "PRESSURE_VESSEL_FILESYSTEMS_RW";

/// Turns off GE-Proton's `gamedrive` compat option, which it forces on under
/// umu: `s:` → the library dir (`/home` here, from umu's
/// `STEAM_COMPAT_LIBRARY_PATHS`), and from protonfixes' `setup_mount_drives`
/// `u:` → `/media`, `v:` → `/run/media`, `w:` → `/mnt`, `x:` → `$HOME` on
/// every run (#199). gogo's games live inside the prefix and need none of
/// them. The launcher sets it before the L4 env, so an L4 `env` can turn
/// the drives back on.
pub const GAME_DRIVE_OFF: (&str, &str) = ("PROTON_SET_GAME_DRIVE", "0");

/// Decision 4 on Linux: home folders are real dirs (no absolute links under
/// `drive_c/users/`), and `dosdevices/` keeps only [`KEPT_DRIVES`].
/// Idempotent.
pub fn localize(prefix: &Path) -> Result<(), MacAppError> {
    crate::macapp::setup::localize_home(prefix)?;
    let Ok(entries) = std::fs::read_dir(prefix.join("dosdevices")) else {
        return Ok(());
    };
    for e in entries.flatten() {
        let name = e.file_name();
        if !KEPT_DRIVES.iter().any(|k| name == *k) && e.file_type()?.is_symlink() {
            std::fs::remove_file(e.path())?;
        }
    }
    Ok(())
}

/// `z:` → `/` for a step that hands Wine unix paths (winetricks' cached
/// downloads, an installer's log); `true` when it had to be put back (a
/// prefix set up before `z:` was kept, #176).
pub fn restore_z(prefix: &Path) -> Result<bool, MacAppError> {
    let z = prefix.join("dosdevices/z:");
    if z.symlink_metadata().is_ok() {
        return Ok(false);
    }
    std::fs::create_dir_all(prefix.join("dosdevices"))?;
    #[cfg(unix)]
    std::os::unix::fs::symlink("/", &z)?;
    Ok(true)
}

/// The layer a baked renderer came from, as far as setup can tell: L1 when
/// it's the scan's pick, L3 when the compat DB names it, else a build flag.
pub(crate) fn baked_renderer_layer(baked: &str, l1: &renderer::Pick, db: &CompatEntry) -> Layer {
    if baked == l1.renderer.default_ref() {
        Layer::Auto
    } else if db.renderer.as_deref() == Some(baked) {
        Layer::Db
    } else {
        Layer::Flags
    }
}

/// The fixes for a game on a Proton build (design 18 §4.3): the umu id's
/// module and `default.py`, the L3 file under the ownership rule, and
/// `layers` (L1, L4, flags, resolved for Linux). Unreadable modules are
/// warned about and count as none.
pub fn fixes_for(
    gogid: &str,
    build: &Path,
    umu_id: &str,
    layers: Vec<(Layer, CompatEntry)>,
    warnings: &mut Vec<String>,
) -> Fixes {
    let mut read = |what: &str, r: std::io::Result<Option<protonfix::Module>>| match r {
        Ok(m) => m,
        Err(e) => {
            warnings.push(format!(
                "can't read the protonfix {what} in {}: {e}",
                build.display()
            ));
            None
        }
    };
    let module = read(umu_id, protonfix::read_module(build, umu_id));
    let default = read(protonfix::DEFAULT_MODULE, protonfix::read_default(build));
    let f = fixes::resolve(fixes::Inputs {
        umu_id,
        module,
        default,
        db: compat::db_raw(gogid),
        layers,
    });
    warnings.extend(f.warnings.iter().cloned());
    f
}

/// The umu id for `gogid` on `build` (rule 1–3 of design 18 §4.1); its
/// warnings go to `warnings`. A lookup never fails.
pub fn umu_id_for(gogid: &str, build: &Path, refresh: bool, warnings: &mut Vec<String>) -> UmuId {
    let numeric = gogid.parse::<u64>().unwrap_or_default();
    let l = lutris::block_on_detached(umu_id::lookup(build, numeric, refresh));
    warnings.extend(l.warnings);
    l.umu
}

/// The game's files: `game/` for a depot build, else its dir in the prefix.
pub(crate) fn game_files(dirs: &GameDirs, tree: Option<&str>) -> PathBuf {
    if dirs.game.is_dir() {
        return dirs.game.clone();
    }
    tree.map_or_else(
        || dirs.game.clone(),
        |n| dirs.prefix.join("drive_c/GOG").join(n),
    )
}

/// Bring `games/<gogid>/` up to date (see the module docs). Never deletes
/// the prefix or anything in it but links out of the data root.
pub fn setup(
    gogid: &str,
    opts: &SetupOptions,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<SetupOutcome, MacAppError> {
    let dirs = GameDirs::for_game(gogid)?;
    let data_root = crate::state::data_root();
    refuse_private_tmp("gogo's data dir", &data_root)?;
    let prior = LinuxState::load(&dirs.state_toml)?;
    let (baked, raw) = game::read_launch_toml(&dirs)?;
    if baked.gogid != gogid {
        return Err(MacAppError::Compat {
            path: game::launch_toml_path(&dirs),
            reason: format!("it is for {}, not {gogid}", baked.gogid),
        });
    }
    let l4_path = compat::user_path(gogid);
    let l4_raw = match std::fs::read(&l4_path) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let l4 = compat::load_user_for(gogid, Os::Linux)?;
    let mut warnings: Vec<String> = l4
        .warnings
        .iter()
        .map(|w| format!("{}: {w}", l4_path.display()))
        .collect();
    let mut notes = Vec::new();

    // The runtime: L4, else what the build baked (flags > L3 > default).
    let runtime = l4.entry.runtime.clone().unwrap_or(baked.runtime.clone());
    let r = ProtonRef::parse(&runtime).map_err(MacAppError::Proton)?;
    let (build, runtime_installed) = ensure_proton(&r, opts.prompt, emit)?;
    if let ProtonRef::Path(_) = r
        && !build.managed
    {
        for p in proton::inspect(&build.dir, "the game's runtime").problems() {
            warnings.push(format!("{}: {p}", build.dir.display()));
        }
    }
    let umu_run = ensure_umu(opts.prompt, emit)?;
    let umu_id = umu_id_for(gogid, &build.dir, opts.refresh, &mut warnings);

    // The game files, for the renderer's scan and the prefix link.
    let tree = baked.tree_name();
    let game_dir = tree
        .as_ref()
        .map(|n| dirs.prefix.join("drive_c/GOG").join(n));
    let db = compat::db_entry_for(gogid, Os::Linux).entry;

    // The prefix, through umu.
    let umu = Umu {
        run: &umu_run,
        proton: &build.dir,
        prefix: &dirs.prefix,
        umu_id: &umu_id.id,
        cwd: &dirs.root,
        log: dirs.root.join(LOG_DIR).join(SETUP_LOG),
    };
    umu.check()?;
    let prefix_created = !is_prefix(&dirs.prefix);
    if prefix_created {
        umu.create_prefix()?;
    }
    crate::macapp::setup::localize_home(&dirs.prefix)?;
    if let Some(name) = &tree
        && dirs.game.is_dir()
        && dirs
            .prefix
            .join("drive_c/GOG")
            .join(name)
            .symlink_metadata()
            .is_err()
    {
        crate::macapp::setup::link_game(&dirs, name)?;
    }

    // L1 for the renderer: a fresh scan of the game as installed (gogo's
    // own cnc-ddraw discounted).
    let exe_unix = windows_to_unix(&dirs.prefix, &baked.exe).filter(|e| e.is_file());
    let scan_dir = game_dir.clone().filter(|d| d.is_dir());
    let l1 = renderer::pick_linux(&match (&scan_dir, &exe_unix) {
        (Some(d), Some(e)) => pe::scan_exe(d, e),
        (Some(d), None) => pe::scan(d),
        _ => Err(pe::ScanError::NoExecutable {
            dir: dirs.game.clone(),
        }),
    });
    let renderer_ref = l4.entry.renderer.clone().unwrap_or(baked.renderer.clone());
    let renderer_layer = if l4.entry.renderer.is_some() {
        Layer::User
    } else {
        baked_renderer_layer(&baked.renderer, &l1, &db)
    };

    // The fixes: L1 verbs and the renderer, L3 under the ownership rule,
    // L4.
    let fx = fixes_for(
        gogid,
        &build.dir,
        &umu_id.id,
        vec![
            (
                Layer::Auto,
                CompatEntry {
                    verbs: Some(baked.verbs.clone()),
                    ..Default::default()
                },
            ),
            (
                renderer_layer,
                CompatEntry {
                    renderer: Some(renderer_ref.clone()),
                    ..Default::default()
                },
            ),
            (Layer::User, l4.entry.clone()),
        ],
        &mut warnings,
    );
    let summary = fx.summary();
    let now = fx.protonfix_state();
    let change = protonfix::compare(prior.as_ref().map(|s| s.protonfix.as_str()), &now);
    if prior.is_some() && change.rerun() {
        notes.push(change.to_string());
    }

    // What runs: the baked exe/args/env with L3 (as owned) and L4 on top.
    let over = CompatEntry {
        exe: fx.effective.exe.as_ref().map(|s| s.value.clone()),
        args: fx.effective.args.as_ref().map(|s| s.value.clone()),
        variant: fx.effective.variant.as_ref().map(|s| s.value.clone()),
        env: fx
            .effective
            .env
            .iter()
            .filter(|(k, s)| match super::envargs::check_key(k) {
                Ok(()) => true,
                Err(why) => {
                    warnings.push(format!(
                        "env {k} ({}): {why}; not applied",
                        s.layer.describe()
                    ));
                    false
                }
            })
            .map(|(k, s)| (k.clone(), s.value.clone()))
            .collect(),
        dll_overrides: fx
            .effective
            .dll_overrides
            .iter()
            .map(|(k, s)| (k.clone(), s.value.clone()))
            .collect(),
        ..Default::default()
    };
    let mut eff: LaunchToml =
        baked
            .effective(&CompatEntry::default(), &over)
            .map_err(|reason| MacAppError::Compat {
                path: l4_path.clone(),
                reason,
            })?;

    // The renderer (design 18 §3): switches, overrides, the game-dir step,
    // all in the dir of the exe this game runs (#199). gogo's cnc-ddraw
    // from an earlier setup leaves the tree's other dirs.
    let ddraw_dir = scan_dir.as_deref().map(|d| eff.exe_dir_in(d));
    let mut rplan = renderer::apply_linux(&renderer_ref, &l1, ddraw_dir.as_deref(), &build.dir);
    if let (Some(root), Some(keep)) = (&scan_dir, &ddraw_dir) {
        let stale = baked
            .ddraw_dirs_in(root)
            .into_iter()
            .chain(eff.ddraw_dirs_in(root));
        for d in stale.filter(|d| d != keep) {
            if ddraw::remove_gogo(&d) {
                rplan
                    .notes
                    .push(format!("removed gogo's cnc-ddraw from {}", d.display()));
            }
        }
    }
    // `wined3d` chosen (L3, L4, a flag, the walk) moves D3D9–12 to wined3d;
    // as L1's own pick it means Proton's defaults (#172).
    if rplan.renderer == Renderer::Wined3d.id()
        && renderer_layer != Layer::Auto
        && !rplan.env.contains_key(PROTON_USE_WINED3D)
    {
        rplan.env.insert(PROTON_USE_WINED3D.into(), "1".into());
        rplan.notes.push(format!("wined3d: {PROTON_USE_WINED3D}=1"));
    }
    if rplan.ddraw == renderer::DdrawStep::LeaveDdrawCompat
        && eff
            .dll_overrides
            .get("ddraw")
            .is_some_and(|v| v.starts_with('n'))
    {
        rplan.warnings.push(format!(
            "ddraw={} loads the game's DDrawCompat, which doesn't run under Wine: drop it from {}",
            eff.dll_overrides["ddraw"],
            l4_path.display()
        ));
    }
    warnings.extend(rplan.warnings.iter().cloned());
    notes.extend(rplan.notes.iter().cloned());
    for (k, v) in &rplan.env {
        eff.env.entry(k.clone()).or_insert(v.clone());
    }
    for (k, v) in &rplan.dll_overrides {
        eff.dll_overrides.entry(k.clone()).or_insert(v.clone());
    }

    // Registry values: L3 (as owned), then L4; again only when they changed.
    let mut registry: Vec<compat::RegistryValue> = Vec::new();
    // GOG's install script first (design 18 §4.9), so these win over it.
    if let (Some(name), Some(dir)) = (&tree, &scan_dir) {
        let ctx = crate::macapp::goggame::Ctx {
            app: &format!(r"C:\GOG\{name}"),
            userdocs: r"C:\users\steamuser\Documents",
            group: &baked.title,
        };
        if let Some(script) = crate::macapp::goggame::load(dir, gogid, &ctx) {
            warnings.extend(script.warnings);
            registry.extend(script.values);
        }
    }
    registry.extend(
        fx.effective
            .registry
            .as_ref()
            .map(|r| r.value.clone())
            .unwrap_or_default(),
    );
    let reg_lines: Vec<String> = registry.iter().map(game::registry_line).collect();
    if !registry.is_empty()
        && (prefix_created || prior.as_ref().is_none_or(|s| s.registry != reg_lines))
    {
        for v in &registry {
            umu.reg_add(v)?;
        }
    }

    // Verbs (R12): only the missing ones, or umu's winetricks installs none.
    let verbs = fx.setup_verbs();
    let pending = fixes::pending_verbs(&dirs.prefix, &verbs);
    let mut verbs_installed = Vec::new();
    if let Some(cmd) =
        fixes::winetricks_command(&umu_run, &build.dir, &dirs.prefix, &umu_id.id, &pending)
    {
        restore_z(&dirs.prefix)?;
        let status = umu.run(&format!("winetricks {}", pending.join(" ")), cmd)?;
        let left = fixes::pending_verbs(&dirs.prefix, &pending);
        verbs_installed = pending
            .iter()
            .filter(|v| !left.contains(v))
            .cloned()
            .collect();
        if !left.is_empty() {
            warnings.push(format!(
                "winetricks (exit {}) didn't install {}: protonfixes may try again at \
                 launch; output in {}",
                status.code().unwrap_or(-1),
                left.join(", "),
                umu.log.display()
            ));
        }
    }
    // Last: anything above may have linked out again (decision 4).
    localize(&dirs.prefix)?;

    let rel = |p: &Path| state_rel(&data_root, p);
    let stamp = crate::macapp::launch::stamp(&raw, l4_raw.as_deref());
    let game_files = game_files(&dirs, tree.as_deref());
    let unix = |w: &str| {
        windows_to_unix(&dirs.prefix, w)
            .map(|p| rel(&p))
            .unwrap_or_else(|| w.to_string())
    };
    let [(_, umu_python), (_, _)] = umu_run.launch_conf();
    // A wrapper hand-written into the file and not one the launcher can run:
    // the game runs without it, and `doctor` names it too.
    let wrapper = match &fx.effective.wrapper {
        Some(w) => match super::envargs::check_wrapper(&w.value) {
            Ok(_) => w.value.clone(),
            Err(why) => {
                warnings.push(format!(
                    "wrapper {:?} in the {}: {why}; running the game without it",
                    w.value,
                    w.layer.describe()
                ));
                Vec::new()
            }
        },
        None => Vec::new(),
    };
    let conf = LaunchConf {
        gogid: gogid.to_string(),
        title: eff.title.clone(),
        umu_id: umu_id.id.clone(),
        stamp: stamp.clone(),
        umu_run: rel(umu_run.umu_run()),
        umu_python,
        proton_dir: rel(&build.dir),
        proton_version: proton::version_of(&build.dir).unwrap_or_default(),
        proton_sha: proton::sha256_of(&build.dir.join("proton")).unwrap_or_default(),
        prefix: rel(&dirs.prefix),
        game_dir: rel(&game_files),
        workdir: unix(&eff.workdir),
        exe: unix(&eff.exe),
        args: super::envargs::launch_args(&eff.args, &fx.effective),
        wrapper,
        env: eff.env.clone(),
        dll_overrides: eff.dll_overrides.clone(),
        renderer: rplan.renderer.clone(),
        protonfixes: fx.disabled_by().is_none(),
        umu_folders: None,
        gogo_path: eff.gogo.path.display().to_string(),
        data_root: data_root.display().to_string(),
        library_game: crate::macapp::launch::in_library(&dirs.root),
    };
    write_if_changed(&dirs.launch_conf, &conf.render())?;
    let state = LinuxState {
        schema: STATE_SCHEMA,
        gogid: gogid.to_string(),
        title: Some(baked.title.clone()),
        runtime: build.reference.clone(),
        renderer: rplan.renderer.clone(),
        umu_id: umu_id.id.clone(),
        protonfix: now,
        verbs,
        registry: reg_lines,
        stamp,
        gogo_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    write_if_changed(&dirs.state_toml, &state.to_toml())?;
    // The launcher and the menu entry (#177): rewritten when the template
    // changed (R30) or the data root moved.
    let exe_now = windows_to_unix(&dirs.prefix, &eff.exe);
    let launcher = launcher::write(
        &dirs,
        gogid,
        &baked.title,
        Some(&game_files),
        exe_now.as_deref(),
    )?;
    if let Some(old) = &launcher.updated_from {
        let new = launcher::LAUNCHER_VERSION.to_string();
        notes.push(if *old == new {
            format!("launch.sh: launcher {new} rewritten")
        } else {
            format!("launch.sh: launcher {old} -> {new}")
        });
    }
    Ok(SetupOutcome {
        launcher,
        state,
        conf,
        prefix_created,
        runtime_installed,
        umu_id,
        fixes: summary,
        verbs_installed,
        notes,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn run_limited_stops_what_never_ends() {
        let tmp = tempfile::tempdir().unwrap();
        let run = UmuRun::System {
            umu_run: "umu-run".into(),
        };
        let umu = Umu {
            run: &run,
            proton: tmp.path(),
            prefix: tmp.path(),
            umu_id: "umu-1",
            cwd: tmp.path(),
            log: tmp.path().join("setup.log"),
        };
        let mut sleep = Command::new("sleep");
        sleep.arg("30");
        let started = Instant::now();
        let ran = umu
            .run_limited("a hang", sleep, Duration::from_millis(300))
            .unwrap();
        assert!(ran.is_none());
        assert!(started.elapsed() < Duration::from_secs(10));
        let mut quick = Command::new("true");
        quick.arg("x");
        let ran = umu
            .run_limited("quick", quick, Duration::from_secs(20))
            .unwrap();
        assert!(ran.unwrap().success());
        assert!(
            std::fs::read_to_string(tmp.path().join("setup.log"))
                .unwrap()
                .contains("## a hang")
        );
    }

    use super::*;

    #[test]
    fn only_a_changed_catalog_build_with_its_marker_is_reinstalled() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("proton"), "#!/bin/sh\n").unwrap();
        std::fs::write(dir.join("version"), "1 GE-Proton11-12\n").unwrap();
        std::fs::write(dir.join(proton::TOOLMANIFEST), "\"manifest\" {}\n").unwrap();
        let catalog = ProtonRef::Catalog {
            id: "ge-proton".into(),
            version: Some("GE-Proton11-12".into()),
        };
        let path = ProtonRef::Path(dir.to_path_buf());
        // No marker: someone else's, never touched.
        assert!(changed_managed(&catalog, dir).is_none());
        let marker = proton::marker_for(dir, "ge-proton/GE-Proton11-12", "ab").unwrap();
        proton::write_marker(dir, &marker).unwrap();
        assert!(changed_managed(&catalog, dir).is_none());
        std::fs::write(dir.join("proton"), "#!/bin/sh\n# edited\n").unwrap();
        assert!(matches!(
            changed_managed(&catalog, dir),
            Some(proton::BuildProblem::ProtonChanged { .. })
        ));
        assert!(changed_managed(&path, dir).is_none());
    }
}
