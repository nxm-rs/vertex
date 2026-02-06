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

use super::{KademliaConfig, PSlice, SwarmRouting, MAX_PO};

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
    pending_dials: Mutex<HashSet<OverlayAddress>>,
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

    const MAX_PENDING_CANDIDATES: usize = 64;

    fn connect_neighbours(&self, candidates: &mut Vec<OverlayAddress>, max: usize) {
        let depth = SwarmTopology::depth(self);
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

            if pending.contains(&peer) {
                continue;
            }

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
        let depth = SwarmTopology::depth(self);
        let saturation = self.config.saturation_peers;
        let pending = self.pending_dials.lock();

        let mut bin_stats: Vec<(u8, usize, usize, usize)> = Vec::new();
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

        bin_stats.sort_by(|a, b| b.0.cmp(&a.0));

        for (po, connected, known, deficit) in bin_stats {
            if candidates.len() >= max {
                break;
            }

            let pool_ratio = known as f32 / saturation as f32;
            let peers_to_add = if pool_ratio >= 4.0 {
                deficit
            } else if pool_ratio >= 2.0 {
                ((deficit * 3) / 4).max(1)
            } else if pool_ratio >= 1.0 {
                (deficit / 2).max(1)
            } else {
                (deficit / 4).max(1)
            };

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
            depth: SwarmTopology::depth(self),
            connection_candidates: self.connection_candidates.lock().len(),
        }
    }

    fn connection_failed(&self, peer: &OverlayAddress) {
        if let Some(provider) = &self.failure_provider {
            provider.record_failure(peer);
        }

        let po = self.proximity(peer);
        let is_neighbor = po >= SwarmTopology::depth(self);
        let max_attempts = if is_neighbor {
            self.config.max_neighbor_attempts
        } else {
            self.config.max_connect_attempts
        };

        let failure_score = self.get_failure_score(peer);
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

    fn get_failure_score(&self, peer: &OverlayAddress) -> f64 {
        self.failure_provider
            .as_ref()
            .map(|p| p.failure_score(peer))
            .unwrap_or(0.0)
    }

    fn should_skip_for_failures(&self, peer: &OverlayAddress) -> bool {
        let po = self.proximity(peer);
        let is_neighbor = po >= SwarmTopology::depth(self);
        let max_attempts = if is_neighbor {
            self.config.max_neighbor_attempts
        } else {
            self.config.max_connect_attempts
        };

        let failure_score = self.get_failure_score(peer);
        let threshold = -(max_attempts as f64 * 1.5);

        failure_score <= threshold
    }

    /// Log the current routing status showing bin populations.
    pub fn log_status(&self) {
        let connected_bins = self.connected_peers.bin_sizes();
        let known_bins = self.known_peers.bin_sizes();
        let depth = SwarmTopology::depth(self);

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

    pub fn connected_peers_in_bin_addrs(&self, po: u8) -> Vec<OverlayAddress> {
        self.connected_peers.peers_in_bin(po)
    }

    fn add_known_peers(&self, peers: &[OverlayAddress]) {
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

    fn peer_connected(&self, peer: OverlayAddress) {
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

    fn peer_disconnected(&self, peer: &OverlayAddress) {
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

    fn do_remove_peer(&self, peer: &OverlayAddress) {
        self.known_peers.remove(peer);
        self.connected_peers.remove(peer);
        debug!(%peer, "removed peer from routing");
    }
}

impl<I: SwarmIdentity> SwarmTopology for KademliaRouting<I> {
    type Identity = I;

    fn identity(&self) -> &Self::Identity {
        &self.identity
    }

    fn depth(&self) -> u8 {
        self.depth.load(Ordering::Relaxed)
    }

    fn neighbors(&self, depth: u8) -> Vec<OverlayAddress> {
        self.connected_peers
            .iter_by_proximity()
            .filter(|(po, _)| *po >= depth)
            .map(|(_, peer)| peer)
            .collect()
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

    fn connected_peers_count(&self) -> usize {
        self.connected_peers.len()
    }

    fn known_peers_count(&self) -> usize {
        self.known_peers.len()
    }

    fn pending_connections_count(&self) -> usize {
        self.pending_dials.lock().len()
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

impl<I: SwarmIdentity> SwarmRouting<I> for KademliaRouting<I> {
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
        let score = self.get_failure_score(peer);
        if score >= 0.0 {
            0
        } else {
            ((-score) / 1.5).ceil() as u32
        }
    }

    fn remove_peer(&self, peer: &OverlayAddress) {
        self.do_remove_peer(peer);
    }

    fn evaluate_connections(&self) {
        let mut new_candidates = Vec::new();

        self.connect_neighbours(&mut new_candidates, self.config.max_neighbor_candidates);
        let neighbor_candidates = new_candidates.len();

        self.connect_balanced(&mut new_candidates, self.config.max_balanced_candidates);
        let balanced_candidates = new_candidates.len() - neighbor_candidates;

        let mut candidates = self.connection_candidates.lock();
        let before_len = candidates.len();

        for candidate in new_candidates {
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }

        if candidates.len() > Self::MAX_PENDING_CANDIDATES {
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

    fn mark_pending_dial(&self, peer: OverlayAddress) {
        self.pending_dials.lock().insert(peer);
    }

    fn clear_pending_dial(&self, peer: &OverlayAddress) {
        self.pending_dials.lock().remove(peer);
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
        assert_eq!(SwarmTopology::depth(&*routing), 0);
    }

    #[test]
    fn test_add_and_connect_peers() {
        let base = addr_from_byte(0x00);
        let config = KademliaConfig::default();
        let routing = make_routing(base, config);

        let peer1 = addr_from_byte(0x80);
        let peer2 = addr_from_byte(0x40);

        SwarmRouting::add_peers(&*routing, &[peer1, peer2]);
        assert_eq!(routing.known_peers().len(), 2);
        assert_eq!(routing.connected_peers().len(), 0);

        SwarmRouting::connected(&*routing, peer1);
        assert_eq!(routing.known_peers().len(), 1);
        assert_eq!(routing.connected_peers().len(), 1);

        SwarmRouting::connected(&*routing, peer2);
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

        assert!(SwarmRouting::should_accept_peer(&*routing, &peer1, false));

        SwarmRouting::connected(&*routing, peer1);
        assert!(SwarmRouting::should_accept_peer(&*routing, &peer2, true));

        SwarmRouting::connected(&*routing, peer2);
        assert!(!SwarmRouting::should_accept_peer(&*routing, &peer3, true));
    }

    #[test]
    fn test_disconnect_and_reconnect() {
        let base = addr_from_byte(0x00);
        let config = KademliaConfig::default();
        let routing = make_routing(base, config);

        let peer = addr_from_byte(0x80);

        SwarmRouting::add_peers(&*routing, &[peer]);
        SwarmRouting::connected(&*routing, peer);
        assert!(routing.connected_peers().contains(&peer));
        assert!(!routing.known_peers().contains(&peer));

        SwarmRouting::disconnected(&*routing, &peer);
        assert!(!routing.connected_peers().contains(&peer));
        assert!(routing.known_peers().contains(&peer));
    }

    #[test]
    fn test_depth_calculation() {
        let base = addr_from_byte(0x00);
        let config = KademliaConfig::default().with_low_watermark(2);
        let routing = make_routing(base, config);

        assert_eq!(SwarmTopology::depth(&*routing), 0);

        let mut peer_bytes1 = [0x00u8; 32];
        peer_bytes1[0] = 0x04;
        let peer1 = OverlayAddress::from(peer_bytes1);

        let mut peer_bytes2 = [0x00u8; 32];
        peer_bytes2[0] = 0x05;
        let peer2 = OverlayAddress::from(peer_bytes2);

        SwarmRouting::connected(&*routing, peer1);
        SwarmRouting::connected(&*routing, peer2);

        assert_eq!(SwarmTopology::depth(&*routing), 5);
    }

    #[test]
    fn test_closest_to() {
        let base = addr_from_byte(0x00);
        let config = KademliaConfig::default();
        let routing = make_routing(base, config);

        let peer_po0 = addr_from_byte(0x80);
        let peer_po1 = addr_from_byte(0x40);
        let peer_po2 = addr_from_byte(0x20);

        SwarmRouting::connected(&*routing, peer_po0);
        SwarmRouting::connected(&*routing, peer_po1);
        SwarmRouting::connected(&*routing, peer_po2);

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

        SwarmRouting::connected(&*routing, peer_po0);
        SwarmRouting::connected(&*routing, peer_po1);
        SwarmRouting::connected(&*routing, peer_po5);

        let neighbors_d0 = routing.neighbors(0);
        assert_eq!(neighbors_d0.len(), 3);

        let neighbors_d2 = routing.neighbors(2);
        assert_eq!(neighbors_d2.len(), 1);
        assert_eq!(neighbors_d2[0], peer_po5);
    }
}
