//! Global bytes/s governor — one `Arc` shared by everything (`docs/design/04` §6).

use std::sync::Arc;

/// Unkeyed direct limiter, cells = bytes (`docs/design/01` §5).
pub type Limiter = governor::DefaultDirectRateLimiter;

/// Build the shared limiter for `bytes_per_second`; `None` → unlimited (no bucket).
pub fn bytes_per_second(bytes_per_second: Option<u64>) -> Option<Arc<Limiter>> {
    let rate = bytes_per_second?;
    let cells = u32::try_from(rate.clamp(1, u32::MAX as u64)).expect("clamped to u32");
    let quota = governor::Quota::per_second(std::num::NonZeroU32::new(cells).expect(">0"));
    Some(Arc::new(governor::RateLimiter::direct(quota)))
}
