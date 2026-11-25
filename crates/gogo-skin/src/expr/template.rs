//! Templates: text with `{…}` holes (design 27 §7).

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
    /// Evaluate against a scope. `Text` and `Interp` give text; `Expr` gives whatever
    /// the expression computes. Never parses.
    pub fn eval(&self, scope: &dyn Scope) -> Result<Value, EvalError> {
        match self {
            Template::Text(t) => Ok(Value::Str(t.clone())),
            Template::Expr(e) => eval(e, scope),
            Template::Interp(segs) => {
                let mut out = String::new();
                for seg in segs {
                    match seg {
                        Seg::Lit(t) => out.push_str(t),
                        Seg::Expr(e) => out.push_str(&eval(e, scope)?.display()),
                    }
                }
                Ok(Value::Str(out))
            }
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
