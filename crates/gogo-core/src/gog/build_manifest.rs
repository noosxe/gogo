//! The installed build's manifest, kept in the game dir as `build-manifest.json`
//! (design 21 §5.2, D6). A later update diffs from it after GOG stops listing the
//! build, and `gogo builds` describes an unlisted installed build from it.
//!
//! The file carries the game's Galaxy client secret (design 21 §1.2): nothing here
//! prints, logs or formats its contents, and it is written readable by the user only.

use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use gog_warp::content_system::types::Manifest;
use serde::{Deserialize, Serialize};

/// The file's name in a game dir.
pub const FILE: &str = "build-manifest.json";

/// The schema this gogo reads and writes.
pub const SCHEMA: u32 = 1;

/// The file: a small header and the build manifest as gog-warp parsed it (its
/// unknown fields are kept, so nothing GOG sent is dropped).
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stored {
    pub schema: u32,
    pub build_id: String,
    pub generation: u32,
    /// When it was fetched, RFC 3339.
    pub fetched: String,
    pub manifest: Manifest,
}

// No `Debug`: a derived one would print the manifest.
impl std::fmt::Debug for Stored {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stored")
            .field("build_id", &self.build_id)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

pub fn path(root: &Path) -> PathBuf {
    root.join(FILE)
}

/// Write build `build_id`'s manifest to game dir `root`, atomically.
pub fn write(
    root: &Path,
    build_id: &str,
    generation: u32,
    manifest: &Manifest,
) -> std::io::Result<()> {
    write_named(root, FILE, build_id, generation, manifest)
}

/// [`write`] under another name in `root`: an update keeps its target's manifest
/// beside the game's until the commit renames it into place (design 21 §11g).
pub fn write_named(
    root: &Path,
    name: &str,
    build_id: &str,
    generation: u32,
    manifest: &Manifest,
) -> std::io::Result<()> {
    let stored = Stored {
        schema: SCHEMA,
        build_id: build_id.to_string(),
        generation,
        fetched: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        manifest: manifest.clone(),
    };
    let bytes = serde_json::to_vec(&stored).map_err(std::io::Error::other)?;
    let tmp = root.join(format!("{name}.tmp"));
    write_private(&tmp, &bytes)?;
    std::fs::rename(&tmp, root.join(name)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

fn write_private(p: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(p)?.write_all(bytes)
}

/// The file in game dir `root`. `None` when there is none (a game built before
/// this) or it can't be read: both mean "no manifest kept".
pub fn load(root: &Path) -> Option<Stored> {
    let raw = std::fs::read(path(root)).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// What `gogo builds` says about the stored build when GOG doesn't list it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Described {
    /// When the manifest was fetched (the build's own publish date isn't in it).
    pub fetched: Option<DateTime<Utc>>,
    pub facts: super::builds_report::Facts,
}

/// Describe the kept manifest of game dir `root`, when it is build `build_id`'s,
/// for `language`.
pub fn describe(root: &Path, build_id: &str, language: &str) -> Option<Described> {
    let s = load(root).filter(|s| s.build_id == build_id)?;
    Some(Described {
        fetched: DateTime::parse_from_rfc3339(&s.fetched)
            .ok()
            .map(|d| d.with_timezone(&Utc)),
        facts: super::builds_report::facts(&s.manifest, language),
    })
}

/// Write the manifest unless it is for another build than the game's: a failure
/// is a warning, because the build works without it. Never names the contents.
pub fn write_or_warn(
    root: &Path,
    game_build: Option<&str>,
    kept: &(String, u32, Manifest),
) -> Option<String> {
    if game_build != Some(kept.0.as_str()) {
        return None;
    }
    write(root, &kept.0, kept.1, &kept.2).err().map(|e| {
        format!(
            "couldn't write {}: {e}; the game works, an update later fetches the build's \
             manifest from GOG while it lists the build",
            path(root).display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        serde_json::from_value(serde_json::json!({
            "baseProductId": "1", "installDirectory": "G", "platform": "windows",
            "clientId": "cid", "clientSecret": "sekrit", "extra": 5,
            "products": [{"name": "G", "productId": "1", "temp_executable": "", "temp_arguments": ""},
                         {"name": "D", "productId": "7", "temp_executable": "", "temp_arguments": ""}],
            "depots": [{"size": 9, "compressedSize": 4, "languages": ["en-US"],
                        "manifest": "e", "productId": "1"}]
        }))
        .unwrap()
    }

    #[test]
    fn it_round_trips_with_what_gog_sent() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "42", 2, &manifest()).unwrap();
        assert!(!d.path().join(format!("{FILE}.tmp")).exists());
        let s = load(d.path()).unwrap();
        assert_eq!((s.build_id.as_str(), s.generation), ("42", 2));
        let raw = std::fs::read_to_string(path(d.path())).unwrap();
        assert!(raw.contains("sekrit") && raw.contains("\"extra\":5"));
        // Only the user can read it: it holds the client secret.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(path(d.path())).unwrap();
            assert_eq!(m.permissions().mode() & 0o077, 0);
        }
        // Debug never shows it.
        assert!(!format!("{s:?}").contains("sekrit"));
    }

    #[test]
    fn describe_reads_size_and_dlc_for_the_matching_build_only() {
        let d = tempfile::tempdir().unwrap();
        assert!(
            describe(d.path(), "42", "en-US").is_none(),
            "no file is fine"
        );
        write(d.path(), "42", 2, &manifest()).unwrap();
        let got = describe(d.path(), "42", "en-US").unwrap();
        assert_eq!(got.facts.download_size, Some(4));
        assert_eq!(got.facts.dlcs, vec![7]);
        assert!(got.fetched.is_some());
        assert!(describe(d.path(), "43", "en-US").is_none());
        std::fs::write(path(d.path()), "not json").unwrap();
        assert!(
            describe(d.path(), "42", "en-US").is_none(),
            "garbage is no manifest"
        );
    }

    #[test]
    fn a_failed_write_warns_without_the_contents_and_another_build_is_skipped() {
        let d = tempfile::tempdir().unwrap();
        let kept = ("42".to_string(), 2, manifest());
        assert!(write_or_warn(d.path(), Some("41"), &kept).is_none());
        assert!(!path(d.path()).exists());
        assert!(write_or_warn(d.path(), None, &kept).is_none());
        assert!(write_or_warn(d.path(), Some("42"), &kept).is_none());
        assert!(path(d.path()).exists());
        let gone = d.path().join("missing");
        let w = write_or_warn(&gone, Some("42"), &kept).expect("a warning");
        assert!(w.contains(FILE) && !w.contains("sekrit"));
    }
}
