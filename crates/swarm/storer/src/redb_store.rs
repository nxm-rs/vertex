//! redb-based chunk storage backend.
//!
//! This module provides [`RedbChunkStore`], a persistent chunk store
//! backed by the redb embedded database.

use std::path::Path;

use nectar_primitives::ChunkAddress;
use redb::{Database, ReadableTable, TableDefinition};
use tracing::debug;

use crate::{ChunkStore, StorerResult};

/// Table definition for chunks.
/// Key: 32-byte chunk address
/// Value: chunk data bytes
const CHUNKS_TABLE: TableDefinition<&[u8; 32], &[u8]> = TableDefinition::new("chunks");

/// redb-based chunk store.
///
/// Uses redb for ACID-compliant persistent storage of chunks.
/// Thread-safe for concurrent reads and writes.
pub struct RedbChunkStore {
    db: Database,
}

impl RedbChunkStore {
    /// Open or create a chunk store at the given path.
    pub fn open<P: AsRef<Path>>(path: P) -> StorerResult<Self> {
        let db = Database::create(path)?;

        // Ensure the chunks table exists
        let write_txn = db.begin_write()?;
        {
            let _ = write_txn.open_table(CHUNKS_TABLE)?;
        }
        write_txn.commit()?;

        debug!("Opened redb chunk store");
        Ok(Self { db })
    }

    /// Open an existing store (fails if it doesn't exist).
    #[allow(dead_code)]
    pub fn open_existing<P: AsRef<Path>>(path: P) -> StorerResult<Self> {
        let db = Database::open(path)?;
        Ok(Self { db })
    }
}

/// Convert ChunkAddress to a fixed-size byte array for redb key.
fn address_to_key(address: &ChunkAddress) -> &[u8; 32] {
    // ChunkAddress is SwarmAddress(B256), and B256 derefs to [u8; 32]
    address.0.as_ref()
}

impl ChunkStore for RedbChunkStore {
    fn put(&self, address: &ChunkAddress, data: &[u8]) -> StorerResult<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(CHUNKS_TABLE)?;
            // Use insert which doesn't overwrite existing
            let key = address_to_key(address);
            if table.get(key)?.is_none() {
                table.insert(key, data)?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    fn get(&self, address: &ChunkAddress) -> StorerResult<Option<Vec<u8>>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(CHUNKS_TABLE)?;
        let key = address_to_key(address);
        match table.get(key)? {
            Some(value) => Ok(Some(value.value().to_vec())),
            None => Ok(None),
        }
    }

    fn contains(&self, address: &ChunkAddress) -> StorerResult<bool> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(CHUNKS_TABLE)?;
        let key = address_to_key(address);
        Ok(table.get(key)?.is_some())
    }

    fn delete(&self, address: &ChunkAddress) -> StorerResult<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(CHUNKS_TABLE)?;
            let key = address_to_key(address);
            table.remove(key)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn count(&self) -> StorerResult<u64> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(CHUNKS_TABLE)?;
        // Count by iterating
        let mut count = 0u64;
        for _ in table.iter()? {
            count += 1;
        }
        Ok(count)
    }

    fn for_each<F>(&self, mut callback: F) -> StorerResult<()>
    where
        F: FnMut(&ChunkAddress) -> bool,
    {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(CHUNKS_TABLE)?;

        for entry in table.iter()? {
            let (key, _) = entry?;
            let address = ChunkAddress::new(*key.value());
            if !callback(&address) {
                break;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_address(n: u8) -> ChunkAddress {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        ChunkAddress::new(bytes)
    }

    #[test]
    fn test_put_get() {
        let dir = tempdir().unwrap();
        let store = RedbChunkStore::open(dir.path().join("test.redb")).unwrap();

        let addr = test_address(1);
        let data = b"hello world";

        // Put
        store.put(&addr, data).unwrap();

        // Get
        let retrieved = store.get(&addr).unwrap();
        assert_eq!(retrieved, Some(data.to_vec()));
    }

    #[test]
    fn test_contains() {
        let dir = tempdir().unwrap();
        let store = RedbChunkStore::open(dir.path().join("test.redb")).unwrap();

        let addr = test_address(2);
        assert!(!store.contains(&addr).unwrap());

        store.put(&addr, b"data").unwrap();
        assert!(store.contains(&addr).unwrap());
    }

    #[test]
    fn test_delete() {
        let dir = tempdir().unwrap();
        let store = RedbChunkStore::open(dir.path().join("test.redb")).unwrap();

        let addr = test_address(3);
        store.put(&addr, b"data").unwrap();
        assert!(store.contains(&addr).unwrap());

        store.delete(&addr).unwrap();
        assert!(!store.contains(&addr).unwrap());
    }

    #[test]
    fn test_count() {
        let dir = tempdir().unwrap();
        let store = RedbChunkStore::open(dir.path().join("test.redb")).unwrap();

        assert_eq!(store.count().unwrap(), 0);

        for i in 0..5 {
            store.put(&test_address(i), b"data").unwrap();
        }

        assert_eq!(store.count().unwrap(), 5);
    }

    #[test]
    fn test_for_each() {
        let dir = tempdir().unwrap();
        let store = RedbChunkStore::open(dir.path().join("test.redb")).unwrap();

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
    }

    #[test]
    fn test_idempotent_put() {
        let dir = tempdir().unwrap();
        let store = RedbChunkStore::open(dir.path().join("test.redb")).unwrap();

        let addr = test_address(4);
        store.put(&addr, b"first").unwrap();
        store.put(&addr, b"second").unwrap();

        // Should still have first data (no overwrite)
        let retrieved = store.get(&addr).unwrap();
        assert_eq!(retrieved, Some(b"first".to_vec()));
    }
}
