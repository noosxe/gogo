#!/usr/bin/env bash
# Headless test of scripts/host-pass.sh against fake bundles: a stub launcher
# that writes a #77-style run log and a Unity Player.log, a stub wineserver.
# No Wine, no window, nothing takes the screen; safe for agents and CI.
#
#   scripts/tests/host-pass-fake.sh      exit 0 = all cases pass
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HP="$REPO/scripts/host-pass.sh"
command -v jq >/dev/null 2>&1 || { echo "needs jq (nix develop)" >&2; exit 3; }
T="$(mktemp -d "${TMPDIR:-/tmp}/host-pass-fake.XXXXXX")"
trap 'pkill -f "$T/" 2>/dev/null || true; [ -n "${KEEP:-}" ] || rm -rf "$T"' EXIT
GOGID=1669531175
fails=0
ok() { printf 'ok   %s\n' "$1"; }
bad() { printf 'FAIL %s\n' "$1"; fails=$((fails + 1)); }
check() { # name jq-expr summary
    if jq -e "$2" "$3" >/dev/null; then ok "$1"; else bad "$1: $2"; fi
}

# fake_bundle <name> <mode>: a state dir and a bundle whose launcher behaves
# as <mode> says:
#   good      runs until SIGTERM, then stops its game and exits 143
#   stubborn  ignores SIGTERM, and so does its game: only wineserver -k ends it
#   quits     the game exits 0 by itself after 1 s
fake_bundle() {
    local name="$1" mode="$2" st="$T/$1/state" app="$T/$1/Fake Game.app"
    mkdir -p "$st/games/$GOGID/prefix/drive_c/users/crossover/AppData/LocalLow/Chance/Neo Cab" \
             "$st/games/$GOGID/game" "$st/runtimes/fake/engine/bin" "$app/Contents/MacOS" "$app/Contents/Resources"
    cat >"$app/Contents/Resources/launch.conf" <<EOF
GOGID='$GOGID'
TITLE='Fake Game'
GOGO_STATE='$T/$name/nonexistent'
EOF
    cat >"$st/games/$GOGID/launch.conf" <<EOF
RUNTIME='winecx/24.0.7_7'
RENDERER='dxmt/v0.80'
RUNTIME_DIR='runtimes/winecx/24.0.7_7+dxmt-v0.80/engine'
VERBS=(d3dcompiler_43 xinput)
PREFIX='games/$GOGID/prefix'
WINESERVER='runtimes/fake/engine/bin/wineserver'
EOF
    # wineserver -k: record the prefix it was given, end the game.
    cat >"$st/runtimes/fake/engine/bin/wineserver" <<EOF
#!/bin/bash
printf '%s %s\n' "\$WINEPREFIX" "\$*" >"$T/$name/wineserver-called"
[ -f "$T/$name/game.pid" ] && kill -KILL "\$(cat "$T/$name/game.pid")" 2>/dev/null
exit 0
EOF
    cat >"$app/Contents/MacOS/Fake Game" <<EOF
#!/bin/bash
MODE='$mode'
mkdir -p "\$GOGO_LOG_DIR"
RUN="\$GOGO_LOG_DIR/\$(date +%Y%m%d-%H%M%S)-\$\$.log"
printf '# gogo run log: Fake Game ($GOGID)\n# renderer: dxmt/v0.80\n# ---\n' >"\$RUN"
echo "\$(date) [\$\$] launch Fake Game" >>"\$GOGO_LOG_DIR/launch.log"
pl="$st/games/$GOGID/prefix/drive_c/users/crossover/AppData/LocalLow/Chance/Neo Cab/Player.log"
printf 'Direct3D:\n    Version:  Direct3D 11.0 [level 11.1]\n    Renderer: Apple M4 Pro (ID=0x0)\n' >"\$pl"
echo 'Creating D3D11 RenderSystem' >"$st/games/$GOGID/game/ogre.log"
{ echo 'info:dxmt: Using feature level D3D_FEATURE_LEVEL_11_1'
  echo '0124:err:combase:RoGetActivationFactory Failed to find library for L"Windows.System.DispatcherQueue"'; } | tee -a "\$RUN"
case "\$MODE" in
    quits) sleep 1; printf '# ---\n# exit: 0\n' >>"\$RUN"; exit 0 ;;
    stubborn) trap '' TERM; (trap '' TERM; exec sleep 300) & ;;
    *) sleep 300 & ;;
esac
echo \$! >"$T/$name/game.pid"
[ "\$MODE" = stubborn ] || trap 'kill \$!; printf "# ---\n# exit: 143 (SIGTERM)\n" >>"\$RUN"; exit 143' TERM
while :; do wait; [ "\$MODE" = stubborn ] || break; done
EOF
    chmod +x "$app/Contents/MacOS/Fake Game" "$st/runtimes/fake/engine/bin/wineserver"
}

run_hp() { # name args… → summary path (last stdout line); exit code in $T/<name>/rc
    local name="$1"; shift
    local rc=0
    GOGO_HOME="$T/$name/state" GOGO_LOG_DIR="$T/$name/logs" \
        "$HP" "$T/$name/Fake Game.app" --out "$T/$name/out" --secs 3 --kill-wait 3 "$@" \
        >"$T/$name/stdout" 2>"$T/$name/stderr" || rc=$?
    echo "$rc" >"$T/$name/rc"
    tail -1 "$T/$name/stdout"
}

# 1. Cooperative launcher: SIGTERM teardown, logs collected.
fake_bundle good good
s="$(run_hp good)"
[ -f "$s" ] || { bad "good: no summary ($(cat "$T/good/stderr"))"; s=/dev/null; }
check "good: exit 1 (alive, no window)" '.status == "running_no_window" and .exit_code == 1' "$s"
if [ "$(cat "$T/good/rc")" = 1 ]; then ok "good: process exit code 1"; else bad "good: rc $(cat "$T/good/rc")"; fi
check "good: renderer and verbs from the game's launch.conf" \
    '.renderer == "dxmt/v0.80" and .verbs == "d3dcompiler_43 xinput" and (.runtime_dir | endswith("+dxmt-v0.80/engine"))' "$s"
check "good: clean SIGTERM teardown" \
    '.teardown.ok and .teardown.leftovers == [] and (.teardown.wineserver_k_fallback | not)' "$s"
# shellcheck disable=SC2016 # $p is jq's
check "good: this launcher's run log, with its exit footer" \
    '(.teardown.launcher_pid as $p | .files.run_log | endswith("-" + $p + ".log")) and .run_exit == "143 (SIGTERM)"' "$s"
check "good: engine logs collected" \
    '.engine_logs | (any(endswith("Neo Cab/Player.log")) and any(. == "game/ogre.log"))' "$s"
check "good: markers" \
    '(.markers.feature_level | any(test("D3D_FEATURE_LEVEL_11_1"))) and (.markers.unity | any(test("Renderer: Apple M4 Pro")))
     and (.markers.ogre | any(test("D3D11 RenderSystem"))) and .markers.metal_layer_failed == []' "$s"
check "good: err lines counted" '.err_lines | any(test("RoGetActivationFactory"))' "$s"
if grep -q -F "$HOME/" "$s"; then bad "good: summary contains \$HOME"; else ok "good: summary has no \$HOME"; fi
if [ -f "$(dirname "$s")/launcher-logs/launch.log" ]; then ok "good: launch.log copied"; else bad "good: launch.log missing"; fi
if pgrep -f "$T/good/" >/dev/null; then bad "good: processes left"; else ok "good: nothing left running"; fi

# 2. Launcher and game ignore SIGTERM: prefix-scoped wineserver -k fallback.
fake_bundle stubborn stubborn
s="$(run_hp stubborn)"
[ -f "$s" ] || { bad "stubborn: no summary ($(cat "$T/stubborn/stderr"))"; s=/dev/null; }
check "stubborn: teardown_leftovers" '.status == "teardown_leftovers" and .exit_code == 1' "$s"
check "stubborn: leftovers recorded, fallback used" \
    '(.teardown.ok | not) and (.teardown.leftovers | length > 0) and .teardown.wineserver_k_fallback' "$s"
if [ "$(cat "$T/stubborn/wineserver-called" 2>/dev/null)" = "$T/stubborn/state/games/$GOGID/prefix -k" ]; then
    ok "stubborn: wineserver -k on the game's prefix"
else bad "stubborn: wineserver call: $(cat "$T/stubborn/wineserver-called" 2>/dev/null)"; fi
sleep 1
if pgrep -f "$T/stubborn/" >/dev/null; then bad "stubborn: processes left"; else ok "stubborn: nothing left running"; fi

# 3. The game quits by itself before --secs.
fake_bundle quits quits
s="$(run_hp quits)"
[ -f "$s" ] || { bad "quits: no summary"; s=/dev/null; }
check "quits: exited early, exit code from the run log" \
    '.status == "exited" and .exited_after_secs != null and .exited_after_secs < 3 and .run_exit == "0" and .teardown.ok' "$s"

# 4. --dry-run launches nothing.
fake_bundle dry good
rc=0; GOGO_HOME="$T/dry/state" "$HP" "$T/dry/Fake Game.app" --dry-run >"$T/dry/out.json" 2>/dev/null || rc=$?
check "dry-run: resolved paths" \
    '.dry_run and .gogid == "'"$GOGID"'" and (.prefix | endswith("games/'"$GOGID"'/prefix")) and .renderer == "dxmt/v0.80"' "$T/dry/out.json"
if [ "$rc" = 0 ] && [ ! -e "$T/dry/game.pid" ]; then ok "dry-run: nothing launched"; else bad "dry-run: rc $rc or launched"; fi

# 5. Usage errors.
rc=0; "$HP" >/dev/null 2>&1 || rc=$?
if [ "$rc" = 2 ]; then ok "usage: no bundle → 2"; else bad "usage: rc $rc"; fi
mkdir -p "$T/notgogo.app"
rc=0; "$HP" "$T/notgogo.app" >/dev/null 2>&1 || rc=$?
if [ "$rc" = 2 ]; then ok "usage: not a gogo bundle → 2"; else bad "usage: rc $rc"; fi

if [ "$fails" != 0 ]; then echo "$fails failed"; exit 1; fi
echo "all host-pass fake cases pass"
