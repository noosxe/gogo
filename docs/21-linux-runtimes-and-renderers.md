# 21 — Linux runtimes and renderers: Proton/umu, DXVK, VKD3D-Proton (#150)

**Research date:** 2026-11-02 (all probes the same day)
**Test bed:** NixOS 26.11 (Zokor), x86_64, kernel 7.2.11-zen1; NVIDIA RTX 4090 (driver
595.112.07, Vulkan 1.4.329) plus the Ryzen 9800X3D iGPU (RADV, Mesa 26.2.6); 32-bit
Vulkan ICDs present (`/run/opengl-driver-32`). No game or Wine binary was run: downloads
were inspected (file lists, `readelf`, scripts), not executed.
**Builds on:** issue #150 (facts from a chat on 2026-10-28), epic #155; docs/14 (Lutris),
docs/15 (runtime distribution), docs/16 (cnc-ddraw), docs/19 (compat layers L0–L4);
designs 12 and 16.
**Status:** research only. No code changed. Facts are marked **verified** (GitHub API, a
file or script read, an archive listing, a live HTTP request) or **reported** (the
project's own docs, not re-checked).

## 0. Summary

- **GE-Proton is the right default, run through umu-launcher.** GE-Proton11-11
  (2026-10-19, Wine 11.0 Staging base) ships DXVK, VKD3D-Proton, D7VK, dxvk-nvapi,
  protonfixes and winetricks in one 565 MB tarball, pinned by a `.sha512sum` asset and
  GitHub's asset digest (**verified**, §1.1). umu-launcher 1.4.4 (GPL-3.0) runs it inside
  Valve's Steam Linux Runtime container and is in nixpkgs (**verified**, §1.2).
- **umu does not look up GOG ids.** `GAMEID` must already be the umu id
  (`umu-<steam appid>` or `umu-<gog id>`). The client does the GOG → umu lookup, as Heroic
  does against `umu.openwinecomponents.org` (**verified**, umu and Heroic source, §2.1).
  #150 implies umu does this itself; it does not.
- **umu-database covers few GOG games, and is GPL-3.0.** 210 GOG rows out of 1,213; only
  77 of them have a fix module under `STORE=gog` (most of those modules are
  symlinks to Steam fixes). The database lists games that *need* fixes, not all games (**verified**, §2.2).
  The CSV is GPL-3.0, so gogo should not vendor it; GE-Proton ships a copy inside its own
  tree, which gogo can read in place (§2.3).
- **umu-protonfixes is BSD-2-Clause** (**verified**), as docs/19 §6.4 assumed. Its GOG
  fixes are mostly winetricks verbs (30 of 78 modules), env vars (16) and DLL overrides
  (14). None picks a renderer (§2.4).
- **Renderer choice under Proton is coarse.** `PROTON_USE_WINED3D=1` moves D3D9–12 to
  wined3d together. The only other switches are `PROTON_NO_D3D10`/`PROTON_NO_D3D11`,
  `PROTON_DXVK_D3D8` and `PROTON_USE_D7VK` (**verified**, `proton` script, §3.1). Plain
  Wine needs DXVK/VKD3D-Proton copied into the prefix plus native overrides, as DXMT does
  on the Mac (§3.2).
- **New since #150:** GE-Proton bundles **D7VK** (D3D7/6/5/3 → Vulkan as a `ddraw.dll`),
  which overlaps cnc-ddraw (§3.3); GE-Proton11 needs **Steam Linux Runtime 4**, not
  sniper (§1.1); GE-Proton now also ships an **aarch64** build.
- **The compat `env` maps cleanly** onto `PROTON_*`, `DXVK_*` and `VKD3D_*` vars, and all
  six compat primitives have a Proton equivalent. Two catches: protonfixes overwrites
  caller env without checking, so an L4 value can lose; and L3/L4 have no platform
  dimension yet, while today's values (`WINEMSYNC`, `DXMT_*`, renderer ids) are Mac-only
  (§4).

## 1. Runtimes

### 1.1 GE-Proton

| Attribute | Verified value |
|---|---|
| Repo | `GloriousEggroll/proton-ge-custom`, 15.3k stars, pushed 2026-11-02 |
| Latest | `GE-Proton11-11`, 2026-10-19 (a hotfix; `GE-Proton11-10` the same morning) |
| Cadence | `GE-Proton11-1` 2026-06-24; 11 releases since 2026-05-01, 8 since 2026-08-01 |
| Assets | `GE-Proton11-11-x86_64.tar.gz` 564,789,552 B; `GE-Proton11-11-aarch64.tar.gz` 648,071,663 B; one `.sha512sum` per tarball |
| Integrity | the `.sha512sum` matched the download; GitHub asset digest `sha256:aa85986c…` |
| Unpacked | 1.5 GB, root `GE-Proton11-11-x86_64/` |
| Layout | `proton` (Python), `toolmanifest.vdf`, `compatibilitytool.vdf`, `version`, `files/{bin,lib,share}`, `protonfixes/` (with `umu-database.csv`, `winetricks`, `gamefixes-*`), `LICENSE`, `LICENSE.OFL`, `PATENTS.AV1` |
| Wine | `wine-11.0 (Staging)` (string in `x86_64-unix/ntdll.so`); `lib/wine` has `i386-unix` too, so classic WoW64 by default, new-style with `PROTON_USE_WOW64=1` |
| Bundled renderers | `files/lib/wine/dxvk` (`v3.1.1-134-g2cb40186c`), `vkd3d-proton` (`0bb64c56`), `d7vk` (i386 `ddraw.dll` only), `nvapi`, `nvidia-libs` |
| Container | `toolmanifest.vdf`: `require_tool_appid "4183110"` = Steam Linux Runtime 4 (`steamrt4`) |
| Licence | repo: none detected by GitHub (`NOASSERTION`); the tarball's `LICENSE` is Valve's BSD-3-Clause for the Proton tree, and the components carry their own (Wine LGPL, DXVK zlib, …). nixpkgs' `proton-ge-bin` labels it BSD-3-Clause. |
| nixpkgs | `proton-ge-bin` = GE-Proton11-11 |

- Valve's own Proton is at `proton-11.0-2e` (2026-10-09) on GitHub; Valve ships it only
  through Steam. **UMU-Proton** (`Open-Wine-Components/umu-proton`) is umu's default
  when `PROTONPATH` is unset; its last release is `UMU-Proton-10.0-4`
  (2026-03-30, 491 MB), well behind GE.
- **Wine-GE is gone**, as #150 says: `GloriousEggroll/wine-ge-custom` is archived, its
  README opens "THIS REPO IS DEAD. UMU-LAUNCHER IS ITS SUCCESSOR", last release
  `GE-Proton8-26` (2024-02-04).

### 1.2 umu-launcher

| Attribute | Verified value |
|---|---|
| Repo | `Open-Wine-Components/umu-launcher`, GPL-3.0, 3.8k stars |
| Latest | `1.4.4`, 2026-07-25 |
| Assets | `umu-launcher-1.4.4-zipapp.tar` (430,080 B), Debian 12/13 and Ubuntu noble/resolute `.deb`s, Fedora 43/44 `.rpm`s; all with asset digests |
| nixpkgs | `umu-launcher` 1.4.4 (also at gogo's flake pin `e7439b6b`), wrapped with `buildFHSEnv` (`umu-launcher-1.4.4-bwrap`). The repo also has its own flake in `packaging/nix` (x86_64 and aarch64; on aarch64 only the unwrapped package) |
| Tried | `nix shell nixpkgs#umu-launcher -c umu-run --help` works: `umu-run [-h] [-v] [--config CONFIG] [winetricks]` |

What `umu-run` does with its environment (**verified**, `umu/umu_run.py` on `main`):

- **`GAMEID`**: unset → `umu-default` (no fixes). Copied to `UMU_ID`. If it matches
  `^umu-[\d\w]+$`, the part after `umu-` becomes `SteamAppId`/`SteamGameId`, so Valve's
  per-appid fixes in the `proton` script apply to the non-Steam copy too.
- **`STORE`**: passed through for protonfixes. Also: with `STORE` set and no
  `WINEPREFIX`, the prefix defaults to **`~/Games/<store>`, one prefix shared by every
  GOG game**. gogo must always set `WINEPREFIX`.
- **`WINEPREFIX`**: the directory gets a `pfx -> .` symlink so that Proton's
  `$STEAM_COMPAT_DATA_PATH/pfx` is the prefix itself; `drive_c/users/<user>` and
  `steamuser` are symlinked to each other.
- **`PROTONPATH`**: a path, a name looked up in `~/.local/share/Steam/compatibilitytools.d`,
  or a codename: `GE-Proton`/`GE-Latest` (download the latest GE-Proton), `UMU-Proton`/
  `UMU-Latest`. Unset → latest UMU-Proton. gogo would always pass a store path.
- **The container**: umu reads `require_tool_appid` and downloads the matching runtime
  from `repo.steampowered.com/<variant>/images/<version>/` into `~/.local/share/umu`.
  `SteamLinuxRuntime_4.tar.xz` is 175,077,288 B (HTTP HEAD, last-modified 2026-10-21);
  sniper is 194,771,152 B. `UMU_NO_RUNTIME=1` skips the container; `UMU_RUNTIME_UPDATE`
  controls updates. So a first GE-Proton launch fetches about 740 MB.
- **Verbs**: `umu-run winetricks <verbs>` runs the winetricks shipped inside the Proton
  tree against `WINEPREFIX`. `PROTON_VERB` (default `waitforexitandrun`) is passed on.
- **`PROTONFIXES_DISABLE=1`** turns protonfixes off (**verified**,
  `protonfixes/__init__.py`).

Lutris (`lutris/util/wine/proton.py`) and Heroic both drive `umu-run` for Proton
(**verified**, source).

### 1.3 Kron4ek Wine builds (standalone Wine)

| Attribute | Verified value |
|---|---|
| Repo | `Kron4ek/Wine-Builds`, MIT (build scripts), 1.3k stars |
| Latest | `11.21`, 2026-10-31; a new tag follows each Wine dev release (`11.20` on 2026-10-18) |
| Flavours | vanilla, `staging`, `staging-tkg`; each as `amd64`, `amd64-wow64`, `x86`. A Proton-Wine flavour is described in the README but has no asset in the latest release |
| Sizes | `wine-11.21-staging-amd64-wow64.tar.xz` 99,958,200 B; `wine-11.21-amd64.tar.xz` 100,390,416 B |
| Integrity | `sha256sums.txt` asset plus asset digests; the staging-wow64 download matched (`a70a9691…`) |
| Layout | `wine-11.21-staging-amd64-wow64/{bin,include,lib,share}`; `lib/wine` has `i386-windows`, `x86_64-unix`, `x86_64-windows`, no `i386-unix` (new-style WoW64) |
| Host deps | "All regular Wine dependencies are required", glibc ≥ 2.27; `bin/wine` asks for `/lib64/ld-linux-x86-64.so.2`. The `-wow64` builds need no 32-bit host libraries |

- These fit the existing store (one GitHub asset, sha256-pinned, `.tar.xz`) as-is.
- **They are not distro-independent.** They use the host's libraries. On NixOS that means
  `nix-ld` with Wine's library set, `steam-run`, or a gogo-built FHS wrapper; on other
  distros, the distro's Wine packages for the dependencies. GE-Proton under umu avoids
  this because the container carries its libraries.
- The distro's Wine is the other option (nixpkgs: `wineWow64Packages.stable` 11.0,
  `.staging` 11.19).

### 1.4 What drops out

GPTK/D3DMetal and the CrossOver lineage (winecx, winecx26) have no Linux role, as #150
says. The CX lineage was chosen on the Mac for Rosetta-era reasons (docs/19 §3.1, §7)
that do not apply here.

## 2. Per-game fixes: umu-database and umu-protonfixes

### 2.1 How a GOG game gets its fix

The chain, as the code does it (**verified**, `umu_run.py`, `protonfixes/fix.py`):

1. The client maps the GOG product id to a umu id. umu does not do this. Heroic's
   `getUmuId` calls `https://umu.openwinecomponents.org/umu_api.php?codename=<id>&store=gog`
   and caches the answer, including "no entry".
2. The client runs `GAMEID=umu-<id> STORE=gog umu-run game.exe`.
3. Inside Proton, protonfixes imports `protonfixes.gamefixes-gog.umu-<id>` (the store
   directory is chosen from `STORE` because `umu-<id>` is not numeric). It runs
   `default.py` and then the game's module. A file in `~/.config/protonfixes/localfixes/`
   with the same name replaces the global one.

Live lookup: `umu_api.php?store=gog&codename=1207658883` returned
`[{"title":"Age of Wonders","umu_id":"umu-61500"}]` (Steam appid 61500).

### 2.2 GOG coverage

From `umu-database.csv` at `7bd4ba23` (2026-10-25) and the `umu-protonfixes` tree at
`4500d09f` (2026-10-25):

| Count | Value |
|---|---|
| Rows, all stores | 1,213 (1,109 distinct umu ids) |
| `gog` rows | **210**, all keyed by the numeric GOG product id; 209 distinct umu ids (212 on 2026-11-03, §8 item 5) |
| …whose umu id is a Steam appid | 182 |
| …whose umu id is the GOG id itself (not on Steam) | 28 |
| `gamefixes-gog/` entries | 78 `umu-*` modules + `default.py` + `__init__.py`: 29 regular files and 51 symlinks into `gamefixes-steam` |
| GOG rows with a module under `STORE=gog` | **77** (76 distinct umu ids); 2 modules have no GOG row |

- The README says the database is for games "that do require fixes" and points to the
  Lutris API for anything wider. So 210 is not a coverage gap to fix; most GOG games
  just need no entry.
- A GOG row with no `gamefixes-gog` module still gets **Valve's** per-appid fixes when
  its umu id is a Steam appid (§1.2), but not the Steam protonfix: under `STORE=gog`
  protonfixes only looks in `gamefixes-gog`.
- The online API (`?store=gog`, 30,254 B JSON, 2026-11-02) and the CSV agree; the API is
  a periodic import of the CSV.

### 2.3 Licences

| Data | Licence | Consequence |
|---|---|---|
| `umu-database` (CSV, tools) | **GPL-3.0** (`LICENSE`) | Don't vendor the CSV into gogo's tree or binary. Read it at run time instead: from the installed GE-Proton (`protonfixes/umu-database.csv`) or from the API |
| `umu-protonfixes` | **BSD-2-Clause** | Translating fixes into gogo's L3 TOML with attribution is fine (docs/19 §6.4; five L3 files already do this) |
| `umu-launcher` | GPL-3.0 | gogo runs it as a separate program; no linking |

The GE-Proton copy of the CSV is the cheapest lookup: no network, versioned with the
runtime that will apply the fix, and gogo distributes nothing.

### 2.4 What the GOG fixes do

Over the 78 `gamefixes-gog` modules bundled in GE-Proton11-11 (77 game modules plus
`default.py`; symlinks followed; a module counts once per call kind):

| Call | Modules |
|---|---|
| `util.protontricks(verb)` (winetricks at launch) | 30 |
| `util.set_environment` | 16 |
| `util.winedll_override` | 14 |
| `util.regedit_add` / registry | 10 |
| `util.replace_command` | 8 |
| `util.append_argument` | 5 |
| `set_ini_options` / `set_xml_options` (edit game config files) | 2 |
| anything mentioning `wined3d` or DXVK | 0 |

These are the six primitives docs/19 §6.4 named, plus game-file edits. `protontricks`
runs winetricks during launch and needs the network the first time; gogo's model runs
verbs in a setup step instead (design 16 decision 2).

## 3. Renderers

### 3.1 Under GE-Proton

The `proton` script decides per launch which DLLs go into the prefix (**verified**,
GE-Proton11-11 `proton`, `setup_prefix` and `check_environment`):

| Setting | Effect |
|---|---|
| default | DXVK `d3d9`, `d3d10core`, `d3d11`, `dxgi`; VKD3D-Proton `d3d12`, `d3d12core`; all set `n` (native) |
| `PROTON_USE_WINED3D=1` (or `PROTON_USE_WINED3D11`) | wined3d for `d3d9`–`d3d12` and `dxgi`, all together |
| `PROTON_DXVK_D3D8=1` | DXVK `d3d8` too (default: wined3d's) |
| `PROTON_NO_D3D10=1`, `PROTON_NO_D3D11=1` | hide those APIs |
| `PROTON_USE_D7VK=1` | D7VK `ddraw.dll` into `syswow64`, `WINEDLLOVERRIDES+=ddraw=n,b` |
| `WINEDLLOVERRIDES` with `dxgi=b` | keep Wine's `dxgi` with DXVK's D3D |
| `PROTON_USE_WOW64=1` | new-style WoW64 |

The same switches can come in as `STEAM_COMPAT_CONFIG` tokens (`wined3d`, `dxvkd3d8`,
`nod3d11`, …). The `proton` script reads 77 distinct `PROTON_*` variables (HDR,
Wayland, NVAPI, FSR/DLSS upgrades, input, …).

So the L1 import scan's per-API choice (D3D9–11 → DXVK, D3D12 → VKD3D-Proton) is
already Proton's default. What L1 adds on Linux is the exceptions: DirectDraw/D3D≤7 →
cnc-ddraw or D7VK, and the import list for `doctor`.

### 3.2 Into a plain Wine prefix

| | DXVK 3.1.1 | VKD3D-Proton 3.1 |
|---|---|---|
| Repo, licence | `doitsujin/dxvk`, zlib | `HansKristian-Work/vkd3d-proton`, LGPL-2.1 |
| Release | 2026-09-15 | 2026-10-28 (3.0.1: 2026-05-06) |
| Asset | `dxvk-3.1.1.tar.gz`, 18,041,512 B, digest `40565b4a…` (matched); also `dxvk-native-3.1.1-steamrt-sniper.tar.gz` | `vkd3d-proton-3.1.tar.zst`, 5,665,978 B, digest `b85ea821…` (matched) |
| Contents | `x64/` and `x32/`: `d3d8 d3d9 d3d10core d3d11 dxgi .dll` — **no setup script** | `x64/`, `x86/`: `d3d12.dll`, `d3d12core.dll`, plus `setup_vkd3d_proton.sh` |
| Install | copy `x64` → `system32`, `x32` → `syswow64`, set `native` overrides for each DLL (README) | the script runs `wineboot -u`, copies the DLLs, adds `HKCU\Software\Wine\DllOverrides` `native` via `wine reg add` |
| nixpkgs | `dxvk` 3.1.1 | `vkd3d-proton` 3.0.1 |
| winetricks | `dxvk` (latest release) and pinned `dxvkNNNN` verbs | `vkd3d` |

- This is the same shape as DXMT on the Mac: PE DLLs into the prefix plus overrides, so
  design 16 decision 3's machinery mostly carries over. Because the DLLs go into the
  prefix, no derived runtime is needed (DXMT needed one only for `winemetal.so`).
- The store's extractor handles `.tar.gz` and `.tar.xz` only (`store.rs`).
  VKD3D-Proton needs **zstd**, D7VK a **zip** (`d7vk-v2.4.zip`).
- DXVK and VKD3D settings are plain env vars (`DXVK_HUD`, `DXVK_CONFIG`,
  `DXVK_CONFIG_FILE`, `DXVK_FILTER_DEVICE_NAME`, `DXVK_LOG_LEVEL`, `VKD3D_CONFIG`,
  `VKD3D_FILTER_DEVICE_NAME`, `VKD3D_SHADER_CACHE_PATH`, …) and work the same under Proton and
  plain Wine. DXVK's README warns that swapping D3D DLLs in online games can count as
  cheating.

### 3.3 DirectDraw and old Direct3D: cnc-ddraw vs D7VK

- **D7VK** (`WinterSnowfall/d7vk`, zlib, 791 stars, v2.4 on 2026-10-28) implements D3D7,
  6, 5 and 3 on Vulkan as a 32-bit `ddraw.dll`. GE-Proton bundles it, off by default.
- **cnc-ddraw** (docs/16) is a Windows DLL, and gogo puts it in the game directory with a
  `ddraw.dll` override (`ddraw.rs`). Under Proton, `setup_ddraw` only touches `syswow64`,
  and `WINEDLLOVERRIDES` from the caller survives (Proton appends to it), so the
  game-directory placement should keep working (**reported**, not run).
- Both are `ddraw.dll`, so they exclude each other per game. cnc-ddraw targets 2D
  DirectDraw games; D7VK targets games that use D3D≤7 for 3D. L1 can't tell those apart
  from imports alone (both import `ddraw.dll`).

### 3.4 wined3d

Still the fallback on Linux, with OpenGL by default and a Vulkan backend selectable
through `WINE_D3D_CONFIG` (`renderer=vulkan`) (**reported**, Wine wiki). Under Proton,
`PROTON_USE_WINED3D=1` selects it.

## 4. The compat model on Linux

### 4.1 Field by field

The L3/L4 schema (`compat.rs` `Entry`) against a Proton launch:

| Field | Under GE-Proton/umu | Under plain Wine |
|---|---|---|
| `env` | **straight through** to `umu-run`'s environment; `PROTON_*`, `DXVK_*`, `VKD3D_*`, `WINE*` all reach Proton/Wine | straight through |
| `dll_overrides` | `WINEDLLOVERRIDES` (Proton appends its own) | same |
| `verbs` | `umu-run winetricks <verbs>` with the runtime's winetricks, in the setup step | gogo's pinned winetricks, as on the Mac |
| `registry` | `reg add` run in the prefix through umu (exact verb, e.g. `PROTON_VERB=runinprefix`, to be tested) | `wine reg`, as on the Mac |
| `args`, `exe`, `replace_command` | unchanged | unchanged |
| `renderer` | `dxvk`/`vkd3d-proton` = default; `wined3d` → `PROTON_USE_WINED3D=1`; `d7vk` → `PROTON_USE_D7VK=1`; `cnc-ddraw` → game-dir DLL + override | DLLs into the prefix (§3.2) |
| `runtime` | `ge-proton/<ver>` store ref | `kron4ek-wine[-staging]/<ver>`, or system Wine |
| `status`, `tested`, `lane` | need a platform (see §4.2) | same |

So yes: `PROTON_*` and DXVK vars map onto the existing `env` with no schema change.

### 4.2 Gaps

1. **No platform dimension.** L3 files and the `tested` entries are Mac results
   (`winecx-…`, `m4pro/macos-…`). The values gogo writes today (`WINEMSYNC`, `DXMT_*`,
   `renderer = "dxmt"`, `d3dmetal`) mean nothing on Linux, and `PROTON_*` means nothing on
   the Mac. `lane`/`lanes` already scope status by build lane. The same idea is needed
   per OS (e.g. `[platforms.linux]` with its own `renderer`, `env`, `status`, `tested`),
   or an explicit map such as `dxmt → dxvk`, `d3dmetal → vkd3d-proton`.
2. **protonfixes runs after gogo's env and overwrites it.** `util.set_environment`
   assigns `os.environ[var]` and `g_session.env[var]` unconditionally. An L4 `env` value
   for a variable that the game's protonfix also sets will lose. Options: pass
   `PROTONFIXES_DISABLE=1` when L4 has any `env`; write the L4 override as a protonfixes
   `localfix`; or accept it and document it.
3. **Two fix sources at once.** With `GAMEID`/`STORE` set, protonfixes applies its
   fixes; gogo's L3 may carry a translated copy of the same fix. Verbs then run twice
   (setup and launch). The design should pick one owner per game: probably protonfixes
   for games it covers, with L3 only for the rest and for `status`.
4. **Renderer granularity.** `renderer = "wined3d"` on Proton switches D3D9–12 at once.
   A per-API override (wined3d for D3D9 only) needs `dll_overrides` instead.

## 5. Other differences from the Mac lane

- **Launcher**: a `.desktop` file plus a launcher script, no bundle and no signing.
  `gogo-launch.toml` (design 16 decision 1) still works as the source of truth.
- **Native Linux installers** (MojoSetup `.sh`) skip Wine entirely; `list --os` already
  knows about them.
- **Vulkan driver**: this machine shows the case `doctor` has to handle: a discrete
  NVIDIA GPU plus an AMD iGPU, both with Vulkan. DXVK and VKD3D pick a device;
  `DXVK_FILTER_DEVICE_NAME`/`VKD3D_FILTER_DEVICE_NAME` override it. 32-bit games under
  classic WoW64 also need the 32-bit ICDs (on NixOS: `hardware.graphics.enable32Bit`).
- **NixOS**: umu from nixpkgs is already FHS-wrapped; a downloaded umu zipapp or a
  Kron4ek build is not. umu is required for the Proton runtime (§8 item 1): gogo downloads
  its zipapp after the user confirms; Python comes from the flake on Nix and from the
  user elsewhere.
- **Testing**: no Vulkan in the VM, so the user's machine is the host pass, as #150 says.

## 6. Corrections to #150

| #150 says | Finding |
|---|---|
| `GAMEID=umu-<id>` with `STORE=gog` "makes umu apply that game's protonfixes on its own" | True once `GAMEID` is the umu id. The **GOG id → umu id lookup is the client's job** (§2.1). The fixes are applied by protonfixes inside GE-/UMU-Proton, not by umu-launcher |
| umu-database "maps store ids, including GOG product ids" | True, for 210 GOG games (§2.2); the licence is **GPL-3.0**, so don't vendor it |
| GE-Proton's renderer choice "comes down to environment variables such as `PROTON_USE_WINED3D=1`" | True, but it is all-or-nothing for D3D9–12 (§3.1) |
| "Steam Linux Runtime container" | GE-Proton11 needs **steamrt4** (appid 4183110), not sniper; umu downloads it (175 MB) on first use |
| DirectDraw → cnc-ddraw | Still valid, but GE-Proton now bundles **D7VK** as an alternative for D3D≤7 (§3.3) |
| Kron4ek tarballs "fit the current download-and-cache store" | True for the store; they still need host libraries, which on NixOS means a wrapper (§1.3) |
| (not mentioned) | umu's default prefix with `STORE` set is `~/Games/<store>`, shared by all games; gogo must set `WINEPREFIX` |
| (not mentioned) | GE-Proton now ships an aarch64 build (steamrt4-arm64) |

Everything else in #150 checked out: Wine-GE is dead, GE-Proton bundles DXVK and
VKD3D-Proton, plain Wine needs them installed into the prefix, umu-protonfixes is BSD-2.

## 7. What carries over from the mac-app lane

| Piece | Carries over? |
|---|---|
| Runtime/renderer store: catalog, sha256 pins, GitHub asset digests, `prune`, N-1 rollback (design 12, design 16 decision 6) | Yes. Add zstd and zip extraction (§3.2) |
| `gogo runtimes` / `gogo renderers` / `gogo apps` | Yes; their catalogs need Linux entries and per-OS filtering |
| Compat layers L1–L4, `--retarget` writing L4 | Yes, plus a platform dimension (§4.2) |
| PE import scan (`pe.rs`) | Yes; the mapping becomes D3D9–11 → dxvk, D3D12 → vkd3d-proton, DirectDraw → cnc-ddraw (or d7vk) |
| `gogo-launch.toml`, setup stamp, per-game state dir, logs, early-exit surfacing, `doctor` | Yes in shape; paths move to XDG dirs, `doctor` gains Vulkan checks |
| Winetricks pin and verbs | Yes for plain Wine; under Proton, the runtime's own winetricks |
| cnc-ddraw (`ddraw.rs`) | Yes, as-is |
| `.app` bundle, signing, `--portable`, D3DMetal, DXMT, CX engines | No |

## 8. Open questions for the design doc

1. **umu is required for the Proton runtime (user decision, 2026-11-03).** The rule:
   umu is required if launching without it would mean extra work, and it would. To run
   GE-Proton's `proton` without umu, gogo would have to rebuild what `umu_run.py` does
   (§1.2):
   - set the Steam variables `proton` expects (`STEAM_COMPAT_DATA_PATH`,
     `STEAM_COMPAT_CLIENT_INSTALL_PATH`, which `setup_prefix` reads unguarded,
     `SteamAppId`/`SteamGameId` from the umu id, `STEAM_COMPAT_TOOL_PATHS`, …);
   - create the `pfx -> .` and `steamuser` symlinks;
   - either download and enter the Steam Linux Runtime container itself, or run outside
     it, which puts GE-Proton's Wine on the host's libraries (on NixOS that means a
     wrapper, the same problem as Kron4ek builds, §1.3);
   - run winetricks through the runtime.

   **How gogo gets it: the zipapp (user decision, 2026-11-03).** gogo downloads umu's
   release zipapp (`umu-launcher-<ver>-zipapp.tar`, 430,080 B for 1.4.4, sha256
   `eb590691…`) into its store, pinned like any catalog entry:
   - The archive holds `umu/umu-run`, one zip file with a `#!/usr/bin/env python3`
     shebang. It vendors its dependencies (`python-xlib`, `urllib3`, `truststore`,
     `six`), so all it needs is a host Python ≥ 3.10 (`requires-python` in umu's
     `pyproject.toml`) (**verified**, archive listing, not run).
   - **Nix**: gogo's flake provides a pinned Python (≥ 3.10) for it.
   - **Outside Nix**: a compatible Python installed by the user is a requirement, and
     `doctor` checks for it. The download itself happens only after the user confirms.
   - **No umu, no Proton runtime**: no fallback that runs `proton` directly. Plain-Wine
     runtimes (Kron4ek, the distro's Wine) don't use umu and stay usable without it.

   Still to check on NixOS: nixpkgs wraps its own `umu-launcher` in `buildFHSEnv`. The
   zipapp run with a plain Python gets no such wrapper. Whether umu's Steam Linux
   Runtime launch works without it is untested. If it doesn't, the Nix build needs the
   FHS wrapper too, or nixpkgs' `umu-launcher` instead of the zipapp.

2. **Where GE-Proton builds live: decided by the user (2026-11-03).** Steam is **not**
   a requirement. umu takes any directory in `PROTONPATH`, and
   `compatibilitytools.d` is only a place where it looks names up
   (`$XDG_DATA_HOME/Steam/compatibilitytools.d`, or under `UMU_FOLDERS_PATH`;
   **verified**, `umu_consts.py`). So:
   - **Default: gogo's store**, pinned and `prune`-able, passed to umu as a path.
   - **If Steam is installed or `compatibilitytools.d` already exists** (Steam, Lutris,
     Heroic or ProtonUp-Qt may use it), gogo asks once whether to keep GE-Proton builds
     there instead, and remembers the answer in its config. `gogo config set` changes it
     later (key name for the design, e.g. `linux.proton_dir = "store" | "steam"`).
   - Detection, for the design to confirm: `~/.local/share/Steam` (on this machine
     `~/.steam/root` points there; Steam is installed, but `compatibilitytools.d` does
     not exist yet) and Flatpak Steam's `~/.var/app/com.valvesoftware.Steam/data/Steam`
     (**reported**).

   - **`prune` never deletes from a shared `compatibilitytools.d`** (user decision,
     2026-11-03), not even builds gogo installed there: Steam, Lutris or Heroic may
     have a game set to that build by name. The exception would be a build gogo can
     prove nothing else uses. That proof isn't available today: each tool keeps its own
     per-game mappings (e.g. Steam's `CompatToolMapping`, **reported**). So in practice
     `prune` lists gogo's old builds there with their sizes and leaves deleting them to
     the user.

   Still to settle in the design: how gogo marks the builds it installed in a shared
   directory, and the check before launch (R27).

   **Unmanaged Proton builds (user decision, 2026-11-03).** gogo can also use a Proton
   build it didn't install: one another tool put in `compatibilitytools.d`, Valve's
   Proton under `steamapps/common/`, or one the user installed from somewhere else.
   umu accepts any directory with a `toolmanifest.vdf` (§1.2).
   - The user decides whether to use such a build, per game or as the default.
   - `gogo runtimes` lists them next to gogo's own, each with an **unmanaged** note:
     not pinned, not installed by gogo, never pruned.
   - Why the note matters: the build may change or vanish (R27); it may not be GE-Proton,
     so protonfixes, D7VK or the bundled umu-database copy may be missing; its
     `require_tool_appid` may name a runtime the pinned umu doesn't know (R2); and L3
     `tested` entries don't match an unknown build (R28).
   - **Later (not in the first round):** inspect each build to find out what it
     supports (Proton base and Wine version, bundled DXVK/VKD3D-Proton/D7VK, protonfixes,
     required runtime) and explain why it may not work for a given game.
3. **Steam Linux Runtime: umu owns it, shared with other umu users (user decision,
   2026-11-03).** gogo follows the pattern umu and Lutris already use rather than
   inventing its own:
   - umu puts the runtime in `$XDG_DATA_HOME/umu/<variant>` (default
     `~/.local/share/umu`) or `$UMU_FOLDERS_PATH/umu`. Flatpak launchers use the host's
     `HOST_XDG_DATA_HOME` with the `xdg-data/umu:create` permission, so every umu user
     on a machine shares one copy. Installs and updates take a file lock (`umu.lock`)
     and re-check for a newer version once they hold it (**verified**,
     `umu_consts.py`, `umu_runtime.py`). Steam's own runtimes under `steamapps/common/`
     are not reused.
   - This machine already has `~/.local/share/umu/{steamrt3,steamrt4,umu.lock,umu-shim}`
     from another umu user.
   - gogo doesn't pin or prune the runtime; R3 is an accepted risk.
   - When gogo overrides a game's `HOME` or `XDG_DATA_HOME`, it sets `UMU_FOLDERS_PATH`
     back to the shared location, as Lutris does (`get_umu_folders_path` in
     `lutris/util/wine/proton.py`), so umu doesn't download the runtime again into the
     game's folder.
   - `doctor` reports which runtime variants are present and their size.
4. **protonfixes: decided per game at app creation (user decision, 2026-11-03).**
   Whether protonfixes matters depends on whether the game needs fixes. So when gogo
   creates the app, it works out whether the game needs any fixes and which, and shows
   the result. Proposed mechanism, for the design to settle:
   1. Resolve the GOG id to a umu id (item 5).
   2. Look for `protonfixes/gamefixes-gog/umu-<id>.py` in the chosen Proton build
      (managed or unmanaged; an unmanaged build may have no `protonfixes/` at all).
   3. If there is one, read it **without running it** and list what it does: winetricks
      verbs, env keys, DLL overrides, registry values, command or argument changes,
      game-file edits (the calls in §2.4). gogo is Rust, so that's a small static
      reader of `util.*` calls (pattern matching, or a Python parser crate such as
      `rustpython-parser`).
   4. Merge that with the L3 entry and the L1 import scan, and show one summary: which
      fixes apply and where each comes from (protonfix, L3, L1), or "no known fixes".
   5. Record the module's hash in the per-game state, so a GE-Proton bump that adds or
      changes the fix is noticed (like design 16's setup stamp).

   Two facts shape this (**verified**, umu-protonfixes `master`):
   - `gamefixes-gog/default.py` runs for **every** GOG game and sets
     `OPENSSL_ia32cap=~0x20000000` (an OpenSSL CPU-detection workaround). Valve's
     per-appid fixes also key on the umu id (§1.2). So gogo should always pass
     `GAMEID`/`STORE=gog`, even for games with no module. In practice the question is
     not on or off but who owns each fix.
   - Steam's `default.py` also parses `-pf_tricks`, `-pf_dxvk_set` and
     `-pf_replace_cmd` launch options; the GOG one doesn't.

   Because the summary lists the protonfix's env keys, gogo can warn at creation and at
   `--retarget` when an L3 or L4 value collides with one (R10). Still to settle in the
   design: what wins on a collision (`PROTONFIXES_DISABLE=1` for that game, a
   protonfixes `localfix`, or a warning only), and whether L3 keeps fixes for games
   that protonfixes already covers (R11).
5. **GOG → umu id lookup: D, the build's CSV with a GitHub fallback (user decision,
   2026-11-03).** Comparison checked 2026-11-03.

   How the copies relate (**verified**): GE-Proton's `protonfixes/umu-database.csv` is
   a git submodule of umu-protonfixes (`subprojects/umu-database`), frozen at release
   time. GE-Proton11-11 carries umu-database `4fba4616` (2026-10-19); protonfixes
   `master` pins `87b499f4` (2026-11-03). GOG rows: **209** in GE-Proton11-11's copy,
   **212** on umu-database `main`, and **209** from the API. The API matched GE's copy
   exactly and was missing the 3 rows added since 2026-10-19 (e.g. The Witcher 3
   standard edition, `1207664643` → `umu-292030`). So the API is not fresher than
   GE's copy today.

   | | A. CSV in the chosen Proton build | B. Online API (`umu_api.php`) | C. CSV from GitHub, cached | D. A, then C as fallback |
   |---|---|---|---|---|
   | Freshness | as of that build's release (GE: 2–3 weeks behind at most, given its cadence) | lagged `main` by 2+ weeks on 2026-11-03 | `main`, or a commit gogo pins | A's, C's where A has none |
   | Network | none | one request per game at creation | one file download (~94 KB), refreshed now and then | only for builds without a CSV |
   | Matches the fixes that will run | **yes**: same snapshot as that build's `gamefixes-gog` | no guarantee | no guarantee (may name a module the build lacks) | yes when A is used |
   | Unmanaged builds without `protonfixes/` | no answer | works | works | works |
   | Privacy | nothing leaves the machine | each GOG id is sent to a community host, so the host sees the user's library | whole file fetched from GitHub; nothing per game | as A, plus C's single fetch |
   | Availability | always | single community host (R14); Heroic depends on it | GitHub | always for managed builds |
   | Licence (GPL-3.0 data) | read in place, nothing distributed | nothing distributed | downloaded to the user's cache by the user's gogo; not vendored | same |
   | Effort | a CSV read from a known path | HTTP + cache (Heroic caches answers, including "none") | HTTP + cache + refresh rule | both A and C |

   What a newer id buys: a GOG game with a Steam umu id also gets Valve's per-appid
   fixes from the `proton` script (§1.2), even with no protonfix module in the build.
   So a fresher id can still help, but rarely. A protonfix that isn't in the build can't
   run whatever the id source says.

   Decided: **D**. gogo reads the chosen build's own CSV: it matches the fixes that
   build can actually run, works offline and sends nothing anywhere. Only when the
   build has none (unmanaged builds) does gogo fall back to a cached copy of the CSV
   from GitHub. gogo doesn't use the API: today it lags, and it sends the user's
   library to a third party one game at a time.

   Still to settle in the design: whether the fallback follows `main` or a commit gogo
   pins, and how often the cache refreshes.
6. **Platform scoping in L3/L4: one file per game, with per-OS tables (user decision,
   2026-11-03).** No separate files per OS: everything about a game stays visible in
   one file. Proposed shape, for the design to settle:
   - **Top level = applies on every OS.** These are the Windows-side facts: `title`,
     `exe`, `args`, `variant`, `replace_command`, `verbs`, `dll_overrides`, `registry`,
     `source`.
   - **`[platforms.macos]` / `[platforms.linux]` = per OS.** `renderer`, `runtime`,
     `env`, `status`, `notes`, `tested`, `lane`/`lanes`, plus any top-level key to
     override or extend it there. The platform table merges over the top level with
     the existing rules (design 16 decision 5): `verbs` add, `-<verb>` removes, `-*`
     clears.
   - `renderer`, `runtime` and `env` only make sense per OS (`dxmt`/`WINEMSYNC` vs
     `dxvk`/`PROTON_*`), so the loader warns about them at the top level, the way it
     warns about unknown keys.
   - OS names follow Rust's `target_os` (`macos`, `linux`).
   - **Migration of today's 13 L3 files:** they use only `title`, `verbs`, `status`,
     `notes`, `tested`, `source`, `lane` and `[lanes.installer]`. `title`, `verbs` and
     `source` stay at the top. `status`, `notes`, `tested`, `lane` and `lanes` move
     under `[platforms.macos]` (`[lanes.installer]` becomes
     `[platforms.macos.lanes.installer]`). L4 files follow the same schema.

   ```toml
   # compat/1435582019.toml — Rebel Galaxy
   title  = "Rebel Galaxy"
   verbs  = ["d3dcompiler_43", "d3dx11_43", "d3dx9_43", "xinput"]
   source = "gogo-host"

   [platforms.macos]
   status = "broken"
   notes  = "unplayable: DXMT loads ~4 min per launch then runs at slideshow speed …"
   tested = [{ runtime = "winecx/24.0.7_7+dxmt-v0.80", host = "m4pro/macos-27.0.1", date = 2026-10-23 }]

   [platforms.linux]
   renderer = "dxvk"
   status   = "unknown"
   ```
7. **Second runtime: none in the first round (user decision, 2026-11-03).** The
   first round ships GE-Proton through umu only. The plain-Wine path (Kron4ek, the
   distro's Wine; §1.3, §3.2) stays researched but unbuilt, and R8 doesn't apply yet.
   Unmanaged Proton builds (item 2) are still allowed.
8. **D7VK vs cnc-ddraw: needs its own research (user decision, 2026-11-03).** Which is
   the L1 default for `ddraw.dll` importers, and can the import scan tell 3D D3D≤7
   games from 2D DirectDraw ones (e.g. `IDirect3D7` use)? Not decided here; to be
   researched before the design's renderer decision (R17).
9. **Launcher: a `.desktop` file running a generated script; gogo does setup (user
   decision, 2026-11-03).** The Linux version of the Mac launcher (design 16 §1.2):
   - `~/.local/share/applications/gogo-<gogid>.desktop` (`Exec=` the script, `Icon=`,
     `Categories=Game;`).
   - `games/<gogid>/launch.sh`, generated by gogo and never edited by hand, sources
     `games/<gogid>/launch.conf`. That file holds only assignments (POSIX
     single-quoted, no code), as on the Mac: `GOGID`, `UMU_ID`, `STAMP`, `UMU_RUN`,
     `PROTON_DIR`, `GAME_DIR`, `EXE`, `ARGS`, `ENV`, `DLL_OVERRIDES`, `LOG_DIR`, and
     `UMU_FOLDERS` when needed.
   - The script exports `WINEPREFIX`, `GAMEID`, `STORE=gog`, `PROTONPATH`, the `ENV`
     entries and `WINEDLLOVERRIDES`, then runs `umu-run` from the game directory and
     tees the log.
   - **gogo owns setup**: when the prefix stamp doesn't match `STAMP` (L4 edited,
     GE-Proton bumped), the script runs `gogo setup <gogid>` if gogo is installed and
     re-reads `launch.conf`. Without gogo the game still launches with the last
     setup.
   - Why this over `Exec=gogo launch <gogid>`: games keep working when gogo is removed
     or broken, what runs is readable in `launch.conf`, and the Mac launcher's tested
     pattern carries over. The cost is shell logic next to the Rust logic (R30).

   Still to settle in the design: the early-exit dialog on Linux (`zenity`/`kdialog`, or
   none), and which paths `launch.conf` writes relative to the state dir (the Mac does
   this so state dirs can be copied).
10. **Native Linux installers: a separate epic (user decision, 2026-11-03).** GOG's
    MojoSetup `.sh` installers need no Wine, so they are out of epic #155 and out of
    this design. The design only keeps room for them: `.desktop` files and per-game
    state shouldn't assume Wine.
11. **aarch64 Linux: in the design, nothing built for it alone (user decision,
    2026-11-03).** The design must not assume x86_64. Every catalog entry carries its
    arch (as on the Mac since #82); GE-Proton's `aarch64` asset, `steamrt4-arm64`
    (umu maps appid 4185400) and the umu flake's aarch64 support (unwrapped only) are
    noted where they apply. No issue is filed for aarch64-only work. Anything
    aarch64 needs beyond that waits until someone works on it.

## 9. Risks

Noted for follow-up; none is solved here. Rough severity: **high** blocks the lane or
breaks games silently, **medium** costs users or maintainers real effort, **low** is
worth knowing.

### 9.1 Runtime and umu

| # | Risk | Severity | Why (section) |
|---|---|---|---|
| R1 | The umu zipapp may not launch the Steam Linux Runtime on NixOS without the FHS wrapper that nixpkgs adds | high | untested (§8 item 1) |
| R2 | A pinned umu can fall behind GE-Proton: umu maps runtime appids to runtimes in a hard-coded table (`umu_runtime.py`: soldier, sniper, steamrt4, steamrt4-arm64). A GE-Proton that needs a newer runtime fails until umu is bumped too | high | §1.1, §1.2 |
| R3 | (accepted, §8 item 3) The Steam Linux Runtime isn't pinned by gogo: umu resolves `latest-public-beta.txt` on `repo.steampowered.com` and updates it in `~/.local/share/umu`. The same game can change behaviour with no gogo change, and the first launch needs the network | medium | §1.2 |
| R4 | Fast GE-Proton cadence (8 releases in 3 months, two hotfixes on one day) and one main maintainer: pins go stale quickly, regressions land between our host passes | medium | §1.1 |
| R5 | Disk and bandwidth: 565 MB download and 1.5 GB unpacked per GE-Proton version, plus 175 MB for the runtime; keeping an N-1 rollback doubles it | medium | §1.1 |
| R6 | Python requirement outside Nix: distros with Python < 3.10 (e.g. Debian 11, which ships 3.9, **reported**) can't run umu | medium | §8 item 1 |
| R7 | umu's default prefix with `STORE=gog` is `~/Games/gog`, shared by every game. One missing `WINEPREFIX` mixes games' prefixes | medium | §1.2 |
| R8 | (deferred with the second runtime, §8 item 7) Kron4ek and distro Wine depend on host libraries (32-bit ones too for non-WoW64 builds); on NixOS they need a wrapper | medium | §1.3 |
| R9 | Python code runs at launch: `proton` and protonfixes execute from the downloaded runtime. Today's runtimes are also code, but gogo has never shipped a runtime that runs per-game scripts | low | §2.1 |

### 9.2 Per-game fixes

| # | Risk | Severity | Why (section) |
|---|---|---|---|
| R10 | protonfixes overwrites caller env, so an L4 user override can lose with no warning | high | §4.2 item 2 |
| R11 | Fixes from two owners: L3 and protonfixes both apply verbs, env and overrides for the same game; double runs or conflicting values | medium | §4.2 item 3 |
| R12 | protonfixes runs winetricks at launch (30 of 78 GOG modules), which needs the network the first time and can fail or stall a launch | medium | §2.4 |
| R13 | umu-database is GPL-3.0: copying the CSV (or a derived table) into gogo's repo or binary would bring GPL terms in | medium | §2.3 |
| R14 | Id lookups lag: the CSV inside a Proton build is frozen at its release, and the GitHub fallback (§8 item 5) needs the network once. gogo doesn't use the online API, so its host being down doesn't matter | low | §2.1, §8 item 5 |
| R15 | A GOG game not on Steam gets `umu-<gog id>`, and umu passes the GOG id as `SteamAppId`. Proton's per-appid hacks then key on a GOG id. Today GOG ids (about 1–2 billion) don't overlap Steam appids (about 4.2 million and below), so no collision yet | low | §1.2, §2.2 |

### 9.3 Renderers and drivers

| # | Risk | Severity | Why (section) |
|---|---|---|---|
| R16 | Renderer switching under Proton is all-or-nothing (`PROTON_USE_WINED3D` covers D3D9–12); per-API choices need hand-written overrides | medium | §3.1 |
| R17 | D7VK and cnc-ddraw are both `ddraw.dll`; picking the wrong one for a game, or both being active, breaks it. The import scan can't tell them apart yet | medium | §3.3 |
| R18 | Multi-GPU machines (this one: NVIDIA plus an AMD iGPU): DXVK/VKD3D may pick the wrong device, and the game runs slow or fails | medium | §5 |
| R19 | Missing 32-bit Vulkan ICDs break 32-bit games under classic WoW64, and the error doesn't name the cause | medium | §5 |
| R20 | DXVK's README warns that swapping D3D DLLs in online games can count as cheating and get accounts banned | low | §3.2 |
| R21 | cnc-ddraw under Proton is reasoned from the script, not tested | low | §3.3 |

### 9.4 Project and process

| # | Risk | Severity | Why (section) |
|---|---|---|---|
| R22 | No Vulkan in the VM: CI and agents can't check rendering; every renderer change waits for the user's host pass | medium | §5 |
| R23 | (addressed by §8 item 6 once migrated) Compat data written for the Mac (`renderer = "dxmt"`, `WINEMSYNC`, Mac `tested` entries) would apply on Linux until L3/L4 get a platform dimension | medium | §4.2 item 1 |
| R24 | New archive formats (zstd, zip) mean new crates, which have to pass `cargo deny` | low | §3.2 |
| R25 | GE-Proton's repo has no licence GitHub can detect; the tarball carries Valve's BSD-3 plus each component's licence. Fine for downloading; check it before gogo mirrors or redistributes anything | low | §1.1 |
| R26 | A shared `compatibilitytools.d` grows without bound: `prune` doesn't delete there (§8 item 2), so old GE-Proton builds (1.5 GB each) pile up unless the user removes them. Builds other tools put there aren't sha-pinned either, so gogo can't vouch for them | medium | §8 item 2 |
| R27 | Other tools can delete or replace a build gogo pinned (ProtonUp-Qt cleanups, a user tidying up). A launch then finds its runtime gone or different; gogo has to check before launching. Steam also lists every build there as a compatibility tool | low | §8 item 2 |
| R28 | An unmanaged Proton build silently lacks what gogo expects (protonfixes, D7VK, a known runtime), so `GAMEID`/`STORE=gog` fixes or a `d7vk` renderer do nothing, and L3 `status` can't vouch for it. The **unmanaged** note covers it until gogo can inspect builds | medium | §8 item 2 |
| R29 | Reading a protonfix without running it is approximate: fixes are Python, with conditionals (exe name, file checks), helper functions and game-file edits. The creation-time summary can miss or overstate what the fix does at launch | medium | §8 item 4 |
| R30 | The generated script duplicates some launch logic in shell (env export, stamp check, logging); it can drift from gogo's Rust side, and a stale script keeps running old settings when gogo is gone | low | §8 item 9 |

## 10. Proposed outline for the design doc (`docs/design/18-linux-runtime.md`)

Numbered after design 17; the name is a suggestion.

0. Summary and decisions table
1. Decision 1: runtime (GE-Proton through umu; no second runtime in the first round,
   §8 item 7)
2. Decision 2: umu as a required dependency (zipapp download after confirmation;
   Python from the flake or the user, §8 item 1); where GE-Proton lives (store by
   default, shared `compatibilitytools.d` on request, unmanaged builds, §8 item 2);
   the Steam Linux Runtime left to umu in its shared folder (§8 item 3)
3. Decision 3: renderer mapping (L1 scan → Proton switches; cnc-ddraw vs D7VK, from
   the research in §8 item 8)
4. Decision 4: per-game fixes, detected at app creation (protonfix reading, GOG → umu
   lookup from the build's CSV with a GitHub fallback, L3 seeding, L4 precedence over protonfixes; §8 item 4)
5. Decision 5: compat schema change for platforms: `[platforms.<os>]` tables in one
   file per game (§8 item 6), and migrating today's 13 L3 files
6. Decision 6: store layout and GC on Linux (XDG paths, archive formats, prune, shared
   tools dir)
7. Decision 7: launcher contract (`.desktop` running a generated `launch.sh` that
   sources `launch.conf`, §8 item 9; `gogo-launch.toml`,
   prefix per game, logs, early-exit surfacing)
8. `doctor` on Linux (Vulkan devices, 32-bit ICDs, umu, container, NixOS hints)
9. Scope notes: native Linux installers (separate epic, §8 item 10); aarch64 kept in
   the design, nothing built for it alone (§8 item 11)
10. Testing (unit and wiremock tests; the user's host pass; no VM rendering)
11. Open items, and which risks from docs/21 §9 the design addresses

## 11. Sources

All checked 2026-11-02.

- GitHub API (repo metadata, releases, trees, contents) for
  `GloriousEggroll/proton-ge-custom` (GE-Proton11-11),
  `GloriousEggroll/wine-ge-custom`, `Open-Wine-Components/umu-launcher` (1.4.4, `main`),
  `Open-Wine-Components/umu-database` (`7bd4ba23`), `Open-Wine-Components/umu-protonfixes`
  (`4500d09f`), `Open-Wine-Components/umu-proton`, `ValveSoftware/Proton`,
  `doitsujin/dxvk` (v3.1.1), `HansKristian-Work/vkd3d-proton` (v3.1),
  `WinterSnowfall/d7vk` (v2.4), `Kron4ek/Wine-Builds` (11.21).
- Source read: `umu/umu_run.py`, `umu/umu_runtime.py`, `umu/umu_consts.py`,
  `umu/umu_proton.py`, `docs/umu.1.scd` (umu-launcher `main`); `fix.py`
  (umu-protonfixes `master`); the `proton` script, `protonfixes/util.py`,
  `protonfixes/__init__.py` and `gamefixes-gog/` inside the GE-Proton11-11 x86_64
  tarball; `src/backend/wiki_game_info/umu/utils.ts` (Heroic, `main`);
  `lutris/util/wine/proton.py` (Lutris, `master`); `src/winetricks` (Winetricks
  `master`, `20260125-next`).
- READMEs: umu-database, DXVK, VKD3D-Proton, Kron4ek/Wine-Builds, wine-ge-custom.
- Downloads inspected (not run): GE-Proton11-11 x86_64 (sha512 matched), `dxvk-3.1.1.tar.gz`,
  `vkd3d-proton-3.1.tar.zst`, `wine-11.21-staging-amd64-wow64.tar.xz` (all sha256 matched
  the asset digests).
- Live HTTP: `https://umu.openwinecomponents.org/umu_api.php` (`?store=gog`,
  `?store=gog&codename=1207658883`); HEAD on
  `repo.steampowered.com/steamrt4/images/latest-public-beta/SteamLinuxRuntime_4.tar.xz`
  and the sniper equivalent.
- nixpkgs (flake registry, `lib.version` 26.11; and gogo's pin `e7439b6b`):
  `umu-launcher`, `proton-ge-bin`, `dxvk`, `vkd3d-proton`, `wineWow64Packages`,
  `winetricks` versions via `nix eval`; `umu-run --help` run from nixpkgs.
- Host: `vulkaninfo --summary` (nixpkgs `vulkan-tools`).
- **Reported only:** wined3d's Vulkan backend via `WINE_D3D_CONFIG`; cnc-ddraw behaviour
  under Proton.
