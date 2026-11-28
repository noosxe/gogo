//! The window's skin import, driven as the shell drives the app.

use std::path::{Path, PathBuf};

use gogo_gui_model::{DWELL, Modal, PromptSpec};
use gogo_ui_interact::Key;
use gogo_ui_shell::accesskit::Role;

use super::commands::ctrl_k;
use super::tests::{SIZE, app, at, bounds, click_at, frame, key};
use super::*;
use crate::skins::{DEFAULT, Skins};

struct Case {
    tmp: tempfile::TempDir,
    skins: Skins,
    app: App,
    ms: u64,
}

impl Case {
    fn new() -> Case {
        let tmp = tempfile::tempdir().expect("a temporary folder");
        let skins = Skins::new(tmp.path().join("skins"), tmp.path().join("gui"));
        let mut app = app().with_skins(Some(skins.clone()), None);
        frame(&mut app);
        Case {
            tmp,
            skins,
            app,
            ms: 0,
        }
    }

    /// A skin folder to import: the Default skin as `mine`, at `version`.
    fn source(&self, version: &str) -> PathBuf {
        let dir = self.tmp.path().join(format!("source-{version}"));
        for (path, data) in gogo_skins::default().files {
            let full = dir.join(path);
            std::fs::create_dir_all(full.parent().expect("a file in a folder")).expect("folders");
            std::fs::write(full, data).expect("a write");
        }
        let manifest = dir.join("skin.kdl");
        let text = std::fs::read_to_string(&manifest).expect("the manifest");
        let text = text
            .replace("skin \"default\"", "skin \"mine\"")
            .replace("\"0.1.0\"", &format!("\"{version}\""));
        std::fs::write(manifest, text).expect("a write");
        dir
    }

    /// Long enough on from the last frame for a screen to take a press.
    fn later(&mut self) -> u64 {
        self.ms += DWELL.as_millis() as u64 + 200;
        self.ms
    }

    /// The question on screen, as a screen reader would be told of it.
    fn asked(&self) -> Option<String> {
        match self.app.data.modal()? {
            Modal::Prompt { spec, .. } => Some(match spec {
                PromptSpec::Text { label, .. } | PromptSpec::Confirm { label } => label.clone(),
                PromptSpec::Offer { label, yes } => format!("{yes}: {label}"),
                _ => "another".to_owned(),
            }),
            Modal::Error { error, .. } => Some(format!("{}: {}", error.title, error.message)),
            _ => Some("another".to_owned()),
        }
    }

    /// Presses the dialog's control called `label`, once the dialog can take it.
    fn press(&mut self, label: &str) {
        let ms = self.later();
        let point = bounds(&mut self.app, Role::Button, label);
        click_at(&mut self.app, point, ms);
        // What comes up is first on the display now, not when the test began.
        self.app.draw(SIZE);
        self.app.show(at(ms + 3));
    }

    /// "Import skin…" from the palette, and `path` typed into its dialog.
    fn import(&mut self, path: &Path) {
        let ms = self.later();
        ctrl_k(&mut self.app, ms);
        frame(&mut self.app);
        self.app
            .input(Input::Text("import skin".to_owned()), at(ms + 5));
        frame(&mut self.app);
        key(&mut self.app, Key::Enter, ms + 10);
        frame(&mut self.app);
        assert_eq!(
            self.asked().as_deref(),
            Some("Folder or .zip file of the skin")
        );
        let text = path.to_str().expect("a UTF-8 path").to_owned();
        let ms = self.later();
        let field = bounds(
            &mut self.app,
            Role::TextInput,
            "Folder or .zip file of the skin",
        );
        click_at(&mut self.app, field, ms);
        frame(&mut self.app);
        self.app.input(Input::Text(text), at(ms + 5));
        frame(&mut self.app);
        self.press("OK");
    }
}

#[test]
fn an_import_is_shown_before_it_is_installed_and_then_offered() {
    let mut case = Case::new();
    case.import(&case.source("1.0.0"));
    let card = case.asked().expect("what is in it");
    assert!(card.starts_with("Install: Skin: mine"), "{card}");
    assert!(card.contains("Version: 1.0.0"), "{card}");
    assert!(case.skins.folder.names().is_empty(), "not yet");

    case.press("Install");
    assert_eq!(case.skins.folder.names(), ["mine"]);
    assert_eq!(
        case.asked().as_deref(),
        Some("The skin mine is installed. Use it now?")
    );
    assert_eq!(case.app.on_screen, DEFAULT);
    case.press("Yes");
    assert_eq!(case.app.on_screen, "mine");
    assert_eq!(case.skins.chosen().as_deref(), Some("mine"));
    assert_eq!(case.asked(), None);

    // Replacing the skin on screen draws the new one, with no further question.
    case.import(&case.source("2.0.0"));
    let card = case.asked().expect("what is in it");
    assert!(card.starts_with("Replace: "), "{card}");
    assert!(
        card.contains("Replaces the installed version 1.0.0"),
        "{card}"
    );
    case.press("Replace");
    assert_eq!(case.asked(), None);
    assert_eq!(case.app.on_screen, "mine");
    assert_eq!(
        case.skins.folder.installed_version("mine").as_deref(),
        Some("2.0.0")
    );
}

#[test]
fn cancelling_installs_nothing() {
    let mut case = Case::new();
    case.import(&case.source("1.0.0"));
    let ms = case.later();
    key(&mut case.app, Key::Escape, ms);
    assert_eq!(case.asked(), None);
    assert!(case.skins.folder.names().is_empty());
}

#[test]
fn a_press_too_soon_does_not_install() {
    let mut case = Case::new();
    case.import(&case.source("1.0.0"));
    // The second half of a double press: the card has only just appeared.
    let point = bounds(&mut case.app, Role::Button, "Install");
    click_at(&mut case.app, point, case.ms + 10);
    frame(&mut case.app);
    assert!(case.asked().is_some_and(|c| c.starts_with("Install: ")));
    assert!(case.skins.folder.names().is_empty());
}

#[test]
fn a_path_of_nothing_is_asked_for_again() {
    let mut case = Case::new();
    case.import(Path::new("\"\""));
    let why = case.asked().expect("an error screen");
    assert!(
        why.contains("Type the path of a folder or .zip file"),
        "{why}"
    );
}

#[test]
fn a_title_that_reorders_text_is_shown_without_it() {
    let mut case = Case::new();
    let source = case.source("1.0.0");
    let manifest = source.join("skin.kdl");
    let text = std::fs::read_to_string(&manifest).expect("the manifest");
    // Written as escapes: KDL refuses the characters themselves.
    let text = text.replace("\"Default\"", "\"Good\\u{202e}\\u{200b}skin\"");
    std::fs::write(manifest, text).expect("a write");
    case.import(&source);
    let card = case.asked().expect("what is in it");
    assert!(card.contains("Good  skin"), "{card:?}");
    assert!(!card.contains(['\u{202e}', '\u{200b}']), "{card:?}");
}

#[test]
fn a_refused_import_says_nothing_of_the_path() {
    let mut case = Case::new();
    let source = case.tmp.path().join("odd-name-7f3a");
    case.import(&source);
    assert!(
        case.asked()
            .is_some_and(|why| why.starts_with("The skin was not"))
    );
    let said = SAID.lock().expect("what was said").join("\n");
    assert!(said.contains("gui: skin import refused"), "{said}");
    assert!(!said.contains("odd-name-7f3a"), "{said}");
}

#[test]
fn a_refused_source_says_why_and_installs_nothing() {
    let mut case = Case::new();
    let source = case.source("1.0.0");
    std::fs::write(source.join("README.md"), "hello").expect("a write");
    case.import(&source);
    let why = case.asked().expect("an error screen");
    assert!(why.starts_with("The skin was not imported"), "{why}");
    assert!(why.contains("README.md"), "{why}");
    assert!(case.skins.folder.names().is_empty());
}
