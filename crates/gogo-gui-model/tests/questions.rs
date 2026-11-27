//! Questions asked while planning, and jobs that can't be stopped (design 25 §4).

mod common;

use common::{App, call};
use gogo_contract::{Value, actions};
use gogo_gui_model::{
    DWELL, Invocation, JobKind, JobState, JobStatus, Modal, Outcome, Press, PromptAnswer,
    PromptSpec,
};

fn choice(label: &str) -> PromptSpec {
    PromptSpec::Choice {
        label: label.to_owned(),
        options: vec!["one".to_owned(), "two".to_owned()],
    }
}

/// `game.build` asks a choice, then a yes or no.
fn asking() -> App {
    let mut app = App::new();
    app.fake().plan_questions.insert(
        "game.build",
        vec![
            choice("first"),
            PromptSpec::Confirm {
                label: "second".to_owned(),
            },
        ],
    );
    assert_eq!(app.invoke(&call("game.build")), Outcome::Accepted);
    app.model.pump();
    app
}

fn label(app: &App) -> String {
    match app.model.modal() {
        Some(Modal::Prompt {
            spec: PromptSpec::Choice { label, .. } | PromptSpec::Confirm { label },
            action,
            job,
            ..
        }) => {
            assert_eq!((action, job), (Some("game.build"), None));
            label.clone()
        }
        other => panic!("no question: {other:?}"),
    }
}

fn answer(app: &mut App, answer: PromptAnswer) -> Outcome {
    let ticket = app.draw();
    let press = app.press();
    let outcome = app.model.answer(ticket, answer, press);
    app.model.pump();
    app.model.pump();
    outcome
}

#[test]
fn each_question_is_answered_before_the_card_and_the_check_reuses_the_answers() {
    let mut app = asking();
    assert_eq!(label(&app), "first");
    assert_eq!(answer(&mut app, PromptAnswer::Choice(1)), Outcome::Accepted);
    assert_eq!(label(&app), "second");
    // "No" answers the question; it doesn't give up.
    assert_eq!(answer(&mut app, PromptAnswer::No), Outcome::Accepted);
    assert_eq!(app.screen(), "card");
    assert_eq!(app.runs(), 0);

    assert_eq!(app.confirm(), Outcome::Accepted);
    app.model.pump();
    assert_eq!(app.ran(), ["game.build"]);
    let all = vec![PromptAnswer::Choice(1), PromptAnswer::No];
    assert_eq!(
        app.fake().plan_answers,
        [vec![], all[..1].to_vec(), all.clone(), all]
    );
}

#[test]
fn cancelling_any_question_runs_nothing_and_forgets_the_answers() {
    for after in 0..2 {
        let mut app = asking();
        for _ in 0..after {
            answer(&mut app, PromptAnswer::Choice(0));
        }
        let ticket = app.draw();
        app.model.cancel(ticket);
        app.model.pump();
        assert_eq!(app.screen(), "none", "after {after}");
        assert_eq!(app.runs(), 0);

        // Esc, and the next time the questions start over.
        assert_eq!(app.invoke(&call("game.build")), Outcome::Accepted);
        app.model.pump();
        assert_eq!(label(&app), "first");
        app.draw();
        app.model.dismiss();
        app.model.pump();
        assert_eq!(app.screen(), "none");
        assert_eq!(app.runs(), 0);
    }
}

#[test]
fn a_second_question_takes_neither_the_first_press_nor_an_early_one() {
    let mut app = asking();
    let first = app.draw();
    let press = app.press();
    app.model.answer(first, PromptAnswer::Choice(0), press);
    app.model.pump();
    let second = app.ticket();
    assert_ne!(first, second);
    // Not drawn yet.
    let early = app.press();
    assert_eq!(
        app.model.answer(second, PromptAnswer::Yes, early),
        Outcome::Ignored
    );
    app.show(second);
    // The press that answered the first, and the first's ticket.
    assert_eq!(
        app.model.answer(second, PromptAnswer::Yes, press),
        Outcome::Ignored
    );
    let late = app.press();
    assert_eq!(
        app.model.answer(first, PromptAnswer::Yes, late),
        Outcome::Ignored
    );
    // A press that came down within the dwell.
    app.model.presented(second, app.last_input(), app.now);
    let quick = Press {
        at: app.now - DWELL,
        ..app.press()
    };
    assert_eq!(
        app.model.answer(second, PromptAnswer::Yes, quick),
        Outcome::Ignored
    );
    assert_eq!(label(&app), "second");
}

#[test]
fn a_job_that_cant_be_stopped_refuses_cancel_pause_and_closing() {
    let mut app = App::new();
    assert_eq!(app.invoke(&call("game.verify")), Outcome::Accepted);
    let busy = JobStatus {
        state: JobState::Running,
        stoppable: false,
        ..JobStatus::queued(JobKind::Setup)
    };
    app.fake().sink(0).report(busy.clone());
    app.model.pump();
    let id = app.model.jobs().ids().next().unwrap().clone();

    for name in ["job.cancel", "job.pause"] {
        let invocation =
            Invocation::new(actions::action(name).unwrap()).arg("job", Value::str(id.as_str()));
        assert!(
            matches!(app.invoke(&invocation), Outcome::Refused(_)),
            "{name}"
        );
        let ticket = app.draw();
        app.model.cancel(ticket);
    }
    app.model.cancel_job(&id);
    assert!(app.fake().controls.lock().unwrap().is_empty());

    assert!(app.model.refuse_close());
    assert_eq!(app.screen(), "error");
    let ticket = app.ticket();
    app.model.cancel(ticket);

    // Once the step is over the job can be stopped and the window closed.
    app.fake().sink(0).report(JobStatus {
        stoppable: true,
        ..busy.clone()
    });
    app.model.pump();
    assert!(!app.model.refuse_close());
    // A job that ended protects nothing, whatever it last said.
    app.fake().sink(0).report(JobStatus {
        state: JobState::Done,
        ..busy
    });
    app.model.pump();
    assert!(!app.model.refuse_close());
}
