//! The one shape every failure takes (design 25 §6, design 26 §7 S9).
//!
//! [`ErrorView`] is a record for the app to display, not a Rust error type: nothing in
//! this crate matches on it or propagates it as a cause, so it doesn't implement
//! `std::error::Error`. Where a function returns `Result<_, ErrorView>`, the `Err` is
//! what the user is shown.

use std::collections::BTreeMap;

use gogo_contract::Value;

/// A failure as the app's `error` widget shows it. The backend builds it from the core's
/// errors, which are already redacted; the model adds its own for the calls it refuses.
///
/// None of the model's own messages quote an argument's value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorView {
    /// The class word the CLI uses for its exit classes (`Network`, `Disk`, …).
    pub class: String,
    /// One line saying what failed.
    pub title: String,
    /// The detail, already redacted.
    pub message: String,
    /// What the user can do about it; empty if there is nothing to suggest.
    pub hint: String,
    /// The exit code the CLI would end with (design 07 §2).
    pub exit_class: String,
}

impl ErrorView {
    /// An error with every field given.
    pub fn new(
        class: impl Into<String>,
        title: impl Into<String>,
        message: impl Into<String>,
        hint: impl Into<String>,
        exit_class: impl Into<String>,
    ) -> Self {
        ErrorView {
            class: class.into(),
            title: title.into(),
            message: message.into(),
            hint: hint.into(),
            exit_class: exit_class.into(),
        }
    }

    /// A call the app refuses as it was made.
    pub fn usage(title: impl Into<String>, message: impl Into<String>) -> Self {
        ErrorView::new("Usage", title, message, "", "2")
    }

    /// A failure with no more specific class.
    pub fn failed(title: impl Into<String>, message: impl Into<String>) -> Self {
        ErrorView::new("Error", title, message, "", "1")
    }

    /// The record a skin reads (the contract's `ErrorView` shape).
    pub fn to_value(&self) -> Value {
        let text = |s: &str| Value::str(s);
        Value::Map(BTreeMap::from([
            ("class".to_owned(), text(&self.class)),
            ("title".to_owned(), text(&self.title)),
            ("message".to_owned(), text(&self.message)),
            ("hint".to_owned(), text(&self.hint)),
            ("exit_class".to_owned(), text(&self.exit_class)),
        ]))
    }
}
