//! `gogo saves …` (design 22 §5): GOG cloud saves of a built game. The
//! read-only `locations` and `status` subcommands; pull, push and sync come
//! later.

use gogo::cloud::build_files::{self, Lookup};
use gogo::cloud::classify::{Action, ConflictReason};
use gogo::cloud::error::CloudError;
use gogo::cloud::locations::{Game, Lane, Resolved, resolve};
use gogo::cloud::remote_config::{self, Source};
use gogo::cloud::status::{self as cloud_status, Located, Plan, UnmappedReason};

use crate::{AppError, cli};

#[derive(clap::Subcommand, Debug)]
pub enum SavesCmd {
    /// Where a built game's cloud saves live on this machine: GOG's save
    /// locations for the game (from its remote config, cached for a day)
    /// resolved onto the game's Wine prefix and files, with whether each
    /// dir exists yet. Reads only; nothing is written to the game, its
    /// prefix or the cloud, and no account is needed.
    Locations {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// What a sync of a built game's cloud saves would do now, file by
    /// file: downloads, uploads, deletions and conflicts, from GOG's cloud
    /// listing, the local save dirs and the record of the last sync. Reads
    /// only: nothing is written to the cloud, the game, its prefix or the
    /// sync record. Exits 8 when files need a decision, 5 when a cloud
    /// object fails its checksum.
    Status {
        /// GOG product id.
        #[arg(value_name = "GOGID")]
        gogid: String,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
}

pub async fn run(cmd: SavesCmd) -> Result<i32, AppError> {
    match cmd {
        SavesCmd::Locations { gogid, json } => locations(&gogid, json).await,
        SavesCmd::Status { gogid, json } => status(&gogid, json).await,
    }
}

/// What a command prints and exits with; kept apart from the printing so
/// tests see all three.
#[derive(Debug, PartialEq)]
pub struct Outcome {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Outcome {
    /// The one error → exit-code mapping of every `saves` command is
    /// [`CloudError::exit_code`].
    fn failed(e: &CloudError) -> Outcome {
        Outcome {
            code: e.exit_code(),
            stdout: String::new(),
            stderr: format!("gogo: {e}\n"),
        }
    }
}

async fn locations(gogid: &str, json: bool) -> Result<i32, AppError> {
    let lane = Lane::host();
    let game = match Game::find(gogid, lane) {
        Ok(g) => g,
        Err(e) => return Ok(emit(Outcome::failed(&e))),
    };
    let client_id = match game.client_id() {
        Ok(c) => c,
        Err(e) => return Ok(emit(Outcome::failed(&e))),
    };
    let http = gogo::gog::http().map_err(|e| AppError::Io(std::io::Error::other(e.to_string())))?;
    let cache = gogo::state::cache::cache_dir();
    let out = report(
        &Request {
            gogid,
            client_id: &client_id,
            layout: &game.layout,
            game: Some(&game),
            json,
        },
        &cache,
        remote_config::BASE,
        &http,
    )
    .await;
    Ok(emit(out))
}

async fn status(gogid: &str, json: bool) -> Result<i32, AppError> {
    let game = match Game::find(gogid, Lane::host()) {
        Ok(g) => g,
        Err(e) => return Ok(emit(Outcome::failed(&e))),
    };
    let http = gogo::gog::http().map_err(|e| AppError::Io(std::io::Error::other(e.to_string())))?;
    let cache = gogo::state::cache::cache_dir();
    let located = cloud_status::locate(&game, &http, remote_config::BASE, &cache).await;
    let (client_id, resolved, source) = match located {
        Err(e) => return Ok(emit(Outcome::failed(&e))),
        Ok(Located::Off(plan)) => return Ok(emit(status_outcome(Ok(plan), json))),
        Ok(Located::On {
            client_id,
            resolved,
            source,
        }) => (client_id, resolved, source),
    };
    let mut resolved = resolved;
    // Locations in the game's files need the build's file list (design 22
    // §3.4): offline when gogo has it, else the build's depot manifests.
    if resolved.locations.iter().any(|l| l.in_game_files()) {
        let list = match build_files::lookup(&game) {
            Lookup::Known(l) => Ok(l),
            Lookup::Unknown(why) => Err(why),
            Lookup::Fetch(want) => crate::snapshot_cmd::fetch_build_files(want)
                .await
                .filter(|l| !l.is_empty())
                .ok_or_else(|| build_files::NOT_FETCHED.to_string()),
        };
        build_files::apply(&mut resolved, &game, list);
    }
    let cfg = crate::load_config_from_env()?;
    let store = gogo::auth::store::open(cfg.auth.token_store);
    let session = match gogo::auth::Session::restore(store.clone()).await {
        Ok(s) => s,
        Err(e) => {
            return Ok(emit(Outcome::failed(&gogo::cloud::auth::auth_error(e))));
        }
    };
    let auth = gogo::cloud::auth::CloudAuth::new(&session, &*store);
    let storage = match gogo::cloud::http::StorageClient::new() {
        Ok(s) => s,
        Err(e) => return Ok(emit(Outcome::failed(&e))),
    };
    let plan = cloud_status::plan(
        &game,
        &client_id,
        &resolved,
        source,
        &auth,
        &storage,
        &gogo::cloud::record::default_dir(),
    )
    .await;
    Ok(emit(status_outcome(plan, json)))
}

/// A status run's output and exit code: 0 a plan with nothing to decide, 8
/// conflicts (D6: a dry run exits as the run would), 1 a local file that
/// couldn't be read, 5 a corrupt object (first to win).
pub fn status_outcome(plan: Result<Plan, CloudError>, json: bool) -> Outcome {
    let plan = match plan {
        Ok(p) => p,
        Err(e) => return Outcome::failed(&e),
    };
    let code = if plan.corrupt() > 0 {
        cli::exit::CORRUPT
    } else if plan.unreadable() > 0 {
        cli::exit::FAILED
    } else if plan.conflicts() > 0 {
        cli::exit::CONFLICTS
    } else {
        cli::exit::OK
    };
    let stdout = if json {
        format!(
            "{}\n",
            serde_json::to_string(&plan).expect("a plan serializes")
        )
    } else {
        render_status(&plan)
    };
    Outcome {
        code,
        stdout,
        stderr: String::new(),
    }
}

fn conflict_text(r: ConflictReason) -> &'static str {
    match r {
        ConflictReason::BothChanged => "changed here and in the cloud",
        ConflictReason::LocalChangedCloudDeleted => "changed here, deleted in the cloud",
        ConflictReason::LocalDeletedCloudChanged => "deleted here, changed in the cloud",
        ConflictReason::BothNewDiffer => "on both sides with different content",
        ConflictReason::RolledBack => "a rollback is pending",
        ConflictReason::CaseClash => "cloud names differing only by case",
        ConflictReason::LocationMissing => "the save dir is gone; not taken for a deletion",
        ConflictReason::LocationEmptied => "the save dir is empty; not taken for a deletion",
    }
}

/// How each action reads in the text output, in the order shown.
const ACTIONS: &[(&str, &str)] = &[
    ("download", "download"),
    ("upload", "upload"),
    ("delete_local", "delete here"),
    ("delete_cloud", "delete in cloud"),
    ("conflict", "conflict"),
    ("corrupt", "corrupt"),
    ("unreadable", "unreadable"),
    ("record", "same"),
    ("forget", "forget"),
    ("nothing", "unchanged"),
];

fn label(a: Action) -> &'static str {
    let name = cloud_status::action_name(a);
    ACTIONS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, l)| *l)
        .unwrap_or(name)
}

/// `s` with control characters escaped: names and times come from the
/// cloud or the disk and must not move the terminal's cursor.
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() {
                c.escape_default().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// The plain-text plan.
pub fn render_status(p: &Plan) -> String {
    let mut out = String::new();
    if !p.enabled {
        let why = if p.no_remote_config {
            "GOG has no remote config for this game"
        } else {
            "GOG has them off for its Windows build"
        };
        out.push_str(&format!("{}: no cloud saves ({why})\n", p.gogid));
        return out;
    }
    if p.config_source == Source::StaleCache {
        out.push_str("note: GOG's remote config couldn't be fetched; using an older cached copy\n");
    }
    if p.client_id_changed {
        out.push_str(
            "note: the sync record is for another edition's client; the next sync sets it \
             aside and starts over\n",
        );
    }
    if p.first_sync {
        out.push_str(
            "first sync: no record of an earlier one, so files on both sides are compared by content\n",
        );
    } else if let Some(t) = &p.last_sync {
        out.push_str(&format!("last sync: {}\n", clean(t)));
    }
    if p.rolled_back {
        out.push_str(
            "note: a snapshot rollback is pending: every difference needs a decision \
             (--prefer local|cloud)\n",
        );
    }
    if p.pending_push {
        out.push_str("note: a push from an earlier run is still pending\n");
    }
    for l in &p.locations {
        let tag = if l.unverified {
            " (unverified default location)"
        } else {
            ""
        };
        out.push_str(&format!(
            "{}  {}{tag}\n",
            clean(&l.name),
            clean(&l.path.display().to_string())
        ));
        if let Some(b) = &l.build_files {
            out.push_str(&format!(
                "  left out as the game's own files: {} (list: {})\n",
                l.local_build_files,
                clean(&b.source)
            ));
        }
        if l.missing {
            out.push_str(
                "  warning: this save dir is gone but the last sync recorded files in it \
                 (a moved prefix, a drive not mounted?); none is taken for deleted\n",
            );
        } else if l.emptied {
            out.push_str(
                "  warning: this save dir is empty but the last sync recorded files in it; \
                 none is taken for deleted without a decision\n",
            );
        }
        let prefix = format!("{}/", l.name);
        for f in l.files.iter().filter(|f| f.action != Action::Nothing) {
            let rel = clean(f.name.strip_prefix(&prefix).unwrap_or(&f.name));
            let local = f
                .local
                .as_ref()
                .map(|x| format!("{}, {}", gogo::fmt::human_size(x.size), clean(&x.mtime)))
                .unwrap_or_else(|| "-".into());
            let cloud = match &f.cloud {
                Some(c) if c.tombstone => "deleted".to_string(),
                Some(c) => format!(
                    "{} stored, {}",
                    gogo::fmt::human_size(c.bytes),
                    clean(c.last_modified.as_deref().unwrap_or("?"))
                ),
                None => "-".into(),
            };
            let why = match f.action {
                Action::Conflict(r) => format!(" ({})", conflict_text(r)),
                _ => String::new(),
            };
            out.push_str(&format!(
                "  {:<15} {rel}{why}\n  {:<15}   here: {local}; cloud: {cloud}\n",
                label(f.action),
                ""
            ));
        }
        for i in &l.ignored {
            out.push_str(&format!(
                "  ignored: {} (a link or special file)\n",
                clean(i)
            ));
        }
    }
    let counts: Vec<String> = ACTIONS
        .iter()
        .filter_map(|(n, l)| {
            let c = p.count(n);
            (c > 0).then(|| format!("{c} {l}"))
        })
        .collect();
    if counts.is_empty() {
        out.push_str("nothing to sync: no saves on either side\n");
    } else {
        out.push_str(&format!("plan: {}\n", counts.join(", ")));
    }
    let unmapped = |r| p.unmapped.iter().filter(|u| u.reason == r).count();
    for (r, what) in [
        (UnmappedReason::NoLocation, "belong to no declared location"),
        (
            UnmappedReason::SkippedLocation,
            "belong to a skipped location",
        ),
        (
            UnmappedReason::BadPath,
            "have a path that can't be mapped safely",
        ),
        (
            UnmappedReason::Excluded,
            "are excluded (logs, caches, partial files)",
        ),
        (
            UnmappedReason::BuildFile,
            "have the path of one of the game's own files",
        ),
    ] {
        let n = unmapped(r);
        if n > 0 {
            out.push_str(&format!("note: {n} cloud objects {what}; never synced\n"));
        }
    }
    for s in &p.skipped {
        out.push_str(&format!(
            "warning: location {:?} skipped: {}\n",
            s.name,
            clean(&s.message)
        ));
    }
    if p.container.as_ref().is_some_and(|c| c.missing) {
        out.push_str("cloud: nothing stored for this game yet\n");
    } else if let Some(c) = &p.container {
        out.push_str(&format!(
            "cloud: {} objects ({} deletion markers){}\n",
            c.objects,
            c.tombstones,
            match (c.bytes_used, c.quota_bytes) {
                (Some(u), Some(q)) => format!(
                    ", {} of {} used",
                    gogo::fmt::human_size(u),
                    gogo::fmt::human_size(q)
                ),
                (Some(u), None) => format!(", {} used", gogo::fmt::human_size(u)),
                _ => String::new(),
            }
        ));
    }
    if p.conflicts() > 0 {
        out.push_str("conflicts need a decision: a sync will ask, or take --prefer local|cloud\n");
    }
    out
}

fn emit(o: Outcome) -> i32 {
    print!("{}", o.stdout);
    eprint!("{}", o.stderr);
    o.code
}

pub struct Request<'a> {
    pub gogid: &'a str,
    pub client_id: &'a str,
    pub layout: &'a gogo::cloud::locations::Layout,
    /// The built game, for the build-file list of locations in its files
    /// (none in tests that only resolve).
    pub game: Option<&'a Game>,
    pub json: bool,
}

/// Fetch (or read from the cache) the remote config and resolve it. A 404
/// means GOG has no config for the client: no cloud saves, exit 0. A real
/// network failure with no cache to fall back to is exit 4.
pub async fn report(
    req: &Request<'_>,
    cache: &std::path::Path,
    base: &str,
    http: &reqwest::Client,
) -> Outcome {
    let lane = req.layout.lane;
    let (resolved, source, no_config) =
        match remote_config::get_in(cache, http, base, req.client_id).await {
            Ok((cfg, source)) => {
                let mut r = resolve(&cfg, req.client_id, req.layout);
                if let Some(g) = req.game {
                    build_note(&mut r, g);
                }
                (r, source, false)
            }
            Err(remote_config::RemoteConfigError::Status { status: 404, .. }) => {
                (Resolved::off(), Source::Network, true)
            }
            Err(e) => return Outcome::failed(&e.into()),
        };
    let stdout = if req.json {
        let mut report = serde_json::to_value(&resolved).unwrap();
        report["gogid"] = serde_json::json!(req.gogid);
        report["lane"] = serde_json::json!(lane);
        report["windows_user"] = serde_json::json!(lane.windows_user());
        report["config_source"] = serde_json::json!(source);
        report["no_remote_config"] = serde_json::json!(no_config);
        format!("{report}\n")
    } else if no_config {
        format!(
            "{}: no cloud saves (GOG has no remote config for this game)\n",
            req.gogid
        )
    } else {
        render(req.gogid, source, &resolved)
    };
    Outcome {
        code: cli::exit::OK,
        stdout,
        stderr: String::new(),
    }
}

/// The build-file list of the locations in the game's files, offline
/// (design 22 §3.4): where it comes from, or the location skipped. A list
/// only the network has is named, and left to `saves status`.
fn build_note(r: &mut Resolved, game: &Game) {
    if !r.locations.iter().any(|l| l.in_game_files()) {
        return;
    }
    match build_files::lookup(game) {
        Lookup::Known(l) => build_files::apply(r, game, Ok(l)),
        Lookup::Unknown(why) => build_files::apply(r, game, Err(why)),
        Lookup::Fetch(_) => {
            for l in r.locations.iter_mut().filter(|l| l.in_game_files()) {
                l.build_files = Some(gogo::cloud::locations::BuildNote {
                    source: "the build's depot manifests, fetched by `saves status`".into(),
                    files: None,
                });
            }
        }
    }
}

/// The plain-text report.
pub fn render(gogid: &str, source: Source, r: &Resolved) -> String {
    let mut out = String::new();
    if !r.enabled {
        out.push_str(&format!(
            "{gogid}: no cloud saves (GOG has them off for its Windows build)\n"
        ));
        return out;
    }
    if source == Source::StaleCache {
        out.push_str("note: GOG's remote config couldn't be fetched; using an older cached copy\n");
    }
    if r.locations.is_empty() {
        out.push_str(&format!("{gogid}: no usable save locations\n"));
    }
    for l in &r.locations {
        let state = if l.exists { "exists" } else { "not there yet" };
        let tag = if l.unverified {
            " (unverified default location)"
        } else {
            ""
        };
        out.push_str(&format!(
            "{}  <?{}?>  {}  [{state}]{tag}\n",
            l.name,
            l.variable.name(),
            l.path.display()
        ));
        if let Some(b) = &l.build_files {
            let n = b.files.map(|n| format!(", {n} listed")).unwrap_or_default();
            out.push_str(&format!(
                "  in the game's files: the game's own files are left out (list: {}{n})\n",
                b.source
            ));
        }
    }
    for s in &r.skipped {
        out.push_str(&format!(
            "warning: location {:?} skipped: {}\n",
            s.name, s.message
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;
    use gogo::cloud::locations::{Layout, resolve_storage};
    use gogo::cloud::remote_config::{CloudStorage, RawLocation};

    #[test]
    fn locations_parses() {
        let c = cli::Cli::try_parse_from(["gogo", "saves", "locations", "123", "--json"]).unwrap();
        let cli::Command::Saves {
            cmd: SavesCmd::Locations { gogid, json },
        } = c.command
        else {
            panic!("expected saves locations")
        };
        assert_eq!(gogid, "123");
        assert!(json);
    }

    fn resolved(enabled: bool, locs: &[(&str, &str)]) -> Resolved {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(dir.join("prefix")).unwrap();
        std::fs::create_dir_all(dir.join("game")).unwrap();
        let layout = Layout {
            lane: Lane::Linux,
            prefix: dir.join("prefix"),
            game_files: dir.join("game"),
        };
        let s = CloudStorage {
            enabled,
            locations: locs
                .iter()
                .map(|(n, l)| RawLocation {
                    name: n.to_string(),
                    location: l.to_string(),
                })
                .collect(),
        };
        resolve_storage(Some(&s), "1", &layout)
    }

    #[test]
    fn renders_locations_skips_and_the_off_case() {
        let r = resolved(true, &[("saves", "<?DOCUMENTS?>/G"), ("x", "<?NOPE?>/y")]);
        let t = render("5", Source::Network, &r);
        assert!(t.contains("saves  <?DOCUMENTS?>  "));
        assert!(t.contains("[not there yet]"));
        assert!(t.contains("warning: location \"x\" skipped: unknown variable <?NOPE?>"));
        let d = render("5", Source::StaleCache, &resolved(true, &[]));
        assert!(d.contains("older cached copy"));
        assert!(d.contains("__default") && d.contains("unverified"));
        let off = render("5", Source::Cache, &resolved(false, &[]));
        assert!(off.contains("no cloud saves"));
    }

    const CONFIG: &str = r#"{"content":{"Windows":{"cloudStorage":{"enabled":true,
        "locations":[{"name":"saves","location":"<?DOCUMENTS?>/G"}]}}}}"#;
    const CONFIG_OFF: &str =
        r#"{"content":{"Windows":{"cloudStorage":{"enabled":false,"locations":[]}}}}"#;

    struct Fixture {
        _dir: tempfile::TempDir,
        cache: std::path::PathBuf,
        layout: gogo::cloud::locations::Layout,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(base.join("prefix")).unwrap();
        std::fs::create_dir_all(base.join("game")).unwrap();
        Fixture {
            cache: base.join("cache"),
            layout: Layout {
                lane: Lane::Linux,
                prefix: base.join("prefix"),
                game_files: base.join("game"),
            },
            _dir: dir,
        }
    }

    async fn run_report(f: &Fixture, base: &str, json: bool) -> Outcome {
        report(
            &Request {
                gogid: "5",
                client_id: "77",
                layout: &f.layout,
                game: None,
                json,
            },
            &f.cache,
            base,
            &reqwest::Client::new(),
        )
        .await
    }

    async fn serving(status: u16, body: &str) -> wiremock::MockServer {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::method("GET"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn locations_json_lists_resolved_and_skipped() {
        let f = fixture();
        let server = serving(200, CONFIG).await;
        let o = run_report(&f, &server.uri(), true).await;
        assert_eq!(o.code, 0);
        assert!(o.stderr.is_empty());
        let v: serde_json::Value = serde_json::from_str(&o.stdout).unwrap();
        assert_eq!(v["enabled"], true);
        assert_eq!(v["gogid"], "5");
        assert_eq!(v["lane"], "linux");
        assert_eq!(v["windows_user"], "steamuser");
        assert_eq!(v["config_source"], "network");
        assert_eq!(v["locations"][0]["name"], "saves");
        assert_eq!(v["locations"][0]["exists"], false);
        assert!(v.get("user_id").is_none() && v.get("token").is_none());
    }

    #[tokio::test]
    async fn disabled_storage_is_exit_0_with_enabled_false() {
        let f = fixture();
        let server = serving(200, CONFIG_OFF).await;
        let o = run_report(&f, &server.uri(), true).await;
        assert_eq!(o.code, 0);
        let v: serde_json::Value = serde_json::from_str(&o.stdout).unwrap();
        assert_eq!(v["enabled"], false);
        assert_eq!(v["locations"].as_array().unwrap().len(), 0);
        let o = run_report(&f, &server.uri(), false).await;
        assert_eq!(o.code, 0);
        assert!(o.stdout.contains("no cloud saves"));
    }

    #[tokio::test]
    async fn a_404_means_no_cloud_saves_not_a_failure() {
        let f = fixture();
        let server = serving(404, "").await;
        let o = run_report(&f, &server.uri(), false).await;
        assert_eq!(o.code, 0);
        assert!(o.stdout.contains("no remote config"));
        let o = run_report(&f, &server.uri(), true).await;
        assert_eq!(o.code, 0);
        let v: serde_json::Value = serde_json::from_str(&o.stdout).unwrap();
        assert_eq!(v["enabled"], false);
        assert_eq!(v["no_remote_config"], true);
    }

    #[tokio::test]
    async fn network_failure_is_exit_4_and_server_errors_too() {
        let f = fixture();
        let o = run_report(&f, "http://127.0.0.1:1", false).await;
        assert_eq!(o.code, cli::exit::NETWORK);
        assert!(o.stdout.is_empty() && o.stderr.starts_with("gogo: "));
        let server = serving(500, "").await;
        assert_eq!(
            run_report(&f, &server.uri(), false).await.code,
            cli::exit::NETWORK
        );
    }

    #[tokio::test]
    async fn a_foreign_200_is_a_failure_not_no_cloud_saves() {
        let f = fixture();
        let server = serving(200, "{}").await;
        let o = run_report(&f, &server.uri(), false).await;
        assert_eq!(o.code, cli::exit::FAILED);
    }

    #[tokio::test]
    async fn an_old_cache_stands_in_with_a_note_when_the_network_is_down() {
        let f = fixture();
        let server = serving(200, CONFIG).await;
        assert_eq!(run_report(&f, &server.uri(), false).await.code, 0);
        let cached = f.cache.join("remote-config/77.json");
        let file = std::fs::File::options().write(true).open(&cached).unwrap();
        file.set_modified(
            std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 24 * 3600),
        )
        .unwrap();
        drop(file);
        let o = run_report(&f, "http://127.0.0.1:1", false).await;
        assert_eq!(o.code, 0);
        assert!(o.stdout.contains("older cached copy"));
        assert!(o.stdout.contains("saves  <?DOCUMENTS?>"));
        let o = run_report(&f, "http://127.0.0.1:1", true).await;
        let v: serde_json::Value = serde_json::from_str(&o.stdout).unwrap();
        assert_eq!(v["config_source"], "stale_cache");
    }

    const INSTALL_CONFIG: &str = r#"{"content":{"Windows":{"cloudStorage":{"enabled":true,
        "locations":[{"name":"game","location":"<?INSTALL?>/SAVE"}]}}}}"#;

    /// An installer-lane game in `f`'s prefix, `drive_c/GOG/G`.
    fn installer_game(f: &Fixture, log: bool) -> Game {
        let root = f.layout.prefix.parent().unwrap().to_path_buf();
        let gf = f.layout.prefix.join("drive_c/GOG/G");
        std::fs::create_dir_all(gf.join("SAVE")).unwrap();
        std::fs::write(
            root.join("content.toml"),
            "schema = 1\nlane = \"installer\"\nbuild_id = \"b1\"\n",
        )
        .unwrap();
        if log {
            std::fs::create_dir_all(root.join("install")).unwrap();
            std::fs::write(
                root.join("install/inno.log"),
                "Dest filename: C:\\GOG\\G\\Game.exe\r\n",
            )
            .unwrap();
        }
        Game {
            gogid: "5".into(),
            layout: Layout {
                lane: Lane::Linux,
                prefix: f.layout.prefix.clone(),
                game_files: gf,
            },
            root,
        }
    }

    #[tokio::test]
    async fn locations_name_the_build_file_list_or_skip_the_location() {
        let f = fixture();
        let server = serving(200, INSTALL_CONFIG).await;
        let uri = server.uri();
        let run = async |game: &Game, json: bool| {
            let req = Request {
                gogid: "5",
                client_id: "77",
                layout: &game.layout,
                game: Some(game),
                json,
            };
            report(&req, &f.cache, &uri, &reqwest::Client::new()).await
        };
        let g = installer_game(&f, true);
        let o = run(&g, false).await;
        assert!(
            o.stdout.contains(
                "in the game's files: the game's own files are left out \
                 (list: the installer's log, 1 listed)"
            ),
            "{}",
            o.stdout
        );
        let v: serde_json::Value = serde_json::from_str(&run(&g, true).await.stdout).unwrap();
        assert_eq!(
            v["locations"][0]["build_files"]["source"],
            "the installer's log"
        );

        std::fs::remove_file(g.root.join("install/inno.log")).unwrap();
        let o = run(&g, false).await;
        assert_eq!(o.code, 0);
        assert!(
            o.stdout
                .contains("warning: location \"game\" skipped: it is in the game's files"),
            "{}",
            o.stdout
        );
        let v: serde_json::Value = serde_json::from_str(&run(&g, true).await.stdout).unwrap();
        assert_eq!(v["skipped"][0]["reason"]["kind"], "build_files_unknown");
    }

    #[test]
    fn control_characters_are_escaped() {
        assert_eq!(clean("a\u{1b}[2Jb\n"), "a\\u{1b}[2Jb\\n");
        assert_eq!(clean("Slot 1/sav.dat"), "Slot 1/sav.dat");
    }

    #[test]
    fn status_parses() {
        let c = cli::Cli::try_parse_from(["gogo", "saves", "status", "123"]).unwrap();
        let cli::Command::Saves {
            cmd: SavesCmd::Status { gogid, json },
        } = c.command
        else {
            panic!("expected saves status")
        };
        assert_eq!(gogid, "123");
        assert!(!json);
    }

    /// `saves status` against a mock remote config and storage, with a fake
    /// exchange: exit codes and both outputs.
    #[tokio::test]
    async fn status_against_mock_storage() {
        use gogo::cloud::auth::CloudAuth;
        use gogo::cloud::http::StorageClient;
        use gogo::cloud::locations::Game;
        use gogo::cloud::storage::{gzip, md5_hex};
        use gogo::cloud::testing::{FixedExchange, MainSession, NoManifests, keep_manifest};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        const CLIENT: &str = "11111111111111111";
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let root = base.join("games/5");
        let saves = root.join("prefix/drive_c/users/steamuser/Documents/G");
        std::fs::create_dir_all(&saves).unwrap();
        std::fs::create_dir_all(root.join("game")).unwrap();
        keep_manifest(&root, CLIENT);
        std::fs::write(saves.join("mine.sav"), b"local").unwrap();
        std::fs::write(saves.join("both.sav"), b"local").unwrap();
        let game = Game {
            gogid: "5".into(),
            layout: Layout {
                lane: Lane::Linux,
                prefix: root.join("prefix"),
                game_files: root.join("game"),
            },
            root: root.clone(),
        };

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/components/galaxy_client/clients/{CLIENT}")))
            .respond_with(ResponseTemplate::new(200).set_body_string(CONFIG))
            .mount(&server)
            .await;
        let objects = [
            ("saves/theirs.sav", gzip(b"cloud")),
            ("saves/both.sav", gzip(b"cloud")),
        ];
        let listing: Vec<serde_json::Value> = objects
            .iter()
            .map(|(n, b)| serde_json::json!({"name": n, "bytes": b.len(), "hash": md5_hex(b)}))
            .collect();
        Mock::given(method("GET"))
            .and(path(format!("/v1/7/{CLIENT}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(listing))
            .mount(&server)
            .await;
        for (n, b) in &objects {
            Mock::given(method("GET"))
                .and(path(format!("/v1/7/{CLIENT}/{n}")))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(b.clone()))
                .mount(&server)
                .await;
        }

        let session = MainSession::new();
        let exchange = FixedExchange::default();
        let auth = CloudAuth::with_parts(&session, || Ok(()), NoManifests, &exchange);
        let storage =
            StorageClient::with_base(format!("{}/v1/", server.uri()).parse().unwrap()).unwrap();
        let run = || async {
            cloud_status::status(&cloud_status::Inputs {
                game: &game,
                auth: &auth,
                storage: &storage,
                http: &reqwest::Client::new(),
                config_base: &server.uri(),
                cache_dir: &base.join("cache"),
                record_dir: &base.join("records"),
            })
            .await
        };

        let text = status_outcome(run().await, false);
        assert_eq!(text.code, cli::exit::CONFLICTS, "{}", text.stdout);
        assert!(text.stdout.contains("first sync"), "{}", text.stdout);
        assert!(
            text.stdout.contains("download        theirs.sav"),
            "{}",
            text.stdout
        );
        assert!(
            text.stdout.contains("upload          mine.sav"),
            "{}",
            text.stdout
        );
        assert!(
            text.stdout
                .contains("conflict        both.sav (on both sides with different content)"),
            "{}",
            text.stdout
        );
        assert!(
            text.stdout
                .contains("plan: 1 download, 1 upload, 1 conflict"),
            "{}",
            text.stdout
        );

        let json = status_outcome(run().await, true);
        assert_eq!(json.code, cli::exit::CONFLICTS);
        let v: serde_json::Value = serde_json::from_str(&json.stdout).unwrap();
        assert_eq!(v["first_sync"], true);
        assert_eq!(v["counts"]["download"], 1);
        assert_eq!(v["locations"][0]["files"][0]["action"], "conflict");
        assert_eq!(v["locations"][0]["files"][0]["reason"], "both_new_differ");
        for secret in [CLIENT, "made-up-game-token", gogo::cloud::testing::SECRET] {
            assert!(!json.stdout.contains(secret), "{secret}");
        }
        assert!(!base.join("records").exists(), "status writes no record");

        // The game's own files in a location in the game's files.
        let mut plan = run().await.unwrap();
        plan.locations[0].local_build_files = 3;
        plan.locations[0].build_files = Some(gogo::cloud::locations::BuildNote {
            source: "the depot cache".into(),
            files: Some(9),
        });
        plan.unmapped.push(cloud_status::Unmapped {
            name: "saves/Game.exe".into(),
            reason: UnmappedReason::BuildFile,
        });
        let t = render_status(&plan);
        assert!(
            t.contains("  left out as the game's own files: 3 (list: the depot cache)"),
            "{t}"
        );
        assert!(
            t.contains("note: 1 cloud objects have the path of one of the game's own files"),
            "{t}"
        );

        // Exit precedence: a corrupt object (5) over an unreadable file (1)
        // over conflicts (8).
        let mut plan = run().await.unwrap();
        plan.counts.insert("unreadable".into(), 1);
        assert_eq!(
            status_outcome(Ok(plan.clone()), false).code,
            cli::exit::FAILED
        );
        plan.counts.insert("corrupt".into(), 1);
        assert_eq!(status_outcome(Ok(plan), false).code, cli::exit::CORRUPT);

        // A broken or newer record stops the run, naming the file.
        std::fs::create_dir_all(base.join("records")).unwrap();
        for body in ["schema = ", "schema = 2\nclient_id = \"1\"\n"] {
            std::fs::write(base.join("records/5.toml"), body).unwrap();
            let o = status_outcome(run().await, false);
            assert_eq!(o.code, cli::exit::FAILED, "{body}");
            assert!(o.stderr.contains("5.toml"), "{}", o.stderr);
            assert!(o.stdout.is_empty());
        }

        // An error maps through the one table.
        let e = status_outcome(Err(CloudError::NoLicense { gogid: 5 }), false);
        assert_eq!(e.code, cli::exit::AUTH);
        assert!(e.stderr.starts_with("gogo: "));
    }
}
