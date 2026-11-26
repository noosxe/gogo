//! S13 on the Default skin: the Tab order of the `game` and `shell` routes is the tree
//! order, and that is the order they read in (design 24 §8 PR E1).

mod common;

use common::*;
use gogo_gui_testkit::FixtureScope;
use gogo_skin::{Binding, Bound, Mode, SkinPackage, UiState};
use gogo_ui_interact::{FocusOrder, Frame, Intent, Key, LayerKind};
use gogo_ui_ir::{NodeId, Tree};
use gogo_ui_layout::{Laid, Rect, Scroll, Viewport, layout};
use gogo_ui_text::FontSet;

fn route(route: &str, width: f32) -> (Bound, Laid) {
    let skin = gogo_skins::default();
    let package =
        SkinPackage::from_files(skin.name, skin.to_files()).unwrap_or_else(|r| panic!("{r}"));
    let scope = FixtureScope::new();
    let ui = UiState::defaults(&package.manifest);
    let binding = Binding {
        scope: &scope,
        ui: &ui,
        mode: Mode::Dark,
        default: None,
    };
    let bound = package
        .bind(route, &binding)
        .unwrap_or_else(|e| panic!("{route}: {e:?}"));
    let laid = layout(
        &bound.tree,
        &mut FontSet::bundled(),
        Viewport::new(width, 800.0),
        &Scroll::none(),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    (bound, laid)
}

/// The box around the focusable controls at and under `node`.
fn controls(tree: &Tree, laid: &Laid, node: NodeId) -> Option<Rect> {
    let own = tree
        .get(node)
        .filter(|n| n.a11y.focusable)
        .and_then(|_| laid.rect(node));
    tree.children(node)
        .iter()
        .filter_map(|&child| controls(tree, laid, child))
        .chain(own)
        .reduce(|a, b| {
            Rect::new(
                a.x0.min(b.x0),
                a.y0.min(b.y0),
                a.x1.max(b.x1),
                a.y1.max(b.y1),
            )
        })
}

/// Whether `a` is read before `b`: wholly above it, or beside it and to its left.
fn reads_before(a: &Rect, b: &Rect) -> bool {
    let above = a.y1 <= b.y0;
    let below = b.y1 <= a.y0;
    above || (!below && a.x1 <= b.x0)
}

#[test]
fn s13_default_focus_order_is_reading_order() {
    for (name, width) in [("game", 1280.0), ("shell", 1280.0), ("game", 960.0)] {
        let (bound, laid) = route(name, width);
        let tree = &bound.tree;
        // Within every container, the parts holding controls come in reading order.
        let mut compared = 0;
        for parent in tree.iter() {
            let boxes: Vec<(NodeId, Rect)> = tree
                .children(parent)
                .iter()
                .filter_map(|&c| Some((c, controls(tree, &laid, c)?)))
                .collect();
            for (i, (a, first)) in boxes.iter().enumerate() {
                for (b, second) in &boxes[i + 1..] {
                    compared += 1;
                    assert!(
                        reads_before(first, second),
                        "{name} {width}: {:?} {first:?} then {:?} {second:?}",
                        tree.key(*a),
                        tree.key(*b),
                    );
                }
            }
        }
        assert!(compared > 0, "{name}");
    }
}

#[test]
fn s13_default_tab_walks_the_tree_order_and_reaches_every_action() {
    for name in ["game", "shell"] {
        let (bound, laid) = route(name, 1280.0);
        let tree = &bound.tree;
        let layers = [gogo_ui_interact::Layer::new(
            PAGE,
            LayerKind::Page,
            tree,
            &laid,
        )];
        let frame = Frame { layers: &layers };
        let expected: Vec<&str> = tree
            .iter()
            .filter(|&id| tree.get(id).is_some_and(|n| n.a11y.focusable))
            .filter_map(|id| tree.key(id))
            .collect();
        let order = FocusOrder::of(&frame);
        let keys: Vec<&str> = order.targets().iter().map(|t| t.key.as_str()).collect();
        assert_eq!(keys, expected, "{name}");
        for id in tree.action_nodes() {
            let key = tree.key(id).unwrap();
            assert!(keys.contains(&key), "{name}: `{key}` has no Tab stop");
        }
        let mut d = Driver::new();
        let walked: Vec<String> = (0..expected.len()).map(|_| d.tab(&frame)).collect();
        assert_eq!(walked, expected, "{name}");
    }
}

#[test]
fn s19_default_play_and_search_keys() {
    for (name, key, mods, node) in [
        ("game", Key::Char('p'), NONE, Some("play")),
        ("shell", Key::Char('k'), CTRL, None),
    ] {
        let (bound, laid) = route(name, 1280.0);
        let layers = [gogo_ui_interact::Layer::new(
            PAGE,
            LayerKind::Page,
            &bound.tree,
            &laid,
        )];
        let mut d = Driver::new();
        let out = d.down(&Frame { layers: &layers }, key, mods);
        match (node, &out[..]) {
            (Some(id), [Intent::Invoke { target, action, .. }]) => {
                assert_eq!(bound.tree.by_name(id), Some(target.node));
                let call = bound.actions.get(*action).expect("a call");
                assert_eq!(call.spec.name, "game.play");
            }
            // The skin's own "Ctrl K" button does not see it.
            (None, [Intent::Palette { .. }]) => {}
            (_, other) => panic!("{name}: {other:?}"),
        }
    }
}
