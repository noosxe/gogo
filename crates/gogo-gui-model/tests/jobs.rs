//! The job table: one control per job (the model's side of #532) and coalesced progress.

use std::sync::{Arc, Mutex};

use gogo_contract::{Value, shapes};
use gogo_gui_model::{
    ErrorView, FileProgress, JobControl, JobId, JobKind, JobSink, JobState, JobStatus, JobTable,
    MAX_ENDED, PromptReply, PromptSpec, Signal, Step, StepState,
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
    rig.table.drain();
    assert!(rig.table.retry(&id));
    rig.table.drain();
    assert_eq!(rig.table.status(&id).unwrap().state, JobState::Queued);
    assert!(!old.report(status(JobState::Done)), "the old run is over");

    let new = rig.restarted.lock().unwrap()[0].clone();
    assert!(new.report(status(JobState::Cancelled)));
    rig.table.drain();
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

#[test]
fn ending_again_before_the_next_drain_is_still_reported() {
    let mut rig = Rig::new();
    let (id, sink) = rig.add("a", true);
    sink.report(status(JobState::Failed));
    assert_eq!(rig.table.drain().len(), 1);
    assert!(rig.table.retry(&id));
    let again = rig.restarted.lock().unwrap()[0].clone();
    again.report(status(JobState::Failed));
    assert!(!rig.table.retry(&id), "that failure is unseen");
    let moved = rig.table.drain();
    assert_eq!(moved.len(), 1, "the second failure is not lost");
    assert_eq!(moved[0].to, JobState::Failed);
}

/// A restart can't overwrite an end nobody has been told about (S9).
#[test]
fn a_job_cannot_be_restarted_before_its_end_is_drained() {
    for (end, restart) in [
        (
            JobState::Failed,
            JobTable::retry as fn(&mut JobTable, &JobId) -> bool,
        ),
        (JobState::Cancelled, JobTable::resume),
    ] {
        let mut rig = Rig::new();
        let (id, sink) = rig.add("a", true);
        sink.report(status(JobState::Running));
        rig.table.drain();
        sink.report(status(end));
        assert!(!restart(&mut rig.table, &id), "{end:?}");
        assert!(rig.calls().is_empty(), "{end:?}");
        let moved = rig.table.drain();
        assert_eq!(moved[0].to, end);
        assert!(restart(&mut rig.table, &id), "{end:?}");
    }
}

#[test]
fn a_job_added_already_failed_is_reported() {
    let mut rig = Rig::new();
    let calls = Arc::clone(&rig.calls);
    let restarted = Arc::clone(&rig.restarted);
    rig.table.jobs(true).add(
        status(JobState::Failed),
        Box::new(Control {
            name: "a",
            calls,
            restarted,
        }),
    );
    let moved = rig.table.drain();
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].to, JobState::Failed);
}

#[test]
fn only_the_newest_ended_jobs_are_kept() {
    let mut rig = Rig::new();
    let (live, running) = rig.add("live", true);
    running.report(status(JobState::Running));
    let ids: Vec<JobId> = (0..MAX_ENDED + 3)
        .map(|_| {
            let (id, sink) = rig.add("a", true);
            sink.report(status(JobState::Done));
            id
        })
        .collect();
    assert_eq!(
        rig.table.drain().len(),
        MAX_ENDED + 4,
        "each is reported first"
    );
    assert_eq!(rig.table.ids().count(), MAX_ENDED + 1);
    assert!(rig.table.contains(&live), "a live job is never dropped");
    assert!(!rig.table.contains(&ids[2]));
    assert!(rig.table.contains(&ids[3]));
}

#[test]
fn questions_come_in_the_order_they_were_asked() {
    let mut rig = Rig::new();
    let (a, sink_a) = rig.add("a", true);
    let (b, sink_b) = rig.add("b", true);
    for (sink, label) in [(&sink_b, "1"), (&sink_a, "2"), (&sink_b, "3")] {
        sink.ask(
            PromptSpec::Confirm {
                label: label.into(),
            },
            PromptReply::new(|_| {}),
        );
    }
    let asked: Vec<_> = rig
        .table
        .take_prompts()
        .into_iter()
        .map(|(id, spec, _)| (id, spec))
        .collect();
    let ids: Vec<_> = asked.iter().map(|(id, _)| id.clone()).collect();
    assert_eq!(ids, [b.clone(), a, b]);
}
