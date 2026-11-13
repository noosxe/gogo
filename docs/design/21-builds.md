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
  current builds have only file entries (#362's scan). I3 shipped the `verify()` row first.
  I6 turned the `upgrade_from` row on before #362's fix, with a fall back to `verify()` when
  it fails (§11f).

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
| Cache on another build than asked for | §3.5: `upgrade_from` when the old manifest is known, falling back to `verify()` if it fails (§11f); else `verify()`. The built game is unaffected. |
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
| Q3 | `update --drop-dlc` when the target lacks an installed DLC? | **No.** The update stays refused until DLC removal (#353) exists, and then reuses it (§7.1). Built (#442, §11h): the update takes such a DLC out in the same run, with no extra flag; the plan names it with the saves warning, and the question defaults to no. |
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

## 11b. As built (I4, #427)

- `gog/build_hint.rs` holds the pure comparison (`depot_hint`, `installer_hint`), the texts
  and `for_game`, which reads only `content.toml`, the listing cache and the cached catalog
  entry. Nothing fetches (Q4).
- The game's branch is the **installed build's branch in the cached listing**, not a
  `content.toml` field (it has none yet). A build the listing no longer holds is compared
  on the default branch. "Newer" is by date, then build id, among public builds of that
  branch.
- `apps list`: a `BUILD` column (`2.31a ↑ 2.5_RedModFix`, `2.0 (gone)`, or the version
  alone with no cache; a build with no version name shows its id). `--json` rows gain
  `build` (`id`, `version`, `branch`, `published`, `state`, `listed`, `newest`,
  `default_newer`, `checked`, `stale`); it is absent for a game with no build to compare.
- `apps show`: a `build:` block in the content section, with `newest: unknown (gogo builds
  checks)` with no cache, `(gogo builds --installed refreshes)` past `catalog_ttl_hours`,
  and `default branch: X is newer` for a named-branch game.
- `doctor` (Linux): `update` at a new **info** level (printed `info`, never fails) when a
  newer build is listed, and `update` at **warn** when the installed build is gone. Quiet
  with no cache or when up to date.
- Installer lane: the installed version against the cached catalog's Windows installer
  (the game's language when known). Versions can't be ordered, so any difference counts as
  newer; there is no "gone". The cache time is the catalog file's mtime.
- Cache writers: `depot`, `linux-app` builds and `dlc list` / `dlc add` already fetch through
  `builds::fetch_listing`, and no direct `get_builds` call remains outside it.

## 11c. As built (I2, #425)

- `gogo builds <game>` and `gogo builds --installed` (`gogo-cli/src/builds_cmd.rs`); the table,
  footer and JSON are pure code in `gog/builds_report.rs`. Every listing goes through
  `builds::fetch_listing`, so the cache is refreshed (not with `--password`).
- The table has an `ACCESS` column (`public` / `password`) only when a listed build is
  password-protected, so the common table stays as §2.2 draws it.
- The installer lane's footer line names the build whose version equals the installer's
  (`installer: V, the version of build B (offline lane).`); `--json` marks that build with
  `installer: true` and gives `installer: {version}`.
- `--patches` lists the catalog's patch installers for the OS (name, the version each leads to,
  language, size) under the footer; in JSON they are `installer.patches`.
- `HERE` reads the built game (`content.toml`, else derived as `dlc list` does) and the depot
  cache record. An installed or cached build that GOG no longer lists gets a row at the bottom;
  its version comes from `content.toml` only (`build-manifest.json` is I5's), and is `-` for a
  cache-only build.
- A manifest that can't be fetched shows `?` for size and DLC (null in JSON), warns on stderr and
  still exits 0. A language the build lacks shows `-`.
- `--installed` covers every game `apps list` does (the games dir plus location records). A depot game gets a fresh listing; an
  installer-lane game compares with the catalog's installer version (a catalog call per game).
  A game that can't be read, or whose check fails, is `unchecked` with the reason
  (`error` in JSON, `checked: null`). The status is `build_hint`'s (the one `apps list` shows, read right after the refresh):  `up to date`, `newer available`,
  `no longer listed`, `unknown`.
- `--json` gains `withPassword`; its `builds` also hold the unlisted installed build
  (`listed: false`, null date, sizes and DLC).

## 11d. As built (I3, #426)

- `--build <id|version>` is on `gogo depot` and `gogo linux-app <game>`; `--password` is on
  `linux-app` too (it needs `--build`). Both go through `Listing::resolve`
  (`builds::pick_for_run`), and an unknown or ambiguous value is exit 2 with the builds
  listed. A game with no listed build at all gets the D9 message instead. The `--json`
  error object of §3.3 isn't built: no command has one yet, so the error stays the usual
  `gogo: …` line on stderr.
- **Plan lines** come from `builds::plan_lines`, as the `build:` line of `linux-app`'s plan
  (further lines are indented under it) and as `gogo: build: …` lines of `gogo depot`:

  ```
  build:     2.31a (58989493373906337, picked; newest is 2.5_RedModFix)
             downgrade from 2.5_RedModFix: saves made by a newer version may not load
  build:     2.5_RedModFix (60152385347545148, newer than the installed 2.31a; --build 2.31a keeps it)
  ```

  A build on a named branch adds `on branch <name>`; a rebuild across branches adds `the
  installed one is on branch X, this on the default branch`. `gogo depot` doesn't change a
  built game, so it prints the picked line only.
- **Without `--build`, the build is the one the run takes.** A warm depot cache is used as
  it is (as before), so a rebuild with a cache on the installed build says nothing, and the
  `newer than the installed` line appears when the run downloads the newest (a cold or
  repaired cache). §3.4's "a rebuild updates" holds for those; the cache isn't forced onto
  the newest build here.
- **D8.** A rebuild that keeps the recorded DLC refuses one the build lacks
  (`DlcSelectError::KeptNotInBuild`, exit 2), naming it and the build, with the `--dlc`
  hint. `--dlc` names a DLC the build lacks: design 19's `not in this build`, with the build
  named by version and id. The check runs on every rebuild, not only with `--build`.
- **Language.** With `--build`, `--lang` or the game's recorded language (a rebuild) must be
  in the picked build, so it is never replaced by another (§3.4). A first build with no
  `--lang` keeps the default language's fallback.
- **D9.** `--build` with an installer path, a game tree path, or a game that GOG lists no
  build for is exit 2: `--build picks a depot build: installers come in the newest version
  only; --from-depot --build X picks a depot build`.
- **Records.** `content.toml` gains `branch` (only when the build isn't on the default
  branch; recorded when it is the build the tree holds). `.gogo-depot.json` gains `version`
  and `generation`; a rewrite for the same build (`dlc add`) keeps them, another build never
  inherits them, and a record without them reads and writes as before. `content.toml`'s
  `version` falls back to the record's.
- **A cache on another build** takes the existing `verify()` download (`CacheCheck::OtherBuild`
  repair path), asked for the picked build. The `upgrade_from` path is I6. Before the first
  file of another build is written, the record gets `upgrading: {from, to}` (§3.5); a record
  that has it reads as `CacheCheck::Interrupted`, never `Complete`, so the next run hashes the
  tree, and the record the finished download writes clears it. `--build` never builds from a
  cache with no record (exit 2, naming `gogo depot <game> --build X`), and the decline hints
  repeat `--build`.

## 11e. As built (I5, #428)

- `gog/build_manifest.rs` owns `games/<gogid>/build-manifest.json`: one JSON file
  `{schema, buildId, generation, fetched, manifest}`, where `manifest` is gog-warp's parsed
  manifest serialized back (unknown fields kept). This replaces §5.2's "zlib as fetched plus
  a `build-manifest.toml` header": one readable file, atomic by itself, no second file to
  keep in step. It is written with mode `0600` (the client secret is in it), its `Debug`
  shows only the ids, and no message or log names its contents.
- **Where it is written.** `linux-app` from a depot, first build and rebuild, right before
  `content.toml`, and only when its build id is the one `content.toml` records. The CLI
  fetches it with the listing request that already looked up the version name
  (`depot::build_info_with_manifest`), for the build the cache record names; the plan
  carries it in `DepotContent::manifest`. A write failure, or a build GOG doesn't list (no
  manifest to fetch), warns or stays silent and never fails the build.
- **Not written:** the installer lane, `dlc add` (the build doesn't change; the file is
  left alone), and old games until their next rebuild. `doctor` says nothing about a missing file.
- **Snapshots and rollback.** The file lives in the game dir, so a snapshot of the dir holds
  it and a rollback restores it with `content.toml`; no exclusion list touches it.
- **`gogo builds`.** An installed build GOG doesn't list gets its size and DLC from the
  file when its `buildId` matches `content.toml`'s (a file for another build, or an unreadable
  one, counts as none). The manifest has no publish date, so `PUBLISHED` stays `-`; the footer
  gives the date the manifest was fetched, and `--json` adds `manifestFetched`, `downloadSize`,
  `diskSize` and `dlcs` to that row. A game without the file keeps the bare row.

## 11f. As built (I6, #429)

- **§3.5's first row is on.** A cache whose record is for another build switches with
  `upgrade_from` when all of these hold (`depot::can_switch`):
  - the check says `OtherBuild`: a whole cache, not interrupted, nothing lost;
  - the wanted build is generation 2;
  - the record names its language, the wanted one, and its DLC.

  It also needs the cached build's manifest (`depot::cached_manifest`): the game dir's
  `build-manifest.json` when its `buildId` is the cache's (no request), else GOG's listing
  while it still lists that build. Both builds must name the same install directory
  (`depot::same_install_dir`, any case): gog-warp joins the new build's onto the root and
  diffs the paths below it, so across two dirs the unchanged files would be neither fetched
  nor moved. Chaos Gate's `1.2 GOG v5` (`chaosgate`) and later builds (`WH40K Chaos Gate`)
  are such a pair. Without all this, the run hashes the cache with `verify()`, as before. A
  language switch together with a build switch also hashes.
- **What is diffed.** The new side is the wanted DLC. The old side is the recorded DLC that
  are still wanted (`depot::switch_inputs`). A recorded DLC the run doesn't take is neither
  updated nor deleted: its files stay as they are, as `verify()` leaves them, and the new
  record drops it. Files of the old build that the new one dropped are deleted by gog-warp
  (`report.deleted`). Other held languages stay in the record, not whole, as they did with
  `verify()`, and a later switch to one re-checks it.
- **The plan line** says `cache holds build <id>: fetching only the files this build changed
  (no hashing) and deleting the ones it dropped`. The download figure stays the whole
  build's, as an upper bound.
- **The marker.** `upgrading {from, to}` is written before the downloader starts, as in I3,
  and only the new record clears it. A Ctrl-C or crash leaves it, and the next run hashes
  (`Interrupted`).
- **A failed switch falls back** (`depot::switch_or_verify`). When `upgrade_from` fails and
  the run wasn't cancelled, the run says so (a `note` event; `gogo: switching the cache from
  build <id> by its changed files failed (<error>); checking the cache (hashing) and fetching
  what's missing instead`), removes gog-warp's `!Temp` and runs `verify()` against the new
  build. The marker stays until that run's record is written, so a fallback that also fails
  leaves a cache that the next run hashes.
- **The fallback deletes what the new build dropped.** gog-warp deletes those files only
  after a successful move, and `verify()` never removes a file. So before hashing, the
  fallback fetches both builds' depot manifests (`dlc_filter::fetch_depots`, the run's
  language and DLC on each side) and removes the paths the old build lays and the new one
  doesn't, compared without case as gog-warp does (`depot::dropped_paths`,
  `depot::remove_dropped`). It removes only plain relative paths (no `..`), only files and
  links, never a dir, and never a path the old build doesn't list. A file of the user's own
  stays. When a listing fails, the files stay, and a `note` says so.
- **gog-warp's `!Temp`** sits in the game tree's dir (`<root>/<installDirectory>/!Temp`), not
  at the cache root. The record's walk skips it at either level. It is removed only by a
  switch: at its start (a whole cache has none of its own, so one there is stale), unless
  the run is already cancelled, and in the fallback. The removal is skipped, with a `note`,
  when the install directory is empty, absolute, or has a `.` or `..` component
  (`depot::warp_temp_dir`). A `!Temp` that a cancelled switch leaves stays until the next
  switch. A later build would copy it into `game/`, as one from an interrupted `dlc add`
  already can.
- **#362 on a real switch.** Warhammer 40,000: Chaos Gate (1433928487) switched from
  `1.2 GOG v9` (59612759543911543) to `1.2 GOG v8` (59609663239679307) and back, on a scratch
  copy of a cache. The diffs were 670.6 KiB and 63.9 KiB. Both builds have two
  `DepotDirectory` entries, `Save_G` and `ScreenShots`. Results:
  - both dirs empty in the cache, as a fresh download leaves them: both directions
    succeeded, and a hash check of the result found nothing to fetch;
  - a file put into `Save_G`: gog-warp failed with `Directory not empty (os error 39)`, the
    fallback hashed and fetched the 2–3 files that differed, and the result checked clean;
  - `1.2 GOG v8` → `1.2 GOG v6` (55396789143644982, 26.8 MiB) with that file: ENOTEMPTY
    again. The fallback removed the 4 files only v8 has (`Galaxy.dll`, `steam_api.dll`,
    `steam_appid.txt`, `directplay.cmd`), hashed, and fetched 17 files. The record matched
    v6 plus the user's file, and the result checked clean.

  So the bug needs a directory entry whose dir holds files, which a cache gogo downloaded
  only has when the build lists files inside it. None of the 26 cached test games has one,
  and #362's scan names The Witcher EE (`Data\FX`) and XCOM 2 as such games.
- **No offline test drives `upgrade_from` itself.** gog-warp's hosts are constants, and its
  downloader fetches depot manifests, secure links and chunks from them. Without a TLS
  stand-in for GOG's servers, a gogo test can't feed it local data. The test belongs in
  gog-warp, next to the move loop (#362's fix sketch). gogo's tests cover the decision,
  the inputs, the plan line, the fallback, the marker through failure and cancel, and the
  record's walk.

## 11g. As designed and built (I7, #430)

§7 was a sketch. This is the update command as built. Where it reads §7 one way out of
several, the PR lists the reading under "Decisions to confirm".

### The command

```
gogo linux-app update <gogid> [--build <id|version>] [--dry-run] [--yes]
                              [--no-snapshots] [--limit RATE] [--no-disk-check]
                              [--json-progress] [--json]
```

- **Target**: `--build` resolves as §3.2 (exit 2 on an unknown or ambiguous value), and an
  older build is allowed (a downgrade, said in the plan). Without it, the newest public
  build on the game's branch (§11 Q2): `content.toml`'s `branch`, else the installed
  build's branch in the listing, else the default branch.
- **Same build as installed**: `<title> is up to date (build <id>, <version>)`, exit 0,
  nothing written.
- `--dry-run` prints the plan and changes nothing (no recovery either). `--json` prints
  the plan (`{dryRun, plan}`) or the outcome. Without `--yes`, a terminal gets the
  question and anything else exits 2 asking for `--yes`, as `dlc add` does.
- `--limit`, `--no-disk-check` and `--json-progress` are the download's, as in `dlc add`.
- No `--password`: an update to a password-protected build is a rebuild with `--build`
  and `--password`.

### Refusals (before any write or download)

| Case | Exit | Message names |
|---|---|---|
| installer-lane game (§6, Q5) | 7 | `gogo update <gogid>` then `gogo linux-app <gogid> --yes` |
| the game is running, or another of its programs | 7 | quit it |
| a `dlc add` stopped during its download (`dlc-download.json`) | 7 | `gogo linux-app dlc <gogid> add` |
| an update stopped during its download, and `--build` names another target | 7 | the pending target |
| no installed manifest: not listed and no `build-manifest.json` | 7 | a rebuild |
| a generation-1 build on either side | 7 | a rebuild |
| the two builds lay their tree in different dirs (§11f) | 7 | a rebuild |
| an installed DLC missing from the target (Q3), when both builds' depot lists couldn't be fetched (since #442, §11h; before it, any such DLC, exit 2) | 7 | the DLC, its removal first (`gogo linux-app dlc <gogid> remove <id>`, design 19 §3.7) and then the update, or the rebuild with `--build` and `--dlc` without it |
| the recorded language missing from the target | 2 | the target's languages |
| no usable cache (Q6): none, another build, another language, lost files, an interrupted switch, or an installed DLC it doesn't hold | 7 | `gogo linux-app <gogid> --build <target> --yes` |

A pending lay journal (an add's or an update's) is settled first, whatever was asked, as
`dlc add` does. A pending snapshot rollback too. Exit codes follow §7.1: 7 for the
conversion class, 2 for usage, 130 for Ctrl-C.

### Order

1. **Plan** (network, no write): the listing (cached as every listing is), the target, both
   build manifests (the installed one from `build-manifest.json` or the listing), the
   refusals above, and the files the target drops, from both builds' depot manifests for
   the game's language and DLC (`dlc_filter::fetch_depots`, `depot::dropped_paths`). A
   failed depot listing is a note, not a refusal: the cache walk below still finds what the
   switch deleted. The plan says the versions, `downgrade` when it is one, the DLC kept,
   the language, the source, how the game files are laid (reflink or copy) and how many
   files the target drops, and the download as an upper bound (the whole build; only
   changed files are fetched). No `prepare()` call: it would need a downloader set up for
   the plan alone.
2. **Under the game's lock** (`relocate::lock_game`, held to the end): settle a pending
   rollback and a pending journal, then read the game again. If its build, DLC set,
   language or dirs differ from the plan's (another command ran in between), the update
   stops with nothing written: `changed since the plan: run the update again`. Then
   refuse a running game and check the cache again. A download record whose `from` is not
   the installed build (a snapshot rollback since) is stale: it goes, with a note, and the
   update is planned afresh rather than resumed.
3. **Snapshot**: `snapshots::protect(…, Trigger::Update, "before update <old> → <new>")`,
   which takes the data root's lock beside the game's. A failure stops the update with
   exit 7 and nothing written; `--no-snapshots` skips it. A resumed download took it on
   its first try.
4. **Download record** `games/<gogid>/update-download.json` (`{from, to, version, branch,
   before, dropped, fetched}`), durably, with `before` a walk of the cache (paths, sizes,
   mtimes, as `dlc add`'s). The target's manifest goes to `build-manifest.next.json`
   (mode 0600) beside it.
5. **Switch the cache**: `depot::run` for the target with the recorded language and the
   installed DLC. That is I6's path: `upgrade_from` when it can, the `upgrading` marker,
   the `verify()` fallback with `!Temp` cleanup and the prune of dropped files. The game
   is untouched and playable all along.
6. **Lay** (design 19 §3.4's journal, `kind = "update"`, `from`, `to`): the files the run
   wrote (the walk now against `before`) go onto `game/` by `dlc::lay_list` (tree paths and
   the support overlays' `app/`), and `removed` holds the paths to take out of `game/`:
   what `before` had and the walk now hasn't, plus the dropped list of step 1. Removed
   paths are mapped onto the game's casing, kept only when they are files or links in
   `game/` and no laid file takes the path, and moved into `.dlc-backup/` like a replaced
   file. Dirs they leave empty stay. A path is removed only while it is the installed
   build's file: its MD5 from the old build's depot manifest when it gives one, else its
   size (the manifest's, or the cache walk's for a file the switch deleted). A file that
   differs, one with neither to compare, and one under a symlinked dir (which would reach
   outside `game/`) stay in place, each a warning: `kept <path>, which build <to>
   dropped: <why>`.
7. **Commit**: `build-manifest.next.json` renamed to `build-manifest.json` (or the old one
   removed when the target's couldn't be fetched), then `content.toml`: `build_id`,
   `version`, `branch` (cleared on the default branch), and every DLC row's `build_id`
   and `version`. Then the backups, the journal and the download record go.
8. **Setup** runs once, as after `dlc add`, so GOG's install scripts of the new build are
   applied. A failure is a warning naming `gogo linux-app setup`.
9. **Entry point**: when the new build's `goggame-<gogid>.info` points to another exe
   than `gogo-launch.toml`'s, a warning names a rebuild (`gogo linux-app <gogid> --yes`),
   which rewrites the launch file. The update doesn't rewrite it itself.

### What it keeps (§7.2)

The prefix, saves, L4, `launch.conf`, `gogo-launch.toml` and the game's settings are not
written. In `game/` only paths the old or the new build lists are written or removed: a
mod or a log of the game's own stays. A modded file the update replaces or removes is in
the backup until the commit, and then only in the snapshot. Only `game/`, the backup dir,
the journal, the two records above and the depot cache are written.

### Interruption (§7.3)

- **Before or during the download**: `game/` and `content.toml` are as they were, and the
  game plays. A rerun of `update` finds `update-download.json`, takes its target (unless
  `--build` names another: refused), skips the cache check and the snapshot, and runs the
  download again: the cache's `upgrading` marker makes it hash. The files it wrote on the
  first try still count, since `before` is from before it. A finished download is laid
  only while the cache record still names the target: when another command moved the
  cache in between, the update stops, drops the record and says the cache moved.
- **During the lay**: the journal is there. The launcher refuses (its message names `dlc
  add`, which settles either kind), `doctor` fails naming `gogo linux-app update`, and the
  next `update` or `dlc add` settles it: forward commits the update as step 7 does, back
  restores the replaced and removed files from the backup. An update journal goes forward
  only while the cache record still names its `to`; otherwise it is rolled back, with a
  message that the cache is on another build now. After a back, the cache is on
  the target already, so the next `update` refuses for the cache and names the rebuild.
- **After a snapshot rollback** to `before update`, `content.toml` names the old build
  again and the hint shows the update again. The cache stays on the new build, so the
  next `update` refuses for the cache (Q6) and names the rebuild.

### Real runs

Warhammer 40,000: Chaos Gate (1433928487), in a scratch data root and cache, from a copy of
an existing cache. The game dir was laid out by hand on `1.2 GOG v8` (an empty prefix, setup
failing as a warning), with a file of the user's own in `Save_G`:

| Run | Result |
|---|---|
| v8 → v9 (newest) | `upgrade_from` fetched 3 files (1.4 MiB); 3 laid, 0 removed. `game/` matched the cache's tree plus the user's file; `build-manifest.json` on v9, mode 0600 |
| v9 → `--build "1.2 GOG v6"` | the plan said `downgrade` and `4 file(s) this build dropped`; 21 laid, 4 removed; `game/` matched the cache plus the user's file |
| v6 → v9 | 24 laid, 1 removed, matched; the next run said `up to date` |
| v8 → v9 → v6 again, `steam_appid.txt` (which v6 drops) edited before v9 → v6 | 21 laid, 3 removed; the edited file stayed, with the warning `kept steam_appid.txt, which build … dropped: changed since install`; `game/` matched the cache plus the user's two files |
| `--build "1.2 GOG v5"` on the real game, dry run | refused: another install directory |
| Cyberpunk 2077 (1423049311), dry run | `up to date`; `--build 2.31a` passed the DLC check and stopped at the cache refusal (a scratch cache) |

### Not built

- A `prepare()`-based download size (step 1).
- Rewriting `gogo-launch.toml` for a new entry point (step 9).
- DLC removal inside the update for a target that lacks one (Q3). Built since (#442,
  §11h).

## 11h. As built (#442): an update that drops a DLC

When the target build doesn't have an installed DLC, `linux-app update` takes it out in
the same run: one plan, one question, one snapshot, one journaled lay. The command and its
flags are §11g's. Before this, such a target was refused (exit 2), naming `dlc remove` and
then the update.

**Decisions (2026-11-13):**

1. **The target's file set.** The update's own diff covers most of it, as long as the
   DLC is on the old side of gog-warp's diff. `DepotOptions::dropping` names the DLC, and
   `depot::switch_inputs` keeps the recorded ones it names in `old_dlcs` (`dlcs`, the new
   side, leaves them out). gog-warp then deletes the DLC's files from the cache and
   rewrites the tree paths whose cached copy was the DLC's. The `verify()` fallback does
   the same: its prune reads `old_dlcs`, and the hashing fixes a rewritten path. The
   cache walk then gives the files to lay and the ones the switch deleted, as in §11g.
   What the walk can't see is a game path the DLC shares with a product that stays
   through another cache path: a support overlay's `app/` file laid over a base tree
   file. Its cache path is deleted, while the base's tree file may not change at all.
   So `dlc_remove::reverse_diff_to` works across the two builds. It keys every entry by
   its game path, takes "now" from the installed build's depots (base and every
   installed DLC), and "after" from the target's (base and the DLC that stay):
   - a path only the dropped DLC lay is **removed** while it still has the installed
     build's MD5 (else size);
   - a path the target lays for a product that stays is **laid again** from the switched
     cache in the target's version, unless `game/` already holds that version.

   The `removed` list holds the update's dropped files, what the switch deleted, the
   DLC's files and its `goggame-<id>.*` files by name (as design 19 §3.7). It never holds
   a path the target lays for a product that stays.
2. **Consent: no `--drop-dlc`** (Q3 stands). The plan names the DLC (`DLC removed: X
   (build B doesn't have it): N file(s) out`, and how many replaced files are laid
   again) and adds the saves line. On a terminal, the question repeats that line and
   defaults to **no**. `--yes` agrees, and headless runs need it, as for any update.
3. **The saves warning** is design 19 §3.7's. Its way back is the snapshot, or a rebuild on
   the installed build with the DLC (`gogo linux-app <gogid> --build <from> --dlc … --yes`),
   since `dlc add` can't bring the DLC back on a build that lacks it.
4. **The snapshot** is the update's (`Trigger::Update`), labelled `before update A → B,
   removing X`.
5. **`content.toml`**: the commit drops the DLC's rows, together with the new build on
   the rows that stay.
6. **The depot cache** ends up on the target without the DLC. Its files there are deleted
   (by gog-warp's diff, or the fallback's prune) and the new record doesn't list it. This
   differs from `dlc remove`, which keeps the DLC in the cache: here the cached files
   would be the old build's, and the target has no such DLC for a later `dlc add` to
   take.
7. **One journal**, the update's (`kind = "update"`), with `removing` (the DLC), `md5s`
   (each file laid again, by its game path) and `prune` (the dirs the DLC's removed files
   may leave empty, except dirs a product that stays lists a file under). The download
   record `update-download.json` gains `removing`. Recovery:
   - **forward** commits the build and the DLC's removal, then prunes those dirs;
   - **back** puts the DLC's files and the replaced ones back, so the DLC stays installed
     on the old build;
   - it goes back when the cache isn't on the target, or a file laid again from it no
     longer has its recorded MD5.

   When both builds' depot lists can't be fetched, the update is refused, exit 7, as `dlc
   remove`'s decision 7: without them there is nothing to check the DLC's files by. A path
   the target lays again that isn't plain (`..`, absolute) is refused in the plan.
8. **`--build`** shares all of it. A downgrade to a build older than the DLC is the
   common case.
9. **The offline-installer lane** has no update (§6, Q5). Its rebuild with `--dlc` decides
   which DLC stay.

**Order** is §11g's, with these additions:
- the plan computes the cross-build diff and hashes only the shared paths in `game/`;
- after the switch, each file laid again has its source in the cache checked (MD5, else
  size) before the lay. A mismatch lays nothing and drops the download record. The game
  stays on the old build with the DLC, and the update names the rebuild without it:
  the cache is on the target already, so a rerun can't retry;
- the commit is followed by the DLC's emptied dirs.

**Output.** `--dry-run --json`'s plan gains `removing`, `dlcFiles` and `dlcRestored`. The
outcome gains `dlcsRemoved`, and the human summary ends with `DLC removed: X`.

**Tests** (`linuxapp_pipeline`'s `update::drops_a_dlc`, on fake depot manifests of both
builds):
- a base file the DLC's overlay replaced, which the target keeps as it was: laid again
  from the cache, with no help from the switch;
- a tree file the DLC changed, which the target changes too: the target's version;
- a cache copy that isn't the target's after the switch: nothing laid;
- a changed DLC file: kept, with a warning naming the DLC;
- a stop at each lay step: forward, with the dirs pruned, and back.

Also: `dlc_remove::reverse_diff_to` across two builds, `switch_inputs` with `dropping`,
and the refusal without depot lists.

**Not run:** a real update that drops a DLC. No cached test game has a listed build older
than one of its DLC, so this is a host pass: a dry run's counts against the DLC's depot,
then the update, the game starting, and `gogo builds` and `apps show` on the new build.

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
