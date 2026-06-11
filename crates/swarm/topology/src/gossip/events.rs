//! Gossip input events, actions, and intake outcomes.

use libp2p::PeerId;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};

/// Input events sent from topology to the gossip task.
pub(crate) enum GossipInput {
    /// Mark a peer as discovered via gossip (for delayed gossip exchange).
    MarkGossipDial(PeerId),
    /// Peer accepted into routing — trigger gossip exchange (immediate or delayed).
    PeerActivated {
        peer_id: PeerId,
        swarm_peer: SwarmPeer,
        node_type: SwarmNodeType,
    },
    /// Connection closed — clean up and check depth change gossip.
    ConnectionClosed {
        peer_id: PeerId,
        overlay: Option<OverlayAddress>,
    },
    /// Routing depth changed.
    DepthChanged(u8),
    /// Gossiped peers received — admit through intake into the known table.
    PeersReceived {
        gossiper: OverlayAddress,
        peers: Vec<SwarmPeer>,
    },
}

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
