//! IR layout styles → taffy styles (design 27 §4, §5.1).

use gogo_ui_ir::{Align, Dim, Direction, Display, Justify, LayoutStyle, Node, Position, Track};
use taffy::prelude::*;
use taffy::style::{GridTemplateComponent, LengthPercentage, LengthPercentageAuto};

use crate::LayoutError;

fn finite(key: &str, field: &'static str, v: f32) -> Result<f32, LayoutError> {
    if v.is_finite() {
        Ok(v)
    } else {
        Err(LayoutError::NotFinite {
            key: key.to_string(),
            field,
        })
    }
}

fn dim(key: &str, field: &'static str, d: Dim) -> Result<Dimension, LayoutError> {
    Ok(match d {
        Dim::Auto => Dimension::auto(),
        Dim::Px(p) => Dimension::length(finite(key, field, p)?),
        Dim::Pct(p) => Dimension::percent(finite(key, field, p)? / 100.0),
    })
}

fn lpa(key: &str, field: &'static str, d: Dim) -> Result<LengthPercentageAuto, LayoutError> {
    Ok(match d {
        Dim::Auto => LengthPercentageAuto::auto(),
        Dim::Px(p) => LengthPercentageAuto::length(finite(key, field, p)?),
        Dim::Pct(p) => LengthPercentageAuto::percent(finite(key, field, p)? / 100.0),
    })
}

fn len(key: &str, field: &'static str, v: f32) -> Result<LengthPercentage, LayoutError> {
    Ok(LengthPercentage::length(finite(key, field, v)?))
}

fn align(a: Align) -> AlignItems {
    match a {
        Align::Start => AlignItems::START,
        Align::Center => AlignItems::CENTER,
        Align::End => AlignItems::END,
        Align::Stretch => AlignItems::STRETCH,
        Align::Baseline => AlignItems::BASELINE,
    }
}

fn track(key: &str, t: &Track) -> Result<GridTemplateComponent<String>, LayoutError> {
    Ok(match t {
        Track::Px(p) => length(finite(key, "grid track", *p)?),
        Track::Fr(f) => fr(finite(key, "grid track", *f)?),
        Track::Auto => auto(),
    })
}

/// The taffy style of a node. `Err` names the first non-finite length.
pub(crate) fn style(key: &str, n: &Node) -> Result<Style, LayoutError> {
    let l: &LayoutStyle = &n.layout;
    let bw = |i: usize| n.paint.border[i].map_or(0.0, |s| s.width);
    let mut s = Style {
        display: match l.display {
            Display::Flex => taffy::Display::Flex,
            Display::Grid => taffy::Display::Grid,
        },
        flex_direction: match l.direction {
            Direction::Row => FlexDirection::Row,
            Direction::Column => FlexDirection::Column,
        },
        flex_wrap: if l.wrap {
            FlexWrap::Wrap
        } else {
            FlexWrap::NoWrap
        },
        align_items: l.align_items.map_or(AlignItems::NORMAL, align),
        align_self: l.align_self.map(align),
        justify_content: match l.justify {
            None => JustifyContent::NORMAL,
            Some(Justify::Start) => JustifyContent::START,
            Some(Justify::Center) => JustifyContent::CENTER,
            Some(Justify::End) => JustifyContent::END,
            Some(Justify::SpaceBetween) => JustifyContent::SPACE_BETWEEN,
            Some(Justify::SpaceAround) => JustifyContent::SPACE_AROUND,
        },
        gap: Size {
            width: len(key, "column gap", l.column_gap)?,
            height: len(key, "row gap", l.row_gap)?,
        },
        padding: taffy::Rect {
            left: len(key, "padding", l.padding.left)?,
            right: len(key, "padding", l.padding.right)?,
            top: len(key, "padding", l.padding.top)?,
            bottom: len(key, "padding", l.padding.bottom)?,
        },
        margin: taffy::Rect {
            left: LengthPercentageAuto::length(finite(key, "margin", l.margin.left)?),
            right: LengthPercentageAuto::length(finite(key, "margin", l.margin.right)?),
            top: LengthPercentageAuto::length(finite(key, "margin", l.margin.top)?),
            bottom: LengthPercentageAuto::length(finite(key, "margin", l.margin.bottom)?),
        },
        border: taffy::Rect {
            top: len(key, "border width", bw(0))?,
            right: len(key, "border width", bw(1))?,
            bottom: len(key, "border width", bw(2))?,
            left: len(key, "border width", bw(3))?,
        },
        size: Size {
            width: dim(key, "width", l.width)?,
            height: dim(key, "height", l.height)?,
        },
        min_size: Size {
            width: lpa(key, "min width", l.min_width)?,
            height: lpa(key, "min height", l.min_height)?,
        },
        max_size: Size {
            width: lpa(key, "max width", l.max_width)?,
            height: lpa(key, "max height", l.max_height)?,
        },
        flex_grow: finite(key, "grow", l.grow)?,
        flex_shrink: finite(key, "shrink", l.shrink)?,
        aspect_ratio: match l.aspect {
            Some(a) => Some(finite(key, "aspect", a)?),
            None => None,
        },
        grid_template_columns: l
            .columns
            .iter()
            .map(|t| track(key, t))
            .collect::<Result<_, _>>()?,
        grid_template_rows: l
            .rows
            .iter()
            .map(|t| track(key, t))
            .collect::<Result<_, _>>()?,
        // Always set: since taffy 0.15 an inset does nothing without it.
        position: taffy::Position::Relative,
        ..Style::default()
    };
    if l.position == Position::Absolute {
        s.position = taffy::Position::Absolute;
        s.inset = taffy::Rect {
            top: lpa(key, "inset", l.inset.top)?,
            right: lpa(key, "inset", l.inset.right)?,
            bottom: lpa(key, "inset", l.inset.bottom)?,
            left: lpa(key, "inset", l.inset.left)?,
        };
    }
    if let Some((r, c)) = l.cell {
        s.grid_row = Line {
            start: line(clamp_line(r)),
            end: span(1),
        };
        s.grid_column = Line {
            start: line(clamp_line(c)),
            end: span(l.col_span.max(1)),
        };
    } else if l.col_span > 1 {
        s.grid_column = Line {
            start: auto(),
            end: span(l.col_span),
        };
    }
    Ok(s)
}

/// Grid lines are `i16` in taffy; a skin never gets near it, but a hand-built tree might.
fn clamp_line(v: u16) -> i16 {
    i16::try_from(v).unwrap_or(i16::MAX)
}
