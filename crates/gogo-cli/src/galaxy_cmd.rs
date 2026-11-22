//! `gogo linux-app galaxy-start <gogid> --launcher` (design 23 §2.1, #488):
//! what `launch.sh` runs before a game with `GALAXY=1`. One line on stdout
//! for the run log, exit 0 whatever happens: a launch never waits on it
//! beyond [`galaxy::BUDGET`]. Tracing is off for it (`main`), errors are
//! never formatted where they could hold a token.

use gogo::linuxapp::{comet, galaxy, umu::Host};

use crate::cli;

/// The longest resolve error the run log gets, as the launcher cuts setup's.
const MAX_REASON: usize = 300;

pub async fn run(gogid: &str) -> i32 {
    galaxy::harden();
    println!("{}", outcome(gogid).await.line());
    cli::exit::OK
}

async fn outcome(gogid: &str) -> galaxy::Outcome {
    if gogid.parse::<u64>().is_err() {
        return galaxy::Outcome::Skipped(format!("{gogid:.40} is not a GOG product id"));
    }
    let Ok(loaded) = crate::load_config_quietly() else {
        return galaxy::Outcome::Skipped("gogo's config doesn't load (gogo doctor)".into());
    };
    let cfg = loaded.config;
    let comet = match comet::available(&Host::real(), &comet::mode(&cfg)) {
        Ok(Some(bin)) => Ok(bin),
        Ok(None) => Err("comet not installed (gogo runtimes install comet)".to_string()),
        Err(e) => Err(e.to_string().chars().take(MAX_REASON).collect()),
    };
    let comet_dir = gogo::state::data_root().join("comet");
    let account = galaxy::LiveAccount {
        store: gogo::auth::store::open(cfg.auth.token_store),
        comet_dir: comet_dir.clone(),
    };
    let env = |k: &str| std::env::var_os(k);
    let start = galaxy::Start {
        comet,
        runtime_dir: std::env::var_os("XDG_RUNTIME_DIR"),
        comet_dir,
        port: galaxy::PORT,
        budget: galaxy::BUDGET,
        env: &env,
    };
    galaxy::start(&start, &account).await
}
