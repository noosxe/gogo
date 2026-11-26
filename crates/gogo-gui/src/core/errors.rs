//! One door for failures: every [`ErrorView`] the engine raises is made here (design 25
//! §6). The core's errors are redacted when built (design 07 §4); this scrubs them again,
//! hides the home folder and drops the CLI's own advice.

use std::fmt::Display;

use gogo::gog::error::{ApiError, AuthError};
use gogo_gui_model::ErrorView;

/// What a failure is, for the class word and exit code (design 07 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Class {
    Auth,
    Network,
    Other,
}

pub(crate) trait Classed: Display {
    fn class(&self) -> Class;
}

impl Classed for AuthError {
    fn class(&self) -> Class {
        match self {
            AuthError::Http(_) => Class::Network,
            _ => Class::Auth,
        }
    }
}

impl Classed for ApiError {
    fn class(&self) -> Class {
        match self {
            ApiError::Unauthorized => Class::Auth,
            ApiError::Http(_) | ApiError::RateLimited { .. } | ApiError::Status { .. } => {
                Class::Network
            }
            _ => Class::Other,
        }
    }
}

impl Classed for gogo::gog::depot::DepotError {
    fn class(&self) -> Class {
        Class::Other
    }
}

/// `text` safe to show.
fn clean(text: &str) -> String {
    let mut out = gogo::redact::scrub_text(text);
    if let Some(home) = std::env::var_os("HOME").and_then(|h| h.into_string().ok())
        && home.len() > 1
    {
        out = out.replace(home.trim_end_matches('/'), "~");
    }
    out.replace(" — run `gogo login`", "")
        .replace("run `gogo login`", "sign in")
}

/// The error screen for `error`, under the app's own `title`.
pub(crate) fn view<E: Classed>(title: &str, error: &E) -> ErrorView {
    let (class, exit, hint) = match error.class() {
        Class::Auth => ("Auth", "3", "Sign in again."),
        Class::Network => ("Network", "4", "Check the connection and try again."),
        Class::Other => ("Error", "1", ""),
    };
    ErrorView::new(class, title, clean(&error.to_string()), hint, exit)
}

/// For an action the app does not carry out yet.
pub(crate) fn unavailable() -> ErrorView {
    ErrorView::usage(
        "Not available yet",
        "This version of the app can't do that.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_lose_secrets_and_cli_advice() {
        let e = ApiError::Other(
            "GET https://api.gog.com/products/1?access_token=FAKE-ACCESS-91c with code \
             FAKE-Code_0123456789abcdefXYZ"
                .into(),
        );
        let v = view("The library could not be read", &e);
        assert!(!format!("{v:?}").contains("FAKE-"), "{v:?}");
        let v = view("Not signed in", &AuthError::Unauthorized);
        assert_eq!(v.message, "unauthorized");
        assert_eq!((v.class.as_str(), v.exit_class.as_str()), ("Auth", "3"));
    }
}
