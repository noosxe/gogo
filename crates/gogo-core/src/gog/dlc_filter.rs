//! Which files of a depot cache a build takes (design 19 §3.3, #376).
//!
//! A cache can hold more DLC than a build wants (`gogo depot --all-dlcs`, an earlier
//! build with more DLC). Base and DLC files share one tree, so telling them apart
//! takes each product's file list from the build's depot manifests. gog-warp fetches
//! those inside its downloader but doesn't expose their entries, so this module reads
//! the same public manifests itself: the build manifest names each depot's product,
//! languages and manifest hash, and the depot manifest lists its paths.
//!
//! The lists are kept in the cache record ([`super::depot`]'s `products`), so a
//! rebuild from the same cache needs no fetch. Paths are compared lowercased: GOG's
//! manifests are case-insensitive, and gog-warp writes each path as its depot spells
//! it, so the tree on disk keeps the manifest's casing (two depots can spell one dir
//! differently).

use std::collections::{BTreeMap, HashSet};
use std::io::Read as _;
use std::path::Path;

use gog_warp::content_system::types::{Manifest, v1};
use serde::Deserialize;

use super::content::DlcRef;
use super::depot::{self, DepotError};
use crate::macapp::info;

/// Each product's files in a depot cache: product id → paths relative to the cache
/// root, `/`-separated, as the manifest spells them. Support files are under
/// `gog-support/` as gog-warp lays them out.
pub type ProductFiles = BTreeMap<String, Vec<String>>;

/// One entry of a depot manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// `/`-separated, no leading or trailing `/`.
    pub path: String,
    /// GOG's support files go to the support root, not the install dir.
    pub support: bool,
    /// Its size in bytes, when the manifest gives one (a link has none).
    pub size: Option<u64>,
    /// Its MD5 (lowercase hex), when the manifest gives one: a v2 file's own, or
    /// its only chunk's; a v1 file's hash.
    pub md5: Option<String>,
}

#[derive(Deserialize)]
struct V2Details {
    depot: V2Depot,
}

#[derive(Deserialize)]
struct V2Depot {
    #[serde(default)]
    items: Vec<V2Item>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum V2Item {
    #[serde(rename = "DepotFile")]
    File {
        path: String,
        #[serde(default)]
        flags: Vec<String>,
        #[serde(default)]
        chunks: Vec<V2Chunk>,
        #[serde(default, rename = "sfcRef")]
        sfc_ref: Option<V2Chunk>,
        #[serde(default)]
        md5: Option<String>,
    },
    #[serde(rename = "DepotLink")]
    Link { path: String },
    #[serde(other)]
    Other,
}

/// A chunk of a v2 file, or its place in a small-files container: `size` is
/// the uncompressed size.
#[derive(Deserialize)]
struct V2Chunk {
    #[serde(default)]
    size: u64,
    /// The uncompressed chunk's MD5.
    #[serde(default)]
    md5: Option<String>,
}

#[derive(Deserialize)]
struct V1Details {
    depot: V1Depot,
}

#[derive(Deserialize)]
struct V1Depot {
    #[serde(default)]
    files: Vec<V1Item>,
}

#[derive(Deserialize)]
struct V1Item {
    path: String,
    #[serde(default)]
    directory: bool,
    #[serde(default)]
    support: bool,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    hash: Option<String>,
}

/// gog-warp's path normalization: backslashes become `/`, no `/` at either end.
fn norm(path: &str) -> String {
    path.replace('\\', "/").trim_matches('/').to_string()
}

/// A generation 2 depot manifest (the JSON, already inflated): its files and links.
/// Directories and patch entries are left out.
pub fn parse_v2(json: &[u8]) -> Result<Vec<Entry>, String> {
    let d: V2Details = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    Ok(d.depot
        .items
        .into_iter()
        .filter_map(|i| match i {
            V2Item::File {
                path,
                flags,
                chunks,
                sfc_ref,
                md5,
            } => Some(Entry {
                // A file of one chunk is that chunk.
                md5: md5
                    .or_else(|| match chunks.as_slice() {
                        [one] => one.md5.clone(),
                        _ => None,
                    })
                    .map(|m| m.to_lowercase()),
                path: norm(&path),
                support: flags.iter().any(|f| f == "support"),
                // Its chunks add up to it; a file kept only in a small-files
                // container has none, and its place there gives the size.
                size: Some(if chunks.is_empty() {
                    sfc_ref.map_or(0, |c| c.size)
                } else {
                    chunks.iter().map(|c| c.size).sum()
                }),
            }),
            V2Item::Link { path } => Some(Entry {
                path: norm(&path),
                support: false,
                size: None,
                md5: None,
            }),
            V2Item::Other => None,
        })
        .collect())
}

/// A generation 1 depot manifest: its files.
pub fn parse_v1(json: &[u8]) -> Result<Vec<Entry>, String> {
    let d: V1Details = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    Ok(d.depot
        .files
        .into_iter()
        .filter(|f| !f.directory)
        .map(|f| Entry {
            path: norm(&f.path),
            support: f.support,
            size: f.size,
            md5: f.hash.map(|h| h.to_lowercase()),
        })
        .collect())
}

/// Where gog-warp puts `e` of `product`, relative to the cache root: the install
/// dir, or the support root (`gog-support/<base>/`, then `<product>/` on
/// generation 2).
pub fn root_path(install_dir: &str, base: &str, product: &str, v2: bool, e: &Entry) -> String {
    if e.support {
        if v2 {
            format!("gog-support/{base}/{product}/{}", e.path)
        } else {
            format!("gog-support/{base}/{}", e.path)
        }
    } else {
        format!("{}/{}", norm(install_dir), e.path)
    }
}

/// Whether a depot of `langs` is fetched for `language`: it names it (as a v2 code
/// or a v1 name) or `*`.
pub fn language_matches(langs: &[String], language: &str) -> bool {
    let v1 = depot::v1_language(language);
    langs
        .iter()
        .any(|l| l == "*" || l.eq_ignore_ascii_case(language) || l.eq_ignore_ascii_case(&v1))
}

async fn get(http: &reqwest::Client, url: &str) -> Result<Vec<u8>, DepotError> {
    let r = http
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| DepotError::Library(format!("depot manifest: {}", e.without_url())))?;
    Ok(r.bytes()
        .await
        .map_err(|e| DepotError::Library(format!("depot manifest: {}", e.without_url())))?
        .to_vec())
}

/// One depot of a build and its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepotList {
    /// The product it belongs to (the base game or a DLC).
    pub product: String,
    /// As the build manifest names them: v2 codes, v1 names, or `*`.
    pub languages: Vec<String>,
    /// The depot manifest's hash (v2) or name (v1): one can back several depots.
    pub manifest: String,
    /// Relative to the cache root, as gog-warp lays them out ([`root_path`]).
    pub paths: Vec<String>,
    /// The sizes the manifest gives, by path as in `paths`; a path with none (a
    /// link) is left out.
    pub sizes: BTreeMap<String, u64>,
    /// The MD5s the manifest gives, by path as in `paths`.
    pub md5s: BTreeMap<String, String>,
}

/// The `paths`, `sizes` and `md5s` of a [`DepotList`] from manifest entries.
fn laid(entries: &[Entry], at: impl Fn(&Entry) -> String) -> Laid {
    let mut sizes = BTreeMap::new();
    let mut md5s = BTreeMap::new();
    let paths = entries
        .iter()
        .map(|e| {
            let p = at(e);
            if let Some(n) = e.size {
                sizes.insert(p.clone(), n);
            }
            if let Some(m) = &e.md5 {
                md5s.insert(p.clone(), m.clone());
            }
            p
        })
        .collect();
    (paths, sizes, md5s)
}

/// [`laid`]'s answer: paths, sizes and MD5s.
type Laid = (Vec<String>, BTreeMap<String, u64>, BTreeMap<String, String>);

/// Fetch the depots of `manifest`'s build with their files: every depot, or with
/// `only = (products, languages)` those of `products` in one of `languages` (or
/// `*`). A depot manifest backing several depots is fetched once.
pub async fn fetch_depots(
    http: &reqwest::Client,
    manifest: &Manifest,
    only: Option<(&[String], &[String])>,
) -> Result<Vec<DepotList>, DepotError> {
    use gog_warp::constants::domains::GOG_CDN;
    let wanted = |product: &str, langs: &[String]| {
        only.is_none_or(|(products, languages)| {
            products.iter().any(|p| p == product)
                && languages.iter().any(|l| language_matches(langs, l))
        })
    };
    let install_dir = manifest.install_directory();
    let mut out: Vec<DepotList> = Vec::new();
    let mut fetched: std::collections::HashMap<String, Vec<Entry>> =
        std::collections::HashMap::new();
    match manifest {
        Manifest::V2(m) => {
            let base = m.base_product_id();
            for d in m.depots() {
                let product = d.product_id();
                let languages = d.languages().to_vec();
                if !wanted(product, &languages) {
                    continue;
                }
                let hash = d.manifest().to_string();
                if !fetched.contains_key(&hash) {
                    let url = format!(
                        "{GOG_CDN}/content-system/v2/meta/{}",
                        gog_warp::utils::hash_to_galaxy_path(&hash)
                    );
                    let raw = get(http, &url).await?;
                    let mut json = Vec::new();
                    flate2::read::ZlibDecoder::new(&raw[..])
                        .read_to_end(&mut json)
                        .map_err(|e| DepotError::Library(format!("depot manifest: {e}")))?;
                    fetched.insert(hash.clone(), parse_v2(&json).map_err(DepotError::Library)?);
                }
                let (paths, sizes, md5s) = laid(&fetched[&hash], |e| {
                    root_path(&install_dir, base, product, true, e)
                });
                out.push(DepotList {
                    product: product.to_string(),
                    languages,
                    manifest: hash,
                    paths,
                    sizes,
                    md5s,
                });
            }
        }
        Manifest::V1(m) => {
            let p = m.product();
            let base = p.root_game_id();
            for d in p.depots() {
                let v1::ManifestDepot::Files {
                    languages,
                    game_ids,
                    manifest: name,
                    ..
                } = d
                else {
                    continue;
                };
                // gog-warp's rule: a depot naming the root game is the base's.
                let product = if game_ids.contains(base) {
                    Some(base.clone())
                } else {
                    match only {
                        Some((products, _)) => {
                            game_ids.iter().find(|g| products.contains(*g)).cloned()
                        }
                        None => game_ids.first().cloned(),
                    }
                };
                let Some(product) = product else {
                    continue;
                };
                if !wanted(&product, languages) {
                    continue;
                }
                let Some(first) = game_ids.first() else {
                    continue;
                };
                if !fetched.contains_key(name) {
                    let url = format!(
                        "{GOG_CDN}/content-system/v1/manifests/{first}/windows/{}/{name}",
                        p.timestamp()
                    );
                    let entries = parse_v1(&get(http, &url).await?).map_err(DepotError::Library)?;
                    fetched.insert(name.clone(), entries);
                }
                let (paths, sizes, md5s) = laid(&fetched[name], |e| {
                    root_path(&install_dir, base, &product, false, e)
                });
                out.push(DepotList {
                    product,
                    languages: languages.clone(),
                    manifest: name.clone(),
                    paths,
                    sizes,
                    md5s,
                });
            }
        }
    }
    Ok(out)
}

/// Pure: each of `products`' files in `language`, from a build's depots. Every
/// product gets an entry; one with no depots (a zero-byte DLC) gets an empty list.
pub fn product_files(depots: &[DepotList], language: &str, products: &[String]) -> ProductFiles {
    let mut out: ProductFiles = products.iter().map(|p| (p.clone(), Vec::new())).collect();
    for d in depots {
        if let Some(list) = out.get_mut(&d.product)
            && language_matches(&d.languages, language)
        {
            list.extend(d.paths.iter().cloned());
        }
    }
    for list in out.values_mut() {
        list.sort();
        list.dedup();
    }
    out
}

/// Fetch the file lists of `products` (base and DLC ids) in `manifest`'s build, for
/// `language`. Every asked product gets an entry; one with no depots (a zero-byte
/// DLC) gets an empty list.
pub async fn fetch(
    http: &reqwest::Client,
    manifest: &Manifest,
    language: &str,
    products: &[String],
) -> Result<ProductFiles, DepotError> {
    let depots = fetch_depots(http, manifest, Some((products, &[language.to_string()]))).await?;
    Ok(product_files(&depots, language, products))
}

/// Pure: a path that `language`'s depots share with a depot `against` picks, through
/// another depot manifest, when there is one. The cache holds one copy of such a
/// path, the last fetched one's, so which language's bytes are there can't be told
/// without hashing. Paths are compared lowercased.
pub fn clash<'a>(
    depots: &'a [DepotList],
    language: &str,
    against: impl Fn(&DepotList) -> bool,
) -> Option<(&'a DepotList, &'a str)> {
    let ours = |d: &DepotList| language_matches(&d.languages, language);
    let mut held: std::collections::HashMap<String, HashSet<&str>> =
        std::collections::HashMap::new();
    for d in depots.iter().filter(|d| ours(d)) {
        for p in &d.paths {
            held.entry(p.to_lowercase())
                .or_default()
                .insert(d.manifest.as_str());
        }
    }
    depots
        .iter()
        .filter(|d| !ours(d) && against(d))
        .find_map(|d| {
            d.paths
                .iter()
                .find(|p| {
                    held.get(&p.to_lowercase())
                        .is_some_and(|m| !m.contains(d.manifest.as_str()))
                })
                .map(|p| (d, p.as_str()))
        })
}

/// What a build of a cache's tree has to leave out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeFilter {
    /// Lowercased paths relative to the cache root.
    drop: HashSet<String>,
    /// The DLC left out: their `goggame-<id>.*` files are left out by name too.
    pub dropped: Vec<DlcRef>,
    /// Paths a dropped DLC shares with a kept product: kept as the cache has them.
    pub shared: usize,
    /// The cache's other languages, left out (#389).
    pub languages: Vec<String>,
    /// Their paths the filter drops, as their lists spell them.
    language_paths: Vec<String>,
    /// Their paths a kept depot lists too: kept as the cache has them.
    pub language_shared: usize,
}

impl TreeFilter {
    /// Whether the build takes `rel` (relative to the cache root).
    pub fn keeps(&self, rel: &str) -> bool {
        let lower = rel.to_lowercase();
        if self.drop.contains(&lower) {
            return false;
        }
        let name = lower.rsplit('/').next().unwrap_or(&lower);
        !self
            .dropped
            .iter()
            .any(|d| name.starts_with(&format!("goggame-{}.", d.id)))
    }

    /// [`keeps`](Self::keeps) for a path given relative to the cache root as a
    /// [`Path`].
    pub fn keeps_path(&self, rel: &Path) -> bool {
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        self.keeps(&parts.join("/"))
    }
}

/// Pure: the filter that leaves out `dropping`'s files, except the ones a kept
/// product (`keeping`: the base and the wanted DLC) lists too. `lists` are the
/// build's language's. `Err` names the products `lists` has no entry for.
pub fn filter(
    lists: &ProductFiles,
    keeping: &[String],
    dropping: &[DlcRef],
) -> Result<TreeFilter, Vec<String>> {
    filter_languages(lists, keeping, dropping, &[])
}

/// Pure: [`filter`] that also leaves out the files of the cache's other languages
/// (#389): every path `others` (language → its products' lists) names that a kept
/// product doesn't list in the build's language. A path both list is kept as the
/// cache has it, and so is one no list names. `Err` names what has no list: a
/// product, or `<product> (<language>)` for another language's.
pub fn filter_languages(
    lists: &ProductFiles,
    keeping: &[String],
    dropping: &[DlcRef],
    others: &[(String, ProductFiles)],
) -> Result<TreeFilter, Vec<String>> {
    let products: Vec<String> = keeping
        .iter()
        .cloned()
        .chain(dropping.iter().map(|d| d.id.to_string()))
        .collect();
    let mut lacking: Vec<String> = products
        .iter()
        .filter(|p| !lists.contains_key(*p))
        .cloned()
        .collect();
    for (language, l) in others {
        lacking.extend(
            products
                .iter()
                .filter(|p| !l.contains_key(*p))
                .map(|p| format!("{p} ({language})")),
        );
    }
    if !lacking.is_empty() {
        return Err(lacking);
    }
    let kept: HashSet<String> = keeping
        .iter()
        .flat_map(|p| lists[p].iter().map(|f| f.to_lowercase()))
        .collect();
    let mut shared = 0;
    let mut drop = HashSet::new();
    for d in dropping {
        for f in &lists[&d.id.to_string()] {
            let lower = f.to_lowercase();
            if kept.contains(&lower) {
                shared += 1;
            } else {
                drop.insert(lower);
            }
        }
    }
    let mut language_paths = Vec::new();
    let mut language_shared = HashSet::new();
    for (_, l) in others {
        for f in products.iter().flat_map(|p| &l[p]) {
            let lower = f.to_lowercase();
            if kept.contains(&lower) {
                language_shared.insert(lower);
            } else if drop.insert(lower) {
                language_paths.push(f.clone());
            }
        }
    }
    Ok(TreeFilter {
        drop,
        dropped: dropping.to_vec(),
        shared,
        languages: others.iter().map(|(l, _)| l.clone()).collect(),
        language_paths,
        language_shared: language_shared.len(),
    })
}

impl TreeFilter {
    /// `(files, bytes)` of the cache's other languages under cache root `root` that
    /// the build leaves out: what the plan says (#389).
    pub fn languages_left_out(&self, root: &Path) -> (u64, u64) {
        let mut disk = super::cache_upgrade::OnDisk::default();
        self.language_paths
            .iter()
            .filter_map(|p| disk.find(root, p))
            .filter_map(|p| root.join(p).symlink_metadata().ok())
            .fold((0, 0), |(n, b), m| (n + 1, b + m.len()))
    }
}

/// The DLC a depot build takes from its cache tree.
#[derive(Debug, Clone, Default)]
pub struct DepotContent {
    /// Installed and recorded in `content.toml`, nothing else.
    pub dlcs: Vec<DlcRef>,
    /// The build's language, recorded in `content.toml` (#389): the cache record's
    /// names only the last one fetched. `None`: the record's.
    pub language: Option<String>,
    /// The build's id and version name (`2.31`) from GOG's builds list, for
    /// `content.toml`'s `version` (#418). Used only when the id is the build the
    /// tree holds. `None`: the list didn't answer or the build isn't in it.
    pub version: Option<(String, String)>,
    /// The build's id and branch, when the build isn't on the default branch
    /// (design 21 §5.1); used only when the id is the build the tree holds.
    pub branch: Option<(String, String)>,
    /// The plan's lines about the build (design 21 §3.4); the first is `build:`.
    pub build_lines: Vec<String>,
    /// The build's id, generation and manifest, for the game dir's
    /// `build-manifest.json` (design 21 §5.2); written only when the id is the
    /// build the tree holds. `None`: GOG didn't answer or doesn't list the build.
    pub manifest: Option<(String, u32, gog_warp::content_system::types::Manifest)>,
    /// Leaves out the cache's other DLC; `None` when it holds no other.
    pub filter: Option<TreeFilter>,
}

impl DepotContent {
    /// Whether the build takes `rel`, a path relative to the cache root.
    pub fn keeps(&self, rel: &Path) -> bool {
        self.filter.as_ref().is_none_or(|f| f.keeps_path(rel))
    }
}

/// What a build has to do about the DLC and languages a cache holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Need {
    /// The cache holds no DLC or language the build doesn't take: it's materialized
    /// whole.
    Whole,
    /// It holds `extra` DLC or `languages` too: they're left out with the file lists
    /// of `products` (the base, the wanted DLC and `extra`) of build `build_id`, in
    /// `language` (the build's) and in each of `languages`.
    Filter {
        extra: Vec<DlcRef>,
        products: Vec<String>,
        build_id: Option<String>,
        language: String,
        /// The cache's other languages (#389).
        languages: Vec<String>,
    },
}

/// Pure (reads the cache): what building `tree` (in cache root `root`) for base
/// `base` with DLC `wanted` in `language` needs. The cache's DLC are those its
/// record lists in any language it holds and those with a `goggame-<id>.info` in
/// the tree (an older record lists none). Its languages are the record's: an older
/// one names one or none, and is taken to hold only that.
pub fn need(root: &Path, tree: &Path, base: &str, wanted: &[u64], language: &str) -> Need {
    let rec = depot::read_record(root);
    let infos = info::read_infos(tree);
    let mut held: Vec<String> = rec.as_ref().map(|r| r.all_dlcs()).unwrap_or_default();
    held.extend(
        infos
            .iter()
            .map(|(id, _)| id.clone())
            .filter(|id| id != base),
    );
    held.sort();
    held.dedup();
    let extra: Vec<DlcRef> = held
        .iter()
        .filter_map(|id| id.parse::<u64>().ok())
        .filter(|id| !wanted.contains(id))
        .map(|id| DlcRef {
            id,
            title: infos
                .iter()
                .find(|(i, _)| *i == id.to_string())
                .and_then(|(_, i)| i.name.clone())
                .unwrap_or_else(|| id.to_string()),
        })
        .collect();
    let languages: Vec<String> = rec
        .as_ref()
        .map(|r| r.languages())
        .unwrap_or_default()
        .into_iter()
        .filter(|l| !l.eq_ignore_ascii_case(language))
        .collect();
    if extra.is_empty() && languages.is_empty() {
        return Need::Whole;
    }
    let mut products = vec![base.to_string()];
    products.extend(wanted.iter().map(u64::to_string));
    products.extend(extra.iter().map(|d| d.id.to_string()));
    products.dedup();
    let info_build = infos
        .iter()
        .find(|(id, _)| id == base)
        .and_then(|(_, i)| i.build_id_text());
    Need::Filter {
        extra,
        products,
        build_id: rec.as_ref().map(|r| r.build_id.clone()).or(info_build),
        language: language.to_string(),
        languages,
    }
}

/// `REDmod (1597316373), …`.
pub fn names(dlcs: &[DlcRef]) -> String {
    dlcs.iter()
        .map(|d| format!("{} ({})", d.title, d.id))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(id: u64, t: &str) -> DlcRef {
        DlcRef {
            id,
            title: t.into(),
        }
    }

    #[test]
    fn v2_lists_files_and_links_with_support_flags() {
        let json = br#"{"depot":{"items":[
            {"type":"DepotFile","path":"bin\\x64\\Game.exe","chunks":[{"size":5,"compressedSize":3},{"size":2}],"flags":["executable"]},
            {"type":"DepotDirectory","path":"tools\\redmod"},
            {"type":"DepotFile","path":"app/goggame-2.script","chunks":[],"flags":["support"],"sfcRef":{"offset":10,"size":4}},
            {"type":"DepotLink","path":"link","target":"bin"},
            {"type":"DepotDiff","path_source":"a","path_target":"b"}
        ],"smallFilesContainer":null}}"#;
        let e = parse_v2(json).unwrap();
        assert_eq!(
            e,
            [
                Entry {
                    path: "bin/x64/Game.exe".into(),
                    support: false,
                    size: Some(7),
                    md5: None,
                },
                Entry {
                    path: "app/goggame-2.script".into(),
                    support: true,
                    size: Some(4),
                    md5: None,
                },
                Entry {
                    path: "link".into(),
                    support: false,
                    size: None,
                    md5: None,
                },
            ]
        );
        assert_eq!(
            root_path("Game", "1", "2", true, &e[1]),
            "gog-support/1/2/app/goggame-2.script"
        );
        assert_eq!(
            root_path("Game", "1", "2", true, &e[0]),
            "Game/bin/x64/Game.exe"
        );
    }

    /// A file's MD5 comes from its own `md5`, else from its only chunk's, lowercased.
    #[test]
    fn v2_file_md5_is_its_own_or_its_single_chunks() {
        let json = br#"{"depot":{"items":[
            {"type":"DepotFile","path":"a","md5":"ABC","chunks":[{"size":1,"md5":"x"},{"size":1,"md5":"y"}]},
            {"type":"DepotFile","path":"b","chunks":[{"size":1,"md5":"DEF"}]},
            {"type":"DepotFile","path":"c","chunks":[{"size":1,"md5":"x"},{"size":1,"md5":"y"}]}
        ]}}"#;
        let md5: Vec<_> = parse_v2(json).unwrap().into_iter().map(|e| e.md5).collect();
        assert_eq!(md5, [Some("abc".into()), Some("def".into()), None]);
    }

    #[test]
    fn v1_lists_files_not_directories() {
        let json = br#"{"depot":{"name":"x","files":[
            {"path":"/data/a.pak","size":3,"hash":"h"},
            {"directory":true,"path":"/data"},
            {"path":"/support.ico","size":1,"support":true}
        ]}}"#;
        let e = parse_v1(json).unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].path, "data/a.pak");
        assert_eq!(e[0].size, Some(3));
        assert_eq!(
            root_path("Game", "1", "2", false, &e[1]),
            "gog-support/1/support.ico"
        );
    }

    fn lists() -> ProductFiles {
        let mut l = ProductFiles::new();
        l.insert(
            "1".into(),
            vec!["Game/bin/game.exe".into(), "Game/shared.dat".into()],
        );
        l.insert(
            "2".into(),
            vec!["Game/DLC/A/a.pak".into(), "Game/goggame-2.info".into()],
        );
        l.insert(
            "3".into(),
            vec![
                "Game/tools/redmod/r.exe".into(),
                "Game/Shared.DAT".into(),
                "gog-support/1/3/app/goggame-3.script".into(),
            ],
        );
        l
    }

    #[test]
    fn the_filter_drops_only_the_unwanted_dlc_and_keeps_shared_paths() {
        let f = filter(&lists(), &["1".into(), "2".into()], &[r(3, "REDmod")]).unwrap();
        assert!(f.keeps("Game/bin/game.exe"));
        assert!(f.keeps("Game/DLC/A/a.pak"));
        // The disk keeps another casing than the manifest.
        assert!(!f.keeps("Game/Tools/REDmod/r.exe"));
        assert!(!f.keeps("gog-support/1/3/app/goggame-3.script"));
        // Shared with the base: kept.
        assert!(f.keeps("Game/shared.dat"));
        assert_eq!(f.shared, 1);
        // A support file the lists don't name still goes by its name.
        assert!(!f.keeps("Game/goggame-3.info"));
        assert!(f.keeps_path(Path::new("Game/goggame-2.info")));
    }

    /// The other languages' files are left out (#389), except the paths a kept
    /// product lists too, and a missing list is named with its language.
    #[test]
    fn the_filter_leaves_out_other_languages() {
        let en: ProductFiles = [(
            "1".to_string(),
            vec![
                "Game/bin/game.exe".to_string(),
                "Game/lang/en.pak".into(),
                "Game/lang/common.pak".into(),
            ],
        )]
        .into();
        let de: ProductFiles = [(
            "1".to_string(),
            vec![
                "Game/bin/game.exe".to_string(),
                "Game/lang/de.pak".into(),
                "Game/Lang/Common.pak".into(),
            ],
        )]
        .into();
        let f = filter_languages(&en, &["1".into()], &[], &[("de-DE".into(), de.clone())]).unwrap();
        assert!(f.keeps("Game/lang/en.pak"));
        assert!(!f.keeps("Game/lang/de.pak"));
        assert!(f.keeps("Game/lang/common.pak"), "shared: kept");
        assert!(f.keeps("Game/unlisted.txt"), "no list names it: kept");
        assert_eq!(f.languages, ["de-DE"]);
        assert_eq!(f.language_shared, 2);

        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("Game/LANG")).unwrap();
        std::fs::write(d.path().join("Game/LANG/de.pak"), b"1234").unwrap();
        assert_eq!(f.languages_left_out(d.path()), (1, 4));

        let lacking = filter_languages(&en, &["1".into()], &[r(2, "A")], &[("de-DE".into(), de)])
            .unwrap_err();
        assert_eq!(lacking, ["2", "2 (de-DE)"]);
    }

    #[test]
    fn a_language_clash_is_a_shared_path_through_another_manifest() {
        let d = |l: &str, m: &str, p: &str| DepotList {
            product: "1".into(),
            languages: vec![l.into()],
            manifest: m.into(),
            paths: vec![p.into()],
            sizes: BTreeMap::new(),
            md5s: BTreeMap::new(),
        };
        let one = [d("en-US", "x", "G/a"), d("de-DE", "x", "G/A")];
        assert!(clash(&one, "en-US", |_| true).is_none(), "one manifest");
        let two = [d("en-US", "x", "G/a"), d("de-DE", "y", "G/A")];
        assert_eq!(clash(&two, "en-US", |_| true).map(|(_, p)| p), Some("G/A"));
        assert!(
            clash(&two, "en-US", |d| language_matches(&d.languages, "fr-FR")).is_none(),
            "only the depots asked about"
        );
    }

    #[test]
    fn a_product_without_a_list_is_named() {
        let mut l = lists();
        l.remove("3");
        assert_eq!(
            filter(&l, &["1".into()], &[r(2, "A"), r(3, "B")]).unwrap_err(),
            ["3"]
        );
    }

    #[test]
    fn need_reads_the_record_and_the_infos() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        let tree = root.join("Game");
        std::fs::create_dir_all(&tree).unwrap();
        let info = |id: &str, name: &str| {
            std::fs::write(
                tree.join(format!("goggame-{id}.info")),
                format!(r#"{{"gameId":"{id}","rootGameId":"1","name":"{name}","buildId":"77"}}"#),
            )
            .unwrap();
        };
        info("1", "Game");
        assert_eq!(need(root, &tree, "1", &[], "en-US"), Need::Whole);
        info("3", "REDmod");
        // No record: the infos say what's there.
        match need(root, &tree, "1", &[], "en-US") {
            Need::Filter {
                extra, build_id, ..
            } => {
                assert_eq!(extra, [r(3, "REDmod")]);
                assert_eq!(build_id.as_deref(), Some("77"));
            }
            n => panic!("{n:?}"),
        }
        assert_eq!(need(root, &tree, "1", &[3], "en-US"), Need::Whole);
        // The record lists one with no info (yet): it counts.
        depot::write_record(root, "88", "en-US", &["2".into(), "3".into()]).unwrap();
        match need(root, &tree, "1", &[3], "en-US") {
            Need::Filter {
                extra,
                products,
                build_id,
                language,
                languages,
            } => {
                assert_eq!(extra, [r(2, "2")]);
                assert_eq!(products, ["1", "3", "2"]);
                assert_eq!(build_id.as_deref(), Some("88"));
                assert_eq!(language, "en-US");
                assert!(languages.is_empty());
            }
            n => panic!("{n:?}"),
        }
    }
}
