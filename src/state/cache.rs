//! Catalog cache files (`docs/design/06` §3): one product JSON per id under the cache
//! dir, refreshed per run (cheap) or on demand. Contains no secrets.

/// Cache path for one product (`<cache>/products/<id>.json`).
pub fn product_path(_product_id: u64) -> std::path::PathBuf {
    todo!("docs/design/06 §1/§3 (GOGO_HOME flattens)")
}

/// Read cached product JSON, if any.
pub fn load_product(_product_id: u64) -> anyhow::Result<Option<crate::gog::catalog::Product>> {
    todo!("docs/design/06 §3")
}

/// Write product JSON atomically after a successful batch fetch.
pub fn save_product(_product: &crate::gog::catalog::Product) -> anyhow::Result<()> {
    todo!("docs/design/06 §3")
}
