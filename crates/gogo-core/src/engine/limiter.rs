//! Global bytes/s governor — one instance shared by every worker (`docs/design/04` §6).
//!
//! Review A4: a single ask larger than the burst capacity is rejected by governor with
//! `InsufficientCapacity` — and burst = `min(1 MiB, limit)`, so a default 64 KiB read
//! buffer fits, but anything bigger (or a tiny limit like the 32 KiB/s floor) would not.
//! We therefore split every ask into ≤ burst-sized slices: the configured rate holds
//! *exactly*, even at the floor. `limit = 0` keeps `None` — no limiter object at all
//! (zero overhead path).

use std::sync::Arc;

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
    inner: Option<Arc<Direct>>,
    burst: u64,
}

impl GlobalLimiter {
    /// `bytes_per_second == 0` → unlimited.
    pub fn new(bytes_per_second: u64) -> Self {
        if bytes_per_second == 0 {
            return Self {
                inner: None,
                burst: 0,
            };
        }
        let rate = u32::try_from(bytes_per_second.min(u32::MAX as u64)).expect("clamped to u32");
        let quota = Quota::per_second(rate.try_into().expect("> 0"));
        // governor's default burst equals the refill rate; cap it at 1 MiB (04 §6).
        let burst = bytes_per_second.clamp(1, MAX_BURST);
        Self {
            inner: Some(Arc::new(RateLimiter::direct(quota))),
            burst,
        }
    }

    pub fn unlimited() -> Self {
        Self::new(0)
    }

    /// Consume `ask` cells, splitting into ≤ burst-sized slices so governor never sees
    /// an over-capacity ask (A4). Unlimited → returns immediately.
    pub async fn acquire(&self, ask: u64) {
        let Some(limiter) = &self.inner else {
            return;
        };
        for take in split_ask(ask, self.burst) {
            let cells = u32::try_from(take).expect("≤ burst ≤ 1 MiB");
            let cells = std::num::NonZero::new(cells).expect("split yields ≥ 1");
            let _ = limiter.until_n_ready(cells).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
