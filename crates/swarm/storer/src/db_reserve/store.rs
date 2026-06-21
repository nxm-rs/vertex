//! The [`DbReserve`] store: construction, accessors, and the storage lattice
//! impls (`SwarmLocalStore`, `ReserveStore`, `BinCursorStore`) plus the lazy
//! bin-scan iterator.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

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
    BatchGroup, BatchGroupKey, BinCounter, BinKey, EPOCH_KEY, Entry, EntryKey, EntryValue,
    Payload, Replay, ReplayKey, ReplayValue, ReserveMetadata, stamp_hash,
};
use super::tx::{
    PutOutcome, bump_or_insert_payload, decode_body, decode_stamp, delete_entry_in_tx,
    delete_entry_rows_in_tx, po_of_reserve_bin, stamp_timestamp, storage_err,
};

/// Persisting, proximity-ordered, per-stamped-entry reserve.
///
/// Implements the storage lattice [`SwarmLocalStore`] -> [`ReserveStore`] ->
/// [`BinCursorStore`]. The payload and all index rows commit in one transaction
/// per operation via the shared database handle.
pub struct DbReserve<DB: Database, BS: BatchStore> {
    db: Arc<DB>,
    /// Validate-on-ingest admission for stamped chunks.
    admission: AdmissionValidator,
    /// Loads the batch a stamp references, and the live [`PostageContext`].
    batches: BS,
    /// In-memory size counter, kept in step with the authoritative [`Entry`]
    /// table count for cheap reads.
    reserve: Reserve,
    overlay: OverlayAddress,
    /// Current storage-responsibility radius (a single `0..=MAX_PO` byte).
    ///
    /// `Relaxed` suffices: the radius carries no happens-before relationship
    /// with other reserve state, so callers needing a consistent
    /// (radius, occupancy) pair re-derive the radius from the occupancy.
    radius: AtomicU8,
    /// Stable instance identifier, loaded once at open and cached for cheap
    /// reads. Changes only when the reserve is recreated; a pull-syncer
    /// comparing this across calls detects a wipe and invalidates cached cursors.
    epoch: u64,
}

impl<DB: Database, BS: BatchStore> DbReserve<DB, BS> {
    /// Construct a per-entry reserve over a shared database, ensuring all six
    /// tables exist and initialising the in-memory size from the persisted
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
        let epoch = db.update(|tx| {
            tx.ensure_table(Payload::NAME)?;
            tx.ensure_table(Entry::NAME)?;
            tx.ensure_table(BatchGroup::NAME)?;
            tx.ensure_table(Replay::NAME)?;
            tx.ensure_table(BinCounter::NAME)?;
            tx.ensure_table(StampIndexTable::NAME)?;
            tx.ensure_table(ReserveMetadata::NAME)?;
            // Load the persisted epoch or generate and persist one on first open.
            if let Some(e) = tx.get::<ReserveMetadata>(EPOCH_KEY)? {
                Ok(e)
            } else {
                let e = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as u64;
                tx.put::<ReserveMetadata>(EPOCH_KEY, e)?;
                Ok(e)
            }
        })?;

        let overlay = identity.overlay_address();
        let reserve = Reserve::with_strategy(capacity, strategy).with_identity(identity);
        let size = db.view(|tx| tx.count::<Entry>())? as u64;
        reserve.set_count(size);

        Ok(Self {
            db,
            admission,
            batches,
            reserve,
            overlay,
            radius: AtomicU8::new(radius.get()),
            epoch,
        })
    }

    /// Read the radius cell back into a [`StorageRadius`]. The cell only ever
    /// holds a valid `0..=MAX_PO` bin, so the fallback is unreachable but keeps
    /// the read infallible.
    fn load_radius(&self) -> StorageRadius {
        let raw = self.radius.load(Ordering::Relaxed);
        Bin::try_from(raw)
            .map(StorageRadius::new)
            .unwrap_or(StorageRadius::ZERO)
    }

    /// Apply a new storage-responsibility radius (a single relaxed atomic
    /// store). The controller sheds bins via the eviction verbs before
    /// narrowing responsibility, so this moves no chunk and never fails. Also
    /// exposed via the [`SettableRadius`](vertex_swarm_api::SettableRadius)
    /// trait for generic callers.
    pub fn set_storage_radius(&self, radius: StorageRadius) {
        self.radius.store(radius.get(), Ordering::Relaxed);
    }

    /// Stable instance identifier captured once at reserve creation.
    ///
    /// A pull-syncer compares this value across cursor handshakes to detect
    /// that the reserve was wiped and recreated, invalidating its cached
    /// per-bin cursors.
    pub fn reserve_epoch(&self) -> u64 {
        self.epoch
    }

    fn overlay(&self) -> OverlayAddress {
        self.overlay
    }

    fn po_of(&self, address: &ChunkAddress) -> u8 {
        address.proximity(&self.overlay).get()
    }

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

/// Batch-store reads on the synchronous `put` path. The typed
/// [`BatchStore::Error`] maps into [`SwarmError::storage`] (preserved as the
/// diagnostic source), which requires the `Send + Sync + 'static` bound on the
/// associated error.
impl<DB: Database, BS: BatchStore> DbReserve<DB, BS>
where
    BS::Error: Send + Sync + 'static,
{
    /// Load the batch a stamp references, returning `None` if unknown.
    fn load_batch(&self, batch_id: &BatchId) -> SwarmResult<Option<nectar_postage::Batch>> {
        self.batches.get(batch_id).map_err(SwarmError::storage)
    }

    fn context(&self) -> SwarmResult<PostageContext> {
        self.batches.context().map_err(SwarmError::storage)
    }
}

// `SwarmLocalStore: Send + Sync` requires the store to be shareable across the
// node's async tasks, hence the `BS: Send + Sync` bound. The `BS::Error` bound
// lets the typed batch-store error map into `SwarmError::storage` on `put`.
impl<DB: Database, BS: BatchStore + Send + Sync> SwarmLocalStore for DbReserve<DB, BS>
where
    BS::Error: Send + Sync + 'static,
{
    fn put(&self, chunk: CachedChunk) -> SwarmResult<()> {
        let address = *chunk.address();
        let (any, stamp) = chunk.into_parts();
        let stamp = stamp.ok_or_else(|| SwarmError::InvalidChunk {
            address: Some(address),
            reason: "reserve put requires a stamp; a stampless put is invalid".into(),
        })?;

        // Validate the stamp against its batch before any write. An unknown
        // batch or a failed structural/signature check rejects without writing.
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

        let outcome = self
            .db
            .update(|tx| {
                // Newest-wins arbitration against the full (batch, stampIndex)
                // slot, inside the tx so the slot cannot change between the read
                // and the conditional write.
                let stored = tx.get::<StampIndexTable>(slot)?;
                match decide(stored.as_ref(), &incoming) {
                    Arbitration::Reject { .. } => Ok(PutOutcome::Rejected),
                    Arbitration::Admit { displaced } => {
                        if let Some(d) = displaced {
                            let target = EvictTarget {
                                batch: incoming.batch_id,
                                stamp_hash: d.stamp_hash,
                                addr: d.address,
                            };
                            // A restamp may re-point the slot at a DIFFERENT
                            // content address whose proximity order differs from
                            // the incoming chunk's. Delete the displaced rows
                            // under the displaced address's own proximity, or the
                            // lookup misses and the rows leak a refcount.
                            let displaced_po = self.po_of(&d.address);
                            let _ = delete_entry_rows_in_tx(tx, displaced_po, &target)?;
                        }

                        // BinCounter is monotonic and never rewound on eviction,
                        // so its value is the count of entries ever admitted to
                        // this bin. A silent wrap would reuse sequence 0 and
                        // overwrite a live Replay row, corrupting sync cursor
                        // ordering, so overflow is a hard error (unreachable: a
                        // u64 outlasts any node lifetime).
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

                        bump_or_insert_payload(tx, address, &typed_bytes)?;

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
            // Restamp is size-neutral (one displaced, one added); reject writes
            // nothing.
            PutOutcome::Admitted => self.reserve.on_added(),
            PutOutcome::Restamped | PutOutcome::Rejected => {}
        }
        Ok(())
    }

    fn get(&self, address: &ChunkAddress) -> SwarmResult<Option<CachedChunk>> {
        // Resolve the chunk with its newest stamp (greatest timestamp). The body
        // is the refcounted payload; stamps are per entry. An address under N
        // batches has up to N entries; per-entry access is via the bin scan.
        let result = self.db.view(|tx| {
            let Some(payload) = tx.get::<Payload>(*address)? else {
                return Ok(None);
            };
            // The address pins the po, so its entries share one po prefix and
            // differ by (batch, stampHash). Walk that contiguous sub-range.
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
            // Payload without an entry violates the refcount invariant; treat as
            // absent.
            return Ok(None);
        };
        let stamp = decode_stamp(&stamp_bytes).map_err(|e| SwarmError::InvalidChunk {
            address: Some(*address),
            reason: format!("stored entry stamp failed to decode: {e}"),
        })?;
        let stamped = StampedChunk::new(decode_body(address, &typed_bytes)?, stamp);
        Ok(Some(CachedChunk::from(stamped)))
    }

    fn contains(&self, address: &ChunkAddress) -> bool {
        // Present iff a payload row exists. A backend error reads as "not
        // present" per the infallible contract.
        self.db
            .view(|tx| Ok(tx.get::<Payload>(*address)?.is_some()))
            .unwrap_or(false)
    }

    fn remove(&self, address: &ChunkAddress) -> SwarmResult<()> {
        // Remove every stamped entry for the address and the shared payload.
        let targets = self.entries_for_address(address)?;
        let _ = self.evict_entries(&targets)?;
        Ok(())
    }
}

impl<DB: Database, BS: BatchStore> DbReserve<DB, BS> {
    /// Collect every stamped entry for an address (its `(batch, stampHash)`
    /// pairs). The address pins the proximity order, so the entries are a
    /// contiguous sub-range of the `po` prefix.
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
        Ok(self
            .db
            .view(|tx| tx.count::<Entry>())
            .map_err(storage_err)? as u64)
    }

    fn capacity(&self) -> u64 {
        self.reserve.capacity()
    }

    fn count_in(&self, po: ProximityOrder) -> SwarmResult<u64> {
        // po is the leading Entry key byte, so entries at this po are contiguous:
        // seek and count forward while po stays equal.
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
        // The furthest entry has the smallest po; the Entry table is keyed
        // `[po][...]`, so `first()` is that entry. Per-entry eviction: one goes.
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
        // Collect up to `max` entries in this bin via the Entry `[po]` prefix,
        // then delete in one atomic tx. For the reserve a `Bin` is the proximity
        // order, so cross the boundary explicitly rather than punning the byte.
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
        // `[batch][po][addr][stampHash]` prefix, then delete in one atomic tx. A
        // `Some(b)` bound selects bins strictly shallower than `b`, a contiguous
        // front slice: stop as soon as po >= b. The bound is a reserve `Bin`;
        // cross to the keyed proximity order explicitly.
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
        // Seek to `(bin, start_seq)` and stream forward while the key's bin stays
        // equal. The cursor owns its read snapshot, so the iterator outlives this
        // call.
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test assertions over known-bounds fixtures"
)]
mod epoch_tests {
    use std::sync::Arc;

    use vertex_storage_redb::RedbDatabase;
    use vertex_swarm_postage::{AdmissionValidator, DbBatchStore};
    use vertex_swarm_primitives::StorageRadius;
    use vertex_swarm_test_utils::MockIdentity;

    use crate::EvictionStrategy;

    use super::DbReserve;

    fn open_reserve(db: Arc<RedbDatabase>) -> DbReserve<RedbDatabase, DbBatchStore<RedbDatabase>> {
        let identity = MockIdentity::with_first_byte(0x00);
        let batches = DbBatchStore::new(Arc::clone(&db)).unwrap();
        DbReserve::new(
            db,
            &identity,
            batches,
            AdmissionValidator::new(8),
            10_000,
            EvictionStrategy::NoEviction,
            StorageRadius::ZERO,
        )
        .unwrap()
    }

    #[test]
    fn epoch_is_nonzero() {
        let db = RedbDatabase::in_memory().unwrap().into_arc();
        let reserve = open_reserve(db);
        assert_ne!(reserve.reserve_epoch(), 0);
    }

    #[test]
    fn epoch_survives_reopen() {
        let db = RedbDatabase::in_memory().unwrap().into_arc();
        let epoch_first = open_reserve(Arc::clone(&db)).reserve_epoch();
        let epoch_second = open_reserve(Arc::clone(&db)).reserve_epoch();
        assert_eq!(epoch_first, epoch_second);
    }

    #[test]
    fn distinct_reserves_get_distinct_epochs() {
        // Two independent in-memory databases each generate their epoch from
        // the nanosecond wall clock. Nanosecond resolution on real hardware
        // makes collision negligible (one in ~584 years).
        let epoch_a = open_reserve(RedbDatabase::in_memory().unwrap().into_arc()).reserve_epoch();
        let epoch_b = open_reserve(RedbDatabase::in_memory().unwrap().into_arc()).reserve_epoch();
        assert_ne!(epoch_a, epoch_b);
    }
}
