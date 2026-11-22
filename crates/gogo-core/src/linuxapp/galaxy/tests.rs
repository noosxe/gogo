//! galaxy-start against a fake `comet` (a Python script on the test's own
//! port) and made-up tokens in scratch dirs. No real Comet, no account, no
//! network.

use std::collections::HashMap;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::{AtomicUsize, Ordering};

use gog_warp::Core;
use gog_warp::auth::types::Token;

use super::*;
use crate::linuxapp::comet::Source;

const ACCESS: &str = "FAKE-ACCESS-3f9c1d77e2";
const REFRESH: &str = "FAKE-REFRESH-a81be0c4d6";
const USER_ID: &str = "48151623420001";
const SECRETS: [&str; 3] = [ACCESS, REFRESH, USER_ID];

fn creds(note: Option<&'static str>) -> Credentials {
    Credentials {
        access_token: Secret::new(ACCESS),
        refresh_token: Secret::new(REFRESH),
        user_id: Secret::new(USER_ID),
        username: "Player One".into(),
        note,
    }
}

struct FakeAccount {
    answer: Result<(), String>,
    calls: AtomicUsize,
}

impl FakeAccount {
    fn ok() -> Self {
        Self {
            answer: Ok(()),
            calls: AtomicUsize::new(0),
        }
    }
    fn err(why: &str) -> Self {
        Self {
            answer: Err(why.into()),
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl Account for FakeAccount {
    async fn credentials(&self, _deadline: Instant) -> Result<Credentials, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.answer.clone().map(|()| creds(None))
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn bash() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join("bash"))
        .find(|p| p.is_file())
        .expect("bash on PATH")
}

/// What the fake does once it has recorded its view of the hand-off.
#[derive(Clone, Copy)]
enum Fake {
    Listen,
    Exit,
    Never,
}

/// The fake comet, in its working dir (Comet's data dir): its argv, its
/// environment, its process group, a copy of the credential file (made-up
/// tokens) and the modes, then `ready`. `Listen` then waits for the test to
/// bind the port (a shell can't listen), `Exit` exits 3, `Never` waits.
fn fake_comet(dir: &Path, mode: Fake) -> PathBuf {
    let then = match mode {
        Fake::Listen | Fake::Never => "exec sleep 60\n",
        Fake::Exit => "exit 3\n",
    };
    let script = format!("#!{}\n{FAKE_BODY}{then}", bash().display());
    let path = dir.join("comet");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

const FAKE_BODY: &str = r#"f="$XDG_CONFIG_PATH/heroic/gog_store/auth.json"
printf '%s\n' "$@" > argv
env > env
read -r -a st < /proc/self/stat
[ "${st[4]}" = "$$" ] && echo yes > own-group
cp "$f" auth.json
stat -c '%a' "$f" "$(dirname "$f")" "$XDG_CONFIG_PATH" > modes
echo ok > ready
"#;

struct Capture {
    argv: Vec<String>,
    env: HashMap<String, String>,
    own_group: bool,
    auth: serde_json::Value,
    modes: String,
}

struct World {
    _tmp: tempfile::TempDir,
    runtime: PathBuf,
    comet_dir: PathBuf,
    bin: PathBuf,
    home: PathBuf,
}

impl World {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("run");
        std::fs::create_dir(&runtime).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        Self {
            comet_dir: tmp.path().join("data/comet"),
            runtime,
            bin,
            home,
            _tmp: tmp,
        }
    }

    fn env(&self) -> impl Fn(&str) -> Option<OsString> + Sync + '_ {
        move |k| match k {
            "HOME" => Some(self.home.clone().into()),
            "PATH" => std::env::var_os("PATH"),
            "XDG_RUNTIME_DIR" => Some(self.runtime.clone().into()),
            "LANG" => Some("C.UTF-8".into()),
            // Never passed on.
            "WINEPREFIX" => Some("/somewhere/pfx".into()),
            "RUST_LOG" => Some("trace".into()),
            _ => None,
        }
    }

    async fn start(
        &self,
        comet: Result<CometBin, String>,
        port: u16,
        budget: Duration,
        account: &dyn Account,
    ) -> Outcome {
        let env = self.env();
        let s = Start {
            comet,
            runtime_dir: Some(self.runtime.clone().into()),
            comet_dir: self.comet_dir.clone(),
            port,
            budget,
            env: &env,
        };
        start(&s, account).await
    }

    /// Wait for the fake's `ready`, then read what it recorded.
    fn capture(&self) -> Capture {
        let dir = &self.comet_dir;
        for _ in 0..250 {
            if dir.join("ready").exists() {
                let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap_or_default();
                return Capture {
                    argv: read("argv").lines().map(str::to_string).collect(),
                    env: read("env")
                        .lines()
                        .filter_map(|l| l.split_once('='))
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect(),
                    own_group: read("own-group").trim() == "yes",
                    auth: serde_json::from_str(&read("auth.json")).unwrap_or_default(),
                    modes: read("modes").trim().to_string(),
                };
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the fake comet recorded nothing");
    }

    fn auth_dirs(&self) -> Vec<PathBuf> {
        std::fs::read_dir(self.runtime.join(RUNTIME_SUBDIR))
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .unwrap()
                            .to_string_lossy()
                            .starts_with(AUTH_PREFIX)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn store_bin(path: PathBuf) -> Result<CometBin, String> {
    Ok(CometBin {
        path,
        source: Source::Store,
    })
}

fn kill(pid: u32) {
    if let Some(p) = rustix::process::Pid::from_raw(pid as i32) {
        let _ = rustix::process::kill_process(p, rustix::process::Signal::KILL);
    }
}

fn assert_no_secret(what: &str, text: &str) {
    for s in SECRETS {
        assert!(!text.contains(s), "{what} holds a secret: {text}");
    }
}

/// Bind `port` once the fake comet has recorded its view, as Comet binds
/// after reading the file. The thread's result keeps the port open.
fn listen_when_ready(w: &World, port: u16) -> std::thread::JoinHandle<TcpListener> {
    let ready = w.comet_dir.join("ready");
    std::thread::spawn(move || {
        while !ready.exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
        TcpListener::bind(("127.0.0.1", port)).unwrap()
    })
}

#[tokio::test]
async fn start_hands_over_through_a_private_file_and_deletes_it() {
    let w = World::new();
    let port = free_port();
    let bin = fake_comet(&w.bin, Fake::Listen);
    let account = FakeAccount::ok();
    let bound = listen_when_ready(&w, port);
    let out = w.start(store_bin(bin), port, BUDGET, &account).await;
    let _listener = bound.join().unwrap();
    let Outcome::Started {
        pid,
        ref comet,
        note,
    } = out
    else {
        panic!("{out:?}");
    };
    let cap = w.capture();
    let comet_start = proc_start(pid).unwrap();
    kill(pid);
    assert_eq!(comet, "comet 0.4.1");
    assert_eq!(note, None);
    assert_eq!(out.line(), format!("comet 0.4.1 started (pid {pid})"));

    // The nested Heroic shape (design 23 §1.1); file 0600, its dirs 0700.
    let entry = &cap.auth[GALAXY_CLIENT_ID];
    assert_eq!(entry["access_token"], ACCESS);
    assert_eq!(entry["refresh_token"], REFRESH);
    assert_eq!(entry["user_id"], USER_ID);
    assert_eq!(
        cap.auth.as_object().map(|o| o.len()),
        Some(1),
        "{}",
        cap.auth
    );
    assert_eq!(cap.modes, "600\n700\n700");

    // Never argv, never the environment.
    assert_eq!(
        cap.argv,
        ["--from-heroic", "--username", "Player One", "--quit"]
    );
    let set = [
        "XDG_CONFIG_PATH",
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "COMET_LOG",
        // bash's own.
        "PWD",
        "SHLVL",
        "_",
        "OLDPWD",
    ];
    for k in cap.env.keys() {
        assert!(
            ENV_ALLOW.contains(&k.as_str()) || set.contains(&k.as_str()),
            "{k} reached comet"
        );
    }
    assert!(!cap.env.contains_key("WINEPREFIX") && !cap.env.contains_key("RUST_LOG"));
    let env = |k: &str| cap.env.get(k).cloned().unwrap_or_default();
    assert_eq!(env("COMET_LOG"), "off");
    assert_eq!(env("HOME"), w.home.to_str().unwrap());
    assert_eq!(
        env("XDG_DATA_HOME"),
        w.comet_dir.join("data").to_str().unwrap()
    );
    assert_eq!(
        env("XDG_CONFIG_HOME"),
        w.comet_dir.join("config").to_str().unwrap()
    );
    assert_eq!(
        env("XDG_CACHE_HOME"),
        w.comet_dir.join("cache").to_str().unwrap()
    );
    assert!(env("XDG_CONFIG_PATH").starts_with(w.runtime.join("gogo/auth-").to_str().unwrap()));
    assert!(cap.own_group, "comet must be in its own process group");
    assert_no_secret("argv", &cap.argv.join(" "));
    assert_no_secret("environ", &format!("{:?}", cap.env));
    assert_no_secret("the line", &out.line());

    // gogo names the Comet it started, for the launcher's stop.
    let owner = std::fs::read_to_string(w.runtime.join("gogo/comet/.comet")).unwrap();
    assert_eq!(owner, format!("{pid} {comet_start}\n"));

    // The file and its dir are gone once the port listens.
    assert!(w.auth_dirs().is_empty(), "{:?}", w.auth_dirs());
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&w.runtime.join("gogo")), 0o700);
    assert_eq!(mode(&w.comet_dir.join("data")), 0o700);
}

#[tokio::test]
async fn a_port_already_listening_is_reused_and_nothing_is_started() {
    let w = World::new();
    let held = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port();
    let account = FakeAccount::ok();
    let bin = fake_comet(&w.bin, Fake::Listen);
    let out = w.start(store_bin(bin), port, BUDGET, &account).await;
    let Outcome::Reusing { port: p, .. } = &out else {
        panic!("{out:?}");
    };
    assert_eq!(*p, port);
    assert!(out.line().starts_with(&format!("reusing port {port}")));
    assert_eq!(
        account.calls.load(Ordering::SeqCst),
        0,
        "no session touched"
    );
    assert!(!w.comet_dir.join("capture.json").exists());
    assert!(w.auth_dirs().is_empty());
}

#[tokio::test]
async fn gogos_own_comet_is_named_when_reused() {
    let w = World::new();
    let held = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port();
    let me = std::process::id();
    let reg = w.runtime.join("gogo/comet");
    std::fs::create_dir_all(&reg).unwrap();
    std::fs::write(
        reg.join(".comet"),
        format!("{me} {}", proc_start(me).unwrap()),
    )
    .unwrap();
    let out = w
        .start(Err("unused".into()), port, BUDGET, &FakeAccount::ok())
        .await;
    assert_eq!(
        out.line(),
        format!("reusing port {port} (gogo's comet, pid {me})")
    );
    // A stale record (wrong start time) names nothing of gogo's.
    std::fs::write(reg.join(".comet"), format!("{me} 1")).unwrap();
    let out = w
        .start(Err("unused".into()), port, BUDGET, &FakeAccount::ok())
        .await;
    assert!(!out.line().contains("gogo's comet"), "{}", out.line());
}

#[tokio::test]
async fn no_private_runtime_dir_skips_without_touching_anything() {
    let w = World::new();
    let account = FakeAccount::ok();
    let env = w.env();
    let base = |rt: Option<OsString>| Start {
        comet: Err("unused".into()),
        runtime_dir: rt,
        comet_dir: w.comet_dir.clone(),
        port: free_port(),
        budget: BUDGET,
        env: &env,
    };
    let out = start(&base(None), &account).await;
    assert_eq!(out.line(), "skipped: no XDG_RUNTIME_DIR");
    let out = start(&base(Some("".into())), &account).await;
    assert_eq!(out.line(), "skipped: no XDG_RUNTIME_DIR");
    let out = start(&base(Some("relative/run".into())), &account).await;
    assert!(out.line().contains("isn't a private dir"), "{}", out.line());

    let open = w.home.join("open");
    std::fs::create_dir(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = start(&base(Some(open.clone().into())), &account).await;
    assert!(out.line().contains("isn't a private dir"), "{}", out.line());
    assert!(!open.join("gogo").exists());

    let link = w.home.join("link");
    std::os::unix::fs::symlink(&w.runtime, &link).unwrap();
    let out = start(&base(Some(link.into())), &account).await;
    assert!(out.line().contains("isn't a private dir"), "{}", out.line());
    assert_eq!(account.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn account_and_binary_refusals_are_skips() {
    let w = World::new();
    let port = free_port();
    let out = w
        .start(
            Err("comet not installed (gogo runtimes install comet)".into()),
            port,
            BUDGET,
            &FakeAccount::ok(),
        )
        .await;
    assert_eq!(
        out.line(),
        "skipped: comet not installed (gogo runtimes install comet)"
    );
    let bin = fake_comet(&w.bin, Fake::Listen);
    let out = w
        .start(
            store_bin(bin),
            port,
            BUDGET,
            &FakeAccount::err(NOT_SIGNED_IN),
        )
        .await;
    assert_eq!(out.line(), "skipped: not signed in (gogo login)");
    assert!(!w.comet_dir.join("capture.json").exists());
}

#[tokio::test]
async fn a_comet_that_exits_or_never_listens_is_skipped_and_the_file_goes() {
    let w = World::new();
    let port = free_port();
    let bin = fake_comet(&w.bin, Fake::Exit);
    let out = w
        .start(store_bin(bin), port, BUDGET, &FakeAccount::ok())
        .await;
    assert_eq!(
        out.line(),
        format!("skipped: comet exited (code 3) before opening port {port}")
    );
    assert!(w.auth_dirs().is_empty());

    let w = World::new();
    let bin = fake_comet(&w.bin, Fake::Never);
    let t0 = Instant::now();
    let out = w
        .start(
            store_bin(bin),
            port,
            Duration::from_millis(1500),
            &FakeAccount::ok(),
        )
        .await;
    assert_eq!(
        out.line(),
        format!("skipped: port {port} not open after 1.5s")
    );
    assert!(t0.elapsed() < Duration::from_secs(3));
    assert!(w.auth_dirs().is_empty());
    // It got the file before the budget ran out.
    let cap = w.capture();
    assert_eq!(cap.modes, "600\n700\n700");
}

#[tokio::test]
async fn stale_credential_dirs_of_dead_runs_are_swept() {
    let w = World::new();
    let ours = w.runtime.join("gogo");
    std::fs::create_dir(&ours).unwrap();
    std::fs::set_permissions(&ours, std::fs::Permissions::from_mode(0o700)).unwrap();
    // pid_max is at most 2^22: this pid can't be running.
    let dead = ours.join("auth-4194999-0badf00d0");
    std::fs::create_dir_all(dead.join("heroic/gog_store")).unwrap();
    std::fs::write(dead.join("heroic/gog_store/auth.json"), "{}").unwrap();
    let me = std::process::id();
    // A live run's dir: its pid and start time match.
    let live = ours.join(format!("auth-{me}-{}-1", proc_start(me).unwrap()));
    std::fs::create_dir(&live).unwrap();
    // The same pid with another start time: a dead run's, the pid reused.
    let reused = ours.join(format!("auth-{me}-1-1"));
    std::fs::create_dir_all(reused.join("heroic/gog_store")).unwrap();
    std::fs::write(reused.join("heroic/gog_store/auth.json"), "{}").unwrap();
    let port = free_port();
    let bin = fake_comet(&w.bin, Fake::Exit);
    let _ = w
        .start(store_bin(bin), port, BUDGET, &FakeAccount::ok())
        .await;
    assert!(!dead.exists());
    assert!(!reused.exists());
    assert!(live.exists());
}

#[test]
fn the_line_never_holds_the_username_or_tokens() {
    let o = Outcome::Started {
        comet: "comet 0.4.1".into(),
        pid: 42,
        note: Some("with the stored session (the refresh failed)"),
    };
    assert_eq!(
        o.line(),
        "comet 0.4.1 started (pid 42) with the stored session (the refresh failed)"
    );
    assert_eq!(format!("{:?}", creds(None)), "Credentials(<redacted>)");
    assert_no_secret("Debug", &format!("{:?}", creds(None)));
}

#[test]
fn ss_output_names_the_holder() {
    let line = "LISTEN 0 128 127.0.0.1:9977 0.0.0.0:* users:((\"comet\",pid=4321,fd=9))\n";
    assert_eq!(parse_ss(line).as_deref(), Some("comet, pid 4321"));
    assert_eq!(parse_ss("LISTEN 0 128 127.0.0.1:9977 0.0.0.0:*\n"), None);
}

#[test]
fn listening_sees_a_bound_port_without_connecting() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    assert!(listening(port));
    drop(l);
    assert!(!listening(port));
}

// ------------------------------------------------------------ the session

fn galaxy_token(expires_in: u32) -> Token {
    serde_json::from_value(serde_json::json!({
        "access_token": ACCESS,
        "refresh_token": REFRESH,
        "user_id": USER_ID,
        "expires_in": expires_in,
        "token_type": "bearer",
        "session_id": "s",
        "login_time": jiff::Timestamp::now().as_second()
    }))
    .unwrap()
}

/// A main session on a real `Core` holding a fresh made-up token (no
/// network), or one whose refresh fails.
struct Main {
    core: Core,
    refresh: Result<(), fn() -> AuthError>,
}

impl Main {
    fn new(refresh: Result<(), fn() -> AuthError>) -> Self {
        let core = Core::new();
        let map = HashMap::from([(GALAXY_CLIENT_ID.to_string(), galaxy_token(3600))]);
        core.deserialize_tokens(&serde_json::to_string(&map).unwrap())
            .unwrap();
        Self { core, refresh }
    }
}

#[async_trait::async_trait]
impl TokenProvider for Main {
    async fn bearer(&self) -> Result<String, AuthError> {
        match self.refresh {
            Ok(()) => Ok(self
                .core
                .obtain_galaxy_token()
                .await?
                .access_token()
                .clone()),
            Err(e) => Err(e()),
        }
    }
    fn user_id(&self) -> Option<u64> {
        None
    }
    fn gog_warp_core(&self) -> Option<&Core> {
        Some(&self.core)
    }
}

fn deadline() -> Instant {
    Instant::now() + BUDGET
}

#[tokio::test]
async fn credentials_come_from_the_refreshed_main_session_and_are_persisted() {
    let tmp = tempfile::tempdir().unwrap();
    let main = Main::new(Ok(()));
    let saves = AtomicUsize::new(0);
    let persist = || {
        saves.fetch_add(1, Ordering::SeqCst);
        Ok(())
    };
    let c = credentials_from(
        &main,
        &persist,
        async { Some("Player One".into()) },
        tmp.path(),
        deadline(),
    )
    .await
    .unwrap();
    assert_eq!(saves.load(Ordering::SeqCst), 1);
    assert_eq!(c.access_token.expose(), ACCESS);
    assert_eq!(c.refresh_token.expose(), REFRESH);
    assert_eq!(c.user_id.expose(), USER_ID);
    assert_eq!(c.username, "Player One");
    assert_eq!(c.note, None);
    // The name is cached for an offline start, 0600.
    let cache = tmp.path().join(USERNAME_CACHE);
    assert_eq!(std::fs::read_to_string(&cache).unwrap(), "Player One\n");
    assert_eq!(
        std::fs::metadata(&cache).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[tokio::test]
async fn a_failed_refresh_starts_with_the_stored_tokens_and_the_cached_name() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join(USERNAME_CACHE), "Cached Name\n").unwrap();
    let main = Main::new(Err(|| {
        // Its text holds what a refresh URL would: it is never shown.
        AuthError::Upstream(format!(
            "GET https://auth.gog.com/token?refresh_token={REFRESH}"
        ))
    }));
    let persist = || Ok(());
    let c = credentials_from(
        &main,
        &persist,
        async { panic!("not asked offline") },
        tmp.path(),
        deadline(),
    )
    .await
    .unwrap();
    assert_eq!(c.note, Some("with the stored session (the refresh failed)"));
    assert_eq!(c.username, "Cached Name");
    assert_eq!(c.refresh_token.expose(), REFRESH);

    // No cache either: the placeholder.
    let tmp = tempfile::tempdir().unwrap();
    let c = credentials_from(&main, &persist, async { None }, tmp.path(), deadline())
        .await
        .unwrap();
    assert_eq!(c.username, PLACEHOLDER_USER);
}

#[tokio::test]
async fn not_signed_in_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let main = Main::new(Err(|| AuthError::Unauthorized));
    let persist = || Ok(());
    let e = credentials_from(&main, &persist, async { None }, tmp.path(), deadline())
        .await
        .unwrap_err();
    assert_eq!(e, NOT_SIGNED_IN);
    assert_no_secret("the reason", &e);
}

#[tokio::test]
async fn a_name_unfit_for_argv_is_not_used() {
    let tmp = tempfile::tempdir().unwrap();
    let main = Main::new(Ok(()));
    let persist = || Ok(());
    let c = credentials_from(
        &main,
        &persist,
        async { Some("--evil".into()) },
        tmp.path(),
        deadline(),
    )
    .await
    .unwrap();
    assert_eq!(c.username, PLACEHOLDER_USER);
    assert!(!tmp.path().join(USERNAME_CACHE).exists());
}
