//! Layout styles: flex and grid in this crate's own enums (design 27 §4, §5.1).
//!
//! The layout crate maps these to its solver; nothing here names a solver type.

/// A length along one axis.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Dim {
    /// Let the layout decide (content size, or stretch).
    Auto,
    /// Logical pixels.
    Px(f32),
    /// A percentage of the parent's size on that axis (`50.0` is half).
    Pct(f32),
}

/// A grid track size.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Track {
    /// Logical pixels.
    Px(f32),
    /// A share of the free space (`1fr`); the value is the share, above 0.
    Fr(f32),
    /// Sized by content.
    Auto,
}

/// How a node lays its children out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Display {
    /// Flexbox; [`LayoutStyle::direction`] picks the axis.
    #[default]
    Flex,
    /// CSS grid with [`LayoutStyle::columns`] and [`LayoutStyle::rows`].
    Grid,
}

/// The main axis of a flex container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Direction {
    /// Horizontal.
    Row,
    /// Vertical. The default: `column`, `box` and `form` are vertical (design 27 §5.1).
    #[default]
    Column,
}

/// Alignment on the cross axis (`align`, `align-self`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Align {
    /// The start of the cross axis.
    Start,
    /// The middle of the cross axis.
    Center,
    /// The end of the cross axis.
    End,
    /// Fill the cross axis.
    Stretch,
    /// Align the first text baselines.
    Baseline,
}

/// Distribution on the main axis (`justify`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Justify {
    /// Items packed at the start.
    Start,
    /// Items packed in the middle.
    Center,
    /// Items packed at the end.
    End,
    /// `between`: free space between the items.
    SpaceBetween,
    /// `around`: free space around each item.
    SpaceAround,
}

/// Whether a node takes part in its parent's flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Position {
    /// In the flow. [`LayoutStyle::inset`] is ignored.
    #[default]
    Relative,
    /// Out of the flow, placed by [`LayoutStyle::inset`] against the parent (design 27 §4).
    Absolute,
}

/// Four per-side values, in CSS order.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Sides<T> {
    /// The top side.
    pub top: T,
    /// The right side.
    pub right: T,
    /// The bottom side.
    pub bottom: T,
    /// The left side.
    pub left: T,
}

impl<T: Copy> Sides<T> {
    /// The same value on every side.
    pub const fn all(v: T) -> Self {
        Sides {
            top: v,
            right: v,
            bottom: v,
            left: v,
        }
    }

    /// `vertical` on top and bottom, `horizontal` on left and right.
    pub const fn symmetric(vertical: T, horizontal: T) -> Self {
        Sides {
            top: vertical,
            right: horizontal,
            bottom: vertical,
            left: horizontal,
        }
    }
}

/// Everything layout needs to know about one node. Other crates build it from
/// [`LayoutStyle::default`], [`row`](LayoutStyle::row), [`column`](LayoutStyle::column)
/// or [`grid`](LayoutStyle::grid) and set the public fields; the struct is
/// `#[non_exhaustive]` because scroll and overflow arrive later. [`Default`] is a vertical flex
/// container with no sizes, gaps or spacing, as CSS defines an initial flex item
/// (`shrink` is 1).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct LayoutStyle {
    /// How the children are laid out. Default [`Display::Flex`].
    pub display: Display,
    /// Flex main axis. Ignored for a grid. Default [`Direction::Column`].
    pub direction: Direction,
    /// Flex items wrap onto further lines. Default `false`.
    pub wrap: bool,
    /// Cross-axis alignment of the children. `None`: the solver's default (stretch).
    pub align_items: Option<Align>,
    /// This node's own cross-axis alignment, overriding its parent's. `None`: use the
    /// parent's.
    pub align_self: Option<Align>,
    /// Main-axis distribution of the children. `None`: start.
    pub justify: Option<Justify>,
    /// Gap between rows, in px.
    pub row_gap: f32,
    /// Gap between columns, in px.
    pub column_gap: f32,
    /// Inside spacing in px, before the border is added (see the crate docs).
    pub padding: Sides<f32>,
    /// Outside spacing in px. May be negative.
    pub margin: Sides<f32>,
    /// Width, border box. Default [`Dim::Auto`].
    pub width: Dim,
    /// Height, border box. Default [`Dim::Auto`].
    pub height: Dim,
    /// Smallest width. Default [`Dim::Auto`] (no minimum).
    pub min_width: Dim,
    /// Smallest height. Default [`Dim::Auto`] (no minimum).
    pub min_height: Dim,
    /// Largest width. Default [`Dim::Auto`] (no maximum).
    pub max_width: Dim,
    /// Largest height. Default [`Dim::Auto`] (no maximum).
    pub max_height: Dim,
    /// Share of the free main-axis space this node takes. Default 0.
    pub grow: f32,
    /// How much this node shrinks when the line overflows. Default 1.
    pub shrink: f32,
    /// Grid column tracks. Empty: implicit auto tracks.
    pub columns: Vec<Track>,
    /// Grid row tracks. Empty: implicit auto rows.
    pub rows: Vec<Track>,
    /// Grid columns this node spans, at least 1. Default 1.
    pub col_span: u16,
    /// Default [`Position::Relative`].
    pub position: Position,
    /// Offsets from the parent's edges for an [`Position::Absolute`] node. [`Dim::Auto`]
    /// leaves a side free. Default all `Auto`.
    pub inset: Sides<Dim>,
    /// Width over height, when only one of them is set. `None`: no fixed ratio.
    pub aspect: Option<f32>,
    /// The grid cell this node sits in as `(row, column)`, 1-based. `None`: auto
    /// placement. A `layer` is a one-cell grid whose children all have `Some((1, 1))`
    /// (design 27 §5.1).
    pub cell: Option<(u16, u16)>,
}

impl Default for LayoutStyle {
    fn default() -> Self {
        LayoutStyle {
            display: Display::Flex,
            direction: Direction::Column,
            wrap: false,
            align_items: None,
            align_self: None,
            justify: None,
            row_gap: 0.0,
            column_gap: 0.0,
            padding: Sides::all(0.0),
            margin: Sides::all(0.0),
            width: Dim::Auto,
            height: Dim::Auto,
            min_width: Dim::Auto,
            min_height: Dim::Auto,
            max_width: Dim::Auto,
            max_height: Dim::Auto,
            grow: 0.0,
            shrink: 1.0,
            columns: Vec::new(),
            rows: Vec::new(),
            col_span: 1,
            position: Position::Relative,
            inset: Sides::all(Dim::Auto),
            aspect: None,
            cell: None,
        }
    }
}

impl LayoutStyle {
    /// A horizontal flex container.
    pub fn row() -> Self {
        LayoutStyle {
            direction: Direction::Row,
            ..Self::default()
        }
    }

    /// A vertical flex container (the default, spelled out).
    pub fn column() -> Self {
        Self::default()
    }

    /// A grid with the given column tracks.
    pub fn grid(columns: Vec<Track>) -> Self {
        LayoutStyle {
            display: Display::Grid,
            columns,
            ..Self::default()
        }
    }
}
