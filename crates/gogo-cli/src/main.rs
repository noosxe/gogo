//! clap parsing, tracing setup, exit-code mapping — no business logic (`docs/design/01` §1).
//! This crate is a thin shell over `gogo-core`; everything domain lives there.

mod cli;
mod output;

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
    Io(std::io::Error),
}

impl From<gogo::macapp::MacAppError> for AppError {
    fn from(e: gogo::macapp::MacAppError) -> Self {
        Self::MacApp(e)
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
        Command::Info { game, json } => info(game, json).await,
        Command::Download(args) => download(args, cli.no_progress, cli.quiet).await,
        Command::Update { games, all, strict } => update(games, all, strict).await,
        Command::Verify {
            games,
            all,
            path,
            fix,
            rebuild,
        } => verify(games, all, path, fix, rebuild).await,
        Command::Config { sub } => config_cmd(sub),
        Command::MacApp {
            installer,
            name,
            runtime,
            runtime_path,
            recipe,
            out,
            yes,
            json_progress,
            json,
        } => {
            mac_app(
                installer,
                name,
                runtime,
                runtime_path,
                recipe,
                out,
                yes,
                json_progress,
                json,
            )
            .await
        }
        Command::Runtimes(args) => runtimes_cmd(args).await,
        Command::Shim { tool, args } => Ok(gogo_shims::run_tool(&tool, &args)),
        Command::Completions { shell } => completions(shell),
    }
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
    if let Some(needle) = &search {
        let needle = needle.to_lowercase();
        games.retain(|g| g.title.to_lowercase().contains(&needle));
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
            gogo::state::cache::load_product(game.id).unwrap_or(None)
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
async fn mac_app(
    installer: std::path::PathBuf,
    name: Option<String>,
    runtime: Option<crate::cli::RuntimeSource>,
    runtime_path: Option<std::path::PathBuf>,
    recipe: Option<String>,
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

    let recipe = recipe
        .as_deref()
        .map(gogo::macapp::RecipeChoice::parse)
        .unwrap_or_default();
    // Interactive selection (design 11 §1): a TTY prompt over the ranked
    // candidates; non-TTY runs take the ranked best.
    let on_select: Option<std::sync::Arc<gogo::macapp::SelectFn>> =
        if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            Some(std::sync::Arc::new(|lines: &[String]| -> Option<usize> {
                eprintln!("recipes available on lutris.net:");
                for (i, line) in lines.iter().enumerate() {
                    eprintln!("  {}. {line}", i + 1);
                }
                eprint!("apply which? [1-{}, none] ", lines.len());
                use std::io::Write as _;
                std::io::stderr().flush().ok();
                let mut ans = String::new();
                std::io::stdin().read_line(&mut ans).ok()?;
                let t = ans.trim();
                if t.eq_ignore_ascii_case("none") {
                    return None;
                }
                t.parse::<usize>()
                    .ok()
                    .filter(|n| (1..=lines.len()).contains(n))
                    .map(|n| n - 1)
            }))
        } else {
            None
        };

    let opts = gogo::macapp::MacAppOptions {
        installer,
        name,
        mode: runtime.map(Into::into).unwrap_or_default(),
        runtime_path,
        out_dir: out.unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        }),
        overwrite: yes,
        recipe,
    };

    // Plan: resolve + probe the runtime, validate the installer, size things up.
    let plan = {
        let opts = opts.clone();
        let jp = json_progress;
        tokio::task::spawn_blocking(move || {
            gogo::macapp::plan(&opts, &mut |ev| emit_macapp_event(ev, jp))
        })
        .await
        .map_err(|e| AppError::Io(std::io::Error::other(e.to_string())))??
    };

    // Confirmation mirrors `download --all` (03 §3): on a TTY ask, non-TTY
    // without --yes print the plan and abort — a conversion never starts
    // silently, and hundreds-of-MB bundles deserve the pause (docs/13 §4).
    if !yes {
        eprintln!(
            "converting:\n  installer: {}\n  bundle:    {}\n  runtime:   {} ({})\n  est. size: {}",
            plan.installer.installer.display(),
            plan.bundle_path.display(),
            plan.runtime.version,
            plan.runtime.kind,
            gogo::fmt::human_size(plan.estimated_bytes),
        );
        if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            eprintln!("gogo: non-interactive without --yes — aborting (plan above)");
            return Ok(cli::exit::USAGE);
        }
        print!("proceed? [y/N]: ");
        {
            use std::io::Write as _;
            std::io::stdout().flush().ok();
        }
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if !line.trim().eq_ignore_ascii_case("y") {
            eprintln!("gogo: aborted");
            return Ok(cli::exit::USAGE);
        }
    }

    let bytes = {
        let plan = plan.clone();
        let jp = json_progress;
        let sel = on_select.clone();
        tokio::task::spawn_blocking(move || {
            let policy = if cfg!(target_os = "macos") {
                gogo::macapp::bundle::CodesignPolicy::Adhoc
            } else {
                gogo::macapp::bundle::CodesignPolicy::Skip
            };
            gogo::macapp::execute(&plan, policy, sel.as_deref(), &mut |ev| {
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
                "name": plan.display,
                "slug": plan.slug,
                "runtime": {
                    "kind": plan.runtime.kind,
                    "version": plan.runtime.version,
                    "wine": plan.runtime.wine,
                },
                "bytes": bytes,
            })
        );
    } else {
        println!(
            "created {} ({}) — drag it into /Applications and double-click to play",
            plan.bundle_path.display(),
            gogo::fmt::human_size(bytes),
        );
    }
    Ok(cli::exit::OK)
}

// — runtimes (design 12 §4/§10): Wine runtime catalog + store management —

/// `gogo runtimes list|install|prune`. `list` live-probes available catalog
/// entries with a short client timeout (`--offline` skips); `install` is
/// macOS-only — runtimes exist to feed `mac-app`.
async fn runtimes_cmd(args: crate::cli::RuntimesArgs) -> Result<i32, AppError> {
    use gogo::macapp::{catalog, store};

    match args.cmd {
        crate::cli::RuntimesCmd::List { json, offline } => {
            let cached = store::load_manifest();
            let http = if offline {
                None
            } else {
                catalog::http_client().ok()
            };
            let mut json_rows: Vec<serde_json::Value> = Vec::new();
            let mut table = comfy_table::Table::new();
            table.set_header(vec!["ID", "STATUS", "LATEST", "SIZE", "ARCH", "CACHED"]);
            for entry in catalog::entries() {
                let latest = match &http {
                    Some(http) if entry.discovery.is_some() => {
                        catalog::probe(entry, None, http, catalog::GITHUB_API)
                            .await
                            .ok()
                    }
                    _ => None,
                };
                let status = match &entry.status {
                    catalog::Status::Available => "available".to_string(),
                    catalog::Status::ConsentRequired => "consent (Apple EULA)".to_string(),
                    catalog::Status::Tool => "tool (design 12 §8)".to_string(),
                    catalog::Status::Gone { since, .. } => format!("gone since {since}"),
                };
                let mine: Vec<&store::ManifestEntry> =
                    cached.iter().filter(|m| m.id == entry.id).collect();
                let cached_col = if mine.is_empty() {
                    "-".to_string()
                } else {
                    mine.iter()
                        .map(|m| m.version.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                table.add_row(vec![
                    entry.id.to_string(),
                    status.clone(),
                    latest
                        .as_ref()
                        .map(|l| l.tag.clone())
                        .unwrap_or_else(|| "-".into()),
                    latest
                        .as_ref()
                        .map(|l| gogo::fmt::human_size(l.size))
                        .unwrap_or_else(|| "-".into()),
                    entry.arch.to_string(),
                    cached_col,
                ]);
                json_rows.push(serde_json::json!({
                    "id": entry.id,
                    "status": status,
                    "license": entry.license,
                    "arch": entry.arch,
                    "latest": latest.as_ref().map(|l| serde_json::json!({
                        "tag": l.tag,
                        "bytes": l.size,
                    })),
                    "cached": mine.iter().map(|m| serde_json::json!({
                        "version": m.version,
                        "bytes": m.bytes,
                        "fetched_at": m.fetched_at,
                    })).collect::<Vec<_>>(),
                }));
                if let catalog::Status::Gone { story, .. } = &entry.status {
                    eprintln!("note: {}: {story}", entry.id);
                }
            }
            if json {
                println!("{}", serde_json::json!({ "entries": json_rows }));
            } else {
                println!("{table}");
            }
            Ok(cli::exit::OK)
        }
        crate::cli::RuntimesCmd::Install { id, version, json } => {
            if !cfg!(target_os = "macos") {
                eprintln!(
                    "gogo: `gogo runtimes install` is macOS-only — runtimes feed `gogo mac-app`"
                );
                return Ok(cli::exit::USAGE);
            }
            let Some(entry) = catalog::find(&id) else {
                eprintln!("gogo: unknown runtime id '{id}' — see `gogo runtimes list`");
                return Ok(cli::exit::USAGE);
            };
            let http = catalog::http_client()?;
            let m = store::install(
                entry,
                version.as_deref(),
                &http,
                catalog::GITHUB_API,
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
        crate::cli::RuntimesCmd::Prune { all, id } => {
            let ids: Vec<String> = if all {
                store::load_manifest()
                    .iter()
                    .map(|m| m.id.clone())
                    .collect()
            } else {
                match id {
                    Some(i) => vec![i],
                    None => {
                        eprintln!("gogo: `gogo runtimes prune` needs an <id> or --all");
                        return Ok(cli::exit::USAGE);
                    }
                }
            };
            if ids.is_empty() {
                println!("nothing to prune");
                return Ok(cli::exit::OK);
            }
            for path in store::prune(&ids)? {
                println!("pruned {}", path.display());
            }
            Ok(cli::exit::OK)
        }
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
