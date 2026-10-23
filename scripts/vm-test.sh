#!/usr/bin/env bash
# Launch-test `gogo mac-app` bundles inside a throwaway Tart macOS VM, so game
# windows never steal focus on the developer's Mac (docs/design/08-testing.md §5.1,
# how-to: docs/vm-testing.md).
#
#   scripts/vm-test.sh golden [--refresh]        create/refresh the gog-golden image
#   scripts/vm-test.sh run <X.app> [opts]        clone → copy app → launch → collect → delete
#   scripts/vm-test.sh smoke [manifest] [opts]   per-layer smoke cases (scripts/vm-smoke.txt)
#   scripts/vm-test.sh shell [<X.app>]           interactive clone (deleted on exit)
#   scripts/vm-test.sh cleanup                   stop + delete every gog-test-* clone
#   scripts/vm-test.sh status                    golden + clones + running VM count
#
# run options:
#   --secs N            seconds to let the game run before judging it (default 30)
#   --out DIR           results root (default <repo>/target/vm-test)
#   --name NAME         clone name (default gog-test-<app>-<timestamp>)
#   --no-quarantine     skip the Gatekeeper quarantine xattr
#   --strict-gatekeeper if Gatekeeper blocks the quarantined launch, stop there
#                       (default: record the block, dismiss the dialog, strip
#                       quarantine, launch again)
#   --keep              leave the clone stopped-but-present for inspection
#   --wait-secs N       how long to queue for a free VM slot (default 1800)
#   --no-state          v2 bundles (design 16) get their games/<gogid>, L4
#                       override and store runtime(s) seeded into the guest by
#                       default, derived from gogo-launch.toml; this skips that
#                       (--state forces it). v1 self-contained bundles: none.
#   --runtimes share|copy  store runtimes as read-only virtiofs shares (default,
#                       nothing copied) or streamed with tar like the game state
#   --kill term|kill    #42 teardown check: after --secs, signal the launcher,
#                       wait --kill-wait N (default 15) s, assert no Wine left
#   --grep REGEX        record matching wine/launcher log lines as log_grep
#   --click X,Y         click there (System Events) after launch; repeatable
#   --osascript FILE    run this AppleScript in the guest after launch
#   --hook-delay N      seconds after launch before the hooks run (default 10;
#                       counts towards neither --secs)
#
# smoke options: --bundles DIR (default $GOGO_VM_BUNDLES or <repo>/target/v2),
#   --out DIR, --only case1,case2
#
# Output (machine-readable): the LAST stdout line of `run` is the summary JSON
# path (of `smoke`: smoke.json); the summary is printed to stdout before it.
# Exit codes: 0 = launched and still running with ≥1 window (smoke: all cases
# pass); 1 = test ran but the app failed (blocked/crashed/no window/teardown
# leftovers; smoke: a case failed); 2 = usage; 3 = infrastructure (tart/VM/SSH
# problem; the result says nothing about the app).
#
# Host needs: Tart (Homebrew `openai/tools/tart`, or a release archive on PATH)
# plus jq/ssh (the script re-execs itself under `nix develop` when jq is missing).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if ! command -v jq >/dev/null 2>&1 && [ -z "${GOGO_VM_IN_NIX:-}" ]; then
    GOGO_VM_IN_NIX=1 exec nix develop "$REPO" -c "$0" "$@"
fi

GOLDEN="${GOGO_VM_GOLDEN:-gog-golden}"
# Newest first; the first one the host can run wins (guest ≤ host macOS).
BASE_IMAGES="${GOGO_VM_BASE:-ghcr.io/cirruslabs/macos-golden-gate-base:latest ghcr.io/cirruslabs/macos-tahoe-base:latest}"
STATE="${GOGO_VM_STATE:-$HOME/.local/state/gogo-vm}"
KEY="$STATE/id_ed25519"
LOCK="$STATE/slot.lock"
MAX_VMS=2 # Apple's macOS-guest licence/Virtualization.framework limit
PREFIX_CLONE="gog-test-"

log() { printf '[vm-test] %s\n' "$*" >&2; }
die() { log "error: $*"; exit "${2:-3}"; }

TART="${TART:-$(command -v tart || true)}"
for c in /opt/homebrew/bin/tart "$HOME/.local/bin/tart"; do
    [ -n "$TART" ] && break
    [ -x "$c" ] && TART="$c"
done
[ -n "$TART" ] || die "tart not found (brew install openai/tools/tart)"

SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
          -o LogLevel=ERROR -o ConnectTimeout=5 -o ServerAliveInterval=10)

vm_exists()  { "$TART" list --format json | jq -e --arg n "$1" 'any(.[]; .Name == $n)' >/dev/null; }
# Running state from the process table, not `tart list`: listing opens every
# (ASIF) disk image to size it and races a booting VM for its exclusive disk
# lock ("DiskImages2: Cannot acquire exclusive lock" → VZErrorDomain Code=1).
tart_runs() { ps -axo command= | awk '$1 ~ /\/tart$/ && $2 == "run" {print $3}'; }
vm_running() { tart_runs | grep -qx -- "$1"; }
running_count() { tart_runs | grep -c . || true; }

vssh() { local ip="$1"; shift; ssh "${SSH_OPTS[@]}" -i "$KEY" -o BatchMode=yes "admin@$ip" "$@"; }

# Password ssh for the one-time golden bootstrap, without sshpass: OpenSSH
# reads the password from SSH_ASKPASS when SSH_ASKPASS_REQUIRE=force.
pssh() {
    local ip="$1"; shift
    local ask="$STATE/askpass.sh"
    printf '#!/bin/sh\necho admin\n' >"$ask"; chmod 700 "$ask"
    SSH_ASKPASS="$ask" SSH_ASKPASS_REQUIRE=force DISPLAY=none \
        ssh "${SSH_OPTS[@]}" -o PreferredAuthentications=password,keyboard-interactive \
        "admin@$ip" "$@" </dev/null
}

wait_ip() { # name timeout → prints ip
    local n="$1" t="${2:-180}" ip
    for _ in $(seq "$t"); do
        ip="$("$TART" ip "$n" --wait 1 2>/dev/null || true)"
        [ -n "$ip" ] && { echo "$ip"; return 0; }
        sleep 1
    done
    return 1
}

wait_ssh() { # ip timeout [pw]
    local ip="$1" t="${2:-180}" pw="${3:-}"
    for _ in $(seq "$t"); do
        if [ -n "$pw" ]; then pssh "$ip" true 2>/dev/null && return 0
        else vssh "$ip" true 2>/dev/null && return 0; fi
        sleep 1
    done
    return 1
}

# Serialise slot accounting so two callers can't both see "1 running" and start
# a 3rd VM. mkdir is atomic; a stale lock (dead owner pid) is taken over.
acquire_slot() {
    local deadline=$(( $(date +%s) + ${1:-1800} ))
    mkdir -p "$STATE"
    while :; do
        if mkdir "$LOCK" 2>/dev/null; then
            echo $$ >"$LOCK/pid"
            if [ "$(running_count)" -lt "$MAX_VMS" ]; then return 0; fi
            rm -rf "$LOCK"
        elif [ -f "$LOCK/pid" ] && ! kill -0 "$(cat "$LOCK/pid" 2>/dev/null)" 2>/dev/null; then
            rm -rf "$LOCK"; continue
        fi
        [ "$(date +%s)" -lt "$deadline" ] || return 1
        log "waiting for a free VM slot ($(running_count)/$MAX_VMS macOS VMs running)…"
        sleep 10
    done
}
release_slot() { rm -rf "$LOCK"; }

# Start <name> headless in the background; returns once it has an IP.
# Caller holds the slot lock; it's released once the VM counts as running.
start_vm() { # name logfile [tart run args…]
    local n="$1" logf="$2" try; shift 2
    # Virtualization.framework sometimes refuses a start right after another
    # VM stopped ("VZErrorDomain Code=1 … failed to start"); retry a few times.
    for try in 1 2 3; do
        nohup "$TART" run "$n" --no-graphics "$@" >>"$logf" 2>&1 &
        for _ in $(seq 30); do vm_running "$n" && break; sleep 1; done
        vm_running "$n" && break
        log "boot attempt $try of $n failed: $(tail -1 "$logf")"
        sleep 10
    done
    release_slot
    vm_running "$n" || return 1
    wait_ip "$n" 180
}

stop_delete() { # name
    "$TART" stop "$1" --timeout 20 >/dev/null 2>&1 || true
    "$TART" delete "$1" >/dev/null 2>&1 || true
}

# ---------------------------------------------------------------- golden ----
golden_provision() { # ip — idempotent guest setup, run over key ssh
    vssh "$1" bash -s <<'EOF'
set -eu
# Rosetta: Wine x86_64 runtimes (and GPTK) need it; the base image has none.
if ! /usr/bin/pgrep -q oahd && ! arch -x86_64 /usr/bin/true 2>/dev/null; then
    sudo softwareupdate --install-rosetta --agree-to-license
fi
arch -x86_64 /usr/bin/true && echo "rosetta: ok"
# Cirrus base images ship with Gatekeeper OFF ("override=security disabled");
# turn it back on so spctl verdicts and first-launch dialogs match a real Mac.
sudo spctl --global-enable 2>/dev/null || sudo spctl --master-enable
spctl --status
# macOS 15+ shows "com.apple.sshd-session is requesting to bypass the system
# private window picker" after an ssh `screencapture`, on top of the game.
# replayd keys the approval by the TCC-responsible binary; far-future dates
# suppress it. (TCC ScreenCapture/Accessibility for sshd-keygen-wrapper and
# osascript is pre-granted by the Cirrus image, SIP is off there.)
F="$HOME/Library/Group Containers/group.com.apple.replayd/ScreenCaptureApprovals"
D="3000-01-01 00:00:00 +0000"
for b in /usr/libexec/sshd-keygen-wrapper /usr/libexec/sshd-session; do
    defaults write "$F" "$b" -dict kScreenCaptureApprovalLastAlerted -date "$D" \
        kScreenCaptureApprovalLastUsed -date "$D" kScreenCapturePrivacyHintDate -date "$D" \
        kScreenCapturePrivacyHintPolicy -int 2592000
done
killall replayd 2>/dev/null || true
# No "Software Update Available" banners over the game.
sudo softwareupdate --schedule off >/dev/null 2>&1 || true
sudo defaults write /Library/Preferences/com.apple.SoftwareUpdate AutomaticCheckEnabled -bool false
# Quiet guest: no sleep / screensaver so screenshots show the game.
sudo pmset -a sleep 0 displaysleep 0 >/dev/null
defaults -currentHost write com.apple.screensaver idleTime 0
# No "reopen windows" / crash-reporter dialogs between runs.
defaults write com.apple.CrashReporter DialogType none
defaults write com.apple.loginwindow TALLogoutSavesState -bool false
mkdir -p ~/gogo-test
sw_vers
EOF
}

cmd_golden() {
    local refresh=0
    [ "${1:-}" = "--refresh" ] && refresh=1
    mkdir -p "$STATE"
    [ -f "$KEY" ] || ssh-keygen -q -t ed25519 -N '' -C gogo-vm-test -f "$KEY"
    if [ "$refresh" = 1 ] && vm_exists "$GOLDEN"; then
        log "refresh: deleting $GOLDEN"; stop_delete "$GOLDEN"
    fi
    if ! vm_exists "$GOLDEN"; then
        local img="" ok=""
        for img in $BASE_IMAGES; do
            log "cloning $img → $GOLDEN (first pull is ~25–50 GB)"
            if "$TART" clone "$img" "$GOLDEN"; then ok=1; break; fi
        done
        [ -n "$ok" ] || die "could not clone any of: $BASE_IMAGES"
        "$TART" set "$GOLDEN" --memory 8192 --cpu 4
    fi
    local ip
    if vm_running "$GOLDEN"; then
        ip="$(wait_ip "$GOLDEN" 60)" || die "$GOLDEN is running but has no IP"
    else
        acquire_slot 600 || die "no free VM slot"
        ip="$(start_vm "$GOLDEN" "$STATE/golden-run.log")" || die "golden VM did not boot (see $STATE/golden-run.log)"
    fi
    trap '"$TART" stop "$GOLDEN" --timeout 30 >/dev/null 2>&1 || true' EXIT
    if ! wait_ssh "$ip" 30; then
        log "installing ssh key via password login (admin/admin)"
        wait_ssh "$ip" 180 pw || die "ssh to $GOLDEN failed"
        # pssh's stdin is /dev/null (askpass needs no tty); pass the key inline.
        pssh "$ip" "mkdir -p ~/.ssh && chmod 700 ~/.ssh && { grep -q gogo-vm-test ~/.ssh/authorized_keys 2>/dev/null || echo '$(cat "$KEY.pub")' >> ~/.ssh/authorized_keys; } && chmod 600 ~/.ssh/authorized_keys"
        wait_ssh "$ip" 30 || die "key ssh still failing"
    fi
    golden_provision "$ip"
    # Shut down from inside: `tart stop` can hard-stop before the guest has
    # flushed, and the provisioning is silently lost (seen 2026-10-22).
    vssh "$ip" "sync; sudo shutdown -h now" >/dev/null 2>&1 || true
    for _ in $(seq 120); do vm_running "$GOLDEN" || break; sleep 1; done
    vm_running "$GOLDEN" && "$TART" stop "$GOLDEN" --timeout 60 >/dev/null
    trap - EXIT
    log "golden ready: $GOLDEN"
}

# ------------------------------------------------------------------- run ----
# Guest-side probe: prints one JSON object. Window count via CoreGraphics from
# JXA — CGWindowListCopyWindowInfo needs no TCC grant for owner names/pids.
GUEST_PROBE='
ObjC.import("CoreGraphics");
const sys = ["Finder","Dock","SystemUIServer","Control Center","Window Server",
  "WindowManager","Notification Center","Spotlight","loginwindow","TextInputMenuAgent",
  "Wallpaper","CoreServicesUIAgent","universalAccessAuthWarn","Siri"];
const raw = $.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly, 0);
const ws = ObjC.deepUnwrap(ObjC.castRefToObject(raw)) || [];
// Wine puts fullscreen games (and their dialogs) on layers 25-27, not 0, and
// a game that captured the display (mode change) on the shielding layer.
const out = ws.filter(w => !sys.includes(w.kCGWindowOwnerName) &&
    (w.kCGWindowLayer === 0 || /wine|\.exe/i.test(w.kCGWindowOwnerName)))
  .map(w => ({owner: w.kCGWindowOwnerName, pid: w.kCGWindowOwnerPID, layer: w.kCGWindowLayer,
              w: Math.round(w.kCGWindowBounds.Width), h: Math.round(w.kCGWindowBounds.Height)}));
JSON.stringify(out);
'
# Same, unfiltered: what else is on screen (dialogs covering the game).
GUEST_PROBE_ALL='
ObjC.import("CoreGraphics");
const raw = $.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly, 0);
const ws = ObjC.deepUnwrap(ObjC.castRefToObject(raw)) || [];
JSON.stringify(ws.filter(w => (w.kCGWindowLayer < 1000 || /wine/i.test(w.kCGWindowOwnerName)) && w.kCGWindowBounds.Width > 40 &&
    !["Window Server","Dock","Wallpaper","SystemUIServer","Control Center"].includes(w.kCGWindowOwnerName))
  .map(w => ({owner: w.kCGWindowOwnerName, pid: w.kCGWindowOwnerPID, layer: w.kCGWindowLayer,
              w: Math.round(w.kCGWindowBounds.Width), h: Math.round(w.kCGWindowBounds.Height)})));
'

# The host's gogo state dir (design 16 §0); a v2 bundle needs parts of it.
HOST_STATE="${GOGO_VM_HOST_STATE:-$HOME/Library/Application Support/gogo}"
# shellcheck disable=SC2016 # expanded by the guest shell
GUEST_STATE='$HOME/Library/Application Support/gogo'
SHARES="/Volumes/My Shared Files"

toml_str() { # file key → first string value (gogo writes 'x' or "x")
    sed -n "s/^$2 *= *[\"']\\(.*\\)[\"'] *\$/\\1/p" "$1" | head -1
}

# What a v2 bundle needs from the host state dir, derived from its
# gogo-launch.toml and the game's effective launch.conf, one unit per line:
#   copy <rel>   games/<gogid>, compat/<gogid>.toml (writable; tar over ssh)
#   store <rel>  a store entry: runtimes/<id>[/<ver>], renderers/<id>[/<ver>]
#   abs <dir>    a runtime outside the store (`path:`), same path in the guest
# With mode=share a store unit is the whole <id> dir, so a derived runtime
# (<ver>+<renderer>-<rver>) and the base its Frameworks link to resolve alike.
state_units() { # app mode
    local toml="$1/Contents/Resources/gogo-launch.toml" mode="$2" gogid rt p kind rest id ver
    gogid="$(toml_str "$toml" gogid)"
    [ -n "$gogid" ] && [ -d "$HOST_STATE/games/$gogid" ] || return 1
    echo "copy games/$gogid"
    if [ -f "$HOST_STATE/compat/$gogid.toml" ]; then echo "copy compat/$gogid.toml"; fi
    rt="$(toml_str "$toml" runtime)"
    {
        case "$rt" in path:*) echo "${rt#path:}" ;; ?*) echo "runtimes/$rt" ;; esac
        # The effective config can point elsewhere (an L4 retarget, a derived
        # renderer runtime): follow its resolved paths too.
        if [ -f "$HOST_STATE/games/$gogid/launch.conf" ]; then (
            # shellcheck source=/dev/null
            source "$HOST_STATE/games/$gogid/launch.conf"
            printf '%s\n' "${RUNTIME_DIR:-}" "${FRAMEWORKS:-}"
        ); fi
    } | while IFS= read -r p; do
        [ -n "$p" ] || continue
        case "$p" in "$HOST_STATE"/*) p="${p#"$HOST_STATE"/}" ;; esac
        case "$p" in
            /*) echo "abs $p" ;;
            runtimes/*|renderers/*)
                kind="${p%%/*}"; rest="${p#*/}"; id="${rest%%/*}"; rest="${rest#*/}"; ver="${rest%%/*}"
                if [ "$mode" = share ]; then echo "store $kind/$id"; else echo "store $kind/$id/$ver"; fi ;;
        esac
    done | sort -u
}

cmd_run() {
    local app="" secs=30 out="$REPO/target/vm-test" name="" quar=1 strict=0 keep=0 waits=1800
    local state=auto rtmode=share kill_sig="" kill_wait=15 grep_re="" hook_osa="" hook_delay=10 clicks=()
    while [ $# -gt 0 ]; do
        case "$1" in
            --secs) secs="$2"; shift 2 ;;
            --out) out="$2"; shift 2 ;;
            --name) name="$2"; shift 2 ;;
            --no-quarantine) quar=0; shift ;;
            --strict-gatekeeper) strict=1; shift ;;
            --keep) keep=1; shift ;;
            --wait-secs) waits="$2"; shift 2 ;;
            --state) state=1; shift ;;
            --no-state) state=0; shift ;;
            --runtimes) rtmode="$2"; shift 2 ;;
            --kill) kill_sig="$(printf '%s' "$2" | tr '[:lower:]' '[:upper:]')"; shift 2 ;;
            --kill-wait) kill_wait="$2"; shift 2 ;;
            --grep) grep_re="$2"; shift 2 ;;
            --click) clicks+=("$2"); shift 2 ;;
            --osascript) hook_osa="$2"; shift 2 ;;
            --hook-delay) hook_delay="$2"; shift 2 ;;
            -*) die "unknown option $1" 2 ;;
            *) app="$1"; shift ;;
        esac
    done
    [ -n "$app" ] && [ -d "$app" ] && [[ "$app" == *.app ]] || die "run needs an existing X.app" 2
    case "$rtmode" in share|copy) ;; *) die "--runtimes share|copy" 2 ;; esac
    case "$kill_sig" in ""|TERM|KILL|INT|HUP) ;; *) die "--kill term|kill|int|hup" 2 ;; esac
    [ -z "$hook_osa" ] || [ -f "$hook_osa" ] || die "--osascript: no file $hook_osa" 2
    app="$(cd "$app" && pwd)"
    local base; base="$(basename "$app")"
    local slug; slug="$(printf '%s' "${base%.app}" | tr -cs 'A-Za-z0-9' '-' | tr '[:upper:]' '[:lower:]' | sed 's/^-//;s/-$//' | cut -c1-24)"
    local ts; ts="$(date +%Y%m%d-%H%M%S)"
    name="${name:-$PREFIX_CLONE$slug-$ts}"
    local res="$out/$slug-$ts"
    mkdir -p "$res"
    vm_exists "$GOLDEN" || die "no $GOLDEN image; run: scripts/vm-test.sh golden"
    [ -f "$KEY" ] || die "no ssh key at $KEY; run: scripts/vm-test.sh golden"

    # v2 bundle (design 16): only a launcher plus gogo-launch.toml. A v1
    # self-contained bundle carries its engine in Resources/wine.
    local toml="$app/Contents/Resources/gogo-launch.toml" model=v1 gogid="" runtime=""
    if [ -f "$toml" ] && [ ! -d "$app/Contents/Resources/wine" ]; then
        model=v2; gogid="$(toml_str "$toml" gogid)"; runtime="$(toml_str "$toml" runtime)"
    fi
    [ "$state" = auto ] && { [ "$model" = v2 ] && state=1 || state=0; }

    # Everything below writes into the result dir; summary assembled at the end.
    local status="infra_error" exit_code=3 ip="" gk_block=false gatekeeper="not_quarantined" spctl_rc="" cs_rc=""
    local launched=false alive=false windows="[]" note="" cs_after_rc="" modified="null" gk_left="null"
    local units=() teardown="null"
    # shellcheck disable=SC2329 # invoked by the EXIT trap
    finish() {
        local procs="[]"
        [ -f "$res/processes.txt" ] && procs="$(jq -R -s 'split("\n") | map(select(length>0) | sub("^ +"; ""))' "$res/processes.txt")"
        lines() { if [ -s "$1" ]; then jq -R -s 'split("\n") | map(select(length>0))' "$1"; else echo '[]'; fi; }
        jq -n --arg status "$status" --argjson exit "$exit_code" --arg app "$app" \
              --arg model "$model" --arg gogid "$gogid" --arg runtime "$runtime" \
              --arg vm "$name" --arg ip "$ip" --argjson secs "$secs" \
              --argjson quarantined "$([ "$quar" = 1 ] && echo true || echo false)" \
              --argjson gatekeeper_blocked "$gk_block" --arg gatekeeper "$gatekeeper" \
              --argjson gk_left "$gk_left" \
              --arg spctl_rc "$spctl_rc" --arg codesign_rc "$cs_rc" --arg cs_after "$cs_after_rc" \
              --argjson modified "$modified" \
              --argjson launched "$launched" --argjson running "$alive" \
              --argjson windows "$windows" --argjson processes "$procs" \
              --argjson missing "$(lines "$res/missing-libs.txt")" \
              --argjson outside "$(lines "$res/writes-outside-gogo.txt")" \
              --argjson grep "$(lines "$res/log-grep.txt")" \
              --argjson units "$(printf '%s\n' ${units[@]+"${units[@]}"} | jq -R -s 'split("\n") | map(select(length>0))')" \
              --arg rtmode "$([ "$state" = 1 ] && echo "$rtmode")" \
              --argjson teardown "$teardown" \
              --arg note "$note" --arg dir "$res" \
              --arg screenshot "$([ -s "$res/screenshot.png" ] && echo "$res/screenshot.png")" \
              '{status:$status, exit_code:$exit, app:$app, model:$model, gogid:$gogid, runtime:$runtime,
                vm:$vm, ip:$ip, secs:$secs,
                state:{units:$units, runtimes:$rtmode},
                quarantined:$quarantined, gatekeeper:$gatekeeper, gatekeeper_blocked:$gatekeeper_blocked,
                gatekeeper_dialog_left:$gk_left,
                codesign_verify_rc:$codesign_rc, codesign_strict_rc:$cs_after, spctl_rc:$spctl_rc,
                bundle_modified:$modified,
                gatekeeper_screenshot:(if $gatekeeper_blocked then $dir+"/gatekeeper.png" else "" end),
                launched:$launched, running_after_secs:$running, windows:$windows,
                processes:$processes, missing_libs:$missing, writes_outside_gogo:$outside,
                log_grep:$grep, teardown:$teardown,
                note:$note, results_dir:$dir, screenshot:$screenshot,
                files:{codesign:($dir+"/codesign.txt"), spctl:($dir+"/spctl.txt"),
                       stdout:($dir+"/wine-stdout.log"), stderr:($dir+"/wine-stderr.log"),
                       launcher_logs:($dir+"/launcher-logs"),
                       prefix_logs:($dir+"/prefix-logs"), system_log:($dir+"/system.log"),
                       vm_log:($dir+"/tart-run.log")}}' >"$res/summary.json"
        if [ "$keep" = 1 ]; then "$TART" stop "$name" --timeout 20 >/dev/null 2>&1 || true
        else stop_delete "$name"; fi
        cat "$res/summary.json"
        echo "$res/summary.json"
        exit "$exit_code"
    }
    trap finish EXIT
    trap 'note="interrupted"; exit 3' INT TERM

    # Plan the state seeding before boot: read-only store shares are
    # virtiofs mounts, given to `tart run`. Sharing (the default) copies no
    # runtime at all; --runtimes copy streams it with tar like the rest.
    local dirs=(--dir="out:$res") u p i=0 shares=()
    if [ "$state" = 1 ]; then
        [ "$model" = v2 ] || { note="--state: $base has no gogo-launch.toml (not a v2 bundle)"; exit 3; }
        while IFS= read -r u; do units+=("$u"); done < <(state_units "$app" "$rtmode") \
            || true
        [ "${#units[@]}" -gt 0 ] || { note="--state: no $HOST_STATE/games/$gogid for $base"; exit 3; }
        for u in "${units[@]}"; do
            case "$u" in
                "store "*) [ "$rtmode" = share ] || continue; p="$HOST_STATE/${u#store }" ;;
                "abs "*) [ "$rtmode" = share ] || continue; p="${u#abs }" ;;
                *) continue ;;
            esac
            [ -d "$p" ] || { note="--state: $p missing on the host"; exit 3; }
            dirs+=(--dir="rt$i:$p:ro"); shares+=("rt$i ${u#* }"); i=$((i + 1))
        done
    fi

    log "clone $GOLDEN → $name"
    "$TART" clone "$GOLDEN" "$name" || { note="tart clone failed"; exit 3; }
    acquire_slot "$waits" || { note="no free VM slot after ${waits}s"; exit 3; }
    ip="$(start_vm "$name" "$res/tart-run.log" "${dirs[@]}")" \
        || { note="VM did not boot / no IP"; exit 3; }
    wait_ssh "$ip" 180 || { note="ssh to $ip failed"; exit 3; }
    log "vm $name up at $ip; copying $base"

    local G="$SHARES"
    local dst="/Users/admin/Applications/$base"
    # tar over ssh, not a virtiofs share + ditto: Wine prefixes carry absolute
    # symlinks (dosdevices/z: -> /, drive_c/users/<you>/Desktop -> ~/Desktop)
    # that virtiofs fails with ELOOP. tar keeps them as links, like a real
    # zip/dmg download would, and lands on a local volume Wine can write to.
    tar -C "$(dirname "$app")" -cf - "$base" | vssh "$ip" "mkdir -p ~/Applications && tar -C ~/Applications -xf -" \
        || { note="copy into VM failed"; exit 3; }
    if [ "$state" = 1 ]; then
        # State-dir paths in launch.conf are relative (design 16 §1.2), so the
        # same layout under the guest's admin user resolves unchanged.
        local copies=() abs=() s
        for u in "${units[@]}"; do
            case "$u" in
                "copy "*) copies+=("${u#copy }") ;;
                "store "*) [ "$rtmode" = copy ] && copies+=("${u#store }") ;;
                "abs "*) [ "$rtmode" = copy ] && abs+=("${u#abs /}") ;;
            esac
        done
        log "seeding guest state ($rtmode runtimes): ${units[*]}"
        tar -C "$HOST_STATE" -cf - "${copies[@]}" \
            | vssh "$ip" "mkdir -p \"$GUEST_STATE\" && tar -C \"$GUEST_STATE\" -xf -" \
            || { note="state copy into VM failed"; exit 3; }
        if [ "${#abs[@]}" -gt 0 ]; then
            tar -C / -cf - "${abs[@]}" | vssh "$ip" "sudo tar -C / -xf -" \
                || { note="runtime copy into VM failed"; exit 3; }
        fi
        for s in ${shares[@]+"${shares[@]}"}; do
            local tag="${s%% *}" tgt="${s#* }" sudo=""
            case "$tgt" in /*) sudo=sudo ;; *) tgt="$GUEST_STATE/$tgt" ;; esac
            vssh "$ip" "for _ in \$(seq 30); do [ -d \"$G/$tag\" ] && break; sleep 1; done
                        $sudo mkdir -p \"\$(dirname \"$tgt\")\" && $sudo ln -sfn \"$G/$tag\" \"$tgt\" && ls \"$tgt/\" >/dev/null" \
                || { note="store share $tag → $tgt not mounted in the guest"; exit 3; }
        done
    fi

    # A stat listing of Contents/ (path size mtime mode): equal before and
    # after the run ⇔ nothing wrote into the signed bundle (#53).
    bundle_hash() { vssh "$ip" "cd \"$dst\" && find Contents -exec stat -f '%N %z %m %p' {} + | LC_ALL=C sort | shasum -a 256 | cut -c1-64"; }
    local h0; h0="$(bundle_hash 2>/dev/null || true)"
    if [ "$quar" = 1 ]; then
        vssh "$ip" "xattr -w com.apple.quarantine \"0081;\$(printf %x \$(date +%s));Safari;\" \"$dst\""
    fi
    set +e
    vssh "$ip" "codesign --verify --deep --strict --verbose=2 \"$dst\"" >"$res/codesign.txt" 2>&1; cs_rc=$?
    vssh "$ip" "spctl -a -vv \"$dst\"" >"$res/spctl.txt" 2>&1; spctl_rc=$?
    set -e

    # Launch through LaunchServices (what a double-click does). `open --stdout`
    # hands the files straight to the launcher (= Wine's stdout/stderr) but
    # itself stays attached for the app's lifetime, so detach it.
    local since; since="$(vssh "$ip" 'date "+%Y-%m-%d %H:%M:%S"; mkdir -p ~/gogo-test; touch ~/gogo-test/.t0' | head -1)"
    launch() {
        vssh "$ip" "nohup open --stdout \"$G/out/wine-stdout.log\" --stderr \"$G/out/wine-stderr.log\" \"$dst\" >/dev/null 2>&1 &" \
            >>"$res/open.txt" 2>&1
    }
    # "pid stat command" of the launcher and anything Wine; quarantined launches
    # run from an AppTranslocation path, so match the bundle name, not $dst.
    game_procs() {
        vssh "$ip" "ps -axo pid=,stat=,command= | awk -v b='/$base/Contents/' 'index(\$0, b) || tolower(\$0) ~ /wineserver|wine(64)?-preloader|\\.exe( |\$)/' | grep -v awk" 2>/dev/null || true
    }
    # Live = present and not stopped (T): macOS holds a quarantined app's first
    # launch SIGSTOPped behind the "downloaded from the Internet" dialog.
    n_live() { game_procs | awk '$2 !~ /T/' | grep -c . || true; }
    all_windows() { vssh "$ip" "osascript -l JavaScript -e '$GUEST_PROBE_ALL'" 2>/dev/null || echo '[]'; }
    launch || true
    sleep 8
    if [ "$quar" = 1 ]; then
        local p; p="$(game_procs)"
        if [ -z "$p" ]; then gatekeeper="blocked"
        elif [ "$(n_live)" -eq 0 ]; then gatekeeper="prompted"
        else gatekeeper="allowed"; fi
        if [ "$gatekeeper" != allowed ]; then
            gk_block=true
            log "quarantined launch: $gatekeeper (Gatekeeper dialog); screenshot → gatekeeper.png"
            vssh "$ip" "screencapture -x \"$G/out/gatekeeper.png\"" >/dev/null 2>&1 || true
            all_windows >"$res/gatekeeper-windows.json"
            vssh "$ip" "/usr/bin/log show --style compact --start '$since' --predicate 'process == \"syspolicyd\" OR process == \"XprotectService\" OR process == \"CoreServicesUIAgent\"'" >"$res/gatekeeper.log" 2>&1 || true
            if [ "$strict" = 1 ]; then
                status="gatekeeper_$gatekeeper"; exit_code=1; note="strict: stopped at the Gatekeeper dialog"
                exit 1
            fi
            # Equivalent of the user clicking Done, then "Open Anyway": close
            # the dialog, drop the stopped first launch, strip quarantine and
            # launch again. GK_DISMISS is defined below.
            vssh "$ip" "osascript -e '$GK_DISMISS'" >"$res/gatekeeper-dismiss.txt" 2>&1 || true
            vssh "$ip" "pkill -9 -f AppTranslocation; xattr -dr com.apple.quarantine \"$dst\"" >/dev/null 2>&1 || true
            sleep 2
            launch || true
            sleep 5
        fi
    fi
    [ "$(n_live)" -gt 0 ] && launched=true
    # First-run dialogs the game itself shows (docs/19 §8.2 Q8).
    if [ "${#clicks[@]}" -gt 0 ] || [ -n "$hook_osa" ]; then
        sleep "$hook_delay"
        local c
        for c in ${clicks[@]+"${clicks[@]}"}; do
            log "click at $c"
            vssh "$ip" "osascript -e 'tell application \"System Events\" to click at {$c}'" >>"$res/hooks.txt" 2>&1 || true
            sleep 2
        done
        if [ -n "$hook_osa" ]; then
            log "osascript hook $hook_osa"
            vssh "$ip" "osascript -" <"$hook_osa" >>"$res/hooks.txt" 2>&1 || true
        fi
    fi
    log "running for ${secs}s…"
    sleep "$secs"

    game_procs >"$res/processes.txt"
    [ "$(n_live)" -gt 0 ] && alive=true
    windows="$(vssh "$ip" "osascript -l JavaScript -e '$GUEST_PROBE'" 2>/dev/null || echo '[]')"
    echo "$windows" | jq -e . >/dev/null 2>&1 || windows="[]"
    all_windows >"$res/windows-all.json"
    if [ "$gk_block" = true ]; then
        gk_left="$(jq 'any(.[]; .owner == "CoreServicesUIAgent")' "$res/windows-all.json" 2>/dev/null || echo null)"
    fi
    vssh "$ip" "screencapture -x \"$G/out/screenshot.png\"" >"$res/screencapture.txt" 2>&1 || true

    # #42: kill the launcher like a force-quit / supervisor would, then check
    # nothing Wine is left. The guest runs only this game, so any wine*
    # process is this prefix's.
    if [ -n "$kill_sig" ]; then
        local lpid left
        lpid="$(vssh "$ip" "ps -axo pid=,ppid=,command= | awk -v b='/$base/Contents/MacOS/' 'index(\$0, b) && \$2 == 1 { print \$1; exit }'" 2>/dev/null || true)"
        log "teardown check: SIG$kill_sig → launcher ${lpid:-?}, then ${kill_wait}s"
        if [ -n "$lpid" ]; then
            vssh "$ip" "kill -$kill_sig $lpid" || true
            sleep "$kill_wait"
        fi
        game_procs >"$res/processes-after-kill.txt"
        left="$(jq -R -s 'split("\n") | map(select(length>0) | sub("^ +"; ""))' "$res/processes-after-kill.txt")"
        teardown="$(jq -n --arg sig "$kill_sig" --arg pid "$lpid" --argjson wait "$kill_wait" --argjson left "$left" \
            '{signal:$sig, launcher_pid:$pid, wait_secs:$wait, leftovers:$left,
              ok:(($pid | length) > 0 and ($left | length) == 0)}')"
    fi

    # Wine-side logs: anything *.log/*.txt-ish the game or Wine left in the prefix.
    mkdir -p "$res/prefix-logs"
    local pfx="$dst/Contents/Resources"
    [ "$model" = v2 ] && pfx="$GUEST_STATE/games/$gogid"
    vssh "$ip" "cd \"$pfx\" 2>/dev/null && find . -path ./wine -prune -o -type f \\( -iname '*.log' -o -iname '*.err' -o -iname 'ddraw.ini' -o -iname 'launch.conf' \\) -size -5M -print0 | xargs -0 tar cf - 2>/dev/null" \
        | tar -C "$res/prefix-logs" -xf - 2>/dev/null || true
    if [ -n "$gogid" ]; then
        mkdir -p "$res/launcher-logs"
        vssh "$ip" "tar -C ~/Library/Logs/gogo/$gogid -cf - . 2>/dev/null" | tar -C "$res/launcher-logs" -xf - 2>/dev/null || true
    fi
    vssh "$ip" "/usr/bin/log show --style compact --start '$since' --predicate 'process == \"syspolicyd\" OR process == \"XprotectService\" OR process CONTAINS[c] \"wine\" OR eventMessage CONTAINS[c] \"$base\"'" \
        >"$res/system.log" 2>&1 || true
    vssh "$ip" "ls ~/Library/Logs/DiagnosticReports/ 2>/dev/null | grep -i -E 'wine|exe' | head -5 | while read f; do cat ~/Library/Logs/DiagnosticReports/\"\$f\"; done" \
        >"$res/crash-reports.txt" 2>/dev/null || true
    vssh "$ip" "sw_vers; uname -m; arch -x86_64 /usr/bin/true && echo rosetta=yes || echo rosetta=no; system_profiler SPDisplaysDataType 2>/dev/null | sed -n 1,20p" >"$res/guest-info.txt" 2>&1 || true

    # Self-containment and seal evidence (#68).
    local wlogs=("$res/wine-stderr.log" "$res/wine-stdout.log")
    [ -f "$res/launcher-logs/wine.log" ] && wlogs+=("$res/launcher-logs/wine.log")
    cat "${wlogs[@]}" 2>/dev/null | grep -E -i 'cannot find|Library not loaded|image not found' \
        | LC_ALL=C sort -u | head -20 >"$res/missing-libs.txt" || true
    if [ -n "$grep_re" ]; then
        cat "${wlogs[@]}" "$res"/launcher-logs/launch.log 2>/dev/null | grep -E -- "$grep_re" \
            | LC_ALL=C sort -u | head -20 >"$res/log-grep.txt" || true
    fi
    local h1; h1="$(bundle_hash 2>/dev/null || true)"
    if [ -n "$h0" ] && [ -n "$h1" ]; then
        [ "$h0" = "$h1" ] && modified=false || modified=true
    fi
    set +e
    vssh "$ip" "codesign --verify --deep --strict --verbose=2 \"$dst\"" >"$res/codesign-after.txt" 2>&1; cs_after_rc=$?
    set -e
    # New files in the guest home outside gogo's own dirs: saves or configs a
    # game wrote into the real ~ (design 16 decision 4).
    vssh "$ip" "find ~ -xdev -newer ~/gogo-test/.t0 -type f 2>/dev/null | sed \"s|^\$HOME/||\"" 2>/dev/null \
        | grep -i -E "$OUTSIDE_WATCH" | grep -v -E "$OUTSIDE_SKIP" | head -50 >"$res/writes-outside-gogo.txt" || true

    local nwin; nwin="$(echo "$windows" | jq length)"
    if [ "$alive" = true ] && [ "$nwin" -gt 0 ]; then status="ok"; exit_code=0
    elif [ "$alive" = true ]; then status="running_no_window"; exit_code=1
    elif [ "$launched" = true ]; then status="exited"; exit_code=1
    else status="did_not_launch"; exit_code=1; fi
    if [ "$status" = ok ] && [ "$teardown" != null ] && [ "$(jq .ok <<<"$teardown")" != true ]; then
        status="teardown_leftovers"; exit_code=1
    fi
    exit "$exit_code"
}

# Click the Gatekeeper "could not verify" dialog away (Done / OK), the way a
# user would, so it doesn't stay on top of the relaunched game.
GK_DISMISS='tell application "System Events"
  repeat with p in (every process whose name is "CoreServicesUIAgent" or name is "UserNotificationCenter")
    repeat with w in (every window of p)
      repeat with b in {"Done", "OK", "Cancel"}
        if exists button b of w then
          click button b of w
          exit repeat
        end if
      end repeat
    end repeat
  end repeat
end tell'

# Where a game escaping its prefix would write (relative to ~): the user's
# content folders, home dotfiles, and anything named after Wine or GOG.
# macOS's own churn under ~/Library and the media libraries doesn't match.
OUTSIDE_WATCH='^(Desktop|Documents|Downloads|Saved Games|My Games)/|^\.[^/]+$|^\.(wine|local|config|cache)/|wine|gog|\.exe'
OUTSIDE_SKIP='^(gogo-test/|Applications/|Library/(Application Support/gogo|Logs/gogo|Caches)/|\.(zsh|bash)_|\.ssh/|\.CFUserTextEncoding$)'

# ----------------------------------------------------------------- smoke ----
# Run the per-layer cases in a manifest (default scripts/vm-smoke.txt), one VM
# run each, and judge each summary with the case's jq check. Prints a table and
# writes smoke.json; exit 0 = all pass, 1 = a case failed, 3 = infra.
cmd_smoke() {
    local manifest="$REPO/scripts/vm-smoke.txt" out="$REPO/target/vm-test" only="" bundles
    bundles="${GOGO_VM_BUNDLES:-$REPO/target/v2}"
    while [ $# -gt 0 ]; do
        case "$1" in
            --bundles) bundles="$2"; shift 2 ;;
            --out) out="$2"; shift 2 ;;
            --only) only="$2"; shift 2 ;;
            -*) die "unknown option $1" 2 ;;
            *) manifest="$1"; shift ;;
        esac
    done
    [ -f "$manifest" ] || die "no manifest $manifest" 2
    local dir; dir="$out/smoke-$(date +%Y%m%d-%H%M%S)"
    mkdir -p "$dir"
    local results="$dir/results.jsonl" case layer bundle opts check proves
    : >"$results"
    while IFS='|' read -r case layer bundle opts check proves; do
        case="$(echo "$case" | xargs)"
        case "$case" in ""|\#*) continue ;; esac
        [ -z "$only" ] || [[ ",$only," == *",$case,"* ]] || continue
        layer="$(echo "$layer" | xargs)"; bundle="$(echo "$bundle" | xargs)"
        check="$(echo "$check" | sed 's/^ *//;s/ *$//')"; proves="$(echo "$proves" | sed 's/^ *//;s/ *$//')"
        local verdict summary="" rc=0 sj="{}"
        if [ "$bundle" = - ]; then
            verdict="todo"
        elif [ ! -d "$bundles/$bundle" ]; then
            verdict="missing"; log "$case: no bundle $bundles/$bundle"
        else
            log "case $case ($layer): $bundle $opts"
            # shellcheck disable=SC2086 # opts is a word list from the manifest
            summary="$(cmd_run_sub "$bundles/$bundle" --out "$dir/$case" $opts </dev/null | tail -1)" || rc=$?
            if [ -f "$summary" ]; then
                sj="$(cat "$summary")"
                if [ "$rc" = 3 ]; then verdict="infra"
                elif jq -e "$check" "$summary" >/dev/null 2>&1; then verdict="pass"
                else verdict="fail"; fi
            else
                verdict="infra"
            fi
        fi
        jq -n -c --arg case "$case" --arg layer "$layer" --arg bundle "$bundle" --arg opts "$opts" \
            --arg check "$check" --arg proves "$proves" --arg verdict "$verdict" --arg summary "$summary" \
            --argjson s "$sj" \
            '{case:$case, layer:$layer, bundle:$bundle, opts:$opts, check:$check, proves:$proves,
              verdict:$verdict, summary:$summary, status:($s.status // null),
              gatekeeper:($s.gatekeeper // null), runtime:($s.runtime // null), gogid:($s.gogid // null),
              missing_libs:(($s.missing_libs // []) | length), teardown_ok:$s.teardown.ok}' >>"$results"
    done <"$manifest"
    jq -s '{cases: ., pass: (map(select(.verdict == "pass")) | length),
            fail: (map(select(.verdict == "fail" or .verdict == "missing")) | length),
            infra: (map(select(.verdict == "infra")) | length)}' "$results" >"$dir/smoke.json"
    # Table (markdown, ready for a PR) and a ready-to-paste compat line.
    {
        echo "| case | layer | verdict | status | gatekeeper | missing libs | teardown |"
        echo "|---|---|---|---|---|---|---|"
        jq -r '.cases[] | "| \(.case) | \(.layer) | **\(.verdict)** | \(.status // "-") | \(.gatekeeper // "-") | \(.missing_libs) | \(if .teardown_ok == null then "-" else .teardown_ok end) |"' "$dir/smoke.json"
    } | tee "$dir/table.md"
    local first host
    first="$(jq -r '[.cases[] | select(.summary != "") | .summary][0] // ""' "$dir/smoke.json")"
    host="vm/macos-$( [ -n "$first" ] && sed -n 's/^ProductVersion:[[:space:]]*//p' "$(dirname "$first")/guest-info.txt" 2>/dev/null)"
    # One compat line per game that started (status ok) in some case.
    jq -r --arg host "$host" --arg date "$(date +%Y-%m-%d)" \
        '[.cases[] | select(.verdict == "pass" and .status == "ok" and (.gogid // "") != "")]
         | unique_by(.gogid)[]
         | "# compat/\(.gogid).toml: tested = [{ runtime = \"\(.runtime)\", host = \"\($host)\", date = \($date) }]"' \
        "$dir/smoke.json"
    echo "$dir/smoke.json"
    local f i
    f="$(jq '.fail' "$dir/smoke.json")"; i="$(jq '.infra' "$dir/smoke.json")"
    [ "$f" -gt 0 ] && exit 1
    [ "$i" -gt 0 ] && exit 3
    exit 0
}
# cmd_run exits via its EXIT trap; give each case its own process.
cmd_run_sub() { "$0" run "$@"; }

# ----------------------------------------------------------------- shell ----
cmd_shell() {
    local app="${1:-}"
    vm_exists "$GOLDEN" || die "no $GOLDEN image; run: scripts/vm-test.sh golden"
    local name; name="${PREFIX_CLONE}shell-$(date +%Y%m%d-%H%M%S)"
    "$TART" clone "$GOLDEN" "$name"
    trap 'stop_delete "$name"' EXIT
    acquire_slot 1800 || die "no free VM slot"
    local dirs=()
    [ -n "$app" ] && dirs=(--dir="app:$(cd "$(dirname "$app")" && pwd):ro")
    local ip; ip="$(start_vm "$name" "$STATE/$name.log" "${dirs[@]}")" || die "VM did not boot"
    wait_ssh "$ip" 180 || die "ssh failed"
    log "$name at $ip (shared app dir: /Volumes/My Shared Files/app); exit to delete"
    log "vnc/screen: open vnc://$ip (admin/admin) if you need to see it"
    ssh "${SSH_OPTS[@]}" -i "$KEY" -t "admin@$ip" || true
}

cmd_cleanup() {
    "$TART" list --format json | jq -r --arg p "$PREFIX_CLONE" '.[] | select(.Name|startswith($p)) | .Name' |
        while read -r n; do log "deleting $n"; stop_delete "$n"; done
    rm -rf "$LOCK"
}

cmd_status() {
    "$TART" list --format json | jq --arg g "$GOLDEN" --arg p "$PREFIX_CLONE" \
        '{golden: (map(select(.Name==$g)) | .[0] // null),
          clones: map(select(.Name|startswith($p)) | {Name, Running}),
          running: (map(select(.Running)) | length), max_running: 2}'
}

case "${1:-}" in
    golden) shift; cmd_golden "$@" ;;
    run) shift; cmd_run "$@" ;;
    smoke|matrix) shift; cmd_smoke "$@" ;;
    shell) shift; cmd_shell "$@" ;;
    cleanup) cmd_cleanup ;;
    status) cmd_status ;;
    *) sed -n '2,46p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
