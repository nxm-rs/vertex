//! Persisting batch store over the `vertex-storage` `Database`, plus the
//! [`BatchEventHandler`] on-chain ingest seam.
//!
//! [`DbBatchStore`] is generic over the backend (in-memory for tests, redb for
//! production) and defines two tables:
//!
//! - [`Batches`]: `BatchId -> Batch`, the authoritative batch set.
//! - [`ContextTable`]: the current [`PostageContext`], stored as one row under a
//!   fixed key since redb has no schema-level singleton.
//!
//! Every method is a single transaction. The [`BatchStore`] trait is sync, so no
//! transaction guard is ever held across an `await`.

use nectar_postage::{Batch, BatchEvent, BatchEventHandler, BatchId, BatchStore, PostageContext};
use std::sync::Arc;
use vertex_storage::{Database, DatabaseError, DbTx, DbTxMut, Decode, Encode, Table, table};

// `BatchId -> Batch`, compressed (the default).
table!(pub(crate) Batches, "postage_batches", BatchIdKey, Batch);

// Single-row `PostageContext` under [`ContextKey::SINGLETON`]. Uncompressed: the
// value is a tiny fixed record.
table!(pub(crate) ContextTable, "postage_context", ContextKey, PostageContext, compressed = false);

/// Key newtype carrying the 32-byte big-endian [`BatchId`] for the [`Batches`]
/// table (local newtype works around the orphan rule on the foreign `B256`).
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

/// Single-byte key addressing the lone [`ContextTable`] row.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub(crate) struct ContextKey(u8);

impl ContextKey {
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

/// Error returned by [`DbBatchStore`] operations.
#[derive(Debug, thiserror::Error)]
pub enum DbBatchStoreError {
    #[error("postage batch store database error: {0}")]
    Database(#[from] DatabaseError),
}

/// Batch store backed by the `vertex-storage` `Database` trait.
///
/// Implements both [`BatchStore`] (persistence) and [`BatchEventHandler`]
/// (on-chain ingest). Cheap to clone by `Arc` and thread-safe.
pub struct DbBatchStore<DB: Database> {
    db: Arc<DB>,
}

impl<DB: Database> DbBatchStore<DB> {
    /// Create a store, ensuring both tables exist so read paths work on a fresh
    /// database without a separate init step.
    pub fn new(db: Arc<DB>) -> Result<Self, DbBatchStoreError> {
        db.update(|tx| {
            tx.ensure_table(Batches::NAME)?;
            tx.ensure_table(ContextTable::NAME)?;
            Ok(())
        })?;
        Ok(Self { db })
    }

    pub fn database(&self) -> &Arc<DB> {
        &self.db
    }

    /// Atomic read-modify-write in one transaction, so a concurrent writer
    /// cannot clobber the update (lost update). A missing batch is a no-op,
    /// matching the idempotent ingest contract.
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

    /// Acknowledgement half of the evict-before-remove expiry contract: drops the
    /// batch row (idempotent) and returns whether it existed, evicting nothing.
    /// Drive only via the reserve's evict-then-acknowledge path; see the
    /// [`BatchEventHandler`] impl docs.
    pub fn acknowledge_expired(&self, id: &BatchId) -> Result<bool, DbBatchStoreError> {
        self.remove(id)
    }
}

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
        // Key-presence probe: never decodes the batch value.
        Ok(self.db.view(|tx| tx.exists::<Batches>(BatchIdKey(*id)))?)
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
        // Lazy read-only cursor owns its snapshot. Each step decodes its value
        // because the cursor has no key-only mode; only the keys are retained.
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

/// On-chain ingest seam: an indexer decodes contract logs into [`BatchEvent`]s
/// and drives this handler, one transaction per variant. Unknown batches are an
/// idempotent no-op (ordering is the indexer's responsibility).
///
/// `Expired` here is the bare removal: it does not evict the reserve entries
/// stamped under the batch (the reserve is downstream). Removing the batch
/// first orphans them, inflating the reserve size and the consensus-committed
/// storage radius, so live ingest must route `Expired` through the reserve's
/// evict-then-acknowledge entry point (passing [`acknowledge_expired`]) rather
/// than calling this directly. The other three variants are safe to drive here.
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

        // Top-up for an unknown batch is a no-op.
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
        // Two successive mutations leave both fields at their last-written value,
        // with no lost write.
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

    /// Marker for the remove-before-evict race: not reproducible here (no reserve
    /// to orphan), only via the [`DbBatchStore::acknowledge_expired`] seam.
    #[test]
    #[ignore = "cross-crate: enforced and regression-tested in the reserve crate"]
    fn remove_before_evict_orphans_entries_is_reserve_crate_concern() {}

    #[test]
    fn batch_id_key_codec_roundtrip() {
        let k = BatchIdKey(B256::repeat_byte(0x7e));
        assert_eq!(BatchIdKey::decode(k.encode().as_ref()).unwrap(), k);
        assert!(BatchIdKey::decode(&[0u8; 31]).is_err());
    }
}
