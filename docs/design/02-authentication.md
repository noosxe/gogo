# 02 — Authentication

Model: GOG Galaxy's public OAuth client — the same credentials every third-party GOG tool uses;
they are public (embedded in the Galaxy client itself and reproduced across all prior art in
docs/02/docs/08).

## 1. Constants

```
client_id     = 46899977096215655
client_secret = 9d85c43b1482497dbbce61f6e4aa173a433796eeae2ca8c5f6129f2dc4de46d9
redirect_uri  = https://embed.gog.com/on_login_success?origin=client
authorize     = https://auth.gog.com/auth
scope         = (empty — the Galaxy client requests no scoped subset)
```

The token exchange itself happens inside gog-warp (`get_token_with_code`); we never implement
the token endpoint call.

Both constants are public, not secrets (05 §6 untouched): they stay overridable via optional
`[auth] client_id` / `client_secret` config keys (05 §3, review C3) so a GOG-side rotation
doesn't require a binary update. Implementation note: gog-warp's `Core` pins its own
constants — if it can't take injected ones, vendor the two files we use (09 §4) or
hand-roll the exchange behind `Session`; decide at implementation time.

## 2. Login flow (`gogo login`)

1. Build the authorize URL (query-encoded):
   ```
   https://auth.gog.com/auth?client_id=46899977096215655
     &redirect_uri=https%3A%2F%2Fembed.gog.com%2Fon_login_success%3Forigin%3Dclient
     &response_type=code&layout=client&scope=
   ```
2. Print it and best-effort open the browser (`xdg-open` / `open` / `cmd /c start` via
   `std::process` — no extra crate; failure to open is fine, the URL is printed).
3. Prompt (dialoguer input): *"Paste the URL you were redirected to (it contains ?code=…)"*.
   The user logs in (2FA/CAPTCHA happen in the browser) and copies the address bar:
   `https://embed.gog.com/on_login_success?origin=client&code=<CODE>`
4. Parse with `url::Url`: require host `embed.gog.com`, path `/on_login_success`; extract
   `code`. Anything else → re-prompt (max 3 attempts) → `AuthError::LoginAborted` (exit 3).
5. `Session::login(code)` → gog-warp token exchange → tokens held in `Core`.
6. Fetch user data → print `username`, `user_id`, `country`, `preferredLanguage`.
7. Persist: `TokenStore::save(core.serialize_tokens(), user_id)`.

Non-interactive paths:

- `gogo login --code <code>` — paste the code directly (scripted setup, CI).
- `gogo login --print-url` — print URL, exit 0 (the operator opens it elsewhere).
- Already logged in → error unless `--force` (which overwrites the stored session).

## 3. Token storage — `TokenStore`

The persisted value is gog-warp's `serialize_tokens()` JSON blob (`HashMap<client_id, Token>`)
stored **verbatim** — gog-warp owns the format, we own the at-rest location.

```rust
#[async_trait::async_trait]        // (sync trait is fine; async only for keyring blockers)
pub trait TokenStore: Send + Sync {
    fn load(&self) -> Result<Option<String>, AuthError>;
    fn save(&self, blob: &str, user_id: Option<u64>) -> Result<(), AuthError>;
    fn clear(&self) -> Result<(), AuthError>;
}
```

Backends, selected by `auth.token_store`:

- **`keyring`** (default): service `"gogo"`, account = `user_id` or `"default"` before we know
  it. **No silent fallback** (review A6, owner decision): the config decides where tokens
  live, and that decision is honored exactly. If the platform secret service is unavailable
  (headless Linux without Secret Service), fail loudly — `AuthError::Store`, exit 3 — with
  the fix in the message: `gogo config set auth.token_store file`. `gogo login` probes the
  store *before* opening the browser, so a broken store can't waste a login. Switching
  `token_store` never migrates tokens: re-login on the new backend.
- **`file`**: `$XDG_STATE_HOME/gogo/tokens.json` (platform dirs per 06 §1) — tokens are
  machine-managed, non-portable state, not user-edited config; the XDG basedir spec lists
  credentials under state, and they sit next to `state.json`. Created `0600`,
  parent dirs `0700`. Before writing: if the existing file's permissions are wider than 0600,
  tighten and warn.

In-memory handling: the blob and extracted tokens are kept in zeroize-on-drop newtypes; no
`Debug`/`Display` impls on anything token-bearing; JSON dumps of API types are produced from
structs that never include token fields.

`gogo logout` → `TokenStore::clear()` on the **configured** backend; `--all-stores` clears
both (defensive, e.g. after switching backends); `--purge-state` also deletes `state.json`,
the catalog cache, and `verify-cache/` (06 §3).

## 4. Session lifecycle

- Every command that needs auth: `Session::restore(store)`. Failure → exit 3 with the hint
  *"not logged in — run `gogo login`"*. Commands that don't need auth (`config path`,
  `--help`, `login`, `--version`) work without a session.
- `bearer()` is called **per API request** (not cached by us — review B2): gog-warp caches
  the token and refreshes lazily inside `obtain_galaxy_token()` when `login_time +
  expires_in` has passed (live: `expires_in = 3600` exactly), so multi-hour runs refresh
  transparently. The 401 path below stays a safety net (clock skew, revocation).
- After any refresh, compare `serialize_tokens()` with the stored snapshot; changed → re-save.
  Live finding: the refresh_token does **not** rotate (docs/11), but this handles rotation
  should GOG change it.
- 401 during an API call: refresh once, retry the call. Second 401 → `AuthError::Unauthorized`,
  exit 3, hint to re-login. No retry loops.
- Clock skew: gog-warp tracks expiry locally (`login_time + expires_in`); if the system clock
  jumps, a 401 triggers the refresh path anyway — self-correcting.

## 5. Security notes

- The pasted redirect URL contains a one-time code: never echoed at info level; debug logs
  redact `code=…` query values (07 §4).
- CDN URLs embed access tokens in their path (`/token=…~dirs=…/`): always redacted in logs and
  error messages; CDN URLs are never written to disk (sidecars carry offsets only — 04 §5).
- `tokens.json` is `0600`; keyring entries are namespaced under service `gogo`.
- No telemetry, no crash reporting, no DNS-visible calls except GOG endpoints.
