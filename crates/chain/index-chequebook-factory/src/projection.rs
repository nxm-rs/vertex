//! The chequebook-factory projection: a `vertex-storage` table recording the
//! set of chequebook contracts the factory has deployed.
//!
//! The projection is the indexed state the [`ChequebookFactoryIndexer`] folds
//! `SimpleSwapDeployed` logs into. Each deployed chequebook is one row, keyed by
//! its [`Address`], so membership is a single point read: a cheque arriving from
//! a chequebook is factory-deployed iff [`is_factory_deployed`] finds its row.
//!
//! Each row records the `(block, log_index)` of the `SimpleSwapDeployed` log
//! that created it. That source position makes the fold idempotent and
//! monotonic: a replayed finalized log re-applies to the same row with the same
//! value, and a log that is not strictly newer than the stored one leaves the
//! row unchanged, so reordering or replay can never regress the projection. The
//! engine delivers logs in `(block, log_index)` order over the canonical
//! finalized range, so the stored position only ever moves forward in a clean
//! run; the guard is the belt-and-braces that keeps replay a no-op.
//!
//! A chequebook is deployed exactly once, so in practice no two distinct logs
//! ever target the same address; the monotonic guard is what makes a replay of
//! that one log a no-op rather than a way of resolving genuine contention.
//!
//! [`ChequebookFactoryIndexer`]: crate::ChequebookFactoryIndexer

use alloy_primitives::Address;
use serde::{Deserialize, Serialize};
use vertex_storage::{Database, DatabaseError, DbTx, DbTxMut, Decode, Encode, Table, Tables};

vertex_storage::table!(
    pub ChequebookFactoryTable,
    "chequebook_factory_deployed",
    ChequebookKey,
    DeployedRow
);

/// The set of tables this indexer persists, for one-shot initialization.
pub struct ChequebookFactoryTables;

impl Tables for ChequebookFactoryTables {
    const NAMES: &'static [&'static str] = &[ChequebookFactoryTable::NAME];
}

/// The [`ChequebookFactoryTable`] key: a factory-deployed chequebook address.
///
/// A newtype over [`Address`] encoded as its 20 raw bytes, so the table iterates
/// in ascending address order and membership is a single point read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChequebookKey(pub Address);

impl Encode for ChequebookKey {
    type Encoded = [u8; 20];

    fn encode(self) -> Self::Encoded {
        self.0.into_array()
    }
}

impl Decode for ChequebookKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 20] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        Ok(Self(Address::from(bytes)))
    }
}

/// A deployed-chequebook row: the chain position of the `SimpleSwapDeployed` log
/// that recorded it.
///
/// The row carries no further payload; presence in the table is the fact. The
/// `source` position is the idempotency key: a fold only overwrites a row when
/// the incoming log is strictly newer (see [`DeployedRow::superseded_by`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployedRow {
    /// The `(block_number, log_index)` of the `SimpleSwapDeployed` log.
    pub source: LogPosition,
}

impl DeployedRow {
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

/// Whether `chequebook` is in the factory-deployed set.
///
/// This is the lazy read a consumer performs at its own decision point: when a
/// cheque arrives, it validates the cheque's chequebook by checking membership
/// here, rather than the indexer pushing any reaction. See
/// `CHAIN_REACTIONS_DESIGN.md`.
pub fn is_factory_deployed<DB: Database>(
    db: &DB,
    chequebook: Address,
) -> Result<bool, DatabaseError> {
    db.view(|tx| {
        Ok(tx
            .get::<ChequebookFactoryTable>(ChequebookKey(chequebook))?
            .is_some())
    })
}

/// Read the deployment record of `chequebook`, if it is factory-deployed.
pub fn deployment_of<DB: Database>(
    db: &DB,
    chequebook: Address,
) -> Result<Option<DeployedRow>, DatabaseError> {
    db.view(|tx| tx.get::<ChequebookFactoryTable>(ChequebookKey(chequebook)))
}

/// Fold a single deployment into the set: record `chequebook` at `pos`, unless an
/// existing row was set by a strictly-newer log.
///
/// This is the pure projection step. It is idempotent: a replayed or reordered
/// log that is not strictly newer than the stored source leaves the row
/// unchanged.
pub fn apply_deployment<TX: DbTxMut>(
    tx: &TX,
    chequebook: Address,
    pos: LogPosition,
) -> Result<(), DatabaseError> {
    let key = ChequebookKey(chequebook);
    if let Some(existing) = tx.get::<ChequebookFactoryTable>(key)?
        && !existing.superseded_by(pos)
    {
        return Ok(());
    }
    tx.put::<ChequebookFactoryTable>(key, DeployedRow { source: pos })
}
