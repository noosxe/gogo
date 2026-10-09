# gogo — Discovery & Research Notes

**Status:** Discovery complete (live verification 2026-10-08, see doc 11). The design phase is
under way in [`docs/design/`](design/README.md) — implementation follows that contract.

**Research date:** 2026-10-08
**Method:** Public docs, community API documentation, and source-level review of the
established open-source GOG clients/downloader (lgogdownloader, heroic-gogdl, gog-warp,
RikudouSage/GogDownloader, gogg, minigalaxy, wyvern, gog-rs). All factual claims below are
traceable to the sources linked inside each document.

## What we are building (problem statement)

A Rust CLI app that:

1. Performs a proper **sign-in flow** against gog.com
2. **Lists the user's purchased games**
3. **Triggers downloads** of the user's offline installers
4. Has **configuration** for download rate limiting, download directory, etc.

## Key findings at a glance

| Area | Finding |
|---|---|
| Official API? | **None for clients.** GOG's public "Galaxy API" docs target game developers (in-game SDK), not library access. Everything relies on the *unofficial* web/Galaxy REST APIs used by the GOG Galaxy client. |
| Auth | OAuth2 **authorization-code** flow with a browser step. The Galaxy client's `client_id`/`client_secret` are public and universally reused by third-party tools. CLI convention: user logs in inside their browser, then pastes the `code=` from the redirect URL into the terminal (no local redirect server needed — GOG does not let you register custom redirect URIs). |
| Token | `GET https://auth.gog.com/token` (yes, GET) with `authorization_code` or `refresh_token` grant. Token JSON: `access_token`, `refresh_token`, `user_id`, `expires_in`, `token_type`, `scope`, `session_id`. **Access token ≈ 1h; transparent refresh via `refresh_token`.** |
| Library | `GET https://embed.gog.com/user/data/games` → array of owned product IDs. Richer: `GET /account/getFilteredProducts` (paged) and `GET https://api.gog.com/products/{id}?expand=downloads,...`. User info: `embed.gog.com/userData.json`. |
| Installers | Two content models: (A) **classic offline installers** — v1 target, live-verified: `products?expand=downloads` → `files[].downlink` → `{downlink, checksum}` → pre-tokenized CDN; (B) **Galaxy depots** (chunked) — deferred to v2 with the TUI. |
| Downloads | CDN serves pre-tokenized URLs — no auth header needed, Range/resume ✓; checksum XML gives whole-file MD5 + **10 MiB chunks with per-chunk MD5** → chunk-parallel verification possible. |
| Rate limiting | No documented server-side limit. All mature tools are client-side limited (concurrency caps, exponential backoff on 5xx/401). |
| Config | Precedents for per-OS config dirs (`XDG_CONFIG_HOME`, `APPDATA`, `~/Library/Application Support`), env-var overrides (`GOGDL_CONFIG_PATH`), TOML/JSON config, and OS keychain token storage. |
| Rust prior art | **`gog-warp`** (playtron-os, by imLinguin of Heroic) is a modern Rust library with auth + library + full v1/v2 content-system downloader — the single most relevant codebase. Also: `gog` crate (stale), `wyvern` (stale CLI), `gog-conjure`. |
| Risk | Unofficial APIs can change or rate-limit at any time; login page may serve CAPTCHAs; tokens are revocable. lgogdownloader (12+ years old) shows the APIs are nonetheless remarkably stable. |

## Document index

| Doc | Contents |
|---|---|
| [01-gog-platform-and-apis.md](01-gog-platform-and-apis.md) | Domain landscape, API families, concepts (product IDs, depots, generations, CDNs) |
| [02-authentication.md](02-authentication.md) | Sign-in flows (all three observed variants), token lifecycle, storage, security notes |
| [03-library-and-game-metadata.md](03-library-and-game-metadata.md) | Owned-games listing endpoints, product/gameDetails JSON shapes, DLC/extras/movies |
| [04-download-paths-and-content-system.md](04-download-paths-and-content-system.md) | Classic installer path vs Galaxy depot path (v1/v2), manifests, secure links, CDN URL assembly |
| [05-download-operations.md](05-download-operations.md) | Concurrency, resume, verification, rate limiting/retry patterns observed in prior art |
| [06-configuration.md](06-configuration.md) | What existing tools make configurable; config file locations and precedence patterns |
| [07-rust-ecosystem.md](07-rust-ecosystem.md) | Relevant crates (versions verified 2026-10-08), async/HTTP/CLI/limits/storage options |
| [08-prior-art.md](08-prior-art.md) | Survey of existing GOG tools, what each does well, lessons learned |
| [09-risks-and-open-questions.md](09-risks-and-open-questions.md) | Instability, ToS/credentials, unknowns to verify with a live account before design |
| [10-scope-updates.md](10-scope-updates.md) | Owner decisions: CLI-first, gog-warp dependency, installers-only v1, ratatui TUI later |
| [11-open-questions-status.md](11-open-questions-status.md) | Resolution matrix for the open questions (most resolved from sources) + new findings |
| [12-live-verification-runbook.md](12-live-verification-runbook.md) | Live API probe script (`scripts/probe.sh`) for the account-dependent checks |

➡️ **Design phase:** [`docs/design/`](design/README.md) — architecture, auth, CLI spec,
download pipeline, config, state, errors, testing, and the v2 (depots + TUI) roadmap.

## Source shortlist

- Community API reference: <https://gogapidocs.readthedocs.io> (repo: Yepoleb/gogapidocs; the older `gogapidocs.org` is dead)
- GOG official Galaxy SDK docs (for context, not library access): <https://docs.gog.com/galaxyapi/>
- LGOGDownloader: <https://github.com/Sude-/lgogdownloader> (C++, reference implementation)
- heroic-gogdl (Python, Heroic's downloader module): <https://github.com/Heroic-Games-Launcher/heroic-gogdl>
- gog-warp (Rust library): <https://github.com/playtron-os/gog-warp>
- RikudouSage/GogDownloader (PHP): <https://github.com/RikudouSage/GogDownloader>
- gogg (Go): <https://github.com/habedi/gogg>
- galaxy-integrations-python-api (GOG's official plugin sample): <https://github.com/gogcom/galaxy-integrations-python-api>
