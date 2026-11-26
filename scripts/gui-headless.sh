#!/usr/bin/env bash
# Run the desktop GUI in a headless compositor, never on the host display
# (design 24 §5). Linux only. Run it inside the dev shell, which provides the
# libraries the application loads:
#
#   nix develop .#lavapipe -c scripts/gui-headless.sh run -- target/debug/gogo-gui --fixture
#   nix develop -c scripts/gui-headless.sh self-test
#
# run [options] -- COMMAND [ARG...]
#   starts sway (headless, software rendering) in a sandbox, starts COMMAND in
#   it, waits for its first frame, takes a screenshot and stops everything.
#     --renderer auto|gpu|cpu  GOGO_GUI_RENDERER for the application (auto).
#                              gpu needs the .#lavapipe shell (software Vulkan)
#     --no-vulkan              give the application no Vulkan driver at all
#     --size WxH               the output, which the window fills (1280x800)
#     --ready TEXT             the first frame is on screen once COMMAND printed
#                              TEXT ("gui: frame presented"; "" = by sight only)
#     --timeout SECS           the whole run (120)
#     --name NAME              names the output folder
#
# reload [options] -- GOGO_GUI
#   the hot-reload check: starts GOGO_GUI on fictional data over a copy of the
#   built-in skin, edits the copy, breaks it and mends it, with a screenshot of
#   each (1-start, 2-edited, 3-broken, 4-mended). Takes run's options.
#
# self-test
#   checks the sandbox, the screenshot and the teardown with a small Wayland
#   client, so it needs no build.
#
# Output: target/gui-headless/<name>.<pid>/ with screenshots and logs; its path
# is the last line on stdout. Exit codes: 0 ok; 1 the application didn't show,
# ended early or ran out of time; 2 usage; 3 the harness itself failed.
#
# The sandbox has no network, no host sockets, no home folder and no host
# graphics driver. Besides /nix it sees only the output folder and, read-only,
# the repository's scripts/ and COMMAND itself (and for reload the built-in
# skin): nothing else of the repository.
# COMMAND must therefore be in the repository or /nix.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
SELF="$REPO/scripts/gui-headless.sh"
HELPER="$REPO/scripts/linux-e2e-helper.py"
ROOT="$REPO/target/gui-headless"
RUNTIME_DIR=/run/gui
# A screen that differs from the empty compositor by this much shows a window.
SHOWN_PCT=2

log() { printf '[gui-headless] %s\n' "$*" >&2; }
die() { log "error: $1"; exit "${2:-3}"; }
usage() { sed -n '2,/^# COMMAND must therefore/p' "$SELF" | sed 's/^# \{0,1\}//' >&2; exit 2; }

# --- inside the sandbox -------------------------------------------------------

helper() { python3 -I "$HELPER" "$@"; }

# changed_pct of two PNGs, as a whole number.
changed() {
    helper diff "$1" "$2" | python3 -I -c 'import json,sys; print(int(json.load(sys.stdin)["changed_pct"]))'
}

start_sway() {
    local conf="$OUT/sway.conf" env_file="$OUT/sway.env"
    cat >"$conf" <<EOF
xwayland disable
output HEADLESS-1 resolution $SIZE scale 1
default_border none
default_floating_border none
focus_follows_mouse no
exec env -0 > "$env_file.tmp" && mv "$env_file.tmp" "$env_file"
EOF
    sway --config "$conf" >"$OUT/sway.log" 2>&1 &
    SWAY_PID=$!
    for _ in $(seq 300); do
        [ -e "$env_file" ] && break
        kill -0 "$SWAY_PID" 2>/dev/null || die "sway ended at start (see sway.log)"
        sleep 0.1
    done
    [ -e "$env_file" ] || die "sway didn't start in 30 s (see sway.log)"
    local item
    while IFS= read -r -d '' item; do
        case "$item" in
        WAYLAND_DISPLAY=*) export WAYLAND_DISPLAY="${item#*=}" ;;
        SWAYSOCK=*) export SWAYSOCK="${item#*=}" ;;
        esac
    done <"$env_file"
    rm -f "$env_file"
    for _ in $(seq 100); do
        [ -S "$RUNTIME_DIR/${WAYLAND_DISPLAY:-none}" ] && return 0
        sleep 0.1
    done
    die "no Wayland socket (see sway.log)"
}

# The only display in the sandbox is the headless sway: nothing here can reach
# the host's.
check_isolation() {
    local sockets=("$RUNTIME_DIR"/wayland-*) found=() s
    for s in "${sockets[@]}"; do [ -S "$s" ] && found+=("$s"); done
    [ "${#found[@]}" = 1 ] && [ "${found[0]}" = "$RUNTIME_DIR/$WAYLAND_DISPLAY" ] ||
        die "unexpected Wayland sockets: ${found[*]-none}"
    [ -z "${DISPLAY:-}" ] || die "DISPLAY is set inside the sandbox"
    [ ! -e /tmp/.X11-unix ] || die "an X socket is visible inside the sandbox"
    [ ! -e /run/user ] || die "the host's runtime folder is visible inside the sandbox"
}

shot() { grim "$OUT/$1.png" 2>>"$OUT/sway.log" || die "grim failed (see sway.log)"; }

# The application gets the dev shell's libraries; sway and grim must not.
start_app() {
    local env=("GOGO_GUI_RENDERER=$RENDERER" "GUI_OUT=$OUT")
    [ -z "${APP_LD_LIBRARY_PATH:-}" ] || env+=("LD_LIBRARY_PATH=$APP_LD_LIBRARY_PATH")
    [ -z "${APP_VK_DRIVER_FILES:-}" ] || env+=("VK_DRIVER_FILES=$APP_VK_DRIVER_FILES")
    [ -z "${APP_WGPU_BACKEND:-}" ] || env+=("WGPU_BACKEND=$APP_WGPU_BACKEND")
    [ -z "${APP_RUST_LOG:-}" ] || env+=("RUST_LOG=$APP_RUST_LOG")
    env "${env[@]}" "$@" >"$OUT/app.log" 2>&1 &
    APP_PID=$!
}

app_alive() { kill -0 "$APP_PID" 2>/dev/null; }

# Waits until the application printed TEXT at least N times.
wait_log() { # text [n]
    local want="${2:-1}"
    until [ "$(grep -cF -- "$1" "$OUT/app.log" || true)" -ge "$want" ]; do
        app_alive || { log "the application ended (see app.log)"; return 1; }
        sleep 0.1
    done
}

# Waits until the screen is no longer empty, then until it holds still.
wait_shown() { # name
    until shot "$1" && [ "$(changed "$OUT/blank.png" "$OUT/$1.png")" -ge "$SHOWN_PCT" ]; do
        app_alive || { log "the application ended (see app.log)"; return 1; }
        sleep 0.1
    done
    settle "$1"
}

settle() { # name
    for _ in $(seq 50); do
        sleep 0.3
        mv "$OUT/$1.png" "$OUT/.previous.png"
        shot "$1"
        [ "$(changed "$OUT/.previous.png" "$OUT/$1.png")" = 0 ] && break
    done
    rm -f "$OUT/.previous.png"
}

stop_app() {
    kill -TERM "$APP_PID" 2>/dev/null || true
    wait "$APP_PID" 2>/dev/null || true
}

scenario_run() {
    start_app "$@"
    [ -z "$READY" ] || wait_log "$READY" || return 1
    wait_shown frame || return 1
    stop_app
}

# Whether the top halves of two screenshots are the same pixels.
same_top() { # a b
    python3 -I -c '
import importlib.util, sys
spec = importlib.util.spec_from_file_location("helper", sys.argv[1])
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)
a, b = (helper.read_png(p) for p in sys.argv[2:4])
half = a[1] // 2
sys.exit(a[:3] != b[:3] or a[3][:half] != b[3][:half])
' "$HELPER" "$OUT/$1.png" "$OUT/$2.png"
}

# Waits until the screen differs from the screenshot BEFORE, then shoots it.
shot_changed() { # name before
    until shot "$1" && [ "$(helper diff "$OUT/$2.png" "$OUT/$1.png")" != '{"changed_pct": 0.0}' ]; do
        app_alive || { log "the application ended (see app.log)"; return 1; }
        sleep 0.1
    done
    settle "$1"
}

check() { # what command…
    local what="$1"
    shift
    "$@" || { log "reload: $what"; return 1; }
}

scenario_reload() {
    local skin="$OUT/skin/default" route
    mkdir -p "$OUT/skin"
    cp -r --no-preserve=mode "$REPO/crates/gogo-skins/skins/default" "$skin"
    route="$skin/routes/game.kdl"
    cp "$route" "$OUT/game.kdl.good"

    start_app "$@" --fixture --dev "$skin"
    wait_log "gui: skin loaded" || return 1
    wait_log "gui: frame presented" || return 1
    wait_shown 1-start || return 1

    # A colour edit shows.
    sed -i 's/accent "#7aa2ff"/accent "#ff7a45"/' "$skin/tokens.kdl"
    wait_log "gui: skin reloaded" || return 1
    shot_changed 2-edited 1-start || return 1

    # A broken edit keeps the last good skin, with the error over it.
    printf '\nnonsense {\n' >>"$route"
    wait_log "gui: skin reload failed" || return 1
    shot_changed 3-broken 2-edited || return 1
    check "the broken edit changed the skin on screen" same_top 2-edited 3-broken || return 1
    check "the error does not name the file" grep -qF "routes/game.kdl" "$OUT/app.log" || return 1

    # Mended, the panel goes.
    cp "$OUT/game.kdl.good" "$route"
    wait_log "gui: skin reloaded" 2 || return 1
    shot_changed 4-mended 3-broken || return 1
    check "mending did not bring the edited skin back" \
        test "$(changed "$OUT/2-edited.png" "$OUT/4-mended.png")" = 0 || return 1
    stop_app
    grep -F "gui: skin" "$OUT/app.log" >&2
}

inside() {
    OUT="$1" SIZE="$2" RENDERER="$3" READY="$4" SCENARIO="$5"
    shift 5
    start_sway
    check_isolation
    shot blank
    local status=0
    "scenario_$SCENARIO" "$@" || status=1
    swaymsg exit >/dev/null 2>&1 || true
    wait "$SWAY_PID" 2>/dev/null || true
    exit "$status"
}

# --- outside ------------------------------------------------------------------

resolve_tools() {
    if [ -n "${GUI_HEADLESS_TOOLS:-}" ]; then TOOLS="$GUI_HEADLESS_TOOLS"; return 0; fi
    command -v nix >/dev/null || die "nix not found"
    TOOLS="$(nix build --no-link --print-out-paths "$REPO#gui-headless-tools")" ||
        die "couldn't build .#gui-headless-tools"
}

# Only store paths of LD_LIBRARY_PATH: whatever the host appended stays outside.
store_paths() {
    local out="" dir
    local IFS=:
    for dir in ${1:-}; do
        case "$dir" in /nix/store/*) out="${out:+$out:}$dir" ;; esac
    done
    printf '%s' "$out"
}

# Processes still carrying a run's marker.
leftovers() { # run-id
    "$TOOLS/bin/grep" -lzxF "GUI_HEADLESS_RUN=$1" /proc/[0-9]*/environ 2>/dev/null || true
}

# The path of COMMAND as the sandbox sees it. A bare name is looked up in the
# harness's tools first: the caller's PATH may lead outside what is bound.
sandbox_command() {
    local command
    case "$1" in
    */*) command="$1" ;;
    *) command="$(PATH="$TOOLS/bin:$PATH" type -P -- "$1")" || die "no such command: $1" 2 ;;
    esac
    [ -x "$command" ] || die "not a program: $1" 2
    # Not the resolved path: a multi-call binary goes by its link's name.
    command="$("$TOOLS/bin/realpath" -s -- "$command")"
    case "$command" in
    "$REPO"/* | /nix/store/*) ;;
    *) command="$("$TOOLS/bin/realpath" -- "$command")" ;;
    esac
    case "$command" in
    "$REPO"/* | /nix/store/*) printf '%s' "$command" ;;
    *) die "$1 is outside the repository and /nix: the sandbox cannot see it" 2 ;;
    esac
}

cmd_run() { # scenario [options] -- command…
    local scenario="$1"
    shift
    local renderer=auto vulkan=1 size=1280x800 ready="gui: frame presented" timeout=120 name="$scenario"
    while [ $# -gt 0 ]; do
        case "$1" in
        --renderer) renderer="${2:?}"; shift 2 ;;
        --no-vulkan) vulkan=0; shift ;;
        --size) size="${2:?}"; shift 2 ;;
        --ready) ready="${2?}"; shift 2 ;;
        --timeout) timeout="${2:?}"; shift 2 ;;
        --name) name="${2:?}"; shift 2 ;;
        --) shift; break ;;
        *) usage ;;
        esac
    done
    [ $# -gt 0 ] || usage
    case "$renderer" in auto | gpu | cpu) ;; *) usage ;; esac
    [[ "$size" =~ ^[0-9]+x[0-9]+$ ]] || usage
    [[ "$timeout" =~ ^[0-9]+$ ]] || usage
    [[ "$name" =~ ^[A-Za-z0-9._-]+$ ]] || usage
    [ "$(uname -s)" = Linux ] || die "Linux only (macOS: scripts/vm-test.sh)" 2
    resolve_tools

    local icd=/dev/null
    if [ "$vulkan" = 1 ]; then
        case "${VK_DRIVER_FILES:-}" in
        /nix/store/*lvp_icd*) icd="$VK_DRIVER_FILES" ;;
        *) [ "$renderer" != gpu ] ||
            die "--renderer gpu needs software Vulkan: run under nix develop .#lavapipe" 2 ;;
        esac
    fi

    local command
    command="$(sandbox_command "$1")" || exit 2
    shift

    mkdir -p "$ROOT"
    OUT="$ROOT/$name.$$"
    rm -rf -- "$OUT"
    mkdir -p "$OUT/home"
    local id="$name.$$.$RANDOM"

    local sandbox=(--unshare-user --unshare-pid --unshare-ipc --unshare-uts --unshare-net
        --unshare-cgroup-try --die-with-parent --new-session
        --ro-bind /nix /nix --proc /proc --dev /dev --tmpfs /tmp --tmpfs /run
        --perms 0700 --dir "$RUNTIME_DIR"
        --ro-bind "$REPO/scripts" "$REPO/scripts" --bind "$OUT" "$OUT" --chdir "$OUT")
    # Never the whole repository: ignored folders in it can hold secrets.
    case "$command" in
    "$REPO"/*) sandbox+=(--ro-bind "$command" "$command") ;;
    esac
    # The reload check copies the built-in skin into its output folder.
    if [ "$scenario" = reload ]; then
        local builtin="$REPO/crates/gogo-skins/skins/default"
        sandbox+=(--ro-bind "$builtin" "$builtin")
    fi
    local env=(HOME="$OUT/home" PATH="$TOOLS/bin" LANG=C.UTF-8
        XDG_RUNTIME_DIR="$RUNTIME_DIR" XDG_CONFIG_HOME="$OUT/home/config"
        XDG_CACHE_HOME="$OUT/home/cache" XDG_DATA_HOME="$OUT/home/data"
        XDG_STATE_HOME="$OUT/home/state"
        WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman
        WLR_RENDERER_ALLOW_SOFTWARE=1 GUI_HEADLESS_RUN="$id"
        APP_LD_LIBRARY_PATH="$(store_paths "${LD_LIBRARY_PATH:-}")"
        APP_VK_DRIVER_FILES="$icd" APP_WGPU_BACKEND="${WGPU_BACKEND:-}"
        APP_RUST_LOG="${RUST_LOG:-}")

    # timeout kills bwrap, whose PID namespace takes everything in it along;
    # setpriv does the same when this script is killed outright.
    local status=0 pid
    "$TOOLS/bin/setpriv" --pdeathsig TERM -- \
        "$TOOLS/bin/timeout" --kill-after=5 "$timeout" \
        "$TOOLS/bin/env" -i "${env[@]}" \
        "$TOOLS/bin/bwrap" "${sandbox[@]}" \
        "$TOOLS/bin/bash" "$SELF" __inside "$OUT" "$size" "$renderer" "$ready" "$scenario" "$command" "$@" \
        </dev/null &
    pid=$!
    trap 'kill -TERM "$pid" 2>/dev/null || true' INT TERM HUP
    # A trapped signal ends `wait` early; wait again for the real end.
    wait "$pid" || status=$?
    while kill -0 "$pid" 2>/dev/null; do status=0; wait "$pid" || status=$?; done
    trap - INT TERM HUP

    local left=""
    for _ in $(seq 25); do
        left="$(leftovers "$id")"
        [ -z "$left" ] && break
        sleep 0.2
    done
    if [ -n "$left" ]; then
        log "left behind: $left"
        status=3
    fi
    case "$status" in
    0) ;;
    1 | 3) ;;
    124 | 137) log "over the time limit of ${timeout} s"; status=1 ;;
    *) log "the run ended with $status"; status=1 ;;
    esac
    [ "$status" = 0 ] || [ ! -f "$OUT/app.log" ] || { log "end of app.log:"; tail -n 20 "$OUT/app.log" >&2; }
    printf '%s\n' "$OUT"
    return "$status"
}

# --- self-test ----------------------------------------------------------------

st_fail() { log "self-test: $1"; ST_FAILED=1; }

cmd_self_test() {
    [ $# = 0 ] || usage
    resolve_tools
    export GUI_HEADLESS_TOOLS="$TOOLS"
    ST_FAILED=""
    local client="$REPO/scripts/linux-e2e-wayland-client.py" out status

    log "self-test: a window is shown, and the host's display is not reachable"
    status=0
    # shellcheck disable=SC2016  # expanded inside the sandbox
    out="$(WAYLAND_DISPLAY=host-decoy DISPLAY=:99 GOGO_DECOY=1 \
        "$SELF" run --name st-window --ready "" --timeout 60 -- \
        bash -c 'env >"$GUI_OUT/env.txt"; ls -A "$1" >"$GUI_OUT/repo.txt"; exec python3 -I "$0"' \
        "$client" "$REPO" | tail -n 1)" || status=$?
    [ "$status" = 0 ] || st_fail "the window run ended with $status"
    if [ -f "$out/env.txt" ]; then
        ! grep -qE 'host-decoy|^DISPLAY=|GOGO_DECOY' "$out/env.txt" ||
            st_fail "the host's environment reached the application"
    else
        st_fail "the application did not run"
    fi
    # The scripts, and the way to the output folder: nothing else.
    [ "$(cat "$out/repo.txt" 2>/dev/null)" = "scripts
target" ] || st_fail "more of the repository than scripts/ is visible"
    if [ -f "$out/frame.png" ]; then
        # The client draws white: most of the screen changed.
        "$TOOLS/bin/python3" -I "$HELPER" image "$out/frame.png" |
            "$TOOLS/bin/python3" -I -c 'import json,sys; sys.exit(json.load(sys.stdin)["nonblack_pct"] < 90)' ||
            st_fail "the screenshot does not show the window"
    else
        st_fail "no screenshot"
    fi

    log "self-test: an application that ends before showing fails the run"
    status=0
    "$SELF" run --name st-early --ready "" --timeout 60 -- true >/dev/null || status=$?
    [ "$status" = 1 ] || st_fail "an early exit gave $status, not 1"

    log "self-test: the time limit ends a run, with nothing left behind"
    status=0
    "$SELF" run --name st-timeout --timeout 6 -- sleep 600 >/dev/null || status=$?
    [ "$status" = 1 ] || st_fail "a timeout gave $status, not 1"

    log "self-test: a signal ends a run, with nothing left behind"
    # Background jobs ignore SIGINT unless told otherwise.
    env --default-signal=INT,TERM "$SELF" run --name st-signal --timeout 60 -- sleep 600 \
        >"$ROOT/st-signal.out" 2>"$ROOT/st-signal.err" &
    local run=$!
    for _ in $(seq 100); do
        [ -e "$ROOT/st-signal.$run/app.log" ] && break
        sleep 0.2
    done
    [ -e "$ROOT/st-signal.$run/app.log" ] || st_fail "the run never started"
    local start=$SECONDS
    kill -TERM "$run" 2>/dev/null || true
    status=0
    wait "$run" || status=$?
    [ "$status" = 1 ] || st_fail "a signalled run gave $status, not 1"
    [ $((SECONDS - start)) -lt 30 ] || st_fail "the signal did not end the run"
    ! grep -q "left behind" "$ROOT/st-signal.err" || st_fail "processes were left behind"
    rm -f "$ROOT/st-signal.out" "$ROOT/st-signal.err"

    log "self-test: a program the sandbox cannot see is refused"
    status=0
    "$SELF" run --name st-outside -- /nonexistent >/dev/null 2>&1 || status=$?
    [ "$status" = 2 ] || st_fail "a missing program gave $status, not 2"

    log "self-test: gpu without software Vulkan is refused"
    status=0
    VK_DRIVER_FILES="" "$SELF" run --renderer gpu -- true >/dev/null 2>&1 || status=$?
    [ "$status" = 2 ] || st_fail "--renderer gpu without lavapipe gave $status, not 2"

    [ -z "$ST_FAILED" ] || die "self-test: FAILED" 1
    log "self-test: all passed"
}

# ------------------------------------------------------------------------------

[ $# -ge 1 ] || usage
cmd="$1"
shift
case "$cmd" in
run | reload) cmd_run "$cmd" "$@" ;;
self-test) cmd_self_test "$@" ;;
__inside) inside "$@" ;;
-h | --help | help) usage ;;
*) log "unknown command: $cmd"; usage ;;
esac
