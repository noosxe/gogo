# 18 — mac-app from a depot tree (research)

Date: 2026-10-20 · Status: research for design 15 · Live facts from the warm cache
(`~/Library/Caches/gogo/depot/1207659046/`, PR #40's Pharaoh download).

## 1. The depot tree is an installed game

Verified against the live cache (779 MiB):

- `Pharaoh Gold/Pharaoh.exe` + `Data/`, `AUDIO/`, `BINKS/`, manuals, 59 root entries —
  byte-for-byte the layout `setup_pharaoh_gold.exe /S` produces under wine, obtained
  with zero installer execution.
- **`goggame-1207659046.info` sits in the tree root**, alongside `.dll/.ico/.script/
  .hashdb` siblings. Our existing `macapp/info.rs` (`find_entry_point`, `game_name`,
  primary-`FileTask` selection, relative-path resolution) works on it as-is.
- Sibling to the game dir: `gog-support/1207659046/…` (Galaxy support files).
  ~~Not needed for launching; ignored.~~ **Corrected 2026-10-23 (docs/19 §1 item 4):**
  needed for some games. `gog-support/<id>/<id>/app/**` holds files Galaxy merges into
  the install dir, and without them Carmageddon TDR 2000 aborts at `File.cpp:73`
  (missing `ASSETS/options.txt`). The support installer `galaxy_*.exe` (design 15 D3)
  lives in the same tree. The overlay is merged by #64 (design 15 §3).

Consequence: a `--from-depot` bundle lane can reuse the entire post-install half of
the mac-app pipeline (entry discovery, recipe-scope conventions, cnc-ddraw placement,
launcher conf, codesign) and replace only the *install step* — `installer::run_silent`
becomes a tree copy into `prefix/drive_c/GOG/<tree-name>`.

## 2. Copy cost — APFS clonefile

- The tree must live **inside the bundle** (self-contained contract, D-series: no
  links into a purge-able cache). A naïve copy doubles ~780 MiB.
- APFS `clonefile(2)` gives copy-on-write copies: instant, no extra space until
  divergence. Fallback for non-APFS/foreign filesystems: recursive byte copy.
- libc exposes `clonefile`; destination must not exist (parent must), `ENOATTR`/
  cross-device → fallback.

## 3. Recipes are installer-shaped

Lutris scripts assume an install flow (`insert-disc`, `task create_prefix`,
`wineexec setup.exe`, …). A depot tree already **is** the installed artifact those
steps produce. Post-install steps (registry writes, `write_config`) could apply in
principle, but no first-class signal separates them. v1: recipes are off in depot
mode (only `none`); revisit selectively later (design 15 D6).

## 4. Cache warmth and auth

- `mac-app` today is auth-free (pure-local operation). A game-selector input needs
  the session (library resolution) and either a warm cache or a depot download.
- `gogo depot` already owns resolution + download; the CLI layer can run that lane
  and hand `macapp::plan` a plain directory — keeping gogo-core's macapp module
  pure-local and testable (fake trees, no auth).

## 5. The support-installer gap (live finding, 2026-10-20)

First depot-lane bundle: black screen, process alive, audio never initialized —
while an offline-lane control (same wine, same cnc-ddraw, same flags) ran with
audio. Tree diff showed the only deltas were installer-written artifacts
(`Pharaoh.ini`, `*.inf`, `goglog.ini`, uninstaller, Start-menu `.lnk`). Root
cause: `Pharaoh.ini`'s `CDDrive=G` — without the config the exe waits in its CD
check forever. Copying the four config files in made the depot bundle run
(audio live, full window). Proper fix attempt (design 15 D3 amendment): synthesize the
`HKLM\Software\GOG.com\Games\<id>` registry key the main installer writes
(full value set replicated from an offline-lane prefix), then run the depot
cache's own `gog-support/**/galaxy_*.exe` into the clone. Result: the support
Inno still declines — `/LOG` shows `InitializeSetup returned False` right
after extracting `get_hw_caps.dll`, identically under GPTK 3.0 and wine-staging
11.18, with and without the registry key. The gate is internal GOG script
logic. Shipped semantics: registry bootstrap + best-effort support run,
failure = loud Warn + offline-lane remedy hint. Self-contained games (most of
the modern catalog) are unaffected; the install-config class (old Sierra-style
games like Pharaoh) should use the offline installer lane.

## 6. Prefix parity

The offline lane's game lands at `C:\GOG\<slug>` (installer target). Cloning the
depot tree to `drive_c/GOG/<tree-name>` reproduces that shape — `Pharaoh Gold`
even matches the slug the offline lane derives. Registry expectations, save-game
paths next to the exe, and the launcher's unix-path invocation all behave
identically.
