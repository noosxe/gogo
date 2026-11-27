//! Global bytes/s governor — one instance shared by every worker (`docs/design/04` §6).
//!
//! Review A4: a single ask larger than the burst capacity is rejected by governor with
//! `InsufficientCapacity` — and burst = `min(1 MiB, limit)`, so a default 64 KiB read
//! buffer fits, but anything bigger (or a tiny limit like the 32 KiB/s floor) would not.
//! We therefore split every ask into ≤ burst-sized slices: the configured rate holds
//! *exactly*, even at the floor. `limit = 0` keeps `None` — no limiter object at all
//! (zero overhead path).

use std::sync::{Arc, RwLock};

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};

/// Unkeyed direct limiter, cells = bytes (`docs/design/01` §5).
pub type Direct = DefaultDirectRateLimiter;

const MAX_BURST: u64 = 1024 * 1024;

/// Pure split logic (A4): how a request of `ask` cells is consumed given `burst`
/// capacity per call. Deterministic, unit-testable without any clock.
pub fn split_ask(ask: u64, burst: u64) -> Vec<u64> {
    if burst == 0 {
        return vec![ask];
    }
    let mut out = Vec::with_capacity(ask.div_ceil(burst) as usize);
    let mut remaining = ask;
    while remaining > 0 {
        let take = remaining.min(burst);
        out.push(take);
        remaining -= take;
    }
    out
}

/// The shared limiter handle: `None` = unlimited (zero overhead).
pub struct GlobalLimiter {
    /// Swapped by [`GlobalLimiter::set_rate`], so a new rate reaches the workers
    /// already holding the limiter.
    inner: RwLock<Option<Limit>>,
    /// Callers inside [`GlobalLimiter::acquire`] on a limited rate, so a test can
    /// tell that one is waiting for budget.
    #[cfg(test)]
    waiting: std::sync::atomic::AtomicUsize,
}

/// One configured rate.
#[derive(Clone)]
struct Limit {
    direct: Arc<Direct>,
    burst: u64,
}

impl Limit {
    fn new(bytes_per_second: u64) -> Option<Self> {
        if bytes_per_second == 0 {
            return None;
        }
        let rate = u32::try_from(bytes_per_second.min(u32::MAX as u64)).expect("clamped to u32");
        let quota = Quota::per_second(rate.try_into().expect("> 0"));
        // governor's default burst equals the refill rate; cap it at 1 MiB (04 §6).
        let burst = bytes_per_second.clamp(1, MAX_BURST);
        Some(Self {
            direct: Arc::new(RateLimiter::direct(quota)),
            burst,
        })
    }
}

impl GlobalLimiter {
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.waiting.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// `bytes_per_second == 0` → unlimited.
    pub fn new(bytes_per_second: u64) -> Self {
        Self {
            inner: RwLock::new(Limit::new(bytes_per_second)),
            #[cfg(test)]
            waiting: Default::default(),
        }
    }

    /// Change the rate while workers run (`0` → unlimited). An ask already
    /// waiting finishes at the old rate.
    pub fn set_rate(&self, bytes_per_second: u64) {
        *self.inner.write().expect("limiter lock") = Limit::new(bytes_per_second);
    }

    pub fn unlimited() -> Self {
        Self::new(0)
    }

    /// Consume `ask` cells, splitting into ≤ burst-sized slices so governor never sees
    /// an over-capacity ask (A4). Unlimited → returns immediately.
    pub async fn acquire(&self, ask: u64) {
        let limit = self.inner.read().expect("limiter lock").clone();
        let Some(Limit { direct, burst }) = limit else {
            return;
        };
        #[cfg(test)]
        let _waiting = Waiting::enter(&self.waiting);
        for take in split_ask(ask, burst) {
            let cells = u32::try_from(take).expect("≤ burst ≤ 1 MiB");
            let cells = std::num::NonZero::new(cells).expect("split yields ≥ 1");
            let _ = direct.until_n_ready(cells).await;
        }
    }
}

/// Counts a caller until it leaves, also when its future is dropped.
#[cfg(test)]
struct Waiting<'a>(&'a std::sync::atomic::AtomicUsize);

#[cfg(test)]
impl<'a> Waiting<'a> {
    fn enter(count: &'a std::sync::atomic::AtomicUsize) -> Self {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self(count)
    }
}

#[cfg(test)]
impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_new_rate_applies_to_a_limiter_already_shared() {
        let limiter = Arc::new(GlobalLimiter::new(32 * 1024));
        let held = Arc::clone(&limiter);
        limiter.set_rate(0);
        // At the old rate this would take over half a minute.
        tokio::time::timeout(std::time::Duration::from_secs(5), held.acquire(1 << 20))
            .await
            .expect("unlimited after set_rate");
        limiter.set_rate(1 << 30);
        held.acquire(64 * 1024).await;
    }

    #[test]
    fn split_never_exceeds_burst() {
        // 64 KiB buffer, 1 MiB burst: one slice.
        assert_eq!(split_ask(64 * 1024, MAX_BURST), vec![64 * 1024]);
        // 64 KiB buffer at the 32 KiB/s floor: two 32 KiB slices — rate holds exactly.
        assert_eq!(split_ask(64 * 1024, 32 * 1024), vec![32 * 1024, 32 * 1024]);
        // 10 MiB ask (whole-chunk shortcut) against 1 MiB burst: ten slices.
        assert_eq!(split_ask(10 * 1024 * 1024, MAX_BURST).len(), 10);
        // Zero ask: nothing.
        assert!(split_ask(0, MAX_BURST).is_empty());
    }

    #[tokio::test]
    async fn unlimited_acquires_immediately() {
        GlobalLimiter::unlimited().acquire(10 * 1024 * 1024).await;
    }

    #[tokio::test]
    async fn zero_limit_is_unlimited() {
        GlobalLimiter::new(0).acquire(u64::MAX / 2).await;
    }
}
