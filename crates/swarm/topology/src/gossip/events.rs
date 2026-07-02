//! Gossip actions and intake outcomes.

use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_primitives::OverlayAddress;

/// An action to send peers to a specific overlay address.
pub(crate) struct GossipAction {
    pub to: OverlayAddress,
    pub peers: Vec<SwarmPeer>,
}

/// Successful outcome of checking a gossiped record at intake.
#[derive(Debug)]
pub(super) enum GossipCheckOk {
    /// Record matches the stored one (same signature and addresses) - skip.
    AlreadyKnown,
    /// Record admitted; the caller stores it as an unverified peer.
    Admitted,
}
