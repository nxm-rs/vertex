//! The generic authenticated event store: ONE table holding every watched
//! contract's events verbatim, plus the one materialized secondary projection
//! (postage value-sorted eviction).
//!
//! # Verbatim, position-keyed
//!
//! [`EventTable`] is keyed by [`EventKey`] `= (contract, block, log_index)` and
//! holds [`StoredEvent`] `= (address, topic0, topics, data)` exactly as the log
//! carried it. The key *is* the position, so:
//!
//! - **Idempotent upsert is structural.** A replayed finalized log overwrites
//!   its own slot with identical bytes; there is no supersede guard.
//! - **redb's btree returns rows in `(contract, block, log_index)` order**, so a
//!   per-contract view fold reads the contract's stream in canonical order with
//!   no re-sort.
//! - **Revert is a per-contract range-delete** over `[from_block ..= MAX]`.
//!
//! Decoding never happens here; the [`views`](crate::views) decode on read with
//! the concrete nectar `sol!` types.
//!
//! # The one materialized projection
//!
//! [`BatchTable`] + the [`BatchByBalance`] secondary index are the only typed
//! eager structure, justified solely by the value-sorted-scan the reserve (#75)
//! needs. It is a pure ordering HINT carrying no decision: the index orders
//! batches by `normalisedBalance`, and the reserve recomputes true value at
//! dequeue against the live block clock (see `CHAIN_REACTIONS_DESIGN.md`). It is
//! maintained by the [`PostageReducer`](crate::reducer::PostageReducer) via
//! [`vertex_storage::IndexedWrite`] and rebuilt from surviving rows on revert.

use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_rpc_types_eth::Log;
use serde::{Deserialize, Serialize};
use vertex_storage::{Database, DatabaseError, DbTx, DbTxMut, Decode, Encode, Table, Tables};

use crate::registry::ContractId;

/// The hard cap on a stored event's `data` length, in bytes.
///
/// Every watched event has a small, fixed-size non-indexed payload, so a real
/// event never approaches this. The cap bounds both disk use and view-decode
/// allocation against an unexpected or oversized log (see the security surface
/// in the crate rustdoc).
pub const MAX_EVENT_DATA: usize = 8 * 1024;

vertex_storage::table!(pub EventTable, "chain_events", EventKey, StoredEvent);

// The typed postage batch projection and its value-sorted index, declared via
// the projection framework (see `crate::projection`). Both are uncompressed and
// small.
crate::projection!(pub BatchTable, "postage_batches", BatchKey, BatchState);

crate::secondary_index!(
    pub BatchByBalance,
    "postage_batch_by_balance",
    BalanceKey,
    BatchTable,
    |b| BalanceKey {
        balance: b.normalised_balance,
        batch_id: b.batch_id,
    }
);

/// The set of tables this crate persists, for one-shot initialization.
pub struct ContractIndexTables;

impl Tables for ContractIndexTables {
    const NAMES: &'static [&'static str] =
        &[EventTable::NAME, BatchTable::NAME, BatchByBalance::NAME];
}

impl ContractIndexTables {
    /// Create the store's tables if they do not yet exist.
    pub fn init<DB: Database>(db: &DB) -> Result<(), DatabaseError> {
        <Self as Tables>::init(db)
    }
}

/// The [`EventTable`] key: `(contract, block, log_index)`.
///
/// Encoded as `[contract_tag u8][block u64 BE][log_index u64 BE]` = 17 bytes,
/// so the btree groups a contract's rows together and orders them by canonical
/// `(block, log_index)` within the contract. The position is immutable for a
/// finalized log, which is what makes the upsert idempotent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EventKey {
    /// The watched contract this event belongs to.
    pub contract: ContractId,
    /// The block the event was emitted in.
    pub block: u64,
    /// The event's index within its block.
    pub log_index: u64,
}

impl EventKey {
    /// The first key of `contract`'s range (block 0, log 0).
    pub const fn range_start(contract: ContractId) -> Self {
        Self {
            contract,
            block: 0,
            log_index: 0,
        }
    }

    /// The first key of `contract`'s range at or after `from_block`.
    pub const fn range_from(contract: ContractId, from_block: u64) -> Self {
        Self {
            contract,
            block: from_block,
            log_index: 0,
        }
    }

    /// The last key of `contract`'s range (block/log saturated to `MAX`).
    pub const fn range_end(contract: ContractId) -> Self {
        Self {
            contract,
            block: u64::MAX,
            log_index: u64::MAX,
        }
    }
}

impl Encode for EventKey {
    type Encoded = [u8; 17];

    fn encode(self) -> Self::Encoded {
        let mut out = [0u8; 17];
        out[0] = self.contract.tag();
        out[1..9].copy_from_slice(&self.block.to_be_bytes());
        out[9..17].copy_from_slice(&self.log_index.to_be_bytes());
        out
    }
}

impl Decode for EventKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 17] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        let contract = ContractId::from_tag(bytes[0]).ok_or(DatabaseError::Decode)?;
        let mut b = [0u8; 8];
        b.copy_from_slice(&bytes[1..9]);
        let block = u64::from_be_bytes(b);
        b.copy_from_slice(&bytes[9..17]);
        let log_index = u64::from_be_bytes(b);
        Ok(Self {
            contract,
            block,
            log_index,
        })
    }
}

/// One event recorded verbatim: exactly the bytes the log carried.
///
/// `topics` includes `topic0` as its first element (the EVM bounds it to <= 4).
/// `data` is the non-indexed ABI tail. A view re-derives the typed event from
/// `topics` + `data` with the concrete `sol!` type; this struct asserts nothing
/// about meaning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredEvent {
    /// The emitting contract address, a redundant cross-check a view can assert.
    pub address: Address,
    /// The event's `topic0` (also `topics[0]`), kept for cheap filtering.
    pub topic0: B256,
    /// All indexed topics, `topic0` first (EVM-bounded to <= 4).
    pub topics: Vec<B256>,
    /// The non-indexed ABI tail, verbatim.
    pub data: Bytes,
}

impl StoredEvent {
    /// Reconstruct the [`alloy_primitives::LogData`] for `sol!` decoding.
    ///
    /// A view decodes the typed event with
    /// `E::decode_log_data(&stored.log_data())`, reproducing the original ABI
    /// payload without storing the typed value.
    pub fn log_data(&self) -> alloy_primitives::LogData {
        alloy_primitives::LogData::new_unchecked(self.topics.clone(), self.data.clone())
    }
}

/// The [`BatchTable`] key: an on-chain `batchId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BatchKey(pub B256);

vertex_storage::impl_fixed_codec!(BatchKey, 32);

impl From<B256> for BatchKey {
    fn from(id: B256) -> Self {
        Self(id)
    }
}

impl From<BatchKey> for [u8; 32] {
    fn from(k: BatchKey) -> Self {
        k.0.0
    }
}

impl From<[u8; 32]> for BatchKey {
    fn from(b: [u8; 32]) -> Self {
        Self(B256::from(b))
    }
}

/// The [`BatchByBalance`] index key: a batch's `(normalisedBalance, batchId)`,
/// each big-endian so the index iterates ascending by balance (soonest-to-expire
/// head first), with `batchId` as a deterministic tie-break.
///
/// The `batchId` makes the key UNIQUE per batch. Keying on `normalisedBalance`
/// alone collides two batches with equal balance (the same depth bought at the
/// same price is entirely realistic): the secondary index is a one-to-one
/// `IndexKey -> PrimaryKey` map, so a second equal-balance batch would overwrite
/// the first's index slot and silently drop it from the eviction hint, so an
/// expired batch sharing a balance with another could never be offered for
/// eviction. Encoding `batchId` after `balance` keeps the ascending-balance order
/// (the high-order 32 bytes dominate) while giving every batch a distinct slot.
///
/// A pure ordering hint: the reserve recomputes truth at dequeue. It is not a
/// decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BalanceKey {
    /// The batch's `normalisedBalance`, the primary ascending sort key.
    pub balance: U256,
    /// The batch id, the tie-break that makes the key unique per batch.
    pub batch_id: B256,
}

impl BalanceKey {
    /// The smallest possible index key (balance 0, zero batch id): the inclusive
    /// lower bound of an ascending [`range_head`](crate::projection::range_head).
    pub const fn min() -> Self {
        Self {
            balance: U256::ZERO,
            batch_id: B256::ZERO,
        }
    }

    /// The largest possible index key (max balance, all-ones batch id): the
    /// inclusive upper bound of an ascending range scan.
    pub const fn max() -> Self {
        Self {
            balance: U256::MAX,
            batch_id: B256::repeat_byte(0xff),
        }
    }
}

impl Encode for BalanceKey {
    type Encoded = [u8; 64];

    fn encode(self) -> Self::Encoded {
        let mut out = [0u8; 64];
        out[..32].copy_from_slice(&self.balance.to_be_bytes::<32>());
        out[32..].copy_from_slice(self.batch_id.as_slice());
        out
    }
}

impl Decode for BalanceKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 64] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        let mut bal = [0u8; 32];
        bal.copy_from_slice(&bytes[..32]);
        Ok(Self {
            balance: U256::from_be_bytes(bal),
            batch_id: B256::from_slice(&bytes[32..]),
        })
    }
}

/// The typed postage batch projection row, maintained by the
/// [`PostageReducer`](crate::reducer::PostageReducer) alongside the verbatim
/// [`EventTable`] write for `Postage` rows only.
///
/// This carries the small set of fields the value-sorted eviction index needs
/// plus the `owner` (so `batches_by_owner` needs no second table); the
/// authoritative event history stays in [`EventTable`], and the lazy
/// [`views::postage`](crate::views::postage) validity fold reads that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchState {
    /// The on-chain batch id, carried so the value-sorted index key
    /// ([`BalanceKey`]) is unique per batch even when two batches share a
    /// `normalisedBalance`.
    pub batch_id: B256,
    /// The batch owner.
    pub owner: Address,
    /// The batch depth.
    pub depth: u8,
    /// The batch bucket depth.
    pub bucket_depth: u8,
    /// The batch's `normalisedBalance`, the index ordering key.
    pub normalised_balance: U256,
    /// Whether the batch is immutable.
    pub immutable: bool,
    /// The block the batch was created in.
    pub start_block: u64,
}

/// Write one event verbatim, enforcing the [`MAX_EVENT_DATA`] cap.
///
/// Returns `Ok(false)` (skipped, not an error) when the `data` exceeds the cap,
/// so an oversized log cannot wedge the cursor; the caller logs the skip. The
/// position-keyed put is idempotent by construction.
pub(crate) fn put_event<TX: DbTxMut>(
    tx: &TX,
    key: EventKey,
    event: StoredEvent,
) -> Result<bool, DatabaseError> {
    if event.data.len() > MAX_EVENT_DATA {
        return Ok(false);
    }
    tx.put::<EventTable>(key, event)?;
    Ok(true)
}

/// Read a contract's stored events in canonical `(block, log_index)` order.
///
/// The fold backbone of every lazy view. It scans ONLY this contract's key range
/// `[range_start(contract) ..= range_end(contract)]` via the storage trait's
/// bounded [`range`](vertex_storage::DbTx::range), so a view fold over months of
/// multi-contract history touches just the one contract's rows instead of
/// materializing the whole table. Because [`EventKey`] is namespaced by the
/// contract tag in its high-order byte, the range is exactly that contract's
/// stream, in canonical `(block, log_index)` order (the backend returns the
/// range already ordered by the encoded key).
pub(crate) fn events_of<DB: Database>(
    db: &DB,
    contract: ContractId,
) -> Result<Vec<(EventKey, StoredEvent)>, DatabaseError> {
    db.view(|tx| {
        tx.range::<EventTable>(
            EventKey::range_start(contract),
            EventKey::range_end(contract),
        )
    })
}

/// Read a contract's stored events within an open transaction (the in-tx twin of
/// [`events_of`]), used by the generic revert to gather the surviving rows it
/// hands to [`Reducer::rebuild`](crate::reducer::Reducer::rebuild).
pub(crate) fn events_of_tx<TX: DbTx>(
    tx: &TX,
    contract: ContractId,
) -> Result<Vec<(EventKey, StoredEvent)>, DatabaseError> {
    tx.range::<EventTable>(
        EventKey::range_start(contract),
        EventKey::range_end(contract),
    )
}

/// Build a [`StoredEvent`] from a provider [`Log`], enforcing the EVM topic
/// bound implicitly (the log already carries <= 4 topics).
pub(crate) fn stored_event_from_log(log: &Log) -> StoredEvent {
    let topics = log.topics().to_vec();
    let topic0 = topics.first().copied().unwrap_or_default();
    StoredEvent {
        address: log.address(),
        topic0,
        topics,
        data: log.data().data.clone(),
    }
}
