//! The log writer: every formatted tracing line goes through
//! [`gogo::redact::scrub_text`] before it reaches stderr (design 07 §4).
//!
//! gogo's own events carry redacted values only; this is the safety net for a
//! dependency's log line (reqwest, hyper, gog-warp through the `log` bridge)
//! that a later version might start writing with a URL in it.

use gogo::redact::ScrubbedLine;
use tracing_subscriber::fmt::MakeWriter;

/// Wraps a [`MakeWriter`]: each event is buffered, scrubbed and written whole.
pub struct Scrubbing<M>(pub M);

impl<'a, M: MakeWriter<'a>> MakeWriter<'a> for Scrubbing<M> {
    type Writer = ScrubbedLine<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        ScrubbedLine::new(self.0.make_writer())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

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

    const REFRESH: &str = "FAKE-REFRESH-4c1e9a";
    const CDN: &str = "FAKE-CDN-TOKEN-77b2";

    #[test]
    fn log_lines_lose_planted_secrets() {
        let buf = Buf::default();
        let sub = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(Scrubbing(buf.clone()))
            .finish();
        tracing::subscriber::with_default(sub, || {
            tracing::trace!(
                url = %format!("https://auth.gog.com/token?grant_type=refresh_token&refresh_token={REFRESH}"),
                "refresh"
            );
            tracing::debug!(
                "GET https://gog-cdn-fastly.gog.com/token=nva=1~hmac={CDN}/secure/x.bin failed"
            );
            tracing::info!(refresh_token = REFRESH, "field form");
            tracing::warn!("Authorization: Bearer {REFRESH}");
        });
        let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert_eq!(out.lines().count(), 4, "{out}");
        for fake in [REFRESH, CDN] {
            assert!(!out.contains(fake), "{fake} in {out}");
        }
        assert!(out.contains("https://auth.gog.com/token"), "{out}");
        assert!(
            out.contains("https://gog-cdn-fastly.gog.com/[redacted"),
            "{out}"
        );
    }
}
