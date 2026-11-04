//! Depot tree materialization (design 18 §6): the forced copy path everywhere,
//! FICLONE where the test dir supports it. `CARGO_TARGET_TMPDIR` (under
//! `target/`) rather than `/tmp`, which is often tmpfs and never reflinks.

use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, SystemTime};

use gogo::gog::materialize::{Reflink, TreeMethod, materialize_tree, plan_tree, probe_reflink};

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap()
}

/// A tree with nested dirs, an executable with an old mtime, a read-only
/// file, an empty file, a read-only dir, a relative symlink and a dangling
/// one. Returns the regular files' total size.
fn fixture(root: &Path) -> u64 {
    fs::create_dir_all(root.join("data/sub")).unwrap();
    fs::create_dir_all(root.join("locked")).unwrap();
    let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    fs::write(root.join("data/sub/big.bin"), &big).unwrap();
    fs::write(root.join("game.exe"), b"MZ stub").unwrap();
    fs::set_permissions(root.join("game.exe"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(root.join("readme.txt"), b"read me").unwrap();
    fs::set_permissions(root.join("readme.txt"), fs::Permissions::from_mode(0o444)).unwrap();
    fs::write(root.join("empty"), b"").unwrap();
    fs::write(root.join("locked/inside.cfg"), b"x=1").unwrap();
    std::os::unix::fs::symlink("data/sub/big.bin", root.join("link.bin")).unwrap();
    std::os::unix::fs::symlink("nowhere", root.join("dangling")).unwrap();
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    File::options()
        .write(true)
        .open(root.join("game.exe"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o555)).unwrap();
    (big.len() + 7 + 7 + 3) as u64
}

/// Byte-identical files, same modes and mtimes, same symlink targets.
fn assert_same_tree(a: &Path, b: &Path) {
    let ma = fs::symlink_metadata(a).unwrap();
    let mb = fs::symlink_metadata(b).unwrap();
    assert_eq!(ma.file_type(), mb.file_type(), "{}", b.display());
    if ma.file_type().is_symlink() {
        assert_eq!(fs::read_link(a).unwrap(), fs::read_link(b).unwrap());
        return;
    }
    assert_eq!(
        ma.permissions().mode() & 0o7777,
        mb.permissions().mode() & 0o7777,
        "mode of {}",
        b.display()
    );
    assert_eq!(
        ma.modified().unwrap(),
        mb.modified().unwrap(),
        "mtime of {}",
        b.display()
    );
    if ma.is_dir() {
        let names = |p: &Path| {
            let mut v: Vec<_> = fs::read_dir(p)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            v.sort();
            v
        };
        let (na, nb) = (names(a), names(b));
        assert_eq!(na, nb, "entries of {}", b.display());
        for n in na {
            assert_same_tree(&a.join(&n), &b.join(&n));
        }
    } else {
        assert_eq!(
            fs::read(a).unwrap(),
            fs::read(b).unwrap(),
            "{}",
            b.display()
        );
    }
}

/// Read-only dirs would make the tempdir's cleanup fail.
fn unlock(root: &Path) {
    for p in [root.join("src/locked"), root.join("out/game/locked")] {
        let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o755));
    }
}

#[test]
fn forced_copy_is_identical_and_accounts_the_bytes() {
    let tmp = scratch();
    let src = tmp.path().join("src");
    let bytes = fixture(&src);
    let dst = tmp.path().join("out/game");
    let report = materialize_tree(&src, &dst, Reflink::Never).unwrap();
    assert_same_tree(&src, &dst);
    assert_eq!(report.method(), TreeMethod::Copy);
    assert_eq!(report.copied_files, 5);
    assert_eq!(report.copied_bytes, bytes);
    assert_eq!((report.reflinked_files, report.reflinked_bytes), (0, 0));
    assert_eq!(report.symlinks, 2);
    assert_eq!(report.dirs, 4);
    // A copy plan costs the tree's size.
    let plan = plan_tree(&src, &tmp.path().join("plan/game"), bytes);
    if plan.method == TreeMethod::Copy {
        assert_eq!(plan.extra_bytes, bytes);
    }
    unlock(tmp.path());
}

/// FICLONE end to end. Skips (with a line in the output: nextest shows this
/// test's output on success, `.config/nextest.toml`) where the scratch dir's
/// filesystem has no reflink, e.g. ext4 CI runners.
#[test]
fn ficlone_reflinks_every_file_where_supported() {
    let tmp = scratch();
    let src = tmp.path().join("src");
    let bytes = fixture(&src);
    let dst = tmp.path().join("out/game");
    if !probe_reflink(&src, &dst) {
        println!(
            "SKIP ficlone_reflinks_every_file_where_supported: {} does not support FICLONE",
            tmp.path().display()
        );
        unlock(tmp.path());
        return;
    }
    let plan = plan_tree(&src, &dst, bytes);
    assert_eq!(plan.method, TreeMethod::Reflink);
    assert_eq!(plan.extra_bytes, 0);
    let report = materialize_tree(&src, &dst, Reflink::Try).unwrap();
    assert_same_tree(&src, &dst);
    assert_eq!(report.method(), TreeMethod::Reflink);
    assert_eq!(report.reflinked_files, 5);
    assert_eq!(report.reflinked_bytes, bytes);
    assert_eq!((report.copied_files, report.copied_bytes), (0, 0));
    // Clones are independent files: writing one leaves the source alone.
    fs::write(dst.join("data/sub/big.bin"), b"changed").unwrap();
    assert_eq!(
        fs::metadata(src.join("data/sub/big.bin")).unwrap().len(),
        200_000
    );
    println!("ficlone_reflinks_every_file_where_supported: reflinked {bytes} bytes");
    unlock(tmp.path());
}
