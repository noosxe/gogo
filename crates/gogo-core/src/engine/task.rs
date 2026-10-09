//! One `files[]` entry + resolved URL + checksum (`docs/design/01` §1 task.rs).

use uuid::Uuid;

/// A single downloadable file: catalog part + resolved CDN URL + authoritative layout.
pub struct FileTask {
    pub id: Uuid,
    pub product_id: u64,
    pub product_title: Option<String>,
    pub kind: Kind,
    pub entry: crate::gog::catalog::InstallerEntry,
    pub file: crate::gog::catalog::FileEntry,
    /// Final destination; during download the bytes live at `<dest>.part`
    /// (atomic rename on completion — "file with final name" ⇒ complete + verified,
    /// `docs/design/04` §5).
    pub dest: std::path::PathBuf,
    /// Filled during resolve: pre-tokenized CDN URL (never logged/persisted).
    pub url: Option<url::Url>,
    /// Filled during resolve: authoritative layout (name, total_size, chunks).
    pub checksum: Option<crate::gog::checksum::ChecksumDoc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Installer,
    Patch,
    BonusContent,
    LanguagePack,
}
