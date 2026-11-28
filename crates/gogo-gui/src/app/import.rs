//! Importing a skin from the window (design 25 §9, design 27 §10): the app asks for a
//! path, shows what is in it, and installs it once that is confirmed. The questions are
//! the app's own dialogs, with the safety screens' rules; no skin can start or answer
//! one.

use std::path::PathBuf;

use gogo_gui_model::{ErrorView, PromptAnswer, PromptSpec, Ticket};
use gogo_skin::import::is_hidden;
use gogo_skin::{Candidate, ImportError};

use super::{App, say};

/// The most characters of a skin's title shown.
const TITLE: usize = 80;
/// The most warnings shown before installing.
const WARNINGS: usize = 3;

/// Where an import is.
pub(super) enum Step {
    /// Asked for the folder or zip.
    Path,
    /// Showing what was read; `replace` when a skin of its name is installed.
    Install {
        candidate: Box<Candidate>,
        replace: bool,
    },
    /// Installed, and asked whether to draw it.
    Use { name: String },
}

impl App {
    /// "Import skin…".
    pub(super) fn start_import(&mut self) {
        let spec = PromptSpec::Text {
            label: "Folder or .zip file of the skin".to_owned(),
            default: String::new(),
        };
        self.ask_to_import(Step::Path, spec);
    }

    fn ask_to_import(&mut self, step: Step, spec: PromptSpec) {
        match self.data.ask(spec) {
            Some(ticket) => self.importing = Some((ticket, step)),
            // A job's question can come in between two of ours.
            None => {
                self.import_failed("Another question came first. Import the skin again.".to_owned())
            }
        }
    }

    pub(super) fn import_answered(&mut self, ticket: Ticket, answer: PromptAnswer) {
        let Some((_, step)) = self.importing.take_if(|(t, _)| *t == ticket) else {
            return;
        };
        match (step, answer) {
            (Step::Path, PromptAnswer::Text(typed)) => self.read_skin(&typed),
            (Step::Install { candidate, replace }, PromptAnswer::Yes) => {
                self.install_skin(&candidate, replace);
            }
            (Step::Use { name }, PromptAnswer::Yes) => self.set_skin(&name),
            _ => {}
        }
    }

    fn read_skin(&mut self, typed: &str) {
        let Some(path) = typed_path(typed) else {
            self.import_failed("Type the path of a folder or .zip file".to_owned());
            return;
        };
        let Some(skins) = &self.skins else { return };
        match skins.folder.read(&path) {
            Ok(candidate) => {
                let installed = skins.folder.installed_version(candidate.name());
                let replace = installed.is_some();
                let spec = PromptSpec::Offer {
                    label: card(&candidate, installed.as_deref()),
                    yes: if replace { "Replace" } else { "Install" },
                };
                let candidate = Box::new(candidate);
                self.ask_to_import(Step::Install { candidate, replace }, spec);
            }
            Err(e) => self.import_refused(&e),
        }
    }

    fn install_skin(&mut self, candidate: &Candidate, replace: bool) {
        let Some(skins) = &self.skins else { return };
        match skins.folder.install(candidate, replace) {
            Ok(None) => {}
            Ok(Some(e)) => {
                tracing::warn!("the skin is installed, but its folder was not synced: {e}");
            }
            Err(e) => {
                self.import_refused(&e);
                return;
            }
        }
        let name = candidate.name().to_owned();
        say(format_args!("gui: skin imported {name}"));
        if name == self.on_screen {
            // The skin on screen was replaced: draw what is there now.
            self.set_skin(&name);
            return;
        }
        self.offer_skins();
        let spec = PromptSpec::Confirm {
            label: format!("The skin {name} is installed. Use it now?"),
        };
        self.ask_to_import(Step::Use { name }, spec);
    }

    fn import_refused(&mut self, error: &ImportError) {
        // The lines can hold names from the source, so they are shown and not logged.
        say(format_args!("gui: skin import refused"));
        self.import_failed(error.lines().join("\n"));
    }

    fn import_failed(&mut self, why: String) {
        let title = "The skin was not imported";
        self.data.failures().report(ErrorView::failed(title, why));
        self.data.pump();
    }
}

/// The path as typed or pasted: surrounding spaces and quotes dropped, `~/` expanded.
fn typed_path(typed: &str) -> Option<PathBuf> {
    let typed = typed.trim();
    let unquoted = ['"', '\'']
        .iter()
        .find_map(|&q| typed.strip_prefix(q)?.strip_suffix(q))
        .unwrap_or(typed);
    if unquoted.is_empty() {
        return None;
    }
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty());
    Some(match (unquoted.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(unquoted),
    })
}

/// What is about to be installed. The title is the skin author's, so it is bounded,
/// quoted, kept to one line and shown without what hides or reorders text.
fn card(candidate: &Candidate, installed: Option<&str>) -> String {
    let manifest = &candidate.manifest;
    let title: String = manifest
        .title
        .chars()
        .map(|c| if is_hidden(c) { ' ' } else { c })
        .take(TITLE)
        .collect();
    let mut card = format!(
        "Skin: {}\nTitle: \u{201c}{title}\u{201d}\nVersion: {}",
        manifest.name, manifest.version
    );
    if let Some(old) = installed {
        card += &format!("\nReplaces the installed version {old}.");
    }
    for warning in candidate.warnings.iter().take(WARNINGS) {
        card += &format!("\nWarning: {warning}");
    }
    let more = candidate.warnings.len().saturating_sub(WARNINGS);
    if more > 0 {
        card += &format!("\n{more} more warnings.");
    }
    card
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_path_loses_its_spaces_and_quotes() {
        for typed in [" /a b.zip ", "\"/a b.zip\"", "'/a b.zip'\n"] {
            assert_eq!(
                typed_path(typed),
                Some(PathBuf::from("/a b.zip")),
                "{typed:?}"
            );
        }
        assert_eq!(typed_path("  "), None);
        assert_eq!(typed_path("\"\""), None);
        assert_eq!(typed_path("a.zip"), Some(PathBuf::from("a.zip")));
    }
}
