//! Checksum XML → `ChecksumDoc { name, total_size, chunks[] }` (`docs/design/04` §2).
//!
//! The XML is **authoritative** (live-verified): `total_size` here can exceed the API's
//! `total_size` (724,243,232 vs 723,517,440 in our sample); the save filename comes from
//! the root element's `name` attribute. 10 MiB chunks, last one partial.

use serde::Deserialize;

use crate::gog::error::ApiError;

/// Live-verified chunk size.
pub const CHUNK_SIZE: u64 = 10 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
pub struct ChecksumDoc {
    /// Save filename, e.g. `setup_<slug>_<version>_(<buildid>).exe`.
    pub name: String,
    pub total_size: u64,
    /// Unix timestamp — used by `--strict` update detection (docs/design/06 §4).
    #[serde(default)]
    pub timestamp: Option<u64>,
    #[serde(default)]
    pub chunks: Vec<Chunk>,
}

/// One `<chunk id from to method="md5">hex</chunk>`.
#[derive(Debug, Clone, Deserialize)]
pub struct Chunk {
    pub id: u32,
    pub from: u64,
    /// Inclusive end offset.
    pub to: u64,
    #[serde(default = "default_method")]
    pub method: String,
    pub md5: String,
}

fn default_method() -> String {
    "md5".to_string()
}

/// Fetch + parse the checksum XML (public, no auth) — quick-xml, buffered, tiny docs.
pub async fn fetch(_http: &reqwest::Client, _checksum_url: &str) -> Result<ChecksumDoc, ApiError> {
    todo!("GET checksum XML → quick-xml parse — docs/04 §2 (sample: gogo-probe/checksum.xml)")
}
