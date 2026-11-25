//! Errors of the expression language. Parse errors carry a byte span; so do the
//! evaluation errors that come from a field or a call.

use miette::{Diagnostic, LabeledSpan, SourceSpan};

use super::parse::BinOp;

/// A byte range in the source text: where something is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Byte offset of the first byte.
    pub offset: usize,
    /// Length in bytes.
    pub len: usize,
}

impl Span {
    pub(crate) fn new(offset: usize, len: usize) -> Self {
        Span { offset, len }
    }

    pub(crate) fn shifted(self, by: usize) -> Self {
        Span {
            offset: self.offset.saturating_add(by),
            len: self.len,
        }
    }

    /// The span from the start of `self` to the end of `other`.
    pub(crate) fn to(self, other: Span) -> Span {
        let end = other.offset.saturating_add(other.len);
        Span {
            offset: self.offset,
            len: end.saturating_sub(self.offset),
        }
    }
}

impl From<Span> for SourceSpan {
    fn from(s: Span) -> Self {
        SourceSpan::new(s.offset.into(), s.len)
    }
}

/// What the parser found where it expected something else.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Found {
    #[error("end of expression")]
    End,
    #[error("`{0}`")]
    Ident(String),
    #[error("a number")]
    Number,
    #[error("a string")]
    Str,
    #[error("`${0}`")]
    Param(String),
    #[error("`@{0}`")]
    Token(String),
    #[error("`.`")]
    Dot,
    #[error("`?.`")]
    QDot,
    #[error("`(`")]
    LParen,
    #[error("`)`")]
    RParen,
    #[error("`,`")]
    Comma,
    #[error("`|`")]
    Pipe,
    #[error("`?`")]
    Question,
    #[error("`:`")]
    Colon,
    #[error("`=`")]
    Assign,
    #[error("`{}`", .0.symbol())]
    Op(BinOp),
    #[error("`-`")]
    Minus,
}

/// What is wrong with an expression's text.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ParseErrorKind {
    #[error("expression is {got} characters long; the limit is {max}")]
    TooLong { got: usize, max: usize },
    #[error("expression nests deeper than {max} levels")]
    TooDeep { max: usize },
    #[error("unexpected character `{0}`")]
    UnexpectedChar(char),
    #[error("unterminated string")]
    UnterminatedString,
    /// The number's text is the error's span.
    #[error("bad number")]
    BadNumber,
    #[error("`{0}` must be followed by a name")]
    MissingName(char),
    #[error("expected {0}")]
    Expected(&'static str),
    #[error("unexpected {0}")]
    Unexpected(Found),
    #[error("unknown pipe stage `{0}`")]
    UnknownStage(String),
    #[error("unexpected {0} after the expression")]
    TrailingInput(Found),
    #[error("comparisons don't chain: put parentheses round one of them")]
    ChainedComparison,
}

/// An expression that does not parse, with where in its source text.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{kind}")]
pub struct ParseError {
    pub kind: ParseErrorKind,
    pub span: Span,
}

impl ParseError {
    pub(crate) fn new(kind: ParseErrorKind, span: Span) -> Self {
        ParseError { kind, span }
    }
}

impl Diagnostic for ParseError {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        Some(Box::new("skin::expr"))
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        Some(Box::new(std::iter::once(LabeledSpan::new_with_span(
            Some(self.kind.to_string()),
            self.span,
        ))))
    }
}

/// What is wrong with a template's text.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TemplateErrorKind {
    #[error("`{{` opens an expression that is never closed; write `{{{{` for a literal brace")]
    UnclosedBrace,
    #[error("`}}` closes nothing; write `}}}}` for a literal brace")]
    StrayCloseBrace,
    #[error("in the expression: {0}")]
    Expr(ParseErrorKind),
}

/// A template that does not parse. The span is into the template text.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{kind}")]
pub struct TemplateError {
    pub kind: TemplateErrorKind,
    pub span: Span,
}

impl Diagnostic for TemplateError {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        Some(Box::new("skin::template"))
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        Some(Box::new(std::iter::once(LabeledSpan::new_with_span(
            Some(self.kind.to_string()),
            self.span,
        ))))
    }
}

/// A failure inside a source or helper the scope called.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    /// What the call asked for does not exist.
    #[error("not found")]
    NotFound,
    /// An argument is not one the source accepts.
    #[error("argument {index} is not valid: expected {expected}")]
    BadArgument {
        /// Zero-based position of the argument.
        index: usize,
        /// What the source wanted there.
        expected: &'static str,
    },
    /// The source cannot answer right now.
    #[error("the source is unavailable")]
    Unavailable,
}

/// The field names of a record, for the "no such field" message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldList(pub Vec<String>);

impl std::fmt::Display for FieldList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, k) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            f.write_str(k)?;
        }
        Ok(())
    }
}

/// Why an expression could not be evaluated. A runtime failure fails the binding of the
/// route that evaluated it, and the route falls back (design 26 §8). Variants that come
/// from a field or a call in the text carry that node's [`Span`] into the expression.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EvalError {
    #[error("unknown route parameter `${0}`")]
    UnknownParam(String),
    #[error("unknown token `@{0}`")]
    UnknownToken(String),
    #[error("alpha must be a number")]
    AlphaNotNumber,
    #[error("`@{0}` is not a colour")]
    NotAColour(String),
    #[error("no field `{name}` (fields: {fields})")]
    NoField {
        name: String,
        fields: FieldList,
        span: Span,
    },
    #[error("`.{name}` on a {ty} value")]
    FieldOnType {
        name: String,
        ty: &'static str,
        span: Span,
    },
    #[error("unknown source or helper `{name}`")]
    UnknownCall { name: String, span: Span },
    #[error("`{name}` failed: {source}")]
    Call {
        name: String,
        span: Span,
        #[source]
        source: CallError,
    },
    #[error("`{name}` takes {expected} argument(s), got {got}")]
    Arity {
        name: String,
        expected: usize,
        got: usize,
        span: Span,
    },
    #[error("`-` needs a number, got {0}")]
    NegNeedsNumber(&'static str),
    #[error("can't compare {left} with {right}")]
    Compare {
        left: &'static str,
        right: &'static str,
    },
    #[error("`{stage}` needs a list, got {got}")]
    NeedsList {
        stage: &'static str,
        got: &'static str,
    },
    #[error("`{stage}` needs a number, got {got}")]
    StageNeedsNumber {
        stage: &'static str,
        got: &'static str,
    },
    #[error("`{name}` needs a number, got {got}")]
    FmtNeedsNumber {
        name: &'static str,
        got: &'static str,
        span: Span,
    },
    #[error("unknown status `{name}`")]
    UnknownStatus { name: String, span: Span },
}

impl EvalError {
    /// Where in the expression the failing field or call is, if the error comes from
    /// one.
    pub fn span(&self) -> Option<Span> {
        match self {
            EvalError::NoField { span, .. }
            | EvalError::FieldOnType { span, .. }
            | EvalError::UnknownCall { span, .. }
            | EvalError::Call { span, .. }
            | EvalError::Arity { span, .. }
            | EvalError::FmtNeedsNumber { span, .. }
            | EvalError::UnknownStatus { span, .. } => Some(*span),
            _ => None,
        }
    }

    fn code_name(&self) -> &'static str {
        match self {
            EvalError::UnknownParam(_) => "gogo_skin::eval::unknown_param",
            EvalError::UnknownToken(_) => "gogo_skin::eval::unknown_token",
            EvalError::AlphaNotNumber => "gogo_skin::eval::alpha_not_number",
            EvalError::NotAColour(_) => "gogo_skin::eval::not_a_colour",
            EvalError::NoField { .. } => "gogo_skin::eval::no_field",
            EvalError::FieldOnType { .. } => "gogo_skin::eval::field_on_type",
            EvalError::UnknownCall { .. } => "gogo_skin::eval::unknown_call",
            EvalError::Call { .. } => "gogo_skin::eval::call",
            EvalError::Arity { .. } => "gogo_skin::eval::arity",
            EvalError::NegNeedsNumber(_) => "gogo_skin::eval::neg_needs_number",
            EvalError::Compare { .. } => "gogo_skin::eval::compare",
            EvalError::NeedsList { .. } => "gogo_skin::eval::needs_list",
            EvalError::StageNeedsNumber { .. } => "gogo_skin::eval::stage_needs_number",
            EvalError::FmtNeedsNumber { .. } => "gogo_skin::eval::fmt_needs_number",
            EvalError::UnknownStatus { .. } => "gogo_skin::eval::unknown_status",
        }
    }
}

impl Diagnostic for EvalError {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        Some(Box::new(self.code_name()))
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        let span = self.span()?;
        Some(Box::new(std::iter::once(LabeledSpan::new_with_span(
            Some(self.to_string()),
            span,
        ))))
    }
}
