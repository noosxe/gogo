//! Whatever the bytes, loading ends with a result and never panics (design 26 S12).

use std::path::Path;

use gogo_skin::SkinPackage;
use proptest::prelude::*;

fn base() -> Vec<(String, Vec<u8>)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../gogo-skins/skins/default");
    let mut out = Vec::new();
    for rel in [
        "skin.kdl",
        "tokens.kdl",
        "styles.kdl",
        "components/stock.kdl",
        "routes/shell.kdl",
        "routes/game.kdl",
    ] {
        out.push((rel.to_owned(), std::fs::read(root.join(rel)).unwrap()));
    }
    out
}

const TARGETS: &[&str] = &[
    "skin.kdl",
    "tokens.kdl",
    "styles.kdl",
    "components/stock.kdl",
    "routes/game.kdl",
];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Any text in place of one KDL file.
    #[test]
    fn any_text_in_a_kdl_file(which in 0..TARGETS.len(), text in "\\PC{0,400}") {
        let mut files = base();
        for (p, d) in &mut files {
            if p == TARGETS[which] {
                *d = text.clone().into_bytes();
            }
        }
        let _ = SkinPackage::from_files("default", files);
    }

    /// A real file with a few bytes of it replaced by braces, quotes and `@`.
    #[test]
    fn damaged_files(which in 0..TARGETS.len(), at in 0usize..4000, junk in "[{}\"@#$\\\\ \n]{1,6}") {
        let mut files = base();
        for (p, d) in &mut files {
            if p == TARGETS[which] && !d.is_empty() {
                let mut s = String::from_utf8_lossy(d).into_owned();
                let mut i = at % s.len();
                while !s.is_char_boundary(i) {
                    i -= 1;
                }
                s.insert_str(i, &junk);
                *d = s.into_bytes();
            }
        }
        let _ = SkinPackage::from_files("default", files);
    }

    /// Any bytes as an asset of any kind.
    #[test]
    fn any_bytes_as_an_asset(ext in prop::sample::select(vec!["png", "svg", "otf", "ttf"]), data in prop::collection::vec(any::<u8>(), 0..300)) {
        let mut files = base();
        files.push((format!("assets/a.{ext}"), data));
        let _ = SkinPackage::from_files("default", files);
    }

    /// Any bytes as a file name.
    #[test]
    fn any_path(path in "\\PC{0,60}") {
        let mut files = base();
        files.push((path, b"x".to_vec()));
        let _ = SkinPackage::from_files("default", files);
    }
}
