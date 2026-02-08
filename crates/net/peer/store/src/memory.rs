//! In-memory peer store (does not persist across restarts).

use std::collections::HashMap;
use std::marker::PhantomData;

use parking_lot::RwLock;

use crate::error::StoreError;
use crate::record::PeerRecord;
use crate::traits::{DataBounds, NetPeerId, NetPeerStore};

/// In-memory peer store for testing or ephemeral storage.
pub struct MemoryPeerStore<Id: NetPeerId, Data: DataBounds = ()> {
    peers: RwLock<HashMap<Id, PeerRecord<Id, Data>>>,
    _marker: PhantomData<Data>,
}

impl<Id: NetPeerId, Data: DataBounds> Default for MemoryPeerStore<Id, Data> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Id: NetPeerId, Data: DataBounds> MemoryPeerStore<Id, Data> {
    pub fn new() -> Self {
        Self {
            peers: RwLock::new(HashMap::new()),
            _marker: PhantomData,
        }
    }
}

impl<Id: NetPeerId, Data: DataBounds> NetPeerStore<Id, Data> for MemoryPeerStore<Id, Data> {
    fn load_all(&self) -> Result<Vec<PeerRecord<Id, Data>>, StoreError> {
        Ok(self.peers.read().values().cloned().collect())
    }

    fn save(&self, record: &PeerRecord<Id, Data>) -> Result<(), StoreError> {
        self.peers.write().insert(record.id().clone(), record.clone());
        Ok(())
    }

    fn save_batch(&self, records: &[PeerRecord<Id, Data>]) -> Result<(), StoreError> {
        let mut store = self.peers.write();
        for record in records {
            store.insert(record.id().clone(), record.clone());
        }
        Ok(())
    }

    fn remove(&self, id: &Id) -> Result<bool, StoreError> {
        Ok(self.peers.write().remove(id).is_some())
    }

    fn get(&self, id: &Id) -> Result<Option<PeerRecord<Id, Data>>, StoreError> {
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

    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    struct TestData {
        value: u32,
    }

    fn test_record(n: u64) -> PeerRecord<TestId, TestData> {
        PeerRecord::new(TestId(n), TestData { value: n as u32 }, 0, 0)
    }

    #[test]
    fn test_basic() {
        let store = MemoryPeerStore::<TestId, TestData>::new();

        assert_eq!(store.count().unwrap(), 0);
        assert!(store.load_all().unwrap().is_empty());

        let record = test_record(1);
        store.save(&record).unwrap();

        assert_eq!(store.count().unwrap(), 1);
        assert!(store.contains(&TestId(1)).unwrap());

        let loaded = store.get(&TestId(1)).unwrap().unwrap();
        assert_eq!(loaded.id(), &TestId(1));

        assert!(store.remove(&TestId(1)).unwrap());
        assert_eq!(store.count().unwrap(), 0);
        assert!(!store.contains(&TestId(1)).unwrap());
    }

    #[test]
    fn test_batch() {
        let store = MemoryPeerStore::<TestId, TestData>::new();

        let records: Vec<_> = (1..=5).map(test_record).collect();
        store.save_batch(&records).unwrap();

        assert_eq!(store.count().unwrap(), 5);
        assert_eq!(store.load_all().unwrap().len(), 5);
    }

    #[test]
    fn test_update() {
        let store = MemoryPeerStore::<TestId, TestData>::new();

        let mut record = test_record(1);
        store.save(&record).unwrap();

        record.set_data(TestData { value: 42 });
        store.save(&record).unwrap();

        assert_eq!(store.count().unwrap(), 1);
        let loaded = store.get(&TestId(1)).unwrap().unwrap();
        assert_eq!(loaded.data().value, 42);
    }

    #[test]
    fn test_clear() {
        let store = MemoryPeerStore::<TestId, TestData>::new();

        let records: Vec<_> = (1..=5).map(test_record).collect();
        store.save_batch(&records).unwrap();
        assert_eq!(store.count().unwrap(), 5);

        store.clear().unwrap();
        assert_eq!(store.count().unwrap(), 0);
    }
}
