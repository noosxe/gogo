//! L1 winetricks verbs from the game's imports (#72, design 16 decision 5).
//!
//! The PE scan (#63) reports the DirectX helper DLLs a game imports and
//! doesn't ship (`helpers`); each maps to the winetricks verb that installs
//! Microsoft's native copy. Wine's builtins for these are incomplete
//! (Rebel Galaxy's D3D11 shaders fail in vkd3d-shader without native
//! `d3dcompiler_43` + `d3dx11_43`, docs/19 §5).
//!
//! Only static imports count, never `runtime_loaded`: Unity names every
//! xinput version as a string just to probe for one. DLLs that ship with
//! Windows itself (`xinput1_4`, `xinput9_1_0`, `xaudio2_8`/`2_9`) are
//! Wine builtins and get no verb.
//!
//! The build bakes these into `gogo-launch.toml` `verbs` (so they're part
//! of the config stamp); setup applies L3 then L4 to them ([`effective`]):
//! their verbs add to these, `-<verb>` / `-*` remove (#128).

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::compat::{CompatEntry, Layer, Sourced};
use super::pe::{Bitness, PeScan};

/// One auto verb and the imports that call for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutoVerb {
    pub verb: String,
    pub imports: Vec<String>,
}

/// The winetricks verb for one imported helper DLL (lowercase name), or
/// `None` when Wine's builtin is the Windows one or winetricks has no verb.
/// `x64` picks `xact_x64` for 64-bit XACT importers (`xact` is 32-bit only;
/// the d3dx/d3dcompiler/xinput verbs install both bitnesses).
pub fn verb_for(dll: &str, x64: bool) -> Option<&'static str> {
    let stem = dll.strip_suffix(".dll")?;
    // Families with one verb per DLL version that winetricks knows.
    const PER_VERSION: &[&str] = &[
        "d3dcompiler_42",
        "d3dcompiler_43",
        "d3dcompiler_46",
        "d3dcompiler_47",
        "d3dx10_43",
        "d3dx11_42",
        "d3dx11_43",
        "d3dcsx_46",
        "d3dcsx_47",
    ];
    if let Some(v) = PER_VERSION.iter().find(|v| **v == stem) {
        return Some(v);
    }
    if let Some(n) = stem.strip_prefix("d3dx9_") {
        return D3DX9.iter().find(|v| v[6..] == *n).copied();
    }
    if stem.starts_with("d3dx10") {
        return Some("d3dx10"); // every d3dx10_33…43
    }
    match stem {
        "xinput1_1" | "xinput1_2" | "xinput1_3" => Some("xinput"),
        // xactengine2_*/3_*, x3daudio1_*, xapofx1_*, and the redist
        // xaudio2_0…2_7 (2_8/2_9 ship with Windows: builtin).
        s if s.starts_with("xactengine")
            || s.starts_with("x3daudio")
            || s.starts_with("xapofx")
            || s.strip_prefix("xaudio2_")
                .and_then(|n| n.parse::<u8>().ok())
                .is_some_and(|n| n <= 7) =>
        {
            Some(if x64 { "xact_x64" } else { "xact" })
        }
        _ => None,
    }
}

/// winetricks' per-version d3dx9 verbs (`d3dx9_24` … `d3dx9_43`).
const D3DX9: &[&str] = &[
    "d3dx9_24", "d3dx9_25", "d3dx9_26", "d3dx9_27", "d3dx9_28", "d3dx9_29", "d3dx9_30", "d3dx9_31",
    "d3dx9_32", "d3dx9_33", "d3dx9_34", "d3dx9_35", "d3dx9_36", "d3dx9_37", "d3dx9_38", "d3dx9_39",
    "d3dx9_40", "d3dx9_41", "d3dx9_42", "d3dx9_43",
];

/// The auto verbs for a scanned game, sorted by verb.
pub fn for_scan(scan: &PeScan) -> Vec<AutoVerb> {
    // 64-bit importers of an XACT DLL need `xact_x64` as well.
    let x64_imports: BTreeSet<&str> = scan
        .files
        .iter()
        .filter(|f| f.bitness == Bitness::X86_64)
        .flat_map(|f| f.imports.iter().map(String::as_str))
        .collect();
    let mut out: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for dll in &scan.helpers {
        let x86 = scan
            .files
            .iter()
            .any(|f| f.bitness != Bitness::X86_64 && f.imports.contains(dll));
        let mut push = |x64| {
            if let Some(v) = verb_for(dll, x64) {
                let imports = out.entry(v).or_default();
                if !imports.contains(dll) {
                    imports.push(dll.clone());
                }
            }
        };
        if x86 || !x64_imports.contains(dll.as_str()) {
            push(false);
        }
        if x64_imports.contains(dll.as_str()) {
            push(true);
        }
    }
    out.into_iter()
        .map(|(verb, imports)| AutoVerb {
            verb: verb.to_string(),
            imports,
        })
        .collect()
}

/// Just the verb names, in order.
pub fn names(auto: &[AutoVerb]) -> Vec<String> {
    auto.iter().map(|a| a.verb.clone()).collect()
}

/// The verbs setup applies, each with the layer that added it: the bundle's
/// baked L1 list, then L3's and L4's edits to it (decision 5, #128: their
/// verbs add, `-<verb>` removes one, `-*` all; [`compat::merge`]).
///
/// [`compat::merge`]: super::compat::merge
pub fn effective(baked: &[String], db: &CompatEntry, l4: &CompatEntry) -> Vec<Sourced<String>> {
    let auto = CompatEntry {
        verbs: Some(baked.to_vec()),
        ..Default::default()
    };
    super::compat::merge(&[(Layer::Auto, &auto), (Layer::Db, db), (Layer::User, l4)]).verbs
}

/// [`effective`] for a planned build: its L1 verbs under `gogid`'s L3 and L4.
pub fn for_plan(gogid: Option<&str>, auto: &[AutoVerb]) -> Vec<Sourced<String>> {
    let (db, l4) = match gogid {
        Some(id) => (
            super::compat::db_entry(id).entry,
            super::compat::load_user(id)
                .map(|p| p.entry)
                .unwrap_or_default(),
        ),
        None => Default::default(),
    };
    effective(&names(auto), &db, &l4)
}

/// The verbs a built game's bundle baked (its L1 list), read through
/// `state.toml`; `None` when the game hasn't been built.
pub fn baked(gogid: &str) -> Option<Vec<String>> {
    let dirs = super::launch::GameDirs::for_game(gogid).ok()?;
    let state = super::launch::GameState::load(&dirs.state_toml).ok()??;
    let path = state.bundle.join("Contents/Resources/gogo-launch.toml");
    let raw = std::fs::read_to_string(&path).ok()?;
    Some(super::launch::LaunchToml::parse(&raw, &path).ok()?.verbs)
}

/// The pre-confirm summary's `verbs:` value: what setup will apply, each
/// verb with its layer, and the imports behind the L1 ones. `auto` is `None`
/// when the tree isn't there yet (the installer lane).
pub fn summary(gogid: Option<&str>, auto: Option<&[AutoVerb]>) -> String {
    let Some(auto) = auto else {
        return "from the game's imports, once installed".into();
    };
    describe(&for_plan(gogid, auto), auto)
}

fn describe(eff: &[Sourced<String>], auto: &[AutoVerb]) -> String {
    if eff.is_empty() {
        return "none".into();
    }
    eff.iter()
        .map(|s| match auto.iter().find(|a| a.verb == s.value) {
            Some(a) if s.layer == Layer::Auto => {
                format!(
                    "{} ({}: {})",
                    s.value,
                    s.layer.describe(),
                    a.imports.join(", ")
                )
            }
            _ => format!("{} ({})", s.value, s.layer.describe()),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macapp::pe::PeFileScan;

    #[test]
    fn helper_dlls_map_to_winetricks_verbs() {
        let cases = [
            ("d3dcompiler_43.dll", Some("d3dcompiler_43")),
            ("d3dcompiler_47.dll", Some("d3dcompiler_47")),
            ("d3dcompiler_39.dll", None), // no winetricks verb
            ("d3dx9_43.dll", Some("d3dx9_43")),
            ("d3dx9_24.dll", Some("d3dx9_24")),
            ("d3dx9_99.dll", None),
            ("d3dx10_43.dll", Some("d3dx10_43")),
            ("d3dx10_40.dll", Some("d3dx10")),
            ("d3dx11_43.dll", Some("d3dx11_43")),
            ("d3dcsx_47.dll", Some("d3dcsx_47")),
            ("xinput1_3.dll", Some("xinput")),
            ("xinput1_1.dll", Some("xinput")),
            ("xinput1_4.dll", None),   // ships with Windows
            ("xinput9_1_0.dll", None), // ships with Windows
            ("xactengine3_7.dll", Some("xact")),
            ("x3daudio1_7.dll", Some("xact")),
            ("xapofx1_5.dll", Some("xact")),
            ("xaudio2_7.dll", Some("xact")),
            ("xaudio2_8.dll", None),
            ("xaudio2_9.dll", None),
            ("d3d11.dll", None),
        ];
        for (dll, want) in cases {
            assert_eq!(verb_for(dll, false), want, "{dll}");
        }
        assert_eq!(verb_for("xactengine3_7.dll", true), Some("xact_x64"));
        assert_eq!(verb_for("d3dx11_43.dll", true), Some("d3dx11_43"));
    }

    fn file(path: &str, bitness: Bitness, imports: &[&str], runtime: &[&str]) -> PeFileScan {
        serde_json::from_value(serde_json::json!({
            "path": path,
            "bitness": bitness,
            "imports": imports,
            "runtime_loaded": runtime,
            "d3d7_iids": false,
        }))
        .unwrap()
    }

    fn scan(files: Vec<PeFileScan>, helpers: &[&str], runtime_loaded: &[&str]) -> PeScan {
        let set = |v: &[&str]| v.iter().map(|s| s.to_string()).collect();
        PeScan {
            root: "/game".into(),
            main_exe: "game.exe".into(),
            main_exe_source: crate::macapp::pe::MainExeSource::Heuristic,
            bitness: Bitness::I386,
            engine: None,
            graphics: crate::macapp::pe::GraphicsApi::D3d11,
            graphics_source: crate::macapp::pe::GraphicsSource::Imports,
            graphics_imports: BTreeSet::new(),
            runtime_graphics: BTreeSet::new(),
            helpers: set(helpers),
            runtime_loaded: set(runtime_loaded),
            local_overrides: BTreeSet::new(),
            files,
        }
    }

    /// Rebel Galaxy (1435582019, 32-bit D3D11 via Ogre), as #63 scanned it.
    #[test]
    fn rebel_galaxy_gets_d3dcompiler_d3dx_and_xinput() {
        let s = scan(
            vec![
                file(
                    "RenderSystem_Direct3D11.dll",
                    Bitness::I386,
                    &["d3d11.dll", "d3dcompiler_43.dll", "d3dx11_43.dll"],
                    &[],
                ),
                file(
                    "RenderSystem_Direct3D9.dll",
                    Bitness::I386,
                    &["d3d9.dll", "d3dx9_43.dll"],
                    &[],
                ),
                file("RebelGalaxyGOG.exe", Bitness::I386, &["xinput1_3.dll"], &[]),
            ],
            &[
                "d3dcompiler_43.dll",
                "d3dx11_43.dll",
                "d3dx9_43.dll",
                "xinput1_3.dll",
            ],
            &[],
        );
        assert_eq!(
            names(&for_scan(&s)),
            ["d3dcompiler_43", "d3dx11_43", "d3dx9_43", "xinput"]
        );
    }

    /// Runtime-only names (Unity's xinput probing) never become verbs; nor
    /// do imports that map to nothing.
    #[test]
    fn runtime_loaded_names_are_ignored() {
        let s = scan(
            vec![file(
                "UnityPlayer.dll",
                Bitness::X86_64,
                &["xinput9_1_0.dll"],
                &["xinput1_3.dll", "xinput1_4.dll", "d3dcompiler_47.dll"],
            )],
            &["xinput9_1_0.dll"],
            &["xinput1_3.dll", "xinput1_4.dll", "d3dcompiler_47.dll"],
        );
        assert!(for_scan(&s).is_empty());
    }

    #[test]
    fn xact_picks_the_importers_bitness() {
        let s = scan(
            vec![
                file("a32.exe", Bitness::I386, &["xactengine3_7.dll"], &[]),
                file(
                    "a64.exe",
                    Bitness::X86_64,
                    &["xactengine3_7.dll", "x3daudio1_7.dll"],
                    &[],
                ),
            ],
            &["xactengine3_7.dll", "x3daudio1_7.dll"],
            &[],
        );
        let auto = for_scan(&s);
        assert_eq!(names(&auto), ["xact", "xact_x64"]);
        assert_eq!(auto[0].imports, ["xactengine3_7.dll"]);
        assert_eq!(auto[1].imports, ["x3daudio1_7.dll", "xactengine3_7.dll"]);
    }

    fn verbs(v: &[&str]) -> CompatEntry {
        CompatEntry {
            verbs: Some(v.iter().map(|s| s.to_string()).collect()),
            ..Default::default()
        }
    }

    fn pairs(eff: &[Sourced<String>]) -> Vec<(&str, Layer)> {
        eff.iter().map(|s| (s.value.as_str(), s.layer)).collect()
    }

    /// #128: L3 and L4 verbs add to L1's, in order, without duplicates.
    #[test]
    fn l3_and_l4_verbs_add_to_auto_verbs() {
        let baked = vec!["d3dcompiler_43".to_string(), "d3dx11_43".into()];
        let none = CompatEntry::default();
        assert_eq!(
            pairs(&effective(&baked, &none, &none)),
            [("d3dcompiler_43", Layer::Auto), ("d3dx11_43", Layer::Auto)]
        );
        // L3 adds; a verb L1 already has keeps L1's place and layer.
        let db = verbs(&["directplay", "d3dx11_43", "directplay"]);
        assert_eq!(
            pairs(&effective(&baked, &db, &none)),
            [
                ("d3dcompiler_43", Layer::Auto),
                ("d3dx11_43", Layer::Auto),
                ("directplay", Layer::Db),
            ]
        );
        // L4 adds after L3.
        let l4 = verbs(&["xinput", "directplay"]);
        assert_eq!(
            pairs(&effective(&baked, &db, &l4)),
            [
                ("d3dcompiler_43", Layer::Auto),
                ("d3dx11_43", Layer::Auto),
                ("directplay", Layer::Db),
                ("xinput", Layer::User),
            ]
        );
        // An empty list has nothing to add.
        assert_eq!(effective(&baked, &db, &verbs(&[])).len(), 3);
    }

    /// #128: `-<verb>` drops one verb of any lower layer, `-*` all of them;
    /// entries apply in order, so `-*` then names replaces the list whole.
    #[test]
    fn l4_removes_or_clears_verbs() {
        let baked = vec!["d3dcompiler_43".to_string(), "d3dx11_43".into()];
        let db = verbs(&["directplay"]);
        let e = effective(&baked, &db, &verbs(&["-d3dx11_43", "-directplay", "-xact"]));
        assert_eq!(pairs(&e), [("d3dcompiler_43", Layer::Auto)]);
        assert!(effective(&baked, &db, &verbs(&["-*"])).is_empty());
        let e = effective(&baked, &db, &verbs(&["-*", "xinput"]));
        assert_eq!(pairs(&e), [("xinput", Layer::User)]);
        // Removed then added back: it moves to the end, under the new layer.
        let e = effective(&baked, &db, &verbs(&["-d3dcompiler_43", "d3dcompiler_43"]));
        assert_eq!(
            pairs(&e),
            [
                ("d3dx11_43", Layer::Auto),
                ("directplay", Layer::Db),
                ("d3dcompiler_43", Layer::User),
            ]
        );
        // L3 can remove an L1 verb too, and L4 can add it back.
        let db = verbs(&["-d3dx11_43"]);
        let e = effective(&baked, &db, &CompatEntry::default());
        assert_eq!(pairs(&e), [("d3dcompiler_43", Layer::Auto)]);
        let e = effective(&baked, &db, &verbs(&["d3dx11_43"]));
        assert_eq!(
            pairs(&e),
            [("d3dcompiler_43", Layer::Auto), ("d3dx11_43", Layer::User)]
        );
    }

    #[test]
    fn summary_names_the_imports_and_the_layer() {
        let auto = [AutoVerb {
            verb: "d3dcompiler_43".into(),
            imports: vec!["d3dcompiler_43.dll".into()],
        }];
        assert_eq!(
            summary(None, Some(&auto)),
            "d3dcompiler_43 (L1 auto: d3dcompiler_43.dll)"
        );
        assert_eq!(summary(None, Some(&[])), "none");
        assert!(summary(None, None).contains("once installed"));
        let eff = effective(&names(&auto), &verbs(&["directplay"]), &verbs(&["xinput"]));
        assert_eq!(
            describe(&eff, &auto),
            "d3dcompiler_43 (L1 auto: d3dcompiler_43.dll), directplay (L3 compat DB), \
             xinput (L4 user override)"
        );
    }
}
