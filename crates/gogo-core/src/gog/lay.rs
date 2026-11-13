//! Laying fetched DLC files onto a built game's `game/` dir (design 19 §3.4).
//!
//! The DLC's files are fetched into a source root (the depot cache, or a staging dir
//! inside the game dir) and then laid onto `game/` under a journal, so that a stop at
//! any point leaves either the game as it was or a state [`recover`] can finish:
//!
//! 1. `dlc-journal.json` in the game dir names every file, the base files it will
//!    replace and the dirs it will create; fsynced.
//! 2. Each file is cloned (or copied, or for a staging root renamed) to
//!    `game/<rel>.gogo-new`.
//! 3. Each base file a DLC file replaces is renamed into `.dlc-backup/<rel>`.
//! 4. Every `.gogo-new` is renamed into place.
//! 5. The caller commits (`content.toml`), then [`finish`] deletes the backups, the
//!    staging dir and the journal.
//!
//! An update (design 21 §11g) uses the same journal with `kind = "update"`: its
//! `removed` paths, files the new build dropped, go into the backup in step 3 like a
//! replaced file, and come back from it on a rollback.
//!
//! Only files the journal names are ever deleted, and a replaced base file only goes
//! back from its backup. Nothing outside `game/`, the backup and staging dirs and the
//! journal is written.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::materialize::{self, Reflink};

/// The journal's name in a game dir.
pub const JOURNAL: &str = "dlc-journal.json";
/// Replaced base files, in the game dir, until the commit.
pub const BACKUP: &str = ".dlc-backup";
/// The source root of an add with no usable depot cache, in the game dir.
pub const STAGING: &str = ".dlc-staging";
/// The suffix of a laid file not yet in place.
pub const NEW: &str = ".gogo-new";

/// One file to lay: `from` (absolute, in the source root) becomes `game/<to>`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LayFile {
    pub from: PathBuf,
    /// `/`-separated, relative to `game/`.
    pub to: String,
}

/// A DLC the journal adds.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AddingDlc {
    pub id: u64,
    pub title: String,
}

/// `dlc-journal.json`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Journal {
    pub gogid: String,
    /// `depot` (the installer lane's marker comes with #351).
    pub lane: String,
    pub build_id: String,
    pub language: String,
    pub adding: Vec<AddingDlc>,
    /// The source root.
    pub source: PathBuf,
    /// The source is the depot cache: its record is updated at the commit.
    pub source_is_cache: bool,
    /// Files are renamed out of the source (a staging root on the game's filesystem)
    /// instead of cloned.
    pub moves: bool,
    pub files: Vec<LayFile>,
    /// Paths under `game/` that existed when the journal was written: base files
    /// a DLC file replaces. Backed up before they are replaced.
    pub replaced: Vec<String>,
    /// Dirs under `game/` the lay creates, parents first.
    pub dirs: Vec<String>,
    /// `update` for an update's lay (design 21 §11g); absent for a DLC add.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// An update's builds: from, to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Paths under `game/` the lay takes out (files the new build dropped): moved
    /// into the backup with the replaced files, put back on a rollback.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
}

/// The journal kind of an update.
pub const KIND_UPDATE: &str = "update";

impl Journal {
    /// An update's journal (design 21 §11g).
    pub fn is_update(&self) -> bool {
        self.kind.as_deref() == Some(KIND_UPDATE)
    }
}

impl Journal {
    /// Map every file onto `game`'s on-disk casing ([`CaseMap`]), then fill
    /// `replaced` and `dirs` from what `game` holds now. Two files that land on one
    /// path keep the later one (a support overlay comes after the tree).
    pub fn survey(&mut self, game: &Path) {
        let mut case = CaseMap::new(game);
        for f in &mut self.files {
            f.to = case.map(&f.to);
        }
        let mut seen = std::collections::HashSet::new();
        let mut kept: Vec<LayFile> = Vec::new();
        for f in self.files.drain(..).rev() {
            if seen.insert(f.to.clone()) {
                kept.push(f);
            }
        }
        kept.reverse();
        self.files = kept;
        self.replaced = self
            .files
            .iter()
            .filter(|f| game.join(&f.to).symlink_metadata().is_ok())
            .map(|f| f.to.clone())
            .collect();
        let mut dirs: Vec<String> = Vec::new();
        for f in &self.files {
            let mut parts: Vec<&str> = f.to.split('/').collect();
            parts.pop();
            let mut at = String::new();
            for p in parts {
                if !at.is_empty() {
                    at.push('/');
                }
                at.push_str(p);
                if !game.join(&at).exists() && !dirs.contains(&at) {
                    dirs.push(at.clone());
                }
            }
        }
        self.dirs = dirs;
        // Removed paths: on the game's casing, plain, files or links that are
        // there, and not a path a laid file takes.
        let laid: std::collections::HashSet<String> =
            self.files.iter().map(|f| f.to.to_lowercase()).collect();
        let mut removed: Vec<String> = Vec::new();
        for r in std::mem::take(&mut self.removed) {
            if !plain(&r) {
                continue;
            }
            let r = case.map(&r);
            let there = game.join(&r).symlink_metadata().is_ok_and(|m| !m.is_dir());
            if there && !laid.contains(&r.to_lowercase()) && !removed.contains(&r) {
                removed.push(r);
            }
        }
        self.removed = removed;
    }
}

/// A `/`-separated relative path with only plain components (no `..`, `.` or
/// empty one).
fn plain(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.starts_with('/')
        && rel
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..")
}

/// Maps a manifest path onto the casing already on disk under `game/`.
///
/// GOG's manifests are case-insensitive (gog-warp compares paths lowercased) and a
/// DLC depot can spell a dir differently from the base tree (`Archive/PC` against an
/// installed `archive/pc`). Joined as is on a case-sensitive filesystem, that path
/// would make a second dir that Wine sees as the same one, and a base file it
/// replaces would look new and not be backed up. So each component matches,
/// case-insensitively, what exists (an exact match first, else the first by name;
/// a dir for every component but the last). A new component keeps the manifest's
/// case, and later paths reuse it.
struct CaseMap<'a> {
    game: &'a Path,
    /// Lowercased prefix → the prefix chosen for it.
    chosen: std::collections::HashMap<String, String>,
    /// A dir's entries (name, is a dir), sorted; `None` when it doesn't exist.
    listed: std::collections::HashMap<String, Option<Vec<(String, bool)>>>,
}

impl<'a> CaseMap<'a> {
    fn new(game: &'a Path) -> Self {
        Self {
            game,
            chosen: Default::default(),
            listed: Default::default(),
        }
    }

    fn entries(&mut self, dir: &str) -> Option<&Vec<(String, bool)>> {
        let game = self.game;
        self.listed
            .entry(dir.to_string())
            .or_insert_with(|| {
                let rd = fs::read_dir(game.join(dir)).ok()?;
                let mut v: Vec<(String, bool)> = rd
                    .flatten()
                    .filter_map(|e| {
                        let name = e.file_name().into_string().ok()?;
                        Some((name, e.path().is_dir()))
                    })
                    .collect();
                v.sort();
                Some(v)
            })
            .as_ref()
    }

    fn map(&mut self, rel: &str) -> String {
        let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
        let mut out = String::new();
        for (i, comp) in parts.iter().enumerate() {
            let last = i + 1 == parts.len();
            let key = format!("{}/{}", out.to_lowercase(), comp.to_lowercase());
            let name = match self.chosen.get(&key) {
                Some(p) => p.rsplit('/').next().unwrap_or(comp).to_string(),
                None => {
                    let lower = comp.to_lowercase();
                    let found = self.entries(&out).and_then(|es| {
                        let fits =
                            |(n, d): &&(String, bool)| (last || *d) && n.to_lowercase() == lower;
                        es.iter()
                            .filter(fits)
                            .find(|(n, _)| n == comp)
                            .or_else(|| es.iter().find(fits))
                            .map(|(n, _)| n.clone())
                    });
                    found.unwrap_or_else(|| comp.to_string())
                }
            };
            if !out.is_empty() {
                out.push('/');
            }
            out.push_str(&name);
            self.chosen.entry(key).or_insert_with(|| out.clone());
        }
        out
    }
}

/// A step boundary of [`lay_files`], for a stop check (Ctrl-C) and the crash tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// After step 1: the journal is written, `game/` untouched.
    Journal,
    /// Between two files of step 2.
    File,
    /// After step 2: every `.gogo-new` is there.
    New,
    /// After step 3: the replaced base files are in the backup.
    Backup,
    /// After step 4: everything in place, not committed.
    Rename,
}

/// What [`lay_files`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayReport {
    pub files: u64,
    pub bytes: u64,
    /// Cloned, or renamed from a staging root: no extra disk.
    pub cloned_files: u64,
    pub copied_bytes: u64,
}

/// Why a lay or a recovery stopped.
#[derive(Debug, thiserror::Error)]
pub enum LayError {
    /// The stop check said so; the journal stays for [`recover`].
    #[error("stopped while laying the DLC files; the journal stays for the next run")]
    Stopped,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// The journal of the game dir `root`.
pub fn journal_path(root: &Path) -> PathBuf {
    root.join(JOURNAL)
}

/// The journal in `root`, if there is one. An unreadable journal is an error: the
/// game must not be taken as consistent.
pub fn read_journal(root: &Path) -> io::Result<Option<Journal>> {
    let raw = match fs::read_to_string(journal_path(root)) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|e| io::Error::other(format!("{}: {e}", journal_path(root).display())))
}

/// Write `value` as JSON to `path` durably: a temp file, fsynced, renamed, the dir
/// fsynced.
pub(crate) fn write_json_durably<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let text = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

fn new_of(game: &Path, rel: &str) -> PathBuf {
    game.join(format!("{rel}{NEW}"))
}

fn exists(p: &Path) -> bool {
    p.symlink_metadata().is_ok()
}

/// Put one file's `.gogo-new` in place from its source.
fn stage(f: &LayFile, game: &Path, j: &Journal, mode: Reflink) -> io::Result<(u64, bool)> {
    let new = new_of(game, &f.to);
    if exists(&new) {
        fs::remove_file(&new)?;
    }
    if let Some(parent) = new.parent() {
        fs::create_dir_all(parent)?;
    }
    let len = fs::symlink_metadata(&f.from)?.len();
    if j.moves {
        fs::rename(&f.from, &new)?;
        Ok((len, true))
    } else {
        Ok((len, materialize::clone_file(&f.from, &new, mode)?))
    }
}

/// Steps 3 and 4 for every file whose `.gogo-new` is there.
fn swap_in(root: &Path, j: &Journal, stop: &mut dyn FnMut(Step) -> bool) -> Result<(), LayError> {
    let game = root.join("game");
    let backup = root.join(BACKUP);
    for rel in &j.replaced {
        let (cur, bak) = (game.join(rel), backup.join(rel));
        if exists(&new_of(&game, rel)) && exists(&cur) && !exists(&bak) {
            if let Some(parent) = bak.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(&cur, &bak)?;
        }
    }
    for rel in &j.removed {
        let (cur, bak) = (game.join(rel), backup.join(rel));
        if exists(&cur) && !exists(&bak) {
            if let Some(parent) = bak.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(&cur, &bak)?;
        }
    }
    if stop(Step::Backup) {
        return Err(LayError::Stopped);
    }
    for f in &j.files {
        let new = new_of(&game, &f.to);
        if exists(&new) {
            fs::rename(&new, game.join(&f.to))?;
        }
    }
    let _ = materialize::sync_filesystem(&game);
    if stop(Step::Rename) {
        return Err(LayError::Stopped);
    }
    Ok(())
}

/// Steps 1 to 4 (module docs) for `j` in the game dir `root`. `stop` is asked at
/// each boundary and between files; `on_file` hears each laid file's size. The
/// caller then commits and calls [`finish`].
pub fn lay_files(
    root: &Path,
    j: &Journal,
    mode: Reflink,
    stop: &mut dyn FnMut(Step) -> bool,
    on_file: &mut dyn FnMut(u64),
) -> Result<LayReport, LayError> {
    let game = root.join("game");
    write_json_durably(&journal_path(root), j)?;
    if stop(Step::Journal) {
        return Err(LayError::Stopped);
    }
    let mut report = LayReport::default();
    for d in &j.dirs {
        fs::create_dir_all(game.join(d))?;
    }
    for f in &j.files {
        if stop(Step::File) {
            return Err(LayError::Stopped);
        }
        let (len, cloned) = stage(f, &game, j, mode)?;
        report.files += 1;
        report.bytes += len;
        if cloned {
            report.cloned_files += 1;
        } else {
            report.copied_bytes += len;
        }
        on_file(len);
    }
    let _ = materialize::sync_filesystem(&game);
    if stop(Step::New) {
        return Err(LayError::Stopped);
    }
    swap_in(root, j, stop)?;
    Ok(report)
}

/// The commit is done: delete the backups, the staging dir and, last, the journal.
pub fn finish(root: &Path) -> io::Result<()> {
    for d in [BACKUP, STAGING] {
        let p = root.join(d);
        if exists(&p) {
            materialize::remove_tree(&p)?;
        }
    }
    match fs::remove_file(journal_path(root)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Which way [`recover`] went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Every file is in place: commit, then [`finish`].
    Forward,
    /// A source file was gone: the game is as before, the journal deleted.
    Back,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Forward => "forward",
            Direction::Back => "back",
        }
    }
}

/// Settle what an interrupted lay left in the game dir `root`: `None` when there is
/// no journal. **Forward** when every file is laid, pending (`.gogo-new`) or still
/// in its source: the missing ones are laid again and everything is swapped in; the
/// caller then commits and calls [`finish`]. **Back** otherwise: laid DLC files
/// and `.gogo-new`s are removed, backups renamed back, created dirs removed when
/// empty, and the backup and staging dirs and the journal deleted.
pub fn recover(root: &Path, mode: Reflink) -> Result<Option<(Journal, Direction)>, LayError> {
    let Some(j) = read_journal(root)? else {
        return Ok(None);
    };
    let game = root.join("game");
    let backup = root.join(BACKUP);
    let in_place = |rel: &str| {
        if j.replaced.iter().any(|r| r == rel) {
            exists(&backup.join(rel)) && exists(&game.join(rel))
        } else {
            exists(&game.join(rel))
        }
    };
    let whole = j
        .files
        .iter()
        .all(|f| exists(&new_of(&game, &f.to)) || in_place(&f.to) || exists(&f.from));
    if whole {
        for d in &j.dirs {
            fs::create_dir_all(game.join(d))?;
        }
        for f in &j.files {
            if !exists(&new_of(&game, &f.to)) && !in_place(&f.to) {
                stage(f, &game, &j, mode)?;
            }
        }
        swap_in(root, &j, &mut |_| false)?;
        return Ok(Some((j, Direction::Forward)));
    }
    for f in &j.files {
        let (cur, new) = (game.join(&f.to), new_of(&game, &f.to));
        if exists(&new) {
            fs::remove_file(&new)?;
        }
        if j.replaced.contains(&f.to) {
            let bak = backup.join(&f.to);
            if exists(&bak) {
                fs::rename(&bak, &cur)?;
            }
        } else if exists(&cur) {
            fs::remove_file(&cur)?;
        }
    }
    for rel in &j.removed {
        let (cur, bak) = (game.join(rel), backup.join(rel));
        if exists(&bak) && !exists(&cur) {
            if let Some(parent) = cur.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(&bak, &cur)?;
        }
    }
    for d in j.dirs.iter().rev() {
        let _ = fs::remove_dir(game.join(d));
    }
    let _ = materialize::sync_filesystem(&game);
    finish(root)?;
    Ok(Some((j, Direction::Back)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Every file under `dir` (relative path → bytes), and every dir (→ None).
    fn tree(dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
            for e in fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                if p.is_dir() {
                    out.insert(rel, None);
                    walk(root, &p, out);
                } else {
                    out.insert(rel, Some(fs::read(&p).unwrap()));
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(dir, dir, &mut out);
        out
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        src: PathBuf,
    }

    /// A game dir with `game/base.dat`, `game/data/shared.pak` (a DLC replaces it)
    /// and a source holding the DLC's `data/shared.pak`, `dlc/ep1/a.archive` and
    /// `goggame-77.info`.
    fn fixture(staging: bool) -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("game-root");
        let game = root.join("game");
        fs::create_dir_all(game.join("data")).unwrap();
        fs::write(game.join("base.dat"), b"base").unwrap();
        fs::write(game.join("data/shared.pak"), b"old shared").unwrap();
        let src = if staging {
            root.join(STAGING).join("Test Game")
        } else {
            tmp.path().join("cache/Test Game")
        };
        fs::create_dir_all(src.join("data")).unwrap();
        fs::create_dir_all(src.join("dlc/ep1")).unwrap();
        fs::write(src.join("data/shared.pak"), b"new shared").unwrap();
        fs::write(src.join("dlc/ep1/a.archive"), b"dlc archive").unwrap();
        fs::write(src.join("goggame-77.info"), b"{}").unwrap();
        Fixture {
            _tmp: tmp,
            root,
            src,
        }
    }

    fn journal(f: &Fixture, moves: bool) -> Journal {
        let files = ["data/shared.pak", "dlc/ep1/a.archive", "goggame-77.info"]
            .iter()
            .map(|r| LayFile {
                from: f.src.join(r),
                to: r.to_string(),
            })
            .collect();
        let mut j = Journal {
            gogid: "1".into(),
            lane: "depot".into(),
            build_id: "B".into(),
            language: "en-US".into(),
            adding: vec![AddingDlc {
                id: 77,
                title: "Ep 1".into(),
            }],
            source: f.src.clone(),
            source_is_cache: !moves,
            moves,
            files,
            replaced: Vec::new(),
            dirs: Vec::new(),
            kind: None,
            from: None,
            to: None,
            removed: Vec::new(),
        };
        j.survey(&f.root.join("game"));
        j
    }

    fn after() -> BTreeMap<String, Option<Vec<u8>>> {
        let mut t = BTreeMap::new();
        t.insert("base.dat".into(), Some(b"base".to_vec()));
        t.insert("data".into(), None);
        t.insert("data/shared.pak".into(), Some(b"new shared".to_vec()));
        t.insert("dlc".into(), None);
        t.insert("dlc/ep1".into(), None);
        t.insert("dlc/ep1/a.archive".into(), Some(b"dlc archive".to_vec()));
        t.insert("goggame-77.info".into(), Some(b"{}".to_vec()));
        t
    }

    #[test]
    fn survey_finds_replaced_files_and_new_dirs() {
        let f = fixture(false);
        let j = journal(&f, false);
        assert_eq!(j.replaced, ["data/shared.pak"]);
        assert_eq!(j.dirs, ["dlc", "dlc/ep1"]);
    }

    #[test]
    fn a_lay_puts_new_files_in_place_and_backs_up_what_it_replaces() {
        for mode in [Reflink::Try, Reflink::Never] {
            let f = fixture(false);
            let j = journal(&f, false);
            let mut sizes = Vec::new();
            let r = lay_files(&f.root, &j, mode, &mut |_| false, &mut |n| sizes.push(n)).unwrap();
            assert_eq!(r.files, 3);
            assert_eq!(r.bytes, 10 + 11 + 2);
            assert_eq!(sizes.len(), 3);
            if mode == Reflink::Never {
                assert_eq!((r.cloned_files, r.copied_bytes), (0, 23));
            }
            assert_eq!(tree(&f.root.join("game")), after());
            assert_eq!(
                fs::read(f.root.join(BACKUP).join("data/shared.pak")).unwrap(),
                b"old shared"
            );
            assert!(read_journal(&f.root).unwrap().is_some());
            // The source (a cache) keeps its files.
            assert!(f.src.join("dlc/ep1/a.archive").exists());
            finish(&f.root).unwrap();
            assert!(!f.root.join(BACKUP).exists() && read_journal(&f.root).unwrap().is_none());
        }
    }

    /// A DLC path that differs only in case from an existing dir and file lands on
    /// the on-disk casing: the base file is seen as replaced and backed up, no
    /// second dir appears, and new components keep one spelling across files.
    #[test]
    fn paths_map_onto_the_casing_on_disk() {
        let f = fixture(false);
        fs::create_dir_all(f.src.join("Data")).unwrap();
        fs::write(f.src.join("Data/Shared.PAK"), b"new shared").unwrap();
        fs::write(f.src.join("Data/extra.pak"), b"extra").unwrap();
        fs::write(f.src.join("dlc/ep1/b.archive"), b"b").unwrap();
        let mut j = journal(&f, false);
        let src = |r: &str| f.src.join(r);
        j.files = vec![
            LayFile {
                from: src("Data/Shared.PAK"),
                to: "Data/Shared.PAK".into(),
            },
            LayFile {
                from: src("Data/extra.pak"),
                to: "DATA/extra.pak".into(),
            },
            LayFile {
                from: src("dlc/ep1/a.archive"),
                to: "Dlc/Ep1/a.archive".into(),
            },
            LayFile {
                from: src("dlc/ep1/b.archive"),
                to: "dlc/EP1/b.archive".into(),
            },
        ];
        j.survey(&f.root.join("game"));
        let tos: Vec<&str> = j.files.iter().map(|x| x.to.as_str()).collect();
        assert_eq!(
            tos,
            [
                "data/shared.pak",
                "data/extra.pak",
                "Dlc/Ep1/a.archive",
                "Dlc/Ep1/b.archive"
            ]
        );
        assert_eq!(j.replaced, ["data/shared.pak"]);
        assert_eq!(j.dirs, ["Dlc", "Dlc/Ep1"]);

        lay_files(&f.root, &j, Reflink::Never, &mut |_| false, &mut |_| {}).unwrap();
        let t = tree(&f.root.join("game"));
        let mut want = BTreeMap::new();
        want.insert("base.dat".to_string(), Some(b"base".to_vec()));
        want.insert("data".into(), None);
        want.insert("data/shared.pak".into(), Some(b"new shared".to_vec()));
        want.insert("data/extra.pak".into(), Some(b"extra".to_vec()));
        want.insert("Dlc".into(), None);
        want.insert("Dlc/Ep1".into(), None);
        want.insert("Dlc/Ep1/a.archive".into(), Some(b"dlc archive".to_vec()));
        want.insert("Dlc/Ep1/b.archive".into(), Some(b"b".to_vec()));
        assert_eq!(t, want);
        assert_eq!(
            fs::read(f.root.join(BACKUP).join("data/shared.pak")).unwrap(),
            b"old shared"
        );
    }

    /// An exact-case match wins over another entry that differs only in case, and
    /// a file never stands in for a dir component.
    #[test]
    fn an_exact_match_wins_and_files_are_not_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let game = tmp.path();
        fs::create_dir_all(game.join("Mods")).unwrap();
        // macOS's default filesystem is case-insensitive: there `mods` is `Mods`,
        // the one dir every spelling maps onto.
        let sensitive = fs::create_dir(game.join("mods")).is_ok();
        let want = if sensitive { "mods" } else { "Mods" };
        fs::write(game.join("tools"), b"a file").unwrap();
        let mut m = CaseMap::new(game);
        assert_eq!(m.map("mods/a"), format!("{want}/a"));
        assert_eq!(
            m.map("MODS/b"),
            format!("{want}/b"),
            "a later path reuses the choice"
        );
        assert_eq!(m.map("Tools/x.exe"), "Tools/x.exe");
        assert_eq!(m.map("TOOLS"), "Tools", "a new spelling sticks");
        let mut m = CaseMap::new(game);
        assert_eq!(m.map("TOOLS"), "tools", "the last component may be a file");
    }

    /// Two files that map onto one path keep the later one.
    #[test]
    fn survey_keeps_the_later_of_two_files_on_one_path() {
        let f = fixture(false);
        let mut j = journal(&f, false);
        j.files.push(LayFile {
            from: f.src.join("overlay/goggame-77.info"),
            to: "GOGGAME-77.info".into(),
        });
        j.survey(&f.root.join("game"));
        let infos: Vec<&LayFile> = j
            .files
            .iter()
            .filter(|x| x.to.eq_ignore_ascii_case("goggame-77.info"))
            .collect();
        assert_eq!(infos.len(), 1);
        assert!(infos[0].from.ends_with("overlay/goggame-77.info"));
    }

    #[test]
    fn a_staging_root_is_renamed_from_and_deleted_by_finish() {
        let f = fixture(true);
        let j = journal(&f, true);
        let r = lay_files(&f.root, &j, Reflink::Never, &mut |_| false, &mut |_| {}).unwrap();
        assert_eq!(r.cloned_files, 3, "renames cost no disk");
        assert!(!f.src.join("dlc/ep1/a.archive").exists());
        assert_eq!(tree(&f.root.join("game")), after());
        finish(&f.root).unwrap();
        assert!(!f.root.join(STAGING).exists());
    }

    /// Crash injection: a stop at each boundary (and mid-files), then recovery.
    /// Forward when the source is whole; back when a source file is gone; the game
    /// tree equals "before" or "after" byte for byte, never a mix.
    #[test]
    fn a_stop_at_any_step_recovers_forward_or_back_to_a_whole_tree() {
        let steps = [
            (Step::Journal, 1),
            (Step::File, 1),
            (Step::File, 2),
            (Step::File, 3),
            (Step::New, 1),
            (Step::Backup, 1),
            (Step::Rename, 1),
        ];
        for staging in [false, true] {
            for (at, nth) in steps {
                for lose_source in [false, true] {
                    let f = fixture(staging);
                    let before = tree(&f.root.join("game"));
                    let j = journal(&f, staging);
                    let mut seen = 0;
                    let err = lay_files(
                        &f.root,
                        &j,
                        Reflink::Never,
                        &mut |s| {
                            if s == at {
                                seen += 1;
                            }
                            s == at && seen == nth
                        },
                        &mut |_| {},
                    )
                    .unwrap_err();
                    assert!(matches!(err, LayError::Stopped));
                    assert!(journal_path(&f.root).exists(), "the journal stays");
                    // A pruned cache (or a lost staging file) loses one not yet laid;
                    // one already laid (or pending) doesn't need its source.
                    let lost = f.src.join("goggame-77.info");
                    let game_dir = f.root.join("game");
                    let laid = exists(&new_of(&game_dir, "goggame-77.info"))
                        || exists(&game_dir.join("goggame-77.info"));
                    let lost_any = lose_source && exists(&lost) && !laid;
                    if lost_any {
                        fs::remove_file(&lost).unwrap();
                    }
                    let (_, dir) = recover(&f.root, Reflink::Never).unwrap().unwrap();
                    let case = format!("staging={staging} at={at:?}#{nth} lose={lose_source}");
                    let game = tree(&f.root.join("game"));
                    if lost_any {
                        assert_eq!(dir, Direction::Back, "{case}");
                        assert_eq!(game, before, "{case}");
                        assert!(!journal_path(&f.root).exists(), "{case}");
                        assert!(!f.root.join(BACKUP).exists(), "{case}");
                    } else {
                        assert_eq!(dir, Direction::Forward, "{case}");
                        assert_eq!(game, after(), "{case}");
                        finish(&f.root).unwrap();
                    }
                    assert!(
                        recover(&f.root, Reflink::Never).unwrap().is_none(),
                        "{case}"
                    );
                }
            }
        }
    }

    /// An update's `removed` paths (design 21 §11g): mapped onto the game's casing,
    /// only files that are there and that no laid file takes; moved into the
    /// backup by the lay, put back by a rollback, and gone after `finish`.
    #[test]
    fn removed_paths_are_backed_up_and_restored_on_a_rollback() {
        let removed_after = |f: &Fixture| {
            let mut t = after();
            t.remove("base.dat");
            let _ = f;
            t
        };
        for (at, lose) in [
            (Step::Journal, false),
            (Step::Backup, false),
            (Step::Rename, false),
            (Step::Journal, true),
            (Step::Rename, true),
        ] {
            let f = fixture(false);
            let before = tree(&f.root.join("game"));
            let mut j = journal(&f, false);
            j.kind = Some(KIND_UPDATE.into());
            j.removed = vec![
                "BASE.dat".into(),
                "data/shared.pak".into(),
                "nothing/here".into(),
                "../outside".into(),
                "data".into(),
            ];
            j.survey(&f.root.join("game"));
            assert_eq!(
                j.removed,
                ["base.dat"],
                "the one plain file, on disk's casing"
            );
            let err =
                lay_files(&f.root, &j, Reflink::Never, &mut |s| s == at, &mut |_| {}).unwrap_err();
            assert!(matches!(err, LayError::Stopped));
            if at == Step::Rename {
                assert!(!f.root.join("game/base.dat").exists());
                assert_eq!(
                    fs::read(f.root.join(BACKUP).join("base.dat")).unwrap(),
                    b"base"
                );
            }
            if lose {
                let lost = f.src.join("goggame-77.info");
                let laid = exists(&f.root.join("game/goggame-77.info"))
                    || exists(&new_of(&f.root.join("game"), "goggame-77.info"));
                if !laid {
                    fs::remove_file(&lost).unwrap();
                } else {
                    continue;
                }
            }
            let (back, dir) = recover(&f.root, Reflink::Never).unwrap().unwrap();
            assert!(back.is_update());
            let game = tree(&f.root.join("game"));
            if lose {
                assert_eq!(dir, Direction::Back, "{at:?}");
                assert_eq!(game, before, "{at:?}");
            } else {
                assert_eq!(dir, Direction::Forward, "{at:?}");
                assert_eq!(game, removed_after(&f), "{at:?}");
                finish(&f.root).unwrap();
                assert!(!f.root.join(BACKUP).exists());
            }
        }
    }

    #[test]
    fn no_journal_is_nothing_to_recover() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(recover(tmp.path(), Reflink::Never).unwrap().is_none());
    }

    #[test]
    fn an_unreadable_journal_is_an_error_not_a_clean_game() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(JOURNAL), "{").unwrap();
        assert!(read_journal(tmp.path()).is_err());
    }
}
