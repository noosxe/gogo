//! The job table: one control per job (the model's side of #532) and coalesced progress.

use std::sync::{Arc, Mutex};

use gogo_contract::{Value, shapes};
use gogo_gui_model::{
    ErrorView, FileProgress, JobControl, JobId, JobKind, JobSink, JobState, JobStatus, JobTable,
    Signal, Step, StepState,
};

type Calls = Arc<Mutex<Vec<(&'static str, &'static str)>>>;
type Sinks = Arc<Mutex<Vec<JobSink>>>;

/// Records what it is asked to do.
struct Control {
    name: &'static str,
    calls: Calls,
    restarted: Sinks,
}

impl JobControl for Control {
    fn cancel(&mut self) {
        self.calls.lock().unwrap().push((self.name, "cancel"));
    }
    fn pause(&mut self) {
        self.calls.lock().unwrap().push((self.name, "pause"));
    }
    fn resume(&mut self, sink: JobSink) {
        self.calls.lock().unwrap().push((self.name, "resume"));
        self.restarted.lock().unwrap().push(sink);
    }
    fn retry(&mut self, sink: JobSink) {
        self.calls.lock().unwrap().push((self.name, "retry"));
        self.restarted.lock().unwrap().push(sink);
    }
}

struct Rig {
    table: JobTable,
    calls: Calls,
    restarted: Sinks,
}

impl Rig {
    fn new() -> Self {
        Rig {
            table: JobTable::new(Signal::new(|| {})),
            calls: Calls::default(),
            restarted: Sinks::default(),
        }
    }

    fn add(&mut self, name: &'static str, listed: bool) -> (JobId, JobSink) {
        let control = Control {
            name,
            calls: Arc::clone(&self.calls),
            restarted: Arc::clone(&self.restarted),
        };
        let mut jobs = self.table.jobs(listed);
        let sink = jobs.add(JobStatus::queued(JobKind::Download), Box::new(control));
        (jobs.added()[0].clone(), sink)
    }

    fn calls(&self) -> Vec<(&'static str, &'static str)> {
        self.calls.lock().unwrap().clone()
    }
}

fn status(state: JobState) -> JobStatus {
    JobStatus {
        state,
        ..JobStatus::queued(JobKind::Download)
    }
}

#[test]
fn cancelling_one_job_leaves_the_other_to_finish() {
    let mut rig = Rig::new();
    let (a, sink_a) = rig.add("a", true);
    let (b, sink_b) = rig.add("b", true);
    sink_a.report(status(JobState::Running));
    sink_b.report(status(JobState::Running));

    assert!(rig.table.cancel(&a));
    assert_eq!(rig.calls(), [("a", "cancel")]);
    sink_a.report(status(JobState::Cancelled));
    sink_b.report(status(JobState::Done));
    rig.table.drain();

    assert_eq!(rig.table.status(&a).unwrap().state, JobState::Cancelled);
    assert_eq!(rig.table.status(&b).unwrap().state, JobState::Done);
}

#[test]
fn a_late_progress_report_cannot_hide_a_failure() {
    let mut rig = Rig::new();
    let (id, sink) = rig.add("a", true);
    let failed = JobStatus {
        error: Some(ErrorView::failed("Download failed", "disk full")),
        ..status(JobState::Failed)
    };
    assert!(sink.report(status(JobState::Running)));
    assert!(sink.report(failed.clone()));
    assert!(!sink.report(status(JobState::Running)));

    let moved = rig.table.drain();
    assert_eq!(moved.len(), 1, "intermediate states are coalesced");
    assert_eq!(moved[0].to, JobState::Failed);
    assert_eq!(moved[0].error, failed.error);
    assert_eq!(rig.table.status(&id), Some(&failed));
}

#[test]
fn controls_apply_only_in_states_that_take_them() {
    let mut rig = Rig::new();
    let (id, sink) = rig.add("a", true);
    assert!(!rig.table.resume(&id));
    assert!(!rig.table.retry(&id));
    assert!(rig.table.pause(&id));
    sink.report(status(JobState::Done));
    assert!(!rig.table.pause(&id));
    assert!(!rig.table.resume(&id));
    assert!(rig.table.cancel(&id), "an ended job is known");
    assert_eq!(rig.calls(), [("a", "pause")], "and left alone");

    let nothing = JobId::new("job-99");
    assert!(!rig.table.cancel(&nothing));
    assert!(!rig.table.pause(&nothing));
}

#[test]
fn retry_and_resume_start_a_new_run_and_shut_out_the_old_one() {
    let mut rig = Rig::new();
    let (id, old) = rig.add("a", true);
    old.report(status(JobState::Failed));
    assert!(rig.table.retry(&id));
    rig.table.drain();
    assert_eq!(rig.table.status(&id).unwrap().state, JobState::Queued);
    assert!(!old.report(status(JobState::Done)), "the old run is over");

    let new = rig.restarted.lock().unwrap()[0].clone();
    assert!(new.report(status(JobState::Cancelled)));
    assert!(rig.table.resume(&id));
    assert!(!new.report(status(JobState::Done)));
}

#[test]
fn unlisted_work_is_reported_and_then_forgotten() {
    let mut rig = Rig::new();
    let (_, sink) = rig.add("a", false);
    assert_eq!(rig.table.ids().count(), 0);
    sink.report(status(JobState::Failed));
    let moved = rig.table.drain();
    assert_eq!(moved.len(), 1);
    assert!(!moved[0].listed);
    assert_eq!(rig.table.queue_value(), Value::List(vec![]));
    assert!(rig.table.drain().is_empty());
}

#[test]
fn an_abandoned_run_cancels_and_removes_its_jobs() {
    let mut rig = Rig::new();
    let calls = Arc::clone(&rig.calls);
    let restarted = Arc::clone(&rig.restarted);
    let mut jobs = rig.table.jobs(true);
    jobs.add(
        JobStatus::queued(JobKind::Update),
        Box::new(Control {
            name: "a",
            calls,
            restarted,
        }),
    );
    jobs.abandon();
    assert_eq!(rig.calls(), [("a", "cancel")]);
    assert_eq!(rig.table.ids().count(), 0);
}

/// The records have exactly the fields the contract gives them.
#[test]
fn job_values_have_the_contract_shape() {
    fn keys(v: &Value) -> Vec<String> {
        match v {
            Value::Map(m) => m.keys().cloned().collect(),
            other => panic!("not a record: {other:?}"),
        }
    }
    fn names(shape: &shapes::Shape) -> Vec<String> {
        let mut n: Vec<String> = shape.fields.iter().map(|f| f.name.to_owned()).collect();
        n.sort();
        n
    }
    let mut rig = Rig::new();
    let (id, sink) = rig.add("a", true);
    sink.report(JobStatus {
        steps: vec![Step {
            name: "Unpack".into(),
            state: StepState::Now,
            took: None,
        }],
        files: vec![FileProgress {
            name: "setup.bin".into(),
            bytes: 1,
            total: 2,
            chunks_done: 0,
            chunks_total: 1,
            state: "downloading".into(),
        }],
        error: Some(ErrorView::failed("t", "m")),
        rate: 10,
        ..status(JobState::Running)
    });
    rig.table.drain();

    let job = rig.table.job_value(&id).unwrap();
    assert_eq!(keys(&job), names(&shapes::JOB));
    let Value::Map(m) = &job else { unreachable!() };
    let first = |k: &str| match &m[k] {
        Value::List(l) => l[0].clone(),
        other => panic!("not a list: {other:?}"),
    };
    assert_eq!(keys(&first("steps")), names(&shapes::STEP));
    assert_eq!(keys(&first("files")), names(&shapes::FILE_PROGRESS));
    assert_eq!(keys(&m["error"]), names(&shapes::ERROR_VIEW));

    let summary = rig.table.summary_value(Some(100));
    assert_eq!(keys(&summary), names(&shapes::QUEUE_SUMMARY));
    let Value::Map(s) = summary else {
        unreachable!()
    };
    assert_eq!(s["active"], Value::Num(1.0));
    assert_eq!(s["rate"], Value::Num(10.0));
    assert_eq!(s["current"], job);
}
