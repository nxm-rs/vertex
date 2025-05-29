//! Prometheus metrics system for Vertex Swarm

use crate::PrometheusConfig;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use metrics_util::layers::{PrefixLayer, Stack};
use once_cell::sync::OnceCell;
use std::sync::atomic::{AtomicBool, Ordering};

/// Global prometheus recorder
static PROMETHEUS_RECORDER: OnceCell<PrometheusRecorder> = OnceCell::new();

/// Install the prometheus recorder as the global metrics recorder
pub fn install_prometheus_recorder() -> PrometheusRecorder {
    PROMETHEUS_RECORDER.get_or_init(|| {
        PrometheusRecorder::install().expect("Failed to install prometheus recorder")
    })
    .clone()
}

/// Handle to the prometheus metrics recorder
#[derive(Clone)]
pub struct PrometheusRecorder {
    /// The handle to the prometheus recorder
    handle: PrometheusHandle,
    /// Whether upkeep has been started
    upkeep_started: AtomicBool,
}

impl std::fmt::Debug for PrometheusRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrometheusRecorder")
            .field("upkeep_started", &self.upkeep_started.load(Ordering::Relaxed))
            .finish()
    }
}

impl PrometheusRecorder {
    /// Install the prometheus recorder with a default configuration
    pub fn install() -> eyre::Result<Self> {
        Self::install_with_config(PrometheusConfig::default())
    }

    /// Install the prometheus recorder with the given configuration
    pub fn install_with_config(config: PrometheusConfig) -> eyre::Result<Self> {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();

        // Configure the metrics stack with prefix
        Stack::new(recorder)
            .push(PrefixLayer::new(&config.prefix))
            .install()?;

        Ok(Self {
            handle,
            upkeep_started: AtomicBool::new(false),
        })
    }

    /// Get the prometheus handle
    pub fn handle(&self) -> &PrometheusHandle {
        &self.handle
    }

    /// Start the upkeep task for the prometheus recorder
    pub fn spawn_upkeep(&self, interval_secs: u64) {
        if self
            .upkeep_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            // Upkeep already started
            return;
        }

        let handle = self.handle.clone();
        tokio::spawn(async move {
            let interval = std::time::Duration::from_secs(interval_secs);
            loop {
                tokio::time::sleep(interval).await;
                handle.run_upkeep();
            }
        });
    }
}

/// Define common metric types
pub mod metrics {
    use metrics::{counter, gauge, histogram};

    /// Module for chunk-related metrics
    pub mod chunks {
        use super::*;

        /// Record a chunk being stored
        pub fn increment_stored() {
            counter!("chunks.stored").increment(1);
        }

        /// Record a chunk being retrieved
        pub fn increment_retrieved() {
            counter!("chunks.retrieved").increment(1);
        }

        /// Set the total number of chunks stored
        pub fn set_total_stored(count: u64) {
            gauge!("chunks.total_stored").set(count as f64);
        }

        /// Record chunk storage time
        pub fn observe_storage_time(duration_ms: f64) {
            histogram!("chunks.storage_time_ms").record(duration_ms);
        }

        /// Record chunk retrieval time
        pub fn observe_retrieval_time(duration_ms: f64) {
            histogram!("chunks.retrieval_time_ms").record(duration_ms);
        }
    }

    /// Module for network-related metrics
    pub mod network {
        use super::*;

        /// Set the number of connected peers
        pub fn set_connected_peers(count: u64) {
            gauge!("network.connected_peers").set(count as f64);
        }

        /// Record bytes sent
        pub fn add_bytes_sent(bytes: u64) {
            counter!("network.bytes_sent").increment(bytes);
        }

        /// Record bytes received
        pub fn add_bytes_received(bytes: u64) {
            counter!("network.bytes_received").increment(bytes);
        }

        /// Set the current upload rate in bytes per second
        pub fn set_upload_rate(bytes_per_sec: f64) {
            gauge!("network.upload_rate_bps").set(bytes_per_sec);
        }

        /// Set the current download rate in bytes per second
        pub fn set_download_rate(bytes_per_sec: f64) {
            gauge!("network.download_rate_bps").set(bytes_per_sec);
        }

        /// Record message send time
        pub fn observe_message_send_time(duration_ms: f64) {
            histogram!("network.message_send_time_ms").record(duration_ms);
        }

        /// Record message receive time
        pub fn observe_message_receive_time(duration_ms: f64) {
            histogram!("network.message_receive_time_ms").record(duration_ms);
        }
    }

    /// Module for bandwidth-related metrics
    pub mod bandwidth {
        use super::*;

        /// Set the free bandwidth allowance remaining in bytes
        pub fn set_free_allowance_remaining(bytes: u64) {
            gauge!("bandwidth.free_allowance_remaining").set(bytes as f64);
        }

        /// Set the current price per byte
        pub fn set_price_per_byte(price: f64) {
            gauge!("bandwidth.price_per_byte").set(price);
        }

        /// Record a payment being sent
        pub fn increment_payments_sent() {
            counter!("bandwidth.payments_sent").increment(1);
        }

        /// Record a payment being received
        pub fn increment_payments_received() {
            counter!("bandwidth.payments_received").increment(1);
        }

        /// Set the total amount of payments sent
        pub fn set_total_payments_sent(amount: u64) {
            gauge!("bandwidth.total_payments_sent").set(amount as f64);
        }

        /// Set the total amount of payments received
        pub fn set_total_payments_received(amount: u64) {
            gauge!("bandwidth.total_payments_received").set(amount as f64);
        }
    }

    /// Module for node-related metrics
    pub mod node {
        use super::*;

        /// Set the neighborhood depth
        pub fn set_neighborhood_depth(depth: u8) {
            gauge!("node.neighborhood_depth").set(depth as f64);
        }

        /// Set the estimated network size
        pub fn set_estimated_network_size(size: u64) {
            gauge!("node.estimated_network_size").set(size as f64);
        }

        /// Record CPU usage percentage
        pub fn set_cpu_usage(percentage: f64) {
            gauge!("node.cpu_usage_percent").set(percentage);
        }

        /// Record memory usage in bytes
        pub fn set_memory_usage(bytes: u64) {
            gauge!("node.memory_usage").set(bytes as f64);
        }

        /// Record disk usage in bytes
        pub fn set_disk_usage(bytes: u64) {
            gauge!("node.disk_usage").set(bytes as f64);
        }

        /// Record uptime in seconds
        pub fn set_uptime(seconds: u64) {
            gauge!("node.uptime_seconds").set(seconds as f64);
        }
    }
}
