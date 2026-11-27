//! IR → boxes on hand-built trees: one test per rule of the crate docs.

use std::sync::Arc;

use gogo_ui_ir::{
    Align, Color, Dim, Kind, LayoutStyle, Node, NodeId, Paint, Position, ScrollSpec, Sides, Stroke,
    TextAlign, TextStyle, Track, Tree,
};
use gogo_ui_layout::{LayoutError, MIN_THUMB, Rect, Scroll, Viewport, layout};
use gogo_ui_text::FontSet;

fn lay(tree: &Tree, w: f32, h: f32) -> gogo_ui_layout::Laid {
    layout(
        tree,
        &mut FontSet::bundled(),
        Viewport::new(w, h),
        &Scroll::none(),
    )
    .expect("layout")
}

fn ls(mut base: LayoutStyle, f: impl FnOnce(&mut LayoutStyle)) -> LayoutStyle {
    f(&mut base);
    base
}

fn px(w: f32, h: f32) -> LayoutStyle {
    ls(LayoutStyle::default(), |s| {
        s.width = Dim::Px(w);
        s.height = Dim::Px(h);
    })
}

fn boxed(layout: LayoutStyle) -> Node {
    Node::container().with_layout(layout)
}

fn add(t: &mut Tree, parent: NodeId, key: &str, node: Node) -> NodeId {
    t.add_child(parent, key, node).expect("add child")
}

fn add_root(t: &mut Tree, key: &str, node: Node) -> NodeId {
    let root = t.root();
    add(t, root, key, node)
}

fn rect(l: &gogo_ui_layout::Laid, id: NodeId) -> (f64, f64, f64, f64) {
    let r = l.rect(id).expect("rect");
    (r.x0, r.y0, r.width(), r.height())
}

fn text(s: &str, size: f32) -> Node {
    Node::new(Kind::Text {
        text: s.to_string(),
        style: TextStyle {
            family: Arc::from("Noto Sans"),
            size,
            ..TextStyle::default()
        },
    })
}

fn text_with(s: &str, style: TextStyle) -> Node {
    Node::new(Kind::Text {
        text: s.to_string(),
        style,
    })
}

#[test]
fn the_root_fills_the_viewport_and_children_stack_in_a_column() {
    let mut t = Tree::new("root", boxed(LayoutStyle::default()));
    let a = add_root(&mut t, "a", boxed(px(100.0, 30.0)));
    let b = add_root(&mut t, "b", boxed(px(60.0, 20.0)));
    let l = lay(&t, 400.0, 300.0);
    assert_eq!(rect(&l, t.root()), (0.0, 0.0, 400.0, 300.0));
    assert_eq!(rect(&l, a), (0.0, 0.0, 100.0, 30.0));
    assert_eq!(rect(&l, b), (0.0, 30.0, 60.0, 20.0));
    assert_eq!(l.size(), (400.0, 300.0));
}

#[test]
fn a_row_with_gap_padding_and_grow_places_boxes_absolutely() {
    let mut t = Tree::new(
        "root",
        boxed(ls(LayoutStyle::row(), |s| {
            s.padding = Sides::all(10.0);
            s.column_gap = 8.0;
        })),
    );
    let side = add_root(&mut t, "side", boxed(px(50.0, 40.0)));
    let main = add_root(
        &mut t,
        "main",
        boxed(ls(LayoutStyle::default(), |s| {
            s.grow = 1.0;
            s.height = Dim::Px(40.0);
        })),
    );
    let inner = add(&mut t, main, "inner", boxed(px(20.0, 10.0)));
    let l = lay(&t, 400.0, 200.0);
    assert_eq!(rect(&l, side), (10.0, 10.0, 50.0, 40.0));
    assert_eq!(rect(&l, main), (68.0, 10.0, 322.0, 40.0));
    // A child's box is absolute: the parent's origin is included.
    assert_eq!(rect(&l, inner), (68.0, 10.0, 20.0, 10.0));
}

#[test]
fn a_border_takes_layout_space_inside_the_box() {
    let bordered = Node::container()
        .with_layout(ls(px(100.0, 50.0), |s| {
            s.padding = Sides::all(4.0);
        }))
        .with_paint(Paint {
            border: [Some(Stroke {
                width: 2.0,
                color: Color::WHITE,
            }); 4],
            ..Paint::default()
        });
    let mut t = Tree::new("root", boxed(LayoutStyle::default()));
    let b = add_root(&mut t, "b", bordered);
    let kid = add(
        &mut t,
        b,
        "kid",
        boxed(ls(LayoutStyle::default(), |s| {
            s.grow = 1.0;
        })),
    );
    let l = lay(&t, 300.0, 200.0);
    // The size stays 100x50 (border-box); the content starts 6 px in.
    assert_eq!(rect(&l, b), (0.0, 0.0, 100.0, 50.0));
    assert_eq!(l.content(b), Some(Rect::new(6.0, 6.0, 94.0, 44.0)));
    assert_eq!(rect(&l, kid), (6.0, 6.0, 88.0, 38.0));
}

#[test]
fn percent_sizes_resolve_against_the_parent() {
    let mut t = Tree::new("root", boxed(LayoutStyle::default()));
    let a = add_root(
        &mut t,
        "a",
        boxed(ls(LayoutStyle::default(), |s| {
            s.width = Dim::Pct(50.0);
            s.height = Dim::Px(10.0);
        })),
    );
    let l = lay(&t, 400.0, 100.0);
    assert_eq!(rect(&l, a), (0.0, 0.0, 200.0, 10.0));
}

#[test]
fn absolute_nodes_leave_the_flow_and_sit_at_their_insets() {
    let mut t = Tree::new("root", boxed(LayoutStyle::default()));
    let flow = add_root(&mut t, "flow", boxed(px(50.0, 50.0)));
    let abs = add_root(
        &mut t,
        "abs",
        boxed(ls(px(30.0, 20.0), |s| {
            s.position = Position::Absolute;
            s.inset = Sides {
                top: Dim::Px(5.0),
                right: Dim::Px(7.0),
                bottom: Dim::Auto,
                left: Dim::Auto,
            };
        })),
    );
    let after = add_root(&mut t, "after", boxed(px(50.0, 10.0)));
    let l = lay(&t, 200.0, 200.0);
    assert_eq!(rect(&l, abs), (163.0, 5.0, 30.0, 20.0));
    // `abs` takes no space in the column.
    assert_eq!(rect(&l, flow).1 + rect(&l, flow).3, rect(&l, after).1);
}

#[test]
fn an_inset_on_an_in_flow_node_is_ignored() {
    let mut t = Tree::new("root", boxed(LayoutStyle::default()));
    let a = add_root(
        &mut t,
        "a",
        boxed(ls(px(30.0, 20.0), |s| {
            s.position = Position::Relative;
            s.inset = Sides::all(Dim::Px(40.0));
        })),
    );
    let l = lay(&t, 200.0, 200.0);
    assert_eq!(rect(&l, a), (0.0, 0.0, 30.0, 20.0));
}

#[test]
fn a_text_leaf_is_measured_and_its_box_hugs_the_text() {
    let mut t = Tree::new(
        "root",
        boxed(ls(LayoutStyle::row(), |s| {
            s.align_items = Some(Align::Start)
        })),
    );
    let label = add_root(&mut t, "label", text("Library", 16.0));
    let l = lay(&t, 400.0, 100.0);
    let (_, _, w, h) = rect(&l, label);
    let tl = l.text(label).expect("text layout");
    assert_eq!(w, f64::from(tl.width().ceil()));
    assert_eq!(h, f64::from(tl.height().ceil()));
    assert!(w > 30.0 && h > 15.0, "{w} {h}");
    assert_eq!(l.content(label), l.rect(label));
}

#[test]
fn wrapped_text_takes_the_width_it_is_offered_and_grows_taller() {
    let long = "alpha beta gamma delta epsilon zeta eta theta iota kappa";
    let wrap = TextStyle {
        family: Arc::from("Noto Sans"),
        size: 14.0,
        wrap: true,
        ..TextStyle::default()
    };
    let mut t = Tree::new("root", boxed(LayoutStyle::default()));
    let narrow = add_root(&mut t, "narrow", boxed(px(120.0, 400.0)));
    let para = add(&mut t, narrow, "para", text_with(long, wrap.clone()));
    let l = lay(&t, 400.0, 400.0);
    let tl = l.text(para).expect("text");
    assert!(tl.line_count() >= 4, "{}", tl.line_count());
    let (_, _, w, h) = rect(&l, para);
    assert_eq!(w, 120.0, "a column stretches the paragraph to its width");
    assert_eq!(h, f64::from(tl.height().ceil()));

    let mut t2 = Tree::new("root", boxed(LayoutStyle::default()));
    let wide = add_root(&mut t2, "wide", boxed(px(390.0, 400.0)));
    let para2 = add(&mut t2, wide, "para", text_with(long, wrap));
    let l2 = lay(&t2, 400.0, 400.0);
    assert!(rect(&l2, para2).3 < h);
    assert_eq!(l2.text(para2).expect("text").line_count(), 1);
}

#[test]
fn max_lines_cuts_with_an_ellipsis_and_the_box_keeps_that_height() {
    let st = TextStyle {
        family: Arc::from("Noto Sans"),
        size: 14.0,
        wrap: true,
        max_lines: 2,
        ..TextStyle::default()
    };
    let mut t = Tree::new("root", boxed(LayoutStyle::default()));
    let col = add_root(&mut t, "col", boxed(px(100.0, 300.0)));
    let p = add(
        &mut t,
        col,
        "p",
        text_with(
            "one two three four five six seven eight nine ten eleven twelve",
            st,
        ),
    );
    let l = lay(&t, 300.0, 300.0);
    let tl = l.text(p).expect("text");
    assert_eq!(tl.line_count(), 2);
    assert!(tl.is_truncated());
    assert!(tl.text().ends_with('…'));
    assert_eq!(rect(&l, p).3, f64::from(tl.height().ceil()));
}

#[test]
fn centred_text_is_aligned_in_its_boxs_width() {
    let st = TextStyle {
        family: Arc::from("Noto Sans"),
        size: 14.0,
        align: TextAlign::Center,
        ..TextStyle::default()
    };
    let mut t = Tree::new("root", boxed(LayoutStyle::default()));
    let col = add_root(&mut t, "col", boxed(px(200.0, 50.0)));
    let p = add(&mut t, col, "p", text_with("Hi", st));
    let l = lay(&t, 300.0, 100.0);
    assert_eq!(rect(&l, p).2, 200.0);
    let line = &l.text(p).expect("text").lines()[0];
    assert!(line.left > 80.0 && line.left < 120.0, "{}", line.left);
}

#[test]
fn a_grid_places_cells_by_track_and_span() {
    let mut t = Tree::new("root", boxed(LayoutStyle::default()));
    let grid = add_root(
        &mut t,
        "grid",
        boxed(ls(
            LayoutStyle::grid(vec![Track::Px(100.0), Track::Fr(1.0), Track::Fr(2.0)]),
            |s| {
                s.column_gap = 10.0;
                s.width = Dim::Pct(100.0);
            },
        )),
    );
    let a = add(
        &mut t,
        grid,
        "a",
        boxed(ls(LayoutStyle::default(), |s| {
            s.height = Dim::Px(20.0);
        })),
    );
    let b = add(
        &mut t,
        grid,
        "b",
        boxed(ls(LayoutStyle::default(), |s| {
            s.height = Dim::Px(20.0);
        })),
    );
    let c = add(
        &mut t,
        grid,
        "c",
        boxed(ls(LayoutStyle::default(), |s| {
            s.height = Dim::Px(20.0);
        })),
    );
    let wide = add(
        &mut t,
        grid,
        "wide",
        boxed(ls(LayoutStyle::default(), |s| {
            s.height = Dim::Px(20.0);
            s.col_span = 3;
        })),
    );
    let l = lay(&t, 430.0, 100.0);
    // 430 - 100 - 2*10 = 310 left for 1fr + 2fr.
    let (ax, _, aw, _) = rect(&l, a);
    let (bx, _, bw, _) = rect(&l, b);
    let (cx, _, cw, _) = rect(&l, c);
    assert_eq!((ax, aw), (0.0, 100.0));
    assert_eq!(bx, 110.0);
    assert!((bw - 103.0).abs() <= 1.0, "{bw}");
    assert_eq!(cx, bx + bw + 10.0);
    assert!((cw - 207.0).abs() <= 1.0, "{cw}");
    let (wx, wy, ww, _) = rect(&l, wide);
    assert_eq!((wx, ww), (0.0, 430.0));
    assert_eq!(wy, 20.0, "the spanning node starts the next row");
}

#[test]
fn a_layer_stacks_every_child_in_the_one_cell() {
    let mut t = Tree::new("root", boxed(LayoutStyle::grid(vec![Track::Fr(1.0)])));
    let layer_style = |w: Dim| {
        ls(LayoutStyle::default(), |s| {
            s.width = w;
            s.height = Dim::Px(40.0);
            s.cell = Some((1, 1));
        })
    };
    let bottom = add_root(&mut t, "bottom", boxed(layer_style(Dim::Auto)));
    let top = add_root(&mut t, "top", boxed(layer_style(Dim::Px(30.0))));
    let l = lay(&t, 200.0, 100.0);
    assert_eq!(rect(&l, bottom).0, 0.0);
    assert_eq!(rect(&l, top).0, 0.0);
    assert_eq!(rect(&l, top).1, rect(&l, bottom).1);
    // The later one is on top.
    assert_eq!(l.hit(10.0, 10.0), Some(top));
    assert_eq!(l.hit(100.0, 10.0), Some(bottom));
}

#[test]
fn a_row_wraps_when_it_is_told_to() {
    let mut t = Tree::new("root", boxed(LayoutStyle::default()));
    let row = add_root(
        &mut t,
        "row",
        boxed(ls(LayoutStyle::row(), |s| {
            s.wrap = true;
            s.width = Dim::Pct(100.0);
        })),
    );
    let ids: Vec<NodeId> = (0..5)
        .map(|i| add(&mut t, row, &format!("k{i}"), boxed(px(60.0, 20.0))))
        .collect();
    let l = lay(&t, 200.0, 200.0);
    assert_eq!(rect(&l, ids[2]).1, 0.0);
    assert_eq!(rect(&l, ids[3]).1, 20.0);
    assert_eq!(
        rect(&l, row).3,
        40.0,
        "the container grows to its two lines"
    );
}

#[test]
fn hit_finds_the_topmost_node_and_nothing_outside() {
    let mut t = Tree::new("root", boxed(LayoutStyle::default()));
    let a = add_root(&mut t, "a", boxed(px(100.0, 100.0)));
    let kid = add(&mut t, a, "kid", boxed(px(20.0, 20.0)));
    let b = add_root(&mut t, "b", boxed(px(100.0, 100.0)));
    let l = lay(&t, 300.0, 300.0);
    assert_eq!(l.hit(5.0, 5.0), Some(kid), "a child is above its parent");
    assert_eq!(l.hit(50.0, 50.0), Some(a));
    assert_eq!(l.hit(50.0, 150.0), Some(b));
    assert_eq!(l.hit(200.0, 250.0), Some(t.root()));
    assert_eq!(l.hit(300.0, 10.0), None, "the right edge is outside");
    assert_eq!(l.hit(-1.0, 10.0), None);
    assert_eq!(l.hit(f64::NAN, 10.0), None);
}

#[test]
fn neighbours_never_both_claim_the_shared_edge() {
    let mut t = Tree::new("root", boxed(LayoutStyle::row()));
    let a = add_root(&mut t, "a", boxed(px(50.0, 50.0)));
    let b = add_root(&mut t, "b", boxed(px(50.0, 50.0)));
    let l = lay(&t, 200.0, 100.0);
    assert_eq!(l.hit(49.0, 10.0), Some(a));
    assert_eq!(l.hit(50.0, 10.0), Some(b));
}

#[test]
fn the_order_is_document_order_even_when_nodes_were_added_out_of_order() {
    let mut t = Tree::new("root", boxed(LayoutStyle::default()));
    let a = add_root(&mut t, "a", boxed(px(10.0, 10.0)));
    let b = add_root(&mut t, "b", boxed(px(10.0, 10.0)));
    // Added last, but it is a's child, so it comes between a and b.
    let late = add(&mut t, a, "late", boxed(px(5.0, 5.0)));
    let l = lay(&t, 100.0, 100.0);
    assert_eq!(l.order(), &[t.root(), a, late, b]);
    assert_eq!(rect(&l, late), (0.0, 0.0, 5.0, 5.0));
    assert_eq!(rect(&l, b), (0.0, 10.0, 10.0, 10.0));
}

#[test]
fn scroll_moves_descendants_and_clips_hits_to_the_container() {
    let mut t = Tree::new("root", boxed(LayoutStyle::default()));
    let list = add_root(&mut t, "list", boxed(px(100.0, 50.0)));
    let rows: Vec<NodeId> = (0..5)
        .map(|i| {
            add(
                &mut t,
                list,
                &format!("row{i}"),
                boxed(ls(px(100.0, 30.0), |s| s.shrink = 0.0)),
            )
        })
        .collect();
    let below = add_root(&mut t, "below", boxed(px(100.0, 100.0)));
    let mut fonts = FontSet::bundled();
    let still =
        layout(&t, &mut fonts, Viewport::new(200.0, 300.0), &Scroll::none()).expect("layout");
    assert_eq!(still.hit(10.0, 40.0), Some(rows[1]));

    let sc = Scroll::none().with(list, 0.0, 40.0);
    let l = layout(&t, &mut fonts, Viewport::new(200.0, 300.0), &sc).expect("layout");
    assert_eq!(
        rect(&l, list),
        (0.0, 0.0, 100.0, 50.0),
        "the container stays"
    );
    assert_eq!(rect(&l, rows[0]).1, -40.0);
    assert_eq!(rect(&l, rows[2]).1, 20.0);
    assert_eq!(l.hit(10.0, 10.0), Some(rows[1]));
    // Rows that overflow the container are not hit below it; what is there is `below`.
    assert_eq!(l.hit(10.0, 60.0), Some(below));
    // The overflowing row is still placed, just not visible there.
    let r4 = l.get(rows[4]).expect("placed");
    assert_eq!(r4.rect.y0, 80.0);
    assert_eq!(r4.visible.height(), 0.0);
}

#[test]
fn scroll_offsets_nest() {
    let mut t = Tree::new("root", boxed(LayoutStyle::default()));
    let outer = add_root(&mut t, "outer", boxed(px(100.0, 100.0)));
    let inner = add(&mut t, outer, "inner", boxed(px(100.0, 100.0)));
    let leaf = add(&mut t, inner, "leaf", boxed(px(10.0, 10.0)));
    let sc = Scroll::none().with(outer, 0.0, 10.0).with(inner, 5.0, 20.0);
    let l = layout(
        &t,
        &mut FontSet::bundled(),
        Viewport::new(200.0, 200.0),
        &sc,
    )
    .expect("layout");
    assert_eq!(rect(&l, inner), (0.0, -10.0, 100.0, 100.0));
    assert_eq!(rect(&l, leaf), (-5.0, -30.0, 10.0, 10.0));
}

#[test]
fn a_non_finite_scroll_offset_counts_as_zero() {
    let mut t = Tree::new("root", boxed(LayoutStyle::default()));
    let list = add_root(&mut t, "list", boxed(px(100.0, 50.0)));
    let row = add(&mut t, list, "row", boxed(px(100.0, 30.0)));
    let sc = Scroll::none().with(list, f32::NAN, f32::INFINITY);
    let l = layout(
        &t,
        &mut FontSet::bundled(),
        Viewport::new(200.0, 300.0),
        &sc,
    )
    .expect("layout");
    assert_eq!(rect(&l, row), (0.0, 0.0, 100.0, 30.0));
}

#[test]
fn a_visual_offset_never_moves_a_box_or_its_hit_area() {
    let mut t = Tree::new("root", boxed(LayoutStyle::default()));
    let key = add_root(
        &mut t,
        "key",
        boxed(px(40.0, 40.0)).with_paint(Paint {
            offset: (20.0, 20.0),
            ..Paint::default()
        }),
    );
    let l = lay(&t, 200.0, 200.0);
    assert_eq!(rect(&l, key), (0.0, 0.0, 40.0, 40.0));
    assert_eq!(l.hit(5.0, 5.0), Some(key));
}

#[test]
fn layout_is_deterministic_and_the_font_set_is_reusable() {
    let mut t = Tree::new("root", boxed(LayoutStyle::default()));
    for i in 0..20 {
        add_root(
            &mut t,
            &format!("t{i}"),
            text(&format!("Game number {i}"), 14.0),
        );
    }
    let mut fonts = FontSet::bundled();
    let a = layout(&t, &mut fonts, Viewport::new(300.0, 600.0), &Scroll::none()).expect("a");
    let b = layout(&t, &mut fonts, Viewport::new(300.0, 600.0), &Scroll::none()).expect("b");
    for id in t.iter() {
        assert_eq!(a.rect(id), b.rect(id));
    }
}

#[test]
fn a_big_flat_tree_lays_out() {
    let mut t = Tree::new("root", boxed(LayoutStyle::default()));
    for i in 0..20_000 {
        add_root(&mut t, &format!("n{i}"), boxed(px(10.0, 2.0)));
    }
    let l = lay(&t, 100.0, 100.0);
    assert_eq!(l.order().len(), 20_001);
}

#[test]
fn a_lone_root_lays_out() {
    let t = Tree::new("root", Node::container());
    let l = lay(&t, 10.0, 10.0);
    assert_eq!(l.order(), &[t.root()]);
    assert_eq!(rect(&l, t.root()), (0.0, 0.0, 10.0, 10.0));
}

#[test]
fn an_id_from_another_tree_is_not_placed() {
    let mut big = Tree::new("root", Node::container());
    let far = add_root(&mut big, "x", Node::container());
    let small = Tree::new("root", Node::container());
    let l = lay(&small, 10.0, 10.0);
    assert!(l.rect(far).is_none());
    assert!(l.text(far).is_none());
}

#[test]
fn bad_input_is_an_error_not_a_panic() {
    let mut fonts = FontSet::bundled();
    let t = Tree::new("root", Node::container());
    for (w, h) in [(f32::NAN, 10.0), (10.0, f32::INFINITY), (-1.0, 10.0)] {
        let e = layout(&t, &mut fonts, Viewport::new(w, h), &Scroll::none());
        assert_eq!(e.err(), Some(LayoutError::BadViewport), "{w} {h}");
    }
    let mut t = Tree::new("root", Node::container());
    add_root(
        &mut t,
        "bad",
        boxed(ls(LayoutStyle::default(), |s| {
            s.width = Dim::Px(f32::NAN);
        })),
    );
    let e = layout(&t, &mut fonts, Viewport::new(10.0, 10.0), &Scroll::none()).err();
    assert_eq!(
        e,
        Some(LayoutError::NotFinite {
            key: "bad".into(),
            field: "width"
        })
    );
    let mut t = Tree::new("root", Node::container());
    add_root(
        &mut t,
        "gap",
        boxed(ls(LayoutStyle::default(), |s| {
            s.row_gap = f32::INFINITY;
        })),
    );
    assert!(layout(&t, &mut fonts, Viewport::new(10.0, 10.0), &Scroll::none()).is_err());
}

#[test]
fn huge_and_zero_sizes_do_not_panic() {
    let mut t = Tree::new("root", boxed(LayoutStyle::default()));
    add_root(&mut t, "huge", boxed(px(1.0e30, 1.0e30)));
    add_root(&mut t, "zero", boxed(px(0.0, 0.0)));
    add_root(&mut t, "t", text("zero width text", 14.0));
    let l = lay(&t, 0.0, 0.0);
    assert_eq!(l.size(), (0.0, 0.0));
    let l = lay(&t, 100.0, 100.0);
    assert_eq!(l.order().len(), 4);
}

#[test]
fn nesting_up_to_the_cap_lays_out_and_deeper_is_an_error() {
    let build = |depth: usize| {
        let mut t = Tree::new("root", Node::container());
        let mut parent = t.root();
        for i in 1..depth {
            parent = add(&mut t, parent, &format!("d{i}"), Node::container());
        }
        add(&mut t, parent, "leaf", text("deep", 12.0));
        t
    };
    // The leaf is one level below the last container.
    let ok = build(gogo_ui_layout::MAX_DEPTH - 1);
    let l = layout(
        &ok,
        &mut FontSet::bundled(),
        Viewport::new(100.0, 100.0),
        &Scroll::none(),
    )
    .expect("at the cap");
    assert_eq!(l.order().len(), gogo_ui_layout::MAX_DEPTH);
    let deep = build(gogo_ui_layout::MAX_DEPTH + 1);
    let e = layout(
        &deep,
        &mut FontSet::bundled(),
        Viewport::new(100.0, 100.0),
        &Scroll::none(),
    );
    assert_eq!(e.err(), Some(LayoutError::TooDeep));
}

#[test]
fn many_text_leaves_in_nested_rows_lay_out() {
    let mut t = Tree::new("root", Node::container());
    for i in 0..300 {
        let row = add_root(&mut t, &format!("row{i}"), boxed(LayoutStyle::row()));
        let inner = add(
            &mut t,
            row,
            &format!("in{i}"),
            boxed(LayoutStyle::default()),
        );
        add(
            &mut t,
            inner,
            &format!("t{i}"),
            text(&format!("Game {i}"), 13.0),
        );
    }
    let l = lay(&t, 400.0, 600.0);
    assert_eq!(l.order().len(), 901);
}

/// A column of `n` rows 30 px high in a scroll container that gets what the root has left
/// under a 40 px header.
fn scrolling(n: usize, spec: ScrollSpec) -> (Tree, NodeId, Vec<NodeId>) {
    let mut t = Tree::new("root", boxed(LayoutStyle::default()));
    add_root(
        &mut t,
        "head",
        boxed(ls(px(100.0, 40.0), |s| s.shrink = 0.0)),
    );
    let mut list = boxed(ls(LayoutStyle::default(), |s| s.grow = 1.0));
    list.scroll = Some(spec);
    let list = add_root(&mut t, "list", list);
    let rows = (0..n)
        .map(|i| {
            let row = ls(LayoutStyle::default(), |s| {
                s.height = Dim::Px(30.0);
                s.shrink = 0.0;
            });
            add(&mut t, list, &format!("row{i}"), boxed(row))
        })
        .collect();
    (t, list, rows)
}

#[test]
fn a_scroll_container_keeps_its_room_and_reports_its_content() {
    let (t, list, rows) = scrolling(10, ScrollSpec::default());
    let l = lay(&t, 200.0, 140.0);
    assert_eq!(rect(&l, list), (0.0, 40.0, 200.0, 100.0));
    let b = *l.scroll_box(list).expect("a scroll box");
    assert_eq!((b.offset, b.max), (0.0, 200.0));
    // The gutter is on the right, and the content is laid out and seen beside it.
    assert_eq!(b.viewport, Rect::new(0.0, 40.0, 192.0, 140.0));
    assert_eq!(b.track, Rect::new(192.0, 40.0, 200.0, 140.0));
    assert_eq!(rect(&l, rows[0]), (0.0, 40.0, 192.0, 30.0));
    // A third of the content shows, so the thumb is a third of the track.
    assert_eq!(b.thumb.y0, 40.0);
    assert!((b.thumb.height() - 100.0 / 3.0).abs() < 0.01);
    // The gutter belongs to the container, never to what scrolls under it.
    assert_eq!(l.hit(196.0, 60.0), Some(list));
}

#[test]
fn a_scroll_offset_is_clamped_to_the_content() {
    let (t, list, rows) = scrolling(10, ScrollSpec::default());
    let mut fonts = FontSet::bundled();
    let mut at = |y: f32| {
        let sc = Scroll::none().with(list, 30.0, y);
        layout(&t, &mut fonts, Viewport::new(200.0, 140.0), &sc).expect("layout")
    };
    let end = at(10_000.0);
    let b = *end.scroll_box(list).expect("a scroll box");
    assert_eq!(b.offset, 200.0);
    assert_eq!(b.thumb.y1, b.track.y1);
    assert_eq!(
        rect(&end, rows[9]),
        (0.0, 110.0, 192.0, 30.0),
        "x is ignored"
    );
    assert_eq!(at(-50.0).scroll_box(list).expect("a box").offset, 0.0);
    // A row scrolled out above the container is not seen under the header.
    assert_eq!(end.get(rows[0]).expect("placed").visible.height(), 0.0);
}

#[test]
fn what_a_column_scrolls_is_not_squeezed_to_fit() {
    let (mut t, list, rows) = scrolling(10, ScrollSpec::default());
    for &row in &rows {
        t.node_mut(row).expect("a row").layout.shrink = 1.0;
    }
    let l = lay(&t, 200.0, 140.0);
    assert_eq!(rect(&l, rows[9]).3, 30.0);
    assert_eq!(l.scroll_box(list).expect("a box").max, 200.0);
}

#[test]
fn content_that_fits_does_not_scroll() {
    let (t, list, _) = scrolling(2, ScrollSpec::default());
    let sc = Scroll::none().with(list, 0.0, 40.0);
    let l = layout(
        &t,
        &mut FontSet::bundled(),
        Viewport::new(200.0, 140.0),
        &sc,
    )
    .expect("layout");
    let b = *l.scroll_box(list).expect("a scroll box");
    assert_eq!((b.offset, b.max), (0.0, 0.0));
    assert_eq!(b.thumb.height(), 0.0);
}

#[test]
fn a_long_list_has_a_thumb_of_the_smallest_length() {
    let (t, list, _) = scrolling(1000, ScrollSpec::default());
    let l = lay(&t, 200.0, 140.0);
    assert_eq!(l.scroll_box(list).expect("a box").thumb.height(), MIN_THUMB);
}

#[test]
fn a_scroll_container_clips_inside_its_border() {
    let (mut t, list, rows) = scrolling(10, ScrollSpec::default());
    let line = Some(Stroke {
        color: Color::rgb(1, 1, 1),
        width: 2.0,
    });
    t.node_mut(list).expect("the list").paint.border = [line; 4];
    let l = lay(&t, 200.0, 140.0);
    let b = *l.scroll_box(list).expect("a scroll box");
    assert_eq!(b.viewport, Rect::new(2.0, 42.0, 190.0, 138.0));
    assert_eq!(b.max, 204.0);
    assert_eq!(l.get(rows[0]).expect("placed").visible.y0, 42.0);
    assert_eq!(l.hit(10.0, 41.0), Some(list), "the border is not a row");
}
