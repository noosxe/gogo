# 13 — macOS App Bundles (Wine-wrapped ports of offline installers)

*Discovery research, gathered from public sources 2026-10-18. Facts are marked
**verified** (checked against a live project page/doc this day) or **reported**
(consistent across community sources, not independently re-verified). This doc
grounds design `docs/design/10-macos-app-bundles.md`.*

Goal: turn a downloaded GOG Windows offline installer into a **self-contained
macOS `.app`** that double-click launches and can be dropped into
`/Applications` — no Porting Kit, no CrossOver, no manual Wine dance.

## 1. Wrapper-technology landscape (macOS, late 2026)

| Technology | State | Scriptable / headless? | Redistribution |
|---|---|---|---|
| **Whisky** | **Unmaintained** — README (verified 2026-10-18): "Whisky is no longer actively maintained. Apps and games may break at any time." SwiftUI Wine wrapper; Apple-Silicon-only, macOS 14+; CrossOver 22.1.1 base + Apple GPTK | GUI only | n/a (dead) |
| **Porting Kit** | Active, free; Wineskin-based. Its "automation" = community-authored port scripts run *inside* the GUI wizard; official GOG flow is click-through (verified: no CLI documented anywhere — site, docs, r/macgaming threads) | **No** — GUI-only, closed source (releases-only repo) | Closed source |
| **Sikarugir** (Wineskin fork, renamed from Kegworks 2025-10) | Active, **LGPL-2.1** (verified). Modernized Wineskin codebase (vitor251093); master-wrapper templates; engines by Gcenx | GUI only (`Creator.app` is a GUI); **no documented headless creation API** | LGPL ✅ (launcher + Creator.app sit outside LGPL, per repo) |
| **CrossOver** (CodeWeavers) | Commercial, closed source; has CLI tools (`~/cxoffice/bin` on Linux, in-bundle on Mac) | Yes | **No** — proprietary |
| **Apple Game Porting Toolkit (GPTK)** | D3DM (Apple's DX11/12→Metal) + DXVK; best-in-class perf; needs Apple Silicon + recent macOS | N/A (it's a Wine variant, not a wrapper) | **Not redistributable** — license is for porting evaluation; tools like Whisky/Mythic make the *user* download it from Apple |
| **Plain Wine** (wine-crossover via Homebrew; Wine 11.x) | Healthy: Wine 11.0 stable (2026-01) ships mature WoW64 — 32-bit Windows code on 64-bit-only macOS; active macOS support | `wine` is just a binary — fully scriptable | **LGPL-2.1** ✅ |
| **Mythic** | Active, open-source macOS launcher built on a custom GPTK implementation; macOS 14+; good architecture precedent (runtime store + per-bottle prefixes) | GUI | Open source — reference, not dependency |

**Conclusion:** nothing existing gives gogo a headless "installer → .app" path.
But the *ingredients* are all scriptable: Wine is LGPL and scriptable, GOG's
Windows installers are Inno Setup with documented silent flags, and the
Wineskin bundle format is just files. gogo should assemble the bundle itself.

## 2. GOG offline installer mechanics

- **Windows installers are Inno Setup packages** (**verified** against
  jrsoftware.org docs 2026-10-18): silent flags
  `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /DIR="<target>"` run the whole
  wizard unattended. This is the exact mechanism Lutris and Heroic use for GOG.
- Multi-part installers: `setup_*.exe` plus `.bin` sibling parts — Inno Setup
  picks siblings up automatically as long as they sit in the same directory.
- Entry point after install: GOG drops `goggame-<id>.info` JSON files (with
  `gameTask`/`primaryTask` entries pointing at the launcher `.lnk`/`.exe`) —
  parseable; gogo already speaks serde JSON.
- Caveat (**reported**): very old GOG titles may ship NSIS-era or custom
  installers; `/VERYSILENT` support must be fail-loud at conversion time, not
  guessed.
- **mac-native titles** ship MojoSetup `.sh` installers containing a prebuilt
  (mostly Intel) `.app`. Different branch — deferred (design 10 §11); ZIP-
  compatibility of MojoSetup archives makes extraction viable later.

## 3. Licensing matrix (what may ship where)

| Component | License | May gogo bundle it? | May gogo fetch it for the user? |
|---|---|---|---|
| Wine (wine-crossover) | LGPL-2.1 | ✅ (with license notice + source pointer in the bundle) | ✅ (it's a public Homebrew artifact) |
| Apple GPTK / D3DM | Apple EULA (porting evaluation) | ❌ **never** | ❌ — user downloads from Apple; gogo only consumes a user-provided mount |
| DXVK | zlib ✅ / MoltenVK Apache-2.0 ✅ | ✅ | ✅ |
| Game content | User's GOG license | ✅ inside the user's local bundle only (never distributed by gogo) | n/a — gogo already downloads it for the user |
| gogo itself | MIT OR Apache-2.0 | — | — |

The Whisky/Mythic precedent settles the GPTK pattern: the user accepts Apple's
terms and downloads the dmg; the tool only consumes it. gogo never automates
that download.

## 4. Precedents and size expectations

- **Wineskin/Sikarugir wrapper layout** (verified structure across repos):
  `Title.app` = launcher stub + Wine engine copy + game prefix. Per-app
  duplication is the model's defining cost; wrappers typically land at
  **hundreds of MB per game** (engine-dominated).
- **Whisky** kept engines in a shared app-container store (smaller disk use,
  not self-contained). **Wineskin** copies the engine into each wrapper
  (self-contained, draggable, big).
- Since the requirement is *self-contained + drag into Applications*, the
  Wineskin model wins; a shared-runtime variant stays a future option
  (design 10 §10).

## 5. Decision

**gogo builds the `.app` itself, headlessly** (design `docs/design/10`):
resolve a Wine runtime (user-supplied GPTK mount / Homebrew wine / explicit
path) → run the installer silently into a fresh WINEPREFIX inside the bundle →
write launcher + `Info.plist` → ad-hoc codesign. Rejected: driving Porting
Kit/Sikarugir (no headless API), depending on CrossOver (proprietary),
bundling/fetching GPTK (Apple license).

## 6. Open questions for implementation

1. Do all *currently sold* GOG Windows installers honor `/VERYSILENT`?
   (spot-check across old titles during implementation — fail-loud otherwise).
2. ~~Exact Wine binary name per runtime~~ **answered (live, 2026-10-18, GPTK 4.0
   beta 3)**: GPTK's "Evaluation environment" dmg ships **no Wine at all** —
   `redist/lib/` holds only D3DMetal.framework + d3d/dxgi `.so` overlays + wine
   PE builtins (69 MB, zero executables). The runnable base is wine-crossover
   (Homebrew `wine` cask); GPTK libs are overlaid onto it. So `brew` is the
   primary mode and `gptk --runtime-path` expects a *wine tree with GPTK libs
   already overlaid*, not the raw Apple dmg. `wine` vs `wine64` name sniffing
   stays (older trees differ).
3. Nested codesigning quirks: GPTK's D3DM frameworks inside a user bundle —
   `codesign --deep --force --sign -` covers it (verify on first GPTK bundle).
4. Does the resulting bundle launch cleanly from `/Applications` given
   LaunchServices path assumptions (prefix contains absolute
   `drive_c/GOG/<name>` paths written at conversion time — bundle must never
   be *rebuilt* after conversion, only moved).
