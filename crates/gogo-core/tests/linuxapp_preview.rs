//! The read-only preview of a Linux build and set-up: it asks the questions in
//! the pipelines' order and changes nothing on disk.
#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use gogo::config::{ProtonDir, UmuMode};
use gogo::linuxapp::preview::{self, Answers, Ask, BuildRequest, Env, Item, Preview, Question};
use gogo::linuxapp::umu::Host;
use gogo::macapp::catalog::{self, Discovery};
use gogo::macapp::launch::{GogoRef, LaunchToml};
use gogo::macapp::prompt::PromptAnswer;
use gogo::macapp::renderer::RendererChoice;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ID: &str = "1207658924";

struct Scratch {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

/// A fake machine under the target dir (the pipelines refuse `/tmp`): gogo's
/// home, a user with a Steam, and a `python3` on `PATH`.
fn scratch(config: &str) -> Scratch {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let root = dir.path().to_path_buf();
    // SAFETY: nextest runs each test in its own process.
    unsafe { std::env::set_var("GOGO_HOME", root.join("gogo")) };
    std::fs::create_dir_all(root.join("home/.local/share/Steam")).unwrap();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let python = root.join("bin/python3");
    std::fs::write(&python, "#!/bin/sh\necho 'Python 3.12.1'\n").unwrap();
    std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
    if !config.is_empty() {
        std::fs::write(root.join("config.toml"), config).unwrap();
    }
    Scratch { _dir: dir, root }
}

impl Scratch {
    fn vars(&self) -> impl Fn(&str) -> Option<OsString> + '_ {
        move |key| match key {
            "HOME" => Some(self.root.join("home").into()),
            "PATH" => Some(self.root.join("bin").into()),
            _ => None,
        }
    }

    /// Every file and dir under the scratch root, with the files' bytes.
    fn tree(&self) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if e.file_type().unwrap().is_dir() {
                    out.insert(p.clone(), None);
                    walk(&p, out);
                } else {
                    out.insert(p.clone(), Some(std::fs::read(&p).unwrap_or_default()));
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&self.root, &mut out);
        out
    }

    /// A game with the Galaxy SDK in its files.
    fn game_tree(&self) -> PathBuf {
        let tree = self.root.join("cache/Game");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("Galaxy64.dll"), b"dll").unwrap();
        tree
    }

    fn built_game(&self) {
        let game = self.root.join("gogo/games").join(ID);
        std::fs::create_dir_all(game.join("game")).unwrap();
        std::fs::write(game.join("game/Galaxy64.dll"), b"dll").unwrap();
        let baked = LaunchToml {
            schema: gogo::macapp::launch::SCHEMA,
            gogid: ID.into(),
            title: "A Game".into(),
            runtime: "ge-proton".into(),
            renderer: "wined3d".into(),
            renderer_flag: false,
            exe: r"C:\GOG\Game\game.exe".into(),
            workdir: r"C:\GOG\Game".into(),
            args: Vec::new(),
            env: Default::default(),
            dll_overrides: Default::default(),
            verbs: Vec::new(),
            portable: None,
            gogo: GogoRef {
                path: "/usr/bin/gogo".into(),
                state: self.root.join("gogo"),
            },
            variants: Vec::new(),
            lane: None,
        };
        std::fs::write(
            game.join("gogo-launch.toml"),
            toml::to_string(&baked).unwrap(),
        )
        .unwrap();
    }
}

/// GitHub's releases for the pinned umu, GE-Proton and Comet.
async fn github() -> MockServer {
    let server = MockServer::start().await;
    for id in [catalog::UMU, catalog::GE_PROTON, catalog::COMET] {
        let Some(Discovery::Pinned(pin)) = &catalog::find(id).unwrap().discovery else {
            panic!("{id} is pinned to a release asset");
        };
        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/{}/releases/tags/{}",
                pin.repo, pin.tag
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "tag_name": pin.tag,
                "assets": [{
                    "name": pin.asset,
                    "size": 1000,
                    "browser_download_url": "http://127.0.0.1:1/never"
                }]
            })))
            .mount(&server)
            .await;
    }
    server
}

/// Preview until it is ready, answering each question with `answers`' pick.
fn walk<T>(
    answers: &mut Answers,
    mut preview: impl FnMut(&Answers) -> Preview<T>,
) -> (Vec<Question>, T) {
    let mut asked = Vec::new();
    loop {
        match preview(answers) {
            Preview::Ready(ready) => return (asked, ready),
            Preview::Ask(q) => {
                let answer = match q.ask {
                    Ask::InstallComet => PromptAnswer::Confirmed(true),
                    _ => PromptAnswer::Index(0),
                };
                assert!(answers.answer(&q, &answer), "{:?}", q.ask);
                asked.push(q);
                assert!(asked.len() < 8, "asks for ever");
            }
        }
    }
}

fn asks(asked: &[Question]) -> Vec<Ask> {
    asked.iter().map(|q| q.ask.clone()).collect()
}

fn proton_dirs() -> Ask {
    Ask::ProtonDir(vec![ProtonDir::Store, ProtonDir::Steam])
}

#[tokio::test(flavor = "multi_thread")]
async fn a_build_asks_everything_first_and_writes_nothing() {
    let server = github().await;
    let s = scratch("[linux]\ngalaxy_default = \"ask\"\n");
    let tree = s.game_tree();
    let before = s.tree();
    let vars = s.vars();
    let env = Env {
        config: s.root.join("config.toml"),
        host: Host {
            root: &s.root,
            env: &vars,
        },
        api: server.uri(),
        http: reqwest::Client::new(),
    };
    let req = BuildRequest {
        gogid: ID,
        tree: Some(&tree),
        runtime: None,
        renderer: RendererChoice::Auto,
        library: None,
    };
    let mut answers = Answers::default();
    let (asked, ready) = walk(&mut answers, |a| preview::build(&req, a, &env).unwrap());

    assert_eq!(
        asks(&asked),
        [Ask::Umu, proton_dirs(), Ask::Galaxy, Ask::InstallComet]
    );
    assert_eq!(s.tree(), before, "a preview writes nothing");
    assert!(!ready.rebuild && !ready.prefix_exists);
    assert_eq!(ready.tools.umu, UmuMode::Zipapp);
    assert!(ready.tools.galaxy && ready.tools.install_comet);
    let items: Vec<Item> = ready.tools.downloads.iter().map(|d| d.item).collect();
    assert_eq!(items, [Item::Umu, Item::Proton, Item::Comet]);
    assert_eq!(ready.dirs.root, s.root.join("gogo/games").join(ID));
    assert_eq!(ready.tree_bytes, Some(3));
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_files_galaxy_is_asked_in_case_they_ship_the_sdk() {
    let server = github().await;
    let s =
        scratch("[linux]\ngalaxy_default = \"ask\"\numu = \"zipapp\"\nproton_dir = \"store\"\n");
    let before = s.tree();
    let vars = s.vars();
    let env = Env {
        config: s.root.join("config.toml"),
        host: Host {
            root: &s.root,
            env: &vars,
        },
        api: server.uri(),
        http: reqwest::Client::new(),
    };
    let req = BuildRequest {
        gogid: ID,
        tree: None,
        runtime: None,
        renderer: RendererChoice::Auto,
        library: None,
    };
    let Preview::Ask(q) = preview::build(&req, &Answers::default(), &env).unwrap() else {
        panic!("asks about Galaxy");
    };
    assert_eq!(q.ask, Ask::Galaxy);
    assert!(q.spec.title.contains("if it ships"), "{}", q.spec.title);
    assert_eq!(s.tree(), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_set_up_asks_in_setups_order_and_stores_only_what_is_asked_to() {
    let server = github().await;
    let s = scratch("# kept\n[linux]\ngalaxy_default = \"ask\"\n");
    s.built_game();
    let before = s.tree();
    let vars = s.vars();
    let config = s.root.join("config.toml");
    let env = Env {
        config: config.clone(),
        host: Host {
            root: &s.root,
            env: &vars,
        },
        api: server.uri(),
        http: reqwest::Client::new(),
    };
    let mut answers = Answers::default();
    let (asked, ready) = walk(&mut answers, |a| preview::setup(ID, a, &env).unwrap());

    assert_eq!(
        asks(&asked),
        [Ask::Galaxy, proton_dirs(), Ask::Umu, Ask::InstallComet]
    );
    assert_eq!(ready.title, "A Game");
    assert!(!ready.prefix_exists);
    assert_eq!(s.tree(), before, "a preview writes nothing");

    // The pipeline's own question is answered; any other is not.
    assert_eq!(answers.replay(&asked[0].spec), Some(PromptAnswer::Index(0)));
    assert_eq!(answers.replay(&asked[1].spec), None);
    assert_eq!(answers.replay(&asked[2].spec), None);

    // After the confirm the stored answers aren't asked again.
    answers.store(&config).unwrap();
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(
        text.contains("# kept") && text.contains("umu = \"zipapp\""),
        "{text}"
    );
    assert!(text.contains("proton_dir = \"store\""), "{text}");
    let Preview::Ask(q) = preview::setup(ID, &Answers::default(), &env).unwrap() else {
        panic!("Galaxy is still the setup's to store");
    };
    assert_eq!(q.ask, Ask::Galaxy);
    let on_only = Answers {
        galaxy: answers.galaxy,
        install_comet: Some(false),
        ..Answers::default()
    };
    assert!(matches!(
        preview::setup(ID, &on_only, &env).unwrap(),
        Preview::Ready(_)
    ));
}
