//! The skin expression language (design 27 §7), its limits (§8), and the formatters and
//! status helpers it calls (design 26 §2).
//!
//! Expressions are **total**: there is no assignment, no user function and no I/O, and
//! every evaluation ends. A tree of at most [`MAX_EXPR_LEN`] characters and
//! [`MAX_EXPR_DEPTH`] levels of nesting has bounded work, and the only loops are pipe
//! stages over a list the scope hands in.
//!
//! # Parsed once
//!
//! The loader parses every template and expression **once**, at load, and keeps the
//! [`Expr`] / [`Template`] in the cached skin (design 27 §9 step 3, issue #539). Binding a
//! route on a data change only evaluates. For that reason:
//!
//! - [`eval`] and [`Template::eval`] take the parsed tree. Nothing in [`eval`] calls
//!   the parser, so there is no path that re-parses text at bind time or at action time;
//! - an action argument that mentions an input variable (`value`, `on`; design 26 §4) is
//!   parsed at load like any other, and kept: [`Expr::mentions`] finds those, and the
//!   action evaluates the stored tree against a scope that binds them.
//!
//! # Syntax
//!
//! Precedence, loosest first: `?:` ternary, `else`, `|` pipes, `or`, `and`, `not`,
//! comparisons, unary `-`, postfix (`.f`, `?.f`, calls), primaries.
//!
//! A string with `{…}` is a [`Template`]; a value that is exactly one `{…}` is a typed
//! expression; `{{` and `}}` stand for literal braces (design 27 §7).
//!
//! # Errors
//!
//! [`ParseError`] and [`TemplateError`] carry a byte [`Span`] into the source text they
//! were parsed from (the template, for a template), and implement [`miette::Diagnostic`]
//! with that label, so the loader can attach the file and render them. [`EvalError`] is a
//! plain error: the loader reports it against the node being bound.

mod error;
mod eval;
mod format;
mod lex;
mod parse;
mod template;

#[cfg(test)]
mod proptests;
#[cfg(test)]
mod tests;

pub use error::{
    CallError, EvalError, ParseError, ParseErrorKind, Span, TemplateError, TemplateErrorKind,
};
pub use eval::{Scope, eval};
pub use parse::{BinOp, Expr, Stage, parse};
pub use template::{Seg, Template, parse_template};

/// The longest expression accepted, in characters (design 27 §8, S12). Longer is a
/// parse error.
pub const MAX_EXPR_LEN: usize = 512;

/// The deepest an expression may nest: parentheses, call arguments, `not`, unary `-`,
/// nested `?:` and `alpha=` each add a level, and the whole expression is level 1. It
/// keeps the recursive-descent parser (and the evaluator after it) within a small, known
/// stack use; realistic expressions nest two or three levels (design 27 §8).
pub const MAX_EXPR_DEPTH: usize = 32;

/// The most items `take` produces: the bound on repeats (S12, design 27 §8). A longer
/// result is cut, silently; data never makes a valid skin fail.
pub const MAX_LIST: usize = 1000;
