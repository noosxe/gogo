//! Installing a skin from a folder or a zip into the skin folder (design 27 §10).
//!
//! A source is untrusted. [`SkinFolder::read`] takes it into memory under the limits of
//! design 27 §8 and validates it with the loader; [`SkinFolder::install`] writes exactly
//! the bytes that were validated, into a staging folder, and renames it into place. A
//! refused or failed import leaves the skin folder as it was. Nothing of a skin is ever
//! run.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Cursor, Read, Write};
use std::path::{Path, PathBuf};

use zip::{CompressionMethod, ZipArchive};

use crate::diag::{Finding, Report};
use crate::limits::{
    MAX_ARCHIVE_BYTES, MAX_ARCHIVE_ENTRIES, MAX_ARCHIVE_RATIO, MAX_DIR_DEPTH, MAX_FILES,
    MAX_SKIN_BYTES, RATIO_FREE_BYTES,
};
use crate::manifest::{Manifest, Mode};
use crate::names::is_skin_name;
use crate::package::{self, SkinPackage};

/// Names no installed skin may take: the built-in skins, and those to come.
pub const RESERVED_NAMES: &[&str] = &["default", "instrument", "slab"];

/// The mode the app draws. A skin without it would fail on every route.
const DRAWN_MODE: Mode = Mode::Dark;

/// Held while the skin folder is changed. A name starting with `.` is never a skin.
const LOCK: &str = ".lock";
const STAGING: &str = ".import";
/// Before the name of a skin being replaced: `.old-<name>`.
const OLD: &str = ".old-";

/// The most characters of a name or a message from a source that are shown.
const SHOWN: usize = 160;

/// The most findings [`ImportError::lines`] lists.
const LISTED: usize = 20;

/// Why nothing was installed.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// The source can't be read, or is not a skin folder or zip.
    #[error("{0}")]
    Source(String),
    /// The archive breaks a rule of design 27 §10.
    #[error("the archive is refused: {0}")]
    Archive(String),
    /// The loader found errors.
    #[error("the skin does not load")]
    Skin(Box<Report>),
    #[error("`{0}` is the name of a built-in skin")]
    Reserved(String),
    #[error("the skin has no dark mode, and the app only draws dark for now")]
    NoDarkMode,
    #[error("a skin named `{name}` is installed already (version {version})")]
    Exists { name: String, version: String },
    #[error("can't write the skin folder: {0}")]
    Io(#[from] io::Error),
}

impl ImportError {
    /// What to show: the reason, then the loader's errors, one line each. Safe to print.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![printable(&self.to_string())];
        if let ImportError::Skin(report) = self {
            lines.extend(report.errors().take(LISTED).map(finding_line));
            let more = report.error_total().saturating_sub(lines.len() - 1);
            if more > 0 {
                lines.push(format!("and {more} more"));
            }
        }
        lines
    }
}

fn finding_line(f: &Finding) -> String {
    printable(&format!("{}: {}", f.location, f.message))
}

/// Whether `c` does not show as itself: a control character, one that is invisible, or
/// one that reorders or breaks the text around it. Text from a skin or its source is
/// shown without these.
pub fn is_hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061c}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}'
                | '\u{feff}'
        )
}

/// Text from a source made safe to print: nothing [hidden](is_hidden), and bounded.
fn printable(text: &str) -> String {
    let mut out: String = text
        .chars()
        .take(SHOWN)
        .map(|c| if is_hidden(c) { '?' } else { c })
        .collect();
    if text.chars().nth(SHOWN).is_some() {
        out.push('…');
    }
    out
}

/// A skin read from a source and found good, held in memory until it is installed.
#[derive(Debug)]
pub struct Candidate {
    files: Vec<(String, Vec<u8>)>,
    pub manifest: Manifest,
    /// The loader's warnings, one line each.
    pub warnings: Vec<String>,
}

impl Candidate {
    /// The name it installs under.
    pub fn name(&self) -> &str {
        &self.manifest.name
    }
}

/// An installed skin, as a list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub name: String,
    /// Its title and version; `None` when it does not load.
    pub about: Option<(String, String)>,
}

/// The folder third-party skins are installed in (design 25 §9).
#[derive(Debug, Clone)]
pub struct SkinFolder {
    root: PathBuf,
}

impl SkinFolder {
    pub fn new(root: impl Into<PathBuf>) -> SkinFolder {
        SkinFolder { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The folder of the installed skin `name`, if there is one. Anything but a skin name
    /// has none, so a value from a skin can't name a path (S3).
    pub fn path(&self, name: &str) -> Option<PathBuf> {
        if !is_skin_name(name) || RESERVED_NAMES.contains(&name) {
            return None;
        }
        let dir = self.root.join(name);
        dir.is_dir().then_some(dir)
    }

    /// The names of the installed skins, sorted. They are not loaded.
    pub fn names(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| self.path(n).is_some())
            .collect();
        names.sort();
        names
    }

    /// Loads the installed skin `name` to say what it is.
    pub fn describe(&self, name: &str) -> Installed {
        let about = self
            .path(name)
            .and_then(|dir| SkinPackage::load(&dir).ok())
            .filter(|p| p.report().error_total() == 0)
            .map(|p| (p.manifest.title.clone(), p.manifest.version.to_string()));
        Installed {
            name: name.to_owned(),
            about,
        }
    }

    /// The version of the skin installed as `name`: `unknown` if it does not load, `None`
    /// if there is none.
    pub fn installed_version(&self, name: &str) -> Option<String> {
        self.path(name)?;
        Some(
            self.describe(name)
                .about
                .map_or_else(|| "unknown".to_owned(), |(_, version)| version),
        )
    }

    /// Reads and validates a folder or a zip. Writes nothing.
    pub fn read(&self, source: &Path) -> Result<Candidate, ImportError> {
        let cant = |e: io::Error| ImportError::Source(format!("can't read the source: {e}"));
        let meta = fs::metadata(source).map_err(cant)?;
        if let (Ok(source), Ok(root)) = (source.canonicalize(), self.root.canonicalize())
            && source.starts_with(root)
        {
            return Err(ImportError::Source(
                "the source is inside the skin folder".to_owned(),
            ));
        }
        let files = if meta.is_dir() {
            read_dir_source(source)?
        } else if meta.is_file() {
            let mut data = Vec::new();
            File::open(source)
                .and_then(|f| f.take(MAX_ARCHIVE_BYTES as u64 + 1).read_to_end(&mut data))
                .map_err(cant)?;
            read_archive(&data)?
        } else {
            return Err(ImportError::Source(
                "the source is not a folder or a file".to_owned(),
            ));
        };
        validate(files)
    }

    /// Installs `candidate`. With `replace`, a skin of the same name gives way to it.
    ///
    /// `Ok` means the skin is in place. It carries the error when the skin folder could
    /// not be synced afterwards, for the caller to log: a power cut could still undo the
    /// install.
    pub fn install(
        &self,
        candidate: &Candidate,
        replace: bool,
    ) -> Result<Option<io::Error>, ImportError> {
        self.install_then(candidate, replace, || Ok(()))
    }

    /// [`install`](Self::install), calling `staged` between writing the staging folder and
    /// moving it into place, so a test can fail there.
    fn install_then(
        &self,
        candidate: &Candidate,
        replace: bool,
        staged: impl FnOnce() -> io::Result<()>,
    ) -> Result<Option<io::Error>, ImportError> {
        fs::create_dir_all(&self.root)?;
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.root.join(LOCK))?;
        lock.lock()?;
        self.sweep()?;

        let name = candidate.name();
        let target = self.root.join(name);
        let exists = fs::symlink_metadata(&target).is_ok();
        if exists && !replace {
            return Err(ImportError::Exists {
                name: name.to_owned(),
                version: self
                    .installed_version(name)
                    .unwrap_or_else(|| "unknown".to_owned()),
            });
        }

        let staging = Staging(self.root.join(STAGING));
        fs::create_dir(&staging.0)?;
        let dir = staging.0.join(name);
        write_files(&dir, &candidate.files)?;
        // What is on disk must load as what was read.
        let loaded = SkinPackage::load(&dir).map_err(|r| ImportError::Skin(Box::new(r)))?;
        if loaded.report().error_total() > 0 {
            return Err(ImportError::Skin(Box::new(loaded.report().clone())));
        }
        staged()?;

        if exists {
            let old = self.root.join(format!("{OLD}{name}"));
            fs::rename(&target, &old)?;
            if let Err(e) = fs::rename(&dir, &target) {
                fs::rename(&old, &target)?;
                return Err(e.into());
            }
            // Left over, it is removed by the next import.
            let _ = remove(&old);
        } else {
            fs::rename(&dir, &target)?;
        }
        // Past the rename the skin is installed, whatever the sync says.
        Ok(sync_dir(&self.root).err())
    }

    /// Clears what an import that was cut off left behind. A skin that was being replaced
    /// comes back if nothing took its place. Only under the lock.
    fn sweep(&self) -> io::Result<()> {
        let staging = self.root.join(STAGING);
        if fs::symlink_metadata(&staging).is_ok() {
            remove(&staging)?;
        }
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let file = entry.file_name();
            let Some(name) = file.to_str().and_then(|f| f.strip_prefix(OLD)) else {
                continue;
            };
            let target = self.root.join(name);
            if is_skin_name(name) && fs::symlink_metadata(&target).is_err() {
                fs::rename(entry.path(), target)?;
            } else {
                remove(&entry.path())?;
            }
        }
        Ok(())
    }
}

/// The staging folder, removed however the import ends.
struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        // Left over, it is removed by the next import.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Removes a folder, or the file or link in its place, without following a link.
fn remove(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Writes the validated files under `dir`, as new, plain, non-executable files.
fn write_files(dir: &Path, files: &[(String, Vec<u8>)]) -> io::Result<()> {
    fs::create_dir(dir)?;
    let mut dirs = vec![dir.to_path_buf()];
    for (path, data) in files {
        let mut full = dir.to_path_buf();
        let mut parts = path.split('/').peekable();
        while let Some(part) = parts.next() {
            full.push(part);
            if parts.peek().is_some() && !dirs.contains(&full) {
                fs::create_dir(&full)?;
                dirs.push(full.clone());
            }
        }
        let mut file = File::options().write(true).create_new(true).open(&full)?;
        file.write_all(data)?;
        file.sync_all()?;
    }
    dirs.iter().try_for_each(|d| sync_dir(d))
}

/// Validates the files with the loader, under the name `skin.kdl` gives.
fn validate(files: Vec<(String, Vec<u8>)>) -> Result<Candidate, ImportError> {
    let manifest = files
        .iter()
        .find(|(path, _)| path == "skin.kdl")
        .ok_or_else(|| ImportError::Source("there is no skin.kdl".to_owned()))?;
    // Without a name to take, the load below says what is wrong with skin.kdl.
    let name = package::declared_name(&manifest.1).unwrap_or_else(|| "skin".to_owned());
    let package = SkinPackage::from_files(&name, files.iter().cloned())
        .map_err(|r| ImportError::Skin(Box::new(r)))?;
    let report = package.report();
    if report.error_total() > 0 {
        return Err(ImportError::Skin(Box::new(report.clone())));
    }
    let manifest = package.manifest.clone();
    if RESERVED_NAMES.contains(&manifest.name.as_str()) {
        return Err(ImportError::Reserved(manifest.name));
    }
    if !manifest.modes.contains(&DRAWN_MODE) {
        return Err(ImportError::NoDarkMode);
    }
    let warnings = report.warnings().take(LISTED).map(finding_line).collect();
    Ok(Candidate {
        files,
        manifest,
        warnings,
    })
}

fn read_dir_source(dir: &Path) -> Result<Vec<(String, Vec<u8>)>, ImportError> {
    // Checked first, so that a wrong folder is one clear message and not a list of
    // everything in it.
    if fs::symlink_metadata(dir.join("skin.kdl")).is_err() {
        return Err(ImportError::Source(
            "there is no skin.kdl in this folder".to_owned(),
        ));
    }
    package::read_folder(dir).map_err(|r| ImportError::Skin(Box::new(r)))
}

/// An entry of an archive that may become part of the skin.
struct Entry {
    index: usize,
    /// Its path as the archive has it, without the `/` that ends a folder's.
    path: String,
    is_dir: bool,
}

fn refused(why: impl Into<String>) -> ImportError {
    ImportError::Archive(why.into())
}

/// `` `name` why ``, with the name made safe to print.
fn entry_refused(name: &str, why: &str) -> ImportError {
    refused(format!("`{}` {why}", printable(name)))
}

/// The skin's files out of a zip, under design 27 §10's rules.
fn read_archive(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>, ImportError> {
    if data.len() > MAX_ARCHIVE_BYTES {
        return Err(refused(format!(
            "it is larger than {MAX_ARCHIVE_BYTES} bytes"
        )));
    }
    let too_many = || refused(format!("it has more than {MAX_ARCHIVE_ENTRIES} entries"));
    // The reader builds an entry for every central directory header it follows, before
    // their number can be asked. Counting the headers' marks first can't be talked round
    // by what an end record claims.
    let marks = data.windows(4).filter(|w| *w == b"PK\x01\x02");
    if marks.take(MAX_ARCHIVE_ENTRIES + 1).count() > MAX_ARCHIVE_ENTRIES {
        return Err(too_many());
    }
    let bad = |e: zip::result::ZipError| refused(format!("not a readable zip: {e}"));
    let mut archive = ZipArchive::new(Cursor::new(data)).map_err(bad)?;
    // The reader keeps one of two entries with a name, so look before anything else.
    if archive.has_duplicate_names() {
        return Err(refused("it has two entries with the same name"));
    }
    let mut entries = Vec::new();
    // Where each entry's packed bytes are: the ratio below rests on their size.
    let mut spans = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index).map_err(bad)?;
        let Ok(raw) = std::str::from_utf8(entry.name_raw()) else {
            return Err(refused("a name in it is not UTF-8"));
        };
        let is_dir = raw.ends_with('/');
        let path = raw.strip_suffix('/').unwrap_or(raw).to_owned();
        let kind = entry.unix_mode().map_or(0, |mode| mode & 0o170_000);
        let plain = if is_dir { 0o040_000 } else { 0o100_000 };
        if entry.is_symlink() {
            return Err(entry_refused(&path, "is a symbolic link"));
        }
        if kind != 0 && kind != plain {
            return Err(entry_refused(&path, "is not a regular file"));
        }
        if entry.encrypted() {
            return Err(entry_refused(&path, "is encrypted"));
        }
        if !matches!(
            entry.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(entry_refused(&path, "is compressed in a way not supported"));
        }
        let span = entry
            .data_start()
            .and_then(|start| Some((start, start.checked_add(entry.compressed_size())?)))
            .filter(|&(_, end)| end <= data.len() as u64);
        let Some(span) = span else {
            return Err(entry_refused(&path, "runs past the end of the archive"));
        };
        spans.push(span);
        // The whole name is checked before it is looked at in parts; the length limit
        // is for the path inside the skin, which may be shorter.
        check_path(&path)?;
        if path.split('/').any(package::is_junk) {
            continue;
        }
        entries.push(Entry {
            index,
            path,
            is_dir,
        });
    }

    spans.sort_unstable();
    if spans.windows(2).any(|pair| pair[1].0 < pair[0].1) {
        return Err(refused("two of its entries share their data"));
    }

    let top = top_folder(&entries);
    let mut files = Vec::new();
    let mut names = Names::default();
    let mut total = 0usize;
    for entry in &entries {
        let path = match &top {
            // The top folder's own entry.
            Some(top) if entry.path == *top => continue,
            Some(top) => entry
                .path
                .strip_prefix(top.as_str())
                .and_then(|p| p.strip_prefix('/'))
                .unwrap_or(&entry.path),
            None => &entry.path,
        };
        if let Err(why) = package::check_rel_path(path) {
            return Err(entry_refused(path, why));
        }
        let parts = path.split('/').count();
        let dirs = if entry.is_dir { parts } else { parts - 1 };
        if dirs > MAX_DIR_DEPTH {
            return Err(entry_refused(path, "is nested too deeply"));
        }
        names.add(path, entry.is_dir)?;
        if entry.is_dir {
            let first = path.split('/').next().unwrap_or(path);
            if !package::is_layout_dir(first) {
                return Err(entry_refused(path, "is not part of the skin layout"));
            }
            continue;
        }
        if files.len() >= MAX_FILES {
            return Err(refused(format!("it has more than {MAX_FILES} files")));
        }
        let limit = package::file_limit(path).map_err(|why| refused(printable(&why)))?;
        let limit = limit.min(MAX_SKIN_BYTES - total);
        let file = archive.by_index(entry.index).map_err(bad)?;
        let packed = file.compressed_size();
        let mut content = Vec::new();
        // One byte past the limit is enough to know; the declared size is not trusted.
        file.take(limit as u64 + 1)
            .read_to_end(&mut content)
            .map_err(|e| entry_refused(path, &format!("can't be unpacked: {e}")))?;
        if content.len() > limit {
            return Err(entry_refused(
                path,
                &format!("unpacks to more than {limit} bytes, over the skin's limits"),
            ));
        }
        let size = content.len() as u64;
        if size > RATIO_FREE_BYTES && size > packed.saturating_mul(MAX_ARCHIVE_RATIO) {
            return Err(entry_refused(
                path,
                &format!("unpacks to over {MAX_ARCHIVE_RATIO} times its size in the archive"),
            ));
        }
        total += content.len();
        files.push((path.to_owned(), content));
    }
    Ok(files)
}

/// [`package::check_rel_path`] but for its length limit.
fn check_path(path: &str) -> Result<(), ImportError> {
    if path.starts_with('/') {
        return Err(entry_refused(path, "is absolute"));
    }
    for part in path.split('/') {
        if part.is_empty() {
            return Err(entry_refused(path, "has an empty component"));
        }
        // Each part alone carries the other rules.
        if let Err(why) = package::check_rel_path(part) {
            return Err(entry_refused(path, why));
        }
    }
    Ok(())
}

/// The folder every entry is in, when the skin is not at the root but inside one folder,
/// as most tools pack a folder.
fn top_folder(entries: &[Entry]) -> Option<String> {
    if entries.iter().any(|e| e.path == "skin.kdl") {
        return None;
    }
    let top = entries.first()?.path.split('/').next()?;
    let inside = |e: &Entry| match e.path.strip_prefix(top) {
        Some("") => e.is_dir,
        Some(rest) => rest.starts_with('/'),
        None => false,
    };
    let manifest = format!("{top}/skin.kdl");
    (entries.iter().all(inside) && entries.iter().any(|e| e.path == manifest))
        .then(|| top.to_owned())
}

/// The names taken so far, folders included, to refuse those that can't both be written:
/// two that differ only in ASCII case, and a file where a folder is.
#[derive(Default)]
struct Names {
    /// By lowercase path: the path as written, and whether it is a folder.
    taken: HashMap<String, (String, bool)>,
}

impl Names {
    fn add(&mut self, path: &str, is_dir: bool) -> Result<(), ImportError> {
        let ends = path.match_indices('/').map(|(i, _)| (&path[..i], true));
        for (part, dir) in ends.chain([(path, is_dir)]) {
            let key = part.to_ascii_lowercase();
            match self.taken.get(&key) {
                None => {
                    self.taken.insert(key, (part.to_owned(), dir));
                }
                Some((written, was_dir)) if written == part && *was_dir && dir => {}
                Some(_) => {
                    return Err(entry_refused(
                        part,
                        "appears twice, differing in case or as both a file and a folder",
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Default skin, renamed to `mine`, as version `version`.
    fn candidate(version: &str) -> Candidate {
        let default = Path::new(env!("CARGO_MANIFEST_DIR")).join("../gogo-skins/skins/default");
        let mut files = package::read_folder(&default).expect("the Default skin reads");
        let (_, manifest) = files
            .iter_mut()
            .find(|f| f.0 == "skin.kdl")
            .expect("it has a manifest");
        *manifest = String::from_utf8_lossy(manifest)
            .replace("skin \"default\"", "skin \"mine\"")
            .replace("\"0.1.0\"", &format!("\"{version}\""))
            .into_bytes();
        validate(files).unwrap_or_else(|e| panic!("{:?}", e.lines()))
    }

    fn listing(root: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_failure_after_staging_leaves_the_folder_as_it_was() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = SkinFolder::new(tmp.path().join("skins"));
        let fail = || Err(io::Error::other("injected"));

        let err = folder.install_then(&candidate("1.0.0"), false, fail);
        assert!(matches!(err, Err(ImportError::Io(_))), "{err:?}");
        assert_eq!(listing(folder.root()), [LOCK]);

        folder.install(&candidate("1.0.0"), false).unwrap();
        let err = folder.install_then(&candidate("2.0.0"), true, fail);
        assert!(matches!(err, Err(ImportError::Io(_))), "{err:?}");
        assert_eq!(listing(folder.root()), [LOCK, "mine"]);
        assert_eq!(folder.installed_version("mine").as_deref(), Some("1.0.0"));
    }

    #[test]
    fn what_a_cut_off_import_left_is_cleared_and_a_lost_skin_comes_back() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = SkinFolder::new(tmp.path().join("skins"));
        folder.install(&candidate("1.0.0"), false).unwrap();
        let root = folder.root();
        // As a replace cut off between its two renames leaves things.
        fs::rename(root.join("mine"), root.join(".old-mine")).unwrap();
        fs::create_dir_all(root.join(".import/mine")).unwrap();
        assert!(folder.names().is_empty(), "neither is a skin");

        let err = folder.install(&candidate("2.0.0"), false);
        assert!(matches!(err, Err(ImportError::Exists { .. })), "{err:?}");
        assert_eq!(listing(root), [LOCK, "mine"]);
        assert_eq!(folder.installed_version("mine").as_deref(), Some("1.0.0"));
    }
}
