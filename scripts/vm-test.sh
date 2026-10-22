#!/usr/bin/env bash
# Launch-test `gogo mac-app` bundles inside a throwaway Tart macOS VM, so game
# windows never steal focus on the developer's Mac (docs/design/08-testing.md §5.1,
# how-to: docs/vm-testing.md).
#
#   scripts/vm-test.sh golden [--refresh]        create/refresh the gog-golden image
#   scripts/vm-test.sh run <X.app> [opts]        clone → copy app → launch → collect → delete
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
#                       (default: record the block, strip quarantine, launch again)
#   --keep              leave the clone stopped-but-present for inspection
#   --wait-secs N       how long to queue for a free VM slot (default 1800)
#
# Output (machine-readable): the LAST stdout line of `run` is the summary JSON
# path; the summary is also printed to stdout as one JSON object before it.
# Exit codes: 0 = launched and still running with ≥1 window; 1 = test ran but
# the app failed (blocked/crashed/no window); 2 = usage; 3 = infrastructure
# (tart/VM/SSH problem; the result says nothing about the app).
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
// Wine puts fullscreen games (and their dialogs) on layers 25-27, not 0.
const out = ws.filter(w => !sys.includes(w.kCGWindowOwnerName) &&
    (w.kCGWindowLayer === 0 || (/wine|\.exe/i.test(w.kCGWindowOwnerName) && w.kCGWindowLayer < 1000)))
  .map(w => ({owner: w.kCGWindowOwnerName, pid: w.kCGWindowOwnerPID, layer: w.kCGWindowLayer,
              w: Math.round(w.kCGWindowBounds.Width), h: Math.round(w.kCGWindowBounds.Height)}));
JSON.stringify(out);
'

cmd_run() {
    local app="" secs=30 out="$REPO/target/vm-test" name="" quar=1 strict=0 keep=0 waits=1800
    while [ $# -gt 0 ]; do
        case "$1" in
            --secs) secs="$2"; shift 2 ;;
            --out) out="$2"; shift 2 ;;
            --name) name="$2"; shift 2 ;;
            --no-quarantine) quar=0; shift ;;
            --strict-gatekeeper) strict=1; shift ;;
            --keep) keep=1; shift ;;
            --wait-secs) waits="$2"; shift 2 ;;
            -*) die "unknown option $1" 2 ;;
            *) app="$1"; shift ;;
        esac
    done
    [ -n "$app" ] && [ -d "$app" ] && [[ "$app" == *.app ]] || die "run needs an existing X.app" 2
    app="$(cd "$app" && pwd)"
    local base; base="$(basename "$app")"
    local slug; slug="$(printf '%s' "${base%.app}" | tr -cs 'A-Za-z0-9' '-' | tr 'A-Z' 'a-z' | sed 's/^-//;s/-$//' | cut -c1-24)"
    local ts; ts="$(date +%Y%m%d-%H%M%S)"
    name="${name:-$PREFIX_CLONE$slug-$ts}"
    local res="$out/$slug-$ts"
    mkdir -p "$res"
    vm_exists "$GOLDEN" || die "no $GOLDEN image; run: scripts/vm-test.sh golden"
    [ -f "$KEY" ] || die "no ssh key at $KEY; run: scripts/vm-test.sh golden"

    # Everything below writes into the result dir; summary assembled at the end.
    local status="infra_error" exit_code=3 ip="" gk_block=false gatekeeper="not_quarantined" spctl_rc="" cs_rc=""
    local launched=false alive=false windows="[]" note=""
    finish() {
        local procs="[]"
        [ -f "$res/processes.txt" ] && procs="$(jq -R -s 'split("\n") | map(select(length>0) | sub("^ +"; ""))' "$res/processes.txt")"
        jq -n --arg status "$status" --argjson exit "$exit_code" --arg app "$app" \
              --arg vm "$name" --arg ip "$ip" --argjson secs "$secs" \
              --argjson quarantined "$([ "$quar" = 1 ] && echo true || echo false)" \
              --argjson gatekeeper_blocked "$gk_block" --arg gatekeeper "$gatekeeper" \
              --arg spctl_rc "$spctl_rc" --arg codesign_rc "$cs_rc" \
              --argjson launched "$launched" --argjson running "$alive" \
              --argjson windows "$windows" --argjson processes "$procs" \
              --arg note "$note" --arg dir "$res" \
              --arg screenshot "$([ -s "$res/screenshot.png" ] && echo "$res/screenshot.png")" \
              '{status:$status, exit_code:$exit, app:$app, vm:$vm, ip:$ip, secs:$secs,
                quarantined:$quarantined, gatekeeper:$gatekeeper, gatekeeper_blocked:$gatekeeper_blocked,
                codesign_verify_rc:$codesign_rc, spctl_rc:$spctl_rc,
                gatekeeper_screenshot:(if $gatekeeper_blocked then $dir+"/gatekeeper.png" else "" end),
                launched:$launched, running_after_secs:$running, windows:$windows,
                processes:$processes, note:$note, results_dir:$dir, screenshot:$screenshot,
                files:{codesign:($dir+"/codesign.txt"), spctl:($dir+"/spctl.txt"),
                       stdout:($dir+"/wine-stdout.log"), stderr:($dir+"/wine-stderr.log"),
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

    log "clone $GOLDEN → $name"
    "$TART" clone "$GOLDEN" "$name" || { note="tart clone failed"; exit 3; }
    acquire_slot "$waits" || { note="no free VM slot after ${waits}s"; exit 3; }
    ip="$(start_vm "$name" "$res/tart-run.log" --dir="out:$res")" \
        || { note="VM did not boot / no IP"; exit 3; }
    wait_ssh "$ip" 180 || { note="ssh to $ip failed"; exit 3; }
    log "vm $name up at $ip; copying $base"

    local G="/Volumes/My Shared Files"
    local dst="/Users/admin/Applications/$base"
    # tar over ssh, not a virtiofs share + ditto: Wine prefixes carry absolute
    # symlinks (dosdevices/z: -> /, drive_c/users/<you>/Desktop -> ~/Desktop)
    # that virtiofs fails with ELOOP. tar keeps them as links, like a real
    # zip/dmg download would, and lands on a local volume Wine can write to.
    tar -C "$(dirname "$app")" -cf - "$base" | vssh "$ip" "mkdir -p ~/Applications && tar -C ~/Applications -xf -" \
        || { note="copy into VM failed"; exit 3; }
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
    local since; since="$(vssh "$ip" 'date "+%Y-%m-%d %H:%M:%S"')"
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
            vssh "$ip" "/usr/bin/log show --style compact --start '$since' --predicate 'process == \"syspolicyd\" OR process == \"XprotectService\" OR process == \"CoreServicesUIAgent\"'" >"$res/gatekeeper.log" 2>&1 || true
            if [ "$strict" = 1 ]; then
                status="gatekeeper_$gatekeeper"; exit_code=1; note="strict: stopped at the Gatekeeper dialog"
                exit 1
            fi
            # Equivalent of the user clicking Open / "Open Anyway": drop the
            # stopped first launch + dialog, strip quarantine, launch again.
            vssh "$ip" "pkill -9 -f AppTranslocation; killall CoreServicesUIAgent; xattr -dr com.apple.quarantine \"$dst\"" >/dev/null 2>&1 || true
            sleep 2
            launch || true
            sleep 5
        fi
    fi
    [ "$(n_live)" -gt 0 ] && launched=true
    log "running for ${secs}s…"
    sleep "$secs"

    game_procs >"$res/processes.txt"
    [ "$(n_live)" -gt 0 ] && alive=true
    windows="$(vssh "$ip" "osascript -l JavaScript -e '$GUEST_PROBE'" 2>/dev/null || echo '[]')"
    echo "$windows" | jq -e . >/dev/null 2>&1 || windows="[]"
    vssh "$ip" "screencapture -x \"$G/out/screenshot.png\"" >"$res/screencapture.txt" 2>&1 || true

    # Wine-side logs: anything *.log/*.txt-ish the game or Wine left in the prefix.
    mkdir -p "$res/prefix-logs"
    vssh "$ip" "cd \"$dst/Contents/Resources\" 2>/dev/null && find . -path ./wine -prune -o -type f \\( -iname '*.log' -o -iname '*.err' -o -iname 'ddraw.ini' -o -iname 'launch.conf' \\) -size -5M -print0 | xargs -0 tar cf - 2>/dev/null" \
        | tar -C "$res/prefix-logs" -xf - 2>/dev/null || true
    vssh "$ip" "/usr/bin/log show --style compact --start '$since' --predicate 'process == \"syspolicyd\" OR process == \"XprotectService\" OR process CONTAINS[c] \"wine\" OR eventMessage CONTAINS[c] \"$base\"'" \
        >"$res/system.log" 2>&1 || true
    vssh "$ip" "ls ~/Library/Logs/DiagnosticReports/ 2>/dev/null | grep -i -E 'wine|exe' | head -5 | while read f; do cat ~/Library/Logs/DiagnosticReports/\"\$f\"; done" \
        >"$res/crash-reports.txt" 2>/dev/null || true
    vssh "$ip" "sw_vers; uname -m; arch -x86_64 /usr/bin/true && echo rosetta=yes || echo rosetta=no; system_profiler SPDisplaysDataType 2>/dev/null | sed -n 1,20p" >"$res/guest-info.txt" 2>&1 || true

    local nwin; nwin="$(echo "$windows" | jq length)"
    if [ "$alive" = true ] && [ "$nwin" -gt 0 ]; then status="ok"; exit_code=0
    elif [ "$alive" = true ]; then status="running_no_window"; exit_code=1
    elif [ "$launched" = true ]; then status="exited"; exit_code=1
    else status="did_not_launch"; exit_code=1; fi
    exit "$exit_code"
}

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
    shell) shift; cmd_shell "$@" ;;
    cleanup) cmd_cleanup ;;
    status) cmd_status ;;
    *) sed -n '2,28p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
