//! Where the read-only API calls a frontend makes go. GOG's hosts, unless a test
//! points them at a local server (the `test-util` feature).

use std::borrow::Cow;

use crate::gog::catalog::{self, Product};
use crate::gog::error::ApiError;
use crate::gog::library::{self, LibraryProduct};
use crate::gog::overview::{self, Overview};
use crate::gog::{API_BASE, EMBED_BASE};

/// The base addresses of the embed and catalog APIs.
#[derive(Debug, Clone)]
pub struct Endpoints {
    embed: Cow<'static, str>,
    api: Cow<'static, str>,
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints {
            embed: Cow::Borrowed(EMBED_BASE),
            api: Cow::Borrowed(API_BASE),
        }
    }
}

impl Endpoints {
    /// Both APIs on one local test server. It receives the bearer token, so this
    /// exists for tests only.
    #[cfg(feature = "test-util")]
    pub fn at(base: &str) -> Self {
        Endpoints {
            embed: Cow::Owned(base.to_string()),
            api: Cow::Owned(base.to_string()),
        }
    }

    /// [`library::list_games`].
    pub async fn list_games(
        &self,
        http: &reqwest::Client,
        bearer: &str,
    ) -> Result<Vec<LibraryProduct>, ApiError> {
        library::list_games_at(http, bearer, &self.embed).await
    }

    /// [`library::owned_ids`].
    pub async fn owned_ids(
        &self,
        http: &reqwest::Client,
        bearer: &str,
    ) -> Result<Vec<u64>, ApiError> {
        library::owned_ids_at(http, bearer, &self.embed).await
    }

    /// [`catalog::detail`].
    pub async fn detail(
        &self,
        http: &reqwest::Client,
        bearer: &str,
        product_id: u64,
    ) -> Result<Product, ApiError> {
        catalog::detail_at(http, bearer, product_id, &self.api).await
    }

    /// A product's public page data. Sends no credential.
    pub async fn overview(
        &self,
        http: &reqwest::Client,
        product_id: u64,
    ) -> Result<Overview, ApiError> {
        overview::overview_at(http, product_id, &self.api).await
    }
}
