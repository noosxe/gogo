//! Paint and effects (design 27 §4, §6; drawn as design 28 §1 and §2 say).

/// Straight (not premultiplied) sRGB with 8 bits per channel: `[r, g, b, a]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Color(pub [u8; 4]);

impl Color {
    pub const TRANSPARENT: Color = Color([0, 0, 0, 0]);
    pub const BLACK: Color = Color([0, 0, 0, 255]);
    pub const WHITE: Color = Color([255, 255, 255, 255]);

    /// An opaque colour.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color([r, g, b, 255])
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
        Color([r, g, b, a])
    }
}

/// One colour stop of a gradient.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct GradientStop {
    /// Position along the gradient, 0 to 1. Stops are in ascending order.
    pub offset: f32,
    pub color: Color,
}

/// What fills a node's box.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Fill {
    Solid(Color),
    /// CSS angle in degrees (180 runs top to bottom). Two to eight stops.
    Linear {
        angle: f32,
        stops: Vec<GradientStop>,
    },
    /// Centred ellipse over the box, from the middle (offset 0) to the farthest corner
    /// (offset 1). Two to eight stops.
    Radial {
        stops: Vec<GradientStop>,
    },
}

/// A line of one width and colour.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Stroke {
    /// Logical pixels. A border's width also counts in layout.
    pub width: f32,
    pub color: Color,
}

/// The outline of a node's box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Shape {
    /// A rectangle with [`Paint::radius`] corners. The default.
    #[default]
    Rect,
    /// The ellipse inscribed in the box (a circle when the box is square); the radii
    /// are ignored.
    Circle,
}

/// One halo of a [`Effect::Glow`].
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct GlowLayer {
    /// CSS blur radius in px: sigma is half of it.
    pub radius: f32,
    pub color: Color,
}

/// The fixed effect list (design 27 §6). Text bloom is not here: it belongs to the
/// text it lights ([`crate::Bloom`]).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Effect {
    /// A drop shadow behind the box.
    Shadow {
        /// Offset in px, positive right.
        dx: f32,
        /// Offset in px, positive down.
        dy: f32,
        /// CSS blur radius in px: sigma is half of it.
        blur: f32,
        /// Growth of the shadow's box in px before blurring.
        spread: f32,
        color: Color,
    },
    /// A shadow drawn inside the box, clipped to it.
    InnerShadow {
        dx: f32,
        dy: f32,
        /// CSS blur radius in px.
        blur: f32,
        color: Color,
    },
    /// One to four halos around the box, innermost first.
    Glow { layers: Vec<GlowLayer> },
    /// A tiled grain over the fill.
    Noise {
        /// Opacity of the grain, 0 to 1.
        amount: f32,
        /// Picks the pattern. The skin format's default is 7.
        seed: u32,
    },
    /// Horizontal black lines clipped to the box.
    Scanlines {
        /// Distance between lines in px, at least 1.
        period: f32,
        /// Opacity of the lines, 0 to 1.
        alpha: f32,
    },
    /// A glass sheen: a white linear gradient at `angle` degrees, from `alpha` fading to
    /// nothing before the middle. Not a mirror.
    Reflection { angle: f32, alpha: f32 },
    /// Blurs what is already drawn behind the box (`backdrop-filter: blur(radius)`:
    /// sigma is `radius`, not half of it). The most expensive effect.
    BackdropBlur { radius: f32 },
}

/// How a node's box looks. [`Default`] draws nothing: no fill, no border, square corners,
/// fully opaque, a rectangle, no offset, no ring, no effects.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Paint {
    /// `None`: no fill.
    pub fill: Option<Fill>,
    /// Top, right, bottom, left. `None`: no border on that side.
    pub border: [Option<Stroke>; 4],
    /// Corner radii in px: top-left, top-right, bottom-right, bottom-left.
    pub radius: [f32; 4],
    /// 0 to 1, for the node and everything under it (a layer, design 28 §1). Default 1.
    pub opacity: f32,
    pub shape: Shape,
    /// A visual offset in px for the node and its descendants (a pressed key). It never
    /// changes layout or hit areas.
    pub offset: (f32, f32),
    /// A focus ring drawn outside the box, over the node's children (last in the paint
    /// order). It takes no layout space.
    pub ring: Option<Stroke>,
    /// Drawn in the order of design 28 §1, whatever their order here.
    pub effects: Vec<Effect>,
}

impl Default for Paint {
    fn default() -> Self {
        Paint {
            fill: None,
            border: [None; 4],
            radius: [0.0; 4],
            opacity: 1.0,
            shape: Shape::Rect,
            offset: (0.0, 0.0),
            ring: None,
            effects: Vec::new(),
        }
    }
}

impl Paint {
    /// A solid fill and nothing else.
    pub fn solid(color: Color) -> Self {
        Paint {
            fill: Some(Fill::Solid(color)),
            ..Self::default()
        }
    }
}
