//! D3DMetal consent (#71, docs/19 §3.2 and §6.2, design 12 D9). D3DMetal is
//! Apple's Game Porting Toolkit translator (D3D11/12 → Metal, 64-bit only).
//! Its licence (EA18380, §2A(iii)/§2C) allows non-commercial distribution
//! of the Framework with Apple's notices, so gogo may fetch it, but only once
//! the user agreed. The answer is asked once and kept in gogo's config as
//! `[mac_app] d3dmetal = "yes"|"no"`:
//!
//! - stored answer: used as is, never asked again;
//! - no answer, a terminal: ask (default no), store the answer;
//! - no answer, headless: no download, a warning, and the caller falls back.
//!
//! It is only asked for 64-bit games that need it (D3D12, or an explicit
//! `--renderer d3dmetal`); a 32-bit game never gets the question.

use std::path::{Path, PathBuf};

use crate::config::{Config, Consent};
use crate::engine::events::ProgressEvent;

use super::error::MacAppError;
use super::prompt::{PromptAnswer, PromptFn, PromptKind, PromptSpec};

/// The config key, as the user writes it.
pub const KEY: &str = "[mac_app] d3dmetal";

/// The config file the answer lives in (`GOGO_CONFIG`, else the default).
pub fn config_path() -> PathBuf {
    Config::path()
}

/// The stored answer, if any. An unreadable config or a value other than
/// `"yes"`/`"no"` counts as no answer.
pub fn stored() -> Option<Consent> {
    stored_at(&config_path())
}

/// May gogo use D3DMetal? Only with a stored yes; never prompts.
pub fn available() -> bool {
    stored() == Some(Consent::Yes)
}

fn stored_at(path: &Path) -> Option<Consent> {
    let raw = std::fs::read_to_string(path).ok()?;
    let table: toml::Table = raw.parse().ok()?;
    match table.get("mac_app")?.get("d3dmetal")?.as_str()? {
        "yes" => Some(Consent::Yes),
        "no" => Some(Consent::No),
        _ => None,
    }
}

/// Write the answer into the config, keeping everything else in the file.
pub fn store(answer: Consent) -> Result<(), MacAppError> {
    store_at(&config_path(), answer)
}

fn store_at(path: &Path, answer: Consent) -> Result<(), MacAppError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let mut doc: toml_edit::DocumentMut = raw.parse().map_err(|e| MacAppError::Compat {
        path: path.to_path_buf(),
        reason: format!("{e}"),
    })?;
    if !doc.contains_table("mac_app") {
        doc["mac_app"] = toml_edit::table();
    }
    doc["mac_app"]["d3dmetal"] = toml_edit::value(answer.as_str());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, doc.to_string())?;
    Ok(())
}

/// The question. `why` names the game's need ("Neo Cab uses Direct3D 12").
pub fn prompt_spec(why: &str) -> PromptSpec {
    PromptSpec {
        kind: PromptKind::Confirm { default: false },
        title: format!(
            "{why}, which needs D3DMetal from Apple's Game Porting Toolkit. It is \
             licensed by Apple (Game Porting Toolkit licence EA18380: \
             non-commercial use, Apple's notices kept) and fetched from \
             github.com/Sikarugir-App/Template (96 MB download, 24 MB kept). \
             Download it? (asked once; saved as {KEY} in {})",
            config_path().display()
        ),
        items: vec![],
        allow_other: false,
        allow_none: false,
        default: 0,
    }
}

/// Ask once (see the module docs). `true`: D3DMetal may be downloaded and
/// used. A cancelled prompt stores nothing and counts as no.
pub fn ensure(why: &str, prompt: Option<&PromptFn>, emit: &mut dyn FnMut(&ProgressEvent)) -> bool {
    ensure_at(&config_path(), why, prompt, emit)
}

fn ensure_at(
    path: &Path,
    why: &str,
    prompt: Option<&PromptFn>,
    emit: &mut dyn FnMut(&ProgressEvent),
) -> bool {
    if let Some(answer) = stored_at(path) {
        return answer == Consent::Yes;
    }
    let Some(prompt) = prompt else {
        emit(&ProgressEvent::Warn {
            message: format!(
                "{why}, which needs D3DMetal (Apple Game Porting Toolkit licence). \
                 Not downloading it without your consent: run once on a terminal to \
                 be asked, or set {KEY} = \"yes\" in {}. Falling back meanwhile",
                path.display()
            ),
        });
        return false;
    };
    let answer = match prompt(&prompt_spec(why)) {
        Some(PromptAnswer::Confirmed(true)) => Consent::Yes,
        Some(PromptAnswer::Confirmed(false)) => Consent::No,
        _ => return false,
    };
    if let Err(e) = store_at(path, answer) {
        emit(&ProgressEvent::Warn {
            message: format!("could not save {KEY} in {}: {e}", path.display()),
        });
    }
    answer == Consent::Yes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn warns(events: &[ProgressEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                ProgressEvent::Warn { message } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    /// A TTY fake that answers `answer` and counts how often it was asked.
    fn fake(
        answer: Option<bool>,
        asked: &Arc<AtomicUsize>,
    ) -> impl Fn(&PromptSpec) -> Option<PromptAnswer> + Send + Sync + 'static {
        let asked = asked.clone();
        move |spec| {
            asked.fetch_add(1, Ordering::SeqCst);
            assert_eq!(spec.kind, PromptKind::Confirm { default: false });
            assert!(spec.title.contains("Apple"), "{}", spec.title);
            assert!(spec.title.contains("licen"), "{}", spec.title);
            answer.map(PromptAnswer::Confirmed)
        }
    }

    #[test]
    fn tty_yes_is_stored_and_not_asked_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# mine\n[download]\nworkers = 4\n").unwrap();
        let asked = Arc::new(AtomicUsize::new(0));
        let p = fake(Some(true), &asked);
        let mut ev = vec![];
        assert!(ensure_at(&path, "X uses Direct3D 12", Some(&p), &mut |e| {
            ev.push(e.clone())
        }));
        assert!(ensure_at(&path, "X uses Direct3D 12", Some(&p), &mut |e| {
            ev.push(e.clone())
        }));
        assert_eq!(asked.load(Ordering::SeqCst), 1, "asked once");
        assert!(warns(&ev).is_empty());
        assert_eq!(stored_at(&path), Some(Consent::Yes));
        // The rest of the file is kept, and the result loads as gogo config.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains("# mine") && raw.contains("workers = 4"),
            "{raw}"
        );
        let loaded = Config::load_from(&path, |_| None).unwrap();
        assert_eq!(loaded.config.mac_app.d3dmetal, Some(Consent::Yes));
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn tty_no_is_stored_and_respected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/config.toml");
        let asked = Arc::new(AtomicUsize::new(0));
        let p = fake(Some(false), &asked);
        assert!(!ensure_at(&path, "X", Some(&p), &mut |_| {}));
        assert!(!ensure_at(&path, "X", Some(&p), &mut |_| {}));
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert_eq!(stored_at(&path), Some(Consent::No));
    }

    #[test]
    fn cancel_stores_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let asked = Arc::new(AtomicUsize::new(0));
        let p = fake(None, &asked);
        assert!(!ensure_at(&path, "X", Some(&p), &mut |_| {}));
        assert_eq!(stored_at(&path), None);
        assert!(!path.exists());
    }

    #[test]
    fn headless_without_answer_warns_and_declines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut ev = vec![];
        assert!(!ensure_at(&path, "X uses Direct3D 12", None, &mut |e| ev
            .push(e.clone())));
        let w = warns(&ev);
        assert_eq!(w.len(), 1);
        assert!(
            w[0].contains("Not downloading") && w[0].contains(KEY),
            "{}",
            w[0]
        );
        assert!(!path.exists(), "headless writes nothing");
    }

    #[test]
    fn stored_answer_wins_headless_and_on_a_tty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let asked = Arc::new(AtomicUsize::new(0));
        let p = fake(Some(false), &asked);
        std::fs::write(&path, "[mac_app]\nd3dmetal = \"yes\"\n").unwrap();
        let mut ev = vec![];
        assert!(ensure_at(&path, "X", None, &mut |e| ev.push(e.clone())));
        assert!(ensure_at(&path, "X", Some(&p), &mut |e| ev.push(e.clone())));
        std::fs::write(&path, "[mac_app]\nd3dmetal = \"no\"\n").unwrap();
        assert!(!ensure_at(&path, "X", None, &mut |e| ev.push(e.clone())));
        assert!(!ensure_at(&path, "X", Some(&p), &mut |e| ev.push(e.clone())));
        assert_eq!(asked.load(Ordering::SeqCst), 0, "never asked");
        assert!(warns(&ev).is_empty());
        // Anything else is no answer.
        std::fs::write(&path, "[mac_app]\nd3dmetal = \"maybe\"\n").unwrap();
        assert_eq!(stored_at(&path), None);
    }
}
