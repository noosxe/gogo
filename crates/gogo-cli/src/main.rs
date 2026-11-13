//! clap parsing, tracing setup, exit-code mapping — no business logic (`docs/design/01` §1).
//! This crate is a thin shell over `gogo-core`; everything domain lives there.

mod apps_sort;
mod builds_cmd;
mod cli;
mod depot_progress;
#[cfg(target_os = "linux")]
mod dlc_cmd;
#[cfg(target_os = "linux")]
mod installer_dlc;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod lane;
#[cfg(target_os = "macos")]
mod menus;
mod output;
mod prompt;
#[cfg(target_os = "linux")]
mod snapshot_cmd;
#[cfg(target_os = "linux")]
mod update_cmd;

use std::sync::Arc;

use clap::Parser;
use gogo::app::AppContext;
use gogo::config::{Config, ConfigError};
use gogo::engine::task::Kind;
use gogo::engine::{self, select::Filters};
use gogo::gog::error::{ApiError, AuthError};
use gogo::gog::library;

use crate::cli::{Cli, ColorWhen, Command, OsFilter};

fn main() {
    let cli = Cli::parse();
    init_tracing(&cli);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let code: i32 = match rt.block_on(run(cli)) {
        Ok(code) => code,
        // C1: the error type carries the class; auth 3 < network 4 < config-usage 2.
        Err(AppError::Config(e)) => {
            eprintln!("gogo: {e}");
            cli::exit::USAGE
        }
        Err(AppError::Auth(e)) => {
            eprintln!("gogo: {e}");
            cli::exit::AUTH
        }
        Err(AppError::Api(e)) => {
            eprintln!("gogo: {e}");
            cli::exit::NETWORK
        }
        Err(AppError::MacApp(e)) => {
            eprintln!("gogo: {e}");
            // Conversion failure (exit 7) — docs/design/10 §4.
            cli::exit::MACAPP
        }
        Err(AppError::Depot(e)) => match e {
            // A DLC the account doesn't own, or doesn't exist (design 19 §2.2).
            // Display only: it lists the valid DLC and never a server message.
            gogo::gog::depot::DepotError::NoCacheDir
            | gogo::gog::depot::DepotError::Dlc(_)
            | gogo::gog::depot::DepotError::Pick(_)
            | gogo::gog::depot::DepotError::Language(_) => {
                eprintln!("gogo: {e}");
                cli::exit::USAGE
            }
            gogo::gog::depot::DepotError::Filter(_) => {
                eprintln!("gogo: {e}");
                cli::exit::DEPOT
            }
            _ => {
                // Debug, not Display: gog-warp's io variants hide their source
                eprintln!("gogo: depot: {e:?}");
                cli::exit::DEPOT
            }
        },
        Err(AppError::Io(e)) => {
            eprintln!("gogo: {e}");
            cli::exit::FAILED
        }
        Err(AppError::Usage) => cli::exit::USAGE,
    };
    // Exit explicitly — never through `rt` teardown: Runtime::drop JOINS blocking
    // threads, and an OS keychain read parked on an unattended user prompt never
    // returns, hanging the process after the error was already printed
    // (live finding 2026-10-10). The parked thread dies with the process.
    std::process::exit(code);
}

/// Typed top-level error → exit class (03 §5, review C1).
/// Typed top-level error → exit class (03 §5, review C1).
enum AppError {
    Config(ConfigError),
    Usage,
    Auth(AuthError),
    Api(ApiError),
    MacApp(gogo::macapp::MacAppError),
    Depot(gogo::gog::depot::DepotError),
    Io(std::io::Error),
}

impl From<gogo::macapp::MacAppError> for AppError {
    fn from(e: gogo::macapp::MacAppError) -> Self {
        Self::MacApp(e)
    }
}

impl From<gogo::gog::depot::DepotError> for AppError {
    fn from(e: gogo::gog::depot::DepotError) -> Self {
        Self::Depot(e)
    }
}

impl From<ConfigError> for AppError {
    fn from(e: ConfigError) -> Self {
        Self::Config(e)
    }
}
impl From<AuthError> for AppError {
    fn from(e: AuthError) -> Self {
        Self::Auth(e)
    }
}
impl From<ApiError> for AppError {
    fn from(e: ApiError) -> Self {
        Self::Api(e)
    }
}
impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// The global `--quiet` and `--no-progress`, for the depot lane's progress.
static PROGRESS_FLAGS: std::sync::OnceLock<(bool, bool)> = std::sync::OnceLock::new();

/// How a command with these flags shows the depot lane's events (#337).
pub(crate) fn depot_mode(json_progress: bool, json: bool) -> depot_progress::Mode {
    let (quiet, no_progress) = PROGRESS_FLAGS.get().copied().unwrap_or_default();
    depot_progress::Mode::pick(
        json_progress,
        json,
        quiet,
        no_progress,
        std::io::IsTerminal::is_terminal(&std::io::stderr()),
    )
}

/// A blocking step of `step` with the spinner on a terminal (#422): nothing is
/// printed off one, nor under `--json` or `--quiet`.
pub(crate) fn phase_sync<T>(json: bool, step: &str, f: impl FnOnce() -> T) -> T {
    depot_progress::Activity::new(depot_mode(false, json)).spin(step, f)
}

/// [`phase_sync`] around a future.
pub(crate) async fn phase<T>(json: bool, step: &str, f: impl std::future::Future<Output = T>) -> T {
    depot_progress::Activity::new(depot_mode(false, json))
        .spin_async(step, f)
        .await
}

/// RUST_LOG wins if set; otherwise derive from --quiet/-v (`docs/design/07` §5).
/// Log output goes to stderr; stdout stays results-only (`docs/design/03` §4).
fn init_tracing(cli: &Cli) {
    use tracing_subscriber::EnvFilter;

    let default = if cli.quiet {
        "warn".to_string()
    } else {
        match cli.verbose {
            // gog-warp's info lines ("Allocating disk space", …) say what the
            // depot progress already shows, and would break its status line
            // (#337); `-v` brings them back.
            0 => "info,gog_warp=warn",
            1 => "debug",
            _ => "trace",
        }
        .to_string()
    };

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Dispatch one command; returns its exit code (`docs/design/03-cli.md` §5).
async fn run(cli: Cli) -> Result<i32, AppError> {
    let _ = CONFIG_PATH.set(cli.config.clone());
    let _ = PROGRESS_FLAGS.set((cli.quiet, cli.no_progress));
    let color = cli.color;
    match cli.command {
        Command::Login {
            code,
            print_url,
            force,
        } => login(code, print_url, force).await,
        Command::Logout {
            all_stores,
            purge_state,
        } => logout(all_stores, purge_state),
        Command::Whoami { json } => whoami(json).await,
        Command::List {
            search,
            os,
            with_dlcs,
            installed,
            json,
            refresh,
        } => list(search, os, with_dlcs, installed, json, refresh, color).await,
        Command::Search(args) => search(args).await,
        Command::Info { game, json } => info(game, json).await,
        Command::Download(args) => download(args, cli.no_progress, cli.quiet).await,
        Command::Depot(args) => depot_cmd(args).await,
        Command::Builds(args) => builds_cmd::run(args).await,
        Command::Update { games, all, strict } => update(games, all, strict).await,
        Command::Verify {
            games,
            all,
            path,
            fix,
            rebuild,
        } => verify(games, all, path, fix, rebuild).await,
        Command::Config { sub } => config_cmd(sub),
        #[cfg(target_os = "macos")]
        Command::MacApp { sub: Some(sub), .. } => mac_app_sub(sub),
        // The bare `--runtime` menu (#76) is a build-time form only.
        #[cfg(target_os = "macos")]
        Command::MacApp {
            retarget: Some(_),
            runtime: Some(None),
            ..
        } => {
            eprintln!("gogo: --retarget needs a runtime id (`--runtime <RUNTIME>`)");
            Ok(cli::exit::USAGE)
        }
        #[cfg(target_os = "macos")]
        Command::MacApp {
            retarget: Some(target),
            runtime,
            renderer,
            runtime_path,
            runtime_version,
            json,
            ..
        } => mac_app_retarget(
            target,
            runtime.flatten(),
            renderer,
            runtime_path,
            runtime_version,
            json,
        ),
        #[cfg(target_os = "macos")]
        Command::MacApp {
            sub: None,
            retarget: _,
            input,
            from_depot,
            name,
            library,
            runtime,
            runtime_path,
            runtime_version,
            recipe,
            interactive,
            renderer,
            ddraw_wrapper,
            no_ddraw_wrapper,
            portable,
            out,
            yes,
            json_progress,
            json,
        } => {
            let renderer = match (renderer, ddraw_wrapper, no_ddraw_wrapper) {
                (_, true, _) => {
                    gogo::macapp::RendererChoice::Force(gogo::macapp::Renderer::CncDdraw)
                }
                (_, _, true) => gogo::macapp::RendererChoice::AutoNoDdraw,
                (Some(r), _, _) => match r.renderer() {
                    Some(r) => gogo::macapp::RendererChoice::Force(r),
                    None => gogo::macapp::RendererChoice::Auto,
                },
                (None, _, _) => gogo::macapp::RendererChoice::Auto,
            };
            let menus = menus::resolve(recipe.as_deref(), runtime, interactive);
            mac_app(
                input,
                from_depot,
                name,
                library,
                menus,
                runtime_path,
                runtime_version,
                renderer,
                portable,
                out,
                yes,
                json_progress,
                json,
            )
            .await
        }
        #[cfg(target_os = "linux")]
        Command::LinuxApp {
            sub: Some(crate::cli::LinuxAppSub::Dlc { gogid, cmd }),
            ..
        } => dlc_cmd::run(gogid, cmd).await,
        #[cfg(target_os = "linux")]
        Command::LinuxApp {
            sub:
                Some(crate::cli::LinuxAppSub::Update {
                    gogid,
                    build,
                    dry_run,
                    yes,
                    limit,
                    no_disk_check,
                    no_snapshots,
                    json_progress,
                    json,
                }),
            ..
        } => {
            update_cmd::run(
                gogid,
                update_cmd::Args {
                    build,
                    dry_run,
                    yes,
                    limit,
                    disk_check: !no_disk_check,
                    no_snapshots,
                    json_progress,
                    json,
                },
            )
            .await
        }
        #[cfg(target_os = "linux")]
        Command::LinuxApp { sub: Some(sub), .. } => linux_app_sub(sub),
        #[cfg(target_os = "linux")]
        Command::LinuxApp {
            retarget: Some(_),
            runtime: Some(None),
            ..
        } => {
            eprintln!("gogo: --retarget needs a runtime ref (`--runtime <RUNTIME>`)");
            Ok(cli::exit::USAGE)
        }
        #[cfg(target_os = "linux")]
        Command::LinuxApp {
            retarget: Some(gogid),
            runtime,
            renderer,
            refresh,
            no_snapshots,
            json,
            ..
        } => linux_app_retarget(
            gogid,
            runtime.flatten(),
            renderer,
            refresh,
            no_snapshots,
            json,
        ),
        #[cfg(target_os = "linux")]
        Command::LinuxApp {
            sub: None,
            retarget: None,
            input,
            from_depot,
            name,
            library,
            runtime,
            renderer,
            refresh,
            interactive,
            yes,
            dlc,
            all_dlcs,
            no_dlcs,
            lang,
            build,
            password,
            no_snapshots,
            json_progress,
            json,
        } => {
            linux_app(LinuxAppArgs {
                no_snapshots,
                dlc: DlcAsk {
                    names: dlc,
                    all: all_dlcs,
                    none: no_dlcs,
                    lang,
                    build,
                    password,
                },
                input,
                from_depot,
                name,
                library,
                runtime,
                renderer: renderer.map_or(gogo::macapp::RendererChoice::Auto, |r| r.choice()),
                refresh,
                interactive,
                yes,
                json_progress,
                json,
            })
            .await
        }
        Command::Runtimes(args) => store_cmd(Shelf::Runtimes, args.cmd).await,
        Command::Renderers(args) => store_cmd(Shelf::Renderers, args.cmd).await,
        Command::Apps(args) => apps_cmd(args.cmd),
        Command::Recipes(args) => recipes_cmd(args),
        Command::Shim { tool, args } => Ok(gogo_shims::run_tool(&tool, &args)),
        Command::PeScan { dir, exe, json } => pe_scan(&dir, exe.as_deref(), json),
        Command::Completions { shell } => completions(shell),
    }
}

fn pe_scan(
    dir: &std::path::Path,
    exe: Option<&std::path::Path>,
    json: bool,
) -> Result<i32, AppError> {
    use gogo::macapp::pe;

    let scan = match exe {
        Some(exe) => pe::scan_exe(dir, exe),
        None => pe::scan(dir),
    };
    let scan = match scan {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(cli::exit::FAILED);
        }
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&scan).unwrap());
        return Ok(cli::exit::OK);
    }
    let list = |v: Vec<String>| {
        if v.is_empty() {
            "—".to_string()
        } else {
            v.join(" ")
        }
    };
    println!("main exe   {} ({:?})", scan.main_exe, scan.main_exe_source);
    println!("bitness    {:?}", scan.bitness);
    println!(
        "engine     {}",
        scan.engine.map_or("—".into(), |e| format!("{e:?}"))
    );
    println!(
        "graphics   {:?} (from {:?})",
        scan.graphics, scan.graphics_source
    );
    println!(
        "imports    {}",
        list(
            scan.graphics_imports
                .iter()
                .map(|g| format!("{g:?}"))
                .collect()
        )
    );
    for (path, apis) in &scan.secondary_graphics {
        println!(
            "tool       {path} imports {} (not counted: the entry point has evidence)",
            list(apis.iter().map(|g| format!("{g:?}")).collect())
        );
    }
    if let Some(c) = scan.d3d7_confidence {
        let block = if scan.dxguid_block {
            " (dxguid block present)"
        } else {
            ""
        };
        println!("d3d≤7      {c:?}{block}");
    }
    if !scan.renderer_dlls.is_empty() {
        println!(
            "renderers  {}",
            list(scan.renderer_dlls.iter().cloned().collect())
        );
    }
    println!(
        "runtime    {}",
        list(scan.runtime_loaded.iter().cloned().collect())
    );
    println!(
        "helpers    {}",
        list(scan.helpers.iter().cloned().collect())
    );
    println!(
        "overrides  {}",
        list(scan.local_overrides.iter().cloned().collect())
    );
    Ok(cli::exit::OK)
}

// — login (A6: config-decided store, probe before the browser, no fallback) —

const REDIRECT_URI: &str = "https://embed.gog.com/on_login_success?origin=client";

async fn login(code: Option<String>, print_url: bool, force: bool) -> Result<i32, AppError> {
    let cfg = load_config_from_env()?;
    let store = gogo::auth::store::open(cfg.auth.token_store);

    // A6: probe the configured backend BEFORE opening the browser — a missing
    // secret service fails here, loudly, with the switch hint.
    store.probe()?;

    if !force && store.load()?.is_some() {
        eprintln!("already logged in — use --force to re-login");
        return Ok(cli::exit::OK);
    }

    // Raw redirect, matching gog-warp's token exchange verbatim (live-verified shape).
    let auth_url = format!(
        "https://auth.gog.com/auth?client_id={}&redirect_uri={REDIRECT_URI}&response_type=code&layout=client2",
        gog_warp::constants::GALAXY_CLIENT_ID,
    );
    if print_url {
        println!("{auth_url}");
        return Ok(cli::exit::OK);
    }
    println!("open in a browser and log in:\n  {auth_url}");

    let code = match code {
        Some(code) => code,
        None => {
            print!("paste the redirect URL (or just the code): ");
            use std::io::Write;
            std::io::stdout().flush().ok();
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            line.split("code=")
                .nth(1)
                .unwrap_or(&line)
                .trim()
                .to_owned()
        }
    };

    gogo::auth::Session::login(&code, store.as_ref()).await?;
    println!("logged in");
    Ok(cli::exit::OK)
}

fn load_config_quietly() -> Result<gogo::config::Loaded, ConfigError> {
    match CONFIG_PATH.get().cloned().flatten() {
        Some(path) => Config::load_from(&path, |k| std::env::var(k).ok()),
        None => Config::load(),
    }
}

fn load_config_from_env() -> Result<Config, ConfigError> {
    let loaded = load_config_quietly()?;
    for warning in &loaded.warnings {
        eprintln!("gogo: warning: unknown config key `{warning}` (ignored)");
    }
    Ok(loaded.config)
}

/// `--config` / resolved config path, set once before dispatch (05 §2).
static CONFIG_PATH: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();

fn logout(all_stores: bool, purge_state: bool) -> Result<i32, AppError> {
    let cfg = load_config_from_env()?;
    // A6: `logout` clears the configured backend; `--all-stores` clears both.
    let stores: Vec<gogo::config::TokenStoreKind> = if all_stores {
        vec![
            gogo::config::TokenStoreKind::Keyring,
            gogo::config::TokenStoreKind::File,
        ]
    } else {
        vec![cfg.auth.token_store]
    };
    for kind in stores {
        gogo::auth::store::open(kind).clear()?;
    }
    if purge_state {
        // D3: --purge-state also removes the verify-cache (06 §3).
        let _ = std::fs::remove_file(gogo::state::State::path());
        let cache = gogo::state::cache::cache_dir();
        let _ = std::fs::remove_dir_all(cache.join("products"));
        let _ = std::fs::remove_dir_all(cache.join("verify-cache"));
    }
    println!("logged out");
    Ok(cli::exit::OK)
}

async fn whoami(json: bool) -> Result<i32, AppError> {
    let ctx = session_ctx().await?;
    match ctx.session.as_ref().map(|s| s.user_id()) {
        Some(Some(id)) if json => println!("{{\"user_id\":{id}}}"),
        Some(Some(id)) => println!("signed in as user {id}"),
        _ => return Err(AuthError::Unauthorized.into()),
    }
    Ok(cli::exit::OK)
}

async fn session_ctx() -> Result<Arc<AppContext>, AppError> {
    let cfg = load_config_from_env()?;
    let http = gogo::gog::http().map_err(|e| AppError::Io(std::io::Error::other(e.to_string())))?;
    // A6: restore from the configured backend — unavailable keyring fails here,
    // loudly, as class 3 with the switch hint.
    let store = gogo::auth::store::open(cfg.auth.token_store);
    let session = gogo::auth::Session::restore(store.clone()).await?;
    let ctx = AppContext::assemble(cfg, http, Some(Arc::new(session)))
        .map_err(|e| AppError::Io(std::io::Error::other(e.to_string())))?;
    Ok(Arc::new(ctx))
}

// — library / catalog —

fn os_filter_to_catalog(os: Option<OsFilter>) -> Option<gogo::gog::catalog::Os> {
    match os {
        Some(OsFilter::Windows) => Some(gogo::gog::catalog::Os::Windows),
        Some(OsFilter::Mac) => Some(gogo::gog::catalog::Os::Mac),
        Some(OsFilter::Linux) => Some(gogo::gog::catalog::Os::Linux),
        // "all" disables OS filtering at selection time (D3).
        Some(OsFilter::All) | None => None,
    }
}

async fn bearer(ctx: &AppContext) -> Result<String, AppError> {
    Ok(ctx
        .session
        .as_ref()
        .ok_or(AuthError::Unauthorized)?
        .bearer()
        .await?)
}

async fn library_products(
    ctx: &AppContext,
) -> Result<Vec<gogo::gog::library::LibraryProduct>, AppError> {
    let token = bearer(ctx).await?;
    Ok(library::list_games(&ctx.http, &token).await?)
}

async fn list(
    search: Option<String>,
    os: Option<OsFilter>,
    with_dlcs: bool,
    installed_only: bool,
    json: bool,
    refresh: bool,
    color: ColorWhen,
) -> Result<i32, AppError> {
    use gogo::gog::catalog::Product;
    use gogo::state::local::{Built, Local};

    let ctx = session_ctx().await?;
    let mut games = phase(json, "checking the library", library_products(&ctx)).await?;
    // Same normalizer as `search` (design 17 §3), on title + slug.
    if let Some(query) = search.as_deref().and_then(gogo::search::Query::parse) {
        games.retain(|g| {
            query.matches_text(&gogo::search::normalize(&format!(
                "{} {}",
                g.title,
                g.slug.as_deref().unwrap_or_default()
            )))
        });
    }

    // What is set up locally (#378): built apps, depot caches, downloaded installers.
    // Read only; a game filter by `--installed` runs before any catalog fetch.
    let state = ctx.state.lock().unwrap().clone();
    let built_apps = match phase_sync(json, "sizing the games", || {
        gogo::macapp::apps::list(&gogo::macapp::doctor::Host::detect().logs)
    }) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("warn: built apps unreadable: {e}");
            Vec::new()
        }
    };
    let cache_base = gogo::gog::depot::default_cache_dir();
    let locals: std::collections::HashMap<u64, Local> = games
        .iter()
        .map(|g| {
            let local = Local {
                built: built_apps
                    .iter()
                    .find(|a| a.gogid == g.id.to_string())
                    .map(Built::from_app),
                cache: gogo::gog::depot::cache_roots(cache_base.as_deref(), g.id, None)
                    .and_then(|(root, _, _)| gogo::state::local::read_cache(&root)),
                installer: state
                    .installs
                    .iter()
                    .find(|r| r.product_id == g.id && r.dlc_id.is_none())
                    .and_then(|r| r.version.clone()),
            };
            (g.id, local)
        })
        .collect();
    if installed_only {
        games.retain(|g| locals.get(&g.id).is_some_and(Local::any));
    }

    // 03 §1: the library list is always live; catalog data (os, sizes, versions)
    // comes from the product cache, fetched live on a miss or with --refresh.
    // A failed fetch degrades the row (empty os/size), never the command.
    let act = depot_progress::Activity::new(depot_mode(false, json));
    let token = act
        .spin_async("refreshing the sign-in", bearer(&ctx))
        .await?;
    let mut products: Vec<Product> = Vec::with_capacity(games.len());
    let mut fetched = 0u64;
    let missing = if refresh {
        games.len() as u64
    } else {
        games
            .iter()
            .filter(|g| gogo::state::cache::load_typed_product(g.id).is_none())
            .count() as u64
    };
    for game in &games {
        let cached = if refresh {
            None
        } else {
            gogo::state::cache::load_typed_product(game.id)
        };
        let fetch = if cached.is_none() {
            act.counting("fetching game details", "games", fetched, missing);
            fetched += 1;
            Some(gogo::gog::catalog::detail(&ctx.http, &token, game.id).await)
        } else {
            None
        };
        let product = match cached {
            Some(p) => p,
            None => match fetch.expect("fetched when uncached") {
                Ok(p) => {
                    if let Err(e) = gogo::state::cache::save_product(&p) {
                        act.say(&format!("warn: catalog cache write failed: {e}"), &|l| {
                            eprintln!("{l}")
                        });
                    }
                    p
                }
                Err(e) => {
                    act.say(
                        &format!("warn: catalog fetch failed for {}: {e}", game.title),
                        &|l| eprintln!("{l}"),
                    );
                    Product {
                        id: game.id,
                        title: Some(game.title.clone()),
                        slug: game.slug.clone(),
                        game_type: None,
                        downloads: None,
                        expanded_dlcs: Vec::new(),
                    }
                }
            },
        };
        products.push(product);
    }
    drop(act);

    let os_filter = os_filter_to_catalog(os);
    // --os filters on what the catalog says the game delivers; rows with unknown
    // os data (fetch degraded) survive the filter rather than vanishing.
    if let Some(want) = os_filter {
        products.retain(|p| {
            let os_list = product_os(p);
            os_list.is_empty() || os_list.contains(&want)
        });
    }

    let downloaded = |product_id: u64, dlc_id: Option<u64>| {
        state
            .installs
            .iter()
            .find(|r| r.product_id == product_id && r.dlc_id == dlc_id)
            .and_then(|r| r.version.clone())
    };

    let mut rows: Vec<gogo::state::records::ListRow> = Vec::new();
    for product in &products {
        rows.push(gogo::state::records::ListRow {
            id: product.id,
            title: product
                .title
                .clone()
                .unwrap_or_else(|| product.id.to_string()),
            os: product_os(product),
            installers: entry_count(product),
            total_size: product_size(product),
            installed_version: downloaded(product.id, None),
            local: locals.get(&product.id).cloned().unwrap_or_default(),
            depth: 0,
        });
        if with_dlcs {
            for dlc in &product.expanded_dlcs {
                rows.push(gogo::state::records::ListRow {
                    id: dlc.id,
                    title: dlc.title.clone().unwrap_or_else(|| dlc.id.to_string()),
                    os: product_os(dlc),
                    installers: entry_count(dlc),
                    total_size: product_size(dlc),
                    installed_version: downloaded(product.id, Some(dlc.id)),
                    local: Local {
                        installer: downloaded(product.id, Some(dlc.id)),
                        ..Local::default()
                    },
                    depth: 1,
                });
            }
        }
    }

    if json {
        let out: Vec<serde_json::Value> = products
            .iter()
            .map(|p| {
                let mut v = serde_json::json!({
                    "id": p.id,
                    "title": p.title,
                    "slug": p.slug,
                    "os": product_os(p),
                    "installers": entry_count(p),
                    "size": product_size(p),
                    "installed": locals.get(&p.id).is_some_and(Local::any),
                    "local": locals.get(&p.id).cloned().unwrap_or_default(),
                });
                if with_dlcs && !p.expanded_dlcs.is_empty() {
                    v["dlcs"] = serde_json::json!(
                        p.expanded_dlcs
                            .iter()
                            .map(|d| serde_json::json!({
                                "id": d.id,
                                "title": d.title,
                                "os": product_os(d),
                                "size": product_size(d),
                            }))
                            .collect::<Vec<_>>()
                    );
                }
                v
            })
            .collect();
        println!("{}", serde_json::to_string(&out).unwrap());
        return Ok(cli::exit::OK);
    }

    println!("{}", output::list_table(&rows, color));
    Ok(cli::exit::OK)
}

/// `gogo search` (design 17): classify what the account owns, match, print.
async fn search(args: cli::SearchArgs) -> Result<i32, AppError> {
    use gogo::search::{self, Filters, Kind};

    let raw = args.query.join(" ");
    let Some(query) = search::Query::parse(&raw) else {
        eprintln!("gogo: the search query needs at least one letter or digit");
        return Ok(cli::exit::USAGE);
    };
    let kinds = args
        .category
        .iter()
        .map(|c| match c {
            cli::SearchCategory::Games => Kind::Game,
            cli::SearchCategory::Dlc => Kind::Dlc,
            cli::SearchCategory::Packs => Kind::Pack,
            cli::SearchCategory::Goodies => Kind::Goodies,
        })
        .collect();
    let filters = Filters::new(
        kinds,
        args.goodies_type,
        os_filter_to_catalog(args.os),
        args.owned,
    );

    let ctx = session_ctx().await?;
    let act = depot_progress::Activity::new(depot_mode(false, args.json));
    let token = act
        .spin_async("refreshing the sign-in", bearer(&ctx))
        .await?;
    let (lib, warnings) = act
        .spin_async(
            "fetching the library and catalog",
            search::gather::gather(&ctx.http, &token, args.refresh),
        )
        .await?;
    drop(act);
    for w in warnings {
        eprintln!("warn: {w}");
    }
    let state = ctx.state.lock().unwrap().clone();
    let installed = |product_id: u64, dlc_id: Option<u64>| {
        state
            .installs
            .iter()
            .find(|r| r.product_id == product_id && r.dlc_id == dlc_id)
            .and_then(|r| r.version.clone())
    };
    let items = search::classify(&lib, &installed);
    let hits = search::search(&items, &query, &filters);
    let suggestion = if hits.is_empty() {
        search::suggest(&items, &query, &filters)
    } else {
        None
    };

    if args.json {
        println!(
            "{}",
            output::search_json(&raw, &hits, suggestion.as_deref())
        );
    } else if hits.is_empty() {
        match suggestion {
            Some(s) => eprintln!("no matches for \"{raw}\" — did you mean \"{s}\"?"),
            None => eprintln!("no matches for \"{raw}\""),
        }
    } else {
        print!(
            "{}",
            output::search_human(
                &hits,
                !filters.asked(Kind::Goodies),
                !filters.asked(Kind::Dlc)
            )
        );
    }
    Ok(cli::exit::OK)
}

/// Distinct platforms a product (or DLC) delivers, in win → mac → linux order.
fn product_os(product: &gogo::gog::catalog::Product) -> Vec<gogo::gog::catalog::Os> {
    let Some(downloads) = &product.downloads else {
        return Vec::new();
    };
    let mut os = Vec::new();
    let mut push = |entry: &gogo::gog::catalog::InstallerEntry| {
        if let Some(o) = entry.os
            && !os.contains(&o)
        {
            os.push(o);
        }
    };
    for entry in &downloads.installers {
        push(entry);
    }
    for entry in downloads
        .bonus_content
        .iter()
        .chain(&downloads.language_packs)
    {
        push(entry);
    }
    use gogo::gog::catalog::Os;
    os.sort_by_key(|o| match o {
        Os::Windows => 0,
        Os::Mac => 1,
        Os::Linux => 2,
    });
    os
}

/// Deliverable-entry count (installers for games; bonus content counts for DLCs).
fn entry_count(product: &gogo::gog::catalog::Product) -> u32 {
    let Some(downloads) = &product.downloads else {
        return 0;
    };
    (downloads.installers.len() + downloads.bonus_content.len()) as u32
}

/// Sum of entry sizes (whole entries — multi-part totals live on the entry).
fn product_size(product: &gogo::gog::catalog::Product) -> u64 {
    let Some(downloads) = &product.downloads else {
        return 0;
    };
    downloads
        .installers
        .iter()
        .chain(&downloads.bonus_content)
        .filter_map(|e| e.total_size)
        .sum()
}

async fn info(game: String, json: bool) -> Result<i32, AppError> {
    let ctx = session_ctx().await?;
    let act = depot_progress::Activity::new(depot_mode(false, json));
    let (token, products) = act
        .spin_async("checking the library", async {
            let token = bearer(ctx.as_ref()).await?;
            let products = resolve_games(&ctx, &token, &[game], &act).await?;
            Ok::<_, AppError>((token, products))
        })
        .await?;
    let Some(product) = products.first() else {
        drop(act);
        eprintln!("no such game");
        return Ok(cli::exit::USAGE);
    };
    // Fresh detail (downloads + DLC tree).
    let detail = act
        .spin_async(
            "fetching the game details",
            gogo::gog::catalog::detail(&ctx.http, &token, product.id),
        )
        .await?;
    drop(act);
    if json {
        println!("{}", serde_json::to_string_pretty(&detail).unwrap());
    } else {
        println!(
            "{} (id {})",
            detail.title.clone().unwrap_or_default(),
            detail.id
        );
        let dl = detail.downloads.as_ref();
        println!(
            "  installers: {}  patches: {}  extras: {}  dlcs: {}",
            dl.map(|d| d.installers.len()).unwrap_or(0),
            dl.map(|d| d.patches.len()).unwrap_or(0),
            dl.map(|d| d.bonus_content.len()).unwrap_or(0),
            detail.expanded_dlcs.len(),
        );
    }
    Ok(cli::exit::OK)
}

/// Game selectors: id, slug, or title substring (03 §3). A DLC id/slug resolves to its
/// parent game with a notice (A1) — the library list is games-only, so DLC selectors
/// can only ever match a parent by fuzzy title; exact DLC ids are usage errors here.
async fn resolve_games(
    ctx: &AppContext,
    token: &str,
    names: &[String],
    act: &depot_progress::Activity,
) -> Result<Vec<gogo::gog::library::LibraryProduct>, AppError> {
    let lib = library::list_games(&ctx.http, token).await?;
    let mut out = Vec::new();
    for name in names {
        let lower = name.to_lowercase();
        let found = lib
            .iter()
            .find(|g| g.id.to_string() == *name)
            .or_else(|| {
                lib.iter()
                    .find(|g| g.slug.as_deref() == Some(name.as_str()))
            })
            .or_else(|| lib.iter().find(|g| g.title.to_lowercase().contains(&lower)));
        match found {
            Some(game) => out.push(game.clone()),
            None => {
                act.say(
                    &format!("no game matches `{name}` — try `gogo list`"),
                    &|l| eprintln!("{l}"),
                );
                return Ok(Vec::new());
            }
        }
    }
    Ok(out)
}

// — download —

/// `gogo depot <GAME>` — Model B lane (docs/design/14): resolve the latest
/// public Windows build and download it into the user cache directory
/// (macOS and Linux). On Linux the plan also says how a build will
/// materialize it (design 18 §6).
async fn depot_cmd(args: cli::DepotArgs) -> Result<i32, AppError> {
    use gogo::gog::depot::{DepotEvent, DepotOptions};

    let built = AppContext::build(load_config_from_env()?, true)
        .await
        .map_err(|e| {
            eprintln!("gogo: {e}");
            AppError::Usage
        })?;
    let ctx = std::sync::Arc::new(built);
    let machine = args.json || args.json_progress;

    // One spinner per preflight phase (#422), the lane's renderer after.
    let act = depot_progress::Activity::new(depot_mode(args.json_progress, args.json));
    let token = act
        .spin_async("refreshing the sign-in", bearer(&ctx))
        .await?;
    let session = ctx
        .session
        .as_ref()
        .and_then(|p| p.gog_warp_core())
        .ok_or(AppError::Usage)?;
    let _ = token; // bearer() above validates the session cheaply

    let selected = act
        .spin_async(
            "checking the library",
            resolve_games(&ctx, &token, std::slice::from_ref(&args.game), &act),
        )
        .await?;
    let Some(game) = selected.first() else {
        drop(act);
        eprintln!("gogo: no owned game matches {}", args.game);
        return Ok(cli::exit::USAGE);
    };

    if args.list_languages {
        return list_languages(&ctx, &token, session, game, &args, &act).await;
    }
    let language = args
        .language
        .clone()
        .unwrap_or_else(|| gogo::gog::language::DEFAULT.to_string());
    let language_explicit = args.language.is_some();
    // `--build`: resolve it first, so an unknown or ambiguous value stops the run
    // before any DLC lookup or download (exit 2, with the builds listed).
    let picked = match &args.build {
        Some(value) => {
            let listing = act
                .spin_async(
                    "fetching the builds list",
                    gogo::gog::builds::fetch_listing(
                        session,
                        game.id,
                        gog_warp::content_system::types::Platform::Windows,
                        args.password.clone(),
                    ),
                )
                .await
                .map_err(gogo::gog::depot::DepotError::library)?;
            let b = gogo::gog::builds::pick_for_run(
                &listing,
                value,
                &gogo::gog::builds::ResolveCtx {
                    title: &game.title,
                    installed: None,
                },
            )
            .map_err(gogo::gog::depot::DepotError::from)?;
            let lines = gogo::gog::builds::plan_lines(&listing, b, true, None);
            if !machine {
                for l in &lines {
                    act.say(&format!("gogo: build: {l}"), &|l| eprintln!("{l}"));
                }
            }
            Some(gogo::gog::builds::BuildEntry::from_build(b))
        }
        None => None,
    };
    let limit = match &args.limit {
        Some(spec) => Some(gogo::config::parse_rate(spec)?),
        None => None,
    };
    let (dlcs, dlc_context) = if args.dlc.is_empty() && !args.all_dlcs {
        (Default::default(), Default::default())
    } else {
        let (detail, owned) = act
            .spin_async("checking the game's DLC", async {
                let detail = gogo::gog::catalog::detail(&ctx.http, &token, game.id).await?;
                let owned = library::owned_ids(&ctx.http, &token).await?;
                Ok::<_, AppError>((detail, owned))
            })
            .await?;
        let dlcs = if args.all_dlcs {
            gogo::gog::dlc::DlcSet::AllOwned
        } else {
            let (build_id, mdlcs) = act
                .spin_async(
                    "fetching the builds list",
                    gogo::gog::depot::build_dlcs(
                        session,
                        game.id,
                        &game.title,
                        args.password.clone(),
                        picked.as_ref().map(|p| p.build_id.as_str()),
                        &language,
                        language_explicit,
                    ),
                )
                .await?;
            // A DLC the picked build lacks names the build by its version too.
            let build_id = picked.as_ref().map_or(build_id, |p| p.label());
            let infos = gogo::gog::dlc::available(&detail.expanded_dlcs, &owned, Some(&mdlcs));
            let lane = gogo::gog::dlc::Lane::Depot { build_id };
            match gogo::gog::dlc::resolve(&args.dlc, &game.title, &infos, &lane) {
                Ok(ids) => gogo::gog::dlc::DlcSet::Ids(ids),
                Err(e) => {
                    drop(act);
                    eprintln!("gogo: {e}");
                    return Ok(cli::exit::USAGE);
                }
            }
        };
        (
            dlcs,
            gogo::gog::depot::DlcContext {
                owned,
                catalog_dlcs: detail.expanded_dlcs,
            },
        )
    };
    let opts = DepotOptions {
        language,
        language_explicit,
        dlcs,
        dlc_context,
        password: args.password.clone(),
        build: picked.as_ref().map(|p| p.build_id.clone()),
        dir: args.dir.clone(),
        // Only the plan's cost line uses it: a bad one is said (on stderr) and
        // doesn't stop a download. A build refuses it.
        library: resolve_library(None).unwrap_or(None),
        limit,
        disk_check: !args.no_disk_check,
        dry_run: args.dry_run,
    };

    let (tx, mut raw_rx) = tokio::sync::mpsc::unbounded_channel::<DepotEvent>();
    // Pass events on to the renderer, noting the plan's DLC for `--json`.
    let (fwd_tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<DepotEvent>();
    let plan_dlcs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let plan_seen = plan_dlcs.clone();
    let forward = tokio::spawn(async move {
        while let Some(e) = raw_rx.recv().await {
            if let DepotEvent::Plan { dlcs, .. } = &e {
                *plan_seen.lock().unwrap() = dlcs.clone();
            }
            if fwd_tx.send(e).is_err() {
                break;
            }
        }
    });
    // Ctrl-C → cancel token; the depot driver links it to the downloader.
    let cancel_src = ctx.cancel.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        cancel_src.cancel();
    });

    let title = game.title.clone();
    let runner = gogo::gog::depot::run(
        session,
        game.id,
        &title,
        &opts,
        &tx,
        &ctx.cancel,
        ctx.free_space.as_ref(),
    );
    let result = depot_progress::drive(runner, &mut rx, &act).await;
    drop(tx);
    let _ = forward.await;
    let install_dir = result?;

    if args.json {
        let summary = serde_json::json!({
            "productId": game.id,
            "title": title,
            "installDir": install_dir,
            "dryRun": args.dry_run,
            "dlcs": &*plan_dlcs.lock().unwrap(),
            "build": picked.as_ref().map(|p| serde_json::json!({
                "buildId": p.build_id,
                "version": p.version,
                "branch": p.branch,
            })),
        });
        println!("{}", serde_json::to_string(&summary).unwrap());
    } else if !machine {
        // --json-progress keeps stdout pure NDJSON
        println!(
            "gogo: {} {}",
            if args.dry_run {
                "planned"
            } else {
                "downloaded to"
            },
            install_dir.display()
        );
    }
    Ok(cli::exit::OK)
}

/// `gogo depot <GAME> --list-languages` (design 14 §11): the newest build's languages
/// from its build manifest. Reads only: no depot manifest, no cache change.
async fn list_languages(
    ctx: &AppContext,
    token: &str,
    session: &gog_warp::Core,
    game: &gogo::gog::library::LibraryProduct,
    args: &cli::DepotArgs,
    act: &depot_progress::Activity,
) -> Result<i32, AppError> {
    use gogo::gog::language;
    let build = act
        .spin_async(
            "reading the build's languages",
            gogo::gog::depot::newest_build(session, game.id, args.password.clone()),
        )
        .await?;
    let langs = language::of_manifest(&build.manifest);
    let cached = gogo::gog::depot::cache_roots(
        gogo::gog::depot::default_cache_dir().as_deref(),
        game.id,
        args.dir.as_deref(),
    )
    .and_then(|(root, _, _)| gogo::gog::depot::read_record(&root))
    // A cache of another build still says which language it was asked for.
    .and_then(|r| r.language);
    let listing = language::listing(&langs, cached.as_deref());
    // DLC titles are for the notes only: the listing stands without them.
    let titles = if listing.dlcs_with_fewer.is_empty() {
        Vec::new()
    } else {
        act.spin_async(
            "fetching the game details",
            gogo::gog::catalog::detail(&ctx.http, token, game.id),
        )
        .await
        .map(|d| d.expanded_dlcs)
        .unwrap_or_default()
    };
    // Nothing is printed while a spinner is up: the result comes after.
    act.finish();
    let title_of = |id: &str| {
        titles
            .iter()
            .find(|p| p.id.to_string() == id)
            .and_then(|p| p.title.clone())
    };
    if args.json {
        let mut v = serde_json::to_value(&listing).unwrap();
        v["productId"] = serde_json::json!(game.id);
        v["title"] = serde_json::json!(game.title);
        v["buildId"] = serde_json::json!(build.build_id);
        v["version"] = serde_json::json!(build.version);
        for d in v["dlcsWithFewer"].as_array_mut().into_iter().flatten() {
            if let Some(t) = d["id"].as_str().and_then(title_of) {
                d["title"] = serde_json::json!(t);
            }
        }
        println!("{}", serde_json::to_string(&v).unwrap());
    } else {
        println!(
            "{} build {} (generation {})",
            game.title, build.version, build.generation
        );
        print!("{}", language::render(&listing, &title_of));
    }
    Ok(cli::exit::OK)
}

async fn download(
    args: cli::DownloadArgs,
    no_progress: bool,
    quiet: bool,
) -> Result<i32, AppError> {
    let cli::DownloadArgs {
        games,
        all,
        yes,
        os,
        language,
        dlc,
        all_dlcs,
        extras,
        patches,
        lang_packs,
        dir,
        workers,
        parallel_files,
        limit,
        force,
        no_disk_check,
        json_progress,
        json,
        dry_run,
    } = args;
    let mut cfg = load_config_from_env()?;

    // 05 §5 CLI overlay: Some(_) wins over every other layer.
    if let Some(dir) = &dir {
        cfg.download.dir = Some(dir.clone());
    }
    if let Some(workers) = workers {
        cfg.download.workers = workers;
    }
    if let Some(parallel) = parallel_files {
        cfg.download.parallel_files = parallel;
    }
    if let Some(limit) = &limit {
        cfg.download.limit = gogo::config::parse_rate(limit)?;
    }
    if let Some(language) = &language {
        cfg.gog.language = language.clone();
    }

    let http = gogo::gog::http().map_err(|e| AppError::Io(std::io::Error::other(e.to_string())))?;
    let store = gogo::auth::store::open(load_config_from_env()?.auth.token_store);
    let session = gogo::auth::Session::restore(store.clone()).await?;
    let mut built = AppContext::assemble(cfg, http, Some(Arc::new(session)))
        .map_err(|e| AppError::Io(std::io::Error::other(e.to_string())))?;
    if no_disk_check {
        // D4: --no-disk-check skips the preflight.
        built.disk_check = false;
    }
    let ctx = Arc::new(built);
    // 03 §4 progress consumers: NDJSON on stdout for --json-progress, indicatif
    // bars on stderr for humans with a TTY; nothing on --no-progress/--quiet or
    // pipes. Subscribed BEFORE planning so PlanReady is captured (dry-run too).
    let machine = json || json_progress;
    let bars_on =
        !machine && !no_progress && !quiet && std::io::IsTerminal::is_terminal(&std::io::stderr());
    let rx = if json_progress || bars_on {
        Some(ctx.events.subscribe())
    } else {
        None
    };
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

    // One spinner per preflight phase (#422); off a terminal, and under the
    // machine modes, it prints nothing.
    let act = depot_progress::Activity::new(depot_mode(json_progress, json));
    let (token, selected) = act
        .spin_async("checking the library", async {
            let token = bearer(&ctx).await?;
            let selected = if all {
                library::list_games(&ctx.http, &token).await?
            } else {
                resolve_games(&ctx, &token, &games, &act).await?
            };
            Ok::<_, AppError>((token, selected))
        })
        .await?;
    if selected.is_empty() && !all {
        return Ok(cli::exit::USAGE);
    }

    // Content filters (03 §3): installers by default; flags add kinds; --os all clears.
    let mut kinds = Vec::new();
    if extras {
        kinds.push(Kind::BonusContent);
    }
    if patches {
        kinds.push(Kind::Patch);
    }
    if lang_packs {
        kinds.push(Kind::LanguagePack);
    }
    let os = os_filter_to_catalog(os).or_else(host_or_configured_os);
    let mut filters = Filters {
        os,
        language: Some(ctx.cfg.gog.language.clone()),
        kinds,
        with_dlcs: all_dlcs || !dlc.is_empty(),
        only_dlcs: None,
    };

    let mut products = Vec::new();
    for (i, game) in selected.iter().enumerate() {
        act.counting(
            "fetching game details",
            "games",
            i as u64,
            selected.len() as u64,
        );
        let detail = gogo::gog::catalog::detail(&ctx.http, &token, game.id).await?;
        products.push(detail);
    }
    act.done();
    // Owned DLC only (design 19 §1.1): `--all-dlcs` takes the owned ones, `--dlc`
    // names them; a name that is unknown or unowned is a usage error.
    if filters.with_dlcs {
        if !dlc.is_empty() && products.len() != 1 {
            eprintln!("gogo: --dlc needs exactly one game");
            return Ok(cli::exit::USAGE);
        }
        let owned = act
            .spin_async(
                "checking the owned DLC",
                library::owned_ids(&ctx.http, &token),
            )
            .await?;
        let mut only = Vec::new();
        for (game, product) in selected.iter().zip(&products) {
            let infos = gogo::gog::dlc::available(&product.expanded_dlcs, &owned, None);
            let lane = gogo::gog::dlc::Lane::Installer;
            let ids = if all_dlcs {
                gogo::gog::dlc::select(
                    &gogo::gog::dlc::DlcSet::AllOwned,
                    &game.title,
                    &infos,
                    &lane,
                )
            } else {
                gogo::gog::dlc::resolve(&dlc, &game.title, &infos, &lane)
            };
            match ids {
                Ok(ids) => only.extend(ids),
                Err(e) => {
                    eprintln!("gogo: {e}");
                    return Ok(cli::exit::USAGE);
                }
            }
        }
        filters.only_dlcs = Some(only);
    }

    let inner = async {
        let tasks = act
            .spin_async(
                "planning the download",
                engine::plan(&ctx, &products, &filters, force),
            )
            .await;
        act.finish();
        if machine {
            print_plan_json(&tasks);
        } else {
            print_plan(&tasks);
        }

        if dry_run {
            return Ok(None);
        }

        // B1: `--all` without `--yes` requires a TTY confirmation; non-TTY aborts
        // with the plan printed (exit 2) — a bulk run never starts silently.
        if all && !yes {
            if console::Term::stdout().features().is_attended() {
                let confirmed = crate::depot_progress::prompting(|| {
                    dialoguer::Confirm::new()
                        .with_prompt("download the plan above?")
                        .default(false)
                        .interact()
                })
                .unwrap_or(false);
                if !confirmed {
                    println!("aborted");
                    return Ok(None);
                }
            } else {
                eprintln!("gogo: `--all` needs `--yes` when not attached to a terminal");
                return Err(AppError::Usage);
            }
        }

        // Ctrl-C → cancel token; workers stop at chunk boundaries (exit 130).
        let cancel_src = ctx.cancel.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            cancel_src.cancel();
        });

        Ok(Some(engine::execute(&ctx, tasks).await))
    };
    // Every exit path (including `?` short-circuits above) releases the renderer.
    let work = async {
        let result = inner.await;
        let _ = done_tx.send(());
        result
    };
    let summary = match (rx, bars_on) {
        (Some(rx), false) => {
            let (result, ()) = tokio::join!(work, render_ndjson(rx, done_rx));
            result?
        }
        (Some(rx), true) => {
            let (result, ()) = tokio::join!(work, render_bars(rx, done_rx));
            result?
        }
        (None, _) => work.await?,
    };
    let Some(summary) = summary else {
        return Ok(cli::exit::OK); // dry-run / aborted
    };
    if machine {
        println!(
            "{}",
            serde_json::json!({"ok": summary.ok, "failed": summary.failed, "skipped": summary.skipped})
        );
    } else {
        println!(
            "done: {} ok, {} skipped, {} failed",
            summary.ok, summary.skipped, summary.failed
        );
    }
    if ctx.cancel.is_cancelled() {
        return Ok(cli::exit::SIGINT);
    }
    // C1: batch exits with the highest failed class.
    Ok(summary
        .worst_exit_class
        .map(i32::from)
        .unwrap_or(cli::exit::OK))
}

fn host_or_configured_os() -> Option<gogo::gog::catalog::Os> {
    use gogo::config::Platform;
    match gogo::config::Config::default().gog.platform {
        Platform::Windows => Some(gogo::gog::catalog::Os::Windows),
        Platform::Mac => Some(gogo::gog::catalog::Os::Mac),
        Platform::Linux => Some(gogo::gog::catalog::Os::Linux),
        Platform::Current | Platform::All => Some(if cfg!(target_os = "macos") {
            gogo::gog::catalog::Os::Mac
        } else if cfg!(target_os = "linux") {
            gogo::gog::catalog::Os::Linux
        } else {
            gogo::gog::catalog::Os::Windows
        }),
    }
}

/// `--json`/`--json-progress` plan summary — same shape as the PlanReady event.
fn print_plan_json(tasks: &[gogo::engine::task::FileTask]) {
    use gogo::engine::task::Status;
    let files = tasks
        .iter()
        .filter(|t| !matches!(t.status, Status::Present))
        .count() as u32;
    let total_bytes = tasks
        .iter()
        .filter(|t| !matches!(t.status, Status::Present))
        .map(|t| t.total_size)
        .sum::<u64>();
    let skipped_bytes = tasks
        .iter()
        .filter(|t| matches!(t.status, Status::Present))
        .map(|t| t.total_size)
        .sum::<u64>();
    println!(
        "{}",
        serde_json::json!({"files": files, "total_bytes": total_bytes, "skipped_bytes": skipped_bytes})
    );
}

/// NDJSON progress on stdout: one externally-tagged JSON object per line,
/// flushed per event (03 §4).
async fn render_ndjson(
    rx: tokio::sync::broadcast::Receiver<gogo::engine::events::ProgressEvent>,
    done: tokio::sync::oneshot::Receiver<()>,
) {
    use std::io::Write;
    gogo::engine::events::render(rx, done, |ev| {
        let mut out = std::io::stdout().lock();
        if serde_json::to_writer(&mut out, ev).is_ok() {
            let _ = out.write_all(b"\n");
        }
        let _ = out.flush();
    })
    .await;
}

/// indicatif bars on stderr: one bar per in-flight file (03 §1 example shape).
async fn render_bars(
    rx: tokio::sync::broadcast::Receiver<gogo::engine::events::ProgressEvent>,
    done: tokio::sync::oneshot::Receiver<()>,
) {
    use gogo::engine::events::ProgressEvent;
    use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
    use std::collections::HashMap;
    use uuid::Uuid;

    let _mp = MultiProgress::new();
    let mut bars: HashMap<Uuid, ProgressBar> = HashMap::new();
    let style = ProgressStyle::with_template(
        "{spinner:.green} {msg} {bytes}/{total_bytes} ({bytes_per_sec}, eta {eta})",
    )
    .expect("static indicatif template");
    let target = _mp.clone();
    gogo::engine::events::render(rx, done, move |ev| match ev {
        ProgressEvent::FileStart {
            id,
            name,
            total_bytes,
        } => {
            let bar = target.add(ProgressBar::new(*total_bytes));
            bar.set_style(style.clone());
            bar.set_message(name.clone());
            bars.insert(*id, bar);
        }
        ProgressEvent::ChunkDone { id, file_bytes, .. } => {
            // D2: file_bytes is cumulative — no delta accumulation needed.
            if let Some(bar) = bars.get(id) {
                bar.set_position(*file_bytes);
            }
        }
        ProgressEvent::FileDone { id, .. } => {
            if let Some(bar) = bars.remove(id) {
                bar.finish_with_message("done");
            }
        }
        ProgressEvent::FileFailed { id, error } => {
            if let Some(bar) = bars.remove(id) {
                bar.finish_with_message(format!("failed: {error}"));
            }
        }
        ProgressEvent::FileSkipped { reason, .. } => {
            let _ = target.println(format!("skipped: {reason}"));
        }
        ProgressEvent::Warn { message } => {
            let _ = target.println(format!("warn: {message}"));
        }
        ProgressEvent::PlanReady { .. } | ProgressEvent::Done { .. } => {}
        // mac-app events never flow through the download bars renderer.
        _ => {}
    })
    .await;
}

/// [`engine::execute`] with the download bars on a terminal, as `download`
/// shows them (#422); off one, or under `--json`/`--quiet`, no subscriber.
async fn execute_rendered(
    ctx: &Arc<AppContext>,
    tasks: Vec<gogo::engine::task::FileTask>,
    json: bool,
) -> gogo::engine::Summary {
    let live = depot_mode(false, json) == depot_progress::Mode::Human { live: true };
    with_progress(&ctx.events, live, engine::execute(ctx, tasks), render_bars).await
}

/// `work` with `render` consuming the engine's events while it runs, when
/// `live`; subscribed before `work` starts, so nothing it sends is missed.
async fn with_progress<T, R: std::future::Future<Output = ()>>(
    events: &tokio::sync::broadcast::Sender<gogo::engine::events::ProgressEvent>,
    live: bool,
    work: impl std::future::Future<Output = T>,
    render: impl FnOnce(
        tokio::sync::broadcast::Receiver<gogo::engine::events::ProgressEvent>,
        tokio::sync::oneshot::Receiver<()>,
    ) -> R,
) -> T {
    if !live {
        return work.await;
    }
    let rx = events.subscribe();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    let work = async {
        let out = work.await;
        let _ = done_tx.send(());
        out
    };
    let (out, ()) = tokio::join!(work, render(rx, done_rx));
    out
}

fn print_plan(tasks: &[gogo::engine::task::FileTask]) {
    use gogo::engine::task::Status;
    let mut total = 0u64;
    for task in tasks {
        let status = match task.status {
            Status::New => "new",
            Status::Resumable => "resume",
            Status::Present => "present",
            Status::Outdated => "update",
            Status::PendingAdoption => "adopt?",
        };
        let name = task
            .entry
            .name
            .clone()
            .unwrap_or_else(|| task.file.id.clone());
        // Multi-part installers split into several `files[]` — show the part id.
        let part = if task.entry.files.len() > 1 {
            format!(" [{}]", task.file.id)
        } else {
            String::new()
        };
        println!(
            "  {status:<8} {:<12} {}{} ({})",
            task.kind.as_str(),
            name,
            part,
            gogo::fmt::human_size(task.total_size)
        );
        if !matches!(task.status, Status::Present) {
            total += task.total_size;
        }
    }
    println!("  total: {}", gogo::fmt::human_size(total));
}

// — update: plan, keep only outdated tasks, execute —

async fn update(games: Vec<String>, all: bool, strict: bool) -> Result<i32, AppError> {
    let ctx = session_ctx().await?;
    let act = depot_progress::Activity::new(depot_mode(false, false));
    let (token, selected) = act
        .spin_async("checking the library", async {
            let token = bearer(&ctx).await?;
            let selected = if all {
                library::list_games(&ctx.http, &token).await?
            } else {
                resolve_games(&ctx, &token, &games, &act).await?
            };
            Ok::<_, AppError>((token, selected))
        })
        .await?;
    if selected.is_empty() {
        return Ok(cli::exit::USAGE);
    }
    let filters = Filters {
        os: host_or_configured_os(),
        language: Some(ctx.cfg.gog.language.clone()),
        kinds: Vec::new(),
        with_dlcs: false,
        only_dlcs: None,
    };
    let mut products = Vec::new();
    for (i, game) in selected.iter().enumerate() {
        act.counting(
            "fetching game details",
            "games",
            i as u64,
            selected.len() as u64,
        );
        products.push(gogo::gog::catalog::detail(&ctx.http, &token, game.id).await?);
    }
    let mut tasks = act
        .spin_async(
            "planning the update",
            engine::plan(&ctx, &products, &filters, false),
        )
        .await;
    // 06 §4 strict path: version-equal files may still have drifted — compare
    // checksum timestamp (primary) and build id via one resolution each.
    if strict {
        act.working("checking for drift");
        for task in tasks
            .iter_mut()
            .filter(|t| matches!(t.status, gogo::engine::task::Status::Present))
        {
            if engine::strict_outdated(&ctx, &token, task).await {
                task.status = gogo::engine::task::Status::Outdated;
            }
        }
    }
    act.finish();
    let tasks: Vec<_> = tasks
        .into_iter()
        .filter(|t| matches!(t.status, gogo::engine::task::Status::Outdated))
        .collect();
    if tasks.is_empty() {
        println!("everything up to date");
        return Ok(cli::exit::OK);
    }
    print_plan(&tasks);
    drop(act);
    let summary = execute_rendered(&ctx, tasks, false).await;
    println!(
        "updated: {} ok, {} skipped, {} failed",
        summary.ok, summary.skipped, summary.failed
    );
    Ok(summary
        .worst_exit_class
        .map(i32::from)
        .unwrap_or(cli::exit::OK))
}

// — verify: local re-check of recorded files against the verify-cache (A5) —

/// [`gogo::engine::verify_against`] with the bytes hashed on the live status
/// line (#420); the line is cleared before the caller prints the verdict.
fn verify_live(
    act: &depot_progress::Activity,
    path: &std::path::Path,
    doc: &gogo::gog::checksum::ChecksumDoc,
) -> bool {
    let step = format!(
        "hashing {}",
        path.file_name().unwrap_or_default().to_string_lossy()
    );
    let total = Some(doc.total_size);
    let ok = gogo::engine::verify_against_with(path, doc, &mut |b| {
        act.task(&step, None, Some((b, total)))
    });
    act.done();
    ok
}

async fn verify(
    games: Vec<String>,
    all: bool,
    path: Vec<std::path::PathBuf>,
    fix: bool,
    rebuild: bool,
) -> Result<i32, AppError> {
    // A5: verify is offline-first — only --fix (which re-downloads) needs a session.
    let cfg = load_config_from_env()?;
    let root = std::path::PathBuf::from(cfg.download.dir.clone().unwrap_or_default());

    if !path.is_empty() {
        return verify_paths(&path);
    }
    if rebuild {
        return rebuild_records(&cfg, &root);
    }

    if games.is_empty() && !all {
        eprintln!("gogo: verify needs a game, --all, --path, or --rebuild");
        return Ok(cli::exit::USAGE);
    }
    let state = gogo::state::State::load().unwrap_or_default();
    let mut records: Vec<gogo::state::records::InstallRecord> = state
        .installs
        .iter()
        .filter(|r| {
            all || games.iter().any(|g| {
                g == &r.product_id.to_string()
                    || r.product_title
                        .as_deref()
                        .is_some_and(|t| t.to_lowercase().contains(&g.to_lowercase()))
            })
        })
        .cloned()
        .collect();
    records.sort_by(|a, b| a.path.cmp(&b.path));

    let mut failed_paths: Vec<gogo::state::records::InstallRecord> = Vec::new();
    let act = depot_progress::Activity::new(depot_mode(false, false));
    let mut missing: Vec<gogo::state::records::InstallRecord> = Vec::new();
    let mut ok = 0;
    for record in &records {
        let doc =
            gogo::state::cache::load_checksum(record.product_id, record.dlc_id, &record.file_id)
                .and_then(|xml| gogo::gog::checksum::parse(&xml).ok());
        let Some(doc) = doc else {
            missing.push(record.clone());
            continue;
        };
        let dest = root.join(&record.path);
        if verify_live(&act, &dest, &doc) {
            println!("  ok {}", record.path.display());
            ok += 1;
        } else {
            println!("  FAIL {}", record.path.display());
            failed_paths.push(record.clone());
        }
    }

    // 03 §1: missing docs are re-resolved from the API when a session is at hand;
    // offline (no keychain answer / no network) they stay skipped with `?` (A5).
    if !missing.is_empty() {
        if let Ok(ctx) = session_ctx().await
            && let Ok(token) = bearer(&ctx).await
        {
            for record in &missing {
                if let Some(doc) = re_resolve_doc(&ctx, &token, record).await {
                    let dest = root.join(&record.path);
                    if verify_live(&act, &dest, &doc) {
                        println!("  ok {} (re-resolved)", record.path.display());
                        ok += 1;
                    } else {
                        println!("  FAIL {} (re-resolved)", record.path.display());
                        failed_paths.push(record.clone());
                    }
                } else {
                    println!("  ? {} (no checksum — skipped)", record.path.display());
                }
            }
        } else {
            eprintln!(
                "warn: offline — {} files skipped (no session)",
                missing.len()
            );
        }
    }

    // --fix: re-download the corrupt files through the normal pipeline (03 §1).
    let mut fixed = 0u32;
    let mut fix_class: Option<i32> = None;
    if fix && !failed_paths.is_empty() {
        let ctx = session_ctx().await?;
        let act = depot_progress::Activity::new(depot_mode(false, false));
        let token = act.spin_async("checking the library", bearer(&ctx)).await?;
        let product_ids: Vec<u64> = {
            let mut ids: Vec<u64> = failed_paths.iter().map(|r| r.product_id).collect();
            ids.sort_unstable();
            ids.dedup();
            ids
        };
        let mut products = Vec::new();
        for (i, id) in product_ids.iter().enumerate() {
            act.counting(
                "fetching game details",
                "games",
                i as u64,
                product_ids.len() as u64,
            );
            match gogo::gog::catalog::detail(&ctx.http, &token, *id).await {
                Ok(p) => products.push(p),
                Err(e) => act.say(&format!("warn: catalog fetch failed for {id}: {e}"), &|l| {
                    eprintln!("{l}")
                }),
            }
        }
        act.done();
        let filters = Filters {
            os: None,
            language: Some(ctx.cfg.gog.language.clone()),
            kinds: Vec::new(),
            // Corrupt DLC files need their DLC planned too (A1: DLCs via parent).
            with_dlcs: true,
            only_dlcs: None,
        };
        // force=true: bypass the present-skip so the corrupt file is replaced.
        let tasks: Vec<_> = act
            .spin_async(
                "planning the repair",
                engine::plan(&ctx, &products, &filters, true),
            )
            .await
            .into_iter()
            .filter(|t| {
                failed_paths.iter().any(|r| {
                    r.product_id == t.product_id && r.dlc_id == t.dlc_id && r.file_id == t.file.id
                })
            })
            .collect();
        drop(act);
        if tasks.is_empty() {
            eprintln!("gogo: --fix found no downloadable replacement for the corrupt files");
        } else {
            let summary = execute_rendered(&ctx, tasks, false).await;
            fixed = summary.ok;
            println!(
                "fixed: {} ok, {} skipped, {} failed",
                summary.ok, summary.skipped, summary.failed
            );
            fix_class = summary.worst_exit_class.map(i32::from);
        }
    }

    let failed = failed_paths.len() as u32;
    println!("{ok} verified, {failed} failed, {fixed} fixed");
    // C1 ordering: a still-corrupt file (fix failed or not requested) is 5;
    // otherwise the fix run's own worst class decides.
    if failed > fixed {
        return Ok(cli::exit::CORRUPT);
    }
    Ok(if failed > 0 {
        fix_class.unwrap_or(cli::exit::OK)
    } else {
        cli::exit::OK
    })
}

/// `verify --path`: arbitrary files against the verify-cache, matched by the
/// checksum doc's `name` — offline, no records touched (03 §1).
fn verify_paths(paths: &[std::path::PathBuf]) -> Result<i32, AppError> {
    let index = checksum_index();
    let act = depot_progress::Activity::new(depot_mode(false, false));

    let mut ok = 0;
    let mut failed = 0;
    let mut unknown = 0;
    for path in paths {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        match index.get(&name) {
            None => {
                println!("  ? {} (no cached checksum)", path.display());
                unknown += 1;
            }
            Some((_, _, _, doc)) => {
                if verify_live(&act, path, doc) {
                    println!("  ok {}", path.display());
                    ok += 1;
                } else {
                    println!("  FAIL {}", path.display());
                    failed += 1;
                }
            }
        }
    }
    println!("{ok} verified, {failed} failed, {unknown} unknown");
    Ok(if failed > 0 {
        cli::exit::CORRUPT
    } else {
        cli::exit::OK
    })
}

/// Fetch a record's checksum doc from the API (via the cached product's
/// downlink), refreshing the A5 verify-cache. `None` = not resolvable now.
async fn re_resolve_doc(
    ctx: &Arc<AppContext>,
    token: &str,
    record: &gogo::state::records::InstallRecord,
) -> Option<gogo::gog::checksum::ChecksumDoc> {
    let product = gogo::state::cache::load_product(record.product_id)
        .ok()
        .flatten()?;
    let scopes: Vec<&gogo::gog::catalog::Downloads> = match record.dlc_id {
        None => product.downloads.iter().collect(),
        Some(dlc) => product
            .expanded_dlcs
            .iter()
            .find(|d| d.id == dlc)
            .map(|d| d.downloads.iter())
            .into_iter()
            .flatten()
            .collect(),
    };
    let downlink = scopes.iter().find_map(|downloads| {
        downloads
            .installers
            .iter()
            .chain(&downloads.patches)
            .chain(&downloads.bonus_content)
            .chain(&downloads.language_packs)
            .find(|e| e.id == record.file_id || e.files.iter().any(|f| f.id == record.file_id))?
            .files
            .iter()
            .find(|f| f.id == record.file_id)
            .map(|f| f.downlink.clone())
    })?;
    let dl = gogo::gog::downlink::resolve(&ctx.http, token, &downlink)
        .await
        .ok()?;
    let raw = gogo::gog::checksum::fetch_raw(&ctx.http, &dl.checksum)
        .await
        .ok()?;
    let _ =
        gogo::state::cache::save_checksum(record.product_id, record.dlc_id, &record.file_id, &raw);
    gogo::gog::checksum::parse(&raw).ok()
}

/// Index the verify-cache by the checksum doc's `name` attribute:
/// `name → (product_id, dlc_id, file_id, ChecksumDoc)` (A5 keying, offline).
fn checksum_index()
-> std::collections::HashMap<String, (u64, Option<u64>, String, gogo::gog::checksum::ChecksumDoc)> {
    let mut index = std::collections::HashMap::new();
    let dir = gogo::state::cache::cache_dir().join("verify-cache");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return index;
    };
    for entry in entries.flatten() {
        let stem = entry
            .path()
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if stem.is_empty() {
            continue;
        }
        // `{product_id}-{dlc_id|base}-{file_id}` — ids never contain '-'.
        let Some((head, file_id)) = stem.rsplit_once('-') else {
            continue;
        };
        let (product_id, dlc_id) = if let Some(pid) = head.strip_suffix("-base") {
            (pid.parse::<u64>(), None)
        } else {
            match head.rsplit_once('-') {
                Some((pid, dlc)) => (pid.parse::<u64>(), dlc.parse::<u64>().ok()),
                None => (head.parse::<u64>(), None),
            }
        };
        let Ok(product_id) = product_id else {
            continue;
        };
        let Ok(xml) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(doc) = gogo::gog::checksum::parse(&xml) else {
            continue;
        };
        index.insert(
            doc.name.clone(),
            (product_id, dlc_id, file_id.to_string(), doc),
        );
    }
    index
}

/// `verify --rebuild` (06 §5): walk the download dir and adopt verified files
/// into records — fresh state or a new machine with copied files. Offline: the
/// checksum docs come from the verify-cache, entry metadata best-effort from the
/// cached products.
fn rebuild_records(cfg: &gogo::config::Config, root: &std::path::Path) -> Result<i32, AppError> {
    let index = checksum_index();
    let state = gogo::state::State::load().unwrap_or_default();
    // Sessionless context: append_record needs only cfg-root + state (A5 offline).
    let http = gogo::gog::http().map_err(|e| AppError::Io(std::io::Error::other(e.to_string())))?;
    let ctx = std::sync::Arc::new(
        AppContext::assemble(cfg.clone(), http, None)
            .map_err(|e| AppError::Io(std::io::Error::other(e.to_string())))?,
    );

    // Collect candidate files (skip in-flight sidecars and hidden entries).
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if name.ends_with(".part")
                || name.ends_with(".part.json")
                || name.ends_with(".part.bitmap")
                || name.ends_with(".tmp")
            {
                continue;
            }
            files.push(path);
        }
    }
    files.sort();

    let mut adopted = 0;
    let mut already = 0;
    let mut failed = 0;
    let mut unknown = 0;
    let act = depot_progress::Activity::new(depot_mode(false, false));
    for path in files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some((product_id, dlc_id, file_id, doc)) = index.get(&name).cloned() else {
            println!("  ? {} (no cached checksum)", path.display());
            unknown += 1;
            continue;
        };
        let recorded = state
            .installs
            .iter()
            .any(|r| r.product_id == product_id && r.dlc_id == dlc_id && r.file_id == file_id);
        if recorded {
            println!("  = {} (already recorded)", path.display());
            already += 1;
            continue;
        }
        if !verify_live(&act, &path, &doc) {
            println!("  FAIL {} (not adopted)", path.display());
            failed += 1;
            continue;
        }
        let (version, kind, product_title) = entry_metadata(product_id, dlc_id, &file_id);
        let task = gogo::engine::task::FileTask {
            id: uuid::Uuid::new_v4(),
            product_id,
            dlc_id,
            product_title,
            kind: match kind.as_deref() {
                Some("patch") => gogo::engine::task::Kind::Patch,
                Some("bonus_content") => gogo::engine::task::Kind::BonusContent,
                Some("language_pack") => gogo::engine::task::Kind::LanguagePack,
                _ => gogo::engine::task::Kind::Installer,
            },
            entry: gogo::gog::catalog::InstallerEntry {
                id: file_id.clone(),
                name: Some(doc.name.clone()),
                os: None,
                language: None,
                language_full: None,
                version,
                total_size: Some(doc.total_size),
                kind: None,
                files: Vec::new(),
            },
            file: gogo::gog::catalog::FileEntry {
                id: file_id,
                size: doc.total_size,
                downlink: String::new(),
            },
            dir: path.parent().map(|p| p.to_path_buf()).unwrap_or_default(),
            save_name: Some(doc.name.clone()),
            status: gogo::engine::task::Status::New,
            total_size: doc.total_size,
            url: None,
            checksum: Some(doc),
        };
        gogo::engine::append_record(&ctx, &task, &task.checksum.clone().unwrap(), &path);
        println!("  + {} (adopted)", path.display());
        adopted += 1;
    }
    println!("{adopted} adopted, {already} already recorded, {failed} failed, {unknown} unknown");
    Ok(if failed > 0 {
        cli::exit::CORRUPT
    } else {
        cli::exit::OK
    })
}

/// Best-effort (version, kind, title) for an adopted file from the cached
/// product JSON — absent cache entries simply yield (None, None, None).
fn entry_metadata(
    product_id: u64,
    dlc_id: Option<u64>,
    file_id: &str,
) -> (Option<String>, Option<String>, Option<String>) {
    let Some(product) = gogo::state::cache::load_product(product_id).unwrap_or(None) else {
        return (None, None, None);
    };
    let title = product.title.clone();
    let scopes: Vec<&gogo::gog::catalog::Downloads> = match dlc_id {
        None => product.downloads.iter().collect(),
        Some(dlc) => product
            .expanded_dlcs
            .iter()
            .find(|d| d.id == dlc)
            .map(|d| d.downloads.iter())
            .into_iter()
            .flatten()
            .collect(),
    };
    for downloads in scopes {
        let groups: [(&Vec<gogo::gog::catalog::InstallerEntry>, &str); 4] = [
            (&downloads.installers, "installer"),
            (&downloads.patches, "patch"),
            (&downloads.bonus_content, "bonus_content"),
            (&downloads.language_packs, "language_pack"),
        ];
        for (entries, kind) in groups {
            if let Some(entry) = entries
                .iter()
                .find(|e| e.id == file_id || e.files.iter().any(|f| f.id == file_id))
            {
                return (entry.version.clone(), Some(kind.to_string()), title);
            }
        }
    }
    (None, None, None)
}

// — config (D7: toml_edit round-trip preserves comments) —

fn config_cmd(sub: cli::ConfigSub) -> Result<i32, AppError> {
    let path = CONFIG_PATH
        .get()
        .and_then(|p| p.clone())
        .unwrap_or_else(gogo::config::Config::default_path);
    match sub {
        cli::ConfigSub::Path => {
            println!("{}", path.display());
            Ok(cli::exit::OK)
        }
        cli::ConfigSub::Get { key } => {
            let cfg = load_config_from_env()?;
            let value = match key.as_str() {
                "download.dir" => cfg.download.dir.clone().unwrap_or_default(),
                "library.dir" => cfg.library.dir.clone().unwrap_or_else(|| {
                    gogo::macapp::launch::games_dir()
                        .to_string_lossy()
                        .into_owned()
                }),
                "download.workers" => cfg.download.workers.to_string(),
                "download.parallel_files" => cfg.download.parallel_files.to_string(),
                "download.limit" => cfg.download.limit.to_string(),
                "auth.token_store" => match cfg.auth.token_store {
                    gogo::config::TokenStoreKind::Keyring => "keyring".into(),
                    gogo::config::TokenStoreKind::File => "file".into(),
                },
                "gog.language" => cfg.gog.language.clone(),
                "gog.platform" => format!("{:?}", cfg.gog.platform).to_lowercase(),
                "gog.locale" => cfg.gog.locale.clone(),
                "network.retries" => cfg.network.retries.to_string(),
                other => {
                    eprintln!("gogo: unknown key `{other}`");
                    return Ok(cli::exit::USAGE);
                }
            };
            println!("{value}");
            Ok(cli::exit::OK)
        }
        cli::ConfigSub::Set { key, value } => {
            let (section, field) = key.split_once('.').ok_or_else(|| ConfigError::Invalid {
                key: key.clone(),
                reason: "expected `section.key`".into(),
            })?;
            let mut doc: toml_edit::DocumentMut = std::fs::read_to_string(&path)
                .unwrap_or_default()
                .parse()
                .map_err(|e: toml_edit::TomlError| ConfigError::Parse(e.to_string()))?;
            doc[section][field] = toml_edit::value(toml_value(&value));
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, doc.to_string())?;
            println!("{} = {} ({})", key, value, path.display());
            Ok(cli::exit::OK)
        }
        cli::ConfigSub::Edit => {
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
            std::process::Command::new(editor)
                .arg(&path)
                .status()
                .map_err(AppError::Io)?;
            Ok(cli::exit::OK)
        }
    }
}

/// `config set` writes typed TOML values: integers, floats, and booleans stay
/// unquoted; everything else is a string (05 §4 "same parse rules as TOML").
fn toml_value(raw: &str) -> toml_edit::Value {
    if let Ok(int) = raw.parse::<i64>() {
        return int.into();
    }
    if let Ok(float) = raw.parse::<f64>() {
        return float.into();
    }
    if let Ok(bool) = raw.parse::<bool>() {
        return bool.into();
    }
    raw.into()
}

// — completions (clap_complete → stdout; 03 §1) —

/// `gogo completions <shell>`: print the completion script to stdout. Pure output —
/// no config, no session, no network, nothing on stderr, so the script pipes cleanly
/// into `source`/`eval`-style installs.
fn completions(shell: clap_complete::Shell) -> Result<i32, AppError> {
    write_completions(shell, &mut std::io::stdout())
}

fn write_completions(
    shell: clap_complete::Shell,
    out: &mut dyn std::io::Write,
) -> Result<i32, AppError> {
    use clap::CommandFactory;

    // Fallible `Generator::try_generate` instead of the panicking free `generate`:
    // a closed pipe (`gogo completions bash | head`) is a normal CLI event and must
    // exit cleanly, not unwind. Same wiring as `generate()` — bin name + build.
    let mut cmd = Cli::command();
    cmd.set_bin_name(cmd.get_name().to_string());
    cmd.build();
    clap_complete::Generator::try_generate(&shell, &cmd, out).map_err(AppError::Io)?;
    Ok(cli::exit::OK)
}

// — mac-app (docs/design/10): installer → self-contained macOS .app —

#[cfg(any(target_os = "macos", target_os = "linux"))]
/// Resolve a game selector to its build source (#73, design 15 §1): the
/// depot tree when a depot build exists (warm cache, else an auto-download
/// with `--yes` or an interactive confirm), else — unless `--from-depot`
/// asked for the depot lane explicitly — its downloaded offline installer.
/// Returns `(installer, game_tree)`, exactly one `Some`.
/// The game library for a build (design 05 §7's order): `--library`, then
/// `GOGO_LIBRARY_DIR`, then `library.dir`, checked. A dir gogo won't use is a
/// usage error, before anything is downloaded.
fn resolve_library(
    flag: Option<std::path::PathBuf>,
) -> Result<Option<gogo::macapp::launch::Library>, AppError> {
    // Quietly: the command's own load says what's wrong with the file.
    let cfg = load_config_quietly()?.config;
    let Some(dir) = flag.or(cfg.library.dir.map(std::path::PathBuf::from)) else {
        return Ok(None);
    };
    let downloads = cfg.download.dir.map(std::path::PathBuf::from);
    gogo::macapp::launch::Library::resolve(&dir, downloads.as_deref()).map_err(|e| {
        eprintln!("gogo: {e}");
        AppError::Usage
    })
}

#[allow(clippy::too_many_arguments)]
async fn resolve_game_source(
    command: &str,
    selector: &str,
    explicit: bool,
    auto: bool,
    json_progress: bool,
    json: bool,
    dlc: Option<&DlcAsk>,
) -> Result<GameSource, AppError> {
    let machine = json || json_progress;
    let built = AppContext::build(load_config_from_env()?, true)
        .await
        .map_err(|e| {
            eprintln!("gogo: {e}");
            AppError::Usage
        })?;
    let ctx = std::sync::Arc::new(built);
    // One spinner per phase (#422): it is cleared between them, so the lines
    // below print on their own; prompts pause it (`depot_progress::prompting`).
    let act = depot_progress::Activity::new(depot_mode(json_progress, json));
    let token = act
        .spin_async("refreshing the sign-in", bearer(&ctx))
        .await?;
    let session = ctx
        .session
        .as_ref()
        .and_then(|p| p.gog_warp_core())
        .ok_or(AppError::Usage)?;
    let sel = selector.to_string();
    let selected = act
        .spin_async(
            "checking the library",
            resolve_games(&ctx, &token, std::slice::from_ref(&sel), &act),
        )
        .await?;
    let Some(game) = selected.first() else {
        act.finish();
        eprintln!("gogo: no owned game matches {selector}");
        return Err(AppError::Usage);
    };
    let title = game.title.clone();
    let Some((root, _, _)) = gogo::gog::depot::cache_roots(
        gogo::gog::depot::default_cache_dir().as_deref(),
        game.id,
        None,
    ) else {
        return Err(AppError::Depot(gogo::gog::depot::DepotError::NoCacheDir));
    };

    // The build to take (design 21 §3): `--build` resolved against GOG's list now,
    // so a bad value stops the run before anything else is looked up. A rebuild's
    // plan also compares the build with the installed one, which needs the list.
    let installed = dlc.and_then(|_| installed_ref(game.id));
    let build_value = dlc.and_then(|a| a.build.as_deref());
    let password = dlc.and_then(|a| a.password.clone());
    let listing = if dlc.is_some() && (build_value.is_some() || installed.is_some()) {
        match act
            .spin_async(
                "fetching the builds list",
                gogo::gog::builds::fetch_listing(
                    session,
                    game.id,
                    gog_warp::content_system::types::Platform::Windows,
                    password.clone(),
                ),
            )
            .await
        {
            Ok(l) => Some(l),
            // The plan's build lines are best effort; a picked build isn't.
            Err(e) if build_value.is_some() => {
                return Err(gogo::gog::depot::DepotError::library(e).into());
            }
            Err(_) => None,
        }
    } else {
        None
    };
    let picked: Option<gogo::gog::builds::BuildEntry> = match (build_value, &listing) {
        (Some(v), Some(l)) => Some(gogo::gog::builds::BuildEntry::from_build(
            gogo::gog::builds::pick_for_run(
                l,
                v,
                &gogo::gog::builds::ResolveCtx {
                    title: &title,
                    installed: installed.as_ref().map(|i| i.build_id.as_str()),
                },
            )
            .map_err(gogo::gog::depot::DepotError::from)?,
        )),
        _ => None,
    };

    // The DLC and language the build wants (design 19 §1.2); `mac-app` asks for none.
    let interactive = !auto
        && !machine
        && std::io::IsTerminal::is_terminal(&std::io::stdin())
        && std::io::IsTerminal::is_terminal(&std::io::stderr());
    let dlc_plan = match dlc {
        Some(ask) => {
            let cached = gogo::gog::depot::read_record(&root).and_then(|r| r.language);
            prepare_dlc(
                &ctx,
                &token,
                session,
                game,
                ask,
                interactive,
                cached,
                picked.as_ref(),
                machine,
            )
            .await?
        }
        None => DlcPlan::default(),
    };
    let mut dlc_plan = dlc_plan;
    // What the plan says about the build, for a run that takes `target` (design 21
    // §3.4): the picked build, else what the cache holds (used as it is) or the
    // newest.
    let lines_for = |from_cache: Option<&str>| -> Vec<String> {
        let Some(l) = &listing else {
            return Vec::new();
        };
        let target = match (&picked, from_cache) {
            (Some(p), _) => l.builds.iter().find(|b| *b.build_id() == p.build_id),
            (None, Some(id)) => l.builds.iter().find(|b| b.build_id() == id),
            (None, None) => l.newest(),
        };
        target.map_or_else(Vec::new, |t| {
            gogo::gog::builds::plan_lines(l, t, picked.is_some(), installed.as_ref())
        })
    };
    let mut notes = dlc_plan.notes.clone();
    let want_ids: Vec<String> = match &dlc_plan.opts.dlcs {
        gogo::gog::dlc::DlcSet::Ids(ids) => ids.iter().map(u64::to_string).collect(),
        _ => Vec::new(),
    };
    let cached_tree = find_game_tree(&root);
    if dlc.is_some() && cached_tree.is_some() {
        act.spin_async(
            "checking the depot cache",
            upgrade_cache_record(
                session,
                game.id,
                &title,
                &root,
                &dlc_plan.opts.language,
                &act,
            ),
        )
        .await;
    }
    let mut facts = lane::GameFacts {
        cache_gap: cached_tree
            .as_ref()
            .map(|_| {
                gogo::gog::depot::check_cache(
                    &root,
                    &gogo::gog::depot::CacheWant {
                        build_id: picked.as_ref().map(|p| p.build_id.as_str()),
                        // The build's language whenever the record names one: a cache
                        // can hold several, and only a whole one builds (#389).
                        language: dlc_plan.explicit_lang.as_deref().or((dlc.is_some()
                            && gogo::gog::depot::read_record(&root)
                                .is_some_and(|r| r.language.is_some()))
                        .then_some(dlc_plan.opts.language.as_str())),
                        dlcs: &want_ids,
                    },
                )
            })
            .filter(|c| *c != gogo::gog::depot::CacheCheck::Complete),
        cached_tree,
        ..Default::default()
    };
    // A picked build is a depot build, whatever the public list says.
    facts.has_depot_build = picked.is_some();
    if facts.cached_tree.is_none() && picked.is_none() {
        facts.has_depot_build = act
            .spin_async(
                "fetching the builds list",
                gogo::gog::depot::has_public_build(session, game.id),
            )
            .await
            .map_err(AppError::Depot)?;
    }
    if !facts.has_depot_build {
        let download_root =
            std::path::PathBuf::from(load_config_from_env()?.download.dir.unwrap_or_default());
        let state = gogo::state::State::load().unwrap_or_default();
        facts.installer = lane::downloaded_installer(&state.installs, game.id, &download_root);
    }
    // A cached tree that isn't known to be whole (#316): download again,
    // which keeps what's there and fetches what's gone.
    let mut repair = None;
    match lane::pick(facts, explicit) {
        lane::Lane::DepotCached(tree) => {
            dlc_plan.build_lines = lines_for(
                gogo::gog::depot::read_record(&root)
                    .map(|r| r.build_id)
                    .as_deref(),
            );
            let content = take_content(
                session, game, &root, &tree, &dlc_plan, dlc, &mut notes, machine,
            )
            .await?;
            return Ok((None, Some(tree), notes, content));
        }
        lane::Lane::DepotDownload => {}
        lane::Lane::DepotRepair(tree, gap) => {
            match &gap {
                gogo::gog::depot::CacheCheck::Missing { shown, count } => eprintln!(
                    "gogo: the depot cache for {title} lost {count} file(s) its download \
                     left ({}{})",
                    shown.join(", "),
                    if *count > shown.len() { ", …" } else { "" }
                ),
                gogo::gog::depot::CacheCheck::Unrecorded => eprintln!(
                    "gogo: the depot cache for {title} has no record of a finished \
                     download ({}), so it may lack files (such as GOG's support overlay)",
                    root.join(gogo::gog::depot::RECORD).display()
                ),
                gogo::gog::depot::CacheCheck::MissingDlc { missing } => eprintln!(
                    "gogo: the depot cache for {title} lacks DLC {}: fetching only those",
                    missing.join(", ")
                ),
                gogo::gog::depot::CacheCheck::UnknownContent => eprintln!(
                    "gogo: the depot cache for {title} was downloaded before DLC and language \
                     were recorded, so it is checked (hashing its files) against what's wanted"
                ),
                gogo::gog::depot::CacheCheck::OtherLanguage => eprintln!(
                    "gogo: the depot cache for {title} is in another language than the \
                     wanted one"
                ),
                gogo::gog::depot::CacheCheck::OtherBuild => eprintln!(
                    "gogo: the depot cache for {title} holds another build than the one wanted"
                ),
                gogo::gog::depot::CacheCheck::Interrupted { from, to } => eprintln!(
                    "gogo: an earlier switch of the depot cache for {title} from build {from} \
                     to {to} didn't finish, so its files are checked (hashing) before use"
                ),
                gogo::gog::depot::CacheCheck::Complete => {}
            }
            repair = Some((tree, gap));
        }
        lane::Lane::InstallerFallback(exe) => {
            eprintln!(
                "gogo: {title} has no depot build — using its offline installer {}",
                exe.display()
            );
            // The installer lane's DLC are `installer_dlc`'s (design 19 §5).
            return Ok((Some(exe), None, notes, None));
        }
        lane::Lane::NoSource => {
            eprintln!(
                "gogo: {title} has no depot build and no downloaded installer — \
                 run `gogo download {selector}` first"
            );
            return Err(AppError::Usage);
        }
        lane::Lane::NoDepotBuild => {
            eprintln!(
                "gogo: {title} has no depot build — pass its installer instead \
                 (`gogo {command} {selector}` falls back to it)"
            );
            return Err(AppError::Usage);
        }
    }

    // Cold cache: auto-download with consent (--yes headless, confirm on TTY).
    let mut go = auto;
    if !go
        && std::io::IsTerminal::is_terminal(&std::io::stdin())
        && std::io::IsTerminal::is_terminal(&std::io::stderr())
        && !machine
    {
        go =
            crate::depot_progress::prompting(|| {
                dialoguer::Confirm::new()
            .with_prompt(if repair.is_some() {
                format!(
                    "check the depot cache for {title} (hashes its files once) and fetch \
                     what's missing now? (same as `gogo depot`)"
                )
            } else {
                format!("depot cache empty for {title} — download it now? (same as `gogo depot`)")
            })
            .default(true)
            .interact()
            })
            .unwrap_or(false);
    }
    if !go {
        // The hint must fetch the build that was picked.
        let build_flag = build_value.map_or(String::new(), |v| format!(" --build {v}"));
        match repair {
            // No record says what's missing: build from it, as before.
            Some((_, gogo::gog::depot::CacheCheck::Unrecorded)) if picked.is_some() => {
                // A cache with no record can't be known to hold the picked build.
                eprintln!(
                    "gogo: the depot cache for {title} has no record, so it can't be \
                     known to hold build {}: run `gogo depot {selector} --build {}` first \
                     (or pass --yes to download here)",
                    picked.as_ref().map_or("", |p| p.build_id.as_str()),
                    build_value.unwrap_or_default()
                );
                return Err(AppError::Usage);
            }
            Some((tree, gogo::gog::depot::CacheCheck::Unrecorded)) => {
                eprintln!(
                    "gogo: warning: building from the unchecked depot cache; \
                     `gogo depot {selector}` checks and completes it"
                );
                dlc_plan.build_lines = lines_for(
                    gogo::gog::depot::read_record(&root)
                        .map(|r| r.build_id)
                        .as_deref(),
                );
                let content = take_content(
                    session, game, &root, &tree, &dlc_plan, dlc, &mut notes, machine,
                )
                .await?;
                return Ok((None, Some(tree), notes, content));
            }
            Some(_) => eprintln!(
                "gogo: the depot cache for {title} is incomplete — run \
                 `gogo depot {selector}{}{build_flag}` first (or pass --yes to download here)",
                want_ids
                    .iter()
                    .map(|id| format!(" --dlc {id}"))
                    .collect::<String>()
            ),
            None => eprintln!(
                "gogo: no depot cache for {title} — run `gogo depot {selector}{}{build_flag}` \
                 first (or pass --yes to download here)",
                want_ids
                    .iter()
                    .map(|id| format!(" --dlc {id}"))
                    .collect::<String>()
            ),
        }
        return Err(AppError::Usage);
    }

    // The same progress as `gogo depot` (#337), after a line on what this is.
    let mode = depot_mode(json_progress, json);
    if matches!(mode, depot_progress::Mode::Human { .. }) {
        eprintln!(
            "{}",
            if repair.is_some() {
                format!(
                    "gogo: checking {title}'s depot cache (hashing its files) and fetching \
                     what's missing"
                )
            } else {
                format!("gogo: downloading {title} into the depot cache")
            }
        );
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<gogo::gog::depot::DepotEvent>();
    let opts = dlc_plan.opts.clone();
    dlc_plan.build_lines = lines_for(None);
    // Reuse the command context's cancellation (ctrl-c wiring is upstream).
    let cancel = ctx.cancel.clone();
    let runner = gogo::gog::depot::run(
        session,
        game.id,
        &title,
        &opts,
        &tx,
        &cancel,
        ctx.free_space.as_ref(),
    );
    let act = depot_progress::Activity::new(mode);
    let result = depot_progress::drive(runner, &mut rx, &act).await;
    result.map_err(AppError::Depot)?;
    let tree = find_game_tree(&root).ok_or_else(|| {
        eprintln!(
            "gogo: depot download finished but no game tree found under {}",
            root.display()
        );
        AppError::Usage
    })?;
    let content = take_content(
        session, game, &root, &tree, &dlc_plan, dlc, &mut notes, machine,
    )
    .await?;
    Ok((None, Some(tree), notes, content))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
/// [`resolve_game_source`]'s answer: the installer or the depot tree, the DLC
/// notes for the plan, and what a depot build takes of its tree (`linux-app` only).
type GameSource = (
    Option<std::path::PathBuf>,
    Option<std::path::PathBuf>,
    Vec<String>,
    Option<gogo::gog::dlc_filter::DepotContent>,
);

#[cfg(any(target_os = "macos", target_os = "linux"))]
/// What a depot build takes of cache tree `tree` (design 19 §3.3): the chosen DLC,
/// and when the cache holds others, the filter that leaves them out. Its file lists
/// come from the cache record, else GOG's depot manifests (then kept in the record).
/// A build that can't tell the other DLC's files apart is refused: installing them
/// would go against the flags. `None` for a build that chooses no DLC (`mac-app`).
#[allow(clippy::too_many_arguments)]
async fn take_content(
    session: &gog_warp::Core,
    game: &gogo::gog::library::LibraryProduct,
    root: &std::path::Path,
    tree: &std::path::Path,
    plan: &DlcPlan,
    dlc: Option<&DlcAsk>,
    notes: &mut Vec<String>,
    machine: bool,
) -> Result<Option<gogo::gog::dlc_filter::DepotContent>, AppError> {
    use gogo::gog::dlc_filter::{self, DepotContent, Need};
    if dlc.is_none() {
        return Ok(None);
    }
    let wanted: Vec<u64> = plan.refs.iter().map(|d| d.id).collect();
    // The build's language (#389): `--lang`, else the game's, else the cache's.
    let language = plan.opts.language.clone();
    // The version of the build the cache holds (#418); best effort.
    let (info, manifest) = match gogo::gog::depot::read_record(root) {
        Some(r) => {
            match phase(
                machine,
                "reading the build's manifest",
                gogo::gog::depot::build_info_with_manifest(
                    session,
                    game.id,
                    plan.opts.password.clone(),
                    &r.build_id,
                ),
            )
            .await
            {
                Some((e, m)) => (Some((r.build_id, e)), m),
                None => (None, None),
            }
        }
        None => (None, None),
    };
    let version = info
        .as_ref()
        .filter(|(_, e)| !e.version.trim().is_empty())
        .map(|(id, e)| (id.clone(), e.version.clone()));
    let branch = info.and_then(|(id, e)| e.branch.map(|b| (id, b)));
    let mut content = DepotContent {
        dlcs: plan.refs.clone(),
        language: Some(language.clone()),
        version,
        branch,
        build_lines: plan.build_lines.clone(),
        manifest,
        filter: None,
    };
    let Need::Filter {
        extra,
        products,
        build_id,
        languages,
        ..
    } = dlc_filter::need(root, tree, &game.id.to_string(), &wanted, &language)
    else {
        return Ok(Some(content));
    };
    let lists_in = async |l: &str| match &build_id {
        Some(b) => gogo::gog::depot::product_lists(session, game.id, root, b, l, &products)
            .await
            .map_err(|e| e.to_string()),
        None => Err("neither the cache record nor the game's info names its build".to_string()),
    };
    let act = depot_progress::Activity::new(depot_mode(false, machine));
    let mut lists = act
        .spin_async("reading the depot manifests", lists_in(&language))
        .await;
    let mut others = Vec::new();
    for l in &languages {
        match act
            .spin_async("reading the depot manifests", lists_in(l))
            .await
        {
            Ok(o) => others.push((l.clone(), o)),
            Err(e) => {
                lists = lists.and(Err(e));
                break;
            }
        }
    }
    drop(act);
    let keeping: Vec<String> = products
        .iter()
        .filter(|p| !extra.iter().any(|d| d.id.to_string() == **p))
        .cloned()
        .collect();
    let filter = lists.and_then(|l| {
        dlc_filter::filter_languages(&l, &keeping, &extra, &others)
            .map_err(|lacking| format!("no file list for {}", lacking.join(", ")))
    });
    match filter {
        Ok(f) => {
            if !extra.is_empty() {
                notes.push(format!(
                    "the depot cache also holds {}: left out of the game dir{}",
                    dlc_filter::names(&extra),
                    if f.shared > 0 {
                        format!(" ({} file(s) it shares with the build are kept)", f.shared)
                    } else {
                        String::new()
                    }
                ));
            }
            if !languages.is_empty() {
                let (files, bytes) = f.languages_left_out(root);
                notes.push(format!(
                    "the depot cache also holds {}: {files} file(s), {} left out of the game \
                     dir{}",
                    languages.join(", "),
                    gogo::fmt::human_size(bytes),
                    if f.language_shared > 0 {
                        format!(
                            " ({} file(s) {language} shares with them are kept)",
                            f.language_shared
                        )
                    } else {
                        String::new()
                    }
                ));
            }
            content.filter = Some(f);
            Ok(Some(content))
        }
        Err(reason) => {
            let mut held = Vec::new();
            if !extra.is_empty() {
                held.push(format!(
                    "DLC this build doesn't take ({})",
                    dlc_filter::names(&extra)
                ));
            }
            if !languages.is_empty() {
                held.push(format!("other languages ({})", languages.join(", ")));
            }
            let with: String = wanted
                .iter()
                .chain(extra.iter().map(|d| &d.id))
                .map(|id| format!(" --dlc {id}"))
                .collect();
            let hint = if extra.is_empty() {
                String::new()
            } else {
                format!(
                    ", or build with the DLC too (`gogo linux-app {}{with}`)",
                    game.id
                )
            };
            Err(AppError::Depot(gogo::gog::depot::DepotError::Filter(
                format!(
                    "the depot cache for {} also holds {}, and their files can't be told \
                     apart from the rest: {reason}. Nothing was changed. Retry when GOG can \
                     be reached{hint}",
                    game.title,
                    held.join(" and "),
                ),
            )))
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
/// Decide a build's DLC set and language (design 19 §1.2, §2.3) and turn them into the
/// depot download's options. A game with no depot build yields the defaults: the
/// installer lane takes no DLC yet.
#[allow(clippy::too_many_arguments)]
async fn prepare_dlc(
    ctx: &AppContext,
    token: &str,
    session: &gog_warp::Core,
    game: &gogo::gog::library::LibraryProduct,
    ask: &DlcAsk,
    interactive: bool,
    cache_language: Option<String>,
    pick: Option<&gogo::gog::builds::BuildEntry>,
    machine: bool,
) -> Result<DlcPlan, AppError> {
    use gogo::gog::dlc::{self, Basis, DlcSet, Want};
    let usage = |e: &dyn std::fmt::Display| {
        eprintln!("gogo: {e}");
        AppError::Usage
    };
    // A rebuild keeps what the game recorded (design 19 D4, D10).
    let dirs =
        gogo::macapp::launch::GameDirs::for_game(&game.id.to_string()).map_err(|e| usage(&e))?;
    let prior = if dirs.game.exists() {
        gogo::gog::content::resolve(
            &dirs.root,
            &dirs.game,
            gogo::gog::content::Lane::Depot,
            None,
        )
        .ok()
        .map(|r| r.content)
    } else {
        None
    };
    let recorded: Option<Vec<u64>> = prior.as_ref().map(|c| c.dlc.iter().map(|d| d.id).collect());
    let recorded_language = prior.as_ref().and_then(|c| c.language.clone());
    // A picked build must hold the language the game has (design 21 §3.4); a first
    // build with no `--lang` keeps the default's fallback.
    let language_checked = ask.lang.is_some() || (pick.is_some() && recorded_language.is_some());
    // A rebuild asking for what the game has (`de` for `de-DE`, any case) keeps the
    // recorded code, so it isn't read as a language change.
    let asked = ask.lang.as_ref().map(|l| {
        recorded_language
            .as_ref()
            .filter(|r| gogo::gog::language::same_language(l, r))
            .cloned()
            .unwrap_or_else(|| l.clone())
    });
    // A first build takes the depot cache's language (#389).
    let language = asked
        .clone()
        .or(recorded_language)
        .or(cache_language)
        .unwrap_or_else(|| gogo::gog::language::DEFAULT.to_string());
    let mut plan = DlcPlan {
        opts: gogo::gog::depot::DepotOptions {
            language: language.clone(),
            language_explicit: language_checked,
            password: ask.password.clone(),
            build: pick.map(|p| p.build_id.clone()),
            ..Default::default()
        },
        explicit_lang: asked,
        notes: Vec::new(),
        refs: Vec::new(),
        build_lines: Vec::new(),
    };
    // A picked build is looked at anyway: its languages and DLC are checked.
    let nothing_to_fetch = pick.is_none()
        && (ask.none
            || (ask.names.is_empty()
                && !ask.all
                && recorded.as_ref().is_some_and(|r| r.is_empty())));
    if nothing_to_fetch {
        let d = dlc::decide(&Want::NoDlc, recorded.as_deref(), &[], false);
        plan.notes = dlc::decision_notes(&d, &[]);
        return Ok(plan);
    }

    let act = depot_progress::Activity::new(depot_mode(false, machine));
    let (detail, owned) = act
        .spin_async("checking the game's DLC", async {
            let detail = gogo::gog::catalog::detail(&ctx.http, token, game.id).await?;
            let owned = library::owned_ids(&ctx.http, token).await?;
            Ok::<_, AppError>((detail, owned))
        })
        .await?;
    let (build_id, mdlcs) = match act
        .spin_async(
            "fetching the builds list",
            gogo::gog::depot::build_dlcs(
                session,
                game.id,
                &game.title,
                ask.password.clone(),
                pick.map(|p| p.build_id.as_str()),
                &language,
                language_checked,
            ),
        )
        .await
    {
        Ok(b) => b,
        // No depot build: the installer lane, which takes no DLC yet.
        Err(gogo::gog::depot::DepotError::NoBuilds(_)) => return Ok(plan),
        Err(e) => return Err(e.into()),
    };
    drop(act);
    let infos = dlc::available(&detail.expanded_dlcs, &owned, Some(&mdlcs));
    // The build is named by its version too when it was picked.
    let label = pick.map_or(build_id, |p| p.label());
    let lane = dlc::Lane::Depot {
        build_id: label.clone(),
    };
    // D8: a rebuild never drops a recorded DLC the build lacks, it refuses.
    if ask.names.is_empty()
        && !ask.all
        && !ask.none
        && let Some(kept) = &recorded
    {
        let titles: Vec<(u64, String)> = prior
            .iter()
            .flat_map(|c| c.dlc.iter().map(|d| (d.id, d.title.clone())))
            .collect();
        dlc::check_kept(kept, &titles, &infos, &label).map_err(|e| usage(&e))?;
    }
    let want = if ask.all {
        Want::All
    } else if ask.none {
        Want::NoDlc
    } else if !ask.names.is_empty() {
        Want::Ids(dlc::resolve(&ask.names, &game.title, &infos, &lane).map_err(|e| usage(&e))?)
    } else {
        Want::Default
    };
    let decision = dlc::decide(&want, recorded.as_deref(), &infos, interactive);
    let set = match &decision.basis {
        Basis::Set(s) => s.clone(),
        Basis::Pick(cands) => DlcSet::Ids(pick_dlc(cands, &infos)?),
    };
    let ids = dlc::select(&set, &game.title, &infos, &lane).map_err(|e| usage(&e))?;
    plan.notes = dlc::decision_notes(&decision, &infos);
    plan.refs = ids
        .iter()
        .map(|id| gogo::gog::content::DlcRef {
            id: *id,
            title: infos
                .iter()
                .find(|d| d.id == *id)
                .map_or_else(|| id.to_string(), |d| d.title.clone()),
        })
        .collect();
    plan.opts.dlcs = if ids.is_empty() {
        DlcSet::None
    } else {
        DlcSet::Ids(ids)
    };
    plan.opts.dlc_context = gogo::gog::depot::DlcContext {
        owned,
        catalog_dlcs: detail.expanded_dlcs,
    };
    Ok(plan)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
/// The build a built game records in its `content.toml` (design 21 §3.4); `None`
/// for a game that isn't built or whose record names no build.
fn installed_ref(gogid: u64) -> Option<gogo::gog::builds::InstalledRef> {
    let dirs = gogo::macapp::launch::GameDirs::for_game(&gogid.to_string()).ok()?;
    if !dirs.game.exists() {
        return None;
    }
    let c = gogo::gog::content::resolve(
        &dirs.root,
        &dirs.game,
        gogo::gog::content::Lane::Depot,
        None,
    )
    .ok()?
    .content;
    Some(gogo::gog::builds::InstalledRef {
        build_id: c.build_id?,
        version: c.version,
        branch: c.branch,
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
/// The first build's DLC picker: every owned DLC preselected (design 19 §1.2).
fn pick_dlc(cands: &[u64], infos: &[gogo::gog::dlc::DlcInfo]) -> Result<Vec<u64>, AppError> {
    let items: Vec<String> = cands
        .iter()
        .map(|id| {
            let i = infos.iter().find(|d| d.id == *id);
            format!(
                "{} ({id}, {})",
                i.map_or(id.to_string(), |d| d.title.clone()),
                i.and_then(|d| d.download_size)
                    .map_or("size unknown".to_string(), gogo::fmt::human_size)
            )
        })
        .collect();
    let picked = crate::depot_progress::prompting(|| {
        dialoguer::MultiSelect::new()
            .with_prompt("DLC to install (space toggles, enter accepts)")
            .items(&items)
            .defaults(&vec![true; items.len()])
            .interact_opt()
    });
    match picked {
        Ok(Some(sel)) => Ok(sel.into_iter().map(|i| cands[i]).collect()),
        _ => {
            eprintln!("gogo: aborted");
            Err(AppError::Usage)
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
/// Bring a depot cache record up to date from its build's depot manifests, fetched
/// once for both steps. A record from before DLC and language tracking (#374) gets
/// its language and DLC, so a cache that holds what a build wants isn't hashed. A
/// record whose languages weren't scanned yet (#391) gets the languages an older
/// gogo's switch left files of, so the build leaves them out. Best effort: when it
/// can't be decided safely (or GOG can't be reached), the record stays as it is and
/// the build goes on as before.
async fn upgrade_cache_record(
    session: &gog_warp::Core,
    product_id: u64,
    title: &str,
    root: &std::path::Path,
    language: &str,
    act: &depot_progress::Activity,
) {
    use gogo::gog::{cache_upgrade, depot, dlc_filter};
    let Some(rec) = depot::read_record(root) else {
        return;
    };
    if !depot::languages_unscanned(root, &rec.build_id) {
        return;
    }
    let fetched = match depot::installed_build(session, product_id, Some(&rec.build_id)).await {
        Ok(depot::InstalledBuild {
            listed: Some((_, _, manifest)),
            ..
        }) => dlc_filter::fetch_depots(session.reqwest_client(), &manifest, None)
            .await
            .map(|depots| cache_upgrade::Fetched {
                base: cache_upgrade::base_of(&manifest),
                tree_name: manifest.install_directory(),
                depots,
            })
            .map_err(|e| format!("its build's depot manifests couldn't be read: {e}")),
        Ok(_) => Err(format!("GOG no longer lists build {}", rec.build_id)),
        Err(e) => Err(format!("its build couldn't be looked up: {e}")),
    };
    for line in cache_upgrade::bring_up_to_date(root, title, language, &rec.build_id, fetched) {
        act.say(&format!("gogo: {line}"), &|l| eprintln!("{l}"));
    }
}

/// The install tree inside a depot cache root: a single child directory, or
/// the child carrying a `goggame-*.info` (DLC-capable roots may hold more).
fn find_game_tree(root: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .filter(|p| !p.file_name().is_some_and(|n| n == "gog-support"))
        .collect();
    if dirs.len() == 1 {
        return dirs.pop();
    }
    dirs.into_iter()
        .find(|d| !gogo::macapp::info::gog_game_ids(d).is_empty())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
/// A doctor's findings, one line each (`mac-app doctor`, `linux-app doctor`).
fn print_findings(findings: &[gogo::macapp::doctor::Finding]) {
    for f in findings {
        let hint = f.hint.as_ref().map(|h| format!(" (fix: {h})"));
        println!(
            "{:<4} {:<9} {}{}",
            f.level.as_str(),
            f.check,
            f.message,
            hint.unwrap_or_default()
        );
        for d in &f.detail {
            println!("               | {d}");
        }
    }
}

#[cfg(target_os = "macos")]
/// `gogo mac-app <sub>` (no build).
fn mac_app_sub(sub: crate::cli::MacAppSub) -> Result<i32, AppError> {
    use gogo::macapp::compat;
    match sub {
        crate::cli::MacAppSub::Compat { gogid, json } => {
            compat::validate_gogid(&gogid)?;
            let db = compat::db_entry(&gogid);
            let user = compat::load_user(&gogid)?;
            // L1: the verbs the built bundle baked (the only L1 field shown
            // here; the rest of L0–L2 lives in the bundle TOML).
            let auto = compat::CompatEntry {
                verbs: gogo::macapp::verbs::baked(&gogid),
                ..Default::default()
            };
            let eff = compat::merge(&[
                (compat::Layer::Auto, &auto),
                (compat::Layer::Db, &db.entry),
                (compat::Layer::User, &user.entry),
            ]);
            let warnings: Vec<String> = db
                .warnings
                .iter()
                .map(|w| format!("compat DB: {w}"))
                .chain(
                    user.warnings
                        .iter()
                        .map(|w| format!("{}: {w}", compat::user_path(&gogid).display())),
                )
                .collect();
            // The renderer setup applies and why (#70): L4, else what the
            // bundle baked, traced back to its layer.
            let mut eff = eff;
            let mut renderer_why = None;
            // The bundle's `[[variant]]`s (#85): which one runs, and an
            // unknown `variant` name (setup refuses it).
            let mut variants: Vec<serde_json::Value> = Vec::new();
            let mut variant_error = None;
            let dirs = gogo::macapp::launch::GameDirs::for_game(&gogid)?;
            // The bundle setup recorded, else a portable one (#84) whose
            // launcher set this game up: its config was baked whole at
            // build, so L3/L4 edits since then don't reach it.
            let portable = gogo::macapp::portable::portable_target(&gogid);
            let source = match gogo::macapp::launch::GameState::load(&dirs.state_toml)? {
                Some(state) => Some((state.bundle, Some(state.renderer))),
                None => portable.clone().map(|b| (b, None)),
            };
            if let Some((bundle, applied)) = source {
                let toml = bundle.join("Contents/Resources/gogo-launch.toml");
                if let Ok(raw) = std::fs::read_to_string(&toml)
                    && let Ok(baked) = gogo::macapp::launch::LaunchToml::parse(&raw, &toml)
                {
                    let game = baked
                        .tree_name()
                        .map(|n| dirs.prefix.join("drive_c/GOG").join(n));
                    let exe = gogo::macapp::launch::windows_to_unix(&dirs.prefix, &baked.exe);
                    match &applied {
                        Some(applied) => {
                            let (r, why) = gogo::macapp::renderer::explain(
                                &gogid,
                                &baked.renderer,
                                game.as_deref().filter(|g| g.is_dir()),
                                exe.as_deref().filter(|e| e.is_file()),
                            );
                            eff.renderer = Some(r);
                            renderer_why = Some((why, applied.clone()));
                        }
                        None => {
                            renderer_why = Some((
                                format!("{} (baked into the portable bundle)", baked.renderer),
                                baked.renderer.clone(),
                            ));
                        }
                    }
                    let default = baked.default_variant().map(|v| v.name.clone());
                    let selected = match eff.variant.as_ref().filter(|_| applied.is_some()) {
                        Some(name) => match baked.select_variant(&name.value) {
                            Ok(_) => Some(name.value.clone()),
                            Err(e) => {
                                variant_error = Some(e);
                                None
                            }
                        },
                        None => default.clone(),
                    };
                    variants = baked
                        .variants
                        .iter()
                        .map(|v| {
                            serde_json::json!({
                                "name": v.name,
                                "exe": v.exe,
                                "workdir": v.workdir,
                                "args": v.args,
                                "default": default.as_deref() == Some(v.name.as_str()),
                                "selected": selected.as_deref() == Some(v.name.as_str()),
                            })
                        })
                        .collect();
                }
            }
            if json {
                let out = serde_json::json!({
                    "gogid": gogid,
                    "user_file": compat::user_path(&gogid),
                    "effective": eff,
                    "status_line": eff.status_line(),
                    "renderer_reason": renderer_why.as_ref().map(|(w, _)| w),
                    "renderer_applied": renderer_why.as_ref().map(|(_, a)| a),
                    "variants": variants,
                    "variant_error": variant_error,
                    "portable_bundle": portable,
                    "warnings": warnings,
                });
                println!("{out}");
                return Ok(cli::exit::OK);
            }
            for w in &warnings {
                eprintln!("warning: {w}");
            }
            if let Some(b) = &portable {
                println!(
                    "portable bundle: {} (its config was baked whole at build; the L3/L4 \
                     lines below are what a rebuild would use, not what it runs)",
                    b.display()
                );
            }
            let lines = eff.display_lines();
            println!("status: {}", eff.status_line());
            match (&renderer_why, &portable) {
                (Some((why, _)), Some(_)) => println!("renderer: {why}"),
                (Some((why, applied)), None) => {
                    println!("renderer: {why}; last applied by setup: {applied}")
                }
                (None, _) => {}
            }
            if lines.is_empty() {
                println!(
                    "no compat data for {gogid} (no L1 verbs, L3 DB and L4 override are empty)"
                );
            } else {
                for line in lines {
                    println!("{line}");
                }
            }
            if !variants.is_empty() {
                println!(
                    "variants (`*` runs; pick one with `variant = \"<name>\"` in the L4 file):"
                );
                for v in &variants {
                    let args: Vec<&str> = v["args"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|a| a.as_str())
                        .collect();
                    println!(
                        "  {} {:?}  {}{}{}",
                        if v["selected"] == true { "*" } else { " " },
                        v["name"].as_str().unwrap_or_default(),
                        v["exe"].as_str().unwrap_or_default(),
                        if args.is_empty() {
                            String::new()
                        } else {
                            format!(" {}", args.join(" "))
                        },
                        if v["default"] == true {
                            "  (default)"
                        } else {
                            ""
                        },
                    );
                }
            }
            if let Some(e) = &variant_error {
                eprintln!("error: {e}");
            }
            println!(
                "L4 override file: {}\n(L0–L2 are computed when the bundle is built; L1 verbs \
                 show once it is, and the renderer line traces the bundle's choice)",
                compat::user_path(&gogid).display()
            );
            Ok(cli::exit::OK)
        }
        crate::cli::MacAppSub::Doctor {
            target,
            stop_leftovers,
            json,
        } => {
            use gogo::macapp::doctor;
            let report = doctor::doctor(
                &target,
                &doctor::DoctorOptions {
                    host: doctor::Host::detect(),
                    stop_leftovers,
                },
            )?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "gogid": report.gogid,
                        "bundle": report.bundle,
                        "ok": report.ok(),
                        "findings": report.findings,
                    })
                );
            } else {
                print_findings(&report.findings);
            }
            Ok(if report.ok() {
                cli::exit::OK
            } else {
                cli::exit::FAILED
            })
        }
        crate::cli::MacAppSub::NextRenderer { gogid, dry_run } => {
            match gogo::macapp::setup::next_renderer(&gogid, dry_run)? {
                Some(next) => {
                    println!("{next}");
                    Ok(cli::exit::OK)
                }
                None => {
                    eprintln!(
                        "gogo: {gogid}: {}",
                        gogo::macapp::renderer::NO_MORE_RENDERERS
                    );
                    Ok(cli::exit::FAILED)
                }
            }
        }
        crate::cli::MacAppSub::Setup {
            gogid,
            bundle,
            json,
        } => {
            let out = gogo::macapp::setup::setup(&gogid, bundle.as_deref())?;
            for w in &out.warnings {
                eprintln!("warning: {w}");
            }
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "gogid": out.state.gogid,
                        "bundle": out.state.bundle,
                        "runtime": out.state.runtime,
                        "renderer": out.state.renderer,
                        "stamp": out.state.stamp,
                        "prefix_created": out.prefix_created,
                        "runtime_installed": out.runtime_installed,
                    })
                );
            } else {
                println!(
                    "{gogid}: runtime {}{}, renderer {}{}",
                    out.state.runtime,
                    if out.runtime_installed.is_some() {
                        " (installed into the store)"
                    } else {
                        ""
                    },
                    out.state.renderer,
                    if out.prefix_created {
                        ", prefix created"
                    } else {
                        ""
                    }
                );
            }
            Ok(cli::exit::OK)
        }
    }
}

#[cfg(target_os = "macos")]
/// `gogo mac-app --retarget <bundle|gogid> --runtime <src> --renderer <r>`
/// (design 16 decision 7): write the choices into L4; the bundle stays
/// sealed.
fn mac_app_retarget(
    target: String,
    runtime: Option<crate::cli::RuntimeSource>,
    renderer: Option<crate::cli::RendererArg>,
    runtime_path: Option<std::path::PathBuf>,
    runtime_version: Option<String>,
    json: bool,
) -> Result<i32, AppError> {
    use crate::cli::RuntimeSource as R;
    use gogo::macapp::{compat, launch, store};

    if runtime.is_none() && renderer.is_none() {
        eprintln!("gogo: --retarget needs --runtime and/or --renderer");
        return Ok(cli::exit::USAGE);
    }

    gogo::macapp::portable::refuse(&target, "--retarget")?;
    let gogid = gogo::macapp::setup::target_gogid(&target)?;
    let with_version = |id: &str| match &runtime_version {
        Some(v) => format!("{id}/{v}"),
        None => id.to_string(),
    };
    let mut updates: Vec<(&str, Option<String>)> = Vec::new();
    if let Some(r) = renderer {
        // `auto` drops the override; a version-less id is written with the
        // catalog's pinned version.
        updates.push(("renderer", r.renderer().map(|r| r.default_ref())));
        // D3DMetal (#71): asked once on a terminal; without a yes, setup
        // falls back to wined3d and says so.
        if r.renderer() == Some(gogo::macapp::Renderer::D3dMetal) {
            let tty = std::io::IsTerminal::is_terminal(&std::io::stdin())
                && std::io::IsTerminal::is_terminal(&std::io::stderr())
                && !json;
            let p = tty.then(prompt::renderer);
            let act = depot_progress::Activity::new(depot_mode(false, json));
            gogo::macapp::d3dmetal::ensure(
                &format!("--renderer d3dmetal was asked for {gogid}"),
                p.as_deref(),
                &mut |ev| emit_live(ev, false, &act),
            );
        }
    }
    let new = match runtime {
        None => None,
        Some(R::Auto) => None,
        Some(R::Winecx) => Some(with_version("winecx")),
        Some(R::Winecx26) => Some(with_version("winecx26")),
        Some(R::Staging) => Some(with_version("wine-staging")),
        Some(R::Devel) => Some(with_version("wine-devel")),
        Some(R::Stable) => Some(with_version("wine-stable")),
        Some(runtime @ (R::Brew | R::Gptk | R::Path)) => {
            let rt = gogo::macapp::runtime::resolve(&gogo::macapp::RuntimeOptions {
                mode: gogo::macapp::RuntimeMode::from(runtime),
                runtime_path,
                version: runtime_version.clone(),
            })?;
            Some(launch::runtime_ref(&rt))
        }
    };
    if runtime.is_some() {
        updates.push(("runtime", new.clone()));
    }
    let updates: Vec<(&str, Option<&str>)> =
        updates.iter().map(|(k, v)| (*k, v.as_deref())).collect();
    // L4, then setup (a missing catalog runtime is installed on the way, and
    // a new engine gets `wineboot -u`): the bundle stays sealed.
    let out = gogo::macapp::setup::retarget(&gogid, &updates)?;
    // No setup ran (no state yet): say now what the first launch would.
    if out.setup.is_none()
        && let Some(r) = &new
        && !r.starts_with("path:")
    {
        let (id, ver) = r
            .split_once('/')
            .map_or((r.as_str(), None), |(i, v)| (i, Some(v)));
        if store::installed(id, ver).is_none() {
            eprintln!(
                "warning: {r} is not in the runtime store yet — `gogo runtimes install {id}` \
                 before the next launch"
            );
        }
    }
    let setup = match &out.setup {
        Some(Ok(s)) => {
            for w in &s.warnings {
                eprintln!("warning: {w}");
            }
            serde_json::json!({
                "ok": true,
                "runtime": s.state.runtime,
                "renderer": s.state.renderer,
                "prefix_updated": s.prefix_updated,
                "runtime_installed": s.runtime_installed,
            })
        }
        Some(Err(e)) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        None => serde_json::Value::Null,
    };
    if json {
        println!(
            "{}",
            serde_json::json!({
                "gogid": gogid,
                "file": compat::user_path(&gogid),
                "changes": out.changes,
                "setup": setup,
            })
        );
    } else if out.changes.is_empty() {
        println!("{gogid}: nothing to change (the override file already says so)");
    } else {
        for c in &out.changes {
            println!(
                "{gogid}: {} {} -> {} ({})",
                c.key,
                c.old.as_deref().unwrap_or("(bundle default)"),
                c.new.as_deref().unwrap_or("(bundle default)"),
                compat::user_path(&gogid).display()
            );
        }
        match &out.setup {
            Some(Ok(s)) => println!(
                "{gogid}: set up on runtime {}, renderer {}{}",
                s.state.runtime,
                s.state.renderer,
                if s.prefix_updated {
                    " (prefix updated for the new engine)"
                } else {
                    ""
                }
            ),
            Some(Err(e)) => {
                eprintln!("gogo: {gogid}: setup failed: {e}");
                eprintln!("the override is saved; the next launch tries the setup again");
                return Ok(cli::exit::FAILED);
            }
            None => println!("the next launch applies it"),
        }
    }
    if matches!(out.setup, Some(Err(_))) {
        return Ok(cli::exit::FAILED);
    }
    Ok(cli::exit::OK)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
/// The path gogo was invoked as, for the launcher (design 16 decision 2):
/// a profile symlink like `~/.nix-profile/bin/gogo` survives upgrades, the
/// resolved store path doesn't, so this never canonicalizes.
fn invoked_path() -> Option<std::path::PathBuf> {
    let argv0 = std::path::PathBuf::from(std::env::args_os().next()?);
    if argv0.components().count() > 1 {
        return Some(if argv0.is_absolute() {
            argv0
        } else {
            std::env::current_dir().ok()?.join(argv0)
        });
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(&argv0))
        .find(|p| p.is_file())
}

#[cfg(target_os = "macos")]
/// `gogo mac-app` — convert one downloaded Windows installer into a
/// self-contained `.app`. The pipeline is synchronous (child processes + fs
/// copies), so it runs on blocking threads; events render exactly like the
/// download engine's (NDJSON on stdout for `--json-progress`, one-liners on
/// stderr otherwise, 03 §4).
#[allow(clippy::too_many_arguments)]
async fn mac_app(
    input: Option<String>,
    from_depot: Option<String>,
    name: Option<String>,
    library: Option<std::path::PathBuf>,
    menus: menus::Menus,
    runtime_path: Option<std::path::PathBuf>,
    runtime_version: Option<String>,
    renderer: gogo::macapp::RendererChoice,
    portable: bool,
    out: Option<std::path::PathBuf>,
    yes: bool,
    json_progress: bool,
    json: bool,
) -> Result<i32, AppError> {
    // Portable builds leave the game library alone (the flag conflicts).
    let library = if portable {
        None
    } else {
        resolve_library(library)?
    };
    // Design 15 §1 / #73: exactly one source. A game selector takes the
    // depot lane (installer fallback when it has no depot build); an .exe
    // path keeps the installer lane; a directory is a ready game tree.
    let (installer, game_tree) = match lane::classify(input.as_deref(), from_depot.as_deref()) {
        Some(lane::Input::Installer(p)) => (Some(p), None),
        Some(lane::Input::Tree(t)) => (None, Some(t)),
        Some(lane::Input::Game { selector, explicit }) => {
            let (i, t, _, _) = resolve_game_source(
                "mac-app",
                &selector,
                explicit,
                yes,
                json_progress,
                json,
                None,
            )
            .await?;
            (i, t)
        }
        None => {
            eprintln!("gogo: mac-app needs a GAME or an INSTALLER path");
            return Ok(cli::exit::USAGE);
        }
    };

    let mut recipe = menus.recipe;
    if game_tree.is_some() {
        // Recipes are installer-shaped (design 15 D6); the tree already is
        // the installed artifact their steps produce.
        if !matches!(
            recipe,
            gogo::macapp::RecipeChoice::Auto | gogo::macapp::RecipeChoice::None
        ) {
            eprintln!(
                "gogo: --recipe is installer-lane only; depot mode bundles the \
                 tree as-is — pass the setup .exe to use a recipe (docs/design/15 D6)"
            );
            return Ok(cli::exit::USAGE);
        }
        recipe = gogo::macapp::RecipeChoice::None;
    }
    // Interactive flow (design 12 §9): the dialoguer renderer on a TTY;
    // --json / --json-progress imply non-interactive; non-TTY runs take the
    // ranked best (recipes) or the headless ladder (runtimes). The recipe
    // and runtime menus show only when asked for (#76).
    let prompter: Option<std::sync::Arc<gogo::macapp::PromptFn>> =
        if std::io::IsTerminal::is_terminal(&std::io::stdin())
            && std::io::IsTerminal::is_terminal(&std::io::stderr())
            && !json
            && !json_progress
        {
            Some(prompt::renderer())
        } else {
            None
        };

    let opts = gogo::macapp::MacAppOptions {
        installer,
        game_tree,
        name,
        mode: menus.mode,
        runtime_path,
        runtime_version,
        out_dir: out.unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        }),
        overwrite: yes,
        recipe,
        runtime_menu: menus.runtime_menu,
        lutris_base: None,
        renderer,
        portable,
        gogo_path: invoked_path(),
        library,
    };

    // Plan: resolve + probe the runtime, validate the installer, size things up.
    let plan = {
        let opts = opts.clone();
        let jp = json_progress;
        let p = prompter.clone();
        let mode = depot_mode(jp, json);
        tokio::task::spawn_blocking(move || {
            // Prompts inside the plan pause the spinner.
            let act = depot_progress::Activity::new(mode);
            act.spin("planning the conversion", || {
                gogo::macapp::plan(&opts, p.as_deref(), &mut |ev| emit_live(ev, jp, &act))
            })
        })
        .await
        .map_err(|e| AppError::Io(std::io::Error::other(e.to_string())))??
    };

    // Confirmation mirrors `download --all` (03 §3): on a TTY ask, non-TTY
    // without --yes print the plan and abort — a conversion never starts
    // silently, and hundreds-of-MB bundles deserve the pause (docs/13 §4).
    if !yes {
        let recipe_line = match &plan.recipe {
            Some(r) => format!(
                "{} — {} ({})",
                r.installer.slug, r.installer.version, r.installer.author
            ),
            None => "none".to_string(),
        };
        let runtime_kind = match &plan.runtime.mode {
            gogo::macapp::RuntimeMode::Catalog(id) => format!("catalog:{id}"),
            m => m.as_str().to_string(),
        };
        let download_line = plan
            .runtime_downloaded
            .as_ref()
            .map(|(id, v)| format!("\n  downloaded during planning: {id} {v}"))
            .unwrap_or_default();
        let source_line = plan
            .installer
            .as_ref()
            .map(|i| i.installer.display().to_string())
            .or_else(|| {
                plan.game_tree
                    .as_ref()
                    .map(|t| format!("{} (depot)", t.display()))
            })
            .unwrap_or_default();
        let verbs_line = format!(
            "\n  verbs:     {}",
            gogo::macapp::verbs::summary(plan.gogid.as_deref(), plan.auto_verbs.as_deref())
        );
        let renderer_line = match &plan.renderer {
            Some(sel) => sel.describe(),
            None => match plan.renderer_choice {
                gogo::macapp::RendererChoice::Force(r) => format!("{} (flags)", r.default_ref()),
                _ => "auto (picked from the installed files)".to_string(),
            },
        };
        // #74: what gogo knows about this game, before the long part (only
        // the depot lane knows the id this early).
        let compat_line = plan
            .compat
            .as_ref()
            .map(|c| {
                format!(
                    "\n  compat:    {}",
                    c.status_line_for(Some(gogo::macapp::compat::Lane::Depot))
                )
            })
            .unwrap_or_default();
        let portable_line = if plan.portable {
            "\n  portable:  runtime, renderer, verb files and game inside the bundle; its \
             launcher creates the prefix on the Mac it runs on"
        } else {
            ""
        };
        eprintln!(
            "converting:\n  source:    {}\n  bundle:    {}{}{}\n  recipe:    {}\n  runtime:   {} ({}){}\n  renderer:  {}{}\n  est. size: {}",
            source_line,
            plan.bundle_path.display(),
            portable_line,
            compat_line,
            recipe_line,
            plan.runtime.version,
            runtime_kind,
            download_line,
            renderer_line,
            verbs_line,
            gogo::fmt::human_size(plan.estimated_bytes),
        );
        if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            eprintln!("gogo: non-interactive without --yes — aborting (plan above)");
            return Ok(cli::exit::USAGE);
        }
        // §9: final Confirm via the renderer when interactive; the plain
        // y/N read otherwise (json implies non-interactive, gated above).
        let go_ahead = match &prompter {
            Some(p) => {
                p(&gogo::macapp::PromptSpec {
                    kind: gogo::macapp::PromptKind::Confirm { default: false },
                    title: "proceed with the conversion?".into(),
                    items: vec![],
                    allow_other: false,
                    allow_none: false,
                    default: 0,
                }) == Some(gogo::macapp::PromptAnswer::Confirmed(true))
            }
            None => {
                print!("proceed? [y/N]: ");
                {
                    use std::io::Write as _;
                    std::io::stdout().flush().ok();
                }
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                line.trim().eq_ignore_ascii_case("y")
            }
        };
        if !go_ahead {
            eprintln!("gogo: aborted");
            return Ok(cli::exit::USAGE);
        }
    }

    let selected = std::sync::Arc::new(std::sync::Mutex::new(plan.renderer.as_ref().map(|s| {
        serde_json::json!({
            "renderer": s.renderer.value,
            "layer": s.renderer.layer.describe(),
            "reason": s.reason,
            "alternatives": s.l1.alternatives.iter().map(|r| r.id()).collect::<Vec<_>>(),
        })
    })));
    let bytes = {
        let plan = plan.clone();
        let jp = json_progress;
        let selected = selected.clone();
        let mode = depot_mode(jp, json);
        tokio::task::spawn_blocking(move || {
            let act = depot_progress::Activity::new(mode);
            let policy = if cfg!(target_os = "macos") {
                gogo::macapp::bundle::CodesignPolicy::Adhoc
            } else {
                gogo::macapp::bundle::CodesignPolicy::Skip
            };
            gogo::macapp::execute(&plan, policy, &mut |ev| {
                if let gogo::engine::events::ProgressEvent::RendererSelected {
                    renderer,
                    layer,
                    reason,
                } = ev
                    && plan.renderer.is_none()
                {
                    *selected.lock().unwrap() = Some(serde_json::json!({
                        "renderer": renderer, "layer": layer, "reason": reason,
                    }));
                }
                emit_live(ev, jp, &act)
            })
        })
        .await
        .map_err(|e| AppError::Io(std::io::Error::other(e.to_string())))??
    };

    if json {
        println!(
            "{}",
            serde_json::json!({
                "bundle": plan.bundle_path,
                "gogid": plan.gogid,
                "portable": plan.portable,
                "state_dir": plan.gogid.as_ref().filter(|_| !plan.portable)
                    .map(|_| plan.state.root.clone()),
                "name": plan.display,
                "slug": plan.slug,
                "recipe": plan.recipe.as_ref().map(|r| serde_json::json!({
                    "slug": r.installer.slug,
                    "version": r.installer.version,
                    "author": r.installer.author,
                    "wine_version": r.wine_version,
                    "from_search": r.from_search,
                })),
                "auto_verbs": plan.auto_verbs,
                // What setup applies: L1 plus the L3/L4 edits (#128).
                "verbs": plan.auto_verbs.as_deref().map(|a| {
                    gogo::macapp::verbs::for_plan(plan.gogid.as_deref(), a)
                }),
                "compat_status": plan.compat.as_ref().map(|c| c.status_line_for(Some(gogo::macapp::compat::Lane::Depot))),
                "renderer": selected.lock().unwrap().clone(),
                "runtime": {
                    "kind": plan.runtime.kind,
                    "version": plan.runtime.version,
                    "wine": plan.runtime.wine,
                    "downloaded": plan.runtime_downloaded.as_ref()
                        .map(|(id, v)| serde_json::json!({"id": id, "version": v})),
                },
            })
        );
    } else {
        println!(
            "created {} ({}) — drag it into /Applications and double-click to play",
            plan.bundle_path.display(),
            gogo::fmt::human_size(bytes),
        );
        if plan.portable {
            println!(
                "portable: copy it to any Mac; on first launch it creates the prefix and \
                 saves under ~/Library/Application Support/gogo/games/ there (no gogo needed). \
                 It can't be retargeted: rebuild it to change its runtime or renderer"
            );
        } else {
            // The games dir, or where this game's dir is: a game in a library.
            let shown = match plan.state.root.parent() {
                Some(p) if gogo::macapp::launch::in_library(&plan.state.root) => p.to_path_buf(),
                _ => gogo::macapp::launch::games_dir(),
            };
            println!("game files, prefix and saves: {}", shown.display());
        }
    }
    Ok(cli::exit::OK)
}

// — linux-app (design 18 §10): a Windows game through GE-Proton and umu —

#[cfg(target_os = "linux")]
/// `gogo linux-app`'s build flags.
struct LinuxAppArgs {
    input: Option<String>,
    from_depot: Option<String>,
    name: Option<String>,
    library: Option<std::path::PathBuf>,
    runtime: Option<Option<String>>,
    renderer: gogo::macapp::RendererChoice,
    refresh: bool,
    interactive: bool,
    yes: bool,
    dlc: DlcAsk,
    no_snapshots: bool,
    json_progress: bool,
    json: bool,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
/// A build's DLC and language flags (design 19 §1.1).
#[derive(Default)]
struct DlcAsk {
    names: Vec<String>,
    all: bool,
    none: bool,
    lang: Option<String>,
    /// `--build` (design 21 §3): the depot build to take instead of the newest.
    build: Option<String>,
    /// `--password`, for a protected build.
    password: Option<String>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl DlcAsk {
    /// A flag that asks for DLC (`--no-dlcs` and a bare `--lang` don't).
    fn wants_dlc(&self) -> bool {
        self.all || !self.names.is_empty()
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
/// What `resolve_game_source` hands back besides the source: the depot options
/// the download must use and the plan's DLC notes.
#[derive(Default)]
struct DlcPlan {
    opts: gogo::gog::depot::DepotOptions,
    /// The DLC the build takes, titled (design 19 §1.2).
    refs: Vec<gogo::gog::content::DlcRef>,
    /// `--lang`, when given: only then is a cache's language checked.
    explicit_lang: Option<String>,
    notes: Vec<String>,
    /// The plan's lines about the build (design 21 §3.4).
    build_lines: Vec<String>,
}

#[cfg(target_os = "linux")]
/// The `--runtime` menu (bare `--runtime`, `--interactive`): gogo's pinned
/// GE-Proton, the GE-Protons in the store, then the unmanaged builds found
/// in each Steam (design 18 §1); "other…" takes a typed ref.
fn linux_runtime_menu(prompt: &gogo::macapp::PromptFn) -> Option<String> {
    use gogo::linuxapp::proton;
    use gogo::macapp::{PromptAnswer, PromptKind, PromptSpec, catalog, store};
    let mut refs: Vec<String> = Vec::new();
    let mut items: Vec<String> = Vec::new();
    let pin = gogo::linuxapp::setup::pinned_version(catalog::GE_PROTON);
    if let Some(pin) = pin {
        let here = store::installed(catalog::GE_PROTON, Some(pin)).is_some();
        refs.push(format!("{}/{pin}", catalog::GE_PROTON));
        items.push(format!(
            "{}/{pin}  gogo's pin ({})",
            catalog::GE_PROTON,
            if here { "installed" } else { "download" }
        ));
    }
    let mut versions: Vec<String> = store::load_manifest()
        .into_iter()
        .filter(|m| m.id == catalog::GE_PROTON && Some(m.version.as_str()) != pin)
        .map(|m| m.version)
        .collect();
    versions.sort();
    versions.dedup();
    for v in versions.into_iter().rev() {
        refs.push(format!("{}/{v}", catalog::GE_PROTON));
        items.push(format!("{}/{v}  installed", catalog::GE_PROTON));
    }
    for b in proton::discover(&gogo::linuxapp::umu::Host::real()) {
        let problems = b.problems();
        refs.push(format!("path:{}", b.path.display()));
        items.push(format!(
            "path:{}  {}, unmanaged{}",
            b.path.display(),
            b.source,
            if problems.is_empty() {
                String::new()
            } else {
                format!(" ({})", problems.join("; "))
            }
        ));
    }
    match prompt(&PromptSpec {
        kind: PromptKind::Select,
        title: "Proton build for this game".into(),
        items,
        allow_other: true,
        allow_none: false,
        default: 0,
    })? {
        PromptAnswer::Index(i) => refs.get(i).cloned(),
        PromptAnswer::Other(s) => Some(s.trim().to_string()),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
/// The plan the confirmation shows (design 18 §10): what runs, what gets
/// downloaded and where, and what the build sets for the game.
fn linux_app_plan_lines(plan: &gogo::linuxapp::build::LinuxAppPlan) -> String {
    use gogo::linuxapp::build;
    let mut out = String::from("setting up:\n");
    let line = |out: &mut String, k: &str, v: String| out.push_str(&format!("  {k:<10} {v}\n"));
    let source = plan
        .installer
        .as_ref()
        .map(|i| {
            format!(
                "{} (offline installer, run under umu)",
                i.installer.display()
            )
        })
        .or_else(|| {
            plan.game_tree
                .as_ref()
                .map(|t| format!("{} (depot)", t.display()))
        })
        .unwrap_or_default();
    line(&mut out, "source:", source);
    if let Some(c) = &plan.depot_content
        && let Some((first, rest)) = c.build_lines.split_first()
    {
        line(&mut out, "build:", first.clone());
        for l in rest {
            out.push_str(&format!("{:<13}{l}\n", ""));
        }
    }
    line(
        &mut out,
        "dlc:",
        if plan.dlcs.is_empty() {
            "none".to_string()
        } else {
            plan.dlcs
                .iter()
                .map(|d| d.title.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        },
    );
    for note in &plan.dlc_notes {
        out.push_str(&format!("{:<13}{note}\n", ""));
    }
    line(
        &mut out,
        "game dir:",
        match &plan.gogid {
            Some(_) if plan.rebuild => format!(
                "{} (rebuild: game files replaced, prefix and saves kept)",
                plan.state.root.display()
            ),
            Some(_) => plan.state.root.display().to_string(),
            None => format!(
                "{} (the game's id is known after the install)",
                plan.library
                    .as_ref()
                    .map_or_else(gogo::macapp::launch::games_dir, |l| l.dir().to_path_buf())
                    .join("<gogid>")
                    .display()
            ),
        },
    );
    if let Some(c) = &plan.compat {
        line(
            &mut out,
            "compat:",
            c.status_line_for(Some(gogo::macapp::compat::Lane::Depot)),
        );
    }
    line(&mut out, "snapshots:", plan.snapshots.line());
    line(&mut out, "umu:", plan.umu_line.clone());
    line(&mut out, "runtime:", plan.runtime_line.clone());
    line(&mut out, "container:", plan.container_line.clone());
    let renderer = match &plan.renderer {
        Some(sel) => {
            let mut r = sel.describe_linux();
            match &plan.renderer_setup {
                Some(p) => r.push_str(&format!("; sets {}", build::switches(p))),
                None if sel.renderer.value == gogo::macapp::Renderer::Wined3d.id()
                    && sel.renderer.layer == gogo::macapp::compat::Layer::Auto =>
                {
                    r.push_str("; sets nothing: Proton's defaults")
                }
                None if sel.renderer.value == gogo::macapp::Renderer::Wined3d.id() => {
                    r.push_str("; sets PROTON_USE_WINED3D=1")
                }
                None => r.push_str("; its switches are known once the Proton build is here"),
            }
            r
        }
        None => match plan.renderer_choice {
            gogo::macapp::RendererChoice::Force(r) => format!("{} (flags)", r.id()),
            _ => "auto (picked from the installed files)".to_string(),
        },
    };
    line(&mut out, "renderer:", renderer);
    if let Some(v) = &plan.auto_verbs {
        line(
            &mut out,
            "verbs:",
            gogo::macapp::verbs::summary(plan.gogid.as_deref(), Some(v)),
        );
    }
    if let Some(g) = &plan.game_files {
        out.push_str(&format!("  {}\n", depot_progress::game_files_line(g)));
    }
    match (&plan.umu_id, &plan.fixes) {
        (Some(id), Some(fx)) => {
            line(&mut out, "umu id:", id.to_string());
            for l in fx.to_string().lines() {
                out.push_str(&format!("  {l}\n"));
            }
        }
        _ => out.push_str(
            "  fixes:     shown after the Proton build is installed (its protonfixes decide them)\n",
        ),
    }
    out
}

#[cfg(target_os = "linux")]
/// `gogo linux-app` (design 18 §10): plan, confirm, build, set up. Like
/// `mac-app`, the pipeline is synchronous and runs on blocking threads.
async fn linux_app(a: LinuxAppArgs) -> Result<i32, AppError> {
    let machine = a.json || a.json_progress;
    let library = resolve_library(a.library.clone())?;
    let mut dlc_notes = Vec::new();
    let mut depot_content = None;
    let input = lane::classify(a.input.as_deref(), a.from_depot.as_deref());
    // D9: installers come in the newest version only.
    if let (Some(i), Some(_)) = (&input, &a.dlc.build)
        && let Err(e) = lane::check_build(i)
    {
        eprintln!("gogo: {e}");
        return Ok(cli::exit::USAGE);
    }
    let (installer, game_tree) = match input {
        Some(lane::Input::Installer(p)) => (Some(p), None),
        Some(lane::Input::Tree(t)) => {
            if a.dlc.wants_dlc() {
                dlc_notes.push("the DLC flags don't apply to a game tree you point at".to_string());
            }
            (None, Some(t))
        }
        Some(lane::Input::Game { selector, explicit }) => {
            let (i, t, notes, content) = resolve_game_source(
                "linux-app",
                &selector,
                explicit,
                a.yes,
                a.json_progress,
                a.json,
                Some(&a.dlc),
            )
            .await?;
            dlc_notes = notes;
            depot_content = content;
            (i, t)
        }
        None => {
            eprintln!("gogo: linux-app needs a GAME or an INSTALLER path");
            return Ok(cli::exit::USAGE);
        }
    };
    // The offline-installer lane's DLC (design 19 §5): chosen now, their
    // installers downloaded after the consent.
    let installer_choice = match &installer {
        Some(setup) => {
            let interactive = !a.yes && dlc_cmd::interactive(machine);
            let c = installer_dlc::plan_build(setup, &a.dlc, interactive, machine).await?;
            dlc_notes.extend(c.notes.iter().cloned());
            Some(c)
        }
        None => None,
    };
    let prompter: Option<std::sync::Arc<gogo::macapp::PromptFn>> =
        if std::io::IsTerminal::is_terminal(&std::io::stdin())
            && std::io::IsTerminal::is_terminal(&std::io::stderr())
            && !machine
        {
            Some(prompt::renderer())
        } else {
            None
        };
    // A `--runtime` value wins; a bare `--runtime` or `--interactive` asks.
    let runtime = a.runtime.clone().flatten();
    let ask = runtime.is_none() && (a.interactive || matches!(a.runtime, Some(None)));
    let runtime = if ask {
        let Some(p) = &prompter else {
            eprintln!(
                "gogo: the runtime menu needs a terminal — pass `--runtime <RUNTIME>` \
                 (ge-proton[/<version>] or path:<dir>)"
            );
            return Ok(cli::exit::USAGE);
        };
        let Some(r) = linux_runtime_menu(p.as_ref()) else {
            eprintln!("gogo: aborted");
            return Ok(cli::exit::USAGE);
        };
        Some(r)
    } else {
        runtime
    };
    let snapshots = {
        use gogo::linuxapp::snapshots::{Backends, SnapshotOptions};
        let config = load_config_quietly()?.config.snapshots;
        let opts = if a.no_snapshots {
            SnapshotOptions::no_snapshots()
        } else if !config.auto {
            SnapshotOptions::off("[snapshots] auto = false")
        } else {
            SnapshotOptions::on(Backends::real())
        };
        SnapshotOptions {
            keep_auto: config.keep_auto,
            ..opts
        }
    };
    let opts = gogo::linuxapp::build::LinuxAppOptions {
        snapshots,
        installer,
        game_tree,
        name: a.name,
        runtime,
        renderer: a.renderer,
        refresh: a.refresh,
        gogo_path: invoked_path(),
        library,
        dlc_notes,
        depot_content,
        installer_dlc: installer_choice
            .as_ref()
            .map(|c| c.build.clone())
            .unwrap_or_default(),
    };
    let jp = a.json_progress;
    let mut plan = {
        let p = prompter.clone();
        let mode = depot_mode(jp, a.json);
        tokio::task::spawn_blocking(move || {
            // The PE scan and the umu database take a while (#422); a prompt
            // inside the plan pauses the spinner.
            let act = depot_progress::Activity::new(mode);
            act.spin("planning the build", || {
                gogo::linuxapp::build::plan(&opts, p.as_deref(), &mut |ev| emit_live(ev, jp, &act))
            })
        })
        .await
        .map_err(|e| AppError::Io(std::io::Error::other(e.to_string())))??
    };

    if !a.yes {
        eprint!("{}", linux_app_plan_lines(&plan));
        if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            eprintln!("gogo: non-interactive without --yes — aborting (plan above)");
            return Ok(cli::exit::USAGE);
        }
        let go_ahead = match &prompter {
            Some(p) => {
                p(&gogo::macapp::PromptSpec {
                    kind: gogo::macapp::PromptKind::Confirm { default: false },
                    title: "proceed?".into(),
                    items: vec![],
                    allow_other: false,
                    allow_none: false,
                    default: 0,
                }) == Some(gogo::macapp::PromptAnswer::Confirmed(true))
            }
            None => {
                print!("proceed? [y/N]: ");
                {
                    use std::io::Write as _;
                    std::io::stdout().flush().ok();
                }
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                line.trim().eq_ignore_ascii_case("y")
            }
        };
        if !go_ahead {
            eprintln!("gogo: aborted");
            return Ok(cli::exit::USAGE);
        }
    }

    if let Some((ctx, _, product)) = installer_choice.and_then(|c| c.fetch) {
        let d = &plan.installer_dlc;
        let ids: Vec<u64> = d.refs.iter().map(|r| r.id).collect();
        let language = d.language.clone().unwrap_or_default();
        let got = installer_dlc::fetch(&ctx, &product, &ids, &d.refs, &language, machine).await?;
        plan.installer_dlc.installers = got;
    }
    let out = {
        let plan = plan.clone();
        let p = prompter.clone();
        let mode = depot_mode(jp, a.json);
        tokio::task::spawn_blocking(move || {
            let act = depot_progress::Activity::new(mode);
            gogo::linuxapp::build::execute(&plan, p.as_deref(), &mut |ev| emit_live(ev, jp, &act))
        })
        .await
        .map_err(|e| AppError::Io(std::io::Error::other(e.to_string())))??
    };
    print_linux_setup(&out.setup, a.json, |v| {
        v["name"] = serde_json::json!(out.title);
        v["game_dir"] = serde_json::json!(out.dirs.root);
        v["lane"] = serde_json::json!(if plan.installer.is_some() {
            "installer"
        } else {
            "depot"
        });
        v["build_id"] = serde_json::json!(out.content.build_id);
        v["language"] = serde_json::json!(out.content.language);
        v["dlcs"] = serde_json::json!(out.content.refs());
        v["snapshot"] = serde_json::json!(
            out.snapshot
                .as_ref()
                .map(gogo::linuxapp::snapshots::meta_view)
        );
        v["pruned"] = serde_json::json!(
            out.pruned
                .iter()
                .map(gogo::linuxapp::snapshots::SnapshotRef::of)
                .collect::<Vec<_>>()
        );
        v["game_files"] = serde_json::json!(out.materialized.as_ref().map(|r| serde_json::json!({
            "method": r.method().as_str(),
            "reflinked_files": r.reflinked_files,
            "reflinked_bytes": r.reflinked_bytes,
            "copied_files": r.copied_files,
            "copied_bytes": r.copied_bytes,
        })));
    });
    if !a.json {
        // A rebuild's `auto` snapshot and its pruning went out as events.
        if let Some(m) = &out.snapshot
            && m.kind == gogo::snapshot::Kind::Initial
        {
            println!("snapshot {} taken ({})", m.id, m.label);
        }
        println!(
            "game files, prefix and saves: {}\nmenu entry: {} (runs {}){}",
            out.dirs.root.display(),
            out.setup.launcher.desktop.display(),
            out.setup.launcher.launch_sh.display(),
            if out.setup.launcher.icon.is_none() {
                "; no icon found in the game"
            } else {
                ""
            }
        );
    }
    Ok(cli::exit::OK)
}

#[cfg(target_os = "linux")]
/// What setup did, as text or one JSON object (`extra` adds the build's keys).
fn print_linux_setup(
    out: &gogo::linuxapp::setup::SetupOutcome,
    json: bool,
    extra: impl FnOnce(&mut serde_json::Value),
) {
    for w in &out.warnings {
        eprintln!("warning: {w}");
    }
    if json {
        let mut v = serde_json::json!({
            "gogid": out.state.gogid,
            "runtime": out.state.runtime,
            "renderer": out.state.renderer,
            "umu_id": out.state.umu_id,
            "protonfix": out.state.protonfix,
            "stamp": out.state.stamp,
            "verbs": out.state.verbs,
            "verbs_installed": out.verbs_installed,
            "prefix_created": out.prefix_created,
            "runtime_installed": out.runtime_installed,
            "fixes": out.fixes.to_string(),
            "notes": out.notes,
            "warnings": out.warnings,
            "launch_sh": out.launcher.launch_sh,
            "desktop": out.launcher.desktop,
            "icon": out.launcher.icon,
        });
        extra(&mut v);
        println!("{v}");
        return;
    }
    for n in &out.notes {
        eprintln!("note: {n}");
    }
    eprint!("{}", out.fixes);
    println!(
        "{}: runtime {}{}, renderer {}, {}{}",
        out.state.gogid,
        out.state.runtime,
        if out.runtime_installed.is_some() {
            " (installed)"
        } else {
            ""
        },
        out.state.renderer,
        out.state.umu_id,
        if out.prefix_created {
            ", prefix created"
        } else {
            ""
        }
    );
}

#[cfg(target_os = "linux")]
/// `gogo linux-app --retarget <gogid> --runtime <ref> --renderer <id>`
/// (design 16 decision 7 on Linux): write the choices into L4's
/// `[platforms.linux]`, then set the game up on them.
fn linux_app_retarget(
    gogid: String,
    runtime: Option<String>,
    renderer: Option<crate::cli::LinuxRendererArg>,
    refresh: bool,
    no_snapshots: bool,
    json: bool,
) -> Result<i32, AppError> {
    use gogo::macapp::compat;
    if runtime.is_none() && renderer.is_none() {
        eprintln!("gogo: --retarget needs --runtime and/or --renderer");
        return Ok(cli::exit::USAGE);
    }
    let mut updates: Vec<(&str, Option<String>)> = Vec::new();
    if let Some(r) = renderer {
        // `auto` drops the override.
        updates.push((
            "renderer",
            match r.choice() {
                gogo::macapp::RendererChoice::Force(r) => Some(r.default_ref()),
                _ => None,
            },
        ));
    }
    if let Some(r) = &runtime {
        // `auto` drops it too: the baked runtime applies again.
        updates.push(("runtime", (r != "auto").then(|| r.clone())));
    }
    let updates: Vec<(&str, Option<&str>)> =
        updates.iter().map(|(k, v)| (*k, v.as_deref())).collect();
    let prompter: Option<std::sync::Arc<gogo::macapp::PromptFn>> =
        if std::io::IsTerminal::is_terminal(&std::io::stdin())
            && std::io::IsTerminal::is_terminal(&std::io::stderr())
            && !json
        {
            Some(prompt::renderer())
        } else {
            None
        };
    let act = depot_progress::Activity::new(depot_mode(false, json));
    let out = gogo::linuxapp::manage::retarget(
        &gogid,
        &updates,
        &gogo::linuxapp::setup::SetupOptions {
            refresh,
            prompt: prompter.as_deref(),
            runtime_snapshot: Some(auto_snapshots(no_snapshots)?),
        },
        &mut |ev| emit_live(ev, false, &act),
    )?;
    drop(act);
    if json {
        let setup = match &out.setup {
            Some(Ok(s)) => serde_json::json!({
                "ok": true,
                "runtime": s.state.runtime,
                "renderer": s.state.renderer,
                "prefix_created": s.prefix_created,
                "runtime_installed": s.runtime_installed,
                "warnings": s.warnings,
            }),
            Some(Err(e)) => serde_json::json!({ "ok": false, "error": e.to_string() }),
            None => serde_json::Value::Null,
        };
        println!(
            "{}",
            serde_json::json!({
                "gogid": gogid,
                "file": compat::user_path(&gogid),
                "changes": out.changes,
                "setup": setup,
            })
        );
    } else if out.changes.is_empty() {
        println!("{gogid}: nothing to change (the override file already says so)");
    } else {
        for c in &out.changes {
            println!(
                "{gogid}: {} {} -> {} ({})",
                c.key,
                c.old.as_deref().unwrap_or("(build default)"),
                c.new.as_deref().unwrap_or("(build default)"),
                compat::user_path(&gogid).display()
            );
        }
        match &out.setup {
            Some(Ok(s)) => {
                for w in &s.warnings {
                    eprintln!("warning: {w}");
                }
                println!(
                    "{gogid}: set up on runtime {}, renderer {}",
                    s.state.runtime, s.state.renderer
                );
            }
            Some(Err(e)) => {
                eprintln!("gogo: {gogid}: setup failed: {e}");
                eprintln!("the override is saved; the next launch tries the setup again");
            }
            None => println!("not built yet: the build applies it"),
        }
    }
    Ok(if matches!(out.setup, Some(Err(_))) {
        cli::exit::FAILED
    } else {
        cli::exit::OK
    })
}

/// `gogo linux-app move`: `dir` is checked as a library, and `None` or `games/`
/// itself means the default.
#[cfg(target_os = "linux")]
fn linux_app_move(
    gogid: &str,
    dir: Option<std::path::PathBuf>,
    dry_run: bool,
    yes: bool,
    json: bool,
) -> Result<i32, AppError> {
    use gogo::macapp::relocate::{MoveOptions, Target};
    let human = gogo::fmt::human_size;
    let library = match dir {
        // Not through `resolve_library(None)`: that would be `library.dir`.
        Some(d) => resolve_library(Some(d))?,
        None => None,
    };
    let target = library.map_or(Target::Default, Target::Library);
    // Twice at most: once more when the snapshots to delete changed between
    // the plan shown and the move's own (after its recovery).
    for attempt in 0..2 {
        // The real move settles an interrupted one before it plans: a plan
        // that can't be made yet (a copy an earlier move left in the way) is
        // the move's to report, after its recovery.
        let plan = match gogo::linuxapp::relocate::plan(gogid, &target, &MoveOptions::default()) {
            Ok(plan) => plan,
            Err(e) if dry_run => return Err(e.into()),
            Err(_) => {
                return linux_app_move_now(gogid, &target, &MoveOptions::default(), json);
            }
        };
        let mut lines = vec![format!(
            "move {gogid} ({}) by {}: {} -> {}",
            human(plan.bytes),
            plan.method.as_str(),
            plan.from.display(),
            plan.to.display()
        )];
        if plan.extra_bytes > 0 {
            lines.push(format!(
                "it needs {} free there until the copy is checked",
                human(plan.extra_bytes)
            ));
        }
        if let Some(s) = &plan.snapshots {
            lines.extend(s.lines(&plan.to));
        }
        if dry_run {
            if json {
                println!("{}", serde_json::to_string(&plan).unwrap());
            } else {
                println!("would {}", lines[0]);
                for l in &lines[1..] {
                    println!("{l}");
                }
            }
            return Ok(cli::exit::OK);
        }
        let mut opts = MoveOptions::default();
        // Deleting snapshots (design 20 §9.1, decision D5) is asked first, and
        // the yes covers the snapshots shown, no others.
        if let Some(s) = plan.snapshots.as_ref().filter(|s| s.needs_yes()) {
            if yes && attempt > 0 {
                eprintln!("plan:");
                for l in &lines {
                    eprintln!("  {l}");
                }
            }
            if let Some(code) =
                snapshot_cmd::ask(&lines, yes, "move it and delete these snapshots?")
            {
                return Ok(code);
            }
            opts.drop_snapshots = true;
            opts.confirmed = Some(s.snapshots.iter().map(|s| s.id).collect());
        }
        match linux_app_move_now(gogid, &target, &opts, json)? {
            PLAN_CHANGED => continue,
            code => return Ok(code),
        }
    }
    Err(AppError::from(gogo::macapp::error::MacAppError::Apps(
        format!("{gogid}'s snapshots keep changing while it is being moved: try again"),
    )))
}

/// [`linux_app_move_now`]'s answer when the snapshots to delete aren't the
/// ones agreed to: plan and ask again.
#[cfg(target_os = "linux")]
const PLAN_CHANGED: i32 = -1;

/// `gogo linux-app move`, past its plan and question.
#[cfg(target_os = "linux")]
fn linux_app_move_now(
    gogid: &str,
    target: &gogo::macapp::relocate::Target,
    opts: &gogo::macapp::relocate::MoveOptions,
    json: bool,
) -> Result<i32, AppError> {
    use gogo::macapp::relocate::MoveEvent;
    let human = gogo::fmt::human_size;
    // On a terminal the live status line, with a rate (#420); else each tenth.
    let act = depot_progress::Activity::new(depot_mode(false, json));
    let mut shown = (0, 0);
    let out = gogo::linuxapp::relocate::move_game(gogid, target, opts, &mut |ev| {
        let (what, kind, done, total) = match ev {
            MoveEvent::Copying { done, total } => ("copying", 1, *done, *total),
            MoveEvent::Verifying { done, total } => ("checking the copy", 2, *done, *total),
        };
        if act.live() {
            if kind == 2 && done >= total {
                // What is left: the old copy's delete and the records.
                act.working("finishing the move");
            } else {
                act.task(what, None, Some((done, Some(total))));
            }
            return;
        }
        let percent = (done * 100).checked_div(total).unwrap_or(100);
        let step = percent / 10 * 10;
        if !json && (kind, step) != shown {
            shown = (kind, step);
            eprintln!("{what}: {percent}% of {}", human(total));
        }
    });
    drop(act);
    if let Err(e) = &out
        && gogo::macapp::relocate::snapshots_changed(e)
    {
        eprintln!("gogo: {e}");
        return Ok(PLAN_CHANGED);
    }
    let out = out?;
    if json {
        println!("{}", serde_json::to_string(&out).unwrap());
        return Ok(cli::exit::OK);
    }
    for line in &out.recovered {
        println!("{line}");
    }
    println!(
        "moved {gogid} ({}) by {}: {} -> {}",
        human(out.plan.bytes),
        out.plan.method.as_str(),
        out.plan.from.display(),
        out.plan.to.display()
    );
    for line in out.snapshots.lines() {
        println!("{line}");
    }
    if let Some(reason) = out
        .plan
        .snapshots
        .as_ref()
        .and_then(|s| s.unavailable.as_ref())
    {
        println!(
            "snapshots: unavailable at {} ({reason})",
            out.plan.to.display()
        );
    }
    for w in &out.warnings {
        eprintln!("warning: {w}");
    }
    Ok(cli::exit::OK)
}

/// `gogo linux-app move --adopt`: `game_dir` is the game's own dir. Its parent
/// is checked as a library, and `games/` itself means the default.
#[cfg(target_os = "linux")]
fn linux_app_adopt(
    gogid: &str,
    game_dir: &std::path::Path,
    dry_run: bool,
    json: bool,
) -> Result<i32, AppError> {
    use gogo::macapp::relocate::Target;
    let game_dir = gogo::macapp::launch::absolute(game_dir);
    if game_dir.file_name().is_none_or(|n| n != gogid) {
        return Err(AppError::from(gogo::macapp::error::MacAppError::Library {
            path: game_dir,
            reason: format!(
                "--adopt takes the game's own dir, the one named {gogid} (not its library)"
            ),
        }));
    }
    let parent = game_dir.parent().unwrap_or(&game_dir);
    let target =
        resolve_library(Some(parent.to_path_buf()))?.map_or(Target::Default, Target::Library);
    if dry_run {
        let plan = gogo::linuxapp::relocate::adopt_plan(gogid, &target)?;
        if json {
            println!("{}", serde_json::to_string(&plan).unwrap());
        } else {
            println!(
                "would adopt {gogid} (nothing moves): {} -> {}",
                plan.from.display(),
                plan.to.display()
            );
        }
        return Ok(cli::exit::OK);
    }
    let out = gogo::linuxapp::relocate::adopt_game(gogid, &target)?;
    if json {
        println!("{}", serde_json::to_string(&out).unwrap());
        return Ok(cli::exit::OK);
    }
    for line in &out.recovered {
        println!("{line}");
    }
    println!(
        "adopted {gogid} (nothing moved): {} -> {}",
        out.plan.from.display(),
        out.plan.to.display()
    );
    for w in &out.warnings {
        eprintln!("warning: {w}");
    }
    Ok(cli::exit::OK)
}

#[cfg(target_os = "linux")]
/// `gogo linux-app <sub>` (no build).
fn linux_app_sub(sub: crate::cli::LinuxAppSub) -> Result<i32, AppError> {
    match sub {
        crate::cli::LinuxAppSub::Doctor { gogid, json } => {
            // vulkaninfo and the tree walks take seconds (#420).
            let report = depot_progress::Activity::new(depot_mode(false, json))
                .spin(&format!("checking {gogid}"), || {
                    gogo::linuxapp::doctor::doctor(&gogid, &gogo::linuxapp::doctor::Host::detect())
                })?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "gogid": report.gogid,
                        "ok": report.ok(),
                        "findings": report.findings,
                    })
                );
            } else {
                print_findings(&report.findings);
            }
            Ok(if report.ok() {
                cli::exit::OK
            } else {
                cli::exit::FAILED
            })
        }
        crate::cli::LinuxAppSub::Setup {
            gogid,
            refresh,
            no_snapshots,
            json,
        } => {
            gogo::macapp::compat::validate_gogid(&gogid)?;
            let prompter: Option<std::sync::Arc<gogo::macapp::PromptFn>> =
                if std::io::IsTerminal::is_terminal(&std::io::stdin())
                    && std::io::IsTerminal::is_terminal(&std::io::stderr())
                    && !json
                {
                    Some(prompt::renderer())
                } else {
                    None
                };
            let act = depot_progress::Activity::new(depot_mode(false, json));
            let out = gogo::linuxapp::setup::setup(
                &gogid,
                &gogo::linuxapp::setup::SetupOptions {
                    refresh,
                    prompt: prompter.as_deref(),
                    runtime_snapshot: Some(auto_snapshots(no_snapshots)?),
                },
                &mut |ev| emit_live(ev, false, &act),
            );
            drop(act);
            let out = out?;
            print_linux_setup(&out, json, |_| {});
            Ok(cli::exit::OK)
        }
        crate::cli::LinuxAppSub::Move {
            gogid,
            dir,
            default: _,
            adopt,
            dry_run,
            yes,
            json,
        } => match adopt {
            Some(game_dir) => linux_app_adopt(&gogid, &game_dir, dry_run, json),
            None => linux_app_move(&gogid, dir, dry_run, yes, json),
        },
        crate::cli::LinuxAppSub::NextRenderer { gogid, dry_run } => {
            match gogo::linuxapp::manage::next_renderer(&gogid, dry_run)? {
                Some(next) => {
                    println!("{next}");
                    Ok(cli::exit::OK)
                }
                None => {
                    eprintln!(
                        "gogo: {gogid}: {}",
                        gogo::macapp::renderer::NO_MORE_RENDERERS
                    );
                    Ok(cli::exit::FAILED)
                }
            }
        }
        crate::cli::LinuxAppSub::Exes { gogid, all, json } => {
            let l = phase_sync(json, "looking for executables", || {
                gogo::linuxapp::exes::list(&gogid, all)
            })?;
            if json {
                println!("{}", serde_json::to_string(&l).unwrap());
            } else {
                print!("{}", gogo::linuxapp::exes::render(&l));
            }
            Ok(cli::exit::OK)
        }
        crate::cli::LinuxAppSub::Run { gogid, exe, args } => linux_app_run(&gogid, &exe, &args),
        crate::cli::LinuxAppSub::Env { cmd } => linux_app_env(cmd),
        // Async: dispatched in `run`.
        crate::cli::LinuxAppSub::Dlc { .. } => unreachable!("linux-app dlc is async"),
        crate::cli::LinuxAppSub::Update { .. } => unreachable!("linux-app update is async"),
        crate::cli::LinuxAppSub::Snapshot { gogid, cmd } => snapshot_cmd::run(gogid, cmd),
        crate::cli::LinuxAppSub::Args {
            gogid,
            clear,
            json,
            args,
        } => linux_app_args(&gogid, clear, json, args),
        crate::cli::LinuxAppSub::Wrapper {
            gogid,
            clear,
            json,
            words,
        } => linux_app_wrapper(&gogid, clear, json, words),
        crate::cli::LinuxAppSub::Display {
            gogid,
            mode,
            hdr,
            monitor,
            no_monitor,
            clear,
            json,
        } => linux_app_display(&gogid, mode, hdr, monitor, no_monitor, clear, json),
        crate::cli::LinuxAppSub::Compat { gogid, json } => {
            let r = gogo::linuxapp::manage::compat(&gogid)?;
            if json {
                println!("{}", serde_json::json!(r));
                return Ok(cli::exit::OK);
            }
            for w in &r.warnings {
                eprintln!("warning: {w}");
            }
            println!("status: {}", r.status_line);
            if let Some(rt) = &r.runtime {
                println!("runtime: {rt}");
            }
            if let Some(x) = &r.renderer {
                println!(
                    "renderer: {} ({}); baked {} ({}); L1 auto would pick {} ({}){}",
                    x.effective,
                    x.effective_layer.describe(),
                    x.baked,
                    x.baked_layer.describe(),
                    x.auto,
                    x.reason,
                    x.applied
                        .as_ref()
                        .map(|a| format!("; last applied by setup: {a}"))
                        .unwrap_or_default()
                );
            }
            let lines = r.effective.display_lines();
            if lines.is_empty() {
                println!("no compat data for {gogid} (L1 verbs, L3 and L4 are empty)");
            }
            for line in lines {
                println!("{line}");
            }
            if let Some(fx) = &r.fixes {
                print!("{fx}");
            }
            if !r.variants.is_empty() {
                println!(
                    "variants (`*` runs; pick one with `variant = \"<name>\"` under \
                     [platforms.linux] in the L4 file):"
                );
                for v in &r.variants {
                    println!(
                        "  {} {:?}  {}{}{}",
                        if v.selected { "*" } else { " " },
                        v.name,
                        v.exe,
                        if v.args.is_empty() {
                            String::new()
                        } else {
                            format!(" {}", v.args.join(" "))
                        },
                        if v.default { "  (default)" } else { "" }
                    );
                }
            }
            if let Some(e) = &r.variant_error {
                eprintln!("error: {e}");
            }
            println!(
                "L4 override file: {}{}",
                r.user_file.display(),
                if r.built {
                    ""
                } else {
                    "\n(not built yet: L1 and the renderer show once `gogo linux-app <game>` ran)"
                }
            );
            Ok(cli::exit::OK)
        }
    }
}

#[cfg(target_os = "linux")]
/// An edit's changes, as `env set` / `unset` / `args` print them (secret
/// values hidden).
fn print_edit(out: &gogo::linuxapp::envargs::EditOutcome, json: bool) {
    use gogo::linuxapp::envargs;
    let hide = |key: &str, v: &Option<String>| {
        v.as_ref()
            .map(|v| envargs::shown(key.strip_prefix("env.").unwrap_or(key), v, false))
    };
    let changes: Vec<gogo::macapp::compat::Change> = out
        .changes
        .iter()
        .map(|c| gogo::macapp::compat::Change {
            key: c.key.clone(),
            old: hide(&c.key, &c.old),
            new: hide(&c.key, &c.new),
        })
        .collect();
    if json {
        println!(
            "{}",
            serde_json::json!({
                "gogid": out.gogid,
                "file": out.file,
                "changes": changes,
                "warnings": out.warnings,
                "built": out.built,
            })
        );
        return;
    }
    for w in &out.warnings {
        eprintln!("warning: {w}");
    }
    if changes.is_empty() {
        println!(
            "{}: nothing to change (the override file already says so)",
            out.gogid
        );
        return;
    }
    for c in &changes {
        let v = |v: &Option<String>| match v {
            Some(v) if c.key.starts_with("env.") => format!("{v:?}"),
            Some(v) => v.clone(),
            None => "(unset)".into(),
        };
        println!(
            "{}: {} {} -> {} ({})",
            out.gogid,
            c.key,
            v(&c.old),
            v(&c.new),
            out.file.display()
        );
    }
    if out.built {
        println!("the next launch applies it (it runs setup on the changed override first)");
    } else {
        println!("not built yet: the build applies it");
    }
}

#[cfg(target_os = "linux")]
/// `gogo linux-app env list|set|unset`.
fn linux_app_env(cmd: crate::cli::LinuxEnvCmd) -> Result<i32, AppError> {
    use crate::cli::LinuxEnvCmd;
    use gogo::linuxapp::doctor::Host;
    use gogo::linuxapp::{catalogue, envargs};
    match cmd {
        LinuxEnvCmd::List {
            gogid,
            reveal,
            all: true,
            json,
        } => {
            let r = catalogue::report(&gogid, &Host::detect(), reveal)?;
            if json {
                println!("{}", serde_json::json!(r));
                return Ok(cli::exit::OK);
            }
            for w in &r.warnings {
                eprintln!("warning: {w}");
            }
            print!("{}", env_catalogue_text(&r));
            Ok(cli::exit::OK)
        }
        LinuxEnvCmd::Describe {
            gogid,
            name,
            reveal,
            json,
        } => {
            let r = catalogue::report(&gogid, &Host::detect(), reveal)?;
            let Some(e) = r.find(&name) else {
                let similar = r.similar(&name);
                let hint = if similar.is_empty() {
                    format!("`gogo linux-app env list {gogid} --all` lists what it has")
                } else {
                    format!("did you mean: {}", similar.join(", "))
                };
                eprintln!(
                    "gogo: {name:?} isn't a switch gogo knows, and {} doesn't show it; {hint}",
                    r.build
                        .as_ref()
                        .and_then(|b| b.version.as_deref())
                        .unwrap_or("this game's build")
                );
                return Ok(cli::exit::FAILED);
            };
            if json {
                println!("{}", serde_json::json!(e));
                return Ok(cli::exit::OK);
            }
            print!("{}", env_describe_text(&r, e));
            Ok(cli::exit::OK)
        }
        LinuxEnvCmd::List {
            gogid,
            reveal,
            all: false,
            json,
        } => {
            let r = envargs::env_report(&gogid, reveal)?;
            if json {
                println!("{}", serde_json::json!(r));
                return Ok(cli::exit::OK);
            }
            for w in &r.warnings {
                eprintln!("warning: {w}");
            }
            print!("{}", env_list_text(&r));
            Ok(cli::exit::OK)
        }
        LinuxEnvCmd::Set { gogid, pairs, json } => {
            let mut parsed = Vec::new();
            for p in &pairs {
                let Some((k, v)) = p.split_once('=') else {
                    eprintln!("gogo: {p:?} isn't KEY=VALUE");
                    return Ok(cli::exit::USAGE);
                };
                parsed.push((k.to_string(), v.to_string()));
            }
            if let Err(why) = envargs::check_set(&parsed) {
                eprintln!("gogo: {gogid}: {why}");
                return Ok(cli::exit::USAGE);
            }
            print_edit(&envargs::set_env(&gogid, &parsed)?, json);
            Ok(cli::exit::OK)
        }
        LinuxEnvCmd::Unset { gogid, keys, json } => {
            for k in &keys {
                if let Err(why) = envargs::check_key(k) {
                    eprintln!("gogo: {k:?}: {why}");
                    return Ok(cli::exit::USAGE);
                }
            }
            print_edit(&envargs::unset_env(&gogid, &keys)?, json);
            Ok(cli::exit::OK)
        }
    }
}

#[cfg(target_os = "linux")]
/// `gogo linux-app wrapper <gogid> [-- <words…> | --clear]`.
fn linux_app_wrapper(
    gogid: &str,
    clear: bool,
    json: bool,
    words: Vec<String>,
) -> Result<i32, AppError> {
    use gogo::linuxapp::envargs;
    if clear || !words.is_empty() {
        if !clear && let Err(why) = envargs::check_wrapper(&words) {
            eprintln!("gogo: {gogid}: {why}");
            return Ok(cli::exit::USAGE);
        }
        let new = (!clear).then_some(words);
        print_edit(&envargs::set_wrapper(gogid, new)?, json);
        return Ok(cli::exit::OK);
    }
    let r = envargs::wrapper_report(gogid)?;
    if json {
        println!("{}", serde_json::json!(r));
        return Ok(cli::exit::OK);
    }
    for w in &r.warnings {
        eprintln!("warning: {w}");
    }
    print!("{}", wrapper_text(&r));
    Ok(cli::exit::OK)
}

#[cfg(target_os = "linux")]
/// `gogo linux-app run <gogid> <task|path> [-- <args…>]`: the game's
/// `launch.sh` takes this process's place (design 18 §7.10), so a TERM sent
/// to it is the launcher's and the exit code is the run's.
fn linux_app_run(gogid: &str, exe: &str, args: &[String]) -> Result<i32, AppError> {
    use std::os::unix::process::CommandExt as _;
    let plan = gogo::linuxapp::exes::plan_run(gogid, exe, args)?;
    eprintln!(
        "gogo: running {}{} once in {gogid}'s prefix",
        plan.label,
        plan.index
            .map(|i| format!(" (play task {i})"))
            .unwrap_or_default()
    );
    let err = std::process::Command::new(&plan.launch_sh)
        .args(plan.argv())
        .exec();
    eprintln!("gogo: {}: {err}", plan.launch_sh.display());
    Ok(cli::exit::FAILED)
}

#[cfg(target_os = "linux")]
/// `gogo linux-app display <gogid> [wayland|x11] [--hdr …] [--monitor …]
/// [--no-monitor] [--clear]`.
fn linux_app_display(
    gogid: &str,
    mode: Option<String>,
    hdr: Option<String>,
    monitor: Option<String>,
    no_monitor: bool,
    clear: bool,
    json: bool,
) -> Result<i32, AppError> {
    use gogo::linuxapp::display;
    use gogo::macapp::compat::{DisplayMode, Hdr};
    if clear || no_monitor || mode.is_some() || hdr.is_some() || monitor.is_some() {
        // clap only lets the spellings through.
        let mode = mode.as_deref().and_then(DisplayMode::from_key);
        let hdr = hdr.as_deref().and_then(Hdr::from_key);
        let out = if clear {
            display::set_display(gogid, Some(None), Some(None), Some(None))?
        } else {
            let monitor = if no_monitor {
                Some(None)
            } else {
                monitor.map(Some)
            };
            display::set_display(gogid, mode.map(Some), hdr.map(Some), monitor)?
        };
        print_edit(&out, json);
        return Ok(cli::exit::OK);
    }
    let r = display::display_report(gogid)?;
    if json {
        println!("{}", serde_json::json!(r));
        return Ok(cli::exit::OK);
    }
    for w in &r.warnings {
        eprintln!("warning: {w}");
    }
    print!("{}", display_text(&r));
    Ok(cli::exit::OK)
}

#[cfg(target_os = "linux")]
/// A display mode's line in `display` and `env list`.
fn display_line(v: &gogo::linuxapp::display::DisplayView) -> String {
    let key = |m: Option<gogo::macapp::compat::DisplayMode>| m.map_or("(none)", |m| m.key());
    let mut out = format!("display: {}\n", v.describe());
    if v.pending {
        let hdr = v
            .applied_hdr
            .map(|h| format!(", hdr {}", h.key()))
            .unwrap_or_default();
        let monitor = v
            .applied_monitor
            .as_ref()
            .map(|m| format!(", monitor {m}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "pending: launch.conf has {}{hdr}{monitor}; the next launch's setup writes the new one\n",
            key(v.applied_mode)
        ));
    }
    out
}

#[cfg(target_os = "linux")]
fn display_text(r: &gogo::linuxapp::display::DisplayReport) -> String {
    use gogo::macapp::compat::DisplayMode;
    use std::fmt::Write as _;
    let mut out = match &r.display {
        Some(v) => display_line(v),
        None => format!(
            "display: (none)  (the environment decides; `gogo linux-app display {} wayland` \
             sets Wine's Wayland driver)\n",
            r.gogid
        ),
    };
    let mode = r
        .display
        .as_ref()
        .and_then(|v| v.mode.as_ref())
        .map(|m| m.value);
    match mode {
        Some(DisplayMode::Wayland) => {
            let env: Vec<String> = r
                .wayland_env
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            let _ = writeln!(
                out,
                "with a Wayland socket at launch the game gets {}; without one it runs on X11, \
                 and the launcher logs why",
                env.join(" ")
            );
        }
        Some(DisplayMode::X11) => {
            let _ = writeln!(
                out,
                "the launcher unsets PROTON_ENABLE_WAYLAND and PROTON_USE_WAYLAND for the game"
            );
        }
        None => {}
    }
    if let Some(p) = r.picks {
        let _ = writeln!(
            out,
            "a launch from this shell: {} ({}; a desktop launch has the session's)",
            p.key(),
            r.session.describe()
        );
    }
    let _ = writeln!(out, "your override file: {}", r.user_file.display());
    out
}

#[cfg(target_os = "linux")]
/// A wrapper's line in `wrapper` and `env list`.
fn wrapper_line(v: &gogo::linuxapp::envargs::WrapperView) -> String {
    use gogo::linuxapp::envargs::shell_words;
    let mut out = match v.layer {
        Some(l) => format!("wrapper: {}  ({})", shell_words(&v.words), l.describe()),
        None => "wrapper: (none)".to_string(),
    };
    if v.pending {
        out.push_str(&if v.applied.is_empty() {
            "\npending: the next launch's setup applies it".to_string()
        } else {
            format!(
                "\npending: launch.conf still has {}; the next launch's setup writes the new one",
                shell_words(&v.applied)
            )
        });
    }
    out.push('\n');
    out
}

#[cfg(target_os = "linux")]
fn wrapper_text(r: &gogo::linuxapp::envargs::WrapperReport) -> String {
    use std::fmt::Write as _;
    let mut out = match &r.wrapper {
        Some(v) => wrapper_line(v),
        None => format!(
            "wrapper: (none)  (`gogo linux-app wrapper {} -- gamescope -f --` sets one)\n",
            r.gogid
        ),
    };
    let _ = writeln!(
        out,
        "it runs outside umu-run with the environment launch.sh starts with; the game's env \
         applies inside it"
    );
    let _ = writeln!(out, "your override file: {}", r.user_file.display());
    out
}

#[cfg(target_os = "linux")]
/// `gogo linux-app args <gogid> [-- <args…> | --clear]`.
fn linux_app_args(
    gogid: &str,
    clear: bool,
    json: bool,
    args: Vec<String>,
) -> Result<i32, AppError> {
    use gogo::linuxapp::envargs;
    if clear || !args.is_empty() {
        for a in &args {
            if let Err(why) = envargs::check_value(&format!("arg {a:?}"), a) {
                eprintln!("gogo: {gogid}: {why}");
                return Ok(cli::exit::USAGE);
            }
        }
        let new = (!clear).then_some(args);
        print_edit(&envargs::set_args(gogid, new)?, json);
        return Ok(cli::exit::OK);
    }
    let r = envargs::args_report(gogid)?;
    if json {
        println!("{}", serde_json::json!(r));
        return Ok(cli::exit::OK);
    }
    for w in &r.warnings {
        eprintln!("warning: {w}");
    }
    print!("{}", args_text(&r));
    Ok(cli::exit::OK)
}

#[cfg(target_os = "linux")]
/// `env list`'s human output: one `KEY=VALUE  (source)` line per key that
/// applies, the values it beats indented under it.
fn env_list_text(r: &gogo::linuxapp::envargs::EnvReport) -> String {
    use gogo::linuxapp::envargs::EnvState;
    use std::fmt::Write as _;
    let mut out = format!(
        "env of {} at its next launch (each line beats the environment launch.sh \
         starts with, and the values listed under it):\n",
        r.gogid
    );
    let width = r
        .lines
        .iter()
        .filter(|l| !matches!(l.state, EnvState::Overridden { .. }))
        .map(|l| l.key.len() + l.value.len() + 1)
        .max()
        .unwrap_or(0)
        .min(40);
    for l in &r.lines {
        let kv = format!("{}={}", l.key, l.value);
        let _ = match &l.state {
            EnvState::Applies => writeln!(out, "{kv:<width$}  ({})", l.source),
            EnvState::Pending => writeln!(
                out,
                "{kv:<width$}  ({}; pending: the next launch's setup applies it)",
                l.source
            ),
            EnvState::Overridden { .. } => writeln!(out, "  over {kv}  ({})", l.source),
        };
    }
    if !r.built {
        out.push_str("(not built yet: the renderer's and the launcher's values show once it is)\n");
    }
    if let Some(v) = &r.wrapper {
        out.push_str(&wrapper_line(v));
    }
    if let Some(v) = &r.display {
        out.push_str(&display_line(v));
    }
    let _ = writeln!(
        out,
        "your override file: {} (`gogo linux-app env set {} KEY=VALUE`)",
        r.user_file.display(),
        r.gogid
    );
    out
}

#[cfg(target_os = "linux")]
/// One `KEY=VALUE  (source)` line for a catalogue entry's active value, as
/// `env list` words it.
fn env_active_line(l: &gogo::linuxapp::envargs::EnvLine) -> String {
    use gogo::linuxapp::envargs::EnvState;
    match &l.state {
        EnvState::Applies => format!("{}={}  ({})", l.key, l.value, l.source),
        EnvState::Pending => format!(
            "{}={}  ({}; pending: the next launch's setup applies it)",
            l.key, l.value, l.source
        ),
        EnvState::Overridden { .. } => format!("over {}={}  ({})", l.key, l.value, l.source),
    }
}

#[cfg(target_os = "linux")]
/// The heading of `env list --all`: the build, the renderer, the GPUs.
fn env_catalogue_heading(r: &gogo::linuxapp::catalogue::CatalogueReport) -> String {
    let build = r
        .build
        .as_ref()
        .map_or("its Proton build isn't installed", |b| {
            b.version
                .as_deref()
                .unwrap_or("a build with no version file")
        });
    let renderer = r.renderer.as_deref().unwrap_or("not picked yet");
    let gpus = if r.gpus.is_empty() {
        "none seen".to_string()
    } else {
        r.gpus.join(", ")
    };
    format!("{} on {build}: renderer {renderer}, GPU {gpus}", r.gogid)
}

#[cfg(target_os = "linux")]
/// `env list --all`'s human output: a section per component, one line per
/// switch (so `grep` works), and what sets it under the line.
fn env_catalogue_text(r: &gogo::linuxapp::catalogue::CatalogueReport) -> String {
    use gogo::linuxapp::catalogue::table::Component;
    use gogo::linuxapp::catalogue::{Applies, Entry};
    use std::fmt::Write as _;
    let mut out = format!("{}\n", env_catalogue_heading(r));
    if let Some(b) = &r.build {
        let parts: Vec<String> = b
            .components
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect();
        if !parts.is_empty() {
            let _ = writeln!(out, "bundled: {}", parts.join(", "));
        }
    }
    out.push_str(
        "yes: does something here; no: doesn't, and why; ?: gogo can't tell.\n\
         [ ]: where gogo knows it from (script, fixes, sample and dll are this build; table is gogo's).\n",
    );
    let tags = |e: &Entry| {
        let mut t: Vec<&str> = e.sources.iter().map(|s| s.tag()).collect();
        t.dedup();
        t.join(", ")
    };
    let mut section = None;
    let shown: Vec<&Entry> = r
        .entries
        .iter()
        .filter(|e| e.component != Component::Plumbing || !e.active.is_empty())
        .collect();
    for (i, e) in shown.iter().enumerate() {
        if section != Some(e.component) {
            section = Some(e.component);
            let _ = write!(out, "\n{}", e.component.id());
            if let Some(v) = r
                .build
                .as_ref()
                .and_then(|b| b.components.get(e.component.id()))
            {
                let _ = write!(out, " {v}");
            }
            if let Some(l) = e.component.link() {
                let _ = write!(out, " ({l})");
            }
            out.push('\n');
        }
        let width = shown[i..]
            .iter()
            .take_while(|o| o.component == e.component)
            .chain(
                shown[..i]
                    .iter()
                    .rev()
                    .take_while(|o| o.component == e.component),
            )
            .map(|o| o.name.len())
            .max()
            .unwrap_or(0)
            .min(36);
        let mark = match &e.applies {
            Applies::Yes => "yes",
            Applies::No { .. } => "no",
            Applies::Unknown { .. } => "?",
        };
        let plumbing = e.component == Component::Plumbing;
        let about = match e.about.as_deref() {
            Some(a) => a,
            None if plumbing => "Not a switch: Steam's, umu's or the build's own.",
            None => "(not in gogo's table yet)",
        };
        let _ = write!(out, "{mark:<3} {:<width$}  {about}", e.name);
        if let Some(d) = &e.default {
            let _ = write!(out, " Default: {d}.");
        }
        if let (false, Applies::No { why } | Applies::Unknown { why }) = (plumbing, &e.applies) {
            let _ = write!(out, " ({why})");
        }
        let t = tags(e);
        if !t.is_empty() {
            let _ = write!(out, " [{t}]");
        }
        out.push('\n');
        for l in &e.active {
            let _ = writeln!(out, "    now: {}", env_active_line(l));
        }
    }
    let plumbing: Vec<&str> = r
        .entries
        .iter()
        .filter(|e| e.component == Component::Plumbing && e.active.is_empty())
        .map(|e| e.name.as_str())
        .collect();
    if !plumbing.is_empty() {
        let _ = writeln!(
            out,
            "\nnot switches (Steam's, umu's and the build's own): {}",
            plumbing.join(", ")
        );
    }
    let undescribed = r.undescribed();
    if undescribed > 0 {
        let _ = writeln!(
            out,
            "\n{undescribed} of these the build has and gogo's table doesn't describe yet \
             (reviewed for {})",
            r.reviewed_for
        );
    }
    let _ = writeln!(
        out,
        "\none in full: gogo linux-app env describe {0} <NAME>; set one: gogo linux-app env set {0} KEY=VALUE",
        r.gogid
    );
    out
}

#[cfg(target_os = "linux")]
/// `env describe`'s human output.
fn env_describe_text(
    r: &gogo::linuxapp::catalogue::CatalogueReport,
    e: &gogo::linuxapp::catalogue::Entry,
) -> String {
    use gogo::linuxapp::catalogue::table::Kind;
    use gogo::linuxapp::catalogue::{Applies, InBuild, Source};
    use std::fmt::Write as _;
    let kind = match e.kind {
        Kind::Env => "env var",
        Kind::Vkd3dFlag => "VKD3D_CONFIG flag",
        Kind::DxvkConf => "dxvk.conf key, through DXVK_CONFIG",
        Kind::Command => "wrapper command",
    };
    let mut out = format!("{} ({}, {kind})\n", e.name, e.component.id());
    let _ = writeln!(
        out,
        "  {}",
        e.about.as_deref().unwrap_or("(not in gogo's table yet)")
    );
    if let Some(d) = &e.default {
        let _ = writeln!(out, "default:  {d}");
    }
    let applies = match &e.applies {
        Applies::Yes => "yes".to_string(),
        Applies::No { why } => format!("no: {why}"),
        Applies::Unknown { why } => format!("can't tell: {why}"),
    };
    let _ = writeln!(out, "applies:  {applies} ({})", env_catalogue_heading(r));
    let build = r
        .build
        .as_ref()
        .and_then(|b| b.version.as_deref())
        .unwrap_or("this build");
    let in_build = match e.in_build {
        InBuild::Found => format!("{build} has it"),
        InBuild::NotFound => format!("no trace of it in {build}"),
        InBuild::NotChecked => format!("not checked against {build}"),
        InBuild::NotOfBuild => "a host tool, not part of the build".to_string(),
    };
    let _ = writeln!(out, "build:    {in_build}");
    let sources: Vec<String> = e
        .sources
        .iter()
        .map(|s| match s {
            Source::ProtonScript { token: Some(t) } => {
                format!("proton script (STEAM_COMPAT_CONFIG token {t})")
            }
            Source::ProtonScript { token: None } => "proton script".to_string(),
            Source::Protonfixes => "protonfixes".to_string(),
            Source::SampleSettings => "user_settings.sample.py".to_string(),
            Source::Dll { file } => file.clone(),
            Source::Table => "gogo's table".to_string(),
        })
        .collect();
    if !sources.is_empty() {
        let _ = writeln!(out, "from:     {}", sources.join(", "));
    }
    if e.proton_sets {
        let _ = writeln!(
            out,
            "note:     the proton script sets this itself in some configurations"
        );
    }
    if e.active.is_empty() {
        let _ = writeln!(out, "now:      not set for this game");
    }
    for l in &e.active {
        let _ = writeln!(out, "now:      {}", env_active_line(l));
    }
    match e.set_with(&r.gogid) {
        _ if e.in_build == InBuild::NotFound => {}
        Ok(cmd) => {
            let _ = writeln!(out, "set with: {cmd}");
        }
        Err(why) => {
            let _ = writeln!(out, "set with: can't: {why}");
        }
    }
    if let Some(l) = e.link {
        let _ = writeln!(out, "docs:     {l}");
    }
    out
}

#[cfg(target_os = "linux")]
/// `args`' human output.
fn args_text(r: &gogo::linuxapp::envargs::ArgsReport) -> String {
    use gogo::linuxapp::envargs;
    use std::fmt::Write as _;
    let words = |a: &[String]| {
        if a.is_empty() {
            "(none)".to_string()
        } else {
            envargs::shell_words(a)
        }
    };
    let mut out = String::new();
    if let Some(exe) = &r.exe {
        let _ = writeln!(out, "exe: {exe}");
    }
    let _ = writeln!(
        out,
        "game args: {}  ({})",
        words(&r.game_args),
        r.game_args_from
    );
    let _ = match &r.extra_args {
        Some(x) => writeln!(
            out,
            "extra args: {}  ({})",
            words(&x.value),
            x.layer.describe()
        ),
        None => writeln!(
            out,
            "extra args: (none)  (`gogo linux-app args {} -- <args…>` adds some)",
            r.gogid
        ),
    };
    let _ = writeln!(
        out,
        "the exe gets: {} (then any args launch.sh itself is given)",
        words(&r.launch_args)
    );
    if r.pending {
        let _ = writeln!(
            out,
            "pending: launch.conf still has {}; the next launch's setup writes the new ones",
            words(r.applied.as_deref().unwrap_or_default())
        );
    }
    if !r.built {
        out.push_str("(GOG's args show once it is built)\n");
    }
    let _ = writeln!(out, "your override file: {}", r.user_file.display());
    out
}

// — runtimes (design 12 §4/§10): Wine runtime catalog + store management —

/// Which catalog entries a `runtimes` / `renderers` command works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shelf {
    /// Wine runtimes (and the winetricks tool provisioned with them).
    Runtimes,
    Renderers,
}

impl Shelf {
    fn command(self) -> &'static str {
        match self {
            Shelf::Runtimes => "runtimes",
            Shelf::Renderers => "renderers",
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Shelf::Runtimes => "runtime",
            Shelf::Renderers => "renderer",
        }
    }

    fn holds(self, entry: &gogo::macapp::catalog::CatalogEntry) -> bool {
        use gogo::macapp::catalog::Kind;
        matches!(entry.kind, Kind::Renderer { .. }) == (self == Shelf::Renderers)
    }

    /// The shelf an id (`<id>[/<ver>]`) belongs on; ids the catalog no
    /// longer has are runtimes (the store only kept those).
    fn of(target: &str) -> Shelf {
        let id = target.split_once('/').map_or(target, |(i, _)| i);
        match gogo::macapp::catalog::find(id) {
            Some(e) if Shelf::Renderers.holds(e) => Shelf::Renderers,
            _ => Shelf::Runtimes,
        }
    }
}

/// The unmanaged Proton builds `runtimes list` shows (design 18 §1): what
/// discovery finds, plus `[linux] default_runtime`'s `path:` build; each
/// with whether it's the default.
fn unmanaged_builds(
    default_runtime: Option<&str>,
) -> Vec<(gogo::linuxapp::proton::Unmanaged, bool)> {
    use gogo::linuxapp::{proton, umu};
    let default = match proton::default_ref(default_runtime) {
        Ok(proton::ProtonRef::Path(p)) => Some(p),
        _ => None,
    };
    let same = |a: &std::path::Path, b: &std::path::Path| {
        a == b || std::fs::canonicalize(a).ok() == std::fs::canonicalize(b).ok()
    };
    let mut out: Vec<(proton::Unmanaged, bool)> = proton::discover(&umu::Host::real())
        .into_iter()
        .map(|b| {
            let d = default.as_deref().is_some_and(|p| same(p, &b.path));
            (b, d)
        })
        .collect();
    if let Some(p) = default
        && !out.iter().any(|(_, d)| *d)
    {
        out.insert(0, (proton::inspect(&p, "[linux] default_runtime"), true));
    }
    out
}

/// `macOS and Linux`, for listings.
fn os_list(os: &[gogo::macapp::catalog::Os]) -> String {
    os.iter()
        .map(|o| o.label())
        .collect::<Vec<_>>()
        .join(" and ")
}

/// A version as listings show it: a release tag's leading `v` dropped
/// (`v7.1.0.0` and `7.1.0.0` are one version).
fn shown_version(v: &str) -> &str {
    v.strip_prefix('v')
        .filter(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or(v)
}

/// Does `list` show `entry` on `os`? This OS's entries; `--all` adds the
/// rest (design 18 §6).
fn listed(
    entry: &gogo::macapp::catalog::CatalogEntry,
    os: gogo::macapp::catalog::Os,
    all: bool,
) -> bool {
    all || entry.runs_on_os(os)
}

/// `gogo runtimes|renderers list|install|prune`. `list` checks the latest
/// version online with a short client timeout (`--offline` skips) and shows
/// this OS's entries (`--all`: every OS's); `install` takes this OS's.
async fn store_cmd(shelf: Shelf, cmd: crate::cli::StoreCmd) -> Result<i32, AppError> {
    use gogo::macapp::{catalog, store};

    let (command, noun) = (shelf.command(), shelf.noun());
    let os = catalog::Os::current();
    // On Linux the default runtime is `[linux] default_runtime`, else
    // GE-Proton (design 18 §1); on the Mac the catalog's.
    let linux_default = (os == catalog::Os::Linux)
        .then(|| gogo::linuxapp::stored_key(&gogo::config::Config::path(), "default_runtime"))
        .flatten();
    let is_default = |id: &str| match os {
        catalog::Os::Macos => id == catalog::DEFAULT_RUNTIME,
        catalog::Os::Linux => {
            matches!(
                gogo::linuxapp::proton::default_ref(linux_default.as_deref()),
                Ok(gogo::linuxapp::proton::ProtonRef::Catalog { id: d, .. }) if d == id
            )
        }
    };
    match cmd {
        crate::cli::StoreCmd::List { json, offline, all } => {
            let runtimes = store::load_manifest();
            let mut cached = runtimes.clone();
            cached.extend(store::load_renderer_manifest());
            let http = if offline {
                None
            } else {
                catalog::http_client().ok()
            };
            let mut json_rows: Vec<serde_json::Value> = Vec::new();
            let mut footnotes: Vec<String> = Vec::new();
            let mut table = output::table();
            table.set_header(vec!["ID", "STATUS", "LATEST", "SIZE", "CACHED", "USED BY"]);
            let shown: Vec<_> = catalog::entries()
                .iter()
                .filter(|e| shelf.holds(e) && listed(e, os, all))
                .collect();
            // One request per entry, online (#422): the count on the status line.
            let act = depot_progress::Activity::new(depot_mode(false, json));
            let mut probed = 0u64;
            let probes = shown
                .iter()
                .filter(|e| {
                    http.is_some()
                        && e.discovery.is_some()
                        && !matches!(e.kind, catalog::Kind::Tool { .. })
                })
                .count() as u64;
            for entry in shown {
                let tool = matches!(entry.kind, catalog::Kind::Tool { .. });
                let here = entry.runs_on_os(os);
                let latest = match &http {
                    Some(http) if entry.discovery.is_some() && !tool => {
                        act.counting("checking for new versions", "entries", probed, probes);
                        probed += 1;
                        catalog::probe(entry, None, http, catalog::GITHUB_API)
                            .await
                            .ok()
                    }
                    _ => None,
                };
                let mine: Vec<&store::ManifestEntry> = cached
                    .iter()
                    .filter(|m| m.id == entry.id && !store::is_derived(&m.version))
                    .collect();
                // Runtimes built from a base runtime and an overlay renderer:
                // under the base id for a runtime, `<base>/<ver>` for the
                // renderer they were made with.
                let derived: Vec<String> = runtimes
                    .iter()
                    .filter(|m| match m.version.split_once('+') {
                        None => false,
                        Some(_) if shelf == Shelf::Runtimes => m.id == entry.id,
                        Some((_, with)) => with.starts_with(&format!("{}-", entry.id)),
                    })
                    .map(|m| match shelf {
                        Shelf::Runtimes => m.version.clone(),
                        Shelf::Renderers => format!("{}/{}", m.id, m.version),
                    })
                    .collect();
                let used_by = store::users_of(entry.id);
                let proton = matches!(entry.kind, catalog::Kind::Proton { .. });
                json_rows.push(serde_json::json!({
                    "id": entry.id,
                    "default": is_default(entry.id),
                    "status": entry.status.key(),
                    "gone_since": match &entry.status {
                        catalog::Status::Gone { since, .. } => Some(since),
                        _ => None,
                    },
                    "license": entry.license,
                    "arch": if here { entry.arch_label_on(os) } else { entry.arch_label() },
                    "os": entry.os.iter().map(|o| o.key()).collect::<Vec<_>>(),
                    "installable": here,
                    "vendor_signed": entry.vendor_signed,
                    "latest": latest.as_ref().map(|l| serde_json::json!({
                        "tag": l.tag,
                        "bytes": l.size,
                    })),
                    "cached": mine.iter().map(|m| {
                        let mut row = serde_json::json!({
                            "version": m.version,
                            "bytes": m.bytes,
                            "fetched_at": m.fetched_at,
                        });
                        // A build in a shared tools dir (design 18 §2.2).
                        if let Some(p) = &m.path {
                            row["path"] = serde_json::json!(p);
                        }
                        row
                    }).collect::<Vec<_>>(),
                    "derived": derived,
                    "used_by": used_by,
                }));
                let versions: Vec<&str> = mine.iter().map(|m| shown_version(&m.version)).collect();
                if tool && !here {
                    footnotes.push(format!(
                        "{}: {} only, not installable on {}",
                        entry.id,
                        os_list(entry.os),
                        os.label()
                    ));
                    continue;
                }
                if tool {
                    // Not a choice: gogo fetches its pinned version itself.
                    let pinned = entry.discovery.as_ref().and_then(|d| d.pinned_version());
                    let pinned = pinned.map(|p| format!(", pinned {p}")).unwrap_or_default();
                    footnotes.push(match versions.as_slice() {
                        [] => format!(
                            "{}: not cached{pinned}; downloaded when a game needs it",
                            entry.id
                        ),
                        v => format!(
                            "{}: cached {}{pinned}; updated when a game needs it",
                            entry.id,
                            v.join(", ")
                        ),
                    });
                    continue;
                }
                let status = match &entry.status {
                    // A retired entry only matters while copies of it remain.
                    catalog::Status::Gone { .. } if mine.is_empty() => continue,
                    _ if !here => format!(
                        "{} only, not installable on {}",
                        os_list(entry.os),
                        os.label()
                    ),
                    catalog::Status::Available if is_default(entry.id) => {
                        "available · default".to_string()
                    }
                    catalog::Status::Available | catalog::Status::Tool => "available".to_string(),
                    catalog::Status::ConsentRequired => "needs consent (Apple licence)".to_string(),
                    catalog::Status::Gone { since, .. } => {
                        format!("gone since {since} (cached copies still work)")
                    }
                };
                let latest_tag = latest.as_ref().map(|l| shown_version(&l.tag));
                let mut cached_col = if versions.is_empty() {
                    "-".to_string()
                } else if proton {
                    // 1.5 GB each (R5), and where: the store or a tools dir.
                    mine.iter()
                        .map(|m| {
                            let at = if m.path.is_some() {
                                ", compatibilitytools.d"
                            } else {
                                ""
                            };
                            format!(
                                "{} ({}{at})",
                                shown_version(&m.version),
                                gogo::fmt::human_size(m.bytes)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                } else {
                    versions.join(", ")
                };
                if let Some(tag) = latest_tag
                    && !versions.is_empty()
                    && !versions.contains(&tag)
                {
                    cached_col.push_str(" (update available)");
                }
                table.add_row(vec![
                    entry.id.to_string(),
                    status,
                    latest_tag.unwrap_or("-").to_string(),
                    latest
                        .as_ref()
                        .map(|l| gogo::fmt::human_size(l.size))
                        .unwrap_or_else(|| "-".into()),
                    cached_col,
                    if used_by.is_empty() {
                        "-".to_string()
                    } else {
                        used_by.join(", ")
                    },
                ]);
            }
            drop(act);
            // Proton builds gogo didn't install (design 18 §1), Linux only.
            let unmanaged = if os == catalog::Os::Linux && shelf == Shelf::Runtimes {
                unmanaged_builds(linux_default.as_deref())
            } else {
                Vec::new()
            };
            if json {
                let mut doc = serde_json::json!({ "entries": json_rows });
                if os == catalog::Os::Linux && shelf == Shelf::Runtimes {
                    doc["unmanaged"] = unmanaged
                        .iter()
                        .map(|(b, default)| {
                            serde_json::json!({
                                "ref": format!("path:{}", b.path.display()),
                                "path": b.path,
                                "source": b.source,
                                "version": b.version,
                                "require_tool_appid": b.appid,
                                "default": default,
                                "problems": b.problems(),
                            })
                        })
                        .collect();
                }
                println!("{doc}");
            } else if table.row_count() == 0 && !all {
                // Linux before its first runtimes (#169).
                println!(
                    "no {command} for {} in the catalog yet: `--all` lists the other OSes'",
                    os.label()
                );
                for note in footnotes {
                    println!("{note}");
                }
            } else {
                println!("{table}");
                for note in footnotes {
                    println!("{note}");
                }
            }
            if !json && !unmanaged.is_empty() {
                println!(
                    "\nunmanaged Proton builds (not pinned, not installed by gogo, never \
                     pruned; use one with `path:<dir>`):"
                );
                let mut t = output::table();
                t.set_header(vec!["PATH", "VERSION", "FOUND IN", "NOTE"]);
                for (b, default) in &unmanaged {
                    let mut note = vec!["unmanaged".to_string()];
                    if *default {
                        note.push("default ([linux] default_runtime)".into());
                    }
                    note.extend(b.problems());
                    t.add_row(vec![
                        b.path.display().to_string(),
                        b.version.clone().unwrap_or_else(|| "-".into()),
                        b.source.clone(),
                        note.join("; "),
                    ]);
                }
                println!("{t}");
            }
            Ok(cli::exit::OK)
        }
        crate::cli::StoreCmd::Install { id, version, json } => {
            let id = catalog::canonical_id(&id);
            let Some(entry) = catalog::find(id) else {
                eprintln!("gogo: unknown {noun} id '{id}' — see `gogo {command} list`");
                return Ok(cli::exit::USAGE);
            };
            if !shelf.holds(entry) {
                let other = Shelf::of(id);
                eprintln!(
                    "gogo: {id} is a {}: `gogo {} install {id}`",
                    other.noun(),
                    other.command()
                );
                return Ok(cli::exit::USAGE);
            }
            if let Err(why) = entry.check_os(os) {
                eprintln!("gogo: {why} — see `gogo {command} list`");
                return Ok(cli::exit::USAGE);
            }
            // D3DMetal (#71) installs here only with the stored consent.
            let consent =
                id == gogo::macapp::Renderer::D3dMetal.id() && gogo::macapp::d3dmetal::available();
            let http = catalog::http_client()?;
            // GE-Proton: the store or a Steam's tools dir, `[linux]
            // proton_dir`, asked once (design 18 §2.2).
            let shared = if matches!(entry.kind, catalog::Kind::Proton { .. }) {
                use gogo::linuxapp::{proton, umu};
                let config = gogo::config::Config::path();
                let tty = std::io::IsTerminal::is_terminal(&std::io::stdin())
                    && std::io::IsTerminal::is_terminal(&std::io::stderr())
                    && !json;
                let host = umu::Host::real();
                let asks = tty
                    && proton::stored_at(&config).is_none()
                    && !proton::steam_roots(&host).is_empty();
                let size = if asks {
                    catalog::probe(entry, version.as_deref(), &http, catalog::GITHUB_API)
                        .await
                        .ok()
                        .map(|r| r.size)
                } else {
                    None
                };
                let p = tty.then(prompt::renderer);
                proton::placement(entry, size, &config, p.as_deref(), &host, &mut |ev| {
                    emit_macapp_event(ev, false)
                })?
            } else {
                None
            };
            let act = depot_progress::Activity::new(depot_mode(false, json));
            let m = store::install_to(
                entry,
                version.as_deref(),
                &http,
                catalog::GITHUB_API,
                consent,
                shared.as_deref(),
                &mut |ev| emit_live(ev, false, &act),
            )
            .await;
            drop(act);
            let m = m?;
            let root = store::tree_root(&m);
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "id": m.id,
                        "version": m.version,
                        "sha256": m.sha256,
                        "bytes": m.bytes,
                        "path": root,
                    })
                );
            } else {
                println!(
                    "cached {} {} ({}) at {}",
                    m.id,
                    m.version,
                    gogo::fmt::human_size(m.bytes),
                    root.display()
                );
            }
            Ok(cli::exit::OK)
        }
        crate::cli::StoreCmd::Prune { unused, id } => {
            let mut swept: Vec<(String, String)> = Vec::new();
            let targets: Vec<String> = match id {
                // Per version: rollback pins only for ids a game uses (#69).
                None if unused => {
                    let plan = store::sweep();
                    swept = plan
                        .kept
                        .into_iter()
                        .filter(|(t, _)| Shelf::of(t) == shelf)
                        .collect();
                    plan.targets
                        .into_iter()
                        .filter(|t| Shelf::of(t) == shelf)
                        .collect()
                }
                // clap requires one of the two.
                None => return Ok(cli::exit::USAGE),
                Some(t) => {
                    let (tid, ver) = t
                        .split_once('/')
                        .map_or((t.as_str(), None), |(i, v)| (i, Some(v)));
                    let tid = catalog::canonical_id(tid);
                    if Shelf::of(tid) != shelf {
                        let other = Shelf::of(tid);
                        eprintln!(
                            "gogo: {tid} is a {}: `gogo {} prune {t}`",
                            other.noun(),
                            other.command()
                        );
                        return Ok(cli::exit::USAGE);
                    }
                    vec![ver.map_or(tid.to_string(), |v| format!("{tid}/{v}"))]
                }
            };
            // Design 16 decision 6: never prune a runtime (base or derived)
            // or a renderer a game still uses (`state.toml` or L4).
            let out = store::prune(&targets)?;
            for (id, users) in &out.kept {
                let mut games: Vec<&str> = users.iter().map(|u| u.gogid.as_str()).collect();
                games.dedup();
                eprintln!("gogo: keeping {id}: used by game(s) {}", games.join(", "));
            }
            for (target, why) in &swept {
                eprintln!("gogo: keeping {target}: {why}");
            }
            // Design 18 §2.2 (R26): listed, never deleted.
            for b in &out.shared {
                println!(
                    "left {} at {} ({}): gogo never deletes from a shared tools dir \
                     (Steam, Lutris or Heroic may use it by name); delete it yourself \
                     when nothing does",
                    b.target,
                    b.path.display(),
                    gogo::fmt::human_size(b.bytes)
                );
            }
            if out.removed.is_empty() && out.shared.is_empty() {
                println!("nothing to prune");
            }
            for path in &out.removed {
                println!("pruned {}", path.display());
            }
            Ok(cli::exit::OK)
        }
    }
}

// — apps: the games set up for .app bundles —

fn apps_cmd(cmd: crate::cli::AppsCmd) -> Result<i32, AppError> {
    use gogo::macapp::apps;

    let logs = gogo::macapp::doctor::Host::detect().logs;
    let dash = |s: Option<&str>| s.unwrap_or("-").to_string();
    match cmd {
        crate::cli::AppsCmd::List { json, sort } => {
            let keys = match sort.as_deref().map(apps_sort::parse) {
                None => apps_sort::default_keys(),
                Some(Ok(k)) => k,
                Some(Err(e)) => {
                    eprintln!("gogo: {e}");
                    return Ok(cli::exit::USAGE);
                }
            };
            let mut all = phase_sync(json, "sizing the games", || apps::list(&logs))?;
            apps_sort::sort(&mut all, &keys);
            if json {
                println!("{}", serde_json::json!({ "apps": all }));
                return Ok(cli::exit::OK);
            }
            if all.is_empty() {
                println!(
                    "no games set up yet: `gogo {} <GAME>` builds one",
                    app_command()
                );
                return Ok(cli::exit::OK);
            }
            let mut table = output::table();
            table.set_header(vec![
                "GOGID",
                "TITLE",
                if cfg!(target_os = "macos") {
                    ".APP"
                } else {
                    "MENU ENTRY"
                },
                "RUNTIME",
                "RENDERER",
                "DLC",
                "BUILD",
                "SIZE",
                "LAST LAUNCH",
            ]);
            for a in &all {
                table.add_row(vec![
                    a.gogid.clone(),
                    dash(a.title.as_deref()),
                    app_column(a),
                    dash(a.runtime.as_deref()),
                    dash(a.renderer.as_deref()),
                    if a.dlcs.is_empty() {
                        "-".to_string()
                    } else {
                        a.dlcs.len().to_string()
                    },
                    a.build.as_ref().map_or("-".to_string(), |b| b.column()),
                    gogo::fmt::human_size(a.bytes),
                    dash(a.last_launch.as_deref()),
                ]);
            }
            println!("{table}");
            for a in all.iter().filter(|a| a.unreachable) {
                println!(
                    "{}: its dir {} isn't there (a drive not mounted?)",
                    a.gogid,
                    a.root.display()
                );
            }
            let orphans = all.iter().filter(|a| a.orphaned()).count();
            if orphans > 0 {
                println!(
                    "{orphans} game(s) with a missing {}: `gogo apps prune --orphaned` \
                     removes them (it asks first)",
                    if cfg!(target_os = "macos") {
                        ".app"
                    } else {
                        "menu entry"
                    }
                );
            }
            Ok(cli::exit::OK)
        }
        crate::cli::AppsCmd::Show { target, json } => {
            let Some(a) = phase_sync(json, "sizing the game", || apps::get(&target, &logs))? else {
                eprintln!("gogo: {target} isn't set up here — see `gogo apps list`");
                return Ok(cli::exit::USAGE);
            };
            if json {
                println!("{}", serde_json::json!(a));
                return Ok(cli::exit::OK);
            }
            let path = |p: Option<&std::path::Path>| {
                p.map_or("-".to_string(), |p| p.display().to_string())
            };
            println!("{} {}", a.gogid, dash(a.title.as_deref()));
            if a.linux {
                println!("  menu entry:  {}", app_column(&a));
                if let Some(d) = &a.desktop {
                    println!("               {}", d.display());
                }
            } else {
                println!("  .app:        {}", app_column(&a));
                if let Some(b) = &a.bundle {
                    println!("               {}", b.display());
                }
            }
            println!("  state:       {}", a.root.display());
            println!("  prefix:      {}", a.prefix.display());
            println!("  logs:        {}", path(a.logs.as_deref()));
            println!("  override:    {}", path(a.override_file.as_deref()));
            println!("  runtime:     {}", dash(a.runtime.as_deref()));
            println!("  renderer:    {}", dash(a.renderer.as_deref()));
            println!("  size:        {}", gogo::fmt::human_size(a.bytes));
            println!("  last launch: {}", dash(a.last_launch.as_deref()));
            print_content(&a);
            if let Some(s) = &a.snapshots {
                for (i, l) in s.lines(&a.gogid).iter().enumerate() {
                    println!("  {:<12} {l}", if i == 0 { "snapshots:" } else { "" });
                }
            }
            Ok(cli::exit::OK)
        }
        crate::cli::AppsCmd::Remove {
            target,
            keep_override,
            yes,
        } => {
            finish_interrupted_moves()?;
            let Some(a) = phase_sync(false, "sizing the game", || apps::get(&target, &logs))?
            else {
                eprintln!("gogo: {target} isn't set up here — see `gogo apps list`");
                return Ok(cli::exit::USAGE);
            };
            remove_apps(&[a], keep_override, yes)
        }
        crate::cli::AppsCmd::Prune {
            orphaned: _,
            keep_override,
            yes,
        } => {
            finish_interrupted_moves()?;
            let orphans: Vec<apps::App> =
                phase_sync(false, "sizing the games", || apps::list(&logs))?
                    .into_iter()
                    .filter(apps::App::orphaned)
                    .collect();
            if orphans.is_empty() {
                println!("nothing to prune: no game lost its .app or menu entry");
                return Ok(cli::exit::OK);
            }
            remove_apps(&orphans, keep_override, yes)
        }
    }
}

/// `apps show`'s content section (design 19 §6.2).
fn print_content(a: &gogo::macapp::apps::App) {
    if let Some(e) = &a.content_error {
        println!("  content:     unreadable: {e}");
        return;
    }
    let Some(c) = &a.content else { return };
    let source = if a.content_derived {
        " (derived from the game's files; no content.toml)"
    } else {
        ""
    };
    println!(
        "  content:     {}{}{source}",
        c.lane.as_str(),
        c.build_id
            .as_deref()
            .map_or(String::new(), |b| format!(", build {b}"))
    );
    if let Some(v) = &c.version {
        println!("  version:     {v}");
    }
    if let Some(l) = &c.language {
        println!("  language:    {l}");
    }
    if let Some(b) = &a.build {
        for (i, l) in b.show_lines(&a.gogid).iter().enumerate() {
            println!("  {:<12} {l}", if i == 0 { "build:" } else { "" });
        }
    }
    if c.dlc.is_empty() {
        println!("  dlc:         none");
    }
    for (i, d) in c.dlc.iter().enumerate() {
        println!(
            "  {:<12} {} ({}){}",
            if i == 0 { "dlc:" } else { "" },
            d.title,
            d.id,
            d.build_id
                .as_deref()
                .map_or(String::new(), |b| format!(", build {b}"))
        );
    }
}

/// The command that builds a game on this OS.
fn app_command() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac-app"
    } else {
        "linux-app"
    }
}

/// `present`, `missing`, `portable` (and `-` when nothing is recorded). A
/// Linux game: its menu entry, `not yet` before #177's launcher made one.
fn app_column(a: &gogo::macapp::apps::App) -> String {
    if a.linux {
        return match (a.desktop_present, a.launcher) {
            (true, _) => "present".to_string(),
            (false, true) => "missing".to_string(),
            (false, false) => "not yet".to_string(),
        };
    }
    let kind = if a.portable { "portable, " } else { "" };
    match (&a.bundle, a.bundle_present) {
        (None, _) => "-".to_string(),
        (Some(_), true) => format!("{kind}present"),
        (Some(_), false) => format!("{kind}missing"),
    }
}

/// An interrupted `linux-app move` first: otherwise a removal could miss the
/// copy it left, or remove the game from the wrong side of it.
fn finish_interrupted_moves() -> Result<(), AppError> {
    #[cfg(target_os = "linux")]
    for line in gogo::linuxapp::relocate::recover_all() {
        println!("{line}");
    }
    Ok(())
}

/// Print what goes, ask (unless `yes`), remove, and report.
fn remove_apps(
    targets: &[gogo::macapp::apps::App],
    keep_override: bool,
    yes: bool,
) -> Result<i32, AppError> {
    let total: u64 = targets.iter().map(|a| a.bytes).sum();
    println!("to remove (Wine prefix and saves included):");
    for a in targets {
        let title = a.title.as_deref().unwrap_or("-");
        println!(
            "  {} {title} ({}) at {}",
            a.gogid,
            gogo::fmt::human_size(a.bytes),
            a.root.display()
        );
        if let Some(b) = a.bundle.as_ref().filter(|_| a.bundle_present) {
            println!("    {} stays, but won't start until rebuilt", b.display());
        }
        if let Some(d) = a.desktop.as_ref().filter(|_| a.desktop_present) {
            println!("    its menu entry {} goes too", d.display());
        }
        if let Some(n) = a.snapshots.as_ref().and_then(|s| s.remove_note()) {
            println!("    and {n}, deleted first");
        }
        if let Some(s) = a.snapshots.as_ref().filter(|s| !s.saves_aside.is_empty()) {
            let bytes: u64 = s.saves_aside_bytes.iter().sum();
            println!(
                "    its saves set aside by rollbacks ({} dir(s), {}) stay unless you say so",
                s.saves_aside.len(),
                gogo::fmt::human_size(bytes)
            );
        }
    }
    if !yes {
        if !console::Term::stdout().features().is_attended() {
            eprintln!("gogo: not attached to a terminal — pass --yes to remove the above");
            return Ok(cli::exit::USAGE);
        }
        let confirmed = crate::depot_progress::prompting(|| {
            dialoguer::Confirm::new()
                .with_prompt(format!(
                    "remove {} game(s), {}?",
                    targets.len(),
                    gogo::fmt::human_size(total)
                ))
                .default(false)
                .interact()
        })
        .unwrap_or(false);
        if !confirmed {
            println!("aborted");
            return Ok(cli::exit::OK);
        }
    }
    // The saves a rollback set aside are the user's (design 20 §6.2, §9.3):
    // asked about apart, default no, never with `--yes` alone.
    #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
    let mut drop_aside: Vec<&str> = Vec::new();
    #[cfg(target_os = "linux")]
    if !yes && console::Term::stdout().features().is_attended() {
        for a in targets {
            let Some(s) = a.snapshots.as_ref().filter(|s| !s.saves_aside.is_empty()) else {
                continue;
            };
            for (d, b) in s.saves_aside.iter().zip(&s.saves_aside_bytes) {
                println!("  {} ({})", d.display(), gogo::fmt::human_size(*b));
            }
            let also = crate::depot_progress::prompting(|| {
                dialoguer::Confirm::new()
                    .with_prompt(format!(
                        "also remove {}'s saves set aside by rollbacks (listed above)?",
                        a.gogid
                    ))
                    .default(false)
                    .interact()
            })
            .unwrap_or(false);
            if also {
                drop_aside.push(&a.gogid);
            }
        }
    }
    let mut failed = false;
    let act = depot_progress::Activity::new(depot_mode(false, false));
    for a in targets {
        let removed = act.spin(
            &format!(
                "removing {} ({})",
                a.title.as_deref().unwrap_or(&a.gogid),
                gogo::fmt::human_size(a.bytes)
            ),
            || gogo::macapp::apps::remove(a, keep_override),
        );
        match removed {
            Ok(out) => {
                for p in &out.paths {
                    println!("removed {}", p.display());
                }
                #[cfg(target_os = "linux")]
                if drop_aside.contains(&a.gogid.as_str()) {
                    match gogo::linuxapp::rollback::remove_saves_aside(&a.gogid) {
                        Ok(Some(d)) => println!("removed {}", d.display()),
                        Ok(None) => {}
                        Err(e) => {
                            eprintln!("gogo: {e}");
                            failed = true;
                        }
                    }
                }
                if let Some(s) = a.snapshots.as_ref().filter(|s| {
                    !s.saves_aside.is_empty() && !drop_aside.contains(&a.gogid.as_str())
                }) && let Some(d) = s.saves_aside.first().and_then(|d| d.parent())
                {
                    println!("{}: its saves set aside stay in {}", a.gogid, d.display());
                }
                if out.snapshots > 0 {
                    println!("{}: deleted {} snapshot(s)", a.gogid, out.snapshots);
                }
                if let Some(p) = &out.snapshots_left {
                    println!(
                        "{}: its snapshots at {} weren't touched (the drive isn't there)",
                        a.gogid,
                        p.display()
                    );
                }
                if let Some(p) = &out.left_behind {
                    println!(
                        "{}: its files at {} weren't touched; delete them yourself if you \
                         don't want them",
                        a.gogid,
                        p.display()
                    );
                }
            }
            Err(e) => {
                eprintln!("gogo: {e}");
                failed = true;
            }
        }
    }
    println!("`gogo runtimes prune --unused` frees the runtimes no game uses now");
    Ok(if failed {
        cli::exit::MACAPP
    } else {
        cli::exit::OK
    })
}

// — recipes (design 11 §1: corpus inspection, nothing applied or stored) —

fn recipes_cmd(args: crate::cli::RecipesArgs) -> Result<i32, AppError> {
    use gogo::macapp::{self, catalog, interpret, lutris};

    match args.cmd {
        crate::cli::RecipesCmd::List {
            installer,
            name,
            json,
            lutris_base,
        } => {
            let display = match (name, installer) {
                (Some(n), _) => n,
                (None, Some(p)) => macapp::display_name(std::path::Path::new(&p), None),
                (None, None) => {
                    eprintln!("error: recipes list needs <INSTALLER> or --name <game>");
                    return Ok(cli::exit::USAGE);
                }
            };
            let candidates = macapp::recipe_candidates(lutris_base.as_deref(), &display)?;
            if json {
                let rows: Vec<serde_json::Value> = candidates.iter().map(installer_json).collect();
                println!("{}", serde_json::to_string(&rows).unwrap());
                return Ok(cli::exit::OK);
            }
            if candidates.is_empty() {
                eprintln!("no Lutris recipes found for {display:?}");
                return Ok(cli::exit::OK);
            }
            println!("Lutris recipes for {display} (ranked):");
            let mut table = output::table();
            table.set_header(vec![
                "#", "SLUG", "VERSION", "AUTHOR", "UPDATED", "WINE PIN",
            ]);
            for (i, inst) in candidates.iter().enumerate() {
                table.add_row(vec![
                    (i + 1).to_string(),
                    inst.slug.clone(),
                    inst.version.clone(),
                    inst.author.clone(),
                    inst.updated_at.clone(),
                    lutris::wine_version(&inst.script).unwrap_or_else(|| "-".into()),
                ]);
            }
            println!("{table}");
            println!("inspect one: gogo recipes show <slug>");
            Ok(cli::exit::OK)
        }
        crate::cli::RecipesCmd::Show {
            recipe,
            json,
            lutris_base,
        } => {
            let path = std::path::Path::new(&recipe);
            let inst = if path.is_file() {
                macapp::parse_script_file(path)?
            } else {
                macapp::recipe_by_slug(lutris_base.as_deref(), &recipe)?
            };
            let pin = lutris::wine_version(&inst.script);
            let rec = catalog::recommend_for_wine(pin.as_deref());
            let ctx = macapp::inspection_ctx(&inst.script);
            let plan = interpret::plan(&inst.script, &ctx);

            if json {
                let steps: Vec<serde_json::Value> = plan
                    .steps
                    .iter()
                    .map(|(i, s)| {
                        serde_json::json!({
                            "index": i,
                            "auto": *i == usize::MAX,
                            "applied": true,
                            "verb": step_verb(s),
                            "detail": step_detail(s),
                        })
                    })
                    .collect();
                let skipped: Vec<serde_json::Value> = plan
                    .skipped
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "index": s.index,
                            "applied": false,
                            "verb": s.verb,
                            "reason": s.reason,
                        })
                    })
                    .collect();
                let out = serde_json::json!({
                    "installer": installer_json(&inst),
                    "wine_pin": pin,
                    "runtime_recommendation": {
                        "id": rec.id,
                        "notice": rec.notice,
                    },
                    "installer_args": plan.installer_args,
                    "steps": steps,
                    "skipped": skipped,
                    "launch_exe": plan.launch_exe,
                    "launch_args": plan.launch_args,
                });
                println!("{}", serde_json::to_string_pretty(&out).unwrap());
                return Ok(cli::exit::OK);
            }

            println!(
                "recipe {} — {} v{} (by {}, {})",
                inst.slug, inst.name, inst.version, inst.author, inst.updated_at
            );
            match &pin {
                Some(p) => println!(
                    "wine pin: {p} → {id}{notice}",
                    id = rec.id,
                    notice = rec
                        .notice
                        .as_ref()
                        .map(|n| format!(" ({n})"))
                        .unwrap_or_default()
                ),
                None => println!(
                    "wine pin: none → default {}",
                    gogo::macapp::catalog::DEFAULT_RUNTIME
                ),
            }
            if !plan.installer_args.is_empty() {
                println!("harvested setup flags: {}", plan.installer_args.join(" "));
            }
            if !plan.steps.is_empty() {
                println!("\nplanned steps (applied):");
                let mut table = output::table();
                table.set_header(vec!["#", "STEP", "DETAIL"]);
                for (i, s) in &plan.steps {
                    // usize::MAX marks auto-appended steps (wine.overrides).
                    let idx = if *i == usize::MAX {
                        "-".to_string()
                    } else {
                        i.to_string()
                    };
                    table.add_row(vec![idx, step_verb(s), step_detail(s)]);
                }
                println!("{table}");
            }
            if !plan.skipped.is_empty() {
                println!("\nskipped (with reasons):");
                let mut table = output::table();
                table.set_header(vec!["#", "STEP", "REASON"]);
                for s in &plan.skipped {
                    table.add_row(vec![s.index.to_string(), s.verb.clone(), s.reason.clone()]);
                }
                println!("{table}");
            }
            if let Some(exe) = &plan.launch_exe {
                println!("\nlaunch: {exe} {}", plan.launch_args.join(" "));
            }
            if plan.is_empty() {
                println!("\n(no installer steps in this script)");
            }
            Ok(cli::exit::OK)
        }
    }
}

fn installer_json(inst: &gogo::macapp::lutris::LutrisInstaller) -> serde_json::Value {
    use gogo::macapp::lutris;
    serde_json::json!({
        "slug": inst.slug,
        "game_slug": inst.game_slug,
        "name": inst.name,
        "version": inst.version,
        "runner": inst.runner,
        "author": inst.author,
        "updated_at": inst.updated_at,
        "gogid": inst.gogid,
        "wine_pin": lutris::wine_version(&inst.script),
    })
}

fn step_verb(step: &gogo::macapp::interpret::Step) -> String {
    use gogo::macapp::interpret::Step;
    match step {
        Step::HarvestInstallerArgs { .. } => "harvest-setup-args".into(),
        Step::Wineexec { .. } => "wineexec".into(),
        Step::Winetricks { .. } => "winetricks".into(),
        Step::Download { .. } => "download".into(),
        Step::Extract { .. } => "extract".into(),
        Step::Merge { .. } => "merge".into(),
        Step::Move { .. } => "move".into(),
        Step::WriteFile { .. } => "write-file".into(),
        Step::WriteConfig { .. } => "write-config".into(),
        Step::Chmodx { .. } => "chmodx".into(),
        Step::Mkdir { .. } => "mkdir".into(),
        Step::Registry { .. } => "registry".into(),
        Step::Launch { .. } => "launch".into(),
    }
}

fn step_detail(step: &gogo::macapp::interpret::Step) -> String {
    use gogo::macapp::interpret::Step;
    match step {
        Step::HarvestInstallerArgs { args } => {
            format!("silent flags: {args}")
        }
        Step::Wineexec { exe, args } => format!(
            "run {} {}",
            exe.display(),
            if args.is_empty() {
                String::new()
            } else {
                format!("\"{args}\"")
            }
        ),
        Step::Winetricks { verbs } => verbs.join(" "),
        Step::Download { id, url } => format!("{id} ← {url}"),
        Step::Extract { file, dst } => format!("{} → {}", file.display(), dst.display()),
        Step::Merge { src, dst } => format!("{} → {}", src.display(), dst.display()),
        Step::Move { src, dst } => format!("{} → {}", src.display(), dst.display()),
        Step::WriteFile { file, .. } => file.display().to_string(),
        Step::WriteConfig {
            file,
            section,
            values,
        } => format!(
            "{} [{}] {}",
            file.display(),
            section,
            values
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Step::Chmodx { file } => file.display().to_string(),
        Step::Mkdir { dirs } => dirs
            .iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join(", "),
        Step::Registry { key, values } => format!(
            "{key}: {}",
            values
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Step::Launch { exe, args, .. } => format!(
            "{} {}",
            exe.as_deref().unwrap_or("(default entry point)"),
            args.join(" ")
        ),
    }
}

/// mac-app events → NDJSON (`--json-progress`) or compact stderr one-liners.
fn emit_macapp_event(ev: &gogo::engine::events::ProgressEvent, json_progress: bool) {
    emit_macapp_to(ev, json_progress, &|l| eprintln!("{l}"));
}

/// [`emit_macapp_event`] with a live status line (#420): the running counts go
/// on it, the lines above it.
fn emit_live(
    ev: &gogo::engine::events::ProgressEvent,
    json_progress: bool,
    act: &depot_progress::Activity,
) {
    if !act.work(ev) {
        emit_macapp_to(ev, json_progress, &|l| act.say(l, &|l| eprintln!("{l}")));
    }
}

/// [`emit_macapp_event`]'s lines go to `print`.
fn emit_macapp_to(
    ev: &gogo::engine::events::ProgressEvent,
    json_progress: bool,
    print: &dyn Fn(&str),
) {
    if ev.is_live_only() {
        return;
    }
    if json_progress {
        if let Ok(line) = serde_json::to_string(ev) {
            println!("{line}");
        }
        return;
    }
    use gogo::engine::events::ProgressEvent as Ev;
    let line = match ev {
        Ev::RuntimeResolved { kind, path } => Some(format!("runtime: {kind} ({path})")),
        Ev::PrefixStaged => Some("prefix staged".into()),
        Ev::InstallStarted => Some("installing silently…".into()),
        Ev::InstallFinished { bytes } => {
            Some(format!("installer finished ({bytes} B of installers)"))
        }
        Ev::BundleAssembled => Some("bundle assembled".into()),
        Ev::Codesigned => Some("codesigned (ad-hoc)".into()),
        Ev::Warn { message } => Some(format!("note: {message}")),
        Ev::RuntimeDownloadStarted { id, version, bytes } => Some(format!(
            "downloading {id} {version} ({})",
            gogo::fmt::human_size(*bytes)
        )),
        Ev::RuntimeDownloadFinished { id, version } => Some(format!("{id} {version} cached")),
        Ev::ToolProvisioned { id, version } => Some(format!("{id} {version} provisioned")),
        Ev::RendererSelected {
            renderer,
            layer,
            reason,
        } => Some(format!("renderer: {renderer} ({layer}: {reason})")),
        // Reported like `--retarget`'s own L4 writes (#110).
        Ev::UserOverrideSet {
            gogid,
            key,
            old,
            new,
            path,
        } => Some(format!(
            "{gogid}: {key} {} -> {} ({})",
            old.as_deref().unwrap_or("(bundle default)"),
            new.as_deref().unwrap_or("(bundle default)"),
            path.display()
        )),
        Ev::RecipeStep { verb } => Some(format!("applied {verb}")),
        Ev::GameFilesStarted {
            method, bytes, dir, ..
        } => Some(match method.as_str() {
            "reflink" => format!("game files: reflinking into {}", dir.display()),
            _ => format!(
                "game files: copying {} into {}",
                gogo::fmt::human_size(*bytes),
                dir.display()
            ),
        }),
        Ev::GameFilesProgress { copied, total } => Some(format!(
            "game files: copied {} / {}",
            gogo::fmt::human_size(*copied),
            gogo::fmt::human_size(*total)
        )),
        Ev::SnapshotCreated { id, label, .. } => Some(format!("snapshot {id} taken ({label})")),
        Ev::SnapshotPruned { id, label, created } => {
            Some(gogo::linuxapp::snapshots::pruned_line(*id, label, created))
        }
        _ => None,
    };
    if let Some(l) = line {
        print(&format!("gogo: {l}"));
    }
}

#[cfg(target_os = "linux")]
/// The `auto` snapshot options of a command that changes a game (design 20
/// §7.1): `--no-snapshots` and `[snapshots] keep_auto`.
pub(crate) fn auto_snapshots(
    no_snapshots: bool,
) -> Result<gogo::linuxapp::snapshots::AutoOptions, AppError> {
    Ok(gogo::linuxapp::snapshots::AutoOptions {
        skip: no_snapshots,
        keep_auto: load_config_quietly()?.config.snapshots.keep_auto,
        ..Default::default()
    })
}

/// Smoke tests for the completion generation (08 §2: thin frontend gets smoke tests):
/// every shell yields a script naming the binary, with the shell's load-bearing
/// marker present.
#[cfg(test)]
mod store_tests {
    use super::listed;
    use gogo::macapp::catalog::{self, Os};

    /// Design 18 §6: each OS lists its own entries; `--all` lists them all.
    #[test]
    fn list_shows_this_os_and_all_shows_the_rest() {
        let ids = |os, all| -> Vec<&str> {
            catalog::entries()
                .iter()
                .filter(|e| listed(e, os, all))
                .map(|e| e.id)
                .collect()
        };
        assert_eq!(ids(Os::Linux, false), ["ge-proton", "umu", "cnc-ddraw"]);
        assert!(ids(Os::Macos, false).contains(&"winecx26"));
        assert!(!ids(Os::Macos, false).contains(&"umu"));
        assert!(!ids(Os::Macos, false).contains(&"ge-proton"));
        assert!(ids(Os::Macos, false).contains(&"cnc-ddraw"));
        assert_eq!(ids(Os::Linux, true).len(), catalog::entries().len());
    }
}

#[cfg(test)]
mod completions_tests {
    use super::write_completions;
    use clap_complete::Shell;

    fn script(shell: Shell) -> String {
        let mut buf = Vec::new();
        let exit = match write_completions(shell, &mut buf) {
            Ok(code) => code,
            Err(_) => panic!("completion generation cannot fail"),
        };
        assert_eq!(exit, crate::cli::exit::OK);
        String::from_utf8(buf).expect("completion script is UTF-8")
    }

    #[test]
    fn every_shell_generates_a_script_naming_gogo() {
        for shell in [
            Shell::Bash,
            Shell::Zsh,
            Shell::Fish,
            Shell::Elvish,
            Shell::PowerShell,
        ] {
            let out = script(shell);
            assert!(!out.trim().is_empty(), "{shell:?}: empty script");
            assert!(out.contains("gogo"), "{shell:?}: binary name missing");
        }
    }

    #[test]
    fn bash_script_registers_the_completions() {
        let out = script(Shell::Bash);
        assert!(out.contains("_gogo()"), "missing completion function");
        assert!(out.contains("complete "), "missing complete registration");
    }

    #[test]
    fn zsh_script_starts_with_compdef() {
        assert!(script(Shell::Zsh).starts_with("#compdef gogo"));
    }

    #[test]
    fn fish_script_completes_the_gogo_command() {
        assert!(script(Shell::Fish).contains("complete -c gogo"));
    }
}

#[cfg(test)]
mod tests {
    use crate::depot_progress::game_files_line;
    use gogo::gog::materialize::{MaterializePlan, TreeMethod};

    /// `update` and `verify --fix` give `engine::execute` a subscriber, so the
    /// download's events reach the renderer; without one (off a terminal, or a
    /// machine mode) nothing subscribes.
    #[tokio::test]
    async fn a_live_run_has_its_events_rendered() {
        use gogo::engine::events::ProgressEvent;
        let (tx, _keep) = tokio::sync::broadcast::channel::<ProgressEvent>(16);
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let out = crate::with_progress(
            &tx,
            true,
            async {
                let _ = tx.send(ProgressEvent::Warn {
                    message: "downloading".into(),
                });
                // The renderer drains what's queued before `done` wins.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                7
            },
            |rx, done| {
                gogo::engine::events::render(rx, done, move |ev| {
                    sink.lock().unwrap().push(format!("{ev:?}"));
                })
            },
        )
        .await;
        assert_eq!(out, 7);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert!(seen[0].contains("downloading"), "{seen:?}");
    }

    #[tokio::test]
    async fn a_run_off_a_terminal_subscribes_nothing() {
        use gogo::engine::events::ProgressEvent;
        let (tx, _keep) = tokio::sync::broadcast::channel::<ProgressEvent>(16);
        let before = tx.receiver_count();
        let out = crate::with_progress(
            &tx,
            false,
            async { tx.receiver_count() },
            |_rx, _done| async { panic!("no renderer off a terminal") },
        )
        .await;
        assert_eq!(out, before);
    }

    #[test]
    fn game_files_line_says_reflink_or_copy_and_its_cost() {
        let mut plan = MaterializePlan {
            method: TreeMethod::Reflink,
            game_dir: "/d/games/1/game".into(),
            extra_bytes: 0,
        };
        assert_eq!(
            game_files_line(&plan),
            "game files: reflink into /d/games/1/game (no extra disk)"
        );
        plan.method = TreeMethod::Copy;
        plan.extra_bytes = 3 * 1024 * 1024 * 1024;
        let line = game_files_line(&plan);
        assert!(
            line.starts_with("game files: copy into /d/games/1/game (+"),
            "{line}"
        );
        assert!(
            line.contains(&gogo::fmt::human_size(plan.extra_bytes)),
            "{line}"
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod envargs_output_tests {
    use super::{
        args_text, display_text, env_catalogue_text, env_describe_text, env_list_text, wrapper_text,
    };
    use gogo::linuxapp::catalogue::table::{Component, Kind, Topic};
    use gogo::linuxapp::catalogue::{
        Applies, BuildSummary, CatalogueReport, Entry, InBuild, Source,
    };
    use gogo::linuxapp::envargs::{ArgsReport, EnvLine, EnvReport, EnvSource, EnvState};
    use gogo::macapp::compat::{Layer, Sourced};

    fn entry(name: &str, component: Component, applies: Applies, in_build: InBuild) -> Entry {
        Entry {
            name: name.into(),
            component,
            kind: Kind::Env,
            topic: Some(Topic::Nvidia),
            about: Some(format!("About {name}.")),
            default: Some("off".into()),
            default_from: Some("table"),
            link: component.link(),
            sources: vec![Source::Table],
            in_build,
            proton_sets: false,
            applies,
            active: vec![],
        }
    }

    fn cyberpunk() -> CatalogueReport {
        let mut dlss = entry(
            "PROTON_DLSS_UPGRADE",
            Component::Proton,
            Applies::Yes,
            InBuild::Found,
        );
        dlss.sources.insert(
            0,
            Source::ProtonScript {
                token: Some("dlss".into()),
            },
        );
        dlss.active = vec![EnvLine {
            key: "PROTON_DLSS_UPGRADE".into(),
            value: "1".into(),
            source: EnvSource::Layer { layer: Layer::User },
            state: EnvState::Pending,
        }];
        let mut uncharted = entry(
            "PROTON_NEW_THING",
            Component::Proton,
            Applies::Unknown {
                why: "the build has it; gogo's table doesn't".into(),
            },
            InBuild::Found,
        );
        uncharted.about = None;
        uncharted.default = None;
        uncharted.default_from = None;
        uncharted.topic = None;
        uncharted.sources = vec![Source::ProtonScript { token: None }];
        let mut gamedir = entry(
            "GAMEID",
            Component::Plumbing,
            Applies::Unknown {
                why: "not a switch: Steam's, umu's or the build's own".into(),
            },
            InBuild::NotFound,
        );
        gamedir.about = None;
        gamedir.default = None;
        gamedir.default_from = None;
        gamedir.sources = vec![];
        gamedir.active = vec![EnvLine {
            key: "GAMEID".into(),
            value: "umu-1091500".into(),
            source: EnvSource::Launcher,
            state: EnvState::Applies,
        }];
        let mut plumbing = gamedir.clone();
        plumbing.name = "STEAM_COMPAT_DATA_PATH".into();
        plumbing.active = vec![];
        CatalogueReport {
            gogid: "1423049311".into(),
            user_file: "/d/compat/1423049311.toml".into(),
            built: true,
            build: Some(BuildSummary {
                dir: "/d/runtimes/ge-proton/x".into(),
                version: Some("GE-Proton11-12".into()),
                components: [("dxvk-nvapi".to_string(), "v0.9.3".to_string())]
                    .into_iter()
                    .collect(),
            }),
            reviewed_for: "GE-Proton11-12",
            renderer: Some("vkd3d-proton".into()),
            gpus: vec!["NVIDIA".into()],
            entries: vec![
                dlss,
                entry(
                    "PROTON_ENABLE_NVAPI",
                    Component::Proton,
                    Applies::No {
                        why: "not in this build".into(),
                    },
                    InBuild::NotFound,
                ),
                uncharted,
                entry(
                    "DXVK_NVAPI_VKREFLEX",
                    Component::DxvkNvapi,
                    Applies::Yes,
                    InBuild::Found,
                ),
                gamedir,
                plumbing,
            ],
            warnings: vec![],
        }
    }

    #[test]
    fn env_list_all_marks_every_switch_on_one_line() {
        assert_eq!(
            env_catalogue_text(&cyberpunk()),
            "1423049311 on GE-Proton11-12: renderer vkd3d-proton, GPU NVIDIA\n\
             bundled: dxvk-nvapi v0.9.3\n\
             yes: does something here; no: doesn't, and why; ?: gogo can't tell.\n\
             [ ]: where gogo knows it from (script, fixes, sample and dll are this build; table is gogo's).\n\
             \n\
             proton (https://github.com/GloriousEggroll/proton-ge-custom#readme)\n\
             yes PROTON_DLSS_UPGRADE  About PROTON_DLSS_UPGRADE. Default: off. [script, table]\n\
             \x20   now: PROTON_DLSS_UPGRADE=1  (L4 user override; pending: the next launch's setup applies it)\n\
             no  PROTON_ENABLE_NVAPI  About PROTON_ENABLE_NVAPI. Default: off. (not in this build) [table]\n\
             ?   PROTON_NEW_THING     (not in gogo's table yet) (the build has it; gogo's table doesn't) [script]\n\
             \n\
             dxvk-nvapi v0.9.3 (https://github.com/jp7677/dxvk-nvapi#readme)\n\
             yes DXVK_NVAPI_VKREFLEX  About DXVK_NVAPI_VKREFLEX. Default: off. [table]\n\
             \n\
             plumbing\n\
             ?   GAMEID  Not a switch: Steam's, umu's or the build's own.\n\
             \x20   now: GAMEID=umu-1091500  (launcher, always)\n\
             \n\
             not switches (Steam's, umu's and the build's own): STEAM_COMPAT_DATA_PATH\n\
             \n\
             1 of these the build has and gogo's table doesn't describe yet (reviewed for GE-Proton11-12)\n\
             \n\
             one in full: gogo linux-app env describe 1423049311 <NAME>; set one: gogo linux-app env set 1423049311 KEY=VALUE\n"
        );
    }

    #[test]
    fn env_describe_says_all_of_one_switch() {
        let r = cyberpunk();
        assert_eq!(
            env_describe_text(&r, r.find("PROTON_DLSS_UPGRADE").unwrap()),
            "PROTON_DLSS_UPGRADE (proton, env var)\n\
             \x20 About PROTON_DLSS_UPGRADE.\n\
             default:  off\n\
             applies:  yes (1423049311 on GE-Proton11-12: renderer vkd3d-proton, GPU NVIDIA)\n\
             build:    GE-Proton11-12 has it\n\
             from:     proton script (STEAM_COMPAT_CONFIG token dlss), gogo's table\n\
             now:      PROTON_DLSS_UPGRADE=1  (L4 user override; pending: the next launch's setup applies it)\n\
             set with: gogo linux-app env set 1423049311 PROTON_DLSS_UPGRADE=<value>\n\
             docs:     https://github.com/GloriousEggroll/proton-ge-custom#readme\n"
        );
        assert_eq!(
            env_describe_text(&r, r.find("PROTON_ENABLE_NVAPI").unwrap()),
            "PROTON_ENABLE_NVAPI (proton, env var)\n\
             \x20 About PROTON_ENABLE_NVAPI.\n\
             default:  off\n\
             applies:  no: not in this build (1423049311 on GE-Proton11-12: renderer vkd3d-proton, GPU NVIDIA)\n\
             build:    no trace of it in GE-Proton11-12\n\
             from:     gogo's table\n\
             now:      not set for this game\n\
             docs:     https://github.com/GloriousEggroll/proton-ge-custom#readme\n"
        );
    }

    #[test]
    fn env_list_shows_the_winner_its_source_and_what_it_beats() {
        let line = |k: &str, v: &str, source, state| EnvLine {
            key: k.into(),
            value: v.into(),
            source,
            state,
        };
        let r = EnvReport {
            gogid: "1423049311".into(),
            user_file: "/d/compat/1423049311.toml".into(),
            built: true,
            lines: vec![
                line(
                    "DXVK_HUD",
                    "fps",
                    EnvSource::Layer { layer: Layer::User },
                    EnvState::Pending,
                ),
                line(
                    "PROTON_SET_GAME_DRIVE",
                    "1",
                    EnvSource::Layer { layer: Layer::User },
                    EnvState::Applies,
                ),
                line(
                    "PROTON_SET_GAME_DRIVE",
                    "0",
                    EnvSource::GogoDefault,
                    EnvState::Overridden {
                        by: "L4 user override".into(),
                    },
                ),
                line("STORE", "gog", EnvSource::Launcher, EnvState::Applies),
            ],
            wrapper: None,
            display: None,
            warnings: vec![],
        };
        assert_eq!(
            env_list_text(&r),
            "env of 1423049311 at its next launch (each line beats the environment \
             launch.sh starts with, and the values listed under it):\n\
             DXVK_HUD=fps             (L4 user override; pending: the next launch's setup applies it)\n\
             PROTON_SET_GAME_DRIVE=1  (L4 user override)\n\
             \x20 over PROTON_SET_GAME_DRIVE=0  (gogo default)\n\
             STORE=gog                (launcher, always)\n\
             your override file: /d/compat/1423049311.toml (`gogo linux-app env set 1423049311 KEY=VALUE`)\n"
        );
    }

    #[test]
    fn the_wrapper_is_shown_with_what_applies_it() {
        use gogo::linuxapp::envargs::{WrapperReport, WrapperView};
        let view = |words: &[&str], applied: &[&str], pending| WrapperView {
            words: words.iter().map(|s| s.to_string()).collect(),
            layer: (!words.is_empty()).then_some(Layer::User),
            applied: applied.iter().map(|s| s.to_string()).collect(),
            pending,
        };
        let report = |wrapper| WrapperReport {
            gogid: "1423049311".into(),
            user_file: "/d/compat/1423049311.toml".into(),
            built: true,
            wrapper,
            warnings: vec![],
        };
        let tail = "it runs outside umu-run with the environment launch.sh starts with; the \
                    game's env applies inside it\n\
                    your override file: /d/compat/1423049311.toml\n";
        assert_eq!(
            wrapper_text(&report(None)),
            format!(
                "wrapper: (none)  (`gogo linux-app wrapper 1423049311 -- gamescope -f --` sets \
                 one)\n{tail}"
            )
        );
        assert_eq!(
            wrapper_text(&report(Some(view(
                &["gamescope", "-W", "a b", "--"],
                &["gamescope", "-W", "a b", "--"],
                false
            )))),
            format!("wrapper: gamescope -W 'a b' --  (L4 user override)\n{tail}")
        );
        assert_eq!(
            wrapper_text(&report(Some(view(&["gamemoderun"], &[], true)))),
            format!(
                "wrapper: gamemoderun  (L4 user override)\n\
                 pending: the next launch's setup applies it\n{tail}"
            )
        );
        assert_eq!(
            wrapper_text(&report(Some(view(&[], &["gamemoderun"], true)))),
            format!(
                "wrapper: (none)\n\
                 pending: launch.conf still has gamemoderun; the next launch's setup writes the \
                 new one\n{tail}"
            )
        );
    }

    #[test]
    fn the_display_mode_is_shown_with_what_the_launcher_sets() {
        use gogo::linuxapp::display::{DisplayReport, DisplayView, Session};
        use gogo::macapp::compat::{DisplayMode, Hdr, Sourced};
        let user = |value| Sourced {
            value,
            layer: Layer::User,
        };
        let report = |display, wayland_env: &[(&str, &str)], picks| DisplayReport {
            gogid: "1423049311".into(),
            user_file: "/d/compat/1423049311.toml".into(),
            built: true,
            display,
            wayland_env: wayland_env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            session: Session {
                wayland_display: None,
                socket: false,
            },
            picks,
            warnings: vec![],
        };
        let tail = "your override file: /d/compat/1423049311.toml\n";
        assert_eq!(
            display_text(&report(None, &[], None)),
            format!(
                "display: (none)  (the environment decides; `gogo linux-app display 1423049311 \
                 wayland` sets Wine's Wayland driver)\n{tail}"
            )
        );
        let view = DisplayView {
            mode: Some(user(DisplayMode::Wayland)),
            hdr: Some(Sourced {
                value: Hdr::Force,
                layer: Layer::User,
            }),
            monitor: Some(Sourced {
                value: "DP-1".into(),
                layer: Layer::User,
            }),
            applied_mode: Some(DisplayMode::X11),
            applied_hdr: None,
            applied_monitor: Some("HDMI-A-1".into()),
            pending: true,
        };
        assert_eq!(
            display_text(&report(
                Some(view),
                &[
                    ("PROTON_ENABLE_WAYLAND", "1"),
                    ("PROTON_FORCE_HDR", "1"),
                    ("PROTON_WAYLAND_MONITOR", "DP-1")
                ],
                Some(DisplayMode::X11)
            )),
            format!(
                "display: wayland (L4 user override), hdr force (L4 user override), \
                 monitor DP-1 (L4 user override)\n\
                 pending: launch.conf has x11, monitor HDMI-A-1; the next launch's setup writes the new one\n\
                 with a Wayland socket at launch the game gets PROTON_ENABLE_WAYLAND=1 \
                 PROTON_FORCE_HDR=1 PROTON_WAYLAND_MONITOR=DP-1; without one it runs on X11, and the launcher logs why\n\
                 a launch from this shell: x11 (WAYLAND_DISPLAY is unset; a desktop launch has \
                 the session's)\n{tail}"
            )
        );
        let view = DisplayView {
            mode: Some(user(DisplayMode::X11)),
            hdr: None,
            monitor: None,
            applied_mode: Some(DisplayMode::X11),
            applied_hdr: None,
            applied_monitor: None,
            pending: false,
        };
        assert_eq!(
            display_text(&report(Some(view), &[], Some(DisplayMode::X11))),
            format!(
                "display: x11 (L4 user override)\n\
                 the launcher unsets PROTON_ENABLE_WAYLAND and PROTON_USE_WAYLAND for the game\n\
                 a launch from this shell: x11 (WAYLAND_DISPLAY is unset; a desktop launch has \
                 the session's)\n{tail}"
            )
        );
    }

    #[test]
    fn args_quote_what_the_shell_would_split() {
        let r = ArgsReport {
            gogid: "1423049311".into(),
            user_file: "/d/compat/1423049311.toml".into(),
            built: true,
            exe: Some("games/1423049311/prefix/drive_c/GOG/x/bin/x64/Cyberpunk2077.exe".into()),
            game_args: vec![],
            game_args_from: "GOG (gogo-launch.toml)".into(),
            extra_args: Some(Sourced {
                value: vec![
                    "--launcher-skip".into(),
                    "-skipStartScreen".into(),
                    "a b".into(),
                ],
                layer: Layer::User,
            }),
            launch_args: vec![
                "--launcher-skip".into(),
                "-skipStartScreen".into(),
                "a b".into(),
            ],
            applied: Some(vec![]),
            pending: true,
            warnings: vec![],
        };
        assert_eq!(
            args_text(&r),
            "exe: games/1423049311/prefix/drive_c/GOG/x/bin/x64/Cyberpunk2077.exe\n\
             game args: (none)  (GOG (gogo-launch.toml))\n\
             extra args: --launcher-skip -skipStartScreen 'a b'  (L4 user override)\n\
             the exe gets: --launcher-skip -skipStartScreen 'a b' (then any args launch.sh itself is given)\n\
             pending: launch.conf still has (none); the next launch's setup writes the new ones\n\
             your override file: /d/compat/1423049311.toml\n"
        );
    }
}
