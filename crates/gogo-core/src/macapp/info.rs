//! `goggame-*.info` parsing → launchable entry point (`docs/design/10` §2.4).
//!
//! GOG drops one `goggame-<id>.info` JSON per game/DLC into the install root.
//! The launchable exe is a `FileTask` in `playTasks` (prefer `isPrimary`, but
//! a `game` task over a primary `launcher` one — #107), relative to the info
//! file's directory. `.lnk`-only games are rejected loudly
//! — parsing Windows shortcuts is out of scope for v1 (design 10 §2.4).
//!
//! Every runnable task of the base game is also a launch variant (#85,
//! docs/19 §6.3): name, exe, args, working dir ([`play_variants`]).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::error::MacAppError;

#[derive(Debug, Clone, Deserialize)]
pub struct GogGameInfo {
    #[serde(rename = "gameId", default)]
    pub game_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "rootGameId", default)]
    pub root_game_id: Option<String>,
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
    #[serde(default)]
    pub name: Option<String>,
    /// One command-line string (`"-f"`, `"-window -nosound"`).
    #[serde(default)]
    pub arguments: Option<String>,
    /// Relative to the info file's directory, like `path`.
    #[serde(rename = "workingDir", default)]
    pub working_dir: Option<String>,
}

/// One runnable `playTasks` entry (#85): what a launch variant runs.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayVariant {
    /// The task's `name`, made unique across the game's variants.
    pub name: String,
    pub exe: PathBuf,
    pub workdir: PathBuf,
    pub args: Vec<String>,
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

/// The base game's product id: `rootGameId` from the infos (shared by the
/// base game and its DLCs), else the lowest id found. Keys the v2 state dir.
pub fn root_game_id(dir: &Path) -> Option<String> {
    collect_info_files(dir, 0)
        .iter()
        .find_map(|p| {
            let info: GogGameInfo = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
            info.root_game_id
        })
        .or_else(|| gog_game_ids(dir).into_iter().next())
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
    // Preference: primary task (unless it is GOG's launcher) > a `game` task
    // when the primary is a launcher (#107) > the primary launcher > first
    // runnable task. GOG marks `GoGLauncher.exe` primary for some games
    // (Rebel Galaxy) and hides the real game task; the launcher needs MFC
    // runtimes a fresh prefix lacks, the game doesn't.
    let mut primary_launcher: Option<PathBuf> = None;
    let mut game: Option<PathBuf> = None;
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
            let candidate = base.join(rel.replace('\\', "/"));
            if !candidate.is_file() {
                continue;
            }
            match (task.is_primary, task.category.as_deref()) {
                (true, Some("launcher")) => {
                    primary_launcher.get_or_insert(candidate.clone());
                }
                (true, _) => return Ok(candidate),
                (false, Some("game")) => {
                    game.get_or_insert(candidate.clone());
                }
                _ => {}
            }
            fallback.get_or_insert(candidate);
        }
    }
    if let Some(launcher) = primary_launcher {
        return Ok(game.unwrap_or(launcher));
    }
    fallback.ok_or(MacAppError::EntryPointNotFound {
        dir: dir.to_path_buf(),
    })
}

/// Every runnable `playTasks` entry of the base game, in info order (#85):
/// the same rules as [`find_entry_point`] (a `FileTask` to an `.exe` that
/// exists on disk; URL and `.lnk` tasks are skipped), and no DLC's tasks (an
/// info whose `gameId` isn't the `rootGameId`). Duplicate names get the exe
/// name appended (Rebel Galaxy names its launcher and its game alike).
pub fn play_variants(dir: &Path) -> Vec<PlayVariant> {
    let mut infos = collect_info_files(dir, 0);
    infos.sort();
    let mut out: Vec<PlayVariant> = Vec::new();
    for info_path in &infos {
        let Ok(raw) = std::fs::read_to_string(info_path) else {
            continue;
        };
        let Ok(info) = serde_json::from_str::<GogGameInfo>(&raw) else {
            continue;
        };
        if let (Some(id), Some(root)) = (&info.game_id, &info.root_game_id)
            && id != root
        {
            continue;
        }
        let base = info_path.parent().unwrap_or(dir);
        for task in &info.play_tasks {
            if !is_runnable_file_task(task) {
                continue;
            }
            let Some(rel) = task.path.as_deref() else {
                continue;
            };
            let exe = base.join(rel.replace('\\', "/"));
            if !exe.is_file() {
                continue;
            }
            let file = exe
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut name = task
                .name
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .unwrap_or(&file)
                .to_string();
            if out.iter().any(|v| v.name == name) {
                name = format!("{name} ({file})");
            }
            let mut n = 2;
            let stem = name.clone();
            while out.iter().any(|v| v.name == name) {
                name = format!("{stem} #{n}");
                n += 1;
            }
            let workdir = match task.working_dir.as_deref().filter(|w| !w.is_empty()) {
                Some(w) => base.join(w.replace('\\', "/")),
                None => exe.parent().unwrap_or(base).to_path_buf(),
            };
            out.push(PlayVariant {
                name,
                exe,
                workdir,
                args: split_args(task.arguments.as_deref().unwrap_or_default()),
                is_primary: task.is_primary,
            });
        }
    }
    out
}

/// A `playTasks` `arguments` string as argv: split on whitespace, with
/// double quotes grouping (`-cfg "my file.ini"`). No other escapes.
pub fn split_args(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in raw.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(cur);
    }
    out
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

    /// Rebel Galaxy (1435582019): GOG marks `GoGLauncher.exe` primary and
    /// hides the real game task (#107).
    #[test]
    fn hidden_game_task_beats_primary_launcher() {
        let tmp = tempfile::tempdir().unwrap();
        write_info(
            tmp.path(),
            "1435582019",
            r#"{"gameId":"1435582019","name":"Rebel Galaxy","playTasks":[
                {"category":"launcher","isPrimary":true,"name":"Rebel Galaxy",
                 "path":"GoGLauncher.exe","type":"FileTask"},
                {"category":"game","isHidden":true,"name":"Rebel Galaxy",
                 "path":"RebelGalaxyGOG.exe","type":"FileTask"},
                {"category":"document","name":"Manual","path":"manual.pdf","type":"FileTask"}
            ]}"#,
        );
        std::fs::write(tmp.path().join("GoGLauncher.exe"), b"MZ").unwrap();
        std::fs::write(tmp.path().join("RebelGalaxyGOG.exe"), b"MZ").unwrap();
        let entry = find_entry_point(tmp.path()).unwrap();
        assert_eq!(entry, tmp.path().join("RebelGalaxyGOG.exe"));
    }

    #[test]
    fn primary_launcher_without_game_task_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        write_info(
            tmp.path(),
            "6",
            r#"{"playTasks":[
                {"type":"FileTask","category":"tool","path":"tool.exe"},
                {"type":"FileTask","category":"launcher","path":"GoGLauncher.exe","isPrimary":true}
            ]}"#,
        );
        std::fs::write(tmp.path().join("tool.exe"), b"MZ").unwrap();
        std::fs::write(tmp.path().join("GoGLauncher.exe"), b"MZ").unwrap();
        let entry = find_entry_point(tmp.path()).unwrap();
        assert_eq!(entry, tmp.path().join("GoGLauncher.exe"));
    }

    #[test]
    fn game_task_missing_on_disk_falls_back_to_primary_launcher() {
        let tmp = tempfile::tempdir().unwrap();
        write_info(
            tmp.path(),
            "7",
            r#"{"playTasks":[
                {"type":"FileTask","category":"launcher","path":"GoGLauncher.exe","isPrimary":true},
                {"type":"FileTask","category":"game","path":"ghost.exe","isHidden":true}
            ]}"#,
        );
        std::fs::write(tmp.path().join("GoGLauncher.exe"), b"MZ").unwrap();
        let entry = find_entry_point(tmp.path()).unwrap();
        assert_eq!(entry, tmp.path().join("GoGLauncher.exe"));
    }

    /// Carmageddon TDR 2000 (1146738698)-like tasks: one exe with `-f` /
    /// `-s`, an alternate exe, a settings tool, plus what must be skipped.
    fn tdr_like(root: &Path) {
        write_info(
            root,
            "1146738698",
            r#"{"gameId":"1146738698","rootGameId":"1146738698","name":"Carmageddon TDR 2000",
              "playTasks":[
                {"category":"game","isPrimary":true,"name":"Carmageddon TDR 2000",
                 "path":"TDR2000.exe","arguments":"-f","type":"FileTask"},
                {"category":"game","name":"Carmageddon TDR 2000 (safe mode)",
                 "path":"TDR2000.exe","arguments":"-s","type":"FileTask"},
                {"category":"game","name":"Carmageddon TDR 2000 (3dfx)",
                 "path":"bin\\TDR2000_3dfx.exe","workingDir":".","type":"FileTask"},
                {"category":"tool","name":"Settings","path":"Setup.exe",
                 "arguments":"-cfg \"tdr settings.ini\"","type":"FileTask"},
                {"category":"tool","name":"Editor","path":"ghost.exe","type":"FileTask"},
                {"category":"document","name":"Manual","path":"manual.pdf","type":"FileTask"},
                {"category":"other","name":"Support","link":"https://gog.com","type":"URLTask"}
              ]}"#,
        );
        write_info(
            &root.join("dlc"),
            "1207000001",
            r#"{"gameId":"1207000001","rootGameId":"1146738698",
              "playTasks":[{"category":"game","name":"DLC","path":"dlc.exe","type":"FileTask"}]}"#,
        );
        for f in [
            "TDR2000.exe",
            "bin/TDR2000_3dfx.exe",
            "Setup.exe",
            "manual.pdf",
            "dlc/dlc.exe",
        ] {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"MZ").unwrap();
        }
    }

    #[test]
    fn play_tasks_become_variants() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        tdr_like(root);
        let v = play_variants(root);
        let names: Vec<&str> = v.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Carmageddon TDR 2000",
                "Carmageddon TDR 2000 (safe mode)",
                "Carmageddon TDR 2000 (3dfx)",
                "Settings"
            ]
        );
        assert!(v[0].is_primary && !v[1].is_primary);
        assert_eq!(v[0].exe, root.join("TDR2000.exe"));
        assert_eq!(v[0].args, ["-f"]);
        assert_eq!(v[0].workdir, root);
        assert_eq!(v[1].exe, root.join("TDR2000.exe"));
        assert_eq!(v[1].args, ["-s"]);
        assert_eq!(v[2].exe, root.join("bin/TDR2000_3dfx.exe"));
        assert!(v[2].args.is_empty());
        // An explicit `workingDir` wins over the exe's directory.
        assert_eq!(v[2].workdir, root.join("."));
        assert_eq!(v[3].exe, root.join("Setup.exe"));
        assert_eq!(v[3].args, ["-cfg", "tdr settings.ini"]);
        // The primary pick is unchanged and is one of the variants.
        assert_eq!(find_entry_point(root).unwrap(), v[0].exe);
    }

    #[test]
    fn duplicate_task_names_are_made_unique() {
        let tmp = tempfile::tempdir().unwrap();
        write_info(
            tmp.path(),
            "1435582019",
            r#"{"playTasks":[
                {"category":"launcher","isPrimary":true,"name":"Rebel Galaxy",
                 "path":"GoGLauncher.exe","type":"FileTask"},
                {"category":"game","isHidden":true,"name":"Rebel Galaxy",
                 "path":"RebelGalaxyGOG.exe","type":"FileTask"},
                {"category":"game","name":"Rebel Galaxy","path":"RebelGalaxyGOG.exe",
                 "arguments":"-safe","type":"FileTask"},
                {"category":"game","path":"Nameless.exe","type":"FileTask"}
            ]}"#,
        );
        for f in ["GoGLauncher.exe", "RebelGalaxyGOG.exe", "Nameless.exe"] {
            std::fs::write(tmp.path().join(f), b"MZ").unwrap();
        }
        let names: Vec<String> = play_variants(tmp.path())
            .into_iter()
            .map(|v| v.name)
            .collect();
        assert_eq!(
            names,
            [
                "Rebel Galaxy",
                "Rebel Galaxy (RebelGalaxyGOG.exe)",
                "Rebel Galaxy (RebelGalaxyGOG.exe) #2",
                "Nameless.exe"
            ]
        );
    }

    #[test]
    fn arguments_split_like_a_command_line() {
        assert!(split_args("").is_empty());
        assert!(split_args("   ").is_empty());
        assert_eq!(split_args(" -f  -w "), ["-f", "-w"]);
        assert_eq!(split_args(r#"-cfg "a b.ini" x"#), ["-cfg", "a b.ini", "x"]);
        assert_eq!(split_args(r#""""#), [""]);
    }
}
