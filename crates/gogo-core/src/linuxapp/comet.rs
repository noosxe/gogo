//! Comet provisioning (design 23 decision 4, #487). Comet is the open
//! stand-in for the GOG Galaxy service that runs beside a game, so the
//! Galaxy SDK has a signed-in user. Where it comes from is `[linux] comet`:
//!
//! - `fetch` (the default): gogo's pinned release binary (catalog
//!   `comet/<ver>`), downloaded only on an explicit install or a consenting
//!   plan. `GOGO_COMET` wins over the store: the flake's `gogo-linux-app`
//!   points it at a Comet built from source, because the release binary
//!   links glibc and doesn't start on NixOS.
//! - `system`: the `comet` on `PATH`, unpinned.
//! - `path:<file>`: that file.
//!
//! Starting Comet and handing it the session are [`super::galaxy`]'s (#488);
//! stopping it is the launcher's. Everything that looks at the machine takes
//! a [`Host`].

use std::path::{Path, PathBuf};

use super::umu::{Host, is_executable};
use crate::config::CometMode;
use crate::engine::events::ProgressEvent;
use crate::macapp::catalog::{self, CatalogEntry};
use crate::macapp::error::MacAppError;
use crate::macapp::store;

/// The config key, as the user writes it.
pub const KEY: &str = "[linux] comet";

/// The Comet the flake's `gogo-linux-app` records: its source build.
pub const COMET_ENV: &str = "GOGO_COMET";

/// The flake package that carries a runnable Comet on NixOS.
pub const FLAKE_PACKAGE: &str = "github:noosxe/gogo#gogo-linux-app";

/// What Comet fetches itself on its first start, for the plan line:
/// the GalaxyPeer redist (design 23 decision 4).
const REDIST_DOWNLOAD: &str = "100 MiB";

/// The Comet version design 23 decision 5's credential-file contract was
/// checked against: `--from-heroic` reads `$XDG_CONFIG_PATH/heroic/gog_store/auth.json`
/// (JSON object keyed by Comet's Galaxy client id, holding `access_token`,
/// `refresh_token` and `user_id`; `HOME` must be set).
/// Bumping the catalog pin fails [`tests::the_pin_is_the_version_the_contract_was_checked_against`]
/// until the contract is re-read at the new tag and this is bumped with it.
#[cfg(test)]
const CONTRACT_CHECKED_AGAINST: &str = "0.4.1";

/// Where a resolved Comet came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `GOGO_COMET`: the flake's build.
    Env,
    /// gogo's pinned release in its store.
    Store,
    /// The `comet` on `PATH`.
    System,
    /// `path:<file>`.
    Path,
}

/// A Comet binary to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CometBin {
    pub path: PathBuf,
    pub source: Source,
}

fn pin(entry: &CatalogEntry) -> Option<&catalog::PinnedAsset> {
    match &entry.discovery {
        Some(catalog::Discovery::Pinned(pin)) => Some(pin),
        _ => None,
    }
}

fn entry() -> &'static CatalogEntry {
    catalog::find(catalog::COMET).expect("the catalog has comet")
}

/// The configured mode; unset is `fetch`.
pub fn mode(config: &crate::config::Config) -> CometMode {
    config.linux.comet.clone().unwrap_or(CometMode::Fetch)
}

/// The mode stored in the config at `path`: unset is `fetch`; an unreadable
/// file counts as unset, a value that doesn't parse is an error.
pub fn mode_at(path: &Path) -> Result<CometMode, MacAppError> {
    match super::stored_key(path, "comet") {
        None => Ok(CometMode::Fetch),
        Some(raw) => raw
            .parse()
            .map_err(|why| MacAppError::Comet(format!("{KEY}: {why}"))),
    }
}

/// `runtimes list`'s Comet line: the mode, and the Comet it resolves to
/// without a download, if any.
pub fn status_line(host: &Host, config: &Path) -> String {
    let mode = match mode_at(config) {
        Ok(m) => m,
        Err(e) => return e.to_string(),
    };
    let how = match available(host, &mode) {
        Ok(Some(bin)) => {
            let from = match bin.source {
                Source::Env => GOGO_COMET_LABEL,
                Source::Store => "gogo's store",
                Source::System => "PATH",
                Source::Path => "path:",
            };
            format!("{} ({from})", bin.path.display())
        }
        Ok(None) => "not installed: `gogo runtimes install comet`".to_string(),
        Err(e) => e.to_string(),
    };
    format!("comet ({KEY} = \"{}\"): {how}", mode.to_config())
}

const GOGO_COMET_LABEL: &str = "from GOGO_COMET";

/// How the run log names `bin` (design 23 §2.1): `comet <ver>` for gogo's
/// pin (the store's copy, or the flake's build of the same tag), else where
/// it came from.
pub fn label(bin: &CometBin) -> String {
    match bin.source {
        Source::Store | Source::Env => {
            format!("comet {}", pin(entry()).map_or("?", |p| p.version))
        }
        Source::System => "comet (system)".to_string(),
        Source::Path => "comet (path:)".to_string(),
    }
}

/// The plan's Comet line: the download (`bytes`, from the catalog probe), its
/// licence, and what Comet fetches itself on its first start.
pub fn plan_line(entry: &CatalogEntry, bytes: u64) -> String {
    let (version, repo) = pin(entry).map_or(("?", "?"), |p| (p.version, p.repo));
    format!(
        "Comet {version}: {} download from github.com/{repo}, kept in gogo's store; \
         licence {}. On its first start Comet fetches the GalaxyPeer redist itself \
         (about {REDIST_DOWNLOAD})",
        crate::fmt::human_size(bytes),
        entry.license
    )
}

/// `comet = "fetch"` on NixOS without a runnable Comet: the release binary
/// links glibc and has no loader there.
fn nixos_error() -> MacAppError {
    MacAppError::Comet(format!(
        "on NixOS the release binary of Comet doesn't start (it needs glibc's loader), \
         and this gogo doesn't carry a build ({COMET_ENV} is unset). Either install \
         gogo's Linux game package (`nix profile install {FLAKE_PACKAGE}`), which \
         builds Comet from source, or set {KEY} = \"system\" or \"path:<file>\" for \
         a Comet of your own"
    ))
}

/// The pinned binary, made executable: a bare release asset is stored as
/// downloaded, without the execute bit (`gogo runtimes install comet` goes
/// through the generic tool install).
fn make_executable(path: &Path) -> Result<(), MacAppError> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut perm = std::fs::metadata(path)?.permissions();
    if perm.mode() & 0o111 != 0o111 {
        perm.set_mode(perm.mode() | 0o755);
        std::fs::set_permissions(path, perm)?;
    }
    Ok(())
}

/// Find the Comet `mode` names, without downloading. `installed` is the
/// store's copy of the pin, if any. `fetch` with none of `GOGO_COMET`, the
/// store or NixOS in the way is `Ok(None)`: [`provision`] downloads.
pub fn resolve(
    host: &Host,
    mode: &CometMode,
    installed: Option<&Path>,
) -> Result<Option<CometBin>, MacAppError> {
    resolve_with(host, mode, installed, true)
}

/// [`resolve`]; `fix` makes a store copy executable. A listing passes
/// `false`: it never writes to the store.
fn resolve_with(
    host: &Host,
    mode: &CometMode,
    installed: Option<&Path>,
    fix: bool,
) -> Result<Option<CometBin>, MacAppError> {
    match mode {
        CometMode::Fetch => {
            if let Some(p) = host.var(COMET_ENV) {
                // The flake's build: it only has to be there.
                let p = PathBuf::from(p);
                if p.is_absolute() && is_executable(&p) {
                    return Ok(Some(CometBin {
                        path: p,
                        source: Source::Env,
                    }));
                }
                return Err(MacAppError::Comet(format!(
                    "{COMET_ENV} is {}, which is not an executable file: point it at a \
                     Comet (the flake's gogo-linux-app sets it), or unset it",
                    p.display()
                )));
            }
            if host.is_nixos() {
                return Err(nixos_error());
            }
            match installed {
                Some(p) => {
                    if fix {
                        make_executable(p)?;
                    }
                    Ok(Some(CometBin {
                        path: p.to_path_buf(),
                        source: Source::Store,
                    }))
                }
                None => Ok(None),
            }
        }
        CometMode::System => host
            .on_path("comet")
            .into_iter()
            .next()
            .map(|path| {
                Some(CometBin {
                    path,
                    source: Source::System,
                })
            })
            .ok_or_else(|| {
                MacAppError::Comet(format!(
                    "{KEY} = \"system\" needs comet on PATH: install one, or set {KEY} = \
                     \"fetch\" for gogo's pinned copy"
                ))
            }),
        CometMode::Path(p) => {
            if p.is_absolute() && is_executable(p) {
                Ok(Some(CometBin {
                    path: p.clone(),
                    source: Source::Path,
                }))
            } else {
                Err(MacAppError::Comet(format!(
                    "{KEY} = \"path:{}\" is not an absolute path to an executable file",
                    p.display()
                )))
            }
        }
    }
}

/// [`resolve`], downloading the pinned release into gogo's store when
/// `fetch` finds nothing. Call it only after the user agreed to the download
/// (`gogo runtimes install comet`, or a plan that showed [`plan_line`]).
pub async fn provision(
    host: &Host<'_>,
    mode: &CometMode,
    http: &reqwest::Client,
    api_base: &str,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<CometBin, MacAppError> {
    let entry = entry();
    let version = pin(entry).map(|p| p.version);
    let stored = store::installed(entry.id, version).map(|(m, root)| root.join(&m.entry_rel));
    if let Some(bin) = resolve(host, mode, stored.as_deref())? {
        return Ok(bin);
    }
    let m = store::install(entry, None, http, api_base, false, emit).await?;
    let path = store::tree_root(&m).join(&m.entry_rel);
    resolve(host, mode, Some(&path))?.ok_or_else(|| {
        MacAppError::Comet("the downloaded Comet is not where the store says".into())
    })
}

/// What `gogo runtimes install` does after the generic tool install: the
/// release asset is a bare file stored without the execute bit, so Comet's
/// payload is made executable here (`store::unpack` stays as it is for every
/// other entry).
pub fn after_install(entry_id: &str, payload: &Path) -> Result<(), MacAppError> {
    if entry_id == catalog::COMET {
        make_executable(payload)?;
    }
    Ok(())
}

/// The Comet already available without a download, for listings and
/// `doctor`: `None` when a `fetch` would still have to download. Read-only:
/// it never changes a file's mode.
pub fn available(host: &Host, mode: &CometMode) -> Result<Option<CometBin>, MacAppError> {
    let entry = entry();
    let version = pin(entry).map(|p| p.version);
    let stored = store::installed(entry.id, version).map(|(m, root)| root.join(&m.entry_rel));
    resolve_with(host, mode, stored.as_deref(), false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt as _;

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
        fn file(&self, d: &str, name: &str, mode: u32) -> PathBuf {
            std::fs::create_dir_all(self.dir(d)).unwrap();
            let p = self.dir(d).join(name);
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
            p
        }
        fn nixos(self) -> Self {
            self.file("etc", "NIXOS", 0o644);
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

    fn comet_err(r: Result<Option<CometBin>, MacAppError>) -> String {
        match r {
            Err(MacAppError::Comet(why)) => why,
            other => panic!("expected a comet error, got {other:?}"),
        }
    }

    /// The pin is the version design 23 decision 5's contract was read at.
    /// The contract (a Comet quirk): `--from-heroic` reads
    /// `$XDG_CONFIG_PATH/heroic/gog_store/auth.json`. Bumping the pin means
    /// re-reading that at the new tag, then bumping `CONTRACT_CHECKED_AGAINST`.
    #[test]
    fn the_pin_is_the_version_the_contract_was_checked_against() {
        let p = pin(entry()).unwrap();
        assert_eq!(
            p.version, CONTRACT_CHECKED_AGAINST,
            "Comet's pin moved: re-check design 23 decision 5 (the --from-heroic \
             credential file) against the new version, then bump CONTRACT_CHECKED_AGAINST"
        );
        assert_eq!(p.tag, format!("v{}", p.version));
    }

    /// The flake builds the pinned version from source: one number everywhere.
    #[test]
    fn the_flake_builds_the_pinned_version() {
        // nix/ is outside the Nix build's source (nix/app.nix), so there the
        // check is skipped; cargo nextest in the repo runs it.
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../nix/comet.nix");
        let Ok(nix) = std::fs::read_to_string(path) else {
            return;
        };
        let version = pin(entry()).unwrap().version;
        assert!(
            nix.lines()
                .any(|l| l.trim() == format!("version = \"{version}\";")),
            "nix/comet.nix must build Comet {version}"
        );
    }

    #[test]
    fn the_plan_line_names_size_licence_and_the_redist() {
        let line = plan_line(entry(), 14_660_192);
        assert!(line.contains("Comet 0.4.1"), "{line}");
        assert!(
            line.contains("14.0 MiB") || line.contains("14.1 MiB"),
            "{line}"
        );
        assert!(line.contains("Apache-2.0"), "{line}");
        assert!(
            line.contains("GalaxyPeer redist") && line.contains("100 MiB"),
            "{line}"
        );
    }

    #[test]
    fn gogo_comet_wins_on_nixos_and_elsewhere() {
        let f = Fake::new().nixos();
        let env = f.file("flake/bin", "comet", 0o755);
        let stored = f.file("store", "comet-x86_64-unknown-linux-gnu", 0o644);
        let f = f.set(COMET_ENV, env.as_os_str());
        let got = f
            .with(|h| resolve(h, &CometMode::Fetch, Some(&stored)))
            .unwrap();
        assert_eq!(
            got,
            Some(CometBin {
                path: env,
                source: Source::Env
            })
        );
        // Not there: named, not silently replaced.
        let f = f.set(COMET_ENV, "/no/such/comet");
        let why = comet_err(f.with(|h| resolve(h, &CometMode::Fetch, None)));
        assert!(
            why.contains(COMET_ENV) && why.contains("/no/such/comet"),
            "{why}"
        );
    }

    #[test]
    fn fetch_on_nixos_without_a_build_names_the_two_ways_out() {
        let f = Fake::new().nixos();
        let why = comet_err(f.with(|h| resolve(h, &CometMode::Fetch, None)));
        assert!(
            why.contains(FLAKE_PACKAGE) && why.contains("system"),
            "{why}"
        );
        // `system` and `path:` ignore the NixOS check.
        let bin = f.file("bin", "comet", 0o755);
        let f = f.path(&["bin"]);
        assert_eq!(
            f.with(|h| resolve(h, &CometMode::System, None))
                .unwrap()
                .unwrap()
                .path,
            bin
        );
    }

    /// `gogo runtimes install comet` stores the bare asset without an execute
    /// bit; resolving it for use makes it executable.
    #[test]
    fn the_stored_release_is_made_executable_when_resolved() {
        let f = Fake::new();
        let stored = f.file("store", "comet-x86_64-unknown-linux-gnu", 0o644);
        assert!(!is_executable(&stored));
        let got = f
            .with(|h| resolve(h, &CometMode::Fetch, Some(&stored)))
            .unwrap()
            .unwrap();
        assert_eq!(got.source, Source::Store);
        assert!(is_executable(&stored));
        assert_eq!(
            std::fs::metadata(&stored).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn mode_is_read_from_the_config_and_defaults_to_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        assert_eq!(mode_at(&cfg).unwrap(), CometMode::Fetch);
        std::fs::write(&cfg, "[linux]\ncomet = \"path:/opt/c\"\n").unwrap();
        assert_eq!(mode_at(&cfg).unwrap(), CometMode::Path("/opt/c".into()));
        std::fs::write(&cfg, "[linux]\ncomet = \"nope\"\n").unwrap();
        assert!(matches!(mode_at(&cfg), Err(MacAppError::Comet(_))));
    }

    /// `gogo runtimes install comet`: the generic install leaves 0644, and
    /// the install command's follow-up makes Comet executable, only Comet.
    #[test]
    fn the_install_path_makes_the_payload_executable() {
        let f = Fake::new();
        let comet = f.file("store", "comet-x86_64-unknown-linux-gnu", 0o644);
        after_install(catalog::COMET, &comet).unwrap();
        assert_eq!(
            std::fs::metadata(&comet).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let other = f.file("store", "umu-run", 0o644);
        after_install(catalog::UMU, &other).unwrap();
        assert_eq!(
            std::fs::metadata(&other).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    /// A listing is read-only: it sees the stored file and leaves its mode.
    #[test]
    fn a_listing_never_changes_the_stored_mode() {
        let f = Fake::new();
        let stored = f.file("store", "comet-x86_64-unknown-linux-gnu", 0o644);
        let got = f
            .with(|h| resolve_with(h, &CometMode::Fetch, Some(&stored), false))
            .unwrap()
            .unwrap();
        assert_eq!(got.source, Source::Store);
        assert_eq!(
            std::fs::metadata(&stored).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn fetch_with_nothing_stored_asks_for_a_download() {
        let f = Fake::new();
        assert_eq!(
            f.with(|h| resolve(h, &CometMode::Fetch, None)).unwrap(),
            None
        );
    }

    #[test]
    fn system_takes_the_first_executable_comet_on_path() {
        let f = Fake::new().path(&["a", "b", "c"]);
        f.file("a", "comet", 0o644);
        let b = f.file("b", "comet", 0o755);
        f.file("c", "comet", 0o755);
        assert_eq!(
            f.with(|h| resolve(h, &CometMode::System, None))
                .unwrap()
                .unwrap()
                .path,
            b
        );
        let why = comet_err(
            Fake::new()
                .path(&["none"])
                .with(|h| resolve(h, &CometMode::System, None)),
        );
        assert!(
            why.contains("comet on PATH") && why.contains("fetch"),
            "{why}"
        );
    }

    #[test]
    fn path_needs_an_absolute_executable_file() {
        let f = Fake::new();
        let good = f.file("opt", "comet", 0o755);
        let got = f
            .with(|h| resolve(h, &CometMode::Path(good.clone()), None))
            .unwrap();
        assert_eq!(got.unwrap().source, Source::Path);
        let plain = f.file("opt", "plain", 0o644);
        for bad in [plain, PathBuf::from("comet"), f.dir("missing")] {
            let why = comet_err(f.with(|h| resolve(h, &CometMode::Path(bad.clone()), None)));
            assert!(why.contains("executable file"), "{why}");
        }
    }
}
