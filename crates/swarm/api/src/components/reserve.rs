//! The storer reserve: proximity-ordered local chunk storage.
//!
//! The reserve is the storer's authoritative, *stamped* chunk store. It refines
//! [`SwarmLocalStore`]'s point access with the proximity axis the protocol needs:
//! a [`StorageRadius`] of responsibility, a population [`count`](ReserveStore::count)
//! against a [`capacity`](ReserveStore::capacity), per-proximity-order accounting,
//! and furthest-first eviction.
//!
//! Two traits stack on top of [`SwarmLocalStore`]:
//!
//! - [`ReserveStore`] — the proximity axis (radius, responsibility, capacity,
//!   eviction). Every chunk in the reserve is stamped; a stampless put is
//!   invalid.
//! - [`BinCursorStore`] — adds the insertion-order axis, an append-only
//!   per-bin sequence used by redistribution and sync to replay what landed in
//!   a bin since a given cursor.

use alloy_primitives::B256;
use nectar_primitives::{Bin, ChunkAddress, ProximityOrder};
use vertex_swarm_primitives::{BatchId, StorageRadius};

use crate::SwarmResult;

use super::SwarmLocalStore;

/// The storer reserve: proximity-ordered, always-stamped local chunk storage.
///
/// Refines [`SwarmLocalStore`] (point access) with the proximity axis a storer
/// reserve needs. Unlike a client cache, the reserve is authoritative and
/// always stamped — a chunk admitted to the reserve carries the stamp that
/// authorises its storage, so a stampless put is invalid.
///
/// The reserve has a fixed [`capacity`](Self::capacity); when [`count`](Self::count)
/// would exceed it the reserve sheds load by [`evict_furthest`](Self::evict_furthest),
/// dropping the chunk furthest (lowest proximity) from the local overlay first.
/// The [`storage_radius`](Self::storage_radius) is the responsibility boundary the
/// reserve advertises: chunks at or within it are ones the node is responsible
/// for ([`is_responsible_for`](Self::is_responsible_for)).
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait ReserveStore: SwarmLocalStore {
    /// The reserve's current storage-responsibility radius.
    ///
    /// Chunks whose proximity to the local overlay is at or beyond this radius
    /// are within the node's area of responsibility. The radius widens or
    /// narrows as the reserve fills or drains relative to its capacity.
    fn storage_radius(&self) -> StorageRadius;

    /// Whether the given address falls within the reserve's current
    /// responsibility radius (i.e. the node is responsible for storing it).
    fn is_responsible_for(&self, address: &ChunkAddress) -> bool;

    /// The number of chunks currently held in the reserve.
    ///
    /// Counting may be O(N) over the backing store on the current
    /// (#214-interim) cursor implementation.
    fn count(&self) -> SwarmResult<u64>;

    /// The maximum number of chunks the reserve will hold before it must
    /// [`evict_furthest`](Self::evict_furthest) to admit new ones.
    fn capacity(&self) -> u64;

    /// The number of chunks held at the given [`ProximityOrder`] relative to the
    /// local overlay.
    ///
    /// Used to compute where the [`storage_radius`](Self::storage_radius) must
    /// sit to keep the population within capacity. O(N) over the backing store
    /// on the current (#214-interim) implementation.
    fn count_in(&self, po: ProximityOrder) -> SwarmResult<u64>;

    /// Evict the chunk furthest (lowest proximity) from the local overlay,
    /// returning its address, or `None` if the reserve is empty.
    ///
    /// The shedding primitive used when the reserve is over capacity. O(N) over
    /// the backing store on the current (#214-interim) implementation.
    fn evict_furthest(&self) -> SwarmResult<Option<ChunkAddress>>;
}

/// One entry yielded by a per-bin insertion-order scan.
///
/// A flat, projected row of the reserve's append-only per-bin index: enough to
/// drive redistribution and sync (which chunk, under which batch, at what
/// insertion sequence) without rehydrating the chunk body. The `stamp_hash`
/// identifies the exact stamp version that admitted the chunk, so a consumer can
/// detect a re-stamp without fetching the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinScanItem {
    /// The chunk's monotonically increasing insertion sequence within its bin.
    pub seq: u64,
    /// The chunk's address.
    pub address: ChunkAddress,
    /// The postage batch under which the chunk was stamped.
    pub batch_id: BatchId,
    /// A hash identifying the exact stamp version that admitted the chunk.
    pub stamp_hash: B256,
}

/// A reserve that also exposes its append-only, per-bin insertion order.
///
/// Adds the insertion-order axis to [`ReserveStore`]: each bin keeps a
/// monotonically increasing cursor, and chunks can be replayed in insertion
/// order from an arbitrary starting sequence. Redistribution and sync use this
/// to stream "what landed in this bin since cursor C" without scanning the whole
/// reserve.
///
/// Object-safe by construction: [`scan_bin_from`](Self::scan_bin_from) returns a
/// boxed iterator rather than an RPITIT, so the trait can be used behind `dyn`.
pub trait BinCursorStore: ReserveStore {
    /// The current insertion cursor for `bin`: the highest sequence assigned in
    /// that bin so far (`0` if the bin is empty).
    fn bin_cursor(&self, bin: Bin) -> SwarmResult<u64>;

    /// Replay `bin`'s entries in insertion order starting at `start_seq`,
    /// inclusive.
    ///
    /// The iterator yields a fallible [`BinScanItem`] per entry. It is `Send` so
    /// a redistribution/sync task can move it across an await point.
    ///
    /// # #214 status
    ///
    /// Proximity-ordered cursor iteration is blocked on #214 (the redb key order
    /// is byte-order, not XOR-proximity, and `DbCursorRO` is unimplemented).
    /// Implementations may return an unsupported error or an empty iterator
    /// until #214 lands.
    fn scan_bin_from<'a>(
        &'a self,
        bin: Bin,
        start_seq: u64,
    ) -> SwarmResult<Box<dyn Iterator<Item = SwarmResult<BinScanItem>> + Send + 'a>>;
}
