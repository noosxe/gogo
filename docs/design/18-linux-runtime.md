# 18 — Linux runtime: GE-Proton through umu, DXVK/VKD3D-Proton/D7VK, the Linux launcher

**Status:** contract (2026-11-03). Nothing implemented yet; the work is tracked under
epic #155 (issues in §13).
**Research:** [docs/21](../21-linux-runtimes-and-renderers.md) (runtimes, renderers,
per-game fixes, the eleven decisions in §8, risks R1–R30 in §9) and
[docs/22](../22-ddraw-renderers-d7vk-vs-cnc-ddraw.md) (the `ddraw.dll` split, §6–7).
**Decisions:** docs/21 §8, all eleven settled by the owner on 2026-11-03. The details
they left open (O1–O9) were settled by the owner on the same day (§12.1).
**Builds on:** design 16 (layers L0–L4, `gogo-launch.toml` + `launch.conf`, setup and
its stamp, store and GC, logs, early-exit dialog, `doctor`), design 12 (catalog and
store), design 06 (paths). Read design 16 first: this doc only says what differs on
Linux.
**Amends:** design 05 §3 (a `[linux]` config table), design 06 §1 (the Linux data root,
§6 here), design 16 decision 5 and compat/README.md (`[platforms.<os>]`, §5 here).
**Scope:** Windows games on Linux, x86_64 first, nothing that assumes it. Native GOG
Linux installers are epic #162, not this doc (§9).

## 0. Summary

A Linux game is a **per-game state dir** plus a `.desktop` file. There's no bundle and
no signing. Everything else follows design 16:

```
~/.local/share/applications/gogo-<gogid>.desktop      Exec= the script below

$XDG_DATA_HOME/gogo/                                   the "data root" (§6), ~/.local/share/gogo
  runtimes/ge-proton/<ver>/<arch>/                     GE-Proton, as pinned (§2.2)
  runtimes/umu/<ver>/umu/umu-run                       umu zipapp, as pinned (§2.1)
  renderers/cnc-ddraw/<ver>/<arch>/                    the only renderer download on Linux (§3)
  games/<gogid>/gogo-launch.toml                       what to run, baked at build (design 16 §1.1)
  games/<gogid>/launch.conf                            effective config, written by setup
  games/<gogid>/launch.sh                              generated launcher (§7)
  games/<gogid>/state.toml                             what setup applied + stamp + protonfix hash
  games/<gogid>/prefix/                                WINEPREFIX (umu adds pfx -> .)
  games/<gogid>/game/                                  game files (reflink or copy of the depot tree)
  compat/<gogid>.toml                                  L4 user override

$XDG_DATA_HOME/umu/                                    Steam Linux Runtime, owned by umu (§2.3)
$XDG_STATE_HOME/gogo/logs/<gogid>/                     launch.log + run logs (§7.3)
```

The launch, in one line: `launch.sh` sources `launch.conf`, checks the stamp, and runs
`GAMEID=umu-<id> STORE=gog WINEPREFIX=… PROTONPATH=… python3 umu-run <exe>` from the
game dir.

### Decisions table

| # | Decision | Source |
|---|---|---|
| 1 | Runtime: GE-Proton through umu-launcher; no second runtime in the first round. Unmanaged Proton builds allowed | docs/21 §8 items 1, 7 |
| 2 | umu is required for Proton, fetched as the release zipapp after the user confirms; on Nix it runs inside `steam.buildRuntimeEnv` (R1, verified), elsewhere with the user's Python (§2.1) | §8 item 1 |
| 2a | GE-Proton lives in gogo's store by default; in a shared `compatibilitytools.d` if the user says so once; `prune` never deletes there | §8 item 2 |
| 2b | The Steam Linux Runtime belongs to umu, in its shared folder; gogo doesn't pin or prune it | §8 item 3 |
| 3 | Renderer mapping: Proton's defaults for D3D9–12; D7VK for D3D≤7 or an unclear scan, cnc-ddraw for confident 2D, a game's own `ddraw.dll` wins, never both | §8 item 8, docs/22 §6 |
| 4 | Per-game fixes worked out at app creation: GOG → umu id from the build's CSV (GitHub fallback), the protonfix read without running it, one summary with each fix's source | §8 items 4, 5 |
| 5 | One compat file per game with `[platforms.<os>]` tables; today's 11 L3 files migrated | §8 item 6 |
| 6 | Store and state on Linux: XDG data root, per-OS catalog entries, zstd later, GC as design 16 | this doc |
| 7 | Launcher: `.desktop` → generated `launch.sh` → sources `launch.conf`; gogo does setup | §8 item 9 |
| 8 | CLI: `gogo linux-app`, the sibling of `mac-app` (§10) | this doc |
| 9 | Native Linux installers: epic #162. aarch64: in the design, nothing built for it alone | §8 items 10, 11 |

## 1. Decision 1: the runtime

**Settled (docs/21 §8 item 7):** the first round ships one runtime, GE-Proton, run
through umu-launcher. It's a catalog entry like `winecx` on the Mac: pinned by sha256
(the release's `.sha512sum` asset and GitHub's asset digest must both agree), `prune`-able,
with the N-1 rollback of design 16 decision 6.

- The catalog entry is `ge-proton/<GE-ProtonX-Y>`: the newest release when the entry is
  written (GE-Proton11-12 as of 2026-11-03; docs/21 inspected 11-11).
  Its `arch` is `x86_64` (`GE-ProtonX-Y-x86_64.tar.gz`); the `aarch64` asset is a
  second entry for the same version when someone needs it (§9).
- gogo never runs GE-Proton's `proton` script itself. Without umu there's no Proton
  runtime (§2.1).
- Plain Wine (Kron4ek builds, the distro's Wine; docs/21 §1.3, §3.2) stays researched but
  unbuilt. Its pieces (DXVK/VKD3D-Proton into the prefix, zstd, a D7VK release zip, the
  host-library wrapper of R8) wait for it.

**Unmanaged builds (docs/21 §8 item 2).** gogo can also run a Proton build it didn't
install: one in a `compatibilitytools.d` put there by Steam, Lutris, Heroic or
ProtonUp-Qt; Valve's Proton under `steamapps/common/`; or a directory the user names.
umu takes any directory with a `toolmanifest.vdf`.

- The ref is `path:<dir>`, as for a `path:` runtime on the Mac (design 16 §1.2).
  `--runtime path:<dir>` picks one for a game; `[linux] default_runtime = "path:<dir>"`
  makes it the default.
- `gogo runtimes list` shows them next to gogo's own with an **unmanaged** note: not
  pinned, not installed by gogo, never pruned. Discovery reads the directories of §2.2
  plus `steamapps/common/Proton*` in each Steam library (`libraryfolders.vdf`).
- What the note warns about: the build may change or vanish (§2.2's launch check), it
  may lack `protonfixes/`, D7VK or a CSV (R28), its `require_tool_appid` may be unknown to
  the pinned umu (R2), and L3 `tested` entries don't match it.
- Inspecting an unmanaged build (Wine version, bundled renderers, required runtime) and
  saying why it may not suit a game is later work, not in the first round.

**As built (#169)** — choices the design left open:

- **Catalog.** `ge-proton/GE-Proton11-12` (still the newest release on 2026-11-04), a new
  `Kind::Proton { appid, sha512 }`, `os = ["linux"]`, `arch` x86_64, 566,859,228 B.
  "Both digests agree" can't be checked without the bytes (one is a sha256, the other a
  sha512), so the entry pins both: the sha256 is GitHub's asset digest (`0595159e…`,
  checked at probe time as for every pinned asset, and against the download), and the
  sha512 is the release's `GE-Proton11-12-x86_64.sha512sum` (`2e374479…`, read from that
  160-byte asset), checked against the download too. A mismatch on either discards the
  archive. `appid` is the expected `require_tool_appid`, `4183110` (steamrt4).
- **Install-time check (R2).** The store reads `toolmanifest.vdf` after unpacking: an
  appid missing from `catalog::umu_runtimes()` fails the install (`MacAppError::Umu`,
  exit 7) with "Bump umu in gogo's catalog (with this GE-Proton, in one commit)"; an
  appid other than the entry's recorded one fails as an invalid runtime. A unit test
  checks every `Kind::Proton` entry's appid against the umu entry.
- **VDF.** `linuxapp::vdf`, a ~150-line KeyValues reader (quoted and bare tokens, `{}`,
  `//` comments, `\\ \" \n \t`): no crate, for two small files.
- **Refs.** `linuxapp::proton::ProtonRef`: `ge-proton[/<ver>]` or `path:<dir>` (an
  absolute dir). `[linux] default_runtime` takes either; unset, the catalog's GE-Proton is
  the Linux default (shown as `available · default` in `runtimes list`, and its newest
  build is kept by the sweep as `winecx26`'s is on the Mac). Resolving a ref for a game is
  #176's.
- **`runtimes list` on Linux** adds an "unmanaged Proton builds" table (path, `version`,
  where it was found, note) and an `unmanaged` array in `--json`. Each row says
  **unmanaged**, "default" when it is `default_runtime`, and the problems: no
  `toolmanifest.vdf`, no `require_tool_appid`, or one umu's pin doesn't know (R2; `doctor`
  failing on it is #178's). A `default_runtime` path that discovery didn't find is listed
  too. Discovery skips builds carrying gogo's marker (they are the `ge-proton` row) and
  hidden dirs (gogo's install temp dirs); one row per build when two paths reach it.
  Steam libraries are the Steam root plus each `path` in `steamapps/libraryfolders.vdf`.

## 2. Decision 2: umu, where GE-Proton lives, the container

### 2.1 umu is required, fetched as the zipapp

**Settled (docs/21 §8 item 1):** umu is required for the Proton runtime. gogo fetches
umu's release zipapp (`umu-launcher-<ver>-zipapp.tar`, 430,080 B for 1.4.4) into its
store as catalog entry `umu/<ver>`, pinned by sha256 and GitHub's digest, after the user
confirms. The archive holds one file, `umu/umu-run`, a zip with a `#!/usr/bin/env
python3` shebang that vendors its dependencies. umu is GPL-3.0; gogo runs it as a
separate program and links nothing.

How gogo runs it:

- **Always through an explicit interpreter**: `<python> <store>/umu/<ver>/umu-run …`,
  never through the shebang. The interpreter is recorded in `launch.conf` as `UMU_PYTHON`
  so the launcher runs the same one setup used.
- **Nix: inside an FHS env (R1, verified 2026-11-03, docs/21 §8 item 1).** On a stock
  NixOS root the bare zipapp fails: pressure-vessel needs `/lib64/ld-linux-x86-64.so.2`,
  a `true` under `/usr` for its bwrap check, and `ld.so.cache` for the host graphics
  stack. Inside `steam.buildRuntimeEnv` (nixpkgs' own wrapper for `umu-launcher`) the same
  zipapp starts steamrt4 with i386 resolved and `/run/opengl-driver{,-32}` mapped. So
  gogo's flake builds that env with `extraPkgs = [ python3 ]` and `runScript =
  "python3"`, and its `gogo-linux-app` package records the env's path at build time (a
  wrapper env var, `GOGO_UMU_PYTHON`). The default package stays without it.
  `UMU_PYTHON` is then that env, not a bare Python. The zipapp pin stays; the user
  installs nothing. A minimal `buildFHSEnv` isn't enough (no i386 loader, no driver
  mapping). Whether the GPU works through it is the host pass's first check (§11.2).
- **NixOS without `gogo-linux-app`** (the default flake package, or a gogo binary from
  elsewhere): no FHS env, and nix-ld doesn't help. Setup refuses `umu = "zipapp"` there
  (`/etc/NIXOS` exists and `GOGO_UMU_PYTHON` is unset) and names the two ways out: install
  `github:noosxe/gogo#gogo-linux-app`, or `umu = "system"` with nixpkgs' `umu-launcher`.
  `doctor` says the same.
- **Elsewhere:** the first `python3` on `PATH` that reports ≥ 3.10 (`requires-python` in
  umu's `pyproject.toml`). None → setup fails with what to install, and `doctor` says the
  same (R6).
- **Host prerequisite:** unprivileged user namespaces (pressure-vessel runs bwrap).
  `doctor` checks `kernel.unprivileged_userns_clone` / `user.max_user_namespaces` where
  they exist.
- **Consent.** The plan shows umu's download (with GE-Proton's 565 MB and the container,
  which umu fetches on first launch: `SteamLinuxRuntime_4.tar.xz` is 175 MB by HTTP HEAD,
  docs/21 §1.2, and 699 MB unpacked, R1) before the confirmation. The answer to "use
  umu" is stored as `[linux] umu = "zipapp"`, so it's asked once. Headless with no answer:
  no download, and the build fails naming the key.
- **Another umu instead.** `[linux] umu = "system"` uses `umu-run` from `PATH` (nixpkgs'
  `umu-launcher`, the Debian/Fedora packages) with no pin. `gogo runtimes list` shows it
  as unmanaged. `UMU_RUN` in `launch.conf` then holds its absolute path, and `UMU_PYTHON`
  is empty.

| Where | How umu runs | `UMU_PYTHON` / `UMU_RUN` |
|---|---|---|
| the flake's `gogo-linux-app` (NixOS or Nix on another distro) | zipapp in `steam.buildRuntimeEnv` | the FHS env / store zipapp |
| other distros (any other gogo) | zipapp with the host's Python ≥ 3.10 | `python3` path / store zipapp |
| NixOS, any gogo but `gogo-linux-app` | refused unless `umu = "system"` | — |
| `umu = "system"`, anywhere | `umu-run` on `PATH` | empty / its absolute path |

**What umu doesn't report (docs/21 §8 item 1).** A `PROTONPATH` without
`toolmanifest.vdf` makes umu raise `FileNotFoundError` and still exit 0, and a failing
launch is retried once. So gogo checks the Proton dir itself before every launch (§2.2),
and the run log's exit code is only trusted once that check passed.

**The umu pin and GE-Proton move together (R2).** umu maps a runtime's
`require_tool_appid` to a container in a hard-coded table. Each catalog umu entry lists
the appids its pin knows (`runtimes = ["1391110", "1628350", "4183110", "4185400"]` for
1.4.4: soldier, sniper, steamrt4, steamrt4-arm64; the first two ids to confirm against
`umu_runtime.py` when the entry is written). When gogo installs a GE-Proton, it reads
`toolmanifest.vdf`. An appid the pinned umu doesn't know fails the install with "bump
umu" for a managed build, and is a `doctor` failure for an unmanaged one. A catalog
bump that adds a GE-Proton needing a new container must bump umu in the same commit; a
unit test checks every GE-Proton entry's appid against the umu entry.

**As built (#168)** — choices the design left open:

- **Catalog.** `umu/1.4.4` is a `Kind::Tool` (payload `umu-run`), `os = ["linux"]`,
  `arch` x86_64, pinned to `eb590691…cc851` (GitHub's digest, re-checked against the
  download on 2026-11-04). `Kind::Tool` gained `notice` (umu's GPL-3.0 note, written to
  the store's `NOTICES.txt` as for renderers; it also names what the zipapp vendors:
  python-xlib LGPL-2.1, urllib3/truststore/six MIT) and `runtimes`, the appid list.
  The list was confirmed against `RUNTIME_VERSIONS` in `umu/umu_runtime.py` at the
  1.4.4 tag: `1391110` soldier, `1628350` sniper, `4183110` steamrt4, `4185400`
  steamrt4-arm64. `catalog::umu_runtimes()` returns it for #169's check.
- **Store layout.** The archive is kept as unpacked: the zipapp is
  `runtimes/umu/<ver>/umu/umu-run` (the manifest's `entry_rel`), beside upstream's
  `umu_run.py` symlink.
- **Code.** `gogo_core::linuxapp::umu`. `provision()` resolves a `UmuRun` (whose
  `launch_conf()` gives the `UMU_PYTHON` / `UMU_RUN` values) and downloads the zipapp
  only after the interpreter checks pass, so nothing is fetched that couldn't run. Its
  errors are `MacAppError::Umu` (exit 7).
- **Interpreter.** `GOGO_UMU_PYTHON` must be an absolute path to an executable; it
  isn't run to check its version (running the FHS env starts bwrap). Empty counts as
  unset. `python3` candidates are run with `--version`; relative `PATH` entries are
  skipped. `umu = "system"` ignores the NixOS check.
- **Consent.** A two-item select, `zipapp` (the default) or `system`, under the plan
  line (umu's download size from the catalog probe, its licence, and the container umu
  fetches on first launch). When the zipapp can't run here and a `umu-run` is on
  `PATH`, `system` is the default and the zipapp item says why. A cancelled prompt
  stores nothing. Wiring it into the `linux-app` plan is #176's.
- **Flake: a package of its own (owner, 2026-11-04).** `nix/umu-python.nix` is
  `steam.buildRuntimeEnv { extraPkgs = [ python3 ]; runScript = "python3"; }` (no
  `allowUnfree`: the env holds nothing of Steam). The env is about 1.5 GiB, so the
  default package (`default` / `gogo`, every system) stays without it, unchanged from
  before. `packages.x86_64-linux.gogo-linux-app` (`nix/linux-app.nix`, also an `apps`
  entry) is a `symlinkJoin` around that same build, with `gogo` wrapped by
  `--set-default GOGO_UMU_PYTHON` (a user's own value wins): no second Rust build. The
  env is also `packages.x86_64-linux.umu-python`. x86_64 Linux only: the env is
  multiArch, and nixpkgs has no i686 set on aarch64. The pin's Python is 3.14 (R1 ran
  3.12); the zipapp's `--help` was smoke-run through it, and a launch on it is #179's
  host pass to confirm.
- **Dev shell: not exported.** Building and testing never need it (tests inject the
  env), and exporting it would build the multiArch FHS env on every `nix develop`,
  CI's included. For a manual run of the lane from the dev shell:
  `export GOGO_UMU_PYTHON=$(nix build .#umu-python --print-out-paths)/bin/gogo-umu-python`.
- **For #177/#178.** `current_exe()` resolves to the unwrapped binary (the wrapper
  execs `.gogo-wrapped`, a link into the default package), so it bypasses the wrapper:
  re-exec gogo through the path it was invoked as. And
  `UMU_PYTHON` is a store path that a Nix GC can remove after a gogo upgrade: setup
  should re-record it, and `doctor` should check that it exists.

### 2.2 Where GE-Proton builds live

**Settled (docs/21 §8 item 2):**

- **Default: gogo's store**, `runtimes/ge-proton/<ver>/<arch>/`, passed to umu as an
  absolute `PROTONPATH`.
- **Shared tools dir, on request.** When Steam is installed or a `compatibilitytools.d`
  already exists, gogo asks once whether to keep its GE-Proton builds there instead, and
  stores the answer as `[linux] proton_dir = "store" | "steam"`. `gogo config set
  linux.proton_dir …` changes it later. It applies to new installs; builds already
  installed stay where they are.
- Detection: `$XDG_DATA_HOME/Steam` (`~/.steam/root` usually points there) and Flatpak
  Steam's `~/.var/app/com.valvesoftware.Steam/data/Steam`. The tools dir is
  `<steam>/compatibilitytools.d`. With both present, gogo asks which.
- **`prune` never deletes from a shared tools dir**, not even gogo's own builds: Steam,
  Lutris or Heroic may have a game set to one by name. `prune` lists gogo's old builds
  there with their sizes and leaves deleting them to the user (R26).

**Settled (owner, 2026-11-03; O1, R27) — how gogo marks its builds there, and the launch check:**

- gogo installs into a temporary dir inside the tools dir, then renames it to
  `GE-ProtonX-Y` (the name umu and Steam show), so a half-extracted build never appears
  under a real name.
- **Marker:** gogo writes `gogo-managed.toml` into the build's root: catalog ref, the
  archive's sha256, the sha256 of the `proton` script, the `version` file's contents,
  `installed_at`, gogo's version. The store manifest records the absolute path too. A
  build without the marker is someone else's.
- **Name already taken.** If `compatibilitytools.d/GE-ProtonX-Y` exists without gogo's
  marker (ProtonUp-Qt installed the same version), gogo doesn't touch it. It installs
  that version into its store instead and says so.
- **Before every launch** (`launch.sh`, cheap): `PROTON_DIR/proton` and `toolmanifest.vdf` exist, `version`
  matches `PROTON_VERSION`, and `sha256sum proton` matches `PROTON_SHA`, all from
  `launch.conf`.
  - Managed build gone or changed: run `gogo linux-app setup <gogid>`, which reinstalls
    the pin (into the same place, or the store when the name is now taken). No gogo: a
    dialog says which runtime is missing and how to fix it; exit 127 as on the Mac
    (design 16 §8, `# exit: 127 (runtime missing)`).
  - Unmanaged build changed: log it, record the new version in the run log header, and
    launch (the user chose to follow that build). Gone: as for a managed build, minus the
    reinstall.
- `doctor` checks the same, plus the marker's archive sha against the catalog.

**As built (#169)** — choices the design left open:

- **Store layout.** `runtimes/ge-proton/<ver>/x86_64/` holds the archive as unpacked,
  top dir kept as for Wine runtimes: `…/GE-Proton11-12/x86_64/GE-Proton11-12-x86_64/proton`
  is the manifest's `entry_rel`, and `store::proton_dir()` (where `proton` is) is the
  `PROTONPATH`. Store builds get the marker too, so one check covers both places.
- **`proton_dir` values.** `"store" | "steam" | "steam-flatpak"`: with both Steams
  present the answer has to say which, so Flatpak's is its own value. `steam` is the
  native Steam, or the only one there is. A stored Steam that's no longer there → the
  store, with a note. The question is a select (store first, the default; then each
  Steam's tools dir) whose title carries the download size from the catalog probe and
  "about 1.5 GB unpacked" (R5). A cancelled prompt fails and stores nothing. Headless and
  unset, with a Steam: the store, a note naming `gogo config set linux.proton_dir
  steam`, nothing stored. No Steam: the store, nothing asked. `gogo runtimes install
  ge-proton` asks today; wiring it into the `linux-app` plan is #176's.
- **Detection.** Native: `$XDG_DATA_HOME/Steam` (default `~/.local/share/Steam`), else
  `~/.steam/root`; one root when the symlink points at the first. Flatpak:
  `~/.var/app/com.valvesoftware.Steam/data/Steam`. An existing root dir counts as a Steam
  install; its `compatibilitytools.d` is created when gogo installs there.
- **Shared install.** `store::install_to(…, shared: Some(tools_dir), …)`: the archive
  downloads into the store's `.tmp/`, unpacks into `<tools>/.gogo-tmp-<ver>-<pid>/`
  (hidden, and a level below where Steam looks), is checked (appid, `proton` beside
  `toolmanifest.vdf`) and marked there, then the build dir itself is renamed to
  `<tools>/GE-ProtonX-Y` and the temp dir removed. Any failure removes the temp dir. The
  manifest records the absolute path (a `path` field, absent for store entries, so
  existing manifests read unchanged) with `entry_rel = "proton"`.
- **Marker** (`gogo-managed.toml`, schema 1): `catalog_ref`, `archive_sha256`,
  `proton_sha256`, `version` (the file's contents, trimmed), `installed_at` (RFC 3339),
  `gogo_version`, under a comment saying gogo never deletes the build from a shared dir.
- **The name already exists.** Without the marker: left alone, the store gets the build,
  and a note says so. With it: gogo's own. Intact and matching the pin, it is adopted
  into the manifest without a download. Changed, it is reinstalled in place (the
  reinstall §2.2 describes). A manifest entry whose shared build lost its marker no
  longer counts as installed.
- **The check for #177/#178.** `linuxapp::proton::check_build(dir, Expect { version,
  proton_sha256 })` (`proton`, `toolmanifest.vdf` and `version` exist, both match) and
  `check_managed(dir)` (the same against the build's marker), returning a
  `BuildProblem`.
- **Prune (R26).** `store::prune` keeps every manifest entry with a `path`, on disk and in
  the manifest, and returns the ones a target named in `PruneOutcome::shared` (target,
  path, size measured now). `gogo runtimes prune` prints each as "left … (size): gogo
  never deletes from a shared tools dir …; delete it yourself when nothing does". The
  sweep still names them, so every `prune --unused` lists them again.

### 2.3 The Steam Linux Runtime

**Settled (docs/21 §8 item 3):** umu owns it. It lives in `$XDG_DATA_HOME/umu/<variant>`
(or `$UMU_FOLDERS_PATH/umu`), shared with every other umu user on the machine, under
umu's own `umu.lock`. gogo doesn't pin, update or prune it (R3, accepted).

- gogo doesn't set `HOME` or `XDG_DATA_HOME` for games in the first round. If a later
  feature does, the launcher sets `UMU_FOLDERS_PATH` back to the shared location (as
  Lutris's `get_umu_folders_path` does), recorded as `UMU_FOLDERS` in `launch.conf`.
- First launch fetches the container if no other umu user did: for steamrt4 a 175 MB
  archive (the HTTP HEAD size, docs/21 §1.2; the R1 run didn't measure the transfer),
  699 MB unpacked (R1). The
  plan says so; `doctor` reports which variants are present and their size, and warns
  when the one the game's build needs is missing.

## 3. Decision 3: renderers

**Settled (docs/21 §8 item 8, docs/22 §6).** Under GE-Proton the renderer is a set of
switches, not a download. The L1 import scan (`pe.rs`) feeds `renderer::select` as on the
Mac, with a Linux table:

| L1 class | Renderer | What setup and the launcher do |
|---|---|---|
| D3D9 / 10 / 11 | `dxvk` | nothing: Proton's default |
| D3D12 | `vkd3d-proton` | nothing: Proton's default |
| D3D8 | `dxvk` | `PROTON_DXVK_D3D8=1` (else wined3d's `d3d8`) |
| Game ships DDrawCompat as `ddraw.dll` (#199) | none | keep it, unloaded: no override, Wine's builtin; no cnc-ddraw, no D7VK |
| Game ships another `ddraw.dll` | none | keep it, `ddraw=n,b`; no cnc-ddraw, no D7VK |
| D3D≤7 immediate mode, interface GUIDs only, or only the `dxguid` block | `d7vk` | `PROTON_USE_D7VK=1` |
| D3D≤7 retained mode (`d3drm`/`d3dxof`) | `wined3d` | nothing: Proton's default `ddraw` |
| DirectDraw, selective DirectDraw GUIDs only, 32-bit | `cnc-ddraw` | `ddraw.dll` + gogo's `ddraw.ini` in the game dir, `ddraw=n,b` (as `ddraw.rs` does on the Mac) |
| OpenGL, Vulkan, packed, nothing found | `wined3d` | nothing |

- **Renderer ids on Linux:** `dxvk`, `vkd3d-proton`, `d7vk`, `cnc-ddraw[/<ver>]`,
  `wined3d`. `dxvk`/`vkd3d-proton`/`d7vk` carry no version: they are whatever the
  chosen Proton build bundles (GE-Proton11-11: DXVK `v3.1.1-134-g2cb40186c`, VKD3D-Proton
  `0bb64c56`, D7VK's GE submodule). `renderer = "wined3d"` sets `PROTON_USE_WINED3D=1`,
  which moves D3D9–12 together; a per-API choice is a `dll_overrides` entry (R16,
  accepted).
- **Never both `ddraw.dll`s.** Setup never sets `PROTON_USE_D7VK` while a `ddraw.dll` sits
  in the game dir (the game's or gogo's), and moving a game to D7VK removes gogo's
  cnc-ddraw (`ddraw::remove_gogo`). A user `renderer = "d7vk"` on a game that ships its
  own `ddraw.dll` warns and keeps the game's DLL (closes R17).
- **The detection rule** of docs/22 §6.2 goes into `pe.rs` for both lanes: device/work
  vs interface GUIDs, discount the `dxguid` block at ≥ 32 of 44 GUIDs (`dxguid_block`
  recorded for `doctor`), check files that load `ddraw.dll` by name, add the Unreal
  `D3DDrv.dll` and LithTech `.ren` hints. The Mac table only changes where docs/22 §6.1
  says so (it doesn't: the Mac keeps wined3d for D3D≤7).

  **As built (#173):**

  - **GUIDs.** The 44 `DEFINE_GUID`s of Wine `wine-11.0`'s `include/d3d.h` (27) and
    `include/ddraw.h` (17), unchanged on `master` as of 2026-11-04: 4 interface, 23
    device/work (the 7 devices incl. `IID_IDirect3DNullDevice`), 17 DirectDraw. Checked
    in files that import `ddraw.dll` and in files that name it (the runtime-string scan
    found `ddraw.dll`), in section data only.
  - **"Its D3D hits don't count" = they aren't evidence, not "2D".** A file with ≥ 32
    (`DXGUID_BLOCK_MIN`; 40 in #173) gets the lowest level, `dxguid_block`, and still
    adds `Direct3d7`: docs/22 §6.1 and the table above both keep "only the `dxguid`
    block" in the D3D≤7 class (D7VK on Linux, wined3d on the Mac), and #174's corpus
    keeps it there (below). Below 32: any device/work GUID → `strong`, interface GUIDs
    only → `medium`. A `d3dim`/`d3drm` import and the renderer DLLs are `strong`.
  - **Runtime-loaded ddraw.** The D3D≤7 evidence of a file that only names `ddraw.dll`
    lands in `runtime_graphics`, beside its `ddraw`: it decides only when no static
    import says anything (or for Unity/Unreal), like every runtime-loaded name.
  - **Engine hints.** `d3ddrv.dll` and `d3d.ren` next to the exe (`.ren` files are
    scanned as PE now) are `strong` unless the file imports `d3d8` or newer (UE2's
    `D3DDrv.dll` is D3D8). Listed in `renderer_dlls`. The engine stays unset: UE1 has
    no `Engine/` dir and picks nothing at runtime the scan could read.
  - **Output** (additive: old JSON still reads, every new field defaults). `PeScan`:
    `d3d7_confidence` (`strong` / `medium` / `dxguid_block` / `null`), `dxguid_block`,
    `renderer_dlls`. Per file: `ddraw_guids` (0–44), `dxguid_block`, `d3d7`;
    `d3d7_iids` stays (interface GUIDs present, block or not). The L1 reason says
    "interface GUIDs only" / "only from the dxguid GUID block: may be 2D"; `__pe-scan`
    prints a `d3d≤7` line. No Mac `doctor` check reads the scan today: the Linux
    `renderer` row (§8) is the first consumer of `dxguid_block`.
  - **Class transitions.** Only 2D DirectDraw → D3D≤7, never back: a device/work GUID
    without `IID_IDirect3D*`, GUIDs in a file that loads ddraw by name, a renderer DLL,
    or a `.ren` file's imports. On the Mac that is cnc-ddraw → wined3d. None of the
    `compat/` games changes (#173's PR lists them). The 64-bit rule is unchanged.
  - The `pe.rs` unit tests run on Linux too (they were macOS-gated with the rest of
    `macapp`).

  **The GOG corpus (#174, docs/22 §5.4):** six account games, unpacked, never run.

  - **D3D≤7 exes carry selective GUIDs.** Carmageddon TDR 2000: 2 (`medium`), Anno
    1503: 7 with device GUIDs (`strong`). Both → `d7vk` / Mac `wined3d`, right. Pharaoh
    and Zeus carry none, Chaos Gate 13 DirectDraw-only ones: `ddraw_2d`, right.
  - **The threshold moved from 40 to 32.** SimCity 3000 (2D) links DirectX 6's
    `dxguid.lib`: 36 GUIDs, the 43 minus the DirectX 7 ones, the same in its sound DLL.
    At 40 they counted as device GUIDs (`strong`). At 32 both blocks are caught; files
    without one carry at most 16. No pick changes: SimCity 3000 ships DDrawCompat,
    which both lanes leave unloaded (#199, below), and #173's local Steam
    sweep (re-run, 60 game dirs) is unchanged.
  - **"Block only" stays D3D≤7.** It leans 2D (the one block-only ddraw game is 2D, no
    D3D≤7 game carries a block), but that is one game, whose pick doesn't depend on its
    class. The Mac's "block only" row stays a host-pass decision (docs/22 §6.1).
  - **No retained mode** in the six (no `d3drm`/`d3dxof` import or name).
  - Not changed, noted: the game-level `d3d7_confidence` is the strongest file's, so a
    sibling tool (SimCity 3000's `Baapp.exe`) can lift it above what decided the class;
    and Zeus's info file names `zeus.exe` for `Zeus.exe`, which `find_entry_point`
    misses on a case-sensitive file system.
- **Where a `ddraw.dll` counts: beside the entry exe (#199, both lanes).** Wine loads
  DLLs from the exe's dir, and SimCity 3000 keeps its DDrawCompat in `Apps/`, beside
  `Apps/SC3U.exe`. Setup looked in the install root, so `own` was false, `ddraw=n,b` was
  never set and Wine's builtin ddraw ran; `doctor` looked in the same place. Now:
  - **One dir, the effective exe's** (`LaunchToml::exe_dir_in`, `ddraw::dir_for`): keeping
    the game's DLL, placing and removing gogo's cnc-ddraw, D7VK's "own ddraw wins" and
    `doctor`. The root when the dir isn't there (an exe path whose case differs). The
    name matches in any case, as in Wine and `pe::scan`.
  - **Variants: the one that runs, nothing else.** The launcher runs one exe. An L3/L4
    `exe` or `variant` changes it and the stamp, so setup re-plans for its dir, and
    gogo's DLL moves: setup takes it out of the root, the baked exe's dir and every
    variant's. A variant that isn't running gets no DLL. A `ddraw.dll` elsewhere in the
    tree is the game's business: it gets no override and nothing touches it.
  - **Existing games:** the next `gogo linux-app setup` applies it; a launch runs setup
    only when the stamp is stale (`gogo-launch.toml` or the L4 file changed), the prefix
    or umu's Python is gone or the build changed, since the launcher already on disk
    can't know gogo changed. Until then `doctor` warns on `renderer` and names that setup.
  - **The Mac:** the same dir. A game's own DLL gets `ddraw=n,b` only under cnc-ddraw,
    as before.
- **DDrawCompat stays unloaded (#199, the owner's decision, both lanes).** The first real
  look past the wrong dir showed the override was the bug's other half: SimCity 3000 with
  `ddraw=n,b` exits with code 9 within 2 s on a null read in DDrawCompat's init under
  GE-Proton11-12, and its README rules Wine out. On Wine's builtin it reaches the menu,
  scaled, and takes clicks (doc 22 §5.5, headless, not real hardware). So:
  - **Recognised by content** (`ddraw::is_ddrawcompat`, `PeScan::ddrawcompat`): the log
    strings `DDrawCompat loaded successfully` or `Loading DDrawCompat `, not the name,
    which every wrapper shares, and not the bare word, which other wrappers can mention.
  - **No override.** `plan_linux` returns `DdrawStep::LeaveDdrawCompat`; the file stays.
  - **D7VK can't be used:** Proton appends `ddraw=n,b` itself, which would load it.
    **cnc-ddraw can't either:** it isn't placed over a game's DLL. Both warn and walk on
    to wined3d. The L1 pick has no alternatives for it. Mac `apply` falls back the same.
  - **An L4 override is heard:** `dll_overrides.ddraw = "n,b"` still applies, setup warns,
    and `doctor` fails `renderer` naming the file to edit.
  - **Any other own `ddraw.dll`** keeps `ddraw=n,b` (Chaos Gate's GOG D3D9 wrapper: it
    exits 1 before a window under Proton, but so does Wine's builtin, so the wrapper isn't
    shown to be the cause). With no override `doctor` warns: the game runs, on the builtin.
- **"Try next renderer" order on Linux** (design 16 §8.2's walk, a Linux table):

  | L1 class | order |
  |---|---|
  | DirectDraw 2D, 32-bit | cnc-ddraw → d7vk → wined3d |
  | D3D≤7 (D7VK pick) | d7vk → cnc-ddraw → wined3d |
  | D3D9–12 | dxvk / vkd3d-proton → wined3d |
  | everything else | wined3d |

- An unmanaged build without `files/lib/wine/d7vk` can't do `d7vk`: setup falls back to
  the next renderer in the order with a warning, as a failed DXMT does on the Mac.
- `--renderer cnc-ddraw|d7vk|wined3d|dxvk|vkd3d-proton` and L3/L4 `renderer` override L1
  as on the Mac. A Mac-only id (`dxmt`, `d3dmetal`) in a Linux table is an error; at the
  top level it can't occur after §5.
- DXVK's own README warns that swapping D3D DLLs in online games may count as cheating
  (R20). Proton does the same for every Steam game; gogo documents it and does nothing
  special.
- Device choice (R18): DXVK/VKD3D pick a GPU themselves. `doctor` warns on a multi-GPU
  machine and names the fix: L4 `env = { DXVK_FILTER_DEVICE_NAME = "…",
  VKD3D_FILTER_DEVICE_NAME = "…" }`.

**As built (#172)** — choices the design left open (`macapp/renderer.rs`):

- **Where it lives.** `Renderer` gains `Dxvk`, `Vkd3dProton`, `D7vk`; `Renderer::on(os)`
  lists each OS's ids. `RendererRef::parse_on(s, os)` reads Linux refs: a Mac-only id is
  compat's error (`compat::mac_only_renderer`, the same check as `[platforms.linux]`),
  and `dxvk`/`vkd3d-proton`/`d7vk` with a version is an error too. `RendererRef::parse`
  stays the Mac's (same ids, same message). `select_on(os, …)` merges with `os`'s ids
  and its L3/L4 (`compat::db_entry_for` / `load_user_for` / `set_user_keys_for`);
  `select` stays the Mac lane's. A `--renderer` id the OS lacks warns and keeps the
  automatic choice.
- **L1 (`from_scan_linux`).** The class is what decided `graphics` (the static imports,
  or the runtime names when those decided). Inside `d3d_legacy`: `d3d9` → `dxvk`;
  `d3d8` without `d3d9` → `dxvk` with `Pick.d3d8` (→ `PROTON_DXVK_D3D8=1`); D3D ≤ 7
  alone → `d7vk` at every #173 confidence. Additions to the table:
  - **64-bit D3D ≤ 7 → wined3d.** GE-Proton builds D7VK for `i386-windows` only
    (`Makefile.in`), and `setup_ddraw` copies it into `syswow64`.
  - **Retained mode** is a `d3drm.dll` / `d3dxof.dll` import in any scanned file outside
    the game's own wrappers (or a runtime name, when those decided). It beats any
    immediate-mode evidence.
  - **The game's own `ddraw.dll`** (2D or D3D ≤ 7 class) → `wined3d` as the L1 pick,
    which does nothing (below), plus `ddraw=n,b`. That is the table's "none", and Proton's
    defaults stay, so a dgVoodoo `ddraw.dll` still reaches DXVK. A D3D8–12 game that
    ships a `ddraw.dll` keeps its class. A cnc-ddraw gogo placed earlier isn't counted
    (`pick_linux`, as on the Mac).
  - **An engine's implied class** (Clickteam: `d3d_legacy` with no API seen) → `wined3d`,
    with nothing set.
- **Reason text.** It is the Mac's `<what>: <seen> (<engine>, <bits> <exe>)`. The D3D ≤ 7
  rows add #173's wording for all three levels: "from a device/work GUID or a
  d3dim/d3drm import" or "from the renderer DLL d3ddrv.dll" (`strong`), "from interface
  GUIDs only" (`medium`), and "only from the dxguid GUID block: may be 2D"
  (`dxguid_block`). The Mac still says nothing for `strong`. The own-`ddraw.dll` row
  ends "…kept (ddraw=n,b): no cnc-ddraw, no D7VK"; DDrawCompat's "…stays unloaded (it
  doesn't run under Wine): Wine's builtin ddraw, no cnc-ddraw, no D7VK".
- **What setup does (`plan_linux`, pure; `apply_linux`, the game-dir step).** It returns
  `env`, `dll_overrides`, a `DdrawStep` (`none` / `place_cnc_ddraw` / `remove_gogo` /
  `keep_games`), notes and warnings, plus the ref in effect after any fallback:
  - `dxvk` and `vkd3d-proton` set nothing; `dxvk` adds `PROTON_DXVK_D3D8=1` for the
    D3D8 class.
  - `d7vk` sets `PROTON_USE_D7VK=1`. gogo adds no override: Proton appends `ddraw=n,b`
    itself.
  - `cnc-ddraw` places gogo's DLL (`ddraw::resolve_or_provision` + `ddraw::apply`) with
    `ddraw=n,b`.
  - `wined3d` sets `PROTON_USE_WINED3D=1`, except when it is `l1`'s own pick: the table's
    "nothing" rows keep Proton's defaults, so a packed D3D9 exe the scan missed stays
    on DXVK.
  - Every renderer but `cnc-ddraw` takes gogo's cnc-ddraw out (`ddraw::remove_gogo`).
    A game's own `ddraw.dll` is never touched, and gets `ddraw=n,b` unless it is
    DDrawCompat (#199).
- **Never both (R17).** `d7vk` on a game dir with its own `ddraw.dll` warns ("…would win
  over D7VK's: keeping the game's") and moves to the next renderer in the order.
  `apply_linux` checks again after its step: a `ddraw.dll` still in the dir (a failed
  removal, or a half-placed cnc-ddraw) drops D7VK the same way. A test runs every
  renderer × game dir (empty, gogo's, the game's) × build (with and without D7VK).
- **Fallbacks follow the order.** D7VK is checked at `files/lib/wine/d7vk/i386-windows/
  ddraw.dll`, the file GE-Proton11-12's `proton` (`setup_ddraw`) copies; without it
  Proton only logs and keeps Wine's `ddraw`. A missing D7VK, a cnc-ddraw with no game
  dir and a cnc-ddraw that can't be placed each warn and take `next_after` in `l1`'s
  order (D3D ≤ 7: `d7vk` → `cnc-ddraw`), skipping what was tried and ending at wined3d.
  An unparseable ref → wined3d.
- **The walk.** `next_after` is shared: a `current` of either OS's ids is found in the
  order (a ref from outside it restarts it, as before). `next_renderer_on(os, gogid,
  pick, current, dry_run)` writes the next ref into `[platforms.<os>]` of L4. The Mac's
  `setup::next_renderer` is unchanged. `linux-app` wires `apply_linux` and the walk
  (#176).
- **The Mac lane** keeps its table, ids, messages and JSON: `Pick.d3d8` is always
  `false` there and isn't serialized when `false`.

**docs/22 §7, placed (owner, 2026-11-03; O9):**

| docs/22 §7 item | Placement |
|---|---|
| 1. A real D3D≤7 corpus (download, `innoextract`, run the rule) | **Done (#174):** selective GUIDs in D3D≤7 exes; DirectX 6's 36-GUID block found, threshold 40 → 32; "block only" stays D7VK (above, "The GOG corpus") |
| 2. Does Linux need cnc-ddraw for 2D? | **Open, host pass** (Pharaoh, Stronghold, X11 and Wayland; §11.2). Default stays cnc-ddraw for confident 2D until then |
| 3. The D7VK pin | **Decided:** the D7VK the Proton build bundles; no separate D7VK download in the first round. Release zips come with the plain-Wine lane |
| 4. `syswow64` vs game dir for D7VK on plain Wine | **Deferred** with the plain-Wine lane (decision 1) |
| 5. Dual-renderer games | **Decided:** default D7VK (the table above), the cnc-ddraw override documented; L3 entries only after a host pass shows one needs it |
| 6. Wayland and D7VK (gamescope?) | **Deferred:** no gamescope by default in the first round. A host pass on Wayland records whether it's needed; running a game under a wrapper is a later feature |
| 7. DXVK-Sarek on MoltenVK | **Out of scope:** Mac lane (epic #50) |
| 8. Retained-mode GOG games | **Deferred:** the RM row needs no code (Proton's default). The corpus (item 1, #174) found none |

## 4. Decision 4: per-game fixes, worked out at app creation

**Settled (docs/21 §8 items 4, 5):** gogo always passes `GAMEID=umu-<id>` and
`STORE=gog`, and works out at creation which fixes the game gets and from where.

### 4.1 GOG → umu id

1. Read `protonfixes/umu-database.csv` in the chosen Proton build (managed or unmanaged).
   A row with `STORE=gog` and `CODENAME=<gogid>` gives the umu id. This matches the
   fixes that build can run, needs no network and sends nothing anywhere.
2. Only when the build has no CSV (an unmanaged build without `protonfixes/`): the
   cached copy of umu-database's CSV from GitHub (below).
3. No row anywhere: `GAMEID=umu-<gogid>`. `gamefixes-gog/default.py` still runs, and
   Valve's per-appid hacks key on a GOG id no Steam appid reaches (R15, accepted). To
   verify in the first host pass.

The id is resolved at build and by setup (a GE-Proton bump can add a row), and written
as `UMU_ID` in `launch.conf`. gogo never uses the online API: it lags the CSV, and it
would send the library to a third party one game at a time. The CSV is GPL-3.0: gogo
reads it in place or downloads it into the user's cache, and never vendors it (R13).

**Settled (owner, 2026-11-03; O4) — the GitHub fallback:**

- Follows **`main`**:
  `https://raw.githubusercontent.com/Open-Wine-Components/umu-database/main/umu-database.csv`.
  The fallback only serves builds whose fixes gogo can't see anyway, so the freshest ids
  are worth more than reproducibility. A pin would go stale between gogo releases.
- Cached as `$XDG_CACHE_HOME/gogo/umu-database.csv` with its `ETag`. Refreshed when
  older than **7 days**, with a conditional GET (`If-None-Match`), and only when a lookup
  needs it. `--refresh` on the build forces it.
- Fetch failure: use the stale copy and warn. No copy and no network: rule 3, with a
  warning. A lookup never fails a build.

**As built (#170)** — choices the design left open:

- **Code.** `gogo_core::linuxapp::umu_id`. `lookup(build, gogid, refresh)` takes the
  Proton dir (`store::proton_dir()`, or a `path:` build) and returns a `Lookup`: an
  `UmuId { id, source }` (`Source::Build { csv }`, `Cache { csv }` or `Fallback`) plus
  the warnings for the caller to print. `UmuId`'s `Display` is the summary line:
  `umu-910830 (umu-database.csv in GE-Proton11-12)`, `… (umu-database.csv from GitHub,
  cached)`, `umu-<gogid> (no umu-database row: the GOG id)`. `refresh` is the
  parameter `--refresh` will pass (#176).
- **The CSV.** The real header (checked 2026-11-04 against `main`) is `TITLE, STORE,
  CODENAME, UMU_ID, COMMON ACRONYM (Optional), NOTE (Optional), EXE_STRINGS
  (Optional)`. Columns are found by name (trimmed, case-insensitive), not position; a
  CSV without `STORE`, `CODENAME` or `UMU_ID` counts as unreadable. `STORE` is compared
  trimmed and case-insensitively to `gog`; `CODENAME` must equal the GOG id exactly
  (trimmed). A matching row whose `UMU_ID` isn't `umu-<something>` is skipped with a
  warning.
- **Several matching rows:** the first in file order wins. If the others give a different
  id, a warning names them. `main` has none today (212 GOG rows, 212 distinct ids).
- **Parser: gogo's own, no crate.** A small RFC 4180 reader (quoted fields with commas,
  `""` and line breaks; LF or CRLF; a leading BOM dropped; blank lines skipped). The file
  needs nothing more, and the `csv` crate would add two crates (`csv`, `csv-core`)
  for what is about 60 lines here. Checked once on the real file: all 212 GOG rows read the
  same as Python's `csv` module, and Rebel Galaxy Outlaw resolved to `umu-910830`.
  Nothing of it was committed (R13).
- **Rule 1 vs rule 2.** A build CSV that exists but can't be read (I/O error, no usable
  header) is warned about and treated as absent, so rule 2 applies. A readable build CSV
  with no row is rule 3 at once, with no fetch and no warning: most GOG games simply
  have no entry. `refresh` only matters when rule 2 is reached.
- **Cache location.** `state::cache::cache_dir()`, like the product cache:
  `$XDG_CACHE_HOME/gogo/umu-database.csv` (`~/.cache/gogo/…`), or `$GOGO_HOME/cache/…`
  when `GOGO_HOME` is set. The ETag is `umu-database.csv.etag` beside it.
- **Age** is the CSV's mtime. A 304 sets it to now, a 200 writes the file (temp file +
  rename) with now as its mtime; a timestamp in the future counts as fresh. The
  conditional GET is sent only when there is both a copy and a stored ETag; `refresh`
  still sends it (a 304 is a valid answer to a forced check).
- **What a 200 must be.** The body must have the three columns, or it's a failed fetch
  (a captive portal's page never replaces a good copy). A response without an `ETag`
  removes the stored one, so the next refresh is unconditional.
- **Network.** One GET with gogo's user agent, a 10 s connect and 30 s total timeout, no
  retries: a failure has the stale copy or rule 3 behind it. Errors are reported
  without the URL.
- **Warnings** start with why GitHub was asked: "<build> has no
  protonfixes/umu-database.csv (typically an unmanaged build without protonfixes), so
  its umu id comes from umu-database's CSV on GitHub", then either "refreshing it failed
  (…): using the cached copy from N day(s) ago" or "fetching it failed (…) and there is
  no cached copy: using umu-<gogid>, gogo's fallback".

### 4.2 Reading the protonfix

1. Look for `protonfixes/gamefixes-gog/umu-<id>.py` in the chosen build (following
   symlinks into `gamefixes-steam`, as Python's import does).
2. Read it **without running it**: a static reader of `util.*` calls over the module's
   text. It finds `protontricks(<verb>)`, `set_environment(<k>, <v>)`,
   `winedll_override(<dll>, <mode>)`, `regedit_add(…)`, `replace_command(…)`,
   `append_argument(…)`, `set_ini_options`/`set_xml_options`. Literal arguments only.
   Anything else (a call with a computed argument, a call inside a condition, other
   code) is listed as "other code: may do more at launch". The reader is pattern
   matching on the source; it takes no Python parser dependency unless pattern matching
   proves too weak (a decision for the implementation issue). R29 is accepted with that
   label.
3. Record the module's sha256 (or "none") in `state.toml` as `protonfix`. Setup compares
   it: a GE-Proton bump that adds, changes or removes the fix re-runs the summary and
   the verb pre-install (below) and logs the change.

**As built (#171)** — the reader, where the design left choices:

- **Code.** `gogo_core::linuxapp::protonfix`. `read_module(build, umu_id)` and
  `read_default(build)` return a `Module` (path, symlink target, sha256, `Reading`) or
  `None`. Like Python's finder, a package (`<name>/__init__.py`) wins over `<name>.py`,
  symlinks are followed and a dangling one counts as no module. An id that isn't
  `umu-` plus letters, digits, `-` and `_` has no module (no path tricks).
- **Pattern matching, no parser.** A small lexer (about 200 lines) makes logical
  lines: string literals (all quote forms and prefixes, escapes), comments, brackets,
  `\` continuations and `;`. Statements are then matched by shape. Over the 79 modules of
  GE-Proton11-12's `gamefixes-gog` (umu-protonfixes `28a02c25`, read once, nothing
  committed), **55 read fully and 24 have other code**. Every one of those 24 is real
  logic (file checks, downloads, `main_with_id` tables, `disable_esync()`-style
  helpers), not a pattern the reader missed. A Python parser crate wouldn't read any of
  them either: the gap is in what the modules do, not in their syntax. So no
  dependency.
- **What runs.** As `fix.py` does: `early_with_id` or else `early`, then
  `main_with_id` or else `main`. Only those bodies are read. A statement directly in
  one of them is a fix when it's `util.<call>` of a known kind with literal arguments.
  Imports, `pass`, bare strings (docstrings) and `log.*` calls do nothing and are
  skipped. Anything else is other code, listed as `line N: <statement>` (cut at 100
  chars; a compound statement by its header, `if x:`): computed arguments, calls
  inside `if`/`for`/`with`, unknown `util` calls (`disable_esync()`), calls to local
  helpers. So is any module-level statement that isn't an import, a `def`, a bare
  string or `NAME = '<string>'`.
- **Literals.** Strings (adjacent ones joined; an f-string only without fields;
  bytes are computed), `True`/`False`/`None`, `[util.]OverrideOrder.*` and
  `[util.]BasePath.*`, and module-level string constants. Positional and keyword
  arguments bind by the real parameter names, `set_xml_options`' misspelt
  `base_attibutte` included. A `match_flags` on `replace_command` makes it other code.
  `util` may be aliased (`from protonfixes import util as u`, `from protonfixes.util
  import protontricks`).
- **Summarised as.** `verb X`, `env K = "v"`, `dll_overrides d = "n,b"`
  (`OverrideOrder.DISABLED` is `""`, shown as disabled), `registry <key> <name> =
  <type> "<data>"`, or `registry <key> (key only)` when name, type and data aren't all
  given (`util.py` then only creates the key). `/reg:64` is noted as the 64-bit view.
  Then `replace_command "a" -> "b"` and `args += "x"`. `set_ini_options` becomes
  `ini <file>: [SECTION] k=v, k=v; [NEXT] …` (comments and blank lines dropped, a
  non-game `base_path` noted). `set_xml_options` becomes `xml <file>: "<line>" after
  each line containing "<anchor>"`. The encoding isn't shown.
- **The hash.** `state_value(module)` is the sha256 of the module's bytes or `none`.
  `compare(stored, now)` gives `Unrecorded`, `Unchanged`, `Added`, `Changed` or
  `Removed`; `rerun()` is true for all but `Unchanged`, and its `Display` is the log
  line. A Linux `state.toml` doesn't exist yet: #176/#177 store it as `protonfix`.
- Out of scope: `~/.config/protonfixes/localfixes/`. A localfix replaces the module at
  launch, but it belongs to the user and every umu client, so gogo doesn't read it.

### 4.3 One summary

The build plan, `gogo linux-app compat <gogid>` and `doctor` show one list: each fix,
its value, and where it comes from (`protonfix umu-<id>`, `L3 compat DB`, `L4 override`,
`L1 auto`), or "no known fixes". `default.py`'s `OPENSSL_ia32cap` is shown once as
"protonfix default (every GOG game)".

**Verbs run at setup, not at launch (R12).** Setup runs the effective verbs (L1, L3, L4)
**plus the protonfix's literal `protontricks` verbs** through `umu-run winetricks
<verbs>`, the Proton build's own winetricks, against the game's prefix. At launch
protonfixes finds them installed and doesn't download (it checks the prefix's
`winetricks.log` before running a verb: reported, to verify in the first host pass). So
launch-time network use for verbs is limited to what the static reader couldn't see.

**Settled (owner, 2026-11-03; O2, O3) — collisions and ownership:**

- **O2, R10: an L3/L4 value that a protonfix also sets.** protonfixes assigns its env
  values unconditionally after gogo's env, so the protonfix wins on a collision.
  Decided: **warn, with an explicit per-game opt-out.**
  - The summary marks the collision (`env DXVK_ASYNC: L4 "0" is overridden at launch by
    protonfix umu-<id> ("1")`), at build, at `--retarget`, in `compat` and in `doctor`.
  - A new compat key, `protonfixes = false` (per platform, §5), makes the launcher export
    `PROTONFIXES_DISABLE=1` for that game. The summary then lists what the game loses
    (the module's verbs, env, overrides, `default.py`). gogo never sets it on its own.
  - Not chosen: writing a protonfixes `localfix`. `~/.config/protonfixes/localfixes/` is
    shared with every umu user on the machine (Heroic, Lutris), and a localfix replaces
    the whole module, so gogo would be editing other tools' behaviour. Not chosen
    either: `PROTONFIXES_DISABLE` automatically whenever L4 has `env`, which throws away
    every fix for one value.
- **O3, R11: does L3 keep fixes that protonfixes already covers?** Decided: **one owner
  per game and platform, decided at run time, per file.** `source` is one top-level key
  per file (each of today's files has exactly one), so the rule takes or skips a whole
  L3 file, not single fields. On Linux, when the chosen build has a `gamefixes-gog`
  module for the game and the game's L3 file has `source = "umu-protonfixes:…"`, its
  top-level fixes (`verbs`, `env`, `dll_overrides`, `registry`, `args`, `exe`,
  `replace_command`) are not applied there; the summary shows them as "covered by
  protonfix umu-<id>". Its `[platforms.linux]` table, if any, still applies (`status`,
  `tested`, `notes`). When the build has no module (an unmanaged build, or a Steam fix
  with no `gamefixes-gog` symlink), the whole file applies. The Mac lane always applies
  it: no protonfixes runs there. So I6 needs no per-field provenance, only the file's
  `source`. A Linux fix that doesn't come from protonfixes goes into an L3 file with
  another `source` (`gogo-host`); a game that needs both kinds is a schema question for
  when one shows up.
  - Example: Stronghold HD's `directplay` comes from `gamefixes-steam/40950.py`. Under
    `STORE=gog` protonfixes only looks in `gamefixes-gog`, so the L3 verb applies on
    Linux unless the build adds a GOG module for it.

**As built (#171)** — the summary, collisions and ownership:

- **Code.** `gogo_core::linuxapp::fixes`. `resolve(Inputs { umu_id, module, default,
  db, layers })` takes the L3 file as text (`compat::db_raw`), so the ownership rule
  can split its top level off, and the other layers resolved for Linux. It returns
  `Fixes`: the effective config, the covered L3 fixes, both modules and the L3
  warnings. `Fixes::summary()` builds a `Summary` (items with source and state, other
  code, the `protonfixes = false` layer), and its `Display` is the text. The plan,
  `compat` and `doctor` print it (#176, #178).
- **The summary**, one line each, in this order: the module (`protonfix umu-40950:
  gamefixes-gog/umu-40950.py -> gamefixes-steam/40950.py (sha256 0123456789ab)`, or
  `none in this Proton build`); gogo's fixes from the effective config
  (`renderer`, `exe`, `variant`, `args`, `replace_command`, `verbs`, `dll_overrides`,
  `env`, `registry`) as `<fix>  (<L1 auto|L3 compat DB|L4 override|flags>)`; the L3
  fixes the protonfix covers; `default.py`'s, as `protonfix default (every GOG game)`;
  the module's; then `other code in <module> (may do more at launch):` and the
  statements. `no known fixes` when nothing applies and there's no other code.
- **Collisions.** These are env keys, DLL overrides (case-insensitive, Wine's `*` and
  `.dll` dropped) and registry values (key and name case-insensitive, hives
  abbreviated). A gogo value is marked when the protonfix that runs last (the game's,
  then `default.py`) sets the same key to a different value: `env DXVK_ASYNC: L4 "0"
  is overridden at launch by protonfix umu-910830 ("1")`. Same value: no mark. The
  protonfix's DLL override is appended to `WINEDLLOVERRIDES` after gogo's, so it's
  counted as winning too.
- **`protonfixes = false`.** `compat::merge` puts `PROTONFIXES_DISABLE = "1"` into the
  effective env at the layer of the winning `protonfixes = false`, unless a higher
  layer sets that env key itself. It's in `compat.rs`, but `parse` drops
  `protonfixes` on macOS, so the Mac lane never gets it. The summary then lists, under
  `protonfixes = false (L4 override): PROTONFIXES_DISABLE=1, so the game loses:`,
  every fix of both modules and their other code. Collisions aren't marked then.
- **Ownership.** `compat::TOP_FIX_KEYS` (`verbs`, `dll_overrides`, `registry`, `args`,
  `exe`, `replace_command`) are what's skipped. `env` is OS-specific, so a top-level
  one is ignored anyway; `title`, `variant` and `source` stay. The rule needs a module
  for the game in the chosen build, an L3 `source` (as resolved for Linux) starting
  with `umu-protonfixes:`, and protonfixes **not** turned off: with
  `protonfixes = false` nothing else owns the game, so L3 applies whole. Covered
  fixes show as `verb directplay  (L3 compat DB: covered by protonfix umu-40950, not
  applied)`. Stronghold HD (`compat/1207658712.toml`) has no `gamefixes-gog` module
  in GE-Proton11-12, so its `directplay` applies (tested against the real file).
- **Verbs at setup (R12).** `Fixes::setup_verbs()` is the effective verbs in order,
  then `default.py`'s and the module's literal `protontricks` verbs, without
  duplicates and with none of the protonfix's when it's off.
  `pending_verbs(prefix, verbs)` drops those already in the prefix's
  `winetricks.log`. umu 1.4.4's winetricks mode exits 1 **without installing
  anything** when any verb it's given is already there (`is_installed_verb`), so
  setup must pass only the missing ones. `winetricks_command(umu, proton, prefix,
  umu_id, verbs)` builds `umu-run winetricks <verbs>` through `UmuRun::command()`
  with `WINEPREFIX`, `PROTONPATH`, `GAMEID`, `STORE=gog` and `PROTONFIXES_DISABLE=1`
  (umu adds `-q`). It's `None` when there's nothing to install. The disable is there
  because the winetricks call is a `waitforexitandrun` Proton run, which would
  otherwise run the protonfix too. Running the command is #176/#177's setup.

## 5. Decision 5: `[platforms.<os>]` in the compat schema

**Settled (docs/21 §8 item 6):** one file per game. Top level = every OS (the Windows-side
facts); `[platforms.macos]` / `[platforms.linux]` = per OS. OS names follow Rust's
`target_os`.

| Where | Keys |
|---|---|
| top level | `title`, `exe`, `args`, `variant`, `replace_command`, `verbs`, `dll_overrides`, `registry`, `source` |
| `[platforms.<os>]` | `renderer`, `runtime`, `env`, `status`, `notes`, `tested`, `lane`, `lanes`, `protonfixes` (Linux only), plus any top-level key to override or extend it there |

- The platform table merges over the top level by design 16 decision 5's rules (maps per
  key, `verbs` add, `-<verb>` removes, `-*` clears, everything else replaced whole), and
  the result is that layer's entry. Layers then merge as before: `flags > L4 > L3 > L2 >
  L1 > L0`.
- `renderer`, `runtime`, `env` and `protonfixes` at the top level warn ("only valid under
  `[platforms.<os>]`") and are ignored, like an unknown key. A table for another OS is
  parsed and checked but not applied.
- `gogo-launch.toml` doesn't change shape: it's per machine, so it holds the effective
  values for one OS. `schema` stays 1 there. The compat file gets no schema number (it
  never had one); the loader tells old and new apart by the keys.
- gogo's L4 writes (`--retarget`, persisted build flags, "Try next renderer") go into
  `[platforms.<this os>]` (`toml_edit`, comments kept).

**Settled (owner, 2026-11-03; O5) — migration:**

- **L3, one PR, all 11 files.** `title`, `verbs`, `source` stay at the top; `status`,
  `notes`, `tested`, `lane` and `[lanes.installer]` move under `[platforms.macos]`
  (`[platforms.macos.lanes.installer]`). No `[platforms.linux]` tables are added until a
  Linux host pass has something to record. A test merges every file for `macos` before
  and after and asserts the same effective entry. `compat/README.md` documents the new
  shape. (docs/21 §8 item 6 says 13 files; there are 11.)
- **L4 files already on users' Macs** hold `renderer`/`runtime` (written by gogo) and
  maybe `env`/`status` at the top level. The loader reads those legacy top-level
  OS-specific keys as `[platforms.macos]` on macOS, with a one-line note, and ignores
  them on Linux with a warning. gogo moves the keys it owns into `[platforms.macos]`
  the next time it edits the file; it never rewrites the user's own keys.
- *#166, as implemented* (`macapp/compat.rs`), where the above leaves a choice:
  - "The keys" means per key, not per file: in L4 the eight pre-#166 OS keys
    (`renderer`, `runtime`, `env`, `status`, `notes`, `tested`, `lane`, `lanes`) at the
    top level are always legacy, even when the file has `[platforms.*]` tables, so a
    user's top-level `env` keeps working after gogo moved its own keys. A
    `[platforms.macos]` value wins over a legacy one. `protonfixes` was never a top-level
    key, so it only warns there.
  - In L3 every `[platforms.<os>]`-only key at the top level warns and is ignored (not
    just `renderer` / `runtime` / `env` / `protonfixes`): the 11 files are migrated, and
    a top-level `status` would otherwise claim a Mac result for Linux.
  - `protonfixes` under `[platforms.macos]` warns ("Linux only") and is ignored.
    Unknown OS tables (`[platforms.windows]`) warn like an unknown key.
  - gogo's write moves the legacy `renderer` / `runtime` under `[platforms.macos]` on
    Linux too (they are Mac values), and drops one that `[platforms.macos]` already sets
    (it never applied). It moves them only when the write changes a value ("no change,
    no write" stays). Emptied `[platforms.<os>]` tables are removed.

```toml
# compat/1435582019.toml — Rebel Galaxy, after migration
title  = "Rebel Galaxy"
verbs  = ["d3dcompiler_43", "d3dx11_43", "d3dx9_43", "xinput"]
source = "gogo-host"

[platforms.macos]
status = "broken"
notes  = "unplayable: DXMT loads ~4 min per launch then runs at slideshow speed (32-bit on WoW64), wined3d garbles the 3D; play it elsewhere"
tested = [
  { runtime = "winecx/24.0.7_7+dxmt-v0.80", host = "m4pro/macos-27.0.1", date = 2026-10-23 },
  { runtime = "winecx/24.0.7_7", host = "m4pro/macos-27.0.1", date = 2026-10-23 },
]
```

## 6. Decision 6: store and state on Linux

**Settled (owner, 2026-11-03; O7) — paths:**

| What | Linux | macOS (unchanged) |
|---|---|---|
| data root: `runtimes/`, `renderers/`, `games/`, `compat/` (L4) | `$XDG_DATA_HOME/gogo` (`~/.local/share/gogo`) | `~/Library/Application Support/gogo` |
| logs | `$XDG_STATE_HOME/gogo/logs/<gogid>/` | `~/Library/Logs/gogo/<gogid>/` |
| `.desktop` | `$XDG_DATA_HOME/applications/gogo-<gogid>.desktop` | — |
| umu-database fallback | `$XDG_CACHE_HOME/gogo/umu-database.csv` | — |
| config, tokens, `state.json` | design 06, unchanged | unchanged |

- **Why the data dir and not the state dir:** prefixes hold saves, and runtimes are large
  downloads users expect under `~/.local/share` (umu, Steam, Lutris all keep theirs
  there). XDG's state dir is for logs and history. Today's code puts `runtimes/`,
  `games/` and `compat/` under `state_dir()` on every OS; on macOS `state_dir()` falls
  back to the data dir, so only Linux moves. Nothing on Linux lives there yet
  (`runtimes install` and depot materialization refuse off macOS), so there's nothing to
  migrate. One function (`data_root()`) replaces those `state_dir()` calls.
- `GOGO_HOME` still flattens everything into one dir.
- **Catalog entries gain `os`.** `Arch` gets Linux values (`x86_64`, `aarch64`), and
  `runtimes`/`renderers list|install` show only this OS's entries (`--all` shows the
  rest, not installable). `runtimes install` and `renderers install` work on Linux; the
  "macOS-only — feed `gogo mac-app`" refusal goes.
- **Linux catalog, first round:** `ge-proton/<newest>` (x86_64), `umu/1.4.4`,
  `cnc-ddraw/7.1.0.0` (the Windows DLL, the same entry as the Mac's, `os = ["macos",
  "linux"]`). Winetricks isn't fetched on Linux: the Proton build's own runs the verbs.
- **Archive formats:** `store.rs` already unpacks `.tar.gz` (GE-Proton) and `.zip`
  (cnc-ddraw). Plain `.tar` (the umu zipapp archive) is added. zstd (VKD3D-Proton releases) waits for the plain-Wine lane (R24, deferred).
- **GC** is design 16 decision 6 as written: `state.toml` and L4 files are the reference
  list; the sweep keeps N-1 per used id. Unmanaged builds and anything in a shared
  tools dir are never deleted (§2.2). A GE-Proton is 1.5 GB unpacked, so `runtimes list`
  shows sizes, and the sweep reports what N-1 costs (R5).
- **Game files from a depot build.** Depot materialization (`gog/depot.rs`) runs on Linux
  too. `games/<gogid>/game` is made with `FICLONE` (reflink) where the filesystem
  supports it (btrfs, XFS, bcachefs), else a plain copy. The plan shows which, and the
  extra disk a copy costs. `gogo depot`'s help loses "macOS only for now".

- **As built (#175)**:
  - Why `depot` said "macOS only": one gate (design 14 D7) in `depot::run`, nothing
    else. gog-warp's downloader, the free-space preflight and the cache path all work on
    Linux, so the gate and `DepotError::UnsupportedPlatform` are gone and `gogo depot`
    downloads on Linux into `$XDG_CACHE_HOME/gogo/depot/<id>`.
  - The code is `gog/materialize.rs` (beside `depot.rs`): `materialize_tree(src, dst,
    Reflink::Try|Never)` and `plan_tree(src, game_dir, tree_bytes)`. The macOS lane keeps
    `bundle::clone_tree` (`cp -c`, clonefile) unchanged; the copy walk is portable unix
    code and is tested on both OSes, only the ioctl is Linux-only.
  - `FICLONE` through `rustix::fs::ioctl_ficlone` (safe API; rustix was already in
    `Cargo.lock` through zbus, now a Linux-only direct dep with `fs`).
  - **Per file, with a per-tree probe for the plan.** Each regular file tries `FICLONE`
    and falls back to a copy on its own: the destination is one fresh dir, but the cache
    tree may cross a mount, and a failed ioctl costs microseconds. The plan can't walk a
    tree that isn't downloaded yet, so it probes once: a 4 KiB file in the nearest
    existing ancestor of the cache root and of `games/<gogid>/game`, cloned and removed
    (any error reads as "copy", so a plan may overstate the cost, never understate it).
    A dry run therefore writes and removes two probe files, and creates no directory.
  - The copy keeps what `cp -Rp` keeps on the Mac path: modes (dirs set after their
    children, so read-only dirs work), mtimes, symlinks as symlinks (relative and
    dangling ones too); the root is followed. Other file types are an error. A copy is
    `io::copy` (copy_file_range); where that still shares extents (NFS server-side copy)
    the report counts it as a copy.
  - The plan: `DepotEvent::Plan` gains `gameFiles: {method, gameDir, extraBytes}` on
    Linux only (`None`, not serialized, on macOS). `gogo depot` prints `game files:
    reflink into <dir> (no extra disk)` or `game files: copy into <dir> (+<size> disk: no
    reflink between the cache and the data dir)`; `extraBytes` is the manifest's disk
    size for a copy. `linux-app` (#176) calls `materialize_tree` and can report its
    `MaterializeReport` (reflinked/copied files and bytes).
  - Tests: the forced copy path (`Reflink::Never`), and FICLONE end to end in
    `CARGO_TARGET_TMPDIR`, which skips with a `SKIP …` line where that filesystem has no
    reflink (`/tmp` is often tmpfs; CI runners are ext4). `.config/nextest.toml` shows
    that test's output on success so the skip is visible.

- **As built (#167)** — choices the design left open:
  - `os` is stated on every catalog entry (`os = ["macos"]` for every Mac one), with no
    default. The catalog is static Rust data, so there is no old data a default would
    keep valid; a default of macOS would let a Linux entry that forgot `os` pass as a
    Mac one.
  - `list --all` rows for another OS read "macOS only, not installable on Linux";
    `--json` rows gain `os` and `installable`. `install` of another OS's entry exits 2:
    "winecx26 is for macOS only: it doesn't install on Linux". The check is in the CLI;
    `store::install` stays OS-agnostic.
  - cnc-ddraw keeps the Mac x86_64 arch (it's a Windows DLL); a Linux `x86_64` runtime
    takes Mac-x86_64 renderer builds, and listings label it plain `x86_64` on Linux.
  - Logs: `state::logs_root()` (`<state_dir>/logs` on Linux, so `GOGO_HOME` flattens
    it) feeds doctor and `apps`. The launcher script still writes
    `~/Library/Logs/gogo`; pointing it at the Linux path is #177's (done: the Linux launcher, §7.5).

## 7. Decision 7: the launcher

**Settled (docs/21 §8 item 9):** `~/.local/share/applications/gogo-<gogid>.desktop` runs
`games/<gogid>/launch.sh`, generated by gogo and never edited by hand, which sources
`games/<gogid>/launch.conf`. gogo does setup.

### 7.1 Files

- **`gogo-launch.toml`** (design 16 §1.1, same schema): written at build into
  `games/<gogid>/`. Its `runtime` is `ge-proton/<ver>` or `path:<dir>`, its renderer a
  Linux id. `[gogo] path` is gogo's invoked path, as on the Mac.
- **`launch.conf`**: only assignments, POSIX single-quoted by `sh_quote`, written by setup.
  Keys: `GOGID`, `TITLE`, `UMU_ID`, `STAMP`, `UMU_RUN`, `UMU_PYTHON`, `PROTON_DIR`,
  `PROTON_VERSION`, `PROTON_SHA`, `PREFIX`, `GAME_DIR`, `WORKDIR`, `EXE`, `ARGS=(…)`,
  `ENV=(…)`, `DLL_OVERRIDES`, `RENDERER`, `PROTONFIXES` (`1`/`0`), `UMU_FOLDERS` (when
  needed), `GOGO_PATH`, `DATA_ROOT`. There's one copy: no bundle to bake a second into.
- **`launch.sh`**: bash, generated from a template inside gogo. It carries
  `LAUNCHER_VERSION`; setup rewrites it whenever the template changed, so a gogo upgrade
  reaches existing games at their next setup (R30).
- **The `.desktop` file**: `Type=Application`, `Name=<title>`, `Exec=<absolute path to
  launch.sh>` (quoted per the Desktop Entry spec), `Icon=` the game's icon as PNG
  (`games/<gogid>/icon.png`, from the exe's icon resource or GOG's `.ico`),
  `Categories=Game;`, `X-Gogo-Gogid=<gogid>`. `apps remove` deletes it.

**Settled (owner, 2026-11-03; O7) — relative paths:** paths under the data root are written
relative to it (`PREFIX='games/1207659013/prefix'`, `GAME_DIR`, `UMU_RUN` and
`PROTON_DIR` for store builds), as on the Mac. The launcher resolves them against
`DATA_ROOT` if that dir exists, else `${XDG_DATA_HOME:-$HOME/.local/share}/gogo`. Absolute:
a `PROTON_DIR` in a shared tools dir or an unmanaged build, a `system` umu, `UMU_PYTHON`,
`UMU_FOLDERS`, `GOGO_PATH`. The log dir isn't written at all: the launcher computes it
from `$XDG_STATE_HOME` at run time (`GOGO_LOG_DIR` overrides), as the Mac computes
`~/Library/Logs`. The `.desktop` file is the one absolute pointer; setup rewrites it when
the data root moved.

**As built (#176)** — the files setup writes (`linuxapp/game.rs`, `linuxapp/setup.rs`):

- **`launch.conf`** (`LaunchConf::render`): a one-line header comment, then the keys in
  the order above (golden file `tests/fixtures/linuxapp/launch.conf.golden`; bash sources
  it back). `EXE` and `WORKDIR` are unix paths relative to the data root, through the
  prefix's `drive_c/GOG/<name>` link into `game/`
  (`games/<gogid>/prefix/drive_c/GOG/<name>/game.exe`): with `z:` gone, Wine maps them to
  `C:\`. `GAME_DIR` is `games/<gogid>/game`, or the installed dir in the prefix (installer
  lane). `UMU_FOLDERS` isn't written yet: nothing sets it. `UMU_PYTHON` comes from
  `[linux] umu` again on every setup.
- **`state.toml`** (`LinuxState`, schema 1): `gogid`, `title`, `runtime`, `renderer`,
  `umu_id`, `protonfix` (§4.2's hash or `none`, compared with `protonfix::compare`),
  `verbs`, `registry`, `stamp`, `gogo_version`. No `bundle`. `store::references` reads
  its `runtime`/`renderer`, and the L4 file's `[platforms.linux]` table, so GC keeps the
  GE-Proton a Linux game uses.
- **The stamp** is the Mac's: sha256 of `gogo-launch.toml` and the L4 file.
  `linuxapp::game::current_stamp(gogid)` computes it for #177's launch-time check.
- **What's baked and what isn't.** `gogo-launch.toml` bakes L2 (exe, args, variants), the
  L1 verbs, the runtime (flag > L3 > `[linux] default_runtime` > pin) and the renderer the
  build selected. L3's `exe`/`args`/`env`/`dll_overrides`/`registry`/`verbs` are not
  baked: setup applies them, because §4.3's ownership rule depends on which Proton build
  is chosen.
- **Setup's order**: read `state.toml`, the TOML and L4 → the runtime (L4, else baked;
  a missing GE-Proton installed) → umu → the umu id → the prefix (`umu-run ""`: success
  is `system.reg` and `drive_c/windows`, not `drive_c`, which umu's own `setup_pfx` makes
  before Proton runs) → home links made real, the game linked → L1 from a fresh scan
  → the fixes (L1 verbs, the renderer at its layer, L3 as owned, L4) → the protonfix
  compared (a change is a note) → the renderer (`apply_linux`) → registry values
  (`umu-run reg add`, only for a new prefix or changed values) → pending verbs only
  (`fixes::pending_verbs`; `z:` back for the run; anything still missing warns) →
  `localize` (home links made real; `dosdevices/` keeps `c:` and `z:`, §7.2) →
  `launch.conf`, `state.toml`, each written only when its bytes changed (a
  setup that changes nothing runs nothing through umu and rewrites nothing).
- **Every umu run** gets `WINEPREFIX`, `PROTONPATH`, `GAMEID`, `STORE=gog`,
  `PROTONFIXES_DISABLE=1` and `games/<gogid>/` as its working dir; its output goes to
  `games/<gogid>/install/setup.log`. A data root, installer, prefix, Proton build or
  umu-run under `/tmp` is refused ("private /tmp") before anything runs.
- **`z:` comes back on every Proton run (real smoke, 2026-11-04).** GE-Proton11-12's
  `proton` script recreates `dosdevices/z: -> /` whenever it's missing (its prefix setup,
  every run). So §7.2's fallback is the decision: setup keeps `z:` (and, until a prefix
  set up before had it back, `restore_z` still puts it there for steps that hand Wine unix
  paths), and drops the rest. The other drives that came back are §7.2's "Host drives".
- **wined3d**: chosen by L3, L4, a flag or the walk, it adds `PROTON_USE_WINED3D=1` (the
  env and overrides the renderer sets fill gaps only); as L1's own pick it sets nothing.
  The plan line and setup's notes say which.

### 7.2 What the launcher does

1. Source `launch.conf`. No file: a dialog and exit 1 (as design 16 Open 1).
2. Compute the stamp (design 16 §2.1: `sha256sum` over `gogo-launch.toml`, then the L4
   file or zero bytes). Stale, or no prefix: run `gogo linux-app setup <gogid>`
   (`GOGO_PATH`, then `PATH`) and re-source. No gogo: log it and continue with the last
   setup. Setup failure: as on the Mac (logged, last applied config runs).
3. The runtime check of §2.2.
4. Export `WINEPREFIX` (always: umu's default `~/Games/gog` is shared by every game, R7),
   `GAMEID=$UMU_ID`, `STORE=gog`, `PROTONPATH=$PROTON_DIR`, the `ENV` entries,
   `WINEDLLOVERRIDES`, `PROTONFIXES_DISABLE=1` when `PROTONFIXES=0`. Renderer switches
   (`PROTON_USE_D7VK`, `PROTON_USE_WINED3D`, `PROTON_DXVK_D3D8`) are already in `ENV`:
   setup put them there.
5. `cd "$WORKDIR"`, run `"$UMU_PYTHON" "$UMU_RUN" "$EXE" "${ARGS[@]}"` (or `"$UMU_RUN" …`
   for a system umu), tee its streams into the run log (§7.3). The launcher stays its
   parent.
6. Early exit: the dialog (§7.4).

**Teardown.** On `INT`/`TERM`/`HUP`, `TERM` to umu-run's process group, then, after 5 s,
`SIGKILL` to every process whose `/proc/<pid>/environ` has this `WINEPREFIX` (Linux lets a
user read their own processes' environment, which beats design 16's cwd heuristic inside
the container). The Mac's watchdog (#103) and `.launchers/<pid>` records carry over
unchanged. Inside the container Wine's processes carry `WINEPREFIX=<prefix>/pfx/`, not
`<prefix>`, and `TERM` doesn't end them within the 5 s: the `SIGKILL` sweep does the work
(real smoke, §7.5).

**Prefix isolation (design 16 decision 4).** Proton's prefix has a `steamuser` home with
real directories, not links into `~`, and umu links `users/<user>` and `steamuser` to each
other (relative). Setup applies decision 4's rule (no absolute links under
`drive_c/users/`). **Settled (owner, 2026-11-04, from #176's real smoke): `z:` → `/`
stays on Linux.** GE-Proton's `proton` script recreates it on every run, so removing it
would hold only between runs. Setup keeps `c:` and `z:` (`linuxapp::setup::KEPT_DRIVES`)
and still drops every other drive letter (Wine's `x:` → `$HOME`, `com1`…); `doctor`'s
`links` check (#178) skips `z:`. macOS is unchanged: setup removes `z:` there.

**Host drives (#199).** On a real run `dosdevices/` grew `s: -> /home`, `v: -> /run/media`,
`x: -> $HOME` and `com1`–`com4`, and `doctor` failed `links` after every launch. Their
sources, read from GE-Proton11-12:

| Links | Made by | When |
|---|---|---|
| `s:` | `proton`'s `setup_game_dir_drive`, from umu's `STEAM_COMPAT_LIBRARY_PATHS` (hence the log's `unable to use parent for game drive, path /home`) | prefix setup and update, verb `run` |
| `u:` `v:` `w:` `x:` (`/media`, `/run/media`, `/mnt`, `$HOME`, each if readable and not empty) | `protonfixes.setup_mount_drives` | every run |
| `com1`… (`/dev/ttyS*`) | Wine's mountmgr | every start |

The first two hang off the `gamedrive` compat option, which `proton` forces on under umu,
and `PROTON_SET_GAME_DRIVE=0` takes it off again (`setup::GAME_DRIVE_OFF`). gogo's games
live inside the prefix and need none of them, so the launcher and every umu run setup makes
set it; an L4 `env` can set it back to `1`. The launcher exports it, so the launcher's
version went to 2 and setup rewrites `launch.sh` (R30). It stops new links only: a link
already there stays until setup's `localize` drops it. The `com` ports can't be stopped
without a registry key per prefix and reach no files, so `doctor` skips `comN -> /dev/*`
as it does `z:`; `localize` still drops them. **What this doesn't do:** `z: -> /` stays
(above), so a game that walks `Z:\` still reaches `$HOME`. It no longer gets a drive letter
for it, and its file dialogs and drive scans no longer list it.

### 7.3 Logs

As design 16 §8.1, in `$XDG_STATE_HOME/gogo/logs/<gogid>/`: `launch.log`, one run log per
launch (newest 10 kept), `latest.log`. Header differences: `# runtime: ge-proton/<ver>`
(or `path:<dir>` with its `version` file), `# umu: <ver or path>`, `# umu_id:`,
`# renderer:`, `# protonfix: <sha or none>`, `# os: <PRETTY_NAME> <arch>` instead of
`# macos:`. umu's and Proton's own output goes into the run log. `PROTON_LOG` isn't set by
default; L4 `env` can set it.

### 7.4 Early-exit dialog

**Settled (owner, 2026-11-03; O6):** the same trigger, message and buttons as design 16 §8.2,
shown with the first tool found:

1. `zenity` (GNOME and most desktops): `--question` with `--extra-button`s.
2. `kdialog` (KDE): `--yesnocancel` with custom button labels.
3. Neither: a desktop notification via `notify-send` if present ("<title> exited after
   2 s, see <log>"), else only `launch.log`.

Buttons: **Open log** (`xdg-open <run log>`), **Try next renderer** (`gogo linux-app
next-renderer <gogid>`, then re-exec, as on the Mac), **Copy report** (to `wl-copy` on
Wayland, else `xclip -selection clipboard`; with neither, the report is written next to
the run log as `report.txt` and the button says **Save report**). `GOGO_LAUNCHER_DIALOG`
and `GOGO_LAUNCHER_NO_DIALOG` work as on the Mac, so the tests don't depend on any of
these tools.

### 7.5 As built (#177)

The code is in `linuxapp/launcher.rs` (the template, the `.desktop` file, the writes) and
`linuxapp/icon.rs`. The tests are in `tests/linuxapp_pipeline.rs`.

**Files, written by setup.** After `launch.conf` and `state.toml`, setup writes
`launch.sh`, `icon.png` and the `.desktop` file. The build gets them through its own
setup run.

- Each file is written only when its bytes changed, through a temp file renamed into
  place. A launcher that runs setup keeps reading the script it started with.
- A changed template reaches a game at its next setup (R30), and setup's notes say
  `launch.sh: launcher <old> -> <new>` (`launch.sh: launcher <n> rewritten` when only
  the bytes changed).
- When setup, run by the launcher, wrote a launcher with another `LAUNCHER_VERSION`, the
  launcher `exec`s the new one for this launch.
- `LAUNCHER_VERSION` is a constant in `launcher.rs`, starting at 1. It is printed in
  `launch.log` and in the run log header (`# launcher:`).
- **The `.desktop` file** is at `game::desktop_path`. It has:
  - `Type=Application`, `Version=1.5`, `Name=`, `Exec=`, `Icon=` (left out when there's
    no icon), `Terminal=false`, `Categories=Game;` and `X-Gogo-Gogid=`;
  - every value escaped as a Desktop Entry string;
  - `Exec=`'s one argument always double-quoted: `"`, `` ` ``, `$` and `\` are
    backslashed, `%` is doubled, and then the string escape applies on top (a literal
    `$` is written `\\$`).
  - A moved data root changes `Exec=`, so the next setup rewrites the file.
- **The icon** is made once, while `icon.png` is missing.
  - It comes from GOG's `goggame-<gogid>.ico` in the game files, else from the exe's
    first `RT_GROUP_ICON` (through `object`'s resource directory).
  - The largest image wins, then the deepest colour.
  - A PNG image is copied as it is. A DIB (1/4/8/16/24/32 bpp, `BI_RGB` or
    `BI_BITFIELDS`) is decoded with its AND mask as transparency (for 32 bpp, only
    when its alpha is empty) and written as an RGBA PNG.
  - No image crate: one IDAT through `flate2`, with its CRC-32.
- **Setup reinstalls a changed managed build** (`setup::changed_managed`).
  - This covers a catalog ref whose build fails `check_managed` (changed `version` or
    `proton`, or missing files). Setup deletes the build and installs the pin again,
    as `store::install_to` does for its own changed shared builds.
  - Before #177, setup took such a build as it found it and recorded its new sha in
    `launch.conf`, which would have hidden the change from the launch check.
  - A `path:` build and one without a marker are left alone.

**`launch.sh`**, step by step (§7.2):

- **Shebang and tools.** `#!/usr/bin/env bash`: NixOS has no `/bin/bash`. Beyond bash, it
  uses coreutils, GNU `grep` (`-z`, for `environ`), `sed`, `awk` and bash's job
  control (`set -m`). The tests run it with a `PATH` of exactly those tools (the
  `LAUNCHER_TOOLS` list) plus the fakes. The unit test runs `shellcheck` on the rendered
  script when it is installed: in the dev shell and on the Ubuntu runner, not on macOS.
- **Roots.**
  - `GOGID` and the data root come from where the script is
    (`<root>/games/<gogid>/launch.sh`), so a missing `launch.conf` still logs to the
    right place.
  - Relative paths resolve against `DATA_ROOT` when that dir exists, else this data
    root. This differs from §7.1's "else `${XDG_DATA_HOME}/gogo`": the two are the same
    unless the data root moved, and then only the script's own location is right.
  - When the data root isn't gogo's default and `GOGO_HOME` is unset (a game built with
    `GOGO_HOME`), gogo runs with `GOGO_HOME=<root>`, so setup and `next-renderer` find
    the game.
- **Logs** go where `state::logs_root()` puts them: `GOGO_LOG_DIR` (this game's dir, as
  on the Mac), else `<GOGO_HOME or that root>/logs/<gogid>`, else
  `${XDG_STATE_HOME:-~/.local/state}/gogo/logs/<gogid>`.
- **When setup runs.** At most once per launch, for the first of these reasons:
  1. no prefix;
  2. a stale stamp;
  3. the Proton build `missing` (managed or not, "minus the reinstall" for an
     unmanaged one), or `changed` and managed;
  4. `UMU_PYTHON` not executable, or `UMU_RUN` gone.

  A build counts as managed when it has `gogo-managed.toml`, or when `PROTON_DIR` is
  relative (a store build). Setup runs with stdin from `/dev/null`, so it's headless and
  never prompts. Its output goes to `launch.log`. A failure goes into the header
  (`# setup: setup failed (exit <n>): <line>; running the last applied config`), as on
  the Mac (#69).
- **After setup:**
  - With no prefix, there's a dialog and exit 1 (as with no `launch.conf`).
  - The runtime check runs again. A changed unmanaged build is logged and launched, with
    `# runtime_note:` in the header. Anything else (missing, or a managed build that is
    still changed) gets a run log that says why, footer `# exit: 127 (runtime
    missing)`, a dialog naming `linux-app setup` and `--retarget`, and exit 127.
  - A umu that is still gone is handled the same way, with footer `(umu missing)`.
- **The environment** is set in umu-run's own subshell, never exported by the launcher.
  This way no tee, watchdog or `sleep` of the launcher's carries `WINEPREFIX` and gets
  killed at teardown. `WINEPREFIX` is unset at the top for the same reason.
  - The order: the `ENV` entries first, then `WINEDLLOVERRIDES` (`DLL_OVERRIDES` before
    any inherited value), `PROTONFIXES_DISABLE=1` for `PROTONFIXES=0`, and
    `UMU_FOLDERS_PATH` when `UMU_FOLDERS` is set.
  - Last come `WINEPREFIX`, `GAMEID`, `STORE=gog` and `PROTONPATH`, which are gogo's and
    can't be overridden.
  - The working dir is `WORKDIR`. The command is `"$UMU_PYTHON" "$UMU_RUN" "$EXE" ARGS…
    "$@"`, or `"$UMU_RUN" …` for a system umu.
- **Run log header** (§7.3), in this order:
  - `# setup:` (only when setup failed), `# launcher:`;
  - `# runtime: <state.toml runtime> (<version file on disk>)`, `# runtime_note:` (only
    when there is one), `# runtime_dir:` (`PROTON_DIR` as written);
  - `# umu:` (`<ver> (zipapp)` for the store zipapp, else the path), `# umu_id:`,
    `# renderer:`;
  - `# protonfix:` (state.toml's hash or `none`), `# protonfixes: on|off`, `# verbs:`,
    `# stamp:`;
  - `# os: <PRETTY_NAME> <arch>`, `# exe:`, `# args:`, `# prefix:`.

  The footer is the Mac's. Two runs in the same second under the same pid (the
  early-exit relaunch `exec`s) would share a name, so the second waits for the next
  second: names keep sorting by time.
- **Waiting for the game.** After umu-run exits, the launcher waits until no process
  carries the prefix. This is the Linux `wineserver -w`: a launcher exe may have started
  the real game. The wait is skipped while another launcher of this game runs.
- **Teardown.**
  - `TERM` goes to umu-run's process group.
  - Then, for up to 5 s, the launcher polls the prefix's processes: one `grep -lzxF`
    over `/proc/[0-9]*/environ` for `WINEPREFIX=<prefix>`, `WINEPREFIX=<prefix>/pfx` and
    `STEAM_COMPAT_DATA_PATH=<prefix>`, each also with a trailing `/` and in the
    `readlink -f` form (`PREFIX_MARKS`; `game::prefix_procs` matches the same set).
    umu's python, `srt-bwrap`, `pv-adverb` and Proton's python carry
    `WINEPREFIX=<prefix>`; every Wine process in the container (`umu.exe`, `wineserver`,
    `services.exe`, `explorer.exe`, the game, …) carries `WINEPREFIX=<prefix>/pfx/`, through
    umu's `pfx -> .` link; all of them carry `STEAM_COMPAT_DATA_PATH=<prefix>`. The first
    real smoke, matching `WINEPREFIX=<prefix>` alone, left 12 Wine processes behind.
  - Whatever is left gets `SIGKILL`, named in `launch.log` (`reaped (SIGKILL): <pid>
    <comm>;…`). Last, the group gets `SIGKILL`.
  - With another live launcher of this game (`.launchers/<pid>`), only this run's group
    goes.
  - The watchdog is the Mac's. Its liveness check reads `/proc/<pid>/stat`: the state
    field (not `Z`) and the start time (field 22), not `ps -o lstart`.
- **Early-exit dialog** (§7.4):
  - The trigger, message, button order and the `next-renderer --dry-run` probe are the
    Mac's. "Try next renderer" `exec`s the script again with its arguments. Nothing was
    exported, so the environment is the one it started with.
  - **zenity** runs as `--question --switch` with one `--extra-button` per label, so
    there's no OK or Cancel; it prints the label and times out after 10 minutes.
  - **kdialog** runs as `--yesnocancel`. Yes is the default (last) label, No the first,
    and Cancel (also Escape) the middle one, "Open log".
  - Both are used only with `DISPLAY` or `WAYLAND_DISPLAY` set. Otherwise `notify-send`
    says "<title> exited after <n>s, see <run log>".
  - Errors before a run (no `launch.conf`, no prefix, runtime missing) use the same
    tools with one OK button. `GOGO_LAUNCHER_DIALOG` gets them too (title, message,
    `OK`), unlike on the Mac.
  - **Copy report**: `wl-copy` with `WAYLAND_DISPLAY`, else `xclip -selection
    clipboard`. With neither, the button reads **Save report** and writes
    `<log dir>/report.txt`.
  - The report has the header lines except `prefix:`, then the last 40 lines of output,
    with `$HOME` written as `~`.
  - **Open log**: `xdg-open <run log>`, detached.

**Real smoke** (before merge; GE-Proton11-12, umu 1.4.4 zipapp, steamrt4, on NixOS 26.11
in a stock-NixOS `bwrap` root with `env -i`, a scratch `GOGO_HOME`, and Xvfb as the
display). The game was built, then `launch.conf`'s `EXE`/`ARGS` were edited to run console
programs (editing `launch.conf` doesn't change the stamp, so no setup ran):

- `cmd /c exit 3` and `cmd /c echo SMOKE` give footers `# exit: 3` and `# exit: 0`, and
  `launch.log` reads `umu-run exited <n>; waiting for the prefix's processes`, `prefix
  idle`, `early exit: code <n> after 3s on wined3d (dialogs off)` with
  `GOGO_LAUNCHER_NO_DIALOG=1`.
- Proton's `umu.exe` starts the exe with `CREATE_NEW_CONSOLE`, so a console program
  gets a windowed `conhost`. Its output goes to that window, not to the run log. With
  no display, `conhost` and the program hang (no Xvfb, `cmd /c exit 3` never ended).
  Games always have a display.
- `TERM` to the launcher mid-run (`ping -n 120`): within 5 s, `TERM` to umu-run's group
  ended none of the 17 processes (umu-run doesn't forward it usefully). The sweep
  reaped all of them (`reaped (SIGKILL): … umu.exe;wineserver;…;ping.exe;conhost.exe;`),
  none was left, and the footer was `# exit: 143 (SIGTERM)`.
- `kill -9` of the launcher: the watchdog logged `launcher <pid> is gone (SIGKILL?):
  tearing down` 1 s later and was done in 6 s. No process was left, `.launchers/` was
  empty, and the footer was `# exit: 137 (launcher killed)`.
- The `.desktop` file passes `desktop-file-validate`. `icon.png` is a 256×256 RGBA PNG
  of the game's icon.

**Checks the tests and the smoke don't reach**: whether Proton ends a real game cleanly on
`TERM` given longer than 5 s, and zenity's `--switch` and kdialog's labels and the menu
entry on GNOME and KDE, which the owner doesn't use (§11.2).

## 8. `gogo linux-app doctor <gogid>`

Read-only, as design 16 §8.3: one line per check, `ok`/`warn`/`fail` with a hint; exit 1 on
a failure.

| check | fails / warns when | hint |
|---|---|---|
| `launcher` | fail: no `gogo-launch.toml`/`launch.conf`/`launch.sh`, or the `.desktop` file's `Exec=` doesn't point at it | `linux-app setup` |
| `compat` | warn: status broken for `linux` (§5); the fix summary (§4.3) with collisions | `linux-app compat` |
| `umu` | fail: the zipapp missing or its sha wrong, no Python ≥ 3.10, NixOS without gogo's FHS env (§2.1), no unprivileged user namespaces, `umu = "system"` with no `umu-run` on `PATH`. warn: `system` (unpinned) | `runtimes install umu`, install Python, install the flake's `gogo-linux-app` |
| `runtime` | fail: Proton dir missing, `proton` or `version` changed for a managed build, `require_tool_appid` unknown to umu (R2). warn: unmanaged, or a managed build in a shared tools dir | `linux-app setup`, `--retarget --runtime ge-proton` |
| `container` | warn: the Steam Linux Runtime variant the build needs isn't in umu's folder (first launch downloads it); else variant and size | none |
| `renderer` | fail: cnc-ddraw without its files; `d7vk` on a build without D7VK; both a `ddraw.dll` beside the exe and `PROTON_USE_D7VK`; `ddraw=n…` loading the game's DDrawCompat beside the exe. warn: another own `ddraw.dll` with no `ddraw=` override, or a value other than `n…` (that's a choice). Notes `dxguid_block` and DDrawCompat left unloaded | `linux-app setup` |
| `vulkan` | fail: no Vulkan ICD found (`/usr/share/vulkan/icd.d`, `/etc/vulkan/icd.d`, `/run/opengl-driver/share/vulkan/icd.d`, `VK_ICD_FILENAMES`/`VK_DRIVER_FILES`). warn: more than one GPU (R18); `vulkaninfo --summary` output when it's on `PATH` | the driver; `DXVK_FILTER_DEVICE_NAME` |
| `vulkan32` | fail: a 32-bit game and no 32-bit ICD (R19) | NixOS: `hardware.graphics.enable32Bit = true`; elsewhere the distro's 32-bit Mesa/NVIDIA package |
| `stamp`, `links`, `processes`, `log` | as design 16 §8.3 (processes found through `/proc/<pid>/environ`; `links` skips `z:`, which stays on Linux, §7.2) | as there |

NixOS gets its own hints where the fix differs: 32-bit graphics, `nix-ld` not needed for
umu (the container carries its libraries), and a gogo other than the flake's `gogo-linux-app` (§2.1: refused, with the two ways out).

**As built (#178)** — `linuxapp/doctor.rs`, `gogo linux-app doctor <gogid> [--json]`:

- **Shape.** The Mac doctor's `Finding` / `Report` (`ok` / `warn` / `fail`, a hint, detail
  lines), its printer and JSON (`gogid`, `ok`, `findings`), exit 1 on a failure. A check
  may give more than one line (`umu`: the zipapp, the Python, user namespaces; `vulkan`:
  the drivers, then the GPUs). Every host lookup goes through a `doctor::Host` (a root
  under which `/etc/NIXOS`, `/proc/sys`, `/sys/class/drm` and the ICD dirs are read, and
  an env), so the tests run each check on a fake root.
- **Read-only, strictly.** Unlike `mac-app doctor` there is no `--stop-leftovers`:
  `processes` names the pids and the `kill` to run. umu and Proton are never run: umu-run
  exits 0 on a broken `PROTONPATH` (§2.1), so a run would prove nothing, and
  running gogo's FHS env starts bwrap. `vulkaninfo --summary` is the one program run, when
  it's on `PATH`, for device names (10 s limit).
- **Not built.** No `games/<gogid>/` → one failing `launcher` line, and the host checks
  (`umu` as setup would resolve it, user namespaces, `vulkan`) still run.
- **What each check reads** (reusing setup's helpers, not copies of them):
  - `launcher`: `gogo-launch.toml` parses, `launch.conf` is there, `launch.sh` is there,
    executable and this gogo's template (another `LAUNCHER_VERSION` or other bytes warn:
    the next setup rewrites it), and the `.desktop` file at `game::desktop_path` has
    `Exec=` equal to `launcher::exec_value(<launch.sh>)`, the value setup writes.
  - `compat`: `manage::compat`. Warns when the Linux status is broken, or when a protonfix
    overrides one of gogo's values (`CompatReport.collisions`, from the new
    `fixes::Summary::collisions`, worded as the summary words them; the hint names
    `protonfixes = false`). The fix summary and compat warnings are its detail lines.
  - `umu`: `launch.conf` read back (`game::ConfValues`, the inverse of `LaunchConf::render`).
    Zipapp: `UMU_RUN` exists and is the store's `umu/<ver>` whose recorded archive sha256
    is the pin's (the store keeps no hash of the unpacked zipapp, so that is what "its sha"
    can mean); another version than the pin warns. `UMU_PYTHON` must still be executable:
    a missing `/nix/store` path says a garbage collection removed it and to re-run setup.
    gogo's FHS env (named `gogo-umu-python`, or what `GOGO_UMU_PYTHON` names) is trusted
    as built; any other Python fails on NixOS (§2.1's two ways out) and is version-checked
    elsewhere (≥ 3.10). System umu: `UMU_RUN` executable → warn (unpinned), else fail.
    Before setup ran, `[linux] umu` is resolved as setup would. User namespaces:
    `kernel.unprivileged_userns_clone = 0` or `user.max_user_namespaces = 0` fail;
    `kernel.apparmor_restrict_unprivileged_userns = 1` (Ubuntu ≥ 23.10) warns. On NixOS
    the Python line adds "nix-ld isn't needed".
  - `runtime`: `PROTON_DIR` (relative to `DATA_ROOT`) through `proton::check_build`
    against `PROTON_VERSION` / `PROTON_SHA`: missing fails; changed fails for a managed build
    (the marker, or a relative `PROTON_DIR`: the launcher's own test) and warns for an
    unmanaged one, as the launcher treats them. A marker is also `check_managed` (only when
    the launch check passed, so one change is one line) and its `archive_sha256` against
    the catalog pin of the same version. R2:
    `require_tool_appid` through `proton::check_appid`; none named warns. Unmanaged builds
    warn; a managed build outside the data root (a shared tools dir) warns with no hint.
  - `container`: the appid's folder in `$UMU_FOLDERS_PATH/umu` (or `UMU_FOLDERS` from
    `launch.conf`), else `$XDG_DATA_HOME/umu`. umu 1.4.4 names it after the variant column
    of `RUNTIME_VERSIONS` (`1391110` → `steamrt2`, `1628350` → `steamrt3`, `4183110` →
    `steamrt4`, `4185400` → `steamrt4-arm64`; read from the pinned zipapp's
    `umu_runtime.py`, a unit test ties the list to `catalog::umu_runtimes()`). Present
    (its `toolmanifest.vdf`) → its size; missing → a warning that the first launch
    downloads it.
  - `renderer`: `state.toml`'s renderer against `launch.conf` and the game dir in the
    prefix (`manage::scan_installed`, setup's scan, now shared): both a `ddraw.dll` and
    `PROTON_USE_D7VK=1` fail; `cnc-ddraw` needs gogo's `ddraw.dll` + `ddraw.ini` and
    `ddraw=n,b`; `d7vk` needs `renderer::has_d7vk` and the switch. The line lists the
    `PROTON_*` switches, and adds the `dxguid_block` note (may be 2D, try
    `next-renderer`) when the scan has it.
  - `vulkan`: ICD manifests as the loader finds them: `VK_DRIVER_FILES` /
    `VK_ICD_FILENAMES` (they replace the search), else `/etc/vulkan/icd.d`,
    `/usr/local/share` and `/usr/share/vulkan/icd.d`, `/run/opengl-driver{,-32}/share/
    vulkan/icd.d` and each `XDG_DATA_DIRS` entry; one per real file. Bitness: the
    manifest's `library_arch`, else the library's ELF class, else the name (`.i686.`,
    `opengl-driver-32/` → 32). The line groups by driver: `radeon (32/64), nvidia
    (32/64), …`. None → fail (NixOS: `hardware.graphics.enable`). GPUs are
    `/sys/class/drm/card<N>` with a PCI vendor, one per device; more than one warns
    (R18) with the `DXVK_FILTER_DEVICE_NAME` / `VKD3D_FILTER_DEVICE_NAME` L4 hint and
    `vulkaninfo`'s device names as detail.
  - `vulkan32`: the scan's bitness. A 64-bit game needs nothing; a 32-bit one on a build
    with `files/lib/wine/i386-unix` needs a 32-bit ICD (R19): none fails, only manifests
    of unknown bitness warn. A build with `x86_64-unix` and no `i386-unix` (new WoW64)
    serves 32-bit games with the 64-bit driver. Hints: NixOS
    `hardware.graphics.enable32Bit = true`, else the distro's 32-bit Vulkan package.
  - `stamp`: no prefix, or not one Proton made (`setup::is_prefix`) fails; no
    `state.toml`/`launch.conf` fails; the stamp (`gogo-launch.toml` + L4) differing from
    `state.toml`'s or `launch.conf`'s `STAMP` warns (the next launch runs setup).
  - `links`: the Mac's walk with the data root as the boundary, minus the
    `dosdevices/` entries of `setup::KEPT_DRIVES` (`z:`, §7.2), Wine's `comN -> /dev/*`
    (§7.2 "Host drives") and links into the game's
    own Proton build (`PROTON_DIR`). Proton links its DLLs and fonts into `drive_c/windows`:
    1,324 absolute links in a GE-Proton11-12 prefix (real smoke). They are inside the data
    root for a store build, outside it for a shared tools dir or a `path:` build, where
    the Mac's rule failed every game (found by the real smoke, fixed before merge). Links
    into another Proton build (a dir with `proton` and `toolmanifest.vdf`: the build the
    game ran on before a `--retarget`) warn: Proton re-links the prefix when a build of
    another version next runs it. Anything else out of the data root fails.
  - `processes`: a live launcher (`game::live_launcher`: a `.launchers/<pid>` record whose
    `/proc/<pid>/stat` start time matches, as `launch.sh` writes it) → ok; else
    `game::prefix_procs`, the launcher's teardown markers (`WINEPREFIX=<p>`,
    `<p>/pfx`, `STEAM_COMPAT_DATA_PATH=<p>`, each with or without `/`, in both path
    forms) → warn with the pids.
  - `log`: the Mac's `check_log` on `state::logs_root()/<gogid>` (the Linux run log has
    the same `# started:` / `# exit:` / `# duration:` lines).
- **Real smoke** (before merge; `nix build .#gogo-linux-app`, GE-Proton11-12 in the store,
  umu 1.4.4 zipapp through `gogo-umu-python` 3.14.7, steamrt4, NixOS 26.11 in a
  stock-NixOS `bwrap` root with `env -i` and scratch `GOGO_HOME` / XDG dirs, RTX 4090 +
  RADV iGPU host). Treasure Adventure Game (1207659013) was built from its depot (setup
  only, the game never ran). `doctor` was all `ok` but the two-GPU warning, exit 0;
  steamrt4 measured 1.3 GiB unpacked in umu's folder (§2.3 says 699 MB). Then one change
  at a time, each restored after: a gone `UMU_PYTHON` store path, a bare `python3`
  recorded on NixOS, an edited `version` in the managed build, steamrt4 moved away,
  Wine's `x:` → `$HOME` back, `VK_DRIVER_FILES` pointing nowhere, only the 64-bit ICDs,
  a `sleep` carrying `WINEPREFIX=<prefix>/pfx/`, the menu entry gone, an unbuilt gogid.
  Each gave the line its check owes (exit 1 for the failures, 0 for the warnings). A
  copy of the build outside the data root, as `path:` with and without the marker,
  gave the shared-dir and unmanaged warnings, an edited `proton` there a warning, and
  after the prefix was re-made on it, its 1,324 links passed `links`; `--retarget
  --runtime auto` back to the store build then gave the "another Proton build" warning.
- **Fixed on the way.** `apps remove` on Linux checked for a running launcher with the
  Mac's `live_launcher` (`ps -o lstart`), which never matches a Linux launcher's record;
  it uses `game::live_launcher` now.

## 9. Scope notes

- **Native Linux installers** (MojoSetup `.sh`): epic #162, not here. This design keeps
  room for them: the `.desktop` file, `games/<gogid>/`, logs, `apps list|remove` and
  `launch.sh` don't assume Wine. A native game's `launch.conf` would hold no umu keys,
  and `gogo-launch.toml` would need a `kind` key (a `schema` bump), decided in #162.
- **aarch64:** nothing assumes x86_64. Every catalog entry carries `os` and `arch`;
  GE-Proton's `aarch64` asset, `steamrt4-arm64` (appid 4185400, in umu 1.4.4's table) and
  umu's flake (unwrapped only on aarch64) are noted where they apply. No issue is filed
  for aarch64-only work.

## 10. CLI

**Settled (owner, 2026-11-03; O8):** **`gogo linux-app`**, the sibling of `gogo mac-app`, built
only on Linux (`#[cfg(target_os = "linux")]`), with the same shape:

- `gogo linux-app <game|setup.exe|dir>`: build from the depot (default) or the offline
  installer (run under umu), `--runtime`, `--renderer`, `--retarget`, `--interactive`,
  `--json`, `--yes` as on the Mac. No `--portable`, `--codesign` or `--out`: the output is
  the state dir plus the `.desktop` file.
- Subcommands: `setup`, `compat`, `doctor`, `next-renderer`, as on the Mac.
- Shared and lane-neutral already: `gogo runtimes`, `gogo renderers`, `gogo apps
  list|show|remove|prune` (on Linux a game is orphaned when its `.desktop` file is gone,
  and `remove` also deletes it).
- The core stays one module (`macapp/` renamed `apps/` or kept, an implementation call),
  with an OS switch where the lanes differ: bundle vs `.desktop`, the launcher template,
  the renderer table, `doctor`'s checks.
- Why a sibling and not a neutral `gogo app`: `mac-app` is in docs, scripts and users'
  hands; `gogo app` next to `gogo apps` reads like a typo; and each name says what it
  builds. A neutral alias can come later without breaking either.
- Leftovers fixed in the same work: `depot`'s "macOS only for now" (§6), the `runtimes
  install`/`renderers install` refusal off macOS (§6).

**As built (#176)** — `linuxapp/build.rs`, `linuxapp/manage.rs`, `gogo linux-app`:

- Flags: `<game|setup.exe|dir>`, `--from-depot`, `--name`, `--runtime [<ref>]`,
  `--renderer auto|dxvk|vkd3d-proton|d7vk|cnc-ddraw|wined3d`, `--refresh`,
  `--interactive`, `--yes`, `--json`, `--json-progress`; `setup <gogid> [--refresh]
  [--json]`. A bare `--runtime` (or `--interactive`) opens a menu: gogo's pinned GE-Proton,
  the GE-Protons in the store, the unmanaged builds each Steam has, "other…" for a typed
  ref. `compat`, `next-renderer`, `--retarget` and `apps` are below; `doctor` is #178.
- The plan asks `[linux] umu` and, for a GE-Proton download, `[linux] proton_dir` before
  anything downloads (headless and unset fails naming the key). Its lines: source, game
  dir (a rebuild keeps the prefix and saves), compat, umu, runtime, container, renderer
  with what it sets, verbs, `game files:` (#175), and the umu id with the fix summary. The
  last needs the build's `protonfixes/`: with the GE-Proton still to download the plan
  says so, and setup prints the summary after the install.
- Depot lane: `game/` is materialized from the tree (a rebuild replaces it), support
  overlays merged, the prefix created through umu, the game linked. GOG's support
  installer runs best-effort as on the Mac, with the GOG.com key imported first as one
  `REGEDIT4` file through `reg import` (the 32-bit view: `Wow6432Node`).
- Installer lane: staged in `games/.build-<slug>-<pid>/` as `GAMEID=umu-0`, the Mac's
  Inno flags with `/DIR=C:\GOG\<slug>` and `/LOG` through `Z:`. The id comes from the
  installed tree; an older prefix for that id is moved to `prefix.old-<time>`, never
  deleted. `gogo-launch.toml` records `lane = "installer"`.
- A `--renderer` / `--runtime` that won is written into L4's `[platforms.linux]` only
  where the L4 file already sets that key (flags > L4, #110), as on the Mac.
- `[gogo] path` is the invoked path (argv0, else `PATH`), never `current_exe()`: in the
  `gogo-linux-app` package that's `.gogo-wrapped`, which skips the wrapper that sets
  `GOGO_UMU_PYTHON`.
- `lane.rs`, the game-source resolution and the depot-tree lookup now build on Linux too.
- **After the build** (`linuxapp/manage.rs`, the second PR):
  - `gogo linux-app compat <gogid> [--json]`: L1 (the baked verbs), L3 and L4 for Linux,
    merged as setup merges them. With the game's Proton build here, L3 is shown under the
    ownership rule (§4.3) with the fix summary; without it, L3 whole and a note. The
    renderer line gives the effective one and its layer, the baked one and its layer, L1's
    pick from the game as installed, and what setup last applied. Variants as on the Mac.
    Nothing is installed or fetched.
  - `gogo linux-app next-renderer <gogid> [--dry-run]`: `renderer::next_renderer_on(Os::Linux,
    …)` after `state.toml`'s renderer (else the baked one), L1 from a fresh scan; written to
    L4's `[platforms.linux]`.
  - `gogo linux-app --retarget <gogid> --runtime <ref> --renderer <id>`: both checked
    (`ProtonRef::parse`, a Linux renderer id) before L4's `[platforms.linux]` is written;
    `auto` drops a key. Setup runs when something changed and the game is built. No
    `.app`/bundle form: the target is a gogid.
- **`gogo apps` on Linux** (`macapp/apps.rs`): a game whose dir has `gogo-launch.toml` is
  a Linux one (`state.toml` read as `LinuxState`). Its menu entry is
  `linuxapp::game::desktop_path`: `$XDG_DATA_HOME/applications/gogo-<gogid>.desktop`, or
  `$GOGO_HOME/applications/` when `GOGO_HOME` is set (it collapses every dir, and keeps a
  scratch home out of the user's menu). Orphaned: `launch.sh` is there (#177 writes it with
  the menu entry) and the `.desktop` file isn't, so a game built before #177 isn't pruned
  for never having had one; `apps list` says `not yet` for it. `apps remove` deletes the
  `.desktop` file with the game, and refuses while a process has the prefix as its
  `WINEPREFIX` (`linuxapp::game::prefix_procs`, through `/proc/<pid>/environ`, §7.2).
- #177's, not here (built: §7.5): the `.desktop` file, `launch.sh`, the stamp check at launch, logs and
  the dialog. The pieces they share are in core: `linuxapp::game` (`launch.conf`,
  `state.toml`, `current_stamp`) and `linuxapp::setup::setup`.

## 11. Testing

### 11.1 Unit and integration (08 conventions, CI on Linux)

- **Compat:** `[platforms.<os>]` merge per key and verbs editing; top-level OS-only keys
  warn; the 11 migrated L3 files merge to the same macOS entry as before; legacy L4
  top-level keys read as `macos`; L4 writes land in the current OS's table.
- **Id lookup:** CSV parsing (fixture rows, not the GPL file), the fallback's TTL,
  `ETag` handling and stale-on-failure (wiremock), rule 3.
- **Protonfix reader:** fixture modules (synthesized, in the shape of real ones): each
  `util.*` call kind, symlinked modules, computed arguments → "other code".
- **Collisions and ownership:** L4 env vs protonfix env (warning text), `protonfixes =
  false` → `PROTONFIXES_DISABLE=1`, L3 `umu-protonfixes:` fields skipped when the build has
  a module and applied when it doesn't.
- **Renderer:** the §3 table per L1 class, the next-renderer walk, never-both, the
  docs/22 §6.2 rule on synthesized PE fixtures (block vs selective GUIDs).
- **Store:** per-OS catalog filtering, `.tar` extraction, the umu/GE-Proton appid check, the
  interpreter choice (`GOGO_UMU_PYTHON`, a host `python3` ≥ 3.10, refusal on a fake `/etc/NIXOS`
  root without the env),
  the shared tools dir (temp dir + rename, marker, name collision → store), prune never
  deleting there.
- **Launcher** (integration, fake `umu-run` and fake gogo, as `macapp_pipeline.rs` does):
  `launch.conf` ⇄ TOML golden; stamp → setup exactly once; `WINEPREFIX`, `GAMEID`,
  `STORE`, `PROTONPATH` exported; a changed `version` file → setup (managed) or a logged
  launch (unmanaged); a missing build with no gogo → exit 127; run log header and footer;
  the dialog through `GOGO_LAUNCHER_DIALOG`; teardown kills fake processes carrying the
  prefix in their environment, as `WINEPREFIX=<prefix>`, `<prefix>/pfx/` or only
  `STEAM_COMPAT_DATA_PATH`.
- **doctor:** a set-up fake game is all green, then each check fails alone (no ICD,
  32-bit game without a 32-bit ICD, missing zipapp, Python too old, changed `proton`).
- **Depot materialization:** reflink when the temp dir supports it, copy otherwise
  (built in #175: `tests/materialize.rs`).

### 11.2 What the harness checks, and what stays the owner's

**Agents and the owner: `scripts/linux-e2e.sh`** (#201, runbook: [docs/linux-e2e.md](../linux-e2e.md)).
One command builds a game in a scratch home, runs it through its generated `launch.sh` in
headless sway inside a bwrap root with its own network, PID namespace, `/tmp` and
`XDG_RUNTIME_DIR`, drives a per-game manifest (`e2e/<gogid>.toml`), and tears everything
down on every exit path. It runs in software (pixman, llvmpipe, lavapipe), or on a real GPU
with no display output (`--gpu /dev/dri/renderD12x`).

| Check | How |
|---|---|
| The display is the headless one, before launching | own netns with only `lo`, `xwininfo -root` at the manifest's size, `HEADLESS-*` outputs only, private `/tmp`, none of the owner's game or Steam paths visible |
| The game reaches its window | sway's tree, within the manifest's wait |
| Correct graphics | each screenshot: not all black, and the non-black bounding box fills the window |
| Working input | a frame changes after a click or key (`expect_change`) |
| Which GPU | `glxinfo`, `vulkaninfo`, and which GPU node the game's processes hold open |
| Exit | the game's own exit (code 0), or the launcher's teardown on `TERM` |
| Doctor | `links` and `processes` ok after the run |
| Nothing left | no process of the run and no scratch dir, on success, failure, timeout, `INT` and `TERM` |

A pass prints a `tested_entry` for L3 `[platforms.linux]`, with
`host = "headless-sway/<software|nvidia|radv>"`. CI runs the harness's self-test on
a plain xterm, never a game.

**Still the owner's Linux host pass** (#179), on their real session (NixOS, RTX 4090 + RADV
iGPU), because a headless compositor can't show it:

1. **Compositor quirks:** Hyprland's tiling and fullscreen behaviour (#199's picture in the
   top-left of a tiled window didn't reproduce on sway).
2. **Display modes and VRR:** a game changing the real mode, gamescope's need, frame pacing.
3. **Audio:** the sandbox has no sound server.
4. **The menu entry and icon** on the owner's desktop, launched from the launcher.

These can now run through the harness once the games are downloaded (each download is the
owner's OK), and move to the owner only where a run shows a compositor-specific result:

- **Smoke:** one D3D11 game (DXVK) and one D3D12 game (VKD3D-Proton).
- **Fixes:** a game with a `gamefixes-gog` module (Rebel Galaxy Outlaw, `umu-910830`), and a
  game with no row (rule 3).
- **ddraw (docs/22 §7 items 2, 6):** Pharaoh and Stronghold with Wine's `ddraw`, then
  cnc-ddraw; Carmageddon TDR 2000 on D7VK. cnc-ddraw under Proton (R21).
- **Multi-GPU** (R18): `--gpu` gives a sandbox with one GPU, so the device DXVK picks among
  several, and `DXVK_FILTER_DEVICE_NAME` moving it, stay on the host pass.

The early-exit dialog on GNOME (zenity) and KDE (kdialog) isn't on any list: the owner
uses neither. The harness turns the dialog off (`GOGO_LAUNCHER_NO_DIALOG=1`).

## 12. Open items and risks

### 12.1 Open details (settled in this doc)

| # | Detail (docs/21 §8 item) | Status |
|---|---|---|
| O1 | Marking gogo's builds in a shared tools dir; the launch check (#2, R27) | **settled** (owner, 2026-11-03), §2.2 |
| O2 | L3/L4 vs protonfix collision (#4, R10) | **settled** (owner, 2026-11-03), §4.3 |
| O3 | L3 fixes protonfixes already covers (#4, R11) | **settled** (owner, 2026-11-03), §4.3 |
| O4 | GitHub fallback: `main` or pinned; refresh (#5) | **settled** (owner, 2026-11-03), §4.1 |
| O5 | Migrating the 11 L3 files (#6) | **settled** (owner, 2026-11-03), §5 |
| O6 | Early-exit dialog on Linux (#9) | **settled** (owner, 2026-11-03), §7.4 |
| O7 | Relative `launch.conf` paths and the Linux data root (#9) | **settled** (owner, 2026-11-03), §6, §7.1 |
| O8 | CLI name for the Linux lane | **settled** (owner, 2026-11-03), §10 |
| O9 | docs/22 §7 items placed | **settled** (owner, 2026-11-03), §3 |

Still open after this doc, by design: the D3D≤7 corpus check
(docs/22 §7 item 1), and the host-pass questions in §11.2.

### 12.2 Risks R1–R30 (docs/21 §9)

| # | Risk (short) | Status | Where |
|---|---|---|---|
| R1 | umu zipapp vs NixOS without FHS wrapper | addressed (verified 2026-11-03, docs/21 §8 item 1): the flake's `gogo-linux-app` runs the zipapp in `steam.buildRuntimeEnv`; any other gogo on NixOS is refused with a hint. GPU through the env: host pass | §2.1, §11.2 |
| R2 | pinned umu behind GE-Proton's container | addressed: appid list per umu pin, install check, catalog test | §2.1 |
| R3 | Steam Linux Runtime unpinned | accepted (§8 item 3) | §2.3 |
| R4 | fast GE-Proton cadence | accepted: catalog pin + N-1 rollback; a host pass per bump | §1, §11.2 |
| R5 | disk and bandwidth | accepted: sizes in the plan and `runtimes list`; sweep keeps N-1 only for used ids | §6 |
| R6 | Python < 3.10 outside Nix | addressed: setup and `doctor` check it; `umu = "system"` as the way out | §2.1, §8 |
| R7 | umu's shared `~/Games/gog` prefix | addressed: `WINEPREFIX` always set; a test asserts it | §7.2 |
| R8 | plain Wine on host libraries | deferred with the plain-Wine lane | §1 |
| R9 | Python code runs at launch | accepted: it's the runtime's own code, pinned by sha | §1 |
| R10 | protonfixes overwrites L4 env | addressed: collision warnings + `protonfixes = false` (O2) | §4.3 |
| R11 | two fix owners | addressed: one owner per game at run time (O3) | §4.3 |
| R12 | protontricks at launch needs network | addressed: setup pre-installs the protonfix's literal verbs | §4.3 |
| R13 | umu-database is GPL-3.0 | addressed: read in place or downloaded to the cache, never vendored | §4.1 |
| R14 | id lookups lag | accepted | §4.1 |
| R15 | GOG id passed as `SteamAppId` | accepted: no overlap today | §4.1 |
| R16 | wined3d switch is all-or-nothing | accepted: per-API via `dll_overrides` | §3 |
| R17 | D7VK and cnc-ddraw both `ddraw.dll` | addressed: never both | §3 |
| R18 | multi-GPU device choice | addressed: `doctor` warns, L4 env fixes; host pass | §3, §8 |
| R19 | missing 32-bit ICDs | addressed: `doctor`'s `vulkan32` | §8 |
| R20 | DXVK in online games | accepted: documented | §3 |
| R21 | cnc-ddraw under Proton untested | **open**: host pass | §11.2 |
| R22 | no Vulkan in the VM | accepted: host pass | §11.2 |
| R23 | Mac compat data applying on Linux | addressed: `[platforms.<os>]` + migration | §5 |
| R24 | new archive crates | deferred: zstd waits for plain Wine; zip and tar.gz exist, plain tar added | §6 |
| R25 | GE-Proton licence not detected | accepted: gogo downloads, never redistributes | §1 |
| R26 | shared tools dir grows | accepted: `prune` lists sizes, deleting is the user's | §2.2 |
| R27 | a pinned build replaced or deleted | addressed: marker + launch check (O1) | §2.2 |
| R28 | unmanaged build lacks what gogo expects | accepted: the unmanaged note; `doctor` checks D7VK and appid | §1, §8 |
| R29 | static protonfix reading is approximate | accepted: "other code" label | §4.2 |
| R30 | shell launcher drifts from Rust | addressed: `LAUNCHER_VERSION`, setup rewrites `launch.sh` | §7.1 |

### 12.3 Corrections to docs/21

- §8 item 6 counts 13 L3 files; `compat/` has 11.
- §3.2 says the store extracts only `.tar.gz` and `.tar.xz`; `store.rs` handles `.zip`
  too (cnc-ddraw). Plain `.tar` (the umu zipapp archive) is the one to add.

## 13. Implementation issues (epic #155)

Filed 2026-11-03 after the owner agreed; each is a sub-issue of #155.

| # | Title | Labels | Blocked by |
|---|---|---|---|
| I1 (#166) | Compat schema: `[platforms.<os>]` tables, migrate the 11 L3 files and legacy L4 keys (§5) | `type:feature`, `area:wine-runtime`, `area:mac-app` | — |
| I2 (#167) | Linux data root and per-OS catalog: `data_root()`, `os`/`arch` on entries, `.tar` extraction, `runtimes`/`renderers install` on Linux (§6) | `type:feature`, `area:wine-runtime`, `area:cli` | — |
| I3 (#168) | umu provisioning: zipapp catalog entry, consent, Python discovery, `[linux] umu`, the flake's `steam.buildRuntimeEnv` wrapper as `gogo-linux-app` (R1) (§2.1) | `type:feature`, `area:wine-runtime`, `area:packaging` | #167 |
| I4 (#169) | GE-Proton catalog entry, umu appid check, `compatibilitytools.d` option with marker, unmanaged builds in `runtimes list` (§1, §2.2) | `type:feature`, `area:wine-runtime` | #167, #168 |
| I5 (#170) | GOG → umu id lookup: the build's CSV, GitHub fallback cache (§4.1) | `type:feature`, `area:wine-runtime`, `area:licensing` | — |
| I6 (#171) | Protonfix static reader, fix summary, collision warnings, `protonfixes = false`, L3 ownership rule (§4.2, §4.3) | `type:feature`, `area:wine-runtime` | #166, #170 |
| I7 (#172) | Linux renderer table and next-renderer order: DXVK/VKD3D-Proton/D7VK switches, cnc-ddraw, never both (§3) | `type:feature`, `area:wine-runtime` | #173 |
| I8 (#173) | `pe.rs`: docs/22 §6.2 detection rule (dxguid block, device GUIDs, runtime-loaded ddraw, engine hints) (§3) | `type:feature`, `area:wine-runtime`, `area:mac-app` | — |
| I9 (#174) | Research: run the D3D≤7 detection rule on real GOG exes (docs/22 §7 item 1) | `type:research`, `area:wine-runtime` | — (done: docs/22 §5.4) |
| I10 (#175) | Depot materialization on Linux (reflink or copy) and `depot` help text (§6) | `type:feature`, `area:depot` | — |
| I11 (#176) | `gogo linux-app` command: build (depot and installer lanes), `setup`, `compat`, `next-renderer`, `--retarget`; `apps` on Linux (§10) | `type:feature`, `area:cli`, `area:wine-runtime` | #166–#172, #175 |
| I12 (#177) | Linux launcher: `.desktop`, `launch.sh` template, `launch.conf`, stamp, runtime check, logs, teardown, early-exit dialog (§7) | `type:feature`, `area:wine-runtime` | #176 (or lands with it) |
| I13 (#178) | `gogo linux-app doctor`: umu, runtime, container, Vulkan and 32-bit ICD checks, NixOS hints (§8) | `type:feature`, `area:wine-runtime` | #177 |
| I14 (#179) | Linux host pass: the compositor-specific checks in §11.2 (script and runbook: #201, `scripts/linux-e2e.sh`) | `type:chore`, `area:wine-runtime` | #177 |
