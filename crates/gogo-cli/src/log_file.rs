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
        // `Line` hands over one whole event; a partial write would split an
        // escape, so write it all and report it all.
        (&*self.0).write_all(&neutralize(b))?;
        Ok(b.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        (&*self.0).flush()
    }
}

/// One event, made safe for a log file: control characters (a newline in a
/// message, an ESC from a hostile string) become `\n`, `\x1b`, … so text from a
/// server or dependency can neither forge a log line nor drive a terminal that
/// `cat`s the file. Only the event's own final newline stays.
fn neutralize(b: &[u8]) -> Vec<u8> {
    use std::fmt::Write as _;
    let text = String::from_utf8_lossy(b);
    let (body, end) = match text.strip_suffix('\n') {
        Some(body) => (body, "\n"),
        None => (&*text, ""),
    };
    let mut out = String::with_capacity(text.len());
    for c in body.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push('\t'),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push_str(end);
    out.into_bytes()
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
            // NONBLOCK: opening a FIFO nobody reads must fail, not hang, before
            // the regular-file check below. Regular files ignore the flag, and
            // only a regular file gets past that check, so it is never cleared.
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            )
            .open(path)
            .map_err(|e| match rustix::io::Errno::from_io_error(&e) {
                Some(rustix::io::Errno::LOOP) => fail(&"is a symbolic link"),
                // A FIFO with no reader.
                Some(rustix::io::Errno::NXIO) => fail(&"is not a regular file"),
                _ => fail(&e),
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

    #[derive(Clone, Default)]
    struct Buf(Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Buf {
        type Writer = Buf;
        fn make_writer(&'a self) -> Buf {
            self.clone()
        }
    }

    /// The subscriber `init_tracing` builds, for `args`, with a captured stderr.
    fn built(args: &[&str], file: &Path) -> (Box<dyn tracing::Subscriber + Send + Sync>, Buf) {
        use clap::Parser;
        let cli = crate::Cli::try_parse_from(args).unwrap();
        let err = Buf::default();
        let sub = crate::build_subscriber(
            tracing_subscriber::EnvFilter::new(crate::default_filter(&cli)),
            err.clone(),
            Some(LogFile::open(file).unwrap()),
        );
        (sub, err)
    }

    #[test]
    fn file_takes_trace_while_stderr_filter_is_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("gogo.log");
        let (sub, err) = built(&["gogo", "--quiet", "list"], &p);
        tracing::subscriber::with_default(sub, || {
            tracing::trace!("deep detail");
            tracing::warn!("loud detail");
        });
        assert!(std::fs::read_to_string(&p).unwrap().contains("deep detail"));
        let err = String::from_utf8(err.0.lock().unwrap().clone()).unwrap();
        assert!(
            err.contains("loud detail") && !err.contains("deep detail"),
            "{err}"
        );
    }

    #[test]
    fn control_characters_cannot_forge_lines_or_drive_a_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("gogo.log");
        log_through(LogFile::open(&p).unwrap(), || {
            tracing::warn!("first\n2026-01-01T00:00:00Z  INFO forged\x1b[2Jtail\r\u{9b}x");
            tracing::warn!("second");
        });
        let out = std::fs::read_to_string(&p).unwrap();
        assert_eq!(out.lines().count(), 2, "{out:?}");
        assert!(out.ends_with('\n'), "{out:?}");
        assert!(
            !out.contains('\x1b') && !out.contains('\r') && !out.contains('\u{9b}'),
            "{out:?}"
        );
        assert!(out.contains("first\\n2026"), "{out:?}");
    }

    /// The writer's own guard, for text the fmt layer leaves raw (it escapes
    /// message text itself, but not every field or dependency formatter).
    #[test]
    fn neutralize_escapes_raw_controls_but_keeps_the_final_newline() {
        let got = neutralize(b"a\nb\x1b[2Jc\r\xc2\x9bd\te\n");
        assert_eq!(
            String::from_utf8(got).unwrap(),
            "a\\nb\\u{1b}[2Jc\\r\\u{9b}d\te\n"
        );
        assert_eq!(neutralize(b"no newline"), b"no newline");
    }

    #[test]
    fn directories_and_fifos_are_refused_without_hanging() {
        let dir = tempfile::tempdir().unwrap();
        let err = LogFile::open(dir.path()).err().unwrap();
        assert!(err.contains("--log-file"), "{err}");

        let fifo = dir.path().join("pipe");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        // Run the opens on a thread: a regression (blocking open) fails here
        // instead of hanging CI.
        let (tx, rx) = std::sync::mpsc::channel();
        let f = fifo.clone();
        std::thread::spawn(move || {
            let no_reader = LogFile::open(&f).err();
            let _reader = OpenOptions::new()
                .read(true)
                .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
                .open(&f)
                .unwrap();
            let with_reader = LogFile::open(&f).err();
            let _ = tx.send((no_reader, with_reader));
        });
        let (no_reader, with_reader) = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("opening a FIFO hung");
        assert!(no_reader.unwrap().contains("not a regular file"));
        assert!(with_reader.unwrap().contains("not a regular file"));
    }
}
