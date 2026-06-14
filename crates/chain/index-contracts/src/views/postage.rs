//! Postage view: batch validity by lazy fold, and the eager batch-set queries
//! over the [`BatchTable`] projection + its value-sorted [`BatchByBalance`]
//! index, composed from the query combinators.
//!
//! The validity query reconstructs the contract's rising
//! `currentTotalOutPayment(block)` line from the verbatim `PriceUpdate` cadence
//! and folds the target batch's lifecycle events, then answers
//! `normalisedBalance > currentTotalOutPayment(block)` exactly as the contract
//! does. This is the lazy compute-at-time the design mandates: it reads the
//! verbatim store plus the live block clock and fires nothing.
//!
//! The eager batch queries (`batch_state`, `all_batches`, `batches_by_owner`,
//! `batches_by_balance`) read the [`PostageReducer`](crate::reducer::PostageReducer)
//! projection through the combinators. `batches_by_balance` is the value-sorted
//! eviction HINT; the reserve recomputes truth at dequeue and skip-reinserts if
//! stale.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use vertex_storage::{Database, DatabaseError};

use crate::projection::{fold_events, list_all, list_by, point_get, range_head};
use crate::registry::{ContractId, abi};
use crate::store::{BalanceKey, BatchKey, BatchState, BatchTable};

/// The contract's pricing chain-state, reconstructed from the `PriceUpdate`
/// cadence: the per-chunk total-out-payment accumulated up to
/// `last_updated_block`, the price in force since, and that block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChainState {
    /// Per-chunk total-out-payment accumulated up to `last_updated_block`.
    pub total_out_payment: U256,
    /// Per-chunk-per-block price in force since `last_updated_block`.
    pub last_price: U256,
    /// The block the price last changed.
    pub last_updated_block: u64,
}

impl ChainState {
    /// The contract's `currentTotalOutPayment(block)`:
    /// `total_out_payment + last_price * (block - last_updated_block)`.
    ///
    /// Blocks at or before `last_updated_block` contribute no elapsed cost,
    /// matching the contract's saturating behaviour rather than underflowing.
    pub fn current_total_out_payment(&self, block: u64) -> U256 {
        let blocks = block.saturating_sub(self.last_updated_block);
        self.total_out_payment + self.last_price * U256::from(blocks)
    }

    /// Fold a `PriceUpdate` at `block`, mirroring the contract's `setPrice`:
    /// when the previous price is non-zero, settle the elapsed cost into
    /// `total_out_payment` first, then adopt the new price at `block`.
    fn fold_price_update(&mut self, price: U256, block: u64) {
        if !self.last_price.is_zero() {
            self.total_out_payment = self.current_total_out_payment(block);
        }
        self.last_price = price;
        self.last_updated_block = block;
    }
}

/// Reconstruct the contract's pricing chain-state by folding the verbatim
/// `PriceUpdate` cadence in canonical order.
///
/// The read-time re-fold over the [`fold_events`] backbone. Returns `None` when
/// NO `PriceUpdate` has been indexed yet: a zero-default `ChainState` makes
/// `current_total_out_payment` return 0, which would make every positive-balance
/// batch validate forever. Distinguishing "no price seen" from "price 0" lets
/// [`is_batch_valid_now`] answer `None` (not-yet-known) rather than a misleading
/// `Some(true)` before the price cadence is known.
//
// TODO(#326): replace this read-time re-fold with an O(1) read of the incremental
// `PostageSummary` projection the `PostageReducer` will maintain on each
// `PriceUpdate`. The signature stays; only the body becomes a `scalar` read.
pub fn chain_state<DB: Database>(db: &DB) -> Result<Option<ChainState>, DatabaseError> {
    fold_events(
        db,
        ContractId::Postage,
        None,
        |state: &mut Option<ChainState>, key, ev| {
            if ev.topic0 != abi::PriceUpdate::SIGNATURE_HASH {
                return;
            }
            if let Ok(decoded) = abi::PriceUpdate::decode_log_data(&ev.log_data()) {
                state
                    .get_or_insert_with(ChainState::default)
                    .fold_price_update(decoded.price, key.block);
            }
        },
    )
}

/// The contract's `currentTotalOutPayment(block)`, the rising line a batch's
/// `normalisedBalance` must stay above to remain valid, or `None` before any
/// price cadence is indexed (the fail-safe).
//
// TODO(#326): becomes an O(1) extrapolation from the `PostageSummary` row.
pub fn current_total_out_payment<DB: Database>(
    db: &DB,
    block: u64,
) -> Result<Option<U256>, DatabaseError> {
    Ok(chain_state(db)?.map(|cs| cs.current_total_out_payment(block)))
}

/// The folded current state of one batch, from its verbatim lifecycle events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Batch {
    /// The batch owner.
    pub owner: Address,
    /// The batch depth.
    pub depth: u8,
    /// The batch bucket depth.
    pub bucket_depth: u8,
    /// The current `normalisedBalance`.
    pub normalised_balance: U256,
    /// Whether the batch is immutable.
    pub immutable: bool,
    /// The block the batch was created in.
    pub start_block: u64,
}

/// Fold one batch's lifecycle (`BatchCreated` / `BatchTopUp` /
/// `BatchDepthIncrease`) from the verbatim rows, in canonical order, over the
/// [`fold_events`] backbone.
///
/// Last-write-wins per field is implicit in the position order: later events
/// overwrite earlier ones. Returns `None` if the batch was never created in the
/// indexed window.
pub fn batch<DB: Database>(db: &DB, batch_id: B256) -> Result<Option<Batch>, DatabaseError> {
    fold_events(
        db,
        ContractId::Postage,
        None,
        |current: &mut Option<Batch>, key, ev| {
            let data = ev.log_data();
            if ev.topic0 == abi::BatchCreated::SIGNATURE_HASH
                && let Ok(d) = abi::BatchCreated::decode_log_data(&data)
                && d.batchId == batch_id
            {
                let start_block = current.map_or(key.block, |b| b.start_block);
                *current = Some(Batch {
                    owner: d.owner,
                    depth: d.depth,
                    bucket_depth: d.bucketDepth,
                    normalised_balance: d.normalisedBalance,
                    immutable: d.immutableFlag,
                    start_block,
                });
            } else if ev.topic0 == abi::BatchTopUp::SIGNATURE_HASH
                && let Ok(d) = abi::BatchTopUp::decode_log_data(&data)
                && d.batchId == batch_id
                && let Some(b) = current.as_mut()
            {
                b.normalised_balance = d.normalisedBalance;
            } else if ev.topic0 == abi::BatchDepthIncrease::SIGNATURE_HASH
                && let Ok(d) = abi::BatchDepthIncrease::decode_log_data(&data)
                && d.batchId == batch_id
                && let Some(b) = current.as_mut()
            {
                b.depth = d.newDepth;
                b.normalised_balance = d.normalisedBalance;
            }
        },
    )
}

/// Whether `batch_id` is valid at `block`, against the verbatim store.
///
/// `Some(valid)` once both the batch AND at least one `PriceUpdate` have been
/// indexed; `None` while either is not yet known. Returning `None` (rather than
/// `Some(true)`) before any price is folded is the fail-safe behaviour: without
/// it, an absent price cadence makes `currentTotalOutPayment` zero and every
/// positive-balance batch would validate forever, which would mask a
/// wrong-address / wrong-ABI failure as "always valid". A consumer treats `None`
/// as not-yet-known, not expired. The lazy compute-at-time query: it reads the
/// store plus the live block clock and fires nothing.
pub fn is_batch_valid_now<DB: Database>(
    db: &DB,
    batch_id: B256,
    block: u64,
) -> Result<Option<bool>, DatabaseError> {
    let Some(b) = batch(db, batch_id)? else {
        return Ok(None);
    };
    let Some(cs) = chain_state(db)? else {
        // No price cadence indexed yet: fail safe to not-yet-known.
        return Ok(None);
    };
    Ok(Some(
        b.normalised_balance > cs.current_total_out_payment(block),
    ))
}

/// The materialized batch projection row for `batch_id`, if any (`point_get`).
///
/// Reads the typed [`BatchTable`] projection (the index's backing row), not the
/// verbatim fold. This is the cheap point-read the eviction queue uses; for the
/// authoritative balance history use [`batch`].
pub fn batch_state<DB: Database>(
    db: &DB,
    batch_id: B256,
) -> Result<Option<BatchState>, DatabaseError> {
    point_get::<BatchTable, _>(db, BatchKey(batch_id))
}

/// Every batch currently in the projection (`list_all`).
pub fn all_batches<DB: Database>(db: &DB) -> Result<Vec<BatchState>, DatabaseError> {
    Ok(list_all::<BatchTable, _>(db)?
        .into_iter()
        .map(|(_, v)| v)
        .collect())
}

/// Every batch owned by `owner` (`list_by` on the projection's `owner` field).
///
/// `BatchState` already carries `owner`, so this is a filtered scan of the live
/// projection with no second table.
pub fn batches_by_owner<DB: Database>(
    db: &DB,
    owner: Address,
) -> Result<Vec<BatchState>, DatabaseError> {
    Ok(list_by::<BatchTable, _, _>(db, |b| b.owner == owner)?
        .into_iter()
        .map(|(_, v)| v)
        .collect())
}

/// Batch ids ordered by ascending `normalisedBalance` within `[lo ..= hi]`,
/// bounded to `limit` (`range_head` over [`BatchByBalance`]).
///
/// The #317 bounded read: it scans only the index's `[lo ..= hi]` window via
/// `DbTx::range` and `take(limit)`s, instead of materializing the whole index and
/// sorting in memory. A pure ordering HINT; the reserve recomputes truth at
/// dequeue. Pass [`BalanceKey::min`]/[`BalanceKey::max`] for an unbounded
/// balance range.
pub fn batches_by_balance<DB: Database>(
    db: &DB,
    lo: BalanceKey,
    hi: BalanceKey,
    limit: usize,
) -> Result<Vec<B256>, DatabaseError> {
    Ok(range_head::<BatchTable, _>(db, lo, hi, limit)?
        .into_iter()
        .map(|pk| pk.0)
        .collect())
}

/// The value-sorted eviction hint: batch ids ordered by ascending
/// `normalisedBalance` (soonest-to-expire first), up to `limit`.
///
/// A pure ordering HINT over the self-healing [`BatchByBalance`] index. The
/// reserve dequeues this head, recomputes true value at dequeue against the live
/// block clock with [`is_batch_valid_now`], and skip-and-reinserts if stale.
/// This function makes no decision and evicts nothing.
///
/// This is the bounded #317 read: it scans only the head `limit` entries of the
/// index via [`batches_by_balance`] (which uses `DbTx::range`), instead of a full
/// `entries()` + in-memory sort.
///
/// Note: the `BatchByBalance` index holds one entry per *historically created*
/// batch, NOT per *live* batch — it grows with chain history because no event
/// signals expiry, so nothing prunes it here. The bounded read above makes this
/// query cheap regardless; the unbounded-growth fix (a conservative `on_block`
/// prune) is tracked separately as the second half of #317 and is NOT in this
/// change.
pub fn eviction_candidates<DB: Database>(
    db: &DB,
    limit: usize,
) -> Result<Vec<B256>, DatabaseError> {
    batches_by_balance(db, BalanceKey::min(), BalanceKey::max(), limit)
}
