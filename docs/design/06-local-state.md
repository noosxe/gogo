# 06 — Local state

gogo keeps four classes of local data. Paths resolve via `dirs` (platform table per docs/06 §1).

## 1. Path map

| Class | Linux (XDG base dirs) | macOS (`~/Library`) | Contents |
|---|---|---|---|
| config | `$XDG_CONFIG_HOME/gogo`, else `~/.config/gogo` | `~/Library/Application Support/gogo` | `config.toml` (the only user-edited file) |
| state | `$XDG_STATE_HOME/gogo`, else `~/.local/state/gogo` | `~/Library/Application Support/gogo` | `state.json` (install records), `tokens.json` (file store), `verify-cache/` |
| cache | `$XDG_CACHE_HOME/gogo`, else `~/.cache/gogo` | `~/Library/Caches/gogo` | `catalog-{locale}.json` |
| data | user-chosen `download.dir` (default under the XDG download dir, 05 §3) | same | `{Title}/setup_….exe`, `.part` + sidecars while running |

**Resolution rules (normative).** All paths resolve through the `dirs` crate, which implements
both standards: the XDG base-directory spec on Linux (`config_dir`, `cache_dir`, `state_dir`,
`data_dir` — env var first, well-known default second) and the macOS `~/Library` conventions
(`config_dir`/`data_dir` → Application Support, `cache_dir` → Caches, `download_dir` →
`~/Downloads`).

- `dirs::state_dir()` is **Linux-only** (the XDG state dir has no macOS counterpart) and
  returns `None` on macOS → fall back to `data_dir()`, i.e. state lives next to config in
  Application Support. Resolution: `state_dir().unwrap_or_else(data_dir)` + `/gogo`.
- **Windows is unsupported** (scope decision, README non-goals): no `%APPDATA%` /
  `%LOCALAPPDATA%` handling, no Windows-specific path logic anywhere.
- `GOGO_HOME` (env override) flattens config + state + cache into a single directory —
  `config.toml`, tokens file, `state.json`, and the catalog cache sit side by side in
  `$GOGO_HOME/`. Purpose: tests, portable/USB setups, containers. It never touches
  `download.dir` (data stays where the user chose it).

Locking: an advisory `fs4` lock file `state.lock` guards state.json against two concurrent
gogo processes (second process exits 1 with a clear message).

## 2. Atomicity rules

Every state write: write `file.tmp` → `fsync` → `rename` over target → (best-effort) fsync
parent dir. A crash never leaves a truncated state.json; a stale `.tmp` is ignored/reaped on
next run. Same procedure for sidecars (04 §5) and the catalog cache.

## 3. Schemas

### `state.json`

```json
{
  "schema": 1,
  "user_id": 1234567,
  "records": [
    {
      "product_id": 1075439258,
      "dlc_id": null,
      "file_id": "en1installer0",
      "kind": "installer",
      "os": "windows",
      "language": "en",
      "title": "Hot Brass",
      "version": "2.2.2",
      "build": 85647,
      "md5": "<whole-file md5 from checksum XML>",
      "total_size": 724243232,
      "api_size_note": 723517440,
      "path": "/home/user/Downloads/gogo/Hot Brass/setup_hot_brass_2.2.2_(85647).exe",
      "checksum_ts": "2026-03-13 08:45:39",
      "chunks": 70,
      "downloaded_at": "2026-10-08T12:00:00Z"
    }
  ]
}
```

- Record identity: `(product_id, dlc_id, file_id)`. `build` is parsed from the filename
  `…_(<build>).exe` at download time (also stored in `path`, so it's auditable).
- `verify-cache/{product_id}-{file_id}.xml` keeps the last checksum doc so `gogo verify`
  works offline; re-resolved from the API when missing.

### `catalog-{locale}.json`

```json
{ "fetched_at": "2026-10-08T11:00:00Z", "owned_ids_fetched_at": "…", "products": { "1075439258": { …raw product JSON… } } }
```

- Products fetched in 50-id batches (`?ids=…`), stored raw (serde passthrough) — rendering and
  filtering happen over typed views built from it.
- **Owned ids are never cached** — `list`/`download` always fetch `/user/data/games` (one cheap
  call) so new purchases appear instantly; the per-product cache has TTL
  `state.catalog_ttl_hours` and `--refresh` bypasses it.
- Cache is disposable at any time; corruption → rebuild silently.

## 4. Update detection

The modern products API has no `updated` flag (live-corrected, docs/11). Detection compares
**local records vs catalog**:

```
cheap path (default):
  outdated(record) =
     catalog[record.product_id] entry (same kind/os/language/file_id, dlc) missing
     or entry.version != record.version

strict path (--strict, one checksum resolution per file):
  additionally compares build id (filename `_(N)`) and checksum `timestamp`
```

- `gogo update` (no args) lists outdated games; `gogo update <games…>` downloads their outdated
  files via the normal pipeline (04).
- Version comparison is exact string equality after trim (GOG versions are plain `x.y.z`);
  build-id comparison is integer. No semver gymnastics — a changed string = outdated.
- Missing catalog entry for an existing record (game removed from account) → flagged
  `obsolete` in `list`, never auto-deleted.

## 5. Multi-machine / backup notes

State is per-machine by design (paths differ). `gogo verify --rebuild` walks the download dir
and adopts existing verified files into records (fresh state or new machine with copied
files) — reuses the plan-stage "adopt" path (04 §2 rule 4).
