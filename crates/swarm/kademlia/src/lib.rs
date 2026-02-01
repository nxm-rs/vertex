//! Kademlia-based peer topology management for Swarm clients.
//!
//! This crate implements a Kademlia-style distributed hash table topology
//! for managing peer connections in the Swarm network.
//!
//! # Architecture
//!
//! The topology maintains two sets of peers:
//! - `known_peers`: Discovered peers that we might want to connect to
//! - `connected_peers`: Currently connected peers
//!
//! A background manage loop periodically evaluates the topology and
//! selects peers to connect to based on:
//! 1. Neighbor connections (PO >= depth) - prioritized for forwarding
//! 2. Balanced connections (PO < depth) - for network connectivity
//!
//! # Usage
//!
//! ```ignore
//! use vertex_swarm_kademlia::{KademliaTopology, KademliaConfig};
//!
//! let config = KademliaConfig::default();
//! let topology = KademliaTopology::new(identity, config);
//!
//! // Spawn the manage loop
//! let handle = topology.clone().spawn_manage_loop();
//!
//! // Add discovered peers
//! topology.add_peers(&discovered_peers);
//!
//! // Get peers to connect to
//! let candidates = topology.peers_to_connect();
//! ```

mod config;
mod pslice;

pub use config::{
    DEFAULT_LOW_WATERMARK, DEFAULT_MANAGE_INTERVAL, DEFAULT_MAX_CONNECT_ATTEMPTS,
    DEFAULT_MAX_NEIGHBOR_ATTEMPTS, DEFAULT_MAX_PENDING_CONNECTIONS, DEFAULT_OVERSATURATION_PEERS,
    DEFAULT_SATURATION_PEERS, KademliaConfig,
};
pub use pslice::{MAX_PO, PSlice};

use nectar_primitives::ChunkAddress;
use parking_lot::Mutex;
use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};
use tokio::{sync::Notify, task::JoinHandle};
use tracing::{debug, info, trace};
use vertex_swarm_api::{SwarmIdentity, SwarmTopology};
use vertex_swarm_primitives::OverlayAddress;
use vertex_tasks::TaskExecutor;

/// Kademlia-based peer topology.
///
/// Implements the `Topology` trait with connection management capabilities.
/// Maintains known and connected peer sets, and provides connection decisions
/// based on Kademlia distance metrics.
///
/// Generic over `I: SwarmIdentity` to derive the self address from the node's identity.
pub struct KademliaTopology<I: SwarmIdentity> {
    /// Node identity (provides overlay address).
    identity: I,

    /// Discovered peers we might want to connect to.
    known_peers: PSlice,

    /// Currently connected peers.
    connected_peers: PSlice,

    /// Peers we're currently trying to connect to (dial in progress).
    pending_connections: Mutex<HashSet<OverlayAddress>>,

    /// Current neighborhood depth.
    depth: AtomicU8,

    /// Configuration.
    config: KademliaConfig,

    /// Notifier to wake the manage loop.
    manage_notify: Arc<Notify>,

    /// Peers we should try to connect to (updated by manage loop).
    connection_candidates: Mutex<Vec<OverlayAddress>>,
}

impl<I: SwarmIdentity> std::fmt::Debug for KademliaTopology<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KademliaTopology")
            .field(
                "depth",
                &self.depth.load(std::sync::atomic::Ordering::Relaxed),
            )
            .field("known_peers", &self.known_peers.len())
            .field("connected_peers", &self.connected_peers.len())
            .finish_non_exhaustive()
    }
}

impl<I: SwarmIdentity> KademliaTopology<I> {
    /// Create a new Kademlia topology with the given identity.
    pub fn new(identity: I, config: KademliaConfig) -> Arc<Self> {
        Arc::new(Self {
            identity,
            known_peers: PSlice::new(),
            connected_peers: PSlice::new(),
            pending_connections: Mutex::new(HashSet::new()),
            depth: AtomicU8::new(0),
            config,
            manage_notify: Arc::new(Notify::new()),
            connection_candidates: Mutex::new(Vec::new()),
        })
    }

    /// Get the base overlay address from the identity.
    fn base(&self) -> OverlayAddress {
        self.identity.overlay_address()
    }

    /// Calculate proximity order between base and a peer.
    fn proximity(&self, peer: &OverlayAddress) -> u8 {
        self.base().proximity(peer)
    }

    /// Spawn the manage loop as a background task using the provided task executor.
    ///
    /// The manage loop periodically evaluates the topology and updates
    /// the connection candidates. It also logs topology status periodically.
    ///
    /// Uses graceful shutdown to allow clean termination.
    pub fn spawn_manage_loop(self: Arc<Self>, executor: &TaskExecutor) -> JoinHandle<()> {
        let this = self.clone();
        executor.spawn_with_graceful_shutdown_signal("kademlia_manage", |shutdown| async move {
            let mut tick_count: u32 = 0;
            // Log status every N ticks (with default 15s interval, 4 ticks = 60s)
            const STATUS_LOG_INTERVAL: u32 = 4;

            // Await the shutdown signal in the background
            let mut shutdown = std::pin::pin!(shutdown);

            loop {
                tokio::select! {
                    guard = &mut shutdown => {
                        debug!("kademlia manage loop shutting down");
                        // Drop the guard to signal we're done with cleanup
                        drop(guard);
                        break;
                    }
                    _ = this.manage_notify.notified() => {
                        trace!("manage loop woken by notification");
                    }
                    _ = tokio::time::sleep(this.config.manage_interval) => {
                        trace!("manage loop woken by timer");
                    }
                }
                this.evaluate_connections();

                tick_count = tick_count.wrapping_add(1);
                if tick_count.is_multiple_of(STATUS_LOG_INTERVAL) {
                    this.log_status();
                }
            }
        })
    }

    /// Evaluate and update connection candidates.
    ///
    /// This is called by the manage loop periodically, but can also be called
    /// directly when immediate evaluation is needed (e.g., after discovering peers).
    pub fn evaluate_connections(&self) {
        let pending = self.pending_connections.lock().clone();
        let pending_count = pending.len();

        // Calculate how many new connections we can start
        let available_slots = self
            .config
            .max_pending_connections
            .saturating_sub(pending_count);
        if available_slots == 0 {
            trace!(pending = pending_count, "no available connection slots");
            return;
        }

        let mut candidates = Vec::new();

        // Strategy 1: Connect to neighbors (PO >= depth)
        self.connect_neighbours(&mut candidates, &pending, available_slots);

        // Strategy 2: Balance distant bins (PO < depth)
        self.connect_balanced(&mut candidates, &pending, available_slots);

        let num_candidates = candidates.len();
        *self.connection_candidates.lock() = candidates;

        if num_candidates > 0 {
            debug!(
                candidates = num_candidates,
                pending = pending_count,
                "evaluated connection candidates"
            );
        }
    }

    /// Add neighbor connection candidates.
    fn connect_neighbours(
        &self,
        candidates: &mut Vec<OverlayAddress>,
        pending: &HashSet<OverlayAddress>,
        max: usize,
    ) {
        let depth = self.depth();

        // Iterate known peers from deepest to shallowest
        for (po, peer) in self.known_peers.iter_by_proximity_desc() {
            if candidates.len() >= max {
                break; // Enough candidates
            }

            if po < depth {
                break; // Below depth, not a neighbor
            }

            if self.connected_peers.exists(&peer) {
                continue; // Already connected
            }

            if pending.contains(&peer) {
                continue; // Already dialing
            }

            if self.connected_peers.bin_size(po) >= self.config.saturation_peers {
                continue; // Bin saturated
            }

            candidates.push(peer);
        }
    }

    /// Add balanced connection candidates for distant bins.
    fn connect_balanced(
        &self,
        candidates: &mut Vec<OverlayAddress>,
        pending: &HashSet<OverlayAddress>,
        max: usize,
    ) {
        let depth = self.depth();

        // For bins below depth, try to maintain some connectivity
        for po in 0..depth {
            if candidates.len() >= max {
                break; // Enough candidates
            }

            let connected_count = self.connected_peers.bin_size(po);

            if connected_count >= self.config.saturation_peers {
                continue; // Bin already saturated
            }

            // Find known peers in this bin that aren't connected or pending
            for peer in self.known_peers.peers_in_bin(po) {
                if !self.connected_peers.exists(&peer) && !pending.contains(&peer) {
                    candidates.push(peer);
                    // Only add one candidate per unsaturated bin per cycle
                    break;
                }
            }
        }
    }

    /// Recalculate the neighborhood depth.
    fn recalc_depth(&self) -> u8 {
        // Find highest bin with at least low_watermark connected peers
        for po in (0..=MAX_PO).rev() {
            if self.connected_peers.bin_size(po) >= self.config.low_watermark {
                return po;
            }
        }
        0
    }

    /// Get statistics about the topology.
    pub fn stats(&self) -> TopologyStats {
        TopologyStats {
            known_peers: self.known_peers.len(),
            connected_peers: self.connected_peers.len(),
            depth: self.depth(),
            connection_candidates: self.connection_candidates.lock().len(),
            pending_connections: self.pending_connections.lock().len(),
        }
    }

    /// Mark a peer as being dialed (connection attempt started).
    ///
    /// Call this when initiating a dial to prevent duplicate connection attempts.
    pub fn start_connecting(&self, peer: OverlayAddress) {
        self.pending_connections.lock().insert(peer);
        trace!(%peer, "marked peer as pending connection");
    }

    /// Mark a connection attempt as failed.
    ///
    /// Call this when a dial fails to allow future connection attempts.
    pub fn connection_failed(&self, peer: &OverlayAddress) {
        self.pending_connections.lock().remove(peer);
        trace!(%peer, "removed peer from pending connections");
    }

    /// Get the number of pending connections.
    pub fn pending_count(&self) -> usize {
        self.pending_connections.lock().len()
    }

    /// Log the current topology status showing bin populations.
    ///
    /// Displays a visual representation of the Kademlia bins showing
    /// connected peers per bin, depth marker, and overall statistics.
    pub fn log_status(&self) {
        let connected_bins = self.connected_peers.bin_sizes();
        let known_bins = self.known_peers.bin_sizes();
        let depth = self.depth();
        let pending = self.pending_connections.lock().len();

        // Build a compact bin representation
        // Format: "bin:connected/known" for non-empty bins
        let mut bin_status = String::new();
        for po in 0..=MAX_PO {
            let c = connected_bins[po as usize];
            let k = known_bins[po as usize];
            if c > 0 || k > 0 {
                if !bin_status.is_empty() {
                    bin_status.push(' ');
                }
                // Mark depth boundary with |
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
            pending,
            bins = %bin_status,
            "kademlia topology"
        );
    }
}

impl<I: SwarmIdentity> SwarmTopology for KademliaTopology<I> {
    type Identity = I;

    fn identity(&self) -> &Self::Identity {
        &self.identity
    }

    fn neighbors(&self, depth: u8) -> Vec<OverlayAddress> {
        // Return connected peers at or above the given depth
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
        // ChunkAddress and OverlayAddress are both SwarmAddress
        // Collect all connected peers with their distance to the target
        let mut peers_with_distance: Vec<_> = self
            .connected_peers
            .all_peers()
            .into_iter()
            .map(|peer| {
                let po = address.proximity(&peer);
                (peer, po)
            })
            .collect();

        // Sort by proximity (descending - higher PO = closer)
        peers_with_distance.sort_by(|a, b| b.1.cmp(&a.1));

        // Return the closest `count` peers
        peers_with_distance
            .into_iter()
            .take(count)
            .map(|(peer, _)| peer)
            .collect()
    }

    fn add_peers(&self, peers: &[OverlayAddress]) {
        let mut added = 0;
        for peer in peers {
            let po = self.proximity(peer);
            if self.known_peers.add(*peer, po) {
                added += 1;
            }
        }

        if added > 0 {
            debug!(added, total = self.known_peers.len(), "added known peers");
            // Wake the manage loop to evaluate new peers
            self.manage_notify.notify_one();
        }
    }

    fn pick(&self, peer: &OverlayAddress, is_full_node: bool) -> bool {
        // Always accept light nodes
        if !is_full_node {
            return true;
        }

        let po = self.proximity(peer);
        let bin_size = self.connected_peers.bin_size(po);

        // Accept if the bin isn't oversaturated
        bin_size < self.config.oversaturation_peers
    }

    fn connected(&self, peer: OverlayAddress) {
        // Remove from pending (connection succeeded)
        self.pending_connections.lock().remove(&peer);

        let po = self.proximity(&peer);

        // Add to connected peers
        if self.connected_peers.add(peer, po) {
            // Remove from known peers (now connected)
            self.known_peers.remove(&peer);

            // Recalculate depth
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

            // Log status on depth change
            if new_depth != old_depth {
                info!(old_depth, new_depth, "kademlia depth changed");
                self.log_status();
            }
        }
    }

    fn disconnected(&self, peer: &OverlayAddress) {
        // Remove from connected peers
        if self.connected_peers.remove(peer) {
            let po = self.proximity(peer);

            // Optionally add back to known peers for reconnection
            self.known_peers.add(*peer, po);

            // Recalculate depth
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

            // Log status on depth change
            if new_depth != old_depth {
                info!(old_depth, new_depth, "kademlia depth changed");
                self.log_status();
            }

            // Wake manage loop to find replacement
            self.manage_notify.notify_one();
        }
    }

    fn peers_to_connect(&self) -> Vec<OverlayAddress> {
        self.connection_candidates.lock().clone()
    }
}

/// Statistics about the topology state.
#[derive(Debug, Clone)]
pub struct TopologyStats {
    /// Number of known (discovered but not connected) peers.
    pub known_peers: usize,
    /// Number of connected peers.
    pub connected_peers: usize,
    /// Current neighborhood depth.
    pub depth: u8,
    /// Number of connection candidates.
    pub connection_candidates: usize,
    /// Number of pending connection attempts.
    pub pending_connections: usize,
}

impl<I: SwarmIdentity> vertex_swarm_api::SwarmTopologyProvider for KademliaTopology<I> {
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
        self.pending_connections.lock().len()
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use alloy_signer_local::LocalSigner;
    use nectar_primitives::SwarmAddress;
    use vertex_swarmspec::Hive;

    /// Mock identity for testing that returns a fixed overlay address.
    #[derive(Clone, Debug)]
    struct MockIdentity {
        overlay: SwarmAddress,
        signer: Arc<LocalSigner<alloy_signer::k256::ecdsa::SigningKey>>,
        spec: Arc<Hive>,
    }

    impl MockIdentity {
        fn with_overlay(overlay: OverlayAddress) -> Self {
            let signer = LocalSigner::random();
            Self {
                overlay,
                signer: Arc::new(signer),
                spec: vertex_swarmspec::init_testnet(),
            }
        }
    }

    impl SwarmIdentity for MockIdentity {
        type Spec = Hive;
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

    fn make_topology(
        base: OverlayAddress,
        config: KademliaConfig,
    ) -> Arc<KademliaTopology<MockIdentity>> {
        let identity = MockIdentity::with_overlay(base);
        KademliaTopology::new(identity, config)
    }

    #[test]
    fn test_topology_creation() {
        let base = addr_from_byte(0x00);
        let config = KademliaConfig::default();
        let topology = make_topology(base, config);

        assert_eq!(topology.self_address(), base);
        assert_eq!(topology.depth(), 0);
    }

    #[test]
    fn test_add_and_connect_peers() {
        let base = addr_from_byte(0x00);
        let config = KademliaConfig::default();
        let topology = make_topology(base, config);

        let peer1 = addr_from_byte(0x80); // PO 0
        let peer2 = addr_from_byte(0x40); // PO 1

        // Add as known peers
        topology.add_peers(&[peer1, peer2]);
        assert_eq!(topology.known_peers.len(), 2);
        assert_eq!(topology.connected_peers.len(), 0);

        // Connect peer1
        topology.connected(peer1);
        assert_eq!(topology.known_peers.len(), 1);
        assert_eq!(topology.connected_peers.len(), 1);

        // Connect peer2
        topology.connected(peer2);
        assert_eq!(topology.known_peers.len(), 0);
        assert_eq!(topology.connected_peers.len(), 2);
    }

    #[test]
    fn test_pick_decision() {
        let base = addr_from_byte(0x00);
        let config = KademliaConfig::default().with_oversaturation_peers(2);
        let topology = make_topology(base, config);

        let peer1 = addr_from_byte(0x80); // PO 0
        let peer2 = addr_from_byte(0xc0); // PO 0
        let peer3 = addr_from_byte(0xa0); // PO 0

        // Client nodes always accepted
        assert!(topology.pick(&peer1, false));

        // First two Storer nodes in bin 0 should be accepted
        topology.connected(peer1);
        assert!(topology.pick(&peer2, true));

        topology.connected(peer2);
        // Third Storer node should be rejected (bin oversaturated)
        assert!(!topology.pick(&peer3, true));
    }

    #[test]
    fn test_disconnect_and_reconnect() {
        let base = addr_from_byte(0x00);
        let config = KademliaConfig::default();
        let topology = make_topology(base, config);

        let peer = addr_from_byte(0x80);

        topology.add_peers(&[peer]);
        topology.connected(peer);
        assert!(topology.connected_peers.exists(&peer));
        assert!(!topology.known_peers.exists(&peer));

        topology.disconnected(&peer);
        assert!(!topology.connected_peers.exists(&peer));
        assert!(topology.known_peers.exists(&peer)); // Added back for potential reconnect
    }

    #[test]
    fn test_depth_calculation() {
        let base = addr_from_byte(0x00);
        let config = KademliaConfig::default().with_low_watermark(2);
        let topology = make_topology(base, config);

        // Initially depth is 0
        assert_eq!(topology.depth(), 0);

        // Add two peers at PO 5 (need low_watermark peers for depth)
        let mut peer_bytes1 = [0x00u8; 32];
        peer_bytes1[0] = 0x04; // 00000100 -> PO 5
        let peer1 = OverlayAddress::from(peer_bytes1);

        let mut peer_bytes2 = [0x00u8; 32];
        peer_bytes2[0] = 0x05; // 00000101 -> still PO 5
        let peer2 = OverlayAddress::from(peer_bytes2);

        topology.connected(peer1);
        topology.connected(peer2);

        // Depth should now be 5 (highest bin with >= low_watermark peers)
        assert_eq!(topology.depth(), 5);
    }

    #[test]
    fn test_closest_to() {
        let base = addr_from_byte(0x00);
        let config = KademliaConfig::default();
        let topology = make_topology(base, config);

        // Add some connected peers at different distances
        let peer_po0 = addr_from_byte(0x80); // PO 0 from base
        let peer_po1 = addr_from_byte(0x40); // PO 1 from base
        let peer_po2 = addr_from_byte(0x20); // PO 2 from base

        topology.connected(peer_po0);
        topology.connected(peer_po1);
        topology.connected(peer_po2);

        // Find closest to a target address
        let mut target_bytes = [0x00u8; 32];
        target_bytes[0] = 0x21; // Close to peer_po2
        let target = ChunkAddress::from(target_bytes);

        let closest = topology.closest_to(&target, 2);
        assert_eq!(closest.len(), 2);
        // peer_po2 should be closest to target
        assert_eq!(closest[0], peer_po2);
    }

    #[test]
    fn test_neighbors() {
        let base = addr_from_byte(0x00);
        let config = KademliaConfig::default().with_low_watermark(1);
        let topology = make_topology(base, config);

        let peer_po0 = addr_from_byte(0x80); // PO 0
        let peer_po1 = addr_from_byte(0x40); // PO 1
        let peer_po5 = {
            let mut bytes = [0x00u8; 32];
            bytes[0] = 0x04;
            OverlayAddress::from(bytes)
        }; // PO 5

        topology.connected(peer_po0);
        topology.connected(peer_po1);
        topology.connected(peer_po5);

        // Neighbors at depth 0 should include all
        let neighbors_d0 = topology.neighbors(0);
        assert_eq!(neighbors_d0.len(), 3);

        // Neighbors at depth 2 should exclude PO 0 and PO 1
        let neighbors_d2 = topology.neighbors(2);
        assert_eq!(neighbors_d2.len(), 1);
        assert_eq!(neighbors_d2[0], peer_po5);
    }
}
