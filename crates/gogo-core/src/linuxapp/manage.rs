//! A built Linux game after the build (design 18 §10): `gogo linux-app
//! compat`, `next-renderer` and `--retarget`, the siblings of the Mac's in
//! `macapp::setup`. They read `games/<gogid>/gogo-launch.toml`, the Linux
//! layers (`[platforms.linux]` of L3 and L4) and `state.toml`; the two that
//! change anything write L4, never the baked TOML.

use std::path::PathBuf;

use serde::Serialize;

use super::game::{self, LinuxState};
use super::proton::ProtonRef;
use super::setup::{self, SetupOptions, SetupOutcome};
use crate::engine::events::ProgressEvent;
use crate::macapp::compat::{self, CompatEntry, Effective, Layer, Os};
use crate::macapp::error::MacAppError;
use crate::macapp::launch::{GameDirs, LaunchToml, windows_to_unix};
use crate::macapp::pe;
use crate::macapp::renderer::{self, RendererRef};

/// A built game: its dirs, the baked TOML, and what setup last applied.
struct Built {
    dirs: GameDirs,
    baked: LaunchToml,
    state: Option<LinuxState>,
}

fn built(gogid: &str) -> Result<Built, MacAppError> {
    let dirs = GameDirs::for_game(gogid)?;
    let (baked, _) = game::read_launch_toml(&dirs)?;
    let state = LinuxState::load(&dirs.state_toml)?;
    Ok(Built { dirs, baked, state })
}

/// The game as installed, scanned as setup scans it (the entry point when
/// it's there, else the tree): the game dir in the prefix
/// (`drive_c/GOG/<name>`, when it's there) and the scan.
pub fn scan_installed(
    dirs: &GameDirs,
    baked: &LaunchToml,
) -> (Option<PathBuf>, Result<pe::PeScan, pe::ScanError>) {
    let dir = baked
        .tree_name()
        .map(|n| dirs.prefix.join("drive_c/GOG").join(n))
        .filter(|d| d.is_dir());
    let exe = windows_to_unix(&dirs.prefix, &baked.exe).filter(|e| e.is_file());
    let scan = match (&dir, &exe) {
        (Some(d), Some(e)) => pe::scan_exe(d, e),
        (Some(d), None) => pe::scan(d),
        _ => Err(pe::ScanError::NoExecutable {
            dir: dirs.game.clone(),
        }),
    };
    (dir, scan)
}

/// L1 for the renderer from the game as installed ([`scan_installed`]),
/// and the dir it scanned.
fn l1(b: &Built) -> (Option<PathBuf>, renderer::Pick) {
    let (dir, scan) = scan_installed(&b.dirs, &b.baked);
    (dir, renderer::pick_linux(&scan))
}

/// `gogo linux-app next-renderer <gogid>`: the renderer after the one the
/// last setup applied, in this game's L1 order (design 18 §3, #172's walk).
/// Unless `dry_run`, it's written to L4 (`[platforms.linux] renderer`), so
/// the next launch sees a stale stamp and setup applies it. `Ok(None)`: no
/// more renderers.
pub fn next_renderer(gogid: &str, dry_run: bool) -> Result<Option<String>, MacAppError> {
    let b = built(gogid)?;
    let current = b
        .state
        .as_ref()
        .map(|s| s.renderer.clone())
        .unwrap_or_else(|| b.baked.renderer.clone());
    let (_, pick) = l1(&b);
    renderer::next_renderer_on(Os::Linux, gogid, &pick, &current, dry_run)
}

/// What [`retarget`] did.
#[derive(Debug)]
pub struct RetargetOutcome {
    pub gogid: String,
    pub changes: Vec<compat::Change>,
    /// Setup on the new values, when the game is built and something
    /// changed. A failure leaves L4 written: the next launch tries again.
    pub setup: Option<Result<SetupOutcome, MacAppError>>,
}

/// `gogo linux-app --retarget <gogid> --runtime <ref> --renderer <id>`
/// (design 16 decision 7 on Linux): write `runtime` / `renderer` (`None`
/// drops a key) into L4's `[platforms.linux]`, then set the game up on them
/// (a missing GE-Proton installed on the way). The baked TOML stays.
pub fn retarget(
    gogid: &str,
    updates: &[(&str, Option<&str>)],
    opts: &SetupOptions,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> Result<RetargetOutcome, MacAppError> {
    compat::validate_gogid(gogid)?;
    for (key, value) in updates {
        let Some(v) = value else { continue };
        match *key {
            "runtime" => {
                ProtonRef::parse(v).map_err(MacAppError::Proton)?;
            }
            "renderer" => {
                RendererRef::parse_on(v, Os::Linux).map_err(|reason| MacAppError::Compat {
                    path: compat::user_path(gogid),
                    reason,
                })?;
            }
            _ => {}
        }
    }
    let changes = compat::set_user_keys_for(gogid, updates, Os::Linux)?;
    let dirs = GameDirs::for_game(gogid)?;
    let setup = (!changes.is_empty() && game::launch_toml_path(&dirs).is_file())
        .then(|| setup::setup(gogid, opts, emit));
    Ok(RetargetOutcome {
        gogid: gogid.to_string(),
        changes,
        setup,
    })
}

/// One `[[variant]]` of the baked TOML.
#[derive(Debug, Clone, Serialize)]
pub struct VariantLine {
    pub name: String,
    pub exe: String,
    pub args: Vec<String>,
    /// The TOML's own entry point.
    pub default: bool,
    /// What setup runs: L4's `variant`, else the default.
    pub selected: bool,
}

/// The renderer: what the build baked and from which layer, what setup
/// applies (L4 over the baked one) and what it last applied.
#[derive(Debug, Clone, Serialize)]
pub struct RendererLine {
    pub baked: String,
    pub baked_layer: Layer,
    /// L1's pick and why, from the game as installed.
    pub auto: String,
    pub reason: String,
    pub effective: String,
    pub effective_layer: Layer,
    /// `state.toml`'s, after any fallback.
    pub applied: Option<String>,
}

/// `gogo linux-app compat <gogid>`.
#[derive(Debug, Clone, Serialize)]
pub struct CompatReport {
    pub gogid: String,
    pub user_file: PathBuf,
    /// L1 (the baked verbs), L3 for Linux under the ownership rule when the
    /// game's Proton build is here, L4.
    pub effective: Effective,
    pub status_line: String,
    pub built: bool,
    pub renderer: Option<RendererLine>,
    /// The runtime setup uses: L4, else the baked one.
    pub runtime: Option<String>,
    /// The fix summary (design 18 §4.3), when the Proton build is here.
    pub fixes: Option<String>,
    /// gogo's fixes a protonfix overrides at launch (O2), one line each.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collisions: Vec<String>,
    /// The summary itself, for callers that need its items (`env list`).
    #[serde(skip)]
    pub summary: Option<super::fixes::Summary>,
    pub variants: Vec<VariantLine>,
    pub variant_error: Option<String>,
    pub warnings: Vec<String>,
}

/// The game's effective compat config on Linux and where each value comes
/// from. Reads only; nothing is installed or looked up online.
pub fn compat(gogid: &str) -> Result<CompatReport, MacAppError> {
    compat::validate_gogid(gogid)?;
    let user_file = compat::user_path(gogid);
    let db = compat::db_entry_for(gogid, Os::Linux);
    let l4 = compat::load_user_for(gogid, Os::Linux)?;
    let mut warnings: Vec<String> = db
        .warnings
        .iter()
        .map(|w| format!("compat DB: {w}"))
        .chain(
            l4.warnings
                .iter()
                .map(|w| format!("{}: {w}", user_file.display())),
        )
        .collect();
    let b = match built(gogid) {
        Ok(b) => Some(b),
        Err(MacAppError::Compat { path, .. }) if !path.exists() => None,
        Err(e) => return Err(e),
    };
    let Some(b) = b else {
        let effective = compat::merge(&[(Layer::Db, &db.entry), (Layer::User, &l4.entry)]);
        return Ok(CompatReport {
            gogid: gogid.to_string(),
            user_file,
            status_line: effective.status_line(),
            effective,
            built: false,
            renderer: None,
            runtime: l4.entry.runtime.clone(),
            fixes: None,
            collisions: vec![],
            summary: None,
            variants: vec![],
            variant_error: None,
            warnings,
        });
    };

    let (_, pick) = l1(&b);
    let baked_layer = setup::baked_renderer_layer(&b.baked.renderer, &pick, &db.entry);
    let (effective_renderer, effective_layer) = match &l4.entry.renderer {
        Some(r) => (r.clone(), Layer::User),
        None => (b.baked.renderer.clone(), baked_layer),
    };
    let renderer = RendererLine {
        baked: b.baked.renderer.clone(),
        baked_layer,
        auto: pick.renderer.default_ref(),
        reason: pick.reason.clone(),
        effective: effective_renderer.clone(),
        effective_layer,
        applied: b.state.as_ref().map(|s| s.renderer.clone()),
    };
    let runtime = l4
        .entry
        .runtime
        .clone()
        .unwrap_or_else(|| b.baked.runtime.clone());

    // The layers setup merges; with the build here, L3 under the ownership
    // rule and the fix summary, as setup applies them.
    let layers = vec![
        (
            Layer::Auto,
            CompatEntry {
                verbs: Some(b.baked.verbs.clone()),
                ..Default::default()
            },
        ),
        (
            effective_layer,
            CompatEntry {
                renderer: Some(effective_renderer),
                ..Default::default()
            },
        ),
        (Layer::User, l4.entry.clone()),
    ];
    let build = ProtonRef::parse(&runtime)
        .ok()
        .and_then(|r| setup::find_proton(&r).ok().flatten());
    let mut collisions = Vec::new();
    let mut full = None;
    let (effective, fixes) = match (&build, &b.state) {
        (Some(build), Some(state)) => {
            let fx = setup::fixes_for(gogid, &build.dir, &state.umu_id, layers, &mut warnings);
            let summary = fx.summary();
            collisions = summary.collisions();
            let text = summary.to_string();
            full = Some(summary);
            (fx.effective.clone(), Some(text))
        }
        _ => {
            let entries: Vec<(Layer, &CompatEntry)> = std::iter::once((Layer::Db, &db.entry))
                .chain(layers.iter().map(|(l, e)| (*l, e)))
                .collect();
            if build.is_none() {
                warnings.push(format!(
                    "{runtime} isn't here: L3 shown whole, without the fix summary \
                     (`gogo linux-app setup {gogid}` installs it)"
                ));
            }
            (compat::merge(&entries), None)
        }
    };

    let default = b.baked.default_variant().map(|v| v.name.clone());
    let mut variant_error = None;
    let selected = match effective.variant.as_ref() {
        Some(v) => match b.baked.select_variant(&v.value) {
            Ok(_) => Some(v.value.clone()),
            Err(e) => {
                variant_error = Some(e);
                None
            }
        },
        None => default.clone(),
    };
    let variants = b
        .baked
        .variants
        .iter()
        .map(|v| VariantLine {
            name: v.name.clone(),
            exe: v.exe.clone(),
            args: v.args.clone(),
            default: default.as_deref() == Some(v.name.as_str()),
            selected: selected.as_deref() == Some(v.name.as_str()),
        })
        .collect();
    Ok(CompatReport {
        gogid: gogid.to_string(),
        user_file,
        status_line: effective.status_line(),
        effective,
        built: true,
        renderer: Some(renderer),
        runtime: Some(runtime),
        fixes,
        collisions,
        summary: full,
        variants,
        variant_error,
        warnings,
    })
}
