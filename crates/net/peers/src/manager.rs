//! Peer manager with Arc-per-peer pattern for minimal lock contention.

use std::collections::HashMap;
use std::sync::Arc;

use libp2p::PeerId;
use parking_lot::RwLock;
use tokio::sync::broadcast;
use tracing::{debug, trace};

use crate::events::{EventEmitter, PeerEvent};
use crate::registry::PeerRegistry;
use crate::state::{ConnectionState, NetPeerSnapshot, PeerState};
use crate::store::{ExtSnapBounds, NetPeerStore, PeerStoreError};
use crate::traits::{NetPeerExt, NetPeerId, NetPeerScoreExt};

/// Type alias for the internal peer map to avoid clippy::type_complexity.
type PeerMap<Id, Ext, ScoreExt> = HashMap<Id, Arc<PeerState<Id, Ext, ScoreExt>>>;

/// Peer manager configuration.
#[derive(Debug, Clone)]
pub struct NetPeerManagerConfig {
    /// Score threshold below which peers get banned.
    pub ban_threshold: f64,
    /// Maximum peers to track. None = unlimited (for bootnodes/crawlers).
    pub max_peers: Option<usize>,
    /// Broadcast channel capacity for peer events.
    pub event_channel_capacity: usize,
}

impl Default for NetPeerManagerConfig {
    fn default() -> Self {
        Self {
            ban_threshold: -100.0,
            max_peers: Some(10_000),
            event_channel_capacity: 256,
        }
    }
}

impl NetPeerManagerConfig {
    /// Config for bootnodes/crawlers with unlimited peer tracking.
    pub fn unlimited() -> Self {
        Self {
            max_peers: None,
            ..Default::default()
        }
    }
}

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
/// The `Ext` type parameter allows protocols to add custom state to each peer.
/// The `ScoreExt` type parameter allows protocols to add custom scoring metrics.
pub struct NetPeerManager<Id: NetPeerId, Ext: NetPeerExt = (), ScoreExt: NetPeerScoreExt = ()> {
    config: NetPeerManagerConfig,
    /// Brief lock to get Arc, then release.
    peers: RwLock<PeerMap<Id, Ext, ScoreExt>>,
    registry: PeerRegistry<Id>,
    events: EventEmitter<Id>,
}

impl<Id: NetPeerId, Ext: NetPeerExt, ScoreExt: NetPeerScoreExt> NetPeerManager<Id, Ext, ScoreExt> {
    pub fn new(config: NetPeerManagerConfig) -> Self {
        Self {
            events: EventEmitter::new(config.event_channel_capacity),
            config,
            peers: RwLock::new(HashMap::new()),
            registry: PeerRegistry::new(),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(NetPeerManagerConfig::default())
    }

    pub fn config(&self) -> &NetPeerManagerConfig {
        &self.config
    }

    /// Get or create peer state. Returns Arc that can be cached for lock-free access.
    ///
    /// Callers should cache the returned Arc along with the id for efficient access.
    pub fn peer(&self, id: Id) -> Arc<PeerState<Id, Ext, ScoreExt>> {
        // Fast path: read lock
        {
            let peers = self.peers.read();
            if let Some(state) = peers.get(&id) {
                return Arc::clone(state);
            }
        }

        // Slow path: write lock (only on first access per peer)
        let mut peers = self.peers.write();

        // Double-check after acquiring write lock
        if let Some(state) = peers.get(&id) {
            return Arc::clone(state);
        }

        // Prune if at capacity (before adding new peer)
        if let Some(max) = self.config.max_peers {
            if peers.len() >= max {
                self.prune_one_peer(&mut peers);
            }
        }

        // Create new peer state
        let state = Arc::new(PeerState::new());
        peers.insert(id.clone(), Arc::clone(&state));

        debug!(?id, "new peer added to manager");
        self.events.peer_discovered(id);

        state
    }

    /// Prune one peer to make room. Uses heuristic:
    /// 1. Banned peers (oldest ban first)
    /// 2. Disconnected peers (lowest score first)
    /// 3. Known peers (oldest last_seen first)
    ///
    /// Never prunes: Connected, Connecting peers
    fn prune_one_peer(&self, peers: &mut PeerMap<Id, Ext, ScoreExt>) {
        // Collect prunable peers with their priority info
        let mut candidates: Vec<(Id, PrunePriority)> = peers
            .iter()
            .filter_map(|(id, state)| {
                let conn_state = state.connection_state();
                // Never prune connected or connecting peers
                if conn_state.is_connected() || conn_state == ConnectionState::Connecting {
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
            // All peers are connected/connecting, can't prune
            trace!("no prunable peers found");
            return;
        }

        // Sort by prune priority (worst first)
        candidates.sort_by(|a, b| b.1.cmp(&a.1));

        // Remove the worst peer
        if let Some((id, _)) = candidates.first() {
            peers.remove(id);
            self.registry.remove_by_id(id);
            debug!(?id, "pruned peer to stay under max_peers");
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

    /// Returns true if transition was valid (peer was dialable).
    pub fn start_connecting(&self, id: Id) -> bool {
        let peer = self.peer(id.clone());
        let old_state = peer.connection_state();

        if !old_state.is_dialable() {
            trace!(?id, ?old_state, "peer not dialable");
            return false;
        }

        peer.set_connection_state(ConnectionState::Connecting);
        self.events
            .state_changed(id.clone(), old_state, ConnectionState::Connecting);
        self.events.peer_connecting(id);

        true
    }

    /// Mark peer connected and register Id ↔ PeerId mapping.
    pub fn on_connected(&self, id: Id, peer_id: PeerId) {
        let peer = self.peer(id.clone());
        let old_state = peer.connection_state();

        peer.set_connection_state(ConnectionState::Connected);

        // Register in bidirectional registry
        self.registry.register(id.clone(), peer_id);

        self.events
            .state_changed(id.clone(), old_state, ConnectionState::Connected);
        self.events.peer_connected(id, Some(peer_id));

        debug!(peer_id = %peer_id, "peer connected");
    }

    /// Handle disconnection by libp2p PeerId. Returns protocol ID if found.
    ///
    /// Note: The registry mapping is preserved after disconnect to allow resolving
    /// peer_id → overlay for buffered events (e.g., hive data arriving after disconnect).
    /// The mapping is cleaned up when the peer is removed from the store or reconnects
    /// with a different peer_id.
    pub fn on_disconnected_by_peer_id(&self, peer_id: &PeerId) -> Option<Id> {
        // Resolve protocol ID from registry (don't remove - keep for buffered event handling)
        let id = self.registry.resolve_id(peer_id)?;

        if let Some(peer) = self.get_peer(&id) {
            let old_state = peer.connection_state();
            peer.set_connection_state(ConnectionState::Disconnected);

            self.events
                .state_changed(id.clone(), old_state, ConnectionState::Disconnected);
            self.events.peer_disconnected(id.clone(), Some(*peer_id));
            debug!(peer_id = %peer_id, "peer disconnected");
        }

        Some(id)
    }

    pub fn on_disconnected(&self, id: &Id) {
        if let Some(peer) = self.get_peer(id) {
            let old_state = peer.connection_state();
            peer.set_connection_state(ConnectionState::Disconnected);

            // Don't remove from registry - keep mapping for buffered event handling
            let peer_id = self.registry.resolve_peer(id);

            self.events
                .state_changed(id.clone(), old_state, ConnectionState::Disconnected);
            self.events.peer_disconnected(id.clone(), peer_id);
        }
    }

    pub fn ban(&self, id: Id, reason: Option<String>) {
        let peer = self.peer(id.clone());
        peer.ban(reason.clone());

        // Remove from registry (disconnect if connected)
        let _ = self.registry.remove_by_id(&id);

        self.events.peer_banned(id, reason);
    }

    pub fn unban(&self, id: &Id) {
        if let Some(peer) = self.get_peer(id) {
            peer.unban();
            self.events.peer_unbanned(id.clone());
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

    pub fn subscribe(&self) -> broadcast::Receiver<PeerEvent<Id>> {
        self.events.subscribe()
    }

    pub fn events(&self) -> &EventEmitter<Id> {
        &self.events
    }

    pub fn load_from_store<S>(&self, store: &S) -> Result<usize, PeerStoreError>
    where
        S: NetPeerStore<Id, Ext::Snapshot, ScoreExt::Snapshot> + ?Sized,
        Ext::Snapshot: ExtSnapBounds,
        ScoreExt::Snapshot: ExtSnapBounds,
    {
        let snapshots = store.load_all()?;
        let count = snapshots.len();

        let mut peers = self.peers.write();
        for snapshot in snapshots {
            let (id, state) = snapshot.into_state::<Ext, ScoreExt>();
            peers.insert(id, Arc::new(state));
        }

        debug!(count, "loaded peers from store");
        Ok(count)
    }

    pub fn save_to_store<S>(&self, store: &S) -> Result<usize, PeerStoreError>
    where
        S: NetPeerStore<Id, Ext::Snapshot, ScoreExt::Snapshot> + ?Sized,
        Ext::Snapshot: ExtSnapBounds,
        ScoreExt::Snapshot: ExtSnapBounds,
    {
        let peers = self.peers.read();
        let snapshots: Vec<NetPeerSnapshot<Id, Ext::Snapshot, ScoreExt::Snapshot>> = peers
            .iter()
            .map(|(id, state)| state.snapshot(id.clone()))
            .collect();
        let count = snapshots.len();
        drop(peers);

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
            .filter(|(_, state)| !state.is_connected() && (now - state.last_seen()) >= max_age_secs)
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

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
    struct TestId(u64);

    fn test_peer_id(n: u8) -> PeerId {
        let bytes = [n; 32];
        let key = libp2p::identity::ed25519::SecretKey::try_from_bytes(bytes).unwrap();
        let keypair =
            libp2p::identity::Keypair::from(libp2p::identity::ed25519::Keypair::from(key));
        keypair.public().to_peer_id()
    }

    #[test]
    fn test_manager_peer_creation() {
        let manager = NetPeerManager::<TestId>::with_defaults();

        // First access creates peer
        let peer1 = manager.peer(TestId(1));
        assert_eq!(manager.peer_count(), 1);

        // Second access returns same Arc
        let peer2 = manager.peer(TestId(1));
        assert!(Arc::ptr_eq(&peer1, &peer2));
        assert_eq!(manager.peer_count(), 1);

        // Different ID creates new peer
        let peer3 = manager.peer(TestId(2));
        assert!(!Arc::ptr_eq(&peer1, &peer3));
        assert_eq!(manager.peer_count(), 2);
    }

    #[test]
    fn test_manager_connection_flow() {
        let manager = NetPeerManager::<TestId>::with_defaults();
        let id = TestId(1);
        let peer_id = test_peer_id(1);

        // Start connecting
        assert!(manager.start_connecting(id));
        assert!(!manager.is_connected(&id));

        // Mark connected
        manager.on_connected(id, peer_id);
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

        // Registry mapping should be preserved for buffered event handling
        assert_eq!(manager.resolve_peer_id(&id), Some(peer_id));
    }

    #[test]
    fn test_manager_banning() {
        let manager = NetPeerManager::<TestId>::with_defaults();
        let id = TestId(1);

        // Create peer
        let _ = manager.peer(id);

        // Ban
        manager.ban(id, Some("test reason".to_string()));
        assert!(manager.is_banned(&id));

        // Can't connect to banned peer
        assert!(!manager.start_connecting(id));

        // Unban
        manager.unban(&id);
        assert!(!manager.is_banned(&id));
        assert!(manager.start_connecting(id));
    }

    #[test]
    fn test_manager_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        let manager = Arc::new(NetPeerManager::<TestId>::with_defaults());
        let mut handles = vec![];

        // Multiple threads accessing same peer
        for _ in 0..10 {
            let manager_clone = Arc::clone(&manager);
            handles.push(thread::spawn(move || {
                let peer = manager_clone.peer(TestId(1));
                for _ in 0..100 {
                    peer.add_score(1.0);
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // Score should be approximately 1000
        let score = manager.score(&TestId(1)).unwrap();
        assert!((score - 1000.0).abs() < 1.0);
    }

    #[tokio::test]
    async fn test_manager_events() {
        let manager = NetPeerManager::<TestId>::with_defaults();
        let mut rx = manager.subscribe();

        // Create peer (emits Discovered event)
        let _ = manager.peer(TestId(1));

        let event = rx.recv().await.unwrap();
        match event {
            PeerEvent::Discovered { id } => assert_eq!(id, TestId(1)),
            _ => panic!("expected Discovered event"),
        }
    }

    #[test]
    fn test_manager_peer_state_independence() {
        let manager = NetPeerManager::<TestId>::with_defaults();

        // Get Arc for peer 1
        let peer1 = manager.peer(TestId(1));
        peer1.set_score(50.0);

        // Get Arc for peer 2
        let peer2 = manager.peer(TestId(2));
        peer2.set_score(100.0);

        // Updates to one don't affect the other
        peer1.add_score(10.0);
        assert!((peer1.score() - 60.0).abs() < 0.001);
        assert!((peer2.score() - 100.0).abs() < 0.001);
    }

    #[test]
    fn test_manager_prune_stale() {
        let manager = NetPeerManager::<TestId>::with_defaults();

        // Create some peers
        for i in 1..=5 {
            let _ = manager.peer(TestId(i));
        }
        assert_eq!(manager.peer_count(), 5);

        // Prune with 0 max age (all disconnected peers are stale)
        manager.prune_stale_peers(0);

        // All should be pruned (none are connected)
        assert_eq!(manager.peer_count(), 0);
    }
}
