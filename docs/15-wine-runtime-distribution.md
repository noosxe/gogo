# 15 — Wine runtime distribution on macOS (catalog research)

**Research date:** 2026-10-19 (live probes same day)
**Input to:** design 12 — runtime catalog, store, interactive selection. Amends docs/13
(landscape, licensing posture) and design 10 §3 (runtime resolution).
**Sources:** live HTTP probes (GitHub repos + releases API, raw.githubusercontent.com),
winetricks source inspection, gogo prior dated research (docs/13, docs/14 — 2026-10-18/19),
community history (user-supplied, cross-checked where verifiable).

## 1. History — why the mac Wine landscape is a monoculture

- macOS 10.15 Catalina removed 32-bit host process support → upstream Wine could no
  longer execute 32-bit Windows binaries on mac.
- CodeWeavers engineered **wine32on64**: 32-bit Windows calls thunked onto 64-bit host
  calls, built with a custom-patched LLVM/Clang toolchain (**cx-llvm**). Shipped in
  CrossOver; the Wine parts are LGPL source.
- **Dean Greer (Gcenx)** packaged the LGPL CrossOver sources as **wine-crossover
  (WineCX)** — standalone macOS bundles + Homebrew casks. For years this was *the* free
  Wine on mac; Sikarugir/Wineskin engines, Porting Kit, and (via CrossOver 22.1.1) Whisky
  all build on this lineage.
- **Why discontinued:**
  1. Upstream **WoW64 matured** — 32-bit Windows code is handled entirely inside the
     Windows/PE layer (no host 32-bit support, no thunking hacks).
  2. **cx-llvm bit-rot** — the legacy patched toolchain grew fragile against modern
     Xcode SDKs and newer macOS.
  3. Gcenx pivoted primary maintenance to **official WineHQ mac packages** (§2) and
     Apple GPTK packaging.
- Live confirmation (2026-10-19): `github.com/Gcenx/wine-crossover` and its
  `/releases/latest` API → **HTTP 404**; the `gcenx/wine` tap README lists exactly one
  cask, `game-porting-toolkit` v3.0. The root README's `brew install wine-crossover`
  advice is **stale** — fix pending (design 12 Phase 0).

## 2. Primary catalog entry — Gcenx/macOS_Wine_builds (live-verified 2026-10-19)

Repo: `https://github.com/Gcenx/macOS_Wine_builds` — "Official Winehq macOS Packages".

| Attribute | Verified value |
|---|---|
| Latest release | tag `11.18`, published 2026-09-25, `prerelease: false` |
| Assets | `wine-staging-11.18-osx64.tar.xz`, `wine-devel-11.18-osx64.tar.xz` (devel = 190,974,384 B ≈ 182 MiB, `application/x-xz`) |
| Integrity | GitHub-native asset `digest: sha256:…` — pinning is free, no manual checksum table |
| URL pattern | `…/releases/download/<tag>/wine-<flavor>-<tag>-osx64.tar.xz` |
| Packages | `wine-stable`, `wine@devel`, `wine@staging`; stable confirmed on the same train (`wine-stable-11.0_1-osx64.tar.xz`, tag `11.0_1` — tags may carry `_N` suffixes) |
| Build config | `--build=x86_64-apple-darwin --enable-archs=i386,x86_64` — WoW64; README: "works for 32 & 64-bit windows binaires"; coreaudio/vulkan/gstreamer on, opengl off |
| Host arch | x86_64 — **Rosetta 2 required on Apple Silicon** |
| Post-extract layout | ✅ audited 2026-10-19 (§8.1): archive root is `Wine Staging.app/`; wine tree — gecko + mono bundled — at `Contents/Resources/wine/{bin,lib,share}` |
| License | Wine LGPL-2.1 — redistributable with notice + source pointer (docs/13 §3) |
| Noise | README AV caution: Mingw-built PE binaries trip antivirus heuristics — expected, not malware |
| Brew path | repo documents `brew install --cask wine-stable` (installs `Wine Stable` into /Applications). homebrew-cask core disabled `wine@staging` 2026-09-01 over Gatekeeper; downloading the tar.xz and sealing under our own ad-hoc signature sidesteps cask Gatekeeper entirely |

## 3. winetricks (live-verified 2026-10-19)

- **URL:** `https://raw.githubusercontent.com/Winetricks/winetricks/master/src/winetricks`
  (HTTP 200, official "latest version" link per README). `WINETRICKS_VERSION=20260125-next`.
  License **LGPL-2.1+**. Single self-contained POSIX `sh` script; explicitly supports
  OS X; pinned by **sha256** (its own bug-report convention asks for the hash).
- Semantics: verbs install **into a WINEPREFIX** (`drive_c/windows/{fonts,system32,…}`
  plus registry), driven through the prefix's own wine (`msiexec`, `regsvr32`, `reg`).
  Post-prefix-creation use is the standard model — exactly how the gogo recipe phase
  runs them. Winetricks is needed regardless of Wine packaging: Wine ships *builtin*
  reimplementations; winetricks fetches **native Microsoft components** Wine legally
  cannot ship — and modern Wine ships *fewer* legacy builtins than older releases
  (the d3dx9 builtin family was removed upstream).
- Host-side helpers the script shells out to:

| Helper | Stock macOS | Used by |
|---|---|---|
| `curl` | ✅ `/usr/bin/curl` | all downloads |
| `unzip` | ✅ `/usr/bin/unzip` | many verbs |
| `shasum` | ✅ | checksums |
| `cabextract` | ❌ | **pervasive**: `corefonts` (self-extracting cabinets), `d3dx9` (DirectX Jun2010), `vcrun*`, win7sp1/xp helper files — all via `w_try_cabextract` |
| `7z` / `unrar` / `xz` | ❌ | a minority of verbs |

  → Decision (design 12 D5): gogo ships **Rust reimplementations** of `cabextract` and
  `7z` as PATH-scoped shims; users install nothing.
- Operational: `WINE=<wine> WINEPREFIX=<prefix> winetricks -q <verbs>`; `--force`
  overrides exit-99 broken-package gates; cache `~/.cache/winetricks` (honors
  `XDG_CACHE_HOME`); **disable self-update** in automation; GUI (zenity/kdialog)
  optional with plain-CLI headless fallback.

## 4. GPTK — user-acquired, overlay-only

- Apple-ID-authenticated download at developer.apple.com; no public direct URL;
  evaluation EULA — **not redistributable, no commercial ports**.
- The dmg ships **no Wine executables** (only `D3DMetal.framework`, d3d/dxgi overlays,
  PE builtins) — a "GPTK runtime" is necessarily a wine tree with GPTK libs overlaid
  (docs/13 §6.2, analyzed 2026-10-18).
- Apple Silicon + macOS 14+. The `gcenx/wine` tap ships a `game-porting-toolkit` cask (v3.0-3, sha256-pinned) backed by a **public auth-free GitHub release** — audited stanza and consent-download consequence in §8.3 (design 12 D9).
- gogo posture unchanged: never download or bundle; register/probe a user-supplied
  mount; `README-GPTK.txt` EULA notice in the produced bundle.

## 5. Tombstones & references

| Entry | Status | Note |
|---|---|---|
| wine-crossover (Gcenx) | **gone** (404, 2026-10-19) | tombstone carries the §1 EOL narrative; periodically recheck both endpoints |
| Whisky | dead (archived 2025-05-11, read-only) | GPTK-consumption licensing precedent only |
| Sikarugir (ex-Kegworks, ex-Wineskin) | reference | GUI-only (no headless engine API); engines by Gcenx; Rosetta 2 required on AS; **`sikarugir.com` is an unaffiliated malware site — never reference it** |
| Porting Kit | reference | GUI-only wizard, closed source |
| CrossOver (CodeWeavers) | excluded | proprietary; purchase/trial flow |
| WineHQ upstream | source-only | no mac binaries (casks disabled 2026-09-01); LGPL; Wine 11.x with mature WoW64 |
| Lutris runners | Linux-only | never consume; gogo consumes only installer scripts (design 11) |
| Mythic / MythicApp-Engine | audit candidate | GPL-3.0, active, custom GPTK implementation in a separate repo; artifact surface unverified — nice-to-have, defer |

## 6. Lutris `wine:` fields (input to design 12 §7)

- Record-level `version` is a **human label** ("GOG", "CD + cnc-ddraw") — never a wine
  version.
- Script `wine.version` names a **Lutris Linux runner build** (`wine-ge-*`, `lutris-*`,
  `system`) — no macOS equivalent exists.
- GOG fixtures in-tree: cuphead-gog — no `wine:` section; pharaoh-cd-cnc-ddraw —
  `wine.overrides` only (already consumed → `reg.exe` registry steps).
- Full `wine:` key inventory is documented in Lutris `docs/installers.rst` — **audit
  before parsing** (§8.4).
- Consequence: recipe wine pins are **advisory**; exact-version parity is impossible on
  macOS and must surface as a notice, never a blocker.

## 7. Deprecation timeline (dated facts only)

| Date | Event |
|---|---|
| 2025-05-11 | Whisky repo archived (read-only) |
| 2025-10 | Kegworks → Sikarugir rename |
| 2026-01 | Wine 11.0 stable (mature WoW64) |
| 2026-09-01 | WineHQ Homebrew casks disabled (Gatekeeper) |
| 2026-09-25 | macOS_Wine_builds tag 11.18 published |
| 2026-10-18 | GPTK 4.0 beta 3 dmg analysis — no wine inside |
| 2026-10-19 | wine-crossover repo + releases 404; tap narrowed to GPTK-only cask |

## 8. Audits — status (2026-10-19)

1. ✅ **macOS_Wine_builds layout** (resolved — partial-range listing of the 11.18
   staging tarball): the archive root is an app wrapper — `Wine Staging.app/`, wine
   tree at `Wine Staging.app/Contents/Resources/wine/{bin,lib,share}` with
   **gecko and mono bundled** (`share/wine/{gecko,mono}` — no extra prefix
   downloads needed). Catalog layout rule: probe
   `<root>/*.app/Contents/Resources/wine/bin/wine{,64}` after extract; the
   wrapper's `Info.plist` / `start/` launcher scripts are not needed at runtime.
2. ✅ **wine-stable asset naming** (resolved): stable ships on the same release
   train — `wine-stable-11.0_1-osx64.tar.xz` on tag `11.0_1`. The
   `wine-<flavor>-<tag>-osx64.tar.xz` pattern holds across flavors; tags may carry
   `_N` suffixes, so the asset glob must tolerate them. All observed releases are
   `prerelease: false`.
3. ✅ **GPTK cask stanza** (resolved): `gcenx/wine/game-porting-toolkit` version
   `3.0-3`, sha256-pinned, backed by a **public auth-free GitHub release**:
   `https://github.com/Gcenx/game-porting-toolkit/releases/download/Game-Porting-Toolkit-3.0-3/game-porting-toolkit-3.0-3.tar.xz`
   (Gcenx-packaged GPTK including an overlaid wine tree). Consequence: gogo may act
   as a **download conduit on explicit user consent** (Apple EULA notice at the
   prompt); gogo never redistributes the payload in its own artifacts
   (design 12 D9).
4. ✅ **Lutris `wine:` key inventory** (resolved from `docs/installers.rst`): the
   runner section supports `version`, `dxvk`, `dxvk_version`, `esync`,
   `overrides`. Only `version` (catalog mapping, design 12 §7) and `overrides`
   (registry — already implemented) carry gogo actions; `dxvk*` / `esync` are
   toggles honored with notices (DXVK on mac needs MoltenVK — future work,
   design 10 §10).
5. **winetricks tool-check block** — the script's exact presence checks and fallback
   order for cabextract vs 7z; shim names/flags must match what it actually invokes.
6. **Shim crate licenses** — `cab` (MS-CAB read) and `sevenz-rust` (7z read): license
   and maturity review for the `gogo-shims` crate (design 12 D5).
