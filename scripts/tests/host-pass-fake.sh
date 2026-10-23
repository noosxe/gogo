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
# Every process this test starts, still alive: the stubs (their command line
# holds $T) and the detached fake games (argv[0] replaced, so by pid file).
stub_pids() {
    pgrep -f "$T/" 2>/dev/null || true
    local f p
    for f in "$T"/*.pid "$T"/*/*.pid; do
        [ -f "$f" ] || continue
        p="$(cat "$f")"
        if kill -0 "$p" 2>/dev/null; then echo "$p"; fi
    done
}
# Reap everything, on success, failure or interrupt alike. Every fake process
# also ends by itself within LIFETIME seconds, should this never run (the
# test itself SIGKILLed).
cleanup() {
    local left
    left="$(stub_pids)"
    # shellcheck disable=SC2086 # pids
    if [ -n "$left" ]; then kill -KILL $left 2>/dev/null || true; fi
    [ -n "${KEEP:-}" ] || rm -rf "$T"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM HUP
LIFETIME=60
GOGID=1669531175
# The fake Wine processes: an ad-hoc signed copy of /bin/sleep. macOS hides
# the environment of Apple-signed binaries from `ps -E` (Wine isn't one), and
# nix's coreutils sleep dispatches on argv[0], which `exec -a` replaces.
WINEPROC="$T/bin/wine-proc"
mkdir -p "$T/bin"
cp /bin/sleep "$WINEPROC"
codesign -f -s - "$WINEPROC" 2>/dev/null || true
fails=0
# What Wine prints with WINEDEBUG=+loaddll (debugstr_w doubles backslashes).
loaddll() { # dll builtin|native
    printf '0024:trace:loaddll:build_module Loaded L"C:\\\\windows\\\\system32\\\\%s" at 000000006A240000: %s\n' "$1" "$2"
}
{ loaddll KERNEL32.dll builtin; loaddll d3d11.dll builtin; loaddll dxgi.dll builtin
  loaddll D3DCompiler_43.dll native; } >"$T/loaddll-d3d.txt"
{ loaddll KERNEL32.dll builtin; loaddll opengl32.dll builtin; } >"$T/loaddll-gl.txt"
ok() { printf 'ok   %s\n' "$1"; }
bad() { printf 'FAIL %s\n' "$1"; fails=$((fails + 1)); }
check() { # name jq-expr summary
    if jq -e "$2" "$3" >/dev/null; then ok "$1"; else bad "$1: $2"; fi
}

# fake_bundle <name> <mode>: a state dir and a bundle whose launcher behaves
# as <mode> says:
#   good      runs until SIGTERM, then stops its game and exits 143
#   stubborn  ignores SIGTERM, and so does its game: only wineserver -k ends it
#   quits     the game exits 0 by itself after 1 s; it draws with OpenGL
#             (loads opengl32 only), the others with D3D11
#   d3dmetal  like good, on D3DMetal (Unity reports "AMD Compatibility Mode"),
#             and the game keeps logging (good goes silent after start-up)
# good and stubborn start the game the way winecx does, outside the
# launcher's process tree: one process with WINEPREFIX=<prefix> in its
# environment, one whose argv[0] is the exe's Windows path. An older run's
# Player-prev.log (another renderer) is already in the prefix.
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
WORKDIR='C:\GOG\Fake Game'
WINESERVER='runtimes/fake/engine/bin/wineserver'
EOF
    # wineserver -k: record the prefix it was given, end the game.
    cat >"$st/runtimes/fake/engine/bin/wineserver" <<EOF
#!/bin/bash
printf '%s %s\n' "\$WINEPREFIX" "\$*" >"$T/$name/wineserver-called"
for f in "$T/$name/game.pid" "$T/$name/exe.pid"; do
    [ -f "\$f" ] && kill -KILL "\$(cat "\$f")" 2>/dev/null
done
exit 0
EOF
    local ll="$st/games/$GOGID/prefix/drive_c/users/crossover/AppData/LocalLow/Chance/Neo Cab"
    printf 'Direct3D:\r\n    Renderer: AMD Compatibility Mode (ID=0x66af)\r\n' >"$ll/Player-prev.log"
    touch -t 202001010000 "$ll/Player-prev.log"
    cat >"$app/Contents/MacOS/Fake Game" <<EOF
#!/bin/bash
MODE='$mode'
mkdir -p "\$GOGO_LOG_DIR"
RUN="\$GOGO_LOG_DIR/\$(date +%Y%m%d-%H%M%S)-\$\$.log"
if [ "\$MODE" = d3dmetal ]; then
    # Header lines name D3DMetal too: they must not count as evidence.
    printf '# gogo run log: Fake Game ($GOGID)\n# renderer: d3dmetal/4.0b3\n# runtime_dir: D3DMetal-4.0b3\n# ---\n' >"\$RUN"
    gpu='AMD Compatibility Mode (ID=0x66af)'
else
    printf '# gogo run log: Fake Game ($GOGID)\n# renderer: dxmt/v0.80\n# ---\n' >"\$RUN"
    gpu='Apple M4 Pro (ID=0x0)'
fi
echo "\$(date) [\$\$] launch Fake Game" >>"\$GOGO_LOG_DIR/launch.log"
printf '%s\n' "\$WINEDEBUG" >"$T/$name/winedebug"
if [ "\$MODE" = quits ]; then tee -a "\$RUN" <"$T/loaddll-gl.txt" >&2; else tee -a "\$RUN" <"$T/loaddll-d3d.txt" >&2; fi
pl="$st/games/$GOGID/prefix/drive_c/users/crossover/AppData/LocalLow/Chance/Neo Cab/Player.log"
printf 'Direct3D:\r\n    Version:  Direct3D 11.0 [level 11.1]\r\n    Renderer: %s\r\n' "\$gpu" >"\$pl"
echo 'Creating D3D11 RenderSystem' >"$st/games/$GOGID/game/ogre.log"
{ echo 'info:dxmt: Using feature level D3D_FEATURE_LEVEL_11_1'
  echo '0124:err:combase:RoGetActivationFactory Failed to find library for L"Windows.System.DispatcherQueue"'; } | tee -a "\$RUN"
if [ "\$MODE" = quits ]; then sleep 1; printf '# ---\n# exit: 0\n' >>"\$RUN"; exit 0; fi
ign=:
if [ "\$MODE" = stubborn ]; then trap '' TERM; ign="trap '' TERM"; fi
( (eval "\$ign"; export WINEPREFIX="$st/games/$GOGID/prefix"; exec "$WINEPROC" $LIFETIME) & echo \$! >"$T/$name/game.pid" )
( (eval "\$ign"; exec -a 'C:\GOG\Fake Game\Fake.exe' "$WINEPROC" $LIFETIME) & echo \$! >"$T/$name/exe.pid" )
TICK=""
if [ "\$MODE" = d3dmetal ]; then
    (for _ in \$(seq \$((2 * $LIFETIME))); do echo "0130:fixme:tick"; echo tick >>"\$pl"; sleep 0.5; done) | tee -a "\$RUN" &
    TICK=\$!
fi
if [ "\$MODE" != stubborn ]; then
    trap 'pkill -P \$\$; kill \$(cat "$T/$name/game.pid" "$T/$name/exe.pid") \$TICK; printf "# ---\n# exit: 143 (SIGTERM)\n" >>"\$RUN"; exit 143' TERM
fi
# Idle, not spinning; gone after LIFETIME seconds even if nobody stops it.
for _ in \$(seq $LIFETIME); do sleep 1; done
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

pid_of() { cat "$T/$1" 2>/dev/null; }
alive() { kill -0 "$1" 2>/dev/null; }
# check that a summary list (processes, leftovers: "pid stat command")
# holds / lacks the pids in the given files.
has_pids() { # name summary jq-path files…
    local name="$1" sum="$2" path="$3" f want=true; shift 3
    case "$1" in --not) want=false; shift ;; esac
    for f in "$@"; do
        if [ "$(jq --arg p "$(pid_of "$f")" "$path | map(split(\" \")[0]) | index(\$p) != null" "$sum")" != "$want" ]; then
            bad "$name ($f)"; return
        fi
    done
    ok "$name"
}

# Another Wine game (another prefix and folder), outside every tree: never
# counted, never stopped.
( (export WINEPREFIX="$T/other/prefix"; exec -a 'C:\GOG\Other Game\Other.exe' "$WINEPROC" $LIFETIME) & echo $! >"$T/decoy.pid" )

# 1. Cooperative launcher: SIGTERM teardown, logs collected. A WINEDEBUG of
# your own is kept, +loaddll added after it.
fake_bundle good good
s="$(WINEDEBUG=fixme-all run_hp good)"
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
check "good: graphics DLLs from +loaddll, each line counted once" \
    '.markers.graphics_dlls == ["1 d3d11.dll: builtin", "1 d3dcompiler_43.dll: native", "1 dxgi.dll: builtin"]
     and .markers.feature_level == ["1 info:dxmt: Using feature level D3D_FEATURE_LEVEL_11_1"]' "$s"
if [ "$(cat "$T/good/winedebug")" = "fixme-all,+loaddll" ]; then ok "good: launcher got WINEDEBUG=fixme-all,+loaddll"
else bad "good: launcher got WINEDEBUG=$(cat "$T/good/winedebug")"; fi
check "good: err lines counted" '.err_lines | any(test("RoGetActivationFactory"))' "$s"
if grep -q -F "$HOME/" "$s"; then bad "good: summary contains \$HOME"; else ok "good: summary has no \$HOME"; fi
if [ -f "$(dirname "$s")/launcher-logs/launch.log" ]; then ok "good: launch.log copied"; else bad "good: launch.log missing"; fi
has_pids "good: detached game found (WINEPREFIX, exe path)" "$s" .processes good/game.pid good/exe.pid
has_pids "good: another prefix's game not counted" "$s" .processes --not decoy.pid
check "good: silent after start-up shows in log_quiet_secs" \
    '.log_quiet_secs >= 2 and .engine_log_quiet_secs >= 2 and .markers.d3dmetal == []' "$s"
check "good: only this run's engine logs, no CRs" \
    '(.engine_logs | any(test("Player-prev")) | not) and (.markers.unity | any(test("AMD")) | not) and ([.markers[][]] | any(test("\r")) | not)' "$s"
if alive "$(pid_of good/game.pid)" || alive "$(pid_of good/exe.pid)"; then bad "good: detached game left running"; else ok "good: detached game stopped"; fi
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
has_pids "stubborn: detached game reported as leftovers" "$s" .teardown.leftovers stubborn/game.pid stubborn/exe.pid
if alive "$(pid_of stubborn/game.pid)" || alive "$(pid_of stubborn/exe.pid)"; then bad "stubborn: detached game left running"; else ok "stubborn: detached game stopped"; fi
if pgrep -f "$T/stubborn/" >/dev/null; then bad "stubborn: processes left"; else ok "stubborn: nothing left running"; fi

# 3. The game quits by itself before --secs.
fake_bundle quits quits
s="$(run_hp quits --secs 6)"
[ -f "$s" ] || { bad "quits: no summary"; s=/dev/null; }
check "quits: exited early, exit code from the run log" \
    '.status == "exited" and .exited_after_secs != null and .exited_after_secs < 6 and .run_exit == "0" and .teardown.ok' "$s"
check "quits: an OpenGL game shows opengl32 only" '.markers.graphics_dlls == ["1 opengl32.dll: builtin"] and .winedebug == "+loaddll"' "$s"

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

# 4. D3DMetal: its marker comes from Unity's renderer line, not from the run
# log's header; a game that keeps logging isn't quiet.
fake_bundle d3dm d3dmetal
s="$(run_hp d3dm)"
[ -f "$s" ] || { bad "d3dmetal: no summary ($(cat "$T/d3dm/stderr"))"; s=/dev/null; }
check "d3dmetal: marker from Unity's renderer, none from the header" \
    '(.markers.d3dmetal | length > 0) and (.markers.d3dmetal | all(test("AMD Compatibility Mode")))
     and ([.markers[][]] | any(test("^[0-9]+ # ")) | not)' "$s"
check "d3dmetal: logs not quiet, clean teardown" \
    '.log_quiet_secs <= 1 and .engine_log_quiet_secs <= 1 and .teardown.ok' "$s"

if alive "$(pid_of decoy.pid)"; then ok "decoy: the other game survived"; else bad "decoy: the other game was stopped"; fi
kill -KILL "$(pid_of decoy.pid)" 2>/dev/null || true
sleep 1
left="$(stub_pids)"
if [ -z "$left" ]; then ok "nothing this test started is still running"
else bad "still running: $(tr "\n" " " <<<"$left")"; fi

if [ "$fails" != 0 ]; then echo "$fails failed"; exit 1; fi
echo "all host-pass fake cases pass"
