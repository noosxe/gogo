//! S1 over the real adapter (design 26 §9, #535): no credential reaches any source, the
//! screen, assistive technology, an error, the log, standard output or a file.
//!
//! The app is driven as the shell drives it: frames, presentation, assistive-technology
//! presses and a paste. The engine is [`CoreBackend`] with gogo's own token store
//! handling over a store in memory, and GOG's API is a local server. Two things are
//! stood in for, because gog-warp's addresses are fixed: the code-for-token exchange
//! and the user name. Every credential here is made up (except GOG Galaxy's public
//! client secret, which must not surface either).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use gogo::auth::TokenProvider;
use gogo::auth::store::TokenStore;
use gogo::gog::endpoints::Endpoints;
use gogo::gog::error::AuthError;
use gogo_contract::sources;
use gogo_gui_model::{Model, SourceKey};
use gogo_ui_interact::PointerButton;
use gogo_ui_shell::accesskit::{Action, ActionRequest, Role, TreeId, TreeUpdate};
use gogo_ui_shell::{Input, ShellApp as _, Zeroizing};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;
use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};
use zeroize::Zeroizing as Wiped;

use crate::app::{App, SAID};
use crate::core::account::{Account, StoredAccount};
use crate::core::art::ArtStore;
use crate::core::browser::Browser;
use crate::core::tests::{NoBrowser, scratch, value};
use crate::core::{CoreBackend, Engine, Local, Machine, Parts};
use crate::data::Data;

const ACCESS: &str = "FAKE-ACCESS-Qm7x1c9e4b2a";
const REFRESH: &str = "FAKE-REFRESH-Zk3v8n5d1f6h";
const SESSION: &str = "FAKE-SESSION-Lp2w9s4g7j0t";
const BAD_CODE: &str = "FAKE-BADCODE-Rt5y8u1i4o7p";
const CODE: &str = "FAKE-CODE-Hn6b3v0c9x2z5m";

/// A gog-warp line that must get through: the bridge from `log` is in place.
const GOG_WARP_WARNING: &str = "a gog-warp warning";

/// A line that carries a code past [`gogo::redact::scrub_text`].
fn unscrubbed(code: &str) -> String {
    format!("exchanging code '{code}'")
}

/// Every credential, and enough of each to catch one that was cut short.
fn sentinels() -> Vec<String> {
    let secret = gog_warp::constants::GALAXY_CLIENT_SECRET;
    [ACCESS, REFRESH, SESSION, BAD_CODE, CODE, secret]
        .iter()
        .flat_map(|s| [(*s).to_string(), s[..16].to_string()])
        .collect()
}

/// The sentinels found in `text`.
fn leaks(text: &str) -> Vec<String> {
    let mut found = sentinels();
    found.retain(|s| text.contains(s.as_str()));
    found
}

/// The token store, in memory.
#[derive(Default)]
struct Memory(Mutex<Option<String>>);

impl TokenStore for Memory {
    fn load(&self) -> Result<Option<String>, AuthError> {
        Ok(self.0.lock().expect("store").clone())
    }

    fn save(&self, blob: &str) -> Result<(), AuthError> {
        *self.0.lock().expect("store") = Some(blob.to_string());
        Ok(())
    }

    fn clear(&self) -> Result<(), AuthError> {
        *self.0.lock().expect("store") = None;
        Ok(())
    }
}

/// gog-warp's token blob. Fresh, so that nothing tries to refresh it over the network.
fn blob() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("a clock")
        .as_secs();
    let client = gog_warp::constants::GALAXY_CLIENT_ID;
    format!(
        r#"{{"{client}":{{"access_token":"{ACCESS}","refresh_token":"{REFRESH}",
        "user_id":"7","expires_in":86400,"token_type":"bearer","scope":null,
        "session_id":"{SESSION}","login_time":{now}}}}}"#
    )
}

/// The stored session without gog-warp's client, which would call GOG itself.
struct Offline(Arc<dyn TokenProvider>);

#[async_trait]
impl TokenProvider for Offline {
    async fn bearer(&self) -> Result<String, AuthError> {
        self.0.bearer().await
    }

    fn user_id(&self) -> Option<u64> {
        self.0.user_id()
    }
}

/// gogo's [`StoredAccount`], with the exchange stood in for.
struct Exchange {
    store: Arc<Memory>,
    stored: Mutex<Arc<StoredAccount>>,
    /// The codes it was given.
    codes: Mutex<Vec<String>>,
}

impl Exchange {
    fn new() -> Arc<Self> {
        let store = Arc::new(Memory::default());
        Arc::new(Exchange {
            stored: Mutex::new(Arc::new(StoredAccount::new(store.clone()))),
            store,
            codes: Mutex::default(),
        })
    }

    fn stored(&self) -> Arc<StoredAccount> {
        Arc::clone(&self.stored.lock().expect("account"))
    }
}

#[async_trait]
impl Account for Exchange {
    async fn session(&self) -> Result<Option<Arc<dyn TokenProvider>>, AuthError> {
        let session = self.stored().session().await?;
        Ok(session.map(|s| Arc::new(Offline(s)) as Arc<dyn TokenProvider>))
    }

    async fn username(&self, _: &dyn TokenProvider) -> Result<Option<String>, AuthError> {
        Ok(Some("made_up_player".to_string()))
    }

    async fn login(&self, code: Wiped<String>) -> Result<(), AuthError> {
        let first = {
            let mut codes = self.codes.lock().expect("codes");
            codes.push(code.to_string());
            codes.len() == 1
        };
        // What gog-warp itself logs during the exchange this stands in for, and a
        // wording the scrubber does not know: only the level filter stops them.
        log::debug!(target: "gog_warp::core", "Requesting token with code {}", *code);
        log::debug!(target: "gog_warp::core", "{}", unscrubbed(&code));
        log::warn!(target: "gog_warp::core", "{GOG_WARP_WARNING}");
        if first {
            // The worst a library could say: nothing about it redacted.
            let secret = gog_warp::constants::GALAXY_CLIENT_SECRET;
            return Err(AuthError::Upstream(format!(
                "GET https://auth.gog.com/token?client_secret={secret}&code={} failed, \
                 refresh_token={REFRESH} access_token={ACCESS} session_id={SESSION} \
                 with code {}",
                *code, *code
            )));
        }
        self.store.save(&blob())?;
        *self.stored.lock().expect("account") = Arc::new(StoredAccount::new(self.store.clone()));
        Ok(())
    }

    async fn persist(&self) -> Result<(), AuthError> {
        self.stored().persist().await
    }
}

struct Nothing;

impl Local for Nothing {
    fn read(&self, _: &[u64]) -> Machine {
        Machine::default()
    }
}

/// A body that is not what was asked for and holds every credential.
fn junk() -> String {
    format!(
        r#"{{"access_token":"{ACCESS}","refresh_token":"{REFRESH}","session_id":"{SESSION}","page":"{CODE}"}}"#
    )
}

async fn gog() -> MockServer {
    let server = MockServer::start().await;
    let json = |body: String| ResponseTemplate::new(200).set_body_raw(body, "application/json");
    // The first library read fails with a body full of credentials.
    Mock::given(path("/account/getFilteredProducts"))
        .respond_with(json(junk()))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(path("/account/getFilteredProducts"))
        .respond_with(json(
            r#"{"page":1,"totalPages":1,"products":[{"id":10,"title":"Made Up","isGame":true}]}"#
                .to_string(),
        ))
        .mount(&server)
        .await;
    Mock::given(path("/user/data/games"))
        .respond_with(json(junk()))
        .mount(&server)
        .await;
    Mock::given(path("/products/10"))
        .respond_with(json(format!(
            r#"{{"id":10,"title":"Made Up","downloads":{{"installers":[{{"id":"a",
            "os":"windows","language":"en","total_size":1,"files":[{{"id":"f","size":1,
            "downlink":"https://api.gog.com/d?access_token={ACCESS}"}}]}}]}},
            "expanded_dlcs":[{{"id":11,"title":"More","downloads":{{"installers":[
            {{"id":"b","os":"windows","files":[]}}]}}}}]}}"#
        )))
        .mount(&server)
        .await;
    let uri = server.uri();
    Mock::given(path("/v2/games/10"))
        .respond_with(json(format!(
            r#"{{"_links":{{"boxArtImage":{{"href":"{uri}/box.jpg?token={SESSION}"}},
            "backgroundImage":{{"href":"{uri}/gone.jpg?access_token={ACCESS}"}}}},
            "_embedded":{{"product":{{"title":"Made Up"}}}}}}"#
        )))
        .mount(&server)
        .await;
    Mock::given(path("/box.jpg"))
        .respond_with(ResponseTemplate::new(200).set_body_string(junk()))
        .mount(&server)
        .await;
    server
}

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<u8>>>);

impl io::Write for Log {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("log").extend_from_slice(b);
        Ok(b.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Log {
    type Writer = Log;

    fn make_writer(&'a self) -> Log {
        self.clone()
    }
}

/// The Default skin with a library page that asks for what this test needs.
fn skin(dir: &Path) -> PathBuf {
    let root = dir.join("default");
    for (name, data) in gogo_skins::default().files {
        let mut data = data.to_vec();
        if *name == "skin.kdl" {
            let text = String::from_utf8(data).expect("utf-8");
            let routes = text
                .lines()
                .find(|l| l.trim_start().starts_with("routes "))
                .expect("routes");
            data = if routes.contains("\"library\"") {
                text.into_bytes()
            } else {
                text.replace(routes, &format!("{routes} \"library\""))
                    .into_bytes()
            };
        }
        let file = root.join(name);
        std::fs::create_dir_all(file.parent().expect("a folder")).expect("folders");
        std::fs::write(file, data).expect("a write");
    }
    let library = r#"route "library" {
    button "Connect account" action="account.sign_in" id="connect"
    button "Open the game" action="nav.go" args="game 10" id="open"
    button "Read again" action="library.refresh" id="again"
    text "{account.username}" id="who"
    each "g" in="{library()}" {
        text "{g.title} {g.status}"
        art "{g.art.cover}" w=40 h=60 label="{g.title} cover"
    }
}
"#;
    std::fs::write(root.join("routes/library.kdl"), library).expect("a route");
    root
}

/// The app, driven as the shell drives it.
struct Driven {
    app: App,
    now: Duration,
    /// Everything that was on screen or told to assistive technology.
    seen: String,
}

impl Driven {
    fn step(&mut self) -> TreeUpdate {
        self.now += Duration::from_millis(20);
        self.app.wake();
        let frame = self.app.frame(1280.0, 800.0, 1.0);
        // What a renderer does: ask for the pictures.
        for key in &frame.list.images {
            let _ = frame.images.get(key);
        }
        self.seen += &format!("{:?}", frame.list.images);
        self.app.presented(self.now, "cpu");
        let tree = self.app.accessibility_tree();
        self.seen += &format!("{tree:?}");
        self.seen += &self.app.dump();
        tree
    }

    fn has(tree: &TreeUpdate, text: &str) -> bool {
        tree.nodes.iter().any(|(_, n)| {
            n.label().is_some_and(|l| l.contains(text))
                || n.value().is_some_and(|v| v.contains(text))
        })
    }

    /// Steps until `text` is on the screen.
    async fn until(&mut self, text: &str) {
        let started = Instant::now();
        loop {
            let tree = self.step();
            if Self::has(&tree, text) {
                return;
            }
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "{text} never shown: {:?}",
                words(&tree)
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// A screen reader presses the button called `label`, after the screen has been up
    /// long enough to have been read.
    fn press(&mut self, label: &str) {
        self.step();
        self.now += Duration::from_millis(700);
        let tree = self.app.accessibility_tree();
        let (id, _) = tree
            .nodes
            .iter()
            .find(|(_, n)| n.role() == Role::Button && n.label() == Some(label))
            .unwrap_or_else(|| panic!("no button {label}: {:?}", words(&tree)));
        let request = ActionRequest {
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: *id,
            data: None,
        };
        self.app.accessibility_action(request, self.now);
    }

    fn paste(&mut self, text: &str) {
        self.press("Paste");
        assert!(self.app.wants_paste(), "the press asked for the clipboard");
        self.app.pasted(Some(Zeroizing::new(text.to_string())));
        assert!(!self.app.wants_paste(), "and only once");
    }
}

/// What assistive technology reads.
fn words(tree: &TreeUpdate) -> Vec<&str> {
    tree.nodes
        .iter()
        .filter_map(|(_, n)| n.label().or(n.value()))
        .collect()
}

fn redirect(code: &str) -> String {
    format!("https://embed.gog.com/on_login_success?origin=client&code={code}")
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_under(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// A press that began before the dialog was on the display reaches nothing on it, however
/// late it ends.
#[tokio::test(flavor = "multi_thread")]
async fn a_press_begun_before_the_dialog_cannot_paste() {
    let dir = scratch();
    let server = gog().await;
    let engine = Arc::new(Engine::new(Parts {
        account: Exchange::new(),
        local: Arc::new(Nothing),
        http: reqwest::Client::new(),
        endpoints: Endpoints::at(&server.uri()),
        language: "en".to_string(),
        download_dir: dir.path().join("downloads"),
        browser: Arc::new(NoBrowser),
    }));
    let backend = CoreBackend::new(engine, tokio::runtime::Handle::current(), None);
    let data = Data::core(backend, None, || {});
    let mut ui = Driven {
        app: App::new(Some(&skin(dir.path())), data).expect("the app"),
        now: Duration::from_secs(1),
        seen: String::new(),
    };
    ui.until("Connect account").await;

    // Where Paste will be.
    ui.press("Connect account");
    let tree = ui.step();
    let bounds = tree
        .nodes
        .iter()
        .find(|(_, n)| n.label() == Some("Paste"))
        .and_then(|(_, n)| n.bounds())
        .expect("a paste button");
    let (x, y) = ((bounds.x0 + bounds.x1) / 2.0, (bounds.y0 + bounds.y1) / 2.0);
    ui.press("Cancel");
    ui.until("Connect account").await;

    // The button goes down before the next dialog is shown, and comes up long after.
    let button = PointerButton::Primary;
    ui.press("Connect account");
    ui.app.input(Input::PointerMoved { x, y }, ui.now);
    ui.app.input(Input::PointerDown { button, x, y }, ui.now);
    ui.step();
    ui.now += Duration::from_secs(2);
    ui.app.input(Input::PointerUp { button, x, y }, ui.now);
    assert!(!ui.app.wants_paste());

    // The same press made afresh does ask.
    ui.app.input(Input::PointerDown { button, x, y }, ui.now);
    ui.app.input(Input::PointerUp { button, x, y }, ui.now);
    assert!(ui.app.wants_paste());
}

#[tokio::test(flavor = "multi_thread")]
async fn s1_no_credential_reaches_anything() {
    // The whole log, every level and every target, through the app's own subscriber.
    let log = Log::default();
    tracing_subscriber::util::SubscriberInitExt::try_init(crate::subscriber(
        EnvFilter::new("trace"),
        log.clone(),
        false,
    ))
    .expect("the only subscriber of this test process (nextest)");

    let dir = scratch();
    let server = gog().await;
    let account = Exchange::new();
    let parts = |account: Arc<Exchange>| Parts {
        account,
        local: Arc::new(Nothing),
        http: reqwest::Client::new(),
        endpoints: Endpoints::at(&server.uri()),
        language: "en".to_string(),
        download_dir: dir.path().join("downloads"),
        browser: Arc::new(NoBrowser) as Arc<dyn Browser>,
    };
    let engine = Arc::new(Engine::new(parts(account.clone())));
    let origin = server.uri();
    let art = ArtStore::new(
        Arc::clone(&engine),
        tokio::runtime::Handle::current(),
        Arc::new(move |url: &reqwest::Url| url.as_str().starts_with(&origin)),
        Some(dir.path().join("covers")),
        || {},
    )
    .expect("a store");
    let backend = CoreBackend::new(
        Arc::clone(&engine),
        tokio::runtime::Handle::current(),
        Some(art.clone()),
    );
    let data = Data::core(backend, Some(art), || {});
    let app = App::new(Some(&skin(dir.path())), data).expect("the app");
    let mut ui = Driven {
        app,
        now: Duration::from_secs(1),
        seen: String::new(),
    };

    // Signed out. A first try fails.
    ui.until("Connect account").await;
    ui.press("Connect account");
    ui.until("Sign in to GOG").await;
    // Too soon to have been seen: the clipboard is not asked for.
    let early = ui.app.accessibility_tree();
    let (paste, _) = early
        .nodes
        .iter()
        .find(|(_, n)| n.label() == Some("Paste"))
        .expect("a paste button");
    let request = ActionRequest {
        action: Action::Click,
        target_tree: TreeId::ROOT,
        target_node: *paste,
        data: None,
    };
    ui.app.accessibility_action(request, ui.now);
    assert!(!ui.app.wants_paste());

    ui.paste("nothing to do with signing in");
    ui.until("That was not a GOG sign-in address").await;
    ui.paste(&redirect(BAD_CODE));
    ui.until("A code is pasted.").await;
    ui.press("Sign in");
    ui.until("Sign-in failed").await;
    ui.press("Close");

    // A second try works; the library's first read does not.
    ui.until("Connect account").await;
    ui.press("Connect account");
    ui.until("Sign in to GOG").await;
    ui.paste(&redirect(CODE));
    ui.until("A code is pasted.").await;
    ui.press("Sign in");
    ui.until("The library could not be read").await;
    ui.press("Close");
    ui.until("made_up_player").await;
    ui.press("Read again");
    ui.until("Made Up not_installed").await;

    // The game page, with its failing DLC read and its pictures.
    ui.press("Open the game");
    ui.until("Verify files").await;
    for _ in 0..20 {
        ui.step();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Every source of the contract, from the same signed-in engine.
    let backend = CoreBackend::new(Arc::clone(&engine), tokio::runtime::Handle::current(), None);
    let mut model = Model::new(backend, || {});
    let mut sources_seen = String::new();
    let mut read = 0;
    for spec in sources::SOURCES {
        let required = spec.params.iter().filter(|p| !p.optional).count();
        let args = vec!["10"; required];
        let Some(key) = SourceKey::new(spec.name, &args) else {
            continue;
        };
        if matches!(spec.name, "queue" | "queue.summary" | "job" | "route") {
            model.pump();
            sources_seen += &format!("{:?}", model.value(&key));
        } else if spec.name != "ui" && spec.name != "plan.preview" {
            let got = value(&mut model, spec.name, &args).await;
            sources_seen += &format!("{}={got:?}\n", spec.name);
            read += 1;
        }
        // Whatever failed on the way is an error screen too.
        model.pump();
        sources_seen += &format!("{:?}", model.modal());
        model.dismiss();
    }

    // The positive controls: the credentials were there to be leaked.
    assert!(read >= 20, "only {read} sources");
    assert!(sources_seen.contains("made_up_player"), "{sources_seen}");
    assert_eq!(*account.codes.lock().expect("codes"), [BAD_CODE, CODE]);
    let kept = account.store.load().expect("store").expect("signed in");
    for credential in [ACCESS, REFRESH, SESSION] {
        assert!(kept.contains(credential));
    }
    let requests = server.received_requests().await.expect("recorded");
    let bearer = format!("Bearer {ACCESS}");
    assert!(
        requests.iter().any(|r| r
            .headers
            .get("authorization")
            .is_some_and(|v| v.as_bytes() == bearer.as_bytes())),
        "the access token was used"
    );
    assert!(!leaks(&format!("x{CODE}x")).is_empty(), "the scan finds");
    assert!(
        gogo::redact::scrub_text(&unscrubbed(CODE)).contains(CODE),
        "the scrubber would not have caught this line"
    );

    // And nowhere else.
    let log = String::from_utf8_lossy(&log.0.lock().expect("log")).into_owned();
    assert!(
        log.contains("Sign-in failed"),
        "the log was captured: {log}"
    );
    assert!(
        log.contains(GOG_WARP_WARNING),
        "`log` records arrive: {log}"
    );
    assert!(!log.contains("Requesting token with code"), "{log}");
    assert!(!log.contains("exchanging code"), "{log}");
    assert!(ui.seen.contains("Sign-in failed"));
    let said = SAID.lock().expect("said").join("\n");
    assert!(said.contains("gui: route game"), "{said}");
    for (what, text) in [
        ("a source", &sources_seen),
        ("the screen or the accessibility tree", &ui.seen),
        ("the log", &log),
        ("standard output", &said),
    ] {
        assert_eq!(leaks(text), Vec::<String>::new(), "in {what}");
    }
    let mut files = Vec::new();
    files_under(dir.path(), &mut files);
    assert!(!files.is_empty());
    for file in files {
        let bytes = std::fs::read(&file).expect("a file");
        assert_eq!(
            leaks(&String::from_utf8_lossy(&bytes)),
            Vec::<String>::new(),
            "in {}",
            file.display()
        );
    }
}
