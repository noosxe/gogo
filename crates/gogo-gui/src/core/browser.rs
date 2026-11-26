//! Opening the user's browser on one of the app's own pages (design 02 §2), behind a
//! trait so that tests open nothing.

use std::process::{Command, Stdio};

pub(crate) trait Browser: Send + Sync {
    /// Opens `url`, which the app built itself. Returns at once; `false` when nothing
    /// could be started.
    fn open(&self, url: &str) -> bool;
}

pub(crate) struct SystemBrowser;

impl Browser for SystemBrowser {
    fn open(&self, url: &str) -> bool {
        let program = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let started = Command::new(program)
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match started {
            Ok(mut child) => {
                // Reaped when it ends, however long that takes.
                std::thread::spawn(move || child.wait());
                true
            }
            Err(_) => false,
        }
    }
}
