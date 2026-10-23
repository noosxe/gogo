# 03 — CLI

`gogo` — a single binary. Global flags apply to all subcommands. Automation rules: **results
on stdout, human progress on stderr, stable exit codes, no prompts unless unavoidable.**

## 1. Commands

### `gogo login`
Interactive browser paste-code flow (02 §2).
`--code <code>` non-interactive · `--print-url` only print URL · `--force` re-login over
existing session.

### `gogo logout`
Delete stored tokens. `--all-stores` clear keyring + file · `--purge-state` also remove
state.json + catalog cache + verify-cache (06 §3).

### `gogo whoami`
Username, user id, country, wallet balance, counts. `--json`.

### `gogo list`
Owned games table: `ID  TITLE  OS  INSTALLERS  SIZE  DOWNLOADED(✓ version | –)`.
Sorted by title. Sources: library list (`getFilteredProducts` `mediaType=1`, always live,
paged — **games only**) + catalog cache (sizes, versions). DLCs are part of their game:
indented rows via `--with-dlcs`, never library entries. `--refresh` also busts the product
cache.
`--search <text>` title substring · `--os <windows|mac|linux|all>` filter · `--with-dlcs` include
DLC rows (indented) · `--refresh` bypass catalog cache · `--json`.

`--json` shape:
```json
[{ "id": 1075439258, "title": "Hot Brass", "slug": "hot_brass",
   "os": ["windows", "mac", "linux"], "installers": 3,
   "total_size": 724243232, "installed_version": "2.2.2" }]
```

### `gogo info <game>`
Full catalog entry: base facts (title, developer, platforms, release date), every download
entry (kind, os, language, version, size, files count), DLC tree, extras list with sizes.
`--json` dumps the raw product JSON (as stored in cache, minus nothing — it has no secrets).

### `gogo download <game>… | --all`
The main event. Selector `--all` means every game in the library list (games only; DLCs
enter exclusively via `--with-dlcs` from their parent game) with `--yes` or a confirmation
prompt listing totals.

Hidden games are excluded: the default `getFilteredProducts` query returns only visible
products (live-checked 2026-10-09 — six candidate surfacing params were all no-ops), so
the listing matches the GOG web default view. Surfacing them is v2 (09 §3).

Content selection flags (all combinable):

| Flag | Default | Meaning |
|---|---|---|
| `--os <os>` | host OS, else `windows` ↓ | `windows`/`mac`/`linux`/`all` |
| `--language <code>` | `en`, else first available | installer language (`en`, `de`, `fr`, …) |
| `--with-dlcs` | off | include DLC installers |
| `--include <kinds>` | off | repeatable: `patches`, `extras`, `lang-packs` |
| `--only <kinds>` | off | exclusive selection, replaces default (installers) |

↓ Default `--os` = host OS (`linux` on Linux, `mac` on macOS); a game with no host-OS
installers falls back to its `windows` entries with a printed notice (GOG ships few Linux
builds — the same reason lgogdownloader defaults to linux+windows). Windows installers on
Linux are a first-class use case, not a corner case: Lutris/Bottles/Steam (Wine/Proton)
consume exactly these files. gogo stores them and stops there — executing installers is the
user's layer and stays a non-goal for the tool itself.

Behavior flags:

| Flag | Default | Meaning |
|---|---|---|
| `--dir <path>` | `download.dir` | destination root for this run |
| `--dry-run` | off | print the plan (files, sizes, destinations), touch nothing |
| `--force` | off | redownload even if a matching verified file exists |
| `--yes` / `-y` | off | skip confirmation prompts |
| `--limit <rate>` | `download.limit` | per-run rate override (05 §3 grammar) |
| `--workers <n>` | `download.workers` | per-run parallelism override |
| `--parallel-files <n>` | `download.parallel_files` | simultaneous files (1..=4) |
| `--no-disk-check` | off | skip free-space preflight |

Game selectors accept **id**, **slug**, or case-insensitive title substring. Ambiguous →
interactive pick (dialoguer) or, non-TTY, exit 2 with the candidate list. Unknown → exit 2.
A DLC id/slug as a selector resolves to its **parent game** (with a notice); DLC content
itself is selected via `--with-dlcs` (A1).

Exit summary (human): per-file lines `✓ name (size, elapsed)` / `↷ skipped (already present)` /
`! failed (reason)` + totals. `--json` adds a machine summary with the same facts.

### `gogo update [<game>…] | --all`
Detect outdated downloads (06 §4: catalog `version` vs record; `--strict` additionally
compares checksum-XML build id + timestamp) and run the download pipeline for exactly those
files. With no args: interactive picker of outdated games (non-TTY: all outdated).

### `gogo verify [<game>…] | --all | --path <file>… | --rebuild`
Recompute per-chunk MD5 of downloaded files against their checksum docs (cached in state;
re-resolved if missing). Read-only, no rate limit. Prints ok/corrupt per file; exit 5 if any
corrupt. `--fix` re-downloads corrupt files (aliases into the download pipeline).

`--rebuild` walks the download dir and adopts verified files into records (fresh state or a
new machine with copied files — 06 §5).

### `gogo config <subcommand>`
`path` — print the config file location (creating nothing).
`get <key>` / `set <key> <value>` — read/write the TOML file (set creates dirs, keeps
comments preserved via a `toml_edit` round-trip — decided, review D7).
`edit` — open `$EDITOR`.

### `gogo completions <shell>`
Print the completion script to stdout — `bash` · `zsh` · `fish` (clap_complete; also
`elvish` · `powershell`). Pure output: no config read, no session, nothing on stderr, so
the script pipes cleanly into an install. Install paths (bash): file-based
(`gogo completions bash > ~/.local/share/bash-completion/completions/gogo`) or
`eval "$(gogo completions bash)"` in `~/.bashrc`; zsh: dump to a `fpath` dir as `_gogo`
(`compinit` picks it up); fish: `gogo completions fish > ~/.config/fish/completions/gogo.fish`.
Known quirk: macOS system bash 3.2 can garble `source <(gogo completions bash)` FIFO
sourcing for any multi-write producer — use the file or eval path there.

### `gogo mac-app <GAME|installer-path>`
> **Amended 2026-10-23 (#73):** a game selector builds from the game's depot tree
> (design 15), falling back to its downloaded installer when it has no depot build;
> an installer path keeps the lane below. Lane rules: design 15 §1.

Convert a downloaded Windows offline installer into a self-contained macOS
`.app` (double-click launch, drag to Applications). Silent Inno Setup install
into a bundled WINEPREFIX; Wine runtime resolved per design 10 §3 + design 12
(`auto` / `brew` / `gptk` / `path` / catalog ids `staging`·`devel`·`stable` with
download + cache), copied into the bundle; ad-hoc codesigned. macOS only (exit 2
elsewhere). Full specs: **design 10**, **design 12**; discovery: docs/13, docs/15.
Exit 7 on conversion failures. Names/flags: `--name`, `--runtime`,
`--runtime-version`, `--runtime-path`, `--out`, `--recipe`, `--interactive` (#76: recipe and runtime menus are opt-in), `--json-progress`,
`--json`, `--yes`. Per-game fixes
(registry tweaks, extra installer flags, entry-point override) come from
community recipes — design 11.

### `gogo runtimes <list|install|prune>`
Manage the Wine runtime store (design 12 §4): `list` (cached + catalog, `--json`),
`install <id> [--version]` (download → probe → cache), `prune [--all | <id>]`.
Catalog entries are live-probed (docs/15); GPTK installs require explicit consent
(Apple EULA, design 12 D9).

### `gogo recipes <list|show>`
Inspect the Lutris corpus without converting anything (design 11 §1).
`list <installer-path>` (or `--name <game>`) searches lutris.net for the
game and prints its ranked `runner: wine` installers (slug, version, author,
updated, wine pin); `show <installer-slug>` live-fetches that script (or
reads a local script JSON file) and prints the execution plan: applied steps
vs skipped-with-reasons, harvested setup flags, the launch override, and the
wine pin's runtime recommendation (design 12 §7). `--json` on both; a hidden
`--lutris-base` exists for tests/mirrors. gogo consumes Lutris scripts — it
hosts no recipes of its own. Full spec: **design 11**.

### `gogo __pe-scan <DIR> [--exe EXE] [--json]` (hidden, debug)
PE import scan of a game tree (#63, docs/19 §6.2): main exe and its bitness,
detected engine, graphics API classification, helper DLLs (`d3dcompiler_*`,
`d3dx*`, `xinput*`, `xact*`) and runtime-loaded DLL names. Read-only; the
input for L1 renderer selection (design 16).


## 2. Global flags

| Flag | Env | Meaning |
|---|---|---|
| `-C, --config <path>` | `GOGO_CONFIG` | config file location (05 §2) |
| `-v / -vv / -vvv` | — | info / debug / trace logging (stderr) |
| `--log-file <path>` | — | full-trace log to file |
| `--json` | — | machine output for results (stdout) |
| `--json-progress` | — | NDJSON progress events on stdout (implies `--json`) |
| `--no-progress` | — | no indicatif bars (auto when !stderr.is_terminal) |
| `--quiet` | — | errors only |
| `--color <when>` | — | auto/always/never |

## 3. Interaction rules

- Never prompt when stdin/stdout is not a TTY (except `login`, which is inherently
  interactive and says so).
- `download --all` without `--yes` shows the plan table + total size and asks for
  confirmation (TTY). Non-TTY without `--yes`: print the plan and abort, exit 2 — a bulk
  run never starts silently (review B1).
- Disk full, checksum mismatch, auth failure: fail fast with a specific exit code; other files
  in the batch continue (failures collected, summary at the end).

## 4. Output streams

| Stream | Contents |
|---|---|
| stdout | tables (list/info), `--json` results, `--json-progress` NDJSON |

`--json-progress` emits one externally-tagged JSON object per event per line
(e.g. `{"ChunkDone":{…}}`), flushed per line; `PlanReady` fires during planning
(so `--dry-run` still yields one event) and `Done` is terminal. The same stream
feeds the v2 TUI (`docs/design/01` §4).
| stderr | progress bars, warnings, log output, prompts |

## 5. Exit codes

| Code | Meaning |
|---|---|
| 0 | success (including "nothing to do") |
| 1 | unexpected/runtime error (bug or I/O) |
| 2 | usage error: unknown game, ambiguous selector, bad flag value |
| 3 | auth: not logged in, login failed/aborted, unauthorized after refresh |
| 4 | network: retries exhausted on API/CDN |
| 5 | verification failure: checksum mismatch (download or `verify`) |
| 6 | disk: insufficient space or unwritable destination |
| 7 | mac-app conversion failure: runtime missing/invalid, installer exit ≠ 0, no entry point, codesign failure, recipe error (design 10 §4, design 11 §5) |
| 130 | interrupted (Ctrl-C), state flushed, resumable |

Batch semantics: if any file fails with code-class X and none with a higher class, exit with
the highest failed class (e.g. one corrupt file → 5 even if others succeeded). Numeric order
is **outcome severity** (auth 3 < network 4 < data-integrity 5 < disk 6 — review C1): a
batch reports the worst thing that happened, so automation always sees it.

## 6. Examples

```
$ gogo login            # browser opens, paste redirect URL back
$ gogo list --search brass
  ID          TITLE     OS                 SIZE     DOWNLOADED
  1075439258  Hot Brass win mac linux      691 MiB  ✓ 2.2.2

$ gogo download Hot Brass --with-dlcs --limit 4MiB/s --dry-run
  plan: 2 files, 1.1 GiB → ~/Downloads/gogo/Hot Brass/
    installer windows en 2.2.2  setup_hot_brass_2.2.2_(85647).exe   691 MiB
    dlc       windows en 1.0    (soundtrack archive, name from server)  212 MiB

$ gogo download 1075439258 -y
  ⠴ setup_hot_brass_2.2.2_(85647).exe  512/691 MiB @ 24 MiB/s  eta 00:12
  ✓ 2 files, 1.1 GiB in 47s

$ gogo update --all --strict
$ gogo verify --all
```
