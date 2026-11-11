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
- Removal is out of scope for the first round (§3.5).

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
| D11 | Removal later (#343 follow-up), not in the first round | 3.5 |
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

### 1.4 `dlc add`

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

### 2.3 Language (D10)

DLC follows the game's language, always: the build's `--lang` (new, #336; default
`en-US` as `gogo depot`) is recorded in `content.toml` and in `.gogo-depot.json`, and `dlc
add` uses the recorded one. There is no `--lang` on `dlc add`: a game with base files in
one language and DLC in another isn't a state gogo should make. Changing the language is a
rebuild with `--lang` (gog-warp then diffs the language depots itself). On the offline
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
- No public per-product file list: the diff report is `pub(crate)`. gogo doesn't need it
  (§3.2 finds the new files on disk, as design 14 §4.1 already does).
- No ownership check anywhere in the downloader.

### 3.2 Adding DLC to a built game

```
resolve game → content.toml (build B, language L, installed DLC I)
  → get_builds → find B; absent → refuse (§4)
  → get_manifest(B) → resolve names against manifest.dlcs() ∩ owned → wanted W (W ∩ I = ∅)
  → source root R:
       the depot cache, if its record says build B and language L      (keeps the cache whole)
       else games/<gogid>/.dlc-staging/ (fresh)                        (no cache, or it moved on)
  → Downloader: manifest(B) upgrade_from(B) language(L) old_language(L)
                dlcs(I ∪ W) old_dlcs(I) verify(false) install_root(R)
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
| `dlcs`/`language` absent (an older gogo) | `UnknownContent` | full `verify` with the wanted set: gog-warp keeps what's valid and fetches the rest. Slow, but it can't know what the tree holds without hashing |
| wanted DLC not in `dlcs` | `MissingDlc(W)` | the targeted add of §3.2 on the cache (no hashing) |
| language differs | `OtherLanguage` | rebuild-time only: a targeted diff from (old language, `dlcs`) to (new, wanted) |
| `buildId` differs from the build being made | `OtherBuild` | as today: the new build is fetched into the cache via `verify`, with the wanted DLC set |
| `dlcs` ⊇ wanted, same language | `Complete` | reused. Extra DLC in the cache is fine: the materialize copies only what the build wants (below) |

- A cache can hold more DLC than one build wants (the user ran `gogo depot --all-dlcs`).
  The build then materializes the base and the wanted DLC's files only: `materialize_tree`
  takes a path filter built from the manifest (`get_depots` for the wanted set, its file
  paths), and the plan says `cache holds 2 more DLC than this build takes`. Until that
  filter exists (it needs the depot manifests, an extra fetch per build), a cache with
  extra DLC is materialized whole and the plan says so; that's today's behaviour.
- The record is written after `download()` as today, now with the DLC set and language
  that run asked for. A `dlc add` into the cache rewrites it with `dlcs ∪ W` and the new
  file list.

### 3.4 Laying files onto `game/` (D6)

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

### 3.5 Removal (D11): later

Not in the first round (decided, §11.1). Reasons:

- It needs the reverse of §3.4: the DLC's files, and for files it replaced, the base
  version back. gog-warp can produce that (old `dlcs = I`, new `dlcs = I \ X`: its diff
  puts X's files in `deleted` and re-downloads base files X had replaced), but the
  `deleted` list is `pub(crate)`, so gogo would need a gog-warp change or its own
  manifest walk.
- Saves made with a DLC can refuse to load without it; the warning text needs a game to
  test on.
- The offline lane would run the DLC's Inno uninstaller (`unins000.exe` beside the DLC's
  files), which is a different mechanism with its own failure modes.
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
dir has been captured live (design 17; docs/03). The first host pass (§10) checks
it.

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

## 7. Progress, flags, scripting

- `dlc add` drives its download through `depot_progress::drive` (design 14 §4.1)
  unchanged: the same modes (JSON, live, plain, silent), phases and `fetched N file(s)`
  line. The build's own line before it: `gogo: downloading Phantom Liberty for Cyberpunk
  2077 (build …)`.
- The lay reuses `GameFilesStarted`/`GameFilesProgress` (design 18 §6, #337) with a
  `kind: "dlc"` so the human line reads `game files: adding 2,431 file(s) (29.2 GiB) to
  <dir>`.
- New events: `DlcPlan { dlcs, source: "cache"|"staging", buildId }`, `DlcInstalled { id }`
  (one per DLC after its script ran), and on recovery `DlcRecovered { action:
  "forward"|"back" }`.
- `--yes`, `--json`, `--json-progress`, `--dry-run`, `--limit`, `--no-disk-check` mean
  what they mean for `gogo depot`. `--interactive` on the build and a bare `dlc add`
  (no names, no `--all`) open the picker on a TTY; headless, a bare `add` is a usage
  error listing the owned DLC.
- Exit codes: 2 for a selection error (§2.2), 7 for depot/installer failures and the
  build refusal (§4), 130 on Ctrl-C (journal kept, §3.4).

## 8. Core API

In `gogo-core`, `dlc.rs` (lane-neutral):

- `DlcInfo { id, title, owned, installed: Option<InstalledDlc>, in_build: bool,
  download_size: Option<u64>, disk_size: Option<u64> }`
- `available(session, game, content) -> Vec<DlcInfo>`: catalog + owned ids + manifest
  sizes (§1.3). One network round of each; the catalog part is cached as design 06 §3.
- `resolve(names, &[DlcInfo]) -> Result<Vec<u64>, DlcSelectError>` (§2).
- `plan_add(...)`, `add(..., events)`; `recover(game_dir)` (§3.4).
- `depot::DepotOptions` gains `dlcs: DlcSet` (`None | Ids(Vec) | AllOwned`) in place of
  `with_dlcs: bool`, and `upgrade_from_same_build: bool` for §3.2.
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
2. **Removal is a follow-up** (§3.5, D11; #353, low priority).

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
| I8 (#353) | DLC removal (§3.5) | I5 |
| I9 (#354) | mac-app DLC (§9) | I5, the Mac lane resuming |
