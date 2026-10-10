//! Filters (os/language/kinds/dlcs) over catalog → `FileTask`s (`docs/design/04` §1).
//!
//! Review A1: DLC entries enter **only** here, via the parent product's
//! `expanded_dlcs`, gated by `with_dlcs` — the library list itself is games-only.

use std::path::Path;

use uuid::Uuid;

use crate::engine::task::{FileTask, Kind, Status};
use crate::gog::catalog::{InstallerEntry, Os, Product};

/// Filter set, built from CLI flags + config defaults.
#[derive(Debug, Clone, Default)]
pub struct Filters {
    /// Default: host OS with fallback to windows (docs/design/06 §1; first-class on
    /// Linux per docs/design/README non-goals — Wine/Proton workflow).
    pub os: Option<Os>,
    pub language: Option<String>,
    /// Empty = installers only (the default content selection, 03 §3).
    pub kinds: Vec<Kind>,
    pub with_dlcs: bool,
}

/// Strip filesystem-hostile characters (`/ \ : * ? " < > |`), control chars, collapse
/// whitespace (`docs/design/04` §2). Paths may collide benignly when two products share
/// a title; the record's product id disambiguates (C8).
pub fn sanitize(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_was_space = false;
    for ch in input.chars() {
        if ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            continue;
        }
        if ch.is_whitespace() {
            if !last_was_space && !out.is_empty() {
                out.push(' ');
            }
            last_was_space = true;
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    out.trim_end().to_string()
}

/// Reduce a product (and, with `with_dlcs`, its DLC tree) to concrete file tasks.
/// Pure: no I/O, no state — statuses are refined by `engine::plan`.
pub fn for_product(product: &Product, filters: &Filters, download_root: &Path) -> Vec<FileTask> {
    let mut tasks = Vec::new();

    let title = product
        .title
        .clone()
        .unwrap_or_else(|| product.id.to_string());
    let dir = download_root.join(sanitize(&title));
    tasks.extend(tasks_for(product, product, None, &dir, filters));

    if filters.with_dlcs {
        for dlc in &product.expanded_dlcs {
            let dlc_title = dlc.title.clone().unwrap_or_else(|| dlc.id.to_string());
            let dir = dir.join(sanitize(&dlc_title));
            tasks.extend(tasks_for(product, dlc, Some(dlc.id), &dir, filters));
        }
    }
    tasks
}

/// `parent` supplies record identity (`product_id` = the library game, A1); `source`
/// supplies the `downloads` (the DLC's own, when descending).
fn tasks_for(
    parent: &Product,
    source: &Product,
    dlc_id: Option<u64>,
    dir: &Path,
    filters: &Filters,
) -> Vec<FileTask> {
    let downloads = match &source.downloads {
        Some(d) => d,
        None => return Vec::new(),
    };
    let groups: [(Kind, &Vec<InstallerEntry>); 4] = [
        (Kind::Installer, &downloads.installers),
        (Kind::Patch, &downloads.patches),
        (Kind::BonusContent, &downloads.bonus_content),
        (Kind::LanguagePack, &downloads.language_packs),
    ];
    let selected: &[Kind] = if filters.kinds.is_empty() {
        &[Kind::Installer]
    } else {
        &filters.kinds
    };

    let mut tasks = Vec::new();
    for (kind, entries) in groups {
        if !selected.contains(&kind) {
            continue;
        }
        let matched = language_fallback(entries, filters.language.as_deref());
        for entry in matched {
            // OS filter applies when the entry declares one (installers do; extras
            // typically don't and always pass).
            if let (Some(want), Some(have)) = (filters.os, entry.os)
                && want != have
            {
                continue;
            }
            for file in &entry.files {
                tasks.push(FileTask {
                    id: Uuid::new_v4(),
                    product_id: parent.id,
                    dlc_id,
                    product_title: parent.title.clone(),
                    kind,
                    entry: entry.clone(),
                    file: file.clone(),
                    dir: dir.to_path_buf(),
                    save_name: None,
                    status: Status::New,
                    total_size: file.size,
                    url: None,
                    checksum: None,
                });
            }
        }
    }
    tasks
}

/// Language fallback chain (`docs/design/04` §1): prefer the requested language when
/// any entry has it; otherwise keep everything (single-language products, or a
/// language we don't know about — never an empty selection).
fn language_fallback<'a>(
    entries: &'a [InstallerEntry],
    preferred: Option<&str>,
) -> Vec<&'a InstallerEntry> {
    let all: Vec<&InstallerEntry> = entries.iter().collect();
    match preferred {
        Some(lang) => {
            let matched: Vec<&InstallerEntry> = all
                .iter()
                .copied()
                .filter(|e| e.language.as_deref() == Some(lang))
                .collect();
            if matched.is_empty() { all } else { matched }
        }
        None => all,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installer(id: &str, os: Option<Os>, language: &str) -> InstallerEntry {
        let os_json = match os {
            Some(Os::Windows) => "\"windows\"",
            Some(Os::Mac) => "\"mac\"",
            Some(Os::Linux) => "\"linux\"",
            None => "null",
        };
        serde_json::from_str(&format!(
            r#"{{"id":"{id}","os":{os_json},"language":"{language}","name":"N {id}",
                "files":[{{"id":"{id}","size":100,
                "downlink":"/products/1/downlink/installer/{id}"}}]}}"#
        ))
        .unwrap()
    }

    fn product_with(entries: Vec<InstallerEntry>) -> Product {
        let json = serde_json::to_string(&entries).unwrap();
        serde_json::from_str(&format!(
            r#"{{"id":1,"title":"Game","downloads":{{"installers":{json}}}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn sanitize_strips_hostile_characters() {
        assert_eq!(sanitize("A/B\\C:D*E?F\"G<H>I|J"), "ABCDEFGHIJ");
        assert_eq!(sanitize("  Weird  \t Spaces  "), "Weird Spaces");
        assert_eq!(sanitize("\u{7}control"), "control");
        assert_eq!(sanitize("正常なタイトル"), "正常なタイトル");
    }

    #[test]
    fn os_and_language_filters_and_multipart_expansion() {
        let product = product_with(vec![
            installer("en1installer0", Some(Os::Windows), "en"),
            installer("en1installer1", Some(Os::Windows), "en"),
            installer("fr1installer0", Some(Os::Windows), "fr"),
            installer("en1installer0", Some(Os::Mac), "en"),
        ]);
        let filters = Filters {
            os: Some(Os::Windows),
            language: Some("en".into()),
            kinds: vec![],
            with_dlcs: false,
        };
        let tasks = for_product(&product, &filters, Path::new("/dl"));
        assert_eq!(
            tasks.len(),
            2,
            "multi-part expansion: both en/windows parts"
        );
        assert!(tasks.iter().all(|t| t.dir == Path::new("/dl/Game")));
        assert_eq!(tasks[0].file.id, "en1installer0");
        assert_eq!(tasks[1].file.id, "en1installer1");
    }

    #[test]
    fn language_fallback_keeps_unknown_languages() {
        let product = product_with(vec![installer("de1installer0", Some(Os::Linux), "de")]);
        let filters = Filters {
            os: Some(Os::Linux),
            language: Some("en".into()),
            kinds: vec![],
            with_dlcs: false,
        };
        assert_eq!(for_product(&product, &filters, Path::new("/dl")).len(), 1);
    }

    #[test]
    fn dlcs_enter_only_via_with_dlcs() {
        let product: Product = serde_json::from_str(
            r#"{"id":1,"title":"Game",
                "downloads":{"installers":[]},
                "expanded_dlcs":[{"id":2,"title":"DLC",
                  "downloads":{"installers":[
                    {"id":"d0","os":"windows","language":"en","files":[
                      {"id":"d0","size":1,"downlink":"/p/2/downlink/installer/d0"}]}]}}]}"#,
        )
        .unwrap();
        let mut filters = Filters::default();
        assert!(for_product(&product, &filters, Path::new("/dl")).is_empty());

        filters.with_dlcs = true;
        let tasks = for_product(&product, &filters, Path::new("/dl"));
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].dlc_id, Some(2));
        assert_eq!(tasks[0].product_id, 1);
        assert_eq!(tasks[0].dir, Path::new("/dl/Game/DLC"));
    }

    #[test]
    fn kind_selection() {
        let product: Product = serde_json::from_str(
            r#"{"id":1,"title":"G",
                "downloads":{"installers":[],"bonus_content":[
                    {"id":"b0","name":"soundtrack","files":[
                      {"id":"b0","size":9,"downlink":"/p/1/downlink/extras/b0"}]}]}}"#,
        )
        .unwrap();
        let mut filters = Filters::default();
        assert!(for_product(&product, &filters, Path::new("/dl")).is_empty());
        filters.kinds = vec![Kind::BonusContent];
        let tasks = for_product(&product, &filters, Path::new("/dl"));
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].kind, Kind::BonusContent);
    }
}
