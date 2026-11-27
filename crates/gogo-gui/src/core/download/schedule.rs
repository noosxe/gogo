//! Which download runs now: [`SLOTS`] at once, first come first served, and never two
//! of one product, which would write the same files (design 25 §5).

use std::sync::{Arc, Mutex};

use tokio::sync::watch;

use super::super::lock;

/// How many downloads run at once.
pub(super) const SLOTS: usize = 2;

#[derive(Default)]
struct Queue {
    /// Ticket and product, in ticket order.
    waiting: Vec<(u64, u64)>,
    /// The products downloading.
    active: Vec<u64>,
    tickets: u64,
}

impl Queue {
    /// The ticket that may start now.
    fn next(&self) -> Option<u64> {
        if self.active.len() >= SLOTS {
            return None;
        }
        self.waiting
            .iter()
            .find(|(_, product)| !self.active.contains(product))
            .map(|(ticket, _)| *ticket)
    }
}

struct Shared {
    queue: Mutex<Queue>,
    /// Bumped whenever the queue changes.
    changed: watch::Sender<u64>,
}

impl Shared {
    fn edit<T>(&self, edit: impl FnOnce(&mut Queue) -> T) -> T {
        let out = edit(&mut lock(&self.queue));
        self.changed.send_modify(|n| *n += 1);
        out
    }
}

#[derive(Clone)]
pub(super) struct Schedule(Arc<Shared>);

impl Default for Schedule {
    fn default() -> Self {
        Schedule(Arc::new(Shared {
            queue: Mutex::default(),
            changed: watch::Sender::new(0),
        }))
    }
}

impl Schedule {
    /// A place in the line, taken now.
    pub(super) fn join(&self, product: u64) -> Place {
        let ticket = self.0.edit(|q| {
            q.tickets += 1;
            q.waiting.push((q.tickets, product));
            q.tickets
        });
        Place {
            shared: Arc::clone(&self.0),
            ticket,
            product,
            running: false,
        }
    }
}

/// A place in the line, then a slot. Dropping it gives either up.
pub(super) struct Place {
    shared: Arc<Shared>,
    ticket: u64,
    product: u64,
    running: bool,
}

impl Place {
    /// Wait for the turn. Safe to drop half way.
    pub(super) async fn turn(&mut self) {
        let mut changed = self.shared.changed.subscribe();
        while !self.running {
            {
                let mut q = lock(&self.shared.queue);
                if q.next() == Some(self.ticket) {
                    q.waiting.retain(|(t, _)| *t != self.ticket);
                    q.active.push(self.product);
                    self.running = true;
                }
            }
            if self.running {
                // Another slot may be free for the one behind.
                self.shared.changed.send_modify(|n| *n += 1);
            } else if changed.changed().await.is_err() {
                return;
            }
        }
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        let (ticket, product, running) = (self.ticket, self.product, self.running);
        self.shared.edit(|q| {
            q.waiting.retain(|(t, _)| *t != ticket);
            if running && let Some(at) = q.active.iter().position(|p| *p == product) {
                q.active.remove(at);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Waker};

    use super::*;

    /// Whether the turn has come, without waiting for it.
    fn ready(place: &mut Place) -> bool {
        let turn = pin!(place.turn());
        turn.poll(&mut Context::from_waker(Waker::noop()))
            .is_ready()
    }

    #[test]
    fn two_run_at_once_and_one_product_never_twice() {
        let schedule = Schedule::default();
        let mut a = schedule.join(1);
        let mut same = schedule.join(1);
        let mut b = schedule.join(2);
        let mut c = schedule.join(3);
        assert!(ready(&mut a));
        assert!(!ready(&mut same), "the same files");
        assert!(ready(&mut b), "goes past the one that cannot run");
        assert!(!ready(&mut c), "both slots are taken");
        drop(a);
        assert!(!ready(&mut c), "the older one goes first");
        assert!(ready(&mut same));
        drop(same);
        assert!(ready(&mut c));
    }

    #[test]
    fn a_place_given_up_holds_nobody_back() {
        let schedule = Schedule::default();
        let mut a = schedule.join(1);
        let b = schedule.join(2);
        let mut c = schedule.join(3);
        assert!(ready(&mut a));
        drop(b);
        assert!(ready(&mut c));
    }
}
