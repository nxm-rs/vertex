//! Aggregated histogram bucket requirements for the protocol cone.

use vertex_metrics::buckets::HistogramBucketConfig;

/// Bucket configs from every instrumented protocol crate, one group per crate.
pub const HISTOGRAM_BUCKETS: &[&[HistogramBucketConfig]] = &[
    vertex_swarm_net_headers::metrics::HISTOGRAM_BUCKETS,
    vertex_swarm_topology::metrics::HISTOGRAM_BUCKETS,
    vertex_swarm_net_handshake::metrics::HISTOGRAM_BUCKETS,
    vertex_swarm_net_hive::metrics::HISTOGRAM_BUCKETS,
    vertex_swarm_net_identify::metrics::HISTOGRAM_BUCKETS,
];

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn suffixes_are_globally_unique_and_groups_non_empty() {
        let mut seen = HashSet::new();
        for group in HISTOGRAM_BUCKETS {
            assert!(!group.is_empty(), "a protocol bucket group is empty");
            for config in *group {
                assert!(
                    seen.insert(config.suffix),
                    "duplicate histogram suffix: {:?}",
                    config.suffix,
                );
            }
        }
    }
}
