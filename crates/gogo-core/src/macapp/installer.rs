//! Silent GOG installer invocation (`docs/design/10` §2.3) — GOG Windows
//! installers are Inno Setup packages (docs/13 §2): `/VERYSILENT
//! /SUPPRESSMSGBOXES /NORESTART /DIR=` runs the whole wizard unattended.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::error::MacAppError;
use super::runtime::{Runtime, wine_cmd};

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
///
/// Everything goes to `log_dir` (#56): Inno's own `/LOG` as [`INNO_LOG`] (it
/// names the step that failed, which the exit code never does) and Wine's
/// stdout/stderr as [`WINE_LOG`], instead of the terminal. A failure carries
/// the dir and the Inno log's last lines.
pub fn run_silent(
    runtime: &Runtime,
    plan: &InstallPlan,
    prefix: &Path,
    win_target: &str,
    bundle: &Path,
    log_dir: &Path,
) -> Result<(), MacAppError> {
    let log_dir = super::launch::absolute(log_dir);
    std::fs::create_dir_all(&log_dir)?;
    let inno_log = log_dir.join(INNO_LOG);
    let wine_log = std::fs::File::create(log_dir.join(WINE_LOG))?;
    let status = wine_cmd(&runtime.wine, &runtime.wine)
        .arg(&plan.installer)
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-"])
        .arg(format!("/DIR={win_target}"))
        .arg(format!("/LOG={}", z_path(&inno_log)))
        .env("WINEPREFIX", prefix)
        .stdin(Stdio::null())
        .stdout(wine_log.try_clone()?)
        .stderr(wine_log)
        .status()
        .map_err(|e| MacAppError::RuntimeInvalid {
            path: runtime.wine.clone(),
            reason: format!("cannot execute wine for install: {e}"),
        })?;
    if status.success() {
        return Ok(());
    }
    Err(MacAppError::InstallerFailed {
        exit: status.code().unwrap_or(-1),
        bundle: bundle.to_path_buf(),
        tail: log_tail(&inno_log, 8),
        logs: log_dir,
    })
}

/// Inno Setup's own log in a [`run_silent`] log dir.
pub const INNO_LOG: &str = "inno.log";
/// Wine's stdout + stderr in a [`run_silent`] log dir.
pub const WINE_LOG: &str = "wine.log";

/// A host path as Wine sees it through `z:` (the build prefix keeps that
/// drive until `setup::localize` drops it).
pub fn z_path(path: &Path) -> String {
    format!("Z:{}", path.display()).replace('/', "\\")
}

/// The last `n` non-empty lines of `path` (Inno writes UTF-8 or the ANSI
/// code page; lossy is enough for a hint), or "" when there is no log.
fn log_tail(path: &Path, n: usize) -> String {
    let Ok(raw) = std::fs::read(path) else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&raw);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Initialize a fresh WINEPREFIX (`wine wineboot -u`, design 10 §2.2).
pub fn stage_prefix(runtime: &Runtime, prefix: &Path) -> Result<(), MacAppError> {
    std::fs::create_dir_all(prefix)?;
    let mut cmd = wine_cmd(&runtime.wine, &runtime.wine);
    cmd.args(["wineboot", "-u"]).env("WINEPREFIX", prefix);
    if runtime.is_crossover() {
        // CX engines: wineboot otherwise loops on a failing wine-mono MSI
        // install (err 1603, docs/19 §8.2 Q5). Only for staging the prefix;
        // whatever the caller already overrides is kept after it.
        let mut dlls = std::ffi::OsString::from("mscoree=");
        if let Some(old) = std::env::var_os("WINEDLLOVERRIDES").filter(|v| !v.is_empty()) {
            dlls.push(";");
            dlls.push(old);
        }
        cmd.env("WINEDLLOVERRIDES", dlls);
    }
    let status = cmd.status().map_err(|e| MacAppError::RuntimeInvalid {
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
    // Next to the wine binary: store runtimes are wrapper trees
    // (`Foo.app/Contents/Resources/wine/bin`), so `root/bin` would miss it
    // and leave the server writing into the prefix after signing (#53).
    let server = runtime.wine.with_file_name("wineserver");
    if !server.is_file() {
        return Ok(());
    }
    let _ = run_with_timeout(
        wine_cmd(&server, &runtime.wine)
            .arg("-k")
            .env("WINEPREFIX", prefix),
        Duration::from_secs(5),
    );
    std::thread::sleep(Duration::from_millis(500));
    Ok(())
}

/// Spawn, wait up to `timeout`, kill on expiry. Stdio is nulled so background
/// daemons can never inherit and hold the caller's pipes open.
pub(crate) fn run_with_timeout(
    cmd: &mut Command,
    timeout: Duration,
) -> Option<std::process::ExitStatus> {
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
    fn z_path_maps_through_the_z_drive() {
        assert_eq!(
            z_path(Path::new("/Users/me/gogo/install/inno.log")),
            r"Z:\Users\me\gogo\install\inno.log"
        );
    }

    #[test]
    fn log_tail_keeps_the_last_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("inno.log");
        std::fs::write(&log, "a\n\nb\nc\n").unwrap();
        assert_eq!(log_tail(&log, 2), "b\nc");
        assert_eq!(log_tail(&log, 9), "a\nb\nc");
        assert_eq!(log_tail(&tmp.path().join("none.log"), 3), "");
    }

    #[test]
    fn plan_is_noop_without_siblings() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("setup_solo.exe"), b"MZ").unwrap();
        let plan = plan(&tmp.path().join("setup_solo.exe")).unwrap();
        assert!(plan.bin_parts.is_empty());
    }
}
