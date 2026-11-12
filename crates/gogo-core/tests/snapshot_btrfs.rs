//! Snapshots on a real btrfs, unprivileged (design 20 §10.3). They run in
//! `CARGO_TARGET_TMPDIR` (under `target/`) when that is on btrfs, or under
//! `GOGO_TEST_BTRFS_DIR` when it is set, and otherwise print a `SKIP …` line
//! (nextest shows it: `.config/nextest.toml`). `GOGO_TEST_BTRFS_REQUIRED=1`
//! turns that skip into a failure, and `GOGO_TEST_BTRFS_IOCTL_ALLOWED=1` makes
//! an EPERM from the delete ioctl one (the CI job, design 20 §10.3). They only ever touch the
//! scratch dir they make.
#![cfg(target_os = "linux")]

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use gogo::snapshot::backend::{self, SnapshotBackend};
use gogo::snapshot::btrfs::{self, Btrfs, Destroyed};
use gogo::snapshot::{Content, Guard, Kind, NewSnapshot, Presence, Store};

const GOGID: &str = "1207658924";

/// A scratch dir on btrfs, emptied on drop (snapshots included: they are
/// read-only, which `remove_dir_all` alone can't undo).
struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    fn new(test: &str) -> Option<Self> {
        let base = std::env::var_os("GOGO_TEST_BTRFS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")));
        let dir = tempfile::tempdir_in(&base).unwrap();
        if !btrfs::is_btrfs(dir.path()).unwrap_or(false) {
            // The CI job sets this: there a skip means the mount is missing.
            assert!(
                std::env::var_os("GOGO_TEST_BTRFS_REQUIRED").is_none(),
                "{test}: {} is not on btrfs and GOGO_TEST_BTRFS_REQUIRED is set",
                base.display()
            );
            println!(
                "SKIP {test}: {} is not on btrfs (set GOGO_TEST_BTRFS_DIR to a btrfs dir)",
                base.display()
            );
            return None;
        }
        Some(Self { dir })
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// `<scratch>/games/<gogid>` as a subvolume with a small game in it.
    fn game(&self) -> PathBuf {
        let games = self.path().join("games");
        fs::create_dir_all(&games).unwrap();
        let game = games.join(GOGID);
        btrfs::create_subvolume(&game).unwrap();
        fs::create_dir_all(game.join("game/data")).unwrap();
        fs::create_dir_all(game.join("prefix/drive_c")).unwrap();
        fs::write(game.join("game/game.exe"), b"MZ a game").unwrap();
        fs::write(game.join("prefix/drive_c/slot1.sav"), b"save").unwrap();
        fs::write(game.join("game/data/big.bin"), noise(1 << 20, 1)).unwrap();
        // A read-only dir, as games ship them: the file-by-file delete must
        // still get through it.
        fs::create_dir_all(game.join("game/locked")).unwrap();
        fs::write(game.join("game/locked/x.cfg"), b"x=1").unwrap();
        set_mode(&game.join("game/locked"), 0o555);
        game
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let snaps = self
            .path()
            .join("games")
            .join(backend::SNAPSHOTS_DIR)
            .join(GOGID);
        if let Ok(rd) = fs::read_dir(&snaps) {
            for e in rd.flatten() {
                let _ = btrfs::destroy_by_rmdir(&e.path());
            }
        }
        let _ = gogo::gog::materialize::remove_tree(self.path());
    }
}

fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
}

/// Incompressible bytes (xorshift), so compression doesn't shrink the extents.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn ino(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    fs::metadata(p).unwrap().ino()
}

#[test]
fn btrfs_subvolume_is_inode_256() {
    let Some(s) = Scratch::new("btrfs_subvolume_is_inode_256") else {
        return;
    };
    let game = s.game();
    assert_eq!(ino(&game), btrfs::SUBVOLUME_ROOT_INO);
    assert!(btrfs::is_subvolume(&game).unwrap());
    assert!(Btrfs.ready(&game).unwrap());
    assert!(!btrfs::is_subvolume(&game.join("game")).unwrap());
    assert!(!Btrfs.ready(&game.join("game")).unwrap());
    assert_eq!(
        Btrfs.probe(&game.join("not/yet")),
        gogo::snapshot::Support::Btrfs
    );
    assert!(!btrfs::is_readonly(&game).unwrap());
    assert_eq!(
        btrfs::create_subvolume(&game).unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );
}

#[test]
fn btrfs_readonly_snapshot_refuses_writes() {
    let Some(s) = Scratch::new("btrfs_readonly_snapshot_refuses_writes") else {
        return;
    };
    let game = s.game();
    Btrfs.snapshot(&game, 1).unwrap();
    let snap = Btrfs.snapshot_path(&game, 1).unwrap();
    assert_eq!(
        snap,
        s.path().join("games/.gogo-snapshots").join(GOGID).join("1")
    );
    assert_eq!(ino(&snap), btrfs::SUBVOLUME_ROOT_INO);
    assert!(btrfs::is_readonly(&snap).unwrap());
    assert_eq!(fs::read(snap.join("game/game.exe")).unwrap(), b"MZ a game");

    let err = fs::write(snap.join("game/game.exe"), b"changed").unwrap_err();
    assert_eq!(err.raw_os_error(), Some(30), "EROFS: {err}");
    let err = fs::File::create(snap.join("new")).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(30), "EROFS: {err}");

    // The game dir is untouched by its snapshot's existence, and vice versa.
    fs::write(game.join("game/game.exe"), b"MZ newer").unwrap();
    assert_eq!(fs::read(snap.join("game/game.exe")).unwrap(), b"MZ a game");
    assert!(Btrfs.snapshot(&game, 1).is_err(), "taken twice");

    let on_disk = Btrfs.on_disk(&game).unwrap();
    assert_eq!(on_disk.len(), 1);
    assert_eq!(on_disk[0].n, 1);
    assert!(on_disk[0].created.is_some());

    // Read-only can be flipped by its owner.
    btrfs::set_readonly(&snap, false).unwrap();
    fs::write(snap.join("new"), b"ok").unwrap();
    btrfs::set_readonly(&snap, true).unwrap();
    assert!(btrfs::is_readonly(&snap).unwrap());
}

#[test]
fn btrfs_delete_through_rmdir() {
    let Some(s) = Scratch::new("btrfs_delete_through_rmdir") else {
        return;
    };
    let game = s.game();
    Btrfs.snapshot(&game, 1).unwrap();
    let snap = Btrfs.snapshot_path(&game, 1).unwrap();
    btrfs::destroy_by_rmdir(&snap).unwrap();
    assert!(!snap.exists());
    // And through the backend, whichever way the mount allows.
    Btrfs.snapshot(&game, 2).unwrap();
    Btrfs.delete(&game, 2).unwrap();
    assert!(!Btrfs.exists(&game, 2).unwrap());
    Btrfs.delete(&game, 2).unwrap(); // gone already: fine
    assert!(Btrfs.on_disk(&game).unwrap().is_empty());
}

#[test]
fn btrfs_delete_through_the_ioctl() {
    let Some(s) = Scratch::new("btrfs_delete_through_the_ioctl") else {
        return;
    };
    let game = s.game();
    Btrfs.snapshot(&game, 1).unwrap();
    let snap = Btrfs.snapshot_path(&game, 1).unwrap();
    match btrfs::destroy_by_ioctl(&snap) {
        Ok(()) => {
            assert!(!snap.exists());
            println!("btrfs_delete_through_the_ioctl: SNAP_DESTROY_V2 allowed here");
        }
        Err(e) if e == rustix::io::Errno::PERM => {
            // The CI job's second mount has `user_subvol_rm_allowed`: EPERM
            // there means the option didn't take, so don't skip quietly.
            assert!(
                std::env::var_os("GOGO_TEST_BTRFS_IOCTL_ALLOWED").is_none(),
                "SNAP_DESTROY_V2 refused (EPERM) but GOGO_TEST_BTRFS_IOCTL_ALLOWED is set"
            );
            println!(
                "SKIP btrfs_delete_through_the_ioctl: SNAP_DESTROY_V2 needs \
                 user_subvol_rm_allowed or CAP_SYS_ADMIN here (EPERM); checking the fallback"
            );
            assert!(snap.exists(), "a refused ioctl leaves it");
            assert_eq!(btrfs::destroy(&snap).unwrap(), Destroyed::Rmdir);
            assert!(!snap.exists());
        }
        Err(e) => panic!("SNAP_DESTROY_V2: {e}"),
    }
}

#[test]
fn btrfs_fiemap_estimates_exclusive_space() {
    let Some(s) = Scratch::new("btrfs_fiemap_estimates_exclusive_space") else {
        return;
    };
    let game = s.game();
    let big = game.join("game/data/big.bin");
    let file = fs::File::open(&big).unwrap();
    let ext = btrfs::extents(&file).unwrap();
    assert!(!ext.is_empty());
    assert_eq!(ext.iter().map(|e| e.length).sum::<u64>(), 1 << 20);

    Btrfs.snapshot(&game, 1).unwrap();
    let sp = Btrfs.space(&game, &[1]).unwrap();
    assert_eq!(
        sp[0].exclusive_bytes, 0,
        "a fresh snapshot shares everything"
    );
    assert!(!sp[0].exact);

    // Rewrite the file: snapshot 1 alone holds the old 1 MiB.
    let mut f = fs::File::create(&big).unwrap();
    f.write_all(&noise(1 << 20, 2)).unwrap();
    f.sync_all().unwrap();
    drop(f);
    Btrfs.snapshot(&game, 2).unwrap();
    let sp = Btrfs.space(&game, &[1, 2]).unwrap();
    assert_eq!(sp[0].n, 1);
    assert!(
        (1 << 20..=(1 << 20) + 65536).contains(&sp[0].exclusive_bytes),
        "{sp:?}"
    );
    assert_eq!(
        sp[1].exclusive_bytes, 0,
        "2 shares everything with the game"
    );
}

#[test]
fn btrfs_store_creates_lists_and_deletes() {
    let Some(s) = Scratch::new("btrfs_store_creates_lists_and_deletes") else {
        return;
    };
    let game = s.game();
    let data_root = s.path().join("data");
    let l1 = tempfile::tempfile().unwrap();
    let l2 = tempfile::tempfile().unwrap();
    let g = Guard::new(&l1, &l2);
    let store = Store::new(&data_root, GOGID, &game, &Btrfs);
    let m = store
        .create(
            &g,
            NewSnapshot {
                kind: Kind::Initial,
                trigger: None,
                label: None,
                content: Content::default(),
            },
        )
        .unwrap();
    assert_eq!(m.id, 1);
    let l = store.list().unwrap();
    assert_eq!(l.entries.len(), 1);
    assert_eq!(l.entries[0].presence, Presence::Present);
    let spaces = store.measure_space(&g).unwrap();
    assert_eq!(spaces.len(), 1);
    assert!(store.load(1).unwrap().unwrap().space.is_some());
    store.delete(&g, 1).unwrap();
    assert!(store.list().unwrap().entries.is_empty());
    assert!(!Btrfs.exists(&game, 1).unwrap());

    // A plain dir on btrfs needs converting first.
    let plain = s.path().join("games/plain");
    fs::create_dir_all(&plain).unwrap();
    let store = Store::new(&data_root, "plain", &plain, &Btrfs);
    assert!(matches!(
        store.create(
            &g,
            NewSnapshot {
                kind: Kind::Manual,
                trigger: None,
                label: None,
                content: Content::default(),
            }
        ),
        Err(gogo::snapshot::SnapshotError::NotReady(_))
    ));
}
