//! Task Executor Metrics

use core::fmt;
use metrics::Counter;

/// Task Executor Metrics
#[derive(Clone, Debug)]
pub struct TaskExecutorMetrics {
    /// Number of spawned critical tasks
    pub(crate) critical_tasks_total: Counter,
    /// Number of finished spawned critical tasks
    pub(crate) finished_critical_tasks_total: Counter,
    /// Number of spawned regular tasks
    pub(crate) regular_tasks_total: Counter,
    /// Number of finished spawned regular tasks
    pub(crate) finished_regular_tasks_total: Counter,
    /// Number of spawned regular blocking tasks
    pub(crate) regular_blocking_tasks_total: Counter,
    /// Number of finished spawned regular blocking tasks
    pub(crate) finished_regular_blocking_tasks_total: Counter,
}

impl Default for TaskExecutorMetrics {
    fn default() -> Self {
        Self {
            critical_tasks_total: metrics::counter!("executor.spawn.critical_tasks_total"),
            finished_critical_tasks_total: metrics::counter!(
                "executor.spawn.finished_critical_tasks_total"
            ),
            regular_tasks_total: metrics::counter!("executor.spawn.regular_tasks_total"),
            finished_regular_tasks_total: metrics::counter!(
                "executor.spawn.finished_regular_tasks_total"
            ),
            regular_blocking_tasks_total: metrics::counter!(
                "executor.spawn.regular_blocking_tasks_total"
            ),
            finished_regular_blocking_tasks_total: metrics::counter!(
                "executor.spawn.finished_regular_blocking_tasks_total"
            ),
        }
    }
}

impl TaskExecutorMetrics {
    /// Increments the counter for spawned critical tasks.
    pub(crate) fn inc_critical_tasks(&self) {
        self.critical_tasks_total.increment(1);
    }

    /// Increments the counter for spawned regular tasks.
    pub(crate) fn inc_regular_tasks(&self) {
        self.regular_tasks_total.increment(1);
    }

    /// Increments the counter for spawned regular blocking tasks.
    pub(crate) fn inc_regular_blocking_tasks(&self) {
        self.regular_blocking_tasks_total.increment(1);
    }
}

/// Helper type for increasing counters even if a task fails
pub struct IncCounterOnDrop(Counter);

impl fmt::Debug for IncCounterOnDrop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("IncCounterOnDrop").finish()
    }
}

impl IncCounterOnDrop {
    /// Creates a new instance of `IncCounterOnDrop` with the given counter.
    pub const fn new(counter: Counter) -> Self {
        Self(counter)
    }
}

impl Drop for IncCounterOnDrop {
    /// Increment the counter when the instance is dropped.
    fn drop(&mut self) {
        self.0.increment(1);
    }
}
