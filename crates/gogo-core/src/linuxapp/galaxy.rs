//! Starting Comet for a launch (design 23 §2.1, decisions 5–8; #488): the
//! internal `gogo linux-app galaxy-start <gogid> --launcher` that `launch.sh`
//! runs before the game when `launch.conf` has `GALAXY=1`.
//!
//! It hands Comet the user's GOG session without ever putting a token on a
//! command line: a Heroic-shaped `auth.json` (mode 0600) in a fresh 0700 dir
//! under `$XDG_RUNTIME_DIR/gogo/`, which Comet's `--from-heroic` reads before
//! it binds its port, deleted as soon as the port listens. Everything is
//! bounded by one [`BUDGET`]; every outcome is one [`Outcome::line`] for the
//! run log, and nothing here ever stops a launch.
//!
//! SECURITY: the tokens live in [`Secret`]s and leave this process only
//! through the file. gog-warp's `Token` and `UserData` derive `Debug` with
//! their secrets and the email: they are never formatted. Auth errors are
//! matched by variant, never formatted (their text can hold the refresh
//! URL, #501). The caller turns tracing off for this command. Comet gets a
//! cleared environment with an allowlist, `/dev/null` for stdio and
//! `COMET_LOG=off`; its output is never read.
//!
//! Stopping Comet and the registry of launchers using it are `launch.sh`'s
//! (design 23 §2.2): `$XDG_RUNTIME_DIR/gogo/comet/` ([`REGISTRY`]).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gog_warp::constants::GALAXY_CLIENT_ID;
use zeroize::Zeroizing;

use super::comet::CometBin;
use crate::auth::store::TokenStore;
use crate::auth::{Session, TokenProvider};
use crate::cloud::auth::Secret;
use crate::gog::error::AuthError;

/// Comet's fixed listener, `127.0.0.1:9977` (docs/23 §2.1).
pub const PORT: u16 = 9977;

/// The most a launch waits for all of this (decision 8).
pub const BUDGET: Duration = Duration::from_secs(5);

/// The refresh's share of the budget; the stored tokens are used after it.
const REFRESH_BUDGET: Duration = Duration::from_millis(2500);

/// The username lookup's share of the budget; the cached name after it.
const USERNAME_BUDGET: Duration = Duration::from_millis(1500);

/// How often the port and the child are looked at.
const POLL: Duration = Duration::from_millis(50);

/// gogo's dir under `$XDG_RUNTIME_DIR`.
pub const RUNTIME_SUBDIR: &str = "gogo";

/// `$XDG_RUNTIME_DIR/gogo/comet`: the launchers' lock, their `.users/<pid>`
/// records and `.comet`, the pid and start time of the Comet gogo started.
pub const REGISTRY: &str = "comet";

/// The credential dirs, `$XDG_RUNTIME_DIR/gogo/auth-<pid>-<nonce>/`.
const AUTH_PREFIX: &str = "auth-";

/// Comet's name for the user when GOG can't be asked and none is cached.
pub const PLACEHOLDER_USER: &str = "gogo";

/// The last username GOG gave, under `<data root>/comet/`.
const USERNAME_CACHE: &str = "username";

/// The environment Comet keeps from gogo's. Everything else goes: no
/// `WINEPREFIX`/`STEAM_COMPAT_DATA_PATH` (the launcher would count Comet as
/// the game's), no `LD_PRELOAD`, no `RUST_LOG`.
const ENV_ALLOW: &[&str] = &[
    "HOME",
    "PATH",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "TZ",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NIX_SSL_CERT_FILE",
    "XDG_RUNTIME_DIR",
];

/// What happened, for the run log's `# galaxy: ` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// gogo started Comet `comet` (its label), now listening, as `pid`.
    Started {
        comet: String,
        pid: u32,
        note: Option<&'static str>,
    },
    /// Something already listens on the port; `who` when it can be named.
    Reusing { port: u16, who: Option<String> },
    /// No Comet for this launch, and why.
    Skipped(String),
}

impl Outcome {
    /// The run log line, without its `# galaxy: ` prefix. It never holds a
    /// token, the user id or the username.
    pub fn line(&self) -> String {
        match self {
            Outcome::Started { comet, pid, note } => {
                format!(
                    "{comet} started (pid {pid}){}",
                    note.map(|n| format!(" {n}")).unwrap_or_default()
                )
            }
            Outcome::Reusing { port, who } => match who {
                Some(w) => format!("reusing port {port} ({w})"),
                None => format!("reusing port {port}"),
            },
            Outcome::Skipped(why) => format!("skipped: {why}"),
        }
    }
}

fn skipped(why: impl Into<String>) -> Outcome {
    Outcome::Skipped(why.into())
}

/// What Comet needs of the session. `Debug` names no value.
pub struct Credentials {
    pub access_token: Secret,
    pub refresh_token: Secret,
    pub user_id: Secret,
    /// The GOG username (Comet's `--username`), or [`PLACEHOLDER_USER`].
    pub username: String,
    /// Said after "started": the stored session was used.
    pub note: Option<&'static str>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials(<redacted>)")
    }
}

/// The user's session, as galaxy-start needs it. `Err` is the skip reason.
#[async_trait::async_trait]
pub trait Account: Send + Sync {
    async fn credentials(&self, deadline: Instant) -> Result<Credentials, String>;
}

fn left(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// The live account: the session restored from gogo's token store, as every
/// command does, persisted back after a refresh.
pub struct LiveAccount {
    pub store: Arc<dyn TokenStore>,
    /// `<data root>/comet`, where the username is cached.
    pub comet_dir: PathBuf,
}

#[async_trait::async_trait]
impl Account for LiveAccount {
    async fn credentials(&self, deadline: Instant) -> Result<Credentials, String> {
        let session = match tokio::time::timeout(
            left(deadline),
            Session::restore(self.store.clone()),
        )
        .await
        {
            Err(_) => return Err("the token store didn't answer in time".into()),
            Ok(Err(AuthError::Unauthorized)) => return Err(NOT_SIGNED_IN.into()),
            Ok(Err(AuthError::Deserialize(_))) => {
                return Err("the stored session is unreadable (gogo login)".into());
            }
            // Matched, never formatted (#501).
            Ok(Err(_)) => return Err("the token store failed".into()),
            Ok(Ok(s)) => s,
        };
        let store = self.store.clone();
        let main = &session;
        let persist = move || main.persist_if_changed(&*store);
        let core = session.gog_warp_core();
        let username = async move {
            match core {
                Some(c) => c.get_user_data().await.ok().map(|u| u.username().clone()),
                None => None,
            }
        };
        credentials_from(&session, &persist, username, &self.comet_dir, deadline).await
    }
}

const NOT_SIGNED_IN: &str = "not signed in (gogo login)";

/// Refresh the Galaxy token in the main session and persist it (design 22
/// §2.1 step 4), find the username, and copy out the three values Comet
/// reads. A refresh that fails or runs out of time leaves the stored tokens,
/// which Comet still gets. `username` asks GOG; the cache in `comet_dir`
/// answers when it can't.
pub async fn credentials_from(
    session: &dyn TokenProvider,
    persist: &(dyn Fn() -> Result<(), AuthError> + Send + Sync),
    username: impl Future<Output = Option<String>> + Send,
    comet_dir: &Path,
    deadline: Instant,
) -> Result<Credentials, String> {
    let refresh = left(deadline).min(REFRESH_BUDGET);
    let mut note = None;
    let refreshed = match tokio::time::timeout(refresh, session.bearer()).await {
        Ok(Ok(_)) => true,
        Ok(Err(AuthError::Unauthorized)) => return Err(NOT_SIGNED_IN.into()),
        Ok(Err(_)) => {
            note = Some("with the stored session (the refresh failed)");
            false
        }
        Err(_) => {
            note = Some("with the stored session (the refresh timed out)");
            false
        }
    };
    if persist().is_err() && note.is_none() {
        note = Some("(the refreshed session couldn't be saved)");
    }
    let Some(token) = session
        .gog_warp_core()
        .and_then(|c| c.get_token_raw(GALAXY_CLIENT_ID))
    else {
        return Err(NOT_SIGNED_IN.into());
    };
    let access_token = Secret::new(token.access_token().as_str());
    let refresh_token = Secret::new(token.refresh_token().as_str());
    let user_id = Secret::new(token.user_id().as_str());
    drop(token);
    if refresh_token.expose().is_empty() || user_id.expose().is_empty() {
        return Err(NOT_SIGNED_IN.into());
    }
    let fetched = if refreshed {
        let budget = left(deadline).min(USERNAME_BUDGET);
        tokio::time::timeout(budget, username)
            .await
            .ok()
            .flatten()
            .filter(|n| usable_name(n))
    } else {
        None
    };
    let username = match fetched {
        Some(name) => {
            let _ = cache_username(comet_dir, &name);
            name
        }
        None => cached_username(comet_dir).unwrap_or_else(|| PLACEHOLDER_USER.to_string()),
    };
    Ok(Credentials {
        access_token,
        refresh_token,
        user_id,
        username,
        note,
    })
}

/// A name fit for Comet's argv: short, printable, not an option.
fn usable_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name.chars().all(|c| !c.is_control())
}

fn cached_username(comet_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(comet_dir.join(USERNAME_CACHE)).ok()?;
    let name = raw.trim().to_string();
    usable_name(&name).then_some(name)
}

fn cache_username(comet_dir: &Path, name: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    private_dir_all(comet_dir)?;
    let path = comet_dir.join(USERNAME_CACHE);
    if std::fs::read_to_string(&path).is_ok_and(|s| s.trim() == name) {
        return Ok(());
    }
    let tmp = comet_dir.join(format!(".{USERNAME_CACHE}.{}", std::process::id()));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(format!("{name}\n").as_bytes())?;
    drop(f);
    std::fs::rename(&tmp, &path)
}

/// `dir` and its missing parents, the ones made here 0700.
fn private_dir_all(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

/// What a start needs besides the account.
pub struct Start<'a> {
    /// The Comet `[linux] comet` resolves to without a download; `Err` is the
    /// skip reason.
    pub comet: Result<CometBin, String>,
    /// `$XDG_RUNTIME_DIR`, as the environment has it.
    pub runtime_dir: Option<OsString>,
    /// `<data root>/comet`: Comet's own XDG data, config and cache.
    pub comet_dir: PathBuf,
    /// [`PORT`] outside tests.
    pub port: u16,
    /// [`BUDGET`] outside tests.
    pub budget: Duration,
    /// The environment Comet's allowlist reads from.
    pub env: &'a (dyn Fn(&str) -> Option<OsString> + Sync),
}

/// Start Comet, or say why not. Never fails: the launch goes on either way.
/// INT, TERM or HUP while it runs (the terminal's Ctrl-C, the launcher's
/// `timeout`) stop the Comet it spawned and delete the credential dir.
pub async fn start(s: &Start<'_>, account: &dyn Account) -> Outcome {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut int), Ok(mut term), Ok(mut hup)) = (
        signal(SignalKind::interrupt()),
        signal(SignalKind::terminate()),
        signal(SignalKind::hangup()),
    ) else {
        return run(s, account).await;
    };
    tokio::select! {
        o = run(s, account) => o,
        // Dropping `run` drops its guards: the dir goes, the child is killed.
        _ = int.recv() => skipped("interrupted"),
        _ = term.recv() => skipped("interrupted"),
        _ = hup.recv() => skipped("interrupted"),
    }
}

async fn run(s: &Start<'_>, account: &dyn Account) -> Outcome {
    let deadline = Instant::now() + s.budget;
    let runtime = match private_runtime_dir(s.runtime_dir.as_deref()) {
        Ok(d) => d,
        Err(why) => return skipped(why),
    };
    if listening(s.port) {
        return Outcome::Reusing {
            port: s.port,
            who: holder(&runtime, s.port),
        };
    }
    let comet = match &s.comet {
        Ok(c) => c,
        Err(why) => return skipped(why.clone()),
    };
    let Some(home) = (s.env)("HOME").filter(|h| !h.is_empty()) else {
        // Comet panics without it (design 23 §1.1).
        return skipped("HOME is unset");
    };
    let creds = match account.credentials(deadline).await {
        Ok(c) => c,
        Err(why) => return skipped(why),
    };
    sweep_stale(&runtime);
    let file = match CredentialFile::write(&runtime, &creds) {
        Ok(f) => f,
        Err(e) => {
            return skipped(format!("couldn't write the credential file ({})", e.kind()));
        }
    };
    let xdg = ["data", "config", "cache"].map(|d| s.comet_dir.join(d));
    for d in &xdg {
        if let Err(e) = private_dir_all(d) {
            return skipped(format!("couldn't make {} ({})", d.display(), e.kind()));
        }
    }
    let mut cmd = Command::new(&comet.path);
    cmd.env_clear();
    for key in ENV_ALLOW {
        if let Some(v) = (s.env)(key) {
            cmd.env(key, v);
        }
    }
    cmd.env("HOME", home)
        .env("XDG_CONFIG_PATH", file.root())
        .env("XDG_DATA_HOME", &xdg[0])
        .env("XDG_CONFIG_HOME", &xdg[1])
        .env("XDG_CACHE_HOME", &xdg[2])
        .env("COMET_LOG", "off")
        .args(["--from-heroic", "--username", &creds.username, "--quit"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .current_dir(&s.comet_dir);
    {
        use std::os::unix::process::CommandExt as _;
        // Out of the launcher's (and the terminal's) process group: no Ctrl-C,
        // no teardown of the game's group, no hangup (decision 6).
        cmd.process_group(0);
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return skipped(format!("comet didn't start ({})", e.kind())),
    };
    let mut child = Spawned(Some(child));
    let note = creds.note;
    drop(creds);
    loop {
        if listening(s.port) {
            // Comet read the file before it bound the port.
            drop(file);
            let pid = child.disarm();
            return Outcome::Started {
                comet: super::comet::label(comet),
                pid,
                note,
            };
        }
        match child.try_wait() {
            Some(code) => {
                return skipped(format!(
                    "comet exited ({code}) before opening port {}",
                    s.port
                ));
            }
            None if Instant::now() >= deadline => {
                return skipped(format!(
                    "port {} not open after {}s",
                    s.port,
                    s.budget.as_secs_f32()
                ));
            }
            None => tokio::time::sleep(POLL).await,
        }
    }
}

/// The child until it is known to listen: killed on drop.
struct Spawned(Option<Child>);

impl Spawned {
    fn try_wait(&mut self) -> Option<String> {
        let child = self.0.as_mut()?;
        match child.try_wait() {
            Ok(Some(st)) => {
                self.0 = None;
                Some(match st.code() {
                    Some(c) => format!("code {c}"),
                    None => {
                        use std::os::unix::process::ExitStatusExt as _;
                        format!("signal {}", st.signal().unwrap_or(0))
                    }
                })
            }
            _ => None,
        }
    }

    fn disarm(&mut self) -> u32 {
        self.0.take().map(|c| c.id()).unwrap_or(0)
    }
}

impl Drop for Spawned {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// `$XDG_RUNTIME_DIR/gogo`, made 0700 if missing. The runtime dir must be
/// an absolute, real dir of this user that nobody else can enter (the
/// basedir spec's 0700; tmpfs); anything else is skipped, never a
/// disk-backed fallback (decision 5).
fn private_runtime_dir(raw: Option<&std::ffi::OsStr>) -> Result<PathBuf, String> {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else {
        return Err("no XDG_RUNTIME_DIR".into());
    };
    let rt = PathBuf::from(raw);
    if !rt.is_absolute() || !owned_private_dir(&rt) {
        return Err("XDG_RUNTIME_DIR isn't a private dir of this user".into());
    }
    let ours = rt.join(RUNTIME_SUBDIR);
    match std::fs::symlink_metadata(&ours) {
        Err(_) => {
            use std::os::unix::fs::DirBuilderExt as _;
            let _ = std::fs::DirBuilder::new().mode(0o700).create(&ours);
        }
        // Ours, but made wider (a `mkdir -p` under a umask): narrowed.
        Ok(m)
            if m.is_dir() && {
                use std::os::unix::fs::MetadataExt as _;
                m.uid() == rustix::process::geteuid().as_raw()
            } =>
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&ours, std::fs::Permissions::from_mode(0o700));
        }
        Ok(_) => {}
    }
    if !owned_private_dir(&ours) {
        return Err(format!(
            "XDG_RUNTIME_DIR/{RUNTIME_SUBDIR} isn't a private dir of this user"
        ));
    }
    Ok(ours)
}

/// A dir (not a symlink to one), ours, with no group or other access.
fn owned_private_dir(p: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let Ok(m) = std::fs::symlink_metadata(p) else {
        return false;
    };
    m.is_dir() && m.uid() == rustix::process::geteuid().as_raw() && m.mode() & 0o077 == 0
}

/// Credential dirs a run that died (SIGKILL, power) left: their pid is gone.
fn sweep_stale(runtime: &Path) {
    let Ok(rd) = std::fs::read_dir(runtime) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_prefix(AUTH_PREFIX))
            .and_then(|n| n.split('-').next())
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        if pid != std::process::id() && !Path::new(&format!("/proc/{pid}")).exists() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// The credential file Comet's `--from-heroic` reads: `<root>/heroic/gog_store/auth.json`
/// in a fresh 0700 dir, removed (with the whole dir) on drop.
struct CredentialFile {
    root: PathBuf,
}

impl std::fmt::Debug for CredentialFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CredentialFile(<redacted>)")
    }
}

/// One entry of Comet's v0.4.1 Heroic import: an object keyed by the Galaxy
/// client id (design 23 §1.1).
#[derive(serde::Serialize)]
struct HeroicEntry<'a> {
    access_token: &'a str,
    refresh_token: &'a str,
    user_id: &'a str,
}

/// The file's bytes. Zeroed on drop; reserved up front so no growth leaves
/// a copy behind.
fn auth_json(c: &Credentials) -> Zeroizing<Vec<u8>> {
    let entry = HeroicEntry {
        access_token: c.access_token.expose(),
        refresh_token: c.refresh_token.expose(),
        user_id: c.user_id.expose(),
    };
    let map = BTreeMap::from([(GALAXY_CLIENT_ID, entry)]);
    let mut buf = Zeroizing::new(Vec::with_capacity(
        256 + c.access_token.expose().len() + c.refresh_token.expose().len(),
    ));
    serde_json::to_writer(&mut *buf, &map).expect("strings serialize");
    buf
}

impl CredentialFile {
    fn write(runtime: &Path, c: &Credentials) -> std::io::Result<Self> {
        use std::io::Write as _;
        use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
        let mut tries = 0;
        let root = loop {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            let p = runtime.join(format!(
                "{AUTH_PREFIX}{}-{nanos:08x}{tries}",
                std::process::id()
            ));
            // Not recursive: an existing dir is never reused.
            match std::fs::DirBuilder::new().mode(0o700).create(&p) {
                Ok(()) => break p,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && tries < 8 => {
                    tries += 1;
                }
                Err(e) => return Err(e),
            }
        };
        let file = Self { root };
        let store = file.root.join("heroic/gog_store");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&store)?;
        let bytes = auth_json(c);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(store.join("auth.json"))?;
        f.write_all(&bytes)?;
        Ok(file)
    }

    /// Comet's `XDG_CONFIG_PATH`.
    fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for CredentialFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Something listens on `127.0.0.1:port` (or on every address), read from
/// the kernel's socket table. Never a connection: Comet's `--quit` counts
/// clients, and a probe would start its idle clock with no game yet.
pub fn listening(port: u16) -> bool {
    let lo = format!("{:08X}", u32::from_ne_bytes([127, 0, 0, 1]));
    let v4 = [lo.clone(), "00000000".to_string()];
    let v6 = [
        "00000000000000000000000000000000".to_string(),
        format!("0000000000000000FFFF0000{lo}"),
    ];
    let want = format!("{port:04X}");
    let scan = |file: &str, addrs: &[String]| {
        std::fs::read_to_string(file).is_ok_and(|t| {
            t.lines().skip(1).any(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                let (Some(local), Some(state)) = (f.get(1), f.get(3)) else {
                    return false;
                };
                let Some((addr, p)) = local.split_once(':') else {
                    return false;
                };
                *state == "0A" && p == want && addrs.iter().any(|a| a == addr)
            })
        })
    };
    scan("/proc/net/tcp", &v4) || scan("/proc/net/tcp6", &v6)
}

/// Who holds the port, when it can be said: gogo's own Comet (the
/// registry's `.comet`), else the process `ss -ltnp` names.
fn holder(runtime: &Path, port: u16) -> Option<String> {
    if let Some(pid) = owned_comet(&runtime.join(REGISTRY)) {
        return Some(format!("gogo's comet, pid {pid}"));
    }
    let out = Command::new("ss")
        .args(["-Hltnp", &format!("sport = :{port}")])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    parse_ss(&String::from_utf8_lossy(&out.stdout))
}

/// `users:(("comet",pid=1234,fd=9))` -> `comet, pid 1234`.
fn parse_ss(out: &str) -> Option<String> {
    let rest = out.split("users:((\"").nth(1)?;
    let (name, rest) = rest.split_once('"')?;
    let pid: String = rest
        .split_once("pid=")?
        .1
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let name: String = name.chars().filter(|c| !c.is_control()).take(32).collect();
    if pid.is_empty() {
        Some(name)
    } else {
        Some(format!("{name}, pid {pid}"))
    }
}

/// The pid in `<registry>/.comet` (`<pid> <start time>`), when that process
/// is still the one that started then.
fn owned_comet(registry: &Path) -> Option<u32> {
    let raw = std::fs::read_to_string(registry.join(".comet")).ok()?;
    let mut it = raw.split_whitespace();
    let pid: u32 = it.next()?.parse().ok()?;
    let start = it.next()?;
    (proc_start(pid).as_deref() == Some(start)).then_some(pid)
}

/// Field 22 of `/proc/<pid>/stat` (the start time; a reused pid differs),
/// for a process that isn't a zombie. As the launcher's `proc_start`.
fn proc_start(pid: u32) -> Option<String> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &s[s.rfind(") ")? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    (f.first() != Some(&"Z")).then(|| f.get(19).map(|s| s.to_string()))?
}

/// No core dumps of this process or (the limit is inherited) of the Comet
/// it starts, which hold the tokens; and this process not dumpable, so no
/// other process of the user can read its memory. Best effort.
pub fn harden() {
    use rustix::process::{Resource, Rlimit, setrlimit};
    let _ = setrlimit(
        Resource::Core,
        Rlimit {
            current: Some(0),
            maximum: Some(0),
        },
    );
    #[cfg(target_os = "linux")]
    let _ = rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable);
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
