//! The three-way classification (design 22 §4.2): per file, the cloud
//! listing, the local file and the sync record decide what a sync does.
//! Content (hashes) decides, never clock times. Pure: the caller hashes the
//! local file and fetches the cloud object's plain MD5 where needed.

use serde::Serialize;

use super::record::FileRecord;

/// The local side of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Local {
    pub size: u64,
    pub mtime: toml::value::Datetime,
    /// The plain MD5, when the caller hashed the file (see [`needs_local_md5`]).
    pub md5: Option<String>,
}

/// The cloud side of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cloud {
    /// Not listed.
    Absent,
    /// Listed as a deletion marker.
    Tombstone,
    /// Listed: the stored gzip's MD5.
    Present { hash: String },
}

/// How a side compares to the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// Not there, and not in the record either.
    Absent,
    Same,
    Changed,
    /// There, not in the record.
    New,
    /// In the record, gone (a tombstone or no entry in the cloud).
    Deleted,
}

/// Why a file needs a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictReason {
    /// Changed on both sides since the last sync.
    BothChanged,
    /// Changed here, deleted in the cloud.
    LocalChangedCloudDeleted,
    /// Deleted here, changed in the cloud.
    LocalDeletedCloudChanged,
    /// No record, on both sides, with different content (first sync, D8).
    BothNewDiffer,
    /// A rollback is pending (§4.3): every transfer or delete needs a decision.
    RolledBack,
    /// Two cloud names that differ only by case map onto one local file.
    CaseClash,
}

/// What a sync would do with one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "action", content = "reason")]
pub enum Action {
    /// The same on both sides.
    Nothing,
    Upload,
    Download,
    /// The cloud deleted it; delete the local file (to the backup dir).
    DeleteLocal,
    /// Deleted here; delete it in the cloud.
    DeleteCloud,
    /// No record, the same content on both sides: only recorded.
    Record,
    /// Deleted on both sides: dropped from the record.
    Forget,
    Conflict(ConflictReason),
    /// The cloud object failed its MD5 check when fetched for a comparison:
    /// nothing is decided for it.
    Corrupt,
}

impl Action {
    /// Moves or deletes data on either side.
    pub fn transfers(self) -> bool {
        matches!(
            self,
            Action::Upload | Action::Download | Action::DeleteLocal | Action::DeleteCloud
        )
    }
}

/// Whether the caller must hash the local file before [`classify`]: when the
/// record's size or mtime differ (a `touch` alone is no change), and on a
/// file that is new on both sides (the first sync compares content).
pub fn needs_local_md5(local: &Local, cloud: &Cloud, record: Option<&FileRecord>) -> bool {
    match record {
        Some(r) => local.size != r.size || local.mtime != r.mtime,
        None => matches!(cloud, Cloud::Present { .. }),
    }
}

/// The local side against the record. Without a hash where
/// [`needs_local_md5`] asked for one, a differing size or mtime counts as a
/// change.
pub fn local_side(local: Option<&Local>, record: Option<&FileRecord>) -> Side {
    match (local, record) {
        (None, None) => Side::Absent,
        (Some(_), None) => Side::New,
        (None, Some(_)) => Side::Deleted,
        (Some(l), Some(r)) => {
            let untouched = l.size == r.size && l.mtime == r.mtime;
            if untouched || l.md5.as_deref() == Some(r.local_md5.as_str()) {
                Side::Same
            } else {
                Side::Changed
            }
        }
    }
}

/// The cloud side against the record.
pub fn cloud_side(cloud: &Cloud, record: Option<&FileRecord>) -> Side {
    match (cloud, record) {
        (Cloud::Present { .. }, None) => Side::New,
        (Cloud::Absent | Cloud::Tombstone, None) => Side::Absent,
        (Cloud::Present { hash }, Some(r)) if *hash == r.cloud_hash => Side::Same,
        (Cloud::Present { .. }, Some(_)) => Side::Changed,
        (Cloud::Absent | Cloud::Tombstone, Some(_)) => Side::Deleted,
    }
}

/// The table of design 22 §4.2. `same_content` answers the new/new row (no
/// record, on both sides): whether the plain MD5s are equal; `None` counts
/// as different. `rolled_back` turns every transfer or delete into a
/// conflict (§4.3). `None`: no file at all (only a tombstone, no record).
pub fn classify(
    local: Side,
    cloud: Side,
    same_content: Option<bool>,
    rolled_back: bool,
) -> Option<Action> {
    use Side::*;
    let a = match (local, cloud) {
        (Absent, Absent) => return None,
        (Same, Same) => Action::Nothing,
        (Changed | New, Same | Absent) => Action::Upload,
        (Same | Absent, Changed | New) => Action::Download,
        (Same, Deleted) => Action::DeleteLocal,
        (Deleted, Same) => Action::DeleteCloud,
        (Changed, Changed) => Action::Conflict(ConflictReason::BothChanged),
        (Changed, Deleted) => Action::Conflict(ConflictReason::LocalChangedCloudDeleted),
        (Deleted, Changed) => Action::Conflict(ConflictReason::LocalDeletedCloudChanged),
        (New, New) => {
            if same_content == Some(true) {
                Action::Record
            } else {
                Action::Conflict(ConflictReason::BothNewDiffer)
            }
        }
        (Deleted, Deleted) => Action::Forget,
        // A side is New only without a record and Same/Changed/Deleted only
        // with one, so these pairs can't happen; deciding nothing is safe.
        (New, Changed | Deleted)
        | (Changed | Deleted, New)
        | (Same | Deleted, Absent)
        | (Absent, Same | Deleted) => Action::Conflict(ConflictReason::BothChanged),
    };
    Some(if rolled_back && a.transfers() {
        Action::Conflict(ConflictReason::RolledBack)
    } else {
        a
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec() -> FileRecord {
        FileRecord {
            cloud_hash: "c".repeat(32),
            local_md5: "l".repeat(32),
            size: 10,
            mtime: "2026-11-13T09:41:07Z".parse().unwrap(),
        }
    }

    fn local(size: u64, mtime: &str, md5: Option<&str>) -> Local {
        Local {
            size,
            mtime: mtime.parse().unwrap(),
            md5: md5.map(str::to_string),
        }
    }

    #[test]
    fn local_side_by_size_mtime_then_hash() {
        let r = rec();
        let same = local(10, "2026-11-13T09:41:07Z", None);
        assert_eq!(local_side(Some(&same), Some(&r)), Side::Same);
        assert!(!needs_local_md5(&same, &Cloud::Absent, Some(&r)));
        // A touch alone is no change.
        let touched = local(10, "2026-11-14T00:00:00Z", Some(&"l".repeat(32)));
        assert!(needs_local_md5(&touched, &Cloud::Absent, Some(&r)));
        assert_eq!(local_side(Some(&touched), Some(&r)), Side::Same);
        let changed = local(11, "2026-11-13T09:41:07Z", Some(&"x".repeat(32)));
        assert_eq!(local_side(Some(&changed), Some(&r)), Side::Changed);
        // Not hashed though it should have been: a change, never "same".
        let unhashed = local(11, "2026-11-13T09:41:07Z", None);
        assert_eq!(local_side(Some(&unhashed), Some(&r)), Side::Changed);
        assert_eq!(local_side(None, Some(&r)), Side::Deleted);
        assert_eq!(local_side(Some(&same), None), Side::New);
        assert_eq!(local_side(None, None), Side::Absent);
        // First sync: hashed only when the cloud has the file too.
        assert!(needs_local_md5(
            &same,
            &Cloud::Present { hash: "h".into() },
            None
        ));
        assert!(!needs_local_md5(&same, &Cloud::Tombstone, None));
    }

    #[test]
    fn cloud_side_by_hash() {
        let r = rec();
        let p = |h: &str| Cloud::Present { hash: h.into() };
        assert_eq!(cloud_side(&p(&"c".repeat(32)), Some(&r)), Side::Same);
        assert_eq!(cloud_side(&p("other"), Some(&r)), Side::Changed);
        assert_eq!(cloud_side(&Cloud::Tombstone, Some(&r)), Side::Deleted);
        assert_eq!(cloud_side(&Cloud::Absent, Some(&r)), Side::Deleted);
        assert_eq!(cloud_side(&p("x"), None), Side::New);
        assert_eq!(cloud_side(&Cloud::Tombstone, None), Side::Absent);
    }

    #[test]
    fn the_table_row_by_row() {
        use Action::*;
        use ConflictReason::*;
        use Side::{Absent, Changed, Deleted, New, Same};
        let c = |l, cl| classify(l, cl, None, false);
        assert_eq!(c(Same, Same), Some(Nothing));
        assert_eq!(c(Changed, Same), Some(Upload));
        assert_eq!(c(New, Absent), Some(Upload));
        assert_eq!(c(Same, Changed), Some(Download));
        assert_eq!(c(Absent, New), Some(Download));
        assert_eq!(c(Same, Deleted), Some(DeleteLocal));
        assert_eq!(c(Deleted, Same), Some(DeleteCloud));
        assert_eq!(c(Changed, Changed), Some(Conflict(BothChanged)));
        assert_eq!(c(Changed, Deleted), Some(Conflict(LocalChangedCloudDeleted)));
        assert_eq!(c(Deleted, Changed), Some(Conflict(LocalDeletedCloudChanged)));
        assert_eq!(classify(New, New, Some(true), false), Some(Record));
        assert_eq!(
            classify(New, New, Some(false), false),
            Some(Conflict(BothNewDiffer))
        );
        assert_eq!(c(New, New), Some(Conflict(BothNewDiffer)));
        assert_eq!(c(Deleted, Deleted), Some(Forget));
        assert_eq!(c(Absent, Absent), None);
    }

    #[test]
    fn a_rollback_turns_every_transfer_into_a_conflict() {
        use Side::*;
        for (l, cl) in [
            (Changed, Same),
            (Same, Changed),
            (Same, Deleted),
            (Deleted, Same),
            (New, Absent),
            (Absent, New),
        ] {
            assert_eq!(
                classify(l, cl, None, true),
                Some(Action::Conflict(ConflictReason::RolledBack)),
                "{l:?}/{cl:?}"
            );
        }
        assert_eq!(classify(Same, Same, None, true), Some(Action::Nothing));
        assert_eq!(classify(Deleted, Deleted, None, true), Some(Action::Forget));
        assert_eq!(
            classify(New, New, Some(true), true),
            Some(Action::Record)
        );
    }
}
