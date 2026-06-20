//! Per-stamped-entry, proximity-ordered reserve over the vertex-storage
//! `Database`.
//!
//! [`DbReserve`] is the storer's authoritative reserve. Its *size* counts
//! distinct *stamped entries* (`(batchID, stampIndex, address)`), not distinct
//! content addresses: one chunk can be stored under several batches, a slot can
//! be re-stamped, and an inclusion proof must carry the precise stamp a sample
//! slot was won with. Primary rows are keyed by full stamped-entry identity; the
//! large chunk body is shared by refcount so evicting one batch's entry never
//! drops a body another entry still needs.
//!
//! # Tables
//!
//! Compound keys are big-endian, so key byte order is the field lexicographic
//! order (pinned by tests). Every mutation commits all affected rows in one
//! `db.update` transaction, so removals leave no dangling rows (no tombstones).
//!
//! - [`Payload`](schema::Payload): `addr -> (refcnt, typed_bytes)`. Refcounted,
//!   content-addressed body ([`AnyChunk`] bytes *without* a stamp). Refcount is
//!   the number of entries referencing the address; the body is rewritten only
//!   on first store.
//! - [`Entry`](schema::Entry): `(po, batch, stampHash, addr) -> EntryValue
//!   { binid, stamp }`. One row per stamped entry; the reserve size is this
//!   table's count. Carries the bin sequence (for [`Replay`](schema::Replay)
//!   compaction) and the exact admitting stamp.
//! - [`BatchGroup`](schema::BatchGroup): `(batch, po, addr, stampHash) -> ()`.
//!   Per-batch grouping so a batch or its shallow bins evict with one prefix scan.
//! - [`Replay`](schema::Replay): `(bin, binid) -> ReplayValue { addr, batch,
//!   stampHash, chunk_type }`. Append-only per-bin insertion-order index replayed
//!   without rehydrating the body; `chunk_type` resolves the CAC-beats-SOC tie
//!   without a body read.
//! - [`BinCounter`](schema::BinCounter): `bin -> u64`. Monotonic per-bin
//!   insertion sequence; never rewound on eviction, so sequences never repeat
//!   (sync resumability).
//! - [`StampIndexTable`]: `(batch, stampIndex_be8) -> (timestamp, stampHash,
//!   addr)`. Newest-timestamp-wins arbiter slot. The reserve arbitrates inside
//!   its own put transaction with [`postage::decide`] so admission and the table
//!   write commit together.
//!
//! # Put and eviction
//!
//! `put` validates the stamp through the stateless [`AdmissionValidator`], then
//! in one transaction arbitrates against the `(batch, stampIndex)` slot
//! ([`postage::decide`], equal-or-older rejects). A restamp deletes the displaced
//! entry's rows and decrements its payload before writing the incoming rows; a
//! new slot writes the rows and bumps the refcount if the body already exists.
//!
//! Eviction operates on entries: remove the index rows and stamp-index slot,
//! decrement the shared payload, drop the body only when the last referencing
//! entry goes.
//!
//! [`AnyChunk`]: nectar_primitives::AnyChunk
//! [`AdmissionValidator`]: vertex_swarm_postage::AdmissionValidator
//! [`StampIndexTable`]: vertex_swarm_postage::StampIndexTable
//! [`postage::decide`]: vertex_swarm_postage::decide

mod schema;
mod store;
mod tx;

#[cfg(test)]
mod consensus_spec;

use alloy_primitives::B256;
use nectar_primitives::ChunkAddress;
use vertex_swarm_primitives::BatchId;

pub use store::DbReserve;

// Re-exports for the consensus spec tests, which reach the schema and API by
// flat name through `use super::*`. `#[cfg(test)]` keeps them out of the build.
#[cfg(test)]
pub(crate) use crate::EvictionStrategy;
#[cfg(test)]
pub(crate) use schema::{BatchGroup, Entry, Payload, Replay};
#[cfg(test)]
pub(crate) use {
    nectar_postage::Stamp,
    vertex_storage::{Database, DbTx, Table},
    vertex_swarm_api::{BinCursorStore, ReserveStore, SwarmError, SwarmLocalStore, SwarmResult},
    vertex_swarm_postage::{AdmissionValidator, BatchStore, StampIndexTable, StampSlotKey},
    vertex_swarm_primitives::{CachedChunk, OverlayAddress, StorageRadius},
};

/// Identity of one stamped entry to evict. Shared by the `store` and `tx`
/// submodules, so it lives in the module root.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EvictTarget {
    pub(crate) batch: BatchId,
    pub(crate) stamp_hash: B256,
    pub(crate) addr: ChunkAddress,
}
