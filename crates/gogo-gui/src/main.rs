//! gogo-gui: the desktop GUI binary (docs/design/24-gui-architecture.md §2, design 25).
//!
//! It draws a skin over the app model and puts it in a window, picking the renderer. For now
//! the model runs on the fake backend with fictional data (`--fixture`); real data comes
//! with PR F1 (#535), through `gogo-core`.
//!
//! - `gogo-gui --fixture [--dev <skin dir>]` opens the window. With `--dev` the folder is
//!   watched and reloaded; a broken edit keeps the last good skin and shows why.
//! - `gogo-gui --render-png <out>` draws a route without a window, on vello_cpu.
//! - `gogo-gui --check-skin <dir>` prints what the loader finds.
//!
//! `GOGO_GUI_RENDERER=auto|gpu|cpu` picks the window's renderer (design 24 §4). Exit
//! codes: 0, 1 on failure (skin errors included), 2 on usage.
//!
//! Agents never run the window on the host display: `scripts/gui-headless.sh`.
#![deny(unsafe_code)]

mod act;
mod app;
mod check;
mod data;
mod page;
mod panel;
mod render;

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use gogo_ui_shell::{RendererChoice, Shell};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Layer as _, filter::FilterExt as _, filter::filter_fn};

const USAGE: u8 = 2;

/// The size `--render-png` draws, in logical px: the size of the golden images.
const PNG_SIZE: (f32, f32) = (960.0, 600.0);

#[derive(Debug, Parser)]
#[command(name = "gogo-gui", version, about = "The gogo desktop GUI")]
struct Cli {
    /// Run on fictional data. Needed for the window: real data is not wired in yet.
    #[arg(long)]
    fixture: bool,

    /// Draw the skin in this folder and reload it when it changes.
    #[arg(long, value_name = "DIR", conflicts_with_all = ["render_png", "check_skin"])]
    dev: Option<PathBuf>,

    /// Draw one route of fictional data into a PNG, without a window.
    #[arg(long, value_name = "OUT", conflicts_with = "check_skin")]
    render_png: Option<PathBuf>,

    /// The route `--render-png` draws.
    #[arg(long, requires = "render_png", default_value = "shell")]
    route: String,

    /// Device pixels per logical pixel for `--render-png`.
    #[arg(long, requires = "render_png", default_value_t = 1,
          value_parser = clap::value_parser!(u8).range(1..=4))]
    scale: u8,

    /// The skin `--render-png` draws instead of the built-in one.
    #[arg(long, value_name = "DIR", requires = "render_png")]
    skin: Option<PathBuf>,

    /// Load the skin in this folder and print what is wrong with it.
    #[arg(long, value_name = "DIR")]
    check_skin: Option<PathBuf>,
}

/// The log subscriber. Whatever `filter` asks for, the records
/// [`gogo::redact::log_record_allowed`] refuses are never written.
fn subscriber<W>(filter: EnvFilter, writer: W, ansi: bool) -> impl tracing::Subscriber + Send + Sync
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    let allowed = filter_fn(|meta| gogo::redact::log_record_allowed(meta.target(), meta.level()));
    tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_ansi(ansi)
            .with_filter(filter.and(allowed)),
    )
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    subscriber(
        EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        std::io::stderr,
        std::io::stderr().is_terminal(),
    )
    .init();

    if let Some(dir) = &cli.check_skin {
        return if check::run(dir) {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let done = match &cli.render_png {
        Some(out) => render_png(out, &cli),
        None if !cli.fixture => {
            eprintln!("gogo-gui has no real data yet: run it with --fixture");
            return ExitCode::from(USAGE);
        }
        None => match renderer_choice() {
            Ok(choice) => window(cli.dev.as_deref(), choice),
            Err(e) => {
                eprintln!("GOGO_GUI_RENDERER: {e}");
                return ExitCode::from(USAGE);
            }
        },
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gogo-gui: {e}");
            ExitCode::FAILURE
        }
    }
}

fn renderer_choice() -> Result<RendererChoice, gogo_ui_shell::UnknownRenderer> {
    match std::env::var("GOGO_GUI_RENDERER") {
        Ok(name) if !name.is_empty() => name.parse(),
        _ => Ok(RendererChoice::Auto),
    }
}

type Failure = Box<dyn std::error::Error>;

fn render_png(out: &Path, cli: &Cli) -> Result<(), Failure> {
    let mut data = data::Data::fixture(|| {})?;
    let mut page = page::Page::new()?;
    if let Some(dir) = &cli.skin {
        page.load(dir).map_err(|lines| lines.join("\n"))?;
    }
    let list = page.draw(&mut data, &cli.route, PNG_SIZE)?;
    let image = gogo_render_cpu::CpuRenderer::new().render_to_rgba(
        &list,
        &data.images(),
        f64::from(cli.scale),
    )?;

    let file = std::io::BufWriter::new(std::fs::File::create(out)?);
    let mut encoder = png::Encoder::new(file, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&image.rgba)?;
    writer.finish()?;
    Ok(())
}

fn window(dev: Option<&Path>, choice: RendererChoice) -> Result<(), Failure> {
    let shell = Shell::new()?;
    let waker = shell.waker();
    let app = app::App::new(dev, data::Data::fixture(move || waker.wake())?)?;
    shell.run(app, choice)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("buffer").extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Buf {
        type Writer = Buf;
        fn make_writer(&'a self) -> Buf {
            self.clone()
        }
    }

    #[test]
    fn no_filter_turns_on_gog_warps_sign_in_line() {
        const CODE: &str = "FAKE-Code_0123456789abcdefXYZ";
        // The bridge `init` installs; here it feeds the scoped subscriber.
        let _ = tracing_log::LogTracer::init();
        log::set_max_level(log::LevelFilter::Trace);
        let buf = Buf::default();
        let sub = subscriber(EnvFilter::new("trace,gog_warp=trace"), buf.clone(), false);
        tracing::subscriber::with_default(sub, || {
            log::debug!(target: "gog_warp::core", "Requesting token with code {}", CODE);
            log::warn!(target: "gog_warp::core", "a warning stays");
        });
        let out = String::from_utf8(buf.0.lock().expect("buffer").clone()).expect("utf-8");
        assert!(out.contains("a warning stays"), "{out}");
        assert!(!out.contains(CODE), "{out}");
    }
}
