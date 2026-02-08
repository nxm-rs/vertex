//! Generic peer connection registry.

use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;
use std::time::Instant;

use libp2p::{Multiaddr, PeerId, swarm::ConnectionId};
use parking_lot::RwLock;

use crate::direction::ConnectionDirection;
use crate::result::ActivateResult;
use crate::state::ConnectionState;

/// Generic peer connection registry.
///
/// Tracks connection lifecycle without protocol-specific knowledge.
/// `Id` is the peer identifier type (e.g., OverlayAddress for Swarm).
pub struct PeerRegistry<Id> {
    by_id: RwLock<HashMap<Id, ConnectionState<Id>>>,
    peer_to_id: RwLock<HashMap<PeerId, Id>>,
    conn_to_id: RwLock<HashMap<ConnectionId, Id>>,
}

impl<Id> Default for PeerRegistry<Id> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Id> PeerRegistry<Id> {
    pub fn new() -> Self {
        Self {
            by_id: RwLock::new(HashMap::new()),
            peer_to_id: RwLock::new(HashMap::new()),
            conn_to_id: RwLock::new(HashMap::new()),
        }
    }
}

impl<Id: Clone + Eq + Hash + Debug> PeerRegistry<Id> {
    pub fn get(&self, id: &Id) -> Option<ConnectionState<Id>> {
        self.by_id.read().get(id).cloned()
    }

    pub fn active_connection_id(&self, id: &Id) -> Option<ConnectionId> {
        self.by_id.read().get(id).and_then(|s| {
            if let ConnectionState::Active { connection_id, .. } = s {
                Some(*connection_id)
            } else {
                None
            }
        })
    }

    pub fn resolve_id(&self, peer_id: &PeerId) -> Option<Id> {
        self.peer_to_id.read().get(peer_id).cloned()
    }

    pub fn contains_peer(&self, peer_id: &PeerId) -> bool {
        self.peer_to_id.read().contains_key(peer_id)
    }

    pub fn resolve_peer_id(&self, id: &Id) -> Option<PeerId> {
        self.by_id.read().get(id).map(|s| s.peer_id())
    }

    /// Start dialing a peer with known ID. Returns all addresses for DialOpts.
    pub fn start_dial(
        &self,
        peer_id: PeerId,
        id: Id,
        addrs: Vec<Multiaddr>,
    ) -> Option<Vec<Multiaddr>> {
        if addrs.is_empty() {
            return None;
        }

        let mut by_id = self.by_id.write();
        let mut peer_to_id = self.peer_to_id.write();

        if by_id.contains_key(&id) || peer_to_id.contains_key(&peer_id) {
            return None;
        }

        let state = ConnectionState::Dialing {
            peer_id,
            id: Some(id.clone()),
            addrs: addrs.clone(),
            started_at: Instant::now(),
        };

        by_id.insert(id.clone(), state);
        peer_to_id.insert(peer_id, id);

        Some(addrs)
    }

    /// Start dialing without known ID (for bootnodes/commands).
    /// Uses a sentinel ID created by the provided function.
    pub fn start_dial_unknown_id<F>(
        &self,
        peer_id: PeerId,
        addrs: Vec<Multiaddr>,
        sentinel_fn: F,
    ) -> Option<Vec<Multiaddr>>
    where
        F: FnOnce(&PeerId) -> Id,
    {
        if addrs.is_empty() {
            return None;
        }

        let mut peer_to_id = self.peer_to_id.write();
        if peer_to_id.contains_key(&peer_id) {
            return None;
        }

        let sentinel = sentinel_fn(&peer_id);

        let state = ConnectionState::Dialing {
            peer_id,
            id: None,
            addrs: addrs.clone(),
            started_at: Instant::now(),
        };

        self.by_id.write().insert(sentinel.clone(), state);
        peer_to_id.insert(peer_id, sentinel);

        Some(addrs)
    }

    /// Complete a dial attempt (success or failure). Returns state for diagnostics.
    pub fn complete_dial(&self, peer_id: &PeerId) -> Option<ConnectionState<Id>> {
        let id = self.peer_to_id.write().remove(peer_id)?;
        self.by_id.write().remove(&id)
    }

    /// Transition from Dialing to Handshaking after TCP/QUIC connection established.
    pub fn connection_established(
        &self,
        peer_id: PeerId,
        connection_id: ConnectionId,
    ) -> Option<ConnectionState<Id>> {
        let id = self.peer_to_id.read().get(&peer_id)?.clone();

        let mut by_id = self.by_id.write();
        let state = by_id.remove(&id)?;

        let ConnectionState::Dialing {
            id: dial_id,
            started_at,
            ..
        } = state
        else {
            by_id.insert(id, state);
            return None;
        };

        let new_state = ConnectionState::Handshaking {
            peer_id,
            connection_id,
            id: dial_id,
            direction: ConnectionDirection::Outbound,
            started_at,
        };

        by_id.insert(id.clone(), new_state.clone());
        drop(by_id);

        self.conn_to_id.write().insert(connection_id, id);

        Some(new_state)
    }

    /// Handle inbound connection (goes directly to Handshaking).
    /// Uses a sentinel ID created by the provided function.
    pub fn inbound_connection<F>(
        &self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        sentinel_fn: F,
    ) -> ConnectionState<Id>
    where
        F: FnOnce(&PeerId) -> Id,
    {
        let sentinel = sentinel_fn(&peer_id);

        let state = ConnectionState::Handshaking {
            peer_id,
            connection_id,
            id: None,
            direction: ConnectionDirection::Inbound,
            started_at: Instant::now(),
        };

        self.by_id.write().insert(sentinel.clone(), state.clone());
        self.peer_to_id.write().insert(peer_id, sentinel.clone());
        self.conn_to_id.write().insert(connection_id, sentinel);

        state
    }

    /// Complete handshake and transition to Active state.
    /// Returns Replaced if there was an existing connection that should be closed.
    pub fn handshake_completed(
        &self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        id: Id,
    ) -> ActivateResult<Id> {
        let mut by_id = self.by_id.write();
        let mut peer_to_id = self.peer_to_id.write();
        let mut conn_to_id = self.conn_to_id.write();

        // Check if this ID already has an ACTIVE connection
        if let Some(ConnectionState::Active {
            peer_id: active_peer_id,
            connection_id: active_conn_id,
            ..
        }) = by_id.get(&id)
        {
            let old_peer_id = *active_peer_id;
            let old_conn_id = *active_conn_id;

            // Remove old connection mappings
            peer_to_id.remove(&old_peer_id);
            conn_to_id.remove(&old_conn_id);

            // Clean up the sentinel entry from handshaking if present
            if let Some(sentinel) = peer_to_id.get(&peer_id).filter(|s| *s != &id) {
                by_id.remove(sentinel);
            }

            let new_state = ConnectionState::Active {
                peer_id,
                id: id.clone(),
                connection_id,
                connected_at: Instant::now(),
            };

            by_id.insert(id.clone(), new_state);
            peer_to_id.insert(peer_id, id.clone());
            conn_to_id.insert(connection_id, id);

            return ActivateResult::Replaced {
                old_peer_id,
                old_connection_id: old_conn_id,
                old_id: None,
            };
        }

        // Check if this PeerId is already ACTIVE with a different ID
        if let Some((existing_id, old_conn_id)) = peer_to_id
            .get(&peer_id)
            .filter(|eid| *eid != &id)
            .and_then(|eid| {
                if let Some(ConnectionState::Active {
                    connection_id: conn_id,
                    ..
                }) = by_id.get(eid)
                {
                    Some((eid.clone(), *conn_id))
                } else {
                    None
                }
            })
        {
            by_id.remove(&existing_id);
            conn_to_id.remove(&old_conn_id);
            peer_to_id.remove(&peer_id);

            let new_state = ConnectionState::Active {
                peer_id,
                id: id.clone(),
                connection_id,
                connected_at: Instant::now(),
            };

            by_id.insert(id.clone(), new_state);
            peer_to_id.insert(peer_id, id.clone());
            conn_to_id.insert(connection_id, id);

            return ActivateResult::Replaced {
                old_peer_id: peer_id,
                old_connection_id: old_conn_id,
                old_id: Some(existing_id),
            };
        }

        // Normal case: transition from Handshaking to Active
        // Clean up the sentinel entry from handshaking
        if let Some(old_conn_id) = peer_to_id
            .get(&peer_id)
            .filter(|oid| *oid != &id)
            .and_then(|old_id| by_id.remove(old_id))
            .and_then(|old_state| old_state.connection_id())
        {
            conn_to_id.remove(&old_conn_id);
        }

        let new_state = ConnectionState::Active {
            peer_id,
            id: id.clone(),
            connection_id,
            connected_at: Instant::now(),
        };

        by_id.insert(id.clone(), new_state);
        peer_to_id.insert(peer_id, id.clone());
        conn_to_id.insert(connection_id, id);

        ActivateResult::Accepted
    }

    pub fn get_by_peer_id(&self, peer_id: &PeerId) -> Option<ConnectionState<Id>> {
        let id = self.peer_to_id.read().get(peer_id)?.clone();
        self.by_id.read().get(&id).cloned()
    }

    pub fn complete_dial_by_peer_id(&self, peer_id: &PeerId) -> Option<ConnectionState<Id>> {
        let id = self.peer_to_id.write().remove(peer_id)?;
        self.remove_by_id_inner(id)
    }

    pub fn active_ids(&self) -> Vec<Id> {
        self.by_id
            .read()
            .iter()
            .filter_map(|(id, state)| matches!(state, ConnectionState::Active { .. }).then_some(id.clone()))
            .collect()
    }

    /// Count of active connections.
    pub fn active_count(&self) -> usize {
        self.by_id
            .read()
            .values()
            .filter(|state| matches!(state, ConnectionState::Active { .. }))
            .count()
    }

    /// Count of pending connections (dialing + handshaking).
    pub fn pending_count(&self) -> usize {
        self.by_id
            .read()
            .values()
            .filter(|state| {
                matches!(
                    state,
                    ConnectionState::Dialing { .. } | ConnectionState::Handshaking { .. }
                )
            })
            .count()
    }

    pub fn disconnected(&self, peer_id: &PeerId) -> Option<ConnectionState<Id>> {
        let id = self.peer_to_id.write().remove(peer_id)?;
        self.remove_by_id_inner(id)
    }

    fn remove_by_id_inner(&self, id: Id) -> Option<ConnectionState<Id>> {
        let state = self.by_id.write().remove(&id)?;

        self.peer_to_id.write().remove(&state.peer_id());
        if let Some(conn_id) = state.connection_id() {
            self.conn_to_id.write().remove(&conn_id);
        }

        Some(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    struct TestId(u64);

    fn test_peer_id(n: u8) -> PeerId {
        let bytes = [n; 32];
        let key = libp2p::identity::ed25519::SecretKey::try_from_bytes(bytes)
            .expect("32 bytes is valid ed25519 secret key");
        let keypair = libp2p::identity::Keypair::from(libp2p::identity::ed25519::Keypair::from(key));
        keypair.public().to_peer_id()
    }

    fn test_addr(port: u16) -> Multiaddr {
        format!("/ip4/127.0.0.1/tcp/{}", port).parse().unwrap()
    }

    fn test_connection_id(n: u8) -> ConnectionId {
        ConnectionId::new_unchecked(n as usize)
    }

    fn sentinel_fn(peer_id: &PeerId) -> TestId {
        let bytes = peer_id.to_bytes();
        TestId(bytes[0] as u64)
    }

    #[test]
    fn test_start_dial() {
        let registry = PeerRegistry::<TestId>::new();
        let id = TestId(1);
        let peer_id = test_peer_id(1);
        let addrs = vec![test_addr(9000), test_addr(9001)];

        let result = registry.start_dial(peer_id, id.clone(), addrs.clone());
        assert_eq!(result, Some(addrs));
        assert!(registry.get(&id).is_some());
    }

    #[test]
    fn test_start_dial_duplicate() {
        let registry = PeerRegistry::<TestId>::new();
        let id = TestId(1);
        let peer_id = test_peer_id(1);
        let addrs = vec![test_addr(9000)];

        let result1 = registry.start_dial(peer_id, id.clone(), addrs.clone());
        assert!(result1.is_some());

        let result2 = registry.start_dial(peer_id, id, addrs);
        assert!(result2.is_none());
    }

    #[test]
    fn test_complete_dial() {
        let registry = PeerRegistry::<TestId>::new();
        let id = TestId(1);
        let peer_id = test_peer_id(1);
        let addrs = vec![test_addr(9000), test_addr(9001), test_addr(9002)];

        registry.start_dial(peer_id, id.clone(), addrs.clone());

        let state = registry.complete_dial(&peer_id);
        assert!(state.is_some());
        let state = state.unwrap();
        assert_eq!(state.addrs(), Some(&addrs));

        assert!(registry.get(&id).is_none());
        assert!(registry.resolve_id(&peer_id).is_none());
    }

    #[test]
    fn test_connection_established() {
        let registry = PeerRegistry::<TestId>::new();
        let id = TestId(1);
        let peer_id = test_peer_id(1);
        let conn_id = test_connection_id(1);

        registry.start_dial(peer_id, id.clone(), vec![test_addr(9000)]);

        let state = registry.connection_established(peer_id, conn_id);
        assert!(state.is_some());

        let state = state.unwrap();
        assert_eq!(state.peer_id(), peer_id);
        assert!(matches!(state, ConnectionState::Handshaking { .. }));

        assert_eq!(registry.resolve_id(&peer_id), Some(id));
    }

    #[test]
    fn test_handshake_completed_new_peer() {
        let registry = PeerRegistry::<TestId>::new();
        let id = TestId(1);
        let peer_id = test_peer_id(1);
        let conn_id = test_connection_id(1);

        registry.start_dial(peer_id, id.clone(), vec![test_addr(9000)]);
        registry.connection_established(peer_id, conn_id);

        let result = registry.handshake_completed(peer_id, conn_id, id.clone());
        assert_eq!(result, ActivateResult::Accepted);

        let state = registry.get(&id).unwrap();
        assert!(matches!(state, ConnectionState::Active { .. }));
    }

    #[test]
    fn test_handshake_replaces_old_connection() {
        let registry = PeerRegistry::<TestId>::new();
        let id = TestId(1);
        let peer_id = test_peer_id(1);
        let conn_id1 = test_connection_id(1);
        let conn_id2 = test_connection_id(2);

        registry.start_dial(peer_id, id.clone(), vec![test_addr(9000)]);
        registry.connection_established(peer_id, conn_id1);
        let result1 = registry.handshake_completed(peer_id, conn_id1, id.clone());
        assert_eq!(result1, ActivateResult::Accepted);

        let result2 = registry.handshake_completed(peer_id, conn_id2, id);
        assert!(matches!(
            result2,
            ActivateResult::Replaced {
                old_connection_id,
                ..
            } if old_connection_id == conn_id1
        ));
    }

    #[test]
    fn test_inbound_connection() {
        let registry = PeerRegistry::<TestId>::new();
        let peer_id = test_peer_id(1);
        let conn_id = test_connection_id(1);

        let state = registry.inbound_connection(peer_id, conn_id, sentinel_fn);
        assert!(matches!(
            state,
            ConnectionState::Handshaking {
                direction: ConnectionDirection::Inbound,
                ..
            }
        ));

        assert!(registry.resolve_id(&peer_id).is_some());
    }

    #[test]
    fn test_disconnected() {
        let registry = PeerRegistry::<TestId>::new();
        let id = TestId(1);
        let peer_id = test_peer_id(1);
        let conn_id = test_connection_id(1);

        registry.start_dial(peer_id, id.clone(), vec![test_addr(9000)]);
        registry.connection_established(peer_id, conn_id);
        registry.handshake_completed(peer_id, conn_id, id.clone());

        let state = registry.disconnected(&peer_id);
        assert!(state.is_some());
        assert!(registry.get(&id).is_none());
        assert_eq!(registry.resolve_id(&peer_id), None);
    }

    #[test]
    fn test_active_count() {
        let registry = PeerRegistry::<TestId>::new();

        let id1 = TestId(1);
        let peer_id1 = test_peer_id(1);
        registry.start_dial(peer_id1, id1.clone(), vec![test_addr(9000)]);

        let id2 = TestId(2);
        let peer_id2 = test_peer_id(2);
        let conn_id2 = test_connection_id(2);
        registry.start_dial(peer_id2, id2.clone(), vec![test_addr(9001)]);
        registry.connection_established(peer_id2, conn_id2);
        registry.handshake_completed(peer_id2, conn_id2, id2);

        assert_eq!(registry.pending_count(), 1);
        assert_eq!(registry.active_count(), 1);
    }
}
