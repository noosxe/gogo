# 22 — GOG cloud saves (#275)

Status: **accepted** (2026-11-13). Facts in §1 are live-verified; the decisions are in §12
(D1–D13, all accepted as recommended). Writes to the cloud wait for D2's live test.

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
stream** (`aadd8693…1f3c`, 89 bytes as Galaxy writes it, no `X-Object-Meta-LocalLastModified`):
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
files that belong to the build (design 20 §6.1's build-file filter) are never pushed.

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
  tests pass explicitly. Both lanes find the game through `GameDirs::for_game`.
- **Client id**: the game's `goggame-<gogid>.info` (the game's files, then anywhere under
  `drive_c` for the installer lane), else the `clientId` field of the kept
  `build-manifest.json`; nothing else of the manifest is read.
- **Remote config cache**: `<cache>/remote-config/<clientId>.json`, fresh for 24 h; a failed
  fetch falls back to an older copy (`config_source: stale_cache`), no copy is an error
  (exit 4). `component_version` is the constant `2.0.45`; the D13 check is the ignored test
  `live_component_version_still_answers` (`GOGO_LIVE_CLIENT_ID=<id> cargo nextest run
  --run-ignored only live_component_version`).
- **Lookup** is case-insensitive per part: the exact case wins; two dirs that differ only
  by case make that one location skipped (`ambiguous`); a missing part keeps its exact case.
  The variable's own dirs (`drive_c`, `users`, `Documents`, …) go through the same lookup.
- **Escape guard** (`Roots::contains`): no `.`/`..`/`:`/NUL part, and the deepest existing
  ancestor, links resolved, must be under the canonical prefix or game dir (a dangling link
  counts as leaving). A link to the game dir from inside the prefix is therefore followed
  anywhere, not only for the depot lane's `drive_c/GOG/<name>`. `Location::file(rel)` does
  the same check for one object path; the pull and push use it for every file.
- **Skipped locations** are reported with a reason (`unknown_variable`, `not_a_variable`,
  `not_for_windows_build`, `bad_path`, `bad_name`, `duplicate_name`, `ambiguous`,
  `escapes_prefix`) in `--json` and as warnings in the text output; the others still resolve.
- **Excludes**: design 20's list plus `*.part`, `*.tmp`, `*.~incomplete`, as
  `cloud::locations::excluded`. The `<?INSTALL?>` build-file filter (§3.4) is **not wired
  in yet**: it needs the build's file list design 20 §6.1 builds per game.
- `__default` is resolved and labelled `unverified` (D10).

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
