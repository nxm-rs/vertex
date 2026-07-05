//! The runtime wall clock: Unix timestamps that follow the timer clock.
//!
//! On native the value is derived from a process-global anchor: the platform
//! wall clock is sampled once at first read and paired with a
//! [`tokio::time::Instant`]; every read is anchor epoch plus timer-clock
//! elapsed. In production the timer clock is the process monotonic clock, so
//! the derived value tracks real wall time. Under `start_paused` or turmoil,
//! `tokio::time::advance` moves it, so paused-clock tests and the sim control
//! bookkeeping timestamps the same way they control timers.
//!
//! Caveats, and why wire-visible timestamps must not read here:
//!
//! - NTP steps after the anchor are ignored.
//! - The monotonic clock halts across suspend, so the derived value lags real
//!   wall time by cumulative suspend until restart. Fine for bookkeeping and
//!   persistence; disqualifying for wire timestamps that remote peers validate
//!   against their own clocks (those read `vertex_util_runtime::time`).
//! - A read on a thread outside a tokio runtime under a paused test silently
//!   returns real elapsed time.
//! - The anchor is process-global, so mixed paused runtimes in one test
//!   process can interleave; process-per-test runners are unaffected.
//!
//! On `wasm32` these are the platform-clock helpers unchanged: there is no
//! tokio timer driver in the browser, and the browser clock is the only clock.

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::sync::OnceLock;

    use tokio::time::Instant;
    use vertex_util_runtime::time::Duration;

    /// Real epoch offset and timer-clock instant captured at first read.
    static ANCHOR: OnceLock<(Duration, Instant)> = OnceLock::new();

    fn since_epoch() -> Duration {
        let (epoch, instant) = *ANCHOR.get_or_init(|| {
            let epoch = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or(Duration::ZERO);
            (epoch, Instant::now())
        });
        epoch + Instant::now().saturating_duration_since(instant)
    }

    /// Returns the runtime clock's Unix timestamp in whole seconds.
    pub fn now_unix_secs() -> u64 {
        since_epoch().as_secs()
    }

    /// Returns the runtime clock's Unix timestamp in whole milliseconds.
    pub fn now_unix_millis() -> u64 {
        // `as_millis` is `u128`; the value fits `u64` until well past year 500000.
        since_epoch().as_millis() as u64
    }

    /// Returns the runtime clock's Unix timestamp in nanoseconds.
    ///
    /// `i64` matches the accounting call sites that consume nanosecond
    /// timestamps.
    pub fn now_unix_nanos() -> i64 {
        since_epoch().as_nanos() as i64
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::{now_unix_millis, now_unix_nanos, now_unix_secs};

#[cfg(target_arch = "wasm32")]
pub use vertex_util_runtime::time::{now_unix_millis, now_unix_nanos, now_unix_secs};

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use vertex_util_runtime::time::Duration;

    // One test owns the process-global anchor so the paused-clock assertions
    // stay deterministic under in-process test runners.
    #[tokio::test(start_paused = true)]
    async fn unix_clock_rides_the_paused_timer_clock() {
        let secs = now_unix_secs();
        let millis = now_unix_millis();
        let nanos = now_unix_nanos();
        // 2023-01-01T00:00:00Z: the anchor samples the real platform clock.
        assert!(secs > 1_672_531_200);
        assert!(millis / 1000 >= secs);
        assert!(nanos > 0);

        tokio::time::advance(Duration::from_secs(120)).await;
        // The clock is paused, so the delta is exactly the advance.
        assert_eq!(now_unix_secs() - secs, 120);
        assert_eq!(now_unix_millis() - millis, 120_000);
    }
}
