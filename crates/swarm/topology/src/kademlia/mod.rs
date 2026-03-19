//! Kademlia-based peer routing for Swarm overlay network.

mod args;
mod candidate_queues;
mod candidates;
mod config;
mod evaluator_task;
mod routing;
mod limits;
pub(crate) mod peer_selection;

pub use args::RoutingArgs;
pub(crate) use candidates::{
    CandidateSelector, CandidateSnapshot, select_balanced_candidates,
    select_neighborhood_candidates,
};
pub use config::KademliaConfig;
pub(crate) use evaluator_task::RoutingEvaluatorHandle;
pub(crate) use routing::KademliaRouting;
pub(crate) use limits::DepthAwareLimits;
pub(crate) use limits::LimitsSnapshot;

use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};

/// Connection capacity management with atomic reservation to prevent TOCTOU races.
pub(crate) trait RoutingCapacity: Send + Sync {
    /// Atomically check capacity and reserve a dial slot.
    /// Returns true if reserved, false if at capacity or already tracking.
    fn try_reserve_dial(&self, overlay: &OverlayAddress, node_type: SwarmNodeType) -> bool;

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
    fn should_accept_inbound(&self, overlay: &OverlayAddress, node_type: SwarmNodeType) -> bool;

    /// Reserve capacity for an accepted inbound connection.
    fn reserve_inbound(&self, overlay: &OverlayAddress);
}

/// Routing operations: extends RoutingCapacity with peer connection/disconnection notifications.
pub(crate) trait SwarmRouting<I: SwarmIdentity>: RoutingCapacity {
    /// Should we accept an inbound connection from this peer?
    fn should_accept_peer(&self, peer: &OverlayAddress, node_type: SwarmNodeType) -> bool;

    /// Notify that a peer has connected.
    fn connected(&self, peer: OverlayAddress);

    /// Update routing tables for a disconnected peer.
    fn on_peer_disconnected(&self, peer: &OverlayAddress);

    /// Remove a peer from all routing state (for banning).
    fn remove_peer(&self, peer: &OverlayAddress);
}
