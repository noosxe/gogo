//! The saves a rollback would drop (design 20 §6): the files under the save
//! roots that are newer than the snapshot, and the copy that sets them aside
//! before the rollback.
//!
//! The save roots are a prefix's user dirs (`drive_c/users/<user>/Documents`,
//! `Saved Games`, `AppData/{Roaming,Local,LocalLow}`), `drive_c/ProgramData`,
//! and the game's own files (`game/` on the depot lane, the install dir on the
//! installer lane), where the files the build itself shipped are left out by
//! the build's file list ([`BuildFiles`]). Paths are reported relative to
//! `drive_c/`, the game's files under their own name (`game/…`), and set aside
//! with the same layout.

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;

use crate::gog::materialize::{self, Reflink};

/// Caches and logs a game, Wine or a driver rewrites at every start, which
/// would drown the real saves (design 20 §6.1). Matched without regard to
/// case against a path relative to its save root's base (the user dir, the
/// game's files, `ProgramData`):
///
/// - `A/B/` (two or more parts): a dir anchored at the base;
/// - `*/A/` or `A/`: a dir of that name anywhere;
/// - anything else: a file name.
///
/// `*` matches any run of characters within one name. Settings files
/// (`*.ini`, `*.cfg`) are never here: losing them is a loss too.
pub const EXCLUDED: &[&str] = &[
    "AppData/Local/Temp/",
    "AppData/Local/Microsoft/",
    "*/INetCache/",
    "AppData/Local/D3DSCache/",
    "AppData/Local/NVIDIA*/",
    "CrashDumps/",
    "*.dxvk-cache",
    "*.dxvk-state-cache",
    "vkd3d-proton.cache*",
    "*.log",
    "*.dmp",
    "Thumbs.db",
];

/// The save roots under a user dir (design 20 §6.1).
pub const USER_ROOTS: &[&str] = &[
    "Documents",
    "Saved Games",
    "AppData/Roaming",
    "AppData/Local",
    "AppData/LocalLow",
];

/// `*` globbing within one name, without regard to case.
fn glob(pattern: &str, name: &str) -> bool {
    let p = pattern.to_ascii_lowercase();
    let n = name.to_ascii_lowercase();
    let parts: Vec<&str> = p.split('*').collect();
    if parts.len() == 1 {
        return p == n;
    }
    let mut rest = n.as_str();
    let first = parts[0];
    if !rest.starts_with(first) {
        return false;
    }
    rest = &rest[first.len()..];
    let last = parts[parts.len() - 1];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

/// Whether `rel` (relative to its save root's base, `/`-separated) is one of
/// [`EXCLUDED`].
pub fn excluded(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
    let Some((name, dirs)) = parts.split_last() else {
        return false;
    };
    EXCLUDED.iter().any(|pat| match pat.strip_suffix('/') {
        Some(dir) => {
            let pat_parts: Vec<&str> = dir.split('/').collect();
            match pat_parts.as_slice() {
                ["*", one] | [one] => dirs.iter().any(|d| glob(one, d)),
                many => dirs.len() >= many.len() && many.iter().zip(dirs).all(|(p, d)| glob(p, d)),
            }
        }
        None => glob(pat, name),
    })
}

/// What a file of the build is expected to be, from its file list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// A depot manifest's size: a file of another size was rewritten.
    Size(u64),
    /// An installer log names the file but not its size: one changed after
    /// the install was rewritten.
    NotAfter(SystemTime),
}

/// The current build's files in the game's own files (design 20 §6.1), keyed
/// by their path relative to the game's files, lowercased.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildFiles {
    /// Where the list came from, for the plan (`the depot cache`, `the
    /// build's depot manifests`, `the installer's log`).
    pub source: String,
    files: BTreeMap<String, Expect>,
}

impl BuildFiles {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            files: BTreeMap::new(),
        }
    }

    /// Add `rel` (relative to the game's files, `/` or `\` separated).
    pub fn insert(&mut self, rel: &str, expect: Expect) {
        let key = rel
            .replace('\\', "/")
            .trim_matches('/')
            .to_ascii_lowercase();
        if !key.is_empty() {
            self.files.insert(key, expect);
        }
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Whether the file at `rel`, as it is now, is the build's own.
    fn is_builds(&self, rel: &str, size: u64, mtime: SystemTime) -> bool {
        match self.files.get(&rel.to_ascii_lowercase()) {
            Some(Expect::Size(n)) => *n == size,
            // Two seconds: a filesystem's mtime and the log's own may round.
            Some(Expect::NotAfter(t)) => mtime <= *t + std::time::Duration::from_secs(2),
            None => false,
        }
    }
}

/// A depot manifest path (relative to the cache root, as gog-warp lays it
/// out) as a path in the game's files: `<install dir>/<rel>`, or a support
/// overlay's `gog-support/<base>[/<product>]/app/<rel>`, which the build
/// merges into the game's files. `None` for anything else.
pub fn depot_rel(path: &str, install_dir: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    let install = install_dir.replace('\\', "/");
    let install = install.trim_matches('/');
    if let Some(rest) = path.strip_prefix(&format!("{install}/"))
        && !rest.is_empty()
    {
        return Some(rest.to_string());
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts.first() != Some(&"gog-support") {
        return None;
    }
    let app = parts
        .iter()
        .take(4)
        .position(|p| p.eq_ignore_ascii_case("app"))?;
    let rest = parts[app + 1..].join("/");
    (app >= 2 && !rest.is_empty()).then_some(rest)
}

/// The files an Inno Setup `/LOG` names (`Dest filename: C:\…`), as paths
/// relative to `root_win` (the install dir as Windows sees it, `C:\GOG\x`),
/// without regard to case. Files outside it are left out. The log is UTF-8
/// (with or without a BOM) or UTF-16LE with a BOM.
pub fn inno_log_files(raw: &[u8], root_win: &str) -> Vec<String> {
    let text = if let Some(rest) = raw.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(raw.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(raw)).into_owned()
    };
    let root = format!("{}\\", root_win.trim_end_matches('\\').to_ascii_lowercase());
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(i) = line.find("Dest filename: ") else {
            continue;
        };
        let dest = line[i + "Dest filename: ".len()..].trim();
        if dest.len() > root.len()
            && dest.is_char_boundary(root.len())
            && dest[..root.len()].eq_ignore_ascii_case(&root)
        {
            out.push(dest[root.len()..].replace('\\', "/"));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Files gogo writes into the game's files itself, which are never saves
/// (design 20 §6.1): the compat `files` setup wrote, and cnc-ddraw's DLL,
/// `ddraw.ini`, `Shaders/` and the DDrawCompat it set aside.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnFiles {
    /// Lowercased paths relative to the game's files.
    files: HashSet<String>,
    /// Lowercased suffixes (`dir/file`): the compat `files`, relative to a
    /// dir in the game's files.
    suffixes: Vec<String>,
    /// Lowercased dirs (relative) whose everything is gogo's.
    dirs: Vec<String>,
}

impl OwnFiles {
    /// gogo's own files in `game_files`: `compat` are the paths of
    /// `state.toml`'s `files` lines; cnc-ddraw's are found by its `ddraw.ini`,
    /// which starts with `; gogo:`.
    pub fn scan(game_files: &Path, compat: &[String]) -> Self {
        let mut own = OwnFiles {
            suffixes: compat
                .iter()
                .map(|p| p.replace('\\', "/").trim_matches('/').to_ascii_lowercase())
                .filter(|p| !p.is_empty())
                .collect(),
            ..OwnFiles::default()
        };
        let mut stack = vec![game_files.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut names = Vec::new();
            for e in rd.flatten() {
                let Ok(ty) = e.file_type() else { continue };
                if ty.is_dir() {
                    stack.push(e.path());
                } else if let Some(n) = e.file_name().to_str() {
                    names.push(n.to_string());
                }
            }
            let Some(ini) = names.iter().find(|n| n.eq_ignore_ascii_case("ddraw.ini")) else {
                continue;
            };
            let gogo =
                std::fs::read_to_string(dir.join(ini)).is_ok_and(|s| s.starts_with("; gogo:"));
            if !gogo {
                continue;
            }
            let rel = |n: &str| {
                let p = dir.join(n);
                p.strip_prefix(game_files)
                    .map(|r| r.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default()
            };
            own.files.insert(rel(ini));
            own.files.insert(rel("ddraw.dll"));
            own.dirs.push(rel("Shaders"));
            for n in &names {
                if let Some(orig) = n.strip_suffix(".ddrawcompat") {
                    own.files.insert(rel(n));
                    own.files.insert(rel(orig));
                }
            }
        }
        own
    }

    fn contains(&self, rel: &str) -> bool {
        let rel = rel.to_ascii_lowercase();
        self.files.contains(&rel)
            || self
                .suffixes
                .iter()
                .any(|s| rel == *s || rel.ends_with(&format!("/{s}")))
            || self.dirs.iter().any(|d| rel.starts_with(&format!("{d}/")))
    }
}

/// One save file newer than the snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SaveFile {
    /// Relative to `drive_c/`; the depot lane's game files as `game/…`.
    pub path: String,
    pub size: u64,
    /// UTC, RFC 3339.
    pub mtime: String,
    #[serde(skip)]
    pub abs: PathBuf,
    #[serde(skip)]
    pub modified: SystemTime,
}

/// The game's own files as a save root.
#[derive(Debug, Clone)]
pub struct GameFiles {
    /// The files now.
    pub dir: PathBuf,
    /// The same dir in the snapshot.
    pub snap_dir: Option<PathBuf>,
    /// Their name in reported paths: `game` (depot lane), or the install dir
    /// relative to `drive_c/` (`GOG/<slug>`).
    pub label: String,
    /// The current build's files; `None` when no list could be had.
    pub list: Option<BuildFiles>,
    /// gogo's own files there.
    pub own: OwnFiles,
    /// The build and DLC set are the snapshot's: nothing gogo laid since is
    /// new, so new files are saves too even without a list.
    pub same_content: bool,
}

/// Where to look.
#[derive(Debug, Clone)]
pub struct Scan {
    /// The game's prefix now.
    pub prefix: PathBuf,
    /// The prefix in the snapshot; `None`: the mtime test alone.
    pub snap_prefix: Option<PathBuf>,
    /// The snapshot's time (`created`).
    pub created: SystemTime,
    pub game: Option<GameFiles>,
}

/// How the game's own files were checked, for the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "check")]
pub enum GameCheck {
    /// Against the build's file list (by name and size, or, from an installer
    /// log, by name and time).
    List { source: String, files: usize },
    /// No list: files rewritten in place only, unless the build and DLC set
    /// are the snapshot's.
    NoList,
    /// No game files to check.
    None,
}

/// What [`find`] found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Found {
    pub files: Vec<SaveFile>,
    /// New files in the game's files gogo can't tell from what a DLC add or
    /// an update laid (no list, and the content changed since the snapshot):
    /// shown, not copied.
    pub unattributed: Vec<SaveFile>,
    pub game_check: GameCheck,
    /// The user dirs looked in (`steamuser`).
    pub users: Vec<String>,
}

impl Found {
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

fn rfc3339(t: SystemTime) -> String {
    DateTime::<Utc>::from(t).to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The user dirs of a prefix's `drive_c/users/`: real dirs only (Proton's
/// `steamuser`; Wine's link named after the host user points at it), never
/// `Public`.
pub fn users(prefix: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(prefix.join("drive_c/users"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| !n.eq_ignore_ascii_case("public"))
        .collect();
    out.sort();
    out
}

/// The regular files under `dir`, relative, `/`-separated; links aren't
/// followed (a prefix's links lead out of it).
fn walk(dir: &Path) -> Vec<(String, std::fs::Metadata)> {
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(dir.join(&rel)) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(ty) = e.file_type() else { continue };
            let r = rel.join(e.file_name());
            if ty.is_dir() {
                stack.push(r);
            } else if ty.is_file()
                && let Ok(meta) = e.metadata()
            {
                out.push((r.to_string_lossy().replace('\\', "/"), meta));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Whether a file is newer than the snapshot (design 20 §6.1), and whether
/// the snapshot lacks it. With the snapshot's tree: absent there, or changed
/// (size or mtime) after `created`. Without it: the mtime test alone.
fn newer(meta: &std::fs::Metadata, in_snap: Option<&Path>, created: SystemTime) -> (bool, bool) {
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    match in_snap.map(std::fs::symlink_metadata) {
        None => (mtime > created, false),
        Some(Ok(m)) if m.is_file() => {
            let changed = m.len() != meta.len() || m.modified().ok() != Some(mtime);
            (changed && mtime > created, false)
        }
        Some(_) => (true, true),
    }
}

fn save_file(path: String, abs: PathBuf, meta: &std::fs::Metadata) -> SaveFile {
    let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    SaveFile {
        path,
        size: meta.len(),
        mtime: rfc3339(modified),
        abs,
        modified,
    }
}

/// The save files newer than the snapshot (design 20 §6.1).
pub fn find(scan: &Scan) -> Found {
    let mut files = Vec::new();
    let mut unattributed = Vec::new();
    let users = users(&scan.prefix);
    let drive_c = scan.prefix.join("drive_c");
    let snap_c = scan.snap_prefix.as_ref().map(|p| p.join("drive_c"));
    let mut roots: Vec<(String, String)> = Vec::new(); // (base rel to drive_c, root rel to base)
    for u in &users {
        for r in USER_ROOTS {
            roots.push((format!("users/{u}"), (*r).to_string()));
        }
    }
    roots.push(("ProgramData".to_string(), String::new()));
    for (base, root) in &roots {
        let dir = if root.is_empty() {
            drive_c.join(base)
        } else {
            drive_c.join(base).join(root)
        };
        for (rel, meta) in walk(&dir) {
            let in_base = if root.is_empty() {
                rel.clone()
            } else {
                format!("{root}/{rel}")
            };
            if excluded(&in_base) {
                continue;
            }
            let path = format!("{base}/{in_base}");
            let snap = snap_c.as_ref().map(|c| c.join(&path));
            if newer(&meta, snap.as_deref(), scan.created).0 {
                files.push(save_file(path, drive_c.join(base).join(&in_base), &meta));
            }
        }
    }
    let game_check = match &scan.game {
        None => GameCheck::None,
        Some(g) => {
            for (rel, meta) in walk(&g.dir) {
                if excluded(&rel) || g.own.contains(&rel) {
                    continue;
                }
                let snap = g.snap_dir.as_ref().map(|d| d.join(&rel));
                let (is_newer, absent) = newer(&meta, snap.as_deref(), scan.created);
                if !is_newer {
                    continue;
                }
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                let f = save_file(format!("{}/{rel}", g.label), g.dir.join(&rel), &meta);
                match &g.list {
                    Some(list) if list.is_builds(&rel, meta.len(), mtime) => {}
                    Some(_) => files.push(f),
                    None if absent && !g.same_content => unattributed.push(f),
                    None => files.push(f),
                }
            }
            match &g.list {
                Some(l) => GameCheck::List {
                    source: l.source.clone(),
                    files: l.len(),
                },
                None => GameCheck::NoList,
            }
        }
    };
    files.sort_by(|a, b| a.path.cmp(&b.path));
    unattributed.sort_by(|a, b| a.path.cmp(&b.path));
    Found {
        files,
        unattributed,
        game_check,
        users,
    }
}

/// Newer saves in one dir, for the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    /// The dir, relative to `drive_c/` with the user dir dropped (`Saved
    /// Games/CD Projekt Red/Cyberpunk 2077`), or the game's (`game/SAVE`).
    pub dir: String,
    pub files: usize,
    pub bytes: u64,
    /// UTC, RFC 3339.
    pub newest: String,
    #[serde(skip)]
    pub newest_at: SystemTime,
}

/// `files` grouped by dir, in path order (design 20 §6.1's display).
pub fn groups(files: &[SaveFile]) -> Vec<Group> {
    let mut by: BTreeMap<String, Group> = BTreeMap::new();
    for f in files {
        let shown = match f.path.strip_prefix("users/") {
            Some(rest) => rest.split_once('/').map_or(rest, |(_, r)| r),
            None => f.path.as_str(),
        };
        let dir = shown.rsplit_once('/').map_or(".", |(d, _)| d).to_string();
        let g = by.entry(dir.clone()).or_insert(Group {
            dir,
            files: 0,
            bytes: 0,
            newest: String::new(),
            newest_at: SystemTime::UNIX_EPOCH,
        });
        g.files += 1;
        g.bytes += f.size;
        if f.modified >= g.newest_at {
            g.newest_at = f.modified;
            g.newest = f.mtime.clone();
        }
    }
    by.into_values().collect()
}

/// What [`copy_aside`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Copied {
    pub files: usize,
    pub bytes: u64,
    /// Files cloned (FICLONE) rather than copied.
    pub cloned: usize,
}

/// Copy `files` into `dir` (design 20 §6.2), each at its `path`: a clone
/// where the filesystem can, else a copy; mode and mtime kept. A file there
/// already (a rerun into the same dir) is replaced.
pub fn copy_aside(files: &[SaveFile], dir: &Path) -> io::Result<Copied> {
    let mut out = Copied::default();
    std::fs::create_dir_all(dir)?;
    for f in files {
        let dst = dir.join(&f.path);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match std::fs::remove_file(&dst) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        if materialize::clone_file(&f.abs, &dst, Reflink::Try)? {
            out.cloned += 1;
        }
        out.files += 1;
        out.bytes += f.size;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exclusion_list_is_the_designs() {
        // The constant is the contract: changing it changes what a rollback
        // shows and copies (design 20 §6.1).
        assert_eq!(
            EXCLUDED,
            [
                "AppData/Local/Temp/",
                "AppData/Local/Microsoft/",
                "*/INetCache/",
                "AppData/Local/D3DSCache/",
                "AppData/Local/NVIDIA*/",
                "CrashDumps/",
                "*.dxvk-cache",
                "*.dxvk-state-cache",
                "vkd3d-proton.cache*",
                "*.log",
                "*.dmp",
                "Thumbs.db",
            ]
        );
        for out in [
            "AppData/Local/Temp/x.tmp",
            "AppData/local/temp/x.tmp",
            "AppData/Local/Microsoft/Windows/INetCache/a",
            "AppData/Roaming/Foo/INetCache/a",
            "AppData/Local/D3DSCache/abc/x",
            "AppData/Local/NVIDIA Corporation/GLCache/x",
            "AppData/Local/NVIDIA/DXCache/x",
            "AppData/Local/CrashDumps/game.exe.123.dmp",
            "CrashDumps/x",
            "game.dxvk-cache",
            "game.dxvk-state-cache",
            "vkd3d-proton.cache",
            "vkd3d-proton.cache.write",
            "dxgi.log",
            "Documents/Foo/output.LOG",
            "core.dmp",
            "SAVE/Thumbs.db",
        ] {
            assert!(excluded(out), "{out} should be excluded");
        }
        for kept in [
            "Saved Games/CD Projekt Red/Cyberpunk 2077/save.dat",
            "Documents/My Games/x/settings.ini",
            "AppData/Local/CD Projekt Red/user.cfg",
            "AppData/LocalLow/Studio/Game/save1.sav",
            "SAVE/slot1.sav",
            "Games/a.GM1",
            "Temp/not-anchored-at-AppData.sav",
            "logbook.sav",
        ] {
            assert!(!excluded(kept), "{kept} should be kept");
        }
    }

    #[test]
    fn globs_match_within_one_name() {
        assert!(glob("*.log", "a.LOG"));
        assert!(!glob("*.log", "a.log.sav"));
        assert!(glob("NVIDIA*", "NVIDIA Corporation"));
        assert!(glob("vkd3d-proton.cache*", "vkd3d-proton.cache"));
        assert!(!glob("thumbs.db", "thumbs.dbx"));
        assert!(glob("a*b*c", "aXbYc"));
        assert!(!glob("a*b*c", "ac"));
    }

    #[test]
    fn depot_paths_map_into_the_game_files() {
        assert_eq!(
            depot_rel("Test Game/Data/a.pak", "Test Game").as_deref(),
            Some("Data/a.pak")
        );
        assert_eq!(
            depot_rel("gog-support/1/2/app/x.dll", "Test Game").as_deref(),
            Some("x.dll")
        );
        assert_eq!(
            depot_rel("gog-support/1/app/sub/y.ini", "Test Game").as_deref(),
            Some("sub/y.ini")
        );
        assert_eq!(depot_rel("gog-support/1/2/redist/v.exe", "Test Game"), None);
        assert_eq!(depot_rel("Other/x", "Test Game"), None);
    }

    #[test]
    fn an_inno_log_names_the_files_under_the_install_dir() {
        let log = "\u{feff}2026-01-01 10:00:00.000   Log opened. (Time zone: UTC+00:00)\n\
                   2026-01-01 10:00:01.000   Dest filename: C:\\GOG\\x\\game.exe\n\
                   2026-01-01 10:00:01.000   Time stamp of our file: 2010-01-01 00:00:00.000\n\
                   2026-01-01 10:00:01.000   Dest filename: c:\\gog\\X\\Data\\a.pak\n\
                   2026-01-01 10:00:01.000   Dest filename: C:\\windows\\system32\\x.dll\n\
                   2026-01-01 10:00:01.000   Dest filename: C:\\GOG\\xy\\other.dat\n";
        assert_eq!(
            inno_log_files(log.as_bytes(), "C:\\GOG\\x"),
            ["Data/a.pak", "game.exe"]
        );
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(
                "Dest filename: C:\\GOG\\x\\SAVE\\s.sav\r\n"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes),
            )
            .collect();
        assert_eq!(inno_log_files(&utf16, "C:\\GOG\\x\\"), ["SAVE/s.sav"]);
    }

    use std::time::Duration;

    /// A file with an mtime `secs` after the epoch.
    fn put(root: &Path, rel: &str, body: &str, secs: u64) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        let f = std::fs::File::options().write(true).open(&p).unwrap();
        f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }

    const THEN: u64 = 1_000_000;
    const AFTER: u64 = THEN + 100;

    /// A game dir now and its snapshot: the prefix's user dirs, ProgramData,
    /// and a `game/` with shipped files.
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let now = tmp.path().join("now");
        let snap = tmp.path().join("snap");
        for root in [&now, &snap] {
            put(
                root,
                "prefix/drive_c/users/steamuser/Saved Games/Studio/old.sav",
                "old",
                THEN - 10,
            );
            put(root, "game/game.exe", "MZ", THEN - 1000);
            put(root, "game/settings.ini", "a=1", THEN - 1000);
            put(root, "game/data.pak", "pak", THEN - 1000);
            std::fs::create_dir_all(root.join("prefix/drive_c/users/Public")).unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink("steamuser", now.join("prefix/drive_c/users/someone")).unwrap();
        // Newer saves in each user root and ProgramData.
        put(
            &now,
            "prefix/drive_c/users/steamuser/Saved Games/Studio/new.sav",
            "n",
            AFTER,
        );
        put(
            &now,
            "prefix/drive_c/users/steamuser/Documents/My Games/g/cfg.ini",
            "c",
            AFTER,
        );
        put(
            &now,
            "prefix/drive_c/users/steamuser/AppData/Roaming/G/r.dat",
            "r",
            AFTER,
        );
        put(
            &now,
            "prefix/drive_c/users/steamuser/AppData/LocalLow/G/ll.dat",
            "l",
            AFTER,
        );
        put(&now, "prefix/drive_c/ProgramData/G/shared.sav", "p", AFTER);
        put(
            &now,
            "prefix/drive_c/users/Public/Documents/x.sav",
            "pub",
            AFTER,
        );
        // Caches and logs: never saves.
        put(
            &now,
            "prefix/drive_c/users/steamuser/AppData/Local/Temp/t.tmp",
            "t",
            AFTER,
        );
        put(
            &now,
            "prefix/drive_c/users/steamuser/AppData/Local/D3DSCache/x/y",
            "d",
            AFTER,
        );
        put(
            &now,
            "prefix/drive_c/users/steamuser/Documents/My Games/g/out.log",
            "o",
            AFTER,
        );
        // The old save, rewritten after the snapshot.
        put(
            &now,
            "prefix/drive_c/users/steamuser/Saved Games/Studio/old.sav",
            "old2",
            AFTER,
        );
        // The game's files: a new save, a shipped .ini rewritten at a new
        // size, a file a later DLC add laid, gogo's own files, caches.
        put(&now, "game/SAVE/slot1.sav", "s1", AFTER);
        put(&now, "game/settings.ini", "a=22", AFTER);
        put(&now, "game/dlc/dlc.pak", "dlc", AFTER);
        put(&now, "game/ddraw.ini", "; gogo: cnc-ddraw\n", AFTER);
        put(&now, "game/ddraw.dll", "MZ", AFTER);
        put(&now, "game/Shaders/a.glsl", "s", AFTER);
        put(&now, "game/fix/patch.cfg", "compat", AFTER);
        put(&now, "game/game.dxvk-cache", "c", AFTER);
        put(&now, "game/Thumbs.db", "c", AFTER);
        (tmp, now, snap)
    }

    fn scan(now: &Path, snap: &Path, list: Option<BuildFiles>, same: bool) -> Found {
        find(&Scan {
            prefix: now.join("prefix"),
            snap_prefix: Some(snap.join("prefix")),
            created: SystemTime::UNIX_EPOCH + Duration::from_secs(THEN),
            game: Some(GameFiles {
                dir: now.join("game"),
                snap_dir: Some(snap.join("game")),
                label: "game".into(),
                list,
                own: OwnFiles::scan(&now.join("game"), &["fix/patch.cfg".to_string()]),
                same_content: same,
            }),
        })
    }

    fn paths(f: &[SaveFile]) -> Vec<&str> {
        f.iter().map(|f| f.path.as_str()).collect()
    }

    const PREFIX_SAVES: [&str; 6] = [
        "ProgramData/G/shared.sav",
        "users/steamuser/AppData/LocalLow/G/ll.dat",
        "users/steamuser/AppData/Roaming/G/r.dat",
        "users/steamuser/Documents/My Games/g/cfg.ini",
        "users/steamuser/Saved Games/Studio/new.sav",
        "users/steamuser/Saved Games/Studio/old.sav",
    ];

    #[test]
    fn the_finder_checks_the_game_files_against_the_builds_list() {
        let (_tmp, now, snap) = fixture();
        let mut list = BuildFiles::new("a fake manifest");
        list.insert("game.exe", Expect::Size(2));
        list.insert("settings.ini", Expect::Size(3));
        list.insert("data.pak", Expect::Size(3));
        list.insert("dlc\\dlc.pak", Expect::Size(3));
        let found = scan(&now, &snap, Some(list), false);
        let mut want: Vec<&str> = PREFIX_SAVES.to_vec();
        want.extend(["game/SAVE/slot1.sav", "game/settings.ini"]);
        want.sort();
        assert_eq!(paths(&found.files), want);
        assert!(found.unattributed.is_empty());
        assert_eq!(found.users, ["steamuser"]);
        assert_eq!(
            found.game_check,
            GameCheck::List {
                source: "a fake manifest".into(),
                files: 4
            }
        );
    }

    #[test]
    fn without_a_list_only_rewritten_files_count_unless_the_content_is_the_snapshots() {
        let (_tmp, now, snap) = fixture();
        let found = scan(&now, &snap, None, false);
        let mut want: Vec<&str> = PREFIX_SAVES.to_vec();
        want.push("game/settings.ini");
        want.sort();
        assert_eq!(paths(&found.files), want);
        assert_eq!(
            paths(&found.unattributed),
            ["game/SAVE/slot1.sav", "game/dlc/dlc.pak"]
        );
        assert_eq!(found.game_check, GameCheck::NoList);
        // The same build and DLC set: nothing gogo laid is new.
        let found = scan(&now, &snap, None, true);
        assert!(found.unattributed.is_empty());
        assert!(paths(&found.files).contains(&"game/SAVE/slot1.sav"));
        assert!(paths(&found.files).contains(&"game/dlc/dlc.pak"));
    }

    #[test]
    fn an_installer_logs_list_goes_by_time() {
        let (_tmp, now, snap) = fixture();
        let log = "Dest filename: C:\\GOG\\x\\settings.ini\n\
                   Dest filename: C:\\GOG\\x\\data.pak\n\
                   Dest filename: C:\\GOG\\x\\game.exe\n";
        let mut list = BuildFiles::new("the installer's log");
        for rel in inno_log_files(log.as_bytes(), "C:\\GOG\\x") {
            list.insert(
                &rel,
                Expect::NotAfter(SystemTime::UNIX_EPOCH + Duration::from_secs(THEN - 500)),
            );
        }
        let found = scan(&now, &snap, Some(list), false);
        let game: Vec<&str> = paths(&found.files)
            .into_iter()
            .filter(|p| p.starts_with("game/"))
            .collect();
        assert_eq!(
            game,
            [
                "game/SAVE/slot1.sav",
                "game/dlc/dlc.pak",
                "game/settings.ini"
            ]
        );
    }

    #[test]
    fn the_copy_aside_keeps_the_paths_from_drive_c() {
        let (tmp, now, snap) = fixture();
        let found = scan(&now, &snap, None, true);
        let aside = tmp.path().join("aside/20261112T104107Z");
        let c = copy_aside(&found.files, &aside).unwrap();
        assert_eq!(c.files, found.files.len());
        assert_eq!(
            std::fs::read_to_string(aside.join("users/steamuser/Saved Games/Studio/new.sav"))
                .unwrap(),
            "n"
        );
        assert_eq!(
            std::fs::read_to_string(aside.join("game/SAVE/slot1.sav")).unwrap(),
            "s1"
        );
        assert_eq!(
            std::fs::read_to_string(aside.join("ProgramData/G/shared.sav")).unwrap(),
            "p"
        );
        let m = std::fs::metadata(aside.join("game/SAVE/slot1.sav")).unwrap();
        assert_eq!(
            m.modified().unwrap(),
            SystemTime::UNIX_EPOCH + Duration::from_secs(AFTER),
            "mtime kept"
        );
        // Again into the same dir: replaced, not refused.
        copy_aside(&found.files, &aside).unwrap();
    }

    #[test]
    fn without_the_snapshots_tree_the_mtime_test_decides() {
        let (_tmp, now, _snap) = fixture();
        let found = find(&Scan {
            prefix: now.join("prefix"),
            snap_prefix: None,
            created: SystemTime::UNIX_EPOCH + Duration::from_secs(THEN),
            game: None,
        });
        assert_eq!(paths(&found.files), PREFIX_SAVES);
        assert_eq!(found.game_check, GameCheck::None);
    }

    #[test]
    fn saves_group_by_dir_without_the_user_dir() {
        let f = |path: &str, size, secs| SaveFile {
            path: path.into(),
            size,
            mtime: rfc3339(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs)),
            abs: PathBuf::new(),
            modified: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs),
        };
        let g = groups(&[
            f("users/steamuser/Saved Games/Studio/Game/a.sav", 10, 5),
            f("users/steamuser/Saved Games/Studio/Game/b.sav", 20, 9),
            f("game/SAVE/slot1.sav", 3, 1),
        ]);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].dir, "Saved Games/Studio/Game");
        assert_eq!((g[0].files, g[0].bytes), (2, 30));
        assert_eq!(g[0].newest, "1970-01-01T00:00:09Z");
        assert_eq!(g[1].dir, "game/SAVE");
    }
}
