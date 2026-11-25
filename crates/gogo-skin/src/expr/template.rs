//! Templates: text with `{…}` holes (design 27 §7).

use std::borrow::Cow;

use gogo_contract::Value;

use super::error::{EvalError, ParseErrorKind, Span, TemplateError, TemplateErrorKind};
use super::eval::{Scope, eval};
use super::parse::{Expr, parse};

/// A piece of interpolated text: `"Update {g.update?.to} available"`.
#[derive(Debug, Clone, PartialEq)]
pub enum Seg {
    Lit(String),
    Expr(Expr),
}

/// A property or argument string, parsed: plain text, a single typed expression, or text
/// with `{…}` holes.
#[derive(Debug, Clone, PartialEq)]
pub enum Template {
    /// No holes. `{{` and `}}` are already resolved to `{` and `}`.
    Text(String),
    /// Exactly one `{…}` and nothing else: the expression's own type is kept.
    Expr(Expr),
    /// Text and expressions, joined as text.
    Interp(Vec<Seg>),
}

impl Template {
    /// Evaluate against a scope, keeping the type: `Expr` gives whatever the expression
    /// computes (borrowed where it can be), `Text` and `Interp` give text. Never parses.
    ///
    /// A failure is a runtime failure of the route's binding (design 26 §8).
    pub fn eval<'a>(&'a self, scope: &'a dyn Scope) -> Result<Cow<'a, Value>, EvalError> {
        match self {
            Template::Expr(e) => eval(e, scope),
            _ => Ok(match self.eval_text(scope)? {
                Cow::Borrowed(t) => Cow::Owned(Value::Str(t.to_string())),
                Cow::Owned(t) => Cow::Owned(Value::Str(t)),
            }),
        }
    }

    /// Evaluate to text. `Text` is lent, not copied; a single expression is shown the way
    /// it would be inside a larger text. Never parses.
    pub fn eval_text<'a>(&'a self, scope: &'a dyn Scope) -> Result<Cow<'a, str>, EvalError> {
        match self {
            Template::Text(t) => Ok(Cow::Borrowed(t)),
            Template::Expr(e) => Ok(match eval(e, scope)? {
                Cow::Borrowed(Value::Str(t)) => Cow::Borrowed(t.as_str()),
                Cow::Owned(Value::Str(t)) => Cow::Owned(t),
                v => Cow::Owned(v.display()),
            }),
            Template::Interp(segs) => {
                // Literal text is known; each hole is guessed at a short word.
                let cap = segs
                    .iter()
                    .map(|seg| match seg {
                        Seg::Lit(t) => t.len(),
                        Seg::Expr(_) => 16,
                    })
                    .sum();
                let mut out = String::with_capacity(cap);
                for seg in segs {
                    match seg {
                        Seg::Lit(t) => out.push_str(t),
                        Seg::Expr(e) => match &*eval(e, scope)? {
                            Value::Str(t) => out.push_str(t),
                            v => out.push_str(&v.display()),
                        },
                    }
                }
                Ok(Cow::Owned(out))
            }
        }
    }

    /// Every bare name the template's expressions use, in source order (see
    /// [`Expr::names`]).
    pub fn names(&self) -> Vec<&str> {
        match self {
            Template::Text(_) => Vec::new(),
            Template::Expr(e) => e.names().into_iter().map(|n| n.name).collect(),
            Template::Interp(segs) => segs
                .iter()
                .filter_map(|s| match s {
                    Seg::Expr(e) => Some(e.names()),
                    Seg::Lit(_) => None,
                })
                .flatten()
                .map(|n| n.name)
                .collect(),
        }
    }

    /// True if any expression in the template mentions the bare name `name`.
    pub fn mentions(&self, name: &str) -> bool {
        match self {
            Template::Text(_) => false,
            Template::Expr(e) => e.mentions(name),
            Template::Interp(segs) => segs.iter().any(|s| match s {
                Seg::Lit(_) => false,
                Seg::Expr(e) => e.mentions(name),
            }),
        }
    }
}

/// Parse a string with `{…}` holes. `{{` and `}}` stand for a literal `{` and `}`; a
/// lone `}` outside a hole, or a `{` that is never closed, is an error. Every
/// expression inside is limited as [`parse`] limits it. Error spans are byte offsets
/// into `s`.
pub fn parse_template(s: &str) -> Result<Template, TemplateError> {
    let err = |kind, offset, len| TemplateError {
        kind,
        span: Span::new(offset, len),
    };
    let mut segs: Vec<Seg> = Vec::new();
    let mut lit = String::new();
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        match c {
            '{' if matches!(it.peek(), Some(&(_, '{'))) => {
                it.next();
                lit.push('{');
            }
            '}' if matches!(it.peek(), Some(&(_, '}'))) => {
                it.next();
                lit.push('}');
            }
            '}' => return Err(err(TemplateErrorKind::StrayCloseBrace, i, 1)),
            '{' => {
                // The hole runs to the first `}` outside a quoted string.
                let inner_start = i + 1;
                let mut quote: Option<char> = None;
                let mut close = None;
                for (j, d) in it.by_ref() {
                    match (quote, d) {
                        (Some(q), d) if d == q => quote = None,
                        (Some(_), _) => {}
                        (None, '\'' | '"') => quote = Some(d),
                        (None, '}') => {
                            close = Some(j);
                            break;
                        }
                        (None, _) => {}
                    }
                }
                let Some(j) = close else {
                    return Err(err(TemplateErrorKind::UnclosedBrace, i, 1));
                };
                if !lit.is_empty() {
                    segs.push(Seg::Lit(std::mem::take(&mut lit)));
                }
                let inner = &s[inner_start..j];
                let e = parse(inner).map_err(|e| TemplateError {
                    kind: TemplateErrorKind::Expr(e.kind),
                    span: e.span.shifted(inner_start),
                })?;
                segs.push(Seg::Expr(e));
            }
            c => lit.push(c),
        }
    }
    if !lit.is_empty() {
        segs.push(Seg::Lit(lit));
    }
    Ok(match (segs.len(), segs.pop()) {
        (0, _) | (_, None) => Template::Text(String::new()),
        (1, Some(Seg::Lit(t))) => Template::Text(t),
        (1, Some(Seg::Expr(e))) => Template::Expr(e),
        (_, Some(last)) => {
            segs.push(last);
            Template::Interp(segs)
        }
    })
}

/// The parse error kind inside a template error, if it is one.
impl TemplateError {
    pub fn expr_kind(&self) -> Option<&ParseErrorKind> {
        match &self.kind {
            TemplateErrorKind::Expr(k) => Some(k),
            _ => None,
        }
    }
}
