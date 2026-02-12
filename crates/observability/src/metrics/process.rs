//! Process-level metrics collection.

use metrics_process::Collector;

/// Returns a hook that collects process metrics on each call.
///
/// Registers metric descriptions on first call, then samples process stats
/// (CPU, memory, file descriptors, threads) on each subsequent call.
pub fn process_metrics_hook() -> impl Fn() + Send + Sync + 'static {
    let collector = Collector::default();
    collector.describe();
    move || collector.collect()
}
