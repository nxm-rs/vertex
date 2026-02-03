//! Public topology commands and events.
//!
//! These are the external API types for interacting with [`TopologyBehaviour`](crate::TopologyBehaviour).
//! Use [`TopologyCommand`] to send instructions and receive [`TopologyEvent`]s from the swarm.
//!
//! # Design
//!
//! These types deliberately avoid libp2p types (`PeerId`, `ConnectionId`) to ensure
//! that topology consumers don't depend on libp2p internals. The behaviour maintains
//! internal mappings between overlay addresses and peer IDs.

use libp2p::Multiaddr;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_primitives::OverlayAddress;

/// Events emitted by the topology behaviour.
#[derive(Clone)]
pub enum TopologyEvent {
    /// Peer completed handshake and is ready for communication.
    ///
    /// Contains the authenticated peer's identity and metadata. The `SwarmPeer`
    /// contains the cryptographic identity (overlay address, signature, etc.)
    /// while `is_full_node` and `welcome_message` are protocol-level metadata.
    PeerAuthenticated {
        /// The authenticated peer's Swarm identity.
        peer: SwarmPeer,
        /// Whether the peer is a full/storer node.
        is_full_node: bool,
        /// The peer's welcome message.
        welcome_message: String,
    },

    /// All connections to a peer have closed.
    PeerConnectionClosed {
        /// The overlay address of the disconnected peer.
        overlay: OverlayAddress,
    },

    /// Received peer addresses via hive protocol.
    HivePeersReceived {
        /// The overlay address of the peer that sent these addresses.
        from: OverlayAddress,
        /// The discovered peers.
        peers: Vec<SwarmPeer>,
    },

    /// Network depth changed.
    DepthChanged { new_depth: u8 },

    /// Dial attempt failed.
    DialFailed { address: Multiaddr, error: String },
}

/// Commands for the topology behaviour.
#[derive(Debug, Clone)]
pub enum TopologyCommand {
    /// Dial a peer at the given address.
    ///
    /// If `for_gossip` is true, the connection is primarily for hive exchange and
    /// will use a delayed health check (allows remote to disconnect first). If false,
    /// the peer is being added to our kademlia routing table and gets an immediate
    /// health check for fast feedback on peer quality.
    Dial {
        /// The multiaddr to dial.
        addr: Multiaddr,
        /// True if dialing primarily for hive gossip exchange (uses delayed ping).
        /// False if dialing to add peer to kademlia routing table (immediate ping).
        for_gossip: bool,
    },

    /// Close all connections to a peer.
    CloseConnection(OverlayAddress),
}
