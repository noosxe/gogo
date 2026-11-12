//! `gogo linux-app move` (design 18 §6.1): the Linux side of
//! [`crate::macapp::relocate`].
//!
//! What a moved prefix needs, read from real ones (#278). Nothing in it:
//!
//! - Its absolute symlinks (about 1300 in `drive_c/windows` and `Program
//!   Files`) lead to the Proton build, which doesn't move, and `dosdevices/z:`
//!   to `/`. None leads into the prefix.
//! - Its links into itself are relative: `pfx`, `dosdevices/c:`, the
//!   `steamuser` ones, and `drive_c/GOG/<name>` to `../../../game`.
//! - The registry holds `C:` paths, and `Z:` only for what's in Proton's own
//!   build or the container.
//! - `config_info` names the Proton build. Its `version` and `tracked_files`
//!   hold no paths.
//!
//! So the installer lane has always been moving prefixes: it renames one out of
//! `.build-<slug>-<pid>/`. A game that wrote its own path into the registry
//! would be the exception, and [`prefix_mentions`] warns about it rather than
//! rewriting what the game owns.

use std::path::Path;

use super::game::{self, LAUNCH_TOML};
use super::{launcher, setup};
use crate::macapp::error::MacAppError;
use crate::macapp::launch::{GameDirs, windows_to_unix, write_atomic};
use crate::macapp::relocate::{self, Lane, MoveEvent, MoveOptions, MoveOutcome, MovePlan, Target};

/// The Linux lane's part of a move.
#[derive(Debug, Clone, Copy, Default)]
pub struct LinuxLane;

impl Lane for LinuxLane {
    fn check_built(&self, gogid: &str, dirs: &GameDirs) -> Result<(), MacAppError> {
        if dirs.root.join(LAUNCH_TOML).is_file() {
            return Ok(());
        }
        Err(MacAppError::Apps(format!(
            "{} has no {LAUNCH_TOML}: {gogid} isn't a `gogo linux-app` game",
            dirs.root.display()
        )))
    }

    fn check_target(&self, root: &Path) -> Result<(), MacAppError> {
        game::refuse_private_tmp("the game's new dir", root)
    }

    fn built_id(&self, root: &Path) -> Option<String> {
        game::read_launch_toml(&GameDirs::at(root.to_path_buf()))
            .ok()
            .map(|(toml, _)| toml.gogid)
    }

    fn check_rewritten(&self, to: &GameDirs) -> Result<(), MacAppError> {
        let Ok(text) = std::fs::read_to_string(&to.launch_conf) else {
            return Ok(());
        };
        let conf = game::ConfValues::parse(&text);
        let data_root = crate::state::data_root();
        for key in ["PREFIX", "GAME_DIR"] {
            let Some(v) = conf.values.get(key) else {
                continue;
            };
            let at = data_root.join(v);
            if !at.starts_with(&to.root) {
                return Err(MacAppError::Library {
                    path: to.root.clone(),
                    reason: format!(
                        "its launch.conf names {} ({key}), another place than this: an earlier \
                         adopt probably stopped halfway. Adopt the dir it names first, or put \
                         this one there",
                        at.display()
                    ),
                });
            }
        }
        Ok(())
    }

    fn refuse_running(&self, gogid: &str, dirs: &GameDirs) -> Result<(), MacAppError> {
        game::refuse_running(gogid, &dirs.root, &dirs.prefix)
    }

    fn rewrite_inside(
        &self,
        from: &GameDirs,
        to: &GameDirs,
        at: &Path,
    ) -> Result<Vec<String>, MacAppError> {
        let mut warnings = Vec::new();
        let conf = at.join("launch.conf");
        match std::fs::read_to_string(&conf) {
            Ok(text) => {
                let data_root = crate::state::data_root();
                let new = game::relocated_conf(&text, &from.root, &to.root, &data_root);
                if new != text {
                    write_atomic(&conf, &new)?;
                }
                let old = from.root.to_string_lossy();
                if new.contains(old.as_ref()) {
                    warnings.push(format!(
                        "launch.conf still names {old} in the game's args or env, which gogo \
                         leaves as you set them: see `gogo linux-app env list {id}` and `args`",
                        id = from.root.file_name().unwrap_or_default().to_string_lossy()
                    ));
                }
            }
            // Built, never set up: the first launch writes it, for the new place.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let prefix = at.join("prefix");
        for file in prefix_mentions(&prefix, &from.root) {
            warnings.push(format!(
                "{file} in the prefix records the game's old path ({}): the game wrote it, so \
                 gogo left it; if the game misbehaves, its own settings may need the new path",
                from.root.display()
            ));
        }
        Ok(warnings)
    }

    /// `launch.sh`, `icon.png` and the menu entry, through setup's writer. No
    /// setup: it could need the network or Wine, and the game has already moved.
    /// Everything setup would write differently is in `launch.conf`, and its
    /// stamp is about the config, not where the game is.
    fn publish(&self, gogid: &str, dirs: &GameDirs) -> Result<(), MacAppError> {
        let (baked, _) = game::read_launch_toml(dirs)?;
        let files = setup::game_files(dirs, baked.tree_name().as_deref());
        let exe = windows_to_unix(&dirs.prefix, &baked.exe);
        launcher::write(dirs, gogid, &baked.title, Some(&files), exe.as_deref())?;
        Ok(())
    }
}

/// The registry files in `prefix` that name `root`, as a unix path or through
/// `Z:` (case aside: Wine's drive letters don't keep it).
fn prefix_mentions(prefix: &Path, root: &Path) -> Vec<&'static str> {
    let unix = root.to_string_lossy().to_ascii_lowercase();
    let dos = format!("z:{}", unix.replace('/', "\\\\"));
    ["system.reg", "user.reg"]
        .into_iter()
        .filter(|file| {
            std::fs::read(prefix.join(file)).is_ok_and(|bytes| {
                let text = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
                text.contains(&unix) || text.contains(&dos)
            })
        })
        .collect()
}

/// A DLC add in progress or left over (design 19 §3.2, §3.4) holds files in the game
/// dir a move must not split: its journal, its download record, its staging dir.
fn refuse_dlc_add(gogid: &str) -> Result<(), MacAppError> {
    let root = GameDirs::for_game(gogid)?.root;
    let what = [
        crate::gog::lay::JOURNAL,
        super::dlc::DOWNLOAD,
        crate::gog::lay::STAGING,
    ]
    .into_iter()
    .find(|f| root.join(f).symlink_metadata().is_ok());
    match what {
        None => Ok(()),
        Some(f) => Err(MacAppError::Apps(format!(
            "a DLC add of {gogid} is in progress or didn't finish ({} is there): run `gogo \
             linux-app dlc {gogid} add` to finish it, then move the game",
            root.join(f).display()
        ))),
    }
}

/// What `gogo linux-app move` will do.
pub fn plan(gogid: &str, target: &Target, opts: &MoveOptions) -> Result<MovePlan, MacAppError> {
    crate::macapp::compat::validate_gogid(gogid)?;
    refuse_dlc_add(gogid)?;
    relocate::plan(gogid, target, &LinuxLane, opts)
}

/// `gogo linux-app move`.
pub fn move_game(
    gogid: &str,
    target: &Target,
    opts: &MoveOptions,
    emit: &mut dyn FnMut(&MoveEvent),
) -> Result<MoveOutcome, MacAppError> {
    crate::macapp::compat::validate_gogid(gogid)?;
    // A snapshot rollback or conversion that stopped is settled first
    // (design 20 §5.3.1, §2.1).
    let settled = super::rollback::settle_pending(gogid, &super::snapshots::Backends::real())?;
    refuse_dlc_add(gogid)?;
    let mut out = relocate::move_game(gogid, target, &LinuxLane, opts, emit)?;
    out.recovered.splice(0..0, settled);
    Ok(out)
}

/// What `gogo linux-app move --adopt` will do.
pub fn adopt_plan(gogid: &str, target: &Target) -> Result<relocate::AdoptPlan, MacAppError> {
    relocate::adopt_plan(gogid, target, &LinuxLane)
}

/// `gogo linux-app move --adopt`: the game is at `target` now.
pub fn adopt_game(gogid: &str, target: &Target) -> Result<relocate::AdoptOutcome, MacAppError> {
    crate::macapp::compat::validate_gogid(gogid)?;
    let settled = super::rollback::settle_pending(gogid, &super::snapshots::Backends::real())?;
    let mut out = relocate::adopt_game(gogid, target, &LinuxLane)?;
    out.recovered.splice(0..0, settled);
    Ok(out)
}

/// Finish whatever interrupted moves left, before something else looks at the
/// games.
pub fn recover_all() -> Vec<String> {
    relocate::recover_all(&LinuxLane)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_is_searched_for_the_old_path_in_both_forms() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        let root = Path::new("/home/Me/games/7");
        std::fs::write(p.join("system.reg"), r#""A"="C:\\GOG\\x""#).unwrap();
        std::fs::write(
            p.join("user.reg"),
            r#""A"="Z:\\HOME\\me\\games\\7\\prefix""#,
        )
        .unwrap();
        assert_eq!(prefix_mentions(p, root), ["user.reg"]);
        std::fs::write(p.join("system.reg"), "\"A\"=\"/home/Me/games/7/x\"").unwrap();
        assert_eq!(prefix_mentions(p, root), ["system.reg", "user.reg"]);
        assert!(prefix_mentions(&p.join("none"), root).is_empty());
    }
}
