//! Postage view: batch validity by lazy fold, and the eager batch-set queries
//! over the [`BatchTable`] projection + its value-sorted [`BatchByBalance`]
//! index, composed from the query combinators.
//!
//! The validity query reads the incrementally-maintained
//! [`PostageSummary`](crate::store::PostageSummary) running cumulative-payment
//! row, extrapolates the contract's rising `currentTotalOutPayment(block)` line
//! from its stored triple in O(1), folds the target batch's lifecycle events, and
//! answers `normalisedBalance > currentTotalOutPayment(block)` exactly as the
//! contract does. The summary is block-clock-derived AT READ (the stored triple
//! extrapolates to any block), so it reads the projection plus the live block
//! clock and fires nothing.
//!
//! The eager batch queries (`batch_state`, `all_batches`, `batches_by_owner`,
//! `batches_by_balance`) read the [`PostageReducer`](crate::reducer::PostageReducer)
//! projection through the combinators. `batches_by_balance` is the value-sorted
//! eviction HINT; the reserve recomputes truth at dequeue and skip-reinserts if
//! stale.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use vertex_storage::{Database, DatabaseError};

use crate::projection::{fold_events, list_all, list_by, point_get, range_head, scalar};
use crate::registry::{ContractId, abi};
use crate::store::{
    BalanceKey, BatchKey, BatchState, BatchTable, ChainState, PostageSummary, SummaryKey,
};

/// The contract's pricing chain-state, read O(1) from the incremental
/// [`PostageSummary`] projection.
///
/// Returns `None` when NO `PriceUpdate` has been indexed yet (the summary row is
/// absent): a zero-default `ChainState` would make `current_total_out_payment`
/// return 0 and validate every positive-balance batch forever. Distinguishing
/// "no price seen" from "price 0" lets [`is_batch_valid_now`] answer `None`
/// (not-yet-known) rather than a misleading `Some(true)` before the price cadence
/// is known. This is the fail-safe.
pub fn chain_state<DB: Database>(db: &DB) -> Result<Option<ChainState>, DatabaseError> {
    scalar::<PostageSummary, _>(db, SummaryKey)
}

/// The contract's `currentTotalOutPayment(block)`, the rising line a batch's
/// `normalisedBalance` must stay above to remain valid, or `None` before any
/// price cadence is indexed (the fail-safe).
///
/// An O(1) extrapolation of the stored [`PostageSummary`] triple to `block`.
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
