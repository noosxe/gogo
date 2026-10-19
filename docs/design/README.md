# gogo — Design

`gogo` is a Rust CLI for GOG.com: sign in once, list your library, and download the offline
installers you own — with checksum verification, resumable transfers, per-second bandwidth
limits, and automation-friendly output.

Everything here builds on the discovery research in `docs/01`–`docs/13`, in
particular the live-verified API model (`docs/04`, `docs/11`, verified
2026-10-08) and the scope baseline (`docs/10 §1`); the macOS bundle research
in `docs/13` grounds design 10.

## Goals (v1)

1. **Auth** — one-time interactive login (browser paste-code OAuth), silent refresh afterwards,
   OS keychain storage with a file fallback.
2. **Library** — list owned games with installer/platform/size info; detail view per game.
3. **Download** — offline installers (Model A), including optional patches, extras, language
   packs and DLC installers; resumable, chunk-verified (MD5), rate-limited, disk-space
   preflighted.
4. **Up to date** — local install records + catalog versions → detect outdated downloads;
   `gogo update` re-fetches them.
5. **Automation** — scriptable: `--json` output, stable exit codes, `--yes`, no interactive
   prompts unless a login or an ambiguous selection demands one.
6. **Config** — `defaults < file < env < CLI` layering, single TOML file.

## Non-goals (v1)

- Depot/chunked content system (Model B) — v2, together with the TUI
- Installing games into a managed library or launching them (we produce
  installers — plus, since design 10, one-shot conversion of an installer into
  a self-contained macOS `.app`; no library tracking, no launch management)
- Cloud saves, wishlist, friends, movies
- Windows support — the *binary* targets **Linux and macOS only** (downloading Windows
  installers on Linux is first-class, though — they're just files; 03 §1)
- A GUI/TUI (v2; the core is built as a library to make that cheap)

## Principles

- **Bytes are sacred.** Every file is MD5-verified per chunk (the checksum XML is
  authoritative — live-verified in docs/11). A completed file in the download dir is always a
  complete, verified file (`.part` + atomic rename, never a half file with a final name).
- **Everything resumes.** Ctrl-C, crash, network flap, expired CDN token — rerun the same
  command and it continues at chunk granularity.
- **Polite client.** Identifying User-Agent, conservative concurrency defaults, exponential
  backoff with jitter, no retries on 416/404, skip-not-fail for bundled extras (403).
- **The core is a library.** The `gogo` binary (`crates/gogo-cli`) is a thin clap frontend over `gogo-core`; the v2
  ratatui TUI (`crates/gogo-tui`) plugs into the same core (plan → progress events → records).
- **No secrets in output.** Tokens, codes, and CDN URL token paths are redacted at every log
  level (see 07 §4).

## Doc map

| Doc | Contents |
|---|---|
| [01-architecture](01-architecture.md) | crate layout, modules, dependencies, concurrency model, seams |
| [02-authentication](02-authentication.md) | login flow, token lifecycle, storage, logout |
| [03-cli](03-cli.md) | command spec, flags, exit codes, output modes |
| [04-download-pipeline](04-download-pipeline.md) | selection → plan → resolve → chunked download → verify → record |
| [05-configuration](05-configuration.md) | layering, TOML schema, env vars, validation |
| [06-local-state](06-local-state.md) | paths, state/record schema, catalog cache, update detection |
| [07-errors-and-observability](07-errors-and-observability.md) | error taxonomy, retry/backoff matrix, logging, redaction, progress events |
| [08-testing](08-testing.md) | unit/integration strategy, fixtures, mock GOG, live smoke |
| [09-v2-roadmap](09-v2-roadmap.md) | depots (Model B), ratatui TUI |
| [10-macos-app-bundles](10-macos-app-bundles.md) | `gogo mac-app`: installer → self-contained .app (Wine runtime resolution, silent install, codesign) |
| [11-recipes](11-recipes.md) | community per-game recipes for `mac-app`: schema, repo distribution, closed verb set |
## Key decisions (traceability)

| Decision | Rationale / source |
|---|---|
| gog-warp for auth + token lifecycle + user data | Apache-2.0, source-verified API (docs/10 §2); not on crates.io → git rev-pin |
| getFilteredProducts (mediaType=1) is the library | /user/data/games mixes DLC/bundle/movie ids into the game list (live: 146 ids vs 83 products, docs/11) — review A1 |
| Own catalog/downlink/checksum/download engine | Nothing in Rust covers Model A (docs/08 gap table) |
| Browser paste-code OAuth | GOG blocks custom redirect URIs; no local server; 2FA/CAPTCHA work naturally (docs/02) |
| Checksum XML authoritative | API `total_size` disagrees with XML `total_size` (live, docs/11); XML carries per-chunk MD5 |
| 10 MiB chunk-parallel downloads | Live schema: `<chunk from to>` spans + per-chunk md5 |
| reqwest 0.12.x (not 0.13) | gog-warp pins 0.12 → one HTTP/TLS stack in the binary |
| keyring-first token store | OS keychain; 0600 file fallback for headless boxes |
| governor token bucket for bytes/s | shared across workers/files; clean pause semantics |
| Default download dir under the user's Downloads dir | visible + conventional (`dirs::download_dir()`); configurable |
| Linux + macOS only (no Windows) | directories follow XDG base dirs (Linux) and `~/Library` (macOS) — 06 §1 |
| `.part` + sidecar JSON, atomic rename on completion | resume is data-only (offsets/md5), CDN URLs are never persisted |
| macOS bundles: gogo assembles the `.app` itself, headlessly | No headless API in Porting Kit/Sikarugir; CrossOver proprietary; GPTK not redistributable; Wine is LGPL + scriptable (docs/13) |

## Status

Design phase. Discovery is complete (docs/09 closed; live verification 2026-10-08 recorded in
docs/11). These documents are the implementation contract for v1.

A critique pass over these docs — incorrect assumptions, contradictions, smells — is recorded
in [review-2026-10-09](review-2026-10-09.md). Its A/B findings are contract bugs; they are
fixed in docs and code together (lockstep), starting with the A block.
