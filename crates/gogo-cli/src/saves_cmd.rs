//! `gogo saves …` (design 22 §5): GOG cloud saves of a built game. This
//! file holds the read-only `locations` subcommand; the status and sync
//! commands come with the storage client.

use gogo::cloud::locations::{CloudError, Game, Lane, Resolved, resolve};
use gogo::cloud::remote_config::{self, Source};

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
}

pub async fn run(cmd: SavesCmd) -> Result<i32, AppError> {
    match cmd {
        SavesCmd::Locations { gogid, json } => locations(&gogid, json).await,
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
    fn failed(e: &CloudError) -> Outcome {
        let code = match e {
            CloudError::RemoteConfig(
                remote_config::RemoteConfigError::Http(_)
                | remote_config::RemoteConfigError::Status { .. },
            ) => cli::exit::NETWORK,
            CloudError::NotBuilt(_) | CloudError::Game(_) => cli::exit::USAGE,
            _ => cli::exit::FAILED,
        };
        Outcome {
            code,
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
            json,
        },
        &cache,
        remote_config::BASE,
        &http,
    )
    .await;
    Ok(emit(out))
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
            Ok((cfg, source)) => (resolve(&cfg, req.client_id, req.layout), source, false),
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
}
