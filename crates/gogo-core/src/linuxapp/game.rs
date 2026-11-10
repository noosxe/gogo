//! A Linux game's files (design 18 §0, §7.1): `games/<gogid>/` holds
//! `gogo-launch.toml` (what the build baked, design 16 §1.1's schema),
//! `launch.conf` (the effective config setup writes for `launch.sh`) and
//! `state.toml` (what setup applied, the store's reference list for GC).
//!
//! The stamp is design 16 §2.1's, over this dir's `gogo-launch.toml` and
//! the L4 file: [`current_stamp`] is what setup records and what the
//! launcher (#177) and `doctor` (#178) recompute.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::macapp::compat;
use crate::macapp::compat::{DisplayMode, Hdr};
use crate::macapp::error::MacAppError;
use crate::macapp::launch::{
    GameDirs, LaunchToml, in_library, sh_array, sh_quote, stamp, state_rel,
};

/// `games/<gogid>/gogo-launch.toml`: one copy, beside the prefix (there's
/// no bundle on Linux).
pub const LAUNCH_TOML: &str = "gogo-launch.toml";

/// `state.toml`'s schema.
pub const STATE_SCHEMA: u32 = 1;

/// `games/<gogid>/launch.sh`, the launcher #177 generates beside
/// `launch.conf`.
pub const LAUNCH_SH: &str = "launch.sh";

/// The game's menu entry (design 18 §6, §7.1):
/// `$XDG_DATA_HOME/applications/gogo-<gogid>.desktop`. `GOGO_HOME` collapses
/// it into `$GOGO_HOME/applications/`, as it does every other dir, so a
/// scratch home never writes the user's menu.
pub fn desktop_path(gogid: &str) -> PathBuf {
    let dir = match std::env::var_os("GOGO_HOME") {
        Some(home) => PathBuf::from(home),
        None => dirs::data_dir().unwrap_or_else(crate::state::data_root),
    };
    dir.join("applications")
        .join(format!("gogo-{gogid}.desktop"))
}

/// Processes of this user running in `prefix`: those whose environment
/// names it (design 18 §7.2: Linux lets a user read their own processes'
/// `/proc/<pid>/environ`, which sees into umu's container where a cwd
/// wouldn't). Inside the container Proton runs Wine with
/// `WINEPREFIX=<prefix>/pfx/` (umu's `pfx -> .` link) and
/// `STEAM_COMPAT_DATA_PATH=<prefix>` (#177's real smoke), so those count
/// too, with or without a trailing `/`. Never this process.
pub fn prefix_procs(prefix: &Path) -> Vec<crate::macapp::doctor::PrefixProc> {
    let canon = prefix.canonicalize().ok();
    let wanted: Vec<Vec<u8>> = std::iter::once(prefix.to_path_buf())
        .chain(canon)
        .flat_map(|p| {
            [
                ("WINEPREFIX=", p.clone()),
                ("WINEPREFIX=", p.join("pfx")),
                ("STEAM_COMPAT_DATA_PATH=", p),
            ]
        })
        .map(|(k, p)| {
            let mut v = k.as_bytes().to_vec();
            v.extend_from_slice(p.as_os_str().as_encoded_bytes());
            v
        })
        .collect();
    let me = std::process::id();
    let mut out: Vec<_> = std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let pid: u32 = e.file_name().to_str()?.parse().ok()?;
            if pid == me {
                return None;
            }
            let env = std::fs::read(e.path().join("environ")).ok()?;
            env.split(|b| *b == 0)
                .any(|kv| {
                    let kv = kv.strip_suffix(b"/").unwrap_or(kv);
                    wanted.iter().any(|w| kv == w.as_slice())
                })
                .then(|| crate::macapp::doctor::PrefixProc {
                    pid,
                    name: std::fs::read_to_string(e.path().join("comm"))
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                })
        })
        .collect();
    out.sort_by_key(|p| p.pid);
    out
}

/// The pid of a running launcher of this game: a `.launchers/<pid>` record
/// (#177) whose process is alive, not a zombie, and started when the record
/// says (field 22 of `/proc/<pid>/stat`, as `launch.sh`'s `proc_start`
/// writes it), so a reused pid doesn't count.
pub fn live_launcher(game_root: &Path) -> Option<u32> {
    std::fs::read_dir(game_root.join(".launchers"))
        .ok()?
        .flatten()
        .find_map(|e| {
            let pid: u32 = e.file_name().to_str()?.parse().ok()?;
            let start = std::fs::read_to_string(e.path()).ok()?;
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            // After the parenthesized name: state is field 3, starttime 22.
            let rest: Vec<&str> = stat.rsplit_once(") ")?.1.split_whitespace().collect();
            (rest.first() != Some(&"Z") && rest.get(19) == Some(&start.trim())).then_some(pid)
        })
}

/// Refuse while the game's launcher or anything in its prefix runs: what
/// `apps remove` and `linux-app move` both can't work around.
pub fn refuse_running(gogid: &str, root: &Path, prefix: &Path) -> Result<(), MacAppError> {
    if let Some(pid) = live_launcher(root) {
        return Err(MacAppError::Apps(format!(
            "{gogid} is running (launcher {pid}): quit it first"
        )));
    }
    let procs = prefix_procs(prefix);
    if !procs.is_empty() {
        let pids: Vec<String> = procs
            .iter()
            .map(|p| format!("{} ({})", p.pid, p.name))
            .collect();
        return Err(MacAppError::Apps(format!(
            "{} process(es) still running in {gogid}'s prefix: {} — quit the game or stop \
             them first",
            procs.len(),
            pids.join(", ")
        )));
    }
    Ok(())
}

/// `launch.conf` for a game moved from `from` to `to`, the result what
/// [`crate::linuxapp::setup::setup`] would write there and every other line as
/// it was: paths under the data root are relative to it, the rest absolute, and
/// a game outside `games/` is marked. Running it twice changes nothing.
pub fn relocated_conf(text: &str, from: &Path, to: &Path, data_root: &Path) -> String {
    const PATHS: [&str; 4] = ["PREFIX", "GAME_DIR", "WORKDIR", "EXE"];
    let rehome = |value: &str| {
        let abs = data_root.join(value);
        match abs.strip_prefix(from) {
            Ok(rest) if rest.as_os_str().is_empty() => state_rel(data_root, to),
            Ok(rest) => state_rel(data_root, &to.join(rest)),
            Err(_) => value.to_string(),
        }
    };
    let mut out = String::with_capacity(text.len());
    let mut has_root = false;
    for line in text.split_inclusive('\n') {
        let key = line.split_once('=').map_or("", |(k, _)| k);
        if PATHS.contains(&key) {
            let value = ConfValues::parse(line);
            out.push_str(&format!("{key}={}\n", sh_quote(&rehome(value.get(key)))));
        } else if key == "DATA_ROOT" {
            has_root = true;
            out.push_str(&format!(
                "DATA_ROOT={}\n",
                sh_quote(&data_root.display().to_string())
            ));
        } else if key != "LIBRARY_GAME" {
            out.push_str(line);
        }
    }
    if !has_root {
        out.push_str(&format!(
            "DATA_ROOT={}\n",
            sh_quote(&data_root.display().to_string())
        ));
    }
    if in_library(to) {
        out.push_str(&format!("LIBRARY_GAME={}\n", sh_quote("1")));
    }
    out
}

/// `launch.conf` read back: the values of [`LaunchConf::render`]'s
/// assignments, POSIX single quotes undone. Arrays (`ARGS`, `ENV`) are in
/// `arrays`, everything else in `values`. Lines that aren't assignments
/// (the header comment) are skipped; a value gogo never writes (unquoted,
/// or quoted another way) is kept as written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfValues {
    pub values: BTreeMap<String, String>,
    pub arrays: BTreeMap<String, Vec<String>>,
}

impl ConfValues {
    pub fn parse(text: &str) -> ConfValues {
        let mut out = ConfValues::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            if k.is_empty() || !k.bytes().all(|b| b.is_ascii_uppercase() || b == b'_') {
                continue;
            }
            match v.strip_prefix('(').and_then(|v| v.strip_suffix(')')) {
                Some(items) => {
                    out.arrays.insert(k.to_string(), sh_words(items));
                }
                None => {
                    let words = sh_words(v);
                    let value = match words.as_slice() {
                        [one] => one.clone(),
                        [] => String::new(),
                        _ => v.to_string(),
                    };
                    out.values.insert(k.to_string(), value);
                }
            }
        }
        out
    }

    pub fn read(path: &Path) -> std::io::Result<ConfValues> {
        Ok(ConfValues::parse(&std::fs::read_to_string(path)?))
    }

    /// A value, `""` when unset.
    pub fn get(&self, key: &str) -> &str {
        self.values.get(key).map(String::as_str).unwrap_or("")
    }
}

/// Words of `sh_quote`d values separated by spaces: `'a'\''b' 'c'` is
/// `a'b`, `c`.
fn sh_words(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur: Option<String> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                let w = cur.get_or_insert_with(String::new);
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    w.push(q);
                }
            }
            '\\' => {
                if let Some(n) = chars.next() {
                    cur.get_or_insert_with(String::new).push(n);
                }
            }
            ' ' | '\t' => words.extend(cur.take()),
            c => cur.get_or_insert_with(String::new).push(c),
        }
    }
    words.extend(cur);
    words
}

/// `games/<gogid>/gogo-launch.toml`.
pub fn launch_toml_path(dirs: &GameDirs) -> PathBuf {
    dirs.root.join(LAUNCH_TOML)
}

/// Read and parse a game's `gogo-launch.toml`, with its bytes (the stamp's
/// first input).
pub fn read_launch_toml(dirs: &GameDirs) -> Result<(LaunchToml, Vec<u8>), MacAppError> {
    let path = launch_toml_path(dirs);
    let raw = std::fs::read(&path).map_err(|e| MacAppError::Compat {
        path: path.clone(),
        reason: if e.kind() == std::io::ErrorKind::NotFound {
            "no gogo-launch.toml: build the game first (`gogo linux-app <game>`)".into()
        } else {
            e.to_string()
        },
    })?;
    let toml = LaunchToml::parse(&String::from_utf8_lossy(&raw), &path)?;
    Ok((toml, raw))
}

/// The stamp of `gogid`'s config as it is on disk now: `gogo-launch.toml`,
/// then the L4 file (zero bytes when there's none). `None` without a
/// `gogo-launch.toml`. The same sha256 `launch.sh` computes with
/// `cat gogo-launch.toml [l4] | sha256sum`.
pub fn current_stamp(gogid: &str) -> Result<Option<String>, MacAppError> {
    let dirs = GameDirs::for_game(gogid)?;
    let toml = match std::fs::read(launch_toml_path(&dirs)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let l4 = match std::fs::read(compat::user_path(gogid)) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    Ok(Some(stamp(&toml, l4.as_deref())))
}

/// `games/<gogid>/state.toml` on Linux: what the last setup applied. The
/// store reads `runtime` and `renderer` as references (design 16 decision
/// 6, design 18 §6); `protonfix` is the module's hash (§4.2) setup compares.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinuxState {
    pub schema: u32,
    pub gogid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `ge-proton/<ver>` or `path:<dir>`.
    pub runtime: String,
    /// The renderer setup applied (after any fallback).
    pub renderer: String,
    /// `umu-<id>`, as `GAMEID` got it.
    pub umu_id: String,
    /// The game's `gamefixes-gog` module's sha256, or `none`.
    pub protonfix: String,
    /// The verbs setup asked winetricks for (the effective ones plus the
    /// protonfix's), whether or not they were already in the prefix.
    #[serde(default)]
    pub verbs: Vec<String>,
    /// The registry values setup wrote ([`registry_line`]): written again
    /// only when these change or the prefix is new.
    #[serde(default)]
    pub registry: Vec<String>,
    /// The compat `files` setup wrote and still holds as its own, one
    /// `<sha256> <path>` line each ([`super::setup::write_files`]): a file
    /// that still has that hash is gogo's to update.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    pub stamp: String,
    pub gogo_version: String,
}

impl LinuxState {
    pub fn load(path: &Path) -> Result<Option<LinuxState>, MacAppError> {
        match std::fs::read_to_string(path) {
            Ok(raw) => toml::from_str(&raw)
                .map(Some)
                .map_err(|e| MacAppError::Compat {
                    path: path.to_path_buf(),
                    reason: e.to_string(),
                }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("LinuxState always serializes")
    }
}

/// One registry value as `state.toml` records it.
pub fn registry_line(v: &compat::RegistryValue) -> String {
    format!(
        "{}\\{}={}:{}",
        v.key,
        v.name.as_deref().unwrap_or("@"),
        v.kind.as_deref().unwrap_or("REG_SZ"),
        v.data
    )
}

/// `games/<gogid>/launch.conf` (design 18 §7.1): assignments only, POSIX
/// single-quoted, for `launch.sh` to source. Paths under the data root are
/// relative to it (O7); the launcher resolves them against `DATA_ROOT`, or
/// `${XDG_DATA_HOME:-$HOME/.local/share}/gogo` when that's gone.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchConf {
    pub gogid: String,
    pub title: String,
    /// `GAMEID`.
    pub umu_id: String,
    pub stamp: String,
    /// The store zipapp (relative) or a system `umu-run` (absolute).
    pub umu_run: String,
    /// Empty for a system umu.
    pub umu_python: String,
    /// `PROTONPATH`: relative for a store build, absolute for one in a
    /// shared tools dir or an unmanaged one.
    pub proton_dir: String,
    /// The build's `version` file, trimmed (the launch check, §2.2).
    pub proton_version: String,
    /// sha256 of the build's `proton` script.
    pub proton_sha: String,
    /// `WINEPREFIX`, always set (R7).
    pub prefix: String,
    pub game_dir: String,
    /// Unix paths through the prefix (`…/prefix/drive_c/GOG/<name>/…`), so
    /// Wine sees the game as `C:\GOG\<name>` with `z:` gone.
    pub workdir: String,
    pub exe: String,
    pub args: Vec<String>,
    /// The command the game runs under, one word each (#271); empty: none,
    /// and no `WRAPPER=` line.
    pub wrapper: Vec<String>,
    /// Wine's display driver (#292); `None`: no `DISPLAY_MODE=` line, and
    /// the launcher leaves the environment's choice alone.
    pub display: Option<DisplayMode>,
    /// The HDR switch for Wayland mode; `None` and `Auto`: no
    /// `DISPLAY_HDR=` line.
    pub hdr: Option<Hdr>,
    /// The primary monitor for Wayland mode; `None`: no `DISPLAY_MONITOR=` line.
    pub monitor: Option<String>,
    /// gogo's env, the renderer switches and `PROTONFIXES_DISABLE` included.
    pub env: BTreeMap<String, String>,
    pub dll_overrides: BTreeMap<String, String>,
    pub renderer: String,
    /// `false`: the launcher exports `PROTONFIXES_DISABLE=1`.
    pub protonfixes: bool,
    /// `UMU_FOLDERS_PATH`, only when gogo moves `HOME`/`XDG_DATA_HOME`
    /// (never in the first round).
    pub umu_folders: Option<String>,
    pub gogo_path: String,
    pub data_root: String,
    /// The game isn't under `<data_root>/games/` (design 18 §6.1): its
    /// launcher takes the data root from `DATA_ROOT`, not from where it is.
    /// `false`: no `LIBRARY_GAME=` line.
    pub library_game: bool,
}

impl LaunchConf {
    pub fn render(&self) -> String {
        let mut out = String::from(
            "# Generated by gogo linux-app setup from gogo-launch.toml and the override \
             file. Do not edit.\n",
        );
        let mut kv = |k: &str, v: String| out.push_str(&format!("{k}={v}\n"));
        kv("GOGID", sh_quote(&self.gogid));
        kv("TITLE", sh_quote(&self.title));
        kv("UMU_ID", sh_quote(&self.umu_id));
        kv("STAMP", sh_quote(&self.stamp));
        kv("UMU_RUN", sh_quote(&self.umu_run));
        kv("UMU_PYTHON", sh_quote(&self.umu_python));
        kv("PROTON_DIR", sh_quote(&self.proton_dir));
        kv("PROTON_VERSION", sh_quote(&self.proton_version));
        kv("PROTON_SHA", sh_quote(&self.proton_sha));
        kv("PREFIX", sh_quote(&self.prefix));
        kv("GAME_DIR", sh_quote(&self.game_dir));
        kv("WORKDIR", sh_quote(&self.workdir));
        kv("EXE", sh_quote(&self.exe));
        kv("ARGS", sh_array(self.args.iter().cloned()));
        if !self.wrapper.is_empty() {
            kv("WRAPPER", sh_array(self.wrapper.iter().cloned()));
        }
        if let Some(d) = self.display {
            kv("DISPLAY_MODE", sh_quote(d.key()));
        }
        if let Some(h) = self.hdr.filter(|h| *h != Hdr::Auto) {
            kv("DISPLAY_HDR", sh_quote(h.key()));
        }
        if let Some(m) = &self.monitor {
            kv("DISPLAY_MONITOR", sh_quote(m));
        }
        kv(
            "ENV",
            sh_array(self.env.iter().map(|(k, v)| format!("{k}={v}"))),
        );
        let dll: Vec<String> = self
            .dll_overrides
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        kv("DLL_OVERRIDES", sh_quote(&dll.join(";")));
        kv("RENDERER", sh_quote(&self.renderer));
        kv(
            "PROTONFIXES",
            sh_quote(if self.protonfixes { "1" } else { "0" }),
        );
        if let Some(f) = &self.umu_folders {
            kv("UMU_FOLDERS", sh_quote(f));
        }
        kv("GOGO_PATH", sh_quote(&self.gogo_path));
        kv("DATA_ROOT", sh_quote(&self.data_root));
        if self.library_game {
            kv("LIBRARY_GAME", sh_quote("1"));
        }
        out
    }
}

/// The umu FHS env (`steam.buildRuntimeEnv`, #168) has a private `/tmp`:
/// a path under it that gogo hands umu (prefix, Proton dir, exe, working
/// dir) isn't there inside. Refuse it up front with what to do instead.
pub fn refuse_private_tmp(what: &str, path: &Path) -> Result<(), MacAppError> {
    let abs = crate::macapp::launch::absolute(path);
    if abs == Path::new("/tmp") || abs.starts_with("/tmp/") {
        return Err(MacAppError::Umu(format!(
            "{what} {} is under /tmp, which umu's container doesn't share (it has a \
             private /tmp): keep gogo's data dir (GOGO_HOME / XDG_DATA_HOME), game files \
             and installers elsewhere",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conf() -> LaunchConf {
        LaunchConf {
            gogid: "1207659013".into(),
            title: "Treasure Adventure Game".into(),
            umu_id: "umu-1207659013".into(),
            stamp: "ab12".into(),
            umu_run: "runtimes/umu/1.4.4/umu/umu-run".into(),
            umu_python: "/nix/store/x-umu-python/bin/gogo-umu-python".into(),
            proton_dir: "runtimes/ge-proton/GE-Proton11-12/x86_64/GE-Proton11-12-x86_64".into(),
            proton_version: "1793687165 GE-Proton11-12".into(),
            proton_sha: "cd34".into(),
            prefix: "games/1207659013/prefix".into(),
            game_dir: "games/1207659013/game".into(),
            workdir: "games/1207659013/prefix/drive_c/GOG/Treasure Adventure Game".into(),
            exe: "games/1207659013/prefix/drive_c/GOG/Treasure Adventure Game/Game.exe".into(),
            // GOG's args, then #234's extra args, with what a shell would
            // touch: spaces, both quotes, `$`, backticks, a backslash.
            args: vec![
                "-windowed".into(),
                "it's".into(),
                "-skipStartScreen".into(),
                "two words".into(),
                "$HOME \"q\" `x` \\".into(),
            ],
            wrapper: vec![],
            display: None,
            hdr: None,
            monitor: None,
            env: [
                ("PROTON_USE_D7VK".to_string(), "1".to_string()),
                ("ODD".to_string(), "a 'b' $X".to_string()),
            ]
            .into(),
            dll_overrides: [("ddraw".to_string(), "n,b".to_string())].into(),
            renderer: "d7vk".into(),
            protonfixes: true,
            umu_folders: None,
            gogo_path: "/home/me/.nix-profile/bin/gogo".into(),
            data_root: "/home/me/.local/share/gogo".into(),
            library_game: false,
        }
    }

    /// A move's rewrite is what setup writes at the new place, every other
    /// line untouched, in both directions and through a quote in the path.
    #[test]
    fn a_relocated_conf_is_what_setup_would_write_there() {
        let (data, from) = (Path::new("/d"), Path::new("/d/games/7"));
        let to = Path::new("/big drive/it's/7");
        let render = |root: &Path, library_game: bool, rel: bool| {
            let mut c = conf();
            let p = |tail: &str| {
                let abs = root.join(tail);
                if rel {
                    abs.strip_prefix(data).unwrap().display().to_string()
                } else {
                    abs.display().to_string()
                }
            };
            c.prefix = p("prefix");
            c.game_dir = p("game");
            c.workdir = p("prefix/drive_c/GOG/x y");
            c.exe = p("prefix/drive_c/GOG/x y/a.exe");
            c.data_root = "/d".into();
            c.library_game = library_game;
            c.render()
        };
        let (at_home, away) = (render(from, false, true), render(to, true, false));
        let _guard = crate::test_util::env_lock();
        // SAFETY: under the crate's env lock.
        unsafe { std::env::set_var("GOGO_HOME", data) };
        assert_eq!(relocated_conf(&at_home, from, to, data), away);
        assert_eq!(relocated_conf(&away, from, to, data), away, "twice is once");
        assert_eq!(relocated_conf(&away, to, from, data), at_home);
        // SAFETY: as above.
        unsafe { std::env::remove_var("GOGO_HOME") };
    }

    #[test]
    fn launch_conf_matches_the_golden_file() {
        let golden = include_str!("../../tests/fixtures/linuxapp/launch.conf.golden");
        assert_eq!(conf().render(), golden);
    }

    /// `WRAPPER=` only with a wrapper: every word as typed, right after `ARGS`.
    #[test]
    fn launch_conf_with_a_wrapper_matches_its_golden_file() {
        let mut c = conf();
        c.wrapper = [
            "gamescope",
            "-W",
            "2560",
            "it's",
            "$HOME \"q\" `x` \\",
            "--",
        ]
        .map(String::from)
        .to_vec();
        let text = c.render();
        assert_eq!(
            text,
            include_str!("../../tests/fixtures/linuxapp/launch-wrapper.conf.golden")
        );
        assert_eq!(ConfValues::parse(&text).arrays["WRAPPER"], c.wrapper);
    }

    /// `DISPLAY_MODE=` and `DISPLAY_HDR=` only when set (#292), after any
    /// `WRAPPER=`; `hdr = "auto"` writes nothing, as no `hdr` does.
    #[test]
    fn launch_conf_has_the_display_mode_only_when_set() {
        let mut c = conf();
        let plain = c.render();
        c.hdr = Some(Hdr::Auto);
        assert_eq!(c.render(), plain);
        c.display = Some(DisplayMode::Wayland);
        c.hdr = Some(Hdr::Force);
        c.monitor = Some("DP-1".into());
        let text = c.render();
        let added: Vec<&str> = text
            .lines()
            .filter(|l| !plain.lines().any(|p| p == *l))
            .collect();
        assert_eq!(
            added,
            [
                "DISPLAY_MODE='wayland'",
                "DISPLAY_HDR='force'",
                "DISPLAY_MONITOR='DP-1'"
            ]
        );
        assert!(text.find("DISPLAY_MODE=").unwrap() > text.find("ARGS=").unwrap());
        let v = ConfValues::parse(&text);
        assert_eq!(v.values["DISPLAY_MODE"], "wayland");
        assert_eq!(v.values["DISPLAY_HDR"], "force");
        assert_eq!(v.values["DISPLAY_MONITOR"], "DP-1");
        c.display = Some(DisplayMode::X11);
        c.hdr = None;
        assert!(c.render().contains("\nDISPLAY_MODE='x11'\n"));
    }

    #[test]
    fn launch_conf_is_assignments_only_and_sources_back() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("launch.conf");
        std::fs::write(&path, conf().render()).unwrap();
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(
                "set -u; source \"$1\"; printf '%s|' \"$EXE\" \"${ARGS[@]}\" \"${ENV[@]}\" \
                 \"$DLL_OVERRIDES\" \"$PROTONFIXES\"",
            )
            .arg("bash")
            .arg(&path)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "games/1207659013/prefix/drive_c/GOG/Treasure Adventure Game/Game.exe|-windowed|it's|\
             -skipStartScreen|two words|$HOME \"q\" `x` \\|ODD=a 'b' $X|PROTON_USE_D7VK=1|\
             ddraw=n,b|1|"
        );
    }

    #[test]
    fn launch_conf_reads_back() {
        let c = conf();
        let v = ConfValues::parse(&c.render());
        assert_eq!(v.get("EXE"), c.exe);
        assert_eq!(v.get("UMU_PYTHON"), c.umu_python);
        assert_eq!(v.get("DLL_OVERRIDES"), "ddraw=n,b");
        assert_eq!(v.get("UMU_FOLDERS"), "");
        assert_eq!(v.arrays["ARGS"], c.args);
        assert_eq!(v.arrays["ENV"], ["ODD=a 'b' $X", "PROTON_USE_D7VK=1"]);
        assert!(!v.values.contains_key("Generated"));
        let empty = ConfValues::parse("ARGS=()\nUMU_PYTHON=''\n");
        assert!(empty.arrays["ARGS"].is_empty());
        assert_eq!(empty.get("UMU_PYTHON"), "");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn live_launcher_needs_a_matching_start_time() {
        let tmp = tempfile::tempdir().unwrap();
        let live = tmp.path().join(".launchers");
        std::fs::create_dir_all(&live).unwrap();
        let me = std::process::id();
        let stat = std::fs::read_to_string(format!("/proc/{me}/stat")).unwrap();
        let start = stat
            .rsplit_once(") ")
            .unwrap()
            .1
            .split_whitespace()
            .nth(19)
            .unwrap()
            .to_string();
        std::fs::write(live.join(me.to_string()), "1").unwrap();
        assert_eq!(
            live_launcher(tmp.path()),
            None,
            "another start time: a reused pid"
        );
        std::fs::write(live.join(me.to_string()), &start).unwrap();
        assert_eq!(live_launcher(tmp.path()), Some(me));
    }

    #[test]
    fn umu_folders_only_when_set() {
        let mut c = conf();
        assert!(!c.render().contains("UMU_FOLDERS"));
        c.umu_folders = Some("/home/me/.local/share".into());
        assert!(c.render().contains("UMU_FOLDERS='/home/me/.local/share'\n"));
    }

    #[test]
    fn state_round_trips_and_tolerates_missing_lists() {
        let s = LinuxState {
            schema: STATE_SCHEMA,
            gogid: "1".into(),
            title: Some("T".into()),
            runtime: "ge-proton/GE-Proton11-12".into(),
            renderer: "dxvk".into(),
            umu_id: "umu-1".into(),
            protonfix: "none".into(),
            verbs: vec!["d3dx9".into()],
            registry: vec![],
            files: vec!["ab Apps/x.ini".into()],
            stamp: "s".into(),
            gogo_version: "0".into(),
        };
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("state.toml");
        std::fs::write(&p, s.to_toml()).unwrap();
        assert_eq!(LinuxState::load(&p).unwrap(), Some(s));
        std::fs::write(
            &p,
            "schema = 1\ngogid = \"1\"\nruntime = \"r\"\nrenderer = \"wined3d\"\n\
             umu_id = \"umu-1\"\nprotonfix = \"none\"\nstamp = \"s\"\ngogo_version = \"0\"\n",
        )
        .unwrap();
        assert!(LinuxState::load(&p).unwrap().unwrap().verbs.is_empty());
        assert_eq!(LinuxState::load(&tmp.path().join("none")).unwrap(), None);
    }

    #[test]
    fn private_tmp_paths_are_refused() {
        for p in ["/tmp", "/tmp/gogo/games/1/prefix"] {
            let e = refuse_private_tmp("the prefix", Path::new(p)).unwrap_err();
            assert!(e.to_string().contains("private /tmp"), "{e}");
        }
        for p in ["/tmpfoo/x", "/home/me/.local/share/gogo", "/var/tmp/x"] {
            refuse_private_tmp("the prefix", Path::new(p)).unwrap();
        }
    }

    #[test]
    fn registry_lines_name_key_value_type_and_data() {
        let v = compat::RegistryValue {
            key: r"HKCU\Software\Wine\X11 Driver".into(),
            name: Some("Decorated".into()),
            kind: None,
            data: "N".into(),
        };
        assert_eq!(
            registry_line(&v),
            r"HKCU\Software\Wine\X11 Driver\Decorated=REG_SZ:N"
        );
    }
}
