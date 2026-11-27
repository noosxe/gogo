//! IR + layout → display list, with the effect lowering of design 28 §2.

use std::collections::HashMap;
use std::sync::Arc;

use gogo_ui_ir as ir;
use gogo_ui_ir::{Effect, Fill, ImageSource, Kind, Node, NodeId, Paint, Tree};
use gogo_ui_layout::{Laid, Placed};
use gogo_ui_text::TextRect;
use kurbo::{
    Affine, BezPath, Ellipse, Point, Rect, RoundedRect, RoundedRectRadii, Shape as _, Vec2,
};
use peniko::{Color, ColorStop, Gradient};

use crate::{Brush, DisplayList, ImageId, ImageKey, Op, Shape, icons};

/// The upper bound on scanline fills per node, so a tall box with a period of 1 cannot make
/// the list huge.
const MAX_SCANLINES: usize = 8192;

/// What the scene needs besides the tree and its layout: state that lives outside the IR.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SceneState {
    /// The node with keyboard focus. It draws its `focus_paint` (when it has one) and its
    /// focus ring.
    pub focus: Option<NodeId>,
}

impl SceneState {
    /// No focus.
    pub fn new() -> SceneState {
        SceneState::default()
    }

    /// Focus on `node`.
    pub fn with_focus(mut self, node: NodeId) -> SceneState {
        self.focus = Some(node);
        self
    }
}

/// Builds the display list of a laid-out tree.
///
/// Nodes the layout has no placement for (a `Laid` of another tree) are skipped, with their
/// subtrees, rather than panicking.
pub fn build(tree: &Tree, laid: &Laid, state: &SceneState) -> DisplayList {
    let root = tree.root();
    let background = match tree.get(root).and_then(|n| n.paint.fill.as_ref()) {
        Some(Fill::Solid(c)) => pcolor(*c),
        _ => Color::from_rgba8(0, 0, 0, 255),
    };
    let (width, height) = laid.size();
    let mut b = Builder {
        tree,
        laid,
        state,
        ops: Vec::new(),
        images: Vec::new(),
        image_ids: HashMap::new(),
    };
    b.walk(root);
    DisplayList {
        width: f64::from(width),
        height: f64::from(height),
        background,
        ops: b.ops,
        images: b.images,
    }
}

/// Work left on the explicit stack: a node to draw, or the end of a node.
enum Work {
    Enter(NodeId, Vec2),
    Exit(Exit),
}

/// What a node still owes after its children.
struct Exit {
    clip: bool,
    ring: Option<(Rect, RoundedRectRadii, ir::Stroke)>,
    opacity: bool,
}

struct Builder<'a> {
    tree: &'a Tree,
    laid: &'a Laid,
    state: &'a SceneState,
    ops: Vec<Op>,
    images: Vec<ImageKey>,
    image_ids: HashMap<ImageKey, ImageId>,
}

impl Builder<'_> {
    fn walk(&mut self, root: NodeId) {
        let mut stack = vec![Work::Enter(root, Vec2::ZERO)];
        while let Some(w) = stack.pop() {
            match w {
                Work::Enter(id, offset) => self.enter(id, offset, &mut stack),
                Work::Exit(x) => self.exit(x),
            }
        }
    }

    fn image(&mut self, key: ImageKey) -> ImageId {
        if let Some(id) = self.image_ids.get(&key) {
            return *id;
        }
        // The table cannot outgrow u32: it holds one entry per distinct key of one tree.
        let id = ImageId(u32::try_from(self.images.len()).unwrap_or(u32::MAX));
        self.images.push(key.clone());
        self.image_ids.insert(key, id);
        id
    }

    fn enter(&mut self, id: NodeId, parent_offset: Vec2, stack: &mut Vec<Work>) {
        let (Some(node), Some(placed)) = (self.tree.get(id), self.laid.get(id)) else {
            return;
        };
        let focused = self.state.focus == Some(id);
        let paint = match (&node.focus_paint, focused) {
            (Some(fp), true) => fp,
            _ => &node.paint,
        };
        let offset =
            parent_offset + Vec2::new(f64::from(paint.offset.0), f64::from(paint.offset.1));
        let rect = to_rect(
            placed.rect.x0,
            placed.rect.y0,
            placed.rect.x1,
            placed.rect.y1,
        ) + offset;
        let geo = Geo::new(rect, paint);

        // A node with no area, or a paint with a non-finite number, draws nothing of its own;
        // its children still get their turn.
        let draws = rect.width() > 0.0 && rect.height() > 0.0 && paint_is_finite(paint);
        let opacity = draws && paint.opacity < 1.0;
        if opacity {
            self.ops.push(Op::PushOpacity {
                alpha: paint.opacity.clamp(0.0, 1.0),
            });
        }
        if draws {
            self.outer_effects(paint, &geo);
            self.body(node, paint, placed, &geo, offset);
            self.border(paint, &geo);
            self.text(node, placed, offset);
        }

        let clip = placed.clips;
        if clip {
            self.ops.push(Op::PushClip {
                shape: Shape::Rect(rect),
            });
        }
        let ring = if focused && draws {
            paint.ring.map(|r| (rect, geo.radii, r))
        } else {
            None
        };
        stack.push(Work::Exit(Exit {
            clip,
            ring,
            opacity,
        }));
        for &c in self.tree.children(id).iter().rev() {
            stack.push(Work::Enter(c, offset));
        }
    }

    fn exit(&mut self, x: Exit) {
        if x.clip {
            self.ops.push(Op::Pop);
        }
        if let Some((rect, radii, ring)) = x.ring {
            // Outside the box, 2 px away, so it never covers the node's own border.
            let w = f64::from(ring.width);
            let grow = w + 2.0;
            let outer = RoundedRect::from_rect(rect.inflate(grow, grow), grow_radii(radii, grow));
            let inner = RoundedRect::from_rect(rect.inflate(2.0, 2.0), grow_radii(radii, 2.0));
            let mut path = outer.to_path(0.1);
            path.extend(inner.to_path(0.1));
            self.ops.push(Op::Fill {
                shape: Shape::Path {
                    path,
                    even_odd: true,
                },
                brush: Brush::Solid(pcolor(ring.color)),
            });
        }
        if x.opacity {
            self.ops.push(Op::Pop);
        }
    }

    /// Shadows, glow, backdrop (design 28 §1, steps 1 to 3).
    fn outer_effects(&mut self, paint: &Paint, geo: &Geo) {
        for e in &paint.effects {
            let Effect::Shadow {
                dx,
                dy,
                blur,
                spread,
                color,
            } = e
            else {
                continue;
            };
            let spread = f64::from(*spread);
            let sr = geo.rect.inflate(spread, spread) + Vec2::new(f64::from(*dx), f64::from(*dy));
            let radii = grow_radii(geo.radii, spread);
            if *blur <= 0.0 {
                // A hard shadow is an offset copy of the shape.
                self.ops.push(Op::Fill {
                    shape: geo.shape_of(sr, radii),
                    brush: Brush::Solid(pcolor(*color)),
                });
            } else {
                let sigma = f64::from(*blur) / 2.0;
                self.ops.push(Op::BlurRect {
                    rect: sr,
                    radii,
                    sigma,
                    color: pcolor(*color),
                    inset: false,
                    bounds: sr.inflate(sigma * 3.0, sigma * 3.0),
                });
            }
        }
        for e in &paint.effects {
            let Effect::Glow { layers } = e else { continue };
            // Outermost first, so the innermost halo ends on top.
            for layer in layers.iter().rev() {
                let sigma = f64::from(layer.radius) / 2.0;
                if sigma <= 0.0 {
                    continue; // a zero-radius halo is invisible
                }
                self.ops.push(Op::BlurRect {
                    rect: geo.rect,
                    radii: geo.radii,
                    sigma,
                    color: pcolor(layer.color),
                    inset: false,
                    bounds: geo.rect.inflate(sigma * 3.0, sigma * 3.0),
                });
            }
        }
        for e in &paint.effects {
            if let Effect::BackdropBlur { radius } = e
                && *radius > 0.0
            {
                self.ops.push(Op::Backdrop {
                    shape: geo.shape(),
                    sigma: f64::from(*radius),
                });
            }
        }
    }

    /// Fill, the image or icon, noise, scanlines, reflection and inner shadow (steps 4 to 8).
    fn body(&mut self, node: &Node, paint: &Paint, placed: &Placed, geo: &Geo, offset: Vec2) {
        let is_icon = matches!(node.kind, Kind::Icon(_));
        if let Some(f) = &paint.fill
            && !is_icon
            && let Some(brush) = fill_brush(f, geo.rect)
        {
            self.ops.push(Op::Fill {
                shape: geo.shape(),
                brush,
            });
        }
        match &node.kind {
            Kind::Image { source, fit } => {
                let key = match source {
                    ImageSource::Handle(h) => Some(ImageKey::Handle(Arc::clone(h))),
                    ImageSource::Asset(a) => Some(ImageKey::Asset(Arc::clone(a))),
                    _ => None,
                };
                if let Some(key) = key {
                    let id = self.image(key);
                    self.ops.push(Op::PushClip { shape: geo.shape() });
                    self.ops.push(Op::Image {
                        id,
                        rect: geo.rect,
                        fit: *fit,
                    });
                    self.ops.push(Op::Pop);
                }
            }
            Kind::Icon(name) => {
                if let Some(Fill::Solid(c)) = &paint.fill
                    && let Some(path) = icons::path(
                        name,
                        geo.rect.x0,
                        geo.rect.y0,
                        geo.rect.width(),
                        geo.rect.height(),
                    )
                {
                    self.ops.push(Op::Fill {
                        shape: Shape::Path {
                            path,
                            even_odd: false,
                        },
                        brush: Brush::Solid(pcolor(*c)),
                    });
                }
            }
            _ => {}
        }
        self.surface_effects(paint, placed, geo, offset);
    }

    fn surface_effects(&mut self, paint: &Paint, placed: &Placed, geo: &Geo, offset: Vec2) {
        let r = geo.rect;
        for e in &paint.effects {
            if let Effect::Noise { amount, seed } = e {
                let id = self.image(ImageKey::Noise { seed: *seed });
                self.ops.push(Op::Fill {
                    shape: geo.shape(),
                    brush: Brush::Image {
                        id,
                        transform: Affine::translate((r.x0, r.y0)),
                        repeat: true,
                        alpha: *amount,
                    },
                });
            }
        }
        for e in &paint.effects {
            let Effect::Scanlines { period, alpha } = e else {
                continue;
            };
            let p = f64::from(period.max(1.0));
            // Only the part of the box that can be seen, moved with the node's own offset.
            let v = placed.visible;
            let shown = Rect::new(v.x0, v.y0, v.x1, v.y1) + offset;
            let area = shown.intersect(r);
            if area.width() <= 0.0 || area.height() <= 0.0 {
                continue;
            }
            // Lines sit on the box's own grid, so scrolling never shifts the pattern.
            let first = ((area.y0 - r.y0) / p).floor().max(0.0);
            let mut y = r.y0 + first * p;
            let mut path = BezPath::new();
            let mut n = 0;
            while y < area.y1 && n < MAX_SCANLINES {
                let line = Rect::new(r.x0, y, r.x1, (y + p / 2.0).min(r.y1));
                path.extend(line.path_elements(0.1));
                y += p;
                n += 1;
            }
            self.ops.push(Op::PushClip { shape: geo.shape() });
            self.ops.push(Op::Fill {
                shape: Shape::Path {
                    path,
                    even_odd: false,
                },
                brush: Brush::Solid(Color::from_rgba8(0, 0, 0, alpha_u8(*alpha))),
            });
            self.ops.push(Op::Pop);
        }
        for e in &paint.effects {
            let Effect::Reflection { angle, alpha } = e else {
                continue;
            };
            // A glass sheen: white, bright at one corner, gone before the middle.
            let stops = [
                (0.0, white(*alpha)),
                (0.42, white(alpha * 0.35)),
                (0.5, white(0.0)),
                (1.0, white(0.0)),
            ];
            self.ops.push(Op::Fill {
                shape: geo.shape(),
                brush: Brush::Gradient(linear(r, *angle, &stops)),
            });
        }
        for e in &paint.effects {
            let Effect::InnerShadow {
                dx,
                dy,
                blur,
                color,
            } = e
            else {
                continue;
            };
            let sigma = (f64::from(*blur) / 2.0).max(0.01);
            let inner = r + Vec2::new(f64::from(*dx), f64::from(*dy));
            self.ops.push(Op::PushClip { shape: geo.shape() });
            self.ops.push(Op::BlurRect {
                rect: inner,
                radii: geo.radii,
                sigma,
                color: pcolor(*color),
                inset: true,
                bounds: r,
            });
            self.ops.push(Op::Pop);
        }
    }

    fn border(&mut self, paint: &Paint, geo: &Geo) {
        let w: [f64; 4] =
            std::array::from_fn(|i| paint.border[i].map_or(0.0, |s| f64::from(s.width)));
        if w.iter().all(|&x| x <= 0.0) {
            return;
        }
        let r = geo.rect;
        let inner = Rect::new(
            r.x0 + w[3],
            r.y0 + w[0],
            (r.x1 - w[1]).max(r.x0),
            (r.y1 - w[2]).max(r.y0),
        );
        let inner_radii = RoundedRectRadii::new(
            (geo.radii.top_left - w[3].max(w[0])).max(0.0),
            (geo.radii.top_right - w[1].max(w[0])).max(0.0),
            (geo.radii.bottom_right - w[1].max(w[2])).max(0.0),
            (geo.radii.bottom_left - w[3].max(w[2])).max(0.0),
        );
        let mut ring = geo.path_of(r, geo.radii);
        ring.extend(geo.path_of(inner, inner_radii));
        let colors: Vec<ir::Color> = paint.border.iter().flatten().map(|s| s.color).collect();
        let uniform =
            paint.border.iter().all(Option::is_some) && colors.windows(2).all(|c| c[0] == c[1]);
        if uniform {
            self.ops.push(Op::Fill {
                shape: Shape::Path {
                    path: ring,
                    even_odd: true,
                },
                brush: Brush::Solid(pcolor(colors[0])),
            });
            return;
        }
        // Per-side colours: clip to the ring, then one mitred quad per side.
        self.ops.push(Op::PushClip {
            shape: Shape::Path {
                path: ring,
                even_odd: true,
            },
        });
        let (o, i) = (r, inner);
        let quads = [
            [(o.x0, o.y0), (o.x1, o.y0), (i.x1, i.y0), (i.x0, i.y0)],
            [(o.x1, o.y0), (o.x1, o.y1), (i.x1, i.y1), (i.x1, i.y0)],
            [(o.x1, o.y1), (o.x0, o.y1), (i.x0, i.y1), (i.x1, i.y1)],
            [(o.x0, o.y1), (o.x0, o.y0), (i.x0, i.y0), (i.x0, i.y1)],
        ];
        for (side, q) in quads.iter().enumerate() {
            let Some(s) = paint.border[side] else {
                continue;
            };
            let mut path = BezPath::new();
            path.move_to(q[0]);
            for p in &q[1..] {
                path.line_to(*p);
            }
            path.close_path();
            self.ops.push(Op::Fill {
                shape: Shape::Path {
                    path,
                    even_odd: false,
                },
                brush: Brush::Solid(pcolor(s.color)),
            });
        }
        self.ops.push(Op::Pop);
    }

    /// The node's own text, after the border so a border never covers it (step 10).
    fn text(&mut self, node: &Node, placed: &Placed, offset: Vec2) {
        let (Kind::Text { style, .. }, Some(layout)) = (&node.kind, &placed.text) else {
            return;
        };
        let mut origin = Point::new(placed.content.x0 + offset.x, placed.content.y0 + offset.y);
        let marks = node.marks.as_deref();
        if let Some(marks) = marks {
            let content = to_rect(
                placed.content.x0,
                placed.content.y0,
                placed.content.x1,
                placed.content.y1,
            ) + offset;
            self.ops.push(Op::PushClip {
                shape: Shape::Rect(content),
            });
            origin.x -= f64::from(layout.scroll_for(
                marks.scroll,
                marks.caret.unwrap_or(marks.selection.end),
                content.width() as f32,
            ));
            for rect in layout.selection_rects(marks.selection.clone()) {
                self.mark(origin, rect, marks.selection_color);
            }
        }
        let runs: Vec<Arc<_>> = layout.runs().into_iter().map(Arc::new).collect();
        // A bloom of radius 0 is the sharp text again: skip the blurred pass.
        if let Some(bloom) = style.bloom.filter(|b| b.radius > 0.0) {
            // One filter layer per run: a layer wraps one primitive (design 28 §5).
            for run in &runs {
                self.ops.push(Op::PushFilter {
                    sigma: f64::from(bloom.radius) / 2.0,
                });
                self.ops.push(Op::Glyphs {
                    run: Arc::clone(run),
                    origin,
                    brush: Brush::Solid(pcolor(bloom.color)),
                });
                self.ops.push(Op::Pop);
            }
        }
        for run in runs {
            self.ops.push(Op::Glyphs {
                run,
                origin,
                brush: Brush::Solid(pcolor(style.color)),
            });
        }
        if let Some(marks) = marks {
            for mut rect in layout.selection_rects(marks.preedit.clone().unwrap_or_default()) {
                rect.y0 = rect.y1 - 1.0;
                self.mark(origin, rect, marks.caret_color);
            }
            if let Some(caret) = marks.caret {
                self.mark(origin, layout.caret_rect(caret), marks.caret_color);
            }
            self.ops.push(Op::Pop);
        }
    }

    /// A solid box over or under the text at `origin`.
    fn mark(&mut self, origin: Point, rect: TextRect, color: ir::Color) {
        let rect = to_rect(
            rect.x0.into(),
            rect.y0.into(),
            rect.x1.into(),
            rect.y1.into(),
        ) + origin.to_vec2();
        self.ops.push(Op::Fill {
            shape: Shape::Rect(rect),
            brush: Brush::Solid(pcolor(color)),
        });
    }
}

/// A node's outline: a rounded box or an ellipse, at its place after offsets.
struct Geo {
    rect: Rect,
    /// Corner radii, each between 0 and half the short side. For an ellipse they are the
    /// circle that fits (what a blur can draw).
    radii: RoundedRectRadii,
    ellipse: bool,
}

impl Geo {
    fn new(rect: Rect, paint: &Paint) -> Geo {
        let half = (rect.width().min(rect.height()) / 2.0).max(0.0);
        if paint.shape == ir::Shape::Circle {
            return Geo {
                rect,
                radii: RoundedRectRadii::from_single_radius(half),
                ellipse: true,
            };
        }
        let c = |v: f32| f64::from(v).clamp(0.0, half);
        Geo {
            rect,
            radii: RoundedRectRadii::new(
                c(paint.radius[0]),
                c(paint.radius[1]),
                c(paint.radius[2]),
                c(paint.radius[3]),
            ),
            ellipse: false,
        }
    }

    fn shape(&self) -> Shape {
        self.shape_of(self.rect, self.radii)
    }

    fn shape_of(&self, rect: Rect, radii: RoundedRectRadii) -> Shape {
        if self.ellipse {
            Shape::Ellipse(Ellipse::from_rect(rect))
        } else if radii == RoundedRectRadii::from_single_radius(0.0) {
            Shape::Rect(rect)
        } else {
            Shape::RoundedRect(RoundedRect::from_rect(rect, radii))
        }
    }

    fn path_of(&self, rect: Rect, radii: RoundedRectRadii) -> BezPath {
        self.shape_of(rect, radii).to_path().0
    }
}

fn grow_radii(r: RoundedRectRadii, d: f64) -> RoundedRectRadii {
    let g = |v: f64| if v > 0.0 { (v + d).max(0.0) } else { 0.0 };
    RoundedRectRadii::new(
        g(r.top_left),
        g(r.top_right),
        g(r.bottom_right),
        g(r.bottom_left),
    )
}

fn to_rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Rect {
    Rect::new(x0, y0, x1, y1)
}

fn pcolor(c: ir::Color) -> Color {
    let [r, g, b, a] = c.0;
    Color::from_rgba8(r, g, b, a)
}

fn alpha_u8(a: f32) -> u8 {
    (a.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn white(alpha: f32) -> Color {
    Color::from_rgba8(255, 255, 255, alpha_u8(alpha))
}

/// A CSS-angle linear gradient (0 up, 90 right, 180 down) across a box, so the 0 and 1
/// stops touch its corners.
fn linear(r: Rect, angle: f32, stops: &[(f32, Color)]) -> Gradient {
    let a = f64::from(angle).to_radians();
    let (dx, dy) = (a.sin(), -a.cos());
    let half = (r.width() * dx.abs() + r.height() * dy.abs()) / 2.0;
    let c = r.center();
    Gradient::new_linear(
        Point::new(c.x - dx * half, c.y - dy * half),
        Point::new(c.x + dx * half, c.y + dy * half),
    )
    .with_stops(
        stops
            .iter()
            .map(|(o, c)| ColorStop::from((*o, *c)))
            .collect::<Vec<_>>()
            .as_slice(),
    )
}

/// The brush of a fill, or `None` for a fill that paints nothing (a fully transparent colour).
fn fill_brush(f: &Fill, r: Rect) -> Option<Brush> {
    Some(match f {
        Fill::Solid(c) => {
            if c.0[3] == 0 {
                return None;
            }
            Brush::Solid(pcolor(*c))
        }
        Fill::Linear { angle, stops } => {
            let s: Vec<(f32, Color)> = stops.iter().map(|s| (s.offset, pcolor(s.color))).collect();
            Brush::Gradient(linear(r, *angle, &s))
        }
        Fill::Radial { stops } => {
            // CSS's default is an ellipse to the farthest corner; a circle that reaches it is
            // close enough for a glass or LED spot.
            let radius = (r.width().hypot(r.height()) / 2.0) as f32;
            let s: Vec<ColorStop> = stops
                .iter()
                .map(|s| ColorStop::from((s.offset, pcolor(s.color))))
                .collect();
            Brush::Gradient(Gradient::new_radial(r.center(), radius).with_stops(s.as_slice()))
        }
        _ => return None,
    })
}

/// Whether every number the scene reads from a paint is finite. The skin loader keeps them
/// in range, but a hand-built tree might not, and a NaN must never reach a renderer.
fn paint_is_finite(p: &Paint) -> bool {
    let fin = |v: &f32| v.is_finite();
    let effects = p.effects.iter().all(|e| match e {
        Effect::Shadow {
            dx,
            dy,
            blur,
            spread,
            ..
        } => [dx, dy, blur, spread].into_iter().all(fin),
        Effect::InnerShadow { dx, dy, blur, .. } => [dx, dy, blur].into_iter().all(fin),
        Effect::Glow { layers } => layers.iter().all(|l| l.radius.is_finite()),
        Effect::Noise { amount, .. } => amount.is_finite(),
        Effect::Scanlines { period, alpha } => period.is_finite() && alpha.is_finite(),
        Effect::Reflection { angle, alpha } => angle.is_finite() && alpha.is_finite(),
        Effect::BackdropBlur { radius } => radius.is_finite(),
        _ => true,
    });
    let fill = match &p.fill {
        Some(Fill::Linear { angle, stops }) => {
            angle.is_finite() && stops.iter().all(|s| s.offset.is_finite())
        }
        Some(Fill::Radial { stops }) => stops.iter().all(|s| s.offset.is_finite()),
        _ => true,
    };
    effects
        && fill
        && p.opacity.is_finite()
        && p.radius.iter().all(fin)
        && p.offset.0.is_finite()
        && p.offset.1.is_finite()
        && p.border.iter().flatten().all(|s| s.width.is_finite())
        && p.ring.is_none_or(|r| r.width.is_finite())
}
