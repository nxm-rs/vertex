//! Events and commands for the topology behaviour.
//!
//! All types use libp2p concepts (PeerId, Multiaddr). The client layer
//! extracts Swarm-layer concepts (OverlayAddress) from HandshakeInfo/SwarmPeer.

use libp2p::{Multiaddr, PeerId, swarm::ConnectionId};
use vertex_swarm_peer::SwarmPeer;
use vertex_net_handshake::HandshakeInfo;

/// Events emitted by the topology behaviour.
#[derive(Clone)]
pub enum TopologyEvent {
    /// Peer completed handshake and is authenticated.
    PeerAuthenticated {
        peer_id: PeerId,
        connection_id: ConnectionId,
        info: HandshakeInfo,
    },

    /// All connections to a peer closed.
    PeerConnectionClosed {
        peer_id: PeerId,
    },

    /// Peer addresses received via hive.
    HivePeersReceived {
        from: PeerId,
        peers: Vec<SwarmPeer>,
    },

    /// Network depth changed.
    DepthChanged {
        new_depth: u8,
    },

    /// Dial attempt failed.
    DialFailed {
        address: Multiaddr,
        error: String,
    },
}

/// Commands accepted by the topology behaviour.
#[derive(Debug, Clone)]
pub enum TopologyCommand {
    /// Dial a peer at the given address.
    Dial(Multiaddr),

    /// Close all connections to a peer.
    CloseConnection(PeerId),

    /// Broadcast peer addresses via hive.
    BroadcastPeers {
        to: PeerId,
        peers: Vec<SwarmPeer>,
    },
}
