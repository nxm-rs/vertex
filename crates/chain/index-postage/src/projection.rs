//! The `vertex-storage` projection the indexer folds PostageStamp logs into.
//!
//! Two tables, both written by a pure, idempotent fold (see
//! `CHAIN_REACTIONS_DESIGN.md`): re-applying a finalized log re-writes the same
//! row to the same value, never accumulates twice, and never triggers a side
//! effect.
//!
//! - [`BatchTable`] is the batch set, keyed by `batchId`. The batch-lifecycle
//!   events ([`BatchCreated`], [`BatchTopUp`], [`BatchDepthIncrease`]) fold into a
//!   [`BatchState`] row carrying owner, depth, bucket depth, immutability,
//!   `normalisedBalance`, and the block the batch started at. Each write is
//!   guarded by the source log position so a replayed or reordered log never
//!   rolls a row back.
//! - [`ChainStateTable`] is a single-row projection of the contract's pricing
//!   chain-state, keyed by the unit [`ChainStateKey`]. It folds the
//!   [`PriceUpdate`] cadence into the running `totalOutPayment` accumulator the
//!   contract maintains, plus the current price and the [`Paused`] flag.
//!
//! # The validity query
//!
//! The headline read helper is [`BatchState::is_valid_now`] /
//! [`ChainState::current_total_out_payment`]. A batch is valid at a block when
//! its static `normalisedBalance` is still above the rising
//! `currentTotalOutPayment(block)` line, exactly as the contract computes it:
//!
//! ```text
//! currentTotalOutPayment(block) = totalOutPayment + lastPrice * (block - lastUpdatedBlock)
//! valid_now(block)              = normalisedBalance > currentTotalOutPayment(block)
//! ```
//!
//! This crate only RECORDS and exposes that query. It fires no eviction and no
//! reaction; expiry has no event to hook (a batch dies when the rising line
//! crosses its static balance, with no transaction at the crossing), so any
//! consumer recomputes it lazily on the block clock at its own decision point.
//!
//! [`BatchCreated`]: crate::events::BatchCreated
//! [`BatchTopUp`]: crate::events::BatchTopUp
//! [`BatchDepthIncrease`]: crate::events::BatchDepthIncrease
//! [`PriceUpdate`]: crate::events::PriceUpdate
//! [`Paused`]: crate::events::Paused

use alloy_primitives::{Address, B256, U256};
use serde::{Deserialize, Serialize};
use vertex_storage::{Database, DatabaseError, DbTx, Decode, Encode, Table, Tables};

// The batch set, keyed by the on-chain `batchId`.
vertex_storage::table!(
    pub BatchTable,
    "postage_batches",
    BatchKey,
    BatchState
);

// The single-row pricing chain-state of the contract.
vertex_storage::table!(
    pub ChainStateTable,
    "postage_chain_state",
    ChainStateKey,
    ChainState
);

/// The set of tables this indexer persists, for one-shot initialization.
pub struct PostageTables;

impl Tables for PostageTables {
    const NAMES: &'static [&'static str] = &[BatchTable::NAME, ChainStateTable::NAME];
}

impl PostageTables {
    /// Create the projection tables if they do not yet exist.
    pub fn init<DB: Database>(db: &DB) -> Result<(), DatabaseError> {
        <Self as Tables>::init(db)
    }
}

/// A log's canonical position: `(block_number, log_index)`.
///
/// Ordered lexicographically, matching the order the engine delivers logs in, so
/// `>` on a [`LogPosition`] means "strictly later in the canonical stream". It is
/// the idempotency key for every fold in this crate: a write only lands when the
/// incoming log is strictly newer than the one that set the current row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LogPosition {
    /// The block the log was emitted in.
    pub block: u64,
    /// The log's index within its block.
    pub log_index: u64,
}

/// The [`BatchTable`] key: an on-chain `batchId`.
///
/// A newtype over the 32-byte batch id, encoded verbatim so the table iterates
/// in batch-id order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BatchKey(pub B256);

impl Encode for BatchKey {
    type Encoded = [u8; 32];

    fn encode(self) -> Self::Encoded {
        self.0.0
    }
}

impl Decode for BatchKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 32] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        Ok(Self(B256::from(bytes)))
    }
}

/// The folded state of a single postage batch.
///
/// Every field is written last-write-wins by its source event, guarded by
/// [`source`](BatchState::source): a fold only lands when the incoming log is
/// strictly newer, so replay and reordering are no-ops. The `start_block` is the
/// block the batch was created in and never changes after creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchState {
    /// The batch owner.
    pub owner: Address,
    /// The batch depth (the number of chunks the batch can stamp is `2^depth`).
    pub depth: u8,
    /// The batch bucket depth.
    pub bucket_depth: u8,
    /// The normalised balance: the per-chunk total-out-payment level the batch
    /// has paid up to. A batch is valid while this stays above the contract's
    /// rising `currentTotalOutPayment(block)` line.
    pub normalised_balance: U256,
    /// Whether the batch is immutable (its depth can never be increased).
    pub immutable: bool,
    /// The block the batch was created in.
    pub start_block: u64,
    /// The `(block, log_index)` of the most recent log that set this row, the
    /// idempotency and supersede key.
    pub source: LogPosition,
}

impl BatchState {
    /// Whether a log at `pos` should overwrite this row.
    ///
    /// True only when `pos` is strictly after the stored source. Equal positions
    /// (a replayed log) and earlier positions (a reordered log) are no-ops, which
    /// is what makes the fold idempotent and monotonic.
    pub fn superseded_by(&self, pos: LogPosition) -> bool {
        pos > self.source
    }

    /// Whether the batch is still valid at `block`, given the current
    /// `chain_state`.
    ///
    /// A batch is valid while its static `normalisedBalance` is strictly above
    /// the contract's rising `currentTotalOutPayment(block)` line. This is a pure
    /// read against the projection plus the block clock; it fires nothing.
    pub fn is_valid_now(&self, chain_state: &ChainState, block: u64) -> bool {
        self.normalised_balance > chain_state.current_total_out_payment(block)
    }
}

/// The [`ChainStateTable`] key: a unit key for the single chain-state row.
///
/// The pricing chain-state is one global record, not a per-key map, so the table
/// holds exactly one row under this constant key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChainStateKey;

impl Encode for ChainStateKey {
    type Encoded = [u8; 1];

    fn encode(self) -> Self::Encoded {
        [0]
    }
}

impl Decode for ChainStateKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        match value {
            [0] => Ok(Self),
            _ => Err(DatabaseError::Decode),
        }
    }
}

/// The folded pricing chain-state of the PostageStamp contract.
///
/// This reconstructs, from the [`PriceUpdate`](crate::events::PriceUpdate)
/// cadence alone, the three values the contract uses to price storage over time:
///
/// - `total_out_payment`: the per-chunk cost accumulated up to `last_updated_block`.
/// - `last_price`: the per-chunk-per-block price in force since `last_updated_block`.
/// - `last_updated_block`: the block the price last changed.
///
/// The contract advances these at each `setPrice`: if the previous price was
/// non-zero it first folds the elapsed cost into `total_out_payment`
/// (`total_out_payment += last_price * (block - last_updated_block)`), then sets
/// `last_price = price` and `last_updated_block = block`. Folding the events in
/// canonical order reproduces the same accumulator the contract holds, so
/// [`current_total_out_payment`](ChainState::current_total_out_payment) matches
/// the contract's `currentTotalOutPayment()` view at any block.
///
/// The `paused` flag and the [`source`](ChainState::source) position round out the
/// row; `source` guards the fold so a replayed or reordered log is a no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ChainState {
    /// The per-chunk total-out-payment accumulated up to `last_updated_block`.
    pub total_out_payment: U256,
    /// The per-chunk-per-block price in force since `last_updated_block`.
    pub last_price: U256,
    /// The block the price last changed (the accumulator's anchor block).
    pub last_updated_block: u64,
    /// Whether the contract is currently paused.
    pub paused: bool,
    /// The `(block, log_index)` of the most recent log that set this row.
    pub source: LogPosition,
}

impl ChainState {
    /// Whether a log at `pos` should overwrite this row.
    pub fn superseded_by(&self, pos: LogPosition) -> bool {
        pos > self.source
    }

    /// The contract's `currentTotalOutPayment(block)`: the per-chunk cost of
    /// storing one chunk since the beginning of time, at `block`.
    ///
    /// `total_out_payment + last_price * (block - last_updated_block)`. Blocks at
    /// or before `last_updated_block` (only reachable for a historical query)
    /// contribute no elapsed cost, matching the contract's saturating behaviour
    /// rather than underflowing.
    pub fn current_total_out_payment(&self, block: u64) -> U256 {
        let blocks = block.saturating_sub(self.last_updated_block);
        self.total_out_payment + self.last_price * U256::from(blocks)
    }

    /// Fold a [`PriceUpdate`](crate::events::PriceUpdate) at `block` into the
    /// accumulator, mirroring the contract's `setPrice`.
    ///
    /// Pure and self-contained: when the previous price is non-zero it first
    /// settles the elapsed cost into `total_out_payment`, then adopts the new
    /// price at `block`. The caller is responsible for the supersede guard; this
    /// only computes the next accumulator state.
    pub fn fold_price_update(&mut self, price: U256, block: u64) {
        if !self.last_price.is_zero() {
            self.total_out_payment = self.current_total_out_payment(block);
        }
        self.last_price = price;
        self.last_updated_block = block;
    }
}

/// Read a batch's state from the projection, if it has been indexed.
pub fn read_batch<DB: Database>(
    db: &DB,
    batch_id: B256,
) -> Result<Option<BatchState>, DatabaseError> {
    db.view(|tx| tx.get::<BatchTable>(BatchKey(batch_id)))
}

/// Read the contract's pricing chain-state from the projection, if any
/// `PriceUpdate` or `Paused` log has been folded yet.
pub fn read_chain_state<DB: Database>(db: &DB) -> Result<Option<ChainState>, DatabaseError> {
    db.view(|tx| tx.get::<ChainStateTable>(ChainStateKey))
}

/// Whether `batch_id` is valid at `block`, against the indexed projection.
///
/// Resolves to `Some(valid)` once both the batch and the chain-state have been
/// indexed, and `None` while either is still missing (a consumer treats a
/// not-yet-indexed batch as not-yet-known, not as expired). This is the lazy,
/// compute-at-time query the chain-reactions design calls for: it reads the
/// projection plus the live block clock and fires nothing.
pub fn is_batch_valid_now<DB: Database>(
    db: &DB,
    batch_id: B256,
    block: u64,
) -> Result<Option<bool>, DatabaseError> {
    db.view(|tx| {
        let Some(batch) = tx.get::<BatchTable>(BatchKey(batch_id))? else {
            return Ok(None);
        };
        let Some(chain_state) = tx.get::<ChainStateTable>(ChainStateKey)? else {
            return Ok(None);
        };
        Ok(Some(batch.is_valid_now(&chain_state, block)))
    })
}
