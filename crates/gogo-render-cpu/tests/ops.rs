//! Ops the test scenes do not produce.

use std::sync::Arc;

use gogo_gui_testkit::scenes;
use gogo_render_cpu::CpuRenderer;
use gogo_ui_scene::{Brush, DisplayList, MemoryStore, Op, Shape};
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
