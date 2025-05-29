//! Custom metrics recorders

use metrics::{Key, Recorder, Unit};
use metrics_util::registry::{Registry, Storage};
use std::sync::Arc;

/// A recorder for metrics that allows accessing the underlying registry
#[derive(Debug, Clone)]
pub struct MetricsRecorder {
    /// Registry for storing metrics
    registry: Arc<Registry<Key, Storage>>,
}

impl MetricsRecorder {
    /// Create a new metrics recorder
    pub fn new() -> Self {
        Self {
            registry: Arc::new(Registry::default()),
        }
    }

    /// Get a snapshot of the current metrics
    pub fn snapshot(&self) -> Vec<MetricSnapshot> {
        let mut snapshots = Vec::new();

        self.registry.visit_counters(|key, counter| {
            snapshots.push(MetricSnapshot {
                name: key.name().to_string(),
                description: key.description().map(ToString::to_string),
                labels: key
                    .labels()
                    .map(|l| (l.key().to_string(), l.value().to_string()))
                    .collect(),
                unit: None,
                value: MetricValue::Counter(counter.get() as f64),
            });
        });

        self.registry.visit_gauges(|key, gauge| {
            snapshots.push(MetricSnapshot {
                name: key.name().to_string(),
                description: key.description().map(ToString::to_string),
                labels: key
                    .labels()
                    .map(|l| (l.key().to_string(), l.value().to_string()))
                    .collect(),
                unit: None,
                value: MetricValue::Gauge(gauge.get()),
            });
        });

        self.registry.visit_histograms(|key, histogram| {
            let values = histogram.get_values();
            let buckets = histogram.get_buckets().into_iter().zip(values).collect();

            snapshots.push(MetricSnapshot {
                name: key.name().to_string(),
                description: key.description().map(ToString::to_string),
                labels: key
                    .labels()
                    .map(|l| (l.key().to_string(), l.value().to_string()))
                    .collect(),
                unit: None,
                value: MetricValue::Histogram {
                    sum: histogram.get_sum(),
                    count: histogram.get_count(),
                    buckets,
                },
            });
        });

        snapshots
    }
}

impl Default for MetricsRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl Recorder for MetricsRecorder {
    fn register_counter(&self, key: &Key, _unit: Option<Unit>, description: Option<&str>) {
        let storage = Storage::new();
        let key = key.clone();
        if let Some(description) = description {
            let _ = key.set_description(description);
        }
        self.registry.register_counter(&key, storage);
    }

    fn register_gauge(&self, key: &Key, _unit: Option<Unit>, description: Option<&str>) {
        let storage = Storage::new();
        let key = key.clone();
        if let Some(description) = description {
            let _ = key.set_description(description);
        }
        self.registry.register_gauge(&key, storage);
    }

    fn register_histogram(&self, key: &Key, _unit: Option<Unit>, description: Option<&str>) {
        let storage = Storage::new();
        let key = key.clone();
        if let Some(description) = description {
            let _ = key.set_description(description);
        }
        self.registry.register_histogram(&key, storage);
    }

    fn increment_counter(&self, key: &Key, value: u64) {
        self.registry.increment_counter(key, value);
    }

    fn update_gauge(&self, key: &Key, value: f64) {
        self.registry.update_gauge(key, value);
    }

    fn record_histogram(&self, key: &Key, value: f64) {
        self.registry.record_histogram(key, value);
    }
}

/// A snapshot of a metric
#[derive(Debug, Clone)]
pub struct MetricSnapshot {
    /// Name of the metric
    pub name: String,
    /// Optional description
    pub description: Option<String>,
    /// Labels attached to the metric
    pub labels: Vec<(String, String)>,
    /// Optional unit
    pub unit: Option<String>,
    /// The metric value
    pub value: MetricValue,
}

/// Possible metric value types
#[derive(Debug, Clone)]
pub enum MetricValue {
    /// A counter value
    Counter(f64),
    /// A gauge value
    Gauge(f64),
    /// A histogram value
    Histogram {
        /// Sum of all observations
        sum: f64,
        /// Count of observations
        count: u64,
        /// Histogram buckets
        buckets: Vec<(f64, u64)>,
    },
}
