//! The Default skin's `game` and `shell` routes as a screen reader sees them: everything
//! with an action is there, named (S14), in focus order (S13).

use accesskit_consumer::{NodeRef, Tree};
use gogo_gui_testkit::FixtureScope;
use gogo_skin::{Binding, Bound, Mode, SkinPackage, UiState};
use gogo_ui_a11y::A11yTree;
use gogo_ui_a11y::accesskit::{Action, Role};
use gogo_ui_interact::{Config, FocusOrder, Frame, Input, Interaction, Key, Layer, LayerId};
use gogo_ui_interact::{LayerKind, Modifiers};
use gogo_ui_layout::{Laid, Scroll, Viewport, layout};
use gogo_ui_text::FontSet;

fn route(route: &str) -> (Bound, Laid) {
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
        Viewport::new(1280.0, 800.0),
        &Scroll::none(),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    (bound, laid)
}

/// An identity per action, as the app records them.
fn identities(bound: &Bound) -> Vec<u64> {
    (0..bound.actions.len() as u64).collect()
}

/// Every node, depth first: the order a screen reader reads.
fn all<'a>(node: NodeRef<'a>, out: &mut Vec<NodeRef<'a>>) {
    out.push(node);
    for child in node.children() {
        all(child, out);
    }
}

#[test]
fn s13_default_screen_reader_order_is_focus_order_and_every_control_is_named() {
    for name in ["game", "shell"] {
        let (bound, laid) = route(name);
        let identity = identities(&bound);
        let layers =
            [Layer::new(LayerId(1), LayerKind::Page, &bound.tree, &laid).identity(&identity)];
        let frame = Frame { layers: &layers };
        let mut input = Interaction::new(Config::default());
        let mut a11y = A11yTree::new("window");
        let tree = Tree::new(a11y.update(&frame, &input, 1.0), true);
        let mut nodes = Vec::new();
        all(tree.state().root(), &mut nodes);

        let focusable: Vec<NodeRef<'_>> = nodes
            .iter()
            .copied()
            .filter(|n| n.data().supports_action(Action::Focus))
            .collect();
        let order = FocusOrder::of(&frame);
        assert_eq!(focusable.len(), order.targets().len(), "{name}");
        for (node, target) in focusable.iter().zip(order.targets()) {
            let label = node.label().unwrap_or_default();
            let ours = bound.tree.get(target.node).unwrap();
            assert!(!label.is_empty(), "{name}: `{}` has no name", target.key);
            if let Some(expected) = &ours.a11y.name {
                assert_eq!(&label, expected, "{name}: `{}`", target.key);
            }
            // A control is one node: its text is not read again.
            assert_eq!(node.children().count(), 0, "{name}: `{}`", target.key);
        }

        // Focus follows the keyboard.
        for target in order.targets() {
            let tab = Input::KeyDown {
                key: Key::Tab,
                mods: Modifiers::default(),
                repeat: false,
            };
            input.handle(tab, Default::default(), &frame);
            input.handle(Input::KeyUp { key: Key::Tab }, Default::default(), &frame);
            let tree = Tree::new(a11y.update(&frame, &input, 1.0), true);
            let focused = tree.state().focus().expect("focus");
            let ours = bound.tree.get(target.node).unwrap();
            if let Some(expected) = &ours.a11y.name {
                assert_eq!(focused.label().as_ref(), Some(expected));
            }
            assert!(focused.data().supports_action(Action::Focus));
        }
    }
}

#[test]
fn default_game_states_text_and_bounds() {
    let (bound, laid) = route("game");
    let identity = identities(&bound);
    let layers = [Layer::new(LayerId(1), LayerKind::Page, &bound.tree, &laid).identity(&identity)];
    let frame = Frame { layers: &layers };
    let input = Interaction::new(Config::default());
    let mut a11y = A11yTree::new("window");
    let tree = Tree::new(a11y.update(&frame, &input, 2.0), true);
    let mut nodes = Vec::new();
    all(tree.state().root(), &mut nodes);

    let of = |role: Role| nodes.iter().copied().filter(move |n| n.role() == role);
    assert_eq!(of(Role::Switch).count(), 2);
    assert!(of(Role::Switch).all(|n| n.toggled().is_some()));
    assert_eq!(of(Role::Tab).count(), 7);
    assert_eq!(
        of(Role::Tab)
            .filter(|n| n.is_selected() == Some(true))
            .count(),
        1
    );
    assert!(of(Role::ComboBox).all(|n| n.value().is_some_and(|v| !v.is_empty())));
    assert!(of(Role::Heading).any(|n| n.label().is_some()));
    assert!(of(Role::Label).count() > 5, "text is read");

    let play = of(Role::Button)
        .find(|n| n.label().as_deref() == Some("Play"))
        .expect("play");
    assert_eq!(play.data().keyboard_shortcut(), Some("P"));
    let ours = laid.rect(bound.tree.by_name("play").unwrap()).unwrap();
    let theirs = play.bounding_box().expect("bounds");
    assert_eq!(
        (theirs.x0, theirs.y0, theirs.x1, theirs.y1),
        (ours.x0 * 2.0, ours.y0 * 2.0, ours.x1 * 2.0, ours.y1 * 2.0)
    );
}
