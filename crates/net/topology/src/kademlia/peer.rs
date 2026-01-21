//! Peer representation in the Kademlia routing table.

use libp2p::Multiaddr;
use vertex_primitives::OverlayAddress;

/// A peer in the Kademlia routing table.
#[derive(Debug, Clone)]
pub struct KademliaPeer {
    /// The peer's overlay address (determines position in DHT).
    pub overlay: OverlayAddress,

    /// The peer's underlay addresses (how to connect).
    pub underlay: Vec<Multiaddr>,

    /// Additional peer information.
    pub info: PeerInfo,
}

impl KademliaPeer {
    /// Create a new peer with just an overlay address.
    pub fn new(overlay: OverlayAddress) -> Self {
        Self {
            overlay,
            underlay: Vec::new(),
            info: PeerInfo::default(),
        }
    }

    /// Create a peer with overlay and underlay addresses.
    pub fn with_underlay(overlay: OverlayAddress, underlay: Vec<Multiaddr>) -> Self {
        Self {
            overlay,
            underlay,
            info: PeerInfo::default(),
        }
    }
}

/// Additional information about a peer.
#[derive(Debug, Clone, Default)]
pub struct PeerInfo {
    /// Whether this peer is a bootnode.
    pub is_bootnode: bool,

    /// Whether this peer is a full node (stores chunks).
    pub is_full_node: bool,

    /// The peer's advertised depth/radius.
    pub depth: Option<u8>,

    /// Timestamp of last successful interaction.
    pub last_seen: Option<u64>,
}

impl PeerInfo {
    /// Mark this peer as a bootnode.
    pub fn bootnode(mut self) -> Self {
        self.is_bootnode = true;
        self
    }

    /// Mark this peer as a full node.
    pub fn full_node(mut self) -> Self {
        self.is_full_node = true;
        self
    }

    /// Set the peer's depth.
    pub fn with_depth(mut self, depth: u8) -> Self {
        self.depth = Some(depth);
        self
    }
}
