# 02 — Authentication (Sign-In Flow) Research

All findings verified against live implementations: `gog-warp/src/auth/` (Rust),
`heroic-gogdl/gogdl/auth.py` (Python), `lgogdownloader/src/website.cpp` + `galaxyapi.cpp` (C++),
and <https://gogapidocs.readthedocs.io/en/latest/auth.html>.

## 1. The mechanism: OAuth2 authorization-code

GOG uses OAuth2, but with quirks:

- **The token endpoint is called with HTTP GET**, parameters in the query string (not the usual
  POST form body). Confirmed in all three implementations.
- **You cannot register your own redirect URI.** Every third-party tool reuses the GOG Galaxy
  client credentials and their redirect URI. Therefore a localhost-callback flow is not
  possible; the CLI convention is **open browser → user logs in → copy `code=` from the
  redirect URL → paste into terminal**.

### Client credentials (public, reused by all tools)

```
client_id     = 46899977096215655
client_secret = 9d85c43b1482497dbbce61f6e4aa173a433796eeae2ca8c5f6129f2dc4de46d9
redirect_uri  = https://embed.gog.com/on_login_success?origin=client
```

Sources: `heroic-gogdl/gogdl/auth.py` (`CLIENT_ID`, `CLIENT_SECRET`, `CODE_URL`),
`gog-warp/src/auth/mod.rs`. An older Galaxy secret (`9d85c43b148649baa8e4a8ad9a7bad99`) still
circulates in older sample code (galaxy-integrations era); the long-form one above is what
current, working tools ship. These credentials are embedded in every GOG Galaxy installation —
they are not secrets in any meaningful sense, but see 09-risks for the ToS angle.

## 2. Endpoints

| Purpose | Method & URL |
|---|---|
| Open login page in browser | `GET https://auth.gog.com/auth?client_id={id}&redirect_uri={redirect}&response_type=code&layout=client2` |
| Exchange code → token | `GET https://auth.gog.com/token?client_id={id}&client_secret={secret}&grant_type=authorization_code&redirect_uri={redirect}&code={code}` |
| Refresh token | `GET https://auth.gog.com/token?client_id={id}&client_secret={secret}&grant_type=refresh_token&refresh_token={token}` |

Notes:

- `layout` values seen: `client2` (gog-warp, pygogapi docs), `client` (older tools),
  `default&brand=gog` (lgogdownloader, which drives the page headlessly). `client2` is the
  current recommendation in the community docs.
- After login (incl. any 2FA) the browser lands on
  `https://embed.gog.com/on_login_success?origin=client&code=XXXX...` — the code is everything
  after `code=`. The page itself renders a blank/client-themed page; the user just copies the
  URL.
- Example authorize URL from `gog-warp/examples/auth.rs`:
  `https://auth.gog.com/auth?client_id=46899977096215655&redirect_uri=https://embed.gog.com/on_login_success?origin=client&response_type=code&layout=client2`

## 3. Token response shape

From `gog-warp/src/auth/types.rs` (matches heroic-gogdl's credentials file):

```json
{
  "access_token":  "...long JWT...",
  "refresh_token": "...opaque...",
  "user_id":       "48628349971017",
  "expires_in":    259200,
  "token_type":    "Bearer",
  "scope":         "client",
  "session_id":    "..."
}
```

- `expires_in` is seconds; **about 1 hour (3600)** — gogapidocs: *"Renew the token when it
  expires after about an hour. Check the original response for an accurate lifetime"*; gog-warp's
  tests use `3600`. Refresh tokens are long-lived but revocable (password change, logout).
  Tools track `login_time` locally (gog-warp stores `login_time`; heroic-gogdl stores
  `loginTime` + `expires_in` in its credentials JSON and compares `time.time()`).
- When expired, tools transparently refresh using the `refresh_token` grant; refresh tokens are
  long-lived but revocable (e.g. password change, manual logout). A failed refresh = re-login.
- `user_id` from the token is needed for `galaxy-library.gog.com/users/{user_id}/releases`.
- `Authorization: Bearer <access_token>` header is sent **per request** on api/embed/
  content-system endpoints.

## 4. Two-factor authentication (2FA)

GOG supports email-code ("two-step") and authenticator-app TOTP ("two-factor"). Two ways this
is handled:

1. **Browser-assisted flow (recommended for a CLI):** 2FA happens inside the browser naturally.
   The CLI never sees it. This is what gog-warp, gogg, and RikudouSage/GogDownloader do, and it
   sidesteps CAPTCHAs as well (see §5).
2. **Headless form login (lgogdownloader's approach):** POST the login forms directly:
   - `GET` the login page, scrape CSRF `login[_token]` from the HTML.
   - `POST https://login.gog.com/login_check` with username/password + CSRF token.
   - If the redirect URL contains `two_step` → `https://login.gog.com/login/two_step`
     (email code, 4 letters): POST `second_step_authentication[token][letter_1..4]` +
     `second_step_authentication[_token]` + `second_step_authentication[send]`.
   - TOTP variant: `https://login.gog.com/login/two_factor/totp` with
     `two_factor_totp_authentication[token][letter_1..6]`.
   - After session cookies are established, lgogdownloader visits
     `https://auth.gog.com/auth?...` **with those cookies**; the server 302s to the redirect URI
     with `code=` (an SSO bridge), then exchanges the code as usual.

   This headless path is brittle (HTML scraping + CAPTCHA risk + 2FA form churn). The community
   docs explicitly warn the login page may require a CAPTCHA. → Treat as fallback, not primary.

## 5. Flow options for our CLI (observed, not yet chosen)

| Option | Used by | Pros | Cons |
|---|---|---|---|
| **Browser + paste code** | gog-warp, gogg, RikudouSage/GogDownloader, heroic-gogdl CLI mode | Robust, handles CAPTCHA/2FA naturally, no scraping | One manual copy/paste step |
| Embedded webview | minigalaxy, Heroic GUI | Smooth UX | GUI dependency, not CLI-friendly |
| Headless form login | lgogdownloader | Fully non-interactive once configured | Brittle, CAPTCHA, must reimplement 2FA forms, stores password |
| Refresh-token only | all (subsequent runs) | Zero-friction re-auth until revoked | Needs initial login anyway |

Observation for the future design phase: gogg additionally supports storing credentials so
downloads are cron-scriptable; RikudouSage/GogDownloader has both `login` (interactive) and
`login-code` (paste a code you obtained elsewhere) commands — a nice split.

## 6. Token storage (observed)

- **Plain JSON file**: heroic-gogdl (credentials JSON in config dir, keyed by client_id), 
  gog-warp example (`.gog.token`), lgogdownloader (config + cookies file).
- **OS keychain**: nicohman/wyvern (Rust) used the `gog` crate's token handling; the `keyring`
  crate (macOS Keychain / Windows Credential Manager / Linux Secret Service) is the standard
  Rust route. (gog-secrets, a related crate family, offers file/keychain/secret-service
  backends.)
- Security considerations recorded for design: file perms 0600, `zeroize` secrets in memory,
  never log tokens, store refresh token only if user opts into "remember me".

## 7. Request authorization cheat-sheet

| Endpoint family | Header |
|---|---|
| `embed.gog.com` (`/user/data/games`, `/account/gameDetails/...`, `/account/getFilteredProducts`) | `Authorization: Bearer ...` (works; historically also session cookies) |
| `api.gog.com` (`/products/...`) | Public without auth; `Bearer` unlocks owned download fields (`galaxy_downlink_json_url`) |
| `content-system.gog.com` (`secure_link`, `builds`) | `Bearer` required |
| `cdn.gog.com` manifests | none (public URLs) |
| Download CDNs | none (URLs are pre-tokenized with expiry) |
| `www.gog.com/downlink/...` / downlink JSON | **Bearer confirmed** (minigalaxy, gogg, lgogdownloader — doc 11); live re-verification in doc 12 |

Sources: gogapidocs auth.html ("Authorizing a request", token per call), gogdl `api.py`
(session-wide Bearer header), lgogdownloader `website.cpp` (cookie jar + token JSON).
