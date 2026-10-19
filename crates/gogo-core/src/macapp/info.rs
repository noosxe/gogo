//! `goggame-*.info` parsing → launchable entry point (`docs/design/10` §2.4).
//!
//! GOG drops one `goggame-<id>.info` JSON per game/DLC into the install root.
//! The launchable exe is a `FileTask` in `playTasks` (prefer `isPrimary`),
//! relative to the info file's directory. `.lnk`-only games are rejected loudly
//! — parsing Windows shortcuts is out of scope for v1 (design 10 §2.4).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::error::MacAppError;

#[derive(Debug, Clone, Deserialize)]
pub struct GogGameInfo {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "playTasks", default)]
    pub play_tasks: Vec<GogTask>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GogTask {
    /// "FileTask" | "URLTask" | …
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    /// "game" | "document" | "launcher" | …
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(rename = "isPrimary", default)]
    pub is_primary: bool,
}

/// Find the exe to launch: scan `dir` (and one nested level, for DLC roots)
/// for `goggame-*.info`, pick the primary game `FileTask`, resolve relative to
/// the info file, and require the target to exist on disk.
/// All GOG game ids found in `goggame-<id>.info` file names (base + DLCs).
pub fn gog_game_ids(dir: &Path) -> Vec<String> {
    let mut ids = collect_info_files(dir, 0)
        .iter()
        .filter_map(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix("goggame-"))
                .and_then(|n| n.strip_suffix(".info"))
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

/// The `gameName` from any `goggame-*.info` in `dir` (first hit).
pub fn game_name(dir: &Path) -> Option<String> {
    collect_info_files(dir, 0).iter().find_map(|p| {
        let info: GogGameInfo = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
        info.name
    })
}

pub fn find_entry_point(dir: &Path) -> Result<PathBuf, MacAppError> {
    let infos = collect_info_files(dir, 0);
    if infos.is_empty() {
        return Err(MacAppError::EntryPointNotFound {
            dir: dir.to_path_buf(),
        });
    }
    // Primary task anywhere wins; else first runnable game task.
    let mut fallback: Option<PathBuf> = None;
    for info_path in &infos {
        let Ok(raw) = std::fs::read_to_string(info_path) else {
            continue;
        };
        let Ok(info) = serde_json::from_str::<GogGameInfo>(&raw) else {
            continue; // a malformed DLC info must not sink the game
        };
        let base = info_path.parent().unwrap_or(dir);
        for task in &info.play_tasks {
            if !is_runnable_file_task(task) {
                continue;
            }
            let Some(rel) = task.path.as_deref() else {
                continue;
            };
            let candidate = base.join(rel);
            if !candidate.is_file() {
                continue;
            }
            if task.is_primary {
                return Ok(candidate);
            }
            fallback.get_or_insert(candidate);
        }
    }
    fallback.ok_or(MacAppError::EntryPointNotFound {
        dir: dir.to_path_buf(),
    })
}

fn is_runnable_file_task(task: &GogTask) -> bool {
    task.kind.as_deref() == Some("FileTask")
        && task.path.as_deref().is_some_and(|p| p.to_lowercase().ends_with(".exe"))
        // `.lnk` shortcuts (category "game" but path to a shortcut) are skipped.
        && !task.path.as_deref().is_some_and(|p| p.to_lowercase().ends_with(".lnk"))
}

fn collect_info_files(dir: &Path, depth: u8) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth < 1 {
                found.extend(collect_info_files(&path, depth + 1));
            }
        } else if path.file_name().is_some_and(|n| {
            let n = n.to_string_lossy();
            n.starts_with("goggame-") && n.ends_with(".info")
        }) {
            found.push(path);
        }
    }
    found
}

#[cfg(test)]
mod info_tests {
    use super::*;

    fn write_info(dir: &Path, id: &str, body: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("goggame-{id}.info"));
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn primary_game_task_wins() {
        let tmp = tempfile::tempdir().unwrap();
        write_info(
            tmp.path(),
            "1",
            r#"{"name":"T","playTasks":[
                {"type":"FileTask","category":"game","path":"alt.exe","isPrimary":false},
                {"type":"FileTask","category":"game","path":"main.exe","isPrimary":true}
            ]}"#,
        );
        std::fs::write(tmp.path().join("alt.exe"), b"MZ").unwrap();
        std::fs::write(tmp.path().join("main.exe"), b"MZ").unwrap();
        let entry = find_entry_point(tmp.path()).unwrap();
        assert_eq!(entry, tmp.path().join("main.exe"));
    }

    #[test]
    fn url_tasks_and_lnk_tasks_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        write_info(
            tmp.path(),
            "2",
            r#"{"playTasks":[
                {"type":"URLTask","link":"https://gog.com","isPrimary":true},
                {"type":"FileTask","category":"game","path":"game.lnk","isPrimary":true}
            ]}"#,
        );
        let err = find_entry_point(tmp.path()).unwrap_err();
        assert!(matches!(err, MacAppError::EntryPointNotFound { .. }));
    }

    #[test]
    fn missing_exe_on_disk_is_not_runnable() {
        let tmp = tempfile::tempdir().unwrap();
        write_info(
            tmp.path(),
            "3",
            r#"{"playTasks":[{"type":"FileTask","category":"game","path":"ghost.exe","isPrimary":true}]}"#,
        );
        let err = find_entry_point(tmp.path()).unwrap_err();
        assert!(matches!(err, MacAppError::EntryPointNotFound { .. }));
    }

    #[test]
    fn dlc_info_one_level_down_is_found() {
        let tmp = tempfile::tempdir().unwrap();
        let dlc = tmp.path().join("dlc_dir");
        write_info(
            &dlc,
            "4",
            r#"{"playTasks":[{"type":"FileTask","category":"game","path":"dlc.exe","isPrimary":true}]}"#,
        );
        std::fs::create_dir_all(&dlc).unwrap();
        std::fs::write(dlc.join("dlc.exe"), b"MZ").unwrap();
        let entry = find_entry_point(tmp.path()).unwrap();
        assert_eq!(entry, dlc.join("dlc.exe"));
    }

    #[test]
    fn non_primary_game_task_is_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        write_info(
            tmp.path(),
            "5",
            r#"{"playTasks":[
                {"type":"FileTask","category":"document","path":"readme.txt"},
                {"type":"FileTask","category":"game","path":"game.exe","isPrimary":false}
            ]}"#,
        );
        std::fs::write(tmp.path().join("game.exe"), b"MZ").unwrap();
        let entry = find_entry_point(tmp.path()).unwrap();
        assert_eq!(entry, tmp.path().join("game.exe"));
    }
}
