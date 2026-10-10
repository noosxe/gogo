//! Products: batch `?ids=…` (50/call) + detail with `expand=downloads,expanded_dlcs`
//! (`docs/design/03`; shapes live-verified in `docs/04`/`docs/11`).

use serde::{Deserialize, Serialize};

use crate::gog::API_BASE;
use crate::gog::error::ApiError;

/// One product from the catalog. Unknown/extra fields are tolerated.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// GOG ids are strings for installers/patches/language_packs but **numbers** for DLC
/// `bonus_content` entries (live-verified, gogo-probe capture). Normalize to String.
fn string_or_number<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct V;
    impl serde::de::Visitor<'_> for V {
        type Value = String;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a string or a number")
        }
        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<String, E> {
            Ok(v.to_owned())
        }
        fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<String, E> {
            Ok(v.to_string())
        }
        fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<String, E> {
            Ok(v.to_string())
        }
    }
    deserializer.deserialize_any(V)
}

/// One `downloads.*[]` entry: `en1installer0`-style id + metadata + parts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct InstallerEntry {
    #[serde(deserialize_with = "string_or_number")]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FileEntry {
    #[serde(deserialize_with = "string_or_number")]
    pub id: String,
    pub size: u64,
    /// API path — resolve through [`crate::gog::downlink`] to get the CDN URL
    /// (the modern API has no direct `galaxy_downlink_json_url`).
    pub downlink: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
        let page: Vec<Product> = resp.json().await.map_err(ApiError::decode)?;
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
    resp.json().await.map_err(ApiError::decode)
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

#[cfg(test)]
mod live_capture_tests {
    use super::Product;

    /// Parses the real capture from gogo-probe/product.json when present (gitignored;
    /// skips silently otherwise). This is the exact live detail shape.
    #[test]
    fn parses_live_detail_capture() {
        let Ok(raw) = std::fs::read_to_string("../../gogo-probe/product.json") else {
            return;
        };
        let p: Product = serde_json::from_str(&raw).unwrap();
        let dl = p.downloads.expect("downloads present in capture");
        assert!(!dl.installers.is_empty());
        // The DLC's bonus_content ids arrive as numbers and normalize to strings.
        assert_eq!(p.expanded_dlcs.len(), 1);
        let dlc = &p.expanded_dlcs[0];
        let bonus = &dlc.downloads.as_ref().unwrap().bonus_content;
        assert!(!bonus.is_empty());
        assert_eq!(bonus[0].id, "100897");
        assert_eq!(bonus[0].files[0].id, "100897");
    }
}
