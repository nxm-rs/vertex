//! Drop-based RAII guards for automatic metric updates.

use core::fmt;
use std::time::Instant;

use metrics::{Counter, Gauge, Histogram};

/// Increments a counter when dropped.
pub struct CounterGuard(Counter);

impl CounterGuard {
    /// Create a guard that increments the counter on drop.
    #[inline]
    pub const fn new(counter: Counter) -> Self {
        Self(counter)
    }

    /// Increment immediately and consume the guard (skip drop).
    pub fn increment_now(self) {
        self.0.increment(1);
        std::mem::forget(self);
    }
}

impl fmt::Debug for CounterGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("CounterGuard").finish()
    }
}

impl Drop for CounterGuard {
    fn drop(&mut self) {
        self.0.increment(1);
    }
}

/// Tracks an active/in-flight gauge: increments on creation, decrements on drop.
pub struct GaugeGuard {
    gauge: Gauge,
    delta: f64,
}

impl GaugeGuard {
    /// Increment gauge by 1 now, decrement on drop.
    #[inline]
    pub fn increment(gauge: Gauge) -> Self {
        gauge.increment(1.0);
        Self { gauge, delta: 1.0 }
    }

    /// Increment gauge by `delta` now, decrement on drop.
    #[inline]
    pub fn increment_by(gauge: Gauge, delta: f64) -> Self {
        gauge.increment(delta);
        Self { gauge, delta }
    }

    /// Only decrement on drop (no increment on creation).
    #[inline]
    pub fn decrement_only(gauge: Gauge) -> Self {
        Self { gauge, delta: 1.0 }
    }

    /// Decrement by `delta` on drop (no increment on creation).
    #[inline]
    pub fn decrement_only_by(gauge: Gauge, delta: f64) -> Self {
        Self { gauge, delta }
    }
}

impl fmt::Debug for GaugeGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GaugeGuard")
            .field("delta", &self.delta)
            .finish()
    }
}

impl Drop for GaugeGuard {
    fn drop(&mut self) {
        self.gauge.decrement(self.delta);
    }
}

/// Records elapsed time to a histogram when dropped.
///
/// Starts timing on creation, records duration as seconds on drop.
pub struct TimingGuard {
    histogram: Histogram,
    start: Instant,
}

impl TimingGuard {
    /// Start timing for the given histogram.
    #[inline]
    pub fn new(histogram: Histogram) -> Self {
        Self {
            histogram,
            start: Instant::now(),
        }
    }

    #[inline]
    pub fn elapsed(&self) -> std::time::Duration {
        self.start.elapsed()
    }

    /// Record now and consume the guard (prevents double-recording on drop).
    #[inline]
    pub fn record_now(self) -> std::time::Duration {
        let elapsed = self.start.elapsed();
        self.histogram.record(elapsed.as_secs_f64());
        std::mem::forget(self);
        elapsed
    }

    /// Discard without recording.
    #[inline]
    pub fn discard(self) {
        std::mem::forget(self);
    }
}

impl fmt::Debug for TimingGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TimingGuard")
            .field("elapsed", &self.start.elapsed())
            .finish()
    }
}

impl Drop for TimingGuard {
    fn drop(&mut self) {
        self.histogram.record(self.start.elapsed().as_secs_f64());
    }
}

/// Combined guard: increments active gauge on creation, decrements gauge and
/// increments finished counter on drop.
pub struct OperationGuard {
    active_gauge: Gauge,
    finished_counter: Counter,
}

impl OperationGuard {
    /// Increment active gauge now; on drop, decrement gauge and increment counter.
    #[inline]
    pub fn new(active_gauge: Gauge, finished_counter: Counter) -> Self {
        active_gauge.increment(1.0);
        Self {
            active_gauge,
            finished_counter,
        }
    }

    /// Create without incrementing the gauge on creation.
    #[inline]
    pub fn without_initial_increment(active_gauge: Gauge, finished_counter: Counter) -> Self {
        Self {
            active_gauge,
            finished_counter,
        }
    }
}

impl fmt::Debug for OperationGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OperationGuard").finish()
    }
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.active_gauge.decrement(1.0);
        self.finished_counter.increment(1);
    }
}

/// Acquire a read lock and record acquisition time to a histogram.
#[inline]
pub fn timed_read<T>(
    lock: &parking_lot::RwLock<T>,
    histogram: Histogram,
) -> parking_lot::RwLockReadGuard<'_, T> {
    let start = Instant::now();
    let guard = lock.read();
    histogram.record(start.elapsed().as_secs_f64());
    guard
}

/// Acquire a write lock and record acquisition time to a histogram.
#[inline]
pub fn timed_write<T>(
    lock: &parking_lot::RwLock<T>,
    histogram: Histogram,
) -> parking_lot::RwLockWriteGuard<'_, T> {
    let start = Instant::now();
    let guard = lock.write();
    histogram.record(start.elapsed().as_secs_f64());
    guard
}

/// Acquire a mutex lock and record acquisition time to a histogram.
#[inline]
pub fn timed_lock<T>(
    lock: &parking_lot::Mutex<T>,
    histogram: Histogram,
) -> parking_lot::MutexGuard<'_, T> {
    let start = Instant::now();
    let guard = lock.lock();
    histogram.record(start.elapsed().as_secs_f64());
    guard
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_guard() {
        let counter = metrics::counter!("test_counter");
        let guard = CounterGuard::new(counter);
        drop(guard);
    }

    #[test]
    fn gauge_guard() {
        let gauge = metrics::gauge!("test_gauge");
        let guard = GaugeGuard::increment(gauge);
        drop(guard);
    }

    #[test]
    fn timing_guard() {
        let histogram = metrics::histogram!("test_histogram");
        let guard = TimingGuard::new(histogram);
        let _ = guard.elapsed();
        drop(guard);
    }

    #[test]
    fn timing_guard_discard() {
        let histogram = metrics::histogram!("test_histogram");
        TimingGuard::new(histogram).discard();
    }

    #[test]
    fn timing_guard_record_now() {
        let histogram = metrics::histogram!("test_histogram");
        let _duration = TimingGuard::new(histogram).record_now();
    }

    #[test]
    fn operation_guard() {
        let gauge = metrics::gauge!("test_active");
        let counter = metrics::counter!("test_finished");
        let guard = OperationGuard::new(gauge, counter);
        drop(guard);
    }

    #[test]
    fn timed_read_guard() {
        let lock = parking_lot::RwLock::new(42);
        let guard = timed_read(&lock, metrics::histogram!("test_lock_read"));
        assert_eq!(*guard, 42);
    }

    #[test]
    fn timed_write_guard() {
        let lock = parking_lot::RwLock::new(42);
        let mut guard = timed_write(&lock, metrics::histogram!("test_lock_write"));
        *guard = 100;
        assert_eq!(*guard, 100);
    }

    #[test]
    fn timed_lock_guard() {
        let lock = parking_lot::Mutex::new(42);
        let guard = timed_lock(&lock, metrics::histogram!("test_mutex_lock"));
        assert_eq!(*guard, 42);
    }
}
