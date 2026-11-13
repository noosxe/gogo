//! `gogo linux-app dlc <gogid> list|add|remove` (design 19 §1.3, §1.4, §3.7, §4, §7):
//! the network side and the output; the work is `gogo::linuxapp::dlc` and
//! `gogo::linuxapp::dlc_remove`.

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
            no_snapshots,
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
                    no_snapshots,
                    json_progress,
                    json,
                },
                cache_base,
            )
            .await
        }
        cli::LinuxDlcCmd::Remove {
            names,
            dry_run,
            yes,
            limit,
            no_disk_check,
            no_snapshots,
            json_progress,
            json,
        } => {
            let limit = match &limit {
                Some(spec) => Some(gogo::config::parse_rate(spec)?),
                None => None,
            };
            remove_cmd(
                game,
                AddArgs {
                    names,
                    all: false,
                    dry_run,
                    yes,
                    limit,
                    disk_check: !no_disk_check,
                    no_snapshots,
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
    no_snapshots: bool,
    json_progress: bool,
    json: bool,
}

async fn build_ctx() -> Result<Arc<AppContext>, AppError> {
    let built = AppContext::build(crate::load_config_from_env()?, true)
        .await
        .map_err(|e| {
            eprintln!("gogo: {e}");
            AppError::Usage
        })?;
    Ok(Arc::new(built))
}

/// [`connect`] with the token refresh on a spinner (#422). The context is
/// built first, outside it, because a failure there prints its own line.
pub(crate) async fn connect_phase(machine: bool) -> Result<(Arc<AppContext>, String), AppError> {
    let ctx = build_ctx().await?;
    let token = crate::phase(machine, "refreshing the sign-in", crate::bearer(&ctx)).await?;
    Ok((ctx, token))
}

pub(crate) async fn connect() -> Result<(Arc<AppContext>, String), AppError> {
    let ctx = build_ctx().await?;
    let token = crate::bearer(&ctx).await?;
    Ok((ctx, token))
}

pub(crate) fn core(ctx: &AppContext) -> Result<&gog_warp::Core, AppError> {
    ctx.session
        .as_ref()
        .and_then(|p| p.gog_warp_core())
        .ok_or(AppError::Usage)
}

fn game_id(game: &Game) -> u64 {
    game.gogid.parse().unwrap_or(0)
}

pub(crate) fn title_of(game: &Game) -> String {
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
    let mut build_langs = None;
    let manifest_dlcs = if is_depot(&game) {
        let ib = depot::installed_build(core(&ctx)?, id, game.content.build_id.as_deref()).await?;
        match (ib.listed, ib.newest) {
            (Some((_, _, m)), _) => {
                build_langs = Some(gogo::gog::language::of_manifest(&m));
                Some(dlc::manifest_dlcs(&m, &language))
            }
            (None, Some((newest, m))) => {
                notes.push(format!(
                    "build {} isn't served by GOG any more: `in this build` and the sizes \
                     are build {newest}'s, and adding DLC needs an update first ({})",
                    game.content.build_id.as_deref().unwrap_or("unknown"),
                    add::update_hint(&game.gogid, &game.installed())
                ));
                build_langs = Some(gogo::gog::language::of_manifest(&m));
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
    if let Some(b) = &build_langs {
        notes.extend(b.notes_for(&language, &|id| {
            infos
                .iter()
                .find(|i| i.id.to_string() == id)
                .map(|i| i.title.clone())
        }));
    }
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

pub(crate) fn interactive(machine: bool) -> bool {
    !machine
        && std::io::IsTerminal::is_terminal(&std::io::stdin())
        && std::io::IsTerminal::is_terminal(&std::io::stderr())
}

/// The progress events of an add: NDJSON under `--json-progress`, else `gogo:` lines.
/// The human lines go to `print`: stderr, or the live renderer, which prints
/// them above its status line.
fn emit(ev: &ProgressEvent, json_progress: bool, quiet: bool, print: &dyn Fn(&str)) {
    if ev.is_live_only() {
        return;
    }
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
        ProgressEvent::SnapshotCreated { id, label, .. } => {
            Some(format!("snapshot {id} taken ({label})"))
        }
        ProgressEvent::SnapshotPruned { id, label, created } => {
            Some(gogo::linuxapp::snapshots::pruned_line(*id, label, created))
        }
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

    // One spinner per phase (#422): nothing off a terminal or in a machine mode.
    let (ctx, token) = connect_phase(machine).await?;
    let session = core(&ctx)?;
    let id = game_id(&game);
    let title = title_of(&game);
    let (detail, owned) = crate::phase(machine, "checking the game's DLC", async {
        let detail = gogo::gog::catalog::detail(&ctx.http, &token, id).await?;
        let owned = library::owned_ids(&ctx.http, &token).await?;
        Ok::<_, AppError>((detail, owned))
    })
    .await?;
    let language = language_of(&game);
    let build_id = game.content.build_id.clone();
    let ib = crate::phase(
        machine,
        "fetching the builds list",
        depot::installed_build(session, id, build_id.as_deref()),
    )
    .await?;
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
    // A cache record from before DLC tracking, of this build (#374): what it would
    // become, read from the build's depot manifests. `add` writes it.
    let cache_upgrade = match &game.cache_root {
        Some(root) => {
            crate::phase(
                machine,
                "checking the depot cache",
                gogo::gog::cache_upgrade::check(
                    session.reqwest_client(),
                    root,
                    &manifest,
                    &build,
                    &language,
                ),
            )
            .await
        }
        None => None,
    };
    let plan = add::plan_add(&add::PlanInput {
        game: &game,
        title: &title,
        version: Some(version),
        newer,
        tree_name: manifest.install_directory().to_string(),
        infos: &infos,
        wanted: &wanted,
        cache_upgrade: cache_upgrade.as_ref(),
    });
    if !machine && !quiet {
        for l in add::plan_lines(&plan) {
            eprintln!("gogo: {l}");
        }
        for n in plan.upgrade.iter().flat_map(|u| &u.notes) {
            eprintln!("gogo: depot cache: {n}");
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
        let go = crate::depot_progress::prompting(|| {
            dialoguer::Confirm::new()
                .with_prompt("add it now?")
                .default(true)
                .interact()
        })
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
    let unowned = crate::phase(
        machine,
        "checking the DLC is owned",
        depot::first_unowned(session, &fetching, generation, &manifest),
    )
    .await?;
    if let Some(bad) = unowned {
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
                old_manifest: None,
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
    let act = depot_progress::Activity::new(mode);
    let live = depot_progress::Live::new(&mut rx, &act);
    // The game files' running count goes on the live status line (#420).
    let mut on_event = |ev: &ProgressEvent| {
        if !live.work(ev) {
            emit(ev, jp, quiet, &|l| live.note(l))
        }
    };
    let opts = add::AddOptions {
        snapshots: crate::auto_snapshots(a.no_snapshots)?,
        ..Default::default()
    };
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
pub(crate) fn report(e: AddError) -> i32 {
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

/// The picker of a bare `remove` on a terminal: the installed DLC, none chosen.
fn pick_installed(installed: &[gogo::gog::content::DlcRef]) -> Result<Vec<u64>, AppError> {
    let items: Vec<String> = installed
        .iter()
        .map(|d| format!("{} ({})", d.title, d.id))
        .collect();
    let picked = crate::depot_progress::prompting(|| {
        dialoguer::MultiSelect::new()
            .with_prompt("DLC to remove (space toggles, enter accepts)")
            .items(&items)
            .interact_opt()
    });
    match picked {
        Ok(Some(sel)) => Ok(sel.into_iter().map(|i| installed[i].id).collect()),
        _ => {
            eprintln!("gogo: aborted");
            Err(AppError::Usage)
        }
    }
}

fn installed_names(installed: &[gogo::gog::content::DlcRef]) -> String {
    if installed.is_empty() {
        return "none".into();
    }
    installed
        .iter()
        .map(|d| format!("{} {}", d.id, d.title))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `dlc remove` (design 19 §3.7).
async fn remove_cmd(
    mut game: Game,
    a: AddArgs,
    cache_base: Option<std::path::PathBuf>,
) -> Result<i32, AppError> {
    use gogo::linuxapp::dlc_remove as rm;
    let machine = a.json || a.json_progress;
    let (quiet, _) = crate::PROGRESS_FLAGS.get().copied().unwrap_or_default();
    let quiet = quiet || a.json;
    let jp = a.json_progress;
    if !is_depot(&game) {
        eprintln!(
            "gogo: {} is an offline-installer game: removing DLC there runs the DLC's own \
             uninstaller, which gogo doesn't do yet; a rebuild with --dlc naming the DLC to \
             keep (or --no-dlcs) leaves it out and keeps the prefix and saves",
            game.gogid
        );
        return Ok(cli::exit::MACAPP);
    }
    // A half-done add, update or removal is settled first, whatever was asked.
    let recovered = if a.dry_run {
        None
    } else {
        match add::recover(&mut game, gogo::gog::materialize::Reflink::Try, &mut |ev| {
            emit(ev, jp, quiet, &|l| eprintln!("{l}"))
        }) {
            Ok(r) => r,
            Err(e) => return Ok(report(e)),
        }
    };
    let installed = game.content.refs();
    let wanted: Vec<u64> = if !a.names.is_empty() {
        // A name that is only part of a title counts only when the question
        // below will be asked to confirm it.
        let asks = !a.yes && interactive(machine);
        let named = rm::resolve_installed(&a.names, &installed, asks);
        if !named.ambiguous.is_empty() || !named.inexact.is_empty() {
            for (n, titles) in &named.ambiguous {
                eprintln!(
                    "gogo: \"{n}\" is ambiguous: matches {}; name one by id or full title",
                    titles.join(", ")
                );
            }
            for (n, title) in &named.inexact {
                eprintln!(
                    "gogo: no installed DLC is titled \"{n}\"; did you mean \"{title}\"? \
                     Without the question (--yes, or no terminal), name it by id or full title"
                );
            }
            return Ok(cli::exit::USAGE);
        }
        if !named.unmatched.is_empty() && !quiet {
            eprintln!(
                "gogo: not installed, nothing to remove: {} (installed: {})",
                named.unmatched.join(", "),
                installed_names(&installed)
            );
        }
        if named.ids.is_empty() && recovered.is_none() && !named.unmatched.is_empty() {
            // Only a name that matches nothing: likely a typo, so not a success.
            let typo = named
                .unmatched
                .iter()
                .any(|n| n.trim().parse::<u64>().is_err());
            if typo {
                return Ok(cli::exit::USAGE);
            }
        }
        named.ids
    } else if recovered.is_some() {
        // A bare rerun after a stop: the recovery was the job.
        Vec::new()
    } else if interactive(machine) && !installed.is_empty() {
        pick_installed(&installed)?
    } else {
        eprintln!(
            "gogo: name the DLC to remove (id or title); installed: {}",
            installed_names(&installed)
        );
        return Ok(cli::exit::USAGE);
    };
    if wanted.is_empty() {
        if a.json {
            println!(
                "{}",
                serde_json::json!({
                    "gogid": game.gogid,
                    "buildId": game.content.build_id,
                    "language": game.content.language,
                    "dlcs": game.content.refs(),
                    "removed": [],
                    "recovered": recovered,
                })
            );
        } else if !quiet && recovered.is_none() {
            eprintln!("gogo: nothing to remove");
        }
        return Ok(cli::exit::OK);
    }

    // The depot manifests need the session; the catalog isn't read.
    let (ctx, _token) = connect_phase(machine).await?;
    let session = core(&ctx)?;
    let id = game_id(&game);
    let title = title_of(&game);
    let language = language_of(&game);
    let Some(build) = game.content.build_id.clone() else {
        eprintln!(
            "gogo: the game's build isn't known (no build in {} or its goggame info), so \
             the DLC's files can't be told apart; a rebuild without it: {}",
            gogo::gog::content::FILE,
            rm::rebuild_hint(&game, &wanted)
        );
        return Ok(cli::exit::MACAPP);
    };
    // The installed build's manifest: kept in the game dir, else GOG's listing.
    let kept = gogo::gog::build_manifest::load(&game.dirs.root).filter(|s| s.build_id == build);
    let (version, generation, manifest) = match kept {
        Some(s) => (game.content.version.clone(), s.generation, s.manifest),
        None => {
            let ib = crate::phase(
                machine,
                "fetching the builds list",
                depot::installed_build(session, id, Some(&build)),
            )
            .await?;
            match ib.listed {
                Some((v, g, m)) => (Some(v), g, m),
                None => {
                    eprintln!(
                        "gogo: build {build} isn't served by GOG any more and the game has no \
                         copy of its manifest, so the DLC's files can't be told apart from \
                         the base game's; a rebuild without it: {}",
                        rm::rebuild_hint(&game, &wanted)
                    );
                    return Ok(cli::exit::MACAPP);
                }
            }
        }
    };
    let mut products = vec![game.gogid.clone()];
    products.extend(game.installed().iter().map(u64::to_string));
    let langs = vec![language.clone()];
    let depots = match crate::phase(
        machine,
        "reading the depot manifests",
        gogo::gog::dlc_filter::fetch_depots(
            session.reqwest_client(),
            &manifest,
            Some((&products, &langs)),
        ),
    )
    .await
    {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "gogo: the build's depot manifests can't be read ({e}), and without them the \
                 DLC's files can't be checked before they come out; nothing was changed. Try \
                 again when GOG can be reached, or rebuild without it: {}",
                rm::rebuild_hint(&game, &wanted)
            );
            return Ok(cli::exit::MACAPP);
        }
    };
    let snapshots = crate::auto_snapshots(a.no_snapshots)?;
    let autosnapshot = gogo::linuxapp::snapshots::auto_skipped(&game.gogid, &game.dirs, &snapshots)
        .is_ok_and(|s| s.is_none());
    let plan = match rm::plan_remove(&rm::PlanInput {
        game: &game,
        title: &title,
        version,
        generation,
        tree_name: manifest.install_directory().to_string(),
        depots: &depots,
        removing: &wanted,
        autosnapshot,
    }) {
        Ok(p) => p,
        Err(why) => {
            eprintln!("gogo: {why}");
            return Ok(cli::exit::MACAPP);
        }
    };
    if !machine && !quiet {
        for l in rm::plan_lines(&plan) {
            eprintln!("gogo: {l}");
        }
    }
    if a.dry_run {
        if a.json {
            println!("{}", serde_json::json!({ "dryRun": true, "plan": plan }));
        }
        return Ok(cli::exit::OK);
    }
    if !a.yes {
        if !interactive(machine) {
            eprintln!("gogo: removing DLC needs consent: pass --yes");
            return Ok(cli::exit::USAGE);
        }
        let prompt = format!("{}. Remove it now?", rm::saves_warning(&plan));
        let go = crate::depot_progress::prompting(|| {
            dialoguer::Confirm::new()
                .with_prompt(prompt.clone())
                .default(false)
                .interact()
        })
        .unwrap_or(false);
        if !go {
            eprintln!("gogo: aborted");
            return Ok(cli::exit::USAGE);
        }
    }

    let deps_root = depot::cache_roots(cache_base.as_deref(), id, None).map(|r| r.2);
    if plan.downloads() && deps_root.is_none() {
        return Err(AppError::Depot(depot::DepotError::NoCacheDir));
    }
    let deps_root = deps_root.unwrap_or_default();
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
        eprintln!("gogo: downloading the base files the DLC replaced, for {title} (build {build})");
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
                old_manifest: None,
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
    let act = depot_progress::Activity::new(mode);
    let live = depot_progress::Live::new(&mut rx, &act);
    let mut on_event = |ev: &ProgressEvent| {
        if !live.work(ev) {
            emit_remove(ev, jp, quiet, &|l| live.note(l))
        }
    };
    let opts = add::AddOptions {
        snapshots,
        ..Default::default()
    };
    let runner = rm::remove(&mut game, &plan, fetch, &opts, &mut on_event, &stop);
    let result = live.drive(runner).await;
    match result {
        Ok(out) => {
            if a.json {
                println!("{}", serde_json::to_string(&out).unwrap());
            } else if !quiet {
                for w in &out.warnings {
                    eprintln!("gogo: warning: {w}");
                }
                let now = if out.dlcs.is_empty() {
                    "none".to_string()
                } else {
                    out.dlcs
                        .iter()
                        .map(|d| d.title.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                if out.removed.is_empty() {
                    eprintln!("gogo: nothing to remove; {title}'s DLC: {now}");
                } else {
                    eprintln!(
                        "gogo: removed {} from {title} (build {build}: {} file(s) out, {} \
                         restored); its DLC now: {now}",
                        out.removed
                            .iter()
                            .map(|d| d.title.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                        out.files_removed,
                        out.files_restored
                    );
                }
            }
            Ok(cli::exit::OK)
        }
        Err(e) => Ok(report(e)),
    }
}

/// [`emit`], with a removal's words for its lay.
fn emit_remove(ev: &ProgressEvent, json_progress: bool, quiet: bool, print: &dyn Fn(&str)) {
    if let ProgressEvent::GameFilesStarted {
        bytes, dir, files, ..
    } = ev
        && !json_progress
    {
        if !quiet {
            print(&format!(
                "gogo: game files: taking the DLC out of {} ({} file(s) restored, {})",
                dir.display(),
                files.unwrap_or(0),
                gogo::fmt::human_size(*bytes)
            ));
        }
        return;
    }
    emit(ev, json_progress, quiet, print)
}
