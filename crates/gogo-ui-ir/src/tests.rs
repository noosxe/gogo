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
fn action_nodes_are_in_document_order() {
    let mut t = Tree::new("r", Node::container());
    let a = t.add_child(t.root(), "r/a", Node::container()).unwrap();
    let b = t
        .add_child(
            t.root(),
            "r/b",
            Node::container().with_action(ActionRef::new(2)),
        )
        .unwrap();
    let a1 = t
        .add_child(a, "r/a/1", Node::container().with_action(ActionRef::new(1)))
        .unwrap();
    assert_eq!(t.action_nodes(), vec![a1, b]);
}

#[test]
fn app_parts_are_opaque_ids() {
    let part = AppPart {
        widget: WidgetId::new(3),
        part: Some(PartId::new(1)),
    };
    let n = Node::container().drawn_by(DrawnBy::App(part));
    assert_eq!(n.drawn_by, DrawnBy::App(part));
}

#[cfg(feature = "serde")]
#[test]
fn serializes_nested() {
    let mut t = Tree::new("r", Node::container().with_name("page"));
    t.add_child(
        t.root(),
        "r/t",
        Node::text("Hi").with_action(ActionRef::new(4)),
    )
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

#[test]
fn ids_round_trip() {
    assert_eq!(ActionRef::new(7).get(), 7);
    assert_eq!(ActionRef::new(7).index(), 7);
    assert_eq!(WidgetId::new(2).get(), 2);
    assert_eq!(PartId::new(5).get(), 5);
}

#[test]
fn lookups_by_key_and_name() {
    let mut t = Tree::new("r", Node::container().with_name("page"));
    let a = t
        .add_child(t.root(), "r/a", Node::container().with_name("dup"))
        .unwrap();
    let b = t
        .add_child(t.root(), "r/b", Node::container().with_name("dup"))
        .unwrap();
    assert_eq!(t.by_key("r"), Some(t.root()));
    assert_eq!(t.by_key("r/b"), Some(b));
    assert_eq!(t.by_key("r/zzz"), None);
    assert_eq!(t.by_name("page"), Some(t.root()));
    // Names are not unique: the first in document order wins.
    assert_eq!(t.by_name("dup"), Some(a));
    assert_eq!(t.by_name(""), None);
}

#[test]
fn iter_handles_deep_and_wide_trees() {
    let mut t = Tree::new("r", Node::container());
    let mut last = t.root();
    for i in 0..5_000 {
        last = t
            .add_child(last, format!("r/d{i}"), Node::container())
            .unwrap();
    }
    assert_eq!(t.iter().count(), 5_001);
    assert_eq!(t.iter().last(), Some(last));

    let mut w = Tree::new("r", Node::container());
    for i in 0..5_000 {
        w.add_child(w.root(), format!("r/w{i}"), Node::text("x"))
            .unwrap();
    }
    let order: Vec<_> = w.iter().collect();
    assert_eq!(order.len(), 5_001);
    assert_eq!(order[0], w.root());
    assert_eq!(order[1].index(), 1);
    assert_eq!(order[5_000].index(), 5_000);
}

#[test]
fn a_big_tree_builds_validates_and_looks_up() {
    let mut t = Tree::new("r", Node::container());
    let mut parents = vec![t.root()];
    for i in 0..20_000usize {
        let p = parents[i / 4];
        let id = t
            .add_child(p, format!("r/n{i}"), Node::container().with_name("n"))
            .unwrap();
        parents.push(id);
    }
    assert_eq!(t.len(), 20_001);
    assert_eq!(t.iter().count(), 20_001);
    assert_eq!(t.validate(), Ok(()));
    assert_eq!(t.by_key("r/n19999").map(NodeId::index), Some(20_000));
    assert_eq!(t.key(t.by_key("r/n12345").unwrap()), Some("r/n12345"));
}

#[test]
fn sides_helpers() {
    let s = Sides::all(3.0);
    assert_eq!((s.top, s.right, s.bottom, s.left), (3.0, 3.0, 3.0, 3.0));
    let s = Sides::symmetric(1.0, 2.0);
    assert_eq!((s.top, s.right, s.bottom, s.left), (1.0, 2.0, 1.0, 2.0));
    const C: Sides<u8> = Sides::all(9);
    assert_eq!(C.left, 9);
}

#[test]
fn focus_order_honours_focusable_and_disabled() {
    let mut t = Tree::new("r", Node::container());
    let focus = |role| Node::container().with_a11y(A11y::control(role, "x"));
    let a = t.add_child(t.root(), "r/a", focus(Role::Button)).unwrap();
    let mut off = focus(Role::Button);
    off.a11y.disabled = true;
    t.add_child(t.root(), "r/off", off).unwrap();
    t.add_child(t.root(), "r/plain", Node::container()).unwrap();
    let b = t
        .add_child(
            t.root(),
            "r/b",
            focus(Role::Tab).with_action(ActionRef::new(1)),
        )
        .unwrap();
    assert_eq!(t.focus_order(), vec![a, b]);
}

fn invalid(f: impl FnOnce(&mut Node)) -> Option<TreeError> {
    let mut t = Tree::new("r", Node::container());
    let id = t.add_child(t.root(), "r/x", Node::container()).unwrap();
    f(t.node_mut(id).unwrap());
    t.validate().err()
}

fn out_of_range(field: &'static str) -> Option<TreeError> {
    Some(TreeError::OutOfRange {
        key: "r/x".into(),
        field,
    })
}

fn stops(offsets: &[f32]) -> Vec<GradientStop> {
    offsets
        .iter()
        .map(|&offset| GradientStop {
            offset,
            color: Color::BLACK,
        })
        .collect()
}

fn with_fill(f: Fill) -> impl FnOnce(&mut Node) {
    move |n| n.paint.fill = Some(f)
}

fn linear(s: Vec<GradientStop>) -> Fill {
    Fill::Linear {
        angle: 180.0,
        stops: s,
    }
}

#[test]
fn validate_gradient_stops() {
    assert_eq!(invalid(with_fill(linear(stops(&[0.0, 1.0])))), None);
    assert_eq!(invalid(with_fill(linear(stops(&[0.0; 8])))), None);
    let count = out_of_range("gradient stop count");
    assert_eq!(invalid(with_fill(linear(stops(&[0.0])))), count);
    assert_eq!(invalid(with_fill(linear(stops(&[0.0; 9])))), count);
    assert_eq!(invalid(with_fill(Fill::Radial { stops: vec![] })), count);
    let offset = out_of_range("gradient stop offset");
    assert_eq!(invalid(with_fill(linear(stops(&[0.0, 1.5])))), offset);
    assert_eq!(invalid(with_fill(linear(stops(&[0.6, 0.4])))), offset);
    assert_eq!(invalid(with_fill(linear(stops(&[0.0, f32::NAN])))), offset);
}

fn glow(n: usize) -> impl FnOnce(&mut Node) {
    move |node| {
        node.paint.effects = vec![Effect::Glow {
            layers: vec![
                GlowLayer {
                    radius: 4.0,
                    color: Color::WHITE,
                };
                n
            ],
        }];
    }
}

#[test]
fn validate_glow_layers() {
    assert_eq!(invalid(glow(1)), None);
    assert_eq!(invalid(glow(4)), None);
    assert_eq!(invalid(glow(0)), out_of_range("glow layer count"));
    assert_eq!(invalid(glow(5)), out_of_range("glow layer count"));
}

#[test]
fn validate_opacity_and_progress() {
    for v in [0.0, 0.5, 1.0] {
        assert_eq!(invalid(|n| n.paint.opacity = v), None);
        assert_eq!(invalid(|n| n.progress = Some(v)), None);
    }
    for v in [-0.1, 1.1, f32::NAN, f32::INFINITY] {
        assert_eq!(invalid(|n| n.paint.opacity = v), out_of_range("opacity"));
        assert_eq!(invalid(|n| n.progress = Some(v)), out_of_range("progress"));
    }
    let loud = Paint {
        opacity: 2.0,
        ..Paint::default()
    };
    assert_eq!(
        invalid(|n| n.focus_paint = Some(loud)),
        out_of_range("opacity")
    );
}

fn with_effect(e: Effect) -> impl FnOnce(&mut Node) {
    move |n| n.paint.effects = vec![e]
}

#[test]
fn validate_effect_ranges() {
    let noise = Effect::Noise {
        amount: 1.5,
        seed: 7,
    };
    assert_eq!(invalid(with_effect(noise)), out_of_range("noise amount"));
    let thin = Effect::Scanlines {
        period: 0.5,
        alpha: 0.5,
    };
    assert_eq!(invalid(with_effect(thin)), out_of_range("scanline period"));
    let faint = Effect::Scanlines {
        period: 2.0,
        alpha: -1.0,
    };
    assert_eq!(invalid(with_effect(faint)), out_of_range("scanline alpha"));
    let sheen = Effect::Reflection {
        angle: 0.0,
        alpha: 2.0,
    };
    assert_eq!(
        invalid(with_effect(sheen)),
        out_of_range("reflection alpha")
    );
    let edge = Effect::Scanlines {
        period: 1.0,
        alpha: 1.0,
    };
    assert_eq!(invalid(with_effect(edge)), None);
}

fn with_text(f: fn(&mut TextStyle)) -> impl FnOnce(&mut Node) {
    move |n| {
        let mut style = TextStyle::default();
        f(&mut style);
        n.kind = Kind::Text {
            text: String::new(),
            style,
        };
    }
}

#[test]
fn validate_text_and_layout_ranges() {
    let field = out_of_range("text size, weight, width or tracking");
    assert_eq!(invalid(with_text(|_| {})), None);
    assert_eq!(invalid(with_text(|s| s.size = 0.5)), field);
    assert_eq!(invalid(with_text(|s| s.size = 2000.0)), field);
    assert_eq!(invalid(with_text(|s| s.weight = 0.0)), field);
    assert_eq!(invalid(with_text(|s| s.width = 10.0)), field);
    assert_eq!(invalid(with_text(|s| s.tracking = 6.0)), field);
    assert_eq!(invalid(with_text(|s| s.size = f32::NAN)), field);
    assert_eq!(
        invalid(|n| n.layout.col_span = 0),
        out_of_range("column span")
    );
    assert_eq!(
        invalid(|n| n.layout.columns = vec![Track::Fr(0.0)]),
        out_of_range("grid fr track")
    );
    assert_eq!(invalid(|n| n.layout.rows = vec![Track::Fr(1.0)]), None);
}

#[test]
fn names_and_families_take_shared_strings() {
    let n = Node::container().with_name(std::sync::Arc::<str>::from("n"));
    assert_eq!(n.name.as_deref(), Some("n"));
    assert_eq!(&*TextStyle::default().family, "");
}
