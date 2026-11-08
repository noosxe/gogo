#!/usr/bin/env bash
# Headless sway end-to-end harness for `gogo linux-app` games (#201, how-to:
# docs/linux-e2e.md): builds a game in a scratch home, runs it through its
# generated launch.sh in a headless compositor inside a bwrap root, drives the
# manifest's steps, checks the frames, then tears everything down.
#
#   scripts/linux-e2e.sh run <gogid> [opts]   one game, from e2e/<gogid>.toml
#   scripts/linux-e2e.sh self-test            the harness itself: a plain xterm,
#                                             teardown on TERM, INT, KILL, timeout,
#                                             the build's display, the null audio
#                                             sink, a live session
#   scripts/linux-e2e.sh cleanup [--prune-installed NAME|all] [--stop-live]
#                                             remove scratch of dead runs; stop
#                                             orphaned live sessions (all of them
#                                             with --stop-live); list the
#                                             installed bases (--reuse-install) and
#                                             their size, and prune the ones named
#   scripts/linux-e2e.sh live start <gogid> [run opts] [live opts]
#                                             a live session an agent drives
#                                             (#241): prints its id, and goes on
#                                             in the background
#   scripts/linux-e2e.sh live <id> CMD …      one command to it (below)
#   scripts/linux-e2e.sh live list            the sessions on this cache
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
#   --installer FILE  build from this GOG setup .exe instead of the depot: its
#                     folder is mounted read-only, in the build's sandbox only
#   --keep            keep the scratch dir, for a look around
#   --no-cache        download Proton, umu's runtime and the game afresh, into
#                     scratch (default: target/linux-e2e/cache, shared by runs)
#   --reuse-install   skip the build when an installed base with the same inputs
#                     is in target/linux-e2e/cache/installed, and run on a
#                     copy-on-write copy of it; else build and save one. For
#                     manifest rounds: a PR's final run and CI build afresh
#   --out DIR         results root (default target/linux-e2e/results)
#
# GOGO_E2E_COPY=auto|snapshot|reflink|copy: how installed bases and a live
# session's disk are copied (default auto: btrfs snapshots where they work
# unprivileged, else reflinks, else a full copy; docs/linux-e2e.md).
#
# live start options, beside run's (--secs aside): the manifest is optional,
# and only its [window], [env], [override] and [exit] count:
#   --env K=V         into [override.env]: gogo's `linux-app env set`, per launch
#   --arg A           into [override].extra_args: `linux-app args`, repeatable
#   --idle-secs N     stop after N s with no command (default 900)
#   --max-secs N      stop N s after taking the cache lock, at the latest (5400)
#   --max-gap N       gaps between commands kept up to N s (default 10)
#   --match-pct N     a replayed screenshot matches at most N% different (15)
#
# live <id> commands, each one answers as JSON (exit 0 ok, 1 failed, 3 the
# session isn't there, 4 not ready yet):
#   status [--wait SECS]          its state; with --wait, until it's ready
#   screenshot [NAME]             a PNG under target/, and the change % since
#                                 the last one
#   key KEYSYM | type TEXT | hold KEYSYM MS
#   click [X Y] | move_to X Y     X Y: pixels of the screenshot, or N% of the window
#   move_by DX DY | scroll NOTCHES | wait SECS
#   settle [--max SECS]           until the screen holds still (wait_settled), or
#                                 "settled": false after SECS (default 30)
#   checkpoint NAME               mark where it is; restore NAME goes back there:
#   restore NAME [--wait SECS]    close, roll the disk back, relaunch, replay
#   log [game|run|launch|sway|dxvk|vkd3d|proton] [--tail N]
#   export                        the path so far as a manifest, on stdout
#   stop                          close the game, check, tear down: the summary
#
# Output: progress on stderr; the summary JSON on stdout, then on the LAST line
# the path of summary.json. Exit codes: 0 = every check passed; 1 = a check
# failed; 2 = usage; 3 = harness problem (says nothing about the game).
#
# Agents launch real games only through this script (AGENTS.md).
set -euo pipefail

# A live session's supervisor runs from a copy of this script (live_start),
# which says where the checkout is: bash reads a script as it goes, and an
# edit under a long session would land mid-function.
REPO="${GOGO_E2E_REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
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

# The resolver address the build sees; pasta forwards it to the host's.
BUILD_DNS=192.0.2.53
# Where --installer's folder appears in the build's sandbox: a fixed path, so its
# host path says nothing and the scratch cleanup never walks it.
INSTALLER_MOUNT=/e2e-installer

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
        # Build only: the session bus for the keyring login. The network is the
        # holder's (start_sandbox): a netns of its own, NATed out by pasta. Its
        # resolver address forwards to the host's, so no nscd or resolved sockets.
        # The holder's root is the real user, so the sandbox names its uid again.
        SB+=(--uid "$(id -u)" --gid "$(id -g)")
        SB+=(--ro-bind "$RUN/etc/resolv.conf" "$(readlink -f /etc/resolv.conf)")
        if [ -S "/run/user/$(id -u)/bus" ]; then
            SB+=(--bind "/run/user/$(id -u)/bus" "$RUNTIME_DIR/bus")
        fi
        [ -z "${INSTALLER_DIR:-}" ] || SB+=(--ro-bind "$INSTALLER_DIR" "$INSTALLER_MOUNT")
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
        [ -z "${INSTALLER_FILE:-}" ] || SE+=(GOGO_E2E_INSTALLER="$INSTALLER_FILE")
    else
        SE+=("${COMPOSITOR_ENV[@]}")
    fi
    if [ "$mode" = build ]; then
        # A holder with user and network namespaces of its own (root, mapped to
        # the real user, so it has the capabilities pasta's own child lacks and
        # bwrap can nest its user namespace in it). pasta joins them from outside,
        # and bwrap starts once the tap is there. SANDBOX_PID is the holder.
        # shellcheck disable=SC2016  # the gate is bash -c text
        env -i "${SE[@]}" "$TOOLS/bin/unshare" --user --map-root-user --net "$TOOLS/bin/bash" -c '
            for _ in $(seq 100); do [ "$(wc -l </proc/net/dev)" -gt 3 ] && break; sleep 0.1; done
            "$1" link set lo up; shift
            exec "$@"' _ "$TOOLS/bin/ip" "$TOOLS/bin/bwrap" "${SB[@]}" "$@" >"$logf" 2>&1 9>&- </dev/null &
        SANDBOX_PID=$!
        # Not before the holder has left ours.
        for _ in $(seq 100); do
            [ "$(readlink "/proc/$SANDBOX_PID/ns/user")" != "$(readlink /proc/self/ns/user)" ] &&
                [ "$(readlink "/proc/$SANDBOX_PID/ns/net")" != "$(readlink /proc/self/ns/net)" ] && break
            sleep 0.05
        done
        "$TOOLS/bin/pasta" --config-net --foreground --quiet --dns-forward "$BUILD_DNS" \
            --userns "/proc/$SANDBOX_PID/ns/user" --netns "/proc/$SANDBOX_PID/ns/net" \
            >>"$logf" 2>&1 9>&- </dev/null &
        NET_PID=$!
    else
        env -i "${SE[@]}" "$TOOLS/bin/bwrap" "${SB[@]}" "$@" >"$logf" 2>&1 9>&- </dev/null &
        SANDBOX_PID=$!
    fi
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
    if [ -n "${NET_PID:-}" ]; then
        kill "$NET_PID" 2>/dev/null || true
        wait "$NET_PID" 2>/dev/null || true
        NET_PID=""
    fi
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
    [ -n "${RUN:-}" ] || { clear_holder; exit "$status"; }
    # An exit nobody planned (set -e, die): the run proved nothing.
    if [ "$status" != 0 ] && [ -z "${BUILD_FAILED:-}" ]; then
        INFRA_ERROR="${INFRA_ERROR:-the script stopped with exit $status; see stderr}"
    fi
    if [ -n "${SANDBOX_PID:-}" ]; then
        kill -KILL "$SANDBOX_PID" 2>/dev/null || true
        wait "$SANDBOX_PID" 2>/dev/null || true
        SANDBOX_PID=""
    fi
    if [ -n "${NET_PID:-}" ]; then
        kill "$NET_PID" 2>/dev/null || true
        wait "$NET_PID" 2>/dev/null || true
        NET_PID=""
    fi
    collect_logs
    sweep
    local left="$SWEPT_LEFT" killed="$SWEPT_KILLED"
    [ -z "${LIVE:-}" ] || live_finish
    local deleted=false
    if [ "$KEEP" = 1 ]; then
        if [ "${REUSE:-0}" = 1 ] && [ -d "$RUN" ] && mv -T "$RUN" "$INSTALLED/.runs/$SLUG"; then
            # Out of the next reused run's way; paths inside still name the old one.
            RUN="$INSTALLED/.runs/$SLUG"
        fi
        log "kept scratch: $RUN"
    elif delete_scratch; then
        deleted=true
    fi
    clear_holder
    write_summary "$status" "$killed" "$left" "$deleted"
    exit "$FINAL_EXIT"
}

# What's left of the run's processes once its sandbox is gone: wait a little
# for them, then kill. Sets SWEPT_KILLED (what had to be killed) and
# SWEPT_LEFT (what even that didn't end).
sweep() {
    local left="" tries=10 pid
    SWEPT_KILLED=""
    while [ "$tries" -gt 0 ]; do
        tries=$((tries - 1))
        left="$(leftovers)"
        [ -n "$left" ] || break
        sleep 0.5
    done
    if [ -n "$left" ]; then
        log "left behind: $(printf '%s' "$left" | tr '\n' ';')"
        SWEPT_KILLED="$left"
        while read -r pid _; do kill -KILL "$pid" 2>/dev/null || true; done <<<"$left"
        sleep 1
        left="$(leftovers)"
    fi
    SWEPT_LEFT="$left"
}

on_signal() {
    INTERRUPTED="$1"
    INTERRUPT_COUNT=$((INTERRUPT_COUNT + 1))
    log "got SIG$1: tearing down"
}

delete_scratch() {
    case "$RUN" in "$ROOT"/runs/?* | "${INSTALLED:-/nonexistent}"/.runs/?*) ;; *) log "not deleting $RUN"; return 1 ;; esac
    [ -d "$RUN" ] || return 0
    # Proton and umu leave read-only dirs; bind-mount targets are just empty
    # dirs out here, so the cache isn't touched.
    chmod -R u+rwX "$RUN" 2>/dev/null || true
    rm -rf "$RUN" 2>/dev/null || true
    # rm -rf takes subvolumes too, emptied, then rmdir(2): only what's gone counts.
    gone_or_say "$RUN"
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
        --arg runtime "${STATE_RUNTIME:-}" --arg date "$(date -u +%F)" \
        --arg installer "${INSTALLER_FILE##*/}" --argjson reuse "$(reuse_json)" --argjson live "$(live_json)" \
        --arg copy "${COPY_MODE:-}" --arg copy_ask "${COPY_ASK:-auto}" --arg copy_note "${COPY_NOTE:-}" '
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
           build:{reused:($reuse.reused // false), reuse:$reuse,
                  # How bases and live disks are copied (#255); null: nothing was.
                  copy:(if $copy == "" then null
                        else {mode:$copy, asked:$copy_ask, note:(if $copy_note == "" then null else $copy_note end)} end),
                  exit:($bld.exit // null), secs:($bld.secs // null), isolation:($bld.isolation // {}),
                  installer:(if $installer == "" then null else $installer end),
                  windows:($bld.windows // []), support_installer:($bld.support_installer // null)},
           game_exit:($res.game_exit // null),
           doctor:($res.doctor // null),
           log_excerpt:$excerpt,
           cleanup:{left:$left, swept:$killed, scratch:$scratch, scratch_deleted:$deleted, kept:($keep == "1")},
           live:$live,
           # A compat entry stands on a fresh run, never a reused base or a live session.
           tested_entry:(if $ok and $gogid != "self-test" and $runtime != "" and ($reuse.reused // false | not)
                            and $live == null
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

# build.reuse: null without --reuse-install. `reused` says the game ran on a
# copy of an earlier build; `saved`, that this run's build became a base.
reuse_json() {
    [ "${REUSE:-0}" = 1 ] || { echo null; return 0; }
    local meta='null'
    [ "${REUSED:-}" != 1 ] || meta="$(jq -c '{built_by, build_results, created, build_secs}' "${REUSE_BASE:-}/meta.json" 2>/dev/null || echo null)"
    jq -cn --argjson reused "$([ "${REUSED:-}" = 1 ] && echo true || echo false)" --arg key "${REUSE_KEY:-}" \
        --arg base "${REUSE_BASE:-}" --arg copy "${COPY_MODE:-}" --arg secs "${COPY_SECS:-}" --arg saved "${SAVED:-}" \
        --arg note "${SAVE_NOTE:-}" --argjson meta "$meta" '
        {reused:$reused, key:(if $key == "" then null else $key end),
         base:(if $base == "" then null else $base end),
         source:(if $reused then "cache" else "fresh build" end),
         copy:(if $copy == "" then null else $copy end),
         copy_secs:(if $secs == "" then null else ($secs | tonumber) end),
         saved:(if $saved == "" then null else ($saved == "true") end),
         note:(if $note == "" then null else $note end),
         base_meta:$meta}'
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

# The scratch a build starts from. Part of the installed-base key
# (install_key_inputs): what's here shapes what the build leaves.
seed_scratch() {
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
    printf 'nameserver %s\n' "$BUILD_DNS" >"$RUN/etc/resolv.conf"
    # Answers `gogo linux-app` would ask for, and never Steam's tools dir.
    printf '[linux]\numu = "zipapp"\nproton_dir = "store"\n' >"$GOGO_HOME_DIR/config.toml"
}

# --- installed bases (--reuse-install, #240) ------------------------------------
#
# A build's output, kept under $INSTALLED/<target>-<key>/: tree/ holds the
# run's gogo, home and xdg as the build left them, before anything launched,
# and meta.json says what built it. A reused run gets a copy-on-write copy
# (reflinks; a full copy where the filesystem has none) at the very path it
# was built at, $INSTALLED/.runs/<target>: launch.sh, the prefix and
# state.toml name absolute paths, and a moved data root would send launch.sh
# through setup again. The base itself is only ever read.

# Bump with any change to what a build gets that the key doesn't already see.
REUSE_FORMAT=1

# Where bases live. target/linux-e2e/cache may be a link to a checkout's
# shared cache (worktrees share one), so bases are shared too: the resolved
# dir must be a target/linux-e2e/cache/installed, ours, and not a link itself.
# GOGO_E2E_INSTALLED_DIR is the self-test's own, under $ROOT, so it never
# prunes the real ones. Runs on a base have their scratch in its .runs/; they
# take turns with every other run on the cache lock (below).
installed_dir() {
    local d="${GOGO_E2E_INSTALLED_DIR:-$ROOT/cache/installed}" real
    if [ -n "${GOGO_E2E_INSTALLED_DIR:-}" ]; then
        case "$d" in "$ROOT"/?*) ;; *) die "installed bases must be under $ROOT: $d" 2 ;; esac
    fi
    mkdir -p "$d/.runs"
    real="$(readlink -f "$d")"
    if [ -L "$d" ] || [ "$(stat -c %u "$real")" != "$(id -u)" ] || [ "$(stat -c %u "$real/.runs")" != "$(id -u)" ] ||
        [ -L "$real/.runs" ]; then
        die "not the harness's own dir: $d" 3
    fi
    if [ -n "${GOGO_E2E_INSTALLED_DIR:-}" ]; then
        [ "$real" = "$d" ] || die "a link on the way to $d" 3
    else
        case "$real" in */target/linux-e2e/cache/installed) ;; *) die "$d leads to $real" 3 ;; esac
    fi
    INSTALLED="$real"
}

# The cache lock: one `run` or `cleanup` at a time over a cache, across every
# checkout that shares it (worktrees link target/linux-e2e/cache to the main
# checkout's). fd 9, in the resolved cache dir, never inherited by a sandbox.
# It covers the whole run: the build writes the store, umu's runtime and the
# depot cache, a reused base is copied and saved in here, and the game takes the
# GPU. .lock.holder beside it says who has it; the flock is the truth.
# GOGO_E2E_LOCK_DIR is the self-test's own dir, under $ROOT.
CACHE_HOLDER=""
cache_lock() { # run (waits) | cleanup (refuses when busy)
    lock_dir
    exec 9>>"$LOCK_REAL/.lock"
    if ! flock -n 9; then
        log "the cache $LOCK_REAL is busy: $(holder_text)"
        [ "$1" != cleanup ] || die "a run is going: not cleaning up under it"
        log "waiting for it"
        if [ -n "${LIVE:-}" ]; then
            live_state waiting "for the cache lock: $(holder_text)"
            # In the background, so a stop (TERM) gets through. The lock is
            # the open file's, ours too once the child has it.
            flock 9 &
            LOCK_WAITER=$!
            wait "$LOCK_WAITER"
            LOCK_WAITER=""
        else
            flock 9
        fi
    fi
    write_holder starting
}

# The cache's dir, resolved and checked: LOCK_REAL, and CACHE_HOLDER in it.
# Live sessions keep their registry beside the lock, in live/ (#241).
lock_dir() {
    local d="${GOGO_E2E_LOCK_DIR:-$ROOT/cache}" real
    if [ -n "${GOGO_E2E_LOCK_DIR:-}" ]; then
        case "$d" in "$ROOT"/?*) ;; *) die "the lock dir must be under $ROOT: $d" 2 ;; esac
    fi
    # A link to a cache that isn't there yet (a fresh main checkout): make it.
    mkdir -p "$(readlink -m "$d")"
    real="$(readlink -f "$d")"
    if [ ! -d "$real" ] || [ "$(stat -c %u "$real")" != "$(id -u)" ]; then die "not the harness's own dir: $d" 3; fi
    if [ -n "${GOGO_E2E_LOCK_DIR:-}" ]; then
        [ "$real" = "$d" ] || die "a link on the way to $d" 3
    else
        case "$real" in */target/linux-e2e/cache) ;; *) die "$d leads to $real" 3 ;; esac
    fi
    CACHE_HOLDER="$real/.lock.holder"
    if [ -L "$real/.lock" ] || [ -L "$CACHE_HOLDER" ]; then die "$real/.lock is a link" 3; fi
    LOCK_REAL="$real"
}

# Who has the lock, from the holder file. Information only: a dead pid means
# the file is stale (a SIGKILL), and the flock decides.
holder_text() {
    local repo="" run="" pid="" since="" live="" idle="" k v
    if [ -f "$CACHE_HOLDER" ] && [ ! -L "$CACHE_HOLDER" ]; then
        while IFS='=' read -r k v; do
            case "$k" in
            repo) repo="$v" ;; run) run="$v" ;; pid) pid="$v" ;; started) since="$v" ;;
            live_until) live="$v" ;; live_idle) idle="$v" ;;
            esac
        done <"$CACHE_HOLDER"
    fi
    case "$pid" in '' | *[!0-9]*) printf 'held, but its holder left no note'; return 0 ;; esac
    local gone=""
    if ! kill -0 "$pid" 2>/dev/null; then gone=" (pid $pid is gone: the note may be stale)"; fi
    [ -z "$live" ] || live=", a live session: it ends after ${idle:-?} s with no command, by $live at the latest"
    printf 'held by %s, %s, pid %s since %s%s%s' "${repo:-?}" "${run:-?}" "$pid" "${since:-?}" "$live" "$gone"
}

write_holder() { # run-id
    local tmp="$CACHE_HOLDER.$$"
    { printf 'repo=%s\nrun=%s\npid=%s\nstarted=%s\n' "$REPO" "$1" "$$" "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
      [ -z "${LIVE_DEADLINE:-}" ] ||
          printf 'live_until=%s\nlive_idle=%s\n' "$(date -u -d "@$LIVE_DEADLINE" +%Y-%m-%dT%H:%M:%SZ)" "$LIVE_IDLE"
    } >"$tmp"
    mv -f "$tmp" "$CACHE_HOLDER" || rm -f "$tmp"
}

# Ours to remove only, and with the lock still held, so nobody's note is lost.
clear_holder() {
    [ -n "${CACHE_HOLDER:-}" ] && [ -f "$CACHE_HOLDER" ] && [ ! -L "$CACHE_HOLDER" ] || return 0
    if grep -qx "pid=$$" "$CACHE_HOLDER" 2>/dev/null; then rm -f "$CACHE_HOLDER"; fi
}

# Every input of a build, as JSON: its sha256 is the key. gogo's sources
# (gogo_identity) cover its compat DB, LAUNCHER_VERSION and the Proton and umu
# pins; the scratch's seed covers config.toml (no L4 yet); the build's sandbox
# code and tools cover the rest. A depot game is keyed on its download cache as it is:
# a newer build on GOG shows up once a fresh run fetches it.
install_key_inputs() {
    local gogo_id="" installer='null' depot='null' code
    if [ -n "$GOGO" ]; then gogo_id="$(gogo_identity)"; fi
    if [ -n "$INSTALLER_DIR" ]; then
        # The .exe and its .bin parts by name, size and mtime: hashing 70 GB isn't worth it.
        installer="$(find "$INSTALLER_DIR" -maxdepth 1 -type f -printf '%f %s %T@\n' | LC_ALL=C sort |
            jq -Rsc --arg f "${INSTALLER_FILE##*/}" '{file:$f, files:(split("\n") | map(select(. != "")))}')"
    elif [ -n "$GOGID" ]; then
        # Files only: a build touches its dirs' mtimes. None yet: an empty listing.
        depot="$( (cd "$CACHE/depot" && find "./$GOGID" -type f -printf '%P %s %T@\n' 2>/dev/null || true) | LC_ALL=C sort |
            sha256sum | cut -d' ' -f1)"
        depot="\"$depot\""
    fi
    code="$(declare -f seed_scratch sandbox_args sandbox_env start_sandbox software_setup | sha256sum | cut -d' ' -f1)"
    jq -cn --argjson format "$REUSE_FORMAT" --arg target "$TARGET" --arg gogo "$gogo_id" --arg umu "$UMU_PY" \
        --argjson cmd "$BUILD_CMD" --argjson installer "$installer" --argjson depot "$depot" \
        --arg tools "$TOOLS" --arg code "$code" --arg run "$RUN" \
        --arg config "$(cat "$GOGO_HOME_DIR/config.toml")" \
        --args '{format:$format, target:$target, gogo:$gogo, umu_python:$umu, build_command:$cmd,
                 installer:$installer, depot:$depot, tools:$tools, harness:$code, run_path:$run,
                 config_toml:$config, build_env:$ARGS.positional}' \
        "${SOFTWARE_ENV[@]}" "${SOFTWARE_COMPOSITOR_ENV[@]}" |
        # $GOGO is in the command; its identity is already in .gogo.
        jq -c --arg g "$GOGO" '.build_command |= map(if . == $g then "<gogo>" else . end)'
}

key_of() { sha256sum | cut -c1-16; }

# What gogo is, for the key. Its store path won't do: the package's src is
# the whole checkout, so a manifest edit is a new path. Its build reads only
# the workspace, compat/ (build.rs), the toolchain and the Nix that builds
# and wraps it, so those files' hash is. A --gogo outside the store: its bytes.
gogo_identity() {
    case "$GOGO" in
    /nix/store/*)
        (cd "$REPO" && find crates compat nix -type f -print0 | LC_ALL=C sort -z |
            xargs -0 sha256sum -- Cargo.toml Cargo.lock rust-toolchain.toml flake.nix flake.lock) |
            sha256sum | sed 's/^/src:/; s/ .*//'
        ;;
    *) sha256sum <"$GOGO" | sed 's/^/sha256:/; s/ .*//' ;;
    esac
}

# --- copies (#255) --------------------------------------------------------------
#
# How a base, or a live session's disk, is copied: COPY_MODE.
#   snapshot  btrfs subvolumes. The tree's gogo, home and xdg are each one, and
#             a copy is `subvolume snapshot`: no walk over the files. Where
#             $RUN and `to` are on one btrfs and the probe below works
#             unprivileged, creating, snapshotting and deleting one.
#   reflink   cp -a --reflink=always: shared extents, but every file is visited.
#   copy      cp -a, where there are no reflinks (CI's ext4).
# GOGO_E2E_COPY picks: auto (the default: the first that works), or snapshot
# (fails without), reflink or copy (never a subvolume, even on btrfs).
# Deleting needs no root: `rm -rf` empties a subvolume and rmdir(2) takes it
# (kernel 4.18+), without user_subvol_rm_allowed. A subvolume nested in another
# isn't in the outer one's snapshots, so the harness never snapshots a tree
# with one in it, only gogo, home and xdg themselves.

copy_ask() {
    COPY_ASK="${GOGO_E2E_COPY:-auto}"
    case "$COPY_ASK" in auto | snapshot | reflink | copy) ;;
    *) die "GOGO_E2E_COPY wants auto, snapshot, reflink or copy: $COPY_ASK" 2 ;; esac
}

is_subvol() { # dir: a btrfs subvolume's root (inode 256), not a link
    [ -d "$1" ] && [ ! -L "$1" ] && [ "$(stat -f -c %T "$1" 2>/dev/null)" = btrfs ] &&
        [ "$(stat -c %i "$1" 2>/dev/null)" = 256 ]
}

# Can a snapshot be made in `to` and from it into $RUN, and both deleted
# again, unprivileged? Prints why not.
probe_snapshot() { # to
    local to="$1" sv="$1/.probe-$$-sv" snap="$RUN/harness/.probe-snap" out="" ok=""
    if [ "$(stat -f -c %T "$to")" != btrfs ] || [ "$(stat -f -c %T "$RUN/harness")" != btrfs ]; then
        echo "not btrfs"; return 1
    fi
    if out="$(helper subvol create "$sv" 2>&1)" && printf 'probe\n' >"$sv/f" &&
        out="$(helper subvol snapshot "$sv" "$snap" 2>&1)" && [ "$(cat "$snap/f" 2>/dev/null)" = probe ]; then
        ok=1
    fi
    rm -rf "$sv" "$snap" 2>/dev/null || true
    if [ -e "$sv" ] || [ -e "$snap" ]; then
        echo "a probe subvolume won't go (rmdir takes an empty one from kernel 4.18): $sv $snap"; return 1
    fi
    [ -n "$ok" ] || { printf '%s\n' "${out:-the probe snapshot is empty}" | head -n 1; return 1; }
}

# The mode for copies between $RUN and `to`: sets COPY_MODE, and COPY_NOTE,
# why it isn't a snapshot.
probe_copy() { # [dir: default the bases']
    local to="${1:-$INSTALLED}" probe why
    COPY_NOTE=""
    if [ "$COPY_ASK" = auto ] || [ "$COPY_ASK" = snapshot ]; then
        if why="$(probe_snapshot "$to")"; then
            COPY_MODE=snapshot
            log "copies: btrfs snapshots ($to, GOGO_E2E_COPY=$COPY_ASK)"
            return 0
        fi
        if [ "$COPY_ASK" = snapshot ]; then
            INFRA_ERROR="GOGO_E2E_COPY=snapshot, but no snapshots between $RUN and $to: $why"
            exit 3
        fi
        COPY_NOTE="no snapshots: $why"
    else
        COPY_NOTE="GOGO_E2E_COPY=$COPY_ASK"
    fi
    COPY_MODE=copy
    if [ "$COPY_ASK" != copy ]; then
        probe="$to/.probe-$$"
        printf 'probe\n' >"$RUN/harness/.probe"
        if cp --reflink=always "$RUN/harness/.probe" "$probe" 2>/dev/null; then COPY_MODE=reflink
        else COPY_NOTE="$COPY_NOTE; no reflinks between $RUN and $to"; fi
        rm -f "$probe" "$RUN/harness/.probe"
    fi
    log "copies: $COPY_MODE ($COPY_NOTE)"
}

# The run's gogo, home and xdg into `to`/: what a base holds.
copy_run_tree() { # to
    local d
    for d in gogo home xdg; do
        copy_dir "$RUN/$d" "$1/$d" || return 1
    done
}

# One of those dirs to `to`, not there yet. A snapshot where `from` is a
# subvolume; else, in snapshot mode, a new subvolume with a reflinked copy in it.
copy_dir() { # from to
    if [ "$COPY_MODE" = snapshot ]; then
        if is_subvol "$1"; then helper subvol snapshot "$1" "$2"; return; fi
        helper subvol create "$2" && cp -a --reflink=always "$1/." "$2/"
        return
    fi
    mkdir "$2" && copy_tree "$1" "$2"
}

copy_tree() { # from to
    if [ "$COPY_MODE" = reflink ]; then cp -a --reflink=always "$1/." "$2/"
    else cp -a "$1/." "$2/"; fi
}

# A base we may copy from: ours, under $INSTALLED, no links, built for this
# run's path and key, and nothing in it ours-only or leading out (the helper's
# tree-check). Prints why not.
base_problem() { # base key
    local base="$1" key="$2" f
    case "$base" in "$INSTALLED"/*) ;; *) echo "not under $INSTALLED"; return 0 ;; esac
    for f in "$base" "$base/meta.json" "$base/tree"; do
        if [ -L "$f" ] || [ ! -e "$f" ] || [ "$(stat -c %u "$f")" != "$(id -u)" ]; then
            echo "$f: missing, a link or not ours"; return 0
        fi
    done
    [ "$(readlink -f "$base")" = "$base" ] || { echo "a link on its path"; return 0; }
    jq -e --arg k "$key" --arg r "$RUN" '.key == $k and .inputs.run_path == $r' "$base/meta.json" >/dev/null 2>&1 ||
        { echo "meta.json isn't for key $key at $RUN"; return 0; }
    tree_problem "$base/tree"
}

# The helper's tree-check, as one line; empty: none. A crash counts as one.
tree_problem() { # tree
    local out
    out="$(helper tree-check "$1" "$RUN" "$HOME" "$ROOT" 2>&1)" && return 0
    printf '%s' "${out:-tree-check failed}" | head -n 5 | tr '\n' ';'
}

# A copy-on-write copy of the base into this run's scratch: before a reused
# run's launch, and a live session's restore (#241), over what a life left.
restore_base() { # base
    local base="$1" t0 d
    t0="$(date +%s.%N)"
    # What seed_scratch made, or a game left: the base has the same dirs.
    # Proton leaves read-only ones. A life's snapshot goes aside, into
    # scratch's .trash, deleted with it: emptying one is a walk over its files.
    for d in gogo home xdg; do
        [ -e "$RUN/$d" ] || continue
        if [ "$COPY_MODE" = snapshot ] && is_subvol "$RUN/$d"; then
            mkdir -p "$RUN/.trash" && mv -T "$RUN/$d" "$RUN/.trash/$d-life${LIFE:-0}" || return 1
            continue
        fi
        chmod -R u+rwX "${RUN:?}/$d" 2>/dev/null || true
        rm -rf "${RUN:?}/$d" || return 1
    done
    if [ "$COPY_MODE" = snapshot ]; then
        for d in gogo home xdg; do copy_dir "$base/tree/$d" "$RUN/$d" || return 1; done
    else
        copy_tree "$base/tree" "$RUN"
    fi
    COPY_SECS="$(awk -v a="$t0" -v b="$(date +%s.%N)" 'BEGIN { printf "%.1f", b - a }')"
}

# Right after the build, before the game's sandbox starts: the state a fresh
# run's launch.sh would find. Under the key of the inputs as they are now (a
# first build fills the depot cache it's keyed on). Sets SAVED / SAVE_NOTE.
save_base() { # build-secs
    local inputs key base tmp problems
    inputs="$(install_key_inputs)"
    key="$(key_of <<<"$inputs")"
    base="$INSTALLED/$TARGET-$key"
    REUSE_KEY="$key" REUSE_BASE="$base"
    if [ -e "$base" ]; then SAVED=false SAVE_NOTE="a base for $key is already there"; return 0; fi
    tmp="$INSTALLED/.partial-$TARGET-$$"
    rm -rf "$tmp"
    mkdir -p "$tmp/tree"
    log "saving the installed base: $base ($COPY_MODE)"
    problems=""
    copy_run_tree "$tmp/tree" || problems="the copy failed (disk full?)"
    [ -n "$problems" ] || problems="$(tree_problem "$tmp/tree")"
    if [ -n "$problems" ]; then
        chmod -R u+rwX "$tmp" 2>/dev/null || true
        rm -rf "$tmp"
        SAVED=false SAVE_NOTE="not saved: $problems"
        log "installed base $SAVE_NOTE"
        return 0
    fi
    jq -n --arg key "$key" --argjson inputs "$inputs" --arg by "$RUN_ID" --arg results "$RESULTS" \
        --arg created "$(date -u +%FT%TZ)" --argjson secs "${1:-null}" --arg copy "$COPY_MODE" \
        '{key:$key, inputs:$inputs, built_by:$by, build_results:$results, created:$created,
          build_secs:$secs, copy:$copy}' >"$tmp/meta.json"
    mv -T "$tmp" "$base"
    SAVED=true SAVE_NOTE=""
}

# A base saved as plain dirs (before #255, or with GOGO_E2E_COPY=reflink|copy),
# met in snapshot mode: its gogo, home and xdg become subvolumes, once, with
# the cache lock held. The new tree is a reflinked copy beside the old one
# (.tree-new-*, which cleanup sweeps), swapped in with one rename, and the old
# tree goes. Any failure leaves the base as it was. Sets COPY_NOTE.
convert_base() { # base
    local base="$1" new="$1/.tree-new-$$" d plain=""
    for d in gogo home xdg; do is_subvol "$base/tree/$d" || plain=1; done
    [ -n "$plain" ] || return 0
    log "the base's tree is plain dirs: making them subvolumes, once"
    local failed=""
    if mkdir "$new"; then
        for d in gogo home xdg; do copy_dir "$base/tree/$d" "$new/$d" || { failed=1; break; }; done
    else failed=1; fi
    if [ -n "$failed" ]; then
        remove_tree "$new" || true
        COPY_NOTE="couldn't make the base's dirs subvolumes: reflinked into this run"
        log "$COPY_NOTE"
        return 0
    fi
    # The tree's own mode and mtime, as a copy keeps the rest's.
    chmod --reference="$base/tree" "$new" && touch -r "$base/tree" "$new" || failed=1
    if [ -n "$failed" ] || ! mv --exchange -T "$new" "$base/tree"; then
        remove_tree "$new" || true
        COPY_NOTE="couldn't swap the base's tree: reflinked into this run"
        log "$COPY_NOTE"
        return 0
    fi
    remove_tree "$new" || log "couldn't delete the base's old tree: $new (cleanup sweeps it)"
    local tmp="$base/.meta-$$"
    if jq --arg at "$(date -u +%FT%TZ)" '.copy = "snapshot" | .converted = $at' "$base/meta.json" >"$tmp"; then
        mv -f "$tmp" "$base/meta.json"
    else rm -f "$tmp"; fi
    COPY_NOTE="the base was plain dirs: made subvolumes"
}

remove_tree() { # dir under $INSTALLED
    case "$1" in "$INSTALLED"/?*) ;; *) log "not deleting $1"; return 1 ;; esac
    [ ! -L "$1" ] || { log "not deleting a link: $1"; return 1; }
    chmod -R u+rwX "$1" 2>/dev/null || true
    rm -rf "$1"
    gone_or_say "$1"
}

# Deleted, or say what's left: a subvolume rm -rf couldn't take (a read-only
# snapshot, a kernel before 4.18) wants `btrfs subvolume delete` by its owner.
gone_or_say() { # path
    [ -e "$1" ] || return 0
    local sv
    sv="$(find "$1" -maxdepth 4 -type d -inum 256 2>/dev/null | head -n 3 | tr '\n' ' ')"
    log "couldn't delete $1${sv:+ (subvolumes left in it: $sv)}"
    return 1
}

# cleanup's list: name, size, when, what built it, how it's copied. du counts
# extents a reflink or a snapshot shares in full, so on btrfs it's an upper bound.
list_installed() {
    local d n=0 total=0 bytes
    for d in "$INSTALLED"/*/; do
        [ -d "$d" ] || continue
        d="${d%/}"
        bytes="$(du -sb "$d" | cut -f1)"
        total=$((total + bytes))
        n=$((n + 1))
        printf '%s\t%s\t%s\t%s\t%s\n' "${d##*/}" "$(numfmt --to=iec "$bytes")" \
            "$(jq -r '.created // "?"' "$d/meta.json" 2>/dev/null || echo '?')" \
            "$(jq -r '.built_by // "?"' "$d/meta.json" 2>/dev/null || echo '?')" \
            "$(if is_subvol "$d/tree/gogo"; then echo snapshot; else echo plain; fi)"
    done
    log "installed bases in $INSTALLED: $n, $(numfmt --to=iec "$total") (du; reflinks and snapshots share extents)"
}


cmd_run() {
    local target="" manifest="" gpu="" expect_gpu="" secs=600 out="" installer=""
    GOGO="" KEEP=0 FRESH=0 REUSE=0 INSTALLER_DIR="" INSTALLER_FILE=""
    # A live session's own (#241): only through `live start`.
    local -a live_env=() live_args=()
    LIVE_IDLE=900 LIVE_MAX=5400 LIVE_GAP=10 LIVE_MATCH=15
    while [ $# -gt 0 ]; do
        case "$1" in
        --env | --arg | --idle-secs | --max-secs | --max-gap | --match-pct)
            [ -n "${LIVE:-}" ] || { log "$1 is for live sessions: live start"; usage; }
            case "$1" in
            --env) live_env+=("${2:?}") ;;
            --arg) live_args+=("${2?}") ;;
            --idle-secs) LIVE_IDLE="${2:?}" ;;
            --max-secs) LIVE_MAX="${2:?}" ;;
            --max-gap) LIVE_GAP="${2:?}" ;;
            --match-pct) LIVE_MATCH="${2:?}" ;;
            esac
            shift 2 ;;
        --manifest) manifest="${2:?}"; shift 2 ;;
        --gpu) gpu="${gpu:+$gpu,}${2:?}"; shift 2 ;;
        --expect-gpu) expect_gpu="${2:?}"; shift 2 ;;
        --secs) secs="${2:?}"; shift 2 ;;
        --gogo) GOGO="${2:?}"; shift 2 ;;
        --installer) installer="${2:?}"; shift 2 ;;
        --out) out="${2:?}"; shift 2 ;;
        --keep) KEEP=1; shift ;;
        --no-cache) FRESH=1; shift ;;
        --reuse-install) REUSE=1; shift ;;
        -h | --help) usage ;;
        -*) log "unknown option: $1"; usage ;;
        *) [ -z "$target" ] || usage; target="$1"; shift ;;
        esac
    done
    [ -n "$target" ] || usage
    case "$secs" in '' | *[!0-9]*) die "--secs wants a number" 2 ;; esac
    case "$target" in *[!A-Za-z0-9._-]* | .*) die "not a game id: $target" 2 ;; esac
    [ -n "$manifest" ] || manifest="$REPO/e2e/$target.toml"
    if [ -n "${LIVE:-}" ]; then
        live_options
        # A game with no manifest yet: its window and exit take the defaults.
        if [ ! -f "$manifest" ] && [ "$manifest" = "$REPO/e2e/$target.toml" ]; then
            case "$target" in *[!0-9]* | 0*) die "no manifest for $target, and it isn't a GOG id" 2 ;; esac
            manifest="$LIVE_RESULTS/live/base.toml"
            printf 'gogid = %s\n' "$target" >"$manifest"
        fi
    fi
    [ -f "$manifest" ] || die "no manifest: $manifest" 2
    TARGET="$target"
    [ "$REUSE" = 0 ] || [ "$FRESH" = 0 ] || die "--reuse-install keeps bases in the cache: not with --no-cache" 2
    copy_ask
    if [ -n "$installer" ]; then
        installer="$(readlink -f -- "$installer")"
        [ -f "$installer" ] || die "--installer: no such file" 2
        case "$installer" in *.exe | *.EXE) ;; *) die "--installer wants a setup .exe: $installer" 2 ;; esac
        INSTALLER_DIR="${installer%/*}"
        INSTALLER_FILE="$INSTALLER_MOUNT/${installer##*/}"
    fi

    resolve_tools
    # The builds need nix; everything after them runs on the pinned PATH.
    local needs_gogo=1
    if [ -n "${LIVE:-}" ]; then
        # Its steps are the session's to find; --env and --arg join its [override].
        MANIFEST_JSON="$(PATH="$TOOLS/bin" helper manifest --live "$manifest")" || die "bad manifest" 2
        MANIFEST_JSON="$(live_override "$MANIFEST_JSON" "${#live_env[@]}" "${live_env[@]+"${live_env[@]}"}" \
            "${live_args[@]+"${live_args[@]}"}")" || exit
    else
        MANIFEST_JSON="$(PATH="$TOOLS/bin" helper manifest "$manifest")" || die "bad manifest" 2
    fi
    GOGID="$(PATH="$TOOLS/bin" jq -r '.gogid // empty' <<<"$MANIFEST_JSON")"
    TITLE="$(PATH="$TOOLS/bin" jq -r .title <<<"$MANIFEST_JSON")"
    if [ -n "$GOGID" ]; then
        [ "$GOGID" = "$target" ] || die "manifest is for $GOGID, not $target" 2
    else
        needs_gogo=0
        GOGID=""
    fi
    # Only now: live_start returns once there is a state, and a usage error
    # before it must reach live start's exit.
    [ -z "${LIVE:-}" ] || live_state starting "building gogo, then the game or a copy of its base"
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
    if [ -n "$INSTALLER_FILE" ] && [ -z "$GOGID" ] && [ "$(jq -c .build_command <<<"$MANIFEST_JSON")" = null ]; then
        die "--installer: this manifest builds nothing" 2
    fi
    if [ -n "$GOGID" ]; then
        # The installer lane reads the id from the tree it installs (design 18 §10).
        BUILD_CMD="$(jq -cn --arg g "$GOGO" --arg i "${INSTALLER_FILE:-$GOGID}" '[$g,"linux-app",$i,"--yes"]')"
    else
        BUILD_CMD="$(jq -c .build_command <<<"$MANIFEST_JSON")"
    fi
    [ "$REUSE" = 0 ] || [ "$BUILD_CMD" != null ] || die "--reuse-install: this manifest builds nothing" 2
    [ "$REUSE" = 0 ] || installed_dir

    # One run at a time over a cache, whichever checkout: they share the cache
    # and the GPU.
    mkdir -p "$ROOT/runs"
    [ -z "${LIVE:-}" ] || live_state waiting "for the cache lock"
    cache_lock run
    # A live session's hard cap counts from the lock: what other runs may wait
    # for it, and what its holder note says.
    [ -z "${LIVE:-}" ] || LIVE_DEADLINE=$(($(date +%s) + LIVE_MAX))
    remove_dead_runs

    local stamp slug
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    slug="$target-$stamp-$$"
    [ -z "${LIVE:-}" ] || slug="$LIVE_ID"
    SLUG="$slug"
    RUN_ID="linux-e2e-$slug"
    write_holder "$RUN_ID"
    RUN="$ROOT/runs/$slug"
    if [ "$REUSE" = 1 ]; then
        # The path the base was built at, the same for every run of it (above).
        RUN="$INSTALLED/.runs/$target"
        remove_dead_reuse_runs
        [ ! -e "$RUN" ] || die "$RUN is in the way (a kept run killed hard?): delete it by hand" 3
    fi
    RESULTS="${out:-$ROOT/results}/$slug"
    [ -z "${LIVE:-}" ] || RESULTS="$LIVE_RESULTS"
    mkdir -p "$RESULTS/shots" "$RUN"
    RESULTS="$(readlink -f "$RESULTS")"
    INTERRUPTED="" INTERRUPT_COUNT=0 SANDBOX_PID="" NET_PID="" MODE_NOW=build
    trap teardown EXIT
    if [ -n "${LIVE:-}" ]; then
        live_register
        log "live session $LIVE_ID: idle limit $LIVE_IDLE s, ends by $(date -d "@$LIVE_DEADLINE" +%T) at the latest"
    fi
    trap 'on_signal INT' INT
    trap 'on_signal TERM' TERM
    trap 'on_signal HUP' HUP
    printf '%s\n' "$$" >"$RUN/.owner"
    if [ "$KEEP" = 1 ]; then : >"$RUN/.kept"; fi

    GOGO_HOME_DIR="$RUN/gogo"
    PREFIX=""
    if [ -n "$GOGID" ]; then PREFIX="$GOGO_HOME_DIR/games/$GOGID/prefix"; fi
    if [ "$FRESH" = 1 ]; then CACHE="$RUN/cache"; else CACHE="$ROOT/cache"; fi
    seed_scratch
    log "scratch: $RUN"
    log "results: $RESULTS"
    REUSED=0 REUSE_KEY="" REUSE_BASE="" COPY_MODE="" COPY_NOTE="" COPY_SECS="" SAVED="" SAVE_NOTE=""
    if [ "$REUSE" = 1 ]; then
        probe_copy
        REUSE_KEY="$(install_key_inputs | key_of)"
        REUSE_BASE="$INSTALLED/$target-$REUSE_KEY"
        if [ -e "$REUSE_BASE" ] || [ -L "$REUSE_BASE" ]; then
            local problem
            problem="$(base_problem "$REUSE_BASE" "$REUSE_KEY")"
            if [ -n "$problem" ]; then
                INFRA_ERROR="won't reuse $REUSE_BASE: $problem"
                exit 3
            fi
            log "reusing the installed base $REUSE_BASE ($COPY_MODE)"
            [ "$COPY_MODE" != snapshot ] || convert_base "$REUSE_BASE"
            restore_base "$REUSE_BASE"
            REUSED=1
            log "copied in $COPY_SECS s: no build"
        else
            log "no installed base for key $REUSE_KEY: building, then saving one"
        fi
    fi

    local forbidden
    forbidden="$(jq -cn --arg h "$HOME" --arg r "/run/user/$(id -u)" '[
        "\($h)/.local/share/applications", "\($h)/.local/share/Steam",
        "\($h)/.local/share/gogo", "\($h)/.local/share/umu", "\($h)/.steam",
        "\($h)/.var/app/com.valvesoftware.Steam", "\($h)/Downloads", "\($h)/.cache/gogo",
        "\($r)/bus", "\($r)/wayland-1", "\($r)/pulse", "\($r)/pipewire-0",
        "/tmp/.X11-unix/X1"]')"
    # Its host path is hidden from both phases; only the build has the mount.
    [ -z "$INSTALLER_DIR" ] || forbidden="$(jq -c --arg d "$INSTALLER_DIR" '. + [$d]' <<<"$forbidden")"
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

    local build_cmd="$BUILD_CMD"
    if [ "$build_cmd" != null ] && [ "$REUSED" = 0 ]; then
        log "building the game on a headless display (log: $RESULTS/build.log)"
        MODE_NOW=build
        local build_extra support_log=""
        # build.rs: <game state>/<INSTALL_LOGS>/support/<INNO_LOG>, once the build has moved it in place
        [ -z "$GOGID" ] || support_log="$GOGO_HOME_DIR/games/$GOGID/install/support/inno.log"
        build_extra="$(jq -cn --argjson c "$build_cmd" --arg cwd "$RUN/home" --arg s "$support_log" \
            --arg i "$INSTALLER_FILE" --arg g "$GOGO_HOME_DIR/games" \
            '{build_command:$c, launch_cwd:$cwd, support_log:$s, installer:$i, games_dir:$g}')"
        drive_config "$RUN/build.json" "$build_extra" software 'llvmpipe|softpipe|lavapipe' "" '[]' \
            "${SOFTWARE_ENV[@]/#/@e:}"
        start_sandbox build "$RESULTS/build-sandbox.log" python3 -I "$RUN/harness/helper.py" drive "$RUN/build.json"
        local build_rc=0
        [ -z "${LIVE:-}" ] || live_state building "the game, on a display of its own (build.log)"
        supervise "${LIVE_DEADLINE:-0}" || build_rc=$?
        if [ "$build_rc" = 3 ]; then
            INFRA_ERROR="no build display: $(jq -r '[.checks[] | select(.ok | not) | .name + ": " + .detail][0] // "see build-sandbox.log"' "$RESULTS/build-result.json" 2>/dev/null || tail -n 3 "$RESULTS/build-sandbox.log" | tr '\n' ' ')"
            exit 3
        fi
        if [ "$build_rc" != 0 ] || [ -n "$INTERRUPTED" ]; then
            BUILD_FAILED="the build failed (exit $build_rc), see $RESULTS/build.log"
            exit 1
        fi
        if [ -n "$INSTALLER_FILE" ] && [ -n "$GOGID" ] && [ ! -e "$GOGO_HOME_DIR/games/$GOGID/launch.sh" ]; then
            BUILD_FAILED="the installer didn't build game $GOGID, see $RESULTS/build.log"
            exit 1
        fi
        [ "$REUSE" = 0 ] || save_base "$(jq '.secs // null' "$RESULTS/build-result.json" 2>/dev/null || echo null)"
    fi

    local launch launch_cwd doctor='null' game_log="" pre_launch='[]'
    if [ -n "$GOGID" ]; then
        launch="$(jq -cn --arg l "$GOGO_HOME_DIR/games/$GOGID/launch.sh" '[$l]')"
        launch_cwd="$GOGO_HOME_DIR/games/$GOGID"
        doctor="$(jq -cn --arg g "$GOGO" --arg i "$GOGID" '[$g,"linux-app","doctor",$i,"--json"]')"
        # The manifest's [override] (#234), written by gogo itself before the launch.
        pre_launch="$(jq -c --arg g "$GOGO" --arg i "$GOGID" '.override as $o | [
            (if ($o.env | length) > 0
             then [$g,"linux-app","env","set",$i] + ($o.env | to_entries | map("\(.key)=\(.value)"))
             else empty end),
            (if $o.extra_args != null then [$g,"linux-app","args",$i,"--"] + $o.extra_args else empty end)
        ]' <<<"$MANIFEST_JSON")"
        game_log="$GOGO_HOME_DIR/logs/$GOGID/latest.log"
    else
        launch="$(jq -c .command <<<"$MANIFEST_JSON")"
        launch_cwd="$RUN/home"
    fi
    local game_extra
    game_extra="$(jq -cn --argjson l "$launch" --arg cwd "$launch_cwd" --argjson d "$doctor" --arg g "$game_log" \
        --argjson f "$forbidden" --arg i "${INSTALLER_FILE:+$INSTALLER_MOUNT}" --argjson p "$pre_launch" \
        '{launch:$l, launch_cwd:$cwd, doctor:$d, game_log:$g, pre_launch:$p,
          forbidden_paths:($f + (if $i == "" then [] else [$i] end))}')"
    if [ -n "${LIVE:-}" ]; then
        live_game_phase "$game_extra"
        exit 0
    fi
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

# The same, for runs on installed bases. Only with the cache lock held: then no
# run of any checkout is using one.
remove_dead_reuse_runs() {
    local d
    for d in "$INSTALLED"/.runs/*/; do
        [ -d "$d" ] && [ ! -L "${d%/}" ] && [ ! -e "$d/.kept" ] || continue
        log "removing dead run's scratch: $d"
        chmod -R u+rwX "$d" 2>/dev/null || true
        rm -rf "$d"
    done
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
    local prune=() name d stop_live=0
    while [ $# -gt 0 ]; do
        case "$1" in
        --prune-installed) prune+=("${2:?}"); shift 2 ;;
        --stop-live) stop_live=1; shift ;;
        *) usage ;;
        esac
    done
    resolve_tools
    use_tools
    mkdir -p "$ROOT/runs"
    # Before the lock: a live session holds it for as long as it lives.
    live_reap "$stop_live"
    # Whoever holds the cache, in this checkout or another, has scratch and
    # saves in it: no sweeping or pruning meanwhile, and no waiting either.
    cache_lock cleanup
    trap clear_holder EXIT
    remove_dead_runs
    installed_dir
    remove_dead_reuse_runs
    # A save no run finished: with the lock held, nobody is writing one.
    for d in "$INSTALLED"/.partial-* "$INSTALLED"/.probe-* "$INSTALLED"/*/.tree-new-* "$INSTALLED"/*/.meta-*; do
        [ -e "$d" ] || continue
        log "removing an unfinished base: $d"
        remove_tree "$d" || true
    done
    for name in "${prune[@]+"${prune[@]}"}"; do
        if [ "$name" = all ]; then
            for d in "$INSTALLED"/*/; do
                [ -d "$d" ] || continue
                log "pruning ${d%/}"
                remove_tree "${d%/}"
            done
            continue
        fi
        case "$name" in */* | .* | '') die "--prune-installed wants a name cleanup lists, or all: $name" 2 ;; esac
        [ -d "$INSTALLED/$name" ] || die "no installed base $name" 2
        log "pruning $INSTALLED/$name"
        remove_tree "$INSTALLED/$name"
    done
    list_installed
    for d in "$INSTALLED"/.runs/*/; do
        [ -d "$d" ] && log "kept, delete it by hand: ${d%/}"
    done
    return 0
}

# --- live sessions (#241) -------------------------------------------------------
#
# `live start` runs this script's run in the background, from a copy of it, as
# the session's supervisor: the same lock, build or reused base, sandbox and
# teardown as a run, but the game phase is a series of lives. In each, the
# driver serves the session's socket ($RUN/live/ctl.sock) instead of running a
# manifest's steps. A restore ends a life (LIVE_RELAUNCH); the supervisor rolls
# the disk back to the launch (restore_base) and starts the next, whose driver
# replays the checkpoint's path. Everything the session records is in
# $RESULTS/live/: state.json, session.json (the path and the checkpoints),
# journal.jsonl (every command), and on the way out manifest.toml, the export.
# The registry, one file per session beside the cache lock (live/<id>), is how
# a client, or `cleanup`, finds it.

LIVE_RELAUNCH=10

live_options() {
    local n
    for n in "$LIVE_IDLE" "$LIVE_MAX"; do
        case "$n" in '' | *[!0-9]* | 0) die "--idle-secs and --max-secs want a whole number of seconds" 2 ;; esac
    done
    for n in "$LIVE_GAP" "$LIVE_MATCH"; do
        [[ "$n" =~ ^[0-9]+(\.[0-9]+)?$ ]] || die "--max-gap and --match-pct want a number" 2
    done
}

# --env K=V and --arg A into the manifest's [override] (#250): through gogo,
# before each launch, as a manifest's are; the export keeps them.
live_override() { # manifest-json n-env env… args…
    local m="$1" n="$2" kv
    shift 2
    local -a envs=("${@:1:$n}") args=("${@:$((n + 1))}")
    for kv in "${envs[@]+"${envs[@]}"}"; do
        [[ "$kv" =~ ^[A-Za-z_][A-Za-z0-9_]*= ]] || die "--env wants NAME=value: $kv" 2
    done
    if [ $((${#envs[@]} + ${#args[@]})) -gt 0 ] && [ "$(PATH="$TOOLS/bin" jq -r '.gogid // empty' <<<"$m")" = "" ]; then
        die "--env and --arg are for a game (a gogid), not a command" 2
    fi
    # On stdin, NUL-joined: jq would take an --arg's `--launcher-skip` for its own.
    local words='[]'
    [ $((${#envs[@]} + ${#args[@]})) = 0 ] ||
        words="$(printf '%s\0' "${envs[@]+"${envs[@]}"}" "${args[@]+"${args[@]}"}" |
            PATH="$TOOLS/bin" jq -Rsc 'split("\u0000") | .[:-1]')"
    PATH="$TOOLS/bin" jq -c --argjson n "$n" --argjson w "$words" '
        ($w[:$n] | map(capture("^(?<key>[^=]+)=(?<value>.*)$")) | from_entries) as $e
        | $w[$n:] as $a
        | .override.env += $e
        | if ($a | length) > 0 then .override.extra_args = $a else . end' <<<"$m"
}

live_registry() { lock_dir; LIVE_REG="$LOCK_REAL/live"; mkdir -p "$LIVE_REG"; }

# A process's start time, in clock ticks since boot (/proc/PID/stat field 22):
# with its pid, who it is. Fields after the ")" of comm, which may hold spaces.
proc_start() { # pid
    local s
    s="$(cat "/proc/$1/stat" 2>/dev/null)" || return 1
    s="${s##*) }"
    # shellcheck disable=SC2086 # split into fields on purpose
    set -- $s
    [ -n "${20:-}" ] && printf '%s\n' "${20}"
}

# Whether the pid in a registry entry is still that session's supervisor: the
# same start time, and its copy of this script serving __live-serve. A pid
# alone isn't: after a SIGKILL, an OOM or a reboot the entry stays, and the
# pid may be anyone's now (a browser, the compositor). No start: not ours.
live_owner() { # pid start id
    local pid="$1" start="$2" id="$3" cmd
    case "$pid" in '' | *[!0-9]*) return 1 ;; esac
    [ -n "$start" ] && [ "$(proc_start "$pid")" = "$start" ] || return 1
    cmd="$(tr '\0' '\n' <"/proc/$pid/cmdline" 2>/dev/null)" || return 1
    # bash, then its copy of the script (results/<id>/live/), then __live-serve.
    [[ $'\n'"$cmd"$'\n' == *"/$id/live/linux-e2e.sh"$'\n__live-serve\n'* ]]
}

# The session's registry entry: who serves it and where it keeps things.
live_register() {
    live_registry
    local tmp="$LIVE_REG/.$LIVE_ID.$$"
    printf 'pid=%s\nstart=%s\nrepo=%s\nresults=%s\ntools=%s\nrun=%s\ndeadline=%s\nidle=%s\n' \
        "$$" "$(proc_start $$)" "$REPO" "$LIVE_RESULTS" "$TOOLS" "${RUN:-}" "${LIVE_DEADLINE:-}" "$LIVE_IDLE" >"$tmp"
    mv -f "$tmp" "$LIVE_REG/$LIVE_ID"
}

live_unregister() {
    [ -n "${LIVE_REG:-}" ] && [ -f "$LIVE_REG/$LIVE_ID" ] || return 0
    if grep -qx "pid=$$" "$LIVE_REG/$LIVE_ID" 2>/dev/null; then rm -f "${LIVE_REG:?}/${LIVE_ID:?}"; fi
}

live_state() { # state detail
    local f="$LIVE_RESULTS/live/state.json"
    PATH="$TOOLS/bin" jq -n --arg s "$1" --arg d "${2:-}" --argjson life "${LIFE:-0}" \
        '{state:$s, detail:$d, life:$life, at:(now | . * 10 | round / 10)}' >"$f.tmp$$" && mv -f "$f.tmp$$" "$f"
}

# The session's exit before a run's teardown was in place (usage, the lock wait).
live_early_exit() {
    local status=$?
    [ -z "${LOCK_WAITER:-}" ] || kill "$LOCK_WAITER" 2>/dev/null || true
    live_state stopped "ended before the game phase (exit $status): see session.log"
    live_unregister
    clear_holder
    exit "$status"
}

# The lives: launch, serve, and on a restore roll the disk back and go again.
live_game_phase() { # game-extra-json
    local game_extra="$1" rc extra
    LIFE=1
    live_snapshot
    while :; do
        extra="$(jq -c --argjson life "$LIFE" --arg id "$LIVE_ID" --arg dir "$RUN/live" --arg r "$RESULTS/live" \
            --argjson idle "$LIVE_IDLE" --argjson gap "$LIVE_GAP" --argjson match "$LIVE_MATCH" \
            '. + {live:{id:$id, life:$life, dir:$dir, session:($r + "/session.json"),
                        journal:($r + "/journal.jsonl"), state:($r + "/state.json"),
                        idle_secs:$idle, max_gap:$gap, match_pct:$match}}' <<<"$game_extra")"
        drive_config "$RUN/drive.json" "$extra" "$GPU_LABEL" "$GPU_EXPECT" "${GPU_WANT:-}" "$GPUS_JSON" \
            "${GPU_NODES[@]/#/@n:}" "${GAME_ENV[@]/#/@e:}"
        log "life $LIFE: launching in headless sway ($GPU_LABEL)"
        MODE_NOW=game
        live_state launching "life $LIFE: the sandbox"
        start_sandbox game "$RESULTS/sandbox.log" python3 -I "$RUN/harness/helper.py" drive "$RUN/drive.json"
        rc=0
        supervise "$LIVE_DEADLINE" || rc=$?
        if [ "$rc" = "$LIVE_RELAUNCH" ] && [ -z "$INTERRUPTED" ] && [ -z "${TIMED_OUT:-}" ]; then
            live_state restoring "life $LIFE ended: rolling the disk back to the launch"
            sweep
            if [ -n "$SWEPT_LEFT" ]; then
                INFRA_ERROR="a restore couldn't end life $LIFE: $(printf '%s' "$SWEPT_LEFT" | tr '\n' ';')"
                exit 3
            fi
            live_keep_life "$LIFE"
            restore_base "$LIVE_DISK" || { INFRA_ERROR="a restore couldn't roll the disk back"; exit 3; }
            log "life $LIFE: rolled back in $COPY_SECS s"
            LIFE=$((LIFE + 1))
            continue
        fi
        [ "$rc" = 0 ] || [ -n "$INTERRUPTED" ] || [ -n "${TIMED_OUT:-}" ] || {
            [ -s "$RESULTS/result.json" ] || INFRA_ERROR="sandbox exited $rc before the driver reported: $(tail -n 3 "$RESULTS/sandbox.log" | tr '\n' ' ')"
        }
        return 0
    done
}

# The disk as it is before the first launch, for every restore: an installed
# base when there is one (only ever read), else a copy-on-write snapshot in
# scratch (a full copy where there are no reflinks).
live_snapshot() {
    if [ "${REUSED:-}" = 1 ] || [ "${SAVED:-}" = true ]; then
        LIVE_DISK="$REUSE_BASE"
        log "restores roll back to the installed base $LIVE_DISK"
        return 0
    fi
    LIVE_DISK="$RUN/live/disk"
    mkdir -p "$LIVE_DISK/tree"
    probe_copy "$RUN/live"
    local t0="$SECONDS"
    copy_run_tree "$LIVE_DISK/tree" || { INFRA_ERROR="couldn't snapshot the disk for restores (disk full?)"; exit 3; }
    log "restores roll back to a snapshot of the launch ($COPY_MODE, $((SECONDS - t0)) s)"
}

# A life's own files, before the next one writes them again.
live_keep_life() { # life
    local f
    for f in result.json game.out sway.log audio.log sandbox.log doctor-before.json doctor-after.json; do
        [ ! -e "$RESULTS/$f" ] || mv -f "$RESULTS/$f" "$RESULTS/life$1-$f"
    done
}

# On the way out (teardown): the export, then out of the registry.
live_finish() {
    local s="$RESULTS/live/session.json"
    if [ -s "$s" ]; then
        helper live-export "$s" >"$RESULTS/live/manifest.toml" 2>"$RESULTS/live/export.err" ||
            log "no export: $(cat "$RESULTS/live/export.err")"
        [ -s "$RESULTS/live/manifest.toml" ] || rm -f "$RESULTS/live/manifest.toml"
    fi
    live_state stopped "the session ended: summary.json"
    live_unregister
}

# summary.json's `live`: null for a run.
live_json() {
    [ -n "${LIVE:-}" ] || { echo null; return 0; }
    local s="$RESULTS/live/session.json"
    [ -s "$s" ] || s=/dev/null
    jq -cs --arg id "$LIVE_ID" --argjson lives "${LIFE:-0}" --arg r "$RESULTS/live" \
        --argjson idle "$LIVE_IDLE" --argjson max "$LIVE_MAX" '
        (.[0] // {}) as $s
        | {id:$id, lives:$lives, idle_secs:$idle, max_secs:$max,
           steps:([($s.path // [])[] | select(.step)] | length),
           checkpoints:(($s.checkpoints // {}) | keys), restores:($s.restores // []),
           last_replay:($s.last_replay // null), session:($r + "/session.json"),
           journal:($r + "/journal.jsonl"),
           export:(if $s.path then $r + "/manifest.toml" else null end)}' "$s"
}

# --- live: the commands --------------------------------------------------------

live_start() {
    local target="" out="" a prev=""
    for a in "$@"; do
        case "$prev" in --out) out="$a" ;; esac
        case "$prev" in --manifest | --gpu | --expect-gpu | --secs | --gogo | --installer | --out | --env | --arg | \
            --idle-secs | --max-secs | --max-gap | --match-pct) prev=""; continue ;; esac
        case "$a" in -h | --help) usage ;; -*) ;; *) [ -n "$target" ] || target="$a" ;; esac
        prev="$a"
    done
    [ -n "$target" ] || usage
    case "$target" in *[!A-Za-z0-9._-]* | .*) die "not a game id: $target" 2 ;; esac
    resolve_tools
    local id results
    id="$target-$(date -u +%Y%m%dT%H%M%SZ)-$$"
    results="${out:-$ROOT/results}/$id"
    mkdir -p "$results/live"
    results="$(readlink -f "$results")"
    # Its own copy of this script: the checkout may change under a long session.
    cp "$SELF" "$results/live/linux-e2e.sh"
    GOGO_E2E_REPO="$REPO" GOGO_E2E_TOOLS="$TOOLS" GOGO_E2E_LIVE_ID="$id" GOGO_E2E_LIVE_RESULTS="$results" \
        setsid "$TOOLS/bin/bash" "$results/live/linux-e2e.sh" __live-serve "$@" \
        </dev/null >"$results/live/serve.out" 2>"$results/session.log" &
    local pid=$! i
    for i in $(seq 600); do
        [ ! -s "$results/live/state.json" ] || break
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.1
    done
    # Ended already: its early exit writes "stopped", maybe before it's gone.
    if ! kill -0 "$pid" 2>/dev/null ||
        [ "$(PATH="$TOOLS/bin" jq -r .state "$results/live/state.json" 2>/dev/null)" = stopped ]; then
        local code=0
        wait "$pid" || code=$?
        sed 's/^/  /' "$results/session.log" >&2
        die "the session didn't start (exit $code)" "$code"
    fi
    PATH="$TOOLS/bin" jq -n --arg id "$id" --arg r "$results" --slurpfile s "$results/live/state.json" \
        '{session:$id, results:$r, state:$s[0].state, detail:$s[0].detail,
          next:"live \($id) status --wait 900, then screenshot, key, …, stop"}'
    printf '%s\n' "$id"
}

# The session named, from its registry entry; after it ended, from its results
# (export, log and status still work). Sets L_PID (only while it's still the
# session's supervisor: live_owner), L_START, L_ID, L_RESULTS, L_RUN, L_TOOLS.
live_find() { # id-or-results-dir
    local id="$1" f k v
    L_PID="" L_START="" L_RESULTS="" L_RUN="" L_TOOLS="" L_REG="" L_ID="${id%/}"; L_ID="${L_ID##*/}"
    if [ -d "$id/live" ]; then
        L_RESULTS="$(readlink -f "$id")"
    else
        case "$id" in *[!A-Za-z0-9._-]* | .* | '') die "not a session id: $id" 2 ;; esac
        live_registry
        f="$LIVE_REG/$id"
        if [ -f "$f" ] && [ ! -L "$f" ]; then
            L_REG="$f"
            while IFS='=' read -r k v; do
                case "$k" in
                pid) L_PID="$v" ;; start) L_START="$v" ;; results) L_RESULTS="$v" ;; run) L_RUN="$v" ;; tools) L_TOOLS="$v" ;;
                esac
            done <"$f"
        elif [ -d "$ROOT/results/$id/live" ]; then
            L_RESULTS="$ROOT/results/$id"
        else
            die "no live session $id (live list)"
        fi
    fi
    if [ -z "$L_TOOLS" ]; then resolve_tools; L_TOOLS="$TOOLS"; fi
    TOOLS="$L_TOOLS"
    use_tools
    [ -z "$L_PID" ] || live_owner "$L_PID" "$L_START" "$L_ID" || L_PID=""
}

live_list() {
    resolve_tools
    use_tools
    live_registry
    local f k v pid start results state
    for f in "$LIVE_REG"/*; do
        [ -f "$f" ] || continue
        pid="" start="" results=""
        while IFS='=' read -r k v; do
            case "$k" in pid) pid="$v" ;; start) start="$v" ;; results) results="$v" ;; esac
        done <"$f"
        if live_owner "$pid" "$start" "${f##*/}"; then
            state="$(jq -r '"\(.state): \(.detail)"' "$results/live/state.json" 2>/dev/null || echo '?')"
            printf '%s\t%s\n' "${f##*/}" "$state"
        else
            printf '%s\tgone (its supervisor died; cleanup clears it)\n' "${f##*/}"
        fi
    done
}

cmd_live() {
    [ $# -ge 1 ] || usage
    case "$1" in
    start) shift; live_start "$@"; return ;;
    list) [ $# -eq 1 ] || usage; live_list; return ;;
    -h | --help) usage ;;
    esac
    [ $# -ge 2 ] || usage
    local id="$1" cmd="$2"
    shift 2
    live_find "$id"
    local dir="${L_RUN:+$L_RUN/live}" state="$L_RESULTS/live/state.json"
    case "$cmd" in
    export)
        [ $# -eq 0 ] || usage
        helper live-export "$L_RESULTS/live/session.json"
        return ;;
    log)
        live_log "$@"
        return ;;
    esac
    [ -n "$L_PID" ] || {
        jq -c '{ok:false, error:"the session has ended", last:.}' "$state" 2>/dev/null || echo '{"ok":false,"error":"the session has ended"}'
        exit 3
    }
    case "$cmd" in
    status)
        if [ "${1:-}" = --wait ]; then
            [ $# -eq 2 ] || usage
            GOGO_E2E_LIVE_DIR="$dir" GOGO_E2E_LIVE_REG="$L_REG" GOGO_E2E_LIVE_START="$L_START" helper live-wait "$state" "$2" "$L_PID" 1
            return
        fi
        [ $# -eq 0 ] || usage
        if [ -n "$dir" ] && [ -S "$dir/ctl.sock" ] && GOGO_E2E_LIVE_DIR="$dir" helper live-call 20 status 2>/dev/null; then return 0; fi
        jq -c '. + {ok:true, socket:false}' "$state"
        ;;
    restore)
        local wait=900 name="${1:-}" reply life
        [ "${2:-}" != --wait ] || wait="${3:?}"
        [ -n "$name" ] || usage
        life="$(jq -r '.life // 1' "$state")"
        reply="$(GOGO_E2E_LIVE_DIR="$dir" helper live-call 60 restore "$name")" || { printf '%s\n' "$reply"; return 1; }
        log "restoring $name: the game closes, the disk rolls back, it relaunches and replays"
        GOGO_E2E_LIVE_DIR="$dir" GOGO_E2E_LIVE_REG="$L_REG" GOGO_E2E_LIVE_START="$L_START" helper live-wait "$state" "$wait" "$L_PID" "$((life + 1))"
        ;;
    stop)
        [ $# -eq 0 ] || usage
        live_stop
        ;;
    wait)
        [ $# -eq 1 ] || usage
        GOGO_E2E_LIVE_DIR="$dir" helper live-call "$(awk -v s="$1" 'BEGIN { print s + 120 }')" wait "$1"
        ;;
    settle)
        local smax=30
        case $# in
        0) ;;
        2) [ "$1" = --max ] || usage; smax="$2" ;;
        *) usage ;;
        esac
        case "$smax" in '' | *[!0-9.]* | *.*.*) die "settle --max wants a number of seconds" 2 ;; esac
        GOGO_E2E_LIVE_DIR="$dir" helper live-call "$(awk -v s="$smax" 'BEGIN { print s + 120 }')" settle "$@"
        ;;
    *)
        [ -n "$dir" ] && [ -S "$dir/ctl.sock" ] || {
            jq -c '{ok:false, error:"not ready: \(.state): \(.detail) (status --wait)"}' "$state"
            exit 4
        }
        GOGO_E2E_LIVE_DIR="$dir" helper live-call 120 "$cmd" "$@"
        ;;
    esac
}

# Stop: the driver closes the game; the supervisor checks and tears down. Before
# there's a game (building, waiting for the lock), a TERM does the same.
live_stop() {
    local dir="${L_RUN:+$L_RUN/live}" i
    if [ -z "$dir" ] || [ ! -S "$dir/ctl.sock" ] || ! GOGO_E2E_LIVE_DIR="$dir" helper live-call 30 stop >/dev/null 2>&1; then
        ! live_owner "$L_PID" "$L_START" "$L_ID" || kill -TERM "$L_PID" 2>/dev/null || true
    fi
    for i in $(seq $(((GRACE_SECS + 240) * 2))); do
        live_owner "$L_PID" "$L_START" "$L_ID" || break
        sleep 0.5
    done
    if live_owner "$L_PID" "$L_START" "$L_ID"; then die "the session $L_PID is still up: cleanup --stop-live"; fi
    local summary="$L_RESULTS/summary.json"
    [ -s "$summary" ] || die "the session ended with no summary: $L_RESULTS/session.log"
    jq . "$summary"
    printf '%s\n' "$summary"
    if jq -e .ok "$summary" >/dev/null; then return 0; fi
    if jq -e '.failed | index("harness")' "$summary" >/dev/null; then return 3; fi
    return 1
}

live_log() { # [game|run|launch|sway|dxvk|vkd3d|proton] [--tail N]
    local which=game tail=60 f="" pat=""
    while [ $# -gt 0 ]; do
        case "$1" in
        --tail) tail="${2:?}"; shift 2 ;;
        game | run | launch | sway | dxvk | vkd3d | proton) which="$1"; shift ;;
        *) usage ;;
        esac
    done
    case "$tail" in '' | *[!0-9]*) die "--tail wants a number" 2 ;; esac
    case "$which" in
    run | launch)
        local name=launch.log
        # From inside the dir: no scratch path on a command line (leftovers).
        if [ -n "$L_RUN" ] && cd "$L_RUN/gogo/logs" 2>/dev/null; then
            if [ "$which" = launch ]; then
                f="$(find . -name launch.log -printf '%T@ %p\n' | sort -n | tail -1 | cut -d' ' -f2-)"
            else
                f="$(find . -name '*.log' ! -name launch.log -printf '%T@ %p\n' | sort -n | tail -1 | cut -d' ' -f2-)"
            fi
            [ -z "$f" ] || f="$PWD/${f#./}"
        fi
        [ "$which" = launch ] || name=run.log
        [ -n "$f" ] || f="$L_RESULTS/$name" ;;
    sway) f="$L_RESULTS/sway.log" ;;
    *) f="$L_RESULTS/game.out" ;;
    esac
    case "$which" in
    dxvk) pat='DXVK|^(info|warn|err): ' ;;
    vkd3d) pat='vkd3d' ;;
    proton) pat='[Pp]roton|^[0-9a-f]+:(err|warn|fixme):|wine' ;;
    esac
    [ -f "$f" ] || die "no $which log yet: $f"
    if [ -n "$pat" ]; then grep -aE -- "$pat" "$f" | tail -n "$tail" || true
    else tail -n "$tail" "$f"; fi | cut -c1-400 | sed "s|$HOME|~|g"
}

# cleanup's part: sessions whose supervisor died (a SIGKILL) leave a registry
# entry and maybe processes; a supervisor past its hard cap, or whose socket
# doesn't answer while it says it's ready, is an orphan; with --stop-live,
# every session is. An orphan gets TERM (its own teardown), then KILL.
live_reap() { # all (1: --stop-live)
    local all="$1" f k v pid start results run deadline id stuck now left i
    live_registry
    for f in "$LIVE_REG"/*; do
        [ -f "$f" ] && [ ! -L "$f" ] || continue
        id="${f##*/}"
        pid="" start="" results="" run="" deadline=""
        while IFS='=' read -r k v; do
            case "$k" in
            pid) pid="$v" ;; start) start="$v" ;; results) results="$v" ;; run) run="$v" ;; deadline) deadline="$v" ;;
            esac
        done <"$f"
        if ! live_owner "$pid" "$start" "$id"; then
            log "live session $id: its supervisor is gone (pid ${pid:-?} isn't it); sweeping"
            left="$(RUN_ID="linux-e2e-$id" RUN="${run:-/nonexistent}" PREFIX="" RUN_PIDNS="" leftovers)"
            [ -z "$left" ] || while read -r k _; do kill -KILL "$k" 2>/dev/null || true; done <<<"$left"
            rm -f "${f:?}"
            continue
        fi
        now="$(date +%s)"
        stuck=""
        [ "$all" != 1 ] || stuck="--stop-live"
        if [ -z "$stuck" ] && [ -n "$deadline" ] && [ "$now" -gt $((deadline + GRACE_SECS + 120)) ]; then
            stuck="past its hard cap"
        fi
        if [ -z "$stuck" ] && [ -n "$run" ] && jq -e '.state == "ready"' "$results/live/state.json" >/dev/null 2>&1 &&
            ! GOGO_E2E_LIVE_DIR="$run/live" helper live-call 20 status >/dev/null 2>&1; then
            stuck="ready, but its socket doesn't answer"
        fi
        if [ -z "$stuck" ]; then
            log "live session $id is in use ($(jq -r '"\(.state): \(.detail)"' "$results/live/state.json" 2>/dev/null)): live $id stop, or cleanup --stop-live"
            continue
        fi
        log "live session $id: $stuck: stopping it"
        # Each signal only while the pid is still it.
        ! live_owner "$pid" "$start" "$id" || kill -TERM "$pid" 2>/dev/null || true
        for i in $(seq $(((GRACE_SECS + 120) * 2))); do live_owner "$pid" "$start" "$id" || break; sleep 0.5; done
        if live_owner "$pid" "$start" "$id"; then
            log "live session $id: still up: KILL"
            kill -KILL "$pid" 2>/dev/null || true
            sleep 1
            rm -f "${f:?}"
        fi
    done
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

st_assert_clean() { # name outdir exit-code want-exit [kept]
    local name="$1" out="$2" code="$3" want="$4" kept="${5:-}" summary
    summary="$(tail -n 1 "$out/stdout")"
    [ "$code" = "$want" ] || st_fail "$name: exit $code, want $want (see $out/stderr)"
    if [ ! -f "$summary" ]; then st_fail "$name: no summary (see $out/stderr)"; return 0; fi
    jq -e --arg k "$kept" '.cleanup.left == "" and .cleanup.swept == ""
        and (if $k == "" then .cleanup.scratch_deleted else .cleanup.kept end)' "$summary" >/dev/null ||
        st_fail "$name: cleanup: $(jq -c .cleanup "$summary")"
    local scratch
    scratch="$(jq -r .cleanup.scratch "$summary")"
    if [ -z "$kept" ]; then [ ! -e "$scratch" ] || st_fail "$name: scratch still there: $scratch"
    else [ -d "$scratch" ] || st_fail "$name: kept scratch not there: $scratch"; fi
    log "self-test: $name: exit $code, cleanup $(jq -c .cleanup.scratch_deleted "$summary")"
}

# The bases' own fingerprint: every entry's type, mode, size, mtime and link,
# and the fake install's stamp.
st_tree_print() { # base
    (cd "$1/tree" && find . -printf '%p %y %m %s %T@ %l\n' | LC_ALL=C sort && cat gogo/games/fake/installed)
}

st_reuse_run() { # name outdir manifest want-exit [extra…]: sets ST_RS, the summary
    local name="$1" out="$2" manifest="$3" want="$4" code=0
    shift 4
    mkdir -p "$out"
    env --default-signal=INT,TERM "$SELF" run self-test --manifest "$manifest" --out "$out" --reuse-install "$@" \
        >"$out/stdout" 2>"$out/stderr" || code=$?
    case " $* " in *" --keep "*) st_assert_clean "$name" "$out" "$code" "$want" kept ;;
    *) st_assert_clean "$name" "$out" "$code" "$want" ;; esac
    ST_RS="$(tail -n 1 "$out/stdout")"
}

st_reuse() { # self-test dir
    local base="$1" bases one two before m2 code
    log "self-test: --reuse-install"
    export GOGO_E2E_INSTALLED_DIR="$base/installed"
    local m="$REPO/e2e/self-test-reuse.toml"

    # A miss: it builds, saves the base, then launches on its own scratch.
    st_reuse_run reuse-build "$base/reuse-1" "$m" 0
    jq -e '.ok and .build.reused == false and .build.exit == 0 and .build.reuse.saved
        and .build.reuse.source == "fresh build" and (.build.reuse.key | length == 16)' "$ST_RS" >/dev/null ||
        st_fail "reuse-build: $(jq -c '{failed, build}' "$ST_RS" 2>/dev/null)"
    one="$(jq -r .build.reuse.base "$ST_RS" 2>/dev/null)"
    [ -f "$one/meta.json" ] || { st_fail "reuse-build: no base at $one"; unset GOGO_E2E_INSTALLED_DIR; return 0; }
    [ ! -e "$one/tree/gogo/games/fake/launched" ] || st_fail "reuse-build: the launch wrote into the base"
    before="$(st_tree_print "$one")"
    jq -e --arg m "$ST_COPY_AUTO" '.build.copy.mode == $m and .build.copy.asked == "auto"' "$ST_RS" >/dev/null ||
        st_fail "reuse-build: copies by $(jq -c .build.copy "$ST_RS"), want $ST_COPY_AUTO"
    if [ "$ST_COPY_AUTO" = snapshot ]; then
        if ! { is_subvol "$one/tree/gogo" && is_subvol "$one/tree/home" && is_subvol "$one/tree/xdg"; }; then
            st_fail "reuse-build: snapshot mode, but the base's dirs aren't subvolumes"
        fi
    fi

    # A hit, twice: no build, the copy's first launch both times. The game
    # writes `launched` and fails if it's there, so the third run passing says
    # the second's write didn't reach it. The third keeps its scratch, which
    # moves out of the next one's way.
    local i keep
    for i in 2 3; do
        keep=""
        [ "$i" = 2 ] || keep=--keep
        st_reuse_run "reuse-hit-$i" "$base/reuse-$i" "$m" 0 $keep
        jq -e --arg b "$one" '.ok and .build.reused and .build.exit == null and .build.reuse.source == "cache"
            and .build.reuse.base == $b and .build.reuse.copy_secs != null and .tested_entry == null
            and (.checks | all(.name | startswith("build_") | not))' "$ST_RS" >/dev/null ||
            st_fail "reuse-hit-$i: $(jq -c '{failed, build}' "$ST_RS" 2>/dev/null)"
        [ "$(st_tree_print "$one")" = "$before" ] || st_fail "reuse-hit-$i: the base changed"
    done
    local kept
    kept="$(jq -r .cleanup.scratch "$ST_RS")"
    case "$kept" in "$GOGO_E2E_INSTALLED_DIR"/.runs/self-test-*) ;; *) st_fail "reuse-keep: kept at $kept" ;; esac
    [ -e "$kept/gogo/games/fake/launched" ] || st_fail "reuse-keep: the kept scratch has no launch in it"
    chmod -R u+rwX "$kept" && rm -rf "$kept"

    # Another build command is another key: it builds and saves a second base.
    m2="$base/self-test-reuse-2.toml"
    sed 's|date +%s%N|date +v2-%s%N|' "$m" >"$m2"
    [ "$(cat "$m")" != "$(cat "$m2")" ] || st_fail "reuse-key: the manifest didn't change"
    st_reuse_run reuse-key "$base/reuse-key" "$m2" 0
    two="$(jq -r .build.reuse.base "$ST_RS" 2>/dev/null)"
    if ! jq -e '.ok and .build.reused == false and .build.exit == 0 and .build.reuse.saved' "$ST_RS" >/dev/null ||
        [ "$two" = "$one" ]; then
        st_fail "reuse-key: didn't build a base of its own: $(jq -c .build "$ST_RS" 2>/dev/null)"
    fi

    # A link out of the run, into the base: refused before anything runs.
    touch -r "$one/tree/gogo/games/fake" "$base/fake-mtime"
    ln -s "$one/tree/gogo/games/fake/installed" "$one/tree/gogo/games/fake/out"
    st_reuse_run reuse-link "$base/reuse-link" "$m" 3
    jq -e '.failed == ["harness", "finished"] and (.checks[0].detail | test("link out of the run"))' "$ST_RS" >/dev/null ||
        st_fail "reuse-link: $(jq -c '[.checks[] | select(.ok | not)]' "$ST_RS" 2>/dev/null)"
    rm -f "$one/tree/gogo/games/fake/out"
    touch -r "$base/fake-mtime" "$one/tree/gogo/games/fake"
    [ "$(st_tree_print "$one")" = "$before" ] || st_fail "reuse-link: the base changed"

    # Usage: a manifest that builds nothing, --no-cache, a dir outside target/.
    code=0
    "$SELF" run self-test --manifest "$REPO/e2e/self-test-hold.toml" --reuse-install >/dev/null 2>&1 || code=$?
    [ "$code" = 2 ] || st_fail "usage: --reuse-install with no build: exit $code, want 2"
    code=0
    "$SELF" run self-test --manifest "$m" --reuse-install --no-cache >/dev/null 2>&1 || code=$?
    [ "$code" = 2 ] || st_fail "usage: --reuse-install --no-cache: exit $code, want 2"
    code=0
    GOGO_E2E_INSTALLED_DIR=/tmp/gogo-e2e-installed "$SELF" run self-test --manifest "$m" --reuse-install \
        >/dev/null 2>&1 || code=$?
    [ "$code" = 2 ] || st_fail "usage: a base dir outside target/: exit $code, want 2"
    [ ! -e /tmp/gogo-e2e-installed ] || st_fail "usage: made a base dir outside target/"

    # cleanup lists both with their size, prunes one by name, then the rest.
    bases="$("$SELF" cleanup 2>"$base/cleanup.stderr")" || st_fail "cleanup failed"
    if ! grep -q "^${one##*/}	" <<<"$bases" || ! grep -q "^${two##*/}	" <<<"$bases" ||
        ! grep -q "installed bases in .*: 2, " "$base/cleanup.stderr"; then
        st_fail "cleanup didn't list both: $bases"
    fi
    "$SELF" cleanup --prune-installed "${one##*/}" >/dev/null 2>&1 || st_fail "cleanup --prune-installed failed"
    if [ -e "$one" ] || [ ! -e "$two" ]; then st_fail "cleanup --prune-installed ${one##*/}: wrong base gone"; fi
    "$SELF" cleanup --prune-installed all >/dev/null 2>&1 || st_fail "cleanup --prune-installed all failed"
    local left
    left="$(find "$GOGO_E2E_INSTALLED_DIR" -mindepth 1 -maxdepth 1 ! -name .runs
        find "$GOGO_E2E_INSTALLED_DIR/.runs" -mindepth 1 -maxdepth 1)"
    [ -z "$left" ] || st_fail "cleanup --prune-installed all left: $left"
    unset GOGO_E2E_INSTALLED_DIR
}

# The modes this machine has (#255): sets ST_COPY_AUTO, what auto should pick,
# and ST_HAVE_SNAP / ST_HAVE_REFLINK. Probed here as the harness does.
st_copy_probe() { # self-test dir
    local d="$1/copy-probe"
    mkdir -p "$d"
    ST_HAVE_SNAP="" ST_HAVE_REFLINK=""
    if helper subvol create "$d/sv" 2>/dev/null && helper subvol snapshot "$d/sv" "$d/snap" 2>/dev/null; then
        rm -rf "$d/sv" "$d/snap"
        [ -e "$d/sv" ] || [ -e "$d/snap" ] || ST_HAVE_SNAP=1
    fi
    printf 'x\n' >"$d/f"
    cp --reflink=always "$d/f" "$d/g" 2>/dev/null && ST_HAVE_REFLINK=1
    rm -rf "$d"
    if [ -n "$ST_HAVE_SNAP" ]; then ST_COPY_AUTO=snapshot
    elif [ -n "$ST_HAVE_REFLINK" ]; then ST_COPY_AUTO=reflink
    else ST_COPY_AUTO=copy; fi
    log "self-test: copies here: auto is $ST_COPY_AUTO (snapshots: ${ST_HAVE_SNAP:-no}, reflinks: ${ST_HAVE_REFLINK:-no})"
}

# Every copy mode, forced with GOGO_E2E_COPY: a build saves a base, a hit runs
# on a copy of it, and the base is subvolumes only in snapshot mode. Where
# snapshots work, a plain base meets auto and becomes subvolumes, unchanged.
# Where they don't (CI), a forced snapshot is a harness failure, and clean.
st_copy() { # self-test dir
    local base="$1" m="$REPO/e2e/self-test-reuse.toml" mode want one before code
    log "self-test: copy modes (GOGO_E2E_COPY)"
    export GOGO_E2E_INSTALLED_DIR="$base/copy-installed"
    for mode in copy reflink snapshot; do
        want="$mode"
        [ "$mode" != reflink ] || [ -n "$ST_HAVE_REFLINK" ] || want=copy
        export GOGO_E2E_COPY="$mode"
        if [ "$mode" = snapshot ] && [ -z "$ST_HAVE_SNAP" ]; then
            st_reuse_run "copy-snapshot-none" "$base/copy-$mode" "$m" 3
            jq -e '.checks[0].name == "harness" and (.checks[0].detail | test("GOGO_E2E_COPY=snapshot"))' "$ST_RS" >/dev/null ||
                st_fail "copy-snapshot-none: $(jq -c '[.checks[] | select(.ok | not)]' "$ST_RS" 2>/dev/null)"
            log "self-test: copy-snapshot: no snapshots here, skipped (a forced one fails cleanly)"
            continue
        fi
        st_reuse_run "copy-$mode-build" "$base/copy-$mode-1" "$m" 0
        one="$(jq -r .build.reuse.base "$ST_RS" 2>/dev/null)"
        jq -e --arg m "$want" --arg a "$mode" '.ok and .build.reuse.saved and .build.copy.mode == $m
            and .build.copy.asked == $a' "$ST_RS" >/dev/null ||
            st_fail "copy-$mode-build: $(jq -c '{failed, copy:.build.copy}' "$ST_RS" 2>/dev/null)"
        [ -d "$one/tree" ] || { st_fail "copy-$mode-build: no base"; continue; }
        if is_subvol "$one/tree/gogo"; then [ "$want" = snapshot ] || st_fail "copy-$mode: a subvolume in the base"
        else [ "$want" != snapshot ] || st_fail "copy-$mode: the base isn't subvolumes"; fi
        before="$(st_tree_print "$one")"
        st_reuse_run "copy-$mode-hit" "$base/copy-$mode-2" "$m" 0
        jq -e --arg m "$want" '.ok and .build.reused and .build.copy.mode == $m' "$ST_RS" >/dev/null ||
            st_fail "copy-$mode-hit: $(jq -c '{failed, copy:.build.copy}' "$ST_RS" 2>/dev/null)"
        [ "$(st_tree_print "$one")" = "$before" ] || st_fail "copy-$mode-hit: the base changed"
        if [ "$mode" = reflink ] && [ -n "$ST_HAVE_SNAP" ]; then
            # The plain base, met by auto: made subvolumes, the same tree, then a launch on it.
            unset GOGO_E2E_COPY
            st_reuse_run "copy-convert" "$base/copy-convert" "$m" 0
            jq -e '.ok and .build.reused and .build.copy.mode == "snapshot"
                and (.build.copy.note | test("made subvolumes"))' "$ST_RS" >/dev/null ||
                st_fail "copy-convert: $(jq -c '{failed, copy:.build.copy}' "$ST_RS" 2>/dev/null)"
            if ! { is_subvol "$one/tree/gogo" && is_subvol "$one/tree/home" && is_subvol "$one/tree/xdg"; }; then
                st_fail "copy-convert: the base's dirs aren't subvolumes"
            fi
            jq -e '.copy == "snapshot" and .converted' "$one/meta.json" >/dev/null || st_fail "copy-convert: meta.json says $(jq -c .copy "$one/meta.json")"
            [ "$(st_tree_print "$one")" = "$before" ] || st_fail "copy-convert: the tree changed"
            [ -z "$(find "$one" -maxdepth 1 -name '.tree-new-*')" ] || st_fail "copy-convert: the old tree is still there"
        fi
        "$SELF" cleanup --prune-installed all >/dev/null 2>&1 || st_fail "copy-$mode: cleanup --prune-installed all failed"
        [ ! -e "$one" ] || st_fail "copy-$mode: prune left $one"
    done
    code=0
    GOGO_E2E_COPY=bogus "$SELF" run self-test --manifest "$m" --reuse-install >/dev/null 2>&1 || code=$?
    [ "$code" = 2 ] || st_fail "usage: GOGO_E2E_COPY=bogus: exit $code, want 2"
    unset GOGO_E2E_COPY GOGO_E2E_INSTALLED_DIR
}

# Where snapshots pay: a tree of many small files, like a Wine prefix's, timed
# in each mode this machine has. Logged, not checked: the numbers are the
# disk's. (Cyberpunk 2077's base is 950 files, all large: every mode is
# well under a second there, docs/linux-e2e.md.)
st_copy_bench() { # self-test dir
    local d="$1/copy-bench" n=20 i t0 out="" mode secs
    mkdir -p "$d"
    if [ -n "$ST_HAVE_SNAP" ]; then helper subvol create "$d/src"; else mkdir "$d/src"; fi
    for i in $(seq "$n"); do
        mkdir "$d/src/d$i"
        (cd "$d/src/d$i" && seq 1000 | split -l 1 -a 4 - f)
    done
    for mode in copy reflink snapshot; do
        case "$mode" in reflink) [ -n "$ST_HAVE_REFLINK" ] || continue ;; snapshot) [ -n "$ST_HAVE_SNAP" ] || continue ;; esac
        sync
        t0="$(date +%s.%N)"
        case "$mode" in
        copy) cp -a "$d/src" "$d/dst" ;;
        reflink) cp -a --reflink=always "$d/src" "$d/dst" ;;
        snapshot) helper subvol snapshot "$d/src" "$d/dst" ;;
        esac
        secs="$(awk -v a="$t0" -v b="$(date +%s.%N)" 'BEGIN { printf "%.2f", b - a }')"
        out="$out $mode ${secs}s,"
        rm -rf "$d/dst"
    done
    rm -rf "$d"
    log "self-test: copying $((n * 1000)) small files:${out%,}"
}

# A live session's answer, checked: the command passes and jq's test holds.
st_live_cmd() { # name test cmd…: sets ST_LR, the reply
    local name="$1" test="$2"
    shift 2
    ST_LR="$("$SELF" live "$ST_LID" "$@" 2>>"$ST_LOUT/client.stderr")" ||
        { st_fail "live: $name: $* failed: $ST_LR"; return 0; }
    jq -e "$test" <<<"$ST_LR" >/dev/null 2>&1 || st_fail "live: $name: $*: $ST_LR"
}

st_live_start() { # outdir extra…: sets ST_LID
    local out="$1"
    shift
    mkdir -p "$out"
    ST_LID="$("$SELF" live start self-test --manifest "$REPO/e2e/self-test-live.toml" --out "$out" "$@" \
        2>>"$out/start.stderr" | tail -n 1)"
    [ -n "$ST_LID" ] || st_fail "live: no session: $(cat "$out/start.stderr")"
}

st_live_gone() { # id outdir: waits for its supervisor; prints its summary
    local f="$GOGO_E2E_LOCK_DIR/live/$1" i
    for i in $(seq 240); do [ -e "$f" ] || break; sleep 0.5; done
    [ ! -e "$f" ] || st_fail "live: $1 still registered"
    printf '%s\n' "$2/$1/summary.json"
}

st_live() { # self-test dir
    local base="$1" out code summary
    log "self-test: a live session"
    out="$base/live"
    ST_LOUT="$out"
    st_live_start "$out"
    [ -n "$ST_LID" ] || return 0
    st_live_cmd ready '.state == "ready" and .life == 1 and .game.running' status --wait 60
    # It holds the cache lock: cleanup names it and leaves it be.
    code=0
    "$SELF" cleanup >/dev/null 2>"$out/cleanup.stderr" || code=$?
    { [ "$code" = 3 ] && grep -q "live session $ST_LID is in use" "$out/cleanup.stderr" &&
        grep -q "a live session: it ends after 900 s" "$out/cleanup.stderr"; } ||
        st_fail "live: cleanup under a session: exit $code: $(cat "$out/cleanup.stderr")"
    st_live_cmd shot '.ok and .since == null and .fills_window' screenshot white
    st_live_cmd key '.ok' key r
    st_live_cmd change '.since == "white" and .change_pct >= 50' screenshot red
    st_live_cmd checkpoint '.checkpoint == "red" and .steps == 4' checkpoint red
    # A dead end: the restore drops it.
    st_live_cmd dead-end '.ok' key b
    st_live_cmd dead-shot '.change_pct >= 50' screenshot blue
    st_live_cmd move '.step.move_to == {x: 50, y: 50}' move_to 50% 50%
    st_live_cmd click '.ok and .at == [10, 10]' click 10 10
    if "$SELF" live "$ST_LID" key NoSuchKey >"$out/bad-key.json" 2>/dev/null ||
        ! jq -e '.ok == false and (.error | test("keymap"))' "$out/bad-key.json" >/dev/null; then
        st_fail "live: a bad key: $(cat "$out/bad-key.json")"
    fi
    # Back to red: a new life on the disk of the launch (the game refuses a
    # second launch on the same disk), the path replayed and matched.
    st_live_cmd restore '.state == "ready" and .life == 2 and .game.running
        and .last_replay.ok and .last_replay.done == 4 and .steps == 4' restore red --wait 120
    st_live_cmd hold '.step.hold == {key: "b", ms: 200}' hold b 200
    st_live_cmd wait '.ok' wait 1
    st_live_cmd after '.name == "blue-2" and .since == "cp-red" and .change_pct >= 50' screenshot blue
    # settle: a still screen settles; an animated one says so within --max.
    st_live_cmd settle '.ok and .settled and .path != null and .step.wait_settled >= 10' settle --max 30
    st_live_cmd animate '.ok' key a
    st_live_cmd never '.ok and (.settled | not) and .secs >= 5 and .secs < 20 and (.step.wait != null)' settle --max 6
    st_live_cmd calm '.ok' key s
    st_live_cmd quit '.ok' type q
    st_live_cmd quit-wait '.game_running | not' wait 1
    "$SELF" live "$ST_LID" export >"$out/export.toml" 2>>"$out/client.stderr" || st_fail "live: export failed"
    if ! { grep -q '^hold = { key = "b", ms = 200 }$' "$out/export.toml" && grep -q '^screenshot = "blue-2"$' "$out/export.toml" &&
        grep -q '^how = "self"$' "$out/export.toml" && grep -q '^expect_change = { from = "white", to = "red"' "$out/export.toml" &&
        grep -q '^wait_settled = [0-9]*$' "$out/export.toml" && grep -q '^# settle: the screen never settled' "$out/export.toml" &&
        ! grep -q '"blue"' "$out/export.toml" && ! grep -q '^click' "$out/export.toml"; }; then
        st_fail "live: the export isn't the path after the restore: $(cat "$out/export.toml")"
    fi
    code=0
    "$SELF" live "$ST_LID" stop >"$out/stdout" 2>>"$out/client.stderr" || code=$?
    st_assert_clean live "$out" "$code" 0
    summary="$(tail -n 1 "$out/stdout")"
    jq -e --arg m "$ST_COPY_AUTO" '.ok and .live.lives == 2 and (.live.restores | length) == 1 and .live.checkpoints == ["red"]
        and .build.copy.mode == $m and .tested_entry == null and (.checks | any(.name == "clean_exit" and .ok))' "$summary" >/dev/null ||
        st_fail "live: summary: $(jq -c '{failed, live}' "$summary" 2>/dev/null)"
    [ "$(cat "$out/export.toml")" = "$(cat "$(jq -r .live.export "$summary")" 2>/dev/null)" ] ||
        st_fail "live: manifest.toml isn't the export"
    [ ! -e "$GOGO_E2E_LOCK_DIR/live/$ST_LID" ] || st_fail "live: still registered after stop"
    # The export is the proof: a fresh run of it passes.
    mkdir -p "$base/live-export"
    code=0
    env --default-signal=INT,TERM "$SELF" run self-test --manifest "$out/export.toml" --out "$base/live-export" \
        >"$base/live-export/stdout" 2>"$base/live-export/stderr" || code=$?
    st_assert_clean live-export "$base/live-export" "$code" 0

    # The limits: no command for --idle-secs, and --max-secs from the lock,
    # each end the session through the same teardown, failed.
    local how test
    for how in idle max; do
        out="$base/live-$how"
        ST_LOUT="$out"
        if [ "$how" = idle ]; then st_live_start "$out" --idle-secs 3; test='index("live_idle")'
        else st_live_start "$out" --max-secs 20; test='index("time_limit")'; fi
        [ -n "$ST_LID" ] || continue
        summary="$(st_live_gone "$ST_LID" "$out")"
        if ! jq -e ".failed | $test" "$summary" >/dev/null 2>&1 ||
            ! jq -e '.cleanup.left == "" and .cleanup.swept == "" and .cleanup.scratch_deleted' "$summary" >/dev/null; then
            st_fail "live-$how: $(jq -c '{failed, cleanup}' "$summary" 2>/dev/null || echo "no summary at $summary")"
        fi
    done

    # A SIGKILLed supervisor: its sandbox dies with it, and cleanup takes its
    # registry entry and scratch.
    out="$base/live-kill"
    ST_LOUT="$out"
    st_live_start "$out"
    [ -n "$ST_LID" ] || return 0
    st_live_cmd kill-ready '.state == "ready"' status --wait 60
    local pid run
    pid="$(sed -n 's/^pid=//p' "$GOGO_E2E_LOCK_DIR/live/$ST_LID")"
    run="$(sed -n 's/^run=//p' "$GOGO_E2E_LOCK_DIR/live/$ST_LID")"
    kill -KILL "$pid"
    sleep 2
    local stray
    stray="$(RUN_ID="linux-e2e-$ST_LID" RUN="$run" PREFIX="" RUN_PIDNS="" leftovers)"
    [ -z "$stray" ] || st_fail "live-kill: left behind: $stray"
    "$SELF" live "$ST_LID" key r >/dev/null 2>&1 && st_fail "live-kill: a dead session answered"
    "$SELF" cleanup 2>>"$out/cleanup.stderr" >/dev/null || st_fail "live-kill: cleanup failed: $(cat "$out/cleanup.stderr")"
    [ ! -e "$GOGO_E2E_LOCK_DIR/live/$ST_LID" ] || st_fail "live-kill: cleanup left its registry entry"
    [ ! -e "$run" ] || st_fail "live-kill: cleanup left $run"

    # A registry entry outlives its supervisor (a SIGKILL, an OOM, a reboot),
    # and its pid may be anyone's later: a live process that isn't the
    # session, past the deadline, is left alone, and the entry cleared.
    local how fake sl
    for how in "" --stop-live; do
        sleep 300 &
        sl=$!
        fake="self-test-$((RANDOM))-stale"
        printf 'pid=%s\nstart=%s\nrepo=/gone\nresults=%s\ntools=%s\nrun=%s\ndeadline=1\nidle=900\n' \
            "$sl" "$(proc_start "$sl")" "$base/$fake" "$TOOLS" "$base/$fake-run" >"$GOGO_E2E_LOCK_DIR/live/$fake"
        "$SELF" cleanup $how >/dev/null 2>>"$base/stale.stderr" || st_fail "live-stale$how: cleanup failed: $(cat "$base/stale.stderr")"
        kill -0 "$sl" 2>/dev/null || st_fail "live-stale$how: cleanup signalled pid $sl, not a session's"
        [ ! -e "$GOGO_E2E_LOCK_DIR/live/$fake" ] || st_fail "live-stale$how: cleanup left the stale entry"
        kill "$sl" 2>/dev/null || true
        wait "$sl" 2>/dev/null || true
    done

    # Usage: live options belong to live start; overrides to a game.
    code=0
    "$SELF" run self-test --manifest "$REPO/e2e/self-test.toml" --idle-secs 5 >/dev/null 2>&1 || code=$?
    [ "$code" = 2 ] || st_fail "usage: run --idle-secs: exit $code, want 2"
    code=0
    "$SELF" live start self-test --manifest "$REPO/e2e/self-test-live.toml" --env A=1 --out "$base/live-usage" \
        >/dev/null 2>&1 || code=$?
    [ "$code" = 2 ] || st_fail "usage: live start --env on a command: exit $code, want 2"
    "$SELF" cleanup --stop-live >/dev/null 2>&1 || true
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
    # Its own cache lock: neither waiting for a real run nor holding one up.
    export GOGO_E2E_LOCK_DIR="$base/cache-lock"

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
    jq -e '.build.reused == false and .build.reuse == null' "$summary" >/dev/null ||
        st_fail "pass: a run without --reuse-install says it reused: $(jq -c .build.reuse "$summary" 2>/dev/null)"

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

    # 4b. The cache lock, as another checkout would meet it: the second run
    #     waits, says who has the lock, and goes once the first is gone; a
    #     cleanup refuses meanwhile. A holder note left by a SIGKILL is stale
    #     information, not a lock.
    log "self-test: the cache lock"
    local lk="$base/cache-lock" held="$base/lock-held" waiter="$base/lock-waiter" w_pid
    mkdir -p "$held" "$waiter"
    st_start "$held" "$REPO/e2e/self-test-hold.toml"
    w_pid="$ST_PID"
    st_wait_window "$held" || st_fail "lock: no window"
    if ! grep -q "^pid=$w_pid\$" "$lk/.lock.holder" 2>/dev/null || ! grep -q "^run=linux-e2e-self-test-" "$lk/.lock.holder"; then
        st_fail "lock: no holder note: $(cat "$lk/.lock.holder" 2>&1)"
    fi
    st_start "$waiter" "$REPO/e2e/self-test.toml"
    local i
    for i in $(seq 60); do grep -q "waiting for it" "$waiter/stderr" 2>/dev/null && break; sleep 0.5; done
    grep -q "held by $REPO, linux-e2e-self-test-.*pid $w_pid " "$waiter/stderr" ||
        st_fail "lock: the waiter didn't name the holder: $(cat "$waiter/stderr")"
    code=0
    "$SELF" cleanup >/dev/null 2>"$base/lock-cleanup.stderr" || code=$?
    { [ "$code" = 3 ] && grep -q "held by $REPO" "$base/lock-cleanup.stderr"; } ||
        st_fail "lock: cleanup under a run: exit $code: $(cat "$base/lock-cleanup.stderr")"
    [ ! -s "$waiter/stdout" ] || st_fail "lock: the waiter ran under the holder"
    kill -TERM "$w_pid"
    wait "$w_pid" 2>/dev/null || true
    code=0
    wait "$ST_PID" || code=$?
    st_assert_clean lock-waiter "$waiter" "$code" 0
    [ ! -s "$lk/.lock.holder" ] || st_fail "lock: a holder note left after every run is gone"
    # A stale note (dead pid, nobody holding): the next run takes the lock.
    printf 'repo=/gone\nrun=linux-e2e-x\npid=2147483646\nstarted=2000-01-01T00:00:00Z\n' >"$lk/.lock.holder"
    "$SELF" cleanup >/dev/null 2>&1 || st_fail "lock: cleanup refused on a stale note"

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

    # 6. --installer: read-only in the build, gone from the game, untouched.
    log "self-test: --installer"
    local fake="$base/fake-installer"
    mkdir -p "$fake"
    printf 'not a real installer\n' >"$fake/setup_fake.exe"
    printf 'part\n' >"$fake/setup_fake-1.bin"
    local fake_before
    fake_before="$(cd "$fake" && find . -printf '%p %m %s %T@\n' | sort)"
    out="$base/installer"; mkdir -p "$out"
    code=0
    env --default-signal=INT,TERM "$SELF" run self-test --manifest "$REPO/e2e/self-test-installer.toml" \
        --installer "$fake/setup_fake.exe" --out "$out" >"$out/stdout" 2>"$out/stderr" || code=$?
    st_assert_clean installer "$out" "$code" 0
    summary="$(tail -n 1 "$out/stdout")"
    jq -e '.build.exit == 0 and .build.installer == "setup_fake.exe"
        and (.checks | any(.name == "build_isolation_installer" and .ok))
        and (.checks | any(.name == "isolation_paths" and .ok))' "$summary" >/dev/null ||
        st_fail "installer: $(jq -c '{build:.build.exit, failed}' "$summary" 2>/dev/null)"
    [ "$(cd "$fake" && find . -printf '%p %m %s %T@\n' | sort)" = "$fake_before" ] ||
        st_fail "installer: its folder changed"
    for bad in "$fake/nothing.exe" "$fake/setup_fake-1.bin"; do
        code=0
        "$SELF" run self-test --manifest "$REPO/e2e/self-test.toml" --installer "$bad" >/dev/null 2>&1 || code=$?
        [ "$code" = 2 ] || st_fail "usage: --installer $bad: exit $code, want 2"
    done
    code=0
    "$SELF" run self-test --manifest "$REPO/e2e/self-test-hold.toml" --installer "$fake/setup_fake.exe" >/dev/null 2>&1 || code=$?
    [ "$code" = 2 ] || st_fail "usage: --installer with no build: exit $code, want 2"

    # 7. --reuse-install: build → base; a reused run on a copy of it; its write
    #    in neither the base nor the next run; a changed key builds again;
    #    a base that links out is refused; cleanup prunes.
    st_copy_probe "$base"
    st_reuse "$base"
    st_copy "$base"
    st_copy_bench "$base"

    # 7b. A live session (#241): start, inputs, a checkpoint, a restore onto the
    #     rolled-back disk, the export (which a fresh run then passes), stop;
    #     the idle limit, the hard cap, and cleanup after a SIGKILL.
    st_live "$base"

    # 8. The sweep sees a stray process that outlived its run: a decoy carrying
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
live) cmd_live "$@" ;;
__live-serve)
    # live_start's supervisor, from its copy of this script.
    LIVE=1 LIVE_ID="${GOGO_E2E_LIVE_ID:?}" LIVE_RESULTS="${GOGO_E2E_LIVE_RESULTS:?}" TOOLS="${GOGO_E2E_TOOLS:?}"
    LIVE_IDLE=900
    trap live_early_exit EXIT
    trap 'exit 1' INT TERM HUP
    live_register
    cmd_run "$@" ;;
self-test) cmd_self_test "$@" ;;
cleanup) cmd_cleanup "$@" ;;
-h | --help | help) usage ;;
*) log "unknown command: $cmd"; usage ;;
esac
