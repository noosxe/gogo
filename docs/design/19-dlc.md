# 19 — DLC: choose it at the first build, add it to a built game

**Status:** contract (2026-11-11). Nothing implemented yet; the work is tracked under
#343 (issues in §12). Folds in #336.
**Builds on:** design 14 (depot lane, cache, `.gogo-depot.json`, events, §4.1 progress),
design 18 (`gogo linux-app`, the game dir, `goggame-<id>.script`, the library and `move`),
design 04 (offline-installer selection, `(product_id, dlc_id, file_id)` records), design
03 (CLI conventions), design 17 (ownership facts), design 15/16 (the Mac lane, on hold).
**Amends:** design 03 §1/§2 (DLC selection by id or title, D8's "a DLC selector resolves
to its parent" stays for game selectors), design 14 §1/§2 (`--dlc`, the record's DLC
set), design 18 §8 (`doctor`'s `dlc` check) and §10 (`linux-app` flags and `dlc`
subcommand).

**Why a new doc and not a section of 14 or 18:** DLC cuts across three lanes (the depot
download, the offline installers, and the builds on top of both) and two OSes. In 14 it
would describe build-time behaviour 14 doesn't own; in 18 (2600 lines) it would bury the
Mac half and the download lane. The doc says only what is new; it points at 14 and 18 for
the rest.

## 0. Summary

```
gogo linux-app <game> [--dlc <id|title>]... [--all-dlcs] [--no-dlcs] [--lang <code>]
gogo linux-app dlc <gogid> list [--json]
gogo linux-app dlc <gogid> add (<id|title>... | --all) [--dry-run] [--yes] [--json] [--json-progress]
gogo depot <game>    [--dlc <id|title>]... [--all-dlcs | --with-dlcs]
gogo download <game> [--dlc <id|title>]... [--all-dlcs | --with-dlcs]
```

- A DLC is named by its GOG product id or its title; it must be **owned**. Unknown and
  unowned names are usage errors that list the valid ones.
- A game remembers its DLC set, language and build in a new per-game file,
  `games/<gogid>/content.toml`. A rebuild keeps that set unless a flag changes it.
- **Depot lane, add to a built game:** fetch only the new DLC's files, for the *installed*
  build, through gog-warp's own old-manifest diff (no hashing of the whole tree), then lay
  the fetched files onto `game/` file by file, with a journal. No rebuild; the prefix,
  saves, settings and L4 are never touched. GOG's install script for the DLC then runs.
- **Depot cache (#336):** the cache record gains the DLC set and language; a cache
  missing a wanted DLC is incomplete and gets completed the same targeted way.
- **Offline-installer lane:** the DLC's own installer runs in the game's prefix into the
  base game's dir, after the base game, one at a time.
- Removal was out of scope for the first round (§3.5); it is built since, on the depot
  lane (§3.7).

### Decisions table

| # | Decision | § |
|---|---|---|
| D1 | Both a build flag set and a `dlc` subcommand, on one core | 1 |
| D2 | Select by product id or unambiguous title; owned only; errors list the valid ones | 2 |
| D3 | First build without a flag: none in `--yes`/non-interactive runs, a picker preselected with all owned DLC when interactive | 1.2 |
| D4 | A rebuild keeps the recorded set | 1.2 |
| D5 | DLC is always the installed build's; a build GOG no longer serves is refused, naming the update path | 4 |
| D6 | Depot add: gog-warp `upgrade_from(same manifest)` with `old_dlcs = installed`, into the cache (or a staging dir with no cache), then a journaled lay onto `game/` | 3 |
| D7 | `.gogo-depot.json` gains `dlcs` and `language`; an unrecorded set means a full verify | 3.3 |
| D8 | Offline lane: the DLC's Inno installer under umu with `/DIR=` the base game's dir | 5 |
| D9 | Record: new `content.toml` per game, not `gogo-launch.toml` | 6 |
| D10 | DLC follows the game's recorded language; changing language is a rebuild | 2.3 |
| D11 | Removal later (#343 follow-up), not in the first round; built since: depot lane (#353), offline-installer lane (#443) | 3.5, 3.7, 5.2 |
| D12 | macOS: same core and record; only the clone call and the subcommand's home differ | 9 |

## 1. CLI

### 1.1 Shape (D1)

Both forms the issue asked about, because they answer different moments:

- **On the build** (`gogo linux-app <game>`, and `mac-app` later): `--dlc <id|title>`
  (repeatable), `--all-dlcs`, `--no-dlcs`, `--lang <code>`. The first install says what
  comes with it, in one command, and a rebuild can change the set.
- **`gogo linux-app dlc <gogid> list|add`**: the game is built; adding to it must not look
  like a rebuild, and listing needs a home. It takes a `gogid` like every other post-build
  subcommand (`setup`, `doctor`, `move`), not a game selector.

Both call one core function, `dlc::install(game, Selection, opts) -> Plan`, so the picker,
the TUI and `--json` all build on it.

`gogo depot` and `gogo download` gain `--dlc` and `--all-dlcs` too. `--with-dlcs` stays as
a hidden alias of `--all-dlcs` (scripts and docs use it). Its meaning narrows: today it
takes every DLC in the build (`manifest.dlcs()`) or every `expanded_dlcs` entry, owned or
not (design 17 §1: `expanded_dlcs` is not an ownership list). From now it is **owned**
DLC only. An unowned DLC's depots can't be used anyway: GOG refuses the secure link for a
product the account doesn't own (`403`, `error: invalid_licence`, before any file is
fetched; §11.2 R4). So today's `--with-dlcs` is expected to fail at the first unowned DLC
in a build rather than skip it, which is why selection is owned-only (D2).

### 1.2 Which DLC a build gets (D3, D4)

| Situation | No DLC flag | `--dlc X` | `--all-dlcs` | `--no-dlcs` |
|---|---|---|---|---|
| First build, interactive (a TTY, no `--yes`) | picker, every owned DLC preselected | X | all owned | none |
| First build, `--yes` or no TTY | **none**, and the plan says `N owned DLC not included (--all-dlcs)` | X | all owned | none |
| Rebuild | **the recorded set** (`content.toml`) | exactly X (the plan names what's dropped) | all owned | none |

- **Why none by default when nobody is asked:** a DLC can be tens of GB (Phantom Liberty
  is a large share of Cyberpunk's build), and a script that ran `gogo linux-app <game>
  --yes` yesterday shouldn't download much more today because GOG added a DLC. The picker
  preselects all owned DLC because that's what a person installing interactively usually
  wants (it's GOG Galaxy's default too). **Decided (@noosxe, 2026-11-11, §11.1):** no DLC
  by default for `--yes` and headless first builds. For a game with one DLC, like
  Cyberpunk 2077's, installing an owned DLC is the obvious choice, but across games it
  isn't a rule that holds, so gogo doesn't guess. The plan's `N owned DLC not included`
  line stays, so the choice is visible.
- **Why a rebuild keeps the set:** a rebuild updates the game; quietly dropping
  Phantom Liberty because the flag wasn't repeated would be a regression the user only
  sees in the game. A rebuild that drops recorded DLC prints it in the plan and needs the
  same consent as the build.
- As built (#349): `linux-app` takes `--dlc` (repeatable), `--all-dlcs`, `--no-dlcs` (clap
  conflicts) and `--lang`; they reach the depot download as a `DlcSet`, a language and a
  `DlcContext`. `dlc::decide` is the table above; the picker is a `dialoguer` multi-select
  with every owned DLC ticked. A rebuild with no `--lang` keeps the recorded language. A
  cached tree is checked with the wanted DLC (and the language only when `--lang` was
  given), so a cache lacking a DLC is completed the targeted way. The offline-installer
  lane accepts the flags and the plan says DLC isn't installed there yet (#351). A failed
  `content.toml` write at the end of a build warns.
- A rebuild that adds DLC is a rebuild (the whole `game/` is re-materialized, design 18
  §10); `dlc add` is the cheap path for a game that's otherwise up to date.

### 1.3 `dlc list`

```
$ gogo linux-app dlc 1423049311 list
id          title                         owned  installed      download   disk
1256837418  Phantom Liberty               yes    no             29.2 GiB   30.1 GiB
1597316373  REDmod                        yes    yes (2.5)     288.5 MiB  605.0 MiB
```

(Ids and sizes are build 2.5_RedModFix's, from the research in §11.2; the `installed`
column is made up.)

- Rows: the union of the game's catalog `expanded_dlcs` (titles; design 04) and the
  installed build's manifest `products` (what can actually be downloaded on the depot
  lane). Goodies-only "DLC" (soundtracks, `bonus_content` only; design 17 §1) are not
  rows.
- `owned`: the id is in `owned_ids()` (`/user/data/games`, design 17 §1).
- `installed`: from `content.toml`; for a game built before this design, from the
  `goggame-<id>.info` files in `game/` (`info::gog_game_ids`, which already finds DLC
  infos one level down) with build `unknown`.
- Sizes: **depot lane** from the manifest, per DLC, without downloading any depot
  manifest: `install_size(lang, [dlc]) - install_size(lang, [])` (gog-warp sums
  `compressedSize`/`size` of the depots tagged with that `productId` and language).
  **Offline lane** from the catalog's DLC `downloads.installers[]` for the game's
  language and Windows. Unowned rows show no size (design 17 Q2: unowned DLC sizes are
  unverified).
- A DLC in the catalog but not in the build manifest is `not in this build`: it can't be
  added on the depot lane (an installer-only DLC; the offline lane can take it). On the
  depot lane it's also what an old build shows for a newer DLC: Cyberpunk 2077's
  1.63_Hotfix manifest lists REDmod but not Phantom Liberty (§4).
- A DLC can be in the build's `products` with **no depot bytes** (preorder or cosmetic
  entitlements: Journey to the Savage Planet's "Hot Garbage", Ghostrunner's "KATANA").
  It shows size `0`; adding it is a record-only change (§3.2).
- `--json`: `{gogid, lane, buildId, language, dlcs: [{id, title, owned, installed,
  installedBuild, inBuild, downloadSize, diskSize}]}`.
- As built (#350): `linuxapp::dlc::listing` and `render_list`. Rows are `available`'s
  plus any DLC `content.toml` lists that neither the catalog nor the manifest has.
  Sizes come from the installed build's manifest. When GOG no longer serves that build,
  the newest build's manifest is used and a note says so, naming the update. The table
  has a header line (`<gogid> (<lane> lane, build <id>, <language>)`), and notes go
  under it: the build is gone, the record is derived, the language is assumed, or
  `N owned DLC not installed`, the game's language isn't offered by the build, or a DLC
  has no files in it (design 14 §11). `--json` also has `installedVersion`, plus `notes`
  when there are any. An installer-lane game is listed from the catalog alone, with
  no sizes.

### 1.4 `dlc add`

> **Amended (design 20, #365):** with autosnapshot on, `dlc add` takes an automatic snapshot
> (`before adding <title>`) after its recovery and refusals and before its journal; `--no-snapshots`
> skips it for one run.

`gogo linux-app dlc <gogid> add <id|title>... | --all [--dry-run] [--yes] [--json]
[--json-progress] [--limit RATE] [--no-disk-check]`

- `--all`: every owned DLC in the build that isn't installed. Already installed ones named
  explicitly are a notice, not an error (a rerun after an interrupted add must succeed).
- The plan: the DLC with sizes, the build (`for build <id> (<version>), the installed
  one`), where the files come from (`depot cache` or `staging in the game dir`), the
  `game files:` method (reflink or copy, design 18 §6), the disk needed, and GOG's install
  script actions per DLC. Consent as the build: `--yes`, else a prompt; headless without
  `--yes` fails before anything is fetched.
- Refused while the game runs or a `move` holds its lock (the same `refuse_running` and
  `game-moves/<gogid>.lock` a move takes, design 18 §6.1), and on a game whose recorded dir
  is unreachable.
- As built (#350): `gogo linux-app dlc <gogid> add (<id|title>... | --all) [--dry-run]
  [--yes] [--limit RATE] [--no-disk-check] [--json] [--json-progress]`.
  - A bare `add` on a terminal opens the build's picker over the owned DLC not yet
    installed. Headless, it's a usage error listing them. A bare `add` that only
    settled an interrupted add (below) is done.
  - `--dry-run` writes nothing, and doesn't recover either. Its `--json` is `{dryRun,
    plan}`, where `plan` is `linuxapp::dlc::AddPlan`. The plan's lines: the DLC with
    sizes (`0 B: record only`), the build line, the newer build with the update command
    when there is one, `files from:` (`the depot cache` or `staging in the game dir`;
    `the depot cache (already there, no download)` when the cache holds every DLC
    added, §3.2), `game files:` (`reflink`, `copy` or `moved from staging`), the
    download (`no download` when nothing is fetched) and the disk, and the number of
    registry values in each DLC's install script when that script is already in the
    source.
  - The add's `--json` is `{gogid, buildId, language, dlcs, added, recovered?, lay?,
    warnings?}`.
  - A generation 1 build is refused, naming a rebuild: the targeted diff is generation 2
    only (§3.3).
  - An installer-lane game runs the DLC's installers instead (#351, §5.1).

## 2. Selection (D2)

### 2.1 Matching

One resolver, `dlc::resolve(names, candidates) -> Result<Vec<DlcId>>`, used by every command:

1. A name that's all digits is a product id: it must be one of the game's DLC.
2. Otherwise a title: case-insensitive, whitespace-collapsed. An exact match wins; else a
   unique substring match; else an error. The base title is stripped as a prefix
   (`"Phantom Liberty"` matches `Cyberpunk 2077: Phantom Liberty`).
3. Two or more matches: an error listing them with ids. Never a guess: a build that
   downloads the wrong 20 GB is worse than a retyped command.

A DLC id given as the **game** (`gogo linux-app 1256837418`) keeps design 03's rule: it
resolves to its parent with a notice. It doesn't add that DLC (that would make the game
selector also a DLC selector, and a mistyped id would choose content).

### 2.2 Errors

Usage errors (exit 2), each listing what would have worked:

- `unknown DLC "x" for Cyberpunk 2077; its DLC: 1256837418 Phantom Liberty (owned), …`
- `you don't own "Phantom Liberty" (1256837418); owned DLC for Cyberpunk 2077: …`
- `"Phantom Liberty" (1256837418) isn't in build <id> on the depot lane; …` (installer-only
  DLC; the hint names the offline lane).
- `--dlc` and `--all-dlcs`/`--no-dlcs` together: clap conflict.
- The server disagrees with the owned set (a stale `owned_ids()`): a secure-link `403`
  with `error: invalid_licence` for a DLC's `productId` becomes the "you don't own" error
  above, with the same list. gogo never prints the response's `error_description`: it
  contains the account id.
  *Implemented (#346) as a probe, not a catch:* gog-warp's downloader doesn't surface the
  status, so before any file is fetched `depot` asks for each chosen DLC's secure link
  itself (`dlc::probe_licence`) and reads only the status and the `error` code. Anything
  else, including transport errors, is left to the downloader.

### 2.3 Language (D10)

DLC follows the game's language, always: the build's `--lang` (new, #336; default
`en-US` as `gogo depot`) is recorded in `content.toml` and in `.gogo-depot.json`, and `dlc
add` uses the recorded one. On the depot lane there is no `--lang` on `dlc add`: a game
with base files in one language and DLC in another isn't a state gogo should make. On
the offline-installer lane, `dlc add --lang` only names the base installer's language
when gogo has none recorded (§5.1), and must match a recorded one. Changing the language is a
rebuild with `--lang` (gog-warp then diffs the language depots itself). `--lang` is
checked against the build's languages, and `de` is taken for `de-DE` when it is the only
match (design 14 §11, #377). On the offline
lane, the DLC installer for the game's language is chosen, else the language-neutral one,
else an error naming the languages there are.

## 3. Depot lane (D6, D7)

### 3.1 What gog-warp offers (pinned rev, read from source)

- `Manifest::dlcs()` is every non-base product in the build's manifest (`products` in v2,
  `game_ids` in v1). DLC depots live **in the base game's build**, tagged with
  `productId`; a DLC has no build of its own. So "the DLC's build" is always "the base
  build it came from", which makes D5 natural.
- The builder takes `dlcs(ids)` (v2) / `old_dlcs` (v1, as gogo already does per
  generation) and `upgrade_from(old_manifest, old_build_id)`. With an old manifest,
  `prepare()` diffs the old depot set (old language, `old_dlcs`) against the new one and
  downloads only what's new or changed, into `<install>/!Temp`, then moves each file into
  place. Paths are compared lowercased.
- No public per-product file list: the diff report and `FileList`'s entries are
  `pub(crate)`. A `dlc add` doesn't need one (§3.2 finds the new files on disk, as
  design 14 §4.1 already does). A build leaving a cached DLC out does, and reads the
  depot manifests itself (§3.3).
- No ownership check anywhere in the downloader.

### 3.2 Adding DLC to a built game

```
resolve game → content.toml (build B, language L, installed DLC I)
  → get_builds → find B; absent → refuse (§4)
  → get_manifest(B) → resolve names against manifest.dlcs() ∩ owned → wanted W (W ∩ I = ∅)
  → source root R:
       the depot cache, if its record says build B and language L      (keeps the cache whole)
         (an older record of build B brought up to date first, #374)
       else games/<gogid>/.dlc-staging/ (fresh)                        (no cache, or it moved on)
  → Downloader: manifest(B) upgrade_from(B) language(L) old_language(L)
                dlcs(O ∪ W) old_dlcs(O) verify(false) install_root(R)
                O = I for staging, I ∩ (what the cache holds) for the cache
  → snapshot R → prepare() → download() → new/changed files F (design 14 §4.1's comparison)
  → lay F onto game/ (§3.4) → GOG's install scripts for W (§3.6) → content.toml → done
```

- **Why `upgrade_from` the same build and not `verify`:** `verify` hashes the whole tree
  before fetching (design 14 §4.1: minutes for 70 GB). The old-manifest diff knows what the
  base files are from the manifest alone, so only DLC files are fetched, and the base tree
  isn't read. It also covers DLC files that **replace** base files: the same path with a
  new hash is in the diff's download set.
- **Why not install straight into `game/`:** gog-warp's diff trusts the old manifest and
  writes its moves as it goes. In the cache that's harmless (the cache is disposable and
  reverifiable); in `game/` an interruption would leave a half-moved tree with no record.
  A separate root plus §3.4 gives an interruption point gogo controls.
- **No cache:** the cache is disposable (design 14 D2), and someone who pruned a 70 GB
  cache shouldn't download it again for a DLC. The staging root gets only the diff (the
  DLC files), since the old manifest says the base is "there". It's inside the game dir
  so §3.4's moves are renames on one filesystem, and it's deleted after the lay.
- **A DLC with no depot bytes** (§1.3): there's nothing to download or lay, so no
  downloader runs and no journal is written. The add records it in `content.toml` (and
  in the cache's `dlcs`), and applies its `goggame-<dlcid>` files if the build's support
  overlay has them and its script (§3.6). A plan shows `0 B` for it.
- The staging dir is named so `move`, `apps remove` and the GC know it: `move` refuses
  while it exists (an add in progress, or a leftover), `apps remove` deletes it with the
  game.
- As built (#350): core `linuxapp::dlc` (`load_game`, `plan_add`, `add`, `recover`).
  The network side lives in the CLI (`dlc_cmd.rs`): the catalog, the owned set,
  `depot::installed_build` (the build list and the installed build's manifest) and the
  secure-link probe (`depot::first_unowned`). The download is a closure over
  `depot::fetch_into`, the downloader part of `depot::run` factored out, so the
  tests fake it. `depot::add_inputs` gives the `DownloadInputs`.
  - **The source is the cache** when its record has this build and language, has
    lost no file, and its tree is there. Otherwise it's `.dlc-staging/`. A record
    from before #347 (no `language` or `dlcs`) counts once it's brought up to date
    (below). The cache needn't hold every installed DLC (#374): one staged by an
    earlier add isn't in it, and the diff is told only what the cache holds
    (`old_dlcs = I ∩ held`), so it fetches into the cache everything the new DLC
    needs there, including a file it shares with the staged one. That file is laid
    too, with a backup, like any shared path. The commit adds only the DLC fetched
    or laid now to the record's `dlcs`, never the staged one.
  - **The DLC is kept in the depot cache whenever the cache can be the source**
    (#374). Before, a cache that lacked an installed DLC, or whose record predated
    #347, sent the add through staging, so the DLC lived only in `game/`, and
    removing and rebuilding the game downloaded it again (Cyberpunk 2077: about
    10 GB). Staging is now only for no cache at all (§3.2 "No cache"), a cache of
    another build or language, a cache that lost files, or an older record that
    can't be brought up to date. The plan says which:
    - `depot cache: the DLC is kept in it too`;
    - `depot cache: its record is brought up to date (en-US; DLC: none), and the
      DLC is kept in it too`;
    - `depot cache: none, so the DLC isn't kept in one (a rebuild with it downloads
      it again)`;
    - `depot cache: <why>, so the DLC isn't kept in it (…)`.

    The `--json` plan has `depotCache: {action: kept|none|notKept, upgraded?,
    reason?}`. Disk: from the cache the game files are a reflink where the
    filesystem allows, else a copy that the plan prices (`+N disk: no reflink from
    the cache`), since the DLC is then on disk twice. From staging they're a rename.
  - **Bringing an older record up to date (#374,** `gog/cache_upgrade.rs`**).** An
    add, and a `linux-app` build, whose cache record is the installed (or cached)
    build but has no `language` or `dlcs` read the build's depot manifests (every
    depot, every language: `dlc_filter::fetch_depots`, the same public URLs as
    §3.3) and decide, without hashing:
    - **Language** L (the game's, or the build's): the base's depots in L or `*`
      are all on disk, the build has L depots when it has language-specific ones at
      all, and no depot of another language shares a path with them through
      another depot manifest. The same path in two languages can't be told apart
      by existence, so that's undecidable.
    - **DLC held:** a product of the build with a `goggame-<id>.info` in the tree
      (or among its listed files), and every listed file of it on disk. A DLC with
      only some files there is left out with a note, and so is an info naming no
      product of the build.
    - **File lists** of every product in L go into `products`, as §3.3 keeps them.
    - Paths are matched case-insensitively per component, as for #381 below.

    When anything doesn't hold (the record lost files, the tree is gone, another
    build, the manifests can't be fetched, the checks above fail), the record is
    left as it is and the add stages, as before. The plan says why. The decision
    is pure (`cache_upgrade::decide`), so a dry run writes nothing. The add writes
    it under the game's lock before anything is fetched (`cache_upgrade::apply`).
    It first checks again that every file the new record claims is there and the
    record is still the old one, and refuses the add when the cache changed. The
    record is the only write: `files` stays as it was, and it's written durably
    (`lay::write_json_durably`, as every record write now is). A crash leaves the
    old record or the new one, never one claiming files that aren't there. A build
    does the same before its cache check, so a cache that holds what's wanted is
    `Complete` instead of hashed (`UnknownContent`), and prints a line either way.
  - **The download's record is a file of its own,** `games/<gogid>/dlc-download.json`
    (`{buildId, source, adding, before, fetched}`). It isn't a field in the journal: the
    launcher and doctor read the journal's existence as "files half laid", and a stop
    during the download must leave the game playable. It's written before the
    downloader runs, and a rerun reuses its source and its snapshot. Once `fetched` is
    set, a rerun lays without downloading again.
  - **Where fetched files land in `game/`:** under `<install dir>/` they keep their
    path, and under `gog-support/<base>/<product>/app/` they go to the game root.
    The overlay wins on a clash, as at a build.
  - **A record-only DLC** takes its `goggame-<dlcid>*` files from the cache's support
    overlay, and only when the cache is the source. With nothing to lay there's no
    journal at all.
  - **Setup runs once after the commit** (the base's and every DLC's
    `goggame-*.script`, §3.6). A failed setup is a warning naming `gogo linux-app
    setup`, since the DLC is in place and recorded by then.
  - `move` refuses while the journal, the download record or the staging dir is there.
    `apps remove` takes them with the game dir.
  - **A DLC the cache already holds is laid from it, not downloaded (#381).** Since
    #376 a build leaves the cache's other DLC out of `game/`, so "the cache holds X,
    the game doesn't" is an ordinary state (`gogo depot --all-dlcs`, then a build
    with `--no-dlcs`). The targeted diff doesn't look at the disk: it would fetch X
    again (13.3 GiB for Phantom Liberty). So when the cache is the source, the plan
    takes X from it as is when:
    - the record (same build and language) lists X in `dlcs`,
    - it keeps X's file list in `products` (§3.3) and the list isn't empty,
    - and every listed path is in the cache as a file or link.

    Each path is looked up as spelled, else by a case-insensitive match per
    component (gog-warp writes a path shared between depots once, as the first
    depot spells it). Existence is the whole check, not a size or a hash: the lists
    keep paths only, and gog-warp writes each file as `<name>.download` and renames
    it once it's whole, so a listed path under its own name is a finished file. A
    build trusts the cache tree the same way.
    - **The lay:** X's listed paths go through `lay_list` like a download's changed
      files (the tree's to their path, the support overlay's `app/` to the game
      root), plus its `goggame-<X>.*` from the support overlay. Then the same journal,
      `Journal::survey` case mapping, backups, recovery and launcher refusal as any
      lay from the cache (§3.4). Nothing is moved out of the cache. The commit writes
      `content.toml` and rewrites the record, which keeps `dlcs` and the file lists.
    - **No downloader runs** when every DLC added is held, so there's no
      `dlc-download.json` and no download status line. In a mixed add (X held, Y
      not) the diff gets `old_dlcs = I ∪ {X}`, `dlcs = I ∪ {X, Y}`, so it fetches
      only Y. The lay takes Y's changed files and X's listed ones.
    - **Otherwise it downloads, as before:** no file list (a record from before
      #376, or no build needed one yet), an empty one, or a listed path that isn't
      there. The targeted diff with `old_dlcs = I` fetches all of X into the cache,
      which completes a partial copy.
    - **Files X shares with the base or an installed DLC are laid**, the one in
      `game/` backed up as for any replaced file. That's what a build with X from
      this cache gives: §3.3's filter keeps a shared path as the cache has it. It's
      also what the download path does, where the diff fetches a shared path whose
      hash differs. The cache's copy is the one gog-warp wrote for this build and
      language (the base's, or X's when X replaces it). Skipping shared paths could
      leave the base's version under X's other files, the mix §3.4 refuses to
      launch. When both copies are the same, laying it costs a clone. Cyberpunk
      2077's three products share no path (#376), so this is the general rule, and a
      test covers it.
    - The plan says `files from: the depot cache (already there, no download)` and
      `no download, disk …`. A mixed add names the held DLC (`…; Phantom Liberty
      already there, the rest downloaded into it`). The plan's `--json` has
      `fromCache: [ids]`, and so does the `DlcPlan` event.
  - **Unverified, for the host pass:** gog-warp (pinned rev) keeps directory and link
    entries in its diff's download list, and in `upgrade_from` mode it renames each
    listed entry out of `!Temp`. A manifest with a directory entry over a non-empty dir
    could fail that rename. This applies equally to #347's targeted cache completion.

### 3.3 The depot cache: completeness and repair (#336)

`.gogo-depot.json` gains two fields, both optional so older records still read:

```json
{ "buildId": "…", "language": "en-US", "dlcs": ["1256837418"], "files": [ … ] }
```

`check_cache` (design 14 D2) gets the wanted language and DLC set, and answers:

| Record | Answer | What a build or `dlc add` does |
|---|---|---|
| no record | `Unrecorded` | as today (warning; repair on consent) |
| files missing | `Missing` | as today: full `verify` repair |
| `dlcs`/`language` absent (an older gogo) | `UnknownContent` | first brought up to date from the build's depot manifests when that's safe (§3.2, #374), then answered again. Otherwise a full `verify` with the wanted set: gog-warp keeps what's valid and fetches the rest. That's slow, but without the record it can't know what the tree holds unless it hashes it |
| wanted DLC not in `dlcs` | `MissingDlc(W)` | the targeted add of §3.2 on the cache (no hashing) |
| language differs, and not one the cache holds whole (#389) | `OtherLanguage` | rebuild-time only: a targeted diff from (old language, `dlcs`) to (new, wanted); the plan's download is an estimate from the depots the new language adds, by manifest hash (design 14 §11, #386) |
| `buildId` differs from the build being made | `OtherBuild` | as today: the new build is fetched into the cache via `verify`, with the wanted DLC set |
| `dlcs` ⊇ wanted, same language (or a language the cache holds whole, against that language's DLC) | `Complete` | reused. Extra DLC or languages in the cache are fine: the materialize copies only what the build wants (below) |

- A cache can hold more DLC than one build wants (the user ran `gogo depot --all-dlcs`,
  or an earlier build took more). The build then materializes the base and the wanted
  DLC's files only, with a path filter built from each product's file list. As built
  (#376):
  - **Which DLC the cache holds:** its record's `dlcs`, plus every DLC with a
    `goggame-<id>.info` in the tree (an older record lists none). None beyond the
    wanted set: the tree is taken whole, with no fetch (`dlc_filter::need`).
  - **The file lists** come from the build's depot manifests. gog-warp fetches them
    inside its downloader but keeps their entries `pub(crate)`, so
    `gog/dlc_filter.rs` reads the same public URLs itself: v2
    `content-system/v2/meta/<hash path>` (zlib JSON), v1
    `content-system/v1/manifests/<game>/windows/<timestamp>/<manifest>`, for the
    depots of each product in the record's language (or `*`). Each path is stored
    relative to the cache root, where gog-warp puts it: `<install dir>/<path>`, a
    support file under `gog-support/<base>/<product>/` (v2) or `gog-support/<base>/`
    (v1).
  - **They're kept in the record**, as `products: {<id>: [paths]}`, so a rebuild
    from the same cache needs no fetch. `gogo depot` stores them after a download
    that leaves DLC in the cache (best effort). A build that lacks some fetches only
    those, for the record's build (`depot::product_lists`), and stores them.
    `write_record` keeps the lists when the build and language stay the same, as a
    completion or a `dlc add` does. Records written before this have none, so the
    first filtered build from them fetches.
  - **The filter** (`dlc_filter::filter`) leaves out every path of a DLC the build
    doesn't take, unless a kept product (the base, a wanted DLC) lists it too. A
    shared path is kept as the cache has it: gog-warp writes one file per path,
    from the first depot that lists it. The left-out DLC's `goggame-<id>.*` are left
    out by name as well. Paths are compared lowercased: manifests are
    case-insensitive, and the tree keeps whatever casing each depot used.
  - **The materialize** (`materialize_tree_filtered`) skips what the filter leaves
    out. It doesn't create a dir the filter empties, so a left-out DLC leaves no
    skeleton (`tools/redmod/`), but it does create a dir that's empty in the cache.
    A support overlay of a left-out DLC (`gog-support/<base>/<dlc>/app`) isn't
    merged, and the others are filtered file by file (`merge_overlay_with`).
  - **The plan** says `the depot cache also holds <DLC>: left out of the game dir`,
    and the build warns with the count and size left out. `game files:` counts the
    kept bytes only.
  - **`content.toml` lists the DLC the build took**, from the choice, not from the
    infos present (`content::for_build`'s `installed`). A row takes its title and
    build from the DLC's info when there is one, and the catalog title otherwise. A
    tree given by path (`--from-depot`), where the flags don't apply, and the
    installer lane still record the infos present. So does a game read without a
    `content.toml` (§6.3).
  - **When the filter can't be built** (the lists can't be fetched: no network, a
    build GOG no longer lists, or no build named by the record or the infos), the
    build is **refused** before anything changes (exit 7). The message names the
    left-out DLC and the two ways on: retry when GOG can be reached, or build with
    those DLC too (`--dlc` for each). Installing them would contradict the flags,
    and recording only the wanted set would be untrue.
  - `mac-app` (on hold) chooses no DLC and still clones the cache tree whole; this
    doesn't change it.
- **A cache can hold several languages; builds filter by language (#389).** A language
  switch (`gogo depot --language`, `linux-app --lang`) is gog-warp's `verify` with no old
  manifest: it fetches what the new language lacks and deletes nothing. The cache is
  never pruned, so the old language's files stay and a switch back costs no download.
  As built:
  - **The record.** `language` is the last language fetched: a path two languages share
    holds its bytes. `otherLanguages: {<code>: {whole, dlcs, products}}` names every
    other language whose files the cache holds, with the DLC it held and its per-product
    file lists. Absent in records from before (#382, #385): they hold `language` only.
    `products` stays the lists of `language`.
  - **Whole.** Another language is whole when its files are on disk as it has them: no
    path it shares with a language fetched since comes from another depot manifest
    (`dlc_filter::clash`, the rule #374's upgrade uses). Only a whole language counts
    for `check_cache` and for the switch estimate (design 14 §11). Every download sets
    them all not whole in `write_record`, before anything else; `gogo depot` then
    fetches the depots of the record's products in each held language and
    `settle_languages` stores their lists and marks whole again those that were whole
    and don't clash. A download that wrote no file keeps them as they were. A `dlc add`
    doesn't check, so it leaves them not whole. When unsure the answer is
    `OtherLanguage`, and a switch back is a `verify` that hashes but fetches only what
    differs.
  - **Another build** keeps the old languages held (their files are still there) but
    not whole, with no DLC or lists: the next filtered build fetches the new build's.
  - **The build's language** is `--lang`, else the game's (`content.toml`), else the
    cache record's (a first build), else `en-US`. `linux-app` checks the cache against
    it whenever the record names a language. `content.toml` records it, not the
    record's last one.
  - **The filter** (`dlc_filter::filter_languages`, the same filter as for DLC) also
    leaves out every path the cache's other languages list for the record's products
    (base, wanted and extra DLC), unless a kept product lists it in the build's
    language. A path both list is kept as the cache has it; a path no list names is
    kept. The lists come from the record, else are fetched for the record's build and
    stored (`depot::product_lists`, per language).
  - **The plan** says `the depot cache also holds <languages>: N file(s), <size> left
    out of the game dir`, and the build's warning names the languages left out.
  - **When the lists can't be had** (offline, a build GOG no longer lists), the build is
    refused as for DLC: nothing changes, nothing untrue is recorded.
  - `mac-app` still clones the tree whole, every language included.
  - **Languages an older switch left (#391,** `cache_upgrade::scan_languages`**).** A
    cache switched by a gogo from before #389 kept the old language's files but names
    only its last language. Such a record can't be told from a new record of a cache
    never switched, so every record gets one scan, and `languagesScanned: true` marks
    it done. A new record keeps the last one's flag; a cache's first record has none,
    so it's scanned once too. The scan runs where #374's upgrade does, before a
    `linux-app` build, right after it when both apply, and both use one fetch of the
    build's depots (every depot, every language; `cache_upgrade::bring_up_to_date`).
    - **Held:** a language L the record doesn't name, when the base has depots in L
      and not in the record's language, and every file of each is on disk with the
      size the depot manifest gives (v2: the sum of its chunks, or its place in the
      small-files container; v1: `size`). A path such a depot shares with the record's
      language through another depot manifest holds that one's bytes, so only its
      presence counts. With no such base depot, the record's DLC's depots in L count
      instead; with none either, L's files can't be told from the record language's
      and L is skipped. At least one file L doesn't share has to be there with its size,
      for L and for each DLC held in it: when L shares every path with the record's
      language (an older title's one `lang/voice.dat`), the file there is that
      language's, so L is skipped with no note. Sizes only: no file is read or hashed.
    - **Recorded** as a switch records the old language: `whole` unless it clashes
      with the record's language (`dlc_filter::clash`), `dlcs` the DLC whose L files
      are all there, `products` every product's list in L. The record language's lists
      go into `products` from the same fetch.
    - **Partly there:** not recorded, with a note. No list names its files, so the
      filter keeps them, as before.
    - **Best effort:** a fetch failure, a build GOG no longer lists or a record that
      changed meanwhile leaves the record as it was and unscanned, with a warning; the
      build goes on and the next one tries again. The record is the only write: no
      file is deleted or pruned.
- As built (#347): `check_cache(root, &CacheWant { build_id, language, dlcs })`; a `None`
  field isn't checked, so `mac-app`/`linux-app` (which pass none until #349) see only
  `Complete`, `Unrecorded` and `Missing`. Order of answers: `Missing`, `OtherBuild`,
  `UnknownContent`, `OtherLanguage`, `MissingDlc`. An old record is `UnknownContent` only to
  a caller that wants a DLC or a language; one that asks for neither reads it as whole.
- `gogo depot` completes the cache the targeted way (`upgrade_from` the same build,
  `old_dlcs` = recorded, `dlcs` = recorded ∪ wanted, `old_language` = language, no
  `verify`) only for `MissingDlc` on generation 2; every other answer, and generation 1,
  is the full `verify` with the wanted set, as before. `OtherLanguage`'s targeted diff stays
  with the rebuild path. The plan line says which (`cache lacks DLC …: fetching only those`).
- The record is written after `download()` as today, now with the DLC set and language
  that run asked for. A `dlc add` into the cache rewrites it with `dlcs ∪ W` and the new
  file list.

### 3.4 Laying files onto `game/` (D6)

> No snapshot is created, deleted or rolled back while this journal is pending (design 20 §5.1).

The new step, in `gog/materialize.rs` (`lay_files(src_root, files, dst, journal)`):

1. The journal `games/<gogid>/dlc-journal.json` is started before the download (it holds
   the source root's snapshot, below) and, before `game/` is touched, gains `{files: [rel
   paths], replaced: [rel paths]}`; each write fsynced. Its other keys: `gogid`, `buildId`,
   `adding: [ids]`, `source`.
2. For each file in F: clone (FICLONE) or copy it to `<dst>/<rel>.gogo-new` (a staging
   root inside the game dir is a `rename` instead), and fsync it.
3. For each file that already exists in `game/` (a DLC file replacing a base one):
   `rename` the old one into `games/<gogid>/.dlc-backup/<rel>`.
4. `rename` every `.gogo-new` into place.
5. Write `content.toml` (the commit), delete the backup dir, the staging dir and the
   journal.

The game's lock (`game-moves/<gogid>.lock`, design 18 §6.1) is held exclusively from step
1 to 5, so a launcher started meanwhile refuses with its existing dialog.

**Interrupted** (Ctrl-C, crash, full disk, a lost network; issue requirement 10):

- **During the download** (by far the longest part): `game/` hasn't been touched. The
  game plays as it was. A rerun converges: gog-warp's diff fetches again into `!Temp` /
  the staging dir; already-moved files in the cache count as fetched (their path is new
  relative to the snapshot taken *before the first run*, so the snapshot is stored with
  the journal, written before the download starts; a rerun reuses it).
- **During the lay**: the journal exists and `content.toml` doesn't list the DLC. The
  next `dlc add`, `doctor` and the launcher see the journal:
  - `gogo linux-app dlc <gogid> add` (any args) **rolls forward** first: every journaled
    file is checked in place or its `.gogo-new` renamed; replaced files are already in
    the backup; then step 5. If the source root lost a file (a pruned cache), it **rolls
    back** instead: `.gogo-new` files and laid DLC files removed, backups renamed back,
    journal deleted, and says so. Either way the game ends in a recorded, consistent
    state.
  - The launcher (`LAUNCHER_VERSION` bump) refuses while `dlc-journal.json` exists, with a
    dialog naming `gogo linux-app dlc <gogid> add` to finish or roll back. Why refuse and
    not run: a half-laid DLC that replaced base files can mix two versions of the game's
    data, and that's the one way a DLC add could damage a save.
  - `doctor` fails a `dlc` check naming the same command.
- Only files named in the journal are ever deleted, and only with their backup in
  place. Nothing outside `game/`, the staging dir, the backup dir and the journal is
  written, so the prefix, saves, L4 and `launch.conf` are untouched.

With reflink the lay is near instant; with a copy it's the DLC's size once (not the
game's), and the plan's `game files:` line says which, as design 18 §6 does today.

As built (#350): `gog/lay.rs` (not `materialize.rs`; it uses `materialize::clone_file`).
- `Journal` is `{gogid, lane, buildId, language, adding: [{id, title}], source,
  sourceIsCache, moves, files: [{from, to}], replaced, dirs}`. `dirs` are the dirs the
  lay creates, which a rollback removes when they're empty.
- `Journal::survey` first maps each `to` onto the casing already in `game/`, one
  component at a time. Manifests are case-insensitive, and a DLC depot can spell a dir
  differently from the base tree. Each component matches what exists case-insensitively
  (an exact match first; a dir for every component but the last). A new component keeps
  the manifest's case, and later files reuse it. Two files mapping to one path keep the
  later one. Without this, a case-sensitive filesystem would get a second dir that Wine
  sees as the same one, and a replaced base file would look new and miss its backup.
  The journal stores the mapped paths, so the lay, `replaced`, `dirs` and `recover` all
  use them.
- `lay_files(root, journal, reflink, stop, on_file)` runs steps 1–4. `stop(Step)` is
  asked at each boundary and between files. Ctrl-C reaches it through the command's
  cancel token, and the crash tests use `AddOptions::fail_at`. `finish` is step 5's
  cleanup, after the caller wrote `content.toml`.
- `recover` goes forward when every file is laid, pending, or still in its source.
  The missing ones are staged again and swapped in, then the caller commits. It goes
  back otherwise.
- A symlinked dir on a laid path refuses the lay (#438). `symlinked_parent(game, rel)`
  walks each parent under `game/` with `symlink_metadata` and names the first link;
  `Journal::check_links` runs it over the files, the replaced and the removed paths.
  `lay_files` runs it before the journal is written, so a refusal leaves no journal, no
  `.gogo-new` and no backup, in `game/` or outside it. `dlc add` and `update` also run it
  right after the survey. `recover` and `roll_back` run it before touching anything, so
  a journal that points through a link stops with the same message and stays. The
  error is `LayError::Symlink`, shown as "`<link>` in the game dir is a symlink, and
  `<path>` would go through it ...; remove or replace the link, then run it again", and
  exits 7 like the other "the game's state refuses" errors. `update`'s removals use the
  same helper but keep their behaviour: skipped with a warning (#437). A link at the
  final path itself is not followed: the lay moves the link into the backup and renames
  the new file over its place (a rollback moves the link back), so the target is never
  written.
- `syncfs` runs on the game's filesystem after step 2 and after step 4, rather than an
  fsync per rename.
- The game's lock is `relocate::lock_game`, now public. It creates `game-moves/` when
  it's missing.
- A recovery emits `DlcRecovered` and needs the game's lock. `dlc add` recovers before
  it fetches anything, whatever DLC it was asked for.

### 3.5 Removal (D11): later

> **Built since (#353):** §3.7 is the removal as built, on the depot lane. The reasons
> below are why it wasn't in the first round.

Not in the first round (decided, §11.1). Reasons:

- It needs the reverse of §3.4: the DLC's files, and for files it replaced, the base
  version back. gog-warp can produce that (old `dlcs = I`, new `dlcs = I \ X`: its diff
  puts X's files in `deleted` and re-downloads base files X had replaced), but the
  `deleted` list is `pub(crate)`, so gogo would need a gog-warp change or its own
  manifest walk.
- Saves made with a DLC can refuse to load without it; the warning text needs a game to
  test on.
- The offline lane would run the DLC's Inno uninstaller, which is a different mechanism
  with its own failure modes. (Built since, #443: §5.2. GOG's DLC installers write their
  own `unins001.exe` beside the base's `unins000.exe`.)
- The workaround exists: a rebuild with `--dlc` naming the set without X (§1.2) gives a
  `game/` without X and keeps the prefix and saves.

Filed as a follow-up issue (§12). Nothing in this design rules it out: the record and the
journal are shaped for it (`removing: [ids]`).

### 3.6 GOG's install script per DLC

GOG ships a `goggame-<dlcid>.script` beside the DLC's `goggame-<dlcid>.info` for some
games. Today setup applies the base game's script only (`macapp::goggame::load(dir,
gogid)`). From now it applies the base's and every installed DLC's, in that order (the
DLC may set keys the base set), with the same subset (design 18 §4.9: `setRegistry`
applied, the rest skipped with a warning). `dlc add` runs that part of setup once after
the lay; the combined registry list changes, so setup's existing "apply when the list
changed" rule already covers rebuilds. No GOG Galaxy service is involved, and nothing
here needs one.

As built (#349): `goggame::load_all` returns the base's actions, then each DLC's by ascending
id (the script is looked for in the game dir, then one level down, beside its info); setup
applies that list.

### 3.7 As built (#353): DLC removal on the depot lane

```
gogo linux-app dlc <gogid> remove <id|title>... [--dry-run] [--yes] [--no-snapshots]
                                  [--limit RATE] [--no-disk-check] [--json] [--json-progress]
```

Core `linuxapp::dlc_remove` (`resolve_installed`, `reverse_diff`, `plan_remove`,
`remove`); the CLI side (the manifests, the question, the download) is `dlc_cmd.rs`.

**Decisions (2026-11-13):**

1. **Name and flags** as above. No `--all`: removal deletes, so names stay explicit. A
   bare `remove` on a terminal opens a picker over the installed DLC with none chosen;
   headless it is a usage error listing them. Names resolve against the **installed**
   set (`content.toml`), not the owned one: an id or a title without case. A name that
   is only part of one installed title counts only when the question will be asked
   (no `--yes`, on a terminal); with `--yes` or headless it is a usage error naming
   that title. A name that is part of several titles is a usage error, `ambiguous:
   matches A, B`. A name that matches nothing installed is a notice, so a rerun after
   a finished removal succeeds; only a title that matches nothing, with nothing else to
   do, exits 2 (likely a typo).
2. **The depot cache keeps the DLC.** Its files and its record (`dlcs`, `products`) are
   not touched. A build already leaves a cached DLC the game doesn't take out of `game/`
   (§3.3), and a later `dlc add` lays it from the cache with no download (#381). The
   plan says `depot cache: <DLC> stays in it, so a later dlc add takes it from there (no
   download)`. Pruning it is a possible follow-up.
3. **Saves.** gogo can't tell which saves need a DLC. The plan has one line, and the
   question repeats it: `saves made with <DLC> may not load without it; the snapshot
   "before removing <DLC>" can restore the game, or gogo linux-app dlc <gogid> add <id>
   brings it back`. The snapshot part is there only when autosnapshot is on for the run.
   The question defaults to no; `--yes` skips it. Saves and the prefix are never
   written. Registry values the DLC's install script set (§3.6) stay in the prefix:
   setup applies the base's and the remaining DLC's scripts again, and has no "unset".
4. **`linux-app update`** to a target that lacks an installed DLC takes it out in the
   same run (#442, design 21 §11h): one plan, one snapshot, one journal (the update's,
   with `removing`). It reuses this removal's reverse diff across the two builds
   (`reverse_diff_to`), its file check and its saves line. Unlike a removal, the depot
   cache doesn't keep the DLC: the cache moves to a build that doesn't have it.
5. **Offline-installer lane:** refused here, exit 7, nothing written, naming the
   rebuild with `--dlc` for the DLC to keep (or `--no-dlcs`). Built since (#443): §5.2
   runs the DLC's Inno uninstaller.
6. **Generation 1 builds** are removed from when nothing has to be downloaded (the
   depot manifests are enough). A base file that must be downloaded is refused, naming
   the rebuild: the targeted diff is generation 2 only, as for an add.
7. **No depot manifests** (offline, or neither GOG's listing nor the game's
   `build-manifest.json` has the installed build): refused, exit 7, nothing written,
   naming the rebuild. Without them there is no MD5 or size to check a file by before
   it comes out.
8. **A changed DLC file** (its MD5, else its size, isn't the manifest's) stays, with a
   warning: `kept <path>, which <DLC> laid: changed since install`, as an update does for
   dropped files. So does one under a symlinked dir. A base file being restored over a
   changed one is replaced; the changed one is in the backup until the commit, then
   only in the snapshot.

**The reverse diff** (pure, `reverse_diff`). The installed build's manifest comes from
the game's `build-manifest.json` when it names the installed build, else from GOG's
listing. Its depots for the base and every installed DLC in the game's language
(`dlc_filter::fetch_depots`, the public URLs of §3.3) give paths, sizes and MD5s in
manifest order. gog-warp writes a path several depots list once, from the first depot
that lists it (its diff downloads it with the first file list that has it). Each entry
is keyed by the path it lands on in `game/`, through the lay's own mapping (the tree's
paths keep theirs, a support overlay's `app/` goes to the game root; GOG's support
installers never reach `game/` and are left out), so a DLC's overlay file and a base
tree file on one game path are one path. On a game path a support overlay's entry wins
over a tree's, as at a build and a lay; otherwise the first depot's entry counts. This
is worked out twice: over the base and every installed DLC ("now"), and over the base
and the DLC that stay ("after"). Paths compare without case.

- A path only the removed DLC list comes out. What's checked is the "now" entry, the
  file the build laid.
- A path the removed DLC share with a kept product is **shared**. It is restored to the
  "after" entry unless `game/` already holds that version (its MD5, else its size). The
  "after" version comes from the depot cache when the cache's record is the installed
  build and language and its copy has the "after" MD5, else from a download.
- The removed DLC's `goggame-<id>.*` in the game root go by name too: a record-only DLC
  (§3.2) laid them from the support overlay, and no depot lists them. Only the
  symlink check applies to them.
- Cyberpunk 2077's products share no path (#376), so a removal there is a pure removal:
  no download, nothing restored.

**The download** (only for a shared file the cache can't give): gog-warp's targeted diff
of the installed build against itself, `upgrade_from(B)`, `old_dlcs` = installed,
`dlcs` = the ones that stay, `language` = `old_language` = the game's, into
`games/<gogid>/.dlc-staging/` (`depot::fetch_into`, as an add's staging). Its diff
fetches the paths whose entry changes. Its delete pass looks only in the staging root,
where the DLC's files aren't, so it deletes nothing. Every file the plan expects from
it is checked by MD5 (else size) before anything is laid; a missing or different one
stops the removal with the game as it was, naming the rebuild. Its record is the add's
file (`dlc-download.json`, with `removing` instead of `adding`), so every refusal for
a pending DLC download (a move, a snapshot, an update) applies. A removal that stops
during it drops the record and the staging dir itself; one a crash left is dropped by
the next removal. An add's pending download refuses a removal, naming `dlc add`; an
update's, naming `update`.

**Order:** plan (no write to the game or the cache; hashes only the shared paths in
`game/` and the cache) → question → settle a pending rollback → the game's lock →
refuse a running game → settle any journal (an add's, an update's or a removal's) →
read the game again, and stop when its DLC, build, language or dir differ from the
plan's (`changed since the plan: run the removal again`), or, when a file is restored
from the cache, when the cache record's build, language or DLC set differ from the
plan's → refuse a restored path that isn't plain (`..`, an absolute path) in the cache
or in `game/` → the `auto` snapshot (`Trigger::DlcRemove`, `before
removing <titles>`; a failure stops it, exit 7) → the download, when needed → the
journal → each file to take out checked (MD5, else size; the progress step
`checking the DLC's files`, since a big DLC is read whole) → each restored file's
source (the cache's or the staging dir's) hashed again, and a mismatch stops the
removal with nothing laid → the lay → the commit
(`content.toml` without the DLC's rows) → backups, staging and the journal go → dirs
the removed files left empty go, unless a kept product lists a file under them → setup
once (the scripts of the base and the remaining DLC). A failed setup is a warning.

**The journal** is §3.4's, with `kind = "remove"` and `removing: [{id, title}]`: `files`
are the restored ones (from the cache or the staging dir), `replaced` their current
copies, `removed` the DLC's files. `lay_files`, the backups, `check_links` and
`recover` are unchanged. The journal also keeps `md5s` (each restored file's MD5, by
its path in `game/`) and `prune` (the dirs to take out once empty). Recovery (any `dlc
add`, `dlc remove` or `update` settles it): forward commits `content.toml` without the
DLC, then does what a removal that didn't stop does, the empty dirs and setup (a
failed setup is a warning naming `gogo linux-app setup`); back puts the DLC's files
and the replaced ones back from the backup, and the DLC stays installed. It goes back,
not forward, when a restored file comes from the cache and the cache's record is on
another build or language now, or when a restored file's source that is still there
no longer has its recorded MD5. The launcher refuses while the journal is there (its
message names `dlc add`, which settles any kind); `doctor` fails naming `dlc remove`,
and warns on a removal's leftover download. Every other refusal for a pending DLC
change (a move, a snapshot, `linux-app run`, an update) names `dlc remove` when the
journal or the download record is a removal's, and `dlc add` otherwise.

**`--dry-run`** writes nothing to the game, its records or the depot cache, and settles
no journal. Like every command that reaches GOG, it may still refresh the sign-in token
and the cached builds listing.

**Output:** `--dry-run --json` prints `{dryRun, plan}` (`linuxapp::dlc_remove::RemovePlan`:
`removing`, `keeping`, `removed` and `restored` with path, size, MD5 and, for a restored
file, `from: cache|download`, `cacheKeeps`, `gameFiles`, `autosnapshot`). `--json`
prints `{gogid, buildId, language, dlcs, removed, recovered?, filesRemoved,
filesRestored, lay?, snapshot, pruned, warnings?}`. `GameFilesStarted` has `kind:
"dlc-remove"`.

**Tests:** `dlc_remove`'s unit tests (the reverse diff: a DLC-only file, a replaced base
file, a path a kept DLC shares, products not installed; name resolution; the MD5/size
check; casing; a DLC overlay file on a base tree path; ambiguous and partial names; a
removal's pending download) and `linuxapp_pipeline`'s `dlc_add::remove` on fake
manifests: a DLC that
replaced a base file, removed with the base file downloaded, and from a cache whose
copy is right with no download; a changed DLC file kept; a download that doesn't give
the base version; a generation 1 refusal; a plan the game moved on from; a stop at each
lay step, finished (emptied dirs included) and undone; a cache copy or record that
changed after the plan; a recovery going back when its cache source changed; a restored
path with `..`; the `before removing` snapshot and `--no-snapshots`; a zero-byte DLC.

**Not run:** a real removal on a game with DLC (Cyberpunk 2077's Phantom Liberty or
REDmod) is a host pass: the dry run's file count against the DLC's depot, the game
starting without it, a save made without the DLC loading, and `dlc add` taking it back
from the cache with no download.

## 4. Build matching (D5)

A DLC's depots come from the same build manifest as the base game's, so gogo always
fetches DLC **for the installed build** `B`, recorded in `content.toml` (for older games:
`.gogo-depot.json`'s `buildId` if the cache's tree is the game's, else `goggame-<gogid>.info`'s
`buildId`, else unknown).

| Case | What `dlc add` does |
|---|---|
| `B` is GOG's newest public build | adds |
| GOG has a newer build, and still lists `B` among the recent ones | adds for `B`; the plan notes the newer build and how to update (a rebuild, which keeps the DLC set) |
| `get_builds` no longer lists `B` (a common case, below) | refuses (exit 7): `build B is no longer served by GOG; update the game first: gogo linux-app <game> --dlc <names> (keeps the prefix, saves and L4)` |
| `B` unknown | refuses the same way, since a DLC for a guessed build is worse than a rebuild |
| `B` is listed but the DLC isn't in its manifest | refuses (exit 2) with `not in this build`, naming the update path as above |

**GOG lists only a limited set of recent builds** (§11.2 R2): for Cyberpunk 2077, 6
builds (2.5_RedModFix back to a 1.63_Hotfix legacy branch) of a `total_count` of 270, and
paging returns the same 6. A manifest's URL comes from that list, so a build that has
rotated off it can't be fetched at all. A game built some versions ago therefore can't
take DLC without an update first: the refusal is an ordinary outcome, and its text must
name the update path. Builds still listed do serve their manifests, including the old
legacy-branch one.

- **Why not update the base silently:** an update is a rebuild of a 70 GB tree with a
  different game version, and saves, mods and the protonfix can depend on the version.
  It must be the user's command. **Why not refuse whenever a newer build exists:** the
  user may hold a version back on purpose (a mod, a known regression), and while the
  build is still among the listed ones, adding DLC to it works.
- An update command that patches the base in place (gog-warp's `upgrade_from` with
  xdelta patches) is the natural next step and is the same machinery; it's not designed
  here.
- On the offline lane the match is by version: the DLC installer's `version` (catalog)
  must equal the base's (the `InstallRecord`'s, else `goggame-<gogid>.info`'s
  `version`/`buildId`). GOG serves only the newest installers, so a mismatch means the
  base is out of date: refused, naming `gogo download <game>` and a rebuild.

## 5. Offline-installer lane (D8)

GOG's DLC installers are Inno Setup `.exe`s of their own (design 04 puts them under
`{download.dir}/{title}/{dlc title}/`; the state records key them by `dlc_id`).

- **Getting them:** `gogo download <game> --dlc X` (or `--all-dlcs`) as today's
  `--with-dlcs`, filtered by the owned set and the names. `dlc add` on an installer-lane
  game downloads what's missing itself, with the same consent as the depot lane, through
  the download engine (verified, resumable).
- **First build:** the base installer runs as today (staging prefix, `/DIR=C:\GOG\<slug>`).
  Then each chosen DLC's installer, in catalog order, in the **same staging prefix**, with
  the same Inno flags and `/DIR=C:\GOG\<slug>`. This matches Lutris's precedent for GOG
  add-ons (the base game's prefix, `/DIR=` the base dir, silent flags; §11.2 R3).
  **Assumption:** a DLC installer may also check that the base game is installed (reports
  say one run in another prefix says the base isn't found; the check itself, a
  `HKLM\Software\GOG.com\Games\<gogid>` key, a `goggame-<id>.info` or a file, isn't
  documented). On this lane the base's own installer ran in the same prefix, so whatever
  it checks should be there. The depot lane never runs DLC installers. A DLC is installed
  when the installer exits 0 and `goggame-<dlcid>.info` is in the game dir. A failed DLC
  fails the build before the prefix moves into place, so no half-built game appears.
- **Adding later:** the same installer run in the game's real prefix, under the game's
  lock, refused while the game runs. Inno installers have no rollback gogo can drive, so
  interruption is handled by rerunning: Inno overwrites what it wrote. A marker
  (`dlc-journal.json` with `lane = "installer"`) makes the launcher refuse and `doctor`
  fail until the rerun succeeds, as in §3.4.
- **Why not unpack the installers with innoextract** (no Proton run): that's a separate
  decision for the whole installer lane, and DLC shouldn't be the only place it differs.

Verified: nothing about DLC installers' file names or whether they install into the base
dir had been captured live (design 17; docs/03) before #351; §5.1 has what its e2e
found.

### 5.1 As built (#351)

Core `linuxapp::dlc_installer` (`run_one`, `version_check`, `add`); the network side
(which game a setup is for, the choice, the version check's catalog half, the download)
is the CLI's `installer_dlc.rs`; `dlc add` routes there from `dlc_cmd.rs`.

**Decisions (2026-11-13):**

1. **First build.** After the base installer succeeds and its info names the game,
   each chosen DLC's installer runs, one at a time in catalog order, in the staging
   prefix with the base's Inno flags, `/DIR=C:\GOG\<slug>` and a log of its own
   (`install/dlc-<id>-inno.log`). Then the entry-point scan, the move into place and
   setup, as before. A DLC whose installer exits non-zero, or leaves no
   `goggame-<id>.info` in the base dir, fails the build before the prefix moves (exit
   7, the Inno log's tail in the message). There is no fallback to a build without the
   failed DLC: the message names `--no-dlcs`, or dropping that `--dlc`.
2. **Where the installers come from.** The build downloads the missing ones through the
   download engine (verified, resumable), into `download.dir` as `gogo download --dlc`
   does, after the build's consent and before anything runs. `linux-app` still doesn't
   download the base installer.
3. **Selection and language.** As the depot lane (§1.2): `--yes` and headless builds
   take none, a terminal gets the picker with every owned DLC preselected, a rebuild
   keeps the recorded set and refuses a kept DLC that has no Windows installer now. A
   DLC can be taken when it is owned and GOG has a Windows installer for it. The DLC
   installers are in the base installer's language (its catalog entry's, from gogo's
   download record), else `--lang`, else `[gog] language`; once DLC are chosen, a
   `--lang` that differs from a known base language is a usage error. The language is
   recorded in `content.toml` only when it is known: from the download record, or
   because chosen DLC installers in it ran. With a setup given by path, gogo finds
   the game from its download record of the file, else from the file name
   (`setup_<slug>_…`, the longest owned slug) against the owned games' slugs. That is
   a guess (`setup_fallout_2_…` names an owned `fallout` when `fallout_2` isn't
   owned): the game the base installer installs must be the guessed one before a DLC
   installer runs, and with no DLC chosen, a game that turns out to be another gets
   no version or language from the guess. Without `--dlc`/`--all-dlcs`, every lookup
   is best effort: an unknown game, no network or an API error is a plan note and the
   build goes on without DLC (no picker either), so a base build needs no network.
   The download takes the record of the file it just planned (same entry, file and
   version label), never an older download of the same DLC.
4. **`dlc add`.** The plan (each DLC with its installer's size, the installers'
   version and language, `C:\GOG\<name>`), then `--yes` or the prompt; headless
   without `--yes` fails before any download. The download comes before the game's
   lock: nothing in the game dir changes while it runs. Then, under the lock and
   refused while the game runs: the `auto` snapshot `before adding <title>` (design 20
   §7.1), `dlc-journal.json` with `lane = "installer"` and the DLC it adds, the
   installers in the game's own prefix with its Proton build and umu id, the commit
   (`content.toml`) and the journal deleted, then setup. While the journal is there the
   launcher refuses, doctor fails `dlc`, and no snapshot operation runs (design 20
   §5.1), as for the depot lane. A stop (Ctrl-C, a crash, a failed installer) leaves
   the journal; the next `dlc add`, bare or naming more DLC, runs every DLC the
   journal names again (Inno overwrites what it wrote), then commits. The game's
   Proton build and umu are got and checked before the journal is written, so a
   runtime that can't be had leaves nothing behind. An add that fails every time
   (an installer that always exits non-zero, or ignores `/DIR=`) has two other ways
   out: `dlc add --abandon` drops the journal under the lock, records nothing for its
   DLC and leaves what the installers wrote in the prefix; and a rollback to the
   snapshot taken before the add is the undo, so this journal doesn't refuse a
   rollback (the restored dir has none). The add's errors, doctor, the snapshot
   refusals and the launcher name all three. The depot lane's recovery refuses such
   a journal rather than settling it. The add needs the base installer's language:
   with none recorded it asks on a terminal, or needs `--lang`, and never assumes
   one. Setup's install-script pass (§3.6) needs no change: it sets again what Inno
   already set.
5. **Version match.** GOG serves a game's DLC installers beside its current base
   installer only. The base's installer version label is recorded at the build
   (`content.toml` `version`): from gogo's download record of the setup, else the
   current label when the setup's file name contains it. A build that takes DLC and
   an add compare it with GOG's current base installer label, the same field of the
   same product, and refuse a difference (exit 7, nothing written), naming
   `gogo download <gogid>` and a rebuild with `--dlc`. An unknown version (a game
   built from a setup given by path that gogo didn't download, whose file name doesn't
   carry the current label) is not checked at all: the plan notes it, and the consent
   covers it. Labels of different products aren't compared, and
   a file name's own version isn't GOG's label: Journey to the Savage Planet's
   installer is labelled `1.0.10b` and named `…_1.0.10_(44657).exe`, and its DLC's
   installer is labelled `1.0.10`. Each DLC row records its installer's label and its
   info's `buildId`, unchecked.
6. **`/DIR=` (the open question above).** A DLC is installed when its installer
   exits 0 and its `goggame-<id>.info` is in the base game's dir. If it isn't,
   `drive_c` (but `windows/`, and the copy GOG's support installer keeps of every info
   under `ProgramData/GOG.com/supportInstaller/<id>/`) is searched for it, and the
   error names where it went,
   or that it went nowhere. There's no move into place: the Inno log's
   `Dest filename` lines would allow one, if a game ever needs it. Doctor warns, for a
   recorded DLC whose info is missing from the base dir, where in the prefix it is.
7. **`content.toml`.** No new fields: `lane = "installer"`, the base's `build_id` (its
   info's), `version` (decision 5) and `language`, and per DLC `id`, `title`,
   `version` (its installer's label), `build_id` (its info's, when there is one) and
   `added`.
8. **`dlc remove`** was refused on this lane here; §5.2 is the removal as built (#443),
   by the DLC's own Inno uninstaller.

**How a setup runs.** Every setup on this lane (the base's, a DLC's, GOG's support
installer) runs as `C:\gogo-setup\<file>`: its dir is linked into the prefix as
`drive_c/gogo-setup` for the run, and named as umu's `STEAM_COMPAT_INSTALL_PATH` so
the runtime's container shares it. A unix path would go through Proton's `umu.exe`,
which cuts the Windows path it makes of it at 259 characters: a setup deep enough in
a download dir is "not found" and exits 1 before Inno starts (seen with the e2e's
scratch paths).

`dlc list` on this lane: "in this build" means GOG has a Windows installer for the DLC,
and both sizes are that installer's (what it unpacks isn't known before it runs).

**e2e.** `e2e/dlc-1519147341.toml` (a first build with `--dlc`) and
`e2e/dlc-add-1519147341.toml` (a base build, an add stopped once its installer runs,
and the bare add that finishes it), through a manifest's `[installer_dlc]`
(`docs/linux-e2e.md`). Both need `--installer` with the game's setup.

Run on 2026-11-13 with Journey to the Savage Planet (`setup_…_1.0.10_(44657).exe`) and
its Hot Garbage DLC, GE-Proton11-12 and umu 1.4.4, headless sway on a GPU: both
pass. The DLC's installer took `/DIR=C:\GOG\<slug>` (its Inno log's `Dest filename`
lines and its info are in the base dir, and no other info is in the prefix but the
support installer's copy), the stopped add left its journal (exit 130) and the bare add
ran the installer again and committed, and the game started with the DLC in place.
`content.toml` `version` stayed unset: the setup came from the harness's mount, not a
gogo download, and its file name's `1.0.10` isn't GOG's current label `1.0.10b`, so
the plan noted that the version isn't known (decision 5). The DLC's row has `version =
"1.0.10"` and no `build_id` (its info has none).

### 5.2 As built (#443): DLC removal on the offline-installer lane

```
gogo linux-app dlc <gogid> remove <id|title>... [--dry-run] [--yes] [--no-snapshots]
                                  [--json] [--json-progress]
gogo linux-app dlc <gogid> remove --abandon [--json]
```

Core `linuxapp::dlc_uninstall` (`find_uninstaller`, `log_files`, `plan`, `remove`,
`abandon`); the CLI side is `dlc_cmd.rs`'s `installer_remove_cmd`. No network: the
record (`content.toml`), the prefix and the install logs are all it reads.

**What GOG's DLC installers leave** (seen on Journey to the Savage Planet and its Hot
Garbage DLC, Inno Setup 5.6.1): each DLC writes an uninstaller of its own beside the
base game's, in the base game's dir: `unins000.{exe,dat,ini,msg}` is the base's,
`unins001.{exe,dat,ini,msg}` the DLC's. Its Inno log says `Creating new uninstall log:
C:\GOG\<name>\unins001.dat`; its `unins001.ini` is `[InstallSettings]
productID=<the DLC's id>`; the registry has `…\Uninstall\<id>_is1` with its
`UninstallString`. Its uninstall log lists its `goggame-<id>.{info,hashdb,ico}`, its
`HKLM\SOFTWARE\GOG.com\Games\<id>` key and its Start Menu group. A file the base
already had and the DLC skipped (`onlyifdoesntexist`, Journey's `webcache.zip`) isn't
in it.

**Decisions (2026-11-13):**

1. **Finding the uninstaller.** The one `unins<n>.ini` in the base game's dir whose
   `productID` is the DLC's id, with its `.exe` and `.dat` beside it; several naming
   it are refused (gogo won't pick one). Else the uninstall log the DLC's Inno log
   names (`install/dlc-<id>-inno.log`), only when it is confirmed: its slot's ini names
   the DLC, or, with no readable ini there, the base game's ini names the base at
   another slot. Slot 0 (`unins000`) is the base game's and is never taken as a DLC's,
   from an ini or a log: running it would uninstall the whole game. The registry isn't
   read. Neither, with the DLC's `goggame-<id>.info` still there: refused, exit
   7, nothing written, naming the rebuild with `--dlc` for the DLC to keep (or
   `--no-dlcs`), which keeps the prefix and saves. Neither, with the info gone too:
   only its record goes (no runtime needed).
2. **A DLC with no uninstaller of its own** (its Inno log says `Will append to existing
   uninstall log`, or the log it names is another product's by its ini): refused, exit
   7, with the rebuild. Its uninstaller is the base game's, which would take the whole
   game out.
3. **A DLC that replaced game files** (its Inno log has a `Dest file exists.` entry in
   the game's dir that it then installed, other than its own `goggame-<id>.*` and
   uninstaller): refused, exit 7, listing them, with the rebuild. Inno deletes what it
   installed and keeps no older copy, so the game would lose those files. The log is
   only trusted when it is the recorded install's and about the game's dir: an
   installer run deletes the DLC's old log before it starts, a log older than the
   DLC's info (by more than two minutes) is an earlier install's, and a log with no
   file entry under the game's dir (compared by path component without case, Unicode)
   says nothing. With no log it can trust, the plan says this can't be checked, the
   log isn't used to find the uninstaller either, and the consent covers it.
4. **Order.** Plan (names against the installed set, each DLC's uninstaller, the
   saves line) → the question (default no; `--yes`, and headless needs it) → settle
   a pending rollback → the game's lock → refuse a running game → read the game again
   → a depot-lane journal is refused; an unfinished installer add is refused, naming
   the add's ways out; an unfinished removal is resumed → the plan again, under the
   lock → the game's Proton build and umu, got and checked (none needed when only
   records go) → the `auto` snapshot `before removing <titles>` (not on a resumed
   run: its first run took it) → `dlc-journal.json` (`lane = "installer"`, `kind =
   "remove"`, `removing`) → the uninstallers, newest first (the highest `unins<n>`:
   a later DLC may sit on an earlier one, and GOG's base uninstaller goes bottom to
   top too) → each checked → the commit (`content.toml` without the DLC's rows) →
   the journal goes → setup (its install-script pass, §3.6, applies the base's and the
   remaining DLC's; a failure is a warning).
   Each runs as `C:\GOG\<name>\unins<n>.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
   /LOG=<game>/install/dlc-<id>-uninstall.log` through umu, as the game's umu id, with
   the game's dir as umu's install path. It is in the prefix already, a short path, so
   it needs no `C:\gogo-setup` link.
5. **Success.** The uninstaller exits 0, and both the DLC's `goggame-<id>.info` and
   its `unins<n>.dat` are gone from the base game's dir. Exit 0 alone isn't trusted:
   Inno's uninstaller copies itself to a temp file and runs from there. Otherwise the
   removal fails (exit 7, the log's tail), and the journal stays.
6. **Recovery and the ways out**, as for an add (§5.1): a `dlc remove`, bare or naming
   more, runs every DLC the journal names again (one whose info and uninstaller are both
   gone counts as done); `dlc remove --abandon` drops the journal under the lock, and
   the record follows the prefix (a DLC whose info is gone is no longer recorded, one
   whose info is there stays, though its uninstaller may have taken some of its files,
   so the message names the rollback and the rebuild that put it back whole; a journal
   naming no DLC is a finished removal); and a rollback to `before removing` undoes
   it, so this
   journal doesn't refuse a rollback either. An add refuses a removal's journal (before
   any network or question) and
   `dlc add --abandon` a removal (naming `dlc remove --abandon`), and the other way
   round. The launcher's dialog, doctor's `dlc` finding, the snapshot refusals and the
   depot lane's recovery name the removal's ways out for a removal's journal
   (`LAUNCHER_VERSION` 17).
7. **Saves.** The depot lane's line (§3.7 decision 3), in the plan and in the question:
   `saves made with <DLC> may not load without it; the snapshot "before removing <DLC>"
   can restore the game, or gogo linux-app dlc <gogid> add <id> brings it back`.
8. **What the uninstaller leaves.** Each game-dir file the DLC's Inno log says it
   installed that is still there afterwards (found without case) is reported (`left` in `--json`, a warning
   otherwise); gogo deletes none of them: it didn't lay them, and a path may be the
   base game's. Registry values are the uninstaller's business.
9. **Version and language:** none apply. Nothing is downloaded, and no installer is
   matched to the base.
10. **Flags.** `--limit` and `--no-disk-check` are refused on this lane (usage, exit 2):
    nothing is downloaded. `--dry-run --json` prints `{dryRun, plan, lines}`
    (`dlc_uninstall::RemovePlan`: `lane`, `into`, `removing` with each DLC's
    `uninstaller` (`number`, `exe`, `dat`, `win`) or none, `resumed`, `logMissing`, and
    `autosnapshot`). `--json` prints `{gogid, lane, dlcs, removed, resumed, left?,
    snapshot, pruned, warnings?}`; `--abandon --json` prints `{gogid, dropped, kept}`.

**Tests:** `dlc_uninstall`'s unit tests (the ini, slot 0 from an ini or a log, two
inis naming the DLC, the log's `Creating new` and `Will append` with and without the
base's ini confirming it, a missing `.dat`, the log's installed and replaced files, a
log about another dir, Unicode case, a log older than the info, the flags) and
`linuxapp_pipeline`'s, on a fake umu whose DLC installers write `unins<n>` and an Inno
log and whose uninstallers take them out: two DLC removed newest first in the game's
prefix; a stop after one, the doctor/snapshot/add/abandon refusals, and the bare rerun;
an uninstaller that exits 4, one that leaves its info, `--abandon` keeping a DLC whose
info is there and dropping one whose info is gone, one that leaves a file; a DLC that
appended to the base's uninstaller, one that replaced `game.exe`, one with no
uninstaller, and one whose files are gone already (no runtime); an install log older
than the info (the plan's note), a journal naming no DLC, and a left file in another
case. The launcher's dialog for a removal's journal.

**e2e.** Through a manifest's `[[installer_dlc.remove]]` (`docs/linux-e2e.md`), with
Journey to the Savage Planet built with Hot Garbage: `e2e/dlc-remove-clean-1519147341.toml`
removes it, and `e2e/dlc-remove-1519147341.toml` stops a removal as soon as the
uninstaller's Wine process starts (what it left running in the prefix killed), then
runs the bare `dlc remove` that finishes it. Both then start the game.

Run on 2026-11-13 with `setup_…_1.0.10_(44657).exe` and its Hot Garbage DLC,
GE-Proton11-12 and umu 1.4.4, headless sway on a GPU: both pass, and the game starts
without the DLC. What the real uninstaller did:

- **Exit code:** 0 through umu, about 3 s for the whole `dlc remove`. umu warns
  `Executable not found: C:\GOG\…\unins001.exe` (it looks for a unix file), as it does
  for every `C:\gogo-setup` setup; Wine runs it.
- **The self-copy:** the first phase starts its second as
  `C:\users\steamuser\AppData\Local\Temp\_iu14D2N.tmp /SECONDPHASE="…\unins001.exe"
  /FIRSTPHASEWND=…` with the same flags, and umu returns once both are done. A Ctrl-C
  to gogo's process group ends umu but not that copy, which goes on in the prefix: the
  next `dlc remove` is refused (`process(es) still running in <gogid>'s prefix`) until
  it is done, which for Hot Garbage is well under a second.
- **`/LOG=`** works: the second phase writes it (`install/dlc-<id>-uninstall.log`).
- **What it deleted:** `goggame-1240193021.{ico,hashdb,info}`, `unins001.{dat,msg}`,
  then `unins001.{exe,ini}`, and the DLC's registry keys (`GOG.com\Games\1240193021`,
  `Uninstall\1240193021_is1`). It tries to delete the game's dir as well, which fails
  as it isn't empty (`Failed to delete directory (145)`), and logs a harmless
  `processSupportScript [uninstall]` error (the DLC has no support script). The base
  game's files, `unins000.*` and `webcache.zip` stay.
- **A stop** lands wherever the uninstaller is. In one run it came before the
  uninstaller deleted anything, and the rerun ran it again from the start. In another
  it cut the uninstaller at `Deleting Uninstall data files.`: the info, `.dat` and
  `.msg` were gone, `unins001.exe` and `.ini` were left. That rerun found no
  uninstaller to run (no `.dat`) and the info gone, so it dropped the record only and
  warned that `unins001.exe` was left (decision 8: gogo deletes none of it).

## 6. Record and state (D9)

### 6.1 `games/<gogid>/content.toml`

```toml
schema   = 1
lane     = "depot"            # or "installer"
build_id = "…"                # depot: the build; installer: the base installer's build
version  = "2.31a"
language = "en-US"

[[dlc]]
id       = 1256837418
title    = "Cyberpunk 2077: Phantom Liberty"
build_id = "…"                # always the base's build on the depot lane; kept per DLC
version  = "2.31a"            #   for the installer lane, where each installer has its own
added    = "2026-11-11"
```

- **Why a new file and not `gogo-launch.toml`:** `gogo-launch.toml` is what to run, with
  `deny_unknown_fields` and a schema an older gogo refuses when newer; it feeds the
  stamp. A DLC add changes no launch setting, so it shouldn't change the stamp or make an
  older gogo refuse the game. `state.toml` is what setup applied, not what's installed.
- Written atomically (06 §2) by a build (depot and installer lanes) and by `dlc add` as
  its commit. A game built before this has none: everything reads it as "base only,
  build from `.gogo-depot.json`/`goggame-*.info`, DLC from the infos".
- `apps remove` and `move` treat it as one more file in the game dir; nothing else
  changes there. On the Mac, the game dir is the same `games/<gogid>/` (design 16), so the
  file is lane- and OS-neutral.

### 6.2 What shows it

| Where | What |
|---|---|
| build plan | `dlc: Phantom Liberty (29.2 GiB), REDmod` or `dlc: none (2 owned not included: --all-dlcs)`; on a rebuild, what's kept, added and dropped |
| `DepotEvent::Plan` / `--json-progress` | `dlcs` becomes `[{id, title, downloadSize, diskSize}]` (was titles only; the NDJSON key stays) |
| `apps list` | a `dlc` column: `2/3` (installed/owned), `-` with none; `--json` rows gain `dlcs: [{id, title}]` |
| `apps show` | the `content.toml` section |
| `doctor` | `dlc`: ok `Phantom Liberty, REDmod (build …)`; **warn** an installed DLC's `goggame-<id>.info` missing from `game/`, or an info for a DLC `content.toml` doesn't list (added by hand); **warn** owned DLC not installed (with `dlc add`); **fail** a `dlc-journal.json` (§3.4) |
| `--json` of the build and `dlc add` | `{gogid, buildId, language, dlcs: [...], added: [...]}` |

### 6.3 As built (#348)

Where the code differs from the table above, or does less for now:

- `gog/content.rs` holds the record (`Content`, `DlcRow`, `load`, `write`, `resolve`).
  A build of either lane writes it as its last step, after setup; a failed setup leaves
  none, and the game reads as derived.
- **Derived** (no file): the lane is `depot` when `game/` exists, else `installer`. The
  base build comes from the base `goggame-<id>.info`'s `buildId`, and the language from
  the depot cache record (`.gogo-depot.json` in the default cache dir for the game),
  used only when its build is the one the infos name. Every other `goggame-*.info` is a
  DLC row. Reading never writes the file.
- `version`: derived content leaves it unset, as neither the infos nor the record carry
  it. A depot build writes the version name (`2.31`) GOG's builds list gives the build
  the cache holds (#418): `linux-app` looks it up after the download, best effort, and
  records it only for that build id. Files from before that have none. An
  installer-lane build records its installer's version label (§5.1), when gogo knows it.
- The `apps list` column counts the DLC the game has. The `installed/owned` form needs
  the network, so it comes with `dlc list` (#350).
- `doctor`'s `dlc` check does the info-versus-record comparison. The owned-but-missing
  warning and the journal `fail` row come with `dlc add` (#350). As built (#350): `doctor` fails
  `dlc` while `dlc-journal.json` exists and warns while `dlc-download.json` does, each
  naming `gogo linux-app dlc <gogid> add`. The owned-but-missing line needs the network,
  so `dlc list` prints it as a note, not doctor.
- A build's plan says `dlc: none` or lists the DLC the build takes (the chosen set;
  for a tree given by path, the DLC already in it); the build flags that choose DLC
  (#349) add the `(N owned not included)` tail and the rebuild kept/added/dropped
  lines, and a cache holding more DLC adds the line of §3.3. The build's `--json` gains `build_id`, `language` and
  `dlcs`.

## 7. Progress, flags, scripting

- `dlc add` drives its download through `depot_progress::drive` (design 14 §4.1)
  unchanged: the same modes (JSON, live, plain, silent), phases and `fetched N file(s)`
  line. The build's own line before it: `gogo: downloading Phantom Liberty for Cyberpunk
  2077 (build …)`.
- The lay reuses `GameFilesStarted`/`GameFilesProgress` (design 18 §6, #337) with a
  `kind: "dlc"` so the human line reads `game files: adding 2,431 file(s) (29.2 GiB) to
  <dir>`.
- New events: `DlcPlan { dlcs, source: "cache"|"staging", buildId, fromCache? }`,
  `DlcInstalled { id }` (one per DLC after its script ran), and on recovery
  `DlcRecovered { action: "forward"|"back" }`.
- `--yes`, `--json`, `--json-progress`, `--dry-run`, `--limit`, `--no-disk-check` mean
  what they mean for `gogo depot`. `--interactive` on the build and a bare `dlc add`
  (no names, no `--all`) open the picker on a TTY; headless, a bare `add` is a usage
  error listing the owned DLC.
- Exit codes: 2 for a selection error (§2.2), 7 for depot/installer failures and the
  build refusal (§4), 130 on Ctrl-C (journal kept, §3.4).
- As built (#350): the new events are `ProgressEvent` variants (the build's stream,
  NDJSON `{"DlcPlan":{dlcs, source, buildId, fromCache?}}`, `{"DlcInstalled":{id}}`,
  `{"DlcRecovered":{action}}`). `GameFilesStarted` gains optional `kind` (`"dlc"`)
  and `files`. The download's `DepotEvent`s go through `depot_progress::drive`
  unchanged, `Finished` included. A selection error or a licence refusal is exit 2; a
  build refusal, a generation 1 build or a depot failure is exit 7; Ctrl-C during the
  download or the lay is exit 130.

## 8. Core API

In `gogo-core`, `dlc.rs` (lane-neutral):

- `DlcInfo { id, title, owned, installed: Option<InstalledDlc>, in_build: bool,
  download_size: Option<u64>, disk_size: Option<u64> }`
- `available(session, game, content) -> Vec<DlcInfo>`: catalog + owned ids + manifest
  sizes (§1.3). One network round of each; the catalog part is cached as design 06 §3.
- `resolve(names, game_title, &[DlcInfo], &Lane) -> Result<Vec<u64>, DlcSelectError>` (§2);
  `Lane::Depot { build_id }` also requires the DLC to be in the build. `select(&DlcSet, ..)`
  gives `AllOwned`'s ids.
- `plan_add(...)`, `add(..., events)`; `recover(game_dir)` (§3.4).
- `depot::DepotOptions` (#346) carries `dlcs: DlcSet` and a `dlc_context` (owned ids and the
  catalog's DLC); `DepotEvent::Plan.dlcs` is `[{id, title, downloadSize, diskSize}]`.
  `DlcSet` is `None | Ids(Vec) | AllOwned`, in place of `with_dlcs: bool`;
  `upgrade_from_same_build: bool` (§3.2) comes with #350.
- The picker is in the CLI (`macapp::prompt`'s menu helpers); the TUI later uses
  `available` + `resolve` the same way.

## 9. macOS (on hold; not ruled out) (D12)

What the Mac lane would need, none of it designed further now:

- The core (`dlc.rs`, `content.toml`, the cache record, the targeted download) is
  OS-neutral already; the Mac's game dir is `games/<gogid>/game`, outside the immutable
  launcher bundle (design 16), so laying files onto it doesn't touch the signed bundle.
- `lay_files` with APFS `clonefile` instead of FICLONE (the per-file clone `bundle.rs`
  already does).
- `gogo mac-app dlc <gogid> list|add` and the build flags; the launcher's journal check in
  the Mac launcher template.
- GOG's DLC scripts through Wine instead of umu; DLC installers through the Mac's
  installer lane (wine, same Inno flags).
- A host pass on the Mac lane (VM for the bundle, real hardware for rendering).

## 10. Testing (design 08)

**Unit (pure):**

- `resolve`: id, exact title, base-title-stripped title, ambiguous, unknown, unowned,
  installer-only on the depot lane; the error text lists the valid ones.
- Default set per §1.2's table (first/rebuild × TTY/`--yes` × flags).
- `check_cache` against records: none, older (no `dlcs`), missing DLC, extra DLC,
  language, build.
- Per-DLC size from a fixture manifest (`install_size` difference); v1 and v2.
- `content.toml` round trip, absent file fallback from fixture `goggame-*.info` trees.
- Build matching (§4) as a pure function of `(B, builds list)`.

**Integration (temp dirs, no network):**

- `lay_files` on a fixture tree: new files, a replaced base file, reflink and
  `Reflink::Never`; then **crash injection** at every step boundary (a failpoint after
  each of steps 1–4) followed by `recover`: forward when the source is whole, back when a
  source file is gone; the game tree equals either "before" or "after" byte for byte, never
  a mix. Journal present → launcher script refuses (the existing launcher test harness).
- The downloader call is behind the existing depot seam: a fake that writes files into
  the root checks that the builder got `upgrade_from(B)`, `old_dlcs = I`, `dlcs = I ∪ W`,
  `verify(false)`, and the recorded language.
- CLI snapshots: `dlc list` human and `--json`, the plan lines, the error messages,
  NDJSON events.
- Offline lane: the installer argument list for a DLC (`/DIR` = the base's), the order,
  and that a failing DLC leaves no game dir (a fake runner).

- **A DLC the cache holds** (§3.2, #381): laid with no fetch, `content.toml` and the
  install script; a shared path laid with its backup; a case-mismatched list path; a
  partial copy and a record without file lists falling back to the download; a stop
  mid-lay rolling forward, and back when the cache lost a file; a mixed add
  fetching only the other DLC.
- **The cache keeps what an add fetches** (§3.2, #374): an older record brought up
  to date (language, DLC from the infos present, file lists), then an add with the
  cache as its source, both downloading and laying a held DLC with no download; an
  add while a staged DLC isn't in the cache, which keeps the new one and doesn't
  claim the staged one; after each, the game removed and rebuilt from the cache
  with the DLC, with the cache `Complete` (no hashing) and nothing to fetch; an
  older record that can't be decided (a language's files missing, a path two
  languages share through other manifests, a lost file, another build), left byte
  for byte as it was while the add stages; a cache that changed between the plan
  and the add, where nothing is written or fetched.
- **The record-only add** (§3.2): a fixture manifest with a DLC whose depots are empty:
  `dlc list` shows `0`, `dlc add` runs no downloader, writes no journal, records the DLC
  and applies its script.
- The secure-link `403` (`invalid_licence`) maps to §2.2's error, and the output never
  contains the response's `error_description`.

**No real-account headless e2e step** (§11.2 R1): the account has no small game with a
small owned DLC that has depot bytes. The only owned DLC with real files are Cyberpunk
2077's, on a base of about 107 GiB compressed; the others are zero-byte entitlements. So
selection, the targeted diff (through the downloader seam), the lay, the journal and
interruption are covered by the synthetic manifest and fixture tests above (#352), and
the real thing is a host pass.

**Host pass (needs @noosxe's OK for each download):**

- `gogo linux-app dlc <gogid> list` against the real API for Cyberpunk 2077: the rows,
  ownership and sizes, before anything downloads (read-only, no OK needed).
- Cyberpunk 2077 + **REDmod** first (288.5 MiB of DLC): `dlc add` on the existing depot
  build, the journal and an interrupted add (kill during the lay, launch refused, rerun
  finishes), then a second `dlc add` as a no-op.
- Then **Phantom Liberty** (29.2 GiB): `dlc add`, and on Hyprland that the expansion shows
  in the game and old saves load. The driving case.
- **Offline lane, one DLC installer** under umu with `/DIR=` in the base game's prefix:
  the exit code, that `goggame-<dlcid>.info` lands beside the base's, and whether it
  prompts (and what it checks) when the base game's registry key is missing. Reading the
  installer first with `innoextract --list` and its `[Registry]`/`[Code]` entries can
  settle the check without running anything.

## 11. Decided and open

### 11.1 Decided (@noosxe, 2026-11-11)

1. **`--yes` and headless first builds take no DLC** (§1.2, D3). For a game with a single
   DLC, such as Cyberpunk 2077, installing it when it's owned is the obvious choice, but
   that isn't a rule that holds across games, so the default is no DLC. The plan keeps
   the `N owned DLC not included (--all-dlcs)` line.
2. **Removal is a follow-up** (§3.5, D11; #353, low priority). Built since: §3.7 (and §5.2 on the offline-installer lane, #443), with
   its own decisions (2026-11-13).

### 11.2 Settled by research (#343, comment "Research for design 19")

A read-only pass on 2026-11-11 (metadata and manifest JSON only, nothing downloaded):

- **R1, e2e candidate:** none. Only Cyberpunk 2077 has owned DLC with depot bytes
  (Phantom Liberty 29.2 GiB, REDmod 288.5 MiB, on a base of about 107 GiB). Two other
  owned DLC (Journey to the Savage Planet, Ghostrunner) have zero-byte depots. Hence
  §10's fixture tests and host pass, and the record-only add in §3.2.
- **R2, old builds:** GOG lists a few recent builds (6 of 270 for Cyberpunk); those still
  serve manifests, and older ones can't be fetched. A DLC can be missing from an older
  build's manifest. §4.
- **R3, DLC installers:** `/DIR=` with silent flags in the base game's prefix is Inno's
  documented flag and Lutris's precedent; what the installer checks for the base game is
  unconfirmed. §5's assumption and the host pass in §10.
- **R4, unowned DLC:** the secure link is refused with `403`, `error: invalid_licence`, for
  an unowned or nonexistent product. §1.1, §2.2.

## 12. Implementation issues (sub-issues of #343)

Filed 2026-11-11, each a sub-issue of #343.

| # | Title | Depends on |
|---|---|---|
| I1 (#346) | DLC core: `dlc.rs` (`available`, `resolve`, owned set, per-DLC sizes), `DlcSet` in `DepotOptions`, `--dlc`/`--all-dlcs` on `depot` and `download` (owned only), the secure-link `403` mapping (§1.1, §2, §8) | — |
| I2 (#347) | Depot cache record: `dlcs`/`language`, `check_cache` answers, targeted completion (#336) (§3.3) | I1 |
| I3 (#348) | `content.toml` per game, written by both build lanes; `apps list/show`, `doctor`'s `dlc` check, plan and `--json` (§6) | I1 |
| I4 (#349) | `linux-app` build flags `--dlc`/`--all-dlcs`/`--no-dlcs`/`--lang`, the default rules and picker (§1.2), DLC scripts in setup (§3.6) | I1, I2, I3 |
| I5 (#350) | `linux-app dlc list` and `add` on the depot lane: targeted download, staging, `lay_files`, journal, recovery, launcher refusal, the record-only add of a zero-byte DLC (§3.2, §3.4, §4, §7) | I2, I3, I4 |
| I6 (#351) | Offline-installer lane: DLC installers at first build and `dlc add` (§5) | I3, I4 |
| I7 (#352) | DLC fixture tests (synthetic manifests: diff, lay, journal, interruption, zero-byte DLC) and the Cyberpunk host-pass runbook (§10) | I5 |
| I8 (#353) | DLC removal (§3.5; as built §3.7) | I5 |
| #443 | DLC removal on the offline-installer lane (as built §5.2) | I6, I8 |
| I9 (#354) | mac-app DLC (§9) | I5, the Mac lane resuming |
