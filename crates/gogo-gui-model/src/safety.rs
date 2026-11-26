//! The safety flow (design 25 §4): the only path from what a skin asked for to
//! [`Backend::run`].
//!
//! Two kinds of call come in, and they don't mix. A skin can only *invoke*: an
//! [`Invocation`] names an action and its arguments, and nothing it can say confirms
//! anything. Answers ([`Model::confirm`](crate::Model::confirm),
//! [`Model::type_name`](crate::Model::type_name),
//! [`Model::hold_start`](crate::Model::hold_start),
//! [`Model::answer`](crate::Model::answer)) are separate methods for the app's own
//! widgets. Each needs
//!
//! - the [`Ticket`] of the screen it answers, which is new for every screen;
//! - a screen the shell has reported as [`presented`](SafetyFlow::presented);
//! - a [`Press`] that began after that, is not a key repeat, and whose [`InputEvent`]
//!   was never used before;
//! - the screen to have been up for [`DWELL`] when the press began.
//!
//! So an answer can't be skipped, replayed, aimed at another plan, or be the tail of the
//! click that opened the screen, and a double click or a repeating Enter can't confirm a
//! plan that appeared between its two halves. Confirming plans again, and runs only if nothing the
//! user saw has changed. [`cancel`](SafetyFlow::cancel) has no preconditions at all.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use gogo_contract::actions::Escalation;
use gogo_contract::{ActionSpec, Decision, Gate, Gates, Kind, PlanFacts, Tier};

use crate::backend::{Backend, Run};
use crate::call::{Call, Invocation};
use crate::error::ErrorView;
use crate::jobs::{JobId, JobState, JobTable, Transition};
use crate::plan::{Plan, PlanHandle, PlanReply, PlanWait, Planned};
use crate::prompt::{PromptAnswer, PromptReply, PromptSpec};
use crate::source::Signal;

/// How long a hold must last (S6).
pub const HOLD: Duration = Duration::from_secs(1);

/// How long a screen must have been up before a press can answer it (S5).
pub const DWELL: Duration = Duration::from_millis(500);

/// How many inline previews are kept.
const MAX_PREVIEWS: usize = 16;

/// How many previews may be waiting for their plan at once.
const MAX_PLANNING: usize = 4;

/// One real user input: a click, a key press or an accessibility activation. The shell
/// numbers them from 1, each larger than the one before, and gives one to exactly one
/// call (S4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InputEvent(pub u64);

/// A press that answers one of the app's own screens: a pointer or key going down, or an
/// accessibility activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Press {
    /// The number the press got **when it went down**, not when it was released.
    pub event: InputEvent,
    /// When it went down, on the monotonic clock given to `presented` and `tick`.
    pub at: Duration,
    /// A key repeating while held. It never answers anything.
    pub repeat: bool,
}

/// Names one screen the safety flow shows. Never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ticket(u64);

/// What a call did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It was taken: something ran, or a screen opened.
    Accepted,
    /// It did not count (a used event, an old ticket, a screen not yet shown, a dialog
    /// in the way) and nothing changed.
    Ignored,
    /// The action was refused. The error is also queued for the `error` widget (S9).
    Refused(ErrorView),
}

/// What the strong confirmation asks for (S6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Strength {
    /// Type this text, the game's title. The match is exact once outer spaces are
    /// trimmed: no case folding and no Unicode normalisation, so a title that can't be
    /// typed as stored can't be confirmed. That fails closed, and cancel is there.
    Typed(String),
    /// Hold the button for [`HOLD`].
    Hold,
}

/// The screen on top, for the app's widgets.
#[derive(Debug, Clone, PartialEq)]
pub enum Modal<'a> {
    /// A question.
    Prompt {
        /// The screen's ticket.
        ticket: Ticket,
        /// What is asked.
        spec: &'a PromptSpec,
        /// Set when a running job asks.
        job: Option<&'a JobId>,
    },
    /// Waiting for a plan; `confirming` when it is the check after a confirmation.
    Planning {
        /// The screen's ticket.
        ticket: Ticket,
        /// The action being planned.
        action: &'static str,
        /// This is the check after a confirmation.
        confirming: bool,
    },
    /// `plan-card`: confirm or cancel.
    PlanCard {
        /// The screen's ticket.
        ticket: Ticket,
        /// The action planned.
        action: &'static str,
        /// The plan.
        plan: &'a Plan,
        /// The tier the plan calls for.
        tier: Tier,
    },
    /// `confirm-destructive`. Its default is to cancel.
    Strong {
        /// The screen's ticket.
        ticket: Ticket,
        /// The action to confirm.
        action: &'static str,
        /// The plan being confirmed.
        plan: &'a Plan,
        /// What it takes.
        strength: &'a Strength,
    },
    /// The `error` widget.
    Error {
        /// The ticket that dismisses it.
        ticket: Ticket,
        /// What failed.
        error: &'a ErrorView,
    },
}

impl Modal<'_> {
    /// The ticket that answers or cancels this screen.
    pub fn ticket(&self) -> Ticket {
        match self {
            Modal::Prompt { ticket, .. }
            | Modal::Planning { ticket, .. }
            | Modal::PlanCard { ticket, .. }
            | Modal::Strong { ticket, .. }
            | Modal::Error { ticket, .. } => *ticket,
        }
    }
}

/// An inline plan preview (`plan.preview`, or a `plan-card` a skin placed). Read-only
/// until the user confirms it.
#[derive(Debug, Clone, PartialEq)]
pub enum Preview<'a> {
    /// Still being planned.
    Pending,
    /// A plan to show.
    Ready {
        /// The plan.
        plan: &'a Plan,
        /// The tier it calls for.
        tier: Tier,
    },
    /// The action has nothing to do.
    Empty,
    /// Planning was refused.
    Failed(&'a ErrorView),
}

/// Something the shell has to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Open the command palette.
    OpenPalette {
        /// What it is limited to.
        scope: Option<String>,
    },
    /// Open the account menu.
    AccountMenu,
    /// Put this text on the clipboard.
    Copy(String),
    /// Write a skin's `ui` variable.
    SetUi {
        /// The variable.
        var: String,
        /// Its new value.
        value: gogo_contract::Value,
    },
    /// Jobs start, finish and fail with a toast, and nothing else does.
    Toast {
        /// The job.
        job: JobId,
        /// What happened to it.
        toast: Toast,
    },
    /// The action's plan turned out to be empty.
    NothingToDo {
        /// The action.
        action: &'static str,
    },
    /// The confirmed plan was no longer the plan; the new one is on screen.
    PlanChanged {
        /// The action.
        action: &'static str,
    },
}

/// What a toast says happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toast {
    /// A job began.
    Started,
    /// A job finished.
    Finished,
    /// A job failed.
    Failed,
}

/// How an action travels through the flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// The model or the shell does it; the engine is not involved.
    Shell,
    /// Steers a job in the table.
    JobControl,
    /// Asks first, then plans.
    Prompt,
    /// Planned before anything runs.
    Plan,
    /// Runs at once.
    Direct,
}

const SHELL: &[&str] = &[
    "nav.go",
    "nav.back",
    "palette.open",
    "account.menu",
    "ui.set",
    "clipboard.copy",
];

const JOB_CONTROL: &[&str] = &["job.pause", "job.resume", "job.retry", "job.cancel"];

/// The immediate and safe actions that run without a plan. An action not listed here and
/// in none of the other groups is planned, so a new one is never run unplanned by
/// accident.
const DIRECT: &[&str] = &[
    "library.refresh",
    "game.play",
    "game.verify",
    "game.open_folder",
    "game.setup",
    "runner.set_renderer",
    "runner.next_renderer",
    "runner.set_display",
    "exe.run",
    "saves.check",
    "queue.set_rate_limit",
    "doctor.run",
    "settings.set",
    "settings.open_file",
    "app.open_url",
];

/// The actions a strong confirmation asks the game's title for. Every other destructive
/// action asks for a hold.
const TYPED: &[&str] = &[
    "game.remove",
    "game.update",
    "game.move",
    "dlc.remove",
    "snapshot.delete",
    "snapshot.rollback",
];

/// Which way an action goes.
pub fn route(spec: &ActionSpec) -> Route {
    let plan_decides = matches!(
        spec.escalation,
        Some(
            Escalation::PlanThenJobIfDownload
                | Escalation::PlanIfDownload
                | Escalation::ImmediateWhenOff
        )
    );
    if SHELL.contains(&spec.name) {
        Route::Shell
    } else if JOB_CONTROL.contains(&spec.name) {
        Route::JobControl
    } else if matches!(spec.kind, Kind::Prompt | Kind::PromptThenPlan) {
        Route::Prompt
    } else if spec.kind.plans() || plan_decides || !DIRECT.contains(&spec.name) {
        Route::Plan
    } else {
        Route::Direct
    }
}

fn gate_error(gate: Gate) -> ErrorView {
    match gate {
        Gate::CloudWrites => ErrorView::new(
            "Usage",
            "Cloud writes are off",
            "this action writes to GOG's cloud saves",
            "Turn on cloud writes in the app's settings",
            "2",
        ),
    }
}

fn closed_gate(spec: &ActionSpec, gates: Gates) -> Option<Gate> {
    spec.gate().filter(|g| match g {
        Gate::CloudWrites => !gates.cloud_writes,
    })
}

/// The facts the arguments settle are taken from the arguments, whatever the backend
/// says.
fn facts_for(call: &Call, mut facts: PlanFacts) -> PlanFacts {
    facts.purge_state |= call.bool("purge_state") == Some(true);
    facts.turning_off = call.bool("on") == Some(false);
    facts
}

/// What the user was shown.
#[derive(Debug, Clone, PartialEq)]
struct Shown {
    plan: Plan,
    facts: PlanFacts,
    inputs: u64,
    kind: Kind,
    tier: Tier,
}

/// A call on its way through.
#[derive(Debug)]
struct Request {
    call: Call,
    answer: Option<PromptAnswer>,
}

#[derive(Debug)]
enum Asking {
    /// An action asks before it plans.
    Action(Call),
    /// A running job asks.
    Job { job: JobId, reply: PromptReply },
}

#[derive(Debug)]
enum Stage {
    Prompt {
        spec: PromptSpec,
        asking: Asking,
    },
    Planning {
        request: Request,
        wait: PlanWait,
        /// The plan the user confirmed, when this checks it.
        confirmed: Option<Shown>,
    },
    Card {
        request: Request,
        shown: Shown,
    },
    Strong {
        request: Request,
        shown: Shown,
        strength: Strength,
        held_since: Option<Duration>,
    },
}

/// When a screen was presented.
#[derive(Debug, Clone, Copy)]
struct Armed {
    /// The last input event there was.
    after: InputEvent,
    at: Duration,
}

impl Armed {
    fn takes(self, press: Press) -> bool {
        !press.repeat
            && press.event > self.after
            && press.at.checked_sub(self.at).is_some_and(|up| up >= DWELL)
    }
}

#[derive(Debug)]
struct Dialog {
    ticket: Ticket,
    armed: Option<Armed>,
    stage: Stage,
}

#[derive(Debug)]
enum PreviewState {
    Pending(PlanWait),
    Ready(Shown),
    Empty,
    Failed(ErrorView),
}

#[derive(Debug)]
struct Inline {
    ticket: Ticket,
    armed: Option<Armed>,
    /// The last frame it was asked for.
    used: u64,
    call: Call,
    state: PreviewState,
}

/// What a job was started with, for the checks a restart needs.
#[derive(Debug, Clone, Copy)]
struct Origin {
    action: &'static ActionSpec,
    /// Started through a strong confirmation.
    destructive: bool,
}

/// What the flow acts on.
pub(crate) struct Engine<'a, B> {
    pub backend: &'a mut B,
    pub jobs: &'a mut JobTable,
}

/// The state of every dialog, waiting question, error and preview.
#[derive(Debug)]
pub struct SafetyFlow {
    signal: Arc<Signal>,
    tickets: u64,
    last_event: InputEvent,
    dialog: Option<Dialog>,
    /// Job questions that wait for the screen to be free.
    questions: VecDeque<(JobId, PromptSpec, PromptReply)>,
    errors: VecDeque<(Ticket, ErrorView)>,
    previews: Vec<Inline>,
    /// Counts [`pump`](SafetyFlow::pump)s, to tell which previews are still on screen.
    frame: u64,
    /// Where each job came from, for resume and retry.
    origins: HashMap<JobId, Origin>,
    effects: Vec<Effect>,
}

impl SafetyFlow {
    pub(crate) fn new(signal: Arc<Signal>) -> Self {
        SafetyFlow {
            signal,
            tickets: 0,
            last_event: InputEvent(0),
            dialog: None,
            questions: VecDeque::new(),
            errors: VecDeque::new(),
            previews: Vec::new(),
            frame: 0,
            origins: HashMap::new(),
            effects: Vec::new(),
        }
    }

    fn ticket(&mut self) -> Ticket {
        self.tickets += 1;
        Ticket(self.tickets)
    }

    /// Use up `event`: false if it, or a later one, was used already.
    fn fresh(&mut self, event: InputEvent) -> bool {
        let fresh = event > self.last_event;
        if fresh {
            self.last_event = event;
        }
        fresh
    }

    /// The screen on top: a dialog, or else the oldest error.
    pub fn modal(&self) -> Option<Modal<'_>> {
        let Some(d) = &self.dialog else {
            return self.errors.front().map(|(ticket, error)| Modal::Error {
                ticket: *ticket,
                error,
            });
        };
        let ticket = d.ticket;
        Some(match &d.stage {
            Stage::Prompt { spec, asking } => Modal::Prompt {
                ticket,
                spec,
                job: match asking {
                    Asking::Job { job, .. } => Some(job),
                    Asking::Action(_) => None,
                },
            },
            Stage::Planning {
                request, confirmed, ..
            } => Modal::Planning {
                ticket,
                action: request.call.name(),
                confirming: confirmed.is_some(),
            },
            Stage::Card { request, shown } => Modal::PlanCard {
                ticket,
                action: request.call.name(),
                plan: &shown.plan,
                tier: shown.tier,
            },
            Stage::Strong {
                request,
                shown,
                strength,
                ..
            } => Modal::Strong {
                ticket,
                action: request.call.name(),
                plan: &shown.plan,
                strength,
            },
        })
    }

    /// Where an inline preview is; `None` once it is used or dropped.
    pub fn preview_state(&self, ticket: Ticket) -> Option<Preview<'_>> {
        let p = self.previews.iter().find(|p| p.ticket == ticket)?;
        Some(match &p.state {
            PreviewState::Pending(_) => Preview::Pending,
            PreviewState::Ready(shown) => Preview::Ready {
                plan: &shown.plan,
                tier: shown.tier,
            },
            PreviewState::Empty => Preview::Empty,
            PreviewState::Failed(e) => Preview::Failed(e),
        })
    }

    pub(crate) fn push_effect(&mut self, effect: Effect) {
        self.effects.push(effect);
    }

    pub(crate) fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    fn refuse(&mut self, error: ErrorView) -> Outcome {
        self.fail(error.clone());
        Outcome::Refused(error)
    }

    pub(crate) fn fail(&mut self, error: ErrorView) {
        // The same error again, straight after, says nothing new.
        if self.errors.back().is_some_and(|(_, last)| *last == error) {
            return;
        }
        let ticket = self.ticket();
        self.errors.push_back((ticket, error));
        self.signal.raise();
    }

    fn show(&mut self, stage: Stage) {
        let ticket = self.ticket();
        self.dialog = Some(Dialog {
            ticket,
            armed: None,
            stage,
        });
        self.signal.raise();
    }

    /// Put the next waiting question on screen, once nothing else is.
    fn advance(&mut self) {
        if self.dialog.is_none()
            && self.errors.is_empty()
            && let Some((job, spec, reply)) = self.questions.pop_front()
        {
            self.show(Stage::Prompt {
                spec,
                asking: Asking::Job { job, reply },
            });
        }
    }

    // --- what a skin asks for ---

    pub(crate) fn invoke<B: Backend>(
        &mut self,
        engine: &mut Engine<'_, B>,
        invocation: &Invocation,
        event: InputEvent,
    ) -> Result<Call, Outcome> {
        if !self.fresh(event) {
            return Err(Outcome::Ignored);
        }
        // Behind a dialog only stopping work is allowed. This is decided before the
        // call is checked, so a skin can't queue up errors behind one.
        let stops = matches!(invocation.spec.name, "job.cancel" | "job.pause");
        if self.modal().is_some() && !stops {
            return Err(Outcome::Ignored);
        }
        let call = Call::check(invocation).map_err(|e| self.refuse(e))?;
        if let Some(gate) = closed_gate(call.spec(), engine.backend.gates()) {
            return Err(self.refuse(gate_error(gate)));
        }
        Ok(call)
    }

    /// Carry on with a checked call whose route is not [`Route::Shell`].
    pub(crate) fn dispatch<B: Backend>(
        &mut self,
        engine: &mut Engine<'_, B>,
        call: Call,
    ) -> Outcome {
        let request = Request { call, answer: None };
        match route(request.call.spec()) {
            Route::Shell => Outcome::Ignored,
            Route::JobControl => self.control(engine, &request.call),
            Route::Prompt => match engine.backend.prompt_for(&request.call) {
                Ok(spec) => {
                    self.show(Stage::Prompt {
                        spec,
                        asking: Asking::Action(request.call),
                    });
                    Outcome::Accepted
                }
                Err(e) => self.refuse(e),
            },
            Route::Plan => {
                self.plan(engine, request, None);
                Outcome::Accepted
            }
            Route::Direct => {
                let spec = request.call.spec();
                let facts = facts_for(&request.call, PlanFacts::nothing_to_do());
                match spec.decide(&facts, &engine.backend.gates()) {
                    Decision::Run { kind, tier } if runs_unplanned(kind, tier) => {
                        match self.run(engine, request, None, kind, tier) {
                            Ok(()) => Outcome::Accepted,
                            Err(e) => self.refuse(e),
                        }
                    }
                    // A direct action never has a plan to show.
                    _ => self.refuse(ErrorView::failed(
                        "This action can't run",
                        "it needs a plan",
                    )),
                }
            }
        }
    }

    fn control<B: Backend>(&mut self, engine: &mut Engine<'_, B>, call: &Call) -> Outcome {
        let id = JobId::new(call.text("job").unwrap_or_default());
        let restarts = matches!(call.name(), "job.resume" | "job.retry");
        if restarts {
            // Without knowing how a job was started, it is not started again.
            let Some(origin) = self.origins.get(&id).copied() else {
                return self.refuse(ErrorView::usage(
                    "This job can't be restarted",
                    "there is no such job",
                ));
            };
            if let Some(gate) = closed_gate(origin.action, engine.backend.gates()) {
                return self.refuse(gate_error(gate));
            }
            // A strong confirmation covers one run. Only a paused job is still in it.
            if origin.destructive && engine.jobs.state(&id) != Some(JobState::Paused) {
                return self.refuse(ErrorView::usage(
                    "This job can't be restarted",
                    "it removes data: start the action again to see what it would do now",
                ));
            }
        }
        let done = match call.name() {
            "job.cancel" => engine.jobs.cancel(&id),
            "job.pause" => engine.jobs.pause(&id),
            "job.resume" => engine.jobs.resume(&id),
            "job.retry" => engine.jobs.retry(&id),
            _ => false,
        };
        if !done {
            return self.refuse(ErrorView::usage(
                "This action can't run",
                format!("there is no job `{}` can act on", call.name()),
            ));
        }
        if call.name() == "job.cancel" {
            self.forget_questions(&id);
            self.advance();
        }
        Outcome::Accepted
    }

    /// Stop a job. Needs no input event and no free screen.
    pub(crate) fn cancel_job<B: Backend>(&mut self, engine: &mut Engine<'_, B>, id: &JobId) {
        if engine.jobs.cancel(id) {
            self.forget_questions(id);
            self.advance();
        }
    }

    /// Drop the questions of a job; each is answered with a cancel.
    fn forget_questions(&mut self, id: &JobId) {
        self.questions.retain(|(job, ..)| job != id);
        let on_screen = matches!(
            &self.dialog,
            Some(Dialog { stage: Stage::Prompt { asking: Asking::Job { job, .. }, .. }, .. })
                if job == id
        );
        if on_screen {
            self.dialog = None;
            self.signal.raise();
        }
    }

    fn plan<B: Backend>(
        &mut self,
        engine: &mut Engine<'_, B>,
        request: Request,
        confirmed: Option<Shown>,
    ) {
        let (reply, wait) = PlanReply::channel(Arc::clone(&self.signal));
        engine
            .backend
            .plan(&request.call, request.answer.as_ref(), reply);
        self.show(Stage::Planning {
            request,
            wait,
            confirmed,
        });
    }

    fn run<B: Backend>(
        &mut self,
        engine: &mut Engine<'_, B>,
        request: Request,
        handle: Option<PlanHandle>,
        kind: Kind,
        tier: Tier,
    ) -> Result<(), ErrorView> {
        let spec = request.call.spec();
        if let Some(gate) = closed_gate(spec, engine.backend.gates()) {
            return Err(gate_error(gate));
        }
        let listed = matches!(kind, Kind::Job | Kind::PlanThenJob | Kind::PlanThenJobs);
        let mut jobs = engine.jobs.jobs(listed);
        let run = Run::new(request.call, request.answer, handle);
        if let Err(e) = engine.backend.run(run, &mut jobs) {
            jobs.abandon();
            return Err(e);
        }
        for id in jobs.added() {
            let origin = Origin {
                action: spec,
                destructive: tier == Tier::Destructive,
            };
            self.origins.insert(id.clone(), origin);
            if listed {
                self.effects.push(Effect::Toast {
                    job: id.clone(),
                    toast: Toast::Started,
                });
            }
        }
        Ok(())
    }

    // --- what the app's own widgets answer ---

    /// The screen with `ticket` is now drawn, at `now`. `latest` is the last input event
    /// the shell has numbered; only a later one can answer. The first report counts.
    pub fn presented(&mut self, ticket: Ticket, latest: InputEvent, now: Duration) {
        let armed = Armed {
            // An event already used counts even if the shell reports an older one.
            after: latest.max(self.last_event),
            at: now,
        };
        if let Some(d) = self.dialog.as_mut().filter(|d| d.ticket == ticket) {
            d.armed.get_or_insert(armed);
        } else if let Some(p) = self
            .previews
            .iter_mut()
            .find(|p| p.ticket == ticket && matches!(p.state, PreviewState::Ready(_)))
        {
            p.armed.get_or_insert(armed);
        }
    }

    /// Take the dialog if `press` may answer it.
    fn answerable(&mut self, ticket: Ticket, press: Press) -> Option<Dialog> {
        let fresh = self.fresh(press.event);
        let ok = fresh
            && self
                .dialog
                .as_ref()
                .is_some_and(|d| d.ticket == ticket && d.armed.is_some_and(|a| a.takes(press)));
        if ok { self.dialog.take() } else { None }
    }

    /// The plan card's confirm control, or Enter on it (S5).
    pub(crate) fn confirm<B: Backend>(
        &mut self,
        engine: &mut Engine<'_, B>,
        ticket: Ticket,
        press: Press,
    ) -> Outcome {
        if self.previews.iter().any(|p| p.ticket == ticket) {
            return self.confirm_preview(engine, ticket, press);
        }
        match self.answerable(ticket, press) {
            Some(Dialog {
                stage: Stage::Card { request, shown },
                ..
            }) => self.agreed(engine, request, shown),
            Some(other) => {
                // Strong confirmation and prompts are not answered by a plain confirm.
                self.dialog = Some(other);
                Outcome::Ignored
            }
            None => Outcome::Ignored,
        }
    }

    fn confirm_preview<B: Backend>(
        &mut self,
        engine: &mut Engine<'_, B>,
        ticket: Ticket,
        press: Press,
    ) -> Outcome {
        if !self.fresh(press.event) || self.modal().is_some() {
            return Outcome::Ignored;
        }
        let Some(i) = self.previews.iter().position(|p| {
            p.ticket == ticket
                && p.armed.is_some_and(|a| a.takes(press))
                && matches!(p.state, PreviewState::Ready(_))
        }) else {
            return Outcome::Ignored;
        };
        let preview = self.previews.remove(i);
        let PreviewState::Ready(shown) = preview.state else {
            return Outcome::Ignored;
        };
        let request = Request {
            call: preview.call,
            answer: None,
        };
        self.agreed(engine, request, shown)
    }

    /// The user agreed to a plan card: ask for more if the tier needs it, else check the
    /// plan again.
    fn agreed<B: Backend>(
        &mut self,
        engine: &mut Engine<'_, B>,
        request: Request,
        shown: Shown,
    ) -> Outcome {
        if shown.tier != Tier::Destructive {
            return self.recheck(engine, request, shown);
        }
        let subject = shown.plan.subject.trim();
        let strength = if TYPED.contains(&request.call.name()) && !subject.is_empty() {
            Strength::Typed(subject.to_owned())
        } else {
            Strength::Hold
        };
        self.show(Stage::Strong {
            request,
            shown,
            strength,
            held_since: None,
        });
        Outcome::Accepted
    }

    fn recheck<B: Backend>(
        &mut self,
        engine: &mut Engine<'_, B>,
        request: Request,
        shown: Shown,
    ) -> Outcome {
        if let Some(gate) = closed_gate(request.call.spec(), engine.backend.gates()) {
            let outcome = self.refuse(gate_error(gate));
            self.advance();
            return outcome;
        }
        self.plan(engine, request, Some(shown));
        Outcome::Accepted
    }

    /// Submit the typed confirmation.
    pub(crate) fn type_name<B: Backend>(
        &mut self,
        engine: &mut Engine<'_, B>,
        ticket: Ticket,
        typed: &str,
        press: Press,
    ) -> Outcome {
        match self.answerable(ticket, press) {
            Some(Dialog {
                stage:
                    Stage::Strong {
                        request,
                        shown,
                        strength: Strength::Typed(want),
                        ..
                    },
                ..
            }) if typed.trim() == want => self.recheck(engine, request, shown),
            Some(other) => {
                self.dialog = Some(other);
                Outcome::Ignored
            }
            None => Outcome::Ignored,
        }
    }

    /// The hold button went down. The hold is timed from [`Press::at`] against what
    /// [`Model::tick`](crate::Model::tick) gets.
    pub fn hold_start(&mut self, ticket: Ticket, press: Press) -> Outcome {
        let Some(mut dialog) = self.answerable(ticket, press) else {
            return Outcome::Ignored;
        };
        let outcome = match &mut dialog.stage {
            Stage::Strong {
                strength: Strength::Hold,
                held_since,
                ..
            } => {
                *held_since = Some(press.at);
                Outcome::Accepted
            }
            _ => Outcome::Ignored,
        };
        self.dialog = Some(dialog);
        outcome
    }

    /// The hold button came up, or the window lost the input: the hold starts over.
    pub fn hold_stop(&mut self, ticket: Ticket) {
        if let Some(Dialog {
            ticket: t,
            stage: Stage::Strong { held_since, .. },
            ..
        }) = &mut self.dialog
            && *t == ticket
        {
            *held_since = None;
        }
    }

    /// Complete a hold that has lasted [`HOLD`].
    pub(crate) fn tick<B: Backend>(&mut self, engine: &mut Engine<'_, B>, now: Duration) {
        let Some(Dialog {
            stage: Stage::Strong { held_since, .. },
            ..
        }) = &mut self.dialog
        else {
            return;
        };
        let Some(since) = *held_since else {
            return;
        };
        match now.checked_sub(since) {
            // A clock that went back proves nothing.
            None => *held_since = None,
            Some(held) if held >= HOLD => {
                if let Some(Dialog {
                    stage: Stage::Strong { request, shown, .. },
                    ..
                }) = self.dialog.take()
                {
                    self.recheck(engine, request, shown);
                }
            }
            Some(_) => {}
        }
    }

    /// Answer the prompt on screen.
    pub(crate) fn answer<B: Backend>(
        &mut self,
        engine: &mut Engine<'_, B>,
        ticket: Ticket,
        answer: PromptAnswer,
        press: Press,
    ) -> Outcome {
        let Some(dialog) = self.answerable(ticket, press) else {
            return Outcome::Ignored;
        };
        match dialog.stage {
            Stage::Prompt { spec, asking } if spec.accepts(&answer) => match asking {
                Asking::Job { reply, .. } => {
                    reply.answer(answer);
                    self.advance();
                    Outcome::Accepted
                }
                // "No" to an action's own question is a cancel.
                Asking::Action(_) if answer == PromptAnswer::No => {
                    self.advance();
                    Outcome::Accepted
                }
                Asking::Action(call) => {
                    if let Some(gate) = closed_gate(call.spec(), engine.backend.gates()) {
                        return self.refuse(gate_error(gate));
                    }
                    let request = Request {
                        call,
                        answer: Some(answer),
                    };
                    self.plan(engine, request, None);
                    Outcome::Accepted
                }
            },
            stage => {
                self.dialog = Some(Dialog { stage, ..dialog });
                Outcome::Ignored
            }
        }
    }

    /// Cancel a screen or a preview, or dismiss an error. Always allowed, and changes
    /// nothing in the engine (S8). A job's question is answered with a cancel.
    pub fn cancel(&mut self, ticket: Ticket) {
        if self.dialog.as_ref().is_some_and(|d| d.ticket == ticket) {
            self.dialog = None;
        }
        self.errors.retain(|(t, _)| *t != ticket);
        self.previews.retain(|p| p.ticket != ticket);
        self.advance();
        self.signal.raise();
    }

    /// Esc: cancel whatever screen is on top.
    pub fn dismiss(&mut self) -> Outcome {
        match self.modal().map(|m| m.ticket()) {
            Some(ticket) => {
                self.cancel(ticket);
                Outcome::Accepted
            }
            None => Outcome::Ignored,
        }
    }

    // --- previews ---

    /// Plan an action to show inline, before the user asks for it. Planning changes
    /// nothing, so this needs no input. The same call keeps its preview.
    ///
    /// Only [`MAX_PLANNING`] plans are waited for at once, and a preview asked for since
    /// the frame before is never dropped to make room, so a page with more previews than
    /// fit refuses the extra ones instead of planning all of them over and over.
    pub(crate) fn preview<B: Backend>(
        &mut self,
        engine: &mut Engine<'_, B>,
        invocation: &Invocation,
    ) -> Result<Ticket, ErrorView> {
        let call = Call::check(invocation)?;
        if route(call.spec()) != Route::Plan {
            return Err(ErrorView::usage(
                "No preview",
                format!("`{}` has no plan to show", call.name()),
            ));
        }
        if let Some(gate) = closed_gate(call.spec(), engine.backend.gates()) {
            return Err(gate_error(gate));
        }
        let frame = self.frame;
        if let Some(p) = self.previews.iter_mut().find(|p| p.call == call) {
            p.used = frame;
            return Ok(p.ticket);
        }
        let busy = || ErrorView::failed("No preview yet", "too many plans are being shown");
        let pending = |p: &Inline| matches!(p.state, PreviewState::Pending(_));
        if self.previews.iter().filter(|p| pending(p)).count() >= MAX_PLANNING {
            return Err(busy());
        }
        if self.previews.len() >= MAX_PREVIEWS {
            let oldest = self
                .previews
                .iter()
                .enumerate()
                .filter(|(_, p)| !pending(p) && p.used + 1 < frame)
                .min_by_key(|(_, p)| p.used)
                .map(|(i, _)| i)
                .ok_or_else(busy)?;
            self.previews.remove(oldest);
        }
        let (reply, wait) = PlanReply::channel(Arc::clone(&self.signal));
        engine.backend.plan(&call, None, reply);
        let ticket = self.ticket();
        self.previews.push(Inline {
            ticket,
            armed: None,
            used: frame,
            call,
            state: PreviewState::Pending(wait),
        });
        Ok(ticket)
    }

    /// Forget every preview: the data they were computed from changed.
    pub fn clear_previews(&mut self) {
        self.previews.clear();
    }

    // --- what comes back from the engine ---

    pub(crate) fn pump<B: Backend>(
        &mut self,
        engine: &mut Engine<'_, B>,
        questions: Vec<(JobId, PromptSpec, PromptReply)>,
        transitions: &[Transition],
    ) {
        self.frame += 1;
        self.questions.extend(questions);
        for t in transitions {
            self.moved(t);
        }
        // Forget only the jobs the table has forgotten.
        self.origins.retain(|id, _| engine.jobs.contains(id));
        self.receive_plan(engine);
        self.receive_previews(engine.backend.gates());
        self.advance();
    }

    fn moved(&mut self, t: &Transition) {
        if !t.to.is_terminal() {
            return;
        }
        self.forget_questions(&t.id);
        if t.to == JobState::Failed {
            let error = t
                .error
                .clone()
                .unwrap_or_else(|| ErrorView::failed("A job failed", "no reason was given"));
            self.fail(error);
        }
        let toast = match t.to {
            JobState::Done => Some(Toast::Finished),
            JobState::Failed => Some(Toast::Failed),
            _ => None,
        };
        if let Some(toast) = toast.filter(|_| t.listed) {
            self.effects.push(Effect::Toast {
                job: t.id.clone(),
                toast,
            });
        }
    }

    fn receive_plan<B: Backend>(&mut self, engine: &mut Engine<'_, B>) {
        let Some(Dialog {
            stage: Stage::Planning { wait, .. },
            ..
        }) = &self.dialog
        else {
            return;
        };
        let Some(result) = wait.take() else {
            return;
        };
        if let Some(Dialog {
            stage: Stage::Planning {
                request, confirmed, ..
            },
            ..
        }) = self.dialog.take()
        {
            self.signal.raise();
            self.planned(engine, request, confirmed, result);
        }
    }

    fn planned<B: Backend>(
        &mut self,
        engine: &mut Engine<'_, B>,
        request: Request,
        confirmed: Option<Shown>,
        result: Result<Planned, ErrorView>,
    ) {
        let action = request.call.name();
        let (shown, handle) = match judge(&request.call, result, engine.backend.gates()) {
            Judged::Run { shown, handle } => (shown, handle),
            Judged::Nothing => {
                self.effects.push(Effect::NothingToDo { action });
                return;
            }
            Judged::Refused(e) => return self.fail(e),
        };
        let (kind, tier) = (shown.kind, shown.tier);
        match confirmed {
            // Confirmed, and still what the user saw: run the new plan.
            Some(seen) if seen == shown => {
                if let Err(e) = self.run(engine, request, Some(handle), kind, tier) {
                    self.fail(e);
                }
            }
            Some(_) if kind.plans() => {
                self.effects.push(Effect::PlanChanged { action });
                self.show(Stage::Card { request, shown });
            }
            Some(_) => self.fail(ErrorView::failed(
                "The plan changed",
                "nothing was done; try the action again",
            )),
            None if kind.plans() => self.show(Stage::Card { request, shown }),
            // The plan holds no change, so the action is as immediate as it looks.
            None if runs_unplanned(kind, tier) => {
                if let Err(e) = self.run(engine, request, Some(handle), kind, tier) {
                    self.fail(e);
                }
            }
            None => self.fail(ErrorView::failed(
                "This action can't run",
                "it needs a confirmation and has no plan to show",
            )),
        }
    }

    fn receive_previews(&mut self, gates: Gates) {
        for p in &mut self.previews {
            let PreviewState::Pending(wait) = &p.state else {
                continue;
            };
            let Some(result) = wait.take() else {
                continue;
            };
            p.state = match judge(&p.call, result, gates) {
                Judged::Run { shown, .. } if shown.kind.plans() => PreviewState::Ready(shown),
                Judged::Run { .. } | Judged::Nothing => PreviewState::Empty,
                Judged::Refused(e) => PreviewState::Failed(e),
            };
            self.signal.raise();
        }
    }
}

/// What a plan that came back calls for.
enum Judged {
    Run { shown: Shown, handle: PlanHandle },
    Nothing,
    Refused(ErrorView),
}

/// The contract decides the kind and tier, never the backend or the skin.
fn judge(call: &Call, result: Result<Planned, ErrorView>, gates: Gates) -> Judged {
    let planned = match result {
        Ok(p) => p,
        Err(e) => return Judged::Refused(e),
    };
    let facts = facts_for(call, planned.facts);
    match call.spec().decide(&facts, &gates) {
        Decision::Run { kind, tier } => Judged::Run {
            shown: Shown {
                plan: planned.plan,
                facts,
                inputs: planned.inputs,
                kind,
                tier,
            },
            handle: planned.handle,
        },
        Decision::NothingToDo => Judged::Nothing,
        Decision::Blocked(gate) => Judged::Refused(gate_error(gate)),
    }
}

/// Whether something may run with no plan card: only what is safe and plans nothing.
fn runs_unplanned(kind: Kind, tier: Tier) -> bool {
    !kind.plans() && tier == Tier::Safe
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No action of contract 1.0 reaches an unplanned run above `Safe`, so the rule is
    /// checked on its own: a future one fails closed.
    #[test]
    fn only_safe_unplanned_kinds_run_without_a_card() {
        use Kind::{Immediate, Job, Plan, PlanThenJob, PlanThenJobs, Prompt, PromptThenPlan};
        for kind in [
            Immediate,
            Plan,
            Prompt,
            Job,
            PlanThenJob,
            PlanThenJobs,
            PromptThenPlan,
        ] {
            for tier in [Tier::Safe, Tier::Confirm, Tier::Gated, Tier::Destructive] {
                let expected = tier == Tier::Safe && matches!(kind, Immediate | Prompt | Job);
                assert_eq!(runs_unplanned(kind, tier), expected, "{kind:?} {tier:?}");
            }
        }
    }
}
