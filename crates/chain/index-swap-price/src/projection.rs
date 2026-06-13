//! The swap price oracle projection: a `vertex-storage` table holding the
//! current exchange rate and cheque value deduction.
//!
//! The projection is the indexed state the [`SwapPriceIndexer`] folds into. It
//! is a tiny, single-contract table with exactly two logical rows, keyed by
//! [`SwapPriceField`]:
//!
//! - [`SwapPriceField::ExchangeRate`] holds the latest `PriceUpdate` price.
//! - [`SwapPriceField::ChequeValueDeduction`] holds the latest
//!   `ChequeValueDeductionUpdate` deduction.
//!
//! Each row records the value together with the `(block, log_index)` of the log
//! that set it. That source position makes the fold idempotent and monotonic: a
//! replayed finalized log re-applies to the same row with the same value, and a
//! log that is not strictly newer than the stored one is ignored, so reordering
//! or replay can never roll the projection back to a stale value. The engine
//! delivers logs in `(block, log_index)` order and only over the canonical
//! finalized range, so the stored position only ever moves forward in a clean
//! run; the guard is the belt-and-braces that keeps replay a no-op.
//!
//! [`SwapPriceIndexer`]: crate::SwapPriceIndexer

use alloy_primitives::U256;
use serde::{Deserialize, Serialize};
use vertex_storage::{Database, DatabaseError, DbTx, DbTxMut, Decode, Encode, Table, Tables};

vertex_storage::table!(
    pub SwapPriceTable,
    "swap_price_oracle",
    SwapPriceField,
    SwapPriceRow
);

/// The set of tables this indexer persists, for one-shot initialization.
pub struct SwapPriceTables;

impl Tables for SwapPriceTables {
    const NAMES: &'static [&'static str] = &[SwapPriceTable::NAME];
}

/// Which projected value a row holds.
///
/// The table has one row per variant: the swap exchange rate and the cheque
/// value deduction are independent on-chain values, each updated by its own
/// event, so they project to independent rows that never contend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SwapPriceField {
    /// The swap exchange rate, set by `PriceUpdate`.
    ExchangeRate = 0,
    /// The cheque value deduction, set by `ChequeValueDeductionUpdate`.
    ChequeValueDeduction = 1,
}

impl Encode for SwapPriceField {
    type Encoded = [u8; 1];

    fn encode(self) -> Self::Encoded {
        [self as u8]
    }
}

impl Decode for SwapPriceField {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        match value {
            [0] => Ok(Self::ExchangeRate),
            [1] => Ok(Self::ChequeValueDeduction),
            _ => Err(DatabaseError::Decode),
        }
    }
}

/// A single projected value with the chain position that set it.
///
/// `source` is the `(block, log_index)` of the log that produced `value`. It is
/// the idempotency key: a fold only overwrites a row when the incoming log is
/// strictly newer (see [`SwapPriceRow::superseded_by`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwapPriceRow {
    /// The current value (an exchange rate or a deduction, per the row's key).
    pub value: U256,
    /// The `(block_number, log_index)` of the log that set `value`.
    pub source: LogPosition,
}

impl SwapPriceRow {
    /// Whether a log at `pos` should overwrite this row.
    ///
    /// True only when `pos` is strictly after the stored source. Equal positions
    /// (a replayed log) and earlier positions (a reordered log) are no-ops, which
    /// is what makes the fold idempotent and monotonic.
    pub fn superseded_by(&self, pos: LogPosition) -> bool {
        pos > self.source
    }
}

/// A log's canonical position: `(block_number, log_index)`.
///
/// Ordered lexicographically, matching the order the engine delivers logs in, so
/// `>` on a [`LogPosition`] means "strictly later in the canonical stream".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LogPosition {
    /// The block the log was emitted in.
    pub block: u64,
    /// The log's index within its block.
    pub log_index: u64,
}

/// Read the current value of `field` from the projection, if it has been set.
pub fn read_field<DB: Database>(
    db: &DB,
    field: SwapPriceField,
) -> Result<Option<SwapPriceRow>, DatabaseError> {
    db.view(|tx| tx.get::<SwapPriceTable>(field))
}

/// Fold a single update into `field`: write `value` at `pos`, unless an existing
/// row was set by a strictly-newer log.
///
/// This is the pure projection step shared by both events. It is idempotent: a
/// replayed or reordered log that is not strictly newer than the stored source
/// leaves the row unchanged.
pub fn apply_update<TX: DbTxMut>(
    tx: &TX,
    field: SwapPriceField,
    value: U256,
    pos: LogPosition,
) -> Result<(), DatabaseError> {
    if let Some(existing) = tx.get::<SwapPriceTable>(field)?
        && !existing.superseded_by(pos)
    {
        return Ok(());
    }
    tx.put::<SwapPriceTable>(field, SwapPriceRow { value, source: pos })
}
