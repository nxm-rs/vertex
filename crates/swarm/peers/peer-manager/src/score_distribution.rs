//! Event-driven per-bucket gauge tracking of peer score distribution.

use std::sync::atomic::{AtomicI64, Ordering};

use metrics::gauge;

/// Bucket boundaries matching the old PEER_SCORE histogram.
const BOUNDARIES: [f64; 12] = [
    -100.0, -50.0, -10.0, -1.0, 0.0, 1.0, 5.0, 10.0, 25.0, 50.0, 75.0, 100.0,
];

/// Upper-bound labels for each bucket (numeric for heatmap Y-axis sorting).
///
/// Each label is the upper boundary of the range. The first bucket uses the
/// first boundary as its label (scores below that value), and the last bucket
/// uses "+Inf" (scores at or above the last boundary).
const RANGE_LABELS: [&str; 13] = [
    "-100", "-50", "-10", "-1", "0", "1", "5", "10", "25", "50", "75", "100", "+Inf",
];

const NUM_BUCKETS: usize = 13;

/// Per-bucket gauge counters for peer score distribution.
///
/// Maintained via O(1) event-driven updates rather than periodic O(n) iteration.
/// Each bucket tracks how many peers currently have a score in that range.
pub struct ScoreDistribution {
    buckets: [AtomicI64; NUM_BUCKETS],
}

impl ScoreDistribution {
    pub fn new() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicI64::new(0)),
        }
    }

    /// Track a newly added peer's initial score.
    pub fn on_peer_added(&self, score: f64) {
        self.increment(Self::bucket_index(score));
    }

    /// Track a removed peer's final score.
    pub fn on_peer_removed(&self, score: f64) {
        self.decrement(Self::bucket_index(score));
    }

    /// Track a score change. Only adjusts counters if the bucket changed.
    pub fn on_score_changed(&self, old_score: f64, new_score: f64) {
        let old_idx = Self::bucket_index(old_score);
        let new_idx = Self::bucket_index(new_score);
        if old_idx != new_idx {
            self.decrement(old_idx);
            self.increment(new_idx);
        }
    }

    /// Emit cumulative gauge values to the metrics system.
    ///
    /// Values are emitted in cumulative histogram format (each bucket includes
    /// all peers with score <= boundary) so that Grafana's heatmap panel with
    /// `rows_layout="le"` can correctly sort and delta the buckets.
    pub fn push_gauges(&self) {
        let mut cumulative: i64 = 0;
        for (i, &label) in RANGE_LABELS.iter().enumerate() {
            cumulative += self.buckets[i].load(Ordering::Relaxed);
            gauge!("peer_manager_score_distribution", "le" => label).set(cumulative as f64);
        }
    }

    fn bucket_index(score: f64) -> usize {
        for (i, &boundary) in BOUNDARIES.iter().enumerate() {
            if score < boundary {
                return i;
            }
        }
        // score >= last boundary (100.0)
        NUM_BUCKETS - 1
    }

    fn increment(&self, idx: usize) {
        self.buckets[idx].fetch_add(1, Ordering::Relaxed);
    }

    fn decrement(&self, idx: usize) {
        self.buckets[idx].fetch_sub(1, Ordering::Relaxed);
    }
}

impl Default for ScoreDistribution {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bucket_index_boundaries() {
        // below -100
        assert_eq!(ScoreDistribution::bucket_index(-200.0), 0);
        assert_eq!(ScoreDistribution::bucket_index(-100.1), 0);

        // -100 to -50
        assert_eq!(ScoreDistribution::bucket_index(-100.0), 1);
        assert_eq!(ScoreDistribution::bucket_index(-50.1), 1);

        // -50 to -10
        assert_eq!(ScoreDistribution::bucket_index(-50.0), 2);
        assert_eq!(ScoreDistribution::bucket_index(-10.1), 2);

        // -10 to -1
        assert_eq!(ScoreDistribution::bucket_index(-10.0), 3);
        assert_eq!(ScoreDistribution::bucket_index(-1.1), 3);

        // -1 to 0
        assert_eq!(ScoreDistribution::bucket_index(-1.0), 4);
        assert_eq!(ScoreDistribution::bucket_index(-0.1), 4);

        // 0 to 1
        assert_eq!(ScoreDistribution::bucket_index(0.0), 5);
        assert_eq!(ScoreDistribution::bucket_index(0.5), 5);

        // 1 to 5
        assert_eq!(ScoreDistribution::bucket_index(1.0), 6);
        assert_eq!(ScoreDistribution::bucket_index(4.9), 6);

        // 5 to 10
        assert_eq!(ScoreDistribution::bucket_index(5.0), 7);
        assert_eq!(ScoreDistribution::bucket_index(9.9), 7);

        // 10 to 25
        assert_eq!(ScoreDistribution::bucket_index(10.0), 8);
        assert_eq!(ScoreDistribution::bucket_index(24.9), 8);

        // 25 to 50
        assert_eq!(ScoreDistribution::bucket_index(25.0), 9);
        assert_eq!(ScoreDistribution::bucket_index(49.9), 9);

        // 50 to 75
        assert_eq!(ScoreDistribution::bucket_index(50.0), 10);
        assert_eq!(ScoreDistribution::bucket_index(74.9), 10);

        // 75 to 100
        assert_eq!(ScoreDistribution::bucket_index(75.0), 11);
        assert_eq!(ScoreDistribution::bucket_index(99.9), 11);

        // above 100
        assert_eq!(ScoreDistribution::bucket_index(100.0), 12);
        assert_eq!(ScoreDistribution::bucket_index(500.0), 12);
    }

    #[test]
    fn test_on_peer_added_removed() {
        let dist = ScoreDistribution::new();

        dist.on_peer_added(0.5); // bucket 5 (0_to_1)
        dist.on_peer_added(0.8); // bucket 5
        dist.on_peer_added(50.0); // bucket 10 (50_to_75)

        assert_eq!(dist.buckets[5].load(Ordering::Relaxed), 2);
        assert_eq!(dist.buckets[10].load(Ordering::Relaxed), 1);

        dist.on_peer_removed(0.5);
        assert_eq!(dist.buckets[5].load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_on_score_changed_same_bucket() {
        let dist = ScoreDistribution::new();
        dist.on_peer_added(0.5);

        // Score changes within the same bucket should be a no-op
        dist.on_score_changed(0.5, 0.9);
        assert_eq!(dist.buckets[5].load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_on_score_changed_cross_bucket() {
        let dist = ScoreDistribution::new();
        dist.on_peer_added(0.5); // bucket 5

        dist.on_score_changed(0.5, 10.0); // bucket 5 -> bucket 8
        assert_eq!(dist.buckets[5].load(Ordering::Relaxed), 0);
        assert_eq!(dist.buckets[8].load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_push_gauges_does_not_panic() {
        let dist = ScoreDistribution::new();
        dist.on_peer_added(0.0);
        dist.on_peer_added(50.0);
        dist.push_gauges();
    }
}
