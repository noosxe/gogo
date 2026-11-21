# 23 — A Galaxy service for Linux builds: Comet (#356)

Status: **accepted** (2026-11-21; decisions 1–12 accepted as recommended). Nothing is built
yet: implementation issues are in §5. The facts behind it are in
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
   recommends it for a game (Cyberpunk once the host pass confirms); the config default is
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

## 2. Around a launch

### 2.1 Start

With `GALAXY=1`, after setup, the refusals and design 22's pre-launch pull, before the
game's start in the run log, `launch.sh` runs the internal
`gogo linux-app galaxy-start <gogid> --launcher`, which:

1. Restores the session as every command does; refreshes the Galaxy token in the **main**
   session and persists it (design 22 §2.1 step 4). The username comes from the session's
   user data.
2. Checks port 9977. Taken (another gogo game's Comet, Heroic's): prints `reusing` (naming
   the process when `ss -ltnp` can), starts nothing, done.
3. Writes the credential file (decision 5).
4. Spawns Comet in its own session (`setsid`): `--from-heroic --username <name> --quit`,
   `XDG_CONFIG_PATH=<run dir>`, `COMET_LOG=off`, stdin/stdout/stderr on `/dev/null`, and
   `XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `XDG_CACHE_HOME` under `<data root>/comet/`, so
   neither the user's nor Heroic's `~/.local/share/comet` is touched.
5. Waits up to 5 s for the port, deletes the run dir, prints Comet's pid.

Every failure prints one line and exits 0; `launch.sh` writes it to the run log as
`# galaxy: comet <ver> started (pid N) | reusing port 9977 | skipped: <reason>`.

### 2.2 Stop

`launch.sh` registers under `<data root>/comet/.users/<launcher pid>`, with the pid it
started (if any). Once the game's prefix is idle (where the run log's footer and design 22's
push are), it removes its record; if it started Comet and no live record is left, it sends
`INT`, then `TERM` after 2 s. The watchdog's teardown (a SIGKILLed launcher) does the same.
`--quit` is a backstop: Comet ends by itself after the last game disconnects, but not if
none ever connected. Two games at once share one Comet, which outlives the first to quit.
The template change bumps `LAUNCHER_VERSION` (to the next number after #464's, whichever
lands first).

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
  token, client secret or user id; `$XDG_RUNTIME_DIR/gogo` is empty while the game runs.

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
