//! Fonts, shaping and line breaking for the GUI engine (design 24 §2, decision 10), on
//! parley.
//!
//! - [`FontSet`]: the bundled fonts, Noto Sans (variable, with `wght` and `wdth` axes) and
//!   JetBrains Mono (variable on `wght` only), loaded from bytes compiled into the crate.
//!   System fonts are off; the `system-fonts` feature adds them as a fallback for glyphs
//!   the bundled set lacks ([`FontSet::with_system_fallback`]).
//! - [`TextLayout`]: one shaped [`gogo_ui_ir::TextStyle`] run, broken into lines at a
//!   width: measured, hit-tested and turned into [`GlyphRun`]s.
//! - [`GlyphRun`]: positioned glyphs of one font, what the scene hands a renderer
//!   (design 28 §1, the `Glyphs` op).
//!
//! # Rules
//!
//! - **Bundled fonts decide.** A family a [`TextStyle`](gogo_ui_ir::TextStyle) names is
//!   looked up among the bundled families only, by the name the font file gives itself
//!   (design 27 §3). An empty or unknown family gets the default face, Noto Sans. After
//!   the run's own family, every other bundled family is tried for a missing glyph, then
//!   (with the feature on) the system's fallback for the script.
//! - **A system copy never stands in for a bundled family.** The bundled fonts are
//!   registered under private family names, so an installed "Noto Sans" of another
//!   version can't change the metrics (design 24 decision 10: the same text on every
//!   machine).
//! - **Units** are logical pixels (design 24 §2). `tracking` is in em, so it is
//!   multiplied by the size; `line_height` is a multiple of the size, and `None` keeps the
//!   font's own.
//! - **Wrapping**: with `wrap`, lines break at word boundaries within the width; without
//!   it the text is one line, however wide. With `wrap` and `max_lines > 0`, only that many
//!   lines are kept, the last one ending in an ellipsis (`…`) when text was cut.
//!
//! The crate knows nothing about gogo (design 24 §2.1 rule 6). The edit buffer (selection,
//! IME composition) arrives with text input (#544).
#![deny(unsafe_code)]
#![warn(missing_docs)]

mod fonts;
mod layout;
mod run;

pub use fonts::{BUNDLED_FAMILIES, DEFAULT_FAMILY, FontSet};
pub use layout::{LineMetrics, TextLayout};
pub use run::{FontData, Glyph, GlyphRun};
