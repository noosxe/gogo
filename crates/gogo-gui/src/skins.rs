//! The installed skins, and which one is chosen (design 25 §9, design 27 §10).

use std::fs::{self, File};
use std::io::{self, Read as _, Write as _};
use std::path::PathBuf;

use gogo_skin::SkinFolder;

/// The built-in skin's name, and the choice when there is no other.
pub(crate) const DEFAULT: &str = "default";

/// The file in the GUI's state folder that holds the chosen skin's name.
const CHOICE: &str = "active-skin";

/// The skin folder and where the choice is kept.
#[derive(Debug, Clone)]
pub(crate) struct Skins {
    pub(crate) folder: SkinFolder,
    state: PathBuf,
}

impl Skins {
    pub(crate) fn new(folder: impl Into<PathBuf>, state: impl Into<PathBuf>) -> Skins {
        Skins {
            folder: SkinFolder::new(folder.into()),
            state: state.into(),
        }
    }

    /// The places of design 25 §9, under `GOGO_HOME` when that is set. A `fictional` run
    /// has them only then, so that it never reads or changes the real ones.
    pub(crate) fn resolve(fictional: bool) -> Option<Skins> {
        let moved = std::env::var_os("GOGO_HOME").is_some_and(|h| !h.is_empty());
        (moved || !fictional).then(|| {
            Skins::new(
                gogo::state::data_root().join("skins"),
                gogo::state::state_dir().join("gui"),
            )
        })
    }

    /// What can be chosen: the built-in skin, then the installed ones.
    pub(crate) fn choices(&self) -> Vec<String> {
        let mut choices = vec![DEFAULT.to_owned()];
        choices.extend(self.folder.names());
        choices
    }

    /// The name chosen before, installed or not. `None` when there is none that is shaped
    /// like a skin's name.
    pub(crate) fn chosen(&self) -> Option<String> {
        let mut text = String::new();
        File::open(self.state.join(CHOICE))
            .and_then(|f| f.take(128).read_to_string(&mut text))
            .ok()?;
        let name = text.trim();
        named(name).then(|| name.to_owned())
    }

    /// Keeps `name` as the choice, written atomically. `Ok` means it is kept: a folder
    /// that can't be synced afterwards is only logged.
    pub(crate) fn choose(&self, name: &str) -> io::Result<()> {
        fs::create_dir_all(&self.state)?;
        let new = self.state.join(format!(".{CHOICE}.{}", std::process::id()));
        let written = File::create(&new)
            .and_then(|mut file| {
                writeln!(file, "{name}")?;
                file.sync_all()
            })
            .and_then(|()| fs::rename(&new, self.state.join(CHOICE)));
        if let Err(e) = written {
            // There may be nothing to remove.
            let _ = fs::remove_file(&new);
            return Err(e);
        }
        if let Err(e) = File::open(&self.state).and_then(|dir| dir.sync_all()) {
            tracing::warn!("the skin choice is saved, but its folder was not synced: {e}");
        }
        Ok(())
    }
}

/// Whether `name` is shaped like a skin's name (design 27 §2): safe to show and to keep.
pub(crate) fn named(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_choice_is_kept_and_only_a_name_is_read_back() {
        let tmp = tempfile::tempdir().expect("a temporary folder");
        let skins = Skins::new(tmp.path().join("skins"), tmp.path().join("gui"));
        assert_eq!(skins.chosen(), None);
        assert_eq!(skins.choices(), [DEFAULT]);

        skins.choose("mine").expect("a write");
        assert_eq!(skins.chosen().as_deref(), Some("mine"));
        assert_eq!(fs::read_dir(&skins.state).expect("the folder").count(), 1);

        // A write that fails leaves nothing behind.
        let kept = skins.state.join(CHOICE);
        fs::remove_file(&kept).expect("removed");
        fs::create_dir_all(kept.join("in-the-way")).expect("folders");
        assert!(skins.choose("mine").is_err());
        assert_eq!(fs::read_dir(&skins.state).expect("the folder").count(), 1);
        fs::remove_dir_all(&kept).expect("removed");

        for bad in ["../mine", "a/b", "Mine", ""] {
            fs::write(skins.state.join(CHOICE), bad).expect("a write");
            assert_eq!(skins.chosen(), None, "{bad:?}");
        }
    }
}
