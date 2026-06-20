//! Per-stamped-entry, proximity-ordered reserve over the vertex-storage
//! `Database`.
//!
//! [`DbReserve`] is the storer's authoritative reserve: the canonical,
//! per-stamped-entry chunk store. It is the reworked successor of the original
//! address-keyed / first-stamp-wins reserve, rebuilt around the consensus
//! invariant that the reserve's *size* counts distinct *stamped entries*
//! (distinct `(batchID, stampIndex, address)`), not distinct content addresses.
//!
//! # Why per-entry, not per-address
//!
//! On Swarm a single content chunk can be stored under several postage batches
//! at once, and a slot within a batch can be *re-stamped* (a newer stamp for the
//! same `(batchID, stampIndex)` supersedes the older one). The redistribution
//! game samples *stamped entries*, the reserve size that drives the storage
//! radius counts *stamped entries*, and an inclusion proof must carry the
//! *precise* stamp a sample slot was won with. An address-keyed, first-stamp-wins
//! store cannot represent any of that. This reserve therefore keys its primary
//! rows by the full stamped-entry identity and shares the (large) chunk payload
//! by reference count so partial eviction of one batch's entry never drops a
//! payload another batch's entry still needs.
//!
//! # The six tables
//!
//! All compound keys are big-endian so the byte order of the encoded key is the
//! `(field, field, ...)` lexicographic order; the orderings are pinned by tests.
//! Every mutation writes (or compacts) all the affected rows inside one
//! `db.update` transaction, so a stamped entry and its index rows commit
//! atomically and a removal never leaves a dangling row (no tombstones).
//!
//! - [`Payload`](schema::Payload): `addr -> (refcnt, typed_bytes)`. The
//!   refcounted, content-addressed chunk body (the type-tagged [`AnyChunk`] bytes,
//!   *without* a stamp, since stamps differ per entry). Present iff at least one
//!   stamped entry references the address; the refcount is the number of such
//!   entries. A second batch storing the same content bumps the refcount and
//!   rewrites no payload; evicting one of several entries decrements it and keeps
//!   the body.
//! - [`Entry`](schema::Entry): `(po, batch, stampHash, addr) -> EntryValue
//!   { binid, stamp }`. One row per stamped entry; the reserve size is this
//!   table's count. The value carries the bin sequence the entry landed at (for
//!   [`Replay`](schema::Replay) compaction) and the precise stamp the entry was
//!   admitted with (so `get` can hand back the chunk with a real stamp and an
//!   inclusion proof can carry exactly that stamp).
//! - [`BatchGroup`](schema::BatchGroup): `(batch, po, addr, stampHash) -> ()`.
//!   Groups every entry by batch (then bin, then address), so a whole batch or a
//!   batch's shallow bins can be evicted with one prefix cursor scan.
//! - [`Replay`](schema::Replay): `(bin, binid) -> ReplayValue { addr, batch,
//!   stampHash, chunk_type }`. The append-only per-bin insertion-order index a
//!   redistribution/sync consumer replays without rehydrating the chunk body.
//!   `chunk_type` lets the sampler resolve the CAC-beats-SOC tie without a body
//!   read.
//! - [`BinCounter`](schema::BinCounter): `bin -> u64`. The per-bin monotonically
//!   increasing insertion sequence (the bin cursor). Never rewound on eviction,
//!   so sequences are never reused (sync resumability).
//! - [`StampIndexTable`]: `(batch, stampIndex_be8) -> (timestamp, stampHash,
//!   addr)`. The newest-timestamp-wins arbiter slot, keyed by the *full*
//!   `(batchID, 8-byte stampIndex)`. Reused verbatim from the postage crate
//!   (PR-C); the reserve performs the arbitration *inside* its own put
//!   transaction with [`postage::decide`] so admission and the six-table write
//!   commit together.
//!
//! # Put, restamp, and second-batch coexistence
//!
//! `put` first validates the stamp on ingest through the stateless
//! [`AdmissionValidator`] (the nectar batch checks plus a per-stamp ecrecover
//! against the batch owner), then, in one transaction:
//!
//! - arbitrates the stamp against its `(batch, stampIndex)` slot
//!   ([`postage::decide`], newest-wins, equal-or-older rejects);
//! - on a *restamp* (the slot held an older stamp) deletes the four rows of the
//!   displaced entry (`Entry`, `BatchGroup`, `Replay`, and the slot occupant is
//!   overwritten) and decrements/compacts its payload, then writes the four new
//!   rows for the incoming entry;
//! - on a *new slot* writes the four rows and, if the same content already had a
//!   payload (a second batch storing it), bumps the refcount instead of
//!   rewriting the body.
//!
//! Eviction (`evict_furthest` / `evict_from_bin` / `evict_batch`) operates on
//! *entries*: it removes an entry's four index rows, the stamp-index slot it
//! owns, and decrements the shared payload, dropping the body only when the last
//! entry referencing it goes.
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

// The public surface: `DbReserve` is re-exported so `crate::db_reserve::DbReserve`
// and `crate::DbReserve` resolve exactly as before the split.
pub use store::DbReserve;

// The consensus spec tests reference the schema and the surrounding API by their
// historical flat names through `use super::*`. Before the split those names
// were in scope in the single module (its tables, plus its top-level `use`
// imports); the re-exports below reproduce exactly that scope so the test bodies
// stay unchanged. They are `#[cfg(test)]` so the production build keeps a minimal
// surface.
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

/// The identity of one stamped entry to evict: its batch, stamp hash and address.
///
/// This is the one record shared across the `store` (put/get/evict) and `tx`
/// (delete) submodules, so it lives in the module root both can reach without a
/// submodule dependency cycle.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EvictTarget {
    pub(crate) batch: BatchId,
    pub(crate) stamp_hash: B256,
    pub(crate) addr: ChunkAddress,
}
