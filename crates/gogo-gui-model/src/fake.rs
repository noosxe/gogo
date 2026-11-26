//! A scripted [`Backend`] that records what it is asked, for tests and for drawing the
//! app without an engine. It does no I/O. Behind the `fake` feature.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use gogo_contract::{Gates, PlanFacts};

use crate::backend::{Backend, Run, Subscription};
use crate::call::Call;
use crate::error::ErrorView;
use crate::jobs::{JobControl, JobKind, JobSink, JobStatus, Jobs};
use crate::plan::{Plan, PlanHandle, PlanReply, PlanStep, Planned, StepKind};
use crate::prompt::{PromptAnswer, PromptSpec};
use crate::source::{Slot, SourceKey, lock};

/// The plan the fake answers with.
#[derive(Debug, Clone)]
pub struct FakePlan {
    pub plan: Plan,
    pub facts: PlanFacts,
    pub inputs: u64,
}

impl Default for FakePlan {
    fn default() -> Self {
        FakePlan {
            plan: Plan {
                verb: "Do".to_owned(),
                title: "Do it?".to_owned(),
                subject: "Example Game".to_owned(),
                steps: vec![PlanStep {
                    kind: StepKind::Info,
                    text: "A step".to_owned(),
                    bytes: None,
                }],
                reversible: true,
                ..Plan::default()
            },
            facts: PlanFacts::changes(),
            inputs: 0,
        }
    }
}

impl FakePlan {
    /// A plan with these facts.
    pub fn with(facts: PlanFacts) -> Self {
        FakePlan {
            facts,
            ..FakePlan::default()
        }
    }
}

/// One call of [`Backend::run`].
#[derive(Debug, Clone, PartialEq)]
pub struct RunRecord {
    pub call: Call,
    pub answer: Option<PromptAnswer>,
    /// Which computed plan ran, counting from 1.
    pub plan: Option<u64>,
}

/// What a job's control was asked to do: the job's number and the word.
pub type ControlLog = Arc<Mutex<Vec<(usize, &'static str)>>>;

struct Control {
    job: usize,
    log: ControlLog,
    sinks: Arc<Mutex<Vec<JobSink>>>,
}

impl JobControl for Control {
    fn cancel(&mut self) {
        lock(&self.log).push((self.job, "cancel"));
    }
    fn pause(&mut self) {
        lock(&self.log).push((self.job, "pause"));
    }
    fn resume(&mut self, sink: JobSink) {
        lock(&self.log).push((self.job, "resume"));
        lock(&self.sinks)[self.job] = sink;
    }
    fn retry(&mut self, sink: JobSink) {
        lock(&self.log).push((self.job, "retry"));
        lock(&self.sinks)[self.job] = sink;
    }
}

/// The fake. Its fields script what it answers and record what it was asked.
#[derive(Default)]
pub struct FakeBackend {
    pub gates: Gates,
    pub rate_limit: Option<u64>,
    /// The plan per action name; actions not here get [`FakePlan::default`].
    pub plans: HashMap<&'static str, Result<FakePlan, ErrorView>>,
    /// Keep plan requests in [`held`](Self::held) instead of answering at once.
    pub hold_plans: bool,
    pub held: VecDeque<(&'static str, PlanReply)>,
    /// The question per action name.
    pub prompts: HashMap<&'static str, PromptSpec>,
    /// Refuse every run with this.
    pub run_error: Option<ErrorView>,
    /// The actions planned, in order.
    pub planned: Vec<&'static str>,
    /// Everything that ran.
    pub runs: Vec<RunRecord>,
    /// The sink of each job, by number.
    pub sinks: Arc<Mutex<Vec<JobSink>>>,
    pub controls: ControlLog,
    pub subscriptions: Vec<(SourceKey, Slot)>,
    serial: u64,
}

impl FakeBackend {
    pub fn new() -> Self {
        FakeBackend::default()
    }

    /// Script the plan of an action.
    pub fn set_plan(&mut self, action: &'static str, plan: FakePlan) {
        self.plans.insert(action, Ok(plan));
    }

    /// Answer a plan request now, as scripted.
    pub fn reply(&mut self, action: &'static str, reply: PlanReply) {
        let scripted = self.plans.get(action).cloned();
        self.serial += 1;
        reply.send(
            scripted
                .unwrap_or_else(|| Ok(FakePlan::default()))
                .map(|p| Planned {
                    plan: p.plan,
                    facts: p.facts,
                    inputs: p.inputs,
                    handle: PlanHandle::new(self.serial),
                }),
        );
    }

    /// Answer the oldest held request.
    pub fn reply_next(&mut self) {
        if let Some((action, reply)) = self.held.pop_front() {
            self.reply(action, reply);
        }
    }

    /// The sink of job number `n`.
    ///
    /// # Panics
    /// If there is no such job.
    pub fn sink(&self, n: usize) -> JobSink {
        lock(&self.sinks)[n].clone()
    }
}

impl Backend for FakeBackend {
    fn subscribe(&mut self, key: &SourceKey, slot: Slot) -> Subscription {
        self.subscriptions.push((key.clone(), slot));
        Box::new(())
    }

    fn gates(&self) -> Gates {
        self.gates
    }

    fn rate_limit(&self) -> Option<u64> {
        self.rate_limit
    }

    fn plan(&mut self, call: &Call, _answer: Option<&PromptAnswer>, reply: PlanReply) {
        self.planned.push(call.name());
        if self.hold_plans {
            self.held.push_back((call.name(), reply));
        } else {
            self.reply(call.name(), reply);
        }
    }

    fn prompt_for(&mut self, call: &Call) -> Result<PromptSpec, ErrorView> {
        Ok(self
            .prompts
            .get(call.name())
            .cloned()
            .unwrap_or_else(|| PromptSpec::Text {
                label: "Value".to_owned(),
                default: String::new(),
            }))
    }

    fn run(&mut self, run: Run, jobs: &mut Jobs<'_>) -> Result<(), ErrorView> {
        let call = run.call().clone();
        let answer = run.answer().cloned();
        let plan = run.into_plan().and_then(PlanHandle::take::<u64>);
        self.runs.push(RunRecord { call, answer, plan });
        let job = lock(&self.sinks).len();
        let sink = jobs.add(
            JobStatus::queued(JobKind::Download),
            Box::new(Control {
                job,
                log: Arc::clone(&self.controls),
                sinks: Arc::clone(&self.sinks),
            }),
        );
        lock(&self.sinks).push(sink);
        match &self.run_error {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }
}
