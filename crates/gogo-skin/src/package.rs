//! Loading a skin package (design 27 §1, §8, §9; S10, S11, S12).
//!
//! [`SkinPackage::load`] reads a folder, [`SkinPackage::from_files`] takes the files of a
//! zip as an in-memory list. Both go through one path: every file is admitted by its path
//! and size, the assets are checked, the KDL is parsed into the skin's own AST, and the
//! manifest, tokens, styles, components and routes are validated against the format and
//! the contract.
//!
//! What fails what: errors in `skin.kdl`, `tokens.kdl`, `styles.kdl`, a component or an
//! asset, and every limit, fail the skin ([`Level::Skin`](crate::Level)); an error in
//! `routes/<name>.kdl` fails that route only. Warnings fail nothing.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use crate::assets::{self, Asset, AssetError, AssetKind};
use crate::ast::{self, Node};
use crate::diag::{Code, Collector, Finding, LoadScope, Report};
use crate::elements::{Place, Sig, Walk, World};
use crate::limits::{
    MAX_DEPTH, MAX_DIR_DEPTH, MAX_FILES, MAX_KDL_FILE_BYTES, MAX_LICENCE_BYTES, MAX_NODES,
    MAX_PATH_BYTES, MAX_SKIN_BYTES,
};
use crate::manifest::{self, Manifest, Mode};
use crate::names::{is_ident, is_style_name, suggest};
use crate::props::{Ctx, Dynamic};
use crate::raw::{Counter, Lit, RawNode};
use crate::resolve;
use crate::source::{Sources, Span};
use crate::styles::{self, Style};
use crate::tokens::{self, TokenSet};

/// A component (design 27 §5.7): a named, parameterised piece of tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Component {
    pub name: String,
    /// The declared parameters, in order.
    pub params: Vec<String>,
    pub body: Vec<Node>,
    pub span: Span,
}

/// A route a skin covers: the elements of `routes/<name>.kdl`.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub name: String,
    /// The contract's route of that name.
    pub spec: &'static gogo_contract::RouteSpec,
    pub children: Vec<Node>,
    pub span: Span,
}

/// A loaded skin: validated, with every template and expression parsed.
///
/// Routes that failed validation are not in [`routes`](Self::routes); the
/// [`report`](Self::report) says which and why.
#[derive(Debug, Clone)]
pub struct SkinPackage {
    pub manifest: Manifest,
    /// The tokens of each mode the manifest lists, references resolved.
    pub tokens: BTreeMap<Mode, TokenSet>,
    pub styles: BTreeMap<String, Style>,
    pub components: BTreeMap<String, Component>,
    /// The routes that loaded, by name.
    pub routes: BTreeMap<String, Route>,
    /// The checked asset files, by path from the skin root (`assets/logo.png`).
    pub assets: BTreeMap<String, Asset>,
    sources: Sources,
    report: Report,
    /// What binding derives from the files once and keeps: the tokens as values.
    tables: OnceLock<crate::bind::Tables>,
    /// What checking a fragment of the default skin against this skin's manifest found,
    /// by fragment name: worked out once, the first time a route borrows it.
    fragments: FragmentCache,
}

/// The errors of each borrowed fragment against one skin's manifest (design 27 §5.7).
/// A copy of a package starts with none.
#[derive(Default)]
pub(crate) struct FragmentCache(Mutex<BTreeMap<String, Arc<Vec<Finding>>>>);

impl Clone for FragmentCache {
    fn clone(&self) -> Self {
        FragmentCache::default()
    }
}

impl std::fmt::Debug for FragmentCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FragmentCache")
    }
}

impl FragmentCache {
    /// The findings for `name`, from `check` the first time.
    pub(crate) fn get_or_check(
        &self,
        name: &str,
        check: impl FnOnce() -> Vec<Finding>,
    ) -> Arc<Vec<Finding>> {
        if let Some(found) = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
        {
            return Arc::clone(found);
        }
        let found = Arc::new(check());
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.to_owned(), Arc::clone(&found));
        found
    }
}

impl SkinPackage {
    /// Load the skin in folder `dir`. The folder's name is the skin's name.
    ///
    /// `Err` carries the findings when the skin fails to load (S12): the app then uses the
    /// default skin. `Ok` can still carry route errors and warnings in its
    /// [`report`](Self::report).
    pub fn load(dir: &Path) -> Result<SkinPackage, Report> {
        let mut intake = Intake::default();
        let name = dir
            .file_name()
            .map(std::ffi::OsStr::to_owned)
            .or_else(|| {
                dir.canonicalize()
                    .ok()?
                    .file_name()
                    .map(std::ffi::OsStr::to_owned)
            })
            .and_then(|n| n.to_str().map(str::to_owned));
        let Some(name) = name else {
            intake
                .col
                .path_error(Code::Io, "", "the skin folder has no usable name");
            return Err(intake.col.report);
        };
        read_dir(&mut intake, dir, "", 0);
        finish(intake, &name)
    }

    /// Load a skin from files in memory: relative paths with `/` separators and their
    /// bytes, as read out of a zip. `name` is the skin's name, which `skin.kdl` must
    /// repeat. The same checks as [`load`](Self::load).
    pub fn from_files(
        name: &str,
        files: impl IntoIterator<Item = (String, Vec<u8>)>,
    ) -> Result<SkinPackage, Report> {
        let mut intake = Intake::default();
        for (path, data) in files {
            if let Some(slot) = intake.admit(&path, data.len() as u64) {
                intake.add(path, slot, data);
            }
        }
        finish(intake, name)
    }

    /// The findings of the load that did not fail the skin: route errors and warnings.
    pub fn report(&self) -> &Report {
        &self.report
    }

    /// The KDL files the spans of the tree point into.
    pub fn sources(&self) -> &Sources {
        &self.sources
    }

    pub(crate) fn fragments(&self) -> &FragmentCache {
        &self.fragments
    }

    pub(crate) fn tables(&self) -> &crate::bind::Tables {
        self.tables
            .get_or_init(|| crate::bind::Tables::of(&self.tokens))
    }
}

// ---- paths ----

/// Check a path relative to the skin root (S10): no `..`, no absolute path, no backslash.
/// The error says what is wrong.
pub(crate) fn check_rel_path(path: &str) -> Result<(), &'static str> {
    if path.is_empty() {
        return Err("is empty");
    }
    if path.len() > MAX_PATH_BYTES {
        return Err("is too long");
    }
    if path.contains('\\') {
        return Err("uses a backslash");
    }
    if path.starts_with('/') {
        return Err("is absolute");
    }
    if path.contains(':') {
        return Err("contains `:`");
    }
    if path.chars().any(char::is_control) {
        return Err("contains a control character");
    }
    for part in path.split('/') {
        match part {
            "" => return Err("has an empty component"),
            "." => return Err("has a `.` component"),
            ".." => return Err("goes up with `..`"),
            _ => {}
        }
    }
    Ok(())
}

/// Where a file sits in the layout (design 27 §1).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Slot {
    Manifest,
    Tokens,
    Styles,
    Component,
    Route(String),
    Asset(AssetKind),
    /// A plain-text licence file (design 27 §1): kept out of every parser.
    Licence,
}

impl Slot {
    fn max_bytes(&self) -> usize {
        match self {
            Slot::Asset(k) => k.max_bytes(),
            Slot::Licence => MAX_LICENCE_BYTES,
            _ => MAX_KDL_FILE_BYTES,
        }
    }
}

/// `LICENSE*` or `LICENCE*` (any extension but a skin file type, or none) and `*.txt`,
/// whatever the case. `license.png` or `licenses.svg` is an asset, not a licence text.
fn is_licence_name(file: &str) -> bool {
    let lower = file.to_ascii_lowercase();
    if lower.ends_with(".txt") {
        return true;
    }
    if !(lower.starts_with("license") || lower.starts_with("licence")) {
        return false;
    }
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or_default();
    !matches!(ext, "kdl" | "png" | "svg" | "otf" | "ttf")
}

const LAYOUT_HELP: &str = "a skin holds skin.kdl, tokens.kdl, styles.kdl, components/*.kdl, routes/*.kdl and assets/ (design 27 §1)";

fn classify(path: &str) -> Result<Slot, String> {
    match path {
        "skin.kdl" => return Ok(Slot::Manifest),
        "tokens.kdl" => return Ok(Slot::Tokens),
        "styles.kdl" => return Ok(Slot::Styles),
        _ => {}
    }
    // Licence files sit at the root or in `assets/`, at any depth below it.
    if let Some((dir, file)) = path.rsplit_once('/').or(Some(("", path)))
        && (dir.is_empty() || dir == "assets" || dir.starts_with("assets/"))
        && is_licence_name(file)
    {
        return Ok(Slot::Licence);
    }
    let ext = path
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    if !matches!(ext.as_str(), "kdl" | "png" | "svg" | "otf" | "ttf") {
        return Err(format!(
            "`{path}`: only KDL, PNG, SVG, OTF and TTF files are allowed (S10)"
        ));
    }
    let parts: Vec<&str> = path.split('/').collect();
    match (parts.as_slice(), ext.as_str()) {
        (["components", f], "kdl") if f.ends_with(".kdl") && f.len() > 4 => Ok(Slot::Component),
        (["routes", f], "kdl") if f.ends_with(".kdl") && f.len() > 4 => {
            Ok(Slot::Route(f[..f.len() - 4].to_owned()))
        }
        (["assets", .., _], "png" | "svg" | "otf" | "ttf") => AssetKind::from_path(path)
            .map(Slot::Asset)
            .ok_or_else(|| format!("`{path}` is not an asset type")),
        _ => Err(format!("`{path}` is not part of the skin layout")),
    }
}

/// Whether `name` is a folder the layout has at the root of a skin.
pub(crate) fn is_layout_dir(name: &str) -> bool {
    matches!(name, "components" | "routes" | "assets")
}

/// How large the file at `path` may be, or why a skin can't hold it at all.
pub(crate) fn file_limit(path: &str) -> Result<usize, String> {
    classify(path).map(|slot| slot.max_bytes())
}

/// What an operating system adds to folders and archives: never part of a skin, and passed
/// over by an import (design 27 §10).
pub(crate) fn is_junk(name: &str) -> bool {
    matches!(name, "__MACOSX" | ".DS_Store")
}

/// The files of the skin folder `dir` as [`SkinPackage::load`] admits them, for an import,
/// which also passes over [`is_junk`]. `Err` when any file or folder is refused.
pub(crate) fn read_folder(dir: &Path) -> Result<Vec<(String, Vec<u8>)>, Report> {
    let mut intake = Intake {
        skip_junk: true,
        ..Intake::default()
    };
    read_dir(&mut intake, dir, "", 0);
    if intake.col.report.error_total() > 0 {
        return Err(intake.col.report);
    }
    Ok(intake
        .files
        .into_iter()
        .map(|(path, _, data)| (path, data))
        .collect())
}

/// The name `skin.kdl` gives, unchecked: what to load the package as when no folder names
/// it. `None` when there is none to find; loading then says what is wrong.
pub(crate) fn declared_name(skin_kdl: &[u8]) -> Option<String> {
    if skin_kdl.len() > MAX_KDL_FILE_BYTES {
        return None;
    }
    let text = std::str::from_utf8(skin_kdl).ok()?;
    let mut col = Collector::default();
    let file = col.sources.add("skin.kdl", text);
    let nodes = crate::raw::parse_file(&mut col, file, &mut Counter::default())?;
    let skin = nodes.iter().find(|n| n.name == "skin")?;
    match &skin.args.first()?.lit {
        Lit::Str(name) => Some(name.clone()),
        _ => None,
    }
}

// ---- intake ----

#[derive(Default)]
struct Intake {
    col: Collector,
    files: Vec<(String, Slot, Vec<u8>)>,
    seen: HashSet<String>,
    total: u64,
    stopped: bool,
    /// Paths refused or unreadable, already reported: not reported again as missing.
    refused: HashSet<String>,
    /// Pass over what an operating system leaves in folders ([`is_junk`]): an import does.
    skip_junk: bool,
}

impl Intake {
    /// Decide whether a file of `size` bytes at `path` is part of the package. Reports
    /// why not.
    fn admit(&mut self, path: &str, size: u64) -> Option<Slot> {
        let slot = self.admit_checked(path, size);
        if slot.is_none() {
            self.refused.insert(path.to_owned());
        }
        slot
    }

    fn admit_checked(&mut self, path: &str, size: u64) -> Option<Slot> {
        if self.stopped {
            return None;
        }
        if self.files.len() >= MAX_FILES {
            self.stopped = true;
            self.col
                .path_error(
                    Code::LimitFiles,
                    path,
                    format!("more than {MAX_FILES} files in the skin (S12)"),
                )
                .help("a skin is a few KDL files and a handful of assets");
            return None;
        }
        if let Err(why) = check_rel_path(path) {
            self.col
                .path_error(Code::PathInvalid, path, format!("`{path}` {why} (S10)"))
                .help("paths are relative to the skin folder and use `/`");
            return None;
        }
        if !self.seen.insert(path.to_ascii_lowercase()) {
            self.col.path_error(
                Code::DuplicatePath,
                path,
                format!("`{path}` appears twice, or twice differing only in ASCII case"),
            );
            return None;
        }
        let slot = match classify(path) {
            Ok(s) => s,
            Err(msg) => {
                self.col
                    .path_error(Code::FileType, path, msg)
                    .help(LAYOUT_HELP);
                return None;
            }
        };
        let limit = slot.max_bytes();
        if size > limit as u64 {
            self.col
                .path_error(
                    Code::LimitFile,
                    path,
                    format!(
                        "`{path}` is {size} bytes; the limit for this kind of file is {limit} (S12)"
                    ),
                )
                .help("design 27 §8 lists the limits");
            return None;
        }
        self.total = self.total.saturating_add(size);
        if self.total > MAX_SKIN_BYTES as u64 {
            self.stopped = true;
            self.col.path_error(
                Code::LimitFolder,
                path,
                format!("the skin is larger than {MAX_SKIN_BYTES} bytes (S12)"),
            );
            return None;
        }
        Some(slot)
    }

    fn add(&mut self, path: String, slot: Slot, data: Vec<u8>) {
        self.files.push((path, slot, data));
    }
}

/// Walk the folder: only the layout's folders are entered, and a symlink anywhere is an
/// error (S10).
fn read_dir(intake: &mut Intake, root: &Path, rel: &str, depth: usize) {
    if intake.stopped {
        return;
    }
    let dir = if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            intake
                .col
                .path_error(Code::Io, rel, format!("can't read the folder: {e}"));
            return;
        }
    };
    let mut names: Vec<(String, std::ffi::OsString)> = Vec::new();
    for e in entries.flatten() {
        // Stop reading a folder that alone passes the file limit (S12): the walk would
        // otherwise hold every entry of a hostile directory in memory.
        if names.len() > MAX_FILES {
            intake.stopped = true;
            intake
                .col
                .path_error(
                    Code::LimitFiles,
                    rel,
                    format!("more than {MAX_FILES} entries in `{rel}` (S12)"),
                )
                .help("a skin is a few KDL files and a handful of assets");
            break;
        }
        let os = e.file_name();
        match os.to_str() {
            Some(n) => names.push((n.to_owned(), os)),
            None => {
                intake.col.path_error(
                    Code::PathInvalid,
                    rel,
                    format!("a file name in `{rel}` is not valid UTF-8 (S10)"),
                );
            }
        }
    }
    names.sort();
    for (name, os) in names {
        let path = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        if intake.skip_junk && is_junk(&name) {
            continue;
        }
        let full = dir.join(&os);
        let Ok(meta) = std::fs::symlink_metadata(&full) else {
            intake
                .col
                .path_error(Code::Io, &path, format!("can't read `{path}`"));
            continue;
        };
        let ft = meta.file_type();
        if ft.is_symlink() {
            intake
                .col
                .path_error(
                    Code::Symlink,
                    &path,
                    format!("`{path}` is a symbolic link (S10)"),
                )
                .help("copy the file into the skin instead of linking it");
        } else if ft.is_dir() {
            let top = depth == 0 && is_layout_dir(&name);
            let inside = depth > 0 && depth < MAX_DIR_DEPTH;
            if top || inside {
                read_dir(intake, root, &path, depth + 1);
            } else {
                intake
                    .col
                    .path_error(
                        Code::FileType,
                        &path,
                        format!("`{path}` is not part of the skin layout"),
                    )
                    .help(LAYOUT_HELP);
            }
        } else if ft.is_file() {
            let Some(slot) = intake.admit(&path, meta.len()) else {
                continue;
            };
            let limit = slot.max_bytes();
            let mut data = Vec::new();
            let read = std::fs::File::open(&full)
                .and_then(|f| f.take(limit as u64 + 1).read_to_end(&mut data));
            match read {
                Ok(_) if data.len() <= limit => intake.add(path, slot, data),
                Ok(_) => {
                    intake.refused.insert(path.clone());
                    intake.col.path_error(
                        Code::LimitFile,
                        &path,
                        format!("`{path}` is larger than {limit} bytes (S12)"),
                    );
                }
                Err(e) => {
                    intake.refused.insert(path.clone());
                    intake
                        .col
                        .path_error(Code::Io, &path, format!("can't read `{path}`: {e}"));
                }
            }
        } else {
            intake.col.path_error(
                Code::FileType,
                &path,
                format!("`{path}` is not a regular file (S10)"),
            );
        }
    }
}

/// The parameters of a component that its body hands to an `action` or `link-action` as
/// `"{param}"`: a `use` of the component must give them plain action names.
fn action_params(nodes: &[RawNode], params: &[String]) -> Vec<String> {
    fn walk(nodes: &[RawNode], params: &[String], out: &mut Vec<String>) {
        for n in nodes {
            for p in n
                .props
                .iter()
                .filter(|p| matches!(p.name.as_str(), "action" | "link-action"))
            {
                if let Lit::Str(s) = &p.value.lit
                    && let Some(name) = s
                        .trim()
                        .strip_prefix('{')
                        .and_then(|r| r.strip_suffix('}'))
                        .map(str::trim)
                    && params.iter().any(|x| x == name)
                    && !out.iter().any(|x| x == name)
                {
                    out.push(name.to_owned());
                }
            }
            walk(&n.children, params, out);
        }
    }
    let mut out = Vec::new();
    walk(nodes, params, &mut out);
    out
}

// ---- the stages ----

/// A KDL file read and parsed.
struct Parsed {
    slot: Slot,
    file: usize,
    nodes: Option<Vec<RawNode>>,
}

fn finish(intake: Intake, name: &str) -> Result<SkinPackage, Report> {
    let Intake {
        mut col,
        files,
        refused,
        ..
    } = intake;
    let mut files = files;
    files.sort_by(|a, b| a.0.cmp(&b.0));

    // Assets first: their bytes are checked, and the routes refer to them.
    let mut assets_map: BTreeMap<String, Asset> = BTreeMap::new();
    for (path, slot, data) in &files {
        let Slot::Asset(kind) = slot else { continue };
        match assets::check(*kind, data) {
            Ok(size) => {
                assets_map.insert(
                    path.clone(),
                    Asset {
                        kind: *kind,
                        bytes: Arc::from(data.as_slice()),
                        size,
                    },
                );
            }
            Err(e) => {
                let code = match e {
                    AssetError::Png(_) => Code::AssetPng,
                    AssetError::Font(_) => Code::AssetFont,
                    _ => Code::AssetSvg,
                };
                col.path_error(code, path, format!("`{path}`: {e}"));
            }
        }
    }

    // The KDL files, in a fixed order, parsed under the scope of their kind.
    let mut counter = Counter::default();
    let mut parsed: Vec<Parsed> = Vec::new();
    for (path, slot, data) in &files {
        if matches!(slot, Slot::Asset(_)) {
            continue;
        }
        if *slot == Slot::Licence {
            if std::str::from_utf8(data).is_err() {
                col.path_error(
                    Code::AssetText,
                    path,
                    format!("`{path}` is not valid UTF-8"),
                )
                .help("a licence file is plain text (design 27 §1)");
            }
            continue;
        }
        let Ok(text) = std::str::from_utf8(data) else {
            col.path_error(
                Code::KdlSyntax,
                path,
                format!("`{path}` is not valid UTF-8"),
            );
            continue;
        };
        let file = col.sources.add(path, text);
        col.scope = match slot {
            Slot::Route(r) => LoadScope::Route(r.clone()),
            _ => LoadScope::Skin,
        };
        let nodes = if counter.tripped {
            None
        } else {
            crate::raw::parse_file(&mut col, file, &mut counter)
        };
        parsed.push(Parsed {
            slot: slot.clone(),
            file,
            nodes,
        });
    }
    col.scope = LoadScope::Skin;
    let find = |s: Slot| parsed.iter().find(|p| p.slot == s);

    // The manifest.
    let Some(man) = find(Slot::Manifest) else {
        col.path_error(
            Code::ManifestStructure,
            "skin.kdl",
            "the skin has no skin.kdl",
        )
        .help("design 27 §2 describes the manifest");
        return Err(col.report);
    };
    let manifest = man
        .nodes
        .as_deref()
        .and_then(|n| manifest::parse(&mut col, n, man.file, name));
    let Some(manifest) = manifest else {
        return Err(col.report);
    };

    // Tokens.
    let mut tokens_ok;
    let token_sets = match find(Slot::Tokens) {
        Some(p) => {
            let before = col.error_count();
            let sets = p
                .nodes
                .as_deref()
                .map(|n| tokens::parse(&mut col, n, p.file, &manifest.modes))
                .unwrap_or_default();
            tokens_ok = col.error_count() == before && p.nodes.is_some();
            sets
        }
        None => {
            col.path_error(
                Code::TokenStructure,
                "tokens.kdl",
                "the skin has no tokens.kdl",
            )
            .help("design 27 §3 describes the tokens");
            tokens_ok = false;
            BTreeMap::new()
        }
    };
    let empty = TokenSet::default();
    let first_tokens = manifest
        .modes
        .first()
        .and_then(|m| token_sets.get(m))
        .unwrap_or(&empty);
    tokens_ok &= manifest
        .modes
        .first()
        .is_some_and(|m| token_sets.contains_key(m));

    // Styles.
    let mut styles_ok = true;
    let styles_map = match find(Slot::Styles) {
        Some(p) => {
            let before = col.error_count();
            let ctx = Ctx {
                tokens: first_tokens,
                tokens_ok,
                dynamic: Dynamic::ColourOnly,
            };
            let m = p
                .nodes
                .as_deref()
                .map(|n| styles::parse(&mut col, n, &ctx))
                .unwrap_or_default();
            styles_ok = col.error_count() == before && p.nodes.is_some();
            m
        }
        None => BTreeMap::new(),
    };

    // Which routes have files, against the manifest.
    let mut route_files: BTreeMap<String, &Parsed> = BTreeMap::new();
    for p in &parsed {
        if let Slot::Route(r) = &p.slot {
            route_files.insert(r.clone(), p);
        }
    }
    for (r, p) in &route_files {
        let span = Span::new(p.file, 0, 0);
        if gogo_contract::routes::route_spec(r).is_none() {
            let names = gogo_contract::routes::ROUTES.iter().map(|x| x.name);
            let help = match suggest(r, names.clone()) {
                Some(s) => format!("did you mean `{s}`?"),
                None => format!("the routes are {}", names.collect::<Vec<_>>().join(", ")),
            };
            col.skin_error(
                Code::UnknownRoute,
                span,
                format!("`routes/{r}.kdl` is not a route of the contract (S16)"),
            )
            .help(help);
        } else if !manifest.routes.contains(r) {
            col.warn(
                Code::RouteFile,
                span,
                format!(
                    "`routes/{r}.kdl` is not listed in the manifest's `routes`, so it is not loaded"
                ),
            )
            .help("add it to `routes` in skin.kdl");
        }
    }
    for r in &manifest.routes {
        if !route_files.contains_key(r) && !refused.contains(&format!("routes/{r}.kdl")) {
            col.skin_error(
                Code::RouteFile,
                manifest.span,
                format!("the manifest covers `{r}` and the skin has no `routes/{r}.kdl`"),
            )
            .help("add the file, or remove the route from `routes`");
        }
    }

    // Components: their names first, so a `use` finds a component declared later.
    let mut comps: BTreeMap<String, Component> = BTreeMap::new();
    let mut sigs: BTreeMap<String, Sig> = BTreeMap::new();
    let comp_before = col.error_count();
    let mut raw_comps: Vec<(&RawNode, String, Vec<String>)> = Vec::new();
    for p in parsed.iter().filter(|p| p.slot == Slot::Component) {
        for n in p.nodes.as_deref().unwrap_or_default() {
            if let Some((cname, params)) = declare(&mut col, n, &sigs) {
                sigs.insert(
                    cname.clone(),
                    Sig {
                        action_params: action_params(&n.children, &params),
                        params: params.clone(),
                    },
                );
                raw_comps.push((n, cname, params));
            }
        }
    }
    let components_ok = col.error_count() == comp_before
        && parsed
            .iter()
            .filter(|p| p.slot == Slot::Component)
            .all(|p| p.nodes.is_some());
    let world = World {
        tokens: first_tokens,
        tokens_ok,
        styles: &styles_map,
        styles_ok,
        components: &sigs,
        components_ok,
        assets: &assets_map,
    };
    let mut edges: BTreeMap<String, Vec<(String, Span)>> = BTreeMap::new();
    for (n, cname, params) in raw_comps {
        let body: Vec<Node> = n
            .children
            .iter()
            .map(|c| ast::convert(&mut col, c))
            .collect();
        let mut walk = Walk {
            col: &mut col,
            world: &world,
            place: Place {
                route: None,
                component: true,
                params: &params,
            },
            uses: Vec::new(),
        };
        walk.tree(&body);
        edges.insert(cname.clone(), walk.uses);
        comps.insert(
            cname.clone(),
            Component {
                name: cname,
                params,
                body,
                span: n.span,
            },
        );
    }

    // Routes.
    let mut routes: BTreeMap<String, Route> = BTreeMap::new();
    for r in &manifest.routes {
        let Some(p) = route_files.get(r) else {
            continue;
        };
        let Some(nodes) = p.nodes.as_deref() else {
            continue;
        };
        col.scope = LoadScope::Route(r.clone());
        let Some(root) = route_root(&mut col, nodes, p.file, r) else {
            continue;
        };
        let Some(spec) = gogo_contract::routes::route_spec(r) else {
            continue;
        };
        let children: Vec<Node> = root
            .children
            .iter()
            .map(|c| ast::convert(&mut col, c))
            .collect();
        let mut walk = Walk {
            col: &mut col,
            world: &world,
            place: Place {
                route: Some(r),
                component: false,
                params: &[],
            },
            uses: Vec::new(),
        };
        walk.tree(&children);
        routes.insert(
            r.clone(),
            Route {
                name: r.clone(),
                spec,
                children,
                span: root.span,
            },
        );
    }
    col.scope = LoadScope::Skin;

    // S20 and S12: components don't loop, and their expansion stays bounded.
    check_expansion(&mut col, &comps, &edges, &routes);

    // The contract: names resolved, field paths and types checked (design 27 §7).
    let looped = col.report.findings().iter().any(|f| {
        matches!(
            f.code,
            Code::ComponentCycle | Code::LimitNodes | Code::LimitDepth
        )
    });
    col.scope = LoadScope::Skin;
    resolve::components(&mut col, &manifest, &mut comps);
    resolve::routes(
        &mut col,
        &manifest,
        &comps,
        &mut routes,
        !looped,
        |col, r| col.scope = LoadScope::Route(r.to_owned()),
    );
    col.scope = LoadScope::Skin;

    // A route with an error falls back, so it is not offered.
    let failed: BTreeSet<String> = col
        .report
        .failed_routes()
        .into_iter()
        .map(str::to_owned)
        .collect();
    routes.retain(|r, _| !failed.contains(r));

    if col.report.has_skin_errors() {
        return Err(col.report);
    }
    Ok(SkinPackage {
        manifest,
        tokens: token_sets,
        styles: styles_map,
        components: comps,
        routes,
        assets: assets_map,
        sources: col.sources,
        report: col.report,
        tables: OnceLock::new(),
        fragments: FragmentCache::default(),
    })
}

/// `route "<name>" { … }`: the only node of `routes/<name>.kdl`.
fn route_root<'a>(
    col: &mut Collector,
    nodes: &'a [RawNode],
    file: usize,
    stem: &str,
) -> Option<&'a RawNode> {
    let at = Span::new(file, 0, 0);
    let [root] = nodes else {
        col.error(
            Code::RouteFile,
            nodes.get(1).map_or(at, |n| n.span),
            format!("`routes/{stem}.kdl` holds exactly one `route \"{stem}\"` node"),
        );
        return None;
    };
    if root.name != "route" || root.args.len() != 1 || !root.props.is_empty() {
        col.error(
            Code::RouteFile,
            root.span,
            format!("the file starts with `route \"{stem}\" {{ … }}`"),
        );
        return None;
    }
    if root.str_arg(0) != Some(stem) {
        col.error(
            Code::RouteFile,
            root.args[0].span,
            format!("this file is `routes/{stem}.kdl`, so the route is named `{stem}`"),
        );
        return None;
    }
    Some(root)
}

/// `component "<name>" params="a b" { … }`: the name and parameters, once they check out.
fn declare(
    col: &mut Collector,
    n: &RawNode,
    known: &BTreeMap<String, Sig>,
) -> Option<(String, Vec<String>)> {
    if n.name != "component" {
        col.error(
            Code::ComponentParam,
            n.name_span,
            format!("component files hold `component` nodes, found `{}`", n.name),
        );
        return None;
    }
    let name = match (n.args.as_slice(), n.str_arg(0)) {
        // A dot is allowed: the default skin's components that others borrow are named
        // `game.builds` (design 27 §5.7).
        ([_], Some(s)) if is_style_name(s) => s,
        _ => {
            col.error(
                Code::ComponentParam,
                n.span,
                "`component` takes one name: `component \"tile\" params=\"a b\" { … }`",
            );
            return None;
        }
    };
    if known.contains_key(name) {
        col.error(
            Code::ComponentParam,
            n.args[0].span,
            format!("component `{name}` is defined twice"),
        );
        return None;
    }
    let mut params = Vec::new();
    for p in &n.props {
        if p.name != "params" {
            col.error(
                Code::ComponentParam,
                p.name_span,
                format!("`component` has `params=`, not `{}`", p.name),
            );
            continue;
        }
        let Lit::Str(s) = &p.value.lit else {
            col.error(
                Code::ComponentParam,
                p.value.span,
                "`params` is a string of names",
            );
            continue;
        };
        for w in s.split_whitespace() {
            if !is_ident(w) || matches!(w, "when" | "id") {
                col.error(
                    Code::ComponentParam,
                    p.value.span,
                    format!("`{w}` is not a parameter name"),
                )
                .help("lowercase letters, digits and `_`; `when` and `id` are reserved");
            } else if params.iter().any(|x: &String| x == w) {
                col.error(
                    Code::ComponentParam,
                    p.value.span,
                    format!("parameter `{w}` is listed twice"),
                );
            } else {
                params.push(w.to_owned());
            }
        }
    }
    Some((name.to_owned(), params))
}

/// Components that use each other in a loop are an error (S20); and a component, or a
/// route, that expands to more nodes or levels than the limits is too (S12).
fn check_expansion(
    col: &mut Collector,
    comps: &BTreeMap<String, Component>,
    edges: &BTreeMap<String, Vec<(String, Span)>>,
    routes: &BTreeMap<String, Route>,
) {
    // Cycles: depth-first, three colours.
    #[derive(Clone, Copy, PartialEq)]
    enum C {
        White,
        Grey,
        Black,
    }
    fn visit(
        name: &str,
        edges: &BTreeMap<String, Vec<(String, Span)>>,
        colour: &mut BTreeMap<String, C>,
        stack: &mut Vec<String>,
        found: &mut Vec<(Span, String)>,
    ) {
        colour.insert(name.to_owned(), C::Grey);
        stack.push(name.to_owned());
        for (next, at) in edges.get(name).map(Vec::as_slice).unwrap_or_default() {
            match colour.get(next).copied().unwrap_or(C::White) {
                C::White => visit(next, edges, colour, stack, found),
                C::Grey => {
                    let from = stack.iter().position(|s| s == next).unwrap_or(0);
                    let mut cycle: Vec<&str> = stack[from..].iter().map(String::as_str).collect();
                    cycle.push(next);
                    found.push((*at, cycle.join(" -> ")));
                }
                C::Black => {}
            }
        }
        stack.pop();
        colour.insert(name.to_owned(), C::Black);
    }
    let mut colour = BTreeMap::new();
    let mut found = Vec::new();
    for name in comps.keys() {
        if colour.get(name).copied().unwrap_or(C::White) == C::White {
            visit(name, edges, &mut colour, &mut Vec::new(), &mut found);
        }
    }
    for (at, cycle) in &found {
        col.skin_error(
            Code::ComponentCycle,
            *at,
            format!("components use each other in a loop: {cycle} (S20)"),
        )
        .help("nothing in a skin loops except `each` over a list");
    }
    if !found.is_empty() {
        return;
    }

    // Expansion: nodes and depth once every `use` is replaced by the component's tree.
    fn measure(
        nodes: &[Node],
        base: usize,
        known: &BTreeMap<String, (usize, usize)>,
    ) -> (usize, usize) {
        let (mut count, mut depth) = (0usize, 0usize);
        for n in nodes {
            count = count.saturating_add(1);
            depth = depth.max(base);
            if n.name == "use"
                && let Some((c, d)) = n.plain_arg(0).and_then(|c| known.get(c))
            {
                count = count.saturating_add(*c);
                depth = depth.max(base.saturating_add(*d));
            }
            let (c, d) = measure(&n.children, base + 1, known);
            count = count.saturating_add(c);
            depth = depth.max(d);
        }
        (count, depth)
    }
    let mut known: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    // Callees before callers: repeat until every component is measured.
    let mut left: Vec<&String> = comps.keys().collect();
    while !left.is_empty() {
        let before = left.len();
        left.retain(|name| {
            let ready = edges
                .get(*name)
                .is_none_or(|e| e.iter().all(|(c, _)| known.contains_key(c)));
            if ready && let Some(c) = comps.get(*name) {
                let m = measure(&c.body, 1, &known);
                known.insert((*name).clone(), m);
                if m.0 > MAX_NODES {
                    col.skin_error(
                        Code::LimitNodes,
                        c.span,
                        format!("component `{name}` expands to more than {MAX_NODES} nodes (S12)"),
                    );
                }
                if m.1 > MAX_DEPTH {
                    col.skin_error(
                        Code::LimitDepth,
                        c.span,
                        format!("component `{name}` expands to more than {MAX_DEPTH} levels (S12)"),
                    );
                }
            }
            !ready
        });
        if left.len() == before {
            break;
        }
    }
    for (name, r) in routes {
        let (c, d) = measure(&r.children, 2, &known);
        if c > MAX_NODES {
            col.skin_error(
                Code::LimitNodes,
                r.span,
                format!("route `{name}` expands to more than {MAX_NODES} nodes (S12)"),
            );
        }
        if d > MAX_DEPTH {
            col.skin_error(
                Code::LimitDepth,
                r.span,
                format!("route `{name}` expands to more than {MAX_DEPTH} levels (S12)"),
            );
        }
    }
}

#[cfg(test)]
mod fragment_cache_tests {
    use super::FragmentCache;

    #[test]
    fn a_fragment_is_checked_once_and_a_copy_starts_empty() {
        let cache = FragmentCache::default();
        let mut runs = 0;
        for _ in 0..3 {
            let found = cache.get_or_check("game.builds", || {
                runs += 1;
                Vec::new()
            });
            assert!(found.is_empty());
        }
        assert_eq!(runs, 1);
        let mut copy_runs = 0;
        let _ = cache.clone().get_or_check("game.builds", || {
            copy_runs += 1;
            Vec::new()
        });
        assert_eq!(copy_runs, 1, "a clone has its own cache");
    }
}
