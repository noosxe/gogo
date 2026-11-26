//! The scenes the renderers are tested on (design 28 §7): a sheet with every effect, and
//! routes of the Default skin on fixture data. Both go through layout and the scene
//! builder, so lowering is exercised too. Fonts are the bundled set only.

use gogo_skin::{Binding, Mode, SkinPackage, UiState};
use gogo_ui_ir::{
    Bloom, Color, Dim, Effect, Fill, Fit, GlowLayer, GradientStop, ImageSource, Kind, LayoutStyle,
    Node, NodeId, Paint, Position, Shape, Sides, Stroke, TextStyle, Tree,
};
use gogo_ui_layout::{Scroll, Viewport, layout};
use gogo_ui_scene::{DisplayList, MemoryStore, SceneState, build};
use gogo_ui_text::FontSet;

use crate::{FixtureScope, covers};

/// A display list and the images it needs.
#[derive(Debug)]
pub struct Scene {
    /// A name for files and messages.
    pub name: &'static str,
    /// What to draw.
    pub list: DisplayList,
    /// Its images.
    pub images: MemoryStore,
}

/// The size of the route scenes in logical px.
pub const ROUTE_SIZE: (f32, f32) = (960.0, 600.0);

/// Why a scene could not be built.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SceneError {
    /// The skin did not load or bind.
    #[error("the skin did not bind: {0}")]
    Skin(String),
    /// The tree was refused.
    #[error(transparent)]
    Tree(#[from] gogo_ui_ir::TreeError),
    /// Layout failed.
    #[error(transparent)]
    Layout(#[from] gogo_ui_layout::LayoutError),
}

fn scene(name: &'static str, list: DisplayList) -> Scene {
    let images = covers::store_for(&list);
    Scene { name, list, images }
}

/// Every scene: the effects sheet and the Default skin's `game` and `shell` routes.
///
/// # Errors
/// [`SceneError`] when one of them cannot be built.
pub fn all() -> Result<Vec<Scene>, SceneError> {
    Ok(vec![
        effects_sheet()?,
        default_route("default-game", "game")?,
        default_route("default-shell", "shell")?,
    ])
}

/// A route of the built-in Default skin, dark mode, on [`FixtureScope`].
///
/// # Errors
/// [`SceneError`].
pub fn default_route(name: &'static str, route: &str) -> Result<Scene, SceneError> {
    let d = gogo_skins::default();
    let package = SkinPackage::from_files(d.name, d.to_files())
        .map_err(|r| SceneError::Skin(r.to_string()))?;
    let scope = FixtureScope::new();
    let ui = UiState::defaults(&package.manifest);
    let bound = package
        .bind(
            route,
            &Binding {
                scope: &scope,
                ui: &ui,
                mode: Mode::Dark,
                default: None,
            },
        )
        .map_err(|e| SceneError::Skin(format!("{e:?}")))?;
    let laid = layout(
        &bound.tree,
        &mut FontSet::bundled(),
        Viewport::new(ROUTE_SIZE.0, ROUTE_SIZE.1),
        &Scroll::none(),
    )?;
    Ok(scene(name, build(&bound.tree, &laid, &SceneState::new())))
}

const CELL: (f32, f32) = (112.0, 100.0);
const COLUMNS: usize = 5;

/// A tree being built, with keys counted for it.
struct Sheet {
    tree: Tree,
    keys: usize,
}

/// A style placing a node at `(x, y)` in its parent with a size.
fn at(x: f32, y: f32, w: f32, h: f32) -> LayoutStyle {
    let mut s = LayoutStyle::default();
    s.position = Position::Absolute;
    s.inset = Sides {
        top: Dim::Px(y),
        right: Dim::Auto,
        bottom: Dim::Auto,
        left: Dim::Px(x),
    };
    s.width = Dim::Px(w);
    s.height = Dim::Px(h);
    s
}

fn boxed(layout: LayoutStyle, paint: Paint) -> Node {
    let mut n = Node::new(Kind::Box);
    n.layout = layout;
    n.paint = paint;
    n
}

fn solid(rgba: [u8; 4], radius: f32, effects: Vec<Effect>) -> Paint {
    Paint {
        radius: [radius; 4],
        effects,
        ..Paint::solid(Color(rgba))
    }
}

fn stops(colors: &[(f32, [u8; 4])]) -> Vec<GradientStop> {
    colors
        .iter()
        .map(|&(offset, c)| GradientStop {
            offset,
            color: Color(c),
        })
        .collect()
}

const WHITE: [u8; 4] = [244, 244, 248, 255];
const BLUE: [u8; 4] = [48, 92, 200, 255];

impl Sheet {
    fn add(&mut self, parent: NodeId, node: Node) -> Result<NodeId, SceneError> {
        self.keys += 1;
        Ok(self
            .tree
            .add_child(parent, format!("n{}", self.keys), node)?)
    }

    /// The `i`th cell of the grid.
    fn cell(&mut self, i: usize) -> Result<NodeId, SceneError> {
        let x = (i % COLUMNS) as f32 * CELL.0;
        let y = (i / COLUMNS) as f32 * CELL.1;
        let root = self.tree.root();
        self.add(root, boxed(at(x, y, CELL.0, CELL.1), Paint::default()))
    }

    /// A box of `paint` in the middle of the `i`th cell.
    fn card(&mut self, i: usize, paint: Paint) -> Result<NodeId, SceneError> {
        let cell = self.cell(i)?;
        self.add(cell, boxed(at(16.0, 16.0, 80.0, 68.0), paint))
    }

    fn text(
        &mut self,
        parent: NodeId,
        layout: LayoutStyle,
        text: &str,
        style: TextStyle,
    ) -> Result<NodeId, SceneError> {
        let mut n = Node::new(Kind::Text {
            text: text.to_owned(),
            style,
        });
        n.layout = layout;
        self.add(parent, n)
    }

    fn image(
        &mut self,
        parent: NodeId,
        layout: LayoutStyle,
        handle: &str,
        fit: Fit,
        paint: Paint,
    ) -> Result<NodeId, SceneError> {
        let mut n = Node::new(Kind::Image {
            source: ImageSource::Handle(handle.into()),
            fit,
        });
        n.layout = layout;
        n.paint = paint;
        self.add(parent, n)
    }

    /// Vertical colour bars: something sharp for a backdrop blur to soften.
    fn bars(&mut self, parent: NodeId, width: f32, height: f32) -> Result<(), SceneError> {
        let colors = [
            [230, 60, 60, 255],
            [240, 190, 40, 255],
            [50, 170, 90, 255],
            BLUE,
            WHITE,
            [20, 20, 28, 255],
        ];
        let w = width / colors.len() as f32;
        for (i, c) in colors.into_iter().enumerate() {
            let layout = at(i as f32 * w, 0.0, w, height);
            self.add(parent, boxed(layout, solid(c, 0.0, Vec::new())))?;
        }
        Ok(())
    }
}

/// One sheet with every effect of design 28 §2, a backdrop inside an open opacity layer and
/// clip, borders, an icon, a focus ring and images scaled up, down and by each fit.
///
/// # Errors
/// [`SceneError`].
pub fn effects_sheet() -> Result<Scene, SceneError> {
    let size = (CELL.0 * COLUMNS as f32, CELL.1 * 4.0);
    let mut root = LayoutStyle::default();
    root.width = Dim::Px(size.0);
    root.height = Dim::Px(size.1);
    let mut s = Sheet {
        tree: Tree::new(
            "sheet",
            boxed(root, solid([104, 112, 128, 255], 0.0, Vec::new())),
        ),
        keys: 0,
    };
    let shadow = |dx, dy, blur, spread, color| Effect::Shadow {
        dx,
        dy,
        blur,
        spread,
        color: Color(color),
    };

    // Row 1: shadows, glow, inner shadow.
    s.card(
        0,
        solid(
            WHITE,
            10.0,
            vec![shadow(0.0, 6.0, 18.0, 0.0, [0, 0, 0, 170])],
        ),
    )?;
    s.card(
        1,
        solid(
            WHITE,
            6.0,
            vec![
                shadow(-4.0, -4.0, 10.0, 3.0, [20, 40, 200, 200]),
                shadow(6.0, 6.0, 0.0, 2.0, [0, 0, 0, 140]),
            ],
        ),
    )?;
    s.card(
        2,
        solid(
            [20, 20, 30, 255],
            12.0,
            vec![Effect::Glow {
                layers: vec![
                    GlowLayer {
                        radius: 6.0,
                        color: Color([255, 60, 220, 255]),
                    },
                    GlowLayer {
                        radius: 20.0,
                        color: Color([40, 230, 255, 220]),
                    },
                ],
            }],
        ),
    )?;
    s.card(
        3,
        solid(
            WHITE,
            14.0,
            vec![Effect::InnerShadow {
                dx: 0.0,
                dy: 4.0,
                blur: 12.0,
                color: Color([0, 0, 0, 200]),
            }],
        ),
    )?;
    let mut corners = solid(
        WHITE,
        0.0,
        vec![shadow(0.0, 4.0, 12.0, 0.0, [0, 0, 0, 180])],
    );
    corners.radius = [24.0, 4.0, 16.0, 0.0];
    s.card(4, corners)?;

    // Row 2: text and gradients.
    let bloomed = s.card(5, solid([16, 18, 28, 255], 6.0, Vec::new()))?;
    s.text(
        bloomed,
        at(8.0, 18.0, 72.0, 32.0),
        "Bloom",
        TextStyle {
            size: 22.0,
            weight: 700.0,
            color: Color([220, 255, 255, 255]),
            bloom: Some(Bloom {
                radius: 12.0,
                color: Color([0, 220, 255, 255]),
            }),
            ..TextStyle::default()
        },
    )?;
    let text = s.card(6, solid(WHITE, 6.0, Vec::new()))?;
    for (y, string, style) in [
        (
            6.0,
            "Small 10px Ag",
            TextStyle {
                size: 10.0,
                ..TextStyle::default()
            },
        ),
        (
            22.0,
            "Narrow bold",
            TextStyle {
                size: 15.0,
                weight: 800.0,
                width: 70.0,
                ..TextStyle::default()
            },
        ),
        (
            46.0,
            "mono 0O1l",
            TextStyle {
                family: "JetBrains Mono".into(),
                size: 12.0,
                color: Color(BLUE),
                ..TextStyle::default()
            },
        ),
    ] {
        s.text(text, at(5.0, y, 75.0, 18.0), string, style)?;
    }
    s.card(
        7,
        Paint {
            fill: Some(Fill::Linear {
                angle: 135.0,
                stops: stops(&[
                    (0.0, [250, 200, 60, 255]),
                    (0.5, [230, 60, 130, 255]),
                    (1.0, [40, 40, 160, 255]),
                ]),
            }),
            radius: [8.0; 4],
            ..Paint::default()
        },
    )?;
    s.card(
        8,
        Paint {
            fill: Some(Fill::Radial {
                stops: stops(&[(0.0, WHITE), (0.6, BLUE), (1.0, [10, 10, 20, 0])]),
            }),
            ..Paint::default()
        },
    )?;
    s.card(
        9,
        solid(
            BLUE,
            8.0,
            vec![Effect::Noise {
                amount: 0.3,
                seed: 7,
            }],
        ),
    )?;

    // Row 3: scanlines, reflection, backdrop, shapes and borders.
    s.card(
        10,
        solid(
            [120, 220, 150, 255],
            10.0,
            vec![Effect::Scanlines {
                period: 4.0,
                alpha: 0.45,
            }],
        ),
    )?;
    s.card(
        11,
        solid(
            [30, 34, 60, 255],
            12.0,
            vec![Effect::Reflection {
                angle: 120.0,
                alpha: 0.55,
            }],
        ),
    )?;
    let behind = s.card(12, Paint::default())?;
    s.bars(behind, 80.0, 68.0)?;
    s.add(
        behind,
        boxed(
            at(12.0, 14.0, 56.0, 40.0),
            solid(
                [255, 255, 255, 50],
                12.0,
                vec![Effect::BackdropBlur { radius: 6.0 }],
            ),
        ),
    )?;
    // A scroll container (clips) at 0.7 opacity holding a backdrop that sticks out of it.
    let faded = s.card(
        13,
        Paint {
            opacity: 0.7,
            ..Paint::default()
        },
    )?;
    s.bars(faded, 80.0, 68.0)?;
    s.add(
        faded,
        boxed(
            at(30.0, 20.0, 70.0, 60.0),
            solid(
                [0, 0, 0, 40],
                16.0,
                vec![Effect::BackdropBlur { radius: 5.0 }],
            ),
        ),
    )?;
    let ringed = s.card(
        14,
        Paint {
            shape: Shape::Circle,
            border: [Some(Stroke {
                width: 4.0,
                color: Color(WHITE),
            }); 4],
            ring: Some(Stroke {
                width: 3.0,
                color: Color([250, 200, 60, 255]),
            }),
            ..Paint::solid(Color(BLUE))
        },
    )?;

    // Row 4: per-side borders with an icon, then images.
    let side = |c| {
        Some(Stroke {
            width: 6.0,
            color: Color(c),
        })
    };
    let bordered = s.card(
        15,
        Paint {
            border: [
                side([230, 60, 60, 255]),
                side([240, 190, 40, 255]),
                side([50, 170, 90, 255]),
                side(BLUE),
            ],
            radius: [10.0; 4],
            ..Paint::solid(Color(WHITE))
        },
    )?;
    let mut icon = Node::new(Kind::Icon("play".into()));
    icon.layout = at(24.0, 18.0, 32.0, 32.0);
    icon.paint = Paint::solid(Color(BLUE));
    s.add(bordered, icon)?;
    let cover = s.card(16, Paint::default())?;
    s.image(
        cover,
        at(16.0, 0.0, 48.0, 68.0),
        "art:sheet:background",
        Fit::Cover,
        solid([0, 0, 0, 255], 10.0, Vec::new()),
    )?;
    let contain = s.card(17, Paint::default())?;
    s.image(
        contain,
        at(0.0, 0.0, 80.0, 68.0),
        "art:sheet:logo",
        Fit::Contain,
        solid([20, 20, 28, 255], 0.0, Vec::new()),
    )?;
    let up = s.card(18, Paint::default())?;
    s.image(
        up,
        at(0.0, 0.0, 80.0, 68.0),
        covers::CHECKER,
        Fit::Fill,
        Paint::default(),
    )?;
    let down = s.card(19, Paint::default())?;
    s.image(
        down,
        at(20.0, 4.0, 45.0, 60.0),
        "art:sheet:cover",
        Fit::Cover,
        Paint::default(),
    )?;

    let laid = layout(
        &s.tree,
        &mut FontSet::bundled(),
        Viewport::new(size.0, size.1),
        &Scroll::none().with(faded, 0.0, 0.0),
    )?;
    let state = SceneState::new().with_focus(ringed);
    Ok(scene("effects", build(&s.tree, &laid, &state)))
}
