//! Owned ids (embed `/user/data/games`) + `getFilteredProducts` paging
//! (`docs/design/03` §1 library source; live-verified `docs/11`).

use crate::gog::error::ApiError;

/// All owned product ids — one call, always live (never cached).
pub async fn owned_ids(_http: &reqwest::Client, _bearer: &str) -> Result<Vec<u64>, ApiError> {
    todo!("GET embed /user/data/games → owned_products[] — docs/03 §2")
}
