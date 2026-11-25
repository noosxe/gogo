//! Resolved text (design 27 §5.2; fonts and shaping are the text crate's job).

use std::sync::Arc;

use crate::paint::Color;

/// Horizontal alignment of a text run inside its box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum TextAlign {
    /// The start of the line. The default.
    #[default]
    Start,
    /// The middle of the line.
    Center,
    /// The end of the line.
    End,
}

/// A glow of the glyphs themselves (`bloom-text`). A box never blooms.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Bloom {
    /// CSS blur radius in px: sigma is half of it.
    pub radius: f32,
    /// The glow's colour, straight sRGB.
    pub color: Color,
}

/// Every property of one text run, resolved: nothing is inherited at this level.
///
/// [`Default`] is a neutral run (the engine's default face at 16 px, regular weight,
/// opaque black, one line). The binder always sets every field from the skin's tokens
/// and styles; the default only serves tests and hand-built trees.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct TextStyle {
    /// A bundled family name. Empty: the engine's default face.
    pub family: Arc<str>,
    /// Font size in px, 1 to 1024.
    pub size: f32,
    /// 1 to 1000; 400 is regular, 700 bold.
    pub weight: f32,
    /// The `wdth` axis in percent, 25 to 200; 100 is normal. Only variable faces honour
    /// it.
    pub width: f32,
    /// Extra letter spacing in em (a fraction of the font size), -5 to 5.
    pub tracking: f32,
    /// A multiple of the font size. `None`: the font's own line height.
    pub line_height: Option<f32>,
    /// The glyph colour.
    pub color: Color,
    /// Horizontal alignment inside the node's box.
    pub align: TextAlign,
    /// Wrap at word boundaries within the node's width. `false`: one line.
    pub wrap: bool,
    /// With `wrap`, the most lines before an ellipsis. 0: no limit.
    pub max_lines: u16,
    /// Glow of the glyphs. `None`: none.
    pub bloom: Option<Bloom>,
}

impl Default for TextStyle {
    fn default() -> Self {
        TextStyle {
            family: Arc::from(""),
            size: 16.0,
            weight: 400.0,
            width: 100.0,
            tracking: 0.0,
            line_height: None,
            color: Color::BLACK,
            align: TextAlign::Start,
            wrap: false,
            max_lines: 0,
            bloom: None,
        }
    }
}
