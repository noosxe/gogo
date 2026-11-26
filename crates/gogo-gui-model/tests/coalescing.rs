//! Sources keep only the newest value, and a producer can't build a backlog (#538).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use gogo_contract::Value;
use gogo_gui_model::{Signal, SourceKey, SourceStore};

fn counting_signal() -> (Arc<Signal>, Arc<AtomicUsize>) {
    let wakes = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&wakes);
    let signal = Signal::new(move || {
        seen.fetch_add(1, Ordering::SeqCst);
    });
    (signal, wakes)
}

#[test]
fn a_flood_against_a_slow_frame_leaves_a_backlog_of_one() {
    let (signal, wakes) = counting_signal();
    let mut store = SourceStore::new(Arc::clone(&signal));
    let key = SourceKey::new("game", &["1"]).unwrap();
    let slot = store.slot(&key);

    // The frame is "slow": nothing reads while the producers write.
    let producers: Vec<_> = (0..4)
        .map(|_| {
            let slot = slot.clone();
            thread::spawn(move || {
                for i in 0..2_500 {
                    slot.set(Value::Num(f64::from(i)));
                }
            })
        })
        .collect();
    for p in producers {
        p.join().unwrap();
    }
    slot.set(Value::str("newest"));

    assert_eq!(wakes.load(Ordering::SeqCst), 1, "one wake is pending");
    assert!(signal.take());
    assert_eq!(store.drain(), std::slice::from_ref(&key));
    assert_eq!(store.get(&key), Some(&Value::str("newest")));
    assert!(!signal.take());
    assert!(store.drain().is_empty(), "nothing queued behind it");

    // The next write after the frame wakes again, once.
    slot.set(Value::Null);
    slot.set(Value::Bool(true));
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
}

#[test]
fn a_write_during_a_drain_is_not_lost() {
    let (signal, wakes) = counting_signal();
    let mut store = SourceStore::new(Arc::clone(&signal));
    let key = SourceKey::new("account", &[]).unwrap();
    let slot = store.slot(&key);
    slot.set(Value::Num(1.0));
    assert!(signal.take());
    // Written after the flag was cleared and before the values are read.
    slot.set(Value::Num(2.0));
    store.drain();
    assert_eq!(store.get(&key), Some(&Value::Num(2.0)));
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
    assert!(signal.take());
    assert!(store.drain().is_empty());
}

#[test]
fn keys_follow_the_contract() {
    assert!(SourceKey::new("game", &["1"]).is_some());
    assert!(SourceKey::new("game", &[]).is_none());
    assert!(SourceKey::new("game", &["1", "2"]).is_none());
    assert!(SourceKey::new("library", &[]).is_some());
    assert!(SourceKey::new("tokens", &[]).is_none());
    assert!(SourceKey::new("job.log", &["1"]).is_none(), "widget only");
}

#[test]
fn a_closed_slot_takes_writes_into_nothing() {
    let (signal, wakes) = counting_signal();
    let mut store = SourceStore::new(signal);
    let key = SourceKey::new("apps", &[]).unwrap();
    let slot = store.slot(&key);
    store.close(&key);
    slot.set(Value::Num(1.0));
    assert_eq!(wakes.load(Ordering::SeqCst), 0, "and wakes nobody");
    assert!(store.drain().is_empty());
    assert_eq!(store.get(&key), None);
    assert!(!store.is_open(&key));
}
