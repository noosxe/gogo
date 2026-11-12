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
//! #103 watchdog and the early-exit dialog (§7.3, §7.4) as on the Mac. A
//! game with a wrapper (§7.8) has it between the two: the launcher starts
//! the wrapper, and the wrapper starts this script again for the game's step.

use std::path::{Path, PathBuf};

use super::game::{self, LAUNCH_SH};
use super::{icon, setup};
use crate::macapp::error::MacAppError;
use crate::macapp::launch::GameDirs;

/// Bumped with every change to [`TEMPLATE`] that matters to a game set up
/// before it. Setup rewrites `launch.sh` whenever its bytes differ, so this
/// is for people: the run log header and `launch.log` name it.
pub const LAUNCHER_VERSION: u32 = 15;

/// `launch.sh`'s first word when a wrapper runs it for the game's own step
/// (design 18 §7.8): the game's environment, then umu-run.
pub const RUN_GAME_ARG: &str = "--gogo-run-game";

/// `launch.sh`'s first word for a one-off run of another exe of the game
/// (design 18 §7.10): `--gogo-one-off <exe> <workdir> [args…]`, the paths
/// absolute, the args all the exe gets. `gogo linux-app run` passes it.
pub const ONE_OFF_ARG: &str = "--gogo-one-off";

/// [`RUN_GAME_ARG`] for a one-off's step under a wrapper:
/// `--gogo-run-one-off <mark> <exe> <workdir> [args…]`.
pub const RUN_ONE_OFF_ARG: &str = "--gogo-run-one-off";

/// `games/<gogid>/.one-offs/<pid>`: a running one-off's record beside its
/// `.launchers/<pid>`: the start time, then the exe.
pub const ONE_OFFS: &str = ".one-offs";

/// A one-off's run log: `one-off-<YYYYmmdd-HHMMSS>-<pid>.log`, apart from
/// the game's (no `latest.log`, pruned on their own).
pub const ONE_OFF_LOG_PREFIX: &str = "one-off-";

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
    TEMPLATE
        .replace("@LAUNCHER_VERSION@", &LAUNCHER_VERSION.to_string())
        .replace(
            "@GAME_DRIVE_OFF@",
            &format!("{}={}", setup::GAME_DRIVE_OFF.0, setup::GAME_DRIVE_OFF.1),
        )
        .replace("@RUN_GAME_ARG@", RUN_GAME_ARG)
        .replace("@RUN_ONE_OFF_ARG@", RUN_ONE_OFF_ARG)
        .replace("@ONE_OFF_ARG@", ONE_OFF_ARG)
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
    out.push_str(&format!("Exec={}\n", exec_value(launch_sh)));
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

/// The menu entry's `Exec=` value for `launch_sh`, as [`desktop_entry`]
/// writes it (`doctor` compares it).
pub fn exec_value(launch_sh: &Path) -> String {
    desktop_string(&exec_arg(&launch_sh.to_string_lossy()))
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
/// `/proc/<pid>/environ`), `sed`, `awk`; `nsenter` (util-linux) for the close request; zenity/kdialog/notify-send,
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
# A one-off run (design 18 section 7.10): set from the argv further down.
ONE_OFF="" ONE_EXE="" ONE_WORKDIR=""
GAME_ARGV=("$@")
SELF="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"
HERE="${SELF%/*}"
GOGID="${HERE##*/}"
TITLE="$GOGID"
XDG_ROOT="${XDG_DATA_HOME:-$HOME/.local/share}/gogo"
# The data root this launcher is in (<root>/games/<gogid>/launch.sh). A game in
# a library (design 18 section 6.1) is outside it: launch.conf names it, and
# when that dir is gone it is gogo's default.
HERE_ROOT="${HERE%/games/*}"
if [ -f "$HERE/launch.conf" ]; then
    LIBRARY_ROOT="$(
        LIBRARY_GAME="" DATA_ROOT=""
        # shellcheck source=/dev/null
        source "$HERE/launch.conf" >/dev/null 2>&1
        if [ -n "$LIBRARY_GAME" ]; then printf '%s' "$DATA_ROOT"; fi
    )"
    if [ -d "$LIBRARY_ROOT" ]; then
        HERE_ROOT="$LIBRARY_ROOT"
    elif [ -n "$LIBRARY_ROOT" ]; then
        HERE_ROOT="$XDG_ROOT"
    fi
fi
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
    # A one-off runs from a terminal: it says why there.
    if [ -n "$ONE_OFF" ]; then printf 'gogo: %s\n' "$1" >&2; return 0; fi
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
    RENDERER="" PROTONFIXES="1" UMU_FOLDERS="" GOGO_PATH="" DATA_ROOT="" LIBRARY_GAME=""
    DISPLAY_MODE="" DISPLAY_HDR="" DISPLAY_MONITOR=""
    ARGS=() ENV=() WRAPPER=()
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
    # A one-off runs its own exe, in its own dir, with only its own args;
    # everything else is the game's.
    if [ -n "$ONE_OFF" ]; then
        EXE="$ONE_EXE" EXE_ABS="$ONE_EXE" WORKDIR_ABS="$ONE_WORKDIR" ARGS=()
    fi
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

# Wine's display driver (design 18 section 7.9). DISPLAY_MODE is launch.conf's:
# wayland wants a Wayland socket where the game runs (under a wrapper, the
# wrapper's environment), x11 keeps the game off the Wayland driver whatever
# the environment says, and "" leaves it alone. DISPLAY_PICK is what the game
# gets, DISPLAY_WHY why it isn't what was asked. GE-Proton only uses its
# Wayland driver with WAYLAND_DISPLAY set and never says it didn't, so the
# check is here.
wayland_socket() {
    local s="${WAYLAND_DISPLAY:-}"
    case "$s" in
        "") return 1 ;;
        /*) ;;
        *) s="${XDG_RUNTIME_DIR:-}/$s" ;;
    esac
    [ -S "$s" ]
}
pick_display() {
    DISPLAY_PICK="" DISPLAY_WHY=""
    case "$DISPLAY_MODE" in
        wayland)
            if wayland_socket; then
                DISPLAY_PICK=wayland
            else
                DISPLAY_PICK=x11
                if [ -z "${WAYLAND_DISPLAY:-}" ]; then
                    DISPLAY_WHY="WAYLAND_DISPLAY is unset"
                else
                    DISPLAY_WHY="no Wayland socket at WAYLAND_DISPLAY=$WAYLAND_DISPLAY"
                fi
            fi
            ;;
        x11) DISPLAY_PICK=x11 ;;
    esac
}
DISPLAY_PICK="" DISPLAY_WHY=""
# What the run log and launch.log say about it.
display_line() {
    case "$DISPLAY_PICK" in
        wayland) printf 'wayland (WAYLAND_DISPLAY=%s)' "$WAYLAND_DISPLAY" ;;
        x11) printf 'x11%s' "${DISPLAY_WHY:+ (wayland wanted, but $DISPLAY_WHY)}" ;;
    esac
}
hdr_line() {
    [ -n "$DISPLAY_HDR" ] || return 0
    if [ "$DISPLAY_PICK" = wayland ]; then
        printf '%s' "$DISPLAY_HDR"
    else
        printf '%s (not applied: not on Wayland)' "$DISPLAY_HDR"
    fi
}
monitor_line() {
    [ -n "$DISPLAY_MONITOR" ] || return 0
    if [ "$DISPLAY_PICK" = wayland ]; then
        printf '%s' "$DISPLAY_MONITOR"
    else
        printf '%s (not applied: not on Wayland)' "$DISPLAY_MONITOR"
    fi
}
# Before ENV, so a player's own PROTON_* in the L4 env still wins.
display_env() {
    pick_display
    case "$DISPLAY_PICK" in
        wayland)
            # GE-Proton reads the alias after the switch: a session's
            # PROTON_USE_WAYLAND=0 would undo it.
            unset PROTON_USE_WAYLAND
            export PROTON_ENABLE_WAYLAND=1
            # forcehdr wins over hdr in Proton, and the aliases are read
            # last: a session's own would undo on and off.
            case "$DISPLAY_HDR" in
                on) unset PROTON_USE_HDR PROTON_FORCE_HDR; export PROTON_ENABLE_HDR=1 ;;
                force) export PROTON_FORCE_HDR=1 ;;
                off) unset PROTON_USE_HDR PROTON_FORCE_HDR; export PROTON_ENABLE_HDR=0 ;;
            esac
            # GE-Proton maps it to WAYLANDDRV_PRIMARY_MONITOR, and xrandr
            # (not there without Xwayland) is its guess only when unset.
            if [ -n "$DISPLAY_MONITOR" ]; then export PROTON_WAYLAND_MONITOR="$DISPLAY_MONITOR"; fi
            ;;
        x11) unset PROTON_ENABLE_WAYLAND PROTON_USE_WAYLAND ;;
    esac
    # Under a wrapper the check ran again in the wrapper's environment
    # (gamescope drops WAYLAND_DISPLAY without --expose-wayland): say what
    # the game got, in the run log.
    if [ -n "$GAME_STEP" ] && [ -n "$DISPLAY_MODE" ]; then
        printf 'gogo: display under the wrapper: %s\n' "$(display_line)" >&2
    fi
}
# The game step under a wrapper (design 18 §7.8): umu-run runs as this
# script's child, so its exit code can be written beside the mark (a wrapper
# like gamescope drops its child's). TERM, INT and HUP are passed on and the
# wait goes on. Not exec'd: this shell lives exactly as long as umu-run.
run_step() {
    local cpid=0 rc
    if [ -n "$UMU_PYTHON" ]; then
        "$UMU_PYTHON" "$UMU_RUN_ABS" "$EXE_ABS" ${ARGS[@]+"${ARGS[@]}"} "$@" 0<&0 &
    else
        "$UMU_RUN_ABS" "$EXE_ABS" ${ARGS[@]+"${ARGS[@]}"} "$@" 0<&0 &
    fi
    cpid=$!
    trap 'kill -TERM "$cpid" 2>/dev/null' TERM
    trap 'kill -INT "$cpid" 2>/dev/null' INT
    trap 'kill -HUP "$cpid" 2>/dev/null' HUP
    wait "$cpid"
    rc=$?
    # A trapped signal ends the wait early, with the child still there.
    while kill -0 "$cpid" 2>/dev/null; do
        wait "$cpid"
        rc=$?
    done
    # umu-run may have gone between the interrupted wait and the test above,
    # already reaped: bash keeps a reaped child's status for this wait.
    wait "$cpid"
    rc=$?
    printf '%s' "$rc" >"$GAME_MARK.code.tmp" && mv -f "$GAME_MARK.code.tmp" "$GAME_MARK.code"
    exit "$rc"
}
# The game's environment (§7.2 step 4), in umu-run's process only. No host
# drives (s:, x: -> $HOME, ...; setup::GAME_DRIVE_OFF), which the L4 env can
# undo. The L4 and fix env next: WINEPREFIX (R7), GAMEID, STORE and
# PROTONPATH are gogo's and come last.
GAME_STEP=""
run_game() {
    local pair
    export @GAME_DRIVE_OFF@
    display_env
    for pair in ${ENV[@]+"${ENV[@]}"}; do export "${pair?}"; done
    if [ -n "$DLL_OVERRIDES" ]; then
        export WINEDLLOVERRIDES="$DLL_OVERRIDES${WINEDLLOVERRIDES:+;$WINEDLLOVERRIDES}"
    fi
    if [ "$PROTONFIXES" = 0 ]; then export PROTONFIXES_DISABLE=1; fi
    if [ -n "$UMU_FOLDERS" ]; then export UMU_FOLDERS_PATH="$UMU_FOLDERS"; fi
    # A game in a library may be outside what the container shares by itself.
    if [ -n "$LIBRARY_GAME" ]; then
        export PRESSURE_VESSEL_FILESYSTEMS_RW="$HERE${PRESSURE_VESSEL_FILESYSTEMS_RW:+:$PRESSURE_VESSEL_FILESYSTEMS_RW}"
    fi
    export WINEPREFIX="$PREFIX_ABS" GAMEID="$UMU_ID" STORE=gog PROTONPATH="$PROTON_ABS"
    if [ -n "$GAME_STEP" ]; then run_step "$@"; fi
    if [ -n "$UMU_PYTHON" ]; then
        exec "$UMU_PYTHON" "$UMU_RUN_ABS" "$EXE_ABS" ${ARGS[@]+"${ARGS[@]}"} "$@"
    fi
    exec "$UMU_RUN_ABS" "$EXE_ABS" ${ARGS[@]+"${ARGS[@]}"} "$@"
}
# What umu-run's subshell runs. With a wrapper (design 18 §7.8) it goes outside
# umu-run and sees the environment this launcher started with: the game's goes
# on after it, in this script's second run (below), whose argv says where to
# say it got there. The wrapper's environment reaches umu-run underneath it:
# gamescope's DISPLAY, gamemoderun's LD_PRELOAD.
start_game() {
    if [ "${#WRAPPER[@]}" -gt 0 ] && [ -n "$ONE_OFF" ]; then
        exec "${WRAPPER[@]}" "$SELF" @RUN_ONE_OFF_ARG@ "$GAME_MARK" "$ONE_EXE" "$ONE_WORKDIR" "$@"
    fi
    if [ "${#WRAPPER[@]}" -gt 0 ]; then
        exec "${WRAPPER[@]}" "$SELF" @RUN_GAME_ARG@ "$GAME_MARK" "$@"
    fi
    run_game "$@"
}
# The wrapper running this script for the game's step: nothing else of the
# launcher happens here. The mark holds this pid, the parent of umu-run
# (run_step), which is there as long as the game is. A one-off's step says
# which exe first.
if [ "${1:-}" = @RUN_ONE_OFF_ARG@ ] && [ "$#" -ge 4 ]; then
    ONE_OFF=1 ONE_EXE="$3" ONE_WORKDIR="$4"
    set -- @RUN_GAME_ARG@ "$2" "${@:5}"
fi
if [ "${1:-}" = @RUN_GAME_ARG@ ] && [ "$#" -ge 2 ]; then
    GAME_MARK="$2"
    shift 2
    load_conf
    cd "$WORKDIR_ABS" || exit 1
    printf '%s' "$$" >"$GAME_MARK.tmp" && mv -f "$GAME_MARK.tmp" "$GAME_MARK"
    GAME_STEP=1
    run_game "$@"
fi

# Design 18 section 7.10: `gogo linux-app run` starts another exe of the game
# (a vendor launcher, a tool) through this launcher, with the game's whole
# setup: its exe, working dir and args replace the game's, and nothing of the
# game's config is written.
if [ "${1:-}" = @ONE_OFF_ARG@ ] && [ "$#" -ge 3 ]; then
    ONE_OFF=1 ONE_EXE="$2" ONE_WORKDIR="$3"
    GAME_ARGV=("${@:4}")
fi

# Design 16 Open 1: nothing to run without the config setup writes.
if [ ! -f "$HERE/launch.conf" ]; then
    log "no launch.conf in $HERE: not launching"
    alert "This game isn't set up ($HERE/launch.conf is missing). Run: gogo linux-app setup $GOGID"
    exit 1
fi
load_conf
if [ -n "$ONE_OFF" ]; then
    log "one-off run of $ONE_EXE for $TITLE ($GOGID) from $HERE (launcher $LAUNCHER_VERSION)"
else
    log "launch $TITLE ($GOGID) from $HERE (launcher $LAUNCHER_VERSION)"
fi
# Design 19 section 3.4: a DLC add that stopped while laying its files leaves
# dlc-journal.json. The game files may then mix two versions of the game's data,
# which could damage a save: no launch until the add is finished or undone.
if [ -e "$HERE/dlc-journal.json" ]; then
    log "a DLC add of $GOGID didn't finish (dlc-journal.json): not launching"
    alert "A DLC install for $TITLE stopped before it finished, so its files may be half there. Run: gogo linux-app dlc $GOGID add (it finishes the install, or undoes it)"
    exit 1
fi
# Design 20 section 5.4: a snapshot rollback or a conversion that stopped
# after its commit leaves its marker in the game dir. The files are the new
# ones, but the override and launcher may not be yet: no setup and no launch
# until gogo finishes it (refused below, with a run log and a dialog).
UNSETTLED="" UNSETTLED_MSG=""
if [ -e "$HERE/.gogo-rollback" ]; then
    UNSETTLED="a snapshot rollback isn't finished"
    UNSETTLED_MSG="A rollback of $TITLE to an earlier snapshot stopped before it finished. Run: gogo linux-app snapshot $GOGID list (it finishes the rollback), then start the game again."
elif [ -e "$HERE/.gogo-converted" ]; then
    UNSETTLED="a conversion for snapshots isn't finished"
    UNSETTLED_MSG="Turning $TITLE's dir into a btrfs subvolume for snapshots stopped before it finished. Run: gogo linux-app snapshot $GOGID list (it finishes it), then start the game again."
fi
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
if [ -n "$why" ] && [ -z "$UNSETTLED" ]; then run_setup "$why"; fi
# A normal launch runs the last applied config when setup can't run; a one-off
# doesn't: the game isn't in the state its config says.
if [ -n "$ONE_OFF" ] && [ -z "$UNSETTLED" ] && [ "$(current_stamp)" != "$STAMP" ]; then
    log "one-off: setup is pending (${SETUP_ERR:-no gogo to run it}): not running"
    alert "$TITLE's config changed and setup didn't apply it${SETUP_ERR:+ ($SETUP_ERR)}. Run: gogo linux-app setup $GOGID"
    exit 1
fi
if [ -n "$ONE_OFF" ] && [ ! -f "$ONE_EXE" ]; then
    log "one-off: no $ONE_EXE: not running"
    alert "There is no $ONE_EXE to run."
    exit 1
fi

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
    if [ -n "$ONE_OFF" ]; then runs=("$LOG_DIR"/one-off-[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]-*.log); fi
    [ -e "${runs[0]}" ] || return 0
    while [ $((${#runs[@]} - i)) -gt "$KEEP_RUNS" ]; do
        rm -f "${runs[$i]}"
        i=$((i + 1))
    done
}
# A one-off's log is named apart (design 18 section 7.10): latest.log, and
# doctor's last run with it, stay the game's.
run_log_name() { printf '%s/%s%s-%s.log' "$LOG_DIR" "${ONE_OFF:+one-off-}" "$(date '+%Y%m%d-%H%M%S')" "$$"; }
start_run_log() {
    local all=(${ARGS[@]+"${ARGS[@]}"} "$@")
    RUN_LOG="$(run_log_name)"
    # A relaunch (exec: the same pid) within the same second: the next
    # second's name, so names still sort by time.
    while [ -e "$RUN_LOG" ]; do
        sleep 1
        RUN_LOG="$(run_log_name)"
    done
    RUN_T0=$SECONDS
    {
        printf '# gogo run log: %s (%s)\n' "$TITLE" "$GOGID"
        if [ -n "$ONE_OFF" ]; then printf '# one-off: %s\n' "$ONE_EXE"; fi
        printf '# started: %s\n' "$(date '+%Y-%m-%d %H:%M:%S %z')"
        if [ -n "$SETUP_ERR" ]; then printf '# setup: %s\n' "$SETUP_ERR"; fi
        printf '# launcher: %s\n' "$LAUNCHER_VERSION"
        printf '# runtime: %s (%s)\n' "$RUNTIME" "${RT_VERSION:-no version file}"
        if [ -n "$RUNTIME_NOTE" ]; then printf '# runtime_note: %s\n' "$RUNTIME_NOTE"; fi
        printf '# runtime_dir: %s\n' "$PROTON_DIR"
        printf '# umu: %s\n' "$(umu_name)"
        printf '# umu_id: %s\n' "$UMU_ID"
        if [ "${#WRAPPER[@]}" -gt 0 ]; then printf '# wrapper: %s\n' "${WRAPPER[*]}"; fi
        if [ -n "$DISPLAY_MODE" ]; then printf '# display: %s\n' "$(display_line)"; fi
        if [ -n "$DISPLAY_HDR" ]; then printf '# hdr: %s\n' "$(hdr_line)"; fi
        if [ -n "$DISPLAY_MONITOR" ]; then printf '# monitor: %s\n' "$(monitor_line)"; fi
        printf '# renderer: %s\n' "$RENDERER"
        printf '# protonfix: %s\n' "$(state_val protonfix)"
        printf '# protonfixes: %s\n' "$([ "$PROTONFIXES" = 0 ] && echo off || echo on)"
        printf '# verbs: %s\n' "$(state_list verbs)"
        printf '# stamp: %s\n' "$STAMP"
        printf '# os: %s\n' "$(os_name)"
        printf '# exe: %s\n' "$EXE"
        printf '# args: %s\n' "${all[*]}"
        printf '# prefix: %s\n' "$PREFIX_ABS"
        printf '# ---\n'
    } >"$RUN_LOG"
    if [ -z "$ONE_OFF" ]; then ln -sfn "$(basename "$RUN_LOG")" "$LOG_DIR/latest.log"; fi
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
    start_run_log "${GAME_ARGV[@]+"${GAME_ARGV[@]}"}"
    printf 'gogo: %s\n' "$1" >&2
    log "$1: not launching"
    end_run_log 127 "$2"
    alert "$3"
    exit 127
}
if [ -n "$UNSETTLED" ]; then
    not_runnable "$UNSETTLED" "$UNSETTLED" "$UNSETTLED_MSG"
fi

# The display the game gets (section 7.9): a Wayland mode with no Wayland
# socket falls back to X11, logged; refusing would leave a player in an X11
# session or a nested compositor with no game at all.
pick_display
if [ "$DISPLAY_MODE" = wayland ] && [ "$DISPLAY_PICK" != wayland ]; then
    if [ "${#WRAPPER[@]}" -gt 0 ]; then
        log "display: wayland wanted, but $DISPLAY_WHY here: the wrapper's environment decides (checked again under it)"
    else
        log "display: wayland wanted, but $DISPLAY_WHY: the game runs on X11 (Xwayland)"
    fi
elif [ -n "$DISPLAY_MODE" ]; then
    log "display: $(display_line)${WRAPPER[0]:+, checked again under the wrapper}"
fi
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
if [ "${#WRAPPER[@]}" -gt 0 ] && ! has "${WRAPPER[0]}"; then
    not_runnable "wrapper ${WRAPPER[0]} isn't on PATH" "wrapper missing" \
        "This game runs under ${WRAPPER[0]}, which isn't on this launch's PATH. Install it, or run the game without it: gogo linux-app wrapper $GOGID --clear"
fi
cd "$WORKDIR_ABS" 2>/dev/null || {
    log "workdir $WORKDIR_ABS missing"
    alert "The game folder $WORKDIR_ABS is missing. Run: gogo linux-app setup $GOGID"
    exit 1
}

# Asking the game to close (design 18 §7.2), before the teardown below: a Unix
# TERM never reaches a Windows program as a close request, so it dies unsaved.
# `wineboot --end-session` sends every window WM_QUERYENDSESSION /
# WM_ENDSESSION. It has to reach the game's own wineserver, whose socket is in
# umu's container (a second umu-run is a second container with a wineserver of
# its own, which sees no game), so it runs inside that container: nsenter into
# the running wineserver's user and mount namespaces, with its environment and its Wine. The
# game then gets CLOSE_GRACE_SECS to end on its own; the request is bounded by
# the same time.
CLOSE_GRACE_SECS="${GOGO_CLOSE_GRACE_SECS:-10}"
CLOSING=""
SKIP_WAIT=""
# shellcheck disable=SC2329 # used by close_game
running() {
    local s
    s="$(cat "/proc/$1/stat" 2>/dev/null)" || return 1
    s="${s##*) }"
    [ "${s%% *}" != Z ]
}
# The wineserver of this prefix, if one runs.
# shellcheck disable=SC2329 # called by close_game
wineserver_pid() {
    local p
    for p in $(prefix_pids); do
        [ "$(cat "/proc/$p/comm" 2>/dev/null)" = wineserver ] && { printf '%s' "$p"; return; }
    done
}
# ask_close <wineserver pid>: wineboot --end-session in its container. Why it
# can't go is in the log. Not the pid namespace: that one belongs to a user
# namespace above ours and nsenter can't join it; wine needs only the mount
# namespace (the socket) and the userns that goes with it.
# shellcheck disable=SC2329 # called by close_game
ask_close() {
    local ws="$1" dir wine="" w ns=(-m) envs=()
    if ! has nsenter; then log "no nsenter: not asking the game to close"; return 1; fi
    dir="$(dirname "$(readlink "/proc/$ws/exe" 2>/dev/null)")"
    for w in wine wine64; do
        [ -x "/proc/$ws/root$dir/$w" ] && { wine="$dir/$w"; break; }
    done
    if [ -z "$wine" ]; then log "no wine next to wineserver $ws ($dir): not asking the game to close"; return 1; fi
    if [ "$(readlink "/proc/$ws/ns/user")" != "$(readlink /proc/self/ns/user)" ]; then
        ns=(--preserve-credentials -U -m)
    fi
    mapfile -d '' envs <"/proc/$ws/environ"
    timeout --kill-after=1 "$CLOSE_GRACE_SECS" nsenter -t "$ws" "${ns[@]}" -- \
        env -i "${envs[@]}" "$wine" wineboot --end-session </dev/null >>"$LOG" 2>&1 3>&- 4>&-
}
# shellcheck disable=SC2329 # called by on_signal, through the traps
close_game() {
    # A second signal skips the wait (see on_signal); no game started yet,
    # nothing to ask.
    [ -n "$CLOSING" ] && return 0
    CLOSING=1
    [ -n "$WPID" ] || return 0
    # A wrapper may have gone and left the game running.
    running "$WPID" || [ -n "$(prefix_pids)" ] || return 0
    local t0=$SECONDS bpid="" ws
    ws="$(wineserver_pid)"
    if [ -n "$ws" ]; then
        ask_close "$ws" &
        bpid=$!
        log "asked the game to close (wineboot --end-session in wineserver $ws's container, ${CLOSE_GRACE_SECS}s)"
    else
        log "no wineserver of this prefix: not asking the game to close"
    fi
    while [ $((SECONDS - t0)) -lt "$CLOSE_GRACE_SECS" ]; do
        if [ -n "$SKIP_WAIT" ]; then
            log "second signal after $((SECONDS - t0))s: not waiting for the game to close"
            if [ -n "$bpid" ]; then kill -KILL "$bpid" 2>/dev/null; fi
            return 0
        fi
        if ! running "$WPID" && { [ -z "$bpid" ] || ! running "$bpid"; } \
            && { [ -n "$(other_launcher)" ] || [ -z "$(prefix_pids)" ]; }; then
            log "closed by itself after $((SECONDS - t0))s"
            return 0
        fi
        sleep 0.25
    done
    log "didn't close in ${CLOSE_GRACE_SECS}s: TERM"
    if [ -n "$bpid" ]; then kill -KILL "$bpid" 2>/dev/null; fi
}

# Teardown (design 18 §7.2): TERM to umu-run's process group, then, after
# 5 s, KILL for every process of this user whose environment names this
# prefix (umu's container and Wine's services leave the group; their
# /proc/<pid>/environ still says whose they are). Inside the container
# Proton runs Wine with WINEPREFIX=<prefix>/pfx/ (umu's pfx -> . link) and
# STEAM_COMPAT_DATA_PATH=<prefix>, so all of these count (real smoke,
# design 18 §7.5).
PREFIX_REAL="$(readlink -f "$PREFIX_ABS" 2>/dev/null || printf '%s' "$PREFIX_ABS")"
PREFIX_MARKS=()
for p in "$PREFIX_ABS" "$PREFIX_REAL"; do
    for v in "WINEPREFIX=$p" "WINEPREFIX=$p/pfx" "STEAM_COMPAT_DATA_PATH=$p"; do
        PREFIX_MARKS+=(-e "$v" -e "$v/")
    done
done
prefix_pids() {
    grep -lzxF "${PREFIX_MARKS[@]}" /proc/[0-9]*/environ 2>/dev/null \
        | sed -e 's|^/proc/||' -e 's|/environ$||' | grep -vx -e "$$"
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
        # A wrapper can take the game out of its process group (§7.8).
        if [ "${#WRAPPER[@]}" -gt 0 ]; then
            left="$(prefix_pids)"
            # shellcheck disable=SC2086
            if [ -n "$left" ]; then kill -TERM $left 2>/dev/null; fi
        fi
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
GAME_MARK="$LOG_DIR/.run-$$.game"
LIVE_DIR="$HERE/.launchers"
ONE_OFF_DIR="$HERE/.one-offs"
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
    # What doctor names while a one-off runs.
    if [ -n "$ONE_OFF" ]; then
        mkdir -p "$ONE_OFF_DIR" && printf '%s\n%s\n' "$ME_START" "$ONE_EXE" >"$ONE_OFF_DIR/$$"
    fi
}
unregister_launch() {
    rm -f "$GAME_MARK" "$GAME_MARK.tmp" "$GAME_MARK.code" "$GAME_MARK.code.tmp"
    if [ "$(cat "$LIVE_DIR/$$" 2>/dev/null)" = "$ME_START" ]; then rm -f "$LIVE_DIR/$$"; fi
    if [ -n "$ONE_OFF" ]; then
        rm -f "$ONE_OFF_DIR/$$"
        rmdir "$ONE_OFF_DIR" 2>/dev/null
    fi
}
# A move of this game holds its lock right now. Without flock (util-linux) or
# without the lock file, nothing says so: the launch goes on.
moving() {
    local lock="$ROOT/game-moves/$GOGID.lock"
    [ -f "$lock" ] || return 1
    if ! has flock; then log "no flock: not checking for a move"; return 1; fi
    # 99 only when the lock is taken: any other failure isn't a move.
    flock --shared --nonblock --conflict-exit-code 99 "$lock" true 3>&- 4>&- 2>/dev/null
    [ $? -eq 99 ]
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
    log "guard armed ($WATCHDOG)"
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
    # Bash defers a signal while its own trap runs, so a second TERM would
    # wait for this whole handler, the close wait included. Replacing the traps
    # first lets the second signal (of any of the three) run at once: it only
    # flags close_game's wait to end. A wait on a child (or a sleep, within
    # 0.25 s) gives way to it.
    trap 'log "got another signal"; SKIP_WAIT=1' INT TERM HUP
    # The guard stays armed while the game is asked to close: a launcher
    # killed meanwhile is still torn down.
    close_game
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
        grep -E '^# (setup|launcher|runtime|runtime_note|runtime_dir|umu|umu_id|wrapper|renderer|protonfix|protonfixes|verbs|stamp|os|exe|args|exit|duration):' "$1"
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
    local log="$LAST_RUN_LOG" gogo next="" msg choice copy copy_label on
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
    # `proton-default` is L1's wined3d pick with nothing set (#229).
    if [ "$RENDERER" = proton-default ]; then on="on Proton's default renderer"; else on="on the $RENDERER renderer"; fi
    msg="$TITLE quit after ${RUN_SECS}s (exit code $rc) $on${WRAPPER[0]:+, under ${WRAPPER[0]}}.

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

# The wrapper has exited: did the game's step ever run? A wrapper that forks
# and leaves is ahead of its child, so give that a moment, and the step, once
# it has marked its pid, the moment it takes to reach umu-run (which is when
# prefix_pids can see it). Nothing here waits on a game already gone.
wrapper_ran_game() {
    local pid="" i
    for i in 1 2 3 4 5 6 7 8; do
        [ -e "$GAME_MARK" ] && break
        [ -n "$(prefix_pids)" ] && return 0
        sleep 0.25
    done
    [ -e "$GAME_MARK" ] || return 1
    pid="$(cat "$GAME_MARK" 2>/dev/null)"
    for i in 1 2 3 4 5 6 7 8 9 10; do
        [ -n "$pid" ] && running "$pid" && [ -z "$(prefix_pids)" ] || break
        sleep 0.1
    done
    return 0
}

start_run_log ${GAME_ARGV[@]+"${GAME_ARGV[@]}"}
log "exec umu-run $EXE_ABS (runtime $RUNTIME, prefix $PREFIX_ABS, log $RUN_LOG)"
if [ "${#WRAPPER[@]}" -gt 0 ]; then log "under wrapper: ${WRAPPER[*]}"; fi
# Registered before the game starts: a launcher killed in the gap would leave
# a game nothing knows about. Only the watchdog needs WPID, so it follows.
register_launch
# Design 18 section 6.1: a `linux-app move` of this game holds
# <root>/game-moves/<gogid>.lock for as long as it runs. Registered first, then
# the lock: a move that took it before this try is seen here, and one that
# takes it after is seen by its own running-check. The kernel drops the lock of
# a move that died, so an interrupted one never gets here.
if moving; then
    log "a move of $GOGID is in progress: not launching"
    unregister_launch
    end_run_log 127 "game is being moved"
    alert "$TITLE is being moved to another place by gogo (gogo linux-app move). Wait for it to finish, then start the game again."
    exit 127
fi
# A move that began and ended since this launcher read its config: the dir it
# runs from is gone, and the registration above has just made an empty one.
if [ ! -f "$HERE/launch.conf" ]; then
    log "$HERE has been moved away: not launching"
    unregister_launch
    rmdir "$LIVE_DIR" "$HERE" 2>/dev/null
    end_run_log 127 "game was moved"
    alert "$TITLE was moved to another place by gogo while this was starting. Start it again from its menu entry."
    exit 127
fi
# A one-off shares the prefix with nothing (design 18 section 7.10): not with
# the game, not with another one-off, not with what either left running.
# Registered first, so of two starting at once neither runs.
if [ -n "$ONE_OFF" ]; then
    busy="$(other_launcher)"
    if [ -n "$busy" ]; then
        busy="launcher $busy of this game is running"
    else
        busy_pids="$(prefix_pids)"
        # shellcheck disable=SC2086
        if [ -n "$busy_pids" ]; then busy="processes are running in its prefix: $(proc_names $busy_pids)"; fi
    fi
    if [ -n "$busy" ]; then
        log "one-off: $busy: not running"
        unregister_launch
        end_run_log 127 "game is running"
        alert "$TITLE is running ($busy). Quit it first."
        exit 127
    fi
fi
set -m
rm -f "$GAME_MARK" "$GAME_MARK.tmp" "$GAME_MARK.code" "$GAME_MARK.code.tmp"
( start_game ${GAME_ARGV[@]+"${GAME_ARGV[@]}"} ) 3>&- 4>&- &
WPID=$!
set +m
start_watchdog
wait "$WPID"
rc=$?
if [ "${#WRAPPER[@]}" -gt 0 ]; then
    log "wrapper ${WRAPPER[0]} exited $rc; waiting for the prefix's processes"
    if ! wrapper_ran_game; then
        # The renderer has nothing to do with this: no early-exit dialog.
        log "wrapper ${WRAPPER[0]} never ran the game"
        stop_watchdog
        unregister_launch
        if [ "$rc" -eq 0 ]; then rc=1; fi
        end_run_log "$rc" "wrapper never ran the game"
        alert "${WRAPPER[0]}, which this game runs under, exited with code $rc before it started the game. Its output is in $LAST_RUN_LOG. To run the game without it: gogo linux-app wrapper $GOGID --clear"
        exit "$rc"
    fi
else
    log "umu-run exited $rc; waiting for the prefix's processes"
fi
# A launcher exe may have started the real game and exited: wait for the
# whole prefix (Wine's wineserver included) to finish, not just umu-run.
if [ -z "$(other_launcher)" ]; then
    while [ -n "$(prefix_pids)" ]; do
        sleep 1 &
        wait $!
    done
fi
log "prefix idle"
# The game step's own record of umu-run's code, which a wrapper may have
# dropped: the step is gone a moment after umu-run.
if [ "${#WRAPPER[@]}" -gt 0 ]; then
    gpid="$(cat "$GAME_MARK" 2>/dev/null)"
    for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
        [ -e "$GAME_MARK.code" ] || [ -z "$gpid" ] || ! running "$gpid" && break
        sleep 0.1
    done
    gcode="$(cat "$GAME_MARK.code" 2>/dev/null)"
    case "$gcode" in
        ''|*[!0-9]*) ;;
        *)
            log "wrapper ${WRAPPER[0]} exited $rc; the game exited $gcode"
            rc="$gcode"
            ;;
    esac
fi
stop_watchdog
unregister_launch
end_run_log "$rc"
# A one-off's tool may well end at once: no dialog, and never a renderer
# switch, which would change the game's config.
if [ -n "$ONE_OFF" ]; then
    log "one-off ended: code $rc after ${RUN_SECS}s"
elif [ "$rc" -ne 0 ] || [ "$RUN_SECS" -lt "$EARLY_EXIT_SECS" ]; then
    early_exit
fi
exit "$rc"
"##;

#[cfg(test)]
mod tests {
    use super::*;

    /// #229: the header keeps the machine value; the dialog says what
    /// `proton-default` is.
    #[test]
    fn proton_default_reads_as_proton_defaults_in_the_dialog() {
        let s = launch_sh();
        assert!(s.contains("printf '# renderer: %s\\n' \"$RENDERER\""));
        assert!(s.contains("on Proton's default renderer"));
    }

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

    /// How the game starts, with a wrapper and without: one template, and
    /// `launch.conf`'s `WRAPPER` picks the branch (design 18 §7.8). The
    /// quoting guarantees of §7.6 live in these lines.
    #[test]
    fn the_game_start_matches_the_golden_file() {
        let s = launch_sh();
        let at = s.find("# The game step under a wrapper").unwrap();
        let end = s.find("# Design 16 Open 1:").unwrap();
        assert_eq!(
            &s[at..end],
            include_str!("../../tests/fixtures/linuxapp/launch.sh.start-game.golden")
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
