//! Peer manager with Arc-per-peer pattern for minimal lock contention.

use std::collections::HashMap;
use std::sync::Arc;

use libp2p::PeerId;
use parking_lot::RwLock;
use tracing::{debug, trace};

use crate::registry::PeerRegistry;
use crate::state::{ConnectionState, NetPeerSnapshot, PeerState};
use crate::store::{ExtSnapBounds, NetPeerStore, PeerStoreError};
use crate::traits::{NetPeerExt, NetPeerId, NetPeerScoreExt};

/// Type alias for the internal peer map.
type PeerMap<Id, Ext, ScoreExt> = HashMap<Id, Arc<PeerState<Id, Ext, ScoreExt>>>;

/// Default ban threshold score (peers below this get banned).
pub const DEFAULT_BAN_THRESHOLD: f64 = -100.0;

/// Default max tracked peers (storage limit, not connection limit).
pub const DEFAULT_MAX_TRACKED_PEERS: usize = 10_000;

/// Priority for pruning (higher = prune first).
#[derive(Debug, Clone, Copy, PartialEq)]
struct PrunePriority {
    is_banned: bool,
    score: f64,
    last_seen: u64,
}

impl Eq for PrunePriority {}

impl PartialOrd for PrunePriority {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PrunePriority {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Banned peers pruned first
        match (self.is_banned, other.is_banned) {
            (true, false) => return std::cmp::Ordering::Greater,
            (false, true) => return std::cmp::Ordering::Less,
            _ => {}
        }
        // Lower score = higher prune priority
        match self.score.partial_cmp(&other.score) {
            Some(std::cmp::Ordering::Less) => return std::cmp::Ordering::Greater,
            Some(std::cmp::Ordering::Greater) => return std::cmp::Ordering::Less,
            _ => {}
        }
        // Older last_seen = higher prune priority
        other.last_seen.cmp(&self.last_seen)
    }
}

/// Peer manager using Arc-per-peer pattern for minimal lock contention.
///
/// Protocol handlers get `Arc<PeerState>` once, then all subsequent operations
/// are lock-free (atomics) or per-peer locked (no global contention).
///
/// Peers are only added when full identity (Ext) is known - dial tracking
/// is handled separately by DialTracker.
pub struct NetPeerManager<Id: NetPeerId, Ext: NetPeerExt, ScoreExt: NetPeerScoreExt = ()> {
    ban_threshold: f64,
    max_tracked_peers: Option<usize>,
    peers: RwLock<PeerMap<Id, Ext, ScoreExt>>,
    registry: PeerRegistry<Id>,
}

impl<Id: NetPeerId, Ext: NetPeerExt, ScoreExt: NetPeerScoreExt> NetPeerManager<Id, Ext, ScoreExt> {
    /// Create a new peer manager with specified limits.
    pub fn new(ban_threshold: f64, max_tracked_peers: Option<usize>) -> Self {
        Self {
            ban_threshold,
            max_tracked_peers,
            peers: RwLock::new(HashMap::new()),
            registry: PeerRegistry::new(),
        }
    }

    /// Create with default settings.
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_BAN_THRESHOLD, Some(DEFAULT_MAX_TRACKED_PEERS))
    }

    /// Get the ban threshold.
    pub fn ban_threshold(&self) -> f64 {
        self.ban_threshold
    }

    /// Get the max tracked peers limit.
    pub fn max_tracked_peers(&self) -> Option<usize> {
        self.max_tracked_peers
    }

    /// Insert a peer with extension data. Returns the peer state.
    ///
    /// If peer already exists, returns existing state (ext is not updated).
    pub fn insert_peer(&self, id: Id, ext: Ext) -> Arc<PeerState<Id, Ext, ScoreExt>> {
        // Fast path: check if exists
        {
            let peers = self.peers.read();
            if let Some(state) = peers.get(&id) {
                return Arc::clone(state);
            }
        }

        // Slow path: insert
        let mut peers = self.peers.write();

        // Double-check after acquiring write lock
        if let Some(state) = peers.get(&id) {
            return Arc::clone(state);
        }

        // Prune if at capacity
        if let Some(max) = self.max_tracked_peers
            && peers.len() >= max
        {
            self.prune_one_peer(&mut peers);
        }

        let state = Arc::new(PeerState::new(ext));
        peers.insert(id.clone(), Arc::clone(&state));

        debug!(?id, "peer inserted");

        state
    }

    /// Prune one peer to make room. Never prunes Connected peers.
    fn prune_one_peer(&self, peers: &mut PeerMap<Id, Ext, ScoreExt>) {
        let mut candidates: Vec<(Id, PrunePriority)> = peers
            .iter()
            .filter_map(|(id, state)| {
                let conn_state = state.connection_state();
                // Never prune connected peers
                if conn_state.is_connected() {
                    return None;
                }
                let priority = PrunePriority {
                    is_banned: conn_state.is_banned(),
                    score: state.score(),
                    last_seen: state.last_seen(),
                };
                Some((id.clone(), priority))
            })
            .collect();

        if candidates.is_empty() {
            trace!("no prunable peers found");
            return;
        }

        candidates.sort_by(|a, b| b.1.cmp(&a.1));

        if let Some((id, _)) = candidates.first() {
            peers.remove(id);
            self.registry.remove_by_id(id);
            debug!(?id, "pruned peer");
        }
    }

    pub fn get_peer(&self, id: &Id) -> Option<Arc<PeerState<Id, Ext, ScoreExt>>> {
        self.peers.read().get(id).map(Arc::clone)
    }

    pub fn contains(&self, id: &Id) -> bool {
        self.peers.read().contains_key(id)
    }

    pub fn peer_count(&self) -> usize {
        self.peers.read().len()
    }

    pub fn peer_ids(&self) -> Vec<Id> {
        self.peers.read().keys().cloned().collect()
    }

    /// Mark peer connected and register Id ↔ PeerId mapping.
    ///
    /// Peer must already exist (via `insert_peer`).
    pub fn on_connected(&self, id: &Id, peer_id: PeerId) -> bool {
        let Some(peer) = self.get_peer(id) else {
            debug!(?id, "on_connected: peer not found");
            return false;
        };

        peer.set_connection_state(ConnectionState::Connected);
        self.registry.register(id.clone(), peer_id);

        debug!(%peer_id, ?id, "peer connected");
        true
    }

    /// Handle disconnection by PeerId. Returns protocol ID if found.
    pub fn on_disconnected_by_peer_id(&self, peer_id: &PeerId) -> Option<Id> {
        let id = self.registry.resolve_id(peer_id)?;

        if let Some(peer) = self.get_peer(&id) {
            peer.set_connection_state(ConnectionState::Disconnected);
            debug!(%peer_id, ?id, "peer disconnected");
        }

        Some(id)
    }

    pub fn on_disconnected(&self, id: &Id) {
        if let Some(peer) = self.get_peer(id) {
            peer.set_connection_state(ConnectionState::Disconnected);
        }
    }

    pub fn ban(&self, id: &Id, reason: Option<String>) {
        if let Some(peer) = self.get_peer(id) {
            peer.ban(reason);
            self.registry.remove_by_id(id);
        }
    }

    pub fn unban(&self, id: &Id) {
        if let Some(peer) = self.get_peer(id) {
            peer.unban();
        }
    }

    pub fn is_banned(&self, id: &Id) -> bool {
        self.get_peer(id).map(|p| p.is_banned()).unwrap_or(false)
    }

    pub fn is_connected(&self, id: &Id) -> bool {
        self.get_peer(id).map(|p| p.is_connected()).unwrap_or(false)
    }

    pub fn connected_peers(&self) -> Vec<Id> {
        self.peers
            .read()
            .iter()
            .filter(|(_, state)| state.is_connected())
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn disconnected_peers(&self) -> Vec<Id> {
        self.peers
            .read()
            .iter()
            .filter(|(_, state)| state.is_disconnected())
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn connected_count(&self) -> usize {
        self.peers
            .read()
            .values()
            .filter(|state| state.is_connected())
            .count()
    }

    pub fn score(&self, id: &Id) -> Option<f64> {
        self.get_peer(id).map(|p| p.score())
    }

    pub fn resolve_peer_id(&self, id: &Id) -> Option<PeerId> {
        self.registry.resolve_peer(id)
    }

    pub fn resolve_id(&self, peer_id: &PeerId) -> Option<Id> {
        self.registry.resolve_id(peer_id)
    }

    pub fn load_from_store<S>(&self, store: &S) -> Result<usize, PeerStoreError>
    where
        S: NetPeerStore<Id, Ext::Snapshot, ScoreExt::Snapshot> + ?Sized,
        Ext::Snapshot: ExtSnapBounds,
        ScoreExt::Snapshot: ExtSnapBounds,
    {
        let snapshots = store.load_all()?;
        let count = snapshots.len();

        let mut sanitized = 0;

        let mut peers = self.peers.write();
        for snapshot in snapshots {
            let (id, state) = snapshot.into_state::<Ext, ScoreExt>();

            // Sanitize Connected state from previous session
            if state.connection_state().is_connected() {
                state.set_connection_state(ConnectionState::Disconnected);
                sanitized += 1;
            }

            peers.insert(id, Arc::new(state));
        }

        if sanitized > 0 {
            debug!(count, sanitized, "loaded peers (sanitized connected states)");
        } else {
            debug!(count, "loaded peers from store");
        }
        Ok(count)
    }

    pub fn save_to_store<S>(&self, store: &S) -> Result<usize, PeerStoreError>
    where
        S: NetPeerStore<Id, Ext::Snapshot, ScoreExt::Snapshot> + ?Sized,
        Ext::Snapshot: ExtSnapBounds,
        ScoreExt::Snapshot: ExtSnapBounds,
    {
        let peers = self.peers.read();
        let mut snapshots: Vec<NetPeerSnapshot<Id, Ext::Snapshot, ScoreExt::Snapshot>> = peers
            .iter()
            .map(|(id, state)| state.snapshot(id.clone()))
            .collect();
        let count = snapshots.len();
        drop(peers);

        // Sanitize Connected state before persisting
        for snapshot in &mut snapshots {
            if snapshot.state.is_connected() {
                snapshot.state = ConnectionState::Disconnected;
            }
        }

        store.save_batch(&snapshots)?;
        store.flush()?;

        debug!(count, "saved peers to store");
        Ok(count)
    }

    pub fn snapshots(&self) -> Vec<NetPeerSnapshot<Id, Ext::Snapshot, ScoreExt::Snapshot>> {
        self.peers
            .read()
            .iter()
            .map(|(id, state)| state.snapshot(id.clone()))
            .collect()
    }

    pub fn remove_peer(&self, id: &Id) {
        self.peers.write().remove(id);
        self.registry.remove_by_id(id);
    }

    /// Remove disconnected peers not seen within max_age_secs.
    pub fn prune_stale_peers(&self, max_age_secs: u64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let mut peers = self.peers.write();
        let stale: Vec<Id> = peers
            .iter()
            .filter(|(_, state)| {
                state.is_disconnected() && (now - state.last_seen()) >= max_age_secs
            })
            .map(|(id, _)| id.clone())
            .collect();

        for id in &stale {
            peers.remove(id);
            self.registry.remove_by_id(id);
        }

        if !stale.is_empty() {
            debug!(count = stale.len(), "pruned stale peers");
        }
    }

    pub fn clear(&self) {
        self.peers.write().clear();
        self.registry.clear();
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::traits::NetPeerExt;

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
    struct TestId(u64);

    #[derive(Clone, Debug, Default)]
    struct TestExt {
        data: u32,
    }

    impl NetPeerExt for TestExt {
        type Snapshot = u32;

        fn snapshot(&self) -> Self::Snapshot {
            self.data
        }

        fn restore(&mut self, snapshot: &Self::Snapshot) {
            self.data = *snapshot;
        }

        fn from_snapshot(snapshot: &Self::Snapshot) -> Self {
            Self { data: *snapshot }
        }
    }

    fn test_peer_id(n: u8) -> PeerId {
        let bytes = [n; 32];
        let key = libp2p::identity::ed25519::SecretKey::try_from_bytes(bytes).unwrap();
        let keypair =
            libp2p::identity::Keypair::from(libp2p::identity::ed25519::Keypair::from(key));
        keypair.public().to_peer_id()
    }

    #[test]
    fn test_manager_insert_peer() {
        let manager = NetPeerManager::<TestId, TestExt>::with_defaults();

        let peer1 = manager.insert_peer(TestId(1), TestExt { data: 42 });
        assert_eq!(manager.peer_count(), 1);
        assert_eq!(peer1.ext().data, 42);

        // Second insert returns same Arc
        let peer2 = manager.insert_peer(TestId(1), TestExt { data: 99 });
        assert!(Arc::ptr_eq(&peer1, &peer2));
        assert_eq!(peer2.ext().data, 42); // Original data preserved

        // Different ID creates new peer
        let peer3 = manager.insert_peer(TestId(2), TestExt { data: 100 });
        assert!(!Arc::ptr_eq(&peer1, &peer3));
        assert_eq!(manager.peer_count(), 2);
    }

    #[test]
    fn test_manager_connection_flow() {
        let manager = NetPeerManager::<TestId, TestExt>::with_defaults();
        let id = TestId(1);
        let peer_id = test_peer_id(1);

        // Insert peer first
        manager.insert_peer(id, TestExt::default());

        // Mark connected
        assert!(manager.on_connected(&id, peer_id));
        assert!(manager.is_connected(&id));
        assert_eq!(manager.connected_count(), 1);

        // Registry should have mapping
        assert_eq!(manager.resolve_peer_id(&id), Some(peer_id));
        assert_eq!(manager.resolve_id(&peer_id), Some(id));

        // Disconnect
        let disconnected_id = manager.on_disconnected_by_peer_id(&peer_id);
        assert_eq!(disconnected_id, Some(id));
        assert!(!manager.is_connected(&id));
        assert_eq!(manager.connected_count(), 0);
    }

    #[test]
    fn test_manager_on_connected_unknown_peer() {
        let manager = NetPeerManager::<TestId, TestExt>::with_defaults();
        let id = TestId(1);
        let peer_id = test_peer_id(1);

        // on_connected without insert returns false
        assert!(!manager.on_connected(&id, peer_id));
        assert!(!manager.is_connected(&id));
    }

    #[test]
    fn test_manager_banning() {
        let manager = NetPeerManager::<TestId, TestExt>::with_defaults();
        let id = TestId(1);

        manager.insert_peer(id, TestExt::default());

        manager.ban(&id, Some("test reason".to_string()));
        assert!(manager.is_banned(&id));

        manager.unban(&id);
        assert!(!manager.is_banned(&id));
    }

    #[test]
    fn test_manager_disconnected_peers() {
        let manager = NetPeerManager::<TestId, TestExt>::with_defaults();

        // Insert some peers
        for i in 1..=5 {
            manager.insert_peer(TestId(i), TestExt::default());
        }

        // All should be disconnected initially
        assert_eq!(manager.disconnected_peers().len(), 5);

        // Connect one
        manager.on_connected(&TestId(1), test_peer_id(1));
        assert_eq!(manager.disconnected_peers().len(), 4);
        assert_eq!(manager.connected_count(), 1);
    }

    #[test]
    fn test_manager_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        let manager = Arc::new(NetPeerManager::<TestId, TestExt>::with_defaults());
        manager.insert_peer(TestId(1), TestExt::default());

        let mut handles = vec![];

        for _ in 0..10 {
            let manager_clone = Arc::clone(&manager);
            handles.push(thread::spawn(move || {
                if let Some(peer) = manager_clone.get_peer(&TestId(1)) {
                    for _ in 0..100 {
                        peer.add_score(1.0);
                    }
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let score = manager.score(&TestId(1)).unwrap();
        assert!((score - 1000.0).abs() < 1.0);
    }

    #[test]
    fn test_manager_prune_stale() {
        let manager = NetPeerManager::<TestId, TestExt>::with_defaults();

        for i in 1..=5 {
            manager.insert_peer(TestId(i), TestExt::default());
        }
        assert_eq!(manager.peer_count(), 5);

        // Prune with 0 max age
        manager.prune_stale_peers(0);

        // All should be pruned (none are connected)
        assert_eq!(manager.peer_count(), 0);
    }
}
