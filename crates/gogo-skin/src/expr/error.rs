//! Errors of the expression language. Parse errors carry a byte span.

use miette::{Diagnostic, LabeledSpan, SourceSpan};

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
}

impl From<Span> for SourceSpan {
    fn from(s: Span) -> Self {
        SourceSpan::new(s.offset.into(), s.len)
    }
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
    #[error("bad number `{0}`")]
    BadNumber(String),
    #[error("`{0}` must be followed by a name")]
    MissingName(char),
    #[error("expected {0}")]
    Expected(&'static str),
    #[error("unexpected {0}")]
    Unexpected(String),
    #[error("unknown pipe stage `{0}`")]
    UnknownStage(String),
    #[error("unexpected {0} after the expression")]
    TrailingInput(String),
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
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct CallError {
    pub message: String,
}

impl CallError {
    pub fn new(message: impl Into<String>) -> Self {
        CallError {
            message: message.into(),
        }
    }
}

/// Why an expression could not be evaluated.
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
    NoField { name: String, fields: String },
    #[error("`.{name}` on a {ty} value")]
    FieldOnType { name: String, ty: &'static str },
    #[error("unknown source or helper `{0}`")]
    UnknownCall(String),
    #[error("`{name}` failed: {source}")]
    Call {
        name: String,
        #[source]
        source: CallError,
    },
    #[error("`{name}` takes {expected} argument(s), got {got}")]
    Arity {
        name: String,
        expected: usize,
        got: usize,
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
    },
    #[error("unknown status `{0}`")]
    UnknownStatus(String),
}
