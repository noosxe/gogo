# 14 — Recipe Sources (per-game community knowledge for making games run)

**Research date:** 2026-10-19 (Lutris API live-probed same day)
**Method:** Primary documentation and source review of the established per-game
configuration systems (Lutris installer scripts, Winetricks verbs, Bottles
dependency manifests, PlayOnLinux/Phoenicis scripts, Porting Kit wrappers,
CrossOver CrossTies) **plus live probes of the lutris.net REST API**. Follow-up
to docs/13 (macOS app bundles): `gogo mac-app` works end-to-end for
well-behaved titles (live-verified with Pharaoh Gold, 2026-10-18); this doc
establishes that gogo should **consume the existing Lutris installer corpus**
rather than invent a recipe format of its own.

## 1. The problem: installers succeed, games still fail

A silent Inno Setup install plus a Wine prefix gets the files on disk. What it
does not get right, for a large fraction of the Windows catalogue:

| Failure class | Typical fix | Example |
|---|---|---|
| Old DirectDraw/renderer engines | `ddraw.dll` override (`native,builtin`) or a wrapper like cnc-ddraw | 1997–2003 isometric titles |
| Installer dies silently under `/VERYSILENT` | undocumented GOG flag `/NOGUI`, use `/SILENT` | Cuphead, SW: Battlefront II (encoded in Lutris GOG scripts) |
| Wrong default resolution / windowed | registry values under the game's `HKCU` key | Impressions games (Pharaoh/Caesar), LucasArts |
| Needs a newer C runtime / d3dcompiler | `vcrun2019`, `d3dx9` redistribute | post-2015 titles |
| Launcher stub exits instead of game | launch the real exe with args | GOG Galaxy-dependent launchers |
| ESYNC/Msync, GPU quirks | env vars (`WINEESYNC`, `MESA_*`, `__GL_*`) | varied |

The community has already encoded most of this knowledge — per game — in
Lutris installer scripts. gogo should read it, not rebuild it.

## 2. Systems survey

### 2.1 Lutris installer scripts (the corpus to consume)

Community-contributed YAML scripts (`docs/installers.rst` in the Lutris repo):

- **Sections:** `game` (exe, args, prefix, arch), `files` (download /
  user-supplied inputs), `installer` (ordered steps), `wine` (prefix options,
  `overrides`), `system` (env), `variables` (custom substitutions).
- **Step vocabulary:** `task` (runner tasks: `wineexec`, `create_prefix`,
  `winetricks`, …), `execute`, `extract`, `merge`/`copy`, `move`,
  `write_file`, `write_config`, `chmodx`, … — plus arbitrary
  `execute: command:` shell.
- **Variables:** `$GAMEDIR`, `$CACHE`, `$USER`, `$RESOLUTION[_WIDTH|_HEIGHT]`,
  `$WINEBIN`, `$INPUT_*`, `$SCRIPTDIR`.
- **Distribution:** lutris.net API + website; thousands of GOG games have
  scripts driving GOG's own offline installers, including the `/NOGUI`-class
  flag lore.

#### 2.1.1 Live API verification (2026-10-19)

| Probe | Result |
|---|---|
| `GET /api/games?search=pharaoh&with-installers=True` | Filtered results (client's own discovery call, `lutris/api.py::search_games`); records carry `provider_games[]` with `{"service": "gog", "slug": "<GOG product id>"}` — Pharaoh → `1207659046` |
| `GET /api/installers/<installer-slug>` | Full record: `script` as a **JSON object** (not YAML string), plus `runner`, `version`, `gogid`, `gogslug`, `published`, `updated_at` — verified with `quake-darkplaces` |
| Game page `lutris.net/games/<slug>` | Lists community installers with **`Download script: /api/installers/<installer-slug>?format=json`** links; Pharaoh has exactly one: `pharaoh-cd-cnc-ddraw` (Wine + cnc-ddraw — the ddraw-override failure class of §1). Also a `lutris:gog:<gogid>` auto-GOG entry, which is Lutris' own gogdl flow — gogo already does that natively |
| `GET /api/installers?game=…` / `?game_slug=…` / `?gogid=…` | **Filters ignored** — returns the whole corpus (15592 records). No server-side game→installers filter exists |
| `GET /api/installers/game/<slug>` (client `INSTALLER_URL % game_slug`) | HTTP 200 with `count: 0` for game slugs (`quake`, `pharaoh`) — resolves installer slugs only (`quake-darkplaces` → 1) |
| `GET /api/installers/cuphead-gog` | The corpus encodes exactly the per-game knowledge gogo needs: a `task: wineexec` step carrying the GOG installer flags (`/SP- /NOCANCEL /SUPPRESSMSGBOXES /NOGUI`) **and** a `task: winetricks` step with `app: corefonts` — winetricks requirements are first-class script steps, GOG-id-keyed (`gogid: 1963513391`) |

**Consequence for gogo:** discovery = name search + `provider_games` GOG-id
match; enumeration = the game page's `Download script` links (stable,
machine-parseable hrefs); fetch = `GET /api/installers/<installer-slug>`.

### 2.2 Winetricks

The canonical per-game fix database; each verb is a **shell function** —
arbitrary code by construction. Do not adopt the format. Do adopt the
bridge: Lutris scripts invoke winetricks per game (`task: winetricks` with
verbs such as `corefonts` — live-verified on cuphead-gog), so gogo executes
those verbs through a **user-installed** winetricks binary (brew install
winetricks), one verb per call with the prefix staged. gogo never ships or
downloads winetricks itself (docs/13 §3); when the binary is absent the
step is skipped with a loud, named requirement.

### 2.3 PCGamingWiki (demoted: not machine-readable)

PCGamingWiki documents per-game fixes richly, but its API rejects arbitrary
Cargo queries (`permissiondenied`, live-verified 2026-10-19) — fixes live in
prose. Supplementary human reference, not a gogo source.

### 2.4 Porting Kit (demoted: no public API)

PK's useful per-game knowledge (winetricks list, Windows-version override,
engine choice) is locked inside downloaded wrappers on the user's disk
(`winetricks.log`, `Info.plist`) — readable only by cracking one open; there
is no API. PK validated that **runtime compatibility is recipe data**
(Pharaoh: Wine-CX works, GPTK 1.1 crashes the game) but the knowledge source
of record stays Lutris.

### 2.5 Bottles dependency manifests

`bottlesdevs/dependencies`: per-component JSON manifests, pinned URL +
checksum, interpreted client-side. **Relevant later**: when gogo grows
redistributable steps (`vcrun2019` etc.), this is the pattern to copy.

### 2.6 PlayOnLinux / Phoenicis, CrossOver

Repo-of-per-app-files shape with an arbitrary-code script engine (POL/Phoenicis),
proprietary XML recipes (CrossOver CrossTies). Nothing to adopt; they validate
the per-game-recipe premise.

## 3. Decision: consume Lutris, interpret a declarative subset

| Aspect | Choice | Source |
|---|---|---|
| Source | **lutris.net installer scripts** — no new format, no gogo recipe repo | §2.1 |
| Key | GOG product id (from `goggame-<id>.info` and the library record) matched against `provider_games[service=gog].slug`; installer records also carry `gogid`/`gogslug` for cross-checking | §2.1.1 |
| Selection | **live search during `mac-app` + the user picks** the script to apply; `--recipe <slug>` for scripting; nothing stored between runs | Lutris parity | 
| Interpretation | **Parity executor** for what wine-game scripts use (design 11 §3.1): `files:` downloads via the gogo downloader, `extract` (zip/tar), `wineexec` through the resolved runtime, `task: winetricks` bridged to a user-installed binary, `merge`/`move`/`write_file`/`write_config`/`chmodx`, `wine.overrides` → registry, `game`/`system.env` → launch. Skipped with loud warnings: host-shell `execute` (opt-in v1.1), `insert_disc`, unknown tasks | Lutris parity |
| Parsing | `script` arrives as JSON — serde_json only; no YAML dependency. `--recipe <file>` accepts the same `?format=json` download | §2.1.1 |
| Storage | **none** — per-run temp working set, deleted when the conversion ends; re-running re-searches and re-asks | ephemeral client |
| Trust | The **user is the trust anchor** (they pick the script; the plan is shown before execution); host-shell steps skipped by default; paths must stay inside the bundle | §4 |
| Binary downloads | recipe-chosen `files:` downloads are v1 (the corpus's patch/wrapper URLs — how Pharaoh's exe swap would apply), fetched per-run into the temp working set; redistributables beyond winetricks stay v2 (Bottles-style pinned URL + sha256) | §2.5 |

Full contract: **design 11** (`docs/design/11-recipes.md`).

## 4. Trust and licensing notes

- gogo is a **consumer**, not a publisher: recipe content stays on lutris.net
  (CC-BY-type community content, upstream's problem to review).
- The interpreter executes **nothing arbitrary**: no shell, no downloads, no
  unknown task types. A Lutris script can at most: set registry keys inside
  the staged prefix, rearrange files inside the produced bundle, and adjust
  the launch line. Anything else is a warning event.
- gogo ships no binaries and performs no recipe-driven network fetches —
  the docs/13 §3 licensing matrix is unaffected.
- Windows-redistributable support (`vcrun2019`, `d3dx9`) remains the known
  v2 gap (design 11 §9).

## 5. Open questions carried into design 11

1. Game→installer-slug enumeration has no server-side filter (§2.1.1) →
   game-page `Download script` hrefs are the enumeration surface; `--recipe
   <installer-slug>` and `--recipe <file>` are the pinned/manual paths.
2. Multiple installers per game (Wine CD vs GOG vs cnc-ddraw variants) →
   prefer scripts whose `files:` reference a GOG installer, then newest
   `updated_at`; user pins with `--recipe <slug>` when in doubt (11 §2).
3. `$USER` / prefix-username mapping for Lutris paths (`drive_c/users/$USER`)
   → resolve against the actual staged prefix layout (11 §3).
4. win32-prefix scripts → warn: gogo stages win64, and the GPTK runtime is
   wine64-only anyway (11 §3).
