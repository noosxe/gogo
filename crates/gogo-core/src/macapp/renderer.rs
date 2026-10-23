//! Renderers (#70, docs/19 §6.2, design 16 decisions 3 and 5): which
//! Direct3D/DirectDraw implementation a game runs on, picked automatically
//! from its PE imports (L1) and overridable by every higher layer.
//!
//! - **wined3d** is the engine itself and needs nothing.
//! - **cnc-ddraw** is game-local: `ddraw.dll` in the game dir plus
//!   `ddraw=n,b`.
//! - **DXMT** is an engine overlay: it replaces Wine builtins, so the game
//!   runs from a derived runtime (`store::derive`).
//! - **D3DMetal** (#71) is an engine overlay like DXMT, from Apple's Game
//!   Porting Toolkit, so it is used only with the user's consent
//!   ([`super::d3dmetal`], asked once). It is 64-bit only: the default for
//!   64-bit Direct3D 12, the alternative after DXMT for 64-bit D3D11, and
//!   never offered to a 32-bit game. Without consent it is skipped, and an
//!   explicit request for it falls back with a warning.
//!
//! The build bakes `flags > L3 > L2 > L1 > L0` into `gogo-launch.toml`;
//! `setup` applies L4 on top and installs what the result needs ([`apply`]).

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use super::compat::{self, CompatEntry, Layer, Sourced};
use super::error::MacAppError;
use super::pe::{self, Bitness, GraphicsApi, GraphicsSource, PeScan};
use super::runtime::Runtime;
use super::{catalog, d3dmetal, ddraw, lutris, store};

/// One renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Renderer {
    Wined3d,
    Dxmt,
    CncDdraw,
    D3dMetal,
}

impl Renderer {
    pub const ALL: [Renderer; 4] = [
        Renderer::Wined3d,
        Renderer::Dxmt,
        Renderer::CncDdraw,
        Renderer::D3dMetal,
    ];

    /// The id used in refs, flags and the compat schema.
    pub fn id(self) -> &'static str {
        match self {
            Renderer::Wined3d => "wined3d",
            Renderer::Dxmt => "dxmt",
            Renderer::CncDdraw => "cnc-ddraw",
            Renderer::D3dMetal => "d3dmetal",
        }
    }

    pub fn from_id(id: &str) -> Option<Renderer> {
        Renderer::ALL.into_iter().find(|r| r.id() == id)
    }

    /// The name in messages.
    pub fn name(self) -> &'static str {
        match self {
            Renderer::Wined3d => "wined3d",
            Renderer::Dxmt => "DXMT",
            Renderer::CncDdraw => "cnc-ddraw",
            Renderer::D3dMetal => "D3DMetal",
        }
    }

    /// The ref this renderer is written as when no version is asked for:
    /// `wined3d`, or `<id>/<catalog-pinned version>`.
    pub fn default_ref(self) -> String {
        match pinned_version(self.id()) {
            Some(v) => format!("{}/{v}", self.id()),
            None => self.id().to_string(),
        }
    }
}

/// The version the catalog pins for `id`, if it is a pinned catalog entry.
fn pinned_version(id: &str) -> Option<&'static str> {
    catalog::find(id)?.discovery.as_ref()?.pinned_version()
}

/// A renderer ref, `<id>[/<ver>]` (gogo-launch.toml `renderer`, compat
/// `renderer`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererRef {
    pub renderer: Renderer,
    pub version: Option<String>,
}

impl RendererRef {
    pub fn parse(s: &str) -> Result<RendererRef, String> {
        let (id, version) = match s.trim().split_once('/') {
            Some((id, v)) => (id, Some(v.to_string())),
            None => (s.trim(), None),
        };
        let renderer = Renderer::from_id(id).ok_or_else(|| {
            format!("unknown renderer `{s}` (wined3d, dxmt, cnc-ddraw, d3dmetal)")
        })?;
        Ok(RendererRef { renderer, version })
    }

    /// The version to install: the ref's own, else the catalog pin.
    pub fn version(&self) -> Option<String> {
        self.version
            .clone()
            .or_else(|| pinned_version(self.renderer.id()).map(str::to_string))
    }
}

/// `--renderer` (and the `--ddraw-wrapper` / `--no-ddraw-wrapper` aliases).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RendererChoice {
    /// Let the layers decide (L1 from the imports unless L2–L4 say otherwise).
    #[default]
    Auto,
    /// `--no-ddraw-wrapper`: as `Auto`, but never cnc-ddraw.
    AutoNoDdraw,
    /// `--renderer <id>` / `--ddraw-wrapper`.
    Force(Renderer),
}

/// L1's answer for one game (the docs/19 §6.2 table).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pick {
    pub renderer: Renderer,
    /// What to try next, in order ("Try next renderer", #78).
    pub alternatives: Vec<Renderer>,
    /// Why, for humans: the API, where it was seen, the bitness.
    pub reason: String,
    /// Set when the pick is known to be a poor fit (D3D12 without D3DMetal).
    pub warning: Option<String>,
    /// The game is 64-bit (D3DMetal is x86_64 only).
    pub x86_64: bool,
    /// D3DMetal consent this pick was made with (#71).
    pub d3dmetal: bool,
}

/// May D3DMetal be used (#71)? Only with a stored yes in gogo's config;
/// this never prompts ([`ask_d3dmetal`] does, at build time).
pub fn d3dmetal_available() -> bool {
    d3dmetal::available()
}

/// The build-time consent question (#71, asked once): for a 64-bit game
/// that imports `d3d12`, one built with `--renderer d3dmetal`, or one whose
/// compat DB entry (L3, for `gogid`) names D3DMetal. Any other game just uses
/// the stored answer, and a 32-bit game never gets D3DMetal. `prompt` is
/// `None` headless: no answer stored → a warning, and `false`.
pub fn ask_d3dmetal(
    gogid: Option<&str>,
    scan: &Result<PeScan, pe::ScanError>,
    choice: RendererChoice,
    prompt: Option<&super::prompt::PromptFn>,
    emit: &mut dyn FnMut(&super::ProgressEvent),
) -> bool {
    let Ok(scan) = scan else {
        return false;
    };
    if scan.bitness != Bitness::X86_64 {
        return false;
    }
    let why = if scan.graphics == GraphicsApi::D3d12 {
        format!("{} uses Direct3D 12", scan.main_exe)
    } else if choice == RendererChoice::Force(Renderer::D3dMetal) {
        format!("--renderer d3dmetal was asked for {}", scan.main_exe)
    } else if gogid.is_some_and(db_wants_d3dmetal) {
        format!("gogo's compat entry for {} picks D3DMetal", scan.main_exe)
    } else {
        return d3dmetal_available();
    };
    d3dmetal::ensure(&why, prompt, emit)
}

/// The docs/19 §6.2 table:
///
/// | graphics | bits | renderer | alternatives |
/// |---|---|---|---|
/// | ddraw only | 32 | cnc-ddraw | wined3d |
/// | ddraw only | 64 | wined3d | — |
/// | ddraw + D3D ≤ 7, d3d8, d3d9 | any | wined3d | — |
/// | d3d10/d3d11/dxgi | 32 | DXMT | wined3d |
/// | d3d10/d3d11/dxgi | 64 | DXMT | D3DMetal (if available), wined3d |
/// | d3d12 | 64 | D3DMetal if available, else wined3d + warning | DXMT, wined3d |
/// | OpenGL, Vulkan, nothing found | any | wined3d | — |
pub fn from_scan(scan: &PeScan, d3dmetal: bool) -> Pick {
    use Renderer::*;
    let bits64 = scan.bitness == Bitness::X86_64;
    let (renderer, alternatives, what, warning) = match scan.graphics {
        GraphicsApi::Ddraw2d if !bits64 => (CncDdraw, vec![Wined3d], "DirectDraw 2D", None),
        GraphicsApi::Ddraw2d => (Wined3d, vec![], "DirectDraw 2D (cnc-ddraw is 32-bit)", None),
        GraphicsApi::D3dLegacy => (Wined3d, vec![], "Direct3D 9 or older", None),
        GraphicsApi::D3d11 => {
            let mut alts = vec![];
            if bits64 && d3dmetal {
                alts.push(D3dMetal);
            }
            alts.push(Wined3d);
            (Dxmt, alts, "Direct3D 10/11", None)
        }
        GraphicsApi::D3d12 if bits64 && d3dmetal => {
            (D3dMetal, vec![Dxmt, Wined3d], "Direct3D 12", None)
        }
        GraphicsApi::D3d12 if bits64 => (
            Wined3d,
            vec![],
            "Direct3D 12",
            Some(format!(
                "Direct3D 12 needs D3DMetal, which needs your consent ({} = \"yes\"): the \
                 game is likely broken on wined3d",
                d3dmetal::KEY
            )),
        ),
        GraphicsApi::D3d12 => (
            Wined3d,
            vec![],
            "Direct3D 12",
            Some(
                "Direct3D 12 on a 32-bit game: D3DMetal is 64-bit only, so the game is \
                 likely broken on wined3d"
                    .to_string(),
            ),
        ),
        GraphicsApi::OpenGl => (Wined3d, vec![], "OpenGL", None),
        GraphicsApi::Vulkan => (Wined3d, vec![], "Vulkan", None),
        GraphicsApi::Unknown => (Wined3d, vec![], "no graphics API found", None),
    };
    let seen = match scan.graphics_source {
        GraphicsSource::Imports => {
            let names: Vec<String> = scan
                .graphics_imports
                .iter()
                .map(|g| format!("{g:?}").to_ascii_lowercase())
                .collect();
            format!("imports {}", names.join(", "))
        }
        GraphicsSource::RuntimeStrings => "runtime-loaded DLL names".to_string(),
        GraphicsSource::Engine => "implied by the engine".to_string(),
        GraphicsSource::None => "no graphics imports".to_string(),
    };
    let engine = scan
        .engine
        .map(|e| format!("{e:?} engine, "))
        .unwrap_or_default();
    let bits = if bits64 { "64-bit" } else { "32-bit" };
    let mut reason = format!("{what}: {seen} ({engine}{bits} {})", scan.main_exe);
    if renderer == CncDdraw && scan.local_overrides.contains("ddraw.dll") {
        reason.push_str("; the game's own ddraw.dll is kept");
    }
    Pick {
        renderer,
        alternatives,
        reason,
        warning,
        x86_64: bits64,
        d3dmetal,
    }
}

/// What "Try next renderer" (#78) says once the order is used up.
pub const NO_MORE_RENDERERS: &str = "no more renderers";

/// The renderer to try after `current` (a ref, `<id>[/<ver>]`), in the
/// order the docs/19 §6.2 table gives for this game: the L1 pick, then its
/// alternatives. A `current` outside that order (an L3/L4 or flag choice)
/// starts it from the top. D3DMetal is skipped unless `d3dmetal` (consent,
/// #71). `None`: [`NO_MORE_RENDERERS`].
pub fn next_after(pick: &Pick, current: &str, d3dmetal: bool) -> Option<Renderer> {
    let order: Vec<Renderer> = std::iter::once(pick.renderer)
        .chain(pick.alternatives.iter().copied())
        .filter(|r| d3dmetal || *r != Renderer::D3dMetal)
        .collect();
    let current = RendererRef::parse(current).ok().map(|r| r.renderer);
    match order.iter().position(|r| Some(*r) == current) {
        Some(i) => order.get(i + 1).copied(),
        None => order.into_iter().find(|r| Some(*r) != current),
    }
}

/// L1 from an import scan with the stored D3DMetal answer; a failed scan
/// picks wined3d. A cnc-ddraw gogo placed on an earlier setup isn't counted
/// as the game's own wrapper.
pub fn pick(scan: &Result<PeScan, pe::ScanError>) -> Pick {
    pick_with(scan, d3dmetal_available())
}

/// Does the L3 entry for `gogid` set the renderer to D3DMetal?
fn db_wants_d3dmetal(gogid: &str) -> bool {
    compat::db_entry(gogid)
        .entry
        .renderer
        .and_then(|r| RendererRef::parse(&r).ok())
        .is_some_and(|r| r.renderer == Renderer::D3dMetal)
}

/// [`pick`] with the D3DMetal answer given ([`ask_d3dmetal`]'s).
pub fn pick_with(scan: &Result<PeScan, pe::ScanError>, d3dmetal: bool) -> Pick {
    match scan {
        Ok(scan) => {
            let mut scan = scan.clone();
            if ddraw::is_gogo(&scan.root) {
                scan.local_overrides.remove("ddraw.dll");
            }
            from_scan(&scan, d3dmetal)
        }
        Err(e) => Pick {
            renderer: Renderer::Wined3d,
            alternatives: vec![],
            reason: format!("import scan failed ({e})"),
            warning: None,
            x86_64: false,
            d3dmetal: false,
        },
    }
}

/// L1 for a game tree: scan it (`exe` = the known entry point, else
/// `goggame-*.info`), then [`pick`].
pub fn auto(tree: &Path, exe: Option<&Path>) -> Pick {
    pick(&match exe {
        Some(exe) => pe::scan_exe(tree, exe),
        None => pe::scan(tree),
    })
}

/// The renderer a build bakes, the layer it came from and why.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Selection {
    pub renderer: Sourced<String>,
    pub reason: String,
    pub l1: Pick,
    /// An L4 renderer that setup applies on top at launch. `None` when a
    /// `--renderer` flag won: the build writes it into L4 (#110).
    pub user: Option<String>,
    pub warnings: Vec<String>,
}

impl Selection {
    /// `dxmt/v0.80 (L1 auto: Direct3D 10/11: imports d3d11 (32-bit Game.exe))`.
    pub fn describe(&self) -> String {
        let mut s = format!(
            "{} ({}: {})",
            self.renderer.value,
            self.renderer.layer.describe(),
            self.reason
        );
        if let Some(u) = &self.user {
            s.push_str(&format!("; L4 user override {u} applies at launch"));
        }
        s
    }
}

/// Merge the renderer field over L0–L3 and the build flags (design 16
/// decision 5). L4 is reported, not baked: setup applies it at launch,
/// unless a `--renderer` flag wins, which the build then writes into L4.
pub fn select(gogid: Option<&str>, l1: Pick, choice: RendererChoice) -> Selection {
    let l0 = CompatEntry {
        renderer: Some(Renderer::Wined3d.id().into()),
        ..Default::default()
    };
    let l1_entry = CompatEntry {
        renderer: Some(l1.renderer.default_ref()),
        ..Default::default()
    };
    let mut db = gogid.map(compat::db_entry).unwrap_or_default();
    let mut warnings: Vec<String> = l1.warning.iter().cloned().collect();
    // An L3 D3DMetal needs a 64-bit game and consent (#71); without them the
    // layers below pick (DXMT for 64-bit D3D11), not setup's wined3d fallback.
    if db
        .entry
        .renderer
        .as_deref()
        .and_then(|r| RendererRef::parse(r).ok())
        .is_some_and(|r| r.renderer == Renderer::D3dMetal)
        && !(l1.x86_64 && l1.d3dmetal)
    {
        db.entry.renderer = None;
        warnings.push(if l1.x86_64 {
            format!(
                "gogo's compat entry picks D3DMetal, which needs your consent ({} = \"yes\"): \
                 using the automatic choice",
                d3dmetal::KEY
            )
        } else {
            "gogo's compat entry picks D3DMetal, which is 64-bit only: using the automatic \
             choice"
                .into()
        });
    }
    let mut user = gogid
        .and_then(|g| compat::load_user(g).ok())
        .and_then(|p| p.entry.renderer);
    let mut layers = vec![
        (Layer::Default, &l0),
        (Layer::Auto, &l1_entry),
        (Layer::Db, &db.entry),
    ];
    let flag_entry;
    let mut flag_reason = String::new();
    match choice {
        RendererChoice::Auto => {}
        // D3DMetal only for a 64-bit game with consent; otherwise the
        // layers below decide.
        RendererChoice::Force(Renderer::D3dMetal) if !l1.x86_64 => warnings.push(
            "--renderer d3dmetal: D3DMetal is 64-bit only and this game is 32-bit; \
             keeping the automatic choice"
                .into(),
        ),
        RendererChoice::Force(Renderer::D3dMetal) if !l1.d3dmetal => warnings.push(format!(
            "--renderer d3dmetal: no consent to download D3DMetal ({} = \"yes\" in {}); \
             keeping the automatic choice",
            d3dmetal::KEY,
            d3dmetal::config_path().display()
        )),
        RendererChoice::Force(r) => {
            flag_entry = CompatEntry {
                renderer: Some(r.default_ref()),
                ..Default::default()
            };
            layers.push((Layer::Flags, &flag_entry));
            flag_reason = format!("--renderer {}", r.id());
            // Flags > L4 (#110): the build writes the flag into the L4
            // file, so that override no longer applies on top.
            if let Some(u) = user.take() {
                flag_reason.push_str(&format!(", replacing the L4 override {u}"));
            }
        }
        RendererChoice::AutoNoDdraw => {
            let below = compat::merge(&layers).renderer;
            if below.is_some_and(|s| s.value.starts_with(Renderer::CncDdraw.id())) {
                flag_entry = CompatEntry {
                    renderer: Some(Renderer::Wined3d.id().into()),
                    ..Default::default()
                };
                layers.push((Layer::Flags, &flag_entry));
                flag_reason = "--no-ddraw-wrapper".into();
            }
        }
    }
    let renderer = compat::merge(&layers)
        .renderer
        .expect("L0 always sets a renderer");
    let reason = match renderer.layer {
        Layer::Default => "the engine's own".to_string(),
        Layer::Auto => l1.reason.clone(),
        Layer::Db => "gogo's compat DB entry".to_string(),
        Layer::Flags => flag_reason,
        Layer::Gog | Layer::User => String::new(),
    };
    if renderer.value.starts_with(Renderer::D3dMetal.id()) && !l1.d3dmetal {
        warnings.push(format!(
            "D3DMetal has no consent ({} = \"yes\"): setup falls back to wined3d",
            d3dmetal::KEY
        ));
    }
    Selection {
        renderer,
        reason,
        l1,
        user,
        warnings,
    }
}

/// What [`apply`] set up.
#[derive(Debug, Clone)]
pub struct Applied {
    /// The renderer ref actually in effect (after any fallback).
    pub renderer: String,
    /// The runtime to run: the input, or the derived runtime for an engine
    /// overlay.
    pub runtime: Runtime,
    /// Overrides the renderer needs (`ddraw=n,b`); explicit ones win.
    pub dll_overrides: BTreeMap<String, String>,
    /// What was done, for the build log.
    pub notes: Vec<String>,
    pub warnings: Vec<String>,
}

/// Install and wire up `renderer` for one game (setup, design 16 §2.2):
/// DXMT or D3DMetal (with consent) → the derived runtime for `rt` (built
/// once, shared); cnc-ddraw →
/// `ddraw.dll` + ini in `game_dir` and `ddraw=n,b`; wined3d → nothing, and a
/// cnc-ddraw gogo placed earlier is removed again. Anything that can't be
/// set up falls back to wined3d with a warning, so a launch never fails on
/// the renderer alone.
pub fn apply(renderer: &str, rt: Runtime, game_dir: Option<&Path>) -> Applied {
    let mut out = Applied {
        renderer: Renderer::Wined3d.id().into(),
        runtime: rt,
        dll_overrides: BTreeMap::new(),
        notes: vec![],
        warnings: vec![],
    };
    let r = match RendererRef::parse(renderer) {
        Ok(r) => r,
        Err(e) => {
            out.warnings.push(format!("{e}: using wined3d"));
            return out;
        }
    };
    if r.renderer != Renderer::CncDdraw
        && let Some(dir) = game_dir
        && ddraw::remove_gogo(dir)
    {
        out.notes
            .push("removed gogo's cnc-ddraw from the game dir".into());
    }
    match r.renderer {
        Renderer::Wined3d => {}
        Renderer::D3dMetal if !d3dmetal_available() => out.warnings.push(format!(
            "D3DMetal: no consent to download it ({} = \"yes\" in {}): using wined3d",
            d3dmetal::KEY,
            d3dmetal::config_path().display()
        )),
        Renderer::CncDdraw => {
            let Some(dir) = game_dir.filter(|d| d.is_dir()) else {
                out.warnings
                    .push("cnc-ddraw: game dir not found: using wined3d".into());
                return out;
            };
            let mut warnings = vec![];
            let tree = ddraw::resolve_or_provision(&mut |ev| {
                if let super::ProgressEvent::Warn { message } = ev {
                    warnings.push(message.clone());
                }
            });
            out.warnings.extend(warnings);
            match tree.map(|t| ddraw::apply(&t, dir)) {
                Some(Ok(ddraw::Applied::Applied { shaders })) => out.notes.push(format!(
                    "cnc-ddraw (dll + ini, {shaders} shaders) in the game dir"
                )),
                Some(Ok(ddraw::Applied::AlreadyPresent)) => out
                    .notes
                    .push("cnc-ddraw: the game dir's own ddraw.dll is kept".into()),
                Some(Err(e)) => {
                    out.warnings.push(format!("cnc-ddraw: {e}: using wined3d"));
                    return out;
                }
                None if dir.join("ddraw.dll").is_file() => {}
                None => {
                    out.warnings
                        .push("cnc-ddraw unavailable (provisioning failed): using wined3d".into());
                    return out;
                }
            }
            out.dll_overrides.insert("ddraw".into(), "n,b".into());
            out.renderer = r.renderer.default_ref();
        }
        Renderer::Dxmt | Renderer::D3dMetal => match derive_for(&r, &out.runtime) {
            Ok((derived, rref)) => {
                out.notes.push(format!(
                    "{}: runtime {}",
                    r.renderer.name(),
                    super::launch::runtime_ref(&derived)
                ));
                out.runtime = derived;
                out.renderer = rref;
            }
            Err(e) => out
                .warnings
                .push(format!("{}: {e}: using wined3d", r.renderer.name())),
        },
    }
    out
}

/// The derived runtime for `rt` × an overlay renderer (`r`'s version),
/// installing the renderer into the store first if it's missing.
fn derive_for(r: &RendererRef, rt: &Runtime) -> Result<(Runtime, String), String> {
    if !rt.is_crossover() {
        return Err(format!(
            "needs a CrossOver-source engine (winecx), not {}",
            rt.version
        ));
    }
    let base_ref = super::launch::runtime_ref(rt);
    let (id, ver) = base_ref
        .split_once('/')
        .filter(|_| !base_ref.starts_with("path:"))
        .ok_or_else(|| format!("runtime {base_ref} is outside the store"))?;
    let (base, _) = store::installed(id, Some(ver))
        .ok_or_else(|| format!("runtime {base_ref} is not in the store manifest"))?;
    let payload =
        ensure_installed(r.renderer.id(), r.version().as_deref()).map_err(|e| e.to_string())?;
    let derived = store::derive(&base, &payload).map_err(|e| e.to_string())?;
    let rt = super::runtime::from_store(&derived).map_err(|e| e.to_string())?;
    Ok((rt, format!("{}/{}", payload.id, payload.version)))
}

/// A renderer payload from the store, downloading it (pinned, sha256
/// checked) when it isn't there. D3DMetal downloads only with a stored yes.
pub fn ensure_installed(
    id: &str,
    version: Option<&str>,
) -> Result<store::ManifestEntry, MacAppError> {
    if let Some((m, _)) = store::installed(id, version) {
        return Ok(m);
    }
    let entry = catalog::find(id).ok_or_else(|| MacAppError::CatalogUnreachable {
        detail: format!("{id} is not in the catalog"),
    })?;
    let consent = id == Renderer::D3dMetal.id() && d3dmetal_available();
    let http = catalog::http_client()?;
    lutris::block_on_detached(store::install(
        entry,
        version,
        &http,
        catalog::GITHUB_API,
        consent,
        &mut |_| {},
    ))
}

/// The renderer setup will apply for an existing game, the layer it came
/// from and why (`gogo mac-app compat`). L4 wins over what the bundle
/// baked; a baked value that differs from L0–L3 came from a build flag.
pub fn explain(
    gogid: &str,
    baked: &str,
    game_dir: Option<&Path>,
    exe: Option<&Path>,
) -> (Sourced<String>, String) {
    if let Some(u) = compat::load_user(gogid).ok().and_then(|p| p.entry.renderer) {
        return (
            Sourced {
                value: u,
                layer: Layer::User,
            },
            "your override file".into(),
        );
    }
    let l1 = match game_dir {
        Some(dir) => auto(dir, exe),
        None => Pick {
            renderer: Renderer::Wined3d,
            alternatives: vec![],
            reason: "game dir not found".into(),
            warning: None,
            x86_64: false,
            d3dmetal: false,
        },
    };
    let sel = select(Some(gogid), l1, RendererChoice::Auto);
    if sel.renderer.value == baked {
        (sel.renderer, sel.reason)
    } else {
        (
            Sourced {
                value: baked.to_string(),
                layer: Layer::Flags,
            },
            format!(
                "a build flag (the layers below pick {})",
                sel.renderer.value
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macapp::pe::GraphicsImport;
    use std::collections::BTreeSet;

    fn scan(graphics: GraphicsApi, bitness: Bitness, imports: &[GraphicsImport]) -> PeScan {
        PeScan {
            root: "/g".into(),
            main_exe: "Game.exe".into(),
            main_exe_source: pe::MainExeSource::GogInfo,
            bitness,
            engine: None,
            graphics,
            graphics_source: if imports.is_empty() {
                GraphicsSource::None
            } else {
                GraphicsSource::Imports
            },
            graphics_imports: imports.iter().copied().collect(),
            runtime_graphics: BTreeSet::new(),
            helpers: BTreeSet::new(),
            runtime_loaded: BTreeSet::new(),
            local_overrides: BTreeSet::new(),
            files: vec![],
        }
    }

    /// #78: the renderers "Try next renderer" walks, per API class, from the
    /// L1 pick until it runs out.
    #[test]
    fn next_renderer_order_per_api_class() {
        use Bitness::*;
        use GraphicsApi as A;
        use GraphicsImport as G;
        let walk = |pick: &Pick, d3dmetal: bool| {
            let mut seen = vec![pick.renderer.id().to_string()];
            while let Some(n) = next_after(pick, seen.last().unwrap(), d3dmetal) {
                assert!(seen.len() < 8, "cycle: {seen:?}");
                seen.push(n.id().to_string());
            }
            seen.push(NO_MORE_RENDERERS.to_string());
            seen
        };
        // (api, bits, imports, d3dmetal consent, renderers in order)
        type Case<'a> = (A, Bitness, &'a [G], bool, &'a [&'a str]);
        let cases: &[Case] = &[
            (
                A::Ddraw2d,
                I386,
                &[G::Ddraw],
                false,
                &["cnc-ddraw", "wined3d"],
            ),
            (A::Ddraw2d, X86_64, &[G::Ddraw], false, &["wined3d"]),
            (A::D3dLegacy, I386, &[G::D3d9], false, &["wined3d"]),
            (A::D3d11, I386, &[G::D3d11], false, &["dxmt", "wined3d"]),
            (A::D3d11, X86_64, &[G::D3d11], false, &["dxmt", "wined3d"]),
            (
                A::D3d11,
                X86_64,
                &[G::D3d11],
                true,
                &["dxmt", "d3dmetal", "wined3d"],
            ),
            (
                A::D3d12,
                X86_64,
                &[G::D3d12],
                true,
                &["d3dmetal", "dxmt", "wined3d"],
            ),
            (A::D3d12, X86_64, &[G::D3d12], false, &["wined3d"]),
            (A::OpenGl, I386, &[], false, &["wined3d"]),
        ];
        for (api, bits, imports, d3dmetal, want) in cases {
            let pick = from_scan(&scan(*api, *bits, imports), *d3dmetal);
            let mut want: Vec<String> = want.iter().map(|s| s.to_string()).collect();
            want.push(NO_MORE_RENDERERS.into());
            assert_eq!(
                walk(&pick, *d3dmetal),
                want,
                "{api:?} {bits:?} d3dmetal={d3dmetal}"
            );
        }
        // Without consent D3DMetal is skipped even if the pick lists it.
        let pick = from_scan(&scan(A::D3d11, X86_64, &[G::D3d11]), true);
        assert_eq!(
            next_after(&pick, "dxmt/v0.80", false),
            Some(Renderer::Wined3d)
        );
        // A renderer from outside the order (L4, a flag) restarts it, versioned
        // refs included.
        let pick = from_scan(&scan(A::D3d11, I386, &[G::D3d11]), false);
        assert_eq!(
            next_after(&pick, "cnc-ddraw/7.1.0", false),
            Some(Renderer::Dxmt)
        );
        assert_eq!(
            next_after(&pick, "dxmt/v0.80", false),
            Some(Renderer::Wined3d)
        );
        assert_eq!(next_after(&pick, "wined3d", false), None);
    }

    #[test]
    fn table_rows_and_alternatives() {
        use Bitness::*;
        use GraphicsApi as A;
        use GraphicsImport as G;
        use Renderer::*;
        // (api, bits, imports, d3dmetal available, pick, alternatives)
        type Row<'a> = (A, Bitness, &'a [G], bool, Renderer, &'a [Renderer]);
        let rows: &[Row] = &[
            (A::Ddraw2d, I386, &[G::Ddraw], false, CncDdraw, &[Wined3d]),
            (A::Ddraw2d, X86_64, &[G::Ddraw], false, Wined3d, &[]),
            (
                A::D3dLegacy,
                I386,
                &[G::Ddraw, G::Direct3d7],
                false,
                Wined3d,
                &[],
            ),
            (A::D3dLegacy, I386, &[G::D3d8], false, Wined3d, &[]),
            (A::D3dLegacy, X86_64, &[G::D3d9], false, Wined3d, &[]),
            (A::D3d11, I386, &[G::D3d11], false, Dxmt, &[Wined3d]),
            (A::D3d11, I386, &[G::D3d11], true, Dxmt, &[Wined3d]),
            (
                A::D3d11,
                X86_64,
                &[G::D3d11, G::Dxgi],
                false,
                Dxmt,
                &[Wined3d],
            ),
            (
                A::D3d11,
                X86_64,
                &[G::D3d11],
                true,
                Dxmt,
                &[D3dMetal, Wined3d],
            ),
            (A::D3d12, X86_64, &[G::D3d12], false, Wined3d, &[]),
            (
                A::D3d12,
                X86_64,
                &[G::D3d12],
                true,
                D3dMetal,
                &[Dxmt, Wined3d],
            ),
            (A::OpenGl, X86_64, &[G::Opengl32], false, Wined3d, &[]),
            (A::Vulkan, X86_64, &[G::Vulkan], false, Wined3d, &[]),
            (A::Unknown, I386, &[], false, Wined3d, &[]),
        ];
        for (api, bits, imports, d3dmetal, want, alts) in rows {
            let p = from_scan(&scan(*api, *bits, imports), *d3dmetal);
            assert_eq!(p.renderer, *want, "{api:?} {bits:?} d3dmetal={d3dmetal}");
            assert_eq!(
                p.alternatives, *alts,
                "{api:?} {bits:?} d3dmetal={d3dmetal}"
            );
        }
        // D3D12 without D3DMetal says so.
        let p = from_scan(&scan(A::D3d12, X86_64, &[G::D3d12]), false);
        assert!(p.warning.as_deref().unwrap().contains("likely broken"));
        // The reason names what was seen.
        let p = from_scan(&scan(A::D3d11, I386, &[G::D3d11, G::Dxgi]), false);
        assert_eq!(
            p.reason,
            "Direct3D 10/11: imports d3d11, dxgi (32-bit Game.exe)"
        );
        // D3D12 on a 32-bit game: no D3DMetal, even with consent.
        let p = from_scan(&scan(A::D3d12, I386, &[G::D3d12]), true);
        assert_eq!((p.renderer, p.alternatives.as_slice()), (Wined3d, &[][..]));
        assert!(p.warning.as_deref().unwrap().contains("64-bit only"));
        assert!(!p.x86_64 && p.d3dmetal);
    }

    #[test]
    fn engine_games_classify_by_their_detected_api() {
        // Neo Cab: Unity, imports only opengl32, loads d3d11 at runtime; the
        // scanner classifies it d3d10_11 from runtime strings.
        let mut s = scan(GraphicsApi::D3d11, Bitness::X86_64, &[]);
        s.engine = Some(pe::Engine::Unity);
        s.graphics_source = GraphicsSource::RuntimeStrings;
        let p = from_scan(&s, false);
        assert_eq!(p.renderer, Renderer::Dxmt);
        assert!(p.reason.contains("Unity engine"), "{}", p.reason);
        // A shipped ddraw.dll stays; the pick still sets the override.
        let mut s = scan(
            GraphicsApi::Ddraw2d,
            Bitness::I386,
            &[GraphicsImport::Ddraw],
        );
        s.local_overrides.insert("ddraw.dll".into());
        let p = from_scan(&s, false);
        assert_eq!(p.renderer, Renderer::CncDdraw);
        assert!(p.reason.ends_with("own ddraw.dll is kept"));
    }

    #[test]
    fn refs_parse_and_default_to_the_catalog_pin() {
        assert_eq!(Renderer::Wined3d.default_ref(), "wined3d");
        assert_eq!(Renderer::Dxmt.default_ref(), "dxmt/v0.80");
        assert_eq!(Renderer::CncDdraw.default_ref(), "cnc-ddraw/7.1.0.0");
        assert_eq!(Renderer::D3dMetal.default_ref(), "d3dmetal/4.0b3");
        let r = RendererRef::parse("dxmt").unwrap();
        assert_eq!(r.renderer, Renderer::Dxmt);
        assert_eq!(r.version().as_deref(), Some("v0.80"));
        let r = RendererRef::parse("cnc-ddraw/7.0.0.0").unwrap();
        assert_eq!(r.version().as_deref(), Some("7.0.0.0"));
        assert!(RendererRef::parse("dxvk").is_err());
    }

    fn pick(r: Renderer) -> Pick {
        Pick {
            renderer: r,
            alternatives: vec![],
            reason: "test".into(),
            warning: None,
            x86_64: true,
            d3dmetal: false,
        }
    }

    #[test]
    fn precedence_flags_db_l1_l0() {
        let home = tempfile::tempdir().unwrap();
        let _g = crate::test_util::env_lock();
        // SAFETY: env mutation under the shared env mutex.
        unsafe { std::env::set_var("GOGO_HOME", home.path()) };

        // L1 beats L0.
        let s = select(Some("1"), pick(Renderer::Dxmt), RendererChoice::Auto);
        assert_eq!(s.renderer.value, "dxmt/v0.80");
        assert_eq!(s.renderer.layer, Layer::Auto);
        assert_eq!(s.reason, "test");
        // No gogid (installer lane before the id is known): still L1.
        let s = select(None, pick(Renderer::Wined3d), RendererChoice::Auto);
        assert_eq!(s.renderer.layer, Layer::Auto);
        // A flag beats L1.
        let s = select(
            Some("1"),
            pick(Renderer::Dxmt),
            RendererChoice::Force(Renderer::CncDdraw),
        );
        assert_eq!(s.renderer.value, "cnc-ddraw/7.1.0.0");
        assert_eq!(s.renderer.layer, Layer::Flags);
        assert_eq!(s.reason, "--renderer cnc-ddraw");
        // --no-ddraw-wrapper only acts on a cnc-ddraw pick.
        let s = select(
            Some("1"),
            pick(Renderer::CncDdraw),
            RendererChoice::AutoNoDdraw,
        );
        assert_eq!(
            (s.renderer.value.as_str(), s.renderer.layer),
            ("wined3d", Layer::Flags)
        );
        let s = select(Some("1"), pick(Renderer::Dxmt), RendererChoice::AutoNoDdraw);
        assert_eq!(s.renderer.layer, Layer::Auto);
        // --renderer d3dmetal (#71): only with consent, only for 64-bit.
        let force = RendererChoice::Force(Renderer::D3dMetal);
        let s = select(Some("1"), pick(Renderer::Dxmt), force);
        assert_eq!(
            (s.renderer.value.as_str(), s.renderer.layer),
            ("dxmt/v0.80", Layer::Auto)
        );
        assert!(
            s.warnings.iter().any(|w| w.contains("no consent")),
            "{:?}",
            s.warnings
        );
        let consented = Pick {
            d3dmetal: true,
            ..pick(Renderer::Dxmt)
        };
        let s = select(Some("1"), consented.clone(), force);
        assert_eq!(
            (s.renderer.value.as_str(), s.renderer.layer),
            ("d3dmetal/4.0b3", Layer::Flags)
        );
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        let s = select(
            Some("1"),
            Pick {
                x86_64: false,
                ..consented
            },
            force,
        );
        assert_eq!(s.renderer.layer, Layer::Auto);
        assert!(s.warnings.iter().any(|w| w.contains("64-bit only")));

        // L4 is reported, not baked; a flag replaces it (#110).
        compat::set_user_keys("1", &[("renderer", Some("wined3d"))]).unwrap();
        let s = select(Some("1"), pick(Renderer::Dxmt), RendererChoice::Auto);
        assert_eq!(s.renderer.value, "dxmt/v0.80");
        assert_eq!(s.user.as_deref(), Some("wined3d"));
        assert!(s.describe().contains("L4 user override wined3d"));
        let s = select(
            Some("1"),
            pick(Renderer::Dxmt),
            RendererChoice::Force(Renderer::Dxmt),
        );
        assert_eq!(s.renderer.layer, Layer::Flags);
        assert_eq!(s.user, None);
        assert!(s.reason.contains("replacing the L4 override wined3d"));
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        // explain(): L4 wins; else a baked value off L0–L3 is a flag.
        let (r, _) = explain("1", "dxmt/v0.80", None, None);
        assert_eq!((r.value.as_str(), r.layer), ("wined3d", Layer::User));
        compat::set_user_keys("1", &[("renderer", None)]).unwrap();
        let (r, why) = explain("1", "dxmt/v0.80", None, None);
        assert_eq!(r.layer, Layer::Flags);
        assert!(why.contains("pick wined3d"), "{why}");
        let (r, _) = explain("1", "wined3d", None, None);
        assert_eq!(r.layer, Layer::Auto);
        unsafe { std::env::remove_var("GOGO_HOME") };
    }

    /// #71: who gets asked. Fake TTY prompts count their calls; the answer
    /// lands in `$GOGO_HOME/config.toml`.
    #[test]
    fn d3dmetal_consent_is_asked_for_64_bit_d3d12_and_never_for_32_bit() {
        use Bitness::*;
        use GraphicsApi as A;
        use GraphicsImport as G;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let home = tempfile::tempdir().unwrap();
        let _g = crate::test_util::env_lock();
        // SAFETY: env mutation under the shared env mutex.
        unsafe {
            std::env::set_var("GOGO_HOME", home.path());
            std::env::remove_var("GOGO_CONFIG");
        }
        let asked = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = asked.clone();
        let yes = move |_: &super::super::prompt::PromptSpec| {
            counter.fetch_add(1, Ordering::SeqCst);
            Some(super::super::prompt::PromptAnswer::Confirmed(true))
        };
        let ok = |a, b, i: &[G]| Ok(scan(a, b, i));
        let auto = RendererChoice::Auto;
        let force = RendererChoice::Force(Renderer::D3dMetal);
        let warns = std::cell::RefCell::new(Vec::<String>::new());
        let mut emit = |e: &super::super::ProgressEvent| {
            if let super::super::ProgressEvent::Warn { message } = e {
                warns.borrow_mut().push(message.clone());
            }
        };

        // 32-bit: never asked, whatever it imports or asks for.
        for (api, i, c) in [
            (A::D3d12, &[G::D3d12][..], auto),
            (A::D3d11, &[G::D3d11][..], force),
        ] {
            assert!(!ask_d3dmetal(
                None,
                &ok(api, I386, i),
                c,
                Some(&yes),
                &mut emit
            ));
        }
        // 64-bit D3D11 without the flag: DXMT's game, not asked.
        assert!(!ask_d3dmetal(
            None,
            &ok(A::D3d11, X86_64, &[G::D3d11]),
            auto,
            Some(&yes),
            &mut emit
        ));
        assert_eq!(asked.load(Ordering::SeqCst), 0);
        assert!(warns.borrow().is_empty() && !home.path().join("config.toml").exists());
        // Headless 64-bit D3D12, no answer: warned, not stored, falls back.
        let d3d12 = ok(A::D3d12, X86_64, &[G::D3d12]);
        assert!(!ask_d3dmetal(None, &d3d12, auto, None, &mut emit));
        assert_eq!(warns.borrow().len(), 1, "{warns:?}");
        assert_eq!(pick_with(&d3d12, false).renderer, Renderer::Wined3d);
        // TTY: asked once, stored, then the stored answer serves everyone.
        assert!(ask_d3dmetal(None, &d3d12, auto, Some(&yes), &mut emit));
        assert!(ask_d3dmetal(None, &d3d12, auto, None, &mut emit));
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert!(d3dmetal_available());
        assert_eq!(super::pick(&d3d12).renderer, Renderer::D3dMetal);
        // … including 64-bit D3D11's alternatives and "Try next renderer".
        let d3d11 = super::pick(&ok(A::D3d11, X86_64, &[G::D3d11]));
        assert_eq!(d3d11.renderer, Renderer::Dxmt);
        assert_eq!(
            next_after(&d3d11, "dxmt/v0.80", d3dmetal_available()),
            Some(Renderer::D3dMetal)
        );
        // A stored no is respected the same way, and never re-asked.
        crate::macapp::d3dmetal::store(crate::config::Consent::No).unwrap();
        assert!(!ask_d3dmetal(None, &d3d12, auto, Some(&yes), &mut emit));
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert_eq!(
            next_after(&d3d11, "dxmt/v0.80", d3dmetal_available()),
            Some(Renderer::Wined3d)
        );
        // apply() without consent falls back before touching the network.
        let a = apply("d3dmetal/4.0b3", fake_runtime(), None);
        assert_eq!(a.renderer, "wined3d");
        assert!(
            a.warnings.iter().any(|w| w.contains("no consent")),
            "{:?}",
            a.warnings
        );
        assert_eq!(warns.borrow().len(), 1, "{warns:?}");
        unsafe { std::env::remove_var("GOGO_HOME") };
    }

    fn fake_runtime() -> Runtime {
        Runtime {
            mode: super::super::runtime::RuntimeMode::Path,
            kind: "path",
            root: "/rt".into(),
            wine: "/rt/bin/wine".into(),
            version: "wine-9.0".into(),
            min_macos: None,
        }
    }

    /// #71 × #74: an L3 entry naming D3DMetal asks for consent on a 64-bit
    /// game, and without a yes (or on a 32-bit game) the layers below pick.
    #[test]
    fn compat_entry_d3dmetal_is_gated_like_the_flag() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let home = tempfile::tempdir().unwrap();
        let db = tempfile::tempdir().unwrap();
        std::fs::write(db.path().join("42.toml"), "renderer = \"d3dmetal\"\n").unwrap();
        let _g = crate::test_util::env_lock();
        // SAFETY: env mutation under the shared env mutex.
        unsafe {
            std::env::set_var("GOGO_HOME", home.path());
            std::env::remove_var("GOGO_CONFIG");
            std::env::set_var(compat::DB_DIR_ENV, db.path());
        }
        let asked = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = asked.clone();
        let no = move |_: &super::super::prompt::PromptSpec| {
            counter.fetch_add(1, Ordering::SeqCst);
            Some(super::super::prompt::PromptAnswer::Confirmed(false))
        };
        let d3d11 = |b| Ok(scan(GraphicsApi::D3d11, b, &[pe::GraphicsImport::D3d11]));
        let auto = RendererChoice::Auto;
        // 32-bit: not asked; the entry is set aside for the L1 pick.
        assert!(!ask_d3dmetal(
            Some("42"),
            &d3d11(Bitness::I386),
            auto,
            Some(&no),
            &mut |_| {}
        ));
        let s = select(Some("42"), pick_with(&d3d11(Bitness::I386), false), auto);
        assert_eq!(
            (s.renderer.value.as_str(), s.renderer.layer),
            ("dxmt/v0.80", Layer::Auto)
        );
        assert!(
            s.warnings.iter().any(|w| w.contains("64-bit only")),
            "{:?}",
            s.warnings
        );
        // 64-bit: asked because of the entry; a no keeps DXMT.
        let s64 = d3d11(Bitness::X86_64);
        let c = ask_d3dmetal(Some("42"), &s64, auto, Some(&no), &mut |_| {});
        assert!(!c);
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        let s = select(Some("42"), pick_with(&s64, c), auto);
        assert_eq!(
            (s.renderer.value.as_str(), s.renderer.layer),
            ("dxmt/v0.80", Layer::Auto)
        );
        assert!(
            s.warnings.iter().any(|w| w.contains("consent")),
            "{:?}",
            s.warnings
        );
        // With consent the entry wins as L3.
        let s = select(Some("42"), pick_with(&s64, true), auto);
        assert_eq!(
            (s.renderer.value.as_str(), s.renderer.layer),
            ("d3dmetal", Layer::Db)
        );
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        unsafe {
            std::env::remove_var("GOGO_HOME");
            std::env::remove_var(compat::DB_DIR_ENV);
        }
    }

    #[test]
    fn db_beats_l1() {
        // L3 entries are compiled in; feed one through the merge directly.
        let db = CompatEntry {
            renderer: Some("dxmt".into()),
            ..Default::default()
        };
        let l1 = CompatEntry {
            renderer: Some("wined3d".into()),
            ..Default::default()
        };
        let m = compat::merge(&[(Layer::Auto, &l1), (Layer::Db, &db)]);
        assert_eq!(m.renderer.unwrap().layer, Layer::Db);
    }
}
