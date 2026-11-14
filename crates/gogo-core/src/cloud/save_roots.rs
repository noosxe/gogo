//! The resolved cloud-save locations as design 20's save roots (design 22
//! §7.2): read offline, from the remote-config cache, so a rollback never
//! needs the network.

use std::path::PathBuf;

use super::locations::{Game, Layout, resolve};
use super::remote_config;

/// The directories of the game's cloud-save locations that lie in the
/// prefix, for the rollback's finder to scan next to the generic roots.
/// Locations in the game's own files are left out: the finder scans those as
/// the game's files, with the build-file filter. Nothing without a cached
/// remote config (or without cloud saves for the game), and never a network
/// call.
pub fn prefix_roots(game: &Game) -> Vec<PathBuf> {
    let Ok(client_id) = game.client_id() else {
        return vec![];
    };
    prefix_roots_of(&client_id, &game.layout)
}

/// [`prefix_roots`] for a client id and layout.
pub fn prefix_roots_of(client_id: &str, layout: &Layout) -> Vec<PathBuf> {
    let Some(cfg) = remote_config::load_cached(client_id) else {
        return vec![];
    };
    resolve(&cfg, client_id, layout)
        .locations
        .into_iter()
        .filter(|l| l.exists && !l.in_game_files())
        .map(|l| l.path)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::locations::Lane;

    /// A game with a prefix, and a cache with `config` for client id `42`.
    struct World {
        _dir: tempfile::TempDir,
        layout: Layout,
        cache: Option<PathBuf>,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    fn world(config: Option<&str>) -> World {
        let guard = crate::test_util::env_lock();
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let layout = Layout {
            lane: Lane::Linux,
            prefix: base.join("games/1/prefix"),
            game_files: base.join("games/1/game"),
        };
        std::fs::create_dir_all(
            layout
                .prefix
                .join("drive_c/users/steamuser/Documents/Studio"),
        )
        .unwrap();
        std::fs::create_dir_all(&layout.game_files).unwrap();
        std::fs::create_dir_all(layout.game_files.join("saves")).unwrap();
        // SAFETY: under the shared env mutex, held by `_guard`.
        unsafe { std::env::set_var("GOGO_HOME", base.join("home")) };
        let cache = config.map(|c| {
            let p = remote_config::cache_path("42").unwrap();
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, c).unwrap();
            p
        });
        World {
            _dir: dir,
            layout,
            cache,
            _guard: guard,
        }
    }

    fn config(locs: &[(&str, &str)]) -> String {
        let locs: Vec<_> = locs
            .iter()
            .map(|(n, l)| serde_json::json!({"name": n, "location": l}))
            .collect();
        serde_json::json!({"content": {"Windows": {"cloudStorage":
            {"enabled": true, "locations": locs}}}})
        .to_string()
    }

    #[test]
    fn the_cached_locations_in_the_prefix_are_the_roots() {
        let w = world(Some(&config(&[
            ("docs", "<?DOCUMENTS?>/Studio"),
            ("elsewhere", "<?DOCUMENTS?>/Missing"),
            ("game", "<?INSTALL?>/saves"),
        ])));
        assert!(w.cache.is_some());
        let roots = prefix_roots_of("42", &w.layout);
        // The one that exists, not the one that doesn't, not the game's own.
        assert_eq!(
            roots,
            [w.layout
                .prefix
                .join("drive_c/users/steamuser/Documents/Studio")]
        );
    }

    #[test]
    fn without_a_cache_entry_there_are_none_and_no_network_is_used() {
        let w = world(None);
        assert_eq!(prefix_roots_of("42", &w.layout), Vec::<PathBuf>::new());
    }

    #[test]
    fn cloud_saves_off_for_the_game_gives_none() {
        let w = world(Some(
            r#"{"content":{"Windows":{"cloudStorage":{"enabled":false,"locations":[]}}}}"#,
        ));
        assert!(prefix_roots_of("42", &w.layout).is_empty());
    }
}
