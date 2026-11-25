//! Style properties to typed layout, paint and text (design 27 §4, §6).
//!
//! The same property names work in a `style` block, inline on an element, and as the
//! parts of an app widget. The loader has checked every value it can know at load; a value
//! that is an expression or a token is checked again here against the same ranges, so a
//! value from the data can't put the engine out of range.

use std::sync::Arc;

use gogo_contract::Value;
use gogo_ui_ir::{
    Align, Bloom, Color, Dim, Direction, Display, Effect, Fill, GlowLayer, GradientStop, Justify,
    LayoutStyle, Paint, Position, Shape, Sides, Stroke, TextAlign, TextStyle, Track,
};

use crate::fonts;
use crate::tokens::Rgba;

/// The text properties an element passes on to its children (design 27 §5.2).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TextCtx {
    pub family: Arc<str>,
    pub size: f32,
    pub weight: f32,
    pub width: f32,
    pub tracking: f32,
    pub line_height: Option<f32>,
    pub color: Color,
    pub align: TextAlign,
    pub upper: bool,
    pub wrap: bool,
    pub max_lines: u16,
    pub bloom: Option<Bloom>,
    /// The family `mono=#true` text uses (`mono-font`, by default the `font.mono` token's).
    pub mono: Option<Arc<str>>,
}

impl Default for TextCtx {
    fn default() -> Self {
        TextCtx {
            family: Arc::from("Noto Sans"),
            size: 14.0,
            weight: 400.0,
            width: 100.0,
            tracking: 0.0,
            line_height: None,
            color: Color::WHITE,
            align: TextAlign::Start,
            upper: false,
            wrap: false,
            max_lines: 0,
            bloom: None,
            mono: None,
        }
    }
}

impl TextCtx {
    /// The resolved run style. Nothing inherits in the IR, so every field is set.
    pub fn run(&self) -> TextStyle {
        TextStyle {
            family: Arc::clone(&self.family),
            size: self.size,
            weight: self.weight,
            width: self.width,
            tracking: self.tracking,
            line_height: self.line_height,
            color: self.color,
            align: self.align,
            wrap: self.wrap,
            max_lines: if self.wrap { self.max_lines } else { 0 },
            bloom: self.bloom,
        }
    }

    /// `text` as it is drawn: in capitals under `upper`.
    pub fn shown(&self, text: &str) -> String {
        if self.upper {
            text.to_uppercase()
        } else {
            text.to_owned()
        }
    }

    /// Switch to `family`. A static face has no `wdth` axis, so an inherited `stretch`
    /// stops there (design 27 §5.2).
    pub fn set_family(&mut self, family: &str) {
        if !fonts::family(family).is_some_and(|f| f.variable) {
            self.width = 100.0;
        }
        self.family = Arc::from(family);
    }
}

/// Everything the properties of one node can set.
#[derive(Debug, Clone, Default)]
pub(crate) struct Computed {
    pub layout: LayoutStyle,
    pub paint: Paint,
    pub text: TextCtx,
    /// `key-hint #true`: show a button's `key=` as a hint chip.
    pub key_hint: bool,
}

type R<T> = Result<T, String>;

fn f(v: &Value, what: &str, lo: f64, hi: f64) -> R<f32> {
    let n = match v {
        Value::Num(n) => *n,
        Value::Str(s) => s
            .trim()
            .parse::<f64>()
            .map_err(|_| format!("`{s}` is not a number"))?,
        v => return Err(format!("{what} must be a number, found {}", v.type_name())),
    };
    if !n.is_finite() || n < lo || n > hi {
        return Err(format!("{what} must be from {lo} to {hi}, found {n}"));
    }
    Ok(n as f32)
}

/// `"18 20"` and `18 20` alike, as numbers in range.
fn nums(vals: &[Value], lo: f64, hi: f64) -> R<Vec<f32>> {
    let mut out = Vec::new();
    for v in vals {
        match v {
            Value::Str(s) => {
                for p in s.split_whitespace() {
                    out.push(f(&Value::Str(p.to_owned()), "a number", lo, hi)?);
                }
            }
            v => out.push(f(v, "a number", lo, hi)?),
        }
    }
    Ok(out)
}

fn word(v: &Value) -> String {
    v.display()
}

fn colour(v: &Value) -> R<Color> {
    match v {
        Value::Str(s) if s == "transparent" => Ok(Color::TRANSPARENT),
        Value::Str(s) => Rgba::parse(s)
            .map(|c| Color::rgba(c.r, c.g, c.b, c.a))
            .ok_or_else(|| format!("`{s}` is not a colour")),
        v => Err(format!("expected a colour, found {}", v.type_name())),
    }
}

fn dim(v: &Value) -> R<Dim> {
    match v {
        Value::Str(s) if s == "auto" => Ok(Dim::Auto),
        Value::Str(s) if s.ends_with('%') => Ok(Dim::Pct(f(
            &Value::Str(s.trim_end_matches('%').to_owned()),
            "a percentage",
            -100_000.0,
            100_000.0,
        )?)),
        v => Ok(Dim::Px(f(v, "a length", -100_000.0, 100_000.0)?)),
    }
}

fn dims(vals: &[Value]) -> R<Vec<Dim>> {
    let mut out = Vec::new();
    for v in vals {
        match v {
            Value::Str(s) => {
                for p in s.split_whitespace() {
                    out.push(dim(&Value::Str(p.to_owned()))?);
                }
            }
            v => out.push(dim(v)?),
        }
    }
    Ok(out)
}

fn align(s: &str) -> R<Align> {
    Ok(match s {
        "start" => Align::Start,
        "center" => Align::Center,
        "end" => Align::End,
        "stretch" => Align::Stretch,
        "baseline" => Align::Baseline,
        _ => {
            return Err(format!(
                "`{s}` is not start, center, end, stretch or baseline"
            ));
        }
    })
}

/// `columns 3` is three equal columns, else each value is a track.
fn tracks(vals: &[Value]) -> R<Vec<Track>> {
    if let [Value::Num(n)] = vals {
        let count = f(&Value::Num(*n), "a track count", 1.0, 64.0)?;
        return Ok(vec![Track::Fr(1.0); count as usize]);
    }
    let mut out = Vec::new();
    for v in vals {
        let words: Vec<String> = match v {
            Value::Num(n) => vec![n.to_string()],
            Value::Str(s) => s.split_whitespace().map(str::to_owned).collect(),
            other => return Err(format!("a track can't be {}", other.type_name())),
        };
        for t in words {
            out.push(if t == "auto" {
                Track::Auto
            } else if let Some(n) = t.strip_suffix("fr") {
                let n = n
                    .parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite() && *n > 0.0 && *n <= 100_000.0)
                    .ok_or_else(|| format!("`{t}` is not a track"))?;
                Track::Fr(n as f32)
            } else {
                Track::Px(f(&Value::Str(t), "a track size", 0.0, 100_000.0)?)
            });
        }
    }
    if out.is_empty() || out.len() > 64 {
        return Err("a track list has 1 to 64 tracks".to_owned());
    }
    Ok(out)
}

fn stops(vals: &[Value]) -> R<Vec<GradientStop>> {
    if !(2..=8).contains(&vals.len()) {
        return Err("a gradient has 2 to 8 colours".to_owned());
    }
    let n = (vals.len() - 1) as f32;
    vals.iter()
        .enumerate()
        .map(|(i, v)| {
            Ok(GradientStop {
                offset: i as f32 / n,
                color: colour(v)?,
            })
        })
        .collect()
}

fn side_ix(side: &str) -> R<usize> {
    ["top", "right", "bottom", "left"]
        .iter()
        .position(|x| *x == side)
        .ok_or_else(|| format!("`{side}` is not top, right, bottom or left"))
}

fn stroke(w: &Value, c: Option<&Value>) -> R<Stroke> {
    Ok(Stroke {
        width: f(w, "a width", 0.0, 1000.0)?,
        color: colour(c.ok_or("a colour is missing")?)?,
    })
}

fn flag(v: &Value) -> bool {
    v.truthy()
}

/// Apply one property. `Ok(false)` if `name` is not a style property.
pub(crate) fn apply(c: &mut Computed, name: &str, v: &[Value]) -> R<bool> {
    let first = || v.first().ok_or_else(|| format!("`{name}` has no value"));
    let l = &mut c.layout;
    let p = &mut c.paint;
    let t = &mut c.text;
    const BOX: f64 = 10_000.0;
    match name {
        // Size
        "w" => l.width = dim(first()?)?,
        "h" => l.height = dim(first()?)?,
        "min-w" => l.min_width = dim(first()?)?,
        "max-w" => l.max_width = dim(first()?)?,
        "min-h" => l.min_height = dim(first()?)?,
        "max-h" => l.max_height = dim(first()?)?,
        "size" => {
            let d = dim(first()?)?;
            l.width = d;
            l.height = d;
        }
        "aspect" => l.aspect = Some(f(first()?, "an aspect ratio", 0.01, 100.0)?),
        // Flex and grid
        "grow" => l.grow = f(first()?, "grow", 0.0, 1000.0)?,
        "shrink" => l.shrink = f(first()?, "shrink", 0.0, 1000.0)?,
        "gap" => match nums(v, 0.0, BOX)?.as_slice() {
            [a] => {
                l.row_gap = *a;
                l.column_gap = *a;
            }
            [r, col] => {
                l.row_gap = *r;
                l.column_gap = *col;
            }
            _ => return Err("gap takes one or two numbers".to_owned()),
        },
        "row-gap" => l.row_gap = f(first()?, "a gap", 0.0, BOX)?,
        "direction" => {
            l.direction = match word(first()?).as_str() {
                "row" => Direction::Row,
                "column" => Direction::Column,
                o => return Err(format!("`{o}` is not row or column")),
            }
        }
        "wrap" => l.wrap = flag(first()?),
        "align" => l.align_items = Some(align(&word(first()?))?),
        "align-self" => l.align_self = Some(align(&word(first()?))?),
        "justify" => {
            l.justify = Some(match word(first()?).as_str() {
                "start" => Justify::Start,
                "center" => Justify::Center,
                "end" => Justify::End,
                "between" => Justify::SpaceBetween,
                "around" => Justify::SpaceAround,
                o => {
                    return Err(format!(
                        "`{o}` is not start, center, end, between or around"
                    ));
                }
            })
        }
        "columns" => {
            l.display = Display::Grid;
            l.columns = tracks(v)?;
        }
        "rows" => l.rows = tracks(v)?,
        "col-span" => l.col_span = f(first()?, "a column span", 1.0, 64.0)? as u16,
        // Box
        "padding" => {
            l.padding = sides(&nums(v, 0.0, BOX)?)?;
        }
        "margin" => {
            l.margin = sides(&nums(v, -BOX, BOX)?)?;
        }
        "inset" => {
            let d = dims(v)?;
            l.position = Position::Absolute;
            l.inset = match d.as_slice() {
                [a] => Sides::all(*a),
                [a, b] => Sides::symmetric(*a, *b),
                [a, b, c2, d2] => {
                    let mut s = Sides::all(*a);
                    s.right = *b;
                    s.bottom = *c2;
                    s.left = *d2;
                    s
                }
                _ => return Err("inset takes 1, 2 or 4 values".to_owned()),
            };
        }
        // Paint
        "fill" => p.fill = Some(Fill::Solid(colour(first()?)?)),
        "gradient" => {
            p.fill = Some(match word(first()?).as_str() {
                "linear" => Fill::Linear {
                    angle: f(
                        v.get(1).ok_or("a linear gradient needs an angle")?,
                        "an angle",
                        -3600.0,
                        3600.0,
                    )?,
                    stops: stops(&v[2..])?,
                },
                "radial" => Fill::Radial {
                    stops: stops(&v[1..])?,
                },
                o => return Err(format!("`{o}` is not linear or radial")),
            });
        }
        "border" => {
            let s = stroke(first()?, v.get(1))?;
            p.border = [Some(s); 4];
        }
        "border-top" | "border-right" | "border-bottom" | "border-left" => {
            p.border[side_ix(&name[7..])?] = Some(stroke(first()?, v.get(1))?);
        }
        "accent-edge" => {
            let side = side_ix(&word(first()?))?;
            p.border[side] = Some(stroke(v.get(1).ok_or("a width is missing")?, v.get(2))?);
        }
        "radius" => match nums(v, 0.0, BOX)?.as_slice() {
            [a] => p.radius = [*a; 4],
            [a, b, c2, d] => p.radius = [*a, *b, *c2, *d],
            _ => return Err("radius takes 1 or 4 numbers".to_owned()),
        },
        "opacity" => p.opacity = f(first()?, "an opacity", 0.0, 1.0)?,
        "shape" => {
            p.shape = match word(first()?).as_str() {
                "circle" => Shape::Circle,
                "rect" => Shape::Rect,
                o => return Err(format!("`{o}` is not circle or rect")),
            }
        }
        "offset" => match nums(v, -BOX, BOX)?.as_slice() {
            [x, y] => p.offset = (*x, *y),
            _ => return Err("offset takes x and y".to_owned()),
        },
        "ring" => p.ring = Some(stroke(first()?, v.get(1))?),
        // Effects
        "effects" => {
            if word(first()?) != "none" {
                return Err("the only value is `none`".to_owned());
            }
            p.effects.clear();
            t.bloom = None;
        }
        "shadow" if word(first()?) == "none" => {
            p.effects.retain(|e| !matches!(e, Effect::Shadow { .. }));
        }
        "shadow" => {
            let n = nums(&v[..3.min(v.len())], -BOX, BOX)?;
            let [dx, dy, blur] = n.as_slice() else {
                return Err("shadow takes x, y, blur and a colour".to_owned());
            };
            if *blur < 0.0 {
                return Err("a blur radius is not negative".to_owned());
            }
            p.effects.push(Effect::Shadow {
                dx: *dx,
                dy: *dy,
                blur: *blur,
                color: colour(v.get(3).ok_or("a colour is missing")?)?,
                spread: v
                    .get(4)
                    .map(|s| f(s, "a spread", -BOX, BOX))
                    .transpose()?
                    .unwrap_or(0.0),
            });
        }
        "inner-shadow" => {
            let n = nums(&v[..3.min(v.len())], -BOX, BOX)?;
            let [dx, dy, blur] = n.as_slice() else {
                return Err("inner-shadow takes x, y, blur and a colour".to_owned());
            };
            if *blur < 0.0 {
                return Err("a blur radius is not negative".to_owned());
            }
            p.effects
                .retain(|e| !matches!(e, Effect::InnerShadow { .. }));
            p.effects.push(Effect::InnerShadow {
                dx: *dx,
                dy: *dy,
                blur: *blur,
                color: colour(v.get(3).ok_or("a colour is missing")?)?,
            });
        }
        "glow" => {
            let mut layers = Vec::new();
            for pair in v.chunks(2) {
                layers.push(GlowLayer {
                    radius: f(&pair[0], "a radius", 0.0, BOX)?,
                    color: colour(pair.get(1).ok_or("a colour is missing")?)?,
                });
            }
            if !(1..=4).contains(&layers.len()) {
                return Err("glow takes 1 to 4 radius and colour pairs".to_owned());
            }
            p.effects.retain(|e| !matches!(e, Effect::Glow { .. }));
            p.effects.push(Effect::Glow { layers });
        }
        "bloom-text" => {
            t.bloom = match first()? {
                Value::Str(x) if x == "none" => None,
                r => Some(Bloom {
                    radius: f(r, "a radius", 0.0, BOX)?,
                    color: colour(v.get(1).ok_or("a colour is missing")?)?,
                }),
            }
        }
        "noise" => {
            p.effects.retain(|e| !matches!(e, Effect::Noise { .. }));
            p.effects.push(Effect::Noise {
                amount: f(first()?, "an amount", 0.0, 1.0)?,
                seed: v
                    .get(1)
                    .map(|s| f(s, "a seed", 0.0, f64::from(u32::MAX)))
                    .transpose()?
                    .map_or(7, |s| s as u32),
            });
        }
        "scanlines" => {
            p.effects.retain(|e| !matches!(e, Effect::Scanlines { .. }));
            p.effects.push(Effect::Scanlines {
                period: f(first()?, "a period", 1.0, 1000.0)?,
                alpha: f(v.get(1).ok_or("an alpha is missing")?, "an alpha", 0.0, 1.0)?,
            });
        }
        "reflection" => {
            p.effects
                .retain(|e| !matches!(e, Effect::Reflection { .. }));
            p.effects.push(Effect::Reflection {
                angle: f(first()?, "an angle", -3600.0, 3600.0)?,
                alpha: f(v.get(1).ok_or("an alpha is missing")?, "an alpha", 0.0, 1.0)?,
            });
        }
        "blur-behind" => {
            p.effects
                .retain(|e| !matches!(e, Effect::BackdropBlur { .. }));
            p.effects.push(Effect::BackdropBlur {
                radius: f(first()?, "a radius", 0.0, 1000.0)?,
            });
        }
        // Text
        "font" => match first()? {
            Value::Map(m) => {
                if let Some(fam) = m.get("family") {
                    family(t, &fam.display())?;
                }
                if let Some(x) = m.get("size") {
                    t.size = f(x, "a font size", 1.0, 1024.0)?;
                }
                if let Some(x) = m.get("weight") {
                    t.weight = f(x, "a weight", 1.0, 1000.0)?;
                }
                if let Some(x) = m.get("stretch") {
                    t.width = f(x, "a stretch", 25.0, 200.0)?;
                }
            }
            o => return Err(format!("`font` takes a type role, found {}", o.type_name())),
        },
        "family" => family(t, &word(first()?))?,
        "font-size" => t.size = f(first()?, "a font size", 1.0, 1024.0)?,
        "weight" => t.weight = f(first()?, "a weight", 1.0, 1000.0)?,
        "stretch" => t.width = f(first()?, "a stretch", 25.0, 200.0)?,
        "tracking" => t.tracking = f(first()?, "tracking", -5.0, 5.0)?,
        "line-height" => t.line_height = Some(f(first()?, "a line height", 0.5, 4.0)?),
        "color" => t.color = colour(first()?)?,
        "text-align" => {
            t.align = match word(first()?).as_str() {
                "start" => TextAlign::Start,
                "center" => TextAlign::Center,
                "end" => TextAlign::End,
                o => return Err(format!("`{o}` is not start, center or end")),
            }
        }
        "upper" => t.upper = flag(first()?),
        "text-wrap" => t.wrap = flag(first()?),
        // `lines N` wraps at most N lines; `lines 1` is one line, so no wrapping.
        "lines" => {
            let n = f(first()?, "a line count", 1.0, 1000.0)?;
            if n.fract() != 0.0 {
                return Err("a line count is a whole number".to_owned());
            }
            t.max_lines = n as u16;
            t.wrap = t.max_lines != 1;
        }
        "key-hint" => c.key_hint = flag(first()?),
        "mono-font" => {
            let fam = word(first()?);
            if fonts::family(&fam).is_none() {
                return Err(format!("font family `{fam}` is not bundled with the app"));
            }
            t.mono = Some(Arc::from(fam.as_str()));
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn family(t: &mut TextCtx, name: &str) -> R<()> {
    if fonts::family(name).is_none() {
        return Err(format!("font family `{name}` is not bundled with the app"));
    }
    t.set_family(name);
    Ok(())
}

/// One, two (vertical, horizontal) or four (top, right, bottom, left) values.
fn sides(v: &[f32]) -> R<Sides<f32>> {
    Ok(match v {
        [a] => Sides::all(*a),
        [a, b] => Sides::symmetric(*a, *b),
        [a, b, c, d] => {
            let mut s = Sides::all(*a);
            s.right = *b;
            s.bottom = *c;
            s.left = *d;
            s
        }
        _ => return Err("takes 1, 2 or 4 values".to_owned()),
    })
}
