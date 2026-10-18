//! Silent GOG installer invocation (`docs/design/10` §2.3) — GOG Windows
//! installers are Inno Setup packages (docs/13 §2): `/VERYSILENT
//! /SUPPRESSMSGBOXES /NORESTART /DIR=` runs the whole wizard unattended.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::error::MacAppError;
use super::runtime::Runtime;

/// The validated installer + its multi-part `.bin` siblings (13 §2: Inno picks
/// siblings up automatically as long as they sit in the same directory).
#[derive(Debug, Clone)]
pub struct InstallPlan {
    pub installer: PathBuf,
    /// Same-dir `.bin` parts detected for the plan summary (not passed to wine —
    /// Inno finds them itself).
    pub bin_parts: Vec<PathBuf>,
}

/// Validate the installer path and detect sibling `.bin` parts (no I/O beyond
/// stat; the actual run happens in [`run_silent`]).
pub fn plan(installer: &Path) -> Result<InstallPlan, MacAppError> {
    let installer = installer.canonicalize()?;
    if !installer.is_file() {
        return Err(MacAppError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("installer not found: {}", installer.display()),
        )));
    }
    let stem = installer
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let dir = installer.parent().unwrap_or(Path::new("."));
    let mut bin_parts = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            // GOG convention: `<stem>.bin`, `<stem>-1.bin`, `<stem>-2.bin`, …
            let is_part = name == format!("{stem}.bin")
                || (name.starts_with(&format!("{stem}-")) && name.ends_with(".bin"));
            if is_part {
                bin_parts.push(entry.path());
            }
        }
    }
    bin_parts.sort();
    Ok(InstallPlan {
        installer,
        bin_parts,
    })
}

/// Run the installer silently into `prefix`, installing to `win_target`
/// (e.g. `C:\GOG\heroes3-gold`). Fail loud on non-zero exit (design 10 §2.3).
pub fn run_silent(
    runtime: &Runtime,
    plan: &InstallPlan,
    prefix: &Path,
    win_target: &str,
    bundle: &Path,
) -> Result<(), MacAppError> {
    let status = Command::new(&runtime.wine)
        .arg(&plan.installer)
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
        .arg(format!("/DIR={win_target}"))
        .env("WINEPREFIX", prefix)
        .status()
        .map_err(|e| MacAppError::RuntimeInvalid {
            path: runtime.wine.clone(),
            reason: format!("cannot execute wine for install: {e}"),
        })?;
    match status.code() {
        Some(0) => Ok(()),
        Some(code) => Err(MacAppError::InstallerFailed {
            exit: code,
            bundle: bundle.to_path_buf(),
        }),
        None => Err(MacAppError::InstallerFailed {
            exit: -1,
            bundle: bundle.to_path_buf(),
        }),
    }
}

/// Initialize a fresh WINEPREFIX (`wine wineboot -u`, design 10 §2.2).
pub fn stage_prefix(runtime: &Runtime, prefix: &Path) -> Result<(), MacAppError> {
    std::fs::create_dir_all(prefix)?;
    let status = Command::new(&runtime.wine)
        .args(["wineboot", "-u"])
        .env("WINEPREFIX", prefix)
        .status()
        .map_err(|e| MacAppError::RuntimeInvalid {
            path: runtime.wine.clone(),
            reason: format!("cannot execute wine for wineboot: {e}"),
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(MacAppError::RuntimeInvalid {
            path: runtime.wine.clone(),
            reason: format!("wineboot -u exited {}", status.code().unwrap_or(-1)),
        })
    }
}

/// Terminate the prefix's wineserver and give it a moment to die.
///
/// Live findings 2026-10-18: `wine <installer>` returns while its wineserver
/// stays alive and keeps writing into the prefix — which broke `codesign
/// --verify` mid-seal ("a sealed resource is missing or invalid"). Worse,
/// `wineserver -p` means "persistent mode", not "ping": probing with it
/// daemonizes a never-exiting server that inherits (and holds) the caller's
/// stdio pipes. So: `-k` only, null stdio, hard timeout, short settle sleep.
pub fn stop_wineserver(runtime: &Runtime, prefix: &Path) -> Result<(), MacAppError> {
    let server = runtime.root.join("bin/wineserver");
    if !server.is_file() {
        return Ok(());
    }
    let _ = run_with_timeout(
        Command::new(&server).arg("-k").env("WINEPREFIX", prefix),
        Duration::from_secs(5),
    );
    std::thread::sleep(Duration::from_millis(500));
    Ok(())
}

/// Spawn, wait up to `timeout`, kill on expiry. Stdio is nulled so background
/// daemons can never inherit and hold the caller's pipes open.
fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Option<std::process::ExitStatus> {
    cmd.stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod installer_tests {
    use super::*;

    #[test]
    fn plan_detects_bin_siblings() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("setup_game_1.0.exe"), b"MZ").unwrap();
        std::fs::write(dir.join("setup_game_1.0-1.bin"), b"part1").unwrap();
        std::fs::write(dir.join("setup_game_1.0-2.bin"), b"part2").unwrap();
        std::fs::write(dir.join("setup_other.bin"), b"unrelated").unwrap();

        let plan = plan(&dir.join("setup_game_1.0.exe")).unwrap();
        assert_eq!(plan.bin_parts.len(), 2, "{:?}", plan.bin_parts);
        assert!(plan.bin_parts[0].ends_with("setup_game_1.0-1.bin"));
        assert!(plan.bin_parts[1].ends_with("setup_game_1.0-2.bin"));
    }

    #[test]
    fn plan_fails_on_missing_installer() {
        let tmp = tempfile::tempdir().unwrap();
        let err = plan(&tmp.path().join("nope.exe")).unwrap_err();
        assert!(matches!(err, MacAppError::Io(_)));
    }

    #[test]
    fn plan_is_noop_without_siblings() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("setup_solo.exe"), b"MZ").unwrap();
        let plan = plan(&tmp.path().join("setup_solo.exe")).unwrap();
        assert!(plan.bin_parts.is_empty());
    }
}
