//! Choosing an installed skin, over the fake engine and a skin folder of the test's own.

use std::path::Path;

use gogo_gui_model::Modal;
use gogo_ui_interact::Key;
use gogo_ui_shell::accesskit::Role;

use super::commands::ctrl_k;
use super::tests::{app, at, click_at, frame, key, press};
use super::*;
use crate::skins::{DEFAULT, Skins};

/// A skin folder with the Default skin installed once more, as `mine`.
fn installed() -> (tempfile::TempDir, Skins) {
    let tmp = tempfile::tempdir().expect("a temporary folder");
    let source = tmp.path().join("source");
    for (path, data) in gogo_skins::default().files {
        let full = source.join(path);
        std::fs::create_dir_all(full.parent().expect("a file in a folder")).expect("folders");
        std::fs::write(full, data).expect("a write");
    }
    rename(&source.join("skin.kdl"), "mine");
    let skins = Skins::new(tmp.path().join("skins"), tmp.path().join("gui"));
    let candidate = skins.folder.read(&source).expect("the skin reads");
    skins
        .folder
        .install(&candidate, false)
        .expect("it installs");
    (tmp, skins)
}

fn rename(manifest: &Path, name: &str) {
    let text = std::fs::read_to_string(manifest).expect("the manifest");
    let renamed = text.replace("skin \"default\"", &format!("skin \"{name}\""));
    std::fs::write(manifest, renamed).expect("a write");
}

fn with(skins: &Skins, once: Option<&str>) -> App {
    let mut app = app().with_skins(Some(skins.clone()), once);
    frame(&mut app);
    app
}

fn error(app: &App) -> Option<String> {
    match app.data.modal() {
        Some(Modal::Error { error, .. }) => Some(format!("{} {}", error.title, error.message)),
        _ => None,
    }
}

fn go_to_settings(app: &mut App, ms: u64) {
    let go = Invocation::new(actions::action("nav.go").expect("nav.go"))
        .arg("route", Value::str("settings"));
    click_at(app, (-10.0, -10.0), ms);
    let event = act::number(app.interaction.latest());
    app.invoke(&go, event);
    app.effects();
    frame(app);
}

fn set(app: &mut App, value: &str, ms: u64) {
    let set = Invocation::new(actions::action("settings.set").expect("settings.set"))
        .arg("key", Value::str("gui.skin"))
        .arg("value", Value::str(value));
    click_at(app, (-10.0, -10.0), ms);
    let event = act::number(app.interaction.latest());
    app.invoke(&set, event);
    app.effects();
    frame(app);
}

#[test]
fn the_settings_route_chooses_an_installed_skin_and_it_is_kept() {
    let (_tmp, skins) = installed();
    let mut app = with(&skins, None);
    assert_eq!(app.on_screen, DEFAULT);
    go_to_settings(&mut app, 10);
    press(&mut app, Role::Button, "Use the mine skin", 100);
    assert_eq!(app.on_screen, "mine");
    assert_eq!(skins.chosen().as_deref(), Some("mine"));
    assert_eq!(error(&app), None);
    assert!(app.data.backend().runs.is_empty(), "the engine has no part");

    // The next run starts with it.
    assert_eq!(with(&skins, None).on_screen, "mine");
}

#[test]
fn the_palette_goes_back_to_the_default_skin() {
    let (_tmp, skins) = installed();
    skins.choose("mine").expect("a write");
    let mut app = with(&skins, None);
    assert_eq!(app.on_screen, "mine");
    ctrl_k(&mut app, 10);
    frame(&mut app);
    app.input(Input::Text("default skin".to_owned()), at(20));
    frame(&mut app);
    key(&mut app, Key::Enter, 30);
    assert_eq!(app.on_screen, DEFAULT);
    assert_eq!(skins.chosen().as_deref(), Some(DEFAULT));
    assert!(app.shown.popup.is_none());
}

#[test]
fn a_switch_of_skin_closes_what_hung_over_the_old_one() {
    let (_tmp, skins) = installed();
    let mut app = with(&skins, None);
    ctrl_k(&mut app, 10);
    frame(&mut app);
    assert!(app.shown.popup.is_some());
    app.set_skin("mine");
    frame(&mut app);
    assert_eq!(app.on_screen, "mine");
    assert!(app.shown.popup.is_none());
}

#[test]
fn a_skin_for_one_run_is_not_kept() {
    let (_tmp, skins) = installed();
    assert_eq!(with(&skins, Some("mine")).on_screen, "mine");
    assert_eq!(skins.chosen(), None);

    skins.choose("mine").expect("a write");
    assert_eq!(with(&skins, Some(DEFAULT)).on_screen, DEFAULT);
    assert_eq!(skins.chosen().as_deref(), Some("mine"));
}

#[test]
fn a_chosen_skin_that_does_not_load_gives_default_says_why_and_stays_chosen() {
    let (_tmp, skins) = installed();
    skins.choose("mine").expect("a write");
    let dir = skins.folder.path("mine").expect("installed");
    std::fs::write(dir.join("tokens.kdl"), "nonsense {").expect("a write");
    let app = with(&skins, None);
    assert_eq!(app.on_screen, DEFAULT);
    let why = error(&app).expect("an error screen");
    assert!(why.contains("tokens.kdl"), "{why}");
    assert_eq!(skins.chosen().as_deref(), Some("mine"));

    std::fs::remove_dir_all(dir).expect("removed");
    let why = error(&with(&skins, None)).expect("an error screen");
    assert!(why.contains("not installed"), "{why}");
}

#[test]
fn only_an_installed_name_can_be_chosen() {
    let (_tmp, skins) = installed();
    for (i, bad) in ["../source", "/etc", "missing"].into_iter().enumerate() {
        let mut app = with(&skins, None);
        set(&mut app, bad, 10 + i as u64 * 100);
        assert_eq!(app.on_screen, DEFAULT, "{bad}");
        assert!(error(&app).is_some(), "{bad}");
        assert_eq!(skins.chosen(), None, "{bad}");
    }
}

#[test]
fn without_a_skin_folder_there_is_only_default() {
    let mut app = app().with_skins(None, None);
    frame(&mut app);
    set(&mut app, "mine", 10);
    assert_eq!(app.on_screen, DEFAULT);
}
