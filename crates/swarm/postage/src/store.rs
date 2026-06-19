//! Persisting batch store over the `vertex-storage` `Database`, and the
//! [`BatchEventHandler`] ingest seam.
//!
//! [`DbBatchStore`] is generic over the storage backend, so the same code
//! serves an in-memory database (tests) and an on-disk redb database
//! (production). It defines two tables:
//!
//! - [`Batches`]: `BatchId -> Batch`, the authoritative batch set.
//! - [`ContextTable`]: a single-row table holding the current
//!   [`PostageContext`]. redb has no schema-level singleton, so the singleton is
//!   modelled as a one-key table under a fixed key.
//!
//! Every method is a single transaction. The [`BatchStore`] trait is
//! synchronous (redb is sync), so each method is a plain function and no redb
//! transaction guard is ever held across an `await` point; async, where it is
//! needed at all, is added at the true edges (gRPC, FFI), not by the store.

use nectar_postage::{Batch, BatchEvent, BatchEventHandler, BatchId, BatchStore, PostageContext};
use std::sync::Arc;
use vertex_storage::{Database, DatabaseError, DbTx, DbTxMut, Decode, Encode, Table, table};

// Batch table: `BatchId -> Batch`.
//
// The authoritative set of batches the node knows about. Values are compressed
// (the default): `Batch` serialises to a small but compressible record.
table!(pub(crate) Batches, "postage_batches", BatchIdKey, Batch);

// Context singleton table: fixed key -> `PostageContext`.
//
// redb offers no schema-level singleton, so the postage context is stored as a
// single row under [`ContextKey::SINGLETON`]. Uncompressed: the value is a tiny
// fixed record (a u64 and a u128).
table!(pub(crate) ContextTable, "postage_context", ContextKey, PostageContext, compressed = false);

/// Key newtype wrapping a [`BatchId`] for the [`Batches`] table.
///
/// [`BatchId`] is `alloy_primitives::B256`, a foreign type, so the
/// vertex-storage [`Encode`]/[`Decode`] codecs cannot be implemented on it
/// directly (orphan rule). This local newtype carries the 32-byte big-endian
/// encoding, which is also the natural byte order of the id.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub(crate) struct BatchIdKey(pub BatchId);

impl Encode for BatchIdKey {
    type Encoded = [u8; 32];

    fn encode(self) -> Self::Encoded {
        self.0.0
    }
}

impl Decode for BatchIdKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 32] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        Ok(Self(BatchId::from(bytes)))
    }
}

/// Key for the [`ContextTable`] singleton.
///
/// The context table holds exactly one row; this single-byte key is its address.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub(crate) struct ContextKey(u8);

impl ContextKey {
    /// The sole key under which the postage context is stored.
    const SINGLETON: Self = Self(0);
}

impl Encode for ContextKey {
    type Encoded = [u8; 1];

    fn encode(self) -> Self::Encoded {
        [self.0]
    }
}

impl Decode for ContextKey {
    fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
        let bytes: [u8; 1] = value.try_into().map_err(|_| DatabaseError::Decode)?;
        Ok(Self(bytes[0]))
    }
}

/// Error type returned by [`DbBatchStore`] operations.
///
/// Wraps the underlying [`DatabaseError`]; the [`BatchStore`] trait requires the
/// associated error to implement [`std::error::Error`], which this does (via
/// `thiserror`).
#[derive(Debug, thiserror::Error)]
pub enum DbBatchStoreError {
    /// An error from the underlying database.
    #[error("postage batch store database error: {0}")]
    Database(#[from] DatabaseError),
}

/// Batch store backed by the `vertex-storage` `Database` trait.
///
/// Generic over the backend, so persistence is decided by whichever database
/// the node opens. The store is cheap to clone-by-`Arc` and thread-safe for
/// concurrent reads and writes. It implements both [`BatchStore`] (persistence)
/// and [`BatchEventHandler`] (the on-chain ingest seam).
pub struct DbBatchStore<DB: Database> {
    db: Arc<DB>,
}

impl<DB: Database> DbBatchStore<DB> {
    /// Create a batch store over a shared database handle.
    ///
    /// Ensures both the batch table and the context singleton table exist, so
    /// every read path works on a fresh database without a separate
    /// initialisation step.
    pub fn new(db: Arc<DB>) -> Result<Self, DbBatchStoreError> {
        db.update(|tx| {
            tx.ensure_table(Batches::NAME)?;
            tx.ensure_table(ContextTable::NAME)?;
            Ok(())
        })?;
        Ok(Self { db })
    }

    /// Borrow the shared database handle.
    pub fn database(&self) -> &Arc<DB> {
        &self.db
    }

    /// Load a batch, apply `mutate` to it, and store it back, all inside a
    /// single read-write transaction.
    ///
    /// This is the atomic read-modify-write the `TopUp` and `DepthIncrease`
    /// events need: the load and the store share one transaction, so a
    /// concurrent writer cannot interleave between the read and the write and
    /// have its update silently clobbered (a lost update). A missing batch is a
    /// no-op (the event is for a batch the node never saw, or already removed),
    /// matching the idempotent ingest contract.
    ///
    /// `mutate` runs on the loaded value only and performs no I/O, so the
    /// transaction is held for the minimum span.
    fn mutate_sync(
        &self,
        id: &BatchId,
        mutate: impl FnOnce(&mut Batch),
    ) -> Result<(), DbBatchStoreError> {
        self.db.update(|tx| {
            if let Some(mut batch) = tx.get::<Batches>(BatchIdKey(*id))? {
                mutate(&mut batch);
                tx.put::<Batches>(BatchIdKey(*id), batch)?;
            }
            Ok(())
        })?;
        Ok(())
    }

    /// Remove an expired batch from the store, the *acknowledgement* half of the
    /// evict-before-remove expiry contract.
    ///
    /// This is the bare store removal: it drops the batch row and returns
    /// whether it existed. It performs **no** reserve eviction, because the
    /// reserve lives downstream of this crate. It must therefore be driven only
    /// as the acknowledgement of the reserve's evict-then-acknowledge entry
    /// point (`ExpirySweep::on_expired_event`), which evicts the batch's stamped
    /// entries first and runs this acknowledgement only once eviction has
    /// succeeded. Calling it standalone for a live `Expired` event orphans the
    /// reserve entries stamped under the batch; see the [`BatchEventHandler`]
    /// impl docs.
    ///
    /// Idempotent: acknowledging an already-removed batch returns `false` and is
    /// a harmless no-op, so a redelivered `Expired` event is safe.
    pub fn acknowledge_expired(&self, id: &BatchId) -> Result<bool, DbBatchStoreError> {
        self.remove(id)
    }
}

// The `BatchStore` trait is synchronous: redb is itself synchronous, so each
// method is a single transaction with no executor in sight. Async, where it is
// needed at all, is added at the true edges (a gRPC service, an FFI boundary),
// not here in the store.
impl<DB: Database> BatchStore for DbBatchStore<DB> {
    type Error = DbBatchStoreError;

    fn get(&self, id: &BatchId) -> Result<Option<Batch>, Self::Error> {
        Ok(self.db.view(|tx| tx.get::<Batches>(BatchIdKey(*id)))?)
    }

    fn put(&self, batch: Batch) -> Result<(), Self::Error> {
        let id = batch.id();
        self.db
            .update(|tx| tx.put::<Batches>(BatchIdKey(id), batch))?;
        Ok(())
    }

    fn remove(&self, id: &BatchId) -> Result<bool, Self::Error> {
        Ok(self.db.update(|tx| tx.delete::<Batches>(BatchIdKey(*id)))?)
    }

    fn contains(&self, id: &BatchId) -> Result<bool, Self::Error> {
        // No key-presence primitive on the transaction trait yet, so presence
        // is a full read whose value is discarded.
        // TODO(#214): switch to a key-presence check once available.
        Ok(self.get(id)?.is_some())
    }

    fn context(&self) -> Result<PostageContext, Self::Error> {
        self.db
            .view(|tx| tx.get::<ContextTable>(ContextKey::SINGLETON))
            .map(|opt| opt.unwrap_or_default())
            .map_err(DbBatchStoreError::from)
    }

    fn set_context(&self, state: PostageContext) -> Result<(), Self::Error> {
        self.db
            .update(|tx| tx.put::<ContextTable>(ContextKey::SINGLETON, state))
            .map_err(DbBatchStoreError::from)
    }

    fn batch_ids(&self) -> Result<Vec<BatchId>, Self::Error> {
        // Iterate the batch table via the lazy read-only cursor (#396): the
        // cursor owns its read snapshot, so it is detached from the borrowed
        // transaction handle. Only keys are needed; values are still decoded by
        // the cursor, but no per-batch second lookup is performed.
        let tx = self.db.tx()?;
        let mut cursor = tx.cursor::<Batches>()?;
        let mut ids = Vec::new();
        let mut entry = cursor.first()?;
        while let Some((key, _batch)) = entry {
            ids.push(key.0);
            entry = cursor.next()?;
        }
        Ok(ids)
    }

    fn count(&self) -> Result<usize, Self::Error> {
        self.db
            .view(|tx| tx.count::<Batches>())
            .map_err(DbBatchStoreError::from)
    }
}

/// The on-chain ingest seam.
///
/// An external `PostageIndexer` decodes contract logs into [`BatchEvent`]s and
/// drives this handler; see the crate-level documentation. The four variants
/// map onto store mutations:
///
/// - [`BatchEvent::Created`]: the batch is fully described by the event, so it
///   is written directly (`put`).
/// - [`BatchEvent::TopUp`]: only the new normalised value is on the wire, so the
///   batch is loaded, its value updated ([`Batch::set_value`]) and written back
///   inside one transaction ([`DbBatchStore::mutate_sync`]), so a concurrent
///   writer cannot clobber the update. A top-up for an unknown batch is a no-op
///   (the create was missed; the indexer is responsible for ordering, this
///   handler is idempotent).
/// - [`BatchEvent::DepthIncrease`]: as top-up, but updates the depth
///   ([`Batch::set_depth`]).
/// - [`BatchEvent::Expired`]: the batch is removed via
///   [`DbBatchStore::acknowledge_expired`].
///
/// Each mutation is a single transaction, so the store never observes a partial
/// event application. Both this handler and the [`BatchStore`] trait are
/// synchronous, so the handler reuses the store methods (and the `mutate_sync`
/// helper) directly, with no executor.
///
/// # `Expired` is the bare removal, not the orphan-safe path
///
/// Handling [`BatchEvent::Expired`] here removes the batch from the store and
/// nothing more. It does **not** evict the reserve entries stamped under that
/// batch, because this crate is the parent of the reserve and cannot reach it.
/// Removing the batch before its entries are evicted orphans those entries: once
/// the batch leaves the store the reserve's reconciliation sweep can no longer
/// see it, so the entries are never shed, which inflates the reserve size and
/// therefore the consensus-committed storage radius.
///
/// The enforced ordering is **evict before remove**, and it is owned jointly
/// with the reserve: the live ingest wiring (#391/#392) must route an `Expired`
/// event through the reserve's evict-then-acknowledge entry point
/// (`ExpirySweep::on_expired_event`), passing [`acknowledge_expired`] as the
/// acknowledgement so the store removal runs *only after* eviction has
/// succeeded. Driving `handle_event(Expired)` (or [`acknowledge_expired`])
/// standalone for the live `Expired` path is therefore a bug: it skips the
/// eviction and strands entries. The other three variants have no reserve
/// coupling and are safe to drive directly.
///
/// [`acknowledge_expired`]: DbBatchStore::acknowledge_expired
impl<DB: Database> BatchEventHandler for DbBatchStore<DB> {
    type Error = DbBatchStoreError;

    fn handle_event(&mut self, event: BatchEvent) -> Result<(), Self::Error> {
        match event {
            BatchEvent::Created { batch } => self.put(batch),
            BatchEvent::TopUp {
                batch_id,
                new_value,
            } => self.mutate_sync(&batch_id, |batch| batch.set_value(new_value)),
            BatchEvent::DepthIncrease {
                batch_id,
                new_depth,
            } => self.mutate_sync(&batch_id, |batch| batch.set_depth(new_depth)),
            BatchEvent::Expired { batch_id } => {
                self.acknowledge_expired(&batch_id)?;
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256};
    use vertex_storage_redb::RedbDatabase;

    fn sample_batch(id_byte: u8, value: u128, depth: u8) -> Batch {
        Batch::new(
            B256::repeat_byte(id_byte),
            value,
            100,
            Address::repeat_byte(0x11),
            depth,
            16,
            false,
        )
    }

    fn in_memory_store() -> DbBatchStore<RedbDatabase> {
        let db = RedbDatabase::in_memory().unwrap().into_arc();
        DbBatchStore::new(db).unwrap()
    }

    #[test]
    fn crud_roundtrip() {
        let store = in_memory_store();
        let batch = sample_batch(0xaa, 1000, 20);
        let id = batch.id();

        assert!(!store.contains(&id).unwrap());
        assert_eq!(store.get(&id).unwrap(), None);
        assert_eq!(store.count().unwrap(), 0);

        store.put(batch.clone()).unwrap();

        assert!(store.contains(&id).unwrap());
        assert_eq!(store.get(&id).unwrap(), Some(batch));
        assert_eq!(store.count().unwrap(), 1);

        // put is upsert: a second put of the same id replaces, not duplicates.
        let updated = sample_batch(0xaa, 2000, 20);
        store.put(updated.clone()).unwrap();
        assert_eq!(store.count().unwrap(), 1);
        assert_eq!(store.get(&id).unwrap(), Some(updated));

        assert!(store.remove(&id).unwrap());
        assert!(!store.remove(&id).unwrap());
        assert!(!store.contains(&id).unwrap());
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn context_defaults_then_persists() {
        let store = in_memory_store();
        // A fresh store reports the default context.
        assert_eq!(store.context().unwrap(), PostageContext::default());

        let ctx = PostageContext::new(12_345, 678);
        store.set_context(ctx).unwrap();
        assert_eq!(store.context().unwrap(), ctx);

        // Overwrite the singleton.
        let ctx2 = PostageContext::new(99_999, 1);
        store.set_context(ctx2).unwrap();
        assert_eq!(store.context().unwrap(), ctx2);
    }

    #[test]
    fn batch_ids_lists_all_keys_sorted() {
        let store = in_memory_store();
        for b in [0x03u8, 0x01, 0x02] {
            store.put(sample_batch(b, 500, 20)).unwrap();
        }
        let ids = store.batch_ids().unwrap();
        // The cursor walks key order, which for the 32-byte big-endian id is
        // ascending by the repeated byte.
        assert_eq!(
            ids,
            vec![
                B256::repeat_byte(0x01),
                B256::repeat_byte(0x02),
                B256::repeat_byte(0x03),
            ]
        );
    }

    #[test]
    fn persistence_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("postage.redb");
        let batch = sample_batch(0xcc, 4242, 22);
        let id = batch.id();
        let ctx = PostageContext::new(7, 8);

        {
            let db = RedbDatabase::create(&path).unwrap().into_arc();
            let store = DbBatchStore::new(db).unwrap();
            store.put(batch.clone()).unwrap();
            store.set_context(ctx).unwrap();
        }

        // Reopen the same file: state survives.
        let db = RedbDatabase::open(&path).unwrap().into_arc();
        let store = DbBatchStore::new(db).unwrap();
        assert_eq!(store.get(&id).unwrap(), Some(batch));
        assert_eq!(store.context().unwrap(), ctx);
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn handle_event_created() {
        let mut store = in_memory_store();
        let batch = sample_batch(0x01, 1000, 20);
        let id = batch.id();
        store
            .handle_event(BatchEvent::Created {
                batch: batch.clone(),
            })
            .unwrap();
        assert_eq!(store.get(&id).unwrap(), Some(batch));
    }

    #[test]
    fn handle_event_topup_updates_value() {
        let mut store = in_memory_store();
        let batch = sample_batch(0x02, 1000, 20);
        let id = batch.id();
        store.handle_event(BatchEvent::Created { batch }).unwrap();
        store
            .handle_event(BatchEvent::TopUp {
                batch_id: id,
                new_value: 5000,
            })
            .unwrap();
        assert_eq!(store.get(&id).unwrap().unwrap().value(), 5000);

        // Top-up for an unknown batch is a no-op (idempotent ingest).
        let unknown = B256::repeat_byte(0xff);
        store
            .handle_event(BatchEvent::TopUp {
                batch_id: unknown,
                new_value: 9,
            })
            .unwrap();
        assert_eq!(store.get(&unknown).unwrap(), None);
    }

    #[test]
    fn handle_event_depth_increase_updates_depth() {
        let mut store = in_memory_store();
        let batch = sample_batch(0x03, 1000, 20);
        let id = batch.id();
        store.handle_event(BatchEvent::Created { batch }).unwrap();
        store
            .handle_event(BatchEvent::DepthIncrease {
                batch_id: id,
                new_depth: 24,
            })
            .unwrap();
        assert_eq!(store.get(&id).unwrap().unwrap().depth(), 24);
    }

    #[test]
    fn handle_event_expired_removes() {
        let mut store = in_memory_store();
        let batch = sample_batch(0x04, 1000, 20);
        let id = batch.id();
        store.handle_event(BatchEvent::Created { batch }).unwrap();
        assert!(store.contains(&id).unwrap());
        store
            .handle_event(BatchEvent::Expired { batch_id: id })
            .unwrap();
        assert!(!store.contains(&id).unwrap());
        // Expiring an unknown batch is a harmless no-op.
        store
            .handle_event(BatchEvent::Expired { batch_id: id })
            .unwrap();
    }

    #[test]
    fn acknowledge_expired_is_idempotent() {
        // The explicit removal seam returns whether the batch existed, and a
        // second acknowledgement is a harmless no-op (redelivered Expired event).
        let store = in_memory_store();
        let batch = sample_batch(0x05, 1000, 20);
        let id = batch.id();
        store.put(batch).unwrap();
        assert!(store.acknowledge_expired(&id).unwrap(), "first removal");
        assert!(
            !store.acknowledge_expired(&id).unwrap(),
            "second removal is a no-op"
        );
        assert!(!store.contains(&id).unwrap());
    }

    #[test]
    fn mutate_event_is_single_transaction() {
        // The TopUp/DepthIncrease path loads, mutates and stores inside one
        // transaction (mutate_sync), so the round trip is atomic. We cannot
        // schedule a real interleaving in a single-threaded test, but we can
        // assert the combined effect of two successive mutations is the last
        // writer's value with no lost write, exercising the one-transaction path.
        let mut store = in_memory_store();
        let batch = sample_batch(0x06, 1000, 20);
        let id = batch.id();
        store.handle_event(BatchEvent::Created { batch }).unwrap();

        store
            .handle_event(BatchEvent::TopUp {
                batch_id: id,
                new_value: 7777,
            })
            .unwrap();
        store
            .handle_event(BatchEvent::DepthIncrease {
                batch_id: id,
                new_depth: 30,
            })
            .unwrap();

        let got = store.get(&id).unwrap().unwrap();
        assert_eq!(got.value(), 7777, "top-up value survives the depth update");
        assert_eq!(got.depth(), 30, "depth update applied");
    }

    /// Regression marker for the remove-before-evict race (the orphan hazard).
    ///
    /// The orphan-safe ordering (evict the reserve entries, then acknowledge the
    /// store removal) is enforced by the reserve crate's
    /// `ExpirySweep::on_expired_event`, which is downstream of this crate, so the
    /// race cannot be reproduced from `vertex-swarm-postage` alone (there is no
    /// reserve here to orphan). The end-to-end "removal before sweep orphans
    /// entries" regression lives in the reserve crate's tests (PR-D/PR-E), where
    /// both halves are in scope. This crate's contribution is the explicit
    /// [`DbBatchStore::acknowledge_expired`] seam and the contract documentation;
    /// this ignored test records the cross-crate gap so it is not lost.
    #[test]
    #[ignore = "cross-crate: enforced and regression-tested in the reserve crate (PR-D/PR-E)"]
    fn remove_before_evict_orphans_entries_is_reserve_crate_concern() {}

    #[test]
    fn batch_id_key_codec_roundtrip() {
        let k = BatchIdKey(B256::repeat_byte(0x7e));
        assert_eq!(BatchIdKey::decode(k.encode().as_ref()).unwrap(), k);
        assert!(BatchIdKey::decode(&[0u8; 31]).is_err());
    }
}
