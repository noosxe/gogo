//! The `--log-file` sink: a private, append-only file that every tracing
//! event at any level is written to, scrubbed (design 07 §4).
//!
//! The file layer wraps this in [`crate::scrub_log::Scrubbing`], so nothing
//! reaches the disk unscrubbed, and formats without ANSI escapes.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;

use tracing_subscriber::fmt::MakeWriter;

/// An open log file, shared by every event writer.
#[derive(Clone)]
pub struct LogFile(Arc<File>);

/// One event's handle on the file (`&File` writes, so no lock is needed;
/// `O_APPEND` keeps whole events from interleaving).
pub struct Handle(Arc<File>);

impl Write for Handle {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        (&*self.0).write(b)
    }

    fn flush(&mut self) -> io::Result<()> {
        (&*self.0).flush()
    }
}

impl<'a> MakeWriter<'a> for LogFile {
    type Writer = Handle;

    fn make_writer(&'a self) -> Handle {
        Handle(self.0.clone())
    }
}

impl LogFile {
    /// Open `path` for appending: created `0600`, an existing regular file is
    /// tightened to `0600`, a symlink as the last component is refused, and a
    /// missing parent directory is an error (never created).
    pub fn open(path: &Path) -> Result<LogFile, String> {
        let fail = |why: &dyn std::fmt::Display| format!("--log-file {}: {why}", path.display());
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(path)
            .map_err(|e| {
                if e.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) {
                    fail(&"is a symbolic link")
                } else {
                    fail(&e)
                }
            })?;
        let meta = file.metadata().map_err(|e| fail(&e))?;
        if !meta.is_file() {
            return Err(fail(&"is not a regular file"));
        }
        if meta.permissions().mode() & 0o177 != 0 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|e| fail(&e))?;
        }
        Ok(LogFile(Arc::new(file)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrub_log::Scrubbing;
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::SubscriberExt;

    const REFRESH: &str = "FAKE-REFRESH-4c1e9a";
    const CDN: &str = "FAKE-CDN-TOKEN-77b2";

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    /// The file layer exactly as `init_tracing` builds it.
    fn log_through(file: LogFile, f: impl FnOnce()) {
        let sub = tracing_subscriber::registry().with(crate::file_layer(file));
        tracing::subscriber::with_default(sub, f);
    }

    #[test]
    fn planted_secrets_never_reach_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("gogo.log");
        log_through(LogFile::open(&p).unwrap(), || {
            tracing::trace!(
                url = %format!("https://auth.gog.com/token?grant_type=refresh_token&refresh_token={REFRESH}"),
                "refresh"
            );
            tracing::debug!(
                "GET https://gog-cdn-fastly.gog.com/token=nva=1~hmac={CDN}/secure/x.bin failed"
            );
            tracing::warn!("Authorization: Bearer {REFRESH}");
        });
        let out = std::fs::read_to_string(&p).unwrap();
        assert_eq!(out.lines().count(), 3, "{out}");
        for fake in [REFRESH, CDN] {
            assert!(!out.contains(fake), "{fake} in {out}");
        }
        assert!(
            out.contains("https://gog-cdn-fastly.gog.com/[redacted"),
            "{out}"
        );
    }

    #[test]
    fn file_is_private_ansi_free_and_appended_to() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("gogo.log");
        std::fs::write(&p, "earlier run\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        log_through(LogFile::open(&p).unwrap(), || {
            tracing::trace!(answer = 42, "trace line");
        });
        assert_eq!(mode(&p), 0o600);
        let out = std::fs::read_to_string(&p).unwrap();
        assert!(out.starts_with("earlier run\n"), "{out}");
        assert!(out.contains("trace line"), "{out}");
        assert!(!out.contains('\x1b'), "{out:?}");

        let fresh = dir.path().join("fresh.log");
        LogFile::open(&fresh).unwrap();
        assert_eq!(mode(&fresh), 0o600);
    }

    #[test]
    fn symlink_missing_parent_and_non_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, "keep").unwrap();
        let link = dir.path().join("link.log");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = LogFile::open(&link).err().unwrap();
        assert!(err.contains("symbolic link"), "{err}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");

        let err = LogFile::open(&dir.path().join("no/such/dir/x.log"))
            .err()
            .unwrap();
        assert!(err.contains("--log-file") && err.contains("x.log"), "{err}");
        assert!(!dir.path().join("no").exists());

        let err = LogFile::open(Path::new("/dev/null")).err().unwrap();
        assert!(err.contains("not a regular file"), "{err}");
    }

    #[test]
    fn file_takes_trace_while_stderr_filter_is_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("gogo.log");
        let file = LogFile::open(&p).unwrap();
        let sub = tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(Scrubbing(io::sink))
                    .with_filter(tracing_subscriber::filter::LevelFilter::ERROR),
            )
            .with(crate::file_layer(file));
        tracing::subscriber::with_default(sub, || tracing::trace!("deep detail"));
        assert!(std::fs::read_to_string(&p).unwrap().contains("deep detail"));
    }
}
