# Launch-testing Linux games in headless sway

`scripts/linux-e2e.sh` is the one way agents launch a `gogo linux-app` game, and the
quick way to check one. One command builds the game in a scratch home, runs it through its
generated `launch.sh` in a headless compositor inside a sandbox, drives a per-game manifest,
checks the frames, tears everything down and prints a JSON summary. Every run is set up the
same way: the tools are pinned in the flake (`.#linux-e2e-tools`, the run's whole `PATH`).

What it can't show stays a host pass: [What it doesn't cover](#what-it-doesnt-cover).

## Policy for agents

- Launch a real game **only through this script** (`AGENTS.md`).
- **Each new game download needs @noosxe's OK.** A game is new when
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
scripts/linux-e2e.sh run 1423049311 --installer ~/backups/cyberpunk/setup_cyberpunk_2077.exe --reuse-install
```
```bash
scripts/linux-e2e.sh run 1423049311 --installer ~/backups/cyberpunk/setup_cyberpunk_2077.exe --reuse-install --display wayland --gpu renderD129
```
```bash
scripts/linux-e2e.sh live start 1423049311 --installer ~/backups/cyberpunk/setup_cyberpunk_2077.exe --reuse-install
```
```bash
scripts/linux-e2e.sh self-test
```
```bash
scripts/linux-e2e.sh cleanup
```
```bash
scripts/linux-e2e.sh cleanup --prune-installed all
```

It needs `nix` and nothing else, and resolves its own tools, so there's no need for
`nix develop`. To find a render node's GPU: `ls -l /sys/class/drm/renderD*/device/driver`.

| `run` option | |
|---|---|
| `--manifest FILE` | default `e2e/<gogid>.toml` |
| `--gpu NODE[,NODE]` | real GPUs, with no display output (below), comma-joined or repeated: sway renders on the first. `renderD12x` or the full path. Default: software |
| `--expect-gpu NODE` | with several GPUs, the one the game must render on. Default: any of them, and the summary says which |
| `--display MODE` | `xwayland` (default) or `wayland`: no X server at all, and gogo's Wayland mode for a Proton game (`linux-app display <gogid> wayland`, [Wayland mode](#wayland-mode)). Beats the manifest's `[window] display` |
| `--secs N` | the limit of the game phase, default 600. Over it, the run stops and fails |
| `--gogo PATH` | this gogo; `.#umu-python` supplies the FHS env. Default: `nix build .#gogo-linux-app` |
| `--installer FILE` | build from this GOG setup `.exe` ([below](#building-from-an-offline-installer)) instead of the depot |
| `--keep` | keep the scratch dir. `cleanup` leaves it; delete it by hand |
| `--no-cache` | download Proton, umu's runtime and the game afresh, into scratch |
| `--reuse-install` | run on a copy of an installed base with the same inputs, with no build, or build and save one ([below](#reusing-an-installed-game)). For manifest rounds only: the final run before a PR builds afresh |
| `--out DIR` | results root, default `target/linux-e2e/results` |

`live start` takes these too, and its own ([Live sessions](#live-sessions)).

**Login.** The build uses your usual keyring login, through the session bus, which only the
build's sandbox sees. It needs `gogo login` done once. Tokens are never printed.

**Exit codes:** `0` every check passed, `1` a check failed, `2` usage, `3` harness problem
(says nothing about the game).

**Output:** progress on stderr, the summary JSON on stdout, then on the last line the path of
`summary.json`. Its `failed` lists the failed checks by name. Everything is in
`target/linux-e2e/results/<gogid>-<time>-<pid>/`:

| File | |
|---|---|
| `summary.json` | checks, steps, screenshots, `display`, audio, `build`, doctor, log excerpt, cleanup, `tested_entry` |
| `shots/<name>.png` | the manifest's screenshots, `not-exited.png` (`not-exited-relaunch_<n>.png` at a [`relaunch`](#a-clean-stop-a-kept-disk-and-a-relaunch)) if a game ignored its exit, `build-window-<n>.png` for each window the build opened |
| `build.log`, `build-sway.log`, `build-sandbox.log` | what the build phase printed |
| `sway.log`, `game.out`, `sandbox.log`, `audio.log` | what the game phase printed. `game.out` is the last launch's; with a `relaunch`, the ones before are `launch<n>-game.out` |
| `support-inno.log` | the support installer's Inno log, when it wrote one |
| `install-inno.log`, `install-wine.log` | what the installer wrote, with `--installer` |
| `run.log`, `launch.log` | the launcher's logs (design 18 §7.3) |
| `doctor-before.json`, `doctor-after.json` | `gogo linux-app doctor` around the run. A live session's life on a disk that already has its report says whose instead: `{"same_disk_as_life": N}` |

## What a run does

1. **Scratch.** `target/linux-e2e/runs/<id>/` holds `GOGO_HOME`, `HOME` and the XDG dirs.
   Proton, umu's runtime and the depot download are bind-mounted from
   `target/linux-e2e/cache/`, so a re-run downloads nothing and the build is still fresh.
   Runs take turns, through [the cache lock](#the-cache-lock). `--reuse-install` copies an installed base in here
   instead, and skips step 2 ([below](#reusing-an-installed-game)).
2. **Build,** in a sandbox with a NATed network of its own (pasta) and the session bus, on a headless sway of its
   own (software rendering, whatever `--gpu` says): the isolation checks, then
   `gogo linux-app <gogid> --yes`. `build` in the summary has its exit, how long it took, the
   windows it opened and how the support installer ended.
3. **Game,** in a sandbox with neither network nor bus (below): sway, Xwayland (none in
   [Wayland mode](#wayland-mode)), a null sound device, the isolation checks, then `launch.sh`, the steps, the exit, and doctor.
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
| `isolation_tmp` | `/tmp` is a tmpfs with only the X server's files (none in Wayland mode) |
| `isolation_runtime_dir` | `XDG_RUNTIME_DIR` is on the sandbox's own `/run` |
| `isolation_env` | no `HYPRLAND_*` |
| `isolation_paths` | the user's game dirs, menu entries, Steam, Downloads, session bus and sound sockets aren't visible |
| `isolation_display` | `xwininfo -root` answers with the manifest's size, on this `/tmp`'s socket, and this sandbox's Xwayland serves that number, `:0`. In Wayland mode: the only listening Wayland socket is our sway's, in `XDG_RUNTIME_DIR`, and its one output has the manifest's size |
| `isolation_no_x` | Wayland mode only: no Xwayland process, no listening X socket (filesystem or abstract), nothing in `/tmp/.X11-unix`, and no `DISPLAY` in the game's environment |
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

## The cache lock

One `run` or `cleanup` at a time over a cache, whichever checkout starts it. The lock is
`.lock` in the *resolved* cache dir (`target/linux-e2e/cache/.lock`, followed through a
worktree's link to the main checkout's; refused if it isn't ours or is itself a link), held
as a `flock` on fd 9, which no sandbox inherits.

- **Why the whole run, not just the build.** The build writes the store (Proton, umu's
  runtime), the depot cache and the installed bases; a reused base is copied and saved in
  there; and the game takes the GPU, and the harness assumes one game at a time. Parallel
  game phases would need per-run GPU and display bookkeeping that nothing needs yet, so the
  simplest rule that is also right is one run per cache. It replaces the old per-checkout
  `target/linux-e2e/lock` and the bases' `.lock`: one lock, no order to get wrong.
- **Waiting.** A `run` that finds it taken logs who has it and waits. `.lock.holder` next
  to it has the holder's checkout path, run id, pid and start time; a run writes it once it
  has the lock and removes it on the way out. The note is information only: a SIGKILLed run
  leaves a stale one (the log says when its pid is gone), and the flock alone decides.
- **`cleanup`** takes the same lock but doesn't wait: with a run going, in any checkout, it
  names the holder and exits 3, so it never sweeps or prunes under a run. Before that, it
  stops orphaned [live sessions](#the-lock-the-limits-and-cleanup).
- **A live session** holds it from its build to its `stop`, bounded by its idle limit and hard
  cap, and a waiting run's log says so ([below](#the-lock-the-limits-and-cleanup)).
- **Self-test.** `self-test` uses a lock dir of its own (`GOGO_E2E_LOCK_DIR`, under
  `target/linux-e2e/`), so it never waits for a real run, and bases of its own
  (`GOGO_E2E_INSTALLED_DIR`): its `cleanup` sweeps `.runs`, and under another lock that would
  be a real session's scratch, so one without the other is refused. It covers two runs on one lock
  (the second waits and names the first, then goes), `cleanup` refusing meanwhile, and a
  stale note.
- Runs from a checkout older than this lock don't take it. Rebase before running in parallel.

## Building from an offline installer

`--installer <setup.exe>` builds through `gogo linux-app <setup.exe> --yes`, gogo's installer
lane (design 18 §10), instead of the depot. Inno Setup runs silently under umu in a staged
prefix (`GAMEID=umu-0`, `/DIR=C:\GOG\<slug>`), and the game's id, entry point and renderer
come from the tree it installed, as they do for a depot build. It uses no game download, so
there's no `cache/depot/<gogid>` and no @noosxe's OK to ask for, but it can only build the game
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
- **Disk:** the installed game is in scratch, deleted with it, so every run installs again
  unless it's `--reuse-install` ([below](#reusing-an-installed-game)). Scratch needs the
  game's size free (Cyberpunk 2077: about 70 GB), and so does each installed base.
- `build.installer` in the summary names the file. A self-test's `build_command` finds it in
  `GOGO_E2E_INSTALLER`.

## Reusing an installed game

`--reuse-install` is for the rounds of a manifest edit: run, read the screenshots, edit,
run again. Without it every run builds the game, which for Cyberpunk 2077 is 9 of a
round's 13–15 minutes. With it, the first run builds as usual and then saves what the
build left as an **installed base**; the next runs with the same inputs skip the build
and launch a copy of it.

```bash
scripts/linux-e2e.sh run 2086050016 --reuse-install
```

- **The default is a fresh build.** CI and the final run before a PR build afresh. A
  reused run never prints `tested_entry`: a compat entry stands on a fresh build.
- **A base is the state right after the build, before any launch.** It's saved after the
  build's sandbox has exited and before the game's starts, so a reused run is a first
  launch on disk: no accepted EULA, no settings, no shader cache. Nothing writes to a base
  after that: each run gets its own copy.
- **The copy is copy-on-write:** a btrfs snapshot, or reflinks (`cp --reflink=always`) on
  XFS or a btrfs without unprivileged snapshots. Either takes well under a second for a
  70 GB game and no space until the game writes. On a filesystem without either (ext4,
  CI's) it's a full copy, logged. [Copies](#copies-snapshots-reflinks-full-copies) has the
  modes, how one is picked and the opt-out.
- **The same path as the build.** `launch.sh`, the prefix and `state.toml` name absolute
  paths, and a data root that moved sends `launch.sh` through setup again. So every
  `--reuse-install` run of a game, the building one too, has its scratch at
  `target/linux-e2e/cache/installed/.runs/<gogid>`, and they take turns with every other
  run on the cache lock. With `--keep`, the dir is renamed to `.runs/<gogid>-<time>-<pid>` after
  teardown, out of the next one's way, so paths inside it name the old one.
- **Checkouts share them.** When `target/linux-e2e/cache` is a link to another checkout's
  cache, as in a worktree, the bases and their runs are that cache's, so a base built in
  one worktree serves the next. That's why the scratch lives beside the bases and not in
  `target/linux-e2e/runs/`.
  The same link is why the lock is the cache's, not the checkout's
  ([below](#the-cache-lock)).
- **The key** is the sha256 of everything that goes into a build: gogo's sources (the
  workspace, `compat/`, the toolchain and the Nix that builds it, so its compat DB,
  `LAUNCHER_VERSION` and the Proton and umu pins; not its store path, which a manifest
  edit changes too), or a `--gogo` binary's bytes, the build command, the
  installer's files by name, size and mtime, or for a depot game the download cache's
  listing, the scratch's seed (`config.toml`: no L4 yet), the build's sandbox code
  (`declare -f`), its environment, the tools' store path and `REUSE_FORMAT`. Another of
  any of them builds a new base beside the old one. A depot game is keyed on its download
  cache as it is, so a newer build on GOG shows up once a fresh run fetches it. The key's
  inputs are in the base's `meta.json`.
- **Checked before every copy:** the bases' dir resolves to a
  `target/linux-e2e/cache/installed`, and the base and its `meta.json` are the harness's
  own, not links, and for this key and path; every entry in it is ours; and
  no link leads from the run's path into the user's home or `target/linux-e2e/` outside
  the run (`tree-check` in the helper). A failure is a harness error (exit 3), and nothing
  is copied. A build that leaves such a link isn't saved. A base is only read, and
  deleting a run's scratch removes its copy only: `rm` doesn't follow links. The isolation
  checks, `nothing_left`, `scratch_deleted` and the run marker are as for any run.
- **What's not in a base:** the run's own logs and results. Proton, umu's runtime and the
  depot download stay in the shared cache, bind-mounted as always.

`summary.json`'s `build` says which it was:

| Key | |
|---|---|
| `build.reused` | `true`: the game ran on a copy of an earlier build, and `build.exit` and `build.secs` are `null` |
| `build.reuse` | `null` without `--reuse-install`. Else `key`, `base` (its dir), `source` (`cache` or `fresh build`), `copy` (`snapshot`, `reflink` or `copy`), `copy_secs`, `saved` (this run's build became a base), `note`, and `base_meta` (the run that built it, when, how long it took) |
| `build.copy` | `null` when nothing was copied (a run without `--reuse-install` or a live session). Else `mode` (`snapshot`, `reflink` or `copy`), `asked` (`GOGO_E2E_COPY`, `auto` by default) and `note`: why it isn't a snapshot, or that a plain base was just made subvolumes |

**Disk.** A Cyberpunk 2077 base is about 70 GB, once. Its copies share its extents.
`cleanup` lists the bases, their size (`du`, which counts shared extents in full) and
whether they're `snapshot` (subvolumes) or `plain`. It also removes saves that never
finished. `cleanup --prune-installed <name>` removes one, by the
name it lists, and `--prune-installed all` all of them.

### Copies: snapshots, reflinks, full copies

A reused run's copy of its base, and a live session's disk and every restore of it, are
made one of three ways (#255):

| Mode | How | When `auto` picks it |
|---|---|---|
| `snapshot` | The base's `gogo`, `home` and `xdg` are btrfs subvolumes. A copy is a `subvolume snapshot` of each: no walk over the files. | The cache and the scratch are on one btrfs, and a probe works unprivileged: it creates a subvolume, snapshots it into the scratch, and deletes both |
| `reflink` | `cp -a --reflink=always`: shared extents, but every file is visited | Reflinks work between the two dirs (btrfs, XFS) |
| `copy` | `cp -a` | Neither works (ext4, as in CI) |

- **The opt-out:** `GOGO_E2E_COPY=reflink` or `copy` forces that path even on btrfs, and
  no subvolume is made. `snapshot` insists: without snapshots the run is a harness error
  (exit 3). `auto` is the default. Anything else is a usage error. The probe and the mode
  are logged (`copies: …`), and `build.copy` records them.
- **No root, no btrfs-progs.** Creating and snapshotting use two ioctls through the
  helper (`subvol create`, `subvol snapshot`). btrfs-progs would have to go in the tools,
  and the tools' store path is part of every base's key, so adding it would orphan every
  base already saved. Deleting needs no ioctl. `rm -rf` empties a subvolume and rmdir(2)
  takes the empty one (kernel 4.18+). `btrfs subvolume delete` would need root or the
  `user_subvol_rm_allowed` mount option. Teardown, `cleanup` and `--prune-installed`
  check that what they deleted is gone. A leftover subvolume shows in their log and keeps
  `scratch_deleted` false.
- **What's a subvolume:** each base's `tree/gogo`, `tree/home` and `tree/xdg`, a reused
  run's copies of them, and a live session's disk and its lives. Nothing is ever
  snapshotted with a subvolume inside it: an inner one would show up in the snapshot as an
  empty dir. The sandbox binds the scratch once (`--bind $RUN $RUN`), so the subvolumes
  are plain dirs in there. The cache's store, depot and umu dirs are bound over empty
  dirs in `xdg` and stay out of every copy. Each subvolume is its own `st_dev`, so a
  rename between `gogo`, `home` and `xdg` is `EXDEV`, as it already is into the store's
  bind mount. Nothing in a build or a launch does one. If a game ever does,
  `GOGO_E2E_COPY=reflink` is the way back.
- **A restore** moves the last life's subvolumes aside, into the scratch's `.trash/`,
  and snapshots the disk again. Emptying a subvolume is a walk over its files, so the
  trash is deleted with the scratch at teardown, off the restore's path.
- **Bases from before.** A base saved before this, or with `GOGO_E2E_COPY=reflink` or
  `copy`, is plain dirs. The first run that meets one in `snapshot` mode converts it, with
  the cache lock held. It builds a reflinked copy of the tree beside the old one, as
  subvolumes, then swaps the two in one rename (`mv --exchange`) and deletes the old tree.
  `meta.json` then says `"copy": "snapshot"` and when it was converted. A failure at any
  step leaves the base as it was, and that run copies by reflink. `cleanup` sweeps a
  half-done conversion (`.tree-new-*`). The key doesn't change, so a checkout from before
  this reuses a converted base too: to it, the subvolumes are plain dirs to read, and
  `rm -rf` prunes them.

What it buys depends on the tree. On Cyberpunk 2077's base (63 GB, but 950 files, all
large), copying takes well under a second in either copy-on-write mode. The disk was never
the slow part of a reused start or a restore (below). The gain is real for trees of many
small files, such as a Wine prefix: the self-test times `copy`, `reflink` and `snapshot`
on 20 000 small files and logs it.

Measured on a dev machine (btrfs, zstd, NVMe, kernel 7.2; 2026-11-08). The times
are for the copy alone, then for deleting it (`rm -rf`), which a restore leaves to
teardown in snapshot mode:

| Tree | `copy` | `reflink` | `snapshot` | `rm -rf` |
|---|---|---|---|---|
| 200 000 files of 100 B (782 MB) | 2.13 s | 2.12 s | 0.05 s | 1.3–1.5 s |
| 20 000 files of 64 KB (1.3 GB) | 0.21 s | 0.21 s | 0.04 s | 0.15 s |
| Cyberpunk 2077's base, 950 files (63 GB) | — | 0.05 s | 0.02 s | 0.02 s |

On btrfs, `cp -a` reflinks through `copy_file_range` anyway, so `copy` costs what
`reflink` does there. The self-test's own line (20 000 files of a few bytes) read
copy 0.23 s, reflink 0.23 s, snapshot 0.05 s.

Cyberpunk 2077 with `--reuse-install`, a short live session, before (main, `reflink`) and
after (this, `snapshot`, which also converted the base on its first run): `live start`,
the title, `wait 30`, a screenshot, `checkpoint title`, `restore title`, `stop`.

| | before: reflink | after: snapshot |
|---|---|---|
| the copy at the start (`copy_secs`) | 0.1 s | 0.2 s (after the base's one-off conversion) |
| `live start` to `ready` (the window) | 27 s | 23 s |
| a restore's rollback | 0.1 s | 0.2 s |
| `restore` to `ready` | 80.4 s (replay 52.9 s) | 73.4 s (replay 45.5 s) |

The differences are noise from the game's start-up. A snapshot is three helper calls,
each a Python start and a btrfs commit, which on 950 files is no faster than a reflink.
A restore's time is the game's relaunch and the replay, which is #257.

**Continuing from a launched state** (the EULA accepted, a menu reached) isn't this: a
base is always pre-launch. A [live session](#live-sessions)'s checkpoints are routes from
that state, and its restores roll a copy back to it.

## Live sessions

A `run` proves a manifest; a **live session** is for writing one. The game keeps running in
the same sandbox while an agent drives it one command at a time: a screenshot, a key, a
click, a look at the PNG, the next key. When it gets lost, it restores a checkpoint. When
it has the route, `export` writes that route as a manifest, and a fresh `run` of it is the
proof, as always. Use one when the next step of a manifest is a guess: a menu nobody has
mapped, a prompt that may or may not come up, a key that may not land. A blind edit costs a
whole round each time (Cyberpunk 2077: 6.5 min with `--reuse-install`, 13–15 without).

```bash
scripts/linux-e2e.sh live start 1423049311 --installer ~/backups/cyberpunk/setup_cyberpunk_2077.exe \
    --gpu renderD129 --reuse-install --arg --launcher-skip --arg -skipStartScreen
```
```bash
scripts/linux-e2e.sh live <id> status --wait 900
```
```bash
scripts/linux-e2e.sh live <id> screenshot title
```
```bash
scripts/linux-e2e.sh live <id> key space
```
```bash
scripts/linux-e2e.sh live <id> checkpoint agreed
```
```bash
scripts/linux-e2e.sh live <id> restore agreed
```
```bash
scripts/linux-e2e.sh live <id> export > /tmp/route.toml
```
```bash
scripts/linux-e2e.sh live <id> stop
```

`live start` takes `run`'s options (but `--secs`) and returns at once with the session's id:
the build or the copy of a base goes on in the background, and `status --wait SECS` says
when the game's window is up. The manifest, `e2e/<gogid>.toml` or `--manifest`, is
optional, and only its `[window]`, `[env]`, `[override]` and `[exit]` count: the session
finds the steps (unless it [starts from them](#starting-from-a-manifest)). A game with no manifest yet gets the defaults.

| `live start` option | |
|---|---|
| `--env K=V`, `--arg A` | into the session's `[override]` (#250): gogo's `linux-app env set` and `linux-app args`, before every launch. Repeatable; the export keeps them |
| `--from-manifest` | start partway along the manifest instead of at the first frame ([below](#starting-from-a-manifest)). With `--to-shot NAME` (default: the manifest's last screenshot) or `--to-step N` |
| `--idle-secs N` | stop after N s with no command. Default 900 |
| `--max-secs N` | stop N s after taking the cache lock, at the latest. Default 5400 |
| `--max-gap N` | the gaps between commands that the replay and the export keep, cut to N s. Default 10 |
| `--match-pct N` | a replayed screenshot matches the recorded one when at most N% of its pixels differ. Default 15 |

| `live <id>` command | Answers (JSON) |
|---|---|
| `status [--wait SECS]` | its state, the game's, the window, the checkpoints and the idle time left; with `--wait`, once it's ready, or exit 4 when it's still busy (building, launching, replaying) |
| `screenshot [NAME]` | the PNG's path under `target/`, `change_pct` against the last screenshot (`since`), and `moving_pct`: how much a second frame half a second later differs, so a video or an animation shows as one. A name already taken gets `-2` |
| `key KEYSYM`, `type TEXT`, `hold KEYSYM MS` | as the manifest's `key` and `type` ([below](#manifests)); `hold` keeps a key down for MS ms |
| `press_until KEY --every SECS --max PRESSES --change PCT`, or `--marked X Y W H COLOR MIN [TOL]` for `--change`; `wait_until --every SECS --max LOOKS --marked ...` | as [the manifest's](#press_until); X Y W H in % of the window. Answers `presses` and `seen_pct`; exit 1 when it runs out |
| `click [X Y]`, `move_to X Y` | X Y in pixels of the screenshot, or `N%` of the window. `click` with none clicks where the pointer is (`click_here`) |
| `move_by DX DY`, `scroll NOTCHES`, `wait SECS` | as the manifest's |
| `settle [--max SECS]` | waits until the picture fills the window and holds still, by the code of the manifest's `wait_settled`. Answers `settled`, `secs` it took, the last frame's `path`, and the `step` it put on the path. An animated screen answers `"settled": false` after `--max` (default 30, at most 600), it doesn't hang |
| `checkpoint NAME` | a screenshot (`cp-NAME`), and the path to here kept under NAME, with about how long its replay takes. After the game quit by itself (`game.running` false in `status`), [a clean stop](#a-clean-stop-a-kept-disk-and-a-relaunch): no screenshot, the disk kept too, `clean_stop: true`. Refused if the exit wasn't [exit]'s code, or something of the game is still up |
| `restore NAME [--wait SECS] [--full-waits]` | the game closed, the disk rolled back, a relaunch and a [fast replay](#a-fast-replay) to NAME; answers once it's done, with `last_replay` (its `timeline`, step by step, and `cut_secs`) and `last_restore`: how long each part took (`phases`). `--full-waits`: every wait at the length the export has |
| `relaunch` | launches the game again on this disk, once it has quit by itself, and answers when its window is up. Refused while it runs, and as `checkpoint` is after a bad exit |
| `log [game\|run\|launch\|sway\|dxvk\|vkd3d\|proton] [--tail N]` | a log's last lines: `game.out` (or its DXVK, VKD3D or Proton/Wine lines), the launcher's run log or `launch.log`, sway's |
| `export` | the route so far, as a manifest, on stdout. Works after `stop` too |
| `stop` | the game closed (or its own exit noted, after a Quit), doctor, teardown, `nothing_left` and `scratch_deleted`, and the summary, as `run` prints it |

`live list` lists the sessions on this cache. Exit codes: `0` ok, `1` the command failed
(the session goes on), `2` usage, `3` no such session, `4` not ready yet.

### Starting from a manifest

To extend a known route (Cyberpunk's title → Settings → Graphics, then the next page), start
from it instead of driving it again by hand:

```bash
scripts/linux-e2e.sh live start 1423049311 --installer ~/backups/cyberpunk/setup_cyberpunk_2077.exe \
    --gpu renderD129 --reuse-install --arg --launcher-skip --arg -skipStartScreen \
    --from-manifest --to-shot graphics
```

The manifest is `e2e/<gogid>.toml` or `--manifest`, and it must exist. After the window is up
the session runs the manifest's steps **up to the point**, in the first life, then is ready:
`status` says `from_manifest` (`ok`, `done` of `of` steps run). The agent goes on live from
there; `checkpoint`, `restore` and `export` work as always.

- **The point.** `--to-shot NAME` runs through that screenshot and the `expect_change`
  checks right after it; with no `--to-*` it is the manifest's last screenshot.
  `--to-step N` runs steps 1 to N, **every** `[[step]]` counting, waits, inputs, screenshots
  and checks alike, from 1 in file order. The two exclude each other. A name or number that
  isn't in the manifest (or `--to-*` without `--from-manifest`) is a usage error, exit 2,
  before anything is built.
- **How the steps run: as a `run` runs them, with their own checks.** A screenshot of a
  restore's replay is compared with the frame recorded in the live session, and a manifest
  step has none. So the manifest part is run through the manifest's checks (`not_black`,
  `fills_window`, `expect_change`, `wait_settled`), by the same step runner as a `run`'s,
  and the frames that run took are kept as the recorded ones: a later `restore` matches
  against them (`--match-pct`, and a frame's own motion, as for any recorded frame). On a
  restore the replay does the manifest part first, then the live part; its `expect_change`
  checks aren't run again (the shots they compare were matched).
- **A check that fails stops it there.** The path ends at the step before, `status` tells
  `from_manifest.step` and `why` (the check and its numbers), and the session is ready at that
  point: the game is where the failed step left it, so take a screenshot before going on. The
  failure isn't held against the session's summary, which is not a proof. If the window never
  comes up, there is no point to be at: `from_manifest.ok` is false and the session behaves as
  any without a window (`restore` or `stop`).
- **`[window]`, `[env]`, `[exit]` and `[override]`** come from the manifest, as for a session
  with no `--from-manifest`; so do the `--env` and `--arg` that add to `[override]`. A clash:
  `--env K=V` replaces the manifest's value for K, `--arg` replaces the manifest's
  `extra_args` as a whole (they are not appended). The merged `[override]` is what the
  manifest part runs under, and what the export writes.
- **The export** is the manifest's steps up to the point, as written (a black or partly
  empty screenshot of the manifest stays, since its own run passed it; a wait a late match
  lengthened is lengthened), then the live steps as usual. **The steps after the point are
  not added back**: they were never run in this session, so nothing says they still hold
  after the new ones. Paste them on by hand if the route continues, and prove it with a
  fresh `run` as always. A comment at the top says how many of its steps are the manifest's.

### Lives, checkpoints and restore

A process with a Vulkan device, Wine's server and pressure-vessel's containers can't be
frozen and thawed (CRIU doesn't do GPU contexts), so a checkpoint is a **route, not a
process**: the session keeps the disk as it was right before the first launch, and the
list of steps since (and the disk again at [a clean stop](#a-clean-stop-a-kept-disk-and-a-relaunch)). Each launch is a **life**: its own sandbox, sway, Xwayland, sound
server and isolation checks. `restore NAME` ends the life (the launcher's own `TERM`
teardown), and the supervisor sweeps, rolls the game's `gogo`, `home` and `xdg` back to the
launch (`restore_base`, the same copy `--reuse-install` makes), and starts the next life,
whose driver replays NAME's steps before it listens again.

- **The disk it rolls back to** is the installed base itself when the session runs on one
  (`--reuse-install`: only ever read), or else a snapshot in scratch taken before the first
  launch, made [as a base's copies are](#copies-snapshots-reflinks-full-copies): subvolumes
  on btrfs, reflinks on XFS, a full copy on ext4, logged.
- **Why the launch, not the moment of the checkpoint.** A snapshot of a running game is
  what a power cut leaves: Wine writes the registry when it likes, a game its settings when
  it quits. And a game relaunched on a disk that remembers a screen (an accepted agreement)
  takes another way to the same menu, maybe with the focus elsewhere: the session would then
  be somewhere the exported manifest never goes. From the launch's disk, the replay is the
  route the export writes, so a replay that matches is a dry run of the export's route
  (of its timing too, with `--full-waits`).
- **The replay** goes by the export's timing, each gap cut to `--max-gap` and each `wait` in
  full, except where [the screen shows it can go sooner](#a-fast-replay). At each screenshot it retakes the frame until at most `--match-pct`% of it differs
  from the one recorded, for up to 30 s more; a late match lengthens the wait before it,
  for the next replay and the export too. The first that never matches stops the replay
  there: the route ends at the last step that matched, `status` says why
  (`last_replay.why`, with both PNGs), and the session is ready from that point.
  A recorded frame that was black isn't compared. A frame's own motion (`moving_pct`) is
  allowed on top of `--match-pct`; one that moved by `--match-pct` or more in half a second
  (a video) is only checked for a picture, since no two frames of it match.
- **A screenshot of a screen that isn't settled** is a trap: Cyberpunk's intro plays for a
  varying time before the agreement comes up. A shot taken while it may still be either
  makes the replay stop there, rightly, as the export would fail at it. Take the shot after
  the key that settles it.
- **What a restore costs:** a relaunch (the sandbox, `launch.sh`, the game's own start)
  plus the replay, at most about what `checkpoint` and `status` say as `replay_secs`, which
  counts only what's past a kept disk, and every wait in full. Cyberpunk 2077
  on the 4090: see [What it cost](#what-it-cost-cyberpunk-2077).
- **Dead ends** (what was done after the checkpoint, before the restore) leave the route,
  and stay in `journal.jsonl`.

#### A fast replay

A replay doesn't sit through what the screen says is over (#257). The route on the path
isn't touched, so **the export is the same, byte for byte in its steps,** whatever a replay cut,
and a `run` still waits the whole `wait`.

- **A wait or a gap leading up to a screenshot** ends once the screen has arrived there: a
  frame a second, compared with the recorded one, and two in a row must be within the
  frame's own motion × 1.5, plus 1%. That is much tighter than `--match-pct`, which also
  takes a screen one step short (Cyberpunk's agreement, scrolled to its end, is 9% off).
- **It doesn't poll,** and the wait runs in full, when a poll can't tell: a run of waits under
  3 s, a recorded frame that moves by `--match-pct` or more or is black, or a frame so like the
  screen before the input that arriving doesn't show. **A wait with an input next has no
  frame to go by, so it stays whole**: that's most of what's left on Cyberpunk.
- **A gap after a matched screenshot** (the agent reading it) is cut to 1 s: the screen is
  where it was then. A `wait` is never cut this way: it says what the game needs.
- **Frames don't go through a PNG:** `grim -t ppm`, the recorded frame from a raw copy
  in scratch (`live/refs/`), a PNG written unfiltered for the agent. 0.7 s a screenshot,
  against 4 s. A `run`'s screenshots aren't changed, since a manifest's timing has them in it.
- **A failure may be its own doing,** so it isn't the verdict: a fast replay that fails past its
  first check ends that life, and the next one replays at full timing, whose answer stands.
  `last_replay.retried_because` says what the fast one saw, `last_replay.full_waits` that this
  was the full one, and a retry is a restore of its own in `session.json`, with `retry: true`.
  A failure with nothing before it to blame stops there, as any replay does.
- **What it proves:** a fast replay that matched says the route still reaches each screen,
  not that its waits are long enough. That's still a fresh `run`'s to say.

**doctor's report before the launch** is once per disk (the launch's, each kept one): it's a
record, which no check reads, and 7 s on Cyberpunk. `doctor-after`, at a `stop`, is
as ever, and every life still has its own isolation checks, sound check, GPU probes and sweep.

#### Moving a game between launches

A `move_relaunch` step (#278) is a `relaunch` with `gogo linux-app move` in the middle, and
only in a `run`, not a live session. The game has quit by itself, a save is planted in the prefix,
and the game is moved to `library` (`$RUN/library`, so a rename), `other_fs` (`/e2e-other-fs`, a
tmpfs of the sandbox's own, so a copy, outside `$HOME` and the scratch: the one path that tests
the container's share of a library) or `default` (`games/`). The copy to `other_fs` is first
killed with `SIGKILL` 0.3 s in, and the game must still be where it was. Then the checks:
`move`, `move_place` (`apps list` says the new dir, the old one's gone), `move_saves` (the save,
everything under `users/`, every link and the registry match), `move_doctor` and
`move_menu_entry`, and the next launch uses the path the menu entry runs. A tmpfs holds the game in
memory, so pick a small one. `e2e/move-1741027632.toml` does all three in turn:

```bash
scripts/linux-e2e.sh run 1741027632 --manifest e2e/move-1741027632.toml --secs 1200
```

#### A clean stop: a kept disk and a relaunch

A game that has quit from its own menu is what a snapshot of a running one isn't: its
settings and Wine's registry are written, and nothing is left to write more. So
`checkpoint NAME` right after a Quit keeps the disk as well as the route (#252), and
`restore NAME` needs no replay. It rolls back to that disk and launches again. A game
relaunched on a disk that remembers a screen takes another way to the same menu, which is
why the route has to say so: the export writes a **`relaunch` step** at that point. A fresh
`run` then quits, launches again in the same sandbox on the same disk, and goes on, as the
session did.

```bash
scripts/linux-e2e.sh live <id> key Return
```
```bash
scripts/linux-e2e.sh live <id> status
```
```bash
scripts/linux-e2e.sh live <id> checkpoint agreed-clean
```
```bash
scripts/linux-e2e.sh live <id> relaunch
```

`Return` on Quit Game, then `status` until `game.running` is false, since a game takes a
moment to go, and a `checkpoint` while it's still up is an ordinary one. The answer's
`clean_stop` says which it was.

- **What counts as a clean stop:** the launcher has exited with [exit]'s `code` (default
  0), and in up to 15 s nothing of the game is left in the sandbox (`wineserver` lingers
  for a moment). Anything else (a crash's exit, a process that stays) isn't one:
  `checkpoint` and `relaunch` are refused with what they found, and `restore` or `stop`
  go on from there. An ordinary checkpoint behaves as ever, on the launch's disk or a kept
  one, whichever its route starts from.
- **`relaunch` while the game runs is refused,** and nothing goes on the path. It never
  closes anything: quitting is the game's to do, and `restore` is what closes a running
  one. A restore's reply says which disk it took (`kept` or `launch`).
- **A restore to a clean-stop checkpoint** ends the life, snapshots the kept disk, and
  starts the next life, whose launch stands for the `relaunch`: `last_replay.steps` is 0.
  With no `relaunch` on the path yet, one is added there. `[override]` isn't written
  again: the disk has it.
- **A restore past one** (an ordinary checkpoint later on the route) rolls back to the
  **last kept disk on its path**, relaunches, and replays only what came after that
  relaunch; `last_replay.skipped` counts the steps it stood on. A checkpoint from before
  the stop has no kept disk on its path, so it goes back to the launch and replays it all.
  A `relaunch` on a path with no kept disk before it is replayed as it was driven: the
  quit, the exit, the launch, in one life. A failure there stops the replay, as a frame
  that doesn't match does.
- **`--from-manifest` takes a manifest with `relaunch` in it.** It's a step like any
  other (it counts for `--to-step`, and may be on the way to `--to-shot`), run with its
  checks, and the session goes on past it, with no kept disk.
- **A screenshot compares with nothing across a relaunch.** A `since` or an
  `expect_change` from before it would only say that another launch looks different, so
  the export writes none. A hand-written one is allowed, and checked.
- **Disk.** Each clean-stop checkpoint keeps a copy under `live/disks/<n>/tree/` in the
  scratch, made by the supervisor outside the sandbox with [the same copies as a
  base](#copies-snapshots-reflinks-full-copies), so without the sandbox's bind mounts:
  a snapshot or reflinks, costing only what the game then writes, but a **full copy** on
  ext4 (Cyberpunk 2077: 63 GB each), and a failed copy fails the `checkpoint`, with the
  session going on. A replaced name's copy stays, since a later checkpoint's route may
  stand on it. They all go with the scratch at `stop` or any other teardown, and
  `scratch_deleted` fails if one won't; `cleanup` takes a dead session's too.
  `live.kept_disks` in the summary counts the checkpoints that have one.

**In a `run`,** the `relaunch` step fits the checks like this, all in the sandbox the run
already has. Each `<n>` counts from 1.

| Check | At the `n`th relaunch |
|---|---|
| `clean_exit_relaunch_<n>` | the game exited by itself within [exit]'s `wait_secs`, with its `code`, also when the last launch ends by `how = "signal"`. If not, the steps end here: there's no way on from a game that didn't quit (`finished` says so) |
| `nothing_left_relaunch_<n>` | nothing but the harness's own processes is up in the sandbox after 15 s. If something is, it fails and is killed, and the run goes on. The harness's `nothing_left`, from outside, stays the one at teardown; a live restore's sweep between lives is as it was, a harness error if something stays |
| `window_relaunch_<n>` | the window is up within `[window]`'s `wait_secs`, as `window` says for the first launch. If not, the steps end here |
| `gpu_used_by_game_launch_<n>` | with `--gpu`: the launch that just ended held the GPU, by `gpu_used_by_game`'s rules, read at that launch's screenshots. A launch with no screenshot isn't asked, unless none has one. `gpu_detail.launches` has what each one held |

`clean_exit`, `torn_down`, `gpu_used_by_game` and doctor's checks are the last launch's,
as for a run with one launch. `steps` has `exit`, `exit_secs` (how long the quit took) and
`window_secs` on each `relaunch`.

### Time, and what the export writes

A live session runs in real time: the game moves on while the agent thinks. Every step keeps
the gap of real time before it. A gap over `--max-gap` (10 s) is cut to it, since the rest was
thinking, not the game. One under 0.2 s is dropped; above that the pause between two keys
is kept, since a game can drop keys pressed at once. So **say what the game needs with `wait`**: after a key that
starts a load, `wait 15` is kept whole, and the replay and the export wait as long.

**`settle` or `wait`?** On a screen that ends up still (a static menu, a loading screen that
ends in one), use `settle`: the export writes `wait_settled`, which waits as long as the machine
needs, where a `wait` is as long as it took you, and breaks on a slower one. Use `wait` for a
screen that never holds still (Cyberpunk's animated menus, a looping video): `settle` there says
`settled: false` after `--max`, and the export writes the time it spent as a plain `wait`, with
a comment saying it didn't settle. A settle that took *t* s exports as `wait_settled = 2t + 10`
(whole seconds, so a machine twice as slow still passes); a restore's replay waits for the
screen to settle again, up to that, and stops there if it doesn't.

`export` writes the route as a manifest, and checks it with the manifest parser:

- `[window]`, `[env]` and `[exit]` from the session's manifest, `[override]` with `--env`
  and `--arg`. `[exit] how = "self"` when the game exited by itself after the last step
  (a Quit from its menu), else `"signal"`.
- The steps in order, each gap a `wait`, checkpoints as comments. A `relaunch` is a step,
  and takes [exit]'s `wait_secs` and `code` along even when the last launch ends by signal.
- After a screenshot that follows an input, an `expect_change` from the screenshot before,
  at half the change the session saw (at most 20%, none under 2%). A wait a fresh run would
  otherwise only sit through becomes a check that the screen moved on.
- A screenshot that was black is dropped (a fresh run's `not_black` would fail it), and
  one that didn't fill the window gets `min_fill = false`.

It's a draft: read it, prune the probes, and prove it with a fresh `run`. `stop` writes it
to `live/manifest.toml` in the results, too.

### The lock, the limits and cleanup

- **The cache lock.** A session takes [the cache lock](#the-cache-lock) as a run does, and
  holds it for its whole life: its game has the GPU, a reused base's scratch
  (`installed/.runs/<gogid>`) is the one every run of that game uses, and a restore copies from
  the base. A run that waits for it is told it's a live session, when it stops if idle, and
  the time it ends by at the latest (`.lock.holder`'s `live_until`). A session that is waiting
  for the lock says so in `status`; `stop` ends it there.
- **It can't outlive the agent.** `--idle-secs` (15 min with no command, `status` counts)
  and `--max-secs` (90 min from the lock) end it through the normal teardown, and fail it
  (`live_idle`, `time_limit`), as do a signal and every other way out. So the longest
  another run waits for a session is its `--max-secs`.
- **The registry** is `live/<id>` beside the lock, in the resolved cache, so every checkout
  that shares the cache sees it: the supervisor's pid and start time, its results and
  scratch. `cleanup` (in any checkout) looks there first. A pid counts as the session only
  while its start time still matches and it still runs the session's `__live-serve`: an
  entry outlives a SIGKILL, an OOM or a reboot, and its pid may be anyone's by then, so
  nothing is ever signalled on the pid alone. A session whose supervisor is gone (its
  sandbox dies with it) has its leftovers swept, its entry removed and its scratch deleted. A
  session past its hard cap, or ready but not answering its socket, is an orphan: `TERM` (its
  own teardown), then `KILL`. Any other is in use: `cleanup` names it and, as under a run,
  doesn't sweep. `cleanup --stop-live` stops every session on the cache.
- **The supervisor runs from a copy** of the script, in `live/` in the results: bash reads a
  script as it goes, and an edit to the checkout's would land mid-function under a long
  session.

### The control socket

One per session: `ctl.sock` in `live/` in its scratch, bound under umask 077 in a 0700 dir. The driver
binds it from inside the sandbox, through the dir's `/proc/self/fd` (a scratch path can pass
a socket address's 108 bytes), and a client checks that it's a socket and ours before it
connects. A request is one line of JSON, `{cmd, args}`, and so is the answer. Commands run
one at a time; while one runs, or the launch, or a replay, `status` still answers, `stop`
stops, and anything else is told to try again (exit 4). The client passes the socket's dir in
`GOGO_E2E_LIVE_DIR`, never on its command line, which the teardown's sweep reads as one of the
run's own.

Everything is in the results dir, `live/`: `state.json` (what it's doing), `session.json`
(the route, the checkpoints, the restores), `journal.jsonl` (every command and its answer,
dead ends too), `manifest.toml` (the export, from `stop`); beside them `session.log` (the
supervisor's stderr), each life's logs as `life<N>-game.out` and so on, and `summary.json`
with a `live` key. A session never prints `tested_entry`: a fresh run does.

### What it cost: Cyberpunk 2077

The proof session (`--reuse-install`, `--arg --launcher-skip --arg -skipStartScreen`, the
RTX 4090), from `live start` to `stop`: 19 min.

| | |
|---|---|
| `live start` to the window (gogo's build, the base's reflink copy, Wine's and the game's start) | 5 min |
| the title, the agreement scrolled and accepted, analytics up: `checkpoint agreed` | 4 min, mostly reading PNGs |
| a `restore agreed` that stopped: the intro was still playing where life 1 had shown the agreement | 1 min 43 s (a 28 s relaunch, the replay up to its 30 s of grace) |
| the route redone from there without that shot, `checkpoint agreed` again | 2 min |
| `restore agreed`: matched, 11 steps | 2 min 38 s (the replay 131 s) |
| analytics declined, Settings, Graphics, back, Quit Game (exit 0), `export`, `stop` | 4 min |

What it found that the old manifest had to guess or got lucky on (the last two are settled now, [below](#a-route-that-doesnt-race-cyberpunk-2077)):
with `-skipStartScreen` the title is up in about 30 s, not 150; the intro takes longer on
some launches than others, so a fixed `wait` before a title screenshot is a race; `click`
doesn't aim (the game's own cursor, from relative input), which a click and a screenshot
showed in 10 s; and back on the menu after Settings, `Left` and `Down` × 4 landed on the
Phantom Liberty banner, not Quit Game, in this session (after that click; the old manifest's
runs reach Quit Game with them): `Return` there would open its store page, and a screenshot
before it caught that.
Each is a 6.5 min round as a blind edit (`--reuse-install`), so four of them are 26 min
before the route is right, against 19 for the session that found them all, its two
restores included. The export then needs its fresh `run`, as any edit does: the
installer build and the whole route, without `--reuse-install`, passed in 13 min 15 s, every
check ok, Quit Game's exit 0. It stays a draft beside `e2e/1423049311.toml`, which goes on
through every Graphics page and Video for #232; the session's lessons go into that one by
hand.

**A clean-stop checkpoint** (#252; the RTX 4090, `--reuse-install`, `--launcher-skip`,
`-skipStartScreen`, 2026-11-08). Past the agreement and analytics, Quit Game from the menu,
`checkpoint agreed-clean`, a relaunch, a dead-end move, `restore agreed-clean`. From each
restore's `last_restore`:

| | a restore before: `agreed`, past the agreement (above, #257) | `restore agreed-clean` | `restore on-credits`, 8 steps past the stop |
|---|---|---|---|
| closing the game | about 8 s | 0.8 s | 0.8 s |
| the disk's rollback | 0.1 s | 0.5 s | 0.2 s |
| the sandbox (sway, the isolation checks, the GPU probes), then launch to the window | about 28 s | 18.4 s + 5.0 s | 18.0 s + 4.5 s |
| the replay | about 131 s (96 s in another session) | 0 s, 0 steps of 32 | 73 s, 8 steps, 32 standing |
| `restore` to `ready` | 102–158 s | **24.7 s** | 96.6 s |

So the replay was the lever. What's left of a restore was mostly the sandbox's own start
(until #257, below),
and the game's window comes up within 5 s of the launch; that's still the window, not the
menu. On the remembered disk Cyberpunk plays its intro from the window on (`space` skips it,
after about 30 s), then goes straight to the menu, no agreement and no analytics. That's the
other route the `relaunch` step keeps honest. The menu after a restore is about 40 s further
on, which the 73 s of the `on-credits` replay includes. Quit Game exits 0 about 2 s after
`Return`, nothing is left in the sandbox, and keeping the disk took 0.17 s (a snapshot).

Back on the menu after `c` on analytics, `Left` and `Down` × 4 ended on My Rewards in this
session, not Quit Game; `Left` went to the Phantom Liberty banner and `Up` from there to Quit
Game, and screenshots caught that. After a relaunch the focus starts on Settings: `Down`,
`Down`.

The export's proof is a fresh `run` of it, the installer build and all, without
`--reuse-install`: passed in 13 min 44 s, of which 8 min 39 s is the build, every check ok: the
agreement, Quit Game's exit 0 after 1.5 s, `nothing_left_relaunch_1`, the window 4.5 s after
the relaunch, then the menu on the remembered disk, and Quit Game again. It stays a draft:
`e2e/1423049311.toml` goes on through every Graphics page and Video for #232, and a relaunch
would add nothing there.

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
| `gpu_used_by_game` (`[gpu] wayland = "unused"`, Wayland mode, no `--expect-gpu`) | the game holds no GPU node: it draws through GDI into shm buffers, as declared (see [Manifests](#manifests)) |

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

## Wayland mode

`--display wayland` on `run` or `live start`, or `display = "wayland"` in a manifest's
`[window]` (#264), runs the game phase with no X server at all. It's the path Wine's Wayland
driver takes on a real desktop, and the one HDR needs (Cyberpunk 2077, #232). The default
stays Xwayland, and the build phase always has Xwayland: it runs an installer, not the game.

- **Sway starts with `xwayland disable`,** and the game's environment has no `DISPLAY`. So a
  process that wants X fails, instead of passing quietly on an Xwayland nobody meant to test.
  The alternative was to keep Xwayland up and catch a stray X client after the fact; that lets
  a game half-work on X and still reach its menu, which is exactly what this mode is there to
  rule out. `no_x_fallback` then names the log line that says what wanted X.
- **A gogo game gets gogo's own Wayland mode**, `gogo linux-app display <gogid> wayland`
  (#292), written into the run's scratch override file before the launch like the rest of
  `[override]` (a manifest's `[override] display` wins). The harness sets no
  `PROTON_ENABLE_WAYLAND` itself: the launcher checks for sway's socket and sets it, and
  `gogo_display` fails when the run log's `# display:` header doesn't say `wayland`. With no
  mode the game would try X, which `no_x_fallback` and the missing window catch. Nothing is
  written to the installed base or any installed games. GE-Proton11-12 (gogo's pin)
  ships `winewayland.drv` for both arches, and its `proton` script turns the switch into
  `WINE_GRAPHICS_DRIVER=wayland`, `WINE_USE_EGL=1`, `WINE_DISABLE_FULLSCREEN_HACK=1` and
  `PROTON_USE_XALIA=0`, only when `WAYLAND_DISPLAY` is set. It guesses the primary monitor with
  `xrandr`, which prints `Error: failed to run xrandr` with no X server: noise, and not matched
  by `no_x_fallback`. `PROTON_WAYLAND_MONITOR` names one instead.
- **The checks** are `isolation_display` (only our sway serves a Wayland socket, at the
  manifest's size), `isolation_no_x` before the launch, and `wayland_client` once the window is
  up: an `xdg_shell` window in sway's tree, and every process the launch started is connected
  to our sway's socket and to no X server. That comes from `ss -xapn` in the sandbox's network
  namespace, by the peer of each socket, so it holds inside pressure-vessel's container too,
  where the socket has another path. `summary.json`'s `display` says the mode, the socket,
  what gogo's launcher picked (`gogo`, its `# display:` line), and what each window's
  processes were connected to.
- **Window discovery, screenshots, `fills_window`, focus and the close** never used X: sway's
  tree, `grim` on the output, and the launcher's own close request through the wineserver.
- **`gpu_gl` asks EGL's surfaceless platform** (`eglinfo`), since there's no GLX.
  `gpu.egl_wayland` records EGL on the Wayland socket beside it: on NVIDIA it's
  `eglInitialize failed`, because egl-wayland wants the DRM device sway names, and the sandbox
  only has the render node. Vulkan games don't go through it (Cyberpunk's VKD3D-Proton renders
  on the 4090 either way); an OpenGL game on Wine's Wayland driver (`WINE_USE_EGL=1`) on
  NVIDIA would, and is expected to fail here (not tried).

**Input.** The virtual keyboard and pointer are Wayland devices already, so nothing changed.
The self-test's client (`scripts/linux-e2e-wayland-client.py`, Wayland-native, no toolkit) asks
for a pointer lock and relative motion after its click, as a game that steers by the mouse
does: headless sway grants the lock (`zwp_pointer_constraints_v1`), and `move_by` arrives as
`zwp_relative_pointer_v1` motion under it, unscaled. `self-test` runs it
(`e2e/self-test-wayland.toml`: click, lock, motion, key, exit), and an xterm in Wayland mode,
which must fail with `no_x_fallback` naming "Can't open display". In Cyberpunk 2077's menu on Wine's
Wayland driver, `move_by` moved the game's own cursor by about as many pixels, and a `click`
after steering it (pin it in a corner with a big `move_by` first) opened the item under it.
`move_to` is a poor fit for such a game: sway turns it into a delta from where its own pointer
is, which a locked or hidden pointer doesn't track, and the game's cursor jumped off the screen.
What a real compositor does with confinement across monitors, and the cursor image, are the
host pass's.

**Proven on** (2026-11-09, RTX 4090, `--gpu renderD129 --expect-gpu renderD129
--reuse-install`): `e2e/1423049311.toml` unchanged, with `--display wayland`. Every check
passed: title, agreement, menu, Settings and its Graphics pages (DLSS Super Resolution, Frame
Generation and Ray Tracing offered), Quit, the relaunch on the same disk, and Quit again.
`winewayland.drv` loaded, `winex11.drv` never did, and DLSS's DLLs and Streamline loaded as on
X. Wine logs that sway lacks `zwp_text_input_manager_v3` and `xdg_toplevel_icon_manager_v1`:
input methods and window icons, nothing a run needs.

**Not covered here,** and a host pass (#179): HDR (a headless output has no colour
management and no HDR metadata, so `DXVK_HDR` and Wine's HDR reporting have nothing to find),
VRR, fractional scaling, several monitors, the cursor image, and Hyprland itself. gogo's own
per-game switch (#292) is what this mode drives; its HDR sub-option (`[override] hdr`) is
written and shows in the header, but there is nothing headless for it to find. `[override] monitor`
(#295) is written the same way (`HEADLESS-1` is the only output there): the header shows it, and
which of several monitors the game opens on is a host pass.

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
display = "wayland"              # "xwayland" (default) or "wayland": Wayland mode, below

[gpu]
wayland = "unused"               # draws without the GPU on Wine's Wayland driver (GDI): below

[env]                            # only for the game's launch: logging, for example
DXVK_LOG_LEVEL = "info"

[override]                       # games only: into the game's override file, through gogo
extra_args = ["-skipStartScreen"] #   (`linux-app args <gogid> -- …`), before the launch
wrapper = ["gamemoderun"]        #   (`linux-app wrapper <gogid> -- …`; #271)
display = "wayland"              #   (`linux-app display <gogid> wayland`; #292; Wayland mode
hdr = "force"                    #   sets display itself), `--hdr force`
monitor = "HEADLESS-1"           #   `--monitor HEADLESS-1` (#295; Wayland mode only)
[override.env]                   #   (`linux-app env set <gogid> K=V…`)
PROTON_DLSS_INDICATOR = "1"

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
move_to = { x = 43, y = 71 }     # the pointer there, percent of the window, no click
[[step]]
click_here = true                # press and release, without moving
[[step]]
scroll = 250                     # wheel notches where the pointer is; negative is up
[[step]]
wait = 5                         # seconds
[[step]]
screenshot = "preferences"
min_fill = false                 # this frame needn't fill the window
[[step]]
expect_change = { from = "menu", to = "preferences", min = 20 }  # % of pixels
[[step]]
move_relaunch = "library"        # after the game quit: `linux-app move`, then launch it again
[[step]]
key = "Escape"                   # one key, held 50 ms: an XKB keysym name, as below
[[step]]
type = "Hello!"                  # text, a key at a time, Shift where the us layout needs it
[[step]]
hold = { key = "w", ms = 500 }   # one key, held down 500 ms (up to 10000)
[[step]]
press_until = { key = "space", change = 20, every = 2, max = 60 }  # a key until the screen says it took ([below](#press_until))
[[step]]
press_until = { key = "Down", every = 1, max = 8, marked = { x = 9, y = 57.5, w = 22, h = 7, color = "#f0f040", tol = 40, min = 1 } }
[[step]]
wait_until = { every = 1, max = 120, marked = { x = 78.6, y = 85.1, w = 14.9, h = 3, color = "#e85048", tol = 50, min = 10 } }
[[step]]
relaunch = true                  # the game has quit by itself: launch it again, same sandbox, same disk
```

`[env]` is the environment `launch.sh` starts with, so `launch.conf`'s own `ENV` beats it.
`[override]` is what a player sets (#234, #271): the harness runs `gogo linux-app env set`,
`gogo linux-app args` and `gogo linux-app wrapper` with the run's scratch `GOGO_HOME` right
before the launch (check
`override`), so `launch.sh` sees a stale stamp and runs setup in the sandbox, and the
game gets them through `launch.conf`. The installed base a `--reuse-install` run copies is
never touched. A [live session](#live-sessions) sets them with `live start --env` and `--arg`.
A wrapper's program has to be in the sandbox: the harness's tools (`env` is one) or a store
path. A store path is how to try a tool the harness doesn't carry without changing every base's
key: `nix build --inputs-from . nixpkgs#gamescope`, the flake's pin, and that path in a manifest
under `target/`. Real gamescope runs here only as
`["env", "SDL_VIDEODRIVER=x11", "<store path>/bin/gamescope", "--backend", "sdl", …, "--"]`, on
`--gpu`: the sandbox has render nodes only, so `--backend wayland` (no Wayland surface for SDL)
and `--backend headless` (`physical device has no primary node`) both fail to start.
`e2e/wrapper-1741027632.toml` is the trivial case, with `env` as the wrapper.

`key` and `type` go through one virtual keyboard with a standard `pc105`/`us` keymap and
real evdev keycodes, so a game sees what a USB keyboard would send. `key` takes the level-1
(unshifted) keysym names of that layout: `Escape`, `Return`, `space`, `Tab`, `BackSpace`,
`a`-`z`, `0`-`9`, `F1`-`F12`, `Up`/`Down`/`Left`/`Right`, `Home`, `End`, `Prior`/`Next` (Page
Up/Down), `Insert`, `Delete`, `Shift_L`, `Control_L`, `Alt_L` and the like (`KEYS` in the
helper). `type` takes printable ASCII, `\n` and `\t`. The manifest check refuses anything else.

Prefer `wait_settled` to a long `wait` before the first click: the same intro takes a
different time on another renderer, and a click meant for the menu lands on the video. Its
picture must fill the window and hold still (under 1% of pixels changing across 2 s), so an
animated menu needs a plain `wait`.

A game that draws its own cursor from relative DirectInput (Carmageddon TDR 2000's menu) can't
be aimed with `click`: while Wine holds the exclusive grab, the compositor's pointer stays put
and only the deltas count. Steer it instead: a big `move_by` pins the cursor in a corner, where
the game clamps it, a second one goes to the target, and `click_here` presses there. A unit of
`move_by` isn't a pixel, since the game and Wine scale it, so measure it from a screenshot.

A game that clips the cursor while hiding the system one (any game under gogo's cnc-ddraw, which
clips to its window) gets a pointer lock on Wine's Wayland driver, and `click` misses it the same
way. Heroes of Might and Magic 3 (`e2e/1207658787.toml`) takes `move_by` 1:1 in window pixels,
but the pin lands on the window's left edge on one launch and on the 4:3 picture's (x 107 of
1280) on another. So aim only at a button wider than the pillarbox, and prefer the game's own
keys.

A game that draws through GDI (gogo's cnc-ddraw is pinned to `renderer=gdi`) never opens the GPU
on Wine's Wayland driver: its frames go to the compositor in shared-memory buffers. On Xwayland
Wine's X11 driver opens the node anyway. So such a manifest says `[gpu] wayland = "unused"`: in
Wayland mode, with `--gpu` and no `--expect-gpu`, `gpu_used_by_game` (and
`gpu_used_by_game_launch_<n>`) then wants the game to hold **no** GPU node, and fails if it holds
one, since that means it draws another way now. Xwayland mode and `--expect-gpu` are unchanged
(Heroes 3, 2026-11-10).

To write one: run it with `--keep` or a first screenshot, read the coordinates off the
PNG, and divide by the resolution. Look at the screenshots yourself: the automatic checks
say "a picture fills the window and it changed", not "it's the right picture".

### `press_until`

A key after a fixed `wait` is a bet on how long the game takes, and a key on a menu is a bet on
where its focus is. `press_until` presses until the screen says the key took, up to `max`
presses, `every` seconds apart. Both are required: no limit is guessed. It's in the step runner,
so a `run`, a live session (`live <id> press_until`), `--from-manifest` and a restore's replay
all do the same thing. Exactly one of two conditions:

| | Passes when | For |
|---|---|---|
| `change = PCT` | the frame after a press differs by that % from the one right before it | a screen that holds still until the key takes. A game's own motion counts as a change, so a video or a scene cut passes it falsely: see `marked` |
| `marked = { x, y, w, h, color, tol, min }` | at least `min`% of the region (% of the window) is within `tol` (default 40, per channel) of `color`, in 2 frames running. Looked at before the first press too: a key that moves a focus would move it off | a screen that moves, and "the focus is on this item": a colour nothing else has there. A colour says it with no game art in git |

- **Running out fails, and stops.** The check `press_until_<n>_<key>` fails with the presses, the
  last %, and the last frame (`shots/press-until-<n>-failed.png`), and a `run`'s steps end there,
  since the ones after it stand on it: a `Return` that would open the wrong menu item is never
  pressed. A replay stops there as at a frame that doesn't match, and `--from-manifest` as at a
  failed check.
- **The export is honest.** One that passed is on the path, and exports, as given. One that ran
  out answers `ok: false` and exit 1, and the session goes on; its keys did reach the game, so the
  path has them as the plain `key`s they were. `restore` drops them.
- **Never use it with a key that confirms or selects.** The key is pressed until the screen shows
  it took, so a slow screen gets another, and that one lands on whatever came up next. So
  `press_until` is for keys that only move or skip, and only where you've seen an extra press do
  nothing. For `Return`, or a prompt's answer key, press once with a plain `key`, then
  `wait_until` what it brings up: if that never comes, the run fails and nothing was pressed twice.
- `summary.json`'s step has `presses`, `secs` and `seen_pct`.
- **`wait_until`** is the same with no key: `marked` only, `max` looks `every` seconds apart, and
  running out fails and stops the same (`wait_until_<n>`). For a screen that moves, where
  `wait_settled` can't say when it's there.

### The automatic checks

| Check | Fails when | Would have caught |
|---|---|---|
| `override` | with `[override]`: a `gogo linux-app env set` / `args` / `wrapper` call failed (the game isn't launched) | a refused key, a gogo without #234 |
| `window` | no window in `wait_secs`, or the game exited first. On a timeout, `result.json`'s `window_diagnosis` says what each side saw: sway's windows (pid, shell, visible, size), whether sway and Xwayland are up, the X server's top-level windows and their map state, and the processes the launch started. An X window that is mapped with no sway window for it points at the compositor, not the game (#309: the harness's sway carries a wlroots patch for one such race) | DDrawCompat's exit 9 in #199 |
| `wayland_client` (`wayland_client_relaunch_<n>`) | Wayland mode, at the window: it isn't an xdg-shell window, or what the launch started isn't connected to our sway's socket, or is connected to an X server | a game that came up through a display it shouldn't have |
| `gogo_display` | Wayland mode, a gogo game, at the end: the launcher's run log has no `# display: wayland …` header, so gogo didn't put the game on Wine's Wayland driver itself (#292) | the harness testing its own switch instead of gogo's |
| `no_x_fallback` | Wayland mode, at the end: a line in `game.out` or the launcher's run log says a process wanted X (Wine's X11 driver, "can't open display" and the like), and names it | a launcher or game that silently needs X |
| `settled` | `wait_settled` timed out | a game stuck in a video |
| `not_black_<shot>` | under 1% of the window's pixels are above level 16 | a black screen |
| `fills_window_<shot>` | the non-black bounding box is under `min_fill` | 800×600 in the top-left of a window: 62% × 75% |
| `changed_<a>_<b>` | under `min`% of pixels changed by more than 16 | a click that didn't land |
| `press_until_<n>_<key>`, `wait_until_<n>` | the `n`th of them ran out, or the game exited first. The steps end there | a key pressed into an intro, a focus that isn't where the route needs it |
| `clean_exit` | `how = "self"`: still up after `wait_secs`, or a non-zero exit | a game that hangs on exit |
| `torn_down` | `how = "signal"`: the launcher doesn't end on `TERM` | a stuck teardown |
| `doctor_links`, `doctor_processes` | doctor's lines aren't `ok` after the run | host drive links (#199), a stray Wine process |
| `finished` | the driver hit an error, or never got to the end | |
| `live_idle` | a live session: no command for `--idle-secs` | an agent that went away |
| `clean_exit_relaunch_<n>`, `nothing_left_relaunch_<n>`, `window_relaunch_<n>`, `gpu_used_by_game_launch_<n>` | a `relaunch` step: [above](#a-clean-stop-a-kept-disk-and-a-relaunch) | a Quit that hangs, a Wine process that stays and holds the prefix |
| `nothing_left`, `scratch_deleted` | as above | |

## From a pass to a compat entry

A pass prints `tested_entry`. Add it to the game's L3 file in `compat/<gogid>.toml`:

```toml
[platforms.linux]
status = "boots"
notes = "headless sway: menu, Preferences and Exit work; audio and a real compositor not judged"
tested = [
  { runtime = "ge-proton/GE-Proton11-12", host = "headless-sway/software", date = 2026-11-06 },
  { runtime = "ge-proton/GE-Proton11-12", host = "headless-sway/nvidia", date = 2026-11-06 },
]
```

`headless-sway/<gpu>` says what it is: not the owner's desktop. A status above `boots`
needs someone to have played it.

## What it doesn't cover

These stay in a host pass (#179, design 18 §11.2), and agents put the steps in the PR:

- **Compositor quirks**, such as the picture in the top-left of a tiled window on Hyprland
  (#199): that never reproduced on sway, so a pass here doesn't rule it out.
- **Real display modes and VRR.** A headless output has one mode and no refresh to sync to.
- **Audio you can hear.** The sandbox's device is a null sink: it takes the stream and drops
  it. `audio.game_streams` in the summary names the streams the game opened, which says it found
  a device, not that it sounds right.
- **The menu entry and its icon** on a real desktop. The harness runs `launch.sh` directly,
  and writes its `.desktop` file into scratch.
- **The early-exit dialog.** It's off here, and GNOME and KDE aren't anything the maintainer runs.
- **HDR and the rest of a real Wayland desktop.** [Wayland mode](#wayland-mode) proves the
  game runs on Wine's Wayland driver with no X server; a headless output has no HDR, no
  colour management and one scale.

## How it was found to work, and what to watch for

- **Software rendering has only Mesa's EGL vendor** (`__EGL_VENDOR_LIBRARY_FILENAMES`), in
  the build's display and a software game phase. Xwayland's GLX probe goes through glvnd to every
  vendor, and on a host with nvidia 595 its `libnvidia-egl-gbm` segfaults Xwayland when
  there's no device: `no X server on ':0'`, with `eGbmGetPlatformDisplayExport` in `sway.log`.
- **An agent's shell is itself a bwrap** (the desktop app's FHS env), with `/etc/passwd` a
  link into `/.host-etc`. Xwayland admits clients by local user, so an unresolvable uid
  gives `Authorization required`: the sandbox makes its own `passwd` and `group`.
- **A headless seat has no pointer**, and then `seat cursor press` reaches nobody. The driver
  holds a wlr virtual pointer for the whole run, over a small Wayland client in the helper.
- **Motion goes through that pointer, not `seat cursor set`.** That command is a warp with no
  delta, so Xwayland reports it as absolute raw motion, and Wine passes the 0-65535 value on,
  scaled, as a delta (+21611 in TDR 2000's DirectInput, which threw its cursor into a corner).
  The clicks were landing all along, just not where they aimed. A real mouse never does this.
- **The keyboard is one virtual keyboard for the whole run,** like the pointer, with a
  `pc105`/`us` keymap from the flake's `xkbcli compile-keymap` and real evdev keycodes
  (`zwp_virtual_keyboard_v1`). Before the first step the driver checks its keycode table
  against that keymap, and a mismatch fails the run. It replaced `wtype` (#236), which made
  a new keyboard per call, with a keymap of its own holding just that call's keysyms from
  keycode 1 up. So every key came on keycode 1, X keycode 9: Escape's place on a real
  keyboard. Xwayland's keysyms were right, which was enough for xterm, but a game that
  reads scan codes through Wine got Escape for every key: TDR 2000's "Return" on its menu
  was an Escape, and Cyberpunk 2077's title screen never saw a space. A key is held 50 ms,
  since a game that polls its input misses an instant press and release. wlroots doesn't
  work out modifiers from a virtual keyboard's keys, so `type` sends Shift's state itself.
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

### A route that doesn't race: Cyberpunk 2077

`e2e/1423049311.toml` used to press `space` a fixed time after the window, and walk the menu by a
count of keys (#266, #262). Now a colour says what's on screen ([`press_until`](#press_until)).

**The intro** (the 4090, `-skipStartScreen`). Black for 9 s, warnings on black to 31 s, then a video
with the prompt, ending in the agreement by itself about 90 s after the window. Half a second apart,
70% of the video's pixels differ, and a scene cut is as big as a key taking, so `change` can't
tell. The prompt's region was 19-26% near its colour in every frame of the video sampled, and 0.0 in
every other. So `wait_until` the prompt, a `title` screenshot, then `press_until space` until the
agreement's dark panel is there.

**The focus.** Left's and Down's traps are why a count of keys went wrong: Down ends swapping My
Rewards and the version box, and Left swaps My Rewards and the banner. The manifest's comment has
the map. Down until My Rewards is framed works from anywhere, Left goes to the banner, and Up climbs
to the item, framed. A row with the focus is 3.3-4.6% near its yellow, and one without 0.0.

**What's pressed more than once.** `Return` on the agreement and `c` on the analytics prompt act, so
each is pressed once, and `wait_until` sees what they bring up: a second `Return` could land on the
prompt, which is always declined. `Down` and `Up` only move the focus. An extra `space` was tried on
each screen it could reach (the agreement, scrolled or not, the prompt, the menu with no focus shown
and with one on Credits), and did nothing.

| Proof | |
|---|---|
| The route to the agreement, 10 restores in a row (`--from-manifest --to-shot agreement`) | 10 of 10, no retries: 38-39 s replay, the prompt after 31-32 looks, `space` taking at its first press. So nothing here shows what the 2 of 9 in #257 were, but a late key now gets pressed again, and running out says so |
| The route past the agreement, Accept and analytics pressed once, 10 restores in a row (`--to-shot menu`) | 10 of 10, no retries, 77-79 s replay. The agreement's route is the row above's, unchanged, so this re-proves it too |
| The route to Quit Game from every place the focus can be (the unmarked one after a dialog, New Game, Load Game, Settings, Credits, Quit Game, the banner, My Rewards, the version box), no `Return` | 9 of 9, 0-6 presses of Down and 1 of Up, Quit Game framed at 4.5% each time |
| A fresh `run` of the manifest, the installer build and all, without `--reuse-install` | passed in 12 min 25 s, of which 8 min 31 s is the build: all 92 checks ok, on the fresh disk (title, agreement, analytics, Settings, every Graphics page, Video, Quit Game framed, exit 0) and on the remembered one after the relaunch (the menu, Quit Game framed, exit 0). The game phase took about 4 min |

**Where a restore goes, and what was cut** (#257; the same RTX 4090 and options, 2026-11-08).
`last_restore.phases`, and `last_replay.timeline`, on the same route as above, before and after.
The "18 s of sandbox" of the table above wasn't the sandbox's start:

| | before | after |
|---|---|---|
| closing the game | 0.8 s | 0.8 s |
| the sandbox's exit (`leave`) | 0.2 s | 0.2 s |
| the **sweep** between lives: nothing found, by a `grep` per process and mark, some 8,000 forks over a desktop's 580 processes | **7.4 s** | **0.05 s**, one process, the same marks |
| the rollback (a snapshot) | 0.2 s | 0.2 s |
| bwrap and the driver's start | 0.05 s | 0.05 s |
| sway, Xwayland, the virtual keyboard and pointer | 0.2 s | 0.2 s |
| the sound server, the isolation checks | 0.1 s | 0.1 s |
| `glxinfo`, `vulkaninfo` | 0.75 s | 0.75 s |
| the override, on the launch's disk only | 1.7 s | 1.7 s |
| **doctor**, before the launch: it reads Cyberpunk's PE files three times (#265) | **7.4 s** | **0 s** once the disk has its report ([below](#a-fast-replay)), 7–10 s the first time |
| the test sound stream: 2 s on a null sink, whatever its length | **2.0 s** | **1.3 s**, now beside the probes, 0 beside anything longer |
| launch to the window | 4.5–5 s (launch's disk 7 s) | the same |

So sway and the input devices, the first idea for reusing something across lives, cost 0.2 s, and
a life keeps its own. The replay's waits and frames, the rest:

| replay step | before | after |
|---|---|---|
| a screenshot | 3.5–5 s | 0.7 s |
| `wait 25` after `space`, to the agreement | 25 s | 4.5 s |
| `wait 15` after accepting, to analytics | 15 s | 2.8 s |
| `wait 10` after `space`, to the remembered disk's menu | 10 s | 4.5 s |
| a 9–10 s gap, then a checkpoint's own screenshot | 14 s | 2.2 s |
| `wait 30` and `wait 34.9` before `space` (the intro, no frame to go by) | as written | as written |
| `scroll 750` | 30 s | 30 s |

| `restore` to `ready` | before | after |
|---|---|---|
| to a clean stop, its disk's first restore | 23.8 s | 17.5 s |
| to a clean stop, again | 24.2 s | **8.2 s** |
| 5 steps past a clean stop: the menu of the remembered disk | 72.8–75.3 s | **44.4–44.7 s** (first on a disk: about 50 s) |
| a running checkpoint, 41 steps from the launch | 216.5 s | **130.4–132.4 s** (43.7 s cut) |

What's left of a restore to the menu is the game's: 30 s of intro before `space` takes, with no
frame to know it by, and 4.5 s to the window. Across 9 restores the
`agreed` replay stopped at its first screenshot twice, with nothing cut, on the intro still
playing: `space` at 34.9 s hadn't taken (the race #241 found, on a route where
`wait` is all there is before that key). With doctor put back in those lives, 3 of 3 passed; 4 of 6 without it.
Too few to tell apart, and doctor isn't near the key, so it's filed as the route's race (#266), not this cut's.
The caches were left alone: nothing above needed them.

The proof that the replay's cuts didn't reach the export: the route of that session, taken
through fast restores (`restore agreed` at 131 s, a clean-stop checkpoint, `restore on-menu2`), with
`wait 25`, `wait 15` and `wait 10` still whole in its export. A fresh `run` of it, the installer
build and all, without `--reuse-install`, passed in 13 min 20 s, of which 8 min 36 s is the build,
all 68 checks ok, Quit Game's exit 0 on both launches, the window 4.5 s after the relaunch,
`nothing_left_relaunch_1`. `--reuse-install`'s base key didn't change: no new base was built.
