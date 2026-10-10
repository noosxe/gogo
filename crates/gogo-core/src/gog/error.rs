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

impl ApiError {
    /// reqwest's Display hides the serde detail ("error decoding response body");
    /// walk the source chain so payload mismatches are debuggable.
    pub fn decode(e: reqwest::Error) -> ApiError {
        let mut msg = e.to_string();
        let mut src = std::error::Error::source(&e);
        while let Some(s) = src {
            msg.push_str(": ");
            msg.push_str(&s.to_string());
            src = s.source();
        }
        ApiError::Other(msg)
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

    #[error("network error during auth: {0}")]
    Http(#[from] reqwest::Error),

    #[error("gog-warp: {0}")]
    Upstream(String),
}

impl From<gog_warp::Error> for AuthError {
    fn from(err: gog_warp::Error) -> Self {
        match err.kind() {
            gog_warp::errors::ErrorKind::NotLoggedIn => Self::Unauthorized,
            _ => Self::Upstream(err.to_string()),
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
