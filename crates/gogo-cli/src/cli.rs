//! clap derive types; args → [`Config`](crate::config::Config) overlay
//! (`docs/design/03-cli.md`, env mapping in `docs/design/05` §4).

use clap::{Parser, Subcommand};

/// `gogo` — sign in to GOG, list your library, download offline installers.
#[derive(Debug, Parser)]
#[command(name = "gogo", version, about)]
pub struct Cli {
    /// Verbose logging (repeatable: -v debug, -vv trace).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Errors only.
    #[arg(long, global = true)]
    pub quiet: bool,

    /// No progress bars (auto when stderr is not a terminal)
    #[arg(long, global = true)]
    pub no_progress: bool,

    /// Config file path override (GOGO_CONFIG also works).
    #[arg(long, global = true)]
    pub config: Option<std::path::PathBuf>,

    /// When to colorize output.
    #[arg(long, global = true, value_enum, default_value_t = ColorWhen::Auto)]
    pub color: ColorWhen,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorWhen {
    Auto,
    Always,
    Never,
}

/// The eleven commands (`docs/design/03-cli.md` §1).
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Sign in to GOG: open the login page in a browser, then paste back
    /// the code it ends on.
    Login {
        /// Non-interactive: paste the code directly (secrets never printed).
        #[arg(long)]
        code: Option<String>,
        /// Only print the auth URL and exit.
        #[arg(long)]
        print_url: bool,
        /// Re-login over an existing session.
        #[arg(long)]
        force: bool,
    },
    /// Delete stored tokens.
    Logout {
        /// Clear keyring + file stores.
        #[arg(long)]
        all_stores: bool,
        /// Also remove state.json and the catalog cache.
        #[arg(long)]
        purge_state: bool,
    },
    /// Show the signed-in account.
    Whoami {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// List owned games.
    List {
        /// Title substring filter.
        #[arg(long)]
        search: Option<String>,
        /// OS filter.
        #[arg(long, value_enum)]
        os: Option<OsFilter>,
        /// Include DLC rows.
        #[arg(long)]
        with_dlcs: bool,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
        /// Bypass the catalog cache.
        #[arg(long)]
        refresh: bool,
    },
    /// Search what you own: games, DLC, packs and goodies.
    Search(SearchArgs),
    /// Show one game's full catalog entry.
    Info {
        /// Game id, slug, or fuzzy title.
        game: String,
        /// Machine-readable output (raw cached product JSON; no secrets in it).
        #[arg(long)]
        json: bool,
    },
    /// Download offline installers (the main event).
    Download(DownloadArgs),

    /// Detect and download updates.
    Update {
        #[arg(value_name = "GAME")]
        games: Vec<String>,
        #[arg(long)]
        all: bool,
        /// Also treat a changed build id or timestamp in GOG's checksum file
        /// as an update.
        #[arg(long)]
        strict: bool,
    },
    /// Re-verify downloaded files against their checksum docs.
    Verify {
        #[arg(value_name = "GAME")]
        games: Vec<String>,
        #[arg(long)]
        all: bool,
        /// Verify these files directly instead of by game.
        #[arg(long, value_name = "FILE", num_args = 1..)]
        path: Vec<std::path::PathBuf>,
        /// Re-download corrupt files.
        #[arg(long)]
        fix: bool,
        /// Scan the download directory and record every file that verifies
        /// (for files downloaded elsewhere or records that were lost).
        #[arg(long)]
        rebuild: bool,
    },
    /// Download a game's files straight from GOG's content servers (its
    /// depot build, as GOG Galaxy does) into the user cache directory.
    Depot(DepotArgs),
    /// Config file management.
    Config {
        #[command(subcommand)]
        sub: ConfigSub,
    },
    #[cfg(target_os = "macos")]
    /// Build a macOS .app that runs a Windows game in Wine: from its depot
    /// build by default, else from its offline installer.
    #[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
    MacApp {
        #[command(subcommand)]
        sub: Option<MacAppSub>,
        /// A game (id, slug or part of the title) — built from its depot
        /// build, or from its downloaded installer when it has none — or the
        /// path to a GOG setup .exe (.bin siblings are picked up
        /// automatically) or an installed game directory.
        #[arg(
            value_name = "GAME|INSTALLER",
            required_unless_present_any = ["from_depot", "retarget"],
            conflicts_with_all = ["from_depot", "retarget"]
        )]
        input: Option<String>,
        /// Point an existing game at another runtime or renderer without
        /// rebuilding: writes `--runtime` / `--renderer` into the game's user
        /// override file, then sets the game up on
        /// it (installing the runtime, `wineboot -u` for a new engine). The
        /// signed bundle is untouched. `auto` drops the override.
        #[arg(long, value_name = "BUNDLE|GOGID", conflicts_with = "from_depot")]
        retarget: Option<String>,
        /// Like the positional GAME, but only from the depot build, never
        /// the installer; also takes an installed game directory.
        #[arg(long, value_name = "GAME|PATH")]
        from_depot: Option<String>,
        /// Where the game goes: `<PATH>/<gogid>/` (`library.dir`,
        /// `GOGO_LIBRARY_DIR`). Default: gogo's data dir. A game that's
        /// already built stays where it is.
        #[arg(long, value_name = "PATH", conflicts_with_all = ["portable", "retarget"])]
        library: Option<std::path::PathBuf>,
        /// Bundle title (default: the game's name, or derived from the
        /// installer file name).
        #[arg(long)]
        name: Option<String>,
        /// Wine runtime to use. Default: the one set for the game in your
        /// override file or gogo's compat list, else the default runtime.
        /// Without a value, pick it from a menu.
        #[arg(long, value_enum, num_args = 0..=1, value_name = "RUNTIME")]
        runtime: Option<Option<RuntimeSource>>,
        /// Runtime directory for --runtime gptk|path.
        #[arg(long, value_name = "DIR")]
        runtime_path: Option<std::path::PathBuf>,
        /// Pin a catalog release tag (`--runtime winecx|winecx26|staging|devel|stable`).
        #[arg(long, value_name = "TAG", requires = "runtime")]
        runtime_version: Option<String>,
        /// Output directory for <name>.app (default: current directory).
        #[arg(long, value_name = "DIR")]
        out: Option<std::path::PathBuf>,
        /// Lutris recipe: none (default), auto (live search: a picker on a
        /// terminal, the ranked best otherwise), a Lutris installer slug, or
        /// a downloaded script JSON path. Without a value: auto.
        #[arg(long, num_args = 0..=1, default_missing_value = "auto", value_name = "RECIPE")]
        recipe: Option<String>,
        /// Ask for the recipe and the runtime in menus (a terminal only;
        /// flags with a value still win). Same as bare `--recipe --runtime`.
        #[arg(long, conflicts_with = "retarget")]
        interactive: bool,
        /// Renderer: `auto` (default) picks it from the graphics API the
        /// game uses (D3D10/11 → dxmt, D3D9 and older → wined3d, 2D
        /// DirectDraw → cnc-ddraw) unless gogo's compat list or your
        /// override file says otherwise.
        #[arg(long, value_enum)]
        renderer: Option<RendererArg>,
        /// Alias for `--renderer cnc-ddraw`.
        #[arg(long, conflicts_with_all = ["no_ddraw_wrapper", "renderer"])]
        ddraw_wrapper: bool,
        /// Like `--renderer auto`, but never cnc-ddraw (wined3d instead).
        #[arg(long, conflicts_with = "renderer")]
        no_ddraw_wrapper: bool,
        /// Self-contained bundle for another Mac: the runtime, the
        /// renderer, the verbs' files and the game go inside the .app, and
        /// its launcher creates the prefix on the Mac it runs on (no gogo
        /// needed there). Can't be retargeted in place; D3DMetal and the GPTK
        /// are refused (Apple's licence).
        #[arg(long, conflicts_with = "retarget")]
        portable: bool,
        /// Skip the size confirmation; overwrite an existing bundle.
        #[arg(long)]
        yes: bool,
        /// Progress events as NDJSON on stdout.
        #[arg(long)]
        json_progress: bool,
        /// Machine-readable summary.
        #[arg(long)]
        json: bool,
    },
    #[cfg(target_os = "linux")]
    /// Set up a Windows game to run through GE-Proton and umu-launcher, as
    /// `games/<gogid>/` in gogo's data dir: from its depot build by default,
    /// else from its offline installer (run under umu).
    #[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
    LinuxApp {
        #[command(subcommand)]
        sub: Option<LinuxAppSub>,
        /// A game (id, slug or part of the title) — built from its depot
        /// build, or from its downloaded installer when it has none — or the
        /// path to a GOG setup .exe (.bin siblings are picked up
        /// automatically) or an installed game directory.
        #[arg(
            value_name = "GAME|INSTALLER",
            required_unless_present_any = ["from_depot", "retarget"],
            conflicts_with_all = ["from_depot", "retarget"]
        )]
        input: Option<String>,
        /// Point a built game at another Proton build or renderer without
        /// rebuilding: writes `--runtime` / `--renderer` into the game's
        /// override file (`[platforms.linux]`), then sets the game up on
        /// them (installing a missing GE-Proton). `auto` drops the override.
        #[arg(long, value_name = "GOGID", conflicts_with = "from_depot")]
        retarget: Option<String>,
        /// Like the positional GAME, but only from the depot build, never
        /// the installer; also takes an installed game directory.
        #[arg(long, value_name = "GAME|PATH")]
        from_depot: Option<String>,
        /// Where the game goes: `<PATH>/<gogid>/` (`library.dir`,
        /// `GOGO_LIBRARY_DIR`). Default: gogo's data dir. A game that's
        /// already built stays where it is.
        #[arg(long, value_name = "PATH", conflicts_with = "retarget")]
        library: Option<std::path::PathBuf>,
        /// The game's title (default: the game's name, or derived from the
        /// installer file name).
        #[arg(long)]
        name: Option<String>,
        /// Proton build: `ge-proton[/<version>]` or `path:<dir>`. Default:
        /// the one set for the game in your override file or gogo's compat
        /// list, else `[linux] default_runtime`, else gogo's pinned
        /// GE-Proton. Without a value, pick it from a menu.
        #[arg(long, num_args = 0..=1, value_name = "RUNTIME")]
        runtime: Option<Option<String>>,
        /// Renderer: `auto` (default) picks it from the graphics API the
        /// game uses (D3D12 → vkd3d-proton, D3D8–11 → dxvk, D3D ≤ 7 → d7vk,
        /// 2D DirectDraw → cnc-ddraw) unless gogo's compat list or your
        /// override file says otherwise.
        #[arg(long, value_enum)]
        renderer: Option<LinuxRendererArg>,
        /// Re-fetch umu-database's CSV for the umu id (only used when the
        /// Proton build has no copy of it).
        #[arg(long)]
        refresh: bool,
        /// Pick the runtime from a menu (a terminal only; a `--runtime`
        /// value still wins). Same as a bare `--runtime`.
        #[arg(long, conflicts_with = "retarget")]
        interactive: bool,
        /// Skip the confirmation; rebuild an existing game's files (its
        /// prefix and saves stay).
        #[arg(long)]
        yes: bool,
        /// Include this DLC (product id or title; repeatable). It must be
        /// owned. A rebuild with `--dlc` has exactly these.
        #[arg(
            long = "dlc",
            value_name = "ID|TITLE",
            conflicts_with_all = ["all_dlcs", "no_dlcs", "retarget"]
        )]
        dlc: Vec<String>,
        /// Include every DLC you own.
        #[arg(long, conflicts_with_all = ["no_dlcs", "retarget"])]
        all_dlcs: bool,
        /// Include no DLC (a rebuild drops the ones it had).
        #[arg(long, conflicts_with = "retarget")]
        no_dlcs: bool,
        /// Content language of the depot download (default: the game's
        /// recorded one, else en-US). Checked against the build's languages
        /// (`gogo depot <game> --list-languages`); `de` is taken for `de-DE`
        /// when it names one language.
        #[arg(long, value_name = "CODE", conflicts_with = "retarget")]
        lang: Option<String>,
        /// Progress events as NDJSON on stdout.
        #[arg(long)]
        json_progress: bool,
        /// Machine-readable summary.
        #[arg(long)]
        json: bool,
    },
    /// Manage Wine runtimes: list the catalog and what's cached, install,
    /// prune.
    Runtimes(RuntimesArgs),
    /// Manage renderers (cnc-ddraw, DXMT, D3DMetal): list the catalog and
    /// what's cached, install, prune.
    Renderers(RenderersArgs),
    /// Manage the games gogo set up (`mac-app`, `linux-app`): each one's
    /// Wine prefix (saves included), game files and launch logs. List them,
    /// show where they live, remove them.
    Apps(AppsArgs),
    /// Inspect Lutris community recipes for a game: list the
    /// ranked candidates, or show one recipe's execution plan.
    Recipes(RecipesArgs),
    /// Internal: archive-extractor stand-ins that winetricks calls.
    /// Double-underscore name keeps it out of help and completion output.
    #[command(hide = true, name = "__shim")]
    Shim {
        tool: String,
        /// Flags pass through verbatim (`-d`, `-F`, `-o…`) — clap must not
        /// intercept them.
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Debug: PE import scan of a game tree — bitness, graphics API, helper
    /// DLLs. Input for renderer selection; changes nothing.
    #[command(hide = true, name = "__pe-scan")]
    PeScan {
        /// Game install / depot tree.
        #[arg(value_name = "DIR")]
        dir: std::path::PathBuf,
        /// Main exe (default: `goggame-*.info` primary task, else a heuristic).
        #[arg(long, value_name = "EXE")]
        exe: Option<std::path::PathBuf>,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Print a shell completion script to stdout.
    Completions {
        /// Shell to generate the script for.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[cfg(target_os = "linux")]
/// `gogo linux-app <sub>`: per-game commands next to the build itself.
#[derive(Debug, Subcommand)]
pub enum LinuxAppSub {
    /// Bring a game's dir up to date with its gogo-launch.toml and override
    /// file: resolve the Proton build (installing a missing GE-Proton),
    /// create a missing prefix with no links out of the game's dir, apply
    /// registry values, verbs and the renderer, write launch.conf. The
    /// launcher runs this when the game's config has changed.
    Setup {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        /// Re-fetch umu-database's CSV (see `linux-app --refresh`).
        #[arg(long)]
        refresh: bool,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Print a game's effective compat config on Linux and where each
    /// value comes from (the build's automatic verbs, gogo's compat list,
    /// your override file), with the protonfix summary when the game's
    /// Proton build is installed.
    Compat {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Can this game work on this machine? Checks the launcher files and
    /// menu entry, umu and its Python, user namespaces, the Proton build,
    /// the Steam Linux Runtime, the renderer's files, Vulkan drivers (32-bit
    /// too, for a 32-bit game), the prefix stamp and links, processes left
    /// running, and the last launch's log; one line per finding (ok / warn /
    /// fail) with a fix hint. Read-only: never launches the game, runs umu
    /// or stops a process. Exits 1 when a check fails (warnings don't).
    Doctor {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Move a built game to `<DIR>/<gogid>/`, or back to gogo's data dir with
    /// `--default`, keeping its Wine prefix and saves. A rename on one
    /// filesystem. Across filesystems, a copy compared byte for byte before
    /// the original goes; stopped at any point, the game still runs from where
    /// it was, and the next move cleans up. Refused while the game is running.
    /// With `--adopt <DIR>`, nothing moves: the game's dir is already somewhere
    /// else (a drive mounted at a new path, a move by hand), and gogo's
    /// record, `launch.conf` and menu entry are pointed at it. `<DIR>` is the
    /// game's own dir, the one ending in its id; refused while the old place is
    /// still there.
    Move {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        /// The library to move it into, as for `--library`.
        #[arg(
            value_name = "DIR",
            required_unless_present_any = ["default", "adopt"],
            conflicts_with_all = ["default", "adopt"]
        )]
        dir: Option<std::path::PathBuf>,
        /// Back to `games/` in gogo's data dir.
        #[arg(long, conflicts_with = "adopt")]
        default: bool,
        /// The game is already at this dir (ending in its id): record that, and
        /// move nothing.
        #[arg(long, value_name = "DIR")]
        adopt: Option<std::path::PathBuf>,
        /// Print what would happen, move nothing.
        #[arg(long)]
        dry_run: bool,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// A built game's DLC: list them, or add DLC without a rebuild (the
    /// depot lane). The DLC is fetched for the game's installed build and
    /// language, then laid onto its files under a journal; the prefix,
    /// saves and settings stay. An add that stops while laying files is
    /// finished (or undone) by the next `dlc add`; until then the game
    /// won't start.
    Dlc {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        #[command(subcommand)]
        cmd: LinuxDlcCmd,
    },
    /// A built game's executables: the play tasks of its `goggame-*.info`
    /// files (base game and DLC), numbered, with their name, path (relative
    /// to the game's files), args, working dir, category, which one is
    /// GOG's primary and which one gogo's launcher runs. `--all` adds every
    /// other `.exe` in the game's files. 32/64-bit from each PE header. Reads
    /// only; `linux-app run` starts one.
    Exes {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        /// Also list every `.exe` in the game's files that isn't a play task.
        #[arg(long)]
        all: bool,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Run one of a game's executables once (a vendor launcher, a settings
    /// or mod tool) in the game's prefix, through its launcher: the same
    /// Proton build, umu container, environment, display mode and HDR,
    /// renderer and wrapper, run log, close request on TERM and teardown as
    /// the game, and setup first when its config changed. Nothing of the
    /// game's config changes: the menu entry still starts the game. The run
    /// log is `one-off-<time>-<pid>.log` beside the game's. Refused while the
    /// game (or another run) is running, or while a DLC add or a setup is
    /// pending.
    Run {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        /// Which exe: a play task's number or name (as `linux-app exes`
        /// lists them; a name two tasks share is refused), or a path relative
        /// to the game's files (`REDprelauncher.exe`,
        /// `tools/redmod/bin/redMod.exe`; `\` and any case work too).
        #[arg(value_name = "TASK|PATH")]
        exe: String,
        /// Args after the play task's own, after `--`. An exe that isn't a
        /// play task gets only these. It runs in the task's working dir,
        /// else the exe's dir.
        #[arg(last = true, value_name = "ARGS", allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Switch a game to the next renderer in its order (the launcher's
    /// "Try next renderer"): the one after what setup last applied,
    /// written to the override file as `renderer = "<id>"`. Prints the new
    /// renderer; exits 1 with "no more renderers" when the order is used
    /// up.
    NextRenderer {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        /// Print the next renderer, write nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// A game's environment variables: list what it gets at launch and
    /// where each value comes from, or set and unset your own (kept in the
    /// game's override file, `[platforms.linux.env]`; the next launch
    /// applies them, no rebuild needed).
    Env {
        #[command(subcommand)]
        cmd: LinuxEnvCmd,
    },
    /// A game's launch args: show them, set the args added after the
    /// game's own (`-- <args…>`, kept in the override file as
    /// `extra_args`; the next launch applies them), or `--clear` them.
    Args {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        /// Remove your extra args.
        #[arg(long, conflicts_with = "args")]
        clear: bool,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
        /// The args to add after the game's own, after `--`
        /// (`-- --launcher-skip -skipStartScreen`). They replace the ones
        /// set before.
        #[arg(last = true, value_name = "ARGS", allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// The command a game runs under, like gamescope or gamemoderun: show
    /// it, set it (`-- <program> <words…>`, kept in the override file as
    /// `wrapper`; the next launch applies it), or `--clear` it.
    Wrapper {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        /// Run the game without your wrapper.
        #[arg(long, conflicts_with = "words")]
        clear: bool,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
        /// The wrapper, after `--`, one word per argument with no shell in
        /// between; the game's command follows its last word
        /// (`-- gamescope -f --`, `-- gamemoderun`). It replaces the one set
        /// before.
        #[arg(last = true, value_name = "WORDS", allow_hyphen_values = true)]
        words: Vec<String>,
    },
    /// Which of Wine's display drivers a game gets: show it, set it
    /// (`wayland`: Wine's Wayland driver, the way to HDR; `x11`: X11, through
    /// Xwayland on a Wayland desktop), or `--clear` it. Kept in the override
    /// file as `display` and `hdr`; the next launch applies them. Without a
    /// Wayland socket at launch, Wayland mode runs the game on X11 and logs why.
    /// `wayland` on a 32-bit game on wined3d (OpenGL) warns that it usually
    /// fails or draws unscaled there and still applies, and suggests `x11`;
    /// DXVK, D7VK and VKD3D-Proton (Vulkan), cnc-ddraw's GDI games and 64-bit
    /// games don't warn.
    Display {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        /// `wayland` or `x11`.
        #[arg(value_name = "MODE", value_parser = ["wayland", "x11"])]
        mode: Option<String>,
        /// HDR in Wayland mode: `auto` (Proton detects it), `on`
        /// (PROTON_ENABLE_HDR=1), `force` (PROTON_FORCE_HDR=1: also on
        /// outputs that report SDR, like Hyprland's cm_auto_hdr) or `off`.
        #[arg(long, value_name = "HDR", value_parser = ["auto", "on", "force", "off"])]
        hdr: Option<String>,
        /// The output Wine's Wayland driver treats as primary, such as `DP-1`
        /// (GE-Proton's PROTON_WAYLAND_MONITOR): the monitor the game opens
        /// on. Without it GE-Proton guesses with xrandr. Wayland mode only.
        #[arg(long, value_name = "OUTPUT", conflicts_with = "no_monitor")]
        monitor: Option<String>,
        /// Drop only the monitor: GE-Proton guesses again.
        #[arg(long, conflicts_with = "clear")]
        no_monitor: bool,
        /// Drop the mode, the HDR switch and the monitor: the environment
        /// decides again.
        #[arg(long, conflicts_with_all = ["mode", "hdr", "monitor"])]
        clear: bool,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
}

#[cfg(target_os = "linux")]
/// `gogo linux-app dlc <gogid> <cmd>` (design 19 §1.3, §1.4).
#[derive(Debug, Subcommand)]
pub enum LinuxDlcCmd {
    /// Every DLC of the game: whether you own it, whether it's installed,
    /// and its download and disk size for the installed build ("not in
    /// this build" when that build can't take it; 0 for a DLC with no
    /// files). Reads only.
    List {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Add DLC to the built game: for its installed build and language,
    /// into the depot cache when it holds that build (else a staging dir in
    /// the game's dir), then onto the game's files. Refused when GOG no
    /// longer serves the installed build (update the game with a rebuild).
    /// With no name and no `--all`, a picker on a terminal. Run it again
    /// after a stop: it resumes, or finishes or undoes a half-laid add.
    Add {
        /// DLC by product id or title (several allowed); each must be owned.
        #[arg(value_name = "ID|TITLE", conflicts_with = "all")]
        names: Vec<String>,
        /// Every owned DLC of the build that isn't installed.
        #[arg(long)]
        all: bool,
        /// Print the plan, change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation.
        #[arg(long)]
        yes: bool,
        /// Global rate limit, e.g. 4MiB/s.
        #[arg(long)]
        limit: Option<String>,
        /// Skip the free-space preflight.
        #[arg(long)]
        no_disk_check: bool,
        /// Progress events as NDJSON on stdout.
        #[arg(long)]
        json_progress: bool,
        /// Machine-readable summary.
        #[arg(long)]
        json: bool,
    },
}

#[cfg(target_os = "linux")]
/// `gogo linux-app env <cmd>`.
#[derive(Debug, Subcommand)]
pub enum LinuxEnvCmd {
    /// Every variable the game gets at its next launch, with its value and
    /// where it comes from (gogo's default, the renderer, gogo's compat
    /// list, your override, the launcher, the protonfix), and the values
    /// they beat. Values of names that look like secrets (TOKEN, SECRET,
    /// PASSWORD, API_KEY, …) are hidden unless `--reveal`.
    List {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        /// Show hidden values.
        #[arg(long)]
        reveal: bool,
        /// Every switch this game's Proton build has, what each does, its
        /// default, whether it applies to this game's renderer and GPU, and
        /// what sets it now. Nothing is left out for not applying.
        #[arg(long)]
        all: bool,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// One switch in full: what it does on this game's Proton build, its
    /// default, whether it applies and why, what sets it now, how to set it.
    Describe {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        /// An env var, a `VKD3D_CONFIG` flag or a `dxvk.conf` key; case
        /// doesn't matter.
        #[arg(value_name = "NAME")]
        name: String,
        /// Show hidden values.
        #[arg(long)]
        reveal: bool,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Set variables for the game. Refuses the launcher's own
    /// (WINEPREFIX, GAMEID, STORE, PROTONPATH) and the ones with their own
    /// setting (WINEDLLOVERRIDES, PROTONFIXES_DISABLE); warns when yours
    /// beats a renderer switch or gogo's default.
    Set {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        /// `KEY=VALUE`, one or more (quote values with spaces in your shell).
        #[arg(value_name = "KEY=VALUE", required = true)]
        pairs: Vec<String>,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Remove variables you set (others' values stay).
    Unset {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        /// `KEY`, one or more.
        #[arg(value_name = "KEY", required = true)]
        keys: Vec<String>,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
}

#[cfg(target_os = "linux")]
/// `linux-app --renderer` values (design 18 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum LinuxRendererArg {
    /// Picked from the game's graphics API, unless gogo's compat list or
    /// your override file says otherwise.
    Auto,
    /// Proton's D3D8–11 on Vulkan.
    Dxvk,
    /// Proton's D3D12 on Vulkan.
    #[value(name = "vkd3d-proton")]
    Vkd3dProton,
    /// Proton's D3D ≤ 7 on Vulkan (`PROTON_USE_D7VK=1`).
    D7vk,
    /// DirectDraw wrapper for 2D classics.
    #[value(name = "cnc-ddraw")]
    CncDdraw,
    /// Wine's Direct3D on OpenGL (`PROTON_USE_WINED3D=1`).
    Wined3d,
}

#[cfg(target_os = "linux")]
impl LinuxRendererArg {
    pub fn choice(self) -> gogo::macapp::RendererChoice {
        use gogo::macapp::{Renderer as R, RendererChoice as C};
        match self {
            LinuxRendererArg::Auto => C::Auto,
            LinuxRendererArg::Dxvk => C::Force(R::Dxvk),
            LinuxRendererArg::Vkd3dProton => C::Force(R::Vkd3dProton),
            LinuxRendererArg::D7vk => C::Force(R::D7vk),
            LinuxRendererArg::CncDdraw => C::Force(R::CncDdraw),
            LinuxRendererArg::Wined3d => C::Force(R::Wined3d),
        }
    }
}

#[cfg(target_os = "macos")]
/// `gogo mac-app <sub>`: per-game commands next to the build itself.
#[derive(Debug, Subcommand)]
pub enum MacAppSub {
    /// Print a game's effective compat config and where each value comes
    /// from (built-in defaults, gogo's compat list, your override file).
    Compat {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Bring a game's state dir up to date with its bundle and override
    /// file: resolve the runtime (installing a missing catalog runtime),
    /// create a missing prefix with no links out of the state dir, apply
    /// registry values and verbs, write the effective launch config. The
    /// launcher runs this when the game's config has changed.
    Setup {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        /// The .app to read (default: the one recorded at build).
        #[arg(long, value_name = "APP")]
        bundle: Option<std::path::PathBuf>,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Can this game work on this Mac? Checks the bundle's signature,
    /// the effective runtime and renderer files, the prefix stamp and links,
    /// Rosetta, Wine processes left running, and the last launch's log; one
    /// line per finding (ok / warn / fail) with a fix hint. Never launches
    /// the game. Exits 1 when a check fails (warnings don't).
    Doctor {
        /// GOG product id, or the game's .app.
        #[arg(value_name = "BUNDLE|GOGID")]
        target: String,
        /// Stop Wine processes left running in the game's prefix with no
        /// launcher (the launcher's own teardown: `wineserver -k`, then
        /// SIGKILL). Changes nothing else.
        #[arg(long)]
        stop_leftovers: bool,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Switch a game to the next renderer in its order (the launcher's
    /// "Try next renderer"): the one after what setup last applied,
    /// written to the override file as `renderer = "<ref>"`. Prints the new
    /// ref; exits 1 with "no more renderers" when the order is used up.
    NextRenderer {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        /// Print the next renderer, write nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, clap::Args)]
pub struct DepotArgs {
    /// One game by id, slug, or fuzzy title.
    #[arg(value_name = "GAME")]
    pub game: String,
    /// Content language code (default en-US). Checked against the build's
    /// languages (`--list-languages`); a short form such as `de` is taken when it
    /// names one language.
    #[arg(long, value_name = "CODE")]
    pub language: Option<String>,
    /// List the languages the newest build offers (code, name, DLC with fewer),
    /// and stop: nothing is downloaded or hashed. With `--json`, as JSON.
    #[arg(long, conflicts_with_all = ["dlc", "all_dlcs", "dry_run", "json_progress"])]
    pub list_languages: bool,
    /// Include this DLC (product id or title; repeatable). It must be owned.
    #[arg(long = "dlc", value_name = "ID|TITLE")]
    pub dlc: Vec<String>,
    /// Include every DLC you own (`--with-dlcs` is the old name).
    #[arg(long, alias = "with-dlcs", conflicts_with = "dlc")]
    pub all_dlcs: bool,
    /// Password for password-protected builds.
    #[arg(long)]
    pub password: Option<String>,
    /// Install root override (default: the user cache directory).
    #[arg(long, value_name = "DIR")]
    pub dir: Option<std::path::PathBuf>,
    /// Global rate limit, e.g. 4MiB/s (same syntax as the download.limit
    /// config key).
    #[arg(long)]
    pub limit: Option<String>,
    /// Skip the free-space preflight.
    #[arg(long)]
    pub no_disk_check: bool,
    /// Resolve and print the plan, touch nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Progress events as NDJSON on stdout.
    #[arg(long)]
    pub json_progress: bool,
    /// Machine-readable summary.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub enum ConfigSub {
    /// Print the config file location (creating nothing).
    Path,
    /// Read a key.
    Get { key: String },
    /// Write a key.
    Set { key: String, value: String },
    /// Open $EDITOR on the file.
    Edit,
}

/// Field bag for [`Command::Search`] (design 17 §3).
#[derive(Debug, clap::Args)]
pub struct SearchArgs {
    /// Words to match (all of them, any order); a lone number also matches ids.
    #[arg(value_name = "QUERY", required = true)]
    pub query: Vec<String>,
    /// Kinds to show (repeatable or comma list).
    #[arg(short, long, value_enum, value_delimiter = ',')]
    pub category: Vec<SearchCategory>,
    /// Keep only goodies of this type (`audio`, …; implies `-c goodies`).
    #[arg(long, value_parser = parse_goodies_type)]
    pub goodies_type: Option<String>,
    /// Keep games and DLC with an installer for this OS.
    #[arg(long, value_enum)]
    pub os: Option<OsFilter>,
    /// Hide DLC you don't own.
    #[arg(long)]
    pub owned: bool,
    /// Bypass the catalog cache.
    #[arg(long)]
    pub refresh: bool,
    /// Machine-readable output.
    #[arg(long)]
    pub json: bool,
}

/// `search --category` values (design 17 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SearchCategory {
    Games,
    Dlc,
    Packs,
    Goodies,
}

/// GOG's `bonus_content.type` vocabulary is lowercase words (`audio`, …).
fn parse_goodies_type(s: &str) -> Result<String, String> {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        Ok(s.to_owned())
    } else {
        Err("expected a lowercase type such as `audio`".into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OsFilter {
    Windows,
    Mac,
    Linux,
    All,
}
/// `--renderer` values (#70).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum RendererArg {
    /// Picked from the game's graphics API, unless gogo's compat list or
    /// your override file says otherwise.
    Auto,
    /// Wine's own Direct3D-on-OpenGL.
    Wined3d,
    /// D3D10/11 on Metal (needs a CrossOver-source runtime, winecx26 or winecx,
    /// and a Metal 3 GPU).
    Dxmt,
    /// DirectDraw wrapper for 2D classics.
    #[value(name = "cnc-ddraw")]
    CncDdraw,
    /// Apple's D3D11/12 translator, 64-bit games only; downloaded on consent
    /// (asked once on a terminal, kept as `[mac_app] d3dmetal` in the config).
    D3dmetal,
}

impl RendererArg {
    /// `None` for `auto`.
    #[cfg(target_os = "macos")]
    pub fn renderer(self) -> Option<gogo::macapp::Renderer> {
        use gogo::macapp::Renderer as R;
        match self {
            RendererArg::Auto => None,
            RendererArg::Wined3d => Some(R::Wined3d),
            RendererArg::Dxmt => Some(R::Dxmt),
            RendererArg::CncDdraw => Some(R::CncDdraw),
            RendererArg::D3dmetal => Some(R::D3dMetal),
        }
    }
}

/// CLI mirror of [`gogo::macapp::RuntimeMode`] (foreign enum — clap's
/// `ValueEnum` can't be implemented on it here).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum RuntimeSource {
    /// The recipe's or a cached runtime, else download the default one;
    /// a Homebrew or GPTK wine only when asked for.
    Auto,
    /// Homebrew wine-crossover.
    Brew,
    /// Cached/downloaded CrossOver-24-source engine `winecx` with its
    /// Frameworks/ (the previous default, to roll back to).
    Winecx,
    /// Cached/downloaded CrossOver-26.3-source engine `winecx26` (Highball
    /// build, Wine 11.0) with its Frameworks/: the default.
    Winecx26,
    /// Cached/downloaded wine-staging.
    #[value(alias = "wine-staging")]
    Staging,
    /// Cached/downloaded wine-devel.
    #[value(alias = "wine-devel")]
    Devel,
    /// Cached/downloaded wine-stable.
    #[value(alias = "wine-stable")]
    Stable,
    /// User-supplied GPTK mount (user accepted Apple's terms).
    Gptk,
    /// Explicit directory containing bin/wine.
    Path,
}

impl From<RuntimeSource> for gogo::macapp::RuntimeMode {
    fn from(s: RuntimeSource) -> Self {
        match s {
            RuntimeSource::Auto => gogo::macapp::RuntimeMode::Auto,
            RuntimeSource::Brew => gogo::macapp::RuntimeMode::Brew,
            RuntimeSource::Winecx => gogo::macapp::RuntimeMode::Catalog("winecx".into()),
            RuntimeSource::Winecx26 => gogo::macapp::RuntimeMode::Catalog("winecx26".into()),
            RuntimeSource::Staging => gogo::macapp::RuntimeMode::Catalog("wine-staging".into()),
            RuntimeSource::Devel => gogo::macapp::RuntimeMode::Catalog("wine-devel".into()),
            RuntimeSource::Stable => gogo::macapp::RuntimeMode::Catalog("wine-stable".into()),
            RuntimeSource::Gptk => gogo::macapp::RuntimeMode::Gptk,
            RuntimeSource::Path => gogo::macapp::RuntimeMode::Path,
        }
    }
}

/// Field bag for [`Command::Runtimes`] (design 12 §10).
#[derive(Debug, clap::Args)]
pub struct RuntimesArgs {
    #[command(subcommand)]
    pub cmd: StoreCmd,
}

/// Field bag for [`Command::Renderers`]: the same verbs over the renderer
/// entries of the catalog and store.
#[derive(Debug, clap::Args)]
pub struct RenderersArgs {
    #[command(subcommand)]
    pub cmd: StoreCmd,
}

/// `list` / `install` / `prune`, shared by `runtimes` and `renderers`.
#[derive(Debug, clap::Subcommand)]
pub enum StoreCmd {
    /// List the catalog (latest versions checked online) with what's cached
    /// and which games use it.
    List {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
        /// Don't check for the latest versions online.
        #[arg(long)]
        offline: bool,
        /// Also list the entries for other OSes (not installable here).
        #[arg(long)]
        all: bool,
    },
    /// Download, verify, and cache a catalog entry for this OS.
    Install {
        /// Catalog id (see `list`).
        #[arg(value_name = "ID")]
        id: String,
        /// Pin a release tag (default: latest).
        #[arg(long)]
        version: Option<String>,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Remove cached versions (never one a game still uses).
    #[command(group = clap::ArgGroup::new("what").required(true).args(["unused", "id"]))]
    Prune {
        /// Remove every cached version no game uses. An id a game uses keeps
        /// its newest two (the current one and the one before, to roll back
        /// to); the default runtime and winetricks keep their newest.
        #[arg(long, alias = "all")]
        unused: bool,
        /// Remove one id (every version), or one version as `<id>/<version>`,
        /// with the runtimes built on it.
        #[arg(value_name = "ID[/VERSION]")]
        id: Option<String>,
    },
}

/// Field bag for [`Command::Apps`].
#[derive(Debug, clap::Args)]
pub struct AppsArgs {
    #[command(subcommand)]
    pub cmd: AppsCmd,
}

#[derive(Debug, clap::Subcommand)]
pub enum AppsCmd {
    /// List every game set up here: its .app on macOS or its menu entry on
    /// Linux (and whether it is still there), runtime, renderer, disk use
    /// and last launch. Disk use counts the game files in full, though a
    /// depot game's may share space with the depot cache. Sorted by title
    /// unless --sort says otherwise.
    List {
        /// Comma-separated columns to sort by, each ascending; a `-` prefix
        /// sorts that column descending. Names are the column headers,
        /// case-insensitive, with `-` or `_` for a space: gogid, title,
        /// menu-entry, runtime, renderer, dlc, size, last-launch. GOGID, DLC
        /// and SIZE sort numerically, LAST LAUNCH by time (never launched
        /// first). Ties fall back to the GOGID.
        #[arg(long, value_name = "COLS", allow_hyphen_values = true)]
        sort: Option<String>,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Show where one game's things live: state folder, Wine prefix,
    /// launch logs, override file, .app or menu entry.
    Show {
        /// GOG product id, or the game's .app.
        #[arg(value_name = "BUNDLE|GOGID")]
        target: String,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Delete one game's state folder (Wine prefix and saves included),
    /// its launch logs and its override file, and on Linux its menu entry.
    /// A .app stays, but won't start again until rebuilt. Refuses while
    /// the game is running.
    Remove {
        /// GOG product id, or the game's .app.
        #[arg(value_name = "BUNDLE|GOGID")]
        target: String,
        /// Keep the override file (`gogo mac-app compat` / `linux-app
        /// compat` show it).
        #[arg(long)]
        keep_override: bool,
        /// Don't ask first.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Remove every orphaned game: on macOS, one whose .app is no longer
    /// where it was built or last launched from (a moved .app updates that
    /// on its next launch, so check the list before saying yes); on Linux,
    /// one whose menu entry was deleted.
    Prune {
        /// Games whose .app or menu entry is missing (the only kind for now).
        #[arg(long, required = true)]
        orphaned: bool,
        /// Keep their override files.
        #[arg(long)]
        keep_override: bool,
        /// Don't ask first.
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

/// Field bag for [`Command::Recipes`] (design 11 §1, carried debt resolved
/// in design 12 §12 phase 5).
#[derive(Debug, clap::Args)]
pub struct RecipesArgs {
    #[command(subcommand)]
    pub cmd: RecipesCmd,
}

#[derive(Debug, clap::Subcommand)]
pub enum RecipesCmd {
    /// List the ranked Lutris installers for a game (searched by installer
    /// filename or --name).
    List {
        /// GOG offline installer path; its derived name seeds the search.
        #[arg(value_name = "INSTALLER")]
        installer: Option<String>,
        /// Search by game name instead of the installer filename.
        #[arg(long, conflicts_with = "installer")]
        name: Option<String>,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
        /// Lutris API base override (tests, mirrors).
        #[arg(long, hide = true)]
        lutris_base: Option<String>,
    },
    /// Show a recipe's execution plan: applied steps vs skipped, with
    /// reasons. Nothing is executed.
    Show {
        /// Installer slug (live fetch) or a local script JSON file.
        #[arg(value_name = "SLUG|FILE")]
        recipe: String,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
        /// Lutris API base override (tests, mirrors).
        #[arg(long, hide = true)]
        lutris_base: Option<String>,
    },
}

/// Stable exit codes (`docs/design/03-cli.md` §5).
// dead_code: the table is a stable published contract — codes no command raises yet
// (DISK: engine reports disk-full as FAILED today) stay defined on purpose.
#[allow(dead_code)]
pub mod exit {
    pub const OK: i32 = 0;
    /// Generic failure.
    pub const FAILED: i32 = 1;
    /// Bad usage (clap normally handles this itself).
    pub const USAGE: i32 = 2;
    /// Auth required / unauthorized — hint: `gogo login`.
    pub const AUTH: i32 = 3;
    /// Network failure after retries.
    pub const NETWORK: i32 = 4;
    /// Corrupt download (checksum mismatch).
    pub const CORRUPT: i32 = 5;
    /// Insufficient disk space.
    pub const DISK: i32 = 6;
    /// mac-app conversion failure (runtime missing/invalid, installer exit ≠ 0,
    /// no entry point, codesign failure) — docs/design/10 §4.
    pub const MACAPP: i32 = 7;
    /// Depot (Model B) conversion/download failures share the conversion class
    /// with mac-app (design 14 §1).
    pub const DEPOT: i32 = 7;
    /// Interrupted (SIGINT); sidecars flushed, safe to resume.
    pub const SIGINT: i32 = 130;
}

/// Field bag for [`Command::Download`] (03 §3 content-selection + overrides).
#[derive(Debug, clap::Args)]
pub struct DownloadArgs {
    /// Game(s) by id, slug, or fuzzy title — or use --all.
    #[arg(value_name = "GAME")]
    pub games: Vec<String>,
    /// Every owned game (with --yes, else a confirmation prompt).
    #[arg(long)]
    pub all: bool,
    /// Skip the --all confirmation prompt.
    #[arg(long)]
    pub yes: bool,
    /// OS filter (default: host OS, falling back to windows).
    #[arg(long, value_enum)]
    pub os: Option<OsFilter>,
    /// Language filter.
    #[arg(long)]
    pub language: Option<String>,
    /// Include this DLC installer (product id or title; repeatable). It must be owned.
    #[arg(long = "dlc", value_name = "ID|TITLE")]
    pub dlc: Vec<String>,
    /// Include every DLC you own (`--with-dlcs` is the old name).
    #[arg(long, alias = "with-dlcs", conflicts_with = "dlc")]
    pub all_dlcs: bool,
    /// Include bonus content (extras).
    #[arg(long)]
    pub extras: bool,
    /// Include patches.
    #[arg(long)]
    pub patches: bool,
    /// Include language packs.
    #[arg(long)]
    pub lang_packs: bool,
    /// Download root override (download.dir).
    #[arg(long)]
    pub dir: Option<String>,
    /// Chunk workers per file (download.workers).
    #[arg(long)]
    pub workers: Option<u32>,
    /// Simultaneous files (download.parallel_files).
    #[arg(long, value_name = "N")]
    pub parallel_files: Option<u32>,
    /// Global rate limit, e.g. 4MiB/s (same syntax as the download.limit
    /// config key).
    #[arg(long)]
    pub limit: Option<String>,
    /// Redownload even if recorded as present.
    #[arg(long)]
    pub force: bool,
    /// Skip the free-space preflight.
    #[arg(long)]
    pub no_disk_check: bool,
    /// Progress events as NDJSON on stdout.
    #[arg(long)]
    pub json_progress: bool,
    /// Machine-readable summary.
    #[arg(long)]
    pub json: bool,
    /// Plan and print, touch nothing.
    #[arg(long)]
    pub dry_run: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_parses_words_and_categories() {
        let c = Cli::try_parse_from([
            "gogo",
            "search",
            "witcher",
            "3",
            "-c",
            "dlc,goodies",
            "-c",
            "packs",
            "--owned",
        ])
        .unwrap();
        let Command::Search(a) = c.command else {
            panic!("expected Search");
        };
        assert_eq!(a.query, ["witcher", "3"]);
        assert_eq!(
            a.category,
            [
                SearchCategory::Dlc,
                SearchCategory::Goodies,
                SearchCategory::Packs
            ]
        );
        assert!(a.owned && !a.json && !a.refresh);
        assert!(Cli::try_parse_from(["gogo", "search"]).is_err());
        assert!(Cli::try_parse_from(["gogo", "search", "x", "-c", "movies"]).is_err());
        assert!(Cli::try_parse_from(["gogo", "search", "x", "--goodies-type", "Audio!"]).is_err());
    }

    #[test]
    fn depot_parses_defaults_and_overrides() {
        let d = Cli::try_parse_from(["gogo", "depot", "pharaoh"]).unwrap();
        let Command::Depot(a) = d.command else {
            panic!("expected Depot");
        };
        assert_eq!(a.game, "pharaoh");
        assert_eq!(a.language, None);
        assert!(a.dlc.is_empty() && !a.all_dlcs && !a.dry_run && !a.json);

        let d = Cli::try_parse_from([
            "gogo",
            "depot",
            "1207659046",
            "--with-dlcs",
            "--language",
            "fr-FR",
            "--limit",
            "4MiB/s",
            "--dry-run",
            "--json-progress",
        ])
        .unwrap();
        let Command::Depot(a) = d.command else {
            panic!("expected Depot")
        };
        assert!(a.all_dlcs && a.dry_run && a.json_progress);
        assert_eq!(a.language.as_deref(), Some("fr-FR"));
        assert_eq!(a.limit.as_deref(), Some("4MiB/s"));
    }

    #[test]
    fn depot_list_languages_stands_alone() {
        let d = Cli::try_parse_from(["gogo", "depot", "1", "--list-languages", "--json"]).unwrap();
        let Command::Depot(a) = d.command else {
            panic!("expected Depot")
        };
        assert!(a.list_languages && a.json);
        for bad in ["--dry-run", "--all-dlcs", "--json-progress"] {
            assert!(
                Cli::try_parse_from(["gogo", "depot", "1", "--list-languages", bad]).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn dlc_flags_on_depot_and_download() {
        let Command::Depot(a) = Cli::try_parse_from([
            "gogo",
            "depot",
            "g",
            "--dlc",
            "Phantom Liberty",
            "--dlc",
            "1597316373",
        ])
        .unwrap()
        .command
        else {
            panic!("expected Depot")
        };
        assert_eq!(a.dlc, ["Phantom Liberty", "1597316373"]);
        assert!(!a.all_dlcs);

        // `--with-dlcs` is the (hidden) old name of `--all-dlcs`
        let Command::Download(a) = Cli::try_parse_from(["gogo", "download", "g", "--with-dlcs"])
            .unwrap()
            .command
        else {
            panic!("expected Download")
        };
        assert!(a.all_dlcs && a.dlc.is_empty());

        assert!(Cli::try_parse_from(["gogo", "depot", "g", "--dlc", "x", "--all-dlcs"]).is_err());
        assert!(
            Cli::try_parse_from(["gogo", "download", "g", "--dlc", "x", "--with-dlcs"]).is_err()
        );
    }

    #[cfg(target_os = "macos")]
    /// `(input, from_depot)` of a parsed `gogo mac-app …` build.
    fn mac_app_source(args: &[&str]) -> (Option<String>, Option<String>) {
        let cli = Cli::try_parse_from(args).unwrap();
        let Command::MacApp {
            sub: None,
            input,
            from_depot,
            ..
        } = cli.command
        else {
            panic!("expected a mac-app build");
        };
        (input, from_depot)
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_app_takes_a_game_or_an_installer() {
        // #73: a game selector is the positional argument (depot lane).
        assert_eq!(
            mac_app_source(&["gogo", "mac-app", "pharaoh", "--yes"]),
            (Some("pharaoh".into()), None)
        );
        assert_eq!(
            mac_app_source(&["gogo", "mac-app", "Downloads/setup_pharaoh_2.1.exe"]),
            (Some("Downloads/setup_pharaoh_2.1.exe".into()), None)
        );
        // --from-depot stays as the explicit alias.
        assert_eq!(
            mac_app_source(&["gogo", "mac-app", "--from-depot", "1207659046"]),
            (None, Some("1207659046".into()))
        );
        // Both at once, or neither, is a usage error.
        assert!(
            Cli::try_parse_from(["gogo", "mac-app", "pharaoh", "--from-depot", "pharaoh"]).is_err()
        );
        assert!(Cli::try_parse_from(["gogo", "mac-app"]).is_err());
    }

    #[test]
    fn runtimes_and_renderers_share_the_store_verbs() {
        let Command::Renderers(a) = Cli::try_parse_from(["gogo", "renderers", "list"])
            .unwrap()
            .command
        else {
            panic!("expected Renderers");
        };
        assert!(matches!(a.cmd, StoreCmd::List { json: false, .. }));
        // prune needs a target or --unused (--all still works), not both.
        let prune = |args: &[&str]| {
            Cli::try_parse_from([&["gogo", "runtimes", "prune"], args].concat()).map(|c| c.command)
        };
        assert!(prune(&[]).is_err());
        assert!(prune(&["--unused", "winecx"]).is_err());
        for flag in ["--unused", "--all"] {
            let Ok(Command::Runtimes(RuntimesArgs {
                cmd:
                    StoreCmd::Prune {
                        unused: true,
                        id: None,
                    },
            })) = prune(&[flag])
            else {
                panic!("{flag} should parse as --unused");
            };
        }
    }

    #[test]
    fn apps_verbs_parse() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from([&["gogo", "apps"], args].concat()).map(|c| c.command)
        };
        assert!(matches!(
            parse(&["list"]),
            Ok(Command::Apps(AppsArgs {
                cmd: AppsCmd::List { json: false, .. }
            }))
        ));
        assert!(matches!(
            parse(&["remove", "1207659013", "-y"]),
            Ok(Command::Apps(AppsArgs {
                cmd: AppsCmd::Remove {
                    yes: true,
                    keep_override: false,
                    ..
                }
            }))
        ));
        // prune names what to remove; there's no bare "remove everything".
        assert!(parse(&["prune"]).is_err());
        assert!(matches!(
            parse(&["prune", "--orphaned"]),
            Ok(Command::Apps(AppsArgs {
                cmd: AppsCmd::Prune { orphaned: true, .. }
            }))
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn runtime_takes_catalog_ids_and_short_names() {
        for name in ["staging", "wine-staging"] {
            let cli = Cli::try_parse_from(["gogo", "mac-app", "x", "--runtime", name]).unwrap();
            let Command::MacApp { runtime, .. } = cli.command else {
                panic!("expected MacApp");
            };
            assert_eq!(runtime, Some(Some(RuntimeSource::Staging)));
        }
    }

    /// Help text is for users: no pointers into the repo's docs or issues.
    #[test]
    fn help_text_does_not_reference_repo_docs() {
        fn texts(cmd: &clap::Command, path: &str, out: &mut Vec<(String, String)>) {
            let path = format!("{path} {}", cmd.get_name());
            for t in [cmd.get_about(), cmd.get_long_about()]
                .into_iter()
                .flatten()
            {
                out.push((path.clone(), t.to_string()));
            }
            for arg in cmd.get_arguments() {
                let at = format!("{path} --{}", arg.get_id());
                for t in [arg.get_help(), arg.get_long_help()].into_iter().flatten() {
                    out.push((at.clone(), t.to_string()));
                }
                for v in arg.get_possible_values() {
                    if let Some(t) = v.get_help() {
                        out.push((format!("{at}={}", v.get_name()), t.to_string()));
                    }
                }
            }
            for sub in cmd.get_subcommands() {
                texts(sub, &path, out);
            }
        }
        let mut all = Vec::new();
        texts(&<Cli as clap::CommandFactory>::command(), "", &mut all);
        let issue_ref = |t: &str| {
            t.match_indices('#')
                .any(|(i, _)| t[i + 1..].starts_with(|c: char| c.is_ascii_digit()))
        };
        let bad: Vec<_> = all
            .iter()
            .filter(|(_, t)| {
                t.contains("docs/") || t.contains("design ") || t.contains('§') || issue_ref(t)
            })
            .collect();
        assert!(bad.is_empty(), "help text references repo docs: {bad:#?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_app_takes_a_library_only_for_a_build() {
        let cli = Cli::try_parse_from(["gogo", "linux-app", "tag", "--library", "/big"]).unwrap();
        let Command::LinuxApp { library, .. } = cli.command else {
            panic!("expected a linux-app build");
        };
        assert_eq!(library.as_deref(), Some(std::path::Path::new("/big")));
        assert!(
            Cli::try_parse_from(["gogo", "linux-app", "--retarget=1", "--library=/big"]).is_err()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_app_takes_a_library_but_not_for_a_portable_build() {
        let cli = Cli::try_parse_from(["gogo", "mac-app", "tag", "--library", "/big"]).unwrap();
        let Command::MacApp { library, .. } = cli.command else {
            panic!("expected a mac-app build");
        };
        assert_eq!(library.as_deref(), Some(std::path::Path::new("/big")));
        assert!(
            Cli::try_parse_from(["gogo", "mac-app", "tag", "--portable", "--library=/big"])
                .is_err()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_app_parses_its_flags() {
        let cli = Cli::try_parse_from([
            "gogo",
            "linux-app",
            "tag",
            "--runtime",
            "ge-proton/GE-Proton10-34",
            "--renderer",
            "vkd3d-proton",
            "--refresh",
            "--yes",
        ])
        .unwrap();
        let Command::LinuxApp {
            sub: None,
            input,
            runtime,
            renderer,
            refresh,
            yes,
            ..
        } = cli.command
        else {
            panic!("expected a linux-app build");
        };
        assert_eq!(input.as_deref(), Some("tag"));
        assert_eq!(runtime, Some(Some("ge-proton/GE-Proton10-34".into())));
        assert_eq!(
            renderer.map(|r| r.choice()),
            Some(gogo::macapp::RendererChoice::Force(
                gogo::macapp::Renderer::Vkd3dProton
            ))
        );
        assert!(refresh && yes);

        // A bare `--runtime` asks; mac-only flags don't exist here.
        let cli = Cli::try_parse_from(["gogo", "linux-app", "tag", "--runtime"]).unwrap();
        let Command::LinuxApp { runtime, .. } = cli.command else {
            panic!("expected linux-app");
        };
        assert_eq!(runtime, Some(None));
        for flag in ["--portable", "--codesign", "--out=x", "--retarget=1"] {
            assert!(
                Cli::try_parse_from(["gogo", "linux-app", "tag", flag]).is_err(),
                "{flag}"
            );
        }
        for r in ["dxmt", "d3dmetal"] {
            assert!(Cli::try_parse_from(["gogo", "linux-app", "x", "--renderer", r]).is_err());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_app_takes_dlc_flags_that_conflict_as_designed() {
        let cli = Cli::try_parse_from([
            "gogo",
            "linux-app",
            "tag",
            "--dlc",
            "REDmod",
            "--dlc",
            "123",
            "--lang",
            "de-DE",
        ])
        .unwrap();
        let Command::LinuxApp {
            dlc,
            all_dlcs,
            no_dlcs,
            lang,
            ..
        } = cli.command
        else {
            panic!("expected a linux-app build");
        };
        assert_eq!(dlc, ["REDmod", "123"]);
        assert!(!all_dlcs && !no_dlcs);
        assert_eq!(lang.as_deref(), Some("de-DE"));
        for pair in [
            ["--dlc=1", "--all-dlcs"],
            ["--dlc=1", "--no-dlcs"],
            ["--all-dlcs", "--no-dlcs"],
        ] {
            assert!(
                Cli::try_parse_from(["gogo", "linux-app", "tag", pair[0], pair[1]]).is_err(),
                "{pair:?}"
            );
        }
        assert!(Cli::try_parse_from(["gogo", "linux-app", "tag", "--all-dlcs"]).is_ok());
        assert!(Cli::try_parse_from(["gogo", "linux-app", "--retarget=1", "--lang=de"]).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_app_setup_takes_a_gogid() {
        let cli =
            Cli::try_parse_from(["gogo", "linux-app", "setup", "1207659013", "--json"]).unwrap();
        let Command::LinuxApp {
            sub:
                Some(LinuxAppSub::Setup {
                    gogid,
                    json,
                    refresh,
                }),
            ..
        } = cli.command
        else {
            panic!("expected linux-app setup");
        };
        assert_eq!(gogid, "1207659013");
        assert!(json && !refresh);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_app_move_takes_a_dir_or_default_and_not_both() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from(
                ["gogo", "linux-app", "move", "7"]
                    .iter()
                    .chain(args)
                    .copied(),
            )
        };
        let Command::LinuxApp {
            sub: Some(LinuxAppSub::Move { dir, default, .. }),
            ..
        } = parse(&["/big"]).unwrap().command
        else {
            panic!("not a move");
        };
        assert_eq!(dir.as_deref(), Some(std::path::Path::new("/big")));
        assert!(!default);
        assert!(matches!(
            parse(&["--default", "--dry-run"]).unwrap().command,
            Command::LinuxApp {
                sub: Some(LinuxAppSub::Move {
                    dir: None,
                    default: true,
                    dry_run: true,
                    ..
                }),
                ..
            }
        ));
        assert!(parse(&[]).is_err(), "a dir or --default");
        assert!(parse(&["/big", "--default"]).is_err());
        let Command::LinuxApp {
            sub: Some(LinuxAppSub::Move { adopt, dir, .. }),
            ..
        } = parse(&["--adopt", "/mnt/x/1", "--dry-run"])
            .unwrap()
            .command
        else {
            panic!("not a move");
        };
        assert_eq!(adopt.as_deref(), Some(std::path::Path::new("/mnt/x/1")));
        assert!(dir.is_none());
        assert!(parse(&["--adopt", "/mnt/x/1", "--default"]).is_err());
        assert!(parse(&["--adopt", "/mnt/x/1", "/big"]).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_app_exes_and_run_parse() {
        let Command::LinuxApp {
            sub: Some(LinuxAppSub::Exes { gogid, all, json }),
            ..
        } = Cli::try_parse_from(["gogo", "linux-app", "exes", "1423049311", "--all", "--json"])
            .unwrap()
            .command
        else {
            panic!("not exes");
        };
        assert_eq!((gogid.as_str(), all, json), ("1423049311", true, true));
        // The exe's own flags after `--`, hyphens and all.
        let Command::LinuxApp {
            sub: Some(LinuxAppSub::Run { gogid, exe, args }),
            ..
        } = Cli::try_parse_from([
            "gogo",
            "linux-app",
            "run",
            "1423049311",
            r"tools\redmod\bin\redMod.exe",
            "--",
            "-x",
            "--help",
        ])
        .unwrap()
        .command
        else {
            panic!("not run");
        };
        assert_eq!(gogid, "1423049311");
        assert_eq!(exe, r"tools\redmod\bin\redMod.exe");
        assert_eq!(args, ["-x", "--help"]);
        assert!(Cli::try_parse_from(["gogo", "linux-app", "run", "1423049311"]).is_err());
        assert!(
            Cli::try_parse_from(["gogo", "linux-app", "run", "1423049311", "1", "-x"]).is_err()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_app_retarget_compat_and_next_renderer_parse() {
        let cli = Cli::try_parse_from([
            "gogo",
            "linux-app",
            "--retarget",
            "1207659013",
            "--renderer",
            "wined3d",
        ])
        .unwrap();
        let Command::LinuxApp {
            retarget,
            renderer,
            input,
            ..
        } = cli.command
        else {
            panic!("expected linux-app");
        };
        assert_eq!(retarget.as_deref(), Some("1207659013"));
        assert_eq!(renderer, Some(LinuxRendererArg::Wined3d));
        assert!(input.is_none());
        assert!(Cli::try_parse_from(["gogo", "linux-app", "x", "--retarget", "1"]).is_err());
        assert!(
            Cli::try_parse_from(["gogo", "linux-app", "--retarget", "1", "--interactive"]).is_err()
        );

        let cli =
            Cli::try_parse_from(["gogo", "linux-app", "next-renderer", "1", "--dry-run"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::LinuxApp {
                sub: Some(LinuxAppSub::NextRenderer { dry_run: true, .. }),
                ..
            }
        ));
        let cli = Cli::try_parse_from([
            "gogo",
            "linux-app",
            "args",
            "1",
            "--",
            "--launcher-skip",
            "-skipStartScreen",
        ])
        .unwrap();
        let Command::LinuxApp {
            sub: Some(LinuxAppSub::Args { args, clear, .. }),
            ..
        } = cli.command
        else {
            panic!("expected linux-app args");
        };
        assert_eq!(args, ["--launcher-skip", "-skipStartScreen"]);
        assert!(!clear);
        assert!(Cli::try_parse_from(["gogo", "linux-app", "args", "1", "--clear"]).is_ok());
        assert!(
            Cli::try_parse_from(["gogo", "linux-app", "args", "1", "--clear", "--", "-x"]).is_err()
        );
        // Without `--`, a dash arg is gogo's flag, not the game's.
        assert!(Cli::try_parse_from(["gogo", "linux-app", "args", "1", "-x"]).is_err());
        // A wrapper's own `--` is one of its words.
        let cli = Cli::try_parse_from([
            "gogo",
            "linux-app",
            "wrapper",
            "1",
            "--",
            "gamescope",
            "-f",
            "--",
        ])
        .unwrap();
        let Command::LinuxApp {
            sub: Some(LinuxAppSub::Wrapper { words, clear, .. }),
            ..
        } = cli.command
        else {
            panic!("expected linux-app wrapper");
        };
        assert_eq!(words, ["gamescope", "-f", "--"]);
        assert!(!clear);
        assert!(Cli::try_parse_from(["gogo", "linux-app", "wrapper", "1", "--clear"]).is_ok());
        assert!(Cli::try_parse_from(["gogo", "linux-app", "wrapper", "1"]).is_ok());
        assert!(
            Cli::try_parse_from(["gogo", "linux-app", "wrapper", "1", "--clear", "--", "x"])
                .is_err()
        );
        assert!(Cli::try_parse_from(["gogo", "linux-app", "wrapper", "1", "gamescope"]).is_err());
        let cli = Cli::try_parse_from([
            "gogo",
            "linux-app",
            "display",
            "1",
            "wayland",
            "--hdr",
            "force",
            "--monitor",
            "DP-1",
        ])
        .unwrap();
        let Command::LinuxApp {
            sub:
                Some(LinuxAppSub::Display {
                    mode,
                    hdr,
                    monitor,
                    clear,
                    ..
                }),
            ..
        } = cli.command
        else {
            panic!("expected linux-app display");
        };
        assert_eq!(
            (mode.as_deref(), hdr.as_deref(), monitor.as_deref(), clear),
            (Some("wayland"), Some("force"), Some("DP-1"), false)
        );
        for ok in [
            &["display", "1"][..],
            &["display", "1", "x11"],
            &["display", "1", "--hdr", "off"],
            &["display", "1", "--clear"],
            &["display", "1", "--monitor", "DP-1"],
            &["display", "1", "--no-monitor"],
        ] {
            let argv = ["gogo", "linux-app"]
                .iter()
                .chain(ok)
                .copied()
                .collect::<Vec<_>>();
            assert!(Cli::try_parse_from(&argv).is_ok(), "{argv:?}");
        }
        for bad in [
            &["display", "1", "wl"][..],
            &["display", "1", "--hdr", "yes"],
            &["display", "1", "x11", "--clear"],
            &["display", "1", "--monitor", "DP-1", "--no-monitor"],
            &["display", "1", "--no-monitor", "--clear"],
            &["display", "1", "--monitor", "DP-1", "--clear"],
        ] {
            let argv = ["gogo", "linux-app"]
                .iter()
                .chain(bad)
                .copied()
                .collect::<Vec<_>>();
            assert!(Cli::try_parse_from(&argv).is_err(), "{argv:?}");
        }
        let cli = Cli::try_parse_from([
            "gogo",
            "linux-app",
            "env",
            "set",
            "1",
            "DXVK_HUD=fps",
            "A=b c",
        ])
        .unwrap();
        let Command::LinuxApp {
            sub:
                Some(LinuxAppSub::Env {
                    cmd: LinuxEnvCmd::Set { pairs, .. },
                }),
            ..
        } = cli.command
        else {
            panic!("expected env set");
        };
        assert_eq!(pairs, ["DXVK_HUD=fps", "A=b c"]);
        assert!(Cli::try_parse_from(["gogo", "linux-app", "env", "set", "1"]).is_err());
        let all = |args: &[&str]| {
            let mut v = vec!["gogo", "linux-app", "env"];
            v.extend_from_slice(args);
            let Command::LinuxApp {
                sub:
                    Some(LinuxAppSub::Env {
                        cmd: LinuxEnvCmd::List { all, .. },
                    }),
                ..
            } = Cli::try_parse_from(v).unwrap().command
            else {
                panic!("expected env list");
            };
            all
        };
        assert!(
            !all(&["list", "1"]),
            "--all is opt-in: the default is #234's"
        );
        assert!(all(&["list", "1", "--all", "--json", "--reveal"]));
        assert!(
            Cli::try_parse_from([
                "gogo",
                "linux-app",
                "env",
                "describe",
                "1",
                "DXVK_HUD",
                "--json"
            ])
            .is_ok()
        );
        assert!(Cli::try_parse_from(["gogo", "linux-app", "env", "describe", "1"]).is_err());
        assert!(Cli::try_parse_from(["gogo", "linux-app", "env", "unset", "1", "A", "B"]).is_ok());
        assert!(
            Cli::try_parse_from([
                "gogo",
                "linux-app",
                "env",
                "list",
                "1",
                "--reveal",
                "--json"
            ])
            .is_ok()
        );
        let cli = Cli::try_parse_from(["gogo", "linux-app", "compat", "1", "--json"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::LinuxApp {
                sub: Some(LinuxAppSub::Compat { json: true, .. }),
                ..
            }
        ));
    }
}
