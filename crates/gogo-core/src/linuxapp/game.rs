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
use crate::macapp::error::MacAppError;
use crate::macapp::launch::{GameDirs, LaunchToml, sh_array, sh_quote, stamp};

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
            args: vec!["-windowed".into(), "it's".into()],
            env: [("PROTON_USE_D7VK".to_string(), "1".to_string())].into(),
            dll_overrides: [("ddraw".to_string(), "n,b".to_string())].into(),
            renderer: "d7vk".into(),
            protonfixes: true,
            umu_folders: None,
            gogo_path: "/home/me/.nix-profile/bin/gogo".into(),
            data_root: "/home/me/.local/share/gogo".into(),
        }
    }

    #[test]
    fn launch_conf_matches_the_golden_file() {
        let golden = include_str!("../../tests/fixtures/linuxapp/launch.conf.golden");
        assert_eq!(conf().render(), golden);
    }

    #[test]
    fn launch_conf_is_assignments_only_and_sources_back() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("launch.conf");
        std::fs::write(&path, conf().render()).unwrap();
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(
                "set -u; source \"$1\"; printf '%s|' \"$EXE\" \"${ARGS[1]}\" \"${ENV[0]}\" \
                 \"$DLL_OVERRIDES\" \"$PROTONFIXES\"",
            )
            .arg("bash")
            .arg(&path)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "games/1207659013/prefix/drive_c/GOG/Treasure Adventure Game/Game.exe|it's|\
             PROTON_USE_D7VK=1|ddraw=n,b|1|"
        );
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
