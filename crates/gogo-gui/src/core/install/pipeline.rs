//! The core pipelines behind installs, as a trait, so that tests drive the app without
//! running umu or Wine.

use std::path::PathBuf;

use gogo::config::Config;
use gogo::engine::events::ProgressEvent;
use gogo::linuxapp::preview::{self, Answers, Env, Preview, SetupPreview};
use gogo::linuxapp::setup::{self, SetupOptions};
use gogo::linuxapp::snapshots::AutoOptions;
use gogo::macapp::MacAppError;
use gogo::macapp::prompt::PromptSpec;

/// What of a run failed.
#[derive(Debug)]
pub(crate) enum Failed {
    Pipeline(MacAppError),
    /// The game is set up; installing Comet afterwards was not.
    Comet(MacAppError),
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

    /// Where free space is measured.
    fn data_root(&self) -> PathBuf;
}

/// This machine's pipelines.
pub(crate) struct Core {
    /// `[snapshots] auto` and `keep_auto`.
    pub snapshots: bool,
    pub keep_auto: usize,
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

    fn data_root(&self) -> PathBuf {
        unreachable!("not supported")
    }
}
