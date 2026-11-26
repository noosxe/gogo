//! The skin expression language (design 27 §7), its limits (§8), and the formatters and
//! status helpers it calls (design 26 §2).
//!
//! Expressions have no assignment, no user function and no I/O, and every evaluation
//! ends: a tree of at most [`MAX_EXPR_LEN`] characters and [`MAX_EXPR_DEPTH`] levels has
//! bounded work, and the only loops are pipe stages over a list the scope hands in.
//!
//! [`eval`] returns a `Result`. A failure at run time (a field on a null without `?.`, a
//! call error from the app, a type mismatch the load-time checks don't cover) fails the
//! binding of the route that evaluated the expression, and the route falls back
//! (design 26 §8).
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
//! Precedence, loosest first: `?:` ternary (right-associative), `else`, `|` pipes
//! (all left-associative), `or`, `and`, `not`, comparisons (non-associative: `a < b < c`
//! is a [`ParseErrorKind::ChainedComparison`]), unary `-`, postfix (`.f`, `?.f`, calls),
//! primaries. `sort -key` and `@token alpha=x` are special syntax, and `true`, `false`
//! and `null` are literals (design 27 §7).
//!
//! A string with `{…}` is a [`Template`]; a value that is exactly one `{…}` is a typed
//! expression; `{{` and `}}` stand for literal braces (design 27 §7).
//!
//! # Bare names
//!
//! A bare name is a variable when the scope binds one; otherwise it is an enum word of
//! the contract. The loader checks that every name is one or the other, with
//! [`Expr::names`] (design 27 §7); evaluating an unbound name gives its text. A bare
//! operand of `==` or `!=` that is an enum word becomes [`Expr::Word`] at load, so it is
//! never read as the source of the same name.
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
    CallError, EvalError, FieldList, Found, ParseError, ParseErrorKind, Span, TemplateError,
    TemplateErrorKind,
};
pub use eval::{Scope, eval};
pub use parse::{BinOp, Expr, NameUse, Stage, parse};
pub use template::{Seg, Template, parse_template};

/// The longest expression accepted, in characters (design 27 §8, S12). Longer is a
/// parse error.
pub const MAX_EXPR_LEN: usize = 512;

/// The deepest an expression may be: the levels of its tree, with the root as level 1.
/// Every node counts, so a chain such as `a | len | len`, `a.b.c` or `a and a and a` adds
/// a level per link; parentheses count as levels too. It bounds the recursion of the
/// parser, the evaluator, `Drop` and `PartialEq` alike. Realistic expressions are two or
/// three levels deep (design 27 §8). This is not the KDL node nesting depth of 48.
pub const MAX_EXPR_DEPTH: usize = 32;

/// The most items `take` produces: the bound on repeats (S12, design 27 §8). A longer
/// result is cut, silently; data never makes a valid skin fail.
pub const MAX_LIST: usize = 1000;
