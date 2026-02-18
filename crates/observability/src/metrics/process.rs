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

/// Returns a hook that records jemalloc allocator stats on each call.
#[cfg(feature = "jemalloc")]
pub fn jemalloc_metrics_hook() -> impl Fn() + Send + Sync + 'static {
    use metrics::gauge;
    use tikv_jemalloc_ctl::{epoch, stats};

    move || {
        if epoch::advance().is_err() {
            return;
        }
        if let Ok(v) = stats::allocated::read() {
            gauge!("jemalloc.allocated_bytes").set(v as f64);
        }
        if let Ok(v) = stats::active::read() {
            gauge!("jemalloc.active_bytes").set(v as f64);
        }
        if let Ok(v) = stats::resident::read() {
            gauge!("jemalloc.resident_bytes").set(v as f64);
        }
        if let Ok(v) = stats::mapped::read() {
            gauge!("jemalloc.mapped_bytes").set(v as f64);
        }
        if let Ok(v) = stats::retained::read() {
            gauge!("jemalloc.retained_bytes").set(v as f64);
        }
    }
}
