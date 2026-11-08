//! gogo's shipped table of switches (design 18 §7.7): what the build can't
//! say about itself. One line each, in gogo's own words; upstream's docs are
//! a link away ([`Component::link`]).
//!
//! The table was reviewed against [`REVIEWED_FOR`], and tests hold it to that
//! build's names (`tests/fixtures/linuxapp/catalogue/`): a name the build reads
//! with no entry fails, and so does an entry the build has lost without being
//! marked [`Spec::absent`]. A catalog pin bump fails until both are redone.

use serde::Serialize;

use crate::macapp::renderer::Renderer;

/// The Proton build this table was last checked against.
pub const REVIEWED_FOR: &str = "GE-Proton11-12";

/// Where a switch belongs, in the order `env list --all` shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Component {
    Proton,
    DxvkNvapi,
    Vkd3dProton,
    Dxvk,
    D7vk,
    Wine,
    /// MangoHud, GameMode, gamescope: host tools, not part of the build.
    Wrappers,
    /// Set for this game, known to nothing else.
    Other,
    /// Read by the build, but Steam's, umu's or the build's own business.
    Plumbing,
}

impl Component {
    pub fn id(self) -> &'static str {
        match self {
            Component::Proton => "proton",
            Component::DxvkNvapi => "dxvk-nvapi",
            Component::Vkd3dProton => "vkd3d-proton",
            Component::Dxvk => "dxvk",
            Component::D7vk => "d7vk",
            Component::Wine => "wine",
            Component::Wrappers => "wrappers",
            Component::Other => "other",
            Component::Plumbing => "plumbing",
        }
    }

    /// Upstream's documentation, where it has a stable home.
    pub fn link(self) -> Option<&'static str> {
        match self {
            Component::Proton => Some("https://github.com/GloriousEggroll/proton-ge-custom#readme"),
            Component::DxvkNvapi => Some("https://github.com/jp7677/dxvk-nvapi#readme"),
            Component::Vkd3dProton => {
                Some("https://github.com/HansKristian-Work/vkd3d-proton#readme")
            }
            Component::Dxvk => Some("https://github.com/doitsujin/dxvk#readme"),
            Component::D7vk => Some("https://github.com/WinterSnowfall/d7vk#readme"),
            Component::Wine => Some("https://gitlab.winehq.org/wine/wine/-/wikis/Debug-Channels"),
            Component::Wrappers | Component::Other | Component::Plumbing => None,
        }
    }

    /// A component for a name the table doesn't have.
    pub fn guess(name: &str) -> Component {
        if name.starts_with("DXVK_NVAPI_") {
            Component::DxvkNvapi
        } else if name.starts_with("DXVK_") {
            Component::Dxvk
        } else if name.starts_with("VKD3D_") {
            Component::Vkd3dProton
        } else if name.starts_with("D7VK_") {
            Component::D7vk
        } else if name.starts_with("WINE") || name.starts_with("WAYLANDDRV_") {
            Component::Wine
        } else if name.starts_with("PROTON") {
            Component::Proton
        } else {
            Component::Other
        }
    }
}

/// What kind of thing a name is, and so how it's set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// An env var.
    Env,
    /// A word in `VKD3D_CONFIG`'s comma-separated list.
    Vkd3dFlag,
    /// A key in `DXVK_CONFIG` (`key=value;key=value`), or `dxvk.conf`.
    DxvkConf,
    /// A command that wraps the game.
    Command,
}

/// What a switch is about, the order inside a component: the flagship's
/// NVIDIA and upscaler switches first (#232).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Topic {
    Nvidia,
    Upscaling,
    RayTracing,
    Renderer,
    Display,
    Performance,
    Sync,
    Input,
    Compat,
    Debug,
}

/// A GPU vendor, as the kernel's DRM layer reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Nvidia,
    Amd,
    Intel,
}

impl Vendor {
    pub fn label(self) -> &'static str {
        match self {
            Vendor::Nvidia => "NVIDIA",
            Vendor::Amd => "AMD",
            Vendor::Intel => "Intel",
        }
    }
}

/// One table entry.
#[derive(Debug, Clone, Copy)]
pub struct Spec {
    pub name: &'static str,
    pub component: Component,
    pub kind: Kind,
    pub topic: Topic,
    /// The default under this Proton; `""` when there's nothing useful to say.
    pub default: &'static str,
    /// One line, in gogo's words.
    pub about: &'static str,
    /// Renderers it does anything on; empty for any.
    pub renderers: &'static [Renderer],
    /// GPU vendors it does anything on; empty for any.
    pub gpus: &'static [Vendor],
    /// The command that must be on `PATH` for it to do anything.
    pub needs: Option<&'static str>,
    /// A reason it can't work on gogo's launcher whatever's installed.
    pub blocked: Option<&'static str>,
    /// The string to look for in the build, when it isn't the name.
    pub probe: Option<&'static str>,
    /// [`REVIEWED_FOR`] doesn't have it: stale advice worth an answer, or newer
    /// builds only. What replaces it, if anything, is in `about`.
    pub absent: bool,
}

impl Spec {
    const fn new(
        name: &'static str,
        component: Component,
        kind: Kind,
        topic: Topic,
        default: &'static str,
        about: &'static str,
    ) -> Spec {
        Spec {
            name,
            component,
            kind,
            topic,
            default,
            about,
            renderers: &[],
            gpus: &[],
            needs: None,
            blocked: None,
            probe: None,
            absent: false,
        }
    }
    const fn on(mut self, renderers: &'static [Renderer]) -> Spec {
        self.renderers = renderers;
        self
    }
    const fn gpu(mut self, gpus: &'static [Vendor]) -> Spec {
        self.gpus = gpus;
        self
    }
    const fn needs(mut self, command: &'static str) -> Spec {
        self.needs = Some(command);
        self
    }
    const fn blocked(mut self, why: &'static str) -> Spec {
        self.blocked = Some(why);
        self
    }
    const fn probe(mut self, s: &'static str) -> Spec {
        self.probe = Some(s);
        self
    }
    const fn absent(mut self) -> Spec {
        self.absent = true;
        self
    }
}

use Component as C;
use Renderer::{D7vk, Dxvk, Vkd3dProton};
use Topic as T;

/// DXVK's `dxgi.dll` also fronts VKD3D-Proton under Proton, so its switches
/// reach D3D12 games too.
const DXGI: &[Renderer] = &[Dxvk, Vkd3dProton, D7vk];
/// DXVK's D3D9-11 side, and D7VK, which is built on it.
const D3D9_11: &[Renderer] = &[Dxvk, D7vk];
const D3D12: &[Renderer] = &[Vkd3dProton];
/// dxvk-nvapi forwards into DXVK or VKD3D-Proton.
const D3D11_12: &[Renderer] = &[Dxvk, Vkd3dProton];

const NV: &[Vendor] = &[Vendor::Nvidia];
const AMD: &[Vendor] = &[Vendor::Amd];
const INTEL: &[Vendor] = &[Vendor::Intel];
const NOT_NV: &[Vendor] = &[Vendor::Amd, Vendor::Intel];

const fn p(name: &'static str, t: Topic, default: &'static str, about: &'static str) -> Spec {
    Spec::new(name, C::Proton, Kind::Env, t, default, about)
}
const fn n(name: &'static str, t: Topic, default: &'static str, about: &'static str) -> Spec {
    Spec::new(name, C::DxvkNvapi, Kind::Env, t, default, about).on(D3D11_12)
}
/// A dxvk-nvapi driver setting with its own `DXVK_NVAPI_DRS_<setting>` var:
/// the DLL holds only the setting's name.
const fn drs(name: &'static str, setting: &'static str, about: &'static str) -> Spec {
    Spec::new(
        name,
        C::DxvkNvapi,
        Kind::Env,
        T::Nvidia,
        "the game's choice",
        about,
    )
    .on(D3D11_12)
    .gpu(NV)
    .probe(setting)
}
const fn v(name: &'static str, t: Topic, default: &'static str, about: &'static str) -> Spec {
    Spec::new(name, C::Vkd3dProton, Kind::Env, t, default, about).on(D3D12)
}
const fn vf(name: &'static str, t: Topic, about: &'static str) -> Spec {
    Spec::new(name, C::Vkd3dProton, Kind::Vkd3dFlag, t, "off", about).on(D3D12)
}
const fn d(name: &'static str, t: Topic, default: &'static str, about: &'static str) -> Spec {
    Spec::new(name, C::Dxvk, Kind::Env, t, default, about).on(DXGI)
}
const fn dc(name: &'static str, t: Topic, default: &'static str, about: &'static str) -> Spec {
    Spec::new(name, C::Dxvk, Kind::DxvkConf, t, default, about).on(DXGI)
}
const fn w(name: &'static str, t: Topic, default: &'static str, about: &'static str) -> Spec {
    Spec::new(name, C::Wine, Kind::Env, t, default, about)
}

/// Read by the build, but not a switch for a player: Steam's and umu's
/// plumbing, the build's own hand-offs, debug-only internals. `env list --all`
/// names them on one line, so nothing is hidden, and they need no words.
pub const PLUMBING: &[&str] = &[
    "FEX_APP_CONFIG",
    "FEX_DISKCACHE",
    "FSR4_UPGRADE",
    "HOST_LC_ALL",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LD_PRELOAD",
    "MLFG_UPGRADE",
    "PRESSURE_VESSEL_RUNTIME_BASE",
    "PROTON_BATTLEYE_RUNTIME",
    "PROTON_EAC_RUNTIME",
    "PROTON_SYSCALL_HACK",
    "PROTON_VERB",
    "SDL_GAMECONTROLLER_ALLOW_STEAM_VIRTUAL_GAMEPAD",
    "SDL_GAMECONTROLLER_IGNORE_DEVICES",
    "STEAM_BASE_FOLDER",
    "STEAM_COMPAT_APP_ID",
    "STEAM_COMPAT_CLIENT_INSTALL_PATH",
    "STEAM_COMPAT_DATA_PATH",
    "STEAM_COMPAT_FEX_CONFIG",
    "STEAM_COMPAT_INSTALL_PATH",
    "STEAM_COMPAT_LIBRARY_PATHS",
    "STEAM_COMPAT_MEDIA_PATH",
    "STEAM_COMPAT_SHADER_PATH",
    "STEAM_COMPAT_TRANSCODED_MEDIA_PATH",
    "STEAM_FEX_MULTIBLOCK",
    "STEAM_FEX_TSOENABLED",
    "UMU_ID",
    "UMU_USE_STEAM",
    "VKD3D_QA_HASHES",
    "VKD3D_SHADER_QUIRKS",
    "VKD3D_UNIX_ENV",
    "VKD3D_UNIX_POST_ENV",
    "VK_IMPLICIT_LAYER_PATH",
    "VR_OVERRIDE",
    "VR_PATHREG_OVERRIDE",
    "WAYLAND_DISPLAY",
    "WINEDLLPATH",
    "WINESERVER",
    "WINE_LD_PRELOAD",
    "WINE_MONO_HIDETYPES",
    "WINE_MOVE_HACK",
    "XDG_CONFIG_HOME",
];

/// The launcher has no wrapper setting yet.
const NO_WRAPPER: &str = "gogo's launcher can't wrap the game in a command yet (#271)";

/// The table: sorted by component and topic when shown, not here.
pub const TABLE: &[Spec] = &[
    // — Proton: NVIDIA —
    p(
        "PROTON_DISABLE_NVAPI",
        T::Nvidia,
        "off (NVAPI is on when an NVIDIA driver is loaded)",
        "Turn dxvk-nvapi off for a game that dislikes it; DLSS and Reflex go with it.",
    )
    .gpu(NV),
    p(
        "PROTON_FORCE_NVAPI",
        T::Nvidia,
        "off",
        "Load dxvk-nvapi without an NVIDIA driver and report an NVIDIA GPU (hides AMD's).",
    )
    .gpu(NOT_NV),
    p(
        "PROTON_ENABLE_NVAPI",
        T::Nvidia,
        "",
        "Old advice: this build ignores it. NVAPI is on by itself on NVIDIA; see PROTON_FORCE_NVAPI.",
    )
    .gpu(NV)
    .absent(),
    p(
        "PROTON_ENABLE_NGX_UPDATER",
        T::Nvidia,
        "",
        "Old advice: this build ignores it. PROTON_DLSS_UPGRADE does the job now.",
    )
    .gpu(NV)
    .absent(),
    p(
        "PROTON_HIDE_NVIDIA_GPU",
        T::Nvidia,
        "off",
        "Report an NVIDIA GPU as AMD, for games that crash on it. Costs DLSS and Reflex.",
    )
    .gpu(NV),
    p(
        "PROTON_NVIDIA_LIBS",
        T::Nvidia,
        "off",
        "Shorthand for NVCUDA, NVENC, NVML and NVOPTIX together.",
    )
    .gpu(NV),
    p(
        "PROTON_NVIDIA_LIBS_NO_32BIT",
        T::Nvidia,
        "off",
        "Leave out the 32-bit copies of the NVIDIA libraries.",
    )
    .gpu(NV),
    p(
        "PROTON_NVIDIA_NVCUDA",
        T::Nvidia,
        "off",
        "CUDA for Windows games: PhysX and CUDA-based effects. Needs NVAPI on.",
    )
    .gpu(NV),
    p(
        "PROTON_NVIDIA_NVENC",
        T::Nvidia,
        "off",
        "NVENC video encoding for Windows games (brings CUDA). Needs NVAPI on.",
    )
    .gpu(NV),
    p(
        "PROTON_NVIDIA_NVML",
        T::Nvidia,
        "on when the NVIDIA driver is loaded",
        "Wine's NVML: GPU clocks and temperatures for games and overlays that ask.",
    )
    .gpu(NV),
    p(
        "PROTON_NVIDIA_NVOPTIX",
        T::Nvidia,
        "off",
        "OptiX ray tracing for Windows apps (brings CUDA and NVML). Rarely a game's need.",
    )
    .gpu(NV),
    // — Proton: upscalers (protonfixes' `upscalers.py`, downloads at launch) —
    p(
        "PROTON_DLSS_UPGRADE",
        T::Upscaling,
        "off",
        "At launch, download a newer DLSS DLL for the game and pick the latest preset; 1 or a version.",
    )
    .gpu(NV),
    p(
        "PROTON_DLSS_INDICATOR",
        T::Upscaling,
        "off",
        "Draw NVIDIA's DLSS and frame generation indicator, to see DLSS is really on.",
    )
    .on(D3D11_12)
    .gpu(NV),
    p(
        "PROTON_STREAMLINE_UPGRADE",
        T::Upscaling,
        "off",
        "At launch, download a newer Streamline for games too old for driver overrides; 1 or a version.",
    )
    .gpu(NV),
    p(
        "PROTON_FSR3_UPGRADE",
        T::Upscaling,
        "off",
        "At launch, download newer FSR 3 DLLs for the game; 1 or a version.",
    ),
    p(
        "PROTON_FSR4_UPGRADE",
        T::Upscaling,
        "off",
        "At launch, download the FSR 4 DLL for games with FSR 3.1; 1 or a version. AMD RDNA3 and up.",
    )
    .gpu(AMD),
    p(
        "PROTON_FSR4_INDICATOR",
        T::Upscaling,
        "off",
        "Draw AMD's FSR upscaling and frame generation watermarks.",
    )
    .gpu(AMD),
    p(
        "PROTON_FSR4_RDNA3_UPGRADE",
        T::Upscaling,
        "",
        "Old advice: this build ignores it. PROTON_FSR4_UPGRADE covers RDNA3 now.",
    )
    .gpu(AMD)
    .absent(),
    p(
        "PROTON_FFX3_UPGRADE",
        T::Upscaling,
        "1.0.1.41314, used with OptiScaler",
        "The FidelityFX 3 DLL version OptiScaler gets.",
    ),
    p(
        "PROTON_FFX4_UPGRADE",
        T::Upscaling,
        "the manifest's, used with OptiScaler",
        "The FidelityFX 4 DLL version OptiScaler gets.",
    ),
    p(
        "PROTON_MLFG_UPGRADE",
        T::Upscaling,
        "on for FSR 4",
        "The ML frame generation part of an FSR 4 upgrade; 0 leaves it out.",
    )
    .gpu(AMD),
    p(
        "PROTON_XESS_UPGRADE",
        T::Upscaling,
        "off",
        "At launch, download newer XeSS DLLs for the game; 1 or a version.",
    ),
    p(
        "PROTON_USE_OPTISCALER",
        T::Upscaling,
        "off",
        "Add OptiScaler, which swaps one upscaler for another in-game; 1 or a version. Downloads at launch.",
    ),
    p(
        "PROTON_OPTISCALER_CONFIG",
        T::Upscaling,
        "",
        "Settings written into OptiScaler's config when PROTON_USE_OPTISCALER is on.",
    ),
    p(
        "PROTON_OPTISCALER_NAME",
        T::Upscaling,
        "dxgi.dll",
        "The DLL name OptiScaler is loaded under.",
    ),
    p(
        "PROTON_OPTISCALER_STREAMLINE",
        T::Upscaling,
        "0",
        "Also upgrade Streamline for OptiScaler; 1 or a version.",
    ),
    p(
        "PROTONFIXES_DISABLE_UPSCALERS",
        T::Upscaling,
        "off",
        "Stop every upscaler download above; the game keeps the DLLs it has.",
    ),
    p(
        "PROTON_NATIVE_AGS",
        T::Upscaling,
        "on for a few AMD games",
        "Use AMD's own AGS library with Wine's ADL instead of Proton's replacement.",
    )
    .gpu(AMD),
    // — Proton: renderer —
    p(
        "PROTON_USE_WINED3D",
        T::Renderer,
        "off",
        "Wine's OpenGL renderer for D3D9-12 in place of DXVK and VKD3D-Proton. No DLSS, no ray tracing.",
    ),
    p(
        "PROTON_USE_WINED3D11",
        T::Renderer,
        "off",
        "The old name of PROTON_USE_WINED3D.",
    ),
    p(
        "PROTON_DXVK_D3D8",
        T::Renderer,
        "off",
        "DXVK's d3d8.dll in place of Wine's, for D3D8 games.",
    ),
    p(
        "PROTON_USE_D7VK",
        T::Renderer,
        "off",
        "D7VK's ddraw.dll, Vulkan for D3D7 and older. gogo's d7vk renderer sets it.",
    ),
    p(
        "PROTON_NO_D3D10",
        T::Renderer,
        "off",
        "Hide D3D10 and dxgi, for games that run better falling back to D3D9.",
    ),
    p(
        "PROTON_NO_D3D11",
        T::Renderer,
        "off",
        "Hide D3D11, for games that run better falling back to an older API.",
    ),
    p(
        "PROTON_NO_D3D12",
        T::Renderer,
        "",
        "Old advice: this build ignores it. A game's own launcher option is the way out of D3D12.",
    )
    .absent(),
    p(
        "PROTON_OLD_GL_STRING",
        T::Renderer,
        "off",
        "Shorten the OpenGL extension string for old games that crash on a long one.",
    ),
    p(
        "PROTON_HIDE_INTEL_GPU",
        T::Renderer,
        "off",
        "Hide Intel GPUs from DXVK and VKD3D-Proton.",
    )
    .gpu(INTEL),
    p(
        "PROTON_HIDE_VANGOGH_GPU",
        T::Renderer,
        "off",
        "Hide the Steam Deck's GPU from games.",
    )
    .gpu(AMD),
    p(
        "PROTON_HIDE_APU",
        T::Renderer,
        "off",
        "Hide AMD integrated graphics, so a game picks the discrete GPU.",
    )
    .gpu(AMD),
    p(
        "PROTON_LOCAL_SHADER_CACHE",
        T::Performance,
        "off",
        "Keep DXVK's and VKD3D-Proton's shader caches in the game's compat folder.",
    ),
    p(
        "PROTON_ADD_CONFIG",
        T::Compat,
        "",
        "Comma-separated STEAM_COMPAT_CONFIG tokens, for the ones without a var of their own.",
    ),
    // — Proton: display —
    p(
        "PROTON_ENABLE_WAYLAND",
        T::Display,
        "off (X11 through XWayland)",
        "Wine's native Wayland driver. Needed for HDR; launchers can misbehave on it.",
    ),
    p(
        "PROTON_USE_WAYLAND",
        T::Display,
        "off",
        "The same as PROTON_ENABLE_WAYLAND.",
    ),
    p(
        "PROTON_WAYLAND_MONITOR",
        T::Display,
        "picked by Proton",
        "The output the Wayland driver treats as primary, such as DP-1.",
    ),
    p(
        "PROTON_ENABLE_HDR",
        T::Display,
        "detected",
        "Legacy: HDR is detected by itself now. 0 turns HDR reporting off.",
    ),
    p(
        "PROTON_USE_HDR",
        T::Display,
        "detected",
        "As PROTON_ENABLE_HDR, and wins when both are set.",
    ),
    p(
        "PROTON_FORCE_HDR",
        T::Display,
        "off",
        "Report HDR on every Wayland output, even SDR-reporting ones. Can wash out colours.",
    ),
    p(
        "PROTON_NO_WM_DECORATION",
        T::Display,
        "off",
        "No window manager borders on the game's windows.",
    ),
    p(
        "PROTON_LIMIT_RESOLUTIONS",
        T::Display,
        "off",
        "List only this many display modes, for old games that overflow on a long list.",
    ),
    p(
        "PROTON_USE_X11_EXCLUSIVE",
        T::Display,
        "",
        "Newer GE builds: keep one exe on X11 under Wayland. Not in the reviewed build.",
    )
    .absent(),
    p(
        "DISABLE_WINE_WAYLAND_STEAM_OVERLAY_LAYER",
        T::Display,
        "off",
        "Don't load the layer that carries Steam's overlay onto Wayland. No effect outside Steam.",
    ),
    // — Proton: sync —
    p(
        "PROTON_NO_NTSYNC",
        T::Sync,
        "off (ntsync when the kernel has it)",
        "Don't use the kernel's ntsync. For comparing, or a game it breaks.",
    ),
    p(
        "PROTON_NO_ESYNC",
        T::Sync,
        "off",
        "Don't use eventfd sync.",
    ),
    p(
        "PROTON_NO_FSYNC",
        T::Sync,
        "off",
        "Don't use futex sync.",
    ),
    p(
        "PROTON_USE_WOW64",
        T::Compat,
        "off (on for a few games)",
        "Wine's new-style WoW64: 32-bit games without 32-bit host libraries.",
    ),
    p(
        "PROTON_USE_ARM64",
        T::Compat,
        "1, on aarch64",
        "On an aarch64 host, use the build's ARM64 side; 0 turns that off.",
    ),
    // — Proton: input —
    p(
        "PROTON_USE_SDL",
        T::Input,
        "off",
        "Controllers through SDL instead of HIDRAW and Steam Input.",
    ),
    p(
        "PROTON_PREFER_SDL",
        T::Input,
        "off",
        "The same as PROTON_USE_SDL.",
    ),
    p(
        "PROTON_NO_STEAMINPUT",
        T::Input,
        "follows Steam",
        "Tell Wine's HID bus that Steam Input is off.",
    ),
    p(
        "PROTON_DISABLE_HIDRAW",
        T::Input,
        "off",
        "No HIDRAW for controllers; 1, or a list of vendor/product ids.",
    ),
    p(
        "PROTON_EMULATE_STEAMINPUT",
        T::Input,
        "off",
        "With SDL input, keep Steam's virtual gamepad details rather than dropping them.",
    ),
    p(
        "PROTON_SPOOF_STEAMINPUT_VIDPID",
        T::Input,
        "on for a few games",
        "Report Steam Input's vendor and product ids for the controller.",
    ),
    p(
        "PROTON_STEAMINPUT_FALLBACK",
        T::Input,
        "off",
        "An XInput-backed stand-in for Steam Input, for games that insist on it outside Steam.",
    ),
    p(
        "PROTON_STEAMINPUT_XINPUT_FALLBACK",
        T::Input,
        "off",
        "The XInput-only form of PROTON_STEAMINPUT_FALLBACK.",
    ),
    p(
        "PROTON_STEAMINPUT_LAYOUT_XBOX",
        T::Input,
        "detected",
        "The Steam Input stand-in presents an Xbox One layout.",
    ),
    p(
        "PROTON_STEAMINPUT_LAYOUT_DS4",
        T::Input,
        "detected",
        "The Steam Input stand-in presents a DualShock 4 layout.",
    ),
    p(
        "PROTON_STEAMINPUT_LAYOUT_DS5",
        T::Input,
        "detected",
        "The Steam Input stand-in presents a DualSense layout.",
    ),
    p(
        "PROTON_STEAM_INPUT_XBOX_PROMPTS",
        T::Input,
        "off",
        "Report a Steam Deck or Steam Controller as Xbox, for games that show keyboard prompts.",
    ),
    p(
        "PROTON_XINPUT_PERSISTENT_PLAYER1",
        T::Input,
        "off",
        "Keep player 1 available when the controller is replaced, for games that stop polling.",
    ),
    p(
        "PROTON_SONY_AUTO_XINPUT",
        T::Input,
        "1",
        "An XInput fallback for Sony controllers, dropped once the game reads native HID.",
    ),
    p(
        "PROTON_SONY_HIDRAW_XINPUT",
        T::Input,
        "off",
        "Translate Sony controllers on HIDRAW to XInput, for games with wrong mappings.",
    ),
    p(
        "PROTON_SONY_DUALSENSE_AS_DUALSHOCK4",
        T::Input,
        "off",
        "Present a DualSense as a DualShock 4 v2, for older games with DS4 support only.",
    ),
    p(
        "PROTON_SONY_DUALSENSE_EDGE_AS_DUALSENSE",
        T::Input,
        "off (on for a few games)",
        "Present a DualSense Edge as a plain DualSense.",
    ),
    p(
        "PROTON_SONY_DUALSHOCK4_V2_AS_V1",
        T::Input,
        "off",
        "Present a DualShock 4 v2 as a v1.",
    ),
    p(
        "PROTON_SONY_WINDOWS_DEVICE_NAMES",
        T::Input,
        "off (on for a few games)",
        "Report Sony controllers under their Windows names.",
    ),
    p(
        "PROTON_DUALSENSE_BT_AS_USB",
        T::Input,
        "",
        "Newer GE builds: a Bluetooth DualSense looks wired. Not in the reviewed build.",
    )
    .absent(),
    p(
        "PROTON_ENABLE_WINEBTH",
        T::Input,
        "off",
        "Wine's Bluetooth driver, for Bluetooth LE devices such as smart trainers. Needs BlueZ.",
    ),
    p(
        "PROTON_NO_XIM",
        T::Input,
        "off",
        "No X input method; may help text entry or key handling in some games.",
    ),
    p(
        "PROTON_USE_XALIA",
        T::Input,
        "detected",
        "Xalia, a gamepad UI for keyboard-and-mouse dialogs; 0 turns it off.",
    ),
    // — Proton: compat —
    p(
        "PROTON_SET_GAME_DRIVE",
        T::Compat,
        "0, gogo's default (Proton's own default under umu is on)",
        "Map the S: drive and host folders; gogo turns it off so x: -> $HOME stays out of reach.",
    ),
    p(
        "PROTON_SET_STEAM_DRIVE",
        T::Compat,
        "off",
        "Map Steam's folder as a drive, for tools that look for it.",
    ),
    p(
        "PROTON_FORCE_LARGE_ADDRESS_AWARE",
        T::Compat,
        "on",
        "Let 32-bit games use up to 4 GB; 0 turns it off for one that breaks.",
    ),
    p(
        "PROTON_LIMIT_ADDRESS_SPACE",
        T::Compat,
        "off (on for a few games)",
        "Limit the address space a game sees, for the few that break on a large one.",
    ),
    p(
        "PROTON_HEAP_DELAY_FREE",
        T::Compat,
        "off",
        "Delay freeing memory, for games with use-after-free bugs.",
    ),
    p(
        "PROTON_HEAP_ZERO_MEMORY",
        T::Compat,
        "off",
        "Zero new allocations, for games that read uninitialised memory.",
    ),
    p(
        "PROTON_USE_WRITECOPY",
        T::Compat,
        "off",
        "Simulate Windows' write-copy memory, which a few games depend on.",
    ),
    p(
        "PROTON_DLL_COPY",
        T::Compat,
        "the built-in list",
        "Which DLLs get copied into the prefix rather than linked.",
    ),
    p(
        "PROTON_CPU_TOPOLOGY",
        T::Performance,
        "the real CPU",
        "CPUs the game sees, count:ids such as 8:0,1,2,3,4,5,6,7. The way to dodge E-cores or big CCDs.",
    ),
    p(
        "ENABLE_VKBASALT",
        T::Display,
        "off",
        "Proton's bundled vkBasalt layer: post-processing such as sharpening. Vulkan renderers only.",
    )
    .on(DXGI),
    p(
        "LOW_LATENCY_LAYER",
        T::Performance,
        "off",
        "Proton's bundled low_latency_layer: Reflex-style pacing on GPUs without Reflex.",
    )
    .on(DXGI)
    .gpu(NOT_NV),
    p(
        "PROTON_FRAME_RATE",
        T::Performance,
        "off",
        "The older name of DXVK_FRAME_RATE and VKD3D_FRAME_RATE; those win when set.",
    )
    .on(DXGI),
    p(
        "FNA3D_FORCE_DRIVER",
        T::Compat,
        "",
        "Which graphics backend FNA games use, such as D3D11.",
    ),
    p(
        "PYROVEIL_CONFIG",
        T::Debug,
        "",
        "The JSON recipe for PYROVEIL.",
    )
    .on(DXGI),
    p(
        "PYROVEIL",
        T::Debug,
        "off",
        "Proton's bundled Pyroveil layer, which patches a game's shaders from a JSON recipe.",
    )
    .on(DXGI),
    // — Proton: debugging —
    p(
        "PROTON_LOG",
        T::Debug,
        "off",
        "Write a Wine log to $PROTON_LOG_DIR/steam-<id>.log; 1 or WINEDEBUG channels. Slows the game.",
    ),
    p(
        "PROTON_LOG_DIR",
        T::Debug,
        "$HOME",
        "Where PROTON_LOG writes.",
    ),
    p(
        "PROTON_CRASH_REPORT_DIR",
        T::Debug,
        "off",
        "Write crash logs into this folder; nothing cleans them up.",
    ),
    p(
        "PROTON_REMOTE_DEBUG_CMD",
        T::Debug,
        "off",
        "A command to run alongside the game, for debuggers.",
    ),
    p(
        "PROTON_MEDIACONV_NO_VIDEO",
        T::Compat,
        "off (on for a few games)",
        "Make the media converter skip video, for games that hang on their cutscenes.",
    ),
    p(
        "PROTON_PIPEWIRE_ALSA_PLUGIN",
        T::Compat,
        "set by the build",
        "Where the build finds PipeWire's ALSA plugin.",
    ),
    p(
        "PROTON_ENABLE_MHWILDS_USB_AUDIO",
        T::Compat,
        "on for that game",
        "A Monster Hunter Wilds controller audio fix.",
    ),
    p(
        "PROTON_DEATH_STRANDING_CONTROLLER_EFFECTS",
        T::Compat,
        "on for that game",
        "A Death Stranding DualSense effects fix.",
    ),
    p(
        "PROTON_DEATH_STRANDING_FORCE_ENVIRONMENT_EFFECTS",
        T::Compat,
        "on for that game",
        "A Death Stranding DualSense environment effects fix.",
    ),
    p(
        "PROTON_VR_RUNTIME",
        T::Compat,
        "set by the build",
        "The OpenVR runtime the build found.",
    ),
    p(
        "STEAM_COMPAT_CONFIG",
        T::Compat,
        "",
        "Comma-separated tokens, Steam's route to the same switches. Set by Steam; see PROTON_ADD_CONFIG.",
    ),
    // — dxvk-nvapi —
    n(
        "DXVK_NVAPI_VKREFLEX",
        T::Nvidia,
        "off",
        "The Reflex layer for native Vulkan games. D3D11/12 games get Reflex without it.",
    )
    .on(&[Dxvk])
    .gpu(NV),
    n(
        "DXVK_NVAPI_FAKE_VKREFLEX",
        T::Nvidia,
        "off",
        "Let Vulkan Reflex initialise without the layer. No latency is saved.",
    )
    .on(&[Dxvk])
    .gpu(NV),
    n(
        "DXVK_NVAPI_SET_NGX_DEBUG_OPTIONS",
        T::Nvidia,
        "DLSSIndicator=0,DLSSGIndicator=0,",
        "NGX debug registry keys. PROTON_DLSS_INDICATOR is the friendly way to the indicators.",
    )
    .gpu(NV),
    n(
        "DXVK_NVAPI_DRS_SETTINGS",
        T::Nvidia,
        "",
        "Driver settings as name=value,… ; the dedicated DXVK_NVAPI_DRS_* var wins over a value here.",
    )
    .gpu(NV)
    .probe("DXVK_NVAPI_DRS_"),
    drs(
        "DXVK_NVAPI_DRS_NGX_DLSS_SR_OVERRIDE",
        "NGX_DLSS_SR_OVERRIDE",
        "on lets the driver override the game's DLSS Super Resolution DLL.",
    ),
    drs(
        "DXVK_NVAPI_DRS_NGX_DLSS_SR_MODE",
        "NGX_DLSS_SR_MODE",
        "Force a Super Resolution quality mode.",
    ),
    drs(
        "DXVK_NVAPI_DRS_NGX_DLSS_SR_OVERRIDE_RENDER_PRESET_SELECTION",
        "NGX_DLSS_SR_OVERRIDE_RENDER_PRESET_SELECTION",
        "The Super Resolution preset, such as render_preset_latest or render_preset_k. Needs the override on.",
    ),
    drs(
        "DXVK_NVAPI_DRS_NGX_DLSS_SR_OVERRIDE_SCALING_RATIO",
        "NGX_DLSS_SR_OVERRIDE_SCALING_RATIO",
        "A custom Super Resolution render scale.",
    ),
    drs(
        "DXVK_NVAPI_DRS_NGX_DLSS_RR_OVERRIDE",
        "NGX_DLSS_RR_OVERRIDE",
        "on lets the driver override the game's DLSS Ray Reconstruction DLL.",
    ),
    drs(
        "DXVK_NVAPI_DRS_NGX_DLSS_RR_MODE",
        "NGX_DLSS_RR_MODE",
        "Force a Ray Reconstruction quality mode.",
    ),
    drs(
        "DXVK_NVAPI_DRS_NGX_DLSS_RR_OVERRIDE_RENDER_PRESET_SELECTION",
        "NGX_DLSS_RR_OVERRIDE_RENDER_PRESET_SELECTION",
        "The Ray Reconstruction preset. Needs the override on.",
    ),
    drs(
        "DXVK_NVAPI_DRS_NGX_DLSS_RR_OVERRIDE_SCALING_RATIO",
        "NGX_DLSS_RR_OVERRIDE_SCALING_RATIO",
        "A custom Ray Reconstruction render scale.",
    ),
    drs(
        "DXVK_NVAPI_DRS_NGX_DLSS_FG_OVERRIDE",
        "NGX_DLSS_FG_OVERRIDE",
        "on lets the driver override the game's DLSS Frame Generation DLL.",
    ),
    drs(
        "DXVK_NVAPI_DRS_NGX_DLSS_FG_OVERRIDE_RENDER_PRESET_SELECTION",
        "NGX_DLSS_FG_OVERRIDE_RENDER_PRESET_SELECTION",
        "The Frame Generation preset. Needs the override on.",
    ),
    drs(
        "DXVK_NVAPI_DRS_NGX_DLSSG_MODE",
        "NGX_DLSSG_MODE",
        "Frame Generation's mode.",
    ),
    drs(
        "DXVK_NVAPI_DRS_NGX_DLSSG_MULTI_FRAME_COUNT",
        "NGX_DLSSG_MULTI_FRAME_COUNT",
        "How many frames Multi Frame Generation adds per rendered frame.",
    ),
    drs(
        "DXVK_NVAPI_DRS_NGX_DLSSG_DYNAMIC_MULTI_FRAME_COUNT_MAX",
        "NGX_DLSSG_DYNAMIC_MULTI_FRAME_COUNT_MAX",
        "The most frames dynamic Multi Frame Generation may add.",
    ),
    drs(
        "DXVK_NVAPI_DRS_NGX_DLSSG_DYNAMIC_TARGET_FRAME_RATE",
        "NGX_DLSSG_DYNAMIC_TARGET_FRAME_RATE",
        "The frame rate dynamic Multi Frame Generation aims for.",
    ),
    drs(
        "DXVK_NVAPI_DRS_NGX_DLAA_OVERRIDE",
        "NGX_DLAA_OVERRIDE",
        "Force DLAA where the game offers DLSS.",
    ),
    drs(
        "DXVK_NVAPI_DRS_NGX_DLSS_NR_OVERRIDE",
        "NGX_DLSS_NR_OVERRIDE",
        "on lets the driver override the game's DLSS Neural Rendering DLL.",
    ),
    drs(
        "DXVK_NVAPI_DRS_NGX_DLSS_NR_OVERRIDE_RENDER_PRESET_SELECTION",
        "NGX_DLSS_NR_OVERRIDE_RENDER_PRESET_SELECTION",
        "The Neural Rendering preset. Needs the override on.",
    ),
    drs(
        "DXVK_NVAPI_DRS_NGX_DLSS_NR_SL_OVERRIDE",
        "NGX_DLSS_NR_SL_OVERRIDE",
        "The override above, for games going through Streamline.",
    ),
    drs(
        "DXVK_NVAPI_DRS_NGX_DLSS_OVERRIDE_OPTIMAL_SETTINGS",
        "NGX_DLSS_OVERRIDE_OPTIMAL_SETTINGS",
        "Have the driver's scale answer the game's optimal settings query.",
    ),
    drs(
        "DXVK_NVAPI_DRS_SL_DLSS_OVERRIDE",
        "SL_DLSS_OVERRIDE",
        "The Super Resolution override, for games going through Streamline.",
    ),
    n(
        "DXVK_NVAPI_DRIVER_VERSION",
        T::Nvidia,
        "the real one",
        "The driver version to report, 100 to 99999: 47141 is 471.41. For games that gate on it.",
    ),
    n(
        "DXVK_NVAPI_ALLOW_OTHER_DRIVERS",
        T::Nvidia,
        "off",
        "Let dxvk-nvapi start on a non-NVIDIA GPU. PROTON_FORCE_NVAPI sets it.",
    )
    .gpu(NOT_NV),
    n(
        "DXVK_NVAPI_GPU_ARCH",
        T::Nvidia,
        "the real one",
        "The NVIDIA architecture to report, such as GB200 or AD100.",
    ),
    n(
        "DXVK_NVAPI_DISABLE_ENTRYPOINTS",
        T::Nvidia,
        "",
        "Hide these NVAPI functions from the game, comma-separated. For a misbehaving call.",
    ),
    n(
        "DXVK_NVAPI_D3D12_NV_SHADER_EXTN",
        T::Nvidia,
        "",
        "Newer dxvk-nvapi: experimental NVIDIA shader extensions in D3D12. Not in the reviewed build.",
    )
    .on(D3D12)
    .gpu(NV)
    .absent(),
    n(
        "DXVK_NVAPI_LOG_LEVEL",
        T::Debug,
        "off (info with PROTON_LOG)",
        "info logs dxvk-nvapi's work; trace logs every call and slows the game.",
    ),
    n(
        "DXVK_NVAPI_LOG_PATH",
        T::Debug,
        "",
        "A folder for nvapi64.log, on top of the console. The log only grows.",
    ),
    // — VKD3D-Proton: env —
    v(
        "VKD3D_CONFIG",
        T::RayTracing,
        "",
        "A comma-separated list of the flags below.",
    ),
    v(
        "VKD3D_FEATURE_LEVEL",
        T::Renderer,
        "the driver's best",
        "The D3D12 feature level to report, such as 12_2. For games that gate on it.",
    ),
    v(
        "VKD3D_SHADER_MODEL",
        T::Renderer,
        "the driver's best",
        "The shader model to report, such as 6_6.",
    ),
    v(
        "VKD3D_FRAME_RATE",
        T::Performance,
        "off",
        "Cap D3D12 frames per second; 0 uncaps.",
    ),
    v(
        "VKD3D_SWAPCHAIN_PRESENT_MODE",
        T::Display,
        "the game's choice",
        "Force a Vulkan present mode, such as VK_PRESENT_MODE_MAILBOX_KHR.",
    ),
    v(
        "VKD3D_SWAPCHAIN_IMAGES",
        T::Display,
        "",
        "How many swapchain images to ask for.",
    ),
    v(
        "VKD3D_SWAPCHAIN_LATENCY_FRAMES",
        T::Performance,
        "",
        "How many frames the swapchain may queue.",
    ),
    v(
        "VKD3D_SWAPCHAIN_DEBUG_LATENCY",
        T::Debug,
        "off",
        "Log swapchain latency.",
    ),
    v(
        "VKD3D_VULKAN_DEVICE",
        T::Renderer,
        "the first suitable",
        "The Vulkan device by index, on a machine with more than one GPU.",
    ),
    v(
        "VKD3D_FILTER_DEVICE_NAME",
        T::Renderer,
        "",
        "Only use a device whose name holds this text; pair with DXVK_FILTER_DEVICE_NAME.",
    ),
    v(
        "VKD3D_DISABLE_EXTENSIONS",
        T::Compat,
        "",
        "Vulkan extensions to pretend aren't there, semicolon-separated.",
    ),
    v(
        "VKD3D_LIMIT_TESS_FACTORS",
        T::Compat,
        "off",
        "Cap tessellation factors, for a game that overdoes them.",
    ),
    v(
        "VKD3D_SHADER_CACHE",
        T::Performance,
        "on",
        "Switch for VKD3D-Proton's own shader cache.",
    ),
    v(
        "VKD3D_SHADER_CACHE_PATH",
        T::Performance,
        "beside the game",
        "Where vkd3d-proton.cache lives; 0 turns the cache off.",
    ),
    v(
        "VKD3D_DEBUG",
        T::Debug,
        "none",
        "VKD3D-Proton's log level: none, err, fixme, warn, info, trace.",
    ),
    v(
        "VKD3D_SHADER_DEBUG",
        T::Debug,
        "none",
        "The shader compiler's log level, the same words.",
    ),
    v(
        "VKD3D_LOG_FILE",
        T::Debug,
        "",
        "Send the log to this file instead of the console.",
    ),
    v(
        "VKD3D_LOG_BUFFERED",
        T::Debug,
        "",
        "Buffer the log, so logging disturbs timing less.",
    ),
    v(
        "VKD3D_QUEUE_PROFILE",
        T::Debug,
        "",
        "Write a queue timeline trace to this file.",
    ),
    v(
        "VKD3D_QUEUE_PROFILE_ABSOLUTE",
        T::Debug,
        "off",
        "Use absolute times in that trace.",
    ),
    v(
        "VKD3D_SHADER_DUMP_PATH",
        T::Debug,
        "",
        "Dump the game's shaders here.",
    ),
    v(
        "VKD3D_SHADER_OVERRIDE",
        T::Debug,
        "",
        "Replace shaders from this folder, by hash.",
    ),
    v(
        "VKD3D_SHADER_DEBUG_RING_SIZE_LOG2",
        T::Debug,
        "",
        "The size of the shader debug ring buffer, as a power of 2.",
    ),
    // — VKD3D-Proton: VKD3D_CONFIG flags —
    vf(
        "dxr",
        T::RayTracing,
        "Force DXR on where VKD3D-Proton thinks it unsafe; it's on by itself where safe.",
    ),
    vf(
        "nodxr",
        T::RayTracing,
        "Turn DXR off: no ray tracing options in the game, for a crash that follows them.",
    ),
    vf(
        "dxr12",
        T::RayTracing,
        "Experimental DXR 1.2, where the driver has opacity micromaps.",
    ),
    vf(
        "force_static_cbv",
        T::Performance,
        "An unsafe NVIDIA speed hack. May give a little, may break.",
    )
    .gpu(NV),
    vf(
        "no_nvx",
        T::Compat,
        "Don't use NVIDIA's NVX extensions; for working around a driver problem.",
    )
    .gpu(NV),
    vf(
        "single_queue",
        T::Compat,
        "No async compute or transfer queues; for hangs that follow them.",
    ),
    vf(
        "no_upload_hvv",
        T::Performance,
        "Keep the upload heap out of resizable BAR, freeing VRAM when it's tight.",
    ),
    vf(
        "small_vram_rebar",
        T::Performance,
        "Use resizable BAR more carefully on GPUs short of VRAM.",
    ),
    vf(
        "no_gpu_upload_heap",
        T::Compat,
        "Don't offer the GPU upload heap to the game.",
    ),
    vf(
        "no_staggered_submit",
        T::Compat,
        "Don't stagger queue submissions.",
    ),
    vf(
        "pipeline_library_app_cache",
        T::Performance,
        "Trust the game's own pipeline cache; the alternative to a shared cache for a game that has one.",
    ),
    vf(
        "global_pipeline_cache",
        T::Performance,
        "One Vulkan pipeline cache for everything, in place of per-library caches.",
    ),
    vf(
        "shader_cache_sync",
        T::Debug,
        "Write the shader cache as it fills, not in the background.",
    ),
    vf(
        "skip_application_workarounds",
        T::Debug,
        "Skip per-game workarounds, to see whether one is the problem.",
    ),
    vf(
        "skip_driver_workarounds",
        T::Debug,
        "Skip per-driver workarounds, to see whether one is the problem.",
    ),
    vf(
        "no_invariant_position",
        T::Compat,
        "Drop the invariant position workaround, on by default.",
    ),
    vf(
        "breadcrumbs",
        T::Debug,
        "Instrument command lists to find what a GPU hang was running. Slows the game.",
    ),
    vf(
        "vk_debug",
        T::Debug,
        "Vulkan debug extensions and the validation layer.",
    ),
    vf(
        "force_host_cached",
        T::Debug,
        "Cached host-visible allocations; speeds up captures.",
    ),
    // — DXVK: env —
    Spec::new(
        "DXVK_HUD",
        C::Dxvk,
        Kind::Env,
        T::Display,
        "off",
        "DXVK's overlay, such as fps,gpuload or devinfo,fps. D3D9-11 only: MangoHud is the D3D12 way.",
    )
    .on(D3D9_11),
    d(
        "DXVK_ENABLE_NVAPI",
        T::Nvidia,
        "1 under Proton while NVAPI is on",
        "Let dxgi.dll report the real NVIDIA GPU for DLSS. Proton sets it; yours is overwritten.",
    )
    .gpu(NV),
    d(
        "DXVK_FILTER_DEVICE_NAME",
        T::Renderer,
        "",
        "Only use a device whose name holds this text. How to pick a GPU; D3D12 needs it too.",
    ),
    d(
        "DXVK_FILTER_DEVICE_UUID",
        T::Renderer,
        "",
        "Only use the device with this Vulkan UUID.",
    ),
    d(
        "DXVK_CONFIG",
        T::Renderer,
        "",
        "dxvk.conf settings in the environment: key = value; key = value.",
    ),
    d(
        "DXVK_CONFIG_FILE",
        T::Renderer,
        "dxvk.conf beside the exe",
        "The path of a dxvk.conf.",
    ),
    d(
        "DXVK_FRAME_RATE",
        T::Performance,
        "off",
        "Cap frames per second; 0 uncaps. Proton turns it into dxgi.maxFrameRate.",
    ),
    d(
        "DXVK_HDR",
        T::Display,
        "detected",
        "Proton's way of telling dxgi.dll about HDR; yours is overwritten when an HDR switch is set.",
    ),
    d(
        "DXVK_NO_HDR",
        T::Display,
        "off",
        "No HDR reporting, D3D12 included. Beats every switch that turns HDR on.",
    ),
    d(
        "DXVK_SHADER_CACHE",
        T::Performance,
        "on",
        "0 turns DXVK's shader cache off.",
    ),
    d(
        "DXVK_SHADER_CACHE_PATH",
        T::Performance,
        "beside the game",
        "Where DXVK keeps its shader cache.",
    ),
    d(
        "DXVK_LOG_LEVEL",
        T::Debug,
        "none (info with PROTON_LOG)",
        "DXVK's log level: none, error, warn, info, debug.",
    ),
    d(
        "DXVK_LOG_PATH",
        T::Debug,
        "",
        "A folder for DXVK's log files; none turns them off.",
    ),
    d(
        "DXVK_DEBUG",
        T::Debug,
        "",
        "Debug modes for tools: markers, validation and the like.",
    ),
    d(
        "DXVK_CAPTURE_FRAMES",
        T::Debug,
        "",
        "Capture a range of frames with a debugging layer.",
    ),
    d(
        "DXVK_SHADER_DUMP_PATH",
        T::Debug,
        "",
        "Dump the game's shaders here.",
    ),
    d(
        "DXVK_NO_VR",
        T::Compat,
        "off",
        "Skip DXVK's OpenVR and OpenXR setup.",
    ),
    Spec::new(
        "DXVK_FORCE_WINDOWED",
        C::Dxvk,
        Kind::Env,
        T::Display,
        "off",
        "Force windowed mode in D3D9 games.",
    )
    .on(D3D9_11),
    d(
        "DXVK_WSI_DRIVER",
        T::Debug,
        "",
        "The window backend for DXVK Native builds; nothing to set under Proton.",
    ),
    Spec::new(
        "DXVK_ASYNC",
        C::Dxvk,
        Kind::Env,
        T::Performance,
        "",
        "Old advice: gone from DXVK. Graphics pipeline libraries do this now, with nothing to set.",
    )
    .on(DXGI)
    .absent(),
    Spec::new(
        "DXVK_STATE_CACHE",
        C::Dxvk,
        Kind::Env,
        T::Performance,
        "",
        "Old advice: gone from DXVK. See DXVK_SHADER_CACHE.",
    )
    .on(DXGI)
    .absent(),
    // — DXVK: dxvk.conf keys, through DXVK_CONFIG —
    dc(
        "dxgi.maxFrameRate",
        T::Performance,
        "0 (off)",
        "A frame rate cap, in DXVK.",
    ),
    dc(
        "dxgi.maxFrameLatency",
        T::Performance,
        "the game's",
        "Frames the CPU may queue ahead; lower cuts latency.",
    ),
    dc(
        "dxgi.syncInterval",
        T::Display,
        "the game's",
        "Force vsync: 0 off, 1 on.",
    ),
    dc(
        "dxgi.hideNvidiaGpu",
        T::Nvidia,
        "Auto",
        "Hide an NVIDIA GPU from the game.",
    )
    .gpu(NV),
    dc(
        "dxgi.hideAmdGpu",
        T::Compat,
        "Auto",
        "Hide an AMD GPU from the game.",
    )
    .gpu(AMD),
    dc(
        "dxgi.customVendorId",
        T::Compat,
        "",
        "Report this PCI vendor id, in hex without 0x.",
    ),
    dc(
        "dxgi.customDeviceId",
        T::Compat,
        "",
        "Report this PCI device id, in hex without 0x.",
    ),
    dc(
        "dxgi.maxDeviceMemory",
        T::Compat,
        "",
        "Report at most this much VRAM in MiB, for old games that overflow on a lot.",
    ),
    dc(
        "dxgi.enableHDR",
        T::Display,
        "Auto",
        "Let D3D11 games see HDR.",
    ),
    dc(
        "dxvk.disableNvLowLatency2",
        T::Nvidia,
        "Auto",
        "Stop DXVK using VK_NV_low_latency2, what Reflex runs on in D3D11 games.",
    )
    .on(D3D9_11)
    .gpu(NV),
    dc(
        "dxvk.numCompilerThreads",
        T::Performance,
        "all cores",
        "Threads for compiling shaders.",
    ),
    dc(
        "dxvk.enableGraphicsPipelineLibrary",
        T::Performance,
        "Auto",
        "Graphics pipeline libraries: what removes most shader stutter.",
    ),
    dc(
        "dxvk.hud",
        T::Display,
        "",
        "DXVK_HUD's value, in dxvk.conf form.",
    )
    .on(D3D9_11),
    dc(
        "d3d11.maxFeatureLevel",
        T::Compat,
        "the driver's best",
        "Cap the feature level D3D11 reports, such as 11_0.",
    )
    .on(D3D9_11),
    // — D7VK —
    Spec::new(
        "D7VK_LOG_LEVEL",
        C::D7vk,
        Kind::Env,
        T::Debug,
        "",
        "D7VK's log level: the same words as DXVK_LOG_LEVEL.",
    )
    .on(&[D7vk]),
    Spec::new(
        "D7VK_LOG_PATH",
        C::D7vk,
        Kind::Env,
        T::Debug,
        "",
        "A folder for D7VK's log files.",
    )
    .on(&[D7vk]),
    // — Wine —
    w(
        "WINEDEBUG",
        T::Debug,
        "-all",
        "Wine's log channels, such as +seh,+loaddll. Prefer PROTON_LOG.",
    ),
    w(
        "WINE_CPU_TOPOLOGY",
        T::Performance,
        "the real CPU",
        "The CPUs Wine reports. PROTON_CPU_TOPOLOGY sets it.",
    ),
    w(
        "WINE_FULLSCREEN_FSR",
        T::Upscaling,
        "off",
        "GE's FSR 1 at the Wine level: upscale from a lower fullscreen mode. Works on any Vulkan game.",
    ),
    w(
        "WINE_FULLSCREEN_FSR_STRENGTH",
        T::Upscaling,
        "2",
        "That upscaler's sharpening: 0 is sharpest, 5 softest.",
    ),
    w(
        "WINE_FULLSCREEN_FSR_CUSTOM_MODE",
        T::Upscaling,
        "",
        "Add a fake fullscreen resolution for it, WIDTHxHEIGHT.",
    ),
    w(
        "WINE_HIDE_AMD_CPU",
        T::Compat,
        "off",
        "Report an AMD CPU as Intel, for games that misbehave on one.",
    ),
    w(
        "WINE_HIDE_AMD_GPU",
        T::Compat,
        "off (PROTON_FORCE_NVAPI sets it)",
        "Report an AMD GPU as NVIDIA.",
    )
    .gpu(AMD),
    w(
        "WINE_DISABLE_HARDWARE_SCHEDULING",
        T::Compat,
        "off (on for a few games)",
        "Report hardware GPU scheduling as off.",
    ),
    w(
        "WINE_DISABLE_GAMESCOPE_MAX_SIZE_HACK",
        T::Display,
        "off (on for a few games)",
        "Drop a window size workaround used under gamescope.",
    ),
    w(
        "WINE_HIDE_NVIDIA_GPU",
        T::Nvidia,
        "off",
        "What PROTON_HIDE_NVIDIA_GPU sets.",
    )
    .gpu(NV),
    w(
        "WAYLANDDRV_PRIMARY_MONITOR",
        T::Display,
        "picked by Proton",
        "What PROTON_WAYLAND_MONITOR sets.",
    ),
    w(
        "WAYLANDDRV_FORCE_HDR",
        T::Display,
        "off",
        "What PROTON_FORCE_HDR sets.",
    ),
    w(
        "WAYLANDDRV_RAWINPUT",
        T::Input,
        "on",
        "Wayland driver: 0 uses accelerated input; another number scales raw input.",
    ),
    w(
        "WAYLANDDRV_FOCUS_LOSS",
        T::Display,
        "",
        "Wayland driver: how a fullscreen game reacts to losing focus.",
    ),
    w(
        "WAYLANDDRV_IME",
        T::Input,
        "",
        "Wayland driver: the input method switch.",
    ),
    w(
        "WAYLANDDRV_SHORTCUT_INHIBIT",
        T::Input,
        "",
        "Wayland driver: whether the game keeps the compositor's shortcuts from firing.",
    ),
    w(
        "WAYLANDDRV_SSD",
        T::Display,
        "on",
        "Wayland driver: server-side window borders. PROTON_NO_WM_DECORATION sets it to 0.",
    ),
    w(
        "WINE_MONO_TRACE",
        T::Debug,
        "off (set with PROTON_LOG)",
        "Trace filter for Wine Mono, the .NET replacement.",
    ),
    w(
        "MONO_LOG_LEVEL",
        T::Debug,
        "",
        "Wine Mono's log level.",
    ),
    w(
        "GST_DEBUG",
        T::Debug,
        "",
        "GStreamer's log level, for video and audio problems.",
    ),
    w(
        "GST_DEBUG_NO_COLOR",
        T::Debug,
        "",
        "No colour codes in GStreamer's log.",
    ),
    w(
        "XALIA_DEBUG",
        T::Debug,
        "",
        "Xalia's log.",
    ),
    w(
        "XALIA_DEBUG_INPUT",
        T::Debug,
        "",
        "Xalia's input log.",
    ),
    // — Wrappers —
    Spec::new(
        "MANGOHUD",
        C::Wrappers,
        Kind::Env,
        T::Display,
        "off",
        "1 turns on MangoHud's overlay (the D3D12-capable one), if its Vulkan layer is installed.",
    )
    .on(DXGI)
    .needs("mangohud"),
    Spec::new(
        "MANGOHUD_CONFIG",
        C::Wrappers,
        Kind::Env,
        T::Display,
        "",
        "MangoHud's settings, comma-separated, such as fps_limit=60,no_display.",
    )
    .on(DXGI)
    .needs("mangohud"),
    Spec::new(
        "gamemoderun",
        C::Wrappers,
        Kind::Command,
        T::Performance,
        "not used",
        "GameMode's wrapper: asks the daemon for performance settings while the game runs.",
    )
    .needs("gamemoderun")
    .blocked(NO_WRAPPER),
    Spec::new(
        "gamescope",
        C::Wrappers,
        Kind::Command,
        T::Display,
        "not used",
        "Valve's micro-compositor: its own resolution, scaling and frame limit for one game.",
    )
    .needs("gamescope")
    .blocked(NO_WRAPPER),
];
