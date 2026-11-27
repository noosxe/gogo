//! Scroll containers as a screen reader sees and moves them.

use gogo_ui_a11y::A11yTree;
use gogo_ui_a11y::accesskit::{
    Action, ActionData, ActionRequest, Node as AxNode, NodeId, Point, Role, ScrollUnit, TreeId,
    TreeUpdate,
};
use gogo_ui_interact::{Config, Frame, Interaction, Layer, LayerId, LayerKind, RequestKind};
use gogo_ui_ir::{
    A11y, ActionRef, Dim, LayoutStyle, Node, Role as IrRole, ScrollSpec, Tree as IrTree,
};
use gogo_ui_layout::{Laid, Scroll, Viewport, layout};
use gogo_ui_text::FontSet;

fn sized(w: f32, h: f32) -> LayoutStyle {
    let mut s = LayoutStyle::default();
    s.width = Dim::Px(w);
    s.height = Dim::Px(h);
    s
}

fn button(name: &str) -> Node {
    Node::container()
        .with_layout(sized(100.0, 40.0))
        .with_a11y(A11y::control(IrRole::Button, name))
        .with_action(ActionRef::new(0))
}

/// Ten buttons in a named container 100 px high, scrolled by 30.
fn screen() -> (IrTree, Laid) {
    let mut tree = IrTree::new("root", Node::container().with_layout(sized(800.0, 600.0)));
    let mut list = Node::container().with_layout(sized(200.0, 100.0));
    list.scroll = Some(ScrollSpec::default());
    list.a11y.name = Some("Games".to_owned());
    let root = tree.root();
    let list = tree.add_child(root, "list", list).expect("add");
    for i in 0..10 {
        tree.add_child(list, format!("b{i}"), button(&format!("Game {i}")))
            .expect("add");
    }
    let laid = layout(
        &tree,
        &mut FontSet::bundled(),
        Viewport::new(800.0, 600.0),
        &Scroll::none().with(list, 0.0, 30.0),
    )
    .expect("layout");
    (tree, laid)
}

fn named<'a>(update: &'a TreeUpdate, name: &str) -> (NodeId, &'a AxNode) {
    let (id, node) = update
        .nodes
        .iter()
        .find(|(_, n)| n.label() == Some(name))
        .expect(name);
    (*id, node)
}

#[test]
fn a_scroll_container_says_where_it_is_and_takes_scroll_requests() {
    let (tree, laid) = screen();
    let layers = [Layer::new(LayerId(1), LayerKind::Page, &tree, &laid).identity(&[7])];
    let frame = Frame { layers: &layers };
    let mut a11y = A11yTree::new("gogo");
    let update = a11y.update(&frame, &Interaction::new(Config::default()), 1.0);

    let (list, node) = named(&update, "Games");
    assert_eq!(node.role(), Role::ScrollView);
    assert!(node.clips_children());
    assert_eq!(
        (node.scroll_y(), node.scroll_y_min(), node.scroll_y_max()),
        (Some(30.0), Some(0.0), Some(300.0))
    );
    for action in [
        Action::ScrollUp,
        Action::ScrollDown,
        Action::SetScrollOffset,
    ] {
        assert!(node.supports_action(action), "{action:?}");
    }
    // What is out of sight is still in the tree, and can ask to be brought back.
    let (last, node) = named(&update, "Game 9");
    assert!(node.supports_action(Action::ScrollIntoView));

    let kind = |target_node, action, data| {
        a11y.map_request(&ActionRequest {
            action,
            target_tree: TreeId::ROOT,
            target_node,
            data,
        })
        .map(|r| r.kind)
    };
    assert_eq!(
        kind(last, Action::ScrollIntoView, None),
        Some(RequestKind::Reveal)
    );
    assert_eq!(
        kind(
            list,
            Action::ScrollDown,
            Some(ActionData::ScrollUnit(ScrollUnit::Page))
        ),
        Some(RequestKind::ScrollBy {
            pages: true,
            back: false
        })
    );
    assert_eq!(
        kind(list, Action::ScrollUp, None),
        Some(RequestKind::ScrollBy {
            pages: false,
            back: true
        })
    );
    let to = ActionData::SetScrollOffset(Point::new(0.0, 120.4));
    assert_eq!(
        kind(list, Action::SetScrollOffset, Some(to)),
        Some(RequestKind::ScrollTo(120))
    );
}
