//! In-memory peer store (does not persist across restarts).

use std::collections::HashMap;

use parking_lot::RwLock;

use crate::error::StoreError;
use crate::traits::{NetPeerStore, NetRecord};

/// In-memory peer store for testing or ephemeral storage.
pub struct MemoryPeerStore<R: NetRecord> {
    peers: RwLock<HashMap<R::Id, R>>,
}

impl<R: NetRecord> Default for MemoryPeerStore<R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: NetRecord> MemoryPeerStore<R> {
    pub fn new() -> Self {
        Self {
            peers: RwLock::new(HashMap::new()),
        }
    }
}

impl<R: NetRecord> NetPeerStore<R> for MemoryPeerStore<R> {
    fn load_all(&self) -> Result<Vec<R>, StoreError> {
        Ok(self.peers.read().values().cloned().collect())
    }

    fn save(&self, record: &R) -> Result<(), StoreError> {
        self.peers.write().insert(record.id().clone(), record.clone());
        Ok(())
    }

    fn save_batch(&self, records: &[R]) -> Result<(), StoreError> {
        let mut store = self.peers.write();
        for record in records {
            store.insert(record.id().clone(), record.clone());
        }
        Ok(())
    }

    fn remove(&self, id: &R::Id) -> Result<bool, StoreError> {
        Ok(self.peers.write().remove(id).is_some())
    }

    fn get(&self, id: &R::Id) -> Result<Option<R>, StoreError> {
        Ok(self.peers.read().get(id).cloned())
    }

    fn count(&self) -> Result<usize, StoreError> {
        Ok(self.peers.read().len())
    }

    fn clear(&self) -> Result<(), StoreError> {
        self.peers.write().clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
    struct TestId(u64);

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct TestRecord {
        id: TestId,
        value: u32,
    }

    impl NetRecord for TestRecord {
        type Id = TestId;
        fn id(&self) -> &TestId { &self.id }
    }

    fn record(n: u64) -> TestRecord {
        TestRecord {
            id: TestId(n),
            value: n as u32,
        }
    }

    #[test]
    fn test_basic() {
        let store = MemoryPeerStore::<TestRecord>::new();

        assert_eq!(store.count().unwrap(), 0);
        assert!(store.load_all().unwrap().is_empty());

        let record = record(1);
        store.save(&record).unwrap();

        assert_eq!(store.count().unwrap(), 1);
        assert!(store.contains(&TestId(1)).unwrap());

        let loaded = store.get(&TestId(1)).unwrap().unwrap();
        assert_eq!(loaded.id, TestId(1));

        assert!(store.remove(&TestId(1)).unwrap());
        assert_eq!(store.count().unwrap(), 0);
        assert!(!store.contains(&TestId(1)).unwrap());
    }

    #[test]
    fn test_batch() {
        let store = MemoryPeerStore::<TestRecord>::new();

        let records: Vec<_> = (1..=5).map(record).collect();
        store.save_batch(&records).unwrap();

        assert_eq!(store.count().unwrap(), 5);
        assert_eq!(store.load_all().unwrap().len(), 5);
    }

    #[test]
    fn test_update() {
        let store = MemoryPeerStore::<TestRecord>::new();

        let mut record = record(1);
        store.save(&record).unwrap();

        record.value = 42;
        store.save(&record).unwrap();

        assert_eq!(store.count().unwrap(), 1);
        let loaded = store.get(&TestId(1)).unwrap().unwrap();
        assert_eq!(loaded.value, 42);
    }

    #[test]
    fn test_clear() {
        let store = MemoryPeerStore::<TestRecord>::new();

        let records: Vec<_> = (1..=5).map(record).collect();
        store.save_batch(&records).unwrap();
        assert_eq!(store.count().unwrap(), 5);

        store.clear().unwrap();
        assert_eq!(store.count().unwrap(), 0);
    }
}
