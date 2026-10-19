# Design 14 — Depot downloads (CLI lane)

Status: contract · Depends on: docs/17 (research), design 01 (architecture), design 03
(CLI shape), design 07 (errors) · 2026-10-20.

## 0. Scope

One command: `gogo depot <GAME>` — resolve an owned game to its latest **Windows**
content-system build and materialize the game tree into the user cache directory via
gog-warp's `Downloader`. macOS full lane; Linux stubbed (D7). `mac-app` consumption of
the cache is a **follow-up** (D8), not this contract.

## 1. Command surface

```
gogo depot <GAME> [--language CODE] [--with-dlcs] [--password PW]
                 [--dir PATH] [--limit RATE] [--no-disk-check]
                 [--dry-run] [--json] [--json-progress]
```

- `GAME` — exactly the resolver `download` uses today (id, slug, fuzzy title;
  single game per invocation, no `--all` — depot runs are heavyweight and deliberate).
- `--language` — v2 code for `language()`, mapped to the v1 name for `old_language()`
  when the build is generation 1 (default `en-US`).
- `--with-dlcs` — pass `manifest.dlcs()` (v1: `old_dlcs`) to the builder.
- `--password` — forwarded to `get_builds` for password-protected titles.
- `--dir` — override the install root (default D2). `--limit` — same grammar as
  `download --limit` (05 §3), forwarded to `set_max_speed` (bytes/s, `-1` unlimited).
- `--no-disk-check` skips the preflight (D5). `--dry-run` = resolve + plan, touch
  nothing. `--json` machine summary; `--json-progress` NDJSON events (D4).

Exit codes follow the existing map (03 §5 / 10 §4): success 0, usage 2, auth 3,
network 4, depot failure 7 (conversion/download), Linux stub 2 with
`gogo: depot downloads are not implemented on Linux yet` (stderr, no partial trees).

## 2. D2 — Cache layout (macOS)

```
~/Library/Caches/gogo/
  depot/<product-id>/            ← install_root: Downloader joins install_directory()
  dependencies/                  ← global_dependencies_root (shared across games)
```

- `dirs::cache_dir()` + `gogo`; `--dir` replaces the `depot/<id>` level only.
- `support_root` = `depot/<product-id>/gog-support` — support files stay inside the
  game's own subtree (self-contained per game), while dependencies dedupe globally.
- No `InstallRecord`, no state writes: the cache is disposable by contract (17 §3);
  a future `gogo depot prune` owns eviction.

## 3. D3 — Build selection

From `get_builds(product, Platform::Windows, password)`: keep `os == Windows`
(guaranteed by the call), take `public` builds, pick max `date_published`, tiebreak
lexicographically-largest `build_id`. Empty list → error 7 naming the product.
The chosen build's `generation` selects v1 vs v2 handling downstream (language + dlcs
setter variants).

## 4. D4 — Events

`gogo-core` owns the mapping; the CLI renders.

```rust
pub enum DepotEvent {
    Resolved { product_id: u64, title: String, build_id: String, version: String, generation: u32 },
    Plan { install_dir: PathBuf, download_size: u64, disk_size: u64, language: String, dlcs: Vec<String> },
    Preparing, Allocating(f32), Downloading { downloaded: u64, written: u64, total_download: u64, total_size: u64 },
    Verifying(f32), Finished { install_dir: PathBuf },
}
```

- `--json-progress`: one NDJSON line per event (camelCase keys like the installer lane).
- Human mode: plain `gogo:` lines for discrete states, a single `\r`-updated byte
  counter line for `Downloading` (indicatif not needed for one aggregate bar).
- `DownloadState` → `DepotEvent` is a pure function (unit-tested); `Resolved`/`Plan`
  are emitted by the driver after manifest resolution using
  `Manifest::install_directory()` + `install_size()`.

## 5. D5 — Driver flow (gogo-core, UI-agnostic)

```
resolve game → token → get_builds → pick_build (pure) → get_manifest
  → emit Resolved/Plan → [dry-run: stop here]
  → preflight: get_required_space() vs free space (FreeSpace trait, 03 §3)
  → Downloader::builder()
        .core(session.core().clone()) .manifest(m, build_id)
        .language(code) / .old_language(name)   // per generation
        [.dlcs(ids)] [.game_dependencies(dm)] .global_dependencies_root(..)
        .install_root(cache depot/<id>) .support_root(..) .verify(true)
  → take_progress_receiver() (spawn forwarder task → DepotEvent channel)
  → forward ctx.cancel → downloader.get_cancellation() (child token)
  → set_max_speed(limit)  → prepare() → download()
  → emit Finished{install_dir}
```

- **gog-warp pin = `noosxe/gog-warp` rev `30a8303b`** (docs/17 §5): adds the
  macOS pre-allocation arm upstream lacks; revert to playtron-os when
  playtron-os/gog-warp#5 lands.
- `Session::core()` — new read-only accessor (`&gog_warp::Core`); the auth surface
  otherwise untouched (C3 stays).
- Cancellation: existing ctrl-c wiring cancels `ctx.cancel`; depot driver links it to
  the downloader's token; exit 130 like the installer lane.
- Concurrency: one downloader at a time (single game per invocation; no cross-command
  locking — the cache is per-product and disposable).

## 6. D6 — What stays out

No `--all`, no records/state updates, no patch/upgrade lane (`upgrade_from`, xdelta),
no macOS/Linux platform builds, no resume across invocations (the downloader's own
prepare/verify makes re-runs converge). All documented as future work in 17 §4.

## 7. D7 — Linux stub

`gogo depot` on non-macOS: resolve+plan still work (they are pure API calls) **only
when** a session exists, but the materialization step returns
`DepotError::UnsupportedPlatform` before creating any directory. Rationale: keep the
command honest and testable while the mac-app pipeline that motivates it is
macOS-only; flipping later is a one-branch change.

## 8. D8 — mac-app consumption (follow-up contract, listed for context)

A later design adds `mac-app --from-depot <GAME>` (or a `--depot` input mode) that
runs this lane (or reuses a warm cache) and wraps the resulting tree without running
an installer under wine at all. Expected wins: no installer GUI/timeout lane, exact
file set, per-language control. Out of scope here.

## 9. Testing

- Unit (pure): `pick_build` ordering, `DownloadState→DepotEvent` mapping, language
  code→v1 name mapping, cache path derivation (temp HOME), limit parse→i32 clamp.
- CLI: arg parse + exit mapping for the stub (Linux target test or injected flag),
  NDJSON shape snapshot.
- Live: a real owned title (Pharaoh, 1207659046) end-to-end once, then
  `mac-app --from-depot` follow-up work consumes it.

## 10. Documentation

`docs/17` (research) + this contract + README command table row + `gogo depot --help`
copy. `docs/10` §1/§5 amended: Model B no longer deferred to "v2 with TUI"; ships as
CLI lane motivated by mac-app.
