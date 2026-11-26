//! The Default skin fixture's `game` and `shell` routes through layout (design 24 §8 PR C2):
//! every node is placed, boxes stay sane, and a few text-driven sizes are pinned so a font
//! or measure change shows up.

use std::path::Path;

use gogo_gui_testkit::FixtureScope;
use gogo_skin::{Binding, Bound, Mode, SkinPackage, UiState};
use gogo_ui_layout::{Laid, Scroll, Viewport, layout};
use gogo_ui_text::FontSet;

fn bind(route: &str) -> Bound {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../gogo-skin/tests/fixtures/default");
    let pkg = SkinPackage::load(&dir).unwrap_or_else(|r| panic!("{r}"));
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

fn lay(bound: &Bound, w: f32, h: f32) -> Laid {
    layout(
        &bound.tree,
        &mut FontSet::bundled(),
        Viewport::new(w, h),
        &Scroll::none(),
    )
    .unwrap_or_else(|e| panic!("{e}"))
}

fn size_of(bound: &Bound, laid: &Laid, name: &str) -> (f64, f64) {
    let id = bound
        .tree
        .by_name(name)
        .unwrap_or_else(|| panic!("no node `{name}`"));
    let r = laid
        .rect(id)
        .unwrap_or_else(|| panic!("`{name}` not placed"));
    (r.width(), r.height())
}

#[test]
fn every_node_of_both_routes_is_placed_with_a_sane_box() {
    for route in ["game", "shell"] {
        let bound = bind(route);
        let laid = lay(&bound, 1280.0, 800.0);
        assert_eq!(laid.order().len(), bound.tree.len(), "{route}");
        for id in bound.tree.iter() {
            let r = laid.rect(id).expect("placed");
            let key = bound.tree.key(id).unwrap_or("?");
            assert!(
                r.width() >= 0.0 && r.height() >= 0.0,
                "{route} {key}: {r:?}"
            );
            assert!(
                r.x0.is_finite() && r.y0.is_finite() && r.x1.is_finite() && r.y1.is_finite(),
                "{route} {key}"
            );
            if let Some(t) = laid.text(id) {
                assert!(
                    t.width() <= r.width() as f32 + 1.0,
                    "{route} {key}: text wider than box"
                );
            }
        }
        let root = laid.rect(bound.tree.root()).expect("root");
        assert_eq!((root.width(), root.height()), (1280.0, 800.0));
    }
}

#[test]
fn text_driven_sizes_of_the_game_route_are_pinned() {
    let bound = bind("game");
    let laid = lay(&bound, 1153.0, 1200.0);
    // Content width 1153: the same text must measure the same with the bundled fonts.
    assert_eq!(size_of(&bound, &laid, "crumb"), (1153.0, 18.0));
    assert_eq!(size_of(&bound, &laid, "cover"), (132.0, 176.0));
    assert_eq!(size_of(&bound, &laid, "title"), (997.0, 46.0));
    assert_eq!(size_of(&bound, &laid, "play"), (128.0, 40.0));
    assert_eq!(size_of(&bound, &laid, "verify"), (106.0, 40.0));
    assert_eq!(size_of(&bound, &laid, "files"), (135.0, 40.0));
    assert_eq!(size_of(&bound, &laid, "builds"), (713.0, 253.0));
    assert_eq!(size_of(&bound, &laid, "runs-with"), (420.0, 261.0));
}

#[test]
fn the_same_route_at_two_widths_reflows_without_overlap_in_the_title_row() {
    let bound = bind("game");
    let wide = lay(&bound, 1400.0, 1200.0);
    let narrow = lay(&bound, 900.0, 1200.0);
    let (ww, _) = size_of(&bound, &wide, "title");
    let (nw, _) = size_of(&bound, &narrow, "title");
    assert!(nw < ww, "{nw} {ww}");
    let id = bound.tree.by_name("play").expect("play");
    let r = narrow.rect(id).expect("rect");
    assert_eq!(narrow.hit(r.x0 + 1.0, r.y0 + 1.0), Some(id));
}
