use crate::*;

#[test]
fn layout_defaults_match_the_skin_format() {
    let l = LayoutStyle::default();
    assert_eq!(l.display, Display::Flex);
    assert_eq!(l.direction, Direction::Column);
    assert_eq!(l.shrink, 1.0);
    assert_eq!(l.grow, 0.0);
    assert_eq!(l.col_span, 1);
    assert_eq!(l.position, Position::Relative);
    assert_eq!(l.width, Dim::Auto);
    assert_eq!(l.inset.top, Dim::Auto);
    assert_eq!(LayoutStyle::row().direction, Direction::Row);
    assert_eq!(LayoutStyle::column(), LayoutStyle::default());
    assert_eq!(
        LayoutStyle::grid(vec![Track::Fr(1.0)]).display,
        Display::Grid
    );
}

#[test]
fn paint_text_and_a11y_defaults() {
    let p = Paint::default();
    assert!(p.fill.is_none() && p.ring.is_none() && p.effects.is_empty());
    assert_eq!(p.opacity, 1.0);
    assert_eq!(p.shape, Shape::Rect);
    assert_eq!(p.border, [None; 4]);
    let t = TextStyle::default();
    assert_eq!((t.size, t.weight, t.width), (16.0, 400.0, 100.0));
    assert!(!t.wrap);
    assert_eq!(Fit::default(), Fit::Cover);
    assert_eq!(DrawnBy::default(), DrawnBy::Skin);
    assert_eq!(A11y::default().role, Role::Generic);
    assert!(!A11y::default().focusable);
    assert_eq!(Node::default().kind, Kind::Box);
}

#[test]
fn constructors() {
    assert_eq!(Sides::symmetric(1.0, 2.0).left, 2.0);
    assert_eq!(Sides::symmetric(1.0, 2.0).bottom, 1.0);
    assert_eq!(Color::rgb(1, 2, 3), Color([1, 2, 3, 255]));
    assert_eq!(
        Paint::solid(Color::WHITE).fill,
        Some(Fill::Solid(Color::WHITE))
    );
    let a = A11y::control(Role::Button, "Go");
    assert!(a.focusable);
    assert_eq!(a.name.as_deref(), Some("Go"));
}

#[test]
fn builds_in_document_order() {
    let mut t = Tree::new("r", Node::container());
    let a = t
        .add_child(t.root(), "r/a", Node::container().with_name("a"))
        .unwrap();
    let b = t.add_child(t.root(), "r/b", Node::text("b")).unwrap();
    let a1 = t.add_child(a, "r/a/1", Node::text("a1")).unwrap();
    let order: Vec<_> = t.iter().collect();
    assert_eq!(order, vec![t.root(), a, a1, b]);
    assert_eq!(t.len(), 4);
    assert!(!t.is_empty());
    assert_eq!(t.parent(a1), Some(a));
    assert_eq!(t.parent(t.root()), None);
    assert_eq!(t.children(t.root()), &[a, b]);
    assert_eq!(t.by_key("r/a/1"), Some(a1));
    assert_eq!(t.key(b), Some("r/b"));
    assert_eq!(t.by_name("a"), Some(a));
    assert_eq!(t.by_name("zzz"), None);
}

#[test]
fn keys_are_unique() {
    let mut t = Tree::new("r", Node::container());
    t.add_child(t.root(), "r/a", Node::container()).unwrap();
    assert_eq!(
        t.add_child(t.root(), "r/a", Node::container()),
        Err(TreeError::DuplicateKey("r/a".into()))
    );
    assert_eq!(
        t.add_child(t.root(), "r", Node::container()),
        Err(TreeError::DuplicateKey("r".into()))
    );
    assert_eq!(t.len(), 2);
}

#[test]
fn leaves_take_no_children_and_foreign_ids_are_refused() {
    let mut t = Tree::new("r", Node::container());
    let txt = t.add_child(t.root(), "r/t", Node::text("x")).unwrap();
    assert_eq!(
        t.add_child(txt, "r/t/c", Node::container()),
        Err(TreeError::LeafParent("r/t".into()))
    );
    let mut o = Tree::new("o", Node::container());
    o.add_child(o.root(), "o/1", Node::container()).unwrap();
    let other = o.add_child(o.root(), "o/2", Node::container()).unwrap();
    assert_eq!(
        t.add_child(other, "r/x", Node::container()),
        Err(TreeError::NoSuchNode)
    );
    assert_eq!(t.validate(), Ok(()));
    assert_eq!(t.len(), 2);
}

#[test]
fn validate_catches_a_container_turned_leaf() {
    let mut t = Tree::new("r", Node::container());
    t.add_child(t.root(), "r/a", Node::container()).unwrap();
    if let Some(n) = t.node_mut(t.root()) {
        n.kind = Kind::Icon("play".into());
    }
    assert_eq!(t.validate(), Err(TreeError::LeafParent("r".into())));
}

#[test]
fn action_nodes_are_in_focus_order() {
    let mut t = Tree::new("r", Node::container());
    let a = t.add_child(t.root(), "r/a", Node::container()).unwrap();
    let b = t
        .add_child(t.root(), "r/b", Node::container().with_action(ActionRef(2)))
        .unwrap();
    let a1 = t
        .add_child(a, "r/a/1", Node::container().with_action(ActionRef(1)))
        .unwrap();
    assert_eq!(t.action_nodes(), vec![a1, b]);
}

#[test]
fn app_parts_are_opaque_ids() {
    let part = AppPart {
        widget: WidgetId(3),
        part: Some(PartId(1)),
    };
    let n = Node::container().drawn_by(DrawnBy::App(part));
    assert_eq!(n.drawn_by, DrawnBy::App(part));
}

#[cfg(feature = "serde")]
#[test]
fn serializes_nested() {
    let mut t = Tree::new("r", Node::container().with_name("page"));
    t.add_child(t.root(), "r/t", Node::text("Hi").with_action(ActionRef(4)))
        .unwrap();
    let v = serde_json::to_value(&t).unwrap();
    assert_eq!(v["key"], "r");
    assert_eq!(v["node"]["name"], "page");
    assert_eq!(v["node"]["kind"], "Box");
    assert_eq!(v["children"][0]["key"], "r/t");
    assert_eq!(v["children"][0]["node"]["kind"]["Text"]["text"], "Hi");
    assert_eq!(v["children"][0]["node"]["action"], 4);
    assert_eq!(
        v["children"][0]["children"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(v["node"]["layout"]["direction"], "Column");
    assert_eq!(v["node"]["paint"]["opacity"], 1.0);
}
