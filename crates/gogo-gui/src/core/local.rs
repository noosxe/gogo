//! What is on this machine, read and never written (design 25 §2).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gogo::gog::catalog::Os;
use gogo::macapp::apps::App;
use gogo::state::State;

/// A downloaded file as the install records name it.
pub(crate) type FileKey = (u64, Option<u64>, String);

/// A game set up on this machine.
#[derive(Debug, Clone, Default)]
pub(crate) struct Built {
    /// A Linux build, else a macOS one.
    pub linux: bool,
    pub bytes: u64,
    pub runtime: Option<String>,
    /// Version and build id, for a depot build that records both.
    pub installed_build: Option<(String, String)>,
    pub dlcs: Vec<u64>,
    /// Unix seconds.
    pub last_played: Option<f64>,
    pub update_available: bool,
    pub running: bool,
}

/// What was read about the products asked for.
#[derive(Debug, Clone, Default)]
pub(crate) struct Machine {
    pub built: HashMap<u64, Built>,
    /// Products with a depot download cache.
    pub depot_cache: HashSet<u64>,
    /// Bytes of downloaded files, per product.
    pub downloaded_bytes: HashMap<u64, u64>,
    /// Downloaded files that are still there, whole.
    pub downloaded: HashSet<FileKey>,
    /// The systems a product's cached catalog entry has installers for.
    pub platforms: HashMap<u64, Vec<Os>>,
}

pub(crate) trait Local: Send + Sync {
    /// Reads the disk: call it off the runtime's workers.
    fn read(&self, products: &[u64]) -> Machine;
}

/// This machine's state, as the CLI reads it.
pub(crate) struct ThisMachine {
    pub download_dir: PathBuf,
}

impl Local for ThisMachine {
    fn read(&self, products: &[u64]) -> Machine {
        let mut machine = Machine::default();
        match gogo::macapp::apps::list(&gogo::state::logs_root()) {
            Ok(apps) => {
                for app in &apps {
                    if let Ok(id) = app.gogid.parse() {
                        machine.built.insert(id, built(app));
                    }
                }
            }
            // Library paths stay out of the log: the error names them.
            Err(_) => tracing::warn!("the installed games could not be read"),
        }
        match State::load() {
            Ok(state) => {
                for r in state.installs {
                    let there = std::fs::metadata(self.download_dir.join(&r.path))
                        .is_ok_and(|m| m.is_file() && m.len() == r.size);
                    if there {
                        *machine.downloaded_bytes.entry(r.product_id).or_default() += r.size;
                        machine
                            .downloaded
                            .insert((r.product_id, r.dlc_id, r.file_id));
                    }
                }
            }
            Err(_) => tracing::warn!("the download records could not be read"),
        }
        let depot_base = gogo::gog::depot::default_cache_dir();
        for &id in products {
            if let Some((root, ..)) = gogo::gog::depot::cache_roots(depot_base.as_deref(), id, None)
                && gogo::state::local::read_cache(&root).is_some()
            {
                machine.depot_cache.insert(id);
            }
            let platforms = gogo::state::cache::load_typed_product(id)
                .map(|p| platforms(&p))
                .unwrap_or_default();
            machine.platforms.insert(id, platforms);
        }
        machine
    }
}

/// The systems `product` has installers for.
pub(crate) fn platforms(product: &gogo::gog::catalog::Product) -> Vec<Os> {
    let installers = product.downloads.iter().flat_map(|d| &d.installers);
    let mut out: Vec<Os> = Vec::new();
    for os in installers.filter_map(|e| e.os) {
        if !out.contains(&os) {
            out.push(os);
        }
    }
    out
}

fn built(app: &App) -> Built {
    let content = app.content.as_ref();
    Built {
        linux: app.linux,
        bytes: app.bytes,
        runtime: app.runtime.clone().filter(|r| !is_path(r)),
        installed_build: content.and_then(|c| Some((c.version.clone()?, c.build_id.clone()?))),
        dlcs: content
            .map(|c| c.dlc.iter().map(|d| d.id).collect())
            .unwrap_or_default(),
        last_played: app.last_launch.as_deref().and_then(local_seconds),
        update_available: app
            .build
            .as_ref()
            .is_some_and(|b| b.state == gogo::gog::build_hint::State::Newer),
        running: app.linux && running(&app.root),
    }
}

/// A runtime named by its folder (`path:<dir>`) holds a local path, which a skin
/// never gets.
fn is_path(runtime: &str) -> bool {
    runtime.starts_with("path:") || runtime.starts_with('/')
}

fn running(root: &Path) -> bool {
    gogo::linuxapp::game::live_launcher(root).is_some()
}

/// `YYYY-MM-DD HH:MM` in the local zone, as the launch logs are named.
fn local_seconds(text: &str) -> Option<f64> {
    let civil = jiff::civil::DateTime::strptime("%Y-%m-%d %H:%M", text).ok()?;
    let zoned = civil.to_zoned_datetime(jiff::tz::TimeZone::system()).ok()?;
    Some(zoned.timestamp().as_second() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_runtime_given_by_its_folder_is_not_shown() {
        assert!(is_path("path:/somewhere/proton"));
        assert!(!is_path("ge-proton/10-1"));
        assert!(local_seconds("2001-02-03 04:05").is_some());
        assert!(local_seconds("yesterday").is_none());
    }
}
