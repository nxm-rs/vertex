//! Per-proximity-order retrieval-latency estimate that paces the staggered race.
//!
//! Retrieval latency is dominated by the forwarding-chain length, which tracks
//! how close the serving entry peer already is to the chunk: a chunk in the
//! peer's neighbourhood returns in one hop, a sparse-bin chunk forwards several.
//! Bucketing observed latency by `PO(serving_peer, chunk)` captures that, so the
//! race paces its stagger to the distance actually being traversed rather than to a
//! single constant. Single-hop ping latency is deliberately not folded in: it
//! reflects one connection, not a forwarding chain.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::RETRIEVAL_STAGGER;

/// Proximity-order buckets tracked. Proximity between two random addresses is
/// geometric (mostly small); deeper observations clamp into the last bucket.
const PO_BUCKETS: usize = 32;

/// EWMA smoothing shift: each sample moves the estimate by `1 / 2^SHIFT` toward
/// it, so the estimate tracks the recent distribution without chasing one outlier.
const EWMA_SHIFT: u32 = 3;

/// Multiplier on the observed round trip for the adaptive stagger: the next attempt
/// waits this many typical round trips, sitting above a head merely in flight
/// while still tracking the real distribution.
const HEDGE_RTT_MULTIPLIER: u32 = 2;

/// Floor on the adaptive stagger, so even a very low-RTT neighbourhood keeps a
/// hair of spacing between dispatched attempts rather than fanning out at once.
const HEDGE_STAGGER_FLOOR: Duration = Duration::from_millis(50);

/// Per-PO EWMA of observed originated-retrieval latency, in nanoseconds.
///
/// Lock-free and shared between the client service (which records a completed
/// retrieval keyed by `PO(serving_peer, chunk)`) and the chunk provider (which
/// reads an estimate to pace its race). A zero bucket means no sample yet.
#[derive(Debug)]
pub struct RetrievalLatency {
    buckets: [AtomicU64; PO_BUCKETS],
}

impl Default for RetrievalLatency {
    fn default() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl RetrievalLatency {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn bucket(po: u8) -> usize {
        (po as usize).min(PO_BUCKETS - 1)
    }

    /// Fold an observed retrieval `latency` for a chunk served at proximity `po`
    /// into that bucket's EWMA.
    pub(crate) fn record(&self, po: u8, latency: Duration) {
        let sample = latency.as_nanos().min(u64::MAX as u128) as u64;
        // `bucket` always indexes in range; the get is a non-panicking access.
        let Some(cell) = self.buckets.get(Self::bucket(po)) else {
            return;
        };
        // Relaxed read-blend-store: a racing sample at most drops one update,
        // immaterial for a smoothed estimate that only paces a timer.
        let prev = cell.load(Ordering::Relaxed);
        let next = if prev == 0 {
            sample
        } else {
            let delta = sample as i64 - prev as i64;
            (prev as i64 + (delta >> EWMA_SHIFT)).max(0) as u64
        };
        cell.store(next, Ordering::Relaxed);
    }

    /// The current latency estimate for proximity `po`, or `None` if unsampled.
    pub(crate) fn estimate(&self, po: u8) -> Option<Duration> {
        match self.buckets.get(Self::bucket(po))?.load(Ordering::Relaxed) {
            0 => None,
            nanos => Some(Duration::from_nanos(nanos)),
        }
    }
}

/// Derive the staggered-race interval from the candidates' per-PO latency
/// estimates: `clamp(k * median(estimates), floor, RETRIEVAL_STAGGER)`.
///
/// `RETRIEVAL_STAGGER` is the ceiling and the cold-start fallback, so the hedge
/// is never slower than the fixed constant, only faster on a low-RTT
/// neighbourhood. With no estimate (a cold node, or a chunk whose proximity has
/// not been seen) every candidate yields `None` and the constant stands.
pub(crate) fn adaptive_stagger(estimates: impl Iterator<Item = Option<Duration>>) -> Duration {
    let mut known: Vec<Duration> = estimates.flatten().collect();
    if known.is_empty() {
        return RETRIEVAL_STAGGER;
    }
    known.sort_unstable();
    match known.get(known.len() / 2) {
        Some(median) => median
            .saturating_mul(HEDGE_RTT_MULTIPLIER)
            .clamp(HEDGE_STAGGER_FLOOR, RETRIEVAL_STAGGER),
        None => RETRIEVAL_STAGGER,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_is_none_until_recorded_then_tracks_samples() {
        let lat = RetrievalLatency::new();
        assert_eq!(lat.estimate(8), None, "unsampled bucket is None");

        lat.record(8, Duration::from_millis(100));
        assert_eq!(
            lat.estimate(8),
            Some(Duration::from_millis(100)),
            "the first sample seeds the bucket exactly"
        );

        // Further samples blend toward the new value, not jump to it.
        lat.record(8, Duration::from_millis(900));
        let est = lat.estimate(8).unwrap();
        assert!(
            est > Duration::from_millis(100) && est < Duration::from_millis(900),
            "EWMA blends toward the new sample: {est:?}"
        );
    }

    #[test]
    fn buckets_are_keyed_by_proximity_order() {
        let lat = RetrievalLatency::new();
        lat.record(2, Duration::from_millis(800));
        lat.record(20, Duration::from_millis(40));
        // A near chunk (high PO) reads its own fast bucket; a far one its slow one.
        assert_eq!(lat.estimate(20), Some(Duration::from_millis(40)));
        assert_eq!(lat.estimate(2), Some(Duration::from_millis(800)));
        // An out-of-range PO clamps into the last bucket rather than panicking.
        assert_eq!(lat.estimate(255), lat.estimate((PO_BUCKETS - 1) as u8));
    }

    #[test]
    fn adaptive_stagger_falls_back_to_the_constant_when_cold() {
        let cold = adaptive_stagger([None, None].into_iter());
        assert_eq!(cold, RETRIEVAL_STAGGER, "no estimate keeps the constant");
    }

    #[test]
    fn adaptive_stagger_tracks_a_low_rtt_neighbourhood_down_to_the_floor() {
        let fast = adaptive_stagger(
            [
                Some(Duration::from_millis(10)),
                Some(Duration::from_millis(15)),
            ]
            .into_iter(),
        );
        assert_eq!(
            fast, HEDGE_STAGGER_FLOOR,
            "k * a few-ms median clamps up to the floor, far below the constant"
        );
    }

    #[test]
    fn adaptive_stagger_clamps_a_high_rtt_neighbourhood_to_the_constant() {
        let slow = adaptive_stagger([Some(Duration::from_millis(900))].into_iter());
        assert_eq!(
            slow, RETRIEVAL_STAGGER,
            "k * a high median clamps down to the ceiling, never slower than the constant"
        );
    }

    #[test]
    fn adaptive_stagger_hedges_at_the_multiplied_median_between_the_bounds() {
        // Median 400ms, k = 2 -> 800ms, inside [50ms, 1200ms].
        let mid = adaptive_stagger(
            [
                Some(Duration::from_millis(400)),
                Some(Duration::from_millis(400)),
                None,
            ]
            .into_iter(),
        );
        assert_eq!(mid, Duration::from_millis(800));
    }
}
