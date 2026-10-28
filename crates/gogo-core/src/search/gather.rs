//! Inputs for `search` (design 17 §6): the live library and owned ids, plus product
//! data from the product cache, fetched on a miss (detail per game, one batch call
//! per 50 other owned ids). No offline mode: the two live calls must succeed.

use std::collections::{HashMap, HashSet};

use crate::gog::catalog::{self, Product};
use crate::gog::error::ApiError;
use crate::gog::{API_BASE, EMBED_BASE, library};
use crate::search::Library;
use crate::state::cache;

/// Fetch everything [`super::classify`] needs. Product fetch and cache-write failures
/// degrade the affected rows and come back as warnings (§8); the library and
/// owned-ids calls are fatal.
pub async fn gather(
    http: &reqwest::Client,
    bearer: &str,
    refresh: bool,
) -> Result<(Library, Vec<String>), ApiError> {
    gather_at(http, bearer, refresh, EMBED_BASE, API_BASE).await
}

pub(crate) async fn gather_at(
    http: &reqwest::Client,
    bearer: &str,
    refresh: bool,
    embed: &str,
    api: &str,
) -> Result<(Library, Vec<String>), ApiError> {
    let games = library::list_games_at(http, bearer, embed).await?;
    let owned: HashSet<u64> = library::owned_ids_at(http, bearer, embed)
        .await?
        .into_iter()
        .collect();
    let mut warnings = Vec::new();
    let cached = |id| (!refresh).then(|| cache::load_typed_product(id)).flatten();

    let mut lib_games = Vec::with_capacity(games.len());
    for game in games {
        let detail = match cached(game.id) {
            Some(p) => Some(p),
            None => match catalog::detail_at(http, bearer, game.id, api).await {
                Ok(p) => {
                    save(&p, &mut warnings);
                    Some(p)
                }
                Err(e) => {
                    warnings.push(format!("catalog fetch failed for {}: {e}", game.title));
                    None
                }
            },
        };
        lib_games.push((game, detail));
    }

    // Owned ids a game's detail does not already explain.
    let covered: HashSet<u64> = lib_games
        .iter()
        .flat_map(|(g, d)| {
            std::iter::once(g.id).chain(d.iter().flat_map(|d| d.expanded_dlcs.iter().map(|x| x.id)))
        })
        .collect();
    let mut rest: Vec<u64> = owned.difference(&covered).copied().collect();
    rest.sort_unstable();

    let mut found: HashMap<u64, Product> = HashMap::new();
    let mut misses = Vec::new();
    for &id in &rest {
        match cached(id) {
            Some(p) => {
                found.insert(id, p);
            }
            None => misses.push(id),
        }
    }
    for chunk in misses.chunks(50) {
        match catalog::batch_at(http, bearer, chunk, api).await {
            Ok(products) => {
                for p in products {
                    save(&p, &mut warnings);
                    found.insert(p.id, p);
                }
            }
            Err(e) => warnings.push(format!(
                "catalog fetch failed for {} products: {e}",
                chunk.len()
            )),
        }
    }
    let others = rest.into_iter().map(|id| (id, found.remove(&id))).collect();
    Ok((
        Library {
            games: lib_games,
            others,
            owned,
        },
        warnings,
    ))
}

fn save(p: &Product, warnings: &mut Vec<String>) {
    if let Err(e) = cache::save_product(p) {
        warnings.push(format!("catalog cache write failed: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/search");

    fn body(name: &str) -> String {
        std::fs::read_to_string(format!("{FIX}/{name}")).unwrap()
    }

    /// Mounts the fixture API; `details` = expected detail calls per game,
    /// `batches` = expected batch calls.
    async fn server(details: u64, batches: u64) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/account/getFilteredProducts"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body("library.json")))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/user/data/games"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body("owned.json")))
            .expect(1)
            .mount(&server)
            .await;
        for (id, file) in [
            (1000000001, "product-hot-brass.json"),
            (1000000002, "product.json"),
            (1000000003, "product-1979.json"),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/products/{id}")))
                .respond_with(ResponseTemplate::new(200).set_body_string(body(file)))
                .expect(details)
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/products"))
            .and(query_param("expand", "downloads"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body("products-batch.json")))
            .expect(batches)
            .mount(&server)
            .await;
        server
    }

    async fn run(server: &MockServer) -> (Library, Vec<String>) {
        let http = crate::gog::http().unwrap();
        gather_at(&http, "t", false, &server.uri(), &server.uri())
            .await
            .unwrap()
    }

    #[test]
    fn call_counts_cold_warm_and_old_cache() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::test_util::env_lock();
        // SAFETY: env mutation under the shared env mutex.
        unsafe { std::env::set_var("GOGO_HOME", dir.path()) };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            // Cold: 1 library page + 1 owned ids + 3 details + ⌈4/50⌉ batch.
            let s = server(1, 1).await;
            let (lib, _) = run(&s).await;
            assert_eq!(lib.games.len(), 3);
            assert!(lib.games.iter().all(|(_, d)| d.is_some()));
            // 1000000101 is in Witcher's expanded_dlcs, so not batch-fetched.
            let other: Vec<u64> = lib.others.iter().map(|(id, _)| *id).collect();
            assert_eq!(other, [1000000301, 1000000302, 1000000303]);
            assert!(lib.others.iter().all(|(_, p)| p.is_some()));
            s.verify().await;

            // Warm: the two live calls only.
            let s = server(0, 0).await;
            run(&s).await;
            s.verify().await;
        });

        // A cache entry written before `game_type` existed is refetched exactly once.
        let old = r#"{"id":1000000001,"title":"Hot Brass","slug":"hot_brass"}"#;
        std::fs::write(cache::product_path(1000000001), old).unwrap();
        rt.block_on(async {
            let s = MockServer::start().await;
            Mock::given(path("/account/getFilteredProducts"))
                .respond_with(ResponseTemplate::new(200).set_body_string(body("library.json")))
                .mount(&s)
                .await;
            Mock::given(path("/user/data/games"))
                .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"owned":[]}"#))
                .mount(&s)
                .await;
            Mock::given(path("/products/1000000001"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_string(body("product-hot-brass.json")),
                )
                .expect(1)
                .mount(&s)
                .await;
            run(&s).await;
            run(&s).await;
            s.verify().await;
        });
        unsafe { std::env::remove_var("GOGO_HOME") };
    }

    #[test]
    fn failed_detail_degrades_and_warns() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::test_util::env_lock();
        // SAFETY: env mutation under the shared env mutex.
        unsafe { std::env::set_var("GOGO_HOME", dir.path()) };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let s = MockServer::start().await;
            Mock::given(path("/account/getFilteredProducts"))
                .respond_with(ResponseTemplate::new(200).set_body_string(body("library.json")))
                .mount(&s)
                .await;
            Mock::given(path("/user/data/games"))
                .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"owned":[]}"#))
                .mount(&s)
                .await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(404))
                .mount(&s)
                .await;
            let (lib, warnings) = run(&s).await;
            assert!(lib.games.iter().all(|(_, d)| d.is_none()));
            assert_eq!(warnings.len(), 3);
        });
        unsafe { std::env::remove_var("GOGO_HOME") };
    }
}
