//! Task Executor Metrics
//!
//! Metrics are lazily initialized on first access to ensure they're registered
//! after the prometheus recorder is installed.
//!
//! Spawn counters and running gauges include a `task` label with the task name
//! (where available) for visibility into what's running. The running gauge also
//! carries a `graceful` label ("true"/"false") indicating whether the task
//! participates in graceful shutdown.

use core::fmt;
use metrics::{Counter, Gauge, counter, gauge};
use std::sync::LazyLock;
use vertex_metrics::lazy_counter;

// Panic counter (no task label - panics are rare, aggregate is sufficient)
static PANICKED_CRITICAL_TASKS_TOTAL: LazyLock<Counter> =
    lazy_counter!("executor_tasks_panicked_total", "type" => "critical");

/// Task Executor Metrics handle.
///
/// This is a zero-cost wrapper that provides access to lazily-initialized
/// global metrics. Cloning is free.
#[derive(Clone, Copy, Default)]
pub struct TaskExecutorMetrics;

impl fmt::Debug for TaskExecutorMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskExecutorMetrics")
            .finish_non_exhaustive()
    }
}

impl TaskExecutorMetrics {
    /// Increments the counter for spawned critical tasks.
    pub(crate) fn inc_critical_tasks(&self, task: &'static str) {
        counter!("executor_spawn_critical_tasks_total", "task" => task).increment(1);
    }

    /// Increments the counter for spawned regular tasks.
    pub(crate) fn inc_regular_tasks(&self, task: &'static str) {
        counter!("executor_spawn_regular_tasks_total", "task" => task).increment(1);
    }

    /// Increments the counter for spawned regular blocking tasks.
    pub(crate) fn inc_regular_blocking_tasks(&self, task: &'static str) {
        counter!("executor_spawn_regular_blocking_tasks_total", "task" => task).increment(1);
    }

    /// Record that a critical task panicked.
    pub(crate) fn record_critical_panic(&self) {
        PANICKED_CRITICAL_TASKS_TOTAL.increment(1);
    }

    /// Get the finished critical tasks counter for drop tracking.
    pub(crate) fn finished_critical_tasks_total(&self, task: &'static str) -> Counter {
        counter!("executor_spawn_finished_critical_tasks_total", "task" => task)
    }

    /// Get the finished regular tasks counter for drop tracking.
    pub(crate) fn finished_regular_tasks_total(&self, task: &'static str) -> Counter {
        counter!("executor_spawn_finished_regular_tasks_total", "task" => task)
    }

    /// Get the finished regular blocking tasks counter for drop tracking.
    pub(crate) fn finished_regular_blocking_tasks_total(&self, task: &'static str) -> Counter {
        counter!("executor_spawn_finished_regular_blocking_tasks_total", "task" => task)
    }

    /// Running task gauge with type, task name, and graceful shutdown labels.
    pub(crate) fn running_task(
        &self,
        task: &'static str,
        task_type: &'static str,
        graceful: bool,
    ) -> Gauge {
        let graceful_label = if graceful { "true" } else { "false" };
        gauge!(
            "executor_tasks_running",
            "type" => task_type,
            "task" => task,
            "graceful" => graceful_label,
        )
    }
}
