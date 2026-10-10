//! Products: batch `?ids=…` (50/call) + detail with `expand=downloads,expanded_dlcs`
//! (`docs/design/03`; shapes live-verified in `docs/04`/`docs/11`).

use serde::Deserialize;

use crate::gog::API_BASE;
use crate::gog::error::ApiError;

/// One product from the catalog. Unknown/extra fields are tolerated.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Product {
    pub id: u64,
    pub title: Option<String>,
    pub slug: Option<String>,
    pub downloads: Option<Downloads>,
    /// DLC products, present on detail fetches with `expand=expanded_dlcs`
    /// (review A1: DLCs enter only through their parent game).
    #[serde(default, rename = "expanded_dlcs")] // snake_case in the camelCase doc (live)
    pub expanded_dlcs: Vec<Product>,
}

/// `downloads` — live-verified top-level shape.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Downloads {
    #[serde(default)]
    pub installers: Vec<InstallerEntry>,
    #[serde(default)]
    pub patches: Vec<InstallerEntry>,
    #[serde(default)]
    pub bonus_content: Vec<InstallerEntry>,
    #[serde(default)]
    pub language_packs: Vec<InstallerEntry>,
}

/// One `downloads.*[]` entry: `en1installer0`-style id + metadata + parts.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct InstallerEntry {
    pub id: String,
    pub name: Option<String>,
    pub os: Option<Os>,
    pub language: Option<String>,
    pub language_full: Option<String>,
    pub version: Option<String>,
    pub total_size: Option<u64>,
    /// Multi-part files: `en1installer0`, `en1installer1`, … (live-verified).
    #[serde(default)]
    pub files: Vec<FileEntry>,
}

/// One downloadable part (`files[]`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FileEntry {
    pub id: String,
    pub size: u64,
    /// API path — resolve through [`crate::gog::downlink`] to get the CDN URL
    /// (the modern API has no direct `galaxy_downlink_json_url`).
    pub downlink: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Windows,
    Mac,
    Linux,
}

/// Fetch catalog entries in batches of 50 (`docs/design/03` §1).
pub async fn batch(
    http: &reqwest::Client,
    bearer: &str,
    product_ids: &[u64],
) -> Result<Vec<Product>, ApiError> {
    batch_at(http, bearer, product_ids, API_BASE).await
}

/// Fetch one product with downloads + DLC tree (the engine's task source).
pub async fn detail(
    http: &reqwest::Client,
    bearer: &str,
    product_id: u64,
) -> Result<Product, ApiError> {
    detail_at(http, bearer, product_id, API_BASE).await
}

pub(crate) async fn batch_at(
    http: &reqwest::Client,
    bearer: &str,
    product_ids: &[u64],
    base: &str,
) -> Result<Vec<Product>, ApiError> {
    let mut out = Vec::with_capacity(product_ids.len());
    for group in product_ids.chunks(50) {
        let ids = group
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let url = format!("{base}/products?ids={ids}");
        let resp = ApiError::check(http.get(&url).bearer_auth(bearer).send().await?).await?;
        let page: Vec<Product> = resp.json().await.map_err(ApiError::from)?;
        out.extend(page);
    }
    Ok(out)
}

pub(crate) async fn detail_at(
    http: &reqwest::Client,
    bearer: &str,
    product_id: u64,
    base: &str,
) -> Result<Product, ApiError> {
    let url = format!("{base}/products/{product_id}?expand=downloads,expanded_dlcs");
    let resp = ApiError::check(http.get(&url).bearer_auth(bearer).send().await?).await?;
    resp.json().await.map_err(ApiError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn batch_groups_ids_and_parses() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::query_param("ids", "1,2"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(
                r#"[{"id":1,"title":"A","downloads":{"installers":[]}},
                    {"id":2,"title":"B","expanded_dlcs":[]}]"#,
            ))
            .mount(&server)
            .await;

        let http = crate::gog::http().unwrap();
        let got = batch_at(&http, "t", &[1, 2], &server.uri()).await.unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].title.as_deref(), Some("A"));
    }

    #[tokio::test]
    async fn detail_expands_downloads_and_dlcs() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/products/7"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(
                r#"{"id":7,"title":"Parent","slug":"parent",
                    "downloads":{"installers":[{"id":"en1installer0","name":"setup",
                      "os":"windows","language":"en","version":"1.0","total_size":100,
                      "files":[{"id":"en1installer0","size":100,
                        "downlink":"/products/7/downlink/installer/en1installer0"}]}]},
                    "expanded_dlcs":[{"id":8,"title":"DLC","downloads":{"installers":[]}}]}"#,
            ))
            .mount(&server)
            .await;

        let http = crate::gog::http().unwrap();
        let p = detail_at(&http, "t", 7, &server.uri()).await.unwrap();
        assert_eq!(p.title.as_deref(), Some("Parent"));
        assert_eq!(p.expanded_dlcs.len(), 1);
        assert_eq!(p.expanded_dlcs[0].id, 8);
        let files = &p.downloads.as_ref().unwrap().installers[0].files;
        assert_eq!(
            files[0].downlink,
            "/products/7/downlink/installer/en1installer0"
        );
    }

    #[tokio::test]
    async fn detail_maps_404() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let http = crate::gog::http().unwrap();
        assert!(matches!(
            detail_at(&http, "t", 9, &server.uri()).await,
            Err(ApiError::NotFound(_))
        ));
    }
}
