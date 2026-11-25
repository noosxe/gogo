//! Why a route failed to bind (design 27 §7 "Failures", §9 step 4; design 26 §8).
//!
//! A [`BindError`] is a [`miette::Diagnostic`] like a loader finding: it names the file,
//! line and column of the skin text that failed, and carries a stable code
//! (`gogo_skin::bind::<kind>`). A route that fails to bind falls back to the default
//! skin's route, and the error is reported with the rest of the skin's findings.

use std::fmt;
use std::sync::Arc;

use miette::{Diagnostic, LabeledSpan, NamedSource, Severity};

use crate::diag::Finding;
use crate::expr::EvalError;
use crate::manifest::Mode;
use crate::source::{Sources, Span};

/// What went wrong.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum BindErrorKind {
    /// An expression failed to evaluate: a field of a null, a call the app can't answer,
    /// a type mismatch the load-time checks don't cover (design 27 §7, "Failures").
    #[error("{0}")]
    Eval(#[from] EvalError),
    /// A property value that is only known now is not a valid one.
    #[error("`{name}`: {why}")]
    Property { name: String, why: String },
    /// A name that does not resolve now: a token, a component, a route, a fragment.
    #[error("{0}")]
    Missing(String),
    /// The action or its arguments are not valid for this data.
    #[error("{0}")]
    Action(String),
    /// A control without an accessible name (S14).
    #[error("{0} has no accessible name (S14)")]
    NoName(&'static str),
    /// A part that can't be hidden, hidden (S5).
    #[error("the `{part}` part of `{widget}` can't be hidden (S5)")]
    HiddenPart {
        widget: &'static str,
        part: &'static str,
    },
    /// A masked value where it must not go (S1, S2).
    #[error("{0}")]
    Masked(String),
    /// `each` or `rows` over something that is not a list.
    #[error("`{what}` takes a list, found {got}")]
    NotAList {
        what: &'static str,
        got: &'static str,
    },
    /// `progress` outside 0 to 1 (design 27 §5.6).
    #[error("`progress` must be between 0 and 1, found {0}")]
    Progress(f64),
    /// The skin has no tokens for the mode asked for.
    #[error("the skin has no `{}` tokens", .0.name())]
    NoMode(Mode),
    /// The route binds to more nodes than the limit (design 27 §8).
    #[error("the route binds to more than {0} nodes")]
    TooManyNodes(usize),
    /// Components, `each` or the outlet nest deeper than the limit.
    #[error("the route nests more than {0} levels deep when bound")]
    TooDeep(usize),
    /// The tree refused a node: a duplicate key, a value out of range.
    #[error("the tree refused a node: {0}")]
    Tree(String),
    /// A borrowed fragment of the default skin does not hold against this skin's manifest
    /// (design 27 §5.7): the loader's findings for it.
    #[error("{}", .0.first().map_or_else(|| "a borrowed fragment is invalid".to_owned(), |f| f.message.clone()))]
    Borrow(Vec<Finding>),
}

impl BindErrorKind {
    /// The code of this kind of failure: `gogo_skin::bind::<kind>`.
    pub const fn code(&self) -> &'static str {
        match self {
            BindErrorKind::Eval(_) => "gogo_skin::bind::eval",
            BindErrorKind::Property { .. } => "gogo_skin::bind::property",
            BindErrorKind::Missing(_) => "gogo_skin::bind::missing",
            BindErrorKind::Action(_) => "gogo_skin::bind::action",
            BindErrorKind::NoName(_) => "gogo_skin::bind::no_name",
            BindErrorKind::HiddenPart { .. } => "gogo_skin::bind::hidden_part",
            BindErrorKind::Masked(_) => "gogo_skin::bind::masked",
            BindErrorKind::NotAList { .. } => "gogo_skin::bind::not_a_list",
            BindErrorKind::Progress(_) => "gogo_skin::bind::progress",
            BindErrorKind::NoMode(_) => "gogo_skin::bind::no_mode",
            BindErrorKind::TooManyNodes(_) => "gogo_skin::bind::too_many_nodes",
            BindErrorKind::TooDeep(_) => "gogo_skin::bind::too_deep",
            BindErrorKind::Tree(_) => "gogo_skin::bind::tree",
            BindErrorKind::Borrow(_) => "gogo_skin::bind::borrow",
        }
    }
}

/// A route that failed to bind: what, and where in the skin's text.
#[derive(Debug, Clone)]
pub struct BindError(Box<Inner>);

#[derive(Debug, Clone)]
struct Inner {
    kind: BindErrorKind,
    route: String,
    location: String,
    span: Option<Span>,
    source: Option<NamedSource<Arc<str>>>,
}

impl BindError {
    pub(crate) fn new(kind: BindErrorKind, route: &str, at: Option<(Span, &Sources)>) -> Self {
        let mut e = Inner {
            kind,
            route: route.to_owned(),
            location: String::new(),
            span: None,
            source: None,
        };
        if let Some((span, sources)) = at
            && let Some(file) = sources.get(span.file)
        {
            e.location = sources.location(span);
            e.span = Some(span);
            e.source = Some(NamedSource::new(file.path.clone(), file.text.clone()));
        }
        BindError(Box::new(e))
    }

    /// What went wrong.
    pub fn kind(&self) -> &BindErrorKind {
        &self.0.kind
    }

    /// The route that was being bound.
    pub fn route(&self) -> &str {
        &self.0.route
    }

    /// `routes/game.kdl:12:5`; empty when the failure has no place in the text.
    pub fn location(&self) -> &str {
        &self.0.location
    }

    /// Where the failure is in the skin's text, if it is.
    pub fn span(&self) -> Option<Span> {
        self.0.span
    }
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "route `{}`", self.0.route)?;
        if !self.0.location.is_empty() {
            write!(f, " ({})", self.0.location)?;
        }
        write!(f, ": {}", self.0.kind)
    }
}

impl std::error::Error for BindError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0.kind)
    }
}

impl Diagnostic for BindError {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.0.kind.code()))
    }

    fn severity(&self) -> Option<Severity> {
        Some(Severity::Error)
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        let text = match &self.0.kind {
            BindErrorKind::Eval(_) => {
                "this failed on the data it was bound to; a route that fails to bind falls back to the default skin's (design 26 §8)"
            }
            BindErrorKind::NoName(_) => {
                "a control needs its own text, a non-empty `label=` or `title=`, or an enclosing `field` (design 27 §5.3)"
            }
            BindErrorKind::Borrow(_) => {
                "a borrowed fragment is checked against the borrowing skin's `ui` variables and sections (design 27 §5.7)"
            }
            _ => return None,
        };
        Some(Box::new(text))
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        self.0.source.as_ref().map(|s| s as &dyn miette::SourceCode)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        let span = self.0.span?;
        self.0.source.as_ref()?;
        Some(Box::new(std::iter::once(LabeledSpan::new_with_span(
            None,
            (span.offset, span.len.max(1)),
        ))))
    }
}
