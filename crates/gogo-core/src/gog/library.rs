//! Games-only library via `getFilteredProducts` (`docs/design/03` §1; review A1).
//!
//! Decision (2026-10-09): the paged `mediaType=1` query is the canonical library —
//! always live, never cached, games only. DLCs never appear here; they enter via their
//! parent game's `expanded_dlcs` (catalog detail). Hidden games are excluded by the
//! default query (live-checked: no surfacing param exists), so the listing matches the
//! GOG web default view; `isHidden` is parsed purely defensively.

use serde::Deserialize;

use crate::gog::EMBED_BASE;
use crate::gog::error::ApiError;

/// One library row from `getFilteredProducts` (camelCase live shape).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryProduct {
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub slug: Option<String>,
    /// Hidden metadata ships with every page; v1 shows the default (visible) view.
    #[serde(default)]
    pub is_hidden: bool,
    #[serde(default)]
    pub is_game: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LibraryPage {
    page: u32,
    total_pages: u32,
    #[serde(default)]
    total_products: u32,
    #[serde(default)]
    products: Vec<LibraryProduct>,
}

/// Walk every page of the games library (`mediaType=1`, sorted by title).
///
/// `bearer` is used per request (review B2): the caller (gog-warp-backed auth) supplies
/// the current access token; the 401 path lives upstream as a safety net.
pub async fn list_games(
    http: &reqwest::Client,
    bearer: &str,
) -> Result<Vec<LibraryProduct>, ApiError> {
    list_games_at(http, bearer, EMBED_BASE).await
}

/// Base-parameterized variant (wiremock tests point it at a local server).
pub(crate) async fn list_games_at(
    http: &reqwest::Client,
    bearer: &str,
    base: &str,
) -> Result<Vec<LibraryProduct>, ApiError> {
    let mut all = Vec::new();
    let mut page: u32 = 1;
    loop {
        let url =
            format!("{base}/account/getFilteredProducts?mediaType=1&sortBy=title&page={page}");
        let resp = ApiError::check(http.get(url).bearer_auth(bearer).send().await?).await?;
        let parsed: LibraryPage = resp.json().await.map_err(ApiError::decode)?;

        // Default query excludes hidden games; filter defensively anyway (A1 decision).
        all.extend(parsed.products.into_iter().filter(|p| !p.is_hidden));

        if page >= parsed.total_pages.max(1) {
            break;
        }
        page += 1;
    }
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page_json(page: u32, total_pages: u32, ids: &[(&str, bool)]) -> String {
        let products: Vec<String> = ids
            .iter()
            .map(|(id, hidden)| {
                format!(
                    r#"{{"id": {id}, "title": "Game {id}", "slug": "game_{id}",
                        "isHidden": {hidden}, "isGame": true}}"#
                )
            })
            .collect();
        format!(
            r#"{{"page": {page}, "totalPages": {total_pages}, "totalProducts": 4,
                "products": [{}]}}"#,
            products.join(",")
        )
    }

    #[tokio::test]
    async fn walks_all_pages_and_filters_hidden() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::query_param("page", "1"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_string(page_json(
                    1,
                    2,
                    &[("10", false), ("11", true)],
                )),
            )
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::query_param("page", "2"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_string(page_json(
                    2,
                    2,
                    &[("12", false)],
                )),
            )
            .mount(&server)
            .await;

        let http = crate::gog::http().unwrap();
        let all = list_games_at(&http, "t", &server.uri()).await.unwrap();
        assert_eq!(all.iter().map(|p| p.id).collect::<Vec<_>>(), [10, 12]);
    }

    #[test]
    fn library_page_parses_live_shape() {
        let raw = r#"{"page":1,"totalPages":1,"totalProducts":83,"productsPerPage":100,
            "hasHiddenProducts":true,
            "products":[{"id":1790237640,"title":"60 Minutes","slug":"60_minutes",
            "isHidden":false,"isHidingDisabled":false,"isGame":true,"isMovie":false}]}"#;
        let parsed: LibraryPage = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.total_pages, 1);
        assert_eq!(parsed.products.len(), 1);
        assert_eq!(parsed.products[0].title, "60 Minutes");
        assert!(!parsed.products[0].is_hidden);
    }
}
