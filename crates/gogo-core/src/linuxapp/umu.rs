//! umu-launcher provisioning (design 18 §2.1, #168). umu is required for
//! the Proton runtime. How it runs is asked once and kept in gogo's config
//! as `[linux] umu`:
//!
//! - `"zipapp"`: gogo's pinned release zipapp (catalog `umu/<ver>`), run
//!   through an explicit interpreter, never its shebang. The interpreter is
//!   `GOGO_UMU_PYTHON` when set (the flake's `gogo-linux-app` sets it to its
//!   `steam.buildRuntimeEnv`, R1), else the first `python3` on `PATH` that
//!   reports ≥ 3.10. On NixOS without `GOGO_UMU_PYTHON` the zipapp can't
//!   start the Steam Linux Runtime, so it's refused with the two ways out.
//! - `"system"`: the `umu-run` on `PATH`, unpinned.
//!
//! No answer: a terminal asks ([`prompt_spec`], the plan shows umu's
//! download size); headless, the build fails naming the key and downloads
//! nothing. The result, [`UmuRun`], is what `launch.conf` records as
//! `UMU_PYTHON` / `UMU_RUN`.
//!
//! Everything that looks at the machine takes a [`Host`] (root, env), so
//! tests run the NixOS, Python and `PATH` cases anywhere.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::config::{Config, UmuMode};
use crate::engine::events::ProgressEvent;
use crate::macapp::catalog::{self, CatalogEntry};
use crate::macapp::error::MacAppError;
use crate::macapp::prompt::{PromptAnswer, PromptFn, PromptKind, PromptSpec};
use crate::macapp::store;

/// The config key, as the user writes it.
pub const KEY: &str = "[linux] umu";

/// The interpreter the flake's `gogo-linux-app` records for the zipapp (R1): its
/// `steam.buildRuntimeEnv` with `runScript = "python3"`.
pub const PYTHON_ENV: &str = "GOGO_UMU_PYTHON";

/// umu's `requires-python` (pyproject.toml at 1.4.4: `>=3.10`).
pub const MIN_PYTHON: (u32, u32) = (3, 10);

/// The flake package that runs the zipapp in an FHS env on NixOS.
pub const FLAKE_PACKAGE: &str = "github:noosxe/gogo#gogo-linux-app";

/// The Steam Linux Runtime umu fetches on a game's first launch, for the
/// plan (docs/21 §1.2 and §8 item 1: steamrt4, by HTTP HEAD and unpacked).
const CONTAINER_DOWNLOAD: &str = "175 MB";
const CONTAINER_UNPACKED: &str = "699 MB";

/// The machine as provisioning sees it. [`Host::real`] outside tests.
pub struct Host<'a> {
    /// `/` on a real machine: `etc/NIXOS` is looked up under it.
    pub root: &'a Path,
    /// Environment lookup (`GOGO_UMU_PYTHON`, `PATH`).
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
}

fn real_env(key: &str) -> Option<OsString> {
    std::env::var_os(key)
}

impl Host<'static> {
    pub fn real() -> Self {
        Host {
            root: Path::new("/"),
            env: &real_env,
        }
    }
}

impl Host<'_> {
    /// NixOS marks its root with `/etc/NIXOS`.
    pub fn is_nixos(&self) -> bool {
        self.root.join("etc/NIXOS").exists()
    }

    pub(crate) fn var(&self, key: &str) -> Option<OsString> {
        (self.env)(key).filter(|v| !v.is_empty())
    }

    /// The absolute directories of `PATH`, in order.
    fn path_dirs(&self) -> Vec<PathBuf> {
        self.var("PATH")
            .map(|p| {
                std::env::split_paths(&p)
                    .filter(|d| d.is_absolute())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every executable `name` on `PATH`, in order.
    fn on_path(&self, name: &str) -> Vec<PathBuf> {
        self.path_dirs()
            .into_iter()
            .map(|d| d.join(name))
            .filter(|p| is_executable(p))
            .collect()
    }
}

#[cfg(unix)]
pub(crate) fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
pub(crate) fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// How umu runs, resolved: what `launch.conf` records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UmuRun {
    /// `<python> <zipapp> …`.
    Zipapp { python: PathBuf, zipapp: PathBuf },
    /// An unpinned `umu-run` from `PATH`, by absolute path.
    System { umu_run: PathBuf },
}

impl UmuRun {
    /// `UMU_PYTHON`: the interpreter; `None` (empty) for a system umu.
    pub fn umu_python(&self) -> Option<&Path> {
        match self {
            UmuRun::Zipapp { python, .. } => Some(python),
            UmuRun::System { .. } => None,
        }
    }

    /// `UMU_RUN`: the store zipapp or the system `umu-run`.
    pub fn umu_run(&self) -> &Path {
        match self {
            UmuRun::Zipapp { zipapp, .. } => zipapp,
            UmuRun::System { umu_run } => umu_run,
        }
    }

    /// The `launch.conf` lines, in design 18 §2.1's table's terms.
    pub fn launch_conf(&self) -> [(&'static str, String); 2] {
        [
            (
                "UMU_PYTHON",
                self.umu_python()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            ),
            ("UMU_RUN", self.umu_run().display().to_string()),
        ]
    }

    /// A command running umu with nothing else set: the interpreter and
    /// the zipapp, or the system `umu-run`.
    pub fn command(&self) -> std::process::Command {
        match self {
            UmuRun::Zipapp { python, zipapp } => {
                let mut c = std::process::Command::new(python);
                c.arg(zipapp);
                c
            }
            UmuRun::System { umu_run } => std::process::Command::new(umu_run),
        }
    }
}

/// The interpreter for the zipapp (see the module docs). The error says
/// what to do instead.
pub fn resolve_python(host: &Host) -> Result<PathBuf, MacAppError> {
    if let Some(p) = host.var(PYTHON_ENV) {
        // The flake's FHS env: trusted as built, not run here (running it
        // starts bwrap). It only has to be there.
        let p = PathBuf::from(p);
        if p.is_absolute() && is_executable(&p) {
            return Ok(p);
        }
        return Err(MacAppError::Umu(format!(
            "{PYTHON_ENV} is {}, which is not an executable file: point it at a \
             Python >= {}.{} (the flake's gogo-linux-app sets it), or unset it",
            p.display(),
            MIN_PYTHON.0,
            MIN_PYTHON.1
        )));
    }
    if host.is_nixos() {
        return Err(MacAppError::Umu(format!(
            "on NixOS umu's zipapp can't start the Steam Linux Runtime without an FHS \
             env, and this gogo doesn't provide one ({PYTHON_ENV} is unset). Either \
             install gogo's Linux game package (`nix profile install {FLAKE_PACKAGE}`), \
             which runs umu in steam.buildRuntimeEnv, or install nixpkgs' \
             umu-launcher and set {KEY} = \"system\""
        )));
    }
    let candidates = host.on_path("python3");
    let mut seen = Vec::new();
    for p in &candidates {
        match python_version(p) {
            Some(v) if v >= MIN_PYTHON => return Ok(p.clone()),
            Some((major, minor)) => seen.push(format!("{} is {major}.{minor}", p.display())),
            None => seen.push(format!("{} reports no version", p.display())),
        }
    }
    let found = if seen.is_empty() {
        "no python3 on PATH".to_string()
    } else {
        seen.join(", ")
    };
    Err(MacAppError::Umu(format!(
        "umu's zipapp needs Python >= {}.{} ({found}): install Python {}.{} or newer, \
         or install your distribution's umu-launcher and set {KEY} = \"system\"",
        MIN_PYTHON.0, MIN_PYTHON.1, MIN_PYTHON.0, MIN_PYTHON.1
    )))
}

/// `python --version`, retried while the kernel says `ETXTBSY`. That only
/// means some process still has the file open for writing right now (a
/// concurrent fork can hold a copy of a writer's fd until its `exec`, so
/// it can hit a freshly installed interpreter, or a test's fake one); it
/// clears within microseconds. Cargo and Go retry the same way.
fn run_version(python: &Path) -> std::io::Result<std::process::Output> {
    let mut tries = 0;
    loop {
        let r = std::process::Command::new(python)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .output();
        match r {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 50 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            r => return r,
        }
    }
}

/// `python --version` as `(major, minor)`. Python 2 prints it on stderr.
pub(crate) fn python_version(python: &Path) -> Option<(u32, u32)> {
    let out = run_version(python).ok()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    parse_python_version(&text)
}

fn parse_python_version(text: &str) -> Option<(u32, u32)> {
    let rest = text.trim().strip_prefix("Python ")?;
    let mut parts = rest.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor: String = parts
        .next()?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    Some((major, minor.parse().ok()?))
}

/// `umu = "system"`: the first `umu-run` on `PATH`, absolute.
pub fn resolve_system(host: &Host) -> Result<PathBuf, MacAppError> {
    host.on_path("umu-run").into_iter().next().ok_or_else(|| {
        MacAppError::Umu(format!(
            "{KEY} = \"system\" needs umu-run on PATH: install your distribution's \
             umu-launcher (nixpkgs: umu-launcher), or set {KEY} = \"zipapp\" for gogo's \
             pinned copy"
        ))
    })
}

/// The stored answer, if any. An unreadable config or another value counts
/// as no answer.
pub fn stored_at(path: &Path) -> Option<UmuMode> {
    match super::stored_key(path, "umu")?.as_str() {
        "zipapp" => Some(UmuMode::Zipapp),
        "system" => Some(UmuMode::System),
        _ => None,
    }
}

/// Write the answer into the config, keeping everything else in the file.
pub fn store_at(path: &Path, mode: UmuMode) -> Result<(), MacAppError> {
    super::store_key(path, "umu", mode.as_str())
}

fn pin(entry: &CatalogEntry) -> Option<&catalog::PinnedAsset> {
    match &entry.discovery {
        Some(catalog::Discovery::Pinned(pin)) => Some(pin),
        _ => None,
    }
}

/// The plan's umu line: the download (`bytes`, from the catalog probe), its
/// licence, and the container umu fetches itself on first launch.
pub fn plan_line(entry: &CatalogEntry, bytes: u64) -> String {
    let (version, repo) = pin(entry).map_or(("?", "?"), |p| (p.version, p.repo));
    format!(
        "umu-launcher {version}: {} download from github.com/{repo}, kept in gogo's \
         store; licence {}. On a game's first launch umu fetches the Steam Linux \
         Runtime itself (about {CONTAINER_DOWNLOAD}, {CONTAINER_UNPACKED} unpacked)",
        crate::fmt::human_size(bytes),
        entry.license
    )
}

/// The question: zipapp (default) or system. `zipapp_blocker`: why the
/// zipapp won't run here ([`resolve_python`]'s error), shown on its item;
/// with one, and a `umu-run` on `PATH`, system is the default.
pub fn prompt_spec(
    entry: &CatalogEntry,
    bytes: u64,
    config: &Path,
    zipapp_blocker: Option<&str>,
    system_found: bool,
) -> PromptSpec {
    let version = pin(entry).map_or("?", |p| p.version);
    let mut zipapp = format!(
        "zipapp: download umu-launcher {version} ({}) into gogo's store",
        crate::fmt::human_size(bytes)
    );
    if let Some(why) = zipapp_blocker {
        zipapp.push_str(&format!(" (won't run here: {why})"));
    }
    let mut system =
        "system: use the umu-run on PATH (your distribution's umu-launcher)".to_string();
    if !system_found {
        system.push_str(" (none on PATH now)");
    }
    PromptSpec {
        kind: PromptKind::Select,
        title: format!(
            "{}.\nHow should gogo run umu-launcher? (asked once; saved as {KEY} in {})",
            plan_line(entry, bytes),
            config.display()
        ),
        items: vec![zipapp, system],
        allow_other: false,
        allow_none: false,
        default: usize::from(zipapp_blocker.is_some() && system_found),
    }
}

/// `[linux] umu` unset and no terminal to ask on.
pub fn unset_error(config: &Path) -> MacAppError {
    MacAppError::Umu(format!(
        "{KEY} is unset in {} and there's no terminal to ask on: set {KEY} = \
         \"zipapp\" (gogo downloads its pinned umu-launcher) or \"system\" (the \
         umu-run on PATH), or run once on a terminal to be asked",
        config.display()
    ))
}

/// The stored mode, or ask with `spec` and store the answer. Headless with
/// no answer: [`unset_error`]. A cancelled prompt stores nothing.
pub fn mode_at(
    config: &Path,
    prompt: Option<&PromptFn>,
    spec: impl FnOnce() -> Result<PromptSpec, MacAppError>,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<UmuMode, MacAppError> {
    if let Some(mode) = stored_at(config) {
        return Ok(mode);
    }
    let Some(prompt) = prompt else {
        return Err(unset_error(config));
    };
    let mode = match prompt(&spec()?) {
        Some(PromptAnswer::Index(0)) => UmuMode::Zipapp,
        Some(PromptAnswer::Index(1)) => UmuMode::System,
        _ => return Err(MacAppError::Umu(format!("{KEY}: no answer, cancelled"))),
    };
    if let Err(e) = store_at(config, mode) {
        emit(&ProgressEvent::Warn {
            message: format!("could not save {KEY} in {}: {e}", config.display()),
        });
    }
    Ok(mode)
}

/// Resolve how umu runs (see the module docs), downloading the pinned
/// zipapp if `[linux] umu = "zipapp"` and it isn't cached. The interpreter
/// is checked first: nothing is downloaded that couldn't run.
pub async fn provision(
    prompt: Option<&PromptFn>,
    host: &Host<'_>,
    http: &reqwest::Client,
    api_base: &str,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<UmuRun, MacAppError> {
    let entry = catalog::find(catalog::UMU).expect("the catalog has umu");
    provision_with(entry, &Config::path(), prompt, host, http, api_base, emit).await
}

async fn provision_with(
    entry: &CatalogEntry,
    config: &Path,
    prompt: Option<&PromptFn>,
    host: &Host<'_>,
    http: &reqwest::Client,
    api_base: &str,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<UmuRun, MacAppError> {
    let mode = match stored_at(config) {
        Some(mode) => mode,
        None if prompt.is_none() => return Err(unset_error(config)),
        None => {
            // The size comes from the probe the install would make anyway.
            let size = catalog::probe(entry, None, http, api_base).await?.size;
            let blocker = resolve_python(host).err().map(|e| match e {
                MacAppError::Umu(why) => why,
                e => e.to_string(),
            });
            let found = resolve_system(host).is_ok();
            mode_at(
                config,
                prompt,
                || Ok(prompt_spec(entry, size, config, blocker.as_deref(), found)),
                emit,
            )?
        }
    };
    match mode {
        UmuMode::System => Ok(UmuRun::System {
            umu_run: resolve_system(host)?,
        }),
        UmuMode::Zipapp => {
            let python = resolve_python(host)?;
            let zipapp = ensure_zipapp(entry, http, api_base, emit).await?;
            Ok(UmuRun::Zipapp { python, zipapp })
        }
    }
}

/// The cached zipapp of `entry`'s pin, or install it.
async fn ensure_zipapp(
    entry: &CatalogEntry,
    http: &reqwest::Client,
    api_base: &str,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<PathBuf, MacAppError> {
    let version = pin(entry).map(|p| p.version);
    if let Some((m, root)) = store::installed(entry.id, version) {
        return Ok(root.join(&m.entry_rel));
    }
    let m = store::install(entry, None, http, api_base, false, emit).await?;
    Ok(store::tree_root(&m).join(&m.entry_rel))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest as _, Sha256};
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A fake executable printing `out` for `--version`.
    fn script(dir: &Path, name: &str, out: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\necho '{out}'\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    struct Fake {
        root: tempfile::TempDir,
        env: HashMap<&'static str, OsString>,
    }

    impl Fake {
        fn new() -> Self {
            Fake {
                root: tempfile::tempdir().unwrap(),
                env: HashMap::new(),
            }
        }
        fn dir(&self, d: &str) -> PathBuf {
            self.root.path().join(d)
        }
        fn nixos(self) -> Self {
            script(&self.dir("etc"), "NIXOS", "");
            self
        }
        fn path(mut self, dirs: &[&str]) -> Self {
            let dirs: Vec<PathBuf> = dirs.iter().map(|d| self.dir(d)).collect();
            self.env.insert("PATH", std::env::join_paths(dirs).unwrap());
            self
        }
        fn set(mut self, k: &'static str, v: impl Into<OsString>) -> Self {
            self.env.insert(k, v.into());
            self
        }
        fn with<R>(&self, f: impl FnOnce(&Host) -> R) -> R {
            let env = |k: &str| self.env.get(k).cloned();
            f(&Host {
                root: self.root.path(),
                env: &env,
            })
        }
    }

    fn umu_err(r: Result<PathBuf, MacAppError>) -> String {
        match r {
            Err(MacAppError::Umu(why)) => why,
            other => panic!("expected a umu error, got {other:?}"),
        }
    }

    #[test]
    fn python_versions_parse() {
        assert_eq!(parse_python_version("Python 3.12.15\n"), Some((3, 12)));
        assert_eq!(parse_python_version("Python 3.10.0rc1"), Some((3, 10)));
        assert_eq!(parse_python_version("Python 2.7.18"), Some((2, 7)));
        assert_eq!(parse_python_version("Python 3.14+"), Some((3, 14)));
        assert_eq!(parse_python_version("pypy"), None);
        assert!((3, 9) < MIN_PYTHON && (3, 10) >= MIN_PYTHON && (4, 0) > MIN_PYTHON);
    }

    #[test]
    fn first_python3_on_path_new_enough_wins() {
        let f = Fake::new().path(&["old", "none", "new", "newer"]);
        script(&f.dir("old"), "python3", "Python 3.9.18");
        let new = script(&f.dir("new"), "python3", "Python 3.10.12");
        script(&f.dir("newer"), "python3", "Python 3.13.1");
        assert_eq!(f.with(resolve_python).unwrap(), new);
    }

    #[test]
    fn no_python_new_enough_says_what_was_found_and_the_way_out() {
        let f = Fake::new().path(&["old"]);
        let old = script(&f.dir("old"), "python3", "Python 3.8.10");
        let why = umu_err(f.with(resolve_python));
        assert!(why.contains(&format!("{} is 3.8", old.display())), "{why}");
        assert!(why.contains("Python >= 3.10") && why.contains("umu = \"system\""));
        let why = umu_err(Fake::new().path(&["empty"]).with(resolve_python));
        assert!(why.contains("no python3 on PATH"), "{why}");
        let why = umu_err(Fake::new().with(resolve_python));
        assert!(why.contains("no python3 on PATH"), "{why}");
    }

    #[test]
    fn a_non_executable_python3_is_skipped() {
        use std::os::unix::fs::PermissionsExt as _;
        let f = Fake::new().path(&["a", "b"]);
        let a = script(&f.dir("a"), "python3", "Python 3.12.1");
        std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o644)).unwrap();
        let b = script(&f.dir("b"), "python3", "Python 3.11.2");
        assert_eq!(f.with(resolve_python).unwrap(), b);
    }

    #[test]
    fn gogo_umu_python_wins_on_nixos_and_elsewhere() {
        let f = Fake::new().nixos().path(&["bin"]);
        script(&f.dir("bin"), "python3", "Python 3.12.1");
        let env = script(&f.dir("fhs/bin"), "gogo-umu-python", "never run");
        let f = f.set(PYTHON_ENV, env.as_os_str());
        assert_eq!(f.with(resolve_python).unwrap(), env);
        // Not there: named, not silently replaced by PATH's.
        let f = f.set(PYTHON_ENV, "/no/such/python");
        let why = umu_err(f.with(resolve_python));
        assert!(
            why.contains(PYTHON_ENV) && why.contains("/no/such/python"),
            "{why}"
        );
    }

    #[test]
    fn nixos_without_the_flake_python_refuses_the_zipapp() {
        let f = Fake::new().nixos().path(&["bin"]);
        // A fine Python on PATH doesn't help: the container can't start.
        script(&f.dir("bin"), "python3", "Python 3.12.1");
        let why = umu_err(f.with(resolve_python));
        assert!(why.contains("NixOS"), "{why}");
        assert!(
            why.contains("nix profile install github:noosxe/gogo#gogo-linux-app"),
            "{why}"
        );
        assert!(why.contains("[linux] umu = \"system\""), "{why}");
        // An empty GOGO_UMU_PYTHON is unset.
        let f = f.set(PYTHON_ENV, "");
        assert!(umu_err(f.with(resolve_python)).contains("NixOS"));
    }

    #[test]
    fn system_umu_is_the_first_umu_run_on_path_absolute() {
        let f = Fake::new().path(&["a", "b", "c"]);
        let b = script(&f.dir("b"), "umu-run", "");
        script(&f.dir("c"), "umu-run", "");
        let got = f.with(resolve_system).unwrap();
        assert_eq!(got, b);
        assert!(got.is_absolute());
        // Relative PATH entries are never used.
        let f = Fake::new().set("PATH", "relative:./bin");
        let why = umu_err(f.with(resolve_system));
        assert!(why.contains("umu-run on PATH") && why.contains("umu = \"zipapp\""));
        // NixOS doesn't matter for a system umu.
        let f = Fake::new().nixos().path(&["b"]);
        let b = script(&f.dir("b"), "umu-run", "");
        assert_eq!(f.with(resolve_system).unwrap(), b);
    }

    #[test]
    fn launch_conf_values_follow_the_design_table() {
        let z = UmuRun::Zipapp {
            python: "/nix/store/x-gogo-umu-python/bin/gogo-umu-python".into(),
            zipapp: "/d/gogo/runtimes/umu/1.4.4/umu/umu-run".into(),
        };
        assert_eq!(
            z.launch_conf(),
            [
                (
                    "UMU_PYTHON",
                    "/nix/store/x-gogo-umu-python/bin/gogo-umu-python".to_string()
                ),
                (
                    "UMU_RUN",
                    "/d/gogo/runtimes/umu/1.4.4/umu/umu-run".to_string()
                ),
            ]
        );
        let c = z.command();
        assert_eq!(
            c.get_program(),
            "/nix/store/x-gogo-umu-python/bin/gogo-umu-python"
        );
        assert_eq!(
            c.get_args().collect::<Vec<_>>(),
            ["/d/gogo/runtimes/umu/1.4.4/umu/umu-run"]
        );
        let s = UmuRun::System {
            umu_run: "/usr/bin/umu-run".into(),
        };
        assert_eq!(
            s.launch_conf(),
            [
                ("UMU_PYTHON", String::new()),
                ("UMU_RUN", "/usr/bin/umu-run".to_string())
            ]
        );
        assert_eq!(s.command().get_args().count(), 0);
    }

    #[test]
    fn plan_and_prompt_show_the_download_size() {
        let entry = catalog::find(catalog::UMU).unwrap();
        let line = plan_line(entry, 430_080);
        assert!(
            line.starts_with("umu-launcher 1.4.4: 420.0 KiB download"),
            "{line}"
        );
        assert!(line.contains("Open-Wine-Components/umu-launcher"), "{line}");
        assert!(
            line.contains("GPL-3.0") && line.contains("175 MB"),
            "{line}"
        );
        let cfg = Path::new("/home/u/.config/gogo/config.toml");
        let spec = prompt_spec(entry, 430_080, cfg, None, false);
        assert_eq!(spec.kind, PromptKind::Select);
        assert!(spec.title.contains(&line), "{}", spec.title);
        assert!(spec.title.contains(KEY) && spec.title.contains("config.toml"));
        assert_eq!(spec.items.len(), 2);
        assert!(spec.items[0].starts_with("zipapp:") && spec.items[0].contains("420.0 KiB"));
        assert!(spec.items[1].starts_with("system:") && spec.items[1].contains("none on PATH"));
        assert_eq!(spec.default, 0);
        // The zipapp won't run here but a umu-run is on PATH: system first.
        let spec = prompt_spec(entry, 430_080, cfg, Some("on NixOS …"), true);
        assert!(spec.items[0].contains("won't run here: on NixOS"));
        assert!(!spec.items[1].contains("none on PATH"));
        assert_eq!(spec.default, 1);
        // Nothing better to offer: zipapp stays the default.
        assert_eq!(prompt_spec(entry, 1, cfg, Some("x"), false).default, 0);
    }

    fn fake_prompt(
        answer: Option<PromptAnswer>,
        asked: &Arc<AtomicUsize>,
    ) -> impl Fn(&PromptSpec) -> Option<PromptAnswer> + Send + Sync + 'static {
        let asked = asked.clone();
        move |spec| {
            asked.fetch_add(1, Ordering::SeqCst);
            assert_eq!(spec.kind, PromptKind::Select);
            answer.clone()
        }
    }

    fn spec() -> Result<PromptSpec, MacAppError> {
        Ok(prompt_spec(
            catalog::find(catalog::UMU).unwrap(),
            430_080,
            Path::new("/c"),
            None,
            false,
        ))
    }

    #[test]
    fn asked_once_and_stored_keeping_the_rest_of_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# mine\n[download]\nworkers = 4\n").unwrap();
        let asked = Arc::new(AtomicUsize::new(0));
        let p = fake_prompt(Some(PromptAnswer::Index(1)), &asked);
        for _ in 0..2 {
            let mode = mode_at(&path, Some(&p), spec, &mut |_| {}).unwrap();
            assert_eq!(mode, UmuMode::System);
        }
        assert_eq!(asked.load(Ordering::SeqCst), 1, "asked once");
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains("# mine") && raw.contains("workers = 4"),
            "{raw}"
        );
        let loaded = Config::load_from(&path, |_| None).unwrap();
        assert_eq!(loaded.config.linux.umu, Some(UmuMode::System));
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    }

    #[test]
    fn headless_without_an_answer_fails_naming_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let err = mode_at(&path, None, || panic!("no spec headless"), &mut |_| {}).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("[linux] umu is unset"), "{msg}");
        assert!(msg.contains("no terminal"), "{msg}");
        assert!(msg.contains(&path.display().to_string()), "{msg}");
        assert!(!path.exists(), "headless writes nothing");
        // A stored answer needs no terminal.
        std::fs::write(&path, "[linux]\numu = \"zipapp\"\n").unwrap();
        assert_eq!(
            mode_at(&path, None, || panic!("not asked"), &mut |_| {}).unwrap(),
            UmuMode::Zipapp
        );
    }

    #[test]
    fn cancel_stores_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let asked = Arc::new(AtomicUsize::new(0));
        let p = fake_prompt(None, &asked);
        let err = mode_at(&path, Some(&p), spec, &mut |_| {}).unwrap_err();
        assert!(err.to_string().contains("cancelled"));
        assert!(!path.exists());
        assert_eq!(stored_at(&path), None);
    }

    // — provisioning against a mocked GitHub —

    fn sha256_hex(b: &[u8]) -> String {
        Sha256::digest(b)
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect()
    }

    /// The release archive's shape: `umu/umu-run` and its `.py` symlink.
    fn zipapp_tar() -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut t = tar::Builder::new(&mut buf);
            let body = b"#!/usr/bin/env python3\nPK fake zipapp";
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o744);
            h.set_cksum();
            t.append_data(&mut h, "umu/umu-run", &body[..]).unwrap();
            let mut l = tar::Header::new_gnu();
            l.set_entry_type(tar::EntryType::Symlink);
            l.set_size(0);
            l.set_mode(0o777);
            t.append_link(&mut l, "umu/umu_run.py", "umu-run").unwrap();
            t.finish().unwrap();
        }
        buf
    }

    /// wiremock as GitHub for a umu entry pinned to `body`; the download
    /// expects exactly `hits` requests.
    async fn mock_umu(body: &[u8], hits: u64) -> (wiremock::MockServer, &'static CatalogEntry) {
        use wiremock::matchers::{method, path};
        let server = wiremock::MockServer::start().await;
        let sha = sha256_hex(body);
        let asset = "umu-launcher-1.4.4-zipapp.tar";
        wiremock::Mock::given(method("GET"))
            .and(path(
                "/repos/Open-Wine-Components/umu-launcher/releases/tags/1.4.4",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "tag_name": "1.4.4", "prerelease": false,
                    "published_at": "2026-07-25T00:00:00Z",
                    "assets": [{
                        "name": asset, "size": body.len(),
                        "browser_download_url": format!("{}/download/{asset}", server.uri()),
                        "digest": format!("sha256:{sha}"),
                    }],
                })),
            )
            .mount(&server)
            .await;
        wiremock::Mock::given(method("GET"))
            .and(path(format!("/download/{asset}")))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
            .expect(hits)
            .mount(&server)
            .await;
        let real = catalog::find(catalog::UMU).unwrap();
        let entry = Box::leak(Box::new(CatalogEntry {
            id: real.id,
            os: real.os,
            kind: catalog::Kind::Tool {
                payload: "umu-run",
                notice: Some(catalog::UMU_NOTICE),
                runtimes: catalog::umu_runtimes(),
            },
            status: catalog::Status::Tool,
            discovery: Some(catalog::Discovery::Pinned(catalog::PinnedAsset {
                repo: "Open-Wine-Components/umu-launcher",
                tag: "1.4.4",
                asset,
                version: "1.4.4",
                sha256: Box::leak(sha.into_boxed_str()),
            })),
            license: real.license,
            redistributable: true,
            min_macos: None,
            arch: real.arch,
            vendor_signed: false,
            note: "test",
            frameworks: None,
        }));
        (server, entry)
    }

    fn env_home(dir: &Path) -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::test_util::env_lock();
        // SAFETY: env mutation under the shared env mutex.
        unsafe { std::env::set_var("GOGO_HOME", dir) };
        guard
    }

    #[tokio::test]
    // The env guard must span the awaits: GOGO_HOME is read throughout.
    // Safe: the test runtime is single-threaded and no other task touches env.
    #[allow(clippy::await_holding_lock)]
    async fn zipapp_is_asked_downloaded_once_and_resolved() {
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        let cfg = home.path().join("config.toml");
        let body = zipapp_tar();
        let (server, entry) = mock_umu(&body, 1).await;
        let http = catalog::http_client().unwrap();
        let f = Fake::new().path(&["bin"]);
        let py = script(&f.dir("bin"), "python3", "Python 3.12.15");
        let asked = Arc::new(AtomicUsize::new(0));
        let size = body.len();
        let p = {
            let asked = asked.clone();
            move |spec: &PromptSpec| {
                asked.fetch_add(1, Ordering::SeqCst);
                // The plan shows the probed download size.
                let shown = crate::fmt::human_size(size as u64);
                assert!(spec.title.contains(&shown), "{}", spec.title);
                Some(PromptAnswer::Index(0))
            }
        };
        let env = |k: &str| f.env.get(k).cloned();
        let host = Host {
            root: f.root.path(),
            env: &env,
        };
        for _ in 0..2 {
            let run = provision_with(
                entry,
                &cfg,
                Some(&p),
                &host,
                &http,
                &server.uri(),
                &mut |_| {},
            )
            .await
            .unwrap();
            let UmuRun::Zipapp { python, zipapp } = &run else {
                panic!("zipapp");
            };
            assert_eq!(python, &py);
            assert!(
                zipapp.ends_with("runtimes/umu/1.4.4/umu/umu-run"),
                "{}",
                zipapp.display()
            );
            assert!(zipapp.is_file());
            let notices = std::fs::read_to_string(
                zipapp
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("NOTICES.txt"),
            )
            .unwrap();
            assert!(notices.contains("GPL-3.0") && notices.contains("separate program"));
        }
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert_eq!(stored_at(&cfg), Some(UmuMode::Zipapp));
        // `expect(1)` on the download: the second run used the cache.
        server.verify().await;
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn nothing_is_downloaded_that_could_not_run() {
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        let cfg = home.path().join("config.toml");
        std::fs::write(&cfg, "[linux]\numu = \"zipapp\"\n").unwrap();
        let (server, entry) = mock_umu(&zipapp_tar(), 0).await;
        let http = catalog::http_client().unwrap();
        let f = Fake::new().nixos().path(&["bin"]);
        script(&f.dir("bin"), "python3", "Python 3.12.15");
        let env = |k: &str| f.env.get(k).cloned();
        let host = Host {
            root: f.root.path(),
            env: &env,
        };
        let err = provision_with(entry, &cfg, None, &host, &http, &server.uri(), &mut |_| {})
            .await
            .unwrap_err();
        assert!(err.to_string().contains("NixOS"), "{err}");
        server.verify().await;
        assert!(store::load_manifest().is_empty());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn system_and_headless_touch_no_network() {
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        let cfg = home.path().join("config.toml");
        // No mocks mounted: any request would fail the call.
        let server = wiremock::MockServer::start().await;
        let http = catalog::http_client().unwrap();
        let entry = catalog::find(catalog::UMU).unwrap();
        let f = Fake::new().nixos().path(&["bin"]);
        let umu = script(&f.dir("bin"), "umu-run", "");
        let env = |k: &str| f.env.get(k).cloned();
        let host = Host {
            root: f.root.path(),
            env: &env,
        };
        let err = provision_with(entry, &cfg, None, &host, &http, &server.uri(), &mut |_| {})
            .await
            .unwrap_err();
        assert!(err.to_string().contains("[linux] umu is unset"), "{err}");
        std::fs::write(&cfg, "[linux]\numu = \"system\"\n").unwrap();
        let run = provision_with(entry, &cfg, None, &host, &http, &server.uri(), &mut |_| {})
            .await
            .unwrap();
        assert_eq!(run, UmuRun::System { umu_run: umu });
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}
