# gogo

A Rust CLI for [GOG.com](https://www.gog.com): sign in, list your library, download
offline installers — resumable, MD5-verified, rate-limited. On macOS it can also
turn a GOG Windows game into a `.app` that runs it through Wine, picking the
renderer and fixes per game.

- **Platforms:** Linux and macOS. `mac-app` is macOS-only.
- **Wine runtimes:** `gogo runtimes` downloads, verifies, and caches official
  macOS Wine builds (the CrossOver-source `winecx` default, staging/devel/stable)
  plus a pinned `winetricks` — `mac-app` picks one for you (or takes
  `--runtime`), no Homebrew required —
  see [`docs/design/12`](docs/design/12-runtime-catalog-and-interactive-flow.md).
- **Unofficial APIs:** gogo uses the same undocumented web/Galaxy APIs as every
  third-party GOG client. See [`docs/01`](docs/01-gog-platform-and-apis.md).
- **License:** MIT OR Apache-2.0. Compat DB entries translated from umu-protonfixes
  are BSD-2-Clause ([`compat/NOTICE-umu-protonfixes`](compat/NOTICE-umu-protonfixes)).

## Install

```sh
# Nix
nix run github:noosxe/gogo -- --version

# or build from source (Rust 1.99+)
cargo install --locked --git https://github.com/noosxe/gogo
```

## Usage

```
gogo login            # sign in: browser + paste-code, tokens in the OS keychain
gogo list             # your library (id, title, platforms)
gogo download <GAME>  # offline installers, chunk-verified and resumable
gogo depot <GAME>     # straight from GOG's depot into the user cache dir
gogo mac-app <GAME>   # macOS: game -> .app from its depot build (no installer run)
gogo mac-app <EXE>    # macOS: installer -> .app
gogo runtimes list    # macOS: Wine catalog + what's cached locally
gogo recipes show <slug>  # what a Lutris recipe would do, step by step
```

`<GAME>` takes a numeric id, a slug, or a fuzzy title; `gogo download --all` grabs
everything (with a confirmation unless `--yes`). Every command has `--help` with
the full flag list, and `--json` / `--json-progress` for scripting.

### download

```sh
gogo download pharaoh                          # fuzzy title match
gogo download 1207658916 --with-dlcs           # by id, include DLCs
gogo download --all --yes --os windows         # everything, Windows installers
gogo download "hot brass" --extras --patches   # + bonus content and patches
gogo depot pharaoh --dry-run                   # plan a depot (Model B) download
gogo depot 1207658916 --with-dlcs              # materialize game files to ~/Library/Caches
```

Useful flags: `--dir`, `--workers`, `--parallel-files`, `--limit 4MiB/s` (rate
cap), `--language`, `--dry-run`, `--force`. Files are verified per 10 MiB chunk
against GOG's checksum XMLs and can be resumed; already-present files are
recorded and skipped. Re-check a download root any time with
`gogo verify [--fix]`, and pick up new versions with `gogo update [GAME]`.

Config file: `gogo config path|get|set|edit` (keys mirror the flags, e.g.
`gogo config set download.limit 8MiB/s`).

### mac-app (macOS)

Builds a `<name>.app` that runs a GOG Windows game through Wine.

```sh
gogo mac-app pharaoh                                       # depot lane (default)
gogo mac-app ~/Downloads/setup_pharaoh_gold_2.1.0.15.exe   # installer lane
```

Given a game (id, slug or fuzzy title), it takes the **depot lane**: the game's
files come straight from GOG's depot (downloaded into the cache on first use)
and no installer runs. Games without a depot build fall back to their downloaded
offline installer, with a notice. `--from-depot <GAME|DIR>` forces the depot
lane (no fallback) and also takes a game tree directory. Given a `setup_*.exe`
path, it takes the **installer lane** and runs the installer silently. Without
`--yes` it prints the plan (source, compat status, runtime, renderer, verbs,
size) and asks first; without a terminal it prints the plan and stops.

**What goes where.** The `.app` is a small signed launcher plus a
`gogo-launch.toml` saying what to run; it is never written after signing.
Everything that changes lives in `~/Library/Application Support/gogo/`:
runtimes in `runtimes/`, and per game in `games/<gogid>/` the Wine prefix (saves
included), the game files (an APFS clone of the depot tree) and `state.toml`.
gogo sets up the prefix at build time; at launch the launcher re-runs
`gogo mac-app setup <gogid>` only when the config changed or the prefix is gone.
Deleting the `.app` keeps the game and its saves (rebuild to get a launcher
back); deleting `games/<gogid>/` removes the game, prefix and saves, and the
`.app` needs a rebuild.

**No questions asked.** Compatibility comes in layers, each overriding the one
below (docs/19 §6):

- runtime: the CrossOver-source `winecx` 24.0.7_7 engine from gogo's store
  (while none is cached, a Homebrew Wine is used instead; `--runtime winecx`
  forces the download);
- renderer from the game's PE imports: DXMT for D3D10/11, wined3d for D3D9 and
  older, cnc-ddraw for 2D DirectDraw, plus winetricks verbs for the d3dx /
  d3dcompiler DLLs it imports;
- GOG's own fixes from the depot (support files, launch arguments);
- gogo's compat DB (`compat/` in this repo, compiled in; no entries yet),
  whose status (`untested`, `known to boot on …`, `known broken: …`) shows in
  the plan;
- your override, `~/Library/Application Support/gogo/compat/<gogid>.toml`, same
  schema, read at every launch;
- flags (`--renderer`, `--runtime`), which win; if the override file already sets
  that key, the flag's value is written into it so it keeps winning.

`gogo mac-app compat <gogid>` prints the merged result and which layer each
field came from.

**D3DMetal** (Apple's D3D11/12 translator, 64-bit games only) is opt-in:
`--renderer d3dmetal` asks once on a terminal, since it is downloaded under
Apple's licence, and keeps the answer as `[mac_app] d3dmetal = "yes"` in the
config.

**Changing a built game** needs no rebuild; the `.app` stays sealed:

```sh
gogo mac-app --retarget <bundle|gogid> --renderer wined3d       # writes the override file
gogo mac-app --retarget <bundle|gogid> --runtime winecx --runtime-version 24.0.7_7
gogo mac-app --retarget <bundle|gogid> --renderer auto          # drop the override
gogo mac-app next-renderer <gogid> --dry-run                    # what "Try next renderer" would pick
```

**When a game won't start.** Each launch writes a log to
`~/Library/Logs/gogo/<gogid>/`. If the game quits within 15 s or with an error,
the launcher shows a dialog with the last error lines and *Open log*, *Copy
report* and *Try next renderer* (which runs `gogo mac-app next-renderer` and
starts the game again). `gogo mac-app doctor` is planned (#79).

**Recipes and runtime menus are opt-in.** `--interactive` asks for both in
menus on a terminal; bare `--recipe` or `--runtime` asks for just that one.
Lutris recipes (`--recipe auto|<slug>|<file.json>`) are applied step by step,
unsupported steps are skipped with a note; `gogo recipes list|show` inspects
them without building anything. Pass `--out` for the output directory and
`--name` to override the bundle title.

Runtimes are managed explicitly too:

```sh
gogo runtimes list                  # catalog (live-probed) + cached versions
gogo runtimes install wine-staging  # download → probe → cache, reused by every bundle
gogo runtimes prune --all           # what no game uses, keeping each id's previous pin
```

Details: [`docs/19`](docs/19-mac-app-compatibility-strategy.md) (why the
layers) and [design 16](docs/design/16-macapp-v2.md) (bundle, state dir,
launcher). Launch-testing bundles without stealing focus: [`docs/vm-testing.md`](docs/vm-testing.md).

### completions

```sh
gogo completions fish | source    # also: bash, zsh, elvish, powershell
```

## Docs

The design docs are the contract: [`docs/design/`](docs/design/README.md)
(architecture, auth, download pipeline, macOS bundles, error model, testing).
Discovery research with live-verified API facts lives in
[`docs/01`–`docs/19`](docs/README.md).

## Development

Everything builds inside the pinned Nix dev shell (`direnv allow`, or prefix
commands with `nix develop -c`):

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo nextest run
```

PRs only — `main` is protected; merging is a human decision.
