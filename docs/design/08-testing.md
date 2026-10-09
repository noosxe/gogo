# 08 — Testing strategy

Target: the engine and API layer are the risk surface — they get real coverage; the thin
frontend (clap wiring, tables) gets smoke tests.

## 1. Fixtures

All under `fixtures/`, **synthesized** — structure mirrors live captures, every value (md5s,
sizes, ids, urls) is fake:

| File | Mirrors | Used by |
|---|---|---|
| `checksum.xml` | live Hot Brass checksum doc (70-chunk layout shrunk to 4 chunks) | checksum parser, engine |
| `product.json` | products API entry with downloads/expanded_dlcs | catalog client |
| `downlink.json` | `{downlink, checksum}` response | downlink client |
| `library.json` | owned-ids array | library client |
| `sidecar-v1.json` | sidecar example (04 §5) | resume tests |

Regeneration note: `probe.sh` artifacts in `gogo-probe/` are the reference shapes — copy
structure, replace values (tokens/ids/md5s) before committing anything.

## 2. Unit tests (fast, no I/O)

| Module | Cases |
|---|---|
| `gog::checksum` | parse fixture; attribute order variance; missing chunks element; whitespace/text-content md5; BOM; `total_size`/`chunks` count mismatch → warn field |
| `engine::chunk` | planner invariants: contiguity, coverage 0..total_size-1, last partial, size < 1 chunk, exact multiple (property-based via `proptest` — ~1 dep, worth it) |
| `config` | 4-layer precedence matrix; rate grammar (accept/reject table); env parse errors; validation ranges; unknown TOML key → warning |
| `state::records` | build-id parse from filename; version equality; outdated() cheap vs strict; adopt-path decision table (04 §2 rules 1–5) |
| `auth` | redirect-URL code extraction (valid host/path, wrong host, missing code, extra params); store save/load/clear round-trip on a temp dir (file backend) |
| `gog::downlink` | response decode; redaction of URL in any error path |
| `output` | size formatting, table snapshot (insta? — plain assert on rendered string is enough) |

## 3. Integration tests (wiremock, in-process)

One mock server stands in for api.gog.com + CDN (Range support, scriptable status per path):

1. **Happy path**: product → downlink → checksum → CDN serves deterministic bytes;
   assert file bytes, sidecar gone, record written, events sequence
   (`PlanReady → FileStart → ChunkDone×N → FileDone → Done`).
2. **Resume**: drop connection after k chunks (wiremock can close mid-body); rerun;
   assert only remaining chunks requested (Range assertions on the mock), same final bytes.
3. **Corrupt chunk**: first GET of chunk 2 returns wrong bytes, retry returns right bytes;
   assert one retry, final md5 ok, sidecar `attempts` recorded.
4. **Persistent corruption**: always-wrong chunk → exit 5, `.part` + sidecar remain.
5. **416 / URL expiry**: 403 on first CDN call → re-resolve (assert 2nd downlink call) → OK.
6. **Extras 403**: bonus_content downlink 403 → Warn event, other files complete, exit 0.
7. **API 5xx then success**: backoff path (shrunk delays via config), one retry, success.
8. **401 → refresh → 200**: mock token refresh endpoint; bearer header updated (asserted).
9. **Disk full**: preflight sees tiny free space (mock via fs4 abstraction seam or a tiny
   tmpfs mount on Linux CI) → exit 6, nothing written.
10. **Rate limit**: `limit = 1 MiB/s`, 4 MiB file, assert elapsed ≥ ~4 s ± tolerance and all
    chunks verified.

## 4. Property/soak extras

- Chunk planner vs random sizes: total spans always == [0, total_size).
- Sidecar round-trip: save → load → identical plan (incl. partial completion states).

## 5. Live smoke (manual, opt-in)

`GOGO_LIVE=1 cargo test --test live -- --ignored` — read-only commands only (`whoami`,
`list`), real account. Never in CI, never writes. Mirrors `scripts/probe.sh` coverage as a regression
harness if GOG changes the API.

## 6. CI shape

- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`
- `cargo nextest run` (unit + integration; wiremock binds 127.0.0.1 — no network needed)
- All commands run at the workspace root (01 §1); integration tests live in `crates/gogo-core/tests/` — the CLI is too thin to warrant its own suite, the TUI mirrors `list`/`info` rendering through core APIs.
- `cargo deny` (licenses/advisories; gog-warp git dep noted as Apache-2.0)
- Coverage informational (target: `engine` + `gog` ≥ 80% line coverage)
