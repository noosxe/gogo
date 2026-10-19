# 16 — GOG-specific solidification: recipes, wrappers, and native lanes

**Status: research (2026-10-19).** Companion to docs/13 (wrapper landscape), docs/14
(recipe research), docs/15 (runtime distribution). Motivation: the pharaoh incident
showed that generic community recipes meet GOG's very predictable conventions
half-way at best — our use case is *GOG offline installers on macOS*, so
everything GOG-specific we can internalize beats borrowing general-purpose
machinery.

## 1. The incident, compressed

`pharaoh-cd-cnc-ddraw` (Lutris, CD edition) under our GOG flow: correct game
(gogid matched), three broken assumptions — (a) the recipe's file paths target
the **CD layout** (`$GAMEDIR/drive_c/SIERRA/Pharaoh`, valid only when the wine
prefix *is* the game dir), not our `drive_c/GOG/<slug>` install; (b) its
enhance-pack `wineexec` is an unrenderable 32-bit GUI under GPTK; (c) when that
step died, wholesale recipe degradation dropped the load-bearing fix —
**cnc-ddraw**. Manual game-dir application of cnc-ddraw (FunkyFr3sh v7.1.0.0,
`ddraw.dll` + `[ddraw]` ini) made the game run (audio engine live, zero
`ChangeDisplaySettings` failures; live-verified 2026-10-19).

## 2. Survey findings (2026-10-19)

| Source | State | GOG-specific value |
|---|---|---|
| **Sikarugir** (Wineskin successor, ex-Kegworks; `github.com/Sikarugir-App/Sikarugir`) | Active, LGPL-2.1, macOS 14.6+ | **CNC-DDRAW ships as a default wrapper toggle**, alongside D3DMetal/DXVK/DXMT/D9VK/WineD3D. The Wineskin lineage treats ddraw wrapping as baseline infrastructure, not per-game scripting. (`sikarugir.com` is unaffiliated malware — never reference.) |
| **winetricks** `cnc-ddraw` verb | Maintained (FunkyFr3sh zips, version-pinned) | Installs into the prefix's system DLL dir + native override. Our vendored-winetricks + shims (design 12 §8) can drive it unchanged. |
| **GOG forums** (e.g. tzar thread) | Per-game `Enable DirectDraw and use cnc-ddraw` guides | GOG-native community practice: game-dir `ddraw.dll` drop-in + ini. Unstructured prose, but the exact shape we live-proved. |
| **robotspacer.tv** (2024-01) | Survey | GOG marks games "**powered by ScummVM / DOSBox**" — the Windows offline installer contains the raw game data; native ScummVM/DOSBox builds run them on macOS with **no wine at all**. |
| **Porting Kit** (portingkit.com) | Active, but closed-source GUI wizard (docs/13 §1) | Per-game port catalog exists; no CLI/API — not importable programmatically. |
| **PCGamingWiki** | Structured per-game fixes, MediaWiki API | Carries GOG ids/pages; candidate *advisory* fix source. Naive fetch 403s (bot guard) — needs proper UA/endpoint work. |
| **Lutris** | No macOS app | Stays our recipe corpus for wine games; `gogid`/`gogslug` fields already consumed and cross-checked. |

## 3. GOG conventions already solidified in gogo

- InnoSetup silent install: `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /DIR=…`
- `goggame-<id>.info` → entry point + playTasks; gogid cross-check post-install
- Deterministic target `C:\GOG\<slug>`; multi-part `.bin` offline installers
- Version-tail naming (`setup_<name>_<dotted-version>.exe` → display name)
- Ladder-tested runtimes; 32-bit installer ⇄ runtime capability findings (docs/15)

## 4. Recommendations (build order)

| # | Item | Priority | Notes |
|---|---|---|---|
| **S1** | **Native cnc-ddraw application** | P0 | The ecosystem default (Sikarugir ships it as a toggle; GOG forums bless the drop-in; winetricks carries the verb). MIT-licensed zip from FunkyFr3sh releases → runtime-store cache (chunk-verified, LGPL/MIT notice in bundle) → game-dir `ddraw.dll` + defaults ini. Interactive offer + `--ddraw-wrapper` / `--no-ddraw-wrapper` flags; visible in `recipes show`. Game-dir placement wins over winetricks' system32: matches GOG-forum practice, needs no dll-override registry edits (DLL search order finds the local dll first — live-proven). |
| **S5** | Rank gog-matching recipes first | P0 | Free: in the picker and `recipes list`, rank installers carrying `gogid`/`gogslug` above the rest. |
| **S2** | DOSBox / ScummVM native lane | P1 | GOG "powered by" titles: wrap the data in a .app around a native dosbox/scummvm binary — no wine, no 32-bit pain, arm64-native builds exist. Separate bundle template; needs its own design doc. |
| **S3** | Curated GOG-era winetricks set | P2 | `quartz`, `devenum`, `directplay`, `d3dx9_43`, … as advisory hints for pre-2005 catalogs; we already have the machinery. |
| **S4** | PCGamingWiki advisory lookups | P2 | Per-gogid fix summaries surfaced in `recipes show` (advisory only, never auto-applied). Blocked on API access polish. |

## 5. Non-goals / rejected

- Importing PortingKit port definitions (closed, GUI-only).
- Generic game-recipe aggregation beyond Lutris (scope: GOG offline installers).
- Bundling GPTK-derived anything into artifacts (D3/D9 unchanged).
