//! A build's game files for a frontend that plans before it writes (design 25
//! §4): which source it starts from, what its download would take, and what of
//! the depot cache the build installs.
//!
//! The defaults are `linux-app`'s (design 18 §10, design 19), except for DLC: a
//! frontend with no way to pick takes **every owned DLC** of the build, and a
//! rebuild keeps what the game recorded. Neither function writes anything.

use std::path::{Path, PathBuf};

use crate::gog::catalog::Product;
use crate::gog::content::{self, DlcRef};
use crate::gog::depot::{self, CacheCheck, CacheWant, DepotError, DepotOptions, DlcContext};
use crate::gog::dlc::{self, Basis, DlcSet, Want};
use crate::gog::dlc_filter::{self, DepotContent, Need};
use crate::gog::{lane, language};
use crate::macapp::error::MacAppError;
use crate::macapp::launch::GameDirs;
use gog_warp::Core;

/// What a build is asked for.
#[derive(Debug, Clone)]
pub struct Request {
    pub product_id: u64,
    pub title: String,
    /// `--lang`.
    pub lang: Option<String>,
    /// `--build`.
    pub build: Option<String>,
    /// `--dlc`; `None`: every owned DLC, or on a rebuild the recorded ones.
    pub dlc: Option<Want>,
    /// The owned product ids.
    pub owned: Vec<u64>,
    /// The game's DLC as its catalog page lists them.
    pub catalog_dlcs: Vec<Product>,
    /// Its downloaded offline installer, for a game with no depot build
    /// ([`lane::downloaded_installer`]).
    pub installer: Option<PathBuf>,
}

/// Where a build's game files come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Files {
    /// The depot cache holds them: this tree.
    Cached(PathBuf),
    /// The depot download fetches them first; `repair`: into a cache that is
    /// there but not known to be whole.
    Fetch { repair: bool },
    /// A downloaded offline installer.
    Installer(PathBuf),
}

/// A build's source.
#[derive(Clone)]
pub struct Source {
    pub files: Files,
    /// The depot cache root.
    pub root: PathBuf,
    /// What the depot download is run with.
    pub options: DepotOptions,
    /// The build's id and, where known, version name; `None` from an installer.
    pub build: Option<(String, Option<String>)>,
    /// Bytes to download and bytes on disk for the whole build.
    pub sizes: Option<(u64, u64)>,
    /// The DLC the build takes.
    pub dlcs: Vec<DlcRef>,
    /// Owned DLC a build from an installer can't take here.
    pub dlcs_not_taken: Vec<DlcRef>,
}

impl std::fmt::Debug for Source {
    /// Leaves out the options: they hold the account's catalog.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Source")
            .field("files", &self.files)
            .field("build", &self.build)
            .field("language", &self.options.language)
            .field("sizes", &self.sizes)
            .field("dlcs", &self.dlcs)
            .field("dlcs_not_taken", &self.dlcs_not_taken)
            .finish_non_exhaustive()
    }
}

/// The DLC `ids` with their titles.
fn refs(ids: &[u64], infos: &[dlc::DlcInfo]) -> Vec<DlcRef> {
    ids.iter()
        .map(|id| DlcRef {
            id: *id,
            title: infos
                .iter()
                .find(|d| d.id == *id)
                .map_or_else(|| id.to_string(), |d| d.title.clone()),
        })
        .collect()
}

/// What a built game records: its DLC and language.
fn recorded(dirs: &GameDirs) -> Option<content::Content> {
    if !dirs.game.exists() {
        return None;
    }
    content::resolve(&dirs.root, &dirs.game, content::Lane::Depot, None)
        .ok()
        .map(|r| r.content)
}

/// The language a build takes: `asked`, else the game's, else the cache's, else
/// the default (design 19 §1.2, #389).
fn pick_language(asked: Option<&str>, game: Option<&str>, cache: Option<&str>) -> String {
    match asked {
        // Asking for what the game has keeps the recorded code.
        Some(a) => game
            .filter(|g| language::same_language(a, g))
            .unwrap_or(a)
            .to_string(),
        None => game.or(cache).unwrap_or(language::DEFAULT).to_string(),
    }
}

/// Which DLC the build wants (the module docs).
fn want(asked: Option<&Want>, recorded: Option<&[u64]>) -> Want {
    match (asked, recorded) {
        (Some(w), _) => w.clone(),
        (None, Some(_)) => Want::Default,
        (None, None) => Want::All,
    }
}

/// Resolve `request` against GOG's builds and what this machine has.
pub async fn resolve(core: &Core, request: &Request) -> Result<Source, DepotError> {
    let id = request.product_id;
    let io = |e: MacAppError| DepotError::Library(e.to_string());
    let dirs = GameDirs::for_game(&id.to_string()).map_err(io)?;
    let prior = recorded(&dirs);
    let kept: Option<Vec<u64>> = prior.as_ref().map(|c| c.dlc.iter().map(|d| d.id).collect());
    let (root, ..) = depot::cache_roots(depot::default_cache_dir().as_deref(), id, None)
        .ok_or(DepotError::NoCacheDir)?;
    let record = depot::read_record(&root);
    let game_language = prior.as_ref().and_then(|c| c.language.as_deref());
    let language = pick_language(
        request.lang.as_deref(),
        game_language,
        record.as_ref().and_then(|r| r.language.as_deref()),
    );
    let explicit = request.lang.is_some() || (request.build.is_some() && game_language.is_some());
    let mut options = DepotOptions {
        language: language.clone(),
        language_explicit: explicit,
        build: request.build.clone(),
        ..DepotOptions::default()
    };
    let want = want(request.dlc.as_ref(), kept.as_deref());

    let chosen =
        depot::chosen_build(core, id, &request.title, None, request.build.as_deref()).await;
    let chosen = match chosen {
        Ok(chosen) => chosen,
        Err(DepotError::NoBuilds(_)) if request.build.is_none() => {
            let Some(installer) = request.installer.clone() else {
                return Err(DepotError::NoBuilds(id));
            };
            // The installer lane runs no DLC installers from here.
            let infos = dlc::available(&request.catalog_dlcs, &request.owned, None);
            let owned: Vec<u64> = infos.iter().filter(|d| d.owned).map(|d| d.id).collect();
            return Ok(Source {
                files: Files::Installer(installer),
                root,
                options,
                build: None,
                sizes: None,
                dlcs: Vec::new(),
                dlcs_not_taken: refs(&owned, &infos),
            });
        }
        Err(e) => return Err(e),
    };
    let language = depot::pick_language(&chosen.manifest, &language, explicit)?;
    let manifest_dlcs = dlc::manifest_dlcs(&chosen.manifest, &language);
    let infos = dlc::available(&request.catalog_dlcs, &request.owned, Some(&manifest_dlcs));
    let label = chosen.build_id.clone();
    // A rebuild never drops a recorded DLC the build lacks (design 21 D8).
    if request.dlc.is_none()
        && let (Some(kept), Some(prior)) = (&kept, &prior)
    {
        let titles: Vec<(u64, String)> =
            prior.dlc.iter().map(|d| (d.id, d.title.clone())).collect();
        dlc::check_kept(kept, &titles, &infos, &label)?;
    }
    let set = match dlc::decide(&want, kept.as_deref(), &infos, false).basis {
        Basis::Set(set) => set,
        Basis::Pick(candidates) => DlcSet::Ids(candidates),
    };
    let lane_of = dlc::Lane::Depot { build_id: label };
    let ids = dlc::select(&set, &request.title, &infos, &lane_of)?;
    let wanted: Vec<String> = ids.iter().map(u64::to_string).collect();
    options.dlcs = if ids.is_empty() {
        DlcSet::None
    } else {
        DlcSet::Ids(ids.clone())
    };
    options.dlc_context = DlcContext {
        owned: request.owned.clone(),
        catalog_dlcs: request.catalog_dlcs.clone(),
    };

    let files = match lane::find_game_tree(&root) {
        Some(tree) => {
            let gap = depot::check_cache(
                &root,
                &CacheWant {
                    build_id: request.build.as_ref().map(|_| chosen.build_id.as_str()),
                    language: (explicit || record.as_ref().is_some_and(|r| r.language.is_some()))
                        .then_some(options.language.as_str()),
                    dlcs: &wanted,
                },
            );
            if gap == CacheCheck::Complete {
                Files::Cached(tree)
            } else {
                Files::Fetch { repair: true }
            }
        }
        None => Files::Fetch { repair: false },
    };
    // A whole cache is built from as it is, whatever GOG's newest build is.
    let build = match (&files, record) {
        (Files::Cached(_), Some(r)) => (r.build_id, r.version),
        _ => (chosen.build_id, Some(chosen.version)),
    };
    let (download, disk) = chosen.manifest.install_size(&language, &wanted, None);
    Ok(Source {
        files,
        root,
        options,
        build: Some(build),
        sizes: Some((download, disk)),
        dlcs: refs(&ids, &infos),
        dlcs_not_taken: Vec::new(),
    })
}

/// What a build takes of the cached tree `tree` (design 19 §3.3), once the
/// download is there: the DLC, and when the cache holds others or other
/// languages, the filter that leaves them out. A build that can't tell their
/// files apart is refused.
pub async fn content(
    core: &Core,
    source: &Source,
    product_id: u64,
    tree: &Path,
) -> Result<DepotContent, DepotError> {
    let root = &source.root;
    let language = &source.options.language;
    let wanted: Vec<u64> = source.dlcs.iter().map(|d| d.id).collect();
    // The build the cache holds, for the records (#418); best effort.
    let (info, manifest) = match depot::read_record(root) {
        Some(r) => match depot::build_info_with_manifest(core, product_id, None, &r.build_id).await
        {
            Some((entry, manifest)) => (Some((r.build_id, entry)), manifest),
            None => (None, None),
        },
        None => (None, None),
    };
    let version = info
        .as_ref()
        .filter(|(_, e)| !e.version.trim().is_empty())
        .map(|(id, e)| (id.clone(), e.version.clone()));
    let branch = info.and_then(|(id, e)| e.branch.map(|b| (id, b)));
    let mut content = DepotContent {
        dlcs: source.dlcs.clone(),
        language: Some(language.clone()),
        version,
        branch,
        build_lines: Vec::new(),
        manifest,
        filter: None,
    };
    let Need::Filter {
        extra,
        products,
        build_id,
        languages,
        ..
    } = dlc_filter::need(root, tree, &product_id.to_string(), &wanted, language)
    else {
        return Ok(content);
    };
    let refused = |why: String| {
        DepotError::Filter(format!(
            "the depot cache also holds DLC or languages the build doesn't take, and \
             their files can't be told apart from the rest: {why}"
        ))
    };
    let Some(build_id) = build_id else {
        return Err(refused("nothing names its build".into()));
    };
    let lists = |l: String| {
        let (build_id, products) = (&build_id, &products);
        async move {
            depot::product_lists(core, product_id, root, build_id, &l, products)
                .await
                .map_err(|e| refused(e.to_string()))
        }
    };
    let own = lists(language.clone()).await?;
    let mut others = Vec::new();
    for l in &languages {
        others.push((l.clone(), lists(l.clone()).await?));
    }
    let keeping: Vec<String> = products
        .iter()
        .filter(|p| !extra.iter().any(|d| d.id.to_string() == **p))
        .cloned()
        .collect();
    content.filter = Some(
        dlc_filter::filter_languages(&own, &keeping, &extra, &others)
            .map_err(|lacking| refused(format!("no file list for {}", lacking.join(", "))))?,
    );
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_build_takes_every_owned_dlc_and_a_rebuild_its_own() {
        assert_eq!(want(None, None), Want::All);
        assert_eq!(want(None, Some(&[3])), Want::Default);
        assert_eq!(want(Some(&Want::NoDlc), None), Want::NoDlc);
        assert_eq!(
            want(Some(&Want::Ids(vec![1])), Some(&[3])),
            Want::Ids(vec![1])
        );
    }

    #[test]
    fn the_language_is_the_asked_one_else_the_games_else_the_caches() {
        assert_eq!(pick_language(None, None, None), language::DEFAULT);
        assert_eq!(pick_language(None, None, Some("de-DE")), "de-DE");
        assert_eq!(pick_language(None, Some("fr-FR"), Some("de-DE")), "fr-FR");
        assert_eq!(pick_language(Some("fr"), Some("fr-FR"), None), "fr-FR");
        assert_eq!(pick_language(Some("pl"), Some("fr-FR"), None), "pl");
    }
}
