//! The v2 launcher contract (design 16 §1, §2, §8): `gogo-launch.toml` (the
//! source of truth), its shell mirror `launch.conf`, the config stamp, the
//! per-game state dir `<state_dir>/games/<gogid>/`, runtime references into
//! the store, and the bash launcher itself. `gogo mac-app setup` is
//! `macapp/setup.rs`.
//!
//! The `.app` holds only the launcher, the TOML and its mirror, `Info.plist`
//! and licence notices. It is signed once and never written again; the
//! prefix, the game files and the effective config live in the state dir.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::compat::{self, CompatEntry, Layer};
use super::error::MacAppError;
use super::runtime::{self, Runtime, RuntimeMode};

/// `gogo-launch.toml` schema version this gogo reads and writes.
pub const SCHEMA: u32 = 1;

/// `Contents/Resources/gogo-launch.toml` (design 16 §1.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchToml {
    pub schema: u32,
    pub gogid: String,
    pub title: String,
    /// `<id>/<ver>` in the store, or `path:<dir>` for a runtime outside it.
    pub runtime: String,
    pub renderer: String,
    /// Windows path of the entry point (`C:\GOG\<name>\game.exe`).
    pub exe: String,
    /// Windows path the game starts in.
    pub workdir: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub dll_overrides: BTreeMap<String, String>,
    /// winetricks verbs; applied by setup, never by the launcher.
    #[serde(default)]
    pub verbs: Vec<String>,
    pub gogo: GogoRef,
}

/// `[gogo]`: where the launcher finds gogo and its state dir.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GogoRef {
    /// The path gogo was invoked as (not canonicalized, decision 2).
    pub path: PathBuf,
    pub state: PathBuf,
}

impl LaunchToml {
    /// Parse, refusing a newer `schema` outright rather than half-reading it.
    pub fn parse(raw: &str, path: &Path) -> Result<LaunchToml, MacAppError> {
        let err = |reason: String| MacAppError::Compat {
            path: path.to_path_buf(),
            reason,
        };
        let table: toml::Table = toml::from_str(raw).map_err(|e| err(e.to_string()))?;
        let schema = table.get("schema").and_then(toml::Value::as_integer);
        match schema {
            Some(s) if s > i64::from(SCHEMA) => {
                return Err(err(format!(
                    "schema {s} is newer than this gogo understands ({SCHEMA}): upgrade gogo"
                )));
            }
            Some(_) => {}
            None => return Err(err("missing `schema`".into())),
        }
        toml::from_str(raw).map_err(|e| err(e.to_string()))
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("LaunchToml always serializes")
    }

    /// The game dir's Windows path: the first two components of `exe`
    /// (`C:\GOG\<name>`), which is where a relative L3/L4 `exe` resolves.
    pub fn game_dir(&self) -> String {
        let parts: Vec<&str> = self.exe.split('\\').collect();
        parts[..parts.len().min(3)].join("\\")
    }

    /// `<name>` of `C:\GOG\<name>`, the tree name the prefix symlink uses.
    pub fn tree_name(&self) -> Option<String> {
        let parts: Vec<&str> = self.exe.split('\\').collect();
        (parts.len() >= 4 && parts[1].eq_ignore_ascii_case("GOG")).then(|| parts[2].to_string())
    }

    /// This entry as a compat layer (what the bundle baked: L0–L3 + flags).
    fn as_compat(&self) -> CompatEntry {
        CompatEntry {
            exe: Some(self.exe.clone()),
            renderer: Some(self.renderer.clone()),
            runtime: Some(self.runtime.clone()),
            verbs: Some(self.verbs.clone()),
            dll_overrides: self.dll_overrides.clone(),
            env: self.env.clone(),
            args: Some(self.args.clone()),
            ..Default::default()
        }
    }

    /// The effective config: this bundle's TOML with the L4 override applied
    /// field by field (design 16 §1.2, decision 5). Fields L4 can't express
    /// in launch terms (status, tested, …) are ignored here.
    pub fn with_override(&self, l4: &CompatEntry) -> LaunchToml {
        let eff = compat::merge(&[(Layer::Db, &self.as_compat()), (Layer::User, l4)]);
        let mut out = self.clone();
        if let Some(r) = eff.runtime {
            out.runtime = r.value;
        }
        if let Some(r) = eff.renderer {
            out.renderer = r.value;
        }
        if let Some(e) = eff.exe {
            let exe = e.value;
            out.exe = if is_windows_abs(&exe) {
                exe
            } else {
                format!("{}\\{}", self.game_dir(), exe.replace('/', "\\"))
            };
            if out.exe != self.exe {
                out.workdir = windows_parent(&out.exe);
            }
        }
        if let Some(a) = eff.args {
            out.args = a.value;
        }
        if let Some(v) = eff.verbs {
            out.verbs = v.value;
        }
        out.env = eff.env.into_iter().map(|(k, s)| (k, s.value)).collect();
        out.dll_overrides = eff
            .dll_overrides
            .into_iter()
            .map(|(k, s)| (k, s.value))
            .collect();
        out
    }
}

fn is_windows_abs(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\'
}

fn windows_parent(p: &str) -> String {
    p.rsplit_once('\\')
        .map(|(head, _)| head.to_string())
        .unwrap_or_else(|| p.to_string())
}

/// `<prefix>/drive_c/a/b.exe` → `C:\a\b.exe`. The path must sit under the
/// prefix's `drive_c` (both build lanes put the game there).
pub fn unix_to_windows(prefix: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(prefix.join("drive_c")).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(format!("C:\\{}", parts.join("\\")))
}

/// The unix path of a `C:\…` path inside `prefix` (the inverse of
/// [`unix_to_windows`]); `None` for other drives.
pub fn windows_to_unix(prefix: &Path, path: &str) -> Option<PathBuf> {
    let rest = path
        .strip_prefix("C:\\")
        .or_else(|| path.strip_prefix("c:\\"))?;
    Some(
        rest.split('\\')
            .filter(|p| !p.is_empty())
            .fold(prefix.join("drive_c"), |acc, p| acc.join(p)),
    )
}

/// POSIX single-quote (the same quoting the fat bundle's `launch.conf` uses).
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn sh_array(items: impl IntoIterator<Item = String>) -> String {
    let quoted: Vec<String> = items.into_iter().map(|s| sh_quote(&s)).collect();
    format!("({})", quoted.join(" "))
}

/// The shell mirror of a [`LaunchToml`] (design 16 §1.2): assignments only,
/// every value single-quoted, arrays for lists. `extra` appends resolved
/// keys (the state copy's `RUNTIME_DIR`, `PREFIX`, `STAMP`, …).
pub fn to_launch_conf(t: &LaunchToml, extra: &[(&str, String)]) -> String {
    let mut out = String::from("# Generated by gogo from gogo-launch.toml. Do not edit.\n");
    let mut kv = |k: &str, v: String| out.push_str(&format!("{k}={v}\n"));
    kv("SCHEMA", t.schema.to_string());
    kv("GOGID", sh_quote(&t.gogid));
    kv("TITLE", sh_quote(&t.title));
    kv("RUNTIME", sh_quote(&t.runtime));
    kv("RENDERER", sh_quote(&t.renderer));
    kv("EXE", sh_quote(&t.exe));
    kv("WORKDIR", sh_quote(&t.workdir));
    kv("ARGS", sh_array(t.args.iter().cloned()));
    kv(
        "ENV",
        sh_array(t.env.iter().map(|(k, v)| format!("{k}={v}"))),
    );
    let dll: Vec<String> = t
        .dll_overrides
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    kv("DLL_OVERRIDES", sh_quote(&dll.join(";")));
    kv("VERBS", sh_array(t.verbs.iter().cloned()));
    kv("GOGO_PATH", sh_quote(&t.gogo.path.to_string_lossy()));
    kv("GOGO_STATE", sh_quote(&t.gogo.state.to_string_lossy()));
    for (k, v) in extra {
        kv(k, sh_quote(v));
    }
    out
}

/// The config stamp (design 16 §2.1): sha256 over the bundle TOML's bytes
/// followed by the L4 file's bytes. No L4 file contributes nothing (the
/// "empty marker" is zero bytes), so the launcher reproduces it with
/// `cat toml [l4] | shasum -a 256`.
pub fn stamp(bundle_toml: &[u8], l4: Option<&[u8]>) -> String {
    let mut h = Sha256::new();
    h.update(bundle_toml);
    if let Some(l4) = l4 {
        h.update(l4);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

// — the state dir —

/// `<state_dir>/games/`.
pub fn games_dir() -> PathBuf {
    crate::state::state_dir().join("games")
}

/// One game's state (design 16 §0, decision 6).
#[derive(Debug, Clone, PartialEq)]
pub struct GameDirs {
    pub root: PathBuf,
    pub prefix: PathBuf,
    /// Depot lane only: the APFS clone of the depot tree.
    pub game: PathBuf,
    pub state_toml: PathBuf,
    pub launch_conf: PathBuf,
}

impl GameDirs {
    pub fn at(root: PathBuf) -> GameDirs {
        GameDirs {
            prefix: root.join("prefix"),
            game: root.join("game"),
            state_toml: root.join("state.toml"),
            launch_conf: root.join("launch.conf"),
            root,
        }
    }

    pub fn for_game(gogid: &str) -> Result<GameDirs, MacAppError> {
        compat::validate_gogid(gogid)?;
        Ok(GameDirs::at(games_dir().join(gogid)))
    }
}

/// `games/<gogid>/state.toml`: what gogo last applied, and the store's
/// reference list for GC (design 16 decision 6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameState {
    pub schema: u32,
    pub gogid: String,
    /// The `.app` this state belongs to (updated when the launcher reports a
    /// moved bundle).
    pub bundle: PathBuf,
    pub runtime: String,
    pub renderer: String,
    /// The verbs the last setup applied (#72): winetricks runs again only
    /// when these change or the prefix is new.
    #[serde(default)]
    pub verbs: Vec<String>,
    pub stamp: String,
    pub gogo_version: String,
}

impl GameState {
    pub fn load(path: &Path) -> Result<Option<GameState>, MacAppError> {
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
}

/// Absolute, `.`-free, symlinks kept (never canonicalized: a profile
/// symlink must survive, decision 2).
pub fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .components()
        .collect()
}

pub(super) fn write_atomic(path: &Path, contents: &str) -> Result<(), MacAppError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// — runtime references (design 16 decision 6; the GC side is
// `store::references`) —

/// How the bundle names its runtime: `<id>/<ver>` when it lives in the store,
/// else `path:<root>` (brew, a GPTK mount, `--runtime path`), which can't be
/// swapped or GC-tracked but still works.
pub fn runtime_ref(rt: &Runtime) -> String {
    match rt.root.strip_prefix(super::store::runtimes_dir()) {
        Ok(rel) if rel.components().count() == 2 => rel.to_string_lossy().into_owned(),
        _ => format!("path:{}", rt.root.display()),
    }
}

/// Resolve a runtime reference to a runnable runtime (probed).
pub fn resolve_ref(r: &str) -> Result<Runtime, MacAppError> {
    if let Some(dir) = r.strip_prefix("path:") {
        return runtime::resolve(&runtime::RuntimeOptions {
            mode: RuntimeMode::Path,
            runtime_path: Some(PathBuf::from(dir)),
            version: None,
        });
    }
    let (id, ver) = match r.split_once('/') {
        Some((id, ver)) => (id, Some(ver)),
        None => (r, None),
    };
    let (m, _) = super::store::installed(id, ver).ok_or_else(|| MacAppError::RuntimeNotFound {
        tried: format!("runtime `{r}` is not in the store (`gogo runtimes install {id}`)"),
    })?;
    runtime::from_store(&m)
}

/// `wineserver` next to the runtime's wine binary.
pub fn wineserver_of(rt: &Runtime) -> PathBuf {
    rt.wine.with_file_name("wineserver")
}

/// The engine root: the directory holding `bin/wine`.
pub fn engine_root(rt: &Runtime) -> PathBuf {
    rt.wine
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| rt.root.clone())
}

/// The dylib dir the launcher puts on `DYLD_FALLBACK_LIBRARY_PATH`: the store
/// entry's `Frameworks/` or a wrapper's `Contents/Frameworks`.
pub fn frameworks_of(rt: &Runtime) -> Option<PathBuf> {
    runtime::frameworks_for(&rt.wine)
}

/// A path for the state `launch.conf`: relative to the state root when under
/// it (the launcher resolves it against the state dir it finds, so a state
/// dir copied to another account still works), absolute otherwise.
pub(super) fn state_rel(state_root: &Path, p: &Path) -> String {
    p.strip_prefix(state_root)
        .map(|r| r.to_string_lossy().into_owned())
        .unwrap_or_else(|_| p.to_string_lossy().into_owned())
}

// — the bundle side —

/// Write `gogo-launch.toml` and its mirror into `Contents/Resources`.
pub fn write_bundle_config(resources: &Path, t: &LaunchToml) -> Result<(), MacAppError> {
    std::fs::write(resources.join("gogo-launch.toml"), t.to_toml())?;
    std::fs::write(resources.join("launch.conf"), to_launch_conf(t, &[]))?;
    Ok(())
}

/// The v2 launcher (design 16 §8), plain bash 3.2 (what macOS ships) with no
/// dependencies beyond `/usr/bin/shasum`, `osascript` and, for teardown,
/// `stat`/`lsof`/`ps`.
pub const LAUNCHER: &str = r#"#!/bin/bash
# Generated by `gogo mac-app` (design 16 §8). An immutable launcher: it only
# reads files gogo wrote, never writes inside this bundle, and runs Wine from
# gogo's runtime store against this game's prefix in the state dir.
set -u
# The environment as started, for "Try next renderer"'s relaunch (#78):
# the exports below must not stack up across relaunches.
ENV0="$(export -p)"
CONTENTS="$(cd "$(dirname "$0")/.." && pwd)"
RES="$CONTENTS/Resources"
# shellcheck source=/dev/null
source "$RES/launch.conf"
STAMP=""
STATE_ROOT="$GOGO_STATE"
[ -d "$STATE_ROOT" ] || STATE_ROOT="$HOME/Library/Application Support/gogo"
GAME_STATE="$STATE_ROOT/games/$GOGID"
LOG_DIR="${GOGO_LOG_DIR:-$HOME/Library/Logs/gogo/$GOGID}"
mkdir -p "$LOG_DIR"
LOG="$LOG_DIR/launch.log"

log() { printf '%s [%s] %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$$" "$*" >>"$LOG"; }
abs() { case "$1" in /*) printf '%s' "$1" ;; *) printf '%s/%s' "$STATE_ROOT" "$1" ;; esac; }
dialog() {
    log "dialog: $1"
    [ -n "${GOGO_LAUNCHER_NO_DIALOG:-}" ] && return 0
    /usr/bin/osascript -e 'on run argv' \
        -e 'display alert (item 1 of argv) message (item 2 of argv) as critical' \
        -e 'end run' "$TITLE" "$1" >/dev/null 2>&1 || true
}
load_state() {
    # shellcheck source=/dev/null
    if [ -f "$GAME_STATE/launch.conf" ]; then source "$GAME_STATE/launch.conf"; fi
}
# Design 16 §2.1: sha256 over the bundle TOML, then the L4 file if any.
current_stamp() {
    local l4="$STATE_ROOT/compat/$GOGID.toml" sum
    sum="$( { cat "$RES/gogo-launch.toml"; if [ -f "$l4" ]; then cat "$l4"; fi; } | /usr/bin/shasum -a 256)"
    printf '%s' "${sum%% *}"
}
find_gogo() {
    if [ -n "${GOGO_PATH:-}" ] && [ -x "$GOGO_PATH" ]; then printf '%s' "$GOGO_PATH"; return; fi
    command -v gogo 2>/dev/null || true
}

log "launch $TITLE ($GOGID) from $CONTENTS"
load_state
want="$(current_stamp)"
# Why this run isn't on the config asked for (a retarget to a runtime that
# isn't in the store, #69): for the run log's header.
SETUP_ERR=""
if [ ! -d "$GAME_STATE/prefix" ] || [ "$want" != "$STAMP" ]; then
    gogo="$(find_gogo)"
    if [ -n "$gogo" ]; then
        log "prefix missing or config stale (${STAMP:-none} -> $want): $gogo mac-app setup $GOGID"
        setup_out="$("$gogo" mac-app setup "$GOGID" --bundle "$(dirname "$CONTENTS")" 2>&1)"
        setup_rc=$?
        [ -n "$setup_out" ] && printf '%s\n' "$setup_out" >>"$LOG"
        if [ "$setup_rc" -eq 0 ]; then
            load_state
        else
            SETUP_ERR="$(printf '%s' "$setup_out" | grep -v '^warning:' | tail -n 1 | cut -c1-300)"
            SETUP_ERR="setup failed (exit $setup_rc): ${SETUP_ERR:-no output}; running the last applied config"
            log "$SETUP_ERR"
        fi
    else
        log "prefix missing or config stale, and no gogo found (GOGO_PATH=$GOGO_PATH)"
    fi
fi
# Design 16 Open 1: with no prefix, Wine would create a default one (home
# symlinks, z:), so stop and say why instead.
if [ ! -d "$GAME_STATE/prefix" ] || [ ! -f "$GAME_STATE/launch.conf" ]; then
    log "no prefix at $GAME_STATE/prefix: not launching"
    dialog "This game's Wine prefix is missing ($GAME_STATE/prefix) and gogo could not set it up. Install gogo and run: gogo mac-app setup $GOGID"
    exit 1
fi

export WINEPREFIX
WINEPREFIX="$(abs "$PREFIX")"
WINE_BIN="$(abs "$WINE")"
WINESERVER_BIN="$(abs "$WINESERVER")"
if [ -n "$FRAMEWORKS" ]; then
    export DYLD_FALLBACK_LIBRARY_PATH
    DYLD_FALLBACK_LIBRARY_PATH="$(abs "$FRAMEWORKS")${DYLD_FALLBACK_LIBRARY_PATH:+:$DYLD_FALLBACK_LIBRARY_PATH}"
fi
if [ -n "$DLL_OVERRIDES" ]; then
    export WINEDLLOVERRIDES="$DLL_OVERRIDES${WINEDLLOVERRIDES:+;$WINEDLLOVERRIDES}"
fi
for pair in ${ENV[@]+"${ENV[@]}"}; do export "${pair?}"; done
# C:\a\b → $WINEPREFIX/drive_c/a/b (old games resolve data files from here).
wd="${WORKDIR#?:\\}"
cd "$WINEPREFIX/drive_c/${wd//\\//}" || { log "workdir $WORKDIR missing"; dialog "Game folder $WORKDIR is missing."; exit 1; }

# Teardown (#42): this prefix's wineserver only. `wineserver -k` ends its
# clients; a server that doesn't answer is SIGKILLed (found by its cwd,
# /tmp/.wine-<uid>/server-<dev>-<ino> of this prefix), and whatever is left
# of the game's own process group is SIGKILLed too.
WPID=""
TORN=""
prefix_server_pid() {
    [ -x /usr/sbin/lsof ] && [ "$(uname)" = Darwin ] || return 0
    local di dir
    di="$(/usr/bin/stat -f '%d %i' "$WINEPREFIX" 2>/dev/null)" || return 0
    # shellcheck disable=SC2086
    dir="$(printf '/.wine-%s/server-%x-%x' "$(id -u)" $di)"
    /usr/sbin/lsof -a -d cwd -c wineserver -Fpn 2>/dev/null | awk -v d="$dir" '
        /^p/ { pid = substr($0, 2) }
        /^n/ { n = substr($0, 2); if (substr(n, length(n) - length(d) + 1) == d) print pid }'
}
teardown() {
    [ -n "$TORN" ] && return
    TORN=1
    if [ -n "$WPID" ]; then kill -TERM -- "-$WPID" 2>/dev/null; fi
    local k left other
    other="$(other_launcher)"
    if [ -n "$other" ]; then
        # A later launch of this game shares the prefix (#103): its game
        # needs the wineserver, so only this run's process group goes.
        log "launcher $other of this game is running: leaving the prefix's wineserver"
        sleep 1
    else
        "$WINESERVER_BIN" -k >/dev/null 2>&1 &
        k=$!
        for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
            kill -0 "$k" 2>/dev/null || break
            sleep 0.25
        done
        kill -KILL "$k" 2>/dev/null
        left="$(prefix_server_pid)"
        if [ -n "$left" ]; then
            log "reaped: wineserver $left did not exit after -k, SIGKILL"
            # shellcheck disable=SC2086
            kill -KILL $left 2>/dev/null
        fi
    fi
    if [ -n "$WPID" ]; then
        left="$(ps -A -o pid=,pgid=,command= | awk -v g="$WPID" '$2 == g { print }')"
        if [ -n "$left" ]; then
            log "reaped (SIGKILL): $(printf '%s' "$left" | tr '\n' ';')"
            kill -KILL -- "-$WPID" 2>/dev/null
        fi
    fi
    log "teardown done"
}

# #103: a launcher killed untrapped (SIGKILL, Force Quit) can't tear down,
# so a watchdog in its own process group does it: it polls the launcher
# every 2s and, once it's gone, runs the same teardown, ends the tees and
# writes the run log's footer. The launcher stops it before any exit of its
# own (and before #78's relaunch, which keeps the pid). Each running
# launcher leaves <state>/games/<gogid>/.launchers/<pid> (its start time),
# so a teardown spares the wineserver of a later launch of the same game.
WATCHDOG=""
ME_START=""
LIVE_DIR="$GAME_STATE/.launchers"
proc_start() { ps -o lstart= -p "$1" 2>/dev/null; }
# $1 is alive and is the process that started at $2: not a zombie, not a
# reused pid.
alive_as() {
    case "$(ps -o stat= -p "$1" 2>/dev/null | tr -d ' ')" in "" | Z*) return 1 ;; esac
    [ "$(proc_start "$1")" = "$2" ]
}
# The pid of another running launcher of this game, if any.
other_launcher() {
    local f pid start
    for f in "$LIVE_DIR"/*; do
        [ -f "$f" ] || continue
        pid="${f##*/}"
        start="$(cat "$f" 2>/dev/null)"
        [ "$pid" = "$$" ] && [ "$start" = "$ME_START" ] && continue
        if alive_as "$pid" "$start"; then printf '%s' "$pid"; return; fi
        rm -f "$f"
    done
}
register_launch() {
    ME_START="$(proc_start $$)"
    mkdir -p "$LIVE_DIR" && printf '%s' "$ME_START" >"$LIVE_DIR/$$"
}
unregister_launch() {
    if [ "$(cat "$LIVE_DIR/$$" 2>/dev/null)" = "$ME_START" ]; then rm -f "$LIVE_DIR/$$"; fi
}
start_watchdog() {
    set -m
    (
        trap '' INT HUP
        while alive_as "$$" "$ME_START"; do sleep 2; done
        log "watchdog: launcher $$ is gone (SIGKILL or Force Quit): tearing down"
        teardown
        # The tees see EOF once Wine's last writer is gone; they ignore
        # TERM, so stragglers get KILL.
        for _ in 1 2 3 4 5 6 7 8; do
            # shellcheck disable=SC2086
            kill -0 $TEE_PIDS 2>/dev/null || break
            sleep 0.25
        done
        # shellcheck disable=SC2086
        if [ -n "$TEE_PIDS" ]; then kill -KILL $TEE_PIDS 2>/dev/null; fi
        unregister_launch
        if [ -n "$RUN_LOG" ]; then run_footer "$RUN_LOG" 137 "launcher killed" $((SECONDS - RUN_T0)); fi
        log "watchdog done"
    ) </dev/null >/dev/null 2>&1 3>&- 4>&- &
    WATCHDOG=$!
    set +m
}
stop_watchdog() {
    [ -n "$WATCHDOG" ] || return 0
    kill -- "-$WATCHDOG" 2>/dev/null
    wait "$WATCHDOG" 2>/dev/null
    WATCHDOG=""
}
on_signal() {
    log "got SIG$1"
    stop_watchdog
    teardown
    unregister_launch
    end_run_log "$2" "SIG$1"
    exit "$2"
}
trap 'on_signal INT 130' INT
trap 'on_signal TERM 143' TERM
trap 'on_signal HUP 129' HUP

# #77: one log per run, <LOG_DIR>/<YYYYmmdd-HHMMSS>-<pid>.log (the last
# KEEP_RUNS kept, latest.log -> the newest): a header naming the effective
# config, Wine's stdout and stderr (teed, so a terminal still sees them),
# then exit code and duration. launch.log stays the launcher's own journal.
KEEP_RUNS=10
RUN_LOG=""
LAST_RUN_LOG=""
RUN_T0=0
RUN_SECS=0
TEE_PIDS=""
macos_version() {
    if [ -x /usr/bin/sw_vers ]; then
        printf '%s (%s) %s' "$(/usr/bin/sw_vers -productVersion)" "$(/usr/bin/sw_vers -buildVersion)" "$(uname -m)"
    else
        uname -srm
    fi
}
prune_runs() {
    local runs=("$LOG_DIR"/[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]-*.log) i=0
    [ -e "${runs[0]}" ] || return 0
    while [ $((${#runs[@]} - i)) -gt "$KEEP_RUNS" ]; do
        rm -f "${runs[$i]}"
        i=$((i + 1))
    done
}
start_run_log() {
    RUN_LOG="$LOG_DIR/$(date '+%Y%m%d-%H%M%S')-$$.log"
    RUN_T0=$SECONDS
    {
        printf '# gogo run log: %s (%s)\n' "$TITLE" "$GOGID"
        printf '# started: %s\n' "$(date '+%Y-%m-%d %H:%M:%S %z')"
        if [ -n "$SETUP_ERR" ]; then printf '# setup: %s\n' "$SETUP_ERR"; fi
        printf '# runtime: %s\n' "$RUNTIME"
        printf '# runtime_dir: %s\n' "${RUNTIME_DIR:-}"
        printf '# renderer: %s\n' "$RENDERER"
        printf '# verbs: %s\n' "${VERBS[*]+${VERBS[*]}}"
        printf '# stamp: %s\n' "$STAMP"
        printf '# macos: %s\n' "$(macos_version)"
        printf '# exe: %s\n' "$EXE"
        printf '# args: %s\n' "${ARGS[*]+${ARGS[*]}}${*:+ $*}"
        printf '# wine: %s\n' "$WINE_BIN"
        printf '# prefix: %s\n' "$WINEPREFIX"
        printf '# ---\n'
    } >"$RUN_LOG"
    ln -sfn "$(basename "$RUN_LOG")" "$LOG_DIR/latest.log"
    prune_runs
    # tee through FIFOs (not process substitution) so the launcher can wait
    # for them to drain before it writes the footer. They ignore the signals
    # the launcher traps, so they outlive a Ctrl-C long enough to flush.
    local fifo="$LOG_DIR/.run-$$"
    rm -f "$fifo.out" "$fifo.err"
    exec 3>&1 4>&2
    if mkfifo "$fifo.out" "$fifo.err" 2>/dev/null; then
        (trap '' INT TERM HUP; exec tee -a "$RUN_LOG") <"$fifo.out" &
        TEE_PIDS="$!"
        (trap '' INT TERM HUP; exec tee -a "$RUN_LOG" >&2) <"$fifo.err" &
        TEE_PIDS="$TEE_PIDS $!"
        exec >"$fifo.out" 2>"$fifo.err"
        rm -f "$fifo.out" "$fifo.err"
    else
        exec >>"$RUN_LOG" 2>&1
    fi
}
# Footer: exit code (and the signal, if one ended the run) and duration.
end_run_log() {
    [ -n "$RUN_LOG" ] || return 0
    local run="$RUN_LOG"
    RUN_LOG=""
    LAST_RUN_LOG="$run"
    RUN_SECS=$((SECONDS - RUN_T0))
    exec >&3 2>&4
    if [ -n "$TEE_PIDS" ]; then
        # A straggler still holding Wine's output open must not hold the
        # launcher: give the tees 2s to see EOF (KILL: they ignore TERM).
        # shellcheck disable=SC2086
        ( sleep 2; kill -KILL $TEE_PIDS ) >/dev/null 2>&1 &
        local dog=$!
        # shellcheck disable=SC2086
        wait $TEE_PIDS 2>/dev/null
        kill "$dog" 2>/dev/null
    fi
    run_footer "$run" "$1" "${2:-}" "$RUN_SECS"
}
# run_footer <run log> <exit code> <reason> <secs>
run_footer() {
    {
        printf '# ---\n'
        printf '# exit: %s%s\n' "$2" "${3:+ ($3)}"
        printf '# duration: %ss\n' "$4"
        printf '# ended: %s\n' "$(date '+%Y-%m-%d %H:%M:%S %z')"
    } >>"$1"
}

# #78: a run that ends within EARLY_EXIT_SECS or with a non-zero code
# explains itself: the last error lines of its run log and three buttons.
# GOGO_LAUNCHER_DIALOG=<cmd> replaces osascript (tests): it gets the title,
# the message and the button labels, and prints the chosen label.
EARLY_EXIT_SECS="${GOGO_EARLY_EXIT_SECS:-15}"
ask() {
    if [ -n "${GOGO_LAUNCHER_DIALOG:-}" ]; then "$GOGO_LAUNCHER_DIALOG" "$TITLE" "$@"; return; fi
    [ -n "${GOGO_LAUNCHER_NO_DIALOG:-}" ] && return 0
    /usr/bin/osascript -e 'on run argv' -e 'set b to items 3 thru -1 of argv' \
        -e 'set r to display dialog (item 2 of argv) with title (item 1 of argv) buttons b default button (item -1 of b) with icon caution giving up after 600' \
        -e 'if gave up of r then return ""' -e 'return button returned of r' -e 'end run' \
        "$TITLE" "$@" 2>/dev/null
}
# Wine's output in a run log: between the header's and the footer's `# ---`.
run_output() { awk '/^# ---$/ { n++; next } n == 1' "$1"; }
last_errors() {
    local errs
    errs="$(run_output "$1" | grep -E -i 'err:|error|fail|exception|cannot|not found' | tail -n 5)"
    [ -n "$errs" ] || errs="$(run_output "$1" | tail -n 5)"
    printf '%s' "$errs" | cut -c1-160
}
# Copy report: what a bug report needs, no home paths.
report() {
    printf 'gogo launch report: %s (%s)\n' "$TITLE" "$GOGID"
    grep -E '^# (setup|runtime|runtime_dir|renderer|verbs|stamp|macos|exe|args|exit|duration):' "$1"
    printf -- '--- last 40 log lines ---\n'
    run_output "$1" | tail -n 40
}
early_exit() {
    local log="$LAST_RUN_LOG" gogo next="" msg choice
    if [ -z "${GOGO_LAUNCHER_DIALOG:-}" ] && [ -n "${GOGO_LAUNCHER_NO_DIALOG:-}" ]; then
        log "early exit: code $rc after ${RUN_SECS}s on $RENDERER (dialogs off)"
        return 0
    fi
    gogo="$(find_gogo)"
    if [ -n "$gogo" ]; then
        next="$("$gogo" mac-app next-renderer "$GOGID" --dry-run 2>>"$LOG")" || next=""
    fi
    log "early exit: code $rc after ${RUN_SECS}s on $RENDERER; next renderer: ${next:-none}"
    msg="$TITLE quit after ${RUN_SECS}s (exit code $rc) on the $RENDERER renderer.

$(last_errors "$log")

"
    if [ -n "$next" ]; then
        choice="$(ask "${msg}Try next renderer switches to $next and starts the game again." \
            "Copy report" "Open log" "Try next renderer")"
    else
        choice="$(ask "${msg}There are no more renderers to try for this game." \
            "Copy report" "Open log" "Close")"
    fi
    log "dialog: ${choice:-dismissed}"
    case "$choice" in
        "Open log") open "$log" ;;
        "Copy report") report "$log" | pbcopy ;;
        "Try next renderer")
            if "$gogo" mac-app next-renderer "$GOGID" >>"$LOG" 2>&1; then
                log "relaunching on $next"
                # The stamp is stale now: the relaunch runs setup, which
                # applies the new renderer.
                exec /usr/bin/env -i /bin/bash -c 'eval "$1"; shift; exec "$@"' relaunch \
                    "$ENV0" "$CONTENTS/MacOS/$(basename "$0")" "$@"
            fi
            log "next-renderer failed: not relaunching"
            ;;
    esac
}

start_run_log "$@"
# The runtime the last setup applied must still be in the store (#69: a
# prune, or a retarget whose setup failed, can leave it missing).
if [ ! -x "$WINE_BIN" ]; then
    msg="runtime $RUNTIME is not in the runtime store ($WINE_BIN is missing)"
    printf 'gogo: %s; reinstall it with `gogo runtimes install %s` or pick another with `gogo mac-app --retarget %s --runtime …`\n' \
        "$msg" "${RUNTIME%%/*}" "$GOGID" >&2
    log "$msg: not launching"
    end_run_log 127 "runtime missing"
    dialog "This game's Wine runtime ($RUNTIME) is not installed. Run: gogo runtimes install ${RUNTIME%%/*}"
    exit 1
fi
log "exec $WINE_BIN $EXE (runtime $RUNTIME, prefix $WINEPREFIX, log $RUN_LOG)"
set -m
"$WINE_BIN" "$EXE" ${ARGS[@]+"${ARGS[@]}"} "$@" 3>&- 4>&- &
WPID=$!
set +m
register_launch
start_watchdog
wait "$WPID"
rc=$?
log "wine exited $rc; waiting for the prefix's wineserver"
# A launcher exe may have started the real game and exited: wait for the
# whole prefix to finish, not just the first process.
"$WINESERVER_BIN" -w >/dev/null 2>&1 &
wait $!
log "prefix idle"
stop_watchdog
unregister_launch
end_run_log "$rc"
if [ "$rc" -ne 0 ] || [ "$RUN_SECS" -lt "$EARLY_EXIT_SECS" ]; then early_exit "$@"; fi
exit "$rc"
"#;

/// `Contents/MacOS/<display>`.
pub fn write_launcher(macos: &Path, display: &str) -> Result<PathBuf, MacAppError> {
    let path = macos.join(display);
    let mut file = std::fs::File::create(&path)?;
    file.write_all(LAUNCHER.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(path)
}

#[cfg(test)]
mod launch_tests {
    use super::*;

    fn sample() -> LaunchToml {
        LaunchToml {
            schema: 1,
            gogid: "1146738698".into(),
            title: "Carmageddon TDR 2000".into(),
            runtime: "winecx/24.0.7_7".into(),
            renderer: "wined3d".into(),
            exe: r"C:\GOG\Carmageddon TDR 2000\TDR2000.exe".into(),
            workdir: r"C:\GOG\Carmageddon TDR 2000".into(),
            args: vec!["-f".into()],
            env: [("WINEMSYNC".to_string(), "1".to_string())].into(),
            dll_overrides: [("ddraw".to_string(), "n,b".to_string())].into(),
            verbs: vec![],
            gogo: GogoRef {
                path: "/Users/me/.nix-profile/bin/gogo".into(),
                state: "/Users/me/Library/Application Support/gogo".into(),
            },
        }
    }

    #[test]
    fn toml_round_trips_and_rejects_unknown_keys() {
        let t = sample();
        let raw = t.to_toml();
        assert_eq!(LaunchToml::parse(&raw, Path::new("x")).unwrap(), t);
        let extra = format!("bogus = 1\n{raw}");
        assert!(LaunchToml::parse(&extra, Path::new("x")).is_err());
    }

    #[test]
    fn newer_schema_is_refused() {
        let raw = sample().to_toml().replace("schema = 1", "schema = 2");
        let e = LaunchToml::parse(&raw, Path::new("x")).unwrap_err();
        assert!(e.to_string().contains("upgrade gogo"), "{e}");
    }

    /// Golden projection (design 16 §9).
    #[test]
    fn launch_conf_golden() {
        let mut t = sample();
        t.title = "It's a game".into();
        let conf = to_launch_conf(&t, &[("STAMP", "abc".into())]);
        assert_eq!(
            conf,
            "# Generated by gogo from gogo-launch.toml. Do not edit.\n\
             SCHEMA=1\n\
             GOGID='1146738698'\n\
             TITLE='It'\\''s a game'\n\
             RUNTIME='winecx/24.0.7_7'\n\
             RENDERER='wined3d'\n\
             EXE='C:\\GOG\\Carmageddon TDR 2000\\TDR2000.exe'\n\
             WORKDIR='C:\\GOG\\Carmageddon TDR 2000'\n\
             ARGS=('-f')\n\
             ENV=('WINEMSYNC=1')\n\
             DLL_OVERRIDES='ddraw=n,b'\n\
             VERBS=()\n\
             GOGO_PATH='/Users/me/.nix-profile/bin/gogo'\n\
             GOGO_STATE='/Users/me/Library/Application Support/gogo'\n\
             STAMP='abc'\n"
        );
    }

    /// The conf is plain bash: sourcing it yields the values back.
    #[test]
    fn launch_conf_sources_in_bash() {
        let tmp = tempfile::tempdir().unwrap();
        let mut t = sample();
        t.args = vec!["a b".into(), "it's".into()];
        let conf = tmp.path().join("launch.conf");
        std::fs::write(&conf, to_launch_conf(&t, &[])).unwrap();
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(r#"source "$1"; printf '%s|' "$TITLE" "${#ARGS[@]}" "${ARGS[1]}" "$EXE""#)
            .arg("x")
            .arg(&conf)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            r"Carmageddon TDR 2000|2|it's|C:\GOG\Carmageddon TDR 2000\TDR2000.exe|"
        );
    }

    #[test]
    fn stamp_matches_shasum() {
        let toml = b"schema = 1\n";
        let l4 = b"renderer = \"dxmt\"\n";
        let ours = stamp(toml, Some(l4));
        // Reference: the exact pipeline the launcher runs.
        let shasum = Path::new("/usr/bin/shasum");
        if shasum.exists() {
            let tmp = tempfile::tempdir().unwrap();
            std::fs::write(tmp.path().join("a"), toml).unwrap();
            std::fs::write(tmp.path().join("b"), l4).unwrap();
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg("cat a b | /usr/bin/shasum -a 256")
                .current_dir(tmp.path())
                .output()
                .unwrap();
            let s = String::from_utf8_lossy(&out.stdout);
            assert_eq!(s.split_whitespace().next().unwrap(), ours);
        }
        assert_ne!(ours, stamp(toml, None));
        assert_eq!(stamp(toml, None), stamp(toml, Some(b"")));
    }

    #[test]
    fn override_applies_field_by_field() {
        let t = sample();
        let l4 = compat::parse(
            "runtime = \"wine-staging/11.18\"\nexe = \"bin/Other.exe\"\nenv = { DXMT_HUD = \"1\" }\n",
        )
        .unwrap()
        .entry;
        let eff = t.with_override(&l4);
        assert_eq!(eff.runtime, "wine-staging/11.18");
        assert_eq!(eff.renderer, "wined3d");
        assert_eq!(eff.exe, r"C:\GOG\Carmageddon TDR 2000\bin\Other.exe");
        assert_eq!(eff.workdir, r"C:\GOG\Carmageddon TDR 2000\bin");
        assert_eq!(eff.env.len(), 2);
        assert_eq!(eff.dll_overrides["ddraw"], "n,b");
        assert_eq!(t.with_override(&CompatEntry::default()), t);
    }

    #[test]
    fn windows_to_unix_inverts() {
        let p = Path::new("/s/prefix");
        let u = windows_to_unix(p, r"C:\GOG\Neo Cab\Neo Cab.exe").unwrap();
        assert_eq!(u, Path::new("/s/prefix/drive_c/GOG/Neo Cab/Neo Cab.exe"));
        assert_eq!(
            unix_to_windows(p, &u).unwrap(),
            r"C:\GOG\Neo Cab\Neo Cab.exe"
        );
        assert!(windows_to_unix(p, r"D:\x.exe").is_none());
    }

    #[test]
    fn windows_paths() {
        let t = sample();
        assert_eq!(t.game_dir(), r"C:\GOG\Carmageddon TDR 2000");
        assert_eq!(t.tree_name().as_deref(), Some("Carmageddon TDR 2000"));
        let prefix = Path::new("/s/prefix");
        assert_eq!(
            unix_to_windows(prefix, Path::new("/s/prefix/drive_c/GOG/x/y.exe")).as_deref(),
            Some(r"C:\GOG\x\y.exe")
        );
        assert_eq!(unix_to_windows(prefix, Path::new("/elsewhere/y.exe")), None);
    }

    #[test]
    fn runtime_refs() {
        let _g = crate::test_util::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: env mutation under the shared env mutex.
        unsafe { std::env::set_var("GOGO_HOME", tmp.path()) };
        let mk = |root: PathBuf| Runtime {
            mode: RuntimeMode::Catalog("wine-staging".into()),
            kind: "catalog",
            wine: root.join("W.app/Contents/Resources/wine/bin/wine"),
            root,
            version: "wine-11.18".into(),
            min_macos: None,
        };
        let store = mk(tmp.path().join("runtimes/wine-staging/11.18"));
        assert_eq!(runtime_ref(&store), "wine-staging/11.18");
        assert_eq!(
            engine_root(&store),
            tmp.path()
                .join("runtimes/wine-staging/11.18/W.app/Contents/Resources/wine")
        );
        assert_eq!(
            wineserver_of(&store),
            store.wine.with_file_name("wineserver")
        );
        let ext = mk(PathBuf::from("/opt/wine"));
        assert_eq!(runtime_ref(&ext), "path:/opt/wine");
        assert!(matches!(
            resolve_ref("wine-staging/11.18"),
            Err(MacAppError::RuntimeNotFound { .. })
        ));
        unsafe { std::env::remove_var("GOGO_HOME") };
    }

    #[test]
    fn launcher_is_bash_32_clean() {
        // No bash-4 features: macOS ships bash 3.2.
        for bad in [
            "declare -A",
            "${!",
            ",,}",
            "^^}",
            "mapfile",
            "readarray",
            "|&",
        ] {
            assert!(!LAUNCHER.contains(bad), "{bad}");
        }
        let out = std::process::Command::new("bash")
            .args(["-n", "-c", LAUNCHER])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
