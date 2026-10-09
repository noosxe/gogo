//! clap parsing, tracing setup, exit-code mapping — no business logic (`docs/design/01` §1).
//! This crate is a thin shell over `gogo-core`; everything domain lives there.

#[allow(dead_code)] // scaffold: command handlers wire in during implementation (docs/design/03)
mod cli;
#[allow(dead_code)] // scaffold: tables land with `list`/`info` implementation
mod output;

use clap::Parser;

use crate::cli::Cli;

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    init_tracing(&cli);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    match rt.block_on(run(cli)) {
        Ok(code) => std::process::ExitCode::from(code as u8),
        Err(err) => {
            eprintln!("gogo: {err:#}");
            std::process::ExitCode::from(cli::exit::FAILED as u8)
        }
    }
}

/// RUST_LOG wins if set; otherwise derive from --quiet/-v (`docs/design/07` §5).
/// Log output goes to stderr; stdout stays results-only (`docs/design/03` §4).
fn init_tracing(cli: &Cli) {
    use tracing_subscriber::EnvFilter;

    let default = if cli.quiet {
        "warn".to_string()
    } else {
        match cli.verbose {
            0 => "info",
            1 => "debug",
            _ => "trace",
        }
        .to_string()
    };

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Dispatch one command; returns its exit code (`docs/design/03-cli.md` §5).
async fn run(cli: Cli) -> anyhow::Result<i32> {
    let _ = cli;
    todo!("command dispatch — docs/design/03 §1; wire AppContext per docs/design/01 §3")
}
