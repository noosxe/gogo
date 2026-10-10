//! InstallRecord + version/build comparison — update detection (`docs/design/06` §4).
//!
//! The modern API has no `updated` flag (live-verified): detection compares catalog
//! `version` (cheap, plan-time) and — at resolve time — the checksum-XML `timestamp`
//! against the local record. Review C7: the checksum `timestamp` is the **primary**
//! change evidence; the build id parsed from the filename is best-effort and a parse
//! failure is never fatal.

use serde::{Deserialize, Serialize};

/// One downloaded `files[]` entry (`docs/design/06` §3). Record identity:
/// `(product_id, dlc_id, file_id)` — the same keying as the verify-cache (A5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallRecord {
    pub product_id: u64,
    /// `None` = base game; DLC downloads key on the DLC's own id.
    #[serde(default)]
    pub dlc_id: Option<u64>,
    pub product_title: Option<String>,
    pub file_id: String,
    /// e.g. `en1installer0`.
    pub entry_id: String,
    pub kind: String,
    pub version: Option<String>,
    /// Build id parsed from the saved filename — best-effort (C7).
    pub build_id: Option<String>,
    /// Checksum-XML timestamp — primary update evidence (C7).
    #[serde(default)]
    pub checksum_ts: Option<u64>,
    pub size: u64,
    /// Path relative to the download root, so the state file survives moves.
    pub path: std::path::PathBuf,
    /// RFC 3339.
    pub installed_at: String,
}

impl InstallRecord {
    /// The verify-cache/state identity key component: `{dlc_id|base}` (A5).
    pub fn dlc_key(dlc_id: Option<u64>) -> String {
        dlc_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "base".into())
    }
}

/// Parse the build id out of a canonical save filename
/// (`setup_<slug>_<version>_(<buildid>).exe`). Best-effort by design (C7): any
/// surprise returns `None` and the caller falls back to timestamp/version evidence.
pub fn parse_build(filename: &str) -> Option<String> {
    let stem = filename.strip_suffix(".exe").unwrap_or(filename);
    let close = stem.rfind(')')?;
    if close != stem.len() - 1 {
        return None;
    }
    let open = stem[..close].rfind('(')?;
    let inner = &stem[open + 1..close];
    if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
        Some(inner.to_string())
    } else {
        None
    }
}

/// Cheap plan-time update check (`docs/design/06` §4): catalog `version` mismatch.
/// `strict` additionally compares build ids when both sides parse (best-effort —
/// unparseable sides are skipped, never fatal). The checksum `timestamp` comparison
/// is the execute-stage authority (04 §3, review B4).
pub fn is_outdated(
    record: &InstallRecord,
    catalog_version: Option<&str>,
    catalog_build: Option<&str>,
    strict: bool,
) -> bool {
    if record.version.as_deref() != catalog_version {
        return true;
    }
    if strict
        && let (Some(record_build), Some(catalog_build)) = (&record.build_id, catalog_build)
        && record_build != catalog_build
    {
        return true;
    }
    false
}

/// Row for the `list` table (`docs/design/03` §1:
/// `ID  TITLE  OS  INSTALLERS  SIZE  DOWNLOADED(✓ version | –)`).
#[derive(Debug, Clone)]
pub struct ListRow {
    pub id: u64,
    pub title: String,
    pub os: Option<crate::gog::catalog::Os>,
    pub installers: u32,
    pub total_size: u64,
    pub installed_version: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> InstallRecord {
        InstallRecord {
            product_id: 7,
            dlc_id: None,
            product_title: Some("Game".into()),
            file_id: "en1installer0".into(),
            entry_id: "en1installer0".into(),
            kind: "installer".into(),
            version: Some("2.0.0.1".into()),
            build_id: Some("12345".into()),
            checksum_ts: Some(1_760_000_000),
            size: 100,
            path: "Game/setup.exe".into(),
            installed_at: "2026-10-09T00:00:00Z".into(),
        }
    }

    #[test]
    fn build_parse_canonical() {
        assert_eq!(
            parse_build("setup_gog_game_2.0.0.1_(12345).exe"),
            Some("12345".into())
        );
        assert_eq!(parse_build("manual.pdf"), None);
        assert_eq!(parse_build("weird_(notdigits).exe"), None);
        assert_eq!(parse_build("trailing_(123)tail.exe"), None);
    }

    #[test]
    fn version_mismatch_is_outdated() {
        let r = record();
        assert!(!is_outdated(&r, Some("2.0.0.1"), None, false));
        assert!(is_outdated(&r, Some("2.0.0.2"), None, false));
        // Never recorded a version → any catalog version means "check it".
        let mut fresh = r.clone();
        fresh.version = None;
        assert!(is_outdated(&fresh, Some("2.0.0.1"), None, false));
    }

    #[test]
    fn strict_compares_builds_when_both_parse() {
        let r = record();
        assert!(is_outdated(&r, Some("2.0.0.1"), Some("99999"), true));
        assert!(!is_outdated(&r, Some("2.0.0.1"), Some("12345"), true));
        // Unparseable catalog side → skipped, not fatal (C7).
        assert!(!is_outdated(&r, Some("2.0.0.1"), None, true));
    }

    #[test]
    fn record_identity_round_trips() {
        let r = record();
        let json = serde_json::to_string(&r).unwrap();
        let back: InstallRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(InstallRecord::dlc_key(None), "base");
        assert_eq!(InstallRecord::dlc_key(Some(8)), "8");
        assert_eq!(back.checksum_ts, Some(1_760_000_000));
    }
}
