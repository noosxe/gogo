# Launch-testing `.app` bundles in a Tart VM

`gogo mac-app` bundles are launch-tested inside a throwaway macOS VM, never on the
developer's Mac: a launched game grabs focus and full-screens over whatever you were doing.
`scripts/vm-test.sh` wraps [Tart](https://tart.run) so that one command clones a clean
guest, runs the bundle like a freshly downloaded app, collects evidence and deletes the
clone. Strategy context: [design/08-testing.md §5.1](design/08-testing.md). The one exception is
rendering on DXMT, D3DMetal or OpenGL, which the VM can't do: a human runs that on the
host ([Host pass](#host-pass-real-hardware-run-by-a-human-80)).

Findings and limits below were measured on 2026-10-22: host M4 Pro, macOS 27.0.1, Tart
2.40.1, guest `macos-golden-gate-base` (macOS 27.0, 26A428).

## Prerequisites (host, once)

| What | How |
|---|---|
| Apple Silicon Mac, macOS ≥ guest version | guest must not be newer than the host |
| Tart | `brew install openai/tools/tart`. The project moved from `cirruslabs` to `openai`, and the old `cirruslabs/cli` tap is stale and fails on Homebrew 7. Homebrew 7 also asks once for `brew trust --formula openai/tools/softnet`. |
| ~45 GB free disk | base image ≈ 41 GB on disk; clones are APFS copy-on-write and cost only what they write |
| `jq`, `shellcheck` | in the Nix dev shell; the script re-execs itself under `nix develop` if `jq` is missing |

## One-time golden image

```sh
scripts/vm-test.sh golden            # create, or re-provision an existing gog-golden
scripts/vm-test.sh golden --refresh  # delete and rebuild from the newest base image
```

Clones `ghcr.io/cirruslabs/macos-golden-gate-base:latest` (falls back to
`macos-tahoe-base`; override with `GOGO_VM_BASE`) to `gog-golden`, sets 4 CPU / 8 GB,
and provisions it over SSH. Idempotent:

- **SSH key login**: per-host key in `~/.local/state/gogo-vm/id_ed25519`, installed via
  the image's `admin`/`admin` password once (no `sshpass`; uses `SSH_ASKPASS`).
- **Rosetta**: installed if missing (the current image already ships it).
- **Gatekeeper back ON.** Cirrus images ship with `spctl` assessments *disabled*. Without
  this, `spctl` says "accepted" for anything and the test proves nothing.
- **Screen-capture consent pre-approved.** On macOS 15 and later, an SSH `screencapture`
  makes macOS show *"com.apple.sshd-session is requesting to bypass the system private window
  picker"* on top of the game. The script writes far-future dates into replayd's
  `ScreenCaptureApprovals` for `sshd-keygen-wrapper` (the TCC-responsible binary) to stop it.
- No sleep, no screensaver, no crash-reporter dialogs, no Software Update banners.
- Shut down from inside the guest (`shutdown -h`), not `tart stop`. A plain `tart stop`
  lost the provisioning once.

**No manual steps.** The Cirrus image already grants Accessibility, ScreenCapture,
PostEvent and AppleEvents in TCC to `sshd-keygen-wrapper` and `osascript`, with SIP off.
So `osascript -e 'tell application "System Events" to click at {x, y}'` works over SSH.
No `cliclick` and no click-through needed.

## Daily use

```sh
scripts/vm-test.sh run "/path/Game.app" --secs 30   # full clone → test → delete
scripts/vm-test.sh run Game.app --strict-gatekeeper # stop at the Gatekeeper dialog
scripts/vm-test.sh run Game.app --no-quarantine     # skip the download simulation
scripts/vm-test.sh run Game.app --keep              # keep the stopped clone for poking
scripts/vm-test.sh run Game.app --kill term         # #42: kill the launcher, assert no Wine left
scripts/vm-test.sh run Game.app --runtimes copy     # copy store runtimes instead of sharing them
scripts/vm-test.sh run Game.app --audio             # give the guest a sound device (off by default)
scripts/vm-test.sh run Game.app --osascript scripts/vm-hooks/press-return.applescript --hook-delay 25
scripts/vm-test.sh smoke                            # per-layer smoke set (below)
scripts/vm-test.sh shell [Game.app]                 # interactive clone, deleted on exit
scripts/vm-test.sh status                           # golden, clones, running count (JSON)
scripts/vm-test.sh cleanup                          # stop + delete every gog-test-* clone
```

What `run` does:

1. `tart clone gog-golden gog-test-<slug>-<ts>`, then boot it headless with the results
   dir shared as `/Volumes/My Shared Files/out`, with `--no-audio --no-clipboard` (below).
2. Copies the bundle to `~/Applications` by streaming `tar` over SSH. A virtiofs
   share + `ditto` doesn't work: Wine prefixes contain absolute symlinks (`dosdevices/z:`
   → `/`, `drive_c/users/<you>/Desktop` → `/Users/<you>/Desktop`), and virtiofs fails
   those with ELOOP. For a v2 bundle it then seeds the game's state (next section).
3. Sets `com.apple.quarantine` (`0081;<hex time>;Safari;`), then records
   `codesign --verify --deep --strict` and `spctl -a -vv`.
4. `open`s the bundle via LaunchServices, like a double-click, with Wine's stdout/stderr
   sent to the results dir. After 8 s it classifies Gatekeeper: `allowed`, `prompted`
   (process present but SIGSTOPped behind the first-launch dialog) or `blocked` (no
   process). Then it screenshots the dialog (`gatekeeper.png`). Unless
   `--strict-gatekeeper` is set, it clicks the dialog away (System Events, *Done*),
   kills the held launch, strips quarantine (what "Open Anyway" does) and relaunches,
   so `screenshot.png` shows the game, not the dialog. `gatekeeper_dialog_left` says
   whether one was still on screen at the end.
5. Runs the `--click` / `--osascript` hooks, if any, then waits `--secs` and collects:
   process list, on-screen windows (CoreGraphics, from JXA), `screencapture -x`, the
   launcher's logs, Wine logs from the prefix, a `log show` slice (syspolicyd, XProtect,
   wine), crash reports and guest info.
6. With `--kill`, signals the launcher and checks for leftovers (see *Teardown*).
7. Records the seal and self-containment evidence: a second strict `codesign` verify,
   whether anything under `Contents/` changed, missing-library lines from the logs, and
   new files in the guest home outside gogo's dirs.
8. Stops and deletes the clone, even on error or Ctrl-C.

**Two-VM limit.** macOS allows at most two macOS guests at once. `run`/`shell` take a slot
lock (`~/.local/state/gogo-vm/slot.lock`) and *queue* for up to `--wait-secs` (default 1800)
while two `tart run` processes exist. Linux VMs (Docker Desktop) don't count.

## Audio and clipboard are off

Every VM the script boots (golden, `run`, `smoke`, `shell`) gets `tart run --no-audio
--no-clipboard`. By default Tart connects the guest's sound device to the host both ways,
so game audio played on your speakers while you worked, and the guest microphone was the
host's real one. On `winecx` that link is also what made the guest ask *"Allow 'wine' to
access your microphone?"*: the CX engine opens CoreAudio input, and the game waited
behind the prompt (`running_no_window`). With `--no-audio` the prompt is gone (TAG on
winecx 24.0.7_7, 2026-10-23). `tcc_prompt_left` in the summary still records any
permission prompt on screen at the end.

**Caveat:** with no audio device, some games may fail to start, or log Wine audio
errors, only in the VM. Re-run with `--audio` (also accepted by `smoke` and `shell`;
the summary records `"audio": true`) before treating that as a real failure.

## v2 bundles: seeding the state dir (design 16)

A `--portable` bundle (#84, design 16 §10) carries its engine and game and sets its own
prefix up, so it needs no seeding: run it with `--no-state` (`--state` is refused for
it). The summary says `model: portable`, `renderer` and `runtime` come from its TOML, and
the prefix logs are collected from the guest's `games/<gogid>/`, which its launcher
created.

```sh
scripts/vm-test.sh run "target/portable/Pharaoh Gold.app" --no-state --secs 60
```

A v2 `.app` is only a ~10 KiB launcher. The prefix, game files and effective config live
in `~/Library/Application Support/gogo/games/<gogid>/`, and Wine comes from gogo's
runtime store. So for any bundle with `Contents/Resources/gogo-launch.toml` (and no
`Resources/wine`), `run` seeds the guest by default (`--no-state` skips it, old
self-contained bundles are left as they were). What to seed is derived, not hand-picked:

The host state dir is `GOGO_VM_HOST_STATE` or `GOGO_HOME` (gogo's own override, which
is the state dir itself), else the `[gogo] state` the bundle recorded when it was built,
else `~/Library/Application Support/gogo`. So a bundle built with an isolated
`GOGO_HOME` is seeded from there without extra flags. `state.host` in the summary says
which dir was used.

| From | Seeded | How |
|---|---|---|
| `gogid` in `gogo-launch.toml` | `games/<gogid>/` (prefix, game, `launch.conf`, `state.toml`), `compat/<gogid>.toml` if present | `tar` over SSH (writable) |
| `runtime` in `gogo-launch.toml`, plus `RUNTIME_DIR`/`FRAMEWORKS` in the game's `launch.conf` (an L4 retarget or a derived `<ver>+<renderer>-<rver>` runtime) | `runtimes/<id>/` (and `renderers/<id>/`) | read-only virtiofs share, symlinked into the guest store |
| `runtime = "path:<dir>"` (outside the store) | that dir at the same absolute path | share + `sudo ln -s` |

Everything lands under the guest `admin` user's `~/Library/Application Support/gogo`.
That works because the launcher writes state paths relative to the state dir
(`PREFIX='games/<gogid>/prefix'`) and falls back to `~/Library/Application Support/gogo`
when the recorded `gogo.state` doesn't exist. The guest has no gogo, so the prefix must
already be up to date on the host: a stale stamp makes the launcher log "no gogo
found" and launch as-is.

**Runtimes are shared, not copied.** A store runtime is 0.5–1 GB and never written,
so by default each `runtimes/<id>` dir is mounted read-only (`tart run --dir`) and
nothing is copied, however many runs use it. Store runtimes contain only relative
symlinks (`winecx`'s `Frameworks/` dylib links, a derived runtime's `Frameworks` →
`../<base>/Frameworks`), so virtiofs's ELOOP problem doesn't apply. Both modes keep
symlinks as symlinks. `--runtimes copy` streams them with `tar` instead, for when you
suspect the share itself. It copies `runtimes/<id>/<ver>` per referenced version, plus
the base a derived runtime links to. The
launcher's logs come back in `launcher-logs/`; the summary lists what was seeded under
`state.units`.

## Teardown check (#42)

`--kill term` lets the game run `--secs`, then sends `SIGTERM` to the launcher (the
bash process under `Contents/MacOS/` whose parent is launchd), waits `--kill-wait`
(15 s) and lists anything Wine left: `wineserver`, `wine*-preloader`, `*.exe`. The guest
runs nothing else Wine, so every match belongs to this game's prefix. The result is
`teardown` in the summary:

```json
"teardown": {"signal": "TERM", "launcher_pid": "812", "wait_secs": 15, "leftovers": [], "ok": true}
```

A non-empty `leftovers` turns an `ok` run into `teardown_leftovers` (exit 1).
`--kill kill` sends `SIGKILL`, which the launcher can't trap (design 16 §8), so it only
records what stays behind. Measured 2026-10-23 on TAG: after `SIGTERM` nothing is left
(the launcher logs `got SIGTERM` and `teardown done`); after `SIGKILL` the game, its
`wineserver` and all services keep running, along with the launcher's two `tee`
subshells.

## First-run dialogs (`--click`, `--osascript`)

Some games stop at their own dialog on first launch (docs/19 §8.2 Q8). Hooks run once,
`--hook-delay` seconds (default 10) after the launch is up: each `--click X,Y` (screen
points of the 1024×768 display) through System Events, then the `--osascript` file. Output
lands in `hooks.txt`.

A synthetic `click at` reaches Wine's window but didn't press its button (Carmageddon,
2026-10-23). Bringing `wine` to the front and pressing Return does, which is what
[`scripts/vm-hooks/press-return.applescript`](../scripts/vm-hooks/press-return.applescript)
does. Carmageddon needs `--hook-delay 25`: its "Hardware Capabilities" dialog takes about
20 s to appear. Past it, the game switches to 640×480 and stops at `FAILED:
FindDepthBufferFormat`, because the guest has no OpenGL for wined3d. That's a host-pass
item (#80), not a bundle bug.

## Smoke set

`scripts/vm-test.sh smoke` runs the cases in [`scripts/vm-smoke.txt`](../scripts/vm-smoke.txt),
one VM run each, against bundles in `target/v2` (`--bundles DIR` or `GOGO_VM_BUNDLES`).
Each case is a line with its layer, bundle, run options, a `jq` check on `summary.json`
and what it proves. `--only a,b` runs a subset. It prints a markdown table, the
`tested = [...]` compat line for each game that started, and the path of `smoke.json`
(exit 0 all pass, 1 a case failed, 3 infra).

A case whose check fails with `running_no_window` while Wine logged that wined3d got no
GL context is reported as **`vm-limit`**, not `fail`, and doesn't fail the set. The guest
has no OpenGL, so a D3D-on-wined3d title can be fine on a Mac and windowless here. The
summary's `vm_no_gl` holds the log lines; judge such titles on the host ([Host pass](#host-pass-real-hardware-run-by-a-human-80)).

| Case | Layer | What it proves |
|---|---|---|
| `depot-lane` | depot lane, L0 runtime | TAG (1207659013) from `--from-depot` starts in a clean guest from the seeded state dir and the store runtime; no write into the bundle; no Gatekeeper dialog left on screen |
| `gog-support` | L2 GOG support overlay | Carmageddon TDR 2000 (1146738698) has `ASSETS/options.txt` from the depot's `gog-support` overlay, so it reaches its first-run dialog instead of `Abort at File.cpp:73` |
| `signing` | launcher seal, Gatekeeper | the launcher verifies `--deep --strict` in the guest and a quarantined launch gets the "could not verify" prompt, not "damaged" (#53) |
| `teardown-term` | launcher teardown (#42) | `SIGTERM` to the launcher leaves no Wine process behind |
| `teardown-kill` | launcher teardown (#42, #103) | `SIGKILL` of the launcher: its watchdog tears down, nothing left after 15 s |
| `runtime-cx24` | L0 runtime (#62) | the default `winecx` engine and its `Frameworks/` start TAG in a clean guest: no missing dylibs, no microphone prompt |
| `renderer-l1-ddraw` | L1 renderer (#70) | Pharaoh Gold (1207659046) imports only `ddraw`: setup gives it cnc-ddraw (`renderer` = `cnc-ddraw/…`) and its dialog is on screen |
| `renderer-l1-wined3d` | L1 renderer (#70) | TAG's legacy Direct3D keeps wined3d on the base `winecx` runtime |
| `renderer-l1-dxmt` | L1 renderer (#70) | Neo Cab (1669531175, Unity, 64-bit) runs from the derived `winecx/<ver>+dxmt-<rver>` runtime and DXMT logs a D3D11 feature level. No window is expected: the guest has no Metal 3 ("Failed to get metal layer"); rendering is the host pass (#80) |
| `prefix-setup` | #67 | placeholder (`-` bundle): fill in the bundle and check when it lands |

Build the bundles first:

```sh
gogo mac-app --from-depot 1207659013 --out target/v2 --yes
gogo mac-app --from-depot 1146738698 --out target/v2 --yes
gogo mac-app --from-depot 1207659046 --out target/v2 --yes   # Pharaoh Gold
gogo mac-app --from-depot 1669531175 --out target/v2 --yes   # Neo Cab
scripts/vm-test.sh smoke
```

Renderer cases only prove start-up here: the VM GPU has **no Metal 3 and no OpenGL**, so
DXMT, D3DMetal and OpenGL rendering are judged on the host ([Host pass](#host-pass-real-hardware-run-by-a-human-80)), never by a VM pass.

## Results (`target/vm-test/<slug>-<timestamp>/`)

Exit code: `0` ok · `1` app failed (Gatekeeper-stopped / exited / no window) · `2` usage ·
`3` infrastructure (Tart/VM/SSH; says nothing about the app). The last stdout line is the
path of `summary.json`, and the summary is printed just before it:

```json
{
  "status": "ok | running_no_window | exited | did_not_launch | teardown_leftovers | gatekeeper_prompted | gatekeeper_blocked | infra_error",
  "exit_code": 0,
  "model": "v2", "gogid": "1207659013", "runtime": "wine-staging/11.18",
  "renderer": "wined3d", "runtime_dir": "runtimes/wine-staging/11.18/engine",
  "state": {"units": ["copy games/1207659013", "store runtimes/wine-staging"], "runtimes": "share"},
  "gatekeeper": "not_quarantined | allowed | prompted | blocked",
  "gatekeeper_dialog_left": false,
  "codesign_verify_rc": "0", "codesign_strict_rc": "0", "spctl_rc": "3",
  "bundle_modified": false,
  "missing_libs": ["…Wine cannot find the FreeType font library…"],
  "writes_outside_gogo": [],
  "log_grep": [], "teardown": null,
  "launched": true, "running_after_secs": true,
  "windows": [{"owner": "wine", "pid": 839, "layer": 26, "w": 1024, "h": 768}],
  "processes": ["839 S C:\\GOG\\Pharaoh Gold\\Pharaoh.exe", "..."],
  "screenshot": ".../screenshot.png", "gatekeeper_screenshot": ".../gatekeeper.png",
  "files": {"codesign": "...", "spctl": "...", "stdout": "...", "stderr": "...",
            "launcher_logs": "...", "run_log": ".../launcher-logs/20261023-142501-812.log",
            "prefix_logs": "...", "system_log": "...", "vm_log": "..."}
}
```

`ok` means "alive with a window". It does **not** mean "renders correctly", so always look
at `screenshot.png`. Give games enough `--secs`: under Rosetta in the guest, TAG takes
30–40 s to open its window (a 25 s run reported `running_no_window` with the game
alive), so the smoke cases use 45.

- `codesign_verify_rc` is the strict verify before launch; `codesign_strict_rc` is the
  same check after the run. `bundle_modified` compares a stat listing of `Contents/`
  before and after. Both catch a write into the signed bundle (#53).
- `renderer`, `runtime_dir`: what setup applied on the host, from the game's state
  `launch.conf` (`RENDERER`, `RUNTIME_DIR`). A DXMT game's `runtime_dir` is its derived
  `<ver>+dxmt-<rver>` runtime (#70).
- `missing_libs`: `cannot find` / `Library not loaded` / `image not found` lines from
  Wine's and the launcher's logs, i.e. what the runtime pulled from the build host.
- `writes_outside_gogo`: files created in the guest home during the run, outside
  `gogo/` state and logs, with macOS's own `~/Library` churn filtered out (design 16
  decision 4: saves stay in the prefix).
- `log_grep`: matches for `--grep`.
- `files.run_log`: this launch's run log (#77, design 16 §8.1): header with the
  effective runtime, renderer, verbs, stamp and macOS version, Wine's output, then
  `# exit:` and `# duration:` (no footer if the run was still going when collected).
  Empty when the launcher never reached Wine.

Other files: `gatekeeper.log`, `gatekeeper-windows.json` (everything on screen at the
prompt), `windows-all.json`, `processes.txt`, `processes-after-kill.txt`,
`codesign-after.txt`, `missing-libs.txt`, `writes-outside-gogo.txt`, `crash-reports.txt`,
`guest-info.txt`, `open.txt`, `launcher-logs/`.

## Known VM limitations

- **GPU is "Apple Paravirtual device"**: Metal GPU families Apple5 + Mac2 only, **no Metal 3**
  (no Apple7/8). So **D3DMetal / GPTK cannot work in the guest** (they need Metal 3), and
  DXVK/MoltenVK and wined3d-GL paths may behave differently from real hardware. Performance
  numbers mean nothing. There is **no usable OpenGL** either. Use the VM for "does it
  start, is it self-contained, what does Gatekeeper say, does it tear down", and real
  hardware for rendering and performance: DXMT, D3DMetal and OpenGL results always come
  from the host pass (#80).
- **SIP is disabled** in Cirrus images, and TCC is pre-granted to `sshd`/`osascript`. That's
  needed for automation, but it isn't a stock Mac: anything SIP- or TCC-sensitive must be
  re-checked on real hardware.
- **Guest has its own Homebrew** (`/opt/homebrew`, only `tart-guest-agent`). It has no Wine,
  FreeType, XQuartz or MoltenVK, so missing-library bugs surface here.
- **Rosetta**: present. Launching an x86_64 Wine bundle on macOS 27 posts *"App Update
  Required — … includes a component that will not open in macOS 28"* (the Rosetta
  retirement notice). It shows up in screenshots.
- The display is 1024×768 (2048×1536 Retina), and fullscreen games take all of it.
- Boot can fail with `VZErrorDomain Code=1` if the disk is busy. The script retries 3×, and
  checks running state from the process table, never `tart list` (which opens the
  ASIF disk images and raced a booting VM for its lock).
- No audio verification. The window list can't read Wine window titles (they aren't
  exposed to Accessibility).

## Host pass (real hardware, run by a human, #80)

The VM can't judge rendering: its GPU has no Metal 3 and no OpenGL (*Known VM
limitations*). DXMT, D3DMetal and OpenGL results therefore come from a short pass on a
real Apple Silicon Mac. A human runs it, because the game takes the screen. Agents
never run it against a real bundle (AGENTS.md). A PR that needs a pass lists the exact
steps for the user instead.

**When it's needed:** any change to DXMT or D3DMetal (catalog pins, the renderer store,
derived `<ver>+<renderer>-<rver>` runtimes), to renderer selection (L1 picks, compat
`renderer` entries, the "Try next renderer" order), or to the `winecx` runtime pin. Also
when a smoke case reports `vm-limit`. Run the cases the change touches. A change to
selection or the runtime pin runs all four.

### Cases

| Case | Game (gogid) | Build (with the setup below) | Pass when |
|---|---|---|---|
| `neo-cab-dxmt` | Neo Cab (1669531175), Unity, 64-bit D3D11 | `"$G" mac-app 1669531175 --runtime winecx --renderer dxmt --out "$GOGO_HOME/apps/neo-cab" --yes` | `renderer` is `dxmt/…` and `runtime_dir` ends in `+dxmt-<rver>/engine`; `markers.feature_level` shows `D3D_FEATURE_LEVEL_11_1`; `markers.graphics_dlls` shows `d3d11.dll` and `dxgi.dll`; `metal_layer_failed` and `wined3d` are empty; `markers.unity` shows `Direct3D 11.0 [level 11.1]` and `Renderer: Apple M…`; the game draws correctly, with input and audio working |
| `neo-cab-d3dmetal` | Neo Cab (1669531175) | `"$G" mac-app 1669531175 --runtime winecx --renderer d3dmetal --out "$GOGO_HOME/apps/neo-cab" --yes` (answer `y` to the D3DMetal prompt once) | `renderer` is `d3dmetal/…` on `+d3dmetal-<rver>`; `markers.unity` shows `Direct3D 11.0 [level 11.1]` and `Renderer: AMD Compatibility Mode` (what D3DMetal reports); `markers.d3dmetal` holds that line; `missing_libs` is empty and there's no `could not load` in `markers.unity`; the game draws, with input and audio working |
| `rebel-galaxy-dxmt` | Rebel Galaxy (1435582019), OGRE, 32-bit D3D11 | `"$G" mac-app 1435582019 --runtime winecx --renderer dxmt --out "$GOGO_HOME/apps/rebel-galaxy" --yes` | `verbs` holds the L1 verbs (`d3dcompiler_43 d3dx11_43 d3dx9_43 xinput`), and `markers.graphics_dlls` shows `d3dcompiler_43.dll: native` (the verb took); `markers.ogre` shows the D3D11 render system and no `OGRE EXCEPTION`; `markers.feature_level` is non-empty; the in-game image is right, not only the menu (docs/19 §8.2 Q7). A loading screen that never ends still reads `ok`: check `log_quiet_secs` and your own eyes. Record the `class_linkage` count, since DXMT's `Class linkage not supported` warnings are part of Q7 |
| `snake-core-gl` | Snake Core (1276268370), Java/libGDX, OpenGL | `"$G" mac-app 1276268370 --runtime winecx --out "$GOGO_HOME/apps/snake-core" --yes` (renderer auto) | `runtime` is `winecx/…` with no derived runtime. `renderer` reads `wined3d`, gogo's label for anything that isn't D3D10/11; an OpenGL game makes no D3D calls, so wined3d isn't used. `markers.graphics_dlls` shows `opengl32.dll: builtin` and no `d3d*`/`dxgi`. The game has no engine log and logs no `err:` lines on a good run. The window ("Snake Core - v1.6.1") is up and playable, with no `OpenGL 2.0 or higher with the FBO extension is required` (that's the wine-staging failure, docs/19 §5) |

The builds pin `--runtime winecx` because these cases are about that runtime; the auto
pick would choose it too. Each game builds into its own `apps/<game>/` dir, so the
`*.app` glob below matches one bundle. Neo Cab's two cases share that bundle: build,
pass, then rebuild with the other
`--renderer` (`--yes` overwrites) and pass again.

### Running it

```sh
# This branch's gogo, signed once so the keychain stops asking (AGENTS.md)
nix develop -c cargo build -p gogo-cli
scripts/dev-sign.sh
G="$PWD/target/debug/gogo"

# Scratch state + config: your real games and settings stay untouched. Keyring
# tokens still work; if your config has token_store = "file", sign in again.
export GOGO_HOME="$HOME/gogo-hostpass"

# Per case: build (table above), then run it for 90 s and play during that time
"$G" mac-app 1669531175 --runtime winecx --renderer dxmt --out "$GOGO_HOME/apps/neo-cab" --yes
scripts/host-pass.sh "$GOGO_HOME/apps/Neo Cab.app" --secs 90

# Clean up when done (run logs stay in ~/Library/Logs/gogo/<gogid>)
rm -rf "$GOGO_HOME"; unset GOGO_HOME
```

`scripts/host-pass.sh <X.app> [--secs N]` (`--help` lists all options):

1. Reads the bundle's `launch.conf` and the game's state `launch.conf`: gogid, state dir,
   renderer, runtime dir, verbs, prefix and `wineserver`. The state dir is `GOGO_HOME`,
   else what the bundle recorded, else `~/Library/Application Support/gogo`.
   `--dry-run` prints these and stops.
2. Starts the launcher (`Contents/MacOS/*`) directly from the shell, not with `open`,
   so `GOGO_HOME` reaches it and a stale config's `gogo mac-app setup` uses the scratch
   state. The launcher's #78 early-exit dialog is off (`GOGO_LAUNCHER_NO_DIALOG=1`,
   `--dialogs` turns it back on), so nothing waits for a click. It adds `+loaddll` to
   `WINEDEBUG`, after any value of your own, so Wine logs one line per DLL it loads; a
   compat entry's `env` can still override it. Because Terminal is the
   parent, a microphone prompt names Terminal, not the game.
3. Waits `--secs` (default 60) and follows the game's processes. Those are the
   launcher's tree, the prefix's `wineserver` and its tree, and anything with
   `WINEPREFIX=<prefix>` in its environment or the game's Windows folder in its command
   line. On `winecx` the game exe is started outside the launcher's tree
   (`start.exe /exec`), so the tree alone misses it. Other Wine games have another
   prefix and aren't counted. Quitting the game early is fine: the run ends there with
   `status: "exited"`.
4. Records the on-screen windows of those processes (CoreGraphics via JXA, as the VM
   harness does) and whether a permission or Gatekeeper prompt is up.
   `--screenshot` adds `screencapture -x`. Terminal needs Screen Recording for that, and
   macOS asks once.
5. Teardown: `SIGTERM` to the launcher (its own #42 teardown), up to `--kill-wait`
   (20 s). Anything of the game still alive, or the prefix's `wineserver` (found by its
   cwd, as the launcher does), counts as a leftover. Leftovers go to
   `WINEPREFIX=<prefix> wineserver -k`, then `SIGKILL`, so nothing is left running.
6. Collects this launch's run log (#77, `<ts>-<launcher pid>.log`, else the newest),
   `launch.log`, and the engine logs from `games/<gogid>/` written during the run: Unity
   `Player.log` (the prefix's `AppData/LocalLow`), OGRE `ogre.log`, and any other `*.log`.
   Older logs, such as Unity's `Player-prev.log` from a run on another renderer, are left
   out so the markers show only this run.

Results go to `target/host-pass/<slug>-<timestamp>/`. The summary is printed with home
paths shown as `~`, so it can be pasted into a PR as is. The last line is the path of
`summary.json`. Exit codes match the VM harness: `0` = alive with a window and a clean
teardown, `1` = the app failed (`exited`, `running_no_window`, `teardown_leftovers`),
`2` = usage, `3` = the harness itself failed.

```json
{
  "status": "ok", "exit_code": 0, "gogid": "1669531175",
  "runtime": "winecx/24.0.7_7", "renderer": "dxmt/v0.80",
  "runtime_dir": "runtimes/winecx/24.0.7_7+dxmt-v0.80/engine", "verbs": "",
  "host": {"macos": "27.0.1", "chip": "Apple M4 Pro"},
  "launched": true, "running_after_secs": true, "exited_after_secs": null, "run_exit": "143 (SIGTERM)",
  "log_quiet_secs": 0, "engine_log_quiet_secs": 4, "winedebug": "+loaddll",
  "windows": [{"owner": "…", "pid": 4242, "layer": 0, "w": 1920, "h": 1080}],
  "prompts_left": [],
  "teardown": {"signal": "TERM", "launcher_pid": "4200", "wait_secs": 20, "leftovers": [],
               "wineserver_k_fallback": false, "killed_after_fallback": [], "ok": true},
  "markers": {"feature_level": ["2 info:dxmt: Using feature level D3D_FEATURE_LEVEL_11_1"],
              "metal_layer_failed": [], "class_linkage": [], "wined3d": [],
              "opengl": [], "unity": ["1 Renderer: Apple M4 Pro (ID=0x…)", "…"], "ogre": [], "d3dmetal": [],
              "graphics_dlls": ["1 d3d11.dll: builtin", "1 dxgi.dll: builtin"]},
  "err_lines": ["3 0124:err:combase:RoGetActivationFactory …DispatcherQueue…"],
  "missing_libs": [], "engine_logs": ["prefix/drive_c/users/crossover/AppData/LocalLow/…/Player.log"],
  "files": {"run_log": "…/launcher-logs/20261023-171500-4200.log", "launch_log": "…", "engine_logs": "…"}
}
```

- `markers`: the top matching lines (with counts) for what a reviewer checks per
  renderer. `feature_level`, `metal_layer_failed`, `class_linkage`, `wined3d` and
  `opengl` come from the launcher's output and the run log. `unity` and `ogre` come from
  the engine logs. `d3dmetal` comes from both: D3DMetal logs nothing of its own on a good
  run, so its evidence is Unity's `Renderer: AMD Compatibility Mode`, or the D3DMetal
  libs named in a failure. The run log's header and footer (`# renderer: …`) never count,
  because they say what was asked for, not what ran.
- `graphics_dlls`: the graphics DLLs Wine loaded, as `<count> <dll>: builtin|native`,
  from the `+loaddll` trace. `opengl32` only means the game draws with OpenGL itself.
  `d3d11`/`dxgi` means D3D11 (DXMT and D3DMetal replace these builtins). `native`
  `d3dcompiler_*`/`d3dx*` comes from winetricks verbs. Counts are per process.
- Wine-side markers read the run log, which holds the launcher's output; the
  launcher's own stdout/stderr files are read only when there's no run log.
- `log_quiet_secs`, `engine_log_quiet_secs`: how long Wine's output and the engine logs
  had been silent when the wait ended. A game stuck on a loading screen can still be
  alive with a window (`ok`), but its logs stop. A quiet time close to `--secs` is a
  stall to look at before calling the case a pass.
- `err_lines`: Wine's `err:` lines, the most frequent first. The Unity
  `RoGetActivationFactory … DispatcherQueue` line is a harmless WinRT probe.
- `run_exit`: the run log's footer. `143 (SIGTERM)` means the pass stopped the game.
- `ok` means only "alive with a window". Whether it draws correctly is your call, and
  goes in the record below.

The script is tested headlessly with fake bundles (a stub launcher and a stub
`wineserver`; no Wine, no window): `scripts/tests/host-pass-fake.sh`, which runs in CI.

### What to record

One row per case, in the PR (or on the issue the change belongs to), plus the summary
JSON in a collapsed `<details>`. A pass that fails is still recorded.

```markdown
| Case | Date | Host | gogo | Renderer / runtime_dir | status | Markers | Eyes on (render / input / audio) | Notes |
|---|---|---|---|---|---|---|---|---|
| neo-cab-dxmt | 2026-10-23 | M4 Pro, macOS 27.0.1 | <commit> | dxmt/v0.80, +dxmt-v0.80 | ok | FL 11_1; Renderer: Apple M4 Pro | ✓ / ✓ / ✓ | |
```

Each passing case becomes a compat `tested` entry for the game (#75):
`{ runtime = "winecx-24.0.7_7", host = "m4pro/macos-27.0.1", date = 2026-10-23 }`. A
worked example is the Neo Cab DXMT pass in #70's comments. It was started by a
double-click before this script existed, and recorded the same evidence: the run log's
runtime path, DXMT's feature level with no `Failed to get metal layer`, and Unity's
`Renderer`.

## What the first real run found (Pharaoh Gold, `--from-depot`, wine-staging 11.18, cnc-ddraw)

1. **Every gogo bundle's signature is invalid at birth.** `codesign` reports files
   added after signing: `prefix/{system,user,userdef}.reg`. wineserver flushes the
   registry lazily *after* the build's signing step. With Gatekeeper on, a quarantined
   bundle is therefore rejected as **"damaged and can't be opened. You should move it to
   the Trash."** That dialog has no "Open Anyway" button.
2. **Bundles aren't self-contained: FreeType.** In the guest, Wine logs *"Wine cannot
   find the FreeType font library"*, so the runtime picks it up from the host's Homebrew
   at build/run time. The game then shows a black fullscreen window plus an 11×58 px
   dialog, probably a message box whose text measured zero width.
3. **The prefix leaks the builder's identity.** `drive_c/users/<builder>/{Desktop,…}` are
   absolute symlinks into the builder's home. That breaks `codesign --strict` (ENOENT on
   other Macs, TCC "Operation not permitted" on the builder's own Mac) and embeds the
   username.
