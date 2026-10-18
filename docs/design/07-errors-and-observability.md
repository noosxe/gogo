# 07 — Errors, retries & observability

## 1. Error type layout

- Library level (`gog`, `auth`, `engine`, `state`): `thiserror` enums — typed, matchable,
  no stringly-typed errors across module boundaries.
- Application level (`main`): `anyhow::Result` + exit-code mapping (03 §5).

```rust
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("not logged in — run `gogo login`")] NotLoggedIn,
    #[error("login aborted")] LoginAborted,
    #[error("token exchange failed: {0}")] Exchange(String),
    #[error("session unauthorized (re-login required)")] Unauthorized,
    #[error("token store: {0}")] Store(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("unauthorized")] Unauthorized,          // → one refresh, then AuthError
    #[error("not found: {0}")] NotFound(u64),
    #[error("forbidden on {resource}")] Forbidden { resource: String },   // extras → skip
    #[error("server error {status}")] Server { status: u16 },             // → backoff
    #[error("network: {0}")] Network(#[from] reqwest::Error),             // → backoff
    #[error("decode: {0}")] Decode(String),
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("checksum mismatch in {file} (chunk {chunk})")] ChunkMismatch { file: String, chunk: u32 },
    #[error("retries exhausted: {url_redacted}")] RetriesExhausted { url_redacted: String },
    #[error("insufficient disk space: need {need}, have {have}")] DiskSpace { need: u64, have: u64 },
    #[error("io: {0}")] Io(#[from] std::io::Error),   // ENOSPC maps to DiskSpace/exit 6 (review C1)
    #[error("aborted")] Aborted,
}
```

Note `url_redacted` in the type itself: errors that carry URLs are constructed redacted —
redaction can't be forgotten at the log site (04 §5 / 07 §4).

## 2. Exit-code mapping (normative; echoes 03 §5)

| Error | Exit |
|---|---|
| `AuthError::*` | 3 |
| `ApiError::NotFound` / usage errors | 2 |
| `DownloadError::ChunkMismatch` (and verify failures) | 5 |
| `DownloadError::RetriesExhausted` / `ApiError` after backoff | 4 |
| `DownloadError::DiskSpace` | 6 |
| `DownloadError::Io` | 6 for ENOSPC, else 1 |
| `MacAppError::*` (design 10 §4) | 7 |
| usage errors in `mac-app` (bad flags, non-macOS) | 2 |
| cancellation | 130 |
| anything else | 1 |

Batch: collect per-file outcomes; exit with the **highest** failed class — numeric order is
outcome severity, 3 < 4 < 5 < 6 < 7 (rationale in 03 §5, review C1).

## 3. Retry / backoff matrix

One shared policy implementation (`engine::retry`), parameterized per class. Backoff =
`delay_k = base · 2^k` with ±20% jitter, capped at 60 s; `retries` from config (default 5).

| Class | Condition | Action |
|---|---|---|
| API JSON (embed/api.gog.com) | 5xx, connect, timeout, reset | retry with backoff |
| API JSON | 401 | refresh token once → retry; 2nd 401 → exit 3 |
| API JSON | 404 | no retry — bad id/slug (usage error) |
| API JSON | 429 | honor `Retry-After` if present, else backoff (never seen live) |
| downlink JSON | 403/404 | extras: `Warn` + skip (bundled); installers: fatal for file |
| checksum XML | 5xx/network | retry with backoff |
| CDN chunk | 5xx, timeout, truncated, reset | retry same Range with backoff |
| CDN chunk | 416 | if sidecar marks span done → done; else re-resolve URL once → retry → fail |
| CDN | 403 (URL expired) | re-resolve downlink (bounded: `network.retries`, budget resets on chunk progress — review B3), rebuild URL, continue remaining chunks |
| CDN chunk | 404 | re-resolve once → `FileFailed` — link revoked / file replaced; no retry loop (review C5) |
| CDN chunk | md5 mismatch | retry chunk ≤3 (fresh GET each time); then `ChunkMismatch`, exit 5 |
| any | cancel token | stop gracefully, flush, exit 130 |

Per-file retry budget: 30 min counted as **time in retry backoff / no progress** (review
B6) — steady transfer never counts, however slow (a rate-limited 60 GiB file at 2 MiB/s is
progress, not retrying), and any progress resets the timer. Exceeded → `FileFailed` with
"gave up, rerun to resume" (sidecars keep all completed chunks).

## 4. Logging & redaction

- `tracing` + `tracing-subscriber` (fmt, stderr). `-v` = info, `-vv` = debug, `-vvv` = trace.
  `--log-file PATH` mirrors everything at trace to a file.
- **Redaction happens at construction, not at formatting**:
  - `redact_url(u: &Url) -> String`: gog.com URLs → scheme + host + first path segment
    (`https://gog-cdn-fastly.gog.com/secure/offline/…[redacted N segments]`); query values of
    `code`, `token` dropped.
  - Authorization headers are never logged; reqwest is configured without request logging.
  - Token-bearing structs implement a redacting `Debug` (placeholders, never values).
- Trace logs do include: timings, status codes, byte counts, chunk ids, retry counters — enough
  to debug a flaky download without leaking credentials.

## 5. Progress rendering

- indicatif `MultiProgress`:
  - one aggregate bar: `⟨bar⟩ 512 MiB / 1.2 GiB  24.1 MiB/s  eta 00:31`
  - one bar per active file (default 1): `⟨bar⟩ setup_hot_brass…  512/691 MiB  chunk 49/70`
- Non-TTY: no bars; a `Warn`-level line per file completion on stderr.
- `--json-progress`: every `ProgressEvent` (01 §4) as one NDJSON line on stdout, e.g.
  `{"type":"chunk_done","id":"…","chunk":49,"bytes":10485760,"file_bytes":51380224,"md5_ok":true}`; final
  `{"type":"done",…}`. Consumers can reconstruct full state from events alone (TUI does).

## 6. Crash-safety summary (invariants)

1. A file in the download dir **with its final name** is complete, verified, and durable —
   `.part` + fsync-before-rename + atomic rename (04 §6). This holds for process crashes
   **and power loss**.
2. Any interruption — crash, Ctrl-C, power cut — loses at most one chunk of progress:
   chunk bytes are fsynced *before* the sidecar rename claims them (04 §4), so a durable
   sidecar never claims non-durable bytes.
3. `state.json` is never half-written (tmp+fsync+rename, 06 §2).
4. Two concurrent gogo processes cannot corrupt state (short-held state lock) or write the
   same destination twice (per-destination `.part` lock — 06 §1, review B5).
