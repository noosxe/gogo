//! InstallRecord + version/build comparison — update detection (`docs/design/06` §4).
//!
//! The modern API has no `updated` flag (live-verified): detection compares catalog
//! `version` + checksum-XML `timestamp` + filename build id against the local record.
//! `version`/`timestamp` compare is the cheap path; `--strict` adds build id.

use serde::{Deserialize, Serialize};

/// One downloaded `files[]` entry (`docs/design/06` §3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallRecord {
    pub product_id: u64,
    pub product_title: Option<String>,
    pub file_id: String,
    /// e.g. `en1installer0`.
    pub entry_id: String,
    pub kind: String,
    pub version: Option<String>,
    /// Build id parsed from the saved filename.
    pub build_id: Option<String>,
    /// Checksum-XML timestamp.
    pub timestamp: Option<u64>,
    pub size: u64,
    /// Path relative to the download root, so the state file survives moves.
    pub path: std::path::PathBuf,
    /// RFC 3339.
    pub installed_at: String,
}

/// Row for the `list` table (`docs/design/03` §1:
/// `ID  TITLE  OS  INSTALLERS  SIZE  INSTALLED(✓ version | –)`).
#[derive(Debug, Clone)]
pub struct ListRow {
    pub id: u64,
    pub title: String,
    pub os: Option<crate::gog::catalog::Os>,
    pub installers: u32,
    pub total_size: u64,
    pub installed_version: Option<String>,
}

/// Cheap update check: catalog `version` (or timestamp) differs from the record.
/// `--strict` additionally compares the XML build id + timestamp (docs/design/06 §4).
pub fn is_outdated(_record: &InstallRecord, _catalog_version: Option<&str>, _strict: bool) -> bool {
    todo!("docs/design/06 §4")
}
