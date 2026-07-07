//! Launch-path histogram bucket aggregation.

use vertex_metrics::buckets::HistogramBucketConfig;

/// Every bucket config the launch path wires: the protocol cone, the redb
/// storage backend, and the gRPC request-observability layer.
pub fn histogram_buckets() -> Vec<HistogramBucketConfig> {
    vertex_swarm_node::metrics::HISTOGRAM_BUCKETS
        .iter()
        .flat_map(|group| group.iter().copied())
        .chain(
            vertex_storage_redb::metrics::HISTOGRAM_BUCKETS
                .iter()
                .copied(),
        )
        .chain(vertex_swarm_rpc::HISTOGRAM_BUCKETS.iter().copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn buckets_are_unique_and_span_both_tiers() {
        let buckets = histogram_buckets();
        let suffixes: HashSet<&str> = buckets.iter().map(|c| c.suffix).collect();
        assert_eq!(
            suffixes.len(),
            buckets.len(),
            "duplicate histogram suffix across the launch aggregate",
        );
        // A protocol-cone sentinel, a storage-backend sentinel, and the gRPC
        // request-observability sentinel must all appear.
        assert!(suffixes.contains("handshake_duration_seconds"));
        assert!(suffixes.contains("db_operation_duration_seconds"));
        assert!(suffixes.contains("grpc_request_duration_seconds"));
    }
}
