//! PE import scan (#63, docs/19 §6.2): which graphics API a game uses and how
//! many bits it has, read from its exe and the DLLs next to it.
//!
//! This is the input for L1 renderer selection (#70) and the automatic
//! winetricks verbs (#72). It only reports what is on disk; it never picks a
//! renderer itself.
//!
//! - **Files**: the primary exe (from `goggame-*.info`, else a size heuristic)
//!   plus every `.exe`/`.dll` in its directory. Other exes count only when the
//!   entry exe and DLLs give no graphics evidence (#226). The D3D import is often not in
//!   the exe: Rebel Galaxy's D3D11 lives in `RenderSystem_Direct3D11.dll`.
//! - **Imports**: regular and delay-load import tables, via the `object` crate.
//! - **Direct3D ≤ 7** has no DLL of its own (it comes from `ddraw.dll` through
//!   `QueryInterface`), so a file that imports `ddraw.dll` or names it is
//!   checked for the 44 ddraw/d3d GUIDs that `dxguid.lib` links into the
//!   image (docs/22 §6.2). Device and work GUIDs (`IID_IDirect3DHALDevice`,
//!   `IID_IDirect3DTexture2` …) are strong evidence, the `IID_IDirect3D*`
//!   interface GUIDs alone medium. ≥ 32 of the 44 is a whole `dxguid` set
//!   (DirectX 7's has 43, DirectX 6's 36), which says only "linked
//!   `dxguid`": its D3D hits are the weakest
//!   evidence ([`D3d7Confidence::DxguidBlock`]), but still count as D3D ≤ 7
//!   (docs/22 §6.1: unsure is not 2D). Unreal's `D3DDrv.dll` and LithTech's
//!   `d3d.ren` next to the exe mark a D3D path too.
//! - **Runtime-loaded DLLs**: a `strings`-style scan of the section data
//!   (ASCII and UTF-16LE) for known DLL names, because some games
//!   `LoadLibrary` their renderer. Used for the classification only when no
//!   static import says anything, or for Unity/Unreal (below).
//! - **Engines** (Unity, Unreal, Clickteam, Java) are flagged from their files:
//!   they choose the API at runtime, so for Unity/Unreal the classification
//!   runs over imports and runtime-loaded names together.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use object::read::pe::{ImageNtHeaders, PeFile};
use object::{FileKind, LittleEndian as LE};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::info;

#[derive(Debug, Error)]
pub enum ScanError {
    #[error("no game executable found under {dir}")]
    NoExecutable { dir: PathBuf },
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is not a Windows PE executable")]
    NotPe { path: PathBuf },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Bitness {
    #[serde(rename = "i386")]
    I386,
    #[serde(rename = "x86_64")]
    X86_64,
}

/// One graphics DLL (or, for [`Self::Direct3d7`], interface) a file uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphicsImport {
    Ddraw,
    /// Direct3D ≤ 7 through ddraw (`IID_IDirect3D`…`IID_IDirect3D7`), or
    /// `d3dim`/`d3drm`.
    Direct3d7,
    D3d8,
    D3d9,
    /// `d3d10.dll`, `d3d10_1.dll`, `d3d10core.dll`.
    D3d10,
    D3d11,
    D3d12,
    Dxgi,
    Opengl32,
    Vulkan,
}

/// The classification #70 maps to a renderer (docs/19 §6.2 table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraphicsApi {
    /// `ddraw` only, no Direct3D: a 2D DirectDraw game (cnc-ddraw class).
    #[serde(rename = "ddraw_2d")]
    Ddraw2d,
    /// ddraw + Direct3D ≤ 7, `d3d8`, `d3d9` (wined3d class).
    #[serde(rename = "d3d_legacy")]
    D3dLegacy,
    /// `d3d10*` / `d3d11` / `dxgi` (DXMT class).
    #[serde(rename = "d3d10_11")]
    D3d11,
    /// `d3d12` without `d3d10`/`d3d11`.
    #[serde(rename = "d3d12")]
    D3d12,
    #[serde(rename = "opengl")]
    OpenGl,
    #[serde(rename = "vulkan")]
    Vulkan,
    #[serde(rename = "unknown")]
    Unknown,
}

/// Where [`PeScan::graphics`] came from, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphicsSource {
    /// Regular or delay-load imports (plus the D3D ≤ 7 IIDs).
    Imports,
    /// Implied by the engine (Clickteam → legacy D3D, LWJGL → OpenGL).
    Engine,
    /// DLL names found as strings: probably `LoadLibrary`.
    RuntimeStrings,
    None,
}

/// How sure the scan is that the game uses Direct3D ≤ 7 (docs/22 §5.3, §6.2),
/// weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum D3d7Confidence {
    /// The D3D GUIDs come only with a whole `dxguid` block (≥ 32 of the
    /// 44 ddraw/d3d GUIDs): a D3D game and a 2D one linked against all of
    /// `dxguid.lib` look the same.
    DxguidBlock,
    /// Only `IID_IDirect3D`/`2`/`3`/`7`: real use, or a 2D game probing for
    /// 3D hardware.
    Medium,
    /// A device or work GUID (`IID_IDirect3DHALDevice`, `…Texture2`,
    /// `…Viewport3` …) outside the block, a `d3dim`/`d3drm` import, or a D3D
    /// renderer DLL (Unreal `D3DDrv.dll`, LithTech `d3d.ren`).
    Strong,
}

/// Engines that pick the graphics API at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    Unity,
    Unreal,
    /// Clickteam Multimedia Fusion: DirectDraw, D3D8 or D3D9 by a user setting.
    Clickteam,
    /// A bundled JRE + jar (libGDX/LWJGL: OpenGL).
    Java,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MainExeSource {
    /// The primary `playTasks` entry of `goggame-*.info`.
    GogInfo,
    /// Largest plausible `.exe` (no usable info file).
    Heuristic,
    /// Passed in by the caller.
    Given,
}

/// Scan result for one game tree. Paths are relative to [`Self::root`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeScan {
    pub root: PathBuf,
    pub main_exe: String,
    pub main_exe_source: MainExeSource,
    /// Bitness of the main exe.
    pub bitness: Bitness,
    pub engine: Option<Engine>,
    pub graphics: GraphicsApi,
    pub graphics_source: GraphicsSource,
    /// Graphics APIs imported by any scanned file.
    pub graphics_imports: BTreeSet<GraphicsImport>,
    /// Graphics APIs only named as strings (runtime-loaded candidates).
    pub runtime_graphics: BTreeSet<GraphicsImport>,
    /// Imported `d3dcompiler_*` / `d3dx*` / `xinput*` / `xact*` / `xaudio2_*`
    /// DLLs the game does not ship itself (#72's winetricks input).
    pub helpers: BTreeSet<String>,
    /// Interesting DLL names found only as strings, minus what is imported.
    pub runtime_loaded: BTreeSet<String>,
    /// Graphics or helper DLLs shipped in the game dir (wrappers such as
    /// dgVoodoo or cnc-ddraw, or a bundled `d3dcompiler_47.dll`). Their own
    /// imports are left out of the classification.
    pub local_overrides: BTreeSet<String>,
    /// The `ddraw.dll` beside the exe is DDrawCompat ([`super::ddraw::is_ddrawcompat`]),
    /// which gogo leaves unloaded.
    #[serde(default)]
    pub ddrawcompat: bool,
    /// The strongest Direct3D ≤ 7 evidence behind a [`GraphicsImport::Direct3d7`]
    /// in [`Self::graphics_imports`] or [`Self::runtime_graphics`]; `None`
    /// when there is none.
    #[serde(default)]
    pub d3d7_confidence: Option<D3d7Confidence>,
    /// A scanned ddraw file carries the whole `dxguid` GUID block (`doctor`
    /// notes it: its D3D GUIDs prove nothing).
    #[serde(default)]
    pub dxguid_block: bool,
    /// D3D renderer DLLs of an engine found next to the exe (`d3ddrv.dll`,
    /// `d3d.ren`).
    #[serde(default)]
    pub renderer_dlls: BTreeSet<String>,
    /// Other `.exe` files beside the entry point (editors, launchers, config
    /// tools) whose graphics imports were left out of the classification
    /// because the entry point and its DLLs already give evidence. Path →
    /// graphics APIs it imports; empty when nothing was set aside.
    #[serde(default)]
    pub secondary_graphics: BTreeMap<String, BTreeSet<GraphicsImport>>,
    pub files: Vec<PeFileScan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeFileScan {
    pub path: String,
    pub bitness: Bitness,
    /// Every imported DLL, lowercased (regular and delay-load).
    pub imports: BTreeSet<String>,
    /// Interesting DLL names found as strings in the image.
    pub runtime_loaded: BTreeSet<String>,
    /// `IID_IDirect3D`/`2`/`3`/`7` present, in or outside the `dxguid`
    /// block. The GUIDs are only looked for in files that import `ddraw.dll`
    /// or name it.
    pub d3d7_iids: bool,
    /// How many of the 44 ddraw/d3d GUIDs are present (0 if not looked for).
    #[serde(default)]
    pub ddraw_guids: u8,
    /// `ddraw_guids` ≥ [`DXGUID_BLOCK_MIN`]: a whole `dxguid` set.
    #[serde(default)]
    pub dxguid_block: bool,
    /// This file's Direct3D ≤ 7 evidence.
    #[serde(default)]
    pub d3d7: Option<D3d7Confidence>,
    #[serde(skip)]
    clickteam: bool,
}

/// Scan a game tree, finding the main exe from `goggame-*.info` or a heuristic.
pub fn scan(root: &Path) -> Result<PeScan, ScanError> {
    let (exe, source) = match info::find_entry_point(root) {
        Ok(exe) => (exe, MainExeSource::GogInfo),
        Err(_) => (
            guess_main_exe(root).ok_or_else(|| ScanError::NoExecutable {
                dir: root.to_path_buf(),
            })?,
            MainExeSource::Heuristic,
        ),
    };
    scan_from(root, &exe, source)
}

/// Scan a game tree whose main exe the caller already knows.
pub fn scan_exe(root: &Path, exe: &Path) -> Result<PeScan, ScanError> {
    scan_from(root, exe, MainExeSource::Given)
}

fn scan_from(root: &Path, exe: &Path, source: MainExeSource) -> Result<PeScan, ScanError> {
    let exe_dir = exe.parent().unwrap_or(root);
    let engine_hint = detect_engine_files(root, exe, exe_dir);

    let main = scan_file(root, exe)?.ok_or_else(|| ScanError::NotPe {
        path: exe.to_path_buf(),
    })?;
    let mut files = vec![main];

    let mut candidates = pe_files_in(exe_dir);
    let mut unreal_exes = BTreeSet::new();
    if engine_hint == Some(Engine::Unreal) {
        // The root stub only starts `<Project>/Binaries/Win64/<Project>-Win64-Shipping.exe`.
        let bins = unreal_binaries(root);
        unreal_exes.extend(bins.iter().map(|p| rel_path(root, p)));
        candidates.extend(bins);
    }
    let mut shipped = BTreeSet::new();
    let mut renderer_dlls = BTreeSet::new();
    for path in candidates {
        let name = lower_name(&path);
        if name.ends_with(".dll") {
            shipped.insert(name.clone());
        }
        if path == exe || is_ignored(&name) {
            continue;
        }
        // Unreadable or non-PE siblings (renamed data files) are skipped.
        if let Ok(Some(mut f)) = scan_file(root, &path) {
            if is_renderer_dll(&name) {
                renderer_dlls.insert(name);
                // UE2's D3DDrv.dll imports d3d8: that import says it all.
                let newer = f
                    .imports
                    .iter()
                    .any(|n| graphics_import(n).is_some_and(|g| g > GraphicsImport::Direct3d7));
                if !newer {
                    f.d3d7 = Some(D3d7Confidence::Strong);
                }
            }
            files.push(f);
        }
    }

    let local_overrides: BTreeSet<String> = shipped
        .iter()
        .filter(|n| graphics_import(n).is_some() || is_helper(n))
        .cloned()
        .collect();

    // The entry point decides (#226): another `.exe` beside it (a building
    // editor importing d3d6, say) only counts when the entry point, its DLLs
    // and the engine's own binaries give no graphics evidence (a launcher
    // stub). Without a known entry point (`Heuristic`) every exe counts.
    let is_tool = |f: &PeFileScan| {
        source != MainExeSource::Heuristic
            && f.path != files[0].path
            && file_name(&f.path).ends_with(".exe")
            && !unreal_exes.contains(&f.path)
    };
    let mut agg = aggregate(&files, &local_overrides, &shipped, |f| !is_tool(f));
    let mut secondary_graphics = BTreeMap::new();
    if agg.has_graphics() {
        for f in files.iter().filter(|f| is_tool(f)) {
            let apis: BTreeSet<GraphicsImport> = f
                .imports
                .iter()
                .filter_map(|n| graphics_import(n))
                .collect();
            if !apis.is_empty() {
                secondary_graphics.insert(f.path.clone(), apis);
            }
        }
    } else {
        agg = aggregate(&files, &local_overrides, &shipped, |_| true);
    }
    let Agg {
        graphics_imports,
        runtime_graphics,
        helpers,
        runtime_loaded,
        d3d7_confidence,
        dxguid_block,
    } = agg;

    let clickteam = files[0].clickteam || files[0].imports.contains("mmfs2.dll");
    let engine = engine_hint.or(clickteam.then_some(Engine::Clickteam));

    // Unity/Unreal load their D3D DLLs at runtime and may import only a
    // fallback (Neo Cab's UnityPlayer.dll imports opengl32, loads d3d11).
    let runtime_choice = matches!(engine, Some(Engine::Unity | Engine::Unreal));
    let static_api = classify(&graphics_imports);
    let engine_api = classify(&graphics_imports.union(&runtime_graphics).copied().collect());
    let (graphics, graphics_source) = match static_api {
        _ if runtime_choice && engine_api != static_api => {
            (engine_api, GraphicsSource::RuntimeStrings)
        }
        GraphicsApi::Unknown => match engine_graphics(engine, exe_dir) {
            Some(api) => (api, GraphicsSource::Engine),
            None => match classify(&runtime_graphics) {
                GraphicsApi::Unknown => (GraphicsApi::Unknown, GraphicsSource::None),
                api => (api, GraphicsSource::RuntimeStrings),
            },
        },
        api => (api, GraphicsSource::Imports),
    };

    Ok(PeScan {
        root: root.to_path_buf(),
        main_exe: files[0].path.clone(),
        main_exe_source: source,
        bitness: files[0].bitness,
        engine,
        graphics,
        graphics_source,
        graphics_imports,
        runtime_graphics,
        helpers,
        runtime_loaded,
        ddrawcompat: local_overrides.contains("ddraw.dll") && super::ddraw::is_ddrawcompat(exe_dir),
        local_overrides,
        d3d7_confidence,
        dxguid_block,
        renderer_dlls,
        secondary_graphics,
        files,
    })
}

/// What the scanned files add up to ([`scan_from`]).
struct Agg {
    graphics_imports: BTreeSet<GraphicsImport>,
    runtime_graphics: BTreeSet<GraphicsImport>,
    helpers: BTreeSet<String>,
    runtime_loaded: BTreeSet<String>,
    d3d7_confidence: Option<D3d7Confidence>,
    dxguid_block: bool,
}

impl Agg {
    fn has_graphics(&self) -> bool {
        !self.graphics_imports.is_empty()
            || !self.runtime_graphics.is_empty()
            || self.d3d7_confidence.is_some()
    }
}

fn aggregate(
    files: &[PeFileScan],
    local_overrides: &BTreeSet<String>,
    shipped: &BTreeSet<String>,
    include: impl Fn(&PeFileScan) -> bool,
) -> Agg {
    let mut graphics_imports = BTreeSet::new();
    let mut runtime_graphics = BTreeSet::new();
    let mut helpers = BTreeSet::new();
    let mut runtime_loaded = BTreeSet::new();
    let mut all_imports = BTreeSet::new();
    let mut d3d7_confidence = None;
    let mut dxguid_block = false;
    let mut runtime_d3d7 = false;
    for f in files.iter().filter(|f| include(f)) {
        let name = file_name(&f.path);
        if local_overrides.contains(&name) {
            continue;
        }
        all_imports.extend(f.imports.iter().cloned());
        graphics_imports.extend(f.imports.iter().filter_map(|n| graphics_import(n)));
        let imports_d3dim = f
            .imports
            .iter()
            .any(|n| graphics_import(n) == Some(GraphicsImport::Direct3d7));
        let d3d7 = f.d3d7.max(imports_d3dim.then_some(D3d7Confidence::Strong));
        if let Some(c) = d3d7 {
            // GUIDs in a file that only names ddraw.dll are as runtime as
            // its ddraw.
            if imports_d3dim || f.imports.contains("ddraw.dll") || is_renderer_dll(&name) {
                graphics_imports.insert(GraphicsImport::Direct3d7);
            } else {
                runtime_d3d7 = true;
            }
            d3d7_confidence = d3d7_confidence.max(Some(c));
        }
        dxguid_block |= f.dxguid_block;
        helpers.extend(
            f.imports
                .iter()
                .filter(|n| is_helper(n) && !shipped.contains(*n))
                .cloned(),
        );
        runtime_loaded.extend(f.runtime_loaded.iter().cloned());
    }
    runtime_loaded.retain(|n| !all_imports.contains(n));
    runtime_graphics.extend(runtime_loaded.iter().filter_map(|n| graphics_import(n)));
    if runtime_d3d7 {
        runtime_graphics.insert(GraphicsImport::Direct3d7);
    }
    Agg {
        graphics_imports,
        runtime_graphics,
        helpers,
        runtime_loaded,
        d3d7_confidence,
        dxguid_block,
    }
}

/// docs/19 §6.2 order: a D3D10/11 path beats D3D12 (DXMT before consent-gated
/// D3DMetal), any Direct3D beats ddraw-only.
fn classify(apis: &BTreeSet<GraphicsImport>) -> GraphicsApi {
    use GraphicsImport as G;
    let has = |g| apis.contains(&g);
    if has(G::D3d10) || has(G::D3d11) {
        GraphicsApi::D3d11
    } else if has(G::D3d12) {
        GraphicsApi::D3d12
    } else if has(G::D3d9) || has(G::D3d8) || has(G::Direct3d7) {
        GraphicsApi::D3dLegacy
    } else if has(G::Dxgi) {
        GraphicsApi::D3d11
    } else if has(G::Vulkan) {
        GraphicsApi::Vulkan
    } else if has(G::Opengl32) {
        GraphicsApi::OpenGl
    } else if has(G::Ddraw) {
        GraphicsApi::Ddraw2d
    } else {
        GraphicsApi::Unknown
    }
}

fn engine_graphics(engine: Option<Engine>, exe_dir: &Path) -> Option<GraphicsApi> {
    match engine? {
        // MMF2 runtimes offer DirectDraw / D3D8 / D3D9 display modes.
        Engine::Clickteam => Some(GraphicsApi::D3dLegacy),
        Engine::Java => jars_in(exe_dir)
            .iter()
            .any(|jar| {
                std::fs::read(jar).is_ok_and(|b| {
                    contains(&b, b"org/lwjgl") || contains(&b, b"badlogic/gdx/backends/lwjgl")
                })
            })
            .then_some(GraphicsApi::OpenGl),
        Engine::Unity | Engine::Unreal => None,
    }
}

fn graphics_import(name: &str) -> Option<GraphicsImport> {
    use GraphicsImport as G;
    Some(match name.strip_suffix(".dll")? {
        "ddraw" => G::Ddraw,
        "d3dim" | "d3dim700" | "d3drm" => G::Direct3d7,
        "d3d8" => G::D3d8,
        "d3d9" => G::D3d9,
        "d3d10" | "d3d10_1" | "d3d10core" | "d3d10_1core" => G::D3d10,
        "d3d11" => G::D3d11,
        "d3d12" => G::D3d12,
        "dxgi" => G::Dxgi,
        "opengl32" => G::Opengl32,
        "vulkan-1" => G::Vulkan,
        _ => return None,
    })
}

const HELPER_PREFIXES: &[&str] = &[
    "d3dcompiler_",
    "d3dx9_",
    "d3dx10",
    "d3dx11_",
    "d3dcsx_",
    "xinput",
    "xactengine",
    "xaudio2_",
    "x3daudio",
    "xapofx",
];

fn is_helper(name: &str) -> bool {
    name.ends_with(".dll") && HELPER_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Middleware that would only add noise (GOG Galaxy / Steam / EOS SDKs,
/// crash reporters, uninstallers).
fn is_ignored(name: &str) -> bool {
    [
        "unins",
        "galaxy",
        "steam_api",
        "eossdk",
        "unitycrashhandler",
        "crashrpt",
        "crashhandler",
    ]
    .iter()
    .any(|p| name.starts_with(p))
}

/// Unreal's and LithTech's Direct3D renderer DLLs (docs/22 §5.3).
fn is_renderer_dll(name: &str) -> bool {
    matches!(name, "d3ddrv.dll" | "d3d.ren")
}

/// The 44 `DEFINE_GUID`s of Wine's `include/d3d.h` (27) and `include/ddraw.h`
/// (17), as of `wine-11.0` (unchanged on `master` 2026-11-04), in GUID memory
/// layout (Data1–3 little-endian, Data4 as bytes). Order matters: the first
/// [`D3D_INTERFACES`] are the `IID_IDirect3D*` interfaces, then the device
/// and work GUIDs up to [`D3D_GUIDS`], then DirectDraw.
const DDRAW_GUIDS: [[u8; 16]; 44] = [
    // d3d.h: interfaces.
    guid(0x3BBA0080, 0x2421, 0x11CF, 0xA31A_00AA00B93356), // IID_IDirect3D
    guid(0x6AAE1EC1, 0x662A, 0x11D0, 0x889D_00AA00BBB76A), // IID_IDirect3D2
    guid(0xBB223240, 0xE72B, 0x11D0, 0xA9B4_00AA00C0993E), // IID_IDirect3D3
    guid(0xF5049E77, 0x4861, 0x11D2, 0xA407_00A0C90629A8), // IID_IDirect3D7
    // d3d.h: devices.
    guid(0xF2086B20, 0x259F, 0x11CF, 0xA31A_00AA00B93356), // IID_IDirect3DRampDevice
    guid(0xA4665C60, 0x2673, 0x11CF, 0xA31A_00AA00B93356), // IID_IDirect3DRGBDevice
    guid(0x84E63DE0, 0x46AA, 0x11CF, 0x816F_0000C020156E), // IID_IDirect3DHALDevice
    guid(0x881949A1, 0xD6F3, 0x11D0, 0x89AB_00A0C9054129), // IID_IDirect3DMMXDevice
    guid(0x50936643, 0x13E9, 0x11D1, 0x89AA_00A0C9054129), // IID_IDirect3DRefDevice
    guid(0xF5049E78, 0x4861, 0x11D2, 0xA407_00A0C90629A8), // IID_IDirect3DTnLHalDevice
    guid(0x8767DF22, 0xBACC, 0x11D1, 0x8969_00A0C90629A8), // IID_IDirect3DNullDevice
    // d3d.h: work interfaces.
    guid(0x64108800, 0x957D, 0x11D0, 0x89AB_00A0C9054129), // IID_IDirect3DDevice
    guid(0x93281501, 0x8CF8, 0x11D0, 0x89AB_00A0C9054129), // IID_IDirect3DDevice2
    guid(0xB0AB3B60, 0x33D7, 0x11D1, 0xA981_00C04FD7B174), // IID_IDirect3DDevice3
    guid(0xF5049E79, 0x4861, 0x11D2, 0xA407_00A0C90629A8), // IID_IDirect3DDevice7
    guid(0x2CDCD9E0, 0x25A0, 0x11CF, 0xA31A_00AA00B93356), // IID_IDirect3DTexture
    guid(0x93281502, 0x8CF8, 0x11D0, 0x89AB_00A0C9054129), // IID_IDirect3DTexture2
    guid(0x4417C142, 0x33AD, 0x11CF, 0x816F_0000C020156E), // IID_IDirect3DLight
    guid(0x4417C144, 0x33AD, 0x11CF, 0x816F_0000C020156E), // IID_IDirect3DMaterial
    guid(0x93281503, 0x8CF8, 0x11D0, 0x89AB_00A0C9054129), // IID_IDirect3DMaterial2
    guid(0xCA9C46F4, 0xD3C5, 0x11D1, 0xB75A_00600852B312), // IID_IDirect3DMaterial3
    guid(0x4417C145, 0x33AD, 0x11CF, 0x816F_0000C020156E), // IID_IDirect3DExecuteBuffer
    guid(0x4417C146, 0x33AD, 0x11CF, 0x816F_0000C020156E), // IID_IDirect3DViewport
    guid(0x93281500, 0x8CF8, 0x11D0, 0x89AB_00A0C9054129), // IID_IDirect3DViewport2
    guid(0xB0AB3B61, 0x33D7, 0x11D1, 0xA981_00C04FD7B174), // IID_IDirect3DViewport3
    guid(0x7A503555, 0x4A83, 0x11D1, 0xA5DB_00A0C90367F8), // IID_IDirect3DVertexBuffer
    guid(0xF5049E7D, 0x4861, 0x11D2, 0xA407_00A0C90629A8), // IID_IDirect3DVertexBuffer7
    // ddraw.h.
    guid(0xD7B70EE0, 0x4340, 0x11CF, 0xB063_0020AFC2CD35), // CLSID_DirectDraw
    guid(0x3C305196, 0x50DB, 0x11D3, 0x9CFE_00C04FD930C5), // CLSID_DirectDraw7
    guid(0x593817A0, 0x7DB3, 0x11CF, 0xA2DE_00AA00B93356), // CLSID_DirectDrawClipper
    guid(0x6C14DB80, 0xA733, 0x11CE, 0xA521_0020AF0BE560), // IID_IDirectDraw
    guid(0xB3A6F3E0, 0x2B43, 0x11CF, 0xA2DE_00AA00B93356), // IID_IDirectDraw2
    guid(0x618F8AD4, 0x8B7A, 0x11D0, 0x8FCC_00C04FD9189D), // IID_IDirectDraw3
    guid(0x9C59509A, 0x39BD, 0x11D1, 0x8C4A_00C04FD930C5), // IID_IDirectDraw4
    guid(0x15E65EC0, 0x3B9C, 0x11D2, 0xB92F_00609797EA5B), // IID_IDirectDraw7
    guid(0x6C14DB81, 0xA733, 0x11CE, 0xA521_0020AF0BE560), // IID_IDirectDrawSurface
    guid(0x57805885, 0x6EEC, 0x11CF, 0x9441_A82303C10E27), // IID_IDirectDrawSurface2
    guid(0xDA044E00, 0x69B2, 0x11D0, 0xA1D5_00AA00B8DFBB), // IID_IDirectDrawSurface3
    guid(0x0B2B8630, 0xAD35, 0x11D0, 0x8EA6_00609797EA5B), // IID_IDirectDrawSurface4
    guid(0x06675A80, 0x3B9B, 0x11D2, 0xB92F_00609797EA5B), // IID_IDirectDrawSurface7
    guid(0x6C14DB84, 0xA733, 0x11CE, 0xA521_0020AF0BE560), // IID_IDirectDrawPalette
    guid(0x6C14DB85, 0xA733, 0x11CE, 0xA521_0020AF0BE560), // IID_IDirectDrawClipper
    guid(0x4B9F0EE0, 0x0D7E, 0x11D0, 0x9B06_00A0C903A3B8), // IID_IDirectDrawColorControl
    guid(0x69C11C3E, 0xB46B, 0x11D1, 0xAD7A_00C04FC29B4E), // IID_IDirectDrawGammaControl
];

/// `DDRAW_GUIDS[..D3D_INTERFACES]`: `IID_IDirect3D`, `2`, `3`, `7`.
const D3D_INTERFACES: usize = 4;
/// `DDRAW_GUIDS[..D3D_GUIDS]`: everything from `d3d.h`.
const D3D_GUIDS: usize = 27;
/// This many of the 44 present = a whole `dxguid` set (docs/22 §5.2, §7.1).
/// DirectX 7's `dxguid.lib` brings 43 (all but `IID_IDirectDraw3`),
/// DirectX 6's 36 (also without the seven DX7 GUIDs: SimCity 3000, #174).
/// The most a file without a block carried is 16.
pub const DXGUID_BLOCK_MIN: u32 = 32;

/// `Data4` written as one big-endian number, the way `DEFINE_GUID` lists it.
const fn guid(d1: u32, d2: u16, d3: u16, d4: u64) -> [u8; 16] {
    let a = d1.to_le_bytes();
    let b = d2.to_le_bytes();
    let c = d3.to_le_bytes();
    let d = d4.to_be_bytes();
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6],
        d[7],
    ]
}

/// For each byte value, the [`DDRAW_GUIDS`] (as bits) that start with it.
const GUID_BY_FIRST_BYTE: [u64; 256] = {
    let mut t = [0u64; 256];
    let mut i = 0;
    while i < DDRAW_GUIDS.len() {
        t[DDRAW_GUIDS[i][0] as usize] |= 1 << i;
        i += 1;
    }
    t
};

/// Which [`DDRAW_GUIDS`] occur in `bytes`, as bits.
fn ddraw_guid_hits(bytes: &[u8]) -> u64 {
    let mut hits = 0;
    for w in bytes.windows(16) {
        let mut cand = GUID_BY_FIRST_BYTE[w[0] as usize] & !hits;
        while cand != 0 {
            let i = cand.trailing_zeros() as usize;
            if w == DDRAW_GUIDS[i] {
                hits |= 1 << i;
            }
            cand &= cand - 1;
        }
    }
    hits
}

/// docs/22 §6.2 for one file: (`d3d7_iids`, `dxguid_block`, evidence).
fn d3d7_verdict(hits: u64) -> (bool, bool, Option<D3d7Confidence>) {
    let interfaces = hits & ((1 << D3D_INTERFACES) - 1) != 0;
    let work = hits & ((1 << D3D_GUIDS) - (1 << D3D_INTERFACES)) != 0;
    let block = hits.count_ones() >= DXGUID_BLOCK_MIN;
    let evidence = if block {
        Some(D3d7Confidence::DxguidBlock)
    } else if work {
        Some(D3d7Confidence::Strong)
    } else if interfaces {
        Some(D3d7Confidence::Medium)
    } else {
        None
    };
    (interfaces, block, evidence)
}

/// Files larger than this are not executables we care about (Clickteam and
/// installer exes carry their data as an overlay, but stay well below it).
const MAX_PE_SIZE: u64 = 1 << 30;

fn scan_file(root: &Path, path: &Path) -> Result<Option<PeFileScan>, ScanError> {
    let io = |source| ScanError::Io {
        path: path.to_path_buf(),
        source,
    };
    if std::fs::metadata(path).map_err(io)?.len() > MAX_PE_SIZE {
        return Ok(None);
    }
    let data = std::fs::read(path).map_err(io)?;
    let rel = rel_path(root, path);
    let parsed = match FileKind::parse(&*data) {
        Ok(FileKind::Pe32) => parse_pe::<object::pe::ImageNtHeaders32>(&data, Bitness::I386),
        Ok(FileKind::Pe64) => parse_pe::<object::pe::ImageNtHeaders64>(&data, Bitness::X86_64),
        _ => None,
    };
    let Some(mut f) = parsed else {
        tracing::debug!(path = %path.display(), "not a parsable PE, skipped");
        return Ok(None);
    };
    f.path = rel;
    Ok(Some(f))
}

fn parse_pe<Pe: ImageNtHeaders>(data: &[u8], bitness: Bitness) -> Option<PeFileScan> {
    let file = PeFile::<Pe, &[u8]>::parse(data).ok()?;
    let mut imports = BTreeSet::new();
    // A damaged table yields what was read before the damage, not an error.
    if let Ok(Some(table)) = file.import_table()
        && let Ok(mut it) = table.descriptors()
    {
        while let Ok(Some(d)) = it.next() {
            if let Ok(name) = table.name(d.name.get(LE)) {
                imports.insert(String::from_utf8_lossy(name).to_ascii_lowercase());
            }
        }
    }
    if let Ok(Some(table)) = file.delay_load_import_table()
        && let Ok(mut it) = table.descriptors()
    {
        while let Ok(Some(d)) = it.next() {
            if let Ok(name) = table.name(d.dll_name_rva.get(LE)) {
                imports.insert(String::from_utf8_lossy(name).to_ascii_lowercase());
            }
        }
    }

    let mut runtime_loaded = BTreeSet::new();
    let mut clickteam = false;
    // Section data only: an appended overlay (Clickteam, installers) is game
    // data, not code, and would only add false hits.
    let sections: Vec<&[u8]> = file
        .section_table()
        .iter()
        .filter_map(|s| s.pe_data(data).ok())
        .collect();
    for bytes in &sections {
        for_each_token(bytes, |token| {
            let lower = token.to_ascii_lowercase();
            if lower.contains("clickteam") {
                clickteam = true;
            }
            if graphics_import(&lower).is_some() || is_helper(&lower) {
                runtime_loaded.insert(lower);
            }
        });
    }

    // GUIDs only in files that import ddraw.dll or name it (LoadLibrary).
    let hits = if imports.contains("ddraw.dll") || runtime_loaded.contains("ddraw.dll") {
        sections.iter().fold(0, |h, b| h | ddraw_guid_hits(b))
    } else {
        0
    };
    let (d3d7_iids, dxguid_block, d3d7) = d3d7_verdict(hits);

    Some(PeFileScan {
        path: String::new(),
        bitness,
        imports,
        runtime_loaded,
        d3d7_iids,
        ddraw_guids: hits.count_ones() as u8,
        dxguid_block,
        d3d7,
        clickteam,
    })
}

/// Call `f` with every run of `[A-Za-z0-9_.-]` (4–64 chars) found as ASCII
/// or UTF-16LE in `bytes`.
fn for_each_token(bytes: &[u8], mut f: impl FnMut(&str)) {
    fn ok(b: u8) -> bool {
        b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')
    }
    let mut emit = |run: &[u8]| {
        if (4..=64).contains(&run.len()) {
            // ok() only admits ASCII, so this cannot fail.
            if let Ok(s) = std::str::from_utf8(run) {
                f(s);
            }
        }
    };
    for run in bytes.split(|b| !ok(*b)) {
        emit(run);
    }
    for start in 0..2 {
        let mut run = Vec::new();
        for &[lo, hi] in bytes.get(start..).unwrap_or_default().as_chunks::<2>().0 {
            if hi == 0 && ok(lo) {
                run.push(lo);
            } else {
                emit(&run);
                run.clear();
            }
        }
        emit(&run);
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn detect_engine_files(root: &Path, exe: &Path, exe_dir: &Path) -> Option<Engine> {
    let stem = exe.file_stem()?.to_string_lossy();
    if exe_dir.join("UnityPlayer.dll").is_file() || exe_dir.join(format!("{stem}_Data")).is_dir() {
        return Some(Engine::Unity);
    }
    if root.join("Engine").is_dir() && (!unreal_binaries(root).is_empty() || is_unreal_dir(exe_dir))
    {
        return Some(Engine::Unreal);
    }
    let has_jre = ["jre", "runtime", "jdk"]
        .iter()
        .any(|d| exe_dir.join(d).join("bin").is_dir());
    if has_jre && !jars_in(exe_dir).is_empty() {
        return Some(Engine::Java);
    }
    None
}

fn is_unreal_dir(dir: &Path) -> bool {
    let s = dir.to_string_lossy().replace('\\', "/");
    s.contains("/Binaries/Win64") || s.contains("/Binaries/Win32")
}

/// `<root>/<Project>/Binaries/Win{64,32}/*.exe`, skipping `Engine/`.
fn unreal_binaries(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for project in entries.flatten().map(|e| e.path()) {
        if !project.is_dir() || project.file_name().is_some_and(|n| n == "Engine") {
            continue;
        }
        for arch in ["Win64", "Win32"] {
            out.extend(
                pe_files_in(&project.join("Binaries").join(arch))
                    .into_iter()
                    .filter(|p| lower_name(p).ends_with(".exe")),
            );
        }
    }
    out
}

/// LithTech renderers (`d3d.ren`) are PE DLLs too.
fn pe_files_in(dir: &Path) -> Vec<PathBuf> {
    files_with_ext(dir, &["exe", "dll", "ren"])
}

fn jars_in(dir: &Path) -> Vec<PathBuf> {
    files_with_ext(dir, &["jar"])
}

fn files_with_ext(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
        })
        .collect();
    out.sort();
    out
}

/// No usable info file: the largest `.exe` in the root (else one level down)
/// that does not look like an installer, uninstaller or crash reporter.
fn guess_main_exe(root: &Path) -> Option<PathBuf> {
    const SKIP: &[&str] = &[
        "unins", "setup", "install", "crash", "redist", "vcredist", "dxsetup", "dotnet",
    ];
    let pick = |dir: &Path| {
        files_with_ext(dir, &["exe"])
            .into_iter()
            .filter(|p| {
                let n = lower_name(p);
                !SKIP.iter().any(|s| n.contains(s))
            })
            .max_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
    };
    pick(root).or_else(|| {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        dirs.iter().find_map(|d| pick(d))
    })
}

fn lower_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn file_name(rel: &str) -> String {
    rel.rsplit('/').next().unwrap_or(rel).to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthesized PE fixtures (design 08 §1): a header, one `.rdata` section
    /// holding the import and delay-load tables, and `blob` appended to it.
    struct Pe<'a> {
        pe64: bool,
        imports: &'a [&'a str],
        delay: &'a [&'a str],
        blob: &'a [u8],
    }

    impl Default for Pe<'_> {
        fn default() -> Self {
            Pe {
                pe64: false,
                imports: &["kernel32.dll"],
                delay: &[],
                blob: &[],
            }
        }
    }

    const SECTION_RVA: u32 = 0x1000;
    const SECTION_OFF: u32 = 0x200;

    fn build(spec: &Pe) -> Vec<u8> {
        // Section layout: [import descs][delay descs][null thunk][names][blob].
        let imp_len = 20 * (spec.imports.len() + 1);
        let delay_len = if spec.delay.is_empty() {
            0
        } else {
            32 * (spec.delay.len() + 1)
        };
        let thunk_off = imp_len + delay_len;
        let mut names = Vec::new();
        let mut name_rvas = Vec::new();
        let names_off = thunk_off + 8;
        for n in spec.imports.iter().chain(spec.delay) {
            name_rvas.push(SECTION_RVA + (names_off + names.len()) as u32);
            names.extend_from_slice(n.as_bytes());
            names.push(0);
        }
        let rva = |off: usize| SECTION_RVA + off as u32;
        let mut sec = Vec::new();
        let (imp_names, delay_names) = name_rvas.split_at(spec.imports.len());
        for &name in imp_names {
            for v in [rva(thunk_off), 0, 0, name, rva(thunk_off)] {
                sec.extend_from_slice(&v.to_le_bytes());
            }
        }
        sec.extend_from_slice(&[0; 20]);
        if !spec.delay.is_empty() {
            for &name in delay_names {
                for v in [1, name, 0, rva(thunk_off), rva(thunk_off), 0, 0, 0] {
                    sec.extend_from_slice(&v.to_le_bytes());
                }
            }
            sec.extend_from_slice(&[0; 32]);
        }
        sec.extend_from_slice(&[0; 8]);
        sec.extend_from_slice(&names);
        sec.extend_from_slice(spec.blob);
        let raw_size = sec.len().next_multiple_of(0x200);
        sec.resize(raw_size, 0);

        let mut h = vec![0u8; SECTION_OFF as usize];
        h[0..2].copy_from_slice(b"MZ");
        h[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        let mut w = Vec::new();
        w.extend_from_slice(b"PE\0\0");
        let (machine, opt_size, chars): (u16, u16, u16) = if spec.pe64 {
            (0x8664, 240, 0x0022)
        } else {
            (0x014C, 224, 0x0102)
        };
        w.extend_from_slice(&machine.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes()); // sections
        w.extend_from_slice(&[0; 12]); // timestamp, symtab ptr, symbol count
        w.extend_from_slice(&opt_size.to_le_bytes());
        w.extend_from_slice(&chars.to_le_bytes());
        // Optional header.
        let u16le = |w: &mut Vec<u8>, v: u16| w.extend_from_slice(&v.to_le_bytes());
        let u32le = |w: &mut Vec<u8>, v: u32| w.extend_from_slice(&v.to_le_bytes());
        let image_size = SECTION_RVA + (raw_size as u32).next_multiple_of(0x1000);
        u16le(&mut w, if spec.pe64 { 0x20B } else { 0x10B });
        w.extend_from_slice(&[0, 0]); // linker version
        for v in [0, raw_size as u32, 0, 0, SECTION_RVA] {
            u32le(&mut w, v); // code/data sizes, entry point, base of code
        }
        if spec.pe64 {
            w.extend_from_slice(&0x1_4000_0000u64.to_le_bytes());
        } else {
            u32le(&mut w, 0); // base of data
            u32le(&mut w, 0x40_0000);
        }
        u32le(&mut w, 0x1000);
        u32le(&mut w, 0x200);
        for v in [6, 0, 0, 0, 6, 0] {
            u16le(&mut w, v); // OS, image, subsystem versions
        }
        for v in [0, image_size, SECTION_OFF, 0] {
            u32le(&mut w, v); // win32 version, image size, header size, checksum
        }
        u16le(&mut w, 2); // GUI subsystem
        u16le(&mut w, 0);
        let word = if spec.pe64 { 8 } else { 4 };
        w.extend(std::iter::repeat_n(0, 4 * word)); // stack/heap sizes
        u32le(&mut w, 0); // loader flags
        u32le(&mut w, 16);
        for i in 0..16 {
            let (va, size) = match i {
                1 => (SECTION_RVA, imp_len as u32),
                13 if !spec.delay.is_empty() => (rva(imp_len), delay_len as u32),
                _ => (0, 0),
            };
            u32le(&mut w, va);
            u32le(&mut w, size);
        }
        w.extend_from_slice(b".rdata\0\0");
        for v in [
            sec.len() as u32,
            SECTION_RVA,
            raw_size as u32,
            SECTION_OFF,
            0,
            0,
        ] {
            u32le(&mut w, v);
        }
        u32le(&mut w, 0); // relocation/line-number counts
        u32le(&mut w, 0x4000_0040); // initialized data, readable
        h[0x40..0x40 + w.len()].copy_from_slice(&w);
        h.extend_from_slice(&sec);
        h
    }

    fn put(dir: &Path, name: &str, spec: &Pe) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, build(spec)).unwrap();
        path
    }

    fn info(dir: &Path, exe: &str) {
        std::fs::write(
            dir.join("goggame-1.info"),
            format!(
                r#"{{"playTasks":[{{"type":"FileTask","category":"game","path":"{exe}","isPrimary":true}}]}}"#
            ),
        )
        .unwrap();
    }

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn bitness_pe32_and_pe32_plus() {
        let tmp = tempfile::tempdir().unwrap();
        let a = put(tmp.path(), "a.exe", &Pe::default());
        let b = put(
            tmp.path(),
            "b/b.exe",
            &Pe {
                pe64: true,
                ..Pe::default()
            },
        );
        assert_eq!(scan_exe(tmp.path(), &a).unwrap().bitness, Bitness::I386);
        let s = scan_exe(tmp.path(), &b).unwrap();
        assert_eq!(s.bitness, Bitness::X86_64);
        assert_eq!(s.main_exe, "b/b.exe");
        assert!(s.files[0].imports.contains("kernel32.dll"));
    }

    #[test]
    fn delay_load_import_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = put(
            tmp.path(),
            "game.exe",
            &Pe {
                pe64: true,
                delay: &["D3D11.dll", "dxgi.dll"],
                ..Pe::default()
            },
        );
        let s = scan_exe(tmp.path(), &exe).unwrap();
        assert_eq!(s.graphics, GraphicsApi::D3d11);
        assert_eq!(s.graphics_source, GraphicsSource::Imports);
        assert!(s.files[0].imports.contains("d3d11.dll"));
    }

    #[test]
    fn d3d11_only_in_sibling_dll() {
        // Rebel Galaxy shape: the exe imports OGRE, a render-system DLL
        // imports D3D11 and the shader compiler.
        let tmp = tempfile::tempdir().unwrap();
        put(
            tmp.path(),
            "Game.exe",
            &Pe {
                imports: &["kernel32.dll", "OgreMain.dll"],
                ..Pe::default()
            },
        );
        put(
            tmp.path(),
            "RenderSystem_Direct3D11.dll",
            &Pe {
                imports: &["d3d11.dll", "D3DCOMPILER_43.dll", "d3dx11_43.dll"],
                ..Pe::default()
            },
        );
        put(
            tmp.path(),
            "Galaxy.dll",
            &Pe {
                imports: &["d3d12.dll"],
                ..Pe::default()
            },
        );
        info(tmp.path(), "Game.exe");
        let s = scan(tmp.path()).unwrap();
        assert_eq!(s.main_exe_source, MainExeSource::GogInfo);
        assert_eq!(s.main_exe, "Game.exe");
        assert_eq!(s.graphics, GraphicsApi::D3d11);
        assert!(!s.graphics_imports.contains(&GraphicsImport::D3d12)); // Galaxy SDK ignored
        assert_eq!(
            s.helpers,
            BTreeSet::from(["d3dcompiler_43.dll".into(), "d3dx11_43.dll".into()])
        );
    }

    #[test]
    fn ddraw_only_is_2d_and_d3d7_iid_makes_it_legacy() {
        let tmp = tempfile::tempdir().unwrap();
        let two_d = put(
            tmp.path(),
            "2d/Pharaoh.exe",
            &Pe {
                imports: &["KERNEL32.dll", "DDRAW.dll"],
                ..Pe::default()
            },
        );
        let s = scan_exe(tmp.path(), &two_d).unwrap();
        assert_eq!(s.graphics, GraphicsApi::Ddraw2d);

        let mut blob = vec![0xCC; 3];
        blob.extend_from_slice(&DDRAW_GUIDS[3]);
        let d3d = put(
            tmp.path(),
            "3d/TDR.exe",
            &Pe {
                imports: &["DDRAW.dll"],
                blob: &blob,
                ..Pe::default()
            },
        );
        let s = scan_exe(tmp.path(), &d3d).unwrap();
        assert_eq!(s.graphics, GraphicsApi::D3dLegacy);
        assert!(s.graphics_imports.contains(&GraphicsImport::Direct3d7));
        assert_eq!(s.d3d7_confidence, Some(D3d7Confidence::Medium));
        assert!(s.files[0].d3d7_iids);
    }

    // Indices into DDRAW_GUIDS.
    const IID_D3D7: usize = 3;
    const IID_HAL_DEVICE: usize = 6;
    const IID_TEXTURE2: usize = 16;
    const IID_DDRAW3: usize = 32;
    const IID_DDRAW7: usize = 34;
    /// What DirectX 7 added (`IID_IDirect3D7`, `…TnLHalDevice`, `…Device7`,
    /// `…VertexBuffer7`, `CLSID_DirectDraw7`, `IID_IDirectDraw7`,
    /// `…Surface7`), plus `IID_IDirectDraw3`, which no `dxguid.lib` has.
    const NOT_IN_DX6_BLOCK: [usize; 8] = [IID_D3D7, 9, 14, 26, 28, IID_DDRAW3, IID_DDRAW7, 39];

    /// The given GUIDs back to back, after a few odd bytes (GUIDs need not
    /// be aligned).
    fn guids(which: impl IntoIterator<Item = usize>) -> Vec<u8> {
        let mut blob = vec![0xCC; 3];
        for i in which {
            blob.extend_from_slice(&DDRAW_GUIDS[i]);
        }
        blob
    }

    /// A 32-bit `ddraw.dll` importer carrying `blob`, scanned.
    fn ddraw_exe(blob: &[u8]) -> PeScan {
        let tmp = tempfile::tempdir().unwrap();
        let exe = put(
            tmp.path(),
            "game.exe",
            &Pe {
                imports: &["kernel32.dll", "DDRAW.dll"],
                blob,
                ..Pe::default()
            },
        );
        scan_exe(tmp.path(), &exe).unwrap()
    }

    #[test]
    fn guid_table_is_wines_44() {
        let set: BTreeSet<[u8; 16]> = DDRAW_GUIDS.iter().copied().collect();
        assert_eq!(set.len(), 44);
        // Spot checks against the byte layout of d3d.h / ddraw.h.
        assert_eq!(
            DDRAW_GUIDS[IID_D3D7],
            [
                0x77, 0x9E, 0x04, 0xF5, 0x61, 0x48, 0xD2, 0x11, 0xA4, 0x07, 0x00, 0xA0, 0xC9, 0x06,
                0x29, 0xA8
            ]
        );
        assert_eq!(
            DDRAW_GUIDS[IID_DDRAW3],
            [
                0xD4, 0x8A, 0x8F, 0x61, 0x7A, 0x8B, 0xD0, 0x11, 0x8F, 0xCC, 0x00, 0xC0, 0x4F, 0xD9,
                0x18, 0x9D
            ]
        );
        assert_eq!(ddraw_guid_hits(&guids(0..44)), (1 << 44) - 1);
        assert_eq!(ddraw_guid_hits(&DDRAW_GUIDS[5][..15]), 0);
    }

    /// docs/22 §5.2: the 43-GUID `dxguid` block (all but `IID_IDirectDraw3`)
    /// is the weakest D3D evidence, yet still D3D ≤ 7 (§6.1: unsure is not 2D).
    #[test]
    fn dxguid_block_is_weak_d3d7_evidence() {
        let s = ddraw_exe(&guids((0..44).filter(|&i| i != IID_DDRAW3)));
        assert_eq!(s.graphics, GraphicsApi::D3dLegacy);
        assert_eq!(s.d3d7_confidence, Some(D3d7Confidence::DxguidBlock));
        assert!(s.dxguid_block);
        assert_eq!(s.files[0].ddraw_guids, 43);
        assert!(s.files[0].dxguid_block);

        // The threshold: 32 is the block, 31 with a device GUID is strong.
        let s = ddraw_exe(&guids(0..32));
        assert_eq!(s.d3d7_confidence, Some(D3d7Confidence::DxguidBlock));
        let s = ddraw_exe(&guids(0..31));
        assert_eq!(s.d3d7_confidence, Some(D3d7Confidence::Strong));
        assert!(!s.dxguid_block);
    }

    /// #174: SimCity 3000 (2D) links DirectX 6's `dxguid.lib`, 36 GUIDs
    /// without the DX7 ones. Its sound DLL carries the same 36: noise, not
    /// device GUIDs.
    #[test]
    fn dx6_dxguid_block_is_a_block_too() {
        let s = ddraw_exe(&guids((0..44).filter(|i| !NOT_IN_DX6_BLOCK.contains(i))));
        assert_eq!(s.files[0].ddraw_guids, 36);
        assert!(s.files[0].dxguid_block);
        assert_eq!(s.graphics, GraphicsApi::D3dLegacy);
        assert_eq!(s.d3d7_confidence, Some(D3d7Confidence::DxguidBlock));
    }

    /// Stronghold Crusader HD carries only `IID_IDirectDraw7`.
    #[test]
    fn selective_ddraw_guids_stay_2d() {
        let s = ddraw_exe(&guids([IID_DDRAW7, 28, 30]));
        assert_eq!(s.graphics, GraphicsApi::Ddraw2d);
        assert_eq!(s.d3d7_confidence, None);
        assert_eq!(s.files[0].ddraw_guids, 3);
        assert!(!s.files[0].d3d7_iids);
    }

    #[test]
    fn device_and_work_guids_are_strong() {
        // A device GUID alone (no IID_IDirect3D*) was 2D before #173.
        for i in [IID_HAL_DEVICE, IID_TEXTURE2, 26] {
            let s = ddraw_exe(&guids([IID_DDRAW7, i]));
            assert_eq!(s.graphics, GraphicsApi::D3dLegacy, "guid {i}");
            assert_eq!(s.d3d7_confidence, Some(D3d7Confidence::Strong), "guid {i}");
            assert!(!s.files[0].d3d7_iids);
        }
        let s = ddraw_exe(&guids([IID_D3D7, IID_HAL_DEVICE, IID_DDRAW7]));
        assert_eq!(s.d3d7_confidence, Some(D3d7Confidence::Strong));
        assert!(s.files[0].d3d7_iids);
    }

    #[test]
    fn d3dim_import_is_strong() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = put(
            tmp.path(),
            "game.exe",
            &Pe {
                imports: &["d3drm.dll", "ddraw.dll"],
                ..Pe::default()
            },
        );
        let s = scan_exe(tmp.path(), &exe).unwrap();
        assert_eq!(s.graphics, GraphicsApi::D3dLegacy);
        assert_eq!(s.d3d7_confidence, Some(D3d7Confidence::Strong));
    }

    #[test]
    fn guids_are_checked_where_ddraw_is_loaded_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let mut blob = b"\0LoadLibraryA\0ddraw.dll\0".to_vec();
        blob.extend(guids([IID_TEXTURE2]));
        let exe = put(
            tmp.path(),
            "3d/game.exe",
            &Pe {
                blob: &blob,
                ..Pe::default()
            },
        );
        let s = scan_exe(tmp.path(), &exe).unwrap();
        assert_eq!(s.graphics, GraphicsApi::D3dLegacy);
        assert_eq!(s.graphics_source, GraphicsSource::RuntimeStrings);
        assert!(s.graphics_imports.is_empty());
        assert_eq!(
            s.runtime_graphics,
            BTreeSet::from([GraphicsImport::Ddraw, GraphicsImport::Direct3d7])
        );
        assert_eq!(s.d3d7_confidence, Some(D3d7Confidence::Strong));

        // Without GUIDs it stays 2D (the RenderWare driver of docs/22 §5.2).
        let exe = put(
            tmp.path(),
            "2d/game.exe",
            &Pe {
                imports: &["gdi32.dll", "user32.dll"],
                blob: b"\0ddraw.dll\0",
                ..Pe::default()
            },
        );
        let s = scan_exe(tmp.path(), &exe).unwrap();
        assert_eq!(s.graphics, GraphicsApi::Ddraw2d);
        assert_eq!(s.d3d7_confidence, None);

        // A file that never names ddraw is not searched: the block in an
        // input DLL is noise.
        let exe = put(
            tmp.path(),
            "noise/game.exe",
            &Pe {
                imports: &["ddraw.dll"],
                ..Pe::default()
            },
        );
        put(
            tmp.path(),
            "noise/input_di8.dll",
            &Pe {
                blob: &guids(0..44),
                ..Pe::default()
            },
        );
        let s = scan_exe(tmp.path(), &exe).unwrap();
        assert_eq!(s.graphics, GraphicsApi::Ddraw2d);
        assert!(!s.dxguid_block);
        assert_eq!(s.files[1].ddraw_guids, 0);
    }

    #[test]
    fn engine_renderer_dlls_mark_d3d7() {
        let tmp = tempfile::tempdir().unwrap();
        // LithTech (Blood II, Shogo): the exe is clean, `d3d.ren` is a PE
        // DLL importing ddraw.
        let exe = put(
            tmp.path(),
            "lith/Client.exe",
            &Pe {
                imports: &["kernel32.dll", "DDRAW.dll"],
                ..Pe::default()
            },
        );
        put(
            tmp.path(),
            "lith/D3D.REN",
            &Pe {
                imports: &["ddraw.dll"],
                ..Pe::default()
            },
        );
        let s = scan_exe(tmp.path(), &exe).unwrap();
        assert!(s.files.iter().any(|f| f.path == "lith/D3D.REN"));
        assert_eq!(s.renderer_dlls, BTreeSet::from(["d3d.ren".into()]));
        assert_eq!(s.graphics, GraphicsApi::D3dLegacy);
        assert!(s.graphics_imports.contains(&GraphicsImport::Direct3d7));
        assert_eq!(s.d3d7_confidence, Some(D3d7Confidence::Strong));

        // Unreal (UE1): the exe imports the engine; D3DDrv.dll imports ddraw.
        let exe = put(
            tmp.path(),
            "ue1/System/Unreal.exe",
            &Pe {
                imports: &["Core.dll", "Engine.dll"],
                ..Pe::default()
            },
        );
        put(
            tmp.path(),
            "ue1/System/D3DDrv.dll",
            &Pe {
                imports: &["DDRAW.dll"],
                ..Pe::default()
            },
        );
        let s = scan_exe(tmp.path(), &exe).unwrap();
        assert_eq!(s.renderer_dlls, BTreeSet::from(["d3ddrv.dll".into()]));
        assert_eq!(s.graphics, GraphicsApi::D3dLegacy);
        assert_eq!(s.d3d7_confidence, Some(D3d7Confidence::Strong));

        // UE2's D3DDrv.dll imports d3d8: that import decides, no D3D ≤ 7.
        let exe = put(
            tmp.path(),
            "ue2/System/UT2004.exe",
            &Pe {
                imports: &["Core.dll"],
                ..Pe::default()
            },
        );
        put(
            tmp.path(),
            "ue2/System/D3DDrv.dll",
            &Pe {
                imports: &["d3d8.dll"],
                ..Pe::default()
            },
        );
        let s = scan_exe(tmp.path(), &exe).unwrap();
        assert_eq!(s.graphics, GraphicsApi::D3dLegacy);
        assert!(!s.graphics_imports.contains(&GraphicsImport::Direct3d7));
        assert_eq!(s.d3d7_confidence, None);
    }

    /// The 64-bit rule is untouched: the GUID check runs the same, the
    /// classification is bit-blind (renderer.rs decides by bitness).
    #[test]
    fn sixty_four_bit_ddraw_scans_the_same() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = put(
            tmp.path(),
            "game.exe",
            &Pe {
                pe64: true,
                imports: &["ddraw.dll"],
                blob: &guids([IID_DDRAW7]),
                ..Pe::default()
            },
        );
        let s = scan_exe(tmp.path(), &exe).unwrap();
        assert_eq!(s.bitness, Bitness::X86_64);
        assert_eq!(s.graphics, GraphicsApi::Ddraw2d);
        assert_eq!(s.files[0].ddraw_guids, 1);

        let exe = put(
            tmp.path(),
            "b/game.exe",
            &Pe {
                pe64: true,
                imports: &["ddraw.dll"],
                blob: &guids(0..44),
                ..Pe::default()
            },
        );
        let s = scan_exe(tmp.path(), &exe).unwrap();
        assert_eq!(s.graphics, GraphicsApi::D3dLegacy);
        assert_eq!(s.d3d7_confidence, Some(D3d7Confidence::DxguidBlock));
    }

    #[test]
    fn runtime_loaded_names_classify_only_without_imports() {
        let tmp = tempfile::tempdir().unwrap();
        let mut blob = b"\0LoadLibraryA\0d3d9.dll\0".to_vec();
        blob.extend(utf16("\0D3DCompiler_47.dll\0"));
        let exe = put(
            tmp.path(),
            "game.exe",
            &Pe {
                blob: &blob,
                ..Pe::default()
            },
        );
        let s = scan_exe(tmp.path(), &exe).unwrap();
        assert_eq!(
            s.runtime_loaded,
            BTreeSet::from(["d3d9.dll".into(), "d3dcompiler_47.dll".into()])
        );
        assert_eq!(s.graphics, GraphicsApi::D3dLegacy);
        assert_eq!(s.graphics_source, GraphicsSource::RuntimeStrings);

        // A static import wins over a string, and imported names are not
        // reported as runtime-loaded.
        let exe = put(
            tmp.path(),
            "gl/game.exe",
            &Pe {
                imports: &["opengl32.dll"],
                blob: b"\0d3d9.dll\0",
                ..Pe::default()
            },
        );
        let s = scan_exe(tmp.path(), &exe).unwrap();
        assert_eq!(s.graphics, GraphicsApi::OpenGl);
        assert_eq!(s.runtime_graphics, BTreeSet::from([GraphicsImport::D3d9]));
        assert!(!s.runtime_loaded.contains("opengl32.dll"));
    }

    #[test]
    fn shipped_wrappers_and_helpers_are_local_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = put(
            tmp.path(),
            "game.exe",
            &Pe {
                imports: &["ddraw.dll", "d3dcompiler_47.dll", "xinput1_3.dll"],
                ..Pe::default()
            },
        );
        // A dgVoodoo-style ddraw.dll that itself imports d3d11.
        put(
            tmp.path(),
            "ddraw.dll",
            &Pe {
                imports: &["d3d11.dll"],
                ..Pe::default()
            },
        );
        put(tmp.path(), "D3DCompiler_47.dll", &Pe::default());
        let s = scan_exe(tmp.path(), &exe).unwrap();
        assert_eq!(s.graphics, GraphicsApi::Ddraw2d);
        assert_eq!(
            s.local_overrides,
            BTreeSet::from(["d3dcompiler_47.dll".into(), "ddraw.dll".into()])
        );
        assert_eq!(s.helpers, BTreeSet::from(["xinput1_3.dll".into()]));
    }

    #[test]
    fn unity_by_files_and_heuristic_main_exe() {
        let tmp = tempfile::tempdir().unwrap();
        put(
            tmp.path(),
            "Game.exe",
            &Pe {
                pe64: true,
                imports: &["UnityPlayer.dll"],
                ..Pe::default()
            },
        );
        put(
            tmp.path(),
            "UnityPlayer.dll",
            &Pe {
                pe64: true,
                imports: &["opengl32.dll"],
                blob: b"\0d3d11.dll\0d3d12.dll\0dxgi.dll\0",
                ..Pe::default()
            },
        );
        // The crash handler is the largest exe, and still not the main one.
        put(
            tmp.path(),
            "UnityCrashHandler64.exe",
            &Pe {
                blob: &[0; 0x1000],
                ..Pe::default()
            },
        );
        std::fs::create_dir(tmp.path().join("Game_Data")).unwrap();
        let s = scan(tmp.path()).unwrap();
        assert_eq!(s.main_exe, "Game.exe");
        assert_eq!(s.main_exe_source, MainExeSource::Heuristic);
        assert_eq!(s.engine, Some(Engine::Unity));
        assert_eq!(s.bitness, Bitness::X86_64);
        // Neo Cab shape: opengl32 imported, D3D11 runtime-loaded and preferred.
        assert_eq!(s.graphics, GraphicsApi::D3d11);
        assert_eq!(s.graphics_source, GraphicsSource::RuntimeStrings);
        assert!(!s.files.iter().any(|f| f.path.contains("CrashHandler")));
    }

    #[test]
    fn clickteam_and_java_engines_imply_an_api() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = put(
            tmp.path(),
            "mmf/game.exe",
            &Pe {
                imports: &["kernel32.dll", "mmfs2.dll"],
                ..Pe::default()
            },
        );
        let s = scan_exe(tmp.path(), &exe).unwrap();
        assert_eq!(s.engine, Some(Engine::Clickteam));
        assert_eq!(s.graphics, GraphicsApi::D3dLegacy);
        assert_eq!(s.graphics_source, GraphicsSource::Engine);

        let java = tmp.path().join("java");
        let exe = put(
            &java,
            "Game.exe",
            &Pe {
                pe64: true,
                ..Pe::default()
            },
        );
        std::fs::create_dir_all(java.join("runtime/bin")).unwrap();
        std::fs::write(
            java.join("Game.jar"),
            b"PK..com/badlogic/gdx/backends/lwjgl3/",
        )
        .unwrap();
        let s = scan_exe(tmp.path(), &exe).unwrap();
        assert_eq!(s.engine, Some(Engine::Java));
        assert_eq!(s.graphics, GraphicsApi::OpenGl);
    }

    #[test]
    fn non_pe_main_exe_is_an_error_and_json_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = tmp.path().join("bad.exe");
        std::fs::write(&bad, b"MZ not really").unwrap();
        assert!(matches!(
            scan_exe(tmp.path(), &bad),
            Err(ScanError::NotPe { .. })
        ));
        assert!(matches!(
            scan(&tmp.path().join("empty")),
            Err(ScanError::NoExecutable { .. })
        ));

        let exe = put(tmp.path(), "ok.exe", &Pe::default());
        let s = scan_exe(tmp.path(), &exe).unwrap();
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["bitness"], "i386");
        assert_eq!(json["graphics"], "unknown");
        assert_eq!(json["d3d7_confidence"], serde_json::Value::Null);
        assert_eq!(json["dxguid_block"], false);
        let back: PeScan = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back.main_exe, "ok.exe");

        // JSON from before #173 (no confidence, block or GUID count) reads.
        let mut old = json;
        for k in ["d3d7_confidence", "dxguid_block", "renderer_dlls"] {
            old.as_object_mut().unwrap().remove(k);
        }
        for f in old["files"].as_array_mut().unwrap() {
            for k in ["ddraw_guids", "dxguid_block", "d3d7"] {
                f.as_object_mut().unwrap().remove(k);
            }
        }
        let back: PeScan = serde_json::from_value(old).unwrap();
        assert_eq!(back.d3d7_confidence, None);
    }

    // --- #226: the entry point decides, other exes only as a fallback ---

    fn imports(list: &'static [&'static str]) -> Pe<'static> {
        Pe {
            imports: list,
            ..Pe::default()
        }
    }

    #[test]
    fn tool_exe_does_not_outweigh_the_entry_point() {
        let tmp = tempfile::tempdir().unwrap();
        put(tmp.path(), "Sc3k.exe", &imports(&["ddraw.dll"]));
        put(
            tmp.path(),
            "Baapp.exe",
            &imports(&["ddraw.dll", "d3dim.dll"]),
        );
        info(tmp.path(), "Sc3k.exe");
        let s = scan(tmp.path()).unwrap();
        assert_eq!(s.graphics, GraphicsApi::Ddraw2d);
        assert!(!s.graphics_imports.contains(&GraphicsImport::Direct3d7));
        assert_eq!(s.d3d7_confidence, None);
        assert_eq!(
            s.secondary_graphics["Baapp.exe"],
            BTreeSet::from([GraphicsImport::Ddraw, GraphicsImport::Direct3d7])
        );
        // The same tree, entry point unknown: today's behaviour.
        let t = scan_exe(tmp.path(), &tmp.path().join("Baapp.exe")).unwrap();
        assert_eq!(t.graphics, GraphicsApi::D3dLegacy);
    }

    #[test]
    fn stub_entry_point_falls_back_to_other_exes() {
        let tmp = tempfile::tempdir().unwrap();
        put(tmp.path(), "Launcher.exe", &imports(&["kernel32.dll"]));
        put(tmp.path(), "Game.exe", &imports(&["d3d9.dll"]));
        info(tmp.path(), "Launcher.exe");
        let s = scan(tmp.path()).unwrap();
        assert_eq!(s.graphics, GraphicsApi::D3dLegacy);
        assert!(s.graphics_imports.contains(&GraphicsImport::D3d9));
        assert!(s.secondary_graphics.is_empty());
    }

    #[test]
    fn no_info_counts_every_exe_as_before() {
        let tmp = tempfile::tempdir().unwrap();
        put(tmp.path(), "Big.exe", &imports(&["ddraw.dll"]));
        put(tmp.path(), "Edit.exe", &imports(&["d3d9.dll"]));
        let s = scan(tmp.path()).unwrap();
        assert_eq!(s.main_exe_source, MainExeSource::Heuristic);
        assert!(s.graphics_imports.contains(&GraphicsImport::D3d9));
        assert!(s.secondary_graphics.is_empty());
    }

    #[test]
    fn dlls_beside_the_entry_point_still_count() {
        let tmp = tempfile::tempdir().unwrap();
        put(tmp.path(), "Game.exe", &imports(&["kernel32.dll"]));
        put(tmp.path(), "engine.dll", &imports(&["d3d9.dll"]));
        put(tmp.path(), "Tool.exe", &imports(&["opengl32.dll"]));
        info(tmp.path(), "Game.exe");
        let s = scan(tmp.path()).unwrap();
        assert!(s.graphics_imports.contains(&GraphicsImport::D3d9));
        assert!(!s.graphics_imports.contains(&GraphicsImport::Opengl32));
        assert!(s.secondary_graphics.contains_key("Tool.exe"));
    }
}
