//! The fake serves the fixture through the model, and only when asked to (design 25 §2).

use gogo_contract::Value;
use gogo_gui_model::fake::FakeBackend;
use gogo_gui_model::fixture::{Fixture, GAME_ID, art};
use gogo_gui_model::{Model, SourceKey};

fn key(name: &str, args: &[&str]) -> SourceKey {
    SourceKey::new(name, args).unwrap()
}

#[test]
fn a_watched_source_gets_the_fixture_value() {
    let mut model = Model::new(FakeBackend::with_fixture(), || {});
    let data = Fixture::new();
    let account = key("account", &[]);
    let game = key("game", &[GAME_ID]);
    let unknown_game = key("game", &["404"]);
    let lacking = key("apps", &[]);
    for k in [&account, &game, &unknown_game, &lacking] {
        model.watch(k);
    }
    model.pump();

    assert!(data.var("account").is_some());
    assert_eq!(model.value(&account), data.var("account"));
    assert!(data.call("game", GAME_ID).is_some());
    assert_eq!(model.value(&game), data.call("game", GAME_ID));
    assert_eq!(model.value(&unknown_game), None);
    assert_eq!(model.value(&lacking), None);
    assert_eq!(model.backend().subscriptions.len(), 4);
}

#[test]
fn a_plain_fake_sets_nothing() {
    let mut model = Model::new(FakeBackend::new(), || {});
    let account = key("account", &[]);
    model.watch(&account);
    model.pump();
    assert_eq!(model.value(&account), None);
}

#[test]
fn routes_and_fields_can_be_changed() {
    let mut data = Fixture::new();
    assert_eq!(data.param("id"), Some(&Value::str(GAME_ID)));
    data.set_route("library", None);
    assert_eq!(data.param("id"), None);
    data.set_field("account", "username", Value::str("player-two"));
    let Some(Value::Map(account)) = data.var("account") else {
        panic!("account is a record");
    };
    assert_eq!(account.get("username"), Some(&Value::str("player-two")));
    assert!(data.has_call_name("game"));
    assert!(!data.has_call_name("account"));
}

#[test]
fn art_fills_its_size() {
    for p in [
        art::checker(),
        art::cover("art:1:cover"),
        art::cover("art:1:background"),
        art::cover("art:1:logo"),
    ] {
        assert!(p.width > 0 && p.height > 0);
        assert_eq!(p.rgba.len(), (p.width * p.height * 4) as usize);
    }
}
