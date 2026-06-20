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
///
/// # Bin and proximity order in the reserve
///
/// [`Bin`] (a routing-table slot) and [`ProximityOrder`] (a metric distance) are
/// distinct nectar types. In *this* trait they coincide deliberately: the reserve
/// measures everything relative to the *local overlay*, so a chunk's reserve bin
/// is exactly its proximity order to the overlay, and the two share the
/// `0..=MAX_PO` range. The bin-scoped verbs ([`evict_from_bin`](Self::evict_from_bin),
/// the `up_to_bin` bound of [`evict_batch`](Self::evict_batch)) therefore name a
/// [`Bin`] but address the proximity index keyed by proximity order; an
/// implementation crosses the boundary explicitly via [`From<ProximityOrder>`](Bin)
/// / [`Bin::get`] rather than treating the two as an unchecked `u8` pun.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait ReserveStore: SwarmLocalStore {
    /// The reserve's current storage-responsibility radius.
    ///
    /// Chunks whose proximity to the local overlay is at or beyond this radius
    /// are within the node's area of responsibility. The radius widens or
    /// narrows as the reserve fills or drains relative to its capacity.
    #[must_use]
    fn storage_radius(&self) -> StorageRadius;

    /// Whether the given address falls within the reserve's current
    /// responsibility radius (i.e. the node is responsible for storing it).
    #[must_use]
    fn is_responsible_for(&self, address: &ChunkAddress) -> bool;

    /// The number of chunks currently held in the reserve.
    fn count(&self) -> SwarmResult<u64>;

    /// The maximum number of chunks the reserve will hold.
    ///
    /// Capacity is enforced by the caller (the eviction-control loop) via the
    /// scoped eviction primitives below, not by [`put`](SwarmLocalStore::put),
    /// which always admits a stamped chunk.
    #[must_use]
    fn capacity(&self) -> u64;

    /// The number of chunks held at the given [`ProximityOrder`] relative to the
    /// local overlay.
    ///
    /// Used to compute where the [`storage_radius`](Self::storage_radius) must
    /// sit to keep the population within capacity. Backed by an O(log n + matches)
    /// cursor walk of the proximity index, not a full table scan.
    fn count_in(&self, po: ProximityOrder) -> SwarmResult<u64>;

    /// Evict the single chunk furthest (lowest proximity) from the local
    /// overlay, returning its address, or `None` if the reserve is empty.
    ///
    /// The point shedding primitive, backed by an O(log n) cursor read of the
    /// proximity index. For bulk shedding under capacity pressure or batch
    /// expiry, prefer [`evict_from_bin`](Self::evict_from_bin) /
    /// [`evict_batch`](Self::evict_batch), which delete a whole group in one
    /// atomic transaction.
    fn evict_furthest(&self) -> SwarmResult<Option<ChunkAddress>>;

    /// Evict up to `max` chunks whose proximity bin equals `bin`, returning the
    /// number evicted.
    ///
    /// The bin-atomic shedding primitive the capacity-control loop uses when the
    /// storage radius grows: a shallow bin is shed as a unit. The targeted rows
    /// are collected up front and deleted (chunk value plus every secondary index
    /// entry) in a single atomic transaction, so the reserve never observes a
    /// partially-evicted bin.
    fn evict_from_bin(&self, bin: Bin, max: u64) -> SwarmResult<u64>;

    /// Evict up to `max` chunks belonging to postage batch `batch`, returning the
    /// number evicted.
    ///
    /// With `up_to_bin = Some(b)` only chunks in bins strictly shallower than `b`
    /// are evicted (shed a batch's out-of-responsibility chunks as the radius
    /// grows); with `up_to_bin = None` the whole batch is evicted (an expired or
    /// invalidated batch). Deletes the chunk value and every secondary index entry
    /// for each target in a single atomic transaction.
    ///
    /// # Expiry ordering seam
    ///
    /// This is the entry point an expiry handler must call to drain an expired
    /// batch's entries *before* the batch is removed from the postage
    /// [`BatchStore`](vertex_swarm_postage::BatchStore). The reserve size counts
    /// per stamped entry and drives the consensus radius, so the invariant is
    /// evict-then-remove: if a batch were removed from the batch store first, its
    /// entries would be orphaned in the reserve (no batch to discover them),
    /// inflating size and the committed radius. Removing the batch first is a bug;
    /// route an expiry through `evict_batch(batch, None, ..)` until it returns `0`
    /// and only then remove the batch.
    fn evict_batch(&self, batch: BatchId, up_to_bin: Option<Bin>, max: u64) -> SwarmResult<u64>;
}

/// A [`ReserveStore`] whose storage radius can be committed at runtime.
///
/// The radius is the consensus-load-bearing output of the size-driven dynamics:
/// it feeds the committed depth a redistribution round commits on chain, and it
/// changes as the reserve fills or drains. [`ReserveStore::storage_radius`] is
/// the *read* seam every consumer uses; this trait adds the matching *write*
/// seam, kept separate so the read surface (which is used widely and behind
/// `dyn`) carries no mutation capability and only the eviction-control loop
/// depends on being able to move the radius.
///
/// The write is a single commit of an already-derived radius: deriving the value
/// (from occupancy against capacity) and shedding the bins that fall out of
/// responsibility are the control loop's job (the storer's radius controller);
/// this method only publishes the result so subsequent
/// [`storage_radius`](ReserveStore::storage_radius) /
/// [`is_responsible_for`](ReserveStore::is_responsible_for) reads observe it. It
/// is infallible and non-blocking: committing a radius moves no data.
///
/// Object-safe (`&self`, a `Copy` argument, no generics), so the controller can
/// hold a `&dyn SettableRadius` as readily as a concrete reserve.
#[auto_impl::auto_impl(&, Arc, Box)]
pub trait SettableRadius: ReserveStore {
    /// Commit a new storage-responsibility radius.
    ///
    /// After this returns, [`storage_radius`](ReserveStore::storage_radius)
    /// reports `radius` and [`is_responsible_for`](ReserveStore::is_responsible_for)
    /// is evaluated against it.
    fn set_storage_radius(&self, radius: StorageRadius);
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
    /// Backed by a lazy read-only cursor that owns its snapshot, so the iterator
    /// outlives the call that opened it. The scan compacts on eviction, so it
    /// yields only entries whose chunk is still present; a consumer that needs the
    /// body still resolves it through [`get`](SwarmLocalStore::get).
    fn scan_bin_from<'a>(
        &'a self,
        bin: Bin,
        start_seq: u64,
    ) -> SwarmResult<Box<dyn Iterator<Item = SwarmResult<BinScanItem>> + Send + 'a>>;
}
