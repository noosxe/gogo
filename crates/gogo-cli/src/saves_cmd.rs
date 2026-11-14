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

fn fail(e: &CloudError) -> i32 {
    eprintln!("gogo: {e}");
    match e {
        CloudError::RemoteConfig(remote_config::RemoteConfigError::Http(_))
        | CloudError::RemoteConfig(remote_config::RemoteConfigError::Status { .. }) => {
            cli::exit::NETWORK
        }
        CloudError::NotBuilt(_) | CloudError::Game(_) => cli::exit::USAGE,
        _ => cli::exit::FAILED,
    }
}

async fn locations(gogid: &str, json: bool) -> Result<i32, AppError> {
    let lane = Lane::host();
    let game = match Game::find(gogid, lane) {
        Ok(g) => g,
        Err(e) => return Ok(fail(&e)),
    };
    let client_id = match game.client_id() {
        Ok(c) => c,
        Err(e) => return Ok(fail(&e)),
    };
    let http = gogo::gog::http().map_err(|e| AppError::Io(std::io::Error::other(e.to_string())))?;
    let (cfg, source) = match remote_config::get(&http, remote_config::BASE, &client_id).await {
        Ok(v) => v,
        Err(e) => return Ok(fail(&e.into())),
    };
    let resolved = resolve(&cfg, &client_id, &game.layout);
    if json {
        let mut report = serde_json::to_value(&resolved).unwrap();
        report["gogid"] = serde_json::json!(gogid);
        report["lane"] = serde_json::json!(lane);
        report["windows_user"] = serde_json::json!(lane.windows_user());
        report["config_source"] = serde_json::json!(source);
        println!("{report}");
        return Ok(cli::exit::OK);
    }
    print!("{}", render(gogid, source, &resolved));
    Ok(cli::exit::OK)
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
}
