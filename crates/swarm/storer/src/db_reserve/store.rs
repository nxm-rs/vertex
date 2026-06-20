//! The [`DbReserve`] store: its construction and accessors, and the storage
//! lattice implementations (`SwarmLocalStore`, `ReserveStore`, `BinCursorStore`)
//! plus the lazy bin-scan iterator.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use alloy_primitives::B256;
use nectar_primitives::{Bin, ChunkAddress, ProximityOrder};
use tracing::debug;
use vertex_storage::{Database, DatabaseError, DbCursorRO, DbTx, DbTxMut, Table};
use vertex_swarm_api::{
    BinCursorStore, BinScanItem, ReserveStore, SettableRadius, SwarmError, SwarmLocalStore,
    SwarmResult,
};
use vertex_swarm_postage::{
    AdmissionValidator, Arbitration, BatchStore, IncomingStamp, PostageContext, StampIndexTable,
    StampSlotKey, decide,
};
use vertex_swarm_primitives::{BatchId, CachedChunk, OverlayAddress, StampedChunk, StorageRadius};

use crate::{EvictionStrategy, Reserve, StorerError};

use super::EvictTarget;
use super::schema::{
    BatchGroup, BatchGroupKey, BinCounter, BinKey, Entry, EntryKey, EntryValue, Payload, Replay,
    ReplayKey, ReplayValue, stamp_hash,
};
use super::tx::{
    PutOutcome, bump_or_insert_payload, decode_body, decode_stamp, delete_entry_in_tx,
    delete_entry_rows_in_tx, po_of_reserve_bin, stamp_timestamp, storage_err,
};

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
    /// Current storage-responsibility radius, behind a cheap lock-free cell.
    ///
    /// The radius is the consensus-load-bearing output of the size-driven
    /// dynamics ([`crate::radius`]): the [`RadiusController`](crate::RadiusController)
    /// applies a new value via [`set_storage_radius`](Self::set_storage_radius)
    /// as the reserve fills or drains, and [`storage_radius`](ReserveStore::storage_radius)
    /// reads it. A single byte (`0..=MAX_PO`) is all the radius ever is, so an
    /// [`AtomicU8`] is both the cheapest write target and a lock-free read on the
    /// hot `is_responsible_for` path; `Relaxed` ordering suffices because the
    /// radius carries no happens-before relationship with other reserve state
    /// (callers that need a consistent (radius, occupancy) pair re-derive the
    /// radius from the occupancy themselves).
    radius: AtomicU8,
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
            radius: AtomicU8::new(radius.get()),
        })
    }

    /// Read the current radius cell back into a [`StorageRadius`].
    ///
    /// The cell only ever holds a value written from a [`StorageRadius`] (a valid
    /// `0..=MAX_PO` bin), so the `Bin::try_from` reconstruction never fails; the
    /// `unwrap_or(StorageRadius::ZERO)` is a total fallback that keeps the read
    /// infallible rather than panicking on a value the cell cannot hold.
    fn load_radius(&self) -> StorageRadius {
        let raw = self.radius.load(Ordering::Relaxed);
        Bin::try_from(raw)
            .map(StorageRadius::new)
            .unwrap_or(StorageRadius::ZERO)
    }

    /// Apply a new storage-responsibility radius.
    ///
    /// This is the write seam the size-driven dynamics ([`crate::radius`]) feed:
    /// the [`RadiusController`](crate::RadiusController) derives the radius from
    /// the reserve's occupancy against its capacity and calls this to commit it,
    /// after which [`storage_radius`](ReserveStore::storage_radius) and
    /// [`is_responsible_for`](ReserveStore::is_responsible_for) observe the new
    /// value. Committing the radius is a single relaxed atomic store; it does not
    /// itself move any chunk (the controller sheds bins through the eviction
    /// verbs before narrowing responsibility), so it never blocks or fails.
    ///
    /// Exposed both as this inherent method and through the
    /// [`SettableRadius`](vertex_swarm_api::SettableRadius) extension trait so the
    /// control loop can target it either concretely or generically over a
    /// [`ReserveStore`].
    pub fn set_storage_radius(&self, radius: StorageRadius) {
        self.radius.store(radius.get(), Ordering::Relaxed);
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

                        // Assign the next bin sequence and write Replay. The
                        // counter is monotonic and never rewound on eviction, so
                        // its value is the count of entries ever admitted to this
                        // bin. Exhausting a u64 requires more than 1.8e19 puts
                        // into a single bin, which is unreachable in any node
                        // lifetime (over half a million years at one million puts
                        // per second). A silent wrap would reuse sequence 0 and
                        // overwrite a live Replay row, corrupting sync cursor
                        // ordering and resumability, so the overflow is surfaced
                        // as a hard error rather than wrapped.
                        let next = tx
                            .get::<BinCounter>(BinKey::from_bin(bin))?
                            .unwrap_or(0)
                            .checked_add(1)
                            .ok_or_else(|| {
                                DatabaseError::other(format!(
                                    "bin counter for bin {} exhausted u64::MAX",
                                    bin.get()
                                ))
                            })?;
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
        self.load_radius()
    }

    fn is_responsible_for(&self, address: &ChunkAddress) -> bool {
        address.proximity(&self.overlay()).get() >= self.radius.load(Ordering::Relaxed)
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

impl<DB: Database, BS: BatchStore + Send + Sync> SettableRadius for DbReserve<DB, BS>
where
    BS::Error: Send + Sync + 'static,
{
    fn set_storage_radius(&self, radius: StorageRadius) {
        // Delegates to the inherent method (the same write the controller's apply
        // path takes); the trait exposes that write generically over a reserve.
        DbReserve::set_storage_radius(self, radius);
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
