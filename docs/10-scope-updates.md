# 10 — Scope Updates & Direction (2026-10-08)

Decisions from the project owner, recorded so the design phase starts from the same baseline.

## 1. Decisions

1. **CLI first.** The initial deliverable is a command-line app.
2. **Use gog-warp** for whatever it covers (auth, token lifecycle, owned-products, user data).
3. **Model A only in v1** — offline installers. No depot downloads initially.
4. ~~**Later phase:** depot downloads (Model B) ship together with a **ratatui TUI**.~~
   Amended 2026-10-20 (docs/17, design 14): Model B ships as a CLI lane first
   (`gogo depot`), motivated by mac-app consuming depot trees instead of running
   installers under wine. The TUI follows whenever it comes.
5. **Dual interface long-term:** CLI = automation/scripting; TUI = human consumption. Same core,
   two front-ends.

## 2. What gog-warp gives us (verified from source, `src/core.rs`)

- `Core::new()` — reqwest client (10s timeouts, no-gzip), token store, broadcast channel
- `get_token_with_code(code)` — finish the browser paste-code flow
- `start_session_from_refresh_token(rt)` — silent re-login from a stored refresh token
- `obtain_galaxy_token()` — returns a valid token, auto-refreshing when
  `login_time + expires_in` has passed; emits `CoreEvent::TokenRefreshed`
- `serialize_tokens()` / `deserialize_tokens(json)` — persistence is a JSON
  `HashMap<client_id, Token>`
- `get_owned_products()` → `Vec<u64>` (embed `/user/data/games`, includes bundles + DLC ids)
- `get_galaxy_library()`, `get_user_data()` (userData.json)
- Model B machinery we will NOT use in v1: `get_builds`, `get_manifest`,
  `get_dependencies_manifest`, the downloader

## 3. What we build ourselves (the Model A installer client)

Nothing below exists in gog-warp — this is the actual gap:

1. **Catalog client**: `GET api.gog.com/products/{id}?locale=en-US&expand=downloads,expanded_dlcs`
   (+ batch form `?ids=...` — minigalaxy batches 50 per call). `downloads` = `{installers[],
   patches[], bonus_content[], language_packs[]}`; each entry = `{id, name, os, language,
   language_full, version, total_size (exact bytes), files[{id, size, downlink}]}`; plus
   `expanded_dlcs[]` with the same downloads shape per DLC.
2. **Downlink resolution**: `GET {files[].downlink}` (= `api.gog.com/products/{pid}/downlink/
   installer/{file_id}`) with Bearer → `{downlink, checksum}` (live-verified; plain JSON, no
   redirect step).
3. **Checksum XML parsing**: root `<file name available md5 chunks timestamp total_size>` +
   `<chunk id from to method="md5">hex</chunk>` children (**10 MiB chunks**) → chunk-parallel
   download with per-chunk md5 verification; XML `name` is the save filename; XML `total_size`
   is authoritative.
4. **Download engine**: Range/resume, N-way parallel parts, MD5 verification, disk-space
   preflight, crash-safe state
5. **Rate limiting**: bytes/sec token bucket shared across workers (gogg's `rate_limiter.go` is
   a clean reference shape; `governor` is the Rust candidate)
6. **Config layering** (defaults < file < env < CLI) and the CLI itself

## 4. Dependency facts discovered (2026-10-08)

- gog-warp license: **Apache-2.0** → safe to depend on and redistribute
- **Not published on crates.io**; repo has **no git tags** → dependency via
  `gog-warp = { git = "https://github.com/playtron-os/gog-warp", rev = "<pin>" }`
- Risk note for design: unversioned git dep means upstream can break on `main`; pin a rev and
  re-verify on bump, or vendor. Its API is small and stable-looking.

## 5. Explicitly deferred (not v1)

- ~~Depot downloads (builds/manifests/secure links/xdelta3/dependencies) — v2~~
  Shipped 2026-10-20 via gog-warp's downloader (`gogo depot`, design 14);
  xdelta3 upgrade/patch lane still deferred
- ratatui TUI — ships with v2
- Cloud saves, wishlist, friends — not planned
- Movies — technically supported by the same installer path (`downloads` entries with
  `playerUrl`); scope decision at design time
