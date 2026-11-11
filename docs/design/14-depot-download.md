# Design 14 — Depot downloads (CLI lane)

Status: contract · Depends on: docs/17 (research), design 01 (architecture), design 03
(CLI shape), design 07 (errors) · 2026-10-20.

## 0. Scope

One command: `gogo depot <GAME>` — resolve an owned game to its latest **Windows**
content-system build and materialize the game tree into the user cache directory via
gog-warp's `Downloader`. macOS and Linux full lane (Linux since #175, D7). `mac-app` consumption of
the cache is a **follow-up** (D8), not this contract.

## 1. Command surface

```
gogo depot <GAME> [--language CODE] [--dlc ID|TITLE]... [--all-dlcs] [--password PW]
                 [--dir PATH] [--limit RATE] [--no-disk-check]
                 [--dry-run] [--json] [--json-progress]
```

- `GAME` — exactly the resolver `download` uses today (id, slug, fuzzy title;
  single game per invocation, no `--all` — depot runs are heavyweight and deliberate).
- `--language` — v2 code for `language()`, mapped to the v1 name for `old_language()`
  when the build is generation 1 (default `en-US`).
- `--with-dlcs` — pass `manifest.dlcs()` (v1: `old_dlcs`) to the builder.
  > **Amended (design 19):** `--dlc <id|title>` and `--all-dlcs` select owned DLC only;
  > `--with-dlcs` becomes a hidden alias of `--all-dlcs`. The download record gains the
  > DLC set and language, and a cache missing a wanted DLC is completed (design 19 §3.3).
- `--password` — forwarded to `get_builds` for password-protected titles.
- `--dir` — override the install root (default D2). `--limit` — same grammar as
  `download --limit` (05 §3), forwarded to `set_max_speed` (bytes/s, `-1` unlimited).
- `--no-disk-check` skips the preflight (D5). `--dry-run` = resolve + plan, touch
  nothing. `--json` machine summary; `--json-progress` NDJSON events (D4).

Exit codes follow the existing map (03 §5 / 10 §4): success 0, usage 2, auth 3,
network 4, depot failure 7 (conversion/download). (Until #175 Linux exited 2 with
`gogo: depot downloads are not implemented on Linux yet`; that stub is gone, D7.)

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
- **Download record (#316).** A finished download writes `depot/<id>/.gogo-depot.json`:
  the build id and every file and link under the root (game tree and `gog-support/`),
  sorted, relative; not gog-warp's leftovers (`.gog-warp-build`, `!Temp`, `*.download`,
  `*.state`) or `.DS_Store`. When the record can't be written the download still counts
  and the cache is just unrecorded. A build that reuses a cached tree (`mac-app`/`linux-app
  <game>`, default cache root only) checks it first. All there: reused. Some gone, or no
  record (an older gogo, or a download that never finished): it says so and downloads
  again with the cold cache's consent (`--yes`, or the prompt). gog-warp keeps the files
  that are there and fetches the rest, after hashing the whole tree once (its `verify`).
  Declined: an incomplete cache is an error; an unrecorded one is built from as before,
  with a warning. Why: a cached tree that had lost its `gog-support/` was taken as whole
  and built without GOG's overlay, silently (SimCity 4 in the e2e cache, #316).

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
    Verifying(f32), Finished { install_dir: PathBuf, fetched_files: u64, fetched_bytes: u64 },
}
```

- `--json-progress`: one NDJSON line per event (camelCase keys like the installer lane).
- Human mode: plain `gogo:` lines for discrete states, a single `\r`-updated byte
  counter line for `Downloading` (indicatif not needed for one aggregate bar). Replaced
  by §4.1.
- Progress units: `DepotEvent` fractions are always `0.0..=1.0`. gog-warp is not
  consistent — `DownloadState::Allocating(f32)` is a fraction in `0..=1`, but
  `DownloadState::Verifying(f32)` is a percentage in `0..=100` — so the mapping divides
  `Verifying` by 100 (issue #46).
- `DownloadState` → `DepotEvent` is a pure function (unit-tested); `Resolved`/`Plan`
  are emitted by the driver after manifest resolution using
  `Manifest::install_directory()` + `install_size()`.

### 4.1 One renderer for `gogo depot` and the builds (#337)

> **Amended (#337):** builds that fill or repair the depot cache (`linux-app <game>`,
> `mac-app <game>`) used to read only `Plan` and print one line, so a 70 GB download
> looked frozen. Both now drive the lane through `depot_progress::drive` in the CLI, which
> hands every event to one `Renderer`. The human form above (a `\r` counter on stdout) is
> replaced by what follows.

- **Modes**, picked from the flags and stderr:

  | Mode | When | Output |
  |---|---|---|
  | JSON | `--json-progress` | every event as one NDJSON line on stdout, as before |
  | live | stderr a terminal, no `--no-progress` | a `gogo:` line on stderr as each phase starts, and a `\r` status line under it |
  | plain | stderr not a terminal, or `--no-progress` | the same phase lines, plus one line per tenth of hashing and of downloading |
  | silent | `--quiet`, or `--json` without `--json-progress` | nothing |

- **Phases.** `Resolved` and `Plan` print as before. `Preparing` → `gogo: preparing`;
  the first `Verifying` → `gogo: hashing the files already in the cache` (gog-warp sends
  `Verifying` while it hashes a reused tree, before it downloads, and sends no
  `Allocating` when it verifies); `Allocating` → `gogo: allocating`; the first
  `Downloading` → `gogo: downloading (<done> / <total>)`. The status line shows the
  phase's percentage, or bytes, speed and ETA while downloading. Speed is taken over the
  last 5 s of `Downloading` events (gog-warp sends one every 500 ms), so bytes already in
  place don't count as speed.
- **Silent phases.** gog-warp says nothing while it prepares (manifests, depots, secure
  links) or allocates under `verify`. A 100 ms timer redraws the status line with a
  spinner and the phase's elapsed time, so it never looks frozen. Plain mode has no timer:
  its phase line is the last word until the next phase.
- **What was fetched.** `Finished` gains `fetchedFiles` and `fetchedBytes`: the files the
  run wrote, new or with a new size or mtime, found by comparing the cache root before and
  after `download()` (gog-warp writes `<name>.download` and renames it, so a fetched file
  always has a new mtime). The human line is `gogo: fetched N file(s), <size> (in m:ss)`,
  or `gogo: nothing to fetch: every file was already in the cache`. For a cold download
  that's the whole tree; for a repair, what was missing.
- **Streams.** Human progress goes to stderr (design 03 §4), so `gogo depot`'s stdout keeps
  only its result line (`gogo: downloaded to …`) or its `--json` summary. A build prints
  one line of its own first (`gogo: downloading <title> into the depot cache`, or
  `gogo: checking <title>'s depot cache (hashing its files) and fetching what's missing`),
  then the same output as `gogo depot`, then its own plan and setup.
- gog-warp's own `info` log lines repeat the phase lines and would break the status line,
  so the default log filter is `info,gog_warp=warn`; `-v` shows them again.
- `--limit` (#324) needs nothing here: the speed shown is what arrives.

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

- **gog-warp pin = `playtron-os/gog-warp` rev `26c9e671`** (docs/17 §5): the
  merge of playtron-os/gog-warp#5, which adds the macOS pre-allocation arm.
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

## 7. D7 — Linux stub (superseded by #175)

> **Amended (#175, design 18 §6):** the stub is gone. The only Mac-only piece was this
> gate: gog-warp downloads on Linux (its `fallocate` arm predates the macOS one), fs4's
> free-space probe and `dirs::cache_dir()` (`$XDG_CACHE_HOME`) work there, and nothing
> else in the lane is OS-specific. `DepotError::UnsupportedPlatform` was removed. On
> Linux the `Plan` event also carries `gameFiles` (design 18 §6 "As built (#175)"); on
> macOS it doesn't, so the Mac's NDJSON and human output are unchanged.

Original text: `gogo depot` on non-macOS: resolve+plan still work (they are pure API calls) **only
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
