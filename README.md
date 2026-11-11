# gogo

A Rust CLI for [GOG.com](https://www.gog.com): sign in, list your library, download
offline installers — resumable, MD5-verified, rate-limited. On macOS it can also
turn a GOG Windows game into a `.app` that runs it through Wine, and on Linux
set one up to run through GE-Proton and umu, picking the renderer and fixes per
game.

- **Platforms:** Linux and macOS. `mac-app` is macOS-only, `linux-app`
  Linux-only.
- **Wine runtimes:** `gogo runtimes` downloads, verifies, and caches official
  macOS Wine builds (the CrossOver-source `winecx26` default, `winecx` rollback, staging/devel/stable)
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
nix profile install github:noosxe/gogo
# Nix, x86_64 Linux: with the FHS env `linux-app` needs on NixOS (umu; ~1.5 GiB more)
nix profile install github:noosxe/gogo#gogo-linux-app

# or build from source (Rust 1.99+)
cargo install --locked --git https://github.com/noosxe/gogo
```

**Which package:** `gogo` (~78 MiB closure) everywhere. `gogo-linux-app` only
on x86_64 Linux, and only for `linux-app` on NixOS: the same binary, wrapped
with the Steam-runtime FHS env umu-launcher runs in (~1.5 GiB). **Install
one, never both:** `gogo-linux-app` already contains gogo, and both ship
`bin/gogo`, so listing both in one profile, `home.packages` or
`environment.systemPackages` fails the build with a `collision between …`
error (`nix build --dry-run` doesn't catch it).

### As a flake input (NixOS, home-manager)

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    home-manager = {
      url = "github:nix-community/home-manager/release-26.05";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    gogo.url = "github:noosxe/gogo";
    # Optional, see below:
    # gogo.inputs.nixpkgs.follows = "nixpkgs";
  };
}
```

`gogo.overlays.default` adds `pkgs.gogo`, and `pkgs.gogo-linux-app` on
x86_64 Linux. They are gogo's own `packages`, built from gogo's pinned
nixpkgs and Rust toolchain whatever channel you're on: the same store paths
as `nix profile install`.

`gogo.inputs.nixpkgs.follows = "nixpkgs"` builds gogo against your nixpkgs
instead (still with gogo's pinned Rust 1.99): one nixpkgs in your lock and no
second copy of glibc/openssl. The trade-off: new store paths, so gogo
compiles on your machine and no cache has it, and your channel isn't the one
gogo is tested on.

NixOS, system-wide (`outputs = { nixpkgs, gogo, ... }:`):

```nix
nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
  system = "x86_64-linux";
  modules = [
    ./configuration.nix
    ({ pkgs, ... }: {
      nixpkgs.overlays = [ gogo.overlays.default ];
      environment.systemPackages = [ pkgs.gogo ]; # or pkgs.gogo-linux-app, not both
    })
  ];
};
```

home-manager as a NixOS module (`home-manager.useGlobalPkgs = true` picks up
the overlay above):

```nix
modules = [
  ./configuration.nix
  home-manager.nixosModules.home-manager
  {
    nixpkgs.overlays = [ gogo.overlays.default ];
    home-manager.useGlobalPkgs = true;
    home-manager.users.alice = { pkgs, ... }: {
      home.packages = [ pkgs.gogo ];
      home.stateVersion = "26.05";
    };
  }
];
```

home-manager standalone:

```nix
homeConfigurations.alice = home-manager.lib.homeManagerConfiguration {
  pkgs = import nixpkgs {
    system = "x86_64-linux";
    overlays = [ gogo.overlays.default ];
  };
  modules = [
    ({ pkgs, ... }: {
      home.packages = [ pkgs.gogo ];
      home.username = "alice";
      home.homeDirectory = "/home/alice";
      home.stateVersion = "26.05";
    })
  ];
};
```

Without the overlay, take the package straight from the flake, in any of
these modules:

```nix
({ pkgs, ... }: {
  environment.systemPackages = [ gogo.packages.${pkgs.stdenv.hostPlatform.system}.gogo ];
  # home-manager: home.packages = [ gogo.packages.${pkgs.stdenv.hostPlatform.system}.gogo ];
})
```

## Usage

```
gogo login            # sign in: browser + paste-code, tokens in the OS keychain
gogo list             # your library (id, title, platforms)
gogo download <GAME>  # offline installers, chunk-verified and resumable
gogo depot <GAME>     # straight from GOG's depot into the user cache dir
gogo mac-app <GAME>   # macOS: game -> .app from its depot build (no installer run)
gogo mac-app <EXE>    # macOS: installer -> .app
gogo linux-app <GAME> # Linux: game -> GE-Proton + umu setup from its depot build
gogo runtimes list    # macOS: Wine catalog + what's cached locally
gogo renderers list   # macOS: renderers (cnc-ddraw, DXMT, D3DMetal), same verbs
gogo apps list        # games set up by mac-app / linux-app (prefix, saves, logs)
gogo recipes show <slug>  # what a Lutris recipe would do, step by step
```

`<GAME>` takes a numeric id, a slug, or a fuzzy title; `gogo download --all` grabs
everything (with a confirmation unless `--yes`). Every command has `--help` with
the full flag list, and `--json` / `--json-progress` for scripting.

### download

```sh
gogo download pharaoh                          # fuzzy title match
gogo download 1207658916 --all-dlcs            # by id, include the DLC you own
gogo download 1207658916 --dlc "Phantom Liberty"  # one DLC, by id or title
gogo download --all --yes --os windows         # everything, Windows installers
gogo download "hot brass" --extras --patches   # + bonus content and patches
gogo depot pharaoh --dry-run                   # plan a depot (Model B) download
gogo depot 1207658916 --all-dlcs               # game files to ~/Library/Caches (Linux: ~/.cache)
gogo depot 1423049311 --list-languages         # the build's languages (--json); --language is checked against them
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
path, it takes the **installer lane** and runs the installer silently (its Inno
and Wine logs land in `games/<gogid>/install/`). Some games only work this way:
the depot Pharaoh asks for its CD, the installed one doesn't. Without
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
`.app` needs a rebuild. `gogo apps` does that bookkeeping for you:

```sh
gogo apps list                   # every game: its .app (present/missing), runtime, size, last launch
gogo apps show 1207659013        # where its state, prefix, logs and override file live
gogo apps remove 1207659013      # state, prefix, saves and logs (asks first; --keep-override)
gogo apps prune --orphaned       # every game whose .app is gone (asks first)
```

**No questions asked.** Compatibility comes in layers, each overriding the one
below (docs/19 §6):

- runtime: the CrossOver-26.3-source `winecx26` engine from gogo's store
  (docs/20; while none is cached, a Homebrew Wine is used instead; `--runtime
  winecx26` forces the download; `--runtime winecx` is the CrossOver 24 rollback);
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
gogo mac-app --retarget <bundle|gogid> --runtime winecx --runtime-version 24.0.7_7   # roll back to CX24
gogo mac-app --retarget <bundle|gogid> --renderer auto          # drop the override
gogo mac-app next-renderer <gogid> --dry-run                    # what "Try next renderer" would pick
```

**When a game won't start.** Each launch writes a log to
`~/Library/Logs/gogo/<gogid>/`. If the game quits within 15 s or with an error,
the launcher shows a dialog with the last error lines and *Open log*, *Copy
report* and *Try next renderer* (which runs `gogo mac-app next-renderer` and
starts the game again). `gogo mac-app doctor` checks everything the game needs
(signature, runtime and renderer files, prefix, Rosetta, leftover Wine processes,
the last launch's exit code) without starting it, one line per finding:

```sh
gogo mac-app doctor 1207659013                     # or the .app path; --json for scripts
gogo mac-app doctor 1207659013 --stop-leftovers    # also stop Wine processes a dead launcher left
```

**Handing a game to another Mac.** `--portable` builds a self-contained
`.app`: the Wine runtime, the renderer, the files the game's winetricks verbs
install and the game itself go inside it (hundreds of MB). Its launcher sets the
prefix up on whatever Mac it runs on, under
`~/Library/Application Support/gogo/games/<gogid>/`, and needs no gogo there.
The config is baked in at build, so `--retarget` and *Try next renderer* refuse
a portable bundle: rebuild it instead. D3DMetal and the GPTK never go into one
(Apple's licence).

```sh
gogo mac-app pharaoh --portable --out ~/Desktop
```

**Recipes and runtime menus are opt-in.** `--interactive` asks for both in
menus on a terminal; bare `--recipe` or `--runtime` asks for just that one.
Lutris recipes (`--recipe auto|<slug>|<file.json>`) are applied step by step,
unsupported steps are skipped with a note; `gogo recipes list|show` inspects
them without building anything. Pass `--out` for the output directory and
`--name` to override the bundle title.

Runtimes and renderers are managed explicitly too, with the same verbs:

```sh
gogo runtimes list                  # catalog (live-probed), cached versions, games using them
gogo runtimes install wine-staging  # download → probe → cache, reused by every bundle
gogo runtimes prune --unused        # what no game uses (a used id keeps its previous pin)
gogo renderers list                 # cnc-ddraw, DXMT, D3DMetal
gogo renderers prune dxmt/v0.80     # one version, with the runtimes built with it
```

Details: [`docs/19`](docs/19-mac-app-compatibility-strategy.md) (why the
layers) and [design 16](docs/design/16-macapp-v2.md) (bundle, state dir,
launcher). Launch-testing bundles without stealing focus: [`docs/vm-testing.md`](docs/vm-testing.md).

### linux-app (Linux)

Sets a GOG Windows game up to run through [GE-Proton](https://github.com/GloriousEggroll/proton-ge-custom)
and [umu-launcher](https://github.com/Open-Wine-Components/umu-launcher), the
way Steam runs Proton, but for GOG games.

```sh
gogo linux-app 1207659013                                  # depot lane (default)
gogo linux-app ~/Downloads/setup_pharaoh_gold_2.1.0.15.exe  # installer lane, run under umu
```

The lanes are `mac-app`'s: a game takes its depot build (falling back to its
downloaded offline installer), `--from-depot` forces the depot, a `setup_*.exe`
takes the installer lane. Without `--yes` it prints the plan and asks first.
The plan shows what runs (umu, the GE-Proton build, the Steam Linux Runtime
umu fetches on its first run), the renderer and what it sets, the verbs, how
the game files land (a reflink where the filesystem has them, else a copy), the
game's umu id and the fixes its protonfix applies.

**Asked once.** On the first build gogo asks two things, both kept in the config:

- `[linux] umu`: `zipapp`, gogo downloads its pinned umu-launcher (on NixOS
  this needs the `gogo-linux-app` package, see Install), or `system`, the
  `umu-run` on `PATH`;
- `[linux] proton_dir`, when a GE-Proton has to be downloaded and you have Steam:
  gogo's store, or Steam's `compatibilitytools.d` so Steam can use it too.

Without a terminal and with no answer stored, it stops and names the key to set.

**What goes where.** Everything lives in `~/.local/share/gogo/`: GE-Proton and
umu in `runtimes/`, and per game in `games/<gogid>/` the Proton prefix (saves
included), the game files, `gogo-launch.toml` (what the build chose),
`launch.conf` (what the launcher runs) and `state.toml`. `gogo linux-app setup
<gogid>` brings that up to date after you change the override file.

**The launcher.** The build adds the game to your desktop's menu:
`~/.local/share/applications/gogo-<gogid>.desktop`, with the game's icon,
runs `games/<gogid>/launch.sh`. You can also run that script from a terminal
and pass the game extra arguments. Before each launch it:

- runs `gogo linux-app setup` itself when the override file changed, the prefix
  is gone, or umu's Python went away (a Nix garbage collection). With no gogo to
  run, it launches what the last setup applied and logs that;
- checks the GE-Proton build. If gogo installed the build and it's gone or
  changed, setup reinstalls it. If you pointed the game at your own build, a
  change is logged and the game runs on it. A missing build with no gogo to
  reinstall it stops the launch with a dialog (exit 127).

Logs are in `~/.local/state/gogo/logs/<gogid>/`: `launch.log` (the launcher's own
journal), one log per run with the effective config at the top (the newest 10
are kept) and `latest.log`. Closing the launcher (Ctrl-C, `kill`) stops the game
and everything else running in its prefix. If the launcher itself is killed,
its watchdog does the same. A game that quits within 15 s or with an error shows
a dialog through zenity or kdialog, or a notification. The dialog has three
buttons:

- **Open log** opens the run log.
- **Try next renderer** switches the renderer and starts the game again.
- **Copy report** puts a report on the clipboard (`wl-copy` or `xclip`).
  Without either, the button is **Save report** and the report goes to
  `report.txt` next to the logs.

`launch.sh` is generated: don't edit it, since setup rewrites it whenever gogo's
launcher changes. Edit the override file instead.

**Layers** as on the Mac: the renderer from the game's imports (DXVK for
D3D8–11, VKD3D-Proton for D3D12, D7VK for D3D ≤ 7, cnc-ddraw for 2D DirectDraw,
wined3d as the fallback), GOG's own fixes, the game's protonfix from the Proton
build, gogo's compat DB, then your override file,
`~/.local/share/gogo/compat/<gogid>.toml`, whose `[platforms.linux]` table holds
the Linux values. `--renderer` and `--runtime` (`ge-proton[/<version>]` or
`path:<dir>` for a Proton build of your own) win.

**DLC** on a depot-built game, without a rebuild:

```sh
gogo linux-app dlc 1423049311 list                 # owned / installed / sizes for the installed build (--json)
gogo linux-app dlc 1423049311 add REDmod --dry-run # the plan: sizes, where the files come from, the disk
gogo linux-app dlc 1423049311 add "Phantom Liberty" --yes
```

`dlc add` fetches only the DLC's files for the game's installed build and
language. They go into the depot cache when it holds that build, else into a
staging dir in the game's dir. Then they're laid onto the game's files, and the
prefix, saves and settings stay. A DLC that replaces a game file gets that file
backed up until the add is done. If an add stops while laying files (Ctrl-C, a
crash), the game won't start until `dlc add` runs again: it finishes the add,
or undoes it if the fetched files are gone. If GOG no longer serves the
installed build, the add is refused, and the update is a rebuild:
`gogo linux-app <gogid> --dlc ...` keeps the prefix and saves.

```sh
gogo linux-app compat 1207659013                                    # merged config, layer per field, protonfix summary
gogo linux-app --retarget 1207659013 --renderer wined3d             # writes the override file, re-runs setup
gogo linux-app --retarget 1207659013 --runtime ge-proton/GE-Proton10-34
gogo linux-app --retarget 1207659013 --renderer auto                # drop the override
gogo linux-app next-renderer 1207659013 --dry-run                   # what "Try next renderer" would pick
```

**Env vars and launch args** of your own go into the same override file, and the
next launch applies them (no rebuild). `env list` shows every variable the game
gets, where each comes from (gogo's default, the renderer, gogo's compat DB, your
override, the launcher, the protonfix) and what it beats. `env list --all` shows
what *exists* for the game's Proton build: every switch from the build's own files,
what it does, its default, and whether it applies to this game's renderer and GPU.
Your args go after the game's own; `-- <args…>` replaces the ones you set before.

```sh
gogo linux-app env list 1423049311                         # --json; --reveal shows values of *TOKEN*/*SECRET*/… names
gogo linux-app env list 1423049311 --all                   # every switch on this game's Proton build
gogo linux-app env describe 1423049311 PROTON_DLSS_UPGRADE  # one in full; also VKD3D_CONFIG flags and dxvk.conf keys
gogo linux-app env set 1423049311 PROTON_DLSS_INDICATOR=1 MANGOHUD=1
gogo linux-app env unset 1423049311 MANGOHUD
gogo linux-app args 1423049311 -- --launcher-skip -skipStartScreen
gogo linux-app args 1423049311                             # what the exe gets, and from where
gogo linux-app args 1423049311 --clear
gogo linux-app wrapper 1423049311 -- gamescope -f --        # runs outside umu-run; one word per argument, no shell
gogo linux-app wrapper 1423049311 -- gamemoderun
gogo linux-app wrapper 1423049311 --clear
gogo linux-app display 1423049311 wayland --hdr force     # Wine's Wayland driver (X11 when there's no Wayland socket)
gogo linux-app display 1423049311 --monitor DP-1           # the output the game opens on (Wayland mode; hyprctl monitors)
gogo linux-app display 1423049311 --no-monitor
gogo linux-app display 1423049311                          # the mode, and what this session would get
gogo linux-app display 1423049311 --clear
```

`env set` refuses what the launcher sets itself (`WINEPREFIX`, `GAMEID`, `STORE`,
`PROTONPATH`), `WINEDLLOVERRIDES` (use `[platforms.linux.dll_overrides]`) and
`PROTONFIXES_DISABLE` (use `protonfixes = false`), and warns when yours beats a
renderer switch or gogo's `PROTON_SET_GAME_DRIVE=0`. A protonfix that sets the
same variable still wins at launch: `env list` and `doctor` say so.

**When a game won't start**, `gogo linux-app doctor <gogid>` checks what it
needs, one `ok` / `warn` / `fail` line each with a fix: the launcher and menu
entry, umu and its Python (on NixOS, that it's the `gogo-linux-app` FHS env and
still there after a garbage collection), user namespaces, the GE-Proton build
(unchanged, its marker, a Steam Linux Runtime umu knows), the runtime umu
downloads, the renderer's files, your Vulkan drivers (a 32-bit one for a 32-bit
game; a warning with more than one GPU), links out of the prefix, processes
left running in it, and the last run's log. It only reads: it never launches
the game, runs umu or stops a process. It exits 1 when a check fails.

```sh
gogo linux-app doctor 1207659013          # --json for scripts
```

`gogo apps list|show|remove|prune` work on Linux too. A game counts as orphaned
once its menu entry was made and then deleted; `apps remove` deletes the menu
entry with the game.

Details: [design 18](docs/design/18-linux-runtime.md) and
[`docs/21`](docs/21-linux-runtimes-and-renderers.md).

### completions

```sh
gogo completions fish | source    # also: bash, zsh, elvish, powershell
```

The Nix packages (`gogo`, `gogo-linux-app`) install the bash, zsh and fish completions automatically.

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
