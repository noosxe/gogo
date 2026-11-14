//! The build's own files in save locations under the game's files (design 22
//! §3.4): a location such as `<?INSTALL?>` or `<?INSTALL?>/SAVE` mixes the
//! game's saves with the files the build laid there, and no sync may ever
//! upload one of those, overwrite it or delete it.
//!
//! The list is design 20 §6.1's ([`BuildFiles`], from the same places the
//! rollback reads it), tested by path alone: any path the build ships is the
//! build's, whatever its size now (a shipped `.ini` the game rewrote stays
//! out too). gogo's own files there ([`OwnFiles`]) are left out the same way.
//! When gogo can't have the list, the location is skipped, never synced
//! blind.

use std::path::Path;
use std::sync::Arc;

use super::locations::{BuildNote, Game, Resolved, SkipReason, Skipped};
use crate::gog::content;
use crate::gog::depot::RecordSummary;
use crate::linuxapp::rollback::{self, FetchWant};
use crate::macapp::launch::GameDirs;
use crate::snapshot::saves::{BuildFiles, OwnFiles};

/// The build's files and gogo's own in the game's files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildFilter {
    list: BuildFiles,
    own: OwnFiles,
}

impl BuildFilter {
    /// `list` plus gogo's own files in `game`'s files: the compat `files`
    /// its `state.toml` records, and the cnc-ddraw files setup wrote. Walks
    /// the game's files once; [`apply`] builds one per command and shares it
    /// between the locations.
    pub fn new(game: &Game, list: BuildFiles) -> BuildFilter {
        let dirs = GameDirs::at(game.root.clone());
        let compat: Vec<String> = crate::linuxapp::game::LinuxState::load(&dirs.state_toml)
            .ok()
            .flatten()
            .map(|s| {
                s.files
                    .iter()
                    .filter_map(|l| l.split_once(' ').map(|(_, p)| p.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        BuildFilter {
            own: OwnFiles::scan(&game.layout.game_files, &compat),
            list,
        }
    }

    /// Whether `rel` (relative to the game's files, `/`-separated) is the
    /// build's or gogo's.
    pub fn contains(&self, rel: &str) -> bool {
        self.list.lists(rel) || self.own.contains(rel)
    }

    pub fn note(&self) -> BuildNote {
        BuildNote {
            source: self.list.source.clone(),
            files: Some(self.list.len()),
        }
    }
}

/// What gogo knows of a game's build files without the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// From the depot cache or the installers' logs.
    Known(BuildFiles),
    /// A depot-lane game whose cache doesn't hold its build (or all its
    /// DLC): the build's depot manifests must be fetched (signed in).
    Fetch(FetchWant),
    /// No way to tell: why, for the skip message.
    Unknown(String),
}

/// The game's build-file list, offline: the installer lane's Inno logs (the
/// main installer's and every DLC's), or the depot cache when it holds the
/// installed build in the installed language with every DLC the game has;
/// else what to fetch, or why there is none.
pub fn lookup(game: &Game) -> Lookup {
    lookup_in(game, crate::gog::depot::default_cache_dir().as_deref())
}

/// [`lookup`] with the user cache dir `cache_base` (the depot cache is
/// `<cache_base>/gogo/depot/<gogid>`).
pub fn lookup_in(game: &Game, cache_base: Option<&Path>) -> Lookup {
    let dirs = GameDirs::at(game.root.clone());
    let cache_root = game
        .gogid
        .parse()
        .ok()
        .and_then(|id| crate::gog::depot::cache_roots(cache_base, id, None))
        .map(|r| r.0);
    let c = match content::load(&game.root) {
        Ok(Some(c)) => c,
        Ok(None) => match derived(game, &dirs, cache_root.as_deref()) {
            Ok(c) => c,
            Err(why) => return Lookup::Unknown(why),
        },
        Err(e) => return Lookup::Unknown(format!("its content record can't be read: {e}")),
    };
    let dlcs: Vec<u64> = c.dlc.iter().map(|d| d.id).collect();
    match c.lane {
        content::Lane::Installer => installer(game, &dirs, &dlcs),
        content::Lane::Depot => depot(game, &dirs, &c, &dlcs, cache_base, cache_root.as_deref()),
    }
}

/// A game with no content record (built before it, or a Mac-lane game):
/// what its files say, through [`content::resolve`]. The installer lane when
/// the installer's log is there, the depot lane when the game's files are
/// `game/`; a depot-lane game counts only when its own info file names the
/// build, so a depot cache of another build never stands in for it.
fn derived(
    game: &Game,
    dirs: &GameDirs,
    cache_root: Option<&Path>,
) -> Result<content::Content, String> {
    let no_record = "no content record of how the game was built, and";
    let lane = if dirs
        .root
        .join(crate::macapp::INSTALL_LOGS)
        .join(crate::macapp::installer::INNO_LOG)
        .is_file()
    {
        content::Lane::Installer
    } else if game.layout.game_files == dirs.game && dirs.game.is_dir() {
        content::Lane::Depot
    } else {
        return Err(format!(
            "{no_record} no installer log; a rebuild writes one"
        ));
    };
    let resolve = |record: Option<&Path>| {
        content::resolve(&game.root, &game.layout.game_files, lane, record)
            .map(|r| r.content)
            .map_err(|e| e.to_string())
    };
    if lane == content::Lane::Installer {
        return resolve(None);
    }
    if resolve(None)?.build_id.is_none() {
        return Err(format!(
            "{no_record} the game's info file names no build; a rebuild writes one"
        ));
    }
    // The cache's record only for its language, and only when it is for
    // the build the info names (`resolve` checks).
    resolve(cache_root)
}

fn installer(game: &Game, dirs: &GameDirs, dlcs: &[u64]) -> Lookup {
    let Ok(label) = game
        .layout
        .game_files
        .strip_prefix(game.layout.prefix.join("drive_c"))
    else {
        return Lookup::Unknown("the install dir isn't in the prefix's drive_c".into());
    };
    let label = label.to_string_lossy().replace('\\', "/");
    let logs = dirs.root.join(crate::macapp::INSTALL_LOGS);
    // Every DLC needs its log: one without it laid files no list names.
    if let Some(id) = dlcs.iter().find(|id| {
        !logs
            .join(crate::linuxapp::dlc_installer::inno_log_name(**id))
            .is_file()
    }) {
        return Lookup::Unknown(format!("no installer log for DLC {id}"));
    }
    match rollback::installer_list(dirs, &label, dlcs, true) {
        Some(list) => Lookup::Known(list),
        None => Lookup::Unknown(format!(
            "an installer log in {}/ (the game's or a DLC's) can't be read or names no file \
             in the install dir",
            crate::macapp::INSTALL_LOGS,
        )),
    }
}

fn depot(
    game: &Game,
    dirs: &GameDirs,
    c: &content::Content,
    dlcs: &[u64],
    cache_base: Option<&Path>,
    cache_root: Option<&Path>,
) -> Lookup {
    let Some(build_id) = c.build_id.clone() else {
        return Lookup::Unknown("its content record names no build".into());
    };
    let Ok(product_id) = game.gogid.parse::<u64>() else {
        return Lookup::Unknown("not a product id".into());
    };
    let tree = crate::linuxapp::game::read_launch_toml(dirs)
        .ok()
        .and_then(|(t, _)| t.tree_name());
    let holds = cache_root
        .and_then(crate::gog::depot::read_record)
        .is_some_and(|r| cache_holds(&r, c.language.as_deref(), dlcs));
    if holds
        && let Some(list) = rollback::cache_list_in(cache_base, &game.gogid, c, tree.as_deref())
    {
        return Lookup::Known(list);
    }
    let mut products = vec![game.gogid.clone()];
    products.extend(dlcs.iter().map(u64::to_string));
    Lookup::Fetch(FetchWant {
        product_id,
        build_id,
        language: c.language.clone().unwrap_or_else(|| "en-US".to_string()),
        products,
    })
}

/// Whether a depot cache with record `rec` holds the files of `language`
/// (the installed one, when known) and of every DLC in `dlcs`: its record's
/// language, or one of the other languages it still holds (#389), each with
/// the DLC it was filled with. A cache filled for another language, or
/// without a DLC, lacks files the build laid.
pub fn cache_holds(rec: &RecordSummary, language: Option<&str>, dlcs: &[u64]) -> bool {
    let held = match (language, rec.language.as_deref()) {
        (Some(want), Some(main)) if !want.eq_ignore_ascii_case(main) => {
            match rec
                .other_languages
                .iter()
                .find(|(l, _)| l.eq_ignore_ascii_case(want))
            {
                Some((_, d)) => Some(d),
                None => return false,
            }
        }
        _ => rec.dlcs.as_ref(),
    };
    dlcs.is_empty() || held.is_some_and(|h| dlcs.iter().all(|id| h.contains(&id.to_string())))
}

/// Attach `list` to every location of `resolved` in `game`'s files, or skip
/// them all (`BuildFilesUnknown`, with `Err`'s reason) when there is none.
/// Locations elsewhere are left as they are.
pub fn apply(resolved: &mut Resolved, game: &Game, list: Result<BuildFiles, String>) {
    if !resolved.locations.iter().any(|l| l.in_game_files()) {
        return;
    }
    let filter = list.map(|l| Arc::new(BuildFilter::new(game, l)));
    let locations = std::mem::take(&mut resolved.locations);
    for mut loc in locations {
        if !loc.in_game_files() {
            resolved.locations.push(loc);
            continue;
        }
        match &filter {
            Ok(f) => {
                loc.set_build_filter(f.clone());
                resolved.locations.push(loc);
            }
            Err(why) => {
                let reason = SkipReason::BuildFilesUnknown(why.clone());
                resolved.skipped.push(Skipped {
                    name: loc.name.clone(),
                    location: loc.path.display().to_string(),
                    message: reason.to_string(),
                    reason,
                });
            }
        }
    }
}

/// [`lookup`] then [`apply`], for callers that can't fetch: a list that
/// needs fetching counts as none.
pub fn apply_offline(resolved: &mut Resolved, game: &Game) {
    if !resolved.locations.iter().any(|l| l.in_game_files()) {
        return;
    }
    let list = match lookup(game) {
        Lookup::Known(l) => Ok(l),
        Lookup::Fetch(_) => Err(NOT_FETCHED.to_string()),
        Lookup::Unknown(why) => Err(why),
    };
    apply(resolved, game, list);
}

/// The reason when a depot-lane list needed fetching and wasn't had.
pub const NOT_FETCHED: &str = "the depot cache doesn't hold the installed build and its \
     depot manifests couldn't be fetched (signed in? online?)";

#[cfg(test)]
mod tests;
