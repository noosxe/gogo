//! `gogo linux-app dlc <gogid> list|add` (design 19 §1.3, §1.4, §4, §7): the network
//! side and the output; the work is `gogo::linuxapp::dlc`.

use std::sync::Arc;

use gogo::app::AppContext;
use gogo::engine::events::ProgressEvent;
use gogo::gog::depot::{self, BuildMatch, DepotEvent};
use gogo::gog::dlc::{self, DlcInfo};
use gogo::gog::library;
use gogo::linuxapp::dlc::{self as add, AddError, Game};

use crate::{AppError, cli, depot_mode, depot_progress};

/// Dispatch `gogo linux-app dlc <gogid> <cmd>`.
pub async fn run(gogid: String, cmd: cli::LinuxDlcCmd) -> Result<i32, AppError> {
    gogo::macapp::compat::validate_gogid(&gogid)?;
    let cache_base = depot::default_cache_dir();
    let game = add::load_game(&gogid, cache_base.as_deref())?;
    match cmd {
        cli::LinuxDlcCmd::List { json } => list(game, json).await,
        cli::LinuxDlcCmd::Add {
            names,
            all,
            dry_run,
            yes,
            limit,
            no_disk_check,
            json_progress,
            json,
        } => {
            let limit = match &limit {
                Some(spec) => Some(gogo::config::parse_rate(spec)?),
                None => None,
            };
            add_cmd(
                game,
                AddArgs {
                    names,
                    all,
                    dry_run,
                    yes,
                    limit,
                    disk_check: !no_disk_check,
                    json_progress,
                    json,
                },
                cache_base,
            )
            .await
        }
    }
}

struct AddArgs {
    names: Vec<String>,
    all: bool,
    dry_run: bool,
    yes: bool,
    limit: Option<u64>,
    disk_check: bool,
    json_progress: bool,
    json: bool,
}

async fn connect() -> Result<(Arc<AppContext>, String), AppError> {
    let built = AppContext::build(crate::load_config_from_env()?, true)
        .await
        .map_err(|e| {
            eprintln!("gogo: {e}");
            AppError::Usage
        })?;
    let ctx = Arc::new(built);
    let token = crate::bearer(&ctx).await?;
    Ok((ctx, token))
}

fn core(ctx: &AppContext) -> Result<&gog_warp::Core, AppError> {
    ctx.session
        .as_ref()
        .and_then(|p| p.gog_warp_core())
        .ok_or(AppError::Usage)
}

fn game_id(game: &Game) -> u64 {
    game.gogid.parse().unwrap_or(0)
}

fn title_of(game: &Game) -> String {
    gogo::linuxapp::game::read_launch_toml(&game.dirs)
        .map(|(t, _)| t.title)
        .unwrap_or_else(|_| game.gogid.clone())
}

fn language_of(game: &Game) -> String {
    game.content
        .language
        .clone()
        .unwrap_or_else(|| add::DEFAULT_LANGUAGE.to_string())
}

fn is_depot(game: &Game) -> bool {
    game.content.lane == gogo::gog::content::Lane::Depot
}

/// `dlc list` (design 19 §1.3). Reads only.
async fn list(game: Game, json: bool) -> Result<i32, AppError> {
    let (ctx, token) = connect().await?;
    let id = game_id(&game);
    let detail = gogo::gog::catalog::detail(&ctx.http, &token, id).await?;
    let owned = library::owned_ids(&ctx.http, &token).await?;
    let language = language_of(&game);
    let mut notes = Vec::new();
    let manifest_dlcs = if is_depot(&game) {
        let ib = depot::installed_build(core(&ctx)?, id, game.content.build_id.as_deref()).await?;
        match (ib.listed, ib.newest) {
            (Some((_, _, m)), _) => Some(dlc::manifest_dlcs(&m, &language)),
            (None, Some((newest, m))) => {
                notes.push(format!(
                    "build {} isn't served by GOG any more: `in this build` and the sizes \
                     are build {newest}'s, and adding DLC needs an update first ({})",
                    game.content.build_id.as_deref().unwrap_or("unknown"),
                    add::update_hint(&game.gogid, &game.installed())
                ));
                Some(dlc::manifest_dlcs(&m, &language))
            }
            (None, None) => None,
        }
    } else {
        notes.push(
            "DLC is added to offline-installer games with their installers, which gogo \
             doesn't do yet"
                .into(),
        );
        None
    };
    if game.derived {
        notes.push(format!(
            "no {}: the installed DLC are read from the game's goggame-*.info files",
            gogo::gog::content::FILE
        ));
    }
    if game.content.language.is_none() {
        notes.push(format!(
            "no language recorded: sizes are for {}",
            add::DEFAULT_LANGUAGE
        ));
    }
    let infos = dlc::available(&detail.expanded_dlcs, &owned, manifest_dlcs.as_deref());
    let missing = infos
        .iter()
        .filter(|d| d.owned && d.in_build && !game.installed().contains(&d.id))
        .count();
    if missing > 0 && is_depot(&game) {
        notes.push(format!(
            "{missing} owned DLC not installed: gogo linux-app dlc {} add <id|title> (or --all)",
            game.gogid
        ));
    }
    let l = add::listing(&game, &infos, &owned, notes);
    if json {
        println!("{}", serde_json::to_string(&l).unwrap());
    } else {
        print!("{}", add::render_list(&l));
    }
    Ok(cli::exit::OK)
}

fn interactive(machine: bool) -> bool {
    !machine
        && std::io::IsTerminal::is_terminal(&std::io::stdin())
        && std::io::IsTerminal::is_terminal(&std::io::stderr())
}

/// The progress events of an add: NDJSON under `--json-progress`, else `gogo:` lines.
/// The human lines go to `print`: stderr, or the live renderer, which prints
/// them above its status line.
fn emit(ev: &ProgressEvent, json_progress: bool, quiet: bool, print: &dyn Fn(&str)) {
    if json_progress {
        if let Ok(line) = serde_json::to_string(ev) {
            println!("{line}");
        }
        return;
    }
    if quiet {
        return;
    }
    let size = gogo::fmt::human_size;
    let line = match ev {
        ProgressEvent::GameFilesStarted {
            bytes, dir, files, ..
        } => Some(format!(
            "game files: adding {} file(s) ({}) to {}",
            files.unwrap_or(0),
            size(*bytes),
            dir.display()
        )),
        ProgressEvent::GameFilesProgress { copied, total } => Some(format!(
            "game files: copied {} / {}",
            size(*copied),
            size(*total)
        )),
        ProgressEvent::DlcRecovered { action } => Some(
            match action.as_str() {
                "forward" => "an earlier DLC add had stopped while laying its files: finished it",
                _ => {
                    "an earlier DLC add had stopped and its files were gone: undid it, the game \
                  is as it was"
                }
            }
            .into(),
        ),
        ProgressEvent::Warn { message } => Some(format!("note: {message}")),
        _ => None,
    };
    if let Some(l) = line {
        print(&format!("gogo: {l}"));
    }
}

/// `dlc add` (design 19 §1.4, §3.2).
async fn add_cmd(
    mut game: Game,
    a: AddArgs,
    cache_base: Option<std::path::PathBuf>,
) -> Result<i32, AppError> {
    let machine = a.json || a.json_progress;
    let (quiet, _) = crate::PROGRESS_FLAGS.get().copied().unwrap_or_default();
    let quiet = quiet || a.json;
    let jp = a.json_progress;
    if !is_depot(&game) {
        eprintln!(
            "gogo: {} is an offline-installer game: adding DLC there runs the DLC's \
             installers, which gogo doesn't do yet",
            game.gogid
        );
        return Ok(cli::exit::USAGE);
    }
    // A half-laid add is settled first, whatever was asked (design 19 §3.4).
    let recovered = if a.dry_run {
        None
    } else {
        // No live renderer yet: the lines go straight to stderr.
        match add::recover(&mut game, gogo::gog::materialize::Reflink::Try, &mut |ev| {
            emit(ev, jp, quiet, &|l| eprintln!("{l}"))
        }) {
            Ok(r) => r,
            Err(e) => return Ok(report(e)),
        }
    };

    let (ctx, token) = connect().await?;
    let session = core(&ctx)?;
    let id = game_id(&game);
    let title = title_of(&game);
    let detail = gogo::gog::catalog::detail(&ctx.http, &token, id).await?;
    let owned = library::owned_ids(&ctx.http, &token).await?;
    let language = language_of(&game);
    let build_id = game.content.build_id.clone();
    let ib = depot::installed_build(session, id, build_id.as_deref()).await?;
    let installed = game.installed();

    let Some((version, generation, manifest)) = ib.listed else {
        // Name what was asked in the update hint, where the catalog can tell.
        let infos = dlc::available(&detail.expanded_dlcs, &owned, None);
        let asked =
            dlc::resolve(&a.names, &title, &infos, &dlc::Lane::Installer).unwrap_or_default();
        let mut all = installed.clone();
        all.extend(asked.into_iter().filter(|i| !installed.contains(i)));
        let why =
            add::refusal(&ib.matched, build_id.as_deref(), &game.gogid, &all).unwrap_or_default();
        eprintln!("gogo: {why}");
        return Ok(cli::exit::DEPOT);
    };
    if generation == 1 {
        eprintln!(
            "gogo: build {} is a generation 1 build, which gogo adds DLC to only with a \
             rebuild: {}",
            build_id.as_deref().unwrap_or("?"),
            add::update_hint(&game.gogid, &installed)
        );
        return Ok(cli::exit::DEPOT);
    }
    let build = build_id.clone().unwrap_or_default();
    let mdlcs = dlc::manifest_dlcs(&manifest, &language);
    let infos = dlc::available(&detail.expanded_dlcs, &owned, Some(&mdlcs));
    let lane = dlc::Lane::Depot {
        build_id: build.clone(),
    };
    let addable: Vec<u64> = infos
        .iter()
        .filter(|d| d.owned && d.in_build && !installed.contains(&d.id))
        .map(|d| d.id)
        .collect();
    let wanted: Vec<u64> = if a.all {
        addable.clone()
    } else if !a.names.is_empty() {
        match dlc::resolve(&a.names, &title, &infos, &lane) {
            Ok(ids) => ids,
            Err(e) => {
                eprintln!("gogo: {}", with_update_hint(&e, &game, &installed));
                return Ok(cli::exit::USAGE);
            }
        }
    } else if recovered.is_some() {
        // A bare rerun after a stop: the recovery was the job.
        Vec::new()
    } else if interactive(machine) && !addable.is_empty() {
        crate::pick_dlc(&addable, &infos)?
    } else {
        eprintln!(
            "gogo: name the DLC to add (id or title), or --all; DLC {title} can take: {}",
            names_of(&addable, &infos)
        );
        return Ok(cli::exit::USAGE);
    };

    let newer = match &ib.matched {
        BuildMatch::Older {
            newest_id,
            newest_version,
        } => Some(add::NewerBuild {
            build_id: newest_id.clone(),
            version: newest_version.clone(),
        }),
        _ => None,
    };
    let plan = add::plan_add(&add::PlanInput {
        game: &game,
        title: &title,
        version: Some(version),
        newer,
        tree_name: manifest.install_directory().to_string(),
        infos: &infos,
        wanted: &wanted,
    });
    if !machine && !quiet {
        for l in add::plan_lines(&plan) {
            eprintln!("gogo: {l}");
        }
    }
    if a.dry_run {
        if a.json {
            println!("{}", serde_json::json!({ "dryRun": true, "plan": plan }));
        }
        return Ok(cli::exit::OK);
    }
    if plan.dlcs.is_empty() {
        if a.json {
            println!(
                "{}",
                serde_json::json!({
                    "gogid": game.gogid,
                    "buildId": game.content.build_id,
                    "language": game.content.language,
                    "dlcs": game.content.refs(),
                    "added": [],
                    "recovered": recovered,
                })
            );
        } else if !quiet && recovered.is_none() {
            eprintln!("gogo: nothing to add");
        }
        return Ok(cli::exit::OK);
    }
    if !a.yes {
        if !interactive(machine) {
            eprintln!("gogo: adding DLC needs consent: pass --yes");
            return Ok(cli::exit::USAGE);
        }
        let go = dialoguer::Confirm::new()
            .with_prompt("add it now?")
            .default(true)
            .interact()
            .unwrap_or(false);
        if !go {
            eprintln!("gogo: aborted");
            return Ok(cli::exit::USAGE);
        }
    }
    // A stale owned set can disagree with the server (design 19 §2.2): asked before
    // any file is fetched; only the verdict is read.
    let fetching: Vec<u64> = plan
        .dlcs
        .iter()
        .filter(|d| d.download_size > 0)
        .filter_map(|d| d.id.parse().ok())
        .collect();
    if let Some(bad) = depot::first_unowned(session, &fetching, generation, &manifest).await? {
        let mut stale = infos.clone();
        for d in stale.iter_mut().filter(|d| d.id == bad) {
            d.owned = false;
        }
        if let Err(e) = dlc::check_id(bad, &title, &stale, &lane) {
            eprintln!("gogo: {e}");
        }
        return Ok(cli::exit::USAGE);
    }

    let Some((_, _, deps_root)) = depot::cache_roots(cache_base.as_deref(), id, None) else {
        return Err(AppError::Depot(depot::DepotError::NoCacheDir));
    };
    let cancel = ctx.cancel.clone();
    {
        let c = cancel.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            c.cancel();
        });
    }
    let mode = depot_mode(a.json_progress, a.json);
    if plan.downloads() && matches!(mode, depot_progress::Mode::Human { .. }) {
        eprintln!(
            "gogo: downloading {} for {title} (build {build})",
            plan.downloading()
                .iter()
                .map(|d| d.title.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<DepotEvent>();
    let free_space = ctx.free_space.clone();
    let fetch = async |inputs: depot::DownloadInputs, root: std::path::PathBuf| {
        let (files, bytes) = depot::fetch_into(
            session,
            &depot::FetchSpec {
                manifest: &manifest,
                build_id: &build,
                inputs: &inputs,
                install_root: &root,
                support_root: &root.join("gog-support"),
                deps_root: &deps_root,
                limit: a.limit,
                disk_check: a.disk_check,
            },
            &tx,
            &cancel,
            free_space.as_ref(),
        )
        .await?;
        let _ = tx.send(DepotEvent::Finished {
            install_dir: root.join(manifest.install_directory()),
            fetched_files: files,
            fetched_bytes: bytes,
        });
        Ok(())
    };
    let stop_token = cancel.clone();
    let stop = move || stop_token.is_cancelled();
    let mut renderer = mode.renderer();
    let live = depot_progress::Live::new(&mut rx, &mut renderer);
    let mut on_event = |ev: &ProgressEvent| emit(ev, jp, quiet, &|l| live.note(l));
    let opts = add::AddOptions::default();
    let runner = add::add(&mut game, &plan, fetch, &opts, &mut on_event, &stop);
    let result = live.drive(runner).await;
    match result {
        Ok(out) => {
            if a.json {
                println!("{}", serde_json::to_string(&out).unwrap());
            } else if !quiet {
                for w in &out.warnings {
                    eprintln!("gogo: warning: {w}");
                }
                eprintln!(
                    "gogo: added {} to {title} (build {build}); its DLC now: {}",
                    out.added
                        .iter()
                        .map(|d| d.title.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    out.dlcs
                        .iter()
                        .map(|d| d.title.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            Ok(cli::exit::OK)
        }
        Err(e) => Ok(report(e)),
    }
}

/// Print an add's failure; its exit code (design 19 §7).
fn report(e: AddError) -> i32 {
    match e {
        AddError::Interrupted(..) => {
            eprintln!("gogo: {e}");
            cli::exit::SIGINT
        }
        AddError::Depot(depot::DepotError::Dlc(e)) => {
            eprintln!("gogo: {e}");
            cli::exit::USAGE
        }
        // Debug for the depot: gog-warp's io variants hide their source in Display.
        AddError::Depot(e) => {
            eprintln!("gogo: depot: {e:?}");
            cli::exit::DEPOT
        }
        AddError::Game(e) => {
            eprintln!("gogo: {e}");
            cli::exit::MACAPP
        }
        AddError::Io(e) => {
            eprintln!("gogo: {e}");
            cli::exit::FAILED
        }
    }
}

fn names_of(ids: &[u64], infos: &[DlcInfo]) -> String {
    if ids.is_empty() {
        return "none (every owned DLC of this build is installed)".into();
    }
    ids.iter()
        .map(|id| {
            infos
                .iter()
                .find(|d| d.id == *id)
                .map_or(id.to_string(), |d| format!("{id} {}", d.title))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A DLC not in the installed build also names the update path (design 19 §4).
fn with_update_hint(e: &dlc::DlcSelectError, game: &Game, installed: &[u64]) -> String {
    match e {
        dlc::DlcSelectError::NotInBuild { id, .. } => {
            let mut all = installed.to_vec();
            all.push(*id);
            format!(
                "{e}; to update the game and take it: {}",
                add::update_hint(&game.gogid, &all)
            )
        }
        _ => e.to_string(),
    }
}
