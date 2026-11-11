//! Bringing a depot cache record from before DLC and language tracking up to date
//! (#374, design 19 §3.3).
//!
//! A record an older gogo wrote has `buildId` and `files` but no `language` or
//! `dlcs`, so it can't say what the tree holds: a build hashes the whole tree, and a
//! `dlc add` stages in the game dir, which leaves the DLC out of the cache. When
//! the build is the installed one and its tree is there, the build's depot manifests
//! can tell what it holds without hashing: the language from the depots whose files
//! are all on disk, the DLC from the `goggame-<id>.info` files present, each checked
//! against its depot files. When that can't be decided safely, the record is left
//! alone.
//!
//! [`decide`] is pure over the fetched depot lists; [`apply`] checks the disk again
//! and writes the record, last and durably. It never touches the tree.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gog_warp::content_system::types::Manifest;

use super::depot::{self, CacheCheck, CacheWant, DepotError};
use super::dlc_filter::{self, DepotList, ProductFiles};
use crate::macapp::info;

/// What an older record becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upgrade {
    pub build_id: String,
    pub language: String,
    /// The DLC the tree holds whole: an info present and every file of theirs.
    pub dlcs: Vec<String>,
    /// Every product's file list in `language` (base and each DLC of the build).
    pub products: ProductFiles,
    /// DLC left out of `dlcs` though some of their files are there.
    pub notes: Vec<String>,
}

impl Upgrade {
    /// `en-US; DLC: none` / `en-US; DLC: 1597316373, 1256837418`.
    pub fn summary(&self) -> String {
        format!(
            "{}; DLC: {}",
            self.language,
            if self.dlcs.is_empty() {
                "none".to_string()
            } else {
                self.dlcs.join(", ")
            }
        )
    }

    /// The paths the new record vouches for: the base's and each held DLC's.
    fn claimed(&self, base: &str) -> impl Iterator<Item = &String> {
        self.products
            .iter()
            .filter(move |(p, _)| *p == base || self.dlcs.contains(p))
            .flat_map(|(_, l)| l.iter())
    }
}

/// Case-insensitive lookups under a cache root, each dir read once.
#[derive(Default)]
pub(crate) struct OnDisk(HashMap<PathBuf, Option<Vec<(String, bool)>>>);

impl OnDisk {
    /// `rel` as spelled on disk under `root`: each component as is when it's there,
    /// else a case-insensitive match (a dir for all but the last). Manifests
    /// are case-insensitive, and gog-warp writes a path shared between depots once,
    /// as the first one spells it. `None` when it isn't there, or is a dir.
    pub(crate) fn find(&mut self, root: &Path, rel: &str) -> Option<String> {
        let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
        let mut at = root.to_path_buf();
        let mut out: Vec<String> = Vec::new();
        for (i, comp) in parts.iter().enumerate() {
            let last = i + 1 == parts.len();
            let fits = |m: &std::fs::Metadata| last != m.is_dir();
            let name = match at.join(comp).symlink_metadata() {
                Ok(m) if fits(&m) => comp.to_string(),
                _ => {
                    let lower = comp.to_lowercase();
                    let entries = self.0.entry(at.clone()).or_insert_with(|| {
                        let rd = std::fs::read_dir(&at).ok()?;
                        Some(
                            rd.flatten()
                                .filter_map(|e| {
                                    let n = e.file_name().into_string().ok()?;
                                    let dir = e.file_type().ok()?.is_dir();
                                    Some((n, dir))
                                })
                                .collect(),
                        )
                    });
                    entries
                        .as_ref()?
                        .iter()
                        .find(|(n, dir)| last != *dir && n.to_lowercase() == lower)?
                        .0
                        .clone()
                }
            };
            at.push(&name);
            out.push(name);
        }
        (!out.is_empty()).then(|| out.join("/"))
    }

    /// The paths of `list` that aren't under `root`.
    fn missing<'a>(&mut self, root: &Path, list: &'a [String]) -> Vec<&'a String> {
        list.iter()
            .filter(|p| self.find(root, p).is_none())
            .collect()
    }
}

fn specific(d: &DepotList) -> bool {
    !d.languages.iter().any(|l| l == "*")
}

fn some_of(v: &[&String]) -> String {
    let shown: Vec<&str> = v.iter().take(3).map(|s| s.as_str()).collect();
    format!(
        "{}{}",
        shown.join(", "),
        if v.len() > shown.len() { ", …" } else { "" }
    )
}

/// Pure (reads the cache, never writes): what the older record of build `build_id`
/// in cache root `root` becomes for `language`, from the build's depots (every
/// depot, every language: [`dlc_filter::fetch_depots`] with no filter). `base` is
/// the base game's product id, `tree_name` the install dir. `Err` says why it
/// can't be decided safely, and the record stays as it is.
///
/// - The record has to be an older one (no `language` or `dlcs`) of `build_id`,
///   with every file it names still there and the tree present.
/// - **Language:** the base's depots in `language` (or `*`) are all on disk, the
///   build has depots in that language when it has language-specific ones at all,
///   and no depot of another language shares a path with them through another
///   depot manifest (the same path in two languages can't be told apart without
///   hashing).
/// - **DLC:** a product of the build with a `goggame-<id>.info` in the tree or in
///   its listed files, and every listed file of it on disk. A DLC with some files
///   there and others not is left out (a later add fetches the rest), and so is an
///   info that names no product of the build.
pub fn decide(
    root: &Path,
    tree_name: &str,
    base: &str,
    build_id: &str,
    depots: &[DepotList],
    language: &str,
) -> Result<Upgrade, String> {
    if !depot::record_is_old(root, build_id) {
        return Err(format!(
            "its record isn't one from before DLC tracking for build {build_id}"
        ));
    }
    let tree = root.join(tree_name);
    if !tree.is_dir() {
        return Err(format!("its game tree ({tree_name}) isn't there"));
    }
    if let CacheCheck::Missing { count, .. } = depot::check_cache(
        root,
        &CacheWant {
            build_id: Some(build_id),
            ..Default::default()
        },
    ) {
        return Err(format!("it lost {count} file(s) its record names"));
    }
    let matches = |d: &DepotList| dlc_filter::language_matches(&d.languages, language);
    let base_depots: Vec<&DepotList> = depots.iter().filter(|d| d.product == base).collect();
    if !base_depots.iter().any(|d| matches(d)) {
        return Err(format!(
            "the build has no depots of the base game in {language}"
        ));
    }
    if base_depots.iter().any(|d| specific(d))
        && !base_depots.iter().any(|d| specific(d) && matches(d))
    {
        return Err(format!("the build has no {language} depots"));
    }
    // A path two languages share through different depot manifests may hold
    // either one's bytes: existence can't tell.
    let mut ours: HashMap<String, HashSet<&str>> = HashMap::new();
    for d in depots.iter().filter(|d| matches(d)) {
        for p in &d.paths {
            ours.entry(p.to_lowercase())
                .or_default()
                .insert(d.manifest.as_str());
        }
    }
    for d in depots.iter().filter(|d| !matches(d)) {
        if let Some(p) = d.paths.iter().find(|p| {
            ours.get(&p.to_lowercase())
                .is_some_and(|m| !m.contains(d.manifest.as_str()))
        }) {
            return Err(format!(
                "{language}'s files share a path with {}'s ({p}), so which one the cache \
                 holds can't be told without hashing",
                d.languages.join("/")
            ));
        }
    }
    let mut products: Vec<String> = vec![base.to_string()];
    for d in depots {
        if !products.contains(&d.product) {
            products.push(d.product.clone());
        }
    }
    let lists = dlc_filter::product_files(depots, language, &products);
    let mut disk = OnDisk::default();
    let base_list = &lists[base];
    if base_list.is_empty() {
        return Err(format!("the base game has no files in {language}"));
    }
    let gone = disk.missing(root, base_list);
    if !gone.is_empty() {
        return Err(format!(
            "{} of the base game's {language} files aren't in it ({})",
            gone.len(),
            some_of(&gone)
        ));
    }
    let infos: Vec<String> = info::read_infos(&tree)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    let mut dlcs = Vec::new();
    let mut notes = Vec::new();
    for (p, list) in lists.iter().filter(|(p, _)| *p != base) {
        let info_name = format!("goggame-{p}.info");
        let has_info = infos.contains(p)
            || list.iter().any(|f| {
                f.rsplit('/')
                    .next()
                    .is_some_and(|n| n.eq_ignore_ascii_case(&info_name))
                    && disk.find(root, f).is_some()
            });
        if !has_info {
            continue;
        }
        let gone = disk.missing(root, list);
        if list.is_empty() {
            notes.push(format!(
                "DLC {p} has no files in {language}: not listed as held"
            ));
        } else if gone.is_empty() {
            dlcs.push(p.clone());
        } else {
            notes.push(format!(
                "DLC {p}'s info is there but {} of its {} files aren't: not listed as held",
                gone.len(),
                list.len()
            ));
        }
    }
    for id in infos
        .iter()
        .filter(|i| *i != base && !lists.contains_key(*i))
    {
        notes.push(format!(
            "goggame-{id}.info names no product of build {build_id}: not listed as held"
        ));
    }
    dlcs.sort();
    Ok(Upgrade {
        build_id: build_id.to_string(),
        language: language.to_string(),
        dlcs,
        products: lists,
        notes,
    })
}

/// Write `up` into cache root `root`'s record, after checking again that every file
/// it claims (the base's and the held DLC's) is there and the record is still the
/// older one it was decided from. `Ok(false)`: something changed, nothing written.
/// The record is the only write, made durably, so a crash leaves the old record or
/// the new one, never a claim of files that aren't there.
pub fn apply(root: &Path, base: &str, up: &Upgrade) -> std::io::Result<bool> {
    let mut disk = OnDisk::default();
    let claimed: Vec<String> = up.claimed(base).cloned().collect();
    if !disk.missing(root, &claimed).is_empty() {
        return Ok(false);
    }
    depot::upgrade_record(root, &up.build_id, &up.language, &up.dlcs, &up.products)
}

/// Fetch build `build_id`'s depots (`manifest`) and [`decide`] for cache root
/// `root`. `None` when there's nothing to bring up to date (no older record of this
/// build); `Some(Err)` names why it can't be done.
pub async fn check(
    http: &reqwest::Client,
    root: &Path,
    manifest: &Manifest,
    build_id: &str,
    language: &str,
) -> Option<Result<Upgrade, String>> {
    if !depot::record_is_old(root, build_id) {
        return None;
    }
    let tree_name = manifest.install_directory();
    if !root.join(&tree_name).is_dir() {
        return None;
    }
    let base = match manifest {
        Manifest::V2(m) => m.base_product_id().to_string(),
        Manifest::V1(m) => m.product().root_game_id().to_string(),
    };
    Some(match dlc_filter::fetch_depots(http, manifest, None).await {
        Ok(depots) => decide(root, &tree_name, &base, build_id, &depots, language),
        Err(e) => Err(why(&e)),
    })
}

fn why(e: &DepotError) -> String {
    format!("its build's depot manifests couldn't be read: {e}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dl(product: &str, langs: &[&str], manifest: &str, paths: &[&str]) -> DepotList {
        DepotList {
            product: product.into(),
            languages: langs.iter().map(|s| s.to_string()).collect(),
            manifest: manifest.into(),
            paths: paths.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn put(root: &Path, rel: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, rel).unwrap();
    }

    fn old_record(root: &Path, build: &str) {
        let mut files = Vec::new();
        for e in walkdir(root) {
            files.push(e);
        }
        files.sort();
        std::fs::write(
            root.join(depot::RECORD),
            serde_json::json!({ "buildId": build, "files": files }).to_string(),
        )
        .unwrap();
    }

    fn walkdir(root: &Path) -> Vec<String> {
        fn go(root: &Path, dir: &Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    go(root, &p, out);
                } else if e.file_name() != depot::RECORD {
                    out.push(
                        p.strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
            }
        }
        let mut out = Vec::new();
        go(root, root, &mut out);
        out
    }

    /// Base `1` (common files + English and German voice depots), DLC `2` and `3`.
    fn depots() -> Vec<DepotList> {
        vec![
            dl(
                "1",
                &["*"],
                "m-base",
                &["G/bin/game.exe", "G/goggame-1.info"],
            ),
            dl("1", &["en-US"], "m-en", &["G/vo/en.pak"]),
            dl("1", &["de-DE"], "m-de", &["G/vo/de.pak"]),
            dl("2", &["*"], "m-2", &["G/dlc2/a.pak", "G/goggame-2.info"]),
            dl("3", &["*"], "m-3", &["G/dlc3/b.pak", "G/goggame-3.info"]),
        ]
    }

    fn tree(root: &Path) {
        for f in ["G/bin/game.exe", "G/goggame-1.info", "G/vo/en.pak"] {
            put(root, f);
        }
        std::fs::write(
            root.join("G/goggame-1.info"),
            r#"{"gameId":"1","name":"G"}"#,
        )
        .unwrap();
    }

    #[test]
    fn an_old_record_gets_language_dlcs_and_lists() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        tree(root);
        put(root, "G/dlc2/a.pak");
        std::fs::write(
            root.join("G/goggame-2.info"),
            r#"{"gameId":"2","name":"D2"}"#,
        )
        .unwrap();
        // DLC 3: an info but not its files.
        std::fs::write(
            root.join("G/goggame-3.info"),
            r#"{"gameId":"3","name":"D3"}"#,
        )
        .unwrap();
        old_record(root, "b1");
        let up = decide(root, "G", "1", "b1", &depots(), "en-US").unwrap();
        assert_eq!(up.language, "en-US");
        assert_eq!(up.dlcs, vec!["2".to_string()]);
        assert_eq!(up.products["1"].len(), 3, "the base's * and en-US files");
        assert!(
            up.notes.iter().any(|n| n.contains("DLC 3")),
            "{:?}",
            up.notes
        );
        let before = std::fs::read_to_string(root.join(depot::RECORD)).unwrap();
        assert!(apply(root, "1", &up).unwrap());
        let rec = depot::read_record(root).unwrap();
        assert_eq!(rec.language.as_deref(), Some("en-US"));
        assert_eq!(rec.dlcs, Some(vec!["2".to_string()]));
        assert!(
            depot::record_products(root, "b1")
                .unwrap()
                .contains_key("3")
        );
        // The files it named stay as they were.
        let files =
            |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap()["files"].clone();
        assert_eq!(
            files(&before),
            files(&std::fs::read_to_string(root.join(depot::RECORD)).unwrap())
        );
        // Once current, there's nothing more to do.
        assert!(decide(root, "G", "1", "b1", &depots(), "en-US").is_err());
        assert!(!apply(root, "1", &up).unwrap());
    }

    #[test]
    fn another_language_or_build_or_a_lost_file_leaves_it_alone() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        tree(root);
        old_record(root, "b1");
        let before = std::fs::read_to_string(root.join(depot::RECORD)).unwrap();
        // German voice files aren't there.
        let e = decide(root, "G", "1", "b1", &depots(), "de-DE").unwrap_err();
        assert!(e.contains("de-DE"), "{e}");
        // A language the build has no depots in.
        let e = decide(root, "G", "1", "b1", &depots(), "fr-FR").unwrap_err();
        assert!(e.contains("no fr-FR depots"), "{e}");
        // Another build.
        assert!(decide(root, "G", "1", "b2", &depots(), "en-US").is_err());
        // A file the record names is gone.
        std::fs::remove_file(root.join("G/vo/en.pak")).unwrap();
        let e = decide(root, "G", "1", "b1", &depots(), "en-US").unwrap_err();
        assert!(e.contains("lost 1 file"), "{e}");
        assert_eq!(
            before,
            std::fs::read_to_string(root.join(depot::RECORD)).unwrap()
        );
    }

    #[test]
    fn a_path_two_languages_share_through_other_manifests_is_undecidable() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        tree(root);
        old_record(root, "b1");
        let mut d = depots();
        d[2].paths = vec!["G/vo/en.pak".into()];
        let e = decide(root, "G", "1", "b1", &d, "en-US").unwrap_err();
        assert!(e.contains("can't be told without hashing"), "{e}");
        // The same depot manifest behind both languages is the same bytes.
        d[2].manifest = "m-en".into();
        assert!(decide(root, "G", "1", "b1", &d, "en-US").is_ok());
    }

    #[test]
    fn apply_refuses_when_a_claimed_file_went_away() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        tree(root);
        put(root, "G/dlc2/a.pak");
        std::fs::write(
            root.join("G/goggame-2.info"),
            r#"{"gameId":"2","name":"D2"}"#,
        )
        .unwrap();
        old_record(root, "b1");
        let up = decide(root, "G", "1", "b1", &depots(), "en-US").unwrap();
        std::fs::remove_file(root.join("G/dlc2/a.pak")).unwrap();
        let before = std::fs::read_to_string(root.join(depot::RECORD)).unwrap();
        assert!(!apply(root, "1", &up).unwrap());
        assert_eq!(
            before,
            std::fs::read_to_string(root.join(depot::RECORD)).unwrap()
        );
    }

    #[test]
    fn paths_match_case_insensitively() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        tree(root);
        old_record(root, "b1");
        let mut d = depots();
        d[0].paths = vec!["G/BIN/Game.exe".into(), "G/goggame-1.info".into()];
        assert!(decide(root, "G", "1", "b1", &d, "en-US").is_ok());
    }
}
