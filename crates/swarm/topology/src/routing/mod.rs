//! Kademlia-based peer routing for Swarm overlay network.

mod config;
mod kademlia;
mod pslice;

pub use config::{
    KademliaConfig, DEFAULT_CLIENT_RESERVED_SLOTS, DEFAULT_HIGH_WATERMARK, DEFAULT_LOW_WATERMARK,
    DEFAULT_MANAGE_INTERVAL, DEFAULT_MAX_BALANCED_CANDIDATES, DEFAULT_MAX_CONNECT_ATTEMPTS,
    DEFAULT_MAX_NEIGHBOR_ATTEMPTS, DEFAULT_MAX_NEIGHBOR_CANDIDATES, DEFAULT_SATURATION_PEERS,
};
pub use kademlia::{KademliaRouting, PeerFailureProvider, RoutingStats};
pub use pslice::{PSlice, MAX_PO};

use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_primitives::OverlayAddress;

/// Internal routing operations for topology behaviour.
///
/// Extends SwarmTopology with mutation and connection management.
/// Implemented by routing algorithms (e.g., Kademlia).
pub trait SwarmRouting<I: SwarmIdentity> {
    /// Add discovered peers (from Hive). May trigger connection evaluation.
    fn add_peers(&self, peers: &[OverlayAddress]);

    /// Should we accept an inbound connection from this peer?
    fn should_accept_peer(&self, peer: &OverlayAddress, is_full_node: bool) -> bool;

    /// Notify that a peer has connected.
    fn connected(&self, peer: OverlayAddress);

    /// Notify that a peer has disconnected.
    fn disconnected(&self, peer: &OverlayAddress);

    /// Get peers we should try to connect to.
    fn peers_to_connect(&self) -> Vec<OverlayAddress>;

    /// Record a connection failure for a peer.
    fn record_connection_failure(&self, peer: &OverlayAddress);

    /// Check if a peer is temporarily unavailable due to recent failures.
    fn is_temporarily_unavailable(&self, peer: &OverlayAddress) -> bool;

    /// Get the current failure count for a peer.
    fn failure_count(&self, peer: &OverlayAddress) -> u32;

    /// Remove a peer from all routing state (for banning).
    fn remove_peer(&self, peer: &OverlayAddress);

    /// Evaluate and update connection candidates based on routing needs.
    fn evaluate_connections(&self);

    /// Mark a peer as having a dial in progress.
    fn mark_pending_dial(&self, peer: OverlayAddress);

    /// Clear the pending dial status for a peer.
    fn clear_pending_dial(&self, peer: &OverlayAddress);
}
