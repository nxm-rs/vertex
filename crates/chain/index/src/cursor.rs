//! The persisted indexing cursor.
//!
//! A [`Cursor`] records how far an indexer has folded: the last fully-applied
//! block and that block's hash. It is persisted in a `vertex-storage` table
//! keyed by the indexer's [`name`](crate::Indexer::name), so every indexer keeps
//! its own independent checkpoint and resumes from it on restart.
//!
//! The cursor is committed in its own `vertex-storage` write transaction, last,
//! after a page's logs are applied (see [`EventEngine`](crate::EventEngine)), so
//! it advances only on a clean page and never claims a range that failed to
//! apply. On restart the engine resumes from `last_block + 1`; re-reading an
//! already-applied range is the indexer's idempotency concern, kept trivial
//! because the range is canonical and finalized.

use alloy_primitives::B256;
use serde::{Deserialize, Serialize};
use vertex_storage::{Database, DatabaseError, DbTx, DbTxMut, Decode, Encode, Table};

vertex_storage::table!(
    pub CursorTable,
    "chain_index_cursors",
    CursorKey,
    Cursor
);

/// The cursor table key: an indexer's [`name`](crate::Indexer::name).
///
/// A thin newtype over [`String`] so the indexer name satisfies the storage
/// [`Encode`]/[`Decode`] key contract. Names are encoded as their UTF-8 bytes
/// and ordered lexicographically.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CursorKey(pub String);

impl From<&str> for CursorKey {
    fn from(name: &str) -> Self {
        Self(name.to_owned())
    }
}

impl Encode for CursorKey {
    type Encoded = Vec<u8>;

    fn encode(self) -> Self::Encoded {
        self.0.into_bytes()
    }
}

impl Decode for CursorKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let name = core::str::from_utf8(value).map_err(|_| DatabaseError::Decode)?;
        Ok(Self(name.to_owned()))
    }
}

/// The set of tables this crate persists, for one-shot initialization.
pub struct CursorTables;

impl vertex_storage::Tables for CursorTables {
    const NAMES: &'static [&'static str] = &[CursorTable::NAME];
}

/// A persisted indexing checkpoint for a single indexer.
///
/// `last_block` is the highest block whose logs have been fully applied;
/// `block_hash` is that block's hash, recorded so a future head-tracking engine
/// can detect a reorg by comparing it against the canonical chain. The MVP
/// indexes only finalized blocks, so the hash is informational today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// Highest block fully applied (inclusive).
    pub last_block: u64,
    /// Hash of `last_block`.
    pub block_hash: B256,
}

impl Cursor {
    /// Load the cursor for `name`, if one has been persisted.
    pub fn load<DB: Database>(db: &DB, name: &str) -> Result<Option<Self>, DatabaseError> {
        db.view(|tx| tx.get::<CursorTable>(CursorKey::from(name)))
    }

    /// Read the cursor for `name` from an open read transaction.
    pub fn read<TX: DbTx>(tx: &TX, name: &str) -> Result<Option<Self>, DatabaseError> {
        tx.get::<CursorTable>(CursorKey::from(name))
    }

    /// Write the cursor for `name` into an open write transaction.
    ///
    /// The caller commits the transaction, so the cursor lands atomically with
    /// whatever indexed state the same transaction carries.
    pub fn write<TX: DbTxMut>(&self, tx: &TX, name: &str) -> Result<(), DatabaseError> {
        tx.put::<CursorTable>(CursorKey::from(name), *self)
    }

    /// The block backfill should resume from: one past the last applied block.
    pub fn next_block(&self) -> u64 {
        self.last_block.saturating_add(1)
    }
}
