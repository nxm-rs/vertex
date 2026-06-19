//! Consensus candidate-feed filters for the reserve sampler.
//!
//! Before a stamped reserve entry may enter the sample, it must pass a small
//! set of filters that every honest node applies identically. Because the
//! sample, the reserve-commitment chunk and the inclusion proofs are verified on
//! chain, these filters are **consensus load bearing**: a node that admits a
//! candidate the protocol would have excluded (or excludes one the protocol
//! would have kept) computes a different sample and loses, or is slashed in, the
//! round. They are therefore pure functions of their inputs, evaluated against a
//! single round-consistent batch snapshot.
//!
//! The three filters below are independent exclusion gates: a candidate failing
//! any one is dropped, so the final sample does not depend on the order they are
//! evaluated in (bee splits them across its iterate/assemble phases; we evaluate
//! them together). [`CandidateFilter::admit`] reports the first gate that fires
//! so callers can meter each exclusion class. The gates are:
//!
//! 1. **Future-timestamp rejection.** The stamp's timestamp, decoded as a
//!    big-endian `u64` of nanoseconds, must not exceed the round's
//!    `consensusTime`. A stamp claiming to have been issued after the round was
//!    fixed cannot have legitimately covered the chunk at sampling time, so it is
//!    excluded.
//! 2. **Below-minimum-balance exclusion.** A candidate whose batch's normalised
//!    per-chunk balance is below the round's `minimumBalance` is excluded. The
//!    set of such batches is computed once per round from the snapshot
//!    ([`RoundBatches::is_below_minimum_balance`]) so the same batches are
//!    excluded uniformly across every candidate.
//! 3. **Rogue-chunk / valid-stamp rejection.** Only content-addressed (CAC) and
//!    single-owner (SOC) chunks are admissible; any other shape is a *rogue*
//!    chunk and is excluded. The stamp must also be admissible for the batch it
//!    references in the snapshot (the batch exists and is neither expired nor
//!    below balance). The expensive per-candidate `ecrecover` is deliberately
//!    **not** repeated here: the reserve only ever admits entries whose stamps
//!    were signature-validated on ingest, so re-recovering the signer for every
//!    sampling candidate would be redundant work that changes no outcome. See
//!    the design note on [`CandidateFilter`].
//!
//! A candidate that passes all three is mapped to a [`SampleItem`] carrying its
//! exact winning stamp, ready for [`reserve_sample`](crate::reserve_sample).

use nectar_primitives::AnyChunk;
use vertex_swarm_postage::{BatchId, PostageContext, Stamp};

use crate::anchor::SampleAnchor;
use crate::sample::SampleItem;

/// Why the candidate filter excluded a stamped reserve entry.
///
/// Kept as an explicit reason (rather than a bare `bool`) so callers can meter
/// each exclusion class, exactly as the reference node's sampler statistics do
/// (future stamps, below-balance, invalid/rogue), without re-deriving which gate
/// fired.
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

/// A round-consistent view of postage batch state for the sampler.
///
/// The sampler reads batch facts for a single redistribution round through this
/// trait so that every candidate in the round sees one frozen snapshot: the
/// `consensusTime`, the `minimumBalance` and the per-batch balance facts must
/// not move underneath the candidate feed mid-round, or two candidates could be
/// judged against different state and the sample would no longer be a pure
/// function of the round inputs.
///
/// It is intentionally minimal: the full
/// [`BatchStore`](vertex_swarm_postage::BatchStore) is consulted once when the
/// snapshot is taken, not per candidate. The node provides a concrete
/// implementation (typically a frozen map of the batches in range); the
/// consensus filters here depend only on this small surface, which keeps them
/// pure and unit-testable with a synthetic snapshot and no chain.
pub trait RoundBatches {
    /// The round's `consensusTime`: the upper bound (inclusive) on a stamp's
    /// big-endian timestamp, in the same nanosecond unit the stamp carries.
    fn consensus_time(&self) -> u64;

    /// Whether `batch` is below the round's minimum balance and must be
    /// excluded. Computed once per round against the frozen snapshot so the
    /// exclusion set is identical for every candidate.
    fn is_below_minimum_balance(&self, batch: &BatchId) -> bool;

    /// Whether `batch` is admissible at all in this round: present in the
    /// snapshot and not expired under the round [`PostageContext`]. A stamp
    /// referencing a batch that fails this is treated as an invalid stamp.
    fn is_admissible(&self, batch: &BatchId) -> bool;
}

/// The consensus candidate-feed filter for one redistribution round.
///
/// Holds a borrow of the round snapshot and applies the three exclusion gates.
/// It is the single place the sampler decides whether a stamped reserve entry
/// may become a sample candidate. The gates are independent drop conditions, so
/// their evaluation order does not affect the sample; [`admit`](Self::admit)
/// returns the first that fires only so each exclusion class can be metered.
///
/// # Why no per-candidate `ecrecover`
///
/// The stamp digest and EIP-191 `ecrecover` are owned by nectar's stamp
/// validation and are run when a stamp is admitted to the reserve (on ingest).
/// By the time a stamped entry is a sampling candidate it has already been
/// signature-validated, and the reserve never holds an entry whose stamp failed
/// recovery. Re-recovering the signer for every candidate of every round would
/// therefore be pure redundant work that cannot change any admission decision.
/// The valid-stamp gate here is consequently structural: it checks that the
/// stamp's batch is still admissible (known, unexpired, not below balance) in
/// the round snapshot, and that the chunk is not a rogue shape. The cryptographic
/// guarantee is upheld upstream, not re-litigated in the hot sampling loop.
#[derive(Clone, Copy, Debug)]
pub struct CandidateFilter<'a, R> {
    batches: &'a R,
    context: &'a PostageContext,
}

impl<'a, R: RoundBatches> CandidateFilter<'a, R> {
    /// Build the filter for a round from its frozen batch snapshot and the
    /// round's [`PostageContext`].
    ///
    /// The context is the frozen round context (block height and cumulative
    /// payout) and is held so callers and the snapshot implementor can read it
    /// back via [`context`](Self::context); it is *not* consulted inside
    /// [`admit`](Self::admit). Expiry is the [`RoundBatches`] implementor's
    /// responsibility: it must fold the same context into
    /// [`RoundBatches::is_admissible`] (and the below-balance set) when it freezes
    /// the snapshot, so every candidate in the round is judged against one
    /// consistent expiry view.
    #[must_use]
    pub const fn new(batches: &'a R, context: &'a PostageContext) -> Self {
        Self { batches, context }
    }

    /// The round's `consensusTime`, threaded from the snapshot.
    #[must_use]
    pub fn consensus_time(&self) -> u64 {
        self.batches.consensus_time()
    }

    /// The round [`PostageContext`].
    #[must_use]
    pub const fn context(&self) -> &PostageContext {
        self.context
    }

    /// Decide whether `(chunk, stamp)` is an admissible sample candidate.
    ///
    /// Applies the three exclusion gates and returns the first reason the
    /// candidate is excluded, or `Ok(())` if it passes them all. The gates are
    /// independent, so the reported reason is the first that fires, not a
    /// protocol-mandated precedence. Pure: it reads only `stamp`, the chunk type,
    /// and the frozen snapshot.
    ///
    /// # Errors
    ///
    /// Returns the [`FilterRejection`] for the first gate the candidate fails.
    pub fn admit(&self, chunk: &AnyChunk, stamp: &Stamp) -> Result<(), FilterRejection> {
        // (1) Future-timestamp: a stamp timestamped after the round was fixed
        // could not have legitimately covered the chunk at sampling time.
        if stamp.timestamp() > self.batches.consensus_time() {
            return Err(FilterRejection::FutureTimestamp);
        }

        let batch = stamp.batch();

        // (2) Below-minimum-balance: uniform per-round exclusion set.
        if self.batches.is_below_minimum_balance(&batch) {
            return Err(FilterRejection::BelowMinimumBalance);
        }

        // (3a) Rogue chunk: only CAC and SOC are admissible shapes.
        if !(chunk.is_content() || chunk.is_single_owner()) {
            return Err(FilterRejection::RogueChunk);
        }

        // (3b) Valid stamp (structural; no redundant ecrecover): the batch must
        // still be admissible (known and unexpired) in the round snapshot.
        if !self.batches.is_admissible(&batch) {
            return Err(FilterRejection::InvalidStamp);
        }

        Ok(())
    }

    /// Build a sample candidate from `(chunk, stamp)` if it passes the filters.
    ///
    /// On admission the exact winning stamp travels with the item via
    /// [`SampleItem::with_stamp`], so the eventual inclusion proof witnesses that
    /// precise stamp. Returns `None` (with the reason discarded) when the
    /// candidate is excluded; use [`Self::admit`] directly when the rejection
    /// reason is needed (e.g. for metrics).
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

    /// A synthetic round snapshot: a fixed `consensusTime`, a below-balance set
    /// and an admissible set, all frozen for the round.
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

    /// A stamp for `batch` at `timestamp`. The signature bytes are irrelevant to
    /// these structural filters (no ecrecover is performed here), so a fixed
    /// dummy signature keeps the test focused on the gates.
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
        // A batch absent from the admissible set is an invalid stamp.
        let s = stamp_for(batch_id(0xee), 500);
        assert_eq!(f.admit(&cac(b"x"), &s), Err(FilterRejection::InvalidStamp));
    }

    #[test]
    fn future_timestamp_excluded_identically_to_below_balance() {
        // The three exclusion classes all produce no candidate; the feed must
        // drop them uniformly regardless of which gate fired.
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
