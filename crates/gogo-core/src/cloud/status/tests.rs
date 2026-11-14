use std::fs;
use std::sync::atomic::Ordering;

use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::cloud::locations::Layout;
use crate::cloud::record::SyncLock;
use crate::cloud::storage::{gzip, md5_hex};
use crate::cloud::testing::{FixedExchange, MainSession, NoManifests, keep_manifest};

const CLIENT: &str = "11111111111111111";

const CONFIG: &str = r#"{"content":{"Windows":{"cloudStorage":{"enabled":true,
    "locations":[{"name":"saves","location":"<?DOCUMENTS?>/G"},
                 {"name":"odd","location":"<?NOPE?>/x"}]}}}}"#;

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

async fn fixture(config: &str) -> Fixture {
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

impl Fixture {
    /// Serve `objects` (name → stored bytes) as the listing and as objects.
    async fn serve(&self, objects: &[(&str, Vec<u8>)]) {
        let listing: Vec<serde_json::Value> = objects
            .iter()
            .map(|(n, b)| {
                serde_json::json!({
                    "name": n, "bytes": b.len(), "hash": md5_hex(b),
                    "last_modified": "2026-11-13T09:00:00.000000+00:00",
                    "content_type": "application/octet-stream"
                })
            })
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
            let p: String = n
                .split('/')
                .map(|s| s.replace(' ', "%20"))
                .collect::<Vec<_>>()
                .join("/");
            Mock::given(method("GET"))
                .and(path(format!("/v1/7/{CLIENT}/{p}")))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(b.clone()))
                .mount(&self.server)
                .await;
        }
    }

    /// Serve a listing of `names` only (one byte of made-up gzip each); no
    /// object is served.
    async fn serve_names(&self, names: &[&str]) {
        let listing: Vec<serde_json::Value> = names
            .iter()
            .map(|n| serde_json::json!({"name": n, "bytes": 25, "hash": md5_hex(n.as_bytes())}))
            .collect();
        Mock::given(method("GET"))
            .and(path(format!("/v1/7/{CLIENT}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(listing))
            .mount(&self.server)
            .await;
    }

    async fn run(&self) -> Result<Plan, CloudError> {
        let auth = CloudAuth::with_parts(&self.session, || Ok(()), NoManifests, &self.exchange);
        let storage =
            StorageClient::with_base(format!("{}/v1/", self.server.uri()).parse().unwrap())
                .unwrap();
        status(&Inputs {
            game: &self.game,
            auth: &auth,
            storage: &storage,
            http: &reqwest::Client::new(),
            config_base: &self.server.uri(),
            cache_dir: &self.cache,
            record_dir: &self.records,
        })
        .await
    }

    fn write(&self, rel: &str, body: &[u8]) {
        let p = self.saves.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
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
}

fn actions(plan: &Plan) -> Vec<(String, Action)> {
    plan.files().map(|f| (f.name.clone(), f.action)).collect()
}

/// Every path under `dir` with its size and mtime.
fn tree(dir: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let m = e.metadata().unwrap();
            out.push((e.path(), m.len(), m.modified().unwrap()));
            if m.is_dir() {
                stack.push(e.path());
            }
        }
    }
    out.sort();
    out
}

#[tokio::test]
async fn first_sync_decides_file_by_file_and_writes_nothing() {
    let f = fixture(CONFIG).await;
    f.write("only-local.sav", b"mine");
    f.write("Slot 1/same.sav", b"same");
    f.write("differ.sav", b"local version");
    f.write("game.log", b"log");
    f.serve(&[
        ("saves/only-cloud.sav", gzip(b"theirs")),
        ("saves/Slot 1/same.sav", gzip(b"same")),
        ("saves/differ.sav", gzip(b"cloud version")),
        ("saves/gone.sav", gzip(b"")),
        ("saves/crash.dmp", gzip(b"dump")),
        ("odd/x", gzip(b"x")),
        ("elsewhere/y", gzip(b"y")),
        ("elsewhere/old", gzip(b"")),
    ])
    .await;
    let before = tree(&f.game.root);
    let plan = f.run().await.unwrap();
    assert_eq!(
        tree(&f.game.root),
        before,
        "status writes nothing in the game"
    );
    assert!(!f.records.join("5.toml").exists(), "nor the record");

    assert!(plan.enabled && plan.first_sync && !plan.client_id_changed);
    assert_eq!(
        actions(&plan),
        [
            ("saves/Slot 1/same.sav".to_string(), Action::Record),
            (
                "saves/differ.sav".into(),
                Action::Conflict(ConflictReason::BothNewDiffer)
            ),
            ("saves/only-cloud.sav".into(), Action::Download),
            ("saves/only-local.sav".into(), Action::Upload),
        ]
    );
    assert_eq!(plan.conflicts(), 1);
    assert_eq!(plan.count("upload"), 1);
    let un: Vec<_> = plan
        .unmapped
        .iter()
        .map(|u| (u.name.as_str(), u.reason))
        .collect();
    assert_eq!(
        un,
        [
            ("elsewhere/y", UnmappedReason::NoLocation),
            ("odd/x", UnmappedReason::SkippedLocation),
            ("saves/crash.dmp", UnmappedReason::Excluded),
        ]
    );
    let c = plan.container.as_ref().unwrap();
    assert_eq!((c.objects, c.tombstones), (8, 2));
    assert_eq!(plan.locations[0].local_excluded, 1);
    assert_eq!(plan.skipped.len(), 1);
    assert_eq!(f.exchange.calls.load(Ordering::SeqCst), 1);

    // No user id, token or client id in the JSON.
    let json = serde_json::to_string(&plan).unwrap();
    for secret in [
        CLIENT,
        "made-up-game-token",
        crate::cloud::testing::SECRET,
        "/v1/7",
    ] {
        assert!(!json.contains(secret), "{secret} in {json}");
    }
}

#[tokio::test]
async fn with_a_record_every_row_of_the_table() {
    let f = fixture(CONFIG).await;
    let mut r = Record::new(CLIENT);
    for (rel, body) in [
        ("same.sav", &b"same"[..]),
        ("touched.sav", b"touched"),
        ("local-changed.sav", b"v1"),
        ("cloud-changed.sav", b"v1"),
        ("cloud-deleted.sav", b"v1"),
        ("local-deleted.sav", b"v1"),
        ("both-changed.sav", b"v1"),
        ("both-deleted.sav", b"v1"),
    ] {
        f.write(rel, body);
        f.recorded(&mut r, rel, body);
    }
    // Local changes after the record was taken.
    let t = fs::File::options()
        .write(true)
        .open(f.saves.join("touched.sav"))
        .unwrap();
    t.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
        .unwrap();
    drop(t);
    f.write("local-changed.sav", b"v2 local");
    f.write("both-changed.sav", b"v2 local");
    fs::remove_file(f.saves.join("local-deleted.sav")).unwrap();
    fs::remove_file(f.saves.join("both-deleted.sav")).unwrap();
    f.save_record(&r);
    f.serve(&[
        ("saves/same.sav", gzip(b"same")),
        ("saves/touched.sav", gzip(b"touched")),
        ("saves/local-changed.sav", gzip(b"v1")),
        ("saves/cloud-changed.sav", gzip(b"v2 cloud")),
        ("saves/cloud-deleted.sav", gzip(b"")),
        ("saves/local-deleted.sav", gzip(b"v1")),
        ("saves/both-changed.sav", gzip(b"v2 cloud")),
    ])
    .await;
    let plan = f.run().await.unwrap();
    assert!(!plan.first_sync);
    let got: BTreeMap<_, _> = actions(&plan).into_iter().collect();
    let want: BTreeMap<String, Action> = [
        ("same.sav", Action::Nothing),
        ("touched.sav", Action::Nothing),
        ("local-changed.sav", Action::Upload),
        ("cloud-changed.sav", Action::Download),
        ("cloud-deleted.sav", Action::DeleteLocal),
        ("local-deleted.sav", Action::DeleteCloud),
        (
            "both-changed.sav",
            Action::Conflict(ConflictReason::BothChanged),
        ),
        ("both-deleted.sav", Action::Forget),
    ]
    .into_iter()
    .map(|(n, a)| (format!("saves/{n}"), a))
    .collect();
    assert_eq!(got, want);
    // Nothing was downloaded: a record answers every comparison.
    let fetched = f
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().starts_with(&format!("/v1/7/{CLIENT}/")))
        .count();
    assert_eq!(fetched, 0);
}

#[tokio::test]
async fn a_rollback_mark_makes_every_transfer_a_conflict() {
    let f = fixture(CONFIG).await;
    let mut r = Record::new(CLIENT);
    f.write("a.sav", b"v1");
    f.recorded(&mut r, "a.sav", b"v1");
    r.rolled_back = Some("2026-11-12T10:00:00Z".parse().unwrap());
    f.save_record(&r);
    f.serve(&[("saves/a.sav", gzip(b"v2"))]).await;
    let plan = f.run().await.unwrap();
    assert!(plan.rolled_back);
    assert_eq!(
        actions(&plan),
        [(
            "saves/a.sav".to_string(),
            Action::Conflict(ConflictReason::RolledBack)
        )]
    );
}

#[tokio::test]
async fn a_record_for_another_client_is_a_first_sync_and_stays_put() {
    let f = fixture(CONFIG).await;
    let mut r = Record::new("99999999999999999");
    f.write("a.sav", b"v1");
    f.recorded(&mut r, "a.sav", b"v1");
    f.save_record(&r);
    let before = fs::read(f.records.join("5.toml")).unwrap();
    f.serve(&[("saves/a.sav", gzip(b"v1"))]).await;
    let plan = f.run().await.unwrap();
    assert!(plan.client_id_changed && plan.first_sync);
    assert_eq!(
        actions(&plan),
        [("saves/a.sav".to_string(), Action::Record)]
    );
    assert_eq!(fs::read(f.records.join("5.toml")).unwrap(), before);
}

#[tokio::test]
async fn names_differing_by_case_meet_one_local_file() {
    let f = fixture(CONFIG).await;
    f.write("Slot/A.sav", b"x");
    f.serve(&[("saves/slot/a.sav", gzip(b"x"))]).await;
    let plan = f.run().await.unwrap();
    assert_eq!(
        actions(&plan),
        [("saves/slot/a.sav".to_string(), Action::Record)]
    );

    let f = fixture(CONFIG).await;
    f.write("Slot/A.sav", b"x");
    f.serve(&[
        ("saves/Slot/A.sav", gzip(b"x")),
        ("saves/slot/a.sav", gzip(b"y")),
    ])
    .await;
    let plan = f.run().await.unwrap();
    assert!(
        plan.files()
            .all(|p| p.action == Action::Conflict(ConflictReason::CaseClash)),
        "{:?}",
        actions(&plan)
    );
    assert_eq!(plan.conflicts(), 2);
}

#[tokio::test]
async fn a_corrupt_object_is_reported_and_the_rest_planned() {
    let f = fixture(CONFIG).await;
    f.write("bad.sav", b"x");
    f.write("new.sav", b"n");
    f.serve(&[("saves/bad.sav", gzip(b"x"))]).await;
    // Same name, other bytes: the listing's hash won't match.
    Mock::given(method("GET"))
        .and(path(format!("/v1/7/{CLIENT}/saves/bad.sav")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(gzip(b"y")))
        .with_priority(1)
        .mount(&f.server)
        .await;
    let plan = f.run().await.unwrap();
    assert_eq!(
        actions(&plan),
        [
            ("saves/bad.sav".to_string(), Action::Corrupt),
            ("saves/new.sav".into(), Action::Upload),
        ]
    );
    assert_eq!(plan.corrupt(), 1);
}

#[tokio::test]
async fn storage_off_needs_no_account() {
    let off = r#"{"content":{"Windows":{"cloudStorage":{"enabled":false,"locations":[]}}}}"#;
    let f = fixture(off).await;
    let plan = f.run().await.unwrap();
    assert!(!plan.enabled && !plan.no_remote_config);
    assert_eq!(f.exchange.calls.load(Ordering::SeqCst), 0);

    // No remote config at all (404): the same.
    let f = fixture(CONFIG).await;
    let other = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&other)
        .await;
    let auth = CloudAuth::with_parts(&f.session, || Ok(()), NoManifests, &f.exchange);
    let storage =
        StorageClient::with_base(format!("{}/v1/", other.uri()).parse().unwrap()).unwrap();
    let plan = status(&Inputs {
        game: &f.game,
        auth: &auth,
        storage: &storage,
        http: &reqwest::Client::new(),
        config_base: &other.uri(),
        cache_dir: &f.cache,
        record_dir: &f.records,
    })
    .await
    .unwrap();
    assert!(plan.no_remote_config);
    assert_eq!(f.exchange.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn no_license_and_storage_errors_stop_the_run() {
    let mut f = fixture(CONFIG).await;
    f.exchange.fail = Some(|gogid| CloudError::NoLicense { gogid });
    let e = f.run().await.unwrap_err();
    assert_eq!(e.exit_code(), 3);

    let f = fixture(CONFIG).await;
    Mock::given(method("GET"))
        .and(path_regex("^/v1/"))
        .respond_with(ResponseTemplate::new(400).set_body_string(r#"{"error":"invalid_version"}"#))
        .mount(&f.server)
        .await;
    let e = f.run().await.unwrap_err();
    assert!(e.to_string().contains("invalid_version"), "{e}");
    assert!(!e.to_string().contains(CLIENT));
}

#[tokio::test]
async fn a_short_listing_plans_nothing() {
    let f = fixture(CONFIG).await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/7/{CLIENT}")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-container-object-count", "5")
                .set_body_string("[]"),
        )
        .mount(&f.server)
        .await;
    assert!(matches!(
        f.run().await,
        Err(CloudError::ListingTruncated { .. })
    ));
}

#[tokio::test]
async fn a_kept_manifest_for_another_client_than_the_info_is_refused() {
    let f = fixture(CONFIG).await;
    fs::write(
        f.game.layout.game_files.join("goggame-5.info"),
        r#"{"clientId":"22222222222222222"}"#,
    )
    .unwrap();
    Mock::given(method("GET"))
        .and(path("/components/galaxy_client/clients/22222222222222222"))
        .respond_with(ResponseTemplate::new(200).set_body_string(CONFIG))
        .mount(&f.server)
        .await;
    assert!(matches!(
        f.run().await,
        Err(CloudError::ClientIdMismatch { gogid: 5 })
    ));
}

#[tokio::test]
async fn a_missing_save_dir_never_plans_cloud_deletions() {
    let f = fixture(CONFIG).await;
    let mut r = Record::new(CLIENT);
    for rel in ["a.sav", "b.sav"] {
        f.write(rel, b"v1");
        f.recorded(&mut r, rel, b"v1");
    }
    f.save_record(&r);
    f.serve(&[
        ("saves/a.sav", gzip(b"v1")),
        ("saves/b.sav", gzip(b"v1")),
        ("saves/new.sav", gzip(b"n")),
    ])
    .await;
    // The prefix moved or the drive isn't mounted: the dir is gone.
    fs::remove_dir_all(&f.saves).unwrap();
    let plan = f.run().await.unwrap();
    assert!(plan.locations[0].missing && !plan.locations[0].emptied);
    assert_eq!(
        actions(&plan),
        [
            (
                "saves/a.sav".to_string(),
                Action::Conflict(ConflictReason::LocationMissing)
            ),
            (
                "saves/b.sav".into(),
                Action::Conflict(ConflictReason::LocationMissing)
            ),
            ("saves/new.sav".into(), Action::Download),
        ]
    );
    assert_eq!(plan.count("delete_cloud"), 0);

    // No record: a missing dir only means downloads.
    let f = fixture(CONFIG).await;
    f.serve(&[("saves/a.sav", gzip(b"v1"))]).await;
    fs::remove_dir_all(&f.saves).unwrap();
    let plan = f.run().await.unwrap();
    assert!(!plan.locations[0].missing);
    assert_eq!(
        actions(&plan),
        [("saves/a.sav".to_string(), Action::Download)]
    );
}

#[tokio::test]
async fn an_emptied_save_dir_asks_before_deleting_every_save() {
    let f = fixture(CONFIG).await;
    let mut r = Record::new(CLIENT);
    for rel in ["a.sav", "b.sav", "c.sav"] {
        f.write(rel, b"v1");
        f.recorded(&mut r, rel, b"v1");
    }
    f.save_record(&r);
    f.serve(&[
        ("saves/a.sav", gzip(b"v1")),
        ("saves/b.sav", gzip(b"v1")),
        ("saves/c.sav", gzip(b"")),
    ])
    .await;
    for rel in ["a.sav", "b.sav", "c.sav"] {
        fs::remove_file(f.saves.join(rel)).unwrap();
    }
    let plan = f.run().await.unwrap();
    assert!(plan.locations[0].emptied);
    let got: BTreeMap<_, _> = actions(&plan).into_iter().collect();
    assert_eq!(
        got["saves/a.sav"],
        Action::Conflict(ConflictReason::LocationEmptied)
    );
    assert_eq!(
        got["saves/b.sav"],
        Action::Conflict(ConflictReason::LocationEmptied)
    );
    // Deleted on both sides already: nothing is lost by forgetting it.
    assert_eq!(got["saves/c.sav"], Action::Forget);

    // One file left: deletions of the others go ahead as planned.
    let f = fixture(CONFIG).await;
    let mut r = Record::new(CLIENT);
    for rel in ["a.sav", "b.sav"] {
        f.write(rel, b"v1");
        f.recorded(&mut r, rel, b"v1");
    }
    f.save_record(&r);
    f.serve(&[("saves/a.sav", gzip(b"v1")), ("saves/b.sav", gzip(b"v1"))])
        .await;
    fs::remove_file(f.saves.join("a.sav")).unwrap();
    let plan = f.run().await.unwrap();
    assert!(!plan.locations[0].emptied);
    assert_eq!(plan.count("delete_cloud"), 1);
}

#[tokio::test]
async fn history_of_another_case_is_no_clash() {
    // A tombstone and a live object differing only by case.
    let f = fixture(CONFIG).await;
    f.write("Slot/A.sav", b"x");
    f.serve(&[
        ("saves/slot/a.sav", gzip(b"")),
        ("saves/Slot/A.sav", gzip(b"x")),
    ])
    .await;
    let plan = f.run().await.unwrap();
    assert_eq!(
        actions(&plan),
        [("saves/Slot/A.sav".to_string(), Action::Record)]
    );
    assert_eq!(plan.conflicts(), 0);

    // A recorded name whose file is gone, beside a live name of another case.
    let f = fixture(CONFIG).await;
    let mut r = Record::new(CLIENT);
    f.write("slot/a.sav", b"x");
    f.recorded(&mut r, "slot/a.sav", b"x");
    fs::remove_file(f.saves.join("slot/a.sav")).unwrap();
    f.save_record(&r);
    f.serve(&[("saves/OTHER.sav", gzip(b"y"))]).await;
    let plan = f.run().await.unwrap();
    assert_eq!(plan.count("conflict"), 0, "{:?}", actions(&plan));
}

#[tokio::test]
async fn a_set_aside_records_marks_dont_count() {
    let f = fixture(CONFIG).await;
    let mut r = Record::new("99999999999999999");
    r.rolled_back = Some("2026-11-12T10:00:00Z".parse().unwrap());
    r.pending_push = true;
    r.last_sync = Some("2026-11-12T09:00:00Z".parse().unwrap());
    f.save_record(&r);
    f.serve(&[("saves/a.sav", gzip(b"v1"))]).await;
    let plan = f.run().await.unwrap();
    assert!(plan.client_id_changed && plan.first_sync);
    assert!(!plan.rolled_back && !plan.pending_push && plan.last_sync.is_none());
    assert_eq!(
        actions(&plan),
        [("saves/a.sav".to_string(), Action::Download)]
    );
}

#[tokio::test]
async fn an_unreadable_local_file_is_reported_and_the_rest_planned() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture(CONFIG).await;
    let mut r = Record::new(CLIENT);
    f.write("locked.sav", b"v1");
    f.recorded(&mut r, "locked.sav", b"v1");
    f.save_record(&r);
    f.write("locked.sav", b"v2 bigger");
    let p = f.saves.join("locked.sav");
    fs::set_permissions(&p, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&p).is_ok() {
        // Running as root: permissions don't stop a read.
        return;
    }
    f.write("new.sav", b"n");
    f.serve(&[("saves/locked.sav", gzip(b"v1"))]).await;
    let plan = f.run().await.unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        actions(&plan),
        [
            ("saves/locked.sav".to_string(), Action::Unreadable),
            ("saves/new.sav".into(), Action::Upload),
        ]
    );
    assert_eq!(plan.unreadable(), 1);
}

#[tokio::test]
async fn hostile_cloud_names_are_never_mapped() {
    let f = fixture(CONFIG).await;
    f.serve_names(&[
        "saves/../escape.sav",
        "saves/a\\..\\..\\b.sav",
        "/saves/abs.sav",
        "saves/nul\u{0}.sav",
        "saves/C:drive.sav",
        "saves/ok.sav",
    ])
    .await;
    let before = tree(&f.game.root);
    let plan = f.run().await.unwrap();
    assert_eq!(tree(&f.game.root), before);
    assert_eq!(
        actions(&plan),
        [("saves/ok.sav".to_string(), Action::Download)]
    );
    let un: BTreeMap<_, _> = plan
        .unmapped
        .iter()
        .map(|u| (u.name.as_str(), u.reason))
        .collect();
    assert_eq!(un["saves/../escape.sav"], UnmappedReason::BadPath);
    assert_eq!(un["saves/a\\..\\..\\b.sav"], UnmappedReason::BadPath);
    assert_eq!(un["/saves/abs.sav"], UnmappedReason::NoLocation);
    assert_eq!(un["saves/nul\u{0}.sav"], UnmappedReason::BadPath);
    assert_eq!(un["saves/C:drive.sav"], UnmappedReason::BadPath);
}

#[tokio::test]
async fn a_link_in_a_save_dir_is_ignored_and_nothing_changes() {
    let f = fixture(CONFIG).await;
    let outside = f.game.root.join("outside.sav");
    fs::write(&outside, b"not a save").unwrap();
    std::os::unix::fs::symlink(&outside, f.saves.join("link.sav")).unwrap();
    f.write("real.sav", b"r");
    f.serve(&[("saves/link.sav", gzip(b"cloud"))]).await;
    let before = tree(&f.game.root);
    let plan = f.run().await.unwrap();
    assert_eq!(tree(&f.game.root), before);
    assert_eq!(plan.locations[0].ignored, ["link.sav"]);
    // The cloud's object of that name would be written through a link that
    // leaves the prefix: it is never mapped, and the link is never read.
    assert_eq!(
        actions(&plan),
        [("saves/real.sav".to_string(), Action::Upload)]
    );
    assert_eq!(
        plan.unmapped,
        [Unmapped {
            name: "saves/link.sav".into(),
            reason: UnmappedReason::BadPath
        }]
    );
}

const INSTALL_CONFIG: &str = r#"{"content":{"Windows":{"cloudStorage":{"enabled":true,
    "locations":[{"name":"game","location":"<?INSTALL?>"},
                 {"name":"saves","location":"<?DOCUMENTS?>/G"}]}}}}"#;

/// An installer-lane game in `drive_c/GOG/G` with `shipped` (relative to it)
/// named by its installer's log.
fn installer_game(f: &mut Fixture, shipped: &[&str]) -> PathBuf {
    let gf = f.game.layout.prefix.join("drive_c/GOG/G");
    fs::create_dir_all(&gf).unwrap();
    f.game.layout.game_files = gf.clone();
    fs::write(
        f.game.root.join("content.toml"),
        "schema = 1\nlane = \"installer\"\nbuild_id = \"b1\"\n",
    )
    .unwrap();
    fs::create_dir_all(f.game.root.join("install")).unwrap();
    let log: String = shipped
        .iter()
        .map(|s| format!("Dest filename: C:\\GOG\\G\\{}\r\n", s.replace('/', "\\")))
        .collect();
    fs::write(f.game.root.join("install/inno.log"), log).unwrap();
    gf
}

#[tokio::test]
async fn build_files_in_the_game_dir_are_never_planned() {
    let mut f = fixture(INSTALL_CONFIG).await;
    let gf = installer_game(&mut f, &["Game.exe", "Data/levels.pak", "settings.ini"]);
    fs::create_dir_all(gf.join("Data")).unwrap();
    fs::write(gf.join("Game.exe"), b"exe").unwrap();
    fs::write(gf.join("Data/levels.pak"), b"pak").unwrap();
    // The game rewrote a shipped file: still the build's (path alone).
    fs::write(gf.join("settings.ini"), b"rewritten by the game").unwrap();
    fs::create_dir_all(gf.join("SAVE")).unwrap();
    fs::write(gf.join("SAVE/slot1.sav"), b"mine").unwrap();
    f.serve(&[
        ("game/SAVE/slot2.sav", gzip(b"theirs")),
        ("game/game.exe", gzip(b"another exe")),
        ("game/Data/levels.pak", gzip(b"")),
    ])
    .await;
    // A record of a build file (an older gogo, or a file an update started
    // shipping): still never acted on.
    let mut r = Record::new(CLIENT);
    r.files.insert(
        "game/settings.ini".into(),
        FileRecord {
            cloud_hash: md5_hex(b"x"),
            local_md5: md5_hex(b"x"),
            size: 1,
            mtime: record::datetime(std::time::SystemTime::UNIX_EPOCH),
        },
    );
    f.save_record(&r);
    let before = tree(&f.game.root);

    let plan = f.run().await.unwrap();
    assert_eq!(
        actions(&plan),
        [
            ("game/SAVE/slot1.sav".to_string(), Action::Upload),
            ("game/SAVE/slot2.sav".to_string(), Action::Download),
        ]
    );
    let lp = &plan.locations[0];
    assert_eq!(lp.local_build_files, 3);
    let note = lp.build_files.as_ref().unwrap();
    assert_eq!(note.source, "the installer's log");
    assert_eq!(note.files, Some(3));
    assert_eq!(
        plan.unmapped,
        [Unmapped {
            name: "game/game.exe".into(),
            reason: UnmappedReason::BuildFile,
        }],
        "the live object over a build file is shown; the tombstone is history"
    );
    assert_eq!(tree(&f.game.root), before, "status writes nothing there");
}

#[tokio::test]
async fn a_game_dir_location_without_a_list_is_skipped_not_synced_blind() {
    let mut f = fixture(INSTALL_CONFIG).await;
    let gf = installer_game(&mut f, &["Game.exe"]);
    fs::remove_file(f.game.root.join("install/inno.log")).unwrap();
    fs::write(gf.join("Game.exe"), b"exe").unwrap();
    f.write("doc.sav", b"doc");
    f.serve(&[
        ("game/Game.exe", gzip(b"exe")),
        ("saves/doc.sav", gzip(b"doc")),
    ])
    .await;
    let plan = f.run().await.unwrap();
    assert_eq!(
        actions(&plan),
        [("saves/doc.sav".to_string(), Action::Record)],
        "the other location still syncs"
    );
    assert_eq!(plan.skipped.len(), 1);
    assert_eq!(plan.skipped[0].name, "game");
    assert!(matches!(
        plan.skipped[0].reason,
        crate::cloud::locations::SkipReason::BuildFilesUnknown(_)
    ));
    assert_eq!(
        plan.unmapped,
        [Unmapped {
            name: "game/Game.exe".into(),
            reason: UnmappedReason::SkippedLocation,
        }]
    );
}

#[tokio::test]
async fn links_below_the_game_dir_never_alias_a_build_file() {
    let mut f = fixture(INSTALL_CONFIG).await;
    let gf = installer_game(&mut f, &["Game.exe", "Data/levels.pak"]);
    fs::create_dir_all(gf.join("Data")).unwrap();
    fs::write(gf.join("Game.exe"), b"exe").unwrap();
    fs::write(gf.join("Data/levels.pak"), b"pak").unwrap();
    fs::create_dir_all(gf.join("SAVE")).unwrap();
    std::os::unix::fs::symlink("../Game.exe", gf.join("SAVE/lnk")).unwrap();
    std::os::unix::fs::symlink("../Data", gf.join("SAVE/d")).unwrap();
    fs::hard_link(gf.join("Game.exe"), gf.join("SAVE/hl.sav")).unwrap();
    fs::write(gf.join("SAVE/slot1.sav"), b"mine").unwrap();
    f.serve_names(&[
        "game/SAVE/lnk",
        "game/SAVE/d/levels.pak",
        "game/SAVE/hl.sav",
        // Another case and Windows separators: the same build file.
        "game/DATA\\LEVELS.PAK",
        "game/GAME.exe",
    ])
    .await;
    let plan = f.run().await.unwrap();
    assert_eq!(
        actions(&plan),
        [("game/SAVE/slot1.sav".to_string(), Action::Upload)],
        "nothing planned over a link, a hard link or a build file"
    );
    let unmapped: Vec<(&str, UnmappedReason)> = plan
        .unmapped
        .iter()
        .map(|u| (u.name.as_str(), u.reason))
        .collect();
    assert_eq!(
        unmapped,
        [
            ("game/DATA\\LEVELS.PAK", UnmappedReason::BuildFile),
            // Refused before the build-file test: `SAVE/hl.sav` made it a
            // hard link too.
            ("game/GAME.exe", UnmappedReason::BadPath),
            ("game/SAVE/d/levels.pak", UnmappedReason::BadPath),
            ("game/SAVE/hl.sav", UnmappedReason::BadPath),
            ("game/SAVE/lnk", UnmappedReason::BadPath),
        ]
    );
    let lp = &plan.locations[0];
    assert_eq!(lp.ignored, ["SAVE/d", "SAVE/hl.sav", "SAVE/lnk"]);
}
