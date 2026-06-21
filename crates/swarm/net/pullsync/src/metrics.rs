//! Metric name suffixes for pullsync.
//!
//! Exchange-level success and error counts come from the headered-stream layer;
//! these name the range-exchange volume counters and the per-page latency
//! histogram a behaviour layer records.

/// Total chunk descriptors offered across all pages.
pub const CHUNKS_OFFERED_TOTAL: &str = "swarm.pullsync.chunks_offered_total";

/// Total chunks selected by `Want` replies.
pub const CHUNKS_WANTED_TOTAL: &str = "swarm.pullsync.chunks_wanted_total";

/// Total chunks delivered in answer to `Want` replies.
pub const CHUNKS_DELIVERED_TOTAL: &str = "swarm.pullsync.chunks_delivered_total";

/// Latency of a single offer-to-deliveries page exchange.
pub const PAGE_DURATION: &str = "swarm.pullsync.page_duration";
