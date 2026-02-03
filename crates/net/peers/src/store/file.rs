//! JSON file-based peer store with atomic writes.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter};
use std::marker::PhantomData;
use std::path::PathBuf;

use parking_lot::{Mutex, RwLock};

use crate::state::NetPeerSnapshot;
use crate::traits::NetPeerId;

use super::{ExtSnapBounds, NetPeerStore, PeerStoreError};

/// JSON file store. Loaded to memory on startup, written back on flush.
pub struct FilePeerStore<
    Id: NetPeerId,
    ExtSnap: ExtSnapBounds = (),
    ScoreExtSnap: ExtSnapBounds = (),
> {
    path: PathBuf,
    peers: RwLock<HashMap<Id, NetPeerSnapshot<Id, ExtSnap, ScoreExtSnap>>>,
    dirty: Mutex<bool>,
    _marker: PhantomData<(ExtSnap, ScoreExtSnap)>,
}

impl<Id: NetPeerId, ExtSnap: ExtSnapBounds, ScoreExtSnap: ExtSnapBounds>
    FilePeerStore<Id, ExtSnap, ScoreExtSnap>
{
    /// Load existing file or create empty store.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, PeerStoreError> {
        let path = path.into();
        let peers = if path.exists() {
            Self::load_from_file(&path)?
        } else {
            HashMap::new()
        };

        Ok(Self {
            path,
            peers: RwLock::new(peers),
            dirty: Mutex::new(false),
            _marker: PhantomData,
        })
    }

    /// Create store, making parent directories if needed.
    pub fn new_with_create_dir(path: impl Into<PathBuf>) -> Result<Self, PeerStoreError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        Self::new(path)
    }

    fn load_from_file(
        path: &PathBuf,
    ) -> Result<HashMap<Id, NetPeerSnapshot<Id, ExtSnap, ScoreExtSnap>>, PeerStoreError> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);

        let snapshots: Vec<NetPeerSnapshot<Id, ExtSnap, ScoreExtSnap>> =
            serde_json::from_reader(reader)
                .map_err(|e| PeerStoreError::Serialization(e.to_string()))?;

        let mut peers = HashMap::with_capacity(snapshots.len());
        for snapshot in snapshots {
            peers.insert(snapshot.id.clone(), snapshot);
        }

        Ok(peers)
    }

    fn save_to_file(&self) -> Result<(), PeerStoreError> {
        let peers = self.peers.read();
        let snapshots: Vec<&NetPeerSnapshot<Id, ExtSnap, ScoreExtSnap>> = peers.values().collect();

        // Write to temp file first, then rename (atomic)
        let tmp_path = self.path.with_extension("json.tmp");
        {
            let file = File::create(&tmp_path)?;
            let writer = BufWriter::new(file);
            serde_json::to_writer_pretty(writer, &snapshots)
                .map_err(|e| PeerStoreError::Serialization(e.to_string()))?;
        }

        fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }

    fn mark_dirty(&self) {
        *self.dirty.lock() = true;
    }

    pub fn is_dirty(&self) -> bool {
        *self.dirty.lock()
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl<Id: NetPeerId, ExtSnap: ExtSnapBounds, ScoreExtSnap: ExtSnapBounds>
    NetPeerStore<Id, ExtSnap, ScoreExtSnap> for FilePeerStore<Id, ExtSnap, ScoreExtSnap>
{
    fn load_all(&self) -> Result<Vec<NetPeerSnapshot<Id, ExtSnap, ScoreExtSnap>>, PeerStoreError> {
        Ok(self.peers.read().values().cloned().collect())
    }

    fn save(
        &self,
        snapshot: &NetPeerSnapshot<Id, ExtSnap, ScoreExtSnap>,
    ) -> Result<(), PeerStoreError> {
        self.peers
            .write()
            .insert(snapshot.id.clone(), snapshot.clone());
        self.mark_dirty();
        Ok(())
    }

    fn save_batch(
        &self,
        snapshots: &[NetPeerSnapshot<Id, ExtSnap, ScoreExtSnap>],
    ) -> Result<(), PeerStoreError> {
        let mut store = self.peers.write();
        for snapshot in snapshots {
            store.insert(snapshot.id.clone(), snapshot.clone());
        }
        drop(store);
        self.mark_dirty();
        Ok(())
    }

    fn remove(&self, id: &Id) -> Result<(), PeerStoreError> {
        self.peers.write().remove(id);
        self.mark_dirty();
        Ok(())
    }

    fn get(
        &self,
        id: &Id,
    ) -> Result<Option<NetPeerSnapshot<Id, ExtSnap, ScoreExtSnap>>, PeerStoreError> {
        Ok(self.peers.read().get(id).cloned())
    }

    fn count(&self) -> Result<usize, PeerStoreError> {
        Ok(self.peers.read().len())
    }

    fn clear(&self) -> Result<(), PeerStoreError> {
        self.peers.write().clear();
        self.mark_dirty();
        Ok(())
    }

    fn flush(&self) -> Result<(), PeerStoreError> {
        if self.is_dirty() {
            self.save_to_file()?;
            *self.dirty.lock() = false;
        }
        Ok(())
    }
}

impl<Id: NetPeerId, ExtSnap: ExtSnapBounds, ScoreExtSnap: ExtSnapBounds> Drop
    for FilePeerStore<Id, ExtSnap, ScoreExtSnap>
{
    fn drop(&mut self) {
        if self.is_dirty() {
            let _ = self.save_to_file();
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::score::PeerScoreSnapshot;
    use crate::state::ConnectionState;

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
    struct TestId(u64);

    fn test_snapshot(n: u64) -> NetPeerSnapshot<TestId> {
        NetPeerSnapshot {
            id: TestId(n),
            scoring: PeerScoreSnapshot::default(),
            state: ConnectionState::Known,
            first_seen: 0,
            last_seen: 0,
            multiaddrs: vec![format!("/ip4/127.0.0.{}/tcp/1634", n).parse().unwrap()],
            ban_info: None,
            ext: (),
        }
    }

    #[test]
    fn test_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers.json");

        let store = FilePeerStore::<TestId>::new(&path).unwrap();

        assert_eq!(store.count().unwrap(), 0);
        assert!(!path.exists());

        let snapshot = test_snapshot(1);
        store.save(&snapshot).unwrap();
        assert!(store.is_dirty());

        store.flush().unwrap();
        assert!(!store.is_dirty());
        assert!(path.exists());

        let loaded = store.get(&TestId(1)).unwrap().unwrap();
        assert_eq!(loaded.id, TestId(1));
    }

    #[test]
    fn test_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers.json");

        {
            let store = FilePeerStore::<TestId>::new(&path).unwrap();
            let snapshots: Vec<_> = (1..=5).map(test_snapshot).collect();
            store.save_batch(&snapshots).unwrap();
            store.flush().unwrap();
        }

        {
            let store = FilePeerStore::<TestId>::new(&path).unwrap();
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

        let store = FilePeerStore::<TestId>::new(&path).unwrap();

        let mut snapshot = test_snapshot(1);
        store.save(&snapshot).unwrap();
        store.flush().unwrap();

        snapshot.scoring.connection_successes = 42;
        store.save(&snapshot).unwrap();
        store.flush().unwrap();

        let store2 = FilePeerStore::<TestId>::new(&path).unwrap();
        let loaded = store2.get(&TestId(1)).unwrap().unwrap();
        assert_eq!(loaded.scoring.connection_successes, 42);
    }
}
