# 19 — Making `mac-app` bundles actually run: runtime, renderers, per-game data

**Research date:** 2026-10-22/23 (landscape probes, host test runs on 2026-10-22, VM test
runs on 2026-10-23)
**Test beds:** host Apple Silicon M4 Pro, macOS 27.0.1, Rosetta 2; clean macOS 27.0 Tart
guest via `scripts/vm-test.sh` (docs/vm-testing.md)
**Builds on:** docs/13 (wrapper landscape), docs/14 (Lutris recipes), docs/15 (runtime
distribution), docs/16 (GOG solidification), docs/18 (depot lane); designs 10, 11, 12, 15;
issues #53–#56 (epic #50).
**Status:** research + recommendation. No code changed. Facts are marked
**verified** (checked live: a probe, an API call, a binary inspection or a test run) or
**reported** (consistent public sources, not re-checked).

## 0. Summary

`gogo mac-app` reliably builds a `.app`. Whether the game in it runs depends on four
things, in this order of impact:

1. **The renderer layer, not the per-game recipe.** Stock Wine on macOS does
   Direct3D through `wined3d` on OpenGL 4.1. That fails D3D10/11 games before
   any per-game fix matters (Neo Cab: `d3d11: failed to create device` on every
   wined3d runtime; it starts on DXMT and on D3DMetal — **verified**, §5). Every
   working macOS stack (CrossOver, Sikarugir, Heroic) picks a renderer **per
   Direct3D API**. gogo has only the cnc-ddraw row.
2. **The runtime lineage.** On gogo's default, Gcenx `wine-staging` 11.x, three of
   the six test titles failed for runtime reasons (Rebel Galaxy, Neo Cab, Snake
   Core). On the CrossOver-source engine (`WineCX24.0.7`, built by Gcenx for
   Sikarugir), with DXMT for the two D3D11 titles, every title reached its own
   UI or first-run dialog. That engine also found FreeType on a clean Mac (#54)
   and takes DXMT/D3DMetal drop-ins (§5).
3. **The bundle model.** A runtime and a Wine prefix frozen into each signed
   `.app` cause #53 (seal broken by Wine's later writes), #55 (builder-home
   symlinks) and — the big one — leave no way to move existing bundles to a new
   runtime when **Rosetta stops being general-purpose in macOS 28** (Apple,
   **verified**). Every Wine on macOS today is x86_64; the ARM64 + FEX path
   needs restricted entitlements a locally ad-hoc-signed bundle cannot carry
   (§7). Our own bundles already show "*will not open in macOS 28*" in the VM.
4. **Per-game data, last.** Lutris scripts were the wrong layer: on Linux the
   compatibility comes from the Lutris *runner* (Wine-GE/Proton + DXVK + VKD3D),
   while scripts mostly encode installer logistics (§2). GOG's own depot
   `gog-support/<id>/app/` overlay — already downloaded, currently ignored —
   turned Carmageddon TDR 2000 from a crash into its normal first-run flow.

**Recommendation (§6):** **(L0)** one curated runtime track, the CrossOver-source
engine today, **referenced by id from a gogo store instead of copied into each
bundle**, so it can be swapped in place (and replaced by an entitlement-signed
ARM64 runtime when one exists) → **(L1)** renderer picked automatically from the
game's PE imports and bitness (DXMT for D3D10/11, wined3d for D3D≤9, cnc-ddraw for
2D DDraw, D3DMetal on consent, native `d3dcompiler`/`d3dx` verbs when imported) →
**(L2)** GOG-native fixes (support overlay, support installer, all `playTasks`),
depot lane by default → **(L3)** a small, declarative, GOG-id-keyed compat DB in this
repo, seeded from umu-protonfixes and from VM runs → **(L4)** a user override file
read at every launch. The signed `.app` becomes a small immutable launcher; the
prefix, game files and saves live in `~/Library/Application Support/gogo/games/<gogid>/`;
`--portable` keeps today's fat bundle for copying to another Mac. The launcher logs
every run and offers "try next renderer" on failure.

## 1. The pipeline today and where it is weak

What `gogo mac-app` does (code: `crates/gogo-core/src/macapp/`):

| Stage | Code | What happens |
|---|---|---|
| Recipe pick | `lutris.rs`, `interpret.rs` | Live lutris.net search by display name, ranked by `gogid` match, user picks one (or `none`); parity executor runs the declarative subset (registry overrides, `winetricks` verbs, `merge`/`write_config`, launch args/env); host-shell steps skipped. Depot lane: recipes forced off (design 15). |
| Runtime | `catalog.rs`, `store.rs`, `runtime.rs` | Interactive menu over Gcenx `macOS_Wine_builds` (staging/devel/stable 11.x, x86_64 under Rosetta), GPTK 3.0-3 (Gcenx-packaged, wine 7.7 + D3DMetal, consent-gated), brew, custom path. `wine-crossover` is a tombstone. |
| Prefix | `installer.rs` | `wineboot -u` into `Contents/Resources/prefix` **inside the bundle**. |
| Install | `installer.rs` / `bundle::clone_tree` | Inno `/VERYSILENT` under the chosen wine, or an APFS clone of the depot tree. |
| Fixes | `ddraw.rs`, `tools.rs` | Optional cnc-ddraw (game-dir `ddraw.dll` + ini + `ddraw=n,b`); winetricks verbs only when a recipe asks. |
| Launch | `bundle.rs` | Bash stub: `source launch.conf`, export `WINEPREFIX`, recipe env, `exec wine game.exe`. |
| Seal | `bundle.rs` | Copy runtime into `Contents/Resources/wine`, strip xattrs, ad-hoc `codesign`. |

Weak points, by impact:

1. **No D3D renderer choice at all.** Every game gets the runtime's builtin
   `wined3d` (D3D1–11 on OpenGL 4.1) except when cnc-ddraw is toggled. There is
   no DXMT, D3DMetal (outside the whole-runtime GPTK choice), DXVK or D9VK, and no
   per-game mapping between the game's API and a renderer. This is the main
   reason "many games don't work" (§5).
2. **The default runtime is the weakest lineage for GOG's catalogue.** Upstream
   new-WoW64 on Rosetta crashes in 32-bit Inno installers (docs/15 §7) — and most
   GOG classics are 32-bit installers. The GPTK runtime is wine 7.7 (2022) with
   CrossOver 22-era patches; it handled one 64-bit D3D11 game and nothing 32-bit in
   §5.
3. **The user picks the runtime and the recipe** from menus they cannot judge
   (mandatory prompts, design 12 §5 amendment). Picking a wine flavour is not a
   decision a player can make well; it should be data.
4. **Depot trees miss GOG's support overlay.** `gog-support/<id>/<id>/app/**` holds
   files Galaxy merges into the install dir (Carmageddon TDR 2000:
   `ASSETS/options.txt`, without which the exe aborts at `File.cpp:73`; Treasure
   Adventure Game: `config.ini`). docs/18 §1 classified `gog-support` as "not needed"
   — wrong for these games (verified, §5).
5. **The prefix lives inside the signed bundle.** Every launch writes registry,
   saves (`drive_c/users/...`) and shader caches into `Contents/`, invalidating the
   ad-hoc seal and making `/Applications` installs depend on write access. Saves
   die with the bundle on rebuild.
6. **Failure is silent.** The launcher `exec`s wine; a game that exits or shows a
   black window leaves no trace a user can find. There is no log file, no
   "this game is known not to work" signal, no way to retry with another renderer
   short of rebuilding.
7. **Missing standard env.** No `WINEMSYNC`/`WINEESYNC`, no `WINEDLLOVERRIDES`
   baseline, no retina/fullscreen policy (`Mac Driver` registry), no
   `MVK_CONFIG_*`/`DXMT_*` knobs — all of which CrossOver/Sikarugir set by default.

## 2. Why Lutris did not pay off (post-mortem)

Re-checked live for the five test titles that have Lutris entries (`gogo recipes
show`, 2026-10-22, **verified**):

| Lutris script | What it actually contains |
|---|---|
| `stronghold-hd-gog` | silent installer flags only |
| `carmageddon-tdr-2000-gog` | silent installer flags + `create_prefix` |
| `treasure-adventure-game-gog` | innoextract + `input_menu` + two host-shell `execute` steps + a `config.ini` merge from `__support/app` |
| `rebel-galaxy-gog` | `winetricks vcrun2012 win7` + installer flags |
| `pharaoh-cd-cnc-ddraw` | CD layout, cnc-ddraw (docs/16 §1) |
| Neo Cab, Snake Core | no Lutris entry at all |

So the earlier attempt was not "done badly" so much as **aimed at the wrong
layer**:

- **Lutris scripts encode installation, not compatibility.** On Linux a GOG script
  can be three lines because the Lutris *runner* supplies the compatibility layer
  by default: a Wine-GE/Proton build, DXVK + VKD3D-Proton auto-installed into
  every prefix, esync/fsync, gamemode. None of that is in the script, and none of
  it exists on macOS (no Vulkan-native GPU driver; DXVK needs MoltenVK and only
  some versions work). The script's knowledge is therefore *incomplete by design*
  once it leaves Linux.
- **Most script steps target installer logistics** that gogo already handles better
  (Inno flags, innoextract, CD paths) and that the depot lane makes moot.
- **The one GOG-specific fix that mattered in TAG's script** (`merge
  __support/app/config.ini`) is exactly the depot `gog-support/.../app/` overlay
  (§1 item 4) — GOG ships it; gogo can apply it without Lutris.
- `wine.version` pins name Linux runner builds; `dxvk`/`esync` toggles assume Linux
  (docs/15 §6). Even a perfect parity executor cannot translate those to macOS.

Verdict: keep the Lutris code as an *optional hint* (`gogo recipes show`), stop
making it a mandatory step in `mac-app`.

## 3. Landscape (2026-10-22)

GitHub state below is from `gh api repos/<repo>` on 2026-10-22 (**verified**:
archived flag, last push, licence, latest release).

### 3.1 Runtimes (the Wine itself)

| Source | State | Lineage / what's in it | Licence | Automation | gogo use |
|---|---|---|---|---|---|
| **CrossOver** (CodeWeavers) | CrossOver 26 shipped 2026-02 on Wine 11.0, with D3DMetal 3.0, DXMT 0.72, VKD3D 1.18 (**reported**, Phoronix/OMG Ubuntu); 26.1/26.3 followed | The reference macOS stack: wine32on64 history, own patches, per-app "CrossTie" recipes, per-bottle renderer setting | Proprietary app; the Wine/DXMT parts are LGPL and CodeWeavers publishes source tarballs (**reported**; their media server returned 403 to our probe) | CLI tools exist inside the app | Not bundleable. Its *source drops* are what the Gcenx/Sikarugir "WineCX" engines are built from. |
| **Gcenx `macOS_Wine_builds`** | Active; tag `11.18` 2026-09-25 (**verified**) | Upstream WineHQ staging/devel/stable, new-WoW64, MoltenVK bundled | LGPL-2.1 | tarball, sha256 digest | gogo's current default. Fails 32-bit Inno installers under Rosetta (docs/15; again on the host in #56). |
| **Gcenx `game-porting-toolkit`** | Last release `3.0-3`, 2026-03-03 (**verified**) | wine **7.7** (GPTK 1.1 base) + D3DMetal 3.0 | Wine LGPL; D3DMetal under Apple's GPTK licence | tarball | gogo catalog `gptk`. Old base; good for 64-bit D3D11/12 only (§5). |
| **Sikarugir `Engines`** (Gcenx) | Active, assets up to `WS12WineSikarugir11.0_1`, `WS12WineCX24.0.7_7` (**verified**) | `WineCX24.0.7` = CrossOver 24.0.7 sources ("wine-9.0 (SikarugirCX 24.0.7)"); `WineSikarugir11.0` = wine 11.0 + Sikarugir patches (DXMT ext-escape) | LGPL-2.1 (engines); wrapper launcher closed | plain tarballs, ~165 MB | **Best runtime candidate** (§5). Engines are not standalone: they need the wrapper's `Frameworks/` dylibs (libinotify, freetype, gnutls, MoltenVK — **verified**: `Library not loaded: @rpath/libinotify.0.dylib` without them). |
| **Heroic `wine-crossover`** | Mirror repo, one asset `Wine-Crossover-23.7.1-1` (2026-04-17, **verified**) | CrossOver 23.7.1 sources | LGPL | tarball | Heroic keeps the CX lineage alive for its users — same conclusion as ours. |
| **Gcenx `wine-crossover`** | 404 (**verified**, as docs/15) | — | — | — | Tombstone stays. |
| **Whisky** | Archived 2025-05-11 (**verified**); README: no longer maintained | CrossOver 22.1.1 + GPTK libs ("WhiskyWine") | GPL-3.0 app | GUI | Precedent only. It stopped because one maintainer could not keep a Wine fork + GPTK current; the lesson is to *ride* a maintained engine, not fork one. |
| **Mythic** (+ `MythicApp/Engine`, `wine`) | App active (2026-09); Engine repo last push 2025-12 | Custom GPTK-based engine, 64-bit only | GPL-3.0 app | GUI | Reference only. |

### 3.2 Renderer layers (per Direct3D API)

| Layer | Covers | State | Licence | Notes |
|---|---|---|---|---|
| **wined3d** (in every Wine) | DDraw/D3D1–11 on OpenGL | Builtin | LGPL | Fine for DDraw/D3D≤7 and simple D3D8/9; D3D11 fails on macOS GL 4.1 (Neo Cab, §5). |
| **cnc-ddraw** | 2D DirectDraw | v7.1.0.0 (**verified**) | MIT | Already in gogo; Sikarugir ships it **on by default** (Info.plist `CNC_DDRAW=1`, verified in Template 1.0.24). |
| **DXMT** (3Shain, now © CodeWeavers) | D3D10/11 → Metal | v0.80 2026-04; Sikarugir carries `v0.80-244` (**verified**) | **LGPL-2.1** (**verified**) | i386 + x86_64 PE builds + `winemetal.so`. Needs a Wine whose `winemac.drv` exports the DXMT hook — CrossOver 24+ sources or patched Wine (DXMT wiki, **verified**). Heroic overlays it onto Gcenx staging. |
| **D3DMetal** (Apple GPTK) | D3D11/12 → Metal, **64-bit only** | 3.0 in CrossOver 26, 4.0 in Sikarugir Template (**verified** files) | Apple GPTK licence: **distribution permitted for non-commercial purposes**, Framework/redist may be distributed separately (GPTK 4.0 License, §2A(iii)/2C, **verified** from the copy Sikarugir publishes) | Supersedes docs/13 §3's "never": a free tool may fetch it, with the notice. Still x86_64-PE-only. |
| **DXVK / D9VK on MoltenVK** | D3D8/9 (and old 10/11) → Vulkan → Metal | Sikarugir ships `DXVK-Sikarugir-v3.1.1-async` (d3d8/9 "D9VK"), on by default; `Gcenx/DXVK-macOS` v1.10.3 (2023, stale) for d3d10/11 | zlib | DXVK ≥2 needs Vulkan features MoltenVK lacks for d3d11; the d3d8/9 path works on macOS 26+ per Sikarugir README (**reported**). |
| **KosmicKrisp** (Mesa Vulkan-on-Metal) | Vulkan ICD alternative to MoltenVK | Shipped as an ICD json in Sikarugir Template 1.0.24 (**verified** file) | MIT | Watch item. |
| **dgVoodoo2** | Glide/DDraw/D3D1–9 → D3D11 | closed-source freeware; redistribution by author permission; 2.80+ regressions under Wine (**reported**, Vogons) | proprietary | Would itself need a working D3D11 underneath (DXMT) — a two-hop stack. Not recommended. |

The pattern is consistent across Sikarugir, CrossOver and Heroic (**verified** in
Sikarugir README/Template, Heroic `src/backend/tools/{index,dxmt}.ts`): *pick the
renderer by Direct3D version, default it automatically, expose a toggle.* Heroic
even refuses to install DXVK into a GPTK prefix and treats DXMT as "wine-staging
copy + DXMT DLLs".

### 3.3 Per-game fix data

| Source | Keyed by | Format | Coverage of this user's 84-game library | Usable? |
|---|---|---|---|---|
| **umu-protonfixes** `gamefixes-gog/` | `umu-<GOG id>` filename, or GOG id → `umu-<steam appid>` via `umu-database.csv` (206 GOG rows) | Python, but **~90 % of calls are six declarative primitives**: `protontricks` verb (335), `winedll_override` (72), `set_environment` (58), `regedit_add` (39), `replace_command` (32), `append_argument` (32) — counted over the whole tree, **verified** | 7 of 84 mapped; 5 have an actual fix (Stronghold HD/Crusader HD → `directplay`; Rebel Galaxy Outlaw; Witcher 3 → `hidewineexports`; Gwent) (**verified**) | Yes, as **seed data** translated into a declarative gogo format. Linux/Proton assumptions (DXVK everywhere) must be filtered. BSD-2-Clause. |
| **Lutris installers** | `gogid` field + name search | JSON script | 5 of 7 test titles have one, all thin (§2) | Hint only. |
| **GOG depot `gog-support`** | GOG product id (it *is* the depot) | `app/**` overlay files or a `galaxy_*.exe` support installer | 3 of 6 test titles carry one (all three pre-2012: Pharaoh, Carmageddon, TAG) | **Yes — apply always** (§5). |
| **`goggame-<id>.info`** | GOG id | JSON `playTasks` (exe, args, working dir, `-f`/`-s` variants) | every GOG game | Already used for the entry point. |
| **PCGamingWiki** | page has `gogcom id` in its infobox | wikitext; Cargo queries still `permissiondenied` (**verified**), but `action=parse&prop=wikitext` works and returns `gogcom id`, `direct3d versions`, exe bitness (**verified** on Rebel Galaxy) | most | Metadata (API, bitness) as a *cross-check*; fixes stay prose. |
| **ProtonDB** | Steam appid | ODbL monthly report dumps (`bdefore/protondb-data`) | via umu map only | Linux reports; marginal for macOS. |
| **CrossOver CrossTies / Porting Kit** | product name | proprietary | — | Not importable (docs/14). |
| **winetricks** | verb name | shell | — | The *executor* for verbs named above; already vendored. |

## 4. Test method

- **Games** (from the signed-in library; depot downloads via `gogo depot <id> --dir
  <scratch>`, 3.6 GB total, plus two offline installers already on disk):

  | Game | GOG id | Year | Exe | API (from imports) |
  |---|---|---|---|---|
  | Pharaoh + Cleopatra | 1207659046 | 1999 | PE32 | DirectDraw 2D |
  | Stronghold HD | 1207658712 | 2001 | PE32 | DirectDraw 2D |
  | Carmageddon TDR 2000 | 1146738698 | 2000 | PE32 | DirectDraw + Direct3D 6/7 |
  | Treasure Adventure Game | 1207659013 | 2011 | PE32 | DirectDraw/D3D8-era (GameMaker) |
  | Rebel Galaxy | 1435582019 | 2015 | PE32 | D3D11 + `D3DCOMPILER_43` + `d3dx11_43` (OGRE) |
  | Neo Cab | 1669531175 | 2019 | PE32+ | D3D11/12 (Unity 2022.3) |
  | Snake Core | 1276268370 | 2023 | PE32+ | Java + OpenGL (libGDX, bundled JRE) |

- **Runtimes**: Gcenx `wine-staging 11.18` (gogo default), Gcenx GPTK `3.0-3`
  (wine 7.7 + D3DMetal), Sikarugir engine `WineCX24.0.7_7` (CrossOver 24.0.7
  sources, wine 9.0) with the Sikarugir Template 1.0.24 `Frameworks/`, and
  `WineSikarugir11.0_1` (fails `wineboot` outside its launcher — dropped). Overlays: DXMT v0.80 (3Shain release, onto staging —
  Heroic's recipe; built, but host testing stopped before it ran), and the Sikarugir Template's renderer kit
  (`renderer/{d9vk,dxmt,cnc_ddraw,d3dmetal}`) copied into the engine's
  `lib/wine/<arch>` dirs the way Sikarugir does.
- **Host harness** (2026-10-22): run the game exe from its tree under a fresh per-runtime prefix for
  30 s with `WINEDEBUG=err+all`, then list the Wine process's windows via
  `CGWindowListCopyWindowInfo` (titles and sizes; no screen-recording permission is
  available in this session, so no screenshots), read Wine's stderr and the
  game's own logs (Unity `Player.log`, OGRE `ogre.log`, TDR `dbg.txt`), and kill
  the wineserver. **"Starts"** below means: process alive at 30 s, a game-titled
  window at game resolution, and the engine log shows the renderer initialised
  with no fatal error. It does *not* mean a human played it — input, audio and
  in-game rendering correctness were not judged.

## 5. Results (live, 2026-10-22/23)

Two test beds:

- **Host**: the developer Mac (M4 Pro, macOS 27.0.1), `bin/try.sh`-style harness
  from the depot tree (§4). Only bed with Metal 3, so the only one where DXMT and
  D3DMetal can be judged. Host runs were stopped once VM testing existed.
- **VM**: real `gogo mac-app --from-depot` bundles run with `scripts/vm-test.sh`
  (docs/vm-testing.md) in a clean macOS 27.0 guest, quarantined like a download,
  screenshot read by eye. Guest GPU is "Apple Paravirtual device" (no Metal 3, no
  usable OpenGL pixel format for Wine), so the VM answers *does it start, is it
  self-contained, what does Gatekeeper do* — not *does the 3D path render*.

Bundles were built with `--recipe none`. CX24 bundles used the engine as
`Contents/Frameworks/wswine.bundle` + Sikarugir Template 1.0.24 `Frameworks/`,
built with `WINEDLLOVERRIDES=mscoree=` (CX24's `wineboot` loops on a failing
wine-mono MSI install, err 1603, under gogo's build-time prefix staging) and
`DYLD_FALLBACK_LIBRARY_PATH` pointing at that `Frameworks/`. Bundles whose
build hit #53 were re-signed by hand after replacing the builder-home symlinks
(#55) so the VM tested the runtime, not the known seal bug.

### 5.1 Matrix

"Renders" = screenshot shows the game's own UI. "Starts" = alive with a game
window, logs show the renderer initialised, no screenshot (host). Blank = not run.

| Game (API, bits) | staging 11.18 | GPTK 3.0-3 (D3DMetal) | CX24 | CX24 + DXMT/D9VK/cnc kit | CX24 + D3DMetal 4.0 |
|---|---|---|---|---|---|
| **Treasure Adventure Game** (DDraw/D3D8-era, 32) | host: starts · **VM: renders** title menu (FreeType missing, game draws its own text) | host: crash (`Exception frame is not in stack limits`) | host: starts · **VM: renders** (correct palette, no FreeType errors) | host: **dies** — D9VK: "Vulkan 1.3 capable setup is required", no adapters on MoltenVK | |
| **Carmageddon TDR 2000** (DDraw + D3D7, 32), raw depot | host: "Application Exception" — `Abort at File.cpp:73` (missing `ASSETS/options.txt`) | host: no output | | | |
| Carmageddon + `gog-support` overlay | | | host: first-run "Hardware Capabilities" message box · **VM: same** (game's normal first-run flow; not clicked through) | host: same | |
| **Pharaoh** (DDraw 2D, 32) depot + cnc-ddraw | (#54: black + 11×58 px dialog) | | **VM: renders** dialog text: *"Please insert your Cleopatra CD."* — the depot install-config gap (docs/18 §5), now legible | | |
| **Rebel Galaxy** (D3D11 + `D3DCOMPILER_43`, 32) | host: exits — OGRE "Named constants have not been initialised" | host: crash (stack limits) | host: same shader error | host: **starts** (1600×928 window) but vkd3d-shader rejects HLSL (`E5017: Uniform initializer`) ×480 | host: exits (D3DMetal is 64-bit only) |
| **Neo Cab** (Unity D3D11, 64) | host: `d3d11: failed to create device` · VM: same (`failed to create factory`) | host: **starts** (D3D11 FL 11.1, "AMD Compatibility Mode") | host: d3d11 device fail | host: **starts** (DXMT, FL 11.1, "Apple M4 Pro") · VM: device FL 11.0 on Paravirtual, then "Failed to get metal layer", no window | host: **starts** (FL 11.1) |
| **Snake Core** (Java/libGDX OpenGL, 64) | host: crash — "OpenGL 2.0 or higher with the FBO extension is required. OpenGL version: 2.1 Metal" · VM: no GL context | host: dyld failure (bad test, ignore) | host: **starts** ("Snake Core - v1.6.1" fullscreen) · VM: no GL context (VM limit) | | |

Other observations, all **verified**:

- **Gatekeeper in the VM** with a *valid* ad-hoc signature + quarantine: "*Apple
  could not verify … is free of malware*" with *Move to Trash / Done* (the
  bypass is in System Settings). With the #53 invalid seal it is "damaged" with
  no bypass. Ad-hoc bundles are always `spctl: rejected`.
- **Rosetta notice**: every bundle (staging and CX24) posted "*App Update
  Required — … includes a component that will not open in macOS 28*" in the
  guest. This is §7 in practice.
- **#53 reproduced on the host build**: `codesign failed … a sealed resource is
  missing or invalid` on 4 of 9 builds (both runtimes), i.e. the race between
  Wine's lazy registry flush and the seal is runtime-independent.
- **Bundle sizes** 1.1–1.9 GiB each; the engine accounts for ~0.5 GiB (CX24) /
  ~0.85 GiB (staging) and CX24's `Frameworks/` another 0.23 GiB — per bundle.
- The CX24 engine executes 32-bit Inno code: the Pharaoh `galaxy_*.exe` support
  installer starts and declines by its own script logic (as under staging,
  docs/18 §5), with no stack overflow.

### 5.2 What the matrix says

1. **The renderer decides D3D10/11 games**: Neo Cab fails on every wined3d
   runtime and starts on both Metal translators. Rebel Galaxy needs DXMT *and*
   a native `d3dcompiler_43` — two independent, data-driven fixes.
2. **The CX lineage is the better default engine**: with the right renderer per
   title it is the only engine on which every title reached its own UI or
   first-run dialog, its OpenGL is usable (Snake Core), its
   bundles found FreeType on a clean Mac (no #54 messages), and it showed the
   real text of Pharaoh's dialog.
3. **GPTK 3.0-3 is only useful for 64-bit D3D11/12** and lost to DXMT on the
   same game in startup terms; its wine 7.7 base crashed both 32-bit titles.
4. **"Everything on" is harmful**: D9VK on MoltenVK killed a game wined3d runs.
5. **GOG's own data fixes what Lutris could not**: the `gog-support` overlay
   turned Carmageddon from a crash into its normal first-run flow.

## 6. Recommendation

One strategy, five layers. Each layer is data the next one can override; nothing
asks the player to choose a Wine flavour.

```
L4  user override      ~/Library/Application Support/gogo/compat/<gogid>.toml, --renderer/--runtime flags
L3  gogo compat DB     compat/<gogid>.toml, curated, GOG-id keyed (seeded from umu-protonfixes + our VM runs)
L2  GOG-native fixes   depot gog-support/<id>/app overlay, support installer, goggame playTasks, GOG.com registry key
L1  auto renderer      chosen from the game's own PE imports + bitness
L0  runtime            one curated, pinned engine track, referenced (not frozen) by the bundle
```

### 6.1 L0 — Runtime: one track, referenced by id

- **Default today (macOS ≤ 27): the CrossOver-source lineage** — Gcenx's
  `WineCX24.0.7` engine (Sikarugir `Engines`), laid out as
  `Contents/Frameworks/wswine.bundle` with the engine's `Frameworks/` dylib set
  beside it (that is what its `@rpath` expects; **verified**: the
  `SharedSupport/wine` layout fails with `Library not loaded: libinotify`). Why:
  runs 32- and 64-bit game code (every test title reaches its own UI or first-run
  dialog with the L1 renderer, §5), its OpenGL works where staging's does not (Snake Core), it takes DXMT and
  D3DMetal drop-ins, and it carries its own FreeType (#54). Keep Gcenx
  `wine-staging` as the alternate track, not the default.
- **Watch items**: a CX 25/26-source engine from Gcenx/Sikarugir (newer Wine 11
  base), Heroic's `Wine-Crossover` mirror as a second source, and the
  `WineSikarugir11` engine (fails `wineboot` outside its own launcher on macOS
  27.0.1 — **verified**; needs investigation before use).
- **Post-Rosetta (§7)**: an ARM64 Wine + FEX runtime from whoever ships it signed
  with the restricted entitlements (CrossOver 27 for its own users; an open
  build once Shanks' MRs land and FEX gains Darwin support). This is a new
  catalog entry, not a redesign — *if* the bundle references runtimes instead of
  embedding them.
- **Distribution**: unchanged store (design 12 §4), plus a per-runtime
  `Frameworks/` set and a `renderers/` set (DXMT, cnc-ddraw, D3DMetal on consent)
  versioned alongside. Pin by sha256 (GitHub asset digests), probe after
  install, keep N-1 for rollback.
- **Bundles reference the runtime**: `Contents/Resources/gogo-launch.toml` names
  `runtime = "winecx-24.0.7_7"` (+ renderer set). The launcher resolves it in
  the store at launch. `gogo mac-app --retarget <bundle> --runtime <id>`
  rewrites one field — no rebuild, no game re-copy, seal stays valid.
  `--portable` keeps today's behaviour (engine copied into the bundle) for
  moving a bundle to another Mac, accepting that it cannot be retargeted in place.

### 6.2 L1 — Renderer from the game's imports

`gogo` already parses `goggame-*.info`; add a PE import scan of the primary exe and
every DLL next to it (Rebel Galaxy's D3D11 dependency is in
`RenderSystem_Direct3D11.dll`, not the exe — **verified**). Table:

| Imports / bitness | Renderer | Evidence (§5) |
|---|---|---|
| `ddraw` only, no `d3d*` device use, 32-bit | cnc-ddraw in the game dir + `ddraw=n,b` | Pharaoh (docs/16, live), Stronghold class |
| `ddraw` + Direct3D ≤ 7, `d3d8`, `d3d9` | wined3d (builtin) | TAG renders on wined3d; D9VK on MoltenVK **crashed** TAG (needs Vulkan 1.3) |
| `d3d10*`/`d3d11`/`dxgi`, 32-bit | DXMT | Rebel Galaxy window up on DXMT; D3DMetal exits (64-bit only) |
| `d3d11`/`dxgi`, 64-bit | DXMT default, D3DMetal on consent | Neo Cab FL 11.1 on both; plain wined3d fails |
| `d3d12` | D3DMetal (consent) | not tested |
| `opengl32` only | wined3d-free; CX lineage | Snake Core runs on CX24, not staging |
| `d3dcompiler_4x`, `d3dx9_*`, `d3dx10_*`, `d3dx11_*`, `xinput*`, `xact*` | add the matching winetricks verb (native MS dll) | Rebel's shaders fail in vkd3d-shader (`E5017 … Uniform initializer`); `d3dcompiler_47/43` are also the top two protonfixes verbs |

Global "turn every layer on" (Sikarugir's defaults) is wrong for gogo: it broke
TAG. Per-game selection is cheap because the imports are on disk at build time.

### 6.3 L2 — GOG-native fixes (always on, both lanes)

- Merge `gog-support/<id>/<id>/app/**` into the install dir in the depot lane
  (Carmageddon: abort → first-run hardware dialog; TAG: `config.ini`) — this
  reverses docs/18 §1's "not needed".
- Run the support installer when the depot carries one (Pharaoh; best-effort, as
  design 15 D3 already does).
- Honour every `playTasks` entry (args such as TDR's `-f` / `-s`) as launch
  variants, not just the primary.
- The **depot lane becomes the default** whenever a depot build exists: it needs
  no 32-bit Inno run, which is exactly what fails on the host today (#56).

### 6.4 L3 — gogo compat DB (GOG-id keyed, declarative)

A directory of small TOML files in this repo (or a sibling data repo), one per
GOG product id, fetched pinned to a commit and cached:

```toml
# compat/1435582019.toml — Rebel Galaxy
title    = "Rebel Galaxy"
exe      = "RebelGalaxyGOG.exe"        # skip GoGLauncher.exe
renderer = "dxmt"
verbs    = ["d3dcompiler_43", "d3dx11_43"]
status   = "boots"                      # unknown | broken | boots | plays
tested   = [{ runtime = "winecx-24.0.7_7", host = "m4pro/macos-27.0.1", date = 2026-10-22 }]
source   = "gogo-vm"                    # or "umu-protonfixes:<file>@<sha>"
```

- Fields are the six primitives umu-protonfixes actually uses (verbs, dll
  overrides, env, registry, exe replace, args) plus `renderer`, `runtime`,
  `exe`, `status`. No code, no shell.
- **Seed**: translate umu-protonfixes' GOG-mapped fixes mechanically (BSD-2,
  attribute), dropping Proton-only knobs (DXVK/fsync/`protonprefix`).
  Most entries will come from **our own VM smoke runs** — `scripts/vm-test.sh`
  writes the evidence; a maintainer turns it into a TOML line.
- Lutris: keep `gogo recipes show` as a human hint; drop the mandatory picker.

### 6.5 L4 — User override, and what lives where

| Where | What | Why |
|---|---|---|
| **Baked into `.app`** (signed, never written after signing) | launcher, `gogo-launch.toml` (runtime id, renderer, verbs, env, exe, args), icon, `Info.plist` | small, immutable, seal stays valid (#53) |
| **gogo store** (`~/Library/Application Support/gogo/runtimes/`) | engines, `Frameworks/`, renderer sets, winetricks cache | shared, swappable, survives macOS 28 changes (§7) |
| **Per-game state** (`~/Library/Application Support/gogo/games/<gogid>/`) | Wine prefix (created on first launch by the user's own wine: correct username, no builder-home symlinks, #55), game files (APFS clone from the depot cache — no extra space), saves | writes never touch the signed bundle; saves survive rebuilds |
| **User override** (`…/gogo/compat/<gogid>.toml`) | same schema as L3 | read at every launch: switching renderer is an edit, not a rebuild |

First launch: create prefix → apply L2/L3/L4 registry + verbs → launch. Later
launches: compare a hash of the effective config with the prefix's stamp,
re-apply only on change. `--portable` builds today's fat bundle for copying to
another Mac.

### 6.6 Failure surfacing

- Launcher tees Wine stderr to `~/Library/Logs/gogo/<gogid>/<ts>.log`
  (currently lost).
- If the game exits within ~15 s, or no window appears, the launcher shows a
  native dialog (`osascript display dialog`) with the last error lines and three
  buttons: *Open log*, *Try next renderer* (walks the L1 table's alternatives and
  writes the choice to the L4 file), *Copy report*.
- At build time, print the L3 `status` ("known to boot on …", "untested",
  "known broken: <why>") before the size confirmation.
- `gogo mac-app doctor <bundle>`: runtime present and probed, renderer files
  present, signature valid, prefix stamp current.

### 6.7 Maintenance cost (estimate)

| Item | Cadence | Effort |
|---|---|---|
| Runtime track (watch Sikarugir `Engines`/Gcenx, bump pin, VM smoke) | per upstream release (~monthly) | ~1 h + automated VM runs |
| Renderer set (DXMT, cnc-ddraw, D3DMetal) | quarterly | ~1 h |
| Compat DB | per game the user cares about | minutes per entry, VM run attached |
| umu-protonfixes import | quarterly script run | low |
| macOS 28 transition (ARM64 runtime track) | once, 2027 | real work — a new catalog entry + launcher entitlement story; cheap only if §6.1 references are in place |

## 7. First-class risk: Rosetta 2 ends as a general tool in macOS 28

### 7.1 What is known

| Fact | Status | Source |
|---|---|---|
| "Rosetta is available for any Mac with Apple silicon using macOS 27 or earlier." From macOS 28, Rosetta "will be available only for certain older, unmaintained games that rely on Intel-based frameworks." | **verified** (Apple support page, read 2026-10-23) | [support.apple.com/102527](https://support.apple.com/en-us/102527) |
| How Apple picks the "games" subset (bundle allow-list? framework detection? Wine?) | **unknown** — Apple has not said | same |
| macOS 26.4+ warns at launch for Rosetta-dependent apps | **reported** | [AppleInsider 2026-02-16](https://appleinsider.com/articles/26/02/16/macos-tahoe-264-warns-if-your-apps-wont-work-when-rosetta-2-dies) |
| A gogo-built x86_64 Wine bundle in the macOS 27.0 test VM shows "App Update Required … will not open in macOS 28" | **verified** (tart-vm-testing session, 2026-10-22; docs/vm-testing.md) | this repo |
| CodeWeavers shipped a CrossOver **Preview** with native **ARM64 Wine + a macOS port of FEX** (x86-64 emulation in-process via ARM64EC); no 32-bit bottles; many launchers broken; D3D12 "coming soon"; target CrossOver 27 "early next year" | **reported** (blog 2026-07-31 is behind a bot challenge we did not pass; summarised by search results and the MacRumors thread) | [CodeWeavers blog](https://www.codeweavers.com/blog/mjohnson/2026/7/31/crossover-preview-the-right-to-bear-arm64-on-mac), [MacRumors](https://forums.macrumors.com/threads/crossover-announces-arm64-support.2486233/) |
| Brendan Shanks (CodeWeavers) on wine-devel, 2026-08-07: all pieces are now in macOS to run ARM64 Wine natively — macOS 26.5 "x18 custom ABI" mode and a 4K-page `posix_spawn` attribute, Xcode/macOS 26.4 PAGEZERO changes for the below-4 GB space x86 emulation needs; **these need restricted entitlements, i.e. an Apple developer account**; MRs to go upstream | **verified** (list archive read) | [wine-devel thread](https://list.winehq.org/hyperkitty/list/wine-devel@list.winehq.org/thread/CKG5CEN2BE5VRXZ7O7NX4YUSBH3247WH/) |
| Upstream FEX has no Darwin support; CodeWeavers' Wine side will publish with CrossOver 27 sources | **reported** (third-party tracker) | [highball#6](https://github.com/gauthierpiarrette/highball/issues/6) |
| Community ARM64 Wine 11.10 + FEX ARM64EC for macOS: x86-64 apps work, **32-bit x86 does not yet** | **verified** (README, LGPL, 0 stars, experimental) | [Irz-Z/wine-macos-native](https://github.com/Irz-Z/wine-macos-native) |
| D3DMetal as distributed today (GPTK 4.0 in Sikarugir Template 1.0.24) is **x86_64-only** Mach-O; DXMT's `winemetal.so` release build is x86_64 too; MoltenVK is universal | **verified** (`file` on the binaries) | §3.2 |
| DXMT has ARM64EC/native-arm64 build work merged | **reported** (search results; PR #75/#76 "native macOS/arm64" closed 2025-07) | [3Shain/dxmt](https://github.com/3Shain/dxmt) |

### 7.2 How each candidate is expected to hold up on macOS 28

| Candidate | macOS 28 outlook | Confidence |
|---|---|---|
| Gcenx `macOS_Wine_builds` (x86_64) | Stops launching unless Apple's game exception covers Wine bundles | Apple's statement is firm; the exception's scope is unknown |
| Gcenx GPTK 3.0 / D3DMetal | Same, *and* D3DMetal itself is x86_64-only | firm for the binary arch; Apple may ship an arm64 D3DMetal (no statement found) |
| Sikarugir CX/Sikarugir engines | Same (x86_64); Sikarugir would need an ARM64 engine | guess — no Sikarugir statement found |
| CrossOver (commercial) | ARM64 build planned for CrossOver 27 (early 2027), 64-bit only | reported, vendor roadmap |
| Whisky | Dead already | firm |
| Heroic on macOS | Uses the same x86_64 Wine builds; inherits the problem | firm for today's builds |
| Upstream Wine ARM64 + FEX (Hangover-style) | The only open path; needs Shanks' MRs upstream, a Darwin FEX, and **restricted entitlements** | reported; entitlement need is verified |
| Native ports (ScummVM, DOSBox — docs/16 S2) | Unaffected — arm64-native | firm |

Two consequences matter more than the dates:

- **32-bit Windows games are the most exposed.** Both ARM64 paths drop 32-bit
  for now, and most GOG classics are 32-bit (5 of 7 test titles). Unless Apple's
  game exception covers x86_64 Wine (it is exactly "older, unmaintained games
  that rely on Intel-based frameworks"), these titles may only keep running via
  native lanes (DOSBox/ScummVM) or a later 32-bit ARM64EC/WoW64 story.
- **Entitlements.** An ARM64 Wine that emulates x86 needs restricted
  entitlements that an ad-hoc-signed, locally built bundle cannot carry. gogo
  cannot build that runtime itself and seal it into every `.app`; it will have
  to **use a runtime signed by whoever holds the entitlement** (CodeWeavers,
  a Gcenx/Sikarugir release, or Apple) — kept outside the bundle as that
  vendor shipped it.

### 7.3 Design consequence

A runtime **frozen into each `.app` at build time cannot be fixed** after macOS 28
ships: the user would have to rebuild every bundle, and some fixes (an
entitlement-signed runtime) cannot be copied into a re-signed bundle at all.
So the recommendation (§6) keeps the **runtime outside the bundle by
reference**, switchable per bundle without a rebuild, and treats "self-contained"
as *self-contained on this Mac* rather than *copy the engine into every app*.

## 8. Open questions, known gaps, and what this means for #53–#56

### 8.1 Bundle bugs filed under epic #50

| Issue | What §5/§6 found | Effect on the design |
|---|---|---|
| #53 signature invalid after build | Reproduced on 4/9 builds, both runtimes; a valid ad-hoc seal still gets "could not verify … Move to Trash / Done" under quarantine | Never write into the bundle after signing: prefix out of the bundle (§6.5). Ad-hoc + quarantine will always need the System Settings bypass; only Developer ID + notarisation removes it. |
| #54 FreeType missing on a clean Mac | Staging bundle: FreeType errors in the guest. CX24 bundle with the engine at `Contents/Frameworks/wswine.bundle` + Template `Frameworks/`: **no FreeType errors**, and Pharaoh's "blank" dialog shows its text ("Please insert your Cleopatra CD.") | Runtime self-containment is a property of the runtime *package* (engine + its dylib closure), so the store entry must carry its `Frameworks/`. Pharaoh's stall is the depot install-config gap, not only fonts. |
| #55 builder-home symlinks | Same on CX24 builds | Create the prefix on first launch as the real user (§6.5). |
| #56 installer-lane stack overflow | CX24 runs 32-bit Inno code without the overflow (Pharaoh support installer runs and declines by its own logic; Stronghold HD's offline installer exits 1, no overflow, cause not investigated) | Depot lane as default (§6.3); offline-installer lane on the CX track needs its own investigation. |

### 8.2 Open questions

1. **Apple's game exception** (§7.1): does it cover Wine-based bundles? Re-test
   every macOS 28 beta in the VM; the Rosetta notice is already the canary.
2. **Entitlement-signed ARM64 runtime**: who will ship an open ARM64 Wine + FEX
   build signed with the needed entitlements, and can a gogo bundle launch it
   by reference (`exec` of a separately signed binary keeps its own
   entitlements)? Needs a test once any such build exists.
3. **32-bit games after Rosetta**: no ARM64 path covers them yet. Track
   CodeWeavers/FEX WoW64 work; DOSBox/ScummVM native lanes (docs/16 S2) cover
   part of the classic catalogue.
4. **Newer CX-source engine**: CX 24.0.7 is Wine 9.0. The `WineSikarugir11.0`
   engine failed `wineboot` outside its launcher (child process exits after
   apiset load) — find out what its launcher sets.
5. **CX24 build-time quirks**: `wineboot` loops on wine-mono (MSI 1603) during
   gogo's prefix staging; we used `WINEDLLOVERRIDES=mscoree=`. Moot once the
   prefix is created at first launch, but the root cause is unknown.
6. **D9VK on macOS**: needs Vulkan 1.3; MoltenVK in the Template does not
   provide it. Sikarugir also ships a KosmicKrisp (Mesa) ICD, which may change
   this on macOS 26+ — untested.
7. **Rendering correctness and input/audio** were not judged; the VM cannot run
   Metal 3 paths. Rebel Galaxy with native `d3dcompiler_43` and every DXMT /
   D3DMetal case needs a short real-hardware pass.
8. Carmageddon past its first-run hardware dialog (needs a click; the VM
   harness can drive it with `osascript`).

## 9. Sources

- Apple, *Using Intel-based apps on a Mac with Apple silicon*: <https://support.apple.com/en-us/102527>
- AppleInsider, macOS 26.4 Rosetta warning: <https://appleinsider.com/articles/26/02/16/macos-tahoe-264-warns-if-your-apps-wont-work-when-rosetta-2-dies>
- CodeWeavers, CrossOver ARM64 preview (2026-07-31): <https://www.codeweavers.com/blog/mjohnson/2026/7/31/crossover-preview-the-right-to-bear-arm64-on-mac>; MacRumors thread: <https://forums.macrumors.com/threads/crossover-announces-arm64-support.2486233/>
- wine-devel, "Wine on ARM64 macOS" (B. Shanks, 2026-08-07): <https://list.winehq.org/hyperkitty/list/wine-devel@list.winehq.org/thread/CKG5CEN2BE5VRXZ7O7NX4YUSBH3247WH/>
- Phoronix, CrossOver 26: <https://phoronix.com/news/CrossOver-26>
- Gcenx/macOS_Wine_builds: <https://github.com/Gcenx/macOS_Wine_builds>; Gcenx/game-porting-toolkit: <https://github.com/Gcenx/game-porting-toolkit>
- Sikarugir: <https://github.com/Sikarugir-App/Sikarugir>, Engines: <https://github.com/Sikarugir-App/Engines>, Template: <https://github.com/Sikarugir-App/Template>
- Heroic: <https://github.com/Heroic-Games-Launcher/HeroicGamesLauncher> (`src/backend/tools/{index,dxmt}.ts`, `src/backend/wine/manager/downloader/constants.ts`), <https://github.com/Heroic-Games-Launcher/wine-crossover>
- Whisky (archived): <https://github.com/Whisky-App/Whisky>; Mythic: <https://github.com/MythicApp/Mythic>
- DXMT: <https://github.com/3Shain/dxmt> (wiki: installation guide); DXVK-macOS: <https://github.com/Gcenx/DXVK-macOS>; MoltenVK: <https://github.com/KhronosGroup/MoltenVK>; cnc-ddraw: <https://github.com/FunkyFr3sh/cnc-ddraw>
- umu-protonfixes: <https://github.com/Open-Wine-Components/umu-protonfixes>; umu-database: <https://github.com/Open-Wine-Components/umu-database>
- Lutris installers API: <https://lutris.net/api/installers/>; PCGamingWiki API: <https://www.pcgamingwiki.com/w/api.php>; ProtonDB dumps: <https://github.com/bdefore/protondb-data>
- winetricks: <https://github.com/Winetricks/winetricks>
- Irz-Z/wine-macos-native: <https://github.com/Irz-Z/wine-macos-native>; highball#6: <https://github.com/gauthierpiarrette/highball/issues/6>
- dgVoodoo2 (Vogons): <https://www.vogons.org/viewtopic.php?p=1405266>
