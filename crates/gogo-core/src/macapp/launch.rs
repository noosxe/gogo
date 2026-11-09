//! The v2 launcher contract (design 16 §1, §2, §8): `gogo-launch.toml` (the
//! source of truth), its shell mirror `launch.conf`, the config stamp, the
//! per-game state dir `<state_dir>/games/<gogid>/`, runtime references into
//! the store, and the bash launcher itself. `gogo mac-app setup` is
//! `macapp/setup.rs`.
//!
//! The `.app` holds only the launcher, the TOML and its mirror, `Info.plist`
//! and licence notices. It is signed once and never written again; the
//! prefix, the game files and the effective config live in the state dir.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::compat::{self, CompatEntry, Layer};
use super::error::MacAppError;
use super::runtime::{self, Runtime, RuntimeMode};

/// `gogo-launch.toml` schema version this gogo reads and writes.
pub const SCHEMA: u32 = 1;

/// `Contents/Resources/gogo-launch.toml` (design 16 §1.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchToml {
    pub schema: u32,
    pub gogid: String,
    pub title: String,
    /// `<id>/<ver>` in the store, or `path:<dir>` for a runtime outside it.
    pub runtime: String,
    pub renderer: String,
    /// Windows path of the entry point (`C:\GOG\<name>\game.exe`).
    pub exe: String,
    /// Windows path the game starts in.
    pub workdir: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub dll_overrides: BTreeMap<String, String>,
    /// winetricks verbs; applied by setup, never by the launcher. A
    /// portable bundle lists the verbs whose files it carries.
    #[serde(default)]
    pub verbs: Vec<String>,
    /// `[portable]`: a self-contained bundle (#84). Absent in a v2 bundle,
    /// so its bytes and stamp don't change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portable: Option<Portable>,
    pub gogo: GogoRef,
    /// `[[variant]]`: the game's runnable `playTasks` (#85). The top-level
    /// `exe`/`workdir`/`args` are what runs by default; setup switches to
    /// one of these when L4 (or L3) names it in `variant`.
    #[serde(rename = "variant", default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<Variant>,
    /// `lane = "installer"`: built by running GOG's offline installer (#56).
    /// Absent = from the depot build, so a depot bundle's bytes don't change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<compat::Lane>,
}

/// One `[[variant]]` of `gogo-launch.toml` (#85), from a `playTasks` entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Variant {
    pub name: String,
    /// Windows path, like the top-level `exe`.
    pub exe: String,
    pub workdir: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// `[portable]` of a `--portable` bundle (#84, docs/19 §6.1): the engine,
/// its dylibs, the game tree and the files the verbs installed are inside
/// the bundle, and the launcher builds the prefix from them on first launch,
/// on whatever Mac it runs on, with no gogo there. Paths are relative to
/// `Contents/Resources`. The rest of the TOML is the effective config as
/// the build resolved it (flags, L4, L3, L2, L1): nothing is applied later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Portable {
    /// The bundled engine's wine binary (`wine/bin/wine`).
    pub wine: String,
    /// The dylib dir for `DYLD_FALLBACK_LIBRARY_PATH`, when the engine has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frameworks: Option<String>,
    /// The game tree (`game/<name>`); the launcher clones it to
    /// `games/<gogid>/game`, where the game may write.
    pub game: String,
    /// What the build's winetricks run added under `drive_c/windows`
    /// (`verbs`, laid out like that dir); copied into the prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<String>,
    /// Registry values the launcher writes into a new prefix: the GOG.com
    /// key (L2), then L3 and L4's `registry`.
    #[serde(rename = "registry", default, skip_serializing_if = "Vec::is_empty")]
    pub registry: Vec<RegValue>,
}

/// One `[[portable.registry]]` value, a `reg add` the launcher runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegValue {
    pub key: String,
    /// Absent: the key's default value (`/ve`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    pub data: String,
    /// `/reg:32`, the view GOG's 32-bit installers write.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wow32: bool,
}

impl RegValue {
    /// The `reg add …` argv (after `wine`).
    pub fn argv(&self) -> Vec<String> {
        let mut argv = vec!["reg".to_string(), "add".to_string(), self.key.clone()];
        match &self.name {
            Some(name) => argv.extend(["/v".to_string(), name.clone()]),
            None => argv.push("/ve".to_string()),
        }
        argv.extend([
            "/t".to_string(),
            self.kind.clone(),
            "/d".to_string(),
            self.data.clone(),
            "/f".to_string(),
        ]);
        if self.wow32 {
            argv.push("/reg:32".to_string());
        }
        argv
    }
}

impl From<&compat::RegistryValue> for RegValue {
    fn from(v: &compat::RegistryValue) -> RegValue {
        RegValue {
            key: v.key.clone(),
            name: v.name.clone(),
            kind: v.kind.clone().unwrap_or_else(|| "REG_SZ".to_string()),
            data: v.data.clone(),
            wow32: false,
        }
    }
}

/// `[gogo]`: where the launcher finds gogo and its state dir.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GogoRef {
    /// The path gogo was invoked as (not canonicalized, decision 2).
    pub path: PathBuf,
    pub state: PathBuf,
}

impl LaunchToml {
    /// The lane the bundle was built from: `lane`, else the depot build.
    pub fn built_from(&self) -> compat::Lane {
        self.lane.unwrap_or(compat::Lane::Depot)
    }

    /// Parse, refusing a newer `schema` outright rather than half-reading it.
    pub fn parse(raw: &str, path: &Path) -> Result<LaunchToml, MacAppError> {
        let err = |reason: String| MacAppError::Compat {
            path: path.to_path_buf(),
            reason,
        };
        let table: toml::Table = toml::from_str(raw).map_err(|e| err(e.to_string()))?;
        let schema = table.get("schema").and_then(toml::Value::as_integer);
        match schema {
            Some(s) if s > i64::from(SCHEMA) => {
                return Err(err(format!(
                    "schema {s} is newer than this gogo understands ({SCHEMA}): upgrade gogo"
                )));
            }
            Some(_) => {}
            None => return Err(err("missing `schema`".into())),
        }
        toml::from_str(raw).map_err(|e| err(e.to_string()))
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("LaunchToml always serializes")
    }

    /// The game dir's Windows path: the first two components of `exe`
    /// (`C:\GOG\<name>`), which is where a relative L3/L4 `exe` resolves.
    pub fn game_dir(&self) -> String {
        let parts: Vec<&str> = self.exe.split('\\').collect();
        parts[..parts.len().min(3)].join("\\")
    }

    /// `<name>` of `C:\GOG\<name>`, the tree name the prefix symlink uses.
    pub fn tree_name(&self) -> Option<String> {
        let parts: Vec<&str> = self.exe.split('\\').collect();
        (parts.len() >= 4 && parts[1].eq_ignore_ascii_case("GOG")).then(|| parts[2].to_string())
    }

    /// The entry exe's dir in the game tree at `root` (the `C:\\GOG\\<name>`
    /// of [`Self::game_dir`], wherever it sits): where the game's `ddraw.dll`
    /// counts and where gogo's cnc-ddraw goes (#199, design 18 §3). `root`
    /// when that dir isn't there.
    pub fn exe_dir_in(&self, root: &Path) -> PathBuf {
        self.dir_in(root, &self.exe)
    }

    /// Every dir in the tree at `root` a `ddraw.dll` of gogo's may have been
    /// put in by an earlier setup: the root (where it went before #199), the
    /// entry exe's and the variants'. Only one is in use at a time, so setup
    /// takes it out of the others ([`Self::exe_dir_in`]).
    pub fn ddraw_dirs_in(&self, root: &Path) -> BTreeSet<PathBuf> {
        let exes = std::iter::once(&self.exe).chain(self.variants.iter().map(|v| &v.exe));
        exes.map(|e| self.dir_in(root, e))
            .chain([root.to_path_buf()])
            .collect()
    }

    fn dir_in(&self, root: &Path, exe: &str) -> PathBuf {
        let dir = exe
            .strip_prefix(&format!("{}\\", self.game_dir()))
            .and_then(|rel| rel.rsplit_once('\\'))
            .map(|(head, _)| head.split('\\').fold(root.to_path_buf(), |p, c| p.join(c)));
        dir.filter(|d| d.is_dir())
            .unwrap_or_else(|| root.to_path_buf())
    }

    /// The variant the bundle runs by default: the one whose exe and args
    /// are the top-level ones (else the first with that exe). `None` when
    /// an L3 `exe` or a recipe replaced the info file's pick.
    pub fn default_variant(&self) -> Option<&Variant> {
        self.variants
            .iter()
            .find(|v| v.exe == self.exe && v.args == self.args)
            .or_else(|| self.variants.iter().find(|v| v.exe == self.exe))
    }

    /// This TOML running the variant `name` instead of its default (#85).
    /// An unknown name is an error listing the ones there are: a variant
    /// never falls back to the default silently.
    pub fn select_variant(&self, name: &str) -> Result<LaunchToml, String> {
        let Some(v) = self.variants.iter().find(|v| v.name == name) else {
            if self.variants.is_empty() {
                return Err(format!(
                    "variant {name:?}: this bundle has no variants (rebuild it with this gogo)"
                ));
            }
            let names: Vec<String> = self
                .variants
                .iter()
                .map(|v| format!("{:?}", v.name))
                .collect();
            return Err(format!(
                "unknown variant {name:?}; available: {}",
                names.join(", ")
            ));
        };
        let mut out = self.clone();
        out.exe = v.exe.clone();
        out.workdir = v.workdir.clone();
        out.args = v.args.clone();
        Ok(out)
    }

    /// What setup runs (#85): the `variant` L4 names (else L3's) switches
    /// exe, workdir and args, then [`Self::with_override`] applies L4. So
    /// an L4 `exe`/`args` beat any variant, and a variant beats the baked
    /// (L3) `exe`/`args`. An unknown variant is an error.
    pub fn effective(&self, db: &CompatEntry, l4: &CompatEntry) -> Result<LaunchToml, String> {
        let base = match l4.variant.as_deref().or(db.variant.as_deref()) {
            Some(name) => self.select_variant(name)?,
            None => self.clone(),
        };
        Ok(base.with_override(l4))
    }

    /// This entry as a compat layer (what the bundle baked: L0–L3 + flags).
    fn as_compat(&self) -> CompatEntry {
        CompatEntry {
            exe: Some(self.exe.clone()),
            renderer: Some(self.renderer.clone()),
            runtime: Some(self.runtime.clone()),
            verbs: Some(self.verbs.clone()),
            dll_overrides: self.dll_overrides.clone(),
            env: self.env.clone(),
            args: Some(self.args.clone()),
            ..Default::default()
        }
    }

    /// The effective config: this bundle's TOML with the L4 override applied
    /// field by field (design 16 §1.2, decision 5). Fields L4 can't express
    /// in launch terms (status, tested, …) are ignored here.
    pub fn with_override(&self, l4: &CompatEntry) -> LaunchToml {
        let eff = compat::merge(&[(Layer::Db, &self.as_compat()), (Layer::User, l4)]);
        let mut out = self.clone();
        if let Some(r) = eff.runtime {
            out.runtime = r.value;
        }
        if let Some(r) = eff.renderer {
            out.renderer = r.value;
        }
        if let Some(e) = eff.exe {
            let exe = e.value;
            out.exe = if is_windows_abs(&exe) {
                exe
            } else {
                format!("{}\\{}", self.game_dir(), exe.replace('/', "\\"))
            };
            if out.exe != self.exe {
                out.workdir = windows_parent(&out.exe);
            }
        }
        if let Some(a) = eff.args {
            out.args = a.value;
        }
        out.verbs = eff.verbs.into_iter().map(|s| s.value).collect();
        out.env = eff.env.into_iter().map(|(k, s)| (k, s.value)).collect();
        out.dll_overrides = eff
            .dll_overrides
            .into_iter()
            .map(|(k, s)| (k, s.value))
            .collect();
        out
    }
}

fn is_windows_abs(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\'
}

fn windows_parent(p: &str) -> String {
    p.rsplit_once('\\')
        .map(|(head, _)| head.to_string())
        .unwrap_or_else(|| p.to_string())
}

/// `<prefix>/drive_c/a/b.exe` → `C:\a\b.exe`. The path must sit under the
/// prefix's `drive_c` (both build lanes put the game there).
pub fn unix_to_windows(prefix: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(prefix.join("drive_c")).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(format!("C:\\{}", parts.join("\\")))
}

/// The unix path of a `C:\…` path inside `prefix` (the inverse of
/// [`unix_to_windows`]); `None` for other drives.
pub fn windows_to_unix(prefix: &Path, path: &str) -> Option<PathBuf> {
    let rest = path
        .strip_prefix("C:\\")
        .or_else(|| path.strip_prefix("c:\\"))?;
    Some(
        rest.split('\\')
            .filter(|p| !p.is_empty())
            .fold(prefix.join("drive_c"), |acc, p| acc.join(p)),
    )
}

/// POSIX single-quote (the same quoting the fat bundle's `launch.conf` uses).
pub(crate) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub(crate) fn sh_array(items: impl IntoIterator<Item = String>) -> String {
    let quoted: Vec<String> = items.into_iter().map(|s| sh_quote(&s)).collect();
    format!("({})", quoted.join(" "))
}

/// The shell mirror of a [`LaunchToml`] (design 16 §1.2): assignments only,
/// every value single-quoted, arrays for lists. `extra` appends resolved
/// keys (the state copy's `RUNTIME_DIR`, `PREFIX`, `STAMP`, …).
pub fn to_launch_conf(t: &LaunchToml, extra: &[(&str, String)]) -> String {
    let mut out = String::from("# Generated by gogo from gogo-launch.toml. Do not edit.\n");
    let mut kv = |k: &str, v: String| out.push_str(&format!("{k}={v}\n"));
    kv("SCHEMA", t.schema.to_string());
    kv("GOGID", sh_quote(&t.gogid));
    kv("TITLE", sh_quote(&t.title));
    kv("RUNTIME", sh_quote(&t.runtime));
    kv("RENDERER", sh_quote(&t.renderer));
    kv("EXE", sh_quote(&t.exe));
    kv("WORKDIR", sh_quote(&t.workdir));
    kv("ARGS", sh_array(t.args.iter().cloned()));
    kv(
        "ENV",
        sh_array(t.env.iter().map(|(k, v)| format!("{k}={v}"))),
    );
    let dll: Vec<String> = t
        .dll_overrides
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    kv("DLL_OVERRIDES", sh_quote(&dll.join(";")));
    kv("VERBS", sh_array(t.verbs.iter().cloned()));
    kv("GOGO_PATH", sh_quote(&t.gogo.path.to_string_lossy()));
    kv("GOGO_STATE", sh_quote(&t.gogo.state.to_string_lossy()));
    // #84: what the launcher sets up a prefix from, relative to Resources.
    // `REG` is five words per value: key, name (empty: the default value),
    // type, data, view (`32` or empty).
    if let Some(p) = &t.portable {
        let wine = Path::new(&p.wine);
        let engine = wine
            .parent()
            .and_then(Path::parent)
            .unwrap_or(Path::new(""));
        kv("PORTABLE", sh_quote("1"));
        kv("WINE", sh_quote(&p.wine));
        kv(
            "WINESERVER",
            sh_quote(&wine.with_file_name("wineserver").to_string_lossy()),
        );
        kv("RUNTIME_DIR", sh_quote(&engine.to_string_lossy()));
        kv(
            "FRAMEWORKS",
            sh_quote(p.frameworks.as_deref().unwrap_or_default()),
        );
        kv("GAME_SRC", sh_quote(&p.game));
        kv(
            "WINDOWS_SRC",
            sh_quote(p.windows.as_deref().unwrap_or_default()),
        );
        kv(
            "REG",
            sh_array(p.registry.iter().flat_map(|r| {
                [
                    r.key.clone(),
                    r.name.clone().unwrap_or_default(),
                    r.kind.clone(),
                    r.data.clone(),
                    if r.wow32 { "32".into() } else { String::new() },
                ]
            })),
        );
    }
    for (k, v) in extra {
        kv(k, sh_quote(v));
    }
    out
}

/// The config stamp (design 16 §2.1): sha256 over the bundle TOML's bytes
/// followed by the L4 file's bytes. No L4 file contributes nothing (the
/// "empty marker" is zero bytes), so the launcher reproduces it with
/// `cat toml [l4] | shasum -a 256`.
pub fn stamp(bundle_toml: &[u8], l4: Option<&[u8]>) -> String {
    let mut h = Sha256::new();
    h.update(bundle_toml);
    if let Some(l4) = l4 {
        h.update(l4);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

// — the state dir —

/// `<data_root>/games/`.
pub fn games_dir() -> PathBuf {
    crate::state::data_root().join("games")
}

/// One game's state (design 16 §0, decision 6).
#[derive(Debug, Clone, PartialEq)]
pub struct GameDirs {
    pub root: PathBuf,
    pub prefix: PathBuf,
    /// Depot lane only: the APFS clone of the depot tree.
    pub game: PathBuf,
    pub state_toml: PathBuf,
    pub launch_conf: PathBuf,
}

impl GameDirs {
    pub fn at(root: PathBuf) -> GameDirs {
        GameDirs {
            prefix: root.join("prefix"),
            game: root.join("game"),
            state_toml: root.join("state.toml"),
            launch_conf: root.join("launch.conf"),
            root,
        }
    }

    /// Where `gogid` lives: its recorded location (design 18 §6.1), else
    /// `games/<gogid>`. Every reader finds a game through this, whatever
    /// the library setting says now.
    pub fn for_game(gogid: &str) -> Result<GameDirs, MacAppError> {
        compat::validate_gogid(gogid)?;
        Ok(GameDirs::at(
            recorded_location(gogid).unwrap_or_else(|| games_dir().join(gogid)),
        ))
    }

    /// Where a build of `gogid` goes: where the game already is, else the
    /// library, else `games/<gogid>`. Changing the setting moves nothing. A
    /// game recorded somewhere that isn't there is refused: a build would
    /// fill the mount point of a drive that isn't mounted.
    pub fn for_build(gogid: &str, library: Option<&Library>) -> Result<GameDirs, MacAppError> {
        let found = GameDirs::for_game(gogid)?;
        let recorded = recorded_location(gogid).is_some();
        if recorded && !found.root.is_dir() {
            return Err(MacAppError::Library {
                path: found.root,
                reason: format!(
                    "{gogid} is recorded there and it isn't there (a drive not mounted?): \
                     mount it, or `gogo apps remove {gogid}` first"
                ),
            });
        }
        let placed = recorded || found.root.symlink_metadata().is_ok();
        Ok(match library {
            Some(l) if !placed => GameDirs::at(l.dir().join(gogid)),
            _ => found,
        })
    }
}

// — where a game lives: the game library (design 18 §6.1) —

/// `<data_root>/game-locations/`: one file per game outside `games/`, named
/// by its gogid and holding the game's absolute dir. It stays with the data
/// root, because everything else about the game is in the dir it points at.
pub fn locations_dir() -> PathBuf {
    crate::state::data_root().join("game-locations")
}

/// A location file's contents, when they're what gogo writes: an absolute
/// path, without `..`, whose last part is the game's id. Anything else isn't
/// trusted, since `gogo apps remove` deletes this dir.
pub(crate) fn parse_location(raw: &str, gogid: &str) -> Option<PathBuf> {
    let p = Path::new(raw.strip_suffix('\n').unwrap_or(raw));
    let safe = p.is_absolute()
        && p.file_name().is_some_and(|n| n == gogid)
        && !p
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
    safe.then(|| p.to_path_buf())
}

/// The location recorded for `gogid`, if it has one. The dir may be gone (a
/// drive that isn't mounted): callers see that as a game without files.
pub fn recorded_location(gogid: &str) -> Option<PathBuf> {
    let raw = std::fs::read_to_string(locations_dir().join(gogid)).ok()?;
    parse_location(&raw, gogid)
}

/// Is `root` somewhere other than `games/`?
pub fn in_library(root: &Path) -> bool {
    !root.starts_with(games_dir())
}

/// Record that `gogid` lives at `root`, creating `root`: a record of a dir
/// that isn't there means a drive that isn't mounted. A game under `games/`
/// needs no record, and gets none: nothing new appears for anyone without a
/// library.
pub fn record_location(gogid: &str, root: &Path) -> Result<(), MacAppError> {
    if !in_library(root) {
        return Ok(());
    }
    std::fs::create_dir_all(root)?;
    write_atomic(
        &locations_dir().join(gogid),
        &format!("{}\n", root.display()),
    )
}

/// Drop `gogid`'s record, if it has one.
pub fn forget_location(gogid: &str) -> Result<(), MacAppError> {
    match std::fs::remove_file(locations_dir().join(gogid)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Every game with a location record, as `(gogid, dir)`, sorted.
pub fn recorded_games() -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = std::fs::read_dir(locations_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let gogid = e.file_name().into_string().ok()?;
            compat::validate_gogid(&gogid).ok()?;
            let root = recorded_location(&gogid)?;
            Some((gogid, root))
        })
        .collect();
    out.sort();
    out
}

/// Recorded games whose dir isn't there now (a drive that isn't mounted).
/// What they reference can't be read, so the store's GC stops for them.
pub fn unreachable_games() -> Vec<(String, PathBuf)> {
    let mut out = recorded_games();
    out.retain(|(_, root)| !root.is_dir());
    out
}

/// Where the installer lane stages a game whose id isn't known yet: on the
/// filesystem the game will end up on.
pub fn staging_root(library: Option<&Library>, slug: &str) -> PathBuf {
    library
        .map(Library::dir)
        .map_or_else(games_dir, Path::to_path_buf)
        .join(format!(".build-{slug}-{}", std::process::id()))
}

/// The warning for a build whose game stays out of `library`, if it does.
pub fn stays_note(dirs: &GameDirs, library: Option<&Library>) -> Option<String> {
    let library = library?;
    (!dirs.root.starts_with(library.dir())).then(|| {
        let how = if cfg!(target_os = "linux") {
            format!(
                "`gogo linux-app move {} <dir>`",
                dirs.root.file_name().unwrap_or_default().to_string_lossy()
            )
        } else {
            "`gogo apps remove` and a new build".to_string()
        };
        format!(
            "this game is already at {}, and the library setting doesn't move games: it \
             stays there ({how} puts it in {})",
            dirs.root.display(),
            library.dir().display()
        )
    })
}

/// [`std::fs::rename`], with a copy when `from` and `to` are on different
/// filesystems (an installer build of a game that stayed where it was).
pub(crate) fn move_tree(from: &Path, to: &Path) -> Result<(), MacAppError> {
    match std::fs::rename(from, to) {
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            crate::gog::materialize::materialize_tree(
                from,
                to,
                crate::gog::materialize::Reflink::Try,
            )?;
            std::fs::remove_dir_all(from)?;
            Ok(())
        }
        other => Ok(other?),
    }
}

/// A checked `library.dir` (design 05 §3): games go in `<dir>/<gogid>/`. The
/// only way to get one is [`Library::resolve`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Library {
    dir: PathBuf,
}

impl Library {
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Check a configured library dir. `None`: it is `games/` itself, which
    /// is what no library means. Refused: a dir inside the data root (only
    /// `games/<gogid>` and a record find a game), the download dir or the
    /// depot cache (their cleanups and walks must not meet a game), one gogo
    /// can't write to or that can't hold the prefix's symlinks. Creates nothing but a probe, removed
    /// again, in the nearest dir that exists.
    pub fn resolve(
        dir: &Path,
        download_dir: Option<&Path>,
    ) -> Result<Option<Library>, MacAppError> {
        let has_parent = |p: &Path| {
            p.components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        };
        // A location record can't hold `..`, so it's resolved here, or
        // refused below when it goes through a dir that doesn't exist.
        let dir = absolute(dir);
        let dir = if has_parent(&dir) {
            real_path(&dir)
        } else {
            dir
        };
        let refuse = |reason: String| MacAppError::Library {
            path: dir.clone(),
            reason,
        };
        if has_parent(&dir) {
            return Err(refuse("`..` goes through a dir that doesn't exist".into()));
        }
        // Location files and launch.conf hold it as one line of text.
        if dir.to_str().is_none_or(|s| s.contains('\n')) {
            return Err(refuse("the path has to be UTF-8, on one line".into()));
        }
        let real = real_path(&dir);
        let data_root = crate::state::data_root();
        if real == real_path(&games_dir()) {
            return Ok(None);
        }
        let real_root = real_path(&data_root);
        if real == real_root {
            return Err(refuse("it is gogo's data dir".into()));
        }
        let inside: [(&str, Option<PathBuf>); 4] = [
            ("the download dir", download_dir.map(Path::to_path_buf)),
            ("gogo's runtime store", Some(super::store::runtimes_dir())),
            ("gogo's renderer store", Some(super::store::renderers_dir())),
            (
                "the depot cache",
                crate::gog::depot::default_cache_dir().map(|c| c.join("gogo/depot")),
            ),
        ];
        for (what, other) in inside {
            if let Some(other) = other
                && real.starts_with(real_path(&other))
            {
                return Err(refuse(format!("it is inside {what} ({})", other.display())));
            }
        }
        // `games/sub` isn't a place a game is looked for, and it wouldn't get
        // a record; `game-locations/` would make a game's dir its own record.
        if real.starts_with(&real_root) {
            return Err(refuse(format!(
                "it is inside gogo's data dir ({})",
                data_root.display()
            )));
        }
        let Some(near) = crate::gog::materialize::existing_ancestor(&dir) else {
            return Err(refuse("no part of the path exists".into()));
        };
        let tag = format!(".gogo-library-probe-{}", std::process::id());
        let (file, link) = (near.join(&tag), near.join(format!("{tag}-link")));
        let probe = (|| {
            std::fs::write(&file, "")?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&tag, &link)?;
            Ok::<(), std::io::Error>(())
        })();
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_file(&file);
        probe.map_err(|e| {
            refuse(format!(
                "{} doesn't take a file and a symlink ({e}): the game's prefix needs both",
                near.display()
            ))
        })?;
        Ok(Some(Library { dir }))
    }
}

/// `p` with its symlinks resolved as far as it exists (a library is compared
/// by where it really is).
pub(crate) fn real_path(p: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut cur = p;
    loop {
        if let Ok(real) = cur.canonicalize() {
            return rest.iter().rev().fold(real, |acc, c| acc.join(c));
        }
        let (Some(parent), Some(name)) = (cur.parent(), cur.file_name()) else {
            return p.to_path_buf();
        };
        rest.push(name);
        cur = parent;
    }
}

/// `games/<gogid>/state.toml`: what gogo last applied, and the store's
/// reference list for GC (design 16 decision 6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameState {
    pub schema: u32,
    pub gogid: String,
    /// The bundle's title, for `gogo apps` once the `.app` is gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The `.app` this state belongs to (updated when the launcher reports a
    /// moved bundle).
    pub bundle: PathBuf,
    pub runtime: String,
    pub renderer: String,
    /// The verbs the last setup applied (#72): winetricks runs again only
    /// when these change or the prefix is new.
    #[serde(default)]
    pub verbs: Vec<String>,
    pub stamp: String,
    pub gogo_version: String,
}

impl GameState {
    pub fn load(path: &Path) -> Result<Option<GameState>, MacAppError> {
        match std::fs::read_to_string(path) {
            Ok(raw) => toml::from_str(&raw)
                .map(Some)
                .map_err(|e| MacAppError::Compat {
                    path: path.to_path_buf(),
                    reason: e.to_string(),
                }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

/// Absolute, `.`-free, symlinks kept (never canonicalized: a profile
/// symlink must survive, decision 2).
pub fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .components()
        .collect()
}

pub(crate) fn write_atomic(path: &Path, contents: &str) -> Result<(), MacAppError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// — runtime references (design 16 decision 6; the GC side is
// `store::references`) —

/// How the bundle names its runtime: `<id>/<ver>` when it lives in the store,
/// else `path:<root>` (brew, a GPTK mount, `--runtime path`), which can't be
/// swapped or GC-tracked but still works.
pub fn runtime_ref(rt: &Runtime) -> String {
    match rt.root.strip_prefix(super::store::runtimes_dir()) {
        Ok(rel) if rel.components().count() == 2 => rel.to_string_lossy().into_owned(),
        _ => format!("path:{}", rt.root.display()),
    }
}

/// Resolve a runtime reference to a runnable runtime (probed).
pub fn resolve_ref(r: &str) -> Result<Runtime, MacAppError> {
    if let Some(dir) = r.strip_prefix("path:") {
        return runtime::resolve(&runtime::RuntimeOptions {
            mode: RuntimeMode::Path,
            runtime_path: Some(PathBuf::from(dir)),
            version: None,
        });
    }
    let (id, ver) = match r.split_once('/') {
        Some((id, ver)) => (id, Some(ver)),
        None => (r, None),
    };
    let (m, _) = super::store::installed(id, ver).ok_or_else(|| MacAppError::RuntimeNotFound {
        tried: format!("runtime `{r}` is not in the store (`gogo runtimes install {id}`)"),
    })?;
    runtime::from_store(&m)
}

/// `wineserver` next to the runtime's wine binary.
pub fn wineserver_of(rt: &Runtime) -> PathBuf {
    rt.wine.with_file_name("wineserver")
}

/// The engine root: the directory holding `bin/wine`.
pub fn engine_root(rt: &Runtime) -> PathBuf {
    rt.wine
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| rt.root.clone())
}

/// The dylib dir the launcher puts on `DYLD_FALLBACK_LIBRARY_PATH`: the store
/// entry's `Frameworks/` or a wrapper's `Contents/Frameworks`.
pub fn frameworks_of(rt: &Runtime) -> Option<PathBuf> {
    runtime::frameworks_for(&rt.wine)
}

/// A path for the state `launch.conf`: relative to the state root when under
/// it (the launcher resolves it against the state dir it finds, so a state
/// dir copied to another account still works), absolute otherwise.
pub(crate) fn state_rel(state_root: &Path, p: &Path) -> String {
    p.strip_prefix(state_root)
        .map(|r| r.to_string_lossy().into_owned())
        .unwrap_or_else(|_| p.to_string_lossy().into_owned())
}

// — the bundle side —

/// Write `gogo-launch.toml` and its mirror into `Contents/Resources`.
pub fn write_bundle_config(resources: &Path, t: &LaunchToml) -> Result<(), MacAppError> {
    std::fs::write(resources.join("gogo-launch.toml"), t.to_toml())?;
    std::fs::write(resources.join("launch.conf"), to_launch_conf(t, &[]))?;
    Ok(())
}

/// The v2 launcher (design 16 §8), plain bash 3.2 (what macOS ships) with no
/// dependencies beyond `/usr/bin/shasum`, `osascript` and, for teardown,
/// `stat`/`lsof`/`ps`.
pub const LAUNCHER: &str = r#"#!/bin/bash
# Generated by `gogo mac-app` (design 16 §8). An immutable launcher: it only
# reads files gogo wrote, never writes inside this bundle, and runs Wine from
# gogo's runtime store against this game's prefix in the state dir. A
# portable bundle (#84, PORTABLE=1) runs the Wine inside it instead, and
# sets the prefix up itself: the Mac it runs on may have no gogo.
set -u
# The environment as started, for "Try next renderer"'s relaunch (#78):
# the exports below must not stack up across relaunches.
ENV0="$(export -p)"
CONTENTS="$(cd "$(dirname "$0")/.." && pwd)"
RES="$CONTENTS/Resources"
# Only a portable bundle's launch.conf sets it, never the environment.
PORTABLE=""
# shellcheck source=/dev/null
source "$RES/launch.conf"
STAMP=""
STATE_ROOT="$GOGO_STATE"
# A portable bundle was built on another Mac: the state dir is this user's.
[ -z "$PORTABLE" ] && [ -d "$STATE_ROOT" ] || STATE_ROOT="$HOME/Library/Application Support/gogo"
GAME_STATE="$STATE_ROOT/games/$GOGID"
# A game in a library (design 18 section 6.1): gogo records where it lives.
if [ -f "$STATE_ROOT/game-locations/$GOGID" ]; then
    IFS= read -r GAME_STATE <"$STATE_ROOT/game-locations/$GOGID"
fi
LOG_DIR="${GOGO_LOG_DIR:-$HOME/Library/Logs/gogo/$GOGID}"
mkdir -p "$LOG_DIR"
LOG="$LOG_DIR/launch.log"

log() { printf '%s [%s] %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$$" "$*" >>"$LOG"; }
abs() { case "$1" in /*) printf '%s' "$1" ;; *) printf '%s/%s' "$STATE_ROOT" "$1" ;; esac; }
dialog() {
    log "dialog: $1"
    [ -n "${GOGO_LAUNCHER_NO_DIALOG:-}" ] && return 0
    /usr/bin/osascript -e 'on run argv' \
        -e 'display alert (item 1 of argv) message (item 2 of argv) as critical' \
        -e 'end run' "$TITLE" "$1" >/dev/null 2>&1 || true
}
load_state() {
    # shellcheck source=/dev/null
    if [ -f "$GAME_STATE/launch.conf" ]; then source "$GAME_STATE/launch.conf"; fi
}
# Design 16 §2.1: sha256 over the bundle TOML, then the L4 file if any (a
# portable bundle baked L4 at build: its stamp is the TOML's alone).
current_stamp() {
    local l4="$STATE_ROOT/compat/$GOGID.toml" sum
    [ -z "$PORTABLE" ] || l4=""
    sum="$( { cat "$RES/gogo-launch.toml"; if [ -n "$l4" ] && [ -f "$l4" ]; then cat "$l4"; fi; } | /usr/bin/shasum -a 256)"
    printf '%s' "${sum%% *}"
}
find_gogo() {
    if [ -n "${GOGO_PATH:-}" ] && [ -x "$GOGO_PATH" ]; then printf '%s' "$GOGO_PATH"; return; fi
    command -v gogo 2>/dev/null || true
}

# #84: a portable bundle's prefix setup, done here because gogo may not be
# on this Mac: `wineboot -u` with the bundled Wine for a new prefix, no
# links out of the state dir (design 16 decision 4), a writable clone of the
# bundled game as games/<gogid>/game (reached as C:\GOG\<name>), and,
# whenever the prefix is new or was set up from another build of this
# bundle, the files the build's verbs installed and the baked registry
# values. games/<gogid>/portable records the stamp it applied.
localize_prefix() {
    local l
    if [ -d "$PREFIX/drive_c/users" ]; then
        find "$PREFIX/drive_c/users" -type l 2>/dev/null | while IFS= read -r l; do
            case "$(readlink "$l")" in /*) rm -f "$l" && mkdir -p "$l" ;; esac
        done
    fi
    for l in "$PREFIX/dosdevices"/*; do
        [ -L "$l" ] || continue
        [ "${l##*/}" = "c:" ] || rm -f "$l"
    done
}
# A command with this prefix and the bundle's dylibs (assignments, not
# /usr/bin/env: SIP would drop DYLD_* there).
pwine() {
    if [ -n "$FRAMEWORKS" ]; then
        WINEPREFIX="$PREFIX" DYLD_FALLBACK_LIBRARY_PATH="$FRAMEWORKS${DYLD_FALLBACK_LIBRARY_PATH:+:$DYLD_FALLBACK_LIBRARY_PATH}" "$@"
    else
        WINEPREFIX="$PREFIX" "$@"
    fi
}
portable_init() {
    local want="$1" mark="$GAME_STATE/portable" have="" fresh="" tree link i n=0
    WINE="$RES/$WINE"
    WINESERVER="$RES/$WINESERVER"
    RUNTIME_DIR="$RES/$RUNTIME_DIR"
    if [ -n "$FRAMEWORKS" ]; then FRAMEWORKS="$RES/$FRAMEWORKS"; fi
    PREFIX="$GAME_STATE/prefix"
    STAMP="$want"
    if [ -f "$mark" ]; then have="$(sed -n 's/^stamp=//p' "$mark")"; fi
    if [ -f "$GAME_STATE/state.toml" ]; then
        log "portable: $GAME_STATE is also set up by gogo; sharing its prefix and game files"
    fi
    mkdir -p "$GAME_STATE" || return 1
    if [ ! -d "$PREFIX" ]; then
        log "portable: creating the prefix $PREFIX with the bundle's Wine"
        mkdir -p "$PREFIX"
        # No wine-mono install (a CX engine loops on it; nobody to click
        # through it elsewhere), as gogo's own builds do.
        if ! WINEDLLOVERRIDES="mscoree=" pwine "$WINE" wineboot -u >>"$LOG" 2>&1; then
            log "portable: wineboot -u failed"
            rm -rf "$PREFIX"
            return 1
        fi
        pwine "$WINESERVER" -w >/dev/null 2>&1
        fresh=1
    fi
    if [ ! -d "$GAME_STATE/game" ]; then
        log "portable: copying the game files to $GAME_STATE/game"
        rm -rf "$GAME_STATE/game.tmp"
        if ! cp -cRp "$RES/$GAME_SRC" "$GAME_STATE/game.tmp" 2>/dev/null; then
            rm -rf "$GAME_STATE/game.tmp"
            cp -Rp "$RES/$GAME_SRC" "$GAME_STATE/game.tmp" || return 1
        fi
        chmod -R u+w "$GAME_STATE/game.tmp"
        mv "$GAME_STATE/game.tmp" "$GAME_STATE/game" || return 1
    fi
    tree="${GAME_SRC##*/}"
    link="$PREFIX/drive_c/GOG/$tree"
    mkdir -p "$PREFIX/drive_c/GOG"
    if [ -L "$link" ] || [ ! -e "$link" ]; then ln -sfn ../../../game "$link"; fi
    if [ -n "$fresh" ] || [ "$have" != "$want" ]; then
        log "portable: applying the bundle's verbs and registry (${have:-none} -> $want)"
        if [ -n "$WINDOWS_SRC" ] && [ -d "$RES/$WINDOWS_SRC" ]; then
            mkdir -p "$PREFIX/drive_c/windows"
            cp -Rpf "$RES/$WINDOWS_SRC/." "$PREFIX/drive_c/windows/" || return 1
            chmod -R u+w "$PREFIX/drive_c/windows"
        fi
        [ -z "${REG[*]+x}" ] || n=${#REG[@]}
        i=0
        while [ "$i" -lt "$n" ]; do
            set -- reg add "${REG[$i]}"
            if [ -n "${REG[$((i + 1))]}" ]; then set -- "$@" /v "${REG[$((i + 1))]}"; else set -- "$@" /ve; fi
            set -- "$@" /t "${REG[$((i + 2))]}" /d "${REG[$((i + 3))]}" /f
            if [ -n "${REG[$((i + 4))]}" ]; then set -- "$@" "/reg:${REG[$((i + 4))]}"; fi
            pwine "$WINE" "$@" >>"$LOG" 2>&1 || log "portable: reg add ${REG[$i]} failed"
            i=$((i + 5))
        done
        pwine "$WINESERVER" -w >/dev/null 2>&1
        printf 'stamp=%s\nbundle=%s\n' "$want" "$(dirname "$CONTENTS")" >"$mark.tmp" && mv "$mark.tmp" "$mark"
    fi
    # Last: wineboot or a reg write may have linked out again.
    localize_prefix
    return 0
}

log "launch $TITLE ($GOGID) from $CONTENTS"
want="$(current_stamp)"
# Why this run isn't on the config asked for (a retarget to a runtime that
# isn't in the store, #69): for the run log's header.
SETUP_ERR=""
if [ -n "$PORTABLE" ]; then
    if ! portable_init "$want"; then
        log "portable: could not set up $GAME_STATE: not launching"
        dialog "This game could not set up its Wine prefix in $GAME_STATE. The details are in $LOG"
        exit 1
    fi
else
    load_state
    if [ ! -d "$GAME_STATE/prefix" ] || [ "$want" != "$STAMP" ]; then
        gogo="$(find_gogo)"
        if [ -n "$gogo" ]; then
            log "prefix missing or config stale (${STAMP:-none} -> $want): $gogo mac-app setup $GOGID"
            setup_out="$("$gogo" mac-app setup "$GOGID" --bundle "$(dirname "$CONTENTS")" 2>&1)"
            setup_rc=$?
            [ -n "$setup_out" ] && printf '%s\n' "$setup_out" >>"$LOG"
            if [ "$setup_rc" -eq 0 ]; then
                load_state
            else
                SETUP_ERR="$(printf '%s' "$setup_out" | grep -v '^warning:' | tail -n 1 | cut -c1-300)"
                SETUP_ERR="setup failed (exit $setup_rc): ${SETUP_ERR:-no output}; running the last applied config"
                log "$SETUP_ERR"
            fi
        else
            log "prefix missing or config stale, and no gogo found (GOGO_PATH=$GOGO_PATH)"
        fi
    fi
    # Design 16 Open 1: with no prefix, Wine would create a default one (home
    # symlinks, z:), so stop and say why instead.
    if [ ! -d "$GAME_STATE/prefix" ] || [ ! -f "$GAME_STATE/launch.conf" ]; then
        log "no prefix at $GAME_STATE/prefix: not launching"
        dialog "This game's Wine prefix is missing ($GAME_STATE/prefix) and gogo could not set it up. Install gogo and run: gogo mac-app setup $GOGID"
        exit 1
    fi
fi

export WINEPREFIX
WINEPREFIX="$(abs "$PREFIX")"
WINE_BIN="$(abs "$WINE")"
WINESERVER_BIN="$(abs "$WINESERVER")"
if [ -n "$FRAMEWORKS" ]; then
    export DYLD_FALLBACK_LIBRARY_PATH
    DYLD_FALLBACK_LIBRARY_PATH="$(abs "$FRAMEWORKS")${DYLD_FALLBACK_LIBRARY_PATH:+:$DYLD_FALLBACK_LIBRARY_PATH}"
fi
if [ -n "$DLL_OVERRIDES" ]; then
    export WINEDLLOVERRIDES="$DLL_OVERRIDES${WINEDLLOVERRIDES:+;$WINEDLLOVERRIDES}"
fi
for pair in ${ENV[@]+"${ENV[@]}"}; do export "${pair?}"; done
# C:\a\b → $WINEPREFIX/drive_c/a/b (old games resolve data files from here).
wd="${WORKDIR#?:\\}"
cd "$WINEPREFIX/drive_c/${wd//\\//}" || { log "workdir $WORKDIR missing"; dialog "Game folder $WORKDIR is missing."; exit 1; }

# Teardown (#42): this prefix's wineserver only. `wineserver -k` ends its
# clients; a server that doesn't answer is SIGKILLed (found by its cwd,
# /tmp/.wine-<uid>/server-<dev>-<ino> of this prefix), and whatever is left
# of the game's own process group is SIGKILLed too.
WPID=""
TORN=""
prefix_server_pid() {
    [ -x /usr/sbin/lsof ] && [ "$(uname)" = Darwin ] || return 0
    local di dir
    di="$(/usr/bin/stat -f '%d %i' "$WINEPREFIX" 2>/dev/null)" || return 0
    # shellcheck disable=SC2086
    dir="$(printf '/.wine-%s/server-%x-%x' "$(id -u)" $di)"
    /usr/sbin/lsof -a -d cwd -c wineserver -Fpn 2>/dev/null | awk -v d="$dir" '
        /^p/ { pid = substr($0, 2) }
        /^n/ { n = substr($0, 2); if (substr(n, length(n) - length(d) + 1) == d) print pid }'
}
teardown() {
    [ -n "$TORN" ] && return
    TORN=1
    if [ -n "$WPID" ]; then kill -TERM -- "-$WPID" 2>/dev/null; fi
    local k left other
    other="$(other_launcher)"
    if [ -n "$other" ]; then
        # A later launch of this game shares the prefix (#103): its game
        # needs the wineserver, so only this run's process group goes.
        log "launcher $other of this game is running: leaving the prefix's wineserver"
        sleep 1
    else
        "$WINESERVER_BIN" -k >/dev/null 2>&1 &
        k=$!
        for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
            kill -0 "$k" 2>/dev/null || break
            sleep 0.25
        done
        kill -KILL "$k" 2>/dev/null
        left="$(prefix_server_pid)"
        if [ -n "$left" ]; then
            log "reaped: wineserver $left did not exit after -k, SIGKILL"
            # shellcheck disable=SC2086
            kill -KILL $left 2>/dev/null
        fi
    fi
    if [ -n "$WPID" ]; then
        left="$(ps -A -o pid=,pgid=,command= | awk -v g="$WPID" '$2 == g { print }')"
        if [ -n "$left" ]; then
            log "reaped (SIGKILL): $(printf '%s' "$left" | tr '\n' ';')"
            kill -KILL -- "-$WPID" 2>/dev/null
        fi
    fi
    log "teardown done"
}

# #103: a launcher killed untrapped (SIGKILL, Force Quit) can't tear down,
# so a watchdog in its own process group does it: it polls the launcher
# every 2s and, once it's gone, runs the same teardown, ends the tees and
# writes the run log's footer. The launcher stops it before any exit of its
# own (and before #78's relaunch, which keeps the pid). Each running
# launcher leaves <state>/games/<gogid>/.launchers/<pid> (its start time),
# so a teardown spares the wineserver of a later launch of the same game.
WATCHDOG=""
ME_START=""
LIVE_DIR="$GAME_STATE/.launchers"
proc_start() { ps -o lstart= -p "$1" 2>/dev/null; }
# $1 is alive and is the process that started at $2: not a zombie, not a
# reused pid.
alive_as() {
    case "$(ps -o stat= -p "$1" 2>/dev/null | tr -d ' ')" in "" | Z*) return 1 ;; esac
    [ "$(proc_start "$1")" = "$2" ]
}
# The pid of another running launcher of this game, if any.
other_launcher() {
    local f pid start
    for f in "$LIVE_DIR"/*; do
        [ -f "$f" ] || continue
        pid="${f##*/}"
        start="$(cat "$f" 2>/dev/null)"
        [ "$pid" = "$$" ] && [ "$start" = "$ME_START" ] && continue
        if alive_as "$pid" "$start"; then printf '%s' "$pid"; return; fi
        rm -f "$f"
    done
}
register_launch() {
    ME_START="$(proc_start $$)"
    mkdir -p "$LIVE_DIR" && printf '%s' "$ME_START" >"$LIVE_DIR/$$"
}
unregister_launch() {
    if [ "$(cat "$LIVE_DIR/$$" 2>/dev/null)" = "$ME_START" ]; then rm -f "$LIVE_DIR/$$"; fi
}
start_watchdog() {
    set -m
    (
        trap '' INT HUP
        while alive_as "$$" "$ME_START"; do sleep 2; done
        log "watchdog: launcher $$ is gone (SIGKILL or Force Quit): tearing down"
        teardown
        # The tees see EOF once Wine's last writer is gone; they ignore
        # TERM, so stragglers get KILL.
        for _ in 1 2 3 4 5 6 7 8; do
            # shellcheck disable=SC2086
            kill -0 $TEE_PIDS 2>/dev/null || break
            sleep 0.25
        done
        # shellcheck disable=SC2086
        if [ -n "$TEE_PIDS" ]; then kill -KILL $TEE_PIDS 2>/dev/null; fi
        unregister_launch
        if [ -n "$RUN_LOG" ]; then run_footer "$RUN_LOG" 137 "launcher killed" $((SECONDS - RUN_T0)); fi
        log "watchdog done"
    ) </dev/null >/dev/null 2>&1 3>&- 4>&- &
    WATCHDOG=$!
    set +m
    log "guard armed ($WATCHDOG)"
}
stop_watchdog() {
    [ -n "$WATCHDOG" ] || return 0
    kill -- "-$WATCHDOG" 2>/dev/null
    wait "$WATCHDOG" 2>/dev/null
    WATCHDOG=""
}
on_signal() {
    log "got SIG$1"
    stop_watchdog
    teardown
    unregister_launch
    end_run_log "$2" "SIG$1"
    exit "$2"
}
trap 'on_signal INT 130' INT
trap 'on_signal TERM 143' TERM
trap 'on_signal HUP 129' HUP

# #77: one log per run, <LOG_DIR>/<YYYYmmdd-HHMMSS>-<pid>.log (the last
# KEEP_RUNS kept, latest.log -> the newest): a header naming the effective
# config, Wine's stdout and stderr (teed, so a terminal still sees them),
# then exit code and duration. launch.log stays the launcher's own journal.
KEEP_RUNS=10
RUN_LOG=""
LAST_RUN_LOG=""
RUN_T0=0
RUN_SECS=0
TEE_PIDS=""
macos_version() {
    if [ -x /usr/bin/sw_vers ]; then
        printf '%s (%s) %s' "$(/usr/bin/sw_vers -productVersion)" "$(/usr/bin/sw_vers -buildVersion)" "$(uname -m)"
    else
        uname -srm
    fi
}
prune_runs() {
    local runs=("$LOG_DIR"/[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]-*.log) i=0
    [ -e "${runs[0]}" ] || return 0
    while [ $((${#runs[@]} - i)) -gt "$KEEP_RUNS" ]; do
        rm -f "${runs[$i]}"
        i=$((i + 1))
    done
}
start_run_log() {
    RUN_LOG="$LOG_DIR/$(date '+%Y%m%d-%H%M%S')-$$.log"
    RUN_T0=$SECONDS
    {
        printf '# gogo run log: %s (%s)\n' "$TITLE" "$GOGID"
        printf '# started: %s\n' "$(date '+%Y-%m-%d %H:%M:%S %z')"
        if [ -n "$SETUP_ERR" ]; then printf '# setup: %s\n' "$SETUP_ERR"; fi
        printf '# runtime: %s\n' "$RUNTIME"
        printf '# runtime_dir: %s\n' "${RUNTIME_DIR:-}"
        printf '# renderer: %s\n' "$RENDERER"
        printf '# verbs: %s\n' "${VERBS[*]+${VERBS[*]}}"
        printf '# stamp: %s\n' "$STAMP"
        printf '# macos: %s\n' "$(macos_version)"
        printf '# exe: %s\n' "$EXE"
        printf '# args: %s\n' "${ARGS[*]+${ARGS[*]}}${*:+ $*}"
        printf '# wine: %s\n' "$WINE_BIN"
        printf '# prefix: %s\n' "$WINEPREFIX"
        printf '# ---\n'
    } >"$RUN_LOG"
    ln -sfn "$(basename "$RUN_LOG")" "$LOG_DIR/latest.log"
    prune_runs
    # tee through FIFOs (not process substitution) so the launcher can wait
    # for them to drain before it writes the footer. They ignore the signals
    # the launcher traps, so they outlive a Ctrl-C long enough to flush.
    local fifo="$LOG_DIR/.run-$$"
    rm -f "$fifo.out" "$fifo.err"
    exec 3>&1 4>&2
    if mkfifo "$fifo.out" "$fifo.err" 2>/dev/null; then
        (trap '' INT TERM HUP; exec tee -a "$RUN_LOG") <"$fifo.out" &
        TEE_PIDS="$!"
        (trap '' INT TERM HUP; exec tee -a "$RUN_LOG" >&2) <"$fifo.err" &
        TEE_PIDS="$TEE_PIDS $!"
        exec >"$fifo.out" 2>"$fifo.err"
        rm -f "$fifo.out" "$fifo.err"
    else
        exec >>"$RUN_LOG" 2>&1
    fi
}
# Footer: exit code (and the signal, if one ended the run) and duration.
end_run_log() {
    [ -n "$RUN_LOG" ] || return 0
    local run="$RUN_LOG"
    RUN_LOG=""
    LAST_RUN_LOG="$run"
    RUN_SECS=$((SECONDS - RUN_T0))
    exec >&3 2>&4
    if [ -n "$TEE_PIDS" ]; then
        # A straggler still holding Wine's output open must not hold the
        # launcher: give the tees 2s to see EOF (KILL: they ignore TERM).
        # shellcheck disable=SC2086
        ( sleep 2; kill -KILL $TEE_PIDS ) >/dev/null 2>&1 &
        local dog=$!
        # shellcheck disable=SC2086
        wait $TEE_PIDS 2>/dev/null
        kill "$dog" 2>/dev/null
    fi
    run_footer "$run" "$1" "${2:-}" "$RUN_SECS"
}
# run_footer <run log> <exit code> <reason> <secs>
run_footer() {
    {
        printf '# ---\n'
        printf '# exit: %s%s\n' "$2" "${3:+ ($3)}"
        printf '# duration: %ss\n' "$4"
        printf '# ended: %s\n' "$(date '+%Y-%m-%d %H:%M:%S %z')"
    } >>"$1"
}

# #78: a run that ends within EARLY_EXIT_SECS or with a non-zero code
# explains itself: the last error lines of its run log and three buttons.
# GOGO_LAUNCHER_DIALOG=<cmd> replaces osascript (tests): it gets the title,
# the message and the button labels, and prints the chosen label.
EARLY_EXIT_SECS="${GOGO_EARLY_EXIT_SECS:-15}"
ask() {
    if [ -n "${GOGO_LAUNCHER_DIALOG:-}" ]; then "$GOGO_LAUNCHER_DIALOG" "$TITLE" "$@"; return; fi
    [ -n "${GOGO_LAUNCHER_NO_DIALOG:-}" ] && return 0
    /usr/bin/osascript -e 'on run argv' -e 'set b to items 3 thru -1 of argv' \
        -e 'set r to display dialog (item 2 of argv) with title (item 1 of argv) buttons b default button (item -1 of b) with icon caution giving up after 600' \
        -e 'if gave up of r then return ""' -e 'return button returned of r' -e 'end run' \
        "$TITLE" "$@" 2>/dev/null
}
# Wine's output in a run log: between the header's and the footer's `# ---`.
run_output() { awk '/^# ---$/ { n++; next } n == 1' "$1"; }
last_errors() {
    local errs
    errs="$(run_output "$1" | grep -E -i 'err:|error|fail|exception|cannot|not found' | tail -n 5)"
    [ -n "$errs" ] || errs="$(run_output "$1" | tail -n 5)"
    printf '%s' "$errs" | cut -c1-160
}
# Copy report: what a bug report needs, no home paths.
report() {
    printf 'gogo launch report: %s (%s)\n' "$TITLE" "$GOGID"
    grep -E '^# (setup|runtime|runtime_dir|renderer|verbs|stamp|macos|exe|args|exit|duration):' "$1"
    printf -- '--- last 40 log lines ---\n'
    run_output "$1" | tail -n 40
}
early_exit() {
    local log="$LAST_RUN_LOG" gogo next="" msg choice
    if [ -z "${GOGO_LAUNCHER_DIALOG:-}" ] && [ -n "${GOGO_LAUNCHER_NO_DIALOG:-}" ]; then
        log "early exit: code $rc after ${RUN_SECS}s on $RENDERER (dialogs off)"
        return 0
    fi
    gogo="$(find_gogo)"
    # A portable bundle's renderer is inside it: a rebuild picks another.
    if [ -n "$gogo" ] && [ -z "$PORTABLE" ]; then
        next="$("$gogo" mac-app next-renderer "$GOGID" --dry-run 2>>"$LOG")" || next=""
    fi
    log "early exit: code $rc after ${RUN_SECS}s on $RENDERER; next renderer: ${next:-none}"
    msg="$TITLE quit after ${RUN_SECS}s (exit code $rc) on the $RENDERER renderer.

$(last_errors "$log")

"
    if [ -n "$next" ]; then
        choice="$(ask "${msg}Try next renderer switches to $next and starts the game again." \
            "Copy report" "Open log" "Try next renderer")"
    elif [ -n "$PORTABLE" ]; then
        choice="$(ask "${msg}This is a portable bundle: its renderer is built in. Another one needs a rebuild (gogo mac-app --portable --renderer ...)." \
            "Copy report" "Open log" "Close")"
    else
        choice="$(ask "${msg}There are no more renderers to try for this game." \
            "Copy report" "Open log" "Close")"
    fi
    log "dialog: ${choice:-dismissed}"
    case "$choice" in
        "Open log") open "$log" ;;
        "Copy report") report "$log" | pbcopy ;;
        "Try next renderer")
            if "$gogo" mac-app next-renderer "$GOGID" >>"$LOG" 2>&1; then
                log "relaunching on $next"
                # The stamp is stale now: the relaunch runs setup, which
                # applies the new renderer.
                exec /usr/bin/env -i /bin/bash -c 'eval "$1"; shift; exec "$@"' relaunch \
                    "$ENV0" "$CONTENTS/MacOS/$(basename "$0")" "$@"
            fi
            log "next-renderer failed: not relaunching"
            ;;
    esac
}

start_run_log "$@"
# The runtime the last setup applied must still be in the store (#69: a
# prune, or a retarget whose setup failed, can leave it missing).
if [ ! -x "$WINE_BIN" ] && [ -n "$PORTABLE" ]; then
    printf 'gogo: the Wine inside this bundle is missing (%s): copy the bundle again\n' "$WINE_BIN" >&2
    log "bundled Wine $WINE_BIN is missing: not launching"
    end_run_log 127 "runtime missing"
    dialog "The Wine inside this bundle is missing ($WINE_BIN). The bundle is damaged: copy it again."
    exit 1
fi
if [ ! -x "$WINE_BIN" ]; then
    msg="runtime $RUNTIME is not in the runtime store ($WINE_BIN is missing)"
    printf 'gogo: %s; reinstall it with `gogo runtimes install %s` or pick another with `gogo mac-app --retarget %s --runtime …`\n' \
        "$msg" "${RUNTIME%%/*}" "$GOGID" >&2
    log "$msg: not launching"
    end_run_log 127 "runtime missing"
    dialog "This game's Wine runtime ($RUNTIME) is not installed. Run: gogo runtimes install ${RUNTIME%%/*}"
    exit 1
fi
log "exec $WINE_BIN $EXE (runtime $RUNTIME, prefix $WINEPREFIX, log $RUN_LOG)"
# Registered before the game starts: a launcher killed in the gap would leave
# a game nothing knows about. Only the watchdog needs WPID, so it follows.
register_launch
set -m
"$WINE_BIN" "$EXE" ${ARGS[@]+"${ARGS[@]}"} "$@" 3>&- 4>&- &
WPID=$!
set +m
start_watchdog
wait "$WPID"
rc=$?
log "wine exited $rc; waiting for the prefix's wineserver"
# A launcher exe may have started the real game and exited: wait for the
# whole prefix to finish, not just the first process.
"$WINESERVER_BIN" -w >/dev/null 2>&1 &
wait $!
log "prefix idle"
stop_watchdog
unregister_launch
end_run_log "$rc"
if [ "$rc" -ne 0 ] || [ "$RUN_SECS" -lt "$EARLY_EXIT_SECS" ]; then early_exit "$@"; fi
exit "$rc"
"#;

/// `Contents/MacOS/<display>`.
pub fn write_launcher(macos: &Path, display: &str) -> Result<PathBuf, MacAppError> {
    let path = macos.join(display);
    let mut file = std::fs::File::create(&path)?;
    file.write_all(LAUNCHER.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(path)
}

#[cfg(all(test, target_os = "macos"))]
mod launch_tests {
    use super::*;
    use crate::macapp::compat::{Layer, Os};

    fn sample() -> LaunchToml {
        LaunchToml {
            schema: 1,
            gogid: "1146738698".into(),
            title: "Carmageddon TDR 2000".into(),
            runtime: "winecx/24.0.7_7".into(),
            renderer: "wined3d".into(),
            exe: r"C:\GOG\Carmageddon TDR 2000\TDR2000.exe".into(),
            workdir: r"C:\GOG\Carmageddon TDR 2000".into(),
            args: vec!["-f".into()],
            env: [("WINEMSYNC".to_string(), "1".to_string())].into(),
            dll_overrides: [("ddraw".to_string(), "n,b".to_string())].into(),
            verbs: vec![],
            gogo: GogoRef {
                path: "/Users/me/.nix-profile/bin/gogo".into(),
                state: "/Users/me/Library/Application Support/gogo".into(),
            },
            variants: vec![],
            portable: None,
            lane: None,
        }
    }

    /// `sample()` with TDR-like `[[variant]]`s (#85).
    fn with_variants() -> LaunchToml {
        let dir = r"C:\GOG\Carmageddon TDR 2000";
        let v = |name: &str, exe: &str, args: &[&str]| Variant {
            name: name.into(),
            exe: format!(r"{dir}\{exe}"),
            workdir: dir.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
        };
        LaunchToml {
            variants: vec![
                v("Carmageddon TDR 2000", "TDR2000.exe", &["-f"]),
                v("Carmageddon TDR 2000 (safe mode)", "TDR2000.exe", &["-s"]),
                v("Settings", "Setup.exe", &[]),
            ],
            ..sample()
        }
    }

    #[test]
    fn variants_round_trip_and_stay_out_when_empty() {
        let t = with_variants();
        let raw = t.to_toml();
        assert!(raw.contains("[[variant]]"), "{raw}");
        assert_eq!(LaunchToml::parse(&raw, Path::new("x")).unwrap(), t);
        assert!(!sample().to_toml().contains("variant"));
        let bad = format!("{raw}bogus = 1\n");
        assert!(LaunchToml::parse(&bad, Path::new("x")).is_err());
    }

    #[test]
    fn variant_switches_exe_and_args() {
        let t = with_variants();
        assert_eq!(t.default_variant().unwrap().name, "Carmageddon TDR 2000");
        let none = CompatEntry::default();
        assert_eq!(t.effective(&none, &none).unwrap(), t);

        let l4 = compat::parse("variant = \"Settings\"\n", Os::Macos, Layer::User)
            .unwrap()
            .entry;
        let eff = t.effective(&none, &l4).unwrap();
        assert_eq!(eff.exe, r"C:\GOG\Carmageddon TDR 2000\Setup.exe");
        assert_eq!(eff.workdir, r"C:\GOG\Carmageddon TDR 2000");
        assert!(eff.args.is_empty());

        // L4 over L3; an L4 `args` beats the variant's.
        let l3 = compat::parse("variant = \"Settings\"\n", Os::Macos, Layer::Db)
            .unwrap()
            .entry;
        let l4 = compat::parse(
            "variant = \"Carmageddon TDR 2000 (safe mode)\"\nargs = [\"-s\", \"-w\"]\n",
            Os::Macos,
            Layer::User,
        )
        .unwrap()
        .entry;
        let eff = t.effective(&l3, &l4).unwrap();
        assert_eq!(eff.exe, r"C:\GOG\Carmageddon TDR 2000\TDR2000.exe");
        assert_eq!(eff.args, ["-s", "-w"]);
    }

    #[test]
    fn unknown_variant_fails_and_lists_the_names() {
        let l4 = compat::parse("variant = \"Turbo\"\n", Os::Macos, Layer::User)
            .unwrap()
            .entry;
        let none = CompatEntry::default();
        let e = with_variants().effective(&none, &l4).unwrap_err();
        assert_eq!(
            e,
            "unknown variant \"Turbo\"; available: \"Carmageddon TDR 2000\", \
             \"Carmageddon TDR 2000 (safe mode)\", \"Settings\""
        );
        let e = sample().effective(&none, &l4).unwrap_err();
        assert!(e.contains("has no variants"), "{e}");
    }

    /// #84: `[portable]` round-trips, stays out of a v2 bundle's TOML, and
    /// its mirror sources in bash with `REG` five words per value.
    #[test]
    fn portable_table_round_trips_and_mirrors() {
        let mut t = sample();
        t.portable = Some(Portable {
            wine: "wine/bin/wine".into(),
            frameworks: Some("wine-frameworks".into()),
            game: "game/Carmageddon TDR 2000".into(),
            windows: None,
            registry: vec![
                RegValue {
                    key: r"HKLM\Software\GOG.com\Games\1".into(),
                    name: Some("PATH".into()),
                    kind: "REG_SZ".into(),
                    data: r"C:\GOG\It's".into(),
                    wow32: true,
                },
                RegValue {
                    key: r"HKCU\Software\Game".into(),
                    name: None,
                    kind: "REG_SZ".into(),
                    data: "x".into(),
                    wow32: false,
                },
            ],
        });
        let raw = t.to_toml();
        assert!(
            raw.contains("[portable]") && raw.contains("[[portable.registry]]"),
            "{raw}"
        );
        assert_eq!(LaunchToml::parse(&raw, Path::new("x")).unwrap(), t);
        assert!(!sample().to_toml().contains("portable"));
        assert!(!to_launch_conf(&sample(), &[]).contains("PORTABLE"));

        let tmp = tempfile::tempdir().unwrap();
        let conf = tmp.path().join("launch.conf");
        std::fs::write(&conf, to_launch_conf(&t, &[])).unwrap();
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(
                r#"source "$1"; printf '%s|' "$PORTABLE" "$WINE" "$WINESERVER" "$RUNTIME_DIR" \
                   "$FRAMEWORKS" "$GAME_SRC" "$WINDOWS_SRC" "${#REG[@]}" "${REG[3]}" "${REG[4]}" \
                   "${REG[6]}""#,
            )
            .arg("x")
            .arg(&conf)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            r"1|wine/bin/wine|wine/bin/wineserver|wine|wine-frameworks|game/Carmageddon TDR 2000||10|C:\GOG\It's|32||"
        );
    }

    #[test]
    fn toml_round_trips_and_rejects_unknown_keys() {
        let t = sample();
        let raw = t.to_toml();
        assert_eq!(LaunchToml::parse(&raw, Path::new("x")).unwrap(), t);
        let extra = format!("bogus = 1\n{raw}");
        assert!(LaunchToml::parse(&extra, Path::new("x")).is_err());
    }

    #[test]
    fn newer_schema_is_refused() {
        let raw = sample().to_toml().replace("schema = 1", "schema = 2");
        let e = LaunchToml::parse(&raw, Path::new("x")).unwrap_err();
        assert!(e.to_string().contains("upgrade gogo"), "{e}");
    }

    /// Golden projection (design 16 §9).
    #[test]
    fn launch_conf_golden() {
        let mut t = sample();
        t.title = "It's a game".into();
        let conf = to_launch_conf(&t, &[("STAMP", "abc".into())]);
        assert_eq!(
            conf,
            "# Generated by gogo from gogo-launch.toml. Do not edit.\n\
             SCHEMA=1\n\
             GOGID='1146738698'\n\
             TITLE='It'\\''s a game'\n\
             RUNTIME='winecx/24.0.7_7'\n\
             RENDERER='wined3d'\n\
             EXE='C:\\GOG\\Carmageddon TDR 2000\\TDR2000.exe'\n\
             WORKDIR='C:\\GOG\\Carmageddon TDR 2000'\n\
             ARGS=('-f')\n\
             ENV=('WINEMSYNC=1')\n\
             DLL_OVERRIDES='ddraw=n,b'\n\
             VERBS=()\n\
             GOGO_PATH='/Users/me/.nix-profile/bin/gogo'\n\
             GOGO_STATE='/Users/me/Library/Application Support/gogo'\n\
             STAMP='abc'\n"
        );
    }

    /// The conf is plain bash: sourcing it yields the values back.
    #[test]
    fn launch_conf_sources_in_bash() {
        let tmp = tempfile::tempdir().unwrap();
        let mut t = sample();
        t.args = vec!["a b".into(), "it's".into()];
        let conf = tmp.path().join("launch.conf");
        std::fs::write(&conf, to_launch_conf(&t, &[])).unwrap();
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(r#"source "$1"; printf '%s|' "$TITLE" "${#ARGS[@]}" "${ARGS[1]}" "$EXE""#)
            .arg("x")
            .arg(&conf)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            r"Carmageddon TDR 2000|2|it's|C:\GOG\Carmageddon TDR 2000\TDR2000.exe|"
        );
    }

    #[test]
    fn stamp_matches_shasum() {
        let toml = b"schema = 1\n";
        let l4 = b"renderer = \"dxmt\"\n";
        let ours = stamp(toml, Some(l4));
        // Reference: the exact pipeline the launcher runs.
        let shasum = Path::new("/usr/bin/shasum");
        if shasum.exists() {
            let tmp = tempfile::tempdir().unwrap();
            std::fs::write(tmp.path().join("a"), toml).unwrap();
            std::fs::write(tmp.path().join("b"), l4).unwrap();
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg("cat a b | /usr/bin/shasum -a 256")
                .current_dir(tmp.path())
                .output()
                .unwrap();
            let s = String::from_utf8_lossy(&out.stdout);
            assert_eq!(s.split_whitespace().next().unwrap(), ours);
        }
        assert_ne!(ours, stamp(toml, None));
        assert_eq!(stamp(toml, None), stamp(toml, Some(b"")));
    }

    #[test]
    fn override_applies_field_by_field() {
        let t = sample();
        let l4 = compat::parse(
            "exe = \"bin/Other.exe\"\n[platforms.macos]\nruntime = \"wine-staging/11.18\"\n\
             env = { DXMT_HUD = \"1\" }\n",
            Os::Macos,
            Layer::User,
        )
        .unwrap()
        .entry;
        let eff = t.with_override(&l4);
        assert_eq!(eff.runtime, "wine-staging/11.18");
        assert_eq!(eff.renderer, "wined3d");
        assert_eq!(eff.exe, r"C:\GOG\Carmageddon TDR 2000\bin\Other.exe");
        assert_eq!(eff.workdir, r"C:\GOG\Carmageddon TDR 2000\bin");
        assert_eq!(eff.env.len(), 2);
        assert_eq!(eff.dll_overrides["ddraw"], "n,b");
        assert_eq!(t.with_override(&CompatEntry::default()), t);
    }

    #[test]
    fn windows_to_unix_inverts() {
        let p = Path::new("/s/prefix");
        let u = windows_to_unix(p, r"C:\GOG\Neo Cab\Neo Cab.exe").unwrap();
        assert_eq!(u, Path::new("/s/prefix/drive_c/GOG/Neo Cab/Neo Cab.exe"));
        assert_eq!(
            unix_to_windows(p, &u).unwrap(),
            r"C:\GOG\Neo Cab\Neo Cab.exe"
        );
        assert!(windows_to_unix(p, r"D:\x.exe").is_none());
    }

    #[test]
    fn windows_paths() {
        let t = sample();
        assert_eq!(t.game_dir(), r"C:\GOG\Carmageddon TDR 2000");
        assert_eq!(t.tree_name().as_deref(), Some("Carmageddon TDR 2000"));
        let prefix = Path::new("/s/prefix");
        assert_eq!(
            unix_to_windows(prefix, Path::new("/s/prefix/drive_c/GOG/x/y.exe")).as_deref(),
            Some(r"C:\GOG\x\y.exe")
        );
        assert_eq!(unix_to_windows(prefix, Path::new("/elsewhere/y.exe")), None);
    }

    #[test]
    fn runtime_refs() {
        let _g = crate::test_util::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: env mutation under the shared env mutex.
        unsafe { std::env::set_var("GOGO_HOME", tmp.path()) };
        let mk = |root: PathBuf| Runtime {
            mode: RuntimeMode::Catalog("wine-staging".into()),
            kind: "catalog",
            wine: root.join("W.app/Contents/Resources/wine/bin/wine"),
            root,
            version: "wine-11.18".into(),
            min_macos: None,
        };
        let store = mk(tmp.path().join("runtimes/wine-staging/11.18"));
        assert_eq!(runtime_ref(&store), "wine-staging/11.18");
        assert_eq!(
            engine_root(&store),
            tmp.path()
                .join("runtimes/wine-staging/11.18/W.app/Contents/Resources/wine")
        );
        assert_eq!(
            wineserver_of(&store),
            store.wine.with_file_name("wineserver")
        );
        let ext = mk(PathBuf::from("/opt/wine"));
        assert_eq!(runtime_ref(&ext), "path:/opt/wine");
        assert!(matches!(
            resolve_ref("wine-staging/11.18"),
            Err(MacAppError::RuntimeNotFound { .. })
        ));
        unsafe { std::env::remove_var("GOGO_HOME") };
    }

    #[test]
    fn launcher_is_bash_32_clean() {
        // No bash-4 features: macOS ships bash 3.2.
        for bad in [
            "declare -A",
            "${!",
            ",,}",
            "^^}",
            "mapfile",
            "readarray",
            "|&",
        ] {
            assert!(!LAUNCHER.contains(bad), "{bad}");
        }
        let out = std::process::Command::new("bash")
            .args(["-n", "-c", LAUNCHER])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // — the game library (design 18 §6.1) —

    #[test]
    fn a_library_is_refused_inside_what_gogo_cleans_or_walks() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = crate::test_util::env_lock();
        // SAFETY: env mutation under the shared env mutex.
        unsafe { std::env::set_var("GOGO_HOME", tmp.path().join("home")) };
        let downloads = tmp.path().join("Downloads/gogo");
        std::fs::create_dir_all(&downloads).unwrap();
        let refused = |dir: &Path, needle: &str| {
            let err = Library::resolve(dir, Some(&downloads)).unwrap_err();
            let msg = err.to_string();
            assert!(msg.contains(needle), "{dir:?}: {msg}");
        };
        refused(&downloads, "the download dir");
        refused(&downloads.join("games"), "the download dir");
        let store = crate::state::data_root().join("runtimes");
        refused(&store.join("x"), "runtime store");
        let renderers = crate::state::data_root().join("renderers");
        refused(&renderers, "renderer store");
        refused(&crate::state::data_root(), "data dir");
        let root = crate::state::data_root();
        for inside in ["games/sub", "game-locations", "compat", "logs", "other/x"] {
            refused(&root.join(inside), "inside gogo's data dir");
        }
        if let Some(cache) = crate::gog::depot::default_cache_dir() {
            refused(&cache.join("gogo/depot/1"), "depot cache");
        }
        // Reached through a symlink, it's still the download dir.
        let link = tmp.path().join("looks-fine");
        std::os::unix::fs::symlink(&downloads, &link).unwrap();
        refused(&link.join("games"), "the download dir");
        refused(Path::new("/tmp\nsecond"), "one line");

        // games/ itself is no library at all.
        let games = crate::state::data_root().join("games");
        assert_eq!(Library::resolve(&games, Some(&downloads)).unwrap(), None);

        // A fine one, not created yet: nothing is left behind by the check.
        let big = tmp.path().join("big/GOG");
        let lib = Library::resolve(&big, Some(&downloads)).unwrap().unwrap();
        assert_eq!(lib.dir(), big);
        assert!(!tmp.path().join("big").exists());
        assert_eq!(
            std::fs::read_dir(tmp.path())
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().contains("probe"))
                .count(),
            0
        );

        // `..` never reaches a location record, which wouldn't be believed.
        std::fs::create_dir_all(tmp.path().join("real")).unwrap();
        let lib = Library::resolve(&tmp.path().join("real/../big"), None)
            .unwrap()
            .unwrap();
        assert_eq!(lib.dir(), tmp.path().canonicalize().unwrap().join("big"));
        refused(&tmp.path().join("no-such/../big"), "`..`");
        record_location("7", &lib.dir().join("7")).unwrap();
        assert_eq!(recorded_location("7"), Some(lib.dir().join("7")));
        forget_location("7").unwrap();

        // SAFETY: as above.
        unsafe { std::env::remove_var("GOGO_HOME") };
    }

    #[test]
    fn a_library_gogo_cannot_write_to_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let locked = tmp.path().join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        // root writes anywhere: nothing to test there.
        if std::fs::write(locked.join("probe"), "").is_ok() {
            return;
        }
        let err = Library::resolve(&locked.join("games"), None).unwrap_err();
        assert!(err.to_string().contains("doesn't take"), "{err}");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The Mac launcher's top part, run: `GAME_STATE` from the record when
    /// there is one, `games/<gogid>` otherwise (bash, on either OS).
    #[test]
    fn the_mac_launcher_finds_a_library_game_by_its_record() {
        let tmp = tempfile::tempdir().unwrap();
        let contents = tmp.path().join("Game.app/Contents");
        let (res, macos) = (contents.join("Resources"), contents.join("MacOS"));
        std::fs::create_dir_all(&res).unwrap();
        std::fs::create_dir_all(&macos).unwrap();
        let state = tmp.path().join("state");
        std::fs::create_dir_all(state.join("game-locations")).unwrap();
        std::fs::write(
            res.join("launch.conf"),
            format!(
                "GOGID='7'\nTITLE='T'\nGOGO_STATE={}\n",
                sh_quote(&state.display().to_string())
            ),
        )
        .unwrap();
        let top: String = LAUNCHER
            .lines()
            .take_while(|l| !l.starts_with("LOG_DIR="))
            .map(|l| format!("{l}\n"))
            .collect();
        let script = macos.join("launcher");
        std::fs::write(&script, format!("{top}printf '%s' \"$GAME_STATE\"\n")).unwrap();
        let run = || {
            let out = std::process::Command::new("bash")
                .arg(&script)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
            String::from_utf8(out.stdout).unwrap()
        };
        assert_eq!(run(), state.join("games/7").display().to_string());
        std::fs::write(state.join("game-locations/7"), "/Volumes/Big Drive/7\n").unwrap();
        assert_eq!(run(), "/Volumes/Big Drive/7");
    }
}
