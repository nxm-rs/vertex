//! Drop-based RAII guards for automatic metric updates.

use core::fmt;
use core::num::NonZeroU32;
use std::sync::LazyLock;

use metrics::{Counter, Gauge, Histogram};
use vertex_util_runtime::time::Instant;

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

/// Samples a [`TimingGuard`] one call in `interval`, so a hot loop pays the
/// clock read and histogram record on a fixed fraction of iterations.
///
/// The skip path is a single decrement and branch: no clock read, no atomic,
/// no deref of the lazy handle. The static [`LazyLock`] is only forced on the
/// first sampled call, so a handle materialized before the recorder is
/// installed is never cached.
pub struct TimingSampler {
    histogram: &'static LazyLock<Histogram>,
    interval: NonZeroU32,
    countdown: u32,
}

impl TimingSampler {
    /// Sampler over `histogram`, recording one call in `interval`. Countdown
    /// starts at zero so the first call samples and a quiet node reports promptly.
    #[inline]
    pub const fn new(histogram: &'static LazyLock<Histogram>, interval: NonZeroU32) -> Self {
        Self {
            histogram,
            interval,
            countdown: 0,
        }
    }

    /// Start timing on a sampled call, or `None` to skip.
    #[inline]
    pub fn start(&mut self) -> Option<TimingGuard> {
        if self.countdown > 0 {
            self.countdown -= 1;
            return None;
        }
        self.countdown = self.interval.get() - 1;
        Some(TimingGuard::new(LazyLock::force(self.histogram).clone()))
    }
}

impl fmt::Debug for TimingSampler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TimingSampler")
            .field("interval", &self.interval)
            .field("countdown", &self.countdown)
            .finish()
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

    #[test]
    fn timing_sampler_cadence() {
        static HIST: LazyLock<Histogram> =
            LazyLock::new(|| metrics::histogram!("test_sampler_cadence"));
        let mut sampler = TimingSampler::new(&HIST, NonZeroU32::new(4).unwrap());

        // Interval 4 samples calls 1, 5, 9 over nine calls.
        let sampled: Vec<u32> = (1u32..=9).filter(|_| sampler.start().is_some()).collect();
        assert_eq!(sampled, vec![1, 5, 9]);
    }

    #[test]
    fn timing_sampler_records_at_interval() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};

        use metrics::{
            Counter, Gauge, Histogram, HistogramFn, Key, KeyName, Metadata, Recorder, SharedString,
            Unit,
        };

        #[derive(Default)]
        struct CountingHistogram(AtomicU64);
        impl HistogramFn for CountingHistogram {
            fn record(&self, _value: f64) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        struct CountingRecorder(Arc<CountingHistogram>);
        impl Recorder for CountingRecorder {
            fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
            fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
            fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
            fn register_counter(&self, _: &Key, _: &Metadata<'_>) -> Counter {
                Counter::noop()
            }
            fn register_gauge(&self, _: &Key, _: &Metadata<'_>) -> Gauge {
                Gauge::noop()
            }
            fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
                Histogram::from_arc(self.0.clone())
            }
        }

        static HIST: LazyLock<Histogram> =
            LazyLock::new(|| metrics::histogram!("test_sampler_record"));

        let hist = Arc::new(CountingHistogram::default());
        let recorder = CountingRecorder(hist.clone());
        metrics::with_local_recorder(&recorder, || {
            let mut sampler = TimingSampler::new(&HIST, NonZeroU32::new(2).unwrap());
            // Interval 2 across 4 calls records exactly 2 histograms.
            for _ in 0..4 {
                drop(sampler.start());
            }
        });
        assert_eq!(hist.0.load(Ordering::Relaxed), 2);
    }
}
