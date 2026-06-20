//! Consensus candidate-feed filters for the reserve sampler.
//!
//! A stamped reserve entry must pass three independent exclusion gates before it
//! can enter the sample. These gates are consensus load bearing: disagreeing with
//! the protocol on a single candidate computes a different sample and loses or is
//! slashed in the round. They are pure functions of the candidate and a single
//! round-consistent batch snapshot.
//!
//! The gates are independent drop conditions, so evaluation order does not affect
//! the sample; [`CandidateFilter::admit`] returns the first that fires only so
//! each exclusion class can be metered:
//!
//! 1. Future timestamp: the stamp's big-endian timestamp must not exceed the
//!    round `consensusTime`.
//! 2. Below minimum balance: batches whose normalised per-chunk balance is below
//!    the round `minimumBalance`, computed once per round so the exclusion set is
//!    uniform across candidates.
//! 3. Rogue chunk / valid stamp: only CAC and SOC shapes are admissible, and the
//!    stamp's batch must still be admissible in the snapshot. No per-candidate
//!    `ecrecover`; see [`CandidateFilter`].
//!
//! A passing candidate becomes a [`SampleItem`] carrying its winning stamp, ready
//! for [`reserve_sample`](crate::reserve_sample).

use nectar_primitives::AnyChunk;
use vertex_swarm_postage::{BatchId, PostageContext, Stamp};

use crate::anchor::SampleAnchor;
use crate::sample::SampleItem;

/// Why the candidate filter excluded a stamped reserve entry. An explicit reason
/// (rather than a `bool`) so callers can meter each exclusion class.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterRejection {
    /// The stamp's big-endian timestamp exceeded the round `consensusTime`.
    FutureTimestamp,
    /// The candidate's batch balance is below the round minimum.
    BelowMinimumBalance,
    /// The chunk is neither a CAC nor a SOC (a rogue chunk).
    RogueChunk,
    /// The stamp is not admissible for its batch in the round snapshot (the
    /// batch is unknown or expired).
    InvalidStamp,
}

/// A frozen, round-consistent view of postage batch state for the sampler.
///
/// Every candidate in a round is judged against one snapshot, so the sample is a
/// pure function of the round inputs. The
/// [`BatchStore`](vertex_swarm_postage::BatchStore) is read once at snapshot time,
/// not per candidate, so these filters are unit-testable with no chain.
pub trait RoundBatches {
    /// The round `consensusTime`: inclusive upper bound on a stamp's big-endian
    /// timestamp, in the nanosecond unit the stamp carries.
    fn consensus_time(&self) -> u64;

    /// Whether `batch` is below the round's minimum balance and must be excluded.
    fn is_below_minimum_balance(&self, batch: &BatchId) -> bool;

    /// Whether `batch` is admissible: present in the snapshot and not expired
    /// under the round [`PostageContext`]. A stamp on a non-admissible batch is
    /// treated as invalid.
    fn is_admissible(&self, batch: &BatchId) -> bool;
}

/// The consensus candidate-feed filter for one redistribution round.
///
/// Borrows the round snapshot and applies the three exclusion gates; the single
/// place the sampler decides whether a stamped reserve entry may become a sample
/// candidate.
///
/// The valid-stamp gate is structural and does not re-run `ecrecover`: the reserve
/// signature-validates stamps on ingest and never holds an entry whose stamp
/// failed recovery, so per-candidate recovery cannot change any decision. The gate
/// only checks that the batch is still admissible and the chunk is not a rogue
/// shape.
#[derive(Clone, Copy, Debug)]
pub struct CandidateFilter<'a, R> {
    batches: &'a R,
    context: &'a PostageContext,
}

impl<'a, R: RoundBatches> CandidateFilter<'a, R> {
    /// The held [`PostageContext`] is not consulted inside [`admit`](Self::admit):
    /// the [`RoundBatches`] implementor must fold expiry into
    /// [`RoundBatches::is_admissible`] when it freezes the snapshot.
    #[must_use]
    pub const fn new(batches: &'a R, context: &'a PostageContext) -> Self {
        Self { batches, context }
    }

    #[must_use]
    pub fn consensus_time(&self) -> u64 {
        self.batches.consensus_time()
    }

    #[must_use]
    pub const fn context(&self) -> &PostageContext {
        self.context
    }

    /// Decide whether `(chunk, stamp)` is an admissible sample candidate, or the
    /// first gate's [`FilterRejection`].
    ///
    /// # Errors
    ///
    /// Returns the [`FilterRejection`] for the first gate the candidate fails.
    pub fn admit(&self, chunk: &AnyChunk, stamp: &Stamp) -> Result<(), FilterRejection> {
        if stamp.timestamp() > self.batches.consensus_time() {
            return Err(FilterRejection::FutureTimestamp);
        }

        let batch = stamp.batch();

        if self.batches.is_below_minimum_balance(&batch) {
            return Err(FilterRejection::BelowMinimumBalance);
        }

        if !(chunk.is_content() || chunk.is_single_owner()) {
            return Err(FilterRejection::RogueChunk);
        }

        if !self.batches.is_admissible(&batch) {
            return Err(FilterRejection::InvalidStamp);
        }

        Ok(())
    }

    /// Build a sample candidate if `(chunk, stamp)` passes the filters; `None`
    /// otherwise. The winning stamp travels with the item via
    /// [`SampleItem::with_stamp`] so the inclusion proof witnesses it. Use
    /// [`Self::admit`] when the rejection reason is needed.
    #[must_use]
    pub fn candidate(
        &self,
        sample: SampleAnchor,
        chunk: AnyChunk,
        stamp: Stamp,
    ) -> Option<SampleItem> {
        match self.admit(&chunk, &stamp) {
            Ok(()) => Some(SampleItem::with_stamp(sample, chunk, stamp)),
            Err(_) => None,
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test assertions over known-bounds synthetic inputs"
)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use nectar_primitives::{AnyChunk, DefaultContentChunk};
    use std::collections::HashSet;
    use vertex_swarm_postage::{Stamp, StampIndex};

    struct TestRound {
        consensus_time: u64,
        below_balance: HashSet<BatchId>,
        admissible: HashSet<BatchId>,
    }

    impl RoundBatches for TestRound {
        fn consensus_time(&self) -> u64 {
            self.consensus_time
        }
        fn is_below_minimum_balance(&self, batch: &BatchId) -> bool {
            self.below_balance.contains(batch)
        }
        fn is_admissible(&self, batch: &BatchId) -> bool {
            self.admissible.contains(batch)
        }
    }

    fn cac(payload: &[u8]) -> AnyChunk {
        DefaultContentChunk::new(payload.to_vec()).unwrap().into()
    }

    /// Signature bytes are irrelevant to these structural filters (no ecrecover),
    /// so a fixed dummy signature keeps the test focused on the gates.
    fn stamp_for(batch: BatchId, timestamp: u64) -> Stamp {
        let index = StampIndex::new(0, 0);
        let sig = alloy_primitives::Signature::test_signature();
        Stamp::with_index(batch, index, timestamp, sig)
    }

    fn batch_id(byte: u8) -> BatchId {
        B256::repeat_byte(byte)
    }

    fn anchor() -> SampleAnchor {
        SampleAnchor::new(B256::repeat_byte(0xab))
    }

    fn round(consensus_time: u64) -> TestRound {
        let good = batch_id(0x01);
        TestRound {
            consensus_time,
            below_balance: HashSet::new(),
            admissible: HashSet::from([good]),
        }
    }

    fn ctx() -> PostageContext {
        PostageContext::new(0, 0)
    }

    #[test]
    fn admits_a_well_formed_candidate() {
        let r = round(1_000);
        let c = ctx();
        let f = CandidateFilter::new(&r, &c);
        let s = stamp_for(batch_id(0x01), 500);
        assert_eq!(f.admit(&cac(b"hello"), &s), Ok(()));
        assert!(f.candidate(anchor(), cac(b"hello"), s).is_some());
    }

    #[test]
    fn rejects_future_timestamp() {
        let r = round(1_000);
        let c = ctx();
        let f = CandidateFilter::new(&r, &c);
        // Strictly greater than consensusTime is rejected; equal is allowed.
        let future = stamp_for(batch_id(0x01), 1_001);
        assert_eq!(
            f.admit(&cac(b"x"), &future),
            Err(FilterRejection::FutureTimestamp)
        );
        let boundary = stamp_for(batch_id(0x01), 1_000);
        assert_eq!(f.admit(&cac(b"x"), &boundary), Ok(()));
    }

    #[test]
    fn excludes_below_minimum_balance() {
        let mut r = round(1_000);
        let poor = batch_id(0x09);
        r.admissible.insert(poor);
        r.below_balance.insert(poor);
        let c = ctx();
        let f = CandidateFilter::new(&r, &c);
        let s = stamp_for(poor, 500);
        assert_eq!(
            f.admit(&cac(b"x"), &s),
            Err(FilterRejection::BelowMinimumBalance)
        );
        assert!(f.candidate(anchor(), cac(b"x"), s).is_none());
    }

    #[test]
    fn rejects_invalid_stamp_for_unknown_batch() {
        let r = round(1_000);
        let c = ctx();
        let f = CandidateFilter::new(&r, &c);
        let s = stamp_for(batch_id(0xee), 500);
        assert_eq!(f.admit(&cac(b"x"), &s), Err(FilterRejection::InvalidStamp));
    }

    #[test]
    fn future_timestamp_excluded_identically_to_below_balance() {
        // All three exclusion classes must produce no candidate.
        let mut r = round(1_000);
        let poor = batch_id(0x09);
        r.admissible.insert(poor);
        r.below_balance.insert(poor);
        let c = ctx();
        let f = CandidateFilter::new(&r, &c);

        let future = stamp_for(batch_id(0x01), 5_000);
        let below = stamp_for(poor, 100);
        let unknown = stamp_for(batch_id(0xee), 100);

        assert!(f.candidate(anchor(), cac(b"a"), future).is_none());
        assert!(f.candidate(anchor(), cac(b"b"), below).is_none());
        assert!(f.candidate(anchor(), cac(b"c"), unknown).is_none());
    }
}
