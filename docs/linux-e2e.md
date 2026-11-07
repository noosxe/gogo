# Launch-testing Linux games in headless sway

`scripts/linux-e2e.sh` is the one way agents launch a `gogo linux-app` game, and the owner's
quick way to check one. One command builds the game in a scratch home, runs it through its
generated `launch.sh` in a headless compositor inside a sandbox, drives a per-game manifest,
checks the frames, tears everything down and prints a JSON summary. Every run is set up the
same way: the tools are pinned in the flake (`.#linux-e2e-tools`, the run's whole `PATH`).

What it can't show stays the owner's: [What it doesn't cover](#what-it-doesnt-cover).

## Policy for agents

- Launch a real game **only through this script** (`AGENTS.md`).
- **Each new game download needs the owner's OK.** A game is new when
  `target/linux-e2e/cache/depot/<gogid>` isn't there.
- Screenshots stay in `target/`, and never enter git: they show copyrighted game art.

## Use

```bash
scripts/linux-e2e.sh run 2086050016
```
```bash
scripts/linux-e2e.sh run 2086050016 --gpu /dev/dri/renderD129
```
```bash
scripts/linux-e2e.sh run 2086050016 --gpu renderD128,renderD129 --expect-gpu renderD129
```
```bash
scripts/linux-e2e.sh run 1423049311 --installer ~/backups/cyberpunk/setup_cyberpunk_2077.exe --gpu renderD129
```
```bash
scripts/linux-e2e.sh self-test
```
```bash
scripts/linux-e2e.sh cleanup
```

It needs `nix` and nothing else, and resolves its own tools, so there's no need for
`nix develop`. To find a render node's GPU: `ls -l /sys/class/drm/renderD*/device/driver`.

| `run` option | |
|---|---|
| `--manifest FILE` | default `e2e/<gogid>.toml` |
| `--gpu NODE[,NODE]` | real GPUs, with no display output (below), comma-joined or repeated: sway renders on the first. `renderD12x` or the full path. Default: software |
| `--expect-gpu NODE` | with several GPUs, the one the game must render on. Default: any of them, and the summary says which |
| `--secs N` | the limit of the game phase, default 600. Over it, the run stops and fails |
| `--gogo PATH` | this gogo; `.#umu-python` supplies the FHS env. Default: `nix build .#gogo-linux-app` |
| `--installer FILE` | build from this GOG setup `.exe` ([below](#building-from-an-offline-installer)) instead of the depot |
| `--keep` | keep the scratch dir. `cleanup` leaves it; delete it by hand |
| `--no-cache` | download Proton, umu's runtime and the game afresh, into scratch |
| `--out DIR` | results root, default `target/linux-e2e/results` |

**Login.** The build uses your usual keyring login, through the session bus, which only the
build's sandbox sees. It needs `gogo login` done once. Tokens are never printed.

**Exit codes:** `0` every check passed, `1` a check failed, `2` usage, `3` harness problem
(says nothing about the game).

**Output:** progress on stderr, the summary JSON on stdout, then on the last line the path of
`summary.json`. Its `failed` lists the failed checks by name. Everything is in
`target/linux-e2e/results/<gogid>-<time>-<pid>/`:

| File | |
|---|---|
| `summary.json` | checks, steps, screenshots, audio, `build`, doctor, log excerpt, cleanup, `tested_entry` |
| `shots/<name>.png` | the manifest's screenshots, `not-exited.png` if a game ignored its exit, `build-window-<n>.png` for each window the build opened |
| `build.log`, `build-sway.log`, `build-sandbox.log` | what the build phase printed |
| `sway.log`, `game.out`, `sandbox.log`, `audio.log` | what the game phase printed |
| `support-inno.log` | the support installer's Inno log, when it wrote one |
| `install-inno.log`, `install-wine.log` | what the installer wrote, with `--installer` |
| `run.log`, `launch.log` | the launcher's logs (design 18 §7.3) |
| `doctor-before.json`, `doctor-after.json` | `gogo linux-app doctor` around the run |

## What a run does

1. **Scratch.** `target/linux-e2e/runs/<id>/` holds `GOGO_HOME`, `HOME` and the XDG dirs.
   Proton, umu's runtime and the depot download are bind-mounted from
   `target/linux-e2e/cache/`, so a re-run downloads nothing and the build is still fresh.
   Runs take turns, through a lock.
2. **Build,** in a sandbox with a NATed network of its own (pasta) and the session bus, on a headless sway of its
   own (software rendering, whatever `--gpu` says): the isolation checks, then
   `gogo linux-app <gogid> --yes`. `build` in the summary has its exit, how long it took, the
   windows it opened and how the support installer ended.
3. **Game,** in a sandbox with neither network nor bus (below): sway, Xwayland, a null sound
   device, the isolation checks, then `launch.sh`, the steps, the exit, and doctor.
4. **Teardown,** on every exit path: kill the sandbox, sweep, assert nothing is left, delete
   scratch, write the summary.

### Isolation

Both sandboxes are a whitelist bwrap root: `/nix`, `/etc`, `/sys`, the driver links in
`/run`, scratch and results. The host's `/home`, `/mnt`, `/media`, `/run/user` and
`/tmp` aren't in it. On top of that, the game's has its own network namespace with only
`lo` (X11's abstract socket is per netns, so a shared one could reach the host's `:0`), its
own `/tmp` and `XDG_RUNTIME_DIR`, and only the chosen GPUs' device nodes. `DISPLAY`,
`WAYLAND_DISPLAY` and `HYPRLAND_*` never reach it, and the host's sound sockets aren't in it.

The build's sandbox has a network namespace of its own too, and still reaches the internet.
A holder process (`unshare --user --map-root-user --net`) owns a user and a network namespace;
`pasta` (passt) joins them from outside and NATs them out in user space, and bwrap then nests
its sandbox in them with `--uid`/`--gid` set back to yours (Proton refuses to run as root). Inside
there's `lo` and pasta's tap, named after the host's uplink. DNS goes to a mapped address that
pasta forwards to the host's resolver, so there are no nscd or resolved sockets bound. So the
build's X server's abstract socket isn't in the host's namespace either, and its Xwayland gets
`:0` like the game's. Killing the holder takes the sandbox with it.

The script doesn't trust that: before launching, the driver asserts it, and a failure means
the game isn't started.

| Check | Asserts |
|---|---|
| `isolation_netns` | a network namespace of its own, with only `lo` (the build: `lo` and pasta's tap) |
| `isolation_pidns` | a PID namespace of its own |
| `isolation_tmp` | `/tmp` is a tmpfs with only the X server's files |
| `isolation_runtime_dir` | `XDG_RUNTIME_DIR` is on the sandbox's own `/run` |
| `isolation_env` | no `HYPRLAND_*` |
| `isolation_paths` | the owner's game dirs, menu entries, Steam, Downloads, session bus and sound sockets aren't visible |
| `isolation_display` | `xwininfo -root` answers with the manifest's size, on this `/tmp`'s socket, and this sandbox's Xwayland serves that number, `:0` |
| `isolation_headless` | sway's outputs are all `HEADLESS-*` |
| `isolation_gpu_nodes` | exactly the chosen GPUs' device nodes, and none in software mode or in the build |
| `audio` | the default device is the sandbox's null sink, the only sink, on its own socket, and it takes a test stream. Game phase only |

The build's run of these checks appears in the summary as `build_<check>`.

### Teardown

Everything in the sandbox shares one PID namespace under a bwrap init that dies with the
script, so the kernel kills whatever is left, nested pressure-vessel containers included.

| How the run ends | What happens |
|---|---|
| The steps finish | the game exits by itself, or the launcher gets `TERM`, per `[exit]` |
| `--secs` runs out, `TERM`, `INT`, `HUP` | the driver stops the game with `TERM` to the launcher (its own sweep), and reports. After 45 s, or a second signal, the sandbox is killed |
| The script gets `KILL` | no teardown can run, but the sandbox dies with it. The next run or `cleanup` removes the scratch |

Afterwards the script looks for what's left from outside the sandbox: the launcher's prefix
markers and the run's own marker in `/proc/<pid>/environ`, the scratch path on a command
line, and the sandbox's PID namespace. `nothing_left` fails if it finds anything, and it
kills what it finds. `scratch_deleted` fails if the dir stays.

## Building from an offline installer

`--installer <setup.exe>` builds through `gogo linux-app <setup.exe> --yes`, gogo's installer
lane (design 18 §10), instead of the depot. Inno Setup runs silently under umu in a staged
prefix (`GAMEID=umu-0`, `/DIR=C:\GOG\<slug>`), and the game's id, entry point and renderer
come from the tree it installed, as they do for a depot build. It uses no game download, so
there's no `cache/depot/<gogid>` and no owner's OK to ask for, but it can only build the game
the manifest names: another id fails the build.

- **The folder is read-only, in the build only.** The `.exe`'s folder is bind-mounted at
  `/e2e-installer`, `.bin` parts and all. `build_isolation_installer` asserts the mount is
  read-only by the kernel's word (`statvfs`) and the `.exe` can be read, and a failure means
  the build isn't started. The game's sandbox doesn't have the mount, and its
  `isolation_paths` asserts that, and both phases assert the host path isn't visible.
- **A fixed path** keeps the host's layout out of the sandbox. An installer can stay in
  `~/Downloads` while `isolation_paths` still hides that.
- **Other files in the folder** (`dlc/`, `patches/`, `extras/`) are visible and unused: the
  installer only opens its own `.bin` parts. Nothing else installs them.
- **Disk:** the installed game is in scratch, deleted with it, so every run installs again.
  There's no cache of installed games. Scratch needs the game's size free (Cyberpunk 2077: about 70 GB).
- `build.installer` in the summary names the file. A self-test's `build_command` finds it in
  `GOGO_E2E_INSTALLER`.

## Graphics

**Software (default).** wlroots on pixman, GL on llvmpipe, Vulkan on lavapipe (the host's
`lvp_icd` manifests). There's no GPU node in the sandbox, so nothing can quietly use one.

**`--gpu renderD12x`.** wlroots renders there (`WLR_RENDERER=gles2`,
`WLR_RENDER_DRM_DEVICE`), with no display output. That node is the sandbox's only GPU
(plus `/dev/nvidia*` for NVIDIA), Vulkan only gets that driver's manifests, and GL gets its
vendor. The label in the summary comes from the node's kernel driver: `nvidia`, `radv` or
`intel`.

| Check | Asserts |
|---|---|
| `gpu_gl` | `glxinfo` in the game's environment names the expected driver |
| `gpu_vulkan` | `vulkaninfo` only lists the expected device |
| `gpu_used_by_game` | One GPU: the game's own process holds the node open (`/dev/nvidia0` for NVIDIA). `gpu_detail.game_gpu_fds` lists those, `gpu_detail.gpu_fds` everybody |

**What counts as the game:** a process whose Windows path (its `argv[0]`) is the effective exe,
the launcher log's `# exe:` line, or lies in that exe's folder. `explorer.exe` and `xalia.exe`
hold the node whether or not anything renders there, so they don't count. An exe that runs
from outside the game's folder, such as a launcher unpacking into `Temp`, doesn't count either:
the check fails, and `gpu_detail.gpu_fds` shows who held it. The fds are read at each
screenshot, so the manifest needs one while the game renders.

**DXVK's device:** `DXVK_LOG_LEVEL = "info"` under `[env]` prints it to `game.out` (`Creating
device:`). A Unity game's real API is in its `Player.log` (`Version: Direct3D 12`), under `--keep`.

**`--gpu renderD128,renderD129`.** The sandbox has both, and wlroots renders on the first, so
the order is the compositor's choice: `renderD128,renderD129` is a hybrid laptop's shape,
`renderD129,renderD128` a desktop on the dGPU. The label joins them, compositor first
(`radv+nvidia`), and so does `tested_entry`'s host.

The game gets what a desktop gives it: no `VK_DRIVER_FILES`, no GL vendor, no
`__NV_PRIME_RENDER_OFFLOAD`, and an `XDG_DATA_DIRS` where the loader finds every ICD and
implicit layer, Mesa's device-select included. Nothing steers it, so what it picks is its own
choice. Only the compositor's own variables remain, as on a desktop: on NVIDIA, sway's
`GBM_BACKEND` and `__GLX_VENDOR_LIBRARY_NAME` reach the game too.

| Check | With several GPUs |
|---|---|
| `gpu_gl` | names one of them |
| `gpu_vulkan` | lists each, and nothing else but the host's software driver |
| `gpu_used_by_game` | it's shown to render on `--expect-gpu`'s. Without that, it holds at least one, and `game_renders_on` says what it uses |

**Holding a node doesn't mean rendering on it.** A Vulkan driver opens its node when it's
enumerated and keeps it, so a game holds both GPUs and only uses one, and Wine's helpers
hold both too. `gpu_detail` keeps the two apart:

| Key | |
|---|---|
| `compositor_node` | the first `--gpu` |
| `game_nodes` | the render nodes the game holds |
| `game_renders_on` | the render nodes it uses: the GPU DXVK made its device on, or with no DXVK, a GPU whose kernel driver counts engine time for the game's fds. Empty when neither shows anything |
| `dxvk` | `found`, in the order Vulkan gave them, before DXVK's own sort, and `created`, from `game.out`. Needs `DXVK_LOG_LEVEL = "info"` |
| `game_drm_use` | per node, `engine_ns` and `vram_kib` from `/proc/<pid>/fdinfo`. amdgpu and Intel's drivers count these; NVIDIA's doesn't |
| `vulkan_devices` | `vulkaninfo`'s list, in the loader's order after the layers |

So a game on NVIDIA that isn't on DXVK shows nothing from inside the sandbox, and
`--expect-gpu` can't pass on it. `nvidia-smi pmon -s um` from outside the sandbox lists it by
process; inside, it lists nothing, because of the PID namespace.

**Which GPU a game picks** (RTX 4090 and a Ryzen iGPU, GE-Proton11-12, DXVK 3.1.1):

- DXVK games (D3D9, D3D11) take the discrete GPU, wherever the compositor is. DXVK sorts
  discrete before integrated, after Vulkan's list, so Mesa's device-select and the ICD order
  change what it finds first but not what it picks.
- `DXVK_FILTER_DEVICE_NAME = "AMD"` moves it (a case-sensitive part of the name), and so does
  `MESA_VK_DEVICE_SELECT=<id>` with `MESA_VK_DEVICE_SELECT_FORCE_DEFAULT_DEVICE=1`.
- A game on the iGPU with the compositor on NVIDIA shows nothing: DXVK logs `Failed to create
  Vulkan swapchain: VK_ERROR_INITIALIZATION_FAILED` for every frame, and the window stays white
  or black. The other way round, on the dGPU with the compositor on the iGPU, works.
- GL games (wined3d) follow the compositor: GL's vendor comes from the X server.

- VKD3D-Proton (Cyberpunk 2077, one GPU, the 4090) logs its device with `VKD3D_DEBUG = "info"`
  and nothing that `gpu_detail.game_renders_on` reads, so that stays empty on NVIDIA.
  `gpu_used_by_game` passes on the held node, and `nvidia-smi pmon` from outside shows the load.
  With several GPUs it wasn't tried.

What a real desktop does stays on the host pass.

## Manifests

`e2e/<gogid>.toml`. The parser is `scripts/linux-e2e-helper.py`, which refuses unknown keys
and a step without exactly one action.

```toml
title = "SimCity 3000 Unlimited"
gogid = 2086050016

[window]
wait_secs = 180                  # for the first window; default 180
resolution = "1280x800"          # the headless output; default 1280x800
title = "^SimCity"               # regex for the window; default the largest visible
min_fill = { w = 80, h = 95 }    # % of the window; default 90 x 90

[env]                            # only for the game's launch: logging, for example
DXVK_LOG_LEVEL = "info"

[exit]
how = "self"                     # "self": it exits by itself after the steps, code 0
                                 # "signal" (default): the launcher gets TERM, code 143
wait_secs = 60                   # default 30
code = 255                       # "self": the code it exits with; default 0

[[step]]
wait_settled = 240               # until the picture fills the window and holds still
[[step]]
screenshot = "menu"              # checks not_black_menu and fills_window_menu
[[step]]
click = { x = 43, y = 71 }       # percent of the window
[[step]]
move_by = { x = -3000, y = -3000 } # a relative move, in device units: for a game's own cursor
[[step]]
click_here = true                # press and release, without moving
[[step]]
wait = 5                         # seconds
[[step]]
screenshot = "preferences"
min_fill = false                 # this frame needn't fill the window
[[step]]
expect_change = { from = "menu", to = "preferences", min = 20 }  # % of pixels
[[step]]
key = "Escape"                   # wtype -P, 50 ms, -p: one named key, held
[[step]]
type = "hello"                   # wtype: text
```

Prefer `wait_settled` to a long `wait` before the first click: the same intro takes a
different time on another renderer, and a click meant for the menu lands on the video. Its
picture must fill the window and hold still (under 1% of pixels changing across 2 s), so an
animated menu needs a plain `wait`.

A game that draws its own cursor from relative DirectInput (Carmageddon TDR 2000's menu) can't
be aimed with `click`: while Wine holds the exclusive grab, the compositor's pointer stays put
and only the deltas count. Steer it instead: a big `move_by` pins the cursor in a corner, where
the game clamps it, a second one goes to the target, and `click_here` presses there. A unit of
`move_by` isn't a pixel, since the game and Wine scale it, so measure it from a screenshot.

To write one: run it with `--keep` or a first screenshot, read the coordinates off the
PNG, and divide by the resolution. Look at the screenshots yourself: the automatic checks
say "a picture fills the window and it changed", not "it's the right picture".

### The automatic checks

| Check | Fails when | Would have caught |
|---|---|---|
| `window` | no window in `wait_secs`, or the game exited first | DDrawCompat's exit 9 in #199 |
| `settled` | `wait_settled` timed out | a game stuck in a video |
| `not_black_<shot>` | under 1% of the window's pixels are above level 16 | a black screen |
| `fills_window_<shot>` | the non-black bounding box is under `min_fill` | 800×600 in the top-left of a window: 62% × 75% |
| `changed_<a>_<b>` | under `min`% of pixels changed by more than 16 | a click that didn't land |
| `clean_exit` | `how = "self"`: still up after `wait_secs`, or a non-zero exit | a game that hangs on exit |
| `torn_down` | `how = "signal"`: the launcher doesn't end on `TERM` | a stuck teardown |
| `doctor_links`, `doctor_processes` | doctor's lines aren't `ok` after the run | host drive links (#199), a stray Wine process |
| `finished` | the driver hit an error, or never got to the end | |
| `nothing_left`, `scratch_deleted` | as above | |

## From a pass to a compat entry

A pass prints `tested_entry`. Add it to the game's L3 file in `compat/<gogid>.toml`:

```toml
[platforms.linux]
status = "boots"
notes = "headless sway: menu, Preferences and Exit work; audio and the owner's compositor not judged"
tested = [
  { runtime = "ge-proton/GE-Proton11-12", host = "headless-sway/software", date = 2026-11-06 },
  { runtime = "ge-proton/GE-Proton11-12", host = "headless-sway/nvidia", date = 2026-11-06 },
]
```

`headless-sway/<gpu>` says what it is: not the owner's desktop. A status above `boots`
needs someone to have played it.

## What it doesn't cover

These stay in the owner's host pass (#179, design 18 §11.2), and agents put the steps in the PR:

- **Compositor quirks**, such as the picture in the top-left of a tiled window on Hyprland
  (#199): that never reproduced on sway, so a pass here doesn't rule it out.
- **Real display modes and VRR.** A headless output has one mode and no refresh to sync to.
- **Audio you can hear.** The sandbox's device is a null sink: it takes the stream and drops
  it. `audio.game_streams` in the summary names the streams the game opened, which says it found
  a device, not that it sounds right.
- **The menu entry and its icon** on a real desktop. The harness runs `launch.sh` directly,
  and writes its `.desktop` file into scratch.
- **The early-exit dialog.** It's off here, and GNOME and KDE aren't anything the owner runs.

## How it was found to work, and what to watch for

- **An agent's shell is itself a bwrap** (the desktop app's FHS env), with `/etc/passwd` a
  link into `/.host-etc`. Xwayland admits clients by local user, so an unresolvable uid
  gives `Authorization required`: the sandbox makes its own `passwd` and `group`.
- **A headless seat has no pointer**, and then `seat cursor press` reaches nobody. The driver
  holds a wlr virtual pointer for the whole run, over a small Wayland client in the helper.
- **Motion goes through that pointer, not `seat cursor set`.** That command is a warp with no
  delta, so Xwayland reports it as absolute raw motion, and Wine passes the 0-65535 value on,
  scaled, as a delta (+21611 in TDR 2000's DirectInput, which threw its cursor into a corner).
  The clicks were landing all along, just not where they aimed. A real mouse never does this.
- **Each `wtype` is a new keyboard,** and a key sent at once is lost while Xwayland loads
  its keymap, so `key` and `type` start with a 500 ms pause. A `key` is held 50 ms (`wtype -P`, `-s 50`, `-p`), not sent as an instant `-k`: a game that polls its input misses that.
- **NVIDIA needs its EGL vendor named** (`__EGL_VENDOR_LIBRARY_FILENAMES`), or wlroots gets
  Mesa's EGL on NVIDIA's GBM: `gbm device using incorrect/incompatible backend`.
- **`XDG_RUNTIME_DIR` isn't under `target/`:** a socket path has a limit of 108 bytes.
- **`runtimes` and `renderers` share one mount** (`cache/store`, linked from `GOGO_HOME`): the
  store stages renderers in `runtimes/.tmp`, and a rename across two bind mounts fails. gogo
  then falls back from cnc-ddraw to D7VK with only "provisioning failed" to say so.
- **A support installer hangs with no display** (Zeus, Pharaoh), which is why the build has one
  (#206). On a display it ends at once, exit 1 (`InitializeSetup returned False`). gogo's 180 s
  limit stays for users building with no display.
- **Zeus exits 5 with no audio device** (#204), so `[env] PULSE_SERVER = "unix:/nonexistent/native"`
  in a scratch manifest is how to ask what a game does without one.
- **A click is a percent of the largest window,** which on a game's small first dialog is that
  dialog, not the screen.
- **`gogo depot --dir` puts `gog-support/` beside the game:** it belongs there. TDR 2000 aborts
  without its `ASSETS/options.txt`.
- **The sound server is PulseAudio, not PipeWire:** one process with two modules, and no session
  manager. Proton's `winepulse` speaks to it through `PULSE_SERVER`, which pressure-vessel carries
  into its container. It's set explicitly because `/etc` is the host's, `/etc/pulse` included.
- **Sway comes as `sway-unwrapped`:** nixpkgs' wrapper starts `dbus-run-session`, which needs
  a config in `/etc`.
