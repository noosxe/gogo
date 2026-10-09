# 01 — Architecture

## 1. Crate shape

One package (`gogo`), two targets: the **library** is the product, the CLI binary is a thin
frontend. When the v2 TUI arrives it becomes a second bin/crate in a workspace depending on the
same lib — moving to a workspace later is a cheap, mechanical change, so we don't pay for it now.

**Supported platforms: Linux and macOS only.** Windows is explicitly out of scope (README
non-goals): no Windows path handling, and POSIX semantics are assumed throughout —
`write_at`, in-directory `rename` atomicity, `0600` file modes, XDG/`~/Library` dir standards
(06 §1).

```
src/
├── main.rs              # clap parsing, AppContext build, exit-code mapping (no logic)
├── cli.rs               # clap derive types; args → Config overlay (05 §4)
├── lib.rs               # pub mod exports; prelude
├── config.rs            # schema, layering merge, env overlay, validation (doc 05)
├── app.rs               # AppContext { cfg, http, session, state, limiter, events, cancel }
├── auth/
│   ├── mod.rs           # Session: gog-warp Core wrapper; login, restore, bearer()
│   └── store.rs         # TokenStore trait + KeyringStore + FileStore (doc 02 §3)
├── gog/                 # GOG API clients; all serde types live here
│   ├── mod.rs           # http() client factory: UA, timeouts, rustls, no gzip
│   ├── library.rs       # owned ids (embed /user/data/games), getFilteredProducts paging
│   ├── catalog.rs       # products batch ?ids=… (50/call); Product/Downloads types
│   ├── downlink.rs      # files[].downlink → { downlink, checksum }
│   ├── checksum.rs      # quick-xml → ChecksumDoc { name, total_size, chunks[] }
│   └── error.rs         # ApiError taxonomy (07 §2)
├── engine/
│   ├── mod.rs           # plan() → execute() orchestration; owns event channel
│   ├── select.rs        # filters (os/language/kinds/dlcs) over catalog → FileTasks
│   ├── task.rs          # FileTask: one files[] entry + resolved URL + checksum
│   ├── chunk.rs         # chunk plan, Range math, completion bitmap
│   ├── worker.rs        # per-chunk downloader (Range GET → write_at → md5)
│   ├── limiter.rs       # governor bytes/s bucket, one Arc shared by everything
│   ├── sidecar.rs       # .part.json load/save (atomic)
│   └── events.rs        # ProgressEvent enum + broadcast channel
├── state/
│   ├── mod.rs           # state.json load/save (atomic, fsync, fs4 lock)
│   ├── records.rs       # InstallRecord; version/build comparison (06 §4)
│   └── cache.rs         # catalog cache files (06 §3)
└── output.rs            # human tables / JSON rendering, size formatting
```

## 2. The one real seam: `TokenProvider`

Everything below `auth` depends on an interface, not on gog-warp — tests inject a static token.

```rust
#[async_trait::async_trait]
pub trait TokenProvider: Send + Sync {
    async fn bearer(&self) -> Result<String, AuthError>; // always-fresh access token
    fn user_id(&self) -> Option<u64>;
}
```

The live impl `Session` wraps `gog_warp::Core`:

- `Session::login(code)` — `Core::new()` → `core.get_token_with_code(&code)`
- `Session::restore(store)` — read blob → `deserialize_tokens` →
  `core.start_session_from_refresh_token`
- `bearer()` — `core.obtain_galaxy_token()` (gog-warp refreshes lazily once
  `login_time + expires_in` has passed; live-verified `expires_in = 3600`)
- after a refresh: `core.serialize_tokens()` vs stored snapshot — if different, re-save via
  `TokenStore` (handles rotation even though live refresh tokens turned out to be stable)

**gog-warp scope in v1**: token exchange, refresh, owned-product ids (`get_owned_products()`),
user data (`get_user_data()`). Model B machinery (`get_builds`, `get_manifest`, its downloader)
is unused. gog-warp stays behind `TokenProvider` + `auth::store`, so an upstream API break is
contained to two files.

## 3. Concurrency model

- tokio multi-thread runtime; `AppContext` built once per command, shared via `Arc`.
- **Files run sequentially by default** (`download.parallel_files = 1`); within a file,
  `download.workers` (default 3) chunk workers run in parallel. Chunk workers are
  `tokio::spawn`ed tasks sharing:
  - one owned `std::fs::File` handle per worker (writes at offsets via `write_at`/`seek_write`)
  - an `mpsc` job channel of pending chunks
  - the global `Arc<RateLimiter>` (04 §6)
  - `Arc<FileProgress>` → sidecar updates + indicatif bars + event channel
- API calls (catalog, downlink, checksum) run through a separate small pool
  (`network.api_parallel`, default 4) with their own retry policy — never mixed with CDN workers.
- Cancellation: `tokio_util::sync::CancellationToken`, fed by `tokio::signal::ctrl_c()`. Workers
  stop at chunk/read boundaries, the sidecar is flushed, exit code 130 (03 §5).

## 4. Engine event channel

The engine talks to frontends **only** through typed events — the v2 TUI consumes the same
stream the CLI renders:

```rust
pub enum ProgressEvent {
    PlanReady  { files: u32, total_bytes: u64, skipped_bytes: u64 },
    FileStart  { id: Uuid, name: String, total_bytes: u64 },
    ChunkDone  { id: Uuid, chunk: u32, bytes: u32, md5_ok: bool },
    FileDone   { id: Uuid, path: PathBuf },
    FileFailed { id: Uuid, error: String },
    Warn       { message: String },            // e.g. "bonus content 403 → skipped (bundled)"
    Done       { ok: u32, failed: u32, skipped: u32 },
}
```

Transport: `tokio::sync::broadcast`. CLI renders via indicatif (07 §5); `--json-progress`
prints the same events as NDJSON.

## 5. Dependencies

Verified versions (discovery table in docs/07; spot-checked 2026-10-08). Versions marked ~
re-verify at implementation time.

| Concern | Crate | Version | Note |
|---|---|---|---|
| CLI | clap (derive) | 4.6.7 | + clap_complete 4.6.11 |
| Async/HTTP | tokio | 1.53.2 | rt-multi-thread, macros, signal, fs, io-util |
| | reqwest | **0.12.x** | deliberately matched to gog-warp's pin → one TLS stack; json, stream, rustls-tls; no gzip |
| | futures | 0.3.34 | |
| | tokio-util | 0.7.20 | CancellationToken |
| | async-trait | ~0.1 | dyn-safe `TokenProvider` |
| Rate limit | governor | 0.10.4 | unkeyed direct limiter, cells = bytes |
| Data | serde, serde_json | 1.x | |
| | toml | ~0.9 | config file |
| | quick-xml | 0.42.0 | checksum XML (tiny docs, buffered parse) |
| | md-5 | 0.11.0 | per-chunk + whole-file digests |
| | url | 2.5.8 | code extraction, redaction |
| | uuid | ~1 | FileTask ids (v4, `fast-rng` off) |
| Errors/secrets | thiserror / anyhow | 2.0.21 / 1.0.104 | lib errors / app errors |
| | keyring | 4.2.0 | token storage |
| | zeroize | 1.9.1 | scrub token strings |
| Dirs/UX | dirs | 7.0.0 | config/data/cache/download dirs |
| | indicatif, console | 0.18.6 / 0.16.6 | progress |
| | dialoguer | 0.12.0 | login paste, ambiguous-game prompt |
| | comfy-table | 8.0.1 | list/info tables |
| | bytesize | 2.7.0 | human sizes |
| Disk | fs4 | 1.1.0 | free-space preflight, state.json advisory lock |
| Logging | tracing, tracing-subscriber | ~0.1 / ~0.3 | |
| GOG | gog-warp | git rev-pin | auth/token/owned-ids only |

**Not used in v1**: async-compression (depot-only), crossterm, rpassword, `ctrlc`
(`tokio::signal` covers it), figment/config (hand-rolled 3-step merge is ~60 lines and fully
testable — see 05 §4).

## 6. Data flow (download, happy path)

```
clap args ─► Config overlay ─► AppContext
                                  │
list/update/download ─► engine::select (catalog + filters) ── catalog cache (06 §3)
                                  │  FileTasks (with exact sizes)
                     preflight: disk space (fs4), existing-file/record skip logic (04 §8)
                                  │
              per task: gog::downlink (Bearer) → { downlink, checksum }
                        gog::checksum (public)  → authoritative layout
                                  │
              engine::worker × N ──► CDN (Range, rate-limited)
                 │  sidecar updates, per-chunk md5 verify
                 ▼
       all chunks ok → rename .part → dest → InstallRecord → FileDone event
```

Failure paths and the retry matrix: 07 §3. State, records, update detection: doc 06.
