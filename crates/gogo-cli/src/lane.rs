//! `gogo mac-app` input lanes (#73, design 15 §1): which source a build
//! starts from. Pure decisions only — main.rs gathers the facts (library,
//! depot cache, builds, state.json) and acts on the outcome.

use std::path::{Path, PathBuf};

use gogo::state::records::InstallRecord;

/// What the user pointed `mac-app` at, before any lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A Windows installer path: the installer lane, as given.
    Installer(PathBuf),
    /// An existing depot (or any installed) tree: the depot lane, auth-free.
    Tree(PathBuf),
    /// A game selector (id/slug/fuzzy). `explicit` = it came via
    /// `--from-depot`, which never falls back to the installer lane.
    Game { selector: String, explicit: bool },
}

/// Classify the positional argument / `--from-depot` value (clap makes them
/// exclusive). An existing file, or anything named `*.exe`, is an installer;
/// an existing directory is a tree; everything else is a game selector.
pub fn classify(positional: Option<&str>, from_depot: Option<&str>) -> Option<Input> {
    if let Some(sel) = from_depot {
        return Some(if Path::new(sel).is_dir() {
            Input::Tree(PathBuf::from(sel))
        } else {
            Input::Game {
                selector: sel.to_string(),
                explicit: true,
            }
        });
    }
    let arg = positional?;
    let path = Path::new(arg);
    Some(if path.is_dir() {
        Input::Tree(path.to_path_buf())
    } else if path.is_file() || is_exe(path) {
        Input::Installer(path.to_path_buf())
    } else {
        Input::Game {
            selector: arg.to_string(),
            explicit: false,
        }
    })
}

fn is_exe(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
}

/// What is known about a selected game when picking its lane.
#[derive(Debug, Default)]
pub struct GameFacts {
    /// The depot cache already holds its tree.
    pub cached_tree: Option<PathBuf>,
    /// GOG publishes a public Windows depot build for it.
    pub has_depot_build: bool,
    /// Its downloaded offline installer (`setup_*.exe`), if any.
    pub installer: Option<PathBuf>,
}

/// Where a game selector goes.
#[derive(Debug, PartialEq, Eq)]
pub enum Lane {
    /// Depot lane from the warm cache.
    DepotCached(PathBuf),
    /// Depot lane; the cache is cold, so download first (with consent).
    DepotDownload,
    /// No depot build: the installer lane, with a notice saying so.
    InstallerFallback(PathBuf),
    /// No depot build and no downloaded installer.
    NoSource,
    /// `--from-depot` on a game without a depot build.
    NoDepotBuild,
}

/// The depot lane is the default whenever a depot build exists (docs/19
/// §6.3); the installer lane is the fallback, except under `--from-depot`.
pub fn pick(facts: GameFacts, explicit: bool) -> Lane {
    if let Some(tree) = facts.cached_tree {
        return Lane::DepotCached(tree);
    }
    if facts.has_depot_build {
        return Lane::DepotDownload;
    }
    if explicit {
        return Lane::NoDepotBuild;
    }
    match facts.installer {
        Some(p) => Lane::InstallerFallback(p),
        None => Lane::NoSource,
    }
}

/// The downloaded base-game Windows installer for `product_id` in
/// state.json (the newest when several were saved), as an absolute path
/// under the download `root`. Only files that still exist count.
pub fn downloaded_installer(
    records: &[InstallRecord],
    product_id: u64,
    root: &Path,
) -> Option<PathBuf> {
    records
        .iter()
        .filter(|r| r.product_id == product_id && r.dlc_id.is_none() && r.kind == "installer")
        .filter(|r| {
            r.path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("setup_") && is_exe(Path::new(n)))
        })
        .filter(|r| root.join(&r.path).is_file())
        .max_by(|a, b| a.installed_at.cmp(&b.installed_at))
        .map(|r| root.join(&r.path))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake catalog: what GOG would say about each game (depot build or
    /// not), plus the local depot cache and state.json downloads.
    struct FakeCatalog {
        root: tempfile::TempDir,
        games: Vec<(u64, bool)>,
        cached: Vec<u64>,
        records: Vec<InstallRecord>,
    }

    impl FakeCatalog {
        fn new(games: &[(u64, bool)]) -> Self {
            Self {
                root: tempfile::tempdir().unwrap(),
                games: games.to_vec(),
                cached: Vec::new(),
                records: Vec::new(),
            }
        }

        fn download(&mut self, id: u64, file: &str, at: &str) -> PathBuf {
            let rel = PathBuf::from(format!("game-{id}")).join(file);
            let abs = self.root.path().join(&rel);
            std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
            std::fs::write(&abs, b"MZ").unwrap();
            self.records.push(record(id, None, "installer", rel, at));
            abs
        }

        fn facts(&self, id: u64) -> GameFacts {
            GameFacts {
                cached_tree: self
                    .cached
                    .contains(&id)
                    .then(|| PathBuf::from(format!("/cache/{id}/Game"))),
                has_depot_build: self.games.iter().any(|&(g, b)| g == id && b),
                installer: downloaded_installer(&self.records, id, self.root.path()),
            }
        }
    }

    fn record(id: u64, dlc: Option<u64>, kind: &str, path: PathBuf, at: &str) -> InstallRecord {
        InstallRecord {
            product_id: id,
            dlc_id: dlc,
            product_title: None,
            file_id: "1".into(),
            entry_id: "en1installer0".into(),
            kind: kind.into(),
            version: None,
            build_id: None,
            checksum_ts: None,
            size: 2,
            path,
            installed_at: at.into(),
        }
    }

    #[test]
    fn selector_takes_the_depot_lane() {
        let mut cat = FakeCatalog::new(&[(1, true), (2, true)]);
        cat.cached.push(2);
        // A downloaded installer does not pull a depot game off the depot lane.
        cat.download(1, "setup_one_1.0.exe", "2026-10-01T00:00:00Z");

        assert_eq!(
            classify(Some("one"), None),
            Some(Input::Game {
                selector: "one".into(),
                explicit: false
            })
        );
        assert_eq!(pick(cat.facts(1), false), Lane::DepotDownload);
        assert_eq!(
            pick(cat.facts(2), false),
            Lane::DepotCached(PathBuf::from("/cache/2/Game"))
        );
    }

    #[test]
    fn exe_path_keeps_the_installer_lane() {
        let mut cat = FakeCatalog::new(&[(1, true)]);
        let exe = cat.download(1, "setup_one_1.0.exe", "2026-10-01T00:00:00Z");
        let s = exe.to_str().unwrap();
        assert_eq!(classify(Some(s), None), Some(Input::Installer(exe.clone())));
        // Not downloaded yet: still the installer lane, which reports it.
        assert_eq!(
            classify(Some("Downloads/setup_x_1.0.EXE"), None),
            Some(Input::Installer(PathBuf::from("Downloads/setup_x_1.0.EXE")))
        );
    }

    #[test]
    fn no_depot_build_falls_back_to_the_installer() {
        let mut cat = FakeCatalog::new(&[(1, false), (2, false)]);
        cat.download(1, "setup_one_1.0.exe", "2026-10-01T00:00:00Z");
        let newer = cat.download(1, "setup_one_1.1.exe", "2026-10-05T00:00:00Z");
        assert_eq!(pick(cat.facts(1), false), Lane::InstallerFallback(newer));
        assert_eq!(pick(cat.facts(2), false), Lane::NoSource);
        // --from-depot is explicit: no silent switch of lanes.
        assert_eq!(pick(cat.facts(1), true), Lane::NoDepotBuild);
    }

    #[test]
    fn from_depot_classifies_dirs_and_selectors() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_str().unwrap();
        assert_eq!(
            classify(None, Some(d)),
            Some(Input::Tree(dir.path().to_path_buf()))
        );
        assert_eq!(
            classify(None, Some("1207659046")),
            Some(Input::Game {
                selector: "1207659046".into(),
                explicit: true
            })
        );
        // A tree as the positional argument is the depot lane too.
        assert_eq!(
            classify(Some(d), None),
            Some(Input::Tree(dir.path().to_path_buf()))
        );
        assert_eq!(classify(None, None), None);
    }

    #[test]
    fn installer_lookup_ignores_dlcs_other_os_and_missing_files() {
        let mut cat = FakeCatalog::new(&[]);
        let root = cat.root.path().to_path_buf();
        cat.records.push(record(
            1,
            Some(9),
            "installer",
            "dlc/setup_dlc_1.0.exe".into(),
            "2026-10-09T00:00:00Z",
        ));
        cat.records.push(record(
            1,
            None,
            "installer",
            "one/one_1.0.pkg".into(),
            "2026-10-09T00:00:00Z",
        ));
        cat.records.push(record(
            1,
            None,
            "installer",
            "gone/setup_one_0.9.exe".into(),
            "2026-10-09T00:00:00Z",
        ));
        assert_eq!(downloaded_installer(&cat.records, 1, &root), None);
        let exe = cat.download(1, "setup_one_1.0.exe", "2026-10-01T00:00:00Z");
        cat.download(1, "setup_one_1.0-1.bin", "2026-10-01T00:00:00Z");
        assert_eq!(downloaded_installer(&cat.records, 1, &root), Some(exe));
    }
}
