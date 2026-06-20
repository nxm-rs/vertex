//! Batch-expiry driven eviction.
//!
//! A batch is expired iff `value <= total_amount` ([`Batch::is_expired`], where
//! `value` is [`Batch::value`] and `total_amount` is
//! [`PostageContext::total_amount`]). `total_amount` only ever rises, so expiry
//! is monotone. An expired batch's stamped entries are no longer paid for and
//! must be unreserved; this module turns that chain-side signal into
//! [`ReserveStore::evict_batch(batch, None, max)`](ReserveStore::evict_batch)
//! calls, where `up_to_bin = None` evicts the whole batch.
//!
//! Invariant: evict before remove. [`run`](ExpirySweep::run) only evicts batches
//! still in the [`BatchStore`], so removing a batch before its reserve entries
//! are evicted orphans those entries and inflates the reserve count. An `Expired`
//! event goes through [`on_expired_event`](ExpirySweep::on_expired_event) (evict,
//! then acknowledge removal); `run` is the backstop for value-threshold expiry
//! and crash recovery. Both paths are idempotent.
//!
//! [`expired_batches`] is the pure decision; [`ExpirySweep`] drives the I/O.

use nectar_postage::{Batch, BatchId, BatchStore, PostageContext};
use vertex_swarm_api::{ReserveStore, SwarmError, SwarmResult};

/// Max entries evicted per batch per `evict_batch` call. Bounds the single
/// eviction transaction; a larger batch drains over successive sweeps.
pub const EVICT_BATCH_MAX: u64 = 10_000;

/// The expired IDs (`value <= total_amount`, per [`Batch::is_expired`]) from
/// `(BatchId, value)` pairs, in iteration order.
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

/// The outcome of one expiry sweep.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SweepReport {
    pub evicted_batches: Vec<BatchId>,
    /// Total stamped entries removed across all evicted batches.
    pub evicted_entries: u64,
}

/// Drives batch-expiry eviction against a [`BatchStore`] and a [`ReserveStore`].
///
/// Borrows both, holds no state of its own, cheap to construct per sweep.
#[derive(Debug, Clone, Copy)]
pub struct ExpirySweep<'a, BS, R: ?Sized> {
    batches: &'a BS,
    reserve: &'a R,
}

impl<'a, BS, R> ExpirySweep<'a, BS, R>
where
    BS: BatchStore,
    // The store error funnels into SwarmError::storage (a boxed
    // Error + Send + Sync), so it must be thread-safe.
    BS::Error: Send + Sync + 'static,
    R: ReserveStore + ?Sized,
{
    #[must_use]
    pub const fn new(batches: &'a BS, reserve: &'a R) -> Self {
        Self { batches, reserve }
    }

    /// Reconciliation sweep: evict every batch value-expired at the store's
    /// current [`PostageContext`] and still present in the store. The backstop
    /// path for value-threshold expiry and crash recovery; `Expired` events go
    /// through [`on_expired_event`](Self::on_expired_event). Idempotent.
    pub fn run(&self) -> SwarmResult<SweepReport> {
        let context = self.context()?;
        let total_amount = context.total_amount();

        let ids = self.batch_ids()?;
        let mut pairs: Vec<(BatchId, u128)> = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(batch) = self.batch(&id)? {
                pairs.push((id, batch.value()));
            }
        }

        let expired = expired_batches(pairs, total_amount);

        let mut report = SweepReport::default();
        for id in expired {
            let removed = self.reserve.evict_batch(id, None, EVICT_BATCH_MAX)?;
            // An expired batch may linger in the store until its Expired event
            // is applied; only report a batch that actually shed entries, so the
            // report stays idempotent across re-detection.
            if removed > 0 {
                report.evicted_entries = report.evicted_entries.saturating_add(removed);
                report.evicted_batches.push(id);
            }
        }
        Ok(report)
    }

    /// Handle an `Expired`
    /// [`BatchEvent`](nectar_postage::BatchEvent::Expired) orphan-safely: evict
    /// the batch's reserve entries, then run `acknowledge` (the store removal)
    /// only once eviction succeeds.
    ///
    /// Evicting first guarantees no orphaned entry: once the batch leaves the
    /// store [`run`](Self::run) can no longer see it. On eviction failure
    /// `acknowledge` is not run, leaving the batch for a clean retry. Idempotent.
    /// Returns the number of stamped entries evicted.
    pub fn on_expired_event<A, E>(&self, batch: BatchId, acknowledge: A) -> SwarmResult<u64>
    where
        A: FnOnce() -> Result<(), E>,
        E: core::error::Error + Send + Sync + 'static,
    {
        let removed = self.evict_expired_batch(batch)?;
        acknowledge().map_err(SwarmError::storage)?;
        Ok(removed)
    }

    /// Evict a batch's whole footprint without touching the batch store. Most
    /// callers want [`on_expired_event`](Self::on_expired_event), which enforces
    /// evict-before-remove. Returns the number of stamped entries removed.
    pub fn evict_expired_batch(&self, batch: BatchId) -> SwarmResult<u64> {
        self.reserve.evict_batch(batch, None, EVICT_BATCH_MAX)
    }

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
        let batches = [(id(1), 100u128), (id(2), 200), (id(3), 300)];
        let expired = expired_batches(batches, 200);
        assert_eq!(expired, vec![id(1), id(2)]);
    }

    #[test]
    fn boundary_equal_value_expires() {
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
        let batches = [(id(1), 100u128), (id(2), 200), (id(3), 300)];
        let lo = expired_batches(batches, 150);
        let hi = expired_batches(batches, 250);
        assert_eq!(lo, vec![id(1)]);
        assert_eq!(hi, vec![id(1), id(2)]);
        // Never un-expires: the lower set is contained in the higher.
        for b in &lo {
            assert!(hi.contains(b));
        }
    }

    #[test]
    fn agrees_with_batch_is_expired() {
        // Cross-check the pure rule against Batch::is_expired so the two never
        // drift.
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
