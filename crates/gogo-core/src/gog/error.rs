//! API error taxonomies (`docs/design/07` §2).
//!
//! SECURITY (docs/design/07 §4): nothing here holds library error text or a URL
//! as-is. CDN URLs embed access tokens in the path (`/token=…~dirs=…/`), and the
//! token endpoint takes the refresh token and client secret in its query; reqwest's
//! text carries the URL, and gog-warp's `Debug`/`source()` hand on reqwest's error.
//! Network errors are stored as [`NetError`], gog-warp's as
//! [`crate::redact::describe_error`]'s text: redaction happens at construct time.

use crate::redact::{NetError, describe_error};

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
    Http(NetError),

    /// serde's category and position only: its message can quote a value.
    #[error("unexpected API payload: {0}")]
    Json(String),

    #[error("{0}")]
    Other(String),
}

impl ApiError {
    /// A body that didn't decode: the redacted chain down to serde's category and
    /// position (never its message, which can quote a value).
    pub fn decode(e: reqwest::Error) -> ApiError {
        ApiError::Other(describe_error(&e))
    }

    /// Shared response-status → taxonomy mapping (`docs/design/07` §3).
    /// 2xx passes through; 401/403/404/429 map to their variants (the retry layer keys
    /// off `Status`/`RateLimited`).
    pub async fn check(resp: reqwest::Response) -> Result<reqwest::Response, ApiError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let reason = status.canonical_reason().unwrap_or("unknown").to_string();
        Err(match status.as_u16() {
            401 => ApiError::Unauthorized,
            403 => ApiError::Forbidden(reason),
            404 => ApiError::NotFound(reason),
            429 => ApiError::RateLimited { status: 429 },
            _ => ApiError::Status {
                status: status.as_u16(),
                message: reason,
            },
        })
    }
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

    /// The Galaxy refresh answered 400: only a new login helps.
    #[error("the GOG session is no longer valid — run `gogo login`")]
    InvalidSession,

    #[error("network error during auth: {0}")]
    Http(NetError),

    /// gog-warp's error, as [`describe_error`] shows it.
    #[error("gog-warp: {0}")]
    Upstream(String),
}

impl From<reqwest::Error> for ApiError {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(e.into())
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(crate::redact::json_summary(&e))
    }
}

impl From<reqwest::Error> for AuthError {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(e.into())
    }
}

impl From<gog_warp::Error> for AuthError {
    fn from(err: gog_warp::Error) -> Self {
        use gog_warp::errors::ErrorKind;
        match err.kind() {
            ErrorKind::NotLoggedIn => Self::Unauthorized,
            ErrorKind::InvalidSession => Self::InvalidSession,
            _ => Self::Upstream(describe_error(&err)),
        }
    }
}

impl From<AuthError> for ApiError {
    fn from(err: AuthError) -> Self {
        match err {
            AuthError::Unauthorized => Self::Unauthorized,
            other => Self::Other(other.to_string()),
        }
    }
}
