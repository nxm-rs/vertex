//! In-memory snapshot store (does not persist across restarts).

use parking_lot::RwLock;

use crate::PeerSnapshotStore;
use crate::error::StoreError;

/// In-memory snapshot store for testing or ephemeral storage.
pub struct MemoryPeerStore<R> {
    records: RwLock<Vec<R>>,
}

impl<R> Default for MemoryPeerStore<R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R> MemoryPeerStore<R> {
    pub fn new() -> Self {
        Self {
            records: RwLock::new(Vec::new()),
        }
    }
}

impl<R: Clone + Send + Sync> PeerSnapshotStore<R> for MemoryPeerStore<R> {
    fn load(&self) -> Result<Vec<R>, StoreError> {
        Ok(self.records.read().clone())
    }

    fn store(&self, records: &[R]) -> Result<(), StoreError> {
        *self.records.write() = records.to_vec();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip() {
        let store = MemoryPeerStore::<u32>::new();
        assert!(store.load().unwrap().is_empty());

        store.store(&[1, 2, 3]).unwrap();
        assert_eq!(store.load().unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn test_store_replaces() {
        let store = MemoryPeerStore::<u32>::new();
        store.store(&[1, 2, 3]).unwrap();
        store.store(&[4]).unwrap();
        assert_eq!(store.load().unwrap(), vec![4]);
    }

    #[test]
    fn test_store_empty_clears() {
        let store = MemoryPeerStore::<u32>::new();
        store.store(&[1]).unwrap();
        store.store(&[]).unwrap();
        assert!(store.load().unwrap().is_empty());
    }
}
