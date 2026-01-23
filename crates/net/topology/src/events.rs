//! Events and commands for the topology behaviour.
//!
//! These types define the interface between the topology layer and the rest of the node.
//! The topology behaviour emits [`TopologyEvent`]s and accepts [`TopologyCommand`]s.

use libp2p::{Multiaddr, PeerId};
use vertex_net_hive::BzzAddress;
use vertex_primitives::OverlayAddress;

/// Events emitted by the topology behaviour.
///
/// These events notify the node about topology changes and peer state.
#[derive(Debug, Clone)]
pub enum TopologyEvent {
    /// A peer has completed the handshake and is ready for protocol exchange.
    ///
    /// This is the signal to activate the client handler for this peer.
    /// The overlay address and node type are extracted from the handshake.
    PeerReady {
        /// The libp2p peer ID.
        peer_id: PeerId,
        /// The peer's Swarm overlay address.
        overlay: OverlayAddress,
        /// Whether the peer is a full node (affects pricing thresholds).
        is_full_node: bool,
    },

    /// A peer has disconnected.
    ///
    /// This is emitted when all connections to a peer are closed.
    PeerDisconnected {
        /// The libp2p peer ID.
        peer_id: PeerId,
        /// The peer's Swarm overlay address (if known).
        overlay: Option<OverlayAddress>,
    },

    /// New peers have been discovered via the hive protocol.
    ///
    /// These peers have been announced by a connected peer and should be
    /// considered for connection if we need more peers.
    PeersDiscovered {
        /// The peer that announced these addresses.
        from: PeerId,
        /// The discovered peer addresses.
        /// Note: Uses Vec of tuples to avoid dependency on hive crate.
        /// Format: (overlay, underlays)
        peers: Vec<(OverlayAddress, Vec<Multiaddr>)>,
    },

    /// The network depth (storage radius) has changed.
    ///
    /// This affects which chunks the node is responsible for storing.
    DepthChanged {
        /// The new depth value.
        new_depth: u8,
    },

    /// A connection attempt failed.
    ConnectionFailed {
        /// The peer ID if known.
        peer_id: Option<PeerId>,
        /// The address that failed.
        address: Multiaddr,
        /// Error description.
        error: String,
    },
}

/// Commands accepted by the topology behaviour.
///
/// These commands allow the node to control the topology layer.
#[derive(Debug, Clone)]
pub enum TopologyCommand {
    /// Connect to a peer at the given address.
    ///
    /// The address should include the peer ID (e.g., `/ip4/.../tcp/.../p2p/...`).
    ConnectPeer(Multiaddr),

    /// Disconnect from a peer.
    ///
    /// This closes all connections to the specified peer.
    DisconnectPeer(PeerId),

    /// Broadcast peer addresses to a connected peer via hive.
    ///
    /// This is used to share known peers with the network.
    BroadcastPeers {
        /// The peer to send addresses to.
        to: PeerId,
        /// The peer addresses to broadcast (must be fully signed BzzAddresses).
        peers: Vec<BzzAddress>,
    },

    /// Ban a peer from reconnecting.
    ///
    /// Banned peers will be rejected on connection attempts.
    BanPeer {
        /// The peer to ban.
        peer_id: PeerId,
        /// Optional reason for the ban.
        reason: Option<String>,
    },
}
