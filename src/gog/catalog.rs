//! Products batch `?ids=…` (50/call); Product/Downloads types
//! (`docs/design/03`; shapes live-verified in `docs/04`/`docs/11`).

use serde::Deserialize;

use crate::gog::error::ApiError;

/// One product from the catalog batch. Unknown/extra fields are tolerated.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Product {
    pub id: u64,
    pub title: Option<String>,
    pub slug: Option<String>,
    pub downloads: Option<Downloads>,
}

/// `downloads` — live-verified top-level shape.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Downloads {
    #[serde(default)]
    pub installers: Vec<InstallerEntry>,
    #[serde(default)]
    pub patches: Vec<InstallerEntry>,
    #[serde(default)]
    pub bonus_content: Vec<InstallerEntry>,
    #[serde(default)]
    pub language_packs: Vec<InstallerEntry>,
}

/// One `downloads.*[]` entry: `en1installer0`-style id + metadata + parts.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct InstallerEntry {
    pub id: String,
    pub name: Option<String>,
    pub os: Option<Os>,
    pub language: Option<String>,
    pub language_full: Option<String>,
    pub version: Option<String>,
    pub total_size: Option<u64>,
    /// Multi-part files: `en1installer0`, `en1installer1`, … (live-verified).
    #[serde(default)]
    pub files: Vec<FileEntry>,
}

/// One downloadable part (`files[]`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FileEntry {
    pub id: String,
    pub size: u64,
    /// API path — resolve through [`crate::gog::downlink`] to get the CDN URL
    /// (the modern API has no direct `galaxy_downlink_json_url`).
    pub downlink: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Windows,
    Mac,
    Linux,
}

/// Fetch catalog entries in batches of 50 (`docs/design/03` §1).
pub async fn batch(
    _http: &reqwest::Client,
    _bearer: &str,
    _product_ids: &[u64],
) -> Result<Vec<Product>, ApiError> {
    todo!("GET api /products?ids=… (chunks of 50) — docs/03 §3")
}
