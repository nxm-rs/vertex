//! Kademlia-based peer routing for Swarm overlay network.

pub mod args;
mod config;
mod kademlia;
mod pslice;

pub use args::RoutingArgs;
pub use config::KademliaConfig;
pub use kademlia::KademliaRouting;
pub use pslice::PSlice;

use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_primitives::OverlayAddress;

/// Connection capacity management for routing algorithms.
///
/// Provides atomic capacity reservation to prevent TOCTOU races between
/// checking bin availability and starting a connection.
pub trait RoutingCapacity: Send + Sync {
    /// Atomically check capacity and reserve a dial slot.
    /// Returns true if reserved, false if at capacity or already tracking.
    fn try_reserve_dial(&self, overlay: &OverlayAddress, is_full_node: bool) -> bool;

    /// Release a dial reservation (dial failed before connection established).
    fn release_dial(&self, overlay: &OverlayAddress);

    /// Transition from dialing to handshaking phase.
    fn dial_connected(&self, overlay: &OverlayAddress);

    /// Transition from handshaking to active.
    fn handshake_completed(&self, overlay: &OverlayAddress);

    /// Release a handshaking reservation (handshake failed).
    fn release_handshake(&self, overlay: &OverlayAddress);

    /// Connection fully disconnected - release active slot.
    fn disconnected(&self, overlay: &OverlayAddress);

    /// Check if we can accept an inbound connection (before overlay is known).
    fn should_accept_inbound(&self, overlay: &OverlayAddress, is_full_node: bool) -> bool;

    /// Reserve capacity for an accepted inbound connection.
    fn reserve_inbound(&self, overlay: &OverlayAddress);
}

/// Internal routing operations for topology behaviour.
///
/// Extends SwarmTopology with mutation and connection management.
/// Implemented by routing algorithms (e.g., Kademlia).
pub trait SwarmRouting<I: SwarmIdentity>: RoutingCapacity {
    /// Add discovered peers (from Hive). May trigger connection evaluation.
    fn add_peers(&self, peers: &[OverlayAddress]);

    /// Should we accept an inbound connection from this peer?
    fn should_accept_peer(&self, peer: &OverlayAddress, is_full_node: bool) -> bool;

    /// Notify that a peer has connected.
    fn connected(&self, peer: OverlayAddress);

    /// Update routing tables for a disconnected peer.
    fn on_peer_disconnected(&self, peer: &OverlayAddress);

    /// Get peers we should try to connect to.
    fn peers_to_connect(&self) -> Vec<OverlayAddress>;

    /// Remove a peer from all routing state (for banning).
    fn remove_peer(&self, peer: &OverlayAddress);

    /// Evaluate and update connection candidates based on routing needs.
    fn evaluate_connections(&self);
}
