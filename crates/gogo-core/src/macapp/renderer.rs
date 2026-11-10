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
//!
//! **Linux** (design 18 §3, #172) has its own table ([`from_scan_linux`]),
//! ids and setup ([`plan_linux`], [`apply_linux`]): under GE-Proton
//! **DXVK** and **VKD3D-Proton** are Proton's defaults, **D7VK** and
//! **wined3d** are Proton switches, and **cnc-ddraw** is the one renderer
//! gogo places itself. The OS is a parameter ([`select_on`],
//! [`RendererRef::parse_on`]), so both lanes are tested on either OS.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::compat::{self, CompatEntry, Layer, Os, Sourced};
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
    /// Linux: Proton's D3D8–11 (D3D8 only with [`PROTON_DXVK_D3D8`]).
    Dxvk,
    /// Linux: Proton's D3D12.
    Vkd3dProton,
    /// Linux: Proton's D3D ≤ 7 `ddraw.dll`, with [`PROTON_USE_D7VK`].
    D7vk,
}

impl Renderer {
    /// The macOS renderers, in the order "unknown renderer" lists them.
    pub const MACOS: [Renderer; 4] = [
        Renderer::Wined3d,
        Renderer::Dxmt,
        Renderer::CncDdraw,
        Renderer::D3dMetal,
    ];

    /// The Linux renderers (design 18 §3).
    pub const LINUX: [Renderer; 5] = [
        Renderer::Dxvk,
        Renderer::Vkd3dProton,
        Renderer::D7vk,
        Renderer::CncDdraw,
        Renderer::Wined3d,
    ];

    /// The renderers `os` has.
    pub fn on(os: Os) -> &'static [Renderer] {
        match os {
            Os::Macos => &Renderer::MACOS,
            Os::Linux => &Renderer::LINUX,
        }
    }

    /// The id used in refs, flags and the compat schema.
    pub fn id(self) -> &'static str {
        match self {
            Renderer::Wined3d => "wined3d",
            Renderer::Dxmt => "dxmt",
            Renderer::CncDdraw => "cnc-ddraw",
            Renderer::D3dMetal => "d3dmetal",
            Renderer::Dxvk => "dxvk",
            Renderer::Vkd3dProton => "vkd3d-proton",
            Renderer::D7vk => "d7vk",
        }
    }

    /// A macOS renderer by id (the Mac lane's refs and flags).
    pub fn from_id(id: &str) -> Option<Renderer> {
        Renderer::from_id_on(id, Os::Macos)
    }

    /// A renderer of `os` by id.
    pub fn from_id_on(id: &str, os: Os) -> Option<Renderer> {
        Renderer::on(os).iter().copied().find(|r| r.id() == id)
    }

    /// Whatever the Proton build bundles: no version, no download.
    pub fn proton_bundled(self) -> bool {
        matches!(
            self,
            Renderer::Dxvk | Renderer::Vkd3dProton | Renderer::D7vk
        )
    }

    /// The name in messages.
    pub fn name(self) -> &'static str {
        match self {
            Renderer::Wined3d => "wined3d",
            Renderer::Dxmt => "DXMT",
            Renderer::CncDdraw => "cnc-ddraw",
            Renderer::D3dMetal => "D3DMetal",
            Renderer::Dxvk => "DXVK",
            Renderer::Vkd3dProton => "VKD3D-Proton",
            Renderer::D7vk => "D7VK",
        }
    }

    /// The ref this renderer is written as when no version is asked for:
    /// `wined3d`, or `<id>/<catalog-pinned version>`.
    pub fn default_ref(self) -> String {
        if self.proton_bundled() {
            return self.id().to_string();
        }
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

/// Linux: the applied-renderer label for L1's own `wined3d` pick, where
/// nothing is set and Proton's defaults run (DXVK for D3D9–11, Wine's
/// builtin `ddraw`). It is a label for what setup applied (`state.toml`,
/// `launch.conf`'s `RENDERER`), never a choice: `renderer = "wined3d"` in
/// L3/L4 or a flag still means `PROTON_USE_WINED3D=1`, so [`RendererRef`]
/// does not parse it (design 18 §3).
pub const PROTON_DEFAULT: &str = "proton-default";

/// For people: what [`PROTON_DEFAULT`] means.
pub const PROTON_DEFAULT_DESC: &str = "none (Proton's defaults: DXVK for d3d9–11)";

/// The renderer an applied ref (`state.toml`, `RENDERER=`) stands for:
/// [`PROTON_DEFAULT`] is wined3d's pick with nothing set.
pub fn applied_renderer(s: &str, os: Os) -> Result<Renderer, String> {
    if s.trim() == PROTON_DEFAULT {
        return Ok(Renderer::Wined3d);
    }
    RendererRef::parse_on(s, os).map(|r| r.renderer)
}

/// How a Linux renderer is labelled in what setup applied and in output:
/// L1's own wined3d pick sets nothing, so it is [`PROTON_DEFAULT`]; a
/// wined3d anyone chose, or fell back to, sets `PROTON_USE_WINED3D=1`.
pub fn label_linux(r: Renderer, l1: &Pick) -> &'static str {
    if r == Renderer::Wined3d && l1.renderer == Renderer::Wined3d {
        PROTON_DEFAULT
    } else {
        r.id()
    }
}

/// Setup's rule for a chosen `wined3d` (L3, L4, a flag or the walk): it
/// moves D3D9–12 to wined3d, while L1's own pick (`layer_is_auto`) stays on
/// Proton's defaults (#172). Turns a plan's [`PROTON_DEFAULT`] label back
/// into `wined3d` with the switch when it was chosen. Returns whether it
/// added the switch.
pub fn settle_wined3d(plan: &mut LinuxPlan, layer_is_auto: bool) -> bool {
    if plan.renderer != PROTON_DEFAULT || layer_is_auto {
        return false;
    }
    plan.renderer = Renderer::Wined3d.id().into();
    if plan.env.contains_key(PROTON_USE_WINED3D) {
        return false;
    }
    plan.env.insert(PROTON_USE_WINED3D.into(), "1".into());
    true
}

/// A renderer ref, `<id>[/<ver>]` (gogo-launch.toml `renderer`, compat
/// `renderer`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererRef {
    pub renderer: Renderer,
    pub version: Option<String>,
}

impl RendererRef {
    /// A macOS ref (the Mac lane's).
    pub fn parse(s: &str) -> Result<RendererRef, String> {
        RendererRef::parse_on(s, Os::Macos)
    }

    /// A ref for `os`. On Linux a Mac-only id is compat's error, and the
    /// Proton-bundled renderers take no version.
    pub fn parse_on(s: &str, os: Os) -> Result<RendererRef, String> {
        let (id, version) = match s.trim().split_once('/') {
            Some((id, v)) => (id, Some(v.to_string())),
            None => (s.trim(), None),
        };
        if os == Os::Linux
            && let Some(why) = compat::mac_only_renderer(id)
        {
            return Err(format!("renderer `{s}`: {why}"));
        }
        let renderer = Renderer::from_id_on(id, os).ok_or_else(|| {
            let ids: Vec<&str> = Renderer::on(os).iter().map(|r| r.id()).collect();
            format!("unknown renderer `{s}` ({})", ids.join(", "))
        })?;
        if renderer.proton_bundled() && version.is_some() {
            return Err(format!(
                "renderer `{s}`: {} takes no version (it is the one the Proton build bundles)",
                renderer.id()
            ));
        }
        Ok(RendererRef { renderer, version })
    }

    /// A ref of either OS (the walk's `current`).
    fn parse_any(s: &str) -> Option<Renderer> {
        let id = s.trim().split('/').next().unwrap_or_default();
        if id == PROTON_DEFAULT {
            return Some(Renderer::Wined3d);
        }
        Os::ALL
            .into_iter()
            .find_map(|os| Renderer::from_id_on(id, os))
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
    /// Linux: the game's Direct3D is 8, which DXVK only takes with
    /// [`PROTON_DXVK_D3D8`]. Always `false` on the Mac.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub d3d8: bool,
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
    let mut reason = reason_for(scan, what);
    if scan.graphics == GraphicsApi::D3dLegacy
        && let Some(note) = d3d7_note(scan, false)
    {
        reason.push_str(&note);
    }
    let (renderer, alternatives) = if scan.ddrawcompat && renderer == CncDdraw {
        (Wined3d, vec![])
    } else {
        (renderer, alternatives)
    };
    if scan.ddrawcompat {
        reason.push_str("; the game's DDrawCompat stays unloaded: it doesn't run under Wine");
    } else if renderer == CncDdraw && scan.local_overrides.contains("ddraw.dll") {
        reason.push_str("; the game's own ddraw.dll is kept");
    }
    Pick {
        renderer,
        alternatives,
        reason,
        warning,
        x86_64: bits64,
        d3dmetal,
        d3d8: false,
    }
}

/// The dir of the exe a scan read, where gogo's cnc-ddraw goes (#199).
fn scan_exe_dir(scan: &PeScan) -> PathBuf {
    ddraw::dir_for(&scan.root, Some(&scan.root.join(&scan.main_exe)))
}

/// `<what>: <where it was seen> (<engine>, <bits> <exe>)`, both lanes.
fn reason_for(scan: &PeScan, what: &str) -> String {
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
    let bits = if scan.bitness == Bitness::X86_64 {
        "64-bit"
    } else {
        "32-bit"
    };
    format!("{what}: {seen} ({engine}{bits} {})", scan.main_exe)
}

/// #173's confidence wording for the reason. The Mac says nothing for
/// `strong`; Linux (`strong_too`) names that level as well, since it picks
/// D7VK on every level.
fn d3d7_note(scan: &PeScan, strong_too: bool) -> Option<String> {
    Some(match scan.d3d7_confidence? {
        pe::D3d7Confidence::Medium => "; Direct3D ≤ 7 from interface GUIDs only".into(),
        pe::D3d7Confidence::DxguidBlock => {
            "; Direct3D ≤ 7 only from the dxguid GUID block: may be 2D".into()
        }
        pe::D3d7Confidence::Strong if !strong_too => return None,
        pe::D3d7Confidence::Strong if scan.renderer_dlls.is_empty() => {
            "; Direct3D ≤ 7 from a device/work GUID or a d3dim/d3drm import".into()
        }
        pe::D3d7Confidence::Strong => {
            let dlls: Vec<&str> = scan.renderer_dlls.iter().map(String::as_str).collect();
            format!("; Direct3D ≤ 7 from the renderer DLL {}", dlls.join(", "))
        }
    })
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
    let current = RendererRef::parse_any(current);
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
            if ddraw::is_gogo(&scan_exe_dir(&scan)) {
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
            d3d8: false,
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
        self.describe_as(&self.renderer.value)
    }

    /// [`describe`](Selection::describe) for Linux: L1's own wined3d pick
    /// reads [`PROTON_DEFAULT`] (nothing is set there).
    pub fn describe_linux(&self) -> String {
        if self.renderer.value == Renderer::Wined3d.id() && self.renderer.layer == Layer::Auto {
            self.describe_as(PROTON_DEFAULT)
        } else {
            self.describe()
        }
    }

    fn describe_as(&self, value: &str) -> String {
        let mut s = format!(
            "{} ({}: {})",
            value,
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
///
/// This is the Mac lane's: it reads this OS's L3/L4, as before #172.
pub fn select(gogid: Option<&str>, l1: Pick, choice: RendererChoice) -> Selection {
    select_in(Os::Macos, Os::current(), gogid, l1, choice)
}

/// [`select`] for `os`: its renderers, and L3/L4 resolved for it. On Linux
/// `l1` is [`from_scan_linux`]'s, and a `--renderer` id Linux doesn't have
/// (`dxmt`, `d3dmetal`) warns and keeps the automatic choice.
pub fn select_on(os: Os, gogid: Option<&str>, l1: Pick, choice: RendererChoice) -> Selection {
    select_in(os, os, gogid, l1, choice)
}

/// The merge with `os`'s renderers and the layers of `layers`.
fn select_in(
    os: Os,
    layers_os: Os,
    gogid: Option<&str>,
    l1: Pick,
    choice: RendererChoice,
) -> Selection {
    let l0 = CompatEntry {
        renderer: Some(Renderer::Wined3d.id().into()),
        ..Default::default()
    };
    let l1_entry = CompatEntry {
        renderer: Some(l1.renderer.default_ref()),
        ..Default::default()
    };
    let mut db = gogid
        .map(|g| compat::db_entry_for(g, layers_os))
        .unwrap_or_default();
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
        .and_then(|g| compat::load_user_for(g, layers_os).ok())
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
        RendererChoice::Force(r) if !Renderer::on(os).contains(&r) => warnings.push(format!(
            "--renderer {}: {} has no {}; keeping the automatic choice",
            r.id(),
            os.key(),
            r.name()
        )),
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

/// Why a renderer that needs `ddraw.dll` can't be set up beside DDrawCompat.
const DDRAWCOMPAT_STAYS: &str = "the game's ddraw.dll is DDrawCompat, which stays in place and unloaded (it doesn't run \
     under Wine)";

/// Install and wire up `renderer` for one game (setup, design 16 §2.2):
/// DXMT or D3DMetal (with consent) → the derived runtime for `rt` (built
/// once, shared); cnc-ddraw →
/// `ddraw.dll` + ini in `game_dir` and `ddraw=n,b`; wined3d → nothing, and a
/// cnc-ddraw gogo placed earlier is removed again. `game_dir` is the entry
/// exe's dir ([`LaunchToml::exe_dir_in`]). DDrawCompat there is left unloaded
/// and cnc-ddraw isn't asked to replace it ([`ddraw::is_ddrawcompat`]).
/// Anything that can't be set up falls back to wined3d with a warning, so a
/// launch never fails on the renderer alone.
///
/// [`LaunchToml::exe_dir_in`]: super::launch::LaunchToml::exe_dir_in
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
    // #82: a renderer build for another arch than the runtime's.
    let arch = catalog::runtime_arch(&super::launch::runtime_ref(&out.runtime));
    if let Some(Err(e)) =
        catalog::find(r.renderer.id()).map(|e| catalog::check_renderer_arch(arch, e))
    {
        out.warnings.push(format!("{e}: using wined3d"));
        return out;
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
            if ddraw::is_ddrawcompat(dir) {
                out.warnings
                    .push(format!("cnc-ddraw: {DDRAWCOMPAT_STAYS}: using wined3d"));
                return out;
            }
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
                None if ddraw::has_dll(dir) => {}
                None => {
                    out.warnings
                        .push("cnc-ddraw unavailable (provisioning failed): using wined3d".into());
                    return out;
                }
            }
            out.dll_overrides.insert("ddraw".into(), "n,b".into());
            out.renderer = r.renderer.default_ref();
        }
        // `RendererRef::parse` has only the macOS ids.
        Renderer::Dxvk | Renderer::Vkd3dProton | Renderer::D7vk => out.warnings.push(format!(
            "{} is Linux only: using wined3d",
            r.renderer.name()
        )),
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
            d3d8: false,
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

// — Linux (design 18 §3, #172) —

/// Proton's switch for D7VK's `ddraw.dll` (32-bit only, in `syswow64`).
pub const PROTON_USE_D7VK: &str = "PROTON_USE_D7VK";
/// Proton's switch that moves D3D9–12 to wined3d together.
pub const PROTON_USE_WINED3D: &str = "PROTON_USE_WINED3D";
/// Proton's switch for DXVK's `d3d8` (else wined3d's).
pub const PROTON_DXVK_D3D8: &str = "PROTON_DXVK_D3D8";

/// Where a Proton build keeps D7VK, relative to its root: the file
/// Proton's `setup_ddraw` copies (GE-Proton11-12's `proton` and
/// `Makefile.in`: `files/lib/wine/d7vk/i386-windows/`, no 64-bit build).
pub const D7VK_DLL: &str = "files/lib/wine/d7vk/i386-windows/ddraw.dll";

/// Does the Proton build at `proton` have D7VK ([`D7VK_DLL`])? Without it
/// Proton logs and silently keeps Wine's `ddraw`.
pub fn has_d7vk(proton: &Path) -> bool {
    proton.join(D7VK_DLL).is_file()
}

/// The Direct3D APIs that decided [`PeScan::graphics`]: the static imports,
/// plus the runtime-loaded names when those decided; none for an engine's
/// implied class (Clickteam).
fn deciding_apis(scan: &PeScan) -> std::collections::BTreeSet<pe::GraphicsImport> {
    match scan.graphics_source {
        GraphicsSource::Imports => scan.graphics_imports.clone(),
        GraphicsSource::RuntimeStrings => scan
            .graphics_imports
            .union(&scan.runtime_graphics)
            .copied()
            .collect(),
        GraphicsSource::Engine | GraphicsSource::None => Default::default(),
    }
}

/// D3D ≤ 7 retained mode: a `d3drm` / `d3dxof` import (or runtime name,
/// when those decided) outside the game's own wrapper DLLs.
fn retained_mode(scan: &PeScan) -> bool {
    let rm = |n: &String| n == "d3drm.dll" || n == "d3dxof.dll";
    let imported = scan.files.iter().any(|f| {
        let name = f.path.rsplit(['/', '\\']).next().unwrap_or_default();
        !scan.local_overrides.contains(&name.to_ascii_lowercase()) && f.imports.iter().any(rm)
    });
    imported
        || (scan.graphics_source == GraphicsSource::RuntimeStrings
            && scan.runtime_loaded.iter().any(rm))
}

/// The design 18 §3 table (docs/22 §6.1), Linux's L1:
///
/// | L1 class | bits | renderer | then |
/// |---|---|---|---|
/// | DirectDraw or D3D ≤ 7, the game ships DDrawCompat | any | wined3d (nothing; the DLL stays unloaded, #199) | — |
/// | DirectDraw or D3D ≤ 7, the game ships another `ddraw.dll` | any | wined3d (nothing; the DLL is kept, `ddraw=n,b`) | — |
/// | DirectDraw 2D | 32 | cnc-ddraw | d7vk, wined3d |
/// | DirectDraw 2D | 64 | wined3d | — |
/// | D3D ≤ 7, any #173 confidence | 32 | d7vk | cnc-ddraw, wined3d |
/// | D3D ≤ 7 | 64 | wined3d (D7VK is 32-bit) | — |
/// | D3D ≤ 7 retained mode (`d3drm`/`d3dxof`) | any | wined3d | — |
/// | D3D8 | any | dxvk + [`PROTON_DXVK_D3D8`] | wined3d |
/// | D3D9 / 10 / 11 | any | dxvk | wined3d |
/// | D3D12 | any | vkd3d-proton | wined3d |
/// | OpenGL, Vulkan, an engine's class, nothing found | any | wined3d (nothing) | — |
///
/// Never cnc-ddraw and D7VK together: each is the other's alternative.
pub fn from_scan_linux(scan: &PeScan) -> Pick {
    use Renderer::*;
    use pe::GraphicsImport as G;
    let bits64 = scan.bitness == Bitness::X86_64;
    let apis = deciding_apis(scan);
    let has = |g| apis.contains(&g);
    let d3d7_class = scan.graphics == GraphicsApi::D3dLegacy
        && has(G::Direct3d7)
        && !has(G::D3d8)
        && !has(G::D3d9);
    let ddraw_class = scan.graphics == GraphicsApi::Ddraw2d || d3d7_class;
    let own_ddraw = ddraw_class && scan.local_overrides.contains("ddraw.dll");
    let (renderer, alternatives, what) = match scan.graphics {
        _ if own_ddraw && d3d7_class => (Wined3d, vec![], "Direct3D ≤ 7"),
        _ if own_ddraw => (Wined3d, vec![], "DirectDraw 2D"),
        GraphicsApi::Ddraw2d if !bits64 => (CncDdraw, vec![D7vk, Wined3d], "DirectDraw 2D"),
        GraphicsApi::Ddraw2d => (Wined3d, vec![], "DirectDraw 2D (cnc-ddraw is 32-bit)"),
        GraphicsApi::D3dLegacy if has(G::D3d9) => (Dxvk, vec![Wined3d], "Direct3D 9"),
        GraphicsApi::D3dLegacy if has(G::D3d8) => (Dxvk, vec![Wined3d], "Direct3D 8"),
        GraphicsApi::D3dLegacy if d3d7_class && retained_mode(scan) => (
            Wined3d,
            vec![],
            "Direct3D ≤ 7 retained mode (D7VK has none)",
        ),
        GraphicsApi::D3dLegacy if d3d7_class && bits64 => {
            (Wined3d, vec![], "Direct3D ≤ 7 (D7VK is 32-bit)")
        }
        GraphicsApi::D3dLegacy if d3d7_class => (D7vk, vec![CncDdraw, Wined3d], "Direct3D ≤ 7"),
        GraphicsApi::D3dLegacy => (Wined3d, vec![], "Direct3D 9 or older"),
        GraphicsApi::D3d11 => (Dxvk, vec![Wined3d], "Direct3D 10/11"),
        GraphicsApi::D3d12 => (Vkd3dProton, vec![Wined3d], "Direct3D 12"),
        GraphicsApi::OpenGl => (Wined3d, vec![], "OpenGL"),
        GraphicsApi::Vulkan => (Wined3d, vec![], "Vulkan"),
        GraphicsApi::Unknown => (Wined3d, vec![], "no graphics API found"),
    };
    let mut reason = reason_for(scan, what);
    if d3d7_class && let Some(note) = d3d7_note(scan, true) {
        reason.push_str(&note);
    }
    if scan.ddrawcompat {
        reason.push_str(
            "; the game's DDrawCompat stays unloaded (it doesn't run under Wine): Wine's builtin \
             ddraw, no cnc-ddraw, no D7VK",
        );
    } else if own_ddraw {
        reason.push_str("; the game's own ddraw.dll is kept (ddraw=n,b): no cnc-ddraw, no D7VK");
    }
    Pick {
        renderer,
        alternatives,
        reason,
        warning: None,
        x86_64: bits64,
        d3dmetal: false,
        d3d8: renderer == Dxvk && has(G::D3d8) && !has(G::D3d9),
    }
}

/// [`pick_with`] for Linux: L1 from an import scan; a failed scan picks
/// wined3d (Proton's defaults). A cnc-ddraw gogo placed earlier isn't the
/// game's own `ddraw.dll`.
pub fn pick_linux(scan: &Result<PeScan, pe::ScanError>) -> Pick {
    match scan {
        Ok(scan) => {
            let mut scan = scan.clone();
            if ddraw::is_gogo(&scan_exe_dir(&scan)) {
                scan.local_overrides.remove("ddraw.dll");
            }
            from_scan_linux(&scan)
        }
        Err(e) => Pick {
            renderer: Renderer::Wined3d,
            alternatives: vec![],
            reason: format!("import scan failed ({e})"),
            warning: None,
            x86_64: false,
            d3dmetal: false,
            d3d8: false,
        },
    }
}

/// "Try next renderer" on `os` (design 16 §8.2's walk): the renderer after
/// `current` in `pick`'s order, written to L4 under `[platforms.<os>]`
/// unless `dry_run`. `Ok(None)`: [`NO_MORE_RENDERERS`]. `current` is what
/// setup last applied, as on the Mac.
pub fn next_renderer_on(
    os: Os,
    gogid: &str,
    pick: &Pick,
    current: &str,
    dry_run: bool,
) -> Result<Option<String>, MacAppError> {
    let consent = os == Os::Macos && d3dmetal_available();
    let Some(next) = next_after(pick, current, consent) else {
        return Ok(None);
    };
    let next = next.default_ref();
    if !dry_run {
        compat::set_user_keys_for(gogid, &[("renderer", Some(&next))], os)?;
    }
    Ok(Some(next))
}

/// What setup does in the game dir for a Linux renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DdrawStep {
    /// Nothing there to do.
    None,
    /// Put gogo's cnc-ddraw there ([`ddraw::apply`]).
    PlaceCncDdraw,
    /// Take gogo's cnc-ddraw out again ([`ddraw::remove_gogo`]).
    RemoveGogo,
    /// The game's own `ddraw.dll` stays, with `ddraw=n,b`.
    KeepGames,
    /// The game's DDrawCompat stays, unloaded ([`ddraw::is_ddrawcompat`]).
    LeaveDdrawCompat,
}

/// What a Linux renderer needs (design 18 §3): Proton switches, DLL
/// overrides and the game-dir step. [`plan_linux`] works it out without
/// touching anything; [`apply_linux`] does the game-dir step.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LinuxPlan {
    /// The ref in effect, after any fallback.
    pub renderer: String,
    /// Proton switches for the launch environment.
    pub env: BTreeMap<String, String>,
    /// `ddraw=n,b` for a game-dir `ddraw.dll` (cnc-ddraw or the game's).
    pub dll_overrides: BTreeMap<String, String>,
    pub ddraw: DdrawStep,
    pub notes: Vec<String>,
    pub warnings: Vec<String>,
}

/// Work out `renderer` (a Linux ref) for one game: `l1` is its
/// [`pick_linux`] (the walk order, the D3D8 class), `game_dir` the entry
/// exe's dir ([`ddraw::dir_for`]: every `ddraw.dll` question is about that
/// dir, not the install root), `proton` the build's root.
///
/// - **dxvk**, **vkd3d-proton**: nothing, Proton's defaults; D3D8 adds
///   [`PROTON_DXVK_D3D8`].
/// - **d7vk**: [`PROTON_USE_D7VK`] and gogo's cnc-ddraw out. A game that
///   ships its own `ddraw.dll` keeps it with a warning, and a build without
///   [`D7VK_DLL`] warns; either way the next renderer in `l1`'s order is
///   used.
/// - **cnc-ddraw**: placed in the game dir with `ddraw=n,b`, or the game's
///   own `ddraw.dll` kept.
/// - **DDrawCompat beside the exe** ([`ddraw::is_ddrawcompat`]): never loaded.
///   D7VK (Proton appends `ddraw=n,b` for it, which would load it) and
///   cnc-ddraw (never placed over a game's DLL) warn and fall back.
/// - **wined3d**: [`PROTON_USE_WINED3D`], except as `l1`'s own pick, where
///   design 18 §3 says "nothing" (Proton's defaults: a packed D3D9 game the
///   scan missed stays on DXVK).
///
/// Never both `ddraw.dll`s (R17): D7VK is never set while one sits in the
/// game dir, and every renderer but cnc-ddraw takes gogo's out. The game's
/// own `ddraw.dll` always gets `ddraw=n,b`. Anything that can't be set up
/// falls back along the order, ending at wined3d.
pub fn plan_linux(renderer: &str, l1: &Pick, game_dir: Option<&Path>, proton: &Path) -> LinuxPlan {
    let mut out = LinuxPlan {
        renderer: Renderer::Wined3d.id().into(),
        env: BTreeMap::new(),
        dll_overrides: BTreeMap::new(),
        ddraw: DdrawStep::None,
        notes: vec![],
        warnings: vec![],
    };
    let dir = game_dir.filter(|d| d.is_dir());
    let gogo = dir.is_some_and(ddraw::is_gogo);
    let compat = dir.is_some_and(ddraw::is_ddrawcompat);
    let own = !compat && dir.is_some_and(ddraw::is_games);
    let mut r = match RendererRef::parse_on(renderer, Os::Linux) {
        Ok(r) => r.renderer,
        Err(e) => {
            out.warnings.push(format!("{e}: using wined3d"));
            Renderer::Wined3d
        }
    };
    let mut tried = vec![];
    loop {
        tried.push(r);
        let unusable = match r {
            Renderer::D7vk | Renderer::CncDdraw if compat => Some(DDRAWCOMPAT_STAYS.to_string()),
            Renderer::D7vk if own => Some(
                "the game ships its own ddraw.dll, which would win over D7VK's: keeping the \
                 game's"
                    .to_string(),
            ),
            Renderer::D7vk if !has_d7vk(proton) => Some(format!(
                "the Proton build has no D7VK ({} not found)",
                proton.join(D7VK_DLL).display()
            )),
            Renderer::CncDdraw if dir.is_none() => Some("game dir not found".to_string()),
            _ => None,
        };
        let Some(why) = unusable else { break };
        let next = next_after(l1, r.id(), false)
            .filter(|n| !tried.contains(n))
            .unwrap_or(Renderer::Wined3d);
        out.warnings
            .push(format!("{}: {why}: using {}", r.name(), next.id()));
        if next == r {
            break;
        }
        r = next;
    }
    match r {
        Renderer::D7vk => {
            out.env.insert(PROTON_USE_D7VK.into(), "1".into());
            out.notes.push(format!("D7VK: {PROTON_USE_D7VK}=1"));
        }
        Renderer::Dxvk if l1.d3d8 => {
            out.env.insert(PROTON_DXVK_D3D8.into(), "1".into());
            out.notes
                .push(format!("DXVK for Direct3D 8: {PROTON_DXVK_D3D8}=1"));
        }
        Renderer::Wined3d if l1.renderer != Renderer::Wined3d => {
            out.env.insert(PROTON_USE_WINED3D.into(), "1".into());
            out.notes.push(format!("wined3d: {PROTON_USE_WINED3D}=1"));
        }
        Renderer::CncDdraw if own => {}
        Renderer::CncDdraw => {
            out.ddraw = DdrawStep::PlaceCncDdraw;
            out.dll_overrides.insert("ddraw".into(), "n,b".into());
        }
        _ => {}
    }
    if own {
        out.ddraw = DdrawStep::KeepGames;
        out.dll_overrides.insert("ddraw".into(), "n,b".into());
        out.notes
            .push("the game dir's own ddraw.dll is kept (ddraw=n,b)".into());
    } else if compat {
        out.ddraw = DdrawStep::LeaveDdrawCompat;
        out.notes.push(
            "the game's DDrawCompat stays unloaded (it doesn't run under Wine): Wine's builtin \
             ddraw runs"
                .into(),
        );
    } else if gogo && r != Renderer::CncDdraw {
        out.ddraw = DdrawStep::RemoveGogo;
    }
    out.renderer = if r == Renderer::Wined3d {
        label_linux(r, l1).into()
    } else {
        r.default_ref()
    };
    out
}

/// [`plan_linux`], then its game-dir step: cnc-ddraw placed
/// ([`ddraw::resolve_or_provision`], [`ddraw::apply`]) or gogo's taken out
/// ([`ddraw::remove_gogo`]). A cnc-ddraw that can't be placed, or a D7VK
/// with a `ddraw.dll` still in the game dir afterwards, falls back along
/// `l1`'s order. Running it is setup's job (#176).
pub fn apply_linux(renderer: &str, l1: &Pick, game_dir: Option<&Path>, proton: &Path) -> LinuxPlan {
    let mut warnings = vec![];
    let mut renderer = renderer.to_string();
    let mut tried = vec![];
    loop {
        let mut plan = plan_linux(&renderer, l1, game_dir, proton);
        plan.warnings.splice(0..0, warnings.drain(..));
        let failed = match (plan.ddraw, game_dir) {
            (DdrawStep::PlaceCncDdraw, Some(dir)) => {
                let mut provision = vec![];
                let tree = ddraw::resolve_or_provision(&mut |ev| {
                    if let super::ProgressEvent::Warn { message } = ev {
                        provision.push(message.clone());
                    }
                });
                plan.warnings.extend(provision);
                match tree.map(|t| ddraw::apply(&t, dir)) {
                    Some(Ok(ddraw::Applied::Applied { shaders })) => {
                        plan.notes.push(format!(
                            "cnc-ddraw (dll + ini, {shaders} shaders) in the game dir"
                        ));
                        None
                    }
                    Some(Ok(ddraw::Applied::AlreadyPresent)) => None,
                    Some(Err(e)) => Some(e.to_string()),
                    None => Some("provisioning failed".to_string()),
                }
            }
            (DdrawStep::RemoveGogo, Some(dir)) => {
                if ddraw::remove_gogo(dir) {
                    plan.notes
                        .push("removed gogo's cnc-ddraw from the game dir".into());
                }
                None
            }
            _ => None,
        };
        // Never both (R17), whatever the step did.
        let failed = failed.or_else(|| {
            (plan.env.contains_key(PROTON_USE_D7VK) && game_dir.is_some_and(ddraw::has_dll))
                .then(|| "a ddraw.dll is still in the game dir, which would win over D7VK's".into())
        });
        let Some(why) = failed else { return plan };
        let Ok(r) = applied_renderer(&plan.renderer, Os::Linux) else {
            return plan;
        };
        tried.push(r);
        let next = next_after(l1, r.id(), false)
            .filter(|n| !tried.contains(n))
            .unwrap_or(Renderer::Wined3d);
        warnings = plan.warnings;
        warnings.push(format!("{}: {why}: using {}", r.name(), next.id()));
        renderer = next.default_ref();
    }
}

/// A scan with just the fields the tables read (tests, both lanes).
#[cfg(test)]
pub(crate) fn test_scan(
    graphics: GraphicsApi,
    bitness: Bitness,
    imports: &[pe::GraphicsImport],
) -> PeScan {
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
        runtime_graphics: std::collections::BTreeSet::new(),
        helpers: std::collections::BTreeSet::new(),
        runtime_loaded: std::collections::BTreeSet::new(),
        local_overrides: std::collections::BTreeSet::new(),
        ddrawcompat: false,
        d3d7_confidence: None,
        dxguid_block: false,
        renderer_dlls: std::collections::BTreeSet::new(),
        files: vec![],
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::macapp::pe::GraphicsImport;

    use super::test_scan as scan;

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

    /// #173: the D3D ≤ 7 confidence shows in the reason; the Mac table
    /// (wined3d for every D3D ≤ 7 level) doesn't change.
    #[test]
    fn d3d7_confidence_is_in_the_reason_only() {
        use pe::D3d7Confidence as C;
        let imports = [GraphicsImport::Ddraw, GraphicsImport::Direct3d7];
        for (c, note) in [
            (C::Strong, None),
            (C::Medium, Some("interface GUIDs only")),
            (C::DxguidBlock, Some("dxguid GUID block: may be 2D")),
        ] {
            let mut s = scan(GraphicsApi::D3dLegacy, Bitness::I386, &imports);
            s.d3d7_confidence = Some(c);
            s.dxguid_block = c == C::DxguidBlock;
            let p = from_scan(&s, false);
            assert_eq!(p.renderer, Renderer::Wined3d, "{c:?}");
            assert!(p.alternatives.is_empty());
            match note {
                Some(n) => assert!(p.reason.ends_with(n), "{}", p.reason),
                None => assert!(!p.reason.contains("GUID"), "{}", p.reason),
            }
        }
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
            d3d8: false,
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

    /// #199: DDrawCompat is left alone and unloaded; another game DLL keeps
    /// today's rule (an override only for cnc-ddraw); gogo's goes when it
    /// isn't the renderer.
    #[test]
    fn apply_never_loads_ddrawcompat() {
        let g = tempfile::tempdir().unwrap();
        std::fs::write(g.path().join("ddraw.dll"), "game").unwrap();
        let a = apply("wined3d", fake_runtime(), Some(g.path()));
        assert!(a.dll_overrides.is_empty(), "{a:?}");

        let g = tempfile::tempdir().unwrap();
        let dll = g.path().join("ddraw.dll");
        std::fs::write(&dll, b"MZ..DDrawCompat loaded successfully..").unwrap();
        for r in ["wined3d", "cnc-ddraw"] {
            let a = apply(r, fake_runtime(), Some(g.path()));
            assert_eq!(a.renderer, "wined3d", "{a:?}");
            assert!(a.dll_overrides.is_empty(), "{a:?}");
            assert_eq!(a.warnings.len(), usize::from(r != "wined3d"), "{a:?}");
            assert!(dll.is_file());
        }
        let mut s = scan(
            GraphicsApi::Ddraw2d,
            Bitness::I386,
            &[GraphicsImport::Ddraw],
        );
        s.ddrawcompat = true;
        s.local_overrides.insert("ddraw.dll".into());
        let p = from_scan(&s, false);
        assert_eq!((p.renderer, p.alternatives.len()), (Renderer::Wined3d, 0));
        assert!(
            p.reason.contains("DDrawCompat stays unloaded"),
            "{}",
            p.reason
        );

        let g = tempfile::tempdir().unwrap();
        std::fs::write(g.path().join("ddraw.dll"), "cnc").unwrap();
        std::fs::write(g.path().join("ddraw.ini"), "; gogo: x\n").unwrap();
        let a = apply("wined3d", fake_runtime(), Some(g.path()));
        assert!(a.dll_overrides.is_empty(), "{a:?}");
        assert!(!g.path().join("ddraw.dll").exists());
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
        std::fs::write(
            db.path().join("42.toml"),
            "[platforms.macos]\nrenderer = \"d3dmetal\"\n",
        )
        .unwrap();
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

/// The Linux table, walk and setup (design 18 §3, #172), on any OS; and
/// the Mac picks they must not change.
#[cfg(test)]
mod linux_tests {
    use super::*;
    use crate::macapp::pe::{D3d7Confidence as C, GraphicsImport as G};
    use Bitness::*;
    use GraphicsApi as A;
    use Renderer::*;

    use super::test_scan as scan;

    fn d3d7(c: C, bits: Bitness) -> PeScan {
        let mut s = scan(A::D3dLegacy, bits, &[G::Ddraw, G::Direct3d7]);
        s.d3d7_confidence = Some(c);
        s.dxguid_block = c == C::DxguidBlock;
        s
    }

    fn file(path: &str, imports: &[&str]) -> pe::PeFileScan {
        serde_json::from_value(serde_json::json!({
            "path": path,
            "bitness": "i386",
            "imports": imports,
            "runtime_loaded": [],
            "d3d7_iids": false,
        }))
        .unwrap()
    }

    fn walk(pick: &Pick) -> Vec<&'static str> {
        let mut seen = vec![pick.renderer.id()];
        while let Some(n) = next_after(pick, &pick_ref(seen.last().unwrap()), false) {
            assert!(seen.len() < 8, "cycle: {seen:?}");
            seen.push(n.id());
        }
        seen
    }

    /// What setup last applied: versioned for cnc-ddraw, as `state.toml` has it.
    fn pick_ref(id: &str) -> String {
        Renderer::from_id_on(id, Os::Linux).unwrap().default_ref()
    }

    /// Design 18 §3, one row per L1 class: the pick, its order and the D3D8 flag.
    #[test]
    fn table_rows_and_walk_order() {
        let mut rm = d3d7(C::Strong, I386);
        rm.files = vec![file("Game.exe", &["ddraw.dll", "d3drm.dll"])];
        let mut d3dxof = d3d7(C::Strong, I386);
        d3dxof.files = vec![file("bin/Engine.dll", &["d3dxof.dll"])];
        let mut clickteam = scan(A::D3dLegacy, I386, &[]);
        clickteam.engine = Some(pe::Engine::Clickteam);
        clickteam.graphics_source = GraphicsSource::Engine;
        let mut unity = scan(A::D3d11, X86_64, &[G::Opengl32]);
        unity.engine = Some(pe::Engine::Unity);
        unity.graphics_source = GraphicsSource::RuntimeStrings;
        unity.runtime_graphics.insert(G::D3d11);
        let mut runtime_d3d8 = scan(A::D3dLegacy, I386, &[]);
        runtime_d3d8.graphics_source = GraphicsSource::RuntimeStrings;
        runtime_d3d8.runtime_graphics.insert(G::D3d8);
        // (what, scan, pick, walk, d3d8)
        let rows: Vec<(&str, PeScan, Renderer, &[&str], bool)> = vec![
            (
                "2D 32",
                scan(A::Ddraw2d, I386, &[G::Ddraw]),
                CncDdraw,
                &["cnc-ddraw", "d7vk", "wined3d"],
                false,
            ),
            (
                "2D 64",
                scan(A::Ddraw2d, X86_64, &[G::Ddraw]),
                Wined3d,
                &["wined3d"],
                false,
            ),
            (
                "D3D≤7 strong",
                d3d7(C::Strong, I386),
                D7vk,
                &["d7vk", "cnc-ddraw", "wined3d"],
                false,
            ),
            (
                "D3D≤7 medium",
                d3d7(C::Medium, I386),
                D7vk,
                &["d7vk", "cnc-ddraw", "wined3d"],
                false,
            ),
            (
                "D3D≤7 dxguid_block",
                d3d7(C::DxguidBlock, I386),
                D7vk,
                &["d7vk", "cnc-ddraw", "wined3d"],
                false,
            ),
            (
                "D3D≤7 64",
                d3d7(C::Strong, X86_64),
                Wined3d,
                &["wined3d"],
                false,
            ),
            ("D3D≤7 d3drm", rm, Wined3d, &["wined3d"], false),
            ("D3D≤7 d3dxof", d3dxof, Wined3d, &["wined3d"], false),
            (
                "D3D8",
                scan(A::D3dLegacy, I386, &[G::D3d8]),
                Dxvk,
                &["dxvk", "wined3d"],
                true,
            ),
            (
                "D3D8 runtime",
                runtime_d3d8,
                Dxvk,
                &["dxvk", "wined3d"],
                true,
            ),
            (
                "D3D8 + D3D9",
                scan(A::D3dLegacy, I386, &[G::D3d8, G::D3d9]),
                Dxvk,
                &["dxvk", "wined3d"],
                false,
            ),
            (
                "D3D9 + ddraw/D3D7",
                scan(A::D3dLegacy, X86_64, &[G::Ddraw, G::Direct3d7, G::D3d9]),
                Dxvk,
                &["dxvk", "wined3d"],
                false,
            ),
            (
                "D3D11",
                scan(A::D3d11, I386, &[G::D3d11, G::Dxgi]),
                Dxvk,
                &["dxvk", "wined3d"],
                false,
            ),
            ("Unity D3D11", unity, Dxvk, &["dxvk", "wined3d"], false),
            (
                "D3D12 64",
                scan(A::D3d12, X86_64, &[G::D3d12]),
                Vkd3dProton,
                &["vkd3d-proton", "wined3d"],
                false,
            ),
            (
                "D3D12 32",
                scan(A::D3d12, I386, &[G::D3d12]),
                Vkd3dProton,
                &["vkd3d-proton", "wined3d"],
                false,
            ),
            (
                "OpenGL",
                scan(A::OpenGl, X86_64, &[G::Opengl32]),
                Wined3d,
                &["wined3d"],
                false,
            ),
            (
                "Vulkan",
                scan(A::Vulkan, X86_64, &[G::Vulkan]),
                Wined3d,
                &["wined3d"],
                false,
            ),
            ("Clickteam", clickteam, Wined3d, &["wined3d"], false),
            (
                "nothing",
                scan(A::Unknown, I386, &[]),
                Wined3d,
                &["wined3d"],
                false,
            ),
        ];
        for (what, s, want, order, d3d8) in rows {
            let p = from_scan_linux(&s);
            assert_eq!(p.renderer, want, "{what}: {}", p.reason);
            assert_eq!(walk(&p), order, "{what}");
            assert_eq!(p.d3d8, d3d8, "{what}");
            assert!(!p.d3dmetal && p.warning.is_none(), "{what}");
            // Never both: cnc-ddraw and D7VK only ever follow each other.
            assert!(
                !(p.renderer == D7vk && p.alternatives.contains(&D7vk)),
                "{what}"
            );
        }
        // A runtime name that doesn't decide (a static import does) is ignored.
        let mut s = scan(A::D3dLegacy, I386, &[G::D3d9]);
        s.runtime_graphics.insert(G::D3d8);
        assert!(!from_scan_linux(&s).d3d8);
    }

    /// #173's three confidences, named in the reason.
    #[test]
    fn d3d7_confidence_is_in_the_reason() {
        for (c, note) in [
            (C::Strong, "from a device/work GUID or a d3dim/d3drm import"),
            (C::Medium, "from interface GUIDs only"),
            (C::DxguidBlock, "only from the dxguid GUID block: may be 2D"),
        ] {
            let p = from_scan_linux(&d3d7(c, I386));
            assert_eq!(
                p.reason,
                format!(
                    "Direct3D ≤ 7: imports ddraw, direct3d7 (32-bit Game.exe); Direct3D ≤ 7 {note}"
                ),
                "{c:?}"
            );
        }
        let mut s = d3d7(C::Strong, I386);
        s.renderer_dlls.insert("d3ddrv.dll".into());
        let p = from_scan_linux(&s);
        assert!(
            p.reason.ends_with("from the renderer DLL d3ddrv.dll"),
            "{}",
            p.reason
        );
        // The 64-bit and retained-mode rows say why they aren't D7VK.
        let p = from_scan_linux(&d3d7(C::Medium, X86_64));
        assert!(
            p.reason.starts_with("Direct3D ≤ 7 (D7VK is 32-bit)"),
            "{}",
            p.reason
        );
        // No note outside the D3D ≤ 7 class.
        let p = from_scan_linux(&scan(A::D3d11, I386, &[G::D3d11]));
        assert_eq!(p.reason, "Direct3D 10/11: imports d3d11 (32-bit Game.exe)");
    }

    /// A game that ships its own ddraw.dll keeps it: no cnc-ddraw, no D7VK.
    #[test]
    fn shipped_ddraw_dll_is_neither() {
        for s in [
            scan(A::Ddraw2d, I386, &[G::Ddraw]),
            d3d7(C::Strong, I386),
            d3d7(C::DxguidBlock, I386),
        ] {
            let mut s = s;
            s.local_overrides.insert("ddraw.dll".into());
            let p = from_scan_linux(&s);
            assert_eq!((p.renderer, walk(&p)), (Wined3d, vec!["wined3d"]));
            assert!(
                p.reason
                    .ends_with("own ddraw.dll is kept (ddraw=n,b): no cnc-ddraw, no D7VK"),
                "{}",
                p.reason
            );
        }
        // DDrawCompat: the same pick, a different reason (#199).
        let mut s = d3d7(C::DxguidBlock, I386);
        s.local_overrides.insert("ddraw.dll".into());
        s.ddrawcompat = true;
        let p = from_scan_linux(&s);
        assert_eq!((p.renderer, walk(&p)), (Wined3d, vec!["wined3d"]));
        assert!(
            p.reason.contains("DDrawCompat stays unloaded"),
            "{}",
            p.reason
        );
        // gogo's own cnc-ddraw from an earlier setup isn't the game's.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ddraw.dll"), "x").unwrap();
        std::fs::write(dir.path().join("ddraw.ini"), ddraw::DDRAW_INI).unwrap();
        let mut s = d3d7(C::Medium, I386);
        s.root = dir.path().to_path_buf();
        s.local_overrides.insert("ddraw.dll".into());
        assert_eq!(pick_linux(&Ok(s)).renderer, D7vk);
        // Nor beside an exe in a subdir, where it goes since #199.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Apps")).unwrap();
        std::fs::write(dir.path().join("Apps/ddraw.dll"), "x").unwrap();
        std::fs::write(dir.path().join("Apps/ddraw.ini"), ddraw::DDRAW_INI).unwrap();
        let mut s = d3d7(C::Medium, I386);
        s.root = dir.path().to_path_buf();
        s.main_exe = "Apps/game.exe".into();
        s.local_overrides.insert("ddraw.dll".into());
        assert_eq!(pick_linux(&Ok(s)).renderer, D7vk);
        // A D3D9 game's shipped ddraw.dll doesn't change its class.
        let mut s = scan(A::D3dLegacy, I386, &[G::D3d9]);
        s.local_overrides.insert("ddraw.dll".into());
        assert_eq!(from_scan_linux(&s).renderer, Dxvk);
        // A failed scan: Proton's defaults.
        let e = pick_linux(&Err(pe::ScanError::NoExecutable { dir: "/g".into() }));
        assert_eq!((e.renderer, e.alternatives.len()), (Wined3d, 0));
    }

    #[test]
    fn linux_ids_and_the_mac_only_error() {
        for (s, r) in [
            ("dxvk", Dxvk),
            ("vkd3d-proton", Vkd3dProton),
            ("d7vk", D7vk),
            ("wined3d", Wined3d),
        ] {
            let p = RendererRef::parse_on(s, Os::Linux).unwrap();
            assert_eq!((p.renderer, p.version()), (r, None));
            assert_eq!(r.default_ref(), s);
        }
        let p = RendererRef::parse_on("cnc-ddraw/7.0.0.0", Os::Linux).unwrap();
        assert_eq!(p.version().as_deref(), Some("7.0.0.0"));
        assert!(CncDdraw.default_ref().starts_with("cnc-ddraw/"));
        // Mac-only ids: compat's error, here and in [platforms.linux].
        for id in ["dxmt", "d3dmetal/4.0b3"] {
            let e = RendererRef::parse_on(id, Os::Linux).unwrap_err();
            assert!(e.ends_with("runs on macOS only"), "{e}");
            let raw = format!("[platforms.linux]\nrenderer = \"{id}\"\n");
            let e = compat::parse(&raw, Os::Linux, Layer::Db).unwrap_err();
            assert!(e.ends_with("runs on macOS only"), "{e}");
        }
        let e = RendererRef::parse_on("dxvk/v2.7", Os::Linux).unwrap_err();
        assert!(e.contains("takes no version"), "{e}");
        let e = RendererRef::parse_on("dgvoodoo", Os::Linux).unwrap_err();
        assert!(
            e.ends_with("(dxvk, vkd3d-proton, d7vk, cnc-ddraw, wined3d)"),
            "{e}"
        );
        // The Mac lane keeps its ids and message.
        for id in ["dxvk", "vkd3d-proton", "d7vk"] {
            let e = RendererRef::parse(id).unwrap_err();
            assert!(e.ends_with("(wined3d, dxmt, cnc-ddraw, d3dmetal)"), "{e}");
        }
        assert_eq!(RendererRef::parse("dxmt").unwrap().renderer, Dxmt);
        // Setup warns and falls back on a ref it can't use.
        let l1 = from_scan_linux(&scan(A::D3d11, I386, &[G::D3d11]));
        let plan = plan_linux("dxmt/v0.80", &l1, None, Path::new("/none"));
        assert_eq!(plan.renderer, "wined3d");
        assert!(
            plan.warnings[0].contains("runs on macOS only"),
            "{:?}",
            plan.warnings
        );
    }

    /// A fake Proton build root, with or without D7VK.
    fn proton(d7vk: bool) -> tempfile::TempDir {
        let p = tempfile::tempdir().unwrap();
        if d7vk {
            let dll = p.path().join(D7VK_DLL);
            std::fs::create_dir_all(dll.parent().unwrap()).unwrap();
            std::fs::write(dll, "MZ").unwrap();
        }
        p
    }

    /// A game dir: empty, with gogo's cnc-ddraw, or with the game's own ddraw.dll.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Dir {
        Empty,
        Gogo,
        Own,
    }

    fn game(state: Dir) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        if state != Dir::Empty {
            std::fs::write(d.path().join("ddraw.dll"), "game").unwrap();
        }
        if state == Dir::Gogo {
            std::fs::write(d.path().join("ddraw.ini"), ddraw::DDRAW_INI).unwrap();
        }
        d
    }

    fn env(plan: &LinuxPlan) -> Vec<&str> {
        plan.env.keys().map(String::as_str).collect()
    }

    /// What each renderer needs, without touching anything.
    #[test]
    fn plan_per_renderer() {
        let p = proton(true);
        let g = game(Dir::Empty);
        let d3d8 = from_scan_linux(&scan(A::D3dLegacy, I386, &[G::D3d8]));
        let d3d9 = from_scan_linux(&scan(A::D3dLegacy, I386, &[G::D3d9]));
        let d3d7 = from_scan_linux(&d3d7(C::Medium, I386));
        let nothing = from_scan_linux(&scan(A::Unknown, I386, &[]));
        let plan = |r: &str, l1: &Pick| plan_linux(r, l1, Some(g.path()), p.path());

        let x = plan("dxvk", &d3d9);
        assert_eq!((x.renderer.as_str(), env(&x)), ("dxvk", vec![]));
        let x = plan("dxvk", &d3d8);
        assert_eq!(env(&x), [PROTON_DXVK_D3D8]);
        let x = plan("vkd3d-proton", &d3d9);
        assert!(x.env.is_empty() && x.dll_overrides.is_empty());
        let x = plan("d7vk", &d3d7);
        assert_eq!(
            (x.renderer.as_str(), env(&x)),
            ("d7vk", vec![PROTON_USE_D7VK])
        );
        assert_eq!((x.ddraw, x.dll_overrides.len()), (DdrawStep::None, 0));
        let x = plan("cnc-ddraw", &d3d7);
        assert_eq!((x.ddraw, env(&x)), (DdrawStep::PlaceCncDdraw, vec![]));
        assert_eq!(
            x.dll_overrides.get("ddraw").map(String::as_str),
            Some("n,b")
        );
        assert!(x.renderer.starts_with("cnc-ddraw/"));
        // wined3d: the switch when chosen over another pick, nothing as L1's own.
        let x = plan("wined3d", &d3d9);
        assert_eq!(env(&x), [PROTON_USE_WINED3D]);
        let x = plan("wined3d", &nothing);
        assert!(x.env.is_empty() && x.warnings.is_empty(), "{x:?}");
        // Nothing was written.
        assert_eq!(std::fs::read_dir(g.path()).unwrap().count(), 0);
    }

    /// #199: where a `ddraw.dll` counts.
    #[test]
    fn the_ddraw_dir_is_the_entry_exes() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::create_dir_all(root.join("Apps")).unwrap();
        let apps = root.join("Apps");
        let dir_for = |exe: &Path| ddraw::dir_for(root, Some(exe));
        assert_eq!(dir_for(&apps.join("SC3U.exe")), apps);
        assert_eq!(dir_for(&root.join("game.exe")), root);
        assert_eq!(dir_for(&root.join("No/x.exe")), root);
        assert_eq!(dir_for(Path::new("/elsewhere/x.exe")), root);
        assert_eq!(ddraw::dir_for(root, None), root);

        assert!(!ddraw::has_dll(&apps));
        std::fs::create_dir(apps.join("ddraw.dll")).unwrap();
        assert!(!ddraw::has_dll(&apps), "a dir isn't one");
        std::fs::remove_dir(apps.join("ddraw.dll")).unwrap();
        std::fs::write(apps.join("DDRAW.DLL"), "x").unwrap();
        assert!(ddraw::has_dll(&apps) && ddraw::is_games(&apps));
        std::fs::write(apps.join("ddraw.ini"), "; gogo: x\n").unwrap();
        assert!(ddraw::has_dll(&apps) && !ddraw::is_games(&apps));

        let toml = |exe: &str, variant: &str| {
            crate::macapp::launch::LaunchToml::parse(
                &format!(
                    "schema = 1\ngogid = \"1\"\ntitle = \"T\"\nruntime = \"r\"\n\
                 renderer = \"wined3d\"\nexe = '{exe}'\nworkdir = 'C:\\GOG\\T'\n\
                 [gogo]\npath = \"/g\"\nstate = \"/s\"\n\
                 [[variant]]\nname = \"v\"\nexe = '{variant}'\nworkdir = 'C:\\GOG\\T'\n"
                ),
                Path::new("t"),
            )
            .unwrap()
        };
        let t = toml(r"C:\GOG\T\Apps\SC3U.exe", r"C:\GOG\T\tool.exe");
        assert_eq!(t.exe_dir_in(root), apps);
        assert_eq!(
            t.ddraw_dirs_in(root).into_iter().collect::<Vec<_>>(),
            [root.to_path_buf(), apps.clone()]
        );
        // The tree's copy has no such dir: the root.
        assert_eq!(
            toml(r"C:\GOG\T\No\x.exe", r"C:\GOG\T\x.exe").exe_dir_in(root),
            root
        );
        assert_eq!(
            toml(r"C:\GOG\T\x.exe", r"C:\GOG\T\x.exe").exe_dir_in(root),
            root
        );
    }

    /// #199: whatever is asked for, DDrawCompat isn't loaded or replaced, and
    /// a real wrapper is told from it by content, not by name.
    #[test]
    fn ddrawcompat_is_never_loaded() {
        let tree = tempfile::tempdir().unwrap();
        std::fs::write(tree.path().join("ddraw.dll"), "cnc").unwrap();
        let _g = crate::test_util::env_lock();
        // SAFETY: env mutation under the shared env mutex.
        unsafe { std::env::set_var("GOGO_CNC_DDRAW_DIR", tree.path()) };
        let g = tempfile::tempdir().unwrap();
        let dll = g.path().join("DDRAW.DLL");
        std::fs::write(&dll, b"MZ\0\0Loading DDrawCompat \0").unwrap();
        assert!(ddraw::is_ddrawcompat(g.path()));
        let p = proton(true);
        let l1 = from_scan_linux(&d3d7(C::Medium, I386));
        for r in Renderer::LINUX {
            let x = apply_linux(&r.default_ref(), &l1, Some(g.path()), p.path());
            let what = format!("{r:?}: {x:?}");
            assert!(x.dll_overrides.is_empty(), "{what}");
            assert!(!x.env.contains_key(PROTON_USE_D7VK), "{what}");
            assert_eq!(x.ddraw, DdrawStep::LeaveDdrawCompat, "{what}");
            assert!(!x.renderer.starts_with("cnc-ddraw"), "{what}");
            assert!(!x.renderer.starts_with("d7vk"), "{what}");
            assert_eq!(
                std::fs::read(&dll).unwrap(),
                b"MZ\0\0Loading DDrawCompat \0"
            );
            if matches!(r, D7vk | CncDdraw) {
                assert!(x.warnings[0].contains("is DDrawCompat"), "{what}");
            }
        }
        assert_eq!(std::fs::read_dir(g.path()).unwrap().count(), 1);
        unsafe { std::env::remove_var("GOGO_CNC_DDRAW_DIR") };

        // A wrapper that only mentions it, gogo's own, none: not DDrawCompat.
        std::fs::write(&dll, b"MZ\0\0works with DDrawCompat\0").unwrap();
        assert!(!ddraw::is_ddrawcompat(g.path()));
        let g = tempfile::tempdir().unwrap();
        assert!(!ddraw::is_ddrawcompat(g.path()));
        std::fs::write(g.path().join("ddraw.dll"), b"Loading DDrawCompat ").unwrap();
        std::fs::write(g.path().join("ddraw.ini"), ddraw::DDRAW_INI).unwrap();
        assert!(!ddraw::is_ddrawcompat(g.path()));
    }

    /// R17 in every direction: for every renderer × game-dir state × D7VK
    /// build, after setup there is never PROTON_USE_D7VK with a ddraw.dll
    /// in the game dir, the game's own DLL is never touched and always
    /// gets `ddraw=n,b`, and gogo's goes for anything but cnc-ddraw.
    #[test]
    fn never_both_ddraw_dlls() {
        let tree = tempfile::tempdir().unwrap();
        std::fs::write(tree.path().join("ddraw.dll"), "cnc").unwrap();
        let _g = crate::test_util::env_lock();
        // SAFETY: env mutation under the shared env mutex.
        unsafe { std::env::set_var("GOGO_CNC_DDRAW_DIR", tree.path()) };
        let picks = [
            from_scan_linux(&scan(A::Ddraw2d, I386, &[G::Ddraw])),
            from_scan_linux(&d3d7(C::DxguidBlock, I386)),
            from_scan_linux(&scan(A::D3d11, I386, &[G::D3d11])),
        ];
        for l1 in &picks {
            for r in Renderer::LINUX {
                for state in [Dir::Empty, Dir::Gogo, Dir::Own] {
                    for d7vk in [true, false] {
                        let p = proton(d7vk);
                        let g = game(state);
                        let mut l1 = l1.clone();
                        if state == Dir::Own {
                            // The scan sees the game's DLL too.
                            let mut s = scan(A::Ddraw2d, I386, &[G::Ddraw]);
                            s.local_overrides.insert("ddraw.dll".into());
                            if l1.renderer == CncDdraw {
                                l1 = from_scan_linux(&s);
                            }
                        }
                        let x = apply_linux(&r.default_ref(), &l1, Some(g.path()), p.path());
                        let what =
                            format!("{} {r:?} {state:?} d7vk={d7vk}: {x:?}", l1.renderer.id());
                        let dll = g.path().join("ddraw.dll");
                        assert!(
                            !(x.env.contains_key(PROTON_USE_D7VK) && dll.exists()),
                            "{what}"
                        );
                        match state {
                            Dir::Own => {
                                assert_eq!(
                                    std::fs::read_to_string(&dll).unwrap(),
                                    "game",
                                    "{what}"
                                );
                                assert_eq!(x.ddraw, DdrawStep::KeepGames, "{what}");
                                assert_eq!(x.dll_overrides["ddraw"], "n,b", "{what}");
                            }
                            _ if x.renderer.starts_with("cnc-ddraw") => {
                                assert!(dll.exists() && ddraw::is_gogo(g.path()), "{what}");
                                assert_eq!(x.dll_overrides["ddraw"], "n,b", "{what}");
                            }
                            _ => {
                                assert!(!dll.exists(), "{what}");
                                assert!(x.dll_overrides.is_empty(), "{what}");
                            }
                        }
                        if r == D7vk && state == Dir::Own {
                            assert!(x.warnings[0].contains("ships its own ddraw.dll"), "{what}");
                        }
                    }
                }
            }
        }
        // Moving a game from cnc-ddraw to D7VK takes gogo's out first.
        let p = proton(true);
        let g = game(Dir::Empty);
        let l1 = from_scan_linux(&d3d7(C::Medium, I386));
        let x = apply_linux("cnc-ddraw", &l1, Some(g.path()), p.path());
        assert!(x.renderer.starts_with("cnc-ddraw") && ddraw::is_gogo(g.path()));
        let x = apply_linux("d7vk", &l1, Some(g.path()), p.path());
        assert_eq!(
            (x.renderer.as_str(), env(&x)),
            ("d7vk", vec![PROTON_USE_D7VK])
        );
        assert!(
            x.notes
                .iter()
                .any(|n| n.contains("removed gogo's cnc-ddraw"))
        );
        assert!(!g.path().join("ddraw.dll").exists() && !g.path().join("ddraw.ini").exists());
        // A cnc-ddraw that can't be placed (its ini can't be written) falls
        // back along the order: D7VK next, but the copied ddraw.dll is in
        // the way, so wined3d.
        let l2d = from_scan_linux(&scan(A::Ddraw2d, I386, &[G::Ddraw]));
        let bad = tempfile::tempdir().unwrap();
        std::fs::create_dir(bad.path().join("ddraw.ini")).unwrap();
        let x = apply_linux("cnc-ddraw", &l2d, Some(bad.path()), p.path());
        assert_eq!(
            (x.renderer.as_str(), env(&x)),
            ("wined3d", vec![PROTON_USE_WINED3D])
        );
        assert!(x.warnings[0].ends_with(": using d7vk"), "{:?}", x.warnings);
        assert!(
            x.warnings[1].contains("ships its own ddraw.dll"),
            "{:?}",
            x.warnings
        );
        unsafe { std::env::remove_var("GOGO_CNC_DDRAW_DIR") };
    }

    /// A build without D7VK: the next renderer in the order, with a warning.
    #[test]
    fn no_d7vk_falls_back_along_the_order() {
        let p = proton(false);
        assert!(!has_d7vk(p.path()) && has_d7vk(proton(true).path()));
        let g = game(Dir::Empty);
        let d3d7 = from_scan_linux(&d3d7(C::Strong, I386));
        let x = plan_linux("d7vk", &d3d7, Some(g.path()), p.path());
        assert!(x.renderer.starts_with("cnc-ddraw/"), "{x:?}");
        assert_eq!((x.ddraw, env(&x)), (DdrawStep::PlaceCncDdraw, vec![]));
        assert_eq!(x.warnings.len(), 1);
        assert!(
            x.warnings[0].starts_with("D7VK: the Proton build has no D7VK (")
                && x.warnings[0].ends_with("not found): using cnc-ddraw"),
            "{:?}",
            x.warnings
        );
        // No game dir either: on to wined3d.
        let x = plan_linux("d7vk", &d3d7, None, p.path());
        assert_eq!((x.renderer.as_str(), x.warnings.len()), ("wined3d", 2));
        assert_eq!(env(&x), [PROTON_USE_WINED3D]);
        // A 2D game: d7vk is after cnc-ddraw, so wined3d is next.
        let l2d = from_scan_linux(&scan(A::Ddraw2d, I386, &[G::Ddraw]));
        let x = plan_linux("d7vk", &l2d, Some(g.path()), p.path());
        assert_eq!(x.renderer, "wined3d");
        // A D3D11 game forced to d7vk: its own order from the top.
        let l11 = from_scan_linux(&scan(A::D3d11, I386, &[G::D3d11]));
        let x = plan_linux("d7vk", &l11, Some(g.path()), p.path());
        assert_eq!(x.renderer, "dxvk");
    }

    /// L1's own wined3d pick sets nothing: `proton-default` (#229). A wined3d
    /// anyone chose, or fell back to under another L1, is the real one.
    #[test]
    fn l1s_own_wined3d_is_labelled_proton_default() {
        let p = proton(true);
        let g = game(Dir::Empty);
        let none = from_scan_linux(&scan(A::Unknown, I386, &[]));
        assert_eq!(none.renderer, Wined3d);
        let x = plan_linux("wined3d", &none, Some(g.path()), p.path());
        assert_eq!((x.renderer.as_str(), env(&x)), (PROTON_DEFAULT, vec![]));
        // Chosen (not L1's own): setup turns it into the real thing.
        let mut chosen = x.clone();
        assert!(settle_wined3d(&mut chosen, false));
        assert_eq!(chosen.renderer, "wined3d");
        assert_eq!(env(&chosen), [PROTON_USE_WINED3D]);
        let mut auto = x;
        assert!(!settle_wined3d(&mut auto, true));
        assert_eq!(auto.renderer, PROTON_DEFAULT);
        // Another L1 falling back to wined3d sets the switch: wined3d.
        let l11 = from_scan_linux(&scan(A::D3d11, I386, &[G::D3d11]));
        let x = plan_linux("wined3d", &l11, Some(g.path()), p.path());
        assert_eq!(
            (x.renderer.as_str(), env(&x)),
            ("wined3d", vec![PROTON_USE_WINED3D])
        );
        // Applied refs read back; the label is not a choice.
        assert_eq!(applied_renderer(PROTON_DEFAULT, Os::Linux), Ok(Wined3d));
        assert_eq!(applied_renderer("dxvk", Os::Linux), Ok(Dxvk));
        assert!(RendererRef::parse_on(PROTON_DEFAULT, Os::Linux).is_err());
        // The walk starts from L1's pick.
        assert_eq!(next_after(&none, PROTON_DEFAULT, false), None);
        let ddraw = from_scan_linux(&scan(A::Ddraw2d, I386, &[G::Ddraw])); // cnc-ddraw, d7vk, wined3d
        assert_eq!(next_after(&ddraw, PROTON_DEFAULT, false), None);
    }

    /// L3/L4/flags over the Linux L1, from [platforms.linux] only; the
    /// walk writes there too.
    #[test]
    fn layers_and_the_persisted_walk() {
        let home = tempfile::tempdir().unwrap();
        let db = tempfile::tempdir().unwrap();
        std::fs::write(
            db.path().join("42.toml"),
            "[platforms.macos]\nrenderer = \"dxmt\"\n[platforms.linux]\nrenderer = \"cnc-ddraw\"\n",
        )
        .unwrap();
        let _g = crate::test_util::env_lock();
        // SAFETY: env mutation under the shared env mutex.
        unsafe {
            std::env::set_var("GOGO_HOME", home.path());
            std::env::set_var(compat::DB_DIR_ENV, db.path());
        }
        let l1 = from_scan_linux(&d3d7(C::DxguidBlock, I386));
        let auto = RendererChoice::Auto;
        // L1.
        let s = select_on(Os::Linux, Some("1"), l1.clone(), auto);
        assert_eq!(
            (s.renderer.value.as_str(), s.renderer.layer),
            ("d7vk", Layer::Auto)
        );
        assert!(s.reason.ends_with("may be 2D"), "{}", s.reason);
        // L3 from [platforms.linux]; [platforms.macos] doesn't apply.
        let s = select_on(Os::Linux, Some("42"), l1.clone(), auto);
        assert_eq!(
            (s.renderer.value.as_str(), s.renderer.layer),
            ("cnc-ddraw", Layer::Db)
        );
        let s = select_on(Os::Macos, Some("42"), l1.clone(), auto);
        assert_eq!(
            (s.renderer.value.as_str(), s.renderer.layer),
            ("dxmt", Layer::Db)
        );
        // A flag beats L3; a Mac-only flag keeps the automatic choice.
        let s = select_on(
            Os::Linux,
            Some("42"),
            l1.clone(),
            RendererChoice::Force(D7vk),
        );
        assert_eq!(
            (s.renderer.value.as_str(), s.renderer.layer),
            ("d7vk", Layer::Flags)
        );
        let s = select_on(
            Os::Linux,
            Some("1"),
            l1.clone(),
            RendererChoice::Force(Dxmt),
        );
        assert_eq!(s.renderer.layer, Layer::Auto);
        assert!(
            s.warnings[0].contains("linux has no DXMT"),
            "{:?}",
            s.warnings
        );
        // --no-ddraw-wrapper acts on a cnc-ddraw below it.
        let s = select_on(
            Os::Linux,
            Some("42"),
            l1.clone(),
            RendererChoice::AutoNoDdraw,
        );
        assert_eq!(
            (s.renderer.value.as_str(), s.renderer.layer),
            ("wined3d", Layer::Flags)
        );

        // "Try next renderer": d7vk → cnc-ddraw → wined3d, into [platforms.linux].
        compat::set_user_keys_for("1", &[("renderer", Some("dxmt/v0.80"))], Os::Macos).unwrap();
        let next = |cur: &str| next_renderer_on(Os::Linux, "1", &l1, cur, false).unwrap();
        let cnc = next("d7vk").unwrap();
        assert!(cnc.starts_with("cnc-ddraw/"), "{cnc}");
        let raw = std::fs::read_to_string(compat::user_path("1")).unwrap();
        assert!(
            raw.contains(&format!("[platforms.linux]\nrenderer = \"{cnc}\"")),
            "{raw}"
        );
        // L4 is reported, not baked; a flag replaces it.
        let s = select_on(Os::Linux, Some("1"), l1.clone(), auto);
        assert_eq!(
            (s.renderer.value.as_str(), s.user.as_deref()),
            ("d7vk", Some(cnc.as_str()))
        );
        let s = select_on(
            Os::Linux,
            Some("1"),
            l1.clone(),
            RendererChoice::Force(Wined3d),
        );
        assert_eq!(s.user, None);
        assert!(
            s.reason.contains("replacing the L4 override cnc-ddraw/"),
            "{}",
            s.reason
        );
        assert_eq!(next(&cnc).as_deref(), Some("wined3d"));
        assert_eq!(next("wined3d"), None);
        // A dry run writes nothing; the Mac table was never touched.
        let before = std::fs::read_to_string(compat::user_path("1")).unwrap();
        assert_eq!(
            next_renderer_on(Os::Linux, "1", &l1, "d7vk", true)
                .unwrap()
                .as_deref(),
            Some(cnc.as_str())
        );
        let after = std::fs::read_to_string(compat::user_path("1")).unwrap();
        assert_eq!(before, after);
        let mac = compat::load_user_for("1", Os::Macos)
            .unwrap()
            .entry
            .renderer;
        assert_eq!(mac.as_deref(), Some("dxmt/v0.80"));
        unsafe {
            std::env::remove_var("GOGO_HOME");
            std::env::remove_var(compat::DB_DIR_ENV);
        }
    }

    /// The Mac table is unchanged by #172 (the macOS-gated tests cover the
    /// rest): the same picks, alternatives and reasons on any OS.
    #[test]
    fn mac_picks_unchanged() {
        let rows: Vec<(PeScan, Renderer, &[Renderer])> = vec![
            (scan(A::Ddraw2d, I386, &[G::Ddraw]), CncDdraw, &[Wined3d]),
            (d3d7(C::Strong, I386), Wined3d, &[]),
            (d3d7(C::DxguidBlock, I386), Wined3d, &[]),
            (scan(A::D3dLegacy, I386, &[G::D3d8]), Wined3d, &[]),
            (scan(A::D3dLegacy, I386, &[G::D3d9]), Wined3d, &[]),
            (scan(A::D3d11, I386, &[G::D3d11]), Dxmt, &[Wined3d]),
            (scan(A::D3d12, X86_64, &[G::D3d12]), Wined3d, &[]),
        ];
        for (s, want, alts) in rows {
            let p = from_scan(&s, false);
            assert_eq!((p.renderer, p.alternatives.as_slice()), (want, alts));
            assert!(!p.d3d8);
        }
        let p = from_scan(&d3d7(C::Strong, I386), false);
        assert_eq!(
            p.reason,
            "Direct3D 9 or older: imports ddraw, direct3d7 (32-bit Game.exe)"
        );
        let p = from_scan(&d3d7(C::Medium, I386), false);
        assert!(
            p.reason
                .ends_with("; Direct3D ≤ 7 from interface GUIDs only")
        );
        // Pick's JSON has no new field on the Mac.
        let json = serde_json::to_value(&p).unwrap();
        assert!(json.get("d3d8").is_none(), "{json}");
    }
}
