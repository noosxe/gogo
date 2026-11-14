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
