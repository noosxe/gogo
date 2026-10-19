# 11 — Recipes: consuming Lutris installer scripts for `gogo mac-app`

**Status:** proposed contract (design phase; no implementation yet)
**Depends on:** design 10 (`gogo mac-app`), 07 (events/errors), 06 (paths)
**Research:** docs/14 (systems survey + live Lutris API probes, 2026-10-19)

`gogo mac-app` converts well-behaved installers out of the box (live:
Pharaoh Gold, 2026-10-18). Real catalogues need per-game fixes — DLL
overrides, registry tweaks, extra installer flags, entry-point corrections.
The community has already encoded most of this in **Lutris installer
scripts** (thousands of GOG games). gogo **consumes** them: it looks up the
game on lutris.net, fetches the script, and interprets a strictly
declarative subset. gogo defines no recipe format and hosts no recipe
content — it is a consumer, not a publisher.

## 1. CLI surface (03 §1 carries the user-facing summary)

```
gogo mac-app <installer-path> [--recipe auto|none|<installer-slug>|<file>]   # default: auto
gogo recipes show [<query>]     # print the interpreted plan for a game/installer
```

- `auto` (default): derive the GOG product id from the installer's
  `goggame-<id>.info` (already parsed in design 10 §2) → find the Lutris
  game → enumerate its community installers → pick one → interpret it. Any
  failure in this chain is **not an error**: emit `RecipeMiss` and proceed
  recipe-less (today's behaviour).
- `none`: disable lookup entirely.
- `<installer-slug>`: pin a specific Lutris installer (e.g.
  `--recipe pharaoh-cd-cnc-ddraw`); fatal if it cannot be fetched.
- `<file>`: interpret a script JSON already on disk (the exact
  `/api/installers/<slug>?format=json` download) — offline/authoring of bug
  reports, not a gogo recipe format.
- `gogo recipes show` prints, for the selected installer: metadata (game,
  runner, version, author, updated_at) and the **interpreted plan** — every
  mapped action *and* every skipped step with the reason. No network needed
  for `<file>`; otherwise uses the cache.
- No `gogo recipes update`/`list` — there is no gogo-managed corpus;
  the cache is per-game and transparent (§3).

## 2. Discovery & selection (live-verified, docs/14 §2.1.1)

```
goggame-<id>.info ──(gog id)──►  GET /api/games?search=<gameName>&with-installers=True
                                 │   match provider_games[] where service=="gog"
                                 │   and slug ∈ {goggame id, library product id}
                                 ▼
                        lutris game slug (+ alias match)
                                 │
                                 ▼
        GET lutris.net/games/<slug>  ── parse "Download script" hrefs:
                                 │   /api/installers/<installer-slug>?format=json
                                 ▼
        GET /api/installers/<installer-slug>   → { runner, version, gogid,
            gogslug, published, updated_at, rating, script: {…JSON object…} }
```

Selection rules, in order:

1. `--recipe <installer-slug>` wins outright.
2. Candidates must be `runner: wine`, `published: true`.
3. Prefer scripts whose `files:` reference a GOG installer (they encode
   GOG-install flags like `/NOGUI`); then highest `updated_at`.
4. Ambiguity is fine in `auto` mode (miss = warn); pinned mode resolves to
   exactly one installer or errors.

Cross-check: the fetched record's `gogid`/`gogslug` must agree with our gog
id (string compare); on mismatch → `RecipeMiss` + warn (the search matched a
different edition).

## 3. Interpretation — the declarative subset

The script is JSON (serde_json; **no YAML dependency**). Interpretation
produces a `RecipePlan`; everything mappable is applied, everything else is
skipped with a `RecipeSkipped { step, reason }` event. Unknown/unsafe content
is data to report, never code to run.

### 3.1 Mapping table

| Lutris input | gogo action |
|---|---|
| `wine.overrides: {ddraw: "native,builtin", …}` | registry writes under `HKEY_CURRENT_USER\Software\Wine\DllOverrides` via `wine reg.exe add … /t REG_SZ /f` through the resolved runtime (10 §3) — no shell, argv logged |
| `game.exe`, `game.args` | entry-point override (skips the `goggame-*.info` scan) and launch args in the launcher stub |
| `system.env: {K: V}` | env entries merged into `launch.conf` |
| `installer[]` step `task: {name: wineexec, args: …}` whose `executable` is the script's `files` entry for the **GOG setup** (id/name match: contains `setup`/`gog`/`N/A:` prompt) | harvest `args` → appended to our silent Inno flag list (10 §2). The `wineexec` itself is *not* executed — gogo already ran the installer |
| `installer[]` `merge`/`copy`, `move`, `write_file`, `write_config`, `chmodx` with paths under `$GAMEDIR` | applied via std::fs after substitution + scope validation |
| anything else: `execute` (arbitrary command), `files:` downloads, `extract` of downloaded files, `task: winetricks`, `task: create_prefix`, unknown task names, `insert_disc`, … | `RecipeSkipped` + warn event; v2 candidates are pinned-URL downloads (Bottles pattern) and a winetricks bridge — design 11 §9 |
| `game.arch: win32` | warn (gogo stages win64; GPTK wine is wine64-only) — proceed |
| `script.variables: {K: V}` | substitution map for the variables below |

### 3.2 Variable substitution

| Lutris var | gogo value |
|---|---|
| `$GAMEDIR` | the game install dir inside the prefix (`…/wine/drive_c/GOG/<slug>`) |
| `$BUNDLE` | the `.app` root |
| `$PREFIX` | the staged WINEPREFIX (`…/Resources/wine`) |
| `$USER` | the prefix's actual user dir name (read from `drive_c/users/`) |
| `$RESOLUTION_WIDTH` / `$RESOLUTION_HEIGHT` | main display pixel size (macOS: CoreGraphics) |
| `$CACHE`, `$WINEBIN`, `$INPUT_*`, `$SCRIPTDIR` | unsupported → any step using them is `RecipeSkipped` |

After substitution, every path a mapped step touches is canonicalised and
must resolve **inside `$BUNDLE`** — otherwise the step (not the pipeline) is
skipped with `PathEscape` recorded; in pinned mode it is fatal.

## 4. Cache & fetch (06 §1 paths; 07 retry rules)

- Endpoint base `https://lutris.net` (config override `recipes.lutris_base`,
  05 §schema, for mirrors).
- Fetched scripts are cached at
  `~/Library/Caches/gogo/recipes/lutris/<installer-slug>.json` with the fetch
  time; `auto` re-resolves when older than 24 h (or when the gog id has no
  cached resolution); pinned slugs reuse the cache and refetch with `--refresh`.
- `GET` only, polite UA, 07 §retry matrix (backoff on 5xx; 404 → clean
  `RecipeMiss`).
- `gogo recipes show [<query>]` renders from `<file>` or cache.

## 5. Pipeline integration (10 §2 annotated)

```
plan → resolve runtime → stage prefix → INSTALL (gogo installer, + harvested flags)
     → RECIPE STEPS (overrides registry, file ops) → entry-point scan (skipped if exe set)
     → assemble (launch.conf carries recipe env/args) → strip → codesign
```

- Installer-flag harvesting must happen **before** the install step, so the
  script is fetched and interpreted first; file steps run after the install
  (files exist) and before the entry-point scan.
- Registry steps spawn `wine reg.exe` with `WINEPREFIX` staged and the same
  timeout/null-stdio discipline as every runtime invocation (10 §2); a
  failing **applied** step aborts the conversion (`RecipeError::StepFailed`).
- Events (07 §4): `RecipeResolved { source: auto|slug|file, installer_slug,
  game_slug }`, `RecipeSkipped { step, reason }`, `RecipeApplied { step }`,
  `RecipeMiss { key }` — added to the mac-app stream, no new channel.

## 6. Errors & exit codes (03 §5 unchanged)

Consumption failures inside the mac-app pipeline → `MacAppError::Recipe` →
**exit 7**; no new exit code.

| Variant | When |
|---|---|
| `Recipe::StepFailed { step, msg }` | an applied step failed at runtime |
| `Recipe::PathEscape { path }` | post-substitution path outside the bundle (pinned mode) |
| `Recipe::Unresolvable { msg }` | `--recipe <slug>` could not be fetched/parsed |

Auto-mode discovery/selection/fetch trouble is **never** fatal: `RecipeMiss`
+ warn, pipeline continues recipe-less (today's behaviour).

## 7. Security model

- **Consumer, not executor.** The interpreter implements exactly the §3.1
  mapped rows; there is no shell, no process spawn besides the runtime's own
  `reg.exe`, and no recipe-driven network fetch.
- **Bundle-scoped filesystem.** Mapped file steps are canonicalised against
  `$BUNDLE`; `..` and symlink escapes skip the step (fatal when pinned).
- **Registry writes are prefix-local** by mechanism (`WINEPREFIX=$PREFIX`);
  the host registry is never touched.
- Content provenance: installer metadata (`game`, `version`, `user`,
  `updated_at`, `rating`) is carried in `RecipeResolved` and printed by
  `gogo recipes show`, so the human sees whose script is being applied.
- Upstream trust: scripts stay on lutris.net; gogo pins nothing and republishes
  nothing.

## 8. Testing (08 conventions)

- **Fixtures**: recorded `/api/installers/<slug>?format=json` payloads as
  files — first: `pharaoh-cd-cnc-ddraw` (ddraw override + file ops), plus a
  synthetic script exercising wineexec-args harvesting, `$USER`/`$GAMEDIR`
  substitution, and every skip class.
- **Unit** (lutris.rs/interpret.rs): discovery-match matrix (goggame id vs
  library id vs alias); selection rules; substitution; scope validation incl.
  `..` and symlink escape; skip classification for every unmapped step type.
- **Integration** (`tests/macapp_pipeline.rs`, fake wine runtime):
  - `wine.overrides` → argv log contains
    `reg.exe add "HKEY_CURRENT_USER\Software\Wine\DllOverrides" /v ddraw /d native,builtin /f`
    with `WINEPREFIX` set;
  - wineexec-harvested `/NOGUI` → installer argv gains the flag;
  - `merge`/`write_file` steps → layout assertions on the fake prefix;
  - `game.exe` overrides the scanned entry point; `system.env` lands in
    `launch.conf`;
  - `--recipe auto` with the lutris client stubbed to 404 → `RecipeMiss`,
    sequence identical to the no-recipe baseline (existing tests keep
    passing);
  - pinned `--recipe <slug>` with unreachable network → exit 7
    (`Unresolvable`), JSON error carries the slug.
- **HTTP layer** behind a `LutrisClient` trait; tests use in-memory fixtures;
  no network in CI.

## 9. gogo-core layout (01 §1 tree gains)

```
├── macapp/
│   ├── lutris.rs         # LutrisClient trait: search, game page, installer fetch; cache
│   ├── interpret.rs      # script JSON → RecipePlan (mapping table §3.1); skip taxonomy
│   └── (design 10 modules, unchanged shape)
```

CLI: `MacApp` variant gains `--recipe`; new `Recipes` subcommand (`show`).
No new dependencies — `script` is JSON, parsed with the existing serde_json.

## 10. Future (explicitly out of scope here)

- `files:` download steps with pinned URL + sha256 + license field (Bottles
  pattern, docs/14 §2.3) — unlocks `extract`/redistributable scripts.
- `winetricks:` bridging — declare verbs, execute via a user-installed
  winetricks binary when present (opt-in; gogo never ships it).
- `execute` steps behind an explicit `--allow-recipe-exec` (likely never:
  Lutris scripts are shell by construction; the declarative subset should
  keep covering the real-world corpus).
- Recipe coverage for plain `gogo install` (Linux prefix flows).
