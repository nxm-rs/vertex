//! Per-stamped-entry, proximity-ordered reserve over the vertex-storage
//! `Database`.
//!
//! [`DbReserve`] is the storer's authoritative reserve: a bee-faithful,
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
//! - [`Payload`]: `addr -> (refcnt, typed_bytes)`. The refcounted,
//!   content-addressed chunk body (the type-tagged [`AnyChunk`] bytes, *without*
//!   a stamp, since stamps differ per entry). Present iff at least one stamped
//!   entry references the address; the refcount is the number of such entries.
//!   A second batch storing the same content bumps the refcount and rewrites no
//!   payload; evicting one of several entries decrements it and keeps the body.
//! - [`Entry`]: `(po, batch, stampHash, addr) -> EntryValue { binid, stamp }`.
//!   One row per stamped entry; the reserve size is this table's count. The
//!   value carries the bin sequence the entry landed at (for [`Replay`]
//!   compaction) and the precise stamp the entry was admitted with (so
//!   [`get`](SwarmLocalStore::get) can hand back the chunk with a real stamp and
//!   an inclusion proof can carry exactly that stamp).
//! - [`BatchGroup`]: `(batch, po, addr, stampHash) -> ()`. Groups every entry by
//!   batch (then bin, then address), so a whole batch or a batch's shallow bins
//!   can be evicted with one prefix cursor scan.
//! - [`Replay`]: `(bin, binid) -> ReplayValue { addr, batch, stampHash,
//!   chunk_type }`. The append-only per-bin insertion-order index a
//!   redistribution/sync consumer replays without rehydrating the chunk body.
//!   `chunk_type` lets the sampler resolve the CAC-beats-SOC tie without a body
//!   read.
//! - [`BinCounter`]: `bin -> u64`. The per-bin monotonically increasing
//!   insertion sequence (the bin cursor). Never rewound on eviction, so
//!   sequences are never reused (sync resumability).
//! - [`StampIndexTable`]: `(batch, stampIndex_be8) -> (timestamp, stampHash,
//!   addr)`. The newest-timestamp-wins arbiter slot, keyed by the *full*
//!   `(batchID, 8-byte stampIndex)`. Reused verbatim from the postage crate
//!   (PR-C); the reserve performs the arbitration *inside* its own put
//!   transaction with [`postage::decide`] so admission and the six-table write
//!   commit together.
//!
//! # Put, restamp, and second-batch coexistence
//!
//! [`put`](SwarmLocalStore::put) first validates the stamp on ingest through the
//! stateless [`AdmissionValidator`] (the nectar batch checks plus a per-stamp
//! ecrecover against the batch owner), then, in one transaction:
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
use vertex_swarm_postage::{
    AdmissionValidator, Arbitration, BatchStore, IncomingStamp, PostageContext, StampIndexTable,
    StampSlotKey, decide,
};
use vertex_swarm_primitives::{BatchId, CachedChunk, OverlayAddress, StampedChunk, StorageRadius};

use crate::{EvictionStrategy, Reserve, StorerError};

// -------------------------------------------------------------------------
// Tables (the six-table per-stamped-entry schema).
// -------------------------------------------------------------------------

// Refcounted content payload: `addr -> (refcnt, typed_bytes)`.
//
// One row per distinct *address* (not per entry). The body is the type-tagged
// `AnyChunk` encoding, shared by every stamped entry of that content; the
// refcount is the number of live entries referencing it. Present iff refcnt >=
// 1. Uncompressed: chunk bodies are arbitrary/encrypted, so compression costs
// CPU without saving space.
table!(pub(crate) Payload, "reserve_payload", ChunkAddress, PayloadValue, compressed = false);

// Per-stamped-entry primary index: `(po, batch, stampHash, addr) -> EntryValue`.
//
// One row per stamped entry; the reserve size is the count of this table. Keyed
// proximity-major so the furthest entry (smallest po) is the table's first key
// and a proximity-bin's entries are contiguous. The value carries the bin
// sequence (to address the matching `Replay` row on removal) and the precise
// stamp (so `get` and inclusion proofs use the exact admitting stamp).
table!(pub(crate) Entry, "reserve_entry", EntryKey, EntryValue, compressed = false);

// Batch grouping: `(batch, po, addr, stampHash) -> ()`.
//
// One row per stamped entry, batch-major then bin then address, so a prefix
// cursor over a batch yields its entries bin-ascending for batch eviction.
table!(pub(crate) BatchGroup, "reserve_batch_group", BatchGroupKey, (), compressed = false);

// Insertion-order replay: `(bin, binid) -> ReplayValue`.
//
// The append-only per-bin index a redistribution/sync consumer replays. Keyed
// `(bin, binid)` big-endian so a bin's rows are contiguous and ascending by
// sequence. The value projects what a consumer needs without a body read.
table!(pub(crate) Replay, "reserve_replay", ReplayKey, ReplayValue, compressed = false);

// Per-bin insertion cursor: `bin -> u64`.
//
// The highest sequence assigned in each bin so far; the next insertion writes
// `cursor + 1`. Never rewound on eviction (monotonic, sequences never reused).
table!(pub(crate) BinCounter, "reserve_bin_counter", BinKey, u64, compressed = false);

// Stamp-index arbiter slot: `(batch, stampIndex_be8) -> (timestamp, stampHash,
// addr)`.
//
// This is *the* postage stamp-index table. The reserve does not re-invoke
// `table!` to declare a second type-level handle to the same physical table
// (which would be free to drift in name or value codec and silently
// desynchronise the on-disk slot): it imports the single `StampIndexTable`
// handle the postage crate (PR-C) owns and re-exports. The name and the
// `(key, value)` codec binding therefore live in exactly one place. The reserve
// only changes *when* it is written: rather than calling `DbStampIndexArbiter`,
// it runs the same `decide` arbitration *inside* its own atomic put transaction
// so admission and the six-table write commit together.

// -------------------------------------------------------------------------
// Key newtypes and value records.
// -------------------------------------------------------------------------

/// Newtype key wrapping a [`Bin`] for [`BinCounter`].
///
/// [`Bin`] is a foreign (nectar) type, so the vertex-storage codecs cannot be
/// implemented for it directly (orphan rule). Carries the single-byte encoding.
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

/// The refcounted content payload value: `(refcnt, typed_bytes)`.
///
/// `typed_bytes` is the type-tagged [`AnyChunk`] encoding (no stamp), shared by
/// every stamped entry of the content. `refcnt` is the number of live entries
/// referencing it; the row is deleted when it reaches zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PayloadValue {
    /// Number of live stamped entries referencing this content.
    refcnt: u64,
    /// The type-tagged chunk body, shared across all referencing entries.
    typed_bytes: Vec<u8>,
}

/// Compound key `(po, batch, stampHash, addr)` for [`Entry`].
///
/// Big-endian `[po: 1][batch: 32][stampHash: 32][addr: 32]` (97 bytes): the byte
/// order is proximity-major, so the globally furthest entry (smallest po) is the
/// table's first key and a proximity bin's entries are contiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct EntryKey {
    po: u8,
    batch: BatchId,
    stamp_hash: B256,
    addr: ChunkAddress,
}

impl EntryKey {
    fn new(po: u8, batch: BatchId, stamp_hash: B256, addr: ChunkAddress) -> Self {
        Self {
            po,
            batch,
            stamp_hash,
            addr,
        }
    }
}

impl Encode for EntryKey {
    type Encoded = [u8; 97];

    fn encode(self) -> Self::Encoded {
        let mut out = [0u8; 97];
        out[0] = self.po;
        out[1..33].copy_from_slice(self.batch.as_slice());
        out[33..65].copy_from_slice(self.stamp_hash.as_slice());
        out[65..].copy_from_slice(self.addr.as_slice());
        out
    }
}

impl Decode for EntryKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 97] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        let batch: [u8; 32] = bytes[1..33].try_into().map_err(|_| DatabaseError::Decode)?;
        let hash: [u8; 32] = bytes[33..65]
            .try_into()
            .map_err(|_| DatabaseError::Decode)?;
        let addr: [u8; 32] = bytes[65..].try_into().map_err(|_| DatabaseError::Decode)?;
        Ok(Self {
            po: bytes[0],
            batch: BatchId::from(batch),
            stamp_hash: B256::from(hash),
            addr: ChunkAddress::from(addr),
        })
    }
}

/// The per-entry value: the bin sequence the entry landed at, plus the precise
/// stamp the entry was admitted with (canonical 113-byte encoding).
///
/// The stamp is stored *per entry*, not in the shared payload, because distinct
/// entries of the same content carry distinct stamps. Holding the exact stamp
/// lets [`get`](SwarmLocalStore::get) reconstruct a stamped chunk and lets a
/// later inclusion proof carry the precise stamp the slot was won with, rather
/// than re-loading one by batch id alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EntryValue {
    /// The bin and sequence this entry occupies in [`Replay`].
    bin: u8,
    binid: u64,
    /// The exact admitting stamp, canonical 113-byte encoding.
    stamp_bytes: Vec<u8>,
}

/// Compound key `(batch, po, addr, stampHash)` for [`BatchGroup`].
///
/// Big-endian `[batch: 32][po: 1][addr: 32][stampHash: 32]` (97 bytes): grouped
/// by batch then bin then address, so a batch's entries are a contiguous prefix
/// ascending by bin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct BatchGroupKey {
    batch: BatchId,
    po: u8,
    addr: ChunkAddress,
    stamp_hash: B256,
}

impl BatchGroupKey {
    fn new(batch: BatchId, po: u8, addr: ChunkAddress, stamp_hash: B256) -> Self {
        Self {
            batch,
            po,
            addr,
            stamp_hash,
        }
    }
}

impl Encode for BatchGroupKey {
    type Encoded = [u8; 97];

    fn encode(self) -> Self::Encoded {
        let mut out = [0u8; 97];
        out[..32].copy_from_slice(self.batch.as_slice());
        out[32] = self.po;
        out[33..65].copy_from_slice(self.addr.as_slice());
        out[65..].copy_from_slice(self.stamp_hash.as_slice());
        out
    }
}

impl Decode for BatchGroupKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 97] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        let batch: [u8; 32] = bytes[..32].try_into().map_err(|_| DatabaseError::Decode)?;
        let addr: [u8; 32] = bytes[33..65]
            .try_into()
            .map_err(|_| DatabaseError::Decode)?;
        let hash: [u8; 32] = bytes[65..].try_into().map_err(|_| DatabaseError::Decode)?;
        Ok(Self {
            batch: BatchId::from(batch),
            po: bytes[32],
            addr: ChunkAddress::from(addr),
            stamp_hash: B256::from(hash),
        })
    }
}

/// Compound key `(bin, binid)` for [`Replay`].
///
/// Big-endian `[bin: 1][binid: 8]` so a bin's rows are contiguous and ascending
/// by sequence, exactly the order [`scan_bin_from`](BinCursorStore::scan_bin_from)
/// walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct ReplayKey {
    bin: u8,
    binid: u64,
}

impl ReplayKey {
    fn new(bin: u8, binid: u64) -> Self {
        Self { bin, binid }
    }
}

impl Encode for ReplayKey {
    type Encoded = [u8; 9];

    fn encode(self) -> Self::Encoded {
        let mut out = [0u8; 9];
        out[0] = self.bin;
        out[1..].copy_from_slice(&self.binid.to_be_bytes());
        out
    }
}

impl Decode for ReplayKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 9] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        let mut id = [0u8; 8];
        id.copy_from_slice(&bytes[1..]);
        Ok(Self {
            bin: bytes[0],
            binid: u64::from_be_bytes(id),
        })
    }
}

/// The insertion-order replay value: a flat projection of the stamped entry that
/// landed at a `(bin, binid)`, including the chunk type so a sampler resolves the
/// CAC-beats-SOC tie without a body read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReplayValue {
    address: ChunkAddress,
    batch_id: BatchId,
    stamp_hash: B256,
    /// The chunk type id ([`ChunkTypeId::as_u8`]): 0 = content, 1 = single-owner.
    chunk_type: u8,
}

/// A stable hash of the exact stamp version that admitted an entry.
///
/// Keccak over the stamp's canonical 113-byte serialization, so a re-stamp of
/// the same content under a different batch/index/timestamp yields a different
/// hash. The stamp-entry identity is `(batchID, stampIndex, address)`, but the
/// stamp hash is a compact, collision-resistant stand-in carried in the index
/// rows and is what a consumer compares to detect a re-stamp.
fn stamp_hash(stamp: &Stamp) -> B256 {
    keccak256(stamp.to_bytes())
}

/// Persisting, proximity-ordered, per-stamped-entry reserve.
///
/// Owns a shared database handle (so the payload and all index rows commit in
/// one transaction), an [`AdmissionValidator`] (validate-on-ingest), a
/// [`BatchStore`] (to load the batch a stamp references for validation), the
/// capacity/eviction [`Reserve`] counter, the local overlay and the storage
/// radius. Implements the storage lattice [`SwarmLocalStore`] ->
/// [`ReserveStore`] -> [`BinCursorStore`].
pub struct DbReserve<DB: Database, BS: BatchStore> {
    /// Shared database handle: the payload and every secondary row are written
    /// in one transaction per operation.
    db: Arc<DB>,
    /// Validate-on-ingest admission for stamped chunks (PR-A).
    admission: AdmissionValidator,
    /// The batch store the admission path reads to load a stamp's batch.
    batches: BS,
    /// Confirmation context cache is the validator's; the batch store provides
    /// the live [`PostageContext`] on each put.
    /// Capacity counter; see [`Reserve`]. The authoritative size is the [`Entry`]
    /// table count, but the in-memory counter is kept in step for cheap reads.
    reserve: Reserve,
    /// Local overlay address, resolved once at construction.
    overlay: OverlayAddress,
    /// Current storage-responsibility radius.
    radius: StorageRadius,
}

impl<DB: Database, BS: BatchStore> DbReserve<DB, BS> {
    /// Construct a per-entry reserve over a shared database.
    ///
    /// Ensures all six tables exist, threads the identity through [`Reserve`] for
    /// the overlay, and initialises the in-memory size from the persisted
    /// [`Entry`] table (per-entry count, not per-address).
    pub fn new(
        db: Arc<DB>,
        identity: &impl vertex_swarm_api::SwarmIdentity,
        batches: BS,
        admission: AdmissionValidator,
        capacity: u64,
        strategy: EvictionStrategy,
        radius: StorageRadius,
    ) -> Result<Self, StorerError> {
        db.update(|tx| {
            tx.ensure_table(Payload::NAME)?;
            tx.ensure_table(Entry::NAME)?;
            tx.ensure_table(BatchGroup::NAME)?;
            tx.ensure_table(Replay::NAME)?;
            tx.ensure_table(BinCounter::NAME)?;
            tx.ensure_table(StampIndexTable::NAME)?;
            Ok(())
        })?;

        let overlay = identity.overlay_address();
        let reserve = Reserve::with_strategy(capacity, strategy).with_identity(identity);
        // Per-entry size: initialise from the Entry table count.
        let size = db.view(|tx| tx.count::<Entry>())? as u64;
        reserve.set_count(size);

        Ok(Self {
            db,
            admission,
            batches,
            reserve,
            overlay,
            radius,
        })
    }

    /// The local overlay address.
    fn overlay(&self) -> OverlayAddress {
        self.overlay
    }

    /// The proximity order of a chunk address relative to the local overlay.
    fn po_of(&self, address: &ChunkAddress) -> u8 {
        address.proximity(&self.overlay).get()
    }

    /// The bin a chunk address falls into relative to the local overlay.
    fn bin_of(&self, address: &ChunkAddress) -> Bin {
        address.bin(&self.overlay)
    }

    /// Evict a pre-collected set of stamped entries in one atomic transaction,
    /// returning the number removed. Shared by the eviction verbs.
    fn evict_entries(&self, targets: &[EvictTarget]) -> SwarmResult<u64> {
        if targets.is_empty() {
            return Ok(0);
        }
        let overlay = self.overlay;
        let removed = self
            .db
            .update(|tx| {
                let mut n = 0u64;
                for t in targets {
                    if delete_entry_in_tx(tx, &overlay, t)? {
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

/// The batch-store reads on the synchronous `put` path. These map the typed
/// [`BatchStore::Error`] straight into [`SwarmError::storage`], which preserves
/// it as the diagnostic source; that conversion needs the error to be
/// `Send + Sync + 'static`, so it is the only bound this seam adds (on the
/// associated error, never on the store itself).
impl<DB: Database, BS: BatchStore> DbReserve<DB, BS>
where
    BS::Error: Send + Sync + 'static,
{
    /// Load the batch a stamp references, returning `None` if unknown.
    ///
    /// [`BatchStore`] is synchronous, so the read is a direct call on the
    /// `put` path with no executor or bridge.
    fn load_batch(&self, batch_id: &BatchId) -> SwarmResult<Option<nectar_postage::Batch>> {
        self.batches.get(batch_id).map_err(SwarmError::storage)
    }

    /// The current postage context from the batch store.
    fn context(&self) -> SwarmResult<PostageContext> {
        self.batches.context().map_err(SwarmError::storage)
    }
}

/// The identity of one stamped entry to evict: its batch, stamp hash and address.
#[derive(Debug, Clone, Copy)]
struct EvictTarget {
    batch: BatchId,
    stamp_hash: B256,
    addr: ChunkAddress,
}

// The storage lattice (`SwarmLocalStore: Send + Sync`) requires the store to be
// shareable across the node's async tasks, so the lattice impls keep the
// `BS: Send + Sync` bound. This is the lattice's own threading contract, not the
// async-trait colouring the sync-core change removed: `BatchStore` is now
// synchronous, but `DbReserve` is still held behind a shared handle and read
// from several tasks. The `BS::Error` bound is what lets the typed batch-store
// error map into `SwarmError::storage` on the `put` path.
impl<DB: Database, BS: BatchStore + Send + Sync> SwarmLocalStore for DbReserve<DB, BS>
where
    BS::Error: Send + Sync + 'static,
{
    fn put(&self, chunk: CachedChunk) -> SwarmResult<()> {
        // The reserve is always stamped: a stampless put is invalid.
        let address = *chunk.address();
        let (any, stamp) = chunk.into_parts();
        let stamp = stamp.ok_or_else(|| SwarmError::InvalidChunk {
            address: Some(address),
            reason: "reserve put requires a stamp; a stampless put is invalid".into(),
        })?;

        // --- validate-on-ingest (PR-A) --------------------------------------
        // Load the batch and validate the stamp before any write. A stamp for an
        // unknown batch, or one that fails structural/signature checks, is
        // rejected and nothing is written.
        let batch = self
            .load_batch(&stamp.batch())?
            .ok_or_else(|| SwarmError::InvalidChunk {
                address: Some(address),
                reason: format!("stamp references unknown batch {}", stamp.batch()),
            })?;
        let context = self.context()?;
        self.admission
            .validate(&stamp, &address, &batch, &context)
            .map_err(|e| SwarmError::InvalidChunk {
                address: Some(address),
                reason: format!("stamp failed admission: {e}"),
            })?;

        // Project the per-entry data.
        let hash = stamp_hash(&stamp);
        let po = self.po_of(&address);
        let bin = self.bin_of(&address);
        let chunk_type = any.type_id().as_u8();
        let stamp_bytes = stamp.to_bytes().to_vec();
        let typed_bytes = any.to_typed_bytes();
        let slot = StampSlotKey::new(stamp.batch(), stamp.stamp_index());
        let incoming = IncomingStamp::new(
            stamp.batch(),
            stamp.stamp_index(),
            stamp.timestamp().to_be_bytes(),
            hash,
            address,
        );

        // --- arbitrate + write, atomically ----------------------------------
        let outcome = self
            .db
            .update(|tx| {
                // Newest-wins arbitration against the full (batch, stampIndex)
                // slot, evaluated inside this transaction so the slot cannot
                // change between the read and the conditional write.
                let stored = tx.get::<StampIndexTable>(slot)?;
                match decide(stored.as_ref(), &incoming) {
                    Arbitration::Reject { .. } => Ok(PutOutcome::Rejected),
                    Arbitration::Admit { displaced } => {
                        // Restamp: the slot held an older stamp for this slot.
                        // Delete the four rows of the displaced entry and
                        // decrement its payload before writing the new entry.
                        if let Some(d) = displaced {
                            let target = EvictTarget {
                                batch: incoming.batch_id,
                                stamp_hash: d.stamp_hash,
                                addr: d.address,
                            };
                            // A restamp may re-point the slot at a DIFFERENT
                            // content address, whose proximity order (and hence
                            // po-major Entry/BatchGroup key) differs from the
                            // incoming chunk's `po`. Delete the displaced rows
                            // using the displaced address's own proximity, never
                            // the incoming chunk's, or the lookup misses and the
                            // displaced rows are orphaned with a leaked refcount.
                            let displaced_po = self.po_of(&d.address);
                            let removed = delete_entry_rows_in_tx(tx, displaced_po, &target)?;
                            // The slot occupant is overwritten below; do not
                            // touch BinCounter (monotonic).
                            let _ = removed;
                        }

                        // Assign the next bin sequence and write Replay.
                        let next = tx.get::<BinCounter>(BinKey::from_bin(bin))?.unwrap_or(0) + 1;
                        tx.put::<BinCounter>(BinKey::from_bin(bin), next)?;
                        tx.put::<Replay>(
                            ReplayKey::new(bin.get(), next),
                            ReplayValue {
                                address,
                                batch_id: incoming.batch_id,
                                stamp_hash: hash,
                                chunk_type,
                            },
                        )?;

                        // Entry + BatchGroup.
                        tx.put::<Entry>(
                            EntryKey::new(po, incoming.batch_id, hash, address),
                            EntryValue {
                                bin: bin.get(),
                                binid: next,
                                stamp_bytes: stamp_bytes.clone(),
                            },
                        )?;
                        tx.put::<BatchGroup>(
                            BatchGroupKey::new(incoming.batch_id, po, address, hash),
                            (),
                        )?;

                        // Refcounted payload: bump if present, else write body.
                        bump_or_insert_payload(tx, address, &typed_bytes)?;

                        // Update the arbiter slot to the incoming stamp.
                        tx.put::<StampIndexTable>(slot, incoming.entry())?;

                        Ok(if displaced.is_some() {
                            PutOutcome::Restamped
                        } else {
                            PutOutcome::Admitted
                        })
                    }
                }
            })
            .map_err(storage_err)?;

        match outcome {
            // A new entry: size grows by one. A restamp displaced one entry and
            // added one, so the size is unchanged. A reject writes nothing.
            PutOutcome::Admitted => self.reserve.on_added(),
            PutOutcome::Restamped | PutOutcome::Rejected => {}
        }
        Ok(())
    }

    fn get(&self, address: &ChunkAddress) -> SwarmResult<Option<CachedChunk>> {
        // Return the chunk with its NEWEST valid stamp. The content body lives in
        // the refcounted payload; the stamps live per entry. The newest stamp is
        // the one with the greatest timestamp among this address's entries. (A
        // chunk under N batches has up to N entries here; document: get resolves
        // the single newest. Per-entry/per-stamp access is via the bin scan.)
        let result = self.db.view(|tx| {
            let Some(payload) = tx.get::<Payload>(*address)? else {
                return Ok(None);
            };
            // Find the newest stamp across this address's entries by scanning the
            // proximity-keyed Entry table is O(n); instead read entries grouped
            // by address is not directly keyed, so walk this address's entries by
            // probing every (po) is unnecessary: the address pins the po, so the
            // entries for an address share one po prefix and differ by (batch,
            // stampHash). Walk that contiguous sub-range.
            let po = address.proximity(&self.overlay).get();
            let mut cursor = tx.cursor::<Entry>()?;
            let mut best: Option<(u64, Vec<u8>)> = None;
            let mut row = cursor.seek(EntryKey::new(
                po,
                BatchId::ZERO,
                B256::ZERO,
                ChunkAddress::from([0u8; 32]),
            ))?;
            while let Some((key, value)) = row {
                if key.po != po {
                    break;
                }
                if key.addr == *address {
                    let ts = stamp_timestamp(&value.stamp_bytes);
                    if best.as_ref().is_none_or(|(b, _)| ts > *b) {
                        best = Some((ts, value.stamp_bytes.clone()));
                    }
                }
                row = cursor.next()?;
            }
            Ok(Some((payload.typed_bytes, best)))
        });

        let (typed_bytes, best) = match result.map_err(storage_err)? {
            None => return Ok(None),
            Some(v) => v,
        };
        let Some((_, stamp_bytes)) = best else {
            // Payload present but no entry: should not happen (refcount
            // invariant), treat as absent.
            return Ok(None);
        };
        let stamp = decode_stamp(&stamp_bytes).map_err(|e| SwarmError::InvalidChunk {
            address: Some(*address),
            reason: format!("stored entry stamp failed to decode: {e}"),
        })?;
        // Recombine the shared body with the newest stamp and decode.
        let stamped = StampedChunk::new(decode_body(address, &typed_bytes)?, stamp);
        Ok(Some(CachedChunk::from(stamped)))
    }

    fn contains(&self, address: &ChunkAddress) -> bool {
        // Present iff a payload row exists (refcount >= 1). A backend error is
        // treated as "not present" per the infallible contract.
        self.db
            .view(|tx| Ok(tx.get::<Payload>(*address)?.is_some()))
            .unwrap_or(false)
    }

    fn remove(&self, address: &ChunkAddress) -> SwarmResult<()> {
        // Remove EVERY stamped entry for the address (and the shared payload).
        // Collect the entries first (read cursor), then delete in one tx.
        let targets = self.entries_for_address(address)?;
        let _ = self.evict_entries(&targets)?;
        Ok(())
    }
}

impl<DB: Database, BS: BatchStore> DbReserve<DB, BS> {
    /// Collect every stamped entry for an address (its `(batch, stampHash)`
    /// pairs), for a full removal. The address pins the proximity order, so the
    /// entries are a contiguous sub-range of the `po` prefix.
    fn entries_for_address(&self, address: &ChunkAddress) -> SwarmResult<Vec<EvictTarget>> {
        let po = self.po_of(address);
        let mut targets = Vec::new();
        let tx = self.db.tx().map_err(storage_err)?;
        let mut cursor = tx.cursor::<Entry>().map_err(storage_err)?;
        let mut row = cursor
            .seek(EntryKey::new(
                po,
                BatchId::ZERO,
                B256::ZERO,
                ChunkAddress::from([0u8; 32]),
            ))
            .map_err(storage_err)?;
        while let Some((key, _)) = row {
            if key.po != po {
                break;
            }
            if key.addr == *address {
                targets.push(EvictTarget {
                    batch: key.batch,
                    stamp_hash: key.stamp_hash,
                    addr: key.addr,
                });
            }
            row = cursor.next().map_err(storage_err)?;
        }
        Ok(targets)
    }
}

impl<DB: Database, BS: BatchStore + Send + Sync> ReserveStore for DbReserve<DB, BS>
where
    BS::Error: Send + Sync + 'static,
{
    fn storage_radius(&self) -> StorageRadius {
        self.radius
    }

    fn is_responsible_for(&self, address: &ChunkAddress) -> bool {
        address.proximity(&self.overlay()).get() >= self.radius.get()
    }

    fn count(&self) -> SwarmResult<u64> {
        // Per-entry size: the Entry table count.
        Ok(self
            .db
            .view(|tx| tx.count::<Entry>())
            .map_err(storage_err)? as u64)
    }

    fn capacity(&self) -> u64 {
        self.reserve.capacity()
    }

    fn count_in(&self, po: ProximityOrder) -> SwarmResult<u64> {
        // Cursor range over the Entry `(po, ...)` prefix: every entry at this
        // proximity order is contiguous (po is the leading key byte), so seek to
        // the first key at this po and count forward while po stays equal.
        let target = po.get();
        let tx = self.db.tx().map_err(storage_err)?;
        let mut cursor = tx.cursor::<Entry>().map_err(storage_err)?;
        let mut count = 0u64;
        let mut row = cursor
            .seek(EntryKey::new(
                target,
                BatchId::ZERO,
                B256::ZERO,
                ChunkAddress::from([0u8; 32]),
            ))
            .map_err(storage_err)?;
        while let Some((key, _)) = row {
            if key.po != target {
                break;
            }
            count += 1;
            row = cursor.next().map_err(storage_err)?;
        }
        Ok(count)
    }

    fn evict_furthest(&self) -> SwarmResult<Option<ChunkAddress>> {
        // The furthest entry is the one with the smallest proximity order; the
        // Entry table is keyed `[po][...]`, so `first()` is that entry in
        // O(log n). Eviction is per-entry: a single furthest stamped entry goes.
        let target = {
            let tx = self.db.tx().map_err(storage_err)?;
            let mut cursor = tx.cursor::<Entry>().map_err(storage_err)?;
            cursor
                .first()
                .map_err(storage_err)?
                .map(|(key, _)| EvictTarget {
                    batch: key.batch,
                    stamp_hash: key.stamp_hash,
                    addr: key.addr,
                })
        };

        if let Some(t) = target {
            debug!(addr = %t.addr, "evicting furthest stamped entry from reserve");
            self.evict_entries(&[t])?;
            Ok(Some(t.addr))
        } else {
            Ok(None)
        }
    }

    fn evict_from_bin(&self, bin: Bin, max: u64) -> SwarmResult<u64> {
        if max == 0 {
            return Ok(0);
        }
        // Collect up to `max` entries in this proximity bin via the Entry `[po]`
        // prefix, then delete them in one atomic tx. The Entry table is keyed by
        // proximity-order-to-overlay; for the reserve a `Bin` *is* that proximity
        // order (see `ReserveStore`), so cross the boundary explicitly here rather
        // than punning the byte.
        let target = po_of_reserve_bin(bin);
        let mut targets: Vec<EvictTarget> = Vec::new();
        {
            let tx = self.db.tx().map_err(storage_err)?;
            let mut cursor = tx.cursor::<Entry>().map_err(storage_err)?;
            let mut row = cursor
                .seek(EntryKey::new(
                    target,
                    BatchId::ZERO,
                    B256::ZERO,
                    ChunkAddress::from([0u8; 32]),
                ))
                .map_err(storage_err)?;
            while let Some((key, _)) = row {
                if key.po != target {
                    break;
                }
                targets.push(EvictTarget {
                    batch: key.batch,
                    stamp_hash: key.stamp_hash,
                    addr: key.addr,
                });
                if targets.len() as u64 >= max {
                    break;
                }
                row = cursor.next().map_err(storage_err)?;
            }
        }
        self.evict_entries(&targets)
    }

    fn evict_batch(&self, batch: BatchId, up_to_bin: Option<Bin>, max: u64) -> SwarmResult<u64> {
        if max == 0 {
            return Ok(0);
        }
        // Collect up to `max` of the batch's entries via the BatchGroup
        // `[batch][po][addr][stampHash]` prefix. A `Some(b)` bound (bins strictly
        // shallower than `b`) is a contiguous front slice: stop as soon as po >=
        // b. Then delete in one atomic tx. The bound is a reserve `Bin`, i.e. a
        // proximity-order-to-overlay; cross to the keyed proximity order explicitly.
        let bound = up_to_bin.map(po_of_reserve_bin);
        let mut targets: Vec<EvictTarget> = Vec::new();
        {
            let tx = self.db.tx().map_err(storage_err)?;
            let mut cursor = tx.cursor::<BatchGroup>().map_err(storage_err)?;
            let mut row = cursor
                .seek(BatchGroupKey::new(
                    batch,
                    0,
                    ChunkAddress::from([0u8; 32]),
                    B256::ZERO,
                ))
                .map_err(storage_err)?;
            while let Some((key, _)) = row {
                if key.batch != batch {
                    break;
                }
                if bound.is_some_and(|b| key.po >= b) {
                    break;
                }
                targets.push(EvictTarget {
                    batch: key.batch,
                    stamp_hash: key.stamp_hash,
                    addr: key.addr,
                });
                if targets.len() as u64 >= max {
                    break;
                }
                row = cursor.next().map_err(storage_err)?;
            }
        }
        self.evict_entries(&targets)
    }
}

impl<DB: Database, BS: BatchStore + Send + Sync> BinCursorStore for DbReserve<DB, BS>
where
    BS::Error: Send + Sync + 'static,
{
    fn bin_cursor(&self, bin: Bin) -> SwarmResult<u64> {
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
        // Lazy cursor over Replay: seek to `(bin, start_seq)` and stream forward
        // while the key's bin stays equal. The cursor owns its read snapshot, so
        // the iterator outlives this call.
        let tx = self.db.tx().map_err(storage_err)?;
        let mut cursor = tx.cursor::<Replay>().map_err(storage_err)?;
        let seek = cursor
            .seek(ReplayKey::new(bin.get(), start_seq))
            .map_err(storage_err)?;
        Ok(Box::new(BinScanIter {
            cursor,
            target_bin: bin.get(),
            pending: Some(seek),
        }))
    }
}

/// Lazy insertion-order scan over [`Replay`] for one bin.
struct BinScanIter {
    cursor: Box<dyn DbCursorRO<Replay> + Send>,
    target_bin: u8,
    pending: Option<Option<(ReplayKey, ReplayValue)>>,
}

impl Iterator for BinScanIter {
    type Item = SwarmResult<BinScanItem>;

    fn next(&mut self) -> Option<Self::Item> {
        let row = match self.pending.take() {
            Some(seeded) => seeded,
            None => match self.cursor.next() {
                Ok(row) => row,
                Err(e) => return Some(Err(storage_err(e))),
            },
        };
        let (key, value) = row?;
        if key.bin != self.target_bin {
            return None;
        }
        Some(Ok(BinScanItem {
            seq: key.binid,
            address: value.address,
            batch_id: value.batch_id,
            stamp_hash: value.stamp_hash,
        }))
    }
}

// -------------------------------------------------------------------------
// Transaction helpers (the per-entry write/compaction primitives).
// -------------------------------------------------------------------------

/// Bump the refcount of an existing payload, or insert it with refcount 1.
///
/// Content-addressed: a second stamped entry of the same content shares the body
/// and increments the refcount; the body is never rewritten while present.
fn bump_or_insert_payload<T: DbTxMut>(
    tx: &T,
    address: ChunkAddress,
    typed_bytes: &[u8],
) -> Result<(), DatabaseError> {
    match tx.get::<Payload>(address)? {
        Some(mut p) => {
            p.refcnt += 1;
            tx.put::<Payload>(address, p)?;
        }
        None => {
            tx.put::<Payload>(
                address,
                PayloadValue {
                    refcnt: 1,
                    typed_bytes: typed_bytes.to_vec(),
                },
            )?;
        }
    }
    Ok(())
}

/// Decrement the refcount of a payload, deleting the body when it reaches zero.
///
/// The shared body survives partial eviction: it is dropped only when the last
/// stamped entry referencing it is removed.
fn dec_payload<T: DbTxMut>(tx: &T, address: ChunkAddress) -> Result<(), DatabaseError> {
    if let Some(mut p) = tx.get::<Payload>(address)? {
        if p.refcnt <= 1 {
            tx.delete::<Payload>(address)?;
        } else {
            p.refcnt -= 1;
            tx.put::<Payload>(address, p)?;
        }
    }
    Ok(())
}

/// Delete the four index rows of one stamped entry (`Entry`, `BatchGroup`,
/// `Replay`) and decrement the shared payload, without touching the arbiter slot
/// or the BinCounter. Returns whether the entry existed.
///
/// Used both by the restamp path (displacing the older entry, slot rewritten by
/// the caller) and by `delete_entry_in_tx` (full removal, which also clears the
/// slot).
fn delete_entry_rows_in_tx<T: DbTxMut>(
    tx: &T,
    po: u8,
    target: &EvictTarget,
) -> Result<bool, DatabaseError> {
    let entry_key = EntryKey::new(po, target.batch, target.stamp_hash, target.addr);
    let Some(value) = tx.get::<Entry>(entry_key)? else {
        return Ok(false);
    };
    // Replay row, addressed by the entry's stored (bin, binid).
    tx.delete::<Replay>(ReplayKey::new(value.bin, value.binid))?;
    tx.delete::<BatchGroup>(BatchGroupKey::new(
        target.batch,
        po,
        target.addr,
        target.stamp_hash,
    ))?;
    tx.delete::<Entry>(entry_key)?;
    dec_payload(tx, target.addr)?;
    Ok(true)
}

/// Fully delete a stamped entry: its four index rows, the shared payload
/// decrement, AND its arbiter slot (so the slot does not pin a stale newest
/// stamp after the entry is gone). Returns whether the entry existed.
fn delete_entry_in_tx<T: DbTxMut>(
    tx: &T,
    overlay: &OverlayAddress,
    target: &EvictTarget,
) -> Result<bool, DatabaseError> {
    let po = target.addr.proximity(overlay).get();
    let entry_key = EntryKey::new(po, target.batch, target.stamp_hash, target.addr);
    // Read the entry's stamp to recover its slot key (batch, stampIndex) so the
    // arbiter slot can be cleared. The stamp index is not in the key, so it is
    // decoded from the stored stamp bytes.
    let slot = tx
        .get::<Entry>(entry_key)?
        .and_then(|v| decode_stamp(&v.stamp_bytes).ok())
        .map(|s| StampSlotKey::new(s.batch(), s.stamp_index()));

    let removed = delete_entry_rows_in_tx(tx, po, target)?;
    if removed && let Some(slot) = slot {
        // Clear the slot only if it still points at this entry's stamp hash,
        // so a concurrent restamp's slot is not clobbered.
        if let Some(occupant) = tx.get::<StampIndexTable>(slot)?
            && occupant.stamp_hash == target.stamp_hash
        {
            tx.delete::<StampIndexTable>(slot)?;
        }
    }
    Ok(removed)
}

/// The verdict of a put, used to adjust the in-memory size counter.
enum PutOutcome {
    /// A new stamped entry was added (size += 1).
    Admitted,
    /// An older entry was displaced and a new one added (size unchanged).
    Restamped,
    /// The incoming stamp was stale; nothing written (size unchanged).
    Rejected,
}

/// Decode a stamp from its canonical 113-byte encoding.
fn decode_stamp(bytes: &[u8]) -> Result<Stamp, nectar_postage::StampError> {
    Stamp::try_from_slice(bytes)
}

/// The big-endian timestamp embedded in a canonical stamp encoding (bytes
/// 40..48), used to pick the newest stamp for an address without a full decode.
fn stamp_timestamp(bytes: &[u8]) -> u64 {
    bytes
        .get(40..48)
        .and_then(|s| s.try_into().ok())
        .map_or(0, u64::from_be_bytes)
}

/// Decode the shared content body (type-tagged [`AnyChunk`] bytes) for an
/// address.
fn decode_body(
    address: &ChunkAddress,
    typed_bytes: &[u8],
) -> SwarmResult<nectar_primitives::AnyChunk> {
    nectar_primitives::AnyChunk::from_typed_bytes(address, typed_bytes).map_err(|e| {
        SwarmError::InvalidChunk {
            address: Some(*address),
            reason: format!("stored reserve payload failed to decode: {e}"),
        }
    })
}

/// The proximity order (relative to the local overlay) a reserve [`Bin`] denotes.
///
/// For the reserve a routing [`Bin`] and the [`ProximityOrder`] the Entry/
/// BatchGroup tables key on are the *same* quantity measured against the local
/// overlay (see [`ReserveStore`]); they merely have distinct nectar types because
/// one is a slot and the other a metric, and they share the `0..=MAX_PO` range.
/// This is the single, explicit crossing of that boundary: a `Bin` in, the
/// proximity order it keys on out. The byte value is identical, but routing the
/// conversion through one named helper keeps the `Bin`-vs-`ProximityOrder`
/// conflation intentional and greppable rather than an inline `bin.get()` pun.
#[inline]
fn po_of_reserve_bin(bin: Bin) -> u8 {
    // A `Bin` is range-validated to `0..=MAX_PO`, which is exactly the
    // `ProximityOrder` range, so the proximity order it denotes is its raw byte.
    bin.get()
}

/// Map a storer/database error onto the API's storage error, preserving the
/// source.
fn storage_err<E>(err: E) -> SwarmError
where
    E: std::error::Error + Send + Sync + 'static,
{
    SwarmError::storage(err)
}

// ---------------------------------------------------------------------------
// Key-codec ordering tests (pure, no database).
// ---------------------------------------------------------------------------
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions over known-bounds fixtures"
)]
mod key_codec_tests {
    use super::*;

    #[test]
    fn entry_key_round_trips_and_orders_proximity_major() {
        // Round-trip.
        let k = EntryKey::new(
            7,
            BatchId::repeat_byte(0x11),
            B256::repeat_byte(0x22),
            ChunkAddress::from([0x33u8; 32]),
        );
        assert_eq!(EntryKey::decode(k.encode().as_ref()).unwrap(), k);

        // Proximity-major: a smaller po sorts first regardless of the trailing
        // fields, so `first()` over the table is always the furthest entry.
        let far = EntryKey::new(
            1,
            BatchId::repeat_byte(0xff),
            B256::repeat_byte(0xff),
            ChunkAddress::from([0xffu8; 32]),
        )
        .encode();
        let near =
            EntryKey::new(2, BatchId::ZERO, B256::ZERO, ChunkAddress::from([0u8; 32])).encode();
        assert!(far < near, "smaller proximity order sorts first");

        // Within one po, ordering is by (batch, stampHash, addr).
        let po = 5u8;
        let a =
            EntryKey::new(po, BatchId::ZERO, B256::ZERO, ChunkAddress::from([0u8; 32])).encode();
        let b = EntryKey::new(
            po,
            BatchId::repeat_byte(0x01),
            B256::ZERO,
            ChunkAddress::from([0u8; 32]),
        )
        .encode();
        assert!(a < b, "same po orders by batch next");
    }

    #[test]
    fn batch_group_key_orders_batch_major_then_bin() {
        let k = BatchGroupKey::new(
            BatchId::repeat_byte(0xab),
            9,
            ChunkAddress::from([0xcdu8; 32]),
            B256::repeat_byte(0xef),
        );
        assert_eq!(BatchGroupKey::decode(k.encode().as_ref()).unwrap(), k);

        // A batch's entries are a contiguous prefix, ascending by bin.
        let batch = BatchId::repeat_byte(0x07);
        let lo = BatchGroupKey::new(batch, 1, ChunkAddress::from([0u8; 32]), B256::ZERO).encode();
        let hi = BatchGroupKey::new(batch, 2, ChunkAddress::from([0u8; 32]), B256::ZERO).encode();
        let other = BatchGroupKey::new(
            BatchId::repeat_byte(0x08),
            0,
            ChunkAddress::from([0u8; 32]),
            B256::ZERO,
        )
        .encode();
        assert!(lo < hi, "same batch orders by bin");
        assert!(hi < other, "lower batch sorts before higher batch");
    }

    #[test]
    fn replay_key_orders_bin_major_then_sequence() {
        let k = ReplayKey::new(3, 0x0102_0304_0506_0708);
        assert_eq!(ReplayKey::decode(k.encode().as_ref()).unwrap(), k);

        let a = ReplayKey::new(1, 9).encode();
        let b = ReplayKey::new(1, 10).encode();
        let c = ReplayKey::new(2, 0).encode();
        assert!(a < b, "same bin orders by sequence");
        assert!(b < c, "lower bin sorts before higher bin");
    }

    #[test]
    fn bin_key_round_trips() {
        let bk = BinKey::from_bin(Bin::new(5).unwrap());
        assert_eq!(BinKey::decode(bk.encode().as_ref()).unwrap(), bk);
    }
}

// ---------------------------------------------------------------------------
// Consensus spec tests for the per-entry reserve.
//
// Each test builds a `DbReserve` over an in-memory redb-backed `vertex-storage`
// `Database`, a `DbBatchStore` populated with a real `Batch`, and signs real
// stamps with an `alloy-signer-local` wallet so the validate-on-ingest admission
// path runs exactly as in production. The invariants asserted here are the
// consensus-load-bearing ones: per-entry size counting, newest-wins / equal- and
// older-reject arbitration on the full `(batchID, stampIndex)` slot, refcounted
// payload survival under partial eviction, exact-stamp-in-proof, and full
// compaction (no tombstones) on removal.
// ---------------------------------------------------------------------------
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions over known-bounds fixtures"
)]
mod consensus_spec {
    use super::*;
    use alloy_primitives::Address;
    use alloy_signer::SignerSync;
    use alloy_signer_local::PrivateKeySigner;
    use nectar_postage::{Batch, PostageContext, StampDigest, StampIndex};
    use nectar_primitives::{Chunk, DefaultContentChunk as ContentChunk};
    use std::sync::Arc;
    use vertex_storage_redb::RedbDatabase;
    use vertex_swarm_api::SwarmIdentity as _;
    use vertex_swarm_postage::DbBatchStore;
    use vertex_swarm_test_utils::MockIdentity;

    const THRESHOLD: u64 = 8;
    // bucket_depth 1 splits the address space by the top bit, so two distinct
    // content addresses can be coerced into the *same* bucket (top bit 0) and
    // therefore compete for the same `(batch, stampIndex)` arbiter slot. depth 18
    // gives ample per-bucket capacity for index 0.
    const BUCKET_DEPTH: u8 = 1;
    const DEPTH: u8 = 18;

    fn signer() -> PrivateKeySigner {
        PrivateKeySigner::from_bytes(&B256::repeat_byte(0x42)).expect("valid signer")
    }

    /// A batch owned by `owner`, created at block 0, with ample value so it is
    /// not expired against a zero cumulative payout.
    fn batch_for(owner: Address, id: B256) -> Batch {
        Batch::new(id, 1_000_000, 0, owner, DEPTH, BUCKET_DEPTH, false)
    }

    /// A live context past the confirmation threshold with zero payout (so a
    /// fresh batch is usable and not expired).
    fn live_context() -> PostageContext {
        PostageContext::new(THRESHOLD + 1, 0)
    }

    /// A content chunk whose address falls in bucket 0 of a `BUCKET_DEPTH` batch
    /// (top bit clear), searched by varying the payload. Returns the chunk and
    /// its address.
    fn content_chunk_in_bucket0(seed: u64) -> (nectar_primitives::AnyChunk, ChunkAddress) {
        for n in 0..100_000u64 {
            let payload = format!("vertex reserve consensus fixture {seed}/{n}").into_bytes();
            let chunk = ContentChunk::new(payload).expect("valid content chunk");
            let addr = *chunk.address();
            // bucket_for_address with bucket_depth 1 == top bit of byte 0.
            if addr.as_slice()[0] & 0x80 == 0 {
                return (chunk.into(), addr);
            }
        }
        panic!("no bucket-0 content chunk found within the search bound");
    }

    /// Sign a real stamp for `address` under `batch` at `timestamp`, at the given
    /// within-bucket `index`, with the bucket derived from the address so
    /// `validate_bucket` passes.
    fn signed_stamp(
        signer: &PrivateKeySigner,
        batch: &Batch,
        address: &ChunkAddress,
        index: u32,
        timestamp: u64,
    ) -> Stamp {
        let bucket = batch.bucket_for_address(address);
        let stamp_index = StampIndex::new(bucket, index);
        let digest = StampDigest::new(*address, batch.id(), stamp_index, timestamp);
        let sig = signer
            .sign_hash_sync(&alloy_primitives::eip191_hash_message(
                digest.to_prehash().as_slice(),
            ))
            .expect("sign");
        Stamp::with_index(batch.id(), stamp_index, timestamp, sig)
    }

    /// A test reserve and its shared database, plus the batch store the
    /// validate-on-ingest path reads.
    ///
    /// The reserve owns its own `DbBatchStore` (the `BatchStore` trait is
    /// implemented on the store, not on `Arc<store>`), and the fixture holds a
    /// second store over the *same* database for populating and reading batches:
    /// both see identical persisted state.
    struct Fixture {
        reserve: DbReserve<RedbDatabase, DbBatchStore<RedbDatabase>>,
        db: Arc<RedbDatabase>,
        batches: DbBatchStore<RedbDatabase>,
        overlay: OverlayAddress,
        signer: PrivateKeySigner,
    }

    impl Fixture {
        /// Build a reserve over a fresh in-memory database with a single batch
        /// already registered and the live context persisted.
        fn new() -> Self {
            Self::with_batches(&[B256::repeat_byte(0x11)])
        }

        /// Build a reserve whose batch store already holds the given batch ids
        /// (all owned by the shared signer), with the live context persisted.
        fn with_batches(batch_ids: &[B256]) -> Self {
            let db = RedbDatabase::in_memory().unwrap().into_arc();
            let batches = DbBatchStore::new(Arc::clone(&db)).unwrap();
            let signer = signer();
            let owner = signer.address();
            for id in batch_ids {
                batches.put(batch_for(owner, *id)).unwrap();
            }
            batches.set_context(live_context()).unwrap();

            let identity = MockIdentity::with_first_byte(0x00);
            let overlay = identity.overlay_address();
            // The reserve's own store handle over the same shared database.
            let reserve_batches = DbBatchStore::new(Arc::clone(&db)).unwrap();
            let reserve = DbReserve::new(
                Arc::clone(&db),
                &identity,
                reserve_batches,
                AdmissionValidator::new(THRESHOLD),
                10_000,
                EvictionStrategy::NoEviction,
                StorageRadius::ZERO,
            )
            .unwrap();
            Self {
                reserve,
                db,
                batches,
                overlay,
                signer,
            }
        }

        /// The single batch id this fixture was built with.
        fn batch_id(&self) -> BatchId {
            BatchId::repeat_byte(0x11)
        }

        /// Stamp `chunk` under `batch_id` at `(index, timestamp)` and put it.
        fn put(
            &self,
            chunk: &nectar_primitives::AnyChunk,
            addr: &ChunkAddress,
            batch_id: BatchId,
            index: u32,
            timestamp: u64,
        ) -> SwarmResult<()> {
            let batch = self.batches.get(&batch_id).unwrap().expect("batch present");
            let stamp = signed_stamp(&self.signer, &batch, addr, index, timestamp);
            self.reserve
                .put(CachedChunk::new(chunk.clone(), Some(stamp)))
        }

        /// Count rows in a table via a full cursor walk.
        fn row_count<T: Table>(&self) -> u64 {
            self.db.view(|tx| tx.count::<T>()).unwrap() as u64
        }

        /// The refcount stored for an address's payload, or `None` if absent.
        fn payload_refcnt(&self, addr: &ChunkAddress) -> Option<u64> {
            self.db
                .view(|tx| Ok(tx.get::<Payload>(*addr)?.map(|p| p.refcnt)))
                .unwrap()
        }
    }

    #[test]
    fn reserve_size_counts_stamped_entries_not_addresses() {
        // Same content address stamped under N distinct batches must leave the
        // reserve size == N (one Entry row per (batchID, stampIndex, address)),
        // NOT 1. The reserve size feeds storage_radius / committedDepth, which is
        // consensus-committed.
        let ids = [
            B256::repeat_byte(0x11),
            B256::repeat_byte(0x22),
            B256::repeat_byte(0x33),
        ];
        let fx = Fixture::with_batches(&ids);
        let (chunk, addr) = content_chunk_in_bucket0(1);

        for (i, id) in ids.iter().enumerate() {
            fx.put(&chunk, &addr, *id, 0, 100 + i as u64).unwrap();
        }

        assert_eq!(
            fx.reserve.count().unwrap(),
            ids.len() as u64,
            "size counts distinct stamped entries, not the single content address"
        );
        assert_eq!(
            fx.row_count::<Entry>(),
            ids.len() as u64,
            "one Entry row per stamped entry"
        );
        // One shared, refcounted payload: N entries, one body, refcnt == N.
        assert_eq!(fx.row_count::<Payload>(), 1, "one shared content payload");
        assert_eq!(fx.payload_refcnt(&addr), Some(ids.len() as u64));
    }

    #[test]
    fn newest_timestamp_wins_full_index_keying() {
        // Two stamps for the SAME (batchID, full 8-byte stampIndex): a newer
        // timestamp displaces the older entry (size stays 1, slot updated); an
        // EQUAL timestamp REJECTS (prev >= curr); an OLDER timestamp REJECTS.
        // Distinct indices in the same bucket are different slots and both admit
        // (the gate-refuted bucket-only keying must NOT collapse them).
        let fx = Fixture::new();
        let id = fx.batch_id();
        let (chunk, addr) = content_chunk_in_bucket0(2);

        // First stamp at index 0, timestamp 100.
        fx.put(&chunk, &addr, id, 0, 100).unwrap();
        assert_eq!(fx.reserve.count().unwrap(), 1);

        // Newer timestamp on the SAME slot: restamp, size unchanged.
        fx.put(&chunk, &addr, id, 0, 200).unwrap();
        assert_eq!(
            fx.reserve.count().unwrap(),
            1,
            "restamp displaces the old entry; size unchanged"
        );
        // The surviving entry carries the newer stamp (timestamp 200).
        let got = fx.reserve.get(&addr).unwrap().expect("present");
        assert_eq!(got.stamp().expect("stamped").timestamp(), 200);

        // Equal timestamp on the same slot: REJECT, nothing changes.
        fx.put(&chunk, &addr, id, 0, 200).unwrap();
        assert_eq!(fx.reserve.count().unwrap(), 1);
        assert_eq!(
            fx.reserve
                .get(&addr)
                .unwrap()
                .unwrap()
                .stamp()
                .unwrap()
                .timestamp(),
            200,
            "equal-timestamp re-presentation does not overwrite"
        );

        // Older timestamp on the same slot: REJECT.
        fx.put(&chunk, &addr, id, 0, 150).unwrap();
        assert_eq!(
            fx.reserve
                .get(&addr)
                .unwrap()
                .unwrap()
                .stamp()
                .unwrap()
                .timestamp(),
            200,
            "older stamp is stale and rejected"
        );

        // A DISTINCT index in the same bucket is a different slot: it admits and
        // adds a second entry (bucket-only keying would wrongly collapse these).
        fx.put(&chunk, &addr, id, 1, 50).unwrap();
        assert_eq!(
            fx.reserve.count().unwrap(),
            2,
            "distinct stampIndex in the same bucket is a separate slot"
        );
    }

    #[test]
    fn refcounted_payload_survives_partial_eviction() {
        // Same content under two batches: one Payload row, refcnt 2. The
        // second-batch put must NOT rewrite the body (refcnt bump only). Evicting
        // one entry leaves the body present (refcnt 1) and the surviving entry
        // still resolves.
        let ids = [B256::repeat_byte(0x11), B256::repeat_byte(0x22)];
        let fx = Fixture::with_batches(&ids);
        let (chunk, addr) = content_chunk_in_bucket0(3);

        fx.put(&chunk, &addr, ids[0], 0, 100).unwrap();
        let body_after_first = fx
            .db
            .view(|tx| Ok(tx.get::<Payload>(addr)?.map(|p| p.typed_bytes)))
            .unwrap()
            .expect("payload present");

        fx.put(&chunk, &addr, ids[1], 0, 100).unwrap();
        assert_eq!(fx.row_count::<Payload>(), 1, "one shared payload");
        assert_eq!(
            fx.payload_refcnt(&addr),
            Some(2),
            "second batch bumps refcnt"
        );
        let body_after_second = fx
            .db
            .view(|tx| Ok(tx.get::<Payload>(addr)?.map(|p| p.typed_bytes)))
            .unwrap()
            .expect("payload present");
        assert_eq!(
            body_after_first, body_after_second,
            "second batch must not rewrite the shared body"
        );
        assert_eq!(fx.reserve.count().unwrap(), 2, "two stamped entries");

        // Evict the furthest entry: one entry goes, the body survives (refcnt 1).
        let evicted = fx.reserve.evict_furthest().unwrap();
        assert_eq!(evicted, Some(addr));
        assert_eq!(fx.reserve.count().unwrap(), 1, "one entry removed");
        assert_eq!(
            fx.payload_refcnt(&addr),
            Some(1),
            "shared body survives partial eviction"
        );
        // The surviving entry still resolves to the chunk.
        let got = fx.reserve.get(&addr).unwrap().expect("survivor present");
        assert_eq!(got.address(), &addr);

        // Evicting the last entry drops the body entirely (no tombstone).
        fx.reserve.evict_furthest().unwrap();
        assert_eq!(fx.reserve.count().unwrap(), 0);
        assert_eq!(fx.payload_refcnt(&addr), None, "last entry drops the body");
    }

    #[test]
    fn restamp_to_different_address_cleans_displaced_rows() {
        // A restamp re-points the (batchID, stampIndex) slot at a DIFFERENT
        // content address. The displaced entry's rows must be deleted using the
        // DISPLACED address's proximity (regression guard for the orphaned-row /
        // leaked-refcount bug), and its payload refcount decremented exactly once.
        let fx = Fixture::new();
        let id = fx.batch_id();
        // Two distinct addresses, both in bucket 0, so they share index slot 0.
        let (chunk_a, addr_a) = content_chunk_in_bucket0(10);
        let (chunk_b, addr_b) = content_chunk_in_bucket0(20);
        assert_ne!(addr_a, addr_b, "fixtures must be distinct addresses");

        // Stamp A into slot (id, index 0) at timestamp 100.
        fx.put(&chunk_a, &addr_a, id, 0, 100).unwrap();
        assert_eq!(fx.reserve.count().unwrap(), 1);
        assert!(fx.reserve.contains(&addr_a));

        // Restamp the SAME slot onto address B at a newer timestamp: A is
        // displaced, B admitted. Size unchanged (one displaced, one added).
        fx.put(&chunk_b, &addr_b, id, 0, 200).unwrap();
        assert_eq!(
            fx.reserve.count().unwrap(),
            1,
            "restamp to a different address displaces A and admits B"
        );

        // A's body and all its index rows are gone (no orphaned rows, no leaked
        // refcount); B is present and resolves.
        assert!(!fx.reserve.contains(&addr_a), "displaced A body removed");
        assert_eq!(fx.payload_refcnt(&addr_a), None, "A refcount not leaked");
        assert!(fx.reserve.contains(&addr_b), "B present");
        assert_eq!(fx.payload_refcnt(&addr_b), Some(1));

        // Exactly one of each index row remains (B's), no A residue.
        assert_eq!(fx.row_count::<Entry>(), 1, "one Entry row (B)");
        assert_eq!(fx.row_count::<BatchGroup>(), 1, "one BatchGroup row (B)");
        assert_eq!(fx.row_count::<Replay>(), 1, "one Replay row (B)");
        assert_eq!(fx.row_count::<Payload>(), 1, "one Payload row (B)");
    }

    #[test]
    fn get_returns_exact_admitting_stamp() {
        // get() must surface the PRECISE stamp the entry was admitted with
        // (stored per entry), byte-for-byte, not a stamp re-loaded by batchID
        // alone. An inclusion proof carries exactly this stamp.
        let fx = Fixture::new();
        let id = fx.batch_id();
        let (chunk, addr) = content_chunk_in_bucket0(4);

        let batch = fx.batches.get(&id).unwrap().unwrap();
        let stamp = signed_stamp(&fx.signer, &batch, &addr, 0, 12_345);
        let expected_bytes = stamp.to_bytes();
        fx.reserve
            .put(CachedChunk::new(chunk.clone(), Some(stamp.clone())))
            .unwrap();

        let got = fx.reserve.get(&addr).unwrap().expect("present");
        let got_stamp = got.stamp().expect("stamped");
        assert_eq!(got_stamp.timestamp(), 12_345);
        assert_eq!(
            got_stamp.to_bytes(),
            expected_bytes,
            "the exact admitting stamp bytes are surfaced"
        );
        assert_eq!(got.chunk(), &chunk);
    }

    #[test]
    fn removal_fully_compacts_all_tables_no_tombstones() {
        // remove() must delete every row of every stamped entry for an address
        // across all six tables, leaving no tombstone. Store the same content
        // under two batches (two entries, one shared payload), then remove and
        // assert all tables are empty.
        let ids = [B256::repeat_byte(0x11), B256::repeat_byte(0x22)];
        let fx = Fixture::with_batches(&ids);
        let (chunk, addr) = content_chunk_in_bucket0(5);

        fx.put(&chunk, &addr, ids[0], 0, 100).unwrap();
        fx.put(&chunk, &addr, ids[1], 0, 100).unwrap();
        assert_eq!(fx.reserve.count().unwrap(), 2);

        fx.reserve.remove(&addr).unwrap();

        assert_eq!(fx.reserve.count().unwrap(), 0);
        assert!(!fx.reserve.contains(&addr));
        assert_eq!(fx.row_count::<Entry>(), 0, "no Entry tombstones");
        assert_eq!(fx.row_count::<BatchGroup>(), 0, "no BatchGroup tombstones");
        assert_eq!(fx.row_count::<Replay>(), 0, "no Replay tombstones");
        assert_eq!(fx.row_count::<Payload>(), 0, "no Payload tombstones");
        // The arbiter slots for both batches are cleared, so a later older stamp
        // is admitted afresh (slot did not pin a stale newest).
        let slot0 = StampSlotKey::new(BatchId::from(ids[0]), StampIndex::new(0, 0));
        assert!(
            fx.db
                .view(|tx| tx.get::<StampIndexTable>(slot0))
                .unwrap()
                .is_none(),
            "arbiter slot cleared on full removal"
        );
    }

    #[test]
    fn unknown_batch_and_stampless_puts_are_rejected() {
        // A stamp referencing a batch the node does not know is refused, and a
        // stampless put is invalid; neither writes anything.
        let fx = Fixture::new();
        let (chunk, addr) = content_chunk_in_bucket0(6);

        // Stampless.
        let err = fx
            .reserve
            .put(CachedChunk::new(chunk.clone(), None))
            .unwrap_err();
        assert!(matches!(err, SwarmError::InvalidChunk { .. }));
        assert!(!fx.reserve.contains(&addr));

        // Unknown batch: sign under a batch id the store does not hold.
        let unknown = Batch::new(
            B256::repeat_byte(0x99),
            1_000_000,
            0,
            fx.signer.address(),
            DEPTH,
            BUCKET_DEPTH,
            false,
        );
        let stamp = signed_stamp(&fx.signer, &unknown, &addr, 0, 100);
        let err = fx
            .reserve
            .put(CachedChunk::new(chunk, Some(stamp)))
            .unwrap_err();
        assert!(matches!(err, SwarmError::InvalidChunk { .. }));
        assert_eq!(fx.reserve.count().unwrap(), 0);
    }

    #[test]
    fn bin_scan_replays_entries_in_insertion_order() {
        // The Replay table feeds the redistribution/sync consumer in per-bin
        // insertion order, surfacing the precise (address, batch, stampHash) of
        // each stamped entry without a body read.
        let ids = [B256::repeat_byte(0x11), B256::repeat_byte(0x22)];
        let fx = Fixture::with_batches(&ids);
        let (chunk, addr) = content_chunk_in_bucket0(7);
        let bin = addr.bin(&fx.overlay);

        fx.put(&chunk, &addr, ids[0], 0, 100).unwrap();
        fx.put(&chunk, &addr, ids[1], 0, 100).unwrap();

        let items: Vec<_> = fx
            .reserve
            .scan_bin_from(bin, 0)
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(items.len(), 2, "two entries replayed");
        assert!(
            items[0].seq < items[1].seq,
            "replayed ascending by insertion sequence"
        );
        assert!(items.iter().all(|i| i.address == addr));
    }

    // sample-at-most-once (a chunk under N batches contributes at most one slot to
    // a sample, equal transformed address collapses, CAC beats SOC) is a SAMPLER
    // property, not a reserve one: the reserve exposes the per-entry Replay
    // projection (address, batch, stampHash, chunk_type) the sampler consumes, but
    // the collapse is performed by the sampler (PR-F), not here. The reserve-side
    // guarantee that the projection is per-entry and carries the chunk type is
    // covered by bin_scan_replays_entries_in_insertion_order plus the ReplayValue
    // schema; the collapse itself is asserted in PR-F.
    #[test]
    #[ignore = "sample-at-most-once collapse is a PR-F sampler property; reserve only supplies the per-entry Replay projection"]
    fn sample_collapses_duplicate_transformed_address() {
        // Intentionally deferred to PR-F (sampler). See the comment above: the
        // reserve's responsibility (a per-entry, chunk-typed Replay projection) is
        // covered by the bin-scan test; the at-most-once collapse over transformed
        // addresses belongs to the sampler that consumes that projection.
    }
}
