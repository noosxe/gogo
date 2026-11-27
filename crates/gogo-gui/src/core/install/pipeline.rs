//! The core pipelines behind installs, as a trait, so that tests drive the app without
//! running umu or Wine.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gogo::auth::TokenProvider;
use gogo::config::Config;
use gogo::engine::FreeSpace;
use gogo::engine::events::ProgressEvent;
use gogo::gog::depot::{self, DepotError, DepotEvent};
use gogo::linuxapp::build::{self, LinuxAppOptions};
use gogo::linuxapp::preview::{
    self, Answers, BuildPreview, BuildRequest, Env, Preview, SetupPreview,
};
use gogo::linuxapp::setup::{self, SetupOptions};
use gogo::linuxapp::snapshots::{AutoOptions, Backends, SnapshotOptions};
use gogo::linuxapp::source::{self, Files, Source};
use gogo::macapp::launch::Library;
use gogo::macapp::prompt::PromptSpec;
use gogo::macapp::{MacAppError, RendererChoice};
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// What of a run failed.
#[derive(Debug)]
pub(crate) enum Failed {
    Pipeline(MacAppError),
    /// The game files could not be had.
    Files(DepotError),
    /// The game is set up; installing Comet afterwards was not.
    Comet(MacAppError),
}

/// A build the user agreed to.
#[derive(Debug, Clone)]
pub(crate) struct Build {
    pub product: u64,
    pub source: Source,
    /// `--runtime`.
    pub runtime: Option<String>,
    pub renderer: RendererChoice,
    /// The `gogo` CLI the game's launcher will run.
    pub gogo: PathBuf,
}

/// How installs reach gogo-core. Every method blocks: call it off the runtime's
/// workers.
pub(crate) trait Pipelines: Send + Sync {
    /// Whether this system has these pipelines.
    fn supported(&self) -> bool;

    /// What setting up `gogid` would do. Writes nothing.
    fn preview_setup(
        &self,
        gogid: &str,
        answers: &Answers,
    ) -> Result<Preview<SetupPreview>, MacAppError>;

    /// Store the answers and set `gogid` up: only after the user confirmed.
    fn setup(
        &self,
        gogid: &str,
        answers: &Answers,
        emit: &mut dyn FnMut(&ProgressEvent),
    ) -> Result<(), Failed>;

    /// The `gogo` CLI a built game's launcher will run: never this app.
    fn gogo_cli(&self) -> Option<PathBuf>;

    /// Where a build's game files come from. Writes nothing.
    fn source(
        &self,
        session: Arc<dyn TokenProvider>,
        request: &source::Request,
    ) -> Result<Source, DepotError>;

    /// What a build would do. Writes nothing.
    fn preview_build(
        &self,
        request: &BuildRequest<'_>,
        answers: &Answers,
    ) -> Result<Preview<BuildPreview>, MacAppError>;

    /// Download a build's game files into the depot cache, until `cancel`.
    fn fetch(
        &self,
        session: Arc<dyn TokenProvider>,
        build: &Build,
        limit: Option<u64>,
        cancel: &CancellationToken,
        progress: &mut dyn FnMut(&DepotEvent),
    ) -> Result<(), DepotError>;

    /// Store the answers and build the game: only after the user confirmed, and
    /// with its files there.
    fn build(
        &self,
        session: Arc<dyn TokenProvider>,
        build: &Build,
        answers: &Answers,
        emit: &mut dyn FnMut(&ProgressEvent),
    ) -> Result<(), Failed>;

    /// Where free space is measured.
    fn data_root(&self) -> PathBuf;
}

/// This machine's pipelines.
pub(crate) struct Core {
    /// `[snapshots] auto` and `keep_auto`.
    pub snapshots: bool,
    pub keep_auto: usize,
    /// `library.dir`.
    pub library: Option<PathBuf>,
    pub download_dir: PathBuf,
    pub free_space: Arc<dyn FreeSpace>,
}

/// `gogo` beside this program, else the first on `PATH`.
pub(super) fn find_gogo(current_exe: Option<PathBuf>, path: Option<OsString>) -> Option<PathBuf> {
    let beside = current_exe.and_then(|exe| Some(exe.parent()?.join("gogo")));
    let on_path = path
        .iter()
        .flat_map(std::env::split_paths)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("gogo"));
    beside.into_iter().chain(on_path).find(|p| is_executable(p))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

fn signed_out() -> DepotError {
    DepotError::Library("the sign-in can't reach GOG's builds".to_string())
}

impl Pipelines for Core {
    fn supported(&self) -> bool {
        cfg!(target_os = "linux")
    }

    fn preview_setup(
        &self,
        gogid: &str,
        answers: &Answers,
    ) -> Result<Preview<SetupPreview>, MacAppError> {
        preview::setup(gogid, answers, &Env::real()?)
    }

    fn setup(
        &self,
        gogid: &str,
        answers: &Answers,
        emit: &mut dyn FnMut(&ProgressEvent),
    ) -> Result<(), Failed> {
        answers.store(&Config::path()).map_err(Failed::Pipeline)?;
        // The only question left is setup's own about Galaxy.
        let known = answers.clone();
        let replay = move |spec: &PromptSpec| known.replay(spec);
        let outcome = setup::setup(
            gogid,
            &SetupOptions {
                refresh: false,
                prompt: Some(&replay),
                runtime_snapshot: Some(AutoOptions {
                    skip: !self.snapshots,
                    keep_auto: self.keep_auto,
                    ..AutoOptions::default()
                }),
            },
            emit,
        )
        .map_err(Failed::Pipeline)?;
        if outcome.galaxy_asked_on && answers.install_comet == Some(true) {
            let env = Env::real().map_err(Failed::Comet)?;
            preview::install_comet(&env, emit).map_err(Failed::Comet)?;
        }
        Ok(())
    }

    fn gogo_cli(&self) -> Option<PathBuf> {
        find_gogo(std::env::current_exe().ok(), std::env::var_os("PATH"))
    }

    fn source(
        &self,
        session: Arc<dyn TokenProvider>,
        request: &source::Request,
    ) -> Result<Source, DepotError> {
        let core = session.gog_warp_core().ok_or_else(signed_out)?;
        Handle::current().block_on(source::resolve(core, request))
    }

    fn preview_build(
        &self,
        request: &BuildRequest<'_>,
        answers: &Answers,
    ) -> Result<Preview<BuildPreview>, MacAppError> {
        preview::build(request, answers, &Env::real()?)
    }

    fn fetch(
        &self,
        session: Arc<dyn TokenProvider>,
        build: &Build,
        limit: Option<u64>,
        cancel: &CancellationToken,
        progress: &mut dyn FnMut(&DepotEvent),
    ) -> Result<(), DepotError> {
        let core = session.gog_warp_core().ok_or_else(signed_out)?;
        let mut options = build.source.options.clone();
        options.limit = limit;
        let (events, mut said) = mpsc::unbounded_channel();
        Handle::current().block_on(async {
            let title = build.product.to_string();
            let run = depot::run(
                core,
                build.product,
                &title,
                &options,
                &events,
                cancel,
                self.free_space.as_ref(),
            );
            tokio::pin!(run);
            // Never dropped early: a cancel goes through the token.
            loop {
                tokio::select! {
                    ended = &mut run => return ended.map(drop),
                    Some(event) = said.recv() => progress(&event),
                }
            }
        })
    }

    fn build(
        &self,
        session: Arc<dyn TokenProvider>,
        build: &Build,
        answers: &Answers,
        emit: &mut dyn FnMut(&ProgressEvent),
    ) -> Result<(), Failed> {
        answers.store(&Config::path()).map_err(Failed::Pipeline)?;
        let (installer, game_tree, depot_content) = match &build.source.files {
            Files::Installer(exe) => (Some(exe.clone()), None, None),
            _ => {
                let core = session
                    .gog_warp_core()
                    .ok_or_else(|| Failed::Files(signed_out()))?;
                let tree =
                    gogo::gog::lane::find_game_tree(&build.source.root).ok_or_else(|| {
                        Failed::Files(DepotError::Library(
                            "the depot cache holds no game".to_string(),
                        ))
                    })?;
                let content = Handle::current()
                    .block_on(source::content(core, &build.source, build.product, &tree))
                    .map_err(Failed::Files)?;
                (None, Some(tree), Some(content))
            }
        };
        let library = match &self.library {
            Some(dir) => {
                Library::resolve(dir, Some(&self.download_dir)).map_err(Failed::Pipeline)?
            }
            None => None,
        };
        let snapshots = if self.snapshots {
            SnapshotOptions::on(Backends::real())
        } else {
            SnapshotOptions::off("[snapshots] auto = false")
        };
        let options = LinuxAppOptions {
            installer,
            game_tree,
            runtime: build.runtime.clone(),
            renderer: build.renderer,
            gogo_path: Some(build.gogo.clone()),
            library,
            depot_content,
            snapshots: SnapshotOptions {
                keep_auto: self.keep_auto,
                ..snapshots
            },
            ..LinuxAppOptions::default()
        };
        let known = answers.clone();
        let replay = move |spec: &PromptSpec| known.replay(spec);
        let plan = build::plan(&options, Some(&replay), emit).map_err(Failed::Pipeline)?;
        let built = build::execute(&plan, Some(&replay), emit).map_err(Failed::Pipeline)?;
        if built.setup.galaxy_asked_on && answers.install_comet == Some(true) {
            let env = Env::real().map_err(Failed::Comet)?;
            preview::install_comet(&env, emit).map_err(Failed::Comet)?;
        }
        Ok(())
    }

    fn data_root(&self) -> PathBuf {
        gogo::state::data_root()
    }
}

/// For the tests of everything else: no pipeline to run.
#[cfg(test)]
pub(crate) struct NoPipelines;

#[cfg(test)]
impl Pipelines for NoPipelines {
    fn supported(&self) -> bool {
        false
    }

    fn preview_setup(&self, _: &str, _: &Answers) -> Result<Preview<SetupPreview>, MacAppError> {
        unreachable!("not supported")
    }

    fn setup(&self, _: &str, _: &Answers, _: &mut dyn FnMut(&ProgressEvent)) -> Result<(), Failed> {
        unreachable!("not supported")
    }

    fn gogo_cli(&self) -> Option<PathBuf> {
        unreachable!("not supported")
    }

    fn source(&self, _: Arc<dyn TokenProvider>, _: &source::Request) -> Result<Source, DepotError> {
        unreachable!("not supported")
    }

    fn preview_build(
        &self,
        _: &BuildRequest<'_>,
        _: &Answers,
    ) -> Result<Preview<BuildPreview>, MacAppError> {
        unreachable!("not supported")
    }

    fn fetch(
        &self,
        _: Arc<dyn TokenProvider>,
        _: &Build,
        _: Option<u64>,
        _: &CancellationToken,
        _: &mut dyn FnMut(&DepotEvent),
    ) -> Result<(), DepotError> {
        unreachable!("not supported")
    }

    fn build(
        &self,
        _: Arc<dyn TokenProvider>,
        _: &Build,
        _: &Answers,
        _: &mut dyn FnMut(&ProgressEvent),
    ) -> Result<(), Failed> {
        unreachable!("not supported")
    }

    fn data_root(&self) -> PathBuf {
        unreachable!("not supported")
    }
}
