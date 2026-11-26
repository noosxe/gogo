//! The Default skin loads through the public loader with no errors and no warnings
//! (design 26 S17, design 27 §9).

use std::path::Path;

use gogo_skin::SkinPackage;

fn fixture() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../gogo-skins/skins/default")
}

#[test]
fn default_skin_loads_clean() {
    let pkg = match SkinPackage::load(&fixture()) {
        Ok(p) => p,
        Err(report) => panic!("{report}"),
    };
    assert!(pkg.report().is_clean(), "{}", pkg.report());
    assert_eq!(pkg.manifest.name, "default");
    assert_eq!(pkg.routes.len(), 2);
    assert!(pkg.components.contains_key("status-badge"));
}
