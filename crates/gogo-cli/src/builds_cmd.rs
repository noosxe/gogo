//! `gogo builds <game>` and `gogo builds --installed` (design 21 §2): the network
//! side; what is shown is `gogo::gog::builds_report`.

use chrono::Utc;
use gog_warp::Core;
use gog_warp::content_system::types::Platform;
use gogo::app::AppContext;
use gogo::gog::build_hint;
use gogo::gog::builds::{self, Listing};
use gogo::gog::builds_report::{self as report, DlcName, InstalledRow, Patch, Report, Row};
use gogo::gog::catalog::{self, Os, Product};
use gogo::gog::depot::{self, DepotError};
use gogo::gog::{content, library};

use crate::{AppError, cli};

pub async fn run(args: cli::BuildsArgs) -> Result<i32, AppError> {
    let built = AppContext::build(crate::load_config_from_env()?, true)
        .await
        .map_err(|e| {
            eprintln!("gogo: {e}");
            AppError::Usage
        })?;
    let token = crate::bearer(&built).await?;
    let core = built
        .session
        .as_ref()
        .and_then(|p| p.gog_warp_core())
        .ok_or(AppError::Usage)?;
    if args.installed {
        return installed(&built, &token, core, args.json).await;
    }
    let name = args.game.clone().ok_or(AppError::Usage)?;
    let selected = crate::resolve_games(&built, &token, std::slice::from_ref(&name)).await?;
    let Some(game) = selected.first() else {
        eprintln!("gogo: no owned game matches {name}");
        return Ok(cli::exit::USAGE);
    };
    let (platform, os_name, catalog_os) = match args.os {
        cli::BuildsOs::Windows => (Platform::Windows, "windows", Os::Windows),
        cli::BuildsOs::Osx => (Platform::OsX, "osx", Os::Mac),
    };

    // What this machine uses of the game.
    let cache_base = depot::default_cache_dir();
    let built_game = gogo::linuxapp::dlc::load_game(&game.id.to_string(), cache_base.as_deref())
        .ok()
        .filter(|g| g.content.lane == content::Lane::Depot);
    let cache_build = depot::cache_roots(cache_base.as_deref(), game.id, None)
        .and_then(|(root, _, _)| depot::read_record(&root))
        .map(|r| r.build_id);
    let language = args
        .lang
        .clone()
        .or_else(|| built_game.as_ref().and_then(|g| g.content.language.clone()))
        .unwrap_or_else(|| gogo::gog::language::DEFAULT.to_string());

    let listing = builds::fetch_listing(core, game.id, platform, args.password.clone())
        .await
        .map_err(|e| AppError::Depot(DepotError::library(e)))?;

    // The manifests, in parallel: one that fails is `?` and a warning.
    let facts = report::fetch_facts(core, &listing, &language).await;

    // Titles, owned DLC and the installer lane come from the catalog; none of it
    // is needed for the table, so a failure only leaves it out.
    let detail = catalog::detail(&built.http, &token, game.id).await.ok();
    let owned = if facts.values().any(|f| !f.dlcs.is_empty()) {
        library::owned_ids(&built.http, &token)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut dlc_names: Vec<DlcName> = Vec::new();
    for f in facts.values() {
        for id in &f.dlcs {
            if dlc_names.iter().any(|d| d.id == *id) {
                continue;
            }
            let title = detail
                .as_ref()
                .and_then(|d| d.expanded_dlcs.iter().find(|p| p.id == *id))
                .and_then(|p| p.title.clone())
                .unwrap_or_else(|| id.to_string());
            dlc_names.push(DlcName {
                id: *id,
                title,
                owned: owned.contains(id),
            });
        }
    }
    dlc_names.sort_by_key(|d| d.id);

    let rep = assemble(
        game.id,
        &game.title,
        os_name,
        &language,
        &listing,
        &facts,
        built_game.as_ref().and_then(|g| {
            g.content
                .build_id
                .clone()
                .map(|b| (b, g.content.version.clone()))
        }),
        cache_build,
        detail
            .as_ref()
            .and_then(|d| installer_version(d, catalog_os)),
        args.patches.then(|| {
            detail
                .as_ref()
                .map(|d| patches(d, catalog_os))
                .unwrap_or_default()
        }),
        dlc_names,
        built_game.as_ref().map(|g| g.dirs.root.as_path()),
    );

    for e in rep.failed_manifests() {
        eprintln!(
            "gogo: warning: the manifest of build {} couldn't be fetched; its size and DLC show `?`",
            e.build_id
        );
    }
    if args.json {
        println!("{}", serde_json::to_string(&rep.to_json()).unwrap());
    } else {
        print!("{}", rep.render());
    }
    Ok(cli::exit::OK)
}

/// Put the listing, the manifests' facts and this machine's builds together.
#[allow(clippy::too_many_arguments)]
fn assemble(
    gogid: u64,
    title: &str,
    os: &str,
    language: &str,
    listing: &Listing,
    facts: &std::collections::HashMap<String, report::Facts>,
    game_build: Option<(String, Option<String>)>,
    cache_build: Option<String>,
    installer: Option<String>,
    patches: Option<Vec<Patch>>,
    dlc_names: Vec<DlcName>,
    kept_root: Option<&std::path::Path>,
) -> Report {
    let game_id = game_build.as_ref().map(|(b, _)| b.clone());
    let here = |id: &str| {
        let mut v = Vec::new();
        if game_id.as_deref() == Some(id) {
            v.push("game");
        }
        if cache_build.as_deref() == Some(id) {
            v.push("cache");
        }
        v
    };
    let rows: Vec<Row> = listing
        .entries()
        .into_iter()
        .map(|e| Row {
            facts: facts.get(&e.build_id).cloned(),
            here: here(&e.build_id),
            entry: e,
        })
        .collect();
    let listed = |id: &str| rows.iter().any(|r| r.entry.build_id == id);
    let mut unlisted = Vec::new();
    if let Some((id, version)) = &game_build
        && !listed(id)
    {
        // The manifest kept with the game (design 21 §5.2); a game built before
        // it was kept has none, and the row stays bare.
        let kept =
            kept_root.and_then(|root| gogo::gog::build_manifest::describe(root, id, language));
        unlisted.push(report::Unlisted {
            build_id: id.clone(),
            version: version.clone().unwrap_or_default(),
            here: here(id),
            kept_fetched: kept
                .as_ref()
                .and_then(|k| k.fetched)
                .map(|d| d.format("%Y-%m-%d").to_string()),
            kept: kept.map(|k| k.facts),
        });
    }
    if let Some(id) = &cache_build
        && !listed(id)
        && game_id.as_ref() != Some(id)
    {
        unlisted.push(report::Unlisted {
            build_id: id.clone(),
            version: String::new(),
            here: here(id),
            kept: None,
            kept_fetched: None,
        });
    }
    Report {
        gogid: gogid.to_string(),
        title: title.to_string(),
        os: os.to_string(),
        language: language.to_string(),
        total_count: listing.total_count,
        with_password: listing.with_password,
        newest: listing.newest().map(|b| b.build_id().clone()),
        rows,
        installed_game: game_id,
        installed_cache: cache_build.clone(),
        unlisted,
        installer,
        patches,
        dlc_names,
    }
}

/// The installer's version for `os`: the first installer that names one.
fn installer_version(p: &Product, os: Os) -> Option<String> {
    p.downloads
        .as_ref()?
        .installers
        .iter()
        .filter(|i| i.os == Some(os))
        .find_map(|i| i.version.clone().filter(|v| !v.trim().is_empty()))
}

/// GOG's patch installers for `os`, newest-looking last as GOG lists them.
fn patches(p: &Product, os: Os) -> Vec<Patch> {
    p.downloads
        .iter()
        .flat_map(|d| d.patches.iter())
        .filter(|x| x.os == Some(os))
        .map(|x| Patch {
            name: x.name.clone().unwrap_or_else(|| x.id.clone()),
            version: x.version.clone().unwrap_or_default(),
            language: x
                .language_full
                .clone()
                .or_else(|| x.language.clone())
                .unwrap_or_default(),
            size: x.total_size,
        })
        .collect()
}

/// `--installed`: every built game, checked against a fresh listing (which also
/// refreshes the listing cache).
async fn installed(
    ctx: &AppContext,
    token: &str,
    core: &Core,
    json: bool,
) -> Result<i32, AppError> {
    let cache_base = depot::default_cache_dir();
    let ttl = 24;
    let mut rows = Vec::new();
    for gogid in gogo::macapp::apps::game_ids()? {
        let id: u64 = gogid.parse().unwrap_or(0);
        let game = match gogo::linuxapp::dlc::load_game(&gogid, cache_base.as_deref()) {
            Ok(g) => g,
            Err(e) => {
                rows.push(unchecked(&gogid, &gogid, e.to_string()));
                continue;
            }
        };
        let title = gogo::linuxapp::game::read_launch_toml(&game.dirs)
            .map(|(t, _)| t.title)
            .unwrap_or_else(|_| gogid.clone());
        // Refresh what the comparison reads: the listing cache (depot lane) or the
        // catalog entry (installer lane); then compare as `apps list` does.
        let now = Utc::now();
        let mut detail = None;
        let mut failure = None;
        if game.content.lane == content::Lane::Installer {
            match catalog::detail(&ctx.http, token, id).await {
                Ok(d) => detail = Some(d),
                Err(e) => failure = Some(e.to_string()),
            }
        } else if let Err(e) = builds::fetch_listing(core, id, Platform::Windows, None).await {
            failure = Some(e.to_string());
        }
        let hint = build_hint::for_content(
            &game.content,
            &gogid,
            cache_base.as_deref(),
            detail.as_ref(),
            Some(now),
            ttl,
            now,
        );
        let mut row = match hint {
            Some(h) => InstalledRow::from_hint(&gogid, &title, &h, game.content.build_id.clone()),
            None => unchecked(&gogid, &title, "content.toml names no build".into()),
        };
        row.error = row.error.or(failure);
        rows.push(row);
    }
    if json {
        println!(
            "{}",
            serde_json::to_string(&report::installed_json(&rows, Utc::now())).unwrap()
        );
    } else {
        print!("{}", report::render_installed(&rows));
    }
    Ok(cli::exit::OK)
}

/// A row for a game whose check couldn't be made.
fn unchecked(gogid: &str, title: &str, reason: String) -> InstalledRow {
    InstalledRow {
        gogid: gogid.to_string(),
        title: title.to_string(),
        lane: "depot",
        installed_id: None,
        installed_version: None,
        newest_id: None,
        newest_version: None,
        listed: true,
        state: build_hint::State::Unknown,
        error: Some(reason),
    }
}
