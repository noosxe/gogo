//! GE-Proton, the Linux lane's runtime (design 18 §1, §2.2, #169).
//!
//! - **Managed builds** are catalog entries (`ge-proton/<GE-ProtonX-Y>`),
//!   installed by `store::install_to` into gogo's store or, when `[linux]
//!   proton_dir` says so, into a Steam's `compatibilitytools.d` (O1: a temp
//!   dir renamed into place, [`MARKER`] in the build's root). The appid
//!   check of R2 runs at install ([`check_appid`]).
//! - **Unmanaged builds** are any directory with a `toolmanifest.vdf`:
//!   `path:<dir>` refs, and what [`discover`] finds in the tools dirs and
//!   the Steam libraries. Not pinned, never pruned.
//! - [`check_build`] / [`check_managed`] are the launch check (§2.2), for the
//!   launcher (#177) and `doctor` (#178).
//!
//! What looks at the machine takes a [`Host`] (`HOME`, `XDG_DATA_HOME`), so
//! tests run on fake Steam roots and never see the user's.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::umu::Host;
use super::vdf;
use crate::config::ProtonDir;
use crate::engine::events::ProgressEvent;
use crate::macapp::catalog::{self, CatalogEntry};
use crate::macapp::error::MacAppError;
use crate::macapp::prompt::{PromptAnswer, PromptFn, PromptKind, PromptSpec};

/// The marker gogo writes into the root of each build it installs (O1).
pub const MARKER: &str = "gogo-managed.toml";

/// The config keys, as the user writes them.
pub const KEY: &str = "[linux] proton_dir";
pub const DEFAULT_KEY: &str = "[linux] default_runtime";

/// The file umu requires in a `PROTONPATH` (docs/21 §8 item 1).
pub const TOOLMANIFEST: &str = "toolmanifest.vdf";

/// Flatpak Steam's root, under `$HOME`.
const FLATPAK_STEAM: &str = ".var/app/com.valvesoftware.Steam/data/Steam";

/// What a GE-Proton takes on disk, for the question (docs/21 §1.2:
/// GE-Proton11-11, R5).
const UNPACKED: &str = "1.5 GB";

// — toolmanifest.vdf and the umu appid check (R2) —

/// A build's `require_tool_appid`, from its `toolmanifest.vdf`. `Ok(None)`:
/// the manifest names none.
pub fn require_tool_appid(dir: &Path) -> Result<Option<String>, String> {
    let path = dir.join(TOOLMANIFEST);
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v = vdf::parse(&raw).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(v.get("manifest")
        .and_then(|m| m.str("require_tool_appid"))
        .map(str::to_string))
}

/// The umu pin's version, for messages.
fn umu_version() -> &'static str {
    catalog::find(catalog::UMU)
        .and_then(|e| e.discovery.as_ref()?.pinned_version())
        .unwrap_or("?")
}

/// Does the pinned umu map `appid` to a container (design 18 §2.1)? The
/// error says so in R2's terms.
pub fn check_appid(appid: &str) -> Result<(), String> {
    if catalog::umu_runtimes().contains(&appid) {
        return Ok(());
    }
    Err(format!(
        "require_tool_appid {appid} is a Steam runtime umu {} doesn't know",
        umu_version()
    ))
}

// — the marker (O1) —

/// `gogo-managed.toml`: what gogo installed, to tell its builds from
/// others' and to check them before a launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub schema: u32,
    /// `ge-proton/GE-Proton11-12`.
    pub catalog_ref: String,
    /// The release archive's sha256 (the catalog pin).
    pub archive_sha256: String,
    /// The `proton` script's sha256.
    pub proton_sha256: String,
    /// The `version` file's contents, trimmed.
    pub version: String,
    /// RFC 3339.
    pub installed_at: String,
    pub gogo_version: String,
}

/// The build's marker, if it has a readable one.
pub fn read_marker(dir: &Path) -> Option<Marker> {
    toml::from_str(&std::fs::read_to_string(dir.join(MARKER)).ok()?).ok()
}

pub fn write_marker(dir: &Path, m: &Marker) -> Result<(), MacAppError> {
    let raw = toml::to_string(m).map_err(|e| MacAppError::Io(std::io::Error::other(e)))?;
    std::fs::write(
        dir.join(MARKER),
        format!(
            "# Installed by gogo (design 18 §2.2). gogo never deletes this build \
             from a shared tools dir; delete it yourself when nothing uses it.\n{raw}"
        ),
    )?;
    Ok(())
}

/// Is `dir` a build gogo installed? A build without the marker is someone
/// else's.
pub fn is_managed(dir: &Path) -> bool {
    dir.join(MARKER).is_file()
}

/// The `version` file, trimmed (`1793687165 GE-Proton11-12`).
pub fn version_of(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("version"))
        .ok()
        .map(|v| v.trim().to_string())
}

/// The marker for a build just unpacked at `dir`.
pub fn marker_for(
    dir: &Path,
    catalog_ref: &str,
    archive_sha256: &str,
) -> Result<Marker, MacAppError> {
    let invalid = |reason: String| MacAppError::RuntimeInvalid {
        path: dir.to_path_buf(),
        reason,
    };
    let version = version_of(dir).ok_or_else(|| invalid("no version file".into()))?;
    Ok(Marker {
        schema: 1,
        catalog_ref: catalog_ref.to_string(),
        archive_sha256: archive_sha256.to_string(),
        proton_sha256: sha256_of(&dir.join("proton"))
            .map_err(|e| invalid(format!("proton: {e}")))?,
        version,
        installed_at: chrono::Utc::now().to_rfc3339(),
        gogo_version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

fn sha256_of(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest as _, Sha256};
    use std::io::Read as _;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

// — the launch check (§2.2) —

/// What a build is expected to be: `PROTON_VERSION` and `PROTON_SHA` in
/// `launch.conf`, or a marker's.
#[derive(Debug, Clone, Copy)]
pub struct Expect<'a> {
    pub version: &'a str,
    pub proton_sha256: &'a str,
}

/// Why a build fails the launch check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildProblem {
    /// `proton`, `toolmanifest.vdf` or `version` isn't there (or the whole dir).
    Missing(PathBuf),
    VersionChanged {
        expected: String,
        found: String,
    },
    ProtonChanged {
        expected: String,
        found: String,
    },
    /// [`check_managed`] on a build without gogo's marker.
    NotManaged(PathBuf),
}

impl std::fmt::Display for BuildProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildProblem::Missing(p) => write!(f, "{} is missing", p.display()),
            BuildProblem::VersionChanged { expected, found } => {
                write!(f, "version is '{found}', expected '{expected}'")
            }
            BuildProblem::ProtonChanged { expected, found } => write!(
                f,
                "the proton script changed (sha256 {found}, expected {expected})"
            ),
            BuildProblem::NotManaged(p) => {
                write!(
                    f,
                    "{} has no {MARKER}: not a build gogo installed",
                    p.display()
                )
            }
        }
    }
}

/// The launch check (design 18 §2.2): `proton` and `toolmanifest.vdf`
/// exist, `version` matches and `proton`'s sha256 matches.
pub fn check_build(dir: &Path, expect: Expect) -> Result<(), BuildProblem> {
    for f in ["proton", TOOLMANIFEST, "version"] {
        if !dir.join(f).is_file() {
            return Err(BuildProblem::Missing(dir.join(f)));
        }
    }
    let found = version_of(dir).unwrap_or_default();
    if found != expect.version.trim() {
        return Err(BuildProblem::VersionChanged {
            expected: expect.version.trim().to_string(),
            found,
        });
    }
    let proton = dir.join("proton");
    let found = sha256_of(&proton).map_err(|_| BuildProblem::Missing(proton))?;
    if !found.eq_ignore_ascii_case(expect.proton_sha256) {
        return Err(BuildProblem::ProtonChanged {
            expected: expect.proton_sha256.to_string(),
            found,
        });
    }
    Ok(())
}

/// [`check_build`] against the build's own marker; the marker on success.
pub fn check_managed(dir: &Path) -> Result<Marker, BuildProblem> {
    let m = read_marker(dir).ok_or_else(|| BuildProblem::NotManaged(dir.to_path_buf()))?;
    check_build(
        dir,
        Expect {
            version: &m.version,
            proton_sha256: &m.proton_sha256,
        },
    )?;
    Ok(m)
}

// — Steam roots and tools dirs (§2.2) —

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteamKind {
    Native,
    Flatpak,
}

impl SteamKind {
    pub fn label(self) -> &'static str {
        match self {
            SteamKind::Native => "Steam",
            SteamKind::Flatpak => "Flatpak Steam",
        }
    }

    /// The `[linux] proton_dir` value that picks it.
    pub fn proton_dir(self) -> ProtonDir {
        match self {
            SteamKind::Native => ProtonDir::Steam,
            SteamKind::Flatpak => ProtonDir::SteamFlatpak,
        }
    }
}

/// A Steam install's root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamRoot {
    pub kind: SteamKind,
    pub path: PathBuf,
}

impl SteamRoot {
    /// `<steam>/compatibilitytools.d`, the shared tools dir.
    pub fn tools_dir(&self) -> PathBuf {
        self.path.join("compatibilitytools.d")
    }
}

fn abs_var(host: &Host, key: &str) -> Option<PathBuf> {
    host.var(key).map(PathBuf::from).filter(|p| p.is_absolute())
}

/// The Steam installs here, native first: `$XDG_DATA_HOME/Steam` (or
/// `~/.steam/root`, which usually points there; one entry when it does),
/// then Flatpak's `~/.var/app/com.valvesoftware.Steam/data/Steam`.
pub fn steam_roots(host: &Host) -> Vec<SteamRoot> {
    let Some(home) = abs_var(host, "HOME") else {
        return Vec::new();
    };
    let data = abs_var(host, "XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share"));
    let candidates = [
        (SteamKind::Native, data.join("Steam")),
        (SteamKind::Native, home.join(".steam/root")),
        (SteamKind::Flatpak, home.join(FLATPAK_STEAM)),
    ];
    let mut out: Vec<SteamRoot> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for (kind, path) in candidates {
        if !path.is_dir() {
            continue;
        }
        let real = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if seen.contains(&real) || out.iter().any(|r| r.kind == kind) {
            continue;
        }
        seen.push(real);
        out.push(SteamRoot { kind, path });
    }
    out
}

/// A Steam's libraries: the root itself, then each `path` in
/// `steamapps/libraryfolders.vdf`. An unreadable file lists the root only.
pub fn library_folders(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    let file = root.join("steamapps/libraryfolders.vdf");
    let parsed = std::fs::read_to_string(&file)
        .ok()
        .and_then(|raw| vdf::parse(&raw).ok());
    if let Some(v) = parsed
        && let Some(folders) = v.get("libraryfolders")
    {
        for (_, f) in folders.pairs() {
            if let Some(p) = f.str("path").map(PathBuf::from)
                && p.is_absolute()
                && !out.contains(&p)
            {
                out.push(p);
            }
        }
    }
    out
}

// — unmanaged builds (§1) —

/// A Proton build gogo didn't install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmanaged {
    pub path: PathBuf,
    /// Where it was found: `compatibilitytools.d`, a Steam library, or the
    /// config.
    pub source: String,
    pub version: Option<String>,
    pub appid: Option<String>,
    /// `toolmanifest.vdf` is there: umu can run it.
    pub runnable: bool,
}

impl Unmanaged {
    /// The warnings `runtimes list` shows beside the "unmanaged" note.
    pub fn problems(&self) -> Vec<String> {
        if !self.runnable {
            return vec![format!("no {TOOLMANIFEST}: umu can't run it")];
        }
        match &self.appid {
            None => vec![format!("{TOOLMANIFEST} names no require_tool_appid")],
            Some(a) => check_appid(a).err().into_iter().collect(),
        }
    }
}

/// Look at a directory as a Proton build.
pub fn inspect(dir: &Path, source: &str) -> Unmanaged {
    Unmanaged {
        path: dir.to_path_buf(),
        source: source.to_string(),
        version: version_of(dir),
        appid: require_tool_appid(dir).ok().flatten(),
        runnable: dir.join(TOOLMANIFEST).is_file(),
    }
}

/// The builds in a dir: its subdirs with a `toolmanifest.vdf` (and, when
/// `prefix` is set, a name starting with it), sorted, gogo's and hidden
/// (gogo's temp dirs) skipped.
fn builds_in(dir: &Path, prefix: Option<&str>, source: &str) -> Vec<Unmanaged> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            !name.starts_with('.')
                && prefix.is_none_or(|pre| name.starts_with(pre))
                && p.join(TOOLMANIFEST).is_file()
                && !is_managed(p)
        })
        .collect();
    found.sort();
    found.iter().map(|p| inspect(p, source)).collect()
}

/// Every unmanaged build (design 18 §1): each Steam's
/// `compatibilitytools.d`, then `steamapps/common/Proton*` in each of its
/// libraries. One entry per build, wherever it's seen from.
pub fn discover(host: &Host) -> Vec<Unmanaged> {
    let mut out: Vec<Unmanaged> = Vec::new();
    for root in steam_roots(host) {
        let mut found = builds_in(
            &root.tools_dir(),
            None,
            &format!("{} compatibilitytools.d", root.kind.label()),
        );
        for lib in library_folders(&root.path) {
            found.extend(builds_in(
                &lib.join("steamapps/common"),
                Some("Proton"),
                &format!("{} library", root.kind.label()),
            ));
        }
        for b in found {
            let real = std::fs::canonicalize(&b.path).unwrap_or_else(|_| b.path.clone());
            if !out
                .iter()
                .any(|o| std::fs::canonicalize(&o.path).unwrap_or_else(|_| o.path.clone()) == real)
            {
                out.push(b);
            }
        }
    }
    out
}

// — refs (`ge-proton[/<ver>]`, `path:<dir>`) —

/// A Proton runtime reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtonRef {
    Catalog {
        id: String,
        version: Option<String>,
    },
    /// An unmanaged build (design 16 §1.2's `path:` ref).
    Path(PathBuf),
}

impl ProtonRef {
    pub fn parse(s: &str) -> Result<ProtonRef, String> {
        let s = s.trim();
        if let Some(dir) = s.strip_prefix("path:") {
            let dir = PathBuf::from(dir);
            if !dir.is_absolute() {
                return Err(format!("'{s}': a path: runtime needs an absolute dir"));
            }
            return Ok(ProtonRef::Path(dir));
        }
        let (id, version) = s.split_once('/').map_or((s, None), |(i, v)| (i, Some(v)));
        match catalog::find(id) {
            Some(e) if matches!(e.kind, catalog::Kind::Proton { .. }) => Ok(ProtonRef::Catalog {
                id: id.to_string(),
                version: version.filter(|v| !v.is_empty()).map(str::to_string),
            }),
            _ => Err(format!(
                "'{s}' is not a Proton runtime: use {}[/<version>] or path:<dir>",
                catalog::GE_PROTON
            )),
        }
    }
}

/// The default Proton runtime: `[linux] default_runtime`, else the
/// catalog's GE-Proton.
pub fn default_ref(default_runtime: Option<&str>) -> Result<ProtonRef, String> {
    match default_runtime.filter(|s| !s.trim().is_empty()) {
        Some(s) => ProtonRef::parse(s).map_err(|e| format!("{DEFAULT_KEY}: {e}")),
        None => Ok(ProtonRef::Catalog {
            id: catalog::GE_PROTON.to_string(),
            version: None,
        }),
    }
}

// — `[linux] proton_dir`: asked once (§2.2) —

pub fn stored_at(config: &Path) -> Option<ProtonDir> {
    ProtonDir::parse(&super::stored_key(config, "proton_dir")?)
}

/// The question: the store (the default) or each Steam's tools dir.
pub fn prompt_spec(
    entry: &CatalogEntry,
    bytes: Option<u64>,
    roots: &[SteamRoot],
    store: &Path,
    config: &Path,
) -> PromptSpec {
    let version = entry
        .discovery
        .as_ref()
        .and_then(|d| d.pinned_version())
        .unwrap_or("?");
    let size = bytes.map_or_else(String::new, |b| {
        format!("{} download, ", crate::fmt::human_size(b))
    });
    let mut items = vec![format!(
        "store: gogo's store, {} (only gogo uses it)",
        store.display()
    )];
    for r in roots {
        items.push(format!(
            "{}: {}'s {} (Steam, Lutris and Heroic see it too)",
            r.kind.proton_dir().as_str(),
            r.kind.label(),
            r.tools_dir().display()
        ));
    }
    PromptSpec {
        kind: PromptKind::Select,
        title: format!(
            "{} {version} ({size}about {UNPACKED} unpacked) can live in gogo's store or \
             in a Steam's compatibilitytools.d. gogo never deletes builds there: \
             `prune` lists its old ones for you to delete.\nWhere should new GE-Proton \
             builds go? (asked once; saved as {KEY} in {})",
            entry.id,
            config.display()
        ),
        items,
        allow_other: false,
        allow_none: false,
        default: 0,
    }
}

/// Where a new GE-Proton build goes: `None` for gogo's store, else the
/// tools dir (created if its Steam is there but the dir isn't).
///
/// - `[linux] proton_dir` set: that (a Steam that's gone → the store, and
///   says so).
/// - Unset, no Steam: the store, nothing asked.
/// - Unset, a terminal: asked once ([`prompt_spec`]), the answer stored.
/// - Unset, headless: the store, and says so; nothing stored.
pub fn placement(
    entry: &CatalogEntry,
    bytes: Option<u64>,
    config: &Path,
    prompt: Option<&PromptFn>,
    host: &Host,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<Option<PathBuf>, MacAppError> {
    let roots = steam_roots(host);
    let choice = match stored_at(config) {
        Some(d) => d,
        None if roots.is_empty() => return Ok(None),
        None => {
            let Some(prompt) = prompt else {
                emit(&ProgressEvent::Warn {
                    message: format!(
                        "{KEY} is unset and there's no terminal to ask on: installing \
                         {} into gogo's store (`gogo config set linux.proton_dir steam` \
                         shares new builds with Steam)",
                        entry.id
                    ),
                });
                return Ok(None);
            };
            let store = crate::macapp::store::runtimes_dir().join(entry.id);
            let spec = prompt_spec(entry, bytes, &roots, &store, config);
            let choice = match prompt(&spec) {
                Some(PromptAnswer::Index(0)) => ProtonDir::Store,
                Some(PromptAnswer::Index(i)) if i <= roots.len() => roots[i - 1].kind.proton_dir(),
                _ => return Err(MacAppError::Proton(format!("{KEY}: no answer, cancelled"))),
            };
            if let Err(e) = super::store_key(config, "proton_dir", choice.as_str()) {
                emit(&ProgressEvent::Warn {
                    message: format!("could not save {KEY} in {}: {e}", config.display()),
                });
            }
            choice
        }
    };
    let root = match choice {
        ProtonDir::Store => return Ok(None),
        // `steam`: the native one, else the only one there is.
        ProtonDir::Steam => roots
            .iter()
            .find(|r| r.kind == SteamKind::Native)
            .or(roots.first()),
        ProtonDir::SteamFlatpak => roots.iter().find(|r| r.kind == SteamKind::Flatpak),
    };
    let Some(root) = root else {
        emit(&ProgressEvent::Warn {
            message: format!(
                "{KEY} = \"{}\" but no such Steam is installed: installing {} into \
                 gogo's store",
                choice.as_str(),
                entry.id
            ),
        });
        return Ok(None);
    };
    let tools = root.tools_dir();
    std::fs::create_dir_all(&tools)?;
    Ok(Some(tools))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::OsString;

    /// A synthesized Proton build: `proton`, `version`, `toolmanifest.vdf`.
    pub(crate) fn fake_build(dir: &Path, version: &str, appid: Option<&str>) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("proton"),
            "#!/usr/bin/env python3\n# fake proton\n",
        )
        .unwrap();
        std::fs::write(dir.join("version"), format!("1793687165 {version}\n")).unwrap();
        let req = appid.map_or(String::new(), |a| {
            format!("  \"require_tool_appid\" \"{a}\"\n")
        });
        std::fs::write(
            dir.join(TOOLMANIFEST),
            format!(
                "\"manifest\"\n{{\n  \"version\" \"2\"\n  \"commandline\" \"/proton %verb%\"\n\
                 {req}  \"use_sessions\" \"1\"\n}}\n"
            ),
        )
        .unwrap();
    }

    /// A fake home with `HOME` / `XDG_DATA_HOME` pointing into it.
    pub(crate) struct Home {
        pub dir: tempfile::TempDir,
        env: HashMap<&'static str, OsString>,
    }

    impl Home {
        pub fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let mut env = HashMap::new();
            env.insert("HOME", dir.path().as_os_str().to_owned());
            Home { dir, env }
        }
        pub fn p(&self, rel: &str) -> PathBuf {
            self.dir.path().join(rel)
        }
        pub fn native(self) -> Self {
            std::fs::create_dir_all(self.p(".local/share/Steam/steamapps")).unwrap();
            self
        }
        pub fn flatpak(self) -> Self {
            std::fs::create_dir_all(self.p(FLATPAK_STEAM)).unwrap();
            self
        }
        pub fn with<R>(&self, f: impl FnOnce(&Host) -> R) -> R {
            let env = |k: &str| self.env.get(k).cloned();
            f(&Host {
                root: self.dir.path(),
                env: &env,
            })
        }
    }

    fn ge() -> &'static CatalogEntry {
        catalog::find(catalog::GE_PROTON).unwrap()
    }

    #[test]
    fn appid_is_read_and_checked_against_the_umu_pin() {
        let d = tempfile::tempdir().unwrap();
        fake_build(d.path(), "GE-Proton11-12", Some("4183110"));
        assert_eq!(
            require_tool_appid(d.path()).unwrap().as_deref(),
            Some("4183110")
        );
        assert!(check_appid("4183110").is_ok());
        let why = check_appid("9999999").unwrap_err();
        assert!(
            why.contains("9999999") && why.contains("umu 1.4.4"),
            "{why}"
        );
        fake_build(d.path(), "GE-Proton11-12", None);
        assert_eq!(require_tool_appid(d.path()).unwrap(), None);
        std::fs::remove_file(d.path().join(TOOLMANIFEST)).unwrap();
        assert!(require_tool_appid(d.path()).is_err());
    }

    #[test]
    fn the_launch_check_catches_each_change() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        fake_build(dir, "GE-Proton11-12", Some("4183110"));
        let m = marker_for(dir, "ge-proton/GE-Proton11-12", "abc").unwrap();
        assert_eq!(m.version, "1793687165 GE-Proton11-12");
        assert_eq!(m.proton_sha256.len(), 64);
        assert_eq!(
            check_managed(dir),
            Err(BuildProblem::NotManaged(dir.into()))
        );
        write_marker(dir, &m).unwrap();
        assert!(is_managed(dir));
        assert_eq!(read_marker(dir).as_ref(), Some(&m));
        assert_eq!(check_managed(dir).unwrap(), m);
        let expect = Expect {
            version: &m.version,
            proton_sha256: &m.proton_sha256,
        };
        assert!(check_build(dir, expect).is_ok());
        // `version` changed.
        std::fs::write(dir.join("version"), "1 GE-Proton11-13\n").unwrap();
        assert!(matches!(
            check_build(dir, expect),
            Err(BuildProblem::VersionChanged { found, .. }) if found == "1 GE-Proton11-13"
        ));
        std::fs::write(dir.join("version"), format!("{}\n", m.version)).unwrap();
        // `proton` changed.
        std::fs::write(dir.join("proton"), "#!/bin/sh\n").unwrap();
        let err = check_managed(dir).unwrap_err();
        assert!(matches!(err, BuildProblem::ProtonChanged { .. }));
        assert!(err.to_string().contains("proton script changed"));
        // Gone.
        std::fs::remove_file(dir.join(TOOLMANIFEST)).unwrap();
        assert_eq!(
            check_build(dir, expect),
            Err(BuildProblem::Missing(dir.join(TOOLMANIFEST)))
        );
        assert!(matches!(
            check_build(&dir.join("nope"), expect),
            Err(BuildProblem::Missing(_))
        ));
    }

    #[test]
    fn steam_roots_native_flatpak_both_neither() {
        assert!(Home::new().with(steam_roots).is_empty());
        let h = Home::new().native();
        let roots = h.with(steam_roots);
        assert_eq!(
            roots,
            [SteamRoot {
                kind: SteamKind::Native,
                path: h.p(".local/share/Steam")
            }]
        );
        assert_eq!(
            roots[0].tools_dir(),
            h.p(".local/share/Steam/compatibilitytools.d")
        );
        let h = Home::new().flatpak();
        assert_eq!(
            h.with(steam_roots),
            [SteamRoot {
                kind: SteamKind::Flatpak,
                path: h.p(FLATPAK_STEAM)
            }]
        );
        let h = Home::new().native().flatpak();
        let kinds: Vec<SteamKind> = h.with(steam_roots).iter().map(|r| r.kind).collect();
        assert_eq!(kinds, [SteamKind::Native, SteamKind::Flatpak]);
    }

    #[test]
    fn steam_root_symlink_and_xdg_data_home() {
        // ~/.steam/root -> the data dir's Steam: one root.
        let mut h = Home::new().native();
        std::fs::create_dir_all(h.p(".steam")).unwrap();
        std::os::unix::fs::symlink(h.p(".local/share/Steam"), h.p(".steam/root")).unwrap();
        assert_eq!(h.with(steam_roots).len(), 1);
        // XDG_DATA_HOME moves the data dir; ~/.steam/root still finds it.
        std::fs::create_dir_all(h.p("xdg")).unwrap();
        h.env.insert("XDG_DATA_HOME", h.p("xdg").into_os_string());
        let roots = h.with(steam_roots);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].path, h.p(".steam/root"));
        std::fs::create_dir_all(h.p("xdg/Steam")).unwrap();
        assert_eq!(h.with(steam_roots)[0].path, h.p("xdg/Steam"));
    }

    #[test]
    fn discovery_reads_tools_dirs_and_steam_libraries() {
        let h = Home::new().native().flatpak();
        let steam = h.p(".local/share/Steam");
        let lib = h.p("games/SteamLibrary");
        std::fs::write(
            steam.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n\
                 \t\"1\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t\t\"apps\" {{ \"1\" \"2\" }}\n\t}}\n}}\n",
                steam.display(),
                lib.display()
            ),
        )
        .unwrap();
        // Someone else's GE-Proton, gogo's own, a half-extracted temp dir,
        // a dir that isn't a build.
        fake_build(
            &steam.join("compatibilitytools.d/GE-Proton10-3"),
            "GE-Proton10-3",
            Some("1628350"),
        );
        let mine = steam.join("compatibilitytools.d/GE-Proton11-12");
        fake_build(&mine, "GE-Proton11-12", Some("4183110"));
        write_marker(
            &mine,
            &marker_for(&mine, "ge-proton/GE-Proton11-12", "x").unwrap(),
        )
        .unwrap();
        fake_build(
            &steam.join("compatibilitytools.d/.gogo-tmp-x/GE"),
            "x",
            None,
        );
        std::fs::create_dir_all(steam.join("compatibilitytools.d/junk")).unwrap();
        // Valve's Proton in the second library, needing an appid umu lacks;
        // a non-Proton app beside it.
        fake_build(
            &lib.join("steamapps/common/Proton 9.0 (Beta)"),
            "proton-9.0-1",
            Some("1234567"),
        );
        fake_build(
            &lib.join("steamapps/common/SteamLinuxRuntime_sniper"),
            "x",
            Some("1"),
        );
        fake_build(
            &h.p(FLATPAK_STEAM).join("compatibilitytools.d/Proton-tkg"),
            "tkg",
            Some("1391110"),
        );
        let found = h.with(discover);
        let names: Vec<(String, &str)> = found
            .iter()
            .map(|b| {
                (
                    b.path.file_name().unwrap().to_string_lossy().into_owned(),
                    b.source.as_str(),
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                ("GE-Proton10-3".to_string(), "Steam compatibilitytools.d"),
                ("Proton 9.0 (Beta)".to_string(), "Steam library"),
                (
                    "Proton-tkg".to_string(),
                    "Flatpak Steam compatibilitytools.d"
                ),
            ]
        );
        assert_eq!(
            found[0].version.as_deref(),
            Some("1793687165 GE-Proton10-3")
        );
        assert!(found[0].problems().is_empty());
        let p = found[1].problems();
        assert!(p.len() == 1 && p[0].contains("1234567"), "{p:?}");
        assert!(inspect(&h.p("nowhere"), "x").problems()[0].contains("umu can't run it"));
        // A library listed twice, or the root again, is read once.
        assert_eq!(library_folders(&steam), [steam.clone(), lib]);
        assert_eq!(library_folders(&h.p("none")), [h.p("none")]);
    }

    #[test]
    fn refs_parse() {
        assert_eq!(
            ProtonRef::parse("path:/opt/proton").unwrap(),
            ProtonRef::Path("/opt/proton".into())
        );
        assert!(ProtonRef::parse("path:rel").is_err());
        assert_eq!(
            ProtonRef::parse("ge-proton/GE-Proton11-12").unwrap(),
            ProtonRef::Catalog {
                id: "ge-proton".into(),
                version: Some("GE-Proton11-12".into())
            }
        );
        assert!(ProtonRef::parse("winecx26").is_err());
        assert_eq!(
            default_ref(None).unwrap(),
            ProtonRef::Catalog {
                id: "ge-proton".into(),
                version: None
            }
        );
        assert_eq!(
            default_ref(Some("path:/p")).unwrap(),
            ProtonRef::Path("/p".into())
        );
        assert!(default_ref(Some("nope")).unwrap_err().contains(DEFAULT_KEY));
    }

    fn answer(i: usize) -> impl Fn(&PromptSpec) -> Option<PromptAnswer> + Send + Sync {
        move |_| Some(PromptAnswer::Index(i))
    }

    fn place(h: &Home, cfg: &Path, prompt: Option<&PromptFn>) -> (Option<PathBuf>, Vec<String>) {
        let mut notes = Vec::new();
        let out = h
            .with(|host| {
                placement(ge(), Some(566_859_228), cfg, prompt, host, &mut |ev| {
                    if let ProgressEvent::Warn { message } = ev {
                        notes.push(message.clone());
                    }
                })
            })
            .unwrap();
        (out, notes)
    }

    #[test]
    fn no_steam_means_the_store_and_no_question() {
        let h = Home::new();
        let cfg = h.p("config.toml");
        let never = |_: &PromptSpec| -> Option<PromptAnswer> { panic!("asked") };
        assert_eq!(place(&h, &cfg, Some(&never)), (None, vec![]));
        assert_eq!(stored_at(&cfg), None);
    }

    #[test]
    fn headless_unset_uses_the_store_and_says_so() {
        let h = Home::new().native();
        let cfg = h.p("config.toml");
        let (out, notes) = place(&h, &cfg, None);
        assert_eq!(out, None);
        assert!(
            notes[0].contains("[linux] proton_dir is unset"),
            "{notes:?}"
        );
        assert!(notes[0].contains("gogo's store"));
        assert_eq!(stored_at(&cfg), None, "nothing stored headless");
    }

    #[test]
    fn asked_once_with_both_steams_and_the_answer_kept() {
        let h = Home::new().native().flatpak();
        let cfg = h.p("config.toml");
        std::fs::write(&cfg, "# mine\n[gog]\nlanguage = \"de\"\n").unwrap();
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = asked.clone();
        let p = move |spec: &PromptSpec| {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(spec.items.len(), 3, "{:?}", spec.items);
            assert!(spec.items[0].starts_with("store: "));
            assert!(spec.items[1].starts_with("steam: Steam's "));
            assert!(spec.items[2].starts_with("steam-flatpak: Flatpak Steam's "));
            // R5: the size is in the question.
            assert!(spec.title.contains("540.6 MiB download"), "{}", spec.title);
            assert!(spec.title.contains("1.5 GB unpacked"));
            Some(PromptAnswer::Index(2))
        };
        let flatpak_tools = h.p(FLATPAK_STEAM).join("compatibilitytools.d");
        for _ in 0..2 {
            let (out, notes) = place(&h, &cfg, Some(&p));
            assert_eq!(out.as_deref(), Some(flatpak_tools.as_path()));
            assert!(notes.is_empty());
        }
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(flatpak_tools.is_dir(), "created");
        assert_eq!(stored_at(&cfg), Some(ProtonDir::SteamFlatpak));
        let raw = std::fs::read_to_string(&cfg).unwrap();
        assert!(raw.contains("# mine") && raw.contains("language = \"de\""));
    }

    #[test]
    fn one_steam_offers_two_items_and_store_is_stored_too() {
        let h = Home::new().flatpak();
        let cfg = h.p("config.toml");
        let p = |spec: &PromptSpec| {
            assert_eq!(spec.items.len(), 2);
            assert_eq!(spec.default, 0);
            Some(PromptAnswer::Index(0))
        };
        assert_eq!(place(&h, &cfg, Some(&p)).0, None);
        assert_eq!(stored_at(&cfg), Some(ProtonDir::Store));
        // Cancelled: an error, nothing stored.
        let h = Home::new().native();
        let cfg = h.p("config.toml");
        let cancel = |_: &PromptSpec| None;
        let err = h
            .with(|host| placement(ge(), None, &cfg, Some(&cancel), host, &mut |_| {}))
            .unwrap_err();
        assert!(err.to_string().contains("cancelled"));
        assert_eq!(stored_at(&cfg), None);
    }

    #[test]
    fn a_stored_steam_that_is_gone_falls_back_to_the_store() {
        let h = Home::new().native();
        let cfg = h.p("config.toml");
        std::fs::write(&cfg, "[linux]\nproton_dir = \"steam-flatpak\"\n").unwrap();
        let (out, notes) = place(&h, &cfg, Some(&answer(1)));
        assert_eq!(out, None);
        assert!(notes[0].contains("no such Steam"), "{notes:?}");
        // `steam` with only Flatpak's: that one.
        let h = Home::new().flatpak();
        let cfg = h.p("config.toml");
        std::fs::write(&cfg, "[linux]\nproton_dir = \"steam\"\n").unwrap();
        assert_eq!(
            place(&h, &cfg, None).0,
            Some(h.p(FLATPAK_STEAM).join("compatibilitytools.d"))
        );
    }
}

/// `store::install_to` and `store::prune` on synthesized GE-Proton
/// archives served by wiremock (design 18 §11.1 "Store").
#[cfg(test)]
mod install_tests {
    use super::*;
    use crate::macapp::store;
    use sha2::{Digest as _, Sha256, Sha512};

    const VER: &str = "GE-Proton11-12";

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// `GE-Proton11-12-x86_64/{proton,version,toolmanifest.vdf}` as a
    /// `.tar.gz`, needing `appid`.
    fn archive(appid: &str) -> Vec<u8> {
        let src = tempfile::tempdir().unwrap();
        let top = src.path().join(format!("{VER}-x86_64"));
        tests::fake_build(&top, VER, Some(appid));
        std::fs::create_dir_all(top.join("files/bin")).unwrap();
        std::fs::write(top.join("files/bin/wine"), "x").unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        {
            let mut t = tar::Builder::new(&mut gz);
            t.append_dir_all(format!("{VER}-x86_64"), &top).unwrap();
            t.finish().unwrap();
        }
        gz.finish().unwrap()
    }

    struct Mock {
        server: wiremock::MockServer,
        entry: &'static CatalogEntry,
    }

    /// wiremock as GitHub for a ge-proton entry pinned to `body` (sha256 as
    /// GitHub's digest, sha512 as given or the real one); the download
    /// expects exactly `hits` requests.
    async fn mock(body: &[u8], appid: &'static str, sha512: Option<&str>, hits: u64) -> Mock {
        use wiremock::matchers::{method, path};
        let server = wiremock::MockServer::start().await;
        let sha = hex(&Sha256::digest(body));
        let asset = "GE-Proton11-12-x86_64.tar.gz";
        wiremock::Mock::given(method("GET"))
            .and(path(format!(
                "/repos/GloriousEggroll/proton-ge-custom/releases/tags/{VER}"
            )))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "tag_name": VER, "prerelease": false,
                    "published_at": "2026-11-03T07:19:38Z",
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
        let real = catalog::find(catalog::GE_PROTON).unwrap();
        let sha512 = sha512.map_or_else(|| hex(&Sha512::digest(body)), str::to_string);
        let entry = Box::leak(Box::new(CatalogEntry {
            id: real.id,
            os: real.os,
            kind: catalog::Kind::Proton {
                appid,
                sha512: Box::leak(sha512.into_boxed_str()),
            },
            status: catalog::Status::Available,
            discovery: Some(catalog::Discovery::Pinned(catalog::PinnedAsset {
                repo: "GloriousEggroll/proton-ge-custom",
                tag: VER,
                asset,
                version: VER,
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
        Mock { server, entry }
    }

    fn env_home(dir: &Path) -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::test_util::env_lock();
        // SAFETY: env mutation under the shared env mutex.
        unsafe { std::env::set_var("GOGO_HOME", dir) };
        guard
    }

    async fn install(
        m: &Mock,
        shared: Option<&Path>,
    ) -> (Result<store::ManifestEntry, MacAppError>, Vec<String>) {
        let http = catalog::http_client().unwrap();
        let mut notes = Vec::new();
        let r = store::install_to(
            m.entry,
            None,
            &http,
            &m.server.uri(),
            false,
            shared,
            &mut |ev| {
                if let ProgressEvent::Warn { message } = ev {
                    notes.push(message.clone());
                }
            },
        )
        .await;
        (r, notes)
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    // The env guard spans the awaits: GOGO_HOME is read throughout. The
    // test runtime is single-threaded and no other task touches env.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn store_install_keeps_the_top_dir_under_the_arch_level() {
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        let body = archive("4183110");
        let m = mock(&body, "4183110", None, 1).await;
        let (r, notes) = install(&m, None).await;
        let man = r.unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(man.path, None);
        assert_eq!(man.entry_rel, format!("{VER}-x86_64/proton"));
        let dir = store::proton_dir(&man);
        assert_eq!(
            dir,
            home.path()
                .join(format!("runtimes/ge-proton/{VER}/x86_64/{VER}-x86_64"))
        );
        let marker = check_managed(&dir).unwrap();
        assert_eq!(marker.archive_sha256, man.sha256);
        assert!(man.bytes > 0);
        assert_eq!(store::installed("ge-proton", None).unwrap().0, man);
        // Cached: no second download (`expect(1)`).
        assert_eq!(install(&m, None).await.0.unwrap(), man);
        m.server.verify().await;
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn shared_install_renames_into_place_with_the_marker() {
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        let tools = home.path().join("Steam/compatibilitytools.d");
        std::fs::create_dir_all(&tools).unwrap();
        let body = archive("4183110");
        let m = mock(&body, "4183110", None, 1).await;
        let (r, notes) = install(&m, Some(&tools)).await;
        let man = r.unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        let named = tools.join(VER);
        // The name umu and Steam show; no temp dir left beside it.
        assert_eq!(names(&tools), [VER]);
        assert_eq!(man.path.as_deref(), Some(named.as_path()));
        assert!(man.path.as_ref().unwrap().is_absolute());
        assert_eq!(man.entry_rel, "proton");
        assert_eq!(store::proton_dir(&man), named);
        // The marker: ref, both shas, version, time, gogo's version.
        let mk = read_marker(&named).unwrap();
        assert_eq!(mk.schema, 1);
        assert_eq!(mk.catalog_ref, format!("ge-proton/{VER}"));
        assert_eq!(mk.archive_sha256, hex(&Sha256::digest(&body)));
        assert_eq!(
            mk.proton_sha256,
            hex(&Sha256::digest(
                std::fs::read(named.join("proton")).unwrap()
            ))
        );
        assert_eq!(mk.version, format!("1793687165 {VER}"));
        assert!(chrono::DateTime::parse_from_rfc3339(&mk.installed_at).is_ok());
        assert_eq!(mk.gogo_version, env!("CARGO_PKG_VERSION"));
        let raw = std::fs::read_to_string(named.join(MARKER)).unwrap();
        assert!(raw.starts_with("# Installed by gogo"), "{raw}");
        // The store's manifest records it; nothing went into the store tree.
        assert_eq!(store::load_manifest(), std::slice::from_ref(&man));
        assert!(!home.path().join("runtimes/ge-proton").exists());
        assert_eq!(store::installed("ge-proton", Some(VER)).unwrap().0, man);
        assert_eq!(install(&m, Some(&tools)).await.0.unwrap(), man);
        m.server.verify().await;
        // Another tool replaced it under the same name: not gogo's anymore.
        std::fs::remove_file(named.join(MARKER)).unwrap();
        assert!(store::installed("ge-proton", Some(VER)).is_none());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_name_taken_by_another_tool_is_left_alone() {
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        let tools = home.path().join("compatibilitytools.d");
        let theirs = tools.join(VER);
        tests::fake_build(&theirs, VER, Some("4183110"));
        std::fs::write(theirs.join("proton"), "ProtonUp-Qt's").unwrap();
        let m = mock(&archive("4183110"), "4183110", None, 1).await;
        let (r, notes) = install(&m, Some(&tools)).await;
        let man = r.unwrap();
        assert_eq!(man.path, None, "into the store");
        assert!(
            notes.len() == 1
                && notes[0].contains("isn't gogo's")
                && notes[0].contains("gogo's store instead"),
            "{notes:?}"
        );
        assert_eq!(
            std::fs::read_to_string(theirs.join("proton")).unwrap(),
            "ProtonUp-Qt's"
        );
        assert!(!is_managed(&theirs));
        assert_eq!(names(&tools), [VER]);
        assert!(store::proton_dir(&man).starts_with(home.path().join("runtimes")));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn gogos_own_build_is_adopted_or_reinstalled() {
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        let tools = home.path().join("compatibilitytools.d");
        std::fs::create_dir_all(&tools).unwrap();
        let body = archive("4183110");
        // Installed, then the manifest lost: adopted, no download.
        let m = mock(&body, "4183110", None, 1).await;
        let first = install(&m, Some(&tools)).await.0.unwrap();
        store::save_manifest(&[]).unwrap();
        let again = install(&m, Some(&tools)).await.0.unwrap();
        assert_eq!(
            (again.path.clone(), again.sha256.clone()),
            (first.path, first.sha256)
        );
        assert_eq!(store::load_manifest().len(), 1);
        m.server.verify().await;
        // Changed (and the manifest lost): gogo's own, so reinstalled in place.
        store::save_manifest(&[]).unwrap();
        std::fs::write(tools.join(VER).join("proton"), "edited").unwrap();
        let m = mock(&body, "4183110", None, 1).await;
        install(&m, Some(&tools)).await.0.unwrap();
        assert!(check_managed(&tools.join(VER)).is_ok());
        m.server.verify().await;
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn an_appid_umu_does_not_know_fails_naming_the_umu_bump() {
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        let tools = home.path().join("compatibilitytools.d");
        std::fs::create_dir_all(&tools).unwrap();
        let m = mock(&archive("9999999"), "9999999", None, 1).await;
        let err = install(&m, Some(&tools)).await.0.unwrap_err();
        assert!(matches!(err, MacAppError::Umu(_)), "{err:?}");
        let msg = err.to_string();
        assert!(msg.contains("9999999") && msg.contains("Bump umu"), "{msg}");
        // Nothing half-extracted left in the shared dir, nothing recorded.
        assert!(names(&tools).is_empty(), "{:?}", names(&tools));
        assert!(store::load_manifest().is_empty());
        // The catalog recording another appid than the archive's: refused.
        let m = mock(&archive("1628350"), "4183110", None, 1).await;
        let err = install(&m, None).await.0.unwrap_err().to_string();
        assert!(err.contains("the catalog records 4183110"), "{err}");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_sha512_other_than_the_sha512sum_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        let m = mock(&archive("4183110"), "4183110", Some(&"0".repeat(128)), 1).await;
        let err = install(&m, None).await.0.unwrap_err().to_string();
        assert!(
            err.contains("sha512") && err.contains(".sha512sum"),
            "{err}"
        );
        assert!(store::load_manifest().is_empty());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn prune_never_deletes_from_a_shared_tools_dir() {
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        let tools = home.path().join("compatibilitytools.d");
        std::fs::create_dir_all(&tools).unwrap();
        let m = mock(&archive("4183110"), "4183110", None, 1).await;
        let shared = install(&m, Some(&tools)).await.0.unwrap();
        // An older build in the store beside it.
        let old = store::ManifestEntry {
            version: "GE-Proton11-11".into(),
            path: None,
            entry_rel: "GE-Proton11-11-x86_64/proton".into(),
            fetched_at: "2026-10-20T00:00:00Z".into(),
            ..shared.clone()
        };
        let old_dir = home
            .path()
            .join("runtimes/ge-proton/GE-Proton11-11/x86_64/GE-Proton11-11-x86_64");
        tests::fake_build(&old_dir, "GE-Proton11-11", Some("4183110"));
        let mut all = store::load_manifest();
        all.push(old);
        store::save_manifest(&all).unwrap();
        let out = store::prune(&["ge-proton".into()]).unwrap();
        assert_eq!(
            out.removed,
            [home.path().join("runtimes/ge-proton")],
            "the store's copy goes"
        );
        assert_eq!(out.shared.len(), 1);
        assert_eq!(out.shared[0].target, format!("ge-proton/{VER}"));
        assert_eq!(out.shared[0].path, tools.join(VER));
        assert!(out.shared[0].bytes > 0);
        // On disk and in the manifest still.
        assert!(check_managed(&tools.join(VER)).is_ok());
        assert_eq!(store::load_manifest(), [shared]);
        // By version too, and the sweep offers it each time.
        let out = store::prune(&[format!("ge-proton/{VER}")]).unwrap();
        assert!(out.removed.is_empty() && out.shared.len() == 1);
        assert!(tools.join(VER).is_dir());
    }
}
