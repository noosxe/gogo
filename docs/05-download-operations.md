# 05 — Download Operations Research (concurrency, limits, resume, verification)

What mature tools do operationally. These are *observations*, gathered to inform the design
phase — the design itself is out of scope here.

## 1. Concurrency models observed

| Tool | Model |
|---|---|
| lgogdownloader (C++/curl) | Configurable thread count (`--threads`), separate info-thread pool for metadata, per-file download threads, chunked download with Range + Etag for Galaxy installs |
| heroic-gogdl (Python) | `--workers-count` (default `multiprocessing.cpu_count()`); each worker processes a file/chunk task queue; secure-link requests serialized per product |
| gog-warp (Rust/tokio) | `tokio::sync::Semaphore(6)` around chunk requests; per-file async workers; runtime-adjustable `set_max_speed` |
| gogg (Go) | Multi-threaded file downloads, per-file goroutines |

Pattern: **two levels of concurrency** — N files in flight × M chunks/parts per file — always
with hard caps. Nobody does unbounded parallelism (they'd trip server-side throttling).

## 2. Rate limiting / throttling (client side only)

No documented server limits exist; observed behaviors that keep tools un-throttled:

- **Exponential backoff on failed requests**: gog-warp wraps every API call in
  `reqwest_exponential_backoff` (retry with growing delays; treats 401 as non-retryable
  → triggers token refresh).
- **Conservative sleeps**: gogdl retries `secure_link` failures after `0.2s`.
- **Retry taxonomy (lgogdownloader)**: retry HTTP 5xx; do NOT retry 416 (Range Not Satisfiable)
  — it's the legitimate "resume complete" signal; delete file on failed resume or zero-length.
- **Speed cap**: gog-warp exposes `set_max_speed` mid-download (the app-level knob our
  rate-limit config would map to). Implementation options in Rust recorded in doc 07
  (token-bucket via `governor`, or chunk-based pacing like gogdl's old `--chunk-size`/sleep
  approach).
- Chunk sizes: GOG v2 chunks are server-defined (~4 MiB observed); classic installers resume
  with arbitrary Range offsets.

## 3. Resume & state

- **Classic installers**: HTTP `Range: bytes={offset}-` from existing file size; verified by
  MD5 from checksum XML after completion (lgogdownloader).
- **v2 depots**: per-chunk state (bitmask) persisted next to the download; completed chunks
  (md5-named files) are skipped on re-run (gog-warp `progress.rs::load_chunk_state`,
  `ready_files` set).
- **Manifest caching**: gogdl stores the previous manifest in `MANIFESTS_DIR` keyed by game id —
  used for verify/repair and diffing without re-downloading.
- **Metadata caching**: lgogdownloader caches `product.json` / `game-details.json` /
  `galaxy_install_*.json` per game in its cache dir; GogDownloader keeps a SQLite DB with
  schema migrations and `--new-only`/`--updated-only` refresh modes.

## 4. Integrity verification

- **Classic installers**: MD5 per part from GOG's checksum XML (lgogdownloader hashes with
  RHash; re-checks "duplicate file identical by md5" during organization).
- **v2 depots**: every chunk md5-verified (chunk id == md5 of compressed bytes); whole-file md5
  after assembly; final full-tree verify command (`repair` in gogdl, `verify` module in
  gog-warp).
- **Failure policy observed**: mismatch → re-download file; repeated failure → abort game with
  clear error (GogDownloader has a `RetryDownloadForUnmatchingHashException` with bounded
  retries).

## 5. Other operational behaviors worth copying

- **Pre-flight disk space check** (`shutil.disk_usage` in gogdl; `get_required_space` in
  gog-warp).
- **Total size calculation** before download (gogdl `calculate_download_size`, gogg "calculate
  the total size of the files to be downloaded", RikudouSage `total-size` command).
- **Graceful Ctrl-C** → flush state files so resume works (state is per-chunk, cheap to save).
- **Listing-first UX**: `list`/`games` command, `--os/--language/--search` filters, export to
  file (gogg), `download <id|slug>`.
- **Idempotent re-runs**: "only download missing or newer files" (gogg), `--updated-only`
  (GogDownloader).
- **User-Agent etiquette**: tools identify themselves (e.g. `gogdl/{version} (Heroic Games
  Launcher)`); gogdl's session disables requests' default auth-retry behavior (`session.auth =
  lambda r: r`).
- **Timeouts everywhere**: gogdl uses a request TIMEOUT constant; GogDownloader exposes
  `--idle-timeout` (needed on slow NAS boxes).
