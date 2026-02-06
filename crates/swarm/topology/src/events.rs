//! Topology commands and events.

use libp2p::{Multiaddr, PeerId};
use vertex_swarm_primitives::OverlayAddress;

/// Events emitted by TopologyService for external consumers.
#[derive(Debug, Clone)]
pub enum TopologyServiceEvent {
    /// Peer completed handshake and is ready for protocol use.
    PeerReady {
        overlay: OverlayAddress,
        peer_id: PeerId,
        is_full_node: bool,
    },
    /// All connections to peer closed.
    PeerDisconnected { overlay: OverlayAddress },
    /// Neighborhood depth changed.
    DepthChanged { old_depth: u8, new_depth: u8 },
    /// Dial attempt failed.
    DialFailed { addr: Multiaddr, error: String },
}

/// Commands for the topology behaviour.
#[derive(Debug, Clone)]
pub enum TopologyCommand {
    /// Dial a peer. If `for_gossip` is true, uses delayed health check ping.
    Dial { addr: Multiaddr, for_gossip: bool },
    /// Close all connections to a peer.
    CloseConnection(OverlayAddress),
}
