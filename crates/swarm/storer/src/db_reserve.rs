//! Persisting, proximity-ordered reserve over the vertex-storage `Database`.
//!
//! [`DbReserve`] is the storer's authoritative reserve: an always-stamped chunk
//! store that adds the proximity axis ([`ReserveStore`]) and the per-bin
//! insertion-order axis ([`BinCursorStore`]) on top of point access
//! ([`SwarmLocalStore`]). It owns a [`DbChunkStore`] for the chunk values, a
//! [`Reserve`] for the capacity counter and the local overlay, and five
//! hand-maintained secondary tables:
//!
//! - [`BinCounter`]: `Bin -> u64`, a per-bin monotonically increasing insertion
//!   sequence (the bin cursor). Never rewound on eviction, so sequences are
//!   never reused (sync resumability).
//! - [`BinSeqIndex`]: `(Bin, u64) -> BinSeqEntry`, the insertion-order index
//!   that maps each assigned sequence to a flat projection of the chunk that
//!   landed there (address, batch id, stamp hash) so an insertion-order replay
//!   ([`scan_bin_from`](BinCursorStore::scan_bin_from)) needs no per-row chunk
//!   read.
//! - [`ProximityIndex`]: `(po: u8, addr: ChunkAddress) -> ()`, the
//!   proximity-ordered index that makes per-bin counting
//!   ([`count_in`](ReserveStore::count_in)), furthest-chunk eviction
//!   ([`evict_furthest`](ReserveStore::evict_furthest)) and bin-atomic eviction
//!   ([`evict_from_bin`](ReserveStore::evict_from_bin)) O(log n) cursor moves.
//! - [`AddrIndex`]: `ChunkAddress -> BinSeqKey`, the reverse lookup from an
//!   address to its `(bin, seq)` so a removal can compact the insertion-order
//!   row (no tombstones, bounded growth).
//! - [`BatchIndex`]: `(batch_id, bin, addr) -> ()`, grouping every chunk by its
//!   postage batch (then bin, then address) so a whole batch, or a batch's
//!   shallow bins, can be evicted ([`evict_batch`](ReserveStore::evict_batch))
//!   with a single prefix cursor scan.
//!
//! All secondaries are maintained *by hand* inside the same `db.update`
//! transaction that writes (or removes/evicts) the chunk value, so a chunk and
//! all its index entries commit atomically and a removal never leaves a dangling
//! index row. This is deliberately not the generic
//! [`SecondaryIndex`](vertex_storage::SecondaryIndex)/`put_indexed` machinery:
//! the per-bin sequence is derived from prior state (the current cursor), not
//! extracted from the value, so it cannot be expressed as a stateless
//! extraction.
//!
//! # Cursor-backed reads (#396)
//!
//! The reserve's range reads ride the lazy read-only cursor that #396 landed
//! ([`DbTx::cursor`](vertex_storage::DbTx::cursor)): the cursor owns its read
//! snapshot, so a `scan_bin_from` iterator built from it outlives the call that
//! opened it. `count_in` and `evict_furthest` walk only the relevant prefix of
//! the proximity index, never the whole chunk table.

use std::sync::Arc;

use alloy_primitives::{B256, keccak256};
use nectar_postage::Stamp;
use nectar_primitives::{Bin, ChunkAddress, ProximityOrder};
use serde::{Deserialize, Serialize};
use tracing::debug;
use vertex_storage::{
    Database, DatabaseError, DbCursorRO, DbTx, DbTxMut, Decode, Encode, Table, table,
};
use vertex_swarm_api::{
    BinCursorStore, BinScanItem, ReserveStore, SwarmError, SwarmLocalStore, SwarmResult,
};
use vertex_swarm_primitives::{BatchId, CachedChunk, OverlayAddress, StampedChunk, StorageRadius};

use crate::db_store::ChunkTable;
use crate::{ChunkStore, DbChunkStore, EvictionStrategy, Reserve, StorerError};

// Per-bin insertion cursor table: `Bin -> u64`.
//
// Holds the highest sequence number assigned in each bin so far. A point read
// gives the bin cursor; the next insertion writes `cursor + 1`.
table!(pub(crate) BinCounter, "reserve_bin_counter", BinKey, u64, compressed = false);

// Per-bin insertion-order index: `(Bin, u64) -> BinSeqEntry`.
//
// Maps each assigned `(bin, seq)` to a flat projection of the chunk that landed
// at that sequence (address + batch id + stamp hash), so a consumer can replay
// a bin in insertion order without rehydrating the chunk body. Uncompressed:
// the value is a small fixed record.
table!(pub(crate) BinSeqIndex, "reserve_bin_seq_index", BinSeqKey, BinSeqEntry, compressed = false);

// Proximity-ordered index: `(po: u8, addr: ChunkAddress) -> ()`.
//
// One row per stored chunk, keyed by proximity order to the local overlay
// (most-significant byte) then address. Byte order therefore groups all chunks
// of a proximity bin contiguously and ascending, so a cursor can count a bin's
// population (`count_in`) or find the globally furthest chunk (`first()`, the
// smallest po) in O(log n). Uncompressed: the value is the unit `()`.
table!(pub(crate) ProximityIndex, "reserve_proximity_index", ProxKey, (), compressed = false);

// Reverse lookup: `ChunkAddress -> (Bin, u64)`.
//
// Maps a stored chunk's address back to the `(bin, seq)` it was assigned, so a
// removal/eviction can delete the matching `BinSeqIndex` row without scanning
// the bin. Without it `remove` could not compact the insertion-order index and
// `scan_bin_from` would surface evicted chunks. `BinSeqKey` is the value here
// (its `serde` derive serves the postcard value codec). Uncompressed.
table!(pub(crate) AddrIndex, "reserve_addr_index", ChunkAddress, BinSeqKey, compressed = false);

// Batch-grouped index: `(batch_id, bin, addr) -> ()`.
//
// One row per stored chunk, keyed by postage batch then proximity bin then
// address. Byte order groups every chunk of a batch contiguously (and within a
// batch, by bin), so a prefix cursor scan finds a whole batch (for an expired
// batch) or a batch's shallow bins (as the radius grows) for eviction. The unit
// value keeps the row minimal. Uncompressed.
table!(pub(crate) BatchIndex, "reserve_batch_index", BatchProxKey, (), compressed = false);

/// Newtype key wrapping a [`Bin`] for the [`BinCounter`] table.
///
/// [`Bin`] is a foreign (nectar) type, so the vertex-storage [`Encode`]/
/// [`Decode`] codecs cannot be implemented for it directly (orphan rule). This
/// local newtype carries the single-byte big-endian encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct BinKey(pub u8);

impl BinKey {
    fn from_bin(bin: Bin) -> Self {
        Self(bin.get())
    }
}

impl Encode for BinKey {
    type Encoded = [u8; 1];

    fn encode(self) -> Self::Encoded {
        [self.0]
    }
}

impl Decode for BinKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 1] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        Ok(Self(bytes[0]))
    }
}

/// Compound key `(Bin, u64)` for the [`BinSeqIndex`] table.
///
/// Hand-rolled big-endian encoding (`[bin: 1][seq: 8]`) so the byte order of
/// the encoded key matches the `(bin, seq)` lexicographic order: all entries
/// for a bin are contiguous and ascending by sequence, which is exactly the
/// order the cursor scan ([`scan_bin_from`](BinCursorStore::scan_bin_from))
/// walks. The newtype is required because both halves (and the tuple) are types
/// the local [`Encode`]/[`Decode`] codecs are not otherwise implemented for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct BinSeqKey {
    bin: u8,
    seq: u64,
}

impl BinSeqKey {
    fn new(bin: Bin, seq: u64) -> Self {
        Self {
            bin: bin.get(),
            seq,
        }
    }
}

impl Encode for BinSeqKey {
    type Encoded = [u8; 9];

    fn encode(self) -> Self::Encoded {
        let mut out = [0u8; 9];
        out[0] = self.bin;
        out[1..].copy_from_slice(&self.seq.to_be_bytes());
        out
    }
}

impl Decode for BinSeqKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 9] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        let mut seq = [0u8; 8];
        seq.copy_from_slice(&bytes[1..]);
        Ok(Self {
            bin: bytes[0],
            seq: u64::from_be_bytes(seq),
        })
    }
}

/// Flat per-bin index value: the projection of a stored chunk that a
/// redistribution/sync consumer needs without rehydrating the chunk body.
///
/// Stored as the [`BinSeqIndex`] value (choice (a) of the rework: widen the
/// index value so the insertion-order scan is a single cursor walk with no
/// per-row chunk reads). Serialized via the value codec (postcard), not the key
/// [`Encode`]/[`Decode`] path, so a plain `serde` derive suffices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BinSeqEntry {
    /// The chunk's address.
    address: ChunkAddress,
    /// The postage batch the chunk was stamped under.
    batch_id: BatchId,
    /// A hash identifying the exact stamp version that admitted the chunk.
    stamp_hash: B256,
}

/// Compound key `(po: u8, addr: ChunkAddress)` for the [`ProximityIndex`] table.
///
/// Hand-rolled big-endian encoding (`[po: 1][addr: 32]`) so byte order matches
/// `(po, addr)` lexicographic order: every chunk at a given proximity order is
/// contiguous, and the global minimum proximity order (the furthest chunk from
/// the overlay) is the table's first key. The newtype is required because the
/// tuple of `u8` and the foreign [`ChunkAddress`] has no local codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct ProxKey {
    po: u8,
    addr: ChunkAddress,
}

impl ProxKey {
    fn new(po: u8, addr: ChunkAddress) -> Self {
        Self { po, addr }
    }
}

impl Encode for ProxKey {
    type Encoded = [u8; 33];

    fn encode(self) -> Self::Encoded {
        let mut out = [0u8; 33];
        out[0] = self.po;
        out[1..].copy_from_slice(self.addr.as_slice());
        out
    }
}

impl Decode for ProxKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 33] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        let addr: [u8; 32] = bytes[1..].try_into().map_err(|_| DatabaseError::Decode)?;
        Ok(Self {
            po: bytes[0],
            addr: ChunkAddress::from(addr),
        })
    }
}

/// Compound key `(batch_id, bin, addr)` for the [`BatchIndex`] table.
///
/// Hand-rolled big-endian encoding (`[batch_id: 32][bin: 1][addr: 32]`, 65
/// bytes) so the byte order matches `(batch_id, bin, addr)` lexicographic order:
/// every chunk of a batch is contiguous, grouped by bin then address. A prefix
/// cursor scan over a batch id yields that batch's chunks bin-ascending, which is
/// exactly what batch eviction walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct BatchProxKey {
    batch_id: BatchId,
    bin: u8,
    addr: ChunkAddress,
}

impl BatchProxKey {
    fn new(batch_id: BatchId, bin: u8, addr: ChunkAddress) -> Self {
        Self {
            batch_id,
            bin,
            addr,
        }
    }
}

impl Encode for BatchProxKey {
    type Encoded = [u8; 65];

    fn encode(self) -> Self::Encoded {
        let mut out = [0u8; 65];
        out[..32].copy_from_slice(self.batch_id.as_slice());
        out[32] = self.bin;
        out[33..].copy_from_slice(self.addr.as_slice());
        out
    }
}

impl Decode for BatchProxKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 65] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        let batch: [u8; 32] = bytes[..32].try_into().map_err(|_| DatabaseError::Decode)?;
        let addr: [u8; 32] = bytes[33..].try_into().map_err(|_| DatabaseError::Decode)?;
        Ok(Self {
            batch_id: BatchId::from(batch),
            bin: bytes[32],
            addr: ChunkAddress::from(addr),
        })
    }
}

/// A stable hash of the exact stamp version that admitted a chunk.
///
/// Keccak over the stamp's canonical 113-byte serialization, so a re-stamp of
/// the same chunk under a different batch/index/timestamp yields a different
/// hash. A consumer compares this to detect a re-stamp without fetching the
/// chunk value.
fn stamp_hash(stamp: &Stamp) -> B256 {
    keccak256(stamp.to_bytes())
}

/// Persisting, proximity-ordered, always-stamped reserve.
///
/// Owns the chunk store, the capacity/eviction [`Reserve`], and the secondary
/// tables. Constructed from a shared database handle, the local identity (for
/// the overlay), a capacity, an eviction strategy, and a starting storage
/// radius. Implements the PR-3 storage lattice:
/// [`SwarmLocalStore`] (point access) -> [`ReserveStore`] (proximity axis) ->
/// [`BinCursorStore`] (insertion-order axis).
pub struct DbReserve<DB: Database> {
    /// Shared database handle. Held directly so the chunk value and the
    /// secondary tables can be written in a single transaction; the owned
    /// [`DbChunkStore`] serves point reads, deletes, and counts.
    db: Arc<DB>,
    /// Chunk value store (`ChunkAddress -> typed stamped bytes`).
    store: DbChunkStore<DB>,
    /// Capacity counter and (legacy) eviction policy; see [`Reserve`].
    reserve: Reserve,
    /// Local overlay address, resolved once from the identity at construction.
    /// Held directly (it is `Copy`) so the proximity/bin computations on the hot
    /// path never round-trip through an `Option`.
    overlay: OverlayAddress,
    /// Current storage-responsibility radius.
    radius: StorageRadius,
}

impl<DB: Database> DbReserve<DB> {
    /// Construct a reserve over a shared database.
    ///
    /// Threads the identity through [`Reserve::with_identity`] so proximity-
    /// ranked eviction has the local overlay, ensures all secondary tables
    /// exist, and initialises the in-memory count from the persisted chunk
    /// table.
    pub fn new(
        db: Arc<DB>,
        identity: &impl vertex_swarm_api::SwarmIdentity,
        capacity: u64,
        strategy: EvictionStrategy,
        radius: StorageRadius,
    ) -> Result<Self, StorerError> {
        // DbChunkStore::new ensures the chunk table; add the secondaries.
        let store = DbChunkStore::new(Arc::clone(&db))?;
        db.update(|tx| {
            tx.ensure_table(BinCounter::NAME)?;
            tx.ensure_table(BinSeqIndex::NAME)?;
            tx.ensure_table(ProximityIndex::NAME)?;
            tx.ensure_table(AddrIndex::NAME)?;
            tx.ensure_table(BatchIndex::NAME)?;
            Ok(())
        })?;

        let overlay = identity.overlay_address();
        let reserve = Reserve::with_strategy(capacity, strategy).with_identity(identity);
        reserve.initialize_from(&store)?;

        Ok(Self {
            db,
            store,
            reserve,
            overlay,
            radius,
        })
    }

    /// The local overlay address (resolved from the identity at construction).
    fn overlay(&self) -> OverlayAddress {
        self.overlay
    }

    /// The bin a chunk address falls into relative to the local overlay.
    fn bin_of(&self, address: &ChunkAddress) -> Bin {
        address.bin(&self.overlay)
    }

    /// The proximity order of a chunk address relative to the local overlay.
    fn po_of(&self, address: &ChunkAddress) -> u8 {
        address.proximity(&self.overlay).get()
    }

    /// Delete every target address (chunk value plus all secondary index rows)
    /// in a single atomic transaction, returning the number actually removed.
    ///
    /// Shared by [`evict_from_bin`](ReserveStore::evict_from_bin) and
    /// [`evict_batch`](ReserveStore::evict_batch): the targets are pre-collected
    /// by a read cursor, so this compacts them as a unit (bin-/batch-atomic).
    fn evict_targets(&self, targets: &[ChunkAddress]) -> SwarmResult<u64> {
        if targets.is_empty() {
            return Ok(0);
        }
        let overlay = self.overlay;
        let removed = self
            .db
            .update(|tx| {
                let mut n = 0u64;
                for addr in targets {
                    if delete_in_tx(tx, &overlay, *addr)? {
                        n += 1;
                    }
                }
                Ok(n)
            })
            .map_err(storage_err)?;
        self.reserve.on_removed_n(removed);
        Ok(removed)
    }
}

impl<DB: Database> SwarmLocalStore for DbReserve<DB> {
    fn put(&self, chunk: CachedChunk) -> SwarmResult<()> {
        // The reserve is always stamped: a stampless put is invalid.
        let address = *chunk.address();
        let (any, stamp) = chunk.into_parts();
        let stamp = stamp.ok_or_else(|| SwarmError::InvalidChunk {
            address: Some(address),
            reason: "reserve put requires a stamp; a stampless put is invalid".into(),
        })?;

        // Project the per-bin index value before the stamp is moved into the
        // stored value: address + batch + a stable hash of this exact stamp.
        let entry = BinSeqEntry {
            address,
            batch_id: stamp.batch(),
            stamp_hash: stamp_hash(&stamp),
        };

        // Encode the value with nectar's reserve codec (stamp + typed chunk).
        let value = StampedChunk::new(any, stamp).to_typed_bytes();
        let bin = self.bin_of(&address);
        let po = self.po_of(&address);

        // Write the chunk value, bump the per-bin cursor, and insert the
        // insertion-order and proximity index entries in one transaction so all
        // commit atomically. Content-addressed: a chunk already present is left
        // untouched and does not consume a new sequence.
        let inserted = self
            .db
            .update(|tx| {
                // Content-addressed: a chunk already present is left untouched and
                // does not consume a new sequence.
                if !chunk_absent(tx, address)? {
                    return Ok(false);
                }
                tx.put::<ChunkTable>(address, value.clone())?;

                let next = tx.get::<BinCounter>(BinKey::from_bin(bin))?.unwrap_or(0) + 1;
                let seq_key = BinSeqKey::new(bin, next);
                tx.put::<BinCounter>(BinKey::from_bin(bin), next)?;
                tx.put::<BinSeqIndex>(seq_key, entry.clone())?;
                tx.put::<ProximityIndex>(ProxKey::new(po, address), ())?;
                // Reverse lookup (for compaction on remove) and batch grouping
                // (for batch eviction), committed in the same tx.
                tx.put::<AddrIndex>(address, seq_key)?;
                tx.put::<BatchIndex>(BatchProxKey::new(entry.batch_id, bin.get(), address), ())?;
                Ok(true)
            })
            .map_err(storage_err)?;

        if inserted {
            self.reserve.on_added();
        }
        Ok(())
    }

    fn get(&self, address: &ChunkAddress) -> SwarmResult<Option<CachedChunk>> {
        let bytes = self.store.get(address).map_err(storage_err)?;
        match bytes {
            None => Ok(None),
            Some(bytes) => {
                let stamped = StampedChunk::from_typed_bytes(address, &bytes).map_err(|e| {
                    SwarmError::InvalidChunk {
                        address: Some(*address),
                        reason: format!("stored reserve value failed to decode: {e}"),
                    }
                })?;
                // The reserve is always stamped, so the cached value carries the
                // stamp (`CachedChunk::from` sets `Some(stamp)`).
                Ok(Some(CachedChunk::from(stamped)))
            }
        }
    }

    fn contains(&self, address: &ChunkAddress) -> bool {
        // Mirror the cache's infallible signature: a backend error is treated
        // as "not present" (the caller re-fetches), matching the SwarmLocalStore
        // contract that `contains` cannot fail.
        self.store.contains(address).unwrap_or(false)
    }

    fn remove(&self, address: &ChunkAddress) -> SwarmResult<()> {
        // Delete the chunk value and EVERY secondary index row (proximity,
        // insertion-order via the reverse lookup, batch grouping) in one
        // transaction, so a removed chunk leaves no dangling index entry and the
        // insertion-order scan stays tombstone-free. The BinCounter is not
        // rewound: sequences are monotonic and never reused (sync resumability).
        let overlay = self.overlay;
        let removed = self
            .db
            .update(|tx| delete_in_tx(tx, &overlay, *address))
            .map_err(storage_err)?;
        if removed {
            self.reserve.on_removed();
        }
        Ok(())
    }
}

impl<DB: Database> ReserveStore for DbReserve<DB> {
    fn storage_radius(&self) -> StorageRadius {
        self.radius
    }

    fn is_responsible_for(&self, address: &ChunkAddress) -> bool {
        // Responsible when the chunk's proximity to the local overlay is at or
        // beyond the radius (nectar proximity is the leading matching-bit count).
        address.proximity(&self.overlay()).get() >= self.radius.get()
    }

    fn count(&self) -> SwarmResult<u64> {
        self.store.count().map_err(storage_err)
    }

    fn capacity(&self) -> u64 {
        self.reserve.capacity()
    }

    fn count_in(&self, po: ProximityOrder) -> SwarmResult<u64> {
        // Cursor range over the proximity index `(po, *)` prefix: seek to the
        // first key at or after `(po, 0..0)` and walk forward while the key's
        // proximity order stays equal, counting lazily. O(log n + matches), not
        // a full table walk.
        let target = po.get();
        let tx = self.db.tx().map_err(storage_err)?;
        let mut cursor = tx.cursor::<ProximityIndex>().map_err(storage_err)?;

        let mut count = 0u64;
        let mut entry = cursor
            .seek(ProxKey::new(target, ChunkAddress::from([0u8; 32])))
            .map_err(storage_err)?;
        while let Some((ProxKey { po: row_po, .. }, ())) = entry {
            if row_po != target {
                break;
            }
            count += 1;
            entry = cursor.next().map_err(storage_err)?;
        }
        Ok(count)
    }

    fn evict_furthest(&self) -> SwarmResult<Option<ChunkAddress>> {
        // The furthest chunk from the overlay is the one with the SMALLEST
        // proximity order (higher po = closer). The proximity index is keyed
        // `[po][addr]` big-endian, so `first()` is exactly that chunk in
        // O(log n) — no table walk. Bulk shedding goes through evict_from_bin /
        // evict_batch (whole-group, single atomic tx); this is the point form.
        let furthest = {
            let tx = self.db.tx().map_err(storage_err)?;
            let mut cursor = tx.cursor::<ProximityIndex>().map_err(storage_err)?;
            cursor
                .first()
                .map_err(storage_err)?
                .map(|(ProxKey { addr, .. }, ())| addr)
        };

        if let Some(addr) = furthest {
            debug!(%addr, "evicting furthest chunk from reserve");
            // remove() compacts the chunk value and every secondary index row in
            // one tx and adjusts the count.
            self.remove(&addr)?;
        }
        Ok(furthest)
    }

    fn evict_from_bin(&self, bin: Bin, max: u64) -> SwarmResult<u64> {
        if max == 0 {
            return Ok(0);
        }
        // Collect up to `max` addresses in this proximity bin via a read cursor
        // over the `[po][addr]` index (the bin's rows are contiguous), then
        // delete them all in one atomic write tx.
        let target = bin.get();
        let mut targets: Vec<ChunkAddress> = Vec::new();
        {
            let tx = self.db.tx().map_err(storage_err)?;
            let mut cursor = tx.cursor::<ProximityIndex>().map_err(storage_err)?;
            let mut entry = cursor
                .seek(ProxKey::new(target, ChunkAddress::from([0u8; 32])))
                .map_err(storage_err)?;
            while let Some((ProxKey { po, addr }, ())) = entry {
                if po != target {
                    break;
                }
                targets.push(addr);
                if targets.len() as u64 >= max {
                    break;
                }
                entry = cursor.next().map_err(storage_err)?;
            }
        }
        self.evict_targets(&targets)
    }

    fn evict_batch(&self, batch: BatchId, up_to_bin: Option<Bin>, max: u64) -> SwarmResult<u64> {
        if max == 0 {
            return Ok(0);
        }
        // Collect up to `max` of the batch's addresses via a prefix cursor over
        // the `[batch][bin][addr]` index. Rows are grouped by batch then bin, so
        // a `Some(b)` bound (bins strictly shallower than `b`) is a contiguous
        // front slice: stop as soon as `bin >= b`. Then delete in one atomic tx.
        let bound = up_to_bin.map(Bin::get);
        let mut targets: Vec<ChunkAddress> = Vec::new();
        {
            let tx = self.db.tx().map_err(storage_err)?;
            let mut cursor = tx.cursor::<BatchIndex>().map_err(storage_err)?;
            let mut entry = cursor
                .seek(BatchProxKey::new(batch, 0, ChunkAddress::from([0u8; 32])))
                .map_err(storage_err)?;
            while let Some((
                BatchProxKey {
                    batch_id,
                    bin,
                    addr,
                },
                (),
            )) = entry
            {
                if batch_id != batch {
                    break;
                }
                // Bins < b are a contiguous front slice within the batch, so stop
                // at the first row that reaches the bound.
                if bound.is_some_and(|b| bin >= b) {
                    break;
                }
                targets.push(addr);
                if targets.len() as u64 >= max {
                    break;
                }
                entry = cursor.next().map_err(storage_err)?;
            }
        }
        self.evict_targets(&targets)
    }
}

impl<DB: Database> BinCursorStore for DbReserve<DB> {
    fn bin_cursor(&self, bin: Bin) -> SwarmResult<u64> {
        // Point read of the per-bin counter; an empty bin reads as 0. The point
        // read is correct and cheap, so it is kept rather than a cursor.
        let cursor = self
            .db
            .view(|tx| tx.get::<BinCounter>(BinKey::from_bin(bin)))
            .map_err(storage_err)?
            .unwrap_or(0);
        Ok(cursor)
    }

    fn scan_bin_from<'a>(
        &'a self,
        bin: Bin,
        start_seq: u64,
    ) -> SwarmResult<Box<dyn Iterator<Item = SwarmResult<BinScanItem>> + Send + 'a>> {
        // A real lazy cursor scan over BinSeqIndex: seek to `(bin, start_seq)`
        // and stream forward, yielding one BinScanItem per row while the key's
        // bin stays equal. The cursor owns its read snapshot (#396), so the
        // returned iterator outlives this call even though the read transaction
        // is dropped here.
        let tx = self.db.tx().map_err(storage_err)?;
        let mut cursor = tx.cursor::<BinSeqIndex>().map_err(storage_err)?;
        let seek = cursor
            .seek(BinSeqKey::new(bin, start_seq))
            .map_err(storage_err)?;

        Ok(Box::new(BinScanIter {
            cursor,
            target_bin: bin.get(),
            // Prime with the seek result; the iterator emits it first, then
            // advances with next().
            pending: Some(seek),
        }))
    }
}

/// Lazy insertion-order scan over [`BinSeqIndex`] for one bin.
///
/// Owns the [`DbCursorRO`] (which owns its read snapshot), so it is self-
/// contained and `Send`. Stops as soon as a row crosses out of `target_bin`.
struct BinScanIter {
    cursor: Box<dyn DbCursorRO<BinSeqIndex> + Send>,
    target_bin: u8,
    /// The next row to consider: `Some(seek_result)` immediately after
    /// construction (so the first `next()` emits the seeked row), then `None`
    /// to drive a `cursor.next()` on each subsequent step.
    pending: Option<Option<(BinSeqKey, BinSeqEntry)>>,
}

impl Iterator for BinScanIter {
    type Item = SwarmResult<BinScanItem>;

    fn next(&mut self) -> Option<Self::Item> {
        // Take the primed seek result on the first call, otherwise advance.
        let row = match self.pending.take() {
            Some(seeded) => seeded,
            None => match self.cursor.next() {
                Ok(row) => row,
                Err(e) => return Some(Err(storage_err(e))),
            },
        };

        let (key, entry) = row?;
        // Stop at the bin boundary: BinSeqIndex is ordered `(bin, seq)`, so the
        // first row whose bin differs ends this bin's range.
        if key.bin != self.target_bin {
            return None;
        }
        Some(Ok(BinScanItem {
            seq: key.seq,
            address: entry.address,
            batch_id: entry.batch_id,
            stamp_hash: entry.stamp_hash,
        }))
    }
}

/// Whether `address` is absent from the chunk table within transaction `tx`.
///
/// The single content-addressed presence probe shared by the put path, so the
/// "a present chunk is left untouched" invariant lives in one place.
fn chunk_absent<T: DbTx>(tx: &T, address: ChunkAddress) -> Result<bool, DatabaseError> {
    Ok(tx.get::<ChunkTable>(address)?.is_none())
}

/// Delete a chunk and EVERY secondary index row for it within transaction `tx`.
///
/// The single deletion path shared by `remove` and the eviction verbs, so the
/// full-compaction invariant (chunk value + proximity + insertion-order +
/// reverse lookup + batch grouping all go together) is defined once. Reads the
/// reverse lookup to find the `(bin, seq)` and the batch id needed to address the
/// insertion-order and batch rows. The `BinCounter` is deliberately not rewound
/// (sequences are monotonic). Returns whether a chunk was actually deleted.
fn delete_in_tx<T: DbTxMut>(
    tx: &T,
    overlay: &OverlayAddress,
    address: ChunkAddress,
) -> Result<bool, DatabaseError> {
    if tx.get::<ChunkTable>(address)?.is_none() {
        return Ok(false);
    }
    // Compact the insertion-order and batch rows via the reverse lookup.
    if let Some(seq_key) = tx.get::<AddrIndex>(address)? {
        if let Some(entry) = tx.get::<BinSeqIndex>(seq_key)? {
            tx.delete::<BatchIndex>(BatchProxKey::new(entry.batch_id, seq_key.bin, address))?;
        }
        tx.delete::<BinSeqIndex>(seq_key)?;
        tx.delete::<AddrIndex>(address)?;
    }
    let po = address.proximity(overlay).get();
    tx.delete::<ProximityIndex>(ProxKey::new(po, address))?;
    tx.delete::<ChunkTable>(address)?;
    Ok(true)
}

/// Map a storer/database error onto the API's storage error, preserving the
/// source so the chain is not lost.
fn storage_err<E>(err: E) -> SwarmError
where
    E: std::error::Error + Send + Sync + 'static,
{
    SwarmError::storage(err)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions over known-bounds fixtures"
)]
mod tests {
    use super::*;
    use alloy_primitives::{B256, Signature};
    use nectar_postage::Stamp;
    use nectar_primitives::{AnyChunk, ContentChunk};
    use tempfile::tempdir;
    use vertex_storage_redb::RedbDatabase;
    use vertex_swarm_api::SwarmIdentity;
    use vertex_swarm_test_utils::MockIdentity;

    fn test_stamp() -> Stamp {
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig)
    }

    /// Build a stamped cached chunk from owned payload bytes.
    fn cached_chunk(payload: Vec<u8>) -> CachedChunk {
        let chunk: AnyChunk = ContentChunk::new(payload)
            .expect("valid content chunk")
            .into();
        CachedChunk::new(chunk, Some(test_stamp()))
    }

    /// Build a stampless cached chunk (an invalid reserve put).
    fn stampless_chunk(payload: Vec<u8>) -> CachedChunk {
        let chunk: AnyChunk = ContentChunk::new(payload)
            .expect("valid content chunk")
            .into();
        CachedChunk::new(chunk, None)
    }

    /// A stamp under the postage batch `repeat_byte(batch)`.
    fn stamp_with_batch(batch: u8) -> Stamp {
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        Stamp::new(B256::repeat_byte(batch), 3, 7, 42, sig)
    }

    /// A stamped cached chunk under a chosen batch (for batch-eviction tests).
    fn cached_chunk_batch(payload: Vec<u8>, batch: u8) -> CachedChunk {
        let chunk: AnyChunk = ContentChunk::new(payload)
            .expect("valid content chunk")
            .into();
        CachedChunk::new(chunk, Some(stamp_with_batch(batch)))
    }

    fn new_reserve(
        identity: &MockIdentity,
        capacity: u64,
        strategy: EvictionStrategy,
        radius: StorageRadius,
    ) -> DbReserve<RedbDatabase> {
        let db = RedbDatabase::in_memory().unwrap().into_arc();
        DbReserve::new(db, identity, capacity, strategy, radius).unwrap()
    }

    #[test]
    fn compound_key_round_trips_and_orders() {
        // Big-endian (bin, seq) keys order lexicographically by (bin, seq).
        let bin = Bin::new(5).unwrap();
        let k = BinSeqKey::new(bin, 0x0102030405060708);
        let decoded = BinSeqKey::decode(k.encode().as_ref()).unwrap();
        assert_eq!(decoded, k);

        let a = BinSeqKey::new(Bin::new(1).unwrap(), 9).encode();
        let b = BinSeqKey::new(Bin::new(1).unwrap(), 10).encode();
        let c = BinSeqKey::new(Bin::new(2).unwrap(), 0).encode();
        assert!(a < b, "same bin orders by sequence");
        assert!(b < c, "lower bin sorts before higher bin");

        // BinKey round-trips too.
        let bk = BinKey::from_bin(bin);
        assert_eq!(BinKey::decode(bk.encode().as_ref()).unwrap(), bk);
    }

    #[test]
    fn proximity_key_round_trips_and_orders() {
        // Big-endian (po, addr) keys order by (po, addr); the smallest po sorts
        // first, which is what evict_furthest relies on.
        let addr = ChunkAddress::new([0x11; 32]);
        let k = ProxKey::new(7, addr);
        assert_eq!(ProxKey::decode(k.encode().as_ref()).unwrap(), k);

        let lo = ProxKey::new(0, ChunkAddress::new([0xff; 32])).encode();
        let hi = ProxKey::new(1, ChunkAddress::new([0x00; 32])).encode();
        assert!(
            lo < hi,
            "lower proximity order sorts first regardless of addr"
        );
    }

    #[test]
    fn put_get_round_trip_through_reserve() {
        let identity = MockIdentity::with_first_byte(0x00);
        let reserve = new_reserve(
            &identity,
            100,
            EvictionStrategy::NoEviction,
            StorageRadius::ZERO,
        );

        let chunk = cached_chunk(b"hello reserve".to_vec());
        let address = *chunk.address();
        reserve.put(chunk.clone()).unwrap();

        let got = reserve.get(&address).unwrap().expect("chunk present");
        assert_eq!(got.address(), &address);
        // Stored value round-trips through nectar's stamped codec with a stamp.
        assert!(got.stamp().is_some(), "reserve values are always stamped");
        assert_eq!(got.chunk(), chunk.chunk());
        assert!(reserve.contains(&address));
    }

    #[test]
    fn stampless_put_is_rejected() {
        let identity = MockIdentity::with_first_byte(0x00);
        let reserve = new_reserve(
            &identity,
            100,
            EvictionStrategy::NoEviction,
            StorageRadius::ZERO,
        );

        let chunk = stampless_chunk(b"no stamp here".to_vec());
        let address = *chunk.address();
        let err = reserve.put(chunk).expect_err("stampless put must fail");
        assert!(matches!(err, SwarmError::InvalidChunk { .. }));
        // Nothing was written.
        assert!(!reserve.contains(&address));
        assert_eq!(reserve.count().unwrap(), 0);
    }

    #[test]
    fn count_and_count_in_track_population() {
        let identity = MockIdentity::with_first_byte(0x00);
        let reserve = new_reserve(
            &identity,
            100,
            EvictionStrategy::NoEviction,
            StorageRadius::ZERO,
        );

        let mut addrs = Vec::new();
        for i in 0..6u8 {
            let chunk = cached_chunk(vec![i; 64]);
            addrs.push(*chunk.address());
            reserve.put(chunk).unwrap();
        }
        assert_eq!(reserve.count().unwrap(), 6);

        // The sum of count_in over every proximity order equals the total.
        let overlay = identity.overlay_address();
        let mut total = 0u64;
        for po in 0..=nectar_primitives::MAX_PO {
            total += reserve.count_in(ProximityOrder::new(po).unwrap()).unwrap();
        }
        assert_eq!(total, 6);

        // count_in for a specific address's PO includes that address, and matches
        // an independent count over the chunk table.
        let target_po = addrs[0].proximity(&overlay);
        let independent = addrs
            .iter()
            .filter(|a| a.proximity(&overlay) == target_po)
            .count() as u64;
        assert_eq!(reserve.count_in(target_po).unwrap(), independent);
        assert!(independent >= 1);
    }

    #[test]
    fn scan_bin_from_streams_bin_in_seq_order() {
        let identity = MockIdentity::with_first_byte(0x00);
        let reserve = new_reserve(
            &identity,
            100,
            EvictionStrategy::NoEviction,
            StorageRadius::ZERO,
        );
        let overlay = identity.overlay_address();

        // Insert chunks and record, per bin, the (seq, address) it landed at by
        // mirroring the reserve's own counter.
        let mut expected: std::collections::HashMap<u8, Vec<(u64, ChunkAddress)>> =
            std::collections::HashMap::new();
        let mut per_bin_seq: std::collections::HashMap<u8, u64> = std::collections::HashMap::new();
        for i in 0..20u8 {
            let chunk = cached_chunk(vec![i; 80]);
            let addr = *chunk.address();
            let bin = addr.bin(&overlay).get();
            reserve.put(chunk).unwrap();
            let seq = per_bin_seq.entry(bin).or_insert(0);
            *seq += 1;
            expected.entry(bin).or_default().push((*seq, addr));
        }

        // For every populated bin, a full scan from seq 0 yields exactly that
        // bin's rows in ascending sequence order.
        for (raw_bin, rows) in &expected {
            let bin = Bin::new(*raw_bin).unwrap();
            let scanned: Vec<_> = reserve
                .scan_bin_from(bin, 0)
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            assert_eq!(scanned.len(), rows.len(), "bin {raw_bin} row count");
            for (item, (seq, addr)) in scanned.iter().zip(rows.iter()) {
                assert_eq!(item.seq, *seq, "bin {raw_bin} seq order");
                assert_eq!(&item.address, addr, "bin {raw_bin} address");
                assert_eq!(item.batch_id, test_stamp().batch(), "bin {raw_bin} batch");
                assert_eq!(item.stamp_hash, stamp_hash(&test_stamp()), "stamp hash");
            }

            // A mid-bin start_seq skips earlier sequences (inclusive lower bound).
            if let Some((mid_seq, _)) = rows.get(rows.len() / 2) {
                let from_mid: Vec<_> = reserve
                    .scan_bin_from(bin, *mid_seq)
                    .unwrap()
                    .map(|r| r.unwrap().seq)
                    .collect();
                assert!(from_mid.iter().all(|s| s >= mid_seq));
                assert_eq!(from_mid.first(), Some(mid_seq));
            }
        }
    }

    #[test]
    fn scan_bin_from_stops_at_bin_boundary() {
        // A scan must never bleed into the next bin's rows even though they are
        // contiguous in the underlying table.
        let identity = MockIdentity::with_first_byte(0x00);
        let reserve = new_reserve(
            &identity,
            200,
            EvictionStrategy::NoEviction,
            StorageRadius::ZERO,
        );
        let overlay = identity.overlay_address();
        for i in 0..40u8 {
            reserve.put(cached_chunk(vec![i, i, 1])).unwrap();
        }
        // Pick a populated bin that is not the highest, so a "next bin" exists.
        let mut bins: Vec<u8> = (0..40u8)
            .map(|i| {
                let c = cached_chunk(vec![i, i, 1]);
                c.address().bin(&overlay).get()
            })
            .collect();
        bins.sort_unstable();
        bins.dedup();
        if bins.len() >= 2 {
            let bin = Bin::new(bins[0]).unwrap();
            let scanned: Vec<_> = reserve
                .scan_bin_from(bin, 0)
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            // Every scanned address must actually be in the target bin.
            for item in &scanned {
                assert_eq!(item.address.bin(&overlay).get(), bins[0]);
            }
        }
    }

    #[test]
    fn is_responsible_for_respects_radius() {
        let identity = MockIdentity::with_first_byte(0x00);
        // Radius 0: responsible for everything (proximity >= 0 always holds).
        let r0 = new_reserve(
            &identity,
            10,
            EvictionStrategy::NoEviction,
            StorageRadius::ZERO,
        );
        let chunk = cached_chunk(b"anything".to_vec());
        assert!(r0.is_responsible_for(chunk.address()));

        // A high radius excludes a far address. The overlay's first byte is
        // 0x00; an address whose first byte is 0xff shares zero leading bits.
        let far = ChunkAddress::new([0xff; 32]);
        let r_high = new_reserve(
            &identity,
            10,
            EvictionStrategy::NoEviction,
            StorageRadius::new(Bin::new(4).unwrap()),
        );
        assert!(!r_high.is_responsible_for(&far));
    }

    #[test]
    fn evict_furthest_drops_the_smallest_po_chunk() {
        // Overlay first byte 0x00. evict_furthest must drop the chunk with the
        // smallest proximity order (furthest by XOR), cross-checked against an
        // independent min-po reduce.
        let identity = MockIdentity::with_first_byte(0x00);
        let reserve = new_reserve(
            &identity,
            100,
            EvictionStrategy::NoEviction,
            StorageRadius::ZERO,
        );
        let overlay = identity.overlay_address();

        let mut chunks = Vec::new();
        for i in 0..8u8 {
            chunks.push(cached_chunk(vec![i; 96]));
        }
        for c in &chunks {
            reserve.put(c.clone()).unwrap();
        }
        // Independent expectation: the address with the minimum proximity order
        // (ties broken by the proximity index's address order, but the test set
        // has a unique minimum in practice; assert membership of the min-po set).
        let min_po = chunks
            .iter()
            .map(|c| c.address().proximity(&overlay).get())
            .min()
            .unwrap();
        let min_po_addrs: std::collections::HashSet<_> = chunks
            .iter()
            .map(|c| *c.address())
            .filter(|a| a.proximity(&overlay).get() == min_po)
            .collect();

        let evicted = reserve.evict_furthest().unwrap().expect("a chunk evicted");
        assert!(
            min_po_addrs.contains(&evicted),
            "evicted chunk must have the minimum proximity order"
        );
        // Cross-check against an independent min-proximity-order reduce: the
        // evicted chunk's proximity order is <= every other chunk's (proximity
        // order is the eviction key, NOT full XOR distance — ties at the minimum
        // po are resolved by the index's address order).
        let evicted_po = evicted.proximity(&overlay).get();
        for c in &chunks {
            let a = *c.address();
            if a != evicted {
                assert!(
                    evicted_po <= a.proximity(&overlay).get(),
                    "no remaining chunk may have a smaller proximity order than the evicted one"
                );
            }
        }
        assert!(!reserve.contains(&evicted));
        assert_eq!(reserve.count().unwrap(), 7);
        // count_in for the evicted chunk's po dropped by one relative to the
        // proximity index (the index row was removed with the chunk).
        let po = ProximityOrder::new(min_po).unwrap();
        let remaining_at_min = min_po_addrs.len() as u64 - 1;
        assert_eq!(reserve.count_in(po).unwrap(), remaining_at_min);

        // Eviction on an empty reserve returns None.
        let empty = new_reserve(
            &identity,
            10,
            EvictionStrategy::NoEviction,
            StorageRadius::ZERO,
        );
        assert_eq!(empty.evict_furthest().unwrap(), None);
    }

    #[test]
    fn bin_cursor_advances_per_bin() {
        let identity = MockIdentity::with_first_byte(0x00);
        let reserve = new_reserve(
            &identity,
            100,
            EvictionStrategy::NoEviction,
            StorageRadius::ZERO,
        );

        // An empty bin reads 0.
        let bin0 = Bin::ZERO;
        assert_eq!(reserve.bin_cursor(bin0).unwrap(), 0);

        // Insert chunks, grouping observed cursors by bin. Each bin's cursor
        // equals the number of chunks that landed in it.
        let overlay = identity.overlay_address();
        let mut per_bin = std::collections::HashMap::<u8, u64>::new();
        for i in 0..10u8 {
            let chunk = cached_chunk(vec![i; 48]);
            let bin = chunk.address().bin(&overlay);
            reserve.put(chunk).unwrap();
            *per_bin.entry(bin.get()).or_insert(0) += 1;
        }
        for (raw_bin, expected) in per_bin {
            let cursor = reserve.bin_cursor(Bin::new(raw_bin).unwrap()).unwrap();
            assert_eq!(cursor, expected, "bin {raw_bin} cursor mismatch");
        }
    }

    #[test]
    fn idempotent_put_does_not_double_count() {
        let identity = MockIdentity::with_first_byte(0x00);
        let reserve = new_reserve(
            &identity,
            100,
            EvictionStrategy::NoEviction,
            StorageRadius::ZERO,
        );

        let chunk = cached_chunk(b"once".to_vec());
        let address = *chunk.address();
        let bin = chunk.address().bin(&identity.overlay_address());
        reserve.put(chunk.clone()).unwrap();
        reserve.put(chunk).unwrap();

        assert_eq!(reserve.count().unwrap(), 1);
        // The cursor advanced exactly once for the single distinct chunk.
        assert_eq!(reserve.bin_cursor(bin).unwrap(), 1);
        // The bin scan yields exactly one row.
        let rows: Vec<_> = reserve.scan_bin_from(bin, 0).unwrap().collect();
        assert_eq!(rows.len(), 1);

        // Round-trips on reopen-equivalent fresh read.
        assert!(reserve.contains(&address));
    }

    #[test]
    fn remove_clears_proximity_index() {
        let identity = MockIdentity::with_first_byte(0x00);
        let reserve = new_reserve(
            &identity,
            100,
            EvictionStrategy::NoEviction,
            StorageRadius::ZERO,
        );
        let overlay = identity.overlay_address();
        let chunk = cached_chunk(b"to be removed".to_vec());
        let address = *chunk.address();
        let po = address.proximity(&overlay);
        reserve.put(chunk).unwrap();
        assert_eq!(reserve.count_in(po).unwrap(), 1);

        reserve.remove(&address).unwrap();
        // The proximity index row is gone, so count_in no longer sees it.
        assert_eq!(reserve.count_in(po).unwrap(), 0);
        assert!(!reserve.contains(&address));
        assert_eq!(reserve.count().unwrap(), 0);
    }

    #[test]
    fn persistence_across_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("reserve.redb");
        let identity = MockIdentity::with_first_byte(0x00);
        let chunk = cached_chunk(b"persisted reserve chunk".to_vec());
        let address = *chunk.address();
        let bin = chunk.address().bin(&identity.overlay_address());

        {
            let db = RedbDatabase::create(&path).unwrap().into_arc();
            let reserve = DbReserve::new(
                db,
                &identity,
                100,
                EvictionStrategy::NoEviction,
                StorageRadius::ZERO,
            )
            .unwrap();
            reserve.put(chunk).unwrap();
        }

        let db = RedbDatabase::open(&path).unwrap().into_arc();
        let reserve = DbReserve::new(
            db,
            &identity,
            100,
            EvictionStrategy::NoEviction,
            StorageRadius::ZERO,
        )
        .unwrap();
        // Count is rehydrated from the persisted chunk table at construction.
        assert_eq!(reserve.count().unwrap(), 1);
        assert!(reserve.get(&address).unwrap().is_some());
        // The persisted bin scan still replays the chunk.
        let rows: Vec<_> = reserve
            .scan_bin_from(bin, 0)
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].address, address);
    }

    #[test]
    fn batch_prox_key_round_trips_and_orders() {
        let batch = BatchId::repeat_byte(0x33);
        let addr = ChunkAddress::new([0x11; 32]);
        let k = BatchProxKey::new(batch, 9, addr);
        assert_eq!(BatchProxKey::decode(k.encode().as_ref()).unwrap(), k);

        // Big-endian (batch, bin, addr) keys order lexicographically by that tuple.
        let a = BatchProxKey::new(BatchId::repeat_byte(0x01), 5, ChunkAddress::new([0xff; 32]))
            .encode();
        let b = BatchProxKey::new(BatchId::repeat_byte(0x01), 6, ChunkAddress::new([0x00; 32]))
            .encode();
        let c = BatchProxKey::new(BatchId::repeat_byte(0x02), 0, ChunkAddress::new([0x00; 32]))
            .encode();
        assert!(a < b, "same batch orders by bin");
        assert!(
            b < c,
            "lower batch sorts before higher batch regardless of bin/addr"
        );
    }

    #[test]
    fn remove_compacts_indexes_no_tombstone() {
        // After remove(), the insertion-order scan must not surface the chunk
        // (BinSeqIndex compacted via the reverse lookup), and the chunk can be
        // re-admitted (AddrIndex/BatchIndex cleared).
        let identity = MockIdentity::with_first_byte(0x00);
        let reserve = new_reserve(
            &identity,
            100,
            EvictionStrategy::NoEviction,
            StorageRadius::ZERO,
        );
        let overlay = identity.overlay_address();

        for i in 0..12u8 {
            reserve.put(cached_chunk(vec![i; 64])).unwrap();
        }
        // The i == 0 chunk, then remove it.
        let gone = *cached_chunk(vec![0u8; 64]).address();
        let gone_bin = gone.bin(&overlay);
        reserve.remove(&gone).unwrap();

        let scanned: Vec<_> = reserve
            .scan_bin_from(gone_bin, 0)
            .unwrap()
            .map(|r| r.unwrap().address)
            .collect();
        assert!(
            !scanned.contains(&gone),
            "removed chunk must not appear in the bin scan (no tombstone)"
        );
        assert!(!reserve.contains(&gone));

        // Re-admitting the same content succeeds (the index rows were cleared).
        reserve.put(cached_chunk(vec![0u8; 64])).unwrap();
        assert!(reserve.contains(&gone));
        let again: Vec<_> = reserve
            .scan_bin_from(gone_bin, 0)
            .unwrap()
            .map(|r| r.unwrap().address)
            .collect();
        assert!(
            again.contains(&gone),
            "re-admitted chunk reappears in the scan"
        );
    }

    #[test]
    fn evict_from_bin_sheds_the_whole_bin() {
        let identity = MockIdentity::with_first_byte(0x00);
        let reserve = new_reserve(
            &identity,
            200,
            EvictionStrategy::NoEviction,
            StorageRadius::ZERO,
        );
        let overlay = identity.overlay_address();

        let mut by_bin: std::collections::HashMap<u8, Vec<ChunkAddress>> =
            std::collections::HashMap::new();
        for i in 0..60u8 {
            let c = cached_chunk(vec![i; 50]);
            let addr = *c.address();
            reserve.put(c).unwrap();
            by_bin
                .entry(addr.bin(&overlay).get())
                .or_default()
                .push(addr);
        }
        let (raw_bin, addrs) = by_bin
            .iter()
            .max_by_key(|(_, v)| v.len())
            .map(|(b, v)| (*b, v.clone()))
            .expect("at least one populated bin");
        let bin = Bin::new(raw_bin).unwrap();
        let before = reserve.count().unwrap();

        let evicted = reserve.evict_from_bin(bin, u64::MAX).unwrap();
        assert_eq!(evicted, addrs.len() as u64, "evicts every chunk in the bin");
        assert_eq!(
            reserve
                .count_in(ProximityOrder::new(raw_bin).unwrap())
                .unwrap(),
            0,
            "proximity index compacted"
        );
        assert_eq!(reserve.count().unwrap(), before - addrs.len() as u64);
        assert_eq!(
            reserve.scan_bin_from(bin, 0).unwrap().count(),
            0,
            "insertion-order index compacted"
        );
        for a in &addrs {
            assert!(!reserve.contains(a));
        }

        // The `max` bound caps the number evicted.
        let other = by_bin
            .iter()
            .find(|(b, v)| **b != raw_bin && v.len() >= 2)
            .map(|(b, v)| (*b, v.len()));
        if let Some((raw_other, pop)) = other {
            let n = reserve
                .evict_from_bin(Bin::new(raw_other).unwrap(), 1)
                .unwrap();
            assert_eq!(n, 1, "max caps the eviction count");
            assert_eq!(
                reserve
                    .count_in(ProximityOrder::new(raw_other).unwrap())
                    .unwrap(),
                pop as u64 - 1
            );
        }
    }

    #[test]
    fn evict_batch_whole_batch_only() {
        let identity = MockIdentity::with_first_byte(0x00);
        let reserve = new_reserve(
            &identity,
            200,
            EvictionStrategy::NoEviction,
            StorageRadius::ZERO,
        );

        let mut a_addrs = Vec::new();
        let mut b_addrs = Vec::new();
        for i in 0..15u8 {
            let ca = cached_chunk_batch(vec![i; 40], 0xA1);
            a_addrs.push(*ca.address());
            reserve.put(ca).unwrap();
            let cb = cached_chunk_batch(vec![i; 41], 0xB2);
            b_addrs.push(*cb.address());
            reserve.put(cb).unwrap();
        }
        assert_eq!(reserve.count().unwrap(), 30);

        // max caps it: evict 5 of batch A first.
        let first = reserve
            .evict_batch(BatchId::repeat_byte(0xA1), None, 5)
            .unwrap();
        assert_eq!(first, 5);
        // Then the remaining 10 of batch A.
        let rest = reserve
            .evict_batch(BatchId::repeat_byte(0xA1), None, u64::MAX)
            .unwrap();
        assert_eq!(rest, 10);

        assert_eq!(reserve.count().unwrap(), 15, "only batch B remains");
        for a in &a_addrs {
            assert!(!reserve.contains(a), "batch A fully evicted");
        }
        for b in &b_addrs {
            assert!(reserve.contains(b), "batch B untouched");
        }
    }

    #[test]
    fn evict_batch_respects_up_to_bin() {
        let identity = MockIdentity::with_first_byte(0x00);
        let reserve = new_reserve(
            &identity,
            400,
            EvictionStrategy::NoEviction,
            StorageRadius::ZERO,
        );
        let overlay = identity.overlay_address();

        let mut addrs = Vec::new();
        for i in 0..80u8 {
            let c = cached_chunk_batch(vec![i; 33], 0xC3);
            addrs.push(*c.address());
            reserve.put(c).unwrap();
        }

        // Bins strictly shallower than 1 == bin 0 only.
        let bound = Bin::new(1).unwrap();
        let shallow: Vec<_> = addrs
            .iter()
            .copied()
            .filter(|a| a.bin(&overlay).get() < 1)
            .collect();
        let deep_exists = addrs.iter().any(|a| a.bin(&overlay).get() >= 1);
        // The deterministic CAC addresses populate both sides of bin 1 in practice.
        assert!(
            !shallow.is_empty() && deep_exists,
            "test needs chunks on both sides of the bound"
        );

        let n = reserve
            .evict_batch(BatchId::repeat_byte(0xC3), Some(bound), u64::MAX)
            .unwrap();
        assert_eq!(n, shallow.len() as u64, "evicts exactly the shallow chunks");
        for a in &addrs {
            if a.bin(&overlay).get() < 1 {
                assert!(!reserve.contains(a), "shallow chunk evicted");
            } else {
                assert!(reserve.contains(a), "deep chunk retained");
            }
        }
    }
}
