//! Watching a folder for hot reload (design 24 §2): notify, with a burst of writes (an
//! editor saving, a copy) reported once.

use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecursiveMode, Watcher as _};

/// Quiet time that ends a burst.
const QUIET: Duration = Duration::from_millis(100);
/// A burst that never goes quiet is still reported this often.
const LONGEST: Duration = Duration::from_secs(1);

/// A watch that lasts as long as this value.
pub(crate) struct Watch(#[allow(dead_code)] notify::RecommendedWatcher);

/// Calls `changed` on another thread after files under `dir` changed.
pub(crate) fn watch(
    dir: &Path,
    changed: impl Fn() + Send + 'static,
) -> Result<Watch, notify::Error> {
    let (send, receive) = channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        // An error may have lost events, so it counts as a change.
        if event.is_ok_and(|e| !is_change(&e.kind)) {
            return;
        }
        // The receiver is gone only when the watch is being dropped.
        let _ = send.send(());
    })?;
    watcher.watch(dir, RecursiveMode::Recursive)?;
    std::thread::Builder::new()
        .name("watch".to_owned())
        .spawn(move || debounce(&receive, QUIET, LONGEST, changed))?;
    Ok(Watch(watcher))
}

/// Reading a file is also an event on Linux; reacting to it would reload forever.
fn is_change(kind: &EventKind) -> bool {
    !matches!(kind, EventKind::Access(_))
}

/// One call per burst, until the sender is dropped.
fn debounce(events: &Receiver<()>, quiet: Duration, longest: Duration, changed: impl Fn()) {
    while events.recv().is_ok() {
        let end = Instant::now() + longest;
        loop {
            let wait = quiet.min(end.saturating_duration_since(Instant::now()));
            if wait.is_zero() {
                break;
            }
            match events.recv_timeout(wait) {
                Ok(()) => {}
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        changed();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;
    use std::time::Duration;

    use notify::EventKind;
    use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind};

    use super::{debounce, is_change, watch};

    #[test]
    fn reading_is_not_a_change() {
        assert!(!is_change(&EventKind::Access(AccessKind::Any)));
        assert!(is_change(&EventKind::Create(CreateKind::Any)));
        assert!(is_change(&EventKind::Modify(ModifyKind::Any)));
        assert!(is_change(&EventKind::Remove(RemoveKind::Any)));
    }

    #[test]
    fn a_burst_is_reported_once() {
        let (send, receive) = channel();
        let (report, reports) = channel();
        let thread = std::thread::spawn(move || {
            debounce(
                &receive,
                Duration::from_millis(200),
                Duration::from_secs(5),
                move || {
                    let _ = report.send(());
                },
            );
        });
        for _ in 0..5 {
            send.send(()).expect("the thread is running");
        }
        assert!(reports.recv_timeout(Duration::from_secs(5)).is_ok());
        assert!(reports.recv_timeout(Duration::from_millis(400)).is_err());
        drop(send);
        thread.join().expect("no panic");
    }

    #[test]
    fn a_write_in_a_watched_folder_is_seen() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        std::fs::create_dir(dir.path().join("sub")).expect("a folder");
        let (report, reports) = channel();
        let _watch = watch(dir.path(), move || {
            let _ = report.send(());
        })
        .expect("a watch");
        std::fs::write(dir.path().join("sub/file"), "x").expect("a write");
        assert!(reports.recv_timeout(Duration::from_secs(10)).is_ok());
    }
}
