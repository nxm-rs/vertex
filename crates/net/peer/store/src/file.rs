//! JSON file-based peer store with atomic writes.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter};
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::RwLock;

use crate::error::StoreError;
use crate::record::PeerRecord;
use crate::traits::{DataBounds, NetPeerId, NetPeerStore};

/// JSON file store. Loaded to memory on startup, written back on flush.
pub struct FilePeerStore<Id: NetPeerId, Data: DataBounds = ()> {
    path: PathBuf,
    peers: RwLock<HashMap<Id, PeerRecord<Id, Data>>>,
    dirty: AtomicBool,
    _marker: PhantomData<Data>,
}

impl<Id: NetPeerId, Data: DataBounds> FilePeerStore<Id, Data> {
    /// Load existing file or create empty store.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        let peers = if path.exists() {
            Self::load_from_file(&path)?
        } else {
            HashMap::new()
        };

        Ok(Self {
            path,
            peers: RwLock::new(peers),
            dirty: AtomicBool::new(false),
            _marker: PhantomData,
        })
    }

    /// Create store, making parent directories if needed.
    pub fn new_with_create_dir(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| StoreError::CreateDir {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        Self::new(path)
    }

    fn load_from_file(path: &PathBuf) -> Result<HashMap<Id, PeerRecord<Id, Data>>, StoreError> {
        let file = File::open(path).map_err(|e| StoreError::Open {
            path: path.clone(),
            source: e,
        })?;
        let reader = BufReader::new(file);

        let records: Vec<PeerRecord<Id, Data>> =
            serde_json::from_reader(reader).map_err(|e| StoreError::Deserialize {
                path: path.clone(),
                reason: e.to_string(),
            })?;

        let mut peers = HashMap::with_capacity(records.len());
        for record in records {
            peers.insert(record.id().clone(), record);
        }

        Ok(peers)
    }

    fn save_to_file(&self) -> Result<(), StoreError> {
        let peers = self.peers.read();
        let records: Vec<&PeerRecord<Id, Data>> = peers.values().collect();

        // Write to temp file first, then rename (atomic)
        let tmp_path = self.path.with_extension("json.tmp");
        {
            let file = File::create(&tmp_path).map_err(|e| StoreError::Write {
                path: tmp_path.clone(),
                source: e,
            })?;
            let writer = BufWriter::new(file);
            serde_json::to_writer_pretty(writer, &records).map_err(|e| StoreError::Serialize {
                path: self.path.clone(),
                reason: e.to_string(),
            })?;
        }

        fs::rename(&tmp_path, &self.path).map_err(|e| StoreError::Write {
            path: self.path.clone(),
            source: e,
        })?;
        Ok(())
    }

    fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
    }

    /// Check if there are unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Acquire)
    }

    /// Get the store file path.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl<Id: NetPeerId, Data: DataBounds> NetPeerStore<Id, Data> for FilePeerStore<Id, Data> {
    fn load_all(&self) -> Result<Vec<PeerRecord<Id, Data>>, StoreError> {
        Ok(self.peers.read().values().cloned().collect())
    }

    fn save(&self, record: &PeerRecord<Id, Data>) -> Result<(), StoreError> {
        self.peers.write().insert(record.id().clone(), record.clone());
        self.mark_dirty();
        Ok(())
    }

    fn save_batch(&self, records: &[PeerRecord<Id, Data>]) -> Result<(), StoreError> {
        let mut store = self.peers.write();
        for record in records {
            store.insert(record.id().clone(), record.clone());
        }
        drop(store);
        self.mark_dirty();
        Ok(())
    }

    fn remove(&self, id: &Id) -> Result<bool, StoreError> {
        let removed = self.peers.write().remove(id).is_some();
        if removed {
            self.mark_dirty();
        }
        Ok(removed)
    }

    fn get(&self, id: &Id) -> Result<Option<PeerRecord<Id, Data>>, StoreError> {
        Ok(self.peers.read().get(id).cloned())
    }

    fn count(&self) -> Result<usize, StoreError> {
        Ok(self.peers.read().len())
    }

    fn clear(&self) -> Result<(), StoreError> {
        self.peers.write().clear();
        self.mark_dirty();
        Ok(())
    }

    fn flush(&self) -> Result<(), StoreError> {
        if self.is_dirty() {
            self.save_to_file()?;
            self.dirty.store(false, Ordering::Release);
        }
        Ok(())
    }
}

impl<Id: NetPeerId, Data: DataBounds> Drop for FilePeerStore<Id, Data> {
    fn drop(&mut self) {
        if self.is_dirty() {
            let _ = self.save_to_file();
        }
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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers.json");

        let store = FilePeerStore::<TestId, TestData>::new(&path).unwrap();

        assert_eq!(store.count().unwrap(), 0);
        assert!(!path.exists());

        let record = test_record(1);
        store.save(&record).unwrap();
        assert!(store.is_dirty());

        store.flush().unwrap();
        assert!(!store.is_dirty());
        assert!(path.exists());

        let loaded = store.get(&TestId(1)).unwrap().unwrap();
        assert_eq!(loaded.id(), &TestId(1));
    }

    #[test]
    fn test_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers.json");

        {
            let store = FilePeerStore::<TestId, TestData>::new(&path).unwrap();
            let records: Vec<_> = (1..=5).map(test_record).collect();
            store.save_batch(&records).unwrap();
            store.flush().unwrap();
        }

        {
            let store = FilePeerStore::<TestId, TestData>::new(&path).unwrap();
            assert_eq!(store.count().unwrap(), 5);

            for i in 1..=5 {
                assert!(store.contains(&TestId(i)).unwrap());
            }
        }
    }

    #[test]
    fn test_update() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers.json");

        let store = FilePeerStore::<TestId, TestData>::new(&path).unwrap();

        let mut record = test_record(1);
        store.save(&record).unwrap();
        store.flush().unwrap();

        record.set_data(TestData { value: 42 });
        store.save(&record).unwrap();
        store.flush().unwrap();

        let store2 = FilePeerStore::<TestId, TestData>::new(&path).unwrap();
        let loaded = store2.get(&TestId(1)).unwrap().unwrap();
        assert_eq!(loaded.data().value, 42);
    }

    #[test]
    fn test_remove() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers.json");

        let store = FilePeerStore::<TestId, TestData>::new(&path).unwrap();
        store.save(&test_record(1)).unwrap();
        store.save(&test_record(2)).unwrap();
        store.flush().unwrap();

        assert!(store.remove(&TestId(1)).unwrap());
        assert!(!store.remove(&TestId(1)).unwrap()); // Already removed
        assert_eq!(store.count().unwrap(), 1);

        store.flush().unwrap();

        let store2 = FilePeerStore::<TestId, TestData>::new(&path).unwrap();
        assert_eq!(store2.count().unwrap(), 1);
        assert!(!store2.contains(&TestId(1)).unwrap());
        assert!(store2.contains(&TestId(2)).unwrap());
    }
}
