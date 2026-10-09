# 01 — GOG Platform & API Landscape

Discovery notes. Everything here is **unofficial** unless explicitly marked otherwise.

## 1. There is no official client API

GOG offers the "Galaxy API" (<https://docs.gog.com/galaxyapi/>) but that is the **in-game SDK**
for game developers (achievements, multiplayer, etc.) and requires per-game client credentials
provisioned by GOG. There is no official, documented REST API for third-party apps to access a
user's library or downloads. Every working tool (GOG Galaxy itself aside) uses the same
unofficial web/Galaxy endpoints that the GOG Galaxy 2.0 client uses. The community reference is
<https://gogapidocs.readthedocs.io> (maintained by Yepoleb, author of pygogapi). Note:
`gogapidocs.org` (custom domain) is dead; use the readthedocs.io domain.

Practical consequence: the API surface is stable in practice (lgogdownloader has worked since
~2014) but **not contractually stable**. The app should treat endpoint shapes as data, not
compile-time gospel, and fail gracefully (see 09-risks).

## 2. Domain landscape

Verified against `gogdl/constants.py` (Heroic) and `gog-warp/src/constants/domains.rs`:

| Domain | Purpose | Auth |
|---|---|---|
| `auth.gog.com` | OAuth2 authorize + token endpoints | public client_id/secret |
| `login.gog.com` | HTML login page (email/password, 2FA forms, CAPTCHA) | browser session |
| `www.gog.com` | Store site; also serves installer downlinks (`/downlink/...`) | session cookies (and Bearer works for downlink JSON per lgogdownloader's usage) |
| `embed.gog.com` | "Embedded" web API used by the site itself: owned-games list, gameDetails, wishlist, filtered products | Bearer token (confirmed working; historically cookies) |
| `api.gog.com` | Public catalog API (`/products/{id}?expand=...`), also returns owned download data when authorized | Bearer for owned fields |
| `content-system.gog.com` | Build lists, secure links, dependency repository | Bearer token |
| `cdn.gog.com` | Content-system manifests (v1 manifests, v2 meta) | mostly public URLs |
| `gog-cdn-fastly.gog.com` (and peers, e.g. Lumen/Cloudflare endpoints returned in secure-link responses) | Actual file downloads (chunks, installers) | tokenized URLs, no extra auth |
| `galaxy-library.gog.com` | Galaxy 2.x library sync (`/users/{user_id}/releases`) | Bearer |
| `cloudstorage.gog.com` | Cloud saves | Bearer |
| `remote-config.gog.com` | Galaxy client remote config (lgogdownloader reads it for cloud paths) | public |
| `gamesdb.gog.com`, `presence.gog.com`, `gameplay.gog.com`, `users.gog.com`, `chat.gog.com`, `cfg.gog.com` | Galaxy social/gameplay services | not needed for downloads |
| `af.gog.com` | Affiliate link rewriting | n/a |

## 3. Core concepts

- **Product ID**: stable numeric ID for every GOG product (game, movie, DLC). Example: The
  Witcher = `1207658924`, Cyberpunk 2077 = `1423049311`. Slugs (`the_witcher_3_wild_hunt`) are
  used in URLs/filenames but IDs are the real keys.
- **gameDetails vs productInfo**: two views per product — the authenticated account view
  (`embed.gog.com/account/gameDetails/{id}.json`, human-oriented: "English", "1.4 GB") and the
  catalog view (`api.gog.com/products/{id}?expand=downloads,...`, machine-oriented: exact
  download URLs, versions, checksums). lgogdownloader builds its file lists from the latter.
- **Offline installers** (user's target): per-language/per-platform setup executables
  (`setup_*.exe`, `.pkg`/dmg for mac, mojosetup `.sh` for Linux), plus **extras** (soundtracks,
  manuals, wallpapers), **patches**, **language packs**, and DLC installers.
- **Galaxy depots**: the Galaxy 2.x content model (what "install from Galaxy" uses). Not
  installers — raw game files distributed as chunked depots in two generations:
  - **Generation 1** ("v1"): files inside one big `main.bin` store, uncompressed, chunked
    downloads via HTTP Range.
  - **Generation 2** ("v2"): files split into zlib-compressed chunks, each chunk stored on the
    CDN under its MD5; JSON manifests describe layout. Adds binary **patches (xdelta3)** and
    shared **dependencies** depots (directx, vc redist, etc.).
- **Builds**: versioned snapshots of depots per platform (`builds` endpoint), with optional
  beta branches (password-protected) and `public` flag.
- **Secure links**: short-lived CDN URL templates (`url_format` + parameters incl. token +
  expiry), multiple mirrored endpoints with `priority` and failover hints.
- **downloaderUrl** (`gogdownloader://...`): legacy URI scheme for the retired official GOG
  Downloader app — irrelevant to us, but appears throughout gameDetails JSON.

## 4. Observed API stability

- The OAuth endpoints, `/user/data/games`, `/products/{id}`, `secure_link`, and `builds` have
  been stable across lgogdownloader (2014→now), gogdl (2017→now), and gog-warp (2024→now).
- The Galaxy 2.x additions (v2 depots, galaxy-library.gog.com, xdelta patches) are newer but
  already mirrored by three independent implementations.
- Fields like `galaxy_downlink_json_url` inside download entries are Galaxy-era additions to
  the products response; lgogdownloader depends on them today.
