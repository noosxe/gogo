# 04 — Download pipeline

The heart of gogo. Six stages: **select → plan → resolve → download → finalize → record**.
All API facts below are live-verified (docs/04, docs/11).

## 1. Stage: select (catalog → candidate entries)

Input: game selector(s) + selection flags (03 §1). The catalog (products API, batched
`?ids=`×50, `expand=downloads,expanded_dlcs`, cached per 06 §3) yields, per game:

```
downloads = { installers[], patches[], bonus_content[], language_packs[] }
entry     = { id, name, os, language, language_full, version, total_size,
              files: [ { id, size, downlink } ] }
expanded_dlcs[] = same shape per DLC
(entry ids are strings for installers/patches/language_packs but **numbers** for DLC
`bonus_content` — live-verified 2026-10-10 during the first live plan run; normalized to
strings at parse time)
```

Filter rules:

- kind default = `installers` only; `--include/--only` widens/replaces (patches, extras =
  bonus_content, lang-packs = language_packs; DLC installers join via `--with-dlcs`).
- `--os`: entry `os` is `"windows" | "mac" | "linux"`; `all` selects every entry. Default =
  host OS with fallback to `windows` when a game has no host-OS entries (notice printed;
  details in 03 §1).
- `--language`: exact match on entry `language`; fallback chain `en → en-US → first sorted`
  (per game, deterministic: alphabetical by `language_full`).
- DLCs are selected with the same os/language rules, recursively (DLC of DLC does not occur in
  practice; if it does, one level deep is enough).

Output: candidate entries each carrying their `files[]` items. **Each `files[]` item is one
download task** (multi-part installers = `en1installer0`, `en1installer1`, … all required).

## 2. Stage: plan

For every task compute: destination path, current status (new / resumable / already-present /
outdated / pending-adoption — rule 4 below), and byte totals. Destination:

```
{download.dir}/{sanitized title}/            # DLCs: {dir}/{title}/{dlc title}/
    {checksum_xml_name}                      # e.g. setup_hot_brass_2.2.2_(85647).exe
```

- `sanitized title`: strip filesystem-hostile characters (slash, backslash, colon, asterisk,
  question mark, quote, angle brackets, pipe) plus control chars, collapse whitespace.
  Paths may collide benignly when two products share a title (filenames differ); the record's product id disambiguates (dir name stays pretty).
- The **save filename is the checksum XML `name` attribute** (live-verified canonical form:
  `setup_<slug>_<version>_(<buildid>).exe`) — not the API's per-file id.
- Extras filenames resolve in a fixed chain (review C4): checksum XML `name` →
  `Content-Disposition` filename → sanitized entry `name` + `-{file_id}` (warn when the
  last link is used). `Content-Disposition` shape is unverified — probe item (docs/12).

Skip logic (in order):

1. A state record exists for `(product_id, dlc_id, file_id)` with same `version`+`build`, the
   file exists, and its size matches → **skip** (`↷ already present`) unless `--force`.
2. A record exists but catalog shows a newer `version`/`build` → redownload (04 §8 overwrite).
3. `{dest}.part` + valid sidecar exists → **resume** (04 §5).
4. `{dest}` exists with no record → **pending-adoption**: no checksum fetch, no file read at
   plan time. The execute stage resolves the checksum, verifies the local file (read +
   md5), then adopts it as a record + skips — or re-downloads over it (warn). Plan stays
   O(1) per task (review B4: no API calls or hours of disk I/O before the confirmation).
5. else → download.

Plan emits `PlanReady { files, total_bytes, skipped_bytes }` and a human/JSON plan table
(`--dry-run` stops here).

Plan totals may overstate while `pending-adoption` tasks await verification — accepted
(review B4); the end-of-run summary reports actuals.

## 3. Stage: resolve (per task, just before its download starts)

Resolution happens **at download time, per file, every run** — CDN URLs are pre-tokenized and
time-limited, so they are never cached or persisted:

```
1. GET api.gog.com/products/{pid}/downlink/installer/{file_id}   (Bearer)
   → 200 { "downlink": <cdn url>, "checksum": <xml url> }
2. GET {checksum}   (no auth, small)
   → ChecksumDoc { name, available, notavailablemsg, md5, timestamp, total_size, chunks: [ {id, from, to, md5} ] }
```

- `ChecksumDoc` is authoritative: size = `total_size`; layout = `chunks[]` (10 MiB spans,
  last partial). If API `total_size` disagrees with XML `total_size` (observed live,
  docs/11) → trust the XML, log a debug note.
- No-chunks case (some extras): single whole-file stream; verify via root `md5` if present,
  else size-only (warn).
- `available="false"` (review A2): the checksum doc itself says the file is not
  downloadable — `Warn` carrying `notavailablemsg`, skip the file; never treated as the
  no-chunks case.
- Layout validation on resume: sidecar's `total_size` + chunk count + `from/to` spans must
  match the fresh `ChecksumDoc`; mismatch means GOG replaced the file → discard sidecar,
  restart (warn).
- 403/404 on resolution: for extras → `Warn` + skip (bundled-in-installer case, prior-art
  behavior); for installers → FileFailed (exit path per 07 §3).

## 4. Stage: download (chunk engine)

```
open {dest}.part, set_len(total_size)                 # preallocate
sync_all()   # durable file size (metadata) before any chunk is claimed done
for each worker (download.workers, default 3):
    own File handle; loop { chunk = jobs.recv() or done }
    per chunk:
        resp = GET {downlink} Range: bytes={from}-{to}      # retry policy 07 §3
        md5 = Md5::new(); pos = from
        while let Some(buf) = resp.chunk().await:           # 32–64 KiB reads
            limiter.until_n_ready(buf.len()).await          # global bytes/s bucket
            if cancelled: flush bitmap; exit 130
            file.write_all_at(pos, buf); md5.update(buf); pos += buf.len()
        digest == chunk.md5 ? sync_data(); mark done + save bitmap
                             : retry same Range (≤3) else FileFailed (exit 5)
all chunks done → finalize
```

- Workers write disjoint offsets via their own handles — no locks on the data file.
- The rate limiter is **global** (all files, all workers): one `governor` direct limiter with
  `Quota::per_second(bytes_per_sec)`; burst = min(1 MiB, limit). `limit = 0` → no limiter
  object at all (zero overhead path). Asks are split so a single call never exceeds burst
  capacity (governor rejects larger asks with `InsufficientCapacity` — review A4): a read
  buffer is consumed in ≤ burst-sized slices, so the configured rate holds exactly, even at
  the 32 KiB/s floor (05 §3).
- Progress accounting happens on **written** bytes (not requested), which keeps bars/ETA honest
  under limiting.
- Progress (done-bits) persists after every chunk completion to `{dest}.part.bitmap`
  (atomic tmp+rename; ~1 bit per chunk — review C6). Crash window = one chunk of lost
  progress.
- **Durability ordering (power-loss safe):** a chunk's bytes are made durable
  (`sync_data`) **before** the `.part.bitmap` rename records the chunk done — a durable
  never claims non-durable bytes. The preallocation `sync_all` makes the file size durable
  up front, so per-chunk `sync_data` suffices. Cost ≈ 1 fsync per 10 MiB chunk (~100/GiB)
  — noise against the transfer itself.
## 5. Stage: resume state — sidecar

`{dest}.part` holds the bytes; two sidecars hold the state (review C6): `{dest}.part.json`
is the **plan** (metadata + chunk table, written once at resolve, rewritten only on layout
change) and `{dest}.part.bitmap` is the **progress** (done-bits, ~1 bit per chunk, atomic
tmp+rename per completion — a 6k-chunk file updates ~750 B, not ~600 KB of JSON).
**No URLs in either** (tokens never touch disk; re-resolution is one cheap API call).

Done-bits are durable claims (04 §4 ordering): a chunk is recorded done only after its
bytes are fsynced, so **resume never re-verifies completed chunks** — no re-read, no
re-download, even after a power cut.

Per-run retry `attempts` live in memory only (budgets reset on resume); done-bits are the
only persisted progress.

```json
{
  "schema": 1,
  "product_id": 1075439258,
  "dlc_id": null,
  "file_id": "en1installer0",
  "kind": "installer",
  "os": "windows",
  "language": "en",
  "version": "2.2.2",
  "build": 85647,
  "dest_name": "setup_hot_brass_2.2.2_(85647).exe",
  "total_size": 724243232,
  "whole_md5": "…",
  "chunks": [
    { "id": 0, "from": 0, "to": 10485759, "md5": "…" },
    { "id": 1, "from": 10485760, "to": 20971519, "md5": "…" }
  ]
}
```

Orphaned `.part` files (no sidecar, or sidecar for a different product) are reported by
`gogo verify` and removed with `--fix`.

## 6. Stage: finalize

1. All chunks `done` → verify count + `pos == total_size` for each.
2. Optional whole-file check: if root `md5` present and chunks were verified individually,
   skip the full re-read (per-chunk md5 over the same spans is equivalent evidence).
3. `sync_all()` on the `.part` handle (covers the no-chunks stream path too), then
   `rename({dest}.part, {dest})` — POSIX rename within one directory (atomic on
   Linux/macOS); best-effort fsync of the destination directory makes the new name itself
   durable (same pattern as 06 §2).
4. Append `InstallRecord` to state (06 §3), delete both sidecars (`.json` + `.bitmap`), emit `FileDone`.

## 7. Edge cases

| Case | Handling |
|---|---|
| CDN 403 mid-download (URL expired) | re-resolve downlink (bounded: `network.retries`, budget resets on chunk progress — review B3), rebuild URL, continue remaining chunks (done chunks are URL-independent) |
| 416 on a Range | treat as "span already satisfied" → mark chunk done only if sidecar says so; else re-resolve once, then fail |
| Zero-length body / truncated stream | chunk retry (attempts+1); after retries → FileFailed |
| `total_size` API ≠ XML | XML wins (04 §3); recorded in the record for forensics |
| Same filename for different products | impossible in practice (name embeds slug+build); if it happens, suffix ` (2)` before extension and warn |
| Filename changes between runs (new build) | old file remains (user may keep versions); record links to new path; `update` output shows both |
| Very small file (< 1 chunk) | single chunk task; workers idle |
| Extras without checksum XML | whole-file stream, size-only verify, `Warn` in summary |
| Checksum root `available="false"` | `Warn` with `notavailablemsg`, skip the file (04 §3, review A2) |

## 8. Overwrite & version policy

- GOG filenames embed `<version>_(<build>)`, so a newer version lands in a **new file** — the
  old one is left in place (user decides; `gogo update` prints old→new pairs; a future
  `--prune-versions N` may clean up).
- The state record keys on `(product_id, dlc_id, file_id)` and stores `version`, `build`,
  `md5`, `path` — re-downloading the *same* version replaces the file (resume/repair path).
- `--force` bypasses the "already present" skip only.

## 9. Disk space preflight

Before executing: `fs4::available_space(dest volume)` vs Σ(remaining bytes of planned tasks +
100 MiB margin). Insufficient → list shortfall, exit 6, download nothing. `--no-disk-check`
skips (for exotic filesystems where the call lies).

The `available_space` call sits behind a small `FreeSpace` trait (fs4 impl in production,
mock impl in tests — review D4).
