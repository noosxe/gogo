# 11 — Open Questions: Resolution Status (2026-10-08)

Follow-up deep-dive into lgogdownloader (C++), minigalaxy (Python), gogg (Go) sources, the
community API docs, and gog-warp's full source. Questions reference `docs/09` §2.

## Resolution matrix

| # | Question | Status | Answer & evidence |
|---|---|---|---|
| 1 | `expires_in` exact value | ✅ **Resolved** | **~1 hour (3600s)**, not 72h as earlier assumed. gogapidocs auth.html: *"Renew the token when it expires after about an hour. Check the original response for an accurate lifetime."* gog-warp's own unit test uses `expires_in: 3600`. Refresh via long-lived `refresh_token`. |
| 2 | Bearer on embed.gog.com account endpoints | ✅ **Resolved** | **Yes.** minigalaxy sends its OAuth Bearer on `embed.gog.com/account/getFilteredProducts`, `/userData.json`, `/user/data/games` in production; gogdl and gog-warp do the same on `/user/data/games`. |
| 3 | Installer downlink resolution | ✅ **Resolved + live-verified** | Modern products API has **no** `galaxy_downlink_json_url`. The live equivalent: `downloads.installers[].files[].downlink` = `https://api.gog.com/products/{pid}/downlink/installer/{file_id}`; GET with Bearer → 200 `{downlink, checksum}` directly (no redirect). Same JSON shape lgogdownloader/minigalaxy/gogg consume. |
| 4 | Download auth mode | ✅ **Resolved + live-verified** | Downlink JSON endpoint needs Bearer. The returned CDN URL is **pre-tokenized in its path** (`gog-cdn-fastly.gog.com/token=…~dirs=6~token=…/secure/offline/…`) and serves Range requests **with no Authorization header at all** (live: 206 without auth, 206 with). Sending Bearer anyway is harmless. |
| 5 | Checksum XML schema | ✅ **Resolved + live-verified** | Root `<file name available notavailablemsg md5 chunks timestamp total_size>`; children `<chunk id from to method="md5">md5hex</chunk>`. Live sample: **10 MiB chunks** (70× for a 724 MB installer, last partial), `from/to` span the whole file, XML URL is public. XML `total_size` is authoritative (API size can differ — see live notes). |
| 6 | Rate-limit behavior | ✅ **Resolved (first order)** | Live: 6 Range GETs @0.4s → all `206`, no 429/throttle signals. Prior-art handling stands: backoff 5xx, never-retry 416, 403 = skip bundled extras; conservative client-side limits remain the design default. |
| 7 | Update detection for "only newer" | ✅ **Resolved + live-corrected** | The `updated` flag is **not** in the modern products API (that was the older gameDetails era). Live installer fields: `version` (e.g. `2.2.2`), no `date`. Detection = `version` + checksum-XML `timestamp` + build id in filename (`setup_hot_brass_2.2.2_(85647).exe`) vs locally stored state. |
| 8 | Movies | ✅ **Resolved** | Same download path would apply; live check: **0 movies** in this library (`mediaType=2` → `totalProducts: 0`). Low priority; scope decision at design time. |
| 9 | DLC installers | ✅ **Resolved** | `products/{id}?expand=downloads,expanded_dlcs` returns `expanded_dlcs[]` with full `downloads.installers[]` per DLC (minigalaxy `get_dlc_info` downloads them with the same flow). lgogdownloader additionally uses `dlcs.expanded_all_products_url`. |
| 10 | Beta branches / password-protected builds | ⏸ **Deferred** | Model-B only; gog-warp's `get_builds(product_id, platform, password)` already covers it when v2 lands. |

## New facts that emerged (input for design)

- **gog-warp packaging**: Apache-2.0, **not on crates.io**, no git tags → git rev-pinned
  dependency (doc 10 §4). Core API fully mapped (doc 10 §2).
- **Batch product lookups**: `GET /products?expand=downloads,expanded_dlcs&ids=id1,...,id50`
  (minigalaxy batches 50) — matters for large libraries.
- **minigalaxy installer selection logic**: filter by `os`, prefer configured `language`, fall
  back to `en`, fall back to last entry — a good default rule set.
- **gogg download engine details**: HEAD (Bearer) for size/exists; parallel **Range-region**
  downloads for one file; treats **server-ignored Range (200 instead of 206)** as
  restart-from-zero; **missing checksum ⇒ skip verification, never fail**; 403 → skip file
  (bundled extras).
- **Rate limiter reference**: gogg's `client/rate_limiter.go` = global bytes/sec token bucket
  wrapped around the body reader — directly maps to a Rust `AsyncRead` wrapper or `governor`.
- **Pagination**: owned-games listing is paged with a `next` link to follow generically (gogg
  `resolveNext`).

## Live verification results (2026-10-08, real account, library n=146)

Runbook executed; follow-up probes filled the gaps. Token files deleted; harmless artifacts
(JSON/XML samples) remain in the gitignored `gogo-probe/`.

| Check | Live result |
|---|---|
| `expires_in` | **exactly 3600s**, `token_type: bearer`, `scope: ""`, `session_id` present |
| Bearer on embed endpoints | `/user/data/games` → 146 ids; `/account/getFilteredProducts` → 200 (83 products, 100/page); `/userData.json` → 200 |
| Refresh grant | 200; **refresh_token NOT rotated**; new `expires_in` 3600 |
| Movies | `mediaType=2` → `totalProducts: 0` in this library |
| Downlink endpoint | `api.gog.com/products/{pid}/downlink/installer/{file_id}` + Bearer → 200 `{downlink, checksum}` |
| CDN | `gog-cdn-fastly.gog.com/token=…~dirs=6~token=…/secure/offline/…setup_…(85647).exe` — pre-tokenized, **no auth header needed** (206 with and without Bearer) |
| Rate probe | 6 Range GETs @0.4s → all `206`, no throttle signals |
| Checksum XML | public, no auth; 7.4 KB describing a 724 MB installer |

### Canonical installer flow (live-verified)

```text
1. GET api.gog.com/products/{id}?expand=downloads,expanded_dlcs                 (Bearer)
   downloads = { installers[], patches[], bonus_content[], language_packs[] }
   installer  = { id, name, os, language, language_full, version, total_size,
                  files: [ { id: "en1installer0", size, downlink } ] }
2. GET {files[].downlink} = api.gog.com/products/{pid}/downlink/installer/{file_id} (Bearer)
   → { "downlink": "<pre-tokenized CDN url>", "checksum": "<xml url>" }
3. GET {checksum}  (no auth) → layout XML:
   <file name="setup_hot_brass_2.2.2_(85647).exe" md5="…" chunks="70"
         timestamp="2026-03-13 08:45:39" total_size="724243232">
     <chunk id="0" from="0" to="10485759" method="md5">0123…</chunk> … (10 MiB chunks)
4. GET {downlink}  (no auth needed) — whole-file or per-chunk Range; verify md5(s)
```

### Corrections & notes vs the source-level findings

- `galaxy_downlink_json_url` does **not** exist in the modern API — `files[].downlink` is its
  replacement (same `{downlink, checksum}` JSON shape the three tools consume).
- `updated` flag absent → update detection via `version` + XML `timestamp` + filename build id.
- API `total_size`/`files[].size` (723,517,440) ≠ XML `total_size` (724,243,232) — the
  difference is exactly the final chunk. **XML is authoritative**; confirm against real bytes
  at runtime.
- Chunks are **10 MiB** with per-chunk md5 → chunk-parallel download + per-chunk verify is a
  first-class option, not just whole-file md5.
- Save-name = XML `name` attr (`setup_<slug>_<version>_(<buildid>).exe`).
- `bonus_content` (extras) and `language_packs` ride the identical `files[].downlink` flow.
- lgogdownloader/gogg also handle the legacy `manual_url` redirect flavor; we don't need it.
