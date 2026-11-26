//! What a skin sends is checked against the contract before anything looks at it, and
//! the parts of an action the model does itself.

mod common;

use common::{App, call};
use gogo_contract::actions::{self, ActionSpec};
use gogo_contract::{Kind, Tier, Value};
use gogo_gui_model::{Call, Effect, Invocation, Outcome, SourceKey};

fn invocation(name: &str) -> Invocation {
    Invocation::new(actions::action(name).unwrap())
}

fn refused(i: &Invocation) -> bool {
    Call::check(i).is_err()
}

#[test]
fn bad_arguments_are_refused() {
    let id = || Value::str("1");
    // Unknown, repeated, missing and ill-typed.
    assert!(refused(
        &call("game.play").arg("path", Value::str("/bin/sh"))
    ));
    assert!(refused(&call("game.play").arg("id", id())));
    assert!(refused(&invocation("game.play")));
    assert!(refused(
        &invocation("game.play").arg("id", Value::Bool(true))
    ));
    assert!(refused(&invocation("game.play").arg("id", Value::str(""))));
    assert!(refused(
        &invocation("game.play").arg("id", Value::str("a\nb"))
    ));
    assert!(refused(&call("game.play").arg("exe", Value::Num(1.5))));
    assert!(refused(&call("game.play").arg("exe", Value::Num(-1.0))));
    assert!(refused(&call("game.play").arg("exe", Value::Num(f64::NAN))));
    assert!(refused(
        &invocation("game.open_folder")
            .arg("id", id())
            .arg("which", Value::str("/etc"))
    ));
    // An empty list of ids must not mean every game.
    assert!(refused(
        &invocation("updates.apply_all").arg("ids", Value::List(vec![]))
    ));
    assert!(refused(&invocation("game.play").route_arg("id", id())));
}

/// S1, S2: a masked value is never an argument, however deep.
#[test]
fn masked_values_are_refused() {
    assert!(refused(&invocation("game.play").arg("id", Value::Masked)));
    assert!(refused(
        &invocation("clipboard.copy").arg("field", Value::List(vec![Value::Masked]))
    ));
    let record = Value::Map([("os".to_owned(), Value::Masked)].into());
    assert!(refused(
        &invocation("game.download")
            .arg("id", Value::str("1"))
            .arg("selection", record)
    ));
}

/// An invocation can't bring its own kind or tier.
#[test]
fn only_the_name_of_the_spec_is_used() {
    static FORGED: ActionSpec = ActionSpec {
        name: "game.remove",
        params: &[],
        kind: Kind::Immediate,
        tier: Tier::Safe,
        escalation: None,
        binding: gogo_contract::Binding::Skin,
    };
    assert!(
        refused(&Invocation::new(&FORGED)),
        "its real parameters apply"
    );

    let mut app = App::new();
    let mut forged = call("game.remove");
    forged.spec = &FORGED;
    app.invoke(&forged);
    assert_eq!(app.screen(), "card");
    assert_eq!(app.runs(), 0);
}

#[test]
fn literals_are_read_as_their_parameter_types() {
    let c = Call::check(
        &invocation("snapshot.rollback")
            .arg("id", Value::Num(12.0))
            .arg("n", Value::str("3"))
            .arg("drop_saves", Value::str("false")),
    )
    .unwrap();
    assert_eq!(c.text("id"), Some("12"));
    assert_eq!(c.arg("n"), Some(&Value::Num(3.0)));
    assert_eq!(c.bool("drop_saves"), Some(false));
}

#[test]
fn navigation_checks_the_route_and_keeps_history() {
    let go = |route: &str| invocation("nav.go").arg("route", Value::str(route));
    assert!(refused(&go("plan")));
    assert!(refused(&go("game")), "`game` needs an id");
    assert!(refused(&go("queue").route_arg("id", Value::str("1"))));
    assert!(refused(
        &go("game")
            .route_arg("id", Value::str("1"))
            .route_arg("section", Value::str("nope"))
    ));

    let mut app = App::new();
    let route = SourceKey::new("route", &[]).unwrap();
    let current = |app: &App| format!("{:?}", app.model.value(&route).unwrap());
    app.invoke(&go("game").route_arg("id", Value::str("1")));
    assert!(current(&app).contains("game"));
    assert_eq!(app.model.location().params["id"], Value::str("1"));
    app.invoke(&invocation("nav.back"));
    assert!(current(&app).contains("library"));
    app.invoke(&invocation("nav.back"));
    assert_eq!(app.model.location().route, "library");
}

#[test]
fn shell_actions_become_effects_and_never_reach_the_backend() {
    let mut app = App::new();
    app.invoke(&invocation("clipboard.copy").arg("field", Value::Num(7.0)));
    app.invoke(&invocation("account.menu"));
    app.invoke(&invocation("palette.open"));
    app.invoke(
        &invocation("ui.set")
            .arg("var", Value::str("tab"))
            .arg("value", Value::Bool(true)),
    );
    assert_eq!(
        app.model.take_effects(),
        [
            Effect::Copy("7".into()),
            Effect::AccountMenu,
            Effect::OpenPalette { scope: None },
            Effect::SetUi {
                var: "tab".into(),
                value: Value::Bool(true)
            },
        ]
    );
    assert_eq!(app.runs(), 0);
}

#[test]
fn a_job_action_on_a_job_that_is_not_there_is_refused() {
    let mut app = App::new();
    let i = invocation("job.cancel").arg("job", Value::str("job-9"));
    assert!(matches!(app.invoke(&i), Outcome::Refused(_)));
}

#[test]
fn jobs_reach_the_queue_sources_with_toasts() {
    let mut app = App::new();
    let queue = SourceKey::new("queue", &[]).unwrap();
    assert_eq!(app.model.value(&queue), Some(&Value::List(vec![])));
    app.invoke(&call("game.verify"));
    let Some(Value::List(jobs)) = app.model.value(&queue) else {
        panic!("a list")
    };
    assert_eq!(jobs.len(), 1);
    assert!(matches!(
        app.model.take_effects().as_slice(),
        [Effect::Toast { .. }]
    ));
}
