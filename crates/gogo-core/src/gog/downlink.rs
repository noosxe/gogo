//! `files[].downlink` → `{ downlink, checksum }` (`docs/design/04` §3).
//!
//! Real path (live-verified, docs/11): `GET api/products/{pid}/downlink/installer/{file_id}`
//! with Bearer → **200 JSON** (no redirect) containing the pre-tokenized CDN URL and the
//! checksum XML URL. CDN URLs are pre-tokenized in the path, never persisted, re-resolved
//! per file per run. The same flow serves bonus_content and language_packs — the content
//! type is embedded in the `downlink` path itself (review C4), so this resolver is
//! entry-kind-agnostic.

use crate::gog::API_BASE;
use crate::gog::error::ApiError;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Downlink {
    /// Pre-tokenized CDN URL (`gog-cdn-fastly.gog.com/token=…~dirs=…/secure/offline/…`).
    /// SECURITY: never log, never persist (`docs/design/02` §5, `07` §4).
    pub downlink: String,
    /// Public checksum XML URL (no auth needed).
    pub checksum: String,
}

/// Resolve one `files[]` entry to its CDN URL + checksum doc URL.
///
/// `downlink` is the value from [`crate::gog::catalog::FileEntry::downlink`]: either an
/// absolute URL or an API-relative path (`/products/{pid}/downlink/{kind}/{file_id}`).
pub async fn resolve(
    http: &reqwest::Client,
    bearer: &str,
    downlink: &str,
) -> Result<Downlink, ApiError> {
    let resp = ApiError::check(
        http.get(normalize(downlink))
            .bearer_auth(bearer)
            .send()
            .await?,
    )
    .await?;
    resp.json().await.map_err(ApiError::from)
}

/// Absolute URL passes through; bare path joins onto the API base.
fn normalize(downlink: &str) -> String {
    if downlink.starts_with("http://") || downlink.starts_with("https://") {
        downlink.to_owned()
    } else {
        if downlink.starts_with('/') {
            format!("{API_BASE}{downlink}")
        } else {
            format!("{API_BASE}/{downlink}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_joins_relative_paths() {
        assert_eq!(
            normalize("/products/7/downlink/installer/en1installer0"),
            "https://api.gog.com/products/7/downlink/installer/en1installer0"
        );
    }

    #[test]
    fn normalize_passes_absolute_urls_through() {
        assert_eq!(
            normalize("https://example.com/products/7/downlink/extras/x"),
            "https://example.com/products/7/downlink/extras/x"
        );
    }

    #[tokio::test]
    async fn resolve_parses_downlink_json() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/products/7/downlink/installer/en1installer0",
            ))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(
                r#"{"downlink":"https://cdn.gog.com/token=x~dirs=y/secure/offline/a.exe",
                    "checksum":"https://cdn.gog.com/secure/a.exe.xml"}"#,
            ))
            .expect(1..)
            .mount(&server)
            .await;

        let http = crate::gog::http().unwrap();
        let path = format!(
            "{}/products/7/downlink/installer/en1installer0",
            server.uri()
        );
        let got = resolve(&http, "t", &path).await.unwrap();
        assert_eq!(
            got.downlink,
            "https://cdn.gog.com/token=x~dirs=y/secure/offline/a.exe"
        );
        assert_eq!(got.checksum, "https://cdn.gog.com/secure/a.exe.xml");
    }

    #[tokio::test]
    async fn resolve_maps_403_for_bundled_extras() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let http = crate::gog::http().unwrap();
        assert!(matches!(
            resolve(&http, "t", &server.uri()).await,
            Err(ApiError::Forbidden(_))
        ));
    }
}
