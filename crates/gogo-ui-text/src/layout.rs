//! One shaped text run, broken into lines: measure, hit-test, glyph runs (design 24 §2).

use std::ops::Range;

use gogo_ui_ir::{TextAlign, TextStyle};
use parley::{Affinity, Alignment, AlignmentOptions, Cursor, PositionedLayoutItem, Selection};

use crate::{FontSet, Glyph, GlyphRun};

/// The ellipsis that ends a cut last line.
const ELLIPSIS: char = '…';

/// One line of a [`TextLayout`], in logical px from the text box's top-left corner.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct LineMetrics {
    /// The top of the line box.
    pub top: f32,
    /// The bottom of the line box.
    pub bottom: f32,
    /// The baseline.
    pub baseline: f32,
    /// Where the line's content starts (after alignment).
    pub left: f32,
    /// The width of the content, without hanging whitespace.
    pub width: f32,
    /// The bytes of [`TextLayout::text`] on this line.
    pub text: Range<usize>,
}

/// A box in logical px from the text box's top-left corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextRect {
    /// Left edge.
    pub x0: f32,
    /// Top edge.
    pub y0: f32,
    /// Right edge.
    pub x1: f32,
    /// Bottom edge.
    pub y1: f32,
}

impl From<parley::BoundingBox> for TextRect {
    fn from(b: parley::BoundingBox) -> TextRect {
        TextRect {
            x0: b.x0 as f32,
            y0: b.y0 as f32,
            x1: b.x1 as f32,
            y1: b.y1 as f32,
        }
    }
}

/// The width of a caret.
pub const CARET_WIDTH: f32 = 1.5;

/// A shaped run of text and its line breaks.
///
/// Made by [`FontSet::layout`]. Until [`TextLayout::break_lines`] is called it is one
/// line. Every size is in logical px; positions are relative to the text box's top-left
/// corner (design 24 §2).
pub struct TextLayout {
    inner: parley::Layout<()>,
    /// The text as given.
    source: String,
    /// The text shaped now: `source`, or a cut of it ending in [`ELLIPSIS`].
    text: String,
    style: TextStyle,
    /// The lines drawn: all of them, or `max_lines`.
    visible: usize,
    /// Lines past `max_lines` exist (before [`TextLayout::ellipsize`] cut them).
    truncated: bool,
}

impl std::fmt::Debug for TextLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextLayout")
            .field("text", &self.text)
            .field("lines", &self.inner.len())
            .field("visible", &self.visible)
            .finish_non_exhaustive()
    }
}

impl TextLayout {
    pub(crate) fn new(inner: parley::Layout<()>, text: &str, style: &TextStyle) -> Self {
        let mut t = TextLayout {
            inner,
            source: text.to_string(),
            text: text.to_string(),
            style: style.clone(),
            visible: 0,
            truncated: false,
        };
        t.break_lines(None);
        t
    }

    /// Breaks the text into lines no wider than `max_width` and aligns them in that width.
    /// Text without `wrap` stays one line (only explicit newlines break it); `None` is no
    /// limit. With `wrap` and `max_lines > 0`, only that many lines count for the size and
    /// the glyphs. This is cheap: it does not shape again, so it suits measuring.
    pub fn break_lines(&mut self, max_width: Option<f32>) {
        self.inner
            .set_alignment(self.alignment(), AlignmentOptions::default());
        let max = self.effective_width(max_width);
        self.inner.break_all_lines(max);
        let lines = self.inner.len();
        self.visible = match self.limit() {
            Some(n) => lines.min(n),
            None => lines,
        };
        self.truncated = self.visible < lines;
    }

    /// The width to break at. With `wrap`, the box width. Without it the text never
    /// wraps, but a box wider than the text still aligns it, so the width is the larger of
    /// the box and the text's own width.
    fn effective_width(&self, max_width: Option<f32>) -> Option<f32> {
        let w = max_width.filter(|w| w.is_finite()).map(|w| w.max(0.0))?;
        if self.style.wrap {
            Some(w)
        } else {
            Some(w.max(self.inner.calculate_content_widths().max))
        }
    }

    /// `max_lines` when it applies (with `wrap`, above 0).
    fn limit(&self) -> Option<usize> {
        (self.style.wrap && self.style.max_lines > 0).then_some(usize::from(self.style.max_lines))
    }

    /// Breaks lines as [`TextLayout::break_lines`] does and, when lines past `max_lines`
    /// are cut, ends the last kept line in `…`: the longest prefix of the text that still
    /// fits in `max_lines` lines with the ellipsis. Shapes again a few times (a binary
    /// search over the last line), so it is for the final width, not for measuring.
    pub fn ellipsize(&mut self, fonts: &mut FontSet, max_width: Option<f32>) {
        if self.text != self.source {
            self.inner = fonts.shape(&self.source, &self.style);
            self.text.clone_from(&self.source);
        }
        self.break_lines(max_width);
        let Some(limit) = self.limit() else { return };
        if !self.truncated {
            return;
        }
        let Some(last) = self.inner.get(limit - 1).map(|l| l.text_range()) else {
            return;
        };
        let cuts: Vec<usize> = (last.start..=last.end)
            .filter(|&i| self.source.is_char_boundary(i))
            .collect();
        let candidate = |cut: usize| {
            let mut t = self.source[..cut].trim_end().to_string();
            t.push(ELLIPSIS);
            t
        };
        // The largest cut that fits; the line's start always counts as the fallback.
        let (mut lo, mut hi) = (0usize, cuts.len());
        let mut best: Option<(String, parley::Layout<()>)> = None;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let t = candidate(cuts[mid]);
            let mut lay = fonts.shape(&t, &self.style);
            lay.set_alignment(self.alignment(), AlignmentOptions::default());
            lay.break_all_lines(self.effective_width(max_width));
            if lay.len() <= limit {
                best = Some((t, lay));
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let (text, lay) = match best {
            Some(b) => b,
            None => {
                let t = candidate(last.start);
                let mut lay = fonts.shape(&t, &self.style);
                lay.set_alignment(self.alignment(), AlignmentOptions::default());
                lay.break_all_lines(self.effective_width(max_width));
                (t, lay)
            }
        };
        self.inner = lay;
        self.text = text;
        self.visible = self.inner.len().min(limit);
        self.truncated = true;
    }

    fn alignment(&self) -> Alignment {
        match self.style.align {
            TextAlign::Start => Alignment::Start,
            TextAlign::Center => Alignment::Center,
            TextAlign::End => Alignment::End,
        }
    }

    /// The text shaped now: the given text, or its cut ending in `…` after
    /// [`TextLayout::ellipsize`].
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether lines were cut at `max_lines`.
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// The number of lines drawn.
    pub fn line_count(&self) -> usize {
        self.visible
    }

    /// The width of the widest drawn line, without hanging whitespace.
    pub fn width(&self) -> f32 {
        if !self.truncated {
            return self.inner.width();
        }
        self.inner
            .lines()
            .take(self.visible)
            .map(|l| l.metrics().advance - l.metrics().hanging_advance)
            .fold(0.0, f32::max)
    }

    /// The height of the drawn lines.
    pub fn height(&self) -> f32 {
        if !self.truncated {
            return self.inner.height();
        }
        self.visible
            .checked_sub(1)
            .and_then(|i| self.inner.get(i))
            .map_or(0.0, |l| l.metrics().block_max_coord)
    }

    /// `(width, height)`.
    pub fn size(&self) -> (f32, f32) {
        (self.width(), self.height())
    }

    /// The first line's baseline, from the top. `None` for empty text.
    pub fn first_baseline(&self) -> Option<f32> {
        self.inner.first_baseline()
    }

    /// The drawn lines, top to bottom.
    pub fn lines(&self) -> Vec<LineMetrics> {
        self.inner
            .lines()
            .take(self.visible)
            .map(|l| {
                let m = l.metrics();
                LineMetrics {
                    top: m.block_min_coord,
                    bottom: m.block_max_coord,
                    baseline: m.baseline,
                    left: m.offset,
                    width: m.advance - m.hanging_advance,
                    text: l.text_range(),
                }
            })
            .collect()
    }

    /// The byte offset in [`TextLayout::text`] nearest to a point (relative to the text
    /// box): where a click puts the caret. Above the text is offset 0's line, below it the
    /// last line; past a line's end is that line's end.
    pub fn hit(&self, x: f32, y: f32) -> usize {
        Cursor::from_point(&self.inner, x, y).index()
    }

    fn cursor(&self, offset: usize) -> Cursor {
        let offset = offset.min(self.text.len());
        let affinity = if offset == self.text.len() {
            Affinity::Upstream
        } else {
            Affinity::Downstream
        };
        Cursor::from_byte_index(&self.inner, offset, affinity)
    }

    /// The caret before the byte `offset` of [`TextLayout::text`], [`CARET_WIDTH`] wide.
    pub fn caret_rect(&self, offset: usize) -> TextRect {
        self.cursor(offset)
            .geometry(&self.inner, CARET_WIDTH)
            .into()
    }

    /// The boxes behind a selection of these bytes of [`TextLayout::text`].
    pub fn selection_rects(&self, range: Range<usize>) -> Vec<TextRect> {
        if range.is_empty() {
            return Vec::new();
        }
        Selection::new(self.cursor(range.start), self.cursor(range.end))
            .geometry(&self.inner)
            .into_iter()
            .map(|(rect, _)| rect.into())
            .collect()
    }

    /// How far a one-line field `width` wide has to move this text left so that the
    /// caret at `offset` shows. `last` is where it was: it moves no further than needed,
    /// and never leaves a gap after the end of the text.
    pub fn scroll_for(&self, last: f32, offset: usize, width: f32) -> f32 {
        let caret = self.caret_rect(offset);
        let end = self.caret_rect(self.text.len()).x1;
        let last = if last.is_finite() { last } else { 0.0 };
        let most = (end - width).max(0.0);
        // Not `clamp`: with a field narrower than the caret the bounds cross.
        last.max(caret.x1 - width).min(caret.x0).min(most).max(0.0)
    }

    /// The glyphs of the drawn lines, one run per font change per line.
    pub fn runs(&self) -> Vec<GlyphRun> {
        let mut out = Vec::new();
        for line in self.inner.lines().take(self.visible) {
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(gr) = item else {
                    continue;
                };
                let run = gr.run();
                out.push(GlyphRun {
                    font: run.font().clone(),
                    size: run.font_size(),
                    coords: run
                        .normalized_coords()
                        .iter()
                        .map(|c| c.to_bits())
                        .collect(),
                    glyphs: gr
                        .positioned_glyphs()
                        .map(|g| Glyph {
                            id: g.id,
                            x: g.x,
                            y: g.y,
                        })
                        .collect(),
                });
            }
        }
        out
    }
}
