#!/usr/bin/env bash
# Headless sway end-to-end harness for `gogo linux-app` games (#201, how-to:
# docs/linux-e2e.md): builds a game in a scratch home, runs it through its
# generated launch.sh in a headless compositor inside a bwrap root, drives the
# manifest's steps, checks the frames, then tears everything down.
#
#   scripts/linux-e2e.sh run <gogid> [opts]   one game, from e2e/<gogid>.toml
#   scripts/linux-e2e.sh self-test            the harness itself: a plain xterm,
#                                             teardown on TERM, INT, KILL, timeout,
#                                             the build's display, the null audio sink
#   scripts/linux-e2e.sh cleanup              remove scratch of dead runs
#
# run options:
#   --manifest FILE   manifest (default e2e/<gogid>.toml)
#   --gpu NODE[,NODE] the GPUs the sandbox has, as renderD12x or /dev/dri/renderD12x:
#                     comma-joined or repeated, and sway renders on the first
#                     (default: software: pixman, llvmpipe, lavapipe, and no GPU
#                     node in the sandbox)
#   --expect-gpu NODE with several GPUs, the one the game must render on
#                     (default: any of them, and the summary says which)
#   --secs N          seconds the game phase may take (default 600); over it the
#                     run is stopped and fails
#   --gogo PATH       this gogo (default: `nix build .#gogo-linux-app`)
#   --keep            keep the scratch dir, for a look around
#   --no-cache        download Proton, umu's runtime and the game afresh, into
#                     scratch (default: target/linux-e2e/cache, shared by runs)
#   --out DIR         results root (default target/linux-e2e/results)
#
# Output: progress on stderr; the summary JSON on stdout, then on the LAST line
# the path of summary.json. Exit codes: 0 = every check passed; 1 = a check
# failed; 2 = usage; 3 = harness problem (says nothing about the game).
#
# Agents launch real games only through this script (AGENTS.md).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SELF="$REPO/scripts/linux-e2e.sh"
HELPER_SRC="$REPO/scripts/linux-e2e-helper.py"
ROOT="$REPO/target/linux-e2e"
GRACE_SECS=45 # how long a stopped run gets for the launcher's own teardown
# Inside the sandbox's own /run: a socket path under target/ can pass the
# kernel's 108 bytes.
RUNTIME_DIR=/run/e2e-runtime

log() { printf '[linux-e2e] %s\n' "$*" >&2; }
die() { log "error: $1"; exit "${2:-3}"; }
usage() { sed -n '2,/^# Agents launch/p' "$SELF" | sed 's/^# \{0,1\}//' >&2; exit 2; }

resolve_tools() {
    # self-test's own runs: already resolved.
    if [ -n "${GOGO_E2E_TOOLS:-}" ]; then TOOLS="$GOGO_E2E_TOOLS"; return 0; fi
    command -v nix >/dev/null || die "nix not found"
    TOOLS="$(nix build --no-link --print-out-paths "$REPO#linux-e2e-tools")" ||
        die "couldn't build .#linux-e2e-tools"
}
# The rest of a run sees the pinned tools and nothing else.
use_tools() { export PATH="$TOOLS/bin"; }
helper() { python3 -I "$HELPER_SRC" "$@"; }

# --- the sandbox --------------------------------------------------------------

# A whitelist root: nothing of the host's /home, /mnt, /media or /run but what
# is named here. Everything in it shares one PID namespace under a bwrap init
# that dies with this script, so the kernel kills whatever is left (Wine,
# pressure-vessel's nested namespaces, Xwayland) when the run ends.
sandbox_args() { # mode: build | game
    local mode="$1" d
    SB=(--unshare-user --unshare-pid --unshare-ipc --unshare-uts --unshare-cgroup-try
        --die-with-parent --new-session
        --ro-bind /nix /nix --ro-bind /etc /etc --ro-bind /sys /sys
        --proc /proc --dev /dev --tmpfs /tmp --tmpfs /var/tmp --tmpfs /run
        --perms 0700 --dir "$RUNTIME_DIR")
    # An agent's shell is itself an FHS bwrap, with /etc full of links into
    # /.host-etc.
    SB+=(--ro-bind-try /.host-etc /.host-etc)
    if [ -e /etc/NIXOS ] || [ -e /.host-etc/NIXOS ]; then
        # /usr/bin on NixOS can be envfs, a FUSE mount: only what launch.sh's
        # shebang and system() need.
        SB+=(--dir /usr/bin --symlink "$TOOLS/bin/env" /usr/bin/env
             --dir /bin --symlink "$TOOLS/bin/sh" /bin/sh)
    else
        SB+=(--ro-bind /usr /usr)
        for d in bin sbin lib lib32 lib64; do SB+=(--ro-bind-try "/$d" "/$d"); done
    fi
    # Xwayland admits clients by local user, so the uid must resolve here, the
    # same way on every host.
    SB+=(--ro-bind "$RUN/etc/passwd" "$(readlink -f /etc/passwd)"
         --ro-bind "$RUN/etc/group" "$(readlink -f /etc/group)")
    for d in /run/opengl-driver /run/opengl-driver-32 /run/current-system; do
        SB+=(--ro-bind-try "$d" "$d")
    done
    SB+=(--bind "$RUN" "$RUN" --bind "$RESULTS" "$RESULTS"
         --bind "$CACHE/store" "$RUN/store"
         --bind "$CACHE/depot" "$RUN/xdg/cache/gogo/depot"
         --bind "$CACHE/umu" "$RUN/xdg/data/umu")
    case "$GOGO" in /nix/store/*) ;; ?*) SB+=(--ro-bind "$GOGO" "$GOGO") ;; esac
    if [ "$mode" = build ]; then
        # Build only: the network, and the session bus for the keyring login.
        SB+=(--share-net)
        for d in /run/nscd /run/systemd/resolve; do SB+=(--ro-bind-try "$d" "$d"); done
        if [ -S "/run/user/$(id -u)/bus" ]; then
            SB+=(--bind "/run/user/$(id -u)/bus" "$RUNTIME_DIR/bus")
        fi
    else
        SB+=(--unshare-net)
        for d in "${GPU_BINDS[@]+"${GPU_BINDS[@]}"}"; do SB+=(--dev-bind "$d" "$d"); done
    fi
}

sandbox_env() {
    SE=(HOME="$RUN/home" USER="$(id -un)" LOGNAME="$(id -un)" PATH="$TOOLS/bin"
        LANG=C.UTF-8 GOGO_HOME="$GOGO_HOME_DIR" GOGO_E2E_RUN="$RUN_ID"
        XDG_DATA_HOME="$RUN/xdg/data" XDG_CONFIG_HOME="$RUN/xdg/config"
        XDG_CACHE_HOME="$RUN/xdg/cache" XDG_STATE_HOME="$RUN/xdg/state"
        XDG_RUNTIME_DIR="$RUNTIME_DIR")
    [ -z "${UMU_PY:-}" ] || SE+=(GOGO_UMU_PYTHON="$UMU_PY")
}

# Start a sandbox in the background, stdout and stderr to a log; supervise()
# then watches it.
start_sandbox() { # mode logfile command…
    local mode="$1" logf="$2"; shift 2
    sandbox_args "$mode"
    sandbox_env
    if [ "$mode" = build ]; then
        SE+=(DBUS_SESSION_BUS_ADDRESS="unix:path=$RUNTIME_DIR/bus" "${SOFTWARE_COMPOSITOR_ENV[@]}")
    else
        SE+=("${COMPOSITOR_ENV[@]}")
    fi
    env -i "${SE[@]}" "$TOOLS/bin/bwrap" "${SB[@]}" "$@" >"$logf" 2>&1 9>&- </dev/null &
    SANDBOX_PID=$!
}

# Wait for the sandbox. Past the deadline, or on a signal, ask the driver to
# wrap up (the stop file), and after GRACE_SECS kill bwrap: its init goes, and
# the PID namespace with it. Returns the sandbox's exit status.
supervise() { # deadline-epoch (0 = none)
    local deadline="$1" stop_at=0 now
    while kill -0 "$SANDBOX_PID" 2>/dev/null; do
        now="$(date +%s)"
        if [ "$stop_at" = 0 ] && { [ -n "$INTERRUPTED" ] || { [ "$deadline" != 0 ] && [ "$now" -ge "$deadline" ]; }; }; then
            [ -n "$INTERRUPTED" ] || { TIMED_OUT=1; log "over the time limit: stopping"; }
            : >"$RUN/stop"
            stop_at=$((now + GRACE_SECS))
            [ "$MODE_NOW" = game ] || stop_at="$now"
        fi
        if [ "$stop_at" != 0 ] && { [ "$now" -ge "$stop_at" ] || [ "$INTERRUPT_COUNT" -ge 2 ]; }; then
            log "killing the sandbox"
            kill -KILL "$SANDBOX_PID" 2>/dev/null || true
        fi
        sleep 0.2
    done
    local rc=0
    wait "$SANDBOX_PID" || rc=$?
    SANDBOX_PID=""
    return "$rc"
}

# --- teardown -----------------------------------------------------------------

# Processes of this run still alive: the launcher's prefix markers
# (launcher.rs PREFIX_MARKS), the run's env marker, the scratch path on the
# command line, or the sandbox's PID namespace. Prints "pid comm" lines.
leftovers() {
    local p pid comm hit mark marks=() prefix ns="${RUN_PIDNS:-}"
    marks+=("GOGO_E2E_RUN=$RUN_ID")
    if [ -n "${PREFIX:-}" ]; then
        for prefix in "$PREFIX" "$(readlink -f "$PREFIX" 2>/dev/null || true)"; do
            [ -n "$prefix" ] || continue
            marks+=("WINEPREFIX=$prefix" "WINEPREFIX=$prefix/" "WINEPREFIX=$prefix/pfx" "WINEPREFIX=$prefix/pfx/"
                    "STEAM_COMPAT_DATA_PATH=$prefix" "STEAM_COMPAT_DATA_PATH=$prefix/")
        done
    fi
    for p in /proc/[0-9]*; do
        pid="${p#/proc/}"
        [ "$pid" != "$$" ] && [ "$pid" != "$BASHPID" ] || continue
        hit=""
        for mark in "${marks[@]}"; do
            if grep -qzxF -e "$mark" "$p/environ" 2>/dev/null; then hit=1; break; fi
        done
        if [ -z "$hit" ] && [ -n "$ns" ] && [ "$(readlink "$p/ns/pid" 2>/dev/null || true)" = "$ns" ]; then hit=1; fi
        if [ -z "$hit" ] && grep -qzF -e "$RUN/" "$p/cmdline" 2>/dev/null; then hit=1; fi
        [ -n "$hit" ] || continue
        comm="$(cat "$p/comm" 2>/dev/null || echo '?')"
        printf '%s %s\n' "$pid" "$comm"
    done
}

# Every exit path: kill the sandbox, sweep, assert nothing is left, write the
# summary, delete scratch. Idempotent; the exit code comes from here.
teardown() {
    local status=$?
    trap - EXIT INT TERM HUP
    [ -z "${TORN_DOWN:-}" ] || exit "$status"
    TORN_DOWN=1
    [ -n "${RUN:-}" ] || exit "$status"
    # An exit nobody planned (set -e, die): the run proved nothing.
    if [ "$status" != 0 ] && [ -z "${BUILD_FAILED:-}" ]; then
        INFRA_ERROR="${INFRA_ERROR:-the script stopped with exit $status; see stderr}"
    fi
    if [ -n "${SANDBOX_PID:-}" ]; then
        kill -KILL "$SANDBOX_PID" 2>/dev/null || true
        wait "$SANDBOX_PID" 2>/dev/null || true
    fi
    collect_logs
    local left="" tries=10 pid killed=""
    while [ "$tries" -gt 0 ]; do
        tries=$((tries - 1))
        left="$(leftovers)"
        [ -n "$left" ] || break
        sleep 0.5
    done
    if [ -n "$left" ]; then
        log "left behind: $(printf '%s' "$left" | tr '\n' ';')"
        killed="$left"
        while read -r pid _; do kill -KILL "$pid" 2>/dev/null || true; done <<<"$left"
        sleep 1
        left="$(leftovers)"
    fi
    local deleted=false
    if [ "$KEEP" = 1 ]; then
        log "kept scratch: $RUN"
    elif delete_scratch; then
        deleted=true
    fi
    write_summary "$status" "$killed" "$left" "$deleted"
    exit "$FINAL_EXIT"
}

on_signal() {
    INTERRUPTED="$1"
    INTERRUPT_COUNT=$((INTERRUPT_COUNT + 1))
    log "got SIG$1: tearing down"
}

delete_scratch() {
    case "$RUN" in "$ROOT"/runs/*) ;; *) log "not deleting $RUN"; return 1 ;; esac
    [ -d "$RUN" ] || return 0
    # Proton and umu leave read-only dirs; bind-mount targets are just empty
    # dirs out here, so the cache isn't touched.
    chmod -R u+rwX "$RUN" 2>/dev/null || true
    if ! rm -rf "$RUN"; then log "couldn't delete $RUN"; return 1; fi
}

collect_logs() {
    [ -n "${GOGO_HOME_DIR:-}" ] && [ -d "$RESULTS" ] || return 0
    local d="$GOGO_HOME_DIR/logs/${GOGID:-none}" newest=""
    if [ -d "$d" ]; then
        newest="$(find "$d" -maxdepth 1 -type f -name '*.log' ! -name launch.log -printf '%T@ %p\n' 2>/dev/null |
            sort -n | tail -1 | cut -d' ' -f2- || true)"
        [ -z "$newest" ] || cp "$newest" "$RESULTS/run.log" 2>/dev/null || true
        cp "$d/launch.log" "$RESULTS/launch.log" 2>/dev/null || true
    fi
    if [ -n "${GOGID:-}" ] && [ -f "$GOGO_HOME_DIR/games/$GOGID/state.toml" ]; then
        STATE_RUNTIME="$(sed -n 's/^runtime *= *"\(.*\)"/\1/p' "$GOGO_HOME_DIR/games/$GOGID/state.toml" | head -1)"
    fi
}

# A tail of the game's log, with the home dir as ~.
log_excerpt() {
    local f="$RESULTS/run.log"
    [ -s "$f" ] || f="$RESULTS/game.out"
    [ -s "$f" ] || return 0
    tail -n 40 "$f" | cut -c1-300 | sed "s|$HOME|~|g"
}

write_summary() { # entry-status killed left deleted
    local status="$1" killed="$2" left="$3" deleted="$4" result="$RESULTS/result.json" outer
    [ -s "$result" ] || printf '{"checks":[],"steps":[],"isolation":{},"gpu":{}}\n' >"$result"
    local build_result="$RESULTS/build-result.json"
    [ -s "$build_result" ] || printf '{}\n' >"$build_result"
    outer="$(jq -n \
        --arg killed "$killed" --arg left "$left" --arg build "${BUILD_FAILED:-}" \
        --arg timeout "${TIMED_OUT:-}" --arg sig "$INTERRUPTED" --arg keep "$KEEP" \
        --argjson deleted "$deleted" --arg infra "${INFRA_ERROR:-}" '
        [ (if $infra != "" then {name:"harness",ok:false,detail:$infra} else empty end),
          (if $build != "" then {name:"build",ok:false,detail:$build} else empty end),
          (if $timeout != "" then {name:"time_limit",ok:false,detail:"over --secs"} else empty end),
          (if $sig != "" then {name:"interrupted",ok:false,detail:("SIG"+$sig)} else empty end),
          {name:"nothing_left",ok:($killed == "" and $left == ""),
           detail:(if $killed == "" then "no process of the run left" else "swept: "+($killed|gsub("\n";"; ")) end)},
          {name:"scratch_deleted",ok:($deleted or $keep == "1"),
           detail:(if $keep == "1" then "kept (--keep)" elif $deleted then "deleted" else "still there" end)} ]')"
    local summary="$RESULTS/summary.json" excerpt
    excerpt="$(log_excerpt || true)"
    jq -n --slurpfile r "$result" --slurpfile b "$build_result" --argjson outer "$outer" \
        --arg gogid "${GOGID:-}" --arg title "${TITLE:-}" --arg gpu "${GPU_LABEL:-software}" \
        --arg results "$RESULTS" --arg scratch "$RUN" --argjson deleted "$deleted" \
        --arg keep "$KEEP" --arg left "$left" --arg killed "$killed" --arg excerpt "$excerpt" \
        --arg runtime "${STATE_RUNTIME:-}" --arg date "$(date -u +%F)" '
        ($r[0]) as $res
        | ($b[0]) as $bld
        | (($bld.checks // []) | map(.name = "build_" + .name)) as $build_checks
        | ($build_checks + ($res.checks // []) + $outer) as $seen
        # The driver reports "finished" last: without it, nothing says the steps ran.
        | ($seen + (if any($seen[]; .name == "finished") then []
                    else [{name:"finished",ok:false,detail:"the driver did not reach its end"}] end)) as $checks
        | ([$checks[] | select(.ok | not)] | length == 0) as $ok
        | {ok:$ok, gogid:$gogid, title:$title, gpu:$gpu,
           failed:[$checks[] | select(.ok | not) | .name],
           checks:$checks,
           screenshots:[($res.steps // [])[] | select(.action == "screenshot" and .path) | .path],
           steps:($res.steps // []),
           isolation:($res.isolation // {}), gpu_detail:($res.gpu // {}),
           audio:($res.audio // {}),
           build:{exit:($bld.exit // null), secs:($bld.secs // null), isolation:($bld.isolation // {}),
                  windows:($bld.windows // []), support_installer:($bld.support_installer // null)},
           game_exit:($res.game_exit // null),
           doctor:($res.doctor // null),
           log_excerpt:$excerpt,
           cleanup:{left:$left, swept:$killed, scratch:$scratch, scratch_deleted:$deleted, kept:($keep == "1")},
           tested_entry:(if $ok and $gogid != "self-test" and $runtime != ""
                         then {runtime:$runtime, host:("headless-sway/"+$gpu), date:$date}
                         else null end),
           results:$results}' >"$summary"
    # Only a harness problem with nothing tested is 3; every other failure is 1.
    if jq -e .ok "$summary" >/dev/null; then FINAL_EXIT=0
    elif [ -n "${INFRA_ERROR:-}" ]; then FINAL_EXIT=3
    else FINAL_EXIT=1; fi
    jq . "$summary"
    printf '%s\n' "$summary"
}

# --- GPU ----------------------------------------------------------------------

icd_files() { # file-name glob for each: prints a colon-joined list of ICD manifests
    local out="" dir pat f
    for dir in /run/opengl-driver/share/vulkan/icd.d /run/opengl-driver-32/share/vulkan/icd.d \
               /usr/share/vulkan/icd.d /etc/vulkan/icd.d; do
        for pat in "$@"; do
            for f in "$dir"/$pat; do
                [ -e "$f" ] && out="${out:+$out:}$f"
            done
        done
    done
    printf '%s' "$out"
}

# Software rendering: the default game phase, and every build (its sandbox has
# no GPU node).
software_setup() {
    local icd
    SOFTWARE_COMPOSITOR_ENV=(WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1
                             WLR_RENDERER=pixman WLR_RENDERER_ALLOW_SOFTWARE=1)
    SOFTWARE_ENV=(GOGO_LAUNCHER_NO_DIALOG=1 LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe
                  __GLX_VENDOR_LIBRARY_NAME=mesa)
    icd="$(icd_files 'lvp_icd*.json')"
    [ -z "$icd" ] || SOFTWARE_ENV+=("VK_DRIVER_FILES=$icd")
}

# What one render node needs. Sets N_LABEL, N_EXPECT, N_HELD (the node a process
# of that GPU holds), N_BINDS, N_COMPOSITOR_ENV, N_GAME_ENV and N_ICD.
gpu_node() { # /dev/dri/renderD12x
    local node="$1" driver="" d
    case "$node" in /dev/dri/renderD[0-9]*) ;; *) die "--gpu wants render nodes, renderD12x: $node" 2 ;; esac
    [ -c "$node" ] || die "--gpu: no such device: $node" 2
    driver="$(basename "$(readlink -f "/sys/class/drm/$(basename "$node")/device/driver")")"
    N_BINDS=("$node")
    N_HELD="$node"
    N_COMPOSITOR_ENV=(WLR_RENDERER=gles2 "WLR_RENDER_DRM_DEVICE=$node")
    case "$driver" in
    nvidia)
        N_LABEL=nvidia
        N_EXPECT='nvidia|geforce|quadro|rtx|gtx'
        for d in /dev/nvidia0 /dev/nvidiactl /dev/nvidia-modeset /dev/nvidia-uvm /dev/nvidia-uvm-tools /dev/nvidia-caps; do
            [ -e "$d" ] && N_BINDS+=("$d")
        done
        N_HELD=/dev/nvidia0
        N_COMPOSITOR_ENV+=(GBM_BACKEND=nvidia-drm __GLX_VENDOR_LIBRARY_NAME=nvidia
                           GBM_BACKENDS_PATH=/run/opengl-driver/lib/gbm
                           __EGL_VENDOR_LIBRARY_FILENAMES=/run/opengl-driver/share/glvnd/egl_vendor.d/10_nvidia.json
                           __EGL_EXTERNAL_PLATFORM_CONFIG_DIRS=/run/opengl-driver/share/egl/egl_external_platform.d)
        N_GAME_ENV=(__GLX_VENDOR_LIBRARY_NAME=nvidia __NV_PRIME_RENDER_OFFLOAD=1)
        N_ICD="$(icd_files 'nvidia_icd*.json')"
        ;;
    amdgpu | radeon)
        N_LABEL=radv
        N_EXPECT='amd|radv|radeon'
        N_GAME_ENV=(__GLX_VENDOR_LIBRARY_NAME=mesa)
        N_ICD="$(icd_files 'radeon_icd*.json')"
        ;;
    i915 | xe)
        N_LABEL=intel
        N_EXPECT='intel'
        N_GAME_ENV=(__GLX_VENDOR_LIBRARY_NAME=mesa)
        N_ICD="$(icd_files 'intel_icd*.json')"
        ;;
    *) die "--gpu: don't know the driver of $node: ${driver:-none}" 3 ;;
    esac
}

gpu_setup() { # comma-joined render nodes, the compositor's first ("": software); --expect-gpu
    local list="$1" want="$2" name node seen="," labels="" expects="" first=1 json
    local -a nodes
    software_setup
    GPU_BINDS=()
    GPU_NODES=()
    GPUS_JSON='[]'
    GPU_WANT=""
    COMPOSITOR_ENV=(WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1)
    GAME_ENV=(GOGO_LAUNCHER_NO_DIALOG=1)
    if [ -z "$list" ]; then
        [ -z "$want" ] || die "--expect-gpu needs --gpu" 2
        GPU_LABEL=software
        GPU_EXPECT='llvmpipe|softpipe|lavapipe'
        COMPOSITOR_ENV=("${SOFTWARE_COMPOSITOR_ENV[@]}")
        GAME_ENV=("${SOFTWARE_ENV[@]}")
        return 0
    fi
    [ -z "$want" ] || want="/dev/dri/${want#/dev/dri/}"
    IFS=, read -ra nodes <<<"$list"
    for name in "${nodes[@]}"; do
        node="/dev/dri/${name#/dev/dri/}"
        case "$seen" in *",$node,"*) die "--gpu: named twice: $node" 2 ;; esac
        seen="$seen$node,"
        gpu_node "$node"
        GPU_BINDS+=("${N_BINDS[@]}")
        GPU_NODES+=("${N_BINDS[@]}")
        labels="${labels:+$labels+}$N_LABEL"
        expects="${expects:+$expects|}$N_EXPECT"
        json="$(jq -cn --arg node "$node" --arg label "$N_LABEL" --arg expect "$N_EXPECT" --arg held "$N_HELD" \
            --argjson compositor "$first" '{node:$node, label:$label, expect:$expect, held:$held, compositor:($compositor == 1)}')"
        GPUS_JSON="$(jq -c --argjson g "$json" '. + [$g]' <<<"$GPUS_JSON")"
        if [ "$first" = 1 ]; then COMPOSITOR_ENV+=("${N_COMPOSITOR_ENV[@]}"); fi
        first=0
    done
    case "$seen" in *",${want:-$node},"*) ;; *) die "--expect-gpu isn't one of --gpu: $want" 2 ;; esac
    GPU_LABEL="$labels"
    GPU_EXPECT="$expects"
    GPU_WANT="$want"
    if [ "${#nodes[@]}" = 1 ]; then
        # The one GPU the game may find: nothing else to choose, however it looks.
        GPU_WANT="$node"
        GAME_ENV+=("${N_GAME_ENV[@]}")
        [ -z "$N_ICD" ] || GAME_ENV+=("VK_DRIVER_FILES=$N_ICD")
    else
        # Several: what a desktop gives a game, so the choice is the game's own.
        # The loader searches for every ICD and implicit layer, and nothing steers.
        GAME_ENV+=("XDG_DATA_DIRS=/run/opengl-driver/share:/run/opengl-driver-32/share:/usr/local/share:/usr/share")
    fi
}

# --- run ----------------------------------------------------------------------

cmd_run() {
    local target="" manifest="" gpu="" expect_gpu="" secs=600 out=""
    GOGO="" KEEP=0 FRESH=0
    while [ $# -gt 0 ]; do
        case "$1" in
        --manifest) manifest="${2:?}"; shift 2 ;;
        --gpu) gpu="${gpu:+$gpu,}${2:?}"; shift 2 ;;
        --expect-gpu) expect_gpu="${2:?}"; shift 2 ;;
        --secs) secs="${2:?}"; shift 2 ;;
        --gogo) GOGO="${2:?}"; shift 2 ;;
        --out) out="${2:?}"; shift 2 ;;
        --keep) KEEP=1; shift ;;
        --no-cache) FRESH=1; shift ;;
        -h | --help) usage ;;
        -*) log "unknown option: $1"; usage ;;
        *) [ -z "$target" ] || usage; target="$1"; shift ;;
        esac
    done
    [ -n "$target" ] || usage
    case "$secs" in '' | *[!0-9]*) die "--secs wants a number" 2 ;; esac
    [ -n "$manifest" ] || manifest="$REPO/e2e/$target.toml"
    [ -f "$manifest" ] || die "no manifest: $manifest" 2

    resolve_tools
    # The builds need nix; everything after them runs on the pinned PATH.
    local needs_gogo=1
    MANIFEST_JSON="$(PATH="$TOOLS/bin" helper manifest "$manifest")" || die "bad manifest" 2
    GOGID="$(PATH="$TOOLS/bin" jq -r '.gogid // empty' <<<"$MANIFEST_JSON")"
    TITLE="$(PATH="$TOOLS/bin" jq -r .title <<<"$MANIFEST_JSON")"
    if [ -n "$GOGID" ]; then
        [ "$GOGID" = "$target" ] || die "manifest is for $GOGID, not $target" 2
    else
        needs_gogo=0
        GOGID=""
    fi
    UMU_PY=""
    if [ "$needs_gogo" = 1 ]; then
        if [ -n "$GOGO" ]; then
            GOGO="$(readlink -f "$GOGO")"
            [ -x "$GOGO" ] || die "--gogo: not executable: $GOGO" 2
            local umu_env
            umu_env="$(nix build --no-link --print-out-paths "$REPO#umu-python")" || die "couldn't build .#umu-python"
            UMU_PY="$umu_env/bin/gogo-umu-python"
        else
            log "building .#gogo-linux-app"
            local pkg
            pkg="$(nix build --no-link --print-out-paths "$REPO#gogo-linux-app")" || die "couldn't build .#gogo-linux-app"
            GOGO="$pkg/bin/gogo"
        fi
    fi
    use_tools
    gpu_setup "$gpu" "$expect_gpu"

    # One run at a time: they share the cache and the GPU. fd 9 stays out of
    # the sandbox.
    mkdir -p "$ROOT/runs"
    exec 9>"$ROOT/lock"
    if ! flock -n 9; then
        log "another run is going: waiting"
        flock 9
    fi
    remove_dead_runs

    local stamp slug
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    slug="$target-$stamp-$$"
    RUN_ID="linux-e2e-$slug"
    RUN="$ROOT/runs/$slug"
    RESULTS="${out:-$ROOT/results}/$slug"
    mkdir -p "$RESULTS/shots" "$RUN"
    RESULTS="$(readlink -f "$RESULTS")"
    INTERRUPTED="" INTERRUPT_COUNT=0 SANDBOX_PID="" MODE_NOW=build
    trap teardown EXIT
    trap 'on_signal INT' INT
    trap 'on_signal TERM' TERM
    trap 'on_signal HUP' HUP
    printf '%s\n' "$$" >"$RUN/.owner"
    if [ "$KEEP" = 1 ]; then : >"$RUN/.kept"; fi

    GOGO_HOME_DIR="$RUN/gogo"
    PREFIX=""
    if [ -n "$GOGID" ]; then PREFIX="$GOGO_HOME_DIR/games/$GOGID/prefix"; fi
    if [ "$FRESH" = 1 ]; then CACHE="$RUN/cache"; else CACHE="$ROOT/cache"; fi
    mkdir -p "$CACHE/store/runtimes" "$CACHE/store/renderers" "$CACHE/depot" "$CACHE/umu" \
        "$RUN/home" "$RUN/xdg/data/umu" "$RUN/xdg/config" "$RUN/xdg/cache" "$RUN/xdg/state" \
        "$RUN/harness" "$RUN/store" "$GOGO_HOME_DIR" \
        "$RUN/xdg/cache/gogo/depot"
    # One mount: the store stages renderers in runtimes/.tmp and renames them
    # over, which two bind mounts refuse (EXDEV), and cnc-ddraw falls back.
    ln -s "$RUN/store/runtimes" "$GOGO_HOME_DIR/runtimes"
    ln -s "$RUN/store/renderers" "$GOGO_HOME_DIR/renderers"
    cp "$HELPER_SRC" "$RUN/harness/helper.py"
    mkdir -p "$RUN/etc"
    printf '%s:x:%s:%s::%s:%s\n' "$(id -un)" "$(id -u)" "$(id -g)" "$RUN/home" "$TOOLS/bin/bash" >"$RUN/etc/passwd"
    printf '%s:x:%s:\n' "$(id -gn)" "$(id -g)" >"$RUN/etc/group"
    # Answers `gogo linux-app` would ask for, and never Steam's tools dir.
    printf '[linux]\numu = "zipapp"\nproton_dir = "store"\n' >"$GOGO_HOME_DIR/config.toml"
    log "scratch: $RUN"
    log "results: $RESULTS"

    local forbidden
    forbidden="$(jq -cn --arg h "$HOME" --arg r "/run/user/$(id -u)" '[
        "\($h)/.local/share/applications", "\($h)/.local/share/Steam",
        "\($h)/.local/share/gogo", "\($h)/.local/share/umu", "\($h)/.steam",
        "\($h)/.var/app/com.valvesoftware.Steam", "\($h)/Downloads", "\($h)/.cache/gogo",
        "\($r)/bus", "\($r)/wayland-1", "\($r)/pulse", "\($r)/pipewire-0",
        "/tmp/.X11-unix/X1"]')"
    # What both of the driver's phases read; `extra` is one phase's own keys.
    drive_config() { # file extra-json label expect want-node gpus-json [@n:node…] [@e:KEY=value…]
        local file="$1" extra="$2" label="$3" expect="$4" want="$5" gpus="$6"
        shift 6
        jq -n --argjson m "$MANIFEST_JSON" --arg run "$RUN" --arg results "$RESULTS" \
            --arg shots "$RESULTS/shots" --arg stop "$RUN/stop" \
            --arg netns "$(readlink /proc/self/ns/net)" --arg pidns "$(readlink /proc/self/ns/pid)" \
            --argjson forbidden "$forbidden" --argjson extra "$extra" \
            --arg label "$label" --arg expect "$expect" --arg want "$want" --argjson gpus "$gpus" --arg rt "$RUNTIME_DIR" \
            --args '{manifest:$m, run_dir:$run, results_dir:$results, shots_dir:$shots, stop_file:$stop,
                host_netns:$netns, host_pidns:$pidns, forbidden_paths:$forbidden, runtime_dir:$rt,
                gpu:{label:$label, expect:$expect, want:$want, gpus:$gpus,
                     nodes:($ARGS.positional | map(select(startswith("@n:")) | ltrimstr("@n:")))},
                game_env:($ARGS.positional | map(select(startswith("@e:")) | ltrimstr("@e:"))
                          | map(capture("^(?<key>[^=]+)=(?<value>.*)$")) | from_entries)} + $extra' \
            "$@" >"$file"
    }

    local build_cmd='null'
    if [ -n "$GOGID" ]; then
        build_cmd="$(jq -cn --arg g "$GOGO" --arg i "$GOGID" '[$g,"linux-app",$i,"--yes"]')"
    else
        build_cmd="$(jq -c .build_command <<<"$MANIFEST_JSON")"
    fi
    if [ "$build_cmd" != null ]; then
        log "building the game on a headless display (log: $RESULTS/build.log)"
        MODE_NOW=build
        local build_extra support_log=""
        # build.rs: <game state>/<INSTALL_LOGS>/support/<INNO_LOG>, once the build has moved it in place
        [ -z "$GOGID" ] || support_log="$GOGO_HOME_DIR/games/$GOGID/install/support/inno.log"
        build_extra="$(jq -cn --argjson c "$build_cmd" --arg cwd "$RUN/home" --arg s "$support_log" \
            '{build_command:$c, launch_cwd:$cwd, support_log:$s}')"
        drive_config "$RUN/build.json" "$build_extra" software 'llvmpipe|softpipe|lavapipe' "" '[]' \
            "${SOFTWARE_ENV[@]/#/@e:}"
        start_sandbox build "$RESULTS/build-sandbox.log" python3 -I "$RUN/harness/helper.py" drive "$RUN/build.json"
        local build_rc=0
        supervise 0 || build_rc=$?
        if [ "$build_rc" = 3 ]; then
            INFRA_ERROR="no build display: $(jq -r '[.checks[] | select(.ok | not) | .name + ": " + .detail][0] // "see build-sandbox.log"' "$RESULTS/build-result.json" 2>/dev/null || tail -n 3 "$RESULTS/build-sandbox.log" | tr '\n' ' ')"
            exit 3
        fi
        if [ "$build_rc" != 0 ] || [ -n "$INTERRUPTED" ]; then
            BUILD_FAILED="the build failed (exit $build_rc), see $RESULTS/build.log"
            exit 1
        fi
    fi

    local launch launch_cwd doctor='null' game_log=""
    if [ -n "$GOGID" ]; then
        launch="$(jq -cn --arg l "$GOGO_HOME_DIR/games/$GOGID/launch.sh" '[$l]')"
        launch_cwd="$GOGO_HOME_DIR/games/$GOGID"
        doctor="$(jq -cn --arg g "$GOGO" --arg i "$GOGID" '[$g,"linux-app","doctor",$i,"--json"]')"
        game_log="$GOGO_HOME_DIR/logs/$GOGID/latest.log"
    else
        launch="$(jq -c .command <<<"$MANIFEST_JSON")"
        launch_cwd="$RUN/home"
    fi
    local game_extra
    game_extra="$(jq -cn --argjson l "$launch" --arg cwd "$launch_cwd" --argjson d "$doctor" --arg g "$game_log" \
        '{launch:$l, launch_cwd:$cwd, doctor:$d, game_log:$g}')"
    drive_config "$RUN/drive.json" "$game_extra" "$GPU_LABEL" "$GPU_EXPECT" "${GPU_WANT:-}" "$GPUS_JSON" \
        "${GPU_NODES[@]/#/@n:}" "${GAME_ENV[@]/#/@e:}"

    log "running in headless sway ($GPU_LABEL, up to $secs s)"
    MODE_NOW=game
    start_sandbox game "$RESULTS/sandbox.log" python3 -I "$RUN/harness/helper.py" drive "$RUN/drive.json"
    local rc=0
    supervise "$(($(date +%s) + secs))" || rc=$?
    [ "$rc" = 0 ] || [ -n "$INTERRUPTED" ] || [ -n "${TIMED_OUT:-}" ] || {
        [ -s "$RESULTS/result.json" ] || INFRA_ERROR="sandbox exited $rc before the driver reported: $(tail -n 3 "$RESULTS/sandbox.log" | tr '\n' ' ')"
    }
    exit 0
}

# Scratch whose owner died too hard for its teardown (a SIGKILL, a power cut).
remove_dead_runs() {
    local d owner
    for d in "$ROOT"/runs/*/; do
        [ -d "$d" ] && [ -f "$d/.owner" ] || continue
        owner="$(cat "$d/.owner")"
        [ ! -e "$d/.kept" ] || continue
        kill -0 "$owner" 2>/dev/null && continue
        log "removing dead run's scratch: $d"
        chmod -R u+rwX "$d" 2>/dev/null || true
        rm -rf "$d"
    done
}

cmd_cleanup() {
    resolve_tools
    use_tools
    mkdir -p "$ROOT/runs"
    exec 9>"$ROOT/lock"
    flock -n 9 || die "a run is going"
    remove_dead_runs
}

# --- self-test ----------------------------------------------------------------

st_fail() { log "self-test FAILED: $1"; ST_FAILED=1; }

# Run the harness with signals back to their defaults (a background job of a
# script starts with SIGINT ignored, and bash can't trap that).
st_start() { # outdir manifest extra…
    local out="$1" manifest="$2"; shift 2
    env --default-signal=INT,TERM "$SELF" run self-test --manifest "$manifest" --out "$out" "$@" \
        >"$out/stdout" 2>"$out/stderr" &
    ST_PID=$!
}

st_wait_window() { # outdir → waits for the driver's "window" check
    local out="$1" deadline=$(($(date +%s) + 60))
    while [ "$(date +%s)" -lt "$deadline" ]; do
        if jq -e 'any(.checks[]; .name == "window" and .ok)' "$out"/*/result.json >/dev/null 2>&1; then return 0; fi
        sleep 0.5
    done
    return 1
}

st_assert_clean() { # name outdir exit-code want-exit
    local name="$1" out="$2" code="$3" want="$4" summary
    summary="$(tail -n 1 "$out/stdout")"
    [ "$code" = "$want" ] || st_fail "$name: exit $code, want $want (see $out/stderr)"
    if [ ! -f "$summary" ]; then st_fail "$name: no summary (see $out/stderr)"; return 0; fi
    jq -e '.cleanup.left == "" and .cleanup.swept == "" and .cleanup.scratch_deleted' "$summary" >/dev/null ||
        st_fail "$name: cleanup: $(jq -c .cleanup "$summary")"
    local scratch
    scratch="$(jq -r .cleanup.scratch "$summary")"
    [ ! -e "$scratch" ] || st_fail "$name: scratch still there: $scratch"
    log "self-test: $name: exit $code, cleanup $(jq -c .cleanup.scratch_deleted "$summary")"
}

cmd_self_test() {
    [ $# -eq 0 ] || usage
    resolve_tools
    use_tools
    export GOGO_E2E_TOOLS="$TOOLS"
    ST_FAILED=""
    local base="$ROOT/self-test" code out
    chmod -R u+rwX "$base" 2>/dev/null || true
    rm -rf "$base"
    mkdir -p "$base"

    # 1. The whole path with a plain X11 client: window, click, key, exit.
    out="$base/pass"; mkdir -p "$out"
    log "self-test: the pass case"
    code=0
    env --default-signal=INT,TERM "$SELF" run self-test --manifest "$REPO/e2e/self-test.toml" --out "$out" \
        >"$out/stdout" 2>"$out/stderr" || code=$?
    st_assert_clean pass "$out" "$code" 0
    local summary
    summary="$(tail -n 1 "$out/stdout")"
    if ! jq -e .ok "$summary" >/dev/null 2>&1; then
        st_fail "pass: failed: $(jq -c .failed "$summary" 2>/dev/null)"
    fi
    ST_SUMMARY="$summary"
    jq -e '.build.exit == 0 and (.checks | any(.name == "build_isolation_display" and .ok))' "$summary" >/dev/null ||
        st_fail "pass: the build step had no display: $(jq -c .build "$summary" 2>/dev/null)"
    jq -e '.checks | any(.name == "audio" and .ok)' "$summary" >/dev/null ||
        st_fail "pass: no audio device: $(jq -c .audio "$summary" 2>/dev/null)"

    # 2. A signal mid-run: stopped, reported as failed, nothing left. A client
    #    that never ends, so only the teardown can end it.
    local sig
    for sig in TERM INT; do
        out="$base/sig-$sig"; mkdir -p "$out"
        log "self-test: SIG$sig mid-run"
        st_start "$out" "$REPO/e2e/self-test-hold.toml"
        st_wait_window "$out" || st_fail "sig-$sig: no window"
        kill -"$sig" "$ST_PID"
        code=0
        wait "$ST_PID" || code=$?
        st_assert_clean "sig-$sig" "$out" "$code" 1
    done

    # 3. The time limit: same, with nobody sending a signal.
    out="$base/timeout"; mkdir -p "$out"
    log "self-test: over --secs"
    code=0
    env --default-signal=INT,TERM "$SELF" run self-test --manifest "$REPO/e2e/self-test-hold.toml" \
        --out "$out" --secs 12 >"$out/stdout" 2>"$out/stderr" || code=$?
    st_assert_clean timeout "$out" "$code" 1
    summary="$(tail -n 1 "$out/stdout")"
    jq -e '.failed | index("time_limit")' "$summary" >/dev/null || st_fail "timeout: time_limit didn't fail"

    # 4. A SIGKILL: no teardown can run, but bwrap dies with its parent and the
    #    PID namespace with it; `cleanup` then takes the scratch.
    out="$base/kill"; mkdir -p "$out"
    log "self-test: SIGKILL mid-run"
    st_start "$out" "$REPO/e2e/self-test-hold.toml"
    st_wait_window "$out" || st_fail "kill: no window"
    local killed_run
    killed_run="$(jq -r .run_dir "$ROOT"/runs/*/drive.json 2>/dev/null | head -n 1)"
    local killed_id
    killed_id="$(basename "$killed_run")"
    kill -KILL "$ST_PID"
    wait "$ST_PID" 2>/dev/null || true
    sleep 2
    local stray
    stray="$(RUN_ID="linux-e2e-$killed_id" RUN="$killed_run" PREFIX="" RUN_PIDNS="" leftovers)"
    [ -z "$stray" ] || st_fail "kill: left behind: $stray"
    [ -d "$killed_run" ] || st_fail "kill: expected its scratch to stay for cleanup"
    "$SELF" cleanup 2>>"$out/stderr" || st_fail "kill: cleanup failed"
    [ ! -e "$killed_run" ] || st_fail "kill: cleanup left $killed_run"

    # 5. --gpu's usage errors, before anything needs a GPU.
    log "self-test: --gpu's usage"
    local bad
    for bad in "--expect-gpu renderD128" "--gpu renderD128,renderD128" "--gpu card1" \
        "--gpu renderD128 --gpu renderD128"; do
        code=0
        # shellcheck disable=SC2086 # the words of one case
        "$SELF" run self-test --manifest "$REPO/e2e/self-test.toml" $bad >/dev/null 2>&1 || code=$?
        [ "$code" = 2 ] || st_fail "usage: $bad: exit $code, want 2"
    done

    # 6. The sweep sees a stray process that outlived its run: a decoy carrying
    #    a run's marker, outside any sandbox.
    log "self-test: the sweep finds a stray"
    RUN_ID="self-test-decoy-$$" RUN="$base/decoy" PREFIX="$base/decoy/prefix" RUN_PIDNS=""
    env WINEPREFIX="$PREFIX/pfx/" sleep 60 &
    local decoy=$!
    sleep 0.2
    if ! leftovers | grep -q "^$decoy "; then st_fail "sweep missed the decoy"; fi
    kill -KILL "$decoy" 2>/dev/null || true
    wait "$decoy" 2>/dev/null || true

    if [ -n "$ST_FAILED" ]; then
        log "self-test: FAILED"
        [ -f "${ST_SUMMARY:-}" ] && jq . "$ST_SUMMARY"
        exit 1
    fi
    log "self-test: all passed"
    jq . "$ST_SUMMARY"
    printf '%s\n' "$ST_SUMMARY"
}

# ------------------------------------------------------------------------------

[ $# -ge 1 ] || usage
cmd="$1"; shift
case "$cmd" in
run) cmd_run "$@" ;;
self-test) cmd_self_test "$@" ;;
cleanup) cmd_cleanup "$@" ;;
-h | --help | help) usage ;;
*) log "unknown command: $cmd"; usage ;;
esac
