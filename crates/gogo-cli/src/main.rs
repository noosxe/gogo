//! clap parsing, tracing setup, exit-code mapping — no business logic (`docs/design/01` §1).
//! This crate is a thin shell over `gogo-core`; everything domain lives there.

mod cli;
mod lane;
mod menus;
mod output;
mod prompt;

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
            // UnsupportedPlatform is a usage error (exit 2); the rest of the
            // taxonomy is conversion failure (exit 7) — docs/design/10 §4.
            match e {
                gogo::macapp::MacAppError::UnsupportedPlatform => cli::exit::USAGE,
                _ => cli::exit::MACAPP,
            }
        }
        Err(AppError::Depot(e)) => match e {
            gogo::gog::depot::DepotError::UnsupportedPlatform(_)
            | gogo::gog::depot::DepotError::NoCacheDir => {
                eprintln!("gogo: {e}");
                cli::exit::USAGE
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

/// RUST_LOG wins if set; otherwise derive from --quiet/-v (`docs/design/07` §5).
/// Log output goes to stderr; stdout stays results-only (`docs/design/03` §4).
fn init_tracing(cli: &Cli) {
    use tracing_subscriber::EnvFilter;

    let default = if cli.quiet {
        "warn".to_string()
    } else {
        match cli.verbose {
            0 => "info",
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
            json,
            refresh,
        } => list(search, os, with_dlcs, json, refresh, color).await,
        Command::Search(args) => search(args).await,
        Command::Info { game, json } => info(game, json).await,
        Command::Download(args) => download(args, cli.no_progress, cli.quiet).await,
        Command::Depot(args) => depot_cmd(args).await,
        Command::Update { games, all, strict } => update(games, all, strict).await,
        Command::Verify {
            games,
            all,
            path,
            fix,
            rebuild,
        } => verify(games, all, path, fix, rebuild).await,
        Command::Config { sub } => config_cmd(sub),
        Command::MacApp { sub: Some(sub), .. } => mac_app_sub(sub),
        // The bare `--runtime` menu (#76) is a build-time form only.
        Command::MacApp {
            retarget: Some(_),
            runtime: Some(None),
            ..
        } => {
            eprintln!("gogo: --retarget needs a runtime id (`--runtime <RUNTIME>`)");
            Ok(cli::exit::USAGE)
        }
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
        Command::MacApp {
            sub: None,
            retarget: _,
            input,
            from_depot,
            name,
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

fn load_config_from_env() -> Result<Config, ConfigError> {
    let loaded = match CONFIG_PATH.get().cloned().flatten() {
        Some(path) => Config::load_from(&path, |k| std::env::var(k).ok())?,
        None => Config::load()?,
    };
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
    json: bool,
    refresh: bool,
    color: ColorWhen,
) -> Result<i32, AppError> {
    use gogo::gog::catalog::Product;

    let ctx = session_ctx().await?;
    let mut games = library_products(&ctx).await?;
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

    // 03 §1: the library list is always live; catalog data (os, sizes, versions)
    // comes from the product cache, fetched live on a miss or with --refresh.
    // A failed fetch degrades the row (empty os/size), never the command.
    let token = bearer(&ctx).await?;
    let mut products: Vec<Product> = Vec::with_capacity(games.len());
    for game in &games {
        let cached = if refresh {
            None
        } else {
            gogo::state::cache::load_typed_product(game.id)
        };
        let product = match cached {
            Some(p) => p,
            None => match gogo::gog::catalog::detail(&ctx.http, &token, game.id).await {
                Ok(p) => {
                    if let Err(e) = gogo::state::cache::save_product(&p) {
                        eprintln!("warn: catalog cache write failed: {e}");
                    }
                    p
                }
                Err(e) => {
                    eprintln!("warn: catalog fetch failed for {}: {e}", game.title);
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

    let os_filter = os_filter_to_catalog(os);
    // --os filters on what the catalog says the game delivers; rows with unknown
    // os data (fetch degraded) survive the filter rather than vanishing.
    if let Some(want) = os_filter {
        products.retain(|p| {
            let os_list = product_os(p);
            os_list.is_empty() || os_list.contains(&want)
        });
    }

    let state = ctx.state.lock().unwrap().clone();
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
    let token = bearer(&ctx).await?;
    let (lib, warnings) = search::gather::gather(&ctx.http, &token, args.refresh).await?;
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
    let token = bearer(ctx.as_ref()).await?;
    let products = resolve_games(&ctx, &token, &[game]).await?;
    let Some(product) = products.first() else {
        eprintln!("no such game");
        return Ok(cli::exit::USAGE);
    };
    // Fresh detail (downloads + DLC tree).
    let detail = gogo::gog::catalog::detail(&ctx.http, &token, product.id).await?;
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
                eprintln!("no game matches `{name}` — try `gogo list`");
                return Ok(Vec::new());
            }
        }
    }
    Ok(out)
}

// — download —

/// `gogo depot <GAME>` — Model B lane (docs/design/14): resolve the latest
/// public Windows build and materialize it into the user cache directory.
/// macOS-only materialization (D7); resolve+plan works everywhere.
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

    let token = bearer(&ctx).await?;
    let session = ctx
        .session
        .as_ref()
        .and_then(|p| p.gog_warp_core())
        .ok_or(AppError::Usage)?;
    let _ = token; // bearer() above validates the session cheaply

    let selected = resolve_games(&ctx, &token, std::slice::from_ref(&args.game)).await?;
    let Some(game) = selected.first() else {
        eprintln!("gogo: no owned game matches {}", args.game);
        return Ok(cli::exit::USAGE);
    };

    let limit = match &args.limit {
        Some(spec) => Some(gogo::config::parse_rate(spec)?),
        None => None,
    };
    let opts = DepotOptions {
        language: args.language.clone(),
        with_dlcs: args.with_dlcs,
        password: args.password.clone(),
        dir: args.dir.clone(),
        limit,
        disk_check: !args.no_disk_check,
        dry_run: args.dry_run,
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<DepotEvent>();
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
    tokio::pin!(runner);

    let mut last_dl: Option<(u64, u64)> = None;
    let install_dir = loop {
        tokio::select! {
            res = &mut runner => break res?,
            maybe = rx.recv() => {
                if let Some(e) = maybe {
                    render_depot_event(&e, machine, &mut last_dl);
                }
            }
        }
    };
    // The runner may out-race the last events in the channel; drain in order
    // before the summary.
    while let Ok(e) = rx.try_recv() {
        render_depot_event(&e, machine, &mut last_dl);
    }
    if !machine {
        // final newline after the \r counter line
        if last_dl.is_some() {
            println!();
        }
    }

    if args.json {
        let summary = serde_json::json!({
            "productId": game.id,
            "title": title,
            "installDir": install_dir,
            "dryRun": args.dry_run,
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

/// One depot event → NDJSON (machine) or human lines with a \r byte counter
/// for the download phase (design 14 D4).
fn render_depot_event(
    event: &gogo::gog::depot::DepotEvent,
    machine: bool,
    last_dl: &mut Option<(u64, u64)>,
) {
    use gogo::gog::depot::DepotEvent::*;
    if machine {
        println!("{}", serde_json::to_string(event).unwrap());
        return;
    }
    match event {
        Resolved {
            title,
            version,
            generation,
            ..
        } => {
            println!("gogo: {title} build {version} (generation {generation})")
        }
        Plan {
            install_dir,
            download_size,
            disk_size,
            language,
            dlcs,
        } => {
            println!(
                "gogo: plan {} (download {}, disk {}, {}{})",
                install_dir.display(),
                gogo::fmt::human_size(*download_size),
                gogo::fmt::human_size(*disk_size),
                language,
                if dlcs.is_empty() {
                    String::new()
                } else {
                    format!(" +{} dlcs", dlcs.len())
                }
            );
        }
        Preparing => println!("gogo: preparing"),
        Allocating { fraction } | Verifying { fraction } => {
            print!(
                "\r{}: {:.0}%",
                if matches!(event, Allocating { .. }) {
                    "allocating"
                } else {
                    "verifying"
                },
                fraction * 100.0
            );
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
        Downloading {
            downloaded,
            total_download,
            ..
        } => {
            *last_dl = Some((*downloaded, *total_download));
            print!(
                "\rdownloading {} / {}",
                gogo::fmt::human_size(*downloaded),
                gogo::fmt::human_size(*total_download)
            );
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
        Finished { .. } => {}
    }
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
        with_dlcs,
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

    let token = bearer(&ctx).await?;
    let selected = if all {
        library::list_games(&ctx.http, &token).await?
    } else {
        resolve_games(&ctx, &token, &games).await?
    };
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
    let filters = Filters {
        os,
        language: Some(ctx.cfg.gog.language.clone()),
        kinds,
        with_dlcs,
    };

    let mut products = Vec::new();
    for game in &selected {
        let detail = gogo::gog::catalog::detail(&ctx.http, &token, game.id).await?;
        products.push(detail);
    }

    let inner = async {
        let tasks = engine::plan(&ctx, &products, &filters, force).await;
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
                let confirmed = dialoguer::Confirm::new()
                    .with_prompt("download the plan above?")
                    .default(false)
                    .interact()
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
    let token = bearer(&ctx).await?;
    let selected = if all {
        library::list_games(&ctx.http, &token).await?
    } else {
        resolve_games(&ctx, &token, &games).await?
    };
    if selected.is_empty() {
        return Ok(cli::exit::USAGE);
    }
    let filters = Filters {
        os: host_or_configured_os(),
        language: Some(ctx.cfg.gog.language.clone()),
        kinds: Vec::new(),
        with_dlcs: false,
    };
    let mut products = Vec::new();
    for game in &selected {
        products.push(gogo::gog::catalog::detail(&ctx.http, &token, game.id).await?);
    }
    let mut tasks = engine::plan(&ctx, &products, &filters, false).await;
    // 06 §4 strict path: version-equal files may still have drifted — compare
    // checksum timestamp (primary) and build id via one resolution each.
    if strict {
        for task in tasks
            .iter_mut()
            .filter(|t| matches!(t.status, gogo::engine::task::Status::Present))
        {
            if engine::strict_outdated(&ctx, &token, task).await {
                task.status = gogo::engine::task::Status::Outdated;
            }
        }
    }
    let tasks: Vec<_> = tasks
        .into_iter()
        .filter(|t| matches!(t.status, gogo::engine::task::Status::Outdated))
        .collect();
    if tasks.is_empty() {
        println!("everything up to date");
        return Ok(cli::exit::OK);
    }
    print_plan(&tasks);
    let summary = engine::execute(&ctx, tasks).await;
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
        if gogo::engine::verify_against(&dest, &doc) {
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
                    if gogo::engine::verify_against(&dest, &doc) {
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
        let token = bearer(&ctx).await?;
        let product_ids: Vec<u64> = {
            let mut ids: Vec<u64> = failed_paths.iter().map(|r| r.product_id).collect();
            ids.sort_unstable();
            ids.dedup();
            ids
        };
        let mut products = Vec::new();
        for id in &product_ids {
            match gogo::gog::catalog::detail(&ctx.http, &token, *id).await {
                Ok(p) => products.push(p),
                Err(e) => eprintln!("warn: catalog fetch failed for {id}: {e}"),
            }
        }
        let filters = Filters {
            os: None,
            language: Some(ctx.cfg.gog.language.clone()),
            kinds: Vec::new(),
            // Corrupt DLC files need their DLC planned too (A1: DLCs via parent).
            with_dlcs: true,
        };
        // force=true: bypass the present-skip so the corrupt file is replaced.
        let tasks: Vec<_> = engine::plan(&ctx, &products, &filters, true)
            .await
            .into_iter()
            .filter(|t| {
                failed_paths.iter().any(|r| {
                    r.product_id == t.product_id && r.dlc_id == t.dlc_id && r.file_id == t.file.id
                })
            })
            .collect();
        if tasks.is_empty() {
            eprintln!("gogo: --fix found no downloadable replacement for the corrupt files");
        } else {
            let summary = engine::execute(&ctx, tasks).await;
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
                if gogo::engine::verify_against(path, doc) {
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
        if !gogo::engine::verify_against(&path, &doc) {
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

/// `gogo mac-app` — convert one downloaded Windows installer into a
/// self-contained `.app`. The pipeline is synchronous (child processes + fs
/// copies), so it runs on blocking threads; events render exactly like the
/// download engine's (NDJSON on stdout for `--json-progress`, one-liners on
/// stderr otherwise, 03 §4).
#[allow(clippy::too_many_arguments)]
/// Resolve a game selector to its build source (#73, design 15 §1): the
/// depot tree when a depot build exists (warm cache, else an auto-download
/// with `--yes` or an interactive confirm), else — unless `--from-depot`
/// asked for the depot lane explicitly — its downloaded offline installer.
/// Returns `(installer, game_tree)`, exactly one `Some`.
async fn resolve_game_source(
    selector: &str,
    explicit: bool,
    auto: bool,
    machine: bool,
) -> Result<(Option<std::path::PathBuf>, Option<std::path::PathBuf>), AppError> {
    let built = AppContext::build(load_config_from_env()?, true)
        .await
        .map_err(|e| {
            eprintln!("gogo: {e}");
            AppError::Usage
        })?;
    let ctx = std::sync::Arc::new(built);
    let token = bearer(&ctx).await?;
    let session = ctx
        .session
        .as_ref()
        .and_then(|p| p.gog_warp_core())
        .ok_or(AppError::Usage)?;
    let sel = selector.to_string();
    let selected = resolve_games(&ctx, &token, std::slice::from_ref(&sel)).await?;
    let Some(game) = selected.first() else {
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

    let mut facts = lane::GameFacts {
        cached_tree: find_game_tree(&root),
        ..Default::default()
    };
    if facts.cached_tree.is_none() {
        facts.has_depot_build = gogo::gog::depot::has_public_build(session, game.id)
            .await
            .map_err(AppError::Depot)?;
    }
    if !facts.has_depot_build {
        let download_root =
            std::path::PathBuf::from(load_config_from_env()?.download.dir.unwrap_or_default());
        let state = gogo::state::State::load().unwrap_or_default();
        facts.installer = lane::downloaded_installer(&state.installs, game.id, &download_root);
    }
    match lane::pick(facts, explicit) {
        lane::Lane::DepotCached(tree) => return Ok((None, Some(tree))),
        lane::Lane::DepotDownload => {}
        lane::Lane::InstallerFallback(exe) => {
            eprintln!(
                "gogo: {title} has no depot build — using its offline installer {}",
                exe.display()
            );
            return Ok((Some(exe), None));
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
                 (`gogo mac-app {selector}` falls back to it)"
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
        go = dialoguer::Confirm::new()
            .with_prompt(format!(
                "depot cache empty for {title} — download it now? (same as `gogo depot`)"
            ))
            .default(true)
            .interact()
            .unwrap_or(false);
    }
    if !go {
        eprintln!(
            "gogo: no depot cache for {title} — run `gogo depot {selector}` first \
             (or pass --yes to download here)"
        );
        return Err(AppError::Usage);
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<gogo::gog::depot::DepotEvent>();
    let opts = gogo::gog::depot::DepotOptions::default();
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
    tokio::pin!(runner);
    let result = loop {
        tokio::select! {
            res = &mut runner => break res,
            maybe = rx.recv() => {
                if let (Some(gogo::gog::depot::DepotEvent::Plan { download_size, .. }), false) =
                    (maybe, machine)
                {
                    println!(
                        "gogo: downloading {title} via depot ({})",
                        gogo::fmt::human_size(download_size)
                    );
                }
            }
        }
    };
    result.map_err(AppError::Depot)?;
    let tree = find_game_tree(&root).ok_or_else(|| {
        eprintln!(
            "gogo: depot download finished but no game tree found under {}",
            root.display()
        );
        AppError::Usage
    })?;
    Ok((None, Some(tree)))
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
            let warnings: Vec<String> =
                db.warnings
                    .iter()
                    .map(|w| format!("compat DB: unknown key {w}"))
                    .chain(user.warnings.iter().map(|w| {
                        format!("{}: unknown key {w}", compat::user_path(&gogid).display())
                    }))
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
                for f in &report.findings {
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
            gogo::macapp::d3dmetal::ensure(
                &format!("--renderer d3dmetal was asked for {gogid}"),
                p.as_deref(),
                &mut |ev| emit_macapp_event(ev, false),
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

#[allow(clippy::too_many_arguments)]
async fn mac_app(
    input: Option<String>,
    from_depot: Option<String>,
    name: Option<String>,
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
    if !cfg!(target_os = "macos") {
        // Design 10: macOS-only for now; fail fast with the remedy (exit 2).
        return Err(AppError::MacApp(
            gogo::macapp::MacAppError::UnsupportedPlatform,
        ));
    }

    // Design 15 §1 / #73: exactly one source. A game selector takes the
    // depot lane (installer fallback when it has no depot build); an .exe
    // path keeps the installer lane; a directory is a ready game tree.
    let machine = json || json_progress;
    let (installer, game_tree) = match lane::classify(input.as_deref(), from_depot.as_deref()) {
        Some(lane::Input::Installer(p)) => (Some(p), None),
        Some(lane::Input::Tree(t)) => (None, Some(t)),
        Some(lane::Input::Game { selector, explicit }) => {
            resolve_game_source(&selector, explicit, yes, machine).await?
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
    };

    // Plan: resolve + probe the runtime, validate the installer, size things up.
    let plan = {
        let opts = opts.clone();
        let jp = json_progress;
        let p = prompter.clone();
        tokio::task::spawn_blocking(move || {
            gogo::macapp::plan(&opts, p.as_deref(), &mut |ev| emit_macapp_event(ev, jp))
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
        tokio::task::spawn_blocking(move || {
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
                emit_macapp_event(ev, jp)
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
                    .map(|id| gogo::macapp::launch::games_dir().join(id)),
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
            println!(
                "game files, prefix and saves: {}",
                gogo::macapp::launch::games_dir().display()
            );
        }
    }
    Ok(cli::exit::OK)
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

/// A version as listings show it: a release tag's leading `v` dropped
/// (`v7.1.0.0` and `7.1.0.0` are one version).
fn shown_version(v: &str) -> &str {
    v.strip_prefix('v')
        .filter(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or(v)
}

/// `gogo runtimes|renderers list|install|prune`. `list` checks the latest
/// version online with a short client timeout (`--offline` skips);
/// `install` is macOS-only — runtimes and renderers exist to feed `mac-app`.
async fn store_cmd(shelf: Shelf, cmd: crate::cli::StoreCmd) -> Result<i32, AppError> {
    use gogo::macapp::{catalog, store};

    let (command, noun) = (shelf.command(), shelf.noun());
    match cmd {
        crate::cli::StoreCmd::List { json, offline } => {
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
            let mut table = comfy_table::Table::new();
            table.set_header(vec!["ID", "STATUS", "LATEST", "SIZE", "CACHED", "USED BY"]);
            for entry in catalog::entries().iter().filter(|e| shelf.holds(e)) {
                let tool = matches!(entry.kind, catalog::Kind::Tool { .. });
                let latest = match &http {
                    Some(http) if entry.discovery.is_some() && !tool => {
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
                json_rows.push(serde_json::json!({
                    "id": entry.id,
                    "default": entry.id == catalog::DEFAULT_RUNTIME,
                    "status": entry.status.key(),
                    "gone_since": match &entry.status {
                        catalog::Status::Gone { since, .. } => Some(since),
                        _ => None,
                    },
                    "license": entry.license,
                    "arch": entry.arch_label(),
                    "vendor_signed": entry.vendor_signed,
                    "latest": latest.as_ref().map(|l| serde_json::json!({
                        "tag": l.tag,
                        "bytes": l.size,
                    })),
                    "cached": mine.iter().map(|m| serde_json::json!({
                        "version": m.version,
                        "bytes": m.bytes,
                        "fetched_at": m.fetched_at,
                    })).collect::<Vec<_>>(),
                    "derived": derived,
                    "used_by": used_by,
                }));
                let versions: Vec<&str> = mine.iter().map(|m| shown_version(&m.version)).collect();
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
                    catalog::Status::Available if entry.id == catalog::DEFAULT_RUNTIME => {
                        "available · default".to_string()
                    }
                    catalog::Status::Available | catalog::Status::Tool => "available".to_string(),
                    catalog::Status::ConsentRequired => "needs consent (Apple licence)".to_string(),
                    // A retired entry only matters while copies of it remain.
                    catalog::Status::Gone { .. } if mine.is_empty() => continue,
                    catalog::Status::Gone { since, .. } => {
                        format!("gone since {since} (cached copies still work)")
                    }
                };
                let latest_tag = latest.as_ref().map(|l| shown_version(&l.tag));
                let mut cached_col = if versions.is_empty() {
                    "-".to_string()
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
            if json {
                println!("{}", serde_json::json!({ "entries": json_rows }));
            } else {
                println!("{table}");
                for note in footnotes {
                    println!("{note}");
                }
            }
            Ok(cli::exit::OK)
        }
        crate::cli::StoreCmd::Install { id, version, json } => {
            if !cfg!(target_os = "macos") {
                eprintln!(
                    "gogo: `gogo {command} install` is macOS-only — {command} feed `gogo mac-app`"
                );
                return Ok(cli::exit::USAGE);
            }
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
            // D3DMetal (#71) installs here only with the stored consent.
            let consent =
                id == gogo::macapp::Renderer::D3dMetal.id() && gogo::macapp::d3dmetal::available();
            let http = catalog::http_client()?;
            let m = store::install(
                entry,
                version.as_deref(),
                &http,
                catalog::GITHUB_API,
                consent,
                &mut |ev| {
                    emit_macapp_event(ev, false);
                },
            )
            .await?;
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
            if out.removed.is_empty() {
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
        crate::cli::AppsCmd::List { json } => {
            let all = apps::list(&logs)?;
            if json {
                println!("{}", serde_json::json!({ "apps": all }));
                return Ok(cli::exit::OK);
            }
            if all.is_empty() {
                println!("no games set up yet: `gogo mac-app <GAME>` builds one");
                return Ok(cli::exit::OK);
            }
            let mut table = comfy_table::Table::new();
            table.set_header(vec![
                "GOGID",
                "TITLE",
                ".APP",
                "RUNTIME",
                "RENDERER",
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
                    gogo::fmt::human_size(a.bytes),
                    dash(a.last_launch.as_deref()),
                ]);
            }
            println!("{table}");
            let orphans = all.iter().filter(|a| a.orphaned()).count();
            if orphans > 0 {
                println!(
                    "{orphans} game(s) with a missing .app: `gogo apps prune --orphaned` \
                     removes them (it asks first)"
                );
            }
            Ok(cli::exit::OK)
        }
        crate::cli::AppsCmd::Show { target, json } => {
            let Some(a) = apps::get(&target, &logs)? else {
                eprintln!("gogo: {target} isn't set up on this Mac — see `gogo apps list`");
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
            println!("  .app:        {}", app_column(&a));
            if let Some(b) = &a.bundle {
                println!("               {}", b.display());
            }
            println!("  state:       {}", a.root.display());
            println!("  prefix:      {}", a.prefix.display());
            println!("  logs:        {}", path(a.logs.as_deref()));
            println!("  override:    {}", path(a.override_file.as_deref()));
            println!("  runtime:     {}", dash(a.runtime.as_deref()));
            println!("  renderer:    {}", dash(a.renderer.as_deref()));
            println!("  size:        {}", gogo::fmt::human_size(a.bytes));
            println!("  last launch: {}", dash(a.last_launch.as_deref()));
            Ok(cli::exit::OK)
        }
        crate::cli::AppsCmd::Remove {
            target,
            keep_override,
            yes,
        } => {
            let Some(a) = apps::get(&target, &logs)? else {
                eprintln!("gogo: {target} isn't set up on this Mac — see `gogo apps list`");
                return Ok(cli::exit::USAGE);
            };
            remove_apps(&[a], keep_override, yes)
        }
        crate::cli::AppsCmd::Prune {
            orphaned: _,
            keep_override,
            yes,
        } => {
            let orphans: Vec<apps::App> = apps::list(&logs)?
                .into_iter()
                .filter(apps::App::orphaned)
                .collect();
            if orphans.is_empty() {
                println!("nothing to prune: every game's .app is where it was");
                return Ok(cli::exit::OK);
            }
            remove_apps(&orphans, keep_override, yes)
        }
    }
}

/// `present`, `missing`, `portable` (and `-` when nothing is recorded).
fn app_column(a: &gogo::macapp::apps::App) -> String {
    let kind = if a.portable { "portable, " } else { "" };
    match (&a.bundle, a.bundle_present) {
        (None, _) => "-".to_string(),
        (Some(_), true) => format!("{kind}present"),
        (Some(_), false) => format!("{kind}missing"),
    }
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
    }
    if !yes {
        if !console::Term::stdout().features().is_attended() {
            eprintln!("gogo: not attached to a terminal — pass --yes to remove the above");
            return Ok(cli::exit::USAGE);
        }
        let confirmed = dialoguer::Confirm::new()
            .with_prompt(format!(
                "remove {} game(s), {}?",
                targets.len(),
                gogo::fmt::human_size(total)
            ))
            .default(false)
            .interact()
            .unwrap_or(false);
        if !confirmed {
            println!("aborted");
            return Ok(cli::exit::OK);
        }
    }
    let mut failed = false;
    for a in targets {
        match gogo::macapp::apps::remove(a, keep_override) {
            Ok(out) => {
                for p in &out.paths {
                    println!("removed {}", p.display());
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
            let mut table = comfy_table::Table::new();
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
                let mut table = comfy_table::Table::new();
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
                let mut table = comfy_table::Table::new();
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
        _ => None,
    };
    if let Some(l) = line {
        eprintln!("gogo: {l}");
    }
}

/// Smoke tests for the completion generation (08 §2: thin frontend gets smoke tests):
/// every shell yields a script naming the binary, with the shell's load-bearing
/// marker present.
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
