# 11 — Recipes: Lutris installer scripts for `gogo mac-app`

**Status:** implemented — phased with design 12 §12: selection + parity
executor shipped (PR #30), `gogo recipes list|show` inspection in phase 5
**Depends on:** design 10 (`gogo mac-app`), 07 (events/errors), 06 (paths), 04 (downloader)
**Research:** docs/14 (systems survey + live Lutris API probes, 2026-10-19)
**Acceptance bar:** the user can choose from the Lutris installers relevant to
their game and apply the chosen one — **functional parity with Lutris** for
the steps wine games actually use.

`gogo mac-app` converts well-behaved installers out of the box (live:
Pharaoh Gold needed five more ingredients: a different runtime, an exe swap,
cnc-ddraw, a Windows-version override, sync env flags — design 11 §3.3).
Every one of those ingredients is already encoded in community **Lutris
installer scripts**, keyed by GOG id (live-verified: `cuphead-gog` carries
the `/NOGUI` installer flags *and* `task: winetricks app=corefonts`; the
Pharaoh fix chain maps row-for-row onto script steps). gogo **consumes**
Lutris **live**: during `mac-app` it searches lutris.net for the game, shows
the candidate scripts, and **asks the user which one to apply**. Nothing is
stored on gogo's side — no recipe cache, no corpus, no freshness state. The
fetched script and its downloaded files live in a per-run temp working set
and are deleted when the conversion ends. gogo hosts no recipe content and
defines no format — it is a transient client of the corpus, not a publisher.

## 1. CLI surface (03 §1 carries the user-facing summary)

```
gogo mac-app <installer-path> [--recipe auto|none|<installer-slug>|<file>]
gogo recipes list <installer-path>       # the relevant Lutris installers for this game
gogo recipes show <installer-slug|file>  # the execution plan: applied vs skipped, with reasons
```

- **Selection is the default behaviour** (what `lutris.net/games/<slug>`
  shows, but in the terminal): during `mac-app`, resolve the GOG id from
  `goggame-<id>.info` → Lutris game → its community installers (runner,
  version, author, updated_at, rating) → **prompt the user to pick one**
  (rendering the ranked list; `1/n/none`). `--recipe <installer-slug>` skips
  the prompt (scripting); `none` disables lookup entirely; `--recipe <file>`
  applies a downloaded script JSON offline; `--json` prints the candidate
  list as JSON instead of prompting.
- After the pick, the fetched script is **planned and shown** (applied steps
  vs skipped-with-reasons) before execution — `gogo recipes show
  <installer-slug|file>` is that inspection standalone (live fetch, nothing
  cached).

## 2. Discovery & selection (live-verified, docs/14 §2.1.1)

```
goggame-<id>.info ──(gog id)──►  GET /api/games?search=<gameName>&with-installers=True
                                 │   match provider_games[] where service=="gog"
                                 │   and slug ∈ {goggame id, library product id}
                                 ▼
                        lutris game slug
                                 ▼
        GET lutris.net/games/<slug>  ── parse "Download script" hrefs:
                                 │   /api/installers/<installer-slug>?format=json
                                 ▼
        GET /api/installers/<installer-slug>   → { runner, version, gogid,
            gogslug, published, updated_at, rating, script: {…JSON object…} }
```

Ranking for `auto` (candidates must be `runner: wine`, `published: true`):

1. `--recipe <installer-slug>` wins outright.
2. Scripts whose `files:` reference a GOG installer (they encode GOG-install
   flags like `/NOGUI`).
3. Highest `updated_at`; `rating`/`is_playable` as tie-breakers.

Cross-check: the fetched record's `gogid`/`gogslug` must agree with our gog
id (string compare); on mismatch → `RecipeMiss` + warn (matched a different
edition).

Name-derived discovery hardening (live findings 2026-10-19, shipped with the
phase-5 follow-up):

- **Version-tail strip**: GOG installers are `setup_<name>_<major.minor…>.exe`;
  the display name drops the trailing underscore-delimited dotted version
  (`setup_pharaoh_gold_2.1.0.15.exe` → "Pharaoh gold"). Multi-part dotted
  numbers only — "Portal 2"-style titles keep their trailing token.
- **Query fallback**: Lutris search is strict ("pharaoh gold" → 0 results while
  the game is listed as "Pharaoh"); the search drops trailing tokens until
  something answers (never below one token).
- **Overlap pick**: `pick_game` keeps exact-name → slug, then scores by
  normalized token overlap (GOG-linked first, shortest name as tie-break)
  instead of blindly taking the first result — "Pharaoh gold" over the 20
  loose "pharaoh" matches lands on `pharaoh` (carries
  `pharaoh-cd-cnc-ddraw`), not "Arizona Rose and the Pharaohs' Riddles".
  The post-install gogid cross-check still guards the pick.

## 3. Interpretation — the parity executor

The script is JSON (`serde_json`; **no YAML dependency**). Interpretation
produces a `RecipePlan` before anything runs: `gogo recipes show` renders it
(applied steps vs skipped-with-reason), and execution emits
`RecipeStep { step, verb }` / `RecipeSkipped { step, reason }` events.

### 3.1 Parity matrix (what wine-game scripts actually use)

| Lutris input | gogo action |
|---|---|
| `files: {id: <url>}` | **download** into the recipe cache via the existing chunk-verified downloader (04); checksum if the script carries one (rare); `N/A:` user-prompt files → skipped (no interactive file picking in automation) |
| `task: {name: wineexec, executable: <id>, args, prefix, arch}` | run the downloaded/installed exe **through the resolved runtime** with `WINEPREFIX=$PREFIX`, argv logged, timeout guard (10 §2) — this is how scripts drive the GOG setup and apply patch installers |
| `task: {name: winetricks, app: <verbs>}` | **vendored + shimmed** (design 12 §8, amends the bridge posture): a pinned, sha256-verified winetricks from the runtime store runs with `WINEPREFIX=$PREFIX`, one verb per call, argv logged; Rust-made `cabextract`/`7z` shims are PATH-scoped to the invocation (zero host installs). A user-installed binary (`WINETRICKS` / PATH) overrides; still nothing → `RecipeSkipped { reason: "requires winetricks: <verbs>" }` + warn |
| `extract: {file: <id>, dst, ...}` | zip / tar / tar.gz / tar.xz into `dst` (crates: `zip`, `tar`, `flate2`, `xz2`); 7z via the same readers as the shims (design 12 D5); rar → skipped-loud (v2) |
| `merge`/`copy`, `move`, `write_file`, `write_config`, `chmodx`, `mkdir` | applied via std::fs after substitution + scope validation |
| `wine.overrides: {ddraw: "native,builtin"}` | `wine reg.exe add` under `HKCU\Software\Wine\DllOverrides` through the resolved runtime — no shell, argv logged |
| `wine: {version}` (plus `dxvk` / `dxvk_version` / `esync`) | **advisory** (design 12 §7, D6): `version` maps through catalog data to a runtime recommendation (Linux-only pins → `wine-staging` + notice); toggles honored with notices; never a blocker |
| `game.exe`, `game.args` | entry-point override (skips the `goggame-*.info` scan) and launcher args |
| `system.env: {K: V}` | env merged into `launch.conf` |
| `task: {name: wineexec, args}` on the script's **GOG setup** file | the setup is not re-run (gogo already installed); its `args` are **harvested** into our silent Inno flag list (captures `/NOGUI`-class lore) |
| `execute` / `execute: command:` (host shell) | **skipped by default**, counted; `--allow-recipe-exec` runs them (v1.1) — the one deliberate parity gap |
| `insert_disc`, `task: create_prefix`, unknown tasks | skipped-loud (create_prefix is implicit — gogo stages the prefix) |
| `game.arch: win32` | warn (gogo stages win64; GPTK/CX64 wine is wine64-only) — proceed |

### 3.2 Variables

`$GAMEDIR` → game dir inside the prefix (`…/wine/drive_c/GOG/<slug>`) ·
`$CACHE` → recipe cache scratch (`…/recipes/cache/`) · `$PREFIX` → staged
WINEPREFIX · `$BUNDLE` → the `.app` root · `$USER` → the prefix's actual
user dir name · `$RESOLUTION_WIDTH/HEIGHT` → main display pixels ·
`$WINEBIN` → bundled wine path · `$DISC`, `$INPUT_*`, `$SCRIPTDIR` →
unsupported (steps using them are skipped-loud), plus `script.variables`
custom substitutions.

After substitution, every path a step touches is canonicalised and must
resolve **inside `$BUNDLE`** (`PathEscape` → step skipped; fatal in pinned
mode). Downloads land in the recipe cache, never directly in the bundle;
only extract/merge steps move bytes into it.

### 3.3 Case study — Pharaoh Gold (live, 2026-10-19)

| Working-config ingredient | Corpus source | Executor target |
|---|---|---|
| Wine-CX 8.0.1 runtime, **not** GPTK 1.1 (crashes the game) | Porting Kit engine choice; corpus `wine:` hints | `runtime:` advisory + loud warning |
| GOG 2016 exe exits silently → community 2000 exe swap | Lutris `patch_pharaoh.zip` (`files:` + `extract`) | **§3.1 download + extract** |
| cnc-ddraw in syswow64 + `*ddraw=native,builtin` | winetricks `cnc_ddraw` (PK `winetricks.log`) | **§3.1 winetricks bridge** + overrides |
| `renderer=gl`, `Version=winxp` per-app override | PK winetricks `renderer=gl`; winecfg per-app version | registry verb + launch config |
| `WINEESYNC=1 WINEMSYNC=1`, CWD = game dir | wrapper `Info.plist` / launcher behaviour | `launch:` env + stub |

Six ingredients, every one already encoded by the community — zero
hand-derived game knowledge, and with §3.1's parity executor, all but the
runtime choice are expressible in v1.

## 4. Fetch & ephemeral working set (07 retry rules)

- Endpoint base `https://lutris.net` (config override `recipes.lutris_base`,
  05 §schema, for mirrors).
- **No recipe storage.** Each conversion fetches the game resolution, the
  chosen script, and its `files:` into a per-run temp working set
  (`$(tempdir)/gogo-recipe-<pid>/`), which is removed when the conversion
  ends — success, failure, or interrupt. Nothing recipe-related persists
  between runs; re-running re-searches and re-asks.
- `GET` only, polite UA, 07 §retry matrix (backoff on 5xx; a Lutris outage
  degrades to "no recipes available" — the pipeline continues recipe-less).

## 5. Pipeline integration (10 §2 annotated)

```
plan → resolve runtime → stage prefix → INSTALL (harvested flags folded in)
     → RECIPE STEPS (downloads → wineexec/winetricks/extract/file ops)
     → entry-point scan (skipped if game.exe set)
     → assemble (launch.conf carries recipe env/args) → strip → codesign
```

Scripts are fetched + planned **before** the install step (installer-flag
harvesting must precede it); steps execute after the install; registry steps
run with the staged prefix and the same timeout/null-stdio discipline as
every runtime invocation.

## 6. Errors & exit codes (03 §5 unchanged)

Applied-step failures abort the conversion → `MacAppError::Recipe` →
**exit 7**; no new exit code.

| Variant | When |
|---|---|
| `Recipe::StepFailed { step, msg }` | an applied step failed (wineexec exit ≠ 0, download exhausted, …) |
| `Recipe::PathEscape { path }` | post-substitution path outside the bundle (pinned mode) |
| `Recipe::Unresolvable { msg }` | `--recipe <slug>` could not be fetched/parsed |

Auto-mode discovery/selection trouble is **never** fatal: `RecipeMiss` +
warn, pipeline continues recipe-less.

## 7. Security model

- **The user is the trust anchor.** Recipes are applied when the user picks
  them (explicitly or by confirming the selection); `auto` mode is ranking
  over that same reviewed corpus.
- **No host shell by default**: `execute` steps are skipped and counted;
  `--allow-recipe-exec` unlocks them (v1.1). Every other step runs through
  typed executors (runtime argv, winetricks argv, std::fs, downloader).
- **Rooted filesystem**: canonicalised paths checked against `$BUNDLE` ∪
  `$CACHE` — the per-run recipe cache is a first-class working root outside
  the bundle by §4 (`files:` download → `extract` → `merge`); escapes past
  both roots fail the step (fatal when pinned).
- **Advisory recipes degrade, pinned recipes abort** (amended 2026-10-19):
  a hard failure in the recipe phase *after* the game installed downgrades
  to a loud `Warn` + recipe-less continuation for search-discovered recipes
  — never a launcher-less half-bundle (macOS reads those as "damaged").
  Pinned (`--recipe`) failures stay fatal per the contract above.
- **Downloads** go to the gogo cache (never executed from there except by
  wineexec through the runtime) and carry the script's checksum when present.
- Registry writes are prefix-local by mechanism (`WINEPREFIX=$PREFIX`).
- Provenance (game, version, author, `updated_at`, `rating`) travels in
  `RecipeResolved` and `recipes show`.

## 8. Testing (08 conventions)

- **Fixtures**: recorded `/api/installers/<slug>?format=json` payloads —
  `pharaoh-cd-cnc-ddraw` (overrides + downloads + extract + merge),
  `cuphead-gog` (wineexec GOG-flag harvesting + winetricks), a synthetic
  script covering every skip class and `$CACHE` flows.
- **Unit** (lutris.rs / interpret.rs): discovery-match matrix (goggame id vs
  library id vs alias); ranking; substitution; scope validation incl. `..`
  and symlink escape; skip classification; wineexec-flag harvesting from the
  GOG-setup file id.
- **Integration** (`tests/macapp_pipeline.rs`, fake wine runtime):
  - `wine.overrides` → `reg.exe add … /v ddraw /d native,builtin /f` with
    `WINEPREFIX` set;
  - harvested `/NOGUI` → installer argv gains the flag;
  - `files:` URL → downloader invoked, bytes land in recipe cache; `extract`
    → layout assertions; `merge` → file replaced;
  - `winetricks` with a fake winetricks on PATH → argv per verb; without →
    `RecipeSkipped` with the verb list;
  - `wineexec` → fake runtime argv + exit-code propagation;
  - `game.exe` overrides entry point; `system.env` lands in `launch.conf`;
  - `--recipe auto` with the Lutris client stubbed to 404 → `RecipeMiss`,
    sequence identical to the no-recipe baseline;
  - pinned slug, unreachable network → exit 7 (`Unresolvable`).
- **HTTP** behind a `LutrisClient` trait; tests use in-memory fixtures; no
  network in CI. Working-set teardown (temp dir removed on success/failure/
  interrupt) is asserted explicitly.

## 9. gogo-core layout (01 §1 tree gains)

```
├── macapp/
│   ├── lutris.rs         # LutrisClient trait: search, game page, installer fetch; cache
│   ├── interpret.rs      # script JSON → RecipePlan (parity matrix §3.1)
│   └── (design 10 modules, unchanged shape)
```

CLI: `MacApp` gains `--recipe`; new `recipes list|show`. Dependencies:
`zip`, `tar`, `flate2`, `xz2` (extract; 7z via the shim readers, design 12 D5);
`serde_json` only for the script itself — no YAML. winetricks posture amended
2026-10-19 (design 12 D4/D5): pinned vendored download + Rust extractor shims; a
user-installed binary overrides; loud skip stays the floor.

## 10. Future (explicitly out of scope here)

- `execute` shell steps via `--allow-recipe-exec` (v1.1).
- rar/7z extract; `insert_disc` flows (CD-script support; gogo's offline
  flow makes these rare).
- Redistributable steps beyond winetricks with pinned URL + sha256 + license
  field (Bottles pattern) — mostly subsumed by winetricks bridging.
- Recipe coverage for plain `gogo install` (Linux prefix flows).
- Recipe-lint contributions upstream (Lutris script CI).
