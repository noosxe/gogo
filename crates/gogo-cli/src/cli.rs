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
    /// Interactive browser paste-code login (docs/design/02 §2).
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
        /// Additionally compare checksum-XML build id + timestamp (docs/design/06 §4).
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
        /// Walk the download dir and adopt verified files into records (06 §5).
        #[arg(long)]
        rebuild: bool,
    },
    /// Download a game straight from GOG's content system (depot / Model B)
    /// into the user cache directory (docs/design/14). macOS only for now.
    Depot(DepotArgs),
    /// Config file management.
    Config {
        #[command(subcommand)]
        sub: ConfigSub,
    },
    /// Convert a downloaded Windows installer into a self-contained macOS .app
    /// (docs/design/10).
    #[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
    MacApp {
        #[command(subcommand)]
        sub: Option<MacAppSub>,
        /// Path to the GOG setup .exe (.bin siblings are picked up automatically).
        #[arg(
            value_name = "INSTALLER",
            required_unless_present_any = ["from_depot", "retarget"],
            conflicts_with_all = ["from_depot", "retarget"]
        )]
        installer: Option<std::path::PathBuf>,
        /// Point an existing game at another runtime or renderer without
        /// rebuilding: writes `--runtime` / `--renderer` into the game's user
        /// override file (L4, design 16 decision 7), then sets the game up on
        /// it (installing the runtime, `wineboot -u` for a new engine). The
        /// signed bundle is untouched. `auto` drops the override.
        #[arg(long, value_name = "BUNDLE|GOGID", conflicts_with = "from_depot")]
        retarget: Option<String>,
        /// Build from a depot (Model B) game tree instead of running an
        /// installer: an existing directory, or a game selector resolved
        /// against the local depot cache (docs/design/15).
        #[arg(long, value_name = "GAME|PATH")]
        from_depot: Option<String>,
        /// Bundle title (default: derived from the installer file name).
        #[arg(long)]
        name: Option<String>,
        /// Wine runtime source (docs/design/10 §3).
        #[arg(long, value_enum)]
        runtime: Option<RuntimeSource>,
        /// Runtime directory for --runtime gptk|path.
        #[arg(long, value_name = "DIR")]
        runtime_path: Option<std::path::PathBuf>,
        /// Pin a catalog release tag (`--runtime winecx|staging|devel|stable`).
        #[arg(long, value_name = "TAG", requires = "runtime")]
        runtime_version: Option<String>,
        /// Output directory for <name>.app (default: current directory).
        #[arg(long, value_name = "DIR")]
        out: Option<std::path::PathBuf>,
        /// Lutris recipe: auto (default; live search + selection), none,
        /// a Lutris installer slug, or a downloaded script JSON path
        /// (docs/design/11 §1).
        #[arg(long)]
        recipe: Option<String>,
        /// Renderer (#70): `auto` (default) picks it from the game's PE
        /// imports (D3D10/11 → dxmt, D3D9 and older → wined3d, 2D DirectDraw
        /// → cnc-ddraw) unless gogo's compat DB or your override file says
        /// otherwise.
        #[arg(long, value_enum)]
        renderer: Option<RendererArg>,
        /// Alias for `--renderer cnc-ddraw` (design 13).
        #[arg(long, conflicts_with_all = ["no_ddraw_wrapper", "renderer"])]
        ddraw_wrapper: bool,
        /// Like `--renderer auto`, but never cnc-ddraw (wined3d instead).
        #[arg(long, conflicts_with = "renderer")]
        no_ddraw_wrapper: bool,
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
    /// Manage Wine runtimes: list the catalog + cache, install, prune
    /// (design 12 §10).
    Runtimes(RuntimesArgs),
    /// Inspect Lutris community recipes for a game (design 11 §1): list the
    /// ranked candidates, or show one recipe's execution plan.
    Recipes(RecipesArgs),
    /// Internal: extractor shims materialized for winetricks (design 12 D5).
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
    /// DLLs (#63, docs/19 §6.2). Input for renderer selection; changes nothing.
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
    /// Print a game's effective compat config and the layer each field came
    /// from (design 16 decision 5).
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
    /// registry values and verbs, write the effective launch config (design
    /// 16 §2.2). The launcher runs this when its config stamp is stale.
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
    /// Switch a game to the next renderer in its order (the launcher's
    /// "Try next renderer", #78): the one after what setup last applied,
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
    /// Install root override (default: user cache dir, design 14 D2).
    #[arg(long, value_name = "DIR")]
    pub dir: Option<std::path::PathBuf>,
    /// Global rate limit, e.g. 4MiB/s (download.limit grammar, 05 §3).
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
    /// Picked from the game's imports (L1), unless L3/L4 say otherwise.
    Auto,
    /// Wine's own Direct3D-on-OpenGL.
    Wined3d,
    /// D3D10/11 on Metal (needs the winecx runtime and a Metal 3 GPU).
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
    /// Recipe-recommended or cached runtime, else brew, else download
    /// (design 12 §5).
    Auto,
    /// Homebrew wine-crossover.
    Brew,
    /// Cached/downloaded CrossOver-source engine `winecx` with its
    /// Frameworks/ (the default track, docs/19 §6.1).
    Winecx,
    /// Cached/downloaded wine-staging (design 12 catalog; the alternate track).
    Staging,
    /// Cached/downloaded wine-devel.
    Devel,
    /// Cached/downloaded wine-stable.
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
    pub cmd: RuntimesCmd,
}

#[derive(Debug, clap::Subcommand)]
pub enum RuntimesCmd {
    /// List catalog entries (live-probed) and cached runtimes.
    List {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
        /// Skip live catalog probes.
        #[arg(long)]
        offline: bool,
    },
    /// Download, verify, and cache a catalog runtime (macOS only).
    Install {
        /// Catalog id (see `runtimes list`).
        #[arg(value_name = "ID")]
        id: String,
        /// Pin a release tag (default: latest).
        #[arg(long)]
        version: Option<String>,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Remove cached runtimes and renderers from the store (never one a
    /// game still uses).
    Prune {
        /// Remove every cached runtime and renderer no game uses, but the
        /// newest two versions of each (the previous one, to roll back to).
        #[arg(long)]
        all: bool,
        /// Remove one catalog id (every version), or one version as
        /// `<id>/<version>` (with the derived runtimes built on it).
        #[arg(value_name = "ID[/VERSION]")]
        id: Option<String>,
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
    /// reasons (design 11 §3). Nothing is executed.
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
    /// Include DLC installers (DLCs enter only via their parent game, A1).
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
    /// Global rate limit, e.g. 4MiB/s (download.limit grammar, 05 §3).
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
}
