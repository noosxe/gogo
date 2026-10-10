//! One `files[]` entry + resolved URL + checksum (`docs/design/01` §1 task.rs).

use uuid::Uuid;

/// Plan-time status per `docs/design/04` §2 skip logic. Review B4: plan is O(1) per
/// task — no checksum fetches, no file reads; adoption verifies at execute time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// No `.part`, no record → download.
    New,
    /// `{dest}.part` + sidecars on disk → continue remaining chunks (04 §5).
    Resumable,
    /// Record + matching file → skip (`↷ already present`).
    Present,
    /// Record exists but catalog shows a newer version/build → redownload (04 §8).
    Outdated,
    /// Destination dir holds an unrecorded file — verify + adopt (or redownload) at
    /// execute time; plan totals may overstate (accepted, review B4).
    PendingAdoption,
}

/// A single downloadable file: catalog part + resolved CDN URL + authoritative layout.
#[derive(Debug, Clone)]
pub struct FileTask {
    pub id: Uuid,
    pub product_id: u64,
    /// `None` = base game; DLC downloads key on the DLC's own id (A1/A5).
    pub dlc_id: Option<u64>,
    pub product_title: Option<String>,
    pub kind: Kind,
    pub entry: crate::gog::catalog::InstallerEntry,
    pub file: crate::gog::catalog::FileEntry,
    /// `{download.dir}/{title}[/dlc title]` — resolved absolute at plan time.
    pub dir: std::path::PathBuf,
    /// Final save name — the checksum XML `name` attribute, filled at resolve (04 §2);
    /// for extras the C4 chain applies. `None` until then (plan shows the API name).
    pub save_name: Option<String>,
    pub status: Status,
    /// Plan estimate (API `files[].size`); the authoritative total comes with the
    /// checksum doc and can differ (live-verified, docs/11).
    pub total_size: u64,
    /// Filled during resolve: pre-tokenized CDN URL (never logged/persisted).
    pub url: Option<url::Url>,
    /// Filled during resolve: authoritative layout (name, total_size, chunks).
    pub checksum: Option<crate::gog::checksum::ChecksumDoc>,
}

impl FileTask {
    /// Destination path; during download the bytes live at `<dest>.part`
    /// (atomic rename on completion — "file with final name" ⇒ complete + verified,
    /// `docs/design/04` §5).
    pub fn dest(&self) -> std::path::PathBuf {
        let name = self
            .save_name
            .clone()
            .or_else(|| self.entry.name.clone())
            .unwrap_or_else(|| self.file.id.clone());
        self.dir.join(crate::engine::select::sanitize(&name))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Installer,
    Patch,
    BonusContent,
    LanguagePack,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Installer => "installer",
            Kind::Patch => "patch",
            Kind::BonusContent => "bonus_content",
            Kind::LanguagePack => "language_pack",
        }
    }
}
