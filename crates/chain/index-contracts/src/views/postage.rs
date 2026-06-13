//! Postage view: batch validity by lazy fold, and the value-sorted eviction
//! hint over the one materialized index.
//!
//! The validity query reconstructs the contract's rising
//! `currentTotalOutPayment(block)` line from the verbatim `PriceUpdate` cadence
//! and folds the target batch's lifecycle events, then answers
//! `normalisedBalance > currentTotalOutPayment(block)` exactly as the contract
//! does. This is the lazy compute-at-time the design mandates: it reads the
//! verbatim store plus the live block clock and fires nothing.
//!
//! The eviction surface reads the [`BatchByBalance`] index head (the
//! soonest-to-expire batch by balance) as an ordering HINT; the reserve
//! recomputes truth at dequeue and skip-and-reinserts if stale.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use vertex_storage::{Database, DatabaseError, DbTx};

use crate::registry::{ContractId, abi};
use crate::store::{BatchByBalance, BatchKey, BatchState, BatchTable, events_of};

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
/// This is the read-time replacement for the branch's `ChainStateTable`: the
/// position-ordered raw rows are folded on demand, decoding each `PriceUpdate`.
///
/// Returns `None` when NO `PriceUpdate` has been indexed yet. This is the
/// fail-safe the branch had: a zero-default `ChainState` makes
/// `current_total_out_payment` return 0, which would make every positive-balance
/// batch validate forever. Distinguishing "no price seen" from "price 0" lets
/// [`is_batch_valid_now`] answer `None` (not-yet-known) rather than a misleading
/// `Some(true)` before the price cadence is known.
pub fn chain_state<DB: Database>(db: &DB) -> Result<Option<ChainState>, DatabaseError> {
    let mut state: Option<ChainState> = None;
    for (key, ev) in events_of(db, ContractId::Postage)? {
        if ev.topic0 != abi::PriceUpdate::SIGNATURE_HASH {
            continue;
        }
        if let Ok(decoded) = abi::PriceUpdate::decode_log_data(&ev.log_data()) {
            state
                .get_or_insert_with(ChainState::default)
                .fold_price_update(decoded.price, key.block);
        }
    }
    Ok(state)
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
/// `BatchDepthIncrease`) from the verbatim rows, in canonical order.
///
/// Last-write-wins per field is implicit in the position order: later events
/// overwrite earlier ones. Returns `None` if the batch was never created in the
/// indexed window.
pub fn batch<DB: Database>(db: &DB, batch_id: B256) -> Result<Option<Batch>, DatabaseError> {
    let mut current: Option<Batch> = None;
    for (key, ev) in events_of(db, ContractId::Postage)? {
        let data = ev.log_data();
        if ev.topic0 == abi::BatchCreated::SIGNATURE_HASH
            && let Ok(d) = abi::BatchCreated::decode_log_data(&data)
            && d.batchId == batch_id
        {
            let start_block = current.map_or(key.block, |b| b.start_block);
            current = Some(Batch {
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
    }
    Ok(current)
}

/// Whether `batch_id` is valid at `block`, against the verbatim store.
///
/// `Some(valid)` once both the batch AND at least one `PriceUpdate` have been
/// indexed; `None` while either is not yet known. Returning `None` (rather than
/// `Some(true)`) before any price is folded is the fail-safe behaviour the branch
/// had: without it, an absent price cadence makes `currentTotalOutPayment` zero
/// and every positive-balance batch would validate forever, which would mask a
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

/// The materialized batch projection row for `batch_id`, if any.
///
/// Reads the typed [`BatchTable`] (the index's backing row), not the verbatim
/// fold. This is the cheap point-read the eviction queue uses; for the
/// authoritative balance history use [`batch`].
pub fn batch_state<DB: Database>(
    db: &DB,
    batch_id: B256,
) -> Result<Option<BatchState>, DatabaseError> {
    db.view(|tx| tx.get::<BatchTable>(BatchKey(batch_id)))
}

/// The value-sorted eviction hint: batch ids ordered by ascending
/// `normalisedBalance` (soonest-to-expire first), up to `limit`.
///
/// A pure ordering HINT over the self-healing [`BatchByBalance`] index. The
/// reserve dequeues this head, recomputes true value at dequeue against the live
/// block clock with [`is_batch_valid_now`], and skip-and-reinserts if stale.
/// This function makes no decision and evicts nothing.
///
/// The index is keyed by `(balance, batch_id)`, so iterating it ascending yields
/// the soonest-to-expire batches first with a deterministic tie-break. This reads
/// the whole index then `take(limit)`s the head; unlike `EventTable`, the
/// `BatchByBalance` index holds at most one entry per LIVE batch (bounded by the
/// active batch count, not by chain history), so the read is small. The `batchId`
/// in the key makes it unique per batch, so no equal-balance batch is ever
/// dropped from the hint (see `store.rs::BalanceKey`).
pub fn eviction_candidates<DB: Database>(
    db: &DB,
    limit: usize,
) -> Result<Vec<B256>, DatabaseError> {
    db.view(|tx| {
        let mut entries = tx.entries::<BatchByBalance>()?;
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));
        Ok(entries
            .into_iter()
            .take(limit)
            .map(|(_, pk)| pk.0)
            .collect())
    })
}
