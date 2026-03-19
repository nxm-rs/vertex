//! Gossip input events, actions, and verification result types.

use libp2p::PeerId;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_primitives::{OverlayAddress, SwarmNodeType};

use super::error::VerificationFailure;

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
    /// Gossiped peers received — verify and store.
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

/// Successful outcome of checking a gossiped peer.
#[derive(Debug)]
pub(super) enum GossipCheckOk {
    /// Peer already exists with matching signature - skip verification.
    AlreadyKnown,
    /// Peer enqueued for verification dial.
    Enqueued,
}

/// Result of verifying a gossiped peer against handshake data.
#[derive(Debug)]
pub(super) enum VerificationResult {
    /// Signatures match - fully verified.
    Verified {
        /// The verified peer from handshake (authoritative).
        verified_peer: SwarmPeer,
    },
    /// Same overlay, different signature - identity rotation.
    IdentityUpdated {
        /// The verified peer from handshake (authoritative).
        verified_peer: SwarmPeer,
    },
    /// Different overlay - wrong gossip info, but real peer discovered.
    DifferentPeerAtAddress {
        /// The verified peer from handshake (authoritative).
        verified_peer: SwarmPeer,
        /// The overlay that was gossiped (incorrect).
        gossiped_overlay: OverlayAddress,
    },
    /// Verification failed - penalize gossiper.
    Failed {
        /// Why verification failed.
        reason: VerificationFailure,
    },
    /// Peer was unreachable (dial failed).
    Unreachable {
        /// The overlay of the unreachable peer.
        gossiped_overlay: OverlayAddress,
    },
}
