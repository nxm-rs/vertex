//! Generic peer connection registry.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Debug;
use std::hash::Hash;
use std::time::Instant;

use libp2p::{PeerId, swarm::ConnectionId};
use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::direction::ConnectionDirection;
use crate::result::ActivateResult;
use crate::state::ConnectionState;

/// Registry key distinguishing known-ID from pending connections.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum RegistryKey<Id> {
    /// Peer with known application-level ID.
    Known(Id),
    /// Pending connection (inbound or outbound without ID) awaiting application-level ID.
    Pending(PeerId),
}

impl<Id> From<Id> for RegistryKey<Id> {
    fn from(id: Id) -> Self {
        Self::Known(id)
    }
}

/// Entry in the pending time index to be removed.
struct PendingEntry<Id> {
    key: RegistryKey<Id>,
    started_at: Instant,
}

/// What existing state to replace during activation.
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
    /// Normal activation - just clean up pending entry if any.
    None {
        pending_entry: Option<PendingEntry<Id>>,
    },
}

/// Inner maps protected by a single lock.
struct Maps<Id, R> {
    by_key: HashMap<RegistryKey<Id>, ConnectionState<Id, R>>,
    peer_to_key: HashMap<PeerId, RegistryKey<Id>>,
    conn_to_key: HashMap<ConnectionId, RegistryKey<Id>>,
    /// Pending connections indexed by start time for O(log n + k) stale detection.
    pending_by_time: BTreeMap<Instant, HashSet<RegistryKey<Id>>>,
}

impl<Id, R> Default for Maps<Id, R> {
    fn default() -> Self {
        Self {
            by_key: HashMap::new(),
            peer_to_key: HashMap::new(),
            conn_to_key: HashMap::new(),
            pending_by_time: BTreeMap::new(),
        }
    }
}

impl<Id: Clone + Eq + Hash, R> Maps<Id, R> {
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
/// `R` is the reason type carried inline with each connection state.
///
/// Lifecycle: Connected → Active (via `activate()`).
/// Dial tracking is handled externally by a `DialTracker`.
pub struct PeerRegistry<Id, R = ()> {
    maps: RwLock<Maps<Id, R>>,
}

impl<Id, R> Default for PeerRegistry<Id, R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Id, R> PeerRegistry<Id, R> {
    pub fn new() -> Self {
        Self {
            maps: RwLock::new(Maps::default()),
        }
    }

    fn read(&self) -> RwLockReadGuard<'_, Maps<Id, R>> {
        self.maps.read()
    }

    fn write(&self) -> RwLockWriteGuard<'_, Maps<Id, R>> {
        self.maps.write()
    }
}

impl<Id: Clone + Eq + Hash + Debug, R: Clone + Default + Send + Sync + 'static> PeerRegistry<Id, R> {
    pub fn get(&self, id: &Id) -> Option<ConnectionState<Id, R>> {
        self.read()
            .by_key
            .get(&id.clone().into())
            .cloned()
    }

    pub fn active_connection_id(&self, id: &Id) -> Option<ConnectionId> {
        self.read()
            .by_key
            .get(&id.clone().into())
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
            .get(&id.clone().into())
            .map(|s| s.peer_id())
    }

    /// Register an outbound connection directly in Connected state.
    ///
    /// Used after dial tracking resolves externally (e.g., by a DialTracker).
    /// Returns the new state, or None if the peer_id or id is already tracked.
    pub fn connected_outbound(
        &self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        id: Option<Id>,
        started_at: Instant,
        reason: R,
    ) -> Option<ConnectionState<Id, R>> {
        let mut maps = self.write();

        if maps.peer_to_key.contains_key(&peer_id) {
            return None;
        }

        let key = match &id {
            Some(known_id) => {
                let k = known_id.clone().into();
                if maps.by_key.contains_key(&k) {
                    return None;
                }
                k
            }
            None => RegistryKey::Pending(peer_id),
        };

        let state = ConnectionState::Connected {
            peer_id,
            connection_id,
            id,
            direction: ConnectionDirection::Outbound,
            started_at,
            reason,
        };

        maps.by_key.insert(key.clone(), state.clone());
        maps.peer_to_key.insert(peer_id, key.clone());
        maps.conn_to_key.insert(connection_id, key.clone());
        maps.add_pending(started_at, key);

        Some(state)
    }

    /// Register inbound connection in Connected state (awaiting identity).
    pub fn connected_inbound(
        &self,
        peer_id: PeerId,
        connection_id: ConnectionId,
    ) -> ConnectionState<Id, R> {
        let key = RegistryKey::Pending(peer_id);
        let started_at = Instant::now();

        let state = ConnectionState::Connected {
            peer_id,
            connection_id,
            id: None,
            direction: ConnectionDirection::Inbound,
            started_at,
            reason: R::default(),
        };

        let mut maps = self.write();
        maps.by_key.insert(key.clone(), state.clone());
        maps.peer_to_key.insert(peer_id, key.clone());
        maps.conn_to_key.insert(connection_id, key.clone());
        maps.add_pending(started_at, key);

        state
    }

    /// Activate a connection: transition to Active with confirmed application-level ID.
    pub fn activate(
        &self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        id: Id,
    ) -> ActivateResult<Id> {
        let mut maps = self.write();

        let known_key = id.clone().into();
        let pending_key = RegistryKey::Pending(peer_id);

        // Capture reason from current state before any modifications
        let reason = maps.peer_to_key.get(&peer_id)
            .and_then(|key| maps.by_key.get(key))
            .map(|state| state.reason().clone())
            .unwrap_or_default();

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
                maps.by_key.remove(&existing_id.clone().into());
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
                if let Some(state) = maps.by_key.remove(&pending_key) {
                    if let Some(started_at) = state.started_at() {
                        maps.remove_pending(started_at, &pending_key);
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
            reason,
        };
        maps.by_key.insert(known_key.clone(), new_state);
        maps.peer_to_key.insert(peer_id, known_key.clone());
        maps.conn_to_key.insert(connection_id, known_key.clone());

        // Debug assertions for one-overlay-one-peerid invariant (O(1) checks)
        debug_assert!(
            maps.peer_to_key.get(&peer_id) == Some(&known_key),
            "Invariant violated: peer_id {:?} should map to known_key {:?}",
            peer_id,
            known_key
        );
        debug_assert!(
            matches!(
                maps.by_key.get(&known_key),
                Some(ConnectionState::Active { peer_id: p, .. }) if *p == peer_id
            ),
            "Invariant violated: known_key {:?} should have peer_id {:?}",
            known_key,
            peer_id
        );

        result
    }

    fn find_replacement(maps: &Maps<Id, R>, id: &Id, peer_id: &PeerId) -> Replacement<Id> {
        let known_key = id.clone().into();
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

        // Case 3: Normal activation, clean up pending entry
        let entry = pending_entry(&pending_key);
        Replacement::None {
            pending_entry: entry,
        }
    }

    pub fn get_by_peer_id(&self, peer_id: &PeerId) -> Option<ConnectionState<Id, R>> {
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

    /// Count of active connections (acquires read lock, O(n) scan).
    pub fn active_count(&self) -> usize {
        self.read()
            .by_key
            .values()
            .filter(|state| matches!(state, ConnectionState::Active { .. }))
            .count()
    }

    /// Count of pending connections (acquires read lock, O(n) scan).
    pub fn pending_count(&self) -> usize {
        self.read()
            .by_key
            .values()
            .filter(|state| matches!(state, ConnectionState::Connected { .. }))
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
    pub fn disconnected(&self, peer_id: &PeerId) -> Option<ConnectionState<Id, R>> {
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

impl<Id, R> crate::resolver::PeerResolver for PeerRegistry<Id, R>
where
    Id: Clone + Eq + Hash + Debug + Send + Sync + 'static,
    R: Clone + Default + Send + Sync + 'static,
{
    type Id = Id;

    fn resolve_id(&self, peer_id: &PeerId) -> Option<Id> {
        match self.read().peer_to_key.get(peer_id)? {
            RegistryKey::Known(id) => Some(id.clone()),
            RegistryKey::Pending(_) => None,
        }
    }

    fn resolve_peer_id(&self, id: &Id) -> Option<PeerId> {
        self.read()
            .by_key
            .get(&id.clone().into())
            .map(|s| s.peer_id())
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

    fn test_connection_id(n: u8) -> ConnectionId {
        ConnectionId::new_unchecked(n as usize)
    }

    #[test]
    fn test_connected_outbound() {
        let registry = PeerRegistry::<TestId>::new();
        let id = TestId(1);
        let peer_id = test_peer_id(1);
        let conn_id = test_connection_id(1);

        let state = registry.connected_outbound(
            peer_id, conn_id, Some(id.clone()), Instant::now(), (),
        );
        assert!(state.is_some());
        assert!(matches!(state.unwrap(), ConnectionState::Connected { direction: ConnectionDirection::Outbound, .. }));
        assert!(registry.contains_peer(&peer_id));
    }

    #[test]
    fn test_register_outbound_duplicate() {
        let registry = PeerRegistry::<TestId>::new();
        let peer_id = test_peer_id(1);
        let conn_id1 = test_connection_id(1);
        let conn_id2 = test_connection_id(2);

        let result1 = registry.connected_outbound(
            peer_id, conn_id1, Some(TestId(1)), Instant::now(), (),
        );
        assert!(result1.is_some());

        let result2 = registry.connected_outbound(
            peer_id, conn_id2, Some(TestId(2)), Instant::now(), (),
        );
        assert!(result2.is_none());
    }

    #[test]
    fn test_register_outbound_without_id() {
        let registry = PeerRegistry::<TestId>::new();
        let peer_id = test_peer_id(1);
        let conn_id = test_connection_id(1);

        let state = registry.connected_outbound(
            peer_id, conn_id, None, Instant::now(), (),
        );
        assert!(state.is_some());
        assert!(registry.contains_peer(&peer_id));
        // No Id known yet
        assert!(registry.resolve_id(&peer_id).is_none());
    }

    #[test]
    fn test_activate_new_peer() {
        let registry = PeerRegistry::<TestId>::new();
        let id = TestId(1);
        let peer_id = test_peer_id(1);
        let conn_id = test_connection_id(1);

        registry.connected_outbound(
            peer_id, conn_id, Some(id.clone()), Instant::now(), (),
        );

        let result = registry.activate(peer_id, conn_id, id.clone());
        assert_eq!(result, ActivateResult::Accepted);

        let state = registry.get(&id).unwrap();
        assert!(matches!(state, ConnectionState::Active { .. }));
    }

    #[test]
    fn test_activate_replaces_old_connection() {
        let registry = PeerRegistry::<TestId>::new();
        let id = TestId(1);
        let peer_id = test_peer_id(1);
        let conn_id1 = test_connection_id(1);
        let conn_id2 = test_connection_id(2);

        registry.connected_outbound(
            peer_id, conn_id1, Some(id.clone()), Instant::now(), (),
        );
        let result1 = registry.activate(peer_id, conn_id1, id.clone());
        assert_eq!(result1, ActivateResult::Accepted);

        let result2 = registry.activate(peer_id, conn_id2, id);
        assert!(matches!(
            result2,
            ActivateResult::Replaced {
                old_connection_id,
                ..
            } if old_connection_id == conn_id1
        ));
    }

    #[test]
    fn test_connected_inbound() {
        let registry = PeerRegistry::<TestId>::new();
        let peer_id = test_peer_id(1);
        let conn_id = test_connection_id(1);

        let state = registry.connected_inbound(peer_id, conn_id);
        assert!(matches!(
            state,
            ConnectionState::Connected {
                direction: ConnectionDirection::Inbound,
                ..
            }
        ));

        assert!(registry.resolve_id(&peer_id).is_none());
        assert!(registry.contains_peer(&peer_id));
    }

    #[test]
    fn test_disconnected() {
        let registry = PeerRegistry::<TestId>::new();
        let id = TestId(1);
        let peer_id = test_peer_id(1);
        let conn_id = test_connection_id(1);

        registry.connected_outbound(
            peer_id, conn_id, Some(id.clone()), Instant::now(), (),
        );
        registry.activate(peer_id, conn_id, id.clone());

        let state = registry.disconnected(&peer_id);
        assert!(state.is_some());
        assert!(registry.get(&id).is_none());
        assert_eq!(registry.resolve_id(&peer_id), None);
    }

    #[test]
    fn test_active_count() {
        let registry = PeerRegistry::<TestId>::new();

        // Peer 1: Connected (pending handshake)
        let peer_id1 = test_peer_id(1);
        let conn_id1 = test_connection_id(1);
        registry.connected_outbound(
            peer_id1, conn_id1, Some(TestId(1)), Instant::now(), (),
        );

        // Peer 2: Active
        let peer_id2 = test_peer_id(2);
        let conn_id2 = test_connection_id(2);
        registry.connected_outbound(
            peer_id2, conn_id2, Some(TestId(2)), Instant::now(), (),
        );
        registry.activate(peer_id2, conn_id2, TestId(2));

        assert_eq!(registry.pending_count(), 1);
        assert_eq!(registry.active_count(), 1);
    }

    #[test]
    fn test_stale_pending() {
        let registry = PeerRegistry::<TestId>::new();

        // Peer 1: Connected (handshaking)
        let peer_id1 = test_peer_id(1);
        let conn_id1 = test_connection_id(1);
        registry.connected_outbound(
            peer_id1, conn_id1, Some(TestId(1)), Instant::now(), (),
        );

        // Peer 2: Inbound connected
        let peer_id2 = test_peer_id(2);
        let conn_id2 = test_connection_id(2);
        registry.connected_inbound(peer_id2, conn_id2);

        // Peer 3: Active
        let peer_id3 = test_peer_id(3);
        let conn_id3 = test_connection_id(3);
        registry.connected_outbound(
            peer_id3, conn_id3, Some(TestId(3)), Instant::now(), (),
        );
        registry.activate(peer_id3, conn_id3, TestId(3));

        // With zero timeout, both handshaking peers should be stale
        let stale = registry.stale_pending(Duration::from_secs(0));
        assert_eq!(stale.len(), 2);
        assert!(stale.contains(&peer_id1));
        assert!(stale.contains(&peer_id2));
        assert!(!stale.contains(&peer_id3));

        let stale = registry.stale_pending(Duration::from_secs(3600));
        assert!(stale.is_empty());
    }

    #[test]
    fn test_reason_carried_through_lifecycle() {
        #[derive(Clone, Debug, Default, PartialEq)]
        struct TestReason(String);

        let registry = PeerRegistry::<TestId, TestReason>::new();
        let id = TestId(1);
        let peer_id = test_peer_id(1);
        let conn_id = test_connection_id(1);
        let reason = TestReason("discovery".to_string());

        // Register outbound connected with reason
        let state = registry.connected_outbound(
            peer_id, conn_id, Some(id.clone()), Instant::now(), reason.clone(),
        ).unwrap();
        assert_eq!(state.reason(), &reason);

        // Reason carries through activate
        registry.activate(peer_id, conn_id, id.clone());
        let state = registry.get(&id).unwrap();
        assert_eq!(state.reason(), &reason);

        // Reason available on disconnect
        let state = registry.disconnected(&peer_id).unwrap();
        assert_eq!(state.reason(), &reason);
    }

    #[test]
    fn test_inbound_uses_default_reason() {
        #[derive(Clone, Debug, Default, PartialEq)]
        struct TestReason(Option<String>);

        let registry = PeerRegistry::<TestId, TestReason>::new();
        let peer_id = test_peer_id(1);
        let conn_id = test_connection_id(1);

        let state = registry.connected_inbound(peer_id, conn_id);
        assert_eq!(state.reason(), &TestReason(None));
    }
}
