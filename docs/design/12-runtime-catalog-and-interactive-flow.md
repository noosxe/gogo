# 12 — Runtime catalog, store & interactive mac-app flow

**Status:** implemented — phases 0–5 shipped (PRs #27–#31 + the phase-5 PR;
PR #31's merge landed its commit on main server-side but never flipped the PR
state — closed manually, content verified identical)
**Research:** docs/15 (live-verified 2026-10-19)
**Amends:** design 10 §3 (runtime resolution), §8 (network-after-staging exception),
§10 (shared runtimes: the *download store* moves from future work into v1; bundles stay
self-contained); design 11 §3.1 (winetricks posture, `wine.version` advisory), §9;
design 03 §1 (`mac-app` flags, new `runtimes` command). Exit codes unchanged — the
`MacAppError` taxonomy stays exit 7.

## 1. Goals

1. Discover and catalog the community Wine runtime variants for macOS (docs/15).
2. Interactive `mac-app`: the user picks a **recipe first** (with a `custom` option);
   the runtime question is only asked when the recipe doesn't answer it.
3. Download and cache the chosen runtime for later reuse.
4. Proper Lutris community-patch application: auto-provisioned winetricks plus
   Rust-made extractor shims — no host brew installs required.
5. Recipes that specify a wine runtime produce a **recommendation** (advisory).

## 2. Locked decisions

| # | Decision |
|---|---|
| D1 | The catalog is **data + live probes**, not code: entries carry `status` (available / gone / conditional / user-acquired / reference) and are re-probed at use time; dead entries remain as legible tombstones (docs/15 §1). Availability is a moving target — the wine-crossover 404 between 2026-10-18 and 2026-10-19 is the proof. |
| D2 | **Recipe selection precedes runtime resolution** (both in `plan()`): the runtime must exist before any wine runs (prefix staging, silent install), so a recipe-driven runtime choice requires the recipe to be known first. Precedence: **explicit flag > recipe recommendation > default**. |
| D3 | Bundles stay **self-contained** (runtime copied into `Contents/Resources/wine/`, design 10 §3). The store dedupes *downloads*, not bundle size. Shared-reference bundles remain future work (design 10 §10). **Superseded 2026-10-23 by design 16:** default bundles reference the store runtime by id; self-contained copies remain only for `--portable` (#84). |
| D4 | winetricks flips from "user-installed bridge" to a **pinned vendored download** (sha256, under `runtimes/tools/`), with `WINETRICKS` env var / user-installed binary as override, and skip-loud as the floor. |
| D5 | gogo ships **Rust-made extractor shims** (`cabextract`, `7z`): a `gogo-shims` crate (readers: `cab` MIT, `sevenz-rust` Apache-2.0) behind the CLI's hidden `__shim` subcommand; gogo-core materializes tiny re-exec launcher scripts into the tools dir and PATH-scopes them to the winetricks invocation — zero host installs, shims always in lockstep with the running binary. The 7z reader doubles as recipe `extract` support (closes a design 11 §3.1 gap; rar stays v2). |
| D6 | Recipe `wine.version` is **advisory**: mac-servable pins pre-answer the runtime prompt; Linux-only pins fall back with a notice; never a blocker (docs/15 §6). |
| D7 | Rich prompts: dialoguer (workspace pin 0.12, default features include `FuzzySelect`) in `gogo-cli`; gogo-core exposes a **prompt-spec** callback (testable, UI-agnostic). Ctrl-C/EOF aborts the command (exit 2 + `aborted`) — never a silent default. |
| D8 | Design 10 §8 network carve-out: runtime/tool downloads, recipe `files:`, and winetricks verb fetches are the explicit, named exceptions to "no network after installer staging". |
| D9 | GPTK lane (audit finding, docs/15 §8.3): gogo may **download** Gcenx's `game-porting-toolkit` release **on explicit user consent** — the Apple EULA is shown and acknowledged at the prompt (brew is the precedent conduit); gogo never redistributes the payload in its own artifacts; produced bundles embed it only as the user's copy with `README-GPTK.txt`. Registering a user-supplied mount stays available. |

## 3. Catalog (gogo-core: `macapp/catalog.rs`)

```rust
struct CatalogEntry {
    id: &'static str,          // "wine-staging" | "wine-devel" | "wine-stable" | "winetricks"
    kind: Kind,                // Wine | Tool
    status: Status,            // Available | Gone { since, story } | Conditional { audit } | UserAcquired | Reference
    discovery: Discovery,      // GitHubReleases { repo, asset_glob, tolerate_prerelease } | RawUrl { url, sha256 }
    license: &'static str,     // "LGPL-2.1"
    redistributable: bool,     // false => download path disabled, register-only (GPTK)
    min_macos: Option<&'static str>,
    arch: &'static str,        // "x86_64 (Rosetta on Apple Silicon)" | "aarch64"
}
```

v1 entries (facts in docs/15): `wine-staging` (recommended default — gaming patches),
`wine-devel`, `wine-stable` (naming confirmed — same release train, `_N` tag
suffixes tolerated), `winetricks` (tool), GPTK = consent-download with EULA notice
(D9), `wine-crossover` = tombstone. Layout rule for macOS_Wine_builds extracts: the
archive root is an app wrapper (`Wine <Flavor>.app/`); the wine tree — **gecko and
mono included** — sits at `Contents/Resources/wine/`, so post-extract discovery
probes `<root>/*.app/Contents/Resources/wine/bin/wine{,64}` and falls back to bare
`<root>/bin/wine{,64}`. Probe rules: GitHub `/releases/latest` → on 404 try
`/releases` including prereleases → else tombstone; trust the GitHub asset
`digest: sha256:…` when present; `redistributable: false` bars redistribution in
gogo artifacts (GPTK: consent-download conduit only, D9).

## 4. Store (gogo-core: `macapp/store.rs`)

> **Amended 2026-10-23 (design 16 §6):** the store grows to
> `runtimes/<id>/<ver>/{engine,Frameworks}`, derived
> `runtimes/<id>/<ver>+<renderer>-<rver>/engine`, `renderers/<id>/<ver>/<arch>/` and
> per-game `games/<gogid>/`. `prune` never removes anything a game references and keeps
> N-1. The flat layout below stays readable.

```
<state_dir>/runtimes/                  # ~/Library/Application Support/gogo/runtimes/ on macOS
  wine-staging/11.18/…                 # extracted + probed runtime tree
  tools/winetricks/<sha256[:12]>/winetricks
  tools/shims/{cabextract,7z}          # materialized from embedded bytes
  manifest.json                        # { id, version, sha256, fetched_at, upstream_url }
```

- Mirrors `state/cache.rs` conventions: `GOGO_HOME` flattening for tests, atomic
  install (download → verify sha256 → extract to temp → `find_wine_in` + `wine
  --version` probe → rename into place), manifest update after success.
- Archive deleted after successful extract + probe (manifest keeps the sha256 for
  re-download); ~0.6–1 GiB per extracted wine version, stated honestly in the menu and
  the plan confirmation.
- `list()` / `prune()` for the CLI.

## 5. Resolution & the runtime menu

`RuntimeMode` gains catalog/cached specs; `auto` resolution order:

1. cached runtime matching the recipe recommendation (D6) → 2. cached default
   (wine-staging) → 3. brew (if present) → 4. offer download (prompt on TTY; fail with
   the tombstone explanation otherwise).

Menu content: cached entries (`✓ cached`), catalog entries (size + arch tags
`(x86_64 · Rosetta)` / `(Apple Silicon · macOS 14+)`), brew (only if detected), GPTK
mount registration, custom path. Apple-Silicon hosts get a Rosetta presence check with
a one-line remedy; Intel hosts see no GPTK entries. GPTK downloads only on explicit
consent (EULA notice, D9); registering a user mount stays available.

> **Amended 2026-10-23 (design 16 §5, docs/19 §6.1):** the runtime is **data, not a
> menu**: the L0 default track, overridden by L3/L4 and flags (precedence in design 16
> §5). The mandatory menu below is superseded. The exact interactive surface for
> choosing a runtime by hand is #69's call.
>
> **Amended 2026-10-23 (#76) — the menu is opt-in.** `mac-app` resolves the runtime
> with no prompt: `--runtime <id>` wins, else the ladder (the L3 compat pin, then the
> recipe recommendation, then the cached default track, brew, else a download of the
> default track); L4 is applied on top by `gogo mac-app setup` at launch, so the
> effective order is flag > L4 > L3 > default track. The menu below (content and
> "· easy" preselection unchanged) runs only on a terminal and only when asked for:
> `--runtime` without a value, or `--interactive` (recipe picker too). `--retarget
> --runtime <id>` (#69) is unaffected; a bare `--runtime` there is a usage error, and
> `--interactive` conflicts with `--retarget`.
> Headless and `--json` runs never prompt, as before. Core: `MacAppOptions.runtime_menu`.
>
> **Amended 2026-10-19** (post-v1 field feedback): interactively the runtime
> menu is **mandatory** — the §5 ladder now only computes the preselected
> "· easy" default (the pick gogo would have used: cached recommendation →
> cached staging → brew); the menu itself always runs and lists *every*
> option: cached flavors (✓), locally installed lanes (brew / GPTK, probed
> version), downloadable flavors with arch tags, GPTK behind the D9 consent
> gate, and the custom-path lane. Headless runs keep the Phase-3 behavior
> (easy pick, else download the recommendation). `PromptSpec` gained a
> `default: usize` field (§9 contract) so renderers preselect the easy entry.

## 6. Pipeline restructure (recipe-first)

`plan()`:

1. derive display name / slug (unchanged);
2. **recipe discovery + selection** — search Lutris by display name (`--name`
   overrides), rank per design 11 §2, prompt (D7); `--recipe` / non-TTY bypass;
3. **runtime resolution** — D2 precedence; download into the store if needed (progress
   events); probe;
4. sizes; emit `RuntimeResolved` plus the new download events.

`execute()`: unchanged through the silent install; recipe *application* stays
post-install (prefix exists, `goggame-*.info` available); the authoritative
`gogid`/`gogslug` cross-check now runs post-install against the **pre-selected**
script — on mismatch: `RecipeMiss` warn + recipe-less continuation (same semantics as
today, later in the pipeline). The plan-confirmation screen gains recipe, runtime, and
pending-download lines, so the full cost is visible before the copy starts.

## 7. Recipe → runtime mapping (data, not code)

| `wine.version` (or absent) | macOS action |
|---|---|
| absent / `system` / `*-ge*` / `GE-Proton*` / `lutris-*` | recommend **wine-staging**; notice: "recipe targets Linux runner; closest macOS equivalent" |
| `wine-crossover*` / `CX*` | cached wine-crossover if the user has one; else wine-staging + tombstone notice |
| future mac-servable ids | exact catalog match |

Encoded as catalog data (D1): a wine-crossover resurgence is an entry update, not a
code change.

## 8. Winetricks provisioning + Rust shims

- Provisioning: manifest-driven — download the pinned raw URL → sha256 verify →
  install under `tools/`. LGPL notice written into produced bundles alongside
  `LICENSE-wine.txt`.
- Source (#106): `Discovery::PinnedFile` — `src/winetricks` at a release tag
  (20260125). Since 20220411 Winetricks releases upload only `<tag>.tar.gz.asc`, a
  signature over GitHub's generated source archive; that archive isn't byte-stable and
  the release feed's newest real tarball is 20210206, so neither the feed nor the
  archive can be pinned. The probe asks the contents API for the file at the tag and
  refuses a blob id other than the pinned one (a moved tag), and the raw download is
  checked against the pinned sha256. The `.asc` isn't checked: it signs the archive,
  not the file, and would need gpg plus a pinned key, which the sha256 pin already
  subsumes. Bumping winetricks is a catalog edit (tag, blob id, sha256).
- Resolution: `WINETRICKS` → the store's copy of the pinned version → any other
  store copy → `winetricks` on PATH. Auto-provision (`resolve_or_provision_winetricks`)
  upgrades a stale store copy to the pin, says so with the
  `gogo runtimes prune winetricks/<old>` remedy, and falls back to the stale copy if
  the download fails.
- Execution: `WINE=<bundled wine> WINEPREFIX=<bundle prefix> winetricks -q <verb>` —
  one verb per call, argv logged, timeout guard, self-update disabled.
- **Shims** (new workspace crate `gogo-shims`, binaries `cabextract` + `7z`):
  pure-Rust readers (`cab`, `sevenz-rust` — license audit docs/15 §8.6); embedded via
  `include_bytes!` per target arch; materialized to `tools/shims/` with mode 755 (self-
  written files carry no quarantine); **PATH prepend scoped to the winetricks child
  process only** — the user's environment is untouched.
- Degradation ladder: verb covered by shim → run; verb needs `unrar`/`xz` → skip-loud
  with the named remedy; winetricks unfetchable → user-installed binary; still nothing
  → skip-loud floor (today's behavior).
- Recipe `extract` steps reuse the same crates: 7z archives no longer skip-loud.

## 9. Interactive CLI (`gogo-cli`)

- Core grows the `SelectFn` contract into a **prompt-spec** (title, styled items,
  whether an "other…" free-text entry is offered); the CLI installs a dialoguer-backed
  implementation in a new `prompt.rs`; core tests inject fakes. dialoguer never leaks
  into gogo-core.
- Widgets: recipe picker `FuzzySelect` (items: name · version · updated · rating ·
  author; trailing `custom` / `none` entries); runtime menu `Select`; custom path/slug
  `Input` with re-prompt on invalid input; final `Confirm` (mirrors `download --all`).
- Free text typed into the recipe prompt resolves as a Lutris installer slug first,
  then as a local script-file path.
- TTY gating per design 03 §3; `--json` / `--json-progress` imply non-interactive;
  prompts render on stderr so NDJSON on stdout stays clean.

## 10. CLI surface changes

- `--runtime` accepts `auto | brew | gptk | path | <catalog-id>` (staging/devel/stable);
  new `--runtime-version <ver>`; `--recipe` unchanged.
- New command: `gogo runtimes list [--json]`, `gogo runtimes install <id> [--version]`,
  `gogo runtimes prune [--all | <id>]`.
- Carried debt (design 11 §1): `gogo recipes list|show` — resolved in Phase 5:
  `recipes list <installer-path> | --name <game>` prints the ranked
  `runner: wine` installers; `recipes show <installer-slug|file>` prints the
  classified plan (applied vs skipped-with-reasons) plus the wine pin's §7
  runtime recommendation. Nothing is executed or stored.
- Errors: `MacAppError` gains `CatalogUnreachable`, `RuntimeDownloadFailed`,
  `RuntimeChecksumMismatch` (all exit 7). Events: `RuntimeDownloadStarted` /
  `RuntimeDownloadFinished`, `ToolProvisioned`.

## 11. Testing (08 conventions)

- Unit: catalog probe logic (wiremock: 200 / 404 / prerelease-only states), the §7
  mapping table, manifest round-trip, runtime-menu content per host arch.
- Integration: full pipeline with a fake runtime + fake winetricks script (logs argv) +
  fake catalog server; shim extractors against real cab/7z fixtures.
- New Lutris fixture carrying a `wine.version` pin (existing fixtures pin none).
- Live smoke (manual, env-gated, not CI): real staging 11.18 download + probe; one real
  game conversion exercising a `corefonts` verb.

## 12. Phased implementation (one PR each; agent opens, never merges)

| Phase | Commit/PR | Contents | Acceptance |
|---|---|---|---|
| 0 | `docs: runtime catalog research + design` | docs/15, design 12, amendments to design 10/11/03, root README staleness fix; resolve audits docs/15 §8.1–8.4 | docs consistent; audit results recorded |
| 1 | `feat(macapp): runtime catalog + store` | §3–§5 core, errors/events, `gogo runtimes` CLI | download → probe → cache → reuse round-trip against wiremock + a fake archive |
| 2 | `feat(macapp): winetricks provisioning + shims` | §8 + `gogo-shims` crate | verbs run with zero host installs (fake wine); degradation ladder proven |
| 3 | `feat(macapp): recipe-first planning` | §6–§7 restructure | plan-time selection; post-install cross-check; wine-pin fixture green |
| 4 | `feat(cli): interactive mac-app flow` | §9–§10 prompts | TTY flows driven via injected fakes; non-TTY/JSON bypasses verified |
| 5 | polish | `recipes list/show`, docs sync | fmt / clippy / nextest / deny green |

Canonical checks for every phase: `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings`, `cargo nextest run`, `cargo deny check`.

Status: 0 → PR #27, 1 → PR #28, 2 → PR #29, 3 → PR #30, 4 → PR #31 (see the
status header), 5 → this PR (recipes list/show + docs sync).

## 13. Open items

- Audits docs/15 §8: §8.1–8.4 resolved 2026-10-19 (results recorded there); §8.5–8.6 resolved with Phase 2.
- Mythic Engine artifacts as an optional second GPTK lane — deferred until demanded.
- Shared-reference bundles (no per-app runtime copy) — remains design 10 §10 future
  work; revisit after the store proves itself.
