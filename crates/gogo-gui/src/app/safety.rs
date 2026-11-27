//! Every safety screen, over the fake engine, driven as the shell drives the app. Time is
//! given by hand: a press counts only once its screen has been up for the dwell.

use std::sync::{Arc, Mutex};

use gogo_contract::{PlanFacts, Value, actions};
use gogo_gui_model::fake::FakePlan;
use gogo_gui_model::{
    ConflictFile, ErrorView, Invocation, Modal, Plan, PlanStep, PromptAnswer, PromptReply,
    PromptSpec, Side, StepKind,
};
use gogo_ui_interact::{ImeInput, Key, Modifiers, PointerButton};
use gogo_ui_shell::accesskit::{Action, ActionData, ActionRequest, Role, Toggled, TreeId};

use super::tests::{SIZE, app, at, bounds};
use super::*;
use crate::dialog;

/// The smallest window the app asks for.
const SMALL: (f32, f32) = (640.0, 400.0);

type Heard = Arc<Mutex<Vec<Option<PromptAnswer>>>>;

/// A frame built for `size` and presented at `ms`, until nothing more is asked for.
fn see_at(app: &mut App, size: (f32, f32), ms: u64) {
    app.draw(size);
    if app.show(at(ms)) {
        app.draw(size);
        app.show(at(ms));
    }
}

fn see(app: &mut App, ms: u64) {
    see_at(app, SIZE, ms);
}

fn move_to(app: &mut App, (x, y): (f64, f64), ms: u64) {
    app.input(Input::PointerMoved { x, y }, at(ms));
}

fn down(app: &mut App, (x, y): (f64, f64), ms: u64) {
    let button = PointerButton::Primary;
    move_to(app, (x, y), ms);
    app.input(Input::PointerDown { button, x, y }, at(ms));
}

fn up(app: &mut App, (x, y): (f64, f64), ms: u64) {
    let button = PointerButton::Primary;
    app.input(Input::PointerUp { button, x, y }, at(ms));
}

/// A click on the button called `label`, then a frame.
fn click(app: &mut App, label: &str, ms: u64) {
    let point = bounds(app, Role::Button, label);
    down(app, point, ms);
    up(app, point, ms + 1);
    see(app, ms + 2);
}

fn key_down(app: &mut App, key: Key, ms: u64) {
    let input = Input::KeyDown {
        key,
        mods: Modifiers::default(),
        repeat: false,
    };
    app.input(input, at(ms));
}

fn tap(app: &mut App, key: Key, ms: u64) {
    key_down(app, key, ms);
    app.input(Input::KeyUp { key }, at(ms + 1));
    see(app, ms + 2);
}

fn escape(app: &mut App, ms: u64) {
    tap(app, Key::Escape, ms);
}

/// A screen reader asks for `action` on the node with `role` and `label`, then a frame.
fn assist(
    app: &mut App,
    role: Role,
    label: &str,
    action: Action,
    data: Option<ActionData>,
    ms: u64,
) {
    let tree = app.accessibility_tree();
    let (id, _) = tree
        .nodes
        .iter()
        .find(|(_, n)| n.role() == role && n.label() == Some(label))
        .unwrap_or_else(|| panic!("no {role:?} called {label}: {:?}", read(app)));
    let request = ActionRequest {
        action,
        target_tree: TreeId::ROOT,
        target_node: *id,
        data,
    };
    app.accessibility_action(request, at(ms));
    see(app, ms + 1);
}

/// Everything assistive technology is told.
fn read(app: &mut App) -> Vec<String> {
    app.accessibility_tree()
        .nodes
        .iter()
        .flat_map(|(_, n)| [n.label(), n.value()])
        .flatten()
        .map(str::to_owned)
        .collect()
}

fn heard(app: &mut App, text: &str) -> bool {
    read(app).iter().any(|line| line.contains(text))
}

fn has_role(app: &mut App, role: Role) -> bool {
    app.accessibility_tree()
        .nodes
        .iter()
        .any(|(_, n)| n.role() == role)
}

fn disabled(app: &mut App, label: &str) -> bool {
    app.accessibility_tree()
        .nodes
        .iter()
        .find(|(_, n)| n.role() == Role::Button && n.label() == Some(label))
        .unwrap_or_else(|| panic!("no button {label}"))
        .1
        .is_disabled()
}

fn focused(app: &mut App) -> Option<String> {
    app.interaction.focus().map(|t| t.key.clone())
}

fn screen(app: &App) -> &'static str {
    match app.data.modal() {
        None => "none",
        Some(Modal::Prompt { .. }) => "prompt",
        Some(Modal::Planning { .. }) => "planning",
        Some(Modal::PlanCard { .. }) => "card",
        Some(Modal::Strong { .. }) => "strong",
        Some(Modal::Error { .. }) => "error",
    }
}

fn runs(app: &mut App) -> usize {
    app.data.backend().runs.len()
}

/// A skin's control invokes `name` for a game, from a real press that lands on nothing,
/// and the frame after it is presented at `ms`.
fn start(app: &mut App, name: &str, extra: &[(&str, Value)], ms: u64) {
    let nowhere = (-10.0, -10.0);
    down(app, nowhere, ms);
    up(app, nowhere, ms);
    let spec = actions::action(name).expect(name);
    let mut invocation = Invocation::new(spec);
    for param in spec.params {
        if let Some((_, value)) = extra.iter().find(|(n, _)| *n == param.name) {
            invocation = invocation.arg(param.name, value.clone());
        } else if !param.optional {
            let value = match param.ty {
                actions::ParamType::Bool => Value::Bool(false),
                actions::ParamType::Index | actions::ParamType::Indices => Value::Num(1.0),
                _ => Value::str("1"),
            };
            invocation = invocation.arg(param.name, value);
        }
    }
    let event = act::number(app.interaction.latest());
    app.invoke(&invocation, event);
    see(app, ms);
}

fn script(app: &mut App, action: &'static str, plan: FakePlan) {
    app.data.backend_mut().set_plan(action, plan);
}

/// A destructive plan about "Example Game".
fn destructive(verb: &str) -> FakePlan {
    let mut plan = FakePlan::with(PlanFacts {
        removes_dlc: true,
        purge_state: true,
        ..PlanFacts::changes()
    });
    plan.plan.verb = verb.to_owned();
    plan.plan.title = format!("{verb} it?");
    plan.plan.reversible = false;
    plan
}

/// A running job asks `spec`; returns where its answers go.
fn job_asks(app: &mut App, spec: PromptSpec) -> Heard {
    start(app, "game.verify", &[], 10);
    assert_eq!(runs(app), 1);
    let answers = Heard::default();
    let sink = Arc::clone(&answers);
    app.data.backend().sink(0).ask(
        spec,
        PromptReply::new(move |answer| sink.lock().expect("not poisoned").push(answer)),
    );
    answers
}

fn answers(heard: &Heard) -> Vec<Option<PromptAnswer>> {
    heard.lock().expect("not poisoned").clone()
}

fn confirm_question() -> PromptSpec {
    PromptSpec::Confirm {
        label: "Replace the files?".to_owned(),
    }
}

#[test]
fn a_jobs_question_is_shown_and_answered_and_nothing_is_cancelled_unseen() {
    let mut app = app();
    let answered = job_asks(&mut app, confirm_question());
    // The wake alone asks for a frame: no source changed.
    assert!(app.wake());
    see(&mut app, 1000);
    assert!(heard(&mut app, "Replace the files?"));
    assert!(has_role(&mut app, Role::Dialog));
    assert!(!heard(&mut app, "Verify files"), "the page is not exposed");
    assert_eq!(focused(&mut app).as_deref(), Some(dialog::CANCEL));
    assert!(answers(&answered).is_empty());
    click(&mut app, "Yes", 1100);
    assert!(answers(&answered).is_empty(), "too soon");
    click(&mut app, "Yes", 1600);
    assert_eq!(answers(&answered), [Some(PromptAnswer::Yes)]);
    assert_eq!(screen(&app), "none");
}

#[test]
fn no_answers_no_and_cancel_answers_nothing() {
    for (button, expected) in [("No", Some(PromptAnswer::No)), ("Cancel", None)] {
        let mut app = app();
        let answered = job_asks(&mut app, confirm_question());
        app.wake();
        see(&mut app, 1000);
        click(&mut app, button, 1600);
        assert_eq!(answers(&answered), [expected], "{button}");
    }
}

#[test]
fn a_choice_a_text_and_a_secret_are_answered() {
    let mut app = app();
    let answered = job_asks(
        &mut app,
        PromptSpec::Choice {
            label: "Which build?".to_owned(),
            options: vec!["1.4".to_owned(), "1.5".to_owned()],
        },
    );
    app.wake();
    see(&mut app, 1000);
    click(&mut app, "1.5", 1600);
    assert_eq!(answers(&answered), [Some(PromptAnswer::Choice(1))]);

    let mut app = app_with_text_prompt();
    see(&mut app.0, 1000);
    assert!(heard(&mut app.0, "before"));
    assert_eq!(focused(&mut app.0).as_deref(), Some(dialog::CANCEL));
    click_field(&mut app.0, "Name", 1100);
    tap(&mut app.0, Key::Backspace, 1110);
    app.0.input(Input::Text(" Ünd".to_owned()), at(1120));
    see(&mut app.0, 1121);
    // Enter in the field does nothing.
    tap(&mut app.0, Key::Enter, 1600);
    assert!(answers(&app.1).is_empty());
    click(&mut app.0, "OK", 1700);
    assert_eq!(
        answers(&app.1),
        [Some(PromptAnswer::Text("befor Ünd".to_owned()))]
    );

    let mut app = super::tests::app();
    let answered = job_asks(
        &mut app,
        PromptSpec::Secret {
            label: "The key".to_owned(),
        },
    );
    app.wake();
    see(&mut app, 1000);
    assert!(!heard(&mut app, "Sign in"), "not the sign-in dialog");
    click(&mut app, "Paste", 1600);
    assert!(app.wants_paste());
    app.pasted(Some(Zeroizing::new("sesame".to_owned())));
    see(&mut app, 1610);
    assert!(!heard(&mut app, "sesame"));
    assert!(!app.dump().contains("sesame"));
    click(&mut app, "Send", 1700);
    assert_eq!(
        answers(&answered),
        [Some(PromptAnswer::Secret(gogo_gui_model::Secret::new(
            "sesame"
        )))]
    );
}

fn app_with_text_prompt() -> (App, Heard) {
    let mut app = app();
    let answered = job_asks(
        &mut app,
        PromptSpec::Text {
            label: "Name".to_owned(),
            default: "before".to_owned(),
        },
    );
    app.wake();
    (app, answered)
}

fn click_field(app: &mut App, name: &str, ms: u64) {
    let point = bounds(app, Role::TextInput, name);
    down(app, point, ms);
    up(app, point, ms + 1);
    see(app, ms + 2);
}

#[test]
fn conflicts_apply_only_once_every_file_is_decided() {
    let mut app = app();
    let file = |name: &str| ConflictFile {
        name: name.to_owned(),
        local: "yesterday".to_owned(),
        cloud: "today".to_owned(),
    };
    let answered = job_asks(
        &mut app,
        PromptSpec::Conflicts {
            files: vec![file("a.sav"), file("b.sav")],
        },
    );
    app.wake();
    see(&mut app, 1000);
    assert!(disabled(&mut app, "Apply"));
    click(&mut app, "Keep this computer's a.sav", 1600);
    assert!(disabled(&mut app, "Apply"));
    click(&mut app, "Keep the cloud's b.sav", 1700);
    let tree = app.accessibility_tree();
    let picked = tree
        .nodes
        .iter()
        .filter(|(_, n)| n.toggled() == Some(Toggled::True))
        .count();
    assert_eq!(picked, 2);
    click(&mut app, "Apply", 1800);
    assert_eq!(
        answers(&answered),
        [Some(PromptAnswer::Sides(vec![Side::Local, Side::Cloud]))]
    );
}

#[test]
fn a_plan_card_confirms_after_the_dwell_with_a_fresh_press() {
    let mut app = app();
    start(&mut app, "game.update", &[], 1000);
    assert_eq!(screen(&app), "card");
    assert!(has_role(&mut app, Role::Dialog));
    assert!(heard(&mut app, "Nothing changes until you confirm."));
    assert!(!heard(&mut app, "Verify files"), "the page is not exposed");
    // It starts on nothing, so that Enter confirms.
    assert_eq!(focused(&mut app), None);
    click(&mut app, "Do", 1100);
    assert_eq!(runs(&mut app), 0, "too soon");
    tap(&mut app, Key::Tab, 1110);
    tap(&mut app, Key::Tab, 1120);
    assert_eq!(screen(&app), "card");
    click(&mut app, "Do", 1600);
    assert_eq!(runs(&mut app), 1);
    assert_eq!(screen(&app), "none");
}

#[test]
fn enter_confirms_a_plan_card_with_nothing_focused() {
    let mut app = app();
    start(&mut app, "game.update", &[], 1000);
    // Down before the dwell, repeating after it: neither counts.
    key_down(&mut app, Key::Enter, 1100);
    app.input(
        Input::KeyDown {
            key: Key::Enter,
            mods: Modifiers::default(),
            repeat: true,
        },
        at(1700),
    );
    app.input(Input::KeyUp { key: Key::Enter }, at(1701));
    see(&mut app, 1702);
    assert_eq!(runs(&mut app), 0);
    tap(&mut app, Key::Enter, 1800);
    assert_eq!(runs(&mut app), 1);
}

#[test]
fn a_changed_plan_shows_the_card_again() {
    let mut app = app();
    start(&mut app, "game.update", &[], 1000);
    let first = app.data.modal_ticket();
    script(
        &mut app,
        "game.update",
        FakePlan {
            inputs: 1,
            ..FakePlan::default()
        },
    );
    click(&mut app, "Do", 1600);
    assert_eq!(runs(&mut app), 0);
    assert_eq!(screen(&app), "card");
    assert_ne!(app.data.modal_ticket(), first);
    assert!(heard(&mut app, "The plan changed."));
    click(&mut app, "Do", 1700);
    assert_eq!(runs(&mut app), 0, "the new card has its own dwell");
    click(&mut app, "Do", 2300);
    assert_eq!(runs(&mut app), 1);
}

#[test]
fn a_typed_confirmation_needs_the_exact_title() {
    let mut app = app();
    script(&mut app, "game.remove", destructive("Remove"));
    start(&mut app, "game.remove", &[], 1000);
    assert_eq!(screen(&app), "card");
    click(&mut app, "Remove", 1600);
    assert_eq!(screen(&app), "strong");
    assert!(has_role(&mut app, Role::AlertDialog));
    assert!(!heard(&mut app, "Verify files"));
    assert_eq!(focused(&mut app).as_deref(), Some(dialog::CANCEL));
    assert!(disabled(&mut app, "Remove"));
    let field = "Type the title: Example Game";
    click_field(&mut app, field, 1700);
    for (typed, ms) in [("example", 1710), (" game", 1720)] {
        app.input(Input::Text(typed.to_owned()), at(ms));
        see(&mut app, ms + 1);
    }
    assert!(disabled(&mut app, "Remove"), "the case counts");
    for _ in 0.."example game".len() {
        tap(&mut app, Key::Backspace, 1730);
    }
    app.input(Input::Text("Example Game".to_owned()), at(1800));
    see(&mut app, 1801);
    assert!(!disabled(&mut app, "Remove"));
    assert_eq!(runs(&mut app), 0, "typing answers nothing");
    click(&mut app, "Remove", 2400);
    assert_eq!(runs(&mut app), 1);
}

#[test]
fn the_title_can_be_pasted_or_set_by_assistive_technology() {
    let field = "Type the title: Example Game";
    for paste in [true, false] {
        let mut app = app();
        script(&mut app, "game.remove", destructive("Remove"));
        start(&mut app, "game.remove", &[], 1000);
        click(&mut app, "Remove", 1600);
        if paste {
            click_field(&mut app, field, 2300);
            let mods = Modifiers {
                ctrl: cfg!(not(target_os = "macos")),
                meta: cfg!(target_os = "macos"),
                ..Modifiers::default()
            };
            app.input(
                Input::KeyDown {
                    key: Key::Char('v'),
                    mods,
                    repeat: false,
                },
                at(2310),
            );
            assert!(app.wants_paste());
            app.pasted(Some(Zeroizing::new("Example Game\nand more".to_owned())));
            see(&mut app, 2320);
        } else {
            let value = ActionData::Value("Example Game".into());
            assist(
                &mut app,
                Role::TextInput,
                field,
                Action::SetValue,
                Some(value),
                2300,
            );
        }
        assert_eq!(runs(&mut app), 0, "neither is a press");
        click(&mut app, "Remove", 2900);
        assert_eq!(runs(&mut app), 1, "paste: {paste}");
    }
}

/// A destructive confirmation that takes a hold, presented at 1600.
fn hold_screen() -> App {
    let mut app = app();
    script(&mut app, "account.sign_out", destructive("Sign out"));
    start(
        &mut app,
        "account.sign_out",
        &[("purge_state", Value::Bool(true))],
        1000,
    );
    click(&mut app, "Sign out", 1600);
    assert_eq!(screen(&app), "strong");
    app
}

const HOLD_LABEL: &str = "Hold to Sign out";

/// Frames from `from` to `to`, as the app asks for them while a hold is down.
fn frames(app: &mut App, from: u64, to: u64) {
    for ms in (from..=to).step_by(50) {
        see(app, ms);
    }
}

#[test]
fn a_hold_confirms_only_when_held_for_a_second() {
    let mut app = hold_screen();
    assert_eq!(focused(&mut app).as_deref(), Some(dialog::CANCEL));
    let button = bounds(&mut app, Role::Button, HOLD_LABEL);
    down(&mut app, button, 2300);
    frames(&mut app, 2300, 2800);
    up(&mut app, button, 2810);
    frames(&mut app, 2850, 4000);
    assert_eq!(runs(&mut app), 0, "let go");

    down(&mut app, button, 4100);
    frames(&mut app, 4100, 5000);
    assert_eq!(runs(&mut app), 0, "not yet");
    frames(&mut app, 5050, 5250);
    assert_eq!(runs(&mut app), 1);
    assert_eq!(screen(&app), "none");
}

#[test]
fn a_release_during_a_stalled_frame_is_heard_before_the_hold_completes() {
    let mut app = hold_screen();
    let button = bounds(&mut app, Role::Button, HOLD_LABEL);
    down(&mut app, button, 2300);
    see(&mut app, 2310);
    // The next frame takes two seconds, and the release comes after it.
    app.draw(SIZE);
    app.show(at(4400));
    up(&mut app, button, 4401);
    frames(&mut app, 4450, 5000);
    assert_eq!(runs(&mut app), 0);
}

#[test]
fn a_screen_reader_holds_in_two_steps() {
    let mut app = hold_screen();
    assist(
        &mut app,
        Role::Button,
        HOLD_LABEL,
        Action::Click,
        None,
        2300,
    );
    assert!(heard(&mut app, "Activate again to start holding."));
    frames(&mut app, 2350, 3600);
    assert_eq!(runs(&mut app), 0, "once only arms");
    assist(
        &mut app,
        Role::Button,
        HOLD_LABEL,
        Action::Click,
        None,
        3700,
    );
    assert!(heard(&mut app, "Holding"));
    frames(&mut app, 3750, 4900);
    assert_eq!(runs(&mut app), 1);
}

#[test]
fn trap_1_a_hold_whose_control_moves_never_completes() {
    let mut app = hold_screen();
    let button = bounds(&mut app, Role::Button, HOLD_LABEL);
    down(&mut app, button, 2300);
    see(&mut app, 2310);
    assert!(app.holding.is_some());
    // The window shrinks: the button is no longer under the pointer. No input says so.
    see_at(&mut app, (900.0, 600.0), 2400);
    assert!(app.holding.is_none());
    for ms in (2450..=4000).step_by(50) {
        see_at(&mut app, (900.0, 600.0), ms);
    }
    assert_eq!(runs(&mut app), 0);
    assert_eq!(screen(&app), "strong");
}

#[test]
fn trap_2_esc_on_the_old_frame_cancels_nothing_unseen() {
    let mut app = app();
    let answered = job_asks(&mut app, confirm_question());
    assert!(app.wake());
    // The dialog is not on the display yet.
    let esc = Input::KeyDown {
        key: Key::Escape,
        mods: Modifiers::default(),
        repeat: false,
    };
    app.input(esc, at(900));
    app.input(Input::KeyUp { key: Key::Escape }, at(901));
    assert_eq!(screen(&app), "prompt");
    // Built but not presented: still not on the display.
    app.draw(SIZE);
    escape_without_a_frame(&mut app, 910);
    assert_eq!(screen(&app), "prompt");
    assert!(answers(&answered).is_empty());
    see(&mut app, 1000);
    escape(&mut app, 1001);
    assert_eq!(answers(&answered), [None]);
}

fn escape_without_a_frame(app: &mut App, ms: u64) {
    key_down(app, Key::Escape, ms);
    app.input(Input::KeyUp { key: Key::Escape }, at(ms + 1));
}

#[test]
fn trap_3_a_dialog_that_cannot_be_built_is_not_cancelled_unseen() {
    let mut app = app();
    app.break_dialogs = true;
    let answered = job_asks(&mut app, confirm_question());
    app.wake();
    see(&mut app, 1000);
    see(&mut app, 2000);
    assert_eq!(screen(&app), "prompt");
    assert!(answers(&answered).is_empty());
    assert!(app.since.is_none(), "the model was not told it was shown");
    assert!(heard(&mut app, "A question could not be shown"));
    assert!(!heard(&mut app, "Verify files"));
    tap(&mut app, Key::Enter, 2100);
    assert!(answers(&answered).is_empty());
    escape(&mut app, 2200);
    assert_eq!(answers(&answered), [None]);
}

fn long_plan(warnings: usize) -> FakePlan {
    FakePlan {
        plan: Plan {
            verb: "Verify".to_owned(),
            title: "Replace 60 files?".to_owned(),
            subject: "Example Game".to_owned(),
            steps: (0..60)
                .map(|i| PlanStep {
                    kind: StepKind::Replace,
                    text: format!("file-{i:02}.bin"),
                    bytes: Some(1_000_000),
                })
                .collect(),
            download: Some(60_000_000),
            warnings: (0..warnings).map(|i| format!("warning {i}")).collect(),
            reversible: true,
            ..Plan::default()
        },
        ..FakePlan::default()
    }
}

fn inside(app: &mut App, label: &str) {
    let (x, y) = bounds(app, Role::Button, label);
    assert!(x > 0.0 && x < f64::from(SMALL.0), "{label} at {x}");
    assert!(y > 0.0 && y < f64::from(SMALL.1), "{label} at {y}");
}

#[test]
fn trap_4_a_long_plan_is_paged_with_its_warnings_and_buttons_on_every_page() {
    let mut app = app();
    script(&mut app, "game.update", long_plan(1));
    start(&mut app, "game.update", &[], 1000);
    see_at(&mut app, SMALL, 1000);
    let mut seen = Vec::new();
    for page in 0..60 {
        inside(&mut app, "Cancel");
        inside(&mut app, "Verify");
        assert!(heard(&mut app, "Warning: warning 0"), "page {page}");
        assert!(heard(&mut app, "To download: 60.0 MB"), "page {page}");
        assert!(heard(&mut app, "Nothing changes until you confirm."));
        seen.extend(read(&mut app).into_iter().filter(|l| l.contains(".bin")));
        if disabled(&mut app, "Next") {
            break;
        }
        let next = bounds(&mut app, Role::Button, "Next");
        down(&mut app, next, 1010 + page);
        up(&mut app, next, 1010 + page);
        see_at(&mut app, SMALL, 1010 + page);
    }
    for i in 0..60 {
        let file = format!("file-{i:02}.bin");
        assert!(seen.iter().any(|l| l.contains(&file)), "{file} never shown");
    }
}

#[test]
fn trap_4_what_cannot_fit_cannot_be_confirmed() {
    let mut small = super::tests::app();
    script(&mut small, "game.update", long_plan(12));
    start_small(&mut small);
    assert!(heard(&mut small, "Make the window larger"));
    assert!(disabled(&mut small, "Verify"));
    assert!(!heard(&mut small, "file-00.bin"));
    assert!(small.since.is_none(), "the model was not told it was shown");
    key_down(&mut small, Key::Enter, 3000);
    small.input(Input::KeyUp { key: Key::Enter }, at(3001));
    see_at(&mut small, SMALL, 3002);
    assert_eq!(screen(&small), "card");
    assert_eq!(runs(&mut small), 0);
    // Large enough: now it is seen, and the dwell starts now.
    see_at(&mut small, (1280.0, 1600.0), 4000);
    assert!(small.since.is_some());
    let verify = bounds(&mut small, Role::Button, "Verify");
    down(&mut small, verify, 4100);
    up(&mut small, verify, 4101);
    assert_eq!(runs(&mut small), 0);
    see_at(&mut small, (1280.0, 1600.0), 4600);
    down(&mut small, verify, 4600);
    up(&mut small, verify, 4601);
    assert_eq!(runs(&mut small), 1);
}

/// Starts the update where every frame, the first too, is of the smallest window.
fn start_small(app: &mut App) {
    let nowhere = (-10.0, -10.0);
    down(app, nowhere, 1000);
    up(app, nowhere, 1000);
    let spec = actions::action("game.update").expect("an action");
    let event = act::number(app.interaction.latest());
    app.invoke(&Invocation::new(spec).arg("id", Value::str("1")), event);
    see_at(app, SMALL, 1000);
}

#[test]
fn esc_and_cancel_leave_every_screen_and_run_nothing() {
    let prompts = [
        confirm_question(),
        PromptSpec::Text {
            label: "Name".to_owned(),
            default: String::new(),
        },
        PromptSpec::Secret {
            label: "Key".to_owned(),
        },
        PromptSpec::Choice {
            label: "Which?".to_owned(),
            options: vec!["one".to_owned()],
        },
        PromptSpec::Conflicts {
            files: vec![ConflictFile {
                name: "a.sav".to_owned(),
                local: "l".to_owned(),
                cloud: "c".to_owned(),
            }],
        },
    ];
    for by_esc in [true, false] {
        let leave = |app: &mut App| {
            if by_esc {
                escape(app, 5000);
            } else {
                click(app, "Cancel", 5000);
            }
        };
        for spec in prompts.clone() {
            let mut app = app();
            let answered = job_asks(&mut app, spec.clone());
            app.wake();
            see(&mut app, 1000);
            leave(&mut app);
            assert_eq!(answers(&answered), [None], "{spec:?} esc: {by_esc}");
        }

        let mut planning = app();
        planning.data.backend_mut().hold_plans = true;
        start(&mut planning, "game.update", &[], 1000);
        assert_eq!(screen(&planning), "planning");
        assert!(!heard(&mut planning, "Verify files"));
        leave(&mut planning);
        assert_eq!(screen(&planning), "none");
        assert_eq!(runs(&mut planning), 0);

        let mut card = app();
        start(&mut card, "game.update", &[], 1000);
        leave(&mut card);
        assert_eq!(screen(&card), "none");
        assert_eq!(runs(&mut card), 0);
        assert!(heard(&mut card, "Verify files"), "the page is back");

        let mut typed = app();
        script(&mut typed, "game.remove", destructive("Remove"));
        start(&mut typed, "game.remove", &[], 1000);
        click(&mut typed, "Remove", 1600);
        leave(&mut typed);
        assert_eq!(screen(&typed), "none");
        assert_eq!(runs(&mut typed), 0);

        let mut hold = hold_screen();
        leave(&mut hold);
        assert_eq!(screen(&hold), "none");
        assert_eq!(runs(&mut hold), 0);
    }
}

#[test]
fn enter_on_cancel_cancels_a_plan_card() {
    let mut app = app();
    start(&mut app, "game.update", &[], 1000);
    tap(&mut app, Key::Tab, 1600);
    assert_eq!(focused(&mut app).as_deref(), Some(dialog::CANCEL));
    tap(&mut app, Key::Enter, 1700);
    assert_eq!(screen(&app), "none");
    assert_eq!(runs(&mut app), 0);
}

#[test]
fn errors_wait_behind_an_open_dialog_and_only_the_user_dismisses_them() {
    let mut app = app();
    start(&mut app, "game.update", &[], 1000);
    app.data
        .failures()
        .report(ErrorView::failed("It broke", "the disk is full"));
    app.wake();
    see(&mut app, 1100);
    assert_eq!(screen(&app), "card", "the dialog stays on top");
    assert!(!heard(&mut app, "It broke"));
    escape(&mut app, 1200);
    assert_eq!(screen(&app), "error");
    assert!(has_role(&mut app, Role::AlertDialog));
    assert!(heard(&mut app, "It broke"));
    assert!(heard(&mut app, "the disk is full"));
    assert!(!heard(&mut app, "Verify files"));
    assert_eq!(focused(&mut app).as_deref(), Some(dialog::CLOSE));
    for ms in [2000, 60_000] {
        app.wake();
        see(&mut app, ms);
        assert_eq!(screen(&app), "error");
    }
    click(&mut app, "Close", 61_000);
    assert_eq!(screen(&app), "none");
}

#[test]
fn esc_dismisses_an_error_once_it_is_on_the_display() {
    let mut app = app();
    app.data
        .failures()
        .report(ErrorView::failed("It broke", "no"));
    app.wake();
    escape_without_a_frame(&mut app, 900);
    assert_eq!(screen(&app), "error");
    see(&mut app, 1000);
    escape(&mut app, 1001);
    assert_eq!(screen(&app), "error", "not read yet");
    escape(&mut app, 1600);
    assert_eq!(screen(&app), "none");
}

#[test]
fn a_double_enter_does_not_close_the_error_it_brought_up() {
    let mut app = app();
    start(&mut app, "game.update", &[], 1000);
    // What was fine when the card was made is refused at the confirmation.
    app.data.backend_mut().plans.insert(
        "game.update",
        Err(ErrorView::failed("Refused", "writing is off now")),
    );
    tap(&mut app, Key::Enter, 1600);
    assert_eq!(screen(&app), "error");
    assert_eq!(focused(&mut app).as_deref(), Some(dialog::CLOSE));
    tap(&mut app, Key::Enter, 1700);
    assert_eq!(screen(&app), "error");
    assert!(heard(&mut app, "writing is off now"));
    click(&mut app, "Close", 1800);
    assert_eq!(screen(&app), "error", "a click is no faster");
    assist(&mut app, Role::Button, "Close", Action::Click, None, 1900);
    assert_eq!(screen(&app), "error", "nor is a screen reader's");
    tap(&mut app, Key::Enter, 2200);
    assert_eq!(screen(&app), "none");
    assert_eq!(runs(&mut app), 0);
}

#[test]
fn a_double_esc_does_not_close_the_error_queued_behind_a_dialog() {
    let mut app = app();
    start(&mut app, "game.update", &[], 1000);
    app.data
        .failures()
        .report(ErrorView::failed("It broke", "no"));
    app.wake();
    see(&mut app, 1100);
    escape(&mut app, 1600);
    assert_eq!(screen(&app), "error");
    escape(&mut app, 1700);
    assert_eq!(screen(&app), "error");
    escape(&mut app, 2200);
    assert_eq!(screen(&app), "none");
}

#[test]
fn an_error_too_large_for_the_window_can_still_be_closed_after_the_wait() {
    let mut app = app();
    app.data
        .failures()
        .report(ErrorView::failed("It broke", "very long ".repeat(500)));
    app.wake();
    see_at(&mut app, SMALL, 1000);
    assert!(heard(&mut app, "Make the window larger"));
    let esc = |app: &mut App, ms| {
        escape_without_a_frame(app, ms);
        see_at(app, SMALL, ms + 2);
    };
    esc(&mut app, 1100);
    assert_eq!(screen(&app), "error");
    esc(&mut app, 1600);
    assert_eq!(screen(&app), "none");
}

#[test]
fn a_hold_that_interaction_ends_with_a_new_frame_is_stopped_at_once() {
    let mut app = hold_screen();
    let button = bounds(&mut app, Role::Button, HOLD_LABEL);
    down(&mut app, button, 2300);
    see(&mut app, 2310);
    assert!(app.holding.is_some());
    app.draw((900.0, 600.0));
    // Taking the frame over alone, before anything else looks at the hold.
    app.take_over();
    assert!(app.holding.is_none());
    app.data.tick(at(5000));
    assert_eq!(runs(&mut app), 0, "the model's hold is stopped too");
}

#[test]
fn a_hold_interaction_no_longer_has_is_not_ticked() {
    let mut app = hold_screen();
    let button = bounds(&mut app, Role::Button, HOLD_LABEL);
    down(&mut app, button, 2300);
    see(&mut app, 2310);
    // Interaction lets go and the intent saying so is lost.
    let shown = app.shown.clone();
    let layers = shown.layers();
    let frame = Layers { layers: &layers };
    app.interaction.handle(Input::FocusLost, at(2320), &frame);
    assert!(app.holding.is_some());
    frames(&mut app, 2350, 4000);
    assert!(app.holding.is_none());
    assert_eq!(runs(&mut app), 0);
}

#[test]
fn esc_cancels_even_when_nothing_about_a_dialog_can_be_built() {
    let mut app = app();
    app.break_dialogs = true;
    app.break_panels = true;
    let answered = job_asks(&mut app, confirm_question());
    app.wake();
    see(&mut app, 1000);
    assert_eq!(screen(&app), "prompt");
    assert!(answers(&answered).is_empty());
    assert!(heard(&mut app, "A question could not be shown"));
    assert!(!heard(&mut app, "Verify files"));
    tap(&mut app, Key::Enter, 1100);
    assert!(answers(&answered).is_empty());
    escape(&mut app, 1200);
    assert_eq!(answers(&answered), [None]);
}

#[test]
fn every_control_of_a_dialog_says_what_it_does() {
    let mut app = app();
    script(&mut app, "game.update", long_plan(1));
    start(&mut app, "game.update", &[], 1000);
    see_at(&mut app, SMALL, 1000);
    let dialog = app.shown.dialog.clone().expect("a dialog");
    let layer = dialog.layer();
    let mut controls = 0;
    for id in layer.tree.iter() {
        let node = layer.tree.get(id).expect("a node");
        if node.a11y.focusable || node.a11y.disabled {
            controls += 1;
            assert!(layer.action_identity(id).is_some(), "{:?}", node.a11y.name);
        }
    }
    assert_eq!(controls, 4, "Previous, Next, Cancel and the verb");
    // One screen is one layer, whatever page it is on.
    click_small(&mut app, "Next", 1100);
    assert_eq!(
        app.shown.dialog.as_ref().expect("a dialog").layer().id,
        layer.id
    );
}

fn click_small(app: &mut App, label: &str, ms: u64) {
    let point = bounds(app, Role::Button, label);
    down(app, point, ms);
    up(app, point, ms);
    see_at(app, SMALL, ms);
}

#[test]
fn under_a_dialog_the_page_takes_nothing() {
    let mut app = app();
    let verify = bounds(&mut app, Role::Button, "Verify files");
    let tab = bounds(&mut app, Role::Tab, "Builds");
    start(&mut app, "game.update", &[], 1000);
    for (point, ms) in [(verify, 1600), (tab, 1700)] {
        down(&mut app, point, ms);
        up(&mut app, point, ms + 1);
        see(&mut app, ms + 2);
    }
    assert_eq!(runs(&mut app), 0);
    assert_eq!(app.page.ui("tab"), Some(&Value::str("overview")));
    assert_eq!(screen(&app), "card");
}

/// A press of `key` with the platform's shortcut modifier.
fn chord(app: &mut App, key: char, shift: bool, ms: u64) {
    let mods = Modifiers {
        ctrl: cfg!(not(target_os = "macos")),
        meta: cfg!(target_os = "macos"),
        shift,
        ..Modifiers::default()
    };
    let key = Key::Char(key);
    app.input(
        Input::KeyDown {
            key,
            mods,
            repeat: false,
        },
        at(ms),
    );
    app.input(Input::KeyUp { key }, at(ms + 1));
    see(app, ms + 2);
}

fn type_text(app: &mut App, text: &str, ms: u64) {
    app.input(Input::Text(text.to_owned()), at(ms));
    see(app, ms + 1);
}

fn copied(app: &mut App) -> Option<String> {
    app.copied().map(|text| text.as_str().to_owned())
}

fn field_text(app: &App) -> String {
    app.entry.buffer.as_ref().expect("a field").text()
}

#[test]
fn a_field_is_edited_at_its_caret_and_copied_from() {
    let (mut app, answered) = app_with_text_prompt();
    see(&mut app, 1000);
    click_field(&mut app, "Name", 1100);
    assert_eq!(copied(&mut app), None, "a click copies nothing");
    tap(&mut app, Key::Home, 1110);
    type_text(&mut app, "A ", 1120);
    assert_eq!(field_text(&app), "A before");
    chord(&mut app, 'a', false, 1130);
    chord(&mut app, 'c', false, 1140);
    assert_eq!(copied(&mut app).as_deref(), Some("A before"));
    assert_eq!(copied(&mut app), None, "handed over once");
    chord(&mut app, 'x', false, 1150);
    assert_eq!(copied(&mut app).as_deref(), Some("A before"));
    assert_eq!(field_text(&app), "");
    chord(&mut app, 'c', false, 1160);
    assert_eq!(copied(&mut app), None, "nothing selected");
    chord(&mut app, 'z', false, 1170);
    assert_eq!(field_text(&app), "A before");
    chord(&mut app, 'z', true, 1180);
    assert_eq!(field_text(&app), "");
    // Pasted where the caret is.
    type_text(&mut app, "[]", 1190);
    tap(&mut app, Key::ArrowLeft, 1200);
    chord(&mut app, 'v', false, 1600);
    app.pasted(Some(Zeroizing::new("in".to_owned())));
    see(&mut app, 1610);
    assert_eq!(field_text(&app), "[in]");
    assert!(heard(&mut app, "[in]"));
    click(&mut app, "OK", 1700);
    assert_eq!(
        answers(&answered),
        [Some(PromptAnswer::Text("[in]".to_owned()))]
    );
}

#[test]
fn a_long_value_stays_in_the_field_with_its_caret() {
    let (mut app, _) = app_with_text_prompt();
    see(&mut app, 1000);
    click_field(&mut app, "Name", 1100);
    let narrow = bounds(&mut app, Role::TextInput, "Name");
    type_text(&mut app, &"m".repeat(200), 1110);
    assert_eq!(bounds(&mut app, Role::TextInput, "Name"), narrow);
    assert!(app.entry.scroll > 0.0);
    // A click at the left edge puts the caret into the part that is in view.
    tap(&mut app, Key::Home, 1120);
    assert_eq!(app.entry.scroll, 0.0);
    assert!(app.shown.dialog.as_ref().is_some_and(|d| d.answerable()));
}

#[test]
fn nothing_can_be_typed_edited_or_copied_on_the_secret_screen() {
    let mut app = app();
    let answered = job_asks(
        &mut app,
        PromptSpec::Secret {
            label: "The key".to_owned(),
        },
    );
    app.wake();
    see(&mut app, 1000);
    click(&mut app, "Paste", 1600);
    app.pasted(Some(Zeroizing::new("sesame".to_owned())));
    see(&mut app, 1610);
    assert!(app.entry.buffer.is_none(), "no editor holds a secret");
    type_text(&mut app, "typed", 1620);
    for (i, key) in ['a', 'c', 'x', 'z', 'y'].into_iter().enumerate() {
        chord(&mut app, key, false, 1630 + 10 * i as u64);
        assert_eq!(copied(&mut app), None, "{key}");
    }
    tap(&mut app, Key::Delete, 1690);
    assert!(app.entry.buffer.is_none());
    assert!(!app.dump().contains("sesame"));
    click(&mut app, "Send", 1700);
    assert_eq!(
        answers(&answered),
        [Some(PromptAnswer::Secret(gogo_gui_model::Secret::new(
            "sesame"
        )))]
    );
}

fn compose(app: &mut App, text: &str, ms: u64) {
    app.input(
        Input::Ime(ImeInput::Preedit {
            text: text.to_owned(),
            cursor: Some((text.len(), text.len())),
        }),
        at(ms),
    );
    see(app, ms + 1);
}

/// What winit sends for a result: the composition cleared, then the text.
fn commit(app: &mut App, text: &str, ms: u64) {
    compose(app, "", ms);
    app.input(Input::Ime(ImeInput::Commit(text.to_owned())), at(ms + 2));
    see(app, ms + 3);
}

#[test]
fn an_input_method_composes_in_the_field_and_only_its_result_counts() {
    let mut app = app();
    script(&mut app, "game.remove", destructive("Remove"));
    start(&mut app, "game.remove", &[], 1000);
    click(&mut app, "Remove", 1600);
    see(&mut app, 2300);
    assert_eq!(app.ime_area(), None, "no field has focus");
    let field = "Type the title: Example Game";
    click_field(&mut app, field, 2400);
    let first = app.ime_area().expect("the input method is wanted");
    assert!(first.height > 10.0 && first.x > 0.0, "{first:?}");

    compose(&mut app, "Example Game", 2410);
    assert!(disabled(&mut app, "Remove"), "not typed until committed");
    assert_eq!(field_text(&app), "");
    // The keys are the input method's: neither the dialog nor the field takes them.
    tap(&mut app, Key::Escape, 2420);
    tap(&mut app, Key::Backspace, 2430);
    assert_eq!(screen(&app), "strong");
    commit(&mut app, "Example Game", 2440);
    assert_eq!(field_text(&app), "Example Game");
    assert!(!disabled(&mut app, "Remove"));
    let after = app.ime_area().expect("still");
    assert!(after.x > first.x, "it follows the caret: {after:?}");
    assert_eq!(runs(&mut app), 0, "a commit answers nothing");

    // Tab is the input method's too; a press elsewhere gives the composition up.
    compose(&mut app, "zz", 2450);
    tap(&mut app, Key::Tab, 2460);
    assert_eq!(focused(&mut app).as_deref(), Some(dialog::FIELD));
    click(&mut app, "Cancel", 2900);
    assert_eq!(screen(&app), "none");
    assert_eq!(app.ime_area(), None);
}

#[test]
fn assistive_technology_selects_in_a_field_and_replaces() {
    let (mut app, _) = app_with_text_prompt();
    see(&mut app, 1000);
    let value = ActionData::Value("x".into());
    assist(
        &mut app,
        Role::TextInput,
        "Name",
        Action::ReplaceSelectedText,
        Some(value),
        1100,
    );
    assert_eq!(field_text(&app), "beforex", "at the caret, the end");
}

#[test]
fn the_secret_screen_never_asks_for_an_input_method() {
    let mut app = app();
    let _answered = job_asks(
        &mut app,
        PromptSpec::Secret {
            label: "The key".to_owned(),
        },
    );
    app.wake();
    see(&mut app, 1000);
    for (i, ms) in [1100, 1200, 1300, 1400].into_iter().enumerate() {
        assert_eq!(app.ime_area(), None, "after {i} tabs");
        tap(&mut app, Key::Tab, ms);
    }
    compose(&mut app, "typed", 1500);
    commit(&mut app, "typed", 1510);
    assert!(app.entry.buffer.is_none());
    assert_eq!(app.entry.code, None);
    assert!(!app.dump().contains("typed"));
}
