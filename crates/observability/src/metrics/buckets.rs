//! Histogram bucket presets for common metric patterns.

/// Total handshake duration: 1ms–15s (13 buckets).
pub const DURATION_SECONDS: &[f64] = &[
    0.001, 0.005, 0.010, 0.025, 0.050, 0.100, 0.250, 0.500, 1.0, 2.5, 5.0, 10.0, 15.0,
];

/// Per-stage fine-grained duration: 0.1ms–2.5s (12 buckets).
pub const DURATION_FINE: &[f64] = &[
    0.0001, 0.0005, 0.001, 0.005, 0.010, 0.025, 0.050, 0.100, 0.250, 0.500, 1.0, 2.5,
];

/// Network round-trip duration: 10ms–30s (11 buckets).
pub const DURATION_NETWORK: &[f64] = &[
    0.010, 0.025, 0.050, 0.100, 0.250, 0.500, 1.0, 2.5, 5.0, 10.0, 30.0,
];

/// Lock contention hold time: 1us–50ms (10 buckets).
pub const LOCK_CONTENTION: &[f64] = &[
    0.000001, 0.000005, 0.00001, 0.00005, 0.0001, 0.0005, 0.001, 0.005, 0.010, 0.050,
];

/// Connection lifetime: 1s–1day (11 buckets).
pub const CONNECTION_LIFETIME: &[f64] = &[
    1.0, 10.0, 30.0, 60.0, 300.0, 600.0, 1800.0, 3600.0, 7200.0, 21600.0, 86400.0,
];

/// Poll loop iteration: 10us–1s (10 buckets).
pub const POLL_DURATION: &[f64] = &[
    0.00001, 0.0001, 0.0005, 0.001, 0.005, 0.010, 0.050, 0.100, 0.500, 1.0,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_sorted(name: &str, buckets: &[f64]) {
        for w in buckets.windows(2) {
            assert!(
                w[0] < w[1],
                "{name}: buckets not sorted at {:.6} >= {:.6}",
                w[0],
                w[1]
            );
        }
    }

    #[test]
    fn all_presets_sorted() {
        assert_sorted("DURATION_SECONDS", DURATION_SECONDS);
        assert_sorted("DURATION_FINE", DURATION_FINE);
        assert_sorted("DURATION_NETWORK", DURATION_NETWORK);
        assert_sorted("LOCK_CONTENTION", LOCK_CONTENTION);
        assert_sorted("CONNECTION_LIFETIME", CONNECTION_LIFETIME);
        assert_sorted("POLL_DURATION", POLL_DURATION);
    }
}
