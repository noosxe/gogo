use std::cell::RefCell;
use std::fs;
use std::rc::Rc;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::cloud::locations::{Lane, Layout};
use crate::cloud::remote_config;
use crate::cloud::storage::{gzip, md5_hex};
use crate::cloud::testing::{FixedExchange, MainSession, NoManifests, keep_manifest};

const CLIENT: &str = "11111111111111111";
const CONFIG: &str = r#"{"content":{"Windows":{"cloudStorage":{"enabled":true,
    "locations":[{"name":"saves","location":"<?DOCUMENTS?>/G"}]}}}}"#;
/// The uploader's mtime the mock storage sends.
const UPLOADED: &str = "2024-02-11T03:49:20Z";

struct Fixture {
    _dir: tempfile::TempDir,
    game: Game,
    cache: PathBuf,
    records: PathBuf,
    saves: PathBuf,
    server: MockServer,
    exchange: FixedExchange,
    session: MainSession,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let root = base.join("games/5");
    let game_files = root.join("game");
    let prefix = root.join("prefix");
    let saves = prefix.join("drive_c/users/steamuser/Documents/G");
    fs::create_dir_all(&saves).unwrap();
    fs::create_dir_all(&game_files).unwrap();
    keep_manifest(&root, CLIENT);
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/components/galaxy_client/clients/{CLIENT}")))
        .respond_with(ResponseTemplate::new(200).set_body_string(CONFIG))
        .mount(&server)
        .await;
    Fixture {
        game: Game {
            gogid: "5".into(),
            layout: Layout {
                lane: Lane::Linux,
                prefix,
                game_files,
            },
            root,
        },
        cache: base.join("cache"),
        records: base.join("data/cloud-saves"),
        saves,
        server,
        exchange: FixedExchange::default(),
        session: MainSession::new(),
        _dir: dir,
    }
}

/// A scripted [`Decide`]: one answer for every conflict, one for the
/// question; what was asked.
struct Script {
    choice: Choice,
    confirm: Confirm,
    asked: Vec<String>,
    consents: Vec<(usize, usize)>,
}

impl Script {
    fn new(choice: Choice, confirm: Confirm) -> Self {
        Self {
            choice,
            confirm,
            asked: vec![],
            consents: vec![],
        }
    }
}

impl Decide for Script {
    fn conflict(&mut self, file: &FilePlan, choices: &[Choice]) -> Choice {
        assert_eq!(choices.last(), Some(&Choice::Skip));
        self.asked.push(file.name.clone());
        self.choice
    }
    fn confirm(&mut self, c: &Consent) -> Confirm {
        self.consents.push((c.overwrites, c.deletes));
        self.confirm
    }
}

/// A [`Host`] that logs its calls, with what a save file held at the
/// snapshot (it must still be the old one).
#[derive(Default)]
struct FakeHost {
    log: Rc<RefCell<Vec<String>>>,
    refuse: bool,
    fail_snapshot: bool,
    watch: Option<PathBuf>,
}

impl Host for FakeHost {
    fn lock(&mut self) -> Result<(), CloudError> {
        self.log.borrow_mut().push("lock".into());
        if self.refuse {
            return Err(CloudError::Refused("5 is running".into()));
        }
        Ok(())
    }
    fn snapshot(&mut self) -> Result<Option<String>, CloudError> {
        let seen = self
            .watch
            .as_ref()
            .map(|p| String::from_utf8_lossy(&fs::read(p).unwrap_or_default()).into_owned())
            .unwrap_or_default();
        self.log.borrow_mut().push(format!("snapshot:{seen}"));
        if self.fail_snapshot {
            return Err(CloudError::Refused("the snapshot couldn't be taken".into()));
        }
        Ok(Some("snapshot 3 taken (before cloud-save download)".into()))
    }
}

impl Fixture {
    /// Serve `objects` (name → stored bytes) as the listing and as objects,
    /// each with the uploader's mtime.
    async fn serve(&self, objects: &[(&str, Vec<u8>)]) {
        self.server.reset().await;
        Mock::given(method("GET"))
            .and(path(format!("/components/galaxy_client/clients/{CLIENT}")))
            .respond_with(ResponseTemplate::new(200).set_body_string(CONFIG))
            .mount(&self.server)
            .await;
        let listing: Vec<serde_json::Value> = objects
            .iter()
            .map(|(n, b)| serde_json::json!({"name": n, "bytes": b.len(), "hash": md5_hex(b)}))
            .collect();
        Mock::given(method("GET"))
            .and(path(format!("/v1/7/{CLIENT}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-container-object-count", objects.len().to_string())
                    .set_body_json(listing),
            )
            .mount(&self.server)
            .await;
        for (n, b) in objects {
            Mock::given(method("GET"))
                .and(path(format!("/v1/7/{CLIENT}/{}", n.replace(' ', "%20"))))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("x-object-meta-locallastmodified", UPLOADED)
                        .set_body_bytes(b.clone()),
                )
                .mount(&self.server)
                .await;
        }
    }

    fn write(&self, rel: &str, body: &[u8]) {
        let p = self.saves.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    fn read(&self, rel: &str) -> Option<Vec<u8>> {
        fs::read(self.saves.join(rel)).ok()
    }

    /// Record `rel` as synced with `body` on both sides, as on disk now.
    fn recorded(&self, r: &mut Record, rel: &str, body: &[u8]) {
        let meta = fs::metadata(self.saves.join(rel)).unwrap();
        r.files.insert(
            format!("saves/{rel}"),
            FileRecord {
                cloud_hash: md5_hex(&gzip(body)),
                local_md5: md5_hex(body),
                size: meta.len(),
                mtime: record::datetime(meta.modified().unwrap()),
            },
        );
    }

    fn save_record(&self, r: &Record) {
        SyncLock::acquire(&self.records, 5)
            .unwrap()
            .save(r)
            .unwrap();
    }

    fn record(&self) -> Option<Record> {
        record::load(&self.records, 5).unwrap()
    }

    async fn pull(
        &self,
        opts: &PullOptions,
        host: &mut dyn Host,
        decide: &mut dyn Decide,
    ) -> Result<PullReport, CloudError> {
        let auth = CloudAuth::with_parts(&self.session, || Ok(()), NoManifests, &self.exchange);
        let storage =
            StorageClient::with_base(format!("{}/v1/", self.server.uri()).parse().unwrap())
                .unwrap();
        let (cfg, source) = remote_config::get_in(
            &self.cache,
            &reqwest::Client::new(),
            &self.server.uri(),
            CLIENT,
        )
        .await
        .unwrap();
        let resolved = crate::cloud::locations::resolve(&cfg, CLIENT, &self.game.layout);
        pull(
            &Inputs {
                game: &self.game,
                client_id: CLIENT,
                resolved: &resolved,
                source,
                auth: &auth,
                storage: &storage,
                record_dir: &self.records,
            },
            opts,
            host,
            decide,
        )
        .await
    }

    /// Nothing but GETs ever reached the mock storage.
    async fn only_reads(&self) {
        for r in self.server.received_requests().await.unwrap() {
            assert_eq!(r.method.as_str(), "GET", "{}", r.url);
        }
    }

    fn backups(&self) -> PathBuf {
        backup::game_dir(&self.records, 5)
    }
}

fn opts() -> PullOptions {
    PullOptions {
        dry_run: false,
        keep_backups: 20,
        now: chrono::DateTime::parse_from_rfc3339("2026-11-14T10:15:00Z")
            .unwrap()
            .to_utc(),
        stop_before_rename: None,
    }
}

fn state_of<'a>(r: &'a PullReport, name: &str) -> &'a State {
    &r.files
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("{name} not in {:?}", r.files))
        .state
}

fn uploaded() -> std::time::SystemTime {
    chrono::DateTime::parse_from_rfc3339(UPLOADED)
        .unwrap()
        .to_utc()
        .into()
}

#[tokio::test]
async fn a_first_pull_adds_files_without_a_question_a_snapshot_or_a_backup() {
    let f = fixture().await;
    f.write("only-local.sav", b"mine");
    f.serve(&[
        ("saves/Slot 1/theirs.sav", gzip(b"theirs")),
        ("saves/gone.sav", gzip(b"")),
    ])
    .await;
    let mut host = FakeHost::default();
    let mut d = Script::new(Choice::Skip, Confirm::No);
    let r = f.pull(&opts(), &mut host, &mut d).await.unwrap();
    assert!(r.error.is_none(), "{:?}", r.stopped);
    assert_eq!(state_of(&r, "saves/Slot 1/theirs.sav"), &State::Done);
    assert_eq!(state_of(&r, "saves/only-local.sav"), &State::ForPush);
    assert_eq!(f.read("Slot 1/theirs.sav").unwrap(), b"theirs");
    let m = fs::metadata(f.saves.join("Slot 1/theirs.sav")).unwrap();
    assert_eq!(m.modified().unwrap(), uploaded(), "the uploader's mtime");
    assert!(d.consents.is_empty(), "only adds: no question");
    assert_eq!(*host.log.borrow(), ["lock"], "no snapshot");
    assert!(r.backup_dir.is_none() && !f.backups().exists());
    let rec = f.record().unwrap();
    let e = &rec.files["saves/Slot 1/theirs.sav"];
    assert_eq!(e.cloud_hash, md5_hex(&gzip(b"theirs")));
    assert_eq!(e.local_md5, md5_hex(b"theirs"));
    assert_eq!(e.mtime, record::datetime(uploaded()));
    assert!(!rec.files.contains_key("saves/only-local.sav"));
    assert!(rec.last_sync.is_some());
    f.only_reads().await;

    // Run again: nothing to do, the file is recorded.
    let r = f
        .pull(&opts(), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert!(
        r.files.iter().all(|x| x.state == State::ForPush),
        "{:?}",
        r.files
    );
}

#[tokio::test]
async fn an_overwrite_asks_snapshots_backs_up_then_writes() {
    let f = fixture().await;
    f.write("a.sav", b"old");
    f.write("empty.sav", b"");
    let mut rec = Record::new(CLIENT);
    f.recorded(&mut rec, "a.sav", b"old");
    f.recorded(&mut rec, "empty.sav", b"");
    f.save_record(&rec);
    // `a` changed in the cloud; `empty` lists as an empty gzip, which is a
    // tombstone: a deletion, though it may be someone's real empty save.
    f.serve(&[
        ("saves/a.sav", gzip(b"new")),
        ("saves/empty.sav", gzip(b"")),
    ])
    .await;
    let mut host = FakeHost {
        watch: Some(f.saves.join("a.sav")),
        ..Default::default()
    };
    let mut d = Script::new(Choice::Skip, Confirm::Yes);
    let r = f.pull(&opts(), &mut host, &mut d).await.unwrap();
    assert!(r.error.is_none(), "{:?}", r.stopped);
    assert_eq!(d.consents, [(1, 1)]);
    assert_eq!(*host.log.borrow(), ["lock", "snapshot:old"]);
    assert_eq!(
        r.snapshot.as_deref(),
        Some("snapshot 3 taken (before cloud-save download)")
    );
    assert_eq!(f.read("a.sav").unwrap(), b"new");
    assert!(f.read("empty.sav").is_none(), "deleted");
    let dir = r.backup_dir.clone().unwrap();
    assert_eq!(dir, f.backups().join("20261114T101500Z"));
    assert_eq!(fs::read(dir.join("local/saves/a.sav")).unwrap(), b"old");
    assert_eq!(
        fs::read(dir.join("local/saves/empty.sav")).unwrap(),
        b"",
        "empty ones too"
    );
    let rec = f.record().unwrap();
    assert_eq!(rec.files["saves/a.sav"].local_md5, md5_hex(b"new"));
    assert!(!rec.files.contains_key("saves/empty.sav"));
    f.only_reads().await;
}

#[tokio::test]
async fn without_consent_or_with_a_failed_snapshot_nothing_changes() {
    let f = fixture().await;
    f.write("a.sav", b"old");
    let mut rec = Record::new(CLIENT);
    f.recorded(&mut rec, "a.sav", b"old");
    f.save_record(&rec);
    f.serve(&[("saves/a.sav", gzip(b"new")), ("saves/b.sav", gzip(b"b"))])
        .await;
    let before = fs::read_to_string(record::path(&f.records, 5)).unwrap();

    let mut d = Script::new(Choice::Skip, Confirm::CannotAsk);
    let r = f
        .pull(&opts(), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert!(r.needs_consent && !r.declined);
    assert_eq!(state_of(&r, "saves/a.sav"), &State::NotDone);
    assert_eq!(state_of(&r, "saves/b.sav"), &State::NotDone);

    let mut d = Script::new(Choice::Skip, Confirm::No);
    let r = f
        .pull(&opts(), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert!(r.declined && !r.needs_consent);

    let mut host = FakeHost {
        fail_snapshot: true,
        ..Default::default()
    };
    let mut d = Script::new(Choice::Skip, Confirm::Yes);
    let e = f.pull(&opts(), &mut host, &mut d).await.err().unwrap();
    assert_eq!(e.exit_code(), 7);

    let mut host = FakeHost {
        refuse: true,
        ..Default::default()
    };
    let e = f.pull(&opts(), &mut host, &mut d).await.err().unwrap();
    assert_eq!(e.exit_code(), 7);

    let held = SyncLock::acquire(&f.records, 5).unwrap();
    let mut host = FakeHost::default();
    let e = f.pull(&opts(), &mut host, &mut d).await.err().unwrap();
    assert_eq!(e.exit_code(), 7);
    assert!(host.log.borrow().is_empty(), "the sync lock comes first");
    drop(held);

    assert_eq!(f.read("a.sav").unwrap(), b"old");
    assert!(f.read("b.sav").is_none());
    assert!(!f.backups().exists());
    assert_eq!(
        fs::read_to_string(record::path(&f.records, 5)).unwrap(),
        before
    );
}

#[tokio::test]
async fn a_dry_run_writes_nothing_and_takes_no_lock() {
    let f = fixture().await;
    f.write("a.sav", b"old");
    f.serve(&[("saves/a.sav", gzip(b"new")), ("saves/b.sav", gzip(b"b"))])
        .await;
    let mut host = FakeHost::default();
    let mut d = Script::new(Choice::Cloud, Confirm::Yes);
    let r = f
        .pull(
            &PullOptions {
                dry_run: true,
                ..opts()
            },
            &mut host,
            &mut d,
        )
        .await
        .unwrap();
    assert_eq!(state_of(&r, "saves/a.sav"), &State::Planned);
    assert_eq!(
        r.files.iter().find(|x| x.name == "saves/a.sav").unwrap().op,
        Some(Op::Overwrite)
    );
    assert_eq!(state_of(&r, "saves/b.sav"), &State::Planned);
    assert!(host.log.borrow().is_empty() && d.consents.is_empty());
    assert!(!f.records.exists(), "no lock file, no record");
    assert_eq!(f.read("a.sav").unwrap(), b"old");
    assert!(f.read("b.sav").is_none());
}

#[tokio::test]
async fn conflicts_are_left_or_settled_each_way() {
    let f = fixture().await;
    for n in ["skip", "cloud", "local", "both"] {
        f.write(&format!("{n}.sav"), b"local");
    }
    f.serve(&[
        ("saves/skip.sav", gzip(b"cloud")),
        ("saves/cloud.sav", gzip(b"cloud")),
        ("saves/local.sav", gzip(b"cloud")),
        ("saves/both.sav", gzip(b"cloud")),
    ])
    .await;
    struct ByName(Vec<String>);
    impl Decide for ByName {
        fn conflict(&mut self, file: &FilePlan, choices: &[Choice]) -> Choice {
            self.0.push(file.name.clone());
            let c = match file.name.as_str() {
                "saves/cloud.sav" => Choice::Cloud,
                "saves/local.sav" => Choice::Local,
                "saves/both.sav" => Choice::Both,
                _ => Choice::Skip,
            };
            assert!(choices.contains(&c));
            c
        }
        fn confirm(&mut self, _: &Consent) -> Confirm {
            Confirm::Yes
        }
    }
    let mut d = ByName(vec![]);
    let r = f
        .pull(&opts(), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert_eq!(d.0.len(), 4, "every first-sync difference is asked about");
    assert_eq!(
        state_of(&r, "saves/skip.sav"),
        &State::Left(LeftReason::Undecided)
    );
    assert_eq!(state_of(&r, "saves/local.sav"), &State::KeptLocal);
    assert_eq!(state_of(&r, "saves/cloud.sav"), &State::Done);
    assert_eq!(state_of(&r, "saves/both.sav"), &State::Done);
    assert_eq!(r.left(), 1);
    assert_eq!(f.read("skip.sav").unwrap(), b"local");
    assert_eq!(f.read("local.sav").unwrap(), b"local");
    assert_eq!(f.read("cloud.sav").unwrap(), b"cloud");
    assert_eq!(f.read("both.sav").unwrap(), b"cloud");
    assert_eq!(f.read("both.sav.local-20261114T101500Z").unwrap(), b"local");
    let dir = r.backup_dir.unwrap();
    assert_eq!(
        fs::read(dir.join("local/saves/cloud.sav")).unwrap(),
        b"local"
    );
    let rec = f.record().unwrap();
    let keys: Vec<_> = rec.files.keys().cloned().collect();
    assert_eq!(keys, ["saves/both.sav", "saves/cloud.sav"]);
}

#[tokio::test]
async fn a_pull_stopped_before_a_rename_leaves_the_old_file_and_its_backup() {
    let f = fixture().await;
    f.write("a.sav", b"old a");
    f.write("b.sav", b"old b");
    let mut rec = Record::new(CLIENT);
    f.recorded(&mut rec, "a.sav", b"old a");
    f.recorded(&mut rec, "b.sav", b"old b");
    f.save_record(&rec);
    f.serve(&[
        ("saves/a.sav", gzip(b"new a")),
        ("saves/b.sav", gzip(b"new b")),
    ])
    .await;
    let mut d = Script::new(Choice::Skip, Confirm::Yes);
    let r = f
        .pull(
            &PullOptions {
                stop_before_rename: Some("saves/b.sav".into()),
                ..opts()
            },
            &mut FakeHost::default(),
            &mut d,
        )
        .await
        .unwrap();
    assert_eq!(r.error.as_ref().map(CloudError::exit_code), Some(6));
    assert_eq!(state_of(&r, "saves/a.sav"), &State::Done);
    assert_eq!(state_of(&r, "saves/b.sav"), &State::NotDone);
    assert_eq!(f.read("a.sav").unwrap(), b"new a");
    assert_eq!(f.read("b.sav").unwrap(), b"old b", "the old file, whole");
    let dir = r.backup_dir.clone().unwrap();
    assert_eq!(fs::read(dir.join("local/saves/b.sav")).unwrap(), b"old b");
    let temps: Vec<_> = fs::read_dir(&f.saves)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(write::TEMP_PREFIX))
        .collect();
    assert_eq!(temps.len(), 1, "the crash's temp file");
    // The record has `a` done, `b` as it was.
    let rec = f.record().unwrap();
    assert_eq!(rec.files["saves/a.sav"].local_md5, md5_hex(b"new a"));
    assert_eq!(rec.files["saves/b.sav"].local_md5, md5_hex(b"old b"));

    // The rerun finishes `b` and clears the temp file.
    let r = f
        .pull(&opts(), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert!(r.error.is_none(), "{:?}", r.stopped);
    assert_eq!(state_of(&r, "saves/b.sav"), &State::Done);
    assert_eq!(f.read("b.sav").unwrap(), b"new b");
    assert!(fs::read_dir(&f.saves).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(write::TEMP_PREFIX)
    }));
    assert_ne!(r.backup_dir.unwrap(), dir, "its own backup dir");
}

#[tokio::test]
async fn a_corrupt_object_is_skipped_and_the_rest_done() {
    let f = fixture().await;
    f.serve(&[("saves/a.sav", gzip(b"a")), ("saves/b.sav", gzip(b"b"))])
        .await;
    // `a` served with other bytes than listed.
    f.server.reset().await;
    Mock::given(method("GET"))
        .and(path(format!("/components/galaxy_client/clients/{CLIENT}")))
        .respond_with(ResponseTemplate::new(200).set_body_string(CONFIG))
        .mount(&f.server)
        .await;
    let listing = serde_json::json!([
        {"name": "saves/a.sav", "bytes": gzip(b"a").len(), "hash": md5_hex(&gzip(b"a"))},
        {"name": "saves/b.sav", "bytes": gzip(b"b").len(), "hash": md5_hex(&gzip(b"b"))},
    ]);
    Mock::given(method("GET"))
        .and(path(format!("/v1/7/{CLIENT}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(listing))
        .mount(&f.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/7/{CLIENT}/saves/a.sav")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(gzip(b"X")))
        .mount(&f.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/7/{CLIENT}/saves/b.sav")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(gzip(b"b")))
        .mount(&f.server)
        .await;
    let mut d = Script::new(Choice::Skip, Confirm::Yes);
    let r = f
        .pull(&opts(), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert_eq!(state_of(&r, "saves/a.sav"), &State::Corrupt);
    assert_eq!(state_of(&r, "saves/b.sav"), &State::Done);
    assert_eq!(r.corrupt(), 1);
    assert!(f.read("a.sav").is_none());
    assert!(!f.record().unwrap().files.contains_key("saves/a.sav"));
}

#[tokio::test]
async fn a_client_change_sets_the_record_aside_first() {
    let f = fixture().await;
    let mut old = Record::new("22222222222222222");
    old.rolled_back = Some("2026-11-12T10:00:00Z".parse().unwrap());
    f.save_record(&old);
    f.serve(&[("saves/a.sav", gzip(b"a"))]).await;
    let mut d = Script::new(Choice::Skip, Confirm::Yes);
    let r = f
        .pull(&opts(), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    let aside = r.record_set_aside.clone().unwrap();
    assert_eq!(
        aside,
        f.records.join("5.client-changed-20261114T101500Z.toml")
    );
    assert!(
        fs::read_to_string(&aside)
            .unwrap()
            .contains("22222222222222222")
    );
    let rec = f.record().unwrap();
    assert_eq!(rec.client_id, CLIENT);
    assert!(
        rec.rolled_back.is_none(),
        "the other client's mark isn't carried"
    );
    assert!(rec.files.contains_key("saves/a.sav"));
}

#[tokio::test]
async fn a_rollback_mark_needs_prefer_cloud_and_is_cleared_once_settled() {
    let f = fixture().await;
    f.write("a.sav", b"rolled back");
    let mut rec = Record::new(CLIENT);
    f.recorded(&mut rec, "a.sav", b"rolled back");
    // The cloud moved on from what the record says: a download, which the
    // mark turns into a conflict.
    rec.files.get_mut("saves/a.sav").unwrap().cloud_hash = md5_hex(b"older");
    rec.rolled_back = Some("2026-11-12T10:00:00Z".parse().unwrap());
    f.save_record(&rec);
    f.serve(&[("saves/a.sav", gzip(b"cloud"))]).await;

    let mut d = Script::new(Choice::Skip, Confirm::Yes);
    let r = f
        .pull(&opts(), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert_eq!(
        state_of(&r, "saves/a.sav"),
        &State::Left(LeftReason::Undecided)
    );
    assert!(!r.rolled_back_cleared);
    assert!(f.record().unwrap().rolled_back.is_some());

    let mut d = Script::new(Choice::Cloud, Confirm::Yes);
    let r = f
        .pull(&opts(), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert_eq!(state_of(&r, "saves/a.sav"), &State::Done);
    assert!(r.rolled_back_cleared);
    assert!(f.record().unwrap().rolled_back.is_none());
    assert_eq!(f.read("a.sav").unwrap(), b"cloud");
}

#[tokio::test]
async fn a_missing_save_dir_with_a_record_is_not_made_again() {
    let f = fixture().await;
    f.write("a.sav", b"a");
    let mut rec = Record::new(CLIENT);
    f.recorded(&mut rec, "a.sav", b"a");
    f.save_record(&rec);
    fs::remove_dir_all(&f.saves).unwrap();
    f.serve(&[("saves/a.sav", gzip(b"a")), ("saves/b.sav", gzip(b"b"))])
        .await;
    let mut d = Script::new(Choice::Cloud, Confirm::Yes);
    let r = f
        .pull(&opts(), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert_eq!(
        state_of(&r, "saves/a.sav"),
        &State::Left(LeftReason::LocationMissing)
    );
    assert_eq!(
        state_of(&r, "saves/b.sav"),
        &State::Left(LeftReason::LocationMissing)
    );
    assert!(!f.saves.exists());
}

#[tokio::test]
async fn without_a_prefix_nothing_is_made() {
    let f = fixture().await;
    fs::remove_dir_all(f.game.layout.prefix.join("drive_c")).unwrap();
    f.serve(&[("saves/a.sav", gzip(b"a"))]).await;
    let mut d = Script::new(Choice::Cloud, Confirm::Yes);
    let r = f
        .pull(&opts(), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert_eq!(
        state_of(&r, "saves/a.sav"),
        &State::Left(LeftReason::NoPrefix)
    );
    assert!(!f.game.layout.prefix.join("drive_c").exists());
}

#[tokio::test]
async fn a_file_changed_after_the_plan_is_left_and_backups_are_pruned() {
    let f = fixture().await;
    f.write("a.sav", b"old");
    let mut rec = Record::new(CLIENT);
    f.recorded(&mut rec, "a.sav", b"old");
    f.save_record(&rec);
    f.serve(&[("saves/a.sav", gzip(b"new"))]).await;
    for n in ["20261101T000000Z", "20261102T000000Z", "20261103T000000Z"] {
        fs::create_dir_all(f.backups().join(n)).unwrap();
    }
    // The question is where a user could change the file meanwhile.
    struct Touch(PathBuf);
    impl Decide for Touch {
        fn conflict(&mut self, _: &FilePlan, _: &[Choice]) -> Choice {
            Choice::Skip
        }
        fn confirm(&mut self, _: &Consent) -> Confirm {
            fs::write(&self.0, b"played meanwhile").unwrap();
            Confirm::Yes
        }
    }
    let r = f
        .pull(
            &PullOptions {
                keep_backups: 2,
                ..opts()
            },
            &mut FakeHost::default(),
            &mut Touch(f.saves.join("a.sav")),
        )
        .await
        .unwrap();
    assert_eq!(
        state_of(&r, "saves/a.sav"),
        &State::Left(LeftReason::ChangedSincePlan)
    );
    assert_eq!(f.read("a.sav").unwrap(), b"played meanwhile");
    // Nothing backed up, so nothing pruned.
    assert!(r.pruned.is_empty());
    assert_eq!(fs::read_dir(f.backups()).unwrap().count(), 3);

    // A run that backs up prunes to the newest two (its own included).
    let mut d = Script::new(Choice::Cloud, Confirm::Yes);
    let r = f
        .pull(&opts_keep(2), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert_eq!(f.read("a.sav").unwrap(), b"new");
    assert_eq!(r.pruned.len(), 2);
    let mut left: Vec<_> = fs::read_dir(f.backups())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, ["20261103T000000Z", "20261114T101500Z"]);
}

fn opts_keep(n: usize) -> PullOptions {
    PullOptions {
        keep_backups: n,
        ..opts()
    }
}

#[tokio::test]
async fn a_dir_swapped_for_a_link_after_the_plan_is_never_followed() {
    let f = fixture().await;
    f.write("Slot/a.sav", b"old");
    let mut rec = Record::new(CLIENT);
    f.recorded(&mut rec, "Slot/a.sav", b"old");
    f.save_record(&rec);
    f.serve(&[("saves/Slot/a.sav", gzip(b"new"))]).await;
    let outside = f.game.root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("a.sav"), b"not a save").unwrap();
    // The question is the last moment before the writes: the dir becomes
    // a link to elsewhere then.
    struct Swap(PathBuf, PathBuf);
    impl Decide for Swap {
        fn conflict(&mut self, _: &FilePlan, _: &[Choice]) -> Choice {
            Choice::Skip
        }
        fn confirm(&mut self, _: &Consent) -> Confirm {
            fs::rename(&self.0, self.0.with_extension("real")).unwrap();
            std::os::unix::fs::symlink(&self.1, &self.0).unwrap();
            Confirm::Yes
        }
    }
    let r = f
        .pull(
            &opts(),
            &mut FakeHost::default(),
            &mut Swap(f.saves.join("Slot"), outside.clone()),
        )
        .await
        .unwrap();
    assert_eq!(
        r.error.as_ref().map(CloudError::exit_code),
        Some(1),
        "{:?}",
        r.stopped
    );
    assert_eq!(state_of(&r, "saves/Slot/a.sav"), &State::NotDone);
    assert_eq!(fs::read(outside.join("a.sav")).unwrap(), b"not a save");
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
    assert_eq!(fs::read(f.saves.join("Slot.real/a.sav")).unwrap(), b"old");
}

/// The live host against design 20's fake backend: the `cloud-pull`
/// snapshot under the game's lock, `--no-snapshots`, and a game another
/// gogo holds.
#[test]
fn the_live_host_takes_a_cloud_pull_snapshot_under_the_game_lock() {
    use crate::linuxapp::snapshots::{self as snaps, AutoOptions, Backends};
    const ID: &str = "1999999903";
    let _env = crate::test_util::env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("GOGO_HOME");
    // SAFETY: under the env mutex; restored below.
    unsafe { std::env::set_var("GOGO_HOME", tmp.path()) };
    let root = tmp.path().join("games").join(ID);
    fs::create_dir_all(root.join("prefix/drive_c/users/steamuser/Documents")).unwrap();
    fs::create_dir_all(root.join("game")).unwrap();
    fs::write(root.join("gogo-launch.toml"), "").unwrap();
    let b = Backends::fixed(std::sync::Arc::new(crate::snapshot::fake::Fake::new()));
    snaps::set_auto(ID, &b, true, 5).unwrap();
    let game = Game {
        gogid: ID.into(),
        layout: Layout {
            lane: Lane::Linux,
            prefix: root.join("prefix"),
            game_files: root.join("game"),
        },
        root: root.clone(),
    };
    let auto = |skip| AutoOptions {
        skip,
        keep_auto: 5,
        backends: b.clone(),
    };
    let mut events = vec![];
    let mut emit = |e: &crate::engine::events::ProgressEvent| events.push(format!("{e:?}"));
    {
        let mut host = LiveHost::new(&game, auto(false), &mut emit);
        host.lock().unwrap();
        // Another gogo can't change the game meanwhile.
        let other = crate::macapp::relocate::lock_game(ID);
        assert!(other.is_err());
        let mut quiet = |_: &crate::engine::events::ProgressEvent| {};
        let mut second = LiveHost::new(&game, auto(false), &mut quiet);
        assert_eq!(second.lock().err().unwrap().exit_code(), 7);
        let line = host.snapshot().unwrap().unwrap();
        assert!(line.contains("before cloud-save download"), "{line}");
    }
    let v = snaps::list(ID, &b, 5, false).unwrap();
    let s = v.snapshots.last().unwrap();
    assert_eq!(s.trigger, Some("cloud-pull"));
    assert_eq!(s.label.as_deref(), Some("before cloud-save download"));
    assert!(
        events.iter().any(|e| e.contains("SnapshotCreated")),
        "{events:?}"
    );

    let mut quiet = |_: &crate::engine::events::ProgressEvent| {};
    let mut host = LiveHost::new(&game, auto(true), &mut quiet);
    host.lock().unwrap();
    assert_eq!(host.snapshot().unwrap(), None, "--no-snapshots");
    drop(host);
    // SAFETY: as above.
    unsafe {
        match old {
            Some(h) => std::env::set_var("GOGO_HOME", h),
            None => std::env::remove_var("GOGO_HOME"),
        }
    }
}
