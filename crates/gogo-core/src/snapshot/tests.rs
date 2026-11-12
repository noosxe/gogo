//! The snapshot core on the fake backend (design 20 §10.2): any filesystem,
//! Linux and macOS.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use super::fake::{Fake, FakeStep};
use super::*;

const GOGID: &str = "1423049311";

struct Env {
    _tmp: tempfile::TempDir,
    data_root: PathBuf,
    game_dir: PathBuf,
    fake: Fake,
    root_lock: File,
    game_lock: File,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let data_root = tmp.path().join("data");
        let game_dir = tmp.path().join("games").join(GOGID);
        fs::create_dir_all(game_dir.join("game")).unwrap();
        fs::create_dir_all(game_dir.join("prefix/drive_c/users/steamuser/Documents")).unwrap();
        fs::write(game_dir.join("game/game.exe"), b"MZ the game").unwrap();
        fs::write(
            game_dir.join("prefix/drive_c/users/steamuser/Documents/slot1.sav"),
            b"save one",
        )
        .unwrap();
        fs::write(game_dir.join("gogo-launch.toml"), b"gogid = 1423049311\n").unwrap();
        let locks = tmp.path().join("locks");
        fs::create_dir_all(&locks).unwrap();
        let root_lock = File::create(locks.join("root.lock")).unwrap();
        let game_lock = File::create(locks.join("game.lock")).unwrap();
        Self {
            _tmp: tmp,
            data_root,
            game_dir,
            fake: Fake::new(),
            root_lock,
            game_lock,
        }
    }

    fn store(&self) -> Store<'_> {
        Store::new(&self.data_root, GOGID, &self.game_dir, &self.fake)
    }

    fn guard(&self) -> Guard<'_> {
        Guard::new(&self.root_lock, &self.game_lock)
    }

    fn snap_dir(&self, n: u32) -> PathBuf {
        backend::snapshot_dir(&self.game_dir, n).unwrap()
    }

    fn write_override(&self, text: &str) {
        let p = self.data_root.join("compat").join(format!("{GOGID}.toml"));
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn create(&self, kind: Kind, trigger: Option<Trigger>, label: Option<&str>) -> Meta {
        self.store()
            .create(&self.guard(), req(kind, trigger, label))
            .unwrap()
    }

    fn ids(&self) -> Vec<u32> {
        self.store()
            .metadata()
            .unwrap()
            .iter()
            .map(|m| m.id)
            .collect()
    }
}

fn req(kind: Kind, trigger: Option<Trigger>, label: Option<&str>) -> NewSnapshot {
    NewSnapshot {
        kind,
        trigger,
        label: label.map(String::from),
        content: Content {
            lane: Some("depot".into()),
            build_id: Some("58424385209538727".into()),
            version: Some("2.31".into()),
            language: Some("en-US".into()),
            dlcs: vec![1256837418],
            runtime: Some("GE-Proton10-25".into()),
            renderer: Some("dxvk".into()),
        },
    }
}

fn manual() -> NewSnapshot {
    req(Kind::Manual, None, None)
}

fn auto() -> NewSnapshot {
    req(Kind::Auto, Some(Trigger::Rebuild), None)
}

// — metadata —

#[test]
fn metadata_round_trips_with_every_field() {
    let env = Env::new();
    env.write_override("[platforms.linux]\nenv = { A = \"1\" }\n");
    let m = env.create(
        Kind::Auto,
        Some(Trigger::DlcAdd),
        Some("before adding Phantom Liberty"),
    );
    assert_eq!(m.id, 1);
    assert_eq!(m.state, State::Done);
    assert_eq!(m.backend, BackendKind::Btrfs);
    assert_eq!(m.game_dir, env.game_dir);
    assert_eq!(m.override_copy, OverrideCopy::Copied);
    assert_eq!(m.content.dlcs, vec![1256837418]);
    assert_eq!(env.store().load(1).unwrap().unwrap(), m);

    let text = fs::read_to_string(env.store().dir().join("1.toml")).unwrap();
    for line in [
        "id = 1\n",
        "kind = \"auto\"\n",
        "trigger = \"dlc-add\"\n",
        "label = \"before adding Phantom Liberty\"\n",
        "state = \"done\"\n",
        "backend = \"btrfs\"\n",
        "override = \"copied\"\n",
        "[content]\n",
        "lane = \"depot\"\n",
    ] {
        assert!(text.contains(line), "{line:?} in\n{text}");
    }
    // A TOML datetime, not a string.
    assert!(text.contains("created = 20"), "{text}");

    // [space] round-trips too.
    let mut with_space = m.clone();
    with_space.space = Some(SpaceRecord {
        checked: Utc::now().trunc_subsecs(0),
        exclusive_bytes: 912_345_678,
        exact: false,
    });
    write_toml(&env.store().dir().join("1.toml"), &with_space).unwrap();
    assert_eq!(env.store().load(1).unwrap().unwrap(), with_space);
}

#[test]
fn the_override_is_copied_or_recorded_absent() {
    let env = Env::new();
    let m = env.create(Kind::Initial, None, None);
    assert_eq!(m.override_copy, OverrideCopy::Absent);
    assert!(!env.store().override_copy_path(1).exists());

    env.write_override("x = 1\n");
    let m = env.create(Kind::Manual, None, None);
    assert_eq!(m.override_copy, OverrideCopy::Copied);
    assert_eq!(
        fs::read_to_string(env.store().override_copy_path(2)).unwrap(),
        "x = 1\n"
    );
    // The copy is a copy: a later edit of L4 leaves it alone.
    env.write_override("x = 2\n");
    assert_eq!(
        fs::read_to_string(env.store().override_copy_path(2)).unwrap(),
        "x = 1\n"
    );
}

#[test]
fn settings_default_to_off_and_round_trip() {
    let env = Env::new();
    assert_eq!(
        env.store().settings().unwrap(),
        Settings {
            auto: false,
            next: 1
        }
    );
    env.store().set_auto(&env.guard(), true).unwrap();
    assert_eq!(
        env.store().settings().unwrap(),
        Settings {
            auto: true,
            next: 1
        }
    );
    env.create(Kind::Manual, None, None);
    assert_eq!(
        env.store().settings().unwrap(),
        Settings {
            auto: true,
            next: 2
        }
    );
    let text = fs::read_to_string(env.store().dir().join("settings.toml")).unwrap();
    assert_eq!(text, "auto = true\nnext = 2\n");
}

#[test]
fn numbers_are_never_reused() {
    let env = Env::new();
    let g = env.guard();
    assert_eq!(env.create(Kind::Manual, None, None).id, 1);
    assert_eq!(env.create(Kind::Manual, None, None).id, 2);
    env.store().delete(&g, 2).unwrap();
    assert_eq!(env.create(Kind::Manual, None, None).id, 3);
    env.store().delete(&g, 1).unwrap();
    env.store().delete(&g, 3).unwrap();
    assert_eq!(env.ids(), Vec::<u32>::new());
    assert_eq!(env.create(Kind::Manual, None, None).id, 4);

    // A stray snapshot on disk keeps its number, and so does metadata from a
    // data root restored from a backup with an older `next`.
    fs::create_dir_all(env.snap_dir(7)).unwrap();
    assert_eq!(env.create(Kind::Manual, None, None).id, 8);
    write_toml(
        &env.store().dir().join("settings.toml"),
        &Settings {
            auto: true,
            next: 2,
        },
    )
    .unwrap();
    assert_eq!(env.create(Kind::Manual, None, None).id, 9);
}

#[test]
fn labels_default_by_kind_and_trigger() {
    let env = Env::new();
    let label = |kind, trigger, given: Option<&str>| env.create(kind, trigger, given).label;
    assert_eq!(label(Kind::Initial, None, None), "after the first build");
    assert_eq!(label(Kind::Manual, None, None), "manual");
    assert_eq!(label(Kind::Manual, None, Some("  ")), "manual");
    assert_eq!(
        label(Kind::Manual, None, Some(" before the boss ")),
        "before the boss"
    );
    assert_eq!(
        label(Kind::Auto, Some(Trigger::Rebuild), None),
        "before rebuild"
    );
    assert_eq!(
        label(Kind::Auto, Some(Trigger::CloudPull), None),
        "before cloud-save download"
    );
    assert_eq!(
        label(
            Kind::Auto,
            Some(Trigger::DlcAdd),
            Some(&labels::adding(&["A", "B"]))
        ),
        "before adding A, B"
    );
    assert_eq!(labels::update("2.31", "2.5"), "before update 2.31 → 2.5");
    assert_eq!(
        labels::runtime("GE-Proton10-25", "GE-Proton10-30"),
        "before GE-Proton10-25 → GE-Proton10-30"
    );
    assert_eq!(labels::rollback(3), "before rollback to 3");
    assert_eq!(
        labels::removing("Phantom Liberty"),
        "before removing Phantom Liberty"
    );
    assert_eq!(labels::moved(Path::new("/lib")), "after moving to /lib");
}

#[test]
fn a_trigger_belongs_to_auto_snapshots_only() {
    let env = Env::new();
    let g = env.guard();
    for bad in [
        req(Kind::Auto, None, None),
        req(Kind::Manual, Some(Trigger::Rebuild), None),
        req(Kind::Initial, Some(Trigger::Update), None),
    ] {
        assert!(matches!(
            env.store().create(&g, bad),
            Err(SnapshotError::InvalidRequest(_))
        ));
    }
    assert!(!env.store().dir().exists(), "nothing written");
}

// — retention —

#[test]
fn retention_prunes_old_auto_snapshots_only() {
    let env = Env::new();
    let g = env.guard();
    env.create(Kind::Initial, None, None); // 1
    for _ in 0..3 {
        env.store().create(&g, auto()).unwrap(); // 2, 3, 4
    }
    env.create(Kind::Manual, None, None); // 5
    for _ in 0..4 {
        env.store().create(&g, auto()).unwrap(); // 6..9
    }
    let pruned: Vec<u32> = env
        .store()
        .prune(&g, 5)
        .unwrap()
        .iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(pruned, vec![2, 3], "oldest first");
    assert_eq!(env.ids(), vec![1, 4, 5, 6, 7, 8, 9]);
    assert!(!env.snap_dir(2).exists());
    assert!(env.snap_dir(4).exists());
    assert!(env.store().prune(&g, 5).unwrap().is_empty());

    let pruned = env.store().prune(&g, 0).unwrap();
    assert_eq!(pruned.len(), 5);
    assert_eq!(env.ids(), vec![1, 5], "initial and manual stay");
}

// — reconciliation —

#[test]
fn list_reconciles_disk_and_metadata() {
    let env = Env::new();
    let g = env.guard();
    env.create(Kind::Initial, None, None);
    env.create(Kind::Manual, None, None);
    env.create(Kind::Manual, None, None);
    // 1 removed behind gogo's back; 5 on disk with no metadata.
    crate::gog::materialize::remove_tree(&env.snap_dir(1)).unwrap();
    fs::create_dir_all(env.snap_dir(5)).unwrap();

    let l = env.store().list().unwrap();
    assert!(l.reachable);
    let rows: Vec<(u32, Presence, bool)> = l
        .entries
        .iter()
        .map(|e| (e.id, e.presence, e.meta.is_some()))
        .collect();
    assert_eq!(
        rows,
        vec![
            (5, Presence::Stray, false),
            (3, Presence::Present, true),
            (2, Presence::Present, true),
            (1, Presence::Missing, true),
        ]
    );
    assert!(l.entries[0].fs_created.is_some());

    // Both kinds can be deleted: the stray from disk, the missing one's
    // metadata.
    assert_eq!(env.store().delete(&g, 5).unwrap(), None);
    assert!(!env.snap_dir(5).exists());
    assert!(env.store().delete(&g, 1).unwrap().is_some());
    assert_eq!(env.ids(), vec![2, 3]);
    assert!(matches!(
        env.store().delete(&g, 42),
        Err(SnapshotError::NotFound(42))
    ));
}

#[test]
fn an_unreachable_game_lists_its_metadata_as_unreachable() {
    let env = Env::new();
    env.create(Kind::Manual, None, None);
    let gone = env.data_root.join("not-mounted").join(GOGID);
    let store = Store::new(&env.data_root, GOGID, &gone, &env.fake);
    let l = store.list().unwrap();
    assert!(!l.reachable);
    assert_eq!(l.entries.len(), 1);
    assert_eq!(l.entries[0].presence, Presence::Unreachable);
    let g = env.guard();
    assert!(matches!(
        store.create(&g, manual()),
        Err(SnapshotError::Unreachable(_))
    ));
    assert!(matches!(
        store.delete(&g, 1),
        Err(SnapshotError::Unreachable(_))
    ));
}

// — crash injection and recovery —

/// After recovery, snapshot `n` is either all there (metadata `done`, on disk,
/// L4 copy) or not there at all.
fn assert_whole_or_gone(env: &Env, n: u32) -> bool {
    let meta = env.store().load(n).unwrap();
    let on_disk = env.snap_dir(n).exists();
    let copy = env.store().override_copy_path(n).exists();
    match meta {
        Some(m) => {
            assert_eq!(m.state, State::Done);
            assert!(on_disk, "{n}: done but not on disk");
            assert!(copy, "{n}: done without its L4 copy");
            assert_eq!(
                fs::read(env.snap_dir(n).join("game/game.exe")).unwrap(),
                b"MZ the game"
            );
            true
        }
        None => {
            assert!(!on_disk, "{n}: on disk without metadata");
            assert!(!copy, "{n}: L4 copy without metadata");
            false
        }
    }
}

#[test]
fn a_create_stopped_at_any_step_recovers_whole_or_not_at_all() {
    for step in Step::CREATE {
        let env = Env::new();
        env.write_override("x = 1\n");
        let g = env.guard();
        let err = env
            .store()
            .fail_at(Some(step))
            .create(&g, manual())
            .unwrap_err();
        assert!(matches!(err, SnapshotError::Injected(s) if s == step));
        if step != Step::NumberTaken {
            // Nothing else runs until it's settled.
            assert!(matches!(
                env.store().create(&g, manual()),
                Err(SnapshotError::Unsettled(1, _))
            ));
        }
        let recovered = env.store().recover(&g).unwrap();
        let kept = assert_whole_or_gone(&env, 1);
        assert_eq!(kept, step == Step::SnapshotTaken, "{step:?}");
        let want = match step {
            Step::NumberTaken => vec![],
            Step::SnapshotTaken => vec![Recovered::Finished(1)],
            _ => vec![Recovered::Dropped(1)],
        };
        assert_eq!(recovered, want, "{step:?}");
        // The number is gone either way.
        assert_eq!(env.create(Kind::Manual, None, None).id, 2, "{step:?}");
        assert!(env.store().recover(&g).unwrap().is_empty());
    }
}

#[test]
fn a_failed_filesystem_snapshot_leaves_no_entry() {
    let env = Env::new();
    env.write_override("x = 1\n");
    env.fake.fail_at(FakeStep::Snapshot);
    let err = env.store().create(&env.guard(), manual()).unwrap_err();
    assert!(matches!(err, SnapshotError::Io(_)), "{err}");
    assert!(!assert_whole_or_gone(&env, 1));
    assert_eq!(env.create(Kind::Manual, None, None).id, 2);
}

#[test]
fn a_delete_stopped_at_any_step_is_carried_on() {
    enum Stop {
        At(Step),
        Fake(FakeStep),
    }
    let stops = Step::DELETE.into_iter().map(Stop::At).chain([
        Stop::Fake(FakeStep::Delete),
        Stop::Fake(FakeStep::DeleteMidway),
    ]);
    for stop in stops {
        let env = Env::new();
        env.write_override("x = 1\n");
        let g = env.guard();
        env.create(Kind::Manual, None, None);
        env.create(Kind::Manual, None, None);
        let store = match stop {
            Stop::At(step) => env.store().fail_at(Some(step)),
            Stop::Fake(f) => {
                env.fake.fail_at(f);
                env.store()
            }
        };
        assert!(store.delete(&g, 1).is_err());
        if env.store().load(1).unwrap().is_some() {
            assert_eq!(env.store().load(1).unwrap().unwrap().state, State::Deleting);
            assert!(matches!(
                env.store().create(&g, manual()),
                Err(SnapshotError::Unsettled(1, _))
            ));
        }
        assert_eq!(
            env.store().recover(&g).unwrap(),
            vec![Recovered::Deleted(1)]
        );
        assert!(!assert_whole_or_gone(&env, 1));
        assert!(assert_whole_or_gone(&env, 2), "the other one is untouched");
        assert!(env.store().recover(&g).unwrap().is_empty());
    }
}

#[test]
fn recovery_clears_temp_files_and_orphan_copies() {
    let env = Env::new();
    let g = env.guard();
    env.create(Kind::Manual, None, None);
    let dir = env.store().dir().to_path_buf();
    fs::write(dir.join("2.toml.tmp"), "half").unwrap();
    fs::write(dir.join("9.override.toml"), "x = 1").unwrap();
    fs::write(dir.join("notes.txt"), "kept").unwrap();
    assert!(env.store().recover(&g).unwrap().is_empty());
    assert!(!dir.join("2.toml.tmp").exists());
    assert!(!dir.join("9.override.toml").exists());
    assert!(dir.join("notes.txt").exists());
    assert!(dir.join("1.toml").exists());
}

#[test]
fn a_creating_entry_of_an_unreachable_game_is_left_alone() {
    let env = Env::new();
    let g = env.guard();
    let _ = env
        .store()
        .fail_at(Some(Step::SnapshotTaken))
        .create(&g, manual());
    let gone = env.data_root.join("not-mounted").join(GOGID);
    let store = Store::new(&env.data_root, GOGID, &gone, &env.fake);
    assert!(matches!(
        store.recover(&g),
        Err(SnapshotError::Unreachable(_))
    ));
    assert_eq!(env.store().load(1).unwrap().unwrap().state, State::Creating);
}

// — space —

#[test]
fn space_is_measured_and_stored() {
    let env = Env::new();
    let g = env.guard();
    env.create(Kind::Manual, None, None);
    fs::create_dir_all(env.snap_dir(4)).unwrap(); // a stray: measured, not stored
    let save = env
        .game_dir
        .join("prefix/drive_c/users/steamuser/Documents/slot1.sav");
    fs::write(&save, b"save one, changed").unwrap();
    let spaces = env.store().measure_space(&g).unwrap();
    assert_eq!(spaces.len(), 2);
    let one = spaces.iter().find(|s| s.n == 1).unwrap();
    assert_eq!(one.exclusive_bytes, b"save one".len() as u64);
    let stored = env.store().load(1).unwrap().unwrap().space.unwrap();
    assert_eq!(stored.exclusive_bytes, one.exclusive_bytes);
    assert!(!stored.exact);
}

// — refusals that are the core's own —

#[test]
fn a_dir_that_is_not_ready_is_refused() {
    let env = Env::new();
    let missing = env.game_dir.parent().unwrap().join("not-built");
    let store = Store::new(&env.data_root, GOGID, &missing, &env.fake);
    assert!(store.create(&env.guard(), manual()).is_err());
    assert!(!env.store().dir().exists());
}

#[cfg(not(target_os = "linux"))]
#[test]
fn snapshots_need_linux() {
    assert_eq!(
        probe(Path::new("/")),
        Support::None("snapshots need Linux".into())
    );
    assert!(backend_for(&Support::Btrfs).is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn probe_names_the_filesystem() {
    match probe(Path::new("/proc")) {
        Support::None(r) => assert!(r.contains("/proc"), "{r}"),
        other => panic!("{other:?}"),
    }
    assert!(backend_for(&Support::Btrfs).is_some());
    assert!(backend_for(&Support::None(String::new())).is_none());
}
