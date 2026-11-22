# 23 — A Galaxy service for Linux builds: Comet (#356)

Status: **accepted** (2026-11-21; decisions 1–12 accepted as recommended). Built: the pin
(#487, §1.1), the start/stop around a launch (#488, §1.2) and the switch with its detection and
reporting (#489, §1.3); the rest is in §5. The facts behind it are in
[docs/23](../23-galaxy-sdk-and-comet.md).

A `gogo linux-app` game can run [Comet](https://github.com/imLinguin/comet) beside it, so
the game's GOG Galaxy SDK gets a signed-in user, achievements, stats and leaderboards, and
games that sign in to their publisher's service through it (Cyberpunk 2077's
cross-progression saves) can. gogo isn't a launcher a game is started from: the per-game
`launch.sh` (design 18 §7) starts and stops Comet, with no gogo process staying behind.

## 1. Decisions

1. **The cause is settled** (host pass H3, #486; docs/23 §4): on a Heroic install of
   Cyberpunk 2077, turning off only Comet brings back the ~30 s start-screen wait and hides
   the cross-progression saves, so the cause is the missing Galaxy service. Whether the wait
   is an online or a local timeout stays open and no longer changes the fix. The feature is
   built for sign-in, achievements and cross-progression.
2. **GOG cloud saves stay gogo's** (design 22, #464). Comet has no cloud-save code, so
   there is nothing to arbitrate; only the order around a launch (§2.3).
3. **Opt-in per game, off by default.** `gogo linux-app galaxy <gogid> on|off` writes
   `GALAXY` into `launch.conf`. A compat key `[platforms.linux] galaxy = "comet"`
   recommends it for a game (Cyberpunk 2077 since the 2026-11-22 host pass, #507); the config default is
   `[linux] galaxy_default = "off"|"on"|"ask"` (`off`). Revisit on-by-default for games
   that ship the SDK after a few host passes: per-game breakage (Comet #80) and games that
   keep saves per Galaxy user (Ghostrunner) argue against it today.
4. **The binary is pinned like umu** (design 18 §2.1). A catalog `Kind::Tool`
   `comet/0.4.1` (`comet-x86_64-unknown-linux-gnu`, GitHub's sha256 digest), fetched after
   consent; the plan line names its size, its Apache-2.0 licence and the ~100 MiB GalaxyPeer
   redist Comet fetches on its first start; the notice goes to `NOTICES.txt`. On NixOS the
   glibc binary doesn't start, so the flake builds Comet from source at the pinned tag and
   `gogo-linux-app` sets `GOGO_COMET=<store path>`, as it sets `GOGO_UMU_PYTHON`.
   `[linux] comet = "fetch"|"system"|"path:<file>"`. v0.4.1 rather than Heroic's v0.2.0:
   it is current, and the host pass checks it with Cyberpunk.
5. **Credentials never go through argv** (`/proc/<pid>/cmdline` is readable by every local
   user, and the refresh token is the account). gogo writes a three-field Heroic-shaped
   `auth.json` (access token, refresh token, user id), mode 0600, in a fresh 0700 dir under
   `$XDG_RUNTIME_DIR` (per-user tmpfs), starts Comet with `--from-heroic` and
   `XDG_CONFIG_PATH=<that dir>`, and deletes the file once the port accepts (Comet reads it
   before binding). No `$XDG_RUNTIME_DIR`: Comet is skipped, never a disk-backed file. The
   contract is a Comet quirk, so a unit test ties it to the pinned version; an upstream
   fd/stdin option (draft report in #494) replaces it when there is one.
   The username is the one value that does go on argv: Comet v0.4.1 takes it only as the
   required `--username` argument (no environment variable, and its Heroic import reads just
   the tokens and the user id). It is the GOG display name, not a credential; local users can
   read it from `ps` or `/proc/<pid>/cmdline` while Comet runs. Comet sends it to the game as
   the player's name. gogo moves it off argv once Comet accepts it another way (#510).
6. **`launch.sh` starts and stops it** (§2).
7. **Comet's output is never logged.** It logs game client secrets at its default level and
   token-bearing URLs at `warn` (docs/23 §2.1). The run log carries gogo's `# galaxy:` lines
   only. No debug switch in v1; one added later writes to a 0600 file kept out of
   `report.txt` and Copy report.
8. **Nothing blocks a launch** (§2.4).
9. **Reporting:** `apps show` and `doctor` (§3); the L1 scan records whether the game ships
   `Galaxy.dll` / `Galaxy64.dll`.
10. **Upstream:** @noosxe files the report on Comet (#494); agents open nothing there.
11. **Later:** the dummy `GalaxyCommunication` service for games that need it (#492), the
    Mac launcher (#493), the overlay, `comet preload` for offline achievements, native
    Linux builds that ship the SDK (#162).
12. **Tests:** a fake `comet` in the launcher tests; the real pinned Comet offline in e2e
    (#490, after @noosxe's OK for the download).

### 1.1 As built: the pin (#487)

- **Catalog.** `comet/0.4.1` is a `Kind::Tool` (payload `comet-x86_64-unknown-linux-gnu`, no
  `runtimes`), `os = ["linux"]`, `arch` x86_64, pinned to `868dd0b5…d3a5a` (GitHub's asset
  digest from the releases API, 14,660,192 B). **Not yet checked against a download** (umu's
  was); the real-binary e2e (#490) does that. The release asset is a bare file, stored as
  `runtimes/comet/0.4.1/comet-x86_64-unknown-linux-gnu`, without the execute bit. The notice
  (Apache-2.0, run as a separate program) goes to the store's `NOTICES.txt`.
- **Code.** `gogo_core::linuxapp::comet`: `resolve()` finds a Comet without downloading and
  `provision()` downloads the pin when `fetch` finds nothing. Resolving a store copy sets
  mode 0755 (`store::unpack` is unchanged for everyone). Errors are `MacAppError::Comet`
  (exit 7). `plan_line()` is the consent text (size, Apache-2.0, the ~100 MiB GalaxyPeer
  redist Comet fetches itself on first start).
- **Config.** `[linux] comet = "fetch" | "system" | "path:<file>"`, unset meaning `fetch`.
  `fetch`: `GOGO_COMET` first (it wins over the store, so no download on NixOS), then the
  store copy. On NixOS without `GOGO_COMET` it is refused with the two ways out, as for umu.
  `system`: the first executable `comet` on `PATH` (absolute entries only). `path:`: an
  absolute executable file.
- **Consent.** The only download in this change is `gogo runtimes install comet`, an
  explicit command. Nothing in `linux-app` asks or fetches yet: that is #489's `galaxy on`,
  which calls `plan_line()`.
- **`runtimes list`** shows the catalog row, a footnote, and a `comet (...)` line with the
  mode and the binary it resolves to.
- **Flake.** `nix/comet.nix` builds the tag with `rustPlatform.buildRustPackage` on the
  repo's pinned toolchain; `packages.x86_64-linux.comet` is that build and
  `gogo-linux-app` sets `GOGO_COMET` to it with `--set-default`. What the build needed:
  - the `proto/` directory is a git submodule (Yepoleb/gog_protocols, CC0-1.0), so the
    source is fetched with `fetchSubmodules = true`;
  - `protobuf` (protoc) with `PROTOC` set: without it `build.rs` runs the prebuilt protoc
    of the `protoc-bin-vendored` crate, which can't run in the sandbox;
  - nixpkgs' cargo hook forces system libraries, so `aws-lc` (rustls) and `sqlite`
    (sqlx) are build inputs, with `pkg-config`. No openssl, no cmake;
  - Comet's own `Cargo.lock` has no git dependencies, so a plain `cargoHash` works; the
    upstream test suite is not run.
- **Contract test (decision 5).** The pin's version must equal `CONTRACT_CHECKED_AGAINST`
  in `comet.rs`, and `nix/comet.nix` must build that same version, so a bump forces a
  re-read. Read at v0.4.1 (source only): `--from-heroic` reads
  `$XDG_CONFIG_PATH/heroic/gog_store/auth.json`, falling back to `~/.config` and then
  Flatpak Heroic's path under `$HOME` (`HOME` must be set or Comet panics). **The JSON is an
  object keyed by Comet's Galaxy client id, whose value holds `access_token`,
  `refresh_token` and `user_id`**: decision 5's "three-field `auth.json`" means that nested
  shape, which #488 must write.

### 1.2 As built: start and stop (#488)

- **Gate.** `launch.sh` reads `GALAXY` from `launch.conf` (default empty) and acts only on
  `GALAXY=1`. Nothing writes it yet: #489's `galaxy on` adds it to `LaunchConf`, so every
  existing build behaves as before. Not on one-offs.
- **`galaxy-start`** (`gogo_core::linuxapp::galaxy`, hidden `linux-app galaxy-start <gogid>
  --launcher`). Tracing is never initialised for it, whatever `RUST_LOG` or `-v` say. It
  sets `RLIMIT_CORE` to 0, which exec keeps, so Comet (holding the tokens for the whole
  game) dumps no core either; it also makes itself non-dumpable (`PR_SET_DUMPABLE`), which
  exec resets, so that covers gogo only, not Comet. Order: the
  runtime dir check; the port (already listening: `reusing`, nothing else touched); the
  binary (`comet::available`, never a download); `HOME` (Comet panics without it); the
  session (restore, refresh ≤ 2.5 s, persist; a failed or slow refresh goes on with the
  stored tokens and says so); the username (≤ 1.5 s from GOG's user data, cached in
  `<data root>/comet/username`, 0600; offline with no cache: the placeholder `gogo`); the
  file; the spawn; the wait. One 5 s budget covers all of it; `launch.sh` adds `timeout -k 1
  8` as a backstop.
- **The port is read, never connected to.** Comet's `--quit` counts clients, so a probe
  connection would start its idle clock before any game connects. "Listening" is a LISTEN
  row for the port on 127.0.0.1 or the any-address in `/proc/net/tcp{,6}`. The holder is
  named from `.comet` (gogo's own) or `ss -ltnp`.
- **The file.** `$XDG_RUNTIME_DIR` must be absolute, a real dir (not a symlink), ours, with
  no group/other bits; `$XDG_RUNTIME_DIR/gogo` is made (or narrowed to) 0700. The file is
  `gogo/auth-<pid>-<start time>-<nonce>/heroic/gog_store/auth.json` (gogo's pid and its
  start time from `/proc/<pid>/stat`): dirs 0700 made without `-p` reuse,
  the file `create_new` with 0600. It is removed (whole dir) once the port listens, when
  Comet exits or the budget runs out (Comet is then killed), on INT/TERM/HUP, and on drop;
  each start also sweeps `auth-*` dirs whose pid isn't the process that started at that
  time (a SIGKILLed run, even once its pid is reused).
- **Comet's process.** A cleared environment plus an allowlist (`HOME`, `PATH`, locale, `TZ`,
  CA-bundle variables, `XDG_RUNTIME_DIR`), so no `WINEPREFIX` or `STEAM_COMPAT_DATA_PATH`
  makes the launcher count Comet as the game's; then the `XDG_*` and `COMET_LOG=off` of
  §2.1. It gets its own process group (`process_group(0)`, not `setsid()`: no `unsafe`, and
  with stdin on `/dev/null` a group of its own is enough to miss Ctrl-C and a terminal's
  hangup).
- **Errors** are matched by variant into fixed reasons, never formatted: gogo's auth errors
  can hold the refresh URL (#501).
- **The registry** is `$XDG_RUNTIME_DIR/gogo/comet/` rather than `<data root>/comet/`: one
  port per host, so one registry per user whatever the data root, wiped at logout. It holds
  `.lock` (util-linux `flock`, fd 9, never passed to gogo or Comet; no `flock`: unlocked,
  logged), `.users/<launcher pid>` (its start time, written for `started` and `reusing`)
  and `.comet` (pid and start time of the Comet gogo started, written by `galaxy-start`
  itself: the launcher never takes a pid from gogo's line, which can carry a foreign
  process name from `ss`). Before making anything there, `launch.sh` checks
  `$XDG_RUNTIME_DIR` and each dir as `galaxy-start` does (a real dir, ours, no group or
  other bits; the lock not a link), else `skipped:`. The last launcher out stops
  that Comet, whoever started it: the starter quitting first
  would otherwise leave it to `--quit`, which never fires when the remaining game never
  connected. Records and `.comet` are pruned by pid and start time, so a reused pid is
  never signalled. After a `TERM` the stop waits up to 1 s for Comet to go before it
  releases the lock, so a launcher starting then doesn't reuse a Comet on its way out.
- **Signals during the start.** `GALAXY_ON` is set before `galaxy-start` runs and the
  start's subshell ignores INT, TERM and HUP, so a Ctrl-C or a `TERM` to the launcher
  lets the start finish and record itself (start line included); bash then runs the trap,
  whose stop covers that Comet. gogo itself runs with the default dispositions restored
  (an ignored TERM would be inherited by Comet, which then never stops on it). `timeout`
  keeps gogo in its own process group, so a terminal's Ctrl-C doesn't reach it.
- **Run log.** The start line is the first line after the header's `# ---`; the stop line
  is the footer's first. `run_output` skips `# galaxy:` lines (the early-exit dialog's
  errors are the game's), and the report keeps them. The run's duration and the early-exit
  clock start after Comet. Stop lines: `stopped comet (pid N)`, `comet (pid N) didn't stop
  on INT in 2s: TERM` (`, still running after 1s` if so), `comet (pid N) left running for launcher P`, `nothing of gogo's to
  stop`.
- **#464's slots** are marked in the template: the pull right before `galaxy_start`, the
  push right before `galaxy_stop`.
- **Tests.** `galaxy`'s unit tests run the hand-off against a fake `comet` (bash: it records
  its argv, environ, process group, the file and its modes; the test binds the port as Comet
  would) with made-up tokens; the launcher tests use a fake gogo and a fake Comet for start,
  reuse, skip, INT and TERM stops, the signal path, a signal during the start, the
  watchdog, two launchers, a foreign process name in gogo's line, a runtime dir that isn't
  private (or a link in it), no runtime dir, and a planted token that must not reach the run log, `launch.log`, `report.txt` or
  the launcher's output.
- **Open:** the Galaxy access token Comet gets may be close to expiry (gog-warp refreshes only
  an expired one; #502, decided by H2's hour check).

### 1.3 As built: the switch, detection and reporting (#489)

- **Where it lives.** `galaxy = "comet" | "off"` under `[platforms.linux]`, in the game's L4
  override file like `display` and `wrapper` (`launch.conf` is regenerated by every setup, so it
  can't hold the choice). `gogo linux-app galaxy <gogid> on|off|--clear` edits it; setup renders
  `GALAXY='1'` into `launch.conf` (no line when off), so `launch.sh` is unchanged and
  `LAUNCHER_VERSION` stays 18. A built game picks it up on its next launch's setup.
- **Who decides** (`linuxapp::galaxy_switch`, one merged compat for setup, its question,
  `apps show` and `doctor`): the game's own L4 value, `off` included; else `[linux]
  galaxy_default`: `on` turns it on for a game that ships the SDK, `ask` asks about a game that
  ships it or that compat recommends Comet for, `off` (unset) leaves it off. A compat
  `galaxy = "comet"` is a recommendation only and never turns anything on by itself; `off` is
  the game's own override only (in a shipped file it warns and is ignored).
- **`ask`.** Only inside setup run from a terminal (a build, `linux-app setup`): one prompt per
  candidate game, the answer stored in L4 (`on` or `off`), so it is never asked twice. Headless
  (the launcher's setup, a script) it acts as `off` and leaves a note naming the command. Never
  from `launch.sh`.
- **No download.** `galaxy on` writes the switch and, when Comet isn't available, prints the
  `gogo runtimes install comet` hint; the launch skips Comet until then.
- **SDK detection.** Setup walks the game's files (depth- and count-bounded, symlinks not
  followed) for `Galaxy.dll` / `Galaxy64.dll` in any case on every setup and records
  `galaxy_sdk` in `state.toml` (optional field, no schema bump). A scan that hits its bounds
  (depth 8, 100,000 entries) or can't read a dir records nothing (`None`, "unknown"), not
  "not found". A build from before it has no record: `apps show` (one game),
  `linux-app galaxy` and `doctor` scan on demand and write nothing; `apps list` doesn't scan.
  A changed `galaxy_default` or a game update is picked up by the next setup run (an explicit
  `linux-app setup`, a rebuild, or any change that makes the launcher's setup run); the
  launcher's own staleness check still covers `launch.toml` and the L4 file only.
- **`apps show`:** `galaxy: comet on (<ver>)` / `off` (`comet on (not installed)`, `off (compat
  recommends comet)`, a pending change) and `Galaxy SDK: shipped | not found | unknown`; both in
  `--json`.
- **`doctor`:** one `galaxy` finding always (SDK shipped with galaxy off is **info** with the
  command, since off is the default and many games ship the DLL; galaxy on with no SDK is a
  warning). With galaxy on: `comet` (resolved without a download: the pin from the store or
  `GOGO_COMET`, a `system`/`path:` binary is reported as unchecked), `galaxy-login` (a stored
  login, never a call to GOG; a token store that would prompt is only touched here),
  `galaxy-runtime` (`$XDG_RUNTIME_DIR` as `galaxy-start` requires it, read-only) and
  `galaxy-port` (9977, read from the socket table, never connected to; info, with the holder
  when it can be named). Problems that make a launch skip Comet are warnings, not failures.
  No token, id or user name is printed.

## 2. Around a launch

### 2.1 Start

With `GALAXY=1`, after setup, the refusals and design 22's pre-launch pull, before the
game's start in the run log, `launch.sh` runs the internal
`gogo linux-app galaxy-start <gogid> --launcher`, which:

1. Restores the session as every command does; refreshes the Galaxy token in the **main**
   session and persists it (design 22 §2.1 step 4). The username comes from the session's
   user data.
2. Checks port 9977 (the kernel's socket table, never a connection: §1.2). Taken
   (another gogo game's Comet, Heroic's): prints `reusing` (naming the process when
   `ss -ltnp` can), starts nothing, done.
3. Writes the credential file (decision 5).
4. Spawns Comet in its own session (`setsid`): `--from-heroic --username <name> --quit`,
   `XDG_CONFIG_PATH=<run dir>`, `COMET_LOG=off`, stdin/stdout/stderr on `/dev/null`, and
   `XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `XDG_CACHE_HOME` under `<data root>/comet/`, so
   neither the user's nor Heroic's `~/.local/share/comet` is touched.
5. Waits up to 5 s for the port, deletes the run dir, prints Comet's pid.

Every failure prints one line and exits 0; `launch.sh` writes it to the run log as
`# galaxy: comet <ver> started (pid N) | reusing port 9977 | skipped: <reason>`.

### 2.2 Stop

`launch.sh` registers under `$XDG_RUNTIME_DIR/gogo/comet/.users/<launcher pid>`; the
Comet gogo started is recorded in `.comet` beside it (§1.2). Once the game's prefix is idle
(where the run log's footer and design 22's push are), it removes its record; if no live
record is left and gogo's Comet still runs, whoever started it, it sends `INT`, then `TERM`
after 2 s. The watchdog's teardown (a SIGKILLed launcher) and the signal path do the same.
`--quit` is a backstop: Comet ends by itself after the last game disconnects, but not if
none ever connected. Two games at once share one Comet, which outlives the first to quit.
The template change bumps `LAUNCHER_VERSION` (18, #488; #464 takes the next).

### 2.3 Order

Pull (design 22) → Comet start → game → prefix idle → push (design 22) → Comet stop. Not on
`linux-app run` one-offs.

### 2.4 Failure modes

| Case | What happens |
|---|---|
| not signed in, session rejected | skipped (`gogo login`) |
| signed in, refresh fails (offline) | started with the stored tokens: the SDK gets a local answer at once, the exchanges fail inside Comet, the game sees "not signed in" |
| keyring would prompt with no TTY | the 5 s budget, then skipped |
| no `$XDG_RUNTIME_DIR` | skipped |
| binary missing, wrong pin, crashes, port never opens | skipped or logged; the game starts |
| `gogo` not found by the launcher | no Comet, one log line |

## 3. Reporting

- **`apps show`:** `galaxy: comet on (<ver>)` or `off`; `Galaxy SDK: shipped|not found`.
- **`doctor`:** the binary exists and matches its pin (NixOS: the `GOGO_COMET` store path
  still exists), signed in, `$XDG_RUNTIME_DIR` available, port 9977 free or held (by whom);
  a warning when the game ships the SDK with galaxy off, or galaxy is on with no SDK.

## 4. Host pass (@noosxe; steps go into the implementing PRs)

- **H1, the cause, without Comet:** during the wait, `ss -tunp` for the game's processes
  (127.0.0.1:9977? CD PROJEKT RED hosts?); one run with the network off, timing the second
  press to the menu.
- **H2, with Comet** (`galaxy on`): the second press, network on and off; the Load menu
  lists the cross-progression save; the cross-platform saves setting shows signed in; an
  achievement unlock reaches the GOG profile; unlocks still post after more than an hour;
  two gogo games at once, quit in either order: one Comet, gone after the last.
- **H3, Heroic control** (done 2026-11-21, #486): with Heroic's experimental Comet support
  off, the wait is back and the cross-progression saves don't show.
- **H4, logs:** the run log, `launch.log` and `report.txt` hold no refresh token, access
  token, client secret or user id; no `auth-*` dir is under `$XDG_RUNTIME_DIR/gogo` while
  the game runs (only the `comet/` registry is); `ps -o args` of Comet shows only
  `--from-heroic --username … --quit`.

## 5. Implementation issues (epic #155)

| Issue | Scope | Priority |
|---|---|---|
| #486 | settle the cause: H3 done (closed by #487's PR); the online/local timeout stays open | high |
| #487 | the Comet pin and flake build, `[linux] comet` | high |
| #488 | `galaxy-start`, the credential hand-off, `launch.sh` start/stop | high |
| #489 | `galaxy on/off`, compat/config keys, SDK detection, `apps show`, `doctor` | medium |
| #490 | e2e with the real pinned Comet, offline (needs the download OK) | medium |
| #492 | the dummy Galaxy service per game | low |
| #493 | Comet in the Mac launcher (epic #50) | low |
| #494 | the upstream report, for @noosxe to file | medium |
