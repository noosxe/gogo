//! Golden images of the test scenes on vello_cpu at 1x and 2x (design 28 §7, 08 §7). One
//! set of goldens serves every platform. Bless a deliberate change with `GOGO_BLESS=1`.

use std::path::Path;

use gogo_gui_testkit::golden::{self, GOLDEN_TOLERANCE};
use gogo_gui_testkit::scenes::{self, Scene};
use gogo_render_cpu::CpuRenderer;

fn check(scene: Scene) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens");
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("golden");
    let mut renderer = CpuRenderer::new();
    let mut failures = Vec::new();
    for scale in [1u8, 2] {
        let name = format!("{}@{scale}x", scene.name);
        let image = renderer
            .render_to_rgba(&scene.list, &scene.images, f64::from(scale))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        match golden::check(&dir, &out, &name, &image, &GOLDEN_TOLERANCE) {
            Ok(diff) => println!("{name}: {diff}"),
            Err(e) => failures.push(e.to_string()),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_effects_sheet_matches() {
    check(scenes::effects_sheet().unwrap_or_else(|e| panic!("{e}")));
}

#[test]
fn the_default_game_route_matches() {
    check(scenes::default_route("default-game", "game").unwrap_or_else(|e| panic!("{e}")));
}

#[test]
fn the_default_shell_route_matches() {
    check(scenes::default_route("default-shell", "shell").unwrap_or_else(|e| panic!("{e}")));
}

#[test]
fn the_default_library_matches() {
    check(
        scenes::library_scene("default-library", 200, "grid", 0.0)
            .unwrap_or_else(|e| panic!("{e}")),
    );
}

#[test]
fn the_default_library_scrolled_matches() {
    check(
        scenes::library_scene("default-library-scrolled", 200, "grid", 5000.0)
            .unwrap_or_else(|e| panic!("{e}")),
    );
}

#[test]
fn a_second_frame_of_one_renderer_is_the_same_as_the_first() {
    let scene = scenes::effects_sheet().unwrap_or_else(|e| panic!("{e}"));
    let mut renderer = CpuRenderer::new();
    let mut draw = || {
        renderer
            .render_to_rgba(&scene.list, &scene.images, 1.0)
            .unwrap_or_else(|e| panic!("{e}"))
    };
    assert_eq!(draw(), draw());
}
