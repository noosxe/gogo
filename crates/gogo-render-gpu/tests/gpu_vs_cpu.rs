//! vello_gpu against vello_cpu, the reference, on the test scenes (design 28 §7, 08 §7).
//!
//! Everything renders offscreen. Without a GPU adapter or device the tests skip; with
//! `GOGO_GPU_REQUIRED=1` (the CI lavapipe job) that, or an adapter that is not a software
//! one, fails them instead.
//!
//! The comparisons with vello_cpu run **only on a software adapter**, where their
//! tolerance is measured and the result does not depend on a driver. `GOGO_GPU_ANY_ADAPTER=1`
//! runs them on whatever adapter there is, for a local or host pass.

use std::path::Path;

use gogo_gui_testkit::env_flag;
use gogo_gui_testkit::golden::{self, Tolerance};
use gogo_gui_testkit::scenes::{self, Scene};
use gogo_render_cpu::CpuRenderer;
use gogo_render_gpu::{GpuError, GpuRenderer};
use gogo_ui_scene::{Brush, MemoryStore, Op, Renderer, Shape};
use vello_gpu::kurbo::{Point, Rect};
use vello_gpu::peniko::Gradient;
use vello_gpu::peniko::color::palette::css;

/// How far the GPU render may be from the CPU one: more than 4 levels on at most 0.5% of
/// the pixels, none above 8, mean at most 0.35.
///
/// Measured on lavapipe (Mesa 26.2.6): the largest difference is 4 levels (the effects
/// sheet; 2 on the routes), the largest mean 0.154. A discrete GPU measured the same
/// largest difference and a mean of 0.170. `max` and `mean` are about twice that.
const TOLERANCE: Tolerance = Tolerance {
    level: 4,
    share: 0.005,
    max: 8,
    mean: 0.35,
};

fn gpu() -> Option<GpuRenderer> {
    let required = env_flag("GOGO_GPU_REQUIRED");
    match GpuRenderer::headless() {
        Ok(r) => {
            let info = r.adapter_info().cloned();
            println!("adapter: {info:?}");
            assert!(
                !required || is_software(&r),
                "GOGO_GPU_REQUIRED asks for a software adapter"
            );
            Some(r)
        }
        Err(e @ (GpuError::NoAdapter(_) | GpuError::NoDevice(_))) if !required => {
            println!("skipped: {e}");
            None
        }
        Err(e) => panic!("{e}"),
    }
}

fn is_software(gpu: &GpuRenderer) -> bool {
    gpu.adapter_info()
        .is_some_and(|i| i.device_type == wgpu::DeviceType::Cpu)
}

fn compare(scene: Scene) {
    compare_within(scene, &TOLERANCE);
}

fn compare_within(scene: Scene, tolerance: &Tolerance) {
    let Some(mut gpu) = gpu() else { return };
    if !is_software(&gpu) && !env_flag("GOGO_GPU_ANY_ADAPTER") {
        println!("skipped: not a software adapter (GOGO_GPU_ANY_ADAPTER=1 compares anyway)");
        return;
    }
    let mut cpu = CpuRenderer::new();
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("golden");
    let mut failures = Vec::new();
    for scale in [1u8, 2] {
        let name = format!("gpu-{}@{scale}x", scene.name);
        let scale = f64::from(scale);
        let want = cpu
            .render_to_rgba(&scene.list, &scene.images, scale)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let got = gpu
            .render_to_rgba(&scene.list, &scene.images, scale)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        match golden::compare(&out, &name, &want, &got, tolerance) {
            Ok(diff) => println!("{name}: {diff}"),
            Err(e) => failures.push(e.to_string()),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_effects_sheet_matches_the_cpu() {
    compare(scenes::effects_sheet().unwrap_or_else(|e| panic!("{e}")));
}

#[test]
fn the_default_game_route_matches_the_cpu() {
    compare(scenes::default_route("default-game", "game").unwrap_or_else(|e| panic!("{e}")));
}

#[test]
fn the_default_shell_route_matches_the_cpu() {
    compare(scenes::default_route("default-shell", "shell").unwrap_or_else(|e| panic!("{e}")));
}

/// Ops the scenes do not produce: a gradient on text, and radii in the thousands.
#[test]
fn gradient_text_and_huge_blurs_match_the_cpu() {
    let mut scene = scenes::effects_sheet().unwrap_or_else(|e| panic!("{e}"));
    let run = scene.list.ops.iter().find_map(|op| match op {
        Op::Glyphs { run, .. } => Some(run.clone()),
        _ => None,
    });
    let run = run.unwrap_or_else(|| panic!("the sheet has text"));
    (scene.list.width, scene.list.height) = (160.0, 96.0);
    let rect = Shape::Rect(Rect::new(8.0, 8.0, 56.0, 56.0));
    let gradient = Gradient::new_linear((60.0, 0.0), (120.0, 0.0))
        .with_stops([css::RED, css::BLUE].as_slice());
    scene.list.ops = vec![
        Op::PushFilter { sigma: 5000.0 },
        Op::Fill {
            shape: rect.clone(),
            brush: Brush::Solid(css::WHITE),
        },
        Op::Pop,
        Op::Backdrop {
            shape: rect,
            sigma: 5000.0,
        },
        Op::Glyphs {
            run,
            origin: Point::new(60.0, 40.0),
            brush: Brush::Gradient(gradient),
        },
    ];
    scene.name = "extra-ops";
    // Most of this picture is one blur of the widest allowed sigma, which the two renderers
    // round differently on nearly every pixel (at most 2 levels; mean 0.57 on a discrete
    // GPU at 2x, 0.15 on lavapipe).
    compare_within(
        scene,
        &Tolerance {
            mean: 1.0,
            ..TOLERANCE
        },
    );
}

#[test]
fn a_second_frame_from_the_cached_textures_is_the_same() {
    let Some(mut gpu) = gpu() else { return };
    let scene = scenes::effects_sheet().unwrap_or_else(|e| panic!("{e}"));
    let mut draw = || {
        gpu.render_to_rgba(&scene.list, &scene.images, 1.0)
            .unwrap_or_else(|e| panic!("{e}"))
    };
    assert_eq!(draw(), draw());
}

#[test]
fn images_the_store_lacks_are_skipped() {
    let Some(mut gpu) = gpu() else { return };
    let scene = scenes::effects_sheet().unwrap_or_else(|e| panic!("{e}"));
    gpu.render_to_rgba(&scene.list, &MemoryStore::new(), 1.0)
        .unwrap_or_else(|e| panic!("{e}"));
}

/// vello_gpu's own cap on intermediate textures is 4096 px.
#[test]
fn a_backdrop_wider_than_4096_px_is_drawn() {
    let Some(mut gpu) = gpu() else { return };
    let mut list = scenes::effects_sheet()
        .unwrap_or_else(|e| panic!("{e}"))
        .list;
    (list.width, list.height) = (4600.0, 64.0);
    list.images.clear();
    list.ops = vec![Op::Backdrop {
        shape: Shape::Rect(Rect::new(0.0, 0.0, 4600.0, 64.0)),
        sigma: 8.0,
    }];
    let image = gpu
        .render_to_rgba(&list, &MemoryStore::new(), 1.0)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!((image.width, image.height), (4600, 64));
}

#[test]
fn impossible_sizes_and_scales_are_errors() {
    let Some(mut gpu) = gpu() else { return };
    assert!(matches!(gpu.offscreen(0, 4), Err(GpuError::Size { .. })));
    assert!(matches!(
        gpu.offscreen(1 << 20, 4),
        Err(GpuError::Size { .. })
    ));
    let scene = scenes::effects_sheet().unwrap_or_else(|e| panic!("{e}"));
    let mut surface = gpu.offscreen(4, 4).unwrap_or_else(|e| panic!("{e}"));
    assert!(matches!(
        gpu.render(&scene.list, &scene.images, f64::NAN, &mut surface),
        Err(GpuError::Scale(_))
    ));
}
