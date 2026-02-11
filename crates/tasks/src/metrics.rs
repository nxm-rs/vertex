//! Task Executor Metrics
//!
//! Metrics are lazily initialized on first access to ensure they're registered
//! after the prometheus recorder is installed.

use crate::{lazy_counter, lazy_gauge};
use core::fmt;
use metrics::{Counter, Gauge};
use std::sync::LazyLock;

// Counters for task spawn tracking
static CRITICAL_TASKS_TOTAL: LazyLock<Counter> =
    lazy_counter!("executor.spawn.critical_tasks_total");
static FINISHED_CRITICAL_TASKS_TOTAL: LazyLock<Counter> =
    lazy_counter!("executor.spawn.finished_critical_tasks_total");
static REGULAR_TASKS_TOTAL: LazyLock<Counter> =
    lazy_counter!("executor.spawn.regular_tasks_total");
static FINISHED_REGULAR_TASKS_TOTAL: LazyLock<Counter> =
    lazy_counter!("executor.spawn.finished_regular_tasks_total");
static REGULAR_BLOCKING_TASKS_TOTAL: LazyLock<Counter> =
    lazy_counter!("executor.spawn.regular_blocking_tasks_total");
static FINISHED_REGULAR_BLOCKING_TASKS_TOTAL: LazyLock<Counter> =
    lazy_counter!("executor.spawn.finished_regular_blocking_tasks_total");

// Panic counters
static PANICKED_CRITICAL_TASKS_TOTAL: LazyLock<Counter> =
    lazy_counter!("executor.tasks.panicked_total", "type" => "critical");
static PANICKED_REGULAR_TASKS_TOTAL: LazyLock<Counter> =
    lazy_counter!("executor.tasks.panicked_total", "type" => "regular");

// Running task gauges
static RUNNING_CRITICAL_TASKS: LazyLock<Gauge> =
    lazy_gauge!("executor.tasks.running", "type" => "critical");
static RUNNING_REGULAR_TASKS: LazyLock<Gauge> =
    lazy_gauge!("executor.tasks.running", "type" => "regular");
static RUNNING_BLOCKING_TASKS: LazyLock<Gauge> =
    lazy_gauge!("executor.tasks.running", "type" => "blocking");

// Graceful shutdown tracking
static GRACEFUL_SHUTDOWN_PENDING: LazyLock<Gauge> =
    lazy_gauge!("executor.tasks.graceful_shutdown_pending");

/// Task Executor Metrics handle.
///
/// This is a zero-cost wrapper that provides access to lazily-initialized
/// global metrics. Cloning is free.
#[derive(Clone, Copy, Default)]
pub struct TaskExecutorMetrics;

impl fmt::Debug for TaskExecutorMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskExecutorMetrics").finish_non_exhaustive()
    }
}

impl TaskExecutorMetrics {
    /// Increments the counter for spawned critical tasks.
    pub(crate) fn inc_critical_tasks(&self) {
        CRITICAL_TASKS_TOTAL.increment(1);
    }

    /// Increments the counter for spawned regular tasks.
    pub(crate) fn inc_regular_tasks(&self) {
        REGULAR_TASKS_TOTAL.increment(1);
    }

    /// Increments the counter for spawned regular blocking tasks.
    pub(crate) fn inc_regular_blocking_tasks(&self) {
        REGULAR_BLOCKING_TASKS_TOTAL.increment(1);
    }

    /// Record that a critical task panicked.
    pub(crate) fn record_critical_panic(&self) {
        PANICKED_CRITICAL_TASKS_TOTAL.increment(1);
    }

    /// Record that a regular task panicked.
    #[allow(dead_code)]
    pub(crate) fn record_regular_panic(&self) {
        PANICKED_REGULAR_TASKS_TOTAL.increment(1);
    }

    /// Set the graceful shutdown pending count.
    pub(crate) fn set_graceful_pending(&self, count: f64) {
        GRACEFUL_SHUTDOWN_PENDING.set(count);
    }

    /// Get the finished critical tasks counter for drop tracking.
    pub(crate) fn finished_critical_tasks_total(&self) -> Counter {
        FINISHED_CRITICAL_TASKS_TOTAL.clone()
    }

    /// Get the finished regular tasks counter for drop tracking.
    pub(crate) fn finished_regular_tasks_total(&self) -> Counter {
        FINISHED_REGULAR_TASKS_TOTAL.clone()
    }

    /// Get the finished regular blocking tasks counter for drop tracking.
    pub(crate) fn finished_regular_blocking_tasks_total(&self) -> Counter {
        FINISHED_REGULAR_BLOCKING_TASKS_TOTAL.clone()
    }

    /// Get the running critical tasks gauge for drop tracking.
    pub(crate) fn running_critical_tasks(&self) -> Gauge {
        RUNNING_CRITICAL_TASKS.clone()
    }

    /// Get the running regular tasks gauge for drop tracking.
    pub(crate) fn running_regular_tasks(&self) -> Gauge {
        RUNNING_REGULAR_TASKS.clone()
    }

    /// Get the running blocking tasks gauge for drop tracking.
    pub(crate) fn running_blocking_tasks(&self) -> Gauge {
        RUNNING_BLOCKING_TASKS.clone()
    }
}

/// Increments a counter when dropped. Used for finished task tracking.
pub struct IncCounterOnDrop(Counter);

impl fmt::Debug for IncCounterOnDrop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("IncCounterOnDrop").finish()
    }
}

impl IncCounterOnDrop {
    pub const fn new(counter: Counter) -> Self {
        Self(counter)
    }
}

impl Drop for IncCounterOnDrop {
    fn drop(&mut self) {
        self.0.increment(1);
    }
}

/// Decrements a gauge when dropped. Used for running task tracking.
pub struct DecGaugeOnDrop(Gauge);

impl fmt::Debug for DecGaugeOnDrop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DecGaugeOnDrop").finish()
    }
}

impl DecGaugeOnDrop {
    pub fn new(gauge: Gauge) -> Self {
        Self(gauge)
    }
}

impl Drop for DecGaugeOnDrop {
    fn drop(&mut self) {
        self.0.decrement(1.0);
    }
}
