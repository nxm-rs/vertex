//! Kademlia-based peer routing for Swarm.

use std::collections::HashSet;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

use nectar_primitives::ChunkAddress;
use parking_lot::Mutex;
use tracing::{debug, info, trace};
use vertex_swarm_api::{SwarmIdentity, SwarmTopology};
use vertex_swarm_primitives::OverlayAddress;

use super::{KademliaConfig, PSlice, MAX_PO};

/// Provides failure state for peers (typically implemented by PeerManager).
pub trait PeerFailureProvider: Send + Sync {
    /// Get the failure score for a peer. Higher = more failures.
    fn failure_score(&self, peer: &OverlayAddress) -> f64;

    /// Record a connection failure for a peer.
    fn record_failure(&self, peer: &OverlayAddress);
}

/// Kademlia-based peer routing table.
///
/// Maintains known and connected peer sets, provides connection decisions
/// based on Kademlia distance metrics. Generic over identity type.
pub struct KademliaRouting<I: SwarmIdentity> {
    identity: I,
    known_peers: PSlice,
    connected_peers: PSlice,
    depth: AtomicU8,
    config: KademliaConfig,
    connection_candidates: Mutex<Vec<OverlayAddress>>,
    /// Peers currently being dialed (to avoid re-selecting them).
    pending_dials: Mutex<HashSet<OverlayAddress>>,
    /// Optional provider for failure state (typically PeerManager).
    failure_provider: Option<Arc<dyn PeerFailureProvider>>,
}

impl<I: SwarmIdentity> KademliaRouting<I> {
    pub fn new(identity: I, config: KademliaConfig) -> Arc<Self> {
        Arc::new(Self {
            identity,
            known_peers: PSlice::new(),
            connected_peers: PSlice::new(),
            depth: AtomicU8::new(0),
            config,
            connection_candidates: Mutex::new(Vec::new()),
            pending_dials: Mutex::new(HashSet::new()),
            failure_provider: None,
        })
    }

    /// Create with a failure provider for delegating failure tracking.
    pub fn with_failure_provider(
        identity: I,
        config: KademliaConfig,
        failure_provider: Arc<dyn PeerFailureProvider>,
    ) -> Arc<Self> {
        Arc::new(Self {
            identity,
            known_peers: PSlice::new(),
            connected_peers: PSlice::new(),
            depth: AtomicU8::new(0),
            config,
            connection_candidates: Mutex::new(Vec::new()),
            pending_dials: Mutex::new(HashSet::new()),
            failure_provider: Some(failure_provider),
        })
    }

    fn base(&self) -> OverlayAddress {
        self.identity.overlay_address()
    }

    fn proximity(&self, peer: &OverlayAddress) -> u8 {
        self.base().proximity(peer)
    }

    /// Maximum pending connection candidates to prevent unbounded growth.
    const MAX_PENDING_CANDIDATES: usize = 64;

    /// Evaluate and update connection candidates based on routing needs.
    ///
    /// Uses separate candidate pools for neighbors (depth bins) and balanced (other bins)
    /// to ensure parallel progress on both connectivity goals.
    ///
    /// New candidates are appended to existing ones (with deduplication) rather than
    /// replacing them, ensuring candidates aren't lost between evaluations.
    pub fn evaluate_connections(&self) {
        let mut new_candidates = Vec::new();

        // Neighbors get their own pool of slots
        self.connect_neighbours(&mut new_candidates, self.config.max_neighbor_candidates);
        let neighbor_candidates = new_candidates.len();

        // Balanced gets a separate pool - not competing with neighbors
        self.connect_balanced(&mut new_candidates, self.config.max_balanced_candidates);
        let balanced_candidates = new_candidates.len() - neighbor_candidates;

        // Append to existing candidates instead of replacing (with dedup)
        let mut candidates = self.connection_candidates.lock();
        let before_len = candidates.len();

        for candidate in new_candidates {
            // Only add if not already queued
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }

        // Cap total to prevent unbounded growth
        if candidates.len() > Self::MAX_PENDING_CANDIDATES {
            // Keep the most recent candidates (they're likely more relevant)
            let excess = candidates.len() - Self::MAX_PENDING_CANDIDATES;
            candidates.drain(0..excess);
        }

        let added = candidates.len() - before_len;
        let total = candidates.len();

        if added > 0 {
            debug!(
                added,
                total,
                neighbors = neighbor_candidates,
                balanced = balanced_candidates,
                "evaluated connection candidates"
            );
        } else if total > 0 {
            trace!(total, "no new candidates (existing pending)");
        } else {
            trace!("no connection candidates");
        }
    }

    /// Mark a peer as having a dial in progress.
    ///
    /// Called when a dial is initiated. The peer will be excluded from candidate
    /// selection until `clear_pending_dial` is called.
    pub fn mark_pending_dial(&self, peer: OverlayAddress) {
        self.pending_dials.lock().insert(peer);
    }

    /// Clear the pending dial status for a peer.
    ///
    /// Called when a dial completes (success or failure).
    pub fn clear_pending_dial(&self, peer: &OverlayAddress) {
        self.pending_dials.lock().remove(peer);
    }

    fn connect_neighbours(&self, candidates: &mut Vec<OverlayAddress>, max: usize) {
        let depth = self.depth();
        let pending = self.pending_dials.lock();

        for (po, peer) in self.known_peers.iter_by_proximity_desc() {
            if candidates.len() >= max {
                break;
            }

            if po < depth {
                break;
            }

            if self.connected_peers.exists(&peer) {
                continue;
            }

            // Skip if already dialing
            if pending.contains(&peer) {
                continue;
            }

            // Skip if bin is saturated
            if self.connected_peers.bin_size(po) >= self.config.saturation_peers {
                continue;
            }

            if self.should_skip_for_failures(&peer) {
                continue;
            }

            candidates.push(peer);
        }
    }

    fn connect_balanced(&self, candidates: &mut Vec<OverlayAddress>, max: usize) {
        let depth = self.depth();
        let saturation = self.config.saturation_peers;
        let pending = self.pending_dials.lock();

        // Collect undersaturated bins with their stats for prioritized processing
        let mut bin_stats: Vec<(u8, usize, usize, usize)> = Vec::new(); // (po, connected, known, deficit)
        for po in 0..depth {
            let connected = self.connected_peers.bin_size(po);
            if connected >= saturation {
                continue;
            }
            let known = self.known_peers.bin_size(po);
            if known == 0 {
                continue;
            }
            let deficit = saturation - connected;
            bin_stats.push((po, connected, known, deficit));
        }

        // Sort bins by priority: closer bins (higher PO) first, then by deficit/known ratio
        // Closer bins matter more for replication. Within similar distance, prioritize
        // bins where we have a large pool relative to what we need.
        bin_stats.sort_by(|a, b| {
            // Primary: higher PO first (closer = more important)
            b.0.cmp(&a.0)
        });

        for (po, connected, known, deficit) in bin_stats {
            if candidates.len() >= max {
                break;
            }

            // Calculate aggressiveness based on pool size relative to saturation goal.
            // If we know 1000 peers and need 8, we can be very aggressive.
            // If we know 10 peers and need 8, we need to be careful.
            //
            // Aggressiveness formula:
            // - Base: try to fill the full deficit if pool is >= 4x saturation target
            // - Scale down if pool is smaller
            // - Minimum of 1 candidate per bin to ensure progress
            let pool_ratio = known as f32 / saturation as f32;
            let peers_to_add = if pool_ratio >= 4.0 {
                // Large pool: be very aggressive, try full deficit
                deficit
            } else if pool_ratio >= 2.0 {
                // Medium pool: try 3/4 of deficit
                ((deficit * 3) / 4).max(1)
            } else if pool_ratio >= 1.0 {
                // Pool roughly matches saturation: try half deficit
                (deficit / 2).max(1)
            } else {
                // Small pool: be conservative but still make progress
                (deficit / 4).max(1)
            };

            // Cap peers_to_add to available pool (known peers minus already connected)
            let available_in_pool = known.saturating_sub(connected);
            let peers_to_add = peers_to_add.min(available_in_pool);

            let mut added = 0;
            for peer in self.known_peers.peers_in_bin(po) {
                if candidates.len() >= max || added >= peers_to_add {
                    break;
                }
                if !self.connected_peers.exists(&peer)
                    && !pending.contains(&peer)
                    && !self.should_skip_for_failures(&peer)
                {
                    candidates.push(peer);
                    added += 1;
                }
            }

            if added > 0 {
                trace!(
                    po,
                    connected,
                    known,
                    deficit,
                    added,
                    "connect_balanced added candidates for bin"
                );
            }
        }
    }

    fn recalc_depth(&self) -> u8 {
        for po in (0..=MAX_PO).rev() {
            if self.connected_peers.bin_size(po) >= self.config.low_watermark {
                return po;
            }
        }
        0
    }

    pub fn stats(&self) -> RoutingStats {
        RoutingStats {
            known_peers: self.known_peers.len(),
            connected_peers: self.connected_peers.len(),
            depth: self.depth(),
            connection_candidates: self.connection_candidates.lock().len(),
        }
    }

    /// Mark a connection attempt as failed.
    ///
    /// Delegates to the failure provider if available, otherwise tracks locally.
    /// Peers with excessive failures are pruned from known_peers.
    pub fn connection_failed(&self, peer: &OverlayAddress) {
        // Record failure via provider if available
        if let Some(provider) = &self.failure_provider {
            provider.record_failure(peer);
        }

        let po = self.proximity(peer);
        let is_neighbor = po >= self.depth();
        let max_attempts = if is_neighbor {
            self.config.max_neighbor_attempts
        } else {
            self.config.max_connect_attempts
        };

        // Check if peer should be pruned based on failure score
        let failure_score = self.get_failure_score(peer);
        // Score threshold: each failure adds ~1.5 to negative score
        let threshold = -(max_attempts as f64 * 1.5);

        if failure_score <= threshold {
            if self.known_peers.remove(peer) {
                debug!(
                    %peer,
                    po,
                    failure_score,
                    threshold,
                    "pruned peer from known_peers after max connection attempts"
                );
            }
        } else {
            trace!(
                %peer,
                po,
                failure_score,
                threshold,
                "recorded connection failure"
            );
        }
    }

    /// Get failure score for a peer (from provider or default 0).
    fn get_failure_score(&self, peer: &OverlayAddress) -> f64 {
        self.failure_provider
            .as_ref()
            .map(|p| p.failure_score(peer))
            .unwrap_or(0.0)
    }

    /// Check if a peer should be skipped due to recent failures.
    fn should_skip_for_failures(&self, peer: &OverlayAddress) -> bool {
        let po = self.proximity(peer);
        let is_neighbor = po >= self.depth();
        let max_attempts = if is_neighbor {
            self.config.max_neighbor_attempts
        } else {
            self.config.max_connect_attempts
        };

        let failure_score = self.get_failure_score(peer);
        // Score threshold: each failure adds ~1.5 to negative score
        let threshold = -(max_attempts as f64 * 1.5);

        failure_score <= threshold
    }

    /// Log the current routing status showing bin populations.
    pub fn log_status(&self) {
        let connected_bins = self.connected_peers.bin_sizes();
        let known_bins = self.known_peers.bin_sizes();
        let depth = self.depth();

        let mut bin_status = String::new();
        for po in 0..=MAX_PO {
            let c = connected_bins[po as usize];
            let k = known_bins[po as usize];
            if c > 0 || k > 0 {
                if !bin_status.is_empty() {
                    bin_status.push(' ');
                }
                if po == depth {
                    bin_status.push_str(&format!("[{po}:{c}/{k}]"));
                } else {
                    bin_status.push_str(&format!("{po}:{c}/{k}"));
                }
            }
        }

        let total_connected: usize = connected_bins.iter().sum();
        let total_known: usize = known_bins.iter().sum();

        if bin_status.is_empty() {
            bin_status = "(empty)".to_string();
        }

        debug!(
            depth,
            connected = total_connected,
            known = total_known,
            bins = %bin_status,
            "kademlia routing"
        );
    }

    pub fn connected_peers(&self) -> Vec<OverlayAddress> {
        self.connected_peers.all_peers()
    }

    pub fn known_peers(&self) -> Vec<OverlayAddress> {
        self.known_peers.all_peers()
    }

    pub fn bin_sizes(&self) -> Vec<(usize, usize)> {
        let connected = self.connected_peers.bin_sizes();
        let known = self.known_peers.bin_sizes();
        connected
            .iter()
            .zip(known.iter())
            .map(|(c, k)| (*c, *k))
            .collect()
    }

    pub fn connected_peers_in_bin(&self, po: u8) -> Vec<OverlayAddress> {
        self.connected_peers.peers_in_bin(po)
    }

    /// Remove a peer from all routing state.
    ///
    /// Removes from known peers and connected peers.
    /// Use when a peer is banned and should not be reconnected.
    pub fn remove_peer(&self, peer: &OverlayAddress) {
        self.known_peers.remove(peer);
        self.connected_peers.remove(peer);
        // Note: Failure/ban state is managed by PeerManager
        debug!(%peer, "removed peer from routing");
    }

    /// Add peers to the known peers set (crate-internal).
    ///
    /// Use this from TopologyBehaviour to add discovered peers directly.
    pub(crate) fn add_known_peers(&self, peers: &[OverlayAddress]) {
        let mut added = 0;
        for peer in peers {
            let po = self.proximity(peer);
            if self.known_peers.add(*peer, po) {
                added += 1;
            }
        }

        if added > 0 {
            debug!(added, total = self.known_peers.len(), "added known peers");
        }
    }

    /// Mark a peer as connected (crate-internal).
    ///
    /// Use this from TopologyBehaviour when a peer completes handshake.
    pub(crate) fn peer_connected(&self, peer: OverlayAddress) {
        // Note: Failure state is managed by PeerManager via record_success()

        let po = self.proximity(&peer);

        if self.connected_peers.add(peer, po) {
            self.known_peers.remove(&peer);

            let old_depth = self.depth.load(Ordering::Relaxed);
            let new_depth = self.recalc_depth();
            self.depth.store(new_depth, Ordering::Relaxed);

            debug!(
                %peer,
                po,
                depth = new_depth,
                connected = self.connected_peers.len(),
                "peer connected"
            );

            if new_depth != old_depth {
                info!(old_depth, new_depth, "kademlia depth changed");
                self.log_status();
            }
        }
    }

    /// Mark a peer as disconnected (crate-internal).
    ///
    /// Use this from TopologyBehaviour when all connections to a peer close.
    pub(crate) fn peer_disconnected(&self, peer: &OverlayAddress) {
        if self.connected_peers.remove(peer) {
            let po = self.proximity(peer);

            self.known_peers.add(*peer, po);

            let old_depth = self.depth.load(Ordering::Relaxed);
            let new_depth = self.recalc_depth();
            self.depth.store(new_depth, Ordering::Relaxed);

            debug!(
                %peer,
                po,
                depth = new_depth,
                connected = self.connected_peers.len(),
                "peer disconnected"
            );

            if new_depth != old_depth {
                info!(old_depth, new_depth, "kademlia depth changed");
                self.log_status();
            }
        }
    }
}

impl<I: SwarmIdentity> SwarmTopology for KademliaRouting<I> {
    type Identity = I;

    fn identity(&self) -> &Self::Identity {
        &self.identity
    }

    fn neighbors(&self, depth: u8) -> Vec<OverlayAddress> {
        self.connected_peers
            .iter_by_proximity()
            .filter(|(po, _)| *po >= depth)
            .map(|(_, peer)| peer)
            .collect()
    }

    fn depth(&self) -> u8 {
        self.depth.load(Ordering::Relaxed)
    }

    fn closest_to(&self, address: &ChunkAddress, count: usize) -> Vec<OverlayAddress> {
        let mut peers_with_distance: Vec<_> = self
            .connected_peers
            .all_peers()
            .into_iter()
            .map(|peer| {
                let po = address.proximity(&peer);
                (peer, po)
            })
            .collect();

        peers_with_distance.sort_by(|a, b| b.1.cmp(&a.1));

        peers_with_distance
            .into_iter()
            .take(count)
            .map(|(peer, _)| peer)
            .collect()
    }

    fn add_peers(&self, peers: &[OverlayAddress]) {
        self.add_known_peers(peers);
    }

    fn should_accept_peer(&self, peer: &OverlayAddress, is_full_node: bool) -> bool {
        let po = self.proximity(peer);
        let bin_size = self.connected_peers.bin_size(po);

        if is_full_node {
            bin_size < self.config.high_watermark
        } else {
            bin_size < self.config.max_peers_per_bin()
        }
    }

    fn connected(&self, peer: OverlayAddress) {
        self.peer_connected(peer);
    }

    fn disconnected(&self, peer: &OverlayAddress) {
        self.peer_disconnected(peer);
    }

    fn peers_to_connect(&self) -> Vec<OverlayAddress> {
        std::mem::take(&mut *self.connection_candidates.lock())
    }

    fn record_connection_failure(&self, peer: &OverlayAddress) {
        self.connection_failed(peer);
    }

    fn is_temporarily_unavailable(&self, peer: &OverlayAddress) -> bool {
        self.should_skip_for_failures(peer)
    }

    fn failure_count(&self, peer: &OverlayAddress) -> u32 {
        // Convert failure score to approximate count (each failure adds ~1.5)
        let score = self.get_failure_score(peer);
        if score >= 0.0 {
            0
        } else {
            ((-score) / 1.5).ceil() as u32
        }
    }

    fn remove_peer(&self, peer: &OverlayAddress) {
        KademliaRouting::remove_peer(self, peer);
    }
}

impl<I: SwarmIdentity> vertex_swarm_api::SwarmTopologyProvider for KademliaRouting<I> {
    fn overlay_address(&self) -> String {
        hex::encode(self.base().as_slice())
    }

    fn depth(&self) -> u8 {
        self.depth.load(Ordering::Relaxed)
    }

    fn connected_peers_count(&self) -> usize {
        self.connected_peers.len()
    }

    fn known_peers_count(&self) -> usize {
        self.known_peers.len()
    }

    fn pending_connections_count(&self) -> usize {
        // Pending connections are tracked by PeerManager, not routing
        0
    }

    fn bin_sizes(&self) -> Vec<(usize, usize)> {
        let connected = self.connected_peers.bin_sizes();
        let known = self.known_peers.bin_sizes();
        connected
            .iter()
            .zip(known.iter())
            .map(|(c, k)| (*c, *k))
            .collect()
    }

    fn connected_peers_in_bin(&self, po: u8) -> Vec<String> {
        self.connected_peers
            .peers_in_bin(po)
            .iter()
            .map(|addr| hex::encode(addr.as_slice()))
            .collect()
    }
}

/// Statistics about the routing state.
#[derive(Debug, Clone)]
pub struct RoutingStats {
    pub known_peers: usize,
    pub connected_peers: usize,
    pub depth: u8,
    pub connection_candidates: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use alloy_signer_local::LocalSigner;
    use nectar_primitives::SwarmAddress;
    use vertex_swarm_spec::Spec;

    #[derive(Clone)]
    struct MockIdentity {
        overlay: SwarmAddress,
        signer: Arc<LocalSigner<alloy_signer::k256::ecdsa::SigningKey>>,
        spec: Arc<Spec>,
    }

    impl std::fmt::Debug for MockIdentity {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("MockIdentity")
                .field("overlay", &self.overlay)
                .finish_non_exhaustive()
        }
    }

    impl MockIdentity {
        fn with_overlay(overlay: OverlayAddress) -> Self {
            let signer = LocalSigner::random();
            Self {
                overlay,
                signer: Arc::new(signer),
                spec: vertex_swarm_spec::init_testnet(),
            }
        }
    }

    impl SwarmIdentity for MockIdentity {
        type Spec = Spec;
        type Signer = LocalSigner<alloy_signer::k256::ecdsa::SigningKey>;

        fn spec(&self) -> &Self::Spec {
            &self.spec
        }

        fn nonce(&self) -> B256 {
            B256::ZERO
        }

        fn signer(&self) -> Arc<Self::Signer> {
            self.signer.clone()
        }

        fn node_type(&self) -> vertex_swarm_api::SwarmNodeType {
            vertex_swarm_api::SwarmNodeType::Storer
        }

        fn overlay_address(&self) -> SwarmAddress {
            self.overlay
        }
    }

    fn addr_from_byte(b: u8) -> OverlayAddress {
        let mut bytes = [0u8; 32];
        bytes[0] = b;
        OverlayAddress::from(bytes)
    }

    fn make_routing(
        base: OverlayAddress,
        config: KademliaConfig,
    ) -> Arc<KademliaRouting<MockIdentity>> {
        let identity = MockIdentity::with_overlay(base);
        KademliaRouting::new(identity, config)
    }

    #[test]
    fn test_routing_creation() {
        let base = addr_from_byte(0x00);
        let config = KademliaConfig::default();
        let routing = make_routing(base, config);

        assert_eq!(routing.identity().overlay_address(), base);
        assert_eq!(routing.depth(), 0);
    }

    #[test]
    fn test_add_and_connect_peers() {
        let base = addr_from_byte(0x00);
        let config = KademliaConfig::default();
        let routing = make_routing(base, config);

        let peer1 = addr_from_byte(0x80);
        let peer2 = addr_from_byte(0x40);

        routing.add_peers(&[peer1, peer2]);
        assert_eq!(routing.known_peers().len(), 2);
        assert_eq!(routing.connected_peers().len(), 0);

        routing.connected(peer1);
        assert_eq!(routing.known_peers().len(), 1);
        assert_eq!(routing.connected_peers().len(), 1);

        routing.connected(peer2);
        assert_eq!(routing.known_peers().len(), 0);
        assert_eq!(routing.connected_peers().len(), 2);
    }

    #[test]
    fn test_pick_decision() {
        let base = addr_from_byte(0x00);
        let config = KademliaConfig::default().with_high_watermark(2);
        let routing = make_routing(base, config);

        let peer1 = addr_from_byte(0x80);
        let peer2 = addr_from_byte(0xc0);
        let peer3 = addr_from_byte(0xa0);

        assert!(routing.should_accept_peer(&peer1, false));

        routing.connected(peer1);
        assert!(routing.should_accept_peer(&peer2, true));

        routing.connected(peer2);
        assert!(!routing.should_accept_peer(&peer3, true));
    }

    #[test]
    fn test_disconnect_and_reconnect() {
        let base = addr_from_byte(0x00);
        let config = KademliaConfig::default();
        let routing = make_routing(base, config);

        let peer = addr_from_byte(0x80);

        routing.add_peers(&[peer]);
        routing.connected(peer);
        assert!(routing.connected_peers().contains(&peer));
        assert!(!routing.known_peers().contains(&peer));

        routing.disconnected(&peer);
        assert!(!routing.connected_peers().contains(&peer));
        assert!(routing.known_peers().contains(&peer));
    }

    #[test]
    fn test_depth_calculation() {
        let base = addr_from_byte(0x00);
        let config = KademliaConfig::default().with_low_watermark(2);
        let routing = make_routing(base, config);

        assert_eq!(routing.depth(), 0);

        let mut peer_bytes1 = [0x00u8; 32];
        peer_bytes1[0] = 0x04;
        let peer1 = OverlayAddress::from(peer_bytes1);

        let mut peer_bytes2 = [0x00u8; 32];
        peer_bytes2[0] = 0x05;
        let peer2 = OverlayAddress::from(peer_bytes2);

        routing.connected(peer1);
        routing.connected(peer2);

        assert_eq!(routing.depth(), 5);
    }

    #[test]
    fn test_closest_to() {
        let base = addr_from_byte(0x00);
        let config = KademliaConfig::default();
        let routing = make_routing(base, config);

        let peer_po0 = addr_from_byte(0x80);
        let peer_po1 = addr_from_byte(0x40);
        let peer_po2 = addr_from_byte(0x20);

        routing.connected(peer_po0);
        routing.connected(peer_po1);
        routing.connected(peer_po2);

        let mut target_bytes = [0x00u8; 32];
        target_bytes[0] = 0x21;
        let target = ChunkAddress::from(target_bytes);

        let closest = routing.closest_to(&target, 2);
        assert_eq!(closest.len(), 2);
        assert_eq!(closest[0], peer_po2);
    }

    #[test]
    fn test_neighbors() {
        let base = addr_from_byte(0x00);
        let config = KademliaConfig::default().with_low_watermark(1);
        let routing = make_routing(base, config);

        let peer_po0 = addr_from_byte(0x80);
        let peer_po1 = addr_from_byte(0x40);
        let peer_po5 = {
            let mut bytes = [0x00u8; 32];
            bytes[0] = 0x04;
            OverlayAddress::from(bytes)
        };

        routing.connected(peer_po0);
        routing.connected(peer_po1);
        routing.connected(peer_po5);

        let neighbors_d0 = routing.neighbors(0);
        assert_eq!(neighbors_d0.len(), 3);

        let neighbors_d2 = routing.neighbors(2);
        assert_eq!(neighbors_d2.len(), 1);
        assert_eq!(neighbors_d2[0], peer_po5);
    }
}
