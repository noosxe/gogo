//! GOG API clients; all serde types live here (`docs/design/01` §1).
//!
//! Endpoints (live-verified 2026-10-08, docs/11):
//! - embed API: `https://embed.gog.com` (Bearer) — games library (`getFilteredProducts`)
//! - catalog:   `https://api.gog.com/products?ids=…` (Bearer) — batch product JSON
//! - checksum:  public CDN XML — no auth (`docs/design/04` §2)

pub mod catalog;
pub mod checksum;
pub mod depot;
pub mod downlink;
pub mod error;
pub mod library;

pub const EMBED_BASE: &str = "https://embed.gog.com";
pub const API_BASE: &str = "https://api.gog.com";

/// Shared client factory: UA, timeouts, rustls, no gzip (`docs/design/01` §1;
/// retry/timeout policy in `docs/design/07` §3).
pub fn http() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("gogo/", env!("CARGO_PKG_VERSION")))
        .build()
}
