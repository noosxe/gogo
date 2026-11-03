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
| Game ships its own `ddraw.dll` | none | keep it, `ddraw=n,b`; no cnc-ddraw, no D7VK |
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
  vs interface GUIDs, discount the `dxguid` block at ≥ 40 of 44 GUIDs (`dxguid_block`
  recorded for `doctor`), check files that load `ddraw.dll` by name, add the Unreal
  `D3DDrv.dll` and LithTech `.ren` hints. The Mac table only changes where docs/22 §6.1
  says so (it doesn't: the Mac keeps wined3d for D3D≤7).
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

**docs/22 §7, placed (owner, 2026-11-03; O9):**

| docs/22 §7 item | Placement |
|---|---|
| 1. A real D3D≤7 corpus (download, `innoextract`, run the rule) | **Open, gating:** an implementation issue (§13, I9, #174). Needs the owner's go-ahead for ~3 GB of account downloads. Until it runs, "block only" stays D7VK |
| 2. Does Linux need cnc-ddraw for 2D? | **Open, host pass** (Pharaoh, Stronghold, X11 and Wayland; §11.2). Default stays cnc-ddraw for confident 2D until then |
| 3. The D7VK pin | **Decided:** the D7VK the Proton build bundles; no separate D7VK download in the first round. Release zips come with the plain-Wine lane |
| 4. `syswow64` vs game dir for D7VK on plain Wine | **Deferred** with the plain-Wine lane (decision 1) |
| 5. Dual-renderer games | **Decided:** default D7VK (the table above), the cnc-ddraw override documented; L3 entries only after a host pass shows one needs it |
| 6. Wayland and D7VK (gamescope?) | **Deferred:** no gamescope by default in the first round. A host pass on Wayland records whether it's needed; running a game under a wrapper is a later feature |
| 7. DXVK-Sarek on MoltenVK | **Out of scope:** Mac lane (epic #50) |
| 8. Retained-mode GOG games | **Deferred:** the RM row needs no code (Proton's default); the corpus check (item 1) notes any it finds |

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
    `~/Library/Logs/gogo`; pointing it at the Linux path is #177's.

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
unchanged. Whether `umu-run` forwards `TERM` into the container cleanly is to be checked
in the first host pass.

**Prefix isolation (design 16 decision 4).** Proton's prefix has a `steamuser` home with
real directories, not links into `~`, and umu links `users/<user>` and `steamuser` to each
other (relative). Setup applies decision 4's rule (no absolute links under
`drive_c/users/`). `z:` → `/`: setup removes it as on the Mac, unless the first host pass
shows that Proton recreates it at every launch (reported, not checked). Then it stays on
Linux and `doctor`'s `links` check skips it.

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
| `renderer` | fail: cnc-ddraw without its files; `d7vk` on a build without D7VK; both a game-dir `ddraw.dll` and `PROTON_USE_D7VK`. Notes `dxguid_block` | `linux-app setup` |
| `vulkan` | fail: no Vulkan ICD found (`/usr/share/vulkan/icd.d`, `/etc/vulkan/icd.d`, `/run/opengl-driver/share/vulkan/icd.d`, `VK_ICD_FILENAMES`/`VK_DRIVER_FILES`). warn: more than one GPU (R18); `vulkaninfo --summary` output when it's on `PATH` | the driver; `DXVK_FILTER_DEVICE_NAME` |
| `vulkan32` | fail: a 32-bit game and no 32-bit ICD (R19) | NixOS: `hardware.graphics.enable32Bit = true`; elsewhere the distro's 32-bit Mesa/NVIDIA package |
| `stamp`, `links`, `processes`, `log` | as design 16 §8.3 (processes found through `/proc/<pid>/environ`) | as there |

NixOS gets its own hints where the fix differs: 32-bit graphics, `nix-ld` not needed for
umu (the container carries its libraries), and a gogo other than the flake's `gogo-linux-app` (§2.1: refused, with the two ways out).

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
  the dialog through `GOGO_LAUNCHER_DIALOG`; teardown kills a fake process carrying the
  prefix in its environment.
- **doctor:** a set-up fake game is all green, then each check fails alone (no ICD,
  32-bit game without a 32-bit ICD, missing zipapp, Python too old, changed `proton`).
- **Depot materialization:** reflink when the temp dir supports it, copy otherwise.

### 11.2 What CI and agents can't check

No Vulkan in the VM and no GPU in CI (R22). The rest is the user's **Linux host pass**, run
on their machine (NixOS, RTX 4090 + RADV iGPU), never by an agent against a real game. The
implementation issues list its steps in their PRs:

1. **The FHS env with a GPU** (R1 was verified with a console exe only): a game renders
   through the zipapp in `steam.buildRuntimeEnv`, on NVIDIA and on the RADV iGPU.
2. **Smoke:** one D3D11 game (DXVK) and one D3D12 game (VKD3D-Proton) start, render, and
   exit cleanly; the run log header is right; teardown leaves no process.
3. **Fixes:** a game with a `gamefixes-gog` module (Rebel Galaxy Outlaw, `umu-910830`): the
   summary matches, the verb was pre-installed, no download at launch. A game with no row
   (rule 3) launches.
4. **ddraw (docs/22 §7 items 2, 6):** Pharaoh and Stronghold with Wine's `ddraw`, then
   cnc-ddraw, on X11 and on Wayland; Carmageddon TDR 2000 on D7VK. Decides whether
   cnc-ddraw stays the 2D default and whether gamescope is needed.
5. **cnc-ddraw under Proton** (R21): the game-dir placement plus `ddraw=n,b` loads.
6. **Multi-GPU** (R18): which device DXVK picks; `DXVK_FILTER_DEVICE_NAME` moves it.
7. **Early-exit dialog** on GNOME (zenity) and KDE (kdialog) if available.

Results go into L3 `[platforms.linux]` `tested` entries (`host = "rtx4090/nixos-26.11"`).

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
| I9 (#174) | Research: run the D3D≤7 detection rule on real GOG exes (docs/22 §7 item 1) | `type:research`, `area:wine-runtime` | — (needs the owner's OK for downloads) |
| I10 (#175) | Depot materialization on Linux (reflink or copy) and `depot` help text (§6) | `type:feature`, `area:depot` | — |
| I11 (#176) | `gogo linux-app` command: build (depot and installer lanes), `setup`, `compat`, `next-renderer`, `--retarget`; `apps` on Linux (§10) | `type:feature`, `area:cli`, `area:wine-runtime` | #166–#172, #175 |
| I12 (#177) | Linux launcher: `.desktop`, `launch.sh` template, `launch.conf`, stamp, runtime check, logs, teardown, early-exit dialog (§7) | `type:feature`, `area:wine-runtime` | #176 (or lands with it) |
| I13 (#178) | `gogo linux-app doctor`: umu, runtime, container, Vulkan and 32-bit ICD checks, NixOS hints (§8) | `type:feature`, `area:wine-runtime` | #177 |
| I14 (#179) | Linux host pass: script and runbook for the checks in §11.2 | `type:chore`, `area:wine-runtime` | #177 |
