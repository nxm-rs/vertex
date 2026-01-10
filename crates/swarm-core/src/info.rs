//! Node information types

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use vertex_swarm_api::{
    network::NetworkStatus,
    node::{IncentiveStatus, NodeMode},
    storage::StorageStats,
};

/// Detailed information about the node's state
#[derive(Debug, Clone)]
pub struct NodeInfo {
    /// Node operating mode
    pub mode: NodeMode,

    /// Whether the node is connected to the network
    pub connected: bool,

    /// Time the node was started
    pub start_time: SystemTime,

    /// Node uptime in seconds
    pub uptime: u64,

    /// Network information
    pub network: NetworkStatus,

    /// Storage information (for full and incentivized nodes)
    pub storage: Option<StorageStats>,

    /// Incentives information (for incentivized nodes)
    pub incentives: Option<IncentiveStatus>,
}

impl NodeInfo {
    /// Create a new NodeInfo instance
    pub fn new(
        mode: NodeMode,
        connected: bool,
        start_time: SystemTime,
        network: NetworkStatus,
        storage: Option<StorageStats>,
        incentives: Option<IncentiveStatus>,
    ) -> Self {
        let uptime = SystemTime::now()
            .duration_since(start_time)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        Self {
            mode,
            connected,
            start_time,
            uptime,
            network,
            storage,
            incentives,
        }
    }
}

/// Performance metrics for the node
#[derive(Debug, Clone)]
pub struct NodeMetrics {
    /// Number of chunks retrieved
    pub chunks_retrieved: u64,

    /// Number of chunks stored
    pub chunks_stored: u64,

    /// Network bytes sent
    pub bytes_sent: u64,

    /// Network bytes received
    pub bytes_received: u64,

    /// Current upload bandwidth (bytes/second)
    pub upload_bandwidth: u64,

    /// Current download bandwidth (bytes/second)
    pub download_bandwidth: u64,

    /// Storage usage percentage
    pub storage_usage_percent: f64,

    /// Cache hit ratio (0.0-1.0)
    pub cache_hit_ratio: f64,

    /// Average retrieval latency in milliseconds
    pub avg_retrieval_latency_ms: u64,
}

impl Default for NodeMetrics {
    fn default() -> Self {
        Self {
            chunks_retrieved: 0,
            chunks_stored: 0,
            bytes_sent: 0,
            bytes_received: 0,
            upload_bandwidth: 0,
            download_bandwidth: 0,
            storage_usage_percent: 0.0,
            cache_hit_ratio: 0.0,
            avg_retrieval_latency_ms: 0,
        }
    }
}

/// Node metrics collector
pub struct MetricsCollector {
    /// Current metrics
    metrics: NodeMetrics,

    /// Last update time
    last_update: Instant,

    /// Last bytes sent
    last_bytes_sent: u64,

    /// Last bytes received
    last_bytes_received: u64,

    /// Total cache hits
    cache_hits: u64,

    /// Total cache lookups
    cache_lookups: u64,

    /// Sum of retrieval latencies in milliseconds
    retrieval_latency_sum: u64,

    /// Count of retrieval latency measurements
    retrieval_latency_count: u64,
}

impl MetricsCollector {
    /// Create a new metrics collector
    pub fn new() -> Self {
        Self {
            metrics: NodeMetrics::default(),
            last_update: Instant::now(),
            last_bytes_sent: 0,
            last_bytes_received: 0,
            cache_hits: 0,
            cache_lookups: 0,
            retrieval_latency_sum: 0,
            retrieval_latency_count: 0,
        }
    }

    /// Update metrics with new values
    pub fn update(&mut self, bytes_sent: u64, bytes_received: u64, storage_usage_percent: f64) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();

        if elapsed > 0.0 {
            let bytes_sent_delta = bytes_sent.saturating_sub(self.last_bytes_sent);
            let bytes_received_delta = bytes_received.saturating_sub(self.last_bytes_received);

            self.metrics.upload_bandwidth = (bytes_sent_delta as f64 / elapsed) as u64;
            self.metrics.download_bandwidth = (bytes_received_delta as f64 / elapsed) as u64;

            self.last_bytes_sent = bytes_sent;
            self.last_bytes_received = bytes_received;
            self.last_update = now;
        }

        self.metrics.bytes_sent = bytes_sent;
        self.metrics.bytes_received = bytes_received;
        self.metrics.storage_usage_percent = storage_usage_percent;

        if self.cache_lookups > 0 {
            self.metrics.cache_hit_ratio = self.cache_hits as f64 / self.cache_lookups as f64;
        }

        if self.retrieval_latency_count > 0 {
            self.metrics.avg_retrieval_latency_ms =
                self.retrieval_latency_sum / self.retrieval_latency_count;
        }
    }

    /// Record a chunk retrieval
    pub fn record_retrieval(&mut self, cache_hit: bool, latency_ms: u64) {
        self.metrics.chunks_retrieved += 1;
        self.cache_lookups += 1;

        if cache_hit {
            self.cache_hits += 1;
        }

        self.retrieval_latency_sum += latency_ms;
        self.retrieval_latency_count += 1;
    }

    /// Record a chunk storage
    pub fn record_storage(&mut self) {
        self.metrics.chunks_stored += 1;
    }

    /// Get current metrics
    pub fn metrics(&self) -> &NodeMetrics {
        &self.metrics
    }
}
