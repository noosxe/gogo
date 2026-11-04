# 22 — DirectDraw renderers: D7VK vs cnc-ddraw, and telling 3D from 2D (#150)

**Research date:** 2026-11-03 (all probes the same day)
**Test bed:** NixOS 26.11, x86_64, kernel 7.2.11-zen1 (as docs/21). No game, installer or
Wine binary was run: release archives were hashed and listed, DLLs were read with
`objdump -p`/`strings`, and game files were read by a throwaway Python scanner
(`pefile` from nixpkgs, not committed; §5.2).
**Builds on:** docs/21 §3.3, §8 item 8 and risk R17 (branch
`claude/linux-runtime-research-0bdd7d`, PR #161); docs/16 (cnc-ddraw, S1); docs/19 §6.2
(L1 table); `crates/gogo-core/src/macapp/{pe,renderer,ddraw}.rs`.
**Status:** research only. No code changed. Facts are marked **verified** (GitHub API, a
source file or script read, an archive listing or hash, a binary read) or **reported**
(the project's own docs, not re-checked).

## 0. Summary

- **They cover different halves of `ddraw.dll`, so neither replaces the other.**
  cnc-ddraw re-implements DirectDraw for 2D games and stubs Direct3D. Its
  `IDirect3D7::CreateDevice` returns `E_FAIL` (**verified**, source). D7VK implements
  Direct3D 7/6/5/3 (immediate mode) on DXVK's D3D9 Vulkan backend. It passes plain
  DirectDraw through to Wine's own `ddraw` ("no benefit" for 2D, its README says).
- **The failure modes are asymmetric.** cnc-ddraw on a 3D game breaks the game (no D3D
  device). D7VK on a 2D game costs nothing: it behaves like Wine's builtin `ddraw`. Only
  the cnc-ddraw fixes (scaling, windowing, mode-change workarounds) are lost. So when the
  scan is unsure, cnc-ddraw is the wrong guess.
- **D7VK can't run on the Mac lane.** It requires `robustBufferAccess2`, `nullDescriptor`
  and `geometryShader`. MoltenVK 1.4.2 reports the first two as `false` and never enables
  the third (**verified**, both sources, §3). The Mac keeps cnc-ddraw for 2D and wined3d
  for D3D≤7.
- **gogo already scans for the `IID_IDirect3D*` GUIDs, but the signal is noisy.** `pe.rs`
  marks a ddraw importer as D3D≤7 if any of the four `IID_IDirect3D`/`2`/`3`/`7` GUIDs is
  in its sections. On this machine's files, a contiguous 1,312-byte block of 43 of the 44
  ddraw/d3d GUIDs (all of them, the `dxguid` set) turns up in D3D8, D3D11 and even input
  DLLs that never touch Direct3D 7 (§5.2). A 2D game linked against the whole set would be
  misread. The GOG 2D control, Stronghold Crusader HD, carries only `IID_IDirectDraw7` and
  classifies correctly (**verified**).
- **Recommendation (§6):** on Linux, D3D≤7 evidence → D7VK (`PROTON_USE_D7VK=1`);
  confident 2D → cnc-ddraw as on the Mac; **ambiguous → D7VK, not cnc-ddraw**; a
  `ddraw.dll` the game ships itself → keep it, use neither. The detection rule changes in
  three ways: discount the full `dxguid` block, add the device/texture GUIDs as signals,
  and check GUIDs in runtime-loading files too.

## 1. The two candidates

| | cnc-ddraw | D7VK |
|---|---|---|
| Repo, licence | `FunkyFr3sh/cnc-ddraw`, MIT (**verified**) | `WinterSnowfall/d7vk`, zlib (**verified**) |
| What it implements | DirectDraw 1–7 for 2D games (the `IDirectDraw*`, surface, palette, clipper, gamma interfaces). The `IDirect3D`/`2`/`3`/`7` objects are stubs: `QueryInterface` succeeds, `EnumDevices` lists nothing, `CreateDevice` fails (**verified**, `src/IDirectDraw/IDirectDraw.c`, `src/IDirect3D/IDirect3D7.c`). Its README: "does not support Direct3D games" | Direct3D 7, 6, 5 and 3 (immediate mode, D3D3 execute buffers included). Not retained mode: "D3D retained-mode applications are NOT supported", so `d3drm.dll` games are out. It is not a DirectDraw implementation: it proxies DDraw calls to Wine's (or Windows') `ddraw` (**reported**, README; `ddraw_.dll` string in the DLL **verified**) |
| How it renders | Its own blitter onto GDI, OpenGL (GLSL shaders) or Direct3D 9 (`renderer=auto\|opengl\|openglcore\|gdi\|direct3d9\|direct3d9on12`, auto = D3D9/OpenGL, then GDI) (**verified**, `ddraw.ini`). Under Proton its D3D9 path would land on DXVK | D3D≤7 state is translated onto a modified DXVK D3D9 backend on Vulkan. DXVK's `d3d9.*` options and `DXVK_HUD` apply, plus `ddraw.*` options (`emulateFSAA`, `backBufferResize`, `forceSWVP`, `forceLegacyModeFilter`) (**reported**, README) |
| Scaling, windowing | Windowed, borderless, fullscreen-in-window, aspect ratio, integer scaling, shader upscaling (xBR, FSR, CRT …), FPS limiter, vsync, mouse fixes, Alt+Enter. A `cnc-ddraw config.exe` GUI (**verified**, ini and zip) | No forced resolution. On Wayland, small-picture mode-change problems are left to gamescope, Wine's virtual desktop or "Emulate display mode changes". `ddraw.backBufferResize` is a partial workaround. Forced AA, frame rate caps per game (**reported**, README) |
| Per-game data | `ddraw.ini` ships with 287 per-exe sections (288 `[…]` headers, one is `[ddraw]`). The README lists 439 "Supported Games" (**verified**) | `src/util/config/config.cpp` carries about 136 D3D≤7 game profiles (64 D3D7, 48 D3D6, 21 D3D5, 3 D3D3) (**verified**, v2.4). The issue tracker is its compatibility list (256 issues, 2 with "GOG" in the title) |
| GPU needs | Any. GDI needs no 3D at all | Vulkan 1.3 instance, plus `geometryShader`, `robustBufferAccess2`, `nullDescriptor`, `maintenance5`/`6` and more (**verified**, `dxvk_instance.h`, `dxvk_device_info.cpp`). Its README speaks of a "Vulkan 1.4 capable GPU". DXVK-Sarek backports it to Vulkan 1.1 hardware |
| Bitness | 32-bit `ddraw.dll` only | 32-bit `x32/ddraw.dll` only (**verified**, zip listing) |
| Release | `v7.1.0.0`, 2024-12-28. Assets `cnc-ddraw.zip` (2,688,079 B), `cnc-ddraw_legacy_WindowsME.zip`, `ddraw.pdb`, `proxy-dlls.zip`. **No asset digests.** The zip holds `ddraw.dll` (411,648 B), `ddraw.ini`, `cnc-ddraw config.exe`, `Shaders/` | `v2.4`, 2026-10-28. One asset, `d7vk-v2.4.zip` (3,428,975 B), with digest `sha256:41a1c803…` (matched), holding `d7vk-v2.4/x32/ddraw.dll` (8,900,608 B) |
| gogo pin | catalog `cnc-ddraw` = v7.1.0.0, sha256 `0b13ab89…bb908`, re-matched today (**verified**) | none |
| Maintenance | One main author (2,654 commits). Since 2025-01-01: 65 commits; since 2026-05-03: 2 (latest 2026-08-30). An `experimental` pre-release whose assets were last rebuilt 2025-07-08 (**verified**) | Active. Releases v1.10 (2026-05-19) to v2.4, roughly monthly, plus a `v2.5_WIP` tag. 676 commits on `devel` since 2026-05-03, upstream DXVK merges included; pushed 2026-11-02 (**verified**) |
| Shipped by | Sikarugir (default on), winetricks `cnc_ddraw` (pinned v7.0.0.0), GOG forum guides (docs/16); protonfixes falls back to it for Red Alert 2 (Steam 2229850) | GE-Proton (since 11-x, off by default), Proton-CachyOS (Heroic's description: "includes extra tools like DXVK-Sarek and D7VK"). No winetricks verb (**verified**, winetricks 20260125-next) |

One more DirectDraw/D3D≤7 option exists: dgVoodoo2 (closed freeware, DDraw + D3D1–7 +
Glide → D3D11). umu-protonfixes has a `dgvoodoo2` verb (**verified**, `verbs/dgvoodoo2.verb`).
It is out of scope here because its licence stops gogo from redistributing it, as docs/19
noted.

## 2. Installing and enabling

### 2.1 Under GE-Proton / umu

GE-Proton11-11's `proton` script, `setup_ddraw(use_d7vk)` (**verified**, script at tag
`GE-Proton11-11`):

- `PROTON_USE_D7VK=1`: copies Wine's 32-bit `ddraw.dll` to `syswow64/ddraw_.dll` and
  D7VK's to `syswow64/ddraw.dll`, then appends `ddraw=n,b` to `WINEDLLOVERRIDES`. This is
  the "permanent" layout from D7VK's README, which some games need (it names GTA 2,
  StarLancer, Midtown Madness 2) because they load `ddraw` from the system path.
- Unset: if a previous run left `ddraw_.dll`, it restores Wine's `ddraw.dll`. So turning
  D7VK off is clean per prefix.
- `append_to_env_str` keeps the caller's `WINEDLLOVERRIDES`. gogo's game-dir cnc-ddraw
  (`ddraw=n,b` plus `ddraw.dll` next to the exe) keeps working under Proton when
  `PROTON_USE_D7VK` is unset. **If both are set, the game-dir DLL wins** (the application
  directory comes before `syswow64` in the search order). D7VK is then silently unused.
- GE-Proton's D7VK is a submodule at a `devel` commit, not a release tag:
  `bcd66d0` (2026-10-17) in GE-Proton11-11, `06656b9` (2026-11-02) in GE-Proton11-12,
  released today (**verified**). The D7VK in use follows the GE-Proton pin, not D7VK's
  releases.
- Proton forces `ddraw=n,b` for five Steam ids that ship their own `ddraw.dll` (Arcanum,
  Resident Evil, Resident Evil 2, Dino Crisis, Breath of Fire IV). umu-protonfixes does the
  same for the GOG builds of Resident Evil, Dino Crisis 1/2, Breath of Fire IV, Metal Gear
  Solid and The Wheel of Time ("GOG's dxcfg") (**verified**, `gamefixes-gog/umu-*.py`).
  **GOG often ships its own `ddraw.dll` wrapper.** gogo's `local_overrides` already keeps
  it (`renderer.rs`, "the game's own ddraw.dll is kept").
- protonfixes already picks D7VK for one game: Tex Murphy: Overseer (Steam 302370,
  `PROTON_USE_D7VK=1`). It keeps it off for Gabriel Knight 3 ("d7vk flickers with this
  game") (**verified**).

### 2.2 Into a plain Wine prefix (Kron4ek lane, docs/21 §1.3)

- **cnc-ddraw**: the same as the Mac today. `ddraw.dll` and the ini go in the game dir,
  plus `ddraw=n,b` (`ddraw.rs`). winetricks puts it in the system dir instead.
- **D7VK**: either game-dir `ddraw.dll` plus `ddraw=n,b` ("expected to work in most
  cases"), or the `syswow64` swap above (rename Wine's to `ddraw_.dll`) (**reported**,
  README). Game-dir placement mirrors cnc-ddraw and keeps the prefix clean. The
  `syswow64` swap is what Proton does and what some games require.
- D7VK's asset is a zip. `store.rs` already unpacks zips (for cnc-ddraw:
  `"zip" => …` in its extractor) (**verified**), so docs/21 §3.2's note that D7VK
  needs a zip extractor is already covered. Only the `d7vk-vX/x32/` path inside the
  zip is new.

## 3. D7VK on macOS: no

- D7VK v2.4 loads `vulkan-1.dll` at run time (**verified**, strings). In a Mac Wine, that
  means winevulkan on MoltenVK.
- Its device creation fails unless the GPU has these required features (**verified**,
  `dxvk_device_info.cpp` at v2.4): `geometryShader`, `shaderCullDistance`,
  `robustBufferAccess2`, `nullDescriptor`, `depthClipEnable`, `maintenance5`,
  `maintenance6`, and others.
- MoltenVK v1.4.2 (2026-07-24) sets `robustBufferAccess2 = false` and
  `nullDescriptor = false`, and doesn't enable `geometryShader` anywhere in
  `MVKDevice.mm` (**verified**). Metal has no geometry shaders.
- So D7VK can't create a device on the Mac. That matches docs/19 §6.2, where D9VK on
  MoltenVK crashed TAG. DXVK-Sarek (Vulkan 1.1, D7VK backport, needs `d3d9.dll` too)
  is the only Vulkan route left, and it is untested on MoltenVK (open question, §7).
- **The Mac table doesn't change**: cnc-ddraw for 2D (32-bit), wined3d for D3D≤7. The
  detection fixes in §6.2 apply to both lanes.

## 4. GOG games

- **D7VK profiles that are GOG products** (**verified**, `catalog.gog.com` search for each
  title): Arx Fatalis, Gothic, Sacrifice, Startopia, Escape from Monkey Island, Deus Ex
  GOTY, The Wheel of Time, Hitman: Codename 47, Wizardry 8, Age of Wonders: Shadow Magic,
  Earth 2150 Trilogy, Might and Magic 7, Omikron, Dungeon Keeper 2, Star Wars: Rogue
  Squadron 3D, Total Annihilation: Kingdoms, Interstate '76, Tex Murphy: Overseer,
  FreeSpace 2, Resident Evil. Also Carmageddon TDR 2000 and Anno 1503, which this
  account owns. Two D7VK issues name GOG builds: X-Wing Alliance (crash on start) and
  Dungeon Keeper 2 (missing text/cursor).
- **cnc-ddraw's list** is mostly 2D: Age of Empires, Caesar III, Pharaoh (live-proven,
  docs/16), Stronghold, Fallout, Baldur's Gate, Command & Conquer, Commandos, Chaos Gate
  and so on.
- **Games on both lists (dual renderers)**: Age of Wonders 2 and Shadow Magic, Dungeon
  Keeper 2, Blood II, Shogo, Total Annihilation: Kingdoms, Wizardry 8, Unreal, MechWarrior
  3, Interstate '76, Nightmare Creatures, Virtua Fighter 2 (**verified**, name match of
  the two lists). They ship a software DirectDraw renderer **and** a D3D one, so the right
  wrapper depends on an in-game setting the scan can't see. The default should follow the
  game's default renderer (usually D3D on a modern GPU): D7VK on Linux. cnc-ddraw stays a
  manual `--renderer` choice.
- **GOG-shipped `ddraw.dll`**: Resident Evil, Dino Crisis 1/2, Breath of Fire IV, MGS,
  The Wheel of Time (§2.1). D7VK *and* cnc-ddraw would both have to replace the game's
  own DLL. gogo should keep the game's DLL (as it does) and let L3 decide per game.
- This account owns Carmageddon TDR 2000 (D3D6/7, D7VK profile, gogo compat entry: L1 →
  wined3d), Anno 1503 (D7VK profile), Pharaoh, Zeus, Anno 1602, SimCity 3000, Fallout,
  Heroes 3, Chaos Gate and Stronghold HD / Crusader HD. That is a ready corpus for §7
  item 1.

## 5. Can a static scan tell D3D≤7 from 2D DirectDraw?

### 5.1 What `pe.rs` does today

- Scans the main exe and every `.exe`/`.dll` in its directory. Imports and delay imports
  through `object`.
- **Only in files that import `ddraw.dll`**, it searches section data for four GUIDs:
  `IID_IDirect3D`, `IID_IDirect3D2`, `IID_IDirect3D3`, `IID_IDirect3D7` (`D3D7_IIDS`). A
  hit adds `GraphicsImport::Direct3d7`. Importing `d3dim`, `d3dim700` or `d3drm` also
  does.
- `classify`: any `Direct3d7` → `D3dLegacy` (wined3d). `ddraw` alone → `Ddraw2d`
  (cnc-ddraw if 32-bit).
- A file that only `LoadLibrary`s ddraw (name found as a string, no import) gets **no
  GUID check**. If nothing else is imported, the game classifies as `Ddraw2d` from
  `runtime_graphics`.

So the background claim that "any `ddraw.dll` importer maps to cnc-ddraw" is already
out of date. The GUID check exists. The open question is how reliable it is.

### 5.2 Test on real files

The scanner read every `.exe`/`.dll` under the Steam libraries and a GOG install
(Stronghold Crusader HD, installed from GOG's offline installer). It matched the 44
`DEFINE_GUID`s of Wine's `d3d.h` and `ddraw.h` byte-for-byte and recorded the graphics
imports. None of these files was run.

| File | Imports | ddraw/d3d GUIDs found | `pe.rs` today | Right answer |
|---|---|---|---|---|
| Stronghold Crusader HD (GOG) `Stronghold Crusader.exe`, `Stronghold_Crusader_Extreme.exe` | `ddraw` (`DirectDrawCreate` only) | 1: `IID_IDirectDraw7` | `Ddraw2d` → cnc-ddraw | ✓ 2D (cnc-ddraw list) |
| Same game, Steam build | same | same | same | ✓ |
| Dangerous Waters `dangerouswaters.exe` | `d3d8` | **43/44, contiguous 1,312 B** (the whole `dxguid` d3d/ddraw set) | `D3dLegacy` via `d3d8` | ✓, but the GUIDs are noise |
| Divinity: Original Sin 2 `EoCApp.exe`, A Plague Tale `…_x64.exe` | `d3d11`, `dxgi` | 43/44 (block) | `D3d11` | ✓ (GUIDs ignored: no ddraw import) |
| `input_di8.dll`, `WwiseLibPCx64R.dll`, a launcher DLL | no graphics import | 43/44 (block) | not a ddraw importer | GUIDs are pure noise |
| A screen recorder exe (Once Human) | `ddraw` + `d3d11` + `dxgi` | 16: ddraw IIDs and CLSIDs only | `D3d11` | ✓ |
| PunkBuster `pbcl.dll` (BF4, Crysis) | `ddraw` (screenshots) | 0–1 | `Ddraw2d` *if* it sat next to the exe | ✗ would be noise. It sits in `pb/`, so it isn't scanned |
| 688(I) Hunter-Killer, Fleet Command exes | `kernel32` only, entropy 8.0 | 0 | `Unknown` | packed: nothing static to read |
| 688(I) `RWDL6A21.DLL` (RenderWare driver) | `gdi32`, `user32` | 0, string `ddraw.dll` | `Ddraw2d` from strings | ✓ (software rasterizer) |

Findings:

1. **The `dxguid` block is real and common.** Code that links the whole ddraw/d3d GUID
   set gets 43 GUIDs (all of Wine's `d3d.h`/`ddraw.h` except `IID_IDirectDraw3`) in one contiguous run.
   It shows up in D3D8 and D3D11 games and in DLLs with no graphics at all. In a ddraw
   importer, that block says only "linked `dxguid`", not "uses Direct3D". Today's rule
   would call such a 2D game D3D≤7 and lose cnc-ddraw on the Mac.
2. **Selective GUIDs look like real use.** Stronghold has exactly the one interface it
   queries (`IID_IDirectDraw7` after `DirectDrawCreate`). A 2D game that probes for 3D
   hardware may still carry `IID_IDirect3D*` alone (cnc-ddraw stubs those interfaces for
   exactly that reason). **Device GUIDs** (`IID_IDirect3DHALDevice`, `…TnLHalDevice`,
   `…RGBDevice`, `…RefDevice`, `…MMXDevice`) and **D3D work GUIDs**
   (`IID_IDirect3DTexture2`, `…Device*`, `…VertexBuffer*`, `…Viewport*`) appear only when
   a device is created or a texture or viewport is queried. Outside the block, they are
   the strong signal.
3. **Not tested here: a real D3D≤7 GOG exe.** None was on disk (this account owns
   Carmageddon TDR 2000 and Anno 1503 but hasn't downloaded them). gogo's compat entry
   for TDR records that L1 already picks wined3d for it, so the four-GUID check fires on
   at least one real D3D6/7 game (**reported**, `compat/1146738698.toml`).

### 5.3 Signals, ranked

| Signal | Strength | False positive | False negative |
|---|---|---|---|
| Imports `d3dim.dll`/`d3dim700.dll` | strong 3D | — | rare: games reach D3D through `ddraw` |
| Imports `d3drm.dll`/`d3dxof.dll` | strong 3D, **retained mode** | — | — (D7VK doesn't support RM; → wined3d) |
| Device GUID (HAL/TnLHal/RGB/Ref/MMX/Ramp) outside the block | strong 3D | a 2D game that enumerates devices for a caps screen (unlikely) | games that pass the GUID from the `EnumDevices` callback carry no device GUID |
| D3D work GUIDs (`Texture2`, `Device*`, `VertexBuffer*`, `Viewport*`, `Material*`) outside the block | strong 3D | — | as above, but most D3D3/5/6 code queries `IID_IDirect3DTexture2` |
| `IID_IDirect3D`/`2`/`3`/`7` outside the block (today's rule) | medium | 2D games that probe for D3D | — |
| The 43-GUID `dxguid` block | **none** (ambiguous) | high | — |
| `DirectDrawCreateEx` import, `IID_IDirectDraw7` | none (2D DD7 exists: Stronghold) | high | — |
| A renderer DLL next to the exe: Unreal `D3DDrv.dll`/`SoftDrv.dll`/`GlideDrv.dll`/`OpenGLDrv.dll`; LithTech `*.ren` (Blood II, Shogo); Glide `glide2x.dll`/`glide3x.dll` shipped | strong "has a 3D path" (and possibly a software one) | — | `.ren` files aren't scanned today (`pe_files_in` takes `.exe`/`.dll`) |
| Strings `Direct3D`, `RGB Emulation`, `Direct3D HAL` | weak | menus of 2D games, shared code | — |
| Packed exe (entropy ≈ 8, `kernel32`-only imports) | — | — | **total**: nothing to read. Stay `Unknown` → wined3d, never guess 2D |

## 6. Recommendation

### 6.1 L1 default for ddraw-class games

| Scan result | Linux (GE-Proton / Wine) | macOS |
|---|---|---|
| Game ships its own `ddraw.dll` (`local_overrides`) | keep it, `ddraw=n,b`. No cnc-ddraw, no D7VK (as protonfixes does) | same (today's behaviour) |
| D3D≤7 **immediate mode**: device/work GUID outside the block, or `d3dim*` import, or a D3D renderer DLL | **D7VK**: `PROTON_USE_D7VK=1` under Proton, game-dir `ddraw.dll` + `ddraw=n,b` on plain Wine | wined3d |
| D3D≤7 **retained mode** (`d3drm`/`d3dxof`) | wined3d (Proton default; D7VK doesn't do RM) | wined3d |
| Only `IID_IDirect3D*` outside the block (probe-only or real use) | D7VK (harmless if it's 2D) | wined3d (as today) |
| `ddraw` + only the `dxguid` block, no other signal | **D7VK** (the safe guess: §0 asymmetry) | wined3d. Or cnc-ddraw, if the Mac's 2D failures are judged worse than a broken 3D game; decide with a host pass |
| `ddraw`, selective DirectDraw GUIDs only, 32-bit | **cnc-ddraw** (as the Mac; docs/16) | cnc-ddraw |
| `ddraw` from strings only (runtime-loaded) | run the same GUID check on that file first, then as above | same |
| Packed or nothing found | Proton default (wined3d `ddraw`) | wined3d |

- **D7VK, not cnc-ddraw, is the Linux default whenever 3D is possible.** Under Proton a
  2D game on D7VK gets Wine's own DirectDraw, which is what Proton gives every Steam
  ddraw game by default. A 3D game on cnc-ddraw gets no device.
- **cnc-ddraw stays the default for confident 2D on both lanes.** Whether Linux needs it
  as much as the Mac does is untested: Wine on X11/Wayland handles mode changes better
  than the Mac driver. Keep it for consistency and its scaling. A Linux host pass on
  Pharaoh decides (§7 item 2).
- **Never both.** gogo must not set `PROTON_USE_D7VK` while a game-dir `ddraw.dll` is in
  place (theirs or gogo's), and must remove gogo's cnc-ddraw when it switches a game to
  D7VK. `ddraw::remove_gogo` already exists for the wined3d switch. This closes R17.
- `--renderer cnc-ddraw|d7vk|wined3d` (and L3's `renderer`) override L1 per game, for the
  dual-renderer titles in §4.

### 6.2 Detection rule (for `pe.rs`)

1. Extend `D3D7_IIDS` into two groups: **interface** (`IID_IDirect3D`, `2`, `3`, `7`) and
   **device/work** (the 7 device GUIDs plus `IID_IDirect3DDevice*`, `Texture*`,
   `VertexBuffer*`, `Viewport*`, `Material*`, `ExecuteBuffer`, `Light`). That is 27 D3D
   GUIDs, all in Wine's `d3d.h`.
2. Count the 44 ddraw/d3d GUIDs per file. **If ≥ 40 are present, the file carries the
   `dxguid` block. Its D3D hits don't count** (record `dxguid_block: true` for `doctor`).
   The threshold leaves headroom under the 43 seen.
3. Otherwise any device/work GUID → `Direct3d7` (strong); interface GUID only →
   `Direct3d7` (medium). A `confidence` field would let `renderer.rs` treat "medium" and
   "block only" differently, if a host pass shows that matters.
4. Run the GUID check in files that **load `ddraw.dll` by name** too, not only in
   importers (the `wants_iids` condition).
5. Add renderer-DLL names to the engine hints: Unreal `D3DDrv.dll`, LithTech `d3d.ren`
   (scan `.ren` as PE). These mark a 3D path even if the exe is clean.
6. Leave the 64-bit rule as is: D3D≤7 games are 32-bit, and both wrappers are 32-bit only.

Cost: one more 16-byte comparison pass per ddraw-related file, which `pe.rs` already
reads in full. No new dependency.

**As built (#173):** the rule is in `pe.rs` as above, with Wine `wine-11.0`'s 44 GUIDs.
In step 2, the block's D3D hits stay D3D≤7 at the lowest confidence (`dxguid_block`),
following §6.1's "only the `dxguid` block" row, rather than dropping to 2D. Details,
output fields and class transitions: design 18 §3 "As built (#173)".

## 7. Open questions for the Linux design doc

1. **A real D3D≤7 corpus.** Download (not run) Carmageddon TDR 2000, Anno 1503, Pharaoh,
   Zeus, SimCity 3000 and Chaos Gate from this account. `innoextract` them and run the
   §6.2 rule. Do D3D≤7 GOG exes carry selective GUIDs or the block? This decides whether
   "block only" can lean 2D. Needs the user's go-ahead (account downloads, ~3 GB).
2. **Does Linux need cnc-ddraw for 2D at all?** Host pass: Pharaoh and Stronghold under
   GE-Proton with Wine's `ddraw`, then with cnc-ddraw. On X11 and on Wayland. If Wine
   alone is fine, the Linux 2D default could be "Proton default" and cnc-ddraw opt-in.
3. **The D7VK pin.** Follow GE-Proton's submodule (a `devel` snapshot), or ship D7VK
   release zips for the plain-Wine lane and for pinning? Two different D7VKs per lane
   otherwise.
4. **`syswow64` vs game dir for D7VK on plain Wine.** Proton's swap is what some games
   need. gogo's game-dir pattern is cleaner. Pick one, or make it an L3 field.
5. **Dual-renderer games** (§4): default to D7VK and document the cnc-ddraw override, or
   seed L3 entries for the known ones?
6. **Wayland and D7VK.** Its README's small-picture issue suggests gamescope or a virtual
   desktop. Does the Linux lane run old games in gamescope by default?
7. **DXVK-Sarek on MoltenVK** for the Mac's D3D≤7 games: worth one VM/host probe, or
   settled by the D9VK crash in docs/19?
8. **Retained-mode (`d3drm`) GOG games**: are there any in the catalog? If none matter,
   the RM row is moot.

## 8. Sources

- cnc-ddraw: <https://github.com/FunkyFr3sh/cnc-ddraw> (README, `ddraw.ini`,
  `src/IDirectDraw/IDirectDraw.c`, `src/IDirect3D/IDirect3D7.c` on `master`), release
  v7.1.0.0 (2024-12-28): <https://github.com/FunkyFr3sh/cnc-ddraw/releases/tag/v7.1.0.0>
- D7VK: <https://github.com/WinterSnowfall/d7vk> (README on `devel`; `src/util/config/config.cpp`,
  `src/dxvk/dxvk_instance.h`, `src/dxvk/dxvk_device_info.cpp` at `v2.4`), release v2.4
  (2026-10-28): <https://github.com/WinterSnowfall/d7vk/releases/tag/v2.4>, issues:
  <https://github.com/WinterSnowfall/d7vk/issues>
- DXVK-Sarek: <https://github.com/pythonlover02/DXVK-Sarek> (named in D7VK's README; not read)
- GE-Proton: <https://github.com/GloriousEggroll/proton-ge-custom> (`proton` at
  `GE-Proton11-11`: `setup_ddraw`, `append_to_env_str`, the `ddraw` overrides by
  `SteamGameId`; `d7vk` submodule at GE-Proton11-11 and 11-12)
- umu-protonfixes: <https://github.com/Open-Wine-Components/umu-protonfixes>
  (`gamefixes-steam/302370.py`, `497360.py`, `2229850.py`; `gamefixes-gog/umu-1587885678.py`,
  `umu-1661434114.py`, `umu-1584652180.py`, `umu-1771973390.py`, `umu-1597741823.py`,
  `umu-1580232252.py`, `umu-1207661743.py`; `verbs/dgvoodoo2.verb`)
- MoltenVK v1.4.2 (2026-07-24): <https://github.com/KhronosGroup/MoltenVK>
  (`MoltenVK/MoltenVK/GPUObjects/MVKDevice.mm`)
- Wine headers (GUIDs): <https://github.com/wine-mirror/wine/blob/master/include/d3d.h>,
  <https://github.com/wine-mirror/wine/blob/master/include/ddraw.h>
- winetricks 20260125-next, `cnc_ddraw` verb: <https://github.com/Winetricks/winetricks>
- Heroic Wine Manager (Proton-CachyOS description):
  <https://github.com/Heroic-Games-Launcher/HeroicGamesLauncher> (`src/frontend/screens/WineManager/index.tsx`)
- GOG catalog search (product ids in §4): `https://catalog.gog.com/v1/catalog?query=like:<title>`
