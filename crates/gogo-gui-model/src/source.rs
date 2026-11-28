//! Latest-value source slots (design 25 §3).
//!
//! A producer **overwrites**; it never queues. However fast it writes, the main thread has
//! at most one wake pending and reads only the newest value of each slot.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use gogo_contract::{SourceSpec, Value, sources};

/// Locks, ignoring poison: every write under these locks leaves a whole value behind.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// "Something changed": a flag, and a wake sent only when it goes from clear to set.
pub struct Signal {
    dirty: AtomicBool,
    wake: Box<dyn Fn() + Send + Sync>,
}

impl fmt::Debug for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Signal")
            .field("dirty", &self.dirty)
            .finish_non_exhaustive()
    }
}

impl Signal {
    /// `wake` runs on the producer's thread, at most once between two [`take`](Self::take)
    /// calls. The shell passes its event-loop proxy here.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Signal {
            dirty: AtomicBool::new(false),
            wake: Box::new(wake),
        })
    }

    /// Mark that something changed.
    pub fn raise(&self) {
        if !self.dirty.swap(true, Ordering::AcqRel) {
            (self.wake)();
        }
    }

    /// Clear the flag and say whether it was set. The reader clears **before** it reads,
    /// so a write that lands during the read raises it again and nothing is lost.
    pub fn take(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }
}

/// A source and its arguments: `game(1)`, `queue`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceKey {
    name: &'static str,
    args: Vec<String>,
}

impl SourceKey {
    /// The key of source `name` with `args`. `None` when the contract has no such source, the arguments don't fit its
    /// parameters, or its data only reaches a skin through an app widget.
    pub fn new(name: &str, args: &[&str]) -> Option<Self> {
        let spec: &'static SourceSpec = sources::source(name)?;
        let required = spec.params.iter().filter(|p| !p.optional).count();
        if spec.widget_only.is_some() || args.len() < required || args.len() > spec.params.len() {
            return None;
        }
        Some(SourceKey {
            name: spec.name,
            args: args.iter().map(|a| (*a).to_owned()).collect(),
        })
    }

    /// The source's name, as the contract spells it.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// The arguments, in the order of the source's parameters.
    pub fn args(&self) -> &[String] {
        &self.args
    }
}

#[derive(Debug, Default)]
struct Cell {
    generation: u64,
    value: Option<Value>,
    /// The store forgot this slot: nobody will read it.
    closed: bool,
}

/// The writer's end of one source. Cheap to clone and `Send`.
#[derive(Debug, Clone)]
pub struct Slot {
    cell: Arc<Mutex<Cell>>,
    signal: Arc<Signal>,
}

impl Slot {
    /// Replace the value. On a closed slot this does nothing and wakes nobody.
    pub fn set(&self, value: Value) {
        {
            let mut cell = lock(&self.cell);
            if cell.closed {
                return;
            }
            cell.generation += 1;
            cell.value = Some(value);
        }
        self.signal.raise();
    }
}

#[derive(Debug)]
struct Entry {
    cell: Arc<Mutex<Cell>>,
    seen: u64,
    current: Option<Value>,
}

/// The reader's side: every open slot, and the values as of the last [`drain`](Self::drain).
/// It lives on the main thread.
#[derive(Debug)]
pub struct SourceStore {
    signal: Arc<Signal>,
    entries: HashMap<SourceKey, Entry>,
}

impl SourceStore {
    /// An empty store whose slots raise `signal`.
    pub fn new(signal: Arc<Signal>) -> Self {
        SourceStore {
            signal,
            entries: HashMap::new(),
        }
    }

    /// The writer for `key`, opening its slot if needed.
    pub fn slot(&mut self, key: &SourceKey) -> Slot {
        let entry = self.entries.entry(key.clone()).or_insert_with(|| Entry {
            cell: Arc::default(),
            seen: 0,
            current: None,
        });
        Slot {
            cell: Arc::clone(&entry.cell),
            signal: Arc::clone(&self.signal),
        }
    }

    /// Whether `key` has a slot.
    pub fn is_open(&self, key: &SourceKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Forget `key`. A writer that still holds its slot writes into nothing.
    pub fn close(&mut self, key: &SourceKey) {
        if let Some(entry) = self.entries.remove(key) {
            let mut cell = lock(&entry.cell);
            cell.closed = true;
            cell.value = None;
        }
    }

    /// Take the newest value of every slot written since the last call, and name those
    /// whose value is not the one they had: a producer that writes the same again has
    /// changed nothing. The caller takes the [`Signal`] first.
    pub fn drain(&mut self) -> Vec<SourceKey> {
        let mut changed = Vec::new();
        for (key, entry) in &mut self.entries {
            let cell = lock(&entry.cell);
            if cell.generation != entry.seen {
                entry.seen = cell.generation;
                if entry.current != cell.value {
                    entry.current.clone_from(&cell.value);
                    changed.push(key.clone());
                }
            }
        }
        changed.sort();
        changed
    }

    /// The value as of the last drain; `None` before the first one arrives.
    pub fn get(&self, key: &SourceKey) -> Option<&Value> {
        self.entries.get(key)?.current.as_ref()
    }
}
