//! Database-backed chunk store over the vertex-storage `Database` trait.
//!
//! [`DbChunkStore`] is generic over the storage backend, so the same code
//! serves both in-memory and on-disk databases. The node opens one database
//! and shares it across all consumers; this store only defines its own table.

use std::sync::Arc;

use nectar_primitives::ChunkAddress;
use vertex_storage::{Database, DbTx, DbTxMut, Table, table};

use crate::{ChunkStore, StorerResult};

// Chunk table: ChunkAddress -> raw chunk bytes.
//
// Values are stored uncompressed: chunk payloads are arbitrary and often
// encrypted, so compression costs CPU without saving space.
table!(pub(crate) ChunkTable, "chunks", ChunkAddress, Vec<u8>, compressed = false);

/// Chunk store backed by the vertex-storage `Database` trait.
///
/// Generic over the backend, so persistence is decided by whichever database
/// the node opens (in-memory or on-disk). Each operation is a single
/// transaction; the store is thread-safe for concurrent reads and writes.
pub struct DbChunkStore<DB: Database> {
    db: Arc<DB>,
}

impl<DB: Database> DbChunkStore<DB> {
    /// Create a chunk store over a shared database handle.
    ///
    /// Ensures the chunk table exists before returning, so every read path
    /// works on a fresh database without a separate initialization step.
    pub fn new(db: Arc<DB>) -> StorerResult<Self> {
        db.update(|tx| tx.ensure_table(ChunkTable::NAME))?;
        Ok(Self { db })
    }
}

impl<DB: Database> ChunkStore for DbChunkStore<DB> {
    fn put(&self, address: &ChunkAddress, data: &[u8]) -> StorerResult<()> {
        self.db.update(|tx| {
            // Chunks are content-addressed: never overwrite an existing entry.
            // The duplicate probe checks key presence without decoding the value.
            if !tx.exists::<ChunkTable>(*address)? {
                tx.put::<ChunkTable>(*address, data.to_vec())?;
            }
            Ok(())
        })?;
        Ok(())
    }

    fn get(&self, address: &ChunkAddress) -> StorerResult<Option<Vec<u8>>> {
        Ok(self.db.view(|tx| tx.get::<ChunkTable>(*address))?)
    }

    fn contains(&self, address: &ChunkAddress) -> StorerResult<bool> {
        // Key-presence probe: never decodes the chunk value.
        Ok(self.db.view(|tx| tx.exists::<ChunkTable>(*address))?)
    }

    fn delete(&self, address: &ChunkAddress) -> StorerResult<()> {
        self.db.update(|tx| {
            tx.delete::<ChunkTable>(*address)?;
            Ok(())
        })?;
        Ok(())
    }

    fn count(&self) -> StorerResult<u64> {
        let count = self.db.view(|tx| tx.count::<ChunkTable>())?;
        Ok(count as u64)
    }

    fn for_each<F>(&self, mut callback: F) -> StorerResult<()>
    where
        F: FnMut(&ChunkAddress) -> bool,
    {
        // Stream entries via a lazy cursor: callers that only need the first key,
        // such as Reserve::evict_oldest, stop after one step instead of paying an
        // O(N) scan. Each step still decodes its value because the cursor has no
        // key-only mode.
        let tx = self.db.tx()?;
        let mut cursor = tx.cursor::<ChunkTable>()?;
        let mut entry = cursor.first()?;
        while let Some((address, _data)) = entry {
            if !callback(&address) {
                break;
            }
            entry = cursor.next()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use vertex_storage_redb::RedbDatabase;

    fn test_address(n: u8) -> ChunkAddress {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        ChunkAddress::new(bytes)
    }

    /// Run a test against both the in-memory and on-disk redb backends.
    fn with_backends(test: impl Fn(DbChunkStore<RedbDatabase>)) {
        let mem = DbChunkStore::new(RedbDatabase::in_memory().unwrap().into_arc()).unwrap();
        test(mem);

        let dir = tempdir().unwrap();
        let disk = DbChunkStore::new(
            RedbDatabase::create(dir.path().join("chunks.redb"))
                .unwrap()
                .into_arc(),
        )
        .unwrap();
        test(disk);
    }

    #[test]
    fn test_fresh_database_reads() {
        // Regression: read paths must work on a fresh database before any
        // write. The constructor guarantees the chunk table exists.
        with_backends(|store| {
            let addr = test_address(9);
            assert_eq!(store.count().unwrap(), 0);
            assert_eq!(store.get(&addr).unwrap(), None);
            assert!(!store.contains(&addr).unwrap());

            let mut visited = 0;
            store
                .for_each(|_| {
                    visited += 1;
                    true
                })
                .unwrap();
            assert_eq!(visited, 0);
        });
    }

    #[test]
    fn test_put_get() {
        with_backends(|store| {
            let addr = test_address(1);
            let data = b"hello world";

            store.put(&addr, data).unwrap();

            let retrieved = store.get(&addr).unwrap();
            assert_eq!(retrieved, Some(data.to_vec()));
        });
    }

    #[test]
    fn test_contains() {
        with_backends(|store| {
            let addr = test_address(2);
            assert!(!store.contains(&addr).unwrap());

            store.put(&addr, b"data").unwrap();
            assert!(store.contains(&addr).unwrap());
        });
    }

    #[test]
    fn test_delete() {
        with_backends(|store| {
            let addr = test_address(3);
            store.put(&addr, b"data").unwrap();
            assert!(store.contains(&addr).unwrap());

            store.delete(&addr).unwrap();
            assert!(!store.contains(&addr).unwrap());

            // Deleting a missing chunk is a no-op.
            store.delete(&addr).unwrap();
        });
    }

    #[test]
    fn test_count() {
        with_backends(|store| {
            assert_eq!(store.count().unwrap(), 0);

            for i in 0..5 {
                store.put(&test_address(i), b"data").unwrap();
            }

            assert_eq!(store.count().unwrap(), 5);
        });
    }

    #[test]
    fn test_for_each() {
        with_backends(|store| {
            for i in 0..3 {
                store.put(&test_address(i), b"data").unwrap();
            }

            let mut count = 0;
            store
                .for_each(|_| {
                    count += 1;
                    true
                })
                .unwrap();
            assert_eq!(count, 3);

            // Early termination stops after the first address.
            let mut visited = 0;
            store
                .for_each(|_| {
                    visited += 1;
                    false
                })
                .unwrap();
            assert_eq!(visited, 1);
        });
    }

    #[test]
    fn test_idempotent_put() {
        with_backends(|store| {
            let addr = test_address(4);
            store.put(&addr, b"first").unwrap();
            store.put(&addr, b"second").unwrap();

            // Content-addressed: the first write wins.
            let retrieved = store.get(&addr).unwrap();
            assert_eq!(retrieved, Some(b"first".to_vec()));
        });
    }

    #[test]
    fn test_persistence_across_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("chunks.redb");
        let addr = test_address(7);

        {
            let store = DbChunkStore::new(RedbDatabase::create(&path).unwrap().into_arc()).unwrap();
            store.put(&addr, b"persisted").unwrap();
        }

        let store = DbChunkStore::new(RedbDatabase::open(&path).unwrap().into_arc()).unwrap();
        assert_eq!(store.get(&addr).unwrap(), Some(b"persisted".to_vec()));
        assert_eq!(store.count().unwrap(), 1);
    }
}
