//! Errors of the cloud-saves code and their exit codes (design 22 §5.3).
//!
//! SECURITY (design 07 §4): nothing here holds a token, a client secret, a user id
//! or an unredacted storage URL. The token exchange's own errors carry the
//! request URL (client secret and refresh token in its query), so they are mapped
//! by kind and status only and never formatted (see [`super::auth`]).

/// What went wrong getting credentials, a token, or talking to the storage host.
#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    #[error("not logged in — run `gogo login`")]
    NotLoggedIn,

    /// `invalid_session` from the exchange: the Galaxy refresh token is no longer
    /// accepted.
    #[error("the GOG session is no longer valid — run `gogo login`")]
    InvalidSession,

    /// The session or the token store failed (design 07 §2: auth is exit 3).
    #[error("auth: {0}")]
    Auth(String),

    /// The exchange refused the game's client credentials (HTTP 401).
    #[error("GOG refused the client credentials of product {gogid}")]
    Unauthorized { gogid: u64 },

    /// `no_license`: the account doesn't own the product the credentials are for.
    #[error(
        "this account has no license for product {gogid}: cloud saves use the built \
         game's own id; a pack or another edition needs its own build"
    )]
    NoLicense { gogid: u64 },

    /// Neither the kept manifest nor a fetched one gave both client fields.
    #[error("no cloud-save client credentials for product {gogid}: {reason}")]
    NoCredentials { gogid: u64, reason: String },

    /// The fetched manifest's `clientId` isn't the one in `goggame-<gogid>.info`.
    #[error(
        "the build manifest of product {gogid} names another client than its \
         goggame-{gogid}.info: not guessing which one the saves belong to"
    )]
    ClientIdMismatch { gogid: u64 },

    /// The exchange answered with another HTTP status.
    #[error("token exchange for product {gogid} failed: HTTP {status}")]
    ExchangeStatus { gogid: u64, status: u16 },

    /// The exchange failed in some other way (gog-warp's error kind only).
    #[error("token exchange for product {gogid} failed: {kind}")]
    Exchange { gogid: u64, kind: String },

    /// No answer: connect, timeout, reset, retries used up. Redacted text only.
    #[error("network: {0}")]
    Network(String),

    /// The storage host answered with an error status. `url` is redacted;
    /// `code` is the body's short `error` field (`invalid_version`), when it has one.
    #[error("cloud storage: HTTP {status} for {url}{}", code.as_deref().map(|c| format!(" ({c})")).unwrap_or_default())]
    Storage {
        status: u16,
        url: String,
        code: Option<String>,
    },

    /// Reading the game dir (`goggame-<gogid>.info`).
    #[error("{0}")]
    Io(String),
}

impl CloudError {
    /// The exit class (design 07 §2, design 22 §5.3): 3 auth and `no_license`,
    /// 4 network, 1 anything else.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::NotLoggedIn
            | Self::InvalidSession
            | Self::Auth(_)
            | Self::Unauthorized { .. }
            | Self::NoLicense { .. } => 3,
            Self::Network(_) => 4,
            Self::ExchangeStatus { status, .. } | Self::Storage { status, .. } => match status {
                401 | 403 => 3,
                429 | 500..=599 => 4,
                _ => 1,
            },
            Self::NoCredentials { .. }
            | Self::ClientIdMismatch { .. }
            | Self::Exchange { .. }
            | Self::Io(_) => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes() {
        assert_eq!(CloudError::NotLoggedIn.exit_code(), 3);
        assert_eq!(CloudError::InvalidSession.exit_code(), 3);
        assert_eq!(CloudError::NoLicense { gogid: 1 }.exit_code(), 3);
        assert_eq!(CloudError::Unauthorized { gogid: 1 }.exit_code(), 3);
        assert_eq!(CloudError::Network("x".into()).exit_code(), 4);
        let storage = |status| CloudError::Storage {
            status,
            url: String::new(),
            code: None,
        };
        assert_eq!(storage(403).exit_code(), 3);
        assert_eq!(storage(503).exit_code(), 4);
        assert_eq!(storage(400).exit_code(), 1);
        assert_eq!(
            CloudError::ExchangeStatus {
                gogid: 1,
                status: 502
            }
            .exit_code(),
            4
        );
        assert_eq!(CloudError::ClientIdMismatch { gogid: 1 }.exit_code(), 1);
    }

    #[test]
    fn storage_message_names_the_code() {
        let e = CloudError::Storage {
            status: 400,
            url: "https://cloudstorage.gog.com/v1/[redacted 2 segments]".into(),
            code: Some("invalid_version".into()),
        };
        assert_eq!(
            e.to_string(),
            "cloud storage: HTTP 400 for https://cloudstorage.gog.com/v1/[redacted 2 segments] (invalid_version)"
        );
    }
}
