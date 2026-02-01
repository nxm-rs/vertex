//! Public topology commands and events.
//!
//! These are the external API types for interacting with [`TopologyBehaviour`](crate::TopologyBehaviour).
//! Use [`TopologyCommand`] to send instructions and receive [`TopologyEvent`]s from the swarm.

use libp2p::{Multiaddr, PeerId, swarm::ConnectionId};
use vertex_net_handshake::HandshakeInfo;
use vertex_swarm_peer::SwarmPeer;

/// Events emitted by the topology behaviour.
#[derive(Clone)]
pub enum TopologyEvent {
    /// Peer completed handshake and is ready for communication.
    PeerAuthenticated {
        peer_id: PeerId,
        connection_id: ConnectionId,
        info: Box<HandshakeInfo>,
    },

    /// All connections to a peer have closed.
    PeerConnectionClosed { peer_id: PeerId },

    /// Received peer addresses via hive protocol.
    HivePeersReceived { from: PeerId, peers: Vec<SwarmPeer> },

    /// Network depth changed.
    DepthChanged { new_depth: u8 },

    /// Dial attempt failed.
    DialFailed { address: Multiaddr, error: String },
}

/// Commands for the topology behaviour.
#[derive(Debug, Clone)]
pub enum TopologyCommand {
    /// Dial a peer at the given address.
    Dial(Multiaddr),

    /// Close all connections to a peer.
    CloseConnection(PeerId),

    /// Broadcast peer addresses via hive protocol.
    BroadcastPeers { to: PeerId, peers: Vec<SwarmPeer> },
}
