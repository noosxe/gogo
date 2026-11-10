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
| D3D≤7 retained mode (`d3drm`/`d3dxof`) | `wined3d` | nothing: Proton's default `ddraw` (labelled `proton-default`, below) |
| DirectDraw, selective DirectDraw GUIDs only, 32-bit | `cnc-ddraw` | `ddraw.dll` + gogo's `ddraw.ini` in the game dir, `ddraw=n,b` (as `ddraw.rs` does on the Mac) |
| OpenGL, Vulkan, packed, nothing found, an engine's implied class (Clickteam) | `wined3d` | nothing (labelled `proton-default`, below) |

- **Renderer ids on Linux:** `dxvk`, `vkd3d-proton`, `d7vk`, `cnc-ddraw[/<ver>]`,
  `wined3d`. `dxvk`/`vkd3d-proton`/`d7vk` carry no version: they are whatever the
  chosen Proton build bundles (GE-Proton11-11: DXVK `v3.1.1-134-g2cb40186c`, VKD3D-Proton
  `0bb64c56`, D7VK's GE submodule). `renderer = "wined3d"` sets `PROTON_USE_WINED3D=1`,
  which moves D3D9–12 together; a per-API choice is a `dll_overrides` entry (R16,
  accepted).
- **`wined3d` as L1's pick is labelled `proton-default` (#229).** The table's `wined3d`
  rows set nothing, so Proton's defaults run: DXVK for D3D9–11 (a Clickteam game that
  loads `mmf2d3d9.dll` renders through DXVK), Wine's builtin `ddraw` for DirectDraw. The
  label says what happens. It is the *applied* ref, not a renderer: `state.toml`'s
  `renderer`, `launch.conf`'s `RENDERER`, `launch.sh`'s `# renderer:` header, the logs,
  `doctor`, `env list` and `--json` all carry `proton-default`, and people read "none
  (Proton's defaults: DXVK for d3d9–11)". The pick and the env it sets don't change.
  `wined3d` keeps one meaning: `PROTON_USE_WINED3D=1`, from L3, L4, a flag, the walk or
  a fallback away from another L1. `renderer = "proton-default"` is not accepted as a
  choice (a plain `auto` is how you get it back). Setup applies it after the layers
  decided (`settle_wined3d`): a `wined3d` chosen over an L1 `wined3d` is still real.
  The compat report's `baked` / `auto` / `effective` use the label for L1's own pick.
  Readers of an applied ref (`doctor`, the walk's "current", `env list`'s applicability)
  take it as L1's wined3d position; `env list` says the Vulkan-side variables are unknown
  there, since which of DXVK or VKD3D-Proton runs depends on the game. `launch.sh` 11
  words the early-exit dialog "on Proton's default renderer".
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
  - **Any other own `ddraw.dll`** keeps `ddraw=n,b` (Chaos Gate's GOG D3D9 wrapper, which
    boots under Proton once the registry values from GOG's install script are set: #205).
    With no override `doctor` warns: the game runs, on the builtin.
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
    with nothing set: labelled `proton-default`, because Proton's DXVK runs it (Treasure
    Adventure Game, Clickteam Fusion 2, #229).
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
| 6. Wayland and D7VK (gamescope?) | **Deferred:** no gamescope by default in the first round. A host pass on Wayland records whether it's needed; a player can run a game under one (§7.8) |
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

### 4.9 GOG's install script (`goggame-<gogid>.script`)

Most older GOG games ship `goggame-<gogid>.script` in the game dir: the JSON GOG Galaxy
runs after an install. A depot build runs no installer, so gogo applies the script's
**registry actions** itself (`macapp/goggame.rs`, shared by the Linux and macOS depot lanes;
#205: Chaos Gate overflows its stack without the `HKCU\Software\RandomGames\WH40K\1.0` values).

Shape: `{"productId", "actions": [{"name", "languages", "install": {"action", "arguments",
"conditions"?}}], "uninstall"?}`. Action types in the cached depots:

| `install.action` | seen in | gogo |
|---|---|---|
| `setRegistry` (`root`, `subkey`, `valueName`, `valueType`, `valueData`, `saveGameData`?, `deleteSubkeys`?) | Chaos Gate, SimCity 3000/4, Stronghold HD | applied |
| `delRegistry` (`root`, `subkey`; `conditions: onLanguageChange`) | AD2044 | skipped, warned |
| `Execute` (`executable`, `runAsAdmin`) | the offline installer wrappers (Zeus, Pharaoh), Chaos Gate's DirectPlay and Firewall helpers, SimCity 4 | skipped, warned |
| `savePath`, `supportData` (`{supportDir}` → `{userdocs}` copies) | SimCity 4, Stronghold HD | skipped, warned |

Registry details:

- `root` is `HKEY_LOCAL_MACHINE`/`CURRENT_USER`/`CLASSES_ROOT`/`USERS`.
- `valueType`: `string`, `expandString`, `dword` and `binary` (any case). `dword` data is
  `$`-prefixed hex (`$0012434f`) or decimal; `binary` is base64, passed to `reg add` as hex.
  An empty or absent `valueName` is the key's default value; no name, type or data means the
  key only (`reg add KEY /f`). Anything else skips that action with a warning.
- Placeholders: `{app}` is `C:\GOG\<tree>` in the prefix, `{userdocs}` the user's Documents
  (`C:\users\steamuser\Documents` on Linux), `{group}` the title. `{{` is a literal `{`;
  other `{…}` (GUIDs) stay as written. `{supportDir}` and the like aren't available: an action
  using one is skipped with a warning.
- A key under `HKLM\SOFTWARE\` outside `Wow6432Node` is also written under
  `Wow6432Node`, since the script doesn't say which view Galaxy used and a 32-bit game reads
  through the redirect.
- Order: the script's values first, then compat `registry` entries, so compat wins. They apply
  when the prefix is new or the combined list changed (Linux), and each setup (macOS).
- Cached games with a script and what it writes: Chaos Gate (20 `HKCU` game settings + DirectPlay
  IPX in `HKLM`), SimCity 3000 Unlimited (3 `HKLM` values), SimCity 4 Deluxe (18 `HKLM`), Stronghold
  HD (9 `HKLM`: App Paths, DirectPlay), AD2044 (a delete only), Zeus & Poseidon and Pharaoh Gold
  (installer `Execute` only).

## 5. Decision 5: `[platforms.<os>]` in the compat schema

**Settled (docs/21 §8 item 6):** one file per game. Top level = every OS (the Windows-side
facts); `[platforms.macos]` / `[platforms.linux]` = per OS. OS names follow Rust's
`target_os`.

| Where | Keys |
|---|---|
| top level | `title`, `exe`, `args`, `variant`, `replace_command`, `verbs`, `dll_overrides`, `registry`, `source` |
| `[platforms.<os>]` | `renderer`, `runtime`, `env`, `status`, `notes`, `tested`, `lane`, `lanes`, `protonfixes` and `extra_args` (Linux only, #234), plus any top-level key to override or extend it there |

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

### 6.1 The game library (#276)

Games are the big part of gogo's data, and the only part someone wants on another drive
(Cyberpunk 2077's build is about 70 GB). `GOGO_HOME` and `XDG_DATA_HOME` move far too much.
`library.dir` (`GOGO_LIBRARY_DIR`, `--library`; design 05) puts new games in
`<library.dir>/<gogid>/`, on both lanes. It moves that dir and nothing else: runtimes,
renderers, `compat/`, logs, config and tokens stay where they are.

**Nothing configured is today's behaviour, byte for byte:** games in `<data_root>/games/`,
`GOGO_HOME` and `XDG_DATA_HOME` as before, no record written, no migration, the same
`launch.conf` and `state.toml`. A test holds this. What differs is the launcher templates, below,
a few lines that do nothing for a game under `games/`: setup rewrites `launch.sh` as on every
upgrade (R30), and a new Mac bundle gets the new launcher.

**Finding a game: a record per game, not the setting.** The setting says where new games go.
It can't say where old ones are, so a game outside `games/` has a location record:
`<data_root>/game-locations/<gogid>`, one line, the game's absolute dir. A record is needed at
all because everything else about the game is in the dir being looked for; it sits in the data
root because that's the one place every gogo run and launcher can already find.

- `GameDirs::for_game` is the one lookup: the record if there is one, else `games/<gogid>`.
  `apps list|show|remove`, `doctor`, `setup`, `env`, `args`, `wrapper`, `compat`, the store's
  references and both launchers go through it or read the same file, and none reads the
  setting. `apps list` is the dirs under `games/` plus the records.
- `GameDirs::for_build` is the one placement: where the game already is, else the library,
  else `games/`.
- A record is checked before it is believed (absolute, no `..`, last part the game's id):
  `apps remove` deletes that dir, and the file is plain text anyone can edit. One that fails
  is ignored.
- `launch.conf` already wrote absolute paths for what's outside the data root, so a library
  game's `PREFIX`, `WORKDIR`, `EXE` and `GAME_DIR` are absolute and everything else stays
  relative to `DATA_ROOT`. The `.desktop` file was already absolute.

**The Linux launcher, `LAUNCHER_VERSION` 7 (8 adds the move lock check, below).** v6 and earlier take their data root from their own
path (`${HERE%/games/*}`), which outside the data root is wrong: logs would land beside the
library and setup would run with the library's parent as `GOGO_HOME`. A library game's
`launch.conf` carries `LIBRARY_GAME='1'` (no such line otherwise), and then the launcher reads
`DATA_ROOT` from it, or uses gogo's default root if that dir is gone. Older launchers keep
working: they exist only for games under `games/`, which they read correctly, and the next
setup replaces them. A library game never has an older launcher, since no older gogo built one.

**The Mac launcher** is immutable in its bundle, so nothing about the location is baked (the
TOML, its stamp and the bundle are byte-identical wherever the game is). It looks for
`<state>/game-locations/<gogid>` at launch and otherwise uses `games/<gogid>`, as a portable
bundle's does too. A bundle built before this reads `games/<gogid>` only: an old `.app` copy
of a game rebuilt into a library stops finding its prefix and says so. Rebuild it.

**The container.** The Steam Linux Runtime shares `$HOME`, the prefix, the exe's dir and the
mount point umu finds above it, and `drive_c/GOG/<name>` is a link out of the prefix to `game/`.
A library elsewhere on the root filesystem (`/games`) is none of those, so umu runs (the
launcher's and setup's) add the game's dir to `PRESSURE_VESSEL_FILESYSTEMS_RW`, and only for a
game outside the data root. Checked against umu's source; not yet on a real library outside
`$HOME` (the owner's drive is under it), so the host pass names it.

**Copying.** `game/` is a per-file `FICLONE` of the source tree where the filesystem allows,
else a full copy (APFS clone or `cp` on the Mac). The source is the depot cache
(`$XDG_CACHE_HOME/gogo/depot/<gogid>`, or `gogo depot --dir`), so a library on another drive
usually means the tree's size again. The plan's `game files:` line says so before anything
runs, and a build that did copy logs a warning with the bytes. To keep it cheap, keep the tree
on the same filesystem: `gogo depot --dir <same drive>` and pass that directory to the build.
An offline installer is not copied at all: it installs into a staging dir in the library
(`<library>/.build-<slug>-<pid>/`, so the final rename stays on one filesystem), and
keeping `download.dir` on that drive only saves reading across drives.

**Changing the setting later moves nothing.** A built game stays where its record says, and a
rebuild goes there too, with a warning when a library is set elsewhere. `gogo linux-app move`
moves a game and keeps its prefix and saves (below); on the Mac, `gogo apps remove` and a new
build is still the only way, at the cost of both. An installer rebuild whose game stayed on another filesystem than the new staging
dir ends with a copy instead of a rename.

**A drive that isn't there.** A record whose dir is missing is still a game: `apps list` shows
it and says why, `doctor` fails a `location` check (a library game gets this check, in or out
of reach; other games don't), and a build refuses, because it would fill the mount point of the
drive that isn't mounted. The store's GC stops: `runtimes prune` refuses while any game can't say what it uses, because deleting its
runtime would break it silently. `apps remove` forgets such a game, its menu entry, logs and
L4 file, and says the files are still at that path.

**Removing.** `apps remove` deletes the game's dir, then its record. A symlinked game dir goes
as the link alone (as `remove_dir_all` always did): the report names the target as left behind,
and never counts or follows it. The library dir itself stays.

**Moving a game (#278).** `gogo linux-app move <gogid> <dir>` puts a built game in
`<dir>/<gogid>/`, and `--default` puts it back in `games/`. `<dir>` is checked as `--library` is.
`--dry-run` says what would happen. The Mac half is still to do: the core
(`macapp/relocate.rs`) is lane-neutral, and a lane only says what counts as running and which of
its files name the old place (`linuxapp/relocate.rs`).

*A prefix needs nothing rewritten.* Read from a real GE-Proton11-12 prefix (Cyberpunk 2077's, 2026-11-09):

| What | What it holds | After a move |
|---|---|---|
| 1324 absolute symlinks in `drive_c/windows`, `Program Files*` | paths into the Proton build | valid: the build doesn't move |
| `dosdevices/z:` | `/` | valid |
| `pfx`, `dosdevices/c:`, `users/<user>`, `Application Data` and the like, `drive_c/GOG/<name>` | relative paths inside the game's dir | valid |
| `system.reg`, `user.reg` | `C:` paths; `Z:` only for fonts and Proton's own build tree | valid |
| `config_info` | the Proton build's version and paths | valid |
| `version`, `tracked_files` | no absolute paths | valid |
| `drive_c/proton_shortcuts/*.desktop` | a `Path=` of the staging dir the prefix was built in | already stale on every installer build; nothing reads them |

Nothing there leads into the prefix by absolute path, and the installer lane has always renamed
prefixes out of `.build-<slug>-<pid>/`. So move rewrites nothing in the prefix, and neither does
setup. One thing does need care across filesystems: 34 entries in that prefix carry `user.wine.sd`
(Wine's security descriptors, and hidden or read-only flags go in `user.DOSATTRIB`), so the copy
keeps `user.*` attributes. A game that wrote its own path into the registry gets a warning, not a
rewrite, and so does an `env` or `args` value naming the old dir: those are the game's and the
user's.

*The steps.* A journal, `<data_root>/game-moves/<gogid>` (from and to), comes first. Then:

1. **The data.** `rename` on one filesystem. On `EXDEV`: refuse if it can't fit, copy to
   `<target>/.moving-<gogid>/` (symlinks as symlinks, modes, mtimes, `user.*`; a reflink where the
   filesystems allow; sockets and FIFOs skipped), `syncfs`, compare every file byte for byte, refuse if
   the game was launched meanwhile, mark the copy `.gogo-moved`, and rename it into place. The
   comparison reads the game twice, minutes on 70 GB, because deleting the source next would take
   someone's saves with it. It drops each copied file's page cache first, so it reads the disk where the
   filesystem lets it.
2. **The game's own files:** `launch.conf`'s `PREFIX`, `GAME_DIR`, `WORKDIR` and `EXE`, and its
   `LIBRARY_GAME` and `DATA_ROOT`, to exactly what setup writes there (a test runs setup after and
   compares; the other lines aren't touched, moving back gives the same bytes as before).
3. **The commit:** the location record is written, or removed for `games/`, and the data root flushed.
   This is the only step that changes where the game is.
4. **Outside the dir:** `launch.sh`, the icon and the menu entry through `launcher::write`, the source
   deleted (read-only dirs too; not while something runs from it, which keeps the journal and warns),
   the marker and the journal deleted.

No setup runs: it can need the network and Wine, and the data has already moved. Nothing it would
write differently is left, and its stamp is about the config, not the place.

*Interrupted.* Before the commit nothing refers to the new place, so a crash, Ctrl-C or full disk
leaves the game runnable where it was. `doctor` warns (a `move` finding), and the next `move`,
`apps remove` or `apps prune` works out from the journal and the record which side of the commit
it stopped on:

| Found | What happens |
|---|---|
| A `.moving-<gogid>/`, or a marked copy the record doesn't name | deleted; the game stays; a new copy starts from nothing |
| The record names the target, and the game is there | the rest of step 4 |
| The record names the target, and the game isn't (a drive not mounted, or a power cut undid it) | refused, and nothing deleted, so the source is still there |
| No source, and an unmarked game at the target (a rename, just before its record) | recorded now. The one window with nowhere to run from, as long as it takes to write one small file |
| No source, and a marked copy | refused: the source may be on a drive that isn't mounted, with newer saves. Mount it and run the move again |

Only a dir with the marker, or `.moving-<gogid>` by its name, is ever deleted as a leftover. A
failed `rename`, a copy error or a mismatch cleans up at once and says the game wasn't touched.
A source that won't delete is a warning: the game has moved, and the journal stays for the next run.

*Adopting (#283).* `gogo linux-app move <gogid> --adopt <dir>` is for a game whose dir moved
without gogo knowing (a drive mounts at a new path, a move by hand): the record, `launch.conf` and
the menu entry are stale, and nothing else is wrong. No data moves, so there is no journal. Decisions:

- `<dir>` is the game's **own** dir, ending in `<gogid>` (what the record holds), not its library,
  so it can't be mistaken for a `move` target. Its parent is checked as `Library::resolve` checks one;
  if that is `games/`, the adopt removes the record.
- It checks the dir is a real dir (not a symlink) with a readable `gogo-launch.toml` whose `gogid` is this game, and no `.gogo-moved` marker (a copy an interrupted move left). After the rewrite and before the commit, `launch.conf`'s `PREFIX` and `GAME_DIR` must name the new dir: an earlier adopt that stopped halfway can leave another, and that is refused rather than committed.
- **Refused while the old place is reachable**: the recorded dir, or `games/<gogid>` when there is no
  record. The error names both paths. Two copies are never chosen between, so the user removes or
  renames one first.
- Refused while the game runs, in the new dir or the old (`refuse_running` on both), under the same
  global and per-game `try_lock`s a move takes, so a launch and an adopt never overlap.
- **An interrupted move of the game is settled first** (`recover`, as `move` does). If that leaves
  its journal (a source that wouldn't go), the adopt is refused, saying so. What recovery did before
  a refusal (a record committed, a copy removed) is appended to the error as "(before that,
  recovery: …)", for `move` too, so those lines aren't lost.
- **A full marked copy and no source dir (#288)** is the one case recovery won't settle on its own
  (the source may be on a drive mounted elsewhere, with newer saves), so `move` refuses it, naming
  both paths and the ways out. `--adopt` settles it by what the user names, never by guessing:
  *the source's new path* (a real dir holding this game, no marker, old place gone: the usual adopt
  checks) → the marked copy is removed, the journal dropped, the adopt goes on; *the copy itself*
  (the move's target, which must hold this game) → "the copy is mine": the marker is removed (the
  only change to it) and recovery finishes as for a rename (rewrite, commit, menu entry, journal). The
  marked copy is only deleted after the named path is proved to hold the game; a kill in between
  leaves the same journal, so the next run settles it the same way.
- Then the steps of a move without its data, shared with it: `rewrite_inside(from = old dir, to = new
  dir, at = new dir)`, the commit (record written or removed, the same flushes in the same order), and
  `publish` for `launch.sh`, the icon and the menu entry. A failure of `publish` is a warning, as in
  a move. A crash after the commit leaves the record right and the menu entry stale: `setup` writes it
  again. Warnings (a path the game's registry or `args` keep) are shown as in a move.
- `--dry-run` and `--json` as `move`; a dry run with a journal pending says it can't tell, since the real run settles it first. The unreachable-dir errors of `move` and `doctor` point at it.

*The launcher and the move's lock* (`LAUNCHER_VERSION` 8, #284). The running-checks alone leave a
window: a game launched from the old place after the last check and quitting before the source goes
loses what it saved. A move therefore also takes the game's own lock, `game-moves/<gogid>.lock`,
exclusively and for its whole run (a recovery too), right after the data root's lock and before its
first running-check. `launch.sh` registers itself (the `.launchers/<pid>` record `live_launcher`
reads), then tries that lock shared and without waiting, and lets go at once; if a move holds it, it
unregisters, logs it, writes a run log footer of 127 and shows the same kind of dialog as its other
refusals. Either the launcher registered before the move's check, which then refuses, or it asked after
the move took the lock, which then refuses the launcher: one side always sees the other. Why not the
data root's `.lock`, shared, as #284 first suggested: a launcher would have to hold it for the whole
session, so playing A would stop a move of B and `move` would claim another gogo was moving a game.
Per game, and held for an instant only, a running game blocks nothing. The kernel drops a dead move's
lock, so an interrupted move (journal left behind) never blocks a launch; the launcher looks at the
lock, never at the journal. It finds the lock under `DATA_ROOT` from `launch.conf` (since v7), else the
data root its own path gives; with no lock file, or no `flock` (util-linux), it launches as before.
Launchers before 8 lack the check, so the window stays for a game whose launcher is older until its
next setup rewrites it.

A launcher that read its dir before a whole move and registers after it (a stale menu entry) finds its own `launch.conf` gone after registering, removes the empty dirs the registration made, and refuses with a dialog.

*Refused:* while its launcher or anything in its prefix runs (the same check as `apps remove`, again
after a copy); a target `Library::resolve` refuses, or under `/tmp`; where the game already is; a
target where `<gogid>` already exists, whatever it is; a dir inside the game's own; a symlinked
game dir; a game not built here; a recorded dir that isn't there (mount it first; `apps remove` forgets
it); while another `move` holds the data root's lock (`flock`, so a killed one can't leave it held). The game's own lock (above) is taken after that one, never the other way round.

**The Mac lane's other paths:** a portable bundle ignores the setting (`--library` conflicts with
`--portable`), since its prefix is made on the Mac it runs on. `scripts/host-pass.sh` follows
the record; `scripts/vm-test.sh` works on scratch state with no library, and
`scripts/linux-e2e.sh` builds under `games/` and reaches a library only by its `move_relaunch` step.

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
  `WRAPPER=(…)` (only with a wrapper, §7.8), `ENV=(…)`, `DLL_OVERRIDES`, `RENDERER`, `PROTONFIXES` (`1`/`0`), `UMU_FOLDERS` (when
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
the data root moved. A game in a library (§6.1) has its own paths absolute, which the rule above
already covers, and `LIBRARY_GAME='1'`, which says `DATA_ROOT` isn't where the launcher is.

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

**Teardown.** On `INT`/`TERM`/`HUP` the launcher first asks the game to close: a Unix
`TERM` never reaches a Windows program as a close request, so a game torn down by it can't
save. It runs `wineboot --end-session`, which sends every window `WM_QUERYENDSESSION` and
`WM_ENDSESSION` and then ends the session's processes. It has to reach the game's own
wineserver, and that one's socket is in `/tmp/.wine-<uid>` of umu's container (private
`/tmp`; `PRESSURE_VESSEL_FILESYSTEMS_RW` refuses `/tmp`). A second `umu-run` (also with
`PROTON_VERB=runinprefix`) is a second container and starts a second wineserver, which sees
no game (live-checked, Chaos Gate). So the launcher enters the first one: it finds the
`wineserver` among the prefix's processes and runs, under `timeout`,
`nsenter -t <wineserver> --preserve-credentials -U -m -- env -i <its environ> <its wine> wineboot --end-session`.
Only the user and mount namespaces: the container's pid namespace belongs to a user
namespace above the launcher's and can't be joined, and Wine doesn't need it. No `nsenter`,
or no wine beside the wineserver: logged, and the teardown goes on. The game then has
`CLOSE_GRACE_SECS` (10, env `GOGO_CLOSE_GRACE_SECS`) to end on its own, umu-run gone and the
prefix empty; the log says `closed by itself after Ns` or `didn't close in Ns: TERM`. The
watchdog stays armed during the wait, and a second signal skips it. The watchdog's own
teardown (a SIGKILLed launcher) skips the request. If the game is still there, `TERM` goes to umu-run's process group, then, after 5 s,
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
  idle`, `early exit: code <n> after 3s on <renderer> (dialogs off)` with
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

### 7.6 Per-game env and launch args (#234)

The owner's Heroic pain (#231): setting env vars and launch args is awkward, and nothing
shows what's active. The CLI covers it now; the TUI (#11) gets a nicer editor later, and a
catalogue of which variables exist (#233) builds on the same report.

**What L4 already had, and what was missing.** `[platforms.linux] env` was in the schema,
merged per key, and setup put it into `launch.conf`'s `ENV` over the renderer's switches
(those fill gaps only, §7.1). `args` existed too, but it *replaces* the game's args (GOG's
`playTasks` arguments, a variant's), which is wrong for "add `-skipStartScreen`": a game
with task args would lose them silently. Missing: a way to set either without editing TOML,
an append, and a view of what applies and why.

**Decided:**

- **Storage: the game's L4 file, `[platforms.linux]`.** `env` is a table
  (`[platforms.linux.env]` when gogo makes it, `toml_edit`, comments kept, removed once
  empty). Launch args are a new key, **`extra_args`**: a list appended after the game's
  args, as Heroic's launcher args are. Linux only (like `protonfixes`: under
  `[platforms.macos]` it warns and is ignored, at the top level too); replaced whole by a
  higher layer, so an L3 entry could carry one and L4 replaces it. `args` keeps its
  meaning. These are the user's keys, not `OWNED_KEYS`: gogo writes them only when asked
  (`compat::edit_user_platform_for`, `UserEdit`).
- **Applied at the next launch, no rebuild.** The launch stamp covers the L4 file (§7.2),
  so a changed one sends the launcher through setup, which writes `ENV` and
  `ARGS = game args + extra_args` (`envargs::launch_args`). Nothing in `launch.conf` or
  `launch.sh` changed shape: no `LAUNCHER_VERSION` bump. They survive rebuilds, setup
  reruns and launcher upgrades because they live in L4, which none of those rewrites.
  The commands don't run setup themselves (unlike `--retarget`, nothing needs
  installing).
- **Where the args go.** `"$UMU_RUN" "$EXE" ARGS… "$@"`: after the exe, the game's args,
  then the user's, then whatever `launch.sh` itself is given. Quoting: `launch.conf` holds
  each word `sh_quote`d in a bash array, so spaces, both quotes, `$`, backticks and `\`
  reach the game as typed (golden file, and the pipeline test checks the argv the fake
  game gets). Line breaks and NUL are refused, in values and args alike: `launch.conf`
  stays one assignment per line for gogo's own reader (`ConfValues`).
- **Key names**: `[A-Za-z_][A-Za-z0-9_]*`, what `export "K=V"` takes. Setup skips a bad
  name hand-written into the file, with a warning, and `doctor` names it.
- **Keys gogo owns** (`envargs::policy`):
  - refused: `WINEPREFIX`, `GAMEID`, `STORE`, `PROTONPATH` (the launcher exports them after
    `ENV`, so a value would never apply); `WINEDLLOVERRIDES` (built from `DLL_OVERRIDES`:
    use `[platforms.linux.dll_overrides]`); `PROTONFIXES_DISABLE` (use `protonfixes =
    false`, so the fix summary knows what the game loses);
  - written with a warning, and the user's value wins: the renderer switches
    (`PROTON_USE_D7VK`, `PROTON_USE_WINED3D`, `PROTON_DXVK_D3D8`) and gogo's
    `PROTON_SET_GAME_DRIVE=0` (§7.2, Host drives);
  - a protonfix that sets the same key still wins at launch (O2): `env set` warns, and
    `env list` and `doctor` show it.
- **Secrets.** Game env vars rarely are, but a user may put a token in one. Keys matching
  `TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `API_KEY`, `APIKEY`, `CREDENTIAL`, `AUTH` or
  `PRIVATE` (case-insensitive) are written as given but shown as `<hidden>` by `env list`
  (unless `--reveal`), `env set`'s report (JSON too) and `doctor`. gogo never logs env
  values: the run log has `# args:` but no env line, and setup's notes don't list them.
  The L4 file and `launch.conf` hold them in plain text, like any config.

**What wins at launch** (lowest first), as `env list` orders it: the environment
`launch.sh` starts with; gogo's default `PROTON_SET_GAME_DRIVE=0`; the renderer's switches;
`gogo-launch.toml`'s `env`; L3, then L4 (and `protonfixes = false` at its layer);
`WINEDLLOVERRIDES`; the launcher's four; then, inside umu, the protonfix (`default.py`,
then the game's module).

**CLI** (`gogo linux-app`, §10). Verb-first `env list|set|unset`, as `config get|set`,
`runtimes list|install` and the issue's own sketch, and the `env list` #233 asked for;
`args` is one command, since it has one value: no args shows it, `-- <args…>` sets it,
`--clear` drops it. They take a GOGID like every other per-game `linux-app` subcommand
(`setup`, `compat`, `doctor`), not a title.

- `env list <gogid> [--reveal] [--json]`: one line per key that applies,
  `KEY=VALUE  (source)`, `pending: the next launch's setup applies it` when `launch.conf`
  doesn't have it yet, and the values it beats under it (`over KEY=VALUE  (source)`).
  Sources: `gogo default`, `renderer <id>`, `gogo-launch.toml`, `L3 compat DB`, `L4 user
  override`, `gogo: dll_overrides + renderer`, `launcher, always`,
  `protonfix <id>, at launch`, and `last setup; the next one may drop it` for a
  `launch.conf` value no layer has while the stamp is stale (a value just unset).
  Renderer values come from `launch.conf` (setup's last run): computing them again would
  mean running the renderer step, which touches the game dir.
- `env set <gogid> KEY=VALUE…`, `env unset <gogid> KEY…`: `Change` lines
  (`env.DXVK_HUD "fps" -> (unset)`), then "the next launch applies it" or "not built yet:
  the build applies it". `unset` of a key another layer sets says who still sets it.
  A refusal is a usage error (exit 2) and writes nothing.
- `args <gogid> [-- <args…> | --clear] [--json]`: the exe, the game's args and where they
  come from (GOG, a variant, an `args` layer), the extra args and their layer, what the
  exe gets, and `pending` while `launch.conf` has older ones.

**For the TUI.** `envargs::env_report` is the "what's active" half: one line per (key,
source) with its state. §7.7's catalogue adds the "what exists" half on top of it.

**For the e2e harness and #241.** A manifest's `[override]` (`env`, `extra_args`, `wrapper`) is
applied by the harness through these commands, with the run's scratch `GOGO_HOME`, right
before the launch (`docs/linux-e2e.md`). So a run (or a live session) sets a game's env
and args without touching the owner's games, and the launch goes through the same stale
stamp and setup a player's would.

**Tests.** `compat`: the writer keeps comments and other tables, reports only changes, no
change no write, empties removed, a fresh file gets `[platforms.linux.env]`; `extra_args`
Linux only and replaced by the higher layer. `envargs`: names, values, ownership, secrets,
the append, shell quoting. The pipeline test sets env and args through `envargs`, checks
`pending`, runs setup, launches through `launch.sh` and compares the fake game's argv and
environment word for word (spaces, quotes, `$`, backticks), checks the run log holds no
value, `doctor`'s `launch` line, unset and clear, and a hand-written bad name and
launcher key. The CLI's parse and its human output have unit tests.

### 7.7 Which switches exist for this game's build (#233)

The owner's other Heroic complaint (#231): nothing tells you what exists, and it takes
hours of searching to find what works with your Proton version. gogo knows the build a game
runs on, so it says. `linuxapp::catalogue` is one report the CLI prints and the TUI (#11)
can browse. It reads files and runs nothing, and it makes no network calls.

**Where it comes from: the build for names, a table for words.** Neither alone would do. A
table alone goes stale, and that's what forum advice is. The build alone has no words and
no way to say what applies. Each entry's `sources` say which it came from
(`catalogue::scan`):

| Source | What it gives | Tag |
|---|---|---|
| `proton` | env vars read and set, `STEAM_COMPAT_CONFIG` token from `check_environment`, default from a `setdefault` outside an `if` | `script` |
| `protonfixes/` helpers (`upscalers.py`, `utilities.py`, …) | what the upscaler code reads; `gamefixes-*/` are §4.2's | `fixes` |
| `user_settings.sample.py` | what Proton documents, commented-out lines too | `sample` |
| strings in 15 DLLs and modules (DXVK, VKD3D-Proton, dxvk-nvapi, D7VK, six of Wine's) | `DXVK_*`, `VKD3D_*`, `D7VK_*`, `PROTON_*`, `WAYLANDDRV_*`; flags and keys the table looks up | `dll` |
| `catalogue::table` (shipped) | one line, default, renderer, GPU, wrapper | `table` |

- **The script reader is a line scanner, not a Python parser.** It knows the shapes these
  scripts use (`check_environment("X", "tok")`, `self.env["X"]`, `.get`, `.setdefault`,
  `"X" in env`, `get_version(env, 'X', …)`, a call split over lines) and ignores the rest,
  comments included. Set-only names are the build talking to itself and need no entry.
- **The DLL scan is evidence, not proof.** A string in the file says the component knows the
  name, not that it reads it. Compilers load some names in pieces (`mov rax, "DXVK_ENA"`), so
  discovery takes only NUL-ended strings, and a miss reads "no trace of it", never "it can't
  work". dxvk-nvapi's driver settings are the case in point: the DLL holds
  `NGX_DLSS_SR_OVERRIDE` and joins `DXVK_NVAPI_DRS_` at run time, so those entries name the
  string to look for (`Spec::probe`). `VKD3D_CONFIG` flags are matched as a string's tail,
  since the linker merges `dxr` into `nodxr`. Flags and `dxvk.conf` keys aren't discovered,
  only confirmed: no pattern tells one from any other lower-case string in the file. All 15
  files take about 1 s in a debug build, so the plain `env list` doesn't read them.
- **Not all of Wine.** `x86_64-windows` alone is 300 MB. The list is the modules that read a
  switch a player would set. A name from anywhere else is `?` if it isn't in the table.
- **`dxvk.conf` keys**: a short list with a player's use, reached through `DXVK_CONFIG`, not
  the whole file. **Env-less `STEAM_COMPAT_CONFIG` tokens** (`vkd3dfl12`, `noopwr`, …) aren't
  entries: mostly per-game hacks. `PROTON_ADD_CONFIG` is the entry that reaches them.

**How the table stays current.** `table::REVIEWED_FOR` names the build it was checked
against, and `tests/fixtures/linuxapp/catalogue/<build>.names` freezes that build's names
(names only). Four tests hold them together:

1. `REVIEWED_FOR` equals the catalog's GE-Proton pin, so **a pin bump fails** until someone
   redoes the review.
2. Every name the build reads has an entry or a place in `PLUMBING`.
3. Every entry is in the build or marked `absent`, and nothing marked `absent` is in it.
4. With `GOGO_CATALOGUE_BUILD=<dir>`, the fixture must match a real build
   (`GOGO_CATALOGUE_BLESS=1` rewrites it). CI has no build, so unset passes.

For a build the table wasn't reviewed for (a `path:` runtime, an old pin), nothing breaks:
names are that build's, what the table lacks shows as `?` with `(not in gogo's table yet)`,
and a warning says the words may lag. `absent` entries are the point, not a leftover: they
answer stale advice (`PROTON_ENABLE_NVAPI`, `DXVK_ASYNC`, `PROTON_ENABLE_NGX_UPDATER`) and
README lines ahead of the release (`PROTON_DUALSENSE_BT_AS_USB`).

**Applicability is marked, never filtered.** `applies` is `yes`, `no` with why, or `?` with
why, checked in this order:

1. The build: no trace of it → `no`.
2. The wrapper's command on `PATH` (`Host`; `MANGOHUD` needs `mangohud`, `gamescope` and
   `gamemoderun` themselves, §7.8).
3. The renderer, `launch.conf`'s `RENDERER`, else the compat pick. Under Proton DXVK's
   `dxgi.dll` also fronts VKD3D-Proton, so `DXVK_FILTER_DEVICE_NAME`, `DXVK_HDR` and
   `DXVK_CONFIG` reach D3D12 games, while `DXVK_HUD` draws on D3D9-11 only. wined3d and
   cnc-ddraw get none of the Vulkan side.
4. The GPU, by vendor, from `/sys/class/drm` as `doctor` reads it. Any matching GPU counts
   (a hybrid laptop has both).

No renderer or GPU known yet gives `?`, not a guess.

**Active values** come from `env_report` unchanged, so "from where" is §7.6's answer. A
`VKD3D_CONFIG` flag or `DXVK_CONFIG` key shows the line that holds it, matched as a whole
word (`dxr` isn't in `nodxr`). Keys nobody knows still show, under `other`. The launcher's
four and `WINEDLLOVERRIDES` are `plumbing`, with their values.

**What it says for the flagship, on GE-Proton11-12** (Cyberpunk 2077, D3D12, NVIDIA, #232).
Read from the script, not reported:

- NVAPI is on by itself when an NVIDIA module is loaded (`/proc/modules`), so **no variable
  is needed for DLSS**. `PROTON_ENABLE_NVAPI` isn't read any more; `PROTON_FORCE_NVAPI` is for
  other vendors, and `PROTON_DISABLE_NVAPI` is the way out.
- `PROTON_DLSS_UPGRADE` downloads at launch (`upscalers.py`), inside umu, not gogo. `gogo` makes
  no network call for it. `PROTON_DLSS_INDICATOR` is how to see DLSS is really on.
- Presets: `DXVK_NVAPI_DRS_NGX_DLSS_SR_OVERRIDE` and its `…_RENDER_PRESET_SELECTION`; likewise
  `RR` and `FG`. These are in dxvk-nvapi 0.9.3's strings.
- Ray tracing is on by itself where VKD3D-Proton thinks it safe; `VKD3D_CONFIG=dxr` forces it
  and `nodxr` takes it out.
- Reflex on D3D11/12 goes through DXVK or VKD3D-Proton: `DXVK_NVAPI_VKREFLEX` is the layer for
  native Vulkan games, marked `no` on `vkd3d-proton`. A test keeps these covered.

**CLI** (§10). `--all` extends #234's verb instead of a parallel command, and the plain `env
list` is unchanged, as is its JSON.

- `env list <gogid> --all [--reveal] [--json]`: sections by component (`proton`, `dxvk-nvapi`,
  `vkd3d-proton`, `dxvk`, `d7vk`, `wine`, `wrappers`, `other`, `plumbing`), inside each NVIDIA
  and upscalers first, then the flags and keys. One line per switch so `grep` works:
  `yes NAME  about Default: d. (why) [sources]`, with `now: KEY=VALUE  (source)` under one
  that's set, and the idle plumbing names on one line.
- `env describe <gogid> <NAME> [--reveal] [--json]`: one switch in full, including its token
  and how to set it (`set with:`, which warns where a value is a list). Any case; a miss exits
  1 and offers the names holding the text. It takes a GOGID like the other per-game commands
  because the answer depends on the game's build.

**How much to describe.** One line each, in gogo's words (a test caps them at 110 bytes), and
a link per component to upstream's README, the stable thing to link. No per-switch links:
anchors move, and `env list --all` is read in a terminal. Descriptions are never pasted from
upstream.

**Tests** (`catalogue/tests.rs`, `linuxapp_pipeline.rs`, `main.rs`): each script shape and what
isn't env use, the sample, whole strings vs a compiler's pieces, flags as tails, a build with
parts missing (`not checked`, not `not found`), applicability per renderer, GPU and tool,
nothing hidden, stale advice, active values by whole word, the four currency tests, flagship
coverage, `report` over a built game, and goldens for both human outputs.

### 7.8 Running a game under a wrapper (#271)

`gamescope … --` and `gamemoderun`, the last piece of the tuning Heroic made awkward. A game
gets one command in front of it; a chain is that command's own business
(`gamemoderun gamescope -f --`).

**Storage and CLI, as §7.6's args.** `wrapper = ["gamescope", "-f", "--"]` under
`[platforms.linux]` in L4: Linux only (under `[platforms.macos]` it warns and is ignored),
replaced whole by a higher layer, a user's key rather than one of `OWNED_KEYS`. The launch
stamp covers the file, so the next launch's setup writes `WRAPPER=(…)` into `launch.conf`,
`sh_quote`d word by word like `ARGS`; there's no such line without a wrapper, and the other
lines are as they were. The launcher is one template either way, and `launch.conf` picks the
branch.

`gogo linux-app wrapper <gogid> [-- <words…> | --clear] [--json]`: no words shows it, words set
it, `--clear` drops it. `env list` adds a `wrapper:` line when there is one (its JSON a
`wrapper` key; without one both are as they were), `env list --all` shows it as `now:` under
`gamescope` and `gamemoderun`, and `env describe` gives the command to set it.

**Argv form, no shell** (`envargs::check_wrapper`; refused with exit 2, nothing written):

- no words at all (`--clear` is how to drop it), a line break or NUL in any word;
- a program that's empty, looks like an option, or is relative with a slash (the launch
  `cd`s first): a bare name for `PATH`, or an absolute path;
- a bare program with a space (`"gamescope -f --"`, a shell string) or a `=` (`DXVK_HUD=1`, a
  shell assignment; `env DXVK_HUD=1 …` is the argv form);
- `%command%` in any word: Steam's, and here the game's command always follows the last word.

Everything else is passed as typed (golden files, and the pipeline test compares the argv a
fake wrapper gets). Allowed but warned: `gamescope` with no `--` after it, since its own options
end there. Setup treats a hand-written bad one as it does a bad env name: a warning, and the
game runs without it, and `doctor` says so.

**Where it goes: outside `umu-run`.** The order is `wrapper… launch.sh <game step> →
umu-run → pressure-vessel's container → Proton → the game`, what Steam's `gamescope --
%command%` gives. gamescope is a compositor: it owns the display connection and the Vulkan
device, runs a nested Xwayland, and starts the game with `DISPLAY` pointing at that. It has to
run on the host with the host's libraries and the session's sockets. Inside the container it
would have the runtime's libraries and `/tmp`, and it wouldn't be the one starting umu.
`gamemoderun` works there too: it's a shell script that adds `LD_PRELOAD` and `LD_LIBRARY_PATH`
and execs (gamemode 1.8.2), and what it sets reaches umu-run underneath it.

**Env: the wrapper gets what the launcher started with; the game's goes on after it.** The
launcher doesn't export the game's environment, and that holds for the wrapper too:
`launch.sh` runs `wrapper… "$SELF" --gogo-run-game <mark> <args…>`, so the wrapper's command is
this script again, whose first step (before any setup, log or teardown code) sources
`launch.conf`, exports `ENV` and the launcher's four, and execs umu-run. Why not hand the
wrapper the game's whole environment, as a plain `export` would:

- **It breaks gogo's tracking.** A process whose environ has `WINEPREFIX` counts as the game's
  (§7.2): gamescope would count as the game, and be `SIGKILL`ed by the sweep with the game.
- **It breaks the wrapper, or the setting.** `MANGOHUD=1`, `VK_*` and a game's own `LD_PRELOAD`
  would apply to gamescope's own Vulkan and process. gamescope also edits what its child gets
  (3.16.31: unsets `ENABLE_VKBASALT`, `SDL_VIDEODRIVER`, `WAYLAND_DISPLAY`, sets `DISPLAY`,
  `ENABLE_GAMESCOPE_WSI=1` unless set). Applied after it, the player's `env` wins, as §7.6 says
  it does.

Why not `wrapper… env K=V… umu-run`: values would be on a command line every user can read,
while §7.6 hides secret-looking values and keeps them out of logs. Why `launch.sh` and not a
second generated file: one template to keep at one version (R30), and `load_conf` and `run_game`
are already there. So the order at launch is §7.6's with one layer added under `ENV`: the
environment `launch.sh` starts with, then what the wrapper sets, then the rest. Nothing is
passed to the wrapper by environment: the mark is an argument, so a wrapper's env, and the
game's, carry nothing new. `env …` as the wrapper's first words is how to give the wrapper
something of its own.

**What a wrapper does to the process tracking.** Read from gamescope 3.16.31's source at the
flake's nixpkgs pin. gamescope stays the launcher's child and starts `gamescopereaper -- <cmd>`
(same session and process group, a subreaper with `PDEATHSIG` `TERM`), which starts the command.
When the command exits, gamescope shuts down (`shutdown_on_primary_child_death`, on by default).
`TERM` makes gamescope shut down, and the reaper pass it to its children. Checked against four
shapes of fake wrapper in the pipeline test (execs; stays the parent; puts the game in a new
session; starts the game and leaves), and a fifth that outlives the game:

| Piece | Without a wrapper | With one |
|---|---|---|
| `WPID` (`wait`, the group for `TERM`/`KILL`) | umu-run, which the subshell became | the wrapper, which the subshell became. umu-run is below it, and for a wrapper that keeps it in the group `TERM` still reaches it |
| Game step ran? | always | `.run-<pid>.game` in the log dir (a path in the wrapper's argv, the game step's pid in it: the step is umu-run's parent and lives as long as it does). `.code` beside it holds the exit code. Both removed with the launcher's record |
| `.launchers/<pid>` (#212) | the launcher | unchanged: launchers, not games |
| `prefix_pids` | the game's | the game's, never the wrapper's. So the game is still seen once it left the wrapper's group, or the wrapper left it |
| The close request (§7.2) | via the prefix's wineserver | the same: it finds the wineserver by environ and enters its namespaces, so the wrapper isn't in the way |
| `closed by itself` | umu-run gone, prefix empty | the wrapper gone too. A wrapper still there after its game isn't a clean close: at the grace, `didn't close`, `TERM`, then `KILL` to its group |
| No game left to ask, wrapper running | n/a | the grace runs out, then `TERM` |
| Wrapper gone, game running | n/a | The launcher used to skip the request when `WPID` was gone. Now it asks whenever the prefix has processes (this also fixes a launcher exe that started the real game and left) |
| `TERM` | umu-run's group | that group, plus, with a wrapper and no other launcher of this game, every process of the prefix: a wrapper may have put the game in a new session. With another launcher, its sweep gets them later |
| The `KILL` sweep and `-WPID` | as §7.2 | as §7.2. The sweep finds the game anywhere, `-WPID` the wrapper |
| Watchdog (#103) | the same teardown | the same teardown, and `TERM`/`KILL` reach the wrapper the same way |
| Footer's "the run ended" | `wait`, then the prefix empty | `wait` for the wrapper, then the prefix empty, so a wrapper that left early doesn't end the run |
| Exit code | umu-run's | the **game's**. The game step stays umu-run's parent (it runs umu-run instead of exec'ing it), writes umu-run's code to `<mark>.code` (tmp, then `mv`) and passes `TERM`/`INT`/`HUP` on, waiting again after a trapped signal. The launcher prefers that code to the wrapper's and logs both (`wrapper gamescope exited 0; the game exited 139`; a signal death is 128+n). gamescope drops its child's code (the source ignores `WaitForChild`'s result), so without this a crash read `# exit: 0`. No `.code` (the step was killed, or never ran) → the wrapper's, as before. Without a wrapper nothing changed: umu-run is still exec'd |

Two new endings, both before a game exists:

- **The wrapper isn't on `PATH`** (`command -v` at launch, after any setup): `not_runnable`, a
  run log with footer `127 (wrapper missing)`, and a dialog naming `wrapper <gogid> --clear`.
  Not "run it without": the player asked for it, and a game at the wrong resolution with no
  word why is worse.
- **The wrapper exited and never ran the game** (no mark and no prefix process after 2 s, which
  is the room a wrapper that forks and leaves needs): footer `<code> (wrapper never ran the game)`
  (a `0` becomes `1`), a dialog with the wrapper's name, the log and `--clear`, and **no**
  early-exit dialog: it would offer "Try next renderer" for something the renderer has nothing to
  do with.

The launcher knows nothing about what a wrapper does beyond this: `launch.sh` has no gamescope
case. `LAUNCHER_VERSION` is 10 (6: the game step stays umu-run's parent, #273; 7: a game in a library, §6.1; 8: refuses while its game is being moved, §6.1; 9: the display mode, §7.9; 10: its monitor, #295), and `# wrapper:` is a new run log header line (only with a
wrapper) that the report includes.

**Live, in the harness** (2026-11-08, Right and Down, D3D11, RTX 4090; `docs/linux-e2e.md`). No
base's key changed for it: gamescope is a store path in a manifest under `target/`, from the
flake's nixpkgs, not one of the harness's tools.

| Wrapper | Ending | What happened |
|---|---|---|
| `env X=y` | `SIGTERM` | window, `asked the game to close`, `closed by itself after 0s`, nothing left |
| gamescope 3.16.31 `--backend sdl`, on X11 | `SIGTERM` | window (picture fills it), renders on the 4090, `closed by itself after 1s`, nothing left |
| same | the game's own Quit (clicks through gamescope) | `wrapper env exited 0`, `prefix idle`, `clean_exit`, nothing left |
| gamescope `--backend wayland` or `headless` | none | `Failed to create backend` (the sandbox has no primary DRM node): `never ran the game`, no renderer offered |

So the process model above is checked against the real thing, on a backend the owner won't use.
Hyprland's Wayland backend is still the host pass's.

**Known and not solved** (read from source and upstream's flags, not run on real hardware;
the owner's host pass, #179 and the PR's steps):

- **`PROTON_ENABLE_WAYLAND=1` under gamescope.** gamescope 3.16.31 unsets `WAYLAND_DISPLAY` for
  its child unless given `--expose-wayland`, so plain gamescope leaves winewayland no socket;
  the game gets Xwayland as `DISPLAY` says. With `--expose-wayland` the socket is gamescope's
  own (`gamescope-0`, xdg-shell clients). Whether that works with GE-Proton isn't tested.
  With gogo's Wayland mode (§7.9) the game step checks for the socket again under the wrapper,
  so the run log says the game got X11, and `doctor` warns about gamescope without
  `--expose-wayland`.
- **HDR.** `--hdr-enabled` needs the gamescope WSI layer in the client (gamescope's help says
  so), `ENABLE_GAMESCOPE_WSI=1` is set by default, and DXVK/VKD3D-Proton need `DXVK_HDR=1`.
  Whether pressure-vessel's container finds the layer from a Nix-installed gamescope, and what
  Hyprland does with a nested HDR surface, is unknown.
- **A launcher exe under gamescope.** The reaper `TERM`s everything left once the command exits,
  and gamescope shuts down. A game whose exe starts the real one and exits relies on umu-run
  staying (it does while Wine's services run); nothing here changes that.
- **gamescope's focus and input on Hyprland**, its window rules, and VRR: the host pass.
- **`gamemoderun` in the container.** `LD_PRELOAD` crosses pressure-vessel by its own rules. That
  the daemon registers the game is the host pass's to see (`gamemoded -s`).

**Doctor.** `launch` names the wrapper and its layer, and warns when its program isn't found:
an executable at that absolute path, or by that name on `PATH`. That is the `PATH` the launch
has when it starts from the same shell; a menu launch has the session's, which the warning says.
It checks the first word only: the rest of a chain is that program's arguments.

**Tests** (`compat`, `envargs`, `game`, `launcher`, `catalogue`, `linuxapp_pipeline.rs`,
`cli.rs`, `main.rs`): the L4 key, the refusals and warnings, the pending and applied view, goldens
for `launch.conf` with a wrapper and for the template's game start (the same one with and
without), the argv word for word, the env split in both dumps, close and `TERM` through each
wrapper shape, a wrapper outliving the game, a killed launcher, a wrapper that left while the
game runs, its exit code, the two new endings, doctor and catalogue with the tool on and off
`PATH`, a hand-written bad one, and clearing.

### 7.9 Display mode: Wine's Wayland driver, and HDR (#292)

Every Linux-lane game ran through Xwayland. Wine's Wayland driver, the path HDR takes on the
owner's Hyprland desktop (Cyberpunk 2077, #232), was reachable only as `env set <gogid>
PROTON_ENABLE_WAYLAND=1`, which nothing in gogo knew about. GE-Proton11-12 acts on that switch
only when `WAYLAND_DISPLAY` is set and says nothing when it isn't, and it keeps `DISPLAY`, so a
fallback to X is silent on a desktop with Xwayland (#264's findings on the issue).

**Storage and CLI, as §7.8's wrapper.** Three keys under `[platforms.linux]` in L4: `display =
"wayland" | "x11"`, `hdr = "auto" | "on" | "force" | "off"` and `monitor = "<output>"` (#295). Linux only, each replaced whole
by a higher layer, the user's keys and not `OWNED_KEYS`; a value outside those is a parse error,
like any bad value. The stamp covers the L4 file, so the next launch's setup writes
`DISPLAY_MODE='…'`, `DISPLAY_HDR='…'` and `DISPLAY_MONITOR='…'` into `launch.conf` (no line without
a mode, none for `hdr = "auto"`, none without a `monitor`). `gogo linux-app display <gogid>
[wayland|x11] [--hdr …] [--monitor <output> | --no-monitor] [--clear] [--json]`: no mode shows it, a
mode, `--hdr` or `--monitor` sets that key, `--no-monitor` drops only the monitor, `--clear` drops
all three (`linuxapp::display`).

**Three states, not two.** No `display` leaves the environment alone: what the player's session
or the L4 `env` says reaches the game, as before. `x11` unsets `PROTON_ENABLE_WAYLAND` and
`PROTON_USE_WAYLAND` for the game, so a session-wide switch doesn't put a game that breaks on
Wayland there. `wayland` asks for the driver.

**At launch** (`LAUNCHER_VERSION` 10). The check is the launcher's, since Proton's isn't visible:
`WAYLAND_DISPLAY` set and a socket at it (`$XDG_RUNTIME_DIR/<name>`, or the absolute path).

- With one, the game step exports `PROTON_ENABLE_WAYLAND=1`, plus `hdr`'s switch: `on` is
  `PROTON_ENABLE_HDR=1`, `force` `PROTON_FORCE_HDR=1`, `off` `PROTON_ENABLE_HDR=0`. It unsets
  what a session may carry that would undo that: `PROTON_USE_WAYLAND` (Proton reads the alias
  after the switch, so a `0` there turns Wayland off), and for `on`/`off` `PROTON_USE_HDR` (the
  same) and `PROTON_FORCE_HDR` (Proton checks `forcehdr` before `hdr`, so a `1` there would force
  HDR over `off`). With `force` the others don't matter.
- With a `monitor` it also exports `PROTON_WAYLAND_MONITOR=<output>`. GE-Proton11-12's `proton`
  script copies that to `WAYLANDDRV_PRIMARY_MONITOR`, which winewayland reads (the `.so` has that
  name and not the `PROTON_` one), and only when it's unset does the script guess with `xrandr`:
  with no X server that prints `failed to run xrandr` and leaves the choice to Wine, on Xwayland's
  idea of primary at best. Wine's Wayland driver can't move a window between outputs, so the
  monitor is picked per game. The value is the compositor's output name (`hyprctl monitors`,
  `wlr-randr`: `DP-1`, `HDMI-A-1`) passed as it is: a name that isn't printable ASCII without spaces,
  or is longer than 64 bytes, is a parse error. It isn't checked against the compositor's outputs,
  since gogo runs in no session at set time and a wrong name only makes Wine pick its own.
- Without one, the game runs on X11 (both switches unset), and `launch.log` says why
  (`display: wayland wanted, but WAYLAND_DISPLAY is unset: the game runs on X11 (Xwayland)`).
- It's exported after gogo's `PROTON_SET_GAME_DRIVE=0` and before `ENV`, so a player's own
  `PROTON_ENABLE_WAYLAND` in L4 still wins, as over a renderer switch. `env set` warns that the
  display mode is the switch that checks.
- The run log header gets `# display: wayland (WAYLAND_DISPLAY=wayland-1)` or `# display: x11
  (wayland wanted, but …)`, `# hdr: force` and `# monitor: DP-1` (both `(not applied: not on
  Wayland)` on X11), `# display:` only with a mode (the other two also with their key alone, as `(not
  applied)`). The report carries them.
- **Under a wrapper** the game step runs the check again, in the wrapper's environment, which is
  the one the game gets: gamescope without `--expose-wayland` drops `WAYLAND_DISPLAY`. The step
  prints `gogo: display under the wrapper: …` into the run log, and `launch.log`'s line says the
  check runs again there (also when `launch.sh`'s own environment has no socket: a wrapper may
  bring one). The header's `# display:` is `launch.sh`'s view; that line is the game's.

**Decided: fall back, don't refuse.** The issue offered both. A refusal would leave a player with
no game in an X11 session, over SSH with X forwarding, or in a nested compositor, for a setting
whose only cost when missing is HDR and the native driver. GE-Proton falls back too; what it
lacked was saying so, and now the launch log, the run log and `doctor` all do. The wrapper case
(§7.8) refuses because a missing program is a broken command; a missing socket is a session
without Wayland.

**HDR: Proton's switches, not DXVK's.** In GE-Proton11-12, HDR is detected by itself on Wine's
Wayland driver (shared DXGI reads the output's state), and `PROTON_ENABLE_HDR` is a legacy
opt-in that sets `DXVK_HDR=1`. `forcehdr` also sets `WAYLANDDRV_FORCE_HDR=1`, for outputs the
compositor reports as SDR: Hyprland's `cm_auto_hdr`, KWin at SDR brightness. gogo sets Proton's
names and lets the build map them, so a build that changes `DXVK_HDR`'s meaning keeps working.
`hdr` and `monitor` apply only on the Wayland driver: HDR on Xwayland is gamescope's (§7.8), and
`DXVK_HDR` and the rest stay plain `env` for whoever wants them.
Whether HDR then looks right is the owner's host pass (#179): headless sway has no colour
management.

**Visibility.** `env list` gets a `display:` line and the switches with source `display wayland,
with a Wayland socket at launch`, and `env list --all` / `env describe` name the command for
`PROTON_ENABLE_WAYLAND`, the HDR switches and `PROTON_WAYLAND_MONITOR`. `doctor` has a `display`
check: the mode, HDR and monitor and their layer, what the launcher would pick in this shell's session (a menu launch has the
desktop's), the last run's `# display:`, `# hdr:` and `# monitor:` and the line under a wrapper, and a warning
for what undoes the mode: `hdr` or `monitor` without Wayland mode, an L4 `env`
`PROTON_WAYLAND_MONITOR` that differs from `monitor`, gamescope without `--expose-wayland`, the
L4 `env` saying the opposite, a hand-set `PROTON_ENABLE_WAYLAND` with no mode. Setup prints the
same warnings.

**Not here.** Checking `monitor` against the compositor's outputs (no cheap, portable way: Hyprland
has `hyprctl`, wlroots `wlr-randr`, KWin neither). Catalogue and
compat entries saying a game wants or breaks on Wayland can now be an L3 `display` key, but none
is written yet (#296). OpenGL games on the Wayland driver go through EGL (`WINE_USE_EGL=1`), which NVIDIA
can't show in the harness (#294).

**The harness.** `scripts/linux-e2e.sh --display wayland` writes `linux-app display <gogid>
wayland` before the launch, as the rest of `[override]` (a manifest's `[override] display` or
`hdr` and `monitor` win), and no longer puts `PROTON_ENABLE_WAYLAND=1` in the launch's environment.
`gogo_display` fails unless the run log's header says `wayland`; `isolation_no_x` and
`no_x_fallback` are unchanged, and with Xwayland off a game gogo didn't switch has no X to fall
back to.

**Tests** (`compat`, `game`, `display`, `envargs`, `launcher`, `linuxapp_pipeline.rs`, `cli.rs`,
`main.rs`): the keys and their errors, Linux only, the L4 edit, `launch.conf`'s lines, the
pending view, the session check against a real socket, the conflicts, and through `launch.sh`
the game's environment with a socket, with none, with a `WAYLAND_DISPLAY` that has no socket, the
L4 env winning, `x11` stripping a session's switches, a cleared mode, the header and log lines,
doctor in both sessions, and a wrapper that drops `WAYLAND_DISPLAY`.

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
  - `launch` (#234, §7.6): `envargs::env_report` and `args_report`. The message names the
    user's env keys and extra args, and says when some apply only at the next launch; the
    detail lines are every env value that applies (`*` marks the user's) and the args.
    Warns when a value of the user's never applies: a bad name (setup skips it), a key
    the launcher sets after `ENV`, or one a protonfix sets at launch. Names the wrapper
    too, and warns when setup skips it or its program isn't found (#271, §7.8).
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
  `--library` (§6.1), `--json`, `--yes` as on the Mac. No `--portable`, `--codesign` or `--out`: the output is
  the state dir plus the `.desktop` file.
- Subcommands: `setup`, `compat`, `doctor`, `next-renderer`, as on the Mac; `move` (§6.1), for now
  on Linux only.
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
- **Installer-lane naming (#235).** The title and slug come from the installer's file
  name (`macapp::display_name`) with GOG's tail dropped token by token from the end: `(64bit)`
  / `(32bit)`, a `(<build>)` number, a dotted version (`2.1.0.15`, `2.31a`) and a revision
  word after one (`1.0.3_hotfix`); at most one version, so a title that ends in
  a dotted number keeps it. `setup_cyberpunk_2077_2.31a_(64bit)_(85116).exe` is
  `Cyberpunk 2077` / `cyberpunk-2077`, so `--name` isn't needed for it. A plain number
  (`portal_2`, `cyberpunk_2077`) is never a version. The slug fixes `C:\GOG\<slug>` before
  the install runs; after it, the title in `gogo-launch.toml` (launcher log, `doctor`, the
  `.desktop` name) is the installed `goggame-*.info`'s `name`, unless `--name` was given (the base game's info, never a DLC's; `--json`'s `name` is that title). A rebuild is
  keyed by the gogid, not the slug: it sets the old prefix aside and installs into a fresh one, so
  a slug that differs from an older build's (say `cyberpunk-2077-2-31a-64bit-85116`)
  changes nothing about where the game lives or which saves are kept.
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
  - `gogo linux-app env list|set|unset <gogid>` and `gogo linux-app args <gogid>
    [-- <args…> | --clear]` (#234): §7.6.
  - `gogo linux-app wrapper <gogid> [-- <words…> | --clear]` (#271): §7.8.
  - `gogo linux-app display <gogid> [wayland|x11] [--hdr auto|on|force|off] [--monitor <output>|--no-monitor] [--clear]` (#292, #295):
    §7.9.
  - `gogo linux-app env list <gogid> --all`, `env describe <gogid> <NAME>` (#233): §7.7.
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
with no display output (`--gpu renderD12x`, or several with the compositor's first).

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
- **Multi-GPU** (R18): `--gpu A,B` gives a sandbox with both. DXVK takes the discrete GPU
  wherever the compositor is, and `DXVK_FILTER_DEVICE_NAME` moves it, but a game on the iGPU
  with the compositor on NVIDIA shows nothing (`docs/linux-e2e.md`, Graphics). The same on the
  owner's Hyprland stays on the host pass.

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
| R18 | multi-GPU device choice | addressed: DXVK prefers the discrete GPU (verified headless, sway on either GPU), `doctor` warns, L4 env moves it; host pass | §3, §8 |
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
