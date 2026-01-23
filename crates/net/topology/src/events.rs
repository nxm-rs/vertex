//! Events and commands for the topology behaviour.
//!
//! These types define the interface between the topology layer (libp2p) and the
//! client layer. All types use pure libp2p concepts (PeerId, Multiaddr) - the
//! client layer is responsible for extracting Swarm-layer concepts (OverlayAddress)
//! from HandshakeInfo.

use libp2p::{Multiaddr, PeerId, swarm::ConnectionId};
use vertex_net_handshake::HandshakeInfo;
use vertex_net_hive::BzzAddress;

/// Events emitted by the topology behaviour.
///
/// All events use libp2p types (PeerId, Multiaddr, ConnectionId).
/// The client layer extracts OverlayAddress from HandshakeInfo.
#[derive(Debug, Clone)]
pub enum TopologyEvent {
    /// A peer has completed the handshake and is authenticated.
    ///
    /// The client layer should:
    /// 1. Extract overlay address from `info.ack.node_address().overlay_address()`
    /// 2. Register the PeerId ↔ OverlayAddress mapping in PeerManager
    /// 3. Notify kademlia of the new connected peer
    PeerAuthenticated {
        /// The libp2p peer ID.
        peer_id: PeerId,
        /// The connection ID for this handshake.
        connection_id: ConnectionId,
        /// The handshake info containing the peer's Ack.
        info: HandshakeInfo,
    },

    /// A peer connection has been closed.
    ///
    /// This is emitted when all connections to a peer are closed.
    /// The client layer should resolve PeerId → OverlayAddress via PeerManager.
    PeerConnectionClosed {
        /// The libp2p peer ID.
        peer_id: PeerId,
    },

    /// Peer addresses received via hive protocol.
    ///
    /// These are raw BzzAddress entries as received on the wire.
    /// The client layer should:
    /// 1. Extract overlay address from each BzzAddress
    /// 2. Cache the underlay addresses in PeerManager
    /// 3. Notify kademlia of the discovered peers
    HivePeersReceived {
        /// The peer that sent us these addresses.
        from: PeerId,
        /// The peer addresses in wire format.
        peers: Vec<BzzAddress>,
    },

    /// The network depth (storage radius) has changed.
    DepthChanged {
        /// The new depth value.
        new_depth: u8,
    },

    /// A dial attempt failed.
    DialFailed {
        /// The address that failed.
        address: Multiaddr,
        /// Error description.
        error: String,
    },
}

/// Commands accepted by the topology behaviour.
///
/// All commands use libp2p types. The client layer resolves OverlayAddress
/// to PeerId via PeerManager before sending commands.
#[derive(Debug, Clone)]
pub enum TopologyCommand {
    /// Dial a peer at the given address.
    ///
    /// The address should include the peer ID (e.g., `/ip4/.../tcp/.../p2p/...`).
    Dial(Multiaddr),

    /// Close all connections to a peer.
    CloseConnection(PeerId),

    /// Broadcast peer addresses to a connected peer via hive.
    BroadcastPeers {
        /// The peer to send addresses to.
        to: PeerId,
        /// The peer addresses to broadcast.
        peers: Vec<BzzAddress>,
    },
}
