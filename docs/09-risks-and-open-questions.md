# 09 — Risks, Unknowns & Open Questions

Everything below is a finding about **uncertainty**, recorded so the design phase can plan
verification. Nothing here has been decided.

## 1. Environmental / legal risks

| Risk | Detail | Mitigation space (for design, TBD) |
|---|---|---|
| Unofficial API | No contract; endpoints/shapes can change without notice. Historically very stable (12+ yrs of lgogdownloader), but Galaxy 2.x additions are newer. | Loose parsing (`#[serde(default)]`), versioned metadata cache, actionable errors pointing at refresh/re-login. |
| Reused client credentials | All tools ship Galaxy's public `client_id`/`secret`. No third party has ever been blocked to our knowledge, but it is formally GOG's goodwill. | Keep credentials overridable via config; honor `User-Agent` identification; conservative request rates. |
| ToS / EULA | Downloading your purchases for offline use is a core GOG promise (DRM-free). Automation is tolerated in practice (many OSS tools). | Personal-use framing, no scraping of store data, respect rate limits. |
| Login CAPTCHA | gogapidocs warns the login page may require CAPTCHA — kills headless logins intermittently. | Browser-assisted code paste as primary flow (CAPTCHAs happen inside the user's browser). |
| Account lockout | Aggressive automation could theoretically trigger fraud controls. | Rate limiting defaults on the safe side; no credential-stuffing features. |
| Token revocation | Refresh tokens die on password change/logout; ~72h access tokens. | Transparent refresh; clean `login` command; clear error message on 401-after-refresh. |

## 2. Technical unknowns to verify with a live account (pre-design spike)

> **Status (2026-10-08, final):** items 1–5 and 7–9 are **resolved and live-verified** (doc 11);
> item 6 resolved to first order (no throttle signals in a gentle burst; conservative defaults
> still planned); item 10 deferred with the Model B scope. The runbook (doc 12) has been
> executed; §2 below is kept as the original question list for the record.
1. **`expires_in` exact value** for access tokens (72h assumed from community knowledge).
2. **Does Bearer auth work on all embed.gog.com account endpoints** (`/account/gameDetails`,
   `/account/getFilteredProducts`)? gogdl sends Bearer and works, but headers vs cookies has
   changed historically.
3. **`galaxy_downlink_json_url` availability** on `api.gog.com/products/{id}?expand=downloads`
   with a user token, and whether its `{downlink, checksum}` response works without cookies.
   (lgogdownloader does exactly this — confirm from our own client.)
4. **`manualUrl` (`/downlink/...`) auth mode**: session cookies only, or Bearer too?
5. **Checksum XML schema** for installers (exact fields: md5, size, total_size, chunks?) —
   lgogdownloader parses `<file ... md5=...>` elements; need one live sample.
6. **Rate-limit behavior**: soft limits (429? dynamic throttling? CDN-level?) — not documented
   anywhere; test gently.
7. **Multi-part installer naming/updates**: how `version`/`date` change on re-release; needed
   for "only newer files" logic (gogg/GogDownloader both implement heuristics worth studying).
8. **Movies**: whether they must be excluded or supported (they appear in `owned`).
9. **DLC installers**: delivered via parent's `downloads` + DLC product IDs; confirm
   `expanded_all_products_url` gives everything in one call (lgogdownloader relies on it).
10. **Beta branches / password-protected builds** — only relevant if Model B ever enters scope.

## 3. Scope questions the design phase must answer (not decided here)

- **Model A only, or A + B?** The requirement says "offline installers" → Model A. Model B
  (depots) would add: zlib chunks, secure links, xdelta3, dependencies, builds — either as v2
  scope or via depending on `gog-warp`.
- **Token storage**: keychain vs config file vs both (portability vs security tradeoff).
- **Config format & precedence**: TOML+layers is the ecosystem default but undecided.
- **Metadata cache**: JSON files vs SQLite (GogDownloader's DB enables powerful `--updated-only`
  but adds a dependency).
- **Output layout**: flat `Title/` vs platform/language subdirs vs template strings
  (lgogdownloader/gogg precedents).
- **Rate-limit config surface**: bytes/sec only, or also max-concurrent-requests/chunks.
- **Distribution**: crates.io + GitHub releases + (later?) package managers.

## 4. Known traps from prior art (carry into design review)

- Never retry HTTP 416 (it means "resume complete"); do retry 5xx; 401 → refresh → retry once.
- Persist resume state atomically (crash-safe) — chunk state files, not in-memory only.
- `secure_link` responses expire: refetch on 403 from CDN; cache endpoints per product like
  gogdl does.
- Handle zero-length/partial files after failed resume: delete and restart (lgogdownloader).
- Always check free disk space before starting (both gogdl and gog-warp do).
- Parse sizes defensively: gameDetails `size` fields are display strings, not bytes.

## 5. Where discovery stopped

- No live API calls were made with a real account (discovery was doc/source-level only).
- gog-warp's `content_system/downloader/{worker,verify,patching}.rs` internals were skimmed,
  not fully traced — do a deep read if Model B enters scope.
- lgogdownloader's man page (`man/lgogdownloader.1`) contains the complete flag surface —
  worth a full read during design for CLI vocabulary.
