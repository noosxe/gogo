# 22 — GOG cloud saves (#275)

Status: **accepted** (2026-11-13). Facts in §1 are live-verified; the decisions are in §12
(D1–D13, all accepted as recommended). Writes to the cloud wait for D2's live test (`push`/`sync` are built, §12f, and gated).

Sync a built game's saves with GOG's cloud storage, the same storage GOG Galaxy and Heroic
use, so saves follow the user between machines and launchers without copying them by hand.
`gogo saves status|pull|push|sync <gogid>` does it on request; the game's launcher can do it
around a launch (§6).

Scope: Windows builds in the **Linux lane** (`gogo linux-app`, Proton prefix, design 18) first,
then the **Mac lane** (`gogo mac-app`, Wine bottle, design 16); both run the Windows build, so
both read GOG's Windows save locations (§3). Native Linux and macOS builds come later (§11).

**Why its own doc.** Cloud saves cut across auth (design 02), the two lanes' prefixes and
launchers (designs 16, 18), snapshots (design 20 §6.3, which puts four requirements on this
design) and the kept build manifest (design 21 §5.2). They add their own state, a new
remote service with its own client credentials, and a sync algorithm whose mistakes lose
saves.

## 0. Summary

| Question | Answer |
|---|---|
| Auth | A **per-game token**: the game's `clientId`/`clientSecret` and the user's Galaxy refresh token, exchanged at `auth.gog.com/token` (`grant_type=refresh_token`, `without_new_session=1`). The Galaxy token itself is refused (403). gog-warp already does this exchange (`Core::obtain_token`). §2. |
| Client credentials | From the game's **build manifest** (v2, `clientId` + `clientSecret`): the kept `build-manifest.json` (design 21 §5.2) or a fresh fetch. `goggame-<id>.info` has `clientId` only. Never committed, printed, logged or stored anywhere new. §2.2. |
| Save locations | `remote-config.gog.com` (public): `content.Windows.cloudStorage.locations[]`, each a `name` (the cloud dir) and a `location` with `<?VAR?>` variables. Mapped onto the prefix's Windows user (`steamuser` in Proton, `crossover` in a CX bottle) and the game dir. §3. |
| Storage API | `cloudstorage.gog.com/v1/<userId>/<clientId>[/<name>/<path>]`: list (GET, JSON), download (GET), upload (PUT), delete. Objects are stored **gzip-compressed**; `hash` is the MD5 of the stored gzip bytes; the original mtime rides in `X-Object-Meta-LocalLastModified`. Deleted files stay listed as **tombstones** (an empty gzip). §1.3. |
| Sync model | Per-file, three-way: the cloud listing, the local files and gogo's record of the last sync. Content (hashes) decides what changed, never clock times. A file changed on both sides is a conflict and is asked about. §4, §5. |
| When | Manual commands first. Around a launch, per game and opt-in: pull before the game starts, push after it exits; offline never blocks a launch. §6. |
| Safety | Every local file a pull overwrites or deletes, and every cloud object a push overwrites or deletes, is copied to a backup dir first. `--dry-run` everywhere. A pull that overwrites takes an `auto` snapshot (`cloud-pull`); a rollback forces the next sync to ask. Snapshots never touch the cloud. §7. |
| Lanes | Linux lane first (both build lanes: depot and installer), Mac lane next, native builds later. §11. |

## 1. Facts (live, read-only, 2026-11-13)

Verified with gogo's own session (gog-warp `Core` restored from the token store, never
persisted, nothing written outside `target/`) against five owned games: Cyberpunk 2077
(1423049311), The Witcher 3 (1207664643 and 1495134320), XCOM 2 (1482002159) and Prey
(1158493447). Listings and two small objects were downloaded into a scratch dir. Nothing was
uploaded, deleted or changed in the cloud. No token or secret was printed.

### 1.1 Auth

- **The Galaxy token is refused.** `GET cloudstorage.gog.com/v1/<userId>/<clientId>` with
  the user's normal (Galaxy client) bearer gives **403** for every game tried.
- **A per-game token works.** `GET auth.gog.com/token?grant_type=refresh_token&client_id=
  <game clientId>&client_secret=<game clientSecret>&refresh_token=<Galaxy refresh
  token>&without_new_session=1` returns a bearer token for that client: `expires_in` 3600,
  `token_type` bearer, no `scope` field, the same `user_id` as the Galaxy token, and its own
  refresh token. No scope parameter is needed. gog-warp's `Core::obtain_token(client_id,
  client_secret)` sends exactly this (pinned rev 26c9e67, `auth::get_token_for`, which adds
  `without_new_session=1` for any client but Galaxy's own, so the user's session isn't
  rotated) and caches the result in its token map.
- **The exchange checks the license.** For a product the account doesn't own, it answers
  **403** `{"error":"no_license", …}`. The Witcher 3 shows the trap: GOG lists both
  1207664643 and 1495134320 with builds and **different** `clientId`s; the account owns only
  the second, and only its exchange works. The product id that counts is the built game's
  `gogid` (the one whose build manifest gogo used), never a pack id (1207664663, the
  Complete Edition pack, has no builds at all).
- `userId` in the storage path is the token response's `user_id`.

### 1.2 Client credentials

- Every v2 build manifest read (all five games, Windows; Cyberpunk on macOS too) has
  top-level `clientId` (17 chars) and `clientSecret` (64 chars). Cyberpunk's Windows and
  macOS builds share one `clientId`.
- `goggame-<id>.info` (in the game's files) has `clientId` but **no secret**
  (keys: `buildId, clientId, gameId, language, languages, name, osBitness, playTasks,
  rootGameId, version`).
- gogo already keeps the manifest in the game dir as `build-manifest.json` (mode 0600, design
  21 §5.2, depot lane) and never prints it; nothing reads its client fields yet.

### 1.3 Storage API

**User-Agent.** The storage API checks it. With gogo's own UA (`gogo/<version>`) Cyberpunk's
list gives **400** `{"error":"invalid_version","error_description":"not supported with this
version"}`; XCOM 2's list, an empty container (`x-gog-backend: empty-swift`), gave 200 with
the same UA, so the check isn't reached for every container. Of the UAs tried on Cyberpunk,
only the Galaxy communication-service form used by Heroic and Lutris passed:
`GOGGalaxyCommunicationService/2.0.13.27 (Windows_32bit) dont_sync_marker/true
installation_source/gog`, also with ` gogo/<version>` appended. Without the
`dont_sync_marker/true installation_source/gog` suffix it fails, with or without the
platform part, and so do other versions with the platform part alone. (Narrowing it further
was not done; D3.)

**List.** `GET /v1/<userId>/<clientId>` with `Accept: application/json` → a flat JSON array
over all locations; without `Accept` → `text/plain`, one name per line. Each entry:

| Field | Example (shape) | Meaning |
|---|---|---|
| `name` | `saves/ManualSave-N/sav.dat` | `<location name>/<path relative to the location>`, `/`-separated |
| `bytes` | `1350` | size of the **stored (gzip)** object |
| `hash` | 32 hex | MD5 of the stored gzip bytes (= the object's `ETag`) |
| `last_modified` | `2024-02-11T03:49:20.911250+00:00` | server time of the upload, microseconds |
| `content_type` | `application/octet-stream` | as uploaded |

Response headers carry the container's state: `x-container-object-count`,
`x-container-bytes-used`, `x-container-meta-quota-bytes`, and per location
`x-container-meta-locallastmodified-<name>` (also hex-encoded: `…-7361766573` is `saves`), an
HTTP date. It is an OpenStack Swift container behind GOG's front end (`x-trans-id`,
`x-storage-policy`).

**Download.** `GET /v1/<userId>/<clientId>/<name>` → the stored bytes, gzip (magic `1f 8b`),
with `Content-Encoding: gzip`, `ETag` (= listed `hash`), `Last-Modified` (server),
`X-Object-Meta-LocalLastModified` (the file's mtime as the uploader saw it, RFC 3339, seconds,
UTC) and `X-Object-Meta-User-Agent` (the uploading client). MD5 of the received bytes equals
the listed `hash`; gunzipped they are the save file. `HEAD` gives the same headers. Pitfall:
an HTTP client that decodes `Content-Encoding: gzip` on its own hands over the plain file,
whose MD5 no longer matches `hash`; gogo's reqwest is built without the `gzip` feature (one
TLS stack, `Cargo.toml`), so it gets the stored bytes.

**Tombstones.** Of Cyberpunk's 231 listed objects, **168** have the hash of an **empty gzip
stream** (`aadd86936a80ee8a369579c3926f1b3c`, 89 bytes as Galaxy writes it, no `X-Object-Meta-LocalLastModified`):
files deleted locally by a client that then marked them deleted. They stay in the listing;
every client treats them as "deleted" (§4.2). Cyberpunk rotates its autosaves, so most of its
history is tombstones.

**Not verified (writes, need an OK first; D2):** upload and delete. From prior art (§9):
`PUT` the gzip body with `Content-Encoding: gzip`, `Etag: <md5 of the gzip body>`,
`X-Object-Meta-LocalLastModified: <mtime, RFC 3339 UTC, seconds>`; `DELETE` the object URL.
Whether Galaxy writes tombstones by `PUT`ting an empty gzip or the server turns a `DELETE`
into one is unknown.

**Quota.** Two numbers that disagree: remote config's `cloudStorage.quota` (Cyberpunk
2 097 152 000, others 209 715 200) and the container's `x-container-meta-quota-bytes`
(Cyberpunk 209 715 200, Witcher 3 Remastered 2 147 483 648). Which one the server enforces
is unknown; gogo uses the smaller as a pre-check only (§5.4).

### 1.4 Save locations

`GET remote-config.gog.com/components/galaxy_client/clients/<clientId>?component_version=2.0.45`,
no auth → JSON with `content.<Windows|MacOS|Linux>.cloudStorage { enabled, locations: [{name,
location}] }` and `content.cloudStorage.quota`.

| Game | Windows locations | MacOS |
|---|---|---|
| Cyberpunk 2077 | `saves` → `<?SAVED_GAMES?>/CD Projekt Red/Cyberpunk 2077` | enabled false |
| The Witcher 3 (both ids) | `saves` → `<?DOCUMENTS?>\\The Witcher 3` | enabled false |
| XCOM 2 | `saves`, `saves_wotc`, `saves_pool`, `saves_pool_wotc`, `saves_photo_wotc` → five dirs under `<?DOCUMENTS?>/My Games/…` | enabled false |
| Prey | `saves` → `<?SAVED_GAMES?>/Arkane Studios/Prey` | — |

None of the five has a `Linux` key. Separators are mixed: `/`, or (Witcher 3) a doubled
backslash. When `locations` is empty but storage is enabled, every client falls back to a
default location, `__default` (§3.3). The `name` is the first path segment of every object
under that location.

## 2. Auth and credentials at runtime

### 2.1 The token

1. gogo restores the session as every command does (design 02).
2. It gets the game's `clientId`/`clientSecret` (§2.2).
3. It exchanges them for a per-game token with gog-warp's `obtain_token`, **in a separate,
   short-lived `gog_warp::Core`** seeded from the session with only the Galaxy token. The
   main session's `persist_if_changed` serializes every token in its map, so a per-game
   token obtained there would be written into the token store (keychain or file) at the next
   save; a separate `Core` keeps it in memory for this command only. One exchange per
   command per game is cheap (one request) and avoids a new kind of stored secret (D4).
4. If the Galaxy token needs a refresh, it is refreshed in the **main** session first
   (`bearer()`), which persists it as usual; the separate `Core` never refreshes the Galaxy
   token itself.
5. `no_license` → exit 3 with the product id and a hint (`gogo saves` uses the built game's
   gogid; a pack or another edition needs its own build). `invalid_session` → exit 3, "run
   gogo login".

### 2.2 Where the client credentials come from

In order, stopping at the first that has both:

1. **The game dir's `build-manifest.json`** (depot lane, design 21 §5.2): the manifest of the
   installed build. Read in place, never copied.
2. **A fresh manifest**: the builds listing for the game's gogid (`Platform::Windows`,
   generation 2) and the manifest of the installed build (`content.toml`'s build id), else
   the newest build. Its `clientId` must equal the one in the game's `goggame-<gogid>.info`
   when that file exists; a mismatch is an error, not a guess. This is the installer lane's
   path (it keeps no manifest) and the fallback for games built before design 21.
   The fetched manifest isn't kept: the installer lane has no place for it, and the next
   sync fetches it again.

The secret exists in memory only for the exchange: it is a field of a type with a redacting
`Debug` (design 07 §4), it is never in an error message, a log line, `--json` output, a
state file or a test fixture. Test fixtures use made-up values. `clientId` is not a secret
(GOG publishes it in `goggame-*.info` and the remote-config URL), but it is still logged only
at `-vv`.

### 2.3 The HTTP client

A second `reqwest::Client` for `cloudstorage.gog.com` only, with the User-Agent of §1.3
(D3) and no automatic decompression. `remote-config.gog.com` and `auth.gog.com` keep gogo's
own UA. Storage URLs contain the user id: they are logged with the path after `/v1/`
redacted (design 07 §4's `redact_url`, extended with this host).

### 2.4 As built (#458)

`gogo::cloud::{auth, http, error}`. Where it differs from, or adds to, the text above:

- **One entry point, `CloudAuth`.** `credentials()` and `token()` both persist the main
  session (`persist_if_changed`, as every command does) after anything that may have
  refreshed the Galaxy token: the builds and manifest fetch (even a failed one) and
  `bearer()`. Callers can't skip it. The kept-manifest path touches no network and persists
  nothing.
- **Seams.** gog-warp's token and content-system URLs are fixed, so `TokenExchange` (live:
  `WarpExchange`, a `SeparateCore` per exchange) and `ManifestSource` (live:
  `WarpManifests`) are traits, with fakes in tests.
- **Error mapping by call.** gog-warp's request errors wrap a `reqwest::Error` whose text
  holds the request URL (for the exchange: client secret and refresh token), so they are
  mapped by kind and status only, never formatted. For the **exchange**: a 403 is
  `no_license` (gog-warp doesn't read the body; 403 is the only one seen live, §1.1).
  gog-warp's 400 (`invalid_session`) and 401 become one "GOG rejected the token exchange:
  the session or the game's client credentials are invalid; try `gogo login`, then re-run",
  since a stale client secret plausibly answers the same. For the **builds and manifest
  fetch**: any status is a `Manifest` error naming the status; a 403 there is not
  `no_license`. `invalid_session` on the main session's own refresh is "run `gogo login`".
- **Exit codes.** `CloudError::exit_code()`: 3 for not logged in, the session, the rejected
  exchange and `no_license`; 4 for network, 429 and 5xx; 1 otherwise, 507 (quota) included.
  Wiring it into the CLI comes with the first `gogo saves` command (C3).
- **The fetched manifest is a Windows, generation-2 build's.** The installed build when it
  is listed and generation 2, else the newest public generation-2 build on the default
  branch (v1 manifests carry no client). Native builds (§11) need their own platform here
  later. The listing comes straight from gog-warp, so not even the builds-listing cache is
  written.
- **A bad `goggame-<gogid>.info`** (unreadable, not JSON) stops the command with an error
  naming the file: it is the check a fetched manifest is held to.
- **Redaction.** design 07 §4's `redact_url` didn't exist yet; `cloud::http::redact_url`
  keeps scheme, host and the first segment (`/v1`) and counts the rest. Storage URLs are a
  `StorageUrl` whose `Debug` and `Display` are that form. A crate-wide `redact_url` for every
  gog.com URL is a separate change.
- **The client.** Redirects are not followed (the UA never reaches another host), every
  automatic decoding is off explicitly, object names with an empty, `.` or `..` segment are
  refused, and at most 4 KiB of an error body is read.
- **Live check.** `crates/gogo-core/tests/live_cloud.rs`, ignored and gated by `GOGO_LIVE=1`
  and `GOGO_LIVE_CLOUD_GOGID`: a fetched manifest, the exchange, one listing GET, through
  `CloudAuth` (so a refreshed session is persisted), printing only the status, the object
  count and the redacted URL. Run once on 2026-11-14 against Cyberpunk 2077: 200, 231
  objects, as in §1.3.

## 3. Save locations per lane

### 3.1 Resolving a location

1. Take `content.Windows.cloudStorage` (both lanes run Windows builds). `enabled: false` or
   no `Windows` key: the game has no cloud saves; `gogo saves` says so and exits 0.
2. Split `location` on any run of `/` or `\` (Witcher 3's doubled backslash included).
3. The first segment must be exactly one `<?VAR?>` from the table below; anything else (an
   unknown variable, a drive letter, a relative path) skips **that location** with a warning
   naming it. The other locations still sync.
4. Map the variable onto a host dir, then append the remaining segments. A segment that
   doesn't exist with that exact case is looked up case-insensitively (Windows paths are, and
   Wine's own lookup is too); when it doesn't exist at all, the exact-case name is created on
   pull.
5. Refuse a resolved path that leaves the prefix or the game dir after normalization (a
   `..` segment, a symlink pointing out, other than the depot lane's `drive_c/GOG/<name> →
   ../../../game` link, which is followed).

### 3.2 Variables

Linux lane: `<prefix>` is `games/<gogid>/prefix` (umu's `pfx` links to it), the Windows user
is Proton's fixed `steamuser`. Mac lane: `<prefix>` is the bottle in the game's state dir, the
Windows user is `crossover`, which the CrossOver engine always uses (design 16 §4; check on
a real bottle, D9). `U` = `<prefix>/drive_c/users/<user>`.

| Variable | Windows meaning | Linux lane (Proton) | Mac lane (CX bottle) |
|---|---|---|---|
| `<?INSTALL?>` | the game's install dir | depot: `games/<gogid>/game/`; installer: `<prefix>/drive_c/GOG/<slug>/` | `games/<gogid>/game/` |
| `<?DOCUMENTS?>` | `%USERPROFILE%\Documents` | `U/Documents` | `U/Documents` |
| `<?SAVED_GAMES?>` | `%USERPROFILE%\Saved Games` | `U/Saved Games` | `U/Saved Games` |
| `<?APPLICATION_DATA_ROAMING?>` | `%APPDATA%` | `U/AppData/Roaming` | `U/AppData/Roaming` |
| `<?APPLICATION_DATA_LOCAL?>` | `%LOCALAPPDATA%` | `U/AppData/Local` | `U/AppData/Local` |
| `<?APPLICATION_DATA_LOCAL_LOW?>` | `%USERPROFILE%\AppData\LocalLow` | `U/AppData/LocalLow` | `U/AppData/LocalLow` |
| `<?APPLICATION_SUPPORT?>` | macOS only | not valid for a Windows build: skip with a warning | same |

Both lanes make the home dirs real dirs, not links to the host's home (design 18 §7.2
"Prefix isolation", design 16 §4), so saves stay inside the prefix. In the Proton prefix umu
also links `users/<host user>` and `steamuser` to each other; gogo always uses the
`steamuser` path, so the host user's name never enters a path gogo prints or records and every path above is under gogo's
control. gogo maps the variables itself rather than asking Wine (`winepath`, as Heroic
does): the users and layouts are fixed by gogo's own builds, and a status check must work
without starting a wineserver.

### 3.3 The default location

When storage is enabled and `locations` is empty, the location is named `__default` and is
`<?APPLICATION_DATA_LOCAL?>/GOG.com/Galaxy/Applications/<clientId>/Storage/Shared/Files`
(Heroic's reading; lgogdownloader stops at `…/Storage`). The five games tried all declare
locations, so this is not verified; it ships behind the same table, and a game that needs it
is the test case (D10).

### 3.4 Excluded files

Inside a location, the same excludes as design 20 §6.1 (`*.log`, `*.dmp`, `Thumbs.db`, Wine/DXVK
caches), plus partial files (`*.part`, `*.tmp`, `*.~incomplete`). In `<?INSTALL?>` locations,
files that belong to the build (design 20 §6.1's file list) are never synced: not pushed,
not downloaded over, not deleted. Without that list the location is skipped (§12c).

## 4. The sync state

### 4.1 Record

`<data_root>/cloud-saves/<gogid>.toml`, written atomically (tmp + fsync + rename), one per
game, outside the game dir so a rollback doesn't restore an old one:

```toml
schema = 1
client_id = "<clientId>"           # public; detects a client change (D11)
auto = false                       # launch-time sync for this game (§6)
rolled_back = 2026-11-12T10:00:00Z # set by a rollback (design 20 §6.3), cleared when settled
pending_push = false               # a post-exit push failed or was skipped offline (§6.3)
last_sync = 2026-11-13T09:58:00Z

[files."saves/ManualSave-12/sav.dat"]
cloud_hash = "<md5 of the stored gzip>"   # the listing's hash after the sync
local_md5 = "<md5 of the plain file>"     # what was on disk after the sync
size = 4718592
mtime = 2026-11-13T09:41:07Z
```

A file is in the record when the last sync left it the same on both sides. Deleted files
are dropped from it once both sides agree (tombstone in the cloud, absent locally).

### 4.2 Classification, per file

For every path that is in the cloud listing (non-tombstone), on disk, or in the record:

- **Local changed**: size or mtime differs from the record and the plain MD5 differs too
  (a `touch` alone is no change). Not in the record but on disk: local new.
- **Cloud changed**: the listed `hash` differs from the record's `cloud_hash`. Not in the
  record but listed: cloud new. A tombstone, or no entry, for a recorded file: cloud deleted.
- **Local deleted**: in the record, not on disk.

| Local | Cloud | Action |
|---|---|---|
| same | same | nothing |
| changed / new | same / absent | **upload** |
| same / absent | changed / new | **download** |
| same | deleted | **delete local** (to the backup dir, §7.1) |
| deleted | same | **delete in the cloud** (D2's mechanism) |
| changed | changed | **conflict** |
| changed | deleted | **conflict** |
| deleted | changed | **conflict** |
| new | new (no record) | download the cloud object to a temp file, gunzip, compare plain MD5: equal → record only; different → **conflict** |
| deleted | deleted | drop from the record |

Clock times decide nothing above: hashes and the record do. `last_modified`,
`X-Object-Meta-LocalLastModified` and local mtime are shown in conflicts and used only by
`--prefer newer` (§5.2). This is the difference from Heroic and Lutris, which compare
timestamps against one "last sync" time per location and decide the whole location at once
(§9).

**First sync** (no record): every file is "new" on its side, so the table's last rows apply
file by file: files only on one side are copied, identical files are recorded, and real
differences are conflicts. Nothing is overwritten on a first sync without a decision.

### 4.3 After a rollback

Design 20 §6.3: a rollback sets `rolled_back`. While it is set, every row that would upload,
download or delete is a **conflict** instead (a rolled-back save is "older" than the cloud's,
not "changed"), and the sync never resolves one alone: it needs `--prefer local|cloud` (or
the launcher's dialog, §6.2). Once a sync settles every file, it clears the mark and rewrites
the record. A rollback on a game with no record does nothing (the first sync asks anyway).

## 5. Commands

### 5.1 Shape

```
gogo saves status <gogid> [--json]
gogo saves sync   <gogid> [--prefer local|cloud|newer] [--dry-run] [--yes] [--json]
gogo saves pull   <gogid> [--prefer cloud] [--dry-run] [--yes] [--json]
gogo saves push   <gogid> [--prefer local] [--dry-run] [--yes] [--json]
gogo saves auto   <gogid> on|off
gogo saves locations <gogid> [--json]      # resolved paths, per location, with existence
```

Top level, not under `linux-app`/`mac-app`: the game is found by gogid in either lane's
library (`Library::resolve`), and the same command works for both (D5).

- `status` is `sync --dry-run` that never prompts: the table of actions, counts and sizes per
  location, conflicts listed, `rolled_back` and `pending_push` shown.
- `sync`: both directions, per §4.2.
- `pull`: only downloads and local deletions; uploads are left for later (shown, not done).
  `push`: only uploads and cloud deletions. Both still stop at conflicts.
- `--prefer local|cloud`: settle every conflict that way. `newer`: by
  `X-Object-Meta-LocalLastModified` (HEAD on the conflicting objects only) against local mtime,
  with a tie (within 2 s, the header's precision plus FAT-era mtimes) left as a conflict.
  `--prefer` is refused with `rolled_back` set unless it is `local` or `cloud` (design 20 §6.3:
  never decided by time after a rollback).
- `--yes`: no prompt for the plan. Without it, on a TTY, a plan that overwrites or deletes
  anything asks once (`3 downloads overwrite local files, 1 local delete (backed up to <dir>)
  [y/N]`); a plan that only adds files doesn't ask.
- A conflict with no `--prefer`, on a TTY: asked per file (`local`, `cloud`, `both` keeps the
  local one renamed `<name>.local-<ts>` and downloads the cloud one, `skip`). Without a TTY:
  nothing in that location's conflicts is done, the rest is, and the exit code is 8 (D6).

### 5.2 Order of a run

1. Locks (§7.4), resolve the game, the record, `rolled_back`.
2. Credentials and token (§2), remote config, the listing (one request for every location).
3. Scan the locations, classify (§4.2), print the plan. `--dry-run` stops here.
4. Conflicts settled (flags, prompt) or set aside.
5. If any download overwrites or deletes a local file: the `cloud-pull` snapshot (§7.2).
6. Backups (§7.1), then downloads (temp file in the same dir, gunzip, verify the stored
   bytes' MD5 against `hash` before gunzip, set mtime from `X-Object-Meta-LocalLastModified`,
   rename into place), local deletions (moved into the backup dir).
7. Uploads (gzip, `Etag`, `X-Object-Meta-LocalLastModified`), cloud deletions.
8. Re-list, rewrite the record from what the cloud now says and what is on disk.

A failure in 6–7 leaves the record as it was for the files not done (each file is recorded
only after it succeeded), so a rerun continues. The record is never advanced past a failed
upload (a Heroic pitfall, §9).

### 5.3 Output

The plan as a table (`location  action  file  size  local mtime  cloud time`), counts per
action, then one line per file done. `--json`: the plan and the result as objects, with the
same fields; no token, secret or user id. Exit codes: 0 done or nothing to do, 3 auth or
`no_license`, 4 network, 5 an object whose MD5 didn't match, 6 disk, 8 conflicts left (D6),
1 anything else.

### 5.4 Limits

- Quota: an upload plan that would take the container's bytes over the smaller of the two
  quotas (§1.3) is refused before any upload, with both numbers.
- Size: no per-file limit is known. Cyberpunk's largest objects are a few MB; the whole
  container is about 65 MB over 231 objects.
- Concurrency: four transfers at a time (saves are small; the polite-client rule of the README).

## 6. Around a launch

### 6.1 Opt-in

Per game, off by default: `gogo saves auto <gogid> on`, or a question at the first build of
a game whose remote config has storage enabled (TTY only; `--cloud-saves on|off` answers it).
`[cloud_saves] auto_default = "ask"|"on"|"off"` in the config sets the default (D1).

### 6.2 What the launcher does

`launch.sh` (Linux, design 18) and the Mac launcher (design 16) already call back into gogo
(`linux-app setup`, `next-renderer`). With `auto` on (read from `launch.conf`, which setup
writes from the record):

- **Before the game starts**, after the refusals and setup, before the run log's game start:
  `gogo saves pull <gogid> --launcher`. `--launcher` means: a 15 s budget for the network
  part, no TTY prompts, conflicts go to a dialog through the launcher's existing dialog
  helper (`Use cloud saves` / `Keep local saves` / `Launch without syncing`), and every
  failure that isn't a conflict prints one line to the run log and returns 0 so the game
  starts. A pull that would need the `cloud-pull` snapshot and can't take it (§7.2) also asks
  (`Launch without syncing` / `Cancel`).
- **After the game exits** and the prefix is idle (where the run log's footer is written
  now): `gogo saves push <gogid> --launcher`. Conflicts here (the cloud changed while the
  game ran, from another machine) go to the same dialog; offline or any failure sets
  `pending_push` and logs one line.
- **Not** on `linux-app run` one-offs, not while another launcher of the game is running (the
  `.launchers/<pid>` registry), and not after an early exit that leads to the next-renderer
  dialog (the saves of a run that didn't start aren't worth pushing).
- `gogo` not found (the launcher's existing fallback): no sync, one log line.

### 6.3 Offline

Nothing blocks a launch. A failed pre-launch pull logs and launches. A failed post-exit push
sets `pending_push`; the next pre-launch pull is a `sync` instead (it pushes what is pending,
after the same classification, so a conflict from another machine still asks), and
`status`/`apps show`/`doctor` show the flag. A pending push is never retried in the
background.

## 7. Safety

### 7.1 Backups

Before anything is overwritten or deleted, on either side, the old bytes are copied to
`<data_root>/cloud-saves/backups/<gogid>/<UTC timestamp>/`:

- local files a download overwrites or a local delete removes: under `local/`, paths kept
  relative to the location, with the location name as the first dir (FICLONE where possible);
- cloud objects an upload overwrites or a cloud delete removes: downloaded first, under
  `cloud/`, as the stored gzip with the listing entry beside them (`.json`), so they can be
  re-uploaded unchanged.

The last line of the run names the dir. Backups are pruned to the newest
`cloud_saves.keep_backups` (default 20) dirs per game, oldest first, each reported; this is a
separate dir from design 20's `saves-aside/`, which gogo never prunes (D7). `apps remove`
lists them with the game, like `saves-aside/`.

### 7.2 Snapshots (design 20 §6.3)

1. **Rollback**: sets `rolled_back` in the record (§4.3). The rollback's own plan says so
   (`cloud saves: the next sync will ask about every difference`).
2. **A pull that overwrites or deletes local files** takes an `auto` snapshot (`trigger =
   "cloud-pull"`, label `before cloud-save download`) when the game has snapshots on
   (Linux lane, btrfs/ZFS). Downloads that only add files, and pushes, take none. A failed
   snapshot stops the pull (exit 7, `--no-snapshots` to skip it for the run), per design 20
   §7.1; in the launcher it is the dialog of §6.2. The backup of §7.1 is taken as well: it is
   what a game without snapshots, and the Mac lane, has.
3. **Save roots**: the resolved locations (§3) become design 20 §6.1's save roots for a
   game with cloud saves, ahead of the generic list, which stays the fallback. `<?INSTALL?>`
   locations go through the build-file filter there too. The resolver is one function both
   use (`cloudsaves::locations(game) -> Vec<Location>`), with no network: it reads the remote
   config from the cache (`<cache>/gogo/remote-config/<clientId>.json`, written by every
   sync) and returns nothing when there is no cache, so a rollback offline falls back to the
   generic roots.
4. **Snapshots never touch the cloud**: a rollback doesn't push, a snapshot delete deletes
   nothing remote, and a sync never reads a snapshot.

### 7.3 Dry run

`--dry-run` on every command that writes, and `status`, run steps 1–3 of §5.2 (network reads
only) and print exactly what a real run would do, with the same exit codes (8 when conflicts
would be left).

### 7.4 Locks and refusals

- A per-game lock `<data_root>/cloud-saves/<gogid>.lock` (flock): two syncs of one game never
  run together.
- A pull that writes local files also takes the game's change lock (design 20 §5.1), so it
  never runs during a rollback, a DLC add, an update or a move, and refuses (exit 7) while the
  game runs, except when the launcher itself calls it before the start or after the exit.
- A push only reads local files; it refuses while the game runs (a save half-written by the
  game) unless the launcher calls it after the exit.
- The record is written only under the lock.

## 8. Interplay

- **`apps show` / `doctor`**: cloud saves on/off, last sync, `pending_push`, `rolled_back`,
  backups and their size. `doctor` flags a record whose `client_id` no longer matches the
  game's (a rebuilt game from another edition, D11).
- **`apps remove`**: asks whether to keep `cloud-saves/<gogid>.toml` and the backups (default
  keep the backups, drop the record); never touches the cloud.
- **`move`**: the record is outside the game dir and keyed by gogid; nothing to do.
- **Update / DLC**: the client id stays the same across builds of one product (Cyberpunk's
  Windows and macOS builds share it); a change is detected by the record's `client_id`.
- **Another client (Galaxy, Heroic) on the same account**: it sees gogo's uploads as normal
  objects with gogo's UA in `X-Object-Meta-User-Agent`, and its tombstones are honoured. gogo
  writes the same gzip format so other clients can read what it uploads (to verify with the
  first upload, D2).

## 9. Prior art

Read from source (2026-11-13), described in our own words; no code taken.

| Project | Licence | Cloud saves |
|---|---|---|
| heroic-gogdl (Heroic's GOG backend) | GPL-3.0 | list/download/upload/delete; sync per location |
| Heroic Games Launcher | GPL-3.0 | runs gogdl's sync before launch (download only) and after exit (upload only); manual sync dialog; resolves paths through Wine |
| Lutris | GPL-3.0 | a port of gogdl's algorithm, hooks around launch, a GTK conflict dialog |
| lgogdownloader | WTFPL | list/download/upload/delete commands, Windows builds only, no sync state |
| GogDownloader (RikudouSage) | MIT | download only |
| Minigalaxy | GPL-3.0 | none |
| comet | Apache-2.0 | none (a Galaxy-protocol emulator) |

- **Credentials**: all take `clientId`/`clientSecret` from the v2 build manifest; Heroic falls
  back to `goggame-*.info` for the id. All exchange the Galaxy refresh token for a per-game
  token with `without_new_session=1` (lgogdownloader: an equivalent no-new-session flag).
- **Paths**: all read remote config (`component_version` 2.0.43–2.0.51), map the same seven
  variables, fall back to `__default`. Lutris assumes the Wine user is `$USER` (wrong under
  Proton's `steamuser`); lgogdownloader spells `Saved Games` wrongly.
- **Format**: deterministic gzip (level 6, mtime 0) so a local file's gzip MD5 can be compared
  with `hash`; lgogdownloader uploads raw bytes instead. All send the Galaxy
  communication-service UA with `dont_sync_marker/true`.
- **Algorithm (gogdl, Lutris)**: one "last sync" timestamp per location; a side "changed"
  when a file's time is newer than it; local-only → upload all, cloud-only → download all,
  both → conflict for the **whole location**; a file missing on one side is deleted on the
  other. Pitfalls gogo avoids: decisions by timestamps (clock skew, the server's
  `last_modified` vs local mtimes), whole-location decisions, deleting on absence after a bad
  timestamp, advancing the timestamp after failed uploads, non-atomic downloads.

## 10. Testing (design 08)

- **Unit**: location resolving (every variable, both lanes, both build lanes, mixed and
  doubled separators, unknown variables, escapes out of the prefix, case-insensitive lookup);
  classification table (§4.2) row by row, with `rolled_back`; tombstone detection; record
  round trip; gzip + MD5 of uploads against fixtures with made-up content.
- **Mock storage**: a wiremock server for auth, remote config and storage (list JSON and
  text, gzip objects, headers, 400 `invalid_version`, 403, `no_license`, 404, tombstones);
  full `sync` runs on a temp prefix: first sync, both-side changes, deletions both ways,
  conflicts with each `--prefer`, a failed upload mid-run and the rerun, quota refusal,
  offline (`pending_push`).
- **Snapshot interplay**: with the fake snapshot backend (design 20 I1): the `cloud-pull`
  trigger only on overwrites, a failed snapshot stopping the pull, `rolled_back` forcing
  conflicts and being cleared.
- **Launcher**: the launcher test harness checks the calls' order and that a failing or
  missing gogo never blocks the start.
- **Live**: read-only `status` on owned games in the live smoke (design 08), gated like the
  other live tests. Writes (`push`, cloud delete) only on a test account or a game
  @noosxe names, with an explicit OK (D2).
- **linux-e2e**: none in the harness (no account there); the host pass lists a real
  round trip with Cyberpunk 2077: pull, play, push, check in Galaxy or Heroic.

## 11. Lanes and order

1. **Linux lane, Windows builds** (both depot and installer lanes): everything above.
2. **Mac lane**: the same core with the `crossover` user and no snapshots; the launcher hooks
   in the Mac launcher. Needs a real bottle to confirm the user and the home dirs (D9).
3. **Native builds** (#162 Linux games, native macOS apps): remote config's `Linux`/`MacOS`
   keys, host paths (`~/.local/share`, `~/.config`, `~/Library/Application Support`), and
   GOG's habit of declaring `enabled: false` for them (all five games tried). Later, on
   request.

## 12. Decisions

Decided on 2026-11-13: every recommendation of the draft was accepted as written.

| # | Question | Decision |
|---|---|---|
| D1 | Launch-time sync on by default? | **Off by default, per game opt-in** (§6.1): asked at the first build of a game with cloud storage (TTY), `--cloud-saves on\|off` answers it, `[cloud_saves] auto_default` changes the default. A wrong sync loses saves; the manual commands earn trust first. |
| D2 | Live writes (upload, delete) aren't verified (§1.3). | **One write test, with @noosxe's explicit OK, on a game they name** (an owned game with no cloud saves yet suits it): PUT one file, read it back, check it in Galaxy or Heroic, delete it, and see whether the delete leaves a tombstone. It gates C5: until it has run, gogo writes nothing to the cloud, and deletions are written the way it shows other clients expect. |
| D3 | User-Agent for the storage API? | **The Galaxy communication-service UA with `dont_sync_marker/true installation_source/gog` and ` gogo/<version>` appended** (§1.3), sent only to `cloudstorage.gog.com`, as a constant with a comment naming the check; re-verified in the live smoke. |
| D4 | Store per-game tokens? | **No**: a separate in-memory `Core` per command (§2.1). One extra request per sync, no new secret at rest, no keychain rewrites. |
| D5 | Command placement? | **Top level, `gogo saves status\|pull\|push\|sync <gogid>`** (plus `auto`, `locations`), for both lanes (§5.1). |
| D6 | Exit code for conflicts left unresolved? | **A new 8** ("needs a decision"): none of 1–7 fits, and scripts need to tell it from a failure. |
| D7 | Backups: where and how long? | **`<data_root>/cloud-saves/backups/<gogid>/`, the newest 20 kept per game** (`cloud_saves.keep_backups`), apart from design 20's never-pruned `saves-aside/` (§7.1). |
| D8 | First sync with saves on both sides? | **File by file** (§4.2): one-sided files are copied, both-sided ones compared by plain MD5, real differences asked about; never a whole-side default. |
| D9 | Mac lane user dir? | **`drive_c/users/crossover`** as design 16 §4 states, confirmed on a real bottle before the Mac lane (C8) lands. |
| D10 | The `__default` location? | **Implemented per §3.3 and marked unverified**: `status` labels it, and the first game found that uses it is checked live before writes go there. |
| D11 | The game's client id changes (another edition rebuilt under the same gogid)? | **The old record is set aside** (renamed with a timestamp), with a warning, and the next sync is a first sync (§4.2). |
| D12 | Push deletions? | **Yes**, as Galaxy does, each cloud object backed up to `cloud/` first (§7.1); `--no-delete` on the manual commands keeps a sync additive. |
| D13 | Remote-config `component_version`? | **A constant (`2.0.45`)**, with a live-smoke check that fails if GOG starts answering it differently. |

## 12a. As built: remote config and locations (C2, #459)

Code: `gogo_core::cloud::{remote_config, locations}`, `gogo saves locations <gogid> [--json]`.

- **Lane** is the host's (Linux → `steamuser`, macOS → `crossover`), a `Lane` value the
  tests pass explicitly. Both lanes find the game through `GameDirs::for_game`, and the Mac
  lane reuses the Linux layout helpers (`setup::game_files`, the `prefix/` and `game/` dirs of
  `GameDirs`). D9's check of the `crossover` user on a real bottle is still pending. (The
  separate location error and client-id reader this shipped with were merged into C1's by C3,
  §12b.)
- **Client id**: the game's `goggame-<gogid>.info` (the game's files, then anywhere under
  `drive_c` for the installer lane), else the `clientId` field of the kept
  `build-manifest.json`, read through a typed struct that holds only that field, so the secret
  is never parsed into a value.
- **Remote config cache**: `<cache>/remote-config/<clientId>.json`, fresh for 24 h (a future
  mtime counts as expired); a failed fetch falls back to an older copy (`config_source:
  stale_cache`), no copy is an error (exit 4). An answer without a `content` object, or a
  corrupt cache file, is no remote config: never used, never cached. A 404 from the service
  means GOG has no config for the client: "no cloud saves", exit 0 (`no_remote_config: true`
  in `--json`). `component_version` is the constant `2.0.45`; the D13 check is the ignored test
  `live_component_version_still_answers` (`GOGO_LIVE_CLIENT_ID=<id> cargo nextest run
  --run-ignored only live_component_version`).
- **Lookup** is case-insensitive per part: the exact case wins; two dirs that differ only
  by case make that one location skipped (`ambiguous`); a missing part keeps its exact case.
  The variable's own dirs (`drive_c`, `users`, `Documents`, …) go through the same lookup.
- **Escape guard** (`Roots::check`): no `.`/`..`/`:`/NUL part, and the deepest existing
  ancestor, links resolved, must be under the canonical prefix. A path may land in the game's
  files only as an `<?INSTALL?>` path or through the prefix's `drive_c/GOG/<name>` link, and an
  `<?INSTALL?>` path may not leave the game's files: a link into the game dir from elsewhere in
  the prefix, and one from the game dir into the prefix, are refused (a save location never
  aliases the other tree). The prefix and game dir needn't exist yet (a depot game that never
  ran): the roots are canonicalized at their deepest existing ancestor. `Location::file(rel)`
  does the same check for one object path and returns the canonical path for the parts that
  exist; it is a check at one moment, so the pull and push must check again just before each
  write and must not follow links then (`O_NOFOLLOW`, or a verified parent dir).
- **Skipped locations** are reported with a reason (`unknown_variable`, `not_a_variable`,
  `not_for_windows_build`, `bad_path`, `bad_name`, `duplicate_name`, `ambiguous`,
  `escapes_prefix`) in `--json` and as warnings in the text output; the others still resolve.
- **Excludes**: design 20's list plus `*.part`, `*.tmp`, `*.~incomplete`, as
  `cloud::locations::excluded`. The `<?INSTALL?>` build-file filter (§3.4) came with #469
  (§12c).
- `__default` is resolved and labelled `unverified` (D10).

## 12b. As built: read side and `gogo saves status` (C3, #460)

Code: `gogo_core::cloud::{client_id, storage, record, scan, classify, status}`,
`gogo saves status <gogid> [--json]`.

- **One error, one reader.** `CloudError` covers every `saves` command (the location error
  joined it) and `CloudError::exit_code()` is the only mapping: 2 a gogid with no game dir, 3
  auth and `no_license`, 4 network (remote config too), 5 a corrupt object, 1 the rest; the
  CLI adds 8 for conflicts. `cloud::client_id` is the one `clientId` reader: it finds
  `goggame-<gogid>.info` in the game's files, then under `drive_c`, and reports a broken file
  (`InfoId::Bad`: unreadable, not JSON, an id that isn't a usable client id) to the caller.
  Credentials refuse it (§2.2's check); `saves locations` falls back to the kept manifest's
  id-only field with a warning. `CloudAuth::credentials` takes the game: the kept manifest is
  read from the game dir, the info file from the game's files, per lane.
- **Order of a status.** The client id and the remote config come first, with no account: a
  game without cloud saves (off, or a 404) never restores the session. Then the record, the
  credentials (their client id must equal the game's, else `ClientIdMismatch`), the token,
  one listing. The CLI restores the session only for that second half.
- **What status writes**: the remote-config cache and, through `CloudAuth`, a refreshed Galaxy
  token. Not the cloud, not the game's dirs or prefix (a test compares the tree before and
  after), not the record: a client-id change is reported (`client_id_changed`) and planned as
  a first sync; the set-aside (D11) is done by the next sync, under the lock. No lock is taken.
- **Listing**: JSON only, read up to 32 MiB; every `hash` is lowercased and must be 32 hex
  digits, and a name listed twice is an error. A listing with fewer entries than
  `x-container-object-count`, or with no count and exactly 10 000 entries (Swift's page), is
  refused (`ListingTruncated`): GOG's paging is unverified, and a missing entry would read as
  a cloud deletion. Paging comes when a game needs it. A 404 on the container listing (and
  only there) is an empty container, nothing ever uploaded: **unverified live**, shown as
  "nothing stored for this game yet".
- **Downloads** read at most the listed `bytes`, which is untrusted and clamped to
  `MAX_OBJECT_GZIP` (200 MiB, the smaller quota seen live; a larger listing is `corrupt`
  without a request), check the MD5 of the stored bytes, and only then gunzip (multi-member
  gzip accepted), at most `MAX_PLAIN` (2 GiB) of output: more is `corrupt` (a gzip bomb). `status` fetches only the first sync's both-sided
  files, four at a time, into memory, and hashes the gunzipped stream; nothing goes to disk.
  An object that fails the check is planned as `corrupt` (exit 5) and the rest still planned.
- **Tombstones**: the hash above (Galaxy's), the MD5 of gogo's own empty gzip
  (`storage::gzip`: level 6, no name, mtime 0, the form C5 uploads), or a 0-byte object. A real
  empty save file reads as deleted, as in Galaxy. So pull and sync (C4, C5) **must back up
  every local file before a `delete_local`** (§7.1), with no exception for small or empty
  files: the tombstone may be someone's real empty save.
- **Record**: as §4.1, keyed by object name, times as TOML datetimes in UTC, `mtime` to the
  nanosecond (compared exactly; any difference means "hash it"). A record that can't be
  parsed, or has another `schema`, is an error, not a first sync: it may hold a rollback mark.
  Written atomically and only through `SyncLock` (`<gogid>.lock`, flock, fails at once when
  held); a set-aside record becomes `<gogid>.client-changed-<UTC>.toml` and never overwrites
  an older one.
- **Matching** is by local path: every listed or recorded name goes through
  `Location::file`, so case differences from Windows clients meet the same local file. Two
  cloud names that land on one file are both conflicts (`case_clash`); only a live object, or
  a recorded name whose file is there, counts, so a tombstone or a deleted file's record of
  another case is history, not a clash. The scan walks the location's canonical path (the
  prefix may be reached through a link), never follows a link, stops at 32 levels, and lists
  links, special files and non-UTF-8 names as `ignored`. A cloud name that would land on a
  link out of the prefix is `bad_path`. Hashing opens with `O_NOFOLLOW | O_NONBLOCK` after
  checking the path is a regular file, and checks the opened file is that same one (device
  and inode); a file that can't be read is planned `unreadable` (exit 1) and the rest is
  still planned.
- **A missing or emptied save dir** never plans cloud deletions. When the location's dir is
  gone (a moved prefix, a drive not mounted) or exists with no file at all, while the record
  has files in it, every row that would be `delete_cloud` is a conflict instead
  (`location_missing`, `location_emptied`), with a warning on the location. With no record, a
  missing dir only means downloads. A dir with at least one file left plans deletions
  normally.
- **Classification** adds four outcomes to §4.2's table: `record` (first sync, same
  content), `forget` (deleted on both sides), `corrupt` and `unreadable`. `rolled_back` turns
  uploads, downloads and deletions into conflicts (`rolled_back`), not `record`/`forget`. A
  record for another client (D11) is no record: its `rolled_back`, `pending_push` and
  `last_sync` aren't shown or used.
- **Objects outside the plan** are listed as `unmapped` with a reason: `no_location`,
  `skipped_location`, `bad_path`, `excluded`; never acted on. Tombstones outside every
  location are left out.
- **`<?INSTALL?>` locations** get the build-file filter of §12c (#469).
- **Exit codes** of `status`: 5 when any object is corrupt, else 1 when a local file was
  unreadable, else 8 when conflicts are left, else 0.
- **Output**: control characters in names, times and messages are escaped in the text
  output; text hides unchanged files and ends with counts per action, unmapped notes and
  the container's use; `--json` has every file with `action`, `reason`, both sides and
  `recorded`, the counts, `first_sync`, `client_id_changed`, `rolled_back`, `pending_push`,
  `last_sync`, the skipped locations, the unmapped objects and the container. No token, secret,
  user id or client id.
- **Tests**: `cloud::testing` (feature `test-util`) has a main session with a made-up token,
  a fixed exchange and a kept manifest with made-up credentials, for core and CLI tests
  against wiremock.
- **Live check** (2026-11-14, read-only, gogo's data and cache dirs in scratch dirs, the game
  dir linked in): Cyberpunk 2077 with no local saves gave 63 downloads, 231 objects of which
  168 tombstones, exit 0, as in §1.3. Nothing in the game's dirs changed.

## 12c. As built: build files in the game's files (#469)

Code: `gogo_core::cloud::build_files`, `Location::{in_game_files, build_file}`.

- **Which locations**: any location that reaches the game's files (`game/` on the depot
  lane, `drive_c/GOG/<slug>` on the installer lane): an `<?INSTALL?>` one, one through the
  prefix's `drive_c/GOG/<name>` (even before that link exists), one whose canonical path is
  in them, and one that holds them (a `C:\GOG`-like dir). Each file is tested by its own
  canonical path relative to the game's files, so `<?INSTALL?>/SAVE`, `<?INSTALL?>` and an
  ancestor all see the same list, and a file of an ancestor outside the game's files is
  never a build file.
- **Links and hard links**: below such a location, `Location::file` refuses a cloud name
  whose path goes through a link (`SAVE/x -> ../Game.exe`, a dir link to `../Data`) or lands
  on a file with more than one hard link (`bad_path`), and the scan lists such local files
  as `ignored`: either could be a build file under another name.
- **The test is by path alone**: a path the list names is the build's whatever its size or
  time, unlike the rollback's name-and-size test (design 20 §6.1). A shipped `settings.ini`
  the game rewrote stays out too: syncing it would let a download overwrite a shipped file
  and an upload carry it to other machines. gogo's own files there (design 20's `OwnFiles`:
  the compat `files`, cnc-ddraw's) are left out the same way. Matching ignores case with
  Unicode lowercasing (the rollback's list too); only the install-dir prefix of an Inno log
  line is compared in ASCII.
- **The list** is design 20's `BuildFiles`, read the way the rollback reads it:
  - installer lane: the main installer's Inno log (`install/inno.log`) and the log of every
    DLC the content record lists (`install/dlc-<id>-inno.log`; the rollback reads those
    now as well, leniently). For cloud saves the reading is strict: a missing main log, or
    any of those logs missing, unreadable or naming no file in the install dir, means no
    list;
  - depot lane: the depot cache when it holds the installed build (by build id) in the
    installed language (the record's own, or another language it still holds) with
    every DLC the game has in that language, else the build's depot manifests for the base game and its DLC in the
    installed language, fetched by `saves status` with the main session (the snapshot
    command's fetcher, Linux only: on macOS such a location is skipped). The list isn't kept
    on disk;
  - no content record (a game built before it, a Mac-lane game): the installer's log when it
    is there; else, for a `game/` tree, `content::resolve`'s derived content, used only when
    the game's own info file names the build (a cache of another build never stands in).
- **No list, no sync**: every location in the game's files is skipped (`build_files_unknown`,
  the message says why: no content record and no build in the info file, a missing
  installer log, manifests that couldn't be fetched offline). The others still sync. A
  location in the game's files with no list attached counts every file as the build's, so a
  caller that forgets the list syncs nothing there rather than everything.
- **What a sync does with build files**: local ones are counted (`local_build_files`) and
  never planned; a live cloud object at a build file's path is `unmapped` with reason
  `build_file` and never acted on; tombstones and record entries there drop out of the plan
  (the record itself is left as it is).
- **Output**: `saves locations` (offline) names each such location's list and its size, or
  says `saves status` fetches it, or skips the location; `--json` has `build_files: {source,
  files}`. `saves status` prints `left out as the game's own files: N (list: …)` per location
  and a note for `build_file` objects; `--json` has `local_build_files` and `build_files`.
- **Live check** (2026-11-14, read-only, gogo's data and cache dirs in scratch dirs, the game
  dirs and the depot cache linked in): eight of the thirteen built games declare
  locations, six of them under `<?INSTALL?>`. The four with a list: Heroes of Might and Magic
  3 (manifests fetched, 273 paths) leaves out its shipped `Games/savegames.txt`, which
  Galaxy had uploaded (`build_file`), and plans only the save; SimCity 3000's `Cities/`
  holds 59 shipped cities, all left out; two others plan only their saves. The two older
  builds whose info files name no build are skipped with the reason.

## 12d. As built: rollback interplay and save roots (C6, #463)

- **The mark**: `SyncLock::mark_rolled_back(now)` (record.rs) sets `rolled_back` in an
  existing record and leaves a mark already set alone (the earliest unsettled rollback
  stands). No record: nothing is written and no `cloud-saves/` dir is created. A record that
  can't be read can't be marked: the rollback goes on, with a warning, and its plan says so.
- **When**: `snapshot rollback` takes the game's sync lock before it changes anything,
  whether or not a record exists (a first sync could create one: the lock file is created,
  never a record). A running sync is waited for 3 s, then refuses the rollback (exit 7,
  "nothing was changed"). The mark is set as the first step of the run, before the journal
  and the pre-rollback snapshot, and a record that can't be marked refuses the rollback.
  `finish()`, which recovery reruns, sets it again (idempotent, best effort: a warning when
  it fails, as the game is rolled back by then). A mark left by a rollback that then fails
  costs one question at the next sync; a missing one could let a sync overwrite the saves.
- **Lock order**: the cloud-save sync lock comes before the snapshot lock, in every command
  that takes both (the rollback now; the commands of #461 and #462 must follow it, a pull taking its
  `cloud-pull` snapshot included). Recovery of a rollback runs under the snapshot lock, so its re-mark waits only 1 s
  and never blocks.
- **Output**: the plan has `cloud saves: the next sync will ask about every difference
  (--prefer local|cloud)` when a record exists (`cloudSaves` in `--json`: `noRecord`,
  `record`, `unreadable`); the result has `cloudSavesMarked`.
- **Save roots**: `cloud::save_roots::prefix_roots` resolves the cached remote config offline
  (`remote_config::load_cached`; no cache, no extra roots, no network) and returns the
  existing locations inside the prefix. They are unioned with design 20's generic roots (a
  file both reach is listed once, matched by canonical path, so a location reached through a
  link in the prefix doesn't list it twice), with the same excludes, applied below the user
  dir the location is in. Files in a location carry its name: the plan's group line reads
  `<dir> (cloud: <name>)` and `--json` has `cloud` on the file and the group. Locations in the game's files (`<?INSTALL?>`, the `drive_c/GOG/<name>`
  link) are not added: the finder already scans the game's files with the build-file filter.
  In practice every non-`<?INSTALL?>` variable resolves under a generic root, so the union
  adds files only for a user dir the generic scan doesn't list (it lists those present) and
  stays correct if the variables grow.

## 12e. As built: `gogo saves pull` (C4, #461)

Code: `gogo_core::cloud::{pull, write, backup}`, `gogo saves pull <gogid> [--prefer cloud]
[--dry-run] [--yes] [--json] [--no-snapshots]`. A pull writes local save files and gogo's own
state only; the storage host only ever sees GETs (the tests check every request).

- **Order** (§5.2): the sync lock (held: exit 7), then the game's change lock
  (`game-moves/<gogid>.lock`) and the running checks of the lane (launcher record, processes
  in the prefix: exit 7), both held for the whole run, prompts included, so nothing else
  changes the game meanwhile. Then `status`'s own plan (`status::plan_detailed`, the same
  classification), the conflict answers, the one question, the `cloud-pull` snapshot (design
  20's `protect`, under the change lock already held), the backups, the D11 set-aside (so a
  refused snapshot leaves even the record where it was), the files. `--dry-run` takes no lock and writes nothing, not even the lock file.
- **What a pull does per action**: `download` (new file, or `overwrite` when a local file is
  there), `delete_local`, `record` and `forget` touch the record only; `upload` and
  `delete_cloud` are `for_push`, shown and never done (not exit 8).
- **Conflicts**: `--prefer cloud` (the only value; `local` and `newer` are refused by the
  parser: they are push's or sync's) takes the cloud's side: a download, or a local delete
  when the cloud has a tombstone or no entry for a recorded file. On a TTY each conflict is
  asked: `cloud`, `local` (kept for a push: decided, the record untouched), `both` (only when
  both sides have a file: the local one kept as `<name>.local-<UTC>`, `-2`, `-3`… when taken,
  and the cloud's downloaded; see the writes below), `skip`. Headless, or `skip`:
  left, exit 8. Never settled by a pull: `case_clash`, and `location_missing`, whose dir a
  pull doesn't make again (it may be a drive not mounted): every write planned in such a
  location is left too. `location_emptied` is settled like any conflict (the dir is there).
  A local-only file in a conflict (no cloud entry, never recorded) has nothing to decide in a
  pull: `for_push`.
- **The question**: asked once when anything is overwritten or deleted, after the conflict
  answers (they add overwrites). `--yes` skips it. Without a TTY (or with `--json`) and
  without `--yes`, nothing is changed and the exit is 2, as for other commands that can't ask
  (`dlc add`, `update`); exit 8 stays for conflicts. A pull that only adds files doesn't ask.
- **Snapshot**: only when a file is overwritten or deleted (a `both` and adds take none),
  before the backups and the set-aside. A failure, or a rollback or
  conversion that isn't settled, stops the pull with no save file touched (exit 7; the message
  names `--no-snapshots`). Games without snapshots (a plain dir, autosnapshot off, the Mac lane)
  skip it; the backups are what they have.
- **Backups**: every file that will be overwritten, deleted or kept by a `both` (cheap
  insurance: its kept copy is otherwise the only one) is copied first, all of them before the
  first write, empty files included (§12b), to
  `cloud-saves/backups/<gogid>/<UTC>/local/<location>/<path>` (`-2`, `-3`… when the second is
  taken), each fsynced. Read through the same no-link walk as the writes, and checked to be
  the planned file (device, inode, size, mtime). A copy, never a move: the backup dir may be on
  another filesystem than the prefix (a btrfs subvolume boundary). A run that backs nothing up
  makes no dir; one that does prunes to `cloud_saves.keep_backups` (default 20, at least 1)
  dirs named like backups, oldest first, each listed, and never its own dir, whatever its
  name says (a clock that went back).
- **Writes** (`cloud::write`): the target dir is opened from `/` one part at a time with
  `O_DIRECTORY | O_NOFOLLOW`, so a link put in place after `Location::file`'s check is refused,
  not followed (a test swaps a dir for a link during the question). Missing dirs are made only
  below the prefix's `drive_c` or the game's files, which must exist: without them the file is
  left (`no_prefix`), gogo makes no prefix. Just before each write the name is mapped again
  (`Location::file` must give the planned path, not a build file) and the target checked
  against the plan (device, inode, size, mtime, or absent): a file changed meanwhile is left
  (`changed_since_plan`, exit 8). A download goes to `.gogo-pull-<pid>-<n>-<ns>.tmp` in the
  same dir (`O_CREAT | O_EXCL | O_NOFOLLOW`, the old file's mode), is gunzipped there (MD5 of
  the stored bytes checked before), gets the uploader's `X-Object-Meta-LocalLastModified` as
  its mtime (none or unparseable: the write time), is fsynced, the target checked once more,
  renamed over it, and the dir fsynced. The mtime is used only from 1980-01-01 to a day after
  now; anything else (or an unparseable header) is a note and the file keeps the write time,
  and a `futimens` that fails is a note too, never a stop. For a `both`, the local file is
  hard-linked to its kept name only once the cloud's file is complete and fsynced beside it,
  then the rename puts the cloud's in place: a corrupt object, a full disk or a failed rename
  leaves the local file at its own path (and a link made is removed again). Where hard links
  aren't supported, a no-replace rename stands in, undone when the second rename fails. A
  delete is `unlinkat` after the same check. A stop at
  any point leaves the old file or the new one, never part of one; a left-over temp file is
  excluded from sync by `*.tmp` and removed by the next pull that writes to that dir (tested by
  a seam that stops after the fsync, before the rename).
- **Record**: written after each file that was done (atomic, under the lock), so a rerun after
  a stop sees the done files as in sync. After a download: the listing's hash, the plain MD5 of
  what was written, the size and mtime on disk. At the end `last_sync`; `rolled_back` is
  cleared only when every planned file was done (nothing left, kept, for push, corrupt or
  unreadable). `pending_push` is never touched by a pull. D11: a record for another client is
  set aside as the first write of a real run, and the run is a first sync.
- **Errors**: a corrupt object (MD5, gzip) is skipped and the rest done (exit 5). Only the
  decoder's errors count as corrupt: a failure of the local write while gunzipping (a full
  disk) stops the run like any failed write or backup (exit 6). A path that fails the walk (a
  link or a file where a dir should be), in a write or a backup, leaves that file
  (`path_refused`, exit 8) with nothing written for it, and the run goes on. A network error
  stops it (exit 4). Files not reached are `not_done`, and the record holds what was done; a
  record that can't be written at the end still gives the report of what was. Exit
  precedence otherwise: 5, 1 (an unreadable local file), 8, 0.
- **Memory**: objects are fetched in batches of at most four and at most 256 MiB of listed
  bytes (one object alone always goes; none is over 200 MiB), each batch written before the
  next is fetched.
- **Output**: one line per file that isn't unchanged (`download`, `overwrite`, `keep both`,
  `delete here`, `same`, `forget`, `left (why)`, `for push`, `kept here`, `corrupt`,
  `not done (would …)`), counts, the snapshot line, pruned backups and the backup dir last;
  `--json` is the plan and the per-file `op`, `choice`, `state`, `reason`, `backup`,
  `kept_as`, with no token, secret, user id or client id.
- **Not yet**: `--launcher` and its budget and dialog (C7). The launcher holds the change lock
  shared while the game runs, so a pull from inside the launcher needs that lock handed over
  then.
- **Live check** (2026-11-14, a scratch copy under `target/`: gogo's data and cache dirs in a
  scratch `GOGO_HOME`, the game's small state files and info file copied, an empty prefix):
  Cyberpunk 2077 pulled 63 files (75 MB) with the uploaders' mtimes, a rerun had nothing to
  pull; with the record edited by hand to fake a cloud change and a cloud deletion, the
  headless run without `--yes` changed nothing (exit 2), and with `--yes` overwrote one file
  and deleted one, both backed up first. Only downloads reached GOG.

## 12f. As built: `gogo saves push` and `sync` (C5, #462)

Code: `gogo_core::cloud::pull` (one runner, `Mode::{Pull, Push, Sync}`), `storage::{put_object,
delete_object, head_local_last_modified, gzip_reader}`, `backup::Backup::save_cloud`,
`gogo saves push <gogid> [--prefer local] [--no-delete] [--dry-run] [--yes] [--json]` and
`gogo saves sync <gogid> [--prefer cloud|local|newer] [--no-delete] [--dry-run] [--yes] [--json]
[--no-snapshots]`. Everything below is tested against a wiremock storage server only; **no run
has written to the real cloud** (D2 below).

- **The gate (D2).** A real run that would write to the cloud (an upload, a cloud deletion, or
  a `both` in a sync, which uploads the kept copy) refuses with exit 7 and "nothing was
  changed" unless `GOGO_CLOUD_WRITES=1` is in the environment. It is checked after the plan
  and before the quota check, the question, any backup or any write. `--dry-run` and `status`
  never need it. The variable is for the one live test below; a later change removes the gate
  once the test has passed.
- **One runner.** `pull`, `push` and `sync` share the pull's order (§12e): sync lock, change
  lock and running refusal, `status`'s plan, conflicts, quota, the one question, the
  `cloud-pull` snapshot (only for local overwrites/deletes, so a push takes none), backups,
  D11 set-aside, downloads and local deletions, then uploads and cloud deletions, then a
  re-list. A push shows downloads and local deletions as `for pull`, as a pull shows uploads as
  `for push`; neither is exit 8.
- **Upload** (`storage::put_object`, the storage client only, so the Galaxy UA stays on that
  host): `PUT` of gogo's deterministic gzip (level 6, no name, mtime 0) with
  `Content-Encoding: gzip`, `Content-Type: application/octet-stream`, `Etag` (the MD5 of the
  gzip bytes) and `X-Object-Meta-LocalLastModified` (the file's mtime, whole seconds, UTC,
  RFC 3339). An `Etag` in the answer that isn't ours is `corrupt` (exit 5). The local file is
  read through the pull's no-link walk, checked against the plan (device, inode, size, mtime
  and the plain MD5 the plan took): a file changed meanwhile is left (`changed_since_plan`).
  One at a time. A file whose gzip would exceed 200 MiB is left (`too_large`). **Empty files
  are never uploaded** (`not_uploaded: empty_file`, exit 0): an empty gzip is a tombstone
  (§12b), so every client would read the save as deleted.
- **Never uploaded:** build files, excluded files and unreadable ones (the plan never lists
  them), and anything in a location whose dir is missing or emptied. `location_missing` is
  left by every mode; `location_emptied` is left by a push and a sync (a pull settles it like
  any conflict), so not even `--prefer local` turns "the save dir is empty" into deleting
  every save in the cloud.
- **Cloud deletion** (`storage::delete_object`, **unverified live**): an HTTP `DELETE` of the
  object URL, a 404 counting as done. Whether the cloud then lists a tombstone or nothing
  isn't known; both read as deleted (§12b), and the post-write re-list notes a deletion that
  left a live object (the record then keeps the file's old entry so it isn't taken for new in
  the cloud). If the live test shows `DELETE` isn't honoured, the fallback is a `PUT` of an
  empty gzip with no `X-Object-Meta-LocalLastModified`, which is how Galaxy's tombstones look;
  it is one function. Just before the `DELETE` the local file is checked to still be absent.
  `--no-delete` turns cloud deletions off (`not_uploaded: no_delete`, the record untouched);
  local deletions of a sync stay.
- **Cloud backups (D12):** every upload over a live cloud object and every cloud deletion
  first downloads the object (MD5 checked) into
  `cloud-saves/backups/<gogid>/<UTC>/cloud/<location>/<path>` as the stored gzip, with the
  listing entry beside it (`<leaf>.gogo-entry.json`), in the same dir as the local backups
  and pruned with them. All backups are done before the first write; a failed one stops the
  run, an object that no longer matches its listing (another machine pushed meanwhile) is
  never overwritten (`corrupt`, a note). A tombstone is overwritten without a backup (an
  empty gzip holds nothing). A brand-new object can't be checked for a concurrent creation.
- **The question:** asked once, after the conflict answers, when anything is overwritten or
  deleted on either side; the text counts local overwrites/deletions and cloud overwrites
  (uploads over a live object)/deletions. `--yes` skips it; with no TTY and no `--yes` the exit
  is 2 and nothing is changed.
- **Quota (§5.4):** before any write, the gzip size of every planned upload (counted on the
  file as it will be uploaded, the replaced objects' sizes subtracted) plus the container's
  `bytes_used` is held against the smaller of the container's `x-container-meta-quota-bytes`
  and the remote config's `cloudStorage.quota` (`Resolved::quota`). Over it: exit 1 with the
  numbers, nothing written. A kept copy of a `both` counts as its plain size plus 1%. Unknown
  numbers skip the check.
- **Conflicts:** `push` offers `local` (upload, or delete the cloud object when the file is
  gone; `skip` otherwise) and `--prefer local`; a conflict it can't settle is left (exit 8).
  `sync` offers `cloud`, `local`, `both`, `skip`. `--prefer cloud|local` settle every
  conflict that way; `newer` `HEAD`s each conflicting object that has a file on both sides
  and compares `X-Object-Meta-LocalLastModified` with the local mtime; within 2 s, or with no
  usable header, the conflict stays (asked, or exit 8), and it is refused after a rollback
  (exit 7, design 20 §6.3). `--prefer newer` is the one option that sends `HEAD`s in a dry
  run (read-only).
- **`both` in a sync** uploads the kept `<name>.local-<UTC>` copy in the same run (the point
  of keeping both across machines), as a new object; a `both` in a pull doesn't.
- **Record:** after each upload, the gzip's MD5 as `cloud_hash`, the plain MD5, size and
  mtime; after each deletion the entry is dropped; each saved atomically right away, so a
  failed upload (network, 5xx) stops the run with exit 4 and the record never moves past it;
  a rerun continues. The final re-list only notes a listing that doesn't show an upload (it
  may lag). `pending_push` is cleared by a push or sync that leaves nothing to push (every
  file done, an empty file, or a download's business); a pull never touches it.
  `rolled_back` is cleared when every planned file was done, as for a pull.
- **Dry run:** a real or dry run of `push`/`sync` plans with the same code; `--dry-run` takes
  no lock and sends only `GET`s (the tests assert that no `PUT`, `POST` or `DELETE` reaches
  the server, with and without the gate).
- **Output:** `upload`, `delete cloud`, `for pull`, `not uploaded` lines next to the pull's;
  `--json` is the same report with `mode`, and `pending_push_cleared`. No token, secret, user id
  or client id.

### The live write test (D2, not run)

Nothing in #462 has written to the real cloud. The test is run once, by hand, on a game the
maintainer names, with their explicit OK, on a scratch copy of gogo's state, with a throwaway
file. The steps:

1. Pick an owned game with cloud saves enabled and an empty (or throwaway) container; point
   `XDG_DATA_HOME`/`XDG_CACHE_HOME` at scratch dirs under `target/` so no real record or
   backup is touched, with the game's dir linked in.
2. `gogo saves status <gogid>` (read-only) and note the object count.
3. Put one small file with a distinctive name in one of the game's save locations
   (`gogo saves locations <gogid>` lists them), e.g. `gogo-d2-test.txt`.
4. `gogo saves push <gogid> --dry-run`: only that file is planned as an upload.
5. `GOGO_CLOUD_WRITES=1 gogo saves push <gogid> --yes`: one `PUT`. Check `status` shows the
   file as the same on both sides, and the listing's `hash` equals the MD5 of the gzip.
6. Read it back: `gogo saves pull <gogid> --dry-run` shows nothing to do; on a second scratch
   state, `gogo saves pull <gogid>` downloads the same bytes and mtime (the
   `X-Object-Meta-LocalLastModified` round trip).
7. Check it in GOG Galaxy or Heroic (the file shows up and downloads there).
8. Delete the local file; `GOGO_CLOUD_WRITES=1 gogo saves push <gogid> --yes`: one `DELETE`.
   Record whether the listing then shows a tombstone (the empty-gzip hash) or no entry, and
   that Galaxy/Heroic treat it as deleted. The cloud backup under `cloud-saves/backups/` holds
   the deleted object.
9. Undo: the delete in step 8 already removed the object; if anything is left, delete the test
   file in Galaxy or Heroic, or push again with the file removed. `--no-delete` was never
   needed. Remove the scratch dirs.
10. Record the findings in this section (delete mechanism, tombstone or not, quota
    behaviour) and open the change that removes the gate.

## 13. Implementation issues (to be filed)

Under epic #155 (Linux runtime), `type:feature`, `area:wine-runtime` + `area:auth` /
`area:cli` where listed; the Mac one under #50 (`area:mac-app`).

| # | Title | Scope | Depends on |
|---|---|---|---|
| C1 | cloud saves: client credentials and per-game token | `cloudsaves::auth`: credentials from `build-manifest.json` or a fetched manifest checked against `goggame-*.info`, the separate in-memory `Core` exchange, `no_license` handling, redacting types, the storage HTTP client and UA, URL redaction for the host (`area:auth`) | — |
| C2 | cloud saves: remote config and location resolving | remote-config fetch and cache, location parsing, variable mapping for both lanes and both build lanes, `__default`, excludes, `gogo saves locations` | — |
| C3 | cloud saves: storage client (read side) and `gogo saves status` | list (JSON), download with MD5 check and gunzip, tombstones, the record, classification (§4.2), `status` and `--json`, mock storage tests (`area:cli`) | C1, C2 |
| C4 | cloud saves: `pull` with backups and the `cloud-pull` snapshot | downloads and local deletes, backups and pruning, locks and refusals, `--prefer`, conflict prompts, exit 8, snapshot trigger (design 20 I5) | C3 |
| C5 | cloud saves: `push` and `sync` (after D2's write test) | gzip upload, cloud deletes per D2, quota pre-check, cloud-side backups, `sync`, `--no-delete` | C3, D2 |
| C6 | cloud saves: rollback interplay and save roots | `rolled_back` set by `snapshot rollback`, forced conflicts, the rollback's plan line, the locations as design 20's save roots (offline, from the cache) | C3, design 20 I4 |
| C7 | linux-app: launch-time sync | `saves auto`, the first-build question and `--cloud-saves`, `launch.conf` key, pull before / push after in `launch.sh` (`LAUNCHER_VERSION` + 1), the dialog, `--launcher` budget, `pending_push`, `apps show`/`doctor` | C4, C5 |
| C8 | mac-app: cloud saves | the `crossover` mapping confirmed on a real bottle, the Mac launcher's hooks, `apps show`/`doctor` (epic #50, `area:mac-app`) | C7 |

C1 and C2 can go in parallel. #275 stays open until C7; C8 and native builds (§11) get their
own issues and don't block it.
