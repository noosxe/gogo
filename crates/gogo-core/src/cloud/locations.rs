//! Save locations (design 22 §3): the remote config's `<?VAR?>` paths mapped
//! onto a game's prefix and files, for the Linux lane (Proton's `steamuser`)
//! and the Mac lane (CrossOver's `crossover`).
//!
//! Resolving reads the disk and never writes it. A location whose path would
//! leave the prefix and the game's files (`..`, an absolute segment, a link
//! out) is refused: that check is the safety boundary the later pull and push
//! rely on ([`Location::file`] does it again for every file).

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::remote_config::{CloudStorage, RemoteConfig, RemoteConfigError};
use crate::macapp::launch::GameDirs;

/// Which Windows user a lane's prefix has (design 22 §3.2, D9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    /// `gogo linux-app`: a Proton prefix, user `steamuser`.
    Linux,
    /// `gogo mac-app`: a CrossOver bottle, user `crossover`.
    Mac,
}

impl Lane {
    /// The lane of the machine gogo runs on.
    pub fn host() -> Lane {
        if cfg!(target_os = "macos") {
            Lane::Mac
        } else {
            Lane::Linux
        }
    }

    pub fn windows_user(self) -> &'static str {
        match self {
            Lane::Linux => "steamuser",
            Lane::Mac => "crossover",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    #[error("{0} isn't built: no game dir for it")]
    NotBuilt(String),
    #[error(
        "no client id for {0}: its goggame-{0}.info and build-manifest.json have none (is it built?)"
    )]
    NoClientId(String),
    #[error("{0}")]
    Game(String),
    #[error(transparent)]
    RemoteConfig(#[from] RemoteConfigError),
}

/// Where a game's pieces are on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub lane: Lane,
    /// The Wine prefix (`drive_c` is directly under it).
    pub prefix: PathBuf,
    /// The game's files: `games/<gogid>/game/` (depot lane) or
    /// `<prefix>/drive_c/GOG/<name>/` (installer lane).
    pub game_files: PathBuf,
}

/// A built game: its layout and the dir that holds `build-manifest.json`.
#[derive(Debug, Clone)]
pub struct Game {
    pub gogid: String,
    pub layout: Layout,
    pub root: PathBuf,
}

impl Game {
    /// Find a built game by gogid (either lane's library), read-only.
    pub fn find(gogid: &str, lane: Lane) -> Result<Game, CloudError> {
        let dirs = GameDirs::for_game(gogid).map_err(|e| CloudError::Game(e.to_string()))?;
        if !dirs.root.is_dir() {
            return Err(CloudError::NotBuilt(gogid.to_string()));
        }
        let tree = crate::linuxapp::game::read_launch_toml(&dirs)
            .ok()
            .and_then(|(t, _)| t.tree_name());
        let game_files = crate::linuxapp::setup::game_files(&dirs, tree.as_deref());
        Ok(Game {
            gogid: gogid.to_string(),
            layout: Layout {
                lane,
                prefix: dirs.prefix.clone(),
                game_files,
            },
            root: dirs.root,
        })
    }

    /// The game's client id (public): its `goggame-<gogid>.info`, else the
    /// `clientId` field of the kept `build-manifest.json` (design 22 §2.2;
    /// nothing else of the manifest is read, never the secret).
    pub fn client_id(&self) -> Result<String, CloudError> {
        let id: u64 = self
            .gogid
            .parse()
            .map_err(|_| CloudError::NoClientId(self.gogid.clone()))?;
        let info = find_game_info(&self.layout, id);
        if let Some(c) = info.and_then(|p| client_id_of_info(&p)) {
            return Ok(c);
        }
        let manifest = crate::gog::build_manifest::path(&self.root);
        if let Some(c) = client_id_of_manifest(&manifest) {
            return Ok(c);
        }
        Err(CloudError::NoClientId(self.gogid.clone()))
    }
}

fn find_game_info(layout: &Layout, id: u64) -> Option<PathBuf> {
    crate::linuxapp::dlc_installer::info_in(&layout.game_files, id)
        .or_else(|| crate::linuxapp::dlc_installer::find_info(&layout.prefix.join("drive_c"), id))
}

fn json_of(path: &Path) -> Option<serde_json::Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(raw.trim_start_matches('\u{feff}')).ok()
}

fn client_id_of_info(path: &Path) -> Option<String> {
    let v = json_of(path)?;
    let c = v.get("clientId")?.as_str()?;
    super::remote_config::check_client_id(c).ok()?;
    Some(c.to_string())
}

/// `clientId` at the top of the manifest value or inside its version
/// wrapper (`{"V2": {...}}`), whichever gogo's serialisation produced.
fn client_id_of_manifest(path: &Path) -> Option<String> {
    let v = json_of(path)?;
    let m = v.get("manifest")?;
    let found = m.get("clientId").or_else(|| {
        m.as_object()?
            .values()
            .find_map(|inner| inner.get("clientId"))
    })?;
    let c = found.as_str()?;
    super::remote_config::check_client_id(c).ok()?;
    Some(c.to_string())
}

// — variables —

/// The `<?VAR?>` variables a Windows build's location may start with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Variable {
    Install,
    Documents,
    SavedGames,
    ApplicationDataRoaming,
    ApplicationDataLocal,
    ApplicationDataLocalLow,
}

impl Variable {
    fn parse(name: &str) -> Result<Variable, SkipReason> {
        Ok(match name {
            "INSTALL" => Variable::Install,
            "DOCUMENTS" => Variable::Documents,
            "SAVED_GAMES" => Variable::SavedGames,
            "APPLICATION_DATA_ROAMING" => Variable::ApplicationDataRoaming,
            "APPLICATION_DATA_LOCAL" => Variable::ApplicationDataLocal,
            "APPLICATION_DATA_LOCAL_LOW" => Variable::ApplicationDataLocalLow,
            "APPLICATION_SUPPORT" => return Err(SkipReason::NotForWindowsBuild),
            other => return Err(SkipReason::UnknownVariable(other.to_string())),
        })
    }

    /// The variable as GOG writes it, without the `<?` `?>`.
    pub fn name(self) -> &'static str {
        match self {
            Variable::Install => "INSTALL",
            Variable::Documents => "DOCUMENTS",
            Variable::SavedGames => "SAVED_GAMES",
            Variable::ApplicationDataRoaming => "APPLICATION_DATA_ROAMING",
            Variable::ApplicationDataLocal => "APPLICATION_DATA_LOCAL",
            Variable::ApplicationDataLocalLow => "APPLICATION_DATA_LOCAL_LOW",
        }
    }

    /// Path parts under the Windows user's dir (`None` for `INSTALL`).
    fn under_user(self) -> Option<&'static [&'static str]> {
        match self {
            Variable::Install => None,
            Variable::Documents => Some(&["Documents"]),
            Variable::SavedGames => Some(&["Saved Games"]),
            Variable::ApplicationDataRoaming => Some(&["AppData", "Roaming"]),
            Variable::ApplicationDataLocal => Some(&["AppData", "Local"]),
            Variable::ApplicationDataLocalLow => Some(&["AppData", "LocalLow"]),
        }
    }
}

/// Why one location was left out (the others still resolve).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum SkipReason {
    /// A first part that is no `<?VAR?>` (a drive letter, a relative or
    /// absolute path), or a `<?VAR?>` in a later part.
    NotAVariable(String),
    UnknownVariable(String),
    /// `<?APPLICATION_SUPPORT?>` is macOS-only.
    NotForWindowsBuild,
    /// A `.`, `..`, NUL or `:` part, or none after the variable's own.
    BadPath(String),
    /// The location's name can't be a cloud dir name.
    BadName(String),
    DuplicateName,
    /// Two dirs differing only by case match a part.
    Ambiguous(String),
    /// The resolved path leaves the prefix and the game's files.
    EscapesPrefix,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkipReason::NotAVariable(p) => {
                write!(f, "starts with {p:?}, not a <?VARIABLE?>")
            }
            SkipReason::UnknownVariable(v) => write!(f, "unknown variable <?{v}?>"),
            SkipReason::NotForWindowsBuild => {
                write!(f, "<?APPLICATION_SUPPORT?> is for macOS builds only")
            }
            SkipReason::BadPath(p) => write!(f, "unusable path part {p:?}"),
            SkipReason::BadName(n) => write!(f, "unusable location name {n:?}"),
            SkipReason::DuplicateName => write!(f, "another location has this name"),
            SkipReason::Ambiguous(p) => write!(
                f,
                "several dirs match {p:?} apart from case, can't tell which one"
            ),
            SkipReason::EscapesPrefix => {
                write!(f, "the path leaves the prefix and the game's files")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skipped {
    pub name: String,
    pub location: String,
    pub reason: SkipReason,
    pub message: String,
}

/// `<?VAR?>` → its name.
fn variable_name(part: &str) -> Option<&str> {
    part.strip_prefix("<?")?.strip_suffix("?>")
}

/// Split a Windows-style location on any run of `/` and `\` (design 22
/// §3.1 step 2): the variable and the parts after it.
pub fn parse_location(location: &str) -> Result<(Variable, Vec<String>), SkipReason> {
    let mut parts = location.split(['/', '\\']).filter(|p| !p.is_empty());
    let first = parts.next().unwrap_or("");
    let Some(var) = variable_name(first) else {
        return Err(SkipReason::NotAVariable(first.to_string()));
    };
    let var = Variable::parse(var)?;
    let mut rest = Vec::new();
    for p in parts {
        if p.contains("<?") {
            return Err(SkipReason::NotAVariable(p.to_string()));
        }
        check_part(p)?;
        rest.push(p.to_string());
    }
    Ok((var, rest))
}

/// A part that is safe to join under a dir: not `.`/`..`, no NUL, and no
/// `:` (a drive letter or a stream name on Windows, never in a save path).
fn check_part(p: &str) -> Result<(), SkipReason> {
    if p == "." || p == ".." || p.contains('\0') || p.contains(':') {
        Err(SkipReason::BadPath(p.to_string()))
    } else {
        Ok(())
    }
}

// — the escape guard —

/// The dirs a resolved path may be in: the prefix and the game's files, both
/// canonical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots(Vec<PathBuf>);

impl Roots {
    pub fn of(layout: &Layout) -> Roots {
        let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        Roots(vec![canon(&layout.prefix), canon(&layout.game_files)])
    }

    /// Whether `path` stays inside the roots: its deepest existing ancestor,
    /// with every link resolved, must be under one of them. A dangling link
    /// counts as leaving. Parts that don't exist yet are checked lexically
    /// (no `..` allowed).
    pub fn contains(&self, path: &Path) -> bool {
        use std::path::Component;
        if path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return false;
        }
        let mut probe = path.to_path_buf();
        loop {
            match std::fs::symlink_metadata(&probe) {
                Ok(_) => {
                    return match probe.canonicalize() {
                        Ok(c) => self.0.iter().any(|r| c.starts_with(r)),
                        Err(_) => false,
                    };
                }
                Err(_) => {
                    if !probe.pop() {
                        return false;
                    }
                }
            }
        }
    }
}

enum Lookup {
    Found(PathBuf),
    Missing,
    Ambiguous,
}

/// `dir/part` with the exact case, else the one entry of `dir` that matches
/// without regard to case (Windows paths are case-insensitive, Wine's own
/// lookup is too), else nothing.
fn lookup(dir: &Path, part: &str) -> Lookup {
    let exact = dir.join(part);
    if std::fs::symlink_metadata(&exact).is_ok() {
        return Lookup::Found(exact);
    }
    let want = part.to_lowercase();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Lookup::Missing;
    };
    let mut hits: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().to_lowercase() == want)
        .map(|e| e.path())
        .collect();
    match hits.len() {
        0 => Lookup::Missing,
        1 => Lookup::Found(hits.remove(0)),
        _ => Lookup::Ambiguous,
    }
}

/// Join `parts` under `base`, looking each one up case-insensitively while
/// the dirs exist; a part that doesn't exist keeps its exact case, and so
/// do the parts after it. Checked against `roots` at the end.
fn join_checked(roots: &Roots, base: &Path, parts: &[String]) -> Result<PathBuf, SkipReason> {
    let mut cur = base.to_path_buf();
    let mut missing = false;
    for p in parts {
        check_part(p)?;
        if missing {
            cur.push(p);
            continue;
        }
        match lookup(&cur, p) {
            Lookup::Found(f) => cur = f,
            Lookup::Missing => {
                missing = true;
                cur.push(p);
            }
            Lookup::Ambiguous => return Err(SkipReason::Ambiguous(p.clone())),
        }
    }
    if roots.contains(&cur) {
        Ok(cur)
    } else {
        Err(SkipReason::EscapesPrefix)
    }
}

// — locations —

/// A resolved save location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Location {
    /// The cloud dir's name: the first part of every object under it.
    pub name: String,
    pub variable: Variable,
    /// Absolute host path (it may not exist yet).
    pub path: PathBuf,
    pub exists: bool,
    /// `__default` (design 22 D10): nobody verified it against a real game.
    pub unverified: bool,
    #[serde(skip)]
    roots: Roots,
}

impl Location {
    pub fn is_install(&self) -> bool {
        self.variable == Variable::Install
    }

    /// `rel` (a cloud object's path below the location, `/` or `\`
    /// separated) as a host path: parts looked up without regard to case,
    /// and refused when it leaves the prefix and the game's files (`..`, an
    /// absolute part, a link out). The one function the pull and push go
    /// through for every file.
    pub fn file(&self, rel: &str) -> Result<PathBuf, SkipReason> {
        let parts: Vec<String> = rel
            .split(['/', '\\'])
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect();
        if parts.is_empty() || rel.starts_with(['/', '\\']) {
            return Err(SkipReason::BadPath(rel.to_string()));
        }
        join_checked(&self.roots, &self.path, &parts)
    }

    /// Whether `rel` (below the location) is never synced (design 22 §3.4).
    pub fn excluded(&self, rel: &str) -> bool {
        excluded(rel)
    }
}

/// Partial-file patterns on top of design 20 §6.1's excludes.
const PARTIAL: &[&str] = &[".part", ".tmp", ".~incomplete"];

/// Whether a file below a save location is left out of the sync: design 20
/// §6.1's caches and logs, and partial files. (`<?INSTALL?>` locations also
/// leave out the build's own files; that filter isn't wired in yet, see
/// design 22 "as built".)
pub fn excluded(rel: &str) -> bool {
    let name = rel
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    PARTIAL.iter().any(|s| name.ends_with(s))
        || crate::snapshot::saves::excluded(&rel.replace('\\', "/"))
}

/// What a remote config resolves to for one game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Resolved {
    /// Cloud storage is on for the game's Windows build.
    pub enabled: bool,
    pub locations: Vec<Location>,
    pub skipped: Vec<Skipped>,
}

impl Resolved {
    fn off() -> Resolved {
        Resolved {
            enabled: false,
            locations: vec![],
            skipped: vec![],
        }
    }
}

/// Valid as a cloud dir name: it's the first part of an object name.
fn check_name(name: &str) -> Result<(), SkipReason> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', '\0'])
        || name.trim() != name
    {
        Err(SkipReason::BadName(name.to_string()))
    } else {
        Ok(())
    }
}

/// The dir a variable is under and the parts below it: the game's files for
/// `INSTALL`, else the Windows user's dir in the prefix.
fn base_of(layout: &Layout, var: Variable) -> (PathBuf, Vec<String>) {
    match var.under_user() {
        None => (layout.game_files.clone(), vec![]),
        Some(under) => (
            layout.prefix.clone(),
            ["drive_c", "users", layout.lane.windows_user()]
                .iter()
                .chain(under)
                .map(|s| s.to_string())
                .collect(),
        ),
    }
}

fn resolve_one(
    layout: &Layout,
    roots: &Roots,
    var: Variable,
    rest: &[String],
) -> Result<PathBuf, SkipReason> {
    let (base, mut parts) = base_of(layout, var);
    parts.extend(rest.iter().cloned());
    join_checked(roots, &base, &parts)
}

/// Resolve `storage` (a game's `Windows.cloudStorage`) onto `layout`.
pub fn resolve_storage(
    storage: Option<&CloudStorage>,
    client_id: &str,
    layout: &Layout,
) -> Resolved {
    let Some(storage) = storage.filter(|s| s.enabled) else {
        return Resolved::off();
    };
    let roots = Roots::of(layout);
    let mut out = Resolved {
        enabled: true,
        locations: vec![],
        skipped: vec![],
    };
    let push = |name: &str,
                location: &str,
                parsed: Result<(Variable, Vec<String>), SkipReason>,
                unverified: bool,
                out: &mut Resolved| {
        let result = check_name(name)
            .and_then(|()| {
                if out.locations.iter().any(|l| l.name == name) {
                    Err(SkipReason::DuplicateName)
                } else {
                    Ok(())
                }
            })
            .and(parsed)
            .and_then(|(var, rest)| {
                let path = resolve_one(layout, &roots, var, &rest)?;
                Ok((var, path))
            });
        match result {
            Ok((variable, path)) => out.locations.push(Location {
                name: name.to_string(),
                variable,
                exists: path.is_dir(),
                path,
                unverified,
                roots: roots.clone(),
            }),
            Err(reason) => out.skipped.push(Skipped {
                name: name.to_string(),
                location: location.to_string(),
                message: reason.to_string(),
                reason,
            }),
        }
    };
    if storage.locations.is_empty() {
        // Design 22 §3.3 (D10): every client falls back to this one.
        let rest: Vec<String> = [
            "GOG.com",
            "Galaxy",
            "Applications",
            client_id,
            "Storage",
            "Shared",
            "Files",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let loc = format!(
            "<?APPLICATION_DATA_LOCAL?>/GOG.com/Galaxy/Applications/{client_id}/Storage/Shared/Files"
        );
        let parsed = super::remote_config::check_client_id(client_id)
            .map_err(|_| SkipReason::BadPath(client_id.to_string()))
            .map(|()| (Variable::ApplicationDataLocal, rest));
        push("__default", &loc, parsed, true, &mut out);
    }
    for l in &storage.locations {
        push(
            &l.name,
            &l.location,
            parse_location(&l.location),
            false,
            &mut out,
        );
    }
    out
}

/// Resolve a remote config's Windows locations onto `layout` (design 22 §3).
pub fn resolve(cfg: &RemoteConfig, client_id: &str, layout: &Layout) -> Resolved {
    resolve_storage(cfg.windows_storage(), client_id, layout)
}

#[cfg(test)]
mod tests;
