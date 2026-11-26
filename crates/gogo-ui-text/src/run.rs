//! Glyph runs: what a renderer draws for text (design 28 §1, the `Glyphs` op).

/// A font file and the index of a face in it, shared by reference (no copy per run). The
/// same type vello's renderers take, so a run goes to them without conversion (design 24
/// decision 6).
pub use parley::FontData;

/// One glyph, placed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glyph {
    /// The glyph id in the run's font.
    pub id: u32,
    /// Left edge of the glyph's advance, in logical px from the text box's left edge.
    pub x: f32,
    /// The baseline, in logical px from the text box's top edge.
    pub y: f32,
}

/// Positioned glyphs of one font at one size and variation, in one line.
///
/// Positions are relative to the text box's top-left corner (its content box); the scene
/// adds the box's origin.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct GlyphRun {
    /// The font the glyph ids index into.
    pub font: FontData,
    /// The font size in logical px.
    pub size: f32,
    /// Normalised variation coordinates (F2Dot14 bits), one per axis of a variable font;
    /// empty for a static face. They pick `wght` and `wdth` in the bundled fonts.
    pub coords: Vec<i16>,
    /// The glyphs, in visual order.
    pub glyphs: Vec<Glyph>,
}
