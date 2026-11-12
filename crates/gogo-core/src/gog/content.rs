//! `games/<gogid>/content.toml` (design 19 §6): what content a built game has: the
//! lane, the build, the language and the DLC. Kept apart from `gogo-launch.toml` so
//! adding a DLC changes no launch stamp and an older gogo doesn't refuse the game.
//!
//! A game built before this has no file. [`resolve`] then derives what it can from the
//! cache record (`.gogo-depot.json`, when the build came from one) and the game's
//! `goggame-*.info` files, and says so (`derived`). Reading never writes the file.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::gog::depot;
use crate::macapp::info;

/// The file's name in a game dir.
pub const FILE: &str = "content.toml";

/// The schema this gogo reads and writes.
pub const SCHEMA: u32 = 1;

/// Which lane built the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lane {
    Depot,
    Installer,
}

impl Lane {
    pub fn as_str(self) -> &'static str {
        match self {
            Lane::Depot => "depot",
            Lane::Installer => "installer",
        }
    }
}

/// One DLC the game has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DlcRow {
    pub id: u64,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added: Option<String>,
}

/// The file's content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Content {
    pub schema: u32,
    pub lane: Lane,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The branch of the build, only when it isn't the default one (design 21 §5.1).
    /// An older gogo ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, rename = "dlc", skip_serializing_if = "Vec::is_empty")]
    pub dlc: Vec<DlcRow>,
}

/// What [`resolve`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub content: Content,
    /// Derived from the cache record and the infos: there is no file.
    pub derived: bool,
}

/// What a reader shows of one DLC (`--json` rows of `apps`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DlcRef {
    pub id: u64,
    pub title: String,
}

impl Content {
    pub fn refs(&self) -> Vec<DlcRef> {
        self.dlc
            .iter()
            .map(|d| DlcRef {
                id: d.id,
                title: d.title.clone(),
            })
            .collect()
    }
}

/// `content.toml` in the game dir `root`.
pub fn path(root: &Path) -> PathBuf {
    root.join(FILE)
}

/// Why a file couldn't be read.
#[derive(Debug, thiserror::Error)]
pub enum ContentError {
    #[error("{}: {reason}", path.display())]
    Invalid { path: PathBuf, reason: String },
}

/// The file in game dir `root`: `Ok(None)` when there is none.
pub fn load(root: &Path) -> Result<Option<Content>, ContentError> {
    let p = path(root);
    let raw = match std::fs::read_to_string(&p) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(ContentError::Invalid {
                path: p,
                reason: e.to_string(),
            });
        }
    };
    let c: Content = toml::from_str(&raw).map_err(|e| ContentError::Invalid {
        path: p.clone(),
        reason: e.message().to_string(),
    })?;
    if c.schema != SCHEMA {
        return Err(ContentError::Invalid {
            path: p,
            reason: format!("schema {} isn't one this gogo knows ({SCHEMA})", c.schema),
        });
    }
    Ok(Some(c))
}

/// Write `content` into game dir `root` atomically (a temp file, then a rename).
pub fn write(root: &Path, content: &Content) -> std::io::Result<()> {
    let text = toml::to_string_pretty(content).map_err(std::io::Error::other)?;
    let p = path(root);
    let tmp = root.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, p).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// What the game files in `install_dir` say: the base game's build, and a row for
/// every DLC info beside it. `added` stamps the DLC rows.
fn from_infos(install_dir: &Path, added: Option<&str>) -> (Option<String>, Vec<DlcRow>) {
    let infos = info::read_infos(install_dir);
    let root = info::root_game_id(install_dir);
    let base = infos
        .iter()
        .find(|(id, _)| Some(id) == root.as_ref())
        .and_then(|(_, i)| i.build_id_text());
    let dlc = infos
        .iter()
        .filter(|(id, _)| Some(id) != root.as_ref())
        .filter_map(|(id, i)| {
            Some(DlcRow {
                id: id.parse().ok()?,
                title: i.name.clone().unwrap_or_else(|| id.clone()),
                build_id: i.build_id_text(),
                version: None,
                added: added.map(str::to_string),
            })
        })
        .collect();
    (base, dlc)
}

/// What a build of `install_dir` has. `record_root` is the depot cache root the tree
/// came from, when there is one: its record gives the build and language.
/// `installed` is the DLC the build put in (design 19 §3.3): exactly those get rows,
/// titled by their info when it's there. `None` (a tree given by path, the installer
/// lane) records every DLC with an info in the tree.
pub fn for_build(
    lane: Lane,
    install_dir: &Path,
    record_root: Option<&Path>,
    installed: Option<&[DlcRef]>,
) -> Content {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let (info_build, present) = from_infos(install_dir, Some(&today));
    let dlc = match installed {
        None => present,
        Some(want) => want
            .iter()
            .map(|w| {
                present
                    .iter()
                    .find(|p| p.id == w.id)
                    .cloned()
                    .unwrap_or_else(|| DlcRow {
                        id: w.id,
                        title: w.title.clone(),
                        build_id: None,
                        version: None,
                        added: Some(today.clone()),
                    })
            })
            .collect(),
    };
    let rec = record_root.and_then(depot::read_record);
    Content {
        schema: SCHEMA,
        lane,
        build_id: rec.as_ref().map(|r| r.build_id.clone()).or(info_build),
        version: rec.as_ref().and_then(|r| r.version.clone()),
        branch: None,
        language: rec.and_then(|r| r.language),
        dlc,
    }
}

/// The content of the game in `root` (its game dir) whose files are in `install_dir`:
/// the file, else derived. A derived record trusts `record_root`'s record only when
/// it is for the build the infos name (or they name none). `Err` for a file that
/// can't be read.
pub fn resolve(
    root: &Path,
    install_dir: &Path,
    lane: Lane,
    record_root: Option<&Path>,
) -> Result<Resolved, ContentError> {
    if let Some(content) = load(root)? {
        return Ok(Resolved {
            content,
            derived: false,
        });
    }
    let (info_build, dlc) = from_infos(install_dir, None);
    let rec = record_root
        .and_then(depot::read_record)
        .filter(|r| info_build.as_ref().is_none_or(|b| *b == r.build_id));
    Ok(Resolved {
        content: Content {
            schema: SCHEMA,
            lane,
            build_id: rec.as_ref().map(|r| r.build_id.clone()).or(info_build),
            version: rec.as_ref().and_then(|r| r.version.clone()),
            branch: None,
            language: rec.and_then(|r| r.language),
            dlc,
        },
        derived: true,
    })
}

/// The DLC ids with an info in `install_dir` that `content` doesn't list, and the
/// listed rows whose info is missing.
pub fn mismatches(content: &Content, install_dir: &Path) -> (Vec<(u64, String)>, Vec<DlcRow>) {
    let (_, present) = from_infos(install_dir, None);
    let unlisted = present
        .iter()
        .filter(|p| !content.dlc.iter().any(|d| d.id == p.id))
        .map(|p| (p.id, p.title.clone()))
        .collect();
    let lost = content
        .dlc
        .iter()
        .filter(|d| !present.iter().any(|p| p.id == d.id))
        .cloned()
        .collect();
    (unlisted, lost)
}

/// `Phantom Liberty, REDmod`, or `none`.
pub fn titles(content: &Content) -> String {
    if content.dlc.is_empty() {
        "none".to_string()
    } else {
        content
            .dlc
            .iter()
            .map(|d| d.title.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(dir: &Path, id: &str, root: &str, name: &str, build: Option<&str>) {
        let b = build.map_or(String::new(), |b| format!(r#","buildId":"{b}""#));
        std::fs::write(
            dir.join(format!("goggame-{id}.info")),
            format!(
                r#"{{"gameId":"{id}","rootGameId":"{root}","name":"{name}"{b},"playTasks":[]}}"#
            ),
        )
        .unwrap();
    }

    fn sample() -> Content {
        Content {
            schema: 1,
            lane: Lane::Depot,
            build_id: Some("5555".into()),
            version: Some("2.31a".into()),
            branch: None,
            language: Some("en-US".into()),
            dlc: vec![DlcRow {
                id: 12,
                title: "Expansion".into(),
                build_id: Some("5555".into()),
                version: None,
                added: Some("2026-11-11".into()),
            }],
        }
    }

    #[test]
    fn branch_is_written_only_when_set_and_old_files_read_without_it() {
        let tmp = tempfile::tempdir().unwrap();
        // No branch: the file has no `branch` key, as before.
        write(tmp.path(), &sample()).unwrap();
        let raw = std::fs::read_to_string(path(tmp.path())).unwrap();
        assert!(!raw.contains("branch"), "{raw}");
        // A named branch round-trips.
        let mut c = sample();
        c.branch = Some("1_63_legacy_patch".into());
        write(tmp.path(), &c).unwrap();
        let raw = std::fs::read_to_string(path(tmp.path())).unwrap();
        assert!(raw.contains("branch = \"1_63_legacy_patch\""), "{raw}");
        assert_eq!(load(tmp.path()).unwrap(), Some(c));
        // A file an older gogo wrote reads with `branch` None.
        std::fs::write(
            path(tmp.path()),
            "schema = 1\nlane = \"depot\"\nbuild_id = \"5555\"\nversion = \"2.31a\"\n",
        )
        .unwrap();
        assert_eq!(load(tmp.path()).unwrap().unwrap().branch, None);
        // An older gogo reading a file with a branch ignores the key: `Content` has
        // no `deny_unknown_fields`, which the same read shows.
        let with = "schema = 1\nlane = \"depot\"\nbranch = \"x\"\nsurprise = 1\n";
        std::fs::write(path(tmp.path()), with).unwrap();
        assert!(load(tmp.path()).is_ok());
    }

    #[test]
    fn round_trips_and_leaves_no_temp_file() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(load(tmp.path()).unwrap(), None);
        write(tmp.path(), &sample()).unwrap();
        assert_eq!(load(tmp.path()).unwrap(), Some(sample()));
        let raw = std::fs::read_to_string(path(tmp.path())).unwrap();
        assert!(
            raw.contains("schema = 1") && raw.contains("[[dlc]]"),
            "{raw}"
        );
        assert!(raw.contains("lane = \"depot\""), "{raw}");
        let names: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from(FILE)]);
        // A second write replaces the first.
        let mut c = sample();
        c.dlc.clear();
        write(tmp.path(), &c).unwrap();
        assert_eq!(load(tmp.path()).unwrap(), Some(c));
    }

    #[test]
    fn an_unreadable_or_newer_file_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(path(tmp.path()), "schema = [").unwrap();
        assert!(load(tmp.path()).is_err());
        std::fs::write(path(tmp.path()), "schema = 2\nlane = \"depot\"\n").unwrap();
        let e = load(tmp.path()).unwrap_err().to_string();
        assert!(e.contains("schema 2"), "{e}");
    }

    #[test]
    fn a_failed_write_keeps_the_old_file() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), &sample()).unwrap();
        // The temp name is taken by a directory: the write fails before the rename.
        std::fs::create_dir(tmp.path().join(format!("{FILE}.tmp"))).unwrap();
        let mut c = sample();
        c.build_id = Some("9".into());
        assert!(write(tmp.path(), &c).is_err());
        assert_eq!(load(tmp.path()).unwrap(), Some(sample()));
    }

    #[test]
    fn derives_from_infos_and_a_matching_record() {
        let tmp = tempfile::tempdir().unwrap();
        let game = tmp.path().join("game");
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        info(&game, "1", "1", "Base", Some("5555"));
        info(&game, "12", "1", "Expansion", Some("5555"));
        info(&game, "13", "1", "Pack", None);
        depot::write_record(&cache, "5555", "en-US", &[]).unwrap();

        let r = resolve(tmp.path(), &game, Lane::Depot, Some(&cache)).unwrap();
        assert!(r.derived);
        assert_eq!(r.content.build_id.as_deref(), Some("5555"));
        assert_eq!(r.content.language.as_deref(), Some("en-US"));
        let ids: Vec<u64> = r.content.dlc.iter().map(|d| d.id).collect();
        assert_eq!(ids, [12, 13]);
        assert_eq!(titles(&r.content), "Expansion, Pack");
        // Reading wrote nothing.
        assert!(!path(tmp.path()).exists());

        // A record for another build isn't this game's.
        depot::write_record(&cache, "9999", "de-DE", &[]).unwrap();
        let r = resolve(tmp.path(), &game, Lane::Depot, Some(&cache)).unwrap();
        assert_eq!(r.content.build_id.as_deref(), Some("5555"));
        assert_eq!(r.content.language, None);
    }

    #[test]
    fn derives_from_infos_alone_and_from_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let game = tmp.path().join("game");
        std::fs::create_dir_all(&game).unwrap();
        let r = resolve(tmp.path(), &game, Lane::Installer, None).unwrap();
        assert!(r.derived && r.content.dlc.is_empty() && r.content.build_id.is_none());
        assert_eq!(r.content.lane, Lane::Installer);
        info(&game, "1", "1", "Base", None);
        let r = resolve(tmp.path(), &game, Lane::Installer, None).unwrap();
        assert!(r.content.dlc.is_empty() && r.content.language.is_none());
    }

    #[test]
    fn the_file_wins_over_the_infos() {
        let tmp = tempfile::tempdir().unwrap();
        let game = tmp.path().join("game");
        std::fs::create_dir_all(&game).unwrap();
        info(&game, "1", "1", "Base", None);
        info(&game, "13", "1", "Pack", None);
        write(tmp.path(), &sample()).unwrap();
        let r = resolve(tmp.path(), &game, Lane::Depot, None).unwrap();
        assert!(!r.derived);
        assert_eq!(r.content, sample());
        let (unlisted, lost) = mismatches(&r.content, &game);
        assert_eq!(unlisted, [(13, "Pack".to_string())]);
        assert_eq!(lost.len(), 1);
        assert_eq!(lost[0].id, 12);
    }

    #[test]
    fn a_build_records_dlc_already_in_the_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let game = tmp.path().join("game");
        std::fs::create_dir_all(&game).unwrap();
        info(&game, "1", "1", "Base", Some("7"));
        info(&game, "12", "1", "Expansion", Some("7"));
        let c = for_build(Lane::Installer, &game, None, None);
        assert_eq!(c.build_id.as_deref(), Some("7"));
        assert_eq!(c.dlc.len(), 1);
        assert!(c.dlc[0].added.is_some());
        // Given the installed set, exactly that is recorded: an info alone doesn't
        // make a row, and a wanted DLC without one (zero bytes) gets its title.
        let want = [DlcRef {
            id: 13,
            title: "Cosmetic".into(),
        }];
        let c = for_build(Lane::Depot, &game, None, Some(&want));
        assert_eq!(c.dlc.len(), 1);
        assert_eq!((c.dlc[0].id, c.dlc[0].title.as_str()), (13, "Cosmetic"));
        let want = [DlcRef {
            id: 12,
            title: "from the catalog".into(),
        }];
        let c = for_build(Lane::Depot, &game, None, Some(&want));
        assert_eq!(c.dlc[0].title, "Expansion");
        assert!(
            for_build(Lane::Depot, &game, None, Some(&[]))
                .dlc
                .is_empty()
        );
    }
}
