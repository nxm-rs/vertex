//! Bidirectional registry mapping protocol IDs to libp2p PeerIds.

use std::collections::HashMap;

use libp2p::PeerId;
use parking_lot::RwLock;

use crate::traits::NetPeerId;

/// Result of a peer registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterResult {
    New,
    /// Old connection should be closed.
    Replaced {
        old_peer_id: PeerId,
    },
    /// Duplicate connection from same node.
    SamePeer,
}

/// Bidirectional Id ↔ PeerId mapping (all operations RwLock-protected).
#[derive(Debug)]
pub struct PeerRegistry<Id: NetPeerId> {
    id_to_peer: RwLock<HashMap<Id, PeerId>>,
    peer_to_id: RwLock<HashMap<PeerId, Id>>,
}

impl<Id: NetPeerId> Default for PeerRegistry<Id> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Id: NetPeerId> PeerRegistry<Id> {
    pub fn new() -> Self {
        Self {
            id_to_peer: RwLock::new(HashMap::new()),
            peer_to_id: RwLock::new(HashMap::new()),
        }
    }

    /// Register Id ↔ PeerId mapping. Returns what was replaced (if any).
    pub fn register(&self, id: Id, peer_id: PeerId) -> RegisterResult {
        let mut id_to_peer = self.id_to_peer.write();
        let mut peer_to_id = self.peer_to_id.write();

        let result = if let Some(old_peer) = id_to_peer.remove(&id) {
            peer_to_id.remove(&old_peer);
            if old_peer == peer_id {
                RegisterResult::SamePeer
            } else {
                RegisterResult::Replaced {
                    old_peer_id: old_peer,
                }
            }
        } else {
            RegisterResult::New
        };

        // Remove any existing mappings for this peer_id (peer changed overlay)
        if let Some(old_id) = peer_to_id.remove(&peer_id) {
            id_to_peer.remove(&old_id);
        }

        // Insert the new mapping
        id_to_peer.insert(id.clone(), peer_id);
        peer_to_id.insert(peer_id, id);

        result
    }

    pub fn remove_by_peer(&self, peer_id: &PeerId) -> Option<Id> {
        let mut id_to_peer = self.id_to_peer.write();
        let mut peer_to_id = self.peer_to_id.write();

        if let Some(id) = peer_to_id.remove(peer_id) {
            id_to_peer.remove(&id);
            Some(id)
        } else {
            None
        }
    }

    pub fn remove_by_id(&self, id: &Id) -> Option<PeerId> {
        let mut id_to_peer = self.id_to_peer.write();
        let mut peer_to_id = self.peer_to_id.write();

        if let Some(peer_id) = id_to_peer.remove(id) {
            peer_to_id.remove(&peer_id);
            Some(peer_id)
        } else {
            None
        }
    }

    pub fn resolve_peer(&self, id: &Id) -> Option<PeerId> {
        self.id_to_peer.read().get(id).copied()
    }

    pub fn resolve_id(&self, peer_id: &PeerId) -> Option<Id> {
        self.peer_to_id.read().get(peer_id).cloned()
    }

    pub fn contains_id(&self, id: &Id) -> bool {
        self.id_to_peer.read().contains_key(id)
    }

    pub fn contains_peer(&self, peer_id: &PeerId) -> bool {
        self.peer_to_id.read().contains_key(peer_id)
    }

    pub fn len(&self) -> usize {
        self.id_to_peer.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn ids(&self) -> Vec<Id> {
        self.id_to_peer.read().keys().cloned().collect()
    }

    pub fn peer_ids(&self) -> Vec<PeerId> {
        self.peer_to_id.read().keys().copied().collect()
    }

    pub fn clear(&self) {
        self.id_to_peer.write().clear();
        self.peer_to_id.write().clear();
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
    struct TestId(u64);

    fn test_peer_id(n: u8) -> PeerId {
        // Create a deterministic peer ID from a byte
        let bytes = [n; 32];
        let key = libp2p::identity::ed25519::SecretKey::try_from_bytes(bytes).unwrap();
        let keypair =
            libp2p::identity::Keypair::from(libp2p::identity::ed25519::Keypair::from(key));
        keypair.public().to_peer_id()
    }

    #[test]
    fn test_registry_basic() {
        let registry = PeerRegistry::new();
        let id = TestId(1);
        let peer_id = test_peer_id(1);

        assert!(registry.is_empty());

        let result = registry.register(id, peer_id);
        assert_eq!(result, RegisterResult::New);
        assert_eq!(registry.len(), 1);
        assert!(registry.contains_id(&id));
        assert!(registry.contains_peer(&peer_id));

        assert_eq!(registry.resolve_peer(&id), Some(peer_id));
        assert_eq!(registry.resolve_id(&peer_id), Some(id));
    }

    #[test]
    fn test_registry_same_peer_reconnect() {
        let registry = PeerRegistry::new();
        let id = TestId(1);
        let peer_id = test_peer_id(1);

        // First registration
        let result = registry.register(id, peer_id);
        assert_eq!(result, RegisterResult::New);

        // Same ID, same PeerId - duplicate connection
        let result = registry.register(id, peer_id);
        assert_eq!(result, RegisterResult::SamePeer);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_registry_replaced_different_peer() {
        let registry = PeerRegistry::new();

        let id = TestId(1);
        let peer1 = test_peer_id(1);
        let peer2 = test_peer_id(2);

        // Register first mapping
        let result = registry.register(id, peer1);
        assert_eq!(result, RegisterResult::New);
        assert_eq!(registry.len(), 1);

        // Same ID, different PeerId - replacement
        let result = registry.register(id, peer2);
        assert_eq!(result, RegisterResult::Replaced { old_peer_id: peer1 });
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.resolve_peer(&id), Some(peer2));
        assert!(!registry.contains_peer(&peer1));
    }

    #[test]
    fn test_registry_peer_changes_overlay() {
        let registry = PeerRegistry::new();

        let id1 = TestId(1);
        let id2 = TestId(2);
        let peer = test_peer_id(1);

        // Register first mapping
        let result = registry.register(id1, peer);
        assert_eq!(result, RegisterResult::New);
        assert_eq!(registry.len(), 1);

        // Same PeerId, different ID - peer changed overlay
        let result = registry.register(id2, peer);
        assert_eq!(result, RegisterResult::New); // New ID, so it's "New"
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.resolve_id(&peer), Some(id2));
        assert!(!registry.contains_id(&id1));
    }

    #[test]
    fn test_registry_remove_by_peer() {
        let registry = PeerRegistry::new();
        let id = TestId(1);
        let peer_id = test_peer_id(1);

        let _ = registry.register(id, peer_id);

        let removed = registry.remove_by_peer(&peer_id);
        assert_eq!(removed, Some(id));
        assert!(registry.is_empty());
    }

    #[test]
    fn test_registry_remove_by_id() {
        let registry = PeerRegistry::new();
        let id = TestId(1);
        let peer_id = test_peer_id(1);

        let _ = registry.register(id, peer_id);

        let removed = registry.remove_by_id(&id);
        assert_eq!(removed, Some(peer_id));
        assert!(registry.is_empty());
    }

    #[test]
    fn test_registry_multiple() {
        let registry = PeerRegistry::new();

        for i in 1..=5 {
            let result = registry.register(TestId(i), test_peer_id(i as u8));
            assert_eq!(result, RegisterResult::New);
        }

        assert_eq!(registry.len(), 5);
        assert_eq!(registry.ids().len(), 5);
        assert_eq!(registry.peer_ids().len(), 5);

        registry.clear();
        assert!(registry.is_empty());
    }
}
