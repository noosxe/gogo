//! API error taxonomies (`docs/design/07` §2).
//!
//! SECURITY (docs/design/07 §4): `Display` impls here must never render CDN URLs —
//! they embed access tokens in the path (`/token=…~dirs=…/`). Redaction happens at
//! construct time; nothing token-bearing is ever formatted into an error.

/// Errors from GOG API calls (catalog, library, downlink, checksum).
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("unauthorized — run `gogo login`")]
    Unauthorized,

    /// Extras may 403 when bundled-only; callers skip with a `Warn` event.
    #[error("forbidden (likely bundled extras — skippable): {0}")]
    Forbidden(String),

    #[error("rate limited (HTTP {status}) — backoff and retry")]
    RateLimited { status: u16 },

    #[error("HTTP {status}: {message}")]
    Status { status: u16, message: String },

    #[error("request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("unexpected API payload: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}

/// Auth-layer errors (`docs/design/02`, exit code 3).
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("login aborted")]
    LoginAborted,

    #[error("unauthorized — run `gogo login`")]
    Unauthorized,

    #[error("token store error: {0}")]
    Store(String),

    #[error("stored session unreadable: {0}")]
    Deserialize(String),

    #[error("network error during auth: {0}")]
    Http(#[from] reqwest::Error),
}

impl From<AuthError> for ApiError {
    fn from(err: AuthError) -> Self {
        match err {
            AuthError::Unauthorized => Self::Unauthorized,
            other => Self::Other(other.to_string()),
        }
    }
}
