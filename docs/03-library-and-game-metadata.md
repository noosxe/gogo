# 03 — Library Listing & Game Metadata Research

How existing tools list purchased games and obtain per-game download metadata.
Sources: gogapidocs account.html/listing.html, heroic-gogdl `api.py`, lgogdownloader
`galaxyapi.cpp`/`downloader.cpp`, gog-warp `library/`, RikudouSage/GogDownloader.

## 1. "What games do I own?"

### a) Simple owned-ID list (fastest)

```
GET https://embed.gog.com/user/data/games
Authorization: Bearer <token>
```

Response: `{ "owned": [1207658691, 1207658713, ...] }` — flat array of product IDs.
Used by gogdl (`does_user_own`) and gog-warp (`get_owned_licenses`). One request for the whole
library; details must then be fetched per ID.

### b) Rich, paginated library listing

```
GET https://embed.gog.com/account/getFilteredProducts?mediaType=1&page=1&sortBy=title
```

Response (from gogapidocs listing.html):

```json
{
  "sortBy": "date_purchased", "page": 1,
  "totalProducts": 1, "totalPages": 1, "productsPerPage": 100,
  "moviesCount": 1,
  "tags": [{"id": "...", "name": "COMPLETED", "productCount": "0"}],
  "products": [ { "id": 1207660413, "title": "...", "slug": "...",
                  "availability": {"isAvailable": true, "isAvailableInAccount": true}, ... } ]
}
```

Parameters: `mediaType` (1=games, 2=movies), `search`, `sortBy`, `page`, `system`, `language`,
`tags`. The 2026-10-08 capture also recorded a `hiddenFlag` **query param** (purpose
unverified; a 2026-10-09 probe of six other candidate params — `hidden`, `showHidden`,
`isHidden`, `hideHidden`, `filter` — surfaced nothing). Response carries
`totalPages`/`totalProducts`, `hasHiddenProducts`, and per-product
`isHidden`/`isHidingDisabled` (no `hiddenFlag` *field* exists). Default query returns all
83 products visible, 0 `isHidden`, `hasHiddenProducts=true` — hidden products are
excluded by default. This is how the GOG website renders the library; gives
titles/slugs/covers without per-ID round trips.

### c) Galaxy 2.x library sync (modern, what the Galaxy client uses)

```
GET https://galaxy-library.gog.com/users/{user_id}/releases?...
```

Paged with `next_page_token`; items carry `platform_id` (big enum incl. non-GOG stores),
`external_id`, `owned`, `hidden`, `owned_since`, `certificate`. Implemented in gog-warp
(`src/library/types.rs`). More machinery than needed for our use case but noted for
completeness.

### d) User identity

```
GET https://embed.gog.com/userData.json
```

Returns `userId`, `username`, `email`, `country`, currencies, friend/chat counters, etc.
Needed to resolve `user_id` for (c). lgogdownloader calls this (`galaxyapi.cpp:603`).

## 2. Per-game metadata: the two views

### a) Catalog view — `api.gog.com/products/{id}`

```
GET https://api.gog.com/products/{id}?expand=downloads,description,screenshots,videos,related_products,changelog
```

- Public for store info; **with Bearer it includes the user's download entries** — notably
  `downloads.installer[]` items carrying `galaxy_downlink_json_url` (see doc 04).
- DLCs: response has `dlcs.products` (IDs) and `dlcs.expanded_all_products_url` (a single
  pre-built URL returning all DLC product objects — lgogdownloader uses it; there is also a
  legacy `GET /products?ids=1,2,3&expand=...` batch form).
- This is the view lgogdownloader parses into its internal `gameDetails` model
  (`productInfoJsonToGameDetails`).
- **Live-verified shape (2026-10-08):** with Bearer, `downloads` = `{installers[], patches[],
  bonus_content[], language_packs[]}`; each entry `{id, name, os, language, language_full,
  version, total_size (exact bytes), files[{id, size, downlink}]}` where `downlink` is an
  api.gog.com endpoint returning `{downlink, checksum}` (doc 04 §1.1). No `updated`/`date`
  fields in the modern API.

### b) Account view — `embed.gog.com/account/gameDetails/{id}.json`

Human-oriented shape (from gogapidocs account.html):

```json
{
  "title": "Unreal Tournament 2004 Editor's Choice Edition",
  "backgroundImage": "//images-4.gog.com/...",
  "cdKey": "",
  "downloads": [
    ["English", { "windows": [
      { "manualUrl": "/downlink/unreal_tournament_2004_ece/en1installer3",
        "downloaderUrl": "gogdownloader://unreal_tournament_2004_ece/installer_win_en",
        "name": "... (Part 1 of 3)", "version": null, "date": "", "size": "1 MB" }
    ]}]
  ],
  "extras": [
    { "manualUrl": "/downlink/file/the_art_of_playing/34143",
      "downloaderUrl": "gogdownloader://the_art_of_playing/34143",
      "name": "wallpaper", "type": "wallpapers", "info": 1, "size": "1 MB" }
  ]
}
```

- `downloads` grouped **by language → by platform** for games; flat list for movies (with
  `playerUrl`).
- `extras`: soundtracks, manuals, wallpapers, artbooks — the "bonus content" everyone wants.
- Also carries `cdKey` (serial keys) and `changelog` in the catalog view.
- Sizes in the account view are display strings ("1.1 GB"); the products API gives exact bytes
  (`total_size`, `files[].size`), and the checksum XML is authoritative for the final file
  (doc 11 live notes).

### c) Wishlist / friends / settings

Account API also exposes wishlist, friends, privacy settings (gogapidocs account.html) —
out of scope for a downloader but documented for completeness.

## 3. What the mature tools actually do

| Tool | Library listing | Per-game details | Installer list source |
|---|---|---|---|
| lgogdownloader | `user/data/games` + `userData.json` | `api.gog.com/products/{id}?expand=downloads,...` (+ `expanded_all_products_url` for DLCs) | `downloads.installer[]` incl. `galaxy_downlink_json_url`; caches `product.json`/`game-details.json` locally |
| heroic-gogdl | `user/data/games` | `/products/{id}` (`get_item_data`) + `embed/account/gameDetails/{id}.json` (`get_game_details`) | Depot manifests (v1/v2), not installers; Linux native installer manager for Linux |
| gogg | `user/data/games` | products + gameDetails | installers, patches, extras with platform/language/content-type filters |
| RikudouSage/GogDownloader | paged listing → local **SQLite metadata DB** with migrations (`update-database`) | same endpoints | installers/extras/saves; `--os`, `--language`, `--search`, `--updated-only`, `--new-only` filters |

## 4. Observations relevant to a future design

- A **two-phase listing** (one `/user/data/games` call, then N product-detail calls) is the
  norm; N grows with library size, so every mature tool **caches metadata locally** (JSON files
  in lgogdownloader, SQLite in GogDownloader) and supports "refresh only new/updated".
- Language and platform are **per-installer dimensions**, not global settings: the same game
  can have separate Windows/Mac/Linux installers × English/German/... versions × multi-part
  files ("Part 1 of 3").
- Movies are products too and appear in `owned`; the app will need to decide whether to show
  them (they download the same way via manualUrl).
- DLC ownership lives under the parent product (`dlcs`, `expanded_dlcs`) and also appears as
  separate owned IDs in `/user/data/games`.
