# Design 17 — `gogo search`

Status: implemented (#149; §9 live checks pending) · Depends on: design 03 (CLI), design 06 (local
state), docs/03 + docs/11 (API shapes) · 2026-10-28.

## 0. Scope

`gogo search <QUERY>` searches **what the signed-in account owns** and prints the
matches grouped by the kind of content GOG sells: games, DLC, packs, and goodies
(soundtracks, manuals, artbooks, wallpapers …). It is read-only and never downloads.
Besides what is owned, it also shows the DLC that owned games offer but the account
does not own, marked **not owned** (§2.1). It does not search the rest of the
GOG store.

`gogo list` stays the games-only table it is today (design 03 §1). `search` is the
cross-kind view: it answers "what do I own that is called X?", including things
`list` cannot show (packs, a soundtrack sold as DLC, the manual inside a game).

## 1. What GOG's data says about kinds

Facts below are marked **live** (seen in a live capture: `gogo-probe/` from
2026-10-09, or the product cache gogo wrote on this machine), **docs** (prior-art
or community docs, not seen live) or **check** (needs a live probe, §9).

| Source | Field | Values | Status |
|---|---|---|---|
| `getFilteredProducts` (library) | `isGame` / `isMovie` | all 83 rows `isGame: true`, `isMovie: false` | live |
| `getFilteredProducts` | `category` | **genre**, not kind (`Adventure`, `Strategy`, `""` …) | live |
| `getFilteredProducts` | `dlcCount` | `0` on every row, even games with owned DLC | live — useless |
| `/user/data/games` | `owned[]` | 146 ids: 83 games + 63 others | live |
| `products/{id}` | `game_type` | `game` (Hot Brass), `dlc` (its soundtrack) | live |
| `products/{id}` | `game_type` | `pack` | docs; **check** |
| `products/{id}` | `expanded_dlcs[]` | **every** DLC of the game, owned or not | live (204 of 207 not in `owned[]`) |
| `downloads.bonus_content[]` | `type` | `audio` | live; full set **check** |
| `downloads.bonus_content[]` | `name` | `Soundtrack`, `Manual`, `Wallpapers`, `Avatars`, `Artbook`, `Goodies pack`, `Reference card`, `Comics`, `Game guide`, `Map` … | live (222 entries, 38 games) |
| account `gameDetails/{id}.json` | `extras[].type` | `wallpapers` | docs only; not used |

What the numbers mean:

- **The library list cannot tell kinds apart.** It is games only (review A1) and its
  `category` is a genre. Kinds come from the products API `game_type`.
- **Of the 63 owned non-game ids, only 3 are DLCs listed under an owned game's
  `expanded_dlcs`.** The other 60 are neither a library game nor a known DLC. They
  are most likely packs/editions (GOTY bundles, "Complete" editions) and
  goodie-only products. Their `game_type` is the main open fact (§9 Q1).
- **`expanded_dlcs` is not an ownership list.** It lists the catalog's DLCs. The
  92 unowned DLCs carrying a non-empty `downloads` block in the cache suggest the
  products API does not filter DLC downloads by ownership either (§9 Q2). Ownership
  therefore always comes from `owned[]`.
- **A goodie can be a product of its own.** "Hot Brass Original Soundtrack" is
  `game_type: dlc` with zero installers and two `bonus_content` entries
  (`type: audio`). GOG sells it as DLC; to the user it is a soundtrack.
- **`bonus_content.type` is a coarse GOG vocabulary** (`audio` for a soundtrack),
  while `name` is free text. The design classifies by `type` and shows `name`.

## 2. Categories

Four kinds, decided from the facts above. Movies are left out: the library has none
(`mediaType=2` → 0, docs/11) and v1 does not download them.

| Kind | CLI value | Rule (first match wins) |
|---|---|---|
| Game | `games` | a `getFilteredProducts` row |
| Pack | `packs` | owned id, `game_type == "pack"` |
| Goodies | `goodies` | (a) a `bonus_content` entry of an owned game or owned DLC, or (b) an owned id with `game_type == "dlc"` and no `installers` but at least one `bonus_content` entry |
| DLC | `dlc` | owned id with `game_type == "dlc"` that has installers |

Goodies carry a **subtype** from `bonus_content.type`, passed through as GOG sends it
(`audio`, …). The full value set is collected by §9 Q3 and then frozen as a
documented, non-exhaustive list; an unknown value is shown as-is, never dropped. A
goodie product (rule b) takes the subtype of its entries (`mixed` if they differ).

Anything owned that fits no rule (unknown `game_type`, product fetch failed) is
shown in an **Other** group with its raw `game_type`, so nothing owned silently
disappears. A search with no Other rows is the expected case; Other rows are a hint
that GOG added a kind.

Parents:

- DLC and goodie products link to their parent game: the owned game whose
  `expanded_dlcs` lists them. No parent found → shown without one.
- Goodie entries (rule a) always have a parent: the game or DLC that carries them.
- Packs have no parent. What a pack contains is not known from the current data
  (§9 Q4); v1 shows the pack itself only.

### 2.1 Not-owned DLC

An owned game's `expanded_dlcs` also lists the DLC the account does not own (§1).
`search` shows them, so "which DLC am I missing?" has an answer:

- A DLC id in an owned game's `expanded_dlcs` but not in `owned[]` is **not owned**.
  It is classified by the same rules (DLC, or goodies for a goodie-only DLC) and
  marked `not owned`.
- Not-owned rows show id, title and parent only. No OS, size or downloaded state:
  whether their `downloads` block is real is unchecked (§9 Q2), and they cannot be
  downloaded anyway. Their `bonus_content` entries are not listed as goodies.
- Only DLC of owned games appear. Packs and games the account does not own are
  store questions and stay out.
- `--owned` hides not-owned rows.

## 3. Command surface

```
gogo search <QUERY>... [-c, --category <KIND>]... [--goodies-type <TYPE>]
                       [--os <windows|mac|linux|all>] [--owned] [--refresh] [--json]
```

| Arg / flag | Default | Meaning |
|---|---|---|
| `<QUERY>...` | required | one or more words, joined with spaces (no quoting needed) |
| `-c, --category` | all kinds | repeatable or comma list: `games`, `dlc`, `packs`, `goodies` |
| `--goodies-type` | any | keep only goodies with this subtype (implies `-c goodies`) |
| `--os` | any | keep games/DLC with an installer for this OS; packs and goodies are not filtered |
| `--owned` | off | hide not-owned DLC (§2.1) |
| `--refresh` | off | refetch product data (as `list --refresh`) |
| `--json` | off | machine output (§7); also honours the global `--json` |

An id query (`gogo search 1075439258`) is just a query whose word is all digits (§4).

Relation to existing commands:

- `list --search <text>` keeps its meaning (games, title substring) but switches to
  the §4 normalizer so `list --search pokemon` and `search pokemon` agree on games.
- `search` output gives ids and slugs that the existing selectors (`download`,
  `info`, `mac-app`) accept. A DLC id given to `download` still resolves to its
  parent (design 03 §1); `search` adds no new download path.

## 4. Matching

Deterministic and explainable; no fuzzy scoring.

1. **Normalize** query and candidate text the same way: Unicode NFKD, drop
   combining marks (so `Pokémon` = `pokemon`, `Ærø` keeps `æ`/`ø` as letters and
   matches only itself), lowercase, map `™ ® ©` to nothing, and turn every run of
   non-alphanumeric characters (`:` `-` `_` `'` spaces) into one space.
2. **Split** the normalized query into words.
3. **A candidate matches** when every query word is a substring of its normalized
   text. Word order does not matter: `witcher wild hunt` matches
   "The Witcher 3: Wild Hunt".
4. **Candidate text** per kind: game, DLC, pack → title + slug (slug `_` → space);
   goodie entry → entry `name` + subtype; goodie product → title + slug + subtype.
5. **Parent match:** DLC and goodies also match when the query matches their
   parent game's text. The match reason is kept (`self` | `parent`) — see §5.
   The parent match tests the row's own text and its parent's text together, so
   `witcher hearts` finds "Hearts of Stone" under "The Witcher 3" (a goodie of a
   DLC takes both the DLC's and the game's text).
6. **Id:** a single all-digit word also matches a product id exactly (any kind).
   Goodie entries have numeric ids of their own; they match too.

Matching itself has no typo tolerance: a substring miss is a miss. Ranking inside a group: exact normalized title match → title
starts with the query → the rest; ties by title.

**"Did you mean" on no match.** Only when a search finds nothing at all:

1. Build a vocabulary of the normalized words in every candidate's text (titles,
   slugs, goodie names; a few hundred words for this library).
2. For each query word that is not a substring of any vocabulary word, take the
   closest vocabulary word by Damerau-Levenshtein distance (`strsim`): at most 1
   edit for words of 4 characters or fewer, 2 for longer ones. Ties go to the word
   that occurs in more candidates, then alphabetical. No candidate within the cap →
   no suggestion.
3. Run the search again with the corrected words. If it finds something, print the
   suggestion; the results themselves are **not** shown, so a typo never silently
   turns into a different search (the user reruns with the suggestion).

Fuzzy ranking of every result (fzf-style scoring) is left out on purpose: it needs
score cut-offs, makes short queries noisy against 200+ goodie names, gives `--json`
consumers no clear hit/miss line, and would disagree with the substring selectors of
`download`/`info`/`mac-app`. It fits a future TUI picker better (`nucleo`).

Normalization needs one new dependency, `unicode-normalization` (MIT/Apache-2.0,
the usual crate for NFKD; check `cargo deny` when adding it). `strsim` is already in
the lockfile (clap uses it for its own suggestions); it becomes a direct dependency.

## 5. Goodies: searched, and listed under their parent

Goodies are searched in their own right (`search soundtrack` finds every owned
soundtrack) **and** grouped by parent in the output, because a bare "Manual" row
means nothing without its game.

Volume control: a game with 20 goodies would bury the result when its title
matches. So goodies matched only through their parent (§4 rule 5) are folded into
one summary line per parent in the human output — `+ 14 goodies (audio 3, …)` —
unless the user asked for goodies (`-c goodies` or `--goodies-type`), which lists
them all. Goodies matched on their own name are always listed. JSON always lists
everything, with `matched_by`.

Owned DLC matched through the parent are listed in full (few per game). Not-owned
DLC matched through the parent fold the same way as goodies: one line per parent,
`+ 23 DLC not owned`, unless `-c dlc` asks for DLC, which lists them all. Not-owned
DLC matched on their own title are always listed.

## 6. Data sources, caching and cost

| Data | Endpoint | Calls | Cached |
|---|---|---|---|
| games | `getFilteredProducts?mediaType=1` | 1 per 100 games | no (design 06 §3) |
| ownership | `/user/data/games` | 1 | no |
| game detail + DLC + goodies | `products/{id}?expand=downloads,expanded_dlcs` | 1 per game on a cache miss | yes, existing product cache |
| non-game owned ids | `products?ids=…&expand=downloads` | 1 per 50 ids not covered by a game's `expanded_dlcs` | yes, same cache |

On this library (83 games, 63 other ids, 60 of them unexplained): warm cache = 2
calls (library page + owned ids) + 0 product calls; cold = 2 + 83 detail + 2 batch
calls. The warm path is what matters: `search` should feel instant after the first
`list`. Not-owned DLC cost nothing extra: they come with the parent's detail.

Cache changes needed:

- `Product` gains `game_type` and `InstallerEntry` gains `type` (bonus subtype). The
  cache stores the typed struct, so entries written before this change lack both.
  An entry without `game_type` counts as a cache miss and is refetched once.
- Non-game products from the batch call are cached under the same
  `products/<id>.json` path.

No offline mode: like `list`, `search` needs the live library and owned ids
(design 06 §3 is unchanged). Without the network it fails with exit 4.

## 7. Output

Human (stdout), groups in fixed order, empty groups omitted:

```
$ gogo search hot brass
Games (1)
  1075439258  Hot Brass                         win mac       691 MiB  ✓ 2.2.2

Goodies (1)
  Hot Brass
    1702049827  Hot Brass Original Soundtrack   audio  (product)   1.3 GiB

$ gogo search witcher 3 -c dlc
DLC (2)
  The Witcher 3: Wild Hunt
    1202585036  Hearts of Stone                 win           3.6 GiB  ✓ 1.32
    1640424747  Blood and Wine                  not owned

$ gogo search witchr
no matches for "witchr" — did you mean "witcher"?

$ gogo search soundtrack -c goodies
Goodies (3)
  Hot Brass
    1702049827  Hot Brass Original Soundtrack   audio  (product)   1.3 GiB
  The Witcher 3: Wild Hunt
    48213       Soundtrack (MP3)                audio              412 MiB
    48214       Soundtrack (FLAC)               audio              1.1 GiB
```

(The Witcher rows and all goodie sizes are illustrative.) Columns: id, title or
entry name, then per kind — games/DLC: OS + size + downloaded state (as `list`);
packs: id + title; goodies: subtype, `(product)` for a goodie product, size.

No match: nothing on stdout, `no matches for "…"` (plus the suggestion, §4) on
stderr, exit 0 (design 03 §5: "nothing to do" is success; exit 1 is reserved for
runtime errors). With `--json` the document has empty `results` and a
`"suggestion": "witcher"` field (or `null`).

`--json` (stdout, one document):

```json
{ "query": "hot brass",
  "results": [
    { "kind": "game", "id": 1075439258, "title": "Hot Brass", "slug": "hot_brass",
      "os": ["windows", "mac"], "size": 724243232, "installed_version": "2.2.2",
      "matched_by": "self" },
    { "kind": "goodies", "id": 1702049827, "title": "Hot Brass Original Soundtrack",
      "subtype": "audio", "product": true, "parent": 1075439258, "size": 1415577600,
      "owned": true, "matched_by": "self" }
  ],
  "suggestion": null }
```

`kind` ∈ `game | dlc | pack | goodies | other`; `other` rows carry `game_type`
(raw or null). Every row carries `owned` (only DLC and goodie products can be
`false`; not-owned rows have no `os`/`size`/`installed_version`). Goodie entries carry `parent` and `parent_kind` (`game | dlc`). The
list is flat and ordered like the human output; consumers group by `kind`.

## 8. Errors and tests

| Case | Behaviour | Exit |
|---|---|---|
| empty query / only punctuation | usage error | 2 |
| unknown `--category` / bad `--goodies-type` syntax | clap error | 2 |
| not signed in (online) | as `list` | 3 |
| library or owned-ids call fails after retries | error (no offline fallback) | 4 |
| one product fetch fails | `warn:` on stderr, row degrades to Other, command continues (as `list`) | 0 |
| cache write fails | `warn:`, continue | 0 |

Tests (design 08 layout; fixtures synthesized from the live shapes):

- **Unit, `search::normalize`** — table: diacritics (`Pokémon`), `™`/`®`,
  punctuation runs, `_` in slugs, CJK passes through, empty after normalization.
- **Unit, `search::matches`** — word order, all-words rule, id match vs a
  digits-in-title game (`1979 Revolution`), parent match reason.
- **Unit, `search::suggest`** — one typo per word (`witchr` → `witcher`), the
  length cap (`mod` gets no 2-edit suggestion), tie-break order, no suggestion when
  nothing is close, multi-word query with one bad word.
- **Unit, `search::classify`** — one fixture per rule in §2: game, pack, DLC with
  installers, goodie product (Hot Brass soundtrack shape), unowned DLC in
  `expanded_dlcs` (shown as not owned, hidden by `--owned`, its bonus entries never
  goodies), unknown `game_type` → Other.
- **Unit, output** — rendered table for the two §7 examples (plain string assert);
  goodie and not-owned folding on/off; JSON shape round-trip.
- **Integration (wiremock)** — library page + owned ids + one detail + one batch:
  assert call counts cold (1+1+N+⌈M/50⌉) and warm (2); old cache entry without
  `game_type` triggers exactly one refetch.
- **Fixtures** — `fixtures/owned.json` (new, fake ids), `product.json` extended with
  `game_type` + a `bonus_content` entry carrying `type`, `products-batch.json`
  (a pack, a goodie product, an unknown type).
- **Live smoke** — `GOGO_LIVE=1`: `search` for a known title returns ≥ 1 game.

## 9. Open questions

Live checks (run with `scripts/probe.sh` once #6 makes it token-capable again;
output stays in `gogo-probe/`):

1. **Q1 — what are the 60 unexplained owned ids?** Batch-fetch them and tally
   `game_type`. Expected `pack` plus goodie products; anything else becomes a new
   kind or stays Other. Also confirm the batch endpoint returns `game_type` and
   `downloads` with `expand=downloads`.
2. **Q2 — are DLC downloads ownership-filtered?** Compare `downloads` of an owned
   vs an unowned DLC from the same game. If unowned DLC carry downloads,
   `download --with-dlcs` (design 04) has the same ownership gap; file it separately.
3. **Q3 — the `bonus_content.type` vocabulary.** Tally `type` over every product in
   the library; freeze the list in this doc (expected from names: audio, manuals,
   wallpapers, artworks, avatars, video, guides…; only `audio` is seen so far).
4. **Q4 — pack contents.** Does a pack's product entry name the products it
   contains (`expand=related_products`, `includes_games` or similar)? If yes, packs
   get children like games; if no, they stay flat.

Decided with the user (2026-10-28): "did you mean" instead of fuzzy ranking (§4);
not-owned DLC are shown (§2.1); no offline mode, the owned ids are not cached (§6).
