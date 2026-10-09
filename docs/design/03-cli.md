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
state.json + catalog cache.

### `gogo whoami`
Username, user id, country, wallet balance, counts. `--json`.

### `gogo list`
Owned games table: `ID  TITLE  OS  INSTALLERS  SIZE  INSTALLED(✓ version | –)`.
Sorted by title. Sources: owned ids (always live, 1 call) + catalog cache.
`--search <text>` title substring · `--os <windows|mac|linux>` filter · `--with-dlcs` include
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
The main event. Selector `--all` means every owned game (with `--yes` or a confirmation
prompt listing totals).

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
| `--no-disk-check` | off | skip free-space preflight |

Game selectors accept **id**, **slug**, or case-insensitive title substring. Ambiguous →
interactive pick (dialoguer) or, non-TTY, exit 2 with the candidate list. Unknown → exit 2.

Exit summary (human): per-file lines `✓ name (size, elapsed)` / `↷ skipped (already present)` /
`! failed (reason)` + totals. `--json` adds a machine summary with the same facts.

### `gogo update [<game>…] | --all`
Detect outdated downloads (06 §4: catalog `version` vs record; `--strict` additionally
compares checksum-XML build id + timestamp) and run the download pipeline for exactly those
files. With no args: interactive picker of outdated games (non-TTY: all outdated).

### `gogo verify [<game>…] | --all | --path <file>…`
Recompute per-chunk MD5 of downloaded files against their checksum docs (cached in state;
re-resolved if missing). Read-only, no rate limit. Prints ok/corrupt per file; exit 5 if any
corrupt. `--fix` re-downloads corrupt files (aliases into the download pipeline).

### `gogo config <subcommand>`
`path` — print the config file location (creating nothing).
`get <key>` / `set <key> <value>` — read/write the TOML file (set creates dirs, keeps
comments via toml_edit-style round-trip if the `toml_edit` crate is pulled in, else rewrites).
`edit` — open `$EDITOR`.

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
- `download --all` without `--yes` shows the plan table + total size and asks for confirmation.
- Disk full, checksum mismatch, auth failure: fail fast with a specific exit code; other files
  in the batch continue (failures collected, summary at the end).

## 4. Output streams

| Stream | Contents |
|---|---|
| stdout | tables (list/info), `--json` results, `--json-progress` NDJSON |
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
| 130 | interrupted (Ctrl-C), state flushed, resumable |

Batch semantics: if any file fails with code-class X and none with a higher class, exit with
the highest failed class (e.g. one corrupt file → 5 even if others succeeded).

## 6. Examples

```
$ gogo login            # browser opens, paste redirect URL back
$ gogo list --search brass
  ID          TITLE     OS                 SIZE     INSTALLED
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
