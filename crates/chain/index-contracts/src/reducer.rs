//! Delegated per-contract reducers: the [`Reducer`] enum the one
//! [`ContractIndexer`](crate::indexer::ContractIndexer) dispatches, and the
//! concrete [`PostageReducer`].
//!
//! # The model
//!
//! A reducer is a value, one per [`ContractId`] that earns an eager projection.
//! It is an **enum**, not a trait object, so each arm receives the concrete
//! `&TX: DbTxMut` without object-safety contortions (a `dyn` method generic over
//! `TX` is not object-safe). A contract that needs no eager structure has **no
//! reducer arm**, stays pure-lazy, and pays nothing.
//!
//! Two invariants are non-negotiable:
//!
//! - **A malformed body can never wedge the cursor.** [`Reducer::reduce`] decodes
//!   with a concrete `sol!` type; a decode miss is a **skip** (`Ok(())`), never an
//!   `Err`. The only error that escapes is a genuine storage fault, which
//!   fail-stops. `reduce` runs in the SAME transaction as the verbatim
//!   `put_event`, so the raw row, the projection, and the cursor commit
//!   atomically.
//! - **Revert is structural.** [`Reducer::rebuild`] clears the projection(s) and
//!   replays [`reduce`](Reducer::reduce) over the surviving raw rows. No reducer
//!   writes a bespoke undo. The default for a reducer-less contract is a no-op.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use vertex_storage::{DatabaseError, DbTxMut, IndexedWrite};

use crate::registry::{ContractId, abi};
use crate::store::{
    BatchByBalance, BatchKey, BatchState, BatchTable, EventKey, PostageSummary, StoredEvent,
    SummaryKey,
};

/// A delegated per-contract reducer, dispatched by [`ContractId`].
///
/// One arm per contract that maintains an eager projection. Adding an eager
/// contract is one arm plus its concrete reducer type; contracts without an arm
/// stay verbatim-only.
#[non_exhaustive]
pub enum Reducer {
    /// The postage batch-set reducer (the one eager projection today).
    Postage(PostageReducer),
}

impl Reducer {
    /// The contract this reducer maintains the projection for.
    pub fn contract(&self) -> ContractId {
        match self {
            Self::Postage(_) => ContractId::Postage,
        }
    }

    /// Decode `ev` and fold it into this reducer's typed projection(s).
    ///
    /// Runs in the verbatim `put_event` transaction. A decode miss is a skip
    /// (`Ok(())`); the only error returned is a storage fault.
    pub fn reduce<TX: DbTxMut>(
        &self,
        tx: &TX,
        key: EventKey,
        ev: &StoredEvent,
    ) -> Result<(), DatabaseError> {
        match self {
            Self::Postage(r) => r.reduce(tx, key, ev),
        }
    }

    /// Clear this reducer's projection(s) and replay [`reduce`](Self::reduce)
    /// over the surviving raw rows.
    ///
    /// The structural-revert half: after the raw tail is range-deleted, the
    /// projection is rebuilt from exactly the rows that survived, so it can never
    /// drift from the source of truth. `surviving` is this contract's remaining
    /// `EventTable` rows in canonical order.
    pub fn rebuild<TX: DbTxMut>(
        &self,
        tx: &TX,
        surviving: &[(EventKey, StoredEvent)],
    ) -> Result<(), DatabaseError> {
        match self {
            Self::Postage(r) => r.rebuild(tx, surviving),
        }
    }
}

/// The postage reducer: maintains the live [`BatchTable`] projection and its
/// self-healing value-sorted [`BatchByBalance`] index.
///
/// Faithful to the prior hardcoded carve-out: `BatchCreated` opens (or refreshes)
/// the row; `BatchTopUp` / `BatchDepthIncrease` read-modify-write the existing row
/// so the index follows the live balance; a mutation for a batch whose create is
/// outside the indexed window is dropped rather than fabricating a partial row.
///
/// The `BatchTable` + index are a pure ordering HINT (the reserve recomputes truth
/// at dequeue); they are not the source of truth, which stays the verbatim
/// `EventTable`.
///
/// It additionally maintains the single-row
/// [`PostageSummary`](crate::store::PostageSummary) running cumulative-payment
/// summary: each `PriceUpdate` settles the elapsed cost and adopts the new price
/// incrementally, so `current_total_out_payment(block)` is an O(1) extrapolation
/// from the stored triple instead of a re-fold of the entire price history. The
/// fold cadence lives on [`ChainState`] so apply-time and rebuild share one code
/// path; the summary stays block-clock-derived at READ, so `reduce` never depends
/// on wall-clock and stays replay-safe.
#[derive(Debug, Clone, Copy, Default)]
pub struct PostageReducer;

impl PostageReducer {
    fn reduce<TX: DbTxMut>(
        &self,
        tx: &TX,
        key: EventKey,
        ev: &StoredEvent,
    ) -> Result<(), DatabaseError> {
        // A PriceUpdate folds into the running summary incrementally; both paths
        // are skips on a decode miss, so a malformed body never wedges the cursor.
        if ev.topic0 == abi::PriceUpdate::SIGNATURE_HASH {
            return fold_price_update(tx, ev, key.block);
        }
        // Decode into a typed batch update; a non-batch event or a decode miss
        // yields None and is a skip (never an error).
        let Some(update) = batch_update_from_event(ev, key.block) else {
            return Ok(());
        };
        apply_batch_update(tx, update)
    }

    fn rebuild<TX: DbTxMut>(
        &self,
        tx: &TX,
        surviving: &[(EventKey, StoredEvent)],
    ) -> Result<(), DatabaseError> {
        // Drop the whole projection + index AND the running summary, then re-fold
        // the surviving postage rows through the same `reduce` paths. A batch
        // created BEFORE the revert boundary but mutated within the reverted range
        // carries a now-stale balance/index slot; keying the drop on the create
        // block would miss those, so the full rebuild from surviving rows is the
        // unambiguously-consistent choice. The summary is re-derived identically
        // because `fold_price_update` is the same cadence used at apply-time.
        tx.clear_indexed::<BatchByBalance>()?;
        tx.clear::<PostageSummary>()?;
        for (key, ev) in surviving {
            self.reduce(tx, *key, ev)?;
        }
        Ok(())
    }
}

/// Fold a decoded `PriceUpdate` at `block` into the single-row
/// [`PostageSummary`] incrementally: load the current [`ChainState`] (default if
/// absent), settle the elapsed cost and adopt the new price via
/// [`ChainState::fold_price_update`], then store it back.
///
/// A decode miss is a skip (`Ok(())`), never an error, so a malformed body cannot
/// wedge the cursor. Runs in the same tx as the verbatim `put_event`.
fn fold_price_update<TX: DbTxMut>(
    tx: &TX,
    ev: &StoredEvent,
    block: u64,
) -> Result<(), DatabaseError> {
    let Ok(decoded) = abi::PriceUpdate::decode_log_data(&ev.log_data()) else {
        return Ok(());
    };
    let mut state = tx.get::<PostageSummary>(SummaryKey)?.unwrap_or_default();
    state.fold_price_update(decoded.price, block);
    tx.put::<PostageSummary>(SummaryKey, state)
}

/// A typed update to the postage batch projection, derived from a decoded batch
/// lifecycle event. Applied by [`apply_batch_update`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchUpdate {
    /// `BatchCreated`: open (or refresh) the full row.
    Created {
        /// The on-chain batch id.
        batch_id: B256,
        /// The batch owner.
        owner: Address,
        /// The batch depth.
        depth: u8,
        /// The batch bucket depth.
        bucket_depth: u8,
        /// The created `normalisedBalance`.
        normalised_balance: U256,
        /// Whether the batch is immutable.
        immutable: bool,
        /// The creation block.
        start_block: u64,
    },
    /// `BatchTopUp`: raise the balance on the existing row.
    Balance {
        /// The on-chain batch id.
        batch_id: B256,
        /// The new `normalisedBalance`.
        normalised_balance: U256,
    },
    /// `BatchDepthIncrease`: raise depth and re-normalise the balance.
    Depth {
        /// The on-chain batch id.
        batch_id: B256,
        /// The new depth.
        new_depth: u8,
        /// The re-normalised `normalisedBalance`.
        normalised_balance: U256,
    },
}

/// Apply a [`BatchUpdate`] to the typed projection, maintaining the value-sorted
/// index self-healingly (a topup that raises the balance moves the index key).
///
/// `Created` writes the full row; `Balance` / `Depth` read-modify-write the
/// existing row so the index follows the live balance. A topup or depth-increase
/// for a batch whose create is outside the indexed window is dropped rather than
/// fabricating a partial row.
pub(crate) fn apply_batch_update<TX: DbTxMut>(
    tx: &TX,
    update: BatchUpdate,
) -> Result<(), DatabaseError> {
    match update {
        BatchUpdate::Created {
            batch_id,
            owner,
            depth,
            bucket_depth,
            normalised_balance,
            immutable,
            start_block,
        } => {
            // Preserve the original creation block if the batch is already known
            // (a duplicate create only refreshes the mutable fields).
            let start_block = tx
                .get::<BatchTable>(BatchKey(batch_id))?
                .map_or(start_block, |b| b.start_block);
            tx.put_indexed::<BatchByBalance>(
                BatchKey(batch_id),
                BatchState {
                    batch_id,
                    owner,
                    depth,
                    bucket_depth,
                    normalised_balance,
                    immutable,
                    start_block,
                },
            )
        }
        BatchUpdate::Balance {
            batch_id,
            normalised_balance,
        } => {
            let Some(mut state) = tx.get::<BatchTable>(BatchKey(batch_id))? else {
                return Ok(());
            };
            state.normalised_balance = normalised_balance;
            tx.put_indexed::<BatchByBalance>(BatchKey(batch_id), state)
        }
        BatchUpdate::Depth {
            batch_id,
            new_depth,
            normalised_balance,
        } => {
            let Some(mut state) = tx.get::<BatchTable>(BatchKey(batch_id))? else {
                return Ok(());
            };
            state.depth = new_depth;
            state.normalised_balance = normalised_balance;
            tx.put_indexed::<BatchByBalance>(BatchKey(batch_id), state)
        }
    }
}

/// Decode a stored postage event into the typed [`BatchUpdate`] that maintains
/// the value-sorted index, or `None` for a non-batch event or a decode miss.
///
/// `block` is the row's creation block, used for a `Created` update's
/// `start_block`. The shared decode the live `reduce` path and the revert rebuild
/// both use, so the projection is derived identically whether built forward or
/// rebuilt from surviving rows.
pub(crate) fn batch_update_from_event(ev: &StoredEvent, block: u64) -> Option<BatchUpdate> {
    let data = ev.log_data();
    if ev.topic0 == abi::BatchCreated::SIGNATURE_HASH {
        let e = abi::BatchCreated::decode_log_data(&data).ok()?;
        Some(BatchUpdate::Created {
            batch_id: e.batchId,
            owner: e.owner,
            depth: e.depth,
            bucket_depth: e.bucketDepth,
            normalised_balance: e.normalisedBalance,
            immutable: e.immutableFlag,
            start_block: block,
        })
    } else if ev.topic0 == abi::BatchTopUp::SIGNATURE_HASH {
        let e = abi::BatchTopUp::decode_log_data(&data).ok()?;
        Some(BatchUpdate::Balance {
            batch_id: e.batchId,
            normalised_balance: e.normalisedBalance,
        })
    } else if ev.topic0 == abi::BatchDepthIncrease::SIGNATURE_HASH {
        let e = abi::BatchDepthIncrease::decode_log_data(&data).ok()?;
        Some(BatchUpdate::Depth {
            batch_id: e.batchId,
            new_depth: e.newDepth,
            normalised_balance: e.normalisedBalance,
        })
    } else {
        None
    }
}
