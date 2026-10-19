# 11 — Recipes (community per-game knowledge for `gogo mac-app`)

**Status:** proposed contract (design phase; no implementation yet)
**Depends on:** design 10 (`gogo mac-app`), 07 (events/errors), 06 (paths)
**Research:** docs/14 (Lutris / Winetricks / Bottles survey, 2026-10-19)

`gogo mac-app` converts well-behaved installers out of the box (live:
Pharaoh Gold, 2026-10-18). Real catalogues need per-game fixes — DLL
overrides, registry tweaks, extra installer flags, entry-point corrections.
A **recipe** is one declarative YAML file per GOG game id, contributed to a
community repository and interpreted by gogo through a **closed verb set** —
recipes are data, never code.

## 1. CLI surface (03 §1 carries the user-facing summary)

```
gogo mac-app <installer-path> [--recipe auto|none|<name>|<path>]   # default: auto
gogo recipes list                       # what is in the cache (+ overrides)
gogo recipes show <game-id|slug|name>   # rendered recipe: metadata, notes, steps
gogo recipes update                     # force re-fetch of the recipe repo
```

- `auto` (default): look up the game id from the installer's
  `goggame-<id>.info` (already parsed in design 10 §2) → local override dir →
  cache. No recipe found is **not an error**: emit `RecipeMiss` and proceed
  recipe-less (today's behaviour).
- `none`: disable lookup entirely.
- `<path>`: use this file (authoring/debug); schema errors are fatal here.
- `<name>`: cache file by name (e.g. `--recipe 1207658924`).
- `gogo recipes *` works offline against the cache; only `update` hits the
  network.

## 2. Schema — `gogo-recipe/1`

One file per game, named `<game-id>.yaml`:

```yaml
schema: gogo-recipe/1
game_id: "1207658924"        # must match the filename and goggame-<id>.info
slug: pharaoh-gold           # informational; second lookup key
title: "Pharaoh + Cleopatra"
revision: 3                  # author bumps on meaningful changes
notes: |
  Human-facing markdown: what the recipe fixes, tested-against runtime,
  known quirks. Rendered by `gogo recipes show`.

runtime:                     # advisory only — gogo never downloads runtimes
  min_wine: "7.7"            # informational probe comparison
  dx_backend: wined3d        # wined3d | dxvk | gptk — future launch knob

install:
  installer_args: ["/NOGUI"] # appended to the silent Inno flag set (10 §2)

steps:                       # ordered, after installer, before entry-point scan
  - registry:
      key: 'HKEY_CURRENT_USER\Software\Wine\DllOverrides'
      values: { ddraw: "native,builtin" }
  - copy:
      from: '$INSTALL/Data/Pharaoh_fixed.exe'
      to:   '$INSTALL/Pharaoh.exe'
  - delete: ['$INSTALL/*.lnk']

launch:
  exe: 'C:\GOG\Pharaoh\Pharaoh.exe'  # overrides goggame entry point
  args: ['-nores']
  env: { WINEESYNC: "1" }            # merged into launch.conf (10 §2)
```

Field rules:

- Unknown **top-level** fields are a schema error (typos must not silently
  no-op). Unknown fields inside advisory sections (`runtime`, `notes`) warn.
- `schema` must be exactly `gogo-recipe/1`; anything else is a schema error.
- `game_id` is a string of digits (GOG ids are u64; YAML would coerce —
  quoting is convention, both accepted, normalised to digits).

### 2.1 Step vocabulary (closed; v1)

| Verb | Fields | Mechanics |
|---|---|---|
| `registry` | `key`, `values: {name: value}` | one `wine reg.exe add "<key>" /v <name> /t REG_SZ /d <value> /f` per value, through the resolved runtime (10 §3) — no shell, argv logged, fake-runtime testable |
| `copy` | `from`, `to` | `std::fs::copy` after substitution + scope validation |
| `move` | `from`, `to` | `std::fs::rename`, fallback copy+remove |
| `delete` | list of globs | single-`*` glob, resolved per match, `remove_file`/`remove_dir_all` |
| `mkdir` | list of paths | `create_dir_all` |
| `env` / `args` / `exe` | map / list / path | recorded into `launch.conf` and the launcher stub argv |
| `installer_args` | list | appended to the Inno silent flag list of the install step |

No `exec`/`run` verb in v1 (docs/14 §4); no network-fetching step
(redistributables are the designated v2 addition, Bottles-style pinned URLs +
sha256). A recipe that asks for either fails validation at parse time.

### 2.2 Variables

| Var | Meaning |
|---|---|
| `$PREFIX` | the staged WINEPREFIX (`…/Pharaoh.app/Contents/Resources/wine`) |
| `$INSTALL` | game install dir inside the prefix (`…/wine/drive_c/GOG/<slug>`) |
| `$BUNDLE` | the `.app` root |
| `$SLUG` | game slug |

Substitution applies to every string in steps/launch/install. After
substitution, every path a verb will touch is canonicalised and must resolve
**inside `$BUNDLE`** — anything else is `RecipeError::PathEscape` (§6).
Registry keys/values are the only unscoped strings (they live inside the
prefix's registry by construction).

## 3. Distribution & cache (no git dependency)

Repo: `noosxe/gogo-recipes`, branch `main`:

```
index.json               # CI-generated: {revision: <git sha>, generated_at, recipes: [{game_id, slug, title, revision}]}
recipes/<game-id>.yaml   # one file per game
```

- Client fetch = plain HTTPS GET of the branch tarball
  (`codeload.github.com/noosxe/gogo-recipes/tar.gz/refs/heads/main`) through
  the existing HTTP stack (07 retry rules) — no libgit2, no API.
- Cache root: `~/Library/Caches/gogo/recipes/` / `$XDG_CACHE_HOME/gogo/recipes/`
  (06 §1). Layout: `<revision>/index.json`, `<revision>/recipes/*.yaml`,
  `current -> <revision>` symlink swapped atomically after a complete,
  schema-validated extract.
- Freshness: if the cached `index.json` is younger than 24 h, `auto` does not
  refresh; `update` always refetches. Fetch/parse failure in `auto` mode
  degrades to "stale cache or `RecipeMiss`" (warn event), **never** a fatal
  error; with an explicit `--recipe <path>` there is no network at all.
- Local override dir: `~/.config/gogo/recipes/*.yaml` (06 §1 config dir) —
  shadow the cache by game id; this is the authoring path (write YAML →
  `gogo mac-app --recipe auto` immediately uses it).
- Config override: `recipes.repo_url` (05 §schema) for forks/self-hosting.

## 4. Pipeline integration (10 §2 annotated)

```
plan → resolve runtime → stage prefix → INSTALL (gogo installer)
     → RECIPE STEPS → entry-point scan → assemble → strip → codesign
        ^ recipe installer_args already folded into INSTALL argv
        ^ launch.conf written during assemble carries recipe env/args/exe
```

- `installer_args` are appended to the silent flag list of the install step.
- Steps run once the installer has finished (files exist) and before the
  entry-point scan, so `launch.exe` overrides what `goggame-*.info` found;
  the scan itself is skipped when `launch.exe` is set.
- Registry steps spawn `wine reg.exe` with the staged prefix env and the same
  timeout/null-stdio discipline as every other runtime invocation (10 §2);
  a failing step aborts the conversion (`RecipeError::StepFailed`).
- Events (07 §4): `RecipeResolved { key, source: override|cache|path, steps }`,
  `RecipeMiss { key }`, `RecipeStep { index, verb }` — added to the
  mac-app event stream, no new channel.

## 5. Errors & exit codes (03 §5 unchanged)

Recipes fail inside the mac-app pipeline → `MacAppError::Recipe` → **exit 7**;
no new exit code. Variants:

| Variant | When |
|---|---|
| `Recipe::Schema { path, msg }` | parse/validate failure (fatal in every mode) |
| `Recipe::StepFailed { index, verb, msg }` | a step's action failed at runtime |
| `Recipe::PathEscape { path }` | post-substitution path resolves outside the bundle |

Auto-mode fetch problems are **not** errors (§3): they surface as
`RecipeMiss` + a warning event and the pipeline continues recipe-less.

## 6. Security model

- **Data, not code.** The interpreter implements exactly the §2.1 verbs;
  there is no escape hatch to a shell in v1 (docs/14 §4).
- **Bundle-scoped filesystem.** Every path verb is canonicalised and checked
  against `$BUNDLE` before touching disk; `..` traversal and symlinked
  escape hatches (a symlink inside the bundle pointing out) are rejected by
  comparing the canonical path.
- **Registry writes are prefix-local** by mechanism: `wine reg.exe` runs with
  `WINEPREFIX=$PREFIX`; the host registry is never touched.
- Repo review is the supply-chain control (PRs, full history, forks); no
  signature infrastructure in v1 — revisit if the repo ever outgrows one
  maintainer org.

## 7. Testing (08 conventions)

- **Unit** (recipe.rs): parse/normalise happy path; schema matrix (bad
  `schema:`, unknown top-level field, non-list `steps`, unknown verb,
  missing `game_id` match); variable substitution; scope validation incl.
  `..` and symlink escape; glob delete matching.
- **Integration** (`tests/macapp_pipeline.rs`, fake wine runtime):
  - `registry` step → fake-runtime argv log contains
    `reg.exe add … /v ddraw /d native,builtin /f` with `WINEPREFIX` set;
  - `installer_args: ["/NOGUI"]` → installer argv gains the flag;
  - `copy`/`delete` steps → layout assertions on the fake prefix;
  - `launch: {args, env}` → `launch.conf` contains them; `exe` overrides the
    scanned entry point in the launcher stub;
  - `--recipe auto` with empty cache → `RecipeMiss` event, sequence identical
    to the no-recipe baseline (existing tests keep passing);
  - explicit `--recipe <bad-path>` and schema-invalid file → exit 7, JSON
    error carries `path`.
- **Fetch** is behind a `RecipeSource` trait; tests use an in-memory source.
  The tarball fetch path itself is covered by a cfg(unix) smoke test against
  a local fixture server only — no network in CI.

## 8. gogo-core layout (01 §1 tree gains)

```
├── macapp/
│   ├── recipe.rs         # schema (serde), validation, substitution, scope checks
│   ├── recipe_fetch.rs   # RecipeSource trait, tarball fetch, cache, overrides
│   └── (design 10 modules, unchanged shape)
```

CLI: `MacApp` variant gains `--recipe`; new `Recipes` subcommand trio
(`list`/`show`/`update`). Dependency: `serde-yaml-ng` (maintained `serde_yaml`
fork, MIT — `serde_yaml` itself is archived; revisit if cargo-deny objects).
`gogo recipes list/show` render from cache + overrides only.

## 9. Future (explicitly out of scope here)

- `winetricks:` bridging — declare verbs, execute via a user-installed
  winetricks binary when present (opt-in; gogo still never ships it).
- Redistributable steps (`vcrun2019`, `d3dx9`) with pinned URL + sha256 +
  license field (Bottles pattern, docs/14 §4).
- `exec` verb behind `--allow-recipe-exec` with hash-pinned scripts.
- Recipe coverage for plain `gogo install` (Linux prefix flows) — same
  schema, new interpreter callsites.
- Recipe-lint CI in the recipes repo (schema validation on PR).
