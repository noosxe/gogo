//! Errors of the cloud-saves code and their exit codes (design 22 §5.3): one
//! type and one exit-code mapping for every `gogo saves` command.
//!
//! SECURITY (design 07 §4): nothing here holds a token, a client secret, a user id
//! or an unredacted storage URL. gog-warp's errors carry the request URL (for the
//! exchange: client secret and refresh token in its query), so they are mapped by
//! kind and status only and never formatted (see [`super::auth`]).

use super::remote_config::RemoteConfigError;

/// What went wrong finding the game, its save locations, credentials, a token,
/// or talking to the storage host.
#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    /// No game dir for the gogid (exit 2: the argument is wrong).
    #[error("{0} isn't built: no game dir for it")]
    NotBuilt(String),

    /// The gogid can't name a game dir (exit 2).
    #[error("{0}")]
    Game(String),

    #[error(
        "no client id for {0}: its goggame-{0}.info and build-manifest.json have none (is it built?)"
    )]
    NoClientId(String),

    /// Fetching or reading the game's remote config (save locations).
    #[error(transparent)]
    RemoteConfig(#[from] RemoteConfigError),

    #[error("not logged in — run `gogo login`")]
    NotLoggedIn,

    /// The main session's Galaxy token can't be refreshed (`invalid_session` on
    /// its own refresh): only a new login helps.
    #[error("the GOG session is no longer valid — run `gogo login`")]
    InvalidSession,

    /// The session or the token store failed (design 07 §2: auth is exit 3).
    #[error("auth: {0}")]
    Auth(String),

    /// The per-game exchange answered 400 or 401. gog-warp reports 400 as
    /// `invalid_session`, but a stale client secret plausibly answers the same, so
    /// this doesn't claim which of the two it was.
    #[error(
        "GOG rejected the token exchange for product {gogid} (HTTP {status}): the \
         session or the game's client credentials are invalid; try `gogo login`, \
         then re-run"
    )]
    ExchangeRejected { gogid: u64, status: u16 },

    /// `no_license` (a 403 from the exchange): the account doesn't own the product
    /// the credentials are for.
    #[error(
        "this account has no license for product {gogid}: cloud saves use the built \
         game's own id; a pack or another edition needs its own build"
    )]
    NoLicense { gogid: u64 },

    /// The exchange answered with another HTTP status.
    #[error("token exchange for product {gogid} failed: HTTP {status}")]
    ExchangeStatus { gogid: u64, status: u16 },

    /// The exchange failed in some other way (gog-warp's error kind only).
    #[error("token exchange for product {gogid} failed: {kind}")]
    Exchange { gogid: u64, kind: String },

    /// The builds listing or a build manifest couldn't be fetched (the content
    /// system, not the exchange: a 403 here is no license claim).
    #[error(
        "could not fetch the builds or build manifest of product {gogid}{}",
        status.map(|s| format!(": HTTP {s}")).unwrap_or_default()
    )]
    Manifest { gogid: u64, status: Option<u16> },

    /// Neither the kept manifest nor a fetched one gave both client fields.
    #[error("no cloud-save client credentials for product {gogid}: {reason}")]
    NoCredentials { gogid: u64, reason: String },

    /// The fetched manifest's `clientId` isn't the one in `goggame-<gogid>.info`.
    #[error(
        "the build manifest of product {gogid} names another client than its \
         goggame-{gogid}.info: not guessing which one the saves belong to"
    )]
    ClientIdMismatch { gogid: u64 },

    /// The game's `goggame-<gogid>.info` exists but can't be used. Refused rather
    /// than skipped: it is the check a fetched manifest is held to.
    #[error("{path}: {reason}; fix or remove the file, then re-run")]
    Info { path: String, reason: String },

    /// An object name with an empty, `.` or `..` segment.
    #[error("cloud object name {name:?} has an empty, `.` or `..` segment")]
    ObjectName { name: String },

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

    /// The listing isn't the JSON array design 22 §1.3 describes.
    #[error("cloud storage: the listing isn't the JSON gogo expects: {0}")]
    Listing(String),

    /// The container says it holds more objects than the listing returned.
    /// Planning on it would take the missing ones for deleted (design 22 as built).
    #[error(
        "cloud storage: the listing has {listed} of the container's {count} objects; \
         not planning on a partial listing"
    )]
    ListingTruncated { listed: usize, count: u64 },

    /// A downloaded object whose MD5 isn't the listed `hash`, or whose gzip
    /// doesn't decode (exit 5).
    #[error("cloud object {name:?}: {reason}")]
    Corrupt { name: String, reason: String },

    /// A local file or gogo's own state (the sync record) can't be read or
    /// written.
    #[error("{path}: {reason}")]
    Io { path: String, reason: String },

    /// A write to a save dir or the backup dir failed (space, permissions,
    /// I/O): exit 6.
    #[error("{path}: {reason}")]
    Disk { path: String, reason: String },

    /// The pull can't run now and changed nothing: the game runs, another
    /// gogo holds its locks, or the snapshot before it couldn't be taken
    /// (exit 7, design 22 §7.2, §7.4).
    #[error("{0}")]
    Refused(String),
    /// An upload plan that would take the container over its quota (design 22
    /// §5.4): refused before any upload. Exit 1: a retry doesn't help.
    #[error("{0}")]
    Quota(String),
}

/// The exit class of an HTTP status (design 07 §2): 3 auth, 4 worth a retry
/// later, 1 anything else. 507 (quota) is 1: a retry doesn't help.
fn status_exit(status: u16) -> i32 {
    match status {
        401 | 403 => 3,
        507 => 1,
        429 | 500..=599 => 4,
        _ => 1,
    }
}

impl CloudError {
    /// The exit class (design 07 §2, design 22 §5.3): 2 a gogid that names no
    /// built game, 3 auth and `no_license`, 4 network (remote config too),
    /// 5 a corrupt object, 6 a failed write, 7 a refused pull, 1 anything else.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::NotBuilt(_) | Self::Game(_) => 2,
            Self::RemoteConfig(RemoteConfigError::Http(_) | RemoteConfigError::Status { .. }) => 4,
            Self::RemoteConfig(_) | Self::NoClientId(_) => 1,
            Self::Corrupt { .. } => 5,
            Self::Disk { .. } => 6,
            Self::Refused(_) => 7,
            Self::Io { .. } | Self::Listing(_) | Self::ListingTruncated { .. } => 1,
            Self::NotLoggedIn
            | Self::InvalidSession
            | Self::Auth(_)
            | Self::ExchangeRejected { .. }
            | Self::NoLicense { .. } => 3,
            Self::Network(_) => 4,
            Self::ExchangeStatus { status, .. } | Self::Storage { status, .. } => {
                status_exit(*status)
            }
            // A 403 from the content system isn't an auth failure of ours.
            Self::Manifest { status, .. } => match status {
                Some(401) => 3,
                Some(s) if *s != 403 => status_exit(*s),
                _ => 1,
            },
            Self::NoCredentials { .. }
            | Self::ClientIdMismatch { .. }
            | Self::Exchange { .. }
            | Self::Info { .. }
            | Self::ObjectName { .. }
            | Self::Quota(_) => 1,
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
        let rejected = CloudError::ExchangeRejected {
            gogid: 1,
            status: 400,
        };
        assert_eq!(rejected.exit_code(), 3);
        assert!(
            rejected
                .to_string()
                .contains("try `gogo login`, then re-run")
        );
        assert_eq!(CloudError::Network("x".into()).exit_code(), 4);
        let storage = |status| CloudError::Storage {
            status,
            url: String::new(),
            code: None,
        };
        assert_eq!(storage(403).exit_code(), 3);
        assert_eq!(storage(503).exit_code(), 4);
        assert_eq!(storage(507).exit_code(), 1);
        assert_eq!(storage(400).exit_code(), 1);
        assert_eq!(
            CloudError::ExchangeStatus {
                gogid: 1,
                status: 502
            }
            .exit_code(),
            4
        );
        let manifest = |status| CloudError::Manifest { gogid: 1, status };
        assert_eq!(manifest(Some(403)).exit_code(), 1);
        assert_eq!(manifest(Some(404)).exit_code(), 1);
        assert_eq!(manifest(Some(503)).exit_code(), 4);
        assert_eq!(manifest(None).exit_code(), 1);
        assert_eq!(CloudError::ClientIdMismatch { gogid: 1 }.exit_code(), 1);
        assert_eq!(CloudError::NotBuilt("1".into()).exit_code(), 2);
        assert_eq!(CloudError::NoClientId("1".into()).exit_code(), 1);
        let rc = |e| CloudError::RemoteConfig(e).exit_code();
        assert_eq!(rc(RemoteConfigError::Http("x".into())), 4);
        assert_eq!(
            rc(RemoteConfigError::Status {
                client_id: "1".into(),
                status: 500
            }),
            4
        );
        assert_eq!(rc(RemoteConfigError::Parse("x".into())), 1);
        let corrupt = CloudError::Corrupt {
            name: "saves/a".into(),
            reason: "x".into(),
        };
        assert_eq!(corrupt.exit_code(), 5);
        let truncated = CloudError::ListingTruncated {
            listed: 1,
            count: 2,
        };
        assert_eq!(truncated.exit_code(), 1);
        let disk = CloudError::Disk {
            path: "p".into(),
            reason: "full".into(),
        };
        assert_eq!(disk.exit_code(), 6);
        assert_eq!(CloudError::Refused("x".into()).exit_code(), 7);
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
