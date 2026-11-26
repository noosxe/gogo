//! S19: global keys are the app's (design 26 §7), shortcuts and holds.

mod common;

use common::*;
use gogo_contract::{Value, actions};
use gogo_gui_model::fake::FakeBackend;
use gogo_gui_model::{
    DWELL, HOLD, InputEvent as ModelEvent, Invocation, Modal, Model, Outcome, Press,
};
use gogo_ui_interact::{
    ARM_EXPIRES, Config, Frame, HOLD_ARM, Input, InputProvenance, Intent, Interaction, Key,
    LayerKind, Modifiers, Primary, RequestKind,
};
use gogo_ui_ir::Node;

fn keyed(name: &str, action: u32, key: &str) -> Node {
    let mut node = button(name, action);
    node.a11y.key = Some(key.to_owned());
    node
}

/// A skin that claims every global key.
fn greedy() -> Screen {
    Screen::new(vec![
        ("esc", keyed("Esc", 0, "Esc")),
        ("enter", keyed("Enter", 1, "Enter")),
        ("palette", keyed("Search", 2, "Ctrl K")),
        ("play", keyed("Play", 3, "P")),
        ("play2", keyed("Play too", 4, "P")),
    ])
}

fn dialog() -> Screen {
    Screen::new(vec![
        ("confirm", app_button("Confirm", 0)),
        ("cancel", app_button("Cancel", 1)),
    ])
}

#[test]
fn s19_a_skin_cannot_take_esc_enter_or_the_palette_key() {
    let s = greedy();
    let layers = [s.layer(PAGE, LayerKind::Page)];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    assert_eq!(names(&d.tap(&frame, Key::Escape)), ["dismiss"]);
    assert!(d.tap(&frame, Key::Enter).is_empty());
    assert_eq!(names(&d.down(&frame, Key::Char('k'), CTRL)), ["palette"]);
    // A held key opens or closes only once.
    assert!(d.repeat(&frame, Key::Char('k')).is_empty());
    d.up(&frame, Key::Char('k'));
    d.down(&frame, Key::Escape, NONE);
    assert!(d.repeat(&frame, Key::Escape).is_empty());
}

#[test]
fn s19_the_palette_key_follows_the_platform() {
    let s = greedy();
    let layers = [s.layer(PAGE, LayerKind::Page)];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    d.ix = Interaction::new(Config::new(Primary::Meta));
    let meta = Modifiers { meta: true, ..NONE };
    assert_eq!(names(&d.down(&frame, Key::Char('K'), meta)), ["palette"]);
    d.up(&frame, Key::Char('K'));
    // The skin's "Ctrl K" can't have it either.
    assert!(d.down(&frame, Key::Char('k'), CTRL).is_empty());
}

#[test]
fn s19_esc_and_enter_reach_a_dialog() {
    let (s, dl) = (greedy(), dialog());
    let layers = [
        s.layer(PAGE, LayerKind::Page),
        dl.layer(DIALOG, LayerKind::Dialog),
    ];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    assert_eq!(names(&d.tap(&frame, Key::Escape)), ["dismiss"]);
    assert_eq!(names(&d.tap(&frame, Key::Enter)), ["enter"]);
    assert_eq!(names(&d.down(&frame, Key::Char('k'), CTRL)), ["palette"]);
    // No skin shortcut under a dialog.
    assert!(d.tap(&frame, Key::Char('p')).is_empty());
    // A focused control takes Enter, so Enter on Cancel cancels.
    d.tab(&frame);
    d.tab(&frame);
    assert_eq!(names(&d.tap(&frame, Key::Enter)), ["app cancel"]);
}

#[test]
fn s19_skin_shortcuts_act_once_on_the_page() {
    let s = greedy();
    let layers = [s.layer(PAGE, LayerKind::Page)];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    // The first in document order.
    assert_eq!(
        names(&d.down(&frame, Key::Char('p'), NONE)),
        ["invoke play"]
    );
    assert!(d.repeat(&frame, Key::Char('p')).is_empty());
    assert!(d.down(&frame, Key::Char('p'), NONE).is_empty());
    d.up(&frame, Key::Char('p'));
    assert!(d.down(&frame, Key::Char('p'), CTRL).is_empty());
}

// --- holds ---

fn hold_frame<'a>(s: &'a Screen) -> [gogo_ui_interact::Layer<'a>; 1] {
    [s.layer(DIALOG, LayerKind::Alert).hold(&["confirm"])]
}

#[test]
fn a_hold_control_is_held_by_pointer_or_key_and_never_activates() {
    let s = dialog();
    let layers = hold_frame(&s);
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    let at = s.center("confirm");
    assert_eq!(names(&d.press_at(&frame, at)), ["hold confirm"]);
    assert_eq!(names(&d.release_at(&frame, at)), ["end confirm"]);
    d.press_at(&frame, at);
    let off = d.send(&frame, Input::PointerMoved { x: 700.0, y: 500.0 });
    assert_eq!(names(&off.intents), ["end confirm"]);
    d.release_at(&frame, at);
    // Focus is still on it.
    for key in [Key::Enter, Key::Space] {
        assert_eq!(names(&d.down(&frame, key, NONE)), ["hold confirm"]);
        assert!(d.repeat(&frame, key).is_empty());
        assert_eq!(names(&d.up(&frame, key)), ["end confirm"]);
    }
    for cancel in [Key::Escape, Key::Tab] {
        d.assist(&frame, DIALOG, "confirm", RequestKind::Focus);
        d.down(&frame, Key::Space, NONE);
        let out = d.down(&frame, cancel, NONE);
        assert_eq!(names(&out)[0], "end confirm", "{cancel:?}");
        d.up(&frame, Key::Space);
        d.up(&frame, cancel);
    }
    d.assist(&frame, DIALOG, "confirm", RequestKind::Focus);
    d.down(&frame, Key::Space, NONE);
    let lost = d.send(&frame, Input::FocusLost);
    assert_eq!(names(&lost.intents), ["end confirm"]);
}

#[test]
fn a_screen_reader_holds_with_two_activations() {
    let s = dialog();
    let layers = hold_frame(&s);
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    let activate =
        |d: &mut Driver| names(&d.assist(&frame, DIALOG, "confirm", RequestKind::Activate));
    assert_eq!(activate(&mut d), ["armed confirm"]);
    assert!(d.ix.armed().is_some());
    assert!(activate(&mut d).is_empty(), "too soon");
    d.now += HOLD_ARM;
    assert_eq!(activate(&mut d), ["hold confirm"]);
    assert!(d.ix.armed().is_none());
    assert!(activate(&mut d).is_empty(), "already holding");
    // It runs until something cancels it.
    assert!(d.ix.sync(&frame).intents.is_empty());
    assert_eq!(
        names(&d.tap(&frame, Key::Escape)),
        ["end confirm", "dismiss"]
    );

    assert_eq!(activate(&mut d), ["armed confirm"]);
    d.assist(&frame, DIALOG, "cancel", RequestKind::Focus);
    assert!(d.ix.armed().is_none(), "moving away disarms");
    d.now += HOLD_ARM;
    assert_eq!(activate(&mut d), ["armed confirm"]);
    d.now += HOLD_ARM;
    activate(&mut d);
    let closed = d.ix.sync(&Frame { layers: &[] });
    assert_eq!(names(&closed.intents), ["end confirm"]);
}

#[test]
fn a_pointer_hold_ends_when_its_control_moves_or_is_covered() {
    let s = dialog();
    let moved = Screen::new(vec![
        ("pad", button("Pad", 0)),
        ("pad2", button("Pad", 1)),
        ("confirm", app_button("Confirm", 0)),
    ]);
    let before = hold_frame(&s);
    let after = hold_frame(&moved);
    let cover = Screen::new(vec![("toast", app_button("Toast", 2))]);
    let covered = [
        s.layer(DIALOG, LayerKind::Alert).hold(&["confirm"]),
        cover.layer(PAGE, LayerKind::Overlay),
    ];
    for after in [&after[..], &covered[..]] {
        let mut d = Driver::new();
        d.press_at(&Frame { layers: &before }, s.center("confirm"));
        let out = d.ix.sync(&Frame { layers: after });
        assert_eq!(names(&out.intents), ["end confirm"]);
    }
}

#[test]
fn a_hold_ends_when_its_control_is_disabled() {
    let s = dialog();
    let mut off = app_button("Confirm", 0);
    off.a11y.disabled = true;
    let disabled = Screen::new(vec![("confirm", off), ("cancel", app_button("Cancel", 1))]);
    let (before, after) = (hold_frame(&s), hold_frame(&disabled));
    let (before, after) = (Frame { layers: &before }, Frame { layers: &after });
    let mut d = Driver::new();
    d.tab(&before);
    d.down(&before, Key::Space, NONE);
    assert_eq!(names(&d.ix.sync(&after).intents), ["end confirm"]);
    assert_ne!(d.focused(), "confirm");
}

#[test]
fn a_press_is_dropped_when_its_control_becomes_a_hold_control() {
    let s = dialog();
    let plain = [s.layer(DIALOG, LayerKind::Alert)];
    let held = hold_frame(&s);
    let (plain, held) = (Frame { layers: &plain }, Frame { layers: &held });
    let mut d = Driver::new();
    d.press_at(&plain, s.center("confirm"));
    assert!(d.release_at(&held, s.center("confirm")).is_empty());
    d.down(&plain, Key::Space, NONE);
    assert!(d.up(&held, Key::Space).is_empty());
}

#[test]
fn any_other_key_or_button_cancels_a_screen_readers_hold() {
    let s = dialog();
    let layers = hold_frame(&s);
    let frame = Frame { layers: &layers };
    let start = |d: &mut Driver| {
        d.assist(&frame, DIALOG, "confirm", RequestKind::Activate);
        d.now += HOLD_ARM;
        let out = d.assist(&frame, DIALOG, "confirm", RequestKind::Activate);
        assert_eq!(names(&out), ["hold confirm"]);
    };
    for (key, mods) in [
        (Key::Char('x'), NONE),
        (Key::Enter, SHIFT),
        (Key::ArrowDown, NONE),
    ] {
        let mut d = Driver::new();
        start(&mut d);
        assert_eq!(
            names(&d.down(&frame, key, mods)),
            ["end confirm"],
            "{key:?}"
        );
    }
    let mut d = Driver::new();
    start(&mut d);
    assert!(
        d.down(&frame, Key::Other, NONE).is_empty(),
        "a bare modifier"
    );
    let (x, y) = s.center("cancel");
    let button = gogo_ui_interact::PointerButton::Secondary;
    let out = d.send(&frame, Input::PointerDown { button, x, y });
    assert_eq!(names(&out.intents), ["end confirm"]);
}

#[test]
fn an_old_arm_expires() {
    let s = dialog();
    let layers = hold_frame(&s);
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    d.assist(&frame, DIALOG, "confirm", RequestKind::Activate);
    d.now += ARM_EXPIRES;
    let late = d.assist(&frame, DIALOG, "confirm", RequestKind::Activate);
    assert_eq!(names(&late), ["armed confirm"]);
}

#[test]
fn s4_a_lost_release_does_not_let_a_later_tap_spend_the_old_press() {
    let s = greedy();
    let layers = [s.layer(PAGE, LayerKind::Page)];
    let frame = Frame { layers: &layers };
    let mut d = Driver::new();
    d.tab(&frame);
    d.down(&frame, Key::Space, NONE);
    // The release never arrives; much later the key is tapped.
    assert!(d.tap(&frame, Key::Space).is_empty());
    assert_eq!(d.tap(&frame, Key::Space).len(), 1);
}

// --- end to end with the app's safety flow ---

/// `gogo-gui`'s translation, for the few intents these tests meet.
fn press(p: &InputProvenance) -> Press {
    Press {
        event: ModelEvent(p.event().get()),
        at: p.at(),
        repeat: p.repeat(),
    }
}

fn app() -> Model<FakeBackend> {
    Model::new(FakeBackend::new(), || {})
}

fn call(name: &str) -> Invocation {
    Invocation::new(actions::action(name).unwrap()).arg("id", Value::str("1"))
}

fn apply(model: &mut Model<FakeBackend>, intents: &[Intent]) -> Vec<Outcome> {
    intents
        .iter()
        .map(|intent| match intent {
            Intent::Invoke { provenance, .. } => {
                model.invoke(&call("game.update"), ModelEvent(provenance.event().get()))
            }
            Intent::Enter { provenance } => model.enter(press(provenance)),
            Intent::Dismiss { .. } => model.dismiss(),
            Intent::HoldStart { provenance, .. } => {
                let ticket = model.modal().unwrap().ticket();
                model.hold_start(ticket, press(provenance))
            }
            other => panic!("{other:?}"),
        })
        .collect()
}

/// A skin asks for `call` with a real, numbered input.
fn open(model: &mut Model<FakeBackend>, d: &mut Driver, call: &Invocation) {
    let nothing = Frame { layers: &[] };
    let out = d.tap(&nothing, Key::Escape);
    let [Intent::Dismiss { provenance }] = &out[..] else {
        panic!("{out:?}");
    };
    let event = ModelEvent(provenance.event().get());
    assert_eq!(model.invoke(call, event), Outcome::Accepted);
    model.pump();
}

fn present(model: &mut Model<FakeBackend>, d: &Driver) {
    let ticket = model.modal().expect("a dialog").ticket();
    model.presented(ticket, ModelEvent(d.ix.latest().get()), d.now);
}

#[test]
fn s19_the_enter_that_opened_the_card_cannot_confirm_it() {
    let s = greedy();
    let card = Screen::new(vec![("text", Node::text("Update?"))]);
    let page = [s.layer(PAGE, LayerKind::Page)];
    let page = Frame { layers: &page };
    let over = [
        s.layer(PAGE, LayerKind::Page),
        card.layer(DIALOG, LayerKind::Dialog),
    ];
    let over = Frame { layers: &over };
    let (mut d, mut model) = (Driver::new(), app());
    d.tab(&page);
    let opened = d.down(&page, Key::Enter, NONE);
    assert_eq!(apply(&mut model, &opened), [Outcome::Accepted]);
    model.pump();
    assert!(matches!(model.modal(), Some(Modal::PlanCard { .. })));
    present(&mut model, &d);

    // The key is still down and repeating, long after the card appeared.
    d.now += DWELL * 4;
    assert!(d.repeat(&over, Key::Enter).is_empty());
    assert!(d.down(&over, Key::Enter, NONE).is_empty());
    d.up(&over, Key::Enter);

    // A fresh press, once the card has been up long enough.
    let fresh = d.down(&over, Key::Enter, NONE);
    assert_eq!(names(&fresh), ["enter"]);
    assert_eq!(apply(&mut model, &fresh), [Outcome::Accepted]);
    model.pump();
    assert_eq!(model.backend_mut().runs.len(), 1);
}

#[test]
fn s19_a_press_begun_before_the_card_cannot_confirm_it() {
    let card = Screen::new(vec![("confirm", app_button("Update", 0))]);
    let over = [card.layer(DIALOG, LayerKind::Dialog)];
    let over = Frame { layers: &over };
    let (mut d, mut model) = (Driver::new(), app());
    open(&mut model, &mut d, &call("game.update"));
    // Down before the card is reported drawn, up long after.
    d.press_at(&over, card.center("confirm"));
    present(&mut model, &d);
    d.now += DWELL * 4;
    let released = d.release_at(&over, card.center("confirm"));
    let Some(Intent::App { provenance, .. }) = released.first() else {
        panic!("{released:?}");
    };
    let ticket = model.modal().unwrap().ticket();
    assert_eq!(model.confirm(ticket, press(provenance)), Outcome::Ignored);
    assert!(model.backend_mut().runs.is_empty());
    // The same click made now does confirm.
    let again = d.click(&over, card.center("confirm"));
    let Some(Intent::App { provenance, .. }) = again.first() else {
        panic!("{again:?}");
    };
    assert_eq!(model.confirm(ticket, press(provenance)), Outcome::Accepted);
}

#[test]
fn s19_esc_closes_the_models_dialog() {
    let s = greedy();
    let layers = [s.layer(PAGE, LayerKind::Page)];
    let frame = Frame { layers: &layers };
    let (mut d, mut model) = (Driver::new(), app());
    open(&mut model, &mut d, &call("game.update"));
    let esc = d.tap(&frame, Key::Escape);
    assert_eq!(apply(&mut model, &esc), [Outcome::Accepted]);
    assert!(model.modal().is_none());
}

#[test]
fn a_screen_reader_hold_confirms_through_the_model() {
    let s = dialog();
    let layers = hold_frame(&s);
    let frame = Frame { layers: &layers };
    let (mut d, mut model) = (Driver::new(), app());
    let prune = Invocation::new(actions::action("apps.prune_orphaned").unwrap())
        .arg("keep_override", Value::Bool(false));
    open(&mut model, &mut d, &prune);
    present(&mut model, &d);
    d.now += DWELL;
    // The card first, then the hold.
    let enter = d.down(&frame, Key::Enter, NONE);
    apply(&mut model, &enter);
    model.pump();
    assert!(
        matches!(model.modal(), Some(Modal::Strong { .. })),
        "{:?}",
        model.modal()
    );
    present(&mut model, &d);
    d.now += DWELL;
    let first = d.assist(&frame, DIALOG, "confirm", RequestKind::Activate);
    assert_eq!(names(&first), ["armed confirm"]);
    d.now += HOLD_ARM;
    let second = d.assist(&frame, DIALOG, "confirm", RequestKind::Activate);
    assert_eq!(apply(&mut model, &second), [Outcome::Accepted]);
    assert!(
        model.backend_mut().runs.is_empty(),
        "not before the hold ends"
    );
    model.tick(d.now + HOLD);
    model.pump();
    assert_eq!(model.backend_mut().runs.len(), 1);
}
