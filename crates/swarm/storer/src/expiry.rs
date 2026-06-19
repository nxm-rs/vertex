//! Batch-expiry driven eviction.
//!
//! A postage batch expires when the chain's cumulative payout per chunk catches
//! up with the batch's per-chunk balance. Concretely, with
//! [`PostageContext::total_amount`] the cumulative payout and [`Batch::value`]
//! the batch's per-chunk balance, the batch is expired iff
//! `value <= total_amount` ([`Batch::is_expired`]). As the chain advances,
//! `total_amount` only ever increases, so expiry is monotone: once a batch is
//! expired it stays expired.
//!
//! When a batch expires, every stamped entry the reserve holds under that batch
//! is no longer paid for and must be unreserved. This module is the seam that
//! turns the chain-side expiry signal into reserve evictions: it reads the live
//! [`PostageContext`] and the batch set from the [`BatchStore`], decides which
//! batches have become expired, and drives
//! [`ReserveStore::evict_batch(batch, None, max)`](ReserveStore::evict_batch)
//! over the reserve's `BatchGroup` index for each. `up_to_bin = None` evicts the
//! *whole* batch, which is correct for expiry (an expired batch is paid for at
//! no bin), as opposed to the radius-growth case which sheds only a batch's
//! shallow, out-of-responsibility bins.
//!
//! # Where the signal comes from
//!
//! The chain indexer is out of scope (it is stubbed; see the postage crate). It
//! reduces contract logs into [`BatchEvent`](nectar_postage::BatchEvent)s and
//! advances the [`PostageContext`] (its `total_amount` rises as
//! redistribution rounds pay out, and an `Expired` event removes the batch from
//! the store).
//!
//! There are two paths into this module, and they have different guarantees:
//!
//! - **The event path (authoritative).** When the indexer delivers a
//!   [`BatchEvent::Expired`](nectar_postage::BatchEvent::Expired) the reserve
//!   must shed that batch's entries *before* the batch leaves the
//!   [`BatchStore`]. [`ExpirySweep::on_expired_event`] enforces exactly this
//!   ordering: it evicts the batch's reserve entries first and only then runs
//!   the supplied acknowledgement (the store removal). This is the only
//!   ordering that cannot orphan entries.
//! - **The reconciliation path (backstop).** [`ExpirySweep::run`] samples the
//!   *resulting state* (the batch store and its context) and evicts any batch
//!   that is value-expired (`value <= total_amount`) but is still present in the
//!   store. It is a backstop for value-threshold expiry the chain crossed
//!   without (or before) an explicit `Expired` event, and for crash recovery. It
//!   is idempotent and replay-safe: a second run at the same context evicts
//!   nothing, because the batch's entries are already gone.
//!
//! # Eviction must precede removal (orphan-avoidance contract)
//!
//! [`run`](ExpirySweep::run) can only evict batches it can still *see* in the
//! [`BatchStore`]: it infers the expired set from
//! [`batch_ids`](BatchStore::batch_ids). If a batch is removed from the store
//! before its reserve entries are evicted, those entries are orphaned - they
//! never appear in any future `batch_ids` snapshot, so no later `run` will ever
//! shed them. Orphaned entries inflate the reserve count, which drives the
//! storage radius and therefore the consensus-committed depth, so this is a
//! correctness hazard, not merely a leak.
//!
//! The contract is therefore: **evict before remove.** The live ingest wiring
//! (#391/#392) must route an `Expired` event through
//! [`on_expired_event`](ExpirySweep::on_expired_event) (evict, then acknowledge
//! removal) rather than removing the batch from the store directly and relying on
//! the next `run` to catch up. `run` remains correct for value-threshold expiry
//! (where the batch is still present) and as a crash-recovery reconciliation, but
//! it is not a substitute for the ordered event path.
//!
//! # Purity seam
//!
//! The decision - *which* of a set of batches are expired at a given
//! `total_amount` - is the pure [`expired_batches`] function, tested in
//! isolation. The driver [`ExpirySweep`] does the I/O (read the store, call
//! `evict_batch`) around it.

use nectar_postage::{Batch, BatchId, BatchStore, PostageContext};
use vertex_swarm_api::{ReserveStore, SwarmError, SwarmResult};

/// The maximum number of entries a single [`ExpirySweep::run`] will evict per
/// batch in one call.
///
/// Eviction is one atomic transaction per `evict_batch` call; this bounds the
/// transaction size so a pathologically large batch does not stall the control
/// loop. The sweep re-runs on the next context advance, so a batch with more
/// than `EVICT_BATCH_MAX` entries drains over successive sweeps rather than in
/// one giant transaction.
pub const EVICT_BATCH_MAX: u64 = 10_000;

/// Decide which of the given batches are expired at `total_amount`.
///
/// Pure and total. `batches` yields `(BatchId, value)` pairs where `value` is
/// the batch's per-chunk balance ([`Batch::value`]); a batch is expired iff
/// `value <= total_amount`, matching [`Batch::is_expired`]. Returns the expired
/// IDs in iteration order. Separated from the I/O so the rule is unit-tested
/// without a store or a reserve.
pub fn expired_batches<I>(batches: I, total_amount: u128) -> Vec<BatchId>
where
    I: IntoIterator<Item = (BatchId, u128)>,
{
    batches
        .into_iter()
        .filter(|&(_, value)| value <= total_amount)
        .map(|(id, _)| id)
        .collect()
}

/// The result of one expiry sweep: which batches were evicted and how many
/// stamped entries went with them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SweepReport {
    /// The batches found expired and evicted this sweep.
    pub evicted_batches: Vec<BatchId>,
    /// The total stamped entries removed across all evicted batches.
    pub evicted_entries: u64,
}

/// Drives batch-expiry eviction against a [`BatchStore`] and a [`ReserveStore`].
///
/// Borrows both for the duration of a sweep; holds no state of its own, so it is
/// cheap to construct per sweep. See the [module docs](self) for the signal and
/// the purity seam.
#[derive(Debug, Clone, Copy)]
pub struct ExpirySweep<'a, BS, R: ?Sized> {
    batches: &'a BS,
    reserve: &'a R,
}

impl<'a, BS, R> ExpirySweep<'a, BS, R>
where
    BS: BatchStore,
    // The synchronous [`BatchStore`] returns its own typed error; funnelling it
    // into [`SwarmError::storage`] (whose source is a boxed
    // `Error + Send + Sync`) requires the store's error be thread-safe. This is
    // the same bound `DbReserve` carries on its own batch-store reads.
    BS::Error: Send + Sync + 'static,
    R: ReserveStore + ?Sized,
{
    /// Construct a sweep over the given batch store and reserve.
    #[must_use]
    pub const fn new(batches: &'a BS, reserve: &'a R) -> Self {
        Self { batches, reserve }
    }

    /// Run one reconciliation sweep: evict every batch that is value-expired at
    /// the store's current [`PostageContext`] *and still present in the store*.
    ///
    /// Reads the live context and batch set, applies [`expired_batches`] to find
    /// the expired IDs, then calls
    /// [`evict_batch(id, None, EVICT_BATCH_MAX)`](ReserveStore::evict_batch) for
    /// each. Idempotent: a second run at the same context evicts nothing and
    /// reports no batches, since an expired batch's entries are already gone (a
    /// batch is reported only when the call actually removed entries).
    ///
    /// This is the **backstop** path (see the [module docs](self)): it can only
    /// shed batches it can still see in the [`BatchStore`]. A batch removed from
    /// the store before its entries were evicted is invisible here and would be
    /// orphaned, so the authoritative path for an `Expired` event is
    /// [`on_expired_event`](Self::on_expired_event), which evicts before the
    /// store removal. Use `run` for value-threshold expiry (the batch is still
    /// present) and crash-recovery reconciliation.
    ///
    /// The [`BatchStore`] surface is synchronous (its production redb backing is
    /// synchronous and its in-memory test backing is too), so a synchronous
    /// control loop can call this directly without any executor bridge.
    pub fn run(&self) -> SwarmResult<SweepReport> {
        let context = self.context()?;
        let total_amount = context.total_amount();

        // Resolve the (id, value) pairs the rule consumes from the live store.
        let ids = self.batch_ids()?;
        let mut pairs: Vec<(BatchId, u128)> = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(batch) = self.batch(&id)? {
                // Reuse Batch::is_expired's exact predicate via the value.
                pairs.push((id, batch.value()));
            }
        }

        let expired = expired_batches(pairs, total_amount);

        let mut report = SweepReport::default();
        for id in expired {
            let removed = self.reserve.evict_batch(id, None, EVICT_BATCH_MAX)?;
            // Only record a batch as swept when it actually had entries to shed.
            // An expired batch may linger in the batch store until the indexer
            // applies its `Expired` event, so a later sweep re-detects it as
            // expired but finds nothing to evict; reporting it again would be
            // misleading and would break idempotency of the report.
            if removed > 0 {
                report.evicted_entries = report.evicted_entries.saturating_add(removed);
                report.evicted_batches.push(id);
            }
        }
        Ok(report)
    }

    /// Handle an `Expired`
    /// [`BatchEvent`](nectar_postage::BatchEvent::Expired) in the orphan-safe
    /// order: evict the batch's reserve entries first, then run `acknowledge`
    /// (the batch-store removal) only once eviction has succeeded.
    ///
    /// This is the **authoritative** event path (see the [module docs](self)).
    /// Evicting before removal is what guarantees no entry is orphaned: once the
    /// batch leaves the [`BatchStore`] the reconciliation [`run`](Self::run) can
    /// no longer see it, so removal must never happen first. If eviction fails
    /// the error short-circuits and `acknowledge` is *not* run, leaving the batch
    /// in the store so the next attempt (event redelivery or a `run` backstop)
    /// retries cleanly rather than stranding entries.
    ///
    /// `acknowledge` is the caller's store-removal step (typically the indexer's
    /// [`BatchEventHandler`](nectar_postage::BatchEventHandler) applying the
    /// `Expired` event). Returns the number of stamped entries evicted.
    ///
    /// Idempotent: replaying the same `Expired` event evicts nothing the second
    /// time (the entries are gone) and still acknowledges, so a redelivered event
    /// is safe.
    pub fn on_expired_event<A, E>(&self, batch: BatchId, acknowledge: A) -> SwarmResult<u64>
    where
        A: FnOnce() -> Result<(), E>,
        E: core::error::Error + Send + Sync + 'static,
    {
        // Evict first: while the batch is still in the store its entries are
        // reachable by `run` as a fallback, so a failure here strands nothing.
        let removed = self.evict_expired_batch(batch)?;
        // Only now that the reserve no longer holds the batch is it safe to drop
        // it from the store. The acknowledgement is the caller's store removal,
        // not a batch-store read; its error is funnelled into
        // `SwarmError::storage` directly.
        acknowledge().map_err(SwarmError::storage)?;
        Ok(removed)
    }

    /// Evict a single batch's whole footprint without touching the batch store,
    /// the eviction primitive [`on_expired_event`](Self::on_expired_event) is
    /// built on.
    ///
    /// Exposed for callers that own the store-removal ordering themselves; most
    /// callers want [`on_expired_event`](Self::on_expired_event), which enforces
    /// the evict-before-remove contract. Returns the number of stamped entries
    /// removed.
    pub fn evict_expired_batch(&self, batch: BatchId) -> SwarmResult<u64> {
        self.reserve.evict_batch(batch, None, EVICT_BATCH_MAX)
    }

    /// The live [`PostageContext`], read straight from the synchronous
    /// [`BatchStore`].
    ///
    /// The reserve (PR-D) and this sweep both read the [`BatchStore`] from
    /// synchronous control paths. The store surface is itself synchronous, so the
    /// read is a plain call whose typed error maps straight into
    /// [`SwarmError::storage`]; there is no executor bridge.
    fn context(&self) -> SwarmResult<PostageContext> {
        self.batches.context().map_err(SwarmError::storage)
    }

    fn batch_ids(&self) -> SwarmResult<Vec<BatchId>> {
        self.batches.batch_ids().map_err(SwarmError::storage)
    }

    fn batch(&self, id: &BatchId) -> SwarmResult<Option<Batch>> {
        self.batches.get(id).map_err(SwarmError::storage)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test assertions over known-bounds inputs"
)]
mod tests {
    use super::*;

    fn id(byte: u8) -> BatchId {
        BatchId::from([byte; 32])
    }

    #[test]
    fn expired_iff_value_at_or_below_total_amount() {
        // value <= total_amount expires; value > total_amount survives.
        let batches = [(id(1), 100u128), (id(2), 200), (id(3), 300)];
        // total_amount = 200: batches with value 100 and 200 expire, 300 lives.
        let expired = expired_batches(batches, 200);
        assert_eq!(expired, vec![id(1), id(2)]);
    }

    #[test]
    fn boundary_equal_value_expires() {
        // The predicate is `value <= total_amount`, so equality expires
        // (matches Batch::is_expired), it does not survive.
        assert_eq!(expired_batches([(id(1), 500u128)], 500), vec![id(1)]);
        assert_eq!(
            expired_batches([(id(1), 501u128)], 500),
            Vec::<BatchId>::new()
        );
    }

    #[test]
    fn nothing_expires_below_all_values() {
        let batches = [(id(1), 100u128), (id(2), 200)];
        assert!(expired_batches(batches, 99).is_empty());
    }

    #[test]
    fn monotone_in_total_amount() {
        // As total_amount rises the expired set only grows (chain payout is
        // monotone), so expiry never un-expires a batch.
        let batches = [(id(1), 100u128), (id(2), 200), (id(3), 300)];
        let lo = expired_batches(batches, 150);
        let hi = expired_batches(batches, 250);
        assert_eq!(lo, vec![id(1)]);
        assert_eq!(hi, vec![id(1), id(2)]);
        // Every batch expired at the lower threshold is still expired at the
        // higher one.
        for b in &lo {
            assert!(hi.contains(b));
        }
    }

    #[test]
    fn agrees_with_batch_is_expired() {
        // Cross-check the pure rule against nectar's own predicate so the two
        // never drift. Construct a batch and compare for a sweep of amounts.
        let batch = Batch::new(
            id(7),
            /* value */ 1_000u128,
            /* start */ 0,
            /* owner */ Default::default(),
            /* depth */ 20,
            /* bucket_depth */ 16,
            /* immutable */ false,
        );
        for total in [0u128, 999, 1_000, 1_001, 5_000] {
            let rule = expired_batches([(batch.id(), batch.value())], total);
            if batch.is_expired(total) {
                assert_eq!(rule, vec![batch.id()], "total={total}");
            } else {
                assert!(rule.is_empty(), "total={total}");
            }
        }
    }
}
