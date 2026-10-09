//! gogo — GOG.com library lister and offline-installer downloader.
//!
//! The library is the product (`docs/design/01-architecture.md` §1): the CLI binary is a
//! thin frontend over these modules, and the v2 TUI will consume the same surface.

pub mod app;
pub mod auth;
pub mod cli;
pub mod config;
pub mod engine;
pub mod gog;
pub mod output;
pub mod state;

/// Convenience re-exports for consumers of the library.
pub mod prelude {
    pub use crate::app::AppContext;
    pub use crate::config::Config;
    pub use crate::engine::events::ProgressEvent;
}

/// Entry point used by the binary: dispatches one command and returns its exit code
/// (`docs/design/03-cli.md` §5).
pub async fn run(cli: crate::cli::Cli) -> anyhow::Result<i32> {
    let _ = cli;
    todo!("command dispatch — docs/design/03 §1; wire AppContext per docs/design/01 §3")
}
