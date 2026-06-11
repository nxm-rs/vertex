//! Wall-clock and monotonic time.
//!
//! The time types are re-exported from `web-time`, which is itself a
//! `std::time` re-export on native targets and a browser-clock shim on
//! `wasm32`. Because `web-time` already resolves the platform difference, this
//! module needs no `cfg(target_arch = "wasm32")` gating of its own; it is a
//! thin, documented surface over that crate plus a few deduplicated helpers.
//!
//! Reach for the helpers ([`now_unix_secs`], [`now_unix_millis`],
//! [`now_unix_nanos`], [`now`]) instead of re-deriving Unix timestamps from
//! [`SystemTime`] at each call site.

pub use web_time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Returns the current monotonic instant.
///
/// Use this for measuring elapsed time and arming timers. It is not tied to
/// wall-clock time and is not comparable across processes.
#[inline]
pub fn now() -> Instant {
    Instant::now()
}

/// Returns the elapsed time since the Unix epoch as a [`Duration`].
///
/// Clamps to [`Duration::ZERO`] if the platform clock reports a time before the
/// epoch, which only happens with a grossly misconfigured clock.
#[inline]
fn since_epoch() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
}

/// Returns the current Unix timestamp in whole seconds.
///
/// Returns `0` if the platform clock is set before the Unix epoch.
#[inline]
pub fn now_unix_secs() -> u64 {
    since_epoch().as_secs()
}

/// Returns the current Unix timestamp in whole milliseconds.
///
/// Returns `0` if the platform clock is set before the Unix epoch.
#[inline]
pub fn now_unix_millis() -> u64 {
    // `as_millis` is `u128`; the value fits `u64` until well past year 500000.
    since_epoch().as_millis() as u64
}

/// Returns the current Unix timestamp in nanoseconds.
///
/// The return type is `i64` to match the wire and accounting call sites that
/// consume nanosecond timestamps. Returns `0` if the platform clock is set
/// before the Unix epoch.
#[inline]
pub fn now_unix_nanos() -> i64 {
    since_epoch().as_nanos() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_is_monotonic() {
        let a = now();
        let b = now();
        assert!(b >= a);
    }

    #[test]
    fn unix_secs_after_2023() {
        // 2023-01-01T00:00:00Z in seconds.
        assert!(now_unix_secs() > 1_672_531_200);
    }

    #[test]
    fn unix_millis_after_2023() {
        assert!(now_unix_millis() > 1_672_531_200_000);
    }

    #[test]
    fn unix_nanos_after_2023() {
        assert!(now_unix_nanos() > 1_672_531_200_000_000_000);
    }

    #[test]
    fn units_are_consistent() {
        let secs = now_unix_secs();
        let millis = now_unix_millis();
        // Milliseconds and seconds are read a moment apart, so allow a small
        // skew rather than asserting exact equality.
        assert!(millis / 1000 >= secs);
        assert!(millis / 1000 <= secs + 2);
    }
}
