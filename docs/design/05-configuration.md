# 05 — Configuration

Layering is explicit and hand-rolled (no figment/config crate): three merge steps over one
serde struct, fully unit-testable.

## 1. Precedence

```
built-in defaults  <  config file (TOML)  <  environment (GOGO_*)  <  CLI flags
```

Merge is **per key**: a layer only overrides keys it explicitly sets. Unknown keys in the TOML
file → warning (stderr), ignored — never a hard error (forward compatibility).

## 2. File location

`--config PATH` / `GOGO_CONFIG` wins; else `<config_dir>/gogo/config.toml`. Base dirs follow
the platform standard (normative table in 06 §1): XDG base dirs on Linux (`$XDG_CONFIG_HOME` →
`~/.config`), `~/Library/Application Support` on macOS. `GOGO_HOME` collapses config/state/cache
into one flat dir (06 §1). A missing file is not an error (`gogo config path` prints where it
would be; `gogo config edit` creates it).

## 3. Schema (defaults shown)

```toml
[download]
dir = "<download_dir>/gogo"   # dirs::download_dir(); fallback ~/Downloads/gogo → ~/gogo
workers = 3                   # parallel chunk connections per file (1..=16)
parallel_files = 1            # simultaneous files (1..=4)
limit = 0                     # bytes/sec; 0 = unlimited (see grammar below)

[auth]
token_store = "keyring"       # "keyring" | "file" — where tokens live; no silent fallback (02 §3, review A6)
# client_id / client_secret: optional overrides of the Galaxy public client — public
# values, not secrets (review C3); unset = built-in defaults (02 §1)

[gog]
language = "en"               # preferred installer language (fallback chain: 04 §1)
platform = "current"          # "current" | "windows" | "mac" | "linux" | "all"
locale = "en-US"              # products API locale (localized titles)

[network]
timeout_secs = 30             # per-request timeout (API JSON calls)
idle_timeout_secs = 30        # no-progress window on CDN streams
retries = 5                   # max attempts per retryable request (07 §3)
api_parallel = 4              # concurrent catalog/downlink/checksum fetches

[state]
catalog_ttl_hours = 24        # catalog cache freshness (06 §3)
```

### Rate value grammar (`--limit`, `download.limit`)

```
<number>[.<number>]<unit>[/s]      unit ∈ B | KiB | MiB | GiB (binary, case-insensitive)
```

Examples: `--limit 4MiB/s`, `--limit 500kib`, `--limit 10485760`. Bare numbers are bytes/sec.
Rejected: sub-32 KiB/s values (pointless, invites stalls) with a clear usage error (exit 2).
Internally: `u64` bytes/sec. At the floor the limiter splits read buffers so a single ask
never exceeds burst capacity (04 §4, review A4).

## 4. Env overlay

Rule: **flattened** `GOGO_<NAME>` (one prefix, no mechanical section join — review C2; this
table is normative). Same parse rules as TOML values:

| Env | Key |
|---|---|
| `GOGO_DOWNLOAD_DIR` | `download.dir` |
| `GOGO_WORKERS` | `download.workers` |
| `GOGO_LIMIT` | `download.limit` |
| `GOGO_PARALLEL_FILES` | `download.parallel_files` |
| `GOGO_TOKEN_STORE` | `auth.token_store` |
| `GOGO_LANGUAGE` / `GOGO_PLATFORM` / `GOGO_LOCALE` | `gog.*` |
| `GOGO_RETRIES` | `network.retries` |
| `GOGO_CONFIG` | config file path (meta, see §2) |
| `GOGO_HOME` | base-dir override: config/state/cache collapse into `$GOGO_HOME` (06 §1) |

Env values are only applied when the variable is set and parses; a bad value → exit 2 with the
offending name (env mistakes should be loud, not silently ignored).

## 5. CLI overlay

`cli.rs` holds `Option<T>` mirrors of override-able keys (`--dir`, `--workers`,
`--parallel-files`, `--limit`, `--language`, `--os` → overlays `gog.platform`, `--retries`);
`Some(_)` wins over every other layer. This is the whole merge surface — no hidden
interactions.

## 6. Validation & path handling

- Range checks after merge: `workers 1..=16`, `parallel_files 1..=4`, `retries 0..=20`,
  `api_parallel 1..=16`, rate grammar above. Violations → exit 2 naming the key and layer.
- `~` expansion + `dirs` resolution on `download.dir` at load time; the resolved absolute path
  is what the engine sees (`gogo config get download.dir` shows the resolved value).
- Secret-bearing keys do not exist in config (tokens live in the TokenStore, 02 §3) — the file
  is safe to commit/share.

## 7. Precedence example

`download.dir` resolution order: CLI `--dir /tmp/x` → env `GOGO_DOWNLOAD_DIR` → TOML
`[download] dir = "…"` → default `<Downloads>/gogo`. Each layer present wins only over lower
layers, per key; e.g. env sets `GOGO_WORKERS=8` while CLI passes `--limit 1MiB/s` → workers 8,
limit 1 MiB/s.
