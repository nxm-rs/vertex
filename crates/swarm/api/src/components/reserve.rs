//! The storer reserve: proximity-ordered, always-stamped local chunk storage.
//!
//! Refines [`SwarmLocalStore`] point access with a proximity axis: a
//! [`StorageRadius`] of responsibility, [`count`](ReserveStore::count) against
//! [`capacity`](ReserveStore::capacity), per-proximity-order accounting, and
//! furthest-first eviction. Every chunk in the reserve is stamped; a stampless
//! put is invalid. [`BinCursorStore`] adds an append-only per-bin insertion
//! sequence used by redistribution and sync.

use alloy_primitives::B256;
use nectar_primitives::{Bin, ChunkAddress, ProximityOrder};
use vertex_swarm_primitives::{BatchId, StorageRadius};

use crate::SwarmResult;

use super::SwarmLocalStore;

/// The storer reserve: proximity-ordered, always-stamped local chunk storage.
///
/// Proximity is measured against the local overlay, so a chunk's reserve bin
/// equals its proximity order to the overlay. Bin-scoped verbs name a [`Bin`]
/// but key the proximity index by proximity order; cross via
/// [`From<ProximityOrder>`](Bin) / [`Bin::get`], never an unchecked `u8` pun.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait ReserveStore: SwarmLocalStore {
    /// The reserve's current storage-responsibility radius. Chunks at or beyond
    /// it are within the node's area of responsibility; it widens or narrows as
    /// the reserve fills or drains relative to capacity.
    #[must_use]
    fn storage_radius(&self) -> StorageRadius;

    /// Whether `address` falls within the current storage radius.
    #[must_use]
    fn is_responsible_for(&self, address: &ChunkAddress) -> bool;

    /// Total chunks currently held in the reserve.
    fn count(&self) -> SwarmResult<u64>;

    /// The maximum number of chunks the reserve will hold. Enforced by the
    /// eviction-control loop, not by [`put`](SwarmLocalStore::put).
    #[must_use]
    fn capacity(&self) -> u64;

    /// Chunks held at `po` relative to the local overlay. Backed by an
    /// O(log n + matches) cursor walk, not a full table scan.
    fn count_in(&self, po: ProximityOrder) -> SwarmResult<u64>;

    /// Evict the single chunk furthest (lowest proximity) from the local
    /// overlay, or `None` if empty. For bulk shedding prefer the group-atomic
    /// [`evict_from_bin`](Self::evict_from_bin) / [`evict_batch`](Self::evict_batch).
    fn evict_furthest(&self) -> SwarmResult<Option<ChunkAddress>>;

    /// Evict up to `max` chunks in `bin`, returning the count. Targets are
    /// deleted (chunk value plus every secondary index entry) in one atomic
    /// transaction, so the reserve never observes a partially-evicted bin.
    fn evict_from_bin(&self, bin: Bin, max: u64) -> SwarmResult<u64>;

    /// Evict up to `max` chunks of batch `batch`, returning the count.
    /// `up_to_bin = Some(b)` evicts only bins strictly shallower than `b` (shed
    /// out-of-responsibility chunks as the radius grows); `None` evicts the whole
    /// batch (expired or invalidated). Atomic per the same rule as
    /// [`evict_from_bin`](Self::evict_from_bin).
    ///
    /// Expiry ordering is evict-then-remove: drain a batch through
    /// `evict_batch(batch, None, ..)` until it returns `0` before removing it
    /// from the [`BatchStore`](vertex_swarm_postage::BatchStore). Removing the
    /// batch first orphans its entries in the reserve (no batch to discover
    /// them), inflating the per-stamped-entry size and the committed radius.
    fn evict_batch(&self, batch: BatchId, up_to_bin: Option<Bin>, max: u64) -> SwarmResult<u64>;
}

/// A [`ReserveStore`] whose storage radius can be committed at runtime.
///
/// The write seam matching the [`storage_radius`](ReserveStore::storage_radius)
/// read seam, kept separate so the widely-used read surface carries no mutation
/// capability. The control loop derives the radius and sheds out-of-responsibility
/// bins; this only publishes an already-derived value and moves no data.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait SettableRadius: ReserveStore {
    /// Publish an already-derived storage radius.
    fn set_storage_radius(&self, radius: StorageRadius);
}

/// A projected row of the reserve's per-bin index: enough to drive
/// redistribution and sync (which chunk, batch, insertion sequence) without
/// rehydrating the chunk body. `stamp_hash` identifies the exact stamp version,
/// so a consumer can detect a re-stamp without fetching the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinScanItem {
    /// Monotonically increasing insertion sequence within the bin.
    pub seq: u64,
    /// Address of the stored chunk.
    pub address: ChunkAddress,
    /// Postage batch the entry was stamped under.
    pub batch_id: BatchId,
    /// Hash of the exact stamp version, to detect a re-stamp without a fetch.
    pub stamp_hash: B256,
}

/// Adds an append-only per-bin insertion-order axis to [`ReserveStore`]. Each
/// bin keeps a monotonic cursor; entries can be replayed from an arbitrary
/// sequence, so redistribution and sync stream "what landed since cursor C"
/// without scanning the whole reserve.
pub trait BinCursorStore: ReserveStore {
    /// Highest sequence assigned in `bin` so far (`0` if empty).
    fn bin_cursor(&self, bin: Bin) -> SwarmResult<u64>;

    /// Replay `bin`'s entries in insertion order from `start_seq`, inclusive.
    /// The iterator is `Send` (movable across an await point) and owns its
    /// snapshot. It compacts on eviction, yielding only entries whose chunk is
    /// still present; resolve the body via [`get`](SwarmLocalStore::get).
    fn scan_bin_from<'a>(
        &'a self,
        bin: Bin,
        start_seq: u64,
    ) -> SwarmResult<Box<dyn Iterator<Item = SwarmResult<BinScanItem>> + Send + 'a>>;
}
