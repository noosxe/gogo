# 09 — v2 roadmap (depots + TUI)

Scope contract: nothing in v1 blocks these; the seams they need already exist
(library-first design, `ProgressEvent` channel, `TokenProvider`).

## 1. Model B — depot downloads

What it is: Galaxy's chunked content system (`content-system.gog.com`): builds → manifests →
secure links → zlib-compressed md5-named chunks + delta patches. This is what Heroic/gogdl
install from; it yields game *data* rather than offline installers.

Approach:

- gog-warp already implements the client side (`get_builds`, `get_manifest`,
  `get_dependencies_manifest`, and a downloader). Reuse it wholesale behind a new
  `engine::model_b` adapter rather than reimplementing.
- The adapter maps gog-warp's downloader progress onto our `ProgressEvent` stream and its
  results into `InstallRecord`s with `kind = "depot"`.
- New CLI surface stays additive: `gogo install <game>` (depot) vs `gogo download` (installers);
  a `[depot]` config section (install dir, language, verify level).
- Open questions deferred to v2 design: delta-patch application order, dependencies
  (`get_dependencies_manifest`), disk layout compatibility with Heroic (nice-to-have, not a
  goal).

## 2. ratatui TUI

- Crate `crates/gogo-tui` — workspace member already reserved (01 §1); add ratatui/crossterm deps and the screen code.
- Consumes `gogo_core`: same plan/execute APIs and the `ProgressEvent` broadcast stream —
  no engine changes required (this was the point of the event channel, 01 §4).
- Screens (sketch):
  - **Library**: searchable game list (installed state, versions, sizes) → Enter = detail
  - **Detail**: download entries tree (kind/os/language/version/size), multi-select,
    queue selected
  - **Queue/Transfers**: live per-file/chunk progress, pause (cancel token per file),
    resume, retry failed
  - **Settings**: config keys with the same validation as 05; writes config.toml
- Input plumbing: crossterm events; render loop at ~10 fps driven by event/batch ticks;
  long operations (login) shell out to the same interactive flow as the CLI.

## 3. Backlog (unscheduled ideas)

- `--prune-versions N` cleanup for stacked installer versions (04 §8)
- Library export/import (`gogo export > library.json`) for sync between machines
- Hidden games: surface products the default library query excludes (`isHidden` metadata
  ships with every page; needs endpoint/param research — the 2026-10-08 capture recorded an
  unverified `hiddenFlag` query param worth one more try)
- Extras type filtering (soundtrack/artwork/manual via bonus_content names)
- Hash cache for `verify` (store per-chunk verified-at timestamps to skip unchanged files)
- Post-download hand-off hints: detect Lutris/Bottles/Steam compatdata prefixes and print (or
  copy) the installer into place — gogo still never executes anything itself

## 4. What would change our mind

- If GOG retires the installer downlink path (Model A) → depots become mandatory; the v2 work
  moves up and the CLI defaults to `install`.
- If gog-warp stalls upstream (no releases), we already own the auth-adjacent surface via
  `TokenProvider`; worst case we vendor the two files we use (Core token lifecycle) — scoped,
  auditable, and the discovery docs contain everything needed to reimplement.
