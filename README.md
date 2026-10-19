# gogo

A Rust CLI for [GOG.com](https://www.gog.com): sign in, list your library, download
offline installers — resumable, MD5-verified, rate-limited. On macOS it can also
convert a downloaded Windows installer into a self-contained `.app`, applying
community fixes (Lutris recipes) along the way.

- **Platforms:** Linux and macOS. `mac-app` is macOS-only.
- **Wine runtimes:** `gogo runtimes` downloads, verifies, and caches official
  macOS Wine builds (staging/devel/stable) plus a pinned `winetricks` —
  `mac-app` picks one interactively or via `--runtime`, no Homebrew required —
  see [`docs/design/12`](docs/design/12-runtime-catalog-and-interactive-flow.md).
- **Unofficial APIs:** gogo uses the same undocumented web/Galaxy APIs as every
  third-party GOG client. See [`docs/01`](docs/01-gog-platform-and-apis.md).
- **License:** MIT OR Apache-2.0.

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
gogo depot <GAME>      # straight from GOG's depot into the user cache dir
gogo mac-app --from-depot <GAME>  # wrap the depot tree (no installer run)
gogo mac-app <EXE>    # macOS: installer -> .app (with a Lutris recipe)
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

Converts a downloaded GOG Windows installer into a self-contained
`<name>.app`: it stages a Wine prefix, runs the installer silently, copies the
Wine runtime inside, and ad-hoc codesigns the bundle. The result needs nothing
else installed.

```sh
gogo mac-app ~/Downloads/setup_pharaoh_gold_2.1.0.15.exe   # interactive: recipe + runtime
open Pharaoh.app
```

On a terminal it asks interactively — which Lutris recipe to apply (if any), then
which Wine runtime: cached ones first, then catalog downloads (recommended
`wine-staging` first, sizes and arch shown), Homebrew, GPTK (Apple Silicon, after
an EULA consent), or a custom path. Non-interactive use pins everything:
`--recipe none|<slug>|<file>`, and `--runtime auto|brew|gptk|path|staging|devel|stable`
(`--runtime staging` downloads + caches on first use; `--runtime path` needs
`--runtime-path <DIR>` with `bin/wine` inside; `--runtime-version` pins a release
tag). Pass `--out` for the output directory, `--name` to override the bundle title.

Runtimes are managed explicitly too:

```sh
gogo runtimes list                  # catalog (live-probed) + cached versions
gogo runtimes install wine-staging  # download → probe → cache, reused by every bundle
gogo runtimes prune --all
```

### Recipes

GOG Windows titles are the community's Wine-testing corpus: most games already
have a Lutris install script encoding the fixes (DLL overrides, winetricks
verbs, patches, launch flags). During planning, gogo live-searches
[lutris.net](https://lutris.net) for the game and, when several scripts exist,
offers a fuzzy-searchable picker (version, author, updated shown); a script's
`wine.version` pin maps to a runtime recommendation — advisory, never a blocker.

Inspect the corpus without converting anything:

```sh
gogo recipes list --name "warcraft-ii-battlenet-edition"   # ranked candidates
gogo recipes show warcraft-ii-battlenet-edition-cd          # the plan, step by step
```

Applied steps are reported; unsupported steps are skipped loudly, never
silently. Nothing is stored — the next conversion searches again. For
scripting: `--recipe none` to disable, `--recipe <slug>` to pick one, or
`--recipe file.json` to apply a downloaded script verbatim. Host-shell steps
in scripts are skipped by default; `winetricks` steps run a pinned,
sha256-verified winetricks provisioned from the runtime store, with Rust-made
`cabextract`/`7z` shims scoped to the invocation (a user-installed binary
overrides; still nothing → skipped with a note).

### completions

```sh
gogo completions fish | source    # also: bash, zsh, elvish, powershell
```

## Docs

The design docs are the contract: [`docs/design/`](docs/design/README.md)
(architecture, auth, download pipeline, macOS bundles, recipes, error model,
testing). Discovery research with live-verified API facts lives in
[`docs/01`–`docs/14`](docs/README.md).

## Development

Everything builds inside the pinned Nix dev shell (`direnv allow`, or prefix
commands with `nix develop -c`):

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo nextest run
```

PRs only — `main` is protected; merging is a human decision.
