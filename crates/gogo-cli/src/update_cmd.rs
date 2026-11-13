//! `gogo linux-app update <gogid>` (design 21 §7, §11g): the network side and the
//! output; the work is `gogo::linuxapp::update`.

use gog_warp::content_system::types::{Build, Manifest, Platform};
use gogo::engine::events::ProgressEvent;
use gogo::gog::builds::{self, ResolveCtx};
use gogo::gog::depot::{self, DepotError, DepotEvent, DepotOptions, DlcContext};
use gogo::gog::dlc::{self, DlcSet};
use gogo::gog::dlc_filter::DepotList;
use gogo::gog::{build_manifest, content, library};
use gogo::linuxapp::dlc as add;
use gogo::linuxapp::update::{self, BuildSide, Planned};

use crate::dlc_cmd::{core, interactive, report, title_of};
use crate::{AppError, cli, depot_mode, depot_progress};

/// The flags of `linux-app update`.
pub struct Args {
    pub build: Option<String>,
    pub dry_run: bool,
    pub yes: bool,
    pub limit: Option<String>,
    pub disk_check: bool,
    pub no_snapshots: bool,
    pub json_progress: bool,
    pub json: bool,
}

/// The progress events of an update: NDJSON under `--json-progress`, else `gogo:`
/// lines through `print`.
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
            "game files: laying {} changed or new file(s) ({}) onto {}",
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

fn side(b: &Build) -> BuildSide {
    BuildSide {
        build_id: b.build_id().clone(),
        version: Some(b.version_name().clone()).filter(|v| !v.trim().is_empty()),
        branch: b.branch().clone(),
        generation: Some(*b.generation()),
    }
}

/// Dispatch `gogo linux-app update <gogid>`.
pub async fn run(gogid: String, a: Args) -> Result<i32, AppError> {
    gogo::macapp::compat::validate_gogid(&gogid)?;
    let machine = a.json || a.json_progress;
    let (quiet, _) = crate::PROGRESS_FLAGS.get().copied().unwrap_or_default();
    let quiet = quiet || a.json;
    let jp = a.json_progress;
    let limit = match &a.limit {
        Some(spec) => Some(gogo::config::parse_rate(spec)?),
        None => None,
    };
    let cache_base = depot::default_cache_dir();
    let mut game = add::load_game(&gogid, cache_base.as_deref())?;
    if game.content.lane == content::Lane::Installer {
        eprintln!("gogo: {}", update::installer_refusal(&gogid));
        return Ok(cli::exit::DEPOT);
    }
    // A half-laid add or update is settled first, whatever was asked (§11g).
    if !a.dry_run
        && let Err(e) = add::recover(&mut game, gogo::gog::materialize::Reflink::Try, &mut |ev| {
            emit(ev, jp, quiet, &|l| eprintln!("{l}"))
        })
    {
        return Ok(report(e));
    }

    // One spinner per phase (#422): nothing off a terminal or in a machine mode.
    let (ctx, token) = crate::dlc_cmd::connect_phase(machine).await?;
    let session = core(&ctx)?;
    let id: u64 = gogid.parse().unwrap_or(0);
    let title = title_of(&game);
    let language = update::language_of(&game);
    let listing = crate::phase(
        machine,
        "fetching the builds list",
        builds::fetch_listing(session, id, Platform::Windows, None),
    )
    .await
    .map_err(DepotError::library)?;
    let installed_id = game.content.build_id.clone().unwrap_or_default();
    let installed = listing
        .builds
        .iter()
        .find(|b| *b.build_id() == installed_id);
    // A download record another installed build left (a snapshot rollback since) is
    // stale: discarded, and the update plans afresh.
    if !a.dry_run
        && let Some(note) = update::discard_stale(&game.dirs.root, &installed_id)
        && !quiet
    {
        eprintln!("gogo: note: {note}");
    }
    let pending = update::read_download(&game.dirs.root).filter(|p| p.from == installed_id);

    // The target: `--build`, else a stopped update's, else the newest on the
    // game's branch (§11 Q2).
    let target: &Build = match (&a.build, &pending) {
        (Some(v), _) => match builds::pick_for_run(
            &listing,
            v,
            &ResolveCtx {
                title: &title,
                installed: Some(&installed_id),
            },
        ) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("gogo: {e}");
                return Ok(cli::exit::USAGE);
            }
        },
        (None, Some(p)) => match listing
            .builds
            .iter()
            .find(|b| *b.build_id() == p.to.build_id)
        {
            Some(b) => b,
            None => {
                eprintln!(
                    "gogo: an update of {gogid} to build {} stopped during its download, and \
                     GOG no longer lists that build; update with a rebuild instead: {}",
                    p.to.build_id,
                    update::rebuild_hint(&gogid, "<build>")
                );
                return Ok(cli::exit::DEPOT);
            }
        },
        (None, None) => {
            let branch = game
                .content
                .branch
                .clone()
                .or_else(|| installed.and_then(|b| b.branch().clone()));
            match builds::newest_on_branch(&listing.builds, branch.as_deref()) {
                Some(b) => b,
                None => {
                    eprintln!(
                        "gogo: GOG lists no public build of {title} on {}",
                        branch.map_or("the default branch".into(), |b| format!("branch {b}"))
                    );
                    return Ok(cli::exit::DEPOT);
                }
            }
        }
    };
    let to = side(target);
    if to.build_id == installed_id {
        if a.json {
            println!(
                "{}",
                serde_json::json!({ "gogid": gogid, "upToDate": true, "build": to })
            );
        } else if !quiet {
            eprintln!(
                "gogo: {title} is up to date (build {}, {})",
                to.build_id,
                to.label()
            );
        }
        return Ok(cli::exit::OK);
    }
    let target_manifest = crate::phase(
        machine,
        "reading the build's manifest",
        session.get_manifest(target),
    )
    .await
    .map_err(DepotError::library)?;

    // The installed build's manifest: kept in the game dir, else GOG's listing.
    let kept = build_manifest::load(&game.dirs.root).filter(|s| s.build_id == installed_id);
    let (from_generation, from_manifest): (Option<u32>, Option<Manifest>) = match (&kept, installed)
    {
        (Some(s), _) => (Some(s.generation), Some(s.manifest.clone())),
        (None, Some(b)) => (
            Some(*b.generation()),
            if *b.generation() == 2 {
                Some(
                    crate::phase(
                        machine,
                        "reading the installed build's manifest",
                        session.get_manifest(b),
                    )
                    .await
                    .map_err(DepotError::library)?,
                )
            } else {
                None
            },
        ),
        (None, None) => (None, None),
    };
    let from_version = installed
        .map(|b| b.version_name().clone())
        .filter(|v| !v.trim().is_empty());
    let same_install_dir = from_manifest
        .as_ref()
        .is_none_or(|m| depot::same_install_dir(m, &target_manifest));
    let target_dlcs: Vec<u64> = dlc::manifest_dlcs(&target_manifest, &language)
        .iter()
        .map(|d| d.id)
        .collect();
    let language_missing = depot::pick_language(&target_manifest, &language, true)
        .err()
        .map(|_| {
            gogo::gog::language::of_manifest(&target_manifest)
                .codes()
                .join(", ")
        });
    let downgrade = installed.is_some_and(|i| {
        (target.date_published(), target.build_id()) < (i.date_published(), i.build_id())
    });
    // The installed DLC the target has stay; the others the update takes out (§11h).
    let installed_ids: Vec<String> = game.installed().iter().map(u64::to_string).collect();
    let (kept_ids, dropping): (Vec<String>, Vec<String>) = installed_ids
        .iter()
        .cloned()
        .partition(|d| target_dlcs.iter().any(|t| t.to_string() == *d));
    let (download_size, disk_size) = target_manifest.install_size(&language, &kept_ids, None);

    // What the target drops, from both builds' depot manifests (§11g step 1): the
    // installed build with every installed DLC, the target with the ones that stay.
    let mut notes = Vec::new();
    let mut dropped = Vec::new();
    let mut lists: Option<(Vec<DepotList>, Vec<DepotList>)> = None;
    if let Some(old) = &from_manifest
        && language_missing.is_none()
        && same_install_dir
    {
        let langs = vec![language.clone()];
        let products = |m: &Manifest, dlcs: &[String]| {
            let mut p = vec![m.product_id()];
            p.extend(dlcs.iter().cloned());
            p
        };
        let http = session.reqwest_client();
        let (op, np) = (
            products(old, &installed_ids),
            products(&target_manifest, &kept_ids),
        );
        match crate::phase(machine, "reading the depot manifests", async {
            (
                gogo::gog::dlc_filter::fetch_depots(http, old, Some((&op, &langs))).await,
                gogo::gog::dlc_filter::fetch_depots(http, &target_manifest, Some((&np, &langs)))
                    .await,
            )
        })
        .await
        {
            (Ok(a), Ok(b)) => {
                // Each with what the installed build gives it, to check the copy in
                // the game by before it comes out.
                dropped = depot::dropped_paths(&a, &b)
                    .into_iter()
                    .map(|path| update::DroppedFile {
                        size: a.iter().find_map(|d| d.sizes.get(&path).copied()),
                        md5: a.iter().find_map(|d| d.md5s.get(&path).cloned()),
                        path,
                    })
                    .collect();
                lists = Some((a, b));
            }
            (Err(e), _) | (_, Err(e)) => notes.push(format!(
                "couldn't list the files build {} dropped ({e}); the files the cache switch \
                 deletes are still taken out of the game",
                to.build_id
            )),
        }
    }

    let planned = update::plan_update(update::PlanInput {
        game: &game,
        title: &title,
        to: to.clone(),
        picked: a.build.is_some(),
        downgrade,
        from_version,
        from_generation,
        target_dlcs: &target_dlcs,
        language_missing,
        same_install_dir,
        tree_name: target_manifest.install_directory().to_string(),
        dropped,
        notes,
        download_size,
        disk_size,
        manifest: Some((
            to.build_id.clone(),
            *target.generation(),
            target_manifest.clone(),
        )),
        depots: match (&lists, &from_manifest) {
            (Some((from, to)), Some(old)) => Some(update::DepotLists {
                from,
                from_tree: old.install_directory().to_string(),
                to,
            }),
            _ => None,
        },
    });
    let mut plan = match planned {
        Ok(Planned::Update(p)) => p,
        Ok(Planned::UpToDate(_)) => return Ok(cli::exit::OK),
        Err(r) => {
            eprintln!("gogo: {r}");
            return Ok(if r.usage {
                cli::exit::USAGE
            } else {
                cli::exit::DEPOT
            });
        }
    };
    let snapshots = crate::auto_snapshots(a.no_snapshots)?;
    plan.autosnapshot = !plan.resume
        && matches!(
            gogo::linuxapp::snapshots::auto_skipped(&gogid, &game.dirs, &snapshots),
            Ok(None)
        );
    if !machine && !quiet {
        for l in update::plan_lines(&plan) {
            eprintln!("gogo: {l}");
        }
        if !plan.resume {
            match gogo::linuxapp::snapshots::auto_skipped(&gogid, &game.dirs, &snapshots) {
                Ok(None) => eprintln!("gogo: snapshot first: {}", plan.snapshot_label()),
                Ok(Some(why)) => eprintln!("gogo: no snapshot first: {why}"),
                Err(_) => {}
            }
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
            eprintln!("gogo: updating a game needs consent: pass --yes");
            return Ok(cli::exit::USAGE);
        }
        // Taking DLC out asks with the saves warning again, and defaults to no
        // (design 19 §3.7's question).
        let (prompt, default) = match plan.saves_warning() {
            Some(w) => (
                format!(
                    "{w}. Update it now and remove {}?",
                    plan.removing
                        .iter()
                        .map(|d| d.title.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                false,
            ),
            None => ("update it now?".to_string(), true),
        };
        let go = crate::depot_progress::prompting(|| {
            dialoguer::Confirm::new()
                .with_prompt(prompt.clone())
                .default(default)
                .interact()
        })
        .unwrap_or(false);
        if !go {
            eprintln!("gogo: aborted");
            return Ok(cli::exit::USAGE);
        }
    }

    // `depot::run`'s DLC check needs the owned set and the catalog's titles.
    let installed_dlcs: Vec<u64> = plan.keeping().iter().map(|d| d.id).collect();
    let dlc_context = if installed_dlcs.is_empty() {
        DlcContext::default()
    } else {
        crate::phase(machine, "checking the game's DLC", async {
            Ok::<_, AppError>(DlcContext {
                owned: library::owned_ids(&ctx.http, &token).await?,
                catalog_dlcs: gogo::gog::catalog::detail(&ctx.http, &token, id)
                    .await?
                    .expanded_dlcs,
            })
        })
        .await?
    };
    let opts = DepotOptions {
        language: language.clone(),
        language_explicit: false,
        dlcs: if installed_dlcs.is_empty() {
            DlcSet::None
        } else {
            DlcSet::Ids(installed_dlcs)
        },
        dlc_context,
        password: None,
        build: Some(to.build_id.clone()),
        dir: None,
        library: None,
        limit,
        disk_check: a.disk_check,
        dry_run: false,
        dropping,
        dropping_paths: plan.dlc_cache_paths.clone(),
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
    if matches!(mode, depot_progress::Mode::Human { .. }) {
        eprintln!(
            "gogo: switching {title}'s depot cache to build {}",
            to.build_id
        );
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<DepotEvent>();
    let free_space = ctx.free_space.clone();
    let fetch = async || {
        depot::run(
            session,
            id,
            &title,
            &opts,
            &tx,
            &cancel,
            free_space.as_ref(),
        )
        .await
        .map(|_| ())
    };
    let stop_token = cancel.clone();
    let stop = move || stop_token.is_cancelled();
    let act = depot_progress::Activity::new(mode);
    let live = depot_progress::Live::new(&mut rx, &act);
    let mut on_event = |ev: &ProgressEvent| {
        if !live.work(ev) {
            emit(ev, jp, quiet, &|l| live.note(l))
        }
    };
    let uopts = update::UpdateOptions {
        snapshots,
        ..Default::default()
    };
    let runner = update::update(&mut game, &plan, fetch, &uopts, &mut on_event, &stop);
    match live.drive(runner).await {
        Ok(out) => {
            if a.json {
                println!("{}", serde_json::to_string(&out).unwrap());
            } else if !quiet {
                for w in &out.warnings {
                    eprintln!("gogo: warning: {w}");
                }
                eprintln!(
                    "gogo: updated {title} from {} to {} (build {}): {} file(s) laid, {} \
                     removed; DLC kept: {}{}",
                    out.from.label(),
                    out.to.label(),
                    out.to.build_id,
                    out.laid,
                    out.removed,
                    if out.dlcs.is_empty() {
                        "none".to_string()
                    } else {
                        out.dlcs
                            .iter()
                            .map(|d| d.title.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    },
                    if out.dlcs_removed.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "; DLC removed: {}",
                            out.dlcs_removed
                                .iter()
                                .map(|d| d.title.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    }
                );
            }
            Ok(cli::exit::OK)
        }
        Err(e) => Ok(report(e)),
    }
}
