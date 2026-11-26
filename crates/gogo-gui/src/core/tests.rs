//! The real adapter over a local server standing in for GOG, a canned account and a
//! canned machine. Nothing here talks to GOG or reads this machine's state.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use gogo::auth::TokenProvider;
use gogo::gog::endpoints::Endpoints;
use gogo::gog::error::AuthError;
use gogo_contract::{Value, actions, shapes, sources};
use gogo_gui_model::{InputEvent, Invocation, Modal, Model, SourceKey};
use gogo_ui_scene::{ImageKey, ImageStore};
use image::ImageFormat;
use wiremock::matchers::{header, header_exists, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::account::Account;
use super::art::ArtStore;
use super::local::{Built, Local, Machine};
use super::{CoreBackend, Engine, Parts};

pub(crate) const ACCESS: &str = "FAKE-ACCESS-5b1f0c9d2e";

struct Token;

#[async_trait]
impl TokenProvider for Token {
    async fn bearer(&self) -> Result<String, AuthError> {
        Ok(ACCESS.to_string())
    }

    fn user_id(&self) -> Option<u64> {
        Some(1)
    }
}

struct SignedIn(bool);

#[async_trait]
impl Account for SignedIn {
    async fn session(&self) -> Result<Option<Arc<dyn TokenProvider>>, AuthError> {
        Ok(self.0.then(|| Arc::new(Token) as Arc<dyn TokenProvider>))
    }

    async fn username(&self, _: &dyn TokenProvider) -> Result<Option<String>, AuthError> {
        Ok(Some("made_up_player".to_string()))
    }
}

/// Game 10 is built; nothing else is on this machine.
struct Canned;

impl Local for Canned {
    fn read(&self, _: &[u64]) -> Machine {
        Machine {
            built: HashMap::from([(
                10,
                Built {
                    linux: true,
                    bytes: 4096,
                    runtime: Some("ge-proton/1".to_string()),
                    installed_build: Some(("1.0".to_string(), "b1".to_string())),
                    ..Built::default()
                },
            )]),
            ..Machine::default()
        }
    }
}

const LIBRARY: &str = r#"{"page":1,"totalPages":1,"products":[
    {"id":10,"title":"Made Up","slug":"made_up","isGame":true},
    {"id":11,"title":"Nothing Real","isGame":true}]}"#;

const DETAIL: &str = r#"{"id":10,"title":"Made Up",
    "downloads":{"installers":[{"id":"en1","name":"setup","os":"windows",
        "language":"en","language_full":"English","total_size":100,
        "files":[{"id":"en1f","size":100,"downlink":"/products/10/downlink/installer/en1f"}]}]},
    "expanded_dlcs":[{"id":12,"title":"More","downloads":{"installers":[
        {"id":"en2","os":"windows","language":"en","total_size":5,
         "files":[{"id":"en2f","size":5,"downlink":"/x"}]}]}}]}"#;

fn page(server: &str) -> String {
    format!(
        r#"{{"description":"<p>Made &amp; up.</p><a href='https://example.com'>x</a>",
        "_links":{{"boxArtImage":{{"href":"{server}/box.jpg"}},
                  "galaxyBackgroundImage":{{"href":"{server}/away.jpg"}}}},
        "_embedded":{{"product":{{"title":"Made Up","globalReleaseDate":"2001-02-03"}},
            "developers":[{{"name":"Nobody"}}],"publishers":[{{"name":"No One"}}],
            "tags":[{{"name":"Puzzle"}}]}}}}"#
    )
}

/// GOG's endpoints, made up.
pub(crate) async fn gog() -> MockServer {
    let server = MockServer::start().await;
    let bearer = format!("Bearer {ACCESS}");
    let json =
        |body: &str| ResponseTemplate::new(200).set_body_raw(body.to_string(), "application/json");
    Mock::given(path("/account/getFilteredProducts"))
        .and(header("authorization", bearer.as_str()))
        .respond_with(json(LIBRARY))
        .mount(&server)
        .await;
    Mock::given(path("/products/10"))
        .and(header("authorization", bearer.as_str()))
        .respond_with(json(DETAIL))
        .mount(&server)
        .await;
    Mock::given(path("/user/data/games"))
        .respond_with(json(r#"{"owned":[10,12]}"#))
        .mount(&server)
        .await;
    Mock::given(path("/v2/games/10"))
        .respond_with(json(&page(&server.uri())))
        .mount(&server)
        .await;
    let jpeg = super::art::tests::encoded(40, 60, ImageFormat::Jpeg);
    Mock::given(path("/box.jpg"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(jpeg))
        .mount(&server)
        .await;
    // A redirect off the allowed server.
    Mock::given(path("/away.jpg"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "http://127.0.0.1:9/b.jpg"),
        )
        .mount(&server)
        .await;
    // Pictures never carry a credential.
    for picture in ["/box.jpg", "/away.jpg", "/v2/games/10"] {
        Mock::given(path(picture))
            .and(header_exists("authorization"))
            .respond_with(ResponseTemplate::new(500))
            .with_priority(1)
            .expect(0)
            .mount(&server)
            .await;
    }
    server
}

pub(crate) struct Rig {
    pub model: Model<CoreBackend>,
    pub engine: Arc<Engine>,
    dir: tempfile::TempDir,
}

pub(crate) fn rig(server: &MockServer, account: Arc<dyn Account>) -> Rig {
    let dir = tempfile::tempdir().expect("a scratch folder");
    let engine = Arc::new(Engine::new(Parts {
        account,
        local: Arc::new(Canned),
        http: reqwest::Client::new(),
        endpoints: Endpoints::at(&server.uri()),
        language: "en".to_string(),
        download_dir: PathBuf::from(dir.path()),
    }));
    let backend = CoreBackend::new(Arc::clone(&engine), tokio::runtime::Handle::current(), None);
    Rig {
        model: Model::new(backend, || {}),
        engine,
        dir,
    }
}

/// Watches `key` and pumps until it has a value.
pub(crate) async fn value(model: &mut Model<CoreBackend>, name: &str, args: &[&str]) -> Value {
    let key = SourceKey::new(name, args).expect("a source");
    model.watch(&key);
    let started = Instant::now();
    loop {
        model.pump();
        if let Some(value) = model.value(&key) {
            return value.clone();
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "{name} never arrived"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn field<'a>(value: &'a Value, name: &str) -> &'a Value {
    match value {
        Value::Map(m) => m
            .get(name)
            .unwrap_or_else(|| panic!("no {name} in {value:?}")),
        other => panic!("not a record: {other:?}"),
    }
}

fn items(value: &Value) -> &[Value] {
    match value {
        Value::List(l) => l,
        other => panic!("not a list: {other:?}"),
    }
}

/// Every field the contract names, and no other.
fn assert_shape(name: &str, value: &Value) {
    let ty = shapes::source_shape(name).expect("a shape");
    let (shape, records) = match (&ty, value) {
        (gogo_contract::FieldType::List(item), Value::List(l)) => (*item, l.iter().collect()),
        (_, v) => (&ty, vec![v]),
    };
    let gogo_contract::FieldType::Record(shape) = shape else {
        return;
    };
    let mut want: Vec<&str> = shape.all_fields().iter().map(|f| f.name).collect();
    want.sort_unstable();
    for record in records {
        let Value::Map(m) = record else {
            panic!("{name}: not a record: {record:?}");
        };
        let have: Vec<&str> = m.keys().map(String::as_str).collect();
        assert_eq!(have, want, "{name}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_library_and_a_game_come_from_the_engine() {
    let server = gog().await;
    let mut rig = rig(&server, Arc::new(SignedIn(true)));
    let model = &mut rig.model;

    let account = value(model, "account", &[]).await;
    assert_eq!(field(&account, "username"), &Value::str("made_up_player"));

    let library = value(model, "library", &[]).await;
    assert_shape("library", &library);
    let first = &items(&library)[0];
    assert_eq!(field(first, "status"), &Value::str("ready"));
    assert_eq!(field(first, "runs_via"), &Value::str("ge-proton/1"));
    assert_eq!(
        field(&items(&library)[1], "status"),
        &Value::str("not_installed")
    );

    let stats = value(model, "library.stats", &[]).await;
    assert_eq!(field(&stats, "count"), &Value::Num(2.0));
    assert_eq!(field(&stats, "installed"), &Value::Num(1.0));

    let game = value(model, "game", &["10"]).await;
    assert_shape("game", &game);
    assert_eq!(field(&game, "description"), &Value::str("Made & up.\nx"));
    assert_eq!(field(&game, "year"), &Value::Num(2001.0));
    assert_eq!(field(&game, "lane"), &Value::str("linux_app"));
    // Art is a handle, not the address the page gave.
    assert!(!format!("{game:?}").contains("http"), "{game:?}");

    let files = value(model, "game.files", &["10"]).await;
    assert_shape("game.files", &files);
    assert_eq!(field(&items(&files)[0], "downloaded"), &Value::Bool(false));
    assert!(!format!("{files:?}").contains("downlink"), "{files:?}");

    let dlc = value(model, "game.dlc", &["10"]).await;
    assert_shape("game.dlc", &dlc);
    assert_eq!(field(&items(&dlc)[0], "owned"), &Value::Bool(true));

    // No live session in a test: nothing, and no error.
    let builds = value(model, "game.builds", &["10"]).await;
    assert!(items(&builds).is_empty());
    assert!(
        model.modal().is_none(),
        "{:?}",
        model.modal().map(|m| m.ticket())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn every_source_gets_a_well_shaped_value() {
    let server = gog().await;
    let mut rig = rig(&server, Arc::new(SignedIn(true)));
    for spec in sources::SOURCES {
        let args: Vec<&str> = spec
            .params
            .iter()
            .filter(|p| !p.optional)
            .map(|_| "10")
            .collect();
        // The model answers its own, and widgets' sources have no key.
        if matches!(
            spec.name,
            "queue" | "queue.summary" | "job" | "route" | "ui" | "plan.preview"
        ) {
            continue;
        }
        if SourceKey::new(spec.name, &args).is_none() {
            continue;
        }
        let got = value(&mut rig.model, spec.name, &args).await;
        assert_shape(spec.name, &got);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn signed_out_there_is_nothing_and_nothing_is_asked() {
    let server = MockServer::start().await;
    let mut rig = rig(&server, Arc::new(SignedIn(false)));
    let account = value(&mut rig.model, "account", &[]).await;
    assert_eq!(field(&account, "signed_in"), &Value::Bool(false));
    assert!(items(&value(&mut rig.model, "library", &[]).await).is_empty());
    assert!(
        server
            .received_requests()
            .await
            .expect("recorded")
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_read_is_empty_and_raises_one_error_without_the_token() {
    let server = MockServer::start().await;
    Mock::given(path("/account/getFilteredProducts"))
        .respond_with(ResponseTemplate::new(500).set_body_string(ACCESS))
        .mount(&server)
        .await;
    let mut rig = rig(&server, Arc::new(SignedIn(true)));
    assert!(items(&value(&mut rig.model, "library", &[]).await).is_empty());
    rig.model.pump();
    match rig.model.modal() {
        Some(Modal::Error { error, .. }) => {
            assert_eq!(error.class, "Network");
            assert!(!format!("{error:?}").contains(ACCESS), "{error:?}");
        }
        other => panic!("{:?}", other.map(|m| m.ticket())),
    }
    // The other sources of the same read raise nothing more.
    rig.model.dismiss();
    value(&mut rig.model, "library.stats", &[]).await;
    value(&mut rig.model, "account", &[]).await;
    rig.model.pump();
    assert!(rig.model.modal().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn refresh_reads_again() {
    let server = gog().await;
    let mut rig = rig(&server, Arc::new(SignedIn(true)));
    value(&mut rig.model, "library", &[]).await;
    let asked = || async {
        let all = server.received_requests().await.expect("recorded");
        all.iter()
            .filter(|r| r.url.path() == "/account/getFilteredProducts")
            .count()
    };
    assert_eq!(asked().await, 1);
    let refresh = actions::action("library.refresh").expect("an action");
    rig.model.invoke(&Invocation::new(refresh), InputEvent(1));
    let started = Instant::now();
    while asked().await < 2 {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "never asked again"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cover_is_fetched_once_kept_and_never_follows_a_redirect_away() {
    let server = gog().await;
    let rig = rig(&server, Arc::new(SignedIn(true)));
    let origin = server.uri();
    let cache = rig.dir.path().join("covers");
    let arrived = Arc::new(AtomicBool::new(false));
    let store = |cache: PathBuf, arrived: Arc<AtomicBool>| {
        let origin = origin.clone();
        ArtStore::new(
            Arc::clone(&rig.engine),
            tokio::runtime::Handle::current(),
            Arc::new(move |url: &reqwest::Url| url.as_str().starts_with(&origin)),
            Some(cache),
            move || arrived.store(true, Ordering::SeqCst),
        )
        .expect("a store")
    };
    let art = store(cache.clone(), Arc::clone(&arrived));
    let cover = ImageKey::Handle("art:10:cover".into());
    assert!(art.get(&cover).is_none(), "not yet");
    let started = Instant::now();
    while !arrived.load(Ordering::SeqCst) {
        assert!(started.elapsed() < Duration::from_secs(20), "no cover");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(art.take_arrived());
    let image = art.get(&cover).expect("arrived");
    assert_eq!((image.width, image.height), (40, 60));
    assert!(cache.join("10-cover.jpg").is_file());

    // The background redirects elsewhere: refused, and nothing kept.
    let background = ImageKey::Handle("art:10:background".into());
    assert!(art.get(&background).is_none());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(art.get(&background).is_none());
    assert!(!cache.join("10-background.jpg").exists());

    // A second run reads the cover from the cache.
    let again = store(cache, Arc::default());
    assert!(again.get(&cover).is_none());
    let started = Instant::now();
    while again.get(&cover).is_none() {
        assert!(started.elapsed() < Duration::from_secs(20), "not read back");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let boxes = server.received_requests().await.expect("recorded");
    assert_eq!(
        boxes.iter().filter(|r| r.url.path() == "/box.jpg").count(),
        1
    );
}
