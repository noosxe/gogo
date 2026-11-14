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

type Object = (&'static str, Vec<u8>);

struct Fixture {
    _dir: tempfile::TempDir,
    config: &'static str,
    game: Game,
    cache: PathBuf,
    records: PathBuf,
    saves: PathBuf,
    server: MockServer,
    exchange: FixedExchange,
    session: MainSession,
}

async fn fixture() -> Fixture {
    fixture_with(CONFIG).await
}

async fn fixture_with(config: &'static str) -> Fixture {
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
        .respond_with(ResponseTemplate::new(200).set_body_string(config))
        .mount(&server)
        .await;
    Fixture {
        config,
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
        self.serve_headers(objects, &[]).await
    }

    /// [`Fixture::serve`] with more container headers (the quota).
    async fn serve_headers(&self, objects: &[(&str, Vec<u8>)], headers: &[(&str, &str)]) {
        self.serve_full(objects, headers, None).await
    }

    /// [`Fixture::serve_headers`], the listing answering with `after`
    /// from its `n`-th request on (a cloud that changed meanwhile).
    async fn serve_full(
        &self,
        objects: &[(&str, Vec<u8>)],
        headers: &[(&str, &str)],
        after: Option<(u64, &[Object])>,
    ) {
        self.server.reset().await;
        Mock::given(method("GET"))
            .and(path(format!("/components/galaxy_client/clients/{CLIENT}")))
            .respond_with(ResponseTemplate::new(200).set_body_string(self.config))
            .mount(&self.server)
            .await;
        let listing: Vec<serde_json::Value> = objects
            .iter()
            .map(|(n, b)| serde_json::json!({"name": n, "bytes": b.len(), "hash": md5_hex(b)}))
            .collect();
        let mut resp = ResponseTemplate::new(200)
            .insert_header("x-container-object-count", objects.len().to_string());
        for (k, v) in headers {
            resp = resp.insert_header(*k, *v);
        }
        let mut first = Mock::given(method("GET"))
            .and(path(format!("/v1/7/{CLIENT}")))
            .respond_with(resp.set_body_json(listing));
        if let Some((n, later)) = after {
            first = first.up_to_n_times(n);
            let later_listing: Vec<serde_json::Value> = later
                .iter()
                .map(|(n, b)| serde_json::json!({"name": n, "bytes": b.len(), "hash": md5_hex(b)}))
                .collect();
            first.mount(&self.server).await;
            Mock::given(method("GET"))
                .and(path(format!("/v1/7/{CLIENT}")))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("x-container-object-count", later.len().to_string())
                        .set_body_json(later_listing),
                )
                .mount(&self.server)
                .await;
        } else {
            first.mount(&self.server).await;
        }
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
        let mut resolved = crate::cloud::locations::resolve(&cfg, CLIENT, &self.game.layout);
        crate::cloud::build_files::apply_offline(&mut resolved, &self.game);
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
        mode: Mode::Pull,
        dry_run: false,
        no_delete: false,
        cloud_writes: false,
        newer: false,
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
    f.serve(&[
        ("saves/Slot/a.sav", gzip(b"new")),
        ("saves/other.sav", gzip(b"other")),
    ])
    .await;
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
    assert!(r.error.is_none(), "{:?}", r.stopped);
    assert_eq!(
        state_of(&r, "saves/Slot/a.sav"),
        &State::Left(LeftReason::PathRefused)
    );
    assert_eq!(
        state_of(&r, "saves/other.sav"),
        &State::Done,
        "the run goes on"
    );
    assert_eq!(r.left(), 1);
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

/// A `both` answer, on one file.
struct BothOn(&'static str, Option<PathBuf>);
impl Decide for BothOn {
    fn conflict(&mut self, file: &FilePlan, _: &[Choice]) -> Choice {
        if let Some(dir) = &self.1 {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).unwrap();
        }
        if file.name == self.0 {
            Choice::Both
        } else {
            Choice::Skip
        }
    }
    fn confirm(&mut self, _: &Consent) -> Confirm {
        Confirm::Yes
    }
}

#[tokio::test]
async fn both_with_a_corrupt_object_leaves_the_local_file_where_it_was() {
    let f = fixture().await;
    // Synced once, then changed on both sides: a conflict the plan doesn't
    // fetch for, so the bad object is only found while writing.
    f.write("a.sav", b"base");
    let mut rec = Record::new(CLIENT);
    f.recorded(&mut rec, "a.sav", b"base");
    f.save_record(&rec);
    f.write("a.sav", b"mine, changed");
    // Listed with the hash of what is served, but not a gzip stream.
    f.server.reset().await;
    Mock::given(method("GET"))
        .and(path(format!("/components/galaxy_client/clients/{CLIENT}")))
        .respond_with(ResponseTemplate::new(200).set_body_string(CONFIG))
        .mount(&f.server)
        .await;
    let junk = b"not gzip at all".to_vec();
    Mock::given(method("GET"))
        .and(path(format!("/v1/7/{CLIENT}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"name": "saves/a.sav", "bytes": junk.len(), "hash": md5_hex(&junk)}
        ])))
        .mount(&f.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/7/{CLIENT}/saves/a.sav")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(junk))
        .mount(&f.server)
        .await;
    let r = f
        .pull(
            &opts(),
            &mut FakeHost::default(),
            &mut BothOn("saves/a.sav", None),
        )
        .await
        .unwrap();
    let file = r.files.iter().find(|x| x.name == "saves/a.sav").unwrap();
    assert_eq!(file.choice, Some(Choice::Both));
    assert_eq!(file.state, State::Corrupt);
    assert!(r.error.is_none(), "the run goes on");
    assert_eq!(f.read("a.sav").unwrap(), b"mine, changed");
    assert_eq!(
        fs::read_dir(&f.saves).unwrap().count(),
        1,
        "nothing beside it"
    );
    assert_eq!(
        f.record().unwrap().files["saves/a.sav"].local_md5,
        md5_hex(b"base"),
        "the record untouched"
    );
}

#[tokio::test]
async fn both_whose_write_fails_leaves_the_local_file_where_it_was_and_backed_up() {
    let f = fixture().await;
    f.write("a.sav", b"mine");
    f.serve(&[("saves/a.sav", gzip(b"cloud"))]).await;
    // Can this process be refused a write at all (not as root)?
    {
        use std::os::unix::fs::PermissionsExt;
        let probe = f.game.root.join("probe");
        fs::create_dir(&probe).unwrap();
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o555)).unwrap();
        let refused = fs::write(probe.join("x"), b"").is_err();
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o755)).unwrap();
        if !refused {
            return;
        }
    }
    // The save dir turns read-only while the conflict is answered.
    let r = f
        .pull(
            &opts(),
            &mut FakeHost::default(),
            &mut BothOn("saves/a.sav", Some(f.saves.clone())),
        )
        .await
        .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&f.saves, fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert_eq!(r.error.as_ref().map(CloudError::exit_code), Some(6));
    assert_eq!(state_of(&r, "saves/a.sav"), &State::NotDone);
    assert_eq!(f.read("a.sav").unwrap(), b"mine");
    assert_eq!(
        fs::read_dir(&f.saves).unwrap().count(),
        1,
        "no kept copy, no temp"
    );
    let dir = r.backup_dir.unwrap();
    assert_eq!(fs::read(dir.join("local/saves/a.sav")).unwrap(), b"mine");
    assert!(
        f.record()
            .is_none_or(|r| !r.files.contains_key("saves/a.sav"))
    );
}

#[tokio::test]
async fn both_is_backed_up_and_keeps_a_free_name() {
    let f = fixture().await;
    f.write("a.sav", b"mine");
    f.write("a.sav.local-20261114T101500Z", b"an earlier both");
    f.serve(&[("saves/a.sav", gzip(b"cloud"))]).await;
    let r = f
        .pull(
            &opts(),
            &mut FakeHost::default(),
            &mut BothOn("saves/a.sav", None),
        )
        .await
        .unwrap();
    assert_eq!(state_of(&r, "saves/a.sav"), &State::Done);
    let kept = r.files.iter().find(|x| x.name == "saves/a.sav").unwrap();
    assert_eq!(
        kept.kept_as.as_deref(),
        Some("a.sav.local-20261114T101500Z-2")
    );
    assert_eq!(f.read("a.sav").unwrap(), b"cloud");
    assert_eq!(f.read("a.sav.local-20261114T101500Z-2").unwrap(), b"mine");
    assert_eq!(
        f.read("a.sav.local-20261114T101500Z").unwrap(),
        b"an earlier both"
    );
    let dir = r.backup_dir.unwrap();
    assert_eq!(fs::read(dir.join("local/saves/a.sav")).unwrap(), b"mine");
}

#[tokio::test]
async fn a_client_change_with_a_failed_snapshot_leaves_the_record_in_place() {
    let f = fixture().await;
    f.write("a.sav", b"local");
    let old = Record::new("22222222222222222");
    f.save_record(&old);
    let before = fs::read_to_string(record::path(&f.records, 5)).unwrap();
    f.serve(&[("saves/a.sav", gzip(b"cloud"))]).await;
    let mut host = FakeHost {
        fail_snapshot: true,
        ..Default::default()
    };
    let mut d = Script::new(Choice::Cloud, Confirm::Yes);
    let e = f.pull(&opts(), &mut host, &mut d).await.err().unwrap();
    assert_eq!(e.exit_code(), 7);
    assert_eq!(
        fs::read_to_string(record::path(&f.records, 5)).unwrap(),
        before
    );
    let names: Vec<_> = fs::read_dir(&f.records)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|n| !n.contains("client-changed")),
        "{names:?}"
    );
    assert_eq!(f.read("a.sav").unwrap(), b"local");
}

#[tokio::test]
async fn an_implausible_uploader_mtime_is_ignored_with_a_note() {
    let f = fixture().await;
    f.server.reset().await;
    Mock::given(method("GET"))
        .and(path(format!("/components/galaxy_client/clients/{CLIENT}")))
        .respond_with(ResponseTemplate::new(200).set_body_string(CONFIG))
        .mount(&f.server)
        .await;
    let body = gzip(b"a");
    Mock::given(method("GET"))
        .and(path(format!("/v1/7/{CLIENT}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"name": "saves/a.sav", "bytes": body.len(), "hash": md5_hex(&body)}
        ])))
        .mount(&f.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/7/{CLIENT}/saves/a.sav")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-object-meta-locallastmodified", "1901-01-01T00:00:00Z")
                .set_body_bytes(body),
        )
        .mount(&f.server)
        .await;
    let start = std::time::SystemTime::now() - std::time::Duration::from_secs(5);
    let mut d = Script::new(Choice::Skip, Confirm::Yes);
    let r = f
        .pull(&opts(), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert_eq!(state_of(&r, "saves/a.sav"), &State::Done);
    let m = fs::metadata(f.saves.join("a.sav"))
        .unwrap()
        .modified()
        .unwrap();
    assert!(m >= start, "the time of the download");
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("1901-01-01T00:00:00Z")),
        "{:?}",
        r.warnings
    );
}

#[test]
fn a_failing_writer_is_a_disk_error_not_a_corrupt_object() {
    struct Full;
    impl std::io::Write for Full {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::StorageFull))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut md5 = md5::Md5::default();
    let e = gunzip_to("saves/a", &gzip(b"some save"), &mut Full, &mut md5).unwrap_err();
    assert!(
        matches!(&e, Fill::Disk(io) if io.kind() == std::io::ErrorKind::StorageFull),
        "{e:?}"
    );
    let mut out = vec![];
    let e = gunzip_to("saves/a", b"not gzip", &mut out, &mut md5).unwrap_err();
    assert!(matches!(e, Fill::Corrupt(_)), "{e:?}");
}

#[test]
fn fetches_are_batched_within_the_budget() {
    let e = |n: &str, bytes: u64| Entry {
        name: n.into(),
        bytes,
        hash: "0".repeat(32),
        last_modified: None,
    };
    let mib = 1024 * 1024;
    let mut q: std::collections::VecDeque<(usize, Entry)> = [
        (0, e("a", 10)),
        (1, e("b", 10)),
        (2, e("c", 10)),
        (3, e("d", 10)),
        (4, e("e", 10)),
        (5, e("big", 200 * mib)),
        (6, e("big2", 200 * mib)),
        (7, e("huge", 900 * mib)),
    ]
    .into();
    let ids = |b: Vec<(usize, Entry)>| b.into_iter().map(|(k, _)| k).collect::<Vec<_>>();
    assert_eq!(ids(next_batch(&mut q)), [0, 1, 2, 3], "four at a time");
    assert_eq!(ids(next_batch(&mut q)), [4, 5], "within the budget");
    assert_eq!(ids(next_batch(&mut q)), [6]);
    assert_eq!(ids(next_batch(&mut q)), [7], "one alone always goes");
    assert!(next_batch(&mut q).is_empty());
}

const INSTALL_CONFIG: &str = r#"{"content":{"Windows":{"cloudStorage":{"enabled":true,
    "locations":[{"name":"game","location":"<?INSTALL?>"}]}}}}"#;

#[tokio::test]
async fn build_files_and_hard_linked_files_in_the_game_dir_are_never_written() {
    let mut f = fixture_with(INSTALL_CONFIG).await;
    let gf = f.game.layout.prefix.join("drive_c/GOG/G");
    fs::create_dir_all(gf.join("SAVE")).unwrap();
    f.game.layout.game_files = gf.clone();
    fs::write(
        f.game.root.join("content.toml"),
        "schema = 1\nlane = \"installer\"\nbuild_id = \"b1\"\n",
    )
    .unwrap();
    fs::create_dir_all(f.game.root.join("install")).unwrap();
    fs::write(
        f.game.root.join("install/inno.log"),
        "Dest filename: C:\\GOG\\G\\Game.exe\r\n",
    )
    .unwrap();
    fs::write(gf.join("Game.exe"), b"exe").unwrap();
    fs::write(gf.join("SAVE/keep.sav"), b"shared").unwrap();
    fs::hard_link(gf.join("SAVE/keep.sav"), gf.join("SAVE/hl.sav")).unwrap();
    f.serve(&[
        ("game/Game.exe", gzip(b"another exe")),
        ("game/SAVE/hl.sav", gzip(b"cloud")),
        ("game/SAVE/slot.sav", gzip(b"slot")),
    ])
    .await;
    let mut d = Script::new(Choice::Cloud, Confirm::Yes);
    let r = f
        .pull(&opts(), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert!(r.error.is_none(), "{:?}", r.stopped);
    let names: Vec<_> = r.files.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(names, ["game/SAVE/slot.sav"], "{:?}", r.plan.unmapped);
    assert_eq!(fs::read(gf.join("SAVE/slot.sav")).unwrap(), b"slot");
    assert_eq!(fs::read(gf.join("Game.exe")).unwrap(), b"exe");
    assert_eq!(fs::read(gf.join("SAVE/hl.sav")).unwrap(), b"shared");
    assert_eq!(fs::read(gf.join("SAVE/keep.sav")).unwrap(), b"shared");
}

// ---- push and sync (#462) -------------------------------------------------

fn mode_opts(mode: Mode) -> PullOptions {
    PullOptions {
        mode,
        cloud_writes: true,
        ..opts()
    }
}

impl Fixture {
    /// Accept every `PUT` and `DELETE` the way the storage API does.
    async fn accept_writes(&self) {
        let any = || wiremock::matchers::path_regex(format!("^/v1/7/{CLIENT}/.+"));
        Mock::given(method("PUT"))
            .and(any())
            .respond_with(ResponseTemplate::new(201))
            .mount(&self.server)
            .await;
        Mock::given(method("DELETE"))
            .and(any())
            .respond_with(ResponseTemplate::new(204))
            .mount(&self.server)
            .await;
    }

    async fn requests(&self, verb: &str) -> Vec<wiremock::Request> {
        self.server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.method.as_str() == verb)
            .collect()
    }
}

fn gunzip(b: &[u8]) -> Vec<u8> {
    let mut out = vec![];
    storage::gunzip_into("x", b, &mut out, 1 << 20).unwrap();
    out
}

#[tokio::test]
async fn a_push_uploads_gzip_with_etag_and_the_local_mtime() {
    let f = fixture().await;
    f.write("Slot 1/new.sav", b"my save");
    f.write("noise.log", b"never");
    f.write("half.tmp", b"never");
    f.serve(&[("saves/theirs.sav", gzip(b"theirs"))]).await;
    f.accept_writes().await;
    let mtime = fs::metadata(f.saves.join("Slot 1/new.sav"))
        .unwrap()
        .modified()
        .unwrap();
    let mut d = Script::new(Choice::Skip, Confirm::No);
    let r = f
        .pull(&mode_opts(Mode::Push), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert!(r.error.is_none(), "{:?}", r.stopped);
    assert_eq!(state_of(&r, "saves/Slot 1/new.sav"), &State::Done);
    assert_eq!(
        state_of(&r, "saves/theirs.sav"),
        &State::ForPull,
        "a push doesn't download"
    );
    assert!(f.read("theirs.sav").is_none());
    let puts = f.requests("PUT").await;
    assert_eq!(puts.len(), 1, "excluded files are never uploaded");
    let put = &puts[0];
    assert!(put.url.path().ends_with("/saves/Slot%201/new.sav"));
    assert_eq!(put.headers["content-encoding"], "gzip");
    assert_eq!(put.headers["etag"].to_str().unwrap(), md5_hex(&put.body));
    assert_eq!(put.body, gzip(b"my save"), "gogo's deterministic gzip");
    assert_eq!(gunzip(&put.body), b"my save");
    let stamp = chrono::DateTime::<chrono::Utc>::from(mtime)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    assert_eq!(
        put.headers["x-object-meta-locallastmodified"],
        stamp.as_str()
    );
    let rec = f.record().unwrap();
    let e = &rec.files["saves/Slot 1/new.sav"];
    assert_eq!(e.cloud_hash, md5_hex(&gzip(b"my save")));
    assert_eq!(e.local_md5, md5_hex(b"my save"));
    assert!(d.consents.is_empty(), "a new object overwrites nothing");
    assert!(r.backup_dir.is_none());
}

#[tokio::test]
async fn the_cloud_write_gate_refuses_a_real_run_and_a_dry_run_only_reads() {
    let f = fixture().await;
    f.write("new.sav", b"mine");
    let mut rec = Record::new(CLIENT);
    f.write("gone-soon.sav", b"x");
    f.recorded(&mut rec, "gone-soon.sav", b"x");
    f.save_record(&rec);
    fs::remove_file(f.saves.join("gone-soon.sav")).unwrap();
    f.serve(&[("saves/gone-soon.sav", gzip(b"x"))]).await;
    f.accept_writes().await;
    let mut d = Script::new(Choice::Skip, Confirm::Yes);
    for mode in [Mode::Push, Mode::Sync] {
        let gate = PullOptions {
            cloud_writes: false,
            ..mode_opts(mode)
        };
        let e = f
            .pull(&gate, &mut FakeHost::default(), &mut d)
            .await
            .err()
            .unwrap();
        assert_eq!(e.exit_code(), 7);
        assert!(e.to_string().contains("nothing was changed"));
        let dry = PullOptions {
            dry_run: true,
            ..gate
        };
        let r = f
            .pull(&dry, &mut FakeHost::default(), &mut d)
            .await
            .unwrap();
        assert_eq!(state_of(&r, "saves/new.sav"), &State::Planned);
        assert_eq!(state_of(&r, "saves/gone-soon.sav"), &State::Planned);
    }
    for r in f.server.received_requests().await.unwrap() {
        assert_eq!(
            r.method.as_str(),
            "GET",
            "a dry run or a refusal only reads"
        );
    }
    assert!(!f.backups().exists() && f.record().unwrap().files.len() == 1);
}

#[tokio::test]
async fn a_dry_run_of_a_push_and_a_sync_sends_no_put_post_or_delete() {
    let f = fixture().await;
    f.write("new.sav", b"mine");
    f.write("edited.sav", b"old");
    let mut rec = Record::new(CLIENT);
    f.recorded(&mut rec, "edited.sav", b"old");
    f.save_record(&rec);
    f.write("edited.sav", b"edited!");
    f.serve(&[
        ("saves/edited.sav", gzip(b"old")),
        ("saves/theirs.sav", gzip(b"t")),
    ])
    .await;
    f.accept_writes().await;
    let mut d = Script::new(Choice::Local, Confirm::Yes);
    for mode in [Mode::Push, Mode::Sync] {
        let r = f
            .pull(
                &PullOptions {
                    dry_run: true,
                    ..mode_opts(mode)
                },
                &mut FakeHost::default(),
                &mut d,
            )
            .await
            .unwrap();
        assert_eq!(state_of(&r, "saves/edited.sav"), &State::Planned);
    }
    for verb in ["PUT", "POST", "DELETE"] {
        assert!(f.requests(verb).await.is_empty(), "{verb}");
    }
    for r in f.server.received_requests().await.unwrap() {
        assert_eq!(r.method.as_str(), "GET", "{}", r.url);
    }
    assert!(!f.backups().exists());
}

#[tokio::test]
async fn an_upload_over_a_cloud_object_asks_and_backs_the_old_one_up_first() {
    let f = fixture().await;
    f.write("a.sav", b"old");
    let mut rec = Record::new(CLIENT);
    f.recorded(&mut rec, "a.sav", b"old");
    f.save_record(&rec);
    f.write("a.sav", b"newer content");
    f.serve(&[("saves/a.sav", gzip(b"old"))]).await;
    f.accept_writes().await;

    // Without consent nothing is written.
    let mut d = Script::new(Choice::Skip, Confirm::CannotAsk);
    let r = f
        .pull(&mode_opts(Mode::Push), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert!(r.needs_consent);
    assert!(f.requests("PUT").await.is_empty() && !f.backups().exists());

    let mut host = FakeHost::default();
    let mut d = Script::new(Choice::Skip, Confirm::Yes);
    let r = f
        .pull(&mode_opts(Mode::Push), &mut host, &mut d)
        .await
        .unwrap();
    assert!(r.error.is_none(), "{:?}", r.stopped);
    assert_eq!(d.consents.len(), 1, "the one question");
    assert_eq!(
        *host.log.borrow(),
        ["lock"],
        "no snapshot: no local file changes"
    );
    let dir = r.backup_dir.clone().unwrap();
    assert_eq!(
        fs::read(dir.join("cloud/saves/a.sav")).unwrap(),
        gzip(b"old")
    );
    let entry: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("cloud/saves/a.sav.gogo-entry.json")).unwrap())
            .unwrap();
    assert_eq!(entry["hash"], md5_hex(&gzip(b"old")));
    assert_eq!(f.requests("PUT").await.len(), 1);
    let rec = f.record().unwrap();
    assert_eq!(
        rec.files["saves/a.sav"].cloud_hash,
        md5_hex(&gzip(b"newer content"))
    );
}

#[tokio::test]
async fn a_cloud_delete_is_backed_up_first_and_no_delete_turns_it_off() {
    let f = fixture().await;
    f.write("gone.sav", b"bye");
    f.write("kept.sav", b"stay");
    let mut rec = Record::new(CLIENT);
    f.recorded(&mut rec, "gone.sav", b"bye");
    f.recorded(&mut rec, "kept.sav", b"stay");
    f.save_record(&rec);
    fs::remove_file(f.saves.join("gone.sav")).unwrap();
    f.serve_full(
        &[
            ("saves/gone.sav", gzip(b"bye")),
            ("saves/kept.sav", gzip(b"stay")),
        ],
        &[],
        Some((2, &[("saves/kept.sav", gzip(b"stay"))])),
    )
    .await;
    f.accept_writes().await;
    let mut d = Script::new(Choice::Skip, Confirm::Yes);

    let r = f
        .pull(
            &PullOptions {
                no_delete: true,
                ..mode_opts(Mode::Push)
            },
            &mut FakeHost::default(),
            &mut d,
        )
        .await
        .unwrap();
    assert_eq!(
        state_of(&r, "saves/gone.sav"),
        &State::NotUploaded(NotUploaded::NoDelete)
    );
    assert!(f.requests("DELETE").await.is_empty() && !f.backups().exists());
    assert!(f.record().unwrap().files.contains_key("saves/gone.sav"));

    let r = f
        .pull(&mode_opts(Mode::Push), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert!(r.error.is_none(), "{:?}", r.stopped);
    assert_eq!(state_of(&r, "saves/gone.sav"), &State::Done);
    assert_eq!(d.consents.last().map(|_| ()), Some(()));
    let del = f.requests("DELETE").await;
    assert_eq!(del.len(), 1);
    assert!(del[0].url.path().ends_with("/saves/gone.sav"));
    let dir = r.backup_dir.unwrap();
    assert_eq!(
        fs::read(dir.join("cloud/saves/gone.sav")).unwrap(),
        gzip(b"bye")
    );
    assert!(!f.record().unwrap().files.contains_key("saves/gone.sav"));
}

#[tokio::test]
async fn an_upload_over_the_quota_is_refused_before_any_write() {
    let f = fixture().await;
    // Incompressible, so its gzip is about its size.
    let mut x = 12345u32;
    let noise: Vec<u8> = (0..4000)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        })
        .collect();
    f.write("big.sav", &noise);
    f.serve_headers(
        &[("saves/other.sav", gzip(b"o"))],
        &[
            ("x-container-bytes-used", "900"),
            ("x-container-meta-quota-bytes", "1000"),
        ],
    )
    .await;
    f.accept_writes().await;
    let mut d = Script::new(Choice::Skip, Confirm::Yes);
    let e = f
        .pull(&mode_opts(Mode::Push), &mut FakeHost::default(), &mut d)
        .await
        .err()
        .unwrap();
    assert!(matches!(e, CloudError::Quota(_)), "{e}");
    assert_eq!(e.exit_code(), 1);
    let msg = e.to_string();
    assert!(msg.contains("900") && msg.contains("1000"), "{msg}");
    assert!(f.requests("PUT").await.is_empty() && !f.backups().exists());
}

#[tokio::test]
async fn a_failed_upload_stops_the_run_and_the_rerun_continues() {
    let f = fixture().await;
    f.write("a.sav", b"aaa");
    f.write("b.sav", b"bbb");
    f.serve(&[]).await;
    Mock::given(method("PUT"))
        .and(path(format!("/v1/7/{CLIENT}/saves/a.sav")))
        .respond_with(ResponseTemplate::new(201))
        .mount(&f.server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("/v1/7/{CLIENT}/saves/b.sav")))
        .respond_with(ResponseTemplate::new(503))
        .mount(&f.server)
        .await;
    let mut d = Script::new(Choice::Skip, Confirm::Yes);
    let r = f
        .pull(&mode_opts(Mode::Push), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert_eq!(r.error.as_ref().map(CloudError::exit_code), Some(4));
    assert_eq!(state_of(&r, "saves/a.sav"), &State::Done);
    assert_eq!(state_of(&r, "saves/b.sav"), &State::NotDone);
    let rec = f.record().unwrap();
    assert!(rec.files.contains_key("saves/a.sav"), "recorded when done");
    assert!(
        !rec.files.contains_key("saves/b.sav"),
        "never advanced past a failed upload"
    );
    // The rerun sees a as listed and recorded, and uploads b.
    f.serve(&[("saves/a.sav", gzip(b"aaa"))]).await;
    f.accept_writes().await;
    let r = f
        .pull(&mode_opts(Mode::Push), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert!(r.error.is_none(), "{:?}", r.stopped);
    assert_eq!(f.requests("PUT").await.len(), 1);
    assert!(f.record().unwrap().files.contains_key("saves/b.sav"));
}

#[tokio::test]
async fn a_missing_or_emptied_save_dir_never_deletes_in_the_cloud() {
    let f = fixture().await;
    f.write("a.sav", b"a");
    f.write("b.sav", b"b");
    let mut rec = Record::new(CLIENT);
    f.recorded(&mut rec, "a.sav", b"a");
    f.recorded(&mut rec, "b.sav", b"b");
    f.save_record(&rec);
    f.serve(&[("saves/a.sav", gzip(b"a")), ("saves/b.sav", gzip(b"b"))])
        .await;
    f.accept_writes().await;
    let mut d = Script::new(Choice::Local, Confirm::Yes);

    // Emptied: even `--prefer local` doesn't delete everything.
    fs::remove_file(f.saves.join("a.sav")).unwrap();
    fs::remove_file(f.saves.join("b.sav")).unwrap();
    let r = f
        .pull(&mode_opts(Mode::Push), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert_eq!(
        state_of(&r, "saves/a.sav"),
        &State::Left(LeftReason::LocationEmptied)
    );
    // Missing.
    fs::remove_dir(&f.saves).unwrap();
    let r = f
        .pull(&mode_opts(Mode::Sync), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert_eq!(
        state_of(&r, "saves/b.sav"),
        &State::Left(LeftReason::LocationMissing)
    );
    assert!(f.requests("DELETE").await.is_empty());
    assert!(f.requests("PUT").await.is_empty());
}

#[tokio::test]
async fn an_empty_file_is_never_uploaded() {
    let f = fixture().await;
    f.write("empty.sav", b"");
    f.write("real.sav", b"real");
    f.serve(&[]).await;
    f.accept_writes().await;
    let mut d = Script::new(Choice::Skip, Confirm::Yes);
    let r = f
        .pull(&mode_opts(Mode::Push), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert_eq!(
        state_of(&r, "saves/empty.sav"),
        &State::NotUploaded(NotUploaded::EmptyFile)
    );
    assert_eq!(f.requests("PUT").await.len(), 1);
}

#[tokio::test]
async fn a_sync_pulls_and_pushes_in_one_run_and_clears_pending_push() {
    let f = fixture().await;
    f.write("mine.sav", b"mine");
    f.write("old.sav", b"old");
    let mut rec = Record::new(CLIENT);
    f.recorded(&mut rec, "old.sav", b"old");
    rec.pending_push = true;
    f.save_record(&rec);
    f.serve(&[
        ("saves/old.sav", gzip(b"old")),
        ("saves/theirs.sav", gzip(b"theirs")),
    ])
    .await;
    f.accept_writes().await;
    let mut d = Script::new(Choice::Skip, Confirm::Yes);
    let r = f
        .pull(&mode_opts(Mode::Sync), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert!(r.error.is_none(), "{:?}", r.stopped);
    assert_eq!(f.read("theirs.sav").unwrap(), b"theirs");
    assert_eq!(state_of(&r, "saves/mine.sav"), &State::Done);
    assert_eq!(f.requests("PUT").await.len(), 1);
    assert!(r.pending_push_cleared);
    let rec = f.record().unwrap();
    assert!(!rec.pending_push && rec.files.contains_key("saves/mine.sav"));
    assert!(rec.files.contains_key("saves/theirs.sav"));

    // A pull leaves `pending_push` alone, and so does a failed push.
    let mut rec = f.record().unwrap();
    rec.pending_push = true;
    f.save_record(&rec);
    f.write("more.sav", b"more");
    let r = f
        .pull(&mode_opts(Mode::Pull), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert!(!r.pending_push_cleared && f.record().unwrap().pending_push);
}

#[tokio::test]
async fn conflicts_in_a_sync_go_each_way_and_both_uploads_the_kept_copy() {
    let f = fixture().await;
    f.write("c.sav", b"base");
    let mut rec = Record::new(CLIENT);
    f.recorded(&mut rec, "c.sav", b"base");
    f.save_record(&rec);
    f.write("c.sav", b"local edit");
    f.serve(&[("saves/c.sav", gzip(b"cloud edit"))]).await;
    f.accept_writes().await;

    // Undecided (no TTY, no --prefer): left, exit 8's business.
    let mut d = Script::new(Choice::Skip, Confirm::Yes);
    let r = f
        .pull(&mode_opts(Mode::Sync), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert_eq!(
        state_of(&r, "saves/c.sav"),
        &State::Left(LeftReason::Undecided)
    );
    assert!(f.requests("PUT").await.is_empty());

    // Local wins: uploaded over the cloud's, which is backed up.
    let mut d = Script::new(Choice::Local, Confirm::Yes);
    let r = f
        .pull(&mode_opts(Mode::Sync), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert_eq!(state_of(&r, "saves/c.sav"), &State::Done);
    assert_eq!(f.read("c.sav").unwrap(), b"local edit");
    let dir = r.backup_dir.unwrap();
    assert_eq!(
        fs::read(dir.join("cloud/saves/c.sav")).unwrap(),
        gzip(b"cloud edit")
    );
    assert_eq!(f.requests("PUT").await.len(), 1);

    // Both: the cloud's downloaded, the local one kept and uploaded too.
    let f = fixture().await;
    f.write("c.sav", b"base");
    let mut rec = Record::new(CLIENT);
    f.recorded(&mut rec, "c.sav", b"base");
    f.save_record(&rec);
    f.write("c.sav", b"local edit");
    f.serve(&[("saves/c.sav", gzip(b"cloud edit"))]).await;
    f.accept_writes().await;
    let mut d = Script::new(Choice::Both, Confirm::Yes);
    let r = f
        .pull(&mode_opts(Mode::Sync), &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert!(r.error.is_none(), "{:?}", r.stopped);
    assert_eq!(f.read("c.sav").unwrap(), b"cloud edit");
    let kept = r
        .files
        .iter()
        .find(|x| x.name.starts_with("saves/c.sav.local-"))
        .unwrap_or_else(|| panic!("{:?}", r.files));
    assert_eq!(kept.state, State::Done);
    let puts = f.requests("PUT").await;
    assert_eq!(puts.len(), 1);
    assert_eq!(gunzip(&puts[0].body), b"local edit");
    assert!(f.record().unwrap().files.contains_key(&kept.name));
}

#[tokio::test]
async fn a_rolled_back_push_needs_prefer_local_and_clears_the_mark() {
    let f = fixture().await;
    f.write("a.sav", b"a");
    let mut rec = Record::new(CLIENT);
    f.recorded(&mut rec, "a.sav", b"a");
    rec.rolled_back = Some(record::datetime(std::time::SystemTime::now()));
    f.save_record(&rec);
    f.write("a.sav", b"rolled back");
    f.serve(&[("saves/a.sav", gzip(b"a"))]).await;
    f.accept_writes().await;
    let r = f
        .pull(
            &mode_opts(Mode::Push),
            &mut FakeHost::default(),
            &mut Script::new(Choice::Skip, Confirm::Yes),
        )
        .await
        .unwrap();
    assert_eq!(
        state_of(&r, "saves/a.sav"),
        &State::Left(LeftReason::Undecided)
    );
    assert!(f.record().unwrap().rolled_back.is_some());
    let r = f
        .pull(
            &mode_opts(Mode::Push),
            &mut FakeHost::default(),
            &mut Script::new(Choice::Local, Confirm::Yes),
        )
        .await
        .unwrap();
    assert!(r.rolled_back_cleared);
    assert!(f.record().unwrap().rolled_back.is_none());
}

#[tokio::test]
async fn prefer_newer_decides_by_mtimes_and_a_tie_stays_a_conflict() {
    let f = fixture().await;
    f.write("c.sav", b"base");
    let mut rec = Record::new(CLIENT);
    f.recorded(&mut rec, "c.sav", b"base");
    f.save_record(&rec);
    f.write("c.sav", b"local edit");
    f.serve(&[("saves/c.sav", gzip(b"cloud edit"))]).await;
    f.accept_writes().await;
    // The cloud's copy is from 2024; the local file was just written.
    Mock::given(method("HEAD"))
        .and(path(format!("/v1/7/{CLIENT}/saves/c.sav")))
        .respond_with(
            ResponseTemplate::new(200).insert_header("x-object-meta-locallastmodified", UPLOADED),
        )
        .mount(&f.server)
        .await;
    let newer = PullOptions {
        newer: true,
        ..mode_opts(Mode::Sync)
    };
    let mut d = Script::new(Choice::Skip, Confirm::Yes);
    let r = f
        .pull(&newer, &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert!(d.asked.is_empty(), "settled by time, nobody asked");
    assert_eq!(r.files[0].choice, Some(Choice::Local));
    assert_eq!(f.requests("PUT").await.len(), 1);

    // A tie (the cloud's time equals the local mtime) is left.
    let f = fixture().await;
    f.write("c.sav", b"base");
    let mut rec = Record::new(CLIENT);
    f.recorded(&mut rec, "c.sav", b"base");
    f.save_record(&rec);
    f.write("c.sav", b"local edit");
    let mtime = chrono::DateTime::<chrono::Utc>::from(
        fs::metadata(f.saves.join("c.sav"))
            .unwrap()
            .modified()
            .unwrap(),
    )
    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    f.serve(&[("saves/c.sav", gzip(b"cloud edit"))]).await;
    f.accept_writes().await;
    Mock::given(method("HEAD"))
        .and(path(format!("/v1/7/{CLIENT}/saves/c.sav")))
        .respond_with(
            ResponseTemplate::new(200).insert_header("x-object-meta-locallastmodified", mtime),
        )
        .mount(&f.server)
        .await;
    let mut d = Script::new(Choice::Skip, Confirm::Yes);
    let r = f
        .pull(&newer, &mut FakeHost::default(), &mut d)
        .await
        .unwrap();
    assert_eq!(d.asked, ["saves/c.sav"]);
    assert_eq!(
        state_of(&r, "saves/c.sav"),
        &State::Left(LeftReason::Undecided)
    );
}

#[tokio::test]
async fn prefer_newer_is_refused_after_a_rollback() {
    let f = fixture().await;
    f.write("a.sav", b"a");
    let mut rec = Record::new(CLIENT);
    f.recorded(&mut rec, "a.sav", b"a");
    rec.rolled_back = Some(record::datetime(std::time::SystemTime::now()));
    f.save_record(&rec);
    f.serve(&[("saves/a.sav", gzip(b"a"))]).await;
    let e = f
        .pull(
            &PullOptions {
                newer: true,
                ..mode_opts(Mode::Sync)
            },
            &mut FakeHost::default(),
            &mut Script::new(Choice::Skip, Confirm::Yes),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(e.exit_code(), 7);
}
