//! clap parsing, tracing setup, exit-code mapping — no business logic (`docs/design/01` §1).

use clap::Parser;

fn main() -> std::process::ExitCode {
    let cli = gogo::cli::Cli::parse();
    init_tracing(&cli);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    match rt.block_on(gogo::run(cli)) {
        Ok(code) => std::process::ExitCode::from(code as u8),
        Err(err) => {
            eprintln!("gogo: {err:#}");
            std::process::ExitCode::from(gogo::cli::exit::FAILED as u8)
        }
    }
}

/// RUST_LOG wins if set; otherwise derive from --quiet/-v (`docs/design/07` §5).
/// Log output goes to stderr; stdout stays results-only (`docs/design/03` §4).
fn init_tracing(cli: &gogo::cli::Cli) {
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
