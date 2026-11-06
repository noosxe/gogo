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
| `--gpu NODE` | a real GPU, with no display output (below). Default: software |
| `--secs N` | the limit of the game phase, default 600. Over it, the run stops and fails |
| `--gogo PATH` | this gogo; `.#umu-python` supplies the FHS env. Default: `nix build .#gogo-linux-app` |
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
| `summary.json` | checks, steps, screenshots, doctor, log excerpt, cleanup, `tested_entry` |
| `shots/<name>.png` | the manifest's screenshots, and `not-exited.png` if a game ignored its exit |
| `build.log`, `sway.log`, `game.out`, `sandbox.log` | what each printed |
| `run.log`, `launch.log` | the launcher's logs (design 18 §7.3) |
| `doctor-before.json`, `doctor-after.json` | `gogo linux-app doctor` around the run |

## What a run does

1. **Scratch.** `target/linux-e2e/runs/<id>/` holds `GOGO_HOME`, `HOME` and the XDG dirs.
   Proton, umu's runtime and the depot download are bind-mounted from
   `target/linux-e2e/cache/`, so a re-run downloads nothing and the build is still fresh.
   Runs take turns, through a lock.
2. **Build,** in a sandbox with the network and the session bus: `gogo linux-app <gogid> --yes`.
3. **Game,** in a sandbox with neither (below): sway, Xwayland, the isolation checks, then
   `launch.sh`, the steps, the exit, and doctor.
4. **Teardown,** on every exit path: kill the sandbox, sweep, assert nothing is left, delete
   scratch, write the summary.

### Isolation

Both sandboxes are a whitelist bwrap root: `/nix`, `/etc`, `/sys`, the driver links in
`/run`, scratch and results. The host's `/home`, `/mnt`, `/media`, `/run/user` and
`/tmp` aren't in it. On top of that, the game's has its own network namespace with only
`lo` (X11's abstract socket is per netns, so a shared one could reach the host's `:0`), its
own `/tmp` and `XDG_RUNTIME_DIR`, and only the chosen GPU's device nodes. `DISPLAY`,
`WAYLAND_DISPLAY` and `HYPRLAND_*` never reach it.

The script doesn't trust that: before launching, the driver asserts it, and a failure means
the game isn't started.

| Check | Asserts |
|---|---|
| `isolation_netns` | a network namespace of its own, with only `lo` |
| `isolation_pidns` | a PID namespace of its own |
| `isolation_tmp` | `/tmp` is a tmpfs with only the X server's files |
| `isolation_runtime_dir` | `XDG_RUNTIME_DIR` is on the sandbox's own `/run` |
| `isolation_env` | no `HYPRLAND_*` |
| `isolation_paths` | the owner's game dirs, menu entries, Steam, Downloads and session bus aren't visible |
| `isolation_display` | `xwininfo -root` answers with the manifest's size, on this `/tmp`'s socket |
| `isolation_headless` | sway's outputs are all `HEADLESS-*` |
| `isolation_gpu_nodes` | exactly the chosen GPU's device nodes, and none in software mode |

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

## Graphics

**Software (default).** wlroots on pixman, GL on llvmpipe, Vulkan on lavapipe (the host's
`lvp_icd` manifests). There's no GPU node in the sandbox, so nothing can quietly use one.

**`--gpu /dev/dri/renderD12x`.** wlroots renders there (`WLR_RENDERER=gles2`,
`WLR_RENDER_DRM_DEVICE`), with no display output. That node is the sandbox's only GPU
(plus `/dev/nvidia*` for NVIDIA), Vulkan only gets that driver's manifests, and GL gets its
vendor. The label in the summary comes from the node's kernel driver: `nvidia`, `radv` or
`intel`.

| Check | Asserts |
|---|---|
| `gpu_gl` | `glxinfo` in the game's environment names the expected driver |
| `gpu_vulkan` | `vulkaninfo` only lists the expected device |
| `gpu_used_by_game` | `--gpu`: a process of the game holds the node open (`/dev/nvidia0` for NVIDIA). `gpu_detail.gpu_fds` lists who |

On a real game the game's process holds `/dev/dri/renderD128` or `/dev/nvidia0` open.
`gpu_used_by_game` also passes when only `explorer.exe` or `xalia.exe` do: read
`gpu_detail.gpu_fds` for who. **DXVK's device:** with `--gpu` the sandbox has one GPU, and
`DXVK_LOG_LEVEL = "info"` under `[env]` prints it to `game.out` (`Found device: …`). In these
runs it was always the chosen one. Which of two GPUs DXVK picks by itself stays a host question
(R18). A Unity game's real API is in its `Player.log` (`Version: Direct3D 12`), under `--keep`.

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
wait = 5                         # seconds
[[step]]
screenshot = "preferences"
min_fill = false                 # this frame needn't fill the window
[[step]]
expect_change = { from = "menu", to = "preferences", min = 20 }  # % of pixels
[[step]]
key = "Escape"                   # wtype -k: one named key
[[step]]
type = "hello"                   # wtype: text
```

Prefer `wait_settled` to a long `wait` before the first click: the same intro takes a
different time on another renderer, and a click meant for the menu lands on the video. Its
picture must fill the window and hold still (under 1% of pixels changing across 2 s), so an
animated menu needs a plain `wait`.

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
- **Audio.** There's no sound server in the sandbox. ALSA's `Unknown PCM default` in the log
  is expected.
- **The menu entry and its icon** on a real desktop. The harness runs `launch.sh` directly,
  and writes its `.desktop` file into scratch.
- **The early-exit dialog.** It's off here, and GNOME and KDE aren't anything the owner runs.

## How it was found to work, and what to watch for

- **An agent's shell is itself a bwrap** (the desktop app's FHS env), with `/etc/passwd` a
  link into `/.host-etc`. Xwayland admits clients by local user, so an unresolvable uid
  gives `Authorization required`: the sandbox makes its own `passwd` and `group`.
- **A headless seat has no pointer**, and then `seat cursor press` reaches nobody. The driver
  holds a wlr virtual pointer for the whole run, over a small Wayland client in the helper.
- **Each `wtype` is a new keyboard,** and a key sent at once is lost while Xwayland loads
  its keymap, so `key` and `type` start with a 500 ms pause.
- **NVIDIA needs its EGL vendor named** (`__EGL_VENDOR_LIBRARY_FILENAMES`), or wlroots gets
  Mesa's EGL on NVIDIA's GBM: `gbm device using incorrect/incompatible backend`.
- **`XDG_RUNTIME_DIR` isn't under `target/`:** a socket path has a limit of 108 bytes.
- **`runtimes` and `renderers` share one mount** (`cache/store`, linked from `GOGO_HOME`): the
  store stages renderers in `runtimes/.tmp`, and a rename across two bind mounts fails. gogo
  then falls back from cnc-ddraw to D7VK with only "provisioning failed" to say so.
- **A support installer can hang with no display** (Zeus, Pharaoh). gogo stops it after 180 s,
  so those builds take 3 minutes longer here.
- **A click is a percent of the largest window,** which on a game's small first dialog is that
  dialog, not the screen.
- **`gogo depot --dir` puts `gog-support/` beside the game:** it belongs there. TDR 2000 aborts
  without its `ASSETS/options.txt`.
- **Sway comes as `sway-unwrapped`:** nixpkgs' wrapper starts `dbus-run-session`, which needs
  a config in `/etc`.
