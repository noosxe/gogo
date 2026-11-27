//! A job's step list and progress, folded from a pipeline's events (design 25 §5).
//!
//! Step names are the app's own words: event texts can hold local paths.

use std::time::Instant;

use gogo::engine::events::{ProgressEvent, step};
use gogo::gog::depot::DepotEvent;
use gogo_gui_model::{JobStatus, Step, StepState};

/// A part of an install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Files,
    Copy,
    Installer,
    Support,
    Move,
    Umu,
    Proton,
    Snapshot,
    Prefix,
    Verbs,
    Comet,
    Finish,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Files => "Download the game files",
            Phase::Copy => "Copy the game files",
            Phase::Installer => "Run GOG's installer",
            Phase::Support => "Run GOG's support setup",
            Phase::Move => "Move the game into place",
            Phase::Umu => "Download umu-launcher",
            Phase::Proton => "Download GE-Proton",
            Phase::Snapshot => "Snapshot",
            Phase::Prefix => "Prepare the Wine prefix",
            Phase::Verbs => "Install winetricks verbs",
            Phase::Comet => "Install Comet",
            Phase::Finish => "Finish setting up",
        }
    }
}

/// The phases of one run, the ones its plan expects first listed as pending.
pub(super) struct Steps {
    list: Vec<(Phase, StepState, Option<Instant>, Option<f64>)>,
    /// The runtime being downloaded, for the `downloading …` steps.
    fetching: Option<Phase>,
    bytes: u64,
    total: Option<u64>,
}

fn runtime(id: &str) -> Phase {
    match id {
        "umu" => Phase::Umu,
        "comet" => Phase::Comet,
        _ => Phase::Proton,
    }
}

impl Steps {
    pub(super) fn new(planned: &[Phase]) -> Self {
        Steps {
            list: planned
                .iter()
                .map(|&p| (p, StepState::Pending, None, None))
                .collect(),
            fetching: None,
            bytes: 0,
            total: None,
        }
    }

    fn index(&mut self, phase: Phase) -> usize {
        if let Some(i) = self.list.iter().position(|s| s.0 == phase) {
            return i;
        }
        // An unplanned phase goes before what is still to come.
        let at = self
            .list
            .iter()
            .position(|s| s.1 == StepState::Pending)
            .unwrap_or(self.list.len());
        self.list
            .insert(at, (phase, StepState::Pending, None, None));
        at
    }

    fn finish(step: &mut (Phase, StepState, Option<Instant>, Option<f64>), now: Instant) {
        if step.1 == StepState::Now {
            step.3 = step.2.map(|s| now.duration_since(s).as_secs_f64());
        }
        step.1 = StepState::Done;
    }

    /// `phase` is under way; whatever was before it is over. Returns whether that is
    /// news.
    fn enter(&mut self, phase: Phase, now: Instant) -> bool {
        let i = self.index(phase);
        if self.list[i].1 == StepState::Now {
            return false;
        }
        for s in &mut self.list {
            if s.1 == StepState::Now {
                Self::finish(s, now);
            }
        }
        self.list[i].1 = StepState::Now;
        self.list[i].2 = Some(now);
        self.bytes = 0;
        self.total = None;
        true
    }

    fn leave(&mut self, phase: Phase, now: Instant) -> bool {
        let i = self.index(phase);
        Self::finish(&mut self.list[i], now);
        self.bytes = 0;
        self.total = None;
        true
    }

    fn phase_of(&self, name: &str) -> Option<Phase> {
        if name == step::PREFIX {
            Some(Phase::Prefix)
        } else if name == step::GAME_FILES || name == step::REMOVING_GAME {
            Some(Phase::Copy)
        } else if name == step::INSTALLER || name == step::DLC_INSTALLER {
            Some(Phase::Installer)
        } else if name == step::SUPPORT {
            Some(Phase::Support)
        } else if name == step::MOVING_GAME || name == step::MOVING_PREFIX {
            Some(Phase::Move)
        } else if name.starts_with("winetricks ") {
            Some(Phase::Verbs)
        } else if ["downloading ", "checking ", "unpacking "]
            .iter()
            .any(|p| name.starts_with(p))
        {
            self.fetching
        } else {
            None
        }
    }

    /// Take in one event. Returns whether a step changed, which is worth reporting at
    /// once; progress alone can wait.
    pub(super) fn apply(&mut self, event: &ProgressEvent, now: Instant) -> bool {
        match event {
            ProgressEvent::RuntimeDownloadStarted { id, .. } => {
                let phase = runtime(id);
                self.fetching = Some(phase);
                self.enter(phase, now)
            }
            ProgressEvent::RuntimeDownloadFinished { id, .. }
            | ProgressEvent::ToolProvisioned { id, .. } => {
                self.fetching = None;
                self.leave(runtime(id), now)
            }
            ProgressEvent::SnapshotCreated { .. } => {
                self.leave(Phase::Snapshot, now);
                true
            }
            ProgressEvent::WorkStarted { step } => self
                .phase_of(step)
                .is_some_and(|phase| self.enter(phase, now)),
            ProgressEvent::WorkProgress {
                step,
                bytes,
                bytes_total,
                ..
            } => {
                let Some(phase) = self.phase_of(step) else {
                    return false;
                };
                let news = self.enter(phase, now);
                self.bytes = bytes.unwrap_or(0);
                self.total = *bytes_total;
                news
            }
            // A download's check and unpacking are steps of it, not its end.
            ProgressEvent::WorkDone { step } => match self.phase_of(step) {
                Some(
                    phase @ (Phase::Prefix | Phase::Verbs | Phase::Installer | Phase::Support),
                ) => self.leave(phase, now),
                _ => false,
            },
            ProgressEvent::PrefixStaged => self.enter(Phase::Prefix, now),
            ProgressEvent::GameFilesStarted { .. } => self.enter(Phase::Copy, now),
            ProgressEvent::InstallStarted => self.enter(Phase::Installer, now),
            _ => false,
        }
    }

    /// Take in one event of the depot download.
    pub(super) fn download(&mut self, event: &DepotEvent, now: Instant) -> bool {
        match event {
            DepotEvent::Preparing
            | DepotEvent::Allocating { .. }
            | DepotEvent::Verifying { .. } => self.enter(Phase::Files, now),
            DepotEvent::Downloading {
                downloaded,
                total_download,
                ..
            } => {
                let news = self.enter(Phase::Files, now);
                self.bytes = *downloaded;
                self.total = Some(*total_download);
                news
            }
            DepotEvent::Finished { .. } => self.leave(Phase::Files, now),
            _ => false,
        }
    }

    /// The run is over: everything done, or the step it was in has failed.
    pub(super) fn end(&mut self, ok: bool, now: Instant) {
        for s in &mut self.list {
            if ok {
                Self::finish(s, now);
            } else if s.1 == StepState::Now {
                Self::finish(s, now);
                s.1 = StepState::Failed;
            }
        }
        self.bytes = 0;
        self.total = None;
    }

    /// A failure between two named steps belongs to the finishing one.
    pub(super) fn closing_if_idle(&mut self, now: Instant) {
        if self.list.iter().all(|s| s.1 != StepState::Now) {
            self.enter(Phase::Finish, now);
        }
    }

    pub(super) fn report(&self, status: &mut JobStatus) {
        status.steps = self
            .list
            .iter()
            .map(|(phase, state, _, took)| Step {
                name: phase.name().to_string(),
                state: *state,
                took: *took,
            })
            .collect();
        status.bytes = self.bytes;
        status.total = self.total;
        status.fraction = self
            .total
            .filter(|&t| t > 0)
            .map(|t| (self.bytes as f64 / t as f64).min(1.0));
    }
}

#[cfg(test)]
mod tests {
    use gogo_gui_model::JobKind;

    use super::*;

    fn states(steps: &Steps) -> Vec<(String, StepState)> {
        let mut status = JobStatus::queued(JobKind::Setup);
        steps.report(&mut status);
        status
            .steps
            .into_iter()
            .map(|s| (s.name, s.state))
            .collect()
    }

    #[test]
    fn events_move_the_planned_steps_and_name_no_path() {
        use StepState::{Done, Now, Pending};
        let t = Instant::now();
        let mut steps = Steps::new(&[Phase::Proton, Phase::Prefix, Phase::Finish]);
        let started = |step: &str| ProgressEvent::WorkStarted { step: step.into() };
        assert!(steps.apply(
            &ProgressEvent::RuntimeDownloadStarted {
                id: "ge-proton".into(),
                version: "1".into(),
                bytes: 10,
            },
            t
        ));
        let progress = ProgressEvent::WorkProgress {
            step: "downloading /somewhere/GE-Proton.tar.gz".into(),
            files: None,
            files_total: None,
            bytes: Some(5),
            bytes_total: Some(10),
        };
        assert!(!steps.apply(&progress, t), "progress alone is no news");
        let mut status = JobStatus::queued(JobKind::Setup);
        steps.report(&mut status);
        assert_eq!(status.fraction, Some(0.5));
        assert!(!format!("{status:?}").contains("somewhere"));

        // Unpacking is part of the download.
        assert!(!steps.apply(&started("unpacking /somewhere/x"), t));
        assert!(steps.apply(&started(step::PREFIX), t));
        assert!(!steps.apply(&started("some /new/step"), t));
        assert!(steps.apply(&started("winetricks corefonts"), t));
        assert_eq!(
            states(&steps),
            [
                ("Download GE-Proton", Done),
                ("Prepare the Wine prefix", Done),
                ("Install winetricks verbs", Now),
                ("Finish setting up", Pending),
            ]
            .map(|(n, s)| (n.to_string(), s))
        );
        steps.end(false, t);
        assert_eq!(states(&steps)[2].1, StepState::Failed);
        steps.end(true, t);
        assert!(states(&steps).iter().all(|s| s.1 == Done));
    }
}
