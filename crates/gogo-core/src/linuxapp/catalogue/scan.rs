//! What a Proton build itself says exists (design 18 §7.7): read, never run.
//!
//! Three kinds of file, all read as data:
//! - the `proton` script and the `protonfixes` helpers: which env vars they
//!   read, which they set, the `STEAM_COMPAT_CONFIG` token a var maps to, and
//!   the default a `setdefault` gives. A line scanner, not a Python parser:
//!   it knows the handful of shapes these scripts use and ignores the rest;
//! - `user_settings.sample.py`: the settings Proton documents for users,
//!   commented out or not;
//! - the bundled DLLs and Wine modules: the names they contain. A string in
//!   the file is evidence the component knows the name, not proof it reads
//!   it, and a name split by the compiler (`mov rax, "DXVK_ENA"`) leaves no
//!   whole string. So a hit counts, and a miss means "no trace of it".

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

/// How the build's scripts use one env var.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ScriptUse {
    /// Read in `proton` (a switch, or plumbing).
    pub reads: bool,
    /// Read in the `protonfixes` helpers.
    pub fixes_reads: bool,
    /// `proton` assigns it, for some configurations at least.
    pub sets: bool,
    /// The `STEAM_COMPAT_CONFIG` token `check_environment` maps it to.
    pub token: Option<String>,
    /// The value of a `setdefault` that isn't inside a condition.
    pub default: Option<String>,
}

/// The DLLs and modules worth reading for names, relative to `lib/wine` under
/// the build's `files/`. Not all of Wine: `x86_64-windows` alone is 300 MB,
/// and these are the ones that read a switch a player would set.
const SCANNED: &[(&str, &str)] = &[
    ("dxvk", "dxvk/x86_64-windows/dxgi.dll"),
    ("dxvk", "dxvk/x86_64-windows/d3d11.dll"),
    ("dxvk", "dxvk/x86_64-windows/d3d9.dll"),
    ("dxvk", "dxvk/x86_64-windows/d3d8.dll"),
    ("vkd3d-proton", "vkd3d-proton/x86_64-windows/d3d12.dll"),
    ("vkd3d-proton", "vkd3d-proton/x86_64-windows/d3d12core.dll"),
    ("dxvk-nvapi", "nvapi/x86_64-windows/nvapi64.dll"),
    ("dxvk-nvapi", "nvapi/x86_64-windows/nvofapi64.dll"),
    ("d7vk", "d7vk/i386-windows/ddraw.dll"),
    ("wine", "x86_64-unix/ntdll.so"),
    ("wine", "x86_64-unix/win32u.so"),
    ("wine", "x86_64-unix/winewayland.so"),
    ("wine", "x86_64-unix/winex11.so"),
    ("wine", "x86_64-unix/winebus.so"),
    ("wine", "x86_64-windows/lsteamclient.dll"),
];

/// The prefixes that make a whole string in a DLL a switch to discover: a
/// name with one of these in a scanned file needs a table entry (or a place
/// in [`super::table::PLUMBING`]).
const DISCOVER: &[&str] = &["DXVK_", "VKD3D_", "D7VK_", "PROTON_", "WAYLANDDRV_"];

/// What a build shows, for the catalogue to merge.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BuildFacts {
    pub dir: PathBuf,
    /// `GE-Proton11-12`, from the `version` file.
    pub version: Option<String>,
    /// Bundled component versions (`dxvk` → `v3.1.1-174-gf86377680`).
    pub components: BTreeMap<String, String>,
    /// Env vars the scripts mention.
    pub script: BTreeMap<String, ScriptUse>,
    /// Names in `user_settings.sample.py`.
    pub sample: BTreeSet<String>,
    /// Names found in the scanned files, and which files: the discovered
    /// ones and the wanted ones.
    pub found: BTreeMap<String, BTreeSet<String>>,
    /// Components with at least one file read. A miss elsewhere says nothing.
    pub scanned: BTreeSet<String>,
    /// `proton` was there and could be read.
    pub script_read: bool,
}

impl BuildFacts {
    /// Read `dir`. `wanted` are names to look up exactly on top of the
    /// discovered ones, `flags` names that may also be the tail of a longer
    /// string (a linker merges `dxr` into `nodxr`).
    pub fn read(dir: &Path, wanted: &[&str], flags: &[&str]) -> BuildFacts {
        let mut f = BuildFacts {
            dir: dir.to_path_buf(),
            version: read_version(dir),
            ..BuildFacts::default()
        };
        if let Ok(text) = std::fs::read(dir.join("proton")) {
            f.script_read = true;
            parse_script(&String::from_utf8_lossy(&text), &mut f.script, false);
        }
        let fixes = dir.join("protonfixes");
        for name in [
            "fix.py",
            "upscalers.py",
            "utilities.py",
            "util.py",
            "engine.py",
        ] {
            if let Ok(text) = std::fs::read(fixes.join(name)) {
                parse_script(&String::from_utf8_lossy(&text), &mut f.script, true);
            }
        }
        if let Ok(text) = std::fs::read(dir.join("user_settings.sample.py")) {
            f.sample = parse_sample(&String::from_utf8_lossy(&text));
        }
        let Some(lib) = wine_lib(dir) else {
            return f;
        };
        for (component, dirname) in [
            ("dxvk", "dxvk"),
            ("vkd3d-proton", "vkd3d-proton"),
            ("dxvk-nvapi", "nvapi"),
            ("d7vk", "d7vk"),
        ] {
            if let Some(v) = read_component_version(&lib.join(dirname).join("version")) {
                f.components.insert(component.to_string(), v);
            }
        }
        let wanted: HashSet<&[u8]> = wanted.iter().map(|w| w.as_bytes()).collect();
        for (component, rel) in SCANNED {
            let Ok(bytes) = std::fs::read(lib.join(rel)) else {
                continue;
            };
            f.scanned.insert((*component).to_string());
            let file = rel.rsplit('/').next().unwrap_or(rel).to_string();
            let idents = identifiers(&bytes);
            for (s, nul) in &idents {
                let Ok(text) = std::str::from_utf8(s) else {
                    continue;
                };
                if wanted.contains(s) || (*nul && is_discovered(text)) {
                    f.found
                        .entry(text.to_string())
                        .or_default()
                        .insert(file.clone());
                }
            }
            for flag in flags {
                let b = flag.as_bytes();
                if idents.keys().any(|s| s.ends_with(b)) {
                    f.found
                        .entry((*flag).to_string())
                        .or_default()
                        .insert(file.clone());
                }
            }
        }
        f
    }

    /// The names that need a table entry: read by a script, documented by the
    /// sample, or discovered in a DLL. A name the scripts only set doesn't:
    /// that's the build talking to itself.
    pub fn switch_names(&self) -> BTreeSet<String> {
        let mut out: BTreeSet<String> = self
            .script
            .iter()
            .filter(|(_, u)| u.reads || u.fixes_reads)
            .map(|(k, _)| k.clone())
            .collect();
        out.extend(self.sample.iter().cloned());
        out.extend(self.found.keys().filter(|k| is_discovered(k)).cloned());
        out
    }
}

/// `GE-Proton11-12` from `1793687165 GE-Proton11-12`.
fn read_version(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("version")).ok()?;
    let v = text.split_whitespace().last()?;
    Some(v.to_string())
}

/// `v3.1.1-174-gf86377680` from ` f863… dxvk (v3.1.1-174-gf86377680)`.
fn read_component_version(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let inner = text.split_once('(')?.1.split_once(')')?.0;
    let inner = inner.split_whitespace().next()?;
    Some(inner.to_string())
}

/// Where Proton keeps its PE components: `files/lib/wine` since 10, older
/// builds `files/lib64/wine` or `dist/lib64/wine`.
fn wine_lib(dir: &Path) -> Option<PathBuf> {
    ["files/lib/wine", "files/lib64/wine", "dist/lib64/wine"]
        .iter()
        .map(|p| dir.join(p))
        .find(|p| p.is_dir())
}

/// Runs of `[A-Za-z0-9_.]` of 3 to 80 bytes, and whether each ended in a NUL,
/// as a C string does. Code that loads a name in pieces (`mov rax, "DXVK_ENA"`)
/// leaves runs that don't, so only NUL-ended ones count as discoveries.
fn identifiers(bytes: &[u8]) -> HashMap<&[u8], bool> {
    let ident = |b: &u8| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'.';
    let mut out: HashMap<&[u8], bool> = HashMap::new();
    for piece in bytes.split_inclusive(|b| !ident(b)) {
        let (run, nul) = match piece.split_last() {
            Some((last, run)) if !ident(last) => (run, *last == 0),
            _ => (piece, false),
        };
        if (3..=80).contains(&run.len()) {
            *out.entry(run).or_insert(false) |= nul;
        }
    }
    out
}

/// A whole string that looks like a switch. `DXVK_NVAPI_DRS_` is a prefix the
/// code joins with a setting's name, not a name.
fn is_discovered(s: &str) -> bool {
    !s.ends_with('_')
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && DISCOVER.iter().any(|p| s.starts_with(p))
}

/// An env var's name as these scripts write them: upper case, digits and `_`,
/// with a `_` or Wine's prefix (`STORE`, `PATH`, `HOME` aren't switches).
fn is_env_name(s: &str) -> bool {
    s.len() >= 3
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && s.as_bytes()[0].is_ascii_uppercase()
        && !s.ends_with('_')
        && (s.contains('_') || s.starts_with("WINE"))
}

/// The line without a trailing comment (a `#` outside quotes).
fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    for (i, c) in line.char_indices() {
        match (quote, c) {
            (None, '#') => return &line[..i],
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), _) if q == c => quote = None,
            _ => {}
        }
    }
    line
}

/// Every quoted string on a line: start of the literal's quote, the text,
/// and the offset after its closing quote.
fn quoted(line: &str) -> Vec<(usize, &str, usize)> {
    let mut out = Vec::new();
    let mut rest = line.char_indices();
    while let Some((i, c)) = rest.next() {
        if c == '"' || c == '\'' {
            let start = i + 1;
            for (j, d) in rest.by_ref() {
                if d == c {
                    out.push((i, &line[start..j], j + 1));
                    break;
                }
            }
        }
    }
    out
}

/// The indent of a statement directly in a class's method.
const METHOD_BODY: usize = 8;

/// How a quoted env name is used on its line.
enum Use {
    Read,
    Set,
    /// `check_environment("X", "token")`.
    Check,
    /// `env.setdefault("X", "value")`.
    SetDefault,
}

/// What the text around a quoted name says it does, if it's env use at all.
fn classify(before: &str, after: &str) -> Option<Use> {
    if before.ends_with("check_environment(") {
        Some(Use::Check)
    } else if is_env_call(before, ".setdefault(") {
        Some(Use::SetDefault)
    } else if is_env_call(before, "[") {
        let assigned = after.starts_with("] =") && !after.starts_with("] ==");
        Some(if assigned { Use::Set } else { Use::Read })
    } else if is_env_call(before, ".pop(") {
        // Taken out, and used: `env.pop('PROTON_FRAME_RATE', None)`.
        Some(if after.contains("None") {
            Use::Read
        } else {
            Use::Set
        })
    } else if before.contains("_to_env_str(") {
        Some(Use::Set)
    } else if is_env_call(before, ".get(") || before.ends_with("env,") {
        // `.ends_with("env,")`: `get_version(env, 'PROTON_DLSS_UPGRADE', 'default')`
        Some(Use::Read)
    } else if (after.starts_with("in ") || after.starts_with("not in ")) && after.contains("env") {
        Some(Use::Read)
    } else {
        None
    }
}

/// One script's env use into `out`. See the module doc for what it knows.
fn parse_script(text: &str, out: &mut BTreeMap<String, ScriptUse>, fixes: bool) {
    let mut previous = "";
    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let line = strip_comment(line);
        let lits = quoted(line);
        for (n, (start, name, end)) in lits.iter().enumerate() {
            let before = line[..*start].trim_end();
            // A call split over lines: the name opens the next one.
            let before = if before.is_empty() { previous } else { before };
            // `check_environment` says it's a var whatever it looks like
            // (`PYROVEIL`).
            if !is_env_name(name) && !before.ends_with("check_environment(") {
                continue;
            }
            let after = line[*end..].trim_start();
            let Some(how) = classify(before, after) else {
                continue;
            };
            let next = lits.get(n + 1).map(|(_, s, _)| *s);
            let e = out.entry((*name).to_string()).or_default();
            match how {
                Use::Set => e.sets = true,
                Use::Read | Use::Check | Use::SetDefault if fixes => e.fixes_reads = true,
                Use::Read => e.reads = true,
                Use::Check => {
                    e.reads = true;
                    e.token = next.map(str::to_string);
                }
                Use::SetDefault => {
                    e.reads = true;
                    // Inside a method body, not inside an `if` in one:
                    // `PROTON_LOG` raises levels with a deeper setdefault.
                    if indent <= METHOD_BODY
                        && let Some(v) = next
                    {
                        e.default = Some(v.to_string());
                    }
                }
            }
        }
        if !line.trim().is_empty() {
            previous = line.trim_end();
        }
    }
}

/// `before` ends with `<environment><tail>`: `self.env`, `g_session.env`,
/// `os.environ`, a plain `env`.
fn is_env_call(before: &str, tail: &str) -> bool {
    before
        .strip_suffix(tail)
        .is_some_and(|r| r.ends_with("env") || r.ends_with("environ"))
}

/// The keys in `user_settings.sample.py`, commented out ones too: the file's
/// point is to name them.
fn parse_sample(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in text.lines() {
        let line = line.trim_start().trim_start_matches('#').trim_start();
        if let Some((_, name, end)) = quoted(line).first().copied()
            && is_env_name(name)
            && line[end..].trim_start().starts_with(':')
        {
            out.insert(name.to_string());
        }
    }
    out
}
