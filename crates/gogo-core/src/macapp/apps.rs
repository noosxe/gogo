//! `gogo apps`: the games gogo set up on this Mac, one per
//! `games/<gogid>/` (prefix, game files, `state.toml`), with the launch
//! logs and the override file that go with each. Listing reads only;
//! [`remove`] deletes a game's state and never touches its `.app`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::error::MacAppError;
use super::launch::{self, GameDirs, GameState, LaunchToml};
use super::{compat, doctor, portable};

/// One set-up game.
#[derive(Debug, Clone, Serialize)]
pub struct App {
    pub gogid: String,
    pub title: Option<String>,
    /// `games/<gogid>/`.
    pub root: PathBuf,
    pub prefix: PathBuf,
    /// The `.app` it was last set up from: `state.toml`'s, or the portable
    /// launcher's marker.
    pub bundle: Option<PathBuf>,
    pub bundle_present: bool,
    /// Set up by a portable bundle's launcher, not by gogo.
    pub portable: bool,
    pub runtime: Option<String>,
    pub renderer: Option<String>,
    /// Logical size of `games/<gogid>/` (cloned game files count in full,
    /// though they share blocks with the depot cache).
    pub bytes: u64,
    /// `~/Library/Logs/gogo/<gogid>/`, when it exists.
    pub logs: Option<PathBuf>,
    /// The newest launch log's start, `YYYY-MM-DD HH:MM` (local time).
    pub last_launch: Option<String>,
    /// `compat/<gogid>.toml`, when it exists.
    pub override_file: Option<PathBuf>,
}

impl App {
    /// The game's `.app` is recorded but no longer where it was. (A moved
    /// bundle updates the record on its next launch.)
    pub fn orphaned(&self) -> bool {
        self.bundle.is_some() && !self.bundle_present
    }
}

/// Every game under `games/`, by gogid. `logs` is the launch-log root
/// (`~/Library/Logs/gogo`).
pub fn list(logs: &Path) -> Result<Vec<App>, MacAppError> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(launch::games_dir()) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    for e in entries.flatten() {
        // Skips `.build-*` staging dirs and anything else not a game.
        let Some(gogid) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if compat::validate_gogid(&gogid).is_ok() && e.path().is_dir() {
            out.push(read(&gogid, logs)?);
        }
    }
    out.sort_by(|a, b| a.gogid.cmp(&b.gogid));
    Ok(out)
}

/// The game `target` (a gogid or its `.app`) names, if it is set up here.
pub fn get(target: &str, logs: &Path) -> Result<Option<App>, MacAppError> {
    let gogid = super::setup::target_gogid(target)?;
    let dirs = GameDirs::for_game(&gogid)?;
    if !dirs.root.is_dir() {
        return Ok(None);
    }
    read(&gogid, logs).map(Some)
}

fn read(gogid: &str, logs: &Path) -> Result<App, MacAppError> {
    let dirs = GameDirs::for_game(gogid)?;
    // A state.toml gogo can't read still leaves a game to list or remove.
    let state = GameState::load(&dirs.state_toml).ok().flatten();
    let marker = state.is_none().then(|| portable::marker(gogid)).flatten();
    let bundle = state
        .as_ref()
        .map(|s| s.bundle.clone())
        .or_else(|| marker.as_ref().map(|m| m.bundle.clone()));
    let bundle_present = bundle.as_deref().is_some_and(Path::is_dir);
    let title = state
        .as_ref()
        .and_then(|s| s.title.clone())
        .or_else(|| conf_title(&dirs.launch_conf))
        .or_else(|| bundle.as_deref().and_then(bundle_title));
    let logs = Some(logs.join(gogid)).filter(|p| p.is_dir());
    let override_file = Some(compat::user_path(gogid)).filter(|p| p.is_file());
    Ok(App {
        gogid: gogid.to_string(),
        title,
        bytes: super::store::dir_size(&dirs.root).unwrap_or(0),
        prefix: dirs.prefix.clone(),
        root: dirs.root,
        bundle,
        bundle_present,
        portable: marker.is_some(),
        runtime: state.as_ref().map(|s| s.runtime.clone()),
        renderer: state.as_ref().map(|s| s.renderer.clone()),
        last_launch: logs.as_deref().and_then(last_launch),
        logs,
        override_file,
    })
}

/// `TITLE='…'` from a `launch.conf` (states written before `title` was
/// recorded).
fn conf_title(conf: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(conf).ok()?;
    let v = raw.lines().find_map(|l| l.strip_prefix("TITLE="))?;
    let v = v.strip_prefix('\'')?.strip_suffix('\'')?;
    Some(v.replace("'\\''", "'"))
}

fn bundle_title(bundle: &Path) -> Option<String> {
    let toml = bundle.join("Contents/Resources/gogo-launch.toml");
    let raw = std::fs::read_to_string(&toml).ok()?;
    LaunchToml::parse(&raw, &toml).ok().map(|t| t.title)
}

/// The newest `YYYYMMDD-HHMMSS-<pid>.log` in `dir`, as `YYYY-MM-DD HH:MM`.
fn last_launch(dir: &Path) -> Option<String> {
    let newest = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        // Not `latest.log` and the like, which sort after the dated ones.
        .filter(|n| n.ends_with(".log") && n.starts_with(|c: char| c.is_ascii_digit()))
        .max()?;
    let b = newest.as_bytes();
    let digits = |r: std::ops::Range<usize>| {
        b.get(r.clone())
            .filter(|s| s.iter().all(u8::is_ascii_digit))
            .map(|_| &newest[r])
    };
    Some(format!(
        "{}-{}-{} {}:{}",
        digits(0..4)?,
        digits(4..6)?,
        digits(6..8)?,
        digits(9..11)?,
        digits(11..13)?
    ))
}

/// What [`remove`] deleted.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Removed {
    pub paths: Vec<PathBuf>,
    pub bytes: u64,
}

/// Delete `app`'s state dir (prefix and saves included) and launch logs,
/// and its override file unless `keep_override`. Refuses while the game or
/// any Wine process in its prefix is running. The `.app` is left alone.
pub fn remove(app: &App, keep_override: bool) -> Result<Removed, MacAppError> {
    if let Some(pid) = doctor::live_launcher(&app.root) {
        return Err(MacAppError::Apps(format!(
            "{} is running (launcher {pid}): quit it first",
            app.gogid
        )));
    }
    let procs = doctor::prefix_procs(&app.prefix);
    if !procs.is_empty() {
        return Err(MacAppError::Apps(format!(
            "{} Wine process(es) still running in {}'s prefix: stop them with \
             `gogo mac-app doctor {} --stop-leftovers`",
            procs.len(),
            app.gogid,
            app.gogid
        )));
    }
    let mut out = Removed {
        bytes: app.bytes,
        ..Removed::default()
    };
    std::fs::remove_dir_all(&app.root)?;
    out.paths.push(app.root.clone());
    if let Some(logs) = &app.logs {
        std::fs::remove_dir_all(logs)?;
        out.paths.push(logs.clone());
    }
    if let Some(file) = app.override_file.as_ref().filter(|_| !keep_override) {
        std::fs::remove_file(file)?;
        out.paths.push(file.clone());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_home(dir: &Path) -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::test_util::env_lock();
        // SAFETY: env mutation under the shared env mutex.
        unsafe { std::env::set_var("GOGO_HOME", dir) };
        guard
    }

    fn game(gogid: &str, bundle: &Path, title: Option<&str>) -> PathBuf {
        let dir = launch::games_dir().join(gogid);
        std::fs::create_dir_all(dir.join("prefix/drive_c")).unwrap();
        let title = title
            .map(|t| format!("title = \"{t}\"\n"))
            .unwrap_or_default();
        std::fs::write(
            dir.join("state.toml"),
            format!(
                "schema = 1\ngogid = \"{gogid}\"\n{title}bundle = \"{}\"\n\
                 runtime = \"winecx/1\"\nrenderer = \"wined3d\"\nstamp = \"s\"\n\
                 gogo_version = \"0\"\n",
                bundle.display()
            ),
        )
        .unwrap();
        dir
    }

    #[test]
    fn lists_games_with_their_bundle_logs_and_override() {
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        let logs = home.path().join("Logs");
        let present = home.path().join("Here.app");
        std::fs::create_dir_all(&present).unwrap();
        game("1207659013", &present, Some("Here"));
        let gone = game("1435582019", &home.path().join("Gone.app"), None);
        std::fs::write(gone.join("launch.conf"), "TITLE='It'\\''s gone'\n").unwrap();
        std::fs::create_dir_all(launch::games_dir().join(".build-x-1")).unwrap();
        std::fs::create_dir_all(logs.join("1435582019")).unwrap();
        for n in [
            "20261023-160501-1.log",
            "20261024-091500-2.log",
            "latest.log",
        ] {
            std::fs::write(logs.join("1435582019").join(n), "").unwrap();
        }
        compat::set_user_keys("1435582019", &[("renderer", Some("wined3d"))]).unwrap();

        let apps = list(&logs).unwrap();
        let ids: Vec<&str> = apps.iter().map(|a| a.gogid.as_str()).collect();
        assert_eq!(ids, ["1207659013", "1435582019"]);
        let (here, gone) = (&apps[0], &apps[1]);
        assert_eq!(here.title.as_deref(), Some("Here"));
        assert!(here.bundle_present && !here.orphaned());
        assert_eq!(here.last_launch, None);
        assert_eq!(gone.title.as_deref(), Some("It's gone"));
        assert!(gone.orphaned());
        assert_eq!(gone.last_launch.as_deref(), Some("2026-10-24 09:15"));
        assert!(gone.override_file.is_some());
        assert_eq!(gone.runtime.as_deref(), Some("winecx/1"));
        assert!(get("1669531175", &logs).unwrap().is_none());
    }

    #[test]
    fn remove_takes_state_logs_and_override_but_not_the_bundle() {
        let home = tempfile::tempdir().unwrap();
        let _g = env_home(home.path());
        let logs = home.path().join("Logs");
        let bundle = home.path().join("Game.app");
        std::fs::create_dir_all(&bundle).unwrap();
        let root = game("1207659013", &bundle, None);
        std::fs::create_dir_all(logs.join("1207659013")).unwrap();
        compat::set_user_keys("1207659013", &[("renderer", Some("wined3d"))]).unwrap();

        let app = get("1207659013", &logs).unwrap().unwrap();
        let out = remove(&app, true).unwrap();
        assert_eq!(out.paths, [root.clone(), logs.join("1207659013")]);
        assert!(!root.exists() && bundle.is_dir());
        assert!(compat::user_path("1207659013").is_file());

        game("1207659013", &bundle, None);
        let app = get("1207659013", &logs).unwrap().unwrap();
        remove(&app, false).unwrap();
        assert!(!compat::user_path("1207659013").exists());
    }
}
