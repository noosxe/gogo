# Design 15 — mac-app from a depot tree

Status: contract · Depends on: docs/18 (research), design 10 (mac-app), design 14
(depot CLI lane) · 2026-10-20.

## 0. Scope

`gogo mac-app --from-depot <GAME-or-PATH>` builds the .app from a depot
(content-system) game tree instead of running an offline installer under wine.
Everything after the "install" step is shared with the offline lane (docs/18 §1).

## 1. Command surface

```
gogo mac-app --from-depot <GAME|PATH> [--name NAME] [--runtime …] [--out DIR]
                 [--ddraw-wrapper|--no-ddraw-wrapper] [--yes] [--json-progress|--json]
```

- `--from-depot` conflicts with the positional `INSTALLER` (usage error if both).
- `GAME|PATH`: an existing directory → used directly (local, auth-free, testable).
  Otherwise a game selector (id/slug/fuzzy, resolved with a session) → depot cache
  `~/Library/Caches/gogo/depot/<id>/<install-dir>` (design 14 D2).
- `--name` overrides the display name (default: `goggame-*.info` `gameName`, else
  the tree directory name; `strip_version_tail` not applied — the info name is
  already clean).
- Cold cache (selector form): on a TTY, confirm-download prompt (default yes) shows
  the depot plan sizes; headless requires `--yes` (auto-downloads) or fails with a
  "run `gogo depot <GAME>` first" hint. Download uses the design 14 driver as-is.
- `--recipe`: only `none` is accepted in depot mode — any other value is a usage
  error; `auto`/absent behaves as `none` (recipes are installer-shaped, docs/18 §3).

## 2. D2 — Core data model (gogo-core, minimal diff)

- `MacAppOptions.installer: PathBuf` → `Option<PathBuf>`; new
  `game_tree: Option<PathBuf>` (CLI resolves selectors/cache/downloads first).
  Exactly one is `Some` (enforced by clap conflicts; core asserts).
- `MacAppPlan.installer: installer::Plan` → `Option<installer::Plan>`; new
  `game_tree: Option<PathBuf>` + `game_tree_bytes: u64`.
- `plan()`: depot branch skips `installer::plan`; display name per §1; estimated
  bytes = runtime + tree; the recipe phase is skipped wholesale (`SelectedRecipe`
  stays `None`).

## 3. D3 — Execute: the swap

Offline lane order (runtime copy → prefix stage → silent install → …) is preserved;
the depot lane replaces exactly one step:

```
stage_prefix                       (unchanged — fresh wineboot prefix)
clone_tree(src=game_tree, dst=prefix/drive_c/GOG/<tree-name>)   ← instead of run_silent
[gog-support installer → run_silent into the clone]              ← D3 amendment
stop_wineserver                    (unchanged quiesce)
… entry discovery inside the CLONED dir (goggame info paths are relative)
… ddraw apply + override, conf, launcher, plist, sign            (unchanged)
```

**D3 amendment (2026-10-20, live finding):** the depot manifest ships only game
files — the exe also needs the *installer-written* side artifacts to start.
Symptom without them: Pharaoh stalls in its CD check (`Pharaoh.ini` carries
`CDDrive=G`), i.e. black screen, audio never initializes, process stays alive.
The depot cache already carries GOG's own remedy: `gog-support/**/galaxy_*.exe`
(sibling of the install tree) — the support installer that writes exactly those
files + registry keys. The depot lane runs it silently into the clone
(`/DIR=C:\GOG\<tree-name>`, same `run_silent` machinery as the offline lane).
Present-but-failing **degrades to a loud Warn** (amended again 2026-10-20):
the support Inno's `InitializeSetup` declines even against a fully replicated
`GOG.com\Games\<id>` registry key and under two independent wines (GPTK 3.0,
Gcenx staging 11.18) — the gate is internal GOG script logic (suspected
`get_hw_caps` probe / Galaxy handshake), not our environment. The lane
synthesizes the key anyway (principled parity with a real install) and
attempts the support run; on decline the bundle ships with a warning naming
the offline lane as the remedy for that game class.

- `install_dir` = `prefix/drive_c/GOG/<tree-name>` in both lanes (docs/18 §5).
- `InstallStarted/InstallFinished` events carry the tree bytes in depot mode.

## 4. D4 — clone_tree

`bundle::clone_tree(src, dst)`:

- macOS: `clonefile(src, dst, 0)` on a not-yet-existing `dst` (parent created);
  on any errno → fallback. Rust `std::fs::copy`-based recursive walk fallback
  (preserving permissions) for non-APFS, cross-device, or non-macOS.
- Errors: missing `src`, existing `dst`, mid-copy failure → `MacAppError::Io`
  (partial bundle left for diagnosis, same contract as the installer lane).

## 5. D5 — Events

No new event kinds: depot resolution emits `gogo:`-style `Warn`-message progress
lines via the existing channel (`DepotResolved`-style detail folded into
`InstallStarted`'s semantics). The design 14 `DepotEvent` stream (when the CLI
auto-downloads) is rendered by the CLI exactly like `gogo depot`.

## 6. D6 — Out of scope (follow-ups)

Recipe post-install steps in depot mode; `--from-depot --with-dlcs` pass-through
(default: whatever the cache holds — re-running `gogo depot --with-dlcs` refreshes
it); DOSBox/ScummVM native lanes (design 13 backlog).

## 7. Testing

- Unit: `clone_tree` (fallback copy, existing-dst error, missing-src error),
  display-name-from-info, options conflict assertion.
- Integration (`macapp_pipeline.rs`): fake depot tree (`goggame-<id>.info` with a
  primary FileTask + a stub exe) + fake runtime → full execute() → assert the tree
  exists at `drive_c/GOG/<name>`, launcher conf's GAME points at the cloned exe,
  and the launcher executes it end-to-end (existing harness pattern).
- Live: Pharaoh from the warm cache → launch window.

## 8. Documentation

`docs/18` + this contract; README row for `--from-depot`; design 10 §6 note that
the installer input is now one of two sources.
