//! Generic peer connection registry.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Debug;
use std::hash::Hash;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use libp2p::{PeerId, swarm::ConnectionId};
use parking_lot::RwLock;

use crate::ActivateResult;
use crate::ConnectionDirection;
use crate::ConnectionState;

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

/// What existing state to replace during activation.
enum Replacement<Id> {
    /// ID already has active connection - replace it.
    ById {
        old_peer_id: PeerId,
        old_conn_id: ConnectionId,
        /// Caller's pending key (if registered under a different key).
        pending_key: Option<RegistryKey<Id>>,
    },
    /// PeerId active with different ID - replace it.
    ByPeerId {
        existing_id: Id,
        old_conn_id: ConnectionId,
    },
    /// Normal activation - just clean up pending entry if any.
    None {
        /// Caller's pending key to clean up.
        pending_key: Option<RegistryKey<Id>>,
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

    /// Insert a state into all relevant indices.
    ///
    /// Updates peer_to_key, conn_to_key, and by_key for all states.
    /// Additionally adds to pending_by_time for Connected states.
    fn insert(&mut self, key: RegistryKey<Id>, state: ConnectionState<Id, R>) {
        self.peer_to_key.insert(state.peer_id(), key.clone());
        if let Some(conn_id) = state.connection_id() {
            self.conn_to_key.insert(conn_id, key.clone());
        }
        if let Some(started_at) = state.started_at() {
            self.add_pending(started_at, key.clone());
        }
        self.by_key.insert(key, state);
    }

    /// Remove all index entries for a RegistryKey.
    ///
    /// Only removes secondary indices if they still point to this key,
    /// guarding against inconsistencies from overwritten entries.
    fn remove_by_key(&mut self, key: &RegistryKey<Id>) -> Option<ConnectionState<Id, R>> {
        let state = self.by_key.remove(key)?;
        if self.peer_to_key.get(&state.peer_id()) == Some(key) {
            self.peer_to_key.remove(&state.peer_id());
        }
        if let Some(conn_id) = state.connection_id()
            && self.conn_to_key.get(&conn_id) == Some(key)
        {
            self.conn_to_key.remove(&conn_id);
        }
        if let Some(started_at) = state.started_at() {
            self.remove_pending(started_at, key);
        }
        Some(state)
    }

    /// Remove all index entries for a PeerId.
    fn remove_by_peer(&mut self, peer_id: &PeerId) -> Option<ConnectionState<Id, R>> {
        let key = self.peer_to_key.remove(peer_id)?;
        let state = self.by_key.remove(&key)?;
        if let Some(conn_id) = state.connection_id() {
            self.conn_to_key.remove(&conn_id);
        }
        if let Some(started_at) = state.started_at() {
            self.remove_pending(started_at, &key);
        }
        Some(state)
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
    /// O(1) counter of active (post-handshake) connections.
    num_active: AtomicUsize,
    /// O(1) counter of pending (pre-handshake) connections.
    num_pending: AtomicUsize,
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
            num_active: AtomicUsize::new(0),
            num_pending: AtomicUsize::new(0),
        }
    }
}

impl<Id: Clone + Eq + Hash + Debug, R: Clone + Default + Send + Sync + 'static>
    PeerRegistry<Id, R>
{
    pub fn get(&self, id: &Id) -> Option<ConnectionState<Id, R>> {
        self.maps.read().by_key.get(&id.clone().into()).cloned()
    }

    pub fn active_connection_id(&self, id: &Id) -> Option<ConnectionId> {
        self.maps
            .read()
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
        match self.maps.read().peer_to_key.get(peer_id)? {
            RegistryKey::Known(id) => Some(id.clone()),
            RegistryKey::Pending(_) => None,
        }
    }

    pub fn contains_peer(&self, peer_id: &PeerId) -> bool {
        self.maps.read().peer_to_key.contains_key(peer_id)
    }

    pub fn resolve_peer_id(&self, id: &Id) -> Option<PeerId> {
        self.maps
            .read()
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
        let state = self.with_maps(|maps| {
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

            let returned = state.clone();
            maps.insert(key, state);
            Some(returned)
        });

        if state.is_some() {
            self.num_pending.fetch_add(1, Ordering::Relaxed);
        }
        state
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

        let returned = state.clone();
        self.with_maps(|maps| maps.insert(key, state));
        self.num_pending.fetch_add(1, Ordering::Relaxed);
        returned
    }

    /// Activate a connection: transition to Active with confirmed application-level ID.
    pub fn activate(
        &self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        id: Id,
    ) -> ActivateResult<Id> {
        let (result, pending_removed, active_removed) = self.with_maps(|maps| {
            let known_key: RegistryKey<Id> = id.clone().into();

            // Capture reason from current state before any modifications
            let reason = maps
                .peer_to_key
                .get(&peer_id)
                .and_then(|key| maps.by_key.get(key))
                .map(|state| state.reason().clone())
                .unwrap_or_default();

            let replacement = Self::find_replacement(maps, &id, &peer_id);

            let mut pending_removed: usize = 0;
            let mut active_removed: usize = 0;

            // Always clean up any existing entry under known_key (Connected or Active).
            // Using remove_by_key ensures all secondary indices are cleaned, fixing a
            // latent bug where stale peer_to_key/conn_to_key entries were left behind
            // for Connected entries under the target key.
            if let Some(state) = maps.remove_by_key(&known_key) {
                if state.is_active() {
                    active_removed += 1;
                } else {
                    pending_removed += 1;
                }
            }

            let result = match replacement {
                Replacement::ById {
                    old_peer_id,
                    old_conn_id,
                    pending_key,
                } => {
                    // Active entry under known_key already removed above.
                    // Remove caller's pending entry if under a different key.
                    if let Some(key) = pending_key
                        && maps.remove_by_key(&key).is_some()
                    {
                        pending_removed += 1;
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
                    // Remove old Active for the different ID.
                    // Any Connected entry under known_key was already removed above.
                    if maps.remove_by_key(&existing_id.clone().into()).is_some() {
                        active_removed += 1;
                    }
                    ActivateResult::Replaced {
                        old_peer_id: peer_id,
                        old_connection_id: old_conn_id,
                        old_id: Some(existing_id),
                    }
                }
                Replacement::None { pending_key } => {
                    // Any Connected entry under known_key was already removed above.
                    // Remove caller's pending entry.
                    if let Some(key) = pending_key
                        && maps.remove_by_key(&key).is_some()
                    {
                        pending_removed += 1;
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
            maps.insert(known_key.clone(), new_state);

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

            (result, pending_removed, active_removed)
        });

        // Update O(1) counters outside the lock
        self.num_active.fetch_add(1, Ordering::Relaxed);
        if active_removed > 0 {
            self.num_active.fetch_sub(active_removed, Ordering::Relaxed);
        }
        if pending_removed > 0 {
            self.num_pending
                .fetch_sub(pending_removed, Ordering::Relaxed);
        }

        result
    }

    fn find_replacement(maps: &Maps<Id, R>, id: &Id, peer_id: &PeerId) -> Replacement<Id> {
        let known_key: RegistryKey<Id> = id.clone().into();
        let pending_key = RegistryKey::Pending(*peer_id);

        // Returns the key if it has a pending (Connected) entry.
        let is_pending_key = |key: &RegistryKey<Id>| -> Option<RegistryKey<Id>> {
            maps.by_key
                .get(key)
                .filter(|state| state.is_pending())
                .map(|_| key.clone())
        };

        // Case 1: ID already has an active connection
        if let Some(ConnectionState::Active {
            peer_id: active_peer_id,
            connection_id: active_conn_id,
            ..
        }) = maps.by_key.get(&known_key)
        {
            let pending = maps
                .peer_to_key
                .get(peer_id)
                .filter(|k| **k != known_key)
                .and_then(is_pending_key);
            return Replacement::ById {
                old_peer_id: *active_peer_id,
                old_conn_id: *active_conn_id,
                pending_key: pending,
            };
        }

        // Case 2: PeerId already active with different ID
        if let Some(key) = maps.peer_to_key.get(peer_id)
            && let RegistryKey::Known(existing_id) = key
            && existing_id != id
            && let Some(ConnectionState::Active { connection_id, .. }) = maps.by_key.get(key)
        {
            return Replacement::ByPeerId {
                existing_id: existing_id.clone(),
                old_conn_id: *connection_id,
            };
        }

        // Case 3: Normal activation, clean up pending entry
        let pending = is_pending_key(&pending_key);
        Replacement::None {
            pending_key: pending,
        }
    }

    pub fn get_by_peer_id(&self, peer_id: &PeerId) -> Option<ConnectionState<Id, R>> {
        let maps = self.maps.read();
        let key = maps.peer_to_key.get(peer_id)?;
        maps.by_key.get(key).cloned()
    }

    #[must_use]
    pub fn active_ids(&self) -> Vec<Id> {
        self.maps
            .read()
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

    /// Count of active connections (O(1) atomic load).
    pub fn active_count(&self) -> usize {
        self.num_active.load(Ordering::Relaxed)
    }

    /// Count of pending connections (O(1) atomic load).
    pub fn pending_count(&self) -> usize {
        self.num_pending.load(Ordering::Relaxed)
    }

    /// Get PeerIds of pending connections that have exceeded the timeout.
    ///
    /// Uses time-indexed lookup for O(log n + k) complexity where k = stale count.
    #[must_use]
    pub fn stale_pending(&self, timeout: std::time::Duration) -> Vec<PeerId> {
        let Some(cutoff) = Instant::now().checked_sub(timeout) else {
            return Vec::new();
        };
        let maps = self.maps.read();

        maps.pending_by_time
            .range(..=cutoff)
            .flat_map(|(_, keys)| keys.iter())
            .filter_map(|key| maps.by_key.get(key).map(|s| s.peer_id()))
            .collect()
    }

    /// Remove peer from all maps and return final state.
    pub fn disconnected(&self, peer_id: &PeerId) -> Option<ConnectionState<Id, R>> {
        let state = self.with_maps(|maps| maps.remove_by_peer(peer_id));

        if let Some(ref s) = state {
            match s {
                ConnectionState::Active { .. } => {
                    self.num_active.fetch_sub(1, Ordering::Relaxed);
                }
                ConnectionState::Connected { .. } => {
                    self.num_pending.fetch_sub(1, Ordering::Relaxed);
                }
            }
        }

        state
    }

    /// Run a write transaction on the inner maps.
    fn with_maps<T>(&self, f: impl FnOnce(&mut Maps<Id, R>) -> T) -> T {
        let mut maps = self.maps.write();
        f(&mut maps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    struct TestId(u64);

    type Registry = PeerRegistry<TestId>;

    fn registry() -> Registry {
        Registry::new()
    }

    fn peer(n: u8) -> PeerId {
        let bytes = [n; 32];
        let key = libp2p::identity::ed25519::SecretKey::try_from_bytes(bytes)
            .expect("32 bytes is valid ed25519 secret key");
        let keypair =
            libp2p::identity::Keypair::from(libp2p::identity::ed25519::Keypair::from(key));
        keypair.public().to_peer_id()
    }

    fn conn(n: u8) -> ConnectionId {
        ConnectionId::new_unchecked(n as usize)
    }

    fn assert_counts(r: &Registry, pending: usize, active: usize) {
        assert_eq!(
            r.pending_count(),
            pending,
            "pending: expected {pending}, got {}",
            r.pending_count()
        );
        assert_eq!(
            r.active_count(),
            active,
            "active: expected {active}, got {}",
            r.active_count()
        );
    }

    #[test]
    fn test_connected_outbound() {
        let r = registry();
        let p = peer(1);

        let state = r
            .connected_outbound(p, conn(1), Some(TestId(1)), Instant::now(), ())
            .unwrap();
        assert!(matches!(
            state,
            ConnectionState::Connected {
                direction: ConnectionDirection::Outbound,
                ..
            }
        ));
        assert!(r.contains_peer(&p));
    }

    #[test]
    fn test_register_outbound_duplicate() {
        let r = registry();
        let p = peer(1);

        assert!(
            r.connected_outbound(p, conn(1), Some(TestId(1)), Instant::now(), ())
                .is_some()
        );
        assert!(
            r.connected_outbound(p, conn(2), Some(TestId(2)), Instant::now(), ())
                .is_none()
        );
    }

    #[test]
    fn test_register_outbound_without_id() {
        let r = registry();
        let p = peer(1);

        assert!(
            r.connected_outbound(p, conn(1), None, Instant::now(), ())
                .is_some()
        );
        assert!(r.contains_peer(&p));
        assert!(r.resolve_id(&p).is_none());
    }

    #[test]
    fn test_activate_new_peer() {
        let r = registry();
        let p = peer(1);
        let id = TestId(1);

        r.connected_outbound(p, conn(1), Some(id.clone()), Instant::now(), ());
        assert_eq!(r.activate(p, conn(1), id.clone()), ActivateResult::Accepted);
        assert!(r.get(&id).unwrap().is_active());
    }

    #[test]
    fn test_activate_replaces_old_connection() {
        let r = registry();
        let p = peer(1);
        let id = TestId(1);

        r.connected_outbound(p, conn(1), Some(id.clone()), Instant::now(), ());
        assert_eq!(r.activate(p, conn(1), id.clone()), ActivateResult::Accepted);

        assert!(matches!(
            r.activate(p, conn(2), id),
            ActivateResult::Replaced {
                old_connection_id,
                ..
            } if old_connection_id == conn(1)
        ));
    }

    #[test]
    fn test_connected_inbound() {
        let r = registry();
        let p = peer(1);

        let state = r.connected_inbound(p, conn(1));
        assert!(matches!(
            state,
            ConnectionState::Connected {
                direction: ConnectionDirection::Inbound,
                ..
            }
        ));
        assert!(r.resolve_id(&p).is_none());
        assert!(r.contains_peer(&p));
    }

    #[test]
    fn test_disconnected() {
        let r = registry();
        let p = peer(1);
        let id = TestId(1);

        r.connected_outbound(p, conn(1), Some(id.clone()), Instant::now(), ());
        r.activate(p, conn(1), id.clone());

        assert!(r.disconnected(&p).is_some());
        assert!(r.get(&id).is_none());
        assert_eq!(r.resolve_id(&p), None);
    }

    #[test]
    fn test_active_count() {
        let r = registry();

        r.connected_outbound(peer(1), conn(1), Some(TestId(1)), Instant::now(), ());
        r.connected_outbound(peer(2), conn(2), Some(TestId(2)), Instant::now(), ());
        r.activate(peer(2), conn(2), TestId(2));

        assert_counts(&r, 1, 1);
    }

    #[test]
    fn test_stale_pending() {
        let r = registry();
        let p1 = peer(1);
        let p2 = peer(2);

        r.connected_outbound(p1, conn(1), Some(TestId(1)), Instant::now(), ());
        r.connected_inbound(p2, conn(2));
        r.connected_outbound(peer(3), conn(3), Some(TestId(3)), Instant::now(), ());
        r.activate(peer(3), conn(3), TestId(3));

        let stale = r.stale_pending(Duration::ZERO);
        assert_eq!(stale.len(), 2);
        assert!(stale.contains(&p1));
        assert!(stale.contains(&p2));

        assert!(r.stale_pending(Duration::from_secs(3600)).is_empty());
    }

    #[test]
    fn test_reason_carried_through_lifecycle() {
        #[derive(Clone, Debug, Default, PartialEq)]
        struct TestReason(String);

        let r = PeerRegistry::<TestId, TestReason>::new();
        let p = peer(1);
        let id = TestId(1);
        let reason = TestReason("discovery".to_string());

        let state = r
            .connected_outbound(p, conn(1), Some(id.clone()), Instant::now(), reason.clone())
            .unwrap();
        assert_eq!(state.reason(), &reason);

        r.activate(p, conn(1), id.clone());
        assert_eq!(r.get(&id).unwrap().reason(), &reason);
        assert_eq!(r.disconnected(&p).unwrap().reason(), &reason);
    }

    #[test]
    fn test_inbound_uses_default_reason() {
        #[derive(Clone, Debug, Default, PartialEq)]
        struct TestReason(Option<String>);

        let r = PeerRegistry::<TestId, TestReason>::new();
        let state = r.connected_inbound(peer(1), conn(1));
        assert_eq!(state.reason(), &TestReason(None));
    }

    #[test]
    fn test_counters_through_lifecycle() {
        let r = registry();
        assert_counts(&r, 0, 0);

        r.connected_outbound(peer(1), conn(1), Some(TestId(1)), Instant::now(), ());
        assert_counts(&r, 1, 0);

        r.connected_inbound(peer(2), conn(2));
        assert_counts(&r, 2, 0);

        r.activate(peer(1), conn(1), TestId(1));
        assert_counts(&r, 1, 1);

        r.activate(peer(2), conn(2), TestId(2));
        assert_counts(&r, 0, 2);

        r.disconnected(&peer(1));
        assert_counts(&r, 0, 1);

        r.disconnected(&peer(2));
        assert_counts(&r, 0, 0);
    }

    #[test]
    fn test_counters_on_replacement() {
        let r = registry();

        r.connected_outbound(peer(1), conn(1), Some(TestId(1)), Instant::now(), ());
        r.activate(peer(1), conn(1), TestId(1));
        assert_counts(&r, 0, 1);

        assert!(r.activate(peer(1), conn(2), TestId(1)).is_replaced());
        assert_counts(&r, 0, 1);
    }

    #[test]
    fn test_counters_disconnect_pending() {
        let r = registry();

        r.connected_inbound(peer(1), conn(1));
        assert_counts(&r, 1, 0);

        r.disconnected(&peer(1));
        assert_counts(&r, 0, 0);
    }

    /// Regression: ByPeerId replacement must clean up Connected entries under the
    /// target key. Previously, stale peer_to_key and conn_to_key entries were left behind.
    #[test]
    fn test_activate_by_peer_id_cleans_connected_entry() {
        let r = registry();

        r.connected_outbound(peer(1), conn(1), Some(TestId(1)), Instant::now(), ());
        r.activate(peer(1), conn(1), TestId(1));

        r.connected_outbound(peer(2), conn(2), Some(TestId(2)), Instant::now(), ());
        assert_counts(&r, 1, 1);

        // Activate peer(1) with TestId(2) → ByPeerId replacement.
        // Must also clean up peer(2)'s Connected entry under Known(TestId(2)).
        let result = r.activate(peer(1), conn(3), TestId(2));
        assert!(matches!(
            result,
            ActivateResult::Replaced {
                old_id: Some(TestId(1)),
                ..
            }
        ));

        let state = r.get(&TestId(2)).unwrap();
        assert!(state.is_active());
        assert_eq!(state.peer_id(), peer(1));

        assert!(r.get(&TestId(1)).is_none());
        assert!(!r.contains_peer(&peer(2)));
        assert_counts(&r, 0, 1);
    }
}
