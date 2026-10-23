#!/usr/bin/env bash
# Host pass (#80): run one `gogo mac-app` bundle on the developer's own Mac and
# collect the evidence the VM harness can't give for Metal-3 renderers (DXMT,
# D3DMetal) and OpenGL. Run by a human: the game takes the screen, so agents
# never run this against a real bundle (AGENTS.md; they use scripts/vm-test.sh).
# How-to and the case list: docs/vm-testing.md, "Host pass".
#
#   scripts/host-pass.sh <X.app> [--secs N] [opts]
#
# Starts the bundle's launcher (Contents/MacOS/*) directly from this shell,
# not with `open`, so GOGO_HOME and the rest of the environment reach it.
# After --secs (or as soon as the game exits) it records the game's windows
# (CoreGraphics via JXA, as the VM harness does), stops the launcher with
# SIGTERM (its own teardown, #42), falls back to the prefix's `wineserver -k`
# if anything is left, then collects this run's log (#77), launch.log and the
# engine logs (Unity Player.log, OGRE ogre.log, other new *.log) from the
# game's state dir.
#
# options:
#   --secs N        how long the game runs before teardown (default 60);
#                   play during this time. Quitting the game early is fine.
#   --out DIR       results root (default <repo>/target/host-pass)
#   --kill-wait N   seconds the launcher gets to tear down after SIGTERM
#                   (default 20)
#   --dialogs       keep the launcher's #78 early-exit dialog (default off:
#                   GOGO_LAUNCHER_NO_DIALOG=1, so nothing waits for a click)
#   --screenshot    `screencapture -x` at the end (Terminal needs the Screen
#                   Recording permission; macOS asks once)
#   --dry-run       print the resolved paths and the command, launch nothing
#
# State dir: GOGO_HOME if set (also passed on to the launcher, so a stale
# config's `gogo mac-app setup` uses it), else the `GOGO_STATE` the bundle
# recorded at build, else ~/Library/Application Support/gogo. Logs:
# GOGO_LOG_DIR, else ~/Library/Logs/gogo/<gogid> (the launcher's rule).
#
# Output: the summary JSON on stdout (home paths shown as ~), then on the LAST
# line the path of summary.json. Exit codes: 0 = alive with ≥1 window at the
# end and a clean teardown; 1 = the app failed (exited, no window, teardown
# leftovers); 2 = usage; 3 = harness problem (says nothing about the app).
set -euo pipefail
# Plain bash 3.2 (what macOS ships): no mapfile, no associative arrays.

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if ! command -v jq >/dev/null 2>&1 && [ -z "${GOGO_HOSTPASS_IN_NIX:-}" ]; then
    GOGO_HOSTPASS_IN_NIX=1 exec nix develop "$REPO" -c "$0" "$@"
fi

log() { printf '[host-pass] %s\n' "$*" >&2; }
die() { log "error: $1"; exit "${2:-3}"; }

# On-screen windows of the given pids, and whether a permission or
# Gatekeeper prompt is up. CGWindowListCopyWindowInfo needs no TCC grant for
# owner names and pids (window titles would).
WINDOW_PROBE='
function run(argv) {
  ObjC.import("CoreGraphics");
  const pids = argv.map(Number);
  const raw = $.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly, 0);
  const ws = ObjC.deepUnwrap(ObjC.castRefToObject(raw)) || [];
  const game = ws.filter(w => pids.includes(w.kCGWindowOwnerPID))
    .map(w => ({owner: w.kCGWindowOwnerName, pid: w.kCGWindowOwnerPID, layer: w.kCGWindowLayer,
                w: Math.round(w.kCGWindowBounds.Width), h: Math.round(w.kCGWindowBounds.Height)}));
  const prompts = ws.filter(w => ["UserNotificationCenter", "CoreServicesUIAgent"].includes(w.kCGWindowOwnerName))
    .map(w => w.kCGWindowOwnerName);
  return JSON.stringify({windows: game, prompts: prompts});
}
'

# pid and all its descendants, one per line.
tree_pids() {
    ps -A -o pid=,ppid= | awk -v root="$1" '
        { kids[$2] = kids[$2] " " $1 }
        END {
            q[1] = root; n = 1
            for (i = 1; i <= n; i++) {
                print q[i]
                m = split(kids[q[i]], k, " ")
                for (j = 1; j <= m; j++) q[++n] = k[j]
            }
        }'
}
# The live (not zombie) pids of a list.
alive_pids() {
    local p s
    for p in "$@"; do
        s="$(ps -o stat= -p "$p" 2>/dev/null | tr -d ' ')" || continue
        case "$s" in "" | Z*) ;; *) echo "$p" ;; esac
    done
    return 0
}
# The wineserver serving this prefix: its cwd is
# /tmp/.wine-<uid>/server-<dev>-<ino> of the prefix (the launcher's rule).
prefix_server_pid() {
    [ -x /usr/sbin/lsof ] && [ "$(uname)" = Darwin ] && [ -d "$1" ] || return 0
    local di dir
    di="$(/usr/bin/stat -f '%d %i' "$1" 2>/dev/null)" || return 0
    # shellcheck disable=SC2086 # two numbers
    dir="$(printf '/.wine-%s/server-%x-%x' "$(id -u)" $di)"
    # lsof exits 1 when nothing matches.
    { /usr/sbin/lsof -a -d cwd -c wineserver -Fpn 2>/dev/null || true; } | awk -v d="$dir" '
        /^p/ { pid = substr($0, 2) }
        /^n/ { n = substr($0, 2); if (substr(n, length(n) - length(d) + 1) == d) print pid }'
}
lines_json() { if [ -s "$1" ]; then jq -R -s 'split("\n") | map(select(length>0))' "$1"; else echo '[]'; fi; }

app="" secs=60 out="$REPO/target/host-pass" kill_wait=20 dialogs=0 shot=0 dry=0
while [ $# -gt 0 ]; do
    case "$1" in
        --secs) secs="$2"; shift 2 ;;
        --out) out="$2"; shift 2 ;;
        --kill-wait) kill_wait="$2"; shift 2 ;;
        --dialogs) dialogs=1; shift ;;
        --screenshot) shot=1; shift ;;
        --dry-run) dry=1; shift ;;
        -h | --help) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
        -*) die "unknown option $1" 2 ;;
        *) [ -z "$app" ] || die "one bundle at a time" 2; app="$1"; shift ;;
    esac
done
[ -n "$app" ] && [ -d "$app" ] && [[ "$app" == *.app ]] || die "usage: host-pass.sh <X.app> [--secs N]" 2
case "$secs$kill_wait" in *[!0-9]*) die "--secs and --kill-wait take whole seconds" 2 ;; esac
app="$(cd "$app" && pwd)"
conf="$app/Contents/Resources/launch.conf"
[ -f "$conf" ] || die "$app has no Contents/Resources/launch.conf (not a gogo v2 bundle)" 2
launcher="$(find "$app/Contents/MacOS" -maxdepth 1 -type f -perm -u+x | head -1)"
[ -n "$launcher" ] || die "no launcher in $app/Contents/MacOS" 2

# What the bundle recorded, then what setup last applied for this game.
# shellcheck disable=SC1090
gogid="$(source "$conf"; printf '%s' "${GOGID:-}")"
# shellcheck disable=SC1090
recorded_state="$(source "$conf"; printf '%s' "${GOGO_STATE:-}")"
[ -n "$gogid" ] || die "no GOGID in $conf" 2
state="${GOGO_HOME:-$recorded_state}"
[ -d "$state" ] || state="$HOME/Library/Application Support/gogo"
game_state="$state/games/$gogid"
game_conf="$game_state/launch.conf"
log_dir="${GOGO_LOG_DIR:-$HOME/Library/Logs/gogo/$gogid}"
abs() { case "$1" in /*) printf '%s' "$1" ;; ?*) printf '%s/%s' "$state" "$1" ;; esac; }
renderer="" runtime="" runtime_dir="" verbs="" prefix="" wineserver=""
if [ -f "$game_conf" ]; then
    # shellcheck disable=SC1090
    # \x1f, not a tab: IFS whitespace would merge empty fields.
    IFS=$'\x1f' read -r renderer runtime runtime_dir verbs prefix wineserver < <(
        source "$game_conf"
        # shellcheck disable=SC2153 # from launch.conf
        printf '%s\x1f%s\x1f%s\x1f%s\x1f%s\x1f%s\n' "${RENDERER:-}" "${RUNTIME:-}" "${RUNTIME_DIR:-}" \
            "${VERBS[*]+${VERBS[*]}}" "${PREFIX:-}" "${WINESERVER:-}") || true
    prefix="$(abs "$prefix")"; wineserver="$(abs "$wineserver")"
else
    log "no $game_conf yet: the launcher's setup will create it (or refuse to launch)"
fi

slug="$(basename "$app" .app | tr -cs 'A-Za-z0-9' '-' | tr '[:upper:]' '[:lower:]' | sed 's/^-//;s/-$//' | cut -c1-24)"
ts="$(date +%Y%m%d-%H%M%S)"
res="$out/$slug-$ts"

if [ "$dry" = 1 ]; then
    jq -n --arg app "$app" --arg launcher "$launcher" --arg gogid "$gogid" --arg state "$state" \
        --arg log_dir "$log_dir" --arg renderer "$renderer" --arg runtime_dir "$runtime_dir" \
        --arg prefix "$prefix" --arg wineserver "$wineserver" --arg res "$res" \
        --argjson secs "$secs" --argjson dialogs "$([ "$dialogs" = 1 ] && echo true || echo false)" \
        '{dry_run:true, app:$app, launcher:$launcher, gogid:$gogid, state:$state, log_dir:$log_dir,
          renderer:$renderer, runtime_dir:$runtime_dir, prefix:$prefix, wineserver:$wineserver,
          secs:$secs, dialogs:$dialogs, results_dir:$res}'
    exit 0
fi

mkdir -p "$res/engine-logs" "$res/launcher-logs"
touch "$res/.t0"
status="infra_error" exit_code=3 note="" lpid="" launched=false alive=false exited_after=null
windows="[]" prompts="[]" teardown="null" run_log="" run_exit=""
game_pids=()

# shellcheck disable=SC2329 # invoked by the EXIT trap
finish() {
    trap - EXIT INT TERM
    # Never leave a game behind, whatever went wrong above.
    if [ -n "$lpid" ] && [ -n "$(alive_pids "$lpid")" ]; then
        kill -TERM "$lpid" 2>/dev/null || true
        for _ in $(seq "$kill_wait"); do [ -n "$(alive_pids "$lpid")" ] || break; sleep 1; done
    fi
    collect || true
    local procs="[]"
    [ -s "$res/processes.txt" ] && procs="$(jq -R -s 'split("\n") | map(select(length>0) | sub("^ +"; ""))' "$res/processes.txt")"
    jq -n --arg status "$status" --argjson exit "$exit_code" --arg app "$app" \
          --arg gogid "$gogid" --arg runtime "$runtime" --arg renderer "$renderer" \
          --arg runtime_dir "$runtime_dir" --arg verbs "$verbs" --arg host_state "$state" \
          --argjson secs "$secs" --argjson launched "$launched" --argjson running "$alive" \
          --argjson exited_after "$exited_after" --arg run_exit "$run_exit" \
          --argjson windows "$windows" --argjson prompts "$prompts" --argjson processes "$procs" \
          --argjson teardown "$teardown" \
          --argjson missing "$(lines_json "$res/missing-libs.txt")" \
          --argjson errs "$(lines_json "$res/err-lines.txt")" \
          --argjson markers "$(markers_json)" \
          --argjson engine "$(cd "$res/engine-logs" && find . -type f | sed 's|^\./||' | LC_ALL=C sort | jq -R -s 'split("\n") | map(select(length>0))')" \
          --arg host_macos "$(sw_vers -productVersion 2>/dev/null || uname -sr)" \
          --arg host_chip "$(sysctl -n machdep.cpu.brand_string 2>/dev/null || uname -m)" \
          --arg run_log "$([ -n "$run_log" ] && echo "$res/launcher-logs/$run_log")" \
          --arg screenshot "$([ -s "$res/screenshot.png" ] && echo "$res/screenshot.png")" \
          --arg note "$note" --arg dir "$res" --arg home "$HOME" \
          '{status:$status, exit_code:$exit, app:$app, model:"v2", gogid:$gogid, runtime:$runtime,
            renderer:$renderer, runtime_dir:$runtime_dir, verbs:$verbs,
            state:{host:$host_state}, host:{macos:$host_macos, chip:$host_chip},
            secs:$secs, launched:$launched, running_after_secs:$running,
            exited_after_secs:$exited_after, run_exit:$run_exit,
            windows:$windows, prompts_left:$prompts, processes:$processes,
            teardown:$teardown, markers:$markers, err_lines:$errs, missing_libs:$missing,
            engine_logs:$engine, note:$note, results_dir:$dir, screenshot:$screenshot,
            files:{stdout:($dir+"/launcher-stdout.log"), stderr:($dir+"/launcher-stderr.log"),
                   run_log:$run_log, launch_log:($dir+"/launcher-logs/launch.log"),
                   engine_logs:($dir+"/engine-logs"), processes:($dir+"/processes.txt")}}
           | walk(if type == "string" then (split($home) | join("~")) else . end)' >"$res/summary.json" \
        || jq -n --arg app "$app" --arg dir "$res" \
            '{status:"infra_error", exit_code:3, app:$app, results_dir:$dir,
              note:"summary assembly failed (harness bug)"}' >"$res/summary.json"
    cat "$res/summary.json"
    echo "$res/summary.json"
    exit "$exit_code"
}
trap finish EXIT
trap 'note="interrupted"; exit 3' INT TERM

# Logs, after the run: this launch's run log (<ts>-<launcher pid>.log, else
# the newest), launch.log, and engine logs from the game's state dir.
collect() {
    [ -z "${collected:-}" ] || return 0
    collected=1
    if [ -d "$log_dir" ]; then
        [ -f "$log_dir/launch.log" ] && cp "$log_dir/launch.log" "$res/launcher-logs/"
        if [ -n "$lpid" ]; then
            run_log="$(cd "$log_dir" && find . -maxdepth 1 -type f -name "[0-9]*-$lpid.log" | sed 's|^\./||' | LC_ALL=C sort | tail -1)"
        fi
        [ -n "$run_log" ] || run_log="$(cd "$log_dir" && find . -maxdepth 1 -type f -name '[0-9]*-*.log' | sed 's|^\./||' | LC_ALL=C sort | tail -1)"
        if [ -n "$run_log" ]; then
            cp "$log_dir/$run_log" "$res/launcher-logs/"
            run_exit="$(sed -n 's/^# exit: //p' "$res/launcher-logs/$run_log" | tail -1)"
        fi
    fi
    if [ -d "$game_state" ]; then
        # Unity and OGRE logs wherever the game keeps them (Unity: the
        # prefix's AppData/LocalLow; OGRE: next to the exe or in Documents),
        # plus any other small *.log written during this run.
        (cd "$game_state" && find . \( -path ./.launchers -prune \) -o -type f -size -20M \
            \( -name 'Player.log' -o -name 'Player-prev.log' -o -iname 'ogre.log' \
               -o \( -iname '*.log' -newer "$res/.t0" \) \) -print) \
            | sed 's|^\./||' >"$res/engine-logs.txt" || true
        while IFS= read -r f; do
            mkdir -p "$res/engine-logs/$(dirname "$f")"
            cp "$game_state/$f" "$res/engine-logs/$f" 2>/dev/null || true
        done <"$res/engine-logs.txt"
    fi
    local all=("$res/launcher-stdout.log" "$res/launcher-stderr.log")
    [ -n "$run_log" ] && all+=("$res/launcher-logs/$run_log")
    cat "${all[@]}" 2>/dev/null | grep -E -i 'cannot find|Library not loaded|image not found' \
        | LC_ALL=C sort -u | head -20 >"$res/missing-libs.txt" || true
    cat "${all[@]}" 2>/dev/null | grep -E '(^|:)err:' | LC_ALL=C sort | uniq -c | sort -rn \
        | head -20 | sed 's/^ *//' >"$res/err-lines.txt" || true
}

# What a reviewer of the pass looks for, per renderer (docs/vm-testing.md):
# name → regex over the run log, then over the engine logs.
MARKERS_WINE=(
    'feature_level|D3D_FEATURE_LEVEL_'
    'metal_layer_failed|Failed to get metal layer'
    'class_linkage|Class linkage not supported'
    'wined3d|wined3d'
    'd3dmetal|D3DMetal|libd3dshared'
    'opengl|OpenGL (version|2\.0)|GL_VERSION|GL_RENDERER'
)
MARKERS_ENGINE=(
    'unity|^[[:space:]]*(Direct3D|Renderer:|Vendor:|VRAM:|Version:)|Failed to initialize player|could not load'
    'ogre|RenderSystem|Direct3D11|D3D11 *:|Feature level|OGRE EXCEPTION|Exception'
)
# shellcheck disable=SC2329 # called from finish
markers_json() {
    local m acc='{}' rl=()
    [ -n "$run_log" ] && rl=("$res/launcher-logs/$run_log")
    for m in "${MARKERS_WINE[@]}"; do
        acc="$(marker "$acc" "$m" "$res/launcher-stdout.log" "$res/launcher-stderr.log" ${rl[@]+"${rl[@]}"})"
    done
    for m in "${MARKERS_ENGINE[@]}"; do acc="$(marker "$acc" "$m" "$res/engine-logs")"; done
    printf '%s' "$acc"
}
# marker <json so far> <name|regex> <files or dirs…>: the top matching lines,
# with counts, added under <name>.
# shellcheck disable=SC2329
marker() {
    local acc="$1" name="${2%%|*}" re="${2#*|}"
    shift 2
    { grep -r -h -E -- "$re" "$@" 2>/dev/null || true; } | sed 's/^[[:space:]]*//' | cut -c1-200 \
        | LC_ALL=C sort | uniq -c | sort -rn | head -8 | sed 's/^ *//' \
        | jq -R -s --argjson acc "$acc" --arg n "$name" '$acc + {($n): split("\n") | map(select(length>0))}'
}

log "launching $(basename "$launcher") ($gogid) for ${secs}s; renderer ${renderer:-?}; results in $res"
log "the game takes the screen: play it during the wait; Ctrl-C stops early and still tears down"
export GOGO_LOG_DIR="$log_dir"
if [ -n "${GOGO_HOME:-}" ]; then export GOGO_HOME; fi
if [ "$dialogs" = 0 ]; then export GOGO_LAUNCHER_NO_DIALOG=1; fi
"$launcher" </dev/null >"$res/launcher-stdout.log" 2>"$res/launcher-stderr.log" &
lpid=$!
launched=true
t0=$SECONDS
while [ $((SECONDS - t0)) -lt "$secs" ]; do
    if [ -z "$(alive_pids "$lpid")" ]; then
        exited_after=$((SECONDS - t0))
        log "the launcher exited after ${exited_after}s"
        break
    fi
    # Remember the game's process tree while it's up: after a SIGTERM the
    # leftovers check needs it, and the launcher may be gone by then.
    while IFS= read -r p; do game_pids+=("$p"); done < <(tree_pids "$lpid")
    sleep 1
done
# shellcheck disable=SC2207 # pids: no spaces or globs
game_pids=($(printf '%s\n' ${game_pids[@]+"${game_pids[@]}"} | LC_ALL=C sort -un))
# shellcheck disable=SC2207
live=($(alive_pids ${game_pids[@]+"${game_pids[@]}"} "$lpid"))
[ "${#live[@]}" -gt 0 ] && [ "$exited_after" = null ] && alive=true
if [ "${#live[@]}" -gt 0 ]; then
    ps -o pid=,stat=,command= -p "$(IFS=,; echo "${live[*]}")" >"$res/processes.txt" 2>/dev/null || true
fi

if command -v osascript >/dev/null 2>&1 && [ "${#live[@]}" -gt 0 ]; then
    probe="$(osascript -l JavaScript -e "$WINDOW_PROBE" "${live[@]}" 2>"$res/window-probe.err" || true)"
    if jq -e . >/dev/null 2>&1 <<<"$probe"; then
        windows="$(jq -c .windows <<<"$probe")"; prompts="$(jq -c .prompts <<<"$probe")"
    fi
fi
if [ "$shot" = 1 ]; then screencapture -x "$res/screenshot.png" 2>/dev/null || true; fi

# Teardown (#42): the launcher's own SIGTERM path first; whatever of the game
# is left after --kill-wait goes to the prefix's `wineserver -k`.
if [ "$alive" = true ] || [ -n "$(alive_pids "$lpid")" ]; then
    log "SIGTERM → launcher $lpid, then up to ${kill_wait}s"
    kill -TERM "$lpid" 2>/dev/null || true
    for _ in $(seq "$kill_wait"); do [ -n "$(alive_pids "$lpid")" ] || break; sleep 1; done
fi
# Reap it, unless it ignored SIGTERM (then the fallback below deals with it).
if [ -z "$(alive_pids "$lpid")" ]; then wait "$lpid" 2>/dev/null || true; fi
# shellcheck disable=SC2207
left=($( { alive_pids ${game_pids[@]+"${game_pids[@]}"} "$lpid"; prefix_server_pid "$prefix"; } | LC_ALL=C sort -un))
fallback=false final=()
if [ "${#left[@]}" -gt 0 ]; then
    ps -o pid=,stat=,command= -p "$(IFS=,; echo "${left[*]}")" >"$res/processes-after-term.txt" 2>/dev/null || true
    fallback=true
    if [ -x "$wineserver" ] && [ -d "$prefix" ]; then
        log "left after SIGTERM: ${left[*]}; WINEPREFIX=<prefix> wineserver -k"
        WINEPREFIX="$prefix" "$wineserver" -k >/dev/null 2>&1 || true
        sleep 3
    fi
    # shellcheck disable=SC2207
    final=($(alive_pids "${left[@]}"))
    if [ "${#final[@]}" -gt 0 ]; then
        log "still left: ${final[*]}; SIGKILL"
        kill -KILL "${final[@]}" 2>/dev/null || true
    fi
fi
teardown="$(jq -n --arg pid "$lpid" --argjson wait "$kill_wait" --argjson fb "$fallback" \
    --argjson left "$(lines_json "$res/processes-after-term.txt" | jq 'map(sub("^ +"; ""))')" \
    --argjson final "$(printf '%s\n' ${final[@]+"${final[@]}"} | jq -R -s 'split("\n") | map(select(length>0))')" \
    '{signal:"TERM", launcher_pid:$pid, wait_secs:$wait, leftovers:$left,
      wineserver_k_fallback:$fb, killed_after_fallback:$final, ok:(($left | length) == 0)}')"

collect
nwin="$(jq length <<<"$windows")"
if [ "$alive" = true ] && [ "$(jq .ok <<<"$teardown")" != true ]; then status="teardown_leftovers"; exit_code=1
elif [ "$alive" = true ] && [ "$nwin" -gt 0 ]; then status="ok"; exit_code=0
elif [ "$alive" = true ]; then status="running_no_window"; exit_code=1
else status="exited"; exit_code=1; fi
exit "$exit_code"
