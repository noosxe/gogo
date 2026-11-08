use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::table::{Component, Kind, REVIEWED_FOR, TABLE, Topic};
use super::*;
use crate::linuxapp::envargs::{EnvLine, EnvReport, EnvSource, EnvState};
use crate::macapp::catalog::{self, Discovery, GE_PROTON};
use crate::macapp::compat::Layer;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/linuxapp/catalogue/"
);

/// A small `proton`, one of each shape the real script uses.
const SCRIPT: &str = r#"
if "PROTON_ADD_CONFIG" in os.environ:
    config = os.environ["PROTON_ADD_CONFIG"].split(',')

class Session:
    def init(self):
        self.check_environment("PROTON_DLSS_UPGRADE", "dlss")
        self.env.setdefault("DXVK_LOG_LEVEL", "none")
        self.env.setdefault("WINEDEBUG", "-all")
        if "PROTON_LOG" in self.env and nonzero(self.env["PROTON_LOG"]):
            self.env.setdefault("DXVK_NVAPI_LOG_LEVEL", "info")
        self.env["DXVK_ENABLE_NVAPI"] = "1"
        append_to_env_str(self.env, "VK_IMPLICIT_LAYER_PATH", "x", ":")
        self.env.pop("WINENTSYNC", "")
        if "PROTON_CPU_TOPOLOGY" not in self.env:
            pass
        x = os.environ.get("PROTON_USE_ARM64", "1")
        y = self.env["PROTON_WAYLAND_MONITOR"]
        # self.env["PROTON_IN_A_COMMENT"] = "1"
        z = "NOT_ENV_USE"
        if self.env.get("A_STRAY") == "B_STRAY":
            pass
        names = {"NOT_ENV_DICT": 1}
"#;

const FIXES: &str = r#"
def setup(env):
    v = get_version(env, 'PROTON_FFX3_UPGRADE', '1.0')
    w = env.get('PROTON_OPTISCALER_NAME', 'dxgi.dll')
    r = env.get(
        'DXVK_FRAME_RATE',
        env.get('VKD3D_FRAME_RATE', env.pop('PROTON_FRAME_RATE', None)),
    )
"#;

const SAMPLE: &str = r#"
user_settings = {
    "PROTON_LOG": "1",
    #"WINEDEBUG": "+timestamp",
#    "DXVK_HUD": "devinfo,fps",
    #vkd3d debug logging
    "not_a_name": "1",
}
"#;

/// A Proton build on disk, with what `read` looks at.
fn fake_build(dir: &Path) {
    let write = |rel: &str, body: &[u8]| {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    write("version", b"1793687165 GE-Proton11-12\n");
    write("proton", SCRIPT.as_bytes());
    write("protonfixes/upscalers.py", FIXES.as_bytes());
    write("user_settings.sample.py", SAMPLE.as_bytes());
    write(
        "files/lib/wine/dxvk/version",
        b" f8637 dxvk (v3.1.1-174-gf86377680)\n",
    );
    write(
        "files/lib/wine/dxvk/x86_64-windows/dxgi.dll",
        // A whole name, a compiler's pieces of one, and a prefix the code
        // joins with a setting: only the first is a discovery.
        b"MZ\x90\0DXVK_HDR\0\x48\xb8DXVK_ENAH\x89\xfeDXVK_UNDOCUMENTED\0DXVK_NVAPI_DRS_\0",
    );
    write(
        "files/lib/wine/vkd3d-proton/x86_64-windows/d3d12core.dll",
        b"\0VKD3D_CONFIG\0nodxr\0single_queue\0",
    );
    write(
        "files/lib/wine/nvapi/x86_64-windows/nvapi64.dll",
        b"\0DXVK_NVAPI_GPU_ARCH\0NGX_DLSS_SR_OVERRIDE\0",
    );
}

#[test]
fn script_shapes_are_told_apart() {
    let t = tempfile::tempdir().unwrap();
    fake_build(t.path());
    let f = scan::BuildFacts::read(t.path(), &[], &[]);
    assert!(f.script_read);
    assert_eq!(f.version.as_deref(), Some("GE-Proton11-12"));
    assert_eq!(f.components["dxvk"], "v3.1.1-174-gf86377680");

    let u = |k: &str| f.script.get(k).unwrap_or_else(|| panic!("{k}"));
    // Read: every form.
    for k in [
        "PROTON_ADD_CONFIG",
        "PROTON_LOG",
        "PROTON_CPU_TOPOLOGY",
        "PROTON_USE_ARM64",
        "PROTON_WAYLAND_MONITOR",
    ] {
        assert!(u(k).reads, "{k}");
        assert!(!u(k).sets, "{k}");
    }
    // The token is the build's, and a read.
    assert_eq!(u("PROTON_DLSS_UPGRADE").token.as_deref(), Some("dlss"));
    assert!(u("PROTON_DLSS_UPGRADE").reads);
    // A setdefault in the method body is a default; one in an `if` isn't.
    assert_eq!(u("DXVK_LOG_LEVEL").default.as_deref(), Some("none"));
    assert_eq!(u("WINEDEBUG").default.as_deref(), Some("-all"));
    assert_eq!(u("DXVK_NVAPI_LOG_LEVEL").default, None);
    // Set: not something it reads from the user.
    for k in ["DXVK_ENABLE_NVAPI", "VK_IMPLICIT_LAYER_PATH", "WINENTSYNC"] {
        assert!(u(k).sets, "{k}");
        assert!(!u(k).reads, "{k}");
    }
    // Comments and strings that aren't env use count for nothing.
    for k in [
        "PROTON_IN_A_COMMENT",
        "NOT_ENV_USE",
        "NOT_ENV_DICT",
        "B_STRAY",
    ] {
        assert!(!f.script.contains_key(k), "{k}");
    }
    // protonfixes: read through a helper, a split call, a pop with a default.
    for k in [
        "PROTON_FFX3_UPGRADE",
        "PROTON_OPTISCALER_NAME",
        "DXVK_FRAME_RATE",
        "VKD3D_FRAME_RATE",
        "PROTON_FRAME_RATE",
    ] {
        assert!(u(k).fixes_reads, "{k}");
    }
}

#[test]
fn the_sample_names_commented_out_settings_too() {
    let t = tempfile::tempdir().unwrap();
    fake_build(t.path());
    let f = scan::BuildFacts::read(t.path(), &[], &[]);
    let got: Vec<&str> = f.sample.iter().map(String::as_str).collect();
    assert_eq!(got, ["DXVK_HUD", "PROTON_LOG", "WINEDEBUG"]);
}

#[test]
fn dll_strings_discover_whole_names_and_only_those() {
    let t = tempfile::tempdir().unwrap();
    fake_build(t.path());
    let f = scan::BuildFacts::read(
        t.path(),
        &["DXVK_NVAPI_DRS_", "NGX_DLSS_SR_OVERRIDE"],
        &["dxr", "single_queue", "vk_debug"],
    );
    assert!(f.found["DXVK_HDR"].contains("dxgi.dll"));
    assert!(f.found["DXVK_UNDOCUMENTED"].contains("dxgi.dll"));
    assert!(f.found["DXVK_NVAPI_GPU_ARCH"].contains("nvapi64.dll"));
    // Pieces of a name loaded by code don't count.
    assert!(!f.found.contains_key("DXVK_ENAH"));
    // A wanted string counts whatever it looks like.
    assert!(f.found.contains_key("NGX_DLSS_SR_OVERRIDE"));
    assert!(f.found.contains_key("DXVK_NVAPI_DRS_"));
    // A joined prefix isn't a switch name of its own.
    assert!(!f.switch_names().contains("DXVK_NVAPI_DRS_"));
    // A flag may be the tail of a longer string: the linker merges them.
    assert!(f.found.contains_key("dxr"));
    assert!(f.found.contains_key("single_queue"));
    assert!(!f.found.contains_key("vk_debug"));
    assert_eq!(
        f.scanned.iter().map(String::as_str).collect::<Vec<_>>(),
        ["dxvk", "dxvk-nvapi", "vkd3d-proton"]
    );
}

#[test]
fn a_missing_build_part_is_not_evidence_of_absence() {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("proton"), SCRIPT).unwrap();
    let f = read_build(t.path());
    let r = report_of(vec![], Some(&f), Some(Renderer::Vkd3dProton), &["NVIDIA"]);
    let e = |n: &str| r.find(n).unwrap();
    assert_eq!(e("PROTON_DLSS_UPGRADE").in_build, InBuild::Found);
    assert_eq!(e("PROTON_ENABLE_NVAPI").in_build, InBuild::NotFound);
    assert_eq!(e("VKD3D_CONFIG").in_build, InBuild::NotChecked);
    assert_eq!(e("DXVK_HDR").in_build, InBuild::NotChecked);
    assert_eq!(e("MANGOHUD").in_build, InBuild::NotOfBuild);
}

fn line(key: &str, value: &str, source: EnvSource, state: EnvState) -> EnvLine {
    EnvLine {
        key: key.into(),
        value: value.into(),
        source,
        state,
    }
}

fn env_of(lines: Vec<EnvLine>) -> EnvReport {
    EnvReport {
        gogid: "1423049311".into(),
        user_file: "/d/compat/1423049311.toml".into(),
        built: true,
        lines,
        wrapper: None,
        warnings: vec![],
    }
}

fn report_of(
    lines: Vec<EnvLine>,
    facts: Option<&BuildFacts>,
    renderer: Option<Renderer>,
    gpus: &[&str],
) -> CatalogueReport {
    let machine = Machine {
        renderer,
        gpus: gpus.iter().map(|g| (*g).to_string()).collect(),
        on_path: &|c| c == "mangohud",
    };
    merge(
        env_of(lines),
        facts,
        renderer.map(|r| r.id().to_string()),
        &machine,
    )
}

/// A `BuildFacts` naming everything the table does, so applicability is all
/// that decides.
fn full_facts() -> BuildFacts {
    let mut f = BuildFacts {
        version: Some(REVIEWED_FOR.into()),
        script_read: true,
        ..BuildFacts::default()
    };
    for s in TABLE {
        if !s.absent {
            f.found
                .entry(s.probe.unwrap_or(s.name).to_string())
                .or_default()
                .insert("x.dll".into());
        }
    }
    for c in ["dxvk", "dxvk-nvapi", "vkd3d-proton", "d7vk", "wine"] {
        f.scanned.insert(c.into());
    }
    f
}

fn yes(r: &CatalogueReport, name: &str) -> bool {
    r.find(name).unwrap().applies == Applies::Yes
}

#[test]
fn applicability_follows_renderer_gpu_and_tools() {
    let facts = full_facts();
    let nv = report_of(
        vec![],
        Some(&facts),
        Some(Renderer::Vkd3dProton),
        &["NVIDIA"],
    );
    // The flagship's: D3D12 on NVIDIA.
    assert!(yes(&nv, "PROTON_DLSS_UPGRADE"));
    assert!(yes(&nv, "DXVK_NVAPI_DRS_NGX_DLSS_SR_OVERRIDE"));
    assert!(yes(&nv, "VKD3D_CONFIG"));
    assert!(yes(&nv, "nodxr"));
    assert!(yes(&nv, "DXVK_FILTER_DEVICE_NAME"), "dxgi fronts D3D12 too");
    // Not for this renderer, and it says why.
    let hud = &nv.find("DXVK_HUD").unwrap().applies;
    assert_eq!(
        *hud,
        Applies::No {
            why: "for dxvk, d7vk; this game's renderer is vkd3d-proton".into()
        }
    );
    // Not for this GPU.
    assert_eq!(
        nv.find("PROTON_FSR4_UPGRADE").unwrap().applies,
        Applies::No {
            why: "for AMD; this machine has NVIDIA".into()
        }
    );
    assert!(!yes(&nv, "PROTON_FORCE_NVAPI"));

    let amd = report_of(vec![], Some(&facts), Some(Renderer::Dxvk), &["AMD"]);
    assert!(yes(&amd, "PROTON_FSR4_UPGRADE"));
    assert!(yes(&amd, "PROTON_FORCE_NVAPI"));
    assert!(yes(&amd, "DXVK_HUD"));
    assert!(!yes(&amd, "PROTON_DLSS_UPGRADE"));
    assert!(!yes(&amd, "VKD3D_CONFIG"), "D3D12 only");

    // Two GPUs: any will do.
    let both = report_of(
        vec![],
        Some(&facts),
        Some(Renderer::Dxvk),
        &["AMD", "NVIDIA"],
    );
    assert!(yes(&both, "PROTON_DLSS_UPGRADE"));
    assert!(yes(&both, "PROTON_FSR4_UPGRADE"));

    // wined3d: the Vulkan side does nothing, Proton's own switches still do.
    let wd = report_of(vec![], Some(&facts), Some(Renderer::Wined3d), &["NVIDIA"]);
    assert!(!yes(&wd, "DXVK_CONFIG"));
    assert!(!yes(&wd, "VKD3D_CONFIG"));
    assert!(!yes(&wd, "DXVK_NVAPI_DRS_NGX_DLSS_SR_OVERRIDE"));
    assert!(yes(&wd, "PROTON_LOG"));

    // Not known yet: says so instead of guessing.
    let none = report_of(vec![], Some(&facts), None, &[]);
    assert!(matches!(
        none.find("DXVK_HUD").unwrap().applies,
        Applies::Unknown { .. }
    ));
    assert!(matches!(
        none.find("PROTON_DLSS_UPGRADE").unwrap().applies,
        Applies::Unknown { .. }
    ));

    // A tool: on PATH or not.
    assert!(yes(&nv, "MANGOHUD"));
    assert_eq!(
        nv.find("gamescope").unwrap().applies,
        Applies::No {
            why: "`gamescope` isn't on PATH".into()
        }
    );
    assert_eq!(
        nv.find("gamemoderun").unwrap().applies,
        Applies::No {
            why: "`gamemoderun` isn't on PATH".into()
        }
    );
}

#[test]
fn nothing_is_hidden_for_not_applying() {
    let facts = full_facts();
    let r = report_of(vec![], Some(&facts), Some(Renderer::CncDdraw), &["Intel"]);
    for s in TABLE {
        assert!(r.find(s.name).is_some(), "{}", s.name);
    }
}

#[test]
fn stale_advice_is_answered_not_dropped() {
    let facts = full_facts();
    let r = report_of(
        vec![],
        Some(&facts),
        Some(Renderer::Vkd3dProton),
        &["NVIDIA"],
    );
    let e = r.find("PROTON_ENABLE_NVAPI").unwrap();
    assert_eq!(e.in_build, InBuild::NotFound);
    assert_eq!(
        e.applies,
        Applies::No {
            why: "not in this build".into()
        }
    );
    assert!(e.about.as_ref().unwrap().contains("PROTON_FORCE_NVAPI"));
    assert!(matches!(
        r.find("DXVK_ASYNC").unwrap().applies,
        Applies::No { .. }
    ));
}

#[test]
fn active_values_join_their_entries() {
    let facts = full_facts();
    let user = EnvSource::Layer { layer: Layer::User };
    let r = report_of(
        vec![
            line(
                "DXVK_CONFIG",
                "dxgi.maxFrameRate = 60; dxgi.syncInterval=0",
                user.clone(),
                EnvState::Applies,
            ),
            line(
                "PROTON_SET_GAME_DRIVE",
                "1",
                user.clone(),
                EnvState::Applies,
            ),
            line(
                "PROTON_SET_GAME_DRIVE",
                "0",
                EnvSource::GogoDefault,
                EnvState::Overridden {
                    by: "L4 user override".into(),
                },
            ),
            line(
                "VKD3D_CONFIG",
                "nodxr,single_queue",
                user.clone(),
                EnvState::Pending,
            ),
            line("MY_OWN_THING", "x", user.clone(), EnvState::Applies),
            line("GAMEID", "umu-1", EnvSource::Launcher, EnvState::Applies),
        ],
        Some(&facts),
        Some(Renderer::Vkd3dProton),
        &["NVIDIA"],
    );
    let active = |n: &str| r.find(n).unwrap().active.len();
    // The winner and the one it beats, both with their source.
    assert_eq!(active("PROTON_SET_GAME_DRIVE"), 2);
    // A flag or a key shows the line that holds it.
    assert_eq!(active("nodxr"), 1);
    assert_eq!(active("single_queue"), 1);
    assert_eq!(active("dxr"), 0, "not the tail of nodxr: whole words only");
    assert_eq!(active("dxgi.maxFrameRate"), 1);
    assert_eq!(active("dxgi.syncInterval"), 1);
    assert_eq!(active("dxgi.maxFrameLatency"), 0);
    // A key nobody knows still shows, marked as such.
    let own = r.find("MY_OWN_THING").unwrap();
    assert_eq!(own.component, Component::Other);
    assert_eq!(own.about, None);
    assert_eq!(own.in_build, InBuild::NotFound);
    assert!(matches!(own.applies, Applies::Unknown { .. }));
    // A launcher key is plumbing, and shows what it's set to.
    let g = r.find("GAMEID").unwrap();
    assert_eq!(g.component, Component::Plumbing);
    assert_eq!(g.active.len(), 1);
}

#[test]
fn a_parsed_default_fills_only_a_table_gap() {
    let t = tempfile::tempdir().unwrap();
    fake_build(t.path());
    let f = read_build(t.path());
    let r = report_of(vec![], Some(&f), Some(Renderer::Vkd3dProton), &["NVIDIA"]);
    // Table's words win: they know about the PROTON_LOG case.
    let log = r.find("DXVK_LOG_LEVEL").unwrap();
    assert_eq!(log.default_from, Some("table"));
    assert_eq!(log.default.as_deref(), Some("none (info with PROTON_LOG)"));
    // A name only the build knows: its parsed bits, and no words.
    let undoc = r.find("DXVK_UNDOCUMENTED").unwrap();
    assert_eq!(undoc.about, None);
    assert_eq!(undoc.component, Component::Dxvk);
    assert_eq!(undoc.in_build, InBuild::Found);
    assert_eq!(undescribed_names(&r), ["DXVK_UNDOCUMENTED", "A_STRAY"]);
    assert_eq!(r.undescribed(), 2);
}

fn undescribed_names(r: &CatalogueReport) -> Vec<&str> {
    r.entries
        .iter()
        .filter(|e| e.about.is_none() && e.component != Component::Plumbing)
        .filter(|e| e.in_build == InBuild::Found)
        .map(|e| e.name.as_str())
        .collect()
}

#[test]
fn find_ignores_case_and_similar_offers_names() {
    let r = report_of(vec![], Some(&full_facts()), Some(Renderer::Dxvk), &["AMD"]);
    assert_eq!(r.find("proton_log").unwrap().name, "PROTON_LOG");
    assert_eq!(
        r.find("DXGI.MAXFRAMERATE").unwrap().name,
        "dxgi.maxFrameRate"
    );
    assert!(r.find("NO_SUCH").is_none());
    assert!(r.similar("dlss_up").contains(&"PROTON_DLSS_UPGRADE"));
}

#[test]
fn a_build_other_than_the_reviewed_one_is_said() {
    let mut f = full_facts();
    f.version = Some("GE-Proton99-1".into());
    let r = report_of(vec![], Some(&f), Some(Renderer::Dxvk), &["AMD"]);
    assert!(r.warnings.iter().any(|w| w.contains("GE-Proton99-1")));
    let r = report_of(vec![], None, Some(Renderer::Dxvk), &["AMD"]);
    assert!(r.warnings.iter().any(|w| w.contains("wasn't found")));
    assert_eq!(r.find("PROTON_LOG").unwrap().in_build, InBuild::NotChecked);
    assert!(r.build.is_none());
}

#[test]
fn set_with_names_the_right_variable_or_refuses() {
    let r = report_of(vec![], Some(&full_facts()), Some(Renderer::Dxvk), &["AMD"]);
    let w = |n: &str| r.find(n).unwrap().set_with("1");
    assert_eq!(
        w("PROTON_LOG").unwrap(),
        "gogo linux-app env set 1 PROTON_LOG=<value>"
    );
    assert_eq!(
        w("nodxr").unwrap(),
        "gogo linux-app env set 1 VKD3D_CONFIG=nodxr (a comma-separated list: keep flags already there)"
    );
    assert_eq!(
        w("dxgi.maxFrameRate").unwrap(),
        "gogo linux-app env set 1 DXVK_CONFIG='dxgi.maxFrameRate=<value>' (`;` between keys: keep keys already there)"
    );
    assert_eq!(
        w("gamescope").unwrap(),
        "gogo linux-app wrapper 1 -- gamescope <options> -- (it replaces any wrapper set before)"
    );
    assert_eq!(
        w("gamemoderun").unwrap(),
        "gogo linux-app wrapper 1 -- gamemoderun (it replaces any wrapper set before)"
    );
    let g = report_of(
        vec![line(
            "WINEPREFIX",
            "/x",
            EnvSource::Launcher,
            EnvState::Applies,
        )],
        None,
        None,
        &[],
    );
    assert!(g.find("WINEPREFIX").unwrap().set_with("1").is_err());
}

// — the table against the build (design 18 §7.7, "staying current") —

/// `<kind> <name>` lines: `switch` needs an entry, `present` is anything a
/// table entry may be found as.
fn frozen() -> (String, BTreeSet<String>, BTreeSet<String>) {
    let path = format!("{FIXTURE}{REVIEWED_FOR}.names");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let (mut version, mut switches, mut present) =
        (String::new(), BTreeSet::new(), BTreeSet::new());
    for l in text.lines().filter(|l| !l.starts_with('#')) {
        match l.split_once(' ') {
            Some(("version", v)) => version = v.into(),
            Some(("switch", n)) => {
                switches.insert(n.to_string());
            }
            Some(("present", n)) => {
                present.insert(n.to_string());
            }
            _ => panic!("{path}: {l:?}"),
        }
    }
    (version, switches, present)
}

/// The fixture's text, from what a real build shows.
fn frozen_text(f: &BuildFacts) -> String {
    let mut out = String::from(
        "# What the build shows, frozen: names only. Regenerate on a GE-Proton bump:\n\
         # GOGO_CATALOGUE_BUILD=<build dir> GOGO_CATALOGUE_BLESS=1 cargo nextest run frozen_names\n",
    );
    out.push_str(&format!("version {}\n", f.version.as_deref().unwrap_or("")));
    for n in f.switch_names() {
        out.push_str(&format!("switch {n}\n"));
    }
    let mut present: BTreeSet<&str> = f.found.keys().map(String::as_str).collect();
    present.extend(f.script.keys().map(String::as_str));
    present.extend(f.sample.iter().map(String::as_str));
    for n in present {
        out.push_str(&format!("present {n}\n"));
    }
    out
}

/// Bumping the pin in `catalog.rs` fails here until the table is reviewed
/// against the new build and the fixture regenerated.
#[test]
fn the_table_was_reviewed_for_the_pinned_build() {
    let ge = catalog::find(GE_PROTON).unwrap();
    let Some(Discovery::Pinned(pin)) = &ge.discovery else {
        panic!("pinned");
    };
    assert_eq!(
        pin.version, REVIEWED_FOR,
        "review the table for the new pin"
    );
    assert_eq!(frozen().0, REVIEWED_FOR);
}

#[test]
fn every_name_the_build_reads_has_a_table_entry() {
    let (_, switches, _) = frozen();
    let known: BTreeSet<&str> = TABLE.iter().map(|s| s.name).collect();
    let missing: Vec<&String> = switches
        .iter()
        .filter(|n| !known.contains(n.as_str()) && !is_plumbing(n))
        .collect();
    assert!(
        missing.is_empty(),
        "{REVIEWED_FOR} reads these; add them to catalogue/table.rs (or PLUMBING): {missing:#?}"
    );
}

#[test]
fn every_table_entry_is_in_the_build_or_marked_absent() {
    let (_, _, present) = frozen();
    let mut stale = Vec::new();
    let mut wrongly_absent = Vec::new();
    for s in TABLE {
        if s.component == Component::Wrappers {
            continue;
        }
        let found = present.contains(s.probe.unwrap_or(s.name));
        if !found && !s.absent {
            stale.push(s.name);
        }
        if found && s.absent {
            wrongly_absent.push(s.name);
        }
    }
    let stale_plumbing: Vec<&&str> = table::PLUMBING
        .iter()
        .filter(|n| !present.contains(**n))
        .collect();
    assert!(
        stale_plumbing.is_empty(),
        "PLUMBING names {REVIEWED_FOR} doesn't have: {stale_plumbing:#?}"
    );
    assert!(
        stale.is_empty(),
        "no trace in {REVIEWED_FOR}; mark absent or remove: {stale:#?}"
    );
    assert!(
        wrongly_absent.is_empty(),
        "marked absent but {REVIEWED_FOR} has them: {wrongly_absent:#?}"
    );
}

/// Run against a real build with `GOGO_CATALOGUE_BUILD=<dir>`: the fixture
/// must match it. With `GOGO_CATALOGUE_BLESS=1` it rewrites the fixture.
/// Passes unset (CI has no build).
#[test]
fn frozen_names_match_a_real_build() {
    let Some(dir) = std::env::var_os("GOGO_CATALOGUE_BUILD") else {
        return;
    };
    let f = read_build(&PathBuf::from(dir));
    let v = f.version.clone().unwrap_or_default();
    let text = frozen_text(&f);
    let path = format!("{FIXTURE}{v}.names");
    if std::env::var_os("GOGO_CATALOGUE_BLESS").is_some() {
        std::fs::create_dir_all(FIXTURE).unwrap();
        std::fs::write(&path, &text).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(text, want, "regenerate {path}: see its header");
}

#[test]
fn the_table_is_well_formed() {
    let mut seen = BTreeSet::new();
    for s in TABLE {
        assert!(seen.insert(s.name), "{} twice", s.name);
        assert!(!is_plumbing(s.name), "{} is both", s.name);
        assert!(!s.about.is_empty(), "{}", s.name);
        assert!(
            !s.about.contains('\n') && s.about.len() <= 110,
            "{}: one line, {} bytes",
            s.name,
            s.about.len()
        );
        match s.kind {
            Kind::Env => assert!(
                envargs::check_key(s.name).is_ok(),
                "{}: not an env name",
                s.name
            ),
            Kind::Vkd3dFlag => assert_eq!(s.component, Component::Vkd3dProton),
            Kind::DxvkConf => assert_eq!(s.component, Component::Dxvk),
            Kind::Command => assert!(s.needs.is_some(), "{}", s.name),
        }
    }
}

/// The owner's flagship (#232): DLSS, ray tracing, Reflex must stay covered,
/// on the renderer and GPU they're for.
#[test]
fn the_flagship_switches_stay_covered() {
    let r = report_of(
        vec![],
        Some(&full_facts()),
        Some(Renderer::Vkd3dProton),
        &["NVIDIA"],
    );
    for name in [
        "PROTON_DISABLE_NVAPI",
        "PROTON_DLSS_UPGRADE",
        "PROTON_DLSS_INDICATOR",
        "PROTON_STREAMLINE_UPGRADE",
        "DXVK_ENABLE_NVAPI",
        "DXVK_NVAPI_DRS_SETTINGS",
        "DXVK_NVAPI_DRS_NGX_DLSS_SR_OVERRIDE",
        "DXVK_NVAPI_DRS_NGX_DLSS_SR_OVERRIDE_RENDER_PRESET_SELECTION",
        "DXVK_NVAPI_DRS_NGX_DLSS_RR_OVERRIDE",
        "DXVK_NVAPI_DRS_NGX_DLSS_FG_OVERRIDE",
        "DXVK_NVAPI_LOG_LEVEL",
        "VKD3D_CONFIG",
        "dxr",
        "nodxr",
        "dxr12",
    ] {
        let e = r.find(name).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(e.applies, Applies::Yes, "{name}");
        assert!(e.about.is_some(), "{name}");
    }
    // Reflex on D3D12 needs no layer, and the catalogue must not suggest one.
    assert!(matches!(
        r.find("DXVK_NVAPI_VKREFLEX").unwrap().applies,
        Applies::No { .. }
    ));
    // They sort ahead of the rest of their component.
    let proton: Vec<&Entry> = r
        .entries
        .iter()
        .filter(|e| e.component == Component::Proton)
        .collect();
    assert_eq!(proton[0].topic, Some(Topic::Nvidia));
}
