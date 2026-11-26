//! Questions the app asks in its own widgets: before an action (kind `prompt`) and while
//! a job runs (design 25 §4).

use std::fmt;

use zeroize::Zeroizing;

/// A question.
#[derive(Debug, Clone, PartialEq)]
pub enum PromptSpec {
    /// One line of text.
    Text {
        /// The question.
        label: String,
        /// What the field starts with.
        default: String,
    },
    /// Typed into `secret-input`; the value never enters a source (S1, S2).
    Secret {
        /// The question.
        label: String,
    },
    /// One of a list.
    Choice {
        /// The question.
        label: String,
        /// What to pick from.
        options: Vec<String>,
    },
    /// Yes or no. The widget's default is no.
    Confirm {
        /// The question.
        label: String,
    },
    /// One decision per cloud-save conflict; nothing applies until all are decided.
    Conflicts {
        /// The conflicting files.
        files: Vec<ConflictFile>,
    },
}

/// A file that changed on both sides.
#[derive(Debug, Clone, PartialEq)]
pub struct ConflictFile {
    /// The file.
    pub name: String,
    /// What the local copy is.
    pub local: String,
    /// What the cloud copy is.
    pub cloud: String,
}

/// Which copy of a conflicting file to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The copy on this machine.
    Local,
    /// The cloud's copy.
    Cloud,
}

/// A typed secret. It never prints, and its memory is wiped when it is dropped.
#[derive(Clone, PartialEq)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    /// Wrap a typed secret.
    pub fn new(value: impl Into<String>) -> Self {
        Secret(Zeroizing::new(value.into()))
    }

    /// The clear value, for the backend to hand to the core.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// An answer.
#[derive(Debug, Clone, PartialEq)]
pub enum PromptAnswer {
    /// What was typed.
    Text(String),
    /// A typed secret.
    Secret(Secret),
    /// An index into the options.
    Choice(usize),
    /// Yes.
    Yes,
    /// No.
    No,
    /// One side per conflicting file, in order.
    Sides(Vec<Side>),
}

impl PromptSpec {
    /// Whether `answer` is a complete answer to this question.
    pub fn accepts(&self, answer: &PromptAnswer) -> bool {
        use PromptAnswer as A;
        match (self, answer) {
            (Self::Text { .. }, A::Text(_)) => true,
            (Self::Secret { .. }, A::Secret(_)) => true,
            (Self::Choice { options, .. }, A::Choice(i)) => *i < options.len(),
            (Self::Confirm { .. }, A::Yes | A::No) => true,
            (Self::Conflicts { files }, A::Sides(sides)) => sides.len() == files.len(),
            _ => false,
        }
    }
}

type Answer = Box<dyn FnOnce(Option<PromptAnswer>) + Send>;

/// How a pipeline gets its answer. `None` means cancelled, and the pipeline stops without
/// changes (S8). Dropping an unanswered reply cancels, so a question can't be lost.
pub struct PromptReply(Option<Answer>);

impl PromptReply {
    /// `answer` is called exactly once.
    pub fn new(answer: impl FnOnce(Option<PromptAnswer>) + Send + 'static) -> Self {
        PromptReply(Some(Box::new(answer)))
    }

    pub(crate) fn answer(mut self, answer: PromptAnswer) {
        if let Some(f) = self.0.take() {
            f(Some(answer));
        }
    }
}

impl Drop for PromptReply {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f(None);
        }
    }
}

impl fmt::Debug for PromptReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PromptReply")
    }
}
