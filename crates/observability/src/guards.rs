//! Drop-based RAII guards for automatic metric updates.
//!
//! These guards ensure metrics are updated even when operations exit early
//! due to errors or panics. They follow the RAII pattern: acquire on creation,
//! release on drop.
//!
//! # Example
//!
//! ```rust
//! use metrics::{counter, gauge};
//! use vertex_observability::guards::GaugeGuard;
//!
//! fn handle_connection() {
//!     // Gauge incremented now, decremented when _guard drops
//!     let _guard = GaugeGuard::increment(gauge!("connections_active"));
//!
//!     // ... handle connection ...
//!     // If this panics or returns early, gauge is still decremented
//! }
//! ```

use core::fmt;
use std::time::Instant;

use metrics::{Counter, Gauge, Histogram};

/// Increments a counter when dropped.
///
/// Use for tracking completed operations (e.g., "finished tasks").
///
/// # Example
///
/// ```rust
/// use metrics::counter;
/// use vertex_observability::guards::CounterGuard;
///
/// fn process_item() {
///     let _done = CounterGuard::new(counter!("items_processed_total"));
///     // ... process ...
/// } // counter incremented on drop
/// ```
pub struct CounterGuard(Counter);

impl CounterGuard {
    /// Create a guard that increments the counter on drop.
    #[inline]
    pub const fn new(counter: Counter) -> Self {
        Self(counter)
    }

    /// Increment now and don't increment on drop.
    ///
    /// Use when you want to record immediately rather than at end of scope.
    pub fn increment_now(self) {
        self.0.increment(1);
        std::mem::forget(self); // Don't run Drop
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

/// Tracks a gauge that represents active/in-flight operations.
///
/// Increments the gauge on creation, decrements on drop. Ensures the gauge
/// stays accurate even if the operation panics or returns early.
///
/// # Example
///
/// ```rust
/// use metrics::gauge;
/// use vertex_observability::guards::GaugeGuard;
///
/// fn handle_request() {
///     let _active = GaugeGuard::increment(gauge!("requests_active"));
///     // gauge is now +1, decremented when _active drops
/// }
/// ```
pub struct GaugeGuard {
    gauge: Gauge,
    delta: f64,
}

impl GaugeGuard {
    /// Create a guard that increments gauge by 1 now and decrements on drop.
    #[inline]
    pub fn increment(gauge: Gauge) -> Self {
        gauge.increment(1.0);
        Self { gauge, delta: 1.0 }
    }

    /// Create a guard that increments gauge by `delta` now and decrements on drop.
    #[inline]
    pub fn increment_by(gauge: Gauge, delta: f64) -> Self {
        gauge.increment(delta);
        Self { gauge, delta }
    }

    /// Create a guard that only decrements on drop (no increment on creation).
    ///
    /// Use when the increment happens elsewhere or conditionally.
    #[inline]
    pub fn decrement_only(gauge: Gauge) -> Self {
        Self { gauge, delta: 1.0 }
    }

    /// Create a guard that decrements by `delta` on drop (no increment on creation).
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
/// Starts timing on creation, records duration on drop. Useful for measuring
/// operation latency without manual timing code.
///
/// # Example
///
/// ```rust
/// use metrics::histogram;
/// use vertex_observability::guards::TimingGuard;
///
/// fn process_request() {
///     let _timing = TimingGuard::new(histogram!("request_duration_seconds"));
///
///     // ... do work ...
/// } // duration recorded automatically
/// ```
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

    /// Get elapsed duration without recording.
    #[inline]
    pub fn elapsed(&self) -> std::time::Duration {
        self.start.elapsed()
    }

    /// Record now and consume the guard (prevents recording on drop).
    #[inline]
    pub fn record_now(self) -> std::time::Duration {
        let elapsed = self.start.elapsed();
        self.histogram.record(elapsed.as_secs_f64());
        std::mem::forget(self); // Don't run Drop
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

/// Combined guard for tracking active operations with both gauge and counter.
///
/// Common pattern: track "active" gauge + "finished" counter together.
///
/// - On creation: increments active gauge
/// - On drop: decrements active gauge, increments finished counter
///
/// # Example
///
/// ```rust
/// use metrics::{counter, gauge};
/// use vertex_observability::guards::OperationGuard;
///
/// fn handle_task() {
///     let _guard = OperationGuard::new(
///         gauge!("tasks_active"),
///         counter!("tasks_finished_total"),
///     );
///     // tasks_active is +1, decremented on drop
///     // tasks_finished_total incremented on drop
/// }
/// ```
pub struct OperationGuard {
    active_gauge: Gauge,
    finished_counter: Counter,
}

impl OperationGuard {
    /// Create a guard that tracks active gauge and finished counter.
    #[inline]
    pub fn new(active_gauge: Gauge, finished_counter: Counter) -> Self {
        active_gauge.increment(1.0);
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

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests verify the API compiles and doesn't panic.
    // Actual metric values require a recorder to be installed.

    #[test]
    fn test_counter_guard_api() {
        let counter = metrics::counter!("test_counter");
        let guard = CounterGuard::new(counter);
        drop(guard);
    }

    #[test]
    fn test_gauge_guard_api() {
        let gauge = metrics::gauge!("test_gauge");
        let guard = GaugeGuard::increment(gauge);
        drop(guard);
    }

    #[test]
    fn test_timing_guard_api() {
        let histogram = metrics::histogram!("test_histogram");
        let guard = TimingGuard::new(histogram);
        let _ = guard.elapsed();
        drop(guard);
    }

    #[test]
    fn test_operation_guard_api() {
        let gauge = metrics::gauge!("test_active");
        let counter = metrics::counter!("test_finished");
        let guard = OperationGuard::new(gauge, counter);
        drop(guard);
    }

    #[test]
    fn test_timing_guard_discard() {
        let histogram = metrics::histogram!("test_histogram");
        let guard = TimingGuard::new(histogram);
        guard.discard(); // Should not record
    }

    #[test]
    fn test_timing_guard_record_now() {
        let histogram = metrics::histogram!("test_histogram");
        let guard = TimingGuard::new(histogram);
        let _duration = guard.record_now(); // Records and consumes
    }
}
