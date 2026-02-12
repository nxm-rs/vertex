//! Generic peer connection registry.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Debug;
use std::hash::Hash;
use std::time::Instant;

use libp2p::{Multiaddr, PeerId, swarm::ConnectionId};
use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::direction::ConnectionDirection;
use crate::result::ActivateResult;
use crate::state::ConnectionState;

/// Registry key distinguishing known-ID from pending connections.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum RegistryKey<Id> {
    /// Peer with known application-level ID.
    Known(Id),
    /// Pending connection (inbound or bootnode dial) awaiting ID from handshake.
    Pending(PeerId),
}

/// Entry in the pending time index to be removed.
struct PendingEntry<Id> {
    key: RegistryKey<Id>,
    started_at: Instant,
}

/// What existing state to replace during handshake completion.
enum Replacement<Id> {
    /// ID already has active connection - replace it.
    ById {
        old_peer_id: PeerId,
        old_conn_id: ConnectionId,
        pending_entry: Option<PendingEntry<Id>>,
    },
    /// PeerId active with different ID - replace it.
    ByPeerId {
        existing_id: Id,
        old_conn_id: ConnectionId,
    },
    /// Normal case - just clean up pending entry if any.
    None {
        pending_entry: Option<PendingEntry<Id>>,
    },
}

/// Inner maps protected by a single lock.
struct Maps<Id> {
    by_key: HashMap<RegistryKey<Id>, ConnectionState<Id>>,
    peer_to_key: HashMap<PeerId, RegistryKey<Id>>,
    conn_to_key: HashMap<ConnectionId, RegistryKey<Id>>,
    /// Pending connections indexed by start time for O(log n + k) stale detection.
    pending_by_time: BTreeMap<Instant, HashSet<RegistryKey<Id>>>,
}

impl<Id> Default for Maps<Id> {
    fn default() -> Self {
        Self {
            by_key: HashMap::new(),
            peer_to_key: HashMap::new(),
            conn_to_key: HashMap::new(),
            pending_by_time: BTreeMap::new(),
        }
    }
}

impl<Id: Clone + Eq + Hash> Maps<Id> {
    fn add_pending(&mut self, started_at: Instant, key: RegistryKey<Id>) {
        self.pending_by_time
            .entry(started_at)
            .or_default()
            .insert(key);
    }

    fn remove_pending(&mut self, started_at: Instant, key: &RegistryKey<Id>) {
        if let Some(keys) = self.pending_by_time.get_mut(&started_at) {
            keys.remove(key);
            if keys.is_empty() {
                self.pending_by_time.remove(&started_at);
            }
        }
    }
}

/// Generic peer connection registry.
///
/// Tracks connection lifecycle without protocol-specific knowledge.
/// `Id` is the peer identifier type (e.g., OverlayAddress for Swarm).
pub struct PeerRegistry<Id> {
    maps: RwLock<Maps<Id>>,
}

impl<Id> Default for PeerRegistry<Id> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Id> PeerRegistry<Id> {
    pub fn new() -> Self {
        Self {
            maps: RwLock::new(Maps::default()),
        }
    }

    fn read(&self) -> RwLockReadGuard<'_, Maps<Id>> {
        self.maps.read()
    }

    fn write(&self) -> RwLockWriteGuard<'_, Maps<Id>> {
        self.maps.write()
    }
}

impl<Id: Clone + Eq + Hash + Debug> PeerRegistry<Id> {
    pub fn get(&self, id: &Id) -> Option<ConnectionState<Id>> {
        self.read()
            .by_key
            .get(&RegistryKey::Known(id.clone()))
            .cloned()
    }

    pub fn active_connection_id(&self, id: &Id) -> Option<ConnectionId> {
        self.read()
            .by_key
            .get(&RegistryKey::Known(id.clone()))
            .and_then(|s| {
                if let ConnectionState::Active { connection_id, .. } = s {
                    Some(*connection_id)
                } else {
                    None
                }
            })
    }

    /// Resolve a PeerId to its application-level ID (only if known).
    pub fn resolve_id(&self, peer_id: &PeerId) -> Option<Id> {
        match self.read().peer_to_key.get(peer_id)? {
            RegistryKey::Known(id) => Some(id.clone()),
            RegistryKey::Pending(_) => None,
        }
    }

    pub fn contains_peer(&self, peer_id: &PeerId) -> bool {
        self.read().peer_to_key.contains_key(peer_id)
    }

    pub fn resolve_peer_id(&self, id: &Id) -> Option<PeerId> {
        self.read()
            .by_key
            .get(&RegistryKey::Known(id.clone()))
            .map(|s| s.peer_id())
    }

    /// Start dialing a peer with known ID. Returns addresses for DialOpts.
    #[must_use]
    pub fn start_dial(
        &self,
        peer_id: PeerId,
        id: Id,
        addrs: Vec<Multiaddr>,
    ) -> Option<Vec<Multiaddr>> {
        if addrs.is_empty() {
            return None;
        }

        let mut maps = self.write();
        let key = RegistryKey::Known(id.clone());

        if maps.by_key.contains_key(&key) || maps.peer_to_key.contains_key(&peer_id) {
            return None;
        }

        let started_at = Instant::now();
        let state = ConnectionState::Dialing {
            peer_id,
            id: Some(id),
            addrs: addrs.clone(),
            started_at,
        };

        maps.by_key.insert(key.clone(), state);
        maps.peer_to_key.insert(peer_id, key.clone());
        maps.add_pending(started_at, key);

        Some(addrs)
    }

    /// Start dialing without known ID (for bootnodes/commands).
    #[must_use]
    pub fn start_dial_unknown_id(
        &self,
        peer_id: PeerId,
        addrs: Vec<Multiaddr>,
    ) -> Option<Vec<Multiaddr>> {
        if addrs.is_empty() {
            return None;
        }

        let mut maps = self.write();

        if maps.peer_to_key.contains_key(&peer_id) {
            return None;
        }

        let key = RegistryKey::Pending(peer_id);
        let started_at = Instant::now();
        let state = ConnectionState::Dialing {
            peer_id,
            id: None,
            addrs: addrs.clone(),
            started_at,
        };

        maps.by_key.insert(key.clone(), state);
        maps.peer_to_key.insert(peer_id, key.clone());
        maps.add_pending(started_at, key);

        Some(addrs)
    }

    /// Complete a dial attempt (success or failure). Returns state for diagnostics.
    pub fn complete_dial(&self, peer_id: &PeerId) -> Option<ConnectionState<Id>> {
        self.disconnected(peer_id)
    }

    /// Transition from Dialing to Handshaking after connection established.
    pub fn connection_established(
        &self,
        peer_id: PeerId,
        connection_id: ConnectionId,
    ) -> Option<ConnectionState<Id>> {
        let mut maps = self.write();

        let key = maps.peer_to_key.get(&peer_id)?.clone();
        let state = maps.by_key.remove(&key)?;

        let ConnectionState::Dialing {
            id: dial_id,
            started_at,
            ..
        } = state
        else {
            maps.by_key.insert(key, state);
            return None;
        };

        let new_state = ConnectionState::Handshaking {
            peer_id,
            connection_id,
            id: dial_id,
            direction: ConnectionDirection::Outbound,
            started_at,
        };

        maps.by_key.insert(key.clone(), new_state.clone());
        maps.conn_to_key.insert(connection_id, key);

        Some(new_state)
    }

    /// Register inbound connection in Handshaking state.
    pub fn inbound_connection(
        &self,
        peer_id: PeerId,
        connection_id: ConnectionId,
    ) -> ConnectionState<Id> {
        let key = RegistryKey::Pending(peer_id);
        let started_at = Instant::now();

        let state = ConnectionState::Handshaking {
            peer_id,
            connection_id,
            id: None,
            direction: ConnectionDirection::Inbound,
            started_at,
        };

        let mut maps = self.write();
        maps.by_key.insert(key.clone(), state.clone());
        maps.peer_to_key.insert(peer_id, key.clone());
        maps.conn_to_key.insert(connection_id, key.clone());
        maps.add_pending(started_at, key);

        state
    }

    /// Transition to Active state, replacing existing connection if present.
    pub fn handshake_completed(
        &self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        id: Id,
    ) -> ActivateResult<Id> {
        let mut maps = self.write();

        let known_key = RegistryKey::Known(id.clone());
        let pending_key = RegistryKey::Pending(peer_id);
        let replacement = Self::find_replacement(&maps, &id, &peer_id);

        // Remove pending time index for the known key if it was pending
        if let Some(started_at) = maps.by_key.get(&known_key).and_then(|s| s.started_at()) {
            maps.remove_pending(started_at, &known_key);
        }

        let result = match replacement {
            Replacement::ById {
                old_peer_id,
                old_conn_id,
                pending_entry,
            } => {
                maps.peer_to_key.remove(&old_peer_id);
                maps.conn_to_key.remove(&old_conn_id);
                if let Some(entry) = pending_entry {
                    maps.by_key.remove(&entry.key);
                    maps.remove_pending(entry.started_at, &entry.key);
                }
                ActivateResult::Replaced {
                    old_peer_id,
                    old_connection_id: old_conn_id,
                    old_id: None,
                }
            }
            Replacement::ByPeerId {
                existing_id,
                old_conn_id,
            } => {
                maps.by_key.remove(&RegistryKey::Known(existing_id.clone()));
                maps.conn_to_key.remove(&old_conn_id);
                maps.peer_to_key.remove(&peer_id);
                ActivateResult::Replaced {
                    old_peer_id: peer_id,
                    old_connection_id: old_conn_id,
                    old_id: Some(existing_id),
                }
            }
            Replacement::None { pending_entry } => {
                if let Some(entry) = pending_entry {
                    if let Some(state) = maps.by_key.remove(&entry.key) {
                        if let Some(old_conn_id) = state.connection_id() {
                            maps.conn_to_key.remove(&old_conn_id);
                        }
                    }
                    maps.remove_pending(entry.started_at, &entry.key);
                }
                // Also clean up the pending key entry if it exists
                if maps.by_key.contains_key(&pending_key) {
                    if let Some(state) = maps.by_key.remove(&pending_key) {
                        if let Some(started_at) = state.started_at() {
                            maps.remove_pending(started_at, &pending_key);
                        }
                    }
                }
                ActivateResult::Accepted
            }
        };

        let new_state = ConnectionState::Active {
            peer_id,
            id: id.clone(),
            connection_id,
            connected_at: Instant::now(),
        };
        maps.by_key.insert(known_key.clone(), new_state);
        maps.peer_to_key.insert(peer_id, known_key.clone());
        maps.conn_to_key.insert(connection_id, known_key);

        result
    }

    fn find_replacement(maps: &Maps<Id>, id: &Id, peer_id: &PeerId) -> Replacement<Id> {
        let known_key = RegistryKey::Known(id.clone());
        let pending_key = RegistryKey::Pending(*peer_id);

        // Helper to create PendingEntry from a key if state is pending
        let pending_entry = |key: &RegistryKey<Id>| -> Option<PendingEntry<Id>> {
            maps.by_key.get(key).and_then(|state| {
                state.started_at().map(|started_at| PendingEntry {
                    key: key.clone(),
                    started_at,
                })
            })
        };

        // Case 1: ID already has an active connection
        if let Some(ConnectionState::Active {
            peer_id: active_peer_id,
            connection_id: active_conn_id,
            ..
        }) = maps.by_key.get(&known_key)
        {
            let entry = maps
                .peer_to_key
                .get(peer_id)
                .filter(|k| **k != known_key)
                .and_then(pending_entry);
            return Replacement::ById {
                old_peer_id: *active_peer_id,
                old_conn_id: *active_conn_id,
                pending_entry: entry,
            };
        }

        // Case 2: PeerId already active with different ID
        if let Some(key) = maps.peer_to_key.get(peer_id) {
            if let RegistryKey::Known(existing_id) = key {
                if existing_id != id {
                    if let Some(ConnectionState::Active { connection_id, .. }) =
                        maps.by_key.get(key)
                    {
                        return Replacement::ByPeerId {
                            existing_id: existing_id.clone(),
                            old_conn_id: *connection_id,
                        };
                    }
                }
            }
        }

        // Case 3: Normal handshake, clean up pending entry
        let entry = pending_entry(&pending_key);
        Replacement::None {
            pending_entry: entry,
        }
    }

    pub fn get_by_peer_id(&self, peer_id: &PeerId) -> Option<ConnectionState<Id>> {
        let maps = self.read();
        let key = maps.peer_to_key.get(peer_id)?;
        maps.by_key.get(key).cloned()
    }

    #[must_use]
    pub fn active_ids(&self) -> Vec<Id> {
        self.read()
            .by_key
            .iter()
            .filter_map(|(key, state)| {
                if let RegistryKey::Known(id) = key {
                    matches!(state, ConnectionState::Active { .. }).then_some(id.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Count of active connections.
    pub fn active_count(&self) -> usize {
        self.read()
            .by_key
            .values()
            .filter(|state| matches!(state, ConnectionState::Active { .. }))
            .count()
    }

    /// Count of pending connections (dialing + handshaking).
    pub fn pending_count(&self) -> usize {
        self.read()
            .by_key
            .values()
            .filter(|state| {
                matches!(
                    state,
                    ConnectionState::Dialing { .. } | ConnectionState::Handshaking { .. }
                )
            })
            .count()
    }

    /// Get PeerIds of pending connections that have exceeded the timeout.
    ///
    /// Uses time-indexed lookup for O(log n + k) complexity where k = stale count.
    #[must_use]
    pub fn stale_pending(&self, timeout: std::time::Duration) -> Vec<PeerId> {
        let Some(cutoff) = Instant::now().checked_sub(timeout) else {
            return Vec::new();
        };
        let maps = self.read();

        maps.pending_by_time
            .range(..=cutoff)
            .flat_map(|(_, keys)| keys.iter())
            .filter_map(|key| maps.by_key.get(key).map(|s| s.peer_id()))
            .collect()
    }

    /// Remove peer from all maps and return final state.
    pub fn disconnected(&self, peer_id: &PeerId) -> Option<ConnectionState<Id>> {
        let mut maps = self.write();

        let key = maps.peer_to_key.remove(peer_id)?;
        let state = maps.by_key.remove(&key)?;

        if let Some(conn_id) = state.connection_id() {
            maps.conn_to_key.remove(&conn_id);
        }

        if let Some(started_at) = state.started_at() {
            maps.remove_pending(started_at, &key);
        }

        Some(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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

        let _ = registry.start_dial(peer_id, id.clone(), addrs.clone());

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

        let _ = registry.start_dial(peer_id, id.clone(), vec![test_addr(9000)]);

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

        let _ = registry.start_dial(peer_id, id.clone(), vec![test_addr(9000)]);
        let _ = registry.connection_established(peer_id, conn_id);

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

        let _ = registry.start_dial(peer_id, id.clone(), vec![test_addr(9000)]);
        let _ = registry.connection_established(peer_id, conn_id1);
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

        let state = registry.inbound_connection(peer_id, conn_id);
        assert!(matches!(
            state,
            ConnectionState::Handshaking {
                direction: ConnectionDirection::Inbound,
                ..
            }
        ));

        // resolve_id returns None for pending connections (ID not yet known)
        assert!(registry.resolve_id(&peer_id).is_none());
        // But the peer is tracked
        assert!(registry.contains_peer(&peer_id));
    }

    #[test]
    fn test_disconnected() {
        let registry = PeerRegistry::<TestId>::new();
        let id = TestId(1);
        let peer_id = test_peer_id(1);
        let conn_id = test_connection_id(1);

        let _ = registry.start_dial(peer_id, id.clone(), vec![test_addr(9000)]);
        let _ = registry.connection_established(peer_id, conn_id);
        let _ = registry.handshake_completed(peer_id, conn_id, id.clone());

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
        let _ = registry.start_dial(peer_id1, id1.clone(), vec![test_addr(9000)]);

        let id2 = TestId(2);
        let peer_id2 = test_peer_id(2);
        let conn_id2 = test_connection_id(2);
        let _ = registry.start_dial(peer_id2, id2.clone(), vec![test_addr(9001)]);
        let _ = registry.connection_established(peer_id2, conn_id2);
        let _ = registry.handshake_completed(peer_id2, conn_id2, id2);

        assert_eq!(registry.pending_count(), 1);
        assert_eq!(registry.active_count(), 1);
    }

    #[test]
    fn test_stale_pending() {
        let registry = PeerRegistry::<TestId>::new();

        // Peer 1: in Dialing state
        let id1 = TestId(1);
        let peer_id1 = test_peer_id(1);
        let _ = registry.start_dial(peer_id1, id1, vec![test_addr(9000)]);

        // Peer 2: in Handshaking state
        let id2 = TestId(2);
        let peer_id2 = test_peer_id(2);
        let conn_id2 = test_connection_id(2);
        let _ = registry.start_dial(peer_id2, id2.clone(), vec![test_addr(9001)]);
        let _ = registry.connection_established(peer_id2, conn_id2);

        // Peer 3: in Active state
        let id3 = TestId(3);
        let peer_id3 = test_peer_id(3);
        let conn_id3 = test_connection_id(3);
        let _ = registry.start_dial(peer_id3, id3.clone(), vec![test_addr(9002)]);
        let _ = registry.connection_established(peer_id3, conn_id3);
        let _ = registry.handshake_completed(peer_id3, conn_id3, id3);

        // With zero timeout, both dialing and handshaking should be stale
        let stale = registry.stale_pending(Duration::from_secs(0));
        assert_eq!(stale.len(), 2);
        assert!(stale.contains(&peer_id1));
        assert!(stale.contains(&peer_id2));
        assert!(!stale.contains(&peer_id3)); // Active connection not included

        // With a large timeout, no pending connections should be stale
        let stale = registry.stale_pending(Duration::from_secs(3600));
        assert!(stale.is_empty());
    }
}
