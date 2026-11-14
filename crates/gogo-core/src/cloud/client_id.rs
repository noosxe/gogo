//! The one reader of a built game's `clientId` (design 22 §2.2): its
//! `goggame-<gogid>.info`, and the id-only field of the kept
//! `build-manifest.json`.
//!
//! Callers differ in what a broken info file means, so the reader reports it
//! and leaves the policy to them: the credentials path refuses it (it is the
//! check a fetched manifest is held to), the account-free `saves locations`
//! falls back to the kept manifest.
//!
//! SECURITY: both files are read through typed structs that hold only the
//! client id, so a manifest's secret is never parsed into a value here.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::locations::Layout;

/// What a game's `goggame-<gogid>.info` says about its client id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InfoId {
    /// No info file, or one without a `clientId` (missing, `null` or empty).
    Absent,
    /// A usable client id (safe in a URL and a file name).
    Id(String),
    /// The file is there but can't be used: unreadable, not JSON, or a
    /// `clientId` that isn't a usable id.
    Bad { path: PathBuf, reason: String },
}

/// The game's info file: in its files (depot lane), else anywhere under the
/// prefix's `drive_c` (installer lane). The name is matched without regard to
/// case.
pub fn find_info(layout: &Layout, gogid: u64) -> Option<PathBuf> {
    crate::linuxapp::dlc_installer::info_in(&layout.game_files, gogid).or_else(|| {
        crate::linuxapp::dlc_installer::find_info(&layout.prefix.join("drive_c"), gogid)
    })
}

/// What gogo reads of an info file or a build manifest: the client id only.
#[derive(Deserialize)]
struct IdOnly {
    #[serde(rename = "clientId", default)]
    client_id: Option<serde_json::Value>,
}

/// The manifest as kept: `{"manifest": {...}}`, the manifest flat or wrapped
/// by gog-warp's version tag (`{"V2": {...}}`), whichever gogo wrote.
#[derive(Deserialize)]
struct ManifestIds {
    manifest: Option<ManifestShape>,
}

#[derive(Deserialize)]
struct ManifestShape {
    #[serde(rename = "clientId", default)]
    client_id: Option<serde_json::Value>,
    #[serde(rename = "V2", default)]
    v2: Option<IdOnly>,
    #[serde(rename = "V1", default)]
    v1: Option<IdOnly>,
}

fn strip_bom(mut raw: Vec<u8>) -> Vec<u8> {
    if raw.starts_with(b"\xEF\xBB\xBF") {
        raw.drain(..3);
    }
    raw
}

/// `Ok(None)` for a missing or empty id, `Err` for one that isn't usable.
fn usable(v: Option<serde_json::Value>) -> Result<Option<String>, String> {
    match v {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) if s.is_empty() => Ok(None),
        Some(serde_json::Value::String(s)) => super::remote_config::check_client_id(&s)
            .map(|()| Some(s))
            .map_err(|_| "its clientId isn't a GOG client id".to_string()),
        Some(_) => Err("its clientId isn't a string".to_string()),
    }
}

/// Read the info file at `path`.
pub fn read_info(path: &Path) -> InfoId {
    let bad = |reason: String| InfoId::Bad {
        path: path.to_path_buf(),
        reason,
    };
    let raw = match std::fs::read(path) {
        Ok(r) => strip_bom(r),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return InfoId::Absent,
        Err(e) => return bad(format!("can't be read ({e})")),
    };
    let info: IdOnly = match serde_json::from_slice(&raw) {
        Ok(i) => i,
        Err(e) => return bad(format!("isn't a valid info file ({e})")),
    };
    match usable(info.client_id) {
        Ok(Some(id)) => InfoId::Id(id),
        Ok(None) => InfoId::Absent,
        Err(reason) => bad(reason),
    }
}

/// The game's info file's client id (found by [`find_info`]).
pub fn info_id(layout: &Layout, gogid: u64) -> InfoId {
    match find_info(layout, gogid) {
        Some(p) => read_info(&p),
        None => InfoId::Absent,
    }
}

/// The `clientId` field of game dir `root`'s kept `build-manifest.json`, when
/// it has a usable one. Nothing else of the manifest is parsed.
pub fn kept_manifest_id(root: &Path) -> Option<String> {
    let raw = strip_bom(std::fs::read(crate::gog::build_manifest::path(root)).ok()?);
    let m: ManifestIds = serde_json::from_slice(&raw).ok()?;
    let m = m.manifest?;
    usable(
        m.client_id
            .or_else(|| m.v2.and_then(|v| v.client_id))
            .or_else(|| m.v1.and_then(|v| v.client_id)),
    )
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_file_states() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("goggame-1.info");
        assert_eq!(read_info(&p), InfoId::Absent);
        for (body, want) in [
            ("\u{feff}{\"clientId\":\"4242\"}", Some("4242")),
            (r#"{"gameId":"1"}"#, None),
            (r#"{"clientId":null}"#, None),
            (r#"{"clientId":""}"#, None),
        ] {
            std::fs::write(&p, body).unwrap();
            match (read_info(&p), want) {
                (InfoId::Id(id), Some(w)) => assert_eq!(id, w),
                (InfoId::Absent, None) => {}
                (got, _) => panic!("{body}: {got:?}"),
            }
        }
        for body in [
            "{",
            r#"{"clientId":"../x"}"#,
            r#"{"clientId":12}"#,
            r#"{"clientId":["1"]}"#,
        ] {
            std::fs::write(&p, body).unwrap();
            assert!(
                matches!(read_info(&p), InfoId::Bad { .. }),
                "{body} should be bad"
            );
        }
        std::fs::remove_file(&p).unwrap();
        std::fs::create_dir(&p).unwrap();
        let InfoId::Bad { reason, .. } = read_info(&p) else {
            panic!("a dir is unreadable")
        };
        assert!(reason.contains("can't be read"));
    }

    #[test]
    fn manifest_id_only() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(kept_manifest_id(dir.path()), None);
        let f = crate::gog::build_manifest::path(dir.path());
        for body in [
            r#"{"schema":1,"manifest":{"V2":{"clientId":"31337","clientSecret":"made-up"}}}"#,
            r#"{"schema":1,"manifest":{"clientId":"31337","clientSecret":"made-up"}}"#,
        ] {
            std::fs::write(&f, body).unwrap();
            assert_eq!(kept_manifest_id(dir.path()).as_deref(), Some("31337"));
        }
        std::fs::write(&f, r#"{"manifest":{"clientId":"../x"}}"#).unwrap();
        assert_eq!(kept_manifest_id(dir.path()), None);
    }
}
