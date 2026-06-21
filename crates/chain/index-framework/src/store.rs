//! Generic event store: one table keyed by `(tag, block, log_index)` holding
//! every watched contract's events verbatim. The key is the position, so a
//! replayed finalized log overwrites its own slot, the btree yields a contract's
//! stream in canonical order, and revert is a per-contract range-delete. Decoding
//! happens on read, not here.

use alloy_primitives::{Address, B256, Bytes};
use alloy_rpc_types_eth::Log;
use serde::{Deserialize, Serialize};
use vertex_storage::{Database, DatabaseError, DbTx, DbTxMut, Decode, Encode};

use crate::tag::ContractTag;

/// Hard cap on a stored event's data length; a real watched event is far
/// smaller, so this only bounds an oversized log.
pub const MAX_EVENT_DATA: usize = 8 * 1024;

vertex_storage::table!(pub EventTable, "chain_events", EventKey, StoredEvent);

/// The [`EventTable`] key: `(tag, block, log_index)`.
///
/// Encoded as `[tag u8][block u64 BE][log_index u64 BE]` = 17 bytes, grouping a
/// contract's rows and ordering them canonically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EventKey {
    /// The watched contract this event belongs to.
    pub tag: ContractTag,
    /// The block the event was emitted in.
    pub block: u64,
    /// The event's index within its block.
    pub log_index: u64,
}

impl EventKey {
    /// The first key of `tag`'s range (block 0, log 0).
    pub const fn range_start(tag: ContractTag) -> Self {
        Self {
            tag,
            block: 0,
            log_index: 0,
        }
    }

    /// The first key of `tag`'s range at or after `from_block`.
    pub const fn range_from(tag: ContractTag, from_block: u64) -> Self {
        Self {
            tag,
            block: from_block,
            log_index: 0,
        }
    }

    /// The last key of `tag`'s range (block/log saturated to `MAX`).
    pub const fn range_end(tag: ContractTag) -> Self {
        Self {
            tag,
            block: u64::MAX,
            log_index: u64::MAX,
        }
    }
}

impl Encode for EventKey {
    type Encoded = [u8; 17];

    fn encode(self) -> Self::Encoded {
        let mut out = [0u8; 17];
        out[0] = self.tag.0;
        out[1..9].copy_from_slice(&self.block.to_be_bytes());
        out[9..17].copy_from_slice(&self.log_index.to_be_bytes());
        out
    }
}

impl Decode for EventKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 17] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        let tag = ContractTag(bytes[0]);
        let block = u64::from_be_bytes(bytes[1..9].try_into().map_err(|_| DatabaseError::Decode)?);
        let log_index =
            u64::from_be_bytes(bytes[9..17].try_into().map_err(|_| DatabaseError::Decode)?);
        Ok(Self {
            tag,
            block,
            log_index,
        })
    }
}

/// One event recorded verbatim: exactly the bytes the log carried.
///
/// `topics` includes `topic0` as its first element (the EVM bounds it to <= 4).
/// `data` is the non-indexed ABI tail. A domain re-derives the typed event from
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
    /// Reconstruct [`alloy_primitives::LogData`] for `sol!` decoding.
    pub fn log_data(&self) -> alloy_primitives::LogData {
        alloy_primitives::LogData::new_unchecked(self.topics.clone(), self.data.clone())
    }
}

/// Write one event verbatim, enforcing the [`MAX_EVENT_DATA`] cap.
///
/// Returns `Ok(false)` (skipped, not an error) when the `data` exceeds the cap,
/// so an oversized log cannot wedge the cursor; the caller logs the skip. The
/// position-keyed put is idempotent by construction.
pub(crate) fn put_event<TX: DbTxMut>(
    tx: &TX,
    key: EventKey,
    event: &StoredEvent,
) -> Result<bool, DatabaseError> {
    if event.data.len() > MAX_EVENT_DATA {
        return Ok(false);
    }
    tx.put::<EventTable>(key, event.clone())?;
    Ok(true)
}

/// Read a contract's stored events in canonical `(block, log_index)` order.
///
/// Scans only this tag's key range via the bounded [`range`](vertex_storage::DbTx::range),
/// so a fold over multi-contract history touches just one contract's rows.
pub fn events_of<DB: Database>(
    db: &DB,
    tag: ContractTag,
) -> Result<Vec<(EventKey, StoredEvent)>, DatabaseError> {
    db.view(|tx| tx.range::<EventTable>(EventKey::range_start(tag), EventKey::range_end(tag)))
}

/// In-tx twin of [`events_of`], used by revert to gather the surviving rows.
pub(crate) fn events_of_tx<TX: DbTx>(
    tx: &TX,
    tag: ContractTag,
) -> Result<Vec<(EventKey, StoredEvent)>, DatabaseError> {
    tx.range::<EventTable>(EventKey::range_start(tag), EventKey::range_end(tag))
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
