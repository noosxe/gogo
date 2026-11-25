//! `SkinPackage::load(dir)` over a real folder: the folder name is the skin name, and a
//! symlink or an unsafe name is an error (design 26 S10, design 27 §1 and §10).

use std::path::Path;

use gogo_skin::{Code, SkinPackage};

fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if src.is_dir() {
            copy(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

/// The Default skin copied into `<tmp>/default`, with a file outside the skin next to it.
fn skin_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("default");
    copy(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/default"),
        &dir,
    );
    std::fs::write(tmp.path().join("secret.png"), b"outside the skin").unwrap();
    (tmp, dir)
}

fn codes(r: Result<SkinPackage, gogo_skin::Report>) -> Vec<Code> {
    match r {
        Ok(p) => p.report().findings().iter().map(|f| f.code).collect(),
        Err(r) => r.findings().iter().map(|f| f.code).collect(),
    }
}

#[test]
fn a_folder_loads_under_its_own_name() {
    let (_tmp, dir) = skin_dir();
    let pkg = SkinPackage::load(&dir).expect("loads");
    assert!(pkg.report().is_clean(), "{}", pkg.report());
    assert_eq!(pkg.manifest.name, "default");
}

#[test]
fn a_folder_with_another_name_is_not_the_skin() {
    let (tmp, dir) = skin_dir();
    let other = tmp.path().join("renamed");
    std::fs::rename(&dir, &other).unwrap();
    let report = SkinPackage::load(&other).expect_err("the manifest names `default`");
    assert!(report.findings().iter().any(|f| f.code == Code::SkinName));
}

#[cfg(unix)]
#[test]
fn a_symlink_out_of_the_skin_is_an_error() {
    let (tmp, dir) = skin_dir();
    let secret = tmp.path().join("secret.png");
    std::os::unix::fs::symlink(&secret, dir.join("assets_link.png")).unwrap();
    std::fs::create_dir(dir.join("assets")).unwrap();
    std::os::unix::fs::symlink(&secret, dir.join("assets/logo.png")).unwrap();
    std::os::unix::fs::symlink(tmp.path(), dir.join("assets/up")).unwrap();
    let report = SkinPackage::load(&dir).expect_err("symlinks fail the skin");
    let symlinks: Vec<_> = report
        .findings()
        .iter()
        .filter(|f| f.code == Code::Symlink)
        .map(|f| f.location.as_str())
        .collect();
    assert!(symlinks.contains(&"assets/logo.png"), "{symlinks:?}");
    assert!(symlinks.contains(&"assets/up"), "{symlinks:?}");
    // The top-level link is outside the layout, and a symlink first.
    assert!(symlinks.contains(&"assets_link.png"), "{symlinks:?}");
}

#[cfg(unix)]
#[test]
fn a_symlinked_layout_folder_is_an_error() {
    let (tmp, dir) = skin_dir();
    let outside = tmp.path().join("elsewhere");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("x.kdl"), "route \"queue\" { }").unwrap();
    std::fs::remove_dir_all(dir.join("routes")).unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("routes")).unwrap();
    let c = codes(SkinPackage::load(&dir));
    assert!(c.contains(&Code::Symlink), "{c:?}");
}

#[cfg(unix)]
#[test]
fn names_that_escape_or_are_not_text_are_errors() {
    use std::os::unix::ffi::OsStrExt;
    let (_tmp, dir) = skin_dir();
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    std::fs::write(dir.join("assets").join("a\\b.png"), b"x").unwrap();
    std::fs::write(dir.join("assets").join("c:d.png"), b"x").unwrap();
    let bad = std::ffi::OsStr::from_bytes(b"bad\xff.png");
    std::fs::write(dir.join("assets").join(bad), b"x").unwrap();
    let c = codes(SkinPackage::load(&dir));
    assert_eq!(
        c.iter().filter(|c| **c == Code::PathInvalid).count(),
        3,
        "{c:?}"
    );
}

#[test]
fn files_of_other_types_and_stray_folders_are_errors() {
    let (_tmp, dir) = skin_dir();
    std::fs::write(dir.join("README.md"), b"hi").unwrap();
    std::fs::create_dir(dir.join("scripts")).unwrap();
    std::fs::write(dir.join("scripts/run.sh"), b"#!/bin/sh").unwrap();
    let c = codes(SkinPackage::load(&dir));
    assert_eq!(
        c.iter().filter(|c| **c == Code::FileType).count(),
        2,
        "{c:?}"
    );
}

#[test]
fn an_unreadable_folder_is_an_error_not_a_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let report = SkinPackage::load(&tmp.path().join("missing")).expect_err("no such folder");
    assert!(report.findings().iter().any(|f| f.code == Code::Io));
}

#[test]
fn a_folder_with_too_many_entries_stops_at_the_limit() {
    let (_tmp, dir) = skin_dir();
    let assets = dir.join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    for i in 0..1500 {
        std::fs::write(assets.join(format!("f{i}.png")), b"x").unwrap();
    }
    let report = SkinPackage::load(&dir).expect_err("too many files fail the skin");
    assert!(
        report.findings().iter().any(|f| f.code == Code::LimitFiles),
        "{report}"
    );
    assert!(
        report.findings().len() < 50,
        "the walk stops, it does not list 1500 files"
    );
}
