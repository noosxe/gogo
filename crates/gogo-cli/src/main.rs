//! clap parsing, tracing setup, exit-code mapping — no business logic (`docs/design/01` §1).
//! This crate is a thin shell over `gogo-core`; everything domain lives there.

#[allow(dead_code)] // clap_complete wires in with shell-completion generation (03 §6)
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

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    init_tracing(&cli);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    match rt.block_on(run(cli)) {
        Ok(code) => std::process::ExitCode::from(code as u8),
        // C1: the error type carries the class; auth 3 < network 4 < config-usage 2.
        Err(AppError::Config(e)) => {
            eprintln!("gogo: {e}");
            std::process::ExitCode::from(cli::exit::USAGE as u8)
        }
        Err(AppError::Auth(e)) => {
            eprintln!("gogo: {e}");
            std::process::ExitCode::from(cli::exit::AUTH as u8)
        }
        Err(AppError::Api(e)) => {
            eprintln!("gogo: {e}");
            std::process::ExitCode::from(cli::exit::NETWORK as u8)
        }
        Err(AppError::Io(e)) => {
            eprintln!("gogo: {e}");
            std::process::ExitCode::from(cli::exit::FAILED as u8)
        }
    }
}

/// Typed top-level error → exit class (03 §5, review C1).
enum AppError {
    Config(ConfigError),
    Auth(AuthError),
    Api(ApiError),
    Io(std::io::Error),
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
            with_dlcs: _,
            json,
            refresh: _,
        } => list(search, os, json, color).await,
        Command::Info { game, json } => info(game, json).await,
        Command::Download(args) => download(args).await,
        Command::Update {
            games,
            all,
            strict: _,
        } => update(games, all).await,
        Command::Verify {
            games,
            all,
            path,
            fix,
            rebuild,
        } => verify(games, all, path, fix, rebuild).await,
        Command::Config { sub } => config_cmd(sub),
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
    let session = gogo::auth::Session::restore(store.as_ref()).await?;
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
    json: bool,
    color: ColorWhen,
) -> Result<i32, AppError> {
    let ctx = session_ctx().await?;
    let mut games = library_products(&ctx).await?;
    if let Some(needle) = &search {
        let needle = needle.to_lowercase();
        games.retain(|g| g.title.to_lowercase().contains(&needle));
    }
    let os = os_filter_to_catalog(os);

    if json {
        let out: Vec<serde_json::Value> = games
            .iter()
            .map(|g| serde_json::json!({"id": g.id, "title": g.title, "slug": g.slug}))
            .collect();
        println!("{}", serde_json::to_string(&out).unwrap());
        return Ok(cli::exit::OK);
    }

    let state = ctx.state.lock().unwrap().clone();
    let rows: Vec<gogo::state::records::ListRow> = games
        .iter()
        .map(|g| gogo::state::records::ListRow {
            id: g.id,
            title: g.title.clone(),
            os,
            installers: 0,
            total_size: 0,
            installed_version: state
                .installs
                .iter()
                .find(|r| r.product_id == g.id)
                .and_then(|r| r.version.clone()),
        })
        .collect();
    println!("{}", output::list_table(&rows, color));
    Ok(cli::exit::OK)
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

async fn download(args: cli::DownloadArgs) -> Result<i32, AppError> {
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
        force: _,
        no_disk_check,
        json_progress: _,
        json: _,
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
    let session = gogo::auth::Session::restore(store.as_ref()).await?;
    let mut built = AppContext::assemble(cfg, http, Some(Arc::new(session)))
        .map_err(|e| AppError::Io(std::io::Error::other(e.to_string())))?;
    if no_disk_check {
        // D4: --no-disk-check skips the preflight.
        built.disk_check = false;
    }
    let ctx = Arc::new(built);

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

    let tasks = engine::plan(&ctx, &products, &filters).await;
    print_plan(&tasks);

    if dry_run {
        return Ok(cli::exit::OK);
    }

    // B1: `--all` without `--yes` requires a TTY confirmation; non-TTY aborts with
    // the plan printed (exit 2) — a bulk run never starts silently.
    if all && !yes {
        if console::Term::stdout().features().is_attended() {
            let confirmed = dialoguer::Confirm::new()
                .with_prompt("download the plan above?")
                .default(false)
                .interact()
                .unwrap_or(false);
            if !confirmed {
                println!("aborted");
                return Ok(cli::exit::OK);
            }
        } else {
            eprintln!("gogo: `--all` needs `--yes` when not attached to a terminal");
            return Ok(cli::exit::USAGE);
        }
    }

    // Ctrl-C → cancel token; workers stop at chunk boundaries (exit 130).
    let cancel_src = ctx.cancel.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        cancel_src.cancel();
    });

    let summary = engine::execute(&ctx, tasks).await;
    println!(
        "done: {} ok, {} skipped, {} failed",
        summary.ok, summary.skipped, summary.failed
    );
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

async fn update(games: Vec<String>, all: bool) -> Result<i32, AppError> {
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
    let tasks: Vec<_> = engine::plan(&ctx, &products, &filters)
        .await
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
    if !path.is_empty() || fix || rebuild {
        eprintln!("gogo: --path/--fix/--rebuild land with the catalog-backed verify pass");
        return Ok(cli::exit::USAGE);
    }
    let root = std::path::PathBuf::from(
        load_config_from_env()?
            .download
            .dir
            .clone()
            .unwrap_or_default(),
    );
    let ctx = session_ctx().await?;
    if all {
        // verify everything recorded
    } else if games.is_empty() {
        eprintln!("gogo: verify needs a game, --all, or --path");
        return Ok(cli::exit::USAGE);
    }
    let state = ctx.state.lock().unwrap().clone();
    let records: Vec<_> = state
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
        .collect();

    let mut failed = 0;
    let mut ok = 0;
    for record in records {
        let Some(xml) =
            gogo::state::cache::load_checksum(record.product_id, record.dlc_id, &record.file_id)
        else {
            println!(
                "  ? {} (no cached checksum — skipped)",
                record.path.display()
            );
            continue;
        };
        let doc = gogo::gog::checksum::parse(&xml)?;
        let dest = root.join(&record.path);
        if gogo::engine::verify_against(&dest, &doc) {
            println!("  ok {}", record.path.display());
            ok += 1;
        } else {
            println!("  FAIL {}", record.path.display());
            failed += 1;
        }
    }
    println!("{ok} verified, {failed} failed");
    Ok(if failed > 0 {
        cli::exit::CORRUPT
    } else {
        cli::exit::OK
    })
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
