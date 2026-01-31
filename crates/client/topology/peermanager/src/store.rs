//! Peer store trait for persistence.
//!
//! This module defines the [`PeerStore`] trait which abstracts over
//! different storage backends for peer data persistence.

use alloy_primitives::{B256, hex};
use auto_impl::auto_impl;
use thiserror::Error;

use crate::state::StoredPeer;

/// Error type for peer store operations.
#[derive(Debug, Error)]
pub enum PeerStoreError {
    /// IO error during storage operations.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    /// Serialization/deserialization error.
    #[error("Serialization error: {0}")]
    Serialization(String),
    /// Storage backend specific error.
    #[error("Storage error: {0}")]
    Storage(String),
}

/// Trait for peer data persistence.
///
/// Implementations can store peers in various backends:
/// - File-based (JSON, bincode, etc.)
/// - Database (SQLite, RocksDB, etc.)
/// - In-memory (for testing)
///
/// The store is responsible for persisting [`StoredPeer`] data which includes
/// the full BzzAddress (overlay, multiaddrs, signature, nonce) along with
/// connection statistics and ban information.
#[auto_impl(&, Box, Arc)]
pub trait PeerStore: Send + Sync {
    /// Load all persisted peers.
    ///
    /// Called on startup to restore the peer database.
    fn load_all(&self) -> Result<Vec<StoredPeer>, PeerStoreError>;

    /// Save a single peer.
    ///
    /// If the peer already exists (by overlay address), it is updated.
    fn save(&self, peer: &StoredPeer) -> Result<(), PeerStoreError>;

    /// Save a batch of peers.
    ///
    /// More efficient than calling `save` repeatedly.
    /// Implementations should use transactions or batch writes where possible.
    fn save_batch(&self, peers: &[StoredPeer]) -> Result<(), PeerStoreError> {
        for peer in peers {
            self.save(peer)?;
        }
        Ok(())
    }

    /// Remove a peer from storage.
    fn remove(&self, overlay: &B256) -> Result<(), PeerStoreError>;

    /// Get a specific peer by overlay address.
    fn get(&self, overlay: &B256) -> Result<Option<StoredPeer>, PeerStoreError>;

    /// Check if a peer exists in storage.
    fn contains(&self, overlay: &B256) -> Result<bool, PeerStoreError> {
        Ok(self.get(overlay)?.is_some())
    }

    /// Get the number of stored peers.
    fn count(&self) -> Result<usize, PeerStoreError>;

    /// Remove all peers from storage.
    fn clear(&self) -> Result<(), PeerStoreError>;

    /// Flush any buffered writes to persistent storage.
    ///
    /// Some implementations may buffer writes for performance.
    /// This ensures all data is persisted.
    fn flush(&self) -> Result<(), PeerStoreError> {
        Ok(()) // Default: no-op for implementations that don't buffer
    }
}

/// In-memory peer store for testing.
///
/// This implementation stores peers in a `HashMap` and does not persist
/// across restarts. Useful for unit tests and development.
#[derive(Debug, Default)]
pub struct MemoryPeerStore {
    peers: parking_lot::RwLock<std::collections::HashMap<B256, StoredPeer>>,
}

impl MemoryPeerStore {
    /// Create a new empty in-memory store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl PeerStore for MemoryPeerStore {
    fn load_all(&self) -> Result<Vec<StoredPeer>, PeerStoreError> {
        Ok(self.peers.read().values().cloned().collect())
    }

    fn save(&self, peer: &StoredPeer) -> Result<(), PeerStoreError> {
        self.peers.write().insert(peer.overlay(), peer.clone());
        Ok(())
    }

    fn save_batch(&self, peers: &[StoredPeer]) -> Result<(), PeerStoreError> {
        let mut store = self.peers.write();
        for peer in peers {
            store.insert(peer.overlay(), peer.clone());
        }
        Ok(())
    }

    fn remove(&self, overlay: &B256) -> Result<(), PeerStoreError> {
        self.peers.write().remove(overlay);
        Ok(())
    }

    fn get(&self, overlay: &B256) -> Result<Option<StoredPeer>, PeerStoreError> {
        Ok(self.peers.read().get(overlay).cloned())
    }

    fn count(&self) -> Result<usize, PeerStoreError> {
        Ok(self.peers.read().len())
    }

    fn clear(&self) -> Result<(), PeerStoreError> {
        self.peers.write().clear();
        Ok(())
    }
}

// ============================================================================
// File-based peer store
// ============================================================================

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;

/// File-based peer store using JSON serialization.
///
/// Stores peers in a single JSON file. The file is loaded entirely into memory
/// on startup and written back on flush/save operations.
///
/// # File Format
///
/// The file contains a JSON object mapping overlay addresses (hex-encoded) to
/// `StoredPeer` objects:
///
/// ```json
/// {
///   "0x1234...": { "overlay": "0x1234...", "multiaddrs": [...], ... },
///   "0x5678...": { "overlay": "0x5678...", "multiaddrs": [...], ... }
/// }
/// ```
///
/// # Thread Safety
///
/// All operations are protected by an RwLock, making this safe for concurrent access.
/// However, for high-throughput scenarios, consider batching writes.
pub struct FilePeerStore {
    /// Path to the JSON file.
    path: PathBuf,
    /// In-memory cache of peers.
    peers: parking_lot::RwLock<HashMap<B256, StoredPeer>>,
    /// Whether there are unsaved changes.
    dirty: parking_lot::Mutex<bool>,
}

impl std::fmt::Debug for FilePeerStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilePeerStore")
            .field("path", &self.path)
            .field("count", &self.peers.read().len())
            .field("dirty", &*self.dirty.lock())
            .finish()
    }
}

impl FilePeerStore {
    /// Create a new file-based store at the given path.
    ///
    /// If the file exists, it will be loaded. Otherwise, an empty store is created.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, PeerStoreError> {
        let path = path.into();
        let peers = if path.exists() {
            Self::load_from_file(&path)?
        } else {
            HashMap::new()
        };

        Ok(Self {
            path,
            peers: parking_lot::RwLock::new(peers),
            dirty: parking_lot::Mutex::new(false),
        })
    }

    /// Create a new file-based store, creating parent directories if needed.
    pub fn new_with_create_dir(path: impl Into<PathBuf>) -> Result<Self, PeerStoreError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        Self::new(path)
    }

    /// Load peers from a JSON file.
    fn load_from_file(path: &PathBuf) -> Result<HashMap<B256, StoredPeer>, PeerStoreError> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);

        // Deserialize as a map of hex string -> StoredPeer
        let raw: HashMap<String, StoredPeer> = serde_json::from_reader(reader)
            .map_err(|e| PeerStoreError::Serialization(e.to_string()))?;

        // Convert hex keys back to B256
        let mut peers = HashMap::with_capacity(raw.len());
        for (_, peer) in raw {
            peers.insert(peer.overlay(), peer);
        }

        Ok(peers)
    }

    /// Save peers to the JSON file.
    fn save_to_file(&self) -> Result<(), PeerStoreError> {
        let peers = self.peers.read();

        // Convert to hex-keyed map for JSON
        let raw: HashMap<String, &StoredPeer> = peers
            .iter()
            .map(|(k, v)| (format!("0x{}", hex::encode(k)), v))
            .collect();

        // Write to a temporary file first, then rename (atomic on most systems)
        let tmp_path = self.path.with_extension("json.tmp");
        {
            let file = File::create(&tmp_path)?;
            let writer = BufWriter::new(file);
            serde_json::to_writer_pretty(writer, &raw)
                .map_err(|e| PeerStoreError::Serialization(e.to_string()))?;
        }

        // Atomic rename
        fs::rename(&tmp_path, &self.path)?;

        Ok(())
    }

    /// Mark the store as dirty (has unsaved changes).
    fn mark_dirty(&self) {
        *self.dirty.lock() = true;
    }

    /// Check if there are unsaved changes.
    pub fn is_dirty(&self) -> bool {
        *self.dirty.lock()
    }

    /// Get the file path.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl PeerStore for FilePeerStore {
    fn load_all(&self) -> Result<Vec<StoredPeer>, PeerStoreError> {
        Ok(self.peers.read().values().cloned().collect())
    }

    fn save(&self, peer: &StoredPeer) -> Result<(), PeerStoreError> {
        self.peers.write().insert(peer.overlay(), peer.clone());
        self.mark_dirty();
        Ok(())
    }

    fn save_batch(&self, peers: &[StoredPeer]) -> Result<(), PeerStoreError> {
        let mut store = self.peers.write();
        for peer in peers {
            store.insert(peer.overlay(), peer.clone());
        }
        drop(store);
        self.mark_dirty();
        Ok(())
    }

    fn remove(&self, overlay: &B256) -> Result<(), PeerStoreError> {
        self.peers.write().remove(overlay);
        self.mark_dirty();
        Ok(())
    }

    fn get(&self, overlay: &B256) -> Result<Option<StoredPeer>, PeerStoreError> {
        Ok(self.peers.read().get(overlay).cloned())
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

impl Drop for FilePeerStore {
    fn drop(&mut self) {
        // Best-effort flush on drop
        if self.is_dirty() {
            let _ = self.save_to_file();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256, Signature, U256};

    fn test_overlay(n: u8) -> B256 {
        B256::repeat_byte(n)
    }

    fn test_signature() -> Signature {
        Signature::new(U256::from(1u64), U256::from(2u64), false)
    }

    fn test_ethereum_address(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn test_peer(n: u8) -> StoredPeer {
        StoredPeer::from_components(
            test_overlay(n),
            vec![format!("/ip4/127.0.0.{}/tcp/1634", n).parse().unwrap()],
            test_signature(),
            B256::repeat_byte(n),
            test_ethereum_address(n),
            true,
        )
    }

    #[test]
    fn test_memory_store_basic() {
        let store = MemoryPeerStore::new();

        // Initially empty
        assert_eq!(store.count().unwrap(), 0);
        assert!(store.load_all().unwrap().is_empty());

        // Save a peer
        let peer = test_peer(1);
        store.save(&peer).unwrap();

        assert_eq!(store.count().unwrap(), 1);
        assert!(store.contains(&peer.overlay()).unwrap());

        // Get the peer
        let loaded = store.get(&peer.overlay()).unwrap().unwrap();
        assert_eq!(loaded.overlay(), peer.overlay());

        // Remove the peer
        store.remove(&peer.overlay()).unwrap();
        assert_eq!(store.count().unwrap(), 0);
        assert!(!store.contains(&peer.overlay()).unwrap());
    }

    #[test]
    fn test_memory_store_batch() {
        let store = MemoryPeerStore::new();

        let peers: Vec<_> = (1..=5).map(test_peer).collect();
        store.save_batch(&peers).unwrap();

        assert_eq!(store.count().unwrap(), 5);

        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.len(), 5);
    }

    #[test]
    fn test_memory_store_update() {
        let store = MemoryPeerStore::new();

        let mut peer = test_peer(1);
        store.save(&peer).unwrap();

        // Update the peer's score
        peer.score.connection_successes = 10;
        store.save(&peer).unwrap();

        // Should still be 1 peer, but updated
        assert_eq!(store.count().unwrap(), 1);
        let loaded = store.get(&peer.overlay()).unwrap().unwrap();
        assert_eq!(loaded.score.connection_successes, 10);
    }

    #[test]
    fn test_memory_store_clear() {
        let store = MemoryPeerStore::new();

        let peers: Vec<_> = (1..=5).map(test_peer).collect();
        store.save_batch(&peers).unwrap();
        assert_eq!(store.count().unwrap(), 5);

        store.clear().unwrap();
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn test_file_store_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers.json");

        let store = FilePeerStore::new(&path).unwrap();

        // Initially empty
        assert_eq!(store.count().unwrap(), 0);
        assert!(!path.exists()); // Not created until flush

        // Save a peer
        let peer = test_peer(1);
        store.save(&peer).unwrap();
        assert!(store.is_dirty());

        // Flush to disk
        store.flush().unwrap();
        assert!(!store.is_dirty());
        assert!(path.exists());

        // Get the peer
        let loaded = store.get(&peer.overlay()).unwrap().unwrap();
        assert_eq!(loaded.overlay(), peer.overlay());
    }

    #[test]
    fn test_file_store_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers.json");

        // Create and populate store
        {
            let store = FilePeerStore::new(&path).unwrap();
            let peers: Vec<_> = (1..=5).map(test_peer).collect();
            store.save_batch(&peers).unwrap();
            store.flush().unwrap();
        }

        // Reload from disk
        {
            let store = FilePeerStore::new(&path).unwrap();
            assert_eq!(store.count().unwrap(), 5);

            for i in 1..=5 {
                let overlay = test_overlay(i);
                assert!(store.contains(&overlay).unwrap());
            }
        }
    }

    #[test]
    fn test_file_store_update() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers.json");

        let store = FilePeerStore::new(&path).unwrap();

        let mut peer = test_peer(1);
        store.save(&peer).unwrap();
        store.flush().unwrap();

        // Update the peer's score
        peer.score.connection_successes = 42;
        store.save(&peer).unwrap();
        store.flush().unwrap();

        // Reload and verify
        let store2 = FilePeerStore::new(&path).unwrap();
        let loaded = store2.get(&peer.overlay()).unwrap().unwrap();
        assert_eq!(loaded.score.connection_successes, 42);
    }

    #[test]
    fn test_file_store_remove() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers.json");

        let store = FilePeerStore::new(&path).unwrap();

        let peer = test_peer(1);
        store.save(&peer).unwrap();
        store.flush().unwrap();

        store.remove(&peer.overlay()).unwrap();
        store.flush().unwrap();

        // Reload and verify removed
        let store2 = FilePeerStore::new(&path).unwrap();
        assert_eq!(store2.count().unwrap(), 0);
    }

    #[test]
    fn test_file_store_drop_flushes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers.json");

        // Create, populate, and drop (should auto-flush)
        {
            let store = FilePeerStore::new(&path).unwrap();
            store.save(&test_peer(1)).unwrap();
            // Don't call flush() - drop should handle it
        }

        // Verify persisted
        let store = FilePeerStore::new(&path).unwrap();
        assert_eq!(store.count().unwrap(), 1);
    }
}
