# gogo

A Rust CLI for [GOG.com](https://www.gog.com): sign in, list your library, download
offline installers — resumable, MD5-verified, rate-limited. On macOS it can also
convert a downloaded Windows installer into a self-contained `.app`, applying
community fixes (Lutris recipes) along the way.

- **Platforms:** Linux and macOS. `mac-app` is macOS-only.
- **No Wine bundled:** `mac-app` uses a runtime you install (Homebrew
  wine-crossover or Apple's Game Porting Toolkit). Same for `winetricks`.
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
gogo mac-app <EXE>    # macOS: installer -> .app (with a Lutris recipe)
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
brew install --cask game-porting-toolkit     # or: brew install wine-crossover / winetricks
gogo mac-app ~/Downloads/setup_pharaoh_gold_2.1.0.15.exe --runtime brew
open Pharaoh.app
```

`--runtime` picks the Wine source: `brew` (wine-crossover), `gptk` (a mounted
Game Porting Toolkit volume — implies you accept Apple's license), `auto`, or an
explicit `--runtime-path <DIR>` with a `bin/wine` inside. Pass `--out` for the
output directory, `--name` to override the bundle title.

### Recipes

GOG Windows titles are the community's Wine-testing corpus: most games already
have a Lutris install script encoding the fixes (DLL overrides, winetricks
verbs, patches, launch flags). At the end of a conversion, gogo live-searches
[lutris.net](https://lutris.net) for the game and, when scripts exist, shows the
ranked list and asks which to apply:

```
recipes available on lutris.net:
  1. pharaoh-cd-cnc-ddraw — CD + cnc-ddraw (legluondunet)
apply which? [1-2, none]: 1
```

Applied steps are reported; unsupported steps are skipped loudly, never
silently. Nothing is stored — the next conversion searches again. For
scripting: `--recipe none` to disable, `--recipe <slug>` to pick one, or
`--recipe file.json` to apply a downloaded script verbatim. Host-shell steps in
scripts are skipped by default; `winetricks` steps use your installed
`winetricks` binary (skipped with a note if absent).

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
