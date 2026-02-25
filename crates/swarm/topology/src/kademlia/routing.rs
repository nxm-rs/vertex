//! Kademlia-based peer routing for Swarm.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicUsize, AtomicU8, Ordering},
    },
};

use nectar_primitives::ChunkAddress;
use parking_lot::RwLock;
use tracing::{debug, info, trace};
use vertex_swarm_api::{SwarmIdentity, SwarmSpec};
use vertex_swarm_peer_manager::PeerManager;
use vertex_swarm_peer_manager::ProximityIndex;
use vertex_swarm_primitives::OverlayAddress;

use super::{
    CandidateSnapshot, CandidateSelector, DepthAwareLimits, KademliaConfig, LimitsSnapshot,
    RoutingCapacity, SwarmRouting, candidate_queues::CandidateQueues,
    evaluator_task::{RoutingEvaluatorHandle, spawn_evaluator},
    select_balanced_candidates, select_neighborhood_candidates,
};
use crate::metrics::{phase, record_phase_transition};

/// Connection phase for capacity tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionPhase {
    Dialing,
    Handshaking,
    Active,
}

/// Phase of a connection being considered for eviction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvictionPhase {
    Handshaking,
    Active,
}

/// A peer identified for eviction due to bin overpopulation.
#[derive(Debug, Clone)]
pub struct EvictionCandidate {
    pub overlay: OverlayAddress,
    pub bin: u8,
    pub phase: EvictionPhase,
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

    fn effective_count(&self, po: u8) -> usize {
        let idx = po as usize;
        self.dialing.get(idx).map_or(0, |c| c.load(Ordering::Relaxed))
            + self.handshaking.get(idx).map_or(0, |c| c.load(Ordering::Relaxed))
            + self.active.get(idx).map_or(0, |c| c.load(Ordering::Relaxed))
    }

    fn inc_dialing(&self, po: u8) {
        if let Some(c) = self.dialing.get(po as usize) {
            c.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn dec_dialing(&self, po: u8) {
        if let Some(c) = self.dialing.get(po as usize) {
            c.fetch_sub(1, Ordering::Relaxed);
        }
    }

    fn inc_handshaking(&self, po: u8) {
        if let Some(c) = self.handshaking.get(po as usize) {
            c.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn dec_handshaking(&self, po: u8) {
        if let Some(c) = self.handshaking.get(po as usize) {
            c.fetch_sub(1, Ordering::Relaxed);
        }
    }

    fn inc_active(&self, po: u8) {
        if let Some(c) = self.active.get(po as usize) {
            c.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn dec_active(&self, po: u8) {
        if let Some(c) = self.active.get(po as usize) {
            c.fetch_sub(1, Ordering::Relaxed);
        }
    }

    #[cfg(test)]
    fn active_count(&self, po: u8) -> usize {
        self.active.get(po as usize).map_or(0, |c| c.load(Ordering::Relaxed))
    }
}

/// Kademlia-based peer routing table.
///
/// Maintains connected peer set and provides connection decisions
/// based on Kademlia distance metrics. Known peers are tracked by PeerManager.
pub struct KademliaRouting<I: SwarmIdentity> {
    identity: I,
    /// Maximum proximity order from the network spec.
    max_po: u8,
    /// Connected peers tracked by proximity bin.
    pub(crate) connected_peers: ProximityIndex,
    /// All known peers - source of truth for dial candidates.
    peer_manager: Arc<PeerManager>,
    depth: AtomicU8,
    config: KademliaConfig,
    /// Per-bin FIFO candidate queues with dedup.
    candidate_queues: CandidateQueues,
    bin_counts: BinCounts,
    /// Tracks overlay → connection phase for state transitions.
    connection_phases: RwLock<HashMap<OverlayAddress, ConnectionPhase>>,
}

impl<I: SwarmIdentity> KademliaRouting<I> {
    pub fn new(identity: I, config: KademliaConfig, peer_manager: Arc<PeerManager>) -> Arc<Self> {
        let max_po = identity.spec().max_po();
        let local_overlay = identity.overlay_address();
        let num_bins = (max_po as usize) + 1;

        Arc::new(Self {
            identity,
            max_po,
            // connected_peers is unbounded (controlled by routing capacity)
            connected_peers: ProximityIndex::new(local_overlay, max_po, 0),
            peer_manager,
            depth: AtomicU8::new(0),
            config,
            candidate_queues: CandidateQueues::new(num_bins, 16),
            bin_counts: BinCounts::new(num_bins),
            connection_phases: RwLock::new(HashMap::new()),
        })
    }

    /// Returns the maximum proximity order for this routing table.
    pub fn max_po(&self) -> u8 {
        self.max_po
    }

    /// Depth-aware per-bin capacity limits.
    pub fn limits(&self) -> &DepthAwareLimits {
        &self.config.limits
    }

    fn base(&self) -> OverlayAddress {
        self.identity.overlay_address()
    }

    fn proximity(&self, peer: &OverlayAddress) -> u8 {
        self.base().proximity(peer).min(self.max_po)
    }

    /// Capture state once for both neighbor and balanced candidate selection.
    ///
    /// Uses lightweight capture: banned/in_backoff sets are left empty because
    /// candidate selection uses `is_eligible_live()` with O(1) DashMap lookups
    /// instead of consulting snapshot sets. This avoids two full DashMap scans
    /// per tick.
    #[tracing::instrument(skip(self), level = "trace")]
    fn capture_candidate_state(&self, effective_depth: u8) -> CandidateSnapshot {
        let queued_set = self.candidate_queues.snapshot_queued();
        let in_progress: HashSet<OverlayAddress> = vertex_observability::timed_read(
            &self.connection_phases,
            metrics::histogram!("topology_routing_phases_lock_seconds"),
        )
        .keys()
        .copied()
        .collect();

        CandidateSnapshot {
            limits: LimitsSnapshot::capture(&self.config.limits, effective_depth),
            in_progress,
            queued: queued_set,
            banned: HashSet::new(),
            in_backoff: HashSet::new(),
        }
    }

    fn recalc_depth(&self) -> u8 {
        for po in (0..=self.max_po).rev() {
            if self.connected_peers.bin_size(po) >= self.config.limits.nominal() {
                return po;
            }
        }
        0
    }

    /// Log the current routing status showing bin populations.
    pub fn log_status(&self) {
        use std::fmt::Write;

        let connected_bins = self.connected_peers.bin_sizes();
        let known_bins = self.peer_manager.bin_sizes();
        let depth = self.depth();

        let mut bin_status = String::with_capacity(128);
        for po in 0..=self.max_po {
            let idx = po as usize;
            let c = connected_bins.get(idx).copied().unwrap_or(0);
            let k = known_bins.get(idx).copied().unwrap_or(0);
            if c > 0 || k > 0 {
                if !bin_status.is_empty() {
                    bin_status.push(' ');
                }
                if po == depth {
                    let _ = write!(bin_status, "[{po}:{c}/{k}]");
                } else {
                    let _ = write!(bin_status, "{po}:{c}/{k}");
                }
            }
        }

        let total_connected: usize = connected_bins.iter().sum();
        let total_known: usize = known_bins.iter().sum();

        if bin_status.is_empty() {
            bin_status.push_str("(empty)");
        }

        debug!(
            depth,
            connected = total_connected,
            known = total_known,
            bins = %bin_status,
            "kademlia routing"
        );
    }

    /// Identify peers to evict from overpopulated bins.
    ///
    /// For each bin below depth where effective_count > target:
    /// 1. Select Handshaking peers first (cheaper to close)
    /// 2. Then Active peers sorted by score ascending (lowest score first)
    ///
    /// Caller filters protected peers (Bootnode/Trusted) and issues disconnects.
    pub fn eviction_candidates(&self) -> Vec<EvictionCandidate> {
        let depth = self.depth.load(Ordering::Relaxed);
        let phases = self.connection_phases.read();
        let mut candidates = Vec::new();

        for bin in 0..depth {
            let effective = self.bin_counts.effective_count(bin);
            let surplus = self.config.limits.surplus(bin, depth, effective);
            if surplus == 0 {
                continue;
            }

            let mut remaining = surplus;

            // Phase 1: Handshaking peers in this bin
            for (overlay, phase) in phases.iter() {
                if remaining == 0 {
                    break;
                }
                if *phase == ConnectionPhase::Handshaking && self.proximity(overlay) == bin {
                    candidates.push(EvictionCandidate {
                        overlay: *overlay,
                        bin,
                        phase: EvictionPhase::Handshaking,
                    });
                    remaining -= 1;
                }
            }

            // Phase 2: Active peers sorted by score ascending
            if remaining > 0 {
                let mut active_in_bin: Vec<_> = self
                    .connected_peers
                    .peers_in_bin(bin)
                    .into_iter()
                    .map(|overlay| {
                        let score = self.peer_manager.get_peer_score(&overlay).unwrap_or(0.0);
                        (overlay, score)
                    })
                    .collect();

                active_in_bin
                    .sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

                for (overlay, _) in active_in_bin.into_iter().take(remaining) {
                    candidates.push(EvictionCandidate {
                        overlay,
                        bin,
                        phase: EvictionPhase::Active,
                    });
                }
            }
        }

        candidates
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
        let known = self.peer_manager.bin_sizes();
        connected
            .into_iter()
            .zip(known.into_iter())
            .map(|(c, k)| (c, k))
            .collect()
    }

    /// Get connected and known peer counts for a single bin.
    pub fn bin_peer_counts(&self, po: u8) -> (usize, usize) {
        (
            self.connected_peers.bin_size(po),
            self.peer_manager.bin_size(po),
        )
    }

    /// Get connection phase counts for a specific bin.
    ///
    /// Returns (dialing, handshaking, active) counts for the given proximity order.
    pub fn bin_phase_counts(&self, po: u8) -> (usize, usize, usize) {
        let idx = po as usize;
        (
            self.bin_counts.dialing.get(idx).map_or(0, |c| c.load(Ordering::Relaxed)),
            self.bin_counts.handshaking.get(idx).map_or(0, |c| c.load(Ordering::Relaxed)),
            self.bin_counts.active.get(idx).map_or(0, |c| c.load(Ordering::Relaxed)),
        )
    }

    /// Get connection phase counts for all bins.
    ///
    /// Returns a vector of (po, dialing, handshaking, active) for each bin.
    pub fn all_bin_phases(&self) -> Vec<(u8, usize, usize, usize)> {
        (0..=self.max_po)
            .map(|po| {
                let (d, h, a) = self.bin_phase_counts(po);
                (po, d, h, a)
            })
            .collect()
    }

    /// Get total known peers count (from PeerManager).
    pub fn known_peers_total(&self) -> usize {
        self.peer_manager.len()
    }

    /// Get total connected peers count.
    pub fn connected_peers_total(&self) -> usize {
        self.connected_peers.len()
    }

    /// Get proximity index cache statistics.
    pub fn proximity_cache_stats(&self) -> vertex_swarm_peer_manager::ProximityIndexCacheStats {
        self.connected_peers.cache_stats()
    }

    pub fn connected_peers_in_bin(&self, po: u8) -> Vec<String> {
        self.connected_peers
            .peers_in_bin(po)
            .iter()
            .map(|addr| hex::encode(addr.as_slice()))
            .collect()
    }

    pub fn connected_overlays_in_bin(&self, po: u8) -> Vec<OverlayAddress> {
        self.connected_peers.peers_in_bin(po)
    }

    fn peer_connected(&self, peer: OverlayAddress) {
        let po = self.proximity(&peer);

        if self.connected_peers.add(peer) {
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
        // Only remove from connected_peers; PeerManager handles banning
        self.connected_peers.remove(peer);
        debug!(%peer, "removed peer from routing");
    }
}

impl<I: SwarmIdentity> RoutingCapacity for KademliaRouting<I> {
    fn try_reserve_dial(&self, overlay: &OverlayAddress, _storer: bool) -> bool {
        let po = self.proximity(overlay);
        let effective = self.bin_counts.effective_count(po);

        let mut phases = self.connection_phases.write();

        if phases.contains_key(overlay) {
            return false;
        }

        // Use depth-aware limits for capacity decision
        if !self.config.limits.needs_more(po, self.depth(), effective) {
            return false;
        }

        self.bin_counts.inc_dialing(po);
        phases.insert(*overlay, ConnectionPhase::Dialing);
        record_phase_transition(phase::NONE, phase::DIALING);
        true
    }

    fn release_dial(&self, overlay: &OverlayAddress) {
        let mut phases = self.connection_phases.write();
        if let Some(ConnectionPhase::Dialing) = phases.remove(overlay) {
            let po = self.proximity(overlay);
            self.bin_counts.dec_dialing(po);
            record_phase_transition(phase::DIALING, phase::NONE);
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
                record_phase_transition(phase::DIALING, phase::HANDSHAKING);
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
                record_phase_transition(phase::HANDSHAKING, phase::ACTIVE);
            }
        }
    }

    fn release_handshake(&self, overlay: &OverlayAddress) {
        let mut phases = self.connection_phases.write();
        if let Some(ConnectionPhase::Handshaking) = phases.remove(overlay) {
            let po = self.proximity(overlay);
            self.bin_counts.dec_handshaking(po);
            record_phase_transition(phase::HANDSHAKING, phase::NONE);
        }
    }

    fn disconnected(&self, overlay: &OverlayAddress) {
        let mut phases = self.connection_phases.write();
        if let Some(phase) = phases.remove(overlay) {
            let po = self.proximity(overlay);
            match phase {
                ConnectionPhase::Dialing => {
                    self.bin_counts.dec_dialing(po);
                    record_phase_transition(phase::DIALING, phase::NONE);
                }
                ConnectionPhase::Handshaking => {
                    self.bin_counts.dec_handshaking(po);
                    record_phase_transition(phase::HANDSHAKING, phase::NONE);
                }
                ConnectionPhase::Active => {
                    self.bin_counts.dec_active(po);
                    record_phase_transition(phase::ACTIVE, phase::NONE);
                }
            }
        }
    }

    fn should_accept_inbound(&self, overlay: &OverlayAddress, _storer: bool) -> bool {
        let po = self.proximity(overlay);
        let effective = self.bin_counts.effective_count(po);

        let phases = vertex_observability::timed_read(
            &self.connection_phases,
            metrics::histogram!("topology_routing_phases_lock_seconds"),
        );
        !phases.contains_key(overlay)
            && self.config.limits.should_accept_inbound(po, self.depth(), effective)
    }

    fn reserve_inbound(&self, overlay: &OverlayAddress) {
        let po = self.proximity(overlay);
        let mut phases = self.connection_phases.write();

        if !phases.contains_key(overlay) {
            self.bin_counts.inc_handshaking(po);
            phases.insert(*overlay, ConnectionPhase::Handshaking);
            record_phase_transition(phase::NONE, phase::HANDSHAKING);
        }
    }
}

impl<I: SwarmIdentity> SwarmRouting<I> for KademliaRouting<I> {
    fn should_accept_peer(&self, peer: &OverlayAddress, _storer: bool) -> bool {
        let po = self.proximity(peer);
        let effective_count = self.bin_counts.effective_count(po);
        // Use depth-aware limits for peer acceptance
        self.config.limits.needs_more(po, self.depth(), effective_count)
    }

    fn connected(&self, peer: OverlayAddress) {
        self.peer_connected(peer);
    }

    fn on_peer_disconnected(&self, peer: &OverlayAddress) {
        self.peer_disconnected(peer);
    }

    fn remove_peer(&self, peer: &OverlayAddress) {
        self.do_remove_peer(peer);
    }
}

// Methods internalized from the poll loop — called only by the background evaluator task
// and topology behaviour within the routing module.
impl<I: SwarmIdentity + 'static> KademliaRouting<I> {
    /// Spawn background evaluator. Returns handle for triggering evaluation.
    pub(crate) fn spawn_evaluator(self: &Arc<Self>) -> Result<RoutingEvaluatorHandle, String> {
        spawn_evaluator(self.clone())
    }

    /// Drain all pending candidates (called from poll loop). O(bins).
    pub(crate) fn drain_candidates(&self) -> Vec<OverlayAddress> {
        self.candidate_queues.drain_all()
    }

    /// Evaluate connections and enqueue candidates into per-bin queues.
    ///
    /// Uses effective depth (without mutating shared limits) for allocation.
    /// Called by the background evaluator task.
    #[tracing::instrument(skip(self), level = "debug")]
    pub(crate) fn evaluate_connections(&self) {
        // Use effective depth (max of connected and estimated) for allocation
        let connected_depth = self.depth.load(Ordering::Relaxed);
        let known_bin_sizes = self.peer_manager.bin_sizes();
        let effective_depth = self.config.limits.effective_depth(connected_depth, &known_bin_sizes);

        if effective_depth != connected_depth {
            trace!(
                connected_depth,
                effective_depth,
                "using estimated depth for allocation"
            );
        }

        // Capture state using effective depth — no mutation of shared limits
        let snapshot = self.capture_candidate_state(effective_depth);
        let mut selector = CandidateSelector::new(
            &snapshot,
            &self.connected_peers,
            self.config.max_neighbor_candidates + self.config.max_balanced_candidates,
        );

        select_neighborhood_candidates(
            &mut selector,
            &self.peer_manager,
            |po| self.bin_counts.effective_count(po),
            self.max_po,
        );
        let neighbor_candidates = selector.len();

        select_balanced_candidates(
            &mut selector,
            &self.peer_manager,
            |po| self.bin_counts.effective_count(po),
        );
        let balanced_candidates = selector.len() - neighbor_candidates;

        let new_candidates = selector.finish();

        let mut added = 0usize;
        for c in new_candidates {
            let po = self.proximity(&c);
            if self.candidate_queues.push(po, c) {
                added += 1;
            }
        }

        if added > 0 {
            debug!(
                added,
                neighbors = neighbor_candidates,
                balanced = balanced_candidates,
                "evaluated connection candidates"
            );
        } else {
            trace!("no new connection candidates");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nectar_primitives::SwarmAddress;
    use vertex_swarm_test_utils::{MockIdentity, make_swarm_peer_minimal};

    fn make_routing(
        base: OverlayAddress,
        config: KademliaConfig,
    ) -> (Arc<KademliaRouting<MockIdentity>>, Arc<PeerManager>) {
        let identity = MockIdentity::with_overlay(base);
        let peer_manager = PeerManager::new(base, 31);
        let routing = KademliaRouting::new(identity, config, peer_manager.clone());
        (routing, peer_manager)
    }

    #[test]
    fn test_routing_creation() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default();
        let (routing, _pm) = make_routing(base, config);

        assert_eq!(routing.depth(), 0);
        assert_eq!(routing.connected_peers.len(), 0);
    }

    #[test]
    fn test_add_and_connect_peers() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default();
        let (routing, pm) = make_routing(base, config);

        let peer1 = SwarmAddress::with_first_byte(0x80);
        let peer2 = SwarmAddress::with_first_byte(0x40);

        // Add peers via PeerManager (not routing.add_peers which is now no-op)
        pm.store_discovered_peer(make_swarm_peer_minimal(0x80));
        pm.store_discovered_peer(make_swarm_peer_minimal(0x40));
        assert_eq!(pm.stats().total_peers, 2);
        assert_eq!(routing.connected_peers.len(), 0);

        SwarmRouting::connected(&*routing, peer1);
        assert_eq!(routing.connected_peers.len(), 1);

        SwarmRouting::connected(&*routing, peer2);
        assert_eq!(routing.connected_peers.len(), 2);
    }

    #[test]
    fn test_capacity_reserve_and_release() {
        let base = SwarmAddress::with_first_byte(0x00);
        // With depth 0, all bins use nominal (3) as target
        let config = KademliaConfig::default().with_nominal(2);
        let (routing, _pm) = make_routing(base, config);

        let peer1 = SwarmAddress::with_first_byte(0x80); // po=0
        let peer2 = SwarmAddress::with_first_byte(0xc0); // po=0
        let peer3 = SwarmAddress::with_first_byte(0xa0); // po=0

        // First reserve succeeds (effective=0 < nominal=2)
        assert!(routing.try_reserve_dial(&peer1, true));

        // Second reserve succeeds (effective=1 < nominal=2)
        assert!(routing.try_reserve_dial(&peer2, true));

        // Third fails (effective=2 >= nominal=2)
        assert!(!routing.try_reserve_dial(&peer3, true));

        // Release one
        routing.release_dial(&peer1);

        // Now third succeeds (effective=1 < nominal=2)
        assert!(routing.try_reserve_dial(&peer3, true));
    }

    #[test]
    fn test_capacity_state_transitions() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default().with_nominal(2);
        let (routing, _pm) = make_routing(base, config);

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
        let config = KademliaConfig::default().with_nominal(2);
        let (routing, _pm) = make_routing(base, config);

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

        // At capacity (effective=2 >= nominal=2)
        assert!(!SwarmRouting::should_accept_peer(&*routing, &peer3, true));
    }

    #[test]
    fn test_disconnect_and_reconnect() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default();
        let (routing, pm) = make_routing(base, config);

        let peer = SwarmAddress::with_first_byte(0x80);

        // Add peer to PeerManager
        pm.store_discovered_peer(make_swarm_peer_minimal(0x80));

        SwarmRouting::connected(&*routing, peer);
        assert!(routing.connected_peers.exists(&peer));
        // Peer still in PeerManager
        assert!(pm.contains(&peer));

        SwarmRouting::on_peer_disconnected(&*routing, &peer);
        assert!(!routing.connected_peers.exists(&peer));
        // Peer still in PeerManager after disconnect
        assert!(pm.contains(&peer));
    }

    #[test]
    fn test_depth_calculation() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default().with_nominal(2);
        let (routing, _pm) = make_routing(base, config);

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
        let (routing, _pm) = make_routing(base, config);

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
        let config = KademliaConfig::default().with_nominal(1);
        let (routing, _pm) = make_routing(base, config);

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
        // With nominal=2 and headroom=0, inbound ceiling = 2
        let config = KademliaConfig::default()
            .with_nominal(2)
            .with_inbound_headroom(0);
        let (routing, _pm) = make_routing(base, config);

        let peer1 = SwarmAddress::with_first_byte(0x80);
        let peer2 = SwarmAddress::with_first_byte(0xc0);
        let peer3 = SwarmAddress::with_first_byte(0xa0);

        // Can accept first inbound
        assert!(routing.should_accept_inbound(&peer1, true));
        routing.reserve_inbound(&peer1);

        // Can accept second inbound
        assert!(routing.should_accept_inbound(&peer2, true));
        routing.reserve_inbound(&peer2);

        // At capacity (effective=2 >= target+headroom=2)
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

    #[test]
    fn test_depth_aware_targets() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default();
        let (routing, _pm) = make_routing(base, config);

        // Initially depth=0, all bins should use nominal
        assert_eq!(routing.config.limits.target(0, 0), 3);
        assert_eq!(routing.config.limits.target(7, 0), 3);

        // At depth 8, targets should vary by bin
        // Bin 7: 160 × 8 / 36 = 35
        assert_eq!(routing.config.limits.target(7, 8), 35);
        // Bin 0: 160 × 1 / 36 = 4
        assert_eq!(routing.config.limits.target(0, 8), 4);
        // Neighborhood (bin >= depth) returns MAX
        assert_eq!(routing.config.limits.target(8, 8), usize::MAX);
    }

    #[test]
    fn test_eviction_candidates_no_surplus() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default().with_nominal(3);
        let (routing, _pm) = make_routing(base, config);

        // No peers, no surplus
        let candidates = routing.eviction_candidates();
        assert!(candidates.is_empty());

        // Add peers below nominal - still no surplus
        let peer1 = SwarmAddress::with_first_byte(0x80); // po=0
        SwarmRouting::connected(&*routing, peer1);
        let candidates = routing.eviction_candidates();
        assert!(candidates.is_empty());
    }

    /// Helper to directly place a peer as Active in routing state (bypasses capacity checks).
    fn force_active(routing: &KademliaRouting<MockIdentity>, peer: OverlayAddress) {
        let po = routing.proximity(&peer);
        routing.bin_counts.inc_active(po);
        routing.connection_phases.write().insert(peer, ConnectionPhase::Active);
        routing.connected_peers.add(peer);
    }

    /// Helper to directly place a peer as Handshaking in routing state.
    fn force_handshaking(routing: &KademliaRouting<MockIdentity>, peer: OverlayAddress) {
        let po = routing.proximity(&peer);
        routing.bin_counts.inc_handshaking(po);
        routing.connection_phases.write().insert(peer, ConnectionPhase::Handshaking);
    }

    #[test]
    fn test_eviction_candidates_handshaking_first() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default(); // nominal=3, total_target=160
        let (routing, _pm) = make_routing(base, config);

        // Place 5 active peers in bin 0 (po=0)
        let active_peers: Vec<_> = (0..5).map(|i| {
            let mut bytes = [0x00u8; 32];
            bytes[0] = 0x80 + i;
            OverlayAddress::from(bytes)
        }).collect();
        for &peer in &active_peers {
            force_active(&routing, peer);
        }

        // Place 1 handshaking peer in bin 0
        let handshaking = {
            let mut bytes = [0x00u8; 32];
            bytes[0] = 0x90;
            OverlayAddress::from(bytes)
        };
        force_handshaking(&routing, handshaking);

        // Simulate depth increase to 8 (bin 0 target = max(160*1/36, 3) = 4)
        // effective = 6 (5 active + 1 handshaking), surplus = 2
        routing.depth.store(8, Ordering::Relaxed);

        let candidates = routing.eviction_candidates();
        assert_eq!(candidates.len(), 2);
        // Handshaking peer should be selected
        assert!(candidates.iter().any(|c| c.overlay == handshaking && c.phase == EvictionPhase::Handshaking));
        // One active peer should also be selected
        assert!(candidates.iter().any(|c| c.phase == EvictionPhase::Active));
    }

    #[test]
    fn test_eviction_candidates_active_lowest_score() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default(); // nominal=3, total_target=160
        let (routing, _pm) = make_routing(base, config);

        // Place 6 active peers in bin 0
        let peers: Vec<_> = (0..6).map(|i| {
            let mut bytes = [0x00u8; 32];
            bytes[0] = 0x80 + i;
            OverlayAddress::from(bytes)
        }).collect();
        for &peer in &peers {
            force_active(&routing, peer);
        }

        // Simulate depth increase to 8 (bin 0 target = 4, surplus = 2)
        routing.depth.store(8, Ordering::Relaxed);

        let candidates = routing.eviction_candidates();
        assert_eq!(candidates.len(), 2);
        for c in &candidates {
            assert_eq!(c.phase, EvictionPhase::Active);
            assert_eq!(c.bin, 0);
        }
    }

    #[test]
    fn test_eviction_candidates_neighborhood_never_evicted() {
        let base = SwarmAddress::with_first_byte(0x00);
        let config = KademliaConfig::default().with_nominal(1);
        let (routing, _pm) = make_routing(base, config);

        // Create peers in bin 5 (which will become neighborhood bin)
        let peer1 = {
            let mut bytes = [0x00u8; 32];
            bytes[0] = 0x04;
            OverlayAddress::from(bytes)
        };
        let peer2 = {
            let mut bytes = [0x00u8; 32];
            bytes[0] = 0x05;
            OverlayAddress::from(bytes)
        };
        let peer3 = {
            let mut bytes = [0x00u8; 32];
            bytes[0] = 0x06;
            OverlayAddress::from(bytes)
        };

        for peer in [peer1, peer2, peer3] {
            routing.try_reserve_dial(&peer, true);
            routing.dial_connected(&peer);
            routing.handshake_completed(&peer);
            SwarmRouting::connected(&*routing, peer);
        }

        // With nominal=1 and 3 peers in bin 5, depth should be 5
        // Bin 5 is neighborhood (>= depth), so no eviction
        assert!(routing.depth() >= 5);
        let candidates = routing.eviction_candidates();
        // Only bins < depth produce candidates; bins >= depth are neighborhood
        for c in &candidates {
            assert!(c.bin < routing.depth(), "neighborhood bin {} should not produce candidates", c.bin);
        }
    }
}
