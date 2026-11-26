//! The Default skin fixture's routes as display lists (design 24 §8 PR C3, design 28 §7
//! "Structure"): op counts and kinds are a snapshot, so a lowering change that moves few
//! pixels still shows up. Bless a deliberate change with `GOGO_BLESS=1`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use gogo_gui_testkit::FixtureScope;
use gogo_skin::{Binding, Bound, Mode, SkinPackage, UiState};
use gogo_ui_layout::{Scroll, Viewport, layout};
use gogo_ui_scene::{DisplayList, Op, SceneState, build};
use gogo_ui_text::FontSet;

fn bind(route: &str) -> Bound {
    let d = gogo_skins::default();
    let pkg = SkinPackage::from_files(d.name, d.to_files()).unwrap_or_else(|r| panic!("{r}"));
    let scope = FixtureScope::new();
    let ui = UiState::defaults(&pkg.manifest);
    let b = Binding {
        scope: &scope,
        ui: &ui,
        mode: Mode::Dark,
        default: None,
    };
    pkg.bind(route, &b)
        .unwrap_or_else(|e| panic!("{route}: {e:?}"))
}

fn list(route: &str) -> DisplayList {
    let bound = bind(route);
    let laid = layout(
        &bound.tree,
        &mut FontSet::bundled(),
        Viewport::new(1280.0, 800.0),
        &Scroll::none(),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    build(&bound.tree, &laid, &SceneState::new())
}

/// The counts, then the kinds in order, runs of one kind folded: `Fill x3 Glyphs x2 Pop`.
fn snapshot(l: &DisplayList) -> String {
    let mut out = l.summary().to_string();
    out.push('\n');
    let mut run: Option<(&'static str, usize)> = None;
    let mut line = String::new();
    let flush = |run: Option<(&'static str, usize)>, line: &mut String| {
        if let Some((k, n)) = run {
            if !line.is_empty() {
                line.push(' ');
            }
            if n == 1 {
                line.push_str(k);
            } else {
                let _ = write!(line, "{k}x{n}");
            }
        }
    };
    for op in &l.ops {
        let k = op.kind();
        match &mut run {
            Some((rk, n)) if *rk == k => *n += 1,
            _ => {
                flush(run.take(), &mut line);
                run = Some((k, 1));
            }
        }
    }
    flush(run.take(), &mut line);
    out.push_str(&line);
    out.push('\n');
    out
}

fn check(route: &str) {
    let l = list(route);
    let got = snapshot(&l);
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("default-{route}.txt"));
    if std::env::var_os("GOGO_BLESS").is_some() {
        std::fs::write(&path, &got).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(
        got, want,
        "{route}: rerun with GOGO_BLESS=1 if the change is intended"
    );
}

#[test]
fn the_game_route_lowers_to_the_snapshot() {
    check("game");
}

#[test]
fn the_shell_route_lowers_to_the_snapshot() {
    check("shell");
}

#[test]
fn lists_are_balanced_and_use_only_images_they_declare() {
    for route in ["game", "shell"] {
        let l = list(route);
        let mut depth = 0i32;
        for op in &l.ops {
            match op {
                Op::PushClip { .. } | Op::PushOpacity { .. } | Op::PushFilter { .. } => depth += 1,
                Op::Pop => depth -= 1,
                Op::Image { id, .. } => assert!(l.image(*id).is_some(), "{route}"),
                _ => {}
            }
            assert!(depth >= 0, "{route}: a Pop without a Push");
        }
        assert_eq!(depth, 0, "{route}");
    }
}

#[test]
fn the_loader_and_the_scene_know_the_same_icons() {
    let mut a: Vec<&str> = gogo_skin::icons::ICON_NAMES.to_vec();
    let mut b: Vec<&str> = gogo_ui_scene::icons::NAMES.to_vec();
    a.sort_unstable();
    b.sort_unstable();
    assert_eq!(
        a, b,
        "the skin loader accepts exactly the icons the scene draws"
    );
}
