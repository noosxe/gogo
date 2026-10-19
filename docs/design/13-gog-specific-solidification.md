# Design 13 — GOG-specific solidification, phase 1: cnc-ddraw + gog ranking

**Status: contract for implementation.** Backed by `docs/16` (research, 2026-10-19).
Scope of this phase: **S1** (native cnc-ddraw application) and **S5** (gog-ranking
of recipe candidates). S2 (DOSBox/ScummVM lane) and later items get their own
contracts. Amends design 11 (recipes) and design 12 (catalog/store) minimally.

## 1. Problem

DirectDraw-era GOG games (the bulk of the classic catalog) black-screen or
instant-exit under wine's mac display driver. The community cure — the
cnc-ddraw wrapper (FunkyFr3sh, MIT) dropped into the game dir — is delivered
today only through Lutris CD-recipes whose paths don't match our GOG layout
(docs/16 §1, live-proven 2026-10-19). gogo must own this fix natively.

## 2. Decisions

- **D1 — choice surface.** `MacAppOptions.ddraw: DdrawChoice { Auto, On, Off }`.
  CLI: `--ddraw-wrapper` → `On`, `--no-ddraw-wrapper` → `Off`, default `Auto`.
  `Auto` resolves at plan time: interactive renderer present → `Confirm`
  (default **true**, after the recipe picker, before the runtime menu);
  headless → **Off** (no surprise downloads in scripts/CI). The resolved bool
  rides on `MacAppPlan` (like `runtime_downloaded`).
- **D2 — provisioning.** New catalog `Kind::Tool` entry `cnc-ddraw`
  (`FunkyFr3sh/cnc-ddraw` GitHub release, asset `cnc-ddraw.zip`, MIT,
  redistributable). `Kind::Tool` is generalized to carry its probe payload
  (`Kind::Tool { payload: &'static str }`: `winetricks` keeps its script +
  chmod semantics; `cnc-ddraw` probes `ddraw.dll`). `store::install` gains
  zip extraction (zip crate, same extract-dir flow as tar). Cache + manifest
  + `gogo runtimes list` visibility for free.
- **D3 — application.** After the recipe phase and entry resolution (install
  dir known): copy `ddraw.dll` + `Shaders/` from the store tree into the game
  dir; write a gogo-authored `ddraw.ini` (`renderer=gdi` pinned for
  determinism, `fullscreen/maintas/windowed=true`); and set the per-prefix
  DllOverrides `ddraw = n,b` via the bundled wine. The override is the
  load-order half — wine's default for ddraw is builtin-first, so the
  game-dir dll is silently ignored without it (live finding 2026-10-19:
  `builtin` vs `native` in `+loaddll` traces decided working vs black-screen;
  winetricks' cnc-ddraw verb and Lutris `wine.overrides` do exactly this).
- **D4 — never fight the recipe.** If the recipe phase already placed a
  `ddraw.dll` in the game dir, skip with a loud `Warn` instead of overwriting.
- **D5 — notices + events.** `Contents/Resources/LICENSE-cnc-ddraw.txt` (MIT
  text + upstream URL) when applied. Events reuse the existing vocabulary:
  `RecipeStep { verb: "cnc-ddraw" }` on success, `Warn` on skip/degrade.
- **D6 — S5 ranking.** `lutris::rank` gains a primary key: installers whose
  record carries `gogid` sort before those without (ahead of the gog-file-URL
  heuristic and recency). Affects the picker, `recipes list`, and the
  headless ranked-best equally.

## 3. Testing

- Unit: store zip round-trip (fixture zip → manifest entry, `ddraw.dll`
  probed); ddraw apply (fixture store tree → dll + Shaders + ini in game dir;
  AlreadyPresent when a dll exists); rank (gogid beats non-gogid, stable).
- Integration (fake runtime + wiremock GitHub): full execute with wrapper `On`
  → bundle contains `ddraw.dll`, `ddraw.ini`, `LICENSE-cnc-ddraw.txt`;
  headless default `Auto` → none of them.
- CLI: flag parsing (`--ddraw-wrapper` / `--no-ddraw-wrapper` mutually
  exclusive — clap conflict).
