//! One group of tests per safety invariant the model enforces (design 26 §7), named
//! after it: S4, S5, S6, S8, S9, S19 and S21. S7 is checked at skin load, not here.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{App, call, removes_dlc};
use gogo_contract::actions::{self, ACTIONS};
use gogo_contract::{Binding, PlanFacts, Tier, Value};
use gogo_gui_model::fake::FakePlan;
use gogo_gui_model::safety::route;
use gogo_gui_model::{
    DWELL, Effect, ErrorView, HOLD, InputEvent, Invocation, JobId, JobKind, JobState, JobStatus,
    Modal, Outcome, Press, Preview, PromptAnswer, PromptReply, PromptSpec, Route, Secret,
    SourceKey, Strength,
};

fn status(state: JobState) -> JobStatus {
    JobStatus {
        state,
        ..JobStatus::queued(JobKind::Download)
    }
}

/// An app with one running job, started by `game.verify`.
fn with_job() -> (App, String) {
    let mut app = App::new();
    assert_eq!(app.invoke(&call("game.verify")), Outcome::Accepted);
    app.fake().sink(0).report(status(JobState::Running));
    app.model.pump();
    let id = app.model.jobs().ids().next().unwrap().as_str().to_owned();
    (app, id)
}

fn job_call(name: &str, id: &str) -> Invocation {
    Invocation::new(actions::action(name).unwrap()).arg("job", Value::str(id))
}

type Answers = Arc<Mutex<Vec<Option<PromptAnswer>>>>;

/// The running job asks a yes/no question.
fn ask(app: &mut App, answers: &Answers) {
    let answers = Arc::clone(answers);
    app.fake().sink(0).ask(
        PromptSpec::Confirm {
            label: "Delete the cloud copy?".into(),
        },
        PromptReply::new(move |a| answers.lock().unwrap().push(a)),
    );
    app.model.pump();
}

// --- S4: actions fire only from user input ---

#[test]
fn s4_one_action_per_input_event() {
    let mut app = App::new();
    let event = app.input();
    assert_eq!(
        app.model.invoke(&call("game.play"), event),
        Outcome::Accepted
    );
    assert_eq!(
        app.model.invoke(&call("game.play"), event),
        Outcome::Ignored
    );
    assert_eq!(
        app.model.invoke(&call("library.refresh"), event),
        Outcome::Ignored
    );
    assert_eq!(
        app.model.invoke(&call("game.play"), InputEvent(0)),
        Outcome::Ignored
    );
    assert_eq!(app.ran(), ["game.play"]);
}

#[test]
fn s4_loading_sources_previews_and_frames_run_nothing() {
    let mut app = App::new();
    app.model.watch(&SourceKey::new("game", &["1"]).unwrap());
    let slot = app.fake().subscriptions[0].1.clone();
    slot.set(Value::str("changed"));
    app.model.preview(&call("game.update")).unwrap();
    app.model.pump();
    app.model.tick(Duration::from_secs(60));
    assert_eq!(app.runs(), 0);
    assert_eq!(app.screen(), "none");
}

// --- S5: every action whose plan is non-empty shows it first ---

#[test]
fn s5_skin_action_stops_at_the_plan_card() {
    let mut app = App::new();
    assert_eq!(app.invoke(&call("game.update")), Outcome::Accepted);
    assert_eq!(app.screen(), "card");
    // The skin's button, activated again and again, is not a confirmation.
    app.draw();
    assert_eq!(app.invoke(&call("game.update")), Outcome::Ignored);
    assert_eq!(app.invoke(&call("game.update")), Outcome::Ignored);
    assert_eq!(app.screen(), "card");
    assert_eq!(app.runs(), 0);

    assert_eq!(app.confirm(), Outcome::Accepted);
    assert_eq!(app.ran(), ["game.update"]);
    assert_eq!(app.screen(), "none");
}

/// Whatever an action's usual kind, a plan with changes is shown before anything runs.
#[test]
fn s5_every_action_in_the_contract() {
    for spec in ACTIONS.iter().filter(|a| a.binding == Binding::Skin) {
        let mut app = App::new();
        app.invoke(&call(spec.name));
        match route(spec) {
            Route::Shell | Route::JobControl => assert_eq!(app.runs(), 0, "{}", spec.name),
            Route::Direct => assert_eq!(app.ran(), [spec.name]),
            Route::Plan | Route::Prompt => {
                if app.screen() == "prompt" {
                    let ticket = app.draw();
                    let press = app.press();
                    let answer = PromptAnswer::Text("x".into());
                    app.model.answer(ticket, answer, press);
                    app.model.pump();
                }
                assert_eq!(app.runs(), 0, "{} ran without its plan", spec.name);
                // `snapshot.set_auto` with `on = true` and the rest all show a card.
                assert_eq!(app.screen(), "card", "{}", spec.name);
            }
        }
    }
}

/// Adding an action to the contract needs a decision here.
#[test]
fn s5_every_action_is_classified() {
    let named = |r: Route| {
        ACTIONS
            .iter()
            .filter(|a| a.binding == Binding::Skin && route(a) == r)
            .map(|a| a.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        named(Route::Direct),
        [
            "library.refresh",
            "game.play",
            "game.verify",
            "game.open_folder",
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
        ]
    );
    // Nothing that asks for more than a plain run is run directly.
    for spec in ACTIONS.iter().filter(|a| route(a) == Route::Direct) {
        assert_eq!(spec.tier, Tier::Safe, "{}", spec.name);
        assert!(spec.escalation.is_none(), "{}", spec.name);
    }
}

#[test]
fn s5_a_download_turns_an_immediate_action_into_a_plan() {
    let mut app = App::new();
    app.fake().set_plan(
        "runner.set_runtime",
        FakePlan::with(PlanFacts::nothing_to_do()),
    );
    app.invoke(&call("runner.set_runtime"));
    assert_eq!(app.ran(), ["runner.set_runtime"], "no download: at once");

    let download = PlanFacts {
        needs_download: true,
        ..PlanFacts::changes()
    };
    app.fake()
        .set_plan("runner.set_runtime", FakePlan::with(download));
    app.invoke(&call("runner.set_runtime"));
    assert_eq!(app.screen(), "card");
    assert_eq!(app.runs(), 1);
}

#[test]
fn s5_an_empty_plan_runs_nothing() {
    let mut app = App::new();
    app.fake()
        .set_plan("game.update", FakePlan::with(PlanFacts::nothing_to_do()));
    app.invoke(&call("game.update"));
    assert_eq!(app.screen(), "none");
    assert_eq!(app.runs(), 0);
    assert!(app.model.take_effects().contains(&Effect::NothingToDo {
        action: "game.update"
    }));
}

#[test]
fn s5_confirm_needs_a_card_that_was_shown() {
    let mut app = App::new();
    app.invoke(&call("game.update"));
    let ticket = app.ticket();
    let press = app.press();
    assert_eq!(app.model.confirm(ticket, press), Outcome::Ignored);
    assert_eq!(app.runs(), 0);
}

#[test]
fn s5_confirm_cannot_come_from_the_input_that_opened_the_card() {
    let mut app = App::new();
    // A double click: both presses exist before the card is drawn.
    let first = app.press();
    let second = app.press();
    app.model.invoke(&call("game.update"), first.event);
    app.model.pump();
    let ticket = app.draw();
    // However late they are delivered.
    for press in [second, first] {
        let late = Press {
            at: app.now,
            ..press
        };
        assert_eq!(app.model.confirm(ticket, late), Outcome::Ignored);
    }
    assert_eq!(app.runs(), 0);
    assert_eq!(app.screen(), "card");
}

/// A double click, or Enter repeating: the first confirms, the plan turns out to have
/// changed, and the second lands on a card the user has not read.
#[test]
fn s5_a_second_click_cannot_confirm_the_changed_plan() {
    let mut app = App::new();
    app.invoke(&call("game.update"));
    app.draw();
    app.fake().set_plan(
        "game.update",
        FakePlan {
            inputs: 1,
            ..FakePlan::default()
        },
    );
    assert_eq!(app.confirm(), Outcome::Accepted);
    // The new card is drawn, and 80 ms later the second click goes down.
    let changed = app.ticket();
    let drawn = app.now;
    app.model.presented(changed, app.last_input(), drawn);
    app.now = drawn + Duration::from_millis(80);
    let second = app.press();
    assert_eq!(app.model.confirm(changed, second), Outcome::Ignored);
    let enter = app.press();
    assert_eq!(app.model.enter(enter), Outcome::Ignored);
    app.model.pump();
    assert_eq!(app.runs(), 0);
    assert_eq!(app.screen(), "card");
}

#[test]
fn s5_confirm_needs_the_card_to_have_been_up_for_a_while() {
    let mut app = App::new();
    app.invoke(&call("game.update"));
    let ticket = app.ticket();
    app.model.presented(ticket, app.last_input(), app.now);
    let shown = app.now;
    // A second report of the same screen doesn't restart the wait.
    app.model
        .presented(ticket, app.last_input(), shown + DWELL * 4);
    app.now = shown + DWELL - Duration::from_millis(1);
    let early = app.press();
    assert_eq!(app.model.confirm(ticket, early), Outcome::Ignored);
    app.now = shown + DWELL;
    let press = app.press();
    assert_eq!(app.model.confirm(ticket, press), Outcome::Accepted);
    app.model.pump();
    assert_eq!(app.runs(), 1);
}

#[test]
fn s5_a_repeating_key_never_confirms() {
    let mut app = App::new();
    app.invoke(&call("game.update"));
    app.draw();
    app.now += Duration::from_secs(10);
    let repeat = Press {
        repeat: true,
        ..app.press()
    };
    assert_eq!(app.model.enter(repeat), Outcome::Ignored);
    assert_eq!(app.runs(), 0);
}

/// A press that existed before the card stays useless even if the shell reports an older
/// number than the model has already taken.
#[test]
fn s5_an_older_press_cannot_confirm_whatever_the_shell_reports() {
    let mut app = App::new();
    let before = app.press();
    app.invoke(&call("game.update"));
    let ticket = app.ticket();
    app.model.presented(ticket, InputEvent(0), app.now);
    app.now += DWELL;
    let late = Press {
        at: app.now,
        ..before
    };
    assert_eq!(app.model.confirm(ticket, late), Outcome::Ignored);
    assert_eq!(app.runs(), 0);
}

#[test]
fn s5_previews_that_do_not_fit_are_not_planned_over_and_over() {
    let mut app = App::new();
    let card = |n: u32| {
        Invocation::new(actions::action("game.update").unwrap())
            .arg("id", Value::str(n.to_string()))
    };
    // Each frame the page asks for all twenty of its cards.
    let frame = |app: &mut App| {
        let held = (0..20)
            .filter(|n| app.model.preview(&card(*n)).is_ok())
            .count();
        app.model.pump();
        held
    };
    for _ in 0..8 {
        frame(&mut app);
    }
    let planned = app.fake().planned.len();
    assert_eq!(planned, 16, "as many as are kept, each once");
    for _ in 0..8 {
        assert_eq!(frame(&mut app), 16);
    }
    assert_eq!(app.fake().planned.len(), planned);

    // Once the page is gone its previews make room.
    app.model.pump();
    app.model.pump();
    assert!(app.model.preview(&card(100)).is_ok());
}

#[test]
fn s5_only_a_few_previews_wait_for_a_plan_at_once() {
    let mut app = App::new();
    app.fake().hold_plans = true;
    let results: Vec<bool> = (0..6)
        .map(|n| {
            let i = Invocation::new(actions::action("game.update").unwrap())
                .arg("id", Value::str(n.to_string()));
            app.model.preview(&i).is_ok()
        })
        .collect();
    assert_eq!(results, [true, true, true, true, false, false]);
    assert_eq!(app.fake().planned.len(), 4);
}

#[test]
fn s5_confirm_cannot_be_replayed() {
    let mut app = App::new();
    app.invoke(&call("game.update"));
    let ticket = app.draw();
    let press = app.press();
    assert_eq!(app.model.confirm(ticket, press), Outcome::Accepted);
    // Again while the plan is checked, and again after it ran.
    assert_eq!(app.model.confirm(ticket, press), Outcome::Ignored);
    let later = app.press();
    assert_eq!(app.model.confirm(ticket, later), Outcome::Ignored);
    app.model.pump();
    let latest = app.press();
    assert_eq!(app.model.confirm(ticket, latest), Outcome::Ignored);
    assert_eq!(app.runs(), 1);
}

#[test]
fn s5_confirm_answers_only_the_plan_it_was_shown_for() {
    let mut app = App::new();
    app.invoke(&call("game.update"));
    let old = app.draw();
    app.model.cancel(old);

    app.invoke(&call("game.move"));
    let new = app.draw();
    assert_ne!(old, new);
    let press = app.press();
    assert_eq!(app.model.confirm(old, press), Outcome::Ignored);
    assert_eq!(app.runs(), 0);
    assert_eq!(app.screen(), "card");
}

#[test]
fn s5_a_plan_that_changed_is_shown_again_not_run() {
    type Change = fn(&mut FakePlan);
    let changes: [Change; 5] = [
        |p| p.inputs += 1,
        |p| p.plan.title = "Update to 2.0?".into(),
        |p| p.plan.download = Some(1),
        |p| p.plan.warnings.push("Saves may not load".into()),
        |p| p.facts.needs_download = true,
    ];
    for change in changes {
        let mut app = App::new();
        app.invoke(&call("game.update"));
        let old = app.draw();
        let mut plan = FakePlan::default();
        change(&mut plan);
        app.fake().set_plan("game.update", plan);

        assert_eq!(app.confirm(), Outcome::Accepted);
        assert_eq!(app.runs(), 0);
        assert_eq!(app.screen(), "card");
        assert_ne!(app.ticket(), old);
        assert!(app.model.take_effects().contains(&Effect::PlanChanged {
            action: "game.update"
        }));
        // The new card needs its own confirmation, and then runs the newest plan.
        assert_eq!(app.confirm(), Outcome::Accepted);
        assert_eq!(app.runs(), 1);
        assert_eq!(app.fake().runs[0].plan, Some(3));
    }
}

#[test]
fn s5_a_plan_that_became_empty_runs_nothing() {
    let mut app = App::new();
    app.invoke(&call("game.update"));
    app.draw();
    app.fake()
        .set_plan("game.update", FakePlan::with(PlanFacts::nothing_to_do()));
    app.confirm();
    assert_eq!(app.runs(), 0);
    assert_eq!(app.screen(), "none");
}

#[test]
fn s5_the_confirmed_plan_is_the_one_that_runs() {
    let mut app = App::new();
    app.invoke(&call("game.update"));
    app.confirm();
    // Planned once for the card and once to check it: the check's plan runs.
    assert_eq!(app.fake().planned, ["game.update", "game.update"]);
    assert_eq!(app.fake().runs[0].plan, Some(2));
}

#[test]
fn s5_inline_preview_needs_the_same_confirm() {
    let mut app = App::new();
    let ticket = app.model.preview(&call("game.update")).unwrap();
    app.model.pump();
    assert!(matches!(
        app.model.preview_state(ticket),
        Some(Preview::Ready { .. })
    ));
    // Not before it was drawn, and not by invoking the action.
    let early = app.press();
    assert_eq!(app.model.confirm(ticket, early), Outcome::Ignored);
    assert_eq!(app.runs(), 0);

    app.show(ticket);
    assert_eq!(app.model.confirm(ticket, early), Outcome::Ignored);
    let press = app.press();
    assert_eq!(app.model.confirm(ticket, press), Outcome::Accepted);
    app.model.pump();
    assert_eq!(app.ran(), ["game.update"]);
    assert!(app.model.preview_state(ticket).is_none(), "used up");
}

#[test]
fn s5_a_stale_preview_cannot_be_confirmed() {
    let mut app = App::new();
    let ticket = app.model.preview(&call("game.update")).unwrap();
    app.model.pump();
    app.show(ticket);
    app.fake().set_plan(
        "game.update",
        FakePlan {
            inputs: 9,
            ..FakePlan::default()
        },
    );
    let press = app.press();
    app.model.confirm(ticket, press);
    app.model.pump();
    assert_eq!(app.runs(), 0);
    assert_eq!(app.screen(), "card");
}

#[test]
fn s5_a_prompt_action_with_changes_still_shows_them() {
    let mut app = App::new();
    app.invoke(&call("env.edit"));
    let ticket = app.draw();
    let press = app.press();
    app.model
        .answer(ticket, PromptAnswer::Text("1".into()), press);
    app.model.pump();
    assert_eq!(app.screen(), "card");
    assert_eq!(app.runs(), 0);
}

/// The prompt-to-run edge: nothing to show, so the answer is enough.
#[test]
fn s5_a_prompt_action_without_changes_runs_after_the_answer() {
    let mut app = App::new();
    app.fake().set_plan(
        "account.sign_in",
        FakePlan::with(PlanFacts::nothing_to_do()),
    );
    app.fake().prompts.insert(
        "account.sign_in",
        PromptSpec::Secret {
            label: "Code".into(),
        },
    );
    app.invoke(&call("account.sign_in"));
    let ticket = app.draw();
    let press = app.press();
    let plain = PromptAnswer::Text("code".into());
    assert_eq!(app.model.answer(ticket, plain, press), Outcome::Ignored);
    let press = app.press();
    let secret = PromptAnswer::Secret(Secret::new("a-code"));
    assert_eq!(
        app.model.answer(ticket, secret.clone(), press),
        Outcome::Accepted
    );
    app.model.pump();
    assert_eq!(app.fake().runs[0].answer, Some(secret));
    assert!(!format!("{:?}", app.fake().runs).contains("a-code"));
}

// --- S6: destructive actions need strong confirmation ---

/// Take `name` to its strong confirmation with `plan` scripted.
fn to_strong(name: &str, plan: FakePlan) -> App {
    let mut app = App::new();
    app.fake()
        .set_plan(actions::action(name).unwrap().name, plan);
    let mut invocation = call(name);
    if name == "account.sign_out" {
        invocation =
            Invocation::new(actions::action(name).unwrap()).arg("purge_state", Value::Bool(true));
    }
    app.invoke(&invocation);
    assert_eq!(app.screen(), "card", "{name}");
    assert_eq!(app.confirm(), Outcome::Accepted, "{name}");
    assert_eq!(app.screen(), "strong", "{name}");
    assert_eq!(app.runs(), 0, "{name}: one confirmation is not enough");
    app
}

fn s6_typed(name: &str, plan: FakePlan) {
    let mut app = to_strong(name, plan);
    assert_eq!(app.strength(), Strength::Typed("Example Game".into()));
    let ticket = app.draw();
    // The default is no: neither Enter nor a plain confirm does anything.
    let press = app.press();
    assert_eq!(app.model.enter(press), Outcome::Ignored);
    let press = app.press();
    assert_eq!(app.model.confirm(ticket, press), Outcome::Ignored);
    // Nor does a hold, or a wrong name.
    let press = app.press();
    app.model.hold_start(ticket, press);
    app.model.tick(press.at + Duration::from_secs(5));
    for wrong in ["", "example game", "Example Gam", "Example Game 2"] {
        let press = app.press();
        assert_eq!(app.model.type_name(ticket, wrong, press), Outcome::Ignored);
    }
    app.model.pump();
    assert_eq!(app.runs(), 0, "{name}");
    assert_eq!(app.screen(), "strong", "{name}");

    let press = app.press();
    assert_eq!(
        app.model.type_name(ticket, "  Example Game ", press),
        Outcome::Accepted
    );
    app.model.pump();
    assert_eq!(app.ran(), [name]);
}

fn s6_hold(name: &str, plan: FakePlan) {
    let mut app = to_strong(name, plan);
    assert_eq!(app.strength(), Strength::Hold, "{name}");
    let ticket = app.draw();
    let press = app.press();
    assert_eq!(app.model.enter(press), Outcome::Ignored);
    let press = app.press();
    assert_eq!(
        app.model.type_name(ticket, "Example Game", press),
        Outcome::Ignored
    );
    let press = app.press();
    app.model.hold_start(ticket, press);
    app.model.tick(press.at + HOLD);
    app.model.pump();
    assert_eq!(app.ran(), [name]);
}

#[test]
fn s6_game_remove() {
    s6_typed("game.remove", FakePlan::default());
}

#[test]
fn s6_dlc_remove() {
    s6_typed("dlc.remove", FakePlan::default());
}

#[test]
fn s6_snapshot_delete() {
    s6_typed("snapshot.delete", FakePlan::default());
}

#[test]
fn s6_snapshot_rollback() {
    s6_typed("snapshot.rollback", FakePlan::default());
}

#[test]
fn s6_game_update_that_removes_dlc() {
    s6_typed("game.update", removes_dlc());
}

#[test]
fn s6_game_update_that_downgrades() {
    let downgrade = PlanFacts {
        downgrades: true,
        ..PlanFacts::changes()
    };
    s6_typed("game.update", FakePlan::with(downgrade));
}

#[test]
fn s6_game_move_across_filesystems() {
    let across = PlanFacts {
        crosses_filesystems: true,
        ..PlanFacts::changes()
    };
    s6_typed("game.move", FakePlan::with(across));
}

#[test]
fn s6_apps_prune_orphaned() {
    s6_hold("apps.prune_orphaned", FakePlan::default());
}

/// The backend says nothing about the purge: the argument alone makes it destructive.
#[test]
fn s6_sign_out_with_purge_state() {
    s6_hold("account.sign_out", FakePlan::default());
}

#[test]
fn s6_apply_all_that_removes_dlc() {
    s6_hold("updates.apply_all", removes_dlc());
}

#[test]
fn s6_apply_all_that_downgrades() {
    let downgrade = PlanFacts {
        downgrades: true,
        ..PlanFacts::changes()
    };
    s6_hold("updates.apply_all", FakePlan::with(downgrade));
}

#[test]
fn s6_without_a_title_it_asks_for_a_hold() {
    let mut plan = FakePlan::default();
    plan.plan.subject = "  ".into();
    s6_hold("game.remove", plan);
}

/// Every action that is always destructive has a test above.
#[test]
fn s6_covers_every_destructive_action() {
    let destructive: Vec<_> = ACTIONS
        .iter()
        .filter(|a| a.tier == Tier::Destructive)
        .map(|a| a.name)
        .collect();
    assert_eq!(
        destructive,
        [
            "game.remove",
            "apps.prune_orphaned",
            "dlc.remove",
            "snapshot.delete",
            "snapshot.rollback",
        ]
    );
}

#[test]
fn s6_a_short_or_broken_hold_does_nothing() {
    let mut app = to_strong("apps.prune_orphaned", FakePlan::default());
    let ticket = app.draw();
    let start = app.now;
    let ms = |n| start + Duration::from_millis(n);
    let hold = |app: &mut App, at| {
        app.now = at;
        let press = app.press();
        app.model.hold_start(ticket, press);
        press
    };

    // Released early.
    hold(&mut app, ms(0));
    app.model.tick(ms(999));
    app.model.hold_stop(ticket);
    app.model.tick(ms(5_000));
    // A clock that goes back.
    let used = hold(&mut app, ms(10_000));
    app.model.tick(ms(9_000));
    app.model.tick(ms(20_000));
    // A used input can't start one, nor can a repeating key.
    app.model.hold_start(
        ticket,
        Press {
            at: ms(30_000),
            ..used
        },
    );
    app.now = ms(31_000);
    let repeat = Press {
        repeat: true,
        ..app.press()
    };
    app.model.hold_start(ticket, repeat);
    app.model.tick(ms(40_000));
    app.model.pump();
    assert_eq!(app.runs(), 0);

    // A new press starts from its own time.
    hold(&mut app, ms(50_000));
    app.model.tick(ms(50_500));
    app.model.pump();
    assert_eq!(app.runs(), 0);
    app.model.tick(ms(51_000));
    app.model.pump();
    assert_eq!(app.runs(), 1);
}

#[test]
fn s6_the_second_screen_cannot_be_passed_by_the_first_inputs() {
    let mut app = App::new();
    app.invoke(&call("game.remove"));
    let card = app.draw();
    let confirm = app.press();
    let queued = app.press();
    app.model.confirm(card, confirm);
    let strong = app.draw();
    let late = Press {
        at: app.now,
        ..queued
    };
    assert_eq!(
        app.model.type_name(strong, "Example Game", late),
        Outcome::Ignored
    );
    assert_eq!(app.runs(), 0);
}

#[test]
fn s6_a_plan_that_became_destructive_asks_again() {
    let mut app = App::new();
    app.invoke(&call("game.update"));
    app.draw();
    app.fake().set_plan("game.update", removes_dlc());
    app.confirm();
    assert_eq!(app.screen(), "card");
    assert!(matches!(
        app.model.modal(),
        Some(Modal::PlanCard {
            tier: Tier::Destructive,
            ..
        })
    ));
    app.confirm();
    assert_eq!(app.screen(), "strong");
    assert_eq!(app.runs(), 0);
}

/// A strong confirmation covers one run: it is not replayed by resume or retry.
#[test]
fn s6_a_destructive_job_cannot_be_restarted() {
    for (state, control) in [
        (JobState::Failed, "job.retry"),
        (JobState::Cancelled, "job.resume"),
    ] {
        let mut app = to_strong("dlc.remove", FakePlan::default());
        let ticket = app.draw();
        let press = app.press();
        app.model.type_name(ticket, "Example Game", press);
        app.model.pump();
        app.fake().sink(0).report(status(state));
        app.model.pump();
        app.model.dismiss();
        let id = app.model.jobs().ids().next().unwrap().as_str().to_owned();
        let outcome = app.invoke(&job_call(control, &id));
        assert!(matches!(outcome, Outcome::Refused(_)), "{control}");
        assert!(app.fake().controls.lock().unwrap().is_empty(), "{control}");
    }
}

#[test]
fn s6_a_paused_destructive_job_can_carry_on() {
    let mut app = to_strong("dlc.remove", FakePlan::default());
    let ticket = app.draw();
    let press = app.press();
    app.model.type_name(ticket, "Example Game", press);
    app.model.pump();
    app.fake().sink(0).report(status(JobState::Paused));
    app.model.pump();
    let id = app.model.jobs().ids().next().unwrap().as_str().to_owned();
    assert_eq!(app.invoke(&job_call("job.resume", &id)), Outcome::Accepted);
}

// --- S8: cancel is always there ---

#[test]
fn s8_cancel_from_every_state_runs_nothing() {
    // Planning.
    let mut app = App::new();
    app.fake().hold_plans = true;
    app.invoke(&call("game.update"));
    assert_eq!(app.screen(), "planning");
    app.model.cancel(app.ticket());
    assert!(app.fake().held[0].1.is_abandoned());
    app.fake().reply_next();
    app.model.pump();
    assert_eq!((app.screen(), app.runs()), ("none", 0), "late plan ignored");

    // Prompt.
    let mut app = App::new();
    app.invoke(&call("env.edit"));
    assert_eq!(app.screen(), "prompt");
    app.model.cancel(app.ticket());
    app.model.pump();
    assert_eq!((app.screen(), app.runs()), ("none", 0));
    assert!(app.fake().planned.is_empty());

    // Plan card.
    let mut app = App::new();
    app.invoke(&call("game.update"));
    app.model.cancel(app.ticket());
    app.model.pump();
    assert_eq!((app.screen(), app.runs()), ("none", 0));

    // Strong confirmation, also mid-hold.
    let mut app = to_strong("apps.prune_orphaned", FakePlan::default());
    let ticket = app.draw();
    let press = app.press();
    app.model.hold_start(ticket, press);
    app.model.cancel(ticket);
    app.model.tick(press.at + Duration::from_secs(5));
    app.model.pump();
    assert_eq!((app.screen(), app.runs()), ("none", 0));

    // The check after confirming.
    let mut app = App::new();
    app.invoke(&call("game.update"));
    app.fake().hold_plans = true;
    app.confirm();
    assert!(matches!(
        app.model.modal(),
        Some(Modal::Planning {
            confirming: true,
            ..
        })
    ));
    app.model.cancel(app.ticket());
    app.fake().reply_next();
    app.model.pump();
    assert_eq!((app.screen(), app.runs()), ("none", 0));

    // An inline preview.
    let mut app = App::new();
    let ticket = app.model.preview(&call("game.update")).unwrap();
    app.model.pump();
    app.show(ticket);
    app.model.cancel(ticket);
    let press = app.press();
    assert_eq!(app.model.confirm(ticket, press), Outcome::Ignored);
    assert_eq!(app.runs(), 0);
}

#[test]
fn s8_a_no_to_an_actions_question_cancels() {
    let mut app = App::new();
    app.fake().prompts.insert(
        "env.edit",
        PromptSpec::Confirm {
            label: "Sure?".into(),
        },
    );
    app.invoke(&call("env.edit"));
    let ticket = app.draw();
    let press = app.press();
    app.model.answer(ticket, PromptAnswer::No, press);
    app.model.pump();
    assert_eq!((app.screen(), app.runs()), ("none", 0));
    assert!(app.fake().planned.is_empty());
}

#[test]
fn s8_cancelling_a_jobs_question_answers_none() {
    let (mut app, _) = with_job();
    let answers = Answers::default();
    ask(&mut app, &answers);
    assert_eq!(app.screen(), "prompt");
    assert_eq!(app.model.dismiss(), Outcome::Accepted);
    assert_eq!(*answers.lock().unwrap(), [None]);
    assert_eq!(app.screen(), "none");
}

#[test]
fn s8_a_jobs_question_needs_a_shown_screen_and_a_new_input() {
    let (mut app, _) = with_job();
    let answers = Answers::default();
    let queued = app.press();
    ask(&mut app, &answers);
    let ticket = app.ticket();
    let press = app.press();
    assert_eq!(
        app.model.answer(ticket, PromptAnswer::Yes, press),
        Outcome::Ignored
    );
    app.show(ticket);
    let late = Press {
        at: app.now,
        ..queued
    };
    assert_eq!(
        app.model.answer(ticket, PromptAnswer::Yes, late),
        Outcome::Ignored
    );
    assert!(answers.lock().unwrap().is_empty());
    let press = app.press();
    app.model.answer(ticket, PromptAnswer::Yes, press);
    assert_eq!(*answers.lock().unwrap(), [Some(PromptAnswer::Yes)]);
}

#[test]
fn s8_a_job_can_be_cancelled_behind_a_dialog() {
    let (mut app, id) = with_job();
    app.invoke(&call("game.update"));
    assert_eq!(app.screen(), "card");
    assert_eq!(app.invoke(&job_call("job.cancel", &id)), Outcome::Accepted);
    assert_eq!(
        app.fake().controls.lock().unwrap().as_slice(),
        [(0, "cancel")]
    );
    assert_eq!(app.screen(), "card", "the card is still there");
}

#[test]
fn s8_cancelling_a_job_answers_its_waiting_questions() {
    let (mut app, id) = with_job();
    // One on screen, one waiting behind it.
    let answers = Answers::default();
    ask(&mut app, &answers);
    ask(&mut app, &answers);
    app.invoke(&job_call("job.cancel", &id));
    assert_eq!(*answers.lock().unwrap(), [None, None]);
    assert_eq!(app.screen(), "none");
}

#[test]
fn s8_dropping_the_model_cancels_every_question() {
    let (mut app, _) = with_job();
    let answers = Answers::default();
    ask(&mut app, &answers);
    drop(app);
    assert_eq!(*answers.lock().unwrap(), [None]);
}

#[test]
fn s8_a_question_waits_for_the_screen_to_be_free() {
    let (mut app, _) = with_job();
    app.invoke(&call("game.update"));
    let card = app.ticket();
    let answers = Answers::default();
    ask(&mut app, &answers);
    assert_eq!(app.ticket(), card, "never replaces what is on screen");
    app.model.cancel(card);
    assert_eq!(app.screen(), "prompt");
    assert!(answers.lock().unwrap().is_empty());
}

// --- S9: errors are uniform ---

fn error(app: &App) -> ErrorView {
    match app.model.modal() {
        Some(Modal::Error { error, .. }) => error.clone(),
        other => panic!("no error: {other:?}"),
    }
}

fn disk() -> ErrorView {
    ErrorView::new("Disk", "No room", "the disk is full", "Free space", "6")
}

#[test]
fn s9_a_refused_plan_shows_the_error() {
    let mut app = App::new();
    app.fake().plans.insert("game.download", Err(disk()));
    app.invoke(&call("game.download"));
    assert_eq!(error(&app), disk());
    assert_eq!(app.runs(), 0);
}

#[test]
fn s9_a_plan_that_never_comes_shows_an_error() {
    let mut app = App::new();
    app.fake().hold_plans = true;
    app.invoke(&call("game.update"));
    app.fake().held.clear();
    app.model.pump();
    assert_eq!(error(&app).title, "Planning failed");
}

#[test]
fn s9_a_refused_run_shows_the_error_and_leaves_no_job() {
    let mut app = App::new();
    app.fake().run_error = Some(disk());
    assert_eq!(app.invoke(&call("game.play")), Outcome::Refused(disk()));
    assert_eq!(error(&app), disk());
    assert_eq!(app.model.jobs().ids().count(), 0);

    app.model.cancel(app.ticket());
    app.invoke(&call("game.update"));
    app.confirm();
    assert_eq!(error(&app), disk());
}

#[test]
fn s9_a_failed_job_shows_its_error() {
    for listed in ["game.verify", "game.play"] {
        let mut app = App::new();
        app.invoke(&call(listed));
        app.fake().sink(0).report(JobStatus {
            error: Some(disk()),
            ..status(JobState::Failed)
        });
        app.model.pump();
        assert_eq!(error(&app), disk(), "{listed}");
    }
}

#[test]
fn s9_a_refused_invocation_shows_an_error() {
    let mut app = App::new();
    let Outcome::Refused(e) = app.invoke(&call("env.reveal")) else {
        panic!("env.reveal is not for skins");
    };
    assert_eq!(error(&app), e);
    assert_eq!(app.runs(), 0);
}

#[test]
fn s9_errors_stay_until_the_user_dismisses_each() {
    let mut app = App::new();
    app.fake().run_error = Some(disk());
    let (a, b) = (app.input(), app.input());
    app.model.invoke(&call("game.play"), a);
    // Behind an error nothing else is taken.
    assert_eq!(app.model.invoke(&call("game.play"), b), Outcome::Ignored);
    app.model.pump();
    app.model.tick(Duration::from_secs(3_600));
    assert_eq!(app.screen(), "error");
    app.model.dismiss();
    assert_eq!(app.screen(), "none");
}

#[test]
fn s9_a_failure_behind_a_dialog_is_shown_after_it() {
    let (mut app, _) = with_job();
    app.invoke(&call("game.update"));
    app.fake().sink(0).report(JobStatus {
        error: Some(disk()),
        ..status(JobState::Failed)
    });
    app.model.pump();
    assert_eq!(app.screen(), "card");
    app.model.dismiss();
    assert_eq!(error(&app), disk());
}

/// A restart asked for before the frame that reports the end must not swallow it.
#[test]
fn s9_a_restart_cannot_hide_an_end_not_yet_reported() {
    for (end, control) in [
        (JobState::Failed, "job.retry"),
        (JobState::Cancelled, "job.resume"),
    ] {
        let (mut app, id) = with_job();
        app.fake().sink(0).report(JobStatus {
            error: Some(disk()),
            ..status(end)
        });
        // No frame in between.
        let event = app.input();
        let outcome = app.model.invoke(&job_call(control, &id), event);
        assert!(matches!(outcome, Outcome::Refused(_)), "{control}");
        assert!(app.fake().controls.lock().unwrap().is_empty(), "{control}");
        app.model.pump();
        assert_eq!(
            app.model.jobs().state(&JobId::new(&id)),
            Some(end),
            "{control}"
        );
        if end == JobState::Failed {
            app.model.dismiss();
            assert_eq!(error(&app), disk(), "the failure is still told");
        }
    }
}

#[test]
fn s9_a_skin_cannot_pile_up_errors() {
    let bad = || Invocation::new(actions::action("game.play").unwrap());
    // Behind a dialog nothing is even checked.
    let mut app = App::new();
    app.invoke(&call("game.update"));
    for _ in 0..50 {
        assert_eq!(app.invoke(&bad()), Outcome::Ignored);
        assert_eq!(app.invoke(&call("env.reveal")), Outcome::Ignored);
    }
    app.model.dismiss();
    assert_eq!(app.screen(), "none");

    // The same refusal over and over, even from the actions allowed behind a dialog,
    // is one error.
    app.invoke(&call("game.update"));
    for _ in 0..50 {
        app.invoke(&job_call("job.cancel", "no-such-job"));
    }
    app.model.dismiss();
    assert_eq!(app.screen(), "error");
    app.model.dismiss();
    assert_eq!(app.screen(), "none");
}

// --- S19: global keys are the app's ---

/// The model's half: Esc and Enter act on the app's own state, whatever a skin bound.
/// Routing the keys before the skin sees them is the shell's.
#[test]
fn s19_esc_and_enter_reach_the_dialog() {
    let mut app = App::new();
    assert_eq!(app.model.dismiss(), Outcome::Ignored);
    let press = app.press();
    assert_eq!(app.model.enter(press), Outcome::Ignored);

    app.invoke(&call("game.update"));
    assert_eq!(app.model.dismiss(), Outcome::Accepted);
    assert_eq!(app.screen(), "none");

    app.invoke(&call("game.update"));
    app.draw();
    let press = app.press();
    assert_eq!(app.model.enter(press), Outcome::Accepted);
    app.model.pump();
    assert_eq!(app.ran(), ["game.update"]);
}

#[test]
fn s19_enter_on_a_destructive_card_only_moves_on() {
    let mut app = App::new();
    app.invoke(&call("game.remove"));
    app.draw();
    let press = app.press();
    app.model.enter(press);
    assert_eq!(app.screen(), "strong");
    app.draw();
    let press = app.press();
    assert_eq!(app.model.enter(press), Outcome::Ignored);
    assert_eq!(app.runs(), 0);
}

// --- S21: skins can't weaken the safety flow through settings ---

fn set(key: &str, value: &str) -> Invocation {
    call_with("settings.set", &[("key", key), ("value", value)])
}

fn call_with(name: &str, args: &[(&str, &str)]) -> Invocation {
    args.iter().fold(
        Invocation::new(actions::action(name).unwrap()),
        |i, (n, v)| i.arg(n, Value::str(*v)),
    )
}

#[test]
fn s21_a_skin_sets_only_listed_settings() {
    for key in [
        "cloud_saves.writes",
        "auth.token_store",
        "download.workers",
        "gui.mode ",
        "",
    ] {
        let mut app = App::new();
        let outcome = app.invoke(&set(key, "true"));
        assert!(matches!(outcome, Outcome::Refused(_)), "{key}");
        assert_eq!(app.runs(), 0, "{key}");
    }
    let mut app = App::new();
    assert_eq!(app.invoke(&set("gui.mode", "dark")), Outcome::Accepted);
    assert_eq!(app.ran(), ["settings.set"]);
}

#[test]
fn s21_a_closed_gate_blocks_before_planning() {
    for name in ["saves.push", "saves.sync"] {
        let mut app = App::new();
        app.fake().gates.cloud_writes = false;
        assert!(matches!(app.invoke(&call(name)), Outcome::Refused(_)));
        assert!(app.model.preview(&call(name)).is_err());
        assert!(app.fake().planned.is_empty());
        assert_eq!(app.runs(), 0);
    }
}

#[test]
fn s21_a_gate_closed_while_the_card_is_up_blocks_the_run() {
    let mut app = App::new();
    app.invoke(&call("saves.push"));
    app.draw();
    app.fake().gates.cloud_writes = false;
    assert!(matches!(app.confirm(), Outcome::Refused(_)));
    assert_eq!(app.runs(), 0);
    assert_eq!(app.screen(), "error");
}

#[test]
fn s21_a_gate_closed_during_the_check_blocks_the_run() {
    let mut app = App::new();
    app.invoke(&call("saves.push"));
    app.fake().hold_plans = true;
    app.confirm();
    app.fake().gates.cloud_writes = false;
    app.fake().reply_next();
    app.model.pump();
    assert_eq!(app.runs(), 0);
    assert_eq!(app.screen(), "error");
}

#[test]
fn s21_a_closed_gate_blocks_retry_and_resume() {
    for (state, control) in [
        (JobState::Failed, "job.retry"),
        (JobState::Cancelled, "job.resume"),
    ] {
        let mut app = App::new();
        app.invoke(&call("saves.push"));
        app.confirm();
        app.fake().sink(0).report(status(state));
        app.model.pump();
        app.model.dismiss();
        let id = app.model.jobs().ids().next().unwrap().as_str().to_owned();
        app.fake().gates.cloud_writes = false;
        let outcome = app.invoke(&job_call(control, &id));
        assert!(matches!(outcome, Outcome::Refused(_)), "{control}");
        assert!(app.fake().controls.lock().unwrap().is_empty(), "{control}");
    }
}

#[test]
fn s5_a_plan_that_differs_only_in_free_space_is_still_the_plan() {
    let mut app = App::new();
    app.invoke(&call("game.update"));
    app.draw();
    let mut plan = FakePlan::default();
    plan.plan.free = Some(7);
    app.fake().set_plan("game.update", plan);
    assert_eq!(app.confirm(), Outcome::Accepted);
    assert_eq!(app.runs(), 1);
}

/// The job of [`with_job`] pauses, is resumed and asks to have `plan` agreed to.
fn resumed_with_a_new_plan(app: &mut App, id: &str, answers: &Answers) {
    app.fake().sink(0).report(status(JobState::Paused));
    app.model.pump();
    assert_eq!(app.invoke(&job_call("job.resume", id)), Outcome::Accepted);
    let answers = Arc::clone(answers);
    app.fake().sink(0).ask(
        PromptSpec::Plan(Box::new(FakePlan::default().plan)),
        PromptReply::new(move |a| answers.lock().unwrap().push(a)),
    );
    app.model.pump();
}

#[test]
fn s5_a_job_with_a_new_plan_waits_for_the_plan_card() {
    let (mut app, id) = with_job();
    let answers = Answers::default();
    resumed_with_a_new_plan(&mut app, &id, &answers);
    assert!(app.model.take_effects().contains(&Effect::PlanChanged {
        action: "game.verify"
    }));
    let Some(Modal::PlanCard { ticket, action, .. }) = app.model.modal() else {
        panic!("a plan card");
    };
    assert_eq!(action, "game.verify");

    // Not yet seen, and not answerable as a question.
    let early = app.press();
    assert_eq!(app.model.confirm(ticket, early), Outcome::Ignored);
    app.show(ticket);
    let press = app.press();
    assert_eq!(
        app.model.answer(ticket, PromptAnswer::Yes, press),
        Outcome::Ignored
    );
    assert!(answers.lock().unwrap().is_empty());

    assert_eq!(app.confirm(), Outcome::Accepted);
    assert_eq!(*answers.lock().unwrap(), [Some(PromptAnswer::Yes)]);
    assert_eq!(app.screen(), "none");
    assert_eq!(app.runs(), 1, "the job carries on; nothing runs anew");
}

#[test]
fn s8_cancelling_pausing_or_stopping_a_job_answers_its_plan_card_with_a_cancel() {
    for stop in ["dismiss", "job.pause", "job.cancel"] {
        let (mut app, id) = with_job();
        let answers = Answers::default();
        resumed_with_a_new_plan(&mut app, &id, &answers);
        assert_eq!(app.screen(), "card");
        if stop == "dismiss" {
            app.model.dismiss();
        } else {
            assert_eq!(app.invoke(&job_call(stop, &id)), Outcome::Accepted);
        }
        assert_eq!(app.screen(), "none", "{stop}");
        assert_eq!(*answers.lock().unwrap(), [None], "{stop}");
    }
}

#[test]
fn s21_a_closed_gate_blocks_the_confirm_of_a_jobs_plan_card() {
    let mut app = App::new();
    app.invoke(&call("saves.push"));
    app.confirm();
    app.fake().sink(0).report(status(JobState::Running));
    app.model.pump();
    let id = app.model.jobs().ids().next().unwrap().as_str().to_owned();
    let answers = Answers::default();
    resumed_with_a_new_plan(&mut app, &id, &answers);
    assert_eq!(app.screen(), "card");
    app.fake().gates.cloud_writes = false;
    assert!(matches!(app.confirm(), Outcome::Refused(_)));
    assert_eq!(*answers.lock().unwrap(), [None]);
    assert_eq!(app.screen(), "error");
}
