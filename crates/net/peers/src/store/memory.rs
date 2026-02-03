//! In-memory peer store (does not persist across restarts).

use std::collections::HashMap;
use std::marker::PhantomData;

use parking_lot::RwLock;

use crate::state::NetPeerSnapshot;
use crate::traits::NetPeerId;

use super::{ExtSnapBounds, NetPeerStore, PeerStoreError};

/// In-memory peer store for testing.
pub struct MemoryPeerStore<
    Id: NetPeerId,
    ExtSnap: ExtSnapBounds = (),
    ScoreExtSnap: ExtSnapBounds = (),
> {
    peers: RwLock<HashMap<Id, NetPeerSnapshot<Id, ExtSnap, ScoreExtSnap>>>,
    _marker: PhantomData<(ExtSnap, ScoreExtSnap)>,
}

impl<Id: NetPeerId, ExtSnap: ExtSnapBounds, ScoreExtSnap: ExtSnapBounds> Default
    for MemoryPeerStore<Id, ExtSnap, ScoreExtSnap>
{
    fn default() -> Self {
        Self::new()
    }
}

impl<Id: NetPeerId, ExtSnap: ExtSnapBounds, ScoreExtSnap: ExtSnapBounds>
    MemoryPeerStore<Id, ExtSnap, ScoreExtSnap>
{
    pub fn new() -> Self {
        Self {
            peers: RwLock::new(HashMap::new()),
            _marker: PhantomData,
        }
    }
}

impl<Id: NetPeerId, ExtSnap: ExtSnapBounds, ScoreExtSnap: ExtSnapBounds>
    NetPeerStore<Id, ExtSnap, ScoreExtSnap> for MemoryPeerStore<Id, ExtSnap, ScoreExtSnap>
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
        Ok(())
    }

    fn remove(&self, id: &Id) -> Result<(), PeerStoreError> {
        self.peers.write().remove(id);
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
        Ok(())
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
        let store = MemoryPeerStore::<TestId>::new();

        assert_eq!(store.count().unwrap(), 0);
        assert!(store.load_all().unwrap().is_empty());

        let snapshot = test_snapshot(1);
        store.save(&snapshot).unwrap();

        assert_eq!(store.count().unwrap(), 1);
        assert!(store.contains(&TestId(1)).unwrap());

        let loaded = store.get(&TestId(1)).unwrap().unwrap();
        assert_eq!(loaded.id, TestId(1));

        store.remove(&TestId(1)).unwrap();
        assert_eq!(store.count().unwrap(), 0);
        assert!(!store.contains(&TestId(1)).unwrap());
    }

    #[test]
    fn test_batch() {
        let store = MemoryPeerStore::<TestId>::new();

        let snapshots: Vec<_> = (1..=5).map(test_snapshot).collect();
        store.save_batch(&snapshots).unwrap();

        assert_eq!(store.count().unwrap(), 5);
        assert_eq!(store.load_all().unwrap().len(), 5);
    }

    #[test]
    fn test_update() {
        let store = MemoryPeerStore::<TestId>::new();

        let mut snapshot = test_snapshot(1);
        store.save(&snapshot).unwrap();

        snapshot.scoring.connection_successes = 10;
        store.save(&snapshot).unwrap();

        assert_eq!(store.count().unwrap(), 1);
        let loaded = store.get(&TestId(1)).unwrap().unwrap();
        assert_eq!(loaded.scoring.connection_successes, 10);
    }

    #[test]
    fn test_clear() {
        let store = MemoryPeerStore::<TestId>::new();

        let snapshots: Vec<_> = (1..=5).map(test_snapshot).collect();
        store.save_batch(&snapshots).unwrap();
        assert_eq!(store.count().unwrap(), 5);

        store.clear().unwrap();
        assert_eq!(store.count().unwrap(), 0);
    }
}
