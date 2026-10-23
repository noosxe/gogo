//! Compile the L3 compat DB (`compat/<gogid>.toml` at the repo root,
//! docs/19 §6.4) into the binary: `$OUT_DIR/compat_db.rs` holds one
//! `include_str!` per entry, sorted by id. Validation happens in the
//! `compat` unit tests, not here.

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let dir = manifest.join("../../compat");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut entries: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "toml"))
                .filter_map(|p| {
                    let id = p.file_stem()?.to_str()?.to_string();
                    Some((id, p.canonicalize().ok()?))
                })
                .collect()
        })
        .unwrap_or_default();
    entries.sort();
    let mut out = String::from("/// `(gogid, toml)` for every compiled-in compat DB entry.\n");
    out.push_str("pub(crate) static COMPAT_DB: &[(&str, &str)] = &[\n");
    for (id, path) in &entries {
        println!("cargo:rerun-if-changed={}", path.display());
        out.push_str(&format!(
            "    ({id:?}, include_str!({:?})),\n",
            path.display().to_string()
        ));
    }
    out.push_str("];\n");
    let dest = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("compat_db.rs");
    std::fs::write(dest, out).unwrap();
}
