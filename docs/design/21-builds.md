# 21 — Builds: list them, pick one, see what's newer (#360)

Status: **accepted** (2026-11-12). D1–D10 are accepted as written, and the open questions are
answered in §11.

The builds GOG serves for a game, and the choice of one: `gogo builds <game>` lists them,
`--build <id|version>` on `gogo depot` and `gogo linux-app` picks one instead of the newest,
and `apps list` / `apps show` / `doctor` say when a built game is behind. This is the
groundwork for an update command for built games. That command is a later issue, sketched in
§7 so that what this design records is what it will need.

Scope: the depot lane (design 14, 18) and the offline-installer lane (§6) on Linux. The Mac
lane is on hold. Nothing here rules it out: the listing takes an OS, and `content.toml` is
OS-neutral (design 19 §6.1).

**Why its own doc.** Builds cut across design 14 (the depot cache and its record), design 18
(rebuilds), design 19 (DLC and build matching, §4) and design 20 (the update trigger). The
update command will be the biggest user, and it needs one place that says what a build is,
which one is "newest", and what gogo keeps about the installed one.

## 0. Summary

| Question | Answer |
|---|---|
| Listing | `gogo builds <game>`: every build GOG's builds endpoint returns for the OS (§2): id, version, date, generation, branch, download size, DLC, and where it is used here (the built game, the depot cache). `--json`. |
| How many | Only what GOG returns: a handful of recent builds (2–6 in the games tried), never `total_count` (5–270). Paging parameters are ignored (§1.2). The footer says how many GOG counts. |
| Newest | The newest **public build on the default branch** (`branch` null). Builds on a named branch (Cyberpunk's `1_63_legacy_patch`) are listed, but never picked on their own (§1.3, D3). |
| Pick | `--build <id\|version>` on `gogo depot` and `gogo linux-app <game>` (build and rebuild). It matches an exact build id, else a version name exactly. Unknown or ambiguous: exit 2 with the list (§3). |
| Hint | `apps list`, `apps show` and `doctor` compare the installed build with a **cached listing** (`<cache>/gogo/builds/<os>/<gogid>.json`), with no network call. Commands that fetch the list anyway refresh it. `gogo builds --installed` refreshes every built game on request (§4). |
| Kept for updates | The installed build's manifest goes into the game dir as `build-manifest.json` (a few KB), so an update can diff from it after GOG stops listing the build (§5, D6). |
| Installer lane | GOG serves the newest installer only. `gogo builds` shows its version and GOG's patch files; `--build` is refused there, and the hint compares with the catalog's version (§6). |
| Update (later) | `gogo linux-app update <gogid> [--build X]`: gog-warp `upgrade_from(installed manifest)` on the depot cache, then design 19's journaled lay onto `game/` (now with deletions), DLC kept, and the `before update <old> → <new>` snapshot first (§7). It depends on #362. |

## 1. Facts (live, read-only, 2026-11-12)

The builds and manifest listings of 10 owned games were read with gogo's own gog-warp
`Core` (`get_builds`, `get_manifest`) and small probes using the same session. These were
listings, manifests and patch indexes only. No game data was downloaded and no token was
printed.

### 1.1 The endpoint

`GET content-system.gog.com/products/<id>/os/<windows|osx>/builds?generation=2&_version=2`,
the call gog-warp's `get_builds` makes (with a bearer token when signed in, plus
`password=` when given).

- Top-level keys: `total_count`, `count`, `items`, `has_private_branches`.
- An item: `build_id`, `product_id`, `os`, `branch` (null, or a name), `version_name`,
  `tags`, `public`, `date_published`, `generation` (1 or 2), and `urls`. A generation-1 item
  also has `legacy_build_id`. gog-warp's `Build` has every field but `legacy_build_id`, and
  all its getters are public.
- `urls` are the build manifest on two CDNs (`gcore`, `fastly`). They are unsigned,
  content-addressed paths (`content-system/v2/meta/<aa>/<bb>/<hash>`) with no token or
  query, so they can be kept. The manifest is public: an unauthenticated request gets the
  same list.

| Game | `count` / `total_count` | Generations listed | Notes |
|---|---|---|---|
| Cyberpunk 2077 | 6 / 270 | 2 | `1.63_Hotfix` on branch `1_63_legacy_patch` (2023), public |
| SimCity 2000 Special Edition | 2 / 5 | 2, 1 (2015, version `""`) | |
| SimCity 4 Deluxe Edition | 2 / 7 | 2, 1 (version `""`) | |
| Heroes of Might and Magic 3: Complete | 5 / 11 | 2 ×4, 1 | languages per build: 4, 4, 3, 2, 2 |
| Carmageddon TDR 2000 | 1 / 2 | 2 | |
| Warhammer 40,000: Chaos Gate | 5 / 17 | 2 | three more gen-1 builds only under `generation=1` (§1.2) |
| Anno 1701 A.D. | 2 / 2 | 1 ×2 | **both named `2.03`** |
| Right and Down | 1 / 2 | 2 | |
| SimCity 3000 Unlimited | 2 / 7 | 2 | |
| Pinball Spire | 5 / 19 | 2 | five builds in seven months |

### 1.2 What the list holds

- **Only a few recent builds.** `page=2`, `limit=100` and `offset=6` all return the same
  items as the plain call. `total_count` counts builds that can't be listed. Nothing found
  lists the rest.
- **`generation=1`** (without `_version=2`) returns only generation-1 builds, with a `link`
  in place of `urls`. For Chaos Gate it lists three 2015 builds that the generation-2 call
  leaves out. Their manifests still answer `200` on the CDN. gog-warp's `Build` requires
  `urls`, so these items don't deserialize through gog-warp. gogo doesn't list them (D4).
- **The CDN keeps old content.** Manifests of 2015 builds still answer, so a build that left
  the list can likely still be fetched if gogo kept its manifest URL or the manifest itself
  (§5). This is not proven for a generation-2 build that rotated off the list: no such build
  id was at hand.
- **Version names are not unique** (Anno 1701: two `2.03` builds) and can be empty (old
  generation-1 builds). An id always works; a version name works only when it is unique.
- **Branches.** Every listed build in the sample was `public: true`. Only Cyberpunk has a
  named branch. `has_private_branches` was `true` for every game tried, so it says nothing
  about one game. A wrong `password` returns the same list with no error. A right one couldn't
  be tried (none is known), so the password-protected case is unverified.
- **A build's languages and DLC differ.** Heroes 3's builds range from 4 languages down to 2,
  and Cyberpunk's `2.5` and `1.63_Hotfix` carry one of the two DLC that the other builds
  carry (Phantom Liberty is missing from `1.63_Hotfix`, design 19 §11.2).
- **Cost.** One listing takes about 40 ms (10 games in sequence: 364 ms). A build manifest
  takes 80–380 ms. Cyberpunk's is 3.5 KB compressed (33 KB JSON, 90 depots) and gives the
  build's size per language and its DLC set (`install_size`, `dlcs()`), so a size and DLC
  column for 6 builds costs well under a second when fetched in parallel. A build's real
  download for an update (a diff) needs the depot manifests of both builds. That is the
  update's plan (§7), not the listing.
- **The manifest holds the game's Galaxy `clientId` / `clientSecret`.** They are the
  product's, not the user's, and anyone can fetch them. gogo still never prints the
  manifest.

### 1.3 Patches and installers

- **Depot patches** (`content-system.gog.com/products/<id>/patches?_version=4&from_build_id=&to_build_id=`,
  gog-warp's `get_patches`, xdelta3): some pairs have one and most don't. Cyberpunk
  `2.5 → 2.5_RedModFix`: yes (11 depots, tiny). `2.31a → 2.5_RedModFix`: yes, **57 GiB
  compressed**. `2.3 → 2.5_RedModFix`, `1.63_Hotfix → 2.5_RedModFix` and the downgrade
  `2.5_RedModFix → 2.31a`: `404`. Chaos Gate and Pinball Spire: the newest pair yes, older
  pairs `404`. On a `404` gog-warp falls back to whole changed files; nothing breaks.
- **Installers** (`api.gog.com/products/<id>?expand=downloads`): one version per installer,
  the newest. Its `version` equals the newest depot build's `version_name` in all three games
  checked (Cyberpunk `2.5_RedModFix`, Heroes 3 `4.0 (3.2) GOG 0.1`, Chaos Gate
  `1.2 GOG v9`). The catalog's `patches` are GOG's patch installers with the version each
  leads to (Cyberpunk: 12 files across `2.31` to `2.5_RedModFix`; Chaos Gate: 4).

### 1.4 What gog-warp does on an upgrade (pinned 26c9e67, from source)

- `Builder::upgrade_from(old_manifest, old_build_id)` diffs the old depot set (old language,
  `old_dlcs`) against the new one, fetches only new or changed files into `!Temp`, applies
  patches where GOG has them (reading the old file from the install root), moves the results
  into place, and **deletes the files the new build dropped** (`report.deleted`). It works in
  either direction, so a downgrade is the same call.
- No switch turns patches off. A patch whose source file was changed locally fails its
  checksum.
- The diff report and the file lists are `pub(crate)`. gogo finds what changed with its own
  before/after file walk (`depot::Snapshot`, design 14 §4.1). It finds what was deleted by
  comparing the record's file list with a walk after the run.
- The `!Temp` move loop fails on directory entries over non-empty directories and on links
  (#362). An update hits the same path as `dlc add`, only harder (§7.5).

## 2. `gogo builds <game>` (requirement 1)

### 2.1 Shape (D1)

```
gogo builds <GAME> [--os windows|osx] [--lang CODE] [--password PW] [--patches] [--json]
gogo builds --installed [--json]
```

- `<GAME>`: id, slug or fuzzy title, as `gogo depot` resolves it (owned games only).
- `--os`: default `windows`, the only OS the depot lanes build from today.
- `--lang`: the language the size column is for. The default is the built game's recorded
  language, else `en-US`. A language a build lacks shows `-` in that row's size, not an error.
- `--password`: passed on as gog-warp does. A wrong one is indistinguishable from none
  (§1.2), so the footer says `with a password: GOG lists N builds` and nothing more.
- `--installed`: one row per built game on this machine (the installed build, the newest, and
  whether it is still listed). It fetches every listing, refreshes the cache (§4.2), and is
  the "check for updates" on request.

**Why a top-level command and not `gogo depot --list-builds`** (like `--list-languages`): the
list is about the game, not the depot download. It also covers the installer lane (§6) and
the built game, and `--installed` spans games. `depot --list-builds` would read as a
download option.

### 2.2 Table

```
$ gogo builds cyberpunk
Cyberpunk 2077 (1423049311), windows, en-US
  BUILD               VERSION         PUBLISHED   GEN  BRANCH              SIZE      DLC  HERE
* 60152385347545148   2.5_RedModFix   2026-10-22  2                        60.3 GiB  2    game, cache
  60148683633405433   2.5             2026-10-21  2                        60.3 GiB  1
  58989493373906337   2.31a           2025-09-16  2                        59.9 GiB  2
  58933689113538427   2.31            2025-08-28  2                        59.9 GiB  2
  58809037770927044   2.3             2025-07-16  2                        60.3 GiB  2
  56624919703786522   1.63_Hotfix     2023-06-23  2    1_63_legacy_patch   62.4 GiB  1
6 of the 270 builds GOG counts are listed; only listed builds can be downloaded.
* newest on the default branch. installer: 2.5_RedModFix (offline lane, §6)
```

- Rows are newest first by `date_published`, then `build_id` (as `pick_build` breaks ties).
- `SIZE` is the build's download size for `--lang` with no DLC (`install_size`). `DLC` is the
  number of DLC products in the build's manifest, owned or not. `--json` names them.
- `HERE` marks this machine's uses: `game` (the built game's `content.toml` `build_id`),
  `cache` (the depot cache record's `buildId`). An installed build that is no longer listed
  gets its own row at the bottom: `<id> <version> (installed; no longer listed by GOG)`, from
  `content.toml` and `build-manifest.json`.
- An empty `version_name` shows as `-`. A version name that two rows share is flagged in the
  footer: `2.03 names 2 builds; pick by id`.
- The manifests are fetched in parallel. One that fails gives `?` in its row and a warning;
  the command still exits 0.

### 2.3 `--json`

```json
{
  "gogid": "1423049311",
  "title": "Cyberpunk 2077",
  "os": "windows",
  "language": "en-US",
  "totalCount": 270,
  "newest": "60152385347545148",
  "installed": { "game": "60152385347545148", "cache": "60152385347545148" },
  "installer": { "version": "2.5_RedModFix" },
  "builds": [
    {
      "buildId": "60152385347545148",
      "version": "2.5_RedModFix",
      "published": "2026-10-22T15:28:42Z",
      "generation": 2,
      "branch": null,
      "public": true,
      "listed": true,
      "downloadSize": 64743211622,
      "diskSize": 66783240192,
      "dlcs": [{ "id": 1256837418, "title": "Cyberpunk 2077: Phantom Liberty", "owned": true }],
      "here": ["game", "cache"]
    }
  ]
}
```

`--installed --json` is an array of `{gogid, title, lane, installed: {buildId, version},
newest: {buildId, version} | null, listed: bool, checked}`.

## 3. `--build <id|version>` (requirement 2)

### 3.1 Where

- `gogo depot <game> --build X`: downloads build X into the cache.
- `gogo linux-app <game> --build X` (and `--from-depot`): builds or rebuilds the game from X.
  It needs the depot lane: with `--build`, a game that would otherwise go through its
  installer is refused (`--build picks a depot build; this game has none, or use --from-depot`).
- `--password` moves to `linux-app` too (it exists only on `depot` today), since a protected
  build can be built only with it.
- Not on `--retarget`, `dlc add` (always the installed build, design 19 §4), or `download`
  (installers, §6).

### 3.2 Matching (D2)

1. An exact `build_id` among the listed builds.
2. Else a `version_name` equal to X (exact, then case-insensitive), when exactly one listed
   build has it.
3. Else exit 2.

There is no partial or fuzzy match: picking the wrong build of a 60 GB game by a typo is
expensive. There is no `newest` keyword either, because leaving the flag out means that.

### 3.3 Errors (exit 2, usage)

```
gogo: no build "2.2" of Cyberpunk 2077 among the 6 GOG lists (of 270):
  60152385347545148  2.5_RedModFix  2026-10-22
  60148683633405433  2.5            2026-10-21
  …
  older builds can't be downloaded: GOG no longer lists them.
```

```
gogo: "2.03" names 2 builds of Anno 1701 A.D.; pick one by id:
  48303309026040603  2.03  2015-08-14
  48297303558037351  2.03  2015-08-12
```

- An id that looks like a build id but isn't listed gets the same first message. If it is the
  installed build, the message adds `this is the installed build; GOG no longer lists it`.
- With `--password`: `… among the N builds GOG lists with that password`.
- `--json` on `depot` and `linux-app`: the error is the usual `{error, …}` object, with
  `builds: [{buildId, version, published}]`.

### 3.4 What the pick changes

- **The plan** says which build and why: `build: 2.31a (58989493373906337, picked; newest is
  2.5_RedModFix)`. On a rebuild onto an older build than the installed one it adds
  `downgrade from 2.5_RedModFix: saves made by a newer version may not load`. No question
  is asked beyond the build's usual confirmation; `--yes` covers it. The rebuild's `before
  rebuild` snapshot (design 20 §7.1) is the way back.
- **DLC** are resolved against the picked build's manifest, as design 19 §2 already does
  against the newest one. A named DLC the build lacks gives design 19's `not in this build`
  (exit 2), with the build named. A rebuild that keeps the recorded set (design 19 §1.2) and
  meets a DLC the build lacks is refused the same way, with a hint to pass `--dlc` without it.
  It never drops one silently (D8).
- **Language**: the recorded language (rebuild) or `--lang` must be in the picked build. A
  build without it is exit 2 with the build's languages (design 14 §11's message with the
  build named). It never falls back to another language, because a rebuild would then change
  the game's language as a side effect.
- **Records**: the build writes `content.toml`'s `build_id` / `version` (and `branch` when
  set, §5) and `build-manifest.json`, as it does for the newest build.
- **Without `--build`** a build or rebuild takes the newest on the default branch, as today.
  A rebuild of a game on an older build therefore updates it, and the plan says so:
  `build: 2.5_RedModFix (newer than the installed 2.31a; --build 2.31a keeps it)`. A game
  doesn't remember a held build in this round (§11 Q1). A game on a named branch is rebuilt
  onto the default branch's newest too, unless `--build` names a build on its branch. The plan
  line names both branches then.

### 3.5 The depot cache on another build (D5)

There is one cache per product (`depot/<gogid>`, design 14). When it holds build Y and the run
wants X:

| Y's manifest | What the download does |
|---|---|
| available: Y is listed, or the built game is on Y and has `build-manifest.json` | `upgrade_from(Y)` → X: only new and changed files, with patches where GOG has them; dropped files deleted. No hashing of the tree. |
| not available | `verify()` against X, as today: hash the tree, fetch what differs. |

- The record is rewritten for X (`buildId`, `version`, `files`, `language`, `dlcs` held). Other
  languages held whole (#389) are kept only when the upgrade included them. Otherwise they are
  dropped from the record. The files stay, and a later switch re-checks them.
- A built game never shares files with the cache (it was cloned or copied, design 18 §6), so
  moving the cache to another build doesn't touch the game. It does make the cache useless as
  the game's `dlc add` source, which then stages in the game dir (design 19 §3.2, as today).
- Before the download, the cache record gets `upgrading: {from, to}`. A run that finds it
  (interrupted) verifies the cache against the record's target. That is correct but hashes
  the tree. Resuming without the hash needs gog-warp's `!Temp` state to be trusted, which the
  update issue can look into.
- The `upgrade_from` path depends on #362's preflight or fix: until then, a diff with
  directory entries over non-empty directories fails at the end of the download. Cyberpunk's
  current builds have only file entries (#362's scan). I3 ships the `verify()` row first and
  turns the `upgrade_from` row on with #362.

## 4. Installed vs newest without the network (requirement 3)

### 4.1 Where it shows

| Where | What |
|---|---|
| `apps list` | a `build` column: the installed version, with `↑ 2.5_RedModFix` when the cached listing has a newer build on the game's branch (the default branch unless `content.toml` names another, §11 Q2), or `(gone)` when the installed build isn't in it. `--json` rows gain `build: {id, version, newest: {id, version} \| null, listed, checked}`. |
| `apps show` | a `build` section: installed id, version, branch and date. Newest on the game's branch per the cache, when it was checked; for a game on a named branch, also `default branch: 2.5_RedModFix is newer` when it is, and the command that updates (today a rebuild: `gogo linux-app <gogid> --yes`; later `linux-app update`). |
| `doctor` | an `update` finding, **info** level (never warn or fail): `2.5_RedModFix is newer than the installed 2.31a (checked 2026-11-10)`. A **warn** when the installed build is no longer listed: `dlc add` and a later update can't fetch its files (design 19 §4), so a rebuild is the way forward. |
| build plan | as §3.4 |

No network in any of the three: they read the cache. With no cache entry they say `newest:
unknown (gogo builds <game> checks)`.

### 4.2 The cache (D7)

`<cache>/gogo/builds/<os>/<gogid>.json` (beside `products/`, the catalog cache, design 06):

```json
{ "schema": 1, "checked": "2026-11-12T09:14:03Z", "totalCount": 270,
  "items": [ /* gog-warp Build, serialized as it is, urls included */ ] }
```

- **Written by every command that fetches the list anyway**: `gogo builds`, `gogo depot`,
  `gogo linux-app` builds, `dlc list` / `dlc add` (they call `installed_build`), and the
  later update. This adds no request. Lists fetched with a password are not cached, so the
  hint never shows a protected build.
- **Staleness**: nothing expires it. The hint always says when it was checked, and `apps
  show` / `doctor` add `(gogo builds --installed refreshes)` when the check is older than
  `state.catalog_ttl_hours` (default 24). GOG publishes a build for a game every few weeks at
  most (Pinball Spire: five in seven months), so a day-old answer is usually right, and a
  wrong one only delays a hint.
- **Why the cache dir**: it is disposable. Deleting it loses only the hint, which the next
  network command restores. It holds the manifest URLs (unsigned, §1.1), which keeps a way to
  a build's manifest after the build leaves the list (§5).
- **Why no background check or check-on-launch**: `apps list` and `doctor` are offline today
  and should stay fast and predictable. The launcher makes no network
  calls, and an update check at launch would make starting a game wait on GOG. There is no
  periodic refresh either (§11 Q4).

## 5. Records (`content.toml`, `.gogo-depot.json`, `build-manifest.json`)

### 5.1 `games/<gogid>/content.toml` (design 19 §6.1)

```toml
schema   = 1
lane     = "depot"
build_id = "58989493373906337"
version  = "2.31a"
branch   = "1_63_legacy_patch"   # new, only when not the default branch
language = "en-US"
```

`branch` is new and optional. An older gogo ignores it (no `deny_unknown_fields` on
`Content`; only its `[[dlc]]` rows have it). It is what tells the hint to compare a legacy-branch game with the newest build on
*its* branch (§11 Q2), and the update to stay on the branch.

### 5.2 `games/<gogid>/build-manifest.json` (D6)

- The installed build's manifest as GOG served it: zlib, as fetched, with a small header
  file beside it (`build-manifest.toml`: `build_id`, `generation`, `fetched`). Cyberpunk's is
  3.5 KB.
- Written by a build (both through `--build` and the newest), and by the later update as part
  of its commit. Written atomically, before `content.toml`.
- **Why**: `upgrade_from` needs the old manifest. GOG drops builds from the list within months
  (Cyberpunk lists 6 of 270), and `match_build`'s `Gone` case would then block the update
  as it blocks `dlc add` today. With the manifest kept, the depot manifests it names are
  fetched by hash from the CDN, which still serves 2015 content (§1.2; unproven for a
  rotated generation-2 build, so the update falls back to a rebuild if a depot manifest
  `404`s).
- **Why in the game dir and not the cache**: it describes `game/`. A snapshot (design 20 §3)
  holds it with `content.toml`, so a rollback restores the matching manifest. `move` and
  `apps remove` treat it as one more file.
- Games built before this have none. They get one at the next rebuild or update. Nothing
  fetches it in the background, and `doctor` doesn't nag about it.
- Never printed or logged: it carries the game's Galaxy client secret (§1.2).

### 5.3 `.gogo-depot.json`

It gains `version` (the build's version name, for `gogo builds`' `HERE` column and `apps show`
without a network call), `generation`, and the transient `upgrading: {from, to}` (§3.5). It
stays one build per cache. Keeping two builds side by side in one cache is out of scope: they
share most paths, and a second cache per build doubles 60 GB games.

## 6. The offline-installer lane (requirement 4)

- GOG serves **one installer version, the newest** (§1.3). There is no build to pick, so
  `--build` on an installer-lane build is refused (exit 2): `installers come in the newest
  version only; --from-depot --build X picks a depot build`.
- `gogo builds` shows the installer's version in its footer (and `installer` in `--json`), and,
  with `--patches`, the catalog's patch installers and the version each leads to (Cyberpunk:
  12). gogo doesn't run patch installers. `gogo update` (design 03) re-downloads outdated
  installers, and a rebuild uses them.
- **Hint**: an installer-lane game's `content.toml` holds the installer's `version`. The hint
  compares it with the cached catalog entry's installer version (`products/<id>.json`, already
  cached with `catalog_ttl_hours`). No new cache is needed. The update path shown is `gogo
  update <game>`, then `gogo linux-app <game> --yes`.
- **Mapping to depot builds**: the installer version equals the newest depot build's version
  name in every game checked, so `gogo builds` marks the matching row `installer`. Nothing
  depends on the match: it is display only.
- **GOG's patch installers** (`patch_<game>_<from>_to_<to>.exe`) are not run. An
  installer-lane game is updated by re-downloading its installer and rebuilding (§11 Q5).

## 7. Basis for updates (requirement 5, a sketch, not designed here)

`gogo linux-app update <gogid> [--build X] [--yes] [--no-snapshots] [--json]`, a subcommand
like `dlc` and `snapshot`. (`gogo update` stays the installer re-download of design 03.)

### 7.1 Order

1. **Locks and recovery**: the game's lock (`relocate::lock_game`) for the whole run. A
   pending lay journal (design 19 §3.4) or snapshot operation is settled first, as `dlc add`
   does. It is refused while the game runs.
2. **Target**: the newest on the game's branch, or `--build X` (§3.2). The same build as
   installed: `up to date`, exit 0.
3. **Refusals** (exit 7 unless noted), each naming a rebuild as the way forward:
   - an installer-lane game (§6);
   - no installed manifest: not listed and no `build-manifest.json`;
   - a generation-1 build on either side (no patches, and gog-warp's v1/v2 diff across
     generations is untested);
   - an installed DLC missing from the target (exit 2, named). There is no `--drop-dlc`: once
     DLC removal (#353) exists, the update reuses it (§11 Q3);
   - the recorded language missing from the target (exit 2).
4. **Plan**: gog-warp `prepare()` with `upgrade_from(installed)` gives the download size,
   with patches included. The plan shows `update: 2.31a → 2.5_RedModFix, 57.0 GiB to
   download (patch)`, the DLC kept, and `downgrade` when it is one. Then the question.
5. **Snapshot**: `linuxapp::snapshots::before_change(gogid, dirs, guard, Trigger::Update,
   "before update <old> → <new>", opts, emit)` under the locks, after the refusals and before
   the first write (PR #415). It skips where it doesn't apply, prunes to `keep_auto`, and a
   failure stops the update with exit 7. `--no-snapshots` skips it for this run (design 20
   §7.1, §9.5).
6. **Source**: the depot cache, when its record is on the installed build with the recorded
   language and holds the installed DLC (design 19 §3.3's test). Otherwise the update is
   refused, naming a rebuild. The cache is never re-seeded from `game/` (§11 Q6).
7. **Download**: on the cache, `upgrade_from(installed manifest, installed build)`, with
   `old_dlcs` = `dlcs` = the installed set (DLC kept: their changed files come along,
   patched where GOG has patches for them) and `old_language` = `language`. Before that the
   record gets `upgrading` (§3.5), and the file snapshot is taken. The game is untouched
   and playable all along.
8. **Lay**: design 19 §3.4's journal and `lay_files`, with the changed files (after/before
   snapshot) laid onto `game/`, and one addition: `removed` (files the new build dropped,
   from the record's old file list minus the walk). Each is moved into the backup dir like
   a replaced file, so a rollback of the journal puts it back. The journal gets
   `kind = "update"`, `from`, `to`. The launcher and `doctor` refuse and fail on it as they
   do for a DLC add.
9. **Commit**: `build-manifest.json`, then `content.toml` (new `build_id`, `version`, each DLC
   row's `build_id`), then `finish`. Setup's next run reapplies the base and DLC scripts
   (design 19 §3.6). If the new `goggame-<gogid>.info` names another exe or arguments,
   `gogo-launch.toml` is rewritten, which changes the stamp, so the launcher re-runs setup.

### 7.2 What it keeps

The prefix, saves, L4, `launch.conf` and files in `game/` that no build lists (mods, the
game's own logs and configs) all stay. Only paths named by the old or the new build are
written or removed. A modded file that the update replaces goes to the backup dir and is
dropped at `finish`. The snapshot is what keeps it.

### 7.3 Interruption

As design 19 §3.4. During the download the game plays the old build, and a rerun verifies the
cache (§3.5). During the lay, the journal rolls forward or back. After a rollback to the
pre-update snapshot, `content.toml` names the old build again, so the hint shows the update as
available again (design 20 §9.5).

### 7.4 What this design puts in place for it

The listing and `--build` resolution (§2, §3), the default-branch "newest" (§1.3), the cached
listing for the hint (§4), `build-manifest.json` (§5.2), the cache's `upgrade_from` switch and
its `upgrading` marker (§3.5), and `branch` in `content.toml`.

### 7.5 gog-warp gaps

- **#362** (the `!Temp` move loop on directory and link entries) blocks any `upgrade_from`
  whose diff holds a directory entry over a non-empty directory. That is likely in updates
  (The Witcher's `Data\FX` is one such directory in #362's scan). The update issue depends on
  it: either gogo's preflight refusal (reading the depot manifests, as `dlc_filter` does), or
  a fix in the fork, sent upstream (AGENTS.md's fork rule).
- **No per-file report**: the diff, its patches and `deleted` are `pub(crate)`. gogo works
  around it with its own walks (§1.4); no gog-warp change is needed.
- **No patch opt-out**: a patch onto a locally changed cache file fails its checksum. The
  cache is gogo's and never run, and it is never re-seeded from `game/` (§11 Q6), so this
  doesn't arise.

No gog-warp change is made or needed for this design.

## 8. Interactions

| Case | What happens |
|---|---|
| Cache on another build than asked for | §3.5: `upgrade_from` when the old manifest is known (after #362), else `verify()` as today. The built game is unaffected. |
| Rebuild onto an older build | Allowed with `--build`. The plan says `downgrade` with the saves warning, and `before rebuild` is snapshotted. The cache moves to that build too (§3.5). Downgrade diffs have no GOG patches (§1.3): whole files. |
| DLC that the chosen build lacks | Named: exit 2, `not in this build` (design 19 §2.2). Kept set on a rebuild: refused with the `--dlc` hint, never silently dropped (§3.4, D8). |
| A language missing from a build | Exit 2 with that build's languages. There is no fallback (§3.4). `gogo builds --lang` shows `-` for it. |
| `dlc add` on a game behind the newest | Unchanged (design 19 §4): adds for the installed build while it's listed. Once the build leaves the list, the refusal stays: "update first" is the rule, and gogo doesn't fetch DLC depots for an unlisted build (§11 Q7). |
| Snapshot rollback across builds | `content.toml` and `build-manifest.json` come back together. The cache stays on whatever it holds, and the next build or update handles it via §3.5. |
| `move` / `apps remove` | `build-manifest.json` is one more file in the game dir. |
| Mac lane | `--os osx` lists builds; nothing builds from them until the lane resumes. |

## 9. Testing (design 08)

- **Pure**: `pick_build` with a named branch newer than the default one (it must not pick
  it). `--build` resolution: id, unique version, case-insensitive version, duplicate version
  (Anno's shape), empty version, unlisted id, the installed id that isn't listed. Fixtures are
  the probe's listing shapes, without `urls` hashes from real games.
- **Records**: `content.toml` with and without `branch` reads in both directions. The listing
  cache round-trips gog-warp's `Build`. `.gogo-depot.json` `upgrading` written and read.
- **Hint**: `apps list` / `apps show` / `doctor` with no cache, a fresh one, a stale one, and
  the installed build gone.
- **Cache switch** (with #362): gog-warp `upgrade_from` on a fixture cache with synthetic
  manifests: changed, new and deleted files land, and the record is rewritten. An interrupted
  run sees `upgrading` and verifies.
- **No live game data in CI.** A host pass (in the implementing PRs): `gogo builds` on
  Cyberpunk and Anno 1701, `gogo depot cyberpunk --build 2.5 --dry-run`, and a rebuild of a
  small game (Pinball Spire, 0.4 GiB) onto its previous build and back, which needs @noosxe's
  OK as a game download.

## 10. Decisions

Decided on 2026-11-12: every answer below was accepted as written.

| # | Question | Decision |
|---|---|---|
| D1 | Name and place of the listing? | **`gogo builds <game>`**, top level, with `--json` and `--installed`. It covers the depot and installer lanes and the built game, which `depot --list-builds` wouldn't (§2.1). |
| D2 | What does `--build` match? | **An exact build id, else a version name that is unique** (exact, then case-insensitive). No fuzzy match and no keyword. Unknown or ambiguous is exit 2 with the list (§3.2): picking the wrong 60 GB build by a typo is the cost avoided. |
| D3 | Which build is "newest"? | **The newest public build on the default branch** (`branch` null). Named branches are listed and pickable, never picked on their own. Today's `pick_build` ignores the branch, so a legacy branch published later would win; this fixes that (§1.2). |
| D4 | List the generation-1 builds that only a `generation=1` query returns? | **No**: gog-warp can't load them (no `urls`), they are from 2015, and the generation-2 list already includes a generation-1 build where GOG offers one (§1.2). |
| D5 | The depot cache on another build? | **`upgrade_from` the cached build when its manifest is known, else `verify()`**. One build per cache. The `upgrade_from` path waits for #362 (§3.5). |
| D6 | Keep the installed build's manifest? | **Yes, `build-manifest.json` in the game dir** (a few KB), so an update can diff from a build GOG no longer lists. A snapshot keeps it in step with `content.toml` (§5.2). |
| D7 | Where does the hint's "newest" come from, and how fresh is it? | **A per-game listing cache in the user cache dir**, written by every command that fetches the list anyway, never expired, always shown with its date. No background or launch-time check; `gogo builds --installed` refreshes on request (§4.2). |
| D8 | A chosen build lacks an installed or recorded DLC? | **Refuse and name it** (exit 2) with the `--dlc` hint. Never drop a DLC silently (§3.4). |
| D9 | `--build` on the installer lane? | **Refused**: GOG serves the newest installer only. The hint compares with the cached catalog version (§6). |
| D10 | Name of the later update command? | **`gogo linux-app update <gogid>`**, since `gogo update` is the installer re-download (design 03) (§7). |

## 11. Open questions, answered

Decided on 2026-11-12.

| # | Question | Decision |
|---|---|---|
| Q1 | Should a game remember a build picked with `--build` (a hold), so that a plain rebuild keeps it? | **No hold in this round.** The plan line says what a plain rebuild does (§3.4). A later issue may add a hold for the "held for a mod" case (design 19 §4). |
| Q2 | Does a game on a named branch follow its own branch or the default one? | **Its own branch**, for the hint and the update. `apps show` says when the default branch has a newer build (§4.1). |
| Q3 | `update --drop-dlc` when the target lacks an installed DLC? | **No.** The update stays refused until DLC removal (#353) exists, and then reuses it (§7.1). |
| Q4 | Should `apps list` refresh old listings when online? | **No: it stays offline.** Only commands that fetch the list anyway, and `gogo builds --installed`, refresh the cache (§4.2). |
| Q5 | Run GOG's patch installers on the installer lane? | **No.** An installer-lane update is a re-download plus a rebuild (§6). |
| Q6 | No usable cache at update time? | **Refuse and point to a rebuild.** The cache is never re-seeded from `game/` (§7.1). |
| Q7 | `dlc add` after the build left the list? | **"Update first" stays the rule.** gogo doesn't fetch DLC depots for a build that is no longer listed (§8, design 19 §4). |

## 11a. As built (I1, #424)

- `gog/builds.rs` holds `pick_build` (re-exported from `gog::depot`), the `Listing` type
  (gog-warp `Build`s plus `total_count`), `BuildEntry` rows, `Listing::resolve` with
  `BuildResolveError` (`Unknown`, `Ambiguous`, messages as §3.3), and the cache functions.
- The cache path is §4.2's `<cache>/gogo/builds/<os>/<gogid>.json` (the §0 summary row
  said it without `<os>`). The file carries `schema`, `checked`, `totalCount` and `items`.
  A listing fetched with a password is never written.
- `builds::fetch_listing` fetches and writes the cache, best effort. `depot::installed_build`,
  `has_public_build`, `build_version`, `newest_build` and `depot::run` fetch through it, so
  every listing they read is cached with no extra request.
- Version matching: an exact hit counts first, so with `Beta` and `beta` listed, `beta` picks the
  second; `BETA` is ambiguous. An empty value never matches an empty version name.
- A password-protected build is a listed build with `public: false`.

## 12. Implementation issues (to be filed as sub-issues of #360)

Under epic #155 (Linux runtime) unless noted, `type:feature`, `area:depot` (+ `area:cli`
where there is CLI).

| # | Title | Scope | Depends on |
|---|---|---|---|
| I1 | builds: core listing, default-branch newest, `--build` resolution | `gog/builds.rs`: a listing type over gog-warp's `Build`, `pick_build` on the default branch (D3) with `match_build` following it, id/version resolution and its errors (§3.2–3.3), the listing cache (§4.2); fixture tests (§9) | — |
| I2 | `gogo builds <game>` and `--installed` | the table, sizes and DLC from manifests in parallel, `HERE` from `content.toml` and the cache record, the installer footer and `--patches`, `--json`; refreshes the cache | I1 |
| I3 | `--build` on `gogo depot` and `gogo linux-app` | the flag and `--password` on `linux-app`, the plan lines (picked, downgrade, newer-than-installed), DLC and language checks against the picked build (D8), `content.toml` `branch`, `.gogo-depot.json` `version`/`generation`; a cache on another build keeps `verify()` | I1 |
| I4 | installed vs newest in `apps list` / `apps show` / `doctor` | the `build` column and section, compared on the game's own branch with the default-branch note in `apps show` (§11 Q2), `doctor`'s `update` (info) and gone (warn) findings, both lanes (§4, §6); cache writes from `depot`, `linux-app` and `dlc list/add` | I1 |
| I5 | keep the installed build's manifest | `build-manifest.json` written by builds (§5.2), read by `gogo builds` for an unlisted installed build | I3 |
| I6 | depot cache: switch builds with `upgrade_from` | §3.5's first row, the `upgrading` marker and its recovery, the record rewrite, fixture tests | I3, I5, #362 |
| I7 | `gogo linux-app update` (the follow-up the issue names) | §7 as answered in §11 (Q3, Q5, Q6): refusals for a lacking DLC and for no usable cache, the installer lane via re-download and rebuild | I4, I6, design 20's #405 |

Not new issues, but requirements on existing ones (as comments): #362 (the update path is
its biggest user; I6 waits for it), #353 (the update reuses DLC removal for a target that
lacks an installed DLC, Q3). A hold (Q1) is left for a later issue.
