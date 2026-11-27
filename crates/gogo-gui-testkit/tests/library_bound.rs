//! A large library costs what the window shows, not what the library holds (#545).
//!
//! The node count is the bound and is exact. The times are printed, with loose limits so
//! that a slow machine or a debug build doesn't fail them: run
//! `cargo nextest run --release -p gogo-gui-testkit library_bound --no-capture`.

use std::time::Duration;

use gogo_gui_testkit::scenes::{LibraryFrame, library};
use gogo_skin::ir::Kind;

const SIZE: (f32, f32) = (1280.0, 800.0);

fn frame(games: usize, view: &str, offset: f32) -> LibraryFrame {
    library(games, view, offset, SIZE).unwrap_or_else(|e| panic!("{games} {view} {offset}: {e}"))
}

fn titles(f: &LibraryFrame) -> Vec<&str> {
    f.tree
        .iter()
        .filter_map(|id| match &f.tree.get(id)?.kind {
            Kind::Text { text, .. } if text.starts_with("Filler Game ") => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// The offsets of the top, the middle and the end of the route.
fn places(games: usize, view: &str) -> [f32; 3] {
    let f = frame(games, view, 0.0);
    let max = f
        .tree
        .iter()
        .find(|&id| f.tree.get(id).and_then(|n| n.name.as_deref()) == Some("main"))
        .and_then(|id| f.laid.scroll_box(id))
        .map(|b| b.max as f32)
        .expect("the route scrolls");
    [0.0, max / 2.0, max]
}

#[test]
fn what_is_bound_does_not_grow_with_the_library() {
    for view in ["grid", "list"] {
        let thousand = places(1_000, view);
        let many = places(10_000, view);
        for (at, (a, b)) in thousand.into_iter().zip(many).enumerate() {
            let small = frame(1_000, view, a);
            let large = frame(10_000, view, b);
            let (small, large) = (small.tree.len(), large.tree.len());
            // The middle can show one row more or less, as the offset falls.
            let slack = if at == 1 { small / 10 } else { 0 };
            assert!(
                small.abs_diff(large) <= slack,
                "{view} at {at}: {small} nodes for 1 000 games, {large} for 10 000"
            );
            assert!(large < 2_000, "{view} at {at}: {large} nodes");
        }
    }
}

#[test]
fn the_rows_in_view_are_bound_wherever_the_route_is() {
    for (view, last) in [("grid", "Filler Game 9999"), ("list", "Filler Game 9999")] {
        let [_, middle, end] = places(10_000, view);
        let at_end = frame(10_000, view, end);
        assert!(titles(&at_end).contains(&last), "{view}");
        // In the middle, games from there and none that aren't near it, save the first
        // and the last row.
        let f = frame(10_000, view, middle);
        let numbers: Vec<usize> = titles(&f)
            .iter()
            .filter_map(|t| t.rsplit(' ').next()?.parse().ok())
            .collect();
        assert!(
            numbers.iter().any(|&n| (4_000..6_000).contains(&n)),
            "{view}"
        );
        assert!(
            numbers
                .iter()
                .all(|&n| !(20..=9_990).contains(&n) || (4_000..6_000).contains(&n)),
            "{view}: {numbers:?}"
        );
    }
}

#[test]
fn library_bound_times() {
    for view in ["grid", "list"] {
        let mean = |games: usize| {
            let mut bind = Duration::ZERO;
            let mut layout = Duration::ZERO;
            for _ in 0..5 {
                let f = frame(games, view, 0.0);
                bind += f.bind / 5;
                layout += f.layout / 5;
            }
            println!("{view} {games}: bind {bind:?}, layout {layout:?}");
            (bind, layout)
        };
        let (_, small) = mean(1_000);
        let (bind, large) = mean(10_000);
        assert!(
            large < small * 3 + Duration::from_millis(20),
            "{view}: layout {small:?} then {large:?}"
        );
        assert!(bind < Duration::from_secs(1), "{view}: bind {bind:?}");
    }
}
