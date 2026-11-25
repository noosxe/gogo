//! The skin's own AST (design 27 §9 steps 2 and 3).
//!
//! A [`Node`] is what a KDL node became: its name, arguments, properties and children,
//! with the file, offset and length of each. Every string value is **already parsed**: a
//! template or expression is a [`Template`] (design 27 §7, closing #539), an action's
//! `args` is a list of [`Term`]s, and `@token` is a [`Val::Token`]. Binding evaluates
//! these trees and never parses text.
//!
//! Names stay strings with spans. Resolving an action, route, section, part, variant or
//! setting name to its `&'static` contract spec, and the field and type checks, are
//! PR B3c's: they walk this tree and report at the span kept here.

use crate::diag::{Code, Collector};
use crate::expr::{Template, TemplateError, TemplateErrorKind, parse_template};
use crate::names::{is_ident, is_token_name};
use crate::raw::{Lit, RawArg, RawNode};
use crate::source::Span;

/// A value in the tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    /// A string, parsed: plain text (`Template::Text`), one `{expr}` or text with holes.
    Text(Template),
    /// `@name`: a token reference.
    Token(String),
    Num(f64),
    Bool(bool),
    Null,
    /// An action's `args`: whitespace-separated terms, each a parsed template.
    Terms(Vec<Term>),
    /// A string that failed to parse. It has been reported; checks skip it.
    Invalid,
}

impl Val {
    /// The text of a plain string (no `{…}` holes).
    pub fn plain(&self) -> Option<&str> {
        match self {
            Val::Text(Template::Text(s)) => Some(s),
            _ => None,
        }
    }

    pub fn is_expr(&self) -> bool {
        matches!(self, Val::Text(Template::Expr(_)))
    }

    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Val::Text(Template::Text(_)) => "text",
            Val::Text(_) => "a template",
            Val::Token(_) => "a token reference",
            Val::Num(_) => "a number",
            Val::Bool(_) => "a boolean",
            Val::Null => "null",
            Val::Terms(_) => "action arguments",
            Val::Invalid => "an invalid value",
        }
    }
}

/// One term of an action's `args`: `{$id}`, `files`, or `mode={value}`.
#[derive(Debug, Clone, PartialEq)]
pub struct Term {
    /// The name of `name=value`.
    pub name: Option<String>,
    pub value: Template,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub val: Val,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Prop {
    pub name: String,
    pub name_span: Span,
    pub arg: Arg,
}

/// A node: an element, a style declaration, a `let`, a `case`.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub name: String,
    pub name_span: Span,
    pub args: Vec<Arg>,
    pub props: Vec<Prop>,
    pub children: Vec<Node>,
    pub span: Span,
}

impl Node {
    pub fn prop(&self, name: &str) -> Option<&Prop> {
        self.props.iter().rev().find(|p| p.name == name)
    }

    /// The text of argument `i`, if it is a plain string.
    pub fn plain_arg(&self, i: usize) -> Option<&str> {
        self.args.get(i).and_then(|a| a.val.plain())
    }

    pub fn plain_prop(&self, name: &str) -> Option<&str> {
        self.prop(name).and_then(|p| p.arg.val.plain())
    }
}

/// Props whose string is a list of action arguments.
const TERM_PROPS: &[&str] = &["args", "link-args"];

/// Convert a raw node and its subtree, parsing every string. Errors go to `col` at the
/// scope it is in.
pub(crate) fn convert(col: &mut Collector, raw: &RawNode) -> Node {
    Node {
        name: raw.name.clone(),
        name_span: raw.name_span,
        args: raw.args.iter().map(|a| arg(col, a, false)).collect(),
        props: raw
            .props
            .iter()
            .map(|p| Prop {
                name: p.name.clone(),
                name_span: p.name_span,
                arg: arg(col, &p.value, TERM_PROPS.contains(&p.name.as_str())),
            })
            .collect(),
        children: raw.children.iter().map(|c| convert(col, c)).collect(),
        span: raw.span,
    }
}

fn arg(col: &mut Collector, a: &RawArg, terms: bool) -> Arg {
    let val = match &a.lit {
        Lit::Num(n) => Val::Num(*n),
        Lit::Bool(b) => Val::Bool(*b),
        Lit::Null => Val::Null,
        Lit::Str(s) if terms => match parse_terms(col, a, s) {
            Some(t) => Val::Terms(t),
            None => Val::Invalid,
        },
        Lit::Str(s) => match token_ref(s) {
            Some(name) => Val::Token(name.to_owned()),
            None => match parse_template(s) {
                Ok(t) => Val::Text(t),
                Err(e) => {
                    report(col, a, 0, &e);
                    Val::Invalid
                }
            },
        },
    };
    Arg { val, span: a.span }
}

/// `@name` as a whole string.
pub(crate) fn token_ref(s: &str) -> Option<&str> {
    s.strip_prefix('@').filter(|n| is_token_name(n))
}

/// Report a template error, pointing inside the string when its text is the string as
/// written, else at the whole value. `base` is where the template starts in the string.
fn report(col: &mut Collector, a: &RawArg, base: usize, e: &TemplateError) {
    let (code, label) = match &e.kind {
        TemplateErrorKind::Expr(k) => (Code::Expression, k.to_string()),
        k => (Code::Template, k.to_string()),
    };
    let span = match a.inner {
        Some(i) => Span::new(
            a.span.file,
            a.span.offset + i + base + e.span.offset,
            e.span.len,
        ),
        None => a.span,
    };
    let help = match &e.kind {
        TemplateErrorKind::Expr(_) => "expressions are described in design 27 §7",
        _ => "write `{{` and `}}` for a literal brace",
    };
    col.error(code, span, e.to_string()).label(label).help(help);
}

/// Split an `args` string into terms: whitespace outside `{…}`; inside, quotes hide braces.
pub(crate) fn split_terms(s: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let (mut depth, mut quote): (usize, Option<char>) = (0, None);
    let mut start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') if depth > 0 => quote = Some(c),
            (None, '{') => depth += 1,
            (None, '}') => depth = depth.saturating_sub(1),
            (None, c) if c.is_whitespace() && depth == 0 => {
                if let Some(st) = start.take() {
                    out.push((st, &s[st..i]));
                }
                continue;
            }
            _ => {}
        }
        start.get_or_insert(i);
    }
    if let Some(st) = start {
        out.push((st, &s[st..]));
    }
    out
}

fn parse_terms(col: &mut Collector, a: &RawArg, s: &str) -> Option<Vec<Term>> {
    let mut out = Vec::new();
    let mut ok = true;
    for (at, text) in split_terms(s) {
        // `name=value`: a lowercase name, then `=`.
        let (name, rest, off) = match text.split_once('=') {
            Some((n, v)) if is_ident(n) => (Some(n.to_owned()), v, n.len() + 1),
            _ => (None, text, 0),
        };
        match parse_template(rest) {
            Ok(value) => {
                let span = match a.inner {
                    Some(i) => Span::new(a.span.file, a.span.offset + i + at, text.len()),
                    None => a.span,
                };
                out.push(Term { name, value, span });
            }
            Err(e) => {
                report(col, a, at + off, &e);
                ok = false;
            }
        }
    }
    ok.then_some(out)
}
