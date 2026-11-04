//! The game's umu-protonfixes module, read without running it (design 18
//! §4.2, #171). protonfixes imports
//! `protonfixes/gamefixes-gog/<umu id>.py` from the Proton build at launch
//! (`STORE=gog`), after `default.py`; [`read_module`] and [`read_default`]
//! find the same files (symlinks followed, as Python's import does: many
//! point into `gamefixes-steam`) and read them statically.
//!
//! The reader is pattern matching over the module's text, no Python parser:
//! a small lexer turns the source into logical lines (strings, comments,
//! brackets and continuations handled), then each statement of the entry
//! functions protonfixes calls (`early`/`early_with_id`, then
//! `main_with_id` or else `main`) either is a `util.<call>` of a known kind
//! with literal arguments ([`Fix`]) or is listed as [`OtherCode`] ("may do
//! more at launch", R29): computed arguments, calls inside conditions or
//! loops, calls to anything else, module-level statements.
//!
//! Signatures checked against `protonfixes/util.py` at umu-protonfixes
//! `28a02c25` (the commit GE-Proton11-12 pins):
//! `protontricks(verb)`, `set_environment(envvar, value)`,
//! `winedll_override(dll, dtype: OverrideOrder)`,
//! `regedit_add(folder, name=None, typ=None, value=None, arch=False)`,
//! `replace_command(orig, repl, match_flags=re.IGNORECASE)`,
//! `append_argument(argument)`,
//! `set_ini_options(ini_opts, cfile, encoding='utf-8', base_path=BasePath.GAME)`,
//! `set_xml_options(base_attibutte, xml_line, cfile, encoding='utf-8', base_path=BasePath.GAME)`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Where a build keeps the GOG modules, under its root.
pub const GAMEFIXES_GOG: &str = "protonfixes/gamefixes-gog";

/// The module protonfixes runs for every game of a store, before the game's.
pub const DEFAULT_MODULE: &str = "default";

/// `state.toml`'s `protonfix` when the build has no module for the game.
pub const NO_MODULE: &str = "none";

/// One fix a module applies at launch, read from a literal call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fix {
    /// `protontricks(verb)`: a winetricks verb, installed at launch unless
    /// the prefix's `winetricks.log` has it.
    Verb(String),
    /// `set_environment(key, value)`: assigned after gogo's env, so it wins.
    Env { key: String, value: String },
    /// `winedll_override(dll, OverrideOrder.X)`: appended to
    /// `WINEDLLOVERRIDES`. `mode` in its syntax (`n,b`); empty = disabled.
    DllOverride { dll: String, mode: String },
    /// `regedit_add(key, name, type, data, arch)`: a value when all three of
    /// name, type and data are given, else the key alone. `reg64` is
    /// `/reg:64` as `util.py` passes it (`arch` for a value, a given
    /// `name` for a key alone).
    Registry {
        key: String,
        value: Option<RegValue>,
        reg64: bool,
    },
    /// `replace_command(from, to)`: a case-insensitive regex substitution
    /// over every argument of the launch command.
    ReplaceCommand { from: String, to: String },
    /// `append_argument(arg)`.
    AppendArgument(String),
    /// `set_ini_options(options, file)`: INI text merged into a game
    /// config file. `base` is the `BasePath` when not the game dir.
    IniOptions {
        file: String,
        base: Option<String>,
        options: String,
    },
    /// `set_xml_options(anchor, line, file)`: `line` inserted after every
    /// line containing `anchor`.
    XmlOptions {
        file: String,
        base: Option<String>,
        anchor: String,
        line: String,
    },
}

/// A registry value [`Fix::Registry`] writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegValue {
    pub name: String,
    pub kind: String,
    pub data: String,
}

impl Fix {
    /// What a collision is about (`env DXVK_ASYNC`), for the kinds gogo's
    /// layers can also set: env, DLL overrides, registry values.
    pub fn key(&self) -> Option<String> {
        match self {
            Fix::Env { key, .. } => Some(format!("env {key}")),
            Fix::DllOverride { dll, .. } => Some(format!("dll_overrides {dll}")),
            Fix::Registry {
                key,
                value: Some(v),
                ..
            } => Some(format!("registry {key} {}", v.name)),
            _ => None,
        }
    }

    /// The value of a [`key`](Self::key)ed fix, quoted (`"1"`).
    pub fn value(&self) -> Option<String> {
        match self {
            Fix::Env { value, .. } => Some(format!("{value:?}")),
            Fix::DllOverride { mode, .. } => Some(format!("{mode:?}")),
            Fix::Registry { value: Some(v), .. } => Some(format!("{} {:?}", v.kind, v.data)),
            _ => None,
        }
    }

    /// The winetricks verb, for a [`Fix::Verb`].
    pub fn verb(&self) -> Option<&str> {
        match self {
            Fix::Verb(v) => Some(v),
            _ => None,
        }
    }
}

impl std::fmt::Display for Fix {
    /// `verb vcrun2019`, `env DXVK_ASYNC = "1"`, `ini system/Gothic.ini:
    /// [GAME] scaleVideos=1`, …
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let base = |b: &Option<String>| match b {
            Some(b) => format!(" (in {b})"),
            None => String::new(),
        };
        match self {
            Fix::Verb(v) => write!(f, "verb {v}"),
            Fix::DllOverride { dll, mode } if mode.is_empty() => {
                write!(f, "dll_overrides {dll} = \"\" (disabled)")
            }
            Fix::Registry {
                key,
                value: None,
                reg64,
            } => write!(
                f,
                "registry {key} (key only{})",
                if *reg64 { ", 64-bit view" } else { "" }
            ),
            Fix::Registry {
                reg64: true,
                value: Some(_),
                ..
            } => write!(
                f,
                "{} = {} (64-bit view)",
                self.key().unwrap_or_default(),
                self.value().unwrap_or_default()
            ),
            Fix::Env { .. } | Fix::DllOverride { .. } | Fix::Registry { .. } => write!(
                f,
                "{} = {}",
                self.key().unwrap_or_default(),
                self.value().unwrap_or_default()
            ),
            Fix::ReplaceCommand { from, to } => write!(f, "replace_command {from:?} -> {to:?}"),
            Fix::AppendArgument(a) => write!(f, "args += {a:?}"),
            Fix::IniOptions {
                file,
                base: b,
                options,
            } => write!(f, "ini {file}{}: {}", base(b), ini_summary(options)),
            Fix::XmlOptions {
                file,
                base: b,
                anchor,
                line,
            } => write!(
                f,
                "xml {file}{}: {line:?} after each line containing {anchor:?}",
                base(b)
            ),
        }
    }
}

/// INI text in one line: `[GAME] scaleVideos=1; [VIDEO] a=1, b=2`. Blank
/// lines and comments dropped, whitespace around `=` and at line ends
/// trimmed.
fn ini_summary(options: &str) -> String {
    let mut groups: Vec<(Option<String>, Vec<String>)> = Vec::new();
    for line in options.lines().map(str::trim) {
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            groups.push((Some(section.trim().to_string()), Vec::new()));
            continue;
        }
        let item = match line.split_once(['=', ':']) {
            Some((k, v)) => format!("{}={}", k.trim(), v.trim()),
            None => line.to_string(),
        };
        match groups.last_mut() {
            Some((_, items)) => items.push(item),
            None => groups.push((None, vec![item])),
        }
    }
    if groups.is_empty() {
        return "(nothing)".into();
    }
    groups
        .into_iter()
        .map(|(section, items)| match section {
            Some(s) if items.is_empty() => format!("[{s}]"),
            Some(s) => format!("[{s}] {}", items.join(", ")),
            None => items.join(", "),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// A statement the reader can't turn into a [`Fix`]: it may do more at
/// launch than the summary shows (R29).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtherCode {
    /// The statement's first line in the module (1-based).
    pub line: usize,
    /// The statement, whitespace collapsed and cut at [`OTHER_CODE_WIDTH`]
    /// chars; a compound one's header (`if not os.path.isfile(x):`).
    pub code: String,
}

/// How much of an [`OtherCode`] statement is kept.
pub const OTHER_CODE_WIDTH: usize = 100;

impl std::fmt::Display for OtherCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.code)
    }
}

/// What the reader got out of a module's text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reading {
    /// In the order protonfixes applies them: `early` before `main`.
    pub fixes: Vec<Fix>,
    pub other: Vec<OtherCode>,
}

/// A module found in a build, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    /// `umu-910830`, or [`DEFAULT_MODULE`].
    pub name: String,
    /// `<build>/protonfixes/gamefixes-gog/<name>.py` (or its package's
    /// `__init__.py`).
    pub path: PathBuf,
    /// The file it resolves to, when `path` is a symlink.
    pub target: Option<PathBuf>,
    /// sha256 of the module's bytes, lowercase hex: `state.toml`'s
    /// `protonfix`.
    pub sha256: String,
    pub reading: Reading,
}

impl Module {
    /// `gamefixes-gog/umu-40950.py -> gamefixes-steam/40950.py`: the
    /// module's path, and its target when a symlink, from `protonfixes/`
    /// on (absolute when outside it).
    pub fn describe(&self) -> String {
        let rel = |p: &Path| -> String {
            let root = self
                .path
                .ancestors()
                .find(|a| a.file_name().is_some_and(|n| n == "protonfixes"));
            let root = root.and_then(|r| std::fs::canonicalize(r).ok().map(|c| (r, c)));
            match root {
                Some((r, canon)) => p
                    .strip_prefix(r)
                    .or_else(|_| p.strip_prefix(&canon))
                    .unwrap_or(p)
                    .display()
                    .to_string(),
                None => p.display().to_string(),
            }
        };
        match &self.target {
            Some(t) => format!("{} -> {}", rel(&self.path), rel(t)),
            None => rel(&self.path),
        }
    }
}

/// The game's module for `umu_id` (`umu-910830`) in the Proton build at
/// `build`, read; `None` when the build has none (or only a dangling
/// symlink, which Python's import doesn't find either). An id that can't
/// be a module name (path separators, dots) has none.
pub fn read_module(build: &Path, umu_id: &str) -> std::io::Result<Option<Module>> {
    let valid = umu_id.starts_with("umu-")
        && umu_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !valid {
        return Ok(None);
    }
    read_named(build, umu_id)
}

/// `gamefixes-gog/default.py`: run for every GOG game before its own module.
pub fn read_default(build: &Path) -> std::io::Result<Option<Module>> {
    read_named(build, DEFAULT_MODULE)
}

fn read_named(build: &Path, name: &str) -> std::io::Result<Option<Module>> {
    let dir = build.join(GAMEFIXES_GOG);
    // A package wins over a module of the same name, as in Python's finder.
    let candidates = [
        dir.join(name).join("__init__.py"),
        dir.join(format!("{name}.py")),
    ];
    for path in candidates {
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotADirectory => continue,
            Err(e) => return Err(e),
        };
        let target = match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => std::fs::canonicalize(&path).ok(),
            _ => None,
        };
        let sha256 = {
            use sha2::{Digest as _, Sha256};
            Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        };
        return Ok(Some(Module {
            name: name.to_string(),
            path,
            target,
            sha256,
            reading: read_source(&String::from_utf8_lossy(&bytes)),
        }));
    }
    Ok(None)
}

// — the protonfix hash in `state.toml` —

/// `state.toml`'s `protonfix` for a build: the module's sha256, or
/// [`NO_MODULE`].
pub fn state_value(module: Option<&Module>) -> String {
    module.map_or_else(|| NO_MODULE.to_string(), |m| m.sha256.clone())
}

/// The protonfix now against what `state.toml` recorded (design 18 §4.2
/// step 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashChange {
    /// Nothing recorded yet (a new game, or a `state.toml` from before).
    Unrecorded {
        now: String,
    },
    Unchanged,
    /// The build has a module where it had none.
    Added {
        now: String,
    },
    Changed {
        was: String,
        now: String,
    },
    Removed {
        was: String,
    },
}

/// Compare `state.toml`'s `protonfix` (`stored`) with [`state_value`]'s.
pub fn compare(stored: Option<&str>, now: &str) -> HashChange {
    let now_s = now.to_string();
    match stored {
        None => HashChange::Unrecorded { now: now_s },
        Some(was) if was == now => HashChange::Unchanged,
        Some(NO_MODULE) => HashChange::Added { now: now_s },
        Some(was) if now == NO_MODULE => HashChange::Removed {
            was: was.to_string(),
        },
        Some(was) => HashChange::Changed {
            was: was.to_string(),
            now: now_s,
        },
    }
}

impl HashChange {
    /// Whether setup re-runs the summary and the verb pre-install: on any
    /// change, and when nothing was recorded.
    pub fn rerun(&self) -> bool {
        !matches!(self, HashChange::Unchanged)
    }
}

impl std::fmt::Display for HashChange {
    /// The line setup logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let short = |h: &str| -> String {
            if h == NO_MODULE {
                h.into()
            } else {
                format!("sha256 {}", &h[..h.len().min(12)])
            }
        };
        match self {
            HashChange::Unrecorded { now } => write!(f, "protonfix: {} (first record)", short(now)),
            HashChange::Unchanged => write!(f, "protonfix unchanged"),
            HashChange::Added { now } => {
                write!(f, "protonfix added by the Proton build ({})", short(now))
            }
            HashChange::Changed { was, now } => write!(
                f,
                "protonfix changed in the Proton build ({} -> {})",
                short(was),
                short(now)
            ),
            HashChange::Removed { was } => write!(
                f,
                "protonfix removed from the Proton build (was {})",
                short(was)
            ),
        }
    }
}

// — the reader —

/// Read a module's source: see the module docs.
pub fn read_source(src: &str) -> Reading {
    let lines = logical_lines(src);
    let top = blocks(&lines, 0, lines.len());

    // Pass 1, the module level: what names reach `util` (imports
    // anywhere), string constants, function bodies, and everything else as
    // other code.
    let mut names = Names::default();
    for l in &lines {
        names.import(&l.text);
    }
    let mut defs: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut out = Reading::default();
    for b in &top {
        let head = &lines[b.start];
        let text = head.text.as_str();
        if let Some(name) = def_name(text) {
            defs.insert(name.to_string(), (b.start + 1, b.end));
            continue;
        }
        if is_noop(text, &names) {
            continue;
        }
        match constant(text, &names) {
            Some((name, value)) => {
                names.consts.insert(name, value);
            }
            None => out.other.push(other(head)),
        }
    }
    // Pass 2: the entry functions, in the order protonfixes calls them.
    let entries = [["early_with_id", "early"], ["main_with_id", "main"]];
    for pair in entries {
        let Some(&(start, end)) = pair.iter().find_map(|n| defs.get(*n)) else {
            continue;
        };
        if start == end {
            // A one-line `def main(): …`: its body is on the header line.
            out.other.push(other(&lines[start - 1]));
            continue;
        }
        for b in blocks(&lines, start, end) {
            let head = &lines[b.start];
            if b.end - b.start == 1 {
                if is_noop(&head.text, &names) {
                    continue;
                }
                if let Some(fix) = util_call(&head.text, &names) {
                    out.fixes.push(fix);
                    continue;
                }
            }
            out.other.push(other(head));
        }
    }
    out.other.sort_by_key(|o| o.line);
    out
}

fn other(l: &Logical) -> OtherCode {
    let code = l.text.split_whitespace().collect::<Vec<_>>().join(" ");
    let code = match code.char_indices().nth(OTHER_CODE_WIDTH) {
        Some((i, _)) => format!("{}…", &code[..i]),
        None => code,
    };
    OtherCode { line: l.line, code }
}

/// One logical line of Python: a statement or a compound statement's
/// header, comments dropped, bracketed line breaks and `\` continuations
/// joined (string literals kept verbatim, newlines and all).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Logical {
    line: usize,
    indent: usize,
    text: String,
}

fn logical_lines(src: &str) -> Vec<Logical> {
    let c: Vec<char> = src.chars().filter(|&ch| ch != '\r').collect();
    let n = c.len();
    let mut out = Vec::new();
    let (mut i, mut line) = (0, 1);
    let (mut cur, mut depth, mut start, mut indent) = (String::new(), 0usize, 1, 0);
    let mut at_line_start = true;
    let mut finish = |cur: &mut String, start: usize, indent: usize| {
        let t = cur.trim();
        if !t.is_empty() {
            out.push(Logical {
                line: start,
                indent,
                text: t.to_string(),
            });
        }
        cur.clear();
    };
    while i < n {
        if at_line_start {
            let mut col = 0;
            while i < n && matches!(c[i], ' ' | '\t' | '\x0c') {
                col = match c[i] {
                    '\t' => (col / 8 + 1) * 8,
                    ' ' => col + 1,
                    _ => 0,
                };
                i += 1;
            }
            if i < n && c[i] == '#' {
                while i < n && c[i] != '\n' {
                    i += 1;
                }
            }
            if i < n && c[i] == '\n' {
                i += 1;
                line += 1;
                continue;
            }
            (indent, start, at_line_start) = (col, line, false);
            continue;
        }
        match c[i] {
            '#' => {
                while i < n && c[i] != '\n' {
                    i += 1;
                }
            }
            '\\' if c.get(i + 1) == Some(&'\n') => {
                cur.push(' ');
                i += 2;
                line += 1;
            }
            '\'' | '"' => {
                let end = string_end(&c, i);
                for &ch in &c[i..end] {
                    if ch == '\n' {
                        line += 1;
                    }
                    cur.push(ch);
                }
                i = end;
            }
            ch @ ('(' | '[' | '{') => {
                depth += 1;
                cur.push(ch);
                i += 1;
            }
            ch @ (')' | ']' | '}') => {
                depth = depth.saturating_sub(1);
                cur.push(ch);
                i += 1;
            }
            '\n' => {
                i += 1;
                line += 1;
                if depth > 0 {
                    cur.push(' ');
                } else {
                    finish(&mut cur, start, indent);
                    at_line_start = true;
                }
            }
            ';' if depth == 0 => {
                // `a; b`: two statements at the same indent.
                finish(&mut cur, start, indent);
                i += 1;
            }
            ch => {
                cur.push(ch);
                i += 1;
            }
        }
    }
    finish(&mut cur, start, indent);
    out
}

/// The index just past the string literal whose opening quote is at `i`
/// (single or triple quoted; a backslash always escapes the next char,
/// raw or not). An unterminated single-quoted string ends at the newline.
fn string_end(c: &[char], i: usize) -> usize {
    let q = c[i];
    let triple = c.get(i + 1) == Some(&q) && c.get(i + 2) == Some(&q);
    let mut j = if triple { i + 3 } else { i + 1 };
    while j < c.len() {
        match c[j] {
            '\\' => j += 2,
            ch if ch == q && !triple => return j + 1,
            ch if ch == q && c.get(j + 1) == Some(&q) && c.get(j + 2) == Some(&q) => {
                return j + 3;
            }
            '\n' if !triple => return j,
            _ => j += 1,
        }
    }
    c.len()
}

/// A statement and the deeper lines under it: `lines[start..end]`.
struct Block {
    start: usize,
    end: usize,
}

/// The statements at the indent of `lines[from]`, each with its block.
fn blocks(lines: &[Logical], from: usize, to: usize) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let Some(first) = lines.get(from) else {
        return out;
    };
    let indent = first.indent;
    for (i, l) in lines.iter().enumerate().take(to).skip(from) {
        match out.last_mut() {
            Some(b) if l.indent > indent => b.end = i + 1,
            _ => out.push(Block {
                start: i,
                end: i + 1,
            }),
        }
    }
    out
}

/// `def name(…):` → `name`.
fn def_name(text: &str) -> Option<&str> {
    let rest = text
        .strip_prefix("async def ")
        .or_else(|| text.strip_prefix("def "))?;
    let end = rest.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
    Some(&rest[..end])
}

/// What the module's imports and constants make of names.
#[derive(Default)]
struct Names {
    /// Names bound to `protonfixes.util` (`util`, or its alias).
    util: BTreeSet<String>,
    /// Local name → `util` function, from `from protonfixes.util import …`.
    direct: BTreeMap<String, String>,
    /// Names bound to protonfixes' logger (`log`).
    log: BTreeSet<String>,
    /// Module-level `NAME = '<literal>'`.
    consts: BTreeMap<String, String>,
}

impl Names {
    /// If `text` is an import, remember what it binds.
    fn import(&mut self, text: &str) {
        if let Some(rest) = text.strip_prefix("import ") {
            for item in rest.split(',') {
                let (module, alias) = as_alias(item);
                if module == "protonfixes.util"
                    && let Some(alias) = alias
                {
                    self.util.insert(alias.to_string());
                }
            }
            return;
        }
        let Some(rest) = text.strip_prefix("from ") else {
            return;
        };
        let Some((module, items)) = rest.split_once(" import ") else {
            return;
        };
        let items = items.trim().trim_start_matches('(').trim_end_matches(')');
        for item in items.split(',').filter(|s| !s.trim().is_empty()) {
            let (name, alias) = as_alias(item);
            let local = alias.unwrap_or(name).to_string();
            match (module.trim(), name) {
                ("protonfixes", "util") => {
                    self.util.insert(local);
                }
                ("protonfixes.util", _) => {
                    self.direct.insert(local, name.to_string());
                }
                ("protonfixes.logger", "log") => {
                    self.log.insert(local);
                }
                _ => {}
            }
        }
    }

    /// `util.<f>` / a directly imported `<f>` → `f`.
    fn util_function<'a>(&'a self, callee: &'a str) -> Option<&'a str> {
        if let Some((obj, f)) = callee.split_once('.') {
            return self.util.contains(obj).then_some(f);
        }
        self.direct.get(callee).map(String::as_str)
    }
}

/// `a as b` → (`a`, Some(`b`)).
fn as_alias(item: &str) -> (&str, Option<&str>) {
    match item.trim().split_once(" as ") {
        Some((a, b)) => (a.trim(), Some(b.trim())),
        None => (item.trim(), None),
    }
}

/// A statement that does nothing at launch: an import, `pass`, a
/// docstring or other bare string, a call on protonfixes' logger.
fn is_noop(text: &str, names: &Names) -> bool {
    let import =
        text.starts_with("import ") || (text.starts_with("from ") && text.contains(" import "));
    if import || text == "pass" || matches!(literal(text, names), Some(Lit::Str(_))) {
        return true;
    }
    match call(text) {
        Some((callee, _)) => callee
            .split_once('.')
            .is_some_and(|(obj, _)| names.log.contains(obj)),
        None => false,
    }
}

/// Module-level `NAME = '<string>'` (or `NAME: str = …`).
fn constant(text: &str, names: &Names) -> Option<(String, String)> {
    let (lhs, rhs) = split_assign(text)?;
    let name = lhs.split(':').next()?.trim();
    if !is_ident(name) {
        return None;
    }
    match literal(rhs, names)? {
        Lit::Str(s) => Some((name.to_string(), s)),
        _ => None,
    }
}

/// `lhs = rhs` at depth 0, not `==` / `<=` / augmented assignment.
fn split_assign(text: &str) -> Option<(&str, &str)> {
    let b = text.as_bytes();
    for (i, ch, depth) in scan(text) {
        if ch == '=' && depth == 0 {
            let prev = i.checked_sub(1).map(|p| b[p]);
            let next = b.get(i + 1).copied();
            if next == Some(b'=') || prev.is_some_and(|p| b"=!<>+-*/%&|^@:".contains(&p)) {
                return None;
            }
            return Some((&text[..i], &text[i + 1..]));
        }
    }
    None
}

fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && cs.all(|c| c.is_alphanumeric() || c == '_')
}

/// The chars of `text` outside string literals, with their byte index and
/// bracket depth (before the char).
fn scan(text: &str) -> Vec<(usize, char, usize)> {
    let idx: Vec<(usize, char)> = text.char_indices().collect();
    let chars: Vec<char> = idx.iter().map(|(_, c)| *c).collect();
    let mut out = Vec::new();
    let (mut i, mut depth) = (0, 0usize);
    while i < chars.len() {
        let ch = chars[i];
        if ch == '\'' || ch == '"' {
            i = string_end(&chars, i);
            continue;
        }
        out.push((idx[i].0, ch, depth));
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }
    out
}

/// `callee(args)` filling the whole statement → (`callee`, `args`).
fn call(text: &str) -> Option<(&str, &str)> {
    let end = text
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
        .unwrap_or(text.len());
    let callee = &text[..end];
    if callee.is_empty() || !text[end..].trim_start().starts_with('(') {
        return None;
    }
    let open = end + text[end..].find('(')?;
    let close = scan(text)
        .into_iter()
        .find(|&(i, ch, depth)| i > open && ch == ')' && depth == 1)?
        .0;
    if !text[close + 1..].trim().is_empty() {
        return None;
    }
    Some((callee, &text[open + 1..close]))
}

/// A literal argument.
#[derive(Debug, Clone, PartialEq)]
enum Lit {
    Str(String),
    /// `OverrideOrder.NATIVE`, `BasePath.GAME` (optionally `util.`-qualified).
    Enum(&'static str, String),
    Bool(bool),
    None,
}

/// The enums of `util.py` the calls take, and their members' values.
const ENUMS: &[(&str, &[(&str, &str)])] = &[
    (
        "OverrideOrder",
        &[
            ("DISABLED", ""),
            ("NATIVE", "n"),
            ("BUILTIN", "b"),
            ("NATIVE_BUILTIN", "n,b"),
            ("BUILTIN_NATIVE", "b,n"),
        ],
    ),
    (
        "BasePath",
        &[("GAME", "game"), ("APPDATA_LOCAL", "appdata_local")],
    ),
];

fn enum_value(ty: &str, member: &str) -> Option<&'static str> {
    ENUMS
        .iter()
        .find(|(t, _)| *t == ty)?
        .1
        .iter()
        .find(|(m, _)| *m == member)
        .map(|(_, v)| *v)
}

/// A literal expression: string literals (adjacent ones concatenated,
/// f-strings without fields), a module string constant, `True` / `False` /
/// `None`, a `util.py` enum member. Anything else is computed: `None`.
fn literal(expr: &str, names: &Names) -> Option<Lit> {
    let mut e = expr.trim();
    // Redundant parentheses around the whole expression.
    while e.starts_with('(')
        && e.ends_with(')')
        && scan(e)
            .iter()
            .find(|&&(_, ch, d)| ch == ')' && d == 1)
            .is_some_and(|&(i, _, _)| i == e.len() - 1)
    {
        e = e[1..e.len() - 1].trim();
    }
    match e {
        "True" => return Some(Lit::Bool(true)),
        "False" => return Some(Lit::Bool(false)),
        "None" => return Some(Lit::None),
        _ => {}
    }
    if e.chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        && !e.is_empty()
    {
        if let Some(v) = names.consts.get(e) {
            return Some(Lit::Str(v.clone()));
        }
        let parts: Vec<&str> = e.split('.').collect();
        let (ty, member) = match parts.as_slice() {
            [ty, m] => (*ty, *m),
            [obj, ty, m] if names.util.contains(*obj) => (*ty, *m),
            _ => return None,
        };
        let (ty, _) = ENUMS.iter().find(|(t, _)| *t == ty)?;
        enum_value(ty, member)?;
        return Some(Lit::Enum(ty, member.to_string()));
    }
    strings(e).map(Lit::Str)
}

/// One or more adjacent string literals, decoded and concatenated.
fn strings(e: &str) -> Option<String> {
    let c: Vec<char> = e.chars().collect();
    let mut i = 0;
    let mut out = String::new();
    let mut any = false;
    while i < c.len() {
        if c[i].is_whitespace() {
            i += 1;
            continue;
        }
        let p0 = i;
        while i < c.len() && c[i].is_ascii_alphabetic() {
            i += 1;
        }
        let prefix: String = c[p0..i].iter().collect::<String>().to_ascii_lowercase();
        if !matches!(prefix.as_str(), "" | "r" | "u" | "f" | "rf" | "fr") {
            return None; // bytes, or not a string at all
        }
        if i >= c.len() || !(c[i] == '\'' || c[i] == '"') {
            return None;
        }
        let end = string_end(&c, i);
        let q = c[i];
        let triple = c.get(i + 1) == Some(&q) && c.get(i + 2) == Some(&q);
        let qlen = if triple { 3 } else { 1 };
        if end < i + 2 * qlen || c[end - 1] != q {
            return None; // unterminated
        }
        let body: String = c[i + qlen..end - qlen].iter().collect();
        if prefix.contains('f')
            && body
                .replace("{{", "")
                .replace("}}", "")
                .contains(['{', '}'])
        {
            return None; // an f-string with fields: computed
        }
        let body = if prefix.contains('f') {
            body.replace("{{", "{").replace("}}", "}")
        } else {
            body
        };
        out.push_str(&if prefix.contains('r') {
            body
        } else {
            unescape(&body)
        });
        any = true;
        i = end;
    }
    any.then_some(out)
}

fn hex(it: &mut std::str::Chars<'_>, n: usize) -> Option<char> {
    let h: String = (0..n).filter_map(|_| it.next()).collect();
    u32::from_str_radix(&h, 16).ok().and_then(char::from_u32)
}

/// Python's escapes in a non-raw string. Unknown ones keep the backslash,
/// as Python does.
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(ch) = it.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        let Some(e) = it.next() else {
            out.push('\\');
            break;
        };
        match e {
            '\n' => {}
            '\\' | '\'' | '"' => out.push(e),
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            '0' => out.push('\0'),
            'a' => out.push('\x07'),
            'b' => out.push('\x08'),
            'f' => out.push('\x0c'),
            'v' => out.push('\x0b'),
            'x' => out.extend(hex(&mut it, 2)),
            'u' => out.extend(hex(&mut it, 4)),
            'U' => out.extend(hex(&mut it, 8)),
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    out
}

/// A known `util` call with literal arguments → its [`Fix`].
fn util_call(text: &str, names: &Names) -> Option<Fix> {
    let (callee, args) = call(text)?;
    let f = names.util_function(callee)?;
    let params: &[&str] = match f {
        "protontricks" => &["verb"],
        "set_environment" => &["envvar", "value"],
        "winedll_override" => &["dll", "dtype"],
        "regedit_add" => &["folder", "name", "typ", "value", "arch"],
        "replace_command" => &["orig", "repl", "match_flags"],
        "append_argument" => &["argument"],
        "set_ini_options" => &["ini_opts", "cfile", "encoding", "base_path"],
        "set_xml_options" => &[
            "base_attibutte",
            "xml_line",
            "cfile",
            "encoding",
            "base_path",
        ],
        _ => return None,
    };
    let a = bind(args, params, names)?;
    let s = |k: &str| match a.get(k) {
        Some(Lit::Str(s)) => Some(s.clone()),
        _ => None,
    };
    let opt_s = |k: &str| match a.get(k) {
        None | Some(Lit::None) => Some(None),
        Some(Lit::Str(s)) => Some(Some(s.clone())),
        _ => None,
    };
    let base = || match a.get("base_path") {
        None => Some(None),
        Some(Lit::Enum("BasePath", m)) => {
            let v = enum_value("BasePath", m)?;
            Some((v != "game").then(|| v.to_string()))
        }
        _ => None,
    };
    // `encoding` changes nothing the summary shows, but must be literal.
    if a.contains_key("encoding") {
        s("encoding")?;
    }
    Some(match f {
        "protontricks" => Fix::Verb(s("verb")?),
        "set_environment" => Fix::Env {
            key: s("envvar")?,
            value: s("value")?,
        },
        "winedll_override" => match a.get("dtype")? {
            Lit::Enum("OverrideOrder", m) => Fix::DllOverride {
                dll: s("dll")?,
                mode: enum_value("OverrideOrder", m)?.to_string(),
            },
            _ => return None,
        },
        "regedit_add" => {
            let (name, kind, data) = (opt_s("name")?, opt_s("typ")?, opt_s("value")?);
            let arch = match a.get("arch") {
                None => false,
                Some(Lit::Bool(b)) => *b,
                _ => return None,
            };
            let reg64_key_only = name.is_some();
            match (name, kind, data) {
                (Some(name), Some(kind), Some(data)) => Fix::Registry {
                    key: s("folder")?,
                    value: Some(RegValue { name, kind, data }),
                    reg64: arch,
                },
                _ => Fix::Registry {
                    key: s("folder")?,
                    value: None,
                    reg64: reg64_key_only,
                },
            }
        }
        "replace_command" => {
            if a.contains_key("match_flags") {
                return None; // a regex flag expression: not read
            }
            Fix::ReplaceCommand {
                from: s("orig")?,
                to: s("repl")?,
            }
        }
        "append_argument" => Fix::AppendArgument(s("argument")?),
        "set_ini_options" => Fix::IniOptions {
            options: s("ini_opts")?,
            file: s("cfile")?,
            base: base()?,
        },
        "set_xml_options" => Fix::XmlOptions {
            anchor: s("base_attibutte")?,
            line: s("xml_line")?,
            file: s("cfile")?,
            base: base()?,
        },
        _ => return None,
    })
}

/// Bind a call's arguments to `params` (positional, then keywords), each a
/// [`literal`]. `None` when any is computed, unknown, doubled, or `*`.
fn bind(args: &str, params: &[&str], names: &Names) -> Option<BTreeMap<String, Lit>> {
    let mut pieces = Vec::new();
    let mut last = 0;
    for (i, ch, depth) in scan(args) {
        if ch == ',' && depth == 0 {
            pieces.push(&args[last..i]);
            last = i + 1;
        }
    }
    pieces.push(&args[last..]);
    if pieces.last().is_some_and(|p| p.trim().is_empty()) {
        pieces.pop(); // a trailing comma, or no arguments
    }
    let mut out = BTreeMap::new();
    let mut keywords = false;
    for (pos, piece) in pieces.iter().enumerate() {
        let piece = piece.trim();
        if piece.is_empty() || piece.starts_with('*') {
            return None;
        }
        let (name, expr) = match split_assign(piece) {
            Some((k, v)) if is_ident(k.trim()) => {
                keywords = true;
                (k.trim(), v)
            }
            _ if keywords => return None,
            _ => (*params.get(pos)?, piece),
        };
        if !params.contains(&name) || out.contains_key(name) {
            return None;
        }
        out.insert(name.to_string(), literal(expr, names)?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixes(src: &str) -> Vec<Fix> {
        let r = read_source(src);
        assert!(r.other.is_empty(), "{:?}", r.other);
        r.fixes
    }

    // Synthesized modules in the shape of umu-protonfixes' (BSD-2-Clause;
    // none copied, docs/21 §2.3).
    const EVERY_KIND: &str = r#""""Game fix for Some Game"""

from protonfixes import util


def main() -> None:
    """Installs a verb, sets env and overrides"""
    util.protontricks('vcrun2019')
    util.set_environment('DXVK_ASYNC', "1")
    util.winedll_override('d3d8', util.OverrideOrder.NATIVE_BUILTIN)
    util.winedll_override(
        '*dsound', util.OverrideOrder.BUILTIN
    )  # comment after the call
    util.winedll_override('nvapi', util.OverrideOrder.DISABLED)
    util.regedit_add('HKLM\\Software\\Wow6432Node\\Some Vendor')
    util.regedit_add(
        'HKEY_CURRENT_USER\\Software\\Wine',
        'Version',
        'REG_SZ',
        'win98',
    )
    util.regedit_add('HKCU\\Software\\Game', 'Lang', 'REG_DWORD', '1', arch=True)
    util.replace_command('Launcher.exe', 'Game.exe')
    util.append_argument('-windowed')
    util.set_ini_options(
        """
        [GAME]
        scaleVideos = 1
        [VIDEO]
        bpp=32
        """,
        'system/Game.ini',
        'cp1251',
    )
    util.set_xml_options('<Settings', '  <Fast>true</Fast>', 'Bin/Game.exe.config')
    util.set_ini_options('[x]\na=1', 'Game/Engine.ini', base_path=util.BasePath.APPDATA_LOCAL)
"#;

    #[test]
    fn reads_every_call_kind() {
        let f = fixes(EVERY_KIND);
        let shown: Vec<String> = f.iter().map(Fix::to_string).collect();
        assert_eq!(
            shown,
            [
                "verb vcrun2019",
                r#"env DXVK_ASYNC = "1""#,
                r#"dll_overrides d3d8 = "n,b""#,
                r#"dll_overrides *dsound = "b""#,
                r#"dll_overrides nvapi = "" (disabled)"#,
                r"registry HKLM\Software\Wow6432Node\Some Vendor (key only)",
                r#"registry HKEY_CURRENT_USER\Software\Wine Version = REG_SZ "win98""#,
                r#"registry HKCU\Software\Game Lang = REG_DWORD "1" (64-bit view)"#,
                r#"replace_command "Launcher.exe" -> "Game.exe""#,
                r#"args += "-windowed""#,
                "ini system/Game.ini: [GAME] scaleVideos=1; [VIDEO] bpp=32",
                r#"xml Bin/Game.exe.config: "  <Fast>true</Fast>" after each line containing "<Settings""#,
                "ini Game/Engine.ini (in appdata_local): [x] a=1",
            ]
        );
        assert_eq!(f[0].verb(), Some("vcrun2019"));
        assert_eq!(f[1].key().as_deref(), Some("env DXVK_ASYNC"));
    }

    #[test]
    fn early_runs_before_main_and_main_with_id_replaces_main() {
        let src = "from protonfixes import util\n\n\
                   def main():\n    util.protontricks('ignored')\n\n\
                   def main_with_id(game_id):\n    util.protontricks('used')\n\n\
                   def early():\n    util.set_environment('A', '1')\n";
        assert_eq!(
            fixes(src),
            [
                Fix::Env {
                    key: "A".into(),
                    value: "1".into()
                },
                Fix::Verb("used".into())
            ]
        );
    }

    #[test]
    fn computed_arguments_and_conditionals_are_other_code() {
        let src = r#"
"""Doc"""
import os
from protonfixes import util
from protonfixes.logger import log

URL = 'https://example.invalid/fix.zip'
TABLE: dict[str, str] = {'a': 'b'}


def main_with_id(game_id: str) -> None:
    log.info('fixing')
    util.protontricks('d3dx9')
    exe = TABLE.get(game_id)
    util.replace_command(f'{exe}.exe', f'{exe}_og.exe')
    if not os.path.isfile('x'):
        util.protontricks('xact')
    for i in range(2):
        util.set_environment('B', str(i))
    util.disable_esync()
    helper()
    util.set_environment('URL', URL)
    util.protontricks(verb='corefonts')


def helper() -> None:
    util.protontricks('never_read')
"#;
        let r = read_source(src);
        assert_eq!(
            r.fixes,
            [
                Fix::Verb("d3dx9".into()),
                Fix::Env {
                    key: "URL".into(),
                    value: "https://example.invalid/fix.zip".into()
                },
                Fix::Verb("corefonts".into()),
            ]
        );
        let other: Vec<String> = r.other.iter().map(ToString::to_string).collect();
        assert_eq!(
            other,
            [
                "line 8: TABLE: dict[str, str] = {'a': 'b'}",
                "line 14: exe = TABLE.get(game_id)",
                "line 15: util.replace_command(f'{exe}.exe', f'{exe}_og.exe')",
                "line 16: if not os.path.isfile('x'):",
                "line 18: for i in range(2):",
                "line 20: util.disable_esync()",
                "line 21: helper()",
            ]
        );
    }

    #[test]
    fn lexing_edge_cases() {
        // `#` and brackets inside strings, `\` continuations, `;`, escapes,
        // implicit concatenation, aliases, a call inside an expression.
        let src = "from protonfixes import util as u\n\
                   from protonfixes.util import protontricks as pt\n\
                   def main():\n\
                   \tu.set_environment('A#(', 'x' \\\n\t  \"y\\tz\")\n\
                   \tpt('verb_a'); u.append_argument(r'-p\\q')\n\
                   \tok = u.protontricks('in_expr')\n\
                   \tu.set_environment('K', 1)\n";
        let r = read_source(src);
        assert_eq!(
            r.fixes,
            [
                Fix::Env {
                    key: "A#(".into(),
                    value: "xy\tz".into()
                },
                Fix::Verb("verb_a".into()),
                Fix::AppendArgument(r"-p\q".into()),
            ]
        );
        assert_eq!(r.other.len(), 2, "{:?}", r.other);
    }

    #[test]
    fn default_py_shape() {
        let src = "\"\"\"Setup for all GOG games\"\"\"\n\nfrom protonfixes import util\n\n\n\
                   def main() -> None:\n    util.set_environment('OPENSSL_ia32cap', '~0x20000000')\n";
        assert_eq!(
            fixes(src),
            [Fix::Env {
                key: "OPENSSL_ia32cap".into(),
                value: "~0x20000000".into()
            }]
        );
    }

    #[test]
    fn hash_changes() {
        assert_eq!(
            compare(None, "abc"),
            HashChange::Unrecorded { now: "abc".into() }
        );
        assert_eq!(compare(Some("abc"), "abc"), HashChange::Unchanged);
        assert!(!compare(Some("none"), "none").rerun());
        assert_eq!(
            compare(Some("none"), "abc"),
            HashChange::Added { now: "abc".into() }
        );
        assert_eq!(
            compare(Some("abc"), "none"),
            HashChange::Removed { was: "abc".into() }
        );
        let c = compare(Some("aaaaaaaaaaaaaaaa"), "bbbbbbbbbbbbbbbb");
        assert!(c.rerun());
        assert_eq!(
            c.to_string(),
            "protonfix changed in the Proton build (sha256 aaaaaaaaaaaa -> sha256 bbbbbbbbbbbb)"
        );
    }

    fn build_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        for (name, body) in files {
            let p = t.path().join("protonfixes").join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        t
    }

    #[test]
    fn missing_module_is_none_and_hash_none() {
        let t = build_with(&[("gamefixes-gog/default.py", "")]);
        assert_eq!(read_module(t.path(), "umu-1").unwrap(), None);
        assert_eq!(read_module(t.path(), "umu-../../x").unwrap(), None);
        assert_eq!(state_value(None), "none");
        assert!(read_default(t.path()).unwrap().is_some());
        // A build without protonfixes/ at all.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(read_default(empty.path()).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_module_is_followed_and_hashed() {
        let src =
            "from protonfixes import util\ndef main():\n    util.protontricks('directplay')\n";
        let t = build_with(&[("gamefixes-steam/40950.py", src)]);
        let gog = t.path().join(GAMEFIXES_GOG);
        std::fs::create_dir_all(&gog).unwrap();
        std::os::unix::fs::symlink("../gamefixes-steam/40950.py", gog.join("umu-40950.py"))
            .unwrap();
        std::os::unix::fs::symlink("../gamefixes-steam/gone.py", gog.join("umu-2.py")).unwrap();

        let m = read_module(t.path(), "umu-40950").unwrap().unwrap();
        assert_eq!(m.reading.fixes, [Fix::Verb("directplay".into())]);
        assert!(
            m.target
                .as_ref()
                .unwrap()
                .ends_with("gamefixes-steam/40950.py")
        );
        assert_eq!(
            m.describe(),
            "gamefixes-gog/umu-40950.py -> gamefixes-steam/40950.py"
        );
        assert_eq!(m.sha256.len(), 64);
        assert_eq!(state_value(Some(&m)), m.sha256);
        // A dangling link: Python's import finds nothing either.
        assert_eq!(read_module(t.path(), "umu-2").unwrap(), None);

        // Edit the target: the hash notices.
        std::fs::write(
            t.path().join("protonfixes/gamefixes-steam/40950.py"),
            format!("{src}    util.protontricks('xact')\n"),
        )
        .unwrap();
        let m2 = read_module(t.path(), "umu-40950").unwrap().unwrap();
        assert!(matches!(
            compare(Some(&m.sha256), &state_value(Some(&m2))),
            HashChange::Changed { .. }
        ));
    }
}
