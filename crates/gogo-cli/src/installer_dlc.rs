//! DLC on the offline-installer lane (design 19 §5, #351), the network side: which
//! game a setup is for, which DLC a build or an add takes, the version check against
//! GOG's current installer, and the DLC installers' download (the download engine,
//! into `download.dir`, recorded as `gogo download` records them).
//!
//! The installers run in `gogo::linuxapp::dlc_installer`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gogo::app::AppContext;
use gogo::engine::select::Filters;
use gogo::engine::task::Status;
use gogo::gog::catalog::{Os, Product};
use gogo::gog::content::DlcRef;
use gogo::gog::dlc::{self, Basis, DlcInfo, Want};
use gogo::linuxapp::dlc_installer::{self as di, BuildDlc, InstallerDlc, VersionCheck};

use crate::{AppError, DlcAsk};

/// The game a setup file is for, as far as gogo can tell.
#[derive(Debug, Clone, Default)]
pub(crate) struct SetupFacts {
    pub gogid: Option<u64>,
    /// The version label gogo's download record has for the file.
    pub version: Option<String>,
    /// The catalog entry id of the file (`en1installer0`), from the record.
    pub entry: Option<String>,
}

/// The download root, as the engine resolves it.
fn download_root(ctx: &AppContext) -> PathBuf {
    PathBuf::from(ctx.cfg.download.dir.clone().unwrap_or_default())
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// gogo's download record for `setup`, else the owned game its name names
/// (`setup_<slug>_…`).
pub(crate) async fn identify(
    ctx: &AppContext,
    token: &str,
    setup: &Path,
) -> Result<SetupFacts, AppError> {
    let root = download_root(ctx);
    let recorded = {
        let state = ctx.state.lock().expect("state lock");
        state
            .installs
            .iter()
            .find(|r| r.dlc_id.is_none() && same_file(&root.join(&r.path), setup))
            .cloned()
    };
    if let Some(r) = recorded {
        return Ok(SetupFacts {
            gogid: Some(r.product_id),
            version: r.version,
            entry: Some(r.entry_id),
        });
    }
    let name = setup
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let games: Vec<(u64, String)> = gogo::gog::library::list_games(&ctx.http, token)
        .await?
        .into_iter()
        .filter_map(|g| Some((g.id, g.slug?)))
        .collect();
    Ok(SetupFacts {
        gogid: di::game_for_setup(&name, &games),
        ..Default::default()
    })
}

/// What a first build on this lane takes, with what downloading it needs later.
pub(crate) struct BuildChoice {
    pub build: BuildDlc,
    pub notes: Vec<String>,
    /// The game's catalog entry and the context, when DLC were chosen.
    pub fetch: Option<(Arc<AppContext>, String, Product)>,
}

fn title_of(infos: &[DlcInfo], id: u64) -> String {
    infos
        .iter()
        .find(|d| d.id == id)
        .map_or_else(|| id.to_string(), |d| d.title.clone())
}

fn installable(infos: &[DlcInfo]) -> Vec<u64> {
    infos
        .iter()
        .filter(|d| d.owned && d.in_build)
        .map(|d| d.id)
        .collect()
}

/// Names to ids on this lane: owned, with a Windows installer.
pub(crate) fn resolve_names(
    names: &[String],
    game_title: &str,
    infos: &[DlcInfo],
) -> Result<Vec<u64>, String> {
    let ids =
        dlc::resolve(names, game_title, infos, &dlc::Lane::Installer).map_err(|e| e.to_string())?;
    if let Some(id) = ids
        .iter()
        .find(|id| !infos.iter().any(|d| d.id == **id && d.in_build))
    {
        return Err(format!(
            "\"{}\" ({id}) has no Windows offline installer; DLC with one: {}",
            title_of(infos, *id),
            names_of(&installable(infos), infos)
        ));
    }
    Ok(ids)
}

pub(crate) fn names_of(ids: &[u64], infos: &[DlcInfo]) -> String {
    if ids.is_empty() {
        return "none".into();
    }
    ids.iter()
        .map(|id| format!("{id} {}", title_of(infos, *id)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// An error's text for a plan note.
fn error_text(e: &AppError) -> String {
    match e {
        AppError::Config(e) => e.to_string(),
        AppError::Usage => "a usage error".into(),
        AppError::Auth(e) => e.to_string(),
        AppError::Api(e) => e.to_string(),
        AppError::MacApp(e) => e.to_string(),
        AppError::Depot(e) => e.to_string(),
        AppError::Io(e) => e.to_string(),
    }
}

/// The base installer's language as far as gogo can tell.
struct Lang {
    /// The language the DLC installers are fetched in.
    code: String,
    /// It's the base installer's own (its catalog entry's, from gogo's download
    /// record), not a guess from `--lang` or `[gog] language`.
    known: bool,
    /// `--lang` differs from the known language: a usage error once DLC are chosen.
    conflict: Option<String>,
}

/// The base installer's language: its catalog entry's (from the record), else
/// `--lang`, else `[gog] language`, unconfirmed. The DLC follow the base.
fn language_for(facts: &SetupFacts, detail: &Product, ask: &DlcAsk, configured: &str) -> Lang {
    let known = facts.entry.as_ref().and_then(|id| {
        detail
            .downloads
            .as_ref()?
            .installers
            .iter()
            .find(|e| &e.id == id)
            .and_then(|e| e.language.clone())
    });
    match (known, &ask.lang) {
        (Some(k), Some(l)) if !gogo::gog::language::same_language(l, &k) => Lang {
            conflict: Some(format!(
                "--lang {l}: the game's installer is in {k}, and its DLC's installers follow \
                 it; download the installer in {l} to build in it"
            )),
            code: k,
            known: true,
        },
        (Some(k), _) => Lang {
            code: k,
            known: true,
            conflict: None,
        },
        (None, l) => Lang {
            code: l.clone().unwrap_or_else(|| configured.to_string()),
            known: false,
            conflict: None,
        },
    }
}

/// Decide a first build's DLC for `setup` (design 19 §1.2, §5). Best effort when no
/// DLC flag asks for any: a game gogo can't tell or reach is built without DLC,
/// with a note. `Err` is a usage error (printed) or the version refusal (exit 7).
pub(crate) async fn plan_build(
    setup: &Path,
    ask: &DlcAsk,
    interactive: bool,
    machine: bool,
) -> Result<BuildChoice, AppError> {
    let asked = ask.wants_dlc();
    let mut out = BuildChoice {
        build: BuildDlc::default(),
        notes: Vec::new(),
        fetch: None,
    };
    let soft = |out: &mut BuildChoice, why: String| -> Result<(), AppError> {
        if asked {
            eprintln!("gogo: {why}");
            return Err(AppError::Usage);
        }
        out.notes.push(why);
        Ok(())
    };
    let connected = crate::dlc_cmd::connect_phase(machine).await;
    let (ctx, token) = match connected {
        Ok(c) => c,
        Err(e) => {
            if asked {
                return Err(e);
            }
            out.notes
                .push("couldn't reach GOG to look for the game's DLC: building without".into());
            return Ok(out);
        }
    };
    // Without a DLC flag every lookup is best effort: a base build needs no network.
    let looked = |out: &mut BuildChoice, what: &str, e: AppError| -> Result<(), AppError> {
        if asked {
            return Err(e);
        }
        out.notes.push(format!(
            "couldn't {what} ({}): building without DLC",
            error_text(&e)
        ));
        Ok(())
    };
    let facts =
        match crate::phase(machine, "checking the setup", identify(&ctx, &token, setup)).await {
            Ok(f) => f,
            Err(e) => {
                looked(&mut out, "tell which game the setup is for", e)?;
                return Ok(out);
            }
        };
    let Some(gogid) = facts.gogid else {
        soft(
            &mut out,
            format!(
                "gogo can't tell which owned game {} is for, so it takes no DLC; build it by \
                 name instead (`gogo download <game>`, then `gogo linux-app <game>`)",
                setup.display()
            ),
        )?;
        return Ok(out);
    };
    let looked_up = crate::phase(machine, "checking the game's DLC", async {
        let detail = gogo::gog::catalog::detail(&ctx.http, &token, gogid).await?;
        let owned = gogo::gog::library::owned_ids(&ctx.http, &token).await?;
        Ok::<_, AppError>((detail, owned))
    })
    .await;
    let (detail, owned) = match looked_up {
        Ok(d) => d,
        Err(e) => {
            looked(&mut out, "look up the game's DLC", e)?;
            return Ok(out);
        }
    };
    let lang = language_for(&facts, &detail, ask, &ctx.cfg.gog.language);
    let language = lang.code.clone();
    let current = dlc::installer_version(&detail, &language);
    let file = setup
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let version = facts
        .version
        .clone()
        .or_else(|| di::setup_version(&file, current.as_deref()));
    out.build.gogid = Some(gogid.to_string());
    out.build.version = version.clone();
    // Recorded only when known; chosen DLC confirm it (below).
    out.build.language = lang.known.then(|| language.clone());

    let title = detail.title.clone().unwrap_or_else(|| gogid.to_string());
    let infos = dlc::installer_infos(&detail.expanded_dlcs, &owned, &language);
    // A rebuild keeps the recorded set (D4).
    let recorded: Option<Vec<u64>> = gogo::macapp::launch::GameDirs::for_game(&gogid.to_string())
        .ok()
        .filter(|d| gogo::linuxapp::game::launch_toml_path(d).is_file())
        .and_then(|d| gogo::gog::content::load(&d.root).ok().flatten())
        .map(|c| c.dlc.iter().map(|d| d.id).collect());
    let want = if ask.all {
        Want::All
    } else if ask.none {
        Want::NoDlc
    } else if !ask.names.is_empty() {
        match resolve_names(&ask.names, &title, &infos) {
            Ok(ids) => Want::Ids(ids),
            Err(e) => {
                eprintln!("gogo: {e}");
                return Err(AppError::Usage);
            }
        }
    } else {
        Want::Default
    };
    let decision = dlc::decide(&want, recorded.as_deref(), &infos, interactive);
    let ids: Vec<u64> = match &decision.basis {
        Basis::Set(dlc::DlcSet::None) => Vec::new(),
        Basis::Set(dlc::DlcSet::AllOwned) => installable(&infos),
        Basis::Set(dlc::DlcSet::Ids(ids)) => {
            // A kept DLC that lost its installer is refused, never dropped silently.
            if let Some(id) = ids
                .iter()
                .find(|id| !infos.iter().any(|d| d.id == **id && d.in_build && d.owned))
            {
                eprintln!(
                    "gogo: \"{}\" ({id}) has no Windows offline installer you own now; pass \
                     --dlc without it (or --no-dlcs) to build without it",
                    title_of(&infos, *id)
                );
                return Err(AppError::Usage);
            }
            ids.clone()
        }
        Basis::Pick(cands) => crate::pick_dlc(cands, &infos)?,
    };
    out.notes.extend(dlc::decision_notes(&decision, &infos));
    if ids.is_empty() {
        return Ok(out);
    }
    if let Some(e) = &lang.conflict {
        eprintln!("gogo: {e}");
        return Err(AppError::Usage);
    }
    out.build.language = Some(language.clone());
    match di::version_check(version.as_deref(), current.as_deref()) {
        VersionCheck::Same => {}
        VersionCheck::Differs { installed, current } => {
            return Err(gogo::macapp::MacAppError::Apps(di::version_refusal(
                &gogid.to_string(),
                &installed,
                &current,
            ))
            .into());
        }
        VersionCheck::Unknown => out.notes.push(di::version_unknown_note(current.as_deref())),
    }
    let size: u64 = ids
        .iter()
        .filter_map(|id| infos.iter().find(|d| d.id == *id)?.download_size)
        .sum();
    out.notes.push(format!(
        "DLC installers: {} ({}), downloaded into {} when missing, then run after the game's",
        gogo::fmt::human_size(size),
        language,
        download_root(&ctx).display()
    ));
    out.build.refs = ids
        .iter()
        .map(|id| DlcRef {
            id: *id,
            title: title_of(&infos, *id),
        })
        .collect();
    out.fetch = Some((ctx, token, detail));
    Ok(out)
}

/// A file a DLC fetch planned: which record is its download.
struct Wanted {
    dlc: Option<u64>,
    entry: String,
    file: String,
    version: Option<String>,
}

impl Wanted {
    fn is(&self, r: &gogo::state::records::InstallRecord) -> bool {
        r.dlc_id == self.dlc
            && r.entry_id == self.entry
            && r.file_id == self.file
            && r.version == self.version
    }
}

/// Download (when missing) the Windows installers of `ids`, DLC of `product`, in
/// `language`, through the download engine; their setup `.exe`s, in `ids` order.
pub(crate) async fn fetch(
    ctx: &Arc<AppContext>,
    product: &Product,
    ids: &[u64],
    infos_titles: &[DlcRef],
    language: &str,
    machine: bool,
) -> Result<Vec<InstallerDlc>, AppError> {
    let filters = Filters {
        os: Some(Os::Windows),
        language: Some(language.to_string()),
        kinds: Vec::new(),
        with_dlcs: true,
        only_dlcs: Some(ids.to_vec()),
    };
    let mut tasks = gogo::engine::plan(ctx, std::slice::from_ref(product), &filters, false).await;
    tasks.retain(|t| t.dlc_id.is_some());
    // The files GOG serves now, by entry, file and version: only their records
    // count below, never an older download of the same DLC.
    let wanted: Vec<Wanted> = tasks
        .iter()
        .map(|t| Wanted {
            dlc: t.dlc_id,
            entry: t.entry.id.clone(),
            file: t.file.id.clone(),
            version: t.entry.version.clone(),
        })
        .collect();
    let missing: u64 = tasks
        .iter()
        .filter(|t| !matches!(t.status, Status::Present))
        .map(|t| t.total_size)
        .sum();
    if missing > 0 {
        if !machine {
            eprintln!(
                "gogo: downloading the DLC installers ({}) into {}",
                gogo::fmt::human_size(missing),
                download_root(ctx).display()
            );
        }
        let bars = !machine && std::io::IsTerminal::is_terminal(&std::io::stderr());
        let cancel = ctx.cancel.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            cancel.cancel();
        });
        let summary = if bars {
            let rx = ctx.events.subscribe();
            let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
            let work = async {
                let s = gogo::engine::execute(ctx, tasks).await;
                let _ = done_tx.send(());
                s
            };
            let (s, ()) = tokio::join!(work, crate::render_bars(rx, done_rx));
            s
        } else {
            gogo::engine::execute(ctx, tasks).await
        };
        if ctx.cancel.is_cancelled() {
            eprintln!("gogo: stopped during the DLC installers' download; nothing was installed");
            return Err(AppError::Usage);
        }
        if summary.failed > 0 {
            return Err(gogo::macapp::MacAppError::Apps(format!(
                "{} DLC installer file(s) failed to download; nothing was installed (rerun to \
                 resume)",
                summary.failed
            ))
            .into());
        }
    }
    let root = download_root(ctx);
    let state = ctx.state.lock().expect("state lock");
    let mut out = Vec::new();
    for id in ids {
        let rec = state.installs.iter().rev().find(|r| {
            r.product_id == product.id
                && r.dlc_id == Some(*id)
                && r.path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
                && wanted.iter().any(|w| w.is(r))
                && root.join(&r.path).is_file()
        });
        let Some(rec) = rec else {
            return Err(gogo::macapp::MacAppError::Apps(format!(
                "no downloaded setup .exe for DLC {id}: its Windows installer didn't download"
            ))
            .into());
        };
        out.push(InstallerDlc {
            id: *id,
            title: infos_titles
                .iter()
                .find(|d| d.id == *id)
                .map_or_else(|| id.to_string(), |d| d.title.clone()),
            setup: root.join(&rec.path),
            version: rec.version.clone(),
        });
    }
    Ok(out)
}
