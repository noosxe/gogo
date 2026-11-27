//! IR + layout → display list: one test per rule of the crate docs (design 28 §1, §2).

use std::sync::Arc;

use gogo_ui_ir::{
    Bloom, Color, Dim, Effect, Fill, Fit, GlowLayer, GradientStop, ImageSource, Kind, LayoutStyle,
    Node, NodeId, Paint, Shape as IrShape, Stroke, TextMarks, TextStyle, Tree,
};
use gogo_ui_layout::{Scroll, Viewport, layout};
use gogo_ui_scene::{
    Brush, DisplayList, Image, ImageKey, ImageStore, MemoryStore, Op, Renderer, SceneState, Shape,
    Surface, build,
};
use gogo_ui_text::FontSet;

fn ls(mut base: LayoutStyle, f: impl FnOnce(&mut LayoutStyle)) -> LayoutStyle {
    f(&mut base);
    base
}

fn sized(w: f32, h: f32) -> LayoutStyle {
    ls(LayoutStyle::default(), |s| {
        s.width = Dim::Px(w);
        s.height = Dim::Px(h);
    })
}

fn boxed(w: f32, h: f32, paint: Paint) -> Node {
    Node::container().with_layout(sized(w, h)).with_paint(paint)
}

fn paint(f: impl FnOnce(&mut Paint)) -> Paint {
    let mut p = Paint::default();
    f(&mut p);
    p
}

fn add(t: &mut Tree, parent: NodeId, key: &str, node: Node) -> NodeId {
    t.add_child(parent, key, node).expect("add child")
}

fn add_root(t: &mut Tree, key: &str, node: Node) -> NodeId {
    let root = t.root();
    add(t, root, key, node)
}

fn scene_with(tree: &Tree, scroll: &Scroll, state: &SceneState) -> DisplayList {
    let laid = layout(
        tree,
        &mut FontSet::bundled(),
        Viewport::new(400.0, 300.0),
        scroll,
    )
    .expect("layout");
    build(tree, &laid, state)
}

fn scene(tree: &Tree) -> DisplayList {
    scene_with(tree, &Scroll::none(), &SceneState::new())
}

fn kinds(l: &DisplayList) -> Vec<&'static str> {
    l.ops.iter().map(Op::kind).collect()
}

/// Every `Push…` has its `Pop`.
fn balanced(l: &DisplayList) -> bool {
    let mut depth = 0i32;
    for op in &l.ops {
        match op {
            Op::PushClip { .. } | Op::PushOpacity { .. } | Op::PushFilter { .. } => depth += 1,
            Op::Pop => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return false;
        }
    }
    depth == 0
}

fn root_tree(child: Node) -> (Tree, NodeId) {
    let mut t = Tree::new("root", Node::container());
    let root = t.root();
    let id = add(&mut t, root, "n", child);
    (t, id)
}

fn red() -> Color {
    Color::rgb(200, 30, 30)
}

#[test]
fn an_empty_paint_draws_nothing_and_the_background_is_the_roots_fill_or_black() {
    let (t, _) = root_tree(boxed(50.0, 50.0, Paint::default()));
    let l = scene(&t);
    assert!(l.ops.is_empty(), "{:?}", kinds(&l));
    assert_eq!((l.width, l.height), (400.0, 300.0));
    assert_eq!(l.background, peniko::Color::from_rgba8(0, 0, 0, 255));

    let mut t = Tree::new(
        "root",
        Node::container().with_paint(Paint::solid(Color::rgb(10, 20, 30))),
    );
    add_root(&mut t, "a", Node::container());
    let l = scene(&t);
    assert_eq!(l.background, peniko::Color::from_rgba8(10, 20, 30, 255));
    assert_eq!(kinds(&l), ["Fill"], "the root's own fill is drawn too");
}

#[test]
fn a_transparent_fill_draws_nothing() {
    let (t, _) = root_tree(boxed(50.0, 50.0, Paint::solid(Color::TRANSPARENT)));
    assert!(scene(&t).ops.is_empty());
}

#[test]
fn effects_are_drawn_in_the_fixed_order_whatever_the_order_they_were_written_in() {
    let effects = vec![
        Effect::InnerShadow {
            dx: 0.0,
            dy: 1.0,
            blur: 4.0,
            color: Color::BLACK,
        },
        Effect::Reflection {
            angle: 135.0,
            alpha: 0.2,
        },
        Effect::Scanlines {
            period: 4.0,
            alpha: 0.1,
        },
        Effect::Noise {
            amount: 0.05,
            seed: 7,
        },
        Effect::BackdropBlur { radius: 12.0 },
        Effect::Glow {
            layers: vec![
                GlowLayer {
                    radius: 4.0,
                    color: red(),
                },
                GlowLayer {
                    radius: 12.0,
                    color: red(),
                },
            ],
        },
        Effect::Shadow {
            dx: 0.0,
            dy: 4.0,
            blur: 8.0,
            spread: 0.0,
            color: Color::BLACK,
        },
    ];
    let p = paint(|p| {
        p.fill = Some(Fill::Solid(Color::rgb(40, 40, 40)));
        p.border = [Some(Stroke {
            width: 1.0,
            color: Color::WHITE,
        }); 4];
        p.radius = [6.0; 4];
        p.effects = effects;
    });
    let (t, _) = root_tree(boxed(100.0, 60.0, p));
    let l = scene(&t);
    assert_eq!(
        kinds(&l),
        [
            "BlurRect", // shadow
            "BlurRect", "BlurRect", // glow, outermost first
            "Backdrop", "Fill", // fill
            "Fill", // noise
            "PushClip", "Fill", "Pop",  // scanlines
            "Fill", // reflection
            "PushClip", "BlurRect", "Pop",  // inner shadow
            "Fill", // border
        ]
    );
    assert!(balanced(&l));
    // The glow draws the outer halo (sigma 6) before the inner one (sigma 2).
    let sigmas: Vec<f64> = l
        .ops
        .iter()
        .filter_map(|o| match o {
            Op::BlurRect {
                sigma,
                inset: false,
                ..
            } => Some(*sigma),
            _ => None,
        })
        .collect();
    assert_eq!(
        sigmas,
        [4.0, 6.0, 2.0],
        "shadow blur 8 -> sigma 4, glow 12 -> 6, glow 4 -> 2"
    );
}

#[test]
fn blur_values_become_sigmas_and_a_backdrop_keeps_its_radius() {
    let p = paint(|p| {
        p.effects = vec![
            Effect::Shadow {
                dx: 3.0,
                dy: 5.0,
                blur: 10.0,
                spread: 2.0,
                color: Color::BLACK,
            },
            Effect::BackdropBlur { radius: 9.0 },
        ];
        p.radius = [4.0; 4];
    });
    let (t, _) = root_tree(boxed(100.0, 60.0, p));
    let l = scene(&t);
    let Op::BlurRect {
        rect,
        radii,
        sigma,
        inset,
        bounds,
        ..
    } = &l.ops[0]
    else {
        panic!("{:?}", kinds(&l));
    };
    assert_eq!(*sigma, 5.0);
    assert!(!inset);
    // The shadow box: 100x60 grown by the spread 2 and moved by (3, 5).
    assert_eq!(
        (rect.x0, rect.y0, rect.x1, rect.y1),
        (1.0, 3.0, 105.0, 67.0)
    );
    assert_eq!(radii.top_left, 6.0);
    assert_eq!(bounds.x0, rect.x0 - 15.0);
    let Op::Backdrop { sigma, .. } = &l.ops[1] else {
        panic!()
    };
    assert_eq!(*sigma, 9.0);
}

#[test]
fn a_shadow_without_blur_is_a_hard_offset_fill() {
    let p = paint(|p| {
        p.effects = vec![Effect::Shadow {
            dx: 4.0,
            dy: 4.0,
            blur: 0.0,
            spread: 0.0,
            color: Color::BLACK,
        }];
    });
    let (t, _) = root_tree(boxed(40.0, 20.0, p));
    let l = scene(&t);
    assert_eq!(kinds(&l), ["Fill"]);
    let Op::Fill {
        shape: Shape::Rect(r),
        ..
    } = &l.ops[0]
    else {
        panic!("{:?}", l.ops[0])
    };
    assert_eq!((r.x0, r.y0), (4.0, 4.0));
}

#[test]
fn an_inner_shadow_is_an_inset_blur_inside_a_clip_of_the_box() {
    let p = paint(|p| {
        p.effects = vec![Effect::InnerShadow {
            dx: 0.0,
            dy: 2.0,
            blur: 6.0,
            color: Color::BLACK,
        }];
        p.radius = [8.0; 4];
    });
    let (t, _) = root_tree(boxed(80.0, 40.0, p));
    let l = scene(&t);
    assert_eq!(kinds(&l), ["PushClip", "BlurRect", "Pop"]);
    let Op::BlurRect {
        inset,
        rect,
        bounds,
        sigma,
        ..
    } = &l.ops[1]
    else {
        panic!()
    };
    assert!(inset);
    assert_eq!(*sigma, 3.0);
    assert_eq!(rect.y0, 2.0, "shifted by the offset");
    assert_eq!(bounds.y0, 0.0, "painted over the whole box");
    assert!(matches!(
        &l.ops[0],
        Op::PushClip {
            shape: Shape::RoundedRect(_)
        }
    ));
}

#[test]
fn shapes_are_rects_rounded_rects_or_ellipses_with_radii_clamped_to_the_box() {
    let plain = scene(&root_tree(boxed(40.0, 40.0, Paint::solid(red()))).0);
    assert!(matches!(
        &plain.ops[0],
        Op::Fill {
            shape: Shape::Rect(_),
            ..
        }
    ));

    let p = paint(|p| {
        p.fill = Some(Fill::Solid(red()));
        p.radius = [100.0, 5.0, 0.0, 5.0];
    });
    let l = scene(&root_tree(boxed(40.0, 20.0, p)).0);
    let Op::Fill {
        shape: Shape::RoundedRect(rr),
        ..
    } = &l.ops[0]
    else {
        panic!()
    };
    assert_eq!(rr.radii().top_left, 10.0, "clamped to half the short side");
    assert_eq!(rr.radii().top_right, 5.0);
    assert_eq!(rr.radii().bottom_right, 0.0);

    let p = paint(|p| {
        p.fill = Some(Fill::Solid(red()));
        p.shape = IrShape::Circle;
    });
    let l = scene(&root_tree(boxed(40.0, 40.0, p)).0);
    assert!(matches!(
        &l.ops[0],
        Op::Fill {
            shape: Shape::Ellipse(_),
            ..
        }
    ));
}

#[test]
fn gradients_become_peniko_gradients_over_the_box() {
    let stops = vec![
        GradientStop {
            offset: 0.0,
            color: Color::rgb(0, 0, 0),
        },
        GradientStop {
            offset: 1.0,
            color: Color::rgb(255, 255, 255),
        },
    ];
    let p = paint(|p| {
        p.fill = Some(Fill::Linear {
            angle: 180.0,
            stops: stops.clone(),
        })
    });
    let l = scene(&root_tree(boxed(100.0, 50.0, p)).0);
    let Op::Fill {
        brush: Brush::Gradient(g),
        ..
    } = &l.ops[0]
    else {
        panic!()
    };
    let peniko::GradientKind::Linear(line) = g.kind else {
        panic!("{:?}", g.kind)
    };
    // Top to bottom across the box (the root is at the origin).
    assert!(
        (line.start.y - 0.0).abs() < 1e-3 && (line.end.y - 50.0).abs() < 1e-3,
        "{line:?}"
    );
    assert!((line.start.x - 50.0).abs() < 1e-3 && (line.end.x - 50.0).abs() < 1e-3);

    let p = paint(|p| p.fill = Some(Fill::Radial { stops }));
    let l = scene(&root_tree(boxed(100.0, 50.0, p)).0);
    let Op::Fill {
        brush: Brush::Gradient(g),
        ..
    } = &l.ops[0]
    else {
        panic!()
    };
    assert!(matches!(g.kind, peniko::GradientKind::Radial(_)));
}

#[test]
fn borders_are_one_even_odd_ring_or_clipped_quads_for_different_sides() {
    let uniform = paint(|p| {
        p.border = [Some(Stroke {
            width: 2.0,
            color: Color::WHITE,
        }); 4]
    });
    let l = scene(&root_tree(boxed(50.0, 50.0, uniform)).0);
    assert_eq!(kinds(&l), ["Fill"]);
    assert!(matches!(
        &l.ops[0],
        Op::Fill {
            shape: Shape::Path { even_odd: true, .. },
            ..
        }
    ));

    let sides = paint(|p| {
        p.border = [
            Some(Stroke {
                width: 2.0,
                color: Color::WHITE,
            }),
            None,
            Some(Stroke {
                width: 1.0,
                color: red(),
            }),
            None,
        ];
    });
    let l = scene(&root_tree(boxed(50.0, 50.0, sides)).0);
    assert_eq!(
        kinds(&l),
        ["PushClip", "Fill", "Fill", "Pop"],
        "only the two sides that are set"
    );
    assert!(balanced(&l));

    let none = paint(|p| {
        p.border = [Some(Stroke {
            width: 0.0,
            color: Color::WHITE,
        }); 4]
    });
    assert!(scene(&root_tree(boxed(50.0, 50.0, none)).0).ops.is_empty());
}

fn text_node(s: &str, style: TextStyle) -> Node {
    Node::new(Kind::Text {
        text: s.into(),
        style,
    })
}

fn text_style() -> TextStyle {
    TextStyle {
        family: Arc::from("Noto Sans"),
        size: 14.0,
        ..TextStyle::default()
    }
}

#[test]
fn text_is_drawn_after_the_border_and_before_the_children() {
    let mut t = Tree::new("root", Node::container());
    let card = add_root(
        &mut t,
        "card",
        boxed(
            120.0,
            60.0,
            paint(|p| {
                p.fill = Some(Fill::Solid(red()));
                p.border = [Some(Stroke {
                    width: 1.0,
                    color: Color::WHITE,
                }); 4];
            }),
        ),
    );
    add(&mut t, card, "label", text_node("Play", text_style()));
    let l = scene(&t);
    // fill, border, then the child's text: a border never covers text.
    assert_eq!(kinds(&l), ["Fill", "Fill", "Glyphs"]);
    let Op::Glyphs { run, origin, brush } = &l.ops[2] else {
        panic!()
    };
    assert_eq!(run.glyphs.len(), 4);
    assert_eq!(
        (origin.x, origin.y),
        (1.0, 1.0),
        "the content box of the card"
    );
    assert!(matches!(brush, Brush::Solid(_)));
}

#[test]
fn bloom_wraps_each_glyph_run_in_its_own_filter_layer_then_draws_the_sharp_text() {
    let style = TextStyle {
        bloom: Some(Bloom {
            radius: 8.0,
            color: Color::rgb(0, 255, 120),
        }),
        wrap: true,
        ..text_style()
    };
    let mut t = Tree::new("root", Node::container());
    let col = add_root(&mut t, "col", boxed(60.0, 200.0, Paint::default()));
    add(
        &mut t,
        col,
        "t",
        text_node("one two three four five", style),
    );
    let l = scene(&t);
    let glyph_ops = l
        .ops
        .iter()
        .filter(|o| matches!(o, Op::Glyphs { .. }))
        .count();
    let filters = l
        .ops
        .iter()
        .filter(|o| matches!(o, Op::PushFilter { .. }))
        .count();
    assert!(filters >= 2, "several lines");
    assert_eq!(glyph_ops, filters * 2, "each run once blurred, once sharp");
    // Every filter layer holds exactly one primitive.
    for (i, op) in l.ops.iter().enumerate() {
        if let Op::PushFilter { sigma } = op {
            assert_eq!(*sigma, 4.0);
            assert!(matches!(l.ops[i + 1], Op::Glyphs { .. }), "{:?}", kinds(&l));
            assert!(matches!(l.ops[i + 2], Op::Pop));
        }
    }
    assert!(balanced(&l));
    // The blurred pass comes first.
    let first_sharp = l
        .ops
        .iter()
        .rposition(|o| matches!(o, Op::Pop))
        .expect("pop")
        + 1;
    assert!(
        l.ops[first_sharp..]
            .iter()
            .all(|o| matches!(o, Op::Glyphs { .. }))
    );
}

#[test]
fn text_with_max_lines_draws_only_the_kept_lines() {
    let style = TextStyle {
        wrap: true,
        max_lines: 2,
        ..text_style()
    };
    let mut t = Tree::new("root", Node::container());
    let col = add_root(&mut t, "col", boxed(80.0, 200.0, Paint::default()));
    add(
        &mut t,
        col,
        "t",
        text_node("one two three four five six seven eight nine ten", style),
    );
    let l = scene(&t);
    assert_eq!(kinds(&l).iter().filter(|k| **k == "Glyphs").count(), 2);
}

#[test]
fn opacity_wraps_the_node_and_its_children_in_one_layer() {
    let mut t = Tree::new("root", Node::container());
    let a = add_root(
        &mut t,
        "a",
        boxed(
            50.0,
            50.0,
            paint(|p| {
                p.fill = Some(Fill::Solid(red()));
                p.opacity = 0.5;
            }),
        ),
    );
    add(
        &mut t,
        a,
        "kid",
        boxed(10.0, 10.0, Paint::solid(Color::WHITE)),
    );
    add_root(&mut t, "b", boxed(10.0, 10.0, Paint::solid(Color::WHITE)));
    let l = scene(&t);
    assert_eq!(kinds(&l), ["PushOpacity", "Fill", "Fill", "Pop", "Fill"]);
    assert!(matches!(l.ops[0], Op::PushOpacity { alpha } if alpha == 0.5));
}

#[test]
fn a_visual_offset_moves_the_node_and_its_descendants_only() {
    let mut t = Tree::new("root", Node::container());
    let key = add_root(
        &mut t,
        "key",
        boxed(
            40.0,
            40.0,
            paint(|p| {
                p.fill = Some(Fill::Solid(red()));
                p.offset = (5.0, 7.0);
            }),
        ),
    );
    add(
        &mut t,
        key,
        "kid",
        boxed(10.0, 10.0, Paint::solid(Color::WHITE)),
    );
    add_root(
        &mut t,
        "next",
        boxed(10.0, 10.0, Paint::solid(Color::WHITE)),
    );
    let l = scene(&t);
    let origins: Vec<(f64, f64)> = l
        .ops
        .iter()
        .filter_map(|o| match o {
            Op::Fill {
                shape: Shape::Rect(r),
                ..
            } => Some((r.x0, r.y0)),
            _ => None,
        })
        .collect();
    // key and kid move by (5, 7); `next` sits below the *layout* box of key and does not.
    assert_eq!(origins, [(5.0, 7.0), (5.0, 7.0), (0.0, 40.0)]);
}

#[test]
fn images_are_referenced_by_id_and_a_key_used_twice_has_one_id() {
    let img = |s: ImageSource| {
        Node::new(Kind::Image {
            source: s,
            fit: Fit::Cover,
        })
        .with_layout(sized(30.0, 40.0))
    };
    let mut t = Tree::new("root", Node::container());
    add_root(&mut t, "a", img(ImageSource::Handle(Arc::from("cover:1"))));
    add_root(&mut t, "b", img(ImageSource::Handle(Arc::from("cover:1"))));
    add_root(
        &mut t,
        "c",
        img(ImageSource::Asset(Arc::from("assets/logo.png"))),
    );
    let l = scene(&t);
    assert_eq!(
        kinds(&l),
        [
            "PushClip", "Image", "Pop", "PushClip", "Image", "Pop", "PushClip", "Image", "Pop"
        ]
    );
    assert_eq!(
        l.images,
        [
            ImageKey::Handle(Arc::from("cover:1")),
            ImageKey::Asset(Arc::from("assets/logo.png")),
        ]
    );
    let ids: Vec<usize> = l
        .ops
        .iter()
        .filter_map(|o| match o {
            Op::Image { id, .. } => Some(id.index()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, [0, 0, 1]);
}

#[test]
fn noise_uses_a_repeating_image_brush_of_its_seed() {
    let p = paint(|p| {
        p.effects = vec![Effect::Noise {
            amount: 0.1,
            seed: 9,
        }]
    });
    let l = scene(&root_tree(boxed(50.0, 50.0, p)).0);
    assert_eq!(l.images, [ImageKey::Noise { seed: 9 }]);
    let Op::Fill {
        brush: Brush::Image { repeat, alpha, .. },
        ..
    } = &l.ops[0]
    else {
        panic!()
    };
    assert!(*repeat);
    assert_eq!(*alpha, 0.1);
    // The store hands out the tile without anyone filling it.
    let store = MemoryStore::new();
    let tile = store.get(&l.images[0]).expect("noise tile");
    assert_eq!((tile.width, tile.height), (128, 128));
    assert!(store.get(&ImageKey::Handle(Arc::from("x"))).is_none());
}

#[test]
fn a_memory_store_serves_what_was_put_in_it() {
    let mut s = MemoryStore::new();
    let key = ImageKey::Handle(Arc::from("cover:1"));
    s.insert(key.clone(), Image::new(2, 1, vec![0; 8]).expect("image"));
    assert_eq!(s.get(&key).map(|i| (i.width, i.height)), Some((2, 1)));
    assert!(Image::new(2, 2, vec![0; 8]).is_none(), "wrong byte count");
    assert!(Image::new(0, 1, vec![]).is_none());
}

#[test]
fn a_known_icon_with_a_solid_fill_is_a_filled_path_and_never_fills_its_box() {
    let icon = |name: &str, p: Paint| {
        Node::new(Kind::Icon(Arc::from(name)))
            .with_layout(sized(20.0, 20.0))
            .with_paint(p)
    };
    let l = scene(&root_tree(icon("play", Paint::solid(red()))).0);
    assert_eq!(kinds(&l), ["Fill"]);
    assert!(matches!(
        &l.ops[0],
        Op::Fill {
            shape: Shape::Path {
                even_odd: false,
                ..
            },
            brush: Brush::Solid(_)
        }
    ));
}

#[test]
fn an_unknown_icon_or_one_without_a_solid_fill_draws_nothing() {
    let icon = |name: &str, p: Paint| {
        Node::new(Kind::Icon(Arc::from(name)))
            .with_layout(sized(20.0, 20.0))
            .with_paint(p)
    };
    assert!(
        scene(&root_tree(icon("no-such-icon", Paint::solid(red()))).0)
            .ops
            .is_empty()
    );
    assert!(
        scene(&root_tree(icon("play", Paint::default())).0)
            .ops
            .is_empty()
    );
    let stops = vec![
        GradientStop {
            offset: 0.0,
            color: Color::BLACK,
        },
        GradientStop {
            offset: 1.0,
            color: Color::WHITE,
        },
    ];
    let grad = paint(|p| p.fill = Some(Fill::Linear { angle: 90.0, stops }));
    assert!(scene(&root_tree(icon("play", grad)).0).ops.is_empty());
}

#[test]
fn the_focused_node_draws_its_focus_paint_and_a_ring_after_its_children() {
    let mut node = boxed(60.0, 30.0, Paint::solid(Color::rgb(50, 50, 50)));
    node.focus_paint = Some(paint(|p| {
        p.fill = Some(Fill::Solid(red()));
        p.ring = Some(Stroke {
            width: 2.0,
            color: Color::WHITE,
        });
    }));
    let mut t = Tree::new("root", Node::container());
    let n = add_root(&mut t, "n", node);
    add(
        &mut t,
        n,
        "kid",
        boxed(5.0, 5.0, Paint::solid(Color::WHITE)),
    );

    let plain = scene(&t);
    assert_eq!(kinds(&plain), ["Fill", "Fill"]);

    let focused = scene_with(&t, &Scroll::none(), &SceneState::new().with_focus(n));
    assert_eq!(kinds(&focused), ["Fill", "Fill", "Fill"]);
    let Op::Fill {
        brush: Brush::Solid(c),
        ..
    } = &focused.ops[0]
    else {
        panic!()
    };
    assert_eq!(
        *c,
        peniko::Color::from_rgba8(200, 30, 30, 255),
        "the focus paint's fill"
    );
    assert!(matches!(
        &focused.ops[2],
        Op::Fill {
            shape: Shape::Path { even_odd: true, .. },
            ..
        }
    ));
}

#[test]
fn a_scroll_container_clips_its_descendants() {
    let mut t = Tree::new("root", Node::container());
    let list = add_root(
        &mut t,
        "list",
        boxed(100.0, 50.0, Paint::solid(Color::rgb(9, 9, 9))),
    );
    for i in 0..4 {
        let mut row = boxed(100.0, 30.0, Paint::solid(red()));
        row.layout.shrink = 0.0;
        add(&mut t, list, &format!("r{i}"), row);
    }
    let sc = Scroll::none().with(list, 0.0, 20.0);
    let l = scene_with(&t, &sc, &SceneState::new());
    assert_eq!(
        kinds(&l),
        ["Fill", "PushClip", "Fill", "Fill", "Fill", "Fill", "Pop"]
    );
    let Op::PushClip {
        shape: Shape::Rect(r),
    } = &l.ops[1]
    else {
        panic!()
    };
    assert_eq!((r.x0, r.y0, r.x1, r.y1), (0.0, 0.0, 100.0, 50.0));
    let Op::Fill {
        shape: Shape::Rect(first),
        ..
    } = &l.ops[2]
    else {
        panic!()
    };
    assert_eq!(first.y0, -20.0);
}

#[test]
fn scanlines_stay_bounded_in_a_huge_box() {
    let p = paint(|p| {
        p.effects = vec![Effect::Scanlines {
            period: 1.0,
            alpha: 0.3,
        }]
    });
    let mut t = Tree::new("root", Node::container());
    add_root(&mut t, "big", boxed(50.0, 1.0e6, p));
    let l = scene(&t);
    let Op::Fill {
        shape: Shape::Path { path, .. },
        ..
    } = &l.ops[1]
    else {
        panic!("{:?}", kinds(&l))
    };
    // Only the visible 300 px of lines, 5 elements per rectangle.
    assert!(
        path.elements().len() <= 5 * 301,
        "{}",
        path.elements().len()
    );
}

#[test]
fn building_is_deterministic_and_balanced_on_a_busy_tree() {
    let mut t = Tree::new("root", Node::container());
    for i in 0..30 {
        let p = paint(|p| {
            p.fill = Some(Fill::Solid(red()));
            p.opacity = if i % 3 == 0 { 0.5 } else { 1.0 };
            p.effects = vec![Effect::Shadow {
                dx: 0.0,
                dy: 2.0,
                blur: 4.0,
                spread: 0.0,
                color: Color::BLACK,
            }];
        });
        let n = add_root(&mut t, &format!("n{i}"), boxed(20.0, 10.0, p));
        add(&mut t, n, &format!("t{i}"), text_node("Hi", text_style()));
    }
    // The same layout twice (a new `FontSet` would hand out new blob ids).
    let laid = layout(
        &t,
        &mut FontSet::bundled(),
        Viewport::new(400.0, 300.0),
        &Scroll::none(),
    )
    .expect("layout");
    let a = build(&t, &laid, &SceneState::new());
    let b = build(&t, &laid, &SceneState::new());
    assert_eq!(a, b);
    assert!(balanced(&a));
    assert_eq!(a.summary().to_string(), b.summary().to_string());
}

#[test]
fn a_deep_tree_builds_without_recursion_trouble() {
    let mut t = Tree::new("root", Node::container());
    let mut parent = t.root();
    for i in 0..120 {
        parent = add(
            &mut t,
            parent,
            &format!("d{i}"),
            boxed(10.0, 10.0, Paint::solid(red())),
        );
    }
    let l = scene(&t);
    assert_eq!(l.ops.len(), 120);
}

#[test]
fn a_layout_of_another_tree_skips_nodes_it_did_not_place() {
    let mut big = Tree::new("root", Node::container());
    add_root(&mut big, "a", boxed(10.0, 10.0, Paint::solid(red())));
    add_root(&mut big, "b", boxed(10.0, 10.0, Paint::solid(red())));
    let small = Tree::new("root", Node::container());
    let laid = layout(
        &small,
        &mut FontSet::bundled(),
        Viewport::new(100.0, 100.0),
        &Scroll::none(),
    )
    .expect("layout");
    let l = build(&big, &laid, &SceneState::new());
    assert!(l.ops.is_empty());
}

#[test]
fn the_summary_counts_ops_by_kind() {
    let p = paint(|p| {
        p.fill = Some(Fill::Solid(red()));
        p.border = [Some(Stroke {
            width: 1.0,
            color: Color::WHITE,
        }); 4];
    });
    let s = scene(&root_tree(boxed(10.0, 10.0, p)).0).summary();
    assert_eq!(s.total, 2);
    assert_eq!(s.counts.get("Fill"), Some(&2));
    assert_eq!(s.to_string(), "total 2\nimages 0\nFill 2\n");
}

/// A renderer that records what it was asked to draw.
struct Recorder {
    seen: Vec<String>,
}

struct Pixels(u32, u32);

impl Surface for Pixels {
    fn size(&self) -> (u32, u32) {
        (self.0, self.1)
    }
}

#[derive(Debug)]
struct RenderFailed;

impl std::fmt::Display for RenderFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("render failed")
    }
}

impl std::error::Error for RenderFailed {}

impl Renderer for Recorder {
    type Surface = Pixels;
    type Error = RenderFailed;

    fn name(&self) -> &'static str {
        "recorder"
    }

    fn render(
        &mut self,
        list: &DisplayList,
        images: &dyn ImageStore,
        scale: f64,
        surface: &mut Pixels,
    ) -> Result<(), RenderFailed> {
        if scale <= 0.0 {
            return Err(RenderFailed);
        }
        surface.0 = (list.width * scale).ceil() as u32;
        surface.1 = (list.height * scale).ceil() as u32;
        for key in &list.images {
            self.seen
                .push(format!("image {}", images.get(key).is_some()));
        }
        Ok(())
    }
}

#[test]
fn a_renderer_gets_the_list_the_store_and_a_scale() {
    let p = paint(|p| {
        p.effects = vec![Effect::Noise {
            amount: 0.1,
            seed: 1,
        }]
    });
    let l = scene(&root_tree(boxed(50.0, 50.0, p)).0);
    let mut r = Recorder { seen: Vec::new() };
    let mut s = Pixels(0, 0);
    r.render(&l, &MemoryStore::new(), 2.0, &mut s)
        .expect("render");
    assert_eq!(s.size(), (800, 600));
    assert_eq!(r.seen, ["image true"]);
    assert_eq!(r.name(), "recorder");
    assert!(r.render(&l, &MemoryStore::new(), 0.0, &mut s).is_err());
}

#[test]
fn zero_radius_blurs_emit_nothing() {
    let p = paint(|p| {
        p.fill = Some(Fill::Solid(red()));
        p.effects = vec![
            Effect::Glow {
                layers: vec![
                    GlowLayer {
                        radius: 0.0,
                        color: red(),
                    },
                    GlowLayer {
                        radius: 6.0,
                        color: red(),
                    },
                ],
            },
            Effect::BackdropBlur { radius: 0.0 },
        ];
    });
    let l = scene(&root_tree(boxed(50.0, 50.0, p)).0);
    assert_eq!(
        kinds(&l),
        ["BlurRect", "Fill"],
        "one glow layer, no backdrop"
    );

    let style = TextStyle {
        bloom: Some(Bloom {
            radius: 0.0,
            color: Color::WHITE,
        }),
        ..text_style()
    };
    let mut t = Tree::new("root", Node::container());
    add_root(&mut t, "t", text_node("Hi", style));
    assert_eq!(kinds(&scene(&t)), ["Glyphs"], "only the sharp text");
}

#[test]
fn a_node_without_area_or_with_a_non_finite_paint_draws_nothing_of_its_own() {
    let mut t = Tree::new("root", Node::container());
    let flat = add_root(&mut t, "flat", boxed(0.0, 20.0, Paint::solid(red())));
    add(
        &mut t,
        flat,
        "kid",
        boxed(10.0, 10.0, Paint::solid(Color::WHITE)),
    );
    let nan = paint(|p| {
        p.fill = Some(Fill::Solid(red()));
        p.effects = vec![Effect::Shadow {
            dx: f32::NAN,
            dy: 0.0,
            blur: 4.0,
            spread: 0.0,
            color: Color::BLACK,
        }];
    });
    let bad = add_root(&mut t, "bad", boxed(20.0, 20.0, nan));
    add(
        &mut t,
        bad,
        "kid2",
        boxed(5.0, 5.0, Paint::solid(Color::WHITE)),
    );
    let l = scene(&t);
    assert_eq!(kinds(&l), ["Fill", "Fill"], "only the two children");
}

#[test]
fn open_layers_and_prefix_describe_what_a_backdrop_sits_in() {
    let mut t = Tree::new("root", Node::container());
    let scroller = add_root(&mut t, "list", boxed(100.0, 50.0, Paint::default()));
    add(
        &mut t,
        scroller,
        "glass",
        boxed(
            60.0,
            30.0,
            paint(|p| {
                p.opacity = 0.5;
                p.effects = vec![Effect::BackdropBlur { radius: 8.0 }];
            }),
        ),
    );
    let sc = Scroll::none().with(scroller, 0.0, 0.0);
    let l = scene_with(&t, &sc, &SceneState::new());
    assert_eq!(
        kinds(&l),
        ["PushClip", "PushOpacity", "Backdrop", "Pop", "Pop"]
    );
    let i = l
        .ops
        .iter()
        .position(|o| matches!(o, Op::Backdrop { .. }))
        .expect("backdrop");
    let open: Vec<&str> = l.open_layers(i).iter().map(|o| o.kind()).collect();
    assert_eq!(open, ["PushClip", "PushOpacity"], "outermost first");
    assert_eq!(l.prefix(i).len(), 2);
    assert_eq!(l.prefix(999).len(), l.ops.len());
    assert!(l.open_layers(0).is_empty());
    assert!(l.open_layers(l.ops.len()).is_empty());
}

#[test]
fn an_edited_run_is_clipped_and_draws_its_selection_preedit_and_caret() {
    const CARET: Color = Color([1, 2, 3, 255]);
    const BEHIND: Color = Color([4, 5, 6, 255]);
    let marks = |caret, selection, preedit| TextMarks {
        caret,
        selection,
        preedit,
        scroll: 0.0,
        caret_color: CARET,
        selection_color: BEHIND,
    };
    let list = |text: &str, marks: TextMarks| {
        let mut node = Node::text(text).with_layout(sized(80.0, 20.0));
        node.marks = Some(Box::new(marks));
        let mut tree = Tree::new("root", boxed(200.0, 100.0, Paint::default()));
        add_root(&mut tree, "text", node);
        scene_with(&tree, &Scroll::none(), &SceneState::new()).ops
    };
    // What is filled in a colour, left to right.
    let filled = |ops: &[Op], color: Color| -> Vec<(f64, f64)> {
        let want = format!(
            "{:?}",
            Brush::Solid(peniko::Color::from_rgba8(
                color.0[0], color.0[1], color.0[2], color.0[3],
            ))
        );
        ops.iter()
            .filter_map(|op| match op {
                Op::Fill {
                    shape: Shape::Rect(r),
                    brush,
                } if format!("{brush:?}") == want => Some((r.x0, r.x1)),
                _ => None,
            })
            .collect()
    };
    let glyph_x = |ops: &[Op]| {
        ops.iter()
            .find_map(|op| match op {
                Op::Glyphs { origin, .. } => Some(origin.x),
                _ => None,
            })
            .expect("glyphs")
    };

    let ops = list("abcdef", marks(Some(3), 1..3, Some(4..6)));
    assert!(matches!(ops.first(), Some(Op::PushClip { .. })), "{ops:?}");
    assert!(matches!(ops.last(), Some(Op::Pop)), "{ops:?}");
    let selection = filled(&ops, BEHIND);
    let [under, caret] = filled(&ops, CARET)[..] else {
        panic!("an underline and a caret: {ops:?}");
    };
    assert_eq!(selection.len(), 1);
    assert!(
        (selection[0].1 - caret.0).abs() < 1.0,
        "{selection:?} {caret:?}"
    );
    assert!(under.0 > caret.0 && caret.1 - caret.0 < 2.0);
    assert_eq!(glyph_x(&ops), 0.0, "short text does not move");

    let long = "m".repeat(40);
    let ops = list(&long, marks(Some(long.len()), 0..0, None));
    assert!(glyph_x(&ops) < 0.0, "it moves to show the caret");
    let caret = filled(&ops, CARET)[0];
    assert!(caret.1 <= 80.01 && caret.0 > 70.0, "{caret:?}");
    assert!(filled(&list("abc", marks(None, 0..0, None)), CARET).is_empty());
}
