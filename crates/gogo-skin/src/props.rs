//! Style properties (design 27 §4 and §6): the table, and the check of one value.
//!
//! The same property names work as children of a `style` and as inline properties of an
//! element (`padding 0 16` and `padding="0 16"`). A value is checked against the shape of
//! its property: how many values, of what type and in what range. `@token` references
//! must name a token of the right type; `transparent` is the one colour word.
//!
//! The checked value is not kept in a typed form here: the typed output depends on the
//! IR types (PR B4). The tree keeps the parsed arguments, and binding reads them again.

use crate::ast::{Arg, Val};
use crate::diag::{Code, Collector};
use crate::expr::Template;
use crate::fonts;
use crate::names::suggest;
use crate::source::Span;
use crate::tokens::{Rgba, TokenKind, TokenSet, TokenValue};

/// What a property's value looks like.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Shape {
    /// A length: a number, `auto`, or a percentage.
    Dim,
    Num(f64, f64),
    Int(i64, i64),
    Flag,
    Choice(&'static [&'static str]),
    Colour,
    /// Lengths: one of these counts of numbers.
    Sides(&'static [usize], f64, f64),
    /// Lengths, `auto` and percentages: one of these counts.
    DimSides(&'static [usize]),
    Tracks,
    /// `<width> <colour>`.
    Stroke,
    /// `<side> <width> <colour>`.
    AccentEdge,
    Gradient,
    Shadow,
    InnerShadow,
    Glow,
    Bloom,
    Noise,
    Scanlines,
    Reflection,
    BlurBehind,
    Offset,
    /// `none` alone.
    NoneOnly,
    /// A type role token: `font @font.display`.
    FontRole,
    /// A bundled family name, or a text token.
    Family,
}

pub(crate) struct PropSpec {
    pub name: &'static str,
    pub shape: Shape,
    /// A string with spaces is several values (`padding="0 16"`), not one.
    pub splits: bool,
}

const fn p(name: &'static str, shape: Shape) -> PropSpec {
    PropSpec {
        name,
        shape,
        splits: !matches!(shape, Shape::Family),
    }
}

const ALIGN: &[&str] = &["start", "center", "end", "stretch", "baseline"];
const SIDES: &[&str] = &["top", "right", "bottom", "left"];

/// Every style property, in the order design 27 §4 lists them.
pub(crate) static STYLE_PROPS: &[PropSpec] = &[
    // Size
    p("w", Shape::Dim),
    p("h", Shape::Dim),
    p("min-w", Shape::Dim),
    p("max-w", Shape::Dim),
    p("min-h", Shape::Dim),
    p("max-h", Shape::Dim),
    p("size", Shape::Dim),
    p("aspect", Shape::Num(0.01, 100.0)),
    // Flex and grid
    p("grow", Shape::Num(0.0, 1000.0)),
    p("shrink", Shape::Num(0.0, 1000.0)),
    p("gap", Shape::Sides(&[1, 2], 0.0, 10_000.0)),
    p("row-gap", Shape::Num(0.0, 10_000.0)),
    p("direction", Shape::Choice(&["row", "column"])),
    p("wrap", Shape::Flag),
    p("align", Shape::Choice(ALIGN)),
    p("align-self", Shape::Choice(ALIGN)),
    p(
        "justify",
        Shape::Choice(&["start", "center", "end", "between", "around"]),
    ),
    p("columns", Shape::Tracks),
    p("rows", Shape::Tracks),
    p("col-span", Shape::Int(1, 64)),
    // Box
    p("padding", Shape::Sides(&[1, 2, 4], 0.0, 10_000.0)),
    p("margin", Shape::Sides(&[1, 2, 4], -10_000.0, 10_000.0)),
    p("inset", Shape::DimSides(&[1, 2, 4])),
    // Paint
    p("fill", Shape::Colour),
    p("gradient", Shape::Gradient),
    p("border", Shape::Stroke),
    p("border-top", Shape::Stroke),
    p("border-right", Shape::Stroke),
    p("border-bottom", Shape::Stroke),
    p("border-left", Shape::Stroke),
    p("accent-edge", Shape::AccentEdge),
    p("radius", Shape::Sides(&[1, 4], 0.0, 10_000.0)),
    p("opacity", Shape::Num(0.0, 1.0)),
    p("shape", Shape::Choice(&["circle", "rect"])),
    p("offset", Shape::Offset),
    p("ring", Shape::Stroke),
    // Effects
    p("shadow", Shape::Shadow),
    p("inner-shadow", Shape::InnerShadow),
    p("glow", Shape::Glow),
    p("bloom-text", Shape::Bloom),
    p("noise", Shape::Noise),
    p("scanlines", Shape::Scanlines),
    p("reflection", Shape::Reflection),
    p("blur-behind", Shape::BlurBehind),
    p("effects", Shape::NoneOnly),
    // Text
    p("font", Shape::FontRole),
    p("family", Shape::Family),
    p("font-size", Shape::Num(1.0, 1024.0)),
    p("weight", Shape::Num(1.0, 1000.0)),
    p("stretch", Shape::Num(25.0, 200.0)),
    p("tracking", Shape::Num(-5.0, 5.0)),
    p("line-height", Shape::Num(0.5, 4.0)),
    p("color", Shape::Colour),
    p("text-align", Shape::Choice(&["start", "center", "end"])),
    p("upper", Shape::Flag),
    p("text-wrap", Shape::Flag),
    p("lines", Shape::Int(1, 1000)),
    // Hints
    p("key-hint", Shape::Flag),
    p("mono-font", Shape::Family),
];

/// The style property called `name`.
pub(crate) fn style_prop(name: &str) -> Option<&'static PropSpec> {
    STYLE_PROPS.iter().find(|s| s.name == name)
}

/// Properties that may be written more than once in one block.
pub(crate) fn repeatable(name: &str) -> bool {
    name == "shadow"
}

/// A "did you mean" help line for an unknown property name.
pub(crate) fn unknown_help(name: &str, extra: &[&str]) -> String {
    let all = STYLE_PROPS
        .iter()
        .map(|s| s.name)
        .chain(extra.iter().copied());
    match suggest(name, all) {
        Some(s) => format!("did you mean `{s}`?"),
        None => "design 27 §4 lists the properties".to_owned(),
    }
}

/// How dynamic values (`"{…}"`) are treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dynamic {
    /// Styles are static: only a colour may be an expression (`"{@warn alpha=.5}"`).
    ColourOnly,
    /// Inline properties may be expressions; they are checked when bound.
    Any,
}

/// What a value check needs to know.
pub(crate) struct Ctx<'a> {
    pub tokens: &'a TokenSet,
    /// False when the tokens failed to load: unknown tokens are not reported again.
    pub tokens_ok: bool,
    pub dynamic: Dynamic,
}

/// What one block (a style, a state, or one element's properties) says about its font,
/// to catch `stretch` on a static family.
#[derive(Default)]
pub(crate) struct Facts {
    family: Option<String>,
    stretch: Option<Span>,
}

impl Facts {
    /// At the end of the block: `stretch` needs a variable family set in the same block.
    pub fn finish(self, col: &mut Collector) {
        if let (Some(sp), Some(fam)) = (self.stretch, &self.family)
            && fonts::family(fam).is_some_and(|f| !f.variable)
        {
            col.error(
                Code::FontStretch,
                sp,
                format!("`stretch` needs a variable font; `{fam}` is static"),
            )
            .help("`stretch` is the `wdth` axis: use it with `Noto Sans`");
        }
    }
}

enum Atom {
    Num(f64, Span),
    Word(String, Span),
    Token(String, Span),
    /// An expression or a template: `is_expr` when it is exactly one `{…}`.
    Dyn(bool, Span),
    Bool(bool, Span),
    Null(Span),
}

impl Atom {
    fn span(&self) -> Span {
        match self {
            Atom::Num(_, s)
            | Atom::Word(_, s)
            | Atom::Token(_, s)
            | Atom::Dyn(_, s)
            | Atom::Bool(_, s)
            | Atom::Null(s) => *s,
        }
    }

    fn describe(&self) -> String {
        match self {
            Atom::Num(n, _) => format!("the number {n}"),
            Atom::Word(w, _) => format!("`{w}`"),
            Atom::Token(t, _) => format!("`@{t}`"),
            Atom::Dyn(..) => "an expression".to_owned(),
            Atom::Bool(b, _) => format!("`#{b}`"),
            Atom::Null(_) => "null".to_owned(),
        }
    }
}

struct Bad {
    code: Code,
    span: Span,
    msg: String,
    help: Option<String>,
}

type R<T> = Result<T, Bad>;

struct Cur<'a> {
    atoms: Vec<Atom>,
    i: usize,
    whole: Span,
    ctx: &'a Ctx<'a>,
}

impl Cur<'_> {
    fn bad<T>(&self, span: Span, msg: impl Into<String>) -> R<T> {
        Err(Bad {
            code: Code::PropertyValue,
            span,
            msg: msg.into(),
            help: None,
        })
    }

    fn peek(&self) -> Option<&Atom> {
        self.atoms.get(self.i)
    }

    fn left(&self) -> usize {
        self.atoms.len() - self.i
    }

    fn next(&mut self, what: &str) -> R<Atom> {
        match self.atoms.get_mut(self.i) {
            Some(a) => {
                self.i += 1;
                Ok(std::mem::replace(a, Atom::Null(self.whole)))
            }
            None => self.bad(self.whole, format!("expected {what}")),
        }
    }

    fn end(&self) -> R<()> {
        match self.atoms.get(self.i) {
            Some(extra) => self.bad(extra.span(), "unexpected extra value"),
            None => Ok(()),
        }
    }

    fn dynamic(&self, a: &Atom, is_expr: bool, colour: bool) -> R<()> {
        let ok = match self.ctx.dynamic {
            Dynamic::Any => true,
            Dynamic::ColourOnly => colour && is_expr,
        };
        if ok {
            Ok(())
        } else {
            Err(Bad {
                code: Code::PropertyValue,
                span: a.span(),
                msg: if colour {
                    "a style takes a colour, a token or one `{@token alpha=…}`".to_owned()
                } else {
                    "styles are static: use a number or a token here".to_owned()
                },
                help: Some("`{…}` expressions are for elements, not styles".to_owned()),
            })
        }
    }

    fn token(&self, name: &str, span: Span, want: TokenKind) -> R<()> {
        if !self.ctx.tokens_ok {
            return Ok(());
        }
        match self.ctx.tokens.kind_of(name) {
            Some(k) if k == want => Ok(()),
            Some(k) => Err(Bad {
                code: Code::TokenRef,
                span,
                msg: format!("`@{name}` is {}, expected {}", k.noun(), want.noun()),
                help: None,
            }),
            None => {
                let names = self.ctx.tokens.iter().map(|(n, _)| n);
                Err(Bad {
                    code: Code::TokenRef,
                    span,
                    msg: format!("unknown token `@{name}`"),
                    help: suggest(name, names).map(|s| format!("did you mean `@{s}`?")),
                })
            }
        }
    }

    /// A number in `lo..=hi`: `None` when it isn't known until bound.
    fn num(&mut self, what: &str, lo: f64, hi: f64) -> R<Option<f64>> {
        let a = self.next(what)?;
        let n = match &a {
            Atom::Num(n, _) => *n,
            Atom::Word(w, _) => match w.parse::<f64>() {
                Ok(n) => n,
                Err(_) => {
                    return self.bad(a.span(), format!("expected {what}, found {}", a.describe()));
                }
            },
            Atom::Token(t, sp) => {
                self.token(t, *sp, TokenKind::Number)?;
                return Ok(None);
            }
            Atom::Dyn(e, _) => {
                self.dynamic(&a, *e, false)?;
                return Ok(None);
            }
            _ => return self.bad(a.span(), format!("expected {what}, found {}", a.describe())),
        };
        if !n.is_finite() || n < lo || n > hi {
            return self.bad(
                a.span(),
                format!("{what} must be from {lo} to {hi}, found {n}"),
            );
        }
        Ok(Some(n))
    }

    fn int(&mut self, what: &str, lo: i64, hi: i64) -> R<()> {
        let span = self.peek().map_or(self.whole, Atom::span);
        if let Some(n) = self.num(what, lo as f64, hi as f64)?
            && n.fract() != 0.0
        {
            return self.bad(span, format!("{what} must be a whole number"));
        }
        Ok(())
    }

    fn dim(&mut self, what: &str) -> R<()> {
        if let Some(Atom::Word(w, _)) = self.peek()
            && (w == "auto"
                || w.strip_suffix('%')
                    .is_some_and(|n| n.parse::<f64>().is_ok()))
        {
            self.i += 1;
            return Ok(());
        }
        self.num(what, -100_000.0, 100_000.0).map(|_| ())
    }

    fn colour(&mut self) -> R<()> {
        let a = self.next("a colour")?;
        match &a {
            Atom::Word(w, _) if w == "transparent" || Rgba::parse(w).is_some() => Ok(()),
            Atom::Word(w, _) => Err(Bad {
                code: Code::PropertyValue,
                span: a.span(),
                msg: format!("`{w}` is not a colour"),
                help: Some(
                    "colours are \"#rrggbb\", \"#rrggbbaa\", `transparent` or a colour token"
                        .to_owned(),
                ),
            }),
            Atom::Token(t, sp) => self.token(t, *sp, TokenKind::Colour),
            Atom::Dyn(e, _) => self.dynamic(&a, *e, true),
            _ => self.bad(
                a.span(),
                format!("expected a colour, found {}", a.describe()),
            ),
        }
    }

    fn word(&mut self, choices: &[&str]) -> R<()> {
        let a = self.next("a word")?;
        match &a {
            Atom::Word(w, _) if choices.contains(&w.as_str()) => Ok(()),
            Atom::Dyn(e, _) => self.dynamic(&a, *e, false),
            _ => Err(Bad {
                code: Code::PropertyValue,
                span: a.span(),
                msg: format!("{} is not one of the choices", a.describe()),
                help: Some(format!("use one of: {}", choices.join(", "))),
            }),
        }
    }

    fn flag(&mut self) -> R<()> {
        let a = self.next("`#true` or `#false`")?;
        match &a {
            Atom::Bool(..) => Ok(()),
            Atom::Dyn(e, _) => self.dynamic(&a, *e, false),
            _ => self.bad(
                a.span(),
                format!("expected `#true` or `#false`, found {}", a.describe()),
            ),
        }
    }

    fn is_none(&self) -> bool {
        matches!(self.peek(), Some(Atom::Word(w, _)) if w == "none")
    }
}

fn atoms(args: &[&Arg], split: bool) -> Option<Vec<Atom>> {
    let mut out = Vec::new();
    for a in args {
        match &a.val {
            Val::Num(n) => out.push(Atom::Num(*n, a.span)),
            Val::Bool(b) => out.push(Atom::Bool(*b, a.span)),
            Val::Null => out.push(Atom::Null(a.span)),
            Val::Token(t) => out.push(Atom::Token(t.clone(), a.span)),
            Val::Text(Template::Text(s)) => {
                if split {
                    out.extend(s.split_whitespace().map(|w| match w.parse::<f64>() {
                        Ok(n) if n.is_finite() => Atom::Num(n, a.span),
                        _ => Atom::Word(w.to_owned(), a.span),
                    }));
                } else {
                    out.push(Atom::Word(s.clone(), a.span));
                }
            }
            Val::Text(t) => out.push(Atom::Dyn(matches!(t, Template::Expr(_)), a.span)),
            Val::Terms(_) => out.push(Atom::Dyn(false, a.span)),
            Val::Invalid => return None,
        }
    }
    Some(out)
}

/// Check the values `args` of property `spec`, written at `at`. Reports the first problem.
pub(crate) fn check(
    col: &mut Collector,
    ctx: &Ctx<'_>,
    facts: &mut Facts,
    spec: &PropSpec,
    args: &[&Arg],
    at: Span,
) {
    let Some(atoms) = atoms(args, spec.splits) else {
        return;
    };
    // Facts for the stretch rule: read before the check consumes the atoms.
    match (spec.name, atoms.first()) {
        ("family", Some(Atom::Word(w, _))) => facts.family = Some(w.clone()),
        ("font", Some(Atom::Token(t, _))) => {
            if let Some(TokenValue::Font(f)) = ctx.tokens.get(t).map(|t| &t.value) {
                facts.family = Some(f.family.clone());
            }
        }
        ("stretch", Some(a)) => facts.stretch = Some(a.span()),
        _ => {}
    }
    let mut cur = Cur {
        atoms,
        i: 0,
        whole: at,
        ctx,
    };
    if let Err(b) = shape(&mut cur, spec) {
        let f = col.error(b.code, b.span, format!("{}: {}", spec.name, b.msg));
        if let Some(h) = b.help {
            f.help(h);
        }
    } else if let Some((name, span)) = args.iter().find_map(|a| family_token(spec, ctx, a))
        && fonts::family(&name).is_none()
    {
        col.error(
            Code::FontFamily,
            span,
            format!("font family `{name}` (from a token) is not bundled with the app"),
        )
        .help(format!("the bundled families are {}", fonts::names()));
    } else if let Some(a) = args.iter().find_map(|a| family_of(spec, a)) {
        // A family is checked where it is named.
        let (name, span) = a;
        if fonts::family(name).is_none() {
            col.error(
                Code::FontFamily,
                span,
                format!("font family `{name}` is not bundled with the app"),
            )
            .help(format!("the bundled families are {}", fonts::names()));
        }
    }
}

/// The family a `family @token` names, when the token is a text token: checked like a
/// written name. A template (`family="{…}"`) is only known when bound (design 27 §4).
fn family_token(spec: &PropSpec, ctx: &Ctx<'_>, a: &Arg) -> Option<(String, Span)> {
    if !matches!(spec.shape, Shape::Family) || !ctx.tokens_ok {
        return None;
    }
    let Val::Token(t) = &a.val else { return None };
    match ctx.tokens.get(t).map(|t| &t.value) {
        Some(TokenValue::Text(s)) => Some((s.clone(), a.span)),
        _ => None,
    }
}

/// The family a `family` or `mono-font` value names, as plain text.
fn family_of<'a>(spec: &PropSpec, a: &'a Arg) -> Option<(&'a str, Span)> {
    if matches!(spec.shape, Shape::Family) {
        a.val.plain().map(|s| (s, a.span))
    } else {
        None
    }
}

fn shape(c: &mut Cur<'_>, spec: &PropSpec) -> R<()> {
    match spec.shape {
        Shape::Dim => {
            c.dim("a length")?;
            c.end()
        }
        Shape::Num(lo, hi) => {
            c.num("a number", lo, hi)?;
            c.end()
        }
        Shape::Int(lo, hi) => {
            c.int("a whole number", lo, hi)?;
            c.end()
        }
        Shape::Flag => {
            c.flag()?;
            c.end()
        }
        Shape::Choice(ch) => {
            c.word(ch)?;
            c.end()
        }
        Shape::Colour => {
            c.colour()?;
            c.end()
        }
        Shape::Sides(counts, lo, hi) => {
            if !counts.contains(&c.left()) {
                let list: Vec<String> = counts.iter().map(usize::to_string).collect();
                return c.bad(
                    c.whole,
                    format!("takes {} values, found {}", list.join(" or "), c.left()),
                );
            }
            while c.left() > 0 {
                c.num("a number", lo, hi)?;
            }
            Ok(())
        }
        Shape::DimSides(counts) => {
            if !counts.contains(&c.left()) {
                let list: Vec<String> = counts.iter().map(usize::to_string).collect();
                return c.bad(
                    c.whole,
                    format!("takes {} values, found {}", list.join(" or "), c.left()),
                );
            }
            while c.left() > 0 {
                c.dim("a length")?;
            }
            Ok(())
        }
        Shape::Tracks => tracks(c),
        Shape::Stroke => {
            c.num("a width", 0.0, 1000.0)?;
            c.colour()?;
            c.end()
        }
        Shape::AccentEdge => {
            c.word(SIDES)?;
            c.num("a width", 0.0, 1000.0)?;
            c.colour()?;
            c.end()
        }
        Shape::Gradient => gradient(c),
        Shape::Shadow => {
            if c.is_none() {
                c.i += 1;
                return c.end();
            }
            for what in ["x", "y"] {
                c.num(what, -10_000.0, 10_000.0)?;
            }
            c.num("a blur radius", 0.0, 10_000.0)?;
            c.colour()?;
            if c.left() > 0 {
                c.num("a spread", -10_000.0, 10_000.0)?;
            }
            c.end()
        }
        Shape::InnerShadow => {
            for what in ["x", "y"] {
                c.num(what, -10_000.0, 10_000.0)?;
            }
            c.num("a blur radius", 0.0, 10_000.0)?;
            c.colour()?;
            c.end()
        }
        Shape::Glow => {
            let mut layers = 0;
            while c.left() > 0 {
                layers += 1;
                if layers > MAX_GLOW_LAYERS {
                    return c.bad(
                        c.peek().map_or(c.whole, Atom::span),
                        format!("at most {MAX_GLOW_LAYERS} glow layers"),
                    );
                }
                c.num("a radius", 0.0, 10_000.0)?;
                c.colour()?;
            }
            if layers == 0 {
                return c.bad(c.whole, "expected a radius and a colour");
            }
            Ok(())
        }
        Shape::Bloom => {
            if c.is_none() {
                c.i += 1;
                return c.end();
            }
            c.num("a radius", 0.0, 10_000.0)?;
            c.colour()?;
            c.end()
        }
        Shape::Noise => {
            c.num("an amount", 0.0, 1.0)?;
            if c.left() > 0 {
                c.int("a seed", 0, i64::from(u32::MAX))?;
            }
            c.end()
        }
        Shape::Scanlines => {
            c.num("a period in pixels", 1.0, 1000.0)?;
            c.num("an alpha", 0.0, 1.0)?;
            c.end()
        }
        Shape::Reflection => {
            c.num("an angle in degrees", -3600.0, 3600.0)?;
            c.num("an alpha", 0.0, 1.0)?;
            c.end()
        }
        Shape::BlurBehind => {
            c.num("a radius", 0.0, 1000.0)?;
            c.end()
        }
        Shape::Offset => {
            c.num("x", -10_000.0, 10_000.0)?;
            c.num("y", -10_000.0, 10_000.0)?;
            c.end()
        }
        Shape::NoneOnly => {
            if c.is_none() {
                c.i += 1;
                c.end()
            } else {
                c.bad(
                    c.peek().map_or(c.whole, Atom::span),
                    "the only value is `none`",
                )
            }
        }
        Shape::FontRole => {
            let a = c.next("a type role")?;
            match &a {
                Atom::Token(t, sp) => c.token(t, *sp, TokenKind::Font)?,
                Atom::Dyn(e, _) => c.dynamic(&a, *e, false)?,
                _ => {
                    return Err(Bad {
                        code: Code::PropertyValue,
                        span: a.span(),
                        msg: format!("expected a type role token, found {}", a.describe()),
                        help: Some(
                            "write `font @font.title`; the roles are in design 26 §6.1".to_owned(),
                        ),
                    });
                }
            }
            c.end()
        }
        Shape::Family => {
            let a = c.next("a font family")?;
            match &a {
                Atom::Word(..) => {}
                Atom::Token(t, sp) => c.token(t, *sp, TokenKind::Text)?,
                Atom::Dyn(e, _) => c.dynamic(&a, *e, false)?,
                _ => {
                    return c.bad(
                        a.span(),
                        format!("expected a font family, found {}", a.describe()),
                    );
                }
            }
            c.end()
        }
    }
}

/// The most layers a `glow` has.
pub(crate) const MAX_GLOW_LAYERS: usize = 4;
/// The most stops a `gradient` has.
pub(crate) const MAX_GRADIENT_STOPS: usize = 8;
/// The most tracks a `columns` or `rows` has.
pub(crate) const MAX_TRACKS: usize = 64;

fn tracks(c: &mut Cur<'_>) -> R<()> {
    if c.left() == 0 {
        return c.bad(c.whole, "expected a count or a list of tracks");
    }
    // A single number is a count of equal columns.
    if c.left() == 1 && matches!(c.peek(), Some(Atom::Num(..))) {
        c.int("a track count", 1, MAX_TRACKS as i64)?;
        return Ok(());
    }
    if c.left() > MAX_TRACKS {
        return c.bad(c.whole, format!("at most {MAX_TRACKS} tracks"));
    }
    while c.left() > 0 {
        if let Some(Atom::Word(w, sp)) = c.peek() {
            let ok = w == "auto"
                || w.strip_suffix("fr")
                    .is_some_and(|n| n.parse::<f64>().is_ok_and(|n| n > 0.0));
            if !ok {
                let msg = format!("`{w}` is not a track: use a number, `auto` or `<n>fr`");
                return c.bad(*sp, msg);
            }
            c.i += 1;
        } else {
            c.num("a track size", 0.0, 100_000.0)?;
        }
    }
    Ok(())
}

fn gradient(c: &mut Cur<'_>) -> R<()> {
    let a = c.next("`linear` or `radial`")?;
    let kind = match &a {
        Atom::Word(w, _) if w == "linear" || w == "radial" => w.clone(),
        _ => {
            return Err(Bad {
                code: Code::PropertyValue,
                span: a.span(),
                msg: format!("{} is not a gradient kind", a.describe()),
                help: Some(
                    "`gradient linear <angle> <colours…>` or `gradient radial <colours…>`"
                        .to_owned(),
                ),
            });
        }
    };
    if kind == "linear" {
        c.num("an angle in degrees", -3600.0, 3600.0)?;
    }
    let stops = c.left();
    if stops < 2 {
        return c.bad(c.whole, "a gradient needs at least two colours");
    }
    if stops > MAX_GRADIENT_STOPS {
        return c.bad(
            c.whole,
            format!("a gradient has at most {MAX_GRADIENT_STOPS} colours"),
        );
    }
    while c.left() > 0 {
        c.colour()?;
    }
    Ok(())
}
