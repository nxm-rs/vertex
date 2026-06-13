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
//! self-healing via [`vertex_storage::IndexedWrite`] and range-reverted
//! alongside [`EventTable`].

use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_rpc_types_eth::Log;
use serde::{Deserialize, Serialize};
use vertex_chain_index::IndexError;
use vertex_storage::{
    Database, DatabaseError, DbTx, DbTxMut, Decode, Encode, IndexedWrite, Table, Tables,
};

use crate::registry::ContractId;

/// The hard cap on a stored event's `data` length, in bytes.
///
/// Every watched event has a small, fixed-size non-indexed payload, so a real
/// event never approaches this. The cap bounds both disk use and view-decode
/// allocation against an unexpected or oversized log (see the security surface
/// in the crate rustdoc).
pub const MAX_EVENT_DATA: usize = 8 * 1024;

vertex_storage::table!(pub EventTable, "chain_events", EventKey, StoredEvent);

// The typed postage batch projection and its value-sorted index. Uncompressed
// (the index macro forces this for the index) and small.
vertex_storage::table!(pub BatchTable, "postage_batches", BatchKey, BatchState);

vertex_storage::index!(
    pub BatchByBalance,
    "postage_batch_by_balance",
    BalanceKey,
    BatchTable,
    |b| BalanceKey(b.normalised_balance)
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

/// The [`BatchByBalance`] index key: a batch's `normalisedBalance`, big-endian
/// so the index iterates ascending (soonest-to-expire head first).
///
/// A pure ordering hint: the reserve recomputes truth at dequeue. It is not a
/// decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BalanceKey(pub U256);

impl Encode for BalanceKey {
    type Encoded = [u8; 32];

    fn encode(self) -> Self::Encoded {
        self.0.to_be_bytes()
    }
}

impl Decode for BalanceKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 32] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        Ok(Self(U256::from_be_bytes(bytes)))
    }
}

/// The typed postage batch projection row, fed alongside the verbatim
/// [`EventTable`] write for `Postage` rows only.
///
/// This carries the small set of fields the value-sorted eviction index needs;
/// the authoritative event history stays in [`EventTable`], and the lazy
/// [`views::postage`](crate::views::postage) validity fold reads that. This row
/// exists only so [`BatchByBalance`] has a typed value to extract from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchState {
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
/// fabricating a partial row, matching the per-branch behaviour.
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

/// Read a contract's stored events in canonical `(block, log_index)` order.
///
/// The fold backbone of every view: it scans `[contract range]` once. redb
/// returns the rows already ordered by the encoded key, so no re-sort is needed.
pub(crate) fn events_of<DB: Database>(
    db: &DB,
    contract: ContractId,
) -> Result<Vec<(EventKey, StoredEvent)>, DatabaseError> {
    db.view(|tx| {
        let mut rows: Vec<(EventKey, StoredEvent)> = tx
            .entries::<EventTable>()?
            .into_iter()
            .filter(|(k, _)| k.contract == contract)
            .collect();
        // entries() iterates the whole table; the filter keeps only this
        // contract's range. The encoded key order already matches
        // (contract, block, log_index), but sort defensively so a view never
        // depends on backend iteration order.
        rows.sort_by(|(a, _), (b, _)| a.cmp(b));
        Ok(rows)
    })
}

/// Range-delete a contract's events (and, for postage, the batch projection +
/// index) from `from_block` onward.
///
/// The generic `revert(from_block)` body. Because every view derives purely from
/// the raw rows, deleting the reorged-out range is necessary and sufficient: no
/// view holds independent state revert could miss. The MVP engine indexes
/// finalized-only and never calls this; it is correct-by-construction today and
/// correct-by-design when head-tracking arrives.
pub(crate) fn revert_contract<DB: Database>(
    db: &DB,
    contract: ContractId,
    from_block: u64,
) -> Result<(), IndexError> {
    db.update(|tx| {
        // The keys to drop: this contract's events at or after from_block.
        let doomed: Vec<EventKey> = tx
            .entries::<EventTable>()?
            .into_iter()
            .filter(|(k, _)| k.contract == contract && k.block >= from_block)
            .map(|(k, _)| k)
            .collect();
        for key in doomed {
            tx.delete::<EventTable>(key)?;
        }

        // The postage batch projection mirrors EventTable; range-delete it too.
        // A batch created at or after from_block is dropped (and its index
        // entry with it); a batch created earlier but mutated in the reverted
        // range is rebuilt on the next forward apply, since EventTable still
        // holds its pre-revert events.
        if contract == ContractId::Postage {
            let doomed_batches: Vec<BatchKey> = tx
                .entries::<BatchTable>()?
                .into_iter()
                .filter(|(_, b)| b.start_block >= from_block)
                .map(|(k, _)| k)
                .collect();
            for key in doomed_batches {
                tx.delete_indexed::<BatchByBalance>(key)?;
            }
        }

        Ok(())
    })?;
    Ok(())
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
