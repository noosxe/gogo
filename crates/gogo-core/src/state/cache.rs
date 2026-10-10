//! Catalog cache files (`docs/design/06` §3): one product JSON per id under the cache
//! dir, plus the verify-cache of checksum XMLs. Contains no secrets.
//!
//! Review A5: the verify-cache is keyed `{product_id}-{dlc_id|base}-{file_id}.xml` —
//! exactly like `InstallRecord` identity — so a DLC file and a base-game file can
//! never collide.

use crate::gog::catalog::Product;

/// Cache base dir (`docs/design/06` §1; `GOGO_HOME` flattens).
pub fn cache_dir() -> std::path::PathBuf {
    if let Some(home) = std::env::var_os("GOGO_HOME") {
        return std::path::PathBuf::from(home).join("cache");
    }
    dirs::cache_dir()
        .map(|d| d.join("gogo"))
        .unwrap_or_else(|| std::path::PathBuf::from(".gogo-cache"))
}

/// Cache path for one product (`<cache>/products/<id>.json`).
pub fn product_path(product_id: u64) -> std::path::PathBuf {
    cache_dir()
        .join("products")
        .join(format!("{product_id}.json"))
}

/// Read cached product JSON, if any.
pub fn load_product(product_id: u64) -> anyhow::Result<Option<Product>> {
    let path = product_path(product_id);
    match std::fs::read_to_string(&path) {
        Ok(raw) => Ok(serde_json::from_str(&raw)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Write product JSON atomically after a successful batch fetch.
pub fn save_product(product: &Product) -> anyhow::Result<()> {
    atomic_write(&product_path(product.id), &serde_json::to_string(product)?)
}

/// Verify-cache path for one file's checksum XML (A5 keying).
pub fn checksum_path(product_id: u64, dlc_id: Option<u64>, file_id: &str) -> std::path::PathBuf {
    cache_dir().join("verify-cache").join(format!(
        "{product_id}-{}-{file_id}.xml",
        crate::state::records::InstallRecord::dlc_key(dlc_id)
    ))
}

/// Read the cached checksum XML (raw), if any — parsed by `gog::checksum::parse`.
pub fn load_checksum(product_id: u64, dlc_id: Option<u64>, file_id: &str) -> Option<String> {
    std::fs::read_to_string(checksum_path(product_id, dlc_id, file_id)).ok()
}

/// Store the last checksum XML so `gogo verify` works offline (06 §3).
pub fn save_checksum(
    product_id: u64,
    dlc_id: Option<u64>,
    file_id: &str,
    xml: &str,
) -> anyhow::Result<()> {
    atomic_write(&checksum_path(product_id, dlc_id, file_id), xml)
}

/// tmp + fsync + rename (06 §2 atomicity rules).
fn atomic_write(path: &std::path::Path, contents: &str) -> anyhow::Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn product(id: u64) -> Product {
        serde_json::from_str(&format!(r#"{{"id":{id},"title":"T"}}"#)).unwrap()
    }

    #[test]
    fn product_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        // Redirect the cache dir via GOGO_HOME for this test (cache_dir reads env once
        // per call; tests that touch it are serialized by the env mutex below).
        let guard = crate::test_util::env_lock();
        // SAFETY: env mutation under the shared env mutex.
        unsafe { std::env::set_var("GOGO_HOME", dir.path()) };
        assert!(load_product(5).unwrap().is_none());
        save_product(&product(5)).unwrap();
        let loaded = load_product(5).unwrap().unwrap();
        assert_eq!(loaded.id, 5);
        assert_eq!(
            product_path(5),
            dir.path().join("cache").join("products").join("5.json")
        );
        // SAFETY: see above.
        unsafe { std::env::remove_var("GOGO_HOME") };
        drop(guard);
    }

    #[test]
    fn verify_cache_keys_dlc_and_base_apart() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::test_util::env_lock();
        // SAFETY: env mutation under the shared env mutex.
        unsafe { std::env::set_var("GOGO_HOME", dir.path()) };

        let base = checksum_path(7, None, "en1installer0");
        let dlc = checksum_path(7, Some(8), "en1installer0");
        assert_ne!(base, dlc, "A5: base and DLC files must not collide");
        assert!(base.to_string_lossy().contains("7-base-en1installer0.xml"));
        assert!(dlc.to_string_lossy().contains("7-8-en1installer0.xml"));

        save_checksum(7, Some(8), "en1installer0", "<file/>").unwrap();
        assert_eq!(
            load_checksum(7, Some(8), "en1installer0").as_deref(),
            Some("<file/>")
        );
        assert_eq!(load_checksum(7, None, "en1installer0"), None);

        // SAFETY: see above.
        unsafe { std::env::remove_var("GOGO_HOME") };
        drop(guard);
    }
}
