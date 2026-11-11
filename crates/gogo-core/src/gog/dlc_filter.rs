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
    },
    #[serde(rename = "DepotLink")]
    Link { path: String },
    #[serde(other)]
    Other,
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
            V2Item::File { path, flags } => Some(Entry {
                path: norm(&path),
                support: flags.iter().any(|f| f == "support"),
            }),
            V2Item::Link { path } => Some(Entry {
                path: norm(&path),
                support: false,
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

fn language_matches(langs: &[String], language: &str) -> bool {
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

/// Fetch the file lists of `products` (base and DLC ids) in `manifest`'s build, for
/// `language`. Every asked product gets an entry; one with no depots (a zero-byte
/// DLC) gets an empty list.
pub async fn fetch(
    http: &reqwest::Client,
    manifest: &Manifest,
    language: &str,
    products: &[String],
) -> Result<ProductFiles, DepotError> {
    use gog_warp::constants::domains::GOG_CDN;
    let mut out: ProductFiles = products.iter().map(|p| (p.clone(), Vec::new())).collect();
    let install_dir = manifest.install_directory();
    // One depot manifest can back several depots (a DLC's per-language info).
    let mut seen: HashSet<String> = HashSet::new();
    match manifest {
        Manifest::V2(m) => {
            let base = m.base_product_id();
            for d in m.depots() {
                let product = d.product_id();
                if !out.contains_key(product)
                    || !language_matches(d.languages(), language)
                    || !seen.insert(format!("{product}/{}", d.manifest()))
                {
                    continue;
                }
                let url = format!(
                    "{GOG_CDN}/content-system/v2/meta/{}",
                    gog_warp::utils::hash_to_galaxy_path(d.manifest())
                );
                let raw = get(http, &url).await?;
                let mut json = Vec::new();
                flate2::read::ZlibDecoder::new(&raw[..])
                    .read_to_end(&mut json)
                    .map_err(|e| DepotError::Library(format!("depot manifest: {e}")))?;
                let entries = parse_v2(&json).map_err(DepotError::Library)?;
                let list = out.get_mut(product).expect("checked");
                list.extend(
                    entries
                        .iter()
                        .map(|e| root_path(&install_dir, base, product, true, e)),
                );
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
                    game_ids.iter().find(|g| out.contains_key(*g)).cloned()
                };
                let Some(product) = product.filter(|p| out.contains_key(p)) else {
                    continue;
                };
                if !language_matches(languages, language)
                    || !seen.insert(format!("{product}/{name}"))
                {
                    continue;
                }
                let Some(first) = game_ids.first() else {
                    continue;
                };
                let url = format!(
                    "{GOG_CDN}/content-system/v1/manifests/{first}/windows/{}/{name}",
                    p.timestamp()
                );
                let entries = parse_v1(&get(http, &url).await?).map_err(DepotError::Library)?;
                let list = out.get_mut(&product).expect("checked");
                list.extend(
                    entries
                        .iter()
                        .map(|e| root_path(&install_dir, base, &product, false, e)),
                );
            }
        }
    }
    for list in out.values_mut() {
        list.sort();
        list.dedup();
    }
    Ok(out)
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
/// product (`keeping`: the base and the wanted DLC) lists too. `Err` names the
/// products `lists` has no entry for.
pub fn filter(
    lists: &ProductFiles,
    keeping: &[String],
    dropping: &[DlcRef],
) -> Result<TreeFilter, Vec<String>> {
    let lacking: Vec<String> = keeping
        .iter()
        .cloned()
        .chain(dropping.iter().map(|d| d.id.to_string()))
        .filter(|p| !lists.contains_key(p))
        .collect();
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
    Ok(TreeFilter {
        drop,
        dropped: dropping.to_vec(),
        shared,
    })
}

/// The DLC a depot build takes from its cache tree.
#[derive(Debug, Clone, Default)]
pub struct DepotContent {
    /// Installed and recorded in `content.toml`, nothing else.
    pub dlcs: Vec<DlcRef>,
    /// Leaves out the cache's other DLC; `None` when it holds no other.
    pub filter: Option<TreeFilter>,
}

impl DepotContent {
    /// Whether the build takes `rel`, a path relative to the cache root.
    pub fn keeps(&self, rel: &Path) -> bool {
        self.filter.as_ref().is_none_or(|f| f.keeps_path(rel))
    }
}

/// What a build has to do about the DLC a cache holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Need {
    /// The cache holds no DLC the build doesn't take: it's materialized whole.
    Whole,
    /// It holds `extra` too: they're left out with the file lists of `products` (the
    /// base, the wanted DLC and `extra`) of build `build_id` in `language`.
    Filter {
        extra: Vec<DlcRef>,
        products: Vec<String>,
        build_id: Option<String>,
        language: Option<String>,
    },
}

/// Pure (reads the cache): what building `tree` (in cache root `root`) for base
/// `base` with DLC `wanted` needs. The cache's DLC are its record's and those with a
/// `goggame-<id>.info` in the tree (an older record lists none).
pub fn need(root: &Path, tree: &Path, base: &str, wanted: &[u64]) -> Need {
    let rec = depot::read_record(root);
    let infos = info::read_infos(tree);
    let mut held: Vec<String> = rec
        .as_ref()
        .and_then(|r| r.dlcs.clone())
        .unwrap_or_default();
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
    if extra.is_empty() {
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
        language: rec.and_then(|r| r.language),
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
            {"type":"DepotFile","path":"bin\\x64\\Game.exe","chunks":[],"flags":["executable"]},
            {"type":"DepotDirectory","path":"tools\\redmod"},
            {"type":"DepotFile","path":"app/goggame-2.script","chunks":[],"flags":["support"]},
            {"type":"DepotLink","path":"link","target":"bin"},
            {"type":"DepotDiff","path_source":"a","path_target":"b"}
        ],"smallFilesContainer":null}}"#;
        let e = parse_v2(json).unwrap();
        assert_eq!(
            e,
            [
                Entry {
                    path: "bin/x64/Game.exe".into(),
                    support: false
                },
                Entry {
                    path: "app/goggame-2.script".into(),
                    support: true
                },
                Entry {
                    path: "link".into(),
                    support: false
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
        assert_eq!(need(root, &tree, "1", &[]), Need::Whole);
        info("3", "REDmod");
        // No record: the infos say what's there.
        match need(root, &tree, "1", &[]) {
            Need::Filter {
                extra, build_id, ..
            } => {
                assert_eq!(extra, [r(3, "REDmod")]);
                assert_eq!(build_id.as_deref(), Some("77"));
            }
            n => panic!("{n:?}"),
        }
        assert_eq!(need(root, &tree, "1", &[3]), Need::Whole);
        // The record lists one with no info (yet): it counts.
        depot::write_record(root, "88", "en-US", &["2".into(), "3".into()]).unwrap();
        match need(root, &tree, "1", &[3]) {
            Need::Filter {
                extra,
                products,
                build_id,
                language,
            } => {
                assert_eq!(extra, [r(2, "2")]);
                assert_eq!(products, ["1", "3", "2"]);
                assert_eq!(build_id.as_deref(), Some("88"));
                assert_eq!(language.as_deref(), Some("en-US"));
            }
            n => panic!("{n:?}"),
        }
    }
}
