//! Kademlia-based peer routing for Swarm.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, AtomicU8, Ordering},
    },
};

use nectar_primitives::ChunkAddress;
use parking_lot::{Mutex, RwLock};
use tracing::{debug, info, trace};
use vertex_swarm_api::{SwarmIdentity, SwarmSpec};
use vertex_swarm_primitives::OverlayAddress;

use super::{KademliaConfig, PSlice, RoutingCapacity, SwarmRouting};

/// Connection phase for capacity tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionPhase {
    Dialing,
    Handshaking,
    Active,
}

/// Per-bin atomic counters for connection phases.
struct BinCounts {
    dialing: Vec<AtomicUsize>,
    handshaking: Vec<AtomicUsize>,
    active: Vec<AtomicUsize>,
}

impl BinCounts {
    fn new(num_bins: usize) -> Self {
        Self {
            dialing: (0..num_bins).map(|_| AtomicUsize::new(0)).collect(),
            handshaking: (0..num_bins).map(|_| AtomicUsize::new(0)).collect(),
            active: (0..num_bins).map(|_| AtomicUsize::new(0)).collect(),
        }
    }

    fn effective_count(&self, bin: u8) -> usize {
        let idx = bin as usize;
        self.dialing[idx].load(Ordering::Relaxed)
            + self.handshaking[idx].load(Ordering::Relaxed)
            + self.active[idx].load(Ordering::Relaxed)
    }

    fn inc_dialing(&self, bin: u8) {
        self.dialing[bin as usize].fetch_add(1, Ordering::Relaxed);
    }

    fn dec_dialing(&self, bin: u8) {
        self.dialing[bin as usize].fetch_sub(1, Ordering::Relaxed);
    }

    fn inc_handshaking(&self, bin: u8) {
        self.handshaking[bin as usize].fetch_add(1, Ordering::Relaxed);
    }

    fn dec_handshaking(&self, bin: u8) {
        self.handshaking[bin as usize].fetch_sub(1, Ordering::Relaxed);
    }

    fn inc_active(&self, bin: u8) {
        self.active[bin as usize].fetch_add(1, Ordering::Relaxed);
    }

    fn dec_active(&self, bin: u8) {
        self.active[bin as usize].fetch_sub(1, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn active_count(&self, bin: u8) -> usize {
        self.active[bin as usize].load(Ordering::Relaxed)
    }
}

/// Kademlia-based peer routing table.
///
/// Maintains known and connected peer sets, provides connection decisions
/// based on Kademlia distance metrics. Owns bin-based capacity tracking.
pub struct KademliaRouting<I: SwarmIdentity> {
    identity: I,
    /// Maximum proximity order from the network spec.
    max_po: u8,
    /// Discovered but not connected peers. Can be trimmed via QoS metrics if memory becomes a concern.
    pub(crate) known_peers: PSlice,
    pub(crate) connected_peers: PSlice,
    depth: AtomicU8,
    config: KademliaConfig,
    connection_candidates: Mutex<Vec<OverlayAddress>>,
    bin_counts: BinCounts,
    /// Tracks overlay → connection phase for state transitions.
    connection_phases: RwLock<HashMap<OverlayAddress, ConnectionPhase>>,
}

impl<I: SwarmIdentity> KademliaRouting<I> {
    pub fn new(identity: I, config: KademliaConfig) -> Arc<Self> {
        let max_po = identity.spec().max_po();
        let num_bins = (max_po as usize) + 1;
        Arc::new(Self {
            identity,
            max_po,
            known_peers: PSlice::new(max_po),
            connected_peers: PSlice::new(max_po),
            depth: AtomicU8::new(0),
            config,
            connection_candidates: Mutex::new(Vec::new()),
            bin_counts: BinCounts::new(num_bins),
            connection_phases: RwLock::new(HashMap::new()),
        })
    }

    /// Returns the maximum proximity order for this routing table.
    pub fn max_po(&self) -> u8 {
        self.max_po
    }

    fn base(&self) -> OverlayAddress {
        self.identity.overlay_address()
    }

    fn proximity(&self, peer: &OverlayAddress) -> u8 {
        self.base().proximity(peer)
    }

    const MAX_PENDING_CANDIDATES: usize = 64;

    fn connect_neighbours(&self, candidates: &mut Vec<OverlayAddress>, max: usize) {
        let depth = self.depth();
        let saturation = self.config.saturation_peers;

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

            let connected = self.connected_peers.bin_size(po);
            if connected >= saturation {
                continue;
            }

            candidates.push(peer);
        }
    }

    fn connect_balanced(&self, candidates: &mut Vec<OverlayAddress>, max: usize) {
        let depth = self.depth();
        let saturation = self.config.saturation_peers;

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
                if !self.connected_peers.exists(&peer) {
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
        for po in (0..=self.max_po).rev() {
            if self.connected_peers.bin_size(po) >= self.config.low_watermark {
                return po;
            }
        }
        0
    }

    /// Log the current routing status showing bin populations.
    pub fn log_status(&self) {
        let connected_bins = self.connected_peers.bin_sizes();
        let known_bins = self.known_peers.bin_sizes();
        let depth = self.depth();

        let mut bin_status = String::new();
        for po in 0..=self.max_po {
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

    pub fn depth(&self) -> u8 {
        self.depth.load(Ordering::Relaxed)
    }

    pub fn neighbors(&self, depth: u8) -> Vec<OverlayAddress> {
        self.connected_peers
            .iter_by_proximity()
            .filter(|(po, _)| *po >= depth)
            .map(|(_, peer)| peer)
            .collect()
    }

    pub fn closest_to(&self, address: &ChunkAddress, count: usize) -> Vec<OverlayAddress> {
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

    pub fn bin_sizes(&self) -> Vec<(usize, usize)> {
        let connected = self.connected_peers.bin_sizes();
        let known = self.known_peers.bin_sizes();
        connected
            .iter()
            .zip(known.iter())
            .map(|(c, k)| (*c, *k))
            .collect()
    }

    pub fn connected_peers_in_bin(&self, po: u8) -> Vec<String> {
        self.connected_peers
            .peers_in_bin(po)
            .iter()
            .map(|addr| hex::encode(addr.as_slice()))
            .collect()
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

    fn capacity_threshold(&self, is_full_node: bool) -> usize {
        if is_full_node {
            self.config.high_watermark
        } else {
            self.config.max_peers_per_bin()
        }
    }

}

impl<I: SwarmIdentity> RoutingCapacity for KademliaRouting<I> {
    fn try_reserve_dial(&self, overlay: &OverlayAddress, is_full_node: bool) -> bool {
        let po = self.proximity(overlay);
        let threshold = self.capacity_threshold(is_full_node);

        let mut phases = self.connection_phases.write();

        if phases.contains_key(overlay) {
            return false;
        }

        if self.bin_counts.effective_count(po) >= threshold {
            return false;
        }

        self.bin_counts.inc_dialing(po);
        phases.insert(*overlay, ConnectionPhase::Dialing);
        true
    }

    fn release_dial(&self, overlay: &OverlayAddress) {
        let mut phases = self.connection_phases.write();
        if let Some(ConnectionPhase::Dialing) = phases.remove(overlay) {
            let po = self.proximity(overlay);
            self.bin_counts.dec_dialing(po);
        }
    }

    fn dial_connected(&self, overlay: &OverlayAddress) {
        let po = self.proximity(overlay);
        let mut phases = self.connection_phases.write();

        if let Some(phase) = phases.get_mut(overlay) {
            if *phase == ConnectionPhase::Dialing {
                self.bin_counts.dec_dialing(po);
                self.bin_counts.inc_handshaking(po);
                *phase = ConnectionPhase::Handshaking;
            }
        }
    }

    fn handshake_completed(&self, overlay: &OverlayAddress) {
        let po = self.proximity(overlay);
        let mut phases = self.connection_phases.write();

        if let Some(phase) = phases.get_mut(overlay) {
            if *phase == ConnectionPhase::Handshaking {
                self.bin_counts.dec_handshaking(po);
                self.bin_counts.inc_active(po);
                *phase = ConnectionPhase::Active;
            }
        }
    }

    fn release_handshake(&self, overlay: &OverlayAddress) {
        let mut phases = self.connection_phases.write();
        if let Some(ConnectionPhase::Handshaking) = phases.remove(overlay) {
            let po = self.proximity(overlay);
            self.bin_counts.dec_handshaking(po);
        }
    }

    fn disconnected(&self, overlay: &OverlayAddress) {
        let mut phases = self.connection_phases.write();
        if let Some(phase) = phases.remove(overlay) {
            let po = self.proximity(overlay);
            match phase {
                ConnectionPhase::Dialing => self.bin_counts.dec_dialing(po),
                ConnectionPhase::Handshaking => self.bin_counts.dec_handshaking(po),
                ConnectionPhase::Active => self.bin_counts.dec_active(po),
            }
        }
    }

    fn should_accept_inbound(&self, overlay: &OverlayAddress, is_full_node: bool) -> bool {
        let po = self.proximity(overlay);
        let threshold = self.capacity_threshold(is_full_node);

        let phases = self.connection_phases.read();
        !phases.contains_key(overlay) && self.bin_counts.effective_count(po) < threshold
    }

    fn reserve_inbound(&self, overlay: &OverlayAddress) {
        let po = self.proximity(overlay);
        let mut phases = self.connection_phases.write();

        if !phases.contains_key(overlay) {
            self.bin_counts.inc_handshaking(po);
            phases.insert(*overlay, ConnectionPhase::Handshaking);
        }
    }
}

impl<I: SwarmIdentity> SwarmRouting<I> for KademliaRouting<I> {
    fn add_peers(&self, peers: &[OverlayAddress]) {
        self.add_known_peers(peers);
    }

    fn should_accept_peer(&self, peer: &OverlayAddress, is_full_node: bool) -> bool {
        let po = self.proximity(peer);
        let effective_count = self.bin_counts.effective_count(po);
        effective_count < self.capacity_threshold(is_full_node)
    }

    fn connected(&self, peer: OverlayAddress) {
        self.peer_connected(peer);
    }

    fn on_peer_disconnected(&self, peer: &OverlayAddress) {
        self.peer_disconnected(peer);
    }

    fn peers_to_connect(&self) -> Vec<OverlayAddress> {
        std::mem::take(&mut *self.connection_candidates.lock())
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use nectar_primitives::SwarmAddress;
    use vertex_swarm_test_utils::MockIdentity;

    fn make_routing(
        base: OverlayAddress,
        config: KademliaConfig,
    ) -> Arc<KademliaRouting<MockIdentity>> {
        let identity = MockIdentity::with_overlay(base);
        KademliaRouting::new(identity, config)
    }

    #[test]
    fn test_routing_creation() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default();
        let routing = make_routing(base, config);

        assert_eq!(routing.depth(), 0);
        assert_eq!(routing.connected_peers.len(), 0);
    }

    #[test]
    fn test_add_and_connect_peers() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default();
        let routing = make_routing(base, config);

        let peer1 = SwarmAddress::with_first_byte(0x80);
        let peer2 = SwarmAddress::with_first_byte(0x40);

        SwarmRouting::add_peers(&*routing, &[peer1, peer2]);
        assert_eq!(routing.known_peers.len(), 2);
        assert_eq!(routing.connected_peers.len(), 0);

        SwarmRouting::connected(&*routing, peer1);
        assert_eq!(routing.known_peers.len(), 1);
        assert_eq!(routing.connected_peers.len(), 1);

        SwarmRouting::connected(&*routing, peer2);
        assert_eq!(routing.known_peers.len(), 0);
        assert_eq!(routing.connected_peers.len(), 2);
    }

    #[test]
    fn test_capacity_reserve_and_release() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default().with_high_watermark(2);
        let routing = make_routing(base, config);

        let peer1 = SwarmAddress::with_first_byte(0x80); // po=0
        let peer2 = SwarmAddress::with_first_byte(0xc0); // po=0
        let peer3 = SwarmAddress::with_first_byte(0xa0); // po=0

        // First reserve succeeds
        assert!(routing.try_reserve_dial(&peer1, true));

        // Second reserve in same bin succeeds
        assert!(routing.try_reserve_dial(&peer2, true));

        // Third fails (at capacity)
        assert!(!routing.try_reserve_dial(&peer3, true));

        // Release one
        routing.release_dial(&peer1);

        // Now third succeeds
        assert!(routing.try_reserve_dial(&peer3, true));
    }

    #[test]
    fn test_capacity_state_transitions() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default().with_high_watermark(2);
        let routing = make_routing(base, config);

        let peer = SwarmAddress::with_first_byte(0x80); // po=0

        // Reserve dial
        assert!(routing.try_reserve_dial(&peer, true));
        assert_eq!(routing.bin_counts.effective_count(0), 1);

        // Transition to handshaking
        routing.dial_connected(&peer);
        assert_eq!(routing.bin_counts.effective_count(0), 1);

        // Transition to active
        routing.handshake_completed(&peer);
        assert_eq!(routing.bin_counts.effective_count(0), 1);
        assert_eq!(routing.bin_counts.active_count(0), 1);

        // Disconnect
        RoutingCapacity::disconnected(&*routing, &peer);
        assert_eq!(routing.bin_counts.effective_count(0), 0);
    }

    #[test]
    fn test_should_accept_peer() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default().with_high_watermark(2);
        let routing = make_routing(base, config);

        let peer1 = SwarmAddress::with_first_byte(0x80);
        let peer2 = SwarmAddress::with_first_byte(0xc0);
        let peer3 = SwarmAddress::with_first_byte(0xa0);

        assert!(SwarmRouting::should_accept_peer(&*routing, &peer1, true));

        // Reserve and activate peer1
        routing.try_reserve_dial(&peer1, true);
        routing.dial_connected(&peer1);
        routing.handshake_completed(&peer1);

        assert!(SwarmRouting::should_accept_peer(&*routing, &peer2, true));

        // Reserve and activate peer2
        routing.try_reserve_dial(&peer2, true);
        routing.dial_connected(&peer2);
        routing.handshake_completed(&peer2);

        // At capacity
        assert!(!SwarmRouting::should_accept_peer(&*routing, &peer3, true));
    }

    #[test]
    fn test_disconnect_and_reconnect() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default();
        let routing = make_routing(base, config);

        let peer = SwarmAddress::with_first_byte(0x80);

        SwarmRouting::add_peers(&*routing, &[peer]);
        SwarmRouting::connected(&*routing, peer);
        assert!(routing.connected_peers.exists(&peer));
        assert!(!routing.known_peers.exists(&peer));

        SwarmRouting::on_peer_disconnected(&*routing, &peer);
        assert!(!routing.connected_peers.exists(&peer));
        assert!(routing.known_peers.exists(&peer));
    }

    #[test]
    fn test_depth_calculation() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default().with_low_watermark(2);
        let routing = make_routing(base, config);

        assert_eq!(routing.depth(), 0);

        let mut peer_bytes1 = [0x00u8; 32];
        peer_bytes1[0] = 0x04;
        let peer1 = OverlayAddress::from(peer_bytes1);

        let mut peer_bytes2 = [0x00u8; 32];
        peer_bytes2[0] = 0x05;
        let peer2 = OverlayAddress::from(peer_bytes2);

        SwarmRouting::connected(&*routing, peer1);
        SwarmRouting::connected(&*routing, peer2);

        assert_eq!(routing.depth(), 5);
    }

    #[test]
    fn test_closest_to() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default();
        let routing = make_routing(base, config);

        let peer_po0 = SwarmAddress::with_first_byte(0x80);
        let peer_po1 = SwarmAddress::with_first_byte(0x40);
        let peer_po2 = SwarmAddress::with_first_byte(0x20);

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
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default().with_low_watermark(1);
        let routing = make_routing(base, config);

        let peer_po0 = SwarmAddress::with_first_byte(0x80);
        let peer_po1 = SwarmAddress::with_first_byte(0x40);
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

    #[test]
    fn test_inbound_capacity() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default().with_high_watermark(2);
        let routing = make_routing(base, config);

        let peer1 = SwarmAddress::with_first_byte(0x80);
        let peer2 = SwarmAddress::with_first_byte(0xc0);
        let peer3 = SwarmAddress::with_first_byte(0xa0);

        // Can accept first inbound
        assert!(routing.should_accept_inbound(&peer1, true));
        routing.reserve_inbound(&peer1);

        // Can accept second inbound
        assert!(routing.should_accept_inbound(&peer2, true));
        routing.reserve_inbound(&peer2);

        // At capacity
        assert!(!routing.should_accept_inbound(&peer3, true));

        // Complete one handshake
        routing.handshake_completed(&peer1);

        // Still at capacity (peer1 now active)
        assert!(!routing.should_accept_inbound(&peer3, true));

        // Disconnect peer1
        RoutingCapacity::disconnected(&*routing, &peer1);

        // Now can accept
        assert!(routing.should_accept_inbound(&peer3, true));
    }
}
