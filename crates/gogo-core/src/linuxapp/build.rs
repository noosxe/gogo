//! `gogo linux-app <game|setup.exe|dir>` (design 18 §10): a Windows game
//! set up to run through GE-Proton and umu, as `games/<gogid>/`. The
//! sibling of `macapp::plan` / `execute`, without the bundle.
//!
//! [`plan`] resolves everything and asks what has to be asked before
//! anything long or downloaded starts: `[linux] umu` (#168), `[linux]
//! proton_dir` when a GE-Proton has to be installed (#169), the renderer
//! (§3), and, when the Proton build is already here, the umu id (#170) and
//! the fix summary (#171). [`execute`] downloads, materializes the depot
//! tree (#175) or runs the offline installer under umu, writes
//! `gogo-launch.toml`, and runs [`super::setup::setup`]. The `.desktop`
//! file and `launch.sh` are #177's.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::fixes;
use super::game::{self, refuse_private_tmp};
use super::proton::{self, ProtonRef};
use super::setup::{self, ProtonBuild, SetupOptions, SetupOutcome, Umu};
use super::snapshots::{self, BuildSnapshots, SnapshotOptions};
use super::umu::{self, Host};
use super::umu_id::UmuId;
use crate::config::{Config, UmuMode};
use crate::engine::events::{ProgressEvent, step, working};
use crate::gog::content;
use crate::gog::depot;
pub use crate::gog::dlc_filter::DepotContent;
use crate::gog::materialize::{self, MaterializePlan, MaterializeReport};
use crate::macapp::compat::{self, CompatEntry, Effective, Layer, Os};
use crate::macapp::error::MacAppError;
use crate::macapp::launch::{self, GameDirs, LaunchToml};
use crate::macapp::prompt::PromptFn;
use crate::macapp::renderer::{self, LinuxPlan, Renderer, RendererChoice};
use crate::macapp::{catalog, ddraw, info, installer, lutris, pe, store, verbs};

/// `gogo linux-app` inputs.
#[derive(Debug, Clone, Default)]
pub struct LinuxAppOptions {
    /// The offline installer lane: a GOG setup `.exe` (`.bin` parts beside
    /// it), run under umu. Exactly one of `installer` / `game_tree`.
    pub installer: Option<PathBuf>,
    /// The depot lane: a game tree (`gogo depot`'s, or any installed one).
    pub game_tree: Option<PathBuf>,
    pub name: Option<String>,
    /// `--runtime`: `ge-proton[/<ver>]` or `path:<dir>`. `None`: L3's
    /// runtime, else `[linux] default_runtime`, else the catalog's GE-Proton.
    pub runtime: Option<String>,
    pub renderer: RendererChoice,
    /// `--refresh`: re-fetch umu-database's CSV (only a build without one
    /// uses it).
    pub refresh: bool,
    /// The path gogo was invoked as (`[gogo] path`, `GOGO_PATH`): in the
    /// `gogo-linux-app` package `current_exe()` is the unwrapped binary,
    /// which skips the wrapper that sets `GOGO_UMU_PYTHON` (#168).
    pub gogo_path: Option<PathBuf>,
    /// The checked `library.dir` (design 18 §6.1). `None`: `games/`.
    pub library: Option<launch::Library>,
    /// What the DLC choice says for the plan (design 19 §1.2): dropped DLC,
    /// how many owned DLC weren't taken, DLC flags the lane ignores.
    pub dlc_notes: Vec<String>,
    /// The depot lane's DLC (design 19 §3.3): what the build installs and
    /// records. `None`: the tree is taken whole and its infos say which DLC it
    /// has (a tree given by path).
    pub depot_content: Option<DepotContent>,
    /// Snapshots of a new game (design 20 §2.1, §7.1): a subvolume game dir
    /// and its `initial` snapshot. The default is off; the CLI turns it on from
    /// `[snapshots] auto` unless `--no-snapshots`.
    pub snapshots: SnapshotOptions,
    /// The offline-installer lane's DLC (design 19 §5): run after the base
    /// installer, in the staging prefix.
    pub installer_dlc: super::dlc_installer::BuildDlc,
}

/// A GE-Proton the build downloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtonDownload {
    pub id: String,
    pub version: String,
    /// From the catalog probe.
    pub bytes: u64,
    /// A Steam's `compatibilitytools.d` (`[linux] proton_dir`), else the store.
    pub shared: Option<PathBuf>,
}

/// Everything resolved before the long part; the CLI's confirmation.
#[derive(Debug, Clone)]
pub struct LinuxAppPlan {
    /// The title. The installer lane replaces it with the installed
    /// `goggame-*.info` name unless `--name` was given (`title_fixed`).
    pub display: String,
    /// `--name` was given: the title is final.
    pub title_fixed: bool,
    pub slug: String,
    /// Depot lane: known now. Installer lane: after the install.
    pub gogid: Option<String>,
    pub installer: Option<installer::InstallPlan>,
    pub game_tree: Option<PathBuf>,
    pub game_tree_bytes: u64,
    /// `games/<gogid>/`, or the installer lane's staging dir.
    pub state: GameDirs,
    /// The checked `library.dir`, for the installer lane's final place.
    pub library: Option<launch::Library>,
    /// A depot rebuild: `game/` is replaced, the prefix and saves stay.
    pub rebuild: bool,
    pub umu_mode: UmuMode,
    /// The plan's umu line (#168): what runs, and any download.
    pub umu_line: String,
    pub proton_ref: ProtonRef,
    /// The build, when it's here already.
    pub proton: Option<ProtonBuild>,
    pub proton_download: Option<ProtonDownload>,
    /// `--runtime` was given: it replaces an L4 `runtime` (#110).
    pub runtime_flag: bool,
    /// The plan's runtime line.
    pub runtime_line: String,
    /// The Steam Linux Runtime umu fetches on its first run here (§2.3).
    pub container_line: String,
    /// L3 and L4 for Linux, when the id is known (depot lane).
    pub compat: Option<Effective>,
    pub renderer_choice: RendererChoice,
    /// Depot lane: the renderer the build bakes.
    pub renderer: Option<renderer::Selection>,
    /// What that renderer sets (with the build here): Proton switches, the
    /// game-dir step.
    pub renderer_setup: Option<LinuxPlan>,
    pub auto_verbs: Option<Vec<verbs::AutoVerb>>,
    /// Depot lane: reflink or copy into `games/<gogid>/game` (#175).
    pub game_files: Option<MaterializePlan>,
    /// With the build here and the id known: the umu id and the fixes.
    pub umu_id: Option<UmuId>,
    pub fixes: Option<fixes::Summary>,
    pub refresh: bool,
    pub gogo_path: PathBuf,
    /// Depot lane: the DLC the build installs (design 19 §6.2): the chosen
    /// set, else those in the game tree. The installer lane's base installer has
    /// none.
    pub dlcs: Vec<content::DlcRef>,
    /// What the build takes of the tree (design 19 §3.3).
    pub depot_content: Option<DepotContent>,
    /// Lines about the DLC choice, shown under `dlcs`.
    pub dlc_notes: Vec<String>,
    /// The plan's `snapshots:` line (design 20 §8).
    pub snapshots: BuildSnapshots,
    pub snapshot_opts: SnapshotOptions,
    /// The installer lane's DLC (design 19 §5). The caller fills in
    /// `installers` (the downloads) between the plan and [`execute`].
    pub installer_dlc: super::dlc_installer::BuildDlc,
}

/// What [`execute`] did.
#[derive(Debug, Clone)]
pub struct BuildOutcome {
    pub gogid: String,
    /// The title baked into `gogo-launch.toml`.
    pub title: String,
    pub dirs: GameDirs,
    pub materialized: Option<MaterializeReport>,
    pub setup: SetupOutcome,
    /// What `content.toml` now says (design 19 §6.1).
    pub content: content::Content,
    /// The `initial` snapshot a first build took, or the `before rebuild`
    /// one a rebuild took (design 20 §7.1).
    pub snapshot: Option<crate::snapshot::Meta>,
    /// Older `auto` snapshots retention deleted after a rebuild's (§7.2).
    pub pruned: Vec<crate::snapshot::Meta>,
}

/// The renderer line's tail: what setup sets for it (design 18 §3, #172),
/// so the two meanings of `wined3d` read apart.
pub fn switches(p: &LinuxPlan) -> String {
    let mut parts: Vec<String> = p.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
    match p.ddraw {
        renderer::DdrawStep::PlaceCncDdraw => match &p.stand_in {
            Some(name) => parts.push(format!(
                "cnc-ddraw's ddraw.dll in the game dir as {name}, in place of the game's DDrawCompat"
            )),
            None => parts.push("cnc-ddraw's ddraw.dll in the game dir, ddraw=n,b".into()),
        },
        renderer::DdrawStep::KeepGames => parts.push("the game's own ddraw.dll, ddraw=n,b".into()),
        renderer::DdrawStep::LeaveDdrawCompat => {
            parts.push("the game's DDrawCompat left unloaded, Wine's builtin ddraw".into())
        }
        renderer::DdrawStep::RemoveGogo | renderer::DdrawStep::None => {}
    }
    if parts.is_empty() {
        "nothing set: Proton's defaults".into()
    } else {
        parts.join(", ")
    }
}

/// [`renderer::plan_linux`] plus setup's rule for a chosen `wined3d`, for
/// the entry exe's dir in `tree` (an L3/L4 `exe` is setup's to apply).
fn renderer_plan(sel: &renderer::Selection, tree: &Path, build: &Path) -> LinuxPlan {
    let entry = info::find_entry_point(tree).ok();
    let dir = ddraw::dir_for(tree, entry.as_deref());
    let mut p = renderer::plan_linux(&sel.renderer.value, &sel.l1, Some(&dir), build);
    renderer::settle_wined3d(&mut p, sel.renderer.layer == Layer::Auto);
    p
}

/// The Steam Linux Runtime variant umu starts for a build's
/// `require_tool_appid` (umu 1.4.4's `RUNTIME_VERSIONS`), and its dir name.
fn container_for(build: Option<&Path>) -> (&'static str, &'static str) {
    let appid = build
        .and_then(|b| proton::require_tool_appid(b).ok().flatten())
        .unwrap_or_else(|| "4183110".into());
    match appid.as_str() {
        "1628350" => ("sniper", "steamrt3"),
        "4185400" => ("steamrt4-arm64", "steamrt4-arm64"),
        "1391110" => ("soldier", "steamrt2"),
        _ => ("steamrt4", "steamrt4"),
    }
}

/// umu's own data dir: `$XDG_DATA_HOME/umu`.
fn umu_folder(host: &Host) -> Option<PathBuf> {
    let abs = |k: &str| host.var(k).map(PathBuf::from).filter(|p| p.is_absolute());
    abs("XDG_DATA_HOME")
        .or_else(|| abs("HOME").map(|h| h.join(".local/share")))
        .map(|d| d.join("umu"))
}

/// Bytes under `p`, symlinks not followed (a Proton prefix links `pfx` to
/// itself).
fn tree_bytes(p: &Path) -> u64 {
    let Ok(meta) = p.symlink_metadata() else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(p)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| tree_bytes(&e.path()))
        .sum()
}

/// [`tree_bytes`] of what a build takes of the cache tree `tree`.
fn kept_bytes(tree: &Path, content: Option<&DepotContent>) -> u64 {
    let Some(c) = content.filter(|c| c.filter.is_some()) else {
        return tree_bytes(tree);
    };
    let Some(root) = tree.parent() else {
        return tree_bytes(tree);
    };
    fn walk(p: &Path, root: &Path, c: &DepotContent) -> u64 {
        if !c.keeps(p.strip_prefix(root).unwrap_or(p)) {
            return 0;
        }
        let Ok(meta) = p.symlink_metadata() else {
            return 0;
        };
        if !meta.is_dir() {
            return meta.len();
        }
        std::fs::read_dir(p)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| walk(&e.path(), root, c))
            .sum()
    }
    walk(tree, root, c)
}

fn probe_size(entry: &catalog::CatalogEntry, version: Option<&str>) -> Result<u64, MacAppError> {
    let http = catalog::http_client()?;
    Ok(lutris::block_on_detached(catalog::probe(entry, version, &http, catalog::GITHUB_API))?.size)
}

/// How umu runs, asked once (`[linux] umu`), and checked now: nothing is
/// downloaded that couldn't run. Headless with no answer fails naming the
/// key.
fn plan_umu(
    prompt: Option<&PromptFn>,
    host: &Host,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<(UmuMode, String), MacAppError> {
    let entry = catalog::find(catalog::UMU).expect("the catalog has umu");
    let version = setup::pinned_version(catalog::UMU);
    let config = Config::path();
    let mode = umu::mode_at(
        &config,
        prompt,
        || {
            let size = probe_size(entry, None)?;
            let blocker = umu::resolve_python(host).err().map(|e| match e {
                MacAppError::Umu(why) => why,
                e => e.to_string(),
            });
            let found = umu::resolve_system(host).is_ok();
            Ok(umu::prompt_spec(
                entry,
                size,
                &config,
                blocker.as_deref(),
                found,
            ))
        },
        emit,
    )?;
    let line = match mode {
        UmuMode::System => format!(
            "the umu-run on PATH, {} (unpinned)",
            umu::resolve_system(host)?.display()
        ),
        UmuMode::Zipapp => {
            let python = umu::resolve_python(host)?;
            match store::installed(catalog::UMU, version) {
                Some((m, _)) => format!(
                    "umu-launcher {} from gogo's store, run by {}",
                    m.version,
                    python.display()
                ),
                None => format!(
                    "{}; run by {}",
                    umu::plan_line(entry, probe_size(entry, None)?),
                    python.display()
                ),
            }
        }
    };
    Ok((mode, line))
}

/// The Proton build: `--runtime`, else L3's (Linux), else `[linux]
/// default_runtime`, else the catalog's GE-Proton. A missing catalog build
/// is planned as a download, `[linux] proton_dir` asked once.
fn plan_proton(
    opts: &LinuxAppOptions,
    compat: Option<&Effective>,
    prompt: Option<&PromptFn>,
    host: &Host,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<
    (
        ProtonRef,
        Option<ProtonBuild>,
        Option<ProtonDownload>,
        String,
    ),
    MacAppError,
> {
    let config = Config::path();
    let l3 = compat
        .and_then(|c| c.runtime.as_ref())
        .filter(|r| r.layer == Layer::Db)
        .map(|r| r.value.clone());
    let r = match opts.runtime.as_deref().or(l3.as_deref()) {
        Some(s) => ProtonRef::parse(s).map_err(MacAppError::Proton)?,
        None => proton::default_ref(super::stored_key(&config, "default_runtime").as_deref())
            .map_err(MacAppError::Proton)?,
    };
    if let Some(b) = setup::find_proton(&r)? {
        let line = if b.managed {
            format!("{} ({})", b.reference, b.dir.display())
        } else {
            let mut problems = proton::inspect(&b.dir, "").problems();
            problems.insert(0, "unmanaged: not pinned, not installed by gogo".into());
            format!("{} ({})", b.reference, problems.join("; "))
        };
        return Ok((r, Some(b), None, line));
    }
    let ProtonRef::Catalog { id, version } = &r else {
        unreachable!("find_proton returns path: builds or fails")
    };
    let entry = catalog::find(id)
        .ok_or_else(|| MacAppError::Proton(format!("{id} is not in gogo's catalog")))?;
    let version = version
        .clone()
        .or(setup::pinned_version(id).map(str::to_string))
        .ok_or_else(|| MacAppError::Proton(format!("{id} has no pinned version")))?;
    let bytes = probe_size(entry, Some(&version))?;
    let shared = proton::placement(entry, Some(bytes), &config, prompt, host, emit)?;
    let into = match &shared {
        Some(d) => format!("{} (shared with Steam, never pruned by gogo)", d.display()),
        None => format!("gogo's store, {}", store::runtimes_dir().join(id).display()),
    };
    let line = format!(
        "{id}/{version}: {} download into {into}, about 1.5 GB unpacked",
        crate::fmt::human_size(bytes)
    );
    let download = ProtonDownload {
        id: id.clone(),
        version,
        bytes,
        shared,
    };
    Ok((r, None, Some(download), line))
}

/// L3 and L4 for Linux (what the plan shows before the confirmation).
fn compat_for(gogid: &str, emit: &mut dyn FnMut(&ProgressEvent)) -> Effective {
    let db = compat::db_entry_for(gogid, Os::Linux);
    for w in &db.warnings {
        emit(&ProgressEvent::Warn {
            message: format!("compat DB {gogid}: {w}"),
        });
    }
    let user = match compat::load_user_for(gogid, Os::Linux) {
        Ok(p) => {
            for w in &p.warnings {
                emit(&ProgressEvent::Warn {
                    message: format!("{}: {w}", compat::user_path(gogid).display()),
                });
            }
            p.entry
        }
        Err(e) => {
            emit(&ProgressEvent::Warn {
                message: e.to_string(),
            });
            CompatEntry::default()
        }
    };
    compat::merge(&[(Layer::Db, &db.entry), (Layer::User, &user)])
}

/// The renderer for a scanned tree (design 18 §3), reported.
fn select_renderer(
    gogid: Option<&str>,
    scan: &Result<pe::PeScan, pe::ScanError>,
    choice: RendererChoice,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> renderer::Selection {
    let sel = renderer::select_on(Os::Linux, gogid, renderer::pick_linux(scan), choice);
    emit(&ProgressEvent::RendererSelected {
        renderer: if sel.renderer.value == Renderer::Wined3d.id()
            && sel.renderer.layer == Layer::Auto
        {
            renderer::PROTON_DEFAULT.to_string()
        } else {
            sel.renderer.value.clone()
        },
        layer: sel.renderer.layer.describe().to_string(),
        reason: sel.reason.clone(),
    });
    for message in &sel.warnings {
        emit(&ProgressEvent::Warn {
            message: message.clone(),
        });
    }
    sel
}

/// The layers the fix summary merges besides L3: L1's verbs, the renderer
/// with its layer, L4.
fn summary_layers(
    gogid: &str,
    auto_verbs: &[String],
    renderer: Option<&renderer::Selection>,
) -> Vec<(Layer, CompatEntry)> {
    let mut layers = vec![(
        Layer::Auto,
        CompatEntry {
            verbs: Some(auto_verbs.to_vec()),
            ..Default::default()
        },
    )];
    if let Some(sel) = renderer {
        layers.push((
            sel.renderer.layer,
            CompatEntry {
                renderer: Some(sel.renderer.value.clone()),
                ..Default::default()
            },
        ));
    }
    if let Ok(l4) = compat::load_user_for(gogid, Os::Linux) {
        layers.push((Layer::User, l4.entry));
    }
    layers
}

/// Resolve and ask (see the module docs). Downloads nothing.
pub fn plan(
    opts: &LinuxAppOptions,
    prompt: Option<&PromptFn>,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<LinuxAppPlan, MacAppError> {
    assert!(
        opts.installer.is_some() ^ opts.game_tree.is_some(),
        "exactly one source: installer or game_tree"
    );
    let data_root = crate::state::data_root();
    refuse_private_tmp("gogo's data dir", &data_root)?;
    let display = match (opts.game_tree.as_ref(), opts.name.as_deref()) {
        (_, Some(n)) => n.trim().to_string(),
        (Some(tree), None) => info::game_name(tree).unwrap_or_else(|| {
            tree.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Game".to_string())
        }),
        (None, None) => crate::macapp::display_name(opts.installer.as_ref().unwrap(), None),
    };
    let slug = crate::macapp::slugify(&display);
    let install = match &opts.installer {
        Some(p) => {
            let plan = installer::plan(p)?;
            refuse_private_tmp("the installer", &plan.installer)?;
            Some(plan)
        }
        None => None,
    };
    let gogid = opts.game_tree.as_deref().and_then(info::root_game_id);
    if opts.game_tree.is_some() && gogid.is_none() {
        return Err(MacAppError::EntryPointNotFound {
            dir: opts.game_tree.clone().unwrap(),
        });
    }
    let compat = gogid.as_deref().map(|id| compat_for(id, emit));

    let host = Host::real();
    let (umu_mode, umu_line) = plan_umu(prompt, &host, emit)?;
    let (proton_ref, proton, proton_download, runtime_line) =
        plan_proton(opts, compat.as_ref(), prompt, &host, emit)?;
    let (variant, dir) = container_for(proton.as_ref().map(|b| b.dir.as_path()));
    let container_line = match umu_folder(&host).map(|d| d.join(dir)) {
        Some(d) if d.is_dir() => format!("{variant} in {} (umu's, shared)", d.display()),
        Some(d) => format!(
            "umu fetches the Steam Linux Runtime ({variant}, about 175 MB, 699 MB unpacked) \
             into {} on its first run",
            d.display()
        ),
        None => format!("umu fetches the Steam Linux Runtime ({variant}) on its first run"),
    };

    let (state, rebuild) = match &gogid {
        Some(id) => {
            let dirs = GameDirs::for_build(id, opts.library.as_ref())?;
            let rebuild = game::launch_toml_path(&dirs).is_file() || dirs.game.exists();
            if let Some(message) = launch::stays_note(&dirs, opts.library.as_ref()) {
                emit(&ProgressEvent::Warn { message });
            }
            (dirs, rebuild)
        }
        None => (
            GameDirs::at(launch::staging_root(opts.library.as_ref(), &slug)),
            false,
        ),
    };
    refuse_private_tmp("the game's dir", &state.root)?;
    let snapshots = BuildSnapshots::plan(&opts.snapshots, gogid.as_deref(), &state.root, rebuild);
    let game_tree_bytes = opts
        .game_tree
        .as_deref()
        .map(|t| kept_bytes(t, opts.depot_content.as_ref()))
        .unwrap_or(0);
    let game_files = opts
        .game_tree
        .as_ref()
        .map(|t| materialize::plan_tree(t, &state.game, game_tree_bytes));

    // One scan of the depot tree: the renderer and the L1 verbs.
    let scan = opts.game_tree.as_ref().map(|t| pe::scan(t));
    let auto_verbs = scan.as_ref().map(|s| match s {
        Ok(s) => verbs::for_scan(s),
        Err(e) => {
            emit(&ProgressEvent::Warn {
                message: format!("import scan failed ({e}); no automatic winetricks verbs"),
            });
            Vec::new()
        }
    });
    let renderer = scan
        .as_ref()
        .map(|s| select_renderer(gogid.as_deref(), s, opts.renderer, emit));
    let renderer_setup = match (&renderer, &proton, &opts.game_tree) {
        (Some(sel), Some(b), Some(tree)) => Some(renderer_plan(sel, tree, &b.dir)),
        _ => None,
    };

    // The umu id and the fixes need the build's protonfixes.
    let mut umu_id = None;
    let mut summary = None;
    if let (Some(id), Some(b)) = (&gogid, &proton) {
        let mut warnings = Vec::new();
        let u = setup::umu_id_for(id, &b.dir, opts.refresh, &mut warnings);
        let names = verbs::names(auto_verbs.as_deref().unwrap_or_default());
        let fx = setup::fixes_for(
            id,
            &b.dir,
            &u.id,
            summary_layers(id, &names, renderer.as_ref()),
            &mut warnings,
        );
        for message in warnings {
            emit(&ProgressEvent::Warn { message });
        }
        summary = Some(fx.summary());
        umu_id = Some(u);
    }

    let dlcs = match (&opts.game_tree, &opts.depot_content) {
        (Some(_), Some(c)) => c.dlcs.clone(),
        (Some(t), None) => content::for_build(content::Lane::Depot, t, None, None).refs(),
        (None, _) => opts.installer_dlc.refs.clone(),
    };
    Ok(LinuxAppPlan {
        installer_dlc: opts.installer_dlc.clone(),
        snapshots,
        snapshot_opts: opts.snapshots.clone(),
        dlcs,
        depot_content: opts.game_tree.as_ref().and(opts.depot_content.clone()),
        dlc_notes: opts.dlc_notes.clone(),
        display,
        title_fixed: opts.name.is_some(),
        slug,
        gogid,
        installer: install,
        game_tree: opts.game_tree.clone(),
        game_tree_bytes,
        state,
        library: opts.library.clone(),
        rebuild,
        umu_mode,
        umu_line,
        proton_ref,
        proton,
        proton_download,
        runtime_flag: opts.runtime.is_some(),
        runtime_line,
        container_line,
        compat,
        renderer_choice: opts.renderer,
        renderer,
        renderer_setup,
        auto_verbs,
        game_files,
        umu_id,
        fixes: summary,
        refresh: opts.refresh,
        gogo_path: opts
            .gogo_path
            .as_deref()
            .map(launch::absolute)
            .or_else(|| std::env::current_exe().ok())
            .unwrap_or_else(|| PathBuf::from("gogo")),
    })
}

/// The GOG registry key (design 15 D3) as a `REGEDIT4` file, for one
/// `reg import` instead of a Proton run per value. The 32-bit view of a
/// 64-bit Proton prefix is `Wow6432Node`.
fn gog_key_reg(id: &str, name: &str, win_path: &str, exe_file: &str) -> String {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let mut out = String::from("REGEDIT4\n\n");
    let values = crate::macapp::gog_game_key(id, name, win_path, exe_file);
    if let Some(first) = values.first() {
        let key = first.key.replacen(
            "HKLM\\Software\\",
            "HKEY_LOCAL_MACHINE\\Software\\Wow6432Node\\",
            1,
        );
        out.push_str(&format!("[{key}]\n"));
    }
    for v in &values {
        out.push_str(&format!(
            "\"{}\"=\"{}\"\n",
            esc(v.name.as_deref().unwrap_or_default()),
            esc(&v.data)
        ));
    }
    out
}

/// `games/<gogid>/gogo-launch.toml` from the installed game at
/// `install_dir` (`prefix/drive_c/GOG/<name>`): the entry point, its
/// variants and args (L2), the L1 verbs, the runtime and renderer. L3's
/// `exe`/`args`/`env`/`dll_overrides` aren't baked: setup applies them,
/// because the ownership rule (§4.3) depends on the Proton build.
fn launch_toml(
    plan: &LinuxAppPlan,
    gogid: &str,
    prefix: &Path,
    install_dir: &Path,
    runtime: &str,
    selection: &renderer::Selection,
    auto: Vec<String>,
) -> Result<LaunchToml, MacAppError> {
    let entry = info::find_entry_point(install_dir)?;
    let missing = || MacAppError::EntryPointNotFound {
        dir: install_dir.to_path_buf(),
    };
    let exe = launch::unix_to_windows(prefix, &entry).ok_or_else(missing)?;
    let tasks: Vec<(launch::Variant, bool)> = info::play_variants(install_dir)
        .into_iter()
        .filter_map(|v| {
            Some((
                launch::Variant {
                    exe: launch::unix_to_windows(prefix, &v.exe)?,
                    workdir: launch::unix_to_windows(prefix, &v.workdir)?,
                    name: v.name,
                    args: v.args,
                },
                v.is_primary,
            ))
        })
        .collect();
    let args = tasks
        .iter()
        .filter(|(v, _)| v.exe == exe)
        .min_by_key(|(_, primary)| !primary)
        .map(|(v, _)| v.args.clone())
        .unwrap_or_default();
    let workdir = exe
        .rsplit_once('\\')
        .map(|(head, _)| head.to_string())
        .unwrap_or_else(|| exe.clone());
    Ok(LaunchToml {
        schema: launch::SCHEMA,
        gogid: gogid.to_string(),
        title: plan.display.clone(),
        runtime: runtime.to_string(),
        renderer: selection.renderer.value.clone(),
        renderer_flag: selection.renderer.layer == Layer::Flags,
        exe,
        workdir,
        args,
        env: Default::default(),
        dll_overrides: Default::default(),
        verbs: auto,
        portable: None,
        gogo: launch::GogoRef {
            path: plan.gogo_path.clone(),
            state: crate::state::data_root(),
        },
        variants: tasks.into_iter().map(|(v, _)| v).collect(),
        lane: plan.installer.is_some().then_some(compat::Lane::Installer),
    })
}

/// Flags > L4 (#110, design 16 decision 5) on Linux: a `--renderer` /
/// `--runtime` that won is written into `[platforms.linux]` of the L4 file
/// where that file already sets the key; else it's only baked.
fn persist_flags(
    plan: &LinuxAppPlan,
    gogid: &str,
    selection: &renderer::Selection,
    runtime: &str,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<(), MacAppError> {
    let l4 = compat::load_user_for(gogid, Os::Linux)?.entry;
    let mut updates = Vec::new();
    if l4.renderer.is_some()
        && matches!(plan.renderer_choice, RendererChoice::Force(_))
        && selection.renderer.layer == Layer::Flags
    {
        updates.push(("renderer", Some(selection.renderer.value.as_str())));
    }
    if l4.runtime.is_some() && plan.runtime_flag {
        updates.push(("runtime", Some(runtime)));
    }
    if updates.is_empty() {
        return Ok(());
    }
    for c in compat::set_user_keys_for(gogid, &updates, Os::Linux)? {
        emit(&ProgressEvent::UserOverrideSet {
            gogid: gogid.to_string(),
            key: c.key,
            old: c.old,
            new: c.new,
            path: compat::user_path(gogid),
        });
    }
    Ok(())
}

/// umu running `plan`'s setup with the installer lane's Inno flags (as on
/// the Mac, design 10 §2.4): `dlc_installer::setup_command`.
fn installer_command(
    umu: &Umu,
    plan: &installer::InstallPlan,
    win_target: &str,
    log: &Path,
) -> Result<(std::process::Command, super::dlc_installer::SetupLink), MacAppError> {
    super::dlc_installer::setup_command(umu, &plan.installer, win_target, log)
}

/// Run the plan: umu and GE-Proton (the downloads the plan named), the game
/// files, `gogo-launch.toml`, then setup.
pub fn execute(
    plan: &LinuxAppPlan,
    prompt: Option<&PromptFn>,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<BuildOutcome, MacAppError> {
    // A rebuild of a game whose snapshot rollback stopped settles it first
    // (design 20 §5.3.1).
    if let Some(id) = &plan.gogid {
        for message in super::rollback::settle_pending(id, &plan.snapshot_opts.backends)? {
            emit(&ProgressEvent::Warn { message });
        }
    }
    let umu_run = setup::ensure_umu(prompt, emit)?;
    let build = match (&plan.proton, &plan.proton_download) {
        (Some(b), _) => b.clone(),
        (None, Some(d)) => {
            let entry = catalog::find(&d.id)
                .ok_or_else(|| MacAppError::Proton(format!("{} is not in gogo's catalog", d.id)))?;
            let http = catalog::http_client()?;
            let m = lutris::block_on_detached(store::install_to(
                entry,
                Some(&d.version),
                &http,
                catalog::GITHUB_API,
                false,
                d.shared.as_deref(),
                emit,
            ))?;
            ProtonBuild {
                reference: format!("{}/{}", m.id, m.version),
                dir: store::proton_dir(&m),
                managed: true,
            }
        }
        (None, None) => unreachable!("the plan has a build or a download"),
    };

    let mut materialized = None;
    let mut title = plan.display.clone();
    // The installer lane knows whether this is the game's first build only
    // once the install has named the game.
    let mut installer_first = false;
    // A rebuild's `before rebuild` snapshot and what retention pruned.
    let mut rebuilt: Option<snapshots::Taken> = None;
    // What the DLC installers left (the installer lane).
    let mut dlc_ran: Vec<super::dlc_installer::Installed> = Vec::new();
    let (gogid, selection, auto, lane, install_dir, cache_root) = if let Some(tree) =
        &plan.game_tree
    {
        let gogid = plan.gogid.clone().expect("the depot lane knows its id");
        let dirs = &plan.state;
        // The depot cache root the tree sits in, when its record is there.
        let cache_root = tree
            .parent()
            .filter(|r| depot::read_record(r).is_some())
            .map(Path::to_path_buf);
        let name = tree
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| plan.slug.clone());
        // A rebuild's `auto` snapshot (design 20 §7.1), before its first
        // write to the game dir.
        if plan.rebuild {
            rebuilt = before_rebuild(plan, &gogid, dirs, emit)?;
        }
        // A new game dir is a subvolume with snapshots on (design 20 §2.1).
        snapshots::make_dir(
            &plan.snapshots,
            &plan.snapshot_opts,
            &dirs.root,
            &mut |message| emit(&ProgressEvent::Warn { message }),
        )?;
        launch::record_location(&gogid, &dirs.root)?;
        // A rebuild re-materializes `game/` and keeps the prefix (saves).
        // A copy of a large tree takes minutes: say so, and how far (#337).
        let method = plan
            .game_files
            .as_ref()
            .map_or(materialize::TreeMethod::Copy, |p| p.method);
        let total = plan
            .game_files
            .as_ref()
            .map(|p| p.extra_bytes)
            .filter(|b| *b > 0)
            .unwrap_or_else(|| tree_bytes(tree));
        emit(&ProgressEvent::GameFilesStarted {
            method: method.as_str().into(),
            bytes: total,
            dir: dirs.game.clone(),
            kind: None,
            files: None,
        });
        if dirs.game.symlink_metadata().is_ok() {
            working(emit, step::REMOVING_GAME, || {
                materialize::remove_tree(&dirs.game)
            })?;
        }
        let mut tenths = materialize::Tenths::new(total);
        // Only the base and the wanted DLC (design 19 §3.3): the filter's paths are
        // relative to the cache root, the tree's parent.
        let tree_name = PathBuf::from(tree.file_name().unwrap_or_default());
        let content_filter = plan.depot_content.clone().unwrap_or_default();
        let (mut files_done, mut bytes_done) = (0u64, 0u64);
        let report = materialize::materialize_tree_filtered(
            tree,
            &dirs.game,
            materialize::Reflink::Try,
            &|rel| content_filter.keeps(&tree_name.join(rel)),
            &mut |_, bytes| {
                files_done += 1;
                bytes_done += bytes;
                emit(&ProgressEvent::WorkProgress {
                    step: step::GAME_FILES.into(),
                    files: Some(files_done),
                    files_total: None,
                    bytes: Some(bytes_done),
                    bytes_total: Some(total),
                });
                if method == materialize::TreeMethod::Copy
                    && let Some(copied) = tenths.add(bytes)
                {
                    emit(&ProgressEvent::GameFilesProgress { copied, total });
                }
                Ok(())
            },
        );
        emit(&ProgressEvent::WorkDone {
            step: step::GAME_FILES.into(),
        });
        let report = report?;
        if launch::in_library(&dirs.root) && report.method() == materialize::TreeMethod::Copy {
            emit(&ProgressEvent::Warn {
                message: format!(
                    "the game files were copied into {}, not reflinked ({} more on disk): \
                     {} and the library aren't on one filesystem that can reflink",
                    dirs.game.display(),
                    crate::fmt::human_size(report.copied_bytes),
                    tree.display()
                ),
            });
        }
        if report.left_out_files > 0
            && let Some(f) = &content_filter.filter
        {
            let what: Vec<String> = [
                (!f.dropped.is_empty())
                    .then(|| format!("DLC {}", crate::gog::dlc_filter::names(&f.dropped))),
                (!f.languages.is_empty()).then(|| format!("languages {}", f.languages.join(", "))),
            ]
            .into_iter()
            .flatten()
            .collect();
            emit(&ProgressEvent::Warn {
                message: format!(
                    "left the depot cache's other {} out of the game dir ({} file(s), {})",
                    what.join(" and "),
                    report.left_out_files,
                    crate::fmt::human_size(report.left_out_bytes)
                ),
            });
        }
        for overlay in crate::macapp::locate_support_overlays(tree) {
            // `gog-support/<base>/<product>/app`: a left-out DLC's is skipped whole.
            let product = overlay
                .parent()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned());
            if content_filter.filter.as_ref().is_some_and(|f| {
                f.dropped
                    .iter()
                    .any(|d| product.as_deref() == Some(d.id.to_string().as_str()))
            }) {
                continue;
            }
            let cache_root = tree.parent().unwrap_or(tree);
            let at = overlay
                .strip_prefix(cache_root)
                .unwrap_or(&overlay)
                .to_path_buf();
            let merged = crate::macapp::bundle::merge_overlay_with(&overlay, &dirs.game, &|rel| {
                content_filter.keeps(&at.join(rel))
            })?;
            if !merged.is_empty() {
                emit(&ProgressEvent::Warn {
                    message: format!(
                        "merged GOG support overlay into the game dir: {} file(s)",
                        merged.len()
                    ),
                });
            }
        }
        materialized = Some(report);

        let mut warnings = Vec::new();
        let id = setup::umu_id_for(&gogid, &build.dir, false, &mut warnings);
        for message in warnings {
            emit(&ProgressEvent::Warn { message });
        }
        let umu = Umu {
            run: &umu_run,
            proton: &build.dir,
            prefix: &dirs.prefix,
            umu_id: &id.id,
            cwd: &dirs.root,
            log: dirs.root.join(setup::LOG_DIR).join(setup::SETUP_LOG),
        };
        umu.check()?;
        emit(&ProgressEvent::PrefixStaged);
        if setup::is_prefix(&dirs.prefix) {
            emit(&ProgressEvent::Warn {
                message: format!(
                    "reusing the existing prefix {} (saves kept)",
                    dirs.prefix.display()
                ),
            });
        } else {
            working(emit, step::PREFIX, || umu.create_prefix())?;
        }
        crate::macapp::setup::localize_home(&dirs.prefix)?;
        let install_dir = crate::macapp::setup::link_game(dirs, &name)?;

        // GOG's support installer (design 15 D3): best-effort, as on the Mac.
        if let Some(support) = crate::macapp::locate_support_installer(tree) {
            emit(&ProgressEvent::WorkStarted {
                step: step::SUPPORT.into(),
            });
            let r =
                restore_and_run_support(plan, &umu, &support, &gogid, &name, &install_dir, emit);
            emit(&ProgressEvent::WorkDone {
                step: step::SUPPORT.into(),
            });
            r?;
        }
        emit(&ProgressEvent::InstallFinished {
            bytes: plan.game_tree_bytes,
        });
        let sel = plan
            .renderer
            .clone()
            .expect("the depot lane plans its renderer");
        let auto = verbs::names(plan.auto_verbs.as_deref().unwrap_or_default());
        let toml = launch_toml(
            plan,
            &gogid,
            &dirs.prefix,
            &install_dir,
            &build.reference,
            &sel,
            auto.clone(),
        )?;
        launch::write_atomic(&game::launch_toml_path(dirs), &toml.to_toml())?;
        (
            gogid,
            sel,
            auto,
            content::Lane::Depot,
            dirs.game.clone(),
            cache_root,
        )
    } else {
        let inst = plan.installer.as_ref().expect("one source");
        let staging = &plan.state;
        let logs = staging.root.join(crate::macapp::INSTALL_LOGS);
        let umu = Umu {
            run: &umu_run,
            proton: &build.dir,
            prefix: &staging.prefix,
            umu_id: "umu-0",
            cwd: &staging.root,
            log: logs.join(installer::WINE_LOG),
        };
        umu.check()?;
        // The staging dir is the game dir to be: a subvolume with snapshots on
        // (design 20 §2.1), renamed into place below.
        snapshots::make_dir(
            &plan.snapshots,
            &plan.snapshot_opts,
            &staging.root,
            &mut |message| emit(&ProgressEvent::Warn { message }),
        )?;
        emit(&ProgressEvent::PrefixStaged);
        working(emit, step::PREFIX, || umu.create_prefix())?;
        crate::macapp::setup::localize_home(&staging.prefix)?;
        restore_z_for_build(&staging.prefix)?;
        emit(&ProgressEvent::InstallStarted);
        let win_target = format!("C:\\GOG\\{}", plan.slug);
        let inno_log = logs.join(installer::INNO_LOG);
        let (cmd, link) = installer_command(&umu, inst, &win_target, &inno_log)?;
        let status = working(emit, step::INSTALLER, || {
            umu.run("the offline installer", cmd)
        })?;
        drop(link);
        let install_dir = staging.prefix.join("drive_c/GOG").join(&plan.slug);
        if !status.success() {
            return Err(MacAppError::InstallerFailed {
                exit: status.code().unwrap_or(-1),
                bundle: staging.root.clone(),
                tail: installer::log_tail(&inno_log, 8),
                logs,
            });
        }
        emit(&ProgressEvent::InstallFinished {
            bytes: tree_bytes(&inst.installer)
                + inst.bin_parts.iter().map(|p| tree_bytes(p)).sum::<u64>(),
        });
        let gogid =
            info::root_game_id(&install_dir).ok_or_else(|| MacAppError::EntryPointNotFound {
                dir: install_dir.clone(),
            })?;
        // The DLC's installers (design 19 §5): after the base, one at a time, in
        // the same staging prefix and into its dir. A failed one fails the build
        // here, before the prefix moves, so no half-built game appears.
        let dlc = &plan.installer_dlc;
        if !dlc.installers.is_empty() {
            if let Some(want) = &dlc.gogid
                && *want != gogid
            {
                return Err(MacAppError::Apps(format!(
                    "the setup installed game {gogid}, not {want} whose DLC were chosen; \
                     nothing was moved into place (staging: {})",
                    staging.root.display()
                )));
            }
            for d in &dlc.installers {
                dlc_ran.push(super::dlc_installer::run_one(
                    &umu,
                    d,
                    &win_target,
                    &install_dir,
                    &logs,
                    emit,
                )?);
            }
        }
        let entry = info::find_entry_point(&install_dir)?;
        let scan = pe::scan_exe(&install_dir, &entry);
        let sel = select_renderer(Some(&gogid), &scan, plan.renderer_choice, emit);
        let auto = verbs::names(&match &scan {
            Ok(s) => verbs::for_scan(s),
            Err(_) => Vec::new(),
        });

        // Into place: an older prefix for the game is set aside, never deleted.
        let target = GameDirs::for_build(&gogid, plan.library.as_ref())?;
        if let Some(message) = launch::stays_note(&target, plan.library.as_ref()) {
            emit(&ProgressEvent::Warn { message });
        }
        installer_first = !game::launch_toml_path(&target).is_file();
        if target.root.symlink_metadata().is_err() {
            // A new game: the staging dir (a subvolume, with snapshots on)
            // becomes the game dir, prefix and install logs in it.
            if let Some(parent) = target.root.parent() {
                std::fs::create_dir_all(parent)?;
            }
            working(emit, step::MOVING_GAME, || {
                launch::move_tree(&staging.root, &target.root)
            })?;
            launch::record_location(&gogid, &target.root)?;
        } else {
            for message in super::rollback::settle_pending(&gogid, &plan.snapshot_opts.backends)? {
                emit(&ProgressEvent::Warn { message });
            }
            // A rebuild's `auto` snapshot (design 20 §7.1): the install ran
            // in the staging dir; nothing in the game dir is written yet.
            if !installer_first {
                rebuilt = before_rebuild(plan, &gogid, &target, emit)?;
            }
            launch::record_location(&gogid, &target.root)?;
            if target.prefix.symlink_metadata().is_ok() {
                let aside = target.root.join(format!(
                    "prefix.old-{}",
                    chrono::Utc::now().format("%Y%m%dT%H%M%S")
                ));
                std::fs::rename(&target.prefix, &aside)?;
                emit(&ProgressEvent::Warn {
                    message: format!("previous prefix moved to {}", aside.display()),
                });
            }
            // Out of a staging subvolume into another one is a copy (reflinked
            // on one btrfs): a rename can't cross subvolumes.
            working(emit, step::MOVING_PREFIX, || {
                launch::move_tree(&staging.prefix, &target.prefix)
            })?;
            let target_logs = target.root.join(crate::macapp::INSTALL_LOGS);
            if logs.is_dir() {
                materialize::remove_tree(&target_logs).ok();
                launch::move_tree(&logs, &target_logs)?;
            }
            materialize::remove_tree(&staging.root).ok();
        }
        let install_dir = target.prefix.join("drive_c/GOG").join(&plan.slug);
        // The stem only guessed the title (the slug had to be fixed before the
        // install); the installed game's own name is the real one (#235).
        title = if plan.title_fixed {
            plan.display.clone()
        } else {
            info::game_name(&install_dir).unwrap_or_else(|| plan.display.clone())
        };
        let mut toml = launch_toml(
            plan,
            &gogid,
            &target.prefix,
            &install_dir,
            &build.reference,
            &sel,
            auto.clone(),
        )?;
        toml.title = title.clone();
        launch::write_atomic(&game::launch_toml_path(&target), &toml.to_toml())?;
        (
            gogid,
            sel,
            auto,
            content::Lane::Installer,
            install_dir,
            None,
        )
    };
    let _ = auto;

    persist_flags(plan, &gogid, &selection, &build.reference, emit)?;
    let mut outcome = setup::setup(
        &gogid,
        &SetupOptions {
            // The plan's lookup already refreshed when asked.
            refresh: plan.refresh && plan.umu_id.is_none(),
            prompt,
            // A rebuild took `before rebuild` already; a first build has
            // nothing to protect.
            runtime_snapshot: None,
        },
        emit,
    )?;
    // The last step: a game with no `content.toml` reads as derived (design 19 §6.1).
    let installed = (lane == content::Lane::Depot)
        .then_some(plan.depot_content.as_ref())
        .flatten()
        .map(|c| c.dlcs.as_slice());
    let mut content = content::for_build(lane, &install_dir, cache_root.as_deref(), installed);
    // The build's language, not the cache record's last one (#389).
    if lane == content::Lane::Depot
        && let Some(l) = plan.depot_content.as_ref().and_then(|c| c.language.clone())
    {
        content.language = Some(l);
    }
    // The build's version name (#418), when it is for the build the record names.
    if lane == content::Lane::Depot
        && let Some((id, v)) = plan.depot_content.as_ref().and_then(|c| c.version.clone())
        && content.build_id.as_deref() == Some(id.as_str())
    {
        content.version = Some(v);
    }
    // The installer lane (design 19 §5, §6.1): the base installer's version label
    // and language, and each DLC's installer label and info build.
    if lane == content::Lane::Installer {
        let d = &plan.installer_dlc;
        content.version = d.version.clone();
        content.language = d.language.clone();
        super::dlc_installer::record(&mut content, &d.installers, &dlc_ran);
    }
    if lane == content::Lane::Depot
        && let Some((id, b)) = plan.depot_content.as_ref().and_then(|c| c.branch.clone())
        && content.build_id.as_deref() == Some(id.as_str())
    {
        content.branch = Some(b);
    }
    let dirs = GameDirs::for_game(&gogid)?;
    // The build's manifest goes first, so `content.toml` never names a build whose
    // manifest was dropped (design 21 §5.2); the depot lane only.
    if lane == content::Lane::Depot
        && let Some(kept) = plan
            .depot_content
            .as_ref()
            .and_then(|c| c.manifest.as_ref())
        && let Some(w) =
            crate::gog::build_manifest::write_or_warn(&dirs.root, content.build_id.as_deref(), kept)
    {
        outcome.warnings.push(w);
    }
    if let Some(w) = write_content_or_warn(&dirs.root, &content) {
        outcome.warnings.push(w);
    }
    // A first build's setting and `initial` snapshot (design 20 §7.1). The
    // game is built and plays either way: a failure warns.
    let first = if plan.game_tree.is_some() {
        !plan.rebuild
    } else {
        installer_first
    };
    let snapshot = if first {
        snapshots::after_first_build(&gogid, &dirs, &plan.snapshots, &plan.snapshot_opts)
            .unwrap_or_else(|e| {
                outcome.warnings.push(format!(
                    "no initial snapshot: {e}; `gogo linux-app snapshot {gogid} create` takes one"
                ));
                None
            })
    } else {
        None
    };
    let (snapshot, pruned) = match rebuilt {
        Some(t) => (Some(t.meta), t.pruned),
        None => (snapshot, Vec::new()),
    };
    Ok(BuildOutcome {
        snapshot,
        pruned,
        content,
        title,
        dirs,
        gogid,
        materialized,
        setup: outcome,
    })
}

/// A rebuild's `before rebuild` snapshot (design 20 §7.1), under the two
/// locks for the snapshot itself. A failure stops the rebuild before its
/// first write to the game dir.
fn before_rebuild(
    plan: &LinuxAppPlan,
    gogid: &str,
    dirs: &GameDirs,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<Option<snapshots::Taken>, MacAppError> {
    snapshots::protect(
        gogid,
        dirs,
        None,
        crate::snapshot::Trigger::Rebuild,
        crate::snapshot::labels::rebuild(),
        &plan.snapshot_opts.auto(),
        emit,
    )
}

/// Write `content.toml`; a failure warns, because the finished build is playable
/// and a game with no file reads as derived (design 19 §6.1).
fn write_content_or_warn(root: &Path, content: &content::Content) -> Option<String> {
    content::write(root, content).err().map(|e| {
        format!(
            "couldn't write {}: {e}; the game works, its content is read from its files",
            content::path(root).display()
        )
    })
}

/// A Proton prefix maps `z:` to `/`; one set up before setup kept it (#176)
/// lost it. Build steps hand Wine unix paths (installer logs).
fn restore_z_for_build(prefix: &Path) -> Result<(), MacAppError> {
    setup::restore_z(prefix).map(|_| ())
}

/// How long the best-effort support installer gets. It writes a few config
/// files; with no display, Zeus's sat idle for good.
const SUPPORT_INSTALLER_LIMIT: Duration = Duration::from_secs(180);

/// The depot lane's GOG support installer: the GOG.com key it checks for
/// (one `reg import`), then the installer into the game dir. A failure is
/// a warning, as on the Mac (design 15 D3 amendment).
fn restore_and_run_support(
    plan: &LinuxAppPlan,
    umu: &Umu,
    support: &Path,
    gogid: &str,
    name: &str,
    install_dir: &Path,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<(), MacAppError> {
    restore_z_for_build(umu.prefix)?;
    let logs = plan
        .state
        .root
        .join(crate::macapp::INSTALL_LOGS)
        .join("support");
    std::fs::create_dir_all(&logs)?;
    let win_path = format!("C:\\GOG\\{name}");
    let exe_file = info::find_entry_point(install_dir)
        .ok()
        .and_then(|e| e.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "game.exe".to_string());
    let reg = logs.join("gog-key.reg");
    std::fs::write(&reg, gog_key_reg(gogid, name, &win_path, &exe_file))?;
    let imported = umu.run(
        "the GOG.com registry key",
        umu.command(["reg".to_string(), "import".into(), installer::z_path(&reg)]),
    )?;
    if !imported.success() {
        emit(&ProgressEvent::Warn {
            message: format!(
                "reg import of the GOG.com key exited {}",
                imported.code().unwrap_or(-1)
            ),
        });
    }
    emit(&ProgressEvent::Warn {
        message: "running GOG support installer into the game dir".into(),
    });
    let support_plan = installer::plan(support)?;
    let (cmd, link) = installer_command(
        umu,
        &support_plan,
        &win_path,
        &logs.join(installer::INNO_LOG),
    )?;
    let ran = umu.run_limited("GOG's support installer", cmd, SUPPORT_INSTALLER_LIMIT)?;
    drop(link);
    match ran {
        Some(status) if status.success() => emit(&ProgressEvent::Warn {
            message: format!(
                "GOG support installer finished (exit {})",
                status.code().unwrap_or(0)
            ),
        }),
        Some(status) => emit(&ProgressEvent::Warn {
            message: format!(
                "GOG support installer declined (exit {}; install-time config not \
                 written; logs in {} and {}) — if the game stalls at launch, build it \
                 from the offline installer",
                status.code().unwrap_or(-1),
                logs.display(),
                umu.log.display()
            ),
        }),
        None => emit(&ProgressEvent::Warn {
            message: format!(
                "GOG support installer still running after {} s, stopped (install-time \
                 config not written; logs in {} and {}) — if the game stalls at launch, \
                 build it from the offline installer",
                SUPPORT_INSTALLER_LIMIT.as_secs(),
                logs.display(),
                umu.log.display()
            ),
        }),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macapp::renderer::PROTON_USE_WINED3D;

    #[test]
    fn a_failed_content_write_warns_instead_of_failing() {
        let d = tempfile::tempdir().unwrap();
        let c = content::for_build(content::Lane::Depot, d.path(), None, None);
        assert!(write_content_or_warn(d.path(), &c).is_none());
        let gone = d.path().join("no/such/dir");
        let w = write_content_or_warn(&gone, &c).expect("a warning");
        assert!(
            w.contains("content.toml") && w.contains("the game works"),
            "{w}"
        );
    }

    #[test]
    fn gog_key_reg_is_regedit4_in_the_32_bit_view() {
        let reg = gog_key_reg(
            "1207659046",
            "Pharaoh \"Gold\"",
            r"C:\GOG\Pharaoh",
            "Pharaoh.exe",
        );
        assert!(reg.starts_with(
            "REGEDIT4\n\n[HKEY_LOCAL_MACHINE\\Software\\Wow6432Node\\GOG.com\\Games\\1207659046]\n"
        ));
        assert!(reg.contains("\"gameID\"=\"1207659046\"\n"), "{reg}");
        assert!(
            reg.contains("\"GAMENAME\"=\"Pharaoh \\\"Gold\\\"\"\n"),
            "{reg}"
        );
        assert!(
            reg.contains("\"EXE\"=\"C:\\\\GOG\\\\Pharaoh\\\\Pharaoh.exe\"\n"),
            "{reg}"
        );
    }

    #[test]
    fn switches_say_which_wined3d() {
        let p = |env: &[(&str, &str)], ddraw| LinuxPlan {
            renderer: "wined3d".into(),
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            dll_overrides: Default::default(),
            ddraw,
            stand_in: None,
            notes: vec![],
            warnings: vec![],
        };
        assert_eq!(
            switches(&p(&[], renderer::DdrawStep::None)),
            "nothing set: Proton's defaults"
        );
        assert_eq!(
            switches(&p(
                &[(PROTON_USE_WINED3D, "1")],
                renderer::DdrawStep::RemoveGogo
            )),
            "PROTON_USE_WINED3D=1"
        );
        assert_eq!(
            switches(&p(&[], renderer::DdrawStep::PlaceCncDdraw)),
            "cnc-ddraw's ddraw.dll in the game dir, ddraw=n,b"
        );
        let mut xdd = p(&[], renderer::DdrawStep::PlaceCncDdraw);
        xdd.stand_in = Some("xdd.dll".into());
        assert_eq!(
            switches(&xdd),
            "cnc-ddraw's ddraw.dll in the game dir as xdd.dll, in place of the game's DDrawCompat"
        );
    }

    #[test]
    fn installer_args_log_through_z() {
        let plan = installer::InstallPlan {
            installer: "/home/me/Downloads/setup_x.exe".into(),
            bin_parts: vec![],
        };
        let args = super::super::dlc_installer::inno_args(
            &plan.installer.display().to_string(),
            r"C:\GOG\x",
            Path::new("/home/me/g/install/inno.log"),
        );
        assert_eq!(args[0], "/home/me/Downloads/setup_x.exe");
        assert!(args.contains(&r"/DIR=C:\GOG\x".to_string()));
        assert!(args.contains(&r"/LOG=Z:\home\me\g\install\inno.log".to_string()));
    }

    #[test]
    fn container_follows_the_build_appid() {
        assert_eq!(container_for(None), ("steamrt4", "steamrt4"));
        let tmp = tempfile::tempdir().unwrap();
        crate::linuxapp::proton::tests::fake_build(tmp.path(), "GE-Proton9-1", Some("1628350"));
        assert_eq!(container_for(Some(tmp.path())), ("sniper", "steamrt3"));
    }
}
