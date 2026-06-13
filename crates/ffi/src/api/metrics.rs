//! Pull-based metrics snapshot surface for embedding hosts.
//!
//! An embedded Vertex client records its counters, gauges, and histograms
//! through the `metrics` facade. Without an installed recorder those records
//! hit the global no-op recorder and the host sees nothing. [`init_metrics`]
//! installs a lightweight in-process recorder; [`metrics_snapshot`] reads its
//! current state back as a typed [`MetricsSnapshot`], so a Dart or other native
//! host can drive a diagnostics view or feed its own telemetry pipeline.
//!
//! The surface is pull-based by design: the host polls when it cares and the
//! node never exports on its own schedule. The snapshot is generic over metric
//! names (flat vectors of name, labels, value), so new instrumentation anywhere
//! in the workspace flows through without an FFI change. Metric names match the
//! reference in `docs/observability/profiling.md`, without the `vertex_`
//! exposition prefix. The design note is `docs/observability/ffi-metrics.md`.
//!
//! Histograms carry a bounded running summary (observation count and value
//! sum), not quantiles: two consecutive snapshots give rates and interval
//! averages at a fixed two-atomics cost per series, no matter how rarely the
//! host polls.

use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};

use flutter_rust_bridge::frb;
use metrics::atomics::AtomicU64;
use metrics::{
    Counter, Gauge, Histogram, HistogramFn, Key, KeyName, Metadata, Recorder, SharedString, Unit,
};
use metrics_util::registry::{Registry, Storage};

use super::logging::now_ms;
use crate::error::FfiError;

/// The registry behind the installed recorder, kept for snapshot reads.
///
/// `metrics` allows exactly one global recorder per process. Like the logging
/// guard, the slot is reserved before installation so a racing second
/// [`init_metrics`] call loses cleanly with an error instead of a panic.
static REGISTRY: OnceLock<Arc<Registry<Key, SummaryStorage>>> = OnceLock::new();

/// A counter's current value.
///
/// Plain data only: the metric name, its label pairs, and the monotonic count.
#[frb(non_opaque)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterValue {
    /// Metric name, for example `topology_connections_total`.
    pub name: String,
    /// Label key-value pairs identifying the series.
    pub labels: Vec<(String, String)>,
    /// Monotonically increasing count since [`init_metrics`].
    pub value: u64,
}

/// A gauge's current value.
#[frb(non_opaque)]
#[derive(Debug, Clone, PartialEq)]
pub struct GaugeValue {
    /// Metric name, for example `topology_connected_peers`.
    pub name: String,
    /// Label key-value pairs identifying the series.
    pub labels: Vec<(String, String)>,
    /// The gauge's current value.
    pub value: f64,
}

/// A histogram's bounded running summary.
///
/// The count and sum since [`init_metrics`]: the delta between two snapshots
/// yields a rate and an interval average. Quantiles are deliberately absent;
/// see the module docs.
#[frb(non_opaque)]
#[derive(Debug, Clone, PartialEq)]
pub struct HistogramValue {
    /// Metric name, for example `topology_dial_duration_seconds`.
    pub name: String,
    /// Label key-value pairs identifying the series.
    pub labels: Vec<(String, String)>,
    /// Number of observations recorded.
    pub count: u64,
    /// Sum of all observed values.
    pub sum: f64,
}

/// A point-in-time view of every metric the node has recorded.
///
/// Entries are sorted by name then labels, so consecutive snapshots line up
/// for delta computation and a diagnostics view renders in a stable order.
#[frb(non_opaque)]
#[derive(Debug, Clone, PartialEq)]
pub struct MetricsSnapshot {
    /// Snapshot time in milliseconds since the Unix epoch.
    pub timestamp_ms: i64,
    /// All counters, sorted.
    pub counters: Vec<CounterValue>,
    /// All gauges, sorted.
    pub gauges: Vec<GaugeValue>,
    /// All histogram summaries, sorted.
    pub histograms: Vec<HistogramValue>,
}

/// Bounded running summary backing one histogram series.
///
/// Two atomics regardless of observation volume: a count and a sum (stored as
/// `f64` bits). This is what keeps the recorder safe on a host that rarely or
/// never polls, where a value-retaining histogram would grow without bound.
#[frb(ignore)]
#[derive(Debug, Default)]
struct HistogramSummary {
    count: AtomicU64,
    sum_bits: AtomicU64,
}

impl HistogramSummary {
    fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    fn sum(&self) -> f64 {
        f64::from_bits(self.sum_bits.load(Ordering::Relaxed))
    }
}

impl HistogramFn for HistogramSummary {
    fn record(&self, value: f64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        // Atomic f64 addition: CAS on the bit pattern.
        let mut current = self.sum_bits.load(Ordering::Relaxed);
        loop {
            let next = (f64::from_bits(current) + value).to_bits();
            match self.sum_bits.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }
}

/// Registry storage with a bounded footprint per series.
///
/// Counters and gauges are the plain atomics the `metrics` facade already
/// knows how to drive; histograms are [`HistogramSummary`] instead of
/// `metrics-util`'s value-retaining bucket.
#[frb(ignore)]
struct SummaryStorage;

impl<K> Storage<K> for SummaryStorage {
    type Counter = Arc<AtomicU64>;
    type Gauge = Arc<AtomicU64>;
    type Histogram = Arc<HistogramSummary>;

    fn counter(&self, _: &K) -> Self::Counter {
        Arc::new(AtomicU64::new(0))
    }

    fn gauge(&self, _: &K) -> Self::Gauge {
        Arc::new(AtomicU64::new(0))
    }

    fn histogram(&self, _: &K) -> Self::Histogram {
        Arc::new(HistogramSummary::default())
    }
}

/// The recorder [`init_metrics`] installs.
///
/// A thin shim over the shared registry: registration hands the facade a
/// handle into [`SummaryStorage`], descriptions and units are dropped (the
/// snapshot carries values, not metadata).
#[frb(ignore)]
struct SnapshotRecorder {
    registry: Arc<Registry<Key, SummaryStorage>>,
}

impl Recorder for SnapshotRecorder {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
        self.registry
            .get_or_create_counter(key, |c| Counter::from_arc(c.clone()))
    }

    fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
        self.registry
            .get_or_create_gauge(key, |g| Gauge::from_arc(g.clone()))
    }

    fn register_histogram(&self, key: &Key, _: &Metadata<'_>) -> Histogram {
        self.registry
            .get_or_create_histogram(key, |h| Histogram::from_arc(h.clone()))
    }
}

/// Split a series key into the flat name and label shape the snapshot carries.
fn name_and_labels(key: &Key) -> (String, Vec<(String, String)>) {
    let name = key.name().to_string();
    let labels = key
        .labels()
        .map(|l| (l.key().to_string(), l.value().to_string()))
        .collect();
    (name, labels)
}

/// Read a snapshot out of a registry.
fn snapshot_from(registry: &Registry<Key, SummaryStorage>) -> MetricsSnapshot {
    let mut counters = Vec::new();
    registry.visit_counters(|key, counter| {
        let (name, labels) = name_and_labels(key);
        counters.push(CounterValue {
            name,
            labels,
            value: counter.load(Ordering::Relaxed),
        });
    });

    let mut gauges = Vec::new();
    registry.visit_gauges(|key, gauge| {
        let (name, labels) = name_and_labels(key);
        gauges.push(GaugeValue {
            name,
            labels,
            value: f64::from_bits(gauge.load(Ordering::Relaxed)),
        });
    });

    let mut histograms = Vec::new();
    registry.visit_histograms(|key, histogram| {
        let (name, labels) = name_and_labels(key);
        histograms.push(HistogramValue {
            name,
            labels,
            count: histogram.count(),
            sum: histogram.sum(),
        });
    });

    counters.sort_by(|a, b| (&a.name, &a.labels).cmp(&(&b.name, &b.labels)));
    gauges.sort_by(|a, b| (&a.name, &a.labels).cmp(&(&b.name, &b.labels)));
    histograms.sort_by(|a, b| (&a.name, &a.labels).cmp(&(&b.name, &b.labels)));

    MetricsSnapshot {
        timestamp_ms: now_ms(),
        counters,
        gauges,
        histograms,
    }
}

/// Initialize metrics collection for the embedded client.
///
/// Installs a process-global recorder that accumulates every counter, gauge,
/// and histogram the node records, for [`metrics_snapshot`] to read back on
/// demand. Call once, before [`super::client::VertexClient::build`], so
/// activity from the first dial onward is captured.
///
/// Single-shot: `metrics` permits one global recorder per process, so a second
/// call returns [`FfiError::MetricsAlreadyInitialized`] without disturbing the
/// installed recorder.
#[frb]
pub fn init_metrics() -> Result<(), FfiError> {
    let registry = Arc::new(Registry::new(SummaryStorage));

    // Reserve the init slot first so a racing second caller loses cleanly
    // before it ever tries to install a recorder.
    if REGISTRY.set(registry.clone()).is_err() {
        return Err(FfiError::MetricsAlreadyInitialized);
    }

    metrics::set_global_recorder(SnapshotRecorder { registry }).map_err(|e| FfiError::Metrics {
        reason: format!("install recorder: {e}"),
    })
}

/// Read the current value of every metric the node has recorded.
///
/// Returns a point-in-time [`MetricsSnapshot`]; requires a prior
/// [`init_metrics`] call, otherwise [`FfiError::MetricsNotInitialized`].
///
/// Each call walks every registered series and allocates the returned vectors,
/// so it is cheap but not free. Poll on demand (every 1 to 5 seconds while a
/// diagnostics view is visible), stop when the data is not being looked at,
/// and never poll from a background task on a mobile host.
#[frb]
pub fn metrics_snapshot() -> Result<MetricsSnapshot, FfiError> {
    let registry = REGISTRY.get().ok_or(FfiError::MetricsNotInitialized)?;
    Ok(snapshot_from(registry))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn histogram_summary_accumulates_count_and_sum() {
        let summary = HistogramSummary::default();
        summary.record(0.25);
        summary.record(0.75);
        summary.record(1.0);
        assert_eq!(summary.count(), 3);
        assert!((summary.sum() - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn snapshot_reads_recorded_values() {
        // Drive the recorder through the facade's scoped install, so the test
        // exercises the same registration path as the process-global recorder
        // without touching global state.
        let registry = Arc::new(Registry::new(SummaryStorage));
        let recorder = SnapshotRecorder {
            registry: registry.clone(),
        };
        metrics::with_local_recorder(&recorder, || {
            metrics::counter!("test_connections_total", "outcome" => "ok").increment(2);
            metrics::gauge!("test_connected_peers").set(7.0);
            metrics::histogram!("test_dial_duration_seconds").record(0.25);
            metrics::histogram!("test_dial_duration_seconds").record(0.75);
        });

        let snapshot = snapshot_from(&registry);

        assert_eq!(
            snapshot.counters,
            vec![CounterValue {
                name: "test_connections_total".to_string(),
                labels: vec![("outcome".to_string(), "ok".to_string())],
                value: 2,
            }]
        );
        assert_eq!(
            snapshot.gauges,
            vec![GaugeValue {
                name: "test_connected_peers".to_string(),
                labels: vec![],
                value: 7.0,
            }]
        );
        assert_eq!(
            snapshot.histograms,
            vec![HistogramValue {
                name: "test_dial_duration_seconds".to_string(),
                labels: vec![],
                count: 2,
                sum: 1.0,
            }]
        );
        assert!(snapshot.timestamp_ms > 0);
    }

    #[test]
    fn snapshot_sorts_series_for_stable_output() {
        let registry = Arc::new(Registry::new(SummaryStorage));
        let recorder = SnapshotRecorder {
            registry: registry.clone(),
        };
        metrics::with_local_recorder(&recorder, || {
            metrics::counter!("zzz_total").increment(1);
            metrics::counter!("aaa_total", "b" => "2").increment(1);
            metrics::counter!("aaa_total", "b" => "1").increment(1);
        });

        let names: Vec<(String, Vec<(String, String)>)> = snapshot_from(&registry)
            .counters
            .into_iter()
            .map(|c| (c.name, c.labels))
            .collect();
        assert_eq!(
            names,
            vec![
                (
                    "aaa_total".to_string(),
                    vec![("b".to_string(), "1".to_string())]
                ),
                (
                    "aaa_total".to_string(),
                    vec![("b".to_string(), "2".to_string())]
                ),
                ("zzz_total".to_string(), vec![]),
            ]
        );
    }

    #[test]
    fn double_init_guard_does_not_panic() {
        // Drive the guard directly: the real `init_metrics` installs a global
        // recorder, which a unit test must not do. The guard is the part that
        // must never panic on a second call.
        let guard: OnceLock<()> = OnceLock::new();
        assert!(guard.set(()).is_ok());
        assert!(guard.set(()).is_err());
    }
}
