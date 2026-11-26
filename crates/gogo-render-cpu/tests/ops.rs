//! Ops the test scenes do not produce.

use std::sync::Arc;

use gogo_gui_testkit::scenes;
use gogo_render_cpu::{CpuError, CpuRenderer, CpuSurface, MAX_FRAME_PIXELS};
use gogo_ui_scene::{Brush, DisplayList, Image, MemoryStore, Op, Renderer, Shape};
use vello_cpu::kurbo::{Point, Rect};
use vello_cpu::peniko::color::palette::css;
use vello_cpu::peniko::{Color, Gradient};

fn sheet() -> DisplayList {
    scenes::effects_sheet()
        .unwrap_or_else(|e| panic!("{e}"))
        .list
}

#[test]
fn a_gradient_on_text_is_placed_in_logical_px_not_from_the_text_origin() {
    let mut list = sheet();
    let run = list.ops.iter().find_map(|op| match op {
        Op::Glyphs { run, .. } => Some(Arc::clone(run)),
        _ => None,
    });
    let run = run.unwrap_or_else(|| panic!("the sheet has text"));
    // Red above y = 99, blue from y = 100 down, where the text is.
    let gradient = Gradient::new_linear((0.0, 99.0), (0.0, 100.0))
        .with_stops([css::RED, css::BLUE].as_slice());
    (list.width, list.height) = (200.0, 160.0);
    list.background = Color::BLACK;
    list.ops = vec![Op::Glyphs {
        run,
        origin: Point::new(0.0, 100.0),
        brush: Brush::Gradient(gradient),
    }];
    let image = CpuRenderer::new()
        .render_to_rgba(&list, &MemoryStore::new(), 1.0)
        .unwrap_or_else(|e| panic!("{e}"));
    let sum = |channel: usize| -> u64 {
        image
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| u64::from(p[channel]))
            .sum()
    };
    assert!(sum(2) > 0 && sum(0) == 0, "red {}, blue {}", sum(0), sum(2));
}

/// The skin format allows radii in the thousands.
#[test]
fn huge_blur_radii_do_not_ask_for_huge_layers() {
    let mut list = sheet();
    (list.width, list.height) = (64.0, 64.0);
    let rect = Shape::Rect(Rect::new(8.0, 8.0, 56.0, 56.0));
    list.ops = vec![
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
    ];
    CpuRenderer::new()
        .render_to_rgba(&list, &MemoryStore::new(), 2.0)
        .unwrap_or_else(|e| panic!("{e}"));
}

fn is_size(r: Result<impl std::fmt::Debug, CpuError>) -> bool {
    matches!(r, Err(CpuError::Size { .. }))
}

#[test]
fn zero_and_nan_sizes_and_scales_are_errors() {
    assert!(is_size(CpuSurface::new(0, 4)));
    assert!(is_size(CpuSurface::new(4, 0)));
    let mut list = sheet();
    let draw = |list: &DisplayList, scale: f64| {
        CpuRenderer::new().render_to_rgba(list, &MemoryStore::new(), scale)
    };
    for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(
            matches!(draw(&list, scale), Err(CpuError::Scale(_))),
            "{scale}"
        );
    }
    list.width = f64::NAN;
    assert!(is_size(draw(&list, 1.0)));
}

#[test]
fn a_frame_too_large_to_allocate_is_an_error_not_an_abort() {
    assert!(is_size(CpuSurface::new(65535, 65535)));
    assert!(is_size(CpuSurface::new(16384, 16385)));
    assert!(is_size(CpuSurface::new(65536, 1)));
    // Each backdrop is one more pixmap of the surface's size.
    let side = 4096;
    let backdrops = MAX_FRAME_PIXELS / (side * side);
    let mut list = sheet();
    list.ops = (0..backdrops)
        .map(|_| Op::Backdrop {
            shape: Shape::Rect(Rect::new(0.0, 0.0, 8.0, 8.0)),
            sigma: 1.0,
        })
        .collect();
    let mut surface = CpuSurface::new(4096, 4096).unwrap_or_else(|e| panic!("{e}"));
    assert!(is_size(CpuRenderer::new().render(
        &list,
        &MemoryStore::new(),
        1.0,
        &mut surface
    )));
}

/// White on the left half, black on the right, then a backdrop inside a clip of the top half
/// and a layer of opacity `alpha`.
fn backdrop_in_layers(alpha: f32) -> Image {
    let mut list = sheet();
    (list.width, list.height) = (32.0, 32.0);
    list.background = Color::BLACK;
    let rect = |r: Rect| Shape::Rect(r);
    list.ops = vec![
        Op::Fill {
            shape: rect(Rect::new(0.0, 0.0, 16.0, 32.0)),
            brush: Brush::Solid(css::WHITE),
        },
        Op::PushClip {
            shape: rect(Rect::new(0.0, 0.0, 32.0, 16.0)),
        },
        Op::PushOpacity { alpha },
        Op::Backdrop {
            shape: rect(Rect::new(0.0, 0.0, 32.0, 32.0)),
            sigma: 4.0,
        },
        Op::Pop,
        Op::Pop,
    ];
    CpuRenderer::new()
        .render_to_rgba(&list, &MemoryStore::new(), 1.0)
        .unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn a_backdrop_is_drawn_through_the_open_opacity_and_clip_layers() {
    let (half, full) = (backdrop_in_layers(0.5), backdrop_in_layers(1.0));
    let red = |i: &Image, x: usize, y: usize| i.rgba[(y * 32 + x) * 4];
    // Outside the clip the edge stays sharp.
    assert_eq!((red(&half, 15, 24), red(&half, 16, 24)), (255, 0));
    // Inside it the edge is blurred, and half as much at half opacity.
    let (sharp, blurred, mixed) = (255, red(&full, 15, 8), red(&half, 15, 8));
    assert!((100..200).contains(&blurred), "{blurred}");
    assert!(
        mixed.abs_diff(blurred.midpoint(sharp)) <= 2,
        "{mixed} between {blurred} and {sharp}"
    );
    assert!(red(&half, 16, 8) > 0 && red(&half, 16, 8) < red(&full, 16, 8));
}
