//! The Linux lane (design 18): Windows games through GE-Proton and
//! umu-launcher. It shares the runtime catalog and store with `macapp`
//! (design 12); what is Linux-only lives here.

use std::path::Path;

use crate::macapp::error::MacAppError;

pub mod build;
pub mod catalogue;
pub mod display;
pub mod dlc;
pub mod doctor;
pub mod envargs;
pub mod fixes;
pub mod game;
pub mod icon;
pub mod launcher;
pub mod manage;
pub mod proton;
pub mod protonfix;
pub mod relocate;
pub mod setup;
pub mod umu;
pub mod umu_id;
pub mod vdf;

/// A `[linux] <key>` string in the config at `path`, if set. An unreadable
/// config counts as unset: the ask-once keys are asked again, never fatal.
pub fn stored_key(path: &Path, key: &str) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let table: toml::Table = raw.parse().ok()?;
    Some(table.get("linux")?.get(key)?.as_str()?.to_string())
}

/// Write `[linux] <key> = "<value>"` into the config, keeping everything
/// else in the file (comments included).
pub fn store_key(path: &Path, key: &str, value: &str) -> Result<(), MacAppError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let mut doc: toml_edit::DocumentMut = raw.parse().map_err(|e| MacAppError::Compat {
        path: path.to_path_buf(),
        reason: format!("{e}"),
    })?;
    if !doc.contains_table("linux") {
        doc["linux"] = toml_edit::table();
    }
    doc["linux"][key] = toml_edit::value(value);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, doc.to_string())?;
    Ok(())
}
