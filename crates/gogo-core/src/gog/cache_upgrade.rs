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
//!
//! The same depots also find the languages an older gogo's switch left files of
//! (#391): [`scan_languages`] and [`apply_scan`], once per record.
//! [`bring_up_to_date`] runs both from one fetch before a `linux-app` build.

use std::collections::HashMap;
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
    if let Some((d, p)) = dlc_filter::clash(depots, language, |_| true) {
        return Err(format!(
            "{language}'s files share a path with {}'s ({p}), so which one the cache \
             holds can't be told without hashing",
            d.languages.join("/")
        ));
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
    let base = base_of(manifest);
    Some(match dlc_filter::fetch_depots(http, manifest, None).await {
        Ok(depots) => decide(root, &tree_name, &base, build_id, &depots, language),
        Err(e) => Err(why(&e)),
    })
}

fn why(e: &DepotError) -> String {
    format!("its build's depot manifests couldn't be read: {e}")
}

/// The base game's product id of `manifest`'s build.
pub fn base_of(manifest: &Manifest) -> String {
    match manifest {
        Manifest::V2(m) => m.base_product_id().to_string(),
        Manifest::V1(m) => m.product().root_game_id().to_string(),
    }
}

/// A language a cache holds that its record didn't name (#391).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundLanguage {
    /// As the build's depots name it.
    pub language: String,
    /// It shares no path with the record's language through another depot
    /// manifest ([`dlc_filter::clash`]), so its files are there as it has them.
    pub whole: bool,
    /// The DLC whose files in it are all there.
    pub dlcs: Vec<String>,
    /// Every product's file list in it.
    pub products: ProductFiles,
}

/// What [`scan_languages`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageScan {
    pub build_id: String,
    /// The record's language, the one the scan was made against.
    pub language: String,
    pub found: Vec<FoundLanguage>,
    /// Every product's file list in `language`, kept with the record's.
    pub lists: ProductFiles,
    /// Languages only some of whose files are there: not recorded.
    pub notes: Vec<String>,
}

impl LanguageScan {
    /// `de-DE, fr-FR (not whole)`.
    pub fn summary(&self) -> String {
        self.found
            .iter()
            .map(|f| {
                if f.whole {
                    f.language.clone()
                } else {
                    format!("{} (not whole)", f.language)
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Pure (reads the cache, never writes): the languages cache root `root` holds that
/// its record doesn't name (#391), from its build's depots (every depot, every
/// language). A gogo from before #389 kept the old language's files on a switch but
/// recorded only the new one, so a build took them into the game dir.
///
/// A language L is held when the base game has depots in L and not in the record's
/// language, and every file of each is on disk with the size its manifest gives (a
/// path such a depot shares with the record's language through another depot
/// manifest holds that one's bytes, so only its presence counts). With no such base
/// depot, the record's DLC's depots in L count instead; with none of those either,
/// L's files can't be told from the record language's. A language with only some of
/// those files there is left out with a note: its files stay unknown to the filter,
/// which keeps them, as before. Sizes, not hashes: this reads no file.
///
/// Each held language gets what a switch gives the old one (#389): `whole` unless it
/// clashes with the record's language, the DLC whose files in it are all there, and
/// every product's list in it.
pub fn scan_languages(
    root: &Path,
    base: &str,
    depots: &[DepotList],
) -> Result<LanguageScan, String> {
    let rec = depot::read_record(root).ok_or("it has no readable record")?;
    let language = rec.language.clone().ok_or("its record names no language")?;
    let named = rec.languages();
    let mut candidates: Vec<&String> = Vec::new();
    for l in depots
        .iter()
        .filter(|d| specific(d))
        .flat_map(|d| &d.languages)
    {
        let is_named = named
            .iter()
            .any(|n| dlc_filter::language_matches(std::slice::from_ref(l), n));
        if !is_named && !candidates.iter().any(|c| c.eq_ignore_ascii_case(l)) {
            candidates.push(l);
        }
    }
    let mut products: Vec<String> = vec![base.to_string()];
    for d in depots {
        if !products.contains(&d.product) {
            products.push(d.product.clone());
        }
    }
    let rec_dlcs = rec.dlcs.clone().unwrap_or_default();
    // Each path of the record language's depots, with the manifests behind it.
    let mut ours: HashMap<String, Vec<&str>> = HashMap::new();
    for d in depots
        .iter()
        .filter(|d| dlc_filter::language_matches(&d.languages, &language))
    {
        for p in &d.paths {
            ours.entry(p.to_lowercase()).or_default().push(&d.manifest);
        }
    }
    let mut disk = OnDisk::default();
    // How many of `d`'s files are there as it has them, of how many.
    let mut there = |d: &DepotList| -> (usize, usize) {
        let ok = d
            .paths
            .iter()
            .filter(|p| {
                let Some(found) = disk.find(root, p) else {
                    return false;
                };
                let contested = ours
                    .get(&p.to_lowercase())
                    .is_some_and(|m| m.iter().any(|m| *m != d.manifest));
                contested
                    || d.sizes.get(*p).is_none_or(|n| {
                        root.join(found)
                            .symlink_metadata()
                            .is_ok_and(|m| m.len() == *n)
                    })
            })
            .count();
        (ok, d.paths.len())
    };
    let mut found = Vec::new();
    let mut notes = Vec::new();
    for l in candidates {
        let in_l = |d: &&DepotList| dlc_filter::language_matches(&d.languages, l);
        let only = |d: &&DepotList| {
            specific(d) && in_l(d) && !dlc_filter::language_matches(&d.languages, &language)
        };
        let mut telling: Vec<&DepotList> = depots
            .iter()
            .filter(only)
            .filter(|d| d.product == base)
            .collect();
        if telling.is_empty() {
            telling = depots
                .iter()
                .filter(only)
                .filter(|d| rec_dlcs.contains(&d.product))
                .collect();
        }
        if telling.is_empty() {
            continue;
        }
        let (ok, total) = telling
            .iter()
            .map(|d| there(d))
            .fold((0, 0), |(a, b), (c, d)| (a + c, b + d));
        if ok < total {
            if ok > 0 {
                notes.push(format!(
                    "{l}: {ok} of its {total} files are there, so it isn't recorded as held \
                     (builds keep them, as before)"
                ));
            }
            continue;
        }
        let mut dlcs: Vec<String> = Vec::new();
        for p in products.iter().filter(|p| *p != base) {
            let of_p: Vec<&DepotList> = depots.iter().filter(|d| d.product == *p).collect();
            let mine: Vec<&DepotList> = of_p.iter().copied().filter(in_l).collect();
            let held = if of_p.is_empty() {
                rec_dlcs.contains(p)
            } else {
                !mine.is_empty()
                    && mine.iter().all(|d| {
                        let (ok, total) = there(d);
                        ok == total
                    })
            };
            if held {
                dlcs.push(p.clone());
            }
        }
        dlcs.sort();
        found.push(FoundLanguage {
            language: l.clone(),
            whole: dlc_filter::clash(depots, &language, |d| {
                dlc_filter::language_matches(&d.languages, l)
            })
            .is_none(),
            dlcs,
            products: dlc_filter::product_files(depots, l, &products),
        });
    }
    Ok(LanguageScan {
        build_id: rec.build_id,
        lists: dlc_filter::product_files(depots, &language, &products),
        language,
        found,
        notes,
    })
}

/// Write `scan` into cache root `root`'s record and mark it scanned, while it is
/// still the record it was made from. `Ok(false)`: the record changed, nothing
/// written. Only the record is written, durably; no file of the cache is touched.
pub fn apply_scan(root: &Path, scan: &LanguageScan) -> std::io::Result<bool> {
    depot::record_scanned_languages(
        root,
        &scan.build_id,
        &scan.language,
        &scan.found,
        &scan.lists,
    )
}

/// A build's depots, fetched once for [`bring_up_to_date`].
#[derive(Debug, Clone)]
pub struct Fetched {
    /// The base game's product id.
    pub base: String,
    /// The install dir.
    pub tree_name: String,
    /// Every depot of the build, in every language.
    pub depots: Vec<DepotList>,
}

/// Bring cache root `root`'s record of build `build_id` up to date before a build
/// in `language`, from `fetched` (the build's depots, or why they couldn't be
/// fetched), and say what happened, one line each. An older record (#374) gets
/// [`decide`] and [`apply`] first; then a record whose languages weren't scanned
/// gets [`scan_languages`] and [`apply_scan`] (#391). Best effort: anything that
/// can't be decided or written leaves the record as it is and says why, and the
/// build goes on. Only the record is written.
pub fn bring_up_to_date(
    root: &Path,
    title: &str,
    language: &str,
    build_id: &str,
    fetched: Result<Fetched, String>,
) -> Vec<String> {
    let mut out = Vec::new();
    if !depot::languages_unscanned(root, build_id) {
        return out;
    }
    let old = depot::record_is_old(root, build_id);
    let cant_upgrade = |why: &str| {
        format!(
            "the depot cache's record for {title} predates DLC tracking and can't be \
             brought up to date: {why}"
        )
    };
    let cant_scan = |why: &str| {
        format!(
            "the depot cache for {title} may hold files of a language its record doesn't \
             name (an earlier switch), and they couldn't be looked for: {why}"
        )
    };
    let f = match fetched {
        Ok(f) => f,
        Err(why) => {
            out.push(if old {
                cant_upgrade(&why)
            } else {
                cant_scan(&why)
            });
            return out;
        }
    };
    if old {
        let why = match decide(root, &f.tree_name, &f.base, build_id, &f.depots, language) {
            Ok(up) => match apply(root, &f.base, &up) {
                Ok(true) => {
                    out.push(format!(
                        "the depot cache's record for {title} is brought up to date ({})",
                        up.summary()
                    ));
                    out.extend(up.notes.iter().map(|n| format!("depot cache: {n}")));
                    None
                }
                Ok(false) => Some("the cache changed while it was checked".to_string()),
                Err(e) => Some(format!("the record couldn't be written: {e}")),
            },
            Err(why) => Some(why),
        };
        if let Some(why) = why {
            out.push(cant_upgrade(&why));
            return out;
        }
    }
    let why = match scan_languages(root, &f.base, &f.depots) {
        Ok(scan) => {
            out.extend(scan.notes.iter().map(|n| format!("depot cache: {n}")));
            match apply_scan(root, &scan) {
                Ok(true) => {
                    if !scan.found.is_empty() {
                        out.push(format!(
                            "the depot cache for {title} also holds {} from an earlier \
                             language switch: now in its record, so builds leave those files \
                             out",
                            scan.summary()
                        ));
                    }
                    return out;
                }
                Ok(false) => "the cache changed while it was checked".to_string(),
                Err(e) => format!("the record couldn't be written: {e}"),
            }
        }
        Err(why) => why,
    };
    out.push(cant_scan(&why));
    out
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
            sizes: Default::default(),
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

    /// An older record of a cache that holds de-DE too (#374, #391): one fetch
    /// brings it up to date in en-US and records de-DE, with the DLC held in it.
    #[test]
    fn an_old_record_is_upgraded_then_scanned_from_one_fetch() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        tree(root);
        put(root, "G/vo/de.pak");
        put(root, "G/dlc2/a.pak");
        std::fs::write(
            root.join("G/goggame-2.info"),
            r#"{"gameId":"2","name":"D2"}"#,
        )
        .unwrap();
        old_record(root, "b1");
        assert!(depot::languages_unscanned(root, "b1"));
        let fetched = Fetched {
            base: "1".into(),
            tree_name: "G".into(),
            depots: depots(),
        };
        let lines = bring_up_to_date(root, "T", "en-US", "b1", Ok(fetched.clone()));
        assert!(
            lines.iter().any(|l| l.contains("brought up to date"))
                && lines.iter().any(|l| l.contains("also holds de-DE from")),
            "{lines:?}"
        );
        let rec = depot::read_record(root).unwrap();
        assert_eq!(rec.languages(), ["en-US", "de-DE"]);
        assert_eq!(
            rec.other_languages,
            [("de-DE".to_string(), vec!["2".to_string()])]
        );
        assert!(depot::record_products_in(root, "b1", "de-DE").is_some_and(|l| l.len() == 3));
        assert!(!depot::languages_unscanned(root, "b1"));
        assert!(bring_up_to_date(root, "T", "en-US", "b1", Ok(fetched)).is_empty());
    }

    /// The scan reads the record, so a record with no language can't be scanned;
    /// a language only DLC depots tell apart counts through the record's DLC, and
    /// one no depot tells apart is skipped.
    #[test]
    fn the_scan_needs_depots_that_tell_a_language_apart() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        tree(root);
        old_record(root, "b1");
        assert!(scan_languages(root, "1", &depots()).is_err());
        depot::write_record(root, "b1", "en-US", &["2".to_string()]).unwrap();
        put(root, "G/dlc2/fr.pak");
        let mut d = vec![
            dl("1", &["*"], "m-base", &["G/bin/game.exe"]),
            dl("2", &["en-US"], "m-2en", &["G/dlc2/en.pak"]),
            dl("2", &["fr-FR"], "m-2fr", &["G/dlc2/fr.pak"]),
            dl("1", &["en-US", "it-IT"], "m-both", &["G/vo/en.pak"]),
        ];
        d[2].sizes
            .insert("G/dlc2/fr.pak".into(), "G/dlc2/fr.pak".len() as u64);
        let scan = scan_languages(root, "1", &d).unwrap();
        let langs: Vec<&str> = scan.found.iter().map(|f| f.language.as_str()).collect();
        assert_eq!(langs, ["fr-FR"], "it-IT shares every depot with en-US");
        assert_eq!(scan.found[0].dlcs, ["2"]);
        assert!(scan.found[0].whole);
        d[2].sizes.insert("G/dlc2/fr.pak".into(), 1);
        assert!(scan_languages(root, "1", &d).unwrap().found.is_empty());
    }
}
