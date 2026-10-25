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
    /// macOS only for now.
    Depot(DepotArgs),
    /// Config file management.
    Config {
        #[command(subcommand)]
        sub: ConfigSub,
    },
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
    /// Manage Wine runtimes: list the catalog and what's cached, install,
    /// prune.
    Runtimes(RuntimesArgs),
    /// Manage renderers (cnc-ddraw, DXMT, D3DMetal): list the catalog and
    /// what's cached, install, prune.
    Renderers(RenderersArgs),
    /// Manage the games gogo set up for its .app bundles: each one's Wine
    /// prefix (saves included), game files and launch logs. List them,
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
    /// Content language code (v2 manifests; v1 names are derived).
    #[arg(long, default_value = "en-US")]
    pub language: String,
    /// Include DLC depots.
    #[arg(long)]
    pub with_dlcs: bool,
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
    },
    /// Download, verify, and cache a catalog entry (macOS only).
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
    /// List every game set up on this Mac: its .app (and whether it is
    /// still there), runtime, renderer, disk use and last launch. Disk use
    /// counts the game files in full, though a depot game's share space
    /// with the depot cache.
    List {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Show where one game's things live: state folder, Wine prefix,
    /// launch logs, override file, .app.
    Show {
        /// GOG product id, or the game's .app.
        #[arg(value_name = "BUNDLE|GOGID")]
        target: String,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Delete one game's state folder (Wine prefix and saves included),
    /// its launch logs and its override file. The .app stays, but won't
    /// start again until rebuilt. Refuses while the game is running.
    Remove {
        /// GOG product id, or the game's .app.
        #[arg(value_name = "BUNDLE|GOGID")]
        target: String,
        /// Keep the override file (`gogo mac-app compat` shows it).
        #[arg(long)]
        keep_override: bool,
        /// Don't ask first.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Remove every game whose .app is no longer where it was built or
    /// last launched from. A moved .app updates that on its next launch,
    /// so check the list before saying yes.
    Prune {
        /// Games whose .app is missing (the only kind for now).
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
    /// Include DLC installers (a DLC comes with its parent game).
    #[arg(long)]
    pub with_dlcs: bool,
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
    fn depot_parses_defaults_and_overrides() {
        let d = Cli::try_parse_from(["gogo", "depot", "pharaoh"]).unwrap();
        let Command::Depot(a) = d.command else {
            panic!("expected Depot");
        };
        assert_eq!(a.game, "pharaoh");
        assert_eq!(a.language, "en-US");
        assert!(!a.with_dlcs && !a.dry_run && !a.json);

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
        assert!(a.with_dlcs && a.dry_run && a.json_progress);
        assert_eq!(a.language, "fr-FR");
        assert_eq!(a.limit.as_deref(), Some("4MiB/s"));
    }

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
                cmd: AppsCmd::List { json: false }
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
}
