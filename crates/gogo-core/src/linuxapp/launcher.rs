//! The Linux launcher (design 18 §7): `games/<gogid>/launch.sh`, generated
//! from [`TEMPLATE`], and the menu entry that runs it,
//! [`game::desktop_path`]. Setup writes both (and `icon.png`) after
//! `launch.conf`, each only when its bytes changed, so a gogo whose
//! template changed ([`LAUNCHER_VERSION`], R30) or a data root that moved
//! reaches the game at its next setup.
//!
//! `launch.sh` sources `launch.conf`, runs `gogo linux-app setup` when the
//! stamp is stale, the prefix is gone, umu's Python is gone or the Proton
//! build failed its check (§2.2), then runs the game through umu-run and
//! stays its parent: run logs, teardown through `/proc/<pid>/environ`, the
//! #103 watchdog and the early-exit dialog (§7.3, §7.4) as on the Mac.

use std::path::{Path, PathBuf};

use super::game::{self, LAUNCH_SH};
use super::icon;
use crate::macapp::error::MacAppError;
use crate::macapp::launch::GameDirs;

/// Bumped with every change to [`TEMPLATE`] that matters to a game set up
/// before it. Setup rewrites `launch.sh` whenever its bytes differ, so this
/// is for people: the run log header and `launch.log` name it.
pub const LAUNCHER_VERSION: u32 = 1;

/// `games/<gogid>/icon.png`.
pub const ICON: &str = "icon.png";

/// What setup wrote for the launcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LauncherFiles {
    pub launch_sh: PathBuf,
    pub desktop: PathBuf,
    /// `None`: neither GOG's `.ico` nor the exe had an icon gogo can read.
    pub icon: Option<PathBuf>,
    /// `launch.sh` was there and is now another launcher version's.
    pub updated_from: Option<String>,
}

/// `launch.sh` for this gogo.
pub fn launch_sh() -> String {
    TEMPLATE.replace("@LAUNCHER_VERSION@", &LAUNCHER_VERSION.to_string())
}

/// The `LAUNCHER_VERSION=` a `launch.sh` carries.
pub fn version_of(script: &str) -> Option<&str> {
    script
        .lines()
        .find_map(|l| l.strip_prefix("LAUNCHER_VERSION="))
}

/// The menu entry (design 18 §7.1). Values are escaped as the Desktop Entry
/// spec's strings; `Exec`'s one argument is also quoted as its own rules
/// say (`"`, `` ` ``, `$`, `\` backslashed; `%` doubled).
pub fn desktop_entry(gogid: &str, title: &str, launch_sh: &Path, icon: Option<&Path>) -> String {
    let mut out = String::from("[Desktop Entry]\nType=Application\nVersion=1.5\n");
    out.push_str(&format!("Name={}\n", desktop_string(title)));
    out.push_str(&format!(
        "Exec={}\n",
        desktop_string(&exec_arg(&launch_sh.to_string_lossy()))
    ));
    if let Some(icon) = icon {
        out.push_str(&format!(
            "Icon={}\n",
            desktop_string(&icon.to_string_lossy())
        ));
    }
    out.push_str("Terminal=false\nCategories=Game;\n");
    out.push_str(&format!("X-Gogo-Gogid={gogid}\n"));
    out
}

/// A `string`/`localestring` value: `\` and control characters escaped, a
/// leading space kept as `\s`.
fn desktop_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.chars().enumerate() {
        match c {
            '\\' => out.push_str(r"\\"),
            '\n' => out.push_str(r"\n"),
            '\t' => out.push_str(r"\t"),
            '\r' => out.push_str(r"\r"),
            ' ' if i == 0 => out.push_str(r"\s"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// One `Exec` argument, always double-quoted.
fn exec_arg(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '%' => out.push_str("%%"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Write through a temp file renamed into place, unless `path` already holds `text`; `true` when written.
fn write_if_changed(path: &Path, text: &[u8]) -> Result<bool, MacAppError> {
    if std::fs::read(path).is_ok_and(|b| b == text) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

/// Write `launch.sh` (0755; a new file renamed over the old, so a launcher
/// running setup keeps reading the script it started with), `icon.png`
/// (once: from `game_files`' `goggame-<gogid>.ico`, else `exe`) and the
/// menu entry.
pub fn write(
    dirs: &GameDirs,
    gogid: &str,
    title: &str,
    game_files: Option<&Path>,
    exe: Option<&Path>,
) -> Result<LauncherFiles, MacAppError> {
    let launch_sh_path = dirs.root.join(LAUNCH_SH);
    let script = launch_sh();
    let before = std::fs::read_to_string(&launch_sh_path).ok();
    if write_if_changed(&launch_sh_path, script.as_bytes())? {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&launch_sh_path, std::fs::Permissions::from_mode(0o755))?;
    }
    let updated_from = before
        .as_deref()
        .filter(|b| *b != script)
        .map(|b| version_of(b).unwrap_or("none").to_string());

    let icon_path = dirs.root.join(ICON);
    if !icon_path.is_file()
        && let Some(png) = icon::find(game_files, gogid, exe)
    {
        write_if_changed(&icon_path, &png)?;
    }
    let icon = icon_path.is_file().then_some(icon_path);

    let desktop = game::desktop_path(gogid);
    let abs = crate::macapp::launch::absolute;
    let entry = desktop_entry(
        gogid,
        title,
        &abs(&launch_sh_path),
        icon.as_deref().map(abs).as_deref(),
    );
    write_if_changed(&desktop, entry.as_bytes())?;
    Ok(LauncherFiles {
        launch_sh: launch_sh_path,
        desktop,
        icon,
        updated_from,
    })
}

/// The launcher (design 18 §7.2). Bash, on what any Linux desktop has:
/// coreutils (`sha256sum`, `mkfifo`, `tee`), GNU `grep` (`-z`, for
/// `/proc/<pid>/environ`), `sed`, `awk`; zenity/kdialog/notify-send,
/// `xdg-open`, `wl-copy`/`xclip` when they're there.
pub const TEMPLATE: &str = r##"#!/usr/bin/env bash
# Generated by gogo (`gogo linux-app setup`, design 18 §7). Don't edit it:
# setup rewrites it whenever gogo's launcher changes. It sources launch.conf
# beside it, runs setup when the config is stale, checks the Proton build,
# and runs the game through umu-run against this game's prefix, staying its
# parent so it can tear the game down.
set -u
LAUNCHER_VERSION=@LAUNCHER_VERSION@
ARGV=("$@")
SELF="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"
HERE="${SELF%/*}"
GOGID="${HERE##*/}"
TITLE="$GOGID"
# The data root this launcher is in (<root>/games/<gogid>/launch.sh).
HERE_ROOT="${HERE%/games/*}"
XDG_ROOT="${XDG_DATA_HOME:-$HOME/.local/share}/gogo"
# gogo's root for the setup this runs: GOGO_HOME as given, else this one
# when it isn't gogo's default (a game built under GOGO_HOME).
if [ -n "${GOGO_HOME:-}" ]; then
    HOME_ROOT="$GOGO_HOME"
elif [ "$HERE_ROOT" != "$XDG_ROOT" ]; then
    HOME_ROOT="$HERE_ROOT"
else
    HOME_ROOT=""
fi
# Logs as gogo's state::logs_root() has them (GOGO_LOG_DIR: this game's dir).
if [ -n "${GOGO_LOG_DIR:-}" ]; then
    LOG_DIR="$GOGO_LOG_DIR"
elif [ -n "$HOME_ROOT" ]; then
    LOG_DIR="$HOME_ROOT/logs/$GOGID"
else
    LOG_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/gogo/logs/$GOGID"
fi
mkdir -p "$LOG_DIR"
LOG="$LOG_DIR/launch.log"
# Only umu-run gets the game's environment: a process here carrying
# WINEPREFIX would count as the game's at teardown.
unset WINEPREFIX

log() { printf '%s [%s] %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$$" "$*" >>"$LOG"; }
has() { command -v "$1" >/dev/null 2>&1; }
gui() { [ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]; }
# An error with one OK: GOGO_LAUNCHER_DIALOG (tests), zenity, kdialog, a
# notification, else launch.log only.
alert() {
    log "dialog: $1"
    if [ -n "${GOGO_LAUNCHER_DIALOG:-}" ]; then
        "$GOGO_LAUNCHER_DIALOG" "$TITLE" "$1" "OK" >/dev/null
        return 0
    fi
    [ -n "${GOGO_LAUNCHER_NO_DIALOG:-}" ] && return 0
    if gui && has zenity; then
        zenity --error --no-markup --title "$TITLE" --text "$1" --width 560 >/dev/null 2>&1
    elif gui && has kdialog; then
        kdialog --title "$TITLE" --error "$1" >/dev/null 2>&1
    elif has notify-send; then
        notify-send -a gogo "$TITLE" "$1" >/dev/null 2>&1
    fi
    return 0
}
abs() { case "$1" in /* | "") printf '%s' "$1" ;; *) printf '%s/%s' "$ROOT" "$1" ;; esac; }
load_conf() {
    UMU_ID="" STAMP="" UMU_RUN="" UMU_PYTHON="" PROTON_DIR="" PROTON_VERSION=""
    PROTON_SHA="" PREFIX="" WORKDIR="" EXE="" DLL_OVERRIDES=""
    RENDERER="" PROTONFIXES="1" UMU_FOLDERS="" GOGO_PATH="" DATA_ROOT=""
    ARGS=() ENV=()
    # shellcheck source=/dev/null
    source "$HERE/launch.conf"
    # O7: relative paths are under the data root launch.conf names, else the
    # one this launcher is in.
    if [ -d "$DATA_ROOT" ]; then ROOT="$DATA_ROOT"; else ROOT="$HERE_ROOT"; fi
    PREFIX_ABS="$(abs "$PREFIX")"
    PROTON_ABS="$(abs "$PROTON_DIR")"
    UMU_RUN_ABS="$(abs "$UMU_RUN")"
    WORKDIR_ABS="$(abs "$WORKDIR")"
    EXE_ABS="$(abs "$EXE")"
}
# Design 16 §2.1: sha256 over gogo-launch.toml, then the L4 file if any.
current_stamp() {
    local l4="$ROOT/compat/$GOGID.toml" sum
    sum="$( { cat "$HERE/gogo-launch.toml"; if [ -f "$l4" ]; then cat "$l4"; fi; } 2>/dev/null | sha256sum)"
    printf '%s' "${sum%% *}"
}
find_gogo() {
    if [ -n "${GOGO_PATH:-}" ] && [ -x "$GOGO_PATH" ]; then printf '%s' "$GOGO_PATH"; return; fi
    command -v gogo 2>/dev/null || true
}
# gogo, with GOGO_HOME when this game's data root isn't gogo's default.
run_gogo() {
    if [ -n "$HOME_ROOT" ] && [ -z "${GOGO_HOME:-}" ]; then
        GOGO_HOME="$HOME_ROOT" "$@"
    else
        "$@"
    fi
}
# design 18 §2.2, the check before every launch: RT_PROBLEM is "" (fine),
# "missing" or "changed"; RT_WHY says what.
check_runtime() {
    local f found sum
    RT_PROBLEM="" RT_WHY=""
    for f in proton toolmanifest.vdf version; do
        if [ ! -f "$PROTON_ABS/$f" ]; then
            RT_PROBLEM=missing RT_WHY="$PROTON_ABS/$f is missing"
            return
        fi
    done
    found=""
    read -r found <"$PROTON_ABS/version" || true
    RT_VERSION="$found"
    if [ -n "$PROTON_VERSION" ] && [ "$found" != "$PROTON_VERSION" ]; then
        RT_PROBLEM=changed RT_WHY="version is '$found', expected '$PROTON_VERSION'"
        return
    fi
    sum="$(sha256sum "$PROTON_ABS/proton" 2>/dev/null)"
    sum="${sum%% *}"
    if [ -n "$PROTON_SHA" ] && [ "$sum" != "$PROTON_SHA" ]; then
        RT_PROBLEM=changed RT_WHY="the proton script changed (sha256 $sum, expected $PROTON_SHA)"
    fi
}
# A build gogo installed: its marker, or a store build (relative PROTON_DIR).
managed() {
    [ -f "$PROTON_ABS/gogo-managed.toml" ] && return 0
    case "$PROTON_DIR" in /*) return 1 ;; *) return 0 ;; esac
}
# UMU_PYTHON is a Nix store path a garbage collection can remove; setup
# records it again.
umu_problem() {
    if [ -n "$UMU_PYTHON" ] && [ ! -x "$UMU_PYTHON" ]; then
        printf "umu's Python %s is gone" "$UMU_PYTHON"
    elif [ ! -e "$UMU_RUN_ABS" ]; then
        printf 'umu-run %s is gone' "$UMU_RUN_ABS"
    fi
}
SETUP_ERR=""
run_setup() {
    local gogo out rc new
    gogo="$(find_gogo)"
    if [ -z "$gogo" ]; then
        log "$1, and no gogo found (GOGO_PATH=$GOGO_PATH): running the last setup"
        return 0
    fi
    log "$1: $gogo linux-app setup $GOGID"
    out="$(run_gogo "$gogo" linux-app setup "$GOGID" </dev/null 2>&1)"
    rc=$?
    [ -n "$out" ] && printf '%s\n' "$out" >>"$LOG"
    if [ "$rc" -ne 0 ]; then
        SETUP_ERR="$(printf '%s' "$out" | grep -v '^warning:' | tail -n 1 | cut -c1-300)"
        SETUP_ERR="setup failed (exit $rc): ${SETUP_ERR:-no output}; running the last applied config"
        log "$SETUP_ERR"
        return 0
    fi
    # R30: setup put a newer launcher here; this launch runs it.
    new="$(sed -n 's/^LAUNCHER_VERSION=//p' "$SELF" 2>/dev/null | head -n 1)"
    if [ -n "$new" ] && [ "$new" != "$LAUNCHER_VERSION" ]; then
        log "launcher $LAUNCHER_VERSION -> $new: restarting with the new one"
        exec "$SELF" ${ARGV[@]+"${ARGV[@]}"}
    fi
    load_conf
}

# Design 16 Open 1: nothing to run without the config setup writes.
if [ ! -f "$HERE/launch.conf" ]; then
    log "no launch.conf in $HERE: not launching"
    alert "This game isn't set up ($HERE/launch.conf is missing). Run: gogo linux-app setup $GOGID"
    exit 1
fi
load_conf
log "launch $TITLE ($GOGID) from $HERE (launcher $LAUNCHER_VERSION)"
want="$(current_stamp)"
check_runtime
why=""
if [ ! -d "$PREFIX_ABS" ]; then
    why="no prefix"
elif [ "$want" != "$STAMP" ]; then
    why="config stale (${STAMP:-none} -> $want)"
elif [ "$RT_PROBLEM" = missing ] || { [ "$RT_PROBLEM" = changed ] && managed; }; then
    why="Proton build $PROTON_DIR: $RT_WHY"
else
    why="$(umu_problem)"
fi
if [ -n "$why" ]; then run_setup "$why"; fi

# What the run log's header says beyond launch.conf: state.toml's view.
state_val() { sed -n "s/^$1 = \"\\(.*\\)\"\$/\\1/p" "$HERE/state.toml" 2>/dev/null | head -n 1; }
state_list() {
    sed -n "s/^$1 = \\[\\(.*\\)\\]\$/\\1/p" "$HERE/state.toml" 2>/dev/null | head -n 1 | tr -d '",'
}
RUNTIME="$(state_val runtime)"
[ -n "$RUNTIME" ] || RUNTIME="path:$PROTON_ABS"
RUNTIME_NOTE=""
RUN_LOG=""
LAST_RUN_LOG=""

if [ ! -d "$PREFIX_ABS" ]; then
    log "no prefix at $PREFIX_ABS: not launching"
    alert "This game's prefix is missing ($PREFIX_ABS) and gogo could not set it up. Install gogo and run: gogo linux-app setup $GOGID"
    exit 1
fi

# #77: one log per run, <LOG_DIR>/<YYYYmmdd-HHMMSS>-<pid>.log (the last
# KEEP_RUNS kept, latest.log -> the newest): a header naming the effective
# config, umu's and Proton's stdout and stderr (teed, so a terminal still
# sees them), then exit code and duration.
KEEP_RUNS=10
RUN_T0=0
RUN_SECS=0
TEE_PIDS=""
os_name() {
    local name=""
    if [ -r /etc/os-release ]; then
        name="$(sed -n 's/^PRETTY_NAME=//p' /etc/os-release | head -n 1 | tr -d '"')"
    fi
    printf '%s %s' "${name:-$(uname -sr)}" "$(uname -m)"
}
umu_name() {
    case "$UMU_RUN" in
        runtimes/umu/*) local v="${UMU_RUN#runtimes/umu/}"; printf '%s (zipapp)' "${v%%/*}" ;;
        *) printf '%s' "$UMU_RUN" ;;
    esac
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
    # A relaunch (exec: the same pid) within the same second: the next
    # second's name, so names still sort by time.
    while [ -e "$RUN_LOG" ]; do
        sleep 1
        RUN_LOG="$LOG_DIR/$(date '+%Y%m%d-%H%M%S')-$$.log"
    done
    RUN_T0=$SECONDS
    {
        printf '# gogo run log: %s (%s)\n' "$TITLE" "$GOGID"
        printf '# started: %s\n' "$(date '+%Y-%m-%d %H:%M:%S %z')"
        if [ -n "$SETUP_ERR" ]; then printf '# setup: %s\n' "$SETUP_ERR"; fi
        printf '# launcher: %s\n' "$LAUNCHER_VERSION"
        printf '# runtime: %s (%s)\n' "$RUNTIME" "${RT_VERSION:-no version file}"
        if [ -n "$RUNTIME_NOTE" ]; then printf '# runtime_note: %s\n' "$RUNTIME_NOTE"; fi
        printf '# runtime_dir: %s\n' "$PROTON_DIR"
        printf '# umu: %s\n' "$(umu_name)"
        printf '# umu_id: %s\n' "$UMU_ID"
        printf '# renderer: %s\n' "$RENDERER"
        printf '# protonfix: %s\n' "$(state_val protonfix)"
        printf '# protonfixes: %s\n' "$([ "$PROTONFIXES" = 0 ] && echo off || echo on)"
        printf '# verbs: %s\n' "$(state_list verbs)"
        printf '# stamp: %s\n' "$STAMP"
        printf '# os: %s\n' "$(os_name)"
        printf '# exe: %s\n' "$EXE"
        printf '# args: %s\n' "${ARGS[*]+${ARGS[*]}}${*:+ $*}"
        printf '# prefix: %s\n' "$PREFIX_ABS"
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
        # A straggler still holding the game's output open must not hold
        # the launcher: give the tees 2s to see EOF (KILL: they ignore TERM).
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
# A launch that can't run: a run log saying why, footer 127, a dialog.
not_runnable() {
    start_run_log "${ARGV[@]+"${ARGV[@]}"}"
    printf 'gogo: %s\n' "$1" >&2
    log "$1: not launching"
    end_run_log 127 "$2"
    alert "$3"
    exit 127
}

# The runtime check again (§2.2), after any setup.
check_runtime
if [ "$RT_PROBLEM" = changed ] && ! managed; then
    # The user picked this build and follows it.
    RUNTIME_NOTE="unmanaged build changed: $RT_WHY; launching it as it is"
    log "Proton build $PROTON_DIR: $RUNTIME_NOTE"
elif [ -n "$RT_PROBLEM" ]; then
    not_runnable "Proton build $PROTON_DIR: $RT_WHY" "runtime missing" \
        "This game's Proton build ($RUNTIME) is missing or changed: $RT_WHY. Run: gogo linux-app setup $GOGID (it reinstalls gogo's own builds), or pick another with gogo linux-app --retarget $GOGID --runtime ..."
fi
why="$(umu_problem)"
if [ -n "$why" ]; then
    not_runnable "$why" "umu missing" \
        "umu-launcher, which runs this game, is missing: $why. Run: gogo linux-app setup $GOGID"
fi
cd "$WORKDIR_ABS" 2>/dev/null || {
    log "workdir $WORKDIR_ABS missing"
    alert "The game folder $WORKDIR_ABS is missing. Run: gogo linux-app setup $GOGID"
    exit 1
}

# Teardown (design 18 §7.2): TERM to umu-run's process group, then, after
# 5 s, KILL for every process of this user whose environment has this
# WINEPREFIX (umu's container and Wine's services leave the group; their
# /proc/<pid>/environ still says whose they are).
PREFIX_REAL="$(readlink -f "$PREFIX_ABS" 2>/dev/null || printf '%s' "$PREFIX_ABS")"
prefix_pids() {
    grep -lzxF -e "WINEPREFIX=$PREFIX_ABS" -e "WINEPREFIX=$PREFIX_ABS/" \
        -e "WINEPREFIX=$PREFIX_REAL" -e "WINEPREFIX=$PREFIX_REAL/" \
        /proc/[0-9]*/environ 2>/dev/null | sed -e 's|^/proc/||' -e 's|/environ$||' | grep -vx -e "$$"
}
proc_names() {
    local p n out=""
    for p in "$@"; do
        n="$(cat "/proc/$p/comm" 2>/dev/null)" || continue
        out="$out$p $n;"
    done
    printf '%s' "$out"
}
WPID=""
TORN=""
teardown() {
    [ -n "$TORN" ] && return
    TORN=1
    if [ -n "$WPID" ]; then kill -TERM -- "-$WPID" 2>/dev/null; fi
    local left other
    other="$(other_launcher)"
    if [ -n "$other" ]; then
        # A later launch of this game shares the prefix (#103): only this
        # run's process group goes.
        log "launcher $other of this game is running: leaving the prefix's other processes"
        sleep 1
    else
        for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
            [ -z "$(prefix_pids)" ] && break
            sleep 0.25
        done
        left="$(prefix_pids)"
        if [ -n "$left" ]; then
            # shellcheck disable=SC2086
            log "reaped (SIGKILL): $(proc_names $left)"
            # shellcheck disable=SC2086
            kill -KILL $left 2>/dev/null
        fi
    fi
    if [ -n "$WPID" ]; then kill -KILL -- "-$WPID" 2>/dev/null; fi
    log "teardown done"
}

# #103: a launcher killed untrapped (SIGKILL) can't tear down, so a
# watchdog in its own process group does it: it polls the launcher every
# 2s and, once it's gone, runs the same teardown, ends the tees and writes
# the run log's footer. The launcher stops it before any exit of its own
# (and before the early-exit relaunch, which keeps the pid). Each running
# launcher leaves games/<gogid>/.launchers/<pid> (its start time), so a
# teardown spares the processes of a later launch of the same game.
WATCHDOG=""
ME_START=""
LIVE_DIR="$HERE/.launchers"
# Field 22 of /proc/<pid>/stat (after the parenthesized name): the start
# time, which a reused pid doesn't share.
proc_start() {
    local s
    s="$(cat "/proc/$1/stat" 2>/dev/null)" || return 0
    s="${s##*) }"
    # shellcheck disable=SC2086
    set -- $s
    printf '%s' "${20:-}"
}
# $1 is alive and is the process that started at $2: not a zombie, not a
# reused pid.
alive_as() {
    local want="$2" s
    s="$(cat "/proc/$1/stat" 2>/dev/null)" || return 1
    s="${s##*) }"
    # shellcheck disable=SC2086
    set -- $s
    [ "$1" != Z ] && [ "${20:-}" = "$want" ]
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
        log "watchdog: launcher $$ is gone (SIGKILL?): tearing down"
        teardown
        # The tees see EOF once the game's last writer is gone; they ignore
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
# shellcheck disable=SC2329 # called by the traps below
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

# #78 / design 18 §7.4: a run that ends within EARLY_EXIT_SECS or with a
# non-zero code explains itself. GOGO_LAUNCHER_DIALOG=<cmd> replaces the
# dialog tools (tests): it gets the title, the message and the button
# labels, and prints the chosen label.
EARLY_EXIT_SECS="${GOGO_EARLY_EXIT_SECS:-15}"
ask() {
    if [ -n "${GOGO_LAUNCHER_DIALOG:-}" ]; then "$GOGO_LAUNCHER_DIALOG" "$TITLE" "$@"; return; fi
    [ -n "${GOGO_LAUNCHER_NO_DIALOG:-}" ] && return 0
    local msg="$1" l a=() rc
    shift
    if gui && has zenity; then
        # --switch: only the buttons given, each printing its label.
        for l in "$@"; do a+=(--extra-button "$l"); done
        zenity --question --switch --no-markup --title "$TITLE" --text "$msg" \
            --width 560 --timeout 600 "${a[@]}" 2>/dev/null
        return 0
    fi
    if gui && has kdialog; then
        # Yes is the default (the last label), No the first, Cancel (and
        # Escape) the middle one.
        kdialog --title "$TITLE" --yes-label "$3" --no-label "$1" --cancel-label "$2" \
            --yesnocancel "$msg" >/dev/null 2>&1
        rc=$?
        case "$rc" in 0) printf '%s' "$3" ;; 1) printf '%s' "$1" ;; 2) printf '%s' "$2" ;; esac
        return 0
    fi
    if has notify-send; then
        notify-send -a gogo "$TITLE" "$TITLE exited after ${RUN_SECS}s, see $LAST_RUN_LOG" >/dev/null 2>&1
    fi
    return 0
}
# The game's output in a run log: between the header's and the footer's `# ---`.
run_output() { awk '/^# ---$/ { n++; next } n == 1' "$1"; }
last_errors() {
    local errs
    errs="$(run_output "$1" | grep -E -i 'err:|error|fail|exception|cannot|not found' | tail -n 5)"
    [ -n "$errs" ] || errs="$(run_output "$1" | tail -n 5)"
    printf '%s' "$errs" | cut -c1-160
}
# The report: what a bug report needs, the home dir as ~.
report() {
    local l
    {
        printf 'gogo launch report: %s (%s)\n' "$TITLE" "$GOGID"
        grep -E '^# (setup|launcher|runtime|runtime_note|runtime_dir|umu|umu_id|renderer|protonfix|protonfixes|verbs|stamp|os|exe|args|exit|duration):' "$1"
        printf -- '--- last 40 log lines ---\n'
        run_output "$1" | tail -n 40
    } | while IFS= read -r l; do printf '%s\n' "${l//"$HOME"/"~"}"; done
}
# Copy report: wl-copy on Wayland, else xclip; with neither, report.txt.
copy_tool() {
    if [ -n "${WAYLAND_DISPLAY:-}" ] && has wl-copy; then
        printf 'wl-copy'
    elif has xclip; then
        printf 'xclip'
    fi
}
early_exit() {
    local log="$LAST_RUN_LOG" gogo next="" msg choice copy copy_label
    if [ -z "${GOGO_LAUNCHER_DIALOG:-}" ] && [ -n "${GOGO_LAUNCHER_NO_DIALOG:-}" ]; then
        log "early exit: code $rc after ${RUN_SECS}s on $RENDERER (dialogs off)"
        return 0
    fi
    gogo="$(find_gogo)"
    if [ -n "$gogo" ]; then
        next="$(run_gogo "$gogo" linux-app next-renderer "$GOGID" --dry-run 2>>"$LOG")" || next=""
    fi
    log "early exit: code $rc after ${RUN_SECS}s on $RENDERER; next renderer: ${next:-none}"
    copy="$(copy_tool)"
    if [ -n "$copy" ]; then copy_label="Copy report"; else copy_label="Save report"; fi
    msg="$TITLE quit after ${RUN_SECS}s (exit code $rc) on the $RENDERER renderer.

$(last_errors "$log")

"
    if [ -n "$next" ]; then
        choice="$(ask "${msg}Try next renderer switches to $next and starts the game again." \
            "$copy_label" "Open log" "Try next renderer")"
    else
        choice="$(ask "${msg}There are no more renderers to try for this game." \
            "$copy_label" "Open log" "Close")"
    fi
    log "dialog: ${choice:-dismissed}"
    case "$choice" in
        "Open log") (xdg-open "$log" </dev/null >/dev/null 2>&1 &) ;;
        "Copy report")
            if [ "$copy" = wl-copy ]; then
                report "$log" | wl-copy
            else
                report "$log" | xclip -selection clipboard
            fi
            ;;
        "Save report")
            report "$log" >"$LOG_DIR/report.txt"
            log "report saved to $LOG_DIR/report.txt"
            ;;
        "Try next renderer")
            if run_gogo "$gogo" linux-app next-renderer "$GOGID" >>"$LOG" 2>&1; then
                log "relaunching on $next"
                # The stamp is stale now: the relaunch runs setup, which
                # applies the new renderer. Nothing was exported here, so
                # the environment is the one this launcher started with.
                exec "$SELF" ${ARGV[@]+"${ARGV[@]}"}
            fi
            log "next-renderer failed: not relaunching"
            ;;
    esac
}

# The game's environment (§7.2 step 4), in umu-run's process only. The L4
# and fix env first: WINEPREFIX (R7), GAMEID, STORE and PROTONPATH are
# gogo's and come last.
run_game() {
    local pair
    for pair in ${ENV[@]+"${ENV[@]}"}; do export "${pair?}"; done
    if [ -n "$DLL_OVERRIDES" ]; then
        export WINEDLLOVERRIDES="$DLL_OVERRIDES${WINEDLLOVERRIDES:+;$WINEDLLOVERRIDES}"
    fi
    if [ "$PROTONFIXES" = 0 ]; then export PROTONFIXES_DISABLE=1; fi
    if [ -n "$UMU_FOLDERS" ]; then export UMU_FOLDERS_PATH="$UMU_FOLDERS"; fi
    export WINEPREFIX="$PREFIX_ABS" GAMEID="$UMU_ID" STORE=gog PROTONPATH="$PROTON_ABS"
    if [ -n "$UMU_PYTHON" ]; then
        exec "$UMU_PYTHON" "$UMU_RUN_ABS" "$EXE_ABS" ${ARGS[@]+"${ARGS[@]}"} "$@"
    fi
    exec "$UMU_RUN_ABS" "$EXE_ABS" ${ARGS[@]+"${ARGS[@]}"} "$@"
}

start_run_log ${ARGV[@]+"${ARGV[@]}"}
log "exec umu-run $EXE_ABS (runtime $RUNTIME, prefix $PREFIX_ABS, log $RUN_LOG)"
set -m
( run_game ${ARGV[@]+"${ARGV[@]}"} ) 3>&- 4>&- &
WPID=$!
set +m
register_launch
start_watchdog
wait "$WPID"
rc=$?
log "umu-run exited $rc; waiting for the prefix's processes"
# A launcher exe may have started the real game and exited: wait for the
# whole prefix (Wine's wineserver included) to finish, not just umu-run.
if [ -z "$(other_launcher)" ]; then
    while [ -n "$(prefix_pids)" ]; do
        sleep 1 &
        wait $!
    done
fi
log "prefix idle"
stop_watchdog
unregister_launch
end_run_log "$rc"
if [ "$rc" -ne 0 ] || [ "$RUN_SECS" -lt "$EARLY_EXIT_SECS" ]; then early_exit; fi
exit "$rc"
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_carries_its_version() {
        let s = launch_sh();
        assert_eq!(version_of(&s), Some(LAUNCHER_VERSION.to_string().as_str()));
        assert!(!s.contains("@LAUNCHER_VERSION@"), "an unfilled placeholder");
        assert!(
            s.starts_with("#!/usr/bin/env bash\n"),
            "NixOS has no /bin/bash"
        );
    }

    /// The rendered launcher is shellcheck-clean (the dev shell has it, as
    /// does the Ubuntu CI image; skipped where it isn't installed).
    #[test]
    fn the_launcher_passes_shellcheck() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(LAUNCH_SH);
        std::fs::write(&path, launch_sh()).unwrap();
        let out = match std::process::Command::new("shellcheck")
            .arg("--shell=bash")
            .arg(&path)
            .output()
        {
            Ok(out) => out,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("shellcheck isn't installed: skipped");
                return;
            }
            Err(e) => panic!("shellcheck: {e}"),
        };
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn desktop_entry_quotes_exec_and_escapes_strings() {
        let e = desktop_entry(
            "1207659013",
            "Treasure \"Adventure\"\\Game",
            Path::new("/home/me/My $Games/100%/games/1207659013/launch.sh"),
            Some(Path::new(
                "/home/me/My $Games/100%/games/1207659013/icon.png",
            )),
        );
        assert_eq!(
            e,
            "[Desktop Entry]\nType=Application\nVersion=1.5\n\
             Name=Treasure \"Adventure\"\\\\Game\n\
             Exec=\"/home/me/My \\\\$Games/100%%/games/1207659013/launch.sh\"\n\
             Icon=/home/me/My $Games/100%/games/1207659013/icon.png\n\
             Terminal=false\nCategories=Game;\nX-Gogo-Gogid=1207659013\n"
        );
        let e = desktop_entry("1", " a\nb", Path::new("/x/a\"b`c\\d"), None);
        assert!(e.contains("Name=\\sa\\nb\n"), "{e}");
        assert!(e.contains(r#"Exec="/x/a\\"b\\`c\\\\d""#), "{e}");
        assert!(!e.contains("Icon="), "{e}");
    }
}
