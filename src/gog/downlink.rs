//! `files[].downlink` → `{ downlink, checksum }` (`docs/design/04` §3).
//!
//! Real path (live-verified, docs/11): `GET api/products/{pid}/downlink/installer/{file_id}`
//! with Bearer → **200 JSON** (no redirect) containing the pre-tokenized CDN URL and the
//! checksum XML URL. CDN URLs are pre-tokenized in the path, never persisted, re-resolved
//! per file per run.

use serde::Deserialize;

use crate::gog::error::ApiError;

#[derive(Debug, Clone, Deserialize)]
pub struct Downlink {
    /// Pre-tokenized CDN URL (`gog-cdn-fastly.gog.com/token=…~dirs=…/secure/offline/…`).
    /// SECURITY: never log, never persist (`docs/design/02` §5, `07` §4).
    pub downlink: String,
    /// Public checksum XML URL (no auth needed).
    pub checksum: String,
}

/// Resolve one `files[]` entry to its CDN URL + checksum doc URL.
pub async fn resolve(
    _http: &reqwest::Client,
    _bearer: &str,
    _product_id: u64,
    _file_id: &str,
) -> Result<Downlink, ApiError> {
    todo!("GET /products/{{pid}}/downlink/installer/{{file_id}} (Bearer) → 200 JSON — docs/04 §3")
}
